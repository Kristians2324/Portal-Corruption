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
            | "magma_block"
            | "glowstone"
            | "shroomlight"
            | "crimson_stem"
            | "warped_stem"
            | "stripped_crimson_stem"
            | "stripped_warped_stem"
            | "crimson_hyphae"
            | "warped_hyphae"
            | "crimson_planks"
            | "warped_planks"
            | "crimson_fence"
            | "warped_fence"
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

/// Computes how deep corruption reaches at horizontal distance `horiz_dist`
/// from a rift at `center_y`, immediately penetrating underground caves near the
/// core and boring all the way to Bedrock (`-63`) as the rift matures.
pub fn allowed_min_y(center_y: i32, age_ticks: u64, horiz_dist: i32, corrupted_blocks: u64) -> i32 {
    if corrupted_blocks >= 2_000 || age_ticks >= 1_200 {
        return BEDROCK_FLOOR_Y;
    }
    let sink_bonus = (age_ticks / 10).min(160) as i32;
    let cone_rise = (horiz_dist / 4).min(36);
    (center_y - 26 - sink_bonus + cone_rise).clamp(BEDROCK_FLOOR_Y, OVERWORLD_MAX_Y - 8)
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

fn transform_wood_and_foliage(
    short: &str,
    pos: [i32; 3],
    flavor: NetherFlavor,
    h: u64,
) -> Option<TransformOutcome> {
    if short.ends_with("_log") || short.ends_with("_wood") {
        // Leave ~10% dead/stripped Overworld trunks scarred rather than uniform stems.
        if in_remnant_cluster(pos, 0x712E_A501, 10) {
            return Some(TransformOutcome::replace("minecraft:stripped_oak_log"));
        }
        let stem = match flavor {
            NetherFlavor::CrimsonForest => "minecraft:crimson_stem",
            NetherFlavor::WarpedForest => "minecraft:warped_stem",
            NetherFlavor::BasaltDeltas | NetherFlavor::SoulSandValley => "minecraft:basalt",
            NetherFlavor::NetherWastes => {
                if h % 3 == 0 {
                    "minecraft:crimson_stem"
                } else {
                    "minecraft:basalt"
                }
            }
        };
        return Some(TransformOutcome::replace(stem));
    }

    if short.ends_with("_leaves") || short == "azalea" || short == "flowering_azalea" {
        let replacement = match (flavor, h % 100) {
            (_, 0..=4) => "minecraft:shroomlight",
            (NetherFlavor::CrimsonForest, 5..=62) => "minecraft:nether_wart_block",
            (NetherFlavor::WarpedForest, 5..=62) => "minecraft:warped_wart_block",
            (NetherFlavor::SoulSandValley | NetherFlavor::BasaltDeltas, 5..=24) => {
                "minecraft:basalt"
            }
            (NetherFlavor::NetherWastes, 5..=39) => "minecraft:nether_wart_block",
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
    let deep_rock = matches!(short, "deepslate" | "cobbled_deepslate" | "tuff");
    let normal_rock = matches!(
        short,
        "stone"
            | "granite"
            | "diorite"
            | "andesite"
            | "calcite"
            | "dripstone_block"
            | "sandstone"
            | "red_sandstone"
            | "terracotta"
    ) || short.ends_with("_terracotta");

    if !deep_rock && !normal_rock {
        return None;
    }

    // Leave ~12% scarred Overworld stone/cobble pockets so terrain visibly feels native Overworld.
    if in_remnant_cluster(pos, 0x5CA2_1001, 12) {
        if short == "stone" && h % 2 == 0 {
            return Some(TransformOutcome::replace("minecraft:cobblestone"));
        }
        return Some(TransformOutcome::unchanged(true));
    }

    // Deep Ancient Debris only in mature rifts, deep underground (-32..-60).
    if rift_mature && pos[1] <= -32 && pos[1] >= -60 {
        let debris_roll = scar_roll(pos, 0xDE82_1500);
        let odds = if pos[1] <= -46 { 250 } else { 380 };
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

    if matches!(short, "ice" | "packed_ice" | "blue_ice" | "frosted_ice") {
        return TransformOutcome::replace("minecraft:air");
    }

    if matches!(short, "snow" | "snow_block") {
        return TransformOutcome::replace("minecraft:air");
    }

    // Soft Overworld foliage / flowers / vines / snow -> clear cleanly to air so
    // Nether roots & fungi are only ever planted AFTER the ground beneath becomes nylium.
    if is_open_space(current) {
        return TransformOutcome::replace("minecraft:air");
    }

    if let Some(wood_out) = transform_wood_and_foliage(short, pos, flavor, h) {
        return wood_out;
    }

    // Surface soils & ground covers.
    if matches!(
        short,
        "grass_block"
            | "dirt"
            | "coarse_dirt"
            | "rooted_dirt"
            | "podzol"
            | "mycelium"
            | "moss_block"
            | "pale_moss_block"
            | "mud"
            | "muddy_mangrove_roots"
            | "clay"
    ) {
        if short != "coarse_dirt" && in_remnant_cluster(pos, 0x5CA2_2002, 12) {
            if h % 4 == 0 && matches!(short, "grass_block" | "dirt") {
                return TransformOutcome::unchanged(true);
            }
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

    if matches!(short, "sand" | "red_sand" | "gravel" | "suspicious_sand" | "suspicious_gravel") {
        if in_remnant_cluster(pos, 0x5CA2_3003, 12) {
            return TransformOutcome::unchanged(true);
        }
        let block = match flavor {
            NetherFlavor::BasaltDeltas => "minecraft:blackstone",
            NetherFlavor::CrimsonForest | NetherFlavor::WarpedForest => "minecraft:netherrack",
            NetherFlavor::SoulSandValley | NetherFlavor::NetherWastes => "minecraft:soul_sand",
        };
        return TransformOutcome::replace(block);
    }

    // Overworld common ores (partially preserved, rest converted into Nether ores/rack).
    if matches!(short, "coal_ore" | "deepslate_coal_ore" | "copper_ore" | "deepslate_copper_ore") {
        if h % 3 == 0 {
            return TransformOutcome::replace("minecraft:nether_quartz_ore");
        }
        return TransformOutcome::unchanged(true);
    }
    if matches!(short, "iron_ore" | "deepslate_iron_ore" | "gold_ore" | "deepslate_gold_ore") {
        if h % 2 == 0 {
            return TransformOutcome::replace("minecraft:nether_gold_ore");
        }
        return TransformOutcome::unchanged(true);
    }

    // Built Overworld structures: scarred bricks and Nether planks.
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
        if in_remnant_cluster(pos, 0x5CA2_4004, 12) {
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

    if short.ends_with("_planks") {
        let plank = match flavor {
            NetherFlavor::WarpedForest => "minecraft:warped_planks",
            _ => "minecraft:crimson_planks",
        };
        return TransformOutcome::replace(plank);
    }

    if short.ends_with("_fence") {
        let fence = match flavor {
            NetherFlavor::WarpedForest => "minecraft:warped_fence",
            _ => "minecraft:crimson_fence",
        };
        return TransformOutcome::replace(fence);
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
        assert!(kept_or_cobble > 10 && warped > 150);
    }

    #[test]
    fn min_y_sinks_to_bedrock_with_maturity_and_near_core() {
        let early_far = allowed_min_y(64, 0, 120, 20);
        let early_core = allowed_min_y(64, 0, 0, 20);
        let late_core = allowed_min_y(64, 1_500, 0, 1_500);
        let mature = allowed_min_y(64, 100, 200, 2_000);
        assert!(early_far > early_core);
        assert_eq!(late_core, BEDROCK_FLOOR_Y);
        assert_eq!(mature, BEDROCK_FLOOR_Y);
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
}
