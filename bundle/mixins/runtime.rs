use crate::mobs::{Species, Who};
use horizon_core::position::BlockPos;
use horizon_world::level::{LevelAccess as _, flags};
use horizon_world::nbt::Tag;
use horizon_world::worldgen::generator::JigsawQuery;
use horizon_world::worldgen::structure::{BoundingBox, Mirror, Rotation, StructureTemplate};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const FLAVOR_BIOMES: [&str; 5] = [
    "minecraft:crimson_forest",
    "minecraft:warped_forest",
    "minecraft:soul_sand_valley",
    "minecraft:basalt_deltas",
    "minecraft:nether_wastes",
];

pub(super) fn apply_pending(game: &mut super::Game<'_>, dimension: &str) {
    if dimension != "minecraft:overworld" || !game.native_authoritative() {
        return;
    }

    apply_biomes(game);
    apply_fast_lava(game);
    apply_mob_buffs(game);
    apply_mob_targets(game);
    apply_bastion_pieces(game);
}

fn apply_biomes(game: &mut super::Game<'_>) {
    let pending = crate::entry::rift_bridge::drain_biomes();
    if pending.is_empty() {
        return;
    }

    let biome_indices: [Option<usize>; 5] = {
        let biomes = game
            .world_biomes
            .as_deref()
            .unwrap_or_else(|| game.res.biomes());
        FLAVOR_BIOMES.map(|name| biomes.list.iter().position(|b| b.name == name))
    };

    let q_height = (game.world.layout().height() / 4).max(1);
    let sections = game.world.layout().sections() as i32;
    let mut touched = HashSet::new();
    let mut visited_cols = HashSet::new();

    for (pos, flavor) in pending {
        let qx_world = pos[0] >> 2;
        let qz_world = pos[2] >> 2;
        if !visited_cols.insert((qx_world, qz_world)) {
            continue;
        }
        let Some(target) = biome_indices
            .get(usize::from(flavor))
            .copied()
            .flatten()
            .or(biome_indices[4])
        else {
            continue;
        };
        let key = (pos[0] >> 4, pos[2] >> 4);
        let Some(chunk) = game.world.chunks.get_mut(&key) else {
            continue;
        };
        let qx = (pos[0] & 15) >> 2;
        let qz = (pos[2] & 15) >> 2;
        // Paint the full vertical column (bedrock to sky ceiling) so flying above
        // corrupted terrain keeps the Nether biome sky haze, spawns, and music.
        if usize::from(chunk.biome(pos[0], 64, pos[2])) == target
            && usize::from(chunk.biome(pos[0], 160, pos[2])) == target
        {
            continue;
        }
        let chunk_mut = Arc::make_mut(chunk);
        for qy in 0..q_height {
            chunk_mut.set_biome_cell(qx, qy, qz, target as u16);
        }
        touched.insert(key);
    }

    if touched.is_empty() {
        return;
    }

    for key in &touched {
        game.world.modified.insert(*key);
        for sy in 0..sections {
            game.world.dirty_sections.insert((key.0, sy, key.1));
        }
    }

    if let Some(host) = game.lan.as_mut() {
        let columns: Vec<(i32, i32)> = touched.into_iter().collect();
        host.hub.resend_biomes(&game.world, &columns);
    }
}

fn apply_fast_lava(game: &mut super::Game<'_>) {
    let pending = crate::entry::rift_bridge::drain_fast_lava();
    if pending.is_empty() {
        return;
    }

    game.with_level(|_, level, ticks| {
        let now = level.game_time();
        for (pos, fluid) in pending {
            let at = BlockPos::new(pos[0], pos[1], pos[2]);
            let chunk_pos = at.chunk();
            let slow_tick = ticks
                .fluids
                .chunk(chunk_pos)
                .map(|c| {
                    c.queued()
                        .iter()
                        .any(|t| t.pos == at && t.trigger_tick > now + 10)
                })
                .unwrap_or(false);
            if slow_tick && let Some(mut chunk_ticks) = ticks.fluids.remove_chunk(chunk_pos) {
                chunk_ticks.remove_if(|t| t.pos == at);
                let _ = ticks.fluids.add_chunk(chunk_pos, chunk_ticks);
            }
            let _ = level.native_schedule_fluid_tick(ticks, pos, &fluid, 8);
        }
    });
}

fn apply_mob_buffs(game: &mut super::Game<'_>) {
    let pending = crate::entry::rift_bridge::drain_mob_buffs();
    if pending.is_empty() {
        return;
    }

    for (mob_id, fire_res, strength, amp) in pending {
        if let Some(mob) = game
            .mobs
            .iter_mut()
            .find(|m| m.id == mob_id && !m.removed && m.alive())
        {
            if fire_res > 0 {
                mob.add_effect("minecraft:fire_resistance", fire_res, 0);
            }
            if strength > 0 {
                mob.add_effect("minecraft:strength", strength, amp);
            }
            mob.needs_sync = true;
        }
    }
}

fn apply_mob_targets(game: &mut super::Game<'_>) {
    let pending = crate::entry::rift_bridge::drain_mob_targets();
    if pending.is_empty() {
        return;
    }

    let alive_ids: HashSet<u32> = game
        .mobs
        .iter()
        .filter(|m| !m.removed && m.alive())
        .map(|m| m.id)
        .collect();

    for (hunter_id, victim_id) in pending {
        if !alive_ids.contains(&victim_id) {
            continue;
        }
        let Some(mob) = game
            .mobs
            .iter_mut()
            .find(|m| m.id == hunter_id && !m.removed && m.alive())
        else {
            continue;
        };
        if mob.ai.target.is_some() {
            continue;
        }
        mob.ai.target = Some(Who::Mob(victim_id));
        mob.ai.aggressive = true;
        if matches!(
            mob.species(),
            Species::Piglin | Species::PiglinBrute | Species::Hoglin
        ) {
            mob.extra.anger = mob.extra.anger.max(400);
        }
        mob.needs_sync = true;
    }
}

fn apply_bastion_pieces(game: &mut super::Game<'_>) {
    let pending_jigsaws = crate::entry::rift_bridge::drain_bastion_jigsaws();
    for (pos, flavor, roll) in pending_jigsaws {
        if !try_place_jigsaw_bastion(game, pos, flavor) {
            try_place_fallback_bastion(game, pos, flavor, roll);
        }
    }

    let pending_templates = crate::entry::rift_bridge::drain_bastion_pieces();
    for (template_id, pos, rot_idx) in pending_templates {
        let rot = match rot_idx & 3 {
            0 => Rotation::None,
            1 => Rotation::Clockwise90,
            2 => Rotation::Clockwise180,
            _ => Rotation::Counterclockwise90,
        };
        if let Some(area) = place_clean_template(game, &template_id, pos, rot) {
            sanitize_and_ground_bastion(game, &[area], 4);
        }
    }
}

fn try_place_jigsaw_bastion(game: &mut super::Game<'_>, pos: [i32; 3], flavor: u8) -> bool {
    let Some(terrain) = game.end.locator() else {
        return false;
    };

    let mut chosen = None;
    for max_depth in [4, 3] {
        let query = JigsawQuery {
            pool: "minecraft:bastion/starts",
            target: "minecraft:empty",
            max_depth,
            position: pos,
        };
        let Some(jigsaw) = terrain.jigsaw_pieces(&query) else {
            continue;
        };
        if jigsaw.pieces.is_empty() {
            continue;
        }
        let bboxes: Vec<BoundingBox> = jigsaw.pieces.iter().map(|p| p.bounding_box).collect();
        let all_loaded = bboxes.iter().all(|bb| {
            ((bb.min[0] >> 4)..=(bb.max[0] >> 4)).all(|cx| {
                ((bb.min[2] >> 4)..=(bb.max[2] >> 4))
                    .all(|cz| game.world.chunks.contains_key(&(cx, cz)))
            })
        });
        if all_loaded {
            chosen = Some((jigsaw, bboxes));
            break;
        }
    }

    let Some((jigsaw, bboxes)) = chosen else {
        return false;
    };

    let (placed, entities) =
        game.with_level(|b, level, ticks| b.place_jigsaw(level, ticks, pos, false, || Some(jigsaw)));
    if !placed {
        return false;
    }

    for entity in entities {
        game.spawn_entity_from_nbt(Tag::Compound(entity));
    }
    let _ = game.process_level_events();
    sanitize_and_ground_bastion(game, &bboxes, flavor);
    true
}

fn place_clean_template(
    game: &mut super::Game<'_>,
    template_id: &str,
    pos: [i32; 3],
    rot: Rotation,
) -> Option<BoundingBox> {
    let entry = game.template_entry(template_id)?;
    let transforms = game.state_transforms()?;
    let template = StructureTemplate::from_nbt(&entry.tag, game.pack.assets.registry, transforms);
    let area = template.bounding_box(pos, rot, [0; 3], Mirror::None);
    let settings = super::super::structure_blocks::load_settings(Mirror::None, rot, 1.0, 0, false);
    let at = BlockPos::new(pos[0], pos[1], pos[2]);
    if game.place_template_entry(&entry, at, settings, Mirror::None, rot) {
        Some(area)
    } else {
        None
    }
}

fn try_place_fallback_bastion(game: &mut super::Game<'_>, pos: [i32; 3], flavor: u8, roll: u64) {
    let rot = match (roll >> 20) & 3 {
        0 => Rotation::None,
        1 => Rotation::Clockwise90,
        2 => Rotation::Clockwise180,
        _ => Rotation::Counterclockwise90,
    };
    let template_id = match (roll >> 24) % 4 {
        0 => "minecraft:bastion/treasure/bases/lava_basin",
        1 => "minecraft:bastion/units/air_base",
        2 => "minecraft:bastion/hoglin_stable/starting_pieces/starting_stairs_0",
        _ => "minecraft:bastion/bridge/starting_pieces/entrance_base",
    };
    if let Some(area) = place_clean_template(game, template_id, pos, rot) {
        sanitize_and_ground_bastion(game, &[area], flavor);
    }
}

fn short_block_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(_, s)| s)
}

fn is_technical_block(short: &str) -> bool {
    matches!(
        short,
        "jigsaw"
            | "structure_block"
            | "structure_void"
            | "command_block"
            | "repeating_command_block"
            | "chain_command_block"
            | "barrier"
    )
}

fn is_fillable_terrain_void(short: &str) -> bool {
    matches!(
        short,
        "air"
            | "cave_air"
            | "void_air"
            | "water"
            | "flowing_water"
            | "lava"
            | "flowing_lava"
            | "fire"
            | "soul_fire"
            | "snow"
            | "short_grass"
            | "tall_grass"
            | "fern"
            | "large_fern"
            | "dead_bush"
            | "crimson_roots"
            | "warped_roots"
            | "nether_sprouts"
            | "crimson_fungus"
            | "warped_fungus"
            | "vine"
            | "weeping_vines"
            | "weeping_vines_plant"
            | "twisting_vines"
            | "twisting_vines_plant"
            | "kelp"
            | "kelp_plant"
            | "seagrass"
            | "tall_seagrass"
            | "lily_pad"
    ) || short.ends_with("_leaves")
}

fn is_bastion_floor_block(short: &str) -> bool {
    matches!(
        short,
        "blackstone"
            | "polished_blackstone"
            | "polished_blackstone_bricks"
            | "cracked_polished_blackstone_bricks"
            | "chiseled_polished_blackstone"
            | "gilded_blackstone"
            | "polished_blackstone_brick_stairs"
            | "polished_blackstone_stairs"
            | "blackstone_stairs"
            | "polished_blackstone_brick_slab"
            | "polished_blackstone_slab"
            | "blackstone_slab"
            | "basalt"
            | "polished_basalt"
            | "smooth_basalt"
            | "nether_bricks"
            | "cracked_nether_bricks"
            | "red_nether_bricks"
            | "netherrack"
            | "crimson_nylium"
            | "warped_nylium"
            | "magma_block"
            | "obsidian"
            | "crying_obsidian"
            | "gold_block"
            | "soul_sand"
            | "soul_soil"
    )
}

fn masonry_hash(x: i32, y: i32, z: i32) -> u64 {
    let mut v = (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ (z as i64 as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^= v >> 30;
    v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v ^ (v >> 27)
}

fn sanitize_and_ground_bastion(
    game: &mut super::Game<'_>,
    bboxes: &[BoundingBox],
    flavor: u8,
) {
    if bboxes.is_empty() {
        return;
    }

    let reg = game.pack.assets.registry;
    let Some(air) = reg.default_state("minecraft:air") else {
        return;
    };
    let blackstone = reg.default_state("minecraft:blackstone").unwrap_or(air);
    let cracked_bricks = reg
        .default_state("minecraft:cracked_polished_blackstone_bricks")
        .unwrap_or(blackstone);
    let basalt = reg.default_state("minecraft:basalt").unwrap_or(blackstone);

    let world_min = game.world.layout().min_y();
    let world_max = world_min + game.world.layout().height();

    // Pass 1: Strip any leftover Jigsaw / Structure / Command / Barrier blocks inside piece boxes.
    let mut scrub_edits: Vec<(BlockPos, horizon_world::registry::BlockStateId)> = Vec::new();
    let mut scrub_entities: Vec<(i32, i32, i32)> = Vec::new();
    let mut seen_scrub = HashSet::new();

    for bb in bboxes {
        for x in (bb.min[0] - 1)..=(bb.max[0] + 1) {
            for z in (bb.min[2] - 1)..=(bb.max[2] + 1) {
                if !game.world.chunks.contains_key(&(x >> 4, z >> 4)) {
                    continue;
                }
                for y in (bb.min[1] - 1).max(world_min)..=(bb.max[1] + 1).min(world_max - 1) {
                    let state = game.world.block(x, y, z);
                    let short = short_block_name(&game.res.facts().get(state).name);
                    if !is_technical_block(short) || !seen_scrub.insert((x, y, z)) {
                        continue;
                    }
                    scrub_entities.push((x, y, z));
                    let replacement = if short == "structure_void" {
                        air
                    } else {
                        let solid_neighbors = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                            .into_iter()
                            .filter(|(dx, dz)| {
                                let ns = game.world.block(x + dx, y, z + dz);
                                let nshort = short_block_name(&game.res.facts().get(ns).name);
                                !is_technical_block(nshort) && !is_fillable_terrain_void(nshort)
                            })
                            .count();
                        if solid_neighbors >= 2 {
                            blackstone
                        } else {
                            air
                        }
                    };
                    scrub_edits.push((BlockPos::new(x, y, z), replacement));
                }
            }
        }
    }

    for key in scrub_entities {
        game.world.block_entities.remove(&key);
    }
    if !scrub_edits.is_empty() {
        game.with_level(|b, level, ticks| {
            for (pos, state) in scrub_edits {
                b.set_block(level, ticks, pos, state, flags::UPDATE_CLIENTS);
            }
        });
    }

    // Pass 2: Extrude weathered blackstone foundations down into solid terrain so no piece floats.
    let mut col_bounds: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
    for bb in bboxes {
        for x in bb.min[0]..=bb.max[0] {
            for z in bb.min[2]..=bb.max[2] {
                col_bounds
                    .entry((x, z))
                    .and_modify(|(lo, hi)| {
                        *lo = (*lo).min(bb.min[1]);
                        *hi = (*hi).max(bb.max[1]);
                    })
                    .or_insert((bb.min[1], bb.max[1]));
            }
        }
    }

    let mut ground_edits: Vec<(BlockPos, horizon_world::registry::BlockStateId)> = Vec::new();
    let mut quart_cells: HashSet<[i32; 3]> = HashSet::new();

    for ((x, z), (col_min_y, col_max_y)) in col_bounds {
        if !game.world.chunks.contains_key(&(x >> 4, z >> 4)) {
            continue;
        }
        quart_cells.insert([x >> 2, 4, z >> 2]);

        let scan_top = (col_min_y + 12).min(col_max_y).min(world_max - 1);
        let mut floor_y = None;
        for y in col_min_y.max(world_min)..=scan_top {
            let s = game.world.block(x, y, z);
            let short = short_block_name(&game.res.facts().get(s).name);
            if is_bastion_floor_block(short) {
                floor_y = Some(y);
                break;
            }
        }
        let Some(base_y) = floor_y else {
            continue;
        };

        let stop_floor = (base_y - 36).max(world_min + 1);
        for y in (stop_floor..base_y).rev() {
            let s = game.world.block(x, y, z);
            let short = short_block_name(&game.res.facts().get(s).name);
            if !is_fillable_terrain_void(short) {
                break;
            }
            let h = masonry_hash(x, y, z) % 20;
            let fill = if flavor == 3 && h < 7 {
                basalt
            } else if h < 4 {
                cracked_bricks
            } else if h == 19 {
                basalt
            } else {
                blackstone
            };
            ground_edits.push((BlockPos::new(x, y, z), fill));
        }
    }

    if !ground_edits.is_empty() {
        game.with_level(|b, level, ticks| {
            for (pos, state) in ground_edits {
                b.set_block(level, ticks, pos, state, flags::UPDATE_CLIENTS);
            }
        });
    }

    if !quart_cells.is_empty() {
        let cells: Vec<[i32; 3]> = quart_cells.into_iter().collect();
        let biome_entries: Vec<([i32; 3], u8)> = cells
            .iter()
            .map(|[qx, qy, qz]| ([qx << 2, qy << 2, qz << 2], flavor))
            .collect();
        crate::entry::rift_bridge::record_quart_cells(&cells);
        crate::entry::rift_bridge::enqueue_biomes(&biome_entries);
        apply_biomes(game);
    }

    let _ = game.process_level_events();
}
