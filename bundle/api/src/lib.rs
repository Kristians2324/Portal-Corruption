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
    append_nether_touched_drops, augment_potential_spawns, pos_is_corrupted,
    should_block_zombification, tick_mobs,
};
use crate::state::{MAX_FRONTIER_PER_RIFT, RiftState};
use crate::types::{
    BEDROCK_FLOOR_Y, NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, RiftSource,
    dist_sq, horiz_dist_sq, neighbors_6, quart_block_origin, quart_coords, spatial_hash, strip_ns,
};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    BlockChangedEvent, BlockStateId, BucketEmptyEvent, ClientDisconnectingEvent,
    ClientLevelLoadEvent, ClientTickPostEvent, ComputeFogColorEvent, Flow, FluidLevelChangeEvent,
    Hooks, Host, LevelDimension, LevelHost, LevelTickPostEvent, LivingConversionPreEvent,
    LootDropsEvent, MusicEvent, NativeBlockEdit, NativeBlockRun, NativeCommand, NativeHeightmap,
    ParticleOptions, ParticleType, PerformCommandEvent, PortalCreateEvent, PotentialSpawnsEvent,
    RegisterCommandsEvent, RenderFogEvent, Text, WorldLifecycleEvent, WorldPhase, WorldSavedEvent,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

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
        // `biome(pos)` returns `None` when the column's chunk is unloaded.
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
                if let Ok(mut rt) = runtime().lock() {
                    rt.server.reset_for_world(world_dir, seed_hint);
                    let cells: Vec<[i32; 3]> = rt.server.corrupted_quarts.iter().copied().collect();
                    rift_bridge::record_quart_cells(&cells);
                }
            }
            WorldPhase::WorldCloseFinal | WorldPhase::WorldOpenFailed | WorldPhase::WorldCloseFailed => {
                if let Ok(mut rt) = runtime().lock() {
                    rt.server.dirty = true;
                    rt.server.save_to_disk();
                    rt.client.reset();
                }
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

        // Watch any change within 22 blocks of an existing source (for doors, seals, broken portals).
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
        process_water_evaporation(host, &mut local_server);
        spread_corruption_step(host, &mut local_server);
        emit_rift_ambient_effects(host, &local_server);
        tick_mobs(host, &mut local_server);
        tick_bastion_emergence(host, &mut local_server);

        if local_server.tick % 400 == 0 && local_server.dirty {
            local_server.save_to_disk();
        }

        if let Ok(mut rt) = runtime().lock() {
            // Merge any items pushed to queues during this tick while local_server ran.
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
                Text::literal("[Rift Bleed] Usage: /rift status | scan | pause | resume | speed <4..128>"),
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
                        let radius = ((r.max_radius_sq as f64).sqrt()) as i32;
                        let _ = host.tell(
                            event.player.uuid,
                            Text::literal(format!(
                                "  #{} [{} | {} | {}] at ({},{},{}) radius={} blocks={}",
                                r.id,
                                status,
                                kind,
                                r.flavor.display_name(),
                                r.center[0],
                                r.center[1],
                                r.center[2],
                                radius,
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
                    let clamped = val.clamp(4, 128);
                    if let Ok(mut rt) = runtime().lock() {
                        rt.server.spread_budget = clamped;
                        rt.server.dirty = true;
                    }
                    let _ = host.tell(
                        event.player.uuid,
                        Text::literal(format!("[Rift Bleed] Spread budget set to {clamped} blocks/tick.")),
                    );
                } else {
                    let _ = host.tell(
                        event.player.uuid,
                        Text::literal("[Rift Bleed] Usage: /rift speed <4..128>"),
                    );
                }
                Flow::Cancel
            }
            _ => Flow::Continue,
        }
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
        let mut frontier = Vec::new();
        if !contained {
            for s in &seeds {
                frontier.extend(neighbors_6(*s));
            }
        }
        state.rifts.push(RiftSource {
            id,
            center: centroid,
            active_portal: true,
            contained,
            flavor,
            age_ticks: 0,
            corrupted_blocks: 0,
            max_radius_sq: 0,
            frontier,
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
        // Lightweight spiral probe of up to 48 columns per cycle around player to catch
        // pre-existing or plugin-lit active portals without lagging.
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
                        for s in &seeds {
                            state.rifts[idx].frontier.extend(neighbors_6(*s));
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

    // Feed Hoglin-trampled coordinates into the nearest active rift frontier.
    while let Some(p) = state.pending_frontier_injections.pop_front() {
        if let Some(rift) = state
            .rifts
            .iter_mut()
            .filter(|r| !r.contained)
            .min_by_key(|r| dist_sq(r.center, p))
        {
            for n in neighbors_6(p) {
                rift.frontier.push(n);
            }
        }
    }

    let total_budget = state.spread_budget.max(4);
    let per_rift_budget = (total_budget / active_indices.len()).max(4);
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
    let mut staged_positions: HashSet<[i32; 3]> = HashSet::new();

    for rift_idx in active_indices {
        state.rifts[rift_idx].age_ticks += 1;

        if state.rifts[rift_idx].frontier.is_empty() {
            seed_fallback_frontier(host, &mut state.rifts[rift_idx]);
        }

        let rift_id = state.rifts[rift_idx].id;
        let rift_center = state.rifts[rift_idx].center;
        let rift_flavor = state.rifts[rift_idx].flavor;
        let rift_age = state.rifts[rift_idx].age_ticks;
        let rift_mature = state.rifts[rift_idx].corrupted_blocks >= 4_000;
        let seed = state.seed;

        let mut remaining = per_rift_budget;
        let mut attempts = per_rift_budget * 5;
        let mut deferred = Vec::new();

        while remaining > 0 && attempts > 0 {
            attempts -= 1;
            let len = state.rifts[rift_idx].frontier.len();
            if len == 0 {
                break;
            }
            // Pick pseudo-randomly from the frontier tail/middle for organic jagged tendrils.
            let pick = if len <= 16 {
                len - 1
            } else {
                let window = len.min(48);
                let h = spatial_hash(
                    rift_center[0],
                    attempts as i32,
                    rift_center[2],
                    rift_age ^ rift_id,
                );
                len - 1 - ((h as usize) % window)
            };
            let pos = state.rifts[rift_idx].frontier.swap_remove(pick);
            if pos[1] < BEDROCK_FLOOR_Y || pos[1] >= OVERWORLD_MAX_Y || !staged_positions.insert(pos)
            {
                continue;
            }

            let hdist = (horiz_dist_sq(pos, rift_center) as f64).sqrt() as i32;
            let min_y = allowed_min_y(
                rift_center[1],
                rift_age,
                hdist,
                state.rifts[rift_idx].corrupted_blocks,
            );
            if pos[1] < min_y {
                continue;
            }

            if host.biome(pos).is_none() {
                if deferred.len() < 256 {
                    deferred.push(pos);
                }
                continue;
            }

            let cur = host.block(pos);
            let above = host.block([pos[0], pos[1] + 1, pos[2]]);
            let below = host.block([pos[0], pos[1] - 1, pos[2]]);
            let local_flavor = if hdist < 26 {
                rift_flavor
            } else {
                NetherFlavor::pick_for_region(pos[0], pos[2], seed ^ rift_id)
            };

            let outcome = transform_block(pos, &cur, &above, &below, local_flavor, rift_mature);

            if let Some(new_block) = outcome.block {
                push_edit(&mut edit, &mut palette_map, pos, new_block);
                remaining -= 1;
                state.rifts[rift_idx].corrupted_blocks += 1;
                let r_sq = horiz_dist_sq(pos, rift_center);
                if r_sq > state.rifts[rift_idx].max_radius_sq {
                    state.rifts[rift_idx].max_radius_sq = r_sq;
                }
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

            if outcome.is_corrupt {
                if matches!(strip_ns(&cur), "lava" | "flowing_lava") {
                    lava_nudges.push((pos, cur));
                }
                if state.mark_quart_corrupted(pos) {
                    biome_updates.push((quart_block_origin(pos), local_flavor.to_u8()));
                    quart_records.push(quart_coords(pos));
                }
                expand_frontier(
                    host,
                    &mut state.rifts[rift_idx].frontier,
                    &staged_positions,
                    pos,
                    min_y,
                    rift_age ^ (attempts as u64),
                );
            }
        }

        if !deferred.is_empty() {
            state.rifts[rift_idx].frontier.extend(deferred);
        }

        if state.rifts[rift_idx].frontier.len() > MAX_FRONTIER_PER_RIFT {
            let excess = state.rifts[rift_idx].frontier.len() - MAX_FRONTIER_PER_RIFT;
            state.rifts[rift_idx].frontier.drain(0..excess);
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

fn expand_frontier(
    host: &dyn Host,
    frontier: &mut Vec<[i32; 3]>,
    staged: &HashSet<[i32; 3]>,
    origin: [i32; 3],
    min_y: i32,
    salt: u64,
) {
    for n in neighbors_6(origin) {
        if n[1] < min_y || n[1] >= OVERWORLD_MAX_Y || staged.contains(&n) {
            continue;
        }
        let nb = host.block(n);
        // Never waste frontier slots on open sky; hug solid terrain & surface vegetation.
        if !is_air_like(&nb) {
            frontier.push(n);
        }
    }

    // Also probe horizontal step-ups / step-downs (+/- 1..2 Y) so hillsides,
    // shallow streams, and forest canopies never block the creeping spread.
    let h = spatial_hash(origin[0], origin[1], origin[2], salt);
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        if let Ok(heights) = host.surface_heights(
            OVERWORLD,
            &[(origin[0] + dx, origin[2] + dz)],
            NativeHeightmap::WorldSurface,
        ) && let Some(Some(top)) = heights.first().copied()
        {
            let surf = [origin[0] + dx, (top - 1).max(min_y), origin[2] + dz];
            if !staged.contains(&surf) {
                frontier.push(surf);
            }
        }
        let step = ((h & 3) as i32) - 1;
        let diag = [
            origin[0] + dx * 2,
            (origin[1] + step).clamp(min_y, OVERWORLD_MAX_Y - 1),
            origin[2] + dz * 2,
        ];
        if !staged.contains(&diag) && !is_air_like(&host.block(diag)) {
            frontier.push(diag);
        }
    }
}

fn seed_fallback_frontier(host: &dyn Host, rift: &mut RiftSource) {
    let rad = ((rift.max_radius_sq as f64).sqrt() as i32).max(8);
    let min_y = allowed_min_y(rift.center[1], rift.age_ticks, rad / 2, rift.corrupted_blocks);

    for i in 0..24u64 {
        let h = spatial_hash(rift.center[0], i as i32, rift.center[2], rift.age_ticks ^ rift.id);
        let angle = ((h & 0xFFFF) as f64 / 65535.0) * std::f64::consts::TAU;
        let r = (((h >> 16) % ((rad as u64).max(12) + 8)) as f64).max(4.0);
        let x = rift.center[0] + (angle.cos() * r).round() as i32;
        let z = rift.center[2] + (angle.sin() * r).round() as i32;
        if let Ok(heights) =
            host.surface_heights(OVERWORLD, &[(x, z)], NativeHeightmap::MotionBlockingNoLeaves)
            && let Some(Some(sy)) = heights.first().copied()
        {
            let p = [x, (sy - 1).max(min_y), z];
            rift.frontier.push(p);
        }
        // Also seed underground probe at min_y..center_y so caves continue deeper.
        let y_range = (rift.center[1] - min_y).max(1) as u64;
        let uy = min_y + ((h >> 32) % y_range) as i32;
        rift.frontier.push([x, uy, z]);
    }
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
