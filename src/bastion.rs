use crate::corruption::{is_nether_terrain_block, is_open_space, is_water_like};
use crate::mobs::paint_biome_column;
use crate::state::{PendingBastion, RiftState};
use crate::types::{
    BastionSite, NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, RiftSource, dist_sq,
    horiz_dist_sq, spatial_hash, strip_ns,
};
use mod_api::{
    BlockEdit, BlockRun, BlockState, BlockUpdateFlags, Compound, Heightmap, LevelRef, ServerHost,
    Text,
};

const BASTION_CHECK_INTERVAL_TICKS: u64 = 600;
const MIN_RIFT_BLOCKS_FOR_BASTION: u64 = 150_000;
const MIN_RADIUS_SQ_FOR_BASTION: i64 = 52 * 52;
const MIN_DEEPEN_RADIUS_FOR_BASTION: i32 = 22;
const SECOND_BASTION_BLOCKS: u64 = 500_000;
const SECOND_BASTION_RING_RADIUS: i32 = 95;
const MAX_BASTIONS_PER_RIFT: u32 = 2;
const MIN_BASTION_SPACING: i64 = 280;
const MIN_PLAYER_DIST_FOR_BASTION: i64 = 112;
const BASTION_RARITY_DENOM: u64 = 8;

/// Releases any active or post-placement Bastion forced chunks cleanly.
pub fn release_bastion_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if let Some(pending) = state.pending_bastion.take() {
        for c in pending.forced_chunks {
            if let Some(chunk_ref) = level.chunk_ref(host, c) {
                let _ = chunk_ref.set_forced(host, false);
            }
        }
    }
    for c in state.bastion_release_chunks.drain(..) {
        if let Some(chunk_ref) = level.chunk_ref(host, c) {
            let _ = chunk_ref.set_forced(host, false);
        }
    }
    state.bastion_release_ttl = 0;
}

pub fn tick_bastion_emergence(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    if state.bastion_release_ttl > 0 {
        state.bastion_release_ttl -= 1;
        if state.bastion_release_ttl == 0 {
            for c in state.bastion_release_chunks.drain(..) {
                if let Some(chunk_ref) = level.chunk_ref(host, c) {
                    let _ = chunk_ref.set_forced(host, false);
                }
            }
        }
    }

    if state.pending_bastion.is_some() {
        process_pending_bastion(host, level, state);
        return;
    }

    if state.paused || state.tick % BASTION_CHECK_INTERVAL_TICKS != 90 {
        return;
    }

    let player_positions = overworld_player_positions(host);

    for idx in 0..state.rifts.len() {
        let rift = &state.rifts[idx];
        if !rift_eligible_for_bastion(rift) {
            continue;
        }
        let roll = spatial_hash(
            rift.center[0],
            rift.bastions_placed as i32,
            rift.center[2],
            state.seed ^ state.tick,
        );
        if roll % BASTION_RARITY_DENOM != 0 {
            continue;
        }
        if let Some((origin, flavor, cand_roll)) =
            find_bastion_site(host, level, state, rift, &player_positions)
        {
            stage_pending_bastion(host, level, state, rift.id, origin, flavor, cand_roll);
            break;
        }
    }
}

pub fn rift_eligible_for_bastion(rift: &RiftSource) -> bool {
    if rift.contained || rift.bastions_placed >= MAX_BASTIONS_PER_RIFT {
        return false;
    }
    if rift.corrupted_blocks < MIN_RIFT_BLOCKS_FOR_BASTION
        || rift.max_radius_sq < MIN_RADIUS_SQ_FOR_BASTION
        || rift.deepen_radius < MIN_DEEPEN_RADIUS_FOR_BASTION
    {
        return false;
    }
    if rift.bastions_placed >= 1
        && (rift.corrupted_blocks < SECOND_BASTION_BLOCKS
            || rift.ring_radius < SECOND_BASTION_RING_RADIUS)
    {
        return false;
    }
    true
}

fn overworld_player_positions(host: &dyn ServerHost) -> Vec<[i32; 3]> {
    host.players()
        .server_players()
        .into_iter()
        .filter(|p| {
            p.dimension(host)
                .is_some_and(|d| d.to_string() == OVERWORLD)
        })
        .filter_map(|p| {
            let pos = p.entity().position(host)?;
            Some([
                pos[0].floor() as i32,
                pos[1].floor() as i32,
                pos[2].floor() as i32,
            ])
        })
        .collect()
}

pub fn all_players_far_from(players: &[[i32; 3]], pos: [i32; 3], min_dist: i64) -> bool {
    let min_sq = min_dist * min_dist;
    players.iter().all(|p| horiz_dist_sq(*p, pos) >= min_sq)
}

pub fn area_metrics_qualify(
    quart_corrupted_count: usize,
    nether_surface_count: usize,
    water_count: usize,
    total_samples: usize,
) -> bool {
    if total_samples == 0 || water_count > 0 {
        return false;
    }
    quart_corrupted_count * 25 >= total_samples * 23 && nether_surface_count * 25 >= total_samples * 20
}

fn find_bastion_site(
    host: &dyn ServerHost,
    ow: LevelRef,
    state: &RiftState,
    rift: &RiftSource,
    overworld_players: &[[i32; 3]],
) -> Option<([i32; 3], NetherFlavor, u64)> {
    let radius = (rift.max_radius_sq as f64).sqrt() as i32;
    let inner = 34;
    let outer = (radius - 14).max(inner);

    for attempt in 0..8u64 {
        let h = spatial_hash(
            rift.center[0],
            rift.bastions_placed as i32,
            rift.center[2],
            state.tick ^ attempt.wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let angle = ((h & 0xFFFF) as f64 / 65535.0) * std::f64::consts::TAU;
        let span = (outer - inner + 1).max(1) as u64;
        let dist = inner + (((h >> 16) % span) as i32);
        let cx = rift.center[0] + (angle.cos() * f64::from(dist)).round() as i32;
        let cz = rift.center[2] + (angle.sin() * f64::from(dist)).round() as i32;

        // Keep away from world spawn, rift cores, and other bastions.
        if horiz_dist_sq([cx, 64, cz], [0, 64, 0]) < 96 * 96 {
            continue;
        }
        if state
            .rifts
            .iter()
            .any(|r| horiz_dist_sq(r.center, [cx, 64, cz]) < 44 * 44)
        {
            continue;
        }
        if state
            .bastions
            .iter()
            .any(|b| dist_sq(b.center, [cx, 64, cz]) < MIN_BASTION_SPACING * MIN_BASTION_SPACING)
        {
            continue;
        }

        // Never build a Bastion where any Overworld player can watch it appear out of thin air.
        if !all_players_far_from(
            overworld_players,
            [cx + 8, 64, cz + 8],
            MIN_PLAYER_DIST_FOR_BASTION,
        ) {
            continue;
        }

        // Probe a 5x5 grid across the candidate footprint to verify gentle slope & heavy corruption.
        let mut probe_cols = Vec::with_capacity(25);
        for dx in [-8, 0, 8, 16, 24] {
            for dz in [-8, 0, 8, 16, 24] {
                probe_cols.push((cx + dx, cz + dz));
            }
        }

        let Ok(heights) =
            host.surface_heights(OVERWORLD, &probe_cols, Heightmap::MotionBlockingNoLeaves)
        else {
            continue;
        };
        if heights.iter().any(Option::is_none) {
            continue;
        }
        let hs: Vec<i32> = heights.into_iter().flatten().collect();
        let min_h = *hs.iter().min().unwrap_or(&0);
        let max_h = *hs.iter().max().unwrap_or(&0);
        let avg_h = hs.iter().sum::<i32>() / (hs.len() as i32);
        if max_h - min_h > 12 || avg_h < 52 || avg_h > 125 {
            continue;
        }

        let mut quart_corrupt = 0usize;
        let mut nether_surface = 0usize;
        let mut water_hits = 0usize;

        for (idx, (px, pz)) in probe_cols.iter().enumerate() {
            let sy = hs[idx] - 1;
            let p = [*px, sy, *pz];
            if state.is_quart_corrupted(p) {
                quart_corrupt += 1;
            }
            if let Some(key) = ow.block(host, p) {
                let name = key.id(host);
                if is_water_like(&name) {
                    water_hits += 1;
                } else if is_nether_terrain_block(&name) {
                    nether_surface += 1;
                }
            }
            if let Some(top_key) = ow.block(host, [*px, hs[idx], *pz])
                && is_water_like(&top_key.id(host))
            {
                water_hits += 1;
            }
        }

        if !area_metrics_qualify(quart_corrupt, nether_surface, water_hits, probe_cols.len()) {
            continue;
        }

        let flavor = NetherFlavor::pick_for_region(cx, cz, state.seed ^ rift.id);
        let base_y = min_h - 1;
        return Some(([cx, base_y, cz], flavor, h));
    }

    None
}

fn stage_pending_bastion(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    state: &mut RiftState,
    rift_id: u64,
    origin: [i32; 3],
    flavor: NetherFlavor,
    roll: u64,
) {
    let ccx = origin[0] >> 4;
    let ccz = origin[2] >> 4;
    let mut forced_chunks = Vec::with_capacity(81);
    for dx in -4..=4 {
        for dz in -4..=4 {
            let chunk = [ccx + dx, ccz + dz];
            if let Some(chunk_ref) = ow.chunk_ref(host, chunk)
                && chunk_ref.set_forced(host, true)
            {
                forced_chunks.push(chunk);
            }
        }
    }

    state.pending_bastion = Some(PendingBastion {
        rift_id,
        origin,
        flavor,
        roll,
        forced_chunks,
        queued_tick: state.tick,
    });
}

fn process_pending_bastion(host: &mut dyn ServerHost, ow: LevelRef, state: &mut RiftState) {
    let Some(pending) = state.pending_bastion.clone() else {
        return;
    };

    // Abort cleanly if an Overworld player approached before construction began.
    let players = overworld_player_positions(host);
    if !all_players_far_from(
        &players,
        [pending.origin[0] + 8, pending.origin[1], pending.origin[2] + 8],
        MIN_PLAYER_DIST_FOR_BASTION,
    ) {
        release_bastion_chunks(host, ow, state);
        return;
    }

    // Timeout safety if chunks cannot load after 8 seconds.
    if state.tick.saturating_sub(pending.queued_tick) > 160 {
        release_bastion_chunks(host, ow, state);
        return;
    }

    let all_loaded = pending
        .forced_chunks
        .iter()
        .all(|[cx, cz]| ow.is_loaded(host, [cx << 4, 64, cz << 4]));
    if !all_loaded {
        return;
    }

    state.pending_bastion = None;

    let cmd = format!(
        "execute in minecraft:overworld run place jigsaw minecraft:bastion/starts minecraft:empty 4 {} {} {}",
        pending.origin[0], pending.origin[1], pending.origin[2]
    );
    let placed_jigsaw = host.commands().run_command_with_output(&cmd).success;

    if placed_jigsaw {
        scrub_and_ground_bastion(host, ow, pending.origin, pending.flavor);
    } else {
        build_fallback_bastion_keep(host, ow, pending.origin, pending.flavor, pending.roll);
    }

    mark_bastion_footprint_biomes(host, ow, state, pending.origin, pending.flavor);

    if let Some(rift) = state.rifts.iter_mut().find(|r| r.id == pending.rift_id) {
        rift.bastions_placed += 1;
    }
    state.bastions.push(BastionSite {
        center: pending.origin,
        flavor: pending.flavor,
    });
    state.bastion_release_chunks = pending.forced_chunks;
    state.bastion_release_ttl = 6;
    state.dirty = true;

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
        if horiz_dist_sq([pos[0] as i32, 64, pos[2] as i32], pending.origin) <= 340 * 340 {
            let _ = player.tell(
                host,
                Text::literal(
                    "[Rift Bleed] A heavy blackstone rumble echoes somewhere deep within the corrupted lands...",
                ),
            );
        }
    }
}

pub fn is_technical_block(short: &str) -> bool {
    matches!(
        short,
        "jigsaw"
            | "structure_block"
            | "structure_void"
            | "command_block"
            | "repeating_command_block"
            | "chain_command_block"
            | "barrier"
    )
}

fn is_bastion_masonry(short: &str) -> bool {
    matches!(
        short,
        "blackstone"
            | "polished_blackstone"
            | "polished_blackstone_bricks"
            | "cracked_polished_blackstone_bricks"
            | "chiseled_polished_blackstone"
            | "gilded_blackstone"
            | "polished_basalt"
            | "basalt"
            | "smooth_basalt"
            | "nether_bricks"
            | "cracked_nether_bricks"
            | "gold_block"
    )
}

fn is_fillable_terrain_void(short: &str) -> bool {
    is_open_space(short)
        || is_water_like(short)
        || matches!(short, "lava" | "flowing_lava" | "fire" | "soul_fire")
}

/// Scrubs any leftover jigsaw/structure/command/barrier blocks around `origin`
/// and extrudes weathered blackstone foundations down to solid ground so no part floats.
fn scrub_and_ground_bastion(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    origin: [i32; 3],
    flavor: NetherFlavor,
) {
    let Some(air) = host.block_state_id("minecraft:air") else {
        return;
    };
    let Some(blackstone) = host.block_state_id("minecraft:blackstone") else {
        return;
    };
    let cracked = host
        .block_state_id("minecraft:cracked_polished_blackstone_bricks")
        .unwrap_or(blackstone);
    let basalt = host
        .block_state_id("minecraft:basalt")
        .unwrap_or(blackstone);

    let radius = 34i32;
    let mut cols: Vec<(i32, i32)> = Vec::with_capacity(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let x = origin[0] + dx;
            let z = origin[2] + dz;
            if ow.is_loaded(host, [x, 64, z]) {
                cols.push((x, z));
            }
        }
    }
    if cols.is_empty() {
        return;
    }

    let below: u32 = 56;
    let above: u32 = 4;
    let span = (below + above) as usize;

    let Ok(col_blocks) = host.read_columns(OVERWORLD, &cols, Heightmap::WorldSurface, (below, above))
    else {
        return;
    };

    let first_col = cols
        .iter()
        .enumerate()
        .find_map(|(i, c)| col_blocks.bases[i].map(|b| (*c, b)));
    let base_is_bottom = first_col.is_some_and(|(col, b)| {
        host.surface_heights(OVERWORLD, &[col], Heightmap::WorldSurface)
            .ok()
            .and_then(|hs| hs.first().copied().flatten())
            .is_none_or(|surf| b <= surf - (below as i32) + 1)
    });

    let tech_flags: Vec<bool> = col_blocks
        .palette
        .iter()
        .map(|s| {
            let id = s.block().id(host);
            is_technical_block(strip_ns(&id))
        })
        .collect();
    let masonry_flags: Vec<bool> = col_blocks
        .palette
        .iter()
        .map(|s| {
            let id = s.block().id(host);
            is_bastion_masonry(strip_ns(&id))
        })
        .collect();

    let mut tech_scrubs: Vec<([i32; 3], BlockState)> = Vec::new();
    let mut pier_runs: Vec<BlockRun> = Vec::new();
    let min_masonry_y = origin[1] - 8;
    let max_masonry_y = origin[1] + 44;

    if col_blocks.blocks.len() == cols.len() * span {
        for (ci, &(x, z)) in cols.iter().enumerate() {
            let Some(base_val) = col_blocks.bases[ci] else {
                continue;
            };
            let base_y = if base_is_bottom {
                base_val
            } else {
                base_val - (below as i32)
            };
            let slice = &col_blocks.blocks[ci * span..(ci + 1) * span];
            let mut lowest_masonry: Option<i32> = None;

            for (dy, &p_idx) in slice.iter().enumerate() {
                let idx = p_idx as usize;
                let y = base_y + (dy as i32);
                if tech_flags.get(idx).copied().unwrap_or(false) {
                    let pos = [x, y, z];
                    let solid_neighbors = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .into_iter()
                        .filter(|(ox, oz)| {
                            ow.block(host, [x + ox, y, z + oz])
                                .map(|k| k.id(host))
                                .is_some_and(|b| {
                                    let s = strip_ns(&b);
                                    !is_technical_block(s) && !is_fillable_terrain_void(s)
                                })
                        })
                        .count();
                    let repl = if solid_neighbors >= 2 { blackstone } else { air };
                    tech_scrubs.push((pos, repl));
                }
                if y >= min_masonry_y
                    && y <= max_masonry_y
                    && masonry_flags.get(idx).copied().unwrap_or(false)
                {
                    lowest_masonry = Some(lowest_masonry.map_or(y, |cur| cur.min(y)));
                }
            }

            if let Some(floor_y) = lowest_masonry {
                let stop_y = (floor_y - 36).max(OVERWORLD_MIN_Y + 1);
                let mut ground_top = floor_y;
                for y in (stop_y..floor_y).rev() {
                    let Some(k) = ow.block(host, [x, y, z]) else {
                        break;
                    };
                    let b = k.id(host);
                    if !is_fillable_terrain_void(strip_ns(&b)) {
                        break;
                    }
                    ground_top = y;
                }
                if ground_top < floor_y {
                    let h = spatial_hash(x, ground_top, z, 0xB457_1001) % 20;
                    let pal_idx = if flavor == NetherFlavor::BasaltDeltas && h < 7 {
                        2
                    } else if h < 4 {
                        1
                    } else if h == 19 {
                        2
                    } else {
                        0
                    };
                    pier_runs.push(BlockRun {
                        x,
                        z,
                        y: ground_top,
                        height: (floor_y - ground_top) as u32,
                        surface: None,
                        block: pal_idx,
                        only: None,
                    });
                }
            }
        }
    }

    for (pos, repl) in tech_scrubs {
        let _ = ow.set_block(host, pos, repl, BlockUpdateFlags::CLIENTS);
    }

    if !pier_runs.is_empty() {
        let edit = BlockEdit {
            palette: vec![blackstone, cracked, basalt],
            filters: Vec::new(),
            runs: pier_runs,
        };
        let _ = host.edit_blocks(OVERWORLD, &edit);
    }
}

/// Clean fallback ruined blackstone bastion keep + magma core if `/place jigsaw`
/// cannot resolve a start piece cleanly.
fn build_fallback_bastion_keep(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    origin: [i32; 3],
    flavor: NetherFlavor,
    roll: u64,
) {
    let Some(blackstone) = host.block_state_id("minecraft:blackstone") else {
        return;
    };
    let bricks = host
        .block_state_id("minecraft:polished_blackstone_bricks")
        .unwrap_or(blackstone);
    let cracked = host
        .block_state_id("minecraft:cracked_polished_blackstone_bricks")
        .unwrap_or(blackstone);
    let gilded = host
        .block_state_id("minecraft:gilded_blackstone")
        .unwrap_or(blackstone);
    let basalt = host
        .block_state_id("minecraft:polished_basalt")
        .unwrap_or(blackstone);
    let magma = host
        .block_state_id("minecraft:magma_block")
        .unwrap_or(blackstone);
    let air = host.block_state_id("minecraft:air").unwrap_or(blackstone);

    let mut runs = Vec::new();
    let half = 9i32;
    let wall_h = 12i32;

    for dx in -half..=half {
        for dz in -half..=half {
            let x = origin[0] + dx;
            let z = origin[2] + dz;
            let edge = dx.abs() == half || dz.abs() == half;
            let corner = dx.abs() >= half - 1 && dz.abs() >= half - 1;
            let h = spatial_hash(x, origin[1], z, roll);

            // Grounded pier downward into solid terrain.
            let mut down_y = origin[1] - 1;
            for y in ((origin[1] - 24).max(OVERWORLD_MIN_Y + 2)..origin[1]).rev() {
                let b = ow.block(host, [x, y, z]).map(|k| k.id(host)).unwrap_or_default();
                if !is_fillable_terrain_void(strip_ns(&b)) {
                    break;
                }
                down_y = y;
            }
            if down_y < origin[1] {
                runs.push(BlockRun {
                    x,
                    z,
                    y: down_y,
                    height: (origin[1] - down_y) as u32,
                    surface: None,
                    block: if flavor == NetherFlavor::BasaltDeltas { 4 } else { 0 },
                    only: None,
                });
            }

            // Courtyard floor.
            let floor_idx = if dx.abs() <= 2 && dz.abs() <= 2 {
                5
            } else if h % 12 == 0 {
                3
            } else if h % 3 == 0 {
                2
            } else {
                1
            };
            runs.push(BlockRun {
                x,
                z,
                y: origin[1],
                height: 1,
                surface: None,
                block: floor_idx,
                only: None,
            });

            // Interior clearance or crenellated outer ramparts / corner towers.
            if edge || corner {
                let top_h = if corner {
                    wall_h + 3
                } else if h % 5 == 0 {
                    wall_h - 2
                } else {
                    wall_h
                };
                runs.push(BlockRun {
                    x,
                    z,
                    y: origin[1] + 1,
                    height: top_h as u32,
                    surface: None,
                    block: if corner { 4 } else if h % 4 == 0 { 2 } else { 1 },
                    only: None,
                });
            } else {
                runs.push(BlockRun {
                    x,
                    z,
                    y: origin[1] + 1,
                    height: (wall_h + 3) as u32,
                    surface: None,
                    block: 6,
                    only: None,
                });
            }
        }
    }

    let edit = BlockEdit {
        palette: vec![blackstone, bricks, cracked, gilded, basalt, magma, air],
        filters: Vec::new(),
        runs,
    };
    let _ = host.edit_blocks(OVERWORLD, &edit);

    // Spawn garrison Piglins + Brutes inside the courtyard.
    for (idx, kind) in [
        "minecraft:piglin_brute",
        "minecraft:piglin_brute",
        "minecraft:piglin",
        "minecraft:piglin",
        "minecraft:piglin",
    ]
    .into_iter()
    .enumerate()
    {
        let mut nbt = Compound::new();
        nbt.insert("IsImmuneToZombification".into(), mod_api::Tag::Byte(1));
        nbt.insert("PersistenceRequired".into(), mod_api::Tag::Byte(1));
        let ox = f64::from(origin[0]) + ((idx as f64) - 2.0) * 2.0;
        let oz = f64::from(origin[2]) + (((idx % 2) as f64) - 0.5) * 2.5;
        let _ = host.summon(OVERWORLD, kind, [ox, f64::from(origin[1] + 1), oz], nbt);
    }
}

fn mark_bastion_footprint_biomes(
    host: &mut dyn ServerHost,
    ow: LevelRef,
    state: &mut RiftState,
    origin: [i32; 3],
    flavor: NetherFlavor,
) {
    for x in (origin[0] - 48..=origin[0] + 48).step_by(4) {
        for z in (origin[2] - 48..=origin[2] + 48).step_by(4) {
            let p = [x, origin[1], z];
            if state.mark_quart_corrupted(p) {
                paint_biome_column(ow, host, x, z, flavor);
            }
        }
    }
    let _ = OVERWORLD_MAX_Y;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::build_ring_columns;

    fn sample_rift() -> RiftSource {
        RiftSource {
            id: 1,
            center: [200, 64, 200],
            active_portal: true,
            contained: false,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 20_000,
            corrupted_blocks: 200_000,
            max_radius_sq: 60 * 60,
            ring_radius: 60,
            ring_columns: build_ring_columns(200, 200, 60),
            active_column: None,
            deepen_radius: 28,
            deepen_columns: Vec::new(),
            bastions_placed: 0,
        }
    }

    #[test]
    fn bastion_requires_heavy_deep_corruption_and_rarity_limits() {
        let mut rift = sample_rift();
        assert!(rift_eligible_for_bastion(&rift));

        let mut small = sample_rift();
        small.corrupted_blocks = 80_000;
        assert!(!rift_eligible_for_bastion(&small));

        let mut shallow = sample_rift();
        shallow.deepen_radius = 10;
        assert!(!rift_eligible_for_bastion(&shallow));

        rift.contained = true;
        assert!(!rift_eligible_for_bastion(&rift));

        rift.contained = false;
        rift.bastions_placed = 1;
        assert!(!rift_eligible_for_bastion(&rift));
        rift.corrupted_blocks = 550_000;
        rift.ring_radius = 100;
        assert!(rift_eligible_for_bastion(&rift));

        rift.bastions_placed = 2;
        assert!(!rift_eligible_for_bastion(&rift));
    }

    #[test]
    fn bastion_refuses_to_appear_near_players_or_uncorrupted_area() {
        let near_player = [[240, 64, 240]];
        let far_player = [[500, 64, 500]];
        assert!(!all_players_far_from(&near_player, [200, 64, 200], 112));
        assert!(all_players_far_from(&far_player, [200, 64, 200], 112));
        assert!(all_players_far_from(&[], [200, 64, 200], 112));

        assert!(!area_metrics_qualify(25, 12, 0, 25));
        assert!(!area_metrics_qualify(24, 22, 1, 25));
        assert!(area_metrics_qualify(24, 21, 0, 25));
    }
}
