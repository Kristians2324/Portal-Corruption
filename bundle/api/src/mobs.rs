use crate::corruption::is_air_like;
use crate::state::RiftState;
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, dist_sq, horiz_dist_sq,
    quart_block_origin, spatial_hash, strip_ns,
};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    Compound, Host, ItemStack, NativeHeightmap, ParticleOptions, ParticleType, PotentialSpawnsEvent,
    SpawnEntry, Tag,
};

const MUTATION_INTERVAL_TICKS: u64 = 20;
const HOGLIN_SPREAD_INTERVAL_TICKS: u64 = 24;
const SUPPLEMENTAL_SPAWN_INTERVAL_TICKS: u64 = 120;

/// Pure mapping of Overworld creatures caught in Nether-corrupted zones to
/// existing vanilla Nether mobs (zero custom models required).
pub fn nether_conversion_target(kind: &str) -> Option<&'static str> {
    match strip_ns(kind) {
        "pig" | "villager" | "wandering_trader" => Some("minecraft:piglin"),
        "cow" | "mooshroom" | "sheep" | "goat" | "horse" | "donkey" => Some("minecraft:hoglin"),
        "skeleton" | "stray" | "bogged" => Some("minecraft:wither_skeleton"),
        "slime" => Some("minecraft:magma_cube"),
        _ => None,
    }
}

/// Hostile Overworld mobs that keep their body but gain Fire Resistance,
/// Strength, ember trails, and Nether drops inside corrupted territory.
pub fn is_nether_touched_candidate(kind: &str) -> bool {
    matches!(
        strip_ns(kind),
        "zombie"
            | "husk"
            | "drowned"
            | "zombie_villager"
            | "spider"
            | "cave_spider"
            | "creeper"
            | "pillager"
            | "vindicator"
            | "evoker"
            | "witch"
            | "enderman"
    )
}

pub fn is_piglin_or_hoglin(kind: &str) -> bool {
    matches!(strip_ns(kind), "piglin" | "piglin_brute" | "hoglin")
}

pub fn is_zombified_outcome(outcome: &str) -> bool {
    matches!(strip_ns(outcome), "zombified_piglin" | "zoglin")
}

pub fn should_block_zombification(
    host: &dyn Host,
    state: &RiftState,
    kind: &str,
    outcome: &str,
    pos: [i32; 3],
) -> bool {
    if !is_piglin_or_hoglin(kind) || !is_zombified_outcome(outcome) {
        return false;
    }
    pos_is_corrupted(host, state, pos)
}

pub fn pos_is_corrupted(host: &dyn Host, state: &RiftState, pos: [i32; 3]) -> bool {
    if state.is_quart_corrupted(pos) {
        return true;
    }
    if let Some(name) = host.biome(pos) {
        return NetherFlavor::from_name(&name).is_some();
    }
    false
}

pub fn tick_mobs(host: &mut dyn Host, state: &mut RiftState) {
    if state.rifts.is_empty() {
        return;
    }

    let do_convert = state.tick % MUTATION_INTERVAL_TICKS == 5;
    let do_hoglin = state.tick % HOGLIN_SPREAD_INTERVAL_TICKS == 11;
    let do_spawn = state.tick % SUPPLEMENTAL_SPAWN_INTERVAL_TICKS == 45;
    if !do_convert && !do_hoglin && !do_spawn {
        return;
    }

    let entities = host.level_entities(OVERWORLD);

    if do_convert {
        process_mob_conversions(host, state, &entities);
    }
    if do_hoglin {
        process_hoglin_edge_spreading(host, state, &entities);
    }
    if do_spawn {
        process_supplemental_spawns(host, state, &entities);
    }
}

fn process_mob_conversions(
    host: &mut dyn Host,
    state: &mut RiftState,
    entities: &[(u32, String, [f64; 3])],
) {
    let mut converted_budget = 6usize;

    for (id, kind, pos) in entities {
        let bp = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];
        if !pos_is_corrupted(host, state, bp) {
            continue;
        }

        if let Some(target_kind) = nether_conversion_target(kind) {
            if converted_budget == 0 {
                continue;
            }
            // Only transform mobs whose chunk is loaded and that stand above or inside corrupt ground.
            if !host.remove_mob(OVERWORLD, *id) {
                continue;
            }
            converted_budget -= 1;
            let mut nbt = Compound::new();
            if target_kind == "minecraft:piglin" || target_kind == "minecraft:hoglin" {
                nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
            }
            if let Some(new_id) = host.summon(OVERWORLD, target_kind, *pos, nbt) {
                state.nether_touched_mobs.insert(new_id);
                rift_bridge::enqueue_mob_buff(new_id, 20 * 180, 20 * 120, 0);
            }
            if let Some(level) = host.level(OVERWORLD) {
                let _ = level.send_particles(
                    host,
                    &ParticleOptions::simple(ParticleType::Flame),
                    None,
                    [pos[0], pos[1] + 0.9, pos[2]],
                    [0.35, 0.45, 0.35],
                    0.03,
                    14,
                );
                let _ = level.play_sound(
                    host,
                    "minecraft:entity.zombie_villager.converted",
                    *pos,
                    5,
                    0.8,
                    0.9,
                );
            }
            continue;
        }

        if is_nether_touched_candidate(kind) {
            let newly_touched = state.nether_touched_mobs.insert(*id);
            if newly_touched {
                rift_bridge::enqueue_mob_buff(*id, 20 * 600, 20 * 600, 0);
            }
            if let Some(level) = host.level(OVERWORLD) {
                let _ = level.send_particles(
                    host,
                    &ParticleOptions::simple(ParticleType::SmallFlame),
                    None,
                    [pos[0], pos[1] + 0.8, pos[2]],
                    [0.25, 0.35, 0.25],
                    0.01,
                    2,
                );
            }
        } else if is_piglin_or_hoglin(kind) && (state.tick + u64::from(*id)) % 80 < MUTATION_INTERVAL_TICKS
        {
            // Keep Overworld Piglins/Hoglins heat-resistant on magma & embers.
            rift_bridge::enqueue_mob_buff(*id, 20 * 90, 0, 0);
        }
    }
}

/// Hoglins act as border vanguard: near the edge of corrupted land they root
/// and trample uncorrupted Overworld grass/dirt into crimson nylium/netherrack
/// and seed new frontier growth!
fn process_hoglin_edge_spreading(
    host: &mut dyn Host,
    state: &mut RiftState,
    entities: &[(u32, String, [f64; 3])],
) {
    if state.paused {
        return;
    }
    let Some(nylium) = host.default_state("minecraft:crimson_nylium") else {
        return;
    };
    let Some(netherrack) = host.default_state("minecraft:netherrack") else {
        return;
    };
    let roots = host.default_state("minecraft:crimson_roots");

    let mut total_trampled = 0usize;
    let mut biome_updates = Vec::new();

    for (id, kind, pos) in entities {
        if total_trampled >= 8 {
            break;
        }
        if strip_ns(kind) != "hoglin" {
            continue;
        }
        let base = [
            pos[0].floor() as i32,
            pos[1].floor() as i32 - 1,
            pos[2].floor() as i32,
        ];
        if base[1] <= OVERWORLD_MIN_Y || base[1] >= OVERWORLD_MAX_Y - 2 {
            continue;
        }
        // Must have some corrupted ground nearby to act as a frontier spreader.
        let near_corrupt = pos_is_corrupted(host, state, base)
            || state
                .rifts
                .iter()
                .any(|r| !r.contained && horiz_dist_sq(r.center, base) <= r.max_radius_sq + 196);
        if !near_corrupt {
            continue;
        }

        let mut trampled_here = false;
        for dx in -2..=2 {
            for dz in -2..=2 {
                if total_trampled >= 8 {
                    break;
                }
                for dy in -1..=1 {
                    let p = [base[0] + dx, base[1] + dy, base[2] + dz];
                    let b = host.block(p);
                    if !matches!(
                        strip_ns(&b),
                        "grass_block"
                            | "dirt"
                            | "podzol"
                            | "mycelium"
                            | "mud"
                            | "moss_block"
                            | "pale_moss_block"
                            | "sand"
                    ) {
                        continue;
                    }
                    let above = host.block([p[0], p[1] + 1, p[2]]);
                    let use_nylium = is_air_like(&above)
                        && spatial_hash(p[0], p[1], p[2], state.tick ^ u64::from(*id)) % 3 != 0;
                    let chosen = if use_nylium { nylium } else { netherrack };
                    if host.set_block(OVERWORLD, p, chosen, 3) {
                        total_trampled += 1;
                        trampled_here = true;
                        if use_nylium
                            && let Some(r_id) = roots
                            && is_air_like(&above)
                            && spatial_hash(p[0], p[1] + 1, p[2], 0x4061) % 4 == 0
                        {
                            let _ = host.set_block(OVERWORLD, [p[0], p[1] + 1, p[2]], r_id, 3);
                        }
                        state.pending_frontier_injections.push_back(p);
                        if state.mark_quart_corrupted(p) {
                            let qb = quart_block_origin(p);
                            biome_updates.push((qb, NetherFlavor::CrimsonForest.to_u8()));
                            rift_bridge::record_quart_cells(&[crate::types::quart_coords(p)]);
                        }
                        break;
                    }
                }
            }
        }

        if trampled_here && let Some(level) = host.level(OVERWORLD) {
            let _ = level.send_particles(
                host,
                &ParticleOptions::simple(ParticleType::CrimsonSpore),
                None,
                [pos[0], pos[1] + 0.3, pos[2]],
                [0.8, 0.3, 0.8],
                0.02,
                8,
            );
            let _ = level.play_sound(
                host,
                "minecraft:entity.hoglin.angry",
                *pos,
                5,
                0.55,
                0.85,
            );
        }
    }

    if !biome_updates.is_empty() {
        rift_bridge::enqueue_biomes(&biome_updates);
        state.dirty = true;
    }
}

fn process_supplemental_spawns(
    host: &mut dyn Host,
    state: &mut RiftState,
    entities: &[(u32, String, [f64; 3])],
) {
    let players = host.server_players();
    if players.is_empty() {
        return;
    }

    let nether_mob_count = entities
        .iter()
        .filter(|(_, k, _)| {
            matches!(
                strip_ns(k),
                "piglin"
                    | "piglin_brute"
                    | "hoglin"
                    | "wither_skeleton"
                    | "magma_cube"
                    | "ghast"
                    | "blaze"
            )
        })
        .count();
    if nether_mob_count >= 28 {
        return;
    }

    for player in players {
        if player.dimension != OVERWORLD {
            continue;
        }
        let pp = [
            player.position[0].floor() as i32,
            player.position[1].floor() as i32,
            player.position[2].floor() as i32,
        ];
        let Some(rift) = state
            .rifts
            .iter()
            .filter(|r| horiz_dist_sq(r.center, pp) <= 96 * 96)
            .min_by_key(|r| dist_sq(r.center, pp))
        else {
            continue;
        };

        let h = spatial_hash(pp[0], pp[1], pp[2], state.tick ^ rift.id);
        let dx = ((h & 63) as i32) - 32;
        let dz = (((h >> 8) & 63) as i32) - 32;
        if dx * dx + dz * dz < 14 * 14 {
            continue;
        }
        let sx = pp[0] + dx;
        let sz = pp[2] + dz;
        let Ok(heights) =
            host.surface_heights(OVERWORLD, &[(sx, sz)], NativeHeightmap::MotionBlockingNoLeaves)
        else {
            continue;
        };
        let Some(Some(sy)) = heights.first().copied() else {
            continue;
        };
        let ground = [sx, sy - 1, sz];
        if !pos_is_corrupted(host, state, ground) {
            continue;
        }
        let mob_kind = match (rift.flavor, (h >> 16) % 8) {
            (NetherFlavor::CrimsonForest, 0..=3) => "minecraft:hoglin",
            (NetherFlavor::CrimsonForest, 4..=6) => "minecraft:piglin",
            (NetherFlavor::SoulSandValley, 0..=3) => "minecraft:wither_skeleton",
            (NetherFlavor::BasaltDeltas, 0..=4) => "minecraft:magma_cube",
            (_, 6) if rift.corrupted_blocks >= 3000 => "minecraft:blaze",
            (_, 7) if rift.corrupted_blocks >= 6000 => "minecraft:ghast",
            _ => "minecraft:piglin",
        };
        let spawn_y = if mob_kind == "minecraft:ghast" {
            f64::from(sy + 14)
        } else {
            f64::from(sy)
        };
        let mut nbt = Compound::new();
        if matches!(mob_kind, "minecraft:piglin" | "minecraft:hoglin") {
            nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
        }
        let _ = host.summon(OVERWORLD, mob_kind, [f64::from(sx) + 0.5, spawn_y, f64::from(sz) + 0.5], nbt);
        break;
    }
}

pub fn augment_potential_spawns(state: &RiftState, event: &mut PotentialSpawnsEvent<'_>) {
    if event.dimension != OVERWORLD || !state.is_quart_corrupted(event.pos) {
        return;
    }

    let push_if_missing = |entries: &mut Vec<SpawnEntry>, entity: &str, weight: u32, min: u32, max: u32| {
        if !entries.iter().any(|e| e.entity == entity) {
            entries.push(SpawnEntry {
                entity: entity.to_owned(),
                weight,
                min,
                max,
                constant: min == max,
            });
        }
    };

    match event.category {
        "monster" => {
            push_if_missing(&mut event.entries, "minecraft:piglin", 24, 2, 4);
            push_if_missing(&mut event.entries, "minecraft:magma_cube", 14, 1, 3);
            push_if_missing(&mut event.entries, "minecraft:wither_skeleton", 8, 1, 2);
            push_if_missing(&mut event.entries, "minecraft:blaze", 4, 1, 1);
            push_if_missing(&mut event.entries, "minecraft:ghast", 6, 1, 1);
        }
        "creature" => {
            push_if_missing(&mut event.entries, "minecraft:hoglin", 18, 1, 3);
            push_if_missing(&mut event.entries, "minecraft:strider", 6, 1, 2);
        }
        _ => {}
    }
}

pub fn append_nether_touched_drops(
    state: &RiftState,
    mob_id: u32,
    kind: &str,
    origin: [f64; 3],
    drops: &mut Vec<ItemStack>,
) {
    if !state.nether_touched_mobs.contains(&mob_id) {
        return;
    }
    let h = spatial_hash(
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
        u64::from(mob_id) ^ 0xD209_5001,
    );
    match strip_ns(kind) {
        "zombie" | "husk" | "drowned" | "spider" | "cave_spider" | "creeper" => {
            drops.push(ItemStack::new("minecraft:gold_nugget", 1 + (h % 3) as u32));
            if h % 3 == 0 {
                drops.push(ItemStack::new("minecraft:blaze_powder", 1));
            }
        }
        "wither_skeleton" | "magma_cube" | "blaze" => {
            if h % 4 == 0 {
                drops.push(ItemStack::new("minecraft:magma_cream", 1));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_overworld_mobs_to_nether_counterparts() {
        assert_eq!(nether_conversion_target("minecraft:pig"), Some("minecraft:piglin"));
        assert_eq!(nether_conversion_target("minecraft:villager"), Some("minecraft:piglin"));
        assert_eq!(nether_conversion_target("minecraft:cow"), Some("minecraft:hoglin"));
        assert_eq!(nether_conversion_target("minecraft:sheep"), Some("minecraft:hoglin"));
        assert_eq!(nether_conversion_target("minecraft:skeleton"), Some("minecraft:wither_skeleton"));
        assert_eq!(nether_conversion_target("minecraft:slime"), Some("minecraft:magma_cube"));
        assert_eq!(nether_conversion_target("minecraft:piglin"), None);
    }

    #[test]
    fn classifies_nether_touched_survivors_and_zombification() {
        assert!(is_nether_touched_candidate("minecraft:zombie"));
        assert!(is_nether_touched_candidate("minecraft:creeper"));
        assert!(is_piglin_or_hoglin("minecraft:piglin"));
        assert!(is_piglin_or_hoglin("minecraft:hoglin"));
        assert!(is_zombified_outcome("minecraft:zombified_piglin"));
        assert!(is_zombified_outcome("minecraft:zoglin"));
    }
}
