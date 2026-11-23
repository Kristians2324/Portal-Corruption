use crate::bastion::{all_players_far_from, is_fillable_terrain_void, overworld_player_positions};
use crate::corruption::{is_nether_terrain_block, is_water_like};
use crate::mobs::{is_piglin_or_hoglin, paint_biome_column};
use crate::state::{ConnectedWasteland, PendingOutpost, RiftState};
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, OutpostKind, OutpostSite,
    RiftSource, horiz_dist_sq, spatial_hash, strip_ns,
};
use mod_api::{
    BlockEdit, BlockRun, BlockState, BlockUpdateFlags, Compound, EquipmentSlot, Heightmap,
    Identifier, ItemStack, LevelRef, ServerHost, SoundSource, Tag, Text,
};

const OUTPOST_CHECK_INTERVAL_TICKS: u64 = 60;
const OUTPOST_OFFSET_TICK: u64 = 30;
const MIN_OUTPOST_PORTAL_DIST: i64 = 48;
const MIN_OUTPOST_SPACING: i64 = 60;
const MIN_OUTPOST_BASTION_SPACING: i64 = 56;
const MIN_PLAYER_DIST_FOR_OUTPOST: i64 = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum OMat {
    Air = 0,
    Blackstone = 1,
    PolishedBricks = 2,
    CrackedBricks = 3,
    ChiseledBlackstone = 4,
    GildedBlackstone = 5,
    Obsidian = 6,
    CryingObsidian = 7,
    Basalt = 8,
    PolishedBasalt = 9,
    RedNetherBricks = 10,
    NetherBricks = 11,
    CrimsonStem = 12,
    StrippedCrimsonStem = 13,
    CrimsonPlanks = 14,
    CrimsonFence = 15,
    SoulSoil = 16,
    SoulSand = 17,
    Magma = 18,
    Shroomlight = 19,
    NetherWartBlock = 20,
    IronBars = 21,
    Chain = 22,
    Campfire = 23,
    SoulCampfire = 24,
    SoulFire = 25,
    GoldBlock = 26,
    IronBlock = 27,
    AncientDebris = 28,
    Lava = 29,
    SoulLantern = 30,
    HayBlock = 31,
    WarpedStem = 32,
    StrippedWarpedStem = 33,
    WarpedPlanks = 34,
    WarpedFence = 35,
    WarpedWartBlock = 36,
    BoneBlock = 37,
    DeepslateBricks = 38,
    PolishedDeepslate = 39,
    SmoothBasalt = 40,
    Glowstone = 41,
}

pub fn wasteland_eligible_for_outpost(
    rift: &RiftSource,
    wasteland: ConnectedWasteland,
) -> Option<OutpostKind> {
    if rift.contained || rift.pacified || rift.regressing {
        return None;
    }

    // Dynamic scaling: outposts unlock early and scale briskly with corrupted area
    // so vast corrupted regions pack in tons of diverse Nether structures.
    let (min_blocks, min_ring) = match wasteland.total_outposts_placed {
        0 => (45_000, 48),
        1 => (75_000, 56),
        2 => (110_000, 64),
        n => {
            let extra = (n - 2) as u64;
            (
                110_000 + extra * 34_000,
                64 + (((extra as f64).sqrt() * 11.0).round() as i32),
            )
        }
    };
    if wasteland.total_corrupted_blocks < min_blocks || wasteland.max_ring_radius < min_ring {
        return None;
    }

    // Cycle smoothly across all six unique Nether invasion archetypes.
    let idx = ((rift.outposts_placed as u64 + (rift.id % 6)) % 6) as u8;
    Some(OutpostKind::from_u8(idx))
}

pub fn site_clear_for_outpost(state: &RiftState, cx: i32, cz: i32) -> bool {
    let p_sq = MIN_OUTPOST_PORTAL_DIST * MIN_OUTPOST_PORTAL_DIST;
    let o_sq = MIN_OUTPOST_SPACING * MIN_OUTPOST_SPACING;
    let b_sq = MIN_OUTPOST_BASTION_SPACING * MIN_OUTPOST_BASTION_SPACING;
    let probe = [cx, 64, cz];

    if horiz_dist_sq(probe, [0, 64, 0]) < 36 * 36 {
        return false;
    }
    let clear_rifts = state
        .rifts
        .iter()
        .all(|r| horiz_dist_sq(r.center, probe) >= p_sq);
    let clear_citadels = state
        .citadels
        .iter()
        .all(|c| horiz_dist_sq(*c, probe) >= p_sq);
    let clear_outposts = state
        .outposts
        .iter()
        .all(|o| horiz_dist_sq(o.center, probe) >= o_sq);
    let clear_bastions = state
        .bastions
        .iter()
        .all(|b| horiz_dist_sq(b.center, probe) >= b_sq);

    clear_rifts && clear_citadels && clear_outposts && clear_bastions
}

pub fn release_outpost_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if let Some(pending) = state.pending_outpost.take() {
        for c in pending.forced_chunks {
            if let Some(cref) = level.chunk_ref(host, c) {
                let _ = cref.set_forced(host, false);
            }
        }
    }
    for c in state.outpost_release_chunks.drain(..) {
        if let Some(cref) = level.chunk_ref(host, c) {
            let _ = cref.set_forced(host, false);
        }
    }
    state.outpost_release_ttl = 0;
}

pub fn tick_outposts(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if state.outpost_release_ttl > 0 {
        state.outpost_release_ttl -= 1;
        if state.outpost_release_ttl == 0 {
            for c in state.outpost_release_chunks.drain(..) {
                if let Some(cref) = level.chunk_ref(host, c) {
                    let _ = cref.set_forced(host, false);
                }
            }
        }
    }

    if state.pending_outpost.is_some() {
        process_pending_outpost(host, level, state);
        return;
    }

    if state.paused || state.tick % OUTPOST_CHECK_INTERVAL_TICKS != OUTPOST_OFFSET_TICK {
        return;
    }
    if state.rifts.is_empty() {
        return;
    }

    let player_positions = overworld_player_positions(host);
    let len = state.rifts.len();
    let start = ((state.tick / OUTPOST_CHECK_INTERVAL_TICKS) as usize) % len;

    for off in 0..len {
        let idx = (start + off) % len;
        let wasteland = state.connected_wasteland_for(idx);
        let rift = &state.rifts[idx];
        let Some(kind) = wasteland_eligible_for_outpost(rift, wasteland) else {
            continue;
        };

        let half = outpost_half_extent(kind);
        let radius = ((rift.max_radius_sq.max(wasteland.total_area_radius_sq) as f64).sqrt() as i32)
            .max(rift.ring_radius);
        let inner = (MIN_OUTPOST_PORTAL_DIST as i32) + 4;
        let outer = (radius - (half / 2 + 2)).max(inner + 4);
        if radius < inner + 4 {
            continue;
        }

        let mut picked: Option<([i32; 3], u64)> = None;
        for attempt in 0..48u64 {
            let h = spatial_hash(
                rift.center[0],
                rift.outposts_placed as i32,
                rift.center[2],
                state.seed ^ state.tick ^ attempt.wrapping_mul(0x517C_C1B7_2722_0A95),
            );
            let angle = ((h & 0xFFFF) as f64 / 65535.0) * std::f64::consts::TAU;
            let span = (outer - inner + 1).max(1) as u64;
            let dist = inner + (((h >> 16) % span) as i32);
            let cx = rift.center[0] + (angle.cos() * f64::from(dist)).round() as i32;
            let cz = rift.center[2] + (angle.sin() * f64::from(dist)).round() as i32;

            if !site_clear_for_outpost(state, cx, cz) {
                continue;
            }
            if !all_players_far_from(&player_positions, [cx, 64, cz], MIN_PLAYER_DIST_FOR_OUTPOST) {
                continue;
            }
            if !state.is_quart_corrupted([cx, rift.center[1], cz]) {
                continue;
            }
            let corners_loaded = [(-half, -half), (-half, half), (half, -half), (half, half), (0, 0)]
                .into_iter()
                .all(|(dx, dz)| level.is_loaded(host, [cx + dx, 64, cz + dz]));
            if corners_loaded {
                let dummy = PendingOutpost {
                    rift_id: rift.id,
                    center: [cx, rift.center[1], cz],
                    kind,
                    flavor: rift.flavor,
                    roll: h,
                    forced_chunks: Vec::new(),
                    queued_tick: state.tick,
                };
                if validate_and_resolve_outpost_floor(host, level, state, &dummy).is_none() {
                    continue;
                }
            }
            picked = Some(([cx, rift.center[1], cz], h));
            break;
        }

        if let Some((center, roll)) = picked {
            stage_pending_outpost(host, level, state, rift.id, center, kind, rift.flavor, roll);
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_pending_outpost(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_id: u64,
    center: [i32; 3],
    kind: OutpostKind,
    flavor: NetherFlavor,
    roll: u64,
) {
    let half = outpost_half_extent(kind) + 6;
    let min_cx = (center[0] - half) >> 4;
    let max_cx = (center[0] + half) >> 4;
    let min_cz = (center[2] - half) >> 4;
    let max_cz = (center[2] + half) >> 4;

    let mut forced_chunks = Vec::new();
    for ccx in min_cx..=max_cx {
        for ccz in min_cz..=max_cz {
            let chunk = [ccx, ccz];
            if let Some(cref) = level.chunk_ref(host, chunk)
                && cref.set_forced(host, true)
            {
                forced_chunks.push(chunk);
            }
        }
    }

    state.pending_outpost = Some(PendingOutpost {
        rift_id,
        center,
        kind,
        flavor,
        roll,
        forced_chunks,
        queued_tick: state.tick,
    });
}

fn process_pending_outpost(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    let Some(pending) = state.pending_outpost.clone() else {
        return;
    };

    let Some(rift_idx) = state.rifts.iter().position(|r| r.id == pending.rift_id) else {
        release_outpost_chunks(host, level, state);
        return;
    };
    if state.rifts[rift_idx].contained
        || state.rifts[rift_idx].pacified
        || !site_clear_for_outpost(state, pending.center[0], pending.center[2])
    {
        release_outpost_chunks(host, level, state);
        return;
    }

    let players = overworld_player_positions(host);
    if !all_players_far_from(&players, pending.center, MIN_PLAYER_DIST_FOR_OUTPOST) {
        release_outpost_chunks(host, level, state);
        return;
    }

    if state.tick.saturating_sub(pending.queued_tick) > 240 {
        release_outpost_chunks(host, level, state);
        return;
    }

    let all_loaded = !pending.forced_chunks.is_empty()
        && pending
            .forced_chunks
            .iter()
            .all(|[cx, cz]| level.is_loaded(host, [cx << 4, 64, cz << 4]));
    if !all_loaded {
        return;
    }

    state.pending_outpost = None;

    let Some(floor_y) = validate_and_resolve_outpost_floor(host, level, state, &pending) else {
        state.outpost_release_chunks = pending.forced_chunks;
        state.outpost_release_ttl = 6;
        return;
    };

    build_outpost_structure(
        host,
        level,
        state,
        pending.center[0],
        floor_y,
        pending.center[2],
        pending.kind,
        pending.flavor,
        pending.roll,
    );

    if let Some(rift) = state.rifts.iter_mut().find(|r| r.id == pending.rift_id) {
        rift.outposts_placed += 1;
    }
    state.outposts.push(OutpostSite {
        center: [pending.center[0], floor_y, pending.center[2]],
        kind: pending.kind,
        flavor: pending.flavor,
    });
    state.outpost_release_chunks = pending.forced_chunks;
    state.outpost_release_ttl = 6;
    state.dirty = true;

    let msg = format!(
        "[Rift Bleed] A colossal {} has risen from the blight near ({}, {}, {})!",
        pending.kind.display_name(),
        pending.center[0],
        floor_y,
        pending.center[2]
    );
    for player in host.players().server_players() {
        if player
            .dimension(host)
            .is_some_and(|d| d.to_string() == OVERWORLD)
        {
            let _ = player.tell(host, Text::literal(msg.clone()));
            if let Some(ppos) = player.entity().position(host)
                && horiz_dist_sq([ppos[0] as i32, 64, ppos[2] as i32], pending.center) <= 320 * 320
            {
                let _ = level.play_sound(
                    host,
                    "minecraft:block.anvil.place",
                    ppos,
                    SoundSource::Hostile,
                    0.5,
                    0.6,
                );
            }
        }
    }
}

pub fn outpost_half_extent(kind: OutpostKind) -> i32 {
    match kind {
        OutpostKind::HoglinVillage => 22,
        OutpostKind::InfernalFoundry => 21,
        OutpostKind::SiegeWatchtower => 18,
        OutpostKind::BoneCatacomb => 21,
        OutpostKind::ArenaPit => 23,
        OutpostKind::AlchemistSpire => 20,
    }
}

fn validate_and_resolve_outpost_floor(
    host: &dyn ServerHost,
    ow: LevelRef,
    state: &RiftState,
    pending: &PendingOutpost,
) -> Option<i32> {
    let half = outpost_half_extent(pending.kind);
    let step = (half / 2).max(6);
    let mut probe_cols = Vec::with_capacity(9);
    for dx in [-step, 0, step] {
        for dz in [-step, 0, step] {
            probe_cols.push((pending.center[0] + dx, pending.center[2] + dz));
        }
    }

    let hs: Vec<i32> = host
        .surface_heights(OVERWORLD, &probe_cols, Heightmap::MotionBlockingNoLeaves)
        .ok()?
        .into_iter()
        .flatten()
        .collect();
    if hs.len() < probe_cols.len() {
        return None;
    }

    let min_h = *hs.iter().min()?;
    let max_h = *hs.iter().max()?;
    if max_h - min_h > 24 || min_h < 42 || max_h > 190 {
        return None;
    }

    let mut corrupted_hits = 0usize;
    let mut water_hits = 0usize;
    let mut lava_hits = 0usize;
    for (idx, (px, pz)) in probe_cols.iter().enumerate() {
        let p = [*px, hs[idx] - 1, *pz];
        if state.is_quart_corrupted(p) {
            corrupted_hits += 1;
        }
        if let Some(key) = ow.block(host, p) {
            let name = key.id(host);
            let s = strip_ns(&name);
            if is_water_like(&name) {
                water_hits += 1;
            } else if matches!(s, "lava" | "flowing_lava") {
                lava_hits += 1;
            } else if is_nether_terrain_block(&name)
                || matches!(
                    s,
                    "obsidian" | "basalt" | "blackstone" | "magma_block" | "polished_basalt"
                )
            {
                corrupted_hits += 1;
            }
        }
        if let Some(top) = ow.block(host, [*px, hs[idx], *pz])
            && is_water_like(&top.id(host))
        {
            water_hits += 1;
        }
    }

    if water_hits > 1 || lava_hits > 3 || corrupted_hits < 5 {
        return None;
    }

    let mut sorted = hs;
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2] - 1;
    Some(median.clamp(OVERWORLD_MIN_Y + 8, OVERWORLD_MAX_Y - 36))
}

#[inline]
fn push_range(u: &mut Vec<(i32, OMat)>, y0: i32, y1: i32, mat: OMat) {
    for dy in y0..=y1 {
        u.push((dy, mat));
    }
}

#[inline]
fn push_brazier_top(u: &mut Vec<(i32, OMat)>, top_solid_y: i32, base: OMat, flame: OMat) {
    u.push((top_solid_y, base));
    u.push((top_solid_y + 1, flame));
}

#[inline]
fn push_hanging_chandelier(u: &mut Vec<(i32, OMat)>, ceiling_y: i32, chain_blocks: i32) {
    for off in 1..=chain_blocks {
        u.push((ceiling_y - off, OMat::Chain));
    }
    u.push((ceiling_y - chain_blocks - 1, OMat::Shroomlight));
}

#[inline]
fn push_enclosed_chimney_col(
    u: &mut Vec<(i32, OMat)>,
    sdx: i32,
    sdz: i32,
    base_y: i32,
    rim_h: i32,
    firebox_open_south: bool,
) {
    let edge = sdx == 2 || sdz == 2;
    let corner = sdx == 2 && sdz == 2;
    if edge {
        let top = if corner { rim_h - 2 } else { rim_h };
        for dy in base_y..=top {
            let m = if firebox_open_south && dy <= 2 && (sdx == 0 || sdz == 0) {
                OMat::IronBars
            } else if dy % 5 == 0 {
                OMat::RedNetherBricks
            } else if dy >= rim_h - 2 {
                OMat::Obsidian
            } else {
                OMat::PolishedBasalt
            };
            u.push((dy, m));
        }
    } else if sdx == 1 || sdz == 1 {
        // Inner brick & obsidian flue sheath seals the shaft solidly from floor to crown.
        for dy in base_y..=(rim_h - 1) {
            let m = if firebox_open_south && dy == 2 && sdx == 0 {
                OMat::IronBars
            } else if dy <= 4 || dy >= rim_h - 3 {
                OMat::Obsidian
            } else {
                OMat::Blackstone
            };
            u.push((dy, m));
        }
    } else {
        // Core center: lava firebox behind bottom grate, solid masonry flue, recessed embers 2 blocks below rim!
        u.push((1, OMat::Lava));
        // dy 2..=3 open firebox chamber under solid flue hood starting at dy=4 (connected horizontally to inner ring).
        push_range(u, 4, rim_h - 4, OMat::Blackstone);
        u.push((rim_h - 3, OMat::Magma));
        u.push((rim_h - 2, OMat::Campfire));
    }
}

/// Pure column geometry blueprint shared across all six large Nether invasion archetypes.
fn plan_outpost_column(
    kind: OutpostKind,
    dx: i32,
    dz: i32,
    roll: u64,
) -> (OMat, Vec<(i32, OMat)>) {
    match kind {
        OutpostKind::HoglinVillage => plan_hoglin_village_col(dx, dz, roll),
        OutpostKind::InfernalFoundry => plan_infernal_foundry_col(dx, dz, roll),
        OutpostKind::SiegeWatchtower => plan_siege_watchtower_col(dx, dz, roll),
        OutpostKind::BoneCatacomb => plan_bone_catacomb_col(dx, dz, roll),
        OutpostKind::ArenaPit => plan_arena_pit_col(dx, dz, roll),
        OutpostKind::AlchemistSpire => plan_alchemist_spire_col(dx, dz, roll),
    }
}

fn plan_hoglin_village_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    let on_war_road = (ax <= 2 && dz >= -8) || (az <= 2 && ax <= 20);
    let in_longhouse = (-13..=13).contains(&dx) && (-19..=-9).contains(&dz);
    let in_pens = (-19..=-9).contains(&dx) && (-6..=10).contains(&dz);
    let in_war_hall = (9..=19).contains(&dx) && (-6..=10).contains(&dz);
    let in_vault = (-6..=6).contains(&dx) && (11..=18).contains(&dz);

    let floor = if on_war_road {
        if roll % 5 == 0 {
            OMat::GildedBlackstone
        } else {
            OMat::CrackedBricks
        }
    } else if in_longhouse || in_war_hall || in_vault {
        OMat::PolishedBricks
    } else if in_pens {
        if roll % 2 == 0 {
            OMat::SoulSand
        } else {
            OMat::SoulSoil
        }
    } else if roll % 8 == 0 {
        OMat::Magma
    } else if roll % 3 == 0 {
        OMat::SoulSoil
    } else {
        OMat::Blackstone
    };

    // 1. Massive Perimeter Palisade (cheb == 21..=22) + inner sentry wall-walk (cheb == 20).
    let south_gate = dz >= 20 && ax <= 2;
    let west_gate = dx <= -20 && az <= 2;
    let east_gate = dx >= 20 && az <= 2;
    let in_gate_axis = south_gate || west_gate || east_gate;
    if cheb >= 21 {
        let gate_tower =
            (dz >= 20 && (3..=4).contains(&ax)) || (ax >= 20 && (3..=4).contains(&az));
        let corner_bastion = ax >= 19 && az >= 19;
        if in_gate_axis {
            push_range(&mut u, 5, 6, OMat::Obsidian);
            u.push((7, OMat::PolishedBricks));
        } else if corner_bastion {
            push_range(&mut u, 1, 8, OMat::Blackstone);
            push_brazier_top(&mut u, 9, OMat::Obsidian, OMat::SoulCampfire);
        } else if gate_tower {
            push_range(&mut u, 1, 7, OMat::PolishedBricks);
            push_brazier_top(&mut u, 8, OMat::Obsidian, OMat::SoulCampfire);
        } else {
            let spike = (dx + dz).abs() % 3 == 0;
            let wall_h = if spike { 7 } else { 6 };
            for dy in 1..=wall_h {
                let m = if dy == wall_h && spike {
                    OMat::Obsidian
                } else if dy == 3 && !spike && (ax % 2 == 0 || az % 2 == 0) {
                    OMat::IronBars
                } else if (dx % 4 == 0 && az >= 21) || (dz % 4 == 0 && ax >= 21) {
                    OMat::StrippedCrimsonStem
                } else if roll % 3 == 0 {
                    OMat::CrackedBricks
                } else {
                    OMat::Blackstone
                };
                u.push((dy, m));
            }
        }
        return (floor, u);
    }
    if cheb == 20 && !in_gate_axis {
        // Supported perimeter wall-walk catwalk at dy = 4.
        let buttress = (dx + dz).abs() % 3 == 0 || ax == 19 || az == 19;
        if buttress {
            push_range(&mut u, 1, 3, OMat::Blackstone);
        }
        u.push((4, OMat::PolishedBricks));
        return (floor, u);
    }

    // Courtyard rampart staircase climbing smoothly onto West sentry walkway (cheb == 20).
    if dx == -19 && (13..=16).contains(&dz) {
        let h = dz - 12; // 1..=4
        push_range(&mut u, 1, h, OMat::CrackedBricks);
        return (floor, u);
    }

    // 2. Grand Chieftain & Household Mead Longhouse (North Wing: dx in -13..=13, dz in -19..=-9).
    if in_longhouse {
        let on_wall = ax == 13 || dz == -19 || dz == -9;
        let doorway = dz == -9 && ax <= 2;
        let dist_from_ridge = (dz + 14).abs(); // 0..=5
        let roof_y = 11 - dist_from_ridge; // 6 at eaves .. 11 at ridge
        let in_chimney = ax <= 1 && (-15..=-13).contains(&dz);

        if in_chimney {
            if ax == 0 && dz == -14 {
                u.push((1, OMat::Campfire));
                push_range(&mut u, 4, 12, OMat::Blackstone);
                u.push((13, OMat::Magma));
                u.push((14, OMat::Campfire));
            } else if ax == 0 && dz == -13 {
                u.push((1, OMat::Blackstone));
                u.push((2, OMat::IronBars));
                push_range(&mut u, 3, 15, OMat::PolishedBasalt);
            } else {
                push_range(&mut u, 1, 15, OMat::PolishedBasalt);
            }
            return (floor, u);
        }

        if on_wall {
            for dy in 1..=(roof_y - 2) {
                if doorway && dy <= 3 {
                    continue;
                }
                let m = if ax == 13 && (dz == -19 || dz == -9) {
                    OMat::CrimsonStem
                } else if dy == 2 && ax % 4 == 0 && !doorway {
                    OMat::IronBars
                } else if dy == 1 {
                    OMat::CrackedBricks
                } else {
                    OMat::CrimsonPlanks
                };
                u.push((dy, m));
            }
        }
        // Two-block-thick pitched roof eliminates diagonal seams and sky pinholes.
        u.push((roof_y - 1, OMat::CrimsonPlanks));
        u.push((
            roof_y,
            if ax % 4 == 0 {
                OMat::StrippedCrimsonStem
            } else {
                OMat::PolishedBasalt
            },
        ));
        if roof_y == 11 && ax % 8 == 0 && ax > 0 {
            u.push((12, OMat::SoulLantern));
        }

        // Interior furnishings: Chieftain Mezzanine Loft (East), Household Sleeping Wallows (West).
        if !on_wall {
            if (7..=12).contains(&dx) && (-18..=-10).contains(&dz) {
                u.push((5, OMat::CrimsonPlanks));
                if dx == 7 && dz == -18 {
                    u.push((6, OMat::SoulLantern));
                }
            } else if (3..=6).contains(&dx) && dz == -10 {
                push_range(&mut u, 1, dx - 2, OMat::StrippedCrimsonStem);
            } else if (-12..=-4).contains(&dx)
                && (-18..=-12).contains(&dz)
                && !(dx == -10 && dz == -17)
            {
                if dx == -8 && dz.abs() >= 15 {
                    u.push((1, OMat::CrimsonFence));
                } else if dz <= -15 || dz == -12 {
                    u.push((
                        1,
                        if (dx + dz) % 2 == 0 {
                            OMat::HayBlock
                        } else {
                            OMat::NetherWartBlock
                        },
                    ));
                }
            }
            if (dx == -6 || dx == 5) && dz == -14 {
                push_hanging_chandelier(&mut u, roof_y - 1, 1);
            }
        }
        return (floor, u);
    }

    // 3. West Sunken Hoglin Feeding Pens & Covered Lean-To (dx in -19..=-9, dz in -6..=10).
    if in_pens {
        let on_fence = dx == -19 || dx == -9 || dz == -6 || dz == 10;
        let pen_gate = dx == -9 && az <= 1;
        let in_shed = (-19..=-15).contains(&dx) && (-5..=4).contains(&dz);
        if in_shed {
            let corner_post = (dx == -19 || dx == -15) && (dz == -5 || dz == 4);
            if corner_post {
                push_range(&mut u, 1, 4, OMat::CrimsonStem);
            } else if dx == -18 && (dz == 2 || dz == 3) {
                u.push((1, OMat::HayBlock));
            }
            u.push((4, OMat::CrimsonPlanks));
            if dx <= -16 {
                u.push((5, OMat::StrippedCrimsonStem));
            }
            return (floor, u);
        }
        if on_fence && !pen_gate {
            u.push((
                1,
                if (dx + dz) % 3 == 0 {
                    OMat::Basalt
                } else {
                    OMat::CrimsonFence
                },
            ));
            if (dx + dz) % 6 == 0 {
                u.push((2, OMat::SoulLantern));
            }
        } else if (-13..=-11).contains(&dx) && (-3..=6).contains(&dz) {
            if dz == -2 || dz == 4 {
                u.push((1, OMat::HayBlock));
            } else if dx == -12 && dz == 1 {
                u.push((1, OMat::Magma));
                u.push((2, OMat::Campfire));
            }
        }
        return (floor, u);
    }

    // 4. East Two-Story War Hall & Armory (dx in 9..=19, dz in -6..=10).
    if in_war_hall {
        let on_wall = dx == 9 || dx == 19 || dz == -6 || dz == 10;
        let war_door = dx == 9 && az <= 1;
        if on_wall {
            for dy in 1..=6 {
                if war_door && dy <= 3 {
                    continue;
                }
                let m = if (dx == 9 || dx == 19) && (dz == -6 || dz == 10) {
                    OMat::Obsidian
                } else if dy == 3 && ((dx == 19 && dz % 3 == 0) || (dx == 14 && (dz == -6 || dz == 10))) {
                    OMat::IronBars
                } else if dy == 6 {
                    OMat::RedNetherBricks
                } else {
                    OMat::PolishedBricks
                };
                u.push((dy, m));
            }
            if (dx + dz) % 2 == 0 {
                u.push((7, OMat::Blackstone));
            }
        }
        u.push((
            6,
            if dx % 2 == 0 {
                OMat::PolishedBricks
            } else {
                OMat::PolishedBasalt
            },
        ));
        u.push((7, OMat::Blackstone));
        if !on_wall {
            if (13..=15).contains(&dx) && (1..=3).contains(&dz) {
                u.push((
                    1,
                    if dx == 14 && dz == 2 {
                        OMat::GildedBlackstone
                    } else {
                        OMat::ChiseledBlackstone
                    },
                ));
            }
            if dx == 14 && (dz == -2 || dz == 6) {
                push_hanging_chandelier(&mut u, 6, 1);
            }
        }
        return (floor, u);
    }

    // 5. South Grated Obsidian Treasury Bunker (dx in -6..=6, dz in 11..=18).
    if in_vault {
        let on_wall = ax == 6 || dz == 11 || dz == 18;
        let vault_door = dz == 11 && ax <= 1;
        if on_wall {
            for dy in 1..=4 {
                if vault_door && dy <= 3 {
                    continue;
                }
                u.push((
                    dy,
                    if ax == 6 || dz == 18 {
                        OMat::Obsidian
                    } else if dy <= 2 {
                        OMat::IronBars
                    } else {
                        OMat::GildedBlackstone
                    },
                ));
            }
        }
        u.push((5, OMat::Obsidian));
        if ax <= 5 && (12..=17).contains(&dz) {
            u.push((6, OMat::GildedBlackstone));
        }
        if dx == 0 && dz == 15 {
            u.push((1, OMat::GoldBlock));
            u.push((2, OMat::CryingObsidian));
            u.push((3, OMat::Shroomlight));
        }
        return (floor, u);
    }

    // Central Courtyard Chieftain War-Pyre Totem.
    if ax <= 2 && az <= 2 {
        if dx == 0 && dz == 0 {
            u.push((1, OMat::Magma));
            u.push((2, OMat::SoulCampfire));
        } else if ax == 2 && az == 2 {
            push_range(&mut u, 1, 3, OMat::StrippedCrimsonStem);
            u.push((4, OMat::SoulLantern));
        } else {
            u.push((1, OMat::ChiseledBlackstone));
        }
    }

    (floor, u)
}

fn plan_infernal_foundry_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    let in_slag_trench = (ax == 6 || ax == 7 || ax == 8) && (-8..=3).contains(&dz);
    let in_treasury = ax <= 6 && (10..=17).contains(&dz);
    let floor = if in_slag_trench || in_treasury {
        OMat::Obsidian
    } else if ax <= 2 || az <= 2 {
        OMat::PolishedBricks
    } else if roll % 5 == 0 {
        OMat::Magma
    } else {
        OMat::CrackedBricks
    };

    // 1. Twin Monumental Smokestacks & Enclosed Blast Fireboxes at (-12, -11) and (12, -11).
    for (sx, sz) in [(-12, -11), (12, -11)] {
        let sdx = (dx - sx).abs();
        let sdz = (dz - sz).abs();
        if sdx <= 2 && sdz <= 2 {
            let south_open = dz - sz > 0;
            push_enclosed_chimney_col(&mut u, sdx, sdz, 1, 26, south_open);
            return (floor, u);
        }
    }

    // 2. Heavy Outer Foundry Bastion Wall (cheb == 20..=21).
    let south_gate = dz >= 19 && ax <= 2;
    let east_west_gate = ax >= 19 && az <= 2;
    if cheb >= 20 {
        let corner_tower = ax >= 18 && az >= 18;
        let gate_tower =
            (dz >= 19 && (3..=4).contains(&ax)) || (ax >= 19 && (3..=4).contains(&az));
        if south_gate || east_west_gate {
            push_range(&mut u, 5, 7, OMat::Obsidian);
        } else if corner_tower {
            push_range(&mut u, 1, 10, OMat::Blackstone);
            push_brazier_top(&mut u, 11, OMat::Obsidian, OMat::SoulCampfire);
        } else if gate_tower {
            push_range(&mut u, 1, 8, OMat::PolishedBasalt);
            push_brazier_top(&mut u, 9, OMat::Obsidian, OMat::SoulCampfire);
        } else {
            for dy in 1..=7 {
                let m = if dy == 4 || dy == 7 {
                    OMat::RedNetherBricks
                } else if (dy == 2 || dy == 5) && (dx % 4 == 0 || dz % 4 == 0) {
                    OMat::IronBars
                } else {
                    OMat::Blackstone
                };
                u.push((dy, m));
            }
        }
        return (floor, u);
    }

    // 3. Covered Industrial Foundry Roof & Heavy Steel Truss Columns (dx in -18..=18, dz in -17..=8).
    let in_hall = ax <= 18 && (-17..=8).contains(&dz);
    if in_hall {
        let col_x = matches!(ax, 18 | 9 | 3);
        let col_z = matches!(dz, -17 | -11 | -5 | 1 | 7);
        if col_x && col_z {
            push_range(&mut u, 1, 11, OMat::PolishedBasalt);
        } else if (ax == 18 && (-17..=8).contains(&dz)) || (dz == -17 && ax <= 18) {
            // North & side curtain walls with industrial iron-bar clerestory bays.
            for dy in 1..=11 {
                let m = if dy >= 3 && dy <= 5 && ((dx + dz).abs() % 4 != 0) {
                    OMat::IronBars
                } else if dy == 8 {
                    OMat::RedNetherBricks
                } else {
                    OMat::PolishedBricks
                };
                u.push((dy, m));
            }
        }

        // Arched Factory Hall Roof (2 blocks thick, skylight bars, glowing suspended lanterns).
        let roof_y = if ax >= 13 {
            12
        } else if ax >= 6 {
            13
        } else {
            14
        };
        if ax == 6 && dz % 3 != 0 {
            u.push((roof_y - 1, OMat::PolishedBasalt));
            u.push((roof_y, OMat::IronBars));
        } else {
            u.push((roof_y - 1, OMat::PolishedBasalt));
            u.push((roof_y, OMat::Obsidian));
        }

        if (ax == 6 && (dz == -14 || dz == -3 || dz == 4)) || (dx == 0 && (dz == -14 || dz == 4)) {
            push_hanging_chandelier(&mut u, roof_y - 1, 1);
        }
    }

    // 4. Grounded Upper Gantry Walkways (dy = 6) + Twin Stepped Staircase Ramps (dz = 5).
    let on_main_gantry = ax <= 16 && (-5..=-1).contains(&dz);
    let on_side_spur = (14..=16).contains(&ax) && (0..=4).contains(&dz);
    if on_main_gantry || on_side_spur {
        let support_leg = (ax == 16 || ax == 10 || ax == 4) && (dz == -5 || dz == -1 || dz == 4);
        if support_leg {
            push_range(&mut u, 1, 5, OMat::PolishedBasalt);
        }
        u.push((6, OMat::PolishedBasalt));
        // Guard railings on open north/south bridge edges without blocking stairs or overlook chest.
        let rail_edge = (on_main_gantry && (dz == -5 || dz == -1) && ax > 3)
            || (on_side_spur && ax == 14 && dz < 4);
        if rail_edge {
            u.push((7, OMat::IronBars));
        }
    } else if (10..=15).contains(&ax) && dz == 5 {
        // Ascending stairs from ax=10 (dy=1) cleanly up to ax=15 (dy=6) joining the side spur.
        push_range(&mut u, 1, ax - 9, OMat::CrackedBricks);
    }

    // 5. Twin Enclosed Molten Slag Casting Trenches (ax in 6..=8, dz in -8..=3).
    if in_slag_trench {
        let rim = ax == 6 || ax == 8 || dz == -8 || dz == 3;
        if rim {
            u.push((1, OMat::Obsidian));
            if dz % 2 == 0 && ax != 7 {
                u.push((2, OMat::IronBars));
            }
        } else {
            u.push((1, OMat::Lava));
        }
        return (floor, u);
    }

    // 6. South Foreman Obsidian Treasury Bunker (dx in -6..=6, dz in 10..=17).
    if in_treasury {
        let wall = ax == 6 || dz == 10 || dz == 17;
        let door = dz == 10 && ax <= 1;
        if wall {
            for dy in 1..=5 {
                if door && dy <= 3 {
                    continue;
                }
                u.push((
                    dy,
                    if ax == 6 && (dz == 10 || dz == 17) {
                        OMat::Obsidian
                    } else if dy == 2 && ax == 3 && dz == 10 {
                        OMat::IronBars
                    } else {
                        OMat::PolishedBricks
                    },
                ));
            }
        }
        u.push((5, OMat::Obsidian));
        u.push((6, OMat::RedNetherBricks));
        if dx == 0 && dz == 14 {
            u.push((1, OMat::IronBlock));
            u.push((2, OMat::AncientDebris));
            u.push((3, OMat::GoldBlock));
        }
        return (floor, u);
    }

    // North Smelting Crucible Shrine (ax <= 2, dz == -14).
    if ax <= 1 && dz == -14 {
        u.push((
            1,
            if dx == 0 {
                OMat::GoldBlock
            } else {
                OMat::IronBlock
            },
        ));
    }

    (floor, u)
}

fn plan_siege_watchtower_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    let floor = if cheb <= 6 {
        OMat::PolishedBricks
    } else if ax <= 2 || az <= 2 {
        OMat::CrackedBricks
    } else if roll % 4 == 0 {
        OMat::Magma
    } else {
        OMat::Blackstone
    };

    // 1. Star-Keep Outer Curtain Wall & Sentry Rampart (cheb == 16..=18).
    let gate = (ax <= 2 && az >= 16) || (az <= 2 && ax >= 16);
    if cheb >= 17 {
        let corner = ax >= 14 && az >= 14;
        if gate {
            push_range(&mut u, 5, 6, OMat::Obsidian);
        } else if corner {
            push_range(&mut u, 1, 9, OMat::PolishedBricks);
            push_brazier_top(&mut u, 10, OMat::Obsidian, OMat::SoulCampfire);
        } else {
            let merlon = (dx + dz).abs() % 2 == 0;
            let top = if cheb == 18 && merlon { 6 } else { 5 };
            push_range(&mut u, 1, top, OMat::Blackstone);
        }
        return (floor, u);
    }
    if cheb == 16 && !gate {
        let pier = (dx + dz).abs() % 3 == 0 || ax == 15 || az == 15;
        if pier {
            push_range(&mut u, 1, 3, OMat::Blackstone);
        }
        u.push((4, OMat::PolishedBricks));
        return (floor, u);
    }
    if dx == -15 && (7..=10).contains(&dz) {
        push_range(&mut u, 1, dz - 6, OMat::CrackedBricks);
        return (floor, u);
    }

    // 2. Central Three-Story Siege Citadel Spire (cheb <= 6, rising to dy = 25).
    if cheb <= 6 {
        let tower_wall = cheb == 6;
        let door = (ax <= 1 && az == 6) || (az <= 1 && ax == 6);
        if tower_wall {
            let corner_buttress = ax >= 5 && az >= 5;
            let max_h = if corner_buttress { 25 } else { 22 };
            for dy in 1..=max_h {
                if door && dy <= 3 {
                    continue;
                }
                let m = if dy == 7 || dy == 14 || dy == 21 {
                    OMat::RedNetherBricks
                } else if dy >= 22 || (corner_buttress && dy % 5 == 0) {
                    OMat::Obsidian
                } else if (dy == 9 || dy == 10 || dy == 16 || dy == 17) && (ax <= 1 || az <= 1) {
                    OMat::IronBars
                } else {
                    OMat::PolishedBricks
                };
                u.push((dy, m));
            }
            if corner_buttress {
                push_brazier_top(&mut u, 25, OMat::CryingObsidian, OMat::SoulCampfire);
            }
            return (floor, u);
        }

        // Solid stairwell Ground (1) -> Floor 2 (7) along West inner wall dx = -5, dz in -3..=3.
        let on_stair_1 = dx == -5 && (-3..=3).contains(&dz);
        if on_stair_1 {
            push_range(&mut u, 1, dz + 4, OMat::CrackedBricks);
        }

        // Floor 2 Deck (dy = 7), leaving stairwell cutout open above (-5, -3..=2).
        let stair1_hole = dx == -5 && (-3..=2).contains(&dz);
        if !stair1_hole {
            u.push((7, OMat::NetherBricks));
        }

        // Solid stairwell Floor 2 (7) -> Floor 3 (14) along East inner wall dx = 5, dz in -3..=3.
        let on_stair_2 = dx == 5 && (-3..=3).contains(&dz);
        if on_stair_2 {
            push_range(&mut u, 8, 11 - dz, OMat::CrackedBricks);
        }

        // Floor 3 Deck (dy = 14), leaving stairwell cutout open above (5, -2..=3).
        let stair2_hole = dx == 5 && (-2..=3).contains(&dz);
        if !stair2_hole {
            u.push((14, OMat::PolishedBricks));
        }

        // Summit Battlement Roof (dy = 21) with central 3x3 eternal beacon skylight.
        if cheb >= 2 {
            u.push((21, OMat::Obsidian));
            if cheb >= 4 {
                u.push((22, OMat::PolishedBasalt));
            }
        }
        if (dx == 0 && dz == 2) || (dx == 0 && dz == -2) {
            push_hanging_chandelier(&mut u, 7, 1);
            push_hanging_chandelier(&mut u, 14, 1);
        }

        // Floor 3 Eternal Summit Beacon Altar at center (0, 0).
        if dx == 0 && dz == 0 {
            u.push((15, OMat::CryingObsidian));
            u.push((16, OMat::Shroomlight));
            push_brazier_top(&mut u, 17, OMat::SoulSoil, OMat::SoulFire);
        }
        return (floor, u);
    }

    // 3. Courtyard Siege Camps: SW Covered Barracks & NE Siege Pavilion + Catapult Timber Frame.
    let in_sw_barracks = (-14..=-8).contains(&dx) && (5..=11).contains(&dz);
    let in_ne_pavilion = (8..=14).contains(&dx) && (-11..=-5).contains(&dz);
    for (in_pav, rx0, rx1, rz0, rz1) in [
        (in_sw_barracks, -14, -8, 5, 11),
        (in_ne_pavilion, 8, 14, -11, -5),
    ] {
        if in_pav {
            let post = (dx == rx0 || dx == rx1) && (dz == rz0 || dz == rz1);
            if post {
                push_range(&mut u, 1, 4, OMat::CrimsonStem);
            }
            u.push((4, OMat::CrimsonPlanks));
            if (rx0 + 1..rx1).contains(&dx) && (rz0 + 1..rz1).contains(&dz) {
                u.push((5, OMat::RedNetherBricks));
            }
            return (floor, u);
        }
    }

    // NW Siege Ballista / Counterweight Frame (dx in -13..=-9, dz in -13..=-9).
    if (-13..=-9).contains(&dx) && (-13..=-9).contains(&dz) {
        if (dx == -13 || dx == -9) && (dz == -13 || dz == -9) {
            push_range(&mut u, 1, 4, OMat::StrippedCrimsonStem);
        } else if dx == -13 || dx == -9 || (dz == -11 && (-12..=-10).contains(&dx)) {
            u.push((4, OMat::StrippedCrimsonStem));
            if dx == -11 && dz == -11 {
                u.push((5, OMat::IronBlock));
            }
        }
        return (floor, u);
    }

    // SE Beast Pen & Muster Hearth.
    if dx == 11 && (8..=12).contains(&dz) {
        u.push((1, OMat::HayBlock));
    }
    if dx == -11 && dz == -4 {
        u.push((1, OMat::Magma));
        u.push((2, OMat::SoulCampfire));
    }

    (floor, u)
}

fn plan_bone_catacomb_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    let on_avenue = ax <= 3 || az <= 3;
    let floor = if on_avenue || cheb <= 10 {
        if roll % 6 == 0 {
            OMat::DeepslateBricks
        } else {
            OMat::PolishedBricks
        }
    } else if roll % 3 == 0 {
        OMat::SoulSand
    } else {
        OMat::SoulSoil
    };

    // 1. Outer Haunted Ossuary Colonnade (cheb == 19..=21).
    let gate = (ax <= 2 && dz >= 19) || (az <= 2 && ax >= 19) || (ax <= 2 && dz <= -19);
    if cheb >= 19 {
        let corner = ax >= 18 && az >= 18;
        if corner {
            push_range(&mut u, 1, 11, OMat::Obsidian);
            push_brazier_top(&mut u, 12, OMat::SoulSoil, OMat::SoulFire);
        } else if gate {
            push_range(&mut u, 6, 8, OMat::BoneBlock);
            u.push((9, OMat::DeepslateBricks));
        } else {
            let pier = (ax % 3 == 0 && az >= 19) || (az % 3 == 0 && ax >= 19) || cheb == 21;
            if pier {
                for dy in 1..=8 {
                    let m = if (ax % 3 == 0 || az % 3 == 0) && dy <= 7 {
                        OMat::BoneBlock
                    } else if dy == 3 && cheb == 21 {
                        OMat::IronBars
                    } else {
                        OMat::DeepslateBricks
                    };
                    u.push((dy, m));
                }
            }
            push_range(&mut u, 8, 9, OMat::DeepslateBricks);
            if (ax % 6 == 0 || az % 6 == 0) && cheb == 20 {
                push_brazier_top(&mut u, 10, OMat::SoulSoil, OMat::SoulFire);
            }
        }
        return (floor, u);
    }

    // 2. Sealed North Ossuary Crypt Mausoleum (dx in -4..=4, dz in -17..=-13).
    if ax <= 4 && (-17..=-13).contains(&dz) {
        let wall = ax == 4 || dz == -17 || dz == -13;
        let side_door = ax == 4 && dz == -15;
        if wall && !side_door {
            for dy in 1..=5 {
                let m = if dy % 2 == 0 && ax % 2 == 0 {
                    OMat::BoneBlock
                } else {
                    OMat::DeepslateBricks
                };
                u.push((dy, m));
            }
        }
        u.push((5, OMat::DeepslateBricks));
        u.push((6, OMat::BoneBlock));
        if ax <= 1 && (dz == -16 || dz == -14) {
            u.push((1, OMat::SoulSand));
        }
        return (floor, u);
    }

    // 3. Giant Fossilized Rib-Arches framing East/West/South approaches (connected solidly down to dy=0).
    if ((ax == 16 && az <= 4) || (dz == 16 && ax <= 4)) && !(ax == 0 && az == 16) {
        if ax == 4 || az == 4 {
            push_range(&mut u, 1, 8, OMat::BoneBlock);
        }
        push_range(&mut u, 7, 8, OMat::BoneBlock);
        return (floor, u);
    }

    // 4. Central Ziggurat Step-Pyramid, Undercrypt (dy=0..=3), and Upper Sanctum Terrace (dy=4).
    if (11..=13).contains(&cheb) {
        let crypt_tunnel = (ax <= 2 && dz >= 11) || (az <= 2 && ax >= 11);
        if crypt_tunnel {
            // Walk-in Undercrypt portal topped solidly by stepped ziggurat terrace overhead!
            push_range(&mut u, 4, 4 + (13 - cheb).min(1), OMat::DeepslateBricks);
        } else {
            let step_h = 14 - cheb; // 1, 2, 3
            push_range(&mut u, 1, step_h, OMat::DeepslateBricks);
            if ax == az {
                u.push((step_h + 1, OMat::SoulLantern));
            }
        }
        return (floor, u);
    }

    if cheb <= 10 {
        // Lower Undercrypt Sarcophagus Pillars & Upper Sanctum Terrace Deck at dy = 4.
        if (ax == 4 || ax == 9) && (az == 4 || az == 9) {
            push_range(&mut u, 1, 4, OMat::BoneBlock);
        } else if (ax == 7 && (-8..=-6).contains(&dz)) || (ax == 7 && (6..=8).contains(&dz)) {
            u.push((1, OMat::PolishedBricks));
            u.push((2, OMat::BoneBlock));
        }
        u.push((4, OMat::DeepslateBricks));

        // Terrace Corner Soul Obelisks.
        if ax >= 9 && az >= 9 {
            push_range(&mut u, 5, 9, OMat::BoneBlock);
            u.push((10, OMat::Obsidian));
            u.push((11, OMat::SoulLantern));
            return (floor, u);
        }

        // South Ziggurat Stairs on Terrace climbing from dy = 4 up onto High Reliquary dy = 8.
        if ax <= 1 && (4..=7).contains(&dz) {
            let reliquary_h = 12 - dz; // 7->5, 6->6, 5->7, 4->8
            push_range(&mut u, 5, reliquary_h, OMat::BoneBlock);
            return (floor, u);
        }

        // Upper Reliquary Sanctuary Pavilion (cheb <= 5) rising to dy = 21.
        if cheb <= 5 {
            if cheb == 5 {
                let col_or_wall = ax >= 4 || az >= 4 || (ax % 2 == 1 && az % 2 == 1);
                let upper_door = (ax <= 1 && az == 5) || (az <= 1 && ax == 5);
                for dy in 5..=12 {
                    if upper_door && dy <= 8 {
                        continue;
                    }
                    if col_or_wall || dy >= 11 {
                        u.push((
                            dy,
                            if dy == 8 || dy == 12 {
                                OMat::DeepslateBricks
                            } else {
                                OMat::BoneBlock
                            },
                        ));
                    }
                }
            }
            // High Inner Sanctum Altar Pedestal (cheb <= 3, deck at dy = 8).
            if cheb <= 3 {
                push_range(&mut u, 5, 8, OMat::PolishedBricks);
                if dx == 0 && dz == 0 {
                    u.push((9, OMat::CryingObsidian));
                    u.push((10, OMat::Shroomlight));
                    push_brazier_top(&mut u, 11, OMat::SoulSoil, OMat::SoulFire);
                }
            }
            // Stepped Vaulted Reliquary Crown Spire (dy = 12..=21, 100% 6-connected).
            let crown_base = 12 + (5 - cheb);
            let crown_top = if cheb <= 1 { 20 } else { crown_base + 1 };
            push_range(&mut u, crown_base, crown_top, OMat::Obsidian);
            if dx == 0 && dz == 0 {
                push_brazier_top(&mut u, 21, OMat::SoulSoil, OMat::SoulFire);
            }
        }
    }

    (floor, u)
}

fn plan_arena_pit_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let r_sq = dx * dx + dz * dz;
    let mut u = Vec::new();

    let in_hazard_ring = (49..=64).contains(&r_sq) && ax > 2 && az > 2;
    let in_south_bunker = ax <= 3 && (13..=17).contains(&dz);
    let floor = if in_hazard_ring || in_south_bunker {
        OMat::Obsidian
    } else if ax <= 2 || az <= 2 {
        OMat::PolishedBricks
    } else if r_sq <= 121 && roll % 5 == 0 {
        OMat::SmoothBasalt
    } else if r_sq <= 121 {
        OMat::CrackedBricks
    } else {
        OMat::Blackstone
    };

    // 1. Monumental Amphitheater Outer Wall Ring (r_sq in 400..=552, clamped by half=23).
    let in_cardinal_tunnel = (ax <= 2 && dz < 13) || az <= 2;
    let in_south_approach = (18..=23).contains(&dz) && ax <= 2;
    if r_sq >= 400 {
        if in_cardinal_tunnel || in_south_approach {
            push_range(&mut u, 6, 14, OMat::PolishedBricks);
            u.push((15, OMat::Obsidian));
        } else {
            let major_buttress = ax <= 3 || az <= 3 || (ax - az).abs() <= 2;
            let wall_top = if major_buttress { 16 } else { 14 };
            for dy in 1..=wall_top {
                let gallery_window = !major_buttress
                    && ((5..=7).contains(&dy) || (10..=12).contains(&dy))
                    && (dx + dz).abs() % 3 != 0;
                let m = if gallery_window {
                    OMat::IronBars
                } else if dy == 4 || dy == 9 || dy == 14 {
                    OMat::RedNetherBricks
                } else if dy >= 15 {
                    OMat::Obsidian
                } else {
                    OMat::PolishedBricks
                };
                u.push((dy, m));
            }
            if (ax - az).abs() == 0 && (16..=17).contains(&ax) {
                push_brazier_top(&mut u, 17, OMat::Magma, OMat::SoulCampfire);
            }
        }
        return (floor, u);
    }

    // 2. South Entrance Beastmaster Strongbox (dx in -3..=3, dz in 13..=17) with flanking arches.
    if in_south_bunker {
        let on_wall = ax == 3 || dz == 13 || dz == 17;
        let side_door = ax == 3 && dz == 15;
        if on_wall && !side_door {
            push_range(&mut u, 1, 4, OMat::Obsidian);
        }
        u.push((5, OMat::PolishedBricks));
        u.push((6, OMat::RedNetherBricks));
        return (floor, u);
    }

    // 3. Emperor's Grandstand Vault Canopy on North Bleachers (dx in -5..=5, dz in -19..=-13).
    if ax <= 5 && (-19..=-13).contains(&dz) {
        push_range(&mut u, 1, 7, OMat::PolishedBricks);
        let pillar = (ax == 5) && (dz == -19 || dz == -13);
        let throne = dx == 0 && dz == -16;
        if pillar {
            push_range(&mut u, 8, 12, OMat::Obsidian);
        } else if throne {
            u.push((8, OMat::GoldBlock));
            u.push((9, OMat::GildedBlackstone));
        }
        u.push((12, OMat::RedNetherBricks));
        u.push((13, OMat::GildedBlackstone));
        return (floor, u);
    }

    // 4. Stepped Stadium Grandstand Bleachers (r_sq in 144..=399).
    if r_sq >= 144 {
        let side_foyer = ax <= 7 && (13..=17).contains(&dz);
        if in_cardinal_tunnel || side_foyer {
            // Walkable radial tunnels & Strongroom side-aisles beneath tiered upper seats!
            push_range(&mut u, 5, 7, OMat::PolishedBricks);
        } else {
            let r = (r_sq as f64).sqrt().floor() as i32;
            let tier_h = (r - 11).clamp(1, 7);
            push_range(&mut u, 1, tier_h, OMat::PolishedBricks);
            if r == 16 {
                u.push((tier_h + 1, OMat::SoulLantern));
            }
        }
        return (floor, u);
    }

    // 5. Gladiator Arena Killing Floor (r_sq < 144): Lava/Magma Hazard Trench & Center Podium.
    if in_hazard_ring {
        if r_sq <= 55 && (dx + dz).abs() % 2 == 0 {
            u.push((1, OMat::Lava));
        } else {
            u.push((1, OMat::Magma));
        }
    } else if r_sq <= 9 {
        u.push((1, OMat::Obsidian));
        if r_sq <= 4 {
            u.push((2, OMat::GildedBlackstone));
        }
        if dx == 0 && dz == 0 {
            u.push((3, OMat::AncientDebris));
            u.push((4, OMat::SoulCampfire));
        }
    }

    (floor, u)
}

fn plan_alchemist_spire_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    let on_path = ax <= 2 || az <= 2;
    let in_pavilion = (9..=14).contains(&ax) && (9..=14).contains(&az);
    let floor = if on_path || cheb <= 6 || in_pavilion {
        if roll % 5 == 0 {
            OMat::CryingObsidian
        } else {
            OMat::PolishedDeepslate
        }
    } else if roll % 4 == 0 {
        OMat::WarpedWartBlock
    } else {
        OMat::DeepslateBricks
    };

    // 1. Outer Crystalline Obsidian Bastion Wall (cheb == 18..=20).
    let gate = (ax <= 2 && az >= 18) || (az <= 2 && ax >= 18);
    if cheb >= 18 {
        let pylon = ax >= 17 && az >= 17;
        if gate {
            push_range(&mut u, 5, 7, OMat::Obsidian);
        } else if pylon {
            push_range(&mut u, 1, 10, OMat::CryingObsidian);
            u.push((11, OMat::Shroomlight));
            u.push((12, OMat::WarpedFence));
        } else {
            for dy in 1..=6 {
                let m = if dy == 3 && (dx % 3 == 0 || dz % 3 == 0) {
                    OMat::IronBars
                } else if (dx % 4 == 0 && az >= 18) || (dz % 4 == 0 && ax >= 18) {
                    OMat::WarpedStem
                } else {
                    OMat::DeepslateBricks
                };
                u.push((dy, m));
            }
            if cheb == 20 && (ax + az) % 4 == 0 {
                u.push((7, OMat::SoulLantern));
            }
        }
        return (floor, u);
    }

    // 2. Four Corner Warped Alchemy & Fungal Pavilions (ax in 9..=14, az in 9..=14).
    if in_pavilion {
        let post = (ax == 9 || ax == 14) && (az == 9 || az == 14);
        if post {
            push_range(&mut u, 1, 4, OMat::WarpedStem);
        }
        u.push((4, OMat::WarpedPlanks));
        if (10..=13).contains(&ax) && (10..=13).contains(&az) {
            u.push((5, OMat::WarpedWartBlock));
        }
        if (11..=12).contains(&ax) && (11..=12).contains(&az) {
            u.push((6, OMat::Shroomlight));
        }
        return (floor, u);
    }

    // 3. Central Three-Story Warped Arcane Citadel Tower (cheb <= 6, Spire up to dy = 28).
    if cheb <= 6 {
        let tower_wall = cheb == 6;
        let door = (ax <= 1 && az == 6) || (az <= 1 && ax == 6);
        if tower_wall {
            let corner_rib = ax >= 5 && az >= 5;
            for dy in 1..=22 {
                if door && dy <= 3 {
                    continue;
                }
                let m = if corner_rib {
                    OMat::StrippedWarpedStem
                } else if dy == 7 || dy == 14 || dy == 21 {
                    OMat::CryingObsidian
                } else if (dy == 9 || dy == 10 || dy == 16 || dy == 17) && (ax <= 1 || az <= 1) {
                    OMat::IronBars
                } else {
                    OMat::DeepslateBricks
                };
                u.push((dy, m));
            }
        } else {
            // Solid Stair 1: Floor 1 -> Floor 2 (dy = 7) along West wall (dx = -5, dz in -3..=3).
            if dx == -5 && (-3..=3).contains(&dz) {
                push_range(&mut u, 1, dz + 4, OMat::Obsidian);
            }
            if !(dx == -5 && (-3..=2).contains(&dz)) {
                u.push((7, OMat::PolishedDeepslate));
            }

            // Solid Stair 2: Floor 2 (7) -> Floor 3 Treasury (14) along East wall (dx = 5, dz in -3..=3).
            if dx == 5 && (-3..=3).contains(&dz) {
                push_range(&mut u, 8, 11 - dz, OMat::Obsidian);
            }
            if !(dx == 5 && (-2..=3).contains(&dz)) {
                u.push((14, OMat::Obsidian));
            }

            // Suspended glowing chandeliers beneath Floor 2 & Floor 3 ceilings.
            if dx == 0 && (dz == -2 || dz == 2) {
                push_hanging_chandelier(&mut u, 7, 1);
                push_hanging_chandelier(&mut u, 14, 1);
            }
            if dx == 0 && dz == 0 {
                u.push((15, OMat::GoldBlock));
                u.push((16, OMat::AncientDebris));
                u.push((17, OMat::Glowstone));
            }
        }

        // Overlapping Tiered Fungal Dome & Beacon Spire (dy = 22..=28, 100% 6-connected).
        match cheb {
            6 => u.push((22, OMat::WarpedWartBlock)),
            5 => push_range(&mut u, 22, 24, OMat::WarpedWartBlock),
            4 => push_range(&mut u, 24, 25, OMat::Obsidian),
            3 => push_range(&mut u, 25, 26, OMat::WarpedWartBlock),
            2 => push_range(&mut u, 26, 27, OMat::CryingObsidian),
            _ => {
                u.push((27, OMat::CryingObsidian));
                u.push((28, OMat::Shroomlight));
            }
        }
        return (floor, u);
    }

    // Courtyard Arcane Obelisk Lantern Posts along diagonal walks.
    if ax == 5 && az == 12 {
        push_range(&mut u, 1, 2, OMat::StrippedWarpedStem);
        u.push((3, OMat::SoulLantern));
    }

    (floor, u)
}

#[allow(clippy::too_many_arguments)]
fn build_outpost_structure(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    state: &mut RiftState,
    cx: i32,
    floor_y: i32,
    cz: i32,
    kind: OutpostKind,
    flavor: NetherFlavor,
    roll: u64,
) {
    let Some(blackstone) = host.block_state_id("minecraft:blackstone") else {
        return;
    };
    let resolve = |id: &str, fallback: BlockState| host.block_state_id(id).unwrap_or(fallback);
    let iron_bars = resolve("minecraft:iron_bars", blackstone);
    // Minecraft 1.21.9+ / 26.3 renamed chain -> iron_chain; try iron_chain first so it never degrades to a blackstone cube!
    let chain = host
        .block_state_id("minecraft:iron_chain")
        .or_else(|| host.block_state_id("minecraft:chain"))
        .unwrap_or(iron_bars);

    let palette = vec![
        resolve("minecraft:air", blackstone),                              // 0 Air
        blackstone,                                                        // 1 Blackstone
        resolve("minecraft:polished_blackstone_bricks", blackstone),       // 2 PolishedBricks
        resolve("minecraft:cracked_polished_blackstone_bricks", blackstone), // 3 CrackedBricks
        resolve("minecraft:chiseled_polished_blackstone", blackstone),     // 4 ChiseledBlackstone
        resolve("minecraft:gilded_blackstone", blackstone),                // 5 GildedBlackstone
        resolve("minecraft:obsidian", blackstone),                         // 6 Obsidian
        resolve("minecraft:crying_obsidian", blackstone),                  // 7 CryingObsidian
        resolve("minecraft:basalt", blackstone),                           // 8 Basalt
        resolve("minecraft:polished_basalt", blackstone),                  // 9 PolishedBasalt
        resolve("minecraft:red_nether_bricks", blackstone),                // 10 RedNetherBricks
        resolve("minecraft:nether_bricks", blackstone),                    // 11 NetherBricks
        resolve("minecraft:crimson_stem", blackstone),                     // 12 CrimsonStem
        resolve("minecraft:stripped_crimson_stem", blackstone),            // 13 StrippedCrimsonStem
        resolve("minecraft:crimson_planks", blackstone),                   // 14 CrimsonPlanks
        resolve("minecraft:crimson_fence", blackstone),                    // 15 CrimsonFence
        resolve("minecraft:soul_soil", blackstone),                        // 16 SoulSoil
        resolve("minecraft:soul_sand", blackstone),                        // 17 SoulSand
        resolve("minecraft:magma_block", blackstone),                      // 18 Magma
        resolve("minecraft:shroomlight", blackstone),                      // 19 Shroomlight
        resolve("minecraft:nether_wart_block", blackstone),                // 20 NetherWartBlock
        iron_bars,                                                         // 21 IronBars
        chain,                                                             // 22 Chain
        resolve("minecraft:campfire", blackstone),                         // 23 Campfire
        resolve("minecraft:soul_campfire", blackstone),                    // 24 SoulCampfire
        resolve("minecraft:soul_fire", blackstone),                        // 25 SoulFire
        resolve("minecraft:gold_block", blackstone),                       // 26 GoldBlock
        resolve("minecraft:iron_block", blackstone),                       // 27 IronBlock
        resolve("minecraft:ancient_debris", blackstone),                   // 28 AncientDebris
        resolve("minecraft:lava", blackstone),                             // 29 Lava
        resolve("minecraft:soul_lantern", blackstone),                     // 30 SoulLantern
        resolve("minecraft:hay_block", blackstone),                        // 31 HayBlock
        resolve("minecraft:warped_stem", blackstone),                      // 32 WarpedStem
        resolve("minecraft:stripped_warped_stem", blackstone),             // 33 StrippedWarpedStem
        resolve("minecraft:warped_planks", blackstone),                    // 34 WarpedPlanks
        resolve("minecraft:warped_fence", blackstone),                     // 35 WarpedFence
        resolve("minecraft:warped_wart_block", blackstone),                // 36 WarpedWartBlock
        resolve("minecraft:bone_block", blackstone),                       // 37 BoneBlock
        resolve("minecraft:deepslate_bricks", blackstone),                 // 38 DeepslateBricks
        resolve("minecraft:polished_deepslate", blackstone),               // 39 PolishedDeepslate
        resolve("minecraft:smooth_basalt", blackstone),                    // 40 SmoothBasalt
        resolve("minecraft:glowstone", blackstone),                        // 41 Glowstone
    ];

    let half = outpost_half_extent(kind);
    let skirt = half + 3;
    let mut runs: Vec<BlockRun> = Vec::with_capacity(4096);

    for dx in -skirt..=skirt {
        for dz in -skirt..=skirt {
            let x = cx + dx;
            let z = cz + dz;
            if !ow.is_loaded(host, [x, 64, z]) {
                continue;
            }
            let ax = dx.abs();
            let az = dz.abs();
            let cheb = ax.max(az);
            let col_roll = spatial_hash(x, floor_y, z, roll);

            let top_existing = host
                .surface_heights(OVERWORLD, &[(x, z)], Heightmap::WorldSurface)
                .ok()
                .and_then(|hs| hs.first().copied().flatten())
                .unwrap_or(floor_y + 10)
                .clamp(floor_y, OVERWORLD_MAX_Y - 2);

            if cheb > half {
                let on_gate = ax <= 3 || az <= 3;
                if on_gate {
                    let out = cheb - half;
                    let step_y = (floor_y - (out / 2)).clamp(OVERWORLD_MIN_Y + 4, floor_y);
                    let mut solid_y = step_y - 1;
                    for y in ((step_y - 32).max(OVERWORLD_MIN_Y + 2)..step_y).rev() {
                        let b = ow.block(host, [x, y, z]).map(|k| k.id(host)).unwrap_or_default();
                        if !is_fillable_terrain_void(strip_ns(&b)) {
                            break;
                        }
                        solid_y = y;
                    }
                    if solid_y < step_y {
                        runs.push(BlockRun {
                            x,
                            z,
                            y: solid_y,
                            height: (step_y - solid_y + 1) as u32,
                            surface: None,
                            block: OMat::Blackstone as u32,
                            only: None,
                        });
                    } else {
                        runs.push(BlockRun {
                            x,
                            z,
                            y: step_y,
                            height: 1,
                            surface: None,
                            block: OMat::CrackedBricks as u32,
                            only: None,
                        });
                    }
                    if top_existing > step_y {
                        runs.push(BlockRun {
                            x,
                            z,
                            y: step_y + 1,
                            height: (top_existing - step_y + 2) as u32,
                            surface: None,
                            block: OMat::Air as u32,
                            only: None,
                        });
                    }
                }
                continue;
            }

            // Solid downward foundation pier down to bedrock/stone — never floating!
            let mut down_y = floor_y;
            for y in ((floor_y - 48).max(OVERWORLD_MIN_Y + 2)..floor_y).rev() {
                let b = ow.block(host, [x, y, z]).map(|k| k.id(host)).unwrap_or_default();
                if !is_fillable_terrain_void(strip_ns(&b)) {
                    break;
                }
                down_y = y;
            }
            if down_y < floor_y {
                runs.push(BlockRun {
                    x,
                    z,
                    y: down_y,
                    height: (floor_y - down_y) as u32,
                    surface: None,
                    block: if col_roll % 4 == 0 {
                        OMat::Basalt as u32
                    } else {
                        OMat::Blackstone as u32
                    },
                    only: None,
                });
            }

            let (floor_mat, upper) = plan_outpost_column(kind, dx, dz, col_roll);
            runs.push(BlockRun {
                x,
                z,
                y: floor_y,
                height: 1,
                surface: None,
                block: floor_mat as u32,
                only: None,
            });
            let clear_h = match kind {
                OutpostKind::HoglinVillage => 18,
                _ => 30,
            };
            let clear_top = (floor_y + clear_h)
                .max(top_existing + 2)
                .min(OVERWORLD_MAX_Y - 1);
            if clear_top > floor_y {
                runs.push(BlockRun {
                    x,
                    z,
                    y: floor_y + 1,
                    height: (clear_top - floor_y) as u32,
                    surface: None,
                    block: OMat::Air as u32,
                    only: None,
                });
            }
            for (dy, mat) in upper {
                let ty = floor_y + dy;
                if ty <= OVERWORLD_MAX_Y - 1 {
                    runs.push(BlockRun {
                        x,
                        z,
                        y: ty,
                        height: 1,
                        surface: None,
                        block: mat as u32,
                        only: None,
                    });
                }
            }
        }
    }

    let edit = BlockEdit {
        palette,
        filters: Vec::new(),
        runs,
    };
    let _ = host.edit_blocks(OVERWORLD, &edit);

    place_outpost_details_and_chests(host, ow, cx, floor_y, cz, kind, roll);

    for x in ((cx - half - 4)..=(cx + half + 4)).step_by(4) {
        for z in ((cz - half - 4)..=(cz + half + 4)).step_by(4) {
            if state.mark_quart_corrupted([x, floor_y, z]) {
                paint_biome_column(ow, host, x, z, flavor);
            }
        }
    }

    spawn_outpost_garrison(host, state, cx, floor_y, cz, kind);
}

fn place_chest_with_loot(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    pos: [i32; 3],
    table: &str,
    seed: i64,
) {
    let Some(chest) = host.block_state_id("minecraft:chest") else {
        return;
    };
    if !ow.is_loaded(host, pos) {
        return;
    }
    if ow
        .set_block(host, pos, chest, BlockUpdateFlags::CLIENTS)
        .unwrap_or(false)
        && let Some(be) = host.block_entity(ow, pos)
    {
        let mut data = Compound::new();
        data.insert("LootTable".into(), Tag::String(table.into()));
        data.insert("LootTableSeed".into(), Tag::Long(seed));
        let _ = be.merge_data(host, data);
    }
}

fn place_block_if_loaded(host: &mut dyn ServerHost, ow: LevelRef, pos: [i32; 3], id: &str) {
    if ow.is_loaded(host, pos)
        && let Some(st) = host.block_state_id(id)
    {
        let _ = ow.set_block(host, pos, st, BlockUpdateFlags::CLIENTS);
    }
}

pub(crate) fn outpost_chest_specs(
    kind: OutpostKind,
) -> &'static [([i32; 3], bool, &'static str, u64)] {
    match kind {
        OutpostKind::HoglinVillage => &[
            ([-2, 1, 15], true, "minecraft:chests/bastion_treasure", 0x1101),
            ([2, 1, 15], true, "minecraft:chests/bastion_treasure", 0x1102),
            ([9, 6, -15], true, "minecraft:chests/bastion_treasure", 0x1103),
            ([-10, 1, -17], false, "minecraft:chests/bastion_hoglin_stable", 0x1104),
            ([16, 1, 2], false, "minecraft:chests/bastion_other", 0x1105),
            ([-16, 1, -1], false, "minecraft:chests/bastion_hoglin_stable", 0x1106),
        ],
        OutpostKind::InfernalFoundry => &[
            ([-2, 1, 14], true, "minecraft:chests/bastion_treasure", 0x2201),
            ([2, 1, 14], true, "minecraft:chests/bastion_treasure", 0x2202),
            ([0, 7, -3], true, "minecraft:chests/nether_bridge", 0x2203),
            ([-14, 1, 8], false, "minecraft:chests/bastion_other", 0x2204),
            ([14, 1, 8], false, "minecraft:chests/nether_bridge", 0x2205),
            ([-7, 1, -14], false, "minecraft:chests/bastion_other", 0x2206),
        ],
        OutpostKind::SiegeWatchtower => &[
            ([2, 15, 0], true, "minecraft:chests/bastion_treasure", 0x3301),
            ([-2, 15, 0], true, "minecraft:chests/bastion_treasure", 0x3302),
            ([-2, 8, -2], true, "minecraft:chests/nether_bridge", 0x3303),
            ([3, 1, -3], false, "minecraft:chests/bastion_other", 0x3304),
            ([-11, 1, 6], false, "minecraft:chests/bastion_other", 0x3305),
            ([11, 1, -6], false, "minecraft:chests/nether_bridge", 0x3306),
        ],
        OutpostKind::BoneCatacomb => &[
            ([-2, 9, -2], true, "minecraft:chests/bastion_treasure", 0x4401),
            ([2, 9, -2], true, "minecraft:chests/bastion_treasure", 0x4402),
            ([0, 1, -15], true, "minecraft:chests/nether_bridge", 0x4403),
            ([-9, 5, 6], false, "minecraft:chests/nether_bridge", 0x4404),
            ([9, 5, -6], false, "minecraft:chests/bastion_other", 0x4405),
            ([-14, 1, 14], false, "minecraft:chests/bastion_other", 0x4406),
        ],
        OutpostKind::ArenaPit => &[
            ([-2, 8, -16], true, "minecraft:chests/bastion_treasure", 0x5501),
            ([2, 8, -16], true, "minecraft:chests/bastion_treasure", 0x5502),
            ([0, 1, 15], true, "minecraft:chests/bastion_treasure", 0x5503),
            ([-15, 1, -2], false, "minecraft:chests/bastion_other", 0x5504),
            ([15, 1, 2], false, "minecraft:chests/bastion_other", 0x5505),
            ([6, 1, 15], false, "minecraft:chests/bastion_hoglin_stable", 0x5506),
        ],
        OutpostKind::AlchemistSpire => &[
            ([-2, 15, -1], true, "minecraft:chests/bastion_treasure", 0x6601),
            ([2, 15, 1], true, "minecraft:chests/bastion_treasure", 0x6602),
            ([0, 8, -2], true, "minecraft:chests/nether_bridge", 0x6603),
            ([3, 1, 2], false, "minecraft:chests/bastion_other", 0x6604),
            ([-11, 1, -11], false, "minecraft:chests/nether_bridge", 0x6605),
            ([11, 1, 11], false, "minecraft:chests/bastion_other", 0x6606),
        ],
    }
}

fn place_outpost_details_and_chests(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    cx: i32,
    fy: i32,
    cz: i32,
    kind: OutpostKind,
    roll: u64,
) {
    let base_seed = (roll & 0x7FFF_FFFF) as i64;
    for ([dx, dy, dz], _, table, salt) in outpost_chest_specs(kind) {
        place_chest_with_loot(
            host,
            ow,
            [cx + dx, fy + dy, cz + dz],
            table,
            base_seed ^ (*salt as i64),
        );
    }

    let furn: &[([i32; 3], &str)] = match kind {
        OutpostKind::HoglinVillage => &[
            ([12, 1, -4], "minecraft:smithing_table"),
            ([12, 1, 7], "minecraft:anvil"),
            ([17, 1, -4], "minecraft:crafting_table"),
            ([14, 2, 2], "minecraft:lodestone"),
            ([10, 6, -12], "minecraft:barrel"),
            ([-12, 1, -17], "minecraft:barrel"),
            ([-17, 1, 0], "minecraft:cauldron"),
        ],
        OutpostKind::InfernalFoundry => &[
            ([-13, 1, -8], "minecraft:blast_furnace"),
            ([-11, 1, -8], "minecraft:blast_furnace"),
            ([11, 1, -8], "minecraft:blast_furnace"),
            ([13, 1, -8], "minecraft:blast_furnace"),
            ([-4, 1, 5], "minecraft:anvil"),
            ([4, 1, 5], "minecraft:smithing_table"),
            ([-10, 1, -4], "minecraft:lava_cauldron"),
            ([10, 1, -4], "minecraft:lava_cauldron"),
        ],
        OutpostKind::SiegeWatchtower => &[
            ([-2, 1, 2], "minecraft:anvil"),
            ([-2, 1, -2], "minecraft:smithing_table"),
            ([11, 1, -8], "minecraft:crafting_table"),
            ([-11, 1, 8], "minecraft:barrel"),
            ([11, 1, 11], "minecraft:target"),
            ([9, 1, 9], "minecraft:target"),
            ([0, 8, 2], "minecraft:lodestone"),
        ],
        OutpostKind::BoneCatacomb => &[
            ([0, 9, 2], "minecraft:enchanting_table"),
            ([-2, 5, -7], "minecraft:wither_skeleton_skull"),
            ([2, 5, -7], "minecraft:wither_skeleton_skull"),
            ([-7, 5, 7], "minecraft:cauldron"),
            ([7, 5, 7], "minecraft:brewing_stand"),
            ([-14, 1, -14], "minecraft:barrel"),
        ],
        OutpostKind::ArenaPit => &[
            ([-16, 1, 2], "minecraft:anvil"),
            ([16, 1, -2], "minecraft:smithing_table"),
            ([-4, 1, 14], "minecraft:lava_cauldron"),
            ([4, 1, 14], "minecraft:lava_cauldron"),
            ([0, 8, -14], "minecraft:lodestone"),
        ],
        OutpostKind::AlchemistSpire => &[
            ([0, 1, -2], "minecraft:enchanting_table"),
            ([0, 8, 2], "minecraft:respawn_anchor"),
            ([-12, 1, 12], "minecraft:brewing_stand"),
            ([12, 1, -12], "minecraft:cauldron"),
            ([-10, 1, -12], "minecraft:brewing_stand"),
            ([10, 1, 12], "minecraft:smithing_table"),
        ],
    };

    for ([dx, dy, dz], id) in furn {
        place_block_if_loaded(host, ow, [cx + dx, fy + dy, cz + dz], id);
    }
}

fn armor_captain(host: &mut dyn ServerHost, eid: u32, title: &str) {
    let Some(ent) = host.entity(eid) else {
        return;
    };
    let _ = ent.set_custom_name(host, Some(Text::literal(title.to_owned())));
    if let Some(living) = host.living(ent) {
        let _ = living.set_attribute_base(host, "minecraft:max_health", 80.0);
        let _ = living.set_health(host, 80.0);
        for (slot, id) in [
            (EquipmentSlot::Head, "minecraft:netherite_helmet"),
            (EquipmentSlot::Chest, "minecraft:golden_chestplate"),
            (EquipmentSlot::MainHand, "minecraft:netherite_axe"),
        ] {
            if let Ok(ident) = Identifier::parse(id)
                && let Some(key) = host.item_key(&ident)
                && let Some(stack) = ItemStack::new(key, 1)
            {
                let _ = living.set_equipment(host, slot, Some(stack));
            }
        }
    }
}

pub(crate) fn outpost_garrison_specs(
    kind: OutpostKind,
) -> &'static [(&'static str, [i32; 3], Option<&'static str>)] {
    match kind {
        OutpostKind::HoglinVillage => &[
            ("minecraft:piglin_brute", [13, 1, -1], Some("War-Kraal Chieftain")),
            ("minecraft:piglin_brute", [10, 6, -13], Some("Kraal Taskmaster")),
            ("minecraft:hoglin", [-6, 1, -14], None),
            ("minecraft:hoglin", [2, 1, -15], None),
            ("minecraft:hoglin", [-15, 1, -3], None),
            ("minecraft:hoglin", [-15, 1, 7], None),
            ("minecraft:hoglin", [-6, 1, 5], None),
            ("minecraft:hoglin", [5, 1, 5], None),
            ("minecraft:piglin", [0, 1, 14], None),
            ("minecraft:piglin", [-3, 1, 14], None),
            ("minecraft:piglin", [11, 1, 5], None),
            ("minecraft:piglin", [16, 1, -1], None),
            ("minecraft:piglin", [-5, 1, -5], None),
            ("minecraft:piglin", [5, 1, -5], None),
            ("minecraft:wither_skeleton", [0, 1, -11], None),
        ],
        OutpostKind::InfernalFoundry => &[
            ("minecraft:piglin_brute", [0, 1, 12], Some("Forge-Master Captain")),
            ("minecraft:piglin_brute", [0, 7, -2], Some("Crucible Warden")),
            ("minecraft:wither_skeleton", [-4, 1, -12], None),
            ("minecraft:wither_skeleton", [4, 1, -12], None),
            ("minecraft:wither_skeleton", [-12, 1, 0], None),
            ("minecraft:wither_skeleton", [12, 1, 0], None),
            ("minecraft:blaze", [-11, 7, -3], None),
            ("minecraft:blaze", [11, 7, -3], None),
            ("minecraft:blaze", [-7, 7, -3], None),
            ("minecraft:blaze", [7, 7, -3], None),
            ("minecraft:piglin", [-3, 1, 15], None),
            ("minecraft:piglin", [3, 1, 15], None),
            ("minecraft:piglin", [-14, 1, 12], None),
            ("minecraft:piglin", [14, 1, 12], None),
            ("minecraft:magma_cube", [0, 1, 4], None),
            ("minecraft:magma_cube", [0, 1, -10], None),
        ],
        OutpostKind::SiegeWatchtower => &[
            ("minecraft:piglin_brute", [0, 8, 0], Some("Siege Captain")),
            ("minecraft:piglin_brute", [0, 15, 2], Some("Tower Commander")),
            ("minecraft:wither_skeleton", [-3, 15, -2], None),
            ("minecraft:wither_skeleton", [3, 15, 2], None),
            ("minecraft:wither_skeleton", [2, 8, -2], None),
            ("minecraft:wither_skeleton", [0, 1, 3], None),
            ("minecraft:blaze", [-4, 8, 3], None),
            ("minecraft:blaze", [4, 8, -3], None),
            ("minecraft:hoglin", [-11, 1, -10], None),
            ("minecraft:hoglin", [13, 1, 10], None),
            ("minecraft:piglin", [-11, 1, 8], None),
            ("minecraft:piglin", [11, 1, -8], None),
            ("minecraft:piglin", [-9, 1, 0], None),
            ("minecraft:piglin", [9, 1, 0], None),
            ("minecraft:piglin", [0, 1, -11], None),
            ("minecraft:piglin", [0, 1, 11], None),
        ],
        OutpostKind::BoneCatacomb => &[
            ("minecraft:piglin_brute", [0, 9, 1], Some("Necropolis Bone-Lord")),
            ("minecraft:piglin_brute", [0, 1, -15], Some("Soul-Pyre Captain")),
            ("minecraft:wither_skeleton", [-2, 5, -7], None),
            ("minecraft:wither_skeleton", [2, 5, -7], None),
            ("minecraft:wither_skeleton", [-7, 5, 2], None),
            ("minecraft:wither_skeleton", [7, 5, -2], None),
            ("minecraft:wither_skeleton", [0, 1, 2], None),
            ("minecraft:wither_skeleton", [-2, 1, -2], None),
            ("minecraft:blaze", [-8, 5, -8], None),
            ("minecraft:blaze", [8, 5, 8], None),
            ("minecraft:blaze", [0, 9, -2], None),
            ("minecraft:piglin", [-15, 1, 0], None),
            ("minecraft:piglin", [15, 1, 0], None),
            ("minecraft:piglin", [0, 1, 15], None),
            ("minecraft:magma_cube", [-14, 1, -14], None),
            ("minecraft:magma_cube", [14, 1, 14], None),
        ],
        OutpostKind::ArenaPit => &[
            ("minecraft:piglin_brute", [0, 8, -15], Some("Arena Champion")),
            ("minecraft:piglin_brute", [0, 1, 14], Some("Pit Beastmaster")),
            ("minecraft:hoglin", [-4, 1, 0], None),
            ("minecraft:hoglin", [4, 1, 0], None),
            ("minecraft:hoglin", [0, 1, -4], None),
            ("minecraft:hoglin", [0, 1, 4], None),
            ("minecraft:wither_skeleton", [-3, 1, -3], None),
            ("minecraft:wither_skeleton", [3, 1, 3], None),
            ("minecraft:wither_skeleton", [-10, 1, 0], None),
            ("minecraft:wither_skeleton", [10, 1, 0], None),
            ("minecraft:blaze", [-10, 4, -10], None),
            ("minecraft:blaze", [10, 4, 10], None),
            ("minecraft:piglin", [-16, 1, 0], None),
            ("minecraft:piglin", [16, 1, 0], None),
            ("minecraft:piglin", [-6, 1, 15], None),
            ("minecraft:piglin", [5, 1, 15], None),
            ("minecraft:magma_cube", [-4, 1, 4], None),
            ("minecraft:magma_cube", [4, 1, -4], None),
        ],
        OutpostKind::AlchemistSpire => &[
            ("minecraft:piglin_brute", [0, 15, 2], Some("Arcane Vault-Warden")),
            ("minecraft:piglin_brute", [0, 8, 0], Some("Spire Enchanter")),
            ("minecraft:blaze", [-11, 1, 11], None),
            ("minecraft:blaze", [11, 1, -11], None),
            ("minecraft:blaze", [-3, 8, -3], None),
            ("minecraft:blaze", [3, 8, 3], None),
            ("minecraft:wither_skeleton", [-2, 15, 2], None),
            ("minecraft:wither_skeleton", [2, 15, -2], None),
            ("minecraft:wither_skeleton", [-2, 1, 2], None),
            ("minecraft:wither_skeleton", [2, 1, -2], None),
            ("minecraft:piglin", [0, 1, -10], None),
            ("minecraft:piglin", [0, 1, 10], None),
            ("minecraft:piglin", [-10, 1, 0], None),
            ("minecraft:piglin", [10, 1, 0], None),
            ("minecraft:piglin", [-12, 1, -10], None),
            ("minecraft:piglin", [12, 1, 10], None),
        ],
    }
}

fn spawn_outpost_garrison(
    host: &mut dyn ServerHost,
    state: &mut RiftState,
    cx: i32,
    fy: i32,
    cz: i32,
    kind: OutpostKind,
) {
    for (mob_kind, [dx, dy, dz], captain_title) in outpost_garrison_specs(kind) {
        let mut nbt = Compound::new();
        nbt.insert("PersistenceRequired".into(), Tag::Byte(1));
        if is_piglin_or_hoglin(mob_kind) {
            nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
            nbt.insert("TimeInOverworld".into(), Tag::Int(0));
        }
        let pos = [
            f64::from(cx + dx) + 0.5,
            f64::from(fy + dy),
            f64::from(cz + dz) + 0.5,
        ];
        if let Some(eid) = host.summon(OVERWORLD, mob_kind, pos, nbt) {
            state.known_nether_mobs.insert(eid);
            state.citadel_guards.insert(eid);
            if let Some(ent) = host.entity(eid) {
                let _ = ent.add_tag(host, "rift_bleed:nether_ally");
                let _ = ent.add_tag(host, "rift_bleed:citadel_guard");
            }
            if let Some(title) = captain_title {
                armor_captain(host, eid, title);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::build_ring_columns;
    use std::collections::{HashMap, HashSet, VecDeque};

    fn matured_rift() -> RiftSource {
        RiftSource {
            id: 1,
            center: [0, 64, 0],
            active_portal: true,
            contained: false,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 20_000,
            corrupted_blocks: 120_000,
            max_radius_sq: 80 * 80,
            ring_radius: 80,
            ring_columns: build_ring_columns(0, 0, 80),
            active_column: None,
            deepen_radius: 20,
            deepen_columns: Vec::new(),
            max_deepen_reached: 20,
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
    fn outposts_scale_dynamically_across_all_six_archetypes() {
        let mut rift = matured_rift();
        let mut seen = Vec::new();
        // Even 12+ outposts unlock dynamically as wasteland territory grows large!
        for placed in 0..12u32 {
            rift.outposts_placed = placed;
            rift.corrupted_blocks = 1_500_000;
            rift.ring_radius = 220;
            let w = ConnectedWasteland {
                total_corrupted_blocks: 1_500_000,
                total_area_radius_sq: 220 * 220,
                max_deepen_radius: 25,
                max_ring_radius: 220,
                total_bastions_placed: 0,
                total_outposts_placed: placed,
                member_count: 1,
            };
            let kind = wasteland_eligible_for_outpost(&rift, w).expect("should unlock outpost");
            if placed < 6 {
                seen.push(kind);
            }
        }
        assert!(seen.contains(&OutpostKind::HoglinVillage));
        assert!(seen.contains(&OutpostKind::InfernalFoundry));
        assert!(seen.contains(&OutpostKind::SiegeWatchtower));
        assert!(seen.contains(&OutpostKind::BoneCatacomb));
        assert!(seen.contains(&OutpostKind::ArenaPit));
        assert!(seen.contains(&OutpostKind::AlchemistSpire));

        let mut st = RiftState::new();
        st.rifts.push(rift);
        assert!(!site_clear_for_outpost(&st, 20, 20));
        assert!(site_clear_for_outpost(&st, 80, 20));
    }

    #[test]
    fn all_six_outpost_blueprints_are_grounded_roofed_and_clearance_verified() {
        for kind in [
            OutpostKind::HoglinVillage,
            OutpostKind::InfernalFoundry,
            OutpostKind::SiegeWatchtower,
            OutpostKind::BoneCatacomb,
            OutpostKind::ArenaPit,
            OutpostKind::AlchemistSpire,
        ] {
            let half = outpost_half_extent(kind);
            assert!(
                half >= 18,
                "{kind:?} half extent should be large (>= 18), got {half}"
            );

            let mut grid: HashMap<[i32; 3], OMat> = HashMap::new();
            for dx in -half..=half {
                for dz in -half..=half {
                    let (floor_mat, upper) = plan_outpost_column(kind, dx, dz, 0);
                    grid.insert([dx, 0, dz], floor_mat);
                    for (dy, mat) in upper {
                        assert_ne!(mat, OMat::Air, "unexpected Air in upper list");
                        grid.insert([dx, dy, dz], mat);
                    }
                }
            }

            // 1. 6-connected BFS support check from dy = 0 guarantees ZERO floating blocks!
            let mut visited: HashSet<[i32; 3]> = HashSet::new();
            let mut q: VecDeque<[i32; 3]> = VecDeque::new();
            for pos in grid.keys() {
                if pos[1] == 0 {
                    visited.insert(*pos);
                    q.push_back(*pos);
                }
            }
            while let Some([x, y, z]) = q.pop_front() {
                for [nx, ny, nz] in [
                    [x + 1, y, z],
                    [x - 1, y, z],
                    [x, y + 1, z],
                    [x, y - 1, z],
                    [x, y, z + 1],
                    [x, y, z - 1],
                ] {
                    let np = [nx, ny, nz];
                    if grid.contains_key(&np) && visited.insert(np) {
                        q.push_back(np);
                    }
                }
            }
            for (pos, mat) in &grid {
                assert!(
                    visited.contains(pos),
                    "{kind:?} has floating block {mat:?} at {pos:?}"
                );
            }

            // 2. Campfires/SoulCampfires/SoulFires/Lanterns must sit on non-air; Chains must attach upward; Shroomlight on chains attached upward.
            for (&[x, y, z], &mat) in &grid {
                if matches!(
                    mat,
                    OMat::Campfire | OMat::SoulCampfire | OMat::SoulFire | OMat::SoulLantern | OMat::Glowstone
                ) {
                    assert!(
                        grid.get(&[x, y - 1, z])
                            .is_some_and(|below| !matches!(below, OMat::Lava | OMat::Campfire)),
                        "{kind:?} has unsupported {mat:?} at [{x}, {y}, {z}]"
                    );
                }
                if mat == OMat::Chain {
                    assert!(
                        grid.contains_key(&[x, y + 1, z]),
                        "{kind:?} has unattached Chain at [{x}, {y}, {z}]"
                    );
                }
            }

            // 3. Every chest must sit on solid non-lava support with Air at its position + 1 block above.
            let chests = outpost_chest_specs(kind);
            assert!(chests.len() >= 6, "{kind:?} should have >= 6 chests");
            for ([dx, dy, dz], _, _, _) in chests {
                let below = grid.get(&[*dx, dy - 1, *dz]);
                assert!(
                    below.is_some_and(|b| !matches!(b, OMat::Lava | OMat::Campfire | OMat::SoulFire)),
                    "{kind:?} chest at [{dx}, {dy}, {dz}] lacks solid floor below"
                );
                assert!(
                    !grid.contains_key(&[*dx, *dy, *dz])
                        && !grid.contains_key(&[*dx, dy + 1, *dz]),
                    "{kind:?} chest at [{dx}, {dy}, {dz}] blocked by geometry"
                );
            }

            // 4. Every garrison spawn spot must stand on solid ground with 2 blocks headroom.
            let roster = outpost_garrison_specs(kind);
            assert!(roster.len() >= 15, "{kind:?} should field >= 15 defenders");
            for (mob, [dx, dy, dz], _) in roster {
                let below = grid.get(&[*dx, dy - 1, *dz]);
                assert!(
                    below.is_some_and(|b| !matches!(b, OMat::Lava | OMat::Campfire | OMat::SoulFire)),
                    "{kind:?} mob {mob} at [{dx}, {dy}, {dz}] lacks solid floor below"
                );
                assert!(
                    !grid.contains_key(&[*dx, *dy, *dz])
                        && !grid.contains_key(&[*dx, dy + 1, *dz]),
                    "{kind:?} mob {mob} at [{dx}, {dy}, {dz}] suffocated in block"
                );
            }
        }
    }
}
