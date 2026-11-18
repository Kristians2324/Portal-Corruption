pub mod atmosphere;
pub mod bastion;
pub mod containment;
pub mod corruption;
pub mod mobs;
pub mod state;
pub mod types;

use crate::atmosphere::{
    ClientAtmosphere, apply_fog_color, apply_nether_music, apply_render_fog,
    tick_client_atmosphere,
};
use crate::bastion::tick_bastion_emergence;
use crate::containment::{
    ContainmentOutcome, ContainmentView, evaluate_chamber, inspect_portal_cluster,
};
use crate::corruption::{allowed_min_y, is_air_like, is_water_like, transform_block};
use crate::mobs::{
    append_nether_touched_drops, augment_potential_spawns, handle_living_death, pos_is_corrupted,
    should_block_zombification, tick_mobs,
};
use crate::state::RiftState;
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, RiftSource, build_ring_columns,
    dist_sq, horiz_dist_sq, quart_block_origin, quart_coords, spatial_hash, strip_ns,
};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    BlockChangedEvent, BlockStateId, BucketEmptyEvent, ChunkLoadEvent, ClientDisconnectingEvent,
    ClientLevelLoadEvent, ClientTickPostEvent, ComputeFogColorEvent, Flow, FluidLevelChangeEvent,
    Hooks, Host, LevelDimension, LevelHost, LevelTickPostEvent, LivingConversionPreEvent,
    LivingDeathEvent, LootDropsEvent, MusicEvent, NativeBlockEdit, NativeBlockRun, NativeCommand,
    NativeHeightmap, ParticleOptions, ParticleType, PerformCommandEvent, PortalCreateEvent,
    PotentialSpawnsEvent, RegisterCommandsEvent, RenderFogEvent, Text, WorldLifecycleEvent,
    WorldPhase, WorldSavedEvent,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

const OWNER_CORE_TICKET: &str = "rift_bleed:core";
const OWNER_FRONTIER_TICKET: &str = "rift_bleed:frontier";

struct RuntimeState {
    server: RiftState,
    client: ClientAtmosphere,
    portal_state_ids: HashSet<BlockStateId>,
    water_state_ids: HashSet<BlockStateId>,
    seal_watch_ids: HashSet<BlockStateId>,
    states_cached: bool,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            server: RiftState::new(),
            client: ClientAtmosphere::default(),
            portal_state_ids: HashSet::new(),
            water_state_ids: HashSet::new(),
            seal_watch_ids: HashSet::new(),
            states_cached: false,
        }
    }

    fn ensure_state_cache(&mut self, host: &dyn Host) {
        if self.states_cached {
            return;
        }
        self.states_cached = true;

        for (id, facts) in host.block_states().iter() {
            match strip_ns(&facts.name) {
                "nether_portal" => {
                    self.portal_state_ids.insert(id);
                }
                "water" | "flowing_water" => {
                    self.water_state_ids.insert(id);
                }
                "crying_obsidian" | "obsidian" | "iron_door" | "iron_trapdoor" => {
                    self.seal_watch_ids.insert(id);
                }
                _ => {}
            }
            if facts.waterlogged {
                self.water_state_ids.insert(id);
            }
        }
    }
}

fn runtime() -> &'static Mutex<RuntimeState> {
    static STATE: OnceLock<Mutex<RuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(RuntimeState::new()))
}

struct HostContainmentView<'a> {
    host: &'a dyn Host,
}

impl ContainmentView for HostContainmentView<'_> {
    fn block_name(&self, pos: [i32; 3]) -> Option<String> {
        self.host.biome(pos)?;
        Some(self.host.block(pos))
    }

    fn is_open(&self, pos: [i32; 3]) -> Option<bool> {
        self.host
            .block_property(pos, "open")
            .map(|v| v == "true")
    }
}

pub struct RiftBleedMod;

pub fn hooks() -> Box<dyn Hooks> {
    Box::new(RiftBleedMod)
}

impl Hooks for RiftBleedMod {
    fn world_lifecycle(&self, host: &mut dyn Host, event: &mut WorldLifecycleEvent<'_>) {
        match event.phase {
            WorldPhase::WorldOpenReady => {
                rift_bridge::reset_world_state();
                let world_dir = host.world_dir();
                let seed_hint = world_dir.as_ref().map(|p| {
                    p.to_string_lossy().bytes().fold(0x51F7_2630_i64, |acc, b| {
                        acc.wrapping_mul(131).wrapping_add(i64::from(b))
                    })
                });
                let mut local = RiftState::new();
                local.reset_for_world(world_dir, seed_hint);
                let cells: Vec<[i32; 3]> = local
                    .corrupted_quart_cols
                    .iter()
                    .map(|[qx, qz]| [*qx, 16, *qz])
                    .collect();
                rift_bridge::record_quart_cells(&cells);
                manage_rift_chunk_tickets(host, &mut local);
                if let Ok(mut rt) = runtime().lock() {
                    rt.server = local;
                }
            }
            WorldPhase::WorldCloseFinal | WorldPhase::WorldOpenFailed | WorldPhase::WorldCloseFailed => {
                let mut local = {
                    let Ok(mut rt) = runtime().lock() else {
                        return;
                    };
                    rt.client.reset();
                    std::mem::replace(&mut rt.server, RiftState::new())
                };
                release_all_chunk_tickets(host, &mut local);
                local.dirty = true;
                local.save_to_disk();
                rift_bridge::reset_world_state();
            }
            _ => {}
        }
    }

    fn world_saved(&self, _host: &mut dyn Host, _event: &mut WorldSavedEvent) {
        if let Ok(mut rt) = runtime().lock() {
            rt.server.save_to_disk();
        }
    }

    fn portal_create(&self, _host: &mut dyn Host, event: &mut PortalCreateEvent<'_>) {
        if event.dimension != OVERWORLD {
            return;
        }
        if let Ok(mut rt) = runtime().lock() {
            rt.server.pending_portals.push_back(event.pos);
        }
    }

    fn chunk_load(&self, _host: &mut dyn Host, _event: &mut ChunkLoadEvent<'_>) {}

    fn block_changed(&self, level: &dyn LevelHost, event: &mut BlockChangedEvent) {
        if level.is_client_side() || event.dimension != LevelDimension::Overworld {
            return;
        }
        let pos = [event.pos.x, event.pos.y, event.pos.z];
        let Ok(mut rt) = runtime().lock() else {
            return;
        };

        if rt.portal_state_ids.contains(&event.new) {
            rt.server.pending_portals.push_back(pos);
        }

        if rt.water_state_ids.contains(&event.new)
            || level.state_with_property(event.new, "waterlogged", "true") == Some(event.new)
        {
            if rt.server.is_quart_corrupted(pos) {
                rt.server.pending_water_evap.push_back(pos);
            }
        }

        let near_source = rt
            .server
            .rifts
            .iter()
            .any(|r| dist_sq(r.center, pos) <= 22 * 22);
        if near_source {
            rt.server.pending_containment_checks.push(pos);
        }
    }

    fn fluid_level_change(&self, level: &dyn LevelHost, event: &mut FluidLevelChangeEvent<'_>) -> Flow {
        if level.is_client_side() || event.dimension != LevelDimension::Overworld {
            return Flow::Continue;
        }
        let pos = [event.pos.x, event.pos.y, event.pos.z];
        let is_corrupted = runtime()
            .lock()
            .map(|rt| rt.server.is_quart_corrupted(pos))
            .unwrap_or(false);
        if !is_corrupted {
            return Flow::Continue;
        }

        if event.fluid.contains("water") {
            if let Ok(mut rt) = runtime().lock() {
                rt.server.pending_water_evap.push_back(pos);
            }
        } else if event.fluid.contains("lava") && event.to > 0 {
            rift_bridge::enqueue_fast_lava(pos, event.fluid.to_owned());
        }
        Flow::Continue
    }

    fn bucket_empty(&self, host: &mut dyn Host, event: &mut BucketEmptyEvent<'_>) -> Flow {
        if !event.fluid.contains("water") {
            return Flow::Continue;
        }
        let player_in_overworld = host
            .server_players()
            .iter()
            .find(|p| p.uuid == event.player)
            .map(|p| p.dimension == OVERWORLD)
            .unwrap_or(false);
        if !player_in_overworld {
            return Flow::Continue;
        }
        let in_corrupt = runtime()
            .lock()
            .map(|rt| pos_is_corrupted(host, &rt.server, event.pos))
            .unwrap_or(false);
        if !in_corrupt {
            return Flow::Continue;
        }

        if let Ok(mut rt) = runtime().lock() {
            rt.server.pending_water_evap.push_back(event.pos);
        }
        if let Some(level) = host.level(OVERWORLD) {
            let at = [
                f64::from(event.pos[0]) + 0.5,
                f64::from(event.pos[1]) + 0.5,
                f64::from(event.pos[2]) + 0.5,
            ];
            let _ = level.play_sound(host, "minecraft:block.fire.extinguish", at, 4, 0.7, 1.8);
            let _ = level.send_particles(
                host,
                &ParticleOptions::simple(ParticleType::LargeSmoke),
                None,
                at,
                [0.25, 0.25, 0.25],
                0.02,
                8,
            );
        }
        Flow::Continue
    }

    fn living_conversion_pre(
        &self,
        host: &mut dyn Host,
        event: &mut LivingConversionPreEvent<'_>,
    ) -> Flow {
        let Some(entity) = host.entity(event.entity) else {
            return Flow::Continue;
        };
        let Some(pos) = entity.position(host) else {
            return Flow::Continue;
        };
        let bp = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];
        let block_it = runtime()
            .lock()
            .map(|rt| should_block_zombification(host, &rt.server, event.kind, event.outcome, bp))
            .unwrap_or(false);
        if block_it {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn living_death(&self, host: &mut dyn Host, event: &mut LivingDeathEvent<'_>) -> Flow {
        let mut local_server = {
            let Ok(mut rt) = runtime().lock() else {
                return Flow::Continue;
            };
            std::mem::replace(&mut rt.server, RiftState::new())
        };
        handle_living_death(host, &mut local_server, event);
        if let Ok(mut rt) = runtime().lock() {
            let extra_portals = std::mem::take(&mut rt.server.pending_portals);
            let extra_checks = std::mem::take(&mut rt.server.pending_containment_checks);
            let extra_water = std::mem::take(&mut rt.server.pending_water_evap);
            local_server.pending_portals.extend(extra_portals);
            local_server.pending_containment_checks.extend(extra_checks);
            local_server.pending_water_evap.extend(extra_water);
            rt.server = local_server;
        }
        Flow::Continue
    }

    fn potential_spawns(&self, _host: &dyn Host, event: &mut PotentialSpawnsEvent<'_>) -> Flow {
        if let Ok(rt) = runtime().lock() {
            augment_potential_spawns(&rt.server, event);
        }
        Flow::Continue
    }

    fn loot_drops(&self, _host: &mut dyn Host, event: &mut LootDropsEvent<'_>) {
        if event.context.dimension != OVERWORLD {
            return;
        }
        let Some((mob_id, kind)) = event.context.entity else {
            return;
        };
        if let Ok(rt) = runtime().lock() {
            append_nether_touched_drops(&rt.server, mob_id, kind, event.context.origin, &mut event.drops);
        }
    }

    fn level_tick_post(&self, host: &mut dyn Host, event: &mut LevelTickPostEvent<'_>) {
        if event.dimension != OVERWORLD {
            return;
        }

        let mut local_server = {
            let Ok(mut rt) = runtime().lock() else {
                return;
            };
            rt.ensure_state_cache(host);
            std::mem::replace(&mut rt.server, RiftState::new())
        };

        local_server.tick += 1;

        drain_pending_portals(host, &mut local_server);
        if local_server.tick % 40 == 7 {
            scan_near_players_for_untracked_portals(host, &mut local_server);
        }
        process_containment_checks(host, &mut local_server);
        if local_server.tick % 10 == 1 {
            manage_rift_chunk_tickets(host, &mut local_server);
        }
        process_water_evaporation(host, &mut local_server);
        spread_corruption_step(host, &mut local_server);
        emit_rift_ambient_effects(host, &local_server);
        tick_mobs(host, &mut local_server);
        tick_bastion_emergence(host, &mut local_server);

        if local_server.tick % 400 == 0 && local_server.dirty {
            local_server.save_to_disk();
        }

        if let Ok(mut rt) = runtime().lock() {
            let extra_portals = std::mem::take(&mut rt.server.pending_portals);
            let extra_checks = std::mem::take(&mut rt.server.pending_containment_checks);
            let extra_water = std::mem::take(&mut rt.server.pending_water_evap);
            local_server.pending_portals.extend(extra_portals);
            local_server.pending_containment_checks.extend(extra_checks);
            local_server.pending_water_evap.extend(extra_water);
            rt.server = local_server;
        }
    }

    fn client_level_load(&self, _host: &mut dyn Host, event: &mut ClientLevelLoadEvent<'_>) {
        if event.dimension == OVERWORLD
            && let Ok(mut rt) = runtime().lock()
        {
            rt.client.world_loaded = true;
        }
    }

    fn client_disconnecting(&self, _host: &dyn Host, _event: &mut ClientDisconnectingEvent) {
        if let Ok(mut rt) = runtime().lock() {
            rt.client.reset();
        }
    }

    fn client_tick_post(&self, host: &mut dyn Host, _event: &mut ClientTickPostEvent) {
        let mut local_client = {
            let Ok(mut rt) = runtime().lock() else {
                return;
            };
            std::mem::take(&mut rt.client)
        };
        tick_client_atmosphere(host, &mut local_client);
        if let Ok(mut rt) = runtime().lock() {
            rt.client = local_client;
        }
    }

    fn compute_fog_color(&self, _host: &dyn Host, event: &mut ComputeFogColorEvent) {
        if let Ok(rt) = runtime().lock() {
            apply_fog_color(&rt.client, event);
        }
    }

    fn render_fog(&self, _host: &dyn Host, event: &mut RenderFogEvent) {
        if let Ok(rt) = runtime().lock() {
            apply_render_fog(&rt.client, event);
        }
    }

    fn music(&self, _host: &dyn Host, event: &mut MusicEvent) {
        if let Ok(rt) = runtime().lock() {
            apply_nether_music(&rt.client, event);
        }
    }

    fn register_commands(&self, _host: &mut dyn Host, event: &mut RegisterCommandsEvent) {
        event.commands.push(NativeCommand::new("rift status"));
        event.commands.push(NativeCommand::new("rift scan"));
        event.commands.push(NativeCommand::op("rift pause", 2));
        event.commands.push(NativeCommand::op("rift resume", 2));
        event.commands.push(NativeCommand::op("rift speed <blocks:integer>", 2));
    }

    fn perform_command(
        &self,
        host: &mut dyn Host,
        event: &mut PerformCommandEvent<'_>,
    ) -> Flow {
        let cmd = event.command.trim();
        let Some(rest) = cmd.strip_prefix("rift") else {
            return Flow::Continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.is_empty() {
            let _ = host.tell(
                event.player.uuid,
                Text::literal("[Rift Bleed] Usage: /rift status | scan | pause | resume | speed <16..600>"),
            );
            return Flow::Cancel;
        }

        match parts[0] {
            "status" => {
                if let Ok(rt) = runtime().lock() {
                    let s = &rt.server;
                    let uncontained = s.rifts.iter().filter(|r| !r.contained).count();
                    let total_blocks: u64 = s.rifts.iter().map(|r| r.corrupted_blocks).sum();
                    let _ = host.tell(
                        event.player.uuid,
                        Text::literal(format!(
                            "[Rift Bleed] Rifts: {} ({} uncontained), Corrupted blocks: {}, Bastions: {}, Budget: {}/tick, Paused: {}",
                            s.rifts.len(),
                            uncontained,
                            total_blocks,
                            s.bastions.len(),
                            s.spread_budget,
                            s.paused
                        )),
                    );
                    for r in &s.rifts {
                        let status = if r.contained { "CONTAINED" } else { "LEAKING" };
                        let kind = if r.active_portal { "Portal" } else { "Scar" };
                        let _ = host.tell(
                            event.player.uuid,
                            Text::literal(format!(
                                "  #{} [{} | {} | {}] at ({},{},{}) ring={} deep={} blocks={}",
                                r.id,
                                status,
                                kind,
                                r.flavor.display_name(),
                                r.center[0],
                                r.center[1],
                                r.center[2],
                                r.ring_radius,
                                r.deepen_radius,
                                r.corrupted_blocks
                            )),
                        );
                    }
                }
                Flow::Cancel
            }
            "scan" => {
                let bp = [
                    event.player.position[0].floor() as i32,
                    event.player.position[1].floor() as i32,
                    event.player.position[2].floor() as i32,
                ];
                let mut found = 0usize;
                if let Ok(mut rt) = runtime().lock() {
                    found = scan_box_for_portals(host, &mut rt.server, bp, 24, 16);
                }
                let _ = host.tell(
                    event.player.uuid,
                    Text::literal(format!("[Rift Bleed] Scanned nearby area; queued {found} portal probe(s).")),
                );
                Flow::Cancel
            }
            "pause" | "resume" if event.player.permission >= 2 => {
                let pause = parts[0] == "pause";
                if let Ok(mut rt) = runtime().lock() {
                    rt.server.paused = pause;
                    rt.server.dirty = true;
                }
                let _ = host.tell(
                    event.player.uuid,
                    Text::literal(format!(
                        "[Rift Bleed] Corruption spreading {}",
                        if pause { "paused." } else { "resumed." }
                    )),
                );
                Flow::Cancel
            }
            "speed" if event.player.permission >= 2 && parts.len() >= 2 => {
                if let Ok(val) = parts[1].parse::<usize>() {
                    let clamped = val.clamp(16, 600);
                    if let Ok(mut rt) = runtime().lock() {
                        rt.server.spread_budget = clamped;
                        rt.server.dirty = true;
                    }
                    let _ = host.tell(
                        event.player.uuid,
                        Text::literal(format!("[Rift Bleed] Spread budget set to {clamped} ops/tick.")),
                    );
                } else {
                    let _ = host.tell(
                        event.player.uuid,
                        Text::literal("[Rift Bleed] Usage: /rift speed <16..600>"),
                    );
                }
                Flow::Cancel
            }
            _ => Flow::Continue,
        }
    }
}

fn manage_rift_chunk_tickets(host: &mut dyn Host, state: &mut RiftState) {
    let mut desired_cores = HashSet::new();
    let mut desired_frontiers: Vec<[i32; 2]> = Vec::new();

    for rift in &state.rifts {
        if rift.contained {
            continue;
        }
        desired_cores.insert([rift.center[0] >> 4, rift.center[2] >> 4]);
        if let Some((col, _, _)) = rift.active_column {
            let chunk = [col[0] >> 4, col[1] >> 4];
            if !desired_frontiers.contains(&chunk) {
                desired_frontiers.push(chunk);
            }
        }
        // Look ahead along the current ring and deepen queues so upcoming chunks
        // are already loaded before the cursor steps onto them.
        for col in rift.ring_columns.iter().rev().take(48) {
            let chunk = [col[0] >> 4, col[1] >> 4];
            if !desired_frontiers.contains(&chunk) {
                desired_frontiers.push(chunk);
                if desired_frontiers.len() >= 4 {
                    break;
                }
            }
        }
        if desired_frontiers.len() < 4 {
            for col in rift.deepen_columns.iter().rev().take(24) {
                let chunk = [col[0] >> 4, col[1] >> 4];
                if !desired_frontiers.contains(&chunk) {
                    desired_frontiers.push(chunk);
                    if desired_frontiers.len() >= 4 {
                        break;
                    }
                }
            }
        }
    }

    // Release unneeded core tickets (for sealed rifts).
    let old_cores: Vec<[i32; 2]> = state.core_tickets.difference(&desired_cores).copied().collect();
    for c in old_cores {
        let _ = host.chunk_load_release(OWNER_CORE_TICKET, OVERWORLD, c);
        state.core_tickets.remove(&c);
    }
    for c in desired_cores {
        if state.core_tickets.insert(c) {
            let _ = host.chunk_load_request(OWNER_CORE_TICKET, OVERWORLD, c);
        }
    }

    // Update bounded sliding window (max 4 frontier chunks at a time).
    let desired_set: HashSet<[i32; 2]> = desired_frontiers.iter().copied().collect();
    while let Some(front) = state.frontier_tickets.front().copied() {
        if state.frontier_tickets.len() > 4 || !desired_set.contains(&front) {
            state.frontier_tickets.pop_front();
            let _ = host.chunk_load_release(OWNER_FRONTIER_TICKET, OVERWORLD, front);
        } else {
            break;
        }
    }
    for c in desired_frontiers {
        if !state.frontier_tickets.contains(&c) && state.frontier_tickets.len() < 4 {
            if host.chunk_load_request(OWNER_FRONTIER_TICKET, OVERWORLD, c) {
                state.frontier_tickets.push_back(c);
            }
        }
    }
}

fn release_all_chunk_tickets(host: &mut dyn Host, state: &mut RiftState) {
    for c in state.core_tickets.drain() {
        let _ = host.chunk_load_release(OWNER_CORE_TICKET, OVERWORLD, c);
    }
    while let Some(c) = state.frontier_tickets.pop_front() {
        let _ = host.chunk_load_release(OWNER_FRONTIER_TICKET, OVERWORLD, c);
    }
}

fn drain_pending_portals(host: &mut dyn Host, state: &mut RiftState) {
    let mut registered_any = false;
    while let Some(pos) = state.pending_portals.pop_front() {
        let view = HostContainmentView { host };
        let (lit, seeds, centroid) = inspect_portal_cluster(&view, pos);
        if !lit {
            continue;
        }
        if let Some(existing) = state.find_near_source_mut(centroid, 12) {
            existing.center = centroid;
            existing.active_portal = true;
            state.pending_containment_checks.push(centroid);
            continue;
        }

        let id = state.next_id;
        state.next_id += 1;
        let flavor = NetherFlavor::pick_for_region(centroid[0], centroid[2], state.seed ^ id);
        let contained = matches!(evaluate_chamber(&view, &seeds), ContainmentOutcome::Contained);
        let ring_columns = build_ring_columns(centroid[0], centroid[2], 1);

        state.rifts.push(RiftSource {
            id,
            center: centroid,
            active_portal: true,
            contained,
            flavor,
            age_ticks: 0,
            corrupted_blocks: 0,
            max_radius_sq: 0,
            ring_radius: 1,
            ring_columns,
            active_column: None,
            deepen_radius: 1,
            deepen_columns: Vec::new(),
            bastions_placed: 0,
        });
        state.dirty = true;
        registered_any = true;

        let msg = if contained {
            format!(
                "[Rift Bleed] Portal at ({}, {}, {}) ignited inside Crying Obsidian containment — Rift sealed!",
                centroid[0], centroid[1], centroid[2]
            )
        } else {
            format!(
                "[Rift Bleed] An uncontained Nether Portal ({}) tears open at ({}, {}, {})! Seal it in Crying Obsidian!",
                flavor.display_name(),
                centroid[0],
                centroid[1],
                centroid[2]
            )
        };
        broadcast_overworld(host, &msg);
    }
    if registered_any {
        manage_rift_chunk_tickets(host, state);
        state.save_to_disk();
    }
}

fn scan_near_players_for_untracked_portals(host: &mut dyn Host, state: &mut RiftState) {
    let players = host.server_players();
    for player in players {
        if player.dimension != OVERWORLD {
            continue;
        }
        let bp = [
            player.position[0].floor() as i32,
            player.position[1].floor() as i32,
            player.position[2].floor() as i32,
        ];
        for i in 0..48u64 {
            let h = spatial_hash(bp[0], bp[1], bp[2], state.tick.wrapping_add(i));
            let dx = ((h & 31) as i32) - 16;
            let dy = (((h >> 5) & 15) as i32) - 8;
            let dz = (((h >> 9) & 31) as i32) - 16;
            let probe = [
                bp[0] + dx,
                (bp[1] + dy).clamp(OVERWORLD_MIN_Y + 2, OVERWORLD_MAX_Y - 2),
                bp[2] + dz,
            ];
            if host.biome(probe).is_none() {
                continue;
            }
            if strip_ns(&host.block(probe)) == "nether_portal"
                && state
                    .rifts
                    .iter()
                    .all(|r| dist_sq(r.center, probe) > 12 * 12)
            {
                state.pending_portals.push_back(probe);
            }
        }
    }
}

fn scan_box_for_portals(
    host: &dyn Host,
    state: &mut RiftState,
    center: [i32; 3],
    horiz_r: i32,
    vert_r: i32,
) -> usize {
    let mut queued = 0usize;
    for x in (center[0] - horiz_r..=center[0] + horiz_r).step_by(2) {
        for z in (center[2] - horiz_r..=center[2] + horiz_r).step_by(2) {
            if host.biome([x, center[1].clamp(0, 200), z]).is_none() {
                continue;
            }
            for y in (center[1] - vert_r).max(OVERWORLD_MIN_Y)..=(center[1] + vert_r).min(OVERWORLD_MAX_Y)
            {
                if strip_ns(&host.block([x, y, z])) == "nether_portal" {
                    state.pending_portals.push_back([x, y, z]);
                    queued += 1;
                    break;
                }
            }
        }
    }
    queued
}

fn process_containment_checks(host: &mut dyn Host, state: &mut RiftState) {
    let periodic = state.tick % 45 == 19;
    if state.pending_containment_checks.is_empty() && !periodic {
        return;
    }

    let triggers = std::mem::take(&mut state.pending_containment_checks);
    let mut alerts: Vec<(String, Option<([i32; 3], &'static str, f32)>)> = Vec::new();
    let mut ticket_change = false;

    {
        let view = HostContainmentView { host };

        for idx in 0..state.rifts.len() {
            let center = state.rifts[idx].center;
            if view.block_name(center).is_none() {
                continue;
            }
            let check_needed = periodic
                || triggers
                    .iter()
                    .any(|p| dist_sq(*p, center) <= 22 * 22);
            if !check_needed {
                continue;
            }

            let prev_contained = state.rifts[idx].contained;
            let prev_active = state.rifts[idx].active_portal;
            let (lit, seeds, updated_center) = inspect_portal_cluster(&view, center);
            state.rifts[idx].active_portal = lit;
            if lit {
                state.rifts[idx].center = updated_center;
            }

            match evaluate_chamber(&view, &seeds) {
                ContainmentOutcome::Indeterminate => {}
                ContainmentOutcome::Contained => {
                    if !prev_contained {
                        state.rifts[idx].contained = true;
                        state.dirty = true;
                        ticket_change = true;
                        let kind = if lit { "Portal" } else { "Rift Scar" };
                        let msg = format!(
                            "[Rift Bleed] {kind} at ({}, {}, {}) has been sealed inside Crying Obsidian! Corruption halted.",
                            updated_center[0], updated_center[1], updated_center[2]
                        );
                        alerts.push((
                            msg,
                            Some((updated_center, "minecraft:block.beacon.deactivate", 0.75)),
                        ));
                    }
                }
                ContainmentOutcome::Breached => {
                    if prev_contained {
                        state.rifts[idx].contained = false;
                        state.dirty = true;
                        ticket_change = true;
                        if state.rifts[idx].ring_columns.is_empty() {
                            let r = state.rifts[idx].ring_radius;
                            state.rifts[idx].ring_columns =
                                build_ring_columns(updated_center[0], updated_center[2], r);
                        }
                        let msg = format!(
                            "[Rift Bleed] Containment breached at ({}, {}, {})! Corruption resumes bleeding into the Overworld!",
                            updated_center[0], updated_center[1], updated_center[2]
                        );
                        alerts.push((
                            msg,
                            Some((updated_center, "minecraft:entity.wither.break_block", 0.7)),
                        ));
                    } else if prev_active && !lit {
                        state.dirty = true;
                        let msg = format!(
                            "[Rift Bleed] Portal broken at ({}, {}, {}) — a Rift Scar remains! Seal the Scar itself with Crying Obsidian to stop the spread!",
                            updated_center[0], updated_center[1], updated_center[2]
                        );
                        alerts.push((msg, None));
                    }
                }
            }
        }
    }

    if ticket_change {
        manage_rift_chunk_tickets(host, state);
    }

    for (msg, sound) in alerts {
        broadcast_overworld(host, &msg);
        if let Some((pos, event, pitch)) = sound
            && let Some(level) = host.level(OVERWORLD)
        {
            let at = [
                f64::from(pos[0]) + 0.5,
                f64::from(pos[1]) + 0.5,
                f64::from(pos[2]) + 0.5,
            ];
            let _ = level.play_sound(host, event, at, 4, 0.9, pitch);
        }
    }
}

fn process_water_evaporation(host: &mut dyn Host, state: &mut RiftState) {
    if state.pending_water_evap.is_empty() {
        return;
    }
    let Some(air) = host.default_state("minecraft:air") else {
        state.pending_water_evap.clear();
        return;
    };

    let mut budget = 32usize;
    while budget > 0 {
        let Some(pos) = state.pending_water_evap.pop_front() else {
            break;
        };
        if host.biome(pos).is_none() {
            continue;
        }
        let block = host.block(pos);
        if is_water_like(&block) {
            if host.set_block(OVERWORLD, pos, air, 3) {
                budget -= 1;
                evaporate_fx(host, pos);
            }
        } else if host.block_property(pos, "waterlogged").as_deref() == Some("true")
            && host.set_block_property(pos, "waterlogged", "false")
        {
            budget -= 1;
            evaporate_fx(host, pos);
        }
    }
}

fn evaporate_fx(host: &mut dyn Host, pos: [i32; 3]) {
    if let Some(level) = host.level(OVERWORLD) {
        let at = [
            f64::from(pos[0]) + 0.5,
            f64::from(pos[1]) + 0.5,
            f64::from(pos[2]) + 0.5,
        ];
        let _ = level.play_sound(host, "minecraft:block.fire.extinguish", at, 4, 0.45, 1.9);
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(ParticleType::LargeSmoke),
            None,
            at,
            [0.2, 0.2, 0.2],
            0.02,
            4,
        );
    }
}

/// Continuously expands every uncontained Rift in concentric 360-degree circles,
/// corrupting both the surface and full underground rock/cave columns down toward
/// bedrock `-63`, even while no player is nearby.
fn spread_corruption_step(host: &mut dyn Host, state: &mut RiftState) {
    if state.paused {
        return;
    }

    let active_indices: Vec<usize> = state
        .rifts
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.contained)
        .map(|(i, _)| i)
        .collect();
    if active_indices.is_empty() {
        return;
    }

    let total_budget = state.spread_budget.max(16);
    let per_rift_budget = (total_budget / active_indices.len()).max(16);
    let mut edit = NativeBlockEdit {
        palette: Vec::new(),
        filters: Vec::new(),
        runs: Vec::new(),
        block_entities: Vec::new(),
        neighbour_updates: false,
    };
    let mut palette_map: HashMap<&'static str, u32> = HashMap::new();
    let mut biome_updates: Vec<([i32; 3], u8)> = Vec::new();
    let mut quart_records: Vec<[i32; 3]> = Vec::new();
    let mut lava_nudges: Vec<([i32; 3], String)> = Vec::new();

    for rift_idx in active_indices {
        state.rifts[rift_idx].age_ticks += 1;

        let rift_id = state.rifts[rift_idx].id;
        let rift_center = state.rifts[rift_idx].center;
        let rift_flavor = state.rifts[rift_idx].flavor;
        let rift_age = state.rifts[rift_idx].age_ticks;
        let rift_mature = state.rifts[rift_idx].corrupted_blocks >= 2_000;
        let seed = state.seed;

        let mut remaining_writes = per_rift_budget;
        let mut scan_budget = per_rift_budget * 8;

        while remaining_writes > 0 && scan_budget > 0 {
            // Pull next column if no active vertical column is currently in progress.
            if state.rifts[rift_idx].active_column.is_none() {
                let is_deepen_turn = state.tick % 4 == 0
                    && state.rifts[rift_idx].deepen_radius < state.rifts[rift_idx].ring_radius
                    && rift_mature;

                let next_col = if is_deepen_turn {
                    if state.rifts[rift_idx].deepen_columns.is_empty() {
                        state.rifts[rift_idx].deepen_radius += 1;
                        let dr = state.rifts[rift_idx].deepen_radius;
                        state.rifts[rift_idx].deepen_columns =
                            build_ring_columns(rift_center[0], rift_center[2], dr);
                    }
                    state.rifts[rift_idx].deepen_columns.pop()
                } else if let Some(hcol) = state.pending_frontier_columns.pop_front() {
                    Some(hcol)
                } else {
                    if state.rifts[rift_idx].ring_columns.is_empty() {
                        state.rifts[rift_idx].ring_radius += 1;
                        let r = state.rifts[rift_idx].ring_radius;
                        state.rifts[rift_idx].ring_columns =
                            build_ring_columns(rift_center[0], rift_center[2], r);
                        state.dirty = true;
                    }
                    state.rifts[rift_idx].ring_columns.pop()
                };

                let Some(col) = next_col else {
                    break;
                };

                // Require column chunk to be loaded (or wait a tick for ticketed chunk to load).
                if host.biome([col[0], 64, col[1]]).is_none() {
                    let chunk = [col[0] >> 4, col[1] >> 4];
                    let _ = host.chunk_load_request(OWNER_FRONTIER_TICKET, OVERWORLD, chunk);
                    if is_deepen_turn {
                        state.rifts[rift_idx].deepen_columns.push(col);
                    } else {
                        state.rifts[rift_idx].ring_columns.push(col);
                    }
                    break;
                }

                let hdist = (horiz_dist_sq(
                    [col[0], rift_center[1], col[1]],
                    rift_center,
                ) as f64)
                    .sqrt() as i32;
                let min_y = allowed_min_y(
                    rift_center[1],
                    rift_age,
                    hdist,
                    state.rifts[rift_idx].corrupted_blocks,
                );

                let top_y = host
                    .surface_heights(OVERWORLD, &[(col[0], col[1])], NativeHeightmap::WorldSurface)
                    .ok()
                    .and_then(|v| v.into_iter().next().flatten())
                    .map(|y| (y + 1).clamp(min_y, OVERWORLD_MAX_Y - 1))
                    .unwrap_or_else(|| (rift_center[1] + 24).clamp(min_y, OVERWORLD_MAX_Y - 1));

                let start_y = if is_deepen_turn {
                    top_y.min(rift_center[1] - 12).max(min_y)
                } else {
                    top_y
                };

                // Paint the full 4x4x4 quart biome column for this `(x, z)` immediately.
                let local_flavor = if hdist < 28 {
                    rift_flavor
                } else {
                    NetherFlavor::pick_for_region(col[0], col[1], seed ^ rift_id)
                };
                if state.mark_quart_corrupted([col[0], start_y, col[1]]) {
                    biome_updates.push((
                        quart_block_origin([col[0], start_y, col[1]]),
                        local_flavor.to_u8(),
                    ));
                    quart_records.push(quart_coords([col[0], start_y, col[1]]));
                }

                let r_sq = horiz_dist_sq([col[0], 0, col[1]], rift_center);
                if r_sq > state.rifts[rift_idx].max_radius_sq {
                    state.rifts[rift_idx].max_radius_sq = r_sq;
                }

                state.rifts[rift_idx].active_column = Some((col, start_y, min_y));
            }

            let Some((col, mut cur_y, min_y)) = state.rifts[rift_idx].active_column else {
                break;
            };
            if host.biome([col[0], 64, col[1]]).is_none() {
                break;
            }

            let hdist = (horiz_dist_sq([col[0], rift_center[1], col[1]], rift_center) as f64).sqrt()
                as i32;
            let local_flavor = if hdist < 28 {
                rift_flavor
            } else {
                NetherFlavor::pick_for_region(col[0], col[1], seed ^ rift_id)
            };

            while cur_y >= min_y && remaining_writes > 0 && scan_budget > 0 {
                scan_budget -= 1;
                let pos = [col[0], cur_y, col[1]];
                let cur = host.block(pos);

                if !is_air_like(&cur) {
                    let above = host.block([pos[0], pos[1] + 1, pos[2]]);
                    let below = host.block([pos[0], pos[1] - 1, pos[2]]);
                    let outcome =
                        transform_block(pos, &cur, &above, &below, local_flavor, rift_mature);

                    if let Some(new_block) = outcome.block {
                        push_edit(&mut edit, &mut palette_map, pos, new_block);
                        remaining_writes -= 1;
                        state.rifts[rift_idx].corrupted_blocks += 1;
                        state.dirty = true;
                    }
                    if let Some(above_block) = outcome.above
                        && is_air_like(&above)
                    {
                        push_edit(
                            &mut edit,
                            &mut palette_map,
                            [pos[0], pos[1] + 1, pos[2]],
                            above_block,
                        );
                    }
                    if let Some(below_block) = outcome.below
                        && is_air_like(&below)
                    {
                        push_edit(
                            &mut edit,
                            &mut palette_map,
                            [pos[0], pos[1] - 1, pos[2]],
                            below_block,
                        );
                    }
                    if outcome.is_corrupt && matches!(strip_ns(&cur), "lava" | "flowing_lava") {
                        lava_nudges.push((pos, cur));
                    }
                }
                cur_y -= 1;
            }

            if cur_y < min_y {
                state.rifts[rift_idx].active_column = None;
            } else {
                state.rifts[rift_idx].active_column = Some((col, cur_y, min_y));
            }
        }
    }

    if !edit.runs.is_empty() {
        let _ = host.edit_blocks(OVERWORLD, &edit);
    }
    if !biome_updates.is_empty() {
        rift_bridge::enqueue_biomes(&biome_updates);
        rift_bridge::record_quart_cells(&quart_records);
    }
    for (p, fluid) in lava_nudges {
        rift_bridge::enqueue_fast_lava(p, fluid);
    }
}

fn push_edit(
    edit: &mut NativeBlockEdit,
    palette_map: &mut HashMap<&'static str, u32>,
    pos: [i32; 3],
    block: &'static str,
) {
    let idx = if let Some(&i) = palette_map.get(block) {
        i
    } else {
        let i = edit.palette.len() as u32;
        edit.palette.push(block.to_owned());
        palette_map.insert(block, i);
        i
    };
    edit.runs.push(NativeBlockRun {
        x: pos[0],
        z: pos[2],
        y: pos[1],
        height: 1,
        surface: None,
        block: idx,
        only: None,
    });
}

fn emit_rift_ambient_effects(host: &mut dyn Host, state: &RiftState) {
    if state.tick % 16 != 0 {
        return;
    }
    let Some(level) = host.level(OVERWORLD) else {
        return;
    };
    for r in &state.rifts {
        if r.contained || host.biome(r.center).is_none() {
            continue;
        }
        let at = [
            f64::from(r.center[0]) + 0.5,
            f64::from(r.center[1]) + 0.8,
            f64::from(r.center[2]) + 0.5,
        ];
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(ParticleType::Portal),
            None,
            at,
            [1.6, 1.4, 1.6],
            0.25,
            10,
        );
        let _ = level.send_particles(
            host,
            &ParticleOptions::simple(ParticleType::Flame),
            None,
            at,
            [1.2, 0.9, 1.2],
            0.03,
            4,
        );
        if state.tick % 160 == 0 {
            let _ = level.play_sound(
                host,
                "minecraft:block.portal.ambient",
                at,
                8,
                0.7,
                0.75,
            );
        }
    }
}

fn broadcast_overworld(host: &mut dyn Host, text: &str) {
    for player in host.server_players() {
        let _ = host.tell(player.uuid, Text::literal(text));
    }
}
