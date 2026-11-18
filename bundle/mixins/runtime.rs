use horizon_core::position::BlockPos;
use horizon_world::level::LevelAccess as _;
use horizon_world::worldgen::structure::{Mirror, Rotation};
use std::collections::HashSet;
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

    let bottom_y = game.world.bottom_y();
    let max_qy = (game.world.layout().height() / 4 - 1).max(0);
    let sections = game.world.layout().sections() as i32;
    let mut touched = HashSet::new();

    for (pos, flavor) in pending {
        let Some(target) = biome_indices
            .get(usize::from(flavor))
            .copied()
            .flatten()
            .or(biome_indices[4])
        else {
            continue;
        };
        let key = (pos[0] >> 4, pos[2] >> 4);
        let qy = ((pos[1] - bottom_y) >> 2).clamp(0, max_qy);
        let Some(chunk) = game.world.chunks.get_mut(&key) else {
            continue;
        };
        if usize::from(chunk.biome(pos[0], pos[1], pos[2])) == target {
            continue;
        }
        Arc::make_mut(chunk).set_biome_cell((pos[0] & 15) >> 2, qy, (pos[2] & 15) >> 2, target as u16);
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

fn apply_bastion_pieces(game: &mut super::Game<'_>) {
    let pending = crate::entry::rift_bridge::drain_bastion_pieces();
    if pending.is_empty() {
        return;
    }

    for (template_id, pos, rot_idx) in pending {
        let rot = match rot_idx & 3 {
            0 => Rotation::None,
            1 => Rotation::Clockwise90,
            2 => Rotation::Clockwise180,
            _ => Rotation::Counterclockwise90,
        };
        if let Some(entry) = game.template_entry(&template_id) {
            let settings = super::super::structure_blocks::load_settings(Mirror::None, rot, 1.0, 0, false);
            let at = BlockPos::new(pos[0], pos[1], pos[2]);
            game.place_template_entry(&entry, at, settings, Mirror::None, rot);
        }
    }
}
