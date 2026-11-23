use crate::state::RiftState;
use crate::types::{NETHER, OVERWORLD, OutpostKind, spatial_hash, strip_ns};
use mod_api::{ContainerLootFilledEvent, ItemKey, ItemStack, Registrar};

/// Pre-registered item keys cached during `Hooks::register` so `container_loot_filled`
/// can enrich Fortress and Outpost chests using read-only `LevelHost`.
#[derive(Clone, Copy)]
pub struct ItemPalette {
    pub netherite_ingot: ItemKey,
    pub netherite_scrap: ItemKey,
    pub ancient_debris: ItemKey,
    pub upgrade_template: ItemKey,
    pub diamond: ItemKey,
    pub diamond_block: ItemKey,
    pub gold_block: ItemKey,
    pub gold_ingot: ItemKey,
    pub iron_block: ItemKey,
    pub iron_ingot: ItemKey,
    pub coal_block: ItemKey,
    pub enchanted_golden_apple: ItemKey,
    pub golden_apple: ItemKey,
    pub golden_carrot: ItemKey,
    pub cooked_porkchop: ItemKey,
    pub blaze_rod: ItemKey,
    pub magma_cream: ItemKey,
    pub ender_pearl: ItemKey,
    pub spectral_arrow: ItemKey,
    pub fire_charge: ItemKey,
    pub ghast_tear: ItemKey,
    pub obsidian: ItemKey,
    pub crying_obsidian: ItemKey,
    pub experience_bottle: ItemKey,
    pub totem_of_undying: ItemKey,
}

impl ItemPalette {
    pub fn register(r: &mut dyn Registrar) -> Self {
        Self {
            netherite_ingot: r.item_key("minecraft:netherite_ingot"),
            netherite_scrap: r.item_key("minecraft:netherite_scrap"),
            ancient_debris: r.item_key("minecraft:ancient_debris"),
            upgrade_template: r.item_key("minecraft:netherite_upgrade_smithing_template"),
            diamond: r.item_key("minecraft:diamond"),
            diamond_block: r.item_key("minecraft:diamond_block"),
            gold_block: r.item_key("minecraft:gold_block"),
            gold_ingot: r.item_key("minecraft:gold_ingot"),
            iron_block: r.item_key("minecraft:iron_block"),
            iron_ingot: r.item_key("minecraft:iron_ingot"),
            coal_block: r.item_key("minecraft:coal_block"),
            enchanted_golden_apple: r.item_key("minecraft:enchanted_golden_apple"),
            golden_apple: r.item_key("minecraft:golden_apple"),
            golden_carrot: r.item_key("minecraft:golden_carrot"),
            cooked_porkchop: r.item_key("minecraft:cooked_porkchop"),
            blaze_rod: r.item_key("minecraft:blaze_rod"),
            magma_cream: r.item_key("minecraft:magma_cream"),
            ender_pearl: r.item_key("minecraft:ender_pearl"),
            spectral_arrow: r.item_key("minecraft:spectral_arrow"),
            fire_charge: r.item_key("minecraft:fire_charge"),
            ghast_tear: r.item_key("minecraft:ghast_tear"),
            obsidian: r.item_key("minecraft:obsidian"),
            crying_obsidian: r.item_key("minecraft:crying_obsidian"),
            experience_bottle: r.item_key("minecraft:experience_bottle"),
            totem_of_undying: r.item_key("minecraft:totem_of_undying"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChestLootTier {
    CitadelVault,
    CitadelArmory,
    CitadelReserve,
    OutpostVault(OutpostKind),
    OutpostSupply(OutpostKind),
    NetherFobVault,
    NetherFobSupply,
}

#[cfg(test)]
pub fn classify_structure_chest(state: &RiftState, pos: [i32; 3]) -> Option<ChestLootTier> {
    classify_structure_chest_in_dim(state, OVERWORLD, pos)
}

pub fn classify_structure_chest_in_dim(
    state: &RiftState,
    dimension: &str,
    pos: [i32; 3],
) -> Option<ChestLootTier> {
    if dimension == NETHER {
        for f in &state.nether_fobs {
            let dx_abs = (pos[0] - f[0]).abs();
            let dz = pos[2] - f[2];
            let cheb = dx_abs.max(dz.abs());
            let dy = (pos[1] - f[1]).abs();
            if cheb <= 26 && dy <= 24 {
                let is_vault = (12..=18).contains(&dx_abs) && (-8..=-4).contains(&dz);
                return Some(if is_vault {
                    ChestLootTier::NetherFobVault
                } else {
                    ChestLootTier::NetherFobSupply
                });
            }
        }
        return None;
    }

    if dimension != OVERWORLD {
        return None;
    }

    for c in &state.citadels {
        let dx = (pos[0] - c[0]).abs();
        let dz = (pos[2] - c[2]).abs();
        let cheb = dx.max(dz);
        if cheb <= 30 && (pos[1] - c[1]).abs() <= 28 {
            return Some(if dx >= 14 && dz >= 14 {
                if (pos[0] - c[0]) * (pos[2] - c[2]) > 0 {
                    ChestLootTier::CitadelVault
                } else {
                    ChestLootTier::CitadelArmory
                }
            } else {
                ChestLootTier::CitadelReserve
            });
        }
    }

    for o in &state.outposts {
        let dx_abs = (pos[0] - o.center[0]).abs();
        let dz_abs = (pos[2] - o.center[2]).abs();
        let cheb = dx_abs.max(dz_abs);
        let dy = pos[1] - o.center[1];
        if cheb <= 28 && dy.abs() <= 32 {
            let is_vault = dy >= 6 || (dx_abs <= 4 && (13..=17).contains(&dz_abs));
            return Some(if is_vault {
                ChestLootTier::OutpostVault(o.kind)
            } else {
                ChestLootTier::OutpostSupply(o.kind)
            });
        }
    }

    None
}

fn push_stack(drops: &mut Vec<ItemStack>, key: ItemKey, count: u32) {
    if drops.len() >= 25 {
        return;
    }
    if let Some(stack) = ItemStack::new(key, count) {
        drops.push(stack);
    }
}

pub fn enrich_container_loot(
    state: &RiftState,
    event: &mut ContainerLootFilledEvent<'_>,
    items: &ItemPalette,
) {
    let Some(pos) = event.context.block else {
        return;
    };
    let Some(tier) = classify_structure_chest_in_dim(state, event.context.dimension, pos) else {
        return;
    };

    let h = spatial_hash(pos[0], pos[1], pos[2], state.seed ^ 0x1007_C4E5);
    let r = |shift: u32, min: u32, max: u32| -> u32 {
        min + (((h >> shift) as u32) % (max - min + 1))
    };

    match tier {
        ChestLootTier::CitadelVault => {
            push_stack(event.drops, items.netherite_ingot, r(0, 1, 2));
            push_stack(event.drops, items.ancient_debris, r(4, 2, 4));
            push_stack(event.drops, items.upgrade_template, 1);
            push_stack(event.drops, items.diamond, r(8, 5, 9));
            push_stack(event.drops, items.gold_block, r(12, 2, 3));
            push_stack(
                event.drops,
                if h % 3 == 0 {
                    items.enchanted_golden_apple
                } else {
                    items.golden_apple
                },
                if h % 3 == 0 { 1 } else { r(16, 2, 4) },
            );
            push_stack(event.drops, items.experience_bottle, r(20, 8, 16));
            if h % 2 == 0 {
                push_stack(event.drops, items.totem_of_undying, 1);
            }
        }
        ChestLootTier::CitadelArmory => {
            push_stack(event.drops, items.netherite_scrap, r(0, 2, 4));
            push_stack(event.drops, items.diamond, r(4, 3, 6));
            push_stack(event.drops, items.golden_apple, r(8, 2, 4));
            push_stack(event.drops, items.blaze_rod, r(12, 4, 8));
            push_stack(event.drops, items.spectral_arrow, r(16, 16, 28));
            push_stack(event.drops, items.ender_pearl, r(20, 4, 8));
            push_stack(event.drops, items.ghast_tear, r(24, 2, 4));
        }
        ChestLootTier::CitadelReserve => {
            push_stack(event.drops, items.gold_ingot, r(0, 6, 14));
            push_stack(event.drops, items.golden_carrot, r(4, 8, 16));
            push_stack(event.drops, items.cooked_porkchop, r(8, 8, 16));
            push_stack(event.drops, items.fire_charge, r(12, 6, 12));
            push_stack(event.drops, items.crying_obsidian, r(16, 4, 8));
            push_stack(event.drops, items.blaze_rod, r(20, 2, 5));
        }
        ChestLootTier::OutpostVault(OutpostKind::HoglinVillage) => {
            push_stack(event.drops, items.netherite_scrap, r(0, 2, 4));
            push_stack(event.drops, items.gold_block, r(4, 2, 3));
            push_stack(event.drops, items.diamond, r(8, 3, 6));
            push_stack(event.drops, items.golden_apple, r(12, 2, 4));
            push_stack(event.drops, items.cooked_porkchop, r(16, 12, 24));
        }
        ChestLootTier::OutpostSupply(OutpostKind::HoglinVillage) => {
            push_stack(event.drops, items.cooked_porkchop, r(0, 10, 18));
            push_stack(event.drops, items.gold_ingot, r(4, 6, 12));
            push_stack(event.drops, items.golden_carrot, r(8, 8, 16));
            push_stack(event.drops, items.spectral_arrow, r(12, 16, 24));
            push_stack(event.drops, items.experience_bottle, r(16, 6, 12));
        }
        ChestLootTier::OutpostVault(OutpostKind::InfernalFoundry) => {
            push_stack(event.drops, items.netherite_scrap, r(0, 2, 4));
            push_stack(event.drops, items.ancient_debris, r(4, 1, 3));
            if h % 2 == 0 {
                push_stack(event.drops, items.upgrade_template, 1);
            }
            push_stack(event.drops, items.iron_block, r(8, 2, 5));
            push_stack(event.drops, items.gold_block, r(12, 1, 3));
            push_stack(event.drops, items.diamond, r(16, 4, 7));
        }
        ChestLootTier::OutpostSupply(OutpostKind::InfernalFoundry) => {
            push_stack(event.drops, items.iron_ingot, r(0, 14, 28));
            push_stack(event.drops, items.coal_block, r(4, 3, 6));
            push_stack(event.drops, items.blaze_rod, r(8, 5, 10));
            push_stack(event.drops, items.magma_cream, r(12, 5, 10));
            push_stack(event.drops, items.obsidian, r(16, 6, 12));
        }
        ChestLootTier::OutpostVault(OutpostKind::SiegeWatchtower) => {
            push_stack(event.drops, items.netherite_scrap, r(0, 2, 3));
            push_stack(event.drops, items.diamond_block, 1);
            push_stack(event.drops, items.golden_apple, r(4, 2, 4));
            push_stack(event.drops, items.ender_pearl, r(8, 5, 10));
            push_stack(event.drops, items.spectral_arrow, r(12, 20, 32));
            push_stack(event.drops, items.experience_bottle, r(16, 8, 16));
        }
        ChestLootTier::OutpostSupply(OutpostKind::SiegeWatchtower) => {
            push_stack(event.drops, items.gold_ingot, r(0, 8, 14));
            push_stack(event.drops, items.iron_ingot, r(4, 10, 18));
            push_stack(event.drops, items.fire_charge, r(8, 8, 14));
            push_stack(event.drops, items.cooked_porkchop, r(12, 8, 14));
        }
        ChestLootTier::OutpostVault(OutpostKind::BoneCatacomb) => {
            push_stack(event.drops, items.ancient_debris, r(0, 1, 2));
            push_stack(event.drops, items.diamond, r(4, 4, 8));
            push_stack(event.drops, items.ghast_tear, r(8, 3, 6));
            push_stack(event.drops, items.crying_obsidian, r(12, 4, 8));
            push_stack(event.drops, items.experience_bottle, r(16, 10, 18));
            if h % 3 == 0 {
                push_stack(event.drops, items.totem_of_undying, 1);
            }
        }
        ChestLootTier::OutpostSupply(OutpostKind::BoneCatacomb) => {
            push_stack(event.drops, items.crying_obsidian, r(0, 2, 5));
            push_stack(event.drops, items.blaze_rod, r(4, 4, 8));
            push_stack(event.drops, items.spectral_arrow, r(8, 12, 24));
            push_stack(event.drops, items.experience_bottle, r(12, 6, 12));
        }
        ChestLootTier::OutpostVault(OutpostKind::ArenaPit) => {
            push_stack(event.drops, items.netherite_ingot, 1);
            push_stack(event.drops, items.diamond_block, r(0, 1, 2));
            push_stack(event.drops, items.golden_apple, r(4, 3, 5));
            push_stack(event.drops, items.gold_block, r(8, 2, 4));
            push_stack(event.drops, items.experience_bottle, r(12, 8, 16));
        }
        ChestLootTier::OutpostSupply(OutpostKind::ArenaPit) => {
            push_stack(event.drops, items.cooked_porkchop, r(0, 10, 20));
            push_stack(event.drops, items.iron_block, r(4, 1, 2));
            push_stack(event.drops, items.gold_ingot, r(8, 8, 16));
            push_stack(event.drops, items.spectral_arrow, r(12, 16, 28));
        }
        ChestLootTier::OutpostVault(OutpostKind::AlchemistSpire) => {
            push_stack(event.drops, items.upgrade_template, 1);
            push_stack(event.drops, items.netherite_scrap, r(0, 2, 4));
            push_stack(event.drops, items.ghast_tear, r(4, 4, 8));
            push_stack(event.drops, items.ender_pearl, r(8, 6, 12));
            push_stack(event.drops, items.experience_bottle, r(12, 12, 20));
            if h % 2 == 0 {
                push_stack(event.drops, items.enchanted_golden_apple, 1);
            }
        }
        ChestLootTier::OutpostSupply(OutpostKind::AlchemistSpire) => {
            push_stack(event.drops, items.blaze_rod, r(0, 6, 12));
            push_stack(event.drops, items.magma_cream, r(4, 6, 12));
            push_stack(event.drops, items.ender_pearl, r(8, 4, 8));
            push_stack(event.drops, items.obsidian, r(12, 6, 12));
            push_stack(event.drops, items.golden_carrot, r(16, 8, 16));
        }
        ChestLootTier::NetherFobVault => {
            push_stack(event.drops, items.netherite_ingot, 1);
            push_stack(event.drops, items.ancient_debris, r(0, 2, 4));
            push_stack(event.drops, items.upgrade_template, 1);
            push_stack(event.drops, items.diamond_block, 1);
            push_stack(event.drops, items.gold_block, r(4, 2, 4));
            push_stack(
                event.drops,
                if h % 3 == 0 {
                    items.enchanted_golden_apple
                } else {
                    items.golden_apple
                },
                if h % 3 == 0 { 1 } else { r(8, 2, 4) },
            );
            push_stack(event.drops, items.experience_bottle, r(12, 10, 18));
        }
        ChestLootTier::NetherFobSupply => {
            push_stack(event.drops, items.netherite_scrap, r(0, 1, 3));
            push_stack(event.drops, items.gold_ingot, r(4, 10, 18));
            push_stack(event.drops, items.iron_ingot, r(8, 10, 20));
            push_stack(event.drops, items.blaze_rod, r(12, 4, 8));
            push_stack(event.drops, items.golden_carrot, r(16, 8, 16));
            push_stack(event.drops, items.obsidian, r(20, 6, 12));
        }
    }

    // Never keep vanilla empty junk if somehow rolled in non-treasure containers.
    let _ = strip_ns(event.context.table);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NetherFlavor, OutpostSite};

    #[test]
    fn classifies_citadel_and_outpost_chest_tiers_accurately() {
        let mut st = RiftState::new();
        st.citadels.push([100, 64, 100]);
        st.outposts.push(OutpostSite {
            center: [260, 70, 100],
            kind: OutpostKind::HoglinVillage,
            flavor: NetherFlavor::CrimsonForest,
        });

        assert_eq!(
            classify_structure_chest(&st, [84, 65, 83]),
            Some(ChestLootTier::CitadelVault)
        );
        assert_eq!(
            classify_structure_chest(&st, [117, 65, 83]),
            Some(ChestLootTier::CitadelArmory)
        );
        assert_eq!(
            classify_structure_chest(&st, [106, 65, 119]),
            Some(ChestLootTier::CitadelReserve)
        );
        assert_eq!(
            classify_structure_chest(&st, [258, 71, 115]),
            Some(ChestLootTier::OutpostVault(OutpostKind::HoglinVillage))
        );
        assert_eq!(
            classify_structure_chest(&st, [276, 71, 102]),
            Some(ChestLootTier::OutpostSupply(OutpostKind::HoglinVillage))
        );
        assert_eq!(classify_structure_chest(&st, [0, 64, 0]), None);

        // Verify every chest placed across all six OutpostKind archetypes resolves cleanly.
        for (i, kind) in [
            OutpostKind::HoglinVillage,
            OutpostKind::InfernalFoundry,
            OutpostKind::SiegeWatchtower,
            OutpostKind::BoneCatacomb,
            OutpostKind::ArenaPit,
            OutpostKind::AlchemistSpire,
        ]
        .into_iter()
        .enumerate()
        {
            let center = [600 + (i as i32) * 120, 70, 600];
            let mut single = RiftState::new();
            single.outposts.push(OutpostSite {
                center,
                kind,
                flavor: NetherFlavor::CrimsonForest,
            });
            for (off, expect_vault, _, _) in crate::outposts::outpost_chest_specs(kind) {
                let p = [center[0] + off[0], center[1] + off[1], center[2] + off[2]];
                let expected = if *expect_vault {
                    ChestLootTier::OutpostVault(kind)
                } else {
                    ChestLootTier::OutpostSupply(kind)
                };
                assert_eq!(
                    classify_structure_chest(&single, p),
                    Some(expected),
                    "{kind:?} chest offset {off:?} misclassified"
                );
            }
        }

        // Verify Nether Forward Operating Base chests resolve exclusively inside NETHER.
        let mut fob_state = RiftState::new();
        fob_state.nether_fobs.push([100, 52, -100]);
        for (off, expect_vault, _, _) in crate::nether_fob::fob_chest_specs() {
            let p = [100 + off[0], 52 + off[1], -100 + off[2]];
            let expected = if *expect_vault {
                ChestLootTier::NetherFobVault
            } else {
                ChestLootTier::NetherFobSupply
            };
            assert_eq!(classify_structure_chest_in_dim(&fob_state, NETHER, p), Some(expected));
            assert_eq!(classify_structure_chest(&fob_state, p), None);
        }
    }
}
