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

const OUTPOST_CHECK_INTERVAL_TICKS: u64 = 300;
const OUTPOST_OFFSET_TICK: u64 = 170;
const MAX_OUTPOSTS_PER_RIFT: u32 = 4;
const MIN_OUTPOST_PORTAL_DIST: i64 = 52;
const MIN_OUTPOST_SPACING: i64 = 105;
const MIN_OUTPOST_BASTION_SPACING: i64 = 64;
const MIN_PLAYER_DIST_FOR_OUTPOST: i64 = 64;

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
}

pub fn wasteland_eligible_for_outpost(
    rift: &RiftSource,
    wasteland: ConnectedWasteland,
) -> Option<OutpostKind> {
    if rift.contained
        || rift.pacified
        || rift.regressing
        || rift.outposts_placed >= MAX_OUTPOSTS_PER_RIFT
    {
        return None;
    }
    let max_group_outposts = ((wasteland.member_count as u32) * MAX_OUTPOSTS_PER_RIFT).min(8);
    if wasteland.total_outposts_placed >= max_group_outposts {
        return None;
    }

    let (min_blocks, min_ring) = match rift.outposts_placed {
        0 => (85_000, 65),
        1 => (190_000, 90),
        2 => (320_000, 110),
        _ => (480_000, 125),
    };
    if wasteland.total_corrupted_blocks < min_blocks || wasteland.max_ring_radius < min_ring {
        return None;
    }

    // Cycle reliably across all three brutal Mordor outpost archetypes!
    Some(match (rift.outposts_placed as u64 + (rift.id % 3)) % 3 {
        0 => OutpostKind::HoglinVillage,
        1 => OutpostKind::InfernalFoundry,
        _ => OutpostKind::SiegeWatchtower,
    })
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

    let player_positions = overworld_player_positions(host);

    for idx in 0..state.rifts.len() {
        let wasteland = state.connected_wasteland_for(idx);
        let rift = &state.rifts[idx];
        let Some(kind) = wasteland_eligible_for_outpost(rift, wasteland) else {
            continue;
        };

        let radius = ((rift.max_radius_sq as f64).sqrt() as i32).max(rift.ring_radius);
        let inner = (MIN_OUTPOST_PORTAL_DIST as i32) + 4;
        let outer = (radius - 14).max(inner + 4);
        if radius < inner + 6 {
            continue;
        }

        let mut picked: Option<([i32; 3], u64)> = None;
        for attempt in 0..16u64 {
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
    let half = outpost_half_extent(kind) + 4;
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

    if state.tick.saturating_sub(pending.queued_tick) > 160 {
        release_outpost_chunks(host, level, state);
        return;
    }

    let all_loaded = pending
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
        "[Rift Bleed] A brutal {} has fortified the outskirts near ({}, {}, {})!",
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
                && horiz_dist_sq([ppos[0] as i32, 64, ppos[2] as i32], pending.center) <= 280 * 280
            {
                let _ = level.play_sound(
                    host,
                    "minecraft:block.anvil.place",
                    ppos,
                    SoundSource::Hostile,
                    0.45,
                    0.65,
                );
            }
        }
    }
}

pub fn outpost_half_extent(kind: OutpostKind) -> i32 {
    match kind {
        OutpostKind::HoglinVillage => 17,
        OutpostKind::InfernalFoundry => 14,
        OutpostKind::SiegeWatchtower => 11,
    }
}

fn validate_and_resolve_outpost_floor(
    host: &dyn ServerHost,
    ow: LevelRef,
    state: &RiftState,
    pending: &PendingOutpost,
) -> Option<i32> {
    let half = outpost_half_extent(pending.kind);
    let step = (half / 2).max(4);
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
    if max_h - min_h > 18 || min_h < 48 || max_h > 165 {
        return None;
    }

    let mut corrupted_hits = 0usize;
    let mut water_hits = 0usize;
    for (idx, (px, pz)) in probe_cols.iter().enumerate() {
        let p = [*px, hs[idx] - 1, *pz];
        if state.is_quart_corrupted(p) {
            corrupted_hits += 1;
        }
        if let Some(key) = ow.block(host, p) {
            let name = key.id(host);
            if is_water_like(&name) {
                water_hits += 1;
            } else if is_nether_terrain_block(&name) {
                corrupted_hits += 1;
            }
        }
        if let Some(top) = ow.block(host, [*px, hs[idx], *pz])
            && is_water_like(&top.id(host))
        {
            water_hits += 1;
        }
    }

    if water_hits > 1 || corrupted_hits < 6 {
        return None;
    }

    let mut sorted = hs;
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2] - 1;
    Some(median.clamp(OVERWORLD_MIN_Y + 8, OVERWORLD_MAX_Y - 32))
}

/// Pure column geometry blueprint shared across all three Mordor outpost archetypes.
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
    }
}

fn plan_hoglin_village_col(dx: i32, dz: i32, roll: u64) -> (OMat, Vec<(i32, OMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    // Base brutal kraal ground floor: packed basalt mud, cracked blackstone, warm cinder trenches.
    let floor = if (dx.abs() <= 2 && dz >= 0) || (dz.abs() <= 2 && dx.abs() <= 16) {
        if roll % 5 == 0 {
            OMat::GildedBlackstone
        } else {
            OMat::CrackedBricks
        }
    } else if dz >= 8 && dx.abs() <= 5 {
        OMat::PolishedBricks
    } else if dx >= 7 && dx <= 14 && dz >= -5 && dz <= 7 {
        OMat::PolishedBricks
    } else if dx >= -14 && dx <= -8 && dz >= -4 && dz <= 6 {
        if roll % 2 == 0 {
            OMat::SoulSand
        } else {
            OMat::SoulSoil
        }
    } else if roll % 7 == 0 {
        OMat::Magma
    } else if roll % 3 == 0 {
        OMat::SoulSoil
    } else {
        OMat::Blackstone
    };

    // 1. Jagged Perimeter Palisade (cheb == 16..=17) with South & West/East brutal gates.
    let in_south_gate = dz >= 15 && dx.abs() <= 2;
    let in_west_gate = dx <= -15 && dz.abs() <= 2;
    if cheb >= 16 && !in_south_gate && !in_west_gate {
        let spike = (dx + dz).abs() % 3 == 0;
        let wall_h = if ax >= 15 && az >= 15 {
            7
        } else if spike {
            5
        } else {
            4
        };
        for dy in 1..=wall_h {
            let m = if dy == wall_h && (ax >= 15 && az >= 15) {
                OMat::Obsidian
            } else if dy == wall_h && spike {
                OMat::Basalt
            } else if dy == 3 && !spike && (dx.abs() % 2 == 0 || dz.abs() % 2 == 0) {
                OMat::IronBars
            } else if (dx % 4 == 0 && az >= 16) || (dz % 4 == 0 && ax >= 16) {
                OMat::StrippedCrimsonStem
            } else if roll % 3 == 0 {
                OMat::CrackedBricks
            } else {
                OMat::Blackstone
            };
            u.push((dy, m));
        }
        if ax >= 15 && az >= 15 {
            u.push((wall_h + 1, OMat::SoulCampfire));
        }
        return (floor, u);
    }

    // Torch/brazier gateposts.
    if (ax == 3 && dz == 16) || (az == 3 && dx == -16) {
        for dy in 1..=5 {
            u.push((dy, if dy == 5 { OMat::SoulCampfire } else { OMat::PolishedBasalt }));
        }
        return (floor, u);
    }

    // 2. North Hoglin Sleeping Household & Mead Longhouse (dx in -9..=9, dz in -14..=-7).
    if (-9..=9).contains(&dx) && (-14..=-7).contains(&dz) {
        let on_wall = dx.abs() == 9 || dz == -14 || dz == -7;
        let doorway = dz == -7 && dx.abs() <= 1;
        if on_wall && !doorway {
            for dy in 1..=4 {
                let m = if dx.abs() == 9 && (dz == -14 || dz == -7) {
                    OMat::CrimsonStem
                } else if dy == 2 && dx.abs() % 3 == 0 && dz == -14 {
                    OMat::IronBars
                } else if dy == 1 {
                    OMat::CrackedBricks
                } else {
                    OMat::CrimsonPlanks
                };
                u.push((dy, m));
            }
        }
        // Pitched brutal timber-and-basalt roof.
        let dist_from_ridge = (dz + 10).abs().min((dz + 11).abs());
        let roof_y = 6 - (dist_from_ridge / 2);
        u.push((
            roof_y,
            if dx.abs() % 4 == 0 {
                OMat::StrippedCrimsonStem
            } else {
                OMat::PolishedBasalt
            },
        ));
        if roof_y == 6 && dx.abs() == 0 {
            u.push((7, OMat::SoulLantern));
        }
        // Interior Hoglin sleeping wallows, hay/crimson fodder nests, and central hearth!
        if !on_wall {
            if dx == 0 && dz == -10 {
                u.push((1, OMat::Campfire));
                u.push((5, OMat::Chain));
            } else if (dx <= -3 && dx >= -7 || dx >= 3 && dx <= 7) && (dz <= -11) {
                u.push((
                    1,
                    if (dx + dz) % 2 == 0 {
                        OMat::HayBlock
                    } else {
                        OMat::NetherWartBlock
                    },
                ));
            }
            if (dx == -4 || dx == 4) && dz == -9 {
                u.push((1, OMat::CrimsonFence));
            }
        }
        return (floor, u);
    }

    // 3. West Sunken Hoglin Feeding Pen & Stable Alcove (dx in -14..=-8, dz in -4..=6).
    if (-14..=-8).contains(&dx) && (-4..=6).contains(&dz) {
        let on_fence = dx == -14 || dx == -8 || dz == -4 || dz == 6;
        let pen_gate = dx == -8 && (dz == 0 || dz == 1);
        if on_fence && !pen_gate {
            u.push((
                1,
                if (dx + dz) % 3 == 0 {
                    OMat::Basalt
                } else {
                    OMat::CrimsonFence
                },
            ));
            if (dx == -14 || dz == -4 || dz == 6) && (dx + dz) % 4 == 0 {
                u.push((2, OMat::IronBars));
            }
        } else if dx <= -11 && dz >= -2 && dz <= 4 {
            if dz % 3 == 0 && dx == -12 {
                u.push((1, OMat::HayBlock));
            }
            if dx == -13 && dz == 1 {
                u.push((1, OMat::Campfire));
            }
        }
        return (floor, u);
    }

    // 4. East Brutal War Room (dx in 7..=14, dz in -5..=7).
    if (7..=14).contains(&dx) && (-5..=7).contains(&dz) {
        let on_wall = dx == 7 || dx == 14 || dz == -5 || dz == 7;
        let war_door = dx == 7 && dz.abs() <= 1;
        if on_wall && !war_door {
            for dy in 1..=5 {
                let m = if (dx == 7 || dx == 14) && (dz == -5 || dz == 7) {
                    OMat::Obsidian
                } else if dy == 3 && (dz == 1 && dx == 14 || dx == 10 && dz.abs() == 5) {
                    OMat::IronBars
                } else if dy == 5 {
                    OMat::RedNetherBricks
                } else {
                    OMat::PolishedBricks
                };
                u.push((dy, m));
            }
        }
        u.push((
            6,
            if dx % 2 == 0 {
                OMat::Blackstone
            } else {
                OMat::PolishedBasalt
            },
        ));
        // Central War Table inside War Room (dx in 10..=11, dz in 0..=2).
        if (10..=11).contains(&dx) && (0..=2).contains(&dz) {
            u.push((
                1,
                if dx == 10 && dz == 1 {
                    OMat::GildedBlackstone
                } else {
                    OMat::ChiseledBlackstone
                },
            ));
            if dx == 11 && dz == 1 {
                u.push((2, OMat::SoulLantern));
            }
        }
        return (floor, u);
    }

    // 5. South-Center Grated Treasure Bunker (dx in -5..=5, dz in 8..=13).
    if (-5..=5).contains(&dx) && (8..=13).contains(&dz) {
        let on_wall = dx.abs() == 5 || dz == 8 || dz == 13;
        let vault_door = dz == 8 && dx.abs() <= 1;
        if on_wall && !vault_door {
            for dy in 1..=4 {
                u.push((
                    dy,
                    if dx.abs() == 5 || dz == 13 {
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
        if dx == 0 && dz == 11 {
            u.push((1, OMat::GoldBlock));
            u.push((2, OMat::CryingObsidian));
        }
        return (floor, u);
    }

    // Courtyard central War-Fire Hearth.
    if dx.abs() <= 1 && dz.abs() <= 1 {
        if dx == 0 && dz == 0 {
            u.push((1, OMat::Magma));
            u.push((2, OMat::SoulCampfire));
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

    let is_quench_pit = (dx >= -5 && dx <= -3 && dz >= -5 && dz <= 1)
        || (dx >= 3 && dx <= 5 && dz >= -5 && dz <= 1);
    let floor = if is_quench_pit {
        OMat::Obsidian
    } else if ax <= 2 || az <= 2 {
        OMat::PolishedBricks
    } else if roll % 5 == 0 {
        OMat::Magma
    } else {
        OMat::CrackedBricks
    };

    // Twin belching industrial smokestacks at (-9, -9) and (9, -9).
    for (sx, sz) in [(-9, -9), (9, -9)] {
        let sdx = (dx - sx).abs();
        let sdz = (dz - sz).abs();
        if sdx <= 2 && sdz <= 2 {
            let edge = sdx == 2 || sdz == 2;
            let corner = sdx == 2 && sdz == 2;
            if edge {
                let top_h = if corner { 20 } else { 22 };
                for dy in 1..=top_h {
                    if !corner && dy <= 2 && (sdx == 0 || sdz == 0) {
                        u.push((dy, OMat::IronBars));
                    } else {
                        u.push((
                            dy,
                            if dy % 6 == 0 {
                                OMat::RedNetherBricks
                            } else if dy >= 19 {
                                OMat::Obsidian
                            } else {
                                OMat::PolishedBasalt
                            },
                        ));
                    }
                }
            } else if sdx == 0 && sdz == 0 {
                u.push((1, OMat::Magma));
                u.push((2, OMat::Campfire));
                u.push((19, OMat::Campfire));
            }
            return (floor, u);
        }
    }

    // Outer heavy foundry wall (cheb == 13..=14) with South Foundry Gate.
    let gate = dz >= 12 && ax <= 2;
    if cheb >= 13 && !gate {
        let top = if ax >= 12 && az >= 12 { 9 } else { 6 };
        for dy in 1..=top {
            let m = if ax >= 12 && az >= 12 && dy == top {
                OMat::SoulCampfire
            } else if dy == 4 {
                OMat::RedNetherBricks
            } else if dy == 2 && (dx % 4 == 0 || dz % 4 == 0) {
                OMat::IronBars
            } else {
                OMat::Blackstone
            };
            u.push((dy, m));
        }
        return (floor, u);
    }

    // Enclosed Smelting Lava Basins (safely contained inside Obsidian rims & Iron Bars).
    if is_quench_pit {
        let rim = dx.abs() == 5 || dx.abs() == 3 || dz == -5 || dz == 1;
        if rim {
            u.push((1, OMat::Obsidian));
            if dz % 2 == 0 {
                u.push((2, OMat::IronBars));
            }
        } else {
            u.push((1, OMat::Lava));
        }
        return (floor, u);
    }

    // Overhead Iron-Chain Gantry Catwalk spanning foundry interior (dy = 6).
    if (ax == 2 || az == 2) && cheb <= 11 {
        u.push((6, OMat::IronBars));
        if cheb % 5 == 0 {
            u.push((7, OMat::Chain));
            u.push((8, OMat::PolishedBasalt));
        }
    }

    // Foreman Vault Bunker along South Wing (dx in -6..=6, dz in 6..=11).
    if (-6..=6).contains(&dx) && (6..=11).contains(&dz) {
        let wall = ax == 6 || dz == 6 || dz == 11;
        let door = dz == 6 && ax <= 1;
        if wall && !door {
            for dy in 1..=5 {
                u.push((
                    dy,
                    if ax == 6 && (dz == 6 || dz == 11) {
                        OMat::Obsidian
                    } else if dy == 2 && ax == 3 && dz == 6 {
                        OMat::IronBars
                    } else {
                        OMat::PolishedBricks
                    },
                ));
            }
        }
        u.push((5, OMat::Obsidian));
        if dx == 0 && dz == 9 {
            u.push((1, OMat::IronBlock));
            u.push((2, OMat::AncientDebris));
        }
    }

    // Iron / Gold Crucible Altar in central axis.
    if ax <= 1 && dz == -8 {
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

    let floor = if cheb <= 4 {
        OMat::PolishedBricks
    } else if ax <= 1 || az <= 1 {
        OMat::CrackedBricks
    } else if roll % 4 == 0 {
        OMat::Magma
    } else {
        OMat::Blackstone
    };

    // Spiked Siege Palisade (cheb == 10..=11) with South & North War Gates.
    let gate = ax <= 1 && az >= 9;
    if cheb >= 10 && !gate {
        let h = if ax >= 9 && az >= 9 {
            6
        } else if (dx + dz) % 2 == 0 {
            4
        } else {
            3
        };
        for dy in 1..=h {
            u.push((
                dy,
                if dy == h && h >= 4 {
                    OMat::Obsidian
                } else {
                    OMat::Blackstone
                },
            ));
        }
        if ax >= 9 && az >= 9 {
            u.push((7, OMat::SoulCampfire));
        }
        return (floor, u);
    }

    // Central Spiked Watchtower Spire (cheb <= 4).
    if cheb <= 4 {
        let tower_wall = cheb == 4 || (ax >= 3 && az >= 3);
        let door = (dz == 4 || dz == -4) && ax <= 1;
        if tower_wall {
            let max_h = if ax >= 3 && az >= 3 { 19 } else { 15 };
            for dy in 1..=max_h {
                if door && dy <= 3 {
                    continue;
                }
                let m = if dy == 6 || dy == 12 {
                    OMat::RedNetherBricks
                } else if dy >= 16 || (ax >= 3 && az >= 3 && dy % 5 == 0) {
                    OMat::Obsidian
                } else if dy == 9 && (ax == 0 || az == 0) {
                    OMat::IronBars
                } else {
                    OMat::PolishedBricks
                };
                u.push((dy, m));
            }
            if ax >= 3 && az >= 3 {
                u.push((20, OMat::CryingObsidian));
                u.push((21, OMat::Obsidian));
            }
        } else {
            // Tower floors (ground door, second war floor at dy=6, beacon summit at dy=13).
            u.push((6, OMat::NetherBricks));
            u.push((13, OMat::PolishedBricks));
            if cheb <= 1 && ax + az > 0 {
                u.push((14, OMat::Obsidian));
            }
            if dx == 0 && dz == 0 {
                u.push((1, OMat::GildedBlackstone));
                u.push((7, OMat::GoldBlock));
                u.push((14, OMat::CryingObsidian));
                u.push((15, OMat::Shroomlight));
                u.push((16, OMat::SoulSoil));
                u.push((17, OMat::SoulFire));
            }
        }
        return (floor, u);
    }

    // Outer barracks feeding pens & muster brazier.
    if dx == -7 && az <= 2 {
        u.push((1, OMat::CrimsonFence));
        if dz == 0 {
            u.push((1, OMat::HayBlock));
        }
    }
    if dx == 7 && dz == 0 {
        u.push((1, OMat::SoulCampfire));
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
        resolve("minecraft:iron_bars", blackstone),                        // 21 IronBars
        resolve("minecraft:chain", blackstone),                            // 22 Chain
        resolve("minecraft:campfire", blackstone),                         // 23 Campfire
        resolve("minecraft:soul_campfire", blackstone),                    // 24 SoulCampfire
        resolve("minecraft:soul_fire", blackstone),                        // 25 SoulFire
        resolve("minecraft:gold_block", blackstone),                       // 26 GoldBlock
        resolve("minecraft:iron_block", blackstone),                       // 27 IronBlock
        resolve("minecraft:ancient_debris", blackstone),                   // 28 AncientDebris
        resolve("minecraft:lava", blackstone),                             // 29 Lava
        resolve("minecraft:soul_lantern", blackstone),                     // 30 SoulLantern
        resolve("minecraft:hay_block", blackstone),                        // 31 HayBlock
    ];

    let half = outpost_half_extent(kind);
    let skirt = half + 3;
    let mut runs: Vec<BlockRun> = Vec::with_capacity(2048);

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
                // Graded entrance apron steps on cardinal axes so doors never cliff-drop.
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

            // Solid downward foundation pier until hitting real ground (never float!).
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
                OutpostKind::HoglinVillage => 10,
                _ => 24,
            };
            let clear_top = (floor_y + clear_h).max(top_existing + 2).min(OVERWORLD_MAX_Y - 1);
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
    match kind {
        OutpostKind::HoglinVillage => {
            // Household ration chest, War-Room tactical chest, and Twin Treasure Vault chests.
            place_chest_with_loot(
                host,
                ow,
                [cx - 6, fy + 1, cz - 12],
                "minecraft:chests/bastion_hoglin_stable",
                base_seed ^ 0x1101,
            );
            place_chest_with_loot(
                host,
                ow,
                [cx + 12, fy + 1, cz + 1],
                "minecraft:chests/bastion_other",
                base_seed ^ 0x1102,
            );
            place_chest_with_loot(
                host,
                ow,
                [cx - 2, fy + 1, cz + 11],
                "minecraft:chests/bastion_treasure",
                base_seed ^ 0x1103,
            );
            place_chest_with_loot(
                host,
                ow,
                [cx + 2, fy + 1, cz + 11],
                "minecraft:chests/bastion_treasure",
                base_seed ^ 0x1104,
            );
            place_block_if_loaded(host, ow, [cx + 9, fy + 1, cz - 3], "minecraft:smithing_table");
            place_block_if_loaded(host, ow, [cx + 9, fy + 1, cz + 4], "minecraft:anvil");
            place_block_if_loaded(host, ow, [cx + 12, fy + 1, cz - 3], "minecraft:crafting_table");
            place_block_if_loaded(host, ow, [cx - 11, fy + 1, cz - 1], "minecraft:cauldron");
        }
        OutpostKind::InfernalFoundry => {
            place_chest_with_loot(
                host,
                ow,
                [cx - 3, fy + 1, cz + 9],
                "minecraft:chests/bastion_treasure",
                base_seed ^ 0x2201,
            );
            place_chest_with_loot(
                host,
                ow,
                [cx + 3, fy + 1, cz + 9],
                "minecraft:chests/nether_bridge",
                base_seed ^ 0x2202,
            );
            place_chest_with_loot(
                host,
                ow,
                [cx, fy + 1, cz - 10],
                "minecraft:chests/nether_bridge",
                base_seed ^ 0x2203,
            );
            for bx in [-4, -2, 2, 4] {
                place_block_if_loaded(host, ow, [cx + bx, fy + 1, cz - 7], "minecraft:blast_furnace");
            }
            place_block_if_loaded(host, ow, [cx - 6, fy + 1, cz + 3], "minecraft:anvil");
            place_block_if_loaded(host, ow, [cx + 6, fy + 1, cz + 3], "minecraft:smithing_table");
            place_block_if_loaded(host, ow, [cx - 6, fy + 1, cz - 3], "minecraft:lava_cauldron");
            place_block_if_loaded(host, ow, [cx + 6, fy + 1, cz - 3], "minecraft:lava_cauldron");
        }
        OutpostKind::SiegeWatchtower => {
            place_chest_with_loot(
                host,
                ow,
                [cx + 2, fy + 7, cz],
                "minecraft:chests/bastion_treasure",
                base_seed ^ 0x3301,
            );
            place_chest_with_loot(
                host,
                ow,
                [cx - 2, fy + 1, cz - 2],
                "minecraft:chests/bastion_other",
                base_seed ^ 0x3302,
            );
            place_block_if_loaded(host, ow, [cx + 2, fy + 1, cz + 2], "minecraft:anvil");
            place_block_if_loaded(host, ow, [cx - 6, fy + 1, cz + 3], "minecraft:crafting_table");
        }
    }
}

fn armor_captain(host: &mut dyn ServerHost, eid: u32, title: &str) {
    let Some(ent) = host.entity(eid) else {
        return;
    };
    let _ = ent.set_custom_name(host, Some(Text::literal(title.to_owned())));
    if let Some(living) = host.living(ent) {
        let _ = living.set_attribute_base(host, "minecraft:max_health", 70.0);
        let _ = living.set_health(host, 70.0);
        for (slot, id) in [
            (EquipmentSlot::Head, "minecraft:golden_helmet"),
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

fn spawn_outpost_garrison(
    host: &mut dyn ServerHost,
    state: &mut RiftState,
    cx: i32,
    fy: i32,
    cz: i32,
    kind: OutpostKind,
) {
    let roster: &[(&str, [i32; 3], Option<&str>)] = match kind {
        OutpostKind::HoglinVillage => &[
            ("minecraft:piglin_brute", [10, 1, -2], Some("War-Kraal Captain")),
            ("minecraft:hoglin", [-5, 1, -10], None),
            ("minecraft:hoglin", [5, 1, -10], None),
            ("minecraft:hoglin", [-11, 1, 1], None),
            ("minecraft:hoglin", [-11, 1, -2], None),
            ("minecraft:hoglin", [0, 1, 5], None),
            ("minecraft:piglin", [-2, 1, 2], None),
            ("minecraft:piglin", [4, 1, -4], None),
            ("minecraft:piglin", [0, 1, 10], None),
            ("minecraft:piglin", [11, 1, 4], None),
        ],
        OutpostKind::InfernalFoundry => &[
            ("minecraft:piglin_brute", [0, 1, 8], Some("Forge-Master Captain")),
            ("minecraft:wither_skeleton", [-4, 1, -7], None),
            ("minecraft:wither_skeleton", [4, 1, -7], None),
            ("minecraft:wither_skeleton", [0, 1, 0], None),
            ("minecraft:blaze", [-9, 4, -6], None),
            ("minecraft:blaze", [9, 4, -6], None),
            ("minecraft:piglin", [-7, 1, 4], None),
            ("minecraft:piglin", [7, 1, 4], None),
        ],
        OutpostKind::SiegeWatchtower => &[
            ("minecraft:piglin_brute", [0, 7, 1], Some("Siege Captain")),
            ("minecraft:wither_skeleton", [0, 14, 1], None),
            ("minecraft:wither_skeleton", [2, 1, -2], None),
            ("minecraft:hoglin", [-7, 1, 0], None),
            ("minecraft:hoglin", [7, 1, 4], None),
            ("minecraft:piglin", [-6, 1, -5], None),
            ("minecraft:piglin", [6, 1, -5], None),
            ("minecraft:piglin", [0, 1, 8], None),
        ],
    };

    for (mob_kind, [dx, dy, dz], captain_title) in roster {
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
    fn outposts_cycle_through_all_three_mordor_archetypes_with_solid_grounding() {
        let mut rift = matured_rift();
        let mut seen = Vec::new();
        for placed in 0..3u32 {
            rift.outposts_placed = placed;
            rift.corrupted_blocks = 500_000;
            rift.ring_radius = 120;
            let w = ConnectedWasteland {
                total_corrupted_blocks: 500_000,
                total_area_radius_sq: 120 * 120,
                max_deepen_radius: 25,
                max_ring_radius: 120,
                total_bastions_placed: 0,
                total_outposts_placed: placed,
                member_count: 1,
            };
            seen.push(wasteland_eligible_for_outpost(&rift, w).expect("should unlock outpost"));
        }
        assert!(seen.contains(&OutpostKind::HoglinVillage));
        assert!(seen.contains(&OutpostKind::InfernalFoundry));
        assert!(seen.contains(&OutpostKind::SiegeWatchtower));

        let mut st = RiftState::new();
        st.rifts.push(rift);
        assert!(!site_clear_for_outpost(&st, 20, 20));
        assert!(site_clear_for_outpost(&st, 80, 20));
    }
}
