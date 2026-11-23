use crate::atmosphere::ParticlePalette;
use crate::corruption::{is_air_like, is_open_space, is_water_like, transform_block};
use crate::state::RiftState;
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, dist_sq, horiz_dist_sq,
    spatial_hash, strip_ns,
};
use mod_api::{
    ActiveEffect, BlockState, BlockUpdateFlags, Compound, EntityRef, EquipmentSlot,
    FinalizeSpawnEvent, GoalSpec, Heightmap, Host, Identifier, ItemStack, LevelRef,
    LivingChangeTargetEvent, LivingDeathEvent, LivingIncomingDamageEvent, MobFilter,
    MobSpawnPlacementCheckEvent, MobSpawnPositionCheckEvent, ParticleOptions, PotentialSpawnsEvent,
    ServerHost, SoundSource, SpawnCheck, SpawnEntry, SpawnReason, Tag,
};
use std::collections::HashMap;

const MOB_TICK_INTERVAL: u64 = 10;
const HOGLIN_SPREAD_INTERVAL_TICKS: u64 = 24;
const SUPPLEMENTAL_SPAWN_INTERVAL_TICKS: u64 = 60;
const STRIDER_SPAWN_INTERVAL_TICKS: u64 = 60;

/// Mobs only unlock once corruption has converted a pretty large Nether biome
/// (~64-block diameter minimum across several chunks).
pub const MIN_RIFT_BLOCKS_FOR_MOBS: u64 = 55_000;
pub const MIN_RADIUS_SQ_FOR_MOBS: i64 = 32 * 32;

#[cfg(test)]
pub fn rift_mobs_unlocked(rift: &crate::types::RiftSource) -> bool {
    rift.corrupted_blocks >= MIN_RIFT_BLOCKS_FOR_MOBS && rift.max_radius_sq >= MIN_RADIUS_SQ_FOR_MOBS
}

pub fn rift_mobs_unlocked_in_state(state: &RiftState, rift_idx: usize) -> bool {
    let Some(rift) = state.rifts.get(rift_idx) else {
        return false;
    };
    if rift.pacified || rift.regressing {
        return false;
    }
    let w = state.connected_wasteland_for(rift_idx);
    w.total_corrupted_blocks >= MIN_RIFT_BLOCKS_FOR_MOBS
        && w.total_area_radius_sq >= MIN_RADIUS_SQ_FOR_MOBS
}

/// True only if `pos` is covered by corrupted terrain belonging to a Rift (or merged
/// touching Rift cluster) that has grown into a large Nether biome
/// (`>= MIN_RIFT_BLOCKS_FOR_MOBS` and area radius `>= 32`).
pub fn location_unlocks_nether_mobs(state: &RiftState, pos: [i32; 3]) -> bool {
    if !state.is_quart_corrupted(pos) {
        return false;
    }
    state.rifts.iter().enumerate().any(|(idx, rift)| {
        !rift.pacified
            && !rift.regressing
            && rift_mobs_unlocked_in_state(state, idx)
            && horiz_dist_sq(rift.center, pos)
                <= (rift.max_radius_sq + 256).max(MIN_RADIUS_SQ_FOR_MOBS)
    })
}

/// True for world-generated / natural / chunk / patrol / structure spawns whose ecology is
/// governed by biome rules (as opposed to `/summon`, spawn eggs, conversion, or breeding).
pub fn is_world_spawn_reason(reason: SpawnReason) -> bool {
    matches!(
        reason,
        SpawnReason::Natural
            | SpawnReason::ChunkGeneration
            | SpawnReason::Spawner
            | SpawnReason::TrialSpawner
            | SpawnReason::Reinforcement
            | SpawnReason::Patrol
            | SpawnReason::Structure
            | SpawnReason::Event
            | SpawnReason::Jockey
            | SpawnReason::Triggered
    )
}

/// True ONLY for Nether fauna appropriate for converted Overworld Nether biomes.
/// Rejects normal Overworld animals, monsters (zombies, creepers, skeletons, spiders,
/// drowned, phantoms, slimes, witches, pillagers), bats, squids, and fish.
pub fn is_allowed_in_nether_biome(kind: &str, flavor: Option<NetherFlavor>) -> bool {
    let short = strip_ns(kind);
    if is_nether_predator(kind) || matches!(short, "strider" | "wither") {
        return true;
    }
    match (short, flavor) {
        (
            "enderman",
            Some(NetherFlavor::WarpedForest | NetherFlavor::SoulSandValley) | None,
        ) => true,
        _ => false,
    }
}

/// Pure decision helper shared by `finalize_spawn` and unit tests:
/// 1) Never lets normal Overworld mobs naturally spawn inside converted Nether biomes/quarts.
/// 2) Still blocks premature Nether mob spawns around immature (`< 55k` / `< 32` radius)
///    or pacified/regressing Rifts.
pub fn should_cancel_world_spawn_kind(
    kind: &str,
    reason: SpawnReason,
    in_nether_zone: bool,
    biome_flavor: Option<NetherFlavor>,
    unlocks_nether_mobs: bool,
    near_any_rift: bool,
) -> bool {
    if !is_world_spawn_reason(reason) {
        return false;
    }
    if in_nether_zone && !is_allowed_in_nether_biome(kind, biome_flavor) {
        return true;
    }
    if (is_nether_predator(kind) || strip_ns(kind) == "strider")
        && near_any_rift
        && !unlocks_nether_mobs
    {
        return true;
    }
    false
}

/// Blocks normal Overworld mobs inside converted Nether biomes AND blocks premature Nether
/// spawns inside immature / pacified / regressing Rifts (while letting commands, spawn eggs,
/// conversions, and already-alive survivors remain).
pub fn should_cancel_finalize_spawn(
    host: &dyn Host,
    state: &RiftState,
    event: &FinalizeSpawnEvent<'_>,
) -> bool {
    if event.dimension != OVERWORLD {
        return false;
    }
    let bp = [
        event.pos[0].floor() as i32,
        event.pos[1].floor() as i32,
        event.pos[2].floor() as i32,
    ];
    let biome_flavor = host
        .level(OVERWORLD)
        .and_then(|l| local_corrupt_flavor(host, l, bp));
    let in_nether_zone = biome_flavor.is_some() || state.is_quart_corrupted(bp);
    let near_any_rift = in_nether_zone
        || state
            .rifts
            .iter()
            .any(|r| horiz_dist_sq(r.center, bp) <= (r.max_radius_sq + 512).max(48 * 48));
    if !near_any_rift {
        return false;
    }
    should_cancel_world_spawn_kind(
        event.kind,
        event.reason,
        in_nether_zone,
        biome_flavor,
        location_unlocks_nether_mobs(state, bp),
        near_any_rift,
    )
}

fn is_nether_open_headroom(name: &str) -> bool {
    is_open_space(name)
        || matches!(
            strip_ns(name),
            "crimson_roots"
                | "warped_roots"
                | "nether_sprouts"
                | "crimson_fungus"
                | "warped_fungus"
                | "twisting_vines"
                | "twisting_vines_plant"
                | "weeping_vines"
                | "weeping_vines_plant"
                | "fire"
                | "soul_fire"
        )
}

fn is_standable_nether_ground(name: &str) -> bool {
    let short = strip_ns(name);
    !name.is_empty()
        && !is_air_like(name)
        && !crate::corruption::is_water_like(name)
        && !is_nether_open_headroom(name)
        && !matches!(
            short,
            "lava"
                | "flowing_lava"
                | "bedrock"
                | "barrier"
                | "nether_wart_block"
                | "warped_wart_block"
        )
}

fn valid_nether_spawn_spot(
    host: &dyn Host,
    level: LevelRef,
    kind: &str,
    bp: [i32; 3],
) -> bool {
    if !level.is_loaded(host, bp) {
        return false;
    }
    let short = strip_ns(kind);
    if short == "ghast" {
        if bp[1] < 48 || (spatial_hash(bp[0], bp[1], bp[2], 0x6A57) % 4) != 0 {
            return false;
        }
        return [
            bp,
            [bp[0], bp[1] + 1, bp[2]],
            [bp[0], bp[1] + 2, bp[2]],
            [bp[0] + 1, bp[1] + 1, bp[2]],
            [bp[0], bp[1] + 1, bp[2] + 1],
        ]
        .into_iter()
        .all(|p| level.block(host, p).is_some_and(|k| is_air_like(&k.id(host))));
    }
    if short == "strider" {
        let on_lava = level
            .block(host, bp)
            .is_some_and(|k| strip_ns(&k.id(host)) == "lava")
            || (bp[1] > OVERWORLD_MIN_Y
                && level
                    .block(host, [bp[0], bp[1] - 1, bp[2]])
                    .is_some_and(|k| strip_ns(&k.id(host)) == "lava"));
        let clear = bp[1] < OVERWORLD_MAX_Y
            && level
                .block(host, [bp[0], bp[1] + 1, bp[2]])
                .is_some_and(|k| is_nether_open_headroom(&k.id(host)));
        return on_lava && clear;
    }

    if bp[1] <= OVERWORLD_MIN_Y || bp[1] + 2 >= OVERWORLD_MAX_Y {
        return false;
    }
    let below_ok = level
        .block(host, [bp[0], bp[1] - 1, bp[2]])
        .is_some_and(|k| is_standable_nether_ground(&k.id(host)));
    if !below_ok {
        return false;
    }
    let body_clear = level
        .block(host, bp)
        .is_some_and(|k| is_nether_open_headroom(&k.id(host)))
        && level
            .block(host, [bp[0], bp[1] + 1, bp[2]])
            .is_some_and(|k| is_nether_open_headroom(&k.id(host)));
    if !body_clear {
        return false;
    }
    if matches!(short, "enderman" | "wither_skeleton" | "hoglin") {
        let top_clear = level
            .block(host, [bp[0], bp[1] + 2, bp[2]])
            .is_some_and(|k| is_nether_open_headroom(&k.id(host)));
        if !top_clear {
            return false;
        }
    }
    true
}

fn evaluate_nether_spawn_check(
    host: &dyn Host,
    state: &RiftState,
    dimension: &str,
    kind: &str,
    reason: SpawnReason,
    bp: [i32; 3],
    level: LevelRef,
) -> SpawnCheck {
    if dimension != OVERWORLD || !is_world_spawn_reason(reason) {
        return SpawnCheck::Default;
    }
    let biome_flavor = local_corrupt_flavor(host, level, bp);
    let in_nether_zone = biome_flavor.is_some() || state.is_quart_corrupted(bp);
    if !in_nether_zone {
        return SpawnCheck::Default;
    }
    if !is_allowed_in_nether_biome(kind, biome_flavor) {
        return SpawnCheck::Fail;
    }
    if !location_unlocks_nether_mobs(state, bp) {
        return SpawnCheck::Fail;
    }
    let peaceful = level.difficulty(host) == Some("peaceful");
    if peaceful && strip_ns(kind) != "strider" {
        return SpawnCheck::Fail;
    }
    if valid_nether_spawn_spot(host, level, kind, bp) {
        SpawnCheck::Succeed
    } else {
        SpawnCheck::Fail
    }
}

/// Ensures normal Overworld mobs fail early at `mob_spawn_placement_check` inside Nether biomes,
/// while allowing unlocked Nether mobs to pass Overworld daylight checks on valid Nether ground.
pub fn handle_mob_spawn_placement_check(
    host: &dyn Host,
    state: &RiftState,
    event: &mut MobSpawnPlacementCheckEvent<'_>,
) {
    let bp = [event.pos.x, event.pos.y, event.pos.z];
    let check = evaluate_nether_spawn_check(
        host,
        state,
        event.dimension,
        event.kind,
        event.reason,
        bp,
        event.level,
    );
    if check != SpawnCheck::Default {
        *event.result = check;
    }
}

/// Mirrors placement checks at `mob_spawn_position_check` so Overworld skylight (`Monster` dark
/// check) or Ghast 5% rolls don't reject valid unlocked Nether biome spawns a second time.
pub fn handle_mob_spawn_position_check(
    host: &dyn Host,
    state: &RiftState,
    event: &mut MobSpawnPositionCheckEvent<'_>,
) {
    let bp = [
        event.pos[0].floor() as i32,
        event.pos[1].floor() as i32,
        event.pos[2].floor() as i32,
    ];
    let check = evaluate_nether_spawn_check(
        host,
        state,
        event.dimension,
        event.kind,
        event.reason,
        bp,
        event.level,
    );
    if check != SpawnCheck::Default {
        *event.result = check;
    }
}

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

/// True for Overworld hostiles and golems that immediately rally to fight Nether invaders on sight.
pub fn is_overworld_defender_mob(kind: &str) -> bool {
    matches!(
        strip_ns(kind),
        "zombie"
            | "husk"
            | "drowned"
            | "zombie_villager"
            | "skeleton"
            | "stray"
            | "bogged"
            | "spider"
            | "cave_spider"
            | "pillager"
            | "vindicator"
            | "evoker"
            | "ravager"
            | "witch"
            | "slime"
            | "breeze"
            | "phantom"
            | "silverfish"
            | "warden"
            | "iron_golem"
            | "snow_golem"
    )
}

/// True when a battle death involving a Nether combatant should scorch uncorrupted ground beneath it.
pub fn should_corrupt_kill_patch(
    attacker_is_nether: bool,
    victim_is_nether: bool,
    attacker_has_id: bool,
    attacker_is_player: bool,
) -> bool {
    (attacker_is_nether && !victim_is_nether)
        || (victim_is_nether && attacker_has_id && !attacker_is_player)
}

/// True when any non-Nether mob dies by Nether hands OR inside/near an active spreading Rift front
/// or Nether battle (covering Blaze/Ghast fire, knockback falls, and kills on uncorrupted border land).
pub fn should_corrupt_non_nether_death(
    victim_is_nether: bool,
    attacker_is_nether: bool,
    near_active_nether_front: bool,
) -> bool {
    !victim_is_nether && (attacker_is_nether || near_active_nether_front)
}

/// Maps any Overworld creature killed by Nether mobs (or dying inside an established
/// large corrupted Nether biome) to its transformed Nether counterpart using vanilla mobs.
pub fn nether_death_conversion_target(kind: &str) -> Option<&'static str> {
    match strip_ns(kind) {
        "pig" | "cow" | "mooshroom" | "sheep" | "goat" | "horse" | "donkey" | "mule"
        | "llama" | "camel" | "sniffer" | "polar_bear" => Some("minecraft:hoglin"),
        "villager" | "wandering_trader" | "pillager" | "evoker" | "illusioner" => {
            Some("minecraft:piglin")
        }
        "iron_golem" | "vindicator" | "ravager" => Some("minecraft:piglin_brute"),
        "zombie" | "husk" | "drowned" | "zombie_villager" => Some("minecraft:zombified_piglin"),
        "skeleton" | "stray" | "bogged" | "spider" | "cave_spider" | "phantom" => {
            Some("minecraft:wither_skeleton")
        }
        "slime" | "chicken" | "rabbit" | "wolf" | "cat" | "fox" | "frog" | "bat" | "armadillo"
        | "ocelot" | "panda" | "turtle" => Some("minecraft:magma_cube"),
        "witch" | "creeper" | "breeze" => Some("minecraft:blaze"),
        _ => None,
    }
}

/// Hostile Overworld mobs that gain Nether-Touched buffs & bonus drops while alive
/// in large corrupted biomes (and still transform on death when slain by Nether mobs).
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

pub fn local_corrupt_flavor(
    host: &dyn Host,
    level: LevelRef,
    pos: [i32; 3],
) -> Option<NetherFlavor> {
    for sy in [pos[1].clamp(-56, 200), 64, 0] {
        if let Some(name) = level.biome_at(host, [pos[0], sy, pos[2]])
            && let Some(flavor) = NetherFlavor::from_name(&name)
        {
            return Some(flavor);
        }
    }
    None
}

pub fn pos_is_corrupted(
    host: &dyn Host,
    level: LevelRef,
    state: &RiftState,
    pos: [i32; 3],
) -> bool {
    state.is_quart_corrupted(pos) || local_corrupt_flavor(host, level, pos).is_some()
}

/// Marks a Piglin, Piglin Brute, or Hoglin immune to Overworld zombification and resets
/// `TimeInOverworld` to 0 so vanilla never enters a repeated shaking / `playConvertedSound` loop.
pub fn stabilize_zombification_immune(host: &mut dyn ServerHost, entity_id: u32) {
    let Some(ent) = host.entity(entity_id) else {
        return;
    };
    let mut nbt = Compound::new();
    nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
    nbt.insert("TimeInOverworld".into(), Tag::Int(0));
    let _ = ent.merge_nbt(host, nbt);
}

pub fn should_block_zombification(
    host: &dyn Host,
    state: &RiftState,
    entity_id: u32,
    kind: &str,
    outcome: &str,
) -> bool {
    if !is_piglin_or_hoglin(kind) || !is_zombified_outcome(outcome) {
        return false;
    }
    let Some(level) = host.level(OVERWORLD) else {
        return false;
    };
    let Some(pos) = host.entity(entity_id).and_then(|e| e.position(host)) else {
        return false;
    };
    let bp = [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ];
    pos_is_corrupted(host, level, state, bp)
}

pub fn paint_biome_column(
    level: LevelRef,
    host: &mut dyn ServerHost,
    x: i32,
    z: i32,
    flavor: NetherFlavor,
) {
    let biome_id = flavor.biome_id();
    let qx = (x >> 2) << 2;
    let qz = (z >> 2) << 2;
    for qy in (-64..=316).step_by(4) {
        let _ = level.set_biome(host, [qx, qy, qz], biome_id);
    }
}

fn apply_mob_effects(
    host: &mut dyn ServerHost,
    entity: EntityRef,
    fire_ticks: i32,
    strength_ticks: i32,
) {
    let Some(living) = host.living(entity) else {
        return;
    };
    if fire_ticks > 0
        && let Some(fire_res) = host.effect_key("minecraft:fire_resistance")
    {
        let _ = living.add_effect(
            host,
            &ActiveEffect::new(fire_res, fire_ticks, 0, true, false, false),
        );
    }
    if strength_ticks > 0
        && let Some(strength) = host.effect_key("minecraft:strength")
    {
        let _ = living.add_effect(
            host,
            &ActiveEffect::new(strength, strength_ticks, 0, true, true, false),
        );
    }
}

pub fn tick_mobs(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    particles: Option<&ParticlePalette>,
) {
    if state.rifts.is_empty() {
        return;
    }

    let do_combat = state.tick % MOB_TICK_INTERVAL == 5;
    let do_hoglin = state.tick % HOGLIN_SPREAD_INTERVAL_TICKS == 11;
    let do_spawn = state.tick % SUPPLEMENTAL_SPAWN_INTERVAL_TICKS == 45;
    let do_strider = state.tick % STRIDER_SPAWN_INTERVAL_TICKS == 25;
    if !do_combat && !do_hoglin && !do_spawn && !do_strider {
        return;
    }

    let Some(raw_entities) = level.all_entities(host) else {
        return;
    };

    let mut entities: Vec<(EntityRef, u32, String, [f64; 3])> =
        Vec::with_capacity(raw_entities.len());
    for ent in raw_entities {
        let Some(kind_key) = ent.kind(host) else {
            continue;
        };
        let Some(pos) = ent.position(host) else {
            continue;
        };
        entities.push((ent, ent.id(), kind_key.id(host), pos));
    }

    if do_combat {
        process_predator_hunting_and_buffs(host, level, state, &entities, particles);
    }
    if do_hoglin {
        process_hoglin_edge_spreading(host, level, state, &entities, particles);
    }
    if do_spawn {
        process_supplemental_spawns(host, level, state, &entities);
    }
    if do_strider {
        try_spawn_lava_striders(host, level, state, &entities);
    }
}

/// Keeps Nether mob tracking updated, directs Nether mobs to actively hunt & kill
/// Overworld creatures via `MobRef::set_target` + `set_brain_target`, and grants
/// Fire Resistance / Strength to surviving Nether-Touched hostiles once corruption matures.
fn process_predator_hunting_and_buffs(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    entities: &[(EntityRef, u32, String, [f64; 3])],
    particles: Option<&ParticlePalette>,
) {
    let mut hunters: Vec<(EntityRef, String, [f64; 3])> = Vec::new();
    let mut prey: Vec<(EntityRef, [f64; 3])> = Vec::new();
    let mut defenders: Vec<(EntityRef, [f64; 3])> = Vec::new();
    let mut fire_buffs: Vec<EntityRef> = Vec::new();
    let mut immunize_mobs: Vec<u32> = Vec::new();
    let mut touched_buffs: Vec<(EntityRef, [f64; 3])> = Vec::new();
    let mut withers: Vec<(EntityRef, u32, [f64; 3])> = Vec::new();

    state.known_nether_mobs.clear();
    state.known_withers.clear();
    state.citadel_guards.clear();
    for (ent, id, kind, pos) in entities {
        let bp = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];

        if strip_ns(kind) == "wither" {
            let is_sovereign = ent
                .tags(host)
                .is_some_and(|ts| ts.iter().any(|t| t == "rift_bleed:sovereign"))
                || state
                    .citadels
                    .iter()
                    .any(|c| horiz_dist_sq(*c, bp) <= 56 * 56);
            if is_sovereign {
                state.known_nether_mobs.insert(*id);
                state.known_withers.insert(*id);
                withers.push((*ent, *id, *pos));
            }
            continue;
        }

        if is_nether_predator(kind) {
            state.known_nether_mobs.insert(*id);
            if ent.tags(host).is_some_and(|ts| {
                ts.iter()
                    .any(|t| t == "rift_bleed:nether_ally" || t == "rift_bleed:citadel_guard")
            }) {
                state.citadel_guards.insert(*id);
            }
            if matches!(
                strip_ns(kind),
                "piglin"
                    | "piglin_brute"
                    | "hoglin"
                    | "wither_skeleton"
                    | "blaze"
                    | "zoglin"
                    | "magma_cube"
                    | "zombified_piglin"
                    | "ghast"
            ) {
                hunters.push((*ent, strip_ns(kind).to_owned(), *pos));
            }
            if is_piglin_or_hoglin(kind) {
                if (state.tick + u64::from(*id)) % 80 < MOB_TICK_INTERVAL {
                    fire_buffs.push(*ent);
                }
                if !state.zombification_immune_mobs.contains(id)
                    && pos_is_corrupted(host, level, state, bp)
                {
                    state.zombification_immune_mobs.insert(*id);
                    immunize_mobs.push(*id);
                }
            }
        } else if nether_death_conversion_target(kind).is_some() {
            prey.push((*ent, *pos));
            if is_overworld_defender_mob(kind) {
                defenders.push((*ent, *pos));
            }
        } else if is_overworld_defender_mob(kind) {
            defenders.push((*ent, *pos));
        }

        if is_nether_touched_candidate(kind) {
            let already_touched = state.nether_touched_mobs.contains(id);
            let active_touched = if already_touched {
                state.is_quart_corrupted(bp)
            } else if location_unlocks_nether_mobs(state, bp) {
                state.nether_touched_mobs.insert(*id);
                touched_buffs.push((*ent, *pos));
                true
            } else {
                false
            };
            if active_touched
                && (state.tick + u64::from(*id)) % 40 < MOB_TICK_INTERVAL
                && let Some(pal) = particles
            {
                let _ = level.send_particles(
                    host,
                    &ParticleOptions::simple(pal.small_flame),
                    None,
                    [pos[0], pos[1] + 0.8, pos[2]],
                    [0.25, 0.35, 0.25],
                    0.01,
                    2,
                );
            }
        }
    }
    state
        .zombification_immune_mobs
        .retain(|id| state.known_nether_mobs.contains(id));
    state
        .magma_melee_ready
        .retain(|id| state.known_nether_mobs.contains(id));

    for id in immunize_mobs {
        stabilize_zombification_immune(host, id);
    }
    for ent in fire_buffs {
        apply_mob_effects(host, ent, 20 * 90, 0);
    }
    for (ent, _pos) in touched_buffs {
        apply_mob_effects(host, ent, 20 * 600, 20 * 600);
    }

    // Ensure Sovereign Withers target only Players, stay tethered inside their Citadel
    // (never flying away from the portal), and keep allied guards off each other / Wither.
    for (w_ent, _, w_pos) in withers {
        let owning_citadel = w_ent
            .tags(host)
            .and_then(|ts| {
                ts.iter()
                    .find_map(|t| t.strip_prefix("rift_bleed:rift_")?.parse::<u64>().ok())
            })
            .and_then(|rid| {
                state.rifts.iter().find(|r| r.id == rid).and_then(|r| {
                    state
                        .citadels
                        .iter()
                        .copied()
                        .min_by_key(|c| horiz_dist_sq(*c, r.center))
                        .filter(|c| horiz_dist_sq(*c, r.center) <= 16 * 16)
                        .or(Some(r.center))
                })
            })
            .or_else(|| {
                let wb = [w_pos[0] as i32, w_pos[1] as i32, w_pos[2] as i32];
                state
                    .citadels
                    .iter()
                    .copied()
                    .min_by_key(|c| horiz_dist_sq(*c, wb))
                    .filter(|c| horiz_dist_sq(*c, wb) <= 120 * 120)
            });

        if let Some(mob) = host.mob(w_ent) {
            if let Some(goals) = mob.target_goals(host) {
                let needs_fix = goals.iter().any(|g| g.id == "hurt_by_target")
                    || !goals.iter().any(|g| g.id == "nearest_target");
                if needs_fix {
                    let _ = mob.remove_target_goal(host, "nearest_target");
                    let _ = mob.remove_target_goal(host, "hurt_by_target");
                    let _ = mob.add_target_goal(
                        host,
                        2,
                        &GoalSpec::NearestTarget {
                            prey: MobFilter::Player,
                            must_see: false,
                            must_reach: false,
                            interval: 10,
                        },
                    );
                }
            }
            if let Some(tgt) = mob.target(host)
                && tgt
                    .entity()
                    .kind(host)
                    .is_some_and(|k| is_nether_predator(&k.id(host)))
            {
                let _ = mob.set_target(host, None);
            }

            if let Some(c) = owning_citadel {
                let anchor_i = [c[0], (c[1] + 10).clamp(OVERWORLD_MIN_Y + 8, OVERWORLD_MAX_Y - 12), c[2]];
                let _ = mob.restrict_to(host, anchor_i, 26);

                let anchor = [
                    f64::from(anchor_i[0]) + 0.5,
                    f64::from(anchor_i[1]),
                    f64::from(anchor_i[2]) + 0.5,
                ];
                let dx = w_pos[0] - anchor[0];
                let dy = w_pos[1] - f64::from(c[1]);
                let dz = w_pos[2] - anchor[2];
                let hd2 = dx * dx + dz * dz;
                if hd2 > 40.0 * 40.0 || dy > 36.0 || dy < -10.0 {
                    let _ = w_ent.set_velocity(host, [0.0, 0.0, 0.0]);
                    let _ = w_ent.teleport_to(host, anchor, None);
                    if let Some(pal) = particles {
                        let _ = level.send_particles(
                            host,
                            &ParticleOptions::simple(pal.smoke),
                            None,
                            anchor,
                            [1.2, 1.0, 1.2],
                            0.04,
                            24,
                        );
                    }
                } else if hd2 > 28.0 * 28.0 || dy > 28.0 || dy < -4.0 {
                    let vdy = w_pos[1] - anchor[1];
                    let dist = (hd2 + vdy * vdy).sqrt().max(1.0);
                    let _ = w_ent.set_velocity(
                        host,
                        [
                            (-dx / dist) * 0.48,
                            (-vdy / dist) * 0.36,
                            (-dz / dist) * 0.48,
                        ],
                    );
                    let _ = mob.move_to(host, anchor, 1.35);
                }
            }
        }
    }

    for (hunter_ent, _, _) in &hunters {
        if state.citadel_guards.contains(&hunter_ent.id())
            && let Some(mob) = host.mob(*hunter_ent)
            && let Some(tgt) = mob.target(host)
        {
            let tgt_id = tgt.entity().id();
            let tgt_wither = tgt
                .entity()
                .kind(host)
                .is_some_and(|k| strip_ns(&k.id(host)) == "wither");
            if tgt_wither || state.citadel_guards.contains(&tgt_id) {
                let _ = mob.set_target(host, None);
                let _ = mob.set_brain_target(host, None);
            }
        }
    }

    if hunters.is_empty() {
        return;
    }

    let hunt_range_sq = 32.0 * 32.0;

    if !prey.is_empty() {
        for &(hunter_ent, ref hunter_short, h_pos) in &hunters {
            let Some(mob) = host.mob(hunter_ent) else {
                continue;
            };

            // Ensure Magma Cubes have an active melee goal and actually strike adjacent
            // Overworld animals rather than passively bouncing beside cows/pigs/sheep.
            if hunter_short == "magma_cube" {
                if state.magma_melee_ready.insert(hunter_ent.id())
                    && let Some(goals) = mob.goals(host)
                    && !goals.iter().any(|g| g.id == "melee_attack")
                {
                    let _ = mob.add_goal(
                        host,
                        2,
                        &GoalSpec::MeleeAttack {
                            speed: 1.3,
                            follow_unseen: true,
                        },
                    );
                }
                if let Some((close_prey, p_pos, _)) = prey
                    .iter()
                    .filter_map(|(pe, pp)| {
                        let dx = h_pos[0] - pp[0];
                        let dy = h_pos[1] - pp[1];
                        let dz = h_pos[2] - pp[2];
                        let d2 = dx * dx + dy * dy + dz * dz;
                        (d2 <= 2.75 * 2.75).then_some((*pe, *pp, d2))
                    })
                    .min_by(|a, b| a.2.total_cmp(&b.2))
                {
                    let _ = close_prey.hurt(
                        host,
                        "minecraft:mob_attack",
                        4.5,
                        Some(hunter_ent),
                        Some(hunter_ent),
                    );
                    let _ = mob.move_to(host, p_pos, 1.3);
                }
            }

            // Skip hunters that already have an active target so we don't re-trigger
            // aggro / angry sounds repeatedly.
            if mob.target(host).is_some() {
                if hunter_short == "magma_cube"
                    && let Some(tgt) = mob.target(host)
                    && let Some(tp) = tgt.entity().position(host)
                {
                    let _ = mob.move_to(host, tp, 1.3);
                }
                continue;
            }
            let mut best: Option<(EntityRef, [f64; 3], f64)> = None;
            for (prey_ent, p_pos) in &prey {
                let dx = h_pos[0] - p_pos[0];
                let dy = h_pos[1] - p_pos[1];
                let dz = h_pos[2] - p_pos[2];
                let d2 = dx * dx + dy * dy + dz * dz;
                if d2 <= hunt_range_sq && best.is_none_or(|(_, _, b)| d2 < b) {
                    best = Some((*prey_ent, *p_pos, d2));
                }
            }
            if let Some((victim_ent, v_pos, _)) = best
                && let Some(target_living) = host.living(victim_ent)
            {
                let _ = mob.set_target(host, Some(target_living));
                let _ = mob.set_brain_target(host, Some(target_living));
                if hunter_short == "magma_cube" {
                    let _ = mob.move_to(host, v_pos, 1.3);
                }
                if mob.is_aggressive(host) != Some(true) {
                    let _ = mob.set_aggressive(host, true);
                }
            }
        }
    }

    // Overworld mobs (zombies, skeletons, spiders, illagers, golems) immediately fight back
    // and target nearby Nether enemies on sight.
    for &(def_ent, d_pos) in &defenders {
        let Some(mob) = host.mob(def_ent) else {
            continue;
        };
        if mob.target(host).is_some() {
            continue;
        }
        let mut best_nether: Option<(EntityRef, f64)> = None;
        for &(hunter_ent, _, h_pos) in &hunters {
            let dx = d_pos[0] - h_pos[0];
            let dy = d_pos[1] - h_pos[1];
            let dz = d_pos[2] - h_pos[2];
            let d2 = dx * dx + dy * dy + dz * dz;
            if d2 <= hunt_range_sq && best_nether.is_none_or(|(_, b)| d2 < b) {
                best_nether = Some((hunter_ent, d2));
            }
        }
        if let Some((target_ent, _)) = best_nether
            && let Some(target_living) = host.living(target_ent)
        {
            let _ = mob.set_target(host, Some(target_living));
            let _ = mob.set_brain_target(host, Some(target_living));
            if mob.is_aggressive(host) != Some(true) {
                let _ = mob.set_aggressive(host, true);
            }
        }
    }
}

/// Stops the Citadel Wither Sovereign and allied Nether guards from targeting
/// or infighting with one another.
pub fn handle_living_change_target(host: &dyn Host, event: &mut LivingChangeTargetEvent<'_>) {
    let Some(target) = *event.new_target else {
        return;
    };
    let Some(mob_kind) = event.mob.entity().kind(host).map(|k| k.id(host)) else {
        return;
    };
    let Some(tgt_kind) = target.entity().kind(host).map(|k| k.id(host)) else {
        return;
    };

    let mob_short = strip_ns(&mob_kind);
    let tgt_short = strip_ns(&tgt_kind);
    let mob_nether = is_nether_predator(&mob_kind) || mob_short == "strider";
    let tgt_nether = is_nether_predator(&tgt_kind) || tgt_short == "strider";

    if (mob_short == "wither" && tgt_nether) || (mob_nether && tgt_short == "wither") {
        *event.new_target = None;
        return;
    }

    if mob_nether && tgt_nether {
        let mob_ally = event
            .mob
            .entity()
            .tags(host)
            .is_some_and(|ts| ts.iter().any(|t| t == "rift_bleed:nether_ally"));
        let tgt_ally = target
            .entity()
            .tags(host)
            .is_some_and(|ts| ts.iter().any(|t| t == "rift_bleed:nether_ally"));
        if mob_ally && tgt_ally {
            *event.new_target = None;
        }
    }
}

pub fn should_cancel_damage_pair(
    state: &RiftState,
    victim_id: Option<u32>,
    victim_kind: &str,
    damage_type: &str,
    causing: Option<u32>,
    direct: Option<u32>,
) -> bool {
    let victim_short = strip_ns(victim_kind);
    let victim_is_nether = is_nether_predator(victim_kind) || victim_short == "strider";
    let victim_is_wither = victim_short == "wither";

    if victim_is_nether {
        if matches!(strip_ns(damage_type), "wither" | "wither_skull") {
            return true;
        }
        if causing.is_some_and(|id| state.known_withers.contains(&id))
            || direct.is_some_and(|id| state.known_withers.contains(&id))
        {
            return true;
        }
        if victim_id.is_some_and(|vid| state.citadel_guards.contains(&vid))
            && (causing.is_some_and(|cid| state.citadel_guards.contains(&cid))
                || direct.is_some_and(|did| state.citadel_guards.contains(&did)))
        {
            return true;
        }
    }

    if victim_is_wither
        && (causing
            .is_some_and(|id| state.known_nether_mobs.contains(&id) || state.citadel_guards.contains(&id))
            || direct.is_some_and(|id| {
                state.known_nether_mobs.contains(&id) || state.citadel_guards.contains(&id)
            }))
    {
        return true;
    }

    false
}

/// Cancels all friendly-fire damage (including Wither skull splash, wither status
/// decay, and accidental swings) between any Wither boss and Nether mobs/allied guards.
pub fn should_cancel_incoming_damage(
    state: &RiftState,
    event: &LivingIncomingDamageEvent<'_>,
) -> bool {
    should_cancel_damage_pair(
        state,
        u32::try_from(event.entity).ok(),
        event.kind,
        event.source.damage_type,
        u32::try_from(event.source.causing).ok(),
        u32::try_from(event.source.direct).ok(),
    )
}

fn resolve_mob_block_cached(
    cache: &mut HashMap<&'static str, BlockState>,
    host: &dyn ServerHost,
    id: &'static str,
) -> Option<BlockState> {
    if let Some(&bs) = cache.get(id) {
        return Some(bs);
    }
    let bs = host
        .block_state_id(id)
        .or_else(|| host.block_key(id).and_then(|k| host.block_state_default(k)))?;
    cache.insert(id, bs);
    Some(bs)
}

#[derive(Clone, Debug)]
pub struct PendingDeath {
    pub kind: String,
    pub pos: [f64; 3],
    pub attacker_id: Option<u32>,
    pub attacker_is_player: bool,
    pub attacker_is_nether_kind: bool,
}

/// Captures a mob death before deferring it across a lock-contended tick (for example when
/// `magma_cube.hurt()` inside `tick_mobs` triggers `living_death` re-entrantly).
pub fn extract_living_death(
    host: &dyn ServerHost,
    event: &LivingDeathEvent<'_>,
) -> Option<PendingDeath> {
    if strip_ns(event.kind) == "player" {
        return None;
    }
    let ow = host.level(OVERWORLD)?;
    if host
        .entity(event.entity)
        .and_then(|e| e.level(host))
        .is_some_and(|l| l != ow)
    {
        return None;
    }
    let pos = host
        .entity(event.entity)
        .and_then(|e| e.position(host))
        .or(event.source.position)?;
    let attacker_id = u32::try_from(event.source.causing)
        .ok()
        .filter(|&v| v > 0)
        .or_else(|| u32::try_from(event.source.direct).ok().filter(|&v| v > 0));
    let attacker_kind = attacker_id.and_then(|aid| host.entity(aid).and_then(|e| e.kind(host)));
    let attacker_is_player = attacker_kind
        .as_ref()
        .is_some_and(|k| strip_ns(&k.id(host)) == "player");
    let attacker_is_nether_kind = attacker_kind.as_ref().is_some_and(|k| {
        let kid = k.id(host);
        is_nether_predator(&kid) || strip_ns(&kid) == "wither"
    });
    Some(PendingDeath {
        kind: event.kind.to_owned(),
        pos,
        attacker_id,
        attacker_is_player,
        attacker_is_nether_kind,
    })
}

/// Scorches an organic ~3-block patch of uncorrupted land beneath a mob death.
fn corrupt_kill_patch(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    bp: [i32; 3],
    particles: Option<&ParticlePalette>,
) {
    if !level.is_loaded(host, bp) {
        return;
    }
    // Never stain a peaceful/healing zone where a Sovereign Wither has been defeated.
    if state.rifts.iter().any(|r| {
        (r.pacified || r.regressing) && horiz_dist_sq(r.center, bp) <= r.max_radius_sq.max(48 * 48)
    }) {
        return;
    }

    let nearest = state
        .rifts
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.pacified)
        .min_by_key(|(_, r)| horiz_dist_sq(r.center, bp));
    let nearest_idx = nearest.map(|(i, _)| i);
    let flavor = nearest
        .map(|(_, r)| r.flavor)
        .unwrap_or_else(|| NetherFlavor::pick_for_region(bp[0], bp[2], state.seed));
    let mature = nearest.is_some_and(|(_, r)| r.corrupted_blocks >= 35_000 || r.age_ticks >= 5_000);

    let min_y = (bp[1] - 4).max(OVERWORLD_MIN_Y + 1);
    let max_y = (bp[1] + 2).min(OVERWORLD_MAX_Y - 1);
    let mut converted_here = 0u64;

    for dx in -3..=3 {
        for dz in -3..=3 {
            let px = bp[0] + dx;
            let pz = bp[2] + dz;
            let h = spatial_hash(px, bp[1], pz, state.seed ^ 0xB100_D111);
            if dx * dx + dz * dz > 9 + (h % 4) as i32 {
                continue;
            }
            if !level.is_loaded(host, [px, bp[1].clamp(OVERWORLD_MIN_Y, OVERWORLD_MAX_Y), pz]) {
                continue;
            }
            for y in (min_y..=max_y).rev() {
                let pos = [px, y, pz];
                let current = level.block(host, pos).map(|k| k.id(host)).unwrap_or_default();
                if current.is_empty() || is_air_like(&current) || is_water_like(&current) {
                    continue;
                }
                let above = if y + 1 <= OVERWORLD_MAX_Y {
                    level
                        .block(host, [px, y + 1, pz])
                        .map(|k| k.id(host))
                        .unwrap_or_else(|| "minecraft:air".to_owned())
                } else {
                    "minecraft:air".to_owned()
                };
                let below = if y - 1 >= OVERWORLD_MIN_Y {
                    level
                        .block(host, [px, y - 1, pz])
                        .map(|k| k.id(host))
                        .unwrap_or_else(|| "minecraft:bedrock".to_owned())
                } else {
                    "minecraft:bedrock".to_owned()
                };

                let out = transform_block(pos, &current, &above, &below, flavor, mature);
                if let Some(target_id) = out.block
                    && let Some(bs) = resolve_mob_block_cached(&mut state.block_state_cache, host, target_id)
                {
                    let flags = if target_id == "minecraft:air" || out.above.is_none() {
                        BlockUpdateFlags::CLIENTS
                            | BlockUpdateFlags::KNOWN_SHAPE
                            | BlockUpdateFlags::SUPPRESS_DROPS
                    } else {
                        BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS
                    };
                    if level.set_block(host, pos, bs, flags).unwrap_or(false) {
                        converted_here += 1;
                        if let Some(above_id) = out.above
                            && y + 1 < OVERWORLD_MAX_Y
                            && is_air_like(&above)
                            && let Some(above_bs) =
                                resolve_mob_block_cached(&mut state.block_state_cache, host, above_id)
                        {
                            let _ = level.set_block(
                                host,
                                [px, y + 1, pz],
                                above_bs,
                                BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                            );
                        }
                    }
                }
                if out.is_corrupt && state.mark_quart_corrupted(pos) {
                    paint_biome_column(level, host, px, pz, flavor);
                }
            }
        }
    }

    if converted_here > 0 {
        if let Some(r_idx) = nearest_idx {
            state.rifts[r_idx].corrupted_blocks += converted_here;
        }
        state.dirty = true;
        if let Some(pal) = particles {
            let _ = level.send_particles(
                host,
                &ParticleOptions::simple(pal.crimson_spore),
                None,
                [f64::from(bp[0]) + 0.5, f64::from(bp[1]) + 0.25, f64::from(bp[2]) + 0.5],
                [1.4, 0.35, 1.4],
                0.025,
                20,
            );
        }
    }
}

fn is_near_active_nether_front(
    host: &dyn ServerHost,
    state: &RiftState,
    bp: [i32; 3],
) -> bool {
    let in_healing_zone = state.rifts.iter().any(|r| {
        (r.pacified || r.regressing) && horiz_dist_sq(r.center, bp) <= r.max_radius_sq.max(48 * 48)
    });
    if in_healing_zone {
        return false;
    }
    if state.is_quart_corrupted(bp) {
        return true;
    }
    if state.rifts.iter().any(|r| {
        r.is_actively_spreading()
            && horiz_dist_sq(r.center, bp) <= i64::from(r.ring_radius.max(8) + 96).pow(2)
    }) {
        return true;
    }
    state.known_nether_mobs.iter().any(|&nid| {
        host.entity(nid)
            .and_then(|e| e.position(host))
            .is_some_and(|np| {
                horiz_dist_sq(
                    [np[0].floor() as i32, np[1].floor() as i32, np[2].floor() as i32],
                    bp,
                ) <= 32 * 32
            })
    })
}

/// Applies kill-patch corruption and Nether death conversion for a captured Overworld death event
/// (used both immediately by `handle_living_death` and when drained from `pending_deaths`).
pub fn apply_living_death(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    death: &PendingDeath,
    particles: Option<&ParticlePalette>,
) {
    let pos = death.pos;
    let bp = [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ];

    let biome_flavor = local_corrupt_flavor(host, level, bp);
    let victim_is_nether = is_nether_predator(&death.kind)
        || matches!(strip_ns(&death.kind), "strider" | "wither")
        || (strip_ns(&death.kind) == "enderman"
            && matches!(
                biome_flavor,
                Some(NetherFlavor::WarpedForest | NetherFlavor::SoulSandValley)
            ));
    let attacker_is_nether = death.attacker_is_nether_kind
        || death.attacker_id.is_some_and(|aid| {
            state.known_nether_mobs.contains(&aid) || state.nether_touched_mobs.contains(&aid)
        });
    let near_active_front = if !victim_is_nether && !attacker_is_nether {
        is_near_active_nether_front(host, state, bp)
    } else {
        false
    };

    if should_corrupt_kill_patch(
        attacker_is_nether,
        victim_is_nether,
        death.attacker_id.is_some(),
        death.attacker_is_player,
    ) || should_corrupt_non_nether_death(victim_is_nether, attacker_is_nether, near_active_front)
    {
        corrupt_kill_patch(host, level, state, bp, particles);
    }

    let Some(target_kind) = nether_death_conversion_target(&death.kind) else {
        return;
    };

    if !attacker_is_nether && !location_unlocks_nether_mobs(state, bp) {
        return;
    }

    let mut nbt = Compound::new();
    if is_piglin_or_hoglin(target_kind) {
        nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
        nbt.insert("TimeInOverworld".into(), Tag::Int(0));
    }

    if let Some(new_id) = host.summon(OVERWORLD, target_kind, pos, nbt) {
        state.known_nether_mobs.insert(new_id);
        if is_piglin_or_hoglin(target_kind) {
            state.zombification_immune_mobs.insert(new_id);
        }
        if let Some(new_ent) = host.entity(new_id) {
            apply_mob_effects(host, new_ent, 20 * 180, 20 * 120);
        }
    }

    if let Some(pal) = particles {
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(pal.flame),
            None,
            [pos[0], pos[1] + 0.8, pos[2]],
            [0.45, 0.5, 0.45],
            0.04,
            18,
        );
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(pal.crimson_spore),
            None,
            [pos[0], pos[1] + 0.8, pos[2]],
            [0.5, 0.4, 0.5],
            0.03,
            12,
        );
    }
    let _ = level.play_sound(
        host,
        "minecraft:block.fire.extinguish",
        pos,
        SoundSource::Hostile,
        0.3,
        0.85,
    );
}

/// Converts any Overworld creature killed by a Nether mob (or slain inside an already-large
/// corrupted Nether biome) into its Nether version on death, and corrupts uncorrupted terrain
/// underneath dying Overworld mobs near active fronts or killed in Nether combat.
pub fn handle_living_death(
    host: &mut dyn ServerHost,
    state: &mut RiftState,
    event: &LivingDeathEvent<'_>,
    particles: Option<&ParticlePalette>,
) {
    let Some(level) = host.level(OVERWORLD) else {
        return;
    };
    let Some(death) = extract_living_death(host, event) else {
        return;
    };
    apply_living_death(host, level, state, &death, particles);
}

/// Hoglins act as border vanguard: near the edge of corrupted land they root
/// and trample uncorrupted Overworld grass/dirt into crimson nylium/netherrack
/// and enqueue those columns cleanly.
fn process_hoglin_edge_spreading(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    entities: &[(EntityRef, u32, String, [f64; 3])],
    particles: Option<&ParticlePalette>,
) {
    if state.paused {
        return;
    }
    let Some(nylium) = host.block_state_id("minecraft:crimson_nylium") else {
        return;
    };
    let Some(netherrack) = host.block_state_id("minecraft:netherrack") else {
        return;
    };
    let air = host.block_state_id("minecraft:air");
    let roots = host.block_state_id("minecraft:crimson_roots");

    let mut total_trampled = 0usize;
    let mut trampled_any = false;

    for (_, id, kind, pos) in entities {
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
        let active_nearby = state
            .rifts
            .iter()
            .any(|r| r.is_actively_spreading() && horiz_dist_sq(r.center, base) <= r.max_radius_sq + 512);
        if !active_nearby {
            continue;
        }
        let near_corrupt = pos_is_corrupted(host, level, state, base)
            || state
                .rifts
                .iter()
                .any(|r| r.is_actively_spreading() && horiz_dist_sq(r.center, base) <= r.max_radius_sq + 256);
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
                    let Some(b_key) = level.block(host, p) else {
                        continue;
                    };
                    let b = b_key.id(host);
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
                    let above_pos = [p[0], p[1] + 1, p[2]];
                    let above = level
                        .block(host, above_pos)
                        .map(|k| k.id(host))
                        .unwrap_or_default();
                    let cleared_above = if !is_air_like(&above) && is_open_space(&above) {
                        if let Some(air_state) = air {
                            let _ = level.set_block(
                                host,
                                above_pos,
                                air_state,
                                BlockUpdateFlags::CLIENTS
                                    | BlockUpdateFlags::KNOWN_SHAPE
                                    | BlockUpdateFlags::SUPPRESS_DROPS,
                            );
                            true
                        } else {
                            false
                        }
                    } else {
                        is_air_like(&above)
                    };
                    let use_nylium = cleared_above
                        && spatial_hash(p[0], p[1], p[2], state.tick ^ u64::from(*id)) % 3 != 0;
                    let chosen = if use_nylium { nylium } else { netherrack };
                    if level
                        .set_block(
                            host,
                            p,
                            chosen,
                            BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                        )
                        .unwrap_or(false)
                    {
                        total_trampled += 1;
                        trampled_here = true;
                        trampled_any = true;
                        if use_nylium
                            && let Some(r_state) = roots
                            && cleared_above
                            && spatial_hash(p[0], p[1] + 1, p[2], 0x4061) % 4 == 0
                        {
                            let _ = level.set_block(
                                host,
                                above_pos,
                                r_state,
                                BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                            );
                        }
                        state.pending_frontier_columns.push_back([p[0], p[2]]);
                        if state.mark_quart_corrupted(p) {
                            paint_biome_column(level, host, p[0], p[2], NetherFlavor::CrimsonForest);
                        }
                        break;
                    }
                }
            }
        }

        if trampled_here && let Some(pal) = particles {
            let _ = level.send_particles(
                host,
                &ParticleOptions::simple(pal.crimson_spore),
                None,
                [pos[0], pos[1] + 0.3, pos[2]],
                [0.8, 0.3, 0.8],
                0.02,
                8,
            );
        }
    }

    if trampled_any {
        state.dirty = true;
    }
}

fn pick_mob_for_flavor(flavor: NetherFlavor, roll: u64, rift_blocks: u64) -> &'static str {
    match (flavor, roll % 10) {
        (NetherFlavor::CrimsonForest, 0..=4) => "minecraft:hoglin",
        (NetherFlavor::CrimsonForest, 5..=8) => "minecraft:piglin",
        (NetherFlavor::CrimsonForest, _) => "minecraft:zombified_piglin",
        (NetherFlavor::WarpedForest, 0..=4) => "minecraft:enderman",
        (NetherFlavor::WarpedForest, 5..=7) => "minecraft:piglin",
        (NetherFlavor::WarpedForest, _) if rift_blocks >= 85_000 => "minecraft:blaze",
        (NetherFlavor::WarpedForest, _) => "minecraft:enderman",
        (NetherFlavor::SoulSandValley, 0..=4) => "minecraft:wither_skeleton",
        (NetherFlavor::SoulSandValley, 5..=6) => "minecraft:enderman",
        (NetherFlavor::SoulSandValley, 7) => "minecraft:blaze",
        (NetherFlavor::SoulSandValley, _) if rift_blocks >= 85_000 => "minecraft:ghast",
        (NetherFlavor::SoulSandValley, _) => "minecraft:wither_skeleton",
        (NetherFlavor::BasaltDeltas, 0..=6) => "minecraft:magma_cube",
        (NetherFlavor::BasaltDeltas, 7..=8) if rift_blocks >= 85_000 => "minecraft:ghast",
        (NetherFlavor::BasaltDeltas, _) => "minecraft:blaze",
        (NetherFlavor::NetherWastes, 0..=4) => "minecraft:piglin",
        (NetherFlavor::NetherWastes, 5..=6) => "minecraft:zombified_piglin",
        (NetherFlavor::NetherWastes, 7) => "minecraft:magma_cube",
        (NetherFlavor::NetherWastes, 8) if rift_blocks >= 85_000 => "minecraft:blaze",
        (NetherFlavor::NetherWastes, _) if rift_blocks >= 95_000 => "minecraft:ghast",
        _ => "minecraft:piglin",
    }
}

/// Depth-scaled chance (`%`) of an extra Nether creature emerging in subterranean caves:
/// still active underground so dark caverns feel Nether-haunted, while gradually thinning out
/// the deeper you go toward bedrock compared with surface hordes.
pub fn underground_spawn_chance_pct(player_y: i32, surface_y: i32) -> u64 {
    if player_y >= surface_y - 10 {
        100
    } else if player_y >= 16 {
        65
    } else if player_y >= -16 {
        45
    } else {
        30
    }
}

fn pick_underground_mob_for_flavor(flavor: NetherFlavor, roll: u64, headroom: i32) -> &'static str {
    match (flavor, roll % 10) {
        (NetherFlavor::CrimsonForest, 0..=3) if headroom >= 3 => "minecraft:hoglin",
        (NetherFlavor::CrimsonForest, 0..=6) => "minecraft:piglin",
        (NetherFlavor::CrimsonForest, 7..=8) => "minecraft:zombified_piglin",
        (NetherFlavor::CrimsonForest, _) => "minecraft:magma_cube",
        (NetherFlavor::WarpedForest, 0..=4) => "minecraft:enderman",
        (NetherFlavor::WarpedForest, 5..=7) => "minecraft:piglin",
        (NetherFlavor::WarpedForest, 8) => "minecraft:blaze",
        (NetherFlavor::WarpedForest, _) => "minecraft:zombified_piglin",
        (NetherFlavor::SoulSandValley, 0..=5) => "minecraft:wither_skeleton",
        (NetherFlavor::SoulSandValley, 6..=7) => "minecraft:enderman",
        (NetherFlavor::SoulSandValley, 8) => "minecraft:blaze",
        (NetherFlavor::SoulSandValley, _) => "minecraft:magma_cube",
        (NetherFlavor::BasaltDeltas, 0..=6) => "minecraft:magma_cube",
        (NetherFlavor::BasaltDeltas, 7..=8) => "minecraft:blaze",
        (NetherFlavor::BasaltDeltas, _) => "minecraft:zombified_piglin",
        (NetherFlavor::NetherWastes, 0..=3) => "minecraft:piglin",
        (NetherFlavor::NetherWastes, 4..=6) => "minecraft:zombified_piglin",
        (NetherFlavor::NetherWastes, 7) => "minecraft:magma_cube",
        (NetherFlavor::NetherWastes, 8) => "minecraft:wither_skeleton",
        (NetherFlavor::NetherWastes, _) => "minecraft:blaze",
    }
}

pub fn tune_underground_spawn_entries(entries: &mut Vec<SpawnEntry>, y: i32) {
    if y >= 48 {
        return;
    }
    // Strip giant ghasts AND any normal Overworld mobs in subterranean Nether biomes,
    // while keeping deep Nether mob weights sparser near bedrock than upper caves/surface.
    entries.retain(|e| {
        strip_ns(&e.entity) != "ghast" && is_allowed_in_nether_biome(&e.entity, None)
    });
    if y < 0 {
        for entry in entries.iter_mut() {
            entry.weight = ((entry.weight * 6) / 10).max(2);
        }
    }
}

fn find_underground_spawn_spot(
    host: &dyn ServerHost,
    level: LevelRef,
    sx: i32,
    sz: i32,
    center_y: i32,
    surface_y: i32,
) -> Option<(i32, i32)> {
    let min_y = (center_y - 8).max(OVERWORLD_MIN_Y + 2);
    let max_y = (center_y + 8).min(surface_y - 4);
    if min_y > max_y {
        return None;
    }
    for y in min_y..=max_y {
        let below = [sx, y - 1, sz];
        let head1 = [sx, y, sz];
        let head2 = [sx, y + 1, sz];
        if !level.is_loaded(host, head1) {
            return None;
        }
        let Some(bk) = level.block(host, below) else {
            continue;
        };
        let b_id = bk.id(host);
        let b_short = strip_ns(&b_id);
        if is_air_like(&b_id)
            || crate::corruption::is_water_like(&b_id)
            || matches!(b_short, "lava" | "flowing_lava" | "magma_block" | "fire" | "soul_fire")
        {
            continue;
        }
        let a1 = level
            .block(host, head1)
            .map(|k| is_air_like(&k.id(host)))
            .unwrap_or(false);
        let a2 = level
            .block(host, head2)
            .map(|k| is_air_like(&k.id(host)))
            .unwrap_or(false);
        if a1 && a2 {
            let headroom = if level
                .block(host, [sx, y + 2, sz])
                .is_some_and(|k| is_air_like(&k.id(host)))
            {
                3
            } else {
                2
            };
            return Some((y, headroom));
        }
    }
    None
}

fn process_supplemental_spawns(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    entities: &[(EntityRef, u32, String, [f64; 3])],
) {
    if level.difficulty(host) == Some("peaceful") {
        return;
    }
    // Wait until at least one active rift (or connected wasteland) forms a pretty large Nether biome.
    if !state
        .rifts
        .iter()
        .enumerate()
        .any(|(idx, r)| r.is_actively_spreading() && rift_mobs_unlocked_in_state(state, idx))
    {
        return;
    }

    // Count only outdoor/roaming Nether mobs (exclude Outpost/Citadel garrison guards so
    // having structures loaded never starves the surrounding corrupted biome!).
    const MAX_GLOBAL_ROAMING_NETHER: usize = 96;
    const MAX_LOCAL_ROAMING_NETHER: usize = 28;

    let mut roaming_positions: Vec<[i32; 3]> = Vec::new();
    for (_, id, k, pos) in entities {
        if (is_nether_predator(k) || strip_ns(k) == "enderman")
            && !state.citadel_guards.contains(id)
        {
            roaming_positions.push([
                pos[0].floor() as i32,
                pos[1].floor() as i32,
                pos[2].floor() as i32,
            ]);
        }
    }
    if roaming_positions.len() >= MAX_GLOBAL_ROAMING_NETHER {
        return;
    }

    let mut candidate_anchors: Vec<(u64, NetherFlavor, u64, [i32; 3])> = Vec::new();

    for player in host.players().server_players() {
        let Some(dim) = player.dimension(host) else {
            continue;
        };
        if dim.to_string() != OVERWORLD {
            continue;
        }
        let Some(pos) = player.entity().position(host) else {
            continue;
        };
        let pp = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];
        if let Some((idx, rift)) = state
            .rifts
            .iter()
            .enumerate()
            .filter(|(idx, r)| {
                r.is_actively_spreading()
                    && rift_mobs_unlocked_in_state(state, *idx)
                    && (state.is_quart_corrupted(pp)
                        || horiz_dist_sq(r.center, pp)
                            <= (r.max_radius_sq + 96 * 96).max(96 * 96))
            })
            .min_by_key(|(_, r)| dist_sq(r.center, pp))
        {
            let w_blocks = state.connected_wasteland_for(idx).total_corrupted_blocks;
            candidate_anchors.push((rift.id, rift.flavor, w_blocks, pp));
        }
    }

    if candidate_anchors.is_empty() {
        for (idx, rift) in state.rifts.iter().enumerate() {
            if r_is_spawn_ready(state, idx, rift) {
                let w_blocks = state.connected_wasteland_for(idx).total_corrupted_blocks;
                candidate_anchors.push((rift.id, rift.flavor, w_blocks, rift.center));
            }
        }
    }
    if candidate_anchors.is_empty() {
        return;
    }

    let cycle = (state.tick / SUPPLEMENTAL_SPAWN_INTERVAL_TICKS) as usize;
    let chosen = candidate_anchors[cycle % candidate_anchors.len()];
    let (rift_id, fallback_flavor, corrupted_blocks, anchor) = chosen;

    let local_roaming = roaming_positions
        .iter()
        .filter(|rp| horiz_dist_sq(**rp, anchor) <= 64 * 64)
        .count();
    if local_roaming >= MAX_LOCAL_ROAMING_NETHER {
        return;
    }

    let mut probes: Vec<(i32, i32)> = Vec::with_capacity(9);
    let loaded_y = anchor[1].clamp(OVERWORLD_MIN_Y + 2, OVERWORLD_MAX_Y - 4);
    for i in 0..8usize {
        let h = spatial_hash(
            anchor[0],
            anchor[1],
            anchor[2],
            state.tick ^ rift_id ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let dist = 14 + (h % 33) as i32; // 14..=46 blocks
        let angle = ((h >> 12) % 360) as f64 * std::f64::consts::TAU / 360.0;
        let sx = anchor[0] + (f64::from(dist) * angle.cos()).round() as i32;
        let sz = anchor[2] + (f64::from(dist) * angle.sin()).round() as i32;
        if level.is_loaded(host, [sx, loaded_y, sz]) {
            probes.push((sx, sz));
        }
    }
    if probes.is_empty() {
        return;
    }
    let probe_count = probes.len();
    probes.push((anchor[0], anchor[2]));

    let Ok(heights) =
        host.surface_heights(OVERWORLD, &probes, Heightmap::MotionBlockingNoLeaves)
    else {
        return;
    };
    let anchor_sy = heights
        .get(probe_count)
        .copied()
        .flatten()
        .unwrap_or(anchor[1] + 2);

    let remaining_budget = (MAX_GLOBAL_ROAMING_NETHER - roaming_positions.len())
        .min(MAX_LOCAL_ROAMING_NETHER - local_roaming);
    if remaining_budget == 0 {
        return;
    }

    let is_underground = anchor[1] < anchor_sy - 10;
    if is_underground {
        let roll = spatial_hash(anchor[0], anchor[1], anchor[2], state.tick ^ rift_id);
        let chance = underground_spawn_chance_pct(anchor[1], anchor_sy);
        if (roll % 100) >= chance {
            return;
        }
    }

    for (idx, ((sx, sz), opt_sy)) in probes
        .into_iter()
        .take(probe_count)
        .zip(heights.into_iter())
        .enumerate()
    {
        let Some(sy) = opt_sy else {
            continue;
        };
        let h = spatial_hash(
            sx,
            sy,
            sz,
            state.tick ^ rift_id ^ (idx as u64).wrapping_mul(0x517C_C1B7_2722_0A95),
        );

        let (base_y, mob_kind, pack_size) = if is_underground {
            let Some((cave_y, headroom)) =
                find_underground_spawn_spot(host, level, sx, sz, anchor[1], sy)
            else {
                continue;
            };
            let g = [sx, cave_y - 1, sz];
            if !location_unlocks_nether_mobs(state, g) {
                continue;
            }
            let local_flavor = local_corrupt_flavor(host, level, g).unwrap_or(fallback_flavor);
            let kind = pick_underground_mob_for_flavor(local_flavor, h >> 16, headroom);
            let count = (1 + ((h >> 24) % 2) as usize).min(remaining_budget);
            (cave_y, kind, count)
        } else {
            if sy <= OVERWORLD_MIN_Y + 4 || sy + 4 >= OVERWORLD_MAX_Y {
                continue;
            }
            let floor_y = resolve_surface_floor_y(host, level, sx, sy, sz);
            let Some(floor_y) = floor_y else {
                continue;
            };
            let g = [sx, floor_y - 1, sz];
            if !location_unlocks_nether_mobs(state, g) {
                continue;
            }
            let local_flavor = local_corrupt_flavor(host, level, g).unwrap_or(fallback_flavor);
            let kind = pick_mob_for_flavor(local_flavor, h >> 16, corrupted_blocks);
            if kind == "minecraft:ghast" {
                let air_ok = level
                    .block(host, [sx, sy + 12, sz])
                    .is_some_and(|k| is_air_like(&k.id(host)))
                    && level
                        .block(host, [sx, sy + 15, sz])
                        .is_some_and(|k| is_air_like(&k.id(host)));
                if !air_ok {
                    continue;
                }
                (sy + 14, kind, 1)
            } else {
                let count = (2 + ((h >> 24) % 2) as usize).min(remaining_budget);
                (floor_y, kind, count)
            }
        };

        let mut spawned_any = false;
        for member in 0..pack_size {
            let mh = spatial_hash(sx, base_y, sz, (member as u64) ^ h);
            let (ox, oz) = if member == 0 || mob_kind == "minecraft:ghast" {
                (0, 0)
            } else {
                let dx = ((mh & 3) as i32) - 1;
                let dz = (((mh >> 4) & 3) as i32) - 1;
                let adj_ground = level
                    .block(host, [sx + dx, base_y - 1, sz + dz])
                    .is_some_and(|k| is_standable_nether_ground(&k.id(host)));
                let adj_clear = level
                    .block(host, [sx + dx, base_y, sz + dz])
                    .is_some_and(|k| is_nether_open_headroom(&k.id(host)))
                    && level
                        .block(host, [sx + dx, base_y + 1, sz + dz])
                        .is_some_and(|k| is_nether_open_headroom(&k.id(host)));
                if adj_ground && adj_clear {
                    (dx, dz)
                } else {
                    (0, 0)
                }
            };
            let spawn_pos = [
                f64::from(sx + ox) + 0.5,
                f64::from(base_y),
                f64::from(sz + oz) + 0.5,
            ];
            let mut nbt = Compound::new();
            if is_piglin_or_hoglin(mob_kind) {
                nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
                nbt.insert("TimeInOverworld".into(), Tag::Int(0));
            }
            if let Some(id) = host.summon(OVERWORLD, mob_kind, spawn_pos, nbt) {
                state.known_nether_mobs.insert(id);
                spawned_any = true;
            }
        }
        if spawned_any {
            return;
        }
    }
}

fn r_is_spawn_ready(state: &RiftState, idx: usize, rift: &crate::types::RiftSource) -> bool {
    rift.is_actively_spreading() && rift_mobs_unlocked_in_state(state, idx)
}

/// Resolves a standable terrain surface `y` at `(sx, sz)` by stepping down past
/// giant-fungus wart canopies / shroomlight so forest floors stay populated.
fn resolve_surface_floor_y(
    host: &dyn ServerHost,
    level: LevelRef,
    sx: i32,
    top_sy: i32,
    sz: i32,
) -> Option<i32> {
    let min_y = (top_sy - 12).max(OVERWORLD_MIN_Y + 2);
    for y in (min_y..=top_sy).rev() {
        let Some(below) = level.block(host, [sx, y - 1, sz]) else {
            continue;
        };
        let b_id = below.id(host);
        if !is_standable_nether_ground(&b_id) {
            continue;
        }
        let open = level
            .block(host, [sx, y, sz])
            .is_some_and(|k| is_nether_open_headroom(&k.id(host)))
            && level
                .block(host, [sx, y + 1, sz])
                .is_some_and(|k| is_nether_open_headroom(&k.id(host)));
        if open {
            return Some(y);
        }
    }
    None
}

fn active_strider_anchor(state: &RiftState, host: &mut dyn ServerHost) -> Option<[i32; 3]> {
    let mut player_anchors = Vec::new();
    for player in host.players().server_players() {
        let Some(dim) = player.dimension(host) else {
            continue;
        };
        if dim.to_string() != OVERWORLD {
            continue;
        }
        let Some(pos) = player.entity().position(host) else {
            continue;
        };
        let pp = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];
        let near_unlocked = state.rifts.iter().enumerate().any(|(idx, rift)| {
            !rift.pacified
                && !rift.regressing
                && rift_mobs_unlocked_in_state(state, idx)
                && (state.is_quart_corrupted(pp)
                    || horiz_dist_sq(rift.center, pp)
                        <= (rift.max_radius_sq + 4096).max(64 * 64))
        });
        if near_unlocked {
            player_anchors.push(pp);
        }
    }
    if !player_anchors.is_empty() {
        let pick = (state.tick / STRIDER_SPAWN_INTERVAL_TICKS) as usize % player_anchors.len();
        return Some(player_anchors[pick]);
    }

    let rift_anchors: Vec<[i32; 3]> = state
        .rifts
        .iter()
        .enumerate()
        .filter(|(idx, r)| !r.pacified && !r.regressing && rift_mobs_unlocked_in_state(state, *idx))
        .map(|(_, r)| r.center)
        .collect();
    if rift_anchors.is_empty() {
        return None;
    }
    let pick = (state.tick / STRIDER_SPAWN_INTERVAL_TICKS) as usize % rift_anchors.len();
    Some(rift_anchors[pick])
}

fn is_open_lava_surface(host: &dyn ServerHost, level: LevelRef, pos: [i32; 3]) -> bool {
    let [x, lava_y, z] = pos;
    level.is_loaded(host, pos)
        && level
            .block(host, pos)
            .is_some_and(|k| strip_ns(&k.id(host)) == "lava")
        && level
            .block(host, [x, lava_y + 1, z])
            .is_some_and(|k| is_air_like(&k.id(host)))
        && level
            .block(host, [x, lava_y + 2, z])
            .is_some_and(|k| is_air_like(&k.id(host)))
}

/// Actively populates corrupted Overworld lava oceans and rivers with Striders
/// (equipping ~25% with saddles so players can immediately ride them across molten lakes).
fn try_spawn_lava_striders(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    entities: &[(EntityRef, u32, String, [f64; 3])],
) {
    const MAX_OVERWORLD_STRIDERS: usize = 16;
    let strider_count = entities
        .iter()
        .filter(|(_, _, kind, _)| strip_ns(kind) == "strider")
        .count();
    if strider_count >= MAX_OVERWORLD_STRIDERS {
        return;
    }

    let Some(anchor) = active_strider_anchor(state, host) else {
        return;
    };

    let mut probes = Vec::with_capacity(8);
    let loaded_y = anchor[1].clamp(OVERWORLD_MIN_Y + 1, OVERWORLD_MAX_Y - 2);
    for i in 0..8usize {
        let h = spatial_hash(
            anchor[0],
            anchor[1],
            anchor[2],
            state.tick ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let dist = 10 + (h % 35) as i32;
        let angle = ((h >> 12) % 360) as f64 * std::f64::consts::TAU / 360.0;
        let sx = anchor[0] + (f64::from(dist) * angle.cos()).round() as i32;
        let sz = anchor[2] + (f64::from(dist) * angle.sin()).round() as i32;
        if level.is_loaded(host, [sx, loaded_y, sz]) {
            probes.push((sx, sz));
        }
    }
    if probes.is_empty() {
        return;
    }

    let Ok(heights) = host.surface_heights(OVERWORLD, &probes, Heightmap::WorldSurface) else {
        return;
    };

    let mut remaining_budget = MAX_OVERWORLD_STRIDERS - strider_count;
    for (idx, ((sx, sz), opt_sy)) in probes.into_iter().zip(heights.into_iter()).enumerate() {
        let Some(sy) = opt_sy else {
            continue;
        };
        let lava_y = sy - 1;
        if lava_y <= OVERWORLD_MIN_Y + 4 || lava_y + 2 > OVERWORLD_MAX_Y {
            continue;
        }
        if !location_unlocks_nether_mobs(state, [sx, lava_y, sz]) {
            continue;
        }
        if !is_open_lava_surface(host, level, [sx, lava_y, sz]) {
            continue;
        }

        let adj_open_lava = is_open_lava_surface(host, level, [sx + 1, lava_y, sz]);
        let roll = spatial_hash(sx, lava_y, sz, state.tick ^ (idx as u64));
        let herd = (1 + ((roll >> 8) % 2) as usize).min(remaining_budget);
        let mut spawned_any = false;
        for member in 0..herd {
            let mx = if member > 0 && adj_open_lava {
                sx + 1
            } else {
                sx
            };
            let spawn_pos = [
                f64::from(mx) + 0.5,
                f64::from(lava_y + 1),
                f64::from(sz) + 0.5,
            ];
            let member_roll = spatial_hash(
                mx,
                lava_y,
                sz,
                state.tick ^ (member as u64).wrapping_mul(0x517C_C1B7_2722_0A95),
            );
            if let Some(eid) =
                host.summon(OVERWORLD, "minecraft:strider", spawn_pos, Compound::new())
            {
                spawned_any = true;
                remaining_budget -= 1;
                if (member_roll % 4) == 0
                    && let Some(ent) = host.entity(eid)
                    && let Some(living) = host.living(ent)
                    && let Ok(ident) = Identifier::parse("minecraft:saddle")
                    && let Some(item_key) = host.item_key(&ident)
                    && let Some(saddle) = ItemStack::new(item_key, 1)
                {
                    let _ = living.set_equipment(host, EquipmentSlot::Saddle, Some(saddle));
                }
            }
            if remaining_budget == 0 {
                break;
            }
        }
        if spawned_any {
            return;
        }
    }
}

pub fn filter_and_populate_nether_spawn_entries(
    entries: &mut Vec<SpawnEntry>,
    category: &str,
    flavor: NetherFlavor,
    y: i32,
) {
    // Strip ALL normal Overworld fauna (cows, sheep, zombies, skeletons, bats, squids, etc.)
    // so only native Nether mobs can ever populate a converted Nether biome.
    entries.retain(|e| is_allowed_in_nether_biome(&e.entity, Some(flavor)));

    let push_if_missing =
        |entries: &mut Vec<SpawnEntry>, entity: &str, weight: u32, min: u32, max: u32| {
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

    match (category, flavor) {
        ("monster", NetherFlavor::CrimsonForest) => {
            push_if_missing(entries, "minecraft:piglin", 28, 2, 4);
            push_if_missing(entries, "minecraft:hoglin", 18, 1, 3);
            push_if_missing(entries, "minecraft:zombified_piglin", 8, 1, 2);
        }
        ("monster", NetherFlavor::WarpedForest) => {
            push_if_missing(entries, "minecraft:enderman", 26, 2, 4);
            push_if_missing(entries, "minecraft:piglin", 10, 1, 2);
            push_if_missing(entries, "minecraft:blaze", 4, 1, 2);
            push_if_missing(entries, "minecraft:magma_cube", 4, 1, 2);
        }
        ("monster", NetherFlavor::SoulSandValley) => {
            push_if_missing(entries, "minecraft:wither_skeleton", 24, 1, 3);
            push_if_missing(entries, "minecraft:ghast", 12, 1, 1);
            push_if_missing(entries, "minecraft:enderman", 8, 1, 2);
            push_if_missing(entries, "minecraft:blaze", 6, 1, 2);
        }
        ("monster", NetherFlavor::BasaltDeltas) => {
            push_if_missing(entries, "minecraft:magma_cube", 32, 2, 4);
            push_if_missing(entries, "minecraft:ghast", 12, 1, 1);
            push_if_missing(entries, "minecraft:blaze", 8, 1, 2);
            push_if_missing(entries, "minecraft:piglin", 4, 1, 2);
        }
        ("monster", NetherFlavor::NetherWastes) => {
            push_if_missing(entries, "minecraft:piglin", 24, 2, 4);
            push_if_missing(entries, "minecraft:zombified_piglin", 18, 2, 4);
            push_if_missing(entries, "minecraft:magma_cube", 10, 1, 2);
            push_if_missing(entries, "minecraft:ghast", 8, 1, 1);
            push_if_missing(entries, "minecraft:blaze", 6, 1, 2);
        }
        ("creature", NetherFlavor::CrimsonForest) => {
            push_if_missing(entries, "minecraft:hoglin", 24, 1, 3);
            push_if_missing(entries, "minecraft:strider", 10, 1, 3);
        }
        ("creature", _) => {
            push_if_missing(entries, "minecraft:strider", 10, 1, 3);
        }
        _ => {}
    }

    if category == "monster" {
        tune_underground_spawn_entries(entries, y);
    }
}

pub fn augment_potential_spawns(
    host: &dyn Host,
    state: &RiftState,
    event: &mut PotentialSpawnsEvent<'_>,
) {
    if event.dimension != OVERWORLD {
        return;
    }

    let biome_flavor = host
        .level(OVERWORLD)
        .and_then(|l| local_corrupt_flavor(host, l, event.pos));
    let in_nether_zone = biome_flavor.is_some() || state.is_quart_corrupted(event.pos);
    let near_corrupt = in_nether_zone
        || state
            .rifts
            .iter()
            .any(|r| horiz_dist_sq(r.center, event.pos) <= (r.max_radius_sq + 512).max(48 * 48));
    if !near_corrupt {
        return;
    }

    // Before the corrupted area becomes a pretty large Nether biome, block premature
    // Nether mob spawns — and inside converted Nether biomes, block normal Overworld mobs too!
    if !location_unlocks_nether_mobs(state, event.pos) {
        event
            .entries
            .retain(|e| !is_nether_predator(&e.entity) && strip_ns(&e.entity) != "strider");
        if in_nether_zone {
            event
                .entries
                .retain(|e| is_allowed_in_nether_biome(&e.entity, biome_flavor));
        }
        return;
    }

    let effective_flavor = biome_flavor
        .unwrap_or_else(|| NetherFlavor::pick_for_region(event.pos[0], event.pos[2], state.seed));
    filter_and_populate_nether_spawn_entries(
        &mut event.entries,
        event.category,
        effective_flavor,
        event.pos[1],
    );
}

fn push_item_drop(host: &dyn ServerHost, drops: &mut Vec<ItemStack>, id: &str, count: u32) {
    let Ok(ident) = Identifier::parse(id) else {
        return;
    };
    if let Some(item_key) = host.item_key(&ident)
        && let Some(stack) = ItemStack::new(item_key, count)
    {
        drops.push(stack);
    }
}

pub fn append_nether_touched_drops(
    host: &dyn ServerHost,
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
            push_item_drop(host, drops, "minecraft:gold_nugget", 1 + (h % 3) as u32);
            if h % 3 == 0 {
                push_item_drop(host, drops, "minecraft:blaze_powder", 1);
            }
        }
        "wither_skeleton" | "magma_cube" | "blaze" => {
            if h % 4 == 0 {
                push_item_drop(host, drops, "minecraft:magma_cream", 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RiftSource, build_ring_columns};

    fn sample_rift(corrupted_blocks: u64, radius: i32) -> RiftSource {
        RiftSource {
            id: 1,
            center: [0, 64, 0],
            active_portal: true,
            contained: false,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 6_000,
            corrupted_blocks,
            max_radius_sq: i64::from(radius) * i64::from(radius),
            ring_radius: radius,
            ring_columns: build_ring_columns(0, 0, radius),
            active_column: None,
            deepen_radius: 10,
            deepen_columns: Vec::new(),
            max_deepen_reached: 10,
            bastions_placed: 0,
            outposts_placed: 0,
            citadel_spawned: false,
            boss_uuid: None,
            pacified: false,
            regressing: false,
            regress_radius: 0,
            regress_columns: Vec::new(),
            regress_active_column: None,
        }
    }

    #[test]
    fn nether_mobs_do_not_unlock_until_corruption_forms_large_biome() {
        let mut state = RiftState::new();
        state.mark_quart_corrupted([4, 64, 4]);

        // Small newly-lit rift must keep mobs locked.
        state.rifts.push(sample_rift(150, 4));
        assert!(!rift_mobs_unlocked(&state.rifts[0]));
        assert!(!location_unlocks_nether_mobs(&state, [4, 64, 4]));

        // Large block count but too narrow radius still locked.
        state.rifts[0] = sample_rift(60_000, 18);
        assert!(!rift_mobs_unlocked(&state.rifts[0]));

        // Wide radius but not enough converted blocks still locked.
        state.rifts[0] = sample_rift(20_000, 36);
        assert!(!rift_mobs_unlocked(&state.rifts[0]));

        // Large biome (>= 55k blocks and radius >= 32) unlocks mobs.
        state.rifts[0] = sample_rift(60_000, 36);
        assert!(rift_mobs_unlocked(&state.rifts[0]));
        assert!(location_unlocks_nether_mobs(&state, [4, 64, 4]));

        // Two separate ~30k rifts stay locked while apart, then pool together once they meet!
        let mut parted = RiftState::new();
        parted.mark_quart_corrupted([4, 64, 4]);
        parted.rifts.push(sample_rift(30_000, 24));
        let mut far_b = sample_rift(30_000, 24);
        far_b.id = 2;
        far_b.center = [120, 64, 0];
        parted.rifts.push(far_b);
        assert!(!location_unlocks_nether_mobs(&parted, [4, 64, 4]));

        // Bring Rift #2 within 24+24 blocks so their corrupted wastelands touch -> pooled 60k unlocks!
        parted.rifts[1].center = [44, 64, 0];
        assert!(location_unlocks_nether_mobs(&parted, [4, 64, 4]));
    }

    #[test]
    fn spawns_biome_appropriate_nether_mobs() {
        assert_eq!(
            pick_mob_for_flavor(NetherFlavor::CrimsonForest, 0, 70_000),
            "minecraft:hoglin"
        );
        assert_eq!(
            pick_mob_for_flavor(NetherFlavor::SoulSandValley, 0, 70_000),
            "minecraft:wither_skeleton"
        );
        assert_eq!(
            pick_mob_for_flavor(NetherFlavor::BasaltDeltas, 0, 70_000),
            "minecraft:magma_cube"
        );
        assert_eq!(
            pick_mob_for_flavor(NetherFlavor::WarpedForest, 0, 70_000),
            "minecraft:enderman"
        );
    }

    #[test]
    fn maps_killed_overworld_creatures_to_nether_versions() {
        assert_eq!(
            nether_death_conversion_target("minecraft:pig"),
            Some("minecraft:hoglin")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:cow"),
            Some("minecraft:hoglin")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:sheep"),
            Some("minecraft:hoglin")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:villager"),
            Some("minecraft:piglin")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:wandering_trader"),
            Some("minecraft:piglin")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:iron_golem"),
            Some("minecraft:piglin_brute")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:zombie"),
            Some("minecraft:zombified_piglin")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:skeleton"),
            Some("minecraft:wither_skeleton")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:slime"),
            Some("minecraft:magma_cube")
        );
        assert_eq!(
            nether_death_conversion_target("minecraft:creeper"),
            Some("minecraft:blaze")
        );
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

    #[test]
    fn citadel_wither_and_nether_guards_never_friendly_fire() {
        let mut st = RiftState::new();
        st.known_withers.insert(100);
        st.known_nether_mobs.insert(201);
        st.citadel_guards.insert(201);
        st.citadel_guards.insert(202);

        // Wither hitting a Hoglin or Piglin Brute Captain must cancel.
        assert!(should_cancel_damage_pair(
            &st,
            Some(201),
            "minecraft:hoglin",
            "minecraft:explosion.player",
            Some(100),
            None,
        ));
        assert!(should_cancel_damage_pair(
            &st,
            Some(202),
            "minecraft:piglin_brute",
            "minecraft:wither_skull",
            Some(100),
            None,
        ));
        // Captain hitting Wither must cancel.
        assert!(should_cancel_damage_pair(
            &st,
            Some(100),
            "minecraft:wither",
            "minecraft:mob_attack",
            Some(201),
            Some(201),
        ));
        // Allied Hoglins & Captains never hurt each other.
        assert!(should_cancel_damage_pair(
            &st,
            Some(202),
            "minecraft:hoglin",
            "minecraft:mob_attack",
            Some(201),
            None,
        ));
        // Players CAN still damage both the Wither and guards!
        assert!(!should_cancel_damage_pair(
            &st,
            Some(100),
            "minecraft:wither",
            "minecraft:player_attack",
            Some(999),
            Some(999),
        ));
        assert!(!should_cancel_damage_pair(
            &st,
            Some(201),
            "minecraft:piglin_brute",
            "minecraft:player_attack",
            Some(999),
            None,
        ));
    }

    #[test]
    fn underground_spawns_scale_by_depth_and_balance_overworld_caves() {
        assert_eq!(underground_spawn_chance_pct(64, 65), 100);
        assert!(underground_spawn_chance_pct(24, 65) > underground_spawn_chance_pct(-5, 65));
        assert!(underground_spawn_chance_pct(-5, 65) > underground_spawn_chance_pct(-45, 65));
        assert!(underground_spawn_chance_pct(-45, 65) > 0);

        let mut deep_entries = vec![
            SpawnEntry {
                entity: "minecraft:zombie".to_owned(),
                weight: 100,
                min: 2,
                max: 4,
                constant: false,
            },
            SpawnEntry {
                entity: "minecraft:magma_cube".to_owned(),
                weight: 20,
                min: 1,
                max: 3,
                constant: false,
            },
            SpawnEntry {
                entity: "minecraft:ghast".to_owned(),
                weight: 10,
                min: 1,
                max: 1,
                constant: true,
            },
        ];
        tune_underground_spawn_entries(&mut deep_entries, -40);
        assert!(!deep_entries.iter().any(|e| e.entity == "minecraft:ghast"));
        assert!(!deep_entries.iter().any(|e| e.entity == "minecraft:zombie"));
        let m_w = deep_entries
            .iter()
            .find(|e| e.entity == "minecraft:magma_cube")
            .unwrap()
            .weight;
        assert!((10..20).contains(&m_w));
    }

    #[test]
    fn normal_overworld_mobs_are_blocked_and_nether_mobs_populated_in_nether_biomes() {
        for normal in [
            "minecraft:cow",
            "minecraft:pig",
            "minecraft:sheep",
            "minecraft:chicken",
            "minecraft:zombie",
            "minecraft:creeper",
            "minecraft:spider",
            "minecraft:skeleton",
            "minecraft:drowned",
            "minecraft:slime",
            "minecraft:bat",
            "minecraft:squid",
            "minecraft:glow_squid",
            "minecraft:cod",
            "minecraft:phantom",
            "minecraft:pillager",
            "minecraft:wandering_trader",
        ] {
            for flavor in [
                NetherFlavor::CrimsonForest,
                NetherFlavor::WarpedForest,
                NetherFlavor::SoulSandValley,
                NetherFlavor::BasaltDeltas,
                NetherFlavor::NetherWastes,
            ] {
                assert!(
                    !is_allowed_in_nether_biome(normal, Some(flavor)),
                    "{normal} must NOT be allowed in {flavor:?}"
                );
            }
            for reason in [
                SpawnReason::Natural,
                SpawnReason::ChunkGeneration,
                SpawnReason::Patrol,
                SpawnReason::Event,
                SpawnReason::Reinforcement,
            ] {
                assert!(should_cancel_world_spawn_kind(
                    normal,
                    reason,
                    true,
                    Some(NetherFlavor::CrimsonForest),
                    true,
                    true,
                ));
                // Uncorrupted Overworld chunks still allow normal spawns.
                assert!(!should_cancel_world_spawn_kind(
                    normal,
                    reason,
                    false,
                    None,
                    false,
                    false,
                ));
            }
            // Player commands and conversions inside Nether biomes are still respected.
            assert!(!should_cancel_world_spawn_kind(
                normal,
                SpawnReason::Command,
                true,
                Some(NetherFlavor::CrimsonForest),
                true,
                true,
            ));
        }

        // Potential spawns list strips Overworld zombies/cows/squids and injects rich Nether spawns.
        let mut monster_entries = vec![
            SpawnEntry {
                entity: "minecraft:zombie".to_owned(),
                weight: 95,
                min: 2,
                max: 4,
                constant: false,
            },
            SpawnEntry {
                entity: "minecraft:creeper".to_owned(),
                weight: 95,
                min: 1,
                max: 4,
                constant: false,
            },
        ];
        filter_and_populate_nether_spawn_entries(
            &mut monster_entries,
            "monster",
            NetherFlavor::CrimsonForest,
            70,
        );
        assert!(!monster_entries.iter().any(|e| e.entity == "minecraft:zombie"));
        assert!(!monster_entries.iter().any(|e| e.entity == "minecraft:creeper"));
        assert!(monster_entries.iter().any(|e| e.entity == "minecraft:piglin"));
        assert!(monster_entries.iter().any(|e| e.entity == "minecraft:hoglin"));

        let mut water_entries = vec![SpawnEntry {
            entity: "minecraft:squid".to_owned(),
            weight: 10,
            min: 1,
            max: 4,
            constant: false,
        }];
        filter_and_populate_nether_spawn_entries(
            &mut water_entries,
            "water_creature",
            NetherFlavor::NetherWastes,
            60,
        );
        assert!(water_entries.is_empty());
    }

    #[test]
    fn overworld_defenders_fight_back_and_kill_patch_triggers_appropriately() {
        assert!(is_overworld_defender_mob("minecraft:zombie"));
        assert!(is_overworld_defender_mob("minecraft:skeleton"));
        assert!(is_overworld_defender_mob("minecraft:iron_golem"));
        assert!(!is_overworld_defender_mob("minecraft:piglin"));
        assert!(!is_overworld_defender_mob("minecraft:cow"));

        // Nether kills Overworld -> scorches uncorrupted patch.
        assert!(should_corrupt_kill_patch(true, false, true, false));
        // Overworld defender kills Nether -> scorches uncorrupted patch.
        assert!(should_corrupt_kill_patch(false, true, true, false));
        // Player killing a stray Nether invader on clean land won't grief their own lawn.
        assert!(!should_corrupt_kill_patch(false, true, true, true));
        // Pure Overworld-on-Overworld fight never spawns Nether corruption.
        assert!(!should_corrupt_kill_patch(false, false, true, false));
        // Natural despawn / fall with no attacker does not trigger a kill patch.
        assert!(!should_corrupt_kill_patch(false, true, false, false));

        // Non-Nether mob dying anywhere near active Nether front / fighting Nether mobs OR slain
        // by a Nether attacker always scorches a small patch; far away peaceful kills do not.
        assert!(should_corrupt_non_nether_death(false, true, false));
        assert!(should_corrupt_non_nether_death(false, false, true));
        assert!(!should_corrupt_non_nether_death(false, false, false));
        assert!(!should_corrupt_non_nether_death(true, false, true));
    }
}
