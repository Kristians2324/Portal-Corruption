use crate::corruption::is_air_like;
use crate::state::RiftState;
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, dist_sq, horiz_dist_sq,
    quart_block_origin, spatial_hash, strip_ns,
};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    Compound, Host, ItemStack, LivingDeathEvent, NativeHeightmap, ParticleOptions, ParticleType,
    PotentialSpawnsEvent, SpawnEntry, Tag,
};

const MOB_TICK_INTERVAL: u64 = 20;
const HOGLIN_SPREAD_INTERVAL_TICKS: u64 = 24;
const SUPPLEMENTAL_SPAWN_INTERVAL_TICKS: u64 = 100;

/// True for Nether mobs native to or brought forth by Rifts.
pub fn is_nether_predator(kind: &str) -> bool {
    matches!(
        strip_ns(kind),
        "piglin"
            | "piglin_brute"
            | "hoglin"
            | "zombified_piglin"
            | "zoglin"
            | "wither_skeleton"
            | "blaze"
            | "magma_cube"
            | "ghast"
    )
}

/// Maps any Overworld creature killed by Nether mobs (or dying inside corrupted
/// territory) to its transformed Nether counterpart (using vanilla mobs/models).
pub fn nether_death_conversion_target(kind: &str) -> Option<&'static str> {
    match strip_ns(kind) {
        "pig" | "cow" | "mooshroom" | "sheep" | "goat" | "horse" | "donkey" | "mule"
        | "llama" | "camel" | "sniffer" => Some("minecraft:hoglin"),
        "villager" | "wandering_trader" | "pillager" | "evoker" | "illusioner" => {
            Some("minecraft:piglin")
        }
        "iron_golem" | "vindicator" | "ravager" => Some("minecraft:piglin_brute"),
        "zombie" | "husk" | "drowned" | "zombie_villager" => Some("minecraft:zombified_piglin"),
        "skeleton" | "stray" | "bogged" | "spider" | "cave_spider" | "phantom" => {
            Some("minecraft:wither_skeleton")
        }
        "slime" | "chicken" | "rabbit" | "wolf" | "cat" | "fox" | "frog" | "bat" => {
            Some("minecraft:magma_cube")
        }
        "witch" | "creeper" | "breeze" => Some("minecraft:blaze"),
        _ => None,
    }
}

/// Hostile Overworld mobs that gain Nether-Touched buffs & bonus drops while alive
/// in corruption (and still transform on death when slain by Nether mobs).
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
    for sy in [pos[1].clamp(-56, 200), 64, 0] {
        if let Some(name) = host.biome([pos[0], sy, pos[2]])
            && NetherFlavor::from_name(&name).is_some()
        {
            return true;
        }
    }
    false
}

pub fn tick_mobs(host: &mut dyn Host, state: &mut RiftState) {
    if state.rifts.is_empty() {
        return;
    }

    let do_combat = state.tick % MOB_TICK_INTERVAL == 5;
    let do_hoglin = state.tick % HOGLIN_SPREAD_INTERVAL_TICKS == 11;
    let do_spawn = state.tick % SUPPLEMENTAL_SPAWN_INTERVAL_TICKS == 45;
    if !do_combat && !do_hoglin && !do_spawn {
        return;
    }

    let entities = host.level_entities(OVERWORLD);

    if do_combat {
        process_predator_hunting_and_buffs(host, state, &entities);
    }
    if do_hoglin {
        process_hoglin_edge_spreading(host, state, &entities);
    }
    if do_spawn {
        process_supplemental_spawns(host, state, &entities);
    }
}

/// Keeps Nether mob tracking updated, directs Nether mobs to actively hunt & kill
/// Overworld creatures, and grants Fire Resistance / Strength to surviving Nether-Touched hostiles.
fn process_predator_hunting_and_buffs(
    host: &mut dyn Host,
    state: &mut RiftState,
    entities: &[(u32, String, [f64; 3])],
) {
    let mut hunters: Vec<(u32, [f64; 3])> = Vec::new();
    let mut prey: Vec<(u32, [f64; 3])> = Vec::new();

    state.known_nether_mobs.clear();
    for (id, kind, pos) in entities {
        if is_nether_predator(kind) {
            state.known_nether_mobs.insert(*id);
            if matches!(
                strip_ns(kind),
                "piglin" | "piglin_brute" | "hoglin" | "wither_skeleton" | "blaze" | "zoglin"
            ) {
                hunters.push((*id, *pos));
            }
            if is_piglin_or_hoglin(kind) && (state.tick + u64::from(*id)) % 80 < MOB_TICK_INTERVAL {
                rift_bridge::enqueue_mob_buff(*id, 20 * 90, 0, 0);
            }
        } else if nether_death_conversion_target(kind).is_some() {
            prey.push((*id, *pos));
        }

        let bp = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];
        if is_nether_touched_candidate(kind) && pos_is_corrupted(host, state, bp) {
            if state.nether_touched_mobs.insert(*id) {
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
        }
    }

    if hunters.is_empty() || prey.is_empty() {
        return;
    }

    let mut pairs = Vec::new();
    let hunt_range_sq = 32.0 * 32.0;

    for (hunter_id, h_pos) in hunters {
        let mut best: Option<(u32, f64)> = None;
        for (prey_id, p_pos) in &prey {
            let dx = h_pos[0] - p_pos[0];
            let dy = h_pos[1] - p_pos[1];
            let dz = h_pos[2] - p_pos[2];
            let d2 = dx * dx + dy * dy + dz * dz;
            if d2 <= hunt_range_sq && best.is_none_or(|(_, b)| d2 < b) {
                best = Some((*prey_id, d2));
            }
        }
        if let Some((victim_id, _)) = best {
            pairs.push((hunter_id, victim_id));
        }
    }

    if !pairs.is_empty() {
        rift_bridge::enqueue_mob_targets(&pairs);
    }
}

/// Converts any Overworld creature killed by a Nether mob (or slain in corrupted
/// territory) into its Nether version on death.
pub fn handle_living_death(
    host: &mut dyn Host,
    state: &mut RiftState,
    event: &LivingDeathEvent<'_>,
) {
    if strip_ns(event.kind) == "player" {
        return;
    }
    let Some(target_kind) = nether_death_conversion_target(event.kind) else {
        return;
    };
    let Some(pos) = host.entity(event.entity).and_then(|e| e.position(host)) else {
        return;
    };
    let bp = [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ];

    let attacker_is_nether = event.attacker != 0
        && (state.known_nether_mobs.contains(&event.attacker)
            || state.nether_touched_mobs.contains(&event.attacker)
            || host
                .entity(event.attacker)
                .and_then(|e| e.kind(host))
                .is_some_and(|k| is_nether_predator(&k)));

    if !attacker_is_nether && !pos_is_corrupted(host, state, bp) {
        return;
    }

    let mut nbt = Compound::new();
    if matches!(target_kind, "minecraft:piglin" | "minecraft:hoglin") {
        nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
    }

    if let Some(new_id) = host.summon(OVERWORLD, target_kind, pos, nbt) {
        state.known_nether_mobs.insert(new_id);
        rift_bridge::enqueue_mob_buff(new_id, 20 * 180, 20 * 120, 0);
    }

    if let Some(level) = host.level(OVERWORLD) {
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(ParticleType::Flame),
            None,
            [pos[0], pos[1] + 0.8, pos[2]],
            [0.45, 0.5, 0.45],
            0.04,
            18,
        );
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(ParticleType::CrimsonSpore),
            None,
            [pos[0], pos[1] + 0.8, pos[2]],
            [0.5, 0.4, 0.5],
            0.03,
            12,
        );
        let _ = level.play_sound(
            host,
            "minecraft:entity.zombie_villager.converted",
            pos,
            5,
            0.85,
            0.9,
        );
    }
}

/// Hoglins act as border vanguard: near the edge of corrupted land they root
/// and trample uncorrupted Overworld grass/dirt into crimson nylium/netherrack
/// and enqueue those columns cleanly!
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
    let mut quart_records = Vec::new();

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
        let near_corrupt = pos_is_corrupted(host, state, base)
            || state
                .rifts
                .iter()
                .any(|r| !r.contained && horiz_dist_sq(r.center, base) <= r.max_radius_sq + 256);
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
                        state.pending_frontier_columns.push_back([p[0], p[2]]);
                        if state.mark_quart_corrupted(p) {
                            let qb = quart_block_origin(p);
                            biome_updates.push((qb, NetherFlavor::CrimsonForest.to_u8()));
                            quart_records.push(crate::types::quart_coords(p));
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
        rift_bridge::record_quart_cells(&quart_records);
        state.dirty = true;
    }
}

fn process_supplemental_spawns(
    host: &mut dyn Host,
    state: &mut RiftState,
    entities: &[(u32, String, [f64; 3])],
) {
    let nether_mob_count = entities
        .iter()
        .filter(|(_, k, _)| is_nether_predator(k))
        .count();
    if nether_mob_count >= 34 {
        return;
    }

    // First look for an uncontained rift near a player, else pick an active core
    // (kept loaded by ticket) so invaders emerge into the world continuously.
    let players = host.server_players();
    let mut spawn_anchor: Option<(&crate::types::RiftSource, [i32; 3])> = None;

    for player in &players {
        if player.dimension != OVERWORLD {
            continue;
        }
        let pp = [
            player.position[0].floor() as i32,
            player.position[1].floor() as i32,
            player.position[2].floor() as i32,
        ];
        if let Some(rift) = state
            .rifts
            .iter()
            .filter(|r| horiz_dist_sq(r.center, pp) <= 96 * 96)
            .min_by_key(|r| dist_sq(r.center, pp))
        {
            spawn_anchor = Some((rift, pp));
            break;
        }
    }

    if spawn_anchor.is_none()
        && let Some(rift) = state.rifts.iter().find(|r| !r.contained)
    {
        spawn_anchor = Some((rift, rift.center));
    }

    let Some((rift, anchor)) = spawn_anchor else {
        return;
    };

    let h = spatial_hash(anchor[0], anchor[1], anchor[2], state.tick ^ rift.id);
    let dx = ((h & 31) as i32) - 16;
    let dz = (((h >> 8) & 31) as i32) - 16;
    let sx = anchor[0] + dx;
    let sz = anchor[2] + dz;
    let Ok(heights) =
        host.surface_heights(OVERWORLD, &[(sx, sz)], NativeHeightmap::MotionBlockingNoLeaves)
    else {
        return;
    };
    let Some(Some(sy)) = heights.first().copied() else {
        return;
    };
    let ground = [sx, sy - 1, sz];
    if !pos_is_corrupted(host, state, ground) && horiz_dist_sq(rift.center, ground) > 16 * 16 {
        return;
    }

    let mob_kind = match (rift.flavor, (h >> 16) % 8) {
        (NetherFlavor::CrimsonForest, 0..=3) => "minecraft:hoglin",
        (NetherFlavor::CrimsonForest, 4..=6) => "minecraft:piglin",
        (NetherFlavor::SoulSandValley, 0..=3) => "minecraft:wither_skeleton",
        (NetherFlavor::BasaltDeltas, 0..=4) => "minecraft:magma_cube",
        (_, 6) if rift.corrupted_blocks >= 2_500 => "minecraft:blaze",
        (_, 7) if rift.corrupted_blocks >= 5_000 => "minecraft:ghast",
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
    if let Some(id) = host.summon(
        OVERWORLD,
        mob_kind,
        [f64::from(sx) + 0.5, spawn_y, f64::from(sz) + 0.5],
        nbt,
    ) {
        state.known_nether_mobs.insert(id);
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
    fn maps_killed_overworld_creatures_to_nether_versions() {
        // Farm animals & beasts become Hoglins; villagers & traders become Piglins.
        assert_eq!(nether_death_conversion_target("minecraft:pig"), Some("minecraft:hoglin"));
        assert_eq!(nether_death_conversion_target("minecraft:cow"), Some("minecraft:hoglin"));
        assert_eq!(nether_death_conversion_target("minecraft:sheep"), Some("minecraft:hoglin"));
        assert_eq!(nether_death_conversion_target("minecraft:villager"), Some("minecraft:piglin"));
        assert_eq!(nether_death_conversion_target("minecraft:wandering_trader"), Some("minecraft:piglin"));
        assert_eq!(nether_death_conversion_target("minecraft:iron_golem"), Some("minecraft:piglin_brute"));
        assert_eq!(nether_death_conversion_target("minecraft:zombie"), Some("minecraft:zombified_piglin"));
        assert_eq!(nether_death_conversion_target("minecraft:skeleton"), Some("minecraft:wither_skeleton"));
        assert_eq!(nether_death_conversion_target("minecraft:slime"), Some("minecraft:magma_cube"));
        assert_eq!(nether_death_conversion_target("minecraft:creeper"), Some("minecraft:blaze"));
        // Nether predators don't convert into another victim.
        assert_eq!(nether_death_conversion_target("minecraft:piglin"), None);
        assert_eq!(nether_death_conversion_target("minecraft:hoglin"), None);
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
