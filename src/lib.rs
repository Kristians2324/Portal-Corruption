mod atmosphere;
mod bastion;
mod citadel;
mod containment;
mod corruption;
mod loot;
mod mobs;
mod nether_fob;
mod outposts;
mod regression;
mod state;
mod types;

use atmosphere::{ClientAtmosphere, ParticlePalette};
use containment::{ContainmentOutcome, ContainmentView, evaluate_chamber, inspect_portal_cluster};
use corruption::{
    SURFACE_CRUST_DEPTH, is_air_like, is_nether_terrain_block, is_preserved_ore,
    is_protected_block, is_snow_or_ice, is_water_like, rift_deepen_ops_per_tick,
    rift_surface_ops_per_tick, surface_column_y_range, transform_block,
};
use loot::ItemPalette;
use mobs::paint_biome_column;
use state::RiftState;
use types::{
    BEDROCK_FLOOR_Y, NETHER, NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, RiftSource,
    build_ring_columns, dist_sq, horiz_dist_sq, neighbors_6, spatial_hash, strip_ns,
};

use mod_api::{
    BlockChangedEvent, BlockSpreadEvent, BlockUpdateFlags, BucketEmptyEvent, ClientHost,
    ClientLevelLoadEvent, ClientLevelUnloadEvent, ClientTickPostEvent, Command,
    CommandArgumentValue, ComputeFogColorEvent, ContainerLootFilledEvent, CreateFluidSourceEvent,
    ExplosionDetonateEvent, FillBucketEvent, FinalizeSpawnEvent, Flow, FluidLevelChangeEvent,
    Heightmap, Hooks, Host, HostSide, Identifier, ItemStack, LevelDimension, LevelHost,
    LevelRandomHost, LevelRef, LevelTickPostEvent, LivingChangeTargetEvent,
    LivingConversionPreEvent, LivingDeathEvent, LivingIncomingDamageEvent, LootDropsEvent,
    MobSpawnPlacementCheckEvent, MobSpawnPositionCheckEvent, MusicEvent, ParticleOptions,
    PortalCreateEvent, PotentialSpawnsEvent, RegisterCommandsEvent, Registrar,
    RegistrationContext, RenderFogEvent, RunModCommandEvent, ServerHost, ServerStartedEvent,
    ServerStoppingEvent, SoundSource, Text, WorldLifecycleEvent, WorldPhase, WorldSavedEvent,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

const MAX_CORE_FORCED_CHUNKS: usize = 16;
const MAX_FRONTIER_FORCED_CHUNKS: usize = 40;

/// Re-entrancy-safe ingress queues populated by hooks (`block_changed`, `portal_create`, etc.)
/// without ever holding this lock across a host call.
#[derive(Default)]
struct HookQueues {
    pending_portals: VecDeque<[i32; 3]>,
    pending_containment_checks: Vec<[i32; 3]>,
    pending_water_evap: Vec<[i32; 3]>,
    pending_bucket_evap: Vec<(u128, [i32; 3])>,
    pending_deaths: Vec<mobs::PendingDeath>,
}

impl HookQueues {
    fn queue_bucket_evap(&mut self, player: u128, pos: [i32; 3]) {
        // Coalesce multiple `bucket_empty` checks from the same click (e.g. waterloggable
        // block + adjacent face block) so only one bucket empties and smoke pops cleanly on top.
        if let Some((_, existing_pos)) = self
            .pending_bucket_evap
            .iter_mut()
            .find(|(uuid, _)| *uuid == player)
        {
            if pos[1] > existing_pos[1] {
                *existing_pos = pos;
            }
        } else {
            self.pending_bucket_evap.push((player, pos));
        }
    }
}

#[derive(Default)]
pub struct Mod {
    queues: Mutex<HookQueues>,
    state: Mutex<RiftState>,
    atmo: Mutex<ClientAtmosphere>,
    particles: Mutex<Option<ParticlePalette>>,
    items: Mutex<Option<ItemPalette>>,
}

struct ServerContainmentView<'a> {
    host: &'a dyn ServerHost,
    level: LevelRef,
}

impl ContainmentView for ServerContainmentView<'_> {
    fn block_name(&self, pos: [i32; 3]) -> Option<String> {
        if !self.level.is_loaded(self.host, pos) {
            return None;
        }
        self.level
            .block(self.host, pos)
            .map(|k| k.id(self.host))
    }

    fn is_open(&self, pos: [i32; 3]) -> Option<bool> {
        if !self.level.is_loaded(self.host, pos) {
            return None;
        }
        self.level
            .block_property(self.host, pos, "open")
            .and_then(|v| match v.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            })
    }
}

fn ensure_world_initialized(host: &dyn ServerHost, state: &mut RiftState) {
    let epoch = host.level(OVERWORLD).map(|l| l.epoch());
    let dir = host.world_dir();
    let needs_init = match (state.world_epoch, epoch) {
        (Some(prev), Some(cur)) => prev != cur,
        (None, Some(_)) => true,
        _ => dir.is_some() && state.world_dir != dir,
    };
    if needs_init {
        let seed = host.level(OVERWORLD).and_then(|l| l.seed(host));
        state.reset_for_world(dir, seed);
        state.world_epoch = epoch;
        state.load_from_host(host);
    }
}

fn broadcast_overworld(host: &mut dyn ServerHost, msg: &str) {
    let text = Text::literal(msg);
    for player in host.players().server_players() {
        if player
            .dimension(host)
            .is_some_and(|d| d.to_string() == OVERWORLD)
        {
            let _ = player.tell(host, text.clone());
        }
    }
}

fn release_all_forced_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    for chunk in state.core_chunks.drain() {
        if let Some(cref) = level.chunk_ref(host, chunk) {
            let _ = cref.set_forced(host, false);
        }
    }
    while let Some(chunk) = state.frontier_chunks.pop_front() {
        if let Some(cref) = level.chunk_ref(host, chunk) {
            let _ = cref.set_forced(host, false);
        }
    }
    while let Some(chunk) = state.regress_chunks.pop_front() {
        if let Some(cref) = level.chunk_ref(host, chunk) {
            let _ = cref.set_forced(host, false);
        }
    }
    bastion::release_bastion_chunks(host, level, state);
    outposts::release_outpost_chunks(host, level, state);
    citadel::release_citadel_chunks(host, level, state);
    nether_fob::release_nether_fob_chunks(host, state);
}

fn push_unique_chunk(dst: &mut Vec<[i32; 2]>, chunk: [i32; 2], core: &HashSet<[i32; 2]>) -> bool {
    if core.contains(&chunk) || dst.contains(&chunk) {
        return false;
    }
    dst.push(chunk);
    true
}

/// Computes bounded core + round-robin frontier/deep-bore forced chunks across ALL active
/// spreading Rifts so remote portals far from any player keep corrupting both surface terrain
/// and deep stone all the way down to Bedrock cleanly and evenly.
pub(crate) fn collect_desired_forced_chunks(state: &RiftState) -> (HashSet<[i32; 2]>, Vec<[i32; 2]>) {
    let mut desired_core: HashSet<[i32; 2]> = HashSet::new();
    let mut per_rift_candidates: Vec<Vec<[i32; 2]>> = Vec::new();

    for rift in &state.rifts {
        if !rift.is_actively_spreading() {
            continue;
        }
        if desired_core.len() < MAX_CORE_FORCED_CHUNKS {
            desired_core.insert([rift.center[0] >> 4, rift.center[2] >> 4]);
        }

        let mut cands = Vec::new();
        if let Some(([x, z], _, _)) = rift.active_column {
            cands.push([x >> 4, z >> 4]);
        }
        let mut added_surf = 0usize;
        for col in rift.ring_columns.iter().rev().take(64) {
            let c = [col[0] >> 4, col[1] >> 4];
            if !cands.contains(&c) {
                cands.push(c);
                added_surf += 1;
                if added_surf >= 4 {
                    break;
                }
            }
        }
        if let Some(([x, z], _, _)) = state.active_deepen_columns.get(&rift.id).copied() {
            let c = [x >> 4, z >> 4];
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
        if let Some(([x, z], _, _, _)) = state.active_priority_deepen.get(&rift.id).copied() {
            let c = [x >> 4, z >> 4];
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
        if let Some(q) = state.priority_deepen_cols.get(&rift.id) {
            let mut added_prio = 0usize;
            for (col, _, _, _) in q.iter().take(24) {
                let c = [col[0] >> 4, col[1] >> 4];
                if !cands.contains(&c) {
                    cands.push(c);
                    added_prio += 1;
                    if added_prio >= 2 {
                        break;
                    }
                }
            }
        }
        let mut added_deep = 0usize;
        for col in rift.deepen_columns.iter().rev().take(96) {
            if state.session_deepened_cols.contains(col) {
                continue;
            }
            let c = [col[0] >> 4, col[1] >> 4];
            if !cands.contains(&c) {
                cands.push(c);
                added_deep += 1;
                if added_deep >= 3 {
                    break;
                }
            }
        }
        per_rift_candidates.push(cands);
    }

    let mut desired_frontier: Vec<[i32; 2]> = Vec::new();
    let mut layer = 0usize;
    loop {
        if desired_frontier.len() >= MAX_FRONTIER_FORCED_CHUNKS {
            break;
        }
        let mut progressed = false;
        for list in &per_rift_candidates {
            if let Some(&chunk) = list.get(layer) {
                progressed = true;
                push_unique_chunk(&mut desired_frontier, chunk, &desired_core);
                if desired_frontier.len() >= MAX_FRONTIER_FORCED_CHUNKS {
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
        layer += 1;
    }

    (desired_core, desired_frontier)
}

fn sync_forced_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    state.needs_forced_chunk_sync = false;
    if state.paused || !state.has_uncontained() {
        for chunk in state.core_chunks.drain() {
            if let Some(cref) = level.chunk_ref(host, chunk) {
                let _ = cref.set_forced(host, false);
            }
        }
        while let Some(chunk) = state.frontier_chunks.pop_front() {
            if let Some(cref) = level.chunk_ref(host, chunk) {
                let _ = cref.set_forced(host, false);
            }
        }
        return;
    }

    let (desired_core, desired_frontier) = collect_desired_forced_chunks(state);

    let to_drop: Vec<[i32; 2]> = state
        .core_chunks
        .iter()
        .copied()
        .filter(|c| !desired_core.contains(c))
        .collect();
    for chunk in to_drop {
        state.core_chunks.remove(&chunk);
        if let Some(cref) = level.chunk_ref(host, chunk) {
            let _ = cref.set_forced(host, false);
        }
    }
    for chunk in desired_core {
        if !state.core_chunks.contains(&chunk)
            && let Some(cref) = level.chunk_ref(host, chunk)
            && cref.set_forced(host, true)
        {
            state.core_chunks.insert(chunk);
        }
    }

    for chunk in desired_frontier {
        if state.core_chunks.contains(&chunk) || state.frontier_chunks.contains(&chunk) {
            continue;
        }
        while state.frontier_chunks.len() >= MAX_FRONTIER_FORCED_CHUNKS {
            if let Some(old) = state.frontier_chunks.pop_front()
                && let Some(cref) = level.chunk_ref(host, old)
            {
                let _ = cref.set_forced(host, false);
            }
        }
        if let Some(cref) = level.chunk_ref(host, chunk)
            && cref.set_forced(host, true)
        {
            state.frontier_chunks.push_back(chunk);
        }
    }
}

fn register_or_refresh_rift(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState, seed_pos: [i32; 3]) {
    let view = ServerContainmentView { host, level };
    let (lit, seeds, centroid) = inspect_portal_cluster(&view, seed_pos);

    if let Some(rift) = state.find_near_source_mut(centroid, 12) {
        rift.center = centroid;
        rift.active_portal = lit;
        if rift.citadel_spawned && !rift.pacified {
            rift.contained = false;
        } else if !rift.pacified {
            let outcome = evaluate_chamber(&view, &seeds);
            if outcome != ContainmentOutcome::Indeterminate {
                rift.contained = outcome == ContainmentOutcome::Contained;
            }
        }
        state.dirty = true;
        return;
    }

    let id = state.next_id;
    state.next_id += 1;
    let flavor = NetherFlavor::pick_for_region(centroid[0], centroid[2], state.seed ^ id);
    let outcome = evaluate_chamber(&view, &seeds);
    let contained = outcome == ContainmentOutcome::Contained;

    let rift = RiftSource {
        id,
        center: centroid,
        active_portal: lit,
        contained,
        flavor,
        age_ticks: 0,
        corrupted_blocks: 0,
        max_radius_sq: 1,
        ring_radius: 1,
        ring_columns: build_ring_columns(centroid[0], centroid[2], 1),
        active_column: None,
        deepen_radius: 1,
        deepen_columns: build_ring_columns(centroid[0], centroid[2], 1),
        max_deepen_reached: 1,
        bastions_placed: 0,
        outposts_placed: 0,
        citadel_spawned: false,
        boss_uuid: None,
        pacified: false,
        regressing: false,
        regress_radius: 0,
        regress_columns: Vec::new(),
        regress_active_column: None,
    };
    state.rifts.push(rift);
    state.needs_forced_chunk_sync = true;
    if state.mark_quart_corrupted(centroid) {
        paint_biome_column(level, host, centroid[0], centroid[2], flavor);
    }
    state.dirty = true;

    if contained {
        broadcast_overworld(
            host,
            &format!(
                "[Rift Bleed] A sealed portal at ({}, {}, {}) is held inside Crying Obsidian.",
                centroid[0], centroid[1], centroid[2]
            ),
        );
    } else {
        broadcast_overworld(
            host,
            &format!(
                "[Rift Bleed] A {} rift tore open at ({}, {}, {})! Enclose all 6 sides with Crying Obsidian + Iron Doors to stop the spread.",
                flavor.display_name(),
                centroid[0],
                centroid[1],
                centroid[2]
            ),
        );
    }
}

fn evaluate_rift_containment(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState, force_all: bool) {
    let periodic = state.tick % 40 == 0;
    let queued = std::mem::take(&mut state.pending_containment_checks);
    if !force_all && !periodic && queued.is_empty() {
        return;
    }

    let mut announcements: Vec<String> = Vec::new();
    let view = ServerContainmentView { host, level };

    for idx in 0..state.rifts.len() {
        if state.rifts[idx].pacified {
            continue;
        }
        let queued_near = queued
            .iter()
            .any(|p| dist_sq(*p, state.rifts[idx].center) <= 22 * 22);
        let check_this = if state.rifts[idx].citadel_spawned {
            force_all || queued_near || (periodic && state.tick % 200 == 0)
        } else {
            force_all || periodic || queued_near
        };
        if !check_this {
            continue;
        }
        let center = state.rifts[idx].center;
        let (lit, seeds, centroid) = inspect_portal_cluster(&view, center);
        let outcome = evaluate_chamber(&view, &seeds);

        let rift = &mut state.rifts[idx];
        rift.active_portal = lit;
        if lit {
            rift.center = centroid;
        }
        // Once the Obsidian Breach Citadel has awoken and while its Wither Sovereign lives,
        // Crying Obsidian quarantining cannot stop corruption — only slaying the Wither can.
        if rift.citadel_spawned && !rift.pacified {
            if rift.contained {
                rift.contained = false;
                state.dirty = true;
            }
            let seal_attempted = outcome == ContainmentOutcome::Contained
                && (force_all
                    || queued
                        .iter()
                        .any(|p| dist_sq(*p, rift.center) <= 22 * 22));
            if seal_attempted {
                announcements.push(format!(
                    "[Rift Bleed] Crying Obsidian cannot seal Rift #{} — defeat the Wither Sovereign first!",
                    rift.id
                ));
            }
            continue;
        }
        match outcome {
            ContainmentOutcome::Contained => {
                if !rift.contained {
                    rift.contained = true;
                    state.dirty = true;
                    announcements.push(format!(
                        "[Rift Bleed] Rift #{} at ({}, {}, {}) is SEALED by Crying Obsidian.",
                        rift.id, rift.center[0], rift.center[1], rift.center[2]
                    ));
                }
            }
            ContainmentOutcome::Breached => {
                if rift.contained {
                    rift.contained = false;
                    state.dirty = true;
                    announcements.push(format!(
                        "[Rift Bleed] Containment BREACHED at Rift #{} ({}, {}, {}) — Nether corruption resumes!",
                        rift.id, rift.center[0], rift.center[1], rift.center[2]
                    ));
                }
            }
            ContainmentOutcome::Indeterminate => {}
        }
    }

    for line in announcements {
        broadcast_overworld(host, &line);
    }
}

pub fn near_corruption_or_active_rift(state: &RiftState, pos: [i32; 3]) -> bool {
    if state.is_quart_corrupted(pos) {
        return true;
    }
    for rift in &state.rifts {
        if !rift.is_actively_spreading() {
            continue;
        }
        let limit = (rift.max_radius_sq + 256).max(16 * 16);
        if horiz_dist_sq(rift.center, pos) <= limit {
            return true;
        }
    }
    false
}

/// Depth inside the active unpacified rift frontier where deep ocean water quenches into a
/// solid volcanic basalt seawall rather than boiling out into a vertical hovering water wall.
const WATER_SEAWALL_RIM_DEPTH: f64 = 3.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaterCorruptAction {
    EvaporateAir,
    QuenchVolcanicBerm(&'static str),
    MoltenLake(&'static str),
}

fn best_active_rift_edge_dist(state: &RiftState, x: i32, z: i32) -> Option<f64> {
    let mut best: Option<f64> = None;
    for rift in &state.rifts {
        if rift.pacified || (!rift.is_actively_spreading() && rift.ring_radius <= 0) {
            continue;
        }
        let dx = f64::from(x - rift.center[0]);
        let dz = f64::from(z - rift.center[2]);
        let edge = f64::from(rift.ring_radius) - dx.hypot(dz);
        best = Some(best.map_or(edge, |b| b.max(edge)));
    }
    best
}

pub fn is_within_evap_basin(state: &RiftState, pos: [i32; 3]) -> bool {
    state.is_quart_corrupted(pos)
        || best_active_rift_edge_dist(state, pos[0], pos[2]).is_some_and(|e| e >= -0.5)
}

fn near_monument_footprint(state: &RiftState, x: i32, z: i32) -> bool {
    state
        .citadels
        .iter()
        .any(|c| (x - c[0]).abs() <= 34 && (z - c[2]).abs() <= 34)
        || state
            .bastions
            .iter()
            .any(|b| (x - b.center[0]).abs() <= 36 && (z - b.center[2]).abs() <= 36)
        || state
            .outposts
            .iter()
            .any(|o| (x - o.center[0]).abs() <= 28 && (z - o.center[2]).abs() <= 28)
}

fn pick_volcanic_berm_block(pos: [i32; 3]) -> &'static str {
    match spatial_hash(pos[0], pos[1], pos[2], 0x5EA1_BA5A) % 16 {
        0..=6 => "minecraft:obsidian",
        7..=10 => "minecraft:basalt",
        11..=13 => "minecraft:blackstone",
        _ => "minecraft:magma_block",
    }
}

fn is_volcanic_berm_block(name: &str) -> bool {
    matches!(
        strip_ns(name),
        "obsidian" | "basalt" | "smooth_basalt" | "blackstone" | "magma_block"
    )
}

fn pick_lava_obsidian_lake_block(pos: [i32; 3], is_surface: bool) -> &'static str {
    if is_surface {
        // Organic 2x2 cooled obsidian crusts & magma islands floating across molten lava lakes.
        let pocket = spatial_hash(pos[0] >> 1, 17, pos[2] >> 1, 0x0B51_DA41) % 100;
        let fine = spatial_hash(pos[0], pos[1], pos[2], 0x0B51_DA42) % 100;
        if pocket < 34 && fine < 62 {
            return match fine % 10 {
                0..=6 => "minecraft:obsidian",
                7..=8 => "minecraft:magma_block",
                _ => "minecraft:blackstone",
            };
        }
        "minecraft:lava"
    } else if spatial_hash(pos[0], pos[1], pos[2], 0x0B51_DA43) % 12 == 0 {
        "minecraft:obsidian"
    } else {
        "minecraft:lava"
    }
}

fn classify_water_corruption(
    pos: [i32; 3],
    edge_dist: Option<f64>,
    is_deep_water: bool,
    touches_frontier_live_water: bool,
    is_surface_water: bool,
    is_lake_top: bool,
) -> WaterCorruptAction {
    if !is_surface_water && !is_deep_water && !touches_frontier_live_water {
        return WaterCorruptAction::EvaporateAir;
    }
    if touches_frontier_live_water {
        return WaterCorruptAction::QuenchVolcanicBerm(pick_volcanic_berm_block(pos));
    }
    if is_deep_water && edge_dist.is_none_or(|d| d <= WATER_SEAWALL_RIM_DEPTH) {
        return WaterCorruptAction::QuenchVolcanicBerm(pick_volcanic_berm_block(pos));
    }
    if is_surface_water || is_deep_water {
        return WaterCorruptAction::MoltenLake(pick_lava_obsidian_lake_block(pos, is_lake_top));
    }
    WaterCorruptAction::EvaporateAir
}

fn column_water_span(host: &dyn ServerHost, level: LevelRef, pos: [i32; 3], min_needed: usize) -> usize {
    let mut span = 0usize;
    let is_aqua_or_berm = |id: &str| is_water_like(id) || is_volcanic_berm_block(id);
    if !level
        .block(host, pos)
        .is_some_and(|k| is_aqua_or_berm(&k.id(host)))
    {
        return 0;
    }
    span += 1;
    for dy in 1..=3 {
        if pos[1] - dy < OVERWORLD_MIN_Y {
            break;
        }
        let p = [pos[0], pos[1] - dy, pos[2]];
        if !level.is_loaded(host, p)
            || !level.block(host, p).is_some_and(|k| is_aqua_or_berm(&k.id(host)))
        {
            break;
        }
        span += 1;
        if span >= min_needed {
            return span;
        }
    }
    for dy in 1..=3 {
        if pos[1] + dy > OVERWORLD_MAX_Y {
            break;
        }
        let p = [pos[0], pos[1] + dy, pos[2]];
        if !level.is_loaded(host, p)
            || !level.block(host, p).is_some_and(|k| is_aqua_or_berm(&k.id(host)))
        {
            break;
        }
        span += 1;
        if span >= min_needed {
            return span;
        }
    }
    span
}

fn decide_water_corruption(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &RiftState,
    pos: [i32; 3],
) -> WaterCorruptAction {
    let edge = best_active_rift_edge_dist(state, pos[0], pos[2]);
    let local_span = column_water_span(host, level, pos, 3);
    let mut is_deep = local_span >= 3;

    let mut touches_frontier_live = false;
    let horiz = [
        [pos[0] + 1, pos[1], pos[2]],
        [pos[0] - 1, pos[1], pos[2]],
        [pos[0], pos[1], pos[2] + 1],
        [pos[0], pos[1], pos[2] - 1],
    ];
    for n in horiz {
        if !level.is_loaded(host, n) {
            touches_frontier_live = true;
            continue;
        }
        let Some(nk) = level.block(host, n) else {
            continue;
        };
        if !is_water_like(&nk.id(host)) {
            continue;
        }
        let n_edge = best_active_rift_edge_dist(state, n[0], n[2]);
        if n_edge.is_none_or(|ne| ne < WATER_SEAWALL_RIM_DEPTH) {
            let n_span = column_water_span(host, level, n, 3);
            if n_span >= 2 {
                touches_frontier_live = true;
                if n_span >= 3 {
                    is_deep = true;
                }
                break;
            }
        }
    }

    let mut tc_water_top: Option<i32> = None;
    if let Ok(tcs) = host.terrain_surface(OVERWORLD, &[(pos[0], pos[2])])
        && let Some(Some(tc)) = tcs.first()
        && let Some(w) = tc.water
    {
        tc_water_top = Some(w - 1);
        if w - (tc.height - 1) >= 3 {
            is_deep = true;
        }
    }

    let above_open = pos[1] >= OVERWORLD_MAX_Y
        || level
            .block(host, [pos[0], pos[1] + 1, pos[2]])
            .is_some_and(|k| {
                let id = k.id(host);
                is_air_like(&id) || is_snow_or_ice(&id)
            });
    let is_lake_top = above_open || tc_water_top.is_some_and(|top| pos[1] >= top);
    let is_surface_water = tc_water_top.is_some() || pos[1] >= 44;

    classify_water_corruption(
        pos,
        edge,
        is_deep,
        touches_frontier_live,
        is_surface_water,
        is_lake_top,
    )
}

/// Seals any existing vertical wall of water touching a dried corrupted seabed column (including
/// already-corrupted worlds loaded with the mod update).
fn plug_exposed_ocean_wall_at_col(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    x: i32,
    z: i32,
) {
    if near_monument_footprint(state, x, z) || best_active_rift_edge_dist(state, x, z).is_none() {
        return;
    }
    let probes_y = [62, 60, 56, 52];
    let mut hit_water = false;
    for py in probes_y {
        let p = [x, py, z];
        if !level.is_loaded(host, p)
            || !level.block(host, p).is_some_and(|k| is_air_like(&k.id(host)))
        {
            continue;
        }
        for [nx, nz] in [[x + 1, z], [x - 1, z], [x, z + 1], [x, z - 1]] {
            let np = [nx, py, nz];
            if level.is_loaded(host, np)
                && level.block(host, np).is_some_and(|k| is_water_like(&k.id(host)))
                && column_water_span(host, level, np, 3) >= 3
            {
                hit_water = true;
                break;
            }
        }
        if hit_water {
            break;
        }
    }
    if !hit_water {
        return;
    }

    let seabed_y = host
        .terrain_surface(OVERWORLD, &[(x, z)])
        .ok()
        .and_then(|v| v.first().and_then(|o| o.as_ref()).map(|c| c.height - 1))
        .unwrap_or(40)
        .clamp(OVERWORLD_MIN_Y + 4, OVERWORLD_MAX_Y - 4);
    let mut top_seal_y = seabed_y;
    for y in (seabed_y + 1..=64).rev() {
        let has_water_adj = [[x + 1, z], [x - 1, z], [x, z + 1], [x, z - 1]]
            .into_iter()
            .any(|[nx, nz]| {
                let np = [nx, y, nz];
                level.is_loaded(host, np)
                    && level.block(host, np).is_some_and(|k| is_water_like(&k.id(host)))
            });
        if has_water_adj {
            top_seal_y = y;
            break;
        }
    }
    if top_seal_y <= seabed_y {
        return;
    }

    let mut placed_any = false;
    for y in (seabed_y + 1)..=top_seal_y {
        let pos = [x, y, z];
        let is_open = level.block(host, pos).is_some_and(|k| {
            let id = k.id(host);
            is_air_like(&id) || is_water_like(&id)
        });
        if !is_open {
            continue;
        }
        if let Some(bs) = resolve_block_cached(&mut state.block_state_cache, host, pick_volcanic_berm_block(pos))
            && level
                .set_block(
                    host,
                    pos,
                    bs,
                    BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                )
                .unwrap_or(false)
        {
            placed_any = true;
        }
    }
    if placed_any {
        state.queue_shore_col([x, z]);
        state.queue_melt_fx([x, top_seal_y, z]);
        state.dirty = true;
    }
}

pub fn is_water_bucket_item(item_id: &str) -> bool {
    matches!(
        strip_ns(item_id),
        "water_bucket"
            | "cod_bucket"
            | "salmon_bucket"
            | "pufferfish_bucket"
            | "tropical_fish_bucket"
            | "axolotl_bucket"
            | "tadpole_bucket"
    )
}

/// Picks the main-hand hotbar slot first (if it holds a water bucket), or falls back to
/// offhand slot `40`, returning `(slot_index, stack_count)`.
pub fn select_water_bucket_slot(
    selected_hotbar_slot: Option<usize>,
    slots: &[(u32, &str, u32)],
) -> Option<(u32, u32)> {
    if let Some(sel) = selected_hotbar_slot.and_then(|s| u32::try_from(s).ok())
        && let Some((slot, _, count)) = slots
            .iter()
            .find(|(slot, id, count)| *slot == sel && *count > 0 && is_water_bucket_item(id))
    {
        return Some((*slot, *count));
    }
    slots
        .iter()
        .find(|(slot, id, count)| *slot == 40 && *count > 0 && is_water_bucket_item(id))
        .map(|(slot, _, count)| (*slot, *count))
}

fn emit_bucket_evap_fx(
    host: &mut dyn ServerHost,
    level: LevelRef,
    pos: [i32; 3],
    particles: Option<&ParticlePalette>,
) {
    let fx_pos = [
        f64::from(pos[0]) + 0.5,
        f64::from(pos[1]) + 0.65,
        f64::from(pos[2]) + 0.5,
    ];
    let top_pos = [
        f64::from(pos[0]) + 0.5,
        f64::from(pos[1]) + 0.92,
        f64::from(pos[2]) + 0.5,
    ];
    let _ = level.play_sound(
        host,
        "minecraft:block.fire.extinguish",
        fx_pos,
        SoundSource::Blocks,
        0.5,
        2.35,
    );
    if let Some(pal) = particles {
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(pal.large_smoke),
            None,
            fx_pos,
            [0.25, 0.22, 0.25],
            0.02,
            8,
        );
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(pal.smoke),
            None,
            top_pos,
            [0.20, 0.18, 0.20],
            0.025,
            5,
        );
    }
}

fn drain_bucket_evaporations(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    bucket_evap: Vec<(u128, [i32; 3])>,
    particles: Option<&ParticlePalette>,
) {
    if bucket_evap.is_empty() {
        return;
    }

    let empty_bucket_key = Identifier::parse("minecraft:bucket")
        .ok()
        .and_then(|id| host.item_key(&id));

    for (player_uuid, pos) in bucket_evap {
        if pos[1] >= OVERWORLD_MIN_Y && pos[1] <= OVERWORLD_MAX_Y && level.is_loaded(host, pos) {
            emit_bucket_evap_fx(host, level, pos, particles);
            state.queue_water_evap(pos);
        }

        let Some(bucket_key) = empty_bucket_key else {
            continue;
        };
        let Some(player) = host
            .players()
            .server_players()
            .into_iter()
            .find(|p| p.uuid() == player_uuid)
        else {
            continue;
        };
        if player.is_creative(host).unwrap_or(false) || player.is_spectator(host).unwrap_or(false) {
            continue;
        }
        let selected = player.selected_slot(host);
        let Some(raw_slots) = player.inventory_slots(host) else {
            continue;
        };
        let resolved: Vec<(u32, String, u32)> = raw_slots
            .iter()
            .map(|(s, st)| (*s, st.item().id(host), st.count()))
            .collect();
        let slot_views: Vec<(u32, &str, u32)> = resolved
            .iter()
            .map(|(s, id, c)| (*s, id.as_str(), *c))
            .collect();

        let Some((slot, count)) = select_water_bucket_slot(selected, &slot_views) else {
            continue;
        };

        if count <= 1 {
            let _ = player.set_inventory_slot(host, slot, Some(ItemStack::of(bucket_key)));
        } else if let Some(orig_key) = raw_slots
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, st)| st.item())
        {
            let _ = player.set_inventory_slot(host, slot, ItemStack::new(orig_key, count - 1));
            let _ = player.give_stack(host, ItemStack::of(bucket_key));
        }
    }
}

fn emit_melt_evap_fx(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    particles: Option<&ParticlePalette>,
) {
    if state.pending_melt_fx.is_empty() {
        return;
    }
    let positions: Vec<[i32; 3]> = state.pending_melt_fx.drain(..).collect();
    let max_fx = 6usize;
    let step = (positions.len() / max_fx).max(1);
    let mut sound_pos = None;

    for pos in positions.iter().step_by(step).take(max_fx) {
        if sound_pos.is_none() {
            sound_pos = Some(*pos);
        }
        if let Some(pal) = particles {
            let top_pos = [
                f64::from(pos[0]) + 0.5,
                f64::from(pos[1]) + 0.85,
                f64::from(pos[2]) + 0.5,
            ];
            let _ = level.send_particles(
                host,
                &ParticleOptions::simple(pal.large_smoke),
                None,
                top_pos,
                [0.24, 0.18, 0.24],
                0.02,
                3,
            );
            let _ = level.send_particles(
                host,
                &ParticleOptions::simple(pal.smoke),
                None,
                top_pos,
                [0.20, 0.16, 0.20],
                0.025,
                2,
            );
        }
    }

    if let Some(spos) = sound_pos
        && state.tick.saturating_sub(state.last_melt_sound_tick) >= 12
    {
        state.last_melt_sound_tick = state.tick;
        let _ = level.play_sound(
            host,
            "minecraft:block.fire.extinguish",
            [
                f64::from(spos[0]) + 0.5,
                f64::from(spos[1]) + 0.7,
                f64::from(spos[2]) + 0.5,
            ],
            SoundSource::Blocks,
            0.18,
            2.15,
        );
    }
}

fn drain_water_evaporation(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    let Some(air) = host.block_state_id("minecraft:air") else {
        return;
    };

    let mut processed = 0usize;
    while processed < 224 {
        let Some(pos) = state.pop_water_evap() else {
            break;
        };
        processed += 1;
        if pos[1] < OVERWORLD_MIN_Y || pos[1] > OVERWORLD_MAX_Y || !level.is_loaded(host, pos) {
            continue;
        }
        if !near_corruption_or_active_rift(state, pos) {
            continue;
        }

        let Some(bkey) = level.block(host, pos) else {
            continue;
        };
        let bname = bkey.id(host);
        let is_water = is_water_like(&bname);
        let is_cold = is_snow_or_ice(&bname);
        if is_water {
            if !is_within_evap_basin(state, pos) {
                continue;
            }
            match decide_water_corruption(host, level, state, pos) {
                WaterCorruptAction::QuenchVolcanicBerm(berm_id) => {
                    if let Some(berm_bs) =
                        resolve_block_cached(&mut state.block_state_cache, host, berm_id)
                        && level
                            .set_block(
                                host,
                                pos,
                                berm_bs,
                                BlockUpdateFlags::CLIENTS
                                    | BlockUpdateFlags::KNOWN_SHAPE
                                    | BlockUpdateFlags::SUPPRESS_DROPS,
                            )
                            .unwrap_or(false)
                    {
                        state.queue_melt_fx(pos);
                        state.queue_shore_col([pos[0], pos[2]]);
                        state.dirty = true;
                    }
                    continue;
                }
                WaterCorruptAction::MoltenLake(lake_id) => {
                    if let Some(lake_bs) =
                        resolve_block_cached(&mut state.block_state_cache, host, lake_id)
                        && level
                            .set_block(
                                host,
                                pos,
                                lake_bs,
                                BlockUpdateFlags::CLIENTS
                                    | BlockUpdateFlags::KNOWN_SHAPE
                                    | BlockUpdateFlags::SUPPRESS_DROPS,
                            )
                            .unwrap_or(false)
                    {
                        if lake_id != "minecraft:lava" {
                            state.queue_melt_fx(pos);
                        }
                        state.dirty = true;
                    }
                    for n in neighbors_6(pos) {
                        if n[1] >= OVERWORLD_MIN_Y
                            && n[1] <= OVERWORLD_MAX_Y
                            && is_within_evap_basin(state, n)
                            && level.block(host, n).is_some_and(|nk| {
                                let nid = nk.id(host);
                                is_water_like(&nid) || is_snow_or_ice(&nid)
                            })
                        {
                            state.queue_water_evap(n);
                        }
                    }
                }
                WaterCorruptAction::EvaporateAir => {
                    if level
                        .set_block(
                            host,
                            pos,
                            air,
                            BlockUpdateFlags::CLIENTS
                                | BlockUpdateFlags::KNOWN_SHAPE
                                | BlockUpdateFlags::SUPPRESS_DROPS,
                        )
                        .unwrap_or(false)
                    {
                        state.queue_melt_fx(pos);
                    }
                    state.dirty = true;
                    for n in neighbors_6(pos) {
                        if n[1] >= OVERWORLD_MIN_Y
                            && n[1] <= OVERWORLD_MAX_Y
                            && is_within_evap_basin(state, n)
                            && level.block(host, n).is_some_and(|nk| {
                                let nid = nk.id(host);
                                is_water_like(&nid) || is_snow_or_ice(&nid)
                            })
                        {
                            state.queue_water_evap(n);
                        }
                    }
                }
            }
        } else if is_cold {
            if level
                .set_block(
                    host,
                    pos,
                    air,
                    BlockUpdateFlags::CLIENTS
                        | BlockUpdateFlags::KNOWN_SHAPE
                        | BlockUpdateFlags::SUPPRESS_DROPS,
                )
                .unwrap_or(false)
            {
                state.queue_melt_fx(pos);
            }
            state.dirty = true;

            // Chain-melt adjacent and sloped snow/ice across mountain peaks and frozen lakes.
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if dx == 0 && dy == 0 && dz == 0 {
                            continue;
                        }
                        let n = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
                        if n[1] >= OVERWORLD_MIN_Y
                            && n[1] <= OVERWORLD_MAX_Y
                            && near_corruption_or_active_rift(state, n)
                            && level.block(host, n).is_some_and(|nk| {
                                let nid = nk.id(host);
                                is_snow_or_ice(&nid)
                                    || (is_water_like(&nid) && is_within_evap_basin(state, n))
                            })
                        {
                            state.queue_water_evap(n);
                        }
                    }
                }
            }
        } else if !is_never_waterlogged(&bname)
            && is_within_evap_basin(state, pos)
            && level.block_property(host, pos, "waterlogged").as_deref() == Some("true")
        {
            let _ = host.set_block_property(pos, "waterlogged", "false");
            state.queue_melt_fx(pos);
            state.dirty = true;
        }
    }

    // Gentle throttled sweep over existing corrupted quart columns (no per-tick Vec clone)
    // so lingering ponds, snow, or ice patches never survive while exposed ocean faces seal cleanly.
    if state.tick % 4 != 0 || state.corrupted_quart_cols_list.is_empty() {
        return;
    }
    for _ in 0..2 {
        let len = state.corrupted_quart_cols_list.len();
        let idx = state.water_sweep_cursor % len;
        state.water_sweep_cursor = state.water_sweep_cursor.wrapping_add(1);
        let qc = state.corrupted_quart_cols_list[idx];
        let sub_x = (qc[0] << 2) + ((state.tick as i32) & 3);
        let sub_z = (qc[1] << 2) + (((state.tick >> 2) as i32) & 3);
        if !level.is_loaded(host, [sub_x, 64, sub_z]) {
            continue;
        }
        let ws = host
            .surface_heights(OVERWORLD, &[(sub_x, sub_z)], Heightmap::WorldSurface)
            .ok()
            .and_then(|hs| hs.first().copied().flatten());
        let mb = host
            .surface_heights(OVERWORLD, &[(sub_x, sub_z)], Heightmap::MotionBlockingNoLeaves)
            .ok()
            .and_then(|hs| hs.first().copied().flatten());

        let mut scan_y_ranges: [Option<(i32, i32)>; 2] = [None, None];
        if let Some(top) = ws {
            scan_y_ranges[0] = Some(((top - 8).max(OVERWORLD_MIN_Y), (top + 1).min(OVERWORLD_MAX_Y)));
        }
        if let Some(ground) = mb {
            scan_y_ranges[1] = Some((
                (ground - 6).max(OVERWORLD_MIN_Y),
                (ground + 4).min(OVERWORLD_MAX_Y),
            ));
        }

        for range in scan_y_ranges.into_iter().flatten() {
            for y in range.0..=range.1 {
                let p = [sub_x, y, sub_z];
                let Some(k) = level.block(host, p) else {
                    continue;
                };
                let id = k.id(host);
                if is_water_like(&id)
                    || is_snow_or_ice(&id)
                    || (!is_never_waterlogged(&id)
                        && level.block_property(host, p, "waterlogged").as_deref() == Some("true"))
                {
                    state.queue_water_evap(p);
                }
            }
        }

        if mb.is_some_and(|g| g < 62) {
            plug_exposed_ocean_wall_at_col(host, level, state, sub_x, sub_z);
        }
        // Upgrade previously drained ocean/river basins in older saves into Lava+Obsidian lakes.
        if best_active_rift_edge_dist(state, sub_x, sub_z)
            .is_some_and(|d| d > WATER_SEAWALL_RIM_DEPTH + 1.0)
            && !near_monument_footprint(state, sub_x, sub_z)
            && let Ok(tcs) = host.terrain_surface(OVERWORLD, &[(sub_x, sub_z)])
            && let Some(Some(tc)) = tcs.first()
            && let Some(w) = tc.water
        {
            let sea_top = (w - 1).clamp(OVERWORLD_MIN_Y + 4, OVERWORLD_MAX_Y - 2);
            if sea_top > tc.height - 1
                && level
                    .block(host, [sub_x, sea_top, sub_z])
                    .is_some_and(|k| is_air_like(&k.id(host)) || is_volcanic_berm_block(&k.id(host)))
            {
                state.queue_shore_col([sub_x, sub_z]);
            }
        }
    }
}

/// Converts interior ocean & river columns (safely behind the Obsidian/Basalt coastal dam)
/// cleanly into molten Lava + Obsidian lakes at sea/river level without creating water walls
/// or cobblestone collisions.
fn step_ocean_shores(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if state.paused || state.shore_columns.is_empty() {
        return;
    }
    let Some(air) = resolve_block_cached(&mut state.block_state_cache, host, "minecraft:air") else {
        return;
    };

    let max_rotations = state.shore_columns.len().min(36);
    let mut converted_ops = 0usize;
    let mut rotated = 0usize;

    while converted_ops < 12 && rotated < max_rotations {
        let Some(col) = state.pop_shore_col() else {
            break;
        };
        rotated += 1;

        let Some(edge_dist) = best_active_rift_edge_dist(state, col[0], col[1]) else {
            state.dirty = true;
            continue;
        };
        if !level.is_loaded(host, [col[0], 64, col[1]]) || edge_dist <= WATER_SEAWALL_RIM_DEPTH {
            state.queue_shore_col(col);
            continue;
        }
        if near_monument_footprint(state, col[0], col[1]) {
            state.dirty = true;
            continue;
        }

        converted_ops += 1;
        let Ok(tcs) = host.terrain_surface(OVERWORLD, &[(col[0], col[1])]) else {
            state.queue_shore_col(col);
            continue;
        };
        let Some(Some(tc)) = tcs.first() else {
            continue;
        };
        let seabed_y = (tc.height - 1).clamp(OVERWORLD_MIN_Y + 4, OVERWORLD_MAX_Y - 4);
        let sea_top_y = tc
            .water
            .map(|w| (w - 1).clamp(seabed_y, OVERWORLD_MAX_Y - 2))
            .unwrap_or(seabed_y);
        if sea_top_y <= seabed_y {
            continue;
        }

        let ws_top = host
            .surface_heights(OVERWORLD, &[(col[0], col[1])], Heightmap::WorldSurface)
            .ok()
            .and_then(|hs| hs.first().copied().flatten())
            .unwrap_or(sea_top_y + 1)
            .clamp(seabed_y, OVERWORLD_MAX_Y - 2);
        let scan_top = sea_top_y.max(ws_top);

        let mut max_neighbor_water_y: Option<i32> = None;
        let horiz = [
            [col[0] + 1, col[1]],
            [col[0] - 1, col[1]],
            [col[0], col[1] + 1],
            [col[0], col[1] - 1],
        ];
        for [nx, nz] in horiz {
            if !level.is_loaded(host, [nx, 64, nz]) {
                max_neighbor_water_y = Some(sea_top_y);
                break;
            }
            for y in (seabed_y + 1..=sea_top_y).rev() {
                if level
                    .block(host, [nx, y, nz])
                    .is_some_and(|k| is_water_like(&k.id(host)))
                {
                    max_neighbor_water_y =
                        Some(max_neighbor_water_y.map_or(y, |cur| cur.max(y)));
                    if best_active_rift_edge_dist(state, nx, nz)
                        .is_some_and(|ne| ne > WATER_SEAWALL_RIM_DEPTH + 0.5)
                    {
                        state.queue_water_evap([nx, y, nz]);
                    }
                    break;
                }
            }
        }

        if let Some(wy) = max_neighbor_water_y {
            // Still touching live uncorrupted water on a neighbor: hold solid Obsidian/Basalt dam!
            for y in (seabed_y + 1)..=wy {
                let p = [col[0], y, col[1]];
                let is_open = level.block(host, p).is_some_and(|k| {
                    let id = k.id(host);
                    is_air_like(&id) || is_water_like(&id)
                });
                if is_open
                    && let Some(bs) =
                        resolve_block_cached(&mut state.block_state_cache, host, pick_volcanic_berm_block(p))
                    && level
                        .set_block(
                            host,
                            p,
                            bs,
                            BlockUpdateFlags::CLIENTS
                                | BlockUpdateFlags::KNOWN_SHAPE
                                | BlockUpdateFlags::SUPPRESS_DROPS,
                        )
                        .unwrap_or(false)
                {
                    state.dirty = true;
                }
            }
            state.queue_shore_col(col);
            continue;
        }

        // Safely separated from all live ocean/river water: convert dam & water basin into a
        // level Lava + cooled Obsidian crust lake from seabed_y + 1 up to sea_top_y.
        let mut changed_any = false;
        for y in (seabed_y + 1)..=sea_top_y {
            let p = [col[0], y, col[1]];
            let eligible = level.block(host, p).is_some_and(|k| {
                let id = k.id(host);
                is_volcanic_berm_block(&id)
                    || is_water_like(&id)
                    || is_snow_or_ice(&id)
                    || is_air_like(&id)
            });
            if eligible {
                let target_id = pick_lava_obsidian_lake_block(p, y == sea_top_y);
                if let Some(bs) = resolve_block_cached(&mut state.block_state_cache, host, target_id)
                    && level
                        .set_block(
                            host,
                            p,
                            bs,
                            BlockUpdateFlags::CLIENTS
                                | BlockUpdateFlags::KNOWN_SHAPE
                                | BlockUpdateFlags::SUPPRESS_DROPS,
                        )
                        .unwrap_or(false)
                {
                    changed_any = true;
                }
            }
        }
        for y in (sea_top_y + 1)..=scan_top {
            let p = [col[0], y, col[1]];
            if level.block(host, p).is_some_and(|k| {
                let id = k.id(host);
                is_volcanic_berm_block(&id) || is_water_like(&id) || is_snow_or_ice(&id)
            }) && level
                .set_block(
                    host,
                    p,
                    air,
                    BlockUpdateFlags::CLIENTS
                        | BlockUpdateFlags::KNOWN_SHAPE
                        | BlockUpdateFlags::SUPPRESS_DROPS,
                )
                .unwrap_or(false)
            {
                changed_any = true;
            }
        }
        if changed_any {
            state.dirty = true;
        }
    }
}

fn resolve_block_cached(
    cache: &mut HashMap<&'static str, mod_api::BlockState>,
    host: &dyn ServerHost,
    id: &'static str,
) -> Option<mod_api::BlockState> {
    if let Some(&bs) = cache.get(id) {
        return Some(bs);
    }
    let bs = host
        .block_state_id(id)
        .or_else(|| host.block_key(id).and_then(|k| host.block_state_default(k)))?;
    cache.insert(id, bs);
    Some(bs)
}

fn is_never_waterlogged(id: &str) -> bool {
    let s = strip_ns(id);
    matches!(
        s,
        "air"
            | "cave_air"
            | "void_air"
            | "water"
            | "flowing_water"
            | "lava"
            | "flowing_lava"
            | "stone"
            | "deepslate"
            | "granite"
            | "diorite"
            | "andesite"
            | "tuff"
            | "calcite"
            | "dirt"
            | "coarse_dirt"
            | "rooted_dirt"
            | "sand"
            | "red_sand"
            | "gravel"
            | "grass_block"
            | "podzol"
            | "mycelium"
            | "moss_block"
            | "pale_moss_block"
            | "netherrack"
            | "crimson_nylium"
            | "warped_nylium"
            | "blackstone"
            | "basalt"
            | "smooth_basalt"
            | "magma_block"
            | "soul_sand"
            | "soul_soil"
            | "bedrock"
            | "obsidian"
            | "crying_obsidian"
            | "ancient_debris"
            | "sandstone"
            | "red_sandstone"
            | "dripstone_block"
            | "sculk"
            | "amethyst_block"
            | "budding_amethyst"
            | "raw_iron_block"
            | "raw_copper_block"
    ) || s.ends_with("_ore")
        || s.ends_with("_log")
        || s.ends_with("_wood")
        || s.ends_with("_leaves")
}

fn pop_tail_loaded_column(
    host: &dyn ServerHost,
    level: LevelRef,
    cols: &mut Vec<[i32; 2]>,
    needs_sync: &mut bool,
) -> Option<[i32; 2]> {
    let tail_start = cols.len().saturating_sub(24);
    for idx in (tail_start..cols.len()).rev() {
        let c = cols[idx];
        if level.is_loaded(host, [c[0], 64, c[1]]) {
            return Some(cols.swap_remove(idx));
        }
        *needs_sync = true;
    }
    None
}

fn pop_next_loaded_column(
    host: &dyn ServerHost,
    level: LevelRef,
    cols: &mut Vec<[i32; 2]>,
    radius: &mut i32,
    center: [i32; 3],
    needs_sync: &mut bool,
) -> Option<[i32; 2]> {
    for _ in 0..6 {
        if cols.is_empty() {
            *radius += 1;
            *cols = build_ring_columns(center[0], center[2], *radius);
            *needs_sync = true;
        }
        if let Some(col) = pop_tail_loaded_column(host, level, cols, needs_sync) {
            return Some(col);
        }
        if !cols.is_empty() {
            break;
        }
    }
    None
}

fn query_surface_and_cliff_heights(
    host: &mut dyn ServerHost,
    col: [i32; 2],
    fallback_y: i32,
) -> (i32, i32, i32) {
    let fallback = (fallback_y + 24).clamp(OVERWORLD_MIN_Y + 8, OVERWORLD_MAX_Y - 3);
    let world_surface = host
        .surface_heights(OVERWORLD, &[(col[0], col[1])], Heightmap::WorldSurface)
        .ok()
        .and_then(|hs| hs.first().copied().flatten())
        .unwrap_or(fallback);
    let probes = [
        (col[0], col[1]),
        (col[0] + 1, col[1]),
        (col[0] - 1, col[1]),
        (col[0], col[1] + 1),
        (col[0], col[1] - 1),
    ];
    let floors = host
        .surface_heights(OVERWORLD, &probes, Heightmap::OceanFloor)
        .unwrap_or_default();
    let ocean_floor = floors
        .first()
        .copied()
        .flatten()
        .unwrap_or(world_surface);
    let min_neighbor_floor = floors
        .iter()
        .skip(1)
        .filter_map(|h| *h)
        .min()
        .unwrap_or(ocean_floor);
    (world_surface, ocean_floor, min_neighbor_floor)
}

fn portal_is_underground(host: &mut dyn ServerHost, rift_center: [i32; 3]) -> bool {
    let portal_floor = host
        .surface_heights(
            OVERWORLD,
            &[(rift_center[0], rift_center[2])],
            Heightmap::OceanFloor,
        )
        .ok()
        .and_then(|hs| hs.first().copied().flatten());
    portal_floor.is_some_and(|ground| rift_center[1] < ground - 14)
}

/// Applies one block corruption step at `pos` and returns `true` if a block changed.
fn capture_structural_properties(
    host: &dyn ServerHost,
    level: LevelRef,
    pos: [i32; 3],
    current: &str,
) -> Vec<(&'static str, String)> {
    let short = strip_ns(current);
    let keys: &[&str] = if short.ends_with("_stairs") {
        &["facing", "half", "shape"]
    } else if short.ends_with("_slab") {
        &["type"]
    } else if short.ends_with("_door") {
        &["facing", "half", "open", "hinge"]
    } else if short.ends_with("_trapdoor") {
        &["facing", "half", "open"]
    } else if short.ends_with("_fence_gate") {
        &["facing", "open"]
    } else if short.ends_with("_log") || short.ends_with("_wood") {
        &["axis"]
    } else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(keys.len());
    for &key in keys {
        if let Some(val) = level.block_property(host, pos, key) {
            out.push((key, val));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn apply_corruption_at(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_idx: usize,
    rift_id: u64,
    rift_center: [i32; 3],
    rift_flavor: NetherFlavor,
    mature: bool,
    pos: [i32; 3],
    current: &str,
) -> bool {
    let y = pos[1];
    let above = if y + 1 <= OVERWORLD_MAX_Y {
        level
            .block(host, [pos[0], y + 1, pos[2]])
            .map(|k| k.id(host))
            .unwrap_or_else(|| "minecraft:air".to_owned())
    } else {
        "minecraft:air".to_owned()
    };
    let below = if y - 1 >= OVERWORLD_MIN_Y {
        level
            .block(host, [pos[0], y - 1, pos[2]])
            .map(|k| k.id(host))
            .unwrap_or_else(|| "minecraft:air".to_owned())
    } else {
        "minecraft:bedrock".to_owned()
    };

    let d2 = horiz_dist_sq(rift_center, pos);
    let local_flavor = if d2 <= 40 * 40 {
        rift_flavor
    } else {
        NetherFlavor::pick_for_region(pos[0], pos[2], state.seed ^ rift_id)
    };

    let check_waterlogged = !is_never_waterlogged(current);
    let was_waterlogged = check_waterlogged
        && level.block_property(host, pos, "waterlogged").as_deref() == Some("true");
    if was_waterlogged {
        let _ = host.set_block_property(pos, "waterlogged", "false");
        state.queue_water_evap(pos);
        state.queue_melt_fx(pos);
    }

    let mut out = transform_block(pos, current, &above, &below, local_flavor, mature);
    let preserved_props = if out.block.is_some_and(|t| t != "minecraft:air") {
        capture_structural_properties(host, level, pos, current)
    } else {
        Vec::new()
    };

    let is_water_or_frozen = is_water_like(current) || is_snow_or_ice(current);
    let mut quenched_berm = false;
    let frozen_over_water = is_snow_or_ice(current)
        && (is_water_like(&below)
            || host
                .terrain_surface(OVERWORLD, &[(pos[0], pos[2])])
                .is_ok_and(|tcs| {
                    tcs.first()
                        .and_then(|o| o.as_ref())
                        .is_some_and(|tc| tc.water.is_some_and(|w| pos[1] < w))
                }));
    if is_water_like(current) || frozen_over_water {
        match decide_water_corruption(host, level, state, pos) {
            WaterCorruptAction::QuenchVolcanicBerm(berm_id) => {
                out.block = Some(berm_id);
                out.above = None;
                out.below = None;
                out.is_corrupt = true;
                quenched_berm = true;
            }
            WaterCorruptAction::MoltenLake(lake_id) => {
                out.block = Some(lake_id);
                out.above = None;
                out.below = None;
                out.is_corrupt = true;
                state.queue_water_evap(pos);
            }
            WaterCorruptAction::EvaporateAir => {
                state.queue_water_evap(pos);
            }
        }
    }

    let mut changed = false;
    if let Some(target_id) = out.block
        && let Some(block_state) = resolve_block_cached(&mut state.block_state_cache, host, target_id)
    {
        let short_target = strip_ns(target_id);
        let shape_connected = short_target.ends_with("_stairs")
            || short_target.ends_with("_wall")
            || short_target.ends_with("_fence")
            || short_target.ends_with("_fence_gate")
            || short_target.ends_with("_door");
        let known_shape_solid = (!is_water_or_frozen || out.block != Some("minecraft:air"))
            && !shape_connected
            && out.above.is_none()
            && out.below.is_none();
        let flags = if target_id == "minecraft:air" || known_shape_solid {
            BlockUpdateFlags::CLIENTS
                | BlockUpdateFlags::KNOWN_SHAPE
                | BlockUpdateFlags::SUPPRESS_DROPS
        } else {
            BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS
        };
        if level.set_block(host, pos, block_state, flags).unwrap_or(false) {
            for (k, v) in preserved_props {
                let _ = host.set_block_property_with(pos, k, &v, flags.bits());
            }
            changed = true;
            if is_water_or_frozen {
                state.queue_melt_fx(pos);
            }
            if quenched_berm {
                state.queue_shore_col([pos[0], pos[2]]);
            }
            state.rifts[rift_idx].corrupted_blocks += 1;
            if d2 > state.rifts[rift_idx].max_radius_sq {
                state.rifts[rift_idx].max_radius_sq = d2;
            }
            state.dirty = true;

            if let Some(above_id) = out.above
                && y + 1 < OVERWORLD_MAX_Y
                && is_air_like(&above)
                && let Some(above_state) =
                    resolve_block_cached(&mut state.block_state_cache, host, above_id)
            {
                let _ = level.set_block(
                    host,
                    [pos[0], y + 1, pos[2]],
                    above_state,
                    BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                );
            }

            if let Some(below_id) = out.below
                && y - 1 > OVERWORLD_MIN_Y
                && is_air_like(&below)
                && let Some(below_state) =
                    resolve_block_cached(&mut state.block_state_cache, host, below_id)
            {
                let _ = level.set_block(
                    host,
                    [pos[0], y - 1, pos[2]],
                    below_state,
                    BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                );
            }
        }
    }

    if out.is_corrupt && state.mark_quart_corrupted(pos) {
        paint_biome_column(level, host, pos[0], pos[2], local_flavor);
        state.dirty = true;
    }

    changed
}

/// Expands the 360-degree surface crust (including mountain peaks, snow/ice caps,
/// steep cliff faces, trees, and rivers) continuously outward without blocking on
/// deep underground stone.
fn corrupt_surface_step(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_idx: usize,
    budget: &mut usize,
) {
    let mut skips = 0usize;
    let rift_id = state.rifts[rift_idx].id;
    let rift_center = state.rifts[rift_idx].center;
    let rift_flavor = state.rifts[rift_idx].flavor;
    let is_cave_portal =
        state.rifts[rift_idx].corrupted_blocks < 4_000 && portal_is_underground(host, rift_center);

    while *budget > 0 && skips < 96 {
        let rift_age = state.rifts[rift_idx].age_ticks;
        let rift_blocks = state.rifts[rift_idx].corrupted_blocks;
        let mature = rift_blocks >= 35_000 || rift_age >= 5_000;

        if state.rifts[rift_idx].active_column.is_none() {
            let next_col = if let Some(col) = state.pending_frontier_columns.pop_front() {
                if level.is_loaded(host, [col[0], 64, col[1]]) {
                    Some(col)
                } else {
                    None
                }
            } else {
                let mut ring_r = state.rifts[rift_idx].ring_radius;
                let mut sync_needed = state.needs_forced_chunk_sync;
                let picked = pop_next_loaded_column(
                    host,
                    level,
                    &mut state.rifts[rift_idx].ring_columns,
                    &mut ring_r,
                    rift_center,
                    &mut sync_needed,
                );
                state.rifts[rift_idx].ring_radius = ring_r;
                state.needs_forced_chunk_sync = sync_needed;
                picked
            };

            let Some(col) = next_col else {
                break;
            };
            let (world_surface, ocean_floor, min_neighbor_floor) =
                query_surface_and_cliff_heights(host, col, rift_center[1]);
            let (top_y, min_y) = surface_column_y_range(
                rift_center[1],
                is_cave_portal,
                world_surface,
                ocean_floor,
                min_neighbor_floor,
            );
            state.rifts[rift_idx].active_column = Some((col, top_y, min_y));
        }

        let Some((col, y, mut min_y)) = state.rifts[rift_idx].active_column else {
            break;
        };
        if y < min_y {
            state.rifts[rift_idx].active_column = None;
            continue;
        }

        let pos = [col[0], y, col[1]];
        if !level.is_loaded(host, pos) {
            state.rifts[rift_idx].active_column = None;
            state.needs_forced_chunk_sync = true;
            break;
        }

        let current = level.block(host, pos).map(|k| k.id(host)).unwrap_or_default();

        // Extend column depth cleanly through thick snow/ice caps, deep ponds, and tall trees
        // so the rock/soil underneath always converts completely too.
        if !current.is_empty() {
            let s = strip_ns(&current);
            if is_water_like(&current)
                || is_snow_or_ice(&current)
                || s.ends_with("_log")
                || s.ends_with("_wood")
                || s.ends_with("_leaves")
                || s.ends_with("_planks")
                || s.ends_with("_stairs")
                || s.ends_with("_slab")
            {
                min_y = min_y.min((y - SURFACE_CRUST_DEPTH).max(BEDROCK_FLOOR_Y));
            }
        }

        state.rifts[rift_idx].active_column = if y - 1 >= min_y {
            Some((col, y - 1, min_y))
        } else {
            None
        };

        if current.is_empty() || is_air_like(&current) {
            skips += 1;
            continue;
        }

        if apply_corruption_at(
            host,
            level,
            state,
            rift_idx,
            rift_id,
            rift_center,
            rift_flavor,
            mature,
            pos,
            &current,
        ) {
            *budget = budget.saturating_sub(1);
        } else {
            skips += 1;
        }
    }
}

/// Simultaneously sweeps inner rings from the highest mountain surface (`WorldSurface + 2`)
/// all the way down to Bedrock (`-63`) with both player/epicenter proximity priority and full-disk
/// deepening cursors so deep stone, caverns, blackstone layers, and diamond-level Ancient Debris
/// always develop rapidly underground.
fn pop_next_deepen_column(
    host: &dyn ServerHost,
    level: LevelRef,
    cols: &mut Vec<[i32; 2]>,
    deepen_radius: &mut i32,
    ring_radius: i32,
    center: [i32; 3],
    needs_sync: &mut bool,
) -> Option<[i32; 2]> {
    let max_r = (ring_radius - 1).max(1);
    for _ in 0..8 {
        if cols.is_empty() {
            *deepen_radius = if *deepen_radius >= max_r {
                1
            } else {
                *deepen_radius + 1
            };
            *cols = build_ring_columns(center[0], center[2], *deepen_radius);
            *needs_sync = true;
        }
        if let Some(col) = pop_tail_loaded_column(host, level, cols, needs_sync) {
            return Some(col);
        }
        if !cols.is_empty() {
            break;
        }
    }
    None
}

fn refill_priority_deepen_cols(
    host: &dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_idx: usize,
) {
    let rift = &state.rifts[rift_idx];
    let rift_id = rift.id;
    let center = rift.center;
    let max_r = (rift.ring_radius - 1).max(1);
    if max_r < 2 {
        return;
    }

    // Track active Overworld players near this rift so moving into a new cave or digging
    // a new shaft immediately refocuses deep rock corruption instead of grinding old rock.
    let mut tracked_player: Option<([i32; 3], i32)> = None;
    for player in host.players().server_players() {
        if !player
            .dimension(host)
            .is_some_and(|d| d.to_string() == OVERWORLD)
        {
            continue;
        }
        let Some(epos) = player.entity().position(host) else {
            continue;
        };
        let px = epos[0].floor() as i32;
        let py = epos[1].floor() as i32;
        let pz = epos[2].floor() as i32;
        if horiz_dist_sq(center, [px, 64, pz]) <= i64::from(rift.ring_radius + 8).pow(2) {
            let surf_y = host
                .surface_heights(OVERWORLD, &[(px, pz)], Heightmap::OceanFloor)
                .ok()
                .and_then(|hs| hs.first().copied().flatten())
                .unwrap_or(center[1]);
            tracked_player = Some(([px, py, pz], surf_y));
            if let Some(prev) = state.last_priority_focus.get(&rift_id).copied() {
                let dxz2 = (px - prev[0]).abs().max((pz - prev[2]).abs());
                let dy = (py - prev[1]).abs();
                if dxz2 >= 12 || dy >= 8 {
                    state.priority_deepen_cols.remove(&rift_id);
                    state.active_priority_deepen.remove(&rift_id);
                    state.last_deepen_probe_tick.remove(&rift_id);
                }
            }
            break;
        }
    }

    if state
        .priority_deepen_cols
        .get(&rift_id)
        .is_some_and(|q| !q.is_empty())
    {
        return;
    }
    if state
        .last_deepen_probe_tick
        .get(&rift_id)
        .is_some_and(|t| state.tick.saturating_sub(*t) < 30)
    {
        return;
    }
    state.last_deepen_probe_tick.insert(rift_id, state.tick);

    struct ProbeFocus {
        xz: [i32; 2],
        scan_r: i32,
        top_y: i32,
        bottom_y: i32,
        cave_band: i32,
    }

    let mut foci: Vec<ProbeFocus> = Vec::new();
    if let Some(([px, py, pz], surf_y)) = tracked_player {
        state.last_priority_focus.insert(rift_id, [px, py, pz]);
        if py < surf_y - 10 {
            // Player is underground: first sweep a wide horizontal cavern band right at player
            // depth, then drill straight to Bedrock underneath their shaft.
            let cave_top = (py + 14).min(surf_y + 2).min(OVERWORLD_MAX_Y - 1);
            let cave_bot = (py - 12).max(BEDROCK_FLOOR_Y);
            foci.push(ProbeFocus {
                xz: [px, pz],
                scan_r: 20,
                top_y: cave_top,
                bottom_y: cave_bot,
                cave_band: py >> 4,
            });
            if cave_bot > BEDROCK_FLOOR_Y {
                foci.push(ProbeFocus {
                    xz: [px, pz],
                    scan_r: 8,
                    top_y: cave_bot,
                    bottom_y: BEDROCK_FLOOR_Y,
                    cave_band: i32::MIN,
                });
            }
        } else {
            foci.push(ProbeFocus {
                xz: [px, pz],
                scan_r: 14,
                top_y: OVERWORLD_MAX_Y,
                bottom_y: BEDROCK_FLOOR_Y,
                cave_band: i32::MIN,
            });
        }
    }
    // Always reinforce the epicenter core alongside active player neighborhoods.
    foci.push(ProbeFocus {
        xz: [center[0], center[2]],
        scan_r: 18,
        top_y: OVERWORLD_MAX_Y,
        bottom_y: BEDROCK_FLOOR_Y,
        cave_band: i32::MIN,
    });

    let max_r_sq = i64::from(max_r) * i64::from(max_r);
    let mut batch: VecDeque<([i32; 2], i32, i32, i32)> = VecDeque::new();
    for focus in foci {
        let limit_r = focus.scan_r.min(max_r);
        for r in 0..=limit_r {
            let ring = if r == 0 {
                vec![focus.xz]
            } else {
                build_ring_columns(focus.xz[0], focus.xz[1], r)
            };
            for col in ring {
                if batch.len() >= 144 {
                    break;
                }
                if horiz_dist_sq(center, [col[0], 64, col[1]]) > max_r_sq {
                    continue;
                }
                if state.session_deepened_cols.contains(&col)
                    || (focus.cave_band != i32::MIN
                        && state
                            .session_cave_band_cols
                            .contains(&(col, focus.cave_band)))
                    || !level.is_loaded(host, [col[0], 64, col[1]])
                {
                    continue;
                }
                batch.push_back((col, focus.top_y, focus.bottom_y, focus.cave_band));
            }
            if batch.len() >= 144 {
                break;
            }
        }
        if batch.len() >= 144 {
            break;
        }
    }
    if !batch.is_empty() {
        state.priority_deepen_cols.insert(rift_id, batch);
    }
}

fn next_deepen_column_for_stream(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_idx: usize,
    rift_id: u64,
    rift_center: [i32; 3],
    priority: bool,
) -> Option<([i32; 2], i32, i32, i32)> {
    if priority {
        let q = state.priority_deepen_cols.get_mut(&rift_id)?;
        while let Some((col, top_y, bottom_y, band)) = q.pop_front() {
            let already_done = state.session_deepened_cols.contains(&col)
                || (band != i32::MIN && state.session_cave_band_cols.contains(&(col, band)));
            if !already_done && level.is_loaded(host, [col[0], 64, col[1]]) {
                let start_y = if top_y >= OVERWORLD_MAX_Y {
                    let floor_y = host
                        .surface_heights(OVERWORLD, &[(col[0], col[1])], Heightmap::OceanFloor)
                        .ok()
                        .and_then(|hs| hs.first().copied().flatten())
                        .unwrap_or(rift_center[1] + 24);
                    (floor_y - SURFACE_CRUST_DEPTH + 4)
                        .clamp(BEDROCK_FLOOR_Y, OVERWORLD_MAX_Y - 1)
                } else {
                    top_y
                };
                if start_y >= bottom_y {
                    return Some((col, start_y, bottom_y, band));
                }
            }
        }
        return None;
    }

    let mut sync_needed = state.needs_forced_chunk_sync;
    if state.rifts[rift_idx].ring_radius <= 2 {
        return None;
    }
    let mut d_rad = state.rifts[rift_idx]
        .deepen_radius
        .clamp(1, (state.rifts[rift_idx].ring_radius - 1).max(1));
    let ring_radius = state.rifts[rift_idx].ring_radius;
    let mut picked = pop_next_deepen_column(
        host,
        level,
        &mut state.rifts[rift_idx].deepen_columns,
        &mut d_rad,
        ring_radius,
        rift_center,
        &mut sync_needed,
    );
    while let Some(c) = picked {
        if !state.session_deepened_cols.contains(&c) {
            picked = Some(c);
            break;
        }
        picked = pop_next_deepen_column(
            host,
            level,
            &mut state.rifts[rift_idx].deepen_columns,
            &mut d_rad,
            ring_radius,
            rift_center,
            &mut sync_needed,
        );
    }
    state.needs_forced_chunk_sync = sync_needed;
    state.rifts[rift_idx].deepen_radius = d_rad;
    state.rifts[rift_idx].max_deepen_reached = state.rifts[rift_idx].max_deepen_reached.max(d_rad);
    picked.map(|col| {
        let floor_y = host
            .surface_heights(OVERWORLD, &[(col[0], col[1])], Heightmap::OceanFloor)
            .ok()
            .and_then(|hs| hs.first().copied().flatten())
            .unwrap_or(rift_center[1] + 24);
        let start_y = (floor_y - SURFACE_CRUST_DEPTH + 4)
            .clamp(BEDROCK_FLOOR_Y, OVERWORLD_MAX_Y - 1);
        (col, start_y, BEDROCK_FLOOR_Y, i32::MIN)
    })
}

fn bore_deepen_stream(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_idx: usize,
    budget: &mut usize,
    priority: bool,
) {
    let mut skips = 0usize;
    let rift_id = state.rifts[rift_idx].id;
    let rift_center = state.rifts[rift_idx].center;
    let rift_flavor = state.rifts[rift_idx].flavor;

    while *budget > 0 && skips < 2048 {
        let rift_age = state.rifts[rift_idx].age_ticks;
        let rift_blocks = state.rifts[rift_idx].corrupted_blocks;
        let mature = rift_blocks >= 35_000 || rift_age >= 5_000;

        let active_missing = if priority {
            !state.active_priority_deepen.contains_key(&rift_id)
        } else {
            !state.active_deepen_columns.contains_key(&rift_id)
        };
        if active_missing {
            let Some((col, start_y, min_y, band)) = next_deepen_column_for_stream(
                host,
                level,
                state,
                rift_idx,
                rift_id,
                rift_center,
                priority,
            ) else {
                break;
            };
            if priority {
                state
                    .active_priority_deepen
                    .insert(rift_id, (col, start_y, min_y, band));
            } else {
                state
                    .active_deepen_columns
                    .insert(rift_id, (col, start_y, min_y));
            }
        }

        let Some((col, y, min_y, band)) = (if priority {
            state.active_priority_deepen.get(&rift_id).copied()
        } else {
            state
                .active_deepen_columns
                .get(&rift_id)
                .copied()
                .map(|(c, y, m)| (c, y, m, i32::MIN))
        }) else {
            break;
        };
        if y < min_y {
            if priority {
                state.active_priority_deepen.remove(&rift_id);
                if band == i32::MIN {
                    state.mark_session_col_deepened(col);
                } else {
                    state.mark_session_cave_col_deepened(col, band);
                }
            } else {
                state.active_deepen_columns.remove(&rift_id);
                state.mark_session_col_deepened(col);
            }
            continue;
        }

        let pos = [col[0], y, col[1]];
        if !level.is_loaded(host, pos) {
            if priority {
                state.active_priority_deepen.remove(&rift_id);
            } else {
                state.active_deepen_columns.remove(&rift_id);
            }
            break;
        }

        if y - 1 >= min_y {
            if priority {
                state
                    .active_priority_deepen
                    .insert(rift_id, (col, y - 1, min_y, band));
            } else {
                state
                    .active_deepen_columns
                    .insert(rift_id, (col, y - 1, min_y));
            }
        } else if priority {
            state.active_priority_deepen.remove(&rift_id);
            if band == i32::MIN {
                state.mark_session_col_deepened(col);
            } else {
                state.mark_session_cave_col_deepened(col, band);
            }
        } else {
            state.active_deepen_columns.remove(&rift_id);
            state.mark_session_col_deepened(col);
        }

        let current = level.block(host, pos).map(|k| k.id(host)).unwrap_or_default();
        if current.is_empty()
            || is_air_like(&current)
            || is_protected_block(&current)
            || is_preserved_ore(&current)
        {
            skips += 1;
            continue;
        }

        if is_nether_terrain_block(&current) {
            if state.mark_quart_corrupted(pos) {
                let d2 = horiz_dist_sq(rift_center, pos);
                let local_flavor = if d2 <= 40 * 40 {
                    rift_flavor
                } else {
                    NetherFlavor::pick_for_region(pos[0], pos[2], state.seed ^ rift_id)
                };
                paint_biome_column(level, host, pos[0], pos[2], local_flavor);
                state.dirty = true;
            }
            skips += 1;
            continue;
        }

        if apply_corruption_at(
            host,
            level,
            state,
            rift_idx,
            rift_id,
            rift_center,
            rift_flavor,
            mature,
            pos,
            &current,
        ) {
            *budget = budget.saturating_sub(1);
        } else {
            skips += 1;
        }
    }
}

fn corrupt_deepen_step(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_idx: usize,
    budget: &mut usize,
) {
    if *budget == 0 || state.rifts[rift_idx].ring_radius <= 2 {
        return;
    }
    refill_priority_deepen_cols(host, level, state, rift_idx);

    let mut priority_budget = (*budget * 45) / 100;
    let mut ring_budget = budget.saturating_sub(priority_budget);

    bore_deepen_stream(host, level, state, rift_idx, &mut priority_budget, true);
    ring_budget += priority_budget;
    bore_deepen_stream(host, level, state, rift_idx, &mut ring_budget, false);

    *budget = ring_budget;
}

fn spread_corruption_tick(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if state.paused || state.rifts.is_empty() {
        return;
    }

    let uncontained_indices: Vec<usize> = state
        .rifts
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_actively_spreading())
        .map(|(i, _)| i)
        .collect();
    if uncontained_indices.is_empty() {
        return;
    }

    for &idx in &uncontained_indices {
        state.rifts[idx].age_ticks += 1;
    }

    // Progressive per-block speed curve: brand-new rifts creep very slowly (~1 block every
    // ~0.5-0.6s) and every corrupted block slightly accelerates both surface & deep spread.
    let surf_rates: Vec<f64> = uncontained_indices
        .iter()
        .map(|&idx| {
            let r = &state.rifts[idx];
            rift_surface_ops_per_tick(state.spread_budget, r.ring_radius, r.corrupted_blocks)
        })
        .collect();
    let total_surf_rate: f64 = surf_rates.iter().sum();
    let global_cap = (state.spread_budget * 5).clamp(48, 84) as f64;
    let surf_scale = if total_surf_rate > global_cap && total_surf_rate > 0.0 {
        global_cap / total_surf_rate
    } else {
        1.0
    };

    let deep_rates: Vec<f64> = uncontained_indices
        .iter()
        .map(|&idx| {
            let r = &state.rifts[idx];
            rift_deepen_ops_per_tick(state.spread_budget, r.ring_radius, r.corrupted_blocks)
        })
        .collect();
    let total_deep_rate: f64 = deep_rates.iter().sum();
    let global_deep_cap = (state.spread_budget * 10).clamp(96, 192) as f64;
    let deep_scale = if total_deep_rate > global_deep_cap && total_deep_rate > 0.0 {
        global_deep_cap / total_deep_rate
    } else {
        1.0
    };

    let count = uncontained_indices.len();
    let start = (state.tick as usize) % count;

    for offset in 0..count {
        let pos_i = (start + offset) % count;
        let r_idx = uncontained_indices[pos_i];
        let rid = state.rifts[r_idx].id;

        let mut surface_budget = {
            let acc = state.surface_budget_accum.entry(rid).or_insert(0.0);
            *acc += surf_rates[pos_i] * surf_scale;
            let whole = acc.floor() as usize;
            *acc -= whole as f64;
            whole
        };
        if surface_budget > 0 {
            corrupt_surface_step(host, level, state, r_idx, &mut surface_budget);
        }

        if state.rifts[r_idx].ring_radius > 2 {
            let mut deep_budget = {
                let acc = state.deepen_budget_accum.entry(rid).or_insert(0.0);
                *acc += deep_rates[pos_i] * deep_scale;
                let whole = acc.floor() as usize;
                *acc -= whole as f64;
                whole
            };
            if deep_budget > 0 {
                corrupt_deepen_step(host, level, state, r_idx, &mut deep_budget);
            }
        }
    }
}

#[mod_api::horizon_mod]
impl Hooks for Mod {
    fn register(&self, r: &mut dyn Registrar, _cx: &RegistrationContext) {
        let pal = ParticlePalette {
            ash: r.particle_key("minecraft:ash"),
            crimson_spore: r.particle_key("minecraft:crimson_spore"),
            warped_spore: r.particle_key("minecraft:warped_spore"),
            white_ash: r.particle_key("minecraft:white_ash"),
            flame: r.particle_key("minecraft:flame"),
            small_flame: r.particle_key("minecraft:small_flame"),
            large_smoke: r.particle_key("minecraft:large_smoke"),
            smoke: r.particle_key("minecraft:smoke"),
        };
        if let Ok(mut guard) = self.particles.lock() {
            *guard = Some(pal);
        }
        let item_pal = ItemPalette::register(r);
        if let Ok(mut guard) = self.items.lock() {
            *guard = Some(item_pal);
        }
    }

    fn server_started(&self, host: &mut dyn ServerHost, _event: &mut ServerStartedEvent) {
        if let Ok(mut state) = self.state.try_lock() {
            ensure_world_initialized(host, &mut state);
        }
    }

    fn world_saved(&self, host: &mut dyn ServerHost, _event: &mut WorldSavedEvent) {
        if let Ok(mut state) = self.state.try_lock() {
            ensure_world_initialized(host, &mut state);
            state.save_to_host(host);
        }
    }

    fn server_stopping(&self, host: &mut dyn ServerHost, _event: &mut ServerStoppingEvent) {
        let ow = host.level(OVERWORLD);
        if let Ok(mut state) = self.state.try_lock() {
            state.save_to_host(host);
            if let Some(level) = ow {
                release_all_forced_chunks(host, level, &mut state);
            }
        }
    }

    fn world_lifecycle(&self, host: &mut dyn ServerHost, event: &mut WorldLifecycleEvent<'_>) {
        if matches!(event.phase, WorldPhase::WorldCloseFinal | WorldPhase::WorldCloseFailed) {
            let ow = host.level(OVERWORLD);
            if let Ok(mut state) = self.state.try_lock() {
                state.save_to_host(host);
                if let Some(level) = ow {
                    release_all_forced_chunks(host, level, &mut state);
                }
            }
        }
    }

    fn portal_create(&self, _host: &mut dyn ServerHost, event: &mut PortalCreateEvent<'_>) {
        if event.dimension != OVERWORLD {
            return;
        }
        if let Ok(mut q) = self.queues.lock() {
            q.pending_portals.push_back(event.pos);
        }
    }

    fn block_changed(&self, level: &dyn LevelHost, event: &mut BlockChangedEvent) {
        if event.dimension != LevelDimension::Overworld {
            return;
        }
        let pos = [event.pos.x, event.pos.y, event.pos.z];
        let new_name = level.state_block(event.new).unwrap_or_default();
        let old_name = level.state_block(event.old).unwrap_or_default();
        let new_short = strip_ns(new_name);
        let old_short = strip_ns(old_name);

        let is_portal = new_short == "nether_portal";
        let is_seal_change = containment::is_seal_block(new_name, None)
            || containment::is_seal_block(old_name, None)
            || matches!(new_short, "iron_door" | "iron_trapdoor")
            || matches!(old_short, "iron_door" | "iron_trapdoor" | "nether_portal");

        if (is_portal || is_seal_change)
            && let Ok(mut q) = self.queues.lock()
        {
            if is_portal {
                q.pending_portals.push_back(pos);
            }
            if is_seal_change {
                q.pending_containment_checks.push(pos);
            }
        }

        // Non-blocking try_lock prevents same-thread re-entrant deadlock when our own
        // `level.set_block` / `edit_blocks` calls synchronously fire `block_changed`.
        if let Ok(state) = self.state.try_lock() {
            let has_water = is_water_like(new_name)
                || level
                    .state_fluid_state(event.new)
                    .is_some_and(|f| !f.is_empty() && !matches!(new_short, "lava" | "flowing_lava"));
            if (has_water || is_snow_or_ice(new_name))
                && near_corruption_or_active_rift(&state, pos)
                && let Ok(mut q) = self.queues.lock()
            {
                q.pending_water_evap.push(pos);
            }
        }
    }

    fn block_spread(&self, _level: &dyn LevelHost, event: &mut BlockSpreadEvent<'_>) -> Flow {
        if event.dimension != LevelDimension::Overworld {
            return Flow::Continue;
        }
        let to = [event.to.x, event.to.y, event.to.z];
        if let Ok(state) = self.state.try_lock()
            && near_corruption_or_active_rift(&state, to)
        {
            if is_water_like(event.kind) || is_snow_or_ice(event.kind) {
                if let Ok(mut q) = self.queues.lock() {
                    q.pending_water_evap.push(to);
                }
                return Flow::Cancel;
            }
            if matches!(
                strip_ns(event.kind),
                "grass_block" | "mycelium" | "vine" | "sculk" | "moss_block" | "pale_moss_block"
            ) {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    }

    fn fluid_level_change(
        &self,
        _level: &dyn LevelHost,
        event: &mut FluidLevelChangeEvent<'_>,
    ) -> Flow {
        if event.dimension != LevelDimension::Overworld {
            return Flow::Continue;
        }
        if !strip_ns(event.fluid).contains("water") {
            return Flow::Continue;
        }
        let pos = [event.pos.x, event.pos.y, event.pos.z];
        if let Ok(state) = self.state.try_lock()
            && is_within_evap_basin(&state, pos)
        {
            if let Ok(mut q) = self.queues.lock() {
                q.pending_water_evap.push(pos);
            }
            return Flow::Cancel;
        }
        Flow::Continue
    }

    fn create_fluid_source(&self, level: &dyn LevelHost, event: &mut CreateFluidSourceEvent) {
        if event.dimension != LevelDimension::Overworld {
            return;
        }
        let block_name = level.state_block(event.state).unwrap_or_default();
        if !is_water_like(block_name) {
            return;
        }
        let pos = [event.pos.x, event.pos.y, event.pos.z];
        if let Ok(state) = self.state.try_lock()
            && is_within_evap_basin(&state, pos)
        {
            event.can_convert = false;
            if let Ok(mut q) = self.queues.lock() {
                q.pending_water_evap.push(pos);
            }
        }
    }

    fn fill_bucket(&self, host: &mut dyn ServerHost, event: &mut FillBucketEvent<'_>) -> Flow {
        let _ = (host, event);
        Flow::Continue
    }

    fn bucket_empty(&self, host: &mut dyn ServerHost, event: &mut BucketEmptyEvent<'_>) -> Flow {
        if !strip_ns(event.fluid).contains("water") {
            return Flow::Continue;
        }
        let in_overworld = host
            .players()
            .server_players()
            .into_iter()
            .find(|p| p.uuid() == event.player)
            .and_then(|p| p.dimension(host))
            .is_some_and(|d| d.to_string() == OVERWORLD);
        if !in_overworld {
            return Flow::Continue;
        }
        let Some(ow) = host.level(OVERWORLD) else {
            return Flow::Continue;
        };
        if let Ok(state) = self.state.try_lock()
            && (near_corruption_or_active_rift(&state, event.pos)
                || mobs::pos_is_corrupted(host, ow, &state, event.pos))
        {
            if let Ok(mut q) = self.queues.lock() {
                q.queue_bucket_evap(event.player, event.pos);
                q.pending_water_evap.push(event.pos);
            }
            return Flow::Cancel;
        }
        Flow::Continue
    }

    fn living_conversion_pre(
        &self,
        host: &mut dyn ServerHost,
        event: &mut LivingConversionPreEvent<'_>,
    ) -> Flow {
        let should_block = self
            .state
            .try_lock()
            .is_ok_and(|state| {
                mobs::should_block_zombification(
                    host,
                    &state,
                    event.entity,
                    event.kind,
                    event.outcome,
                )
            });
        if should_block {
            // Immediately set IsImmuneToZombification=1 & TimeInOverworld=0 so vanilla
            // never re-triggers playConvertedSound() every single tick after cancellation.
            mobs::stabilize_zombification_immune(host, event.entity);
            if let Ok(mut state) = self.state.try_lock() {
                state.zombification_immune_mobs.insert(event.entity);
            }
            return Flow::Cancel;
        }
        Flow::Continue
    }

    fn living_change_target(&self, host: &dyn Host, event: &mut LivingChangeTargetEvent<'_>) -> Flow {
        mobs::handle_living_change_target(host, event);
        Flow::Continue
    }

    fn living_incoming_damage(
        &self,
        _level: &mut dyn LevelRandomHost,
        event: &mut LivingIncomingDamageEvent<'_>,
    ) -> Flow {
        if let Ok(state) = self.state.try_lock()
            && mobs::should_cancel_incoming_damage(&state, event)
        {
            return Flow::Cancel;
        }
        Flow::Continue
    }

    fn living_death(&self, host: &mut dyn ServerHost, event: &mut LivingDeathEvent<'_>) -> Flow {
        let pal = self.particles.lock().ok().and_then(|g| *g);
        if let Ok(mut state) = self.state.try_lock() {
            if !regression::try_handle_boss_death(
                host,
                &mut state,
                event.entity,
                event.kind,
                event.source.position,
                pal.as_ref(),
            ) {
                mobs::handle_living_death(host, &mut state, event, pal.as_ref());
            }
        } else if let Some(pd) = mobs::extract_living_death(host, event)
            && let Ok(mut q) = self.queues.lock()
        {
            // Deferred when a synchronous `hurt()` call inside `tick_mobs` holds `self.state`.
            q.pending_deaths.push(pd);
        }
        Flow::Continue
    }

    fn explosion_detonate(&self, host: &mut dyn ServerHost, event: &mut ExplosionDetonateEvent<'_>) {
        if event.dimension != OVERWORLD {
            return;
        }
        let Some(level) = host.level(OVERWORLD) else {
            return;
        };
        if let Ok(state) = self.state.try_lock() {
            citadel::protect_citadel_portals_from_explosion(host, level, &state, event.blocks);
        }
    }

    fn container_loot_filled(
        &self,
        _level: &dyn LevelHost,
        event: &mut ContainerLootFilledEvent<'_>,
    ) {
        let Some(item_pal) = self.items.lock().ok().and_then(|g| *g) else {
            return;
        };
        if let Ok(state) = self.state.try_lock() {
            loot::enrich_container_loot(&state, event, &item_pal);
        }
    }

    fn finalize_spawn(
        &self,
        host: &dyn mod_api::Host,
        event: &mut FinalizeSpawnEvent<'_>,
    ) -> Flow {
        if let Ok(state) = self.state.try_lock()
            && mobs::should_cancel_finalize_spawn(host, &state, event)
        {
            event.spawn_cancelled = true;
        }
        Flow::Continue
    }

    fn mob_spawn_placement_check(
        &self,
        host: &dyn mod_api::Host,
        event: &mut MobSpawnPlacementCheckEvent<'_>,
    ) {
        if let Ok(state) = self.state.try_lock() {
            mobs::handle_mob_spawn_placement_check(host, &state, event);
        }
    }

    fn mob_spawn_position_check(
        &self,
        host: &dyn mod_api::Host,
        event: &mut MobSpawnPositionCheckEvent<'_>,
    ) {
        if let Ok(state) = self.state.try_lock() {
            mobs::handle_mob_spawn_position_check(host, &state, event);
        }
    }

    fn potential_spawns(&self, host: &dyn mod_api::Host, event: &mut PotentialSpawnsEvent<'_>) -> Flow {
        if let Ok(state) = self.state.try_lock() {
            mobs::augment_potential_spawns(host, &state, event);
        }
        Flow::Continue
    }

    fn loot_drops(&self, host: &mut dyn ServerHost, event: &mut LootDropsEvent<'_>) {
        if event.context.dimension != OVERWORLD {
            return;
        }
        let Some((eid, kind)) = event.context.entity else {
            return;
        };
        if let Ok(state) = self.state.try_lock() {
            mobs::append_nether_touched_drops(
                host,
                &state,
                eid,
                kind,
                event.context.origin,
                event.drops,
            );
        }
    }

    fn register_commands(&self, _host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        event.commands.push(Command::op("rift", 2));
        event.commands.push(Command::op("rift status", 2));
        event.commands.push(Command::op("rift scan", 2));
        event.commands.push(Command::op("rift pause", 2));
        event.commands.push(Command::op("rift resume", 2));
        event.commands.push(Command::op("rift sealcheck", 2));
        event
            .commands
            .push(Command::op("rift speed <ticks:integer>", 2));
    }

    fn run_mod_command(
        &self,
        host: &mut dyn ServerHost,
        event: &mut RunModCommandEvent<'_>,
    ) -> Flow {
        if event.command.literals.first().map(String::as_str) != Some("rift") {
            return Flow::Continue;
        }

        let sub = event.command.literals.get(1).map(String::as_str);
        let msg = if let Ok(mut state) = self.state.try_lock() {
            ensure_world_initialized(host, &mut state);
            match sub {
                None | Some("status") => {
                    let active = state
                        .rifts
                        .iter()
                        .filter(|r| r.is_actively_spreading())
                        .count();
                    let sealed = state
                        .rifts
                        .iter()
                        .filter(|r| r.contained && !r.pacified)
                        .count();
                    let pacified = state.rifts.iter().filter(|r| r.pacified).count();
                    let regressing = state.rifts.iter().filter(|r| r.regressing).count();
                    let blocks: u64 = state.rifts.iter().map(|r| r.corrupted_blocks).sum();
                    format!(
                        "[Rift Bleed] Rifts: {} total ({} active, {} sealed, {} pacified{}) | Corrupted blocks: {} | Bastions: {} | Outposts: {} | Citadels: {} | Speed: {} ops/tick | State: {}",
                        state.rifts.len(),
                        active,
                        sealed,
                        pacified,
                        if regressing > 0 {
                            format!(", {regressing} regressing")
                        } else {
                            String::new()
                        },
                        blocks,
                        state.bastions.len(),
                        state.outposts.len(),
                        state.citadels.len(),
                        state.spread_budget,
                        if state.paused { "PAUSED" } else { "RUNNING" }
                    )
                }
                Some("scan") => {
                    if let Some(ow) = host.level(OVERWORLD) {
                        let base = [
                            event.command.position[0].floor() as i32,
                            event.command.position[1].floor() as i32,
                            event.command.position[2].floor() as i32,
                        ];
                        let mut found = 0usize;
                        for dx in (-24..=24).step_by(2) {
                            for dz in (-24..=24).step_by(2) {
                                for dy in (-12..=12).step_by(2) {
                                    let p = [
                                        base[0] + dx,
                                        (base[1] + dy).clamp(OVERWORLD_MIN_Y + 1, OVERWORLD_MAX_Y - 1),
                                        base[2] + dz,
                                    ];
                                    if ow.is_loaded(host, p)
                                        && ow
                                            .block(host, p)
                                            .is_some_and(|k| strip_ns(&k.id(host)) == "nether_portal")
                                        && state.find_near_source_mut(p, 12).is_none()
                                    {
                                        register_or_refresh_rift(host, ow, &mut state, p);
                                        found += 1;
                                    }
                                }
                            }
                        }
                        format!("[Rift Bleed] Scan finished near ({}, {}, {}): registered {found} new portal(s).", base[0], base[1], base[2])
                    } else {
                        "[Rift Bleed] Overworld is not loaded.".to_owned()
                    }
                }
                Some("pause") => {
                    state.paused = true;
                    state.dirty = true;
                    if let Some(ow) = host.level(OVERWORLD) {
                        sync_forced_chunks(host, ow, &mut state);
                    }
                    "[Rift Bleed] Corruption spread paused.".to_owned()
                }
                Some("resume") => {
                    state.paused = false;
                    state.dirty = true;
                    "[Rift Bleed] Corruption spread resumed.".to_owned()
                }
                Some("sealcheck") => {
                    if let Some(ow) = host.level(OVERWORLD) {
                        evaluate_rift_containment(host, ow, &mut state, true);
                    }
                    if state.rifts.is_empty() {
                        "[Rift Bleed] No Overworld rifts tracked yet.".to_owned()
                    } else {
                        let details: Vec<String> = state
                            .rifts
                            .iter()
                            .map(|r| {
                                let tag = if r.regressing {
                                    "REGRESSING"
                                } else if r.pacified {
                                    "PACIFIED"
                                } else if r.contained {
                                    "SEALED"
                                } else {
                                    "BREACHED"
                                };
                                format!(
                                    "#{} ({}, {}, {}) [{} | {}{}]",
                                    r.id,
                                    r.center[0],
                                    r.center[1],
                                    r.center[2],
                                    tag,
                                    if r.active_portal { "portal" } else { "scar" },
                                    if r.citadel_spawned { "+citadel" } else { "" }
                                )
                            })
                            .collect();
                        format!("[Rift Bleed] {}", details.join(" ; "))
                    }
                }
                Some("speed") => {
                    let arg = event
                        .command
                        .arguments
                        .iter()
                        .find(|a| a.name == "ticks")
                        .and_then(|a| match a.value {
                            CommandArgumentValue::Integer(n) => Some(n),
                            _ => None,
                        });
                    if let Some(n) = arg {
                        state.spread_budget = (n.clamp(4, 1200)) as usize;
                        state.dirty = true;
                        format!(
                            "[Rift Bleed] Spread speed set to {} ops/tick.",
                            state.spread_budget
                        )
                    } else {
                        "[Rift Bleed] Usage: /rift speed <4..1200>".to_owned()
                    }
                }
                _ => "[Rift Bleed] Commands: /rift <status|scan|pause|resume|sealcheck|speed <ticks>>"
                    .to_owned(),
            }
        } else {
            "[Rift Bleed] Failed to lock mod state.".to_owned()
        };

        let _ = host.commands().feedback(Text::literal(msg));
        Flow::Cancel
    }

    fn level_tick_post(&self, host: HostSide, event: &mut LevelTickPostEvent) {
        let HostSide::Server(server) = host else {
            return;
        };
        if event.level.dimension_id() == NETHER {
            if let Ok(mut state) = self.state.try_lock()
                && state.pending_nether_fob.is_some()
            {
                ensure_world_initialized(server, &mut state);
                nether_fob::tick_nether_dimension(server, event.level, &mut state);
            }
            return;
        }
        if event.level.dimension_id() != OVERWORLD {
            return;
        }

        let pal = self.particles.lock().ok().and_then(|g| *g);
        let drained = self.queues.lock().ok().map(|mut q| std::mem::take(&mut *q));
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        ensure_world_initialized(server, &mut state);
        let (bucket_evap, pre_deaths) = if let Some(q) = drained {
            state.pending_portals.extend(q.pending_portals);
            state
                .pending_containment_checks
                .extend(q.pending_containment_checks);
            for wp in q.pending_water_evap {
                state.queue_water_evap(wp);
            }
            (q.pending_bucket_evap, q.pending_deaths)
        } else {
            (Vec::new(), Vec::new())
        };
        state.tick = state.tick.wrapping_add(1);

        for pd in pre_deaths {
            mobs::apply_living_death(server, event.level, &mut state, &pd, pal.as_ref());
        }

        while let Some(pos) = state.pending_portals.pop_front() {
            register_or_refresh_rift(server, event.level, &mut state, pos);
        }

        evaluate_rift_containment(server, event.level, &mut state, false);
        drain_bucket_evaporations(server, event.level, &mut state, bucket_evap, pal.as_ref());
        drain_water_evaporation(server, event.level, &mut state);
        step_ocean_shores(server, event.level, &mut state);

        if state.tick % 4 == 0 || state.needs_forced_chunk_sync {
            sync_forced_chunks(server, event.level, &mut state);
        }

        spread_corruption_tick(server, event.level, &mut state);
        if state.needs_forced_chunk_sync {
            sync_forced_chunks(server, event.level, &mut state);
        }
        emit_melt_evap_fx(server, event.level, &mut state, pal.as_ref());
        mobs::tick_mobs(server, event.level, &mut state, pal.as_ref());
        let same_tick_deaths = self
            .queues
            .lock()
            .ok()
            .map(|mut q| std::mem::take(&mut q.pending_deaths))
            .unwrap_or_default();
        for pd in same_tick_deaths {
            mobs::apply_living_death(server, event.level, &mut state, &pd, pal.as_ref());
        }
        bastion::tick_bastion_emergence(server, event.level, &mut state);
        outposts::tick_outposts(server, event.level, &mut state);
        citadel::tick_citadel_emergence(server, event.level, &mut state);
        nether_fob::tick_nether_fob_emergence(server, &mut state);
        regression::tick_regression(server, event.level, &mut state);

        if state.tick % 1200 == 0 {
            state.save_to_host(server);
        }
    }

    fn client_level_load(&self, _host: &mut dyn ClientHost, _event: &mut ClientLevelLoadEvent<'_>) {
        if let Ok(mut atmo) = self.atmo.try_lock() {
            *atmo = ClientAtmosphere::default();
        }
    }

    fn client_level_unload(
        &self,
        _host: &mut dyn ClientHost,
        _event: &mut ClientLevelUnloadEvent<'_>,
    ) {
        if let Ok(mut atmo) = self.atmo.try_lock() {
            *atmo = ClientAtmosphere::default();
        }
    }

    fn client_tick_post(&self, host: &mut dyn ClientHost, _event: &mut ClientTickPostEvent) {
        let pal = self.particles.lock().ok().and_then(|g| *g);
        if let Ok(mut atmo) = self.atmo.try_lock() {
            atmosphere::tick_client_atmosphere(&mut atmo, host, pal.as_ref());
        }
    }

    fn compute_fog_color(&self, _host: &dyn ClientHost, event: &mut ComputeFogColorEvent<'_>) {
        if let Ok(atmo) = self.atmo.try_lock() {
            atmosphere::apply_fog_color(&atmo, event);
        }
    }

    fn render_fog(&self, _host: &dyn ClientHost, event: &mut RenderFogEvent<'_>) {
        if let Ok(atmo) = self.atmo.try_lock() {
            atmosphere::apply_render_fog(&atmo, event);
        }
    }

    fn music(&self, _host: &dyn ClientHost, event: &mut MusicEvent<'_>) {
        if let Ok(atmo) = self.atmo.try_lock() {
            atmosphere::apply_nether_music(&atmo, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reentrant_hook_access_never_deadlocks() {
        let m = Mod::default();
        let state_guard = m.state.try_lock().expect("initial state lock");
        assert_eq!(state_guard.spread_budget, 14);

        // Simulate a synchronous re-entrant `block_changed` / `finalize_spawn` callback
        // while `level_tick_post` holds `state`: queues stay writable and `try_lock` never hangs.
        assert!(m.state.try_lock().is_err());
        {
            let mut q = m.queues.lock().expect("ingress queue lock");
            q.pending_portals.push_back([10, 70, 20]);
            q.pending_water_evap.push([12, 64, 22]);
        }
        drop(state_guard);

        let q = m.queues.lock().expect("drain queue");
        assert_eq!(q.pending_portals.len(), 1);
        assert_eq!(q.pending_water_evap.len(), 1);
    }

    #[test]
    fn water_bucket_evap_coalesces_clicks_selects_hand_and_respects_corrupted_biomes() {
        // Double bucket_empty events in the same click coalesce onto the top air block.
        let mut q = HookQueues::default();
        q.queue_bucket_evap(42, [10, 64, 10]);
        q.queue_bucket_evap(42, [10, 65, 10]);
        assert_eq!(q.pending_bucket_evap, vec![(42, [10, 65, 10])]);

        // Hand selection prefers active hotbar slot, falls back to offhand (slot 40).
        assert_eq!(
            select_water_bucket_slot(Some(2), &[(2, "minecraft:water_bucket", 1), (40, "minecraft:shield", 1)]),
            Some((2, 1))
        );
        assert_eq!(
            select_water_bucket_slot(Some(0), &[(0, "minecraft:diamond_pickaxe", 1), (40, "minecraft:axolotl_bucket", 1)]),
            Some((40, 1))
        );
        assert_eq!(
            select_water_bucket_slot(Some(0), &[(0, "minecraft:lava_bucket", 1)]),
            None
        );

        // Sealed rifts still evaporate water INSIDE corrupted Nether biomes, while
        // freeing uncorrupted Overworld soil outside once sealed.
        let mut state = RiftState::new();
        state.rifts.push(RiftSource {
            id: 1,
            center: [0, 64, 0],
            active_portal: true,
            contained: true,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 100,
            corrupted_blocks: 500,
            max_radius_sq: 100,
            ring_radius: 10,
            ring_columns: Vec::new(),
            active_column: None,
            deepen_radius: 1,
            deepen_columns: Vec::new(),
            max_deepen_reached: 1,
            bastions_placed: 0,
            outposts_placed: 0,
            citadel_spawned: false,
            boss_uuid: None,
            pacified: false,
            regressing: false,
            regress_radius: 0,
            regress_columns: Vec::new(),
            regress_active_column: None,
        });
        state.mark_quart_corrupted([4, 64, 4]);
        assert!(near_corruption_or_active_rift(&state, [4, 64, 4]));
        assert!(!near_corruption_or_active_rift(&state, [8, 64, 8]));
    }

    #[test]
    fn waterfront_dam_quenches_edge_and_fills_interior_with_lava_and_obsidian() {
        // Outer deep waterfront (<= 3.5 blocks from ring edge, or touching frontier live water)
        // quenches into cooled volcanic/obsidian dam stone rather than water walls or cobble messes.
        assert!(matches!(
            classify_water_corruption([20, 56, 0], Some(2.0), true, true, true, false),
            WaterCorruptAction::QuenchVolcanicBerm(b) if is_volcanic_berm_block(b)
        ));
        assert!(matches!(
            classify_water_corruption([20, 62, 0], Some(2.5), true, false, true, true),
            WaterCorruptAction::QuenchVolcanicBerm(_)
        ));

        // Interior oceans and rivers convert into a mixture of lava and obsidian crust!
        let mut saw_lava = false;
        let mut saw_obsidian = false;
        for x in 0..120 {
            match classify_water_corruption([x, 62, (x * 5) % 80], Some(12.0), true, false, true, true) {
                WaterCorruptAction::MoltenLake("minecraft:lava") => saw_lava = true,
                WaterCorruptAction::MoltenLake("minecraft:obsidian") => saw_obsidian = true,
                WaterCorruptAction::MoltenLake(_) => {}
                other => panic!("expected MoltenLake on interior ocean surface, got {other:?}"),
            }
        }
        assert!(saw_lava && saw_obsidian, "interior oceans/rivers must yield both lava and obsidian");

        // Deep underground cave puddles stay air-evaporated so caves never flood with lava.
        assert_eq!(
            classify_water_corruption([20, -20, 0], Some(10.0), false, false, false, false),
            WaterCorruptAction::EvaporateAir
        );
    }

    #[test]
    fn ocean_lake_crust_blends_lava_and_obsidian_while_keeping_strider_channels() {
        let mut lava_count = 0usize;
        let mut obsidian_count = 0usize;
        for x in 0..40 {
            for z in 0..40 {
                match pick_lava_obsidian_lake_block([x, 62, z], true) {
                    "minecraft:lava" => lava_count += 1,
                    "minecraft:obsidian" => obsidian_count += 1,
                    _ => {}
                }
            }
        }
        assert!(
            lava_count >= 1000 && obsidian_count >= 120,
            "expected predominantly navigable lava lake with obsidian crusts (lava={lava_count}, obsidian={obsidian_count})"
        );
    }

    #[test]
    fn remote_rifts_force_chunks_for_both_surface_and_deep_stone_fairly() {
        let make_rift = |id: u64, cx: i32, cz: i32, active: bool| RiftSource {
            id,
            center: [cx, 64, cz],
            active_portal: true,
            contained: !active,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 1000,
            corrupted_blocks: 50_000,
            max_radius_sq: 48 * 48,
            ring_radius: 48,
            ring_columns: build_ring_columns(cx, cz, 48),
            active_column: None,
            deepen_radius: 24,
            deepen_columns: build_ring_columns(cx, cz, 24),
            max_deepen_reached: 24,
            bastions_placed: 0,
            outposts_placed: 0,
            citadel_spawned: false,
            boss_uuid: None,
            pacified: false,
            regressing: false,
            regress_radius: 0,
            regress_columns: Vec::new(),
            regress_active_column: None,
        };

        let mut st = RiftState::new();
        st.rifts.push(make_rift(1, 0, 0, true));
        st.rifts.push(make_rift(2, 5000, 5000, true));
        st.rifts.push(make_rift(3, -5000, -5000, false));

        let (core, frontier) = collect_desired_forced_chunks(&st);
        assert!(core.contains(&[0, 0]));
        assert!(core.contains(&[5000 >> 4, 5000 >> 4]));
        assert!(!core.contains(&[-5000 >> 4, -5000 >> 4]));

        let r1_surf = st.rifts[0].ring_columns.last().copied().unwrap();
        let r1_deep = st.rifts[0].deepen_columns.last().copied().unwrap();
        let r2_surf = st.rifts[1].ring_columns.last().copied().unwrap();
        let r2_deep = st.rifts[1].deepen_columns.last().copied().unwrap();
        for col in [r1_surf, r1_deep, r2_surf, r2_deep] {
            let chunk = [col[0] >> 4, col[1] >> 4];
            assert!(
                core.contains(&chunk) || frontier.contains(&chunk),
                "expected active rift cursor chunk {chunk:?} to be forced"
            );
        }
    }
}
