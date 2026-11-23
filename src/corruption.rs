use crate::types::{BEDROCK_FLOOR_Y, NetherFlavor, OVERWORLD_MAX_Y, spatial_hash, strip_ns};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformOutcome {
    /// Replacement block id for `pos`, if any.
    pub block: Option<&'static str>,
    /// Optional accent block placed at `pos + [0, 1, 0]` (roots, fungi, fire).
    pub above: Option<&'static str>,
    /// Optional hanging accent placed at `pos - [0, 1, 0]` (vines, shroomlight).
    pub below: Option<&'static str>,
    /// True if `pos` belongs to corrupted Nether terrain (whether transformed now
    /// or already Nether-aligned).
    pub is_corrupt: bool,
}

impl TransformOutcome {
    const fn unchanged(is_corrupt: bool) -> Self {
        Self {
            block: None,
            above: None,
            below: None,
            is_corrupt,
        }
    }

    const fn replace(block: &'static str) -> Self {
        Self {
            block: Some(block),
            above: None,
            below: None,
            is_corrupt: true,
        }
    }
}

pub fn is_air_like(name: &str) -> bool {
    matches!(strip_ns(name), "air" | "cave_air" | "void_air")
}

pub fn is_open_space(name: &str) -> bool {
    is_air_like(name)
        || matches!(
            strip_ns(name),
            "short_grass"
                | "tall_grass"
                | "fern"
                | "large_fern"
                | "dead_bush"
                | "dandelion"
                | "poppy"
                | "blue_orchid"
                | "allium"
                | "azure_bluet"
                | "red_tulip"
                | "orange_tulip"
                | "white_tulip"
                | "pink_tulip"
                | "oxeye_daisy"
                | "cornflower"
                | "lily_of_the_valley"
                | "sunflower"
                | "lilac"
                | "rose_bush"
                | "peony"
                | "snow"
                | "vine"
                | "glow_lichen"
                | "moss_carpet"
                | "pink_petals"
                | "sweet_berry_bush"
                | "lily_pad"
                | "big_dripleaf"
                | "big_dripleaf_stem"
                | "small_dripleaf"
                | "brown_mushroom"
                | "red_mushroom"
                | "sugar_cane"
                | "leaf_litter"
                | "wildflowers"
                | "bush"
                | "firefly_bush"
                | "short_dry_grass"
                | "tall_dry_grass"
                | "cactus_flower"
                | "closed_eyeblossom"
                | "open_eyeblossom"
                | "pale_moss_carpet"
                | "pale_hanging_moss"
                | "torchflower"
                | "pitcher_plant"
                | "wither_rose"
                | "hanging_roots"
                | "spore_blossom"
                | "cave_vines"
                | "cave_vines_plant"
        )
}

pub fn is_snow_or_ice(name: &str) -> bool {
    matches!(
        strip_ns(name),
        "snow"
            | "snow_block"
            | "powder_snow"
            | "ice"
            | "packed_ice"
            | "blue_ice"
            | "frosted_ice"
    )
}

pub fn is_water_like(name: &str) -> bool {
    matches!(
        strip_ns(name),
        "water"
            | "flowing_water"
            | "bubble_column"
            | "kelp"
            | "kelp_plant"
            | "seagrass"
            | "tall_seagrass"
            | "sea_pickle"
    )
}

pub fn is_protected_block(name: &str) -> bool {
    let s = strip_ns(name);
    if matches!(
        s,
        "bedrock"
            | "barrier"
            | "structure_void"
            | "structure_block"
            | "jigsaw"
            | "command_block"
            | "repeating_command_block"
            | "chain_command_block"
            | "crying_obsidian"
            | "obsidian"
            | "nether_portal"
            | "end_portal"
            | "end_portal_frame"
            | "end_gateway"
            | "respawn_anchor"
            | "lodestone"
            | "iron_door"
            | "iron_trapdoor"
            | "chest"
            | "trapped_chest"
            | "ender_chest"
            | "barrel"
            | "furnace"
            | "blast_furnace"
            | "smoker"
            | "crafting_table"
            | "enchanting_table"
            | "anvil"
            | "chipped_anvil"
            | "damaged_anvil"
            | "grindstone"
            | "smithing_table"
            | "loom"
            | "cartography_table"
            | "stonecutter"
            | "brewing_stand"
            | "beacon"
            | "spawner"
            | "trial_spawner"
            | "vault"
            | "bell"
            | "lectern"
            | "jukebox"
            | "hopper"
            | "dropper"
            | "dispenser"
            | "torch"
            | "wall_torch"
            | "soul_torch"
            | "soul_wall_torch"
            | "lantern"
            | "soul_lantern"
            | "campfire"
            | "soul_campfire"
            | "iron_bars"
            | "chain"
            | "cauldron"
            | "lava_cauldron"
            | "rail"
            | "powered_rail"
            | "detector_rail"
            | "activator_rail"
            | "reinforced_deepslate"
    ) {
        return true;
    }

    s.ends_with("_bed")
        || s.ends_with("shulker_box")
        || s.ends_with("_banner")
        || s.ends_with("_sign")
        || s.ends_with("_hanging_sign")
        || s.ends_with("_chest_boat")
}

pub fn is_preserved_ore(name: &str) -> bool {
    matches!(
        strip_ns(name),
        "diamond_ore"
            | "deepslate_diamond_ore"
            | "redstone_ore"
            | "deepslate_redstone_ore"
            | "lapis_ore"
            | "deepslate_lapis_ore"
            | "emerald_ore"
            | "deepslate_emerald_ore"
    )
}

pub fn is_nether_terrain_block(name: &str) -> bool {
    let s = strip_ns(name);
    matches!(
        s,
        "netherrack"
            | "crimson_nylium"
            | "warped_nylium"
            | "soul_sand"
            | "soul_soil"
            | "basalt"
            | "polished_basalt"
            | "smooth_basalt"
            | "blackstone"
            | "gilded_blackstone"
            | "polished_blackstone"
            | "polished_blackstone_bricks"
            | "cracked_polished_blackstone_bricks"
            | "chiseled_polished_blackstone"
            | "nether_bricks"
            | "cracked_nether_bricks"
            | "red_nether_bricks"
            | "nether_brick_fence"
            | "nether_brick_wall"
            | "nether_brick_stairs"
            | "nether_brick_slab"
            | "red_nether_brick_wall"
            | "red_nether_brick_stairs"
            | "red_nether_brick_slab"
            | "blackstone_wall"
            | "blackstone_stairs"
            | "blackstone_slab"
            | "polished_blackstone_wall"
            | "polished_blackstone_stairs"
            | "polished_blackstone_slab"
            | "polished_blackstone_button"
            | "polished_blackstone_pressure_plate"
            | "polished_blackstone_brick_wall"
            | "polished_blackstone_brick_stairs"
            | "polished_blackstone_brick_slab"
            | "magma_block"
            | "glowstone"
            | "shroomlight"
            | "crimson_stem"
            | "warped_stem"
            | "stripped_crimson_stem"
            | "stripped_warped_stem"
            | "crimson_hyphae"
            | "warped_hyphae"
            | "stripped_crimson_hyphae"
            | "stripped_warped_hyphae"
            | "crimson_planks"
            | "warped_planks"
            | "crimson_fence"
            | "warped_fence"
            | "crimson_fence_gate"
            | "warped_fence_gate"
            | "crimson_door"
            | "warped_door"
            | "crimson_trapdoor"
            | "warped_trapdoor"
            | "crimson_button"
            | "warped_button"
            | "crimson_pressure_plate"
            | "warped_pressure_plate"
            | "crimson_stairs"
            | "warped_stairs"
            | "crimson_slab"
            | "warped_slab"
            | "nether_wart_block"
            | "warped_wart_block"
            | "crimson_fungus"
            | "warped_fungus"
            | "crimson_roots"
            | "warped_roots"
            | "nether_sprouts"
            | "weeping_vines"
            | "weeping_vines_plant"
            | "twisting_vines"
            | "twisting_vines_plant"
            | "nether_quartz_ore"
            | "nether_gold_ore"
            | "ancient_debris"
            | "bone_block"
            | "lava"
            | "flowing_lava"
            | "fire"
            | "soul_fire"
    )
}

/// How many blocks below solid ground (`OceanFloor`) the outer surface expansion
/// pass converts before moving to the next column on the circular ring.
/// Penetrates deep through surface dirt and upper cave stone immediately, while the
/// dedicated deep-boring pass drives all the way down to Bedrock (`-63`).
pub const SURFACE_CRUST_DEPTH: i32 = 32;

/// Computes the vertical `[top_y, min_y]` sweep for an outer surface-expansion column.
/// Normal surface/mountain portals sweep from the column's highest block (up to `Y = 319`)
/// down through the surface crust AND any sheer cliff face (`min_neighbor_floor`) so steep
/// mountains never leave untouched stone walls behind. Only young portals lit inside a deep
/// underground cave (`portal_is_underground = true`) start centered on cave elevation before
/// breaking out to the surface.
pub fn surface_column_y_range(
    rift_center_y: i32,
    portal_is_underground: bool,
    world_surface: i32,
    ocean_floor: i32,
    min_neighbor_floor: i32,
) -> (i32, i32) {
    let ground = ocean_floor.min(world_surface);
    let top_cap = (world_surface + 2).min(OVERWORLD_MAX_Y - 1);
    if portal_is_underground {
        let top_y = (rift_center_y + 16).min(top_cap);
        let min_y = (rift_center_y - 10).max(BEDROCK_FLOOR_Y);
        (top_y, min_y)
    } else {
        let cliff_bottom = (min_neighbor_floor - 2).max(ground - 36);
        let min_y = (ground - SURFACE_CRUST_DEPTH)
            .min(cliff_bottom)
            .max(BEDROCK_FLOOR_Y);
        (top_cap, min_y)
    }
}

/// Scales a rift's per-tick block budget gently with `ring_radius` so radial outward
/// expansion and full-depth underground boring both keep a steady, urgent pace across
/// all radii — neither exploding at tiny radii nor stalling as the circle grows.
pub fn effective_rift_budget(base_budget: usize, ring_radius: i32) -> usize {
    let scaled = (8usize + (ring_radius.max(1) as usize) / 2).clamp(8, 52);
    ((scaled * base_budget.max(1) + 7) / 14).max(2)
}

/// Dedicated per-tick block budget for full-depth underground boring down to Bedrock (`-63`).
/// Keeps underground work concentrated inside 1–2 columns per tick so shader/client chunk
/// meshing stays smooth without multi-chunk hitching.
pub fn effective_deepen_budget(base_budget: usize, ring_radius: i32) -> usize {
    (effective_rift_budget(base_budget, ring_radius) * 3 + 16).clamp(32, 128)
}

/// Continuous per-tick surface corruption speed: starts at a slow eerie crawl (~0.08 blocks/tick)
/// on a newly lit portal and increases slightly with every corrupted block up to the mature cap.
pub fn rift_surface_ops_per_tick(
    base_budget: usize,
    ring_radius: i32,
    corrupted_blocks: u64,
) -> f64 {
    let scale = (base_budget.max(1) as f64) / 14.0;
    let b = corrupted_blocks as f64;
    let sqrt_ramp = (b + 25.0).sqrt() - 5.0;
    let raw = (0.08 + b * 0.00025 + sqrt_ramp * 0.085) * scale;
    let cap = if ring_radius <= 2 {
        effective_rift_budget(base_budget, ring_radius) as f64
    } else {
        (effective_rift_budget(base_budget, ring_radius) as f64 * 0.55).max(1.0)
    };
    raw.min(cap).max(0.05 * scale)
}

/// Continuous per-tick underground boring speed that accelerates alongside every corrupted block
/// up to the chunk-friendly `effective_deepen_budget` ceiling.
pub fn rift_deepen_ops_per_tick(
    base_budget: usize,
    ring_radius: i32,
    corrupted_blocks: u64,
) -> f64 {
    if ring_radius <= 2 {
        return 0.0;
    }
    let scale = (base_budget.max(1) as f64) / 14.0;
    let b = corrupted_blocks as f64;
    let sqrt_ramp = (b + 25.0).sqrt() - 5.0;
    let raw = (0.08 + b * 0.00025 + sqrt_ramp * 0.085) * scale * 3.4;
    let max_deep = effective_deepen_budget(base_budget, ring_radius) as f64;
    raw.min(max_deep).max(0.10 * scale)
}

/// Computes how deep the underground deepening pass can sink at horizontal
/// distance `horiz_dist` from a rift at `center_y`. Immediately penetrates shallow
/// caves underneath, sinks steadily in a cone over time, and reaches Bedrock (`-63`)
/// as the rift matures.
#[cfg(test)]
pub fn allowed_min_y(center_y: i32, age_ticks: u64, horiz_dist: i32, corrupted_blocks: u64) -> i32 {
    if corrupted_blocks >= 75_000 || age_ticks >= 9_000 {
        return BEDROCK_FLOOR_Y;
    }
    let age_sink = (age_ticks / 45).min(150) as i32;
    let block_sink = (corrupted_blocks / 600).min(150) as i32;
    let sink_bonus = age_sink.max(block_sink);
    let cone_rise = (horiz_dist / 3).min(40);
    (center_y - 24 - sink_bonus + cone_rise).clamp(BEDROCK_FLOOR_Y, OVERWORLD_MAX_Y - 8)
}

fn scar_roll(pos: [i32; 3], salt: u64) -> u64 {
    spatial_hash(pos[0], pos[1], pos[2], salt)
}

/// Groups surviving Overworld remnants (~`pct`%) into organic 2x2x2 weathered
/// pockets rather than isolated voxels while keeping ~88-90% corrupted.
fn in_remnant_cluster(pos: [i32; 3], salt: u64, pct: u64) -> bool {
    let pocket = spatial_hash(pos[0] >> 1, pos[1] >> 1, pos[2] >> 1, salt) % 100;
    let fine = scar_roll(pos, salt ^ 0x9E37_79B9_7F4A_7C15) % 100;
    pocket < (pct * 2).min(95) && fine < 50
}

fn flavor_surface_block(flavor: NetherFlavor, h: u64) -> &'static str {
    match flavor {
        NetherFlavor::CrimsonForest => {
            if h % 10 < 8 {
                "minecraft:crimson_nylium"
            } else {
                "minecraft:netherrack"
            }
        }
        NetherFlavor::WarpedForest => {
            if h % 10 < 8 {
                "minecraft:warped_nylium"
            } else {
                "minecraft:netherrack"
            }
        }
        NetherFlavor::SoulSandValley => {
            if h % 10 < 6 {
                "minecraft:soul_sand"
            } else {
                "minecraft:soul_soil"
            }
        }
        NetherFlavor::BasaltDeltas => match h % 10 {
            0..=5 => "minecraft:basalt",
            6..=8 => "minecraft:blackstone",
            _ => "minecraft:magma_block",
        },
        NetherFlavor::NetherWastes => match h % 12 {
            0..=9 => "minecraft:netherrack",
            10 => "minecraft:soul_sand",
            _ => "minecraft:magma_block",
        },
    }
}

/// Vanilla Nether flora (`crimson_roots`, `warped_roots`, `nether_sprouts`,
/// `crimson_fungus`, `warped_fungus`) only stays planted on nylium or soul soil —
/// placing them on `netherrack`, `soul_sand`, `basalt`, or `blackstone` pops them
/// off as dropped items on the next neighbor update.
pub fn can_support_nether_flora(surface_block: &str) -> bool {
    matches!(
        strip_ns(surface_block),
        "crimson_nylium" | "warped_nylium" | "soul_soil"
    )
}

fn flavor_surface_flora(flavor: NetherFlavor, surface_block: &str, h: u64) -> Option<&'static str> {
    let roll = h % 100;
    if can_support_nether_flora(surface_block) {
        match (flavor, roll) {
            (NetherFlavor::CrimsonForest, 0..=13) => return Some("minecraft:crimson_roots"),
            (NetherFlavor::CrimsonForest, 14..=17) => return Some("minecraft:crimson_fungus"),
            (NetherFlavor::WarpedForest, 0..=12) => return Some("minecraft:warped_roots"),
            (NetherFlavor::WarpedForest, 13..=17) => return Some("minecraft:nether_sprouts"),
            (NetherFlavor::WarpedForest, 18..=21) => return Some("minecraft:warped_fungus"),
            _ => {}
        }
    }

    match (flavor, strip_ns(surface_block), roll) {
        (NetherFlavor::SoulSandValley, "soul_sand" | "soul_soil", 0..=3) => {
            Some("minecraft:soul_fire")
        }
        (NetherFlavor::CrimsonForest, "netherrack" | "magma_block", 0..=2) => {
            Some("minecraft:fire")
        }
        (NetherFlavor::BasaltDeltas, "magma_block", 0..=3) => Some("minecraft:fire"),
        (NetherFlavor::NetherWastes, "netherrack" | "magma_block", 0..=2) => Some("minecraft:fire"),
        _ => None,
    }
}

fn is_burnable_overworld_growth(short: &str) -> bool {
    if short.ends_with("_sapling") {
        return true;
    }
    if (short.ends_with("_coral")
        || short.ends_with("_coral_fan")
        || short.ends_with("_coral_wall_fan"))
        && !short.starts_with("dead_")
    {
        return true;
    }
    matches!(
        short,
        "mangrove_propagule"
            | "bamboo"
            | "bamboo_sapling"
            | "cactus"
            | "wheat"
            | "carrots"
            | "potatoes"
            | "beetroots"
            | "melon_stem"
            | "pumpkin_stem"
            | "attached_melon_stem"
            | "attached_pumpkin_stem"
            | "torchflower_crop"
            | "pitcher_crop"
            | "cocoa"
            | "pumpkin"
            | "melon"
            | "hay_block"
            | "dried_kelp_block"
            | "frogspawn"
            | "chorus_plant"
            | "chorus_flower"
            | "sculk_vein"
            | "small_amethyst_bud"
            | "medium_amethyst_bud"
            | "large_amethyst_bud"
            | "amethyst_cluster"
    )
}

fn is_overworld_wood_prefix(prefix: &str) -> bool {
    let base = prefix.strip_prefix("stripped_").unwrap_or(prefix);
    matches!(
        base,
        "oak"
            | "spruce"
            | "birch"
            | "jungle"
            | "acacia"
            | "dark_oak"
            | "mangrove"
            | "cherry"
            | "pale_oak"
            | "bamboo"
            | "bamboo_mosaic"
    )
}

fn is_overworld_masonry_prefix(prefix: &str) -> bool {
    matches!(
        prefix,
        "stone"
            | "smooth_stone"
            | "cobblestone"
            | "mossy_cobblestone"
            | "stone_brick"
            | "mossy_stone_brick"
            | "andesite"
            | "polished_andesite"
            | "diorite"
            | "polished_diorite"
            | "granite"
            | "polished_granite"
            | "deepslate"
            | "cobbled_deepslate"
            | "polished_deepslate"
            | "deepslate_brick"
            | "deepslate_tile"
            | "tuff"
            | "polished_tuff"
            | "tuff_brick"
            | "sandstone"
            | "cut_sandstone"
            | "smooth_sandstone"
            | "red_sandstone"
            | "cut_red_sandstone"
            | "smooth_red_sandstone"
            | "brick"
            | "mud_brick"
            | "prismarine"
            | "prismarine_brick"
            | "dark_prismarine"
    )
}

fn pick_warped_wood_replacement(
    crimson_id: &'static str,
    warped_id: &'static str,
    dark_id: &'static str,
    allow_burn: bool,
    flavor: NetherFlavor,
    h: u64,
) -> &'static str {
    let roll = h % 20;
    if allow_burn && roll == 19 {
        return "minecraft:air";
    }
    match flavor {
        NetherFlavor::WarpedForest => {
            if allow_burn && roll == 18 {
                dark_id
            } else {
                warped_id
            }
        }
        NetherFlavor::SoulSandValley | NetherFlavor::BasaltDeltas => match roll {
            0..=10 => dark_id,
            11..=17 => crimson_id,
            _ if allow_burn => "minecraft:air",
            _ => dark_id,
        },
        NetherFlavor::CrimsonForest | NetherFlavor::NetherWastes => match roll {
            0..=15 => crimson_id,
            16..=17 => dark_id,
            _ if allow_burn => "minecraft:air",
            _ => dark_id,
        },
    }
}

fn transform_wood_and_foliage(
    short: &str,
    flavor: NetherFlavor,
    h: u64,
) -> Option<TransformOutcome> {
    let is_stripped_overworld_log = short
        .strip_suffix("_log")
        .or_else(|| short.strip_suffix("_wood"))
        .is_some_and(|p| p.starts_with("stripped_") && is_overworld_wood_prefix(p))
        || short == "stripped_bamboo_block";

    let is_overworld_log = ((short.ends_with("_log") || short.ends_with("_wood"))
        && !short.starts_with("crimson_")
        && !short.starts_with("warped_")
        && !short.starts_with("stripped_crimson_")
        && !short.starts_with("stripped_warped_"))
        || matches!(
            short,
            "mangrove_roots" | "bamboo_block" | "stripped_bamboo_block" | "mushroom_stem"
        );
    if is_overworld_log {
        // Wood always burns, rots away, or warps into Nether stems / charred basalt — never stays Overworld wood.
        let (crimson_stem, warped_stem) = if is_stripped_overworld_log {
            if short.ends_with("_wood") {
                (
                    "minecraft:stripped_crimson_hyphae",
                    "minecraft:stripped_warped_hyphae",
                )
            } else {
                (
                    "minecraft:stripped_crimson_stem",
                    "minecraft:stripped_warped_stem",
                )
            }
        } else if short.ends_with("_wood") {
            ("minecraft:crimson_hyphae", "minecraft:warped_hyphae")
        } else {
            ("minecraft:crimson_stem", "minecraft:warped_stem")
        };

        let roll = h % 20;
        let replacement = match flavor {
            NetherFlavor::CrimsonForest => match roll {
                0..=11 => crimson_stem,
                12..=16 => "minecraft:basalt",
                _ => "minecraft:air",
            },
            NetherFlavor::WarpedForest => match roll {
                0..=11 => warped_stem,
                12..=16 => "minecraft:basalt",
                _ => "minecraft:air",
            },
            NetherFlavor::SoulSandValley | NetherFlavor::BasaltDeltas => match roll {
                0..=11 => "minecraft:basalt",
                12..=14 => "minecraft:blackstone",
                _ => "minecraft:air",
            },
            NetherFlavor::NetherWastes => match roll {
                0..=4 => crimson_stem,
                5..=12 => "minecraft:basalt",
                _ => "minecraft:air",
            },
        };
        return Some(TransformOutcome::replace(replacement));
    }

    if (short.ends_with("_planks")
        || short == "bamboo_mosaic")
        && !short.starts_with("crimson_")
        && !short.starts_with("warped_")
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_planks",
            "minecraft:warped_planks",
            "minecraft:blackstone",
            true,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_stairs")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_stairs",
            "minecraft:warped_stairs",
            "minecraft:blackstone_stairs",
            true,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_slab")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_slab",
            "minecraft:warped_slab",
            "minecraft:blackstone_slab",
            true,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_fence_gate")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_fence_gate",
            "minecraft:warped_fence_gate",
            "minecraft:crimson_fence_gate",
            true,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_fence")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_fence",
            "minecraft:warped_fence",
            "minecraft:nether_brick_fence",
            false,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_trapdoor")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_trapdoor",
            "minecraft:warped_trapdoor",
            "minecraft:crimson_trapdoor",
            true,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_door")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(if flavor == NetherFlavor::WarpedForest {
            "minecraft:warped_door"
        } else {
            "minecraft:crimson_door"
        }));
    }

    if let Some(prefix) = short.strip_suffix("_button")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_button",
            "minecraft:warped_button",
            "minecraft:polished_blackstone_button",
            false,
            flavor,
            h,
        )));
    }

    if let Some(prefix) = short.strip_suffix("_pressure_plate")
        && is_overworld_wood_prefix(prefix)
    {
        return Some(TransformOutcome::replace(pick_warped_wood_replacement(
            "minecraft:crimson_pressure_plate",
            "minecraft:warped_pressure_plate",
            "minecraft:polished_blackstone_pressure_plate",
            false,
            flavor,
            h,
        )));
    }

    if short.ends_with("_leaves")
        || matches!(
            short,
            "azalea" | "flowering_azalea" | "red_mushroom_block" | "brown_mushroom_block"
        )
    {
        let replacement = match (flavor, h % 100) {
            (_, 0..=3) => "minecraft:shroomlight",
            (NetherFlavor::CrimsonForest, 4..=51) => "minecraft:nether_wart_block",
            (NetherFlavor::WarpedForest, 4..=51) => "minecraft:warped_wart_block",
            (NetherFlavor::SoulSandValley | NetherFlavor::BasaltDeltas, 4..=16) => {
                "minecraft:basalt"
            }
            (NetherFlavor::NetherWastes, 4..=28) => "minecraft:nether_wart_block",
            _ => "minecraft:air",
        };
        return Some(TransformOutcome::replace(replacement));
    }

    None
}

fn transform_stone_family(
    short: &str,
    pos: [i32; 3],
    below: &str,
    flavor: NetherFlavor,
    rift_mature: bool,
    h: u64,
) -> Option<TransformOutcome> {
    if short == "raw_iron_block" {
        return Some(TransformOutcome::replace("minecraft:nether_gold_ore"));
    }
    if short == "raw_copper_block" {
        return Some(TransformOutcome::replace("minecraft:nether_quartz_ore"));
    }
    if matches!(short, "amethyst_block" | "budding_amethyst") {
        return Some(TransformOutcome::replace(if h % 5 == 0 {
            "minecraft:crying_obsidian"
        } else {
            "minecraft:blackstone"
        }));
    }

    let deep_rock = matches!(
        short,
        "deepslate"
            | "cobbled_deepslate"
            | "polished_deepslate"
            | "chiseled_deepslate"
            | "cracked_deepslate_bricks"
            | "cracked_deepslate_tiles"
            | "infested_deepslate"
            | "tuff"
            | "polished_tuff"
            | "tuff_bricks"
            | "chiseled_tuff"
            | "chiseled_tuff_bricks"
            | "sculk"
            | "sculk_catalyst"
            | "sculk_sensor"
            | "calibrated_sculk_sensor"
            | "sculk_shrieker"
    );
    let normal_rock = matches!(
        short,
        "stone"
            | "smooth_stone"
            | "infested_stone"
            | "infested_cobblestone"
            | "infested_stone_bricks"
            | "granite"
            | "polished_granite"
            | "diorite"
            | "polished_diorite"
            | "andesite"
            | "polished_andesite"
            | "calcite"
            | "dripstone_block"
            | "pointed_dripstone"
            | "sandstone"
            | "cut_sandstone"
            | "chiseled_sandstone"
            | "smooth_sandstone"
            | "red_sandstone"
            | "cut_red_sandstone"
            | "chiseled_red_sandstone"
            | "smooth_red_sandstone"
            | "bricks"
            | "mud_bricks"
            | "packed_mud"
            | "terracotta"
    ) || short.ends_with("_terracotta");

    if !deep_rock && !normal_rock {
        return None;
    }

    // Overwhelmingly corrupts surface, cliff, and subterranean rock (~95-98%), leaving only
    // rare 2-5% weathered Overworld rock traces underground so caves & shafts clearly burn Nether.
    let scar_pct = if pos[1] >= 36 {
        2
    } else if pos[1] >= 0 {
        4
    } else {
        5
    };
    if in_remnant_cluster(pos, 0x5CA2_1001, scar_pct) {
        if short == "stone" && h % 2 == 0 {
            return Some(TransformOutcome::replace("minecraft:cobblestone"));
        }
        return Some(TransformOutcome::unchanged(true));
    }

    // Deep Ancient Debris in mature rifts across diamond depth (`-16..-60`), richest deep down.
    if rift_mature && pos[1] <= -16 && pos[1] >= -60 {
        let debris_roll = scar_roll(pos, 0xDE82_1500);
        let odds = if pos[1] <= -46 {
            160
        } else if pos[1] <= -32 {
            260
        } else {
            450
        };
        if debris_roll % odds == 0 {
            return Some(TransformOutcome::replace("minecraft:ancient_debris"));
        }
    }

    // Nether ores embedded within corrupted rock.
    if (h % 145) == 0 {
        return Some(TransformOutcome::replace("minecraft:nether_quartz_ore"));
    }
    if (h % 210) == 1 {
        return Some(TransformOutcome::replace("minecraft:nether_gold_ore"));
    }

    // Occasional ceiling lighting and Nether vines above caves — NEVER floods caves with lava!
    let mut below_accent = None;
    if is_air_like(below) {
        let ceil_roll = scar_roll(pos, 0xCE11_0001) % 180;
        below_accent = match (flavor, ceil_roll) {
            (_, 0) => Some("minecraft:glowstone"),
            (NetherFlavor::CrimsonForest, 1..=3) => Some("minecraft:weeping_vines"),
            (NetherFlavor::WarpedForest, 1) => Some("minecraft:shroomlight"),
            _ => None,
        };
    }

    let chosen = if deep_rock || pos[1] < 4 {
        match flavor {
            NetherFlavor::BasaltDeltas if (h % 20) < 12 => "minecraft:basalt",
            NetherFlavor::BasaltDeltas => "minecraft:blackstone",
            NetherFlavor::SoulSandValley if (h % 20) < 8 => "minecraft:soul_soil",
            NetherFlavor::SoulSandValley => "minecraft:blackstone",
            _ if (h % 20) <= 10 => "minecraft:blackstone",
            _ if (h % 20) <= 15 => "minecraft:basalt",
            _ => "minecraft:netherrack",
        }
    } else if pos[1] < 24 {
        match flavor {
            NetherFlavor::BasaltDeltas if (h % 20) < 12 => "minecraft:basalt",
            NetherFlavor::BasaltDeltas => "minecraft:blackstone",
            NetherFlavor::SoulSandValley if (h % 20) < 6 => "minecraft:soul_soil",
            NetherFlavor::SoulSandValley if (h % 20) < 14 => "minecraft:blackstone",
            NetherFlavor::SoulSandValley => "minecraft:netherrack",
            _ if (h % 20) <= 6 => "minecraft:blackstone",
            _ if (h % 20) <= 9 => "minecraft:basalt",
            _ if (h % 20) <= 18 => "minecraft:netherrack",
            _ if is_air_like(below) => "minecraft:netherrack",
            _ => "minecraft:magma_block",
        }
    } else {
        match flavor {
            NetherFlavor::BasaltDeltas if (h % 20) <= 10 => "minecraft:basalt",
            NetherFlavor::BasaltDeltas if (h % 20) <= 18 => "minecraft:blackstone",
            NetherFlavor::BasaltDeltas => "minecraft:netherrack",
            NetherFlavor::SoulSandValley if (h % 20) <= 5 => "minecraft:soul_soil",
            NetherFlavor::SoulSandValley if (h % 20) <= 11 => "minecraft:blackstone",
            NetherFlavor::SoulSandValley => "minecraft:netherrack",
            _ if (h % 20) <= 14 => "minecraft:netherrack",
            _ if (h % 20) <= 17 => "minecraft:blackstone",
            _ if (h % 20) == 18 => "minecraft:basalt",
            _ => {
                // Only embed safe magma crust if not directly dangling over cave air.
                if is_air_like(below) {
                    "minecraft:netherrack"
                } else {
                    "minecraft:magma_block"
                }
            }
        }
    };

    Some(TransformOutcome {
        block: Some(chosen),
        above: None,
        below: below_accent,
        is_corrupt: true,
    })
}

/// Pure deterministic block corruption selector used by runtime frontier and unit tests.
/// Never floods caves with lava, protects player chests/beds/containment/bedrock,
/// preserves rare Overworld ores, leaves ~12% scarred Overworld remnants, and seeds
/// deep Ancient Debris in mature rifts.
pub fn transform_block(
    pos: [i32; 3],
    current: &str,
    above: &str,
    below: &str,
    flavor: NetherFlavor,
    rift_mature: bool,
) -> TransformOutcome {
    if is_protected_block(current) || is_preserved_ore(current) {
        return TransformOutcome::unchanged(false);
    }

    if is_air_like(current) {
        return TransformOutcome::unchanged(false);
    }

    if is_nether_terrain_block(current) {
        return TransformOutcome::unchanged(true);
    }

    let short = strip_ns(current);
    let h = spatial_hash(pos[0], pos[1], pos[2], 0xB13E_D00D);

    // All water and submerged water plants vanish completely into air.
    if is_water_like(current) {
        return TransformOutcome::replace("minecraft:air");
    }

    // All snow (including powder snow and snow blocks) and ice melt completely away.
    if is_snow_or_ice(current) {
        return TransformOutcome::replace("minecraft:air");
    }

    // Soft Overworld foliage / flowers / vines / snow / crops / saplings / wet blocks ->
    // rot, burn, or dry cleanly away with zero green or cold survivors.
    if is_open_space(current) || is_burnable_overworld_growth(short) {
        return TransformOutcome::replace("minecraft:air");
    }

    if short == "wet_sponge" {
        return TransformOutcome::replace("minecraft:sponge");
    }

    if short.ends_with("_coral_block") && !short.starts_with("dead_") {
        return TransformOutcome::replace(if h % 2 == 0 {
            "minecraft:dead_brain_coral_block"
        } else {
            "minecraft:blackstone"
        });
    }

    if let Some(wood_out) = transform_wood_and_foliage(short, flavor, h) {
        return wood_out;
    }

    // Living green / wet surface covers (grass, moss, mycelium, podzol, mud, clay) ALWAYS corrupt —
    // never keep green or muddy patches! Only plain dry earth keeps rare (~2%) scorched dirt traces.
    if matches!(
        short,
        "grass_block"
            | "moss_block"
            | "pale_moss_block"
            | "mycelium"
            | "podzol"
            | "mud"
            | "muddy_mangrove_roots"
            | "clay"
    ) {
        if is_open_space(above) && !matches!(short, "mud" | "muddy_mangrove_roots" | "clay") {
            let block = flavor_surface_block(flavor, h);
            let flora = if is_air_like(above) {
                flavor_surface_flora(flavor, block, scar_roll(pos, 0xF102_A001))
            } else {
                None
            };
            return TransformOutcome {
                block: Some(block),
                above: flora,
                below: None,
                is_corrupt: true,
            };
        }
        return TransformOutcome::replace(match flavor {
            NetherFlavor::SoulSandValley => "minecraft:soul_soil",
            NetherFlavor::BasaltDeltas => "minecraft:blackstone",
            _ => "minecraft:netherrack",
        });
    }

    if matches!(short, "dirt" | "coarse_dirt" | "rooted_dirt") {
        if in_remnant_cluster(pos, 0x5CA2_2002, 2) {
            return TransformOutcome::replace("minecraft:coarse_dirt");
        }
        if is_open_space(above) {
            let block = flavor_surface_block(flavor, h);
            let flora = if is_air_like(above) {
                flavor_surface_flora(flavor, block, scar_roll(pos, 0xF102_A001))
            } else {
                None
            };
            return TransformOutcome {
                block: Some(block),
                above: flora,
                below: None,
                is_corrupt: true,
            };
        }
        return TransformOutcome::replace(if flavor == NetherFlavor::SoulSandValley {
            "minecraft:soul_soil"
        } else {
            "minecraft:netherrack"
        });
    }

    // Shovel-flattened village/trail paths (`dirt_path`): corrupt the majority (~76%) into
    // scarred hell-tracks while leaving ~24% flattened path traces behind.
    if short == "dirt_path" {
        if in_remnant_cluster(pos, 0x5CA2_7A74, 24) {
            return TransformOutcome::unchanged(true);
        }
        let block = match flavor {
            NetherFlavor::SoulSandValley => {
                if h % 4 == 0 {
                    "minecraft:cracked_polished_blackstone_bricks"
                } else {
                    "minecraft:soul_soil"
                }
            }
            NetherFlavor::BasaltDeltas => {
                if h % 2 == 0 {
                    "minecraft:basalt"
                } else {
                    "minecraft:blackstone"
                }
            }
            NetherFlavor::WarpedForest => match h % 6 {
                0..=2 => "minecraft:netherrack",
                3..=4 => "minecraft:soul_soil",
                _ => "minecraft:warped_nylium",
            },
            NetherFlavor::CrimsonForest | NetherFlavor::NetherWastes => match h % 6 {
                0..=2 => "minecraft:netherrack",
                3 => "minecraft:soul_soil",
                4 => "minecraft:cracked_polished_blackstone_bricks",
                _ => "minecraft:crimson_nylium",
            },
        };
        return TransformOutcome::replace(block);
    }

    // Wet cultivated village farmland always rots/dries away completely.
    if short == "farmland" {
        return TransformOutcome::replace(match flavor {
            NetherFlavor::SoulSandValley | NetherFlavor::BasaltDeltas => "minecraft:soul_soil",
            _ if h % 3 == 0 => "minecraft:soul_soil",
            _ => "minecraft:netherrack",
        });
    }

    if matches!(short, "sand" | "red_sand" | "gravel" | "suspicious_sand" | "suspicious_gravel") {
        if in_remnant_cluster(pos, 0x5CA2_3003, 2) {
            return TransformOutcome::unchanged(true);
        }
        let block = match flavor {
            NetherFlavor::BasaltDeltas => "minecraft:blackstone",
            NetherFlavor::CrimsonForest | NetherFlavor::WarpedForest => "minecraft:netherrack",
            NetherFlavor::SoulSandValley | NetherFlavor::NetherWastes => "minecraft:soul_sand",
        };
        return TransformOutcome::replace(block);
    }

    // Overworld common ores (90% converted into Nether ores, tiny fraction left scarred).
    if matches!(short, "coal_ore" | "deepslate_coal_ore" | "copper_ore" | "deepslate_copper_ore") {
        if h % 10 != 0 {
            return TransformOutcome::replace("minecraft:nether_quartz_ore");
        }
        return TransformOutcome::unchanged(true);
    }
    if matches!(short, "iron_ore" | "deepslate_iron_ore" | "gold_ore" | "deepslate_gold_ore") {
        if h % 10 != 0 {
            return TransformOutcome::replace("minecraft:nether_gold_ore");
        }
        return TransformOutcome::unchanged(true);
    }

    // Built Overworld stonework & village masonry (full blocks, stairs, slabs, walls).
    if matches!(
        short,
        "cobblestone"
            | "mossy_cobblestone"
            | "stone_bricks"
            | "mossy_stone_bricks"
            | "cracked_stone_bricks"
            | "chiseled_stone_bricks"
            | "deepslate_bricks"
            | "deepslate_tiles"
    ) {
        if in_remnant_cluster(pos, 0x5CA2_4004, 2) {
            return TransformOutcome::replace("minecraft:cracked_stone_bricks");
        }
        let block = match h % 24 {
            0 => "minecraft:gilded_blackstone",
            1..=7 => "minecraft:cracked_polished_blackstone_bricks",
            8..=15 => "minecraft:polished_blackstone_bricks",
            _ => "minecraft:blackstone",
        };
        return TransformOutcome::replace(block);
    }

    if let Some(prefix) = short.strip_suffix("_stairs")
        && is_overworld_masonry_prefix(prefix)
    {
        if in_remnant_cluster(pos, 0x5CA2_4004, 2) {
            return TransformOutcome::unchanged(true);
        }
        let block = match h % 12 {
            0..=4 => "minecraft:blackstone_stairs",
            5..=9 => "minecraft:polished_blackstone_brick_stairs",
            _ => "minecraft:nether_brick_stairs",
        };
        return TransformOutcome::replace(block);
    }

    if let Some(prefix) = short.strip_suffix("_slab")
        && is_overworld_masonry_prefix(prefix)
    {
        if in_remnant_cluster(pos, 0x5CA2_4004, 2) {
            return TransformOutcome::unchanged(true);
        }
        let block = match h % 12 {
            0..=4 => "minecraft:blackstone_slab",
            5..=9 => "minecraft:polished_blackstone_brick_slab",
            _ => "minecraft:nether_brick_slab",
        };
        return TransformOutcome::replace(block);
    }

    if let Some(prefix) = short.strip_suffix("_wall")
        && is_overworld_masonry_prefix(prefix)
    {
        if in_remnant_cluster(pos, 0x5CA2_4004, 2) {
            return TransformOutcome::unchanged(true);
        }
        let block = match h % 12 {
            0..=4 => "minecraft:blackstone_wall",
            5..=9 => "minecraft:polished_blackstone_brick_wall",
            _ => "minecraft:nether_brick_wall",
        };
        return TransformOutcome::replace(block);
    }

    if let Some(stone_out) = transform_stone_family(short, pos, below, flavor, rift_mature, h) {
        return stone_out;
    }

    TransformOutcome::unchanged(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_and_rare_ores_are_never_overwritten() {
        for block in [
            "minecraft:bedrock",
            "minecraft:crying_obsidian",
            "minecraft:obsidian",
            "minecraft:nether_portal",
            "minecraft:chest",
            "minecraft:red_bed",
            "minecraft:iron_door",
            "minecraft:diamond_ore",
            "minecraft:deepslate_diamond_ore",
            "minecraft:redstone_ore",
            "minecraft:lapis_ore",
            "minecraft:emerald_ore",
        ] {
            let out = transform_block(
                [12, -50, 24],
                block,
                "minecraft:air",
                "minecraft:stone",
                NetherFlavor::CrimsonForest,
                true,
            );
            assert_eq!(out.block, None, "{block} must not be overwritten");
        }
    }

    #[test]
    fn water_evaporates_and_caves_never_flood_with_lava() {
        for block in [
            "minecraft:water",
            "minecraft:flowing_water",
            "minecraft:bubble_column",
            "minecraft:kelp",
            "minecraft:seagrass",
            "minecraft:sea_pickle",
            "minecraft:ice",
        ] {
            for y in [40, 62] {
                let out = transform_block(
                    [4, y, 9],
                    block,
                    "minecraft:air",
                    "minecraft:stone",
                    NetherFlavor::NetherWastes,
                    true,
                );
                assert_eq!(out.block, Some("minecraft:air"), "{block} at y={y} must vanish into air");
            }
        }

        for x in 0..100 {
            let air = transform_block(
                [x, -20, x],
                "minecraft:cave_air",
                "minecraft:deepslate",
                "minecraft:deepslate",
                NetherFlavor::BasaltDeltas,
                true,
            );
            assert_eq!(air.block, None, "cave air must remain air");
            let ceiling = transform_block(
                [x, -15, x * 2],
                "minecraft:stone",
                "minecraft:stone",
                "minecraft:cave_air",
                NetherFlavor::CrimsonForest,
                true,
            );
            assert_ne!(ceiling.block, Some("minecraft:lava"));
            assert_ne!(ceiling.below, Some("minecraft:lava"));
        }
    }

    #[test]
    fn ancient_debris_only_forms_deep_and_in_mature_rifts() {
        let mut found_deep_mature = false;
        for x in 0..800 {
            let shallow = transform_block(
                [x, 20, x * 3],
                "minecraft:stone",
                "minecraft:stone",
                "minecraft:stone",
                NetherFlavor::CrimsonForest,
                true,
            );
            assert_ne!(shallow.block, Some("minecraft:ancient_debris"));

            let immature = transform_block(
                [x, -52, x * 3],
                "minecraft:deepslate",
                "minecraft:deepslate",
                "minecraft:deepslate",
                NetherFlavor::CrimsonForest,
                false,
            );
            assert_ne!(immature.block, Some("minecraft:ancient_debris"));

            let deep = transform_block(
                [x, -52, x * 3],
                "minecraft:deepslate",
                "minecraft:deepslate",
                "minecraft:deepslate",
                NetherFlavor::CrimsonForest,
                true,
            );
            if deep.block == Some("minecraft:ancient_debris") {
                found_deep_mature = true;
            }
        }
        assert!(found_deep_mature, "mature deep deepslate should occasionally yield ancient debris");

        // Diamond-level stone/deepslate (-24) in mature rifts also seeds occasional Ancient Debris
        // and dedicated deep-boring budget stays well above surface budget for vertical volume.
        let found_diamond_tier = (0..1800).any(|x| {
            transform_block(
                [x, -24, x * 5],
                "minecraft:deepslate",
                "minecraft:deepslate",
                "minecraft:deepslate",
                NetherFlavor::NetherWastes,
                true,
            )
            .block
                == Some("minecraft:ancient_debris")
        });
        assert!(found_diamond_tier, "diamond-level mature deepslate should yield ancient debris");
        assert!(effective_deepen_budget(14, 60) > effective_rift_budget(14, 60));
    }

    #[test]
    fn leaves_overworld_scar_patches() {
        let mut kept_or_cobble = 0;
        let mut warped = 0;
        for x in 0..300 {
            let out = transform_block(
                [x, 60, x + 7],
                "minecraft:stone",
                "minecraft:air",
                "minecraft:stone",
                NetherFlavor::CrimsonForest,
                false,
            );
            match out.block {
                None | Some("minecraft:cobblestone") => kept_or_cobble += 1,
                Some(_) => warped += 1,
            }
        }
        assert!(kept_or_cobble >= 2 && kept_or_cobble <= 30 && warped > 260);
    }

    #[test]
    fn mountain_and_cave_stone_variants_convert_cleanly() {
        for block in [
            "minecraft:stone",
            "minecraft:infested_stone",
            "minecraft:deepslate",
            "minecraft:infested_deepslate",
            "minecraft:calcite",
            "minecraft:tuff",
            "minecraft:granite",
            "minecraft:diorite",
            "minecraft:andesite",
            "minecraft:dripstone_block",
            "minecraft:pointed_dripstone",
            "minecraft:packed_mud",
        ] {
            let mut converted = 0;
            for x in 0..40 {
                let out = transform_block(
                    [x, 195, x * 2],
                    block,
                    "minecraft:air",
                    "minecraft:stone",
                    NetherFlavor::SoulSandValley,
                    true,
                );
                if out.block.is_some() {
                    converted += 1;
                }
            }
            assert!(
                converted >= 32,
                "{block} should overwhelmingly convert even at high mountain Y=195 (got {converted}/40)"
            );
        }
        // Deep Dark & amethyst cave blocks convert cleanly near diamond level too.
        for cave_block in [
            "minecraft:sculk",
            "minecraft:sculk_catalyst",
            "minecraft:amethyst_block",
            "minecraft:budding_amethyst",
            "minecraft:raw_iron_block",
            "minecraft:raw_copper_block",
        ] {
            let mut conv = 0;
            for x in 0..200 {
                let out = transform_block(
                    [x, -45, x * 3],
                    cave_block,
                    "minecraft:air",
                    "minecraft:deepslate",
                    NetherFlavor::CrimsonForest,
                    true,
                );
                if out.block.is_some() {
                    conv += 1;
                }
            }
            assert!(
                conv >= 150,
                "{cave_block} should convert heavily underground while leaving rare traces (got {conv}/200)"
            );
        }
    }

    #[test]
    fn snow_powder_snow_and_ice_all_melt_into_air() {
        for block in [
            "minecraft:snow",
            "minecraft:snow_block",
            "minecraft:powder_snow",
            "minecraft:ice",
            "minecraft:packed_ice",
            "minecraft:blue_ice",
            "minecraft:frosted_ice",
        ] {
            let out = transform_block(
                [10, 150, 20],
                block,
                "minecraft:air",
                "minecraft:stone",
                NetherFlavor::CrimsonForest,
                true,
            );
            assert_eq!(out.block, Some("minecraft:air"), "{block} must melt to air");
        }
    }

    #[test]
    fn grass_wood_wet_and_living_plants_always_corrupt_or_burn() {
        for flavor in [
            NetherFlavor::CrimsonForest,
            NetherFlavor::WarpedForest,
            NetherFlavor::SoulSandValley,
            NetherFlavor::BasaltDeltas,
            NetherFlavor::NetherWastes,
        ] {
            for x in 0..400 {
                let grass = transform_block(
                    [x, 70, x * 3],
                    "minecraft:grass_block",
                    "minecraft:air",
                    "minecraft:dirt",
                    flavor,
                    true,
                );
                assert!(grass.block.is_some(), "grass_block must never remain uncorrupted grass");

                let moss = transform_block(
                    [x, 70, x * 5],
                    "minecraft:moss_block",
                    "minecraft:air",
                    "minecraft:stone",
                    flavor,
                    true,
                );
                assert!(moss.block.is_some(), "moss_block must never remain uncorrupted");

                let oak = transform_block(
                    [x, 75, x * 7],
                    "minecraft:oak_log",
                    "minecraft:oak_leaves",
                    "minecraft:dirt",
                    flavor,
                    true,
                );
                assert!(
                    oak.block.is_some() && oak.block != Some("minecraft:stripped_oak_log"),
                    "Overworld wood must burn, rot, or warp into Nether material (got {:?})",
                    oak.block
                );
            }
        }

        for dry_or_burn in [
            ("minecraft:wet_sponge", "minecraft:sponge"),
            ("minecraft:wheat", "minecraft:air"),
            ("minecraft:oak_sapling", "minecraft:air"),
            ("minecraft:bamboo", "minecraft:air"),
            ("minecraft:cactus", "minecraft:air"),
            ("minecraft:hay_block", "minecraft:air"),
            ("minecraft:dried_kelp_block", "minecraft:air"),
            ("minecraft:frogspawn", "minecraft:air"),
        ] {
            let out = transform_block(
                [14, 65, -22],
                dry_or_burn.0,
                "minecraft:air",
                "minecraft:dirt",
                NetherFlavor::NetherWastes,
                true,
            );
            assert_eq!(out.block, Some(dry_or_burn.1));
        }

        assert_eq!(
            transform_block(
                [0, 70, 0],
                "minecraft:nether_brick_fence",
                "minecraft:air",
                "minecraft:nether_bricks",
                NetherFlavor::CrimsonForest,
                true,
            )
            .block,
            None
        );
    }

    #[test]
    fn min_y_sinks_to_bedrock_with_maturity_and_near_core() {
        let early_far = allowed_min_y(64, 0, 40, 20);
        let early_core = allowed_min_y(64, 0, 0, 20);
        let mid_core = allowed_min_y(64, 2_400, 0, 30_000);
        let late_core = allowed_min_y(64, 9_000, 0, 10_000);
        let mature = allowed_min_y(64, 100, 200, 75_000);
        assert!(early_far > early_core, "rim should stay shallower than core");
        assert!(early_core < 45 && early_core > 20, "fresh rift starts reaching shallow caves");
        assert!(mid_core < early_core && mid_core > BEDROCK_FLOOR_Y);
        assert_eq!(late_core, BEDROCK_FLOOR_Y);
        assert_eq!(mature, BEDROCK_FLOOR_Y);
    }

    #[test]
    fn surface_column_y_range_covers_mountain_peaks_cliffs_and_underground_portals() {
        // Open plains: surface=64, ocean_floor=64, neighbors=64 -> shallow crust [66..59].
        let (top_y, min_y) = surface_column_y_range(64, false, 64, 64, 64);
        assert_eq!(top_y, 66);
        assert_eq!(min_y, 64 - SURFACE_CRUST_DEPTH);

        // Portal on mountain base (y=90) expanding UP onto a y=240 snowy peak:
        // must NOT clip top_y at 128 or 90+12, and sweeps through both full crust and sheer cliffs!
        let (peak_top, peak_min) = surface_column_y_range(90, false, 240, 240, 215);
        assert_eq!(peak_top, 242);
        assert_eq!(peak_min, 240 - SURFACE_CRUST_DEPTH);
        let (_, cliff_min) = surface_column_y_range(90, false, 240, 240, 200);
        assert_eq!(cliff_min, 204);

        // Tall tree canopy: WorldSurface=82, OceanFloor=64 -> sweeps canopy AND reaches ground crust.
        let (tree_top, tree_min) = surface_column_y_range(64, false, 82, 64, 64);
        assert_eq!(tree_top, 84);
        assert_eq!(tree_min, 64 - SURFACE_CRUST_DEPTH);

        // Portal genuinely lit inside a deep cave at y=10 -> starts centered on cave elevation.
        let (cave_top, cave_min) = surface_column_y_range(10, true, 64, 64, 64);
        assert_eq!(cave_top, 26);
        assert_eq!(cave_min, 0);
    }

    #[test]
    fn effective_budget_scales_smoothly_for_consistent_radial_pace() {
        let b_small = effective_rift_budget(14, 3);
        let b_mid = effective_rift_budget(14, 30);
        let b_large = effective_rift_budget(14, 90);
        assert!(b_small < b_mid && b_mid < b_large);
        assert!(b_small <= 10, "small radius should start steady, not explosive");
        assert!(b_large <= 56, "large radius stays server-friendly");

        // Newly lit portal creeps slowly, and every single corrupted block nudges speed upward.
        let r0 = rift_surface_ops_per_tick(14, 1, 0);
        let r1 = rift_surface_ops_per_tick(14, 1, 1);
        let r10 = rift_surface_ops_per_tick(14, 3, 10);
        let r500 = rift_surface_ops_per_tick(14, 8, 500);
        let r5k = rift_surface_ops_per_tick(14, 24, 5_000);
        let r40k = rift_surface_ops_per_tick(14, 80, 40_000);
        assert!(r0 <= 0.12, "fresh portal must start very slow (got {r0})");
        assert!(r0 < r1 && r1 < r10 && r10 < r500 && r500 < r5k && r5k < r40k);
        assert!(
            rift_deepen_ops_per_tick(14, 4, 200) < rift_deepen_ops_per_tick(14, 40, 20_000)
        );
    }

    #[test]
    fn circular_rings_cover_full_360_disk_with_zero_gaps() {
        use crate::types::build_ring_columns;
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for r in 1..=12 {
            let cols = build_ring_columns(10, -20, r);
            assert!(!cols.is_empty());
            for c in cols {
                assert!(seen.insert(c), "duplicate column {c:?} at ring {r}");
                let dx = c[0] - 10;
                let dz = c[1] + 20;
                let d2 = dx * dx + dz * dz;
                assert!(d2 <= r * r, "column {c:?} exceeded ring {r}");
            }
        }
        for dx in -12..=12 {
            for dz in -12..=12 {
                if dx * dx + dz * dz <= 144 {
                    assert!(
                        seen.contains(&[10 + dx, -20 + dz]),
                        "missing column ({}, {})",
                        10 + dx,
                        -20 + dz
                    );
                }
            }
        }
    }

    #[test]
    fn nether_flora_only_plants_on_supporting_nether_blocks() {
        let is_nether_plant = |b: &str| {
            matches!(
                strip_ns(b),
                "crimson_roots"
                    | "warped_roots"
                    | "nether_sprouts"
                    | "crimson_fungus"
                    | "warped_fungus"
            )
        };

        // Clearing Overworld foliage/vines/tall grass must produce air, never floating Nether plants.
        for weed in [
            "minecraft:short_grass",
            "minecraft:tall_grass",
            "minecraft:fern",
            "minecraft:vine",
            "minecraft:dandelion",
            "minecraft:snow",
        ] {
            for flavor in [
                NetherFlavor::CrimsonForest,
                NetherFlavor::WarpedForest,
                NetherFlavor::NetherWastes,
            ] {
                for x in 0..40 {
                    let out = transform_block(
                        [x, 70, x * 3],
                        weed,
                        "minecraft:air",
                        "minecraft:stone",
                        flavor,
                        true,
                    );
                    assert_eq!(out.block, Some("minecraft:air"));
                    assert_eq!(out.above, None);
                }
            }
        }

        // Whenever a soil block spawns a Nether plant above it, the ground block itself
        // must support Nether flora (crimson_nylium / warped_nylium / soul_soil), never netherrack.
        for flavor in [
            NetherFlavor::CrimsonForest,
            NetherFlavor::WarpedForest,
            NetherFlavor::SoulSandValley,
            NetherFlavor::BasaltDeltas,
            NetherFlavor::NetherWastes,
        ] {
            for x in 0..300 {
                let out = transform_block(
                    [x, 64, x * 5],
                    "minecraft:grass_block",
                    "minecraft:air",
                    "minecraft:dirt",
                    flavor,
                    true,
                );
                if let Some(above) = out.above
                    && is_nether_plant(above)
                {
                    let base = out.block.expect("plant must have converted base block");
                    assert!(
                        can_support_nether_flora(base),
                        "placed {above} on unsupported base {base} in {flavor:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn village_wood_stairs_slabs_doors_and_paths_corrupt_accurately() {
        let village_wood = [
            "minecraft:oak_planks",
            "minecraft:spruce_planks",
            "minecraft:stripped_oak_log",
            "minecraft:dark_oak_wood",
            "minecraft:oak_stairs",
            "minecraft:spruce_stairs",
            "minecraft:birch_slab",
            "minecraft:oak_fence",
            "minecraft:oak_fence_gate",
            "minecraft:oak_door",
            "minecraft:spruce_trapdoor",
            "minecraft:farmland",
        ];
        for flavor in [
            NetherFlavor::CrimsonForest,
            NetherFlavor::WarpedForest,
            NetherFlavor::SoulSandValley,
            NetherFlavor::BasaltDeltas,
            NetherFlavor::NetherWastes,
        ] {
            for block in village_wood {
                for x in 0..40 {
                    let out = transform_block(
                        [x * 3, 66, x - 5],
                        block,
                        "minecraft:air",
                        "minecraft:dirt",
                        flavor,
                        true,
                    );
                    assert!(
                        out.block.is_some(),
                        "{block} must always corrupt in {flavor:?}, never stay Overworld"
                    );
                }
            }
        }

        for masonry in [
            "minecraft:cobblestone_stairs",
            "minecraft:cobblestone_slab",
            "minecraft:cobblestone_wall",
            "minecraft:stone_brick_stairs",
            "minecraft:smooth_stone",
            "minecraft:smooth_stone_slab",
            "minecraft:andesite_stairs",
            "minecraft:tuff_bricks",
        ] {
            let mut conv = 0;
            for x in 0..100 {
                let out = transform_block(
                    [x, 68, x * 2],
                    masonry,
                    "minecraft:air",
                    "minecraft:stone",
                    NetherFlavor::NetherWastes,
                    true,
                );
                if out.block.is_some() {
                    conv += 1;
                }
            }
            assert!(
                conv >= 90,
                "{masonry} should overwhelmingly corrupt (got {conv}/100)"
            );
        }

        let mut path_corrupted = 0;
        let mut path_preserved = 0;
        for x in 0..400 {
            let out = transform_block(
                [x, 65, (x * 7) % 131],
                "minecraft:dirt_path",
                "minecraft:air",
                "minecraft:dirt",
                NetherFlavor::CrimsonForest,
                true,
            );
            if out.block.is_some() {
                path_corrupted += 1;
            } else if out.is_corrupt {
                path_preserved += 1;
            }
        }
        assert!(
            path_corrupted >= 250 && path_preserved >= 40,
            "dirt_path should mostly corrupt while leaving traces (corrupted={path_corrupted}, preserved={path_preserved})"
        );
    }
}
