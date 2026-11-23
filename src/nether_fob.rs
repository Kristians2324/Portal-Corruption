use crate::bastion::is_fillable_terrain_void;
use crate::mobs::is_piglin_or_hoglin;
use crate::state::{ConnectedWasteland, PendingNetherFob, RiftState};
use crate::types::{
    NETHER, NETHER_MAX_Y, NETHER_MIN_Y, NetherFlavor, RiftSource, horiz_dist_sq, spatial_hash,
    strip_ns,
};
use mod_api::{
    BlockEdit, BlockRun, BlockState, BlockUpdateFlags, Compound, EquipmentSlot, Identifier,
    ItemStack, LevelRef, ServerHost, SoundSource, Tag, Text,
};
use std::collections::{HashSet, VecDeque};

const NETHER_FOB_CHECK_INTERVAL_TICKS: u64 = 160;
const NETHER_FOB_OFFSET_TICK: u64 = 95;
const MIN_NETHER_FOB_SPACING: i64 = 52;
pub const NETHER_FOB_HALF_EXTENT: i32 = 22;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum FMat {
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
    CrimsonPlanks = 13,
    CrimsonFence = 14,
    SoulSoil = 15,
    Magma = 16,
    Shroomlight = 17,
    NetherWartBlock = 18,
    IronBars = 19,
    SoulCampfire = 20,
    SoulFire = 21,
    GoldBlock = 22,
    IronBlock = 23,
    AncientDebris = 24,
    HayBlock = 25,
    SoulLantern = 26,
    WarpedWartBlock = 27,
}

#[inline]
fn push_range(u: &mut Vec<(i32, FMat)>, y0: i32, y1: i32, mat: FMat) {
    for dy in y0..=y1 {
        u.push((dy, mat));
    }
}

pub fn rift_eligible_for_nether_fob(
    state: &RiftState,
    rift: &RiftSource,
    wasteland: ConnectedWasteland,
) -> bool {
    if state.nether_fob_rifts.contains(&rift.id) || rift.contained {
        return false;
    }
    rift.citadel_spawned || crate::citadel::wasteland_eligible_for_citadel(rift, wasteland)
}

pub fn release_nether_fob_chunks(host: &mut dyn ServerHost, state: &mut RiftState) {
    let Some(nether) = host.level(NETHER) else {
        state.pending_nether_fob = None;
        state.nether_fob_release_chunks.clear();
        state.nether_fob_release_ttl = 0;
        return;
    };
    if let Some(pending) = state.pending_nether_fob.take() {
        for c in pending.forced_chunks {
            if let Some(cref) = nether.chunk_ref(host, c) {
                let _ = cref.set_forced(host, false);
            }
        }
    }
    for c in state.nether_fob_release_chunks.drain(..) {
        if let Some(cref) = nether.chunk_ref(host, c) {
            let _ = cref.set_forced(host, false);
        }
    }
    state.nether_fob_release_ttl = 0;
}

pub fn tick_nether_fob_emergence(host: &mut dyn ServerHost, state: &mut RiftState) {
    let Some(nether) = host.level(NETHER) else {
        return;
    };
    tick_nether_dimension(host, nether, state);
}

pub fn tick_nether_dimension(host: &mut dyn ServerHost, nether: LevelRef, state: &mut RiftState) {
    if state.nether_fob_release_ttl > 0 {
        state.nether_fob_release_ttl -= 1;
        if state.nether_fob_release_ttl == 0 {
            for c in state.nether_fob_release_chunks.drain(..) {
                if let Some(cref) = nether.chunk_ref(host, c) {
                    let _ = cref.set_forced(host, false);
                }
            }
        }
    }

    if state.pending_nether_fob.is_some() {
        process_pending_nether_fob(host, nether, state);
        return;
    }

    if state.paused || state.tick % NETHER_FOB_CHECK_INTERVAL_TICKS != NETHER_FOB_OFFSET_TICK {
        return;
    }

    let min_sp_sq = MIN_NETHER_FOB_SPACING * MIN_NETHER_FOB_SPACING;
    for idx in 0..state.rifts.len() {
        let wasteland = state.connected_wasteland_for(idx);
        let rift = &state.rifts[idx];
        if !rift_eligible_for_nether_fob(state, rift, wasteland) {
            continue;
        }
        let nx = rift.center[0].div_euclid(8);
        let nz = rift.center[2].div_euclid(8);
        if state
            .nether_fobs
            .iter()
            .any(|f| horiz_dist_sq(*f, [nx, 64, nz]) < min_sp_sq)
        {
            state.nether_fob_rifts.insert(rift.id);
            state.dirty = true;
            continue;
        }
        let roll = spatial_hash(
            rift.center[0],
            rift.center[1],
            rift.center[2],
            state.seed ^ state.tick ^ 0xF0B4_E77E,
        );
        stage_pending_nether_fob(
            host,
            nether,
            state,
            rift.id,
            [nx, nz],
            rift.center[1],
            rift.flavor,
            roll,
        );
        break;
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_pending_nether_fob(
    host: &mut dyn ServerHost,
    nether: LevelRef,
    state: &mut RiftState,
    rift_id: u64,
    center_xz: [i32; 2],
    ow_y: i32,
    flavor: NetherFlavor,
    roll: u64,
) {
    let pad = NETHER_FOB_HALF_EXTENT + 6;
    let min_cx = (center_xz[0] - pad) >> 4;
    let max_cx = (center_xz[0] + pad) >> 4;
    let min_cz = (center_xz[1] - pad) >> 4;
    let max_cz = (center_xz[1] + pad) >> 4;

    let mut forced_chunks = Vec::new();
    for ccx in min_cx..=max_cx {
        for ccz in min_cz..=max_cz {
            let chunk = [ccx, ccz];
            if let Some(cref) = nether.chunk_ref(host, chunk)
                && cref.set_forced(host, true)
            {
                forced_chunks.push(chunk);
            }
        }
    }

    state.pending_nether_fob = Some(PendingNetherFob {
        rift_id,
        center_xz,
        ow_y,
        flavor,
        roll,
        forced_chunks,
        queued_tick: state.tick,
    });
}

fn find_existing_nether_portal(
    host: &dyn ServerHost,
    nether: LevelRef,
    cx: i32,
    cz: i32,
) -> Option<([i32; 3], HashSet<[i32; 3]>)> {
    let mut hit: Option<[i32; 3]> = None;
    'outer: for r in (0..=22i32).step_by(2) {
        for dx in (-r..=r).step_by(2) {
            for dz in (-r..=r).step_by(2) {
                if dx.abs().max(dz.abs()) != r {
                    continue;
                }
                let x = cx + dx;
                let z = cz + dz;
                if !nether.is_loaded(host, [x, 64, z]) {
                    continue;
                }
                for y in (16..=118).step_by(2) {
                    if let Some(k) = nether.block(host, [x, y, z])
                        && strip_ns(&k.id(host)) == "nether_portal"
                    {
                        hit = Some([x, y, z]);
                        break 'outer;
                    }
                }
            }
        }
    }

    let seed_pos = hit?;
    // Flood-fill connected portal frame + obsidian border to locate its exact walkable base sill.
    let mut portal_cells = HashSet::new();
    let mut protected = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(seed_pos);
    portal_cells.insert(seed_pos);
    protected.insert(seed_pos);

    while let Some(p) = queue.pop_front() {
        for d in [
            [1, 0, 0],
            [-1, 0, 0],
            [0, 1, 0],
            [0, -1, 0],
            [0, 0, 1],
            [0, 0, -1],
        ] {
            let np = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
            if protected.len() >= 96 {
                continue;
            }
            if let Some(k) = nether.block(host, np) {
                let s = k.id(host);
                let short = strip_ns(&s);
                if short == "nether_portal" {
                    if portal_cells.insert(np) {
                        queue.push_back(np);
                    }
                    protected.insert(np);
                } else if matches!(short, "obsidian" | "crying_obsidian") {
                    protected.insert(np);
                }
            }
        }
    }

    let min_x = portal_cells.iter().map(|p| p[0]).min().unwrap_or(seed_pos[0]);
    let max_x = portal_cells.iter().map(|p| p[0]).max().unwrap_or(seed_pos[0]);
    let min_z = portal_cells.iter().map(|p| p[2]).min().unwrap_or(seed_pos[2]);
    let max_z = portal_cells.iter().map(|p| p[2]).max().unwrap_or(seed_pos[2]);
    let min_portal_y = portal_cells.iter().map(|p| p[1]).min().unwrap_or(seed_pos[1]);

    let px = (min_x + max_x) / 2;
    let pz = (min_z + max_z) / 2;
    let floor_y = (min_portal_y - 1).clamp(32, 108);
    Some(([px, floor_y, pz], protected))
}

fn pick_safe_nether_floor_y(host: &dyn ServerHost, nether: LevelRef, cx: i32, cz: i32, ow_y: i32) -> i32 {
    let preferred = ow_y.clamp(42, 88);
    let mut best_y: Option<(i32, i32)> = None;
    for dx in [-8, 0, 8] {
        for dz in [-8, 0, 8] {
            let x = cx + dx;
            let z = cz + dz;
            if !nether.is_loaded(host, [x, 64, z]) {
                continue;
            }
            for y in (34..=96).step_by(2) {
                let b0 = nether.block(host, [x, y, z]).map(|k| k.id(host)).unwrap_or_default();
                let s0 = strip_ns(&b0);
                if b0.is_empty() || is_fillable_terrain_void(s0) {
                    continue;
                }
                let head_clear = (1..=6).all(|dy| {
                    nether
                        .block(host, [x, y + dy, z])
                        .is_some_and(|k| strip_ns(&k.id(host)) == "air")
                });
                if head_clear {
                    let score = (y - preferred).abs();
                    if best_y.is_none_or(|(best_score, _)| score < best_score) {
                        best_y = Some((score, y));
                    }
                }
            }
        }
    }
    best_y.map(|(_, y)| y).unwrap_or(52)
}

fn process_pending_nether_fob(host: &mut dyn ServerHost, nether: LevelRef, state: &mut RiftState) {
    let Some(pending) = state.pending_nether_fob.clone() else {
        return;
    };

    if state.nether_fob_rifts.contains(&pending.rift_id) {
        release_nether_fob_chunks(host, state);
        return;
    }

    if state.tick.saturating_sub(pending.queued_tick) > 240 {
        release_nether_fob_chunks(host, state);
        return;
    }

    let all_loaded = pending
        .forced_chunks
        .iter()
        .all(|[cx, cz]| nether.is_loaded(host, [cx << 4, 64, cz << 4]));
    if !all_loaded {
        return;
    }

    state.pending_nether_fob = None;

    let (center, protected) = if let Some((existing_center, prot)) =
        find_existing_nether_portal(host, nether, pending.center_xz[0], pending.center_xz[1])
    {
        (existing_center, prot)
    } else {
        let fy = pick_safe_nether_floor_y(
            host,
            nether,
            pending.center_xz[0],
            pending.center_xz[1],
            pending.ow_y,
        );
        ([pending.center_xz[0], fy, pending.center_xz[1]], HashSet::new())
    };

    build_nether_fob_structure(
        host,
        nether,
        state,
        center[0],
        center[1],
        center[2],
        pending.flavor,
        pending.roll,
        &protected,
    );

    state.nether_fob_rifts.insert(pending.rift_id);
    state.nether_fobs.push(center);
    state.nether_fob_release_chunks = pending.forced_chunks;
    state.nether_fob_release_ttl = 8;
    state.dirty = true;

    let msg = format!(
        "[Rift Bleed] Across the Nether veil, an armed Hellgate Forward Operating Base stands fortified at Nether ({}, {}, {})!",
        center[0], center[1], center[2]
    );
    for player in host.players().server_players() {
        let _ = player.tell(host, Text::literal(msg.clone()));
    }
    let _ = nether.play_sound(
        host,
        "minecraft:entity.ender_dragon.growl",
        [
            f64::from(center[0]) + 0.5,
            f64::from(center[1]) + 3.0,
            f64::from(center[2]) + 0.5,
        ],
        SoundSource::Hostile,
        0.8,
        0.75,
    );
}

/// Pure column geometry blueprint for the Nether Forward Operating Base (`45x45`, `half = 22`).
fn plan_nether_fob_col(
    dx: i32,
    dz: i32,
    flavor: NetherFlavor,
    roll: u64,
) -> (FMat, Vec<(i32, FMat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let mut u = Vec::new();

    let accent = if flavor == NetherFlavor::WarpedForest {
        FMat::WarpedWartBlock
    } else {
        FMat::NetherWartBlock
    };

    let on_road = ax <= 2 || az <= 2;
    let in_nw_pens = (-17..=-9).contains(&dx) && (-17..=-9).contains(&dz);
    let in_sw_barracks = (-17..=-9).contains(&dx) && (9..=17).contains(&dz);
    let in_ne_forge = (9..=17).contains(&dx) && (-17..=-9).contains(&dz);
    let in_se_forge = (9..=17).contains(&dx) && (9..=17).contains(&dz);
    let in_vault_bunker = (12..=18).contains(&ax) && (-8..=-4).contains(&dz);

    let floor = if cheb <= 4 {
        if ax == az || ax <= 1 || az <= 1 {
            FMat::GildedBlackstone
        } else {
            FMat::PolishedBricks
        }
    } else if cheb <= 7 {
        if cheb == 7 {
            FMat::PolishedBricks
        } else {
            FMat::CryingObsidian
        }
    } else if on_road {
        if roll % 5 == 0 {
            FMat::GildedBlackstone
        } else {
            FMat::PolishedBricks
        }
    } else if in_nw_pens {
        FMat::SoulSoil
    } else if in_sw_barracks || in_ne_forge || in_se_forge || in_vault_bunker {
        FMat::PolishedBricks
    } else if roll % 7 == 0 {
        FMat::Magma
    } else {
        FMat::Blackstone
    };

    // 1. Perimeter Siege Bastion Curtain Walls & Corner Keep Towers (cheb == 20..=22).
    let in_gate = (ax <= 2 && az >= 20) || (az <= 2 && ax >= 20);
    if cheb >= 21 {
        let corner = ax >= 18 && az >= 18;
        let gate_tower = (az >= 20 && (3..=4).contains(&ax)) || (ax >= 20 && (3..=4).contains(&az));
        if in_gate {
            push_range(&mut u, 5, 7, FMat::Obsidian);
            u.push((8, FMat::RedNetherBricks));
        } else if corner {
            push_range(&mut u, 1, 13, FMat::PolishedBricks);
            u.push((14, FMat::Obsidian));
            u.push((15, FMat::SoulCampfire));
        } else if gate_tower {
            push_range(&mut u, 1, 9, FMat::PolishedBricks);
            u.push((10, FMat::Obsidian));
            u.push((11, FMat::SoulCampfire));
        } else {
            let spike = (dx + dz).abs() % 3 == 0;
            let top = if spike { 8 } else { 7 };
            for dy in 1..=top {
                let m = if dy == 4 {
                    FMat::RedNetherBricks
                } else if dy == 5 && !spike && (ax % 2 == 0 || az % 2 == 0) {
                    FMat::IronBars
                } else if dy == top {
                    FMat::Obsidian
                } else {
                    FMat::Blackstone
                };
                u.push((dy, m));
            }
        }
        return (floor, u);
    }

    if cheb == 20 && !in_gate {
        let pier = (dx + dz).abs() % 3 == 0 || ax == 19 || az == 19;
        if pier {
            push_range(&mut u, 1, 3, FMat::PolishedBasalt);
        }
        u.push((4, FMat::PolishedBricks));
        return (floor, u);
    }

    // Courtyard staircase climbing onto West Sentry Wall-Walk.
    if dx == -19 && (11..=14).contains(&dz) {
        push_range(&mut u, 1, dz - 10, FMat::CrackedBricks);
        return (floor, u);
    }

    // 2. Central Invasion Portal Triumphal Ring & Pylons (cheb in 5..=6).
    if (5..=6).contains(&cheb) {
        let pylon = ax >= 5 && az >= 5;
        if pylon {
            push_range(&mut u, 1, 13, FMat::PolishedBricks);
            u.push((14, FMat::Shroomlight));
            u.push((15, FMat::SoulSoil));
            u.push((16, FMat::SoulFire));
        } else if cheb == 5 {
            // Overhead triumphal portal canopy ribs (connected across all 4 pylons).
            push_range(&mut u, 9, 10, FMat::Obsidian);
            u.push((11, accent));
        } else {
            push_range(&mut u, 9, 10, FMat::RedNetherBricks);
        }
        return (floor, u);
    }

    // 3. NW Hoglin Cavalry War-Pens & Covered Stables (dx in -17..=-9, dz in -17..=-9).
    if in_nw_pens {
        let shed = (-17..=-13).contains(&dx);
        if shed {
            let corner_post = (dx == -17 || dx == -13) && (dz == -17 || dz == -9);
            if corner_post {
                push_range(&mut u, 1, 4, FMat::CrimsonStem);
            } else if dx == -16 && (dz == -16 || dz == -10) {
                u.push((1, FMat::HayBlock));
            }
            u.push((4, FMat::CrimsonPlanks));
            if (-16..=-14).contains(&dx) {
                u.push((5, accent));
            }
            return (floor, u);
        }
        let fence = dx == -9 || dz == -17 || dz == -9;
        let gate = dx == -9 && dz == -13;
        if fence && !gate {
            u.push((1, FMat::CrimsonFence));
            if dz == -17 && dx % 4 == 0 {
                u.push((2, FMat::SoulLantern));
            }
        } else if dx == -11 && dz == -15 {
            u.push((1, FMat::NetherWartBlock));
        }
        return (floor, u);
    }

    // 4. SW Piglin Staging Barracks (dx in -17..=-9, dz in 9..=17).
    if in_sw_barracks {
        let on_wall = dx == -17 || dx == -9 || dz == 9 || dz == 17;
        let door = dx == -9 && dz == 13;
        if on_wall && !door {
            for dy in 1..=5 {
                let m = if dy == 3 && !door && (dx == -13 || dz == 13) {
                    FMat::IronBars
                } else if dy == 5 {
                    FMat::RedNetherBricks
                } else {
                    FMat::PolishedBricks
                };
                u.push((dy, m));
            }
        }
        u.push((5, FMat::CrimsonPlanks));
        if (-16..=-10).contains(&dx) && (10..=16).contains(&dz) {
            u.push((6, FMat::Obsidian));
        }
        if dx == -15 && dz == 15 {
            u.push((1, FMat::GildedBlackstone));
            u.push((2, FMat::SoulLantern));
        }
        return (floor, u);
    }

    // 5. NE & SE Hellforge Armory & Ordnance Pavilions (dx in 9..=17, |dz| in 9..=17).
    if in_ne_forge || in_se_forge {
        let corner_pier = (dx == 9 || dx == 17) && (az == 9 || az == 17);
        if corner_pier {
            push_range(&mut u, 1, 5, FMat::PolishedBasalt);
        }
        u.push((5, FMat::NetherBricks));
        if (10..=16).contains(&dx) && (10..=16).contains(&az) {
            u.push((6, FMat::RedNetherBricks));
        }
        if dx == 11 && az == 11 {
            u.push((1, FMat::IronBlock));
            u.push((2, FMat::GoldBlock));
        } else if dx == 15 && az == 15 {
            u.push((1, FMat::Obsidian));
            u.push((2, FMat::AncientDebris));
        }
        return (floor, u);
    }

    // 6. West & East Obsidian Invasion Command Vaults (ax in 12..=18, dz in -8..=-4).
    if in_vault_bunker {
        let wall = ax == 12 || ax == 18 || dz == -8 || dz == -4;
        let door = ax == 15 && dz == -4;
        if wall && !door {
            for dy in 1..=5 {
                let m = if dy == 2 && dz == -4 && ax == 13 {
                    FMat::IronBars
                } else if dy == 5 {
                    FMat::GildedBlackstone
                } else {
                    FMat::Obsidian
                };
                u.push((dy, m));
            }
        }
        u.push((5, FMat::Obsidian));
        u.push((6, FMat::RedNetherBricks));
        if ax == 15 && dz == -7 {
            u.push((1, FMat::GoldBlock));
            u.push((2, FMat::AncientDebris));
            u.push((3, FMat::Shroomlight));
        }
        return (floor, u);
    }

    // Courtyard Torch Obelisks along East/West Armory Flank.
    if ax == 9 && dz == 0 {
        // Road stays open; place lanterns at diagonal corners (+-7, +-8).
    } else if ax == 7 && az == 8 {
        push_range(&mut u, 1, 2, FMat::ChiseledBlackstone);
        u.push((3, FMat::SoulLantern));
    }

    (floor, u)
}

#[allow(clippy::too_many_arguments)]
fn build_nether_fob_structure(
    host: &mut dyn ServerHost,
    nether: LevelRef,
    state: &mut RiftState,
    cx: i32,
    floor_y: i32,
    cz: i32,
    flavor: NetherFlavor,
    roll: u64,
    protected_portal: &HashSet<[i32; 3]>,
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
        resolve("minecraft:crimson_planks", blackstone),                   // 13 CrimsonPlanks
        resolve("minecraft:crimson_fence", blackstone),                    // 14 CrimsonFence
        resolve("minecraft:soul_soil", blackstone),                        // 15 SoulSoil
        resolve("minecraft:magma_block", blackstone),                      // 16 Magma
        resolve("minecraft:shroomlight", blackstone),                      // 17 Shroomlight
        resolve("minecraft:nether_wart_block", blackstone),                // 18 NetherWartBlock
        resolve("minecraft:iron_bars", blackstone),                        // 19 IronBars
        resolve("minecraft:soul_campfire", blackstone),                    // 20 SoulCampfire
        resolve("minecraft:soul_fire", blackstone),                        // 21 SoulFire
        resolve("minecraft:gold_block", blackstone),                       // 22 GoldBlock
        resolve("minecraft:iron_block", blackstone),                       // 23 IronBlock
        resolve("minecraft:ancient_debris", blackstone),                   // 24 AncientDebris
        resolve("minecraft:hay_block", blackstone),                        // 25 HayBlock
        resolve("minecraft:soul_lantern", blackstone),                     // 26 SoulLantern
        resolve("minecraft:warped_wart_block", blackstone),                // 27 WarpedWartBlock
    ];

    let half = NETHER_FOB_HALF_EXTENT;
    let mut runs: Vec<BlockRun> = Vec::with_capacity(4096);
    let build_fresh_portal = protected_portal.is_empty();

    for dx in -half..=half {
        for dz in -half..=half {
            let x = cx + dx;
            let z = cz + dz;
            if !nether.is_loaded(host, [x, 64, z]) {
                continue;
            }
            let ax = dx.abs();
            let az = dz.abs();
            let cheb = ax.max(az);
            let col_roll = spatial_hash(x, floor_y, z, roll);

            // Drop stone foundation piers into open lava seas / caverns beneath walls, portal dais, and grid supports.
            let needs_pier =
                cheb >= 20 || cheb <= 7 || (dx % 6 == 0 && dz % 6 == 0);
            if needs_pier {
                let mut down_y = floor_y;
                for y in (NETHER_MIN_Y..floor_y).rev() {
                    let b = nether.block(host, [x, y, z]).map(|k| k.id(host)).unwrap_or_default();
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
                            FMat::Basalt as u32
                        } else {
                            FMat::Blackstone as u32
                        },
                        only: None,
                    });
                }
            } else if floor_y - 1 >= NETHER_MIN_Y {
                // Solid 2-block underside deck under courtyard over lava lakes.
                runs.push(BlockRun {
                    x,
                    z,
                    y: floor_y - 1,
                    height: 1,
                    surface: None,
                    block: FMat::Blackstone as u32,
                    only: None,
                });
            }

            let (floor_mat, mut upper) = plan_nether_fob_col(dx, dz, flavor, col_roll);

            // If no portal existed yet in the Nether, frame a fresh centered 4x5 Obsidian portal on dy=0.
            if build_fresh_portal && dz == 0 && (-1..=2).contains(&dx) {
                if dx == -1 || dx == 2 {
                    push_range(&mut upper, 1, 4, FMat::Obsidian);
                } else {
                    upper.push((4, FMat::Obsidian));
                }
            }

            let clear_top = (floor_y + 18).min(NETHER_MAX_Y);
            let touches_protected = (ax <= 5 && az <= 5)
                && (floor_y..=clear_top).any(|y| protected_portal.contains(&[x, y, z]));

            if !touches_protected {
                runs.push(BlockRun {
                    x,
                    z,
                    y: floor_y,
                    height: 1,
                    surface: None,
                    block: if build_fresh_portal && dz == 0 && (-1..=2).contains(&dx) {
                        FMat::Obsidian as u32
                    } else {
                        floor_mat as u32
                    },
                    only: None,
                });
                if clear_top > floor_y {
                    runs.push(BlockRun {
                        x,
                        z,
                        y: floor_y + 1,
                        height: (clear_top - floor_y) as u32,
                        surface: None,
                        block: FMat::Air as u32,
                        only: None,
                    });
                }
                for (dy, mat) in upper {
                    let ty = floor_y + dy;
                    if ty <= NETHER_MAX_Y {
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
            } else {
                if !protected_portal.contains(&[x, floor_y, z]) {
                    runs.push(BlockRun {
                        x,
                        z,
                        y: floor_y,
                        height: 1,
                        surface: None,
                        block: floor_mat as u32,
                        only: None,
                    });
                }
                for y in (floor_y + 1)..=clear_top {
                    if protected_portal.contains(&[x, y, z]) {
                        continue;
                    }
                    let dy = y - floor_y;
                    let mat = upper
                        .iter()
                        .rfind(|(udy, _)| *udy == dy)
                        .map(|(_, m)| *m)
                        .unwrap_or(FMat::Air);
                    runs.push(BlockRun {
                        x,
                        z,
                        y,
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
    let _ = host.edit_blocks(NETHER, &edit);

    if build_fresh_portal && let Some(fire) = host.block_state_id("minecraft:fire") {
        let _ = nether.set_block(
            host,
            [cx, floor_y + 1, cz],
            fire,
            BlockUpdateFlags::CLIENTS | BlockUpdateFlags::NEIGHBORS,
        );
    }

    place_fob_details_and_chests(host, nether, cx, floor_y, cz, roll);
    spawn_fob_garrison(host, state, cx, floor_y, cz);
}

pub(crate) fn fob_chest_specs() -> &'static [([i32; 3], bool, &'static str, u64)] {
    &[
        ([-15, 1, -6], true, "minecraft:chests/bastion_treasure", 0xF0B1_0001),
        ([15, 1, -6], true, "minecraft:chests/bastion_treasure", 0xF0B1_0002),
        ([-15, 1, -13], false, "minecraft:chests/bastion_hoglin_stable", 0xF0B1_0003),
        ([-13, 1, 13], false, "minecraft:chests/bastion_other", 0xF0B1_0004),
        ([13, 1, -13], false, "minecraft:chests/nether_bridge", 0xF0B1_0005),
        ([13, 1, 13], false, "minecraft:chests/nether_bridge", 0xF0B1_0006),
    ]
}

fn place_fob_details_and_chests(
    host: &mut dyn ServerHost,
    nether: LevelRef,
    cx: i32,
    fy: i32,
    cz: i32,
    roll: u64,
) {
    let seed_base = (roll & 0x7FFF_FFFF) as i64;
    if let Some(chest) = host.block_state_id("minecraft:chest") {
        for ([dx, dy, dz], _, table, salt) in fob_chest_specs() {
            let pos = [cx + dx, fy + dy, cz + dz];
            if nether.is_loaded(host, pos)
                && nether
                    .set_block(host, pos, chest, BlockUpdateFlags::CLIENTS)
                    .unwrap_or(false)
                && let Some(be) = host.block_entity(nether, pos)
            {
                let mut data = Compound::new();
                data.insert("LootTable".into(), Tag::String((*table).into()));
                data.insert("LootTableSeed".into(), Tag::Long(seed_base ^ (*salt as i64)));
                let _ = be.merge_data(host, data);
            }
        }
    }

    for ([dx, dy, dz], id) in [
        ([11, 1, -15], "minecraft:blast_furnace"),
        ([13, 1, -15], "minecraft:blast_furnace"),
        ([15, 1, -11], "minecraft:anvil"),
        ([15, 1, 11], "minecraft:smithing_table"),
        ([11, 1, 15], "minecraft:lava_cauldron"),
        ([13, 1, 15], "minecraft:brewing_stand"),
        ([-11, 1, 11], "minecraft:crafting_table"),
        ([-15, 1, 11], "minecraft:barrel"),
        ([-14, 1, -15], "minecraft:cauldron"),
        ([0, 1, -5], "minecraft:lodestone"),
        ([0, 1, 5], "minecraft:respawn_anchor"),
    ] {
        let p = [cx + dx, fy + dy, cz + dz];
        if nether.is_loaded(host, p)
            && let Some(st) = host.block_state_id(id)
        {
            let _ = nether.set_block(host, p, st, BlockUpdateFlags::CLIENTS);
        }
    }
}

pub(crate) fn fob_garrison_specs() -> &'static [(&'static str, [i32; 3], Option<&'static str>)] {
    &[
        ("minecraft:piglin_brute", [-4, 1, 4], Some("Overworld Vanguard Commander")),
        ("minecraft:piglin_brute", [15, 1, -5], Some("Hellgate Quartermaster")),
        ("minecraft:piglin_brute", [-15, 1, -5], None),
        ("minecraft:wither_skeleton", [-4, 1, -4], None),
        ("minecraft:wither_skeleton", [4, 1, -4], None),
        ("minecraft:wither_skeleton", [4, 1, 4], None),
        ("minecraft:blaze", [-13, 1, -13], None),
        ("minecraft:blaze", [13, 1, -11], None),
        ("minecraft:blaze", [13, 1, 11], None),
        ("minecraft:hoglin", [-11, 1, -11], None),
        ("minecraft:hoglin", [-11, 1, -13], None),
        ("minecraft:hoglin", [-15, 1, -11], None),
        ("minecraft:hoglin", [0, 1, 12], None),
        ("minecraft:piglin", [-11, 1, 15], None),
        ("minecraft:piglin", [-13, 1, 11], None),
        ("minecraft:piglin", [0, 1, -12], None),
        ("minecraft:piglin", [-10, 1, 0], None),
        ("minecraft:piglin", [10, 1, 0], None),
        ("minecraft:piglin", [0, 1, -17], None),
        ("minecraft:piglin", [0, 1, 17], None),
    ]
}

fn spawn_fob_garrison(
    host: &mut dyn ServerHost,
    state: &mut RiftState,
    cx: i32,
    fy: i32,
    cz: i32,
) {
    for (mob_kind, [dx, dy, dz], captain_title) in fob_garrison_specs() {
        let mut nbt = Compound::new();
        nbt.insert("PersistenceRequired".into(), Tag::Byte(1));
        if is_piglin_or_hoglin(mob_kind) {
            nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
        }
        let pos = [
            f64::from(cx + dx) + 0.5,
            f64::from(fy + dy),
            f64::from(cz + dz) + 0.5,
        ];
        if let Some(eid) = host.summon(NETHER, mob_kind, pos, nbt) {
            state.known_nether_mobs.insert(eid);
            state.citadel_guards.insert(eid);
            if let Some(ent) = host.entity(eid) {
                let _ = ent.add_tag(host, "rift_bleed:nether_ally");
                let _ = ent.add_tag(host, "rift_bleed:citadel_guard");
            }
            if let Some(title) = captain_title
                && let Some(ent) = host.entity(eid)
            {
                let _ = ent.set_custom_name(host, Some(Text::literal((*title).to_owned())));
                if let Some(living) = host.living(ent) {
                    let _ = living.set_attribute_base(host, "minecraft:max_health", 85.0);
                    let _ = living.set_health(host, 85.0);
                    for (slot, id) in [
                        (EquipmentSlot::Head, "minecraft:netherite_helmet"),
                        (EquipmentSlot::Chest, "minecraft:netherite_chestplate"),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::build_ring_columns;
    use std::collections::HashMap;

    #[test]
    fn nether_fob_blueprint_has_zero_floaters_and_clear_spawns() {
        let half = NETHER_FOB_HALF_EXTENT;
        let mut grid: HashMap<[i32; 3], FMat> = HashMap::new();
        for dx in -half..=half {
            for dz in -half..=half {
                let (floor_mat, upper) = plan_nether_fob_col(dx, dz, NetherFlavor::CrimsonForest, 0);
                grid.insert([dx, 0, dz], floor_mat);
                for (dy, mat) in upper {
                    assert_ne!(mat, FMat::Air);
                    grid.insert([dx, dy, dz], mat);
                }
            }
        }

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
            assert!(visited.contains(pos), "FOB has floating {mat:?} at {pos:?}");
        }

        for (&[x, y, z], &mat) in &grid {
            if matches!(mat, FMat::SoulCampfire | FMat::SoulFire | FMat::SoulLantern) {
                assert!(
                    grid.contains_key(&[x, y - 1, z]),
                    "FOB unsupported {mat:?} at [{x}, {y}, {z}]"
                );
            }
        }

        // Portal area inside cheb <= 4 must remain clear above floor dy=0.
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in 1..=7 {
                    assert!(
                        !grid.contains_key(&[dx, dy, dz]),
                        "FOB center portal pad blocked at [{dx}, {dy}, {dz}]"
                    );
                }
            }
        }

        for ([dx, dy, dz], _, _, _) in fob_chest_specs() {
            assert!(
                grid.contains_key(&[*dx, dy - 1, *dz]),
                "FOB chest [{dx}, {dy}, {dz}] lacks floor"
            );
            assert!(
                !grid.contains_key(&[*dx, *dy, *dz]) && !grid.contains_key(&[*dx, dy + 1, *dz]),
                "FOB chest [{dx}, {dy}, {dz}] obstructed"
            );
        }

        for (mob, [dx, dy, dz], _) in fob_garrison_specs() {
            assert!(
                grid.contains_key(&[*dx, dy - 1, *dz]),
                "FOB mob {mob} at [{dx}, {dy}, {dz}] lacks floor"
            );
            assert!(
                !grid.contains_key(&[*dx, *dy, *dz]) && !grid.contains_key(&[*dx, dy + 1, *dz]),
                "FOB mob {mob} at [{dx}, {dy}, {dz}] obstructed"
            );
        }
    }

    #[test]
    fn nether_fob_unlocks_at_citadel_threshold_even_after_pacification() {
        let mut st = RiftState::new();
        let rift = RiftSource {
            id: 7,
            center: [800, 64, -800],
            active_portal: true,
            contained: false,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 50_000,
            corrupted_blocks: 700_000,
            max_radius_sq: 150 * 150,
            ring_radius: 150,
            ring_columns: build_ring_columns(800, -800, 150),
            active_column: None,
            deepen_radius: 45,
            deepen_columns: Vec::new(),
            max_deepen_reached: 45,
            bastions_placed: 2,
            outposts_placed: 4,
            citadel_spawned: true,
            boss_uuid: None,
            pacified: true,
            regressing: true,
            regress_radius: 120,
            regress_columns: Vec::new(),
            regress_active_column: None,
        };
        let w = ConnectedWasteland {
            total_corrupted_blocks: 700_000,
            total_area_radius_sq: 150 * 150,
            max_deepen_radius: 45,
            max_ring_radius: 150,
            total_bastions_placed: 2,
            total_outposts_placed: 4,
            member_count: 1,
        };
        assert!(rift_eligible_for_nether_fob(&st, &rift, w));
        st.nether_fob_rifts.insert(7);
        assert!(!rift_eligible_for_nether_fob(&st, &rift, w));
    }
}
