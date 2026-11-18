use crate::corruption::is_air_like;
use crate::mobs::pos_is_corrupted;
use crate::state::RiftState;
use crate::types::{
    BastionSite, NetherFlavor, OVERWORLD, dist_sq, horiz_dist_sq, quart_block_origin, quart_coords,
    spatial_hash,
};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    Compound, Host, NativeBlockEdit, NativeBlockRun, NativeHeightmap, Tag, Text,
};

const BASTION_CHECK_INTERVAL_TICKS: u64 = 180;
const MIN_RIFT_BLOCKS_FOR_BASTION: u64 = 8_500;
const MIN_RADIUS_SQ_FOR_BASTION: i64 = 68 * 68;
const MAX_BASTIONS_PER_RIFT: u32 = 3;

pub fn tick_bastion_emergence(host: &mut dyn Host, state: &mut RiftState) {
    if state.paused || state.tick % BASTION_CHECK_INTERVAL_TICKS != 90 {
        return;
    }

    let mut chosen: Option<(usize, [i32; 3], NetherFlavor, u64)> = None;
    for (idx, rift) in state.rifts.iter().enumerate() {
        if rift.contained
            || rift.bastions_placed >= MAX_BASTIONS_PER_RIFT
            || rift.corrupted_blocks < MIN_RIFT_BLOCKS_FOR_BASTION
            || rift.max_radius_sq < MIN_RADIUS_SQ_FOR_BASTION
        {
            continue;
        }
        if let Some((site, flavor, roll)) = find_bastion_site(host, state, rift) {
            chosen = Some((idx, site, flavor, roll));
            break;
        }
    }

    let Some((rift_idx, origin, flavor, roll)) = chosen else {
        return;
    };

    prepare_blackstone_pad(host, state, origin, flavor);
    queue_vanilla_bastion(origin, roll);
    mark_bastion_footprint_biomes(state, origin, flavor);
    spawn_bastion_garrison(host, origin);

    state.rifts[rift_idx].bastions_placed += 1;
    state.bastions.push(BastionSite {
        center: origin,
        flavor,
    });
    state.dirty = true;

    for player in host.server_players() {
        if player.dimension == OVERWORLD
            && horiz_dist_sq(
                [player.position[0] as i32, 64, player.position[2] as i32],
                origin,
            ) <= 220 * 220
        {
            let _ = host.tell(
                player.uuid,
                Text::literal(format!(
                    "[Rift Bleed] A Bastion Remnant has breached into the Overworld near ({}, {}, {})!",
                    origin[0], origin[1], origin[2]
                )),
            );
        }
    }
}

fn find_bastion_site(
    host: &dyn Host,
    state: &RiftState,
    rift: &crate::types::RiftSource,
) -> Option<([i32; 3], NetherFlavor, u64)> {
    let radius = (rift.max_radius_sq as f64).sqrt() as i32;
    let min_ring = 58.max(radius / 2);
    let max_ring = 115.min(radius.max(66));

    for attempt in 0..6u64 {
        let h = spatial_hash(rift.center[0], rift.bastions_placed as i32, rift.center[2], state.tick ^ attempt);
        let angle = ((h & 0xFFFF) as f64 / 65535.0) * std::f64::consts::TAU;
        let dist = min_ring + (((h >> 16) % ((max_ring - min_ring + 1).max(1) as u64)) as i32);
        let cx = rift.center[0] + (angle.cos() * f64::from(dist)).round() as i32;
        let cz = rift.center[2] + (angle.sin() * f64::from(dist)).round() as i32;

        // Keep away from world spawn (within 84 blocks of 0,0) and all rift portals/other bastions.
        if horiz_dist_sq([cx, 64, cz], [0, 64, 0]) < 84 * 84 {
            continue;
        }
        if state
            .rifts
            .iter()
            .any(|r| horiz_dist_sq(r.center, [cx, 64, cz]) < 52 * 52)
        {
            continue;
        }
        if state
            .bastions
            .iter()
            .any(|b| dist_sq(b.center, [cx, 64, cz]) < 150 * 150)
        {
            continue;
        }

        let probe_cols = [
            (cx, cz),
            (cx + 22, cz),
            (cx, cz + 22),
            (cx + 22, cz + 22),
            (cx + 11, cz + 11),
        ];
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
        if max_h - min_h > 10 || avg_h < 56 || avg_h > 122 {
            continue;
        }

        // Require terrain there to actually be reached by corruption.
        let mut corrupt_samples = 0;
        for (idx, (px, pz)) in probe_cols.iter().enumerate() {
            if pos_is_corrupted(host, state, [*px, hs[idx] - 1, *pz]) {
                corrupt_samples += 1;
            }
        }
        if corrupt_samples < 2 {
            continue;
        }

        let floor_block = host.block([cx + 11, avg_h - 1, cz + 11]);
        if matches!(
            crate::types::strip_ns(&floor_block),
            "water" | "flowing_water" | "air" | "cave_air"
        ) {
            continue;
        }

        let flavor = NetherFlavor::pick_for_region(cx, cz, state.seed ^ rift.id);
        return Some(([cx, min_h.max(avg_h - 2), cz], flavor, h));
    }

    None
}

fn prepare_blackstone_pad(
    host: &mut dyn Host,
    _state: &mut RiftState,
    origin: [i32; 3],
    flavor: NetherFlavor,
) {
    let mut runs = Vec::new();
    let pad_block = if flavor == NetherFlavor::BasaltDeltas {
        "minecraft:basalt"
    } else {
        "minecraft:blackstone"
    };
    for dx in -2..=28 {
        for dz in -2..=28 {
            let x = origin[0] + dx;
            let z = origin[2] + dz;
            // Fill small pockets right beneath the Bastion footprint so it never floats awkwardly.
            for dy in -4..=0 {
                let b = host.block([x, origin[1] + dy, z]);
                if is_air_like(&b) {
                    runs.push(NativeBlockRun {
                        x,
                        z,
                        y: origin[1] + dy,
                        height: 1,
                        surface: None,
                        block: 0,
                        only: None,
                    });
                }
            }
        }
    }
    if !runs.is_empty() {
        let edit = NativeBlockEdit {
            palette: vec![pad_block.to_owned()],
            filters: Vec::new(),
            runs,
            block_entities: Vec::new(),
            neighbour_updates: false,
        };
        let _ = host.edit_blocks(OVERWORLD, &edit);
    }
}

fn queue_vanilla_bastion(origin: [i32; 3], roll: u64) {
    let rot = (roll >> 20) as u8 & 3;
    match (roll >> 24) % 4 {
        0 => {
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/treasure/bases/lava_basin".to_owned(),
                origin,
                rot,
            );
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/treasure/bases/centers/center_0".to_owned(),
                [origin[0] + 4, origin[1] + 2, origin[2] + 4],
                rot,
            );
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/treasure/entrances/entrance_0".to_owned(),
                [origin[0], origin[1] + 1, origin[2]],
                rot,
            );
        }
        1 => {
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/units/air_base".to_owned(),
                origin,
                rot,
            );
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/units/center_pieces/center_0".to_owned(),
                [origin[0] + 2, origin[1], origin[2] + 2],
                rot,
            );
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/units/ramparts/ramparts_0".to_owned(),
                [origin[0], origin[1] + 6, origin[2]],
                rot,
            );
        }
        2 => {
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/hoglin_stable/starting_pieces/starting_stairs_0".to_owned(),
                origin,
                rot,
            );
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/hoglin_stable/small_stables/inner_0".to_owned(),
                [origin[0] + 8, origin[1], origin[2] + 4],
                rot,
            );
        }
        _ => {
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/bridge/starting_pieces/entrance_base".to_owned(),
                origin,
                rot,
            );
            rift_bridge::enqueue_bastion_piece(
                "minecraft:bastion/bridge/bridge_pieces/bridge".to_owned(),
                [origin[0] + 6, origin[1] + 2, origin[2] + 6],
                rot,
            );
        }
    }
}

fn mark_bastion_footprint_biomes(state: &mut RiftState, origin: [i32; 3], flavor: NetherFlavor) {
    let mut biome_updates = Vec::new();
    let mut quart_records = Vec::new();
    for x in (origin[0] - 4..=origin[0] + 32).step_by(4) {
        for z in (origin[2] - 4..=origin[2] + 32).step_by(4) {
            for y in (origin[1] - 8..=origin[1] + 28).step_by(4) {
                let p = [x, y, z];
                if state.mark_quart_corrupted(p) {
                    biome_updates.push((quart_block_origin(p), flavor.to_u8()));
                    quart_records.push(quart_coords(p));
                }
            }
        }
    }
    if !biome_updates.is_empty() {
        rift_bridge::enqueue_biomes(&biome_updates);
        rift_bridge::record_quart_cells(&quart_records);
    }
}

fn spawn_bastion_garrison(host: &mut dyn Host, origin: [i32; 3]) {
    let spots = [
        [
            f64::from(origin[0]) + 8.5,
            f64::from(origin[1] + 2),
            f64::from(origin[2]) + 8.5,
        ],
        [
            f64::from(origin[0]) + 14.5,
            f64::from(origin[1] + 2),
            f64::from(origin[2]) + 12.5,
        ],
        [
            f64::from(origin[0]) + 10.5,
            f64::from(origin[1] + 4),
            f64::from(origin[2]) + 14.5,
        ],
    ];

    for (idx, pos) in spots.into_iter().enumerate() {
        let mut nbt = Compound::new();
        nbt.insert("PersistenceRequired".into(), Tag::Byte(1));
        nbt.insert("IsImmuneToZombification".into(), Tag::Byte(1));
        let kind = if idx < 2 {
            "minecraft:piglin_brute"
        } else {
            "minecraft:piglin"
        };
        let _ = host.summon(OVERWORLD, kind, pos, nbt);
    }
}
