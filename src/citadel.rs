use crate::bastion::{all_players_far_from, is_fillable_terrain_void, overworld_player_positions};
use crate::mobs::{is_piglin_or_hoglin, paint_biome_column};
use crate::state::{ConnectedWasteland, PendingCitadel, RiftState};
use crate::types::{
    NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, RiftSource, horiz_dist_sq,
    spatial_hash, strip_ns,
};
use mod_api::{
    BlockEdit, BlockRun, BlockState, BlockUpdateFlags, Compound, Heightmap, LevelRef, ServerHost,
    SoundSource, Tag, Text,
};
use std::collections::HashSet;

const CITADEL_CHECK_INTERVAL_TICKS: u64 = 200;
/// Citadel only emerges once corruption is truly massive ("a lot, a lot") AND
/// at least 2 Bastions have spawned on the outskirts.
pub const MIN_BLOCKS_FOR_CITADEL: u64 = 650_000;
pub const MIN_RING_RADIUS_FOR_CITADEL: i32 = 130;
pub const MIN_DEEPEN_RADIUS_FOR_CITADEL: i32 = 40;
pub const MIN_BASTIONS_FOR_CITADEL: u32 = 2;
/// Players within 42 blocks are right inside the 57x57 footprint; wait until they
/// step outside 42 blocks so walls never entomb anyone alive.
const MIN_PLAYER_DIST_FOR_CITADEL: i64 = 42;
const CITADEL_FOOTPRINT_HALF: i32 = 27;

pub fn wasteland_eligible_for_citadel(rift: &RiftSource, wasteland: ConnectedWasteland) -> bool {
    if rift.contained || rift.pacified || rift.regressing || rift.citadel_spawned {
        return false;
    }
    let enough_bastions = rift.bastions_placed >= MIN_BASTIONS_FOR_CITADEL
        || wasteland.total_bastions_placed >= MIN_BASTIONS_FOR_CITADEL;
    enough_bastions
        && wasteland.total_corrupted_blocks >= MIN_BLOCKS_FOR_CITADEL
        && wasteland.max_ring_radius >= MIN_RING_RADIUS_FOR_CITADEL
        && wasteland.max_deepen_radius >= MIN_DEEPEN_RADIUS_FOR_CITADEL
}

pub fn release_citadel_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if let Some(pending) = state.pending_citadel.take() {
        for c in pending.forced_chunks {
            if let Some(chunk_ref) = level.chunk_ref(host, c) {
                let _ = chunk_ref.set_forced(host, false);
            }
        }
    }
    for c in state.citadel_release_chunks.drain(..) {
        if let Some(chunk_ref) = level.chunk_ref(host, c) {
            let _ = chunk_ref.set_forced(host, false);
        }
    }
    state.citadel_release_ttl = 0;
}

pub fn tick_citadel_emergence(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if state.citadel_release_ttl > 0 {
        state.citadel_release_ttl -= 1;
        if state.citadel_release_ttl == 0 {
            for c in state.citadel_release_chunks.drain(..) {
                if let Some(chunk_ref) = level.chunk_ref(host, c) {
                    let _ = chunk_ref.set_forced(host, false);
                }
            }
        }
    }

    if state.pending_citadel.is_some() {
        process_pending_citadel(host, level, state);
        return;
    }

    if state.paused || state.tick % CITADEL_CHECK_INTERVAL_TICKS != 45 {
        return;
    }

    let player_positions = overworld_player_positions(host);

    for idx in 0..state.rifts.len() {
        let wasteland = state.connected_wasteland_for(idx);
        let rift = &state.rifts[idx];
        if !wasteland_eligible_for_citadel(rift, wasteland) {
            continue;
        }
        if !all_players_far_from(&player_positions, rift.center, MIN_PLAYER_DIST_FOR_CITADEL) {
            continue;
        }
        let roll = spatial_hash(
            rift.center[0],
            rift.center[1],
            rift.center[2],
            state.seed ^ state.tick ^ 0xC17A_DE11,
        );
        stage_pending_citadel(host, level, state, rift.id, rift.center, rift.flavor, roll);
        break;
    }
}

fn stage_pending_citadel(
    host: &mut dyn ServerHost,
    level: LevelRef,
    state: &mut RiftState,
    rift_id: u64,
    center: [i32; 3],
    flavor: NetherFlavor,
    roll: u64,
) {
    let min_cx = (center[0] - 36) >> 4;
    let max_cx = (center[0] + 36) >> 4;
    let min_cz = (center[2] - 36) >> 4;
    let max_cz = (center[2] + 36) >> 4;

    let mut forced_chunks = Vec::new();
    for ccx in min_cx..=max_cx {
        for ccz in min_cz..=max_cz {
            let chunk = [ccx, ccz];
            if let Some(chunk_ref) = level.chunk_ref(host, chunk)
                && chunk_ref.set_forced(host, true)
            {
                forced_chunks.push(chunk);
            }
        }
    }

    state.pending_citadel = Some(PendingCitadel {
        rift_id,
        center,
        flavor,
        roll,
        forced_chunks,
        queued_tick: state.tick,
    });
}

fn process_pending_citadel(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    let Some(pending) = state.pending_citadel.clone() else {
        return;
    };

    let Some(rift_idx) = state.rifts.iter().position(|r| r.id == pending.rift_id) else {
        release_citadel_chunks(host, level, state);
        return;
    };
    if state.rifts[rift_idx].contained
        || state.rifts[rift_idx].pacified
        || state.rifts[rift_idx].citadel_spawned
    {
        release_citadel_chunks(host, level, state);
        return;
    }

    let players = overworld_player_positions(host);
    if !all_players_far_from(&players, pending.center, MIN_PLAYER_DIST_FOR_CITADEL) {
        release_citadel_chunks(host, level, state);
        return;
    }

    if state.tick.saturating_sub(pending.queued_tick) > 160 {
        release_citadel_chunks(host, level, state);
        return;
    }

    let all_loaded = pending
        .forced_chunks
        .iter()
        .all(|[cx, cz]| level.is_loaded(host, [cx << 4, 64, cz << 4]));
    if !all_loaded {
        return;
    }

    state.pending_citadel = None;

    let (floor_y, boss_uuid) = build_citadel_and_awaken_boss(
        host,
        level,
        state,
        pending.rift_id,
        pending.center,
        pending.flavor,
        pending.roll,
    );

    if let Some(rift) = state.rifts.iter_mut().find(|r| r.id == pending.rift_id) {
        rift.citadel_spawned = true;
        rift.boss_uuid = boss_uuid;
    }
    state
        .citadels
        .push([pending.center[0], floor_y, pending.center[2]]);
    state.citadel_release_chunks = pending.forced_chunks;
    state.citadel_release_ttl = 6;
    state.dirty = true;

    let msg = format!(
        "[Rift Bleed] An Obsidian Breach Citadel has erupted around Rift #{} at ({}, {}, {}) — the Wither Sovereign awaits!",
        pending.rift_id, pending.center[0], floor_y, pending.center[2]
    );
    for player in host.players().server_players() {
        if player
            .dimension(host)
            .is_some_and(|d| d.to_string() == OVERWORLD)
        {
            let _ = player.tell(host, Text::literal(msg.clone()));
            if let Some(ppos) = player.entity().position(host)
                && horiz_dist_sq([ppos[0] as i32, 64, ppos[2] as i32], pending.center) <= 360 * 360
            {
                let _ = level.play_sound(
                    host,
                    "minecraft:entity.wither.spawn",
                    ppos,
                    SoundSource::Hostile,
                    0.55,
                    0.75,
                );
            }
        }
    }
}

/// Octagonal footprint test covering the 55x55 Citadel perimeter with chamfered corners.
pub fn in_citadel_octagon(dx: i32, dz: i32) -> bool {
    let ax = dx.abs();
    let az = dz.abs();
    ax <= CITADEL_FOOTPRINT_HALF && az <= CITADEL_FOOTPRINT_HALF && ax + az <= 38
}

/// Palette indices used by `plan_citadel_column`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum Mat {
    Air = 0,
    Blackstone = 1,
    PolishedBricks = 2,
    CrackedBricks = 3,
    GildedBlackstone = 4,
    ChiseledBlackstone = 5,
    Obsidian = 6,
    CryingObsidian = 7,
    Basalt = 8,
    NetherBricks = 9,
    RedNetherBricks = 10,
    Magma = 11,
    Shroomlight = 12,
    SoulSoil = 13,
    SoulFire = 14,
    GoldBlock = 15,
    AncientDebris = 16,
    WartAccent = 17,
}

fn mat_for_biome_trim(flavor: NetherFlavor) -> &'static str {
    match flavor {
        NetherFlavor::WarpedForest => "minecraft:warped_wart_block",
        NetherFlavor::SoulSandValley => "minecraft:bone_block",
        NetherFlavor::BasaltDeltas => "minecraft:polished_basalt",
        NetherFlavor::CrimsonForest | NetherFlavor::NetherWastes => "minecraft:nether_wart_block",
    }
}

/// Returns `(floor_block, upper_placements: Vec<(dy_above_floor, Mat)>)` for a single column `(dx, dz)`
/// relative to `[cx, floor_y, cz]`. Shared between runtime builder and unit tests so geometry is
/// deterministic, non-floating, and never seals or fills the portal sanctuary center.
fn plan_citadel_column(dx: i32, dz: i32, roll: u64) -> (Mat, Vec<(i32, Mat)>) {
    let ax = dx.abs();
    let az = dz.abs();
    let cheb = ax.max(az);
    let r2 = dx * dx + dz * dz;
    let abs_sum = ax + az;

    let mut upper: Vec<(i32, Mat)> = Vec::new();

    // 1. Central Portal Sanctuary Dais (cheb <= 5): open-sky platform keeping portal clear.
    if cheb <= 5 {
        let floor = if (ax == 4 && az == 4) || (ax == 0 && az == 4) || (az == 0 && ax == 4) {
            Mat::CryingObsidian
        } else if cheb <= 2 || ax == az {
            Mat::GildedBlackstone
        } else {
            Mat::PolishedBricks
        };
        // Four corner Soul Brazier Pylons at (+-4, +-4), leaving center 3x3 open around portal.
        if ax == 4 && az == 4 {
            upper.push((1, Mat::GoldBlock));
            upper.push((2, Mat::Basalt));
            upper.push((3, Mat::CryingObsidian));
            upper.push((4, Mat::SoulSoil));
            upper.push((5, Mat::SoulFire));
        }
        return (floor, upper);
    }

    // 2. Inner Courtyard & Magma Star Ring (6 <= cheb <= 15).
    let floor = if r2 >= 132 && r2 <= 169 && (dx * dz).abs() > 16 {
        Mat::Magma
    } else if ax <= 1 || az <= 1 || (ax - az).abs() <= 1 {
        if roll % 5 == 0 {
            Mat::GildedBlackstone
        } else {
            Mat::PolishedBricks
        }
    } else if cheb == 9 {
        Mat::CryingObsidian
    } else if cheb == 14 {
        Mat::RedNetherBricks
    } else if roll % 7 == 0 {
        Mat::CrackedBricks
    } else {
        Mat::PolishedBricks
    };

    // Cardinal War Gate arches cut cleanly at |dx| <= 3 or |dz| <= 3.
    let in_cardinal_gate = (ax <= 3 && az >= 20) || (az <= 3 && ax >= 20);
    let in_gate_frame = ((ax == 4 && az >= 20 && az <= 27) || (az == 4 && ax >= 20 && ax <= 27))
        && cheb >= 21;

    // 3. Four Towering Diagonal Horn-Spires centered at (+-17, +-17).
    let sx = (ax - 17).abs();
    let sz = (az - 17).abs();
    let spire_cheb = sx.max(sz);
    let spire_sum = sx + sz;
    let in_spire_footprint = spire_cheb <= 4 && spire_sum <= 6;

    if in_spire_footprint {
        let on_spire_wall = spire_cheb >= 3 && spire_sum >= 5;
        let spire_doorway = (sx == 0 && (az == 13 || az == 21)) || (sz == 0 && (ax == 13 || ax == 21));
        if spire_doorway {
            // Arched doorway into each corner spire at base & battlement walkway levels.
            upper.push((4, Mat::RedNetherBricks));
            upper.push((5, Mat::NetherBricks));
            for dy in 6..=19 {
                upper.push((dy, if dy % 6 == 0 { Mat::RedNetherBricks } else { Mat::PolishedBricks }));
            }
        } else if on_spire_wall {
            let spire_h = if sx >= 3 && sz >= 3 { 24 } else { 21 };
            for dy in 1..=spire_h {
                let block = if dy == spire_h {
                    if sx == 3 && sz == 3 {
                        Mat::CryingObsidian
                    } else {
                        Mat::Obsidian
                    }
                } else if dy == 10 || dy == 17 {
                    Mat::RedNetherBricks
                } else if dy % 5 == 0 {
                    Mat::Obsidian
                } else if roll % 4 == 0 {
                    Mat::CrackedBricks
                } else {
                    Mat::PolishedBricks
                };
                upper.push((dy, block));
            }
            // Inward-sweeping horn tip at outer corner of each spire.
            if sx == 3 && sz == 3 {
                upper.push((spire_h + 1, Mat::Obsidian));
                upper.push((spire_h + 2, Mat::CryingObsidian));
                upper.push((spire_h + 3, Mat::Obsidian));
            }
        } else if spire_cheb == 0 {
            // Central glowing spire core column with lookouts + Ancient Debris heart & beacon.
            for dy in 1..=18 {
                if dy == 8 || dy == 16 {
                    upper.push((dy, Mat::PolishedBricks));
                }
            }
            upper.push((19, Mat::AncientDebris));
            upper.push((20, Mat::CryingObsidian));
            upper.push((21, Mat::Shroomlight));
            upper.push((22, Mat::SoulSoil));
            upper.push((23, Mat::SoulFire));
        } else {
            // Spire interior intermediate floor deck at dy=8 and dy=16 (with shaft opening for ladders/stairs).
            if sx + sz >= 2 {
                upper.push((8, Mat::PolishedBricks));
                upper.push((16, Mat::NetherBricks));
            }
            // Beacon roof deck + biome accent parapet at dy=20.
            if sx + sz == 2 {
                upper.push((20, Mat::WartAccent));
            }
        }
    } else if in_cardinal_gate {
        // Pointed Gothic archway overhead (dy=8..=12), open walkable passage dy=1..=7!
        let arch_bot = if ax == 0 || az == 0 {
            9
        } else if ax <= 1 || az <= 1 {
            8
        } else if ax <= 2 || az <= 2 {
            7
        } else {
            5
        };
        if cheb >= 22 && cheb <= 26 {
            for dy in arch_bot..=12 {
                let block = if dy == arch_bot {
                    Mat::RedNetherBricks
                } else if dy == 12 && (ax == 0 || az == 0) {
                    Mat::Shroomlight
                } else if dy == 12 {
                    Mat::ChiseledBlackstone
                } else {
                    Mat::NetherBricks
                };
                upper.push((dy, block));
            }
        }
    } else if in_gate_frame {
        // Twin flanking Obsidian + Nether Brick gate pylons rising beside each gate.
        let top = if cheb == 23 || cheb == 25 { 15 } else { 13 };
        for dy in 1..=top {
            let block = if dy == top {
                Mat::SoulSoil
            } else if dy % 4 == 0 {
                Mat::RedNetherBricks
            } else if cheb == 24 {
                Mat::Obsidian
            } else {
                Mat::PolishedBricks
            };
            upper.push((dy, block));
        }
        if top >= 14 {
            upper.push((top + 1, Mat::SoulFire));
        }
    } else if cheb >= 22 && in_citadel_octagon(dx, dz) {
        // 4. Outer Octagonal Battlements & Walkway (cheb 22..=27).
        let outer_face = cheb == 26 || abs_sum >= 37 || cheb == 27;
        let inner_face = cheb == 22 && (dx + dz).abs() % 3 != 0;
        let walkway_h = 7;

        if outer_face {
            let rib = (dx - dz).abs() % 4 == 0;
            let parapet_top = if rib { 11 } else if (dx + dz) % 2 == 0 { 10 } else { 9 };
            for dy in 1..=parapet_top {
                let block = if dy == 4 || dy == 8 {
                    Mat::RedNetherBricks
                } else if dy >= 9 && rib {
                    Mat::Obsidian
                } else if rib {
                    Mat::Basalt
                } else if roll % 4 == 0 {
                    Mat::CrackedBricks
                } else {
                    Mat::PolishedBricks
                };
                upper.push((dy, block));
            }
        } else {
            // Walkway deck at dy=7 with lit embrasures & hollow lower gallery or solid buttress base.
            if cheb >= 24 {
                for dy in 1..=walkway_h {
                    upper.push((
                        dy,
                        if dy == walkway_h {
                            Mat::NetherBricks
                        } else {
                            Mat::Blackstone
                        },
                    ));
                }
            } else {
                if inner_face && (ax % 4 == 0 || az % 4 == 0) {
                    for dy in 1..=walkway_h {
                        upper.push((
                            dy,
                            if dy == 4 {
                                Mat::Shroomlight
                            } else {
                                Mat::PolishedBricks
                            },
                        ));
                    }
                } else {
                    upper.push((walkway_h, Mat::PolishedBricks));
                }
                if (ax + az) % 4 == 0 {
                    upper.push((walkway_h + 2, Mat::WartAccent));
                    upper.push((walkway_h + 1, Mat::ChiseledBlackstone));
                }
            }
        }
    }

    // 5. Four Sweeping Diagonal Overhead Arches + High Open-Sky Obsidian Crown (y = +20..+32).
    if ax >= 8 && az >= 8 && (ax - az).abs() <= 1 && cheb <= 16 {
        let diag = (ax + az) / 2;
        let arch_y = 29 - ((diag - 9).pow(2) / 5);
        for dy in arch_y..=(arch_y + 2) {
            upper.push((
                dy,
                if dy == arch_y && diag % 3 == 0 {
                    Mat::Shroomlight
                } else if dy == arch_y + 2 {
                    Mat::CryingObsidian
                } else {
                    Mat::Obsidian
                },
            ));
        }
    }
    if r2 >= 90 && r2 <= 132 {
        let on_ring = r2 >= 100 && r2 <= 125;
        if on_ring {
            upper.push((28, Mat::RedNetherBricks));
            upper.push((29, Mat::Obsidian));
            if (ax + az) % 3 == 0 {
                upper.push((
                    30,
                    if ax == 0 || az == 0 {
                        Mat::Shroomlight
                    } else {
                        Mat::CryingObsidian
                    },
                ));
            }
            if (ax == 0 || az == 0) && r2 >= 100 {
                upper.push((31, Mat::Obsidian));
            }
        }
    }

    (floor, upper)
}

/// Checks whether `pos` is a protected Citadel portal frame block (or directly
/// supports one) so the Wither's explosions never destroy the Nether portal during battle.
pub fn protect_citadel_portals_from_explosion(
    host: &dyn ServerHost,
    level: LevelRef,
    state: &RiftState,
    blocks: &mut Vec<[i32; 3]>,
) {
    if state.citadels.is_empty() && !state.rifts.iter().any(|r| r.citadel_spawned && !r.pacified) {
        return;
    }
    blocks.retain(|b| {
        let near_active_citadel_core = state
            .rifts
            .iter()
            .any(|r| r.citadel_spawned && !r.pacified && horiz_dist_sq(r.center, *b) <= 6 * 6);
        if !near_active_citadel_core {
            return true;
        }
        if let Some(k) = level.block(host, *b) {
            let s = k.id(host);
            if matches!(
                strip_ns(&s),
                "nether_portal" | "obsidian" | "crying_obsidian"
            ) {
                return false;
            }
        }
        if b[1] + 1 <= OVERWORLD_MAX_Y
            && let Some(above) = level.block(host, [b[0], b[1] + 1, b[2]])
        {
            let a = above.id(host);
            if matches!(strip_ns(&a), "nether_portal" | "obsidian") {
                return false;
            }
        }
        true
    });
}

fn build_citadel_and_awaken_boss(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    state: &mut RiftState,
    rift_id: u64,
    center: [i32; 3],
    flavor: NetherFlavor,
    roll: u64,
) -> (i32, Option<u128>) {
    // 1. Protect every existing portal/obsidian block around center and find portal base elevation.
    let mut protected_portal: HashSet<[i32; 3]> = HashSet::new();
    let mut min_portal_y = i32::MAX;
    for dx in -6..=6 {
        for dz in -6..=6 {
            for dy in -4..=10 {
                let p = [
                    center[0] + dx,
                    (center[1] + dy).clamp(OVERWORLD_MIN_Y + 2, OVERWORLD_MAX_Y - 2),
                    center[2] + dz,
                ];
                if let Some(k) = ow.block(host, p) {
                    let s = k.id(host);
                    if matches!(strip_ns(&s), "nether_portal" | "obsidian" | "crying_obsidian") {
                        protected_portal.insert(p);
                        if strip_ns(&s) == "obsidian" {
                            min_portal_y = min_portal_y.min(p[1]);
                        }
                    }
                }
            }
        }
    }
    let floor_y = if min_portal_y < i32::MAX {
        min_portal_y
    } else {
        (center[1] - 1).clamp(OVERWORLD_MIN_Y + 8, OVERWORLD_MAX_Y - 40)
    }
    .clamp(OVERWORLD_MIN_Y + 8, OVERWORLD_MAX_Y - 38);

    // Protect walking clearance immediately above/on front & back of every portal tile.
    let mut portal_air_clear: HashSet<[i32; 3]> = HashSet::new();
    for p in &protected_portal {
        if ow
            .block(host, *p)
            .is_some_and(|k| strip_ns(&k.id(host)) == "nether_portal")
        {
            for ddx in -1..=1 {
                for ddz in -1..=1 {
                    let ap = [p[0] + ddx, p[1], p[2] + ddz];
                    if !protected_portal.contains(&ap) {
                        portal_air_clear.insert(ap);
                    }
                }
            }
        }
    }

    let Some(blackstone) = host.block_state_id("minecraft:blackstone") else {
        return (floor_y, None);
    };
    let resolve = |id: &str, fallback: BlockState| host.block_state_id(id).unwrap_or(fallback);
    let palette = vec![
        resolve("minecraft:air", blackstone),                         // 0 Air
        blackstone,                                                   // 1 Blackstone
        resolve("minecraft:polished_blackstone_bricks", blackstone),  // 2 PolishedBricks
        resolve("minecraft:cracked_polished_blackstone_bricks", blackstone), // 3 CrackedBricks
        resolve("minecraft:gilded_blackstone", blackstone),           // 4 GildedBlackstone
        resolve("minecraft:chiseled_polished_blackstone", blackstone),// 5 ChiseledBlackstone
        resolve("minecraft:obsidian", blackstone),                    // 6 Obsidian
        resolve("minecraft:crying_obsidian", blackstone),             // 7 CryingObsidian
        resolve("minecraft:polished_basalt", blackstone),             // 8 Basalt
        resolve("minecraft:nether_bricks", blackstone),               // 9 NetherBricks
        resolve("minecraft:red_nether_bricks", blackstone),           // 10 RedNetherBricks
        resolve("minecraft:magma_block", blackstone),                 // 11 Magma
        resolve("minecraft:shroomlight", blackstone),                 // 12 Shroomlight
        resolve("minecraft:soul_soil", blackstone),                   // 13 SoulSoil
        resolve("minecraft:soul_fire", blackstone),                   // 14 SoulFire
        resolve("minecraft:gold_block", blackstone),                  // 15 GoldBlock
        resolve("minecraft:ancient_debris", blackstone),              // 16 AncientDebris
        resolve(mat_for_biome_trim(flavor), blackstone),              // 17 WartAccent
    ];

    let mut runs: Vec<BlockRun> = Vec::with_capacity(4096);
    let max_clear_r = 31i32;

    for dx in -max_clear_r..=max_clear_r {
        for dz in -max_clear_r..=max_clear_r {
            let x = center[0] + dx;
            let z = center[2] + dz;
            if !ow.is_loaded(host, [x, 64, z]) {
                continue;
            }
            let ax = dx.abs();
            let az = dz.abs();
            let cheb = ax.max(az);
            let col_roll = spatial_hash(x, floor_y, z, roll ^ rift_id);

            let top_existing = host
                .surface_heights(OVERWORLD, &[(x, z)], Heightmap::WorldSurface)
                .ok()
                .and_then(|hs| hs.first().copied().flatten())
                .unwrap_or(floor_y + 12)
                .clamp(floor_y, OVERWORLD_MAX_Y - 2);

            if !in_citadel_octagon(dx, dz) {
                // External 4 cardinal approach staircases bridging war gates cleanly to natural floor.
                let on_approach_axis = (ax <= 3 && az >= 27 && az <= 31) || (az <= 3 && ax >= 27 && ax <= 31);
                if on_approach_axis {
                    let out_step = cheb - 27; // 0..=4 outward
                    let step_y = (floor_y - (out_step / 2)).clamp(OVERWORLD_MIN_Y + 4, floor_y);
                    // Connect step underside solidly down to terrain floor so ramps never float!
                    let mut solid_y = step_y - 1;
                    for y in ((step_y - 40).max(OVERWORLD_MIN_Y + 2)..step_y).rev() {
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
                            block: Mat::Blackstone as u32,
                            only: None,
                        });
                    } else {
                        runs.push(BlockRun {
                            x,
                            z,
                            y: step_y,
                            height: 1,
                            surface: None,
                            block: Mat::PolishedBricks as u32,
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
                            block: Mat::Air as u32,
                            only: None,
                        });
                    }
                }
                continue;
            }

            // Solid Grounding Pier downward until hitting solid floor (never float!).
            let mut down_y = floor_y;
            for y in ((floor_y - 56).max(OVERWORLD_MIN_Y + 2)..floor_y).rev() {
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
                    block: if cheb >= 22 {
                        Mat::Blackstone as u32
                    } else if col_roll % 5 == 0 {
                        Mat::Basalt as u32
                    } else {
                        Mat::Blackstone as u32
                    },
                    only: None,
                });
            }

            let (floor_mat, upper_blocks) = plan_citadel_column(dx, dz, col_roll);

            // Clear upper volume above courtyard (up to max structure / existing mountain ceiling)
            // BEFORE stamping upper structural blocks cleanly on top.
            let clear_top = (floor_y + 33).max(top_existing + 2).min(OVERWORLD_MAX_Y - 1);
            // Preserve protected portal core coordinates while clearing mountain slopes / leaves.
            let touches_protected = (ax <= 6 && az <= 6)
                && ((floor_y..=clear_top).any(|y| protected_portal.contains(&[x, y, z])));

            if !touches_protected {
                runs.push(BlockRun {
                    x,
                    z,
                    y: floor_y,
                    height: 1,
                    surface: None,
                    block: floor_mat as u32,
                    only: None,
                });
                if clear_top > floor_y {
                    runs.push(BlockRun {
                        x,
                        z,
                        y: floor_y + 1,
                        height: (clear_top - floor_y) as u32,
                        surface: None,
                        block: Mat::Air as u32,
                        only: None,
                    });
                }
                for (dy, mat) in upper_blocks {
                    let target_y = floor_y + dy;
                    if target_y <= OVERWORLD_MAX_Y - 1 {
                        runs.push(BlockRun {
                            x,
                            z,
                            y: target_y,
                            height: 1,
                            surface: None,
                            block: mat as u32,
                            only: None,
                        });
                    }
                }
            } else {
                // Fine-grained column emit preserving every existing portal & obsidian block.
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
                    let mat = if portal_air_clear.contains(&[x, y, z]) {
                        Mat::Air
                    } else {
                        upper_blocks
                            .iter()
                            .find(|(dy, _)| floor_y + *dy == y)
                            .map(|(_, m)| *m)
                            .unwrap_or(Mat::Air)
                    };
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
    let _ = host.edit_blocks(OVERWORLD, &edit);

    // Add exposed Ancient Debris + Vault Loot Chests inside the two primary Spire vaults.
    place_citadel_vault_rewards(host, ow, center, floor_y);

    // Ensure full biome tint across the Citadel footprint.
    for x in ((center[0] - 32)..=(center[0] + 32)).step_by(4) {
        for z in ((center[2] - 32)..=(center[2] + 32)).step_by(4) {
            if state.mark_quart_corrupted([x, floor_y, z]) {
                paint_biome_column(ow, host, x, z, flavor);
            }
        }
    }

    // Summon diverse fortress guards and the Wither Sovereign in the arena center.
    let boss_uuid = spawn_citadel_garrison_and_wither(host, ow, state, rift_id, center, floor_y);
    (floor_y, boss_uuid)
}

fn place_citadel_vault_rewards(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    center: [i32; 3],
    floor_y: i32,
) {
    let debris_state = host.block_state_id("minecraft:ancient_debris");
    let gold_state = host.block_state_id("minecraft:gold_block");
    let chest_state = host.block_state_id("minecraft:chest");

    // Exposed Ancient Debris & gold trophies inside NW and SE spire sanctuaries.
    for (sx, sz) in [(-17, -17), (17, 17)] {
        let p_gold = [center[0] + sx, floor_y + 1, center[2] + sz];
        let p_deb = [center[0] + sx, floor_y + 2, center[2] + sz];
        if ow.is_loaded(host, p_gold) {
            if let Some(g) = gold_state {
                let _ = ow.set_block(host, p_gold, g, BlockUpdateFlags::CLIENTS);
            }
            if let Some(d) = debris_state {
                let _ = ow.set_block(host, p_deb, d, BlockUpdateFlags::CLIENTS);
            }
        }
    }

    // Treasure chests in NE & SW corner bastion-spires with real Bastion treasure loot.
    if let Some(chest) = chest_state {
        for (idx, (cx_off, cz_off)) in [(17, -17), (-17, 17)].iter().enumerate() {
            let chest_pos = [center[0] + cx_off, floor_y + 1, center[2] + cz_off];
            if ow.is_loaded(host, chest_pos)
                && ow
                    .set_block(host, chest_pos, chest, BlockUpdateFlags::CLIENTS)
                    .unwrap_or(false)
                && let Some(be) = host.block_entity(ow, chest_pos)
            {
                let mut data = Compound::new();
                data.insert(
                    "LootTable".into(),
                    Tag::String("minecraft:chests/bastion_treasure".into()),
                );
                data.insert("LootTableSeed".into(), Tag::Long(0xB457_1000 + idx as i64));
                let _ = be.merge_data(host, data);
            }
        }
    }
}

fn spawn_citadel_garrison_and_wither(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    state: &mut RiftState,
    rift_id: u64,
    center: [i32; 3],
    floor_y: i32,
) -> Option<u128> {
    let guards: &[(&str, [i32; 3])] = &[
        ("minecraft:wither_skeleton", [7, 1, 0]),
        ("minecraft:wither_skeleton", [-7, 1, 0]),
        ("minecraft:wither_skeleton", [0, 1, 7]),
        ("minecraft:wither_skeleton", [0, 1, -7]),
        ("minecraft:piglin_brute", [10, 1, 10]),
        ("minecraft:piglin_brute", [-10, 1, -10]),
        ("minecraft:piglin_brute", [24, 8, 0]),
        ("minecraft:piglin", [0, 8, 24]),
        ("minecraft:piglin", [0, 8, -24]),
        ("minecraft:piglin", [-24, 8, 0]),
        ("minecraft:blaze", [17, 18, 17]),
        ("minecraft:blaze", [-17, 18, 17]),
        ("minecraft:blaze", [17, 18, -17]),
        ("minecraft:blaze", [-17, 18, -17]),
        ("minecraft:hoglin", [0, 1, 21]),
        ("minecraft:hoglin", [21, 1, 0]),
        ("minecraft:magma_cube", [-10, 1, 10]),
        ("minecraft:magma_cube", [10, 1, -10]),
    ];

    for (kind, [dx, dy, dz]) in guards {
        let mut nbt = Compound::new();
        nbt.insert("PersistenceRequired".into(), Tag::Byte(1));
        if is_piglin_or_hoglin(kind) {
            nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
            nbt.insert("TimeInOverworld".into(), Tag::Int(0));
        }
        let pos = [
            f64::from(center[0] + dx) + 0.5,
            f64::from(floor_y + dy),
            f64::from(center[2] + dz) + 0.5,
        ];
        if let Some(eid) = host.summon(OVERWORLD, kind, pos, nbt) {
            state.known_nether_mobs.insert(eid);
        }
    }

    // Spawn the Wither Sovereign hovering inside the central Citadel arena!
    let mut wither_nbt = Compound::new();
    wither_nbt.insert("PersistenceRequired".into(), Tag::Byte(1));
    wither_nbt.insert("Invul".into(), Tag::Int(140));
    let spawn_pos = [
        f64::from(center[0]) + 0.5,
        f64::from((floor_y + 10).min(OVERWORLD_MAX_Y - 12)),
        f64::from(center[2]) + 0.5,
    ];
    let boss_id = host.summon(OVERWORLD, "minecraft:wither", spawn_pos, wither_nbt)?;
    state.known_nether_mobs.insert(boss_id);

    let mut boss_uuid = None;
    if let Some(ent) = host.entity(boss_id) {
        let _ = ent.add_tag(host, "rift_bleed:sovereign");
        let _ = ent.add_tag(host, &format!("rift_bleed:rift_{rift_id}"));
        let _ = ent.set_custom_name(host, Some(Text::literal(format!("Rift #{rift_id} Sovereign"))));
        boss_uuid = ent.uuid(host);
    }
    let _ = ow;
    boss_uuid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::build_ring_columns;

    fn matured_rift() -> RiftSource {
        RiftSource {
            id: 1,
            center: [100, 70, 100],
            active_portal: true,
            contained: false,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 50_000,
            corrupted_blocks: 700_000,
            max_radius_sq: 140 * 140,
            ring_radius: 140,
            ring_columns: build_ring_columns(100, 100, 140),
            active_column: None,
            deepen_radius: 45,
            deepen_columns: Vec::new(),
            bastions_placed: 2,
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
    fn citadel_requires_two_bastions_and_massive_corruption() {
        let r = matured_rift();
        let w = ConnectedWasteland {
            total_corrupted_blocks: r.corrupted_blocks,
            total_area_radius_sq: r.max_radius_sq,
            max_deepen_radius: r.deepen_radius,
            max_ring_radius: r.ring_radius,
            total_bastions_placed: r.bastions_placed,
            member_count: 1,
        };
        assert!(wasteland_eligible_for_citadel(&r, w));

        let mut only_one_bastion = r.clone();
        only_one_bastion.bastions_placed = 1;
        let mut w_one = w;
        w_one.total_bastions_placed = 1;
        assert!(!wasteland_eligible_for_citadel(&only_one_bastion, w_one));

        let mut too_small = w;
        too_small.total_corrupted_blocks = 300_000;
        assert!(!wasteland_eligible_for_citadel(&r, too_small));

        let mut shallow = w;
        shallow.max_deepen_radius = 20;
        assert!(!wasteland_eligible_for_citadel(&r, shallow));

        let mut contained = r.clone();
        contained.contained = true;
        assert!(!wasteland_eligible_for_citadel(&contained, w));

        let mut pacified = r.clone();
        pacified.pacified = true;
        assert!(!wasteland_eligible_for_citadel(&pacified, w));

        let mut already = r;
        already.citadel_spawned = true;
        assert!(!wasteland_eligible_for_citadel(&already, w));
    }

    #[test]
    fn citadel_geometry_keeps_portal_open_and_gates_accessible() {
        // Center sanctuary around portal (<=3) has zero upper blocks that could seal or bury portal.
        for dx in -3..=3 {
            for dz in -3..=3 {
                let (_, upper) = plan_citadel_column(dx, dz, 123);
                assert!(
                    upper.is_empty(),
                    "portal center ({dx}, {dz}) must stay clear and open-sky"
                );
            }
        }

        // Cardinal war gates (e.g. dx=0, dz=24) must keep y+1..y+6 completely open for walking.
        let (_, gate_upper) = plan_citadel_column(0, 24, 99);
        for dy in 1..=6 {
            assert!(
                !gate_upper.iter().any(|(uy, _)| *uy == dy),
                "gate passage blocked at dy={dy}"
            );
        }
        assert!(
            gate_upper.iter().any(|(uy, _)| *uy >= 9),
            "gate must still have overhead arch"
        );

        // Corner horn-spires rise over 20 blocks high and have hollow interiors.
        let (_, spire_center) = plan_citadel_column(17, 17, 7);
        assert!(spire_center.iter().any(|(dy, mat)| *dy >= 20 && *mat == Mat::Shroomlight));
    }
}
