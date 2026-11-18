use crate::corruption::{is_nether_terrain_block, is_water_like};
use crate::state::{PendingBastion, RiftState};
use crate::types::{
    NetherFlavor, OVERWORLD, RiftSource, dist_sq, horiz_dist_sq, quart_block_origin, quart_coords,
    spatial_hash,
};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{Host, NativeHeightmap, Text};

const OWNER_BASTION_TICKET: &str = "rift_bleed:bastion";

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

/// Releases any active or post-placement Bastion chunk-load tickets cleanly.
pub fn release_bastion_tickets(host: &mut dyn Host, state: &mut RiftState) {
    if let Some(pending) = state.pending_bastion.take() {
        for c in pending.tickets {
            let _ = host.chunk_load_release(OWNER_BASTION_TICKET, OVERWORLD, c);
        }
    }
    for c in state.bastion_release_tickets.drain(..) {
        let _ = host.chunk_load_release(OWNER_BASTION_TICKET, OVERWORLD, c);
    }
    state.bastion_release_ttl = 0;
}

pub fn tick_bastion_emergence(host: &mut dyn Host, state: &mut RiftState) {
    if state.bastion_release_ttl > 0 {
        state.bastion_release_ttl -= 1;
        if state.bastion_release_ttl == 0 {
            for c in state.bastion_release_tickets.drain(..) {
                let _ = host.chunk_load_release(OWNER_BASTION_TICKET, OVERWORLD, c);
            }
        }
    }

    if state.pending_bastion.is_some() {
        process_pending_bastion(host, state);
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
        let roll = spatial_hash(rift.center[0], rift.bastions_placed as i32, rift.center[2], state.seed ^ state.tick);
        if roll % BASTION_RARITY_DENOM != 0 {
            continue;
        }
        if let Some((origin, flavor, cand_roll)) =
            find_bastion_site(host, state, rift, &player_positions)
        {
            stage_pending_bastion(host, state, rift.id, origin, flavor, cand_roll);
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

fn overworld_player_positions(host: &dyn Host) -> Vec<[i32; 3]> {
    host.server_players()
        .into_iter()
        .filter(|p| p.dimension == OVERWORLD)
        .map(|p| {
            [
                p.position[0].floor() as i32,
                p.position[1].floor() as i32,
                p.position[2].floor() as i32,
            ]
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
    host: &dyn Host,
    state: &RiftState,
    rift: &RiftSource,
    overworld_players: &[[i32; 3]],
) -> Option<([i32; 3], NetherFlavor, u64)> {
    let Some(ow) = host.level(OVERWORLD) else {
        return None;
    };
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
        if !all_players_far_from(overworld_players, [cx + 8, 64, cz + 8], MIN_PLAYER_DIST_FOR_BASTION)
        {
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
            host.surface_heights(OVERWORLD, &probe_cols, NativeHeightmap::MotionBlockingNoLeaves)
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
            if let Some(name) = ow.block(host, p) {
                if is_water_like(&name) {
                    water_hits += 1;
                } else if is_nether_terrain_block(&name) {
                    nether_surface += 1;
                }
            }
            if let Some(top_name) = ow.block(host, [*px, hs[idx], *pz])
                && is_water_like(&top_name)
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
    host: &mut dyn Host,
    state: &mut RiftState,
    rift_id: u64,
    origin: [i32; 3],
    flavor: NetherFlavor,
    roll: u64,
) {
    let ccx = origin[0] >> 4;
    let ccz = origin[2] >> 4;
    let mut tickets = Vec::with_capacity(81);
    for dx in -4..=4 {
        for dz in -4..=4 {
            let chunk = [ccx + dx, ccz + dz];
            if host.chunk_load_request(OWNER_BASTION_TICKET, OVERWORLD, chunk) {
                tickets.push(chunk);
            }
        }
    }

    state.pending_bastion = Some(PendingBastion {
        rift_id,
        origin,
        flavor,
        roll,
        tickets,
        queued_tick: state.tick,
    });
}

fn process_pending_bastion(host: &mut dyn Host, state: &mut RiftState) {
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
        release_bastion_tickets(host, state);
        return;
    }

    // Timeout safety if chunks cannot load after 8 seconds.
    if state.tick.saturating_sub(pending.queued_tick) > 160 {
        release_bastion_tickets(host, state);
        return;
    }

    let Some(ow) = host.level(OVERWORLD) else {
        return;
    };
    let all_loaded = pending
        .tickets
        .iter()
        .all(|[cx, cz]| ow.is_loaded(host, [cx << 4, 64, cz << 4]));
    if !all_loaded {
        return;
    }

    // Re-check player distance one final time right before enqueueing placement.
    if !all_players_far_from(
        &players,
        [pending.origin[0] + 8, pending.origin[1], pending.origin[2] + 8],
        MIN_PLAYER_DIST_FOR_BASTION,
    ) {
        release_bastion_tickets(host, state);
        return;
    }

    state.pending_bastion = None;
    rift_bridge::enqueue_bastion_jigsaw(pending.origin, pending.flavor.to_u8(), pending.roll);
    mark_bastion_footprint_biomes(state, pending.origin, pending.flavor);

    if let Some(rift) = state.rifts.iter_mut().find(|r| r.id == pending.rift_id) {
        rift.bastions_placed += 1;
    }
    state.bastions.push(crate::types::BastionSite {
        center: pending.origin,
        flavor: pending.flavor,
    });
    state.bastion_release_tickets = pending.tickets;
    state.bastion_release_ttl = 4;
    state.dirty = true;

    for player in host.server_players() {
        if player.dimension == OVERWORLD
            && horiz_dist_sq(
                [player.position[0] as i32, 64, player.position[2] as i32],
                pending.origin,
            ) <= 340 * 340
        {
            let _ = host.tell(
                player.uuid,
                Text::literal(
                    "[Rift Bleed] A heavy blackstone rumble echoes somewhere deep within the corrupted lands...",
                ),
            );
        }
    }
}

fn mark_bastion_footprint_biomes(state: &mut RiftState, origin: [i32; 3], flavor: NetherFlavor) {
    let mut biome_updates = Vec::new();
    let mut quart_records = Vec::new();
    for x in (origin[0] - 48..=origin[0] + 48).step_by(4) {
        for z in (origin[2] - 48..=origin[2] + 48).step_by(4) {
            let p = [x, origin[1], z];
            if state.mark_quart_corrupted(p) {
                biome_updates.push((quart_block_origin(p), flavor.to_u8()));
                quart_records.push(quart_coords(p));
            }
        }
    }
    if !biome_updates.is_empty() {
        rift_bridge::enqueue_biomes(&biome_updates);
        rift_bridge::record_quart_cells(&quart_records);
    }
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

        // Too shallow / too small / contained must reject.
        let mut small = sample_rift();
        small.corrupted_blocks = 80_000;
        assert!(!rift_eligible_for_bastion(&small));

        let mut shallow = sample_rift();
        shallow.deepen_radius = 10;
        assert!(!rift_eligible_for_bastion(&shallow));

        rift.contained = true;
        assert!(!rift_eligible_for_bastion(&rift));

        // Second bastion needs massive territory.
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

        // Area must be >= 80% Nether terrain and dry.
        assert!(!area_metrics_qualify(25, 12, 0, 25));
        assert!(!area_metrics_qualify(24, 22, 1, 25));
        assert!(area_metrics_qualify(24, 21, 0, 25));
    }
}
