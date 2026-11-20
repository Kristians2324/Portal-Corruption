use crate::atmosphere::ParticlePalette;
use crate::corruption::{can_support_nether_flora, is_air_like, is_protected_block};
use crate::state::RiftState;
use crate::types::{
    BEDROCK_FLOOR_Y, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, build_ring_columns,
    spatial_hash, strip_ns,
};
use mod_api::{
    BlockUpdateFlags, Heightmap, LevelRef, ParticleOptions, ServerHost, SoundSource, Text,
};

const REGRESS_SURFACE_COLS_PER_TICK: usize = 20;
const REGRESS_DEEP_EDITS_PER_TICK: usize = 64;
const MAX_REGRESS_FORCED_CHUNKS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginalColumnInfo {
    /// Highest solid pre-feature ground Y (`TerrainColumn.height - 1`).
    pub surface_y: i32,
    /// Original top block id (e.g. `"minecraft:grass_block"`, `"minecraft:sand"`).
    pub surface_block: String,
    /// True if the original world generator placed ocean/river/lake water above this column.
    pub had_water: bool,
    /// Original Overworld biome id (e.g. `"minecraft:plains"`, `"minecraft:desert"`).
    pub biome: String,
}

impl Default for OriginalColumnInfo {
    fn default() -> Self {
        Self {
            surface_y: 63,
            surface_block: "minecraft:grass_block".to_owned(),
            had_water: false,
            biome: "minecraft:plains".to_owned(),
        }
    }
}

/// Checks if a dying Wither entity belongs to an active (unpacified) Citadel Rift
/// by persistent boss UUID first, then by fallback `rift_bleed:rift_<id>` entity tag.
pub fn match_wither_boss_rift_idx(
    state: &RiftState,
    entity_uuid: Option<u128>,
    tags: &[String],
) -> Option<usize> {
    state.rifts.iter().position(|r| {
        if !r.citadel_spawned || r.pacified {
            return false;
        }
        if entity_uuid.is_some() && r.boss_uuid == entity_uuid {
            return true;
        }
        let expected_tag = format!("rift_bleed:rift_{}", r.id);
        tags.iter().any(|t| t == &expected_tag)
    })
}

/// Pure transition applied when a Rift's Wither Sovereign is defeated:
/// halts corruption permanently, clears containment requirement, and begins ~80%
/// inward Overworld regression without affecting any other active portal.
pub fn pacify_and_start_regression(state: &mut RiftState, rift_idx: usize) -> Option<u64> {
    let rift = state.rifts.get_mut(rift_idx)?;
    let rift_id = rift.id;
    rift.pacified = true;
    rift.regressing = true;
    rift.boss_uuid = None;
    rift.active_column = None;

    let start_r = rift.ring_radius.max(1);
    rift.regress_radius = start_r;
    rift.regress_columns = build_ring_columns(rift.center[0], rift.center[2], start_r);
    rift.regress_active_column = None;

    // Re-seed deepen sweep to walk inward cleanly from deepest touched radius -> 0.
    let deep_start = rift.deepen_radius.max(rift.ring_radius.min(48)).max(1);
    rift.deepen_radius = deep_start;
    rift.deepen_columns = build_ring_columns(rift.center[0], rift.center[2], deep_start);

    state.active_deepen_columns.remove(&rift_id);
    if state
        .pending_bastion
        .as_ref()
        .is_some_and(|b| b.rift_id == rift_id)
    {
        state.pending_bastion = None;
    }
    if state
        .pending_citadel
        .as_ref()
        .is_some_and(|c| c.rift_id == rift_id)
    {
        state.pending_citadel = None;
    }
    state.dirty = true;
    Some(rift_id)
}

pub fn try_handle_boss_death(
    host: &mut dyn ServerHost,
    state: &mut RiftState,
    entity_id: u32,
    kind: &str,
    death_pos: Option<[f64; 3]>,
    particles: Option<&ParticlePalette>,
) -> bool {
    if strip_ns(kind) != "wither" {
        return false;
    }

    let (entity_uuid, tags) = if let Some(ent) = host.entity(entity_id) {
        let u = ent.uuid(host);
        let t = ent.tags(host).unwrap_or_default();
        (u, t)
    } else {
        (None, Vec::new())
    };

    let Some(rift_idx) = match_wither_boss_rift_idx(state, entity_uuid, &tags) else {
        return false;
    };

    let center = state.rifts[rift_idx].center;
    let Some(rift_id) = pacify_and_start_regression(state, rift_idx) else {
        return false;
    };

    let msg = format!(
        "[Rift Bleed] The Sovereign of Rift #{} at ({}, {}, {}) is vanquished! Corruption halts, the Overworld begins reclaiming the land, and this portal is now permanently safe.",
        rift_id, center[0], center[1], center[2]
    );
    let fx_pos = death_pos.unwrap_or([
        f64::from(center[0]) + 0.5,
        f64::from(center[1]) + 3.0,
        f64::from(center[2]) + 0.5,
    ]);

    if let Some(ow) = host.level(OVERWORLD) {
        if let Some(pal) = particles {
            let _ = ow.send_particles(
                host,
                &ParticleOptions::simple(pal.white_ash),
                None,
                fx_pos,
                [2.5, 2.0, 2.5],
                0.05,
                60,
            );
            let _ = ow.send_particles(
                host,
                &ParticleOptions::simple(pal.flame),
                None,
                fx_pos,
                [1.8, 1.5, 1.8],
                0.08,
                32,
            );
        }
        let _ = ow.play_sound(
            host,
            "minecraft:ui.toast.challenge_complete",
            fx_pos,
            SoundSource::Hostile,
            0.65,
            1.0,
        );
    }

    for player in host.players().server_players() {
        if player
            .dimension(host)
            .is_some_and(|d| d.to_string() == OVERWORLD)
        {
            let _ = player.tell(host, Text::literal(msg.clone()));
        }
    }

    true
}

/// Groups ~20% of wasteland blocks into coherent 4x4x4-based organic scar patches
/// so the reclaimed Overworld heals ~80% back to normal while visibly keeping
/// permanent Nether craters, veins, and Ancient Debris treasures.
pub fn in_persistent_scar_patch(pos: [i32; 3]) -> bool {
    let cluster = spatial_hash(pos[0] >> 2, pos[1] >> 2, pos[2] >> 2, 0x5CA2_9901) % 100;
    let fine = spatial_hash(pos[0], pos[1], pos[2], 0x5CA2_9902) % 100;
    cluster < 36 && fine < 56
}

fn is_monument_masonry(short: &str) -> bool {
    matches!(
        short,
        "polished_blackstone_bricks"
            | "cracked_polished_blackstone_bricks"
            | "chiseled_polished_blackstone"
            | "polished_blackstone"
            | "gilded_blackstone"
            | "nether_bricks"
            | "red_nether_bricks"
            | "cracked_nether_bricks"
            | "polished_basalt"
            | "gold_block"
            | "ancient_debris"
    )
}

fn is_nether_soil_or_stone(short: &str) -> bool {
    matches!(
        short,
        "netherrack"
            | "crimson_nylium"
            | "warped_nylium"
            | "soul_sand"
            | "soul_soil"
            | "basalt"
            | "smooth_basalt"
            | "blackstone"
            | "magma_block"
            | "glowstone"
            | "bone_block"
            | "nether_quartz_ore"
            | "nether_gold_ore"
            | "coarse_dirt"
    )
}

fn pick_restored_surface_block(
    pos: [i32; 3],
    above: &str,
    below: &str,
    original: &OriginalColumnInfo,
) -> &'static str {
    let exposed = is_air_like(above) || strip_ns(above) == "short_grass";
    let unsafe_gravity = is_air_like(below);

    if original.had_water {
        if exposed && !unsafe_gravity {
            return match strip_ns(&original.surface_block) {
                "gravel" => "minecraft:gravel",
                "clay" => "minecraft:clay",
                "sand" => "minecraft:sand",
                _ => {
                    if spatial_hash(pos[0], pos[1], pos[2], 0x5EA8_ED01) % 3 == 0 {
                        "minecraft:gravel"
                    } else {
                        "minecraft:sand"
                    }
                }
            };
        }
        return if strip_ns(&original.surface_block) == "stone" {
            "minecraft:stone"
        } else {
            "minecraft:dirt"
        };
    }

    match strip_ns(&original.surface_block) {
        "sand" => {
            if unsafe_gravity || pos[1] < original.surface_y - 2 {
                "minecraft:sandstone"
            } else {
                "minecraft:sand"
            }
        }
        "red_sand" => {
            if unsafe_gravity || pos[1] < original.surface_y - 2 {
                "minecraft:red_sandstone"
            } else {
                "minecraft:red_sand"
            }
        }
        "gravel" => {
            if unsafe_gravity {
                "minecraft:stone"
            } else {
                "minecraft:gravel"
            }
        }
        "snow_block" | "powder_snow" => {
            if exposed {
                "minecraft:snow_block"
            } else {
                "minecraft:stone"
            }
        }
        "terracotta" => "minecraft:terracotta",
        "podzol" => {
            if exposed {
                "minecraft:podzol"
            } else {
                "minecraft:dirt"
            }
        }
        "mycelium" => {
            if exposed {
                "minecraft:mycelium"
            } else {
                "minecraft:dirt"
            }
        }
        "moss_block" => {
            if exposed {
                "minecraft:moss_block"
            } else {
                "minecraft:dirt"
            }
        }
        "stone" | "calcite" | "andesite" | "diorite" | "granite" => "minecraft:stone",
        _ => {
            if exposed {
                "minecraft:grass_block"
            } else {
                "minecraft:dirt"
            }
        }
    }
}

/// Pure deterministic block restoration rule returning `Some(target_block_id)` when a
/// corrupted block heals back toward the original Overworld (~80% overall, ~20% persistent
/// Nether scars + 100% Ancient Debris + monument masonry preserved, zero water placed).
pub fn transform_restore_block(
    pos: [i32; 3],
    current: &str,
    above: &str,
    below: &str,
    original: &OriginalColumnInfo,
    near_monument: bool,
) -> Option<&'static str> {
    if is_protected_block(current) || is_air_like(current) {
        return None;
    }

    let short = strip_ns(current);
    if is_monument_masonry(short) {
        return None;
    }

    if matches!(short, "shroomlight" | "nether_wart_block" | "warped_wart_block") {
        if near_monument || in_persistent_scar_patch(pos) {
            return None;
        }
        return Some("minecraft:air");
    }

    if matches!(
        short,
        "crimson_roots"
            | "warped_roots"
            | "nether_sprouts"
            | "crimson_fungus"
            | "warped_fungus"
            | "weeping_vines"
            | "weeping_vines_plant"
            | "twisting_vines"
            | "twisting_vines_plant"
            | "fire"
            | "soul_fire"
    ) {
        let base_stays_nether = !near_monument && in_persistent_scar_patch([pos[0], pos[1] - 1, pos[2]])
            && (can_support_nether_flora(below)
                || matches!(strip_ns(below), "soul_sand" | "netherrack" | "magma_block"));
        if base_stays_nether {
            return None;
        }
        return Some("minecraft:air");
    }

    if matches!(
        short,
        "crimson_stem"
            | "warped_stem"
            | "stripped_crimson_stem"
            | "stripped_warped_stem"
            | "crimson_hyphae"
            | "warped_hyphae"
    ) {
        if near_monument || in_persistent_scar_patch(pos) {
            return None;
        }
        let snowy = original.biome.contains("snow")
            || original.biome.contains("taiga")
            || original.biome.contains("peaks");
        return Some(if snowy {
            "minecraft:spruce_log"
        } else {
            "minecraft:oak_log"
        });
    }

    if !is_nether_soil_or_stone(short) {
        return None;
    }

    if in_persistent_scar_patch(pos) {
        return None;
    }

    if pos[1] >= original.surface_y - 3 {
        Some(pick_restored_surface_block(pos, above, below, original))
    } else if pos[1] < 0 {
        Some("minecraft:deepslate")
    } else {
        Some("minecraft:stone")
    }
}

fn near_citadel_or_bastion(state: &RiftState, x: i32, z: i32) -> bool {
    state
        .citadels
        .iter()
        .any(|c| (x - c[0]).abs() <= 32 && (z - c[2]).abs() <= 32)
        || state
            .bastions
            .iter()
            .any(|b| (x - b.center[0]).abs() <= 36 && (z - b.center[2]).abs() <= 36)
}

fn parse_original_info(col: Option<&mod_api::TerrainColumn>) -> OriginalColumnInfo {
    match col {
        Some(c) => OriginalColumnInfo {
            surface_y: (c.height - 1).clamp(OVERWORLD_MIN_Y + 4, OVERWORLD_MAX_Y - 2),
            surface_block: c.block.clone(),
            had_water: c.water.is_some(),
            biome: c.biome.clone(),
        },
        None => OriginalColumnInfo::default(),
    }
}

fn set_overworld_biome_column(
    ow: LevelRef,
    host: &mut dyn ServerHost,
    x: i32,
    z: i32,
    biome: &str,
) {
    let qx = (x >> 2) << 2;
    let qz = (z >> 2) << 2;
    for qy in (-64..=316).step_by(4) {
        let _ = ow.set_biome(host, [qx, qy, qz], biome);
    }
}

fn resolve_cached(host: &dyn ServerHost, id: &str) -> Option<mod_api::BlockState> {
    host.block_state_id(id)
        .or_else(|| host.block_key(id).and_then(|k| host.block_state_default(k)))
}

fn sync_regress_chunks(host: &mut dyn ServerHost, level: LevelRef, state: &mut RiftState) {
    let mut desired: Vec<[i32; 2]> = Vec::new();
    for rift in &state.rifts {
        if !rift.regressing {
            continue;
        }
        if let Some(([x, z], _, _)) = rift.regress_active_column {
            let c = [x >> 4, z >> 4];
            if !desired.contains(&c) {
                desired.push(c);
            }
        }
        for col in rift.regress_columns.iter().rev().take(24) {
            let c = [col[0] >> 4, col[1] >> 4];
            if !desired.contains(&c) {
                desired.push(c);
                if desired.len() >= MAX_REGRESS_FORCED_CHUNKS {
                    break;
                }
            }
        }
        if desired.len() >= MAX_REGRESS_FORCED_CHUNKS {
            break;
        }
    }

    if desired.is_empty() {
        while let Some(old) = state.regress_chunks.pop_front() {
            if let Some(cref) = level.chunk_ref(host, old) {
                let _ = cref.set_forced(host, false);
            }
        }
        return;
    }

    for c in desired {
        if state.regress_chunks.contains(&c) {
            continue;
        }
        while state.regress_chunks.len() >= MAX_REGRESS_FORCED_CHUNKS {
            if let Some(old) = state.regress_chunks.pop_front()
                && let Some(cref) = level.chunk_ref(host, old)
            {
                let _ = cref.set_forced(host, false);
            }
        }
        if let Some(cref) = level.chunk_ref(host, c)
            && cref.set_forced(host, true)
        {
            state.regress_chunks.push_back(c);
        }
    }
}

fn pop_inward_loaded_col(
    host: &dyn ServerHost,
    level: LevelRef,
    cols: &mut Vec<[i32; 2]>,
    radius: &mut i32,
    center: [i32; 3],
) -> Option<[i32; 2]> {
    for _ in 0..4 {
        if cols.is_empty() {
            if *radius <= 0 {
                return None;
            }
            *radius -= 1;
            *cols = build_ring_columns(center[0], center[2], *radius);
        }
        if let Some(pos_idx) = cols.iter().rposition(|c| level.is_loaded(host, [c[0], 64, c[1]])) {
            return Some(cols.swap_remove(pos_idx));
        }
        if cols.len() > 64 {
            break;
        }
    }
    None
}

pub fn tick_regression(host: &mut dyn ServerHost, ow: LevelRef, state: &mut RiftState) {
    if state.paused || !state.has_regressing() {
        if !state.regress_chunks.is_empty() {
            sync_regress_chunks(host, ow, state);
        }
        return;
    }

    if state.tick % 20 == 5 {
        sync_regress_chunks(host, ow, state);
    }

    let regress_indices: Vec<usize> = state
        .rifts
        .iter()
        .enumerate()
        .filter(|(_, r)| r.regressing)
        .map(|(i, _)| i)
        .collect();
    if regress_indices.is_empty() {
        return;
    }

    let rift_idx = regress_indices[(state.tick as usize) % regress_indices.len()];
    let rift_id = state.rifts[rift_idx].id;
    let center = state.rifts[rift_idx].center;

    // Step 1: Inward surface + biome regression batch.
    let mut surface_batch = Vec::with_capacity(REGRESS_SURFACE_COLS_PER_TICK);
    for _ in 0..REGRESS_SURFACE_COLS_PER_TICK {
        let mut rad = state.rifts[rift_idx].regress_radius;
        let picked = pop_inward_loaded_col(
            host,
            ow,
            &mut state.rifts[rift_idx].regress_columns,
            &mut rad,
            center,
        );
        state.rifts[rift_idx].regress_radius = rad;
        if let Some(col) = picked {
            surface_batch.push(col);
        } else {
            break;
        }
    }

    if !surface_batch.is_empty() {
        let query: Vec<(i32, i32)> = surface_batch.iter().map(|c| (c[0], c[1])).collect();
        let terrain = host.terrain_surface(OVERWORLD, &query).unwrap_or_default();
        let ws_heights = host
            .surface_heights(OVERWORLD, &query, Heightmap::WorldSurface)
            .unwrap_or_default();
        let of_heights = host
            .surface_heights(OVERWORLD, &query, Heightmap::OceanFloor)
            .unwrap_or_default();

        for (i, col) in surface_batch.iter().copied().enumerate() {
            if state.other_active_rift_covers(rift_id, col[0], col[1]) {
                continue;
            }
            let orig = parse_original_info(terrain.get(i).and_then(|o| o.as_ref()));
            let near_monument = near_citadel_or_bastion(state, col[0], col[1]);

            if state.is_quart_corrupted([col[0], orig.surface_y, col[1]]) {
                state.unmark_quart_col(col[0], col[1]);
                set_overworld_biome_column(ow, host, col[0], col[1], &orig.biome);
            }

            let top_ws = ws_heights
                .get(i)
                .copied()
                .flatten()
                .unwrap_or(orig.surface_y + 4)
                .clamp(OVERWORLD_MIN_Y + 4, OVERWORLD_MAX_Y - 1);
            let floor_of = of_heights
                .get(i)
                .copied()
                .flatten()
                .unwrap_or(orig.surface_y);
            let top_y = (top_ws + 2).max(orig.surface_y + 2).min(OVERWORLD_MAX_Y - 1);
            let min_y = floor_of
                .min(orig.surface_y)
                .saturating_sub(6)
                .max(BEDROCK_FLOOR_Y);

            let flags = if orig.had_water {
                // Trigger neighbor fluid checks on ocean/river edges so border water can trickle back!
                BlockUpdateFlags::CLIENTS | BlockUpdateFlags::NEIGHBORS | BlockUpdateFlags::SUPPRESS_DROPS
            } else {
                BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS
            };

            for y in (min_y..=top_y).rev() {
                let pos = [col[0], y, col[1]];
                let Some(cur_key) = ow.block(host, pos) else {
                    continue;
                };
                let current = cur_key.id(host);
                let above = if y + 1 <= OVERWORLD_MAX_Y {
                    ow.block(host, [col[0], y + 1, col[1]])
                        .map(|k| k.id(host))
                        .unwrap_or_else(|| "minecraft:air".to_owned())
                } else {
                    "minecraft:air".to_owned()
                };
                let below = if y - 1 >= OVERWORLD_MIN_Y {
                    ow.block(host, [col[0], y - 1, col[1]])
                        .map(|k| k.id(host))
                        .unwrap_or_else(|| "minecraft:bedrock".to_owned())
                } else {
                    "minecraft:bedrock".to_owned()
                };

                if let Some(target) =
                    transform_restore_block(pos, &current, &above, &below, &orig, near_monument)
                    && let Some(bstate) = resolve_cached(host, target)
                {
                    let _ = ow.set_block(host, pos, bstate, flags);
                    state.dirty = true;
                }
            }
        }
    }

    // Step 2: Simultaneous inward deep-underground ~80% stone/deepslate restoration.
    let mut deep_budget = REGRESS_DEEP_EDITS_PER_TICK;
    let mut deep_skips = 0usize;
    let deep_fallback = OriginalColumnInfo {
        surface_y: center[1] - 4,
        ..OriginalColumnInfo::default()
    };

    while deep_budget > 0 && deep_skips < 240 {
        if state.rifts[rift_idx].regress_active_column.is_none() {
            let mut d_rad = state.rifts[rift_idx].deepen_radius;
            let picked = pop_inward_loaded_col(
                host,
                ow,
                &mut state.rifts[rift_idx].deepen_columns,
                &mut d_rad,
                center,
            );
            state.rifts[rift_idx].deepen_radius = d_rad;
            let Some(col) = picked else {
                break;
            };
            if state.other_active_rift_covers(rift_id, col[0], col[1]) {
                deep_skips += 12;
                continue;
            }
            let surf_y = host
                .surface_heights(OVERWORLD, &[(col[0], col[1])], Heightmap::OceanFloor)
                .ok()
                .and_then(|hs| hs.first().copied().flatten())
                .unwrap_or(center[1]);
            let start_y = (surf_y - 5).max(BEDROCK_FLOOR_Y);
            state.rifts[rift_idx].regress_active_column = Some((col, start_y, BEDROCK_FLOOR_Y));
        }

        let Some((col, y, min_y)) = state.rifts[rift_idx].regress_active_column else {
            break;
        };
        if y < min_y {
            state.rifts[rift_idx].regress_active_column = None;
            continue;
        }
        let pos = [col[0], y, col[1]];
        if !ow.is_loaded(host, pos) {
            state.rifts[rift_idx].regress_active_column = None;
            break;
        }
        state.rifts[rift_idx].regress_active_column = if y - 1 >= min_y {
            Some((col, y - 1, min_y))
        } else {
            None
        };

        let current = ow.block(host, pos).map(|k| k.id(host)).unwrap_or_default();
        if current.is_empty() || is_air_like(&current) {
            deep_skips += 1;
            continue;
        }
        let near_monument = near_citadel_or_bastion(state, col[0], col[1]);
        if let Some(target) = transform_restore_block(
            pos,
            &current,
            "minecraft:stone",
            "minecraft:stone",
            &deep_fallback,
            near_monument,
        ) && let Some(bstate) = resolve_cached(host, target)
            && ow
                .set_block(
                    host,
                    pos,
                    bstate,
                    BlockUpdateFlags::CLIENTS | BlockUpdateFlags::SUPPRESS_DROPS,
                )
                .unwrap_or(false)
        {
            deep_budget = deep_budget.saturating_sub(1);
            state.dirty = true;
        } else {
            deep_skips += 1;
        }
    }

    // Complete regression cleanly when both surface and deep inward rings reach center 0.
    let r = &mut state.rifts[rift_idx];
    if r.regress_radius <= 0
        && r.regress_columns.is_empty()
        && r.deepen_radius <= 0
        && r.deepen_columns.is_empty()
        && r.regress_active_column.is_none()
    {
        r.regressing = false;
        state.dirty = true;
        sync_regress_chunks(host, ow, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NetherFlavor, RiftSource};

    fn test_rift(id: u64, center: [i32; 3], citadel: bool, boss_uuid: Option<u128>) -> RiftSource {
        RiftSource {
            id,
            center,
            active_portal: true,
            contained: false,
            flavor: NetherFlavor::CrimsonForest,
            age_ticks: 60_000,
            corrupted_blocks: 700_000,
            max_radius_sq: 140 * 140,
            ring_radius: 140,
            ring_columns: Vec::new(),
            active_column: None,
            deepen_radius: 45,
            deepen_columns: Vec::new(),
            bastions_placed: 2,
            citadel_spawned: citadel,
            boss_uuid,
            pacified: false,
            regressing: false,
            regress_radius: 0,
            regress_columns: Vec::new(),
            regress_active_column: None,
        }
    }

    #[test]
    fn regression_restores_roughly_80_percent_and_preserves_scars_debris_and_drains_oceans() {
        let desert = OriginalColumnInfo {
            surface_y: 65,
            surface_block: "minecraft:sand".to_owned(),
            had_water: false,
            biome: "minecraft:desert".to_owned(),
        };
        let mut sand_restored = 0usize;
        let mut scar_kept = 0usize;
        for x in 0..200 {
            for z in 0..4 {
                match transform_restore_block(
                    [x, 65, z],
                    "minecraft:crimson_nylium",
                    "minecraft:air",
                    "minecraft:netherrack",
                    &desert,
                    false,
                ) {
                    Some("minecraft:sand") => sand_restored += 1,
                    None => scar_kept += 1,
                    other => panic!("unexpected desert restore: {other:?}"),
                }
            }
        }
        assert!(
            sand_restored >= 520 && sand_restored <= 720,
            "expected ~80% sand restored out of 800, got {sand_restored}"
        );
        assert!(
            scar_kept >= 80 && scar_kept <= 280,
            "expected ~20% Nether scars out of 800, got {scar_kept}"
        );

        // Ocean basins restore dry seabed (sand/gravel) and NEVER place water back themselves.
        let ocean = OriginalColumnInfo {
            surface_y: 45,
            surface_block: "minecraft:gravel".to_owned(),
            had_water: true,
            biome: "minecraft:ocean".to_owned(),
        };
        for x in 0..100 {
            let out = transform_restore_block(
                [x, 45, 10],
                "minecraft:netherrack",
                "minecraft:air",
                "minecraft:blackstone",
                &ocean,
                false,
            );
            assert_ne!(out, Some("minecraft:water"));
            if let Some(b) = out {
                assert!(matches!(b, "minecraft:gravel" | "minecraft:sand" | "minecraft:dirt"));
            }
        }

        // Ancient Debris & Citadel/Bastion masonry are ALWAYS preserved.
        assert_eq!(
            transform_restore_block(
                [5, -50, 5],
                "minecraft:ancient_debris",
                "minecraft:netherrack",
                "minecraft:blackstone",
                &desert,
                false,
            ),
            None
        );
        assert_eq!(
            transform_restore_block(
                [10, 72, 10],
                "minecraft:polished_blackstone_bricks",
                "minecraft:air",
                "minecraft:blackstone",
                &desert,
                true,
            ),
            None
        );
    }

    #[test]
    fn wither_boss_death_pacifies_only_its_portal_leaving_other_portals_active() {
        let mut st = RiftState::new();
        let boss_id = 0xDEAD_BEEF_1234_5678_u128;
        st.rifts.push(test_rift(1, [0, 64, 0], true, Some(boss_id)));
        let mut r2 = test_rift(2, [400, 64, 400], false, None);
        r2.corrupted_blocks = 1_200;
        r2.ring_radius = 12;
        st.rifts.push(r2);

        let matched = match_wither_boss_rift_idx(&st, Some(boss_id), &[]);
        assert_eq!(matched, Some(0));
        pacify_and_start_regression(&mut st, 0);

        assert!(st.rifts[0].pacified);
        assert!(st.rifts[0].regressing);
        assert!(!st.rifts[0].is_actively_spreading());

        // Second portal stays 100% unpacified and continues its own cycle cleanly.
        assert!(!st.rifts[1].pacified);
        assert!(!st.rifts[1].regressing);
        assert!(st.rifts[1].is_actively_spreading());

        // Overlapping guard protects Rift #2's active columns from Rift #1's cleanup.
        assert!(st.other_active_rift_covers(1, 405, 402));
        assert!(!st.other_active_rift_covers(1, 5, 5));
    }
}
