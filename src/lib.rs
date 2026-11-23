mod atmosphere;
mod bastion;
mod citadel;
mod containment;
mod corruption;
mod loot;
mod mobs;
mod outposts;
mod regression;
mod state;
mod types;

use atmosphere::{ClientAtmosphere, ParticlePalette};
use containment::{ContainmentOutcome, ContainmentView, evaluate_chamber, inspect_portal_cluster};
use corruption::{
    SURFACE_CRUST_DEPTH, effective_deepen_budget, effective_rift_budget, is_air_like,
    is_nether_terrain_block, is_preserved_ore, is_protected_block, is_snow_or_ice, is_water_like,
    surface_column_y_range, transform_block,
};
use loot::ItemPalette;
use mobs::paint_biome_column;
use state::RiftState;
use types::{
    BEDROCK_FLOOR_Y, NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, RiftSource,
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
    MusicEvent, ParticleOptions, PortalCreateEvent, PotentialSpawnsEvent, RegisterCommandsEvent,
    Registrar, RegistrationContext, RenderFogEvent, RunModCommandEvent, ServerHost,
    ServerStartedEvent, ServerStoppingEvent, SoundSource, Text, WorldLifecycleEvent, WorldPhase,
    WorldSavedEvent,
};
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

const MAX_CORE_FORCED_CHUNKS: usize = 8;
const MAX_FRONTIER_FORCED_CHUNKS: usize = 8;

/// Re-entrancy-safe ingress queues populated by hooks (`block_changed`, `portal_create`, etc.)
/// without ever holding this lock across a host call.
#[derive(Default)]
struct HookQueues {
    pending_portals: VecDeque<[i32; 3]>,
    pending_containment_checks: Vec<[i32; 3]>,
    pending_water_evap: Vec<[i32; 3]>,
    pending_bucket_evap: Vec<(u128, [i32; 3])>,
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
}

fn sync_forced_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
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

    let mut desired_core: HashSet<[i32; 2]> = HashSet::new();
    for rift in &state.rifts {
        if !rift.is_actively_spreading() || desired_core.len() >= MAX_CORE_FORCED_CHUNKS {
            continue;
        }
        desired_core.insert([rift.center[0] >> 4, rift.center[2] >> 4]);
    }

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

    let mut desired_frontier: Vec<[i32; 2]> = Vec::new();
    for rift in &state.rifts {
        if !rift.is_actively_spreading() {
            continue;
        }
        if let Some(([x, z], _, _)) = rift.active_column {
            let c = [x >> 4, z >> 4];
            if !desired_frontier.contains(&c) {
                desired_frontier.push(c);
            }
        }
        for col in rift.ring_columns.iter().rev().take(48) {
            let c = [col[0] >> 4, col[1] >> 4];
            if !state.core_chunks.contains(&c) && !desired_frontier.contains(&c) {
                desired_frontier.push(c);
                if desired_frontier.len() >= MAX_FRONTIER_FORCED_CHUNKS {
                    break;
                }
            }
        }
        if desired_frontier.len() >= MAX_FRONTIER_FORCED_CHUNKS {
            break;
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
        let check_this = force_all
            || periodic
            || queued
                .iter()
                .any(|p| dist_sq(*p, state.rifts[idx].center) <= 22 * 22);
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
/// Distance inside the frontier where the terraced volcanic coastal shelf slopes all the way
/// down to the scorched dried ocean floor.
const WATER_SHELF_END_DEPTH: f64 = 12.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaterCorruptAction {
    EvaporateAir,
    QuenchVolcanicBerm(&'static str),
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
            .any(|o| (x - o.center[0]).abs() <= 22 && (z - o.center[2]).abs() <= 22)
}

fn pick_volcanic_berm_block(pos: [i32; 3]) -> &'static str {
    match spatial_hash(pos[0], pos[1], pos[2], 0x5EA1_BA5A) % 16 {
        0..=5 => "minecraft:basalt",
        6..=9 => "minecraft:blackstone",
        10..=13 => "minecraft:smooth_basalt",
        _ => "minecraft:magma_block",
    }
}

fn is_volcanic_berm_block(name: &str) -> bool {
    matches!(
        strip_ns(name),
        "basalt" | "smooth_basalt" | "blackstone" | "magma_block"
    )
}

fn volcanic_shelf_target_y(col: [i32; 2], edge_dist: f64, seabed_y: i32, sea_top_y: i32) -> i32 {
    if sea_top_y <= seabed_y {
        return seabed_y;
    }
    if edge_dist <= WATER_SEAWALL_RIM_DEPTH {
        return sea_top_y;
    }

    let stack_roll = spatial_hash(col[0] >> 1, 7, col[1] >> 1, 0x57AC_0001) % 24;
    let sea_stack_top = if stack_roll == 0 {
        let extra = 1 + (spatial_hash(col[0], 11, col[1], 0x57AC_0002) % 3) as i32;
        (seabed_y + extra).min(sea_top_y)
    } else {
        seabed_y
    };

    if edge_dist >= WATER_SHELF_END_DEPTH {
        return sea_stack_top;
    }

    let t = ((edge_dist - WATER_SEAWALL_RIM_DEPTH)
        / (WATER_SHELF_END_DEPTH - WATER_SEAWALL_RIM_DEPTH))
        .clamp(0.0, 1.0);
    let raw = f64::from(sea_top_y) + f64::from(seabed_y - sea_top_y) * t;
    let jitter = (spatial_hash(col[0], 19, col[1], 0xBA5A_1701) % 5) as i32 - 2;
    (raw.round() as i32 + jitter).clamp(sea_stack_top, sea_top_y)
}

fn classify_water_corruption(
    pos: [i32; 3],
    edge_dist: Option<f64>,
    is_deep_water: bool,
    touches_deep_live_water: bool,
) -> WaterCorruptAction {
    if touches_deep_live_water {
        return WaterCorruptAction::QuenchVolcanicBerm(pick_volcanic_berm_block(pos));
    }
    if is_deep_water && edge_dist.is_none_or(|d| d <= WATER_SEAWALL_RIM_DEPTH) {
        return WaterCorruptAction::QuenchVolcanicBerm(pick_volcanic_berm_block(pos));
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

    let mut touches_deep_live = false;
    let horiz = [
        [pos[0] + 1, pos[1], pos[2]],
        [pos[0] - 1, pos[1], pos[2]],
        [pos[0], pos[1], pos[2] + 1],
        [pos[0], pos[1], pos[2] - 1],
    ];
    for n in horiz {
        if !level.is_loaded(host, n) {
            continue;
        }
        let Some(nk) = level.block(host, n) else {
            continue;
        };
        if !is_water_like(&nk.id(host)) {
            continue;
        }
        // Only count as deep live water if neighbor's column spans >= 2 blocks vertically
        // (1-block puddles/farm strips still boil cleanly without forming stone curbs).
        let n_edge = best_active_rift_edge_dist(state, n[0], n[2]);
        if n_edge.is_none_or(|ne| ne < WATER_SEAWALL_RIM_DEPTH) {
            let n_span = column_water_span(host, level, n, 3);
            if n_span >= 2 {
                touches_deep_live = true;
                if n_span >= 3 {
                    is_deep = true;
                }
                break;
            }
        }
    }

    if !is_deep
        && let Ok(tcs) = host.terrain_surface(OVERWORLD, &[(pos[0], pos[2])])
        && let Some(Some(tc)) = tcs.first()
        && let Some(w) = tc.water
        && w - (tc.height - 1) >= 3
    {
        is_deep = true;
    }

    classify_water_corruption(pos, edge, is_deep, touches_deep_live)
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
        if let Some(bs) = resolve_block(host, pick_volcanic_berm_block(pos))
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
    while processed < 112 {
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
                    if let Some(berm_bs) = resolve_block(host, berm_id)
                        && level
                            .set_block(
                                host,
                                pos,
                                berm_bs,
                                BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                            )
                            .unwrap_or(false)
                    {
                        state.queue_melt_fx(pos);
                        state.queue_shore_col([pos[0], pos[2]]);
                        state.dirty = true;
                    }
                    continue;
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
        } else if is_within_evap_basin(state, pos)
            && level.block_property(host, pos, "waterlogged").as_deref() == Some("true")
        {
            let _ = host.set_block_property(pos, "waterlogged", "false");
            state.queue_melt_fx(pos);
            state.dirty = true;
        }
    }

    // Continuous gentle surface & river sweep over existing corrupted quart columns
    // so lingering ponds, snow, or ice patches (including beneath tall forest canopies)
    // never survive inside corrupted lands, while any exposed ocean face is sealed.
    if state.corrupted_quart_cols.is_empty() {
        return;
    }
    let cols: Vec<[i32; 2]> = state.corrupted_quart_cols.iter().copied().collect();
    for _ in 0..12 {
        let idx = state.water_sweep_cursor % cols.len();
        state.water_sweep_cursor = state.water_sweep_cursor.wrapping_add(1);
        let qc = cols[idx];
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
                if level
                    .block(host, p)
                    .is_some_and(|k| {
                        let id = k.id(host);
                        is_water_like(&id) || is_snow_or_ice(&id)
                    })
                    || level.block_property(host, p, "waterlogged").as_deref() == Some("true")
                {
                    state.queue_water_evap(p);
                }
            }
        }

        if mb.is_some_and(|g| g < 62) {
            plug_exposed_ocean_wall_at_col(host, level, state, sub_x, sub_z);
        }
    }
}

/// Smoothly erodes interior volcanic seawall columns down into a stepped volcanic coast and dried
/// scorched seabed as the corruption front advances outward, while strictly refusing to lower any
/// column beside live water so vertical water walls never form.
fn step_ocean_shores(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if state.paused || state.shore_columns.is_empty() {
        return;
    }
    let Some(air) = host.block_state_id("minecraft:air") else {
        return;
    };

    let max_rotations = state.shore_columns.len().min(96);
    let mut eroded_ops = 0usize;
    let mut rotated = 0usize;

    while eroded_ops < 16 && rotated < max_rotations {
        let Some(col) = state.pop_shore_col() else {
            break;
        };
        rotated += 1;

        let Some(edge_dist) = best_active_rift_edge_dist(state, col[0], col[1]) else {
            // Covering rift is now pacified; regression handles final restoration.
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

        eroded_ops += 1;
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
                // Conservatively keep seawall sealed at chunk borders until neighbor loads.
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
                        .is_some_and(|ne| ne > WATER_SEAWALL_RIM_DEPTH + 1.0)
                    {
                        state.queue_water_evap([nx, y, nz]);
                    }
                    break;
                }
            }
        }

        let desired_top = volcanic_shelf_target_y(col, edge_dist, seabed_y, sea_top_y);
        let safe_top = match max_neighbor_water_y {
            Some(wy) => desired_top.max(wy),
            None => desired_top,
        };

        // Never leave an air gap below adjacent neighbor water level.
        if let Some(wy) = max_neighbor_water_y {
            for y in (seabed_y + 1)..=wy {
                let p = [col[0], y, col[1]];
                let is_open = level.block(host, p).is_some_and(|k| {
                    let id = k.id(host);
                    is_air_like(&id) || is_water_like(&id)
                });
                if is_open
                    && let Some(bs) = resolve_block(host, pick_volcanic_berm_block(p))
                    && level
                        .set_block(
                            host,
                            p,
                            bs,
                            BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                        )
                        .unwrap_or(false)
                {
                    state.dirty = true;
                }
            }
        }

        let mut cleared_any = false;
        for y in (safe_top + 1..=scan_top).rev() {
            let p = [col[0], y, col[1]];
            let removable = level.block(host, p).is_some_and(|k| {
                let id = k.id(host);
                is_volcanic_berm_block(&id) || is_water_like(&id)
            });
            if removable
                && level
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
                cleared_any = true;
            }
        }
        if cleared_any {
            if (col[0].wrapping_add(col[1]) & 3) == 0 {
                state.queue_melt_fx([col[0], (safe_top + 1).min(OVERWORLD_MAX_Y - 1), col[1]]);
            }
            state.dirty = true;
        }

        if edge_dist <= WATER_SHELF_END_DEPTH + 1.0 || max_neighbor_water_y.is_some() {
            state.queue_shore_col(col);
        } else {
            state.dirty = true;
        }
    }
}

fn resolve_block(host: &dyn ServerHost, id: &str) -> Option<mod_api::BlockState> {
    host.block_state_id(id)
        .or_else(|| host.block_key(id).and_then(|k| host.block_state_default(k)))
}

fn pop_next_loaded_column(
    host: &dyn ServerHost,
    level: LevelRef,
    cols: &mut Vec<[i32; 2]>,
    radius: &mut i32,
    center: [i32; 3],
) -> Option<[i32; 2]> {
    for _ in 0..6 {
        if cols.is_empty() {
            *radius += 1;
            *cols = build_ring_columns(center[0], center[2], *radius);
        }
        if let Some(pos_idx) = cols.iter().rposition(|c| level.is_loaded(host, [c[0], 64, c[1]])) {
            return Some(cols.swap_remove(pos_idx));
        }
        // Keep expanding or waiting if outer ring is still loading; check up to a couple rings.
        if cols.len() > 64 {
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

    let was_waterlogged = level.block_property(host, pos, "waterlogged").as_deref() == Some("true");
    if was_waterlogged {
        let _ = host.set_block_property(pos, "waterlogged", "false");
        state.queue_water_evap(pos);
        state.queue_melt_fx(pos);
    }

    let mut out = transform_block(pos, current, &above, &below, local_flavor, mature);

    let is_water_or_frozen = is_water_like(current) || is_snow_or_ice(current);
    let mut quenched_berm = false;
    if is_water_like(current) {
        if let WaterCorruptAction::QuenchVolcanicBerm(berm_id) =
            decide_water_corruption(host, level, state, pos)
        {
            out.block = Some(berm_id);
            out.above = None;
            out.below = None;
            out.is_corrupt = true;
            quenched_berm = true;
        } else {
            state.queue_water_evap(pos);
        }
    }

    let mut changed = false;
    if let Some(target_id) = out.block
        && let Some(block_state) = resolve_block(host, target_id)
    {
        let flags = if target_id == "minecraft:air" {
            BlockUpdateFlags::CLIENTS
                | BlockUpdateFlags::KNOWN_SHAPE
                | BlockUpdateFlags::SUPPRESS_DROPS
        } else {
            BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS
        };
        if level.set_block(host, pos, block_state, flags).unwrap_or(false) {
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
                && let Some(above_state) = resolve_block(host, above_id)
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
                && let Some(below_state) = resolve_block(host, below_id)
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
                let r = &mut state.rifts[rift_idx];
                let mut ring_r = r.ring_radius;
                let picked = pop_next_loaded_column(
                    host,
                    level,
                    &mut r.ring_columns,
                    &mut ring_r,
                    rift_center,
                );
                r.ring_radius = ring_r;
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
        }
        if let Some(pos_idx) = cols
            .iter()
            .rposition(|c| level.is_loaded(host, [c[0], 64, c[1]]))
        {
            return Some(cols.swap_remove(pos_idx));
        }
        if cols.len() > 64 {
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
        .is_some_and(|t| state.tick.saturating_sub(*t) < 15)
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
        if horiz_dist_sq(center, [px, 64, pz]) <= i64::from(rift.ring_radius + 6).pow(2) {
            let surf_y = host
                .surface_heights(OVERWORLD, &[(px, pz)], Heightmap::OceanFloor)
                .ok()
                .and_then(|hs| hs.first().copied().flatten())
                .unwrap_or(center[1]);
            if py < surf_y - 12 {
                // Player is underground: target cavern walls, floors, and diamond strata at
                // their actual depth first before boring upper unseen rock.
                let cave_top = (py + 22).min(surf_y + 2).min(OVERWORLD_MAX_Y - 1);
                let cave_bot = (py - 28).max(BEDROCK_FLOOR_Y);
                foci.push(ProbeFocus {
                    xz: [px, pz],
                    scan_r: 26,
                    top_y: cave_top,
                    bottom_y: cave_bot,
                    cave_band: py >> 4,
                });
            } else {
                foci.push(ProbeFocus {
                    xz: [px, pz],
                    scan_r: 20,
                    top_y: OVERWORLD_MAX_Y,
                    bottom_y: BEDROCK_FLOOR_Y,
                    cave_band: i32::MIN,
                });
            }
        }
    }
    // Always prioritize the epicenter core alongside active player neighborhoods.
    foci.push(ProbeFocus {
        xz: [center[0], center[2]],
        scan_r: 24,
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
                if batch.len() >= 280 {
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
            if batch.len() >= 280 {
                break;
            }
        }
        if batch.len() >= 280 {
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
                    let (ws, _, _) = query_surface_and_cliff_heights(host, col, rift_center[1]);
                    (ws + 2).min(OVERWORLD_MAX_Y - 1)
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

    let r = &mut state.rifts[rift_idx];
    if r.ring_radius <= 2 {
        return None;
    }
    let mut d_rad = r.deepen_radius.clamp(1, (r.ring_radius - 1).max(1));
    let mut picked = pop_next_deepen_column(
        host,
        level,
        &mut r.deepen_columns,
        &mut d_rad,
        r.ring_radius,
        rift_center,
    );
    while let Some(c) = picked {
        if !state.session_deepened_cols.contains(&c) {
            picked = Some(c);
            break;
        }
        picked = pop_next_deepen_column(
            host,
            level,
            &mut r.deepen_columns,
            &mut d_rad,
            r.ring_radius,
            rift_center,
        );
    }
    r.deepen_radius = d_rad;
    r.max_deepen_reached = r.max_deepen_reached.max(d_rad);
    picked.map(|col| {
        let (ws, _, _) = query_surface_and_cliff_heights(host, col, rift_center[1]);
        let start_y = (ws + 2).min(OVERWORLD_MAX_Y - 1);
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

    while *budget > 0 && skips < 3072 {
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

    if ring_budget > 0 {
        let mut extra = ring_budget;
        bore_deepen_stream(host, level, state, rift_idx, &mut extra, true);
        ring_budget = extra;
    }
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

    // Compute smooth radius-scaled budgets for surface spread and a dedicated larger
    // deep-boring budget for full peak-to-bedrock underground conversion.
    let raw_budgets: Vec<usize> = uncontained_indices
        .iter()
        .map(|&idx| effective_rift_budget(state.spread_budget, state.rifts[idx].ring_radius))
        .collect();
    let total_raw: usize = raw_budgets.iter().sum();
    let global_cap = (state.spread_budget * 5).clamp(48, 84);

    let deepen_raw: Vec<usize> = uncontained_indices
        .iter()
        .map(|&idx| effective_deepen_budget(state.spread_budget, state.rifts[idx].ring_radius))
        .collect();
    let total_deep_raw: usize = deepen_raw.iter().sum();
    let global_deep_cap = (state.spread_budget * 14).clamp(140, 260);

    let count = uncontained_indices.len();
    let start = (state.tick as usize) % count;

    for offset in 0..count {
        let pos_i = (start + offset) % count;
        let r_idx = uncontained_indices[pos_i];
        let rift_budget = if total_raw > global_cap {
            ((raw_budgets[pos_i] * global_cap) / total_raw).max(2)
        } else {
            raw_budgets[pos_i]
        };

        if state.rifts[r_idx].ring_radius <= 2 {
            let mut surf = rift_budget;
            corrupt_surface_step(host, level, state, r_idx, &mut surf);
        } else {
            let mut surface_budget = rift_budget.div_ceil(2);
            corrupt_surface_step(host, level, state, r_idx, &mut surface_budget);

            let mut deep_budget = if total_deep_raw > global_deep_cap {
                ((deepen_raw[pos_i] * global_deep_cap) / total_deep_raw).max(8)
            } else {
                deepen_raw[pos_i]
            };
            corrupt_deepen_step(host, level, state, r_idx, &mut deep_budget);
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
        if let Ok(mut state) = self.state.try_lock()
            && !regression::try_handle_boss_death(
                host,
                &mut state,
                event.entity,
                event.kind,
                event.source.position,
                pal.as_ref(),
            )
        {
            mobs::handle_living_death(host, &mut state, event, pal.as_ref());
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
        _host: &dyn mod_api::Host,
        event: &mut FinalizeSpawnEvent<'_>,
    ) -> Flow {
        if let Ok(state) = self.state.try_lock()
            && mobs::should_cancel_finalize_spawn(&state, event)
        {
            event.spawn_cancelled = true;
        }
        Flow::Continue
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
        if event.level.dimension_id() != OVERWORLD {
            return;
        }

        let pal = self.particles.lock().ok().and_then(|g| *g);
        let drained = self.queues.lock().ok().map(|mut q| std::mem::take(&mut *q));
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        ensure_world_initialized(server, &mut state);
        let bucket_evap = if let Some(q) = drained {
            state.pending_portals.extend(q.pending_portals);
            state
                .pending_containment_checks
                .extend(q.pending_containment_checks);
            for wp in q.pending_water_evap {
                state.queue_water_evap(wp);
            }
            q.pending_bucket_evap
        } else {
            Vec::new()
        };
        state.tick = state.tick.wrapping_add(1);

        while let Some(pos) = state.pending_portals.pop_front() {
            register_or_refresh_rift(server, event.level, &mut state, pos);
        }

        evaluate_rift_containment(server, event.level, &mut state, false);
        drain_bucket_evaporations(server, event.level, &mut state, bucket_evap, pal.as_ref());
        drain_water_evaporation(server, event.level, &mut state);
        step_ocean_shores(server, event.level, &mut state);

        if state.tick % 20 == 0 {
            sync_forced_chunks(server, event.level, &mut state);
        }

        spread_corruption_tick(server, event.level, &mut state);
        emit_melt_evap_fx(server, event.level, &mut state, pal.as_ref());
        mobs::tick_mobs(server, event.level, &mut state, pal.as_ref());
        bastion::tick_bastion_emergence(server, event.level, &mut state);
        outposts::tick_outposts(server, event.level, &mut state);
        citadel::tick_citadel_emergence(server, event.level, &mut state);
        regression::tick_regression(server, event.level, &mut state);

        if state.tick % 200 == 0 {
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
    fn waterfront_seawall_quenches_deep_edge_and_evaporates_interior_shallow_water() {
        // Outer deep waterfront (<= 3.5 blocks from ring edge, or touching deep live water)
        // quenches into cooled volcanic berm stone rather than creating a hovering water wall.
        assert!(matches!(
            classify_water_corruption([20, 56, 0], Some(2.0), true, true),
            WaterCorruptAction::QuenchVolcanicBerm(b) if is_volcanic_berm_block(b)
        ));
        assert!(matches!(
            classify_water_corruption([20, 62, 0], Some(2.5), true, false),
            WaterCorruptAction::QuenchVolcanicBerm(_)
        ));
        // Even a shallow block right beside deep outside water seals rather than punching air.
        assert!(matches!(
            classify_water_corruption([20, 62, 0], Some(5.0), false, true),
            WaterCorruptAction::QuenchVolcanicBerm(_)
        ));
        // Shallow inland puddles boil cleanly to air, and well-inland deep blocks behind
        // sealed neighbors clear out to air.
        assert_eq!(
            classify_water_corruption([20, 62, 0], Some(1.0), false, false),
            WaterCorruptAction::EvaporateAir
        );
        assert_eq!(
            classify_water_corruption([20, 52, 0], Some(10.0), true, false),
            WaterCorruptAction::EvaporateAir
        );
    }

    #[test]
    fn volcanic_shelf_steps_down_smoothly_and_preserves_seabed() {
        let col = [48, -16];
        let seabed = 40;
        let sea_top = 62;

        // Outer rim holds sea level height.
        assert_eq!(volcanic_shelf_target_y(col, 2.0, seabed, sea_top), sea_top);
        // Mid-coastal shelf sits partway between seabed and sea level.
        let mid = volcanic_shelf_target_y(col, 8.0, seabed, sea_top);
        assert!(mid > seabed && mid < sea_top, "expected sloped shelf in (40, 62), got {mid}");
        // Deep interior boils down to seabed (or at most a small 1-3 block volcanic sea-stack).
        let inner = volcanic_shelf_target_y(col, 16.0, seabed, sea_top);
        assert!(inner >= seabed && inner <= seabed + 3, "expected dried seabed/stack, got {inner}");
    }
}
