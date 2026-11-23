use crate::atmosphere::ParticlePalette;
use crate::corruption::{is_air_like, is_open_space};
use crate::state::RiftState;
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, dist_sq, horiz_dist_sq,
    spatial_hash, strip_ns,
};
use mod_api::{
    ActiveEffect, BlockUpdateFlags, Compound, EntityRef, FinalizeSpawnEvent, GoalSpec, Heightmap,
    Host, Identifier, ItemStack, LevelRef, LivingChangeTargetEvent, LivingDeathEvent,
    LivingIncomingDamageEvent, MobFilter, ParticleOptions, PotentialSpawnsEvent, ServerHost,
    SoundSource, SpawnEntry, SpawnReason, Tag,
};

const MOB_TICK_INTERVAL: u64 = 20;
const HOGLIN_SPREAD_INTERVAL_TICKS: u64 = 24;
const SUPPLEMENTAL_SPAWN_INTERVAL_TICKS: u64 = 100;

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

/// Blocks vanilla natural spawns and portal piglin leaks in Overworld near an immature Rift
/// until the corruption actually covers a large Nether biome.
pub fn should_cancel_finalize_spawn(state: &RiftState, event: &FinalizeSpawnEvent<'_>) -> bool {
    if event.dimension != OVERWORLD {
        return false;
    }
    if !is_nether_predator(event.kind) && strip_ns(event.kind) != "strider" {
        return false;
    }
    if !matches!(
        event.reason,
        SpawnReason::Natural | SpawnReason::ChunkGeneration | SpawnReason::Structure
    ) {
        return false;
    }
    let bp = [
        event.pos[0].floor() as i32,
        event.pos[1].floor() as i32,
        event.pos[2].floor() as i32,
    ];
    let near_any_rift = state.is_quart_corrupted(bp)
        || state
            .rifts
            .iter()
            .any(|r| horiz_dist_sq(r.center, bp) <= (r.max_radius_sq + 512).max(32 * 32));
    near_any_rift && !location_unlocks_nether_mobs(state, bp)
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

/// Maps any Overworld creature killed by Nether mobs (or dying inside an established
/// large corrupted Nether biome) to its transformed Nether counterpart using vanilla mobs.
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
    if !do_combat && !do_hoglin && !do_spawn {
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
    let mut hunters: Vec<(EntityRef, [f64; 3])> = Vec::new();
    let mut prey: Vec<(EntityRef, [f64; 3])> = Vec::new();
    let mut fire_buffs: Vec<EntityRef> = Vec::new();
    let mut immunize_mobs: Vec<u32> = Vec::new();
    let mut touched_buffs: Vec<(EntityRef, [f64; 3])> = Vec::new();
    let mut withers: Vec<(EntityRef, u32)> = Vec::new();

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
                    .any(|c| horiz_dist_sq(*c, bp) <= 44 * 44);
            if is_sovereign {
                state.known_nether_mobs.insert(*id);
                state.known_withers.insert(*id);
                withers.push((*ent, *id));
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
                "piglin" | "piglin_brute" | "hoglin" | "wither_skeleton" | "blaze" | "zoglin"
            ) {
                hunters.push((*ent, *pos));
            }
            if is_piglin_or_hoglin(kind) {
                if (state.tick + u64::from(*id)) % 80 < MOB_TICK_INTERVAL {
                    fire_buffs.push(*ent);
                }
                if pos_is_corrupted(host, level, state, bp)
                    && state.zombification_immune_mobs.insert(*id)
                {
                    immunize_mobs.push(*id);
                }
            }
        } else if nether_death_conversion_target(kind).is_some() {
            prey.push((*ent, *pos));
        }

        if is_nether_touched_candidate(kind) && location_unlocks_nether_mobs(state, bp) {
            let newly_touched = state.nether_touched_mobs.insert(*id);
            if newly_touched {
                touched_buffs.push((*ent, *pos));
            }
            if let Some(pal) = particles {
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

    for id in immunize_mobs {
        stabilize_zombification_immune(host, id);
    }
    for ent in fire_buffs {
        apply_mob_effects(host, ent, 20 * 90, 0);
    }
    for (ent, _pos) in touched_buffs {
        apply_mob_effects(host, ent, 20 * 600, 20 * 600);
    }

    // Ensure Sovereign Withers (even loaded from older saves) target only Players and
    // drop any stray lock onto nearby Nether guards, and keep allied guards off the Wither.
    for (w_ent, _) in withers {
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
        }
    }

    for (hunter_ent, _) in &hunters {
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

    if hunters.is_empty() || prey.is_empty() {
        return;
    }

    let hunt_range_sq = 32.0 * 32.0;

    for (hunter_ent, h_pos) in hunters {
        let Some(mob) = host.mob(hunter_ent) else {
            continue;
        };
        // Skip hunters that already have an active target so we don't re-trigger
        // aggro / angry sounds every second.
        if mob.target(host).is_some() {
            continue;
        }
        let mut best: Option<(EntityRef, f64)> = None;
        for (prey_ent, p_pos) in &prey {
            let dx = h_pos[0] - p_pos[0];
            let dy = h_pos[1] - p_pos[1];
            let dz = h_pos[2] - p_pos[2];
            let d2 = dx * dx + dy * dy + dz * dz;
            if d2 <= hunt_range_sq && best.is_none_or(|(_, b)| d2 < b) {
                best = Some((*prey_ent, d2));
            }
        }
        if let Some((victim_ent, _)) = best
            && let Some(target_living) = host.living(victim_ent)
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

/// Converts any Overworld creature killed by a Nether mob (or slain inside an already-large
/// corrupted Nether biome) into its Nether version on death.
pub fn handle_living_death(
    host: &mut dyn ServerHost,
    state: &mut RiftState,
    event: &LivingDeathEvent<'_>,
    particles: Option<&ParticlePalette>,
) {
    if strip_ns(event.kind) == "player" {
        return;
    }
    let Some(target_kind) = nether_death_conversion_target(event.kind) else {
        return;
    };
    let Some(level) = host.level(OVERWORLD) else {
        return;
    };
    let Some(pos) = host
        .entity(event.entity)
        .and_then(|e| e.position(host))
        .or(event.source.position)
    else {
        return;
    };
    let bp = [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ];

    let attacker_id = u32::try_from(event.source.causing)
        .ok()
        .filter(|&v| v > 0)
        .or_else(|| u32::try_from(event.source.direct).ok().filter(|&v| v > 0));

    let attacker_is_nether = attacker_id.is_some_and(|aid| {
        state.known_nether_mobs.contains(&aid)
            || state.nether_touched_mobs.contains(&aid)
            || host
                .entity(aid)
                .and_then(|e| e.kind(host))
                .is_some_and(|k| is_nether_predator(&k.id(host)))
    });

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
        (NetherFlavor::WarpedForest, _) => "minecraft:strider",
        (NetherFlavor::SoulSandValley, 0..=4) => "minecraft:wither_skeleton",
        (NetherFlavor::SoulSandValley, 5..=7) => "minecraft:skeleton",
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

fn process_supplemental_spawns(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    entities: &[(EntityRef, u32, String, [f64; 3])],
) {
    // Wait until at least one active rift (or connected wasteland) forms a pretty large Nether biome.
    if !state
        .rifts
        .iter()
        .enumerate()
        .any(|(idx, r)| r.is_actively_spreading() && rift_mobs_unlocked_in_state(state, idx))
    {
        return;
    }

    let nether_mob_count = entities
        .iter()
        .filter(|(_, _, k, _)| is_nether_predator(k))
        .count();
    if nether_mob_count >= 34 {
        return;
    }

    let mut spawn_anchor: Option<(u64, NetherFlavor, u64, [i32; 3])> = None;

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
                    && horiz_dist_sq(r.center, pp) <= 96 * 96
            })
            .min_by_key(|(_, r)| dist_sq(r.center, pp))
        {
            let w_blocks = state.connected_wasteland_for(idx).total_corrupted_blocks;
            spawn_anchor = Some((rift.id, rift.flavor, w_blocks, pp));
            break;
        }
    }

    if spawn_anchor.is_none()
        && let Some((idx, rift)) = state
            .rifts
            .iter()
            .enumerate()
            .find(|(idx, r)| r.is_actively_spreading() && rift_mobs_unlocked_in_state(state, *idx))
    {
        let w_blocks = state.connected_wasteland_for(idx).total_corrupted_blocks;
        spawn_anchor = Some((rift.id, rift.flavor, w_blocks, rift.center));
    }

    let Some((rift_id, fallback_flavor, corrupted_blocks, anchor)) = spawn_anchor else {
        return;
    };

    let h = spatial_hash(anchor[0], anchor[1], anchor[2], state.tick ^ rift_id);
    let dx = ((h & 31) as i32) - 16;
    let dz = (((h >> 8) & 31) as i32) - 16;
    let sx = anchor[0] + dx;
    let sz = anchor[2] + dz;
    let Ok(heights) =
        host.surface_heights(OVERWORLD, &[(sx, sz)], Heightmap::MotionBlockingNoLeaves)
    else {
        return;
    };
    let Some(Some(sy)) = heights.first().copied() else {
        return;
    };
    let ground = [sx, sy - 1, sz];
    if !location_unlocks_nether_mobs(state, ground) {
        return;
    }

    let local_flavor = local_corrupt_flavor(host, level, ground).unwrap_or(fallback_flavor);
    let mob_kind = pick_mob_for_flavor(local_flavor, h >> 16, corrupted_blocks);
    let spawn_y = if mob_kind == "minecraft:ghast" {
        f64::from(sy + 14)
    } else {
        f64::from(sy)
    };
    let mut nbt = Compound::new();
    if is_piglin_or_hoglin(mob_kind) {
        nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
        nbt.insert("TimeInOverworld".into(), Tag::Int(0));
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

pub fn augment_potential_spawns(
    host: &dyn Host,
    state: &RiftState,
    event: &mut PotentialSpawnsEvent<'_>,
) {
    if event.dimension != OVERWORLD {
        return;
    }

    let near_corrupt = state.is_quart_corrupted(event.pos)
        || state.rifts.iter().any(|r| {
            !r.pacified
                && !r.regressing
                && horiz_dist_sq(r.center, event.pos) <= (r.max_radius_sq + 256).max(32 * 32)
        });
    if !near_corrupt {
        return;
    }

    // Before the corrupted area becomes a pretty large Nether biome, block premature
    // Nether mob spawns even if vanilla biome cells already started converting.
    if !location_unlocks_nether_mobs(state, event.pos) {
        event
            .entries
            .retain(|e| !is_nether_predator(&e.entity) && strip_ns(&e.entity) != "strider");
        return;
    }

    let flavor = host
        .level(OVERWORLD)
        .and_then(|l| local_corrupt_flavor(host, l, event.pos))
        .unwrap_or_else(|| NetherFlavor::pick_for_region(event.pos[0], event.pos[2], state.seed));

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

    match (event.category, flavor) {
        ("monster", NetherFlavor::CrimsonForest) => {
            push_if_missing(&mut event.entries, "minecraft:piglin", 24, 2, 4);
            push_if_missing(&mut event.entries, "minecraft:zombified_piglin", 6, 1, 2);
        }
        ("monster", NetherFlavor::WarpedForest) => {
            push_if_missing(&mut event.entries, "minecraft:enderman", 20, 1, 3);
            push_if_missing(&mut event.entries, "minecraft:piglin", 8, 1, 2);
            push_if_missing(&mut event.entries, "minecraft:blaze", 3, 1, 1);
        }
        ("monster", NetherFlavor::SoulSandValley) => {
            push_if_missing(&mut event.entries, "minecraft:wither_skeleton", 16, 1, 2);
            push_if_missing(&mut event.entries, "minecraft:skeleton", 14, 2, 3);
            push_if_missing(&mut event.entries, "minecraft:ghast", 10, 1, 1);
            push_if_missing(&mut event.entries, "minecraft:blaze", 4, 1, 1);
        }
        ("monster", NetherFlavor::BasaltDeltas) => {
            push_if_missing(&mut event.entries, "minecraft:magma_cube", 28, 2, 4);
            push_if_missing(&mut event.entries, "minecraft:ghast", 10, 1, 1);
            push_if_missing(&mut event.entries, "minecraft:blaze", 4, 1, 1);
        }
        ("monster", NetherFlavor::NetherWastes) => {
            push_if_missing(&mut event.entries, "minecraft:piglin", 20, 2, 4);
            push_if_missing(&mut event.entries, "minecraft:zombified_piglin", 12, 1, 3);
            push_if_missing(&mut event.entries, "minecraft:magma_cube", 8, 1, 2);
            push_if_missing(&mut event.entries, "minecraft:ghast", 6, 1, 1);
            push_if_missing(&mut event.entries, "minecraft:blaze", 4, 1, 1);
        }
        ("creature", NetherFlavor::CrimsonForest) => {
            push_if_missing(&mut event.entries, "minecraft:hoglin", 20, 1, 3);
            push_if_missing(&mut event.entries, "minecraft:strider", 4, 1, 2);
        }
        ("creature", _) => {
            push_if_missing(&mut event.entries, "minecraft:strider", 6, 1, 2);
        }
        _ => {}
    }
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
}
