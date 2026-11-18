use crate::types::{OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, dist_sq, neighbors_6, strip_ns};
use std::collections::{HashSet, VecDeque};

const MAX_VISITED: usize = 2048;
const ESCAPE_RADIUS: i32 = 18;
const ESCAPE_RADIUS_SQ: i64 = (ESCAPE_RADIUS as i64) * (ESCAPE_RADIUS as i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentOutcome {
    /// Sealed inside Crying Obsidian (+ closed iron doors / trapdoors).
    Contained,
    /// Open or breached — corruption can escape into the Overworld.
    Breached,
    /// A boundary chunk is currently unloaded, so status should hold until loaded.
    Indeterminate,
}

pub trait ContainmentView {
    fn block_name(&self, pos: [i32; 3]) -> Option<String>;
    fn is_open(&self, pos: [i32; 3]) -> Option<bool>;
}

/// Checks if a single block seals the corruption flood-fill.
/// Plain obsidian intentionally leaks; only Crying Obsidian and closed Iron
/// Doors / Iron Trapdoors hold back the rift.
pub fn is_seal_block(block: &str, open: Option<bool>) -> bool {
    match strip_ns(block) {
        "crying_obsidian" => true,
        "iron_door" | "iron_trapdoor" => open == Some(false),
        _ => false,
    }
}

/// Flood-fills connected `minecraft:nether_portal` blocks around `approx_center`
/// to compute an updated portal center and whether any lit portal panes remain.
pub fn inspect_portal_cluster<V: ContainmentView>(
    view: &V,
    approx_center: [i32; 3],
) -> (bool, Vec<[i32; 3]>, [i32; 3]) {
    let mut seeds = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    for dy in -3..=3 {
        for dx in -3..=3 {
            for dz in -3..=3 {
                let p = [
                    approx_center[0] + dx,
                    (approx_center[1] + dy).clamp(OVERWORLD_MIN_Y, OVERWORLD_MAX_Y),
                    approx_center[2] + dz,
                ];
                if let Some(name) = view.block_name(p)
                    && strip_ns(&name) == "nether_portal"
                    && visited.insert(p)
                {
                    queue.push_back(p);
                }
            }
        }
    }

    while let Some(p) = queue.pop_front() {
        seeds.push(p);
        if seeds.len() >= 196 {
            break;
        }
        for n in neighbors_6(p) {
            if n[1] < OVERWORLD_MIN_Y || n[1] > OVERWORLD_MAX_Y || !visited.insert(n) {
                continue;
            }
            if let Some(name) = view.block_name(n)
                && strip_ns(&name) == "nether_portal"
            {
                queue.push_back(n);
            }
        }
    }

    if seeds.is_empty() {
        return (false, vec![approx_center], approx_center);
    }

    let mut sx = 0i64;
    let mut sy = 0i64;
    let mut sz = 0i64;
    for p in &seeds {
        sx += i64::from(p[0]);
        sy += i64::from(p[1]);
        sz += i64::from(p[2]);
    }
    let count = seeds.len() as i64;
    let centroid = [
        (sx / count) as i32,
        (sy / count) as i32,
        (sz / count) as i32,
    ];
    (true, seeds, centroid)
}

/// Performs a 3D 6-neighbor BFS from `seeds` (a lit portal cluster or a
/// lingering Rift Scar center). If BFS exhausts inside `MAX_VISITED` and
/// `ESCAPE_RADIUS` hitting only `crying_obsidian` and closed iron doors/trapdoors,
/// the chamber is sealed (supports 2-door airlocks natively).
pub fn evaluate_chamber<V: ContainmentView>(view: &V, seeds: &[[i32; 3]]) -> ContainmentOutcome {
    if seeds.is_empty() {
        return ContainmentOutcome::Breached;
    }

    let origin = seeds[0];
    let mut visited = HashSet::with_capacity(256);
    let mut queue = VecDeque::with_capacity(256);

    for &s in seeds {
        let Some(name) = view.block_name(s) else {
            return ContainmentOutcome::Indeterminate;
        };
        let open = view.is_open(s);
        if !is_seal_block(&name, open) && visited.insert(s) {
            queue.push_back(s);
        }
    }

    if queue.is_empty() {
        return ContainmentOutcome::Contained;
    }

    while let Some(pos) = queue.pop_front() {
        if dist_sq(pos, origin) > ESCAPE_RADIUS_SQ || visited.len() >= MAX_VISITED {
            return ContainmentOutcome::Breached;
        }

        for n in neighbors_6(pos) {
            if n[1] < OVERWORLD_MIN_Y || n[1] > OVERWORLD_MAX_Y {
                return ContainmentOutcome::Breached;
            }
            if !visited.insert(n) {
                continue;
            }
            let Some(name) = view.block_name(n) else {
                return ContainmentOutcome::Indeterminate;
            };
            let open = view.is_open(n);
            if !is_seal_block(&name, open) {
                queue.push_back(n);
            }
        }
    }

    ContainmentOutcome::Contained
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MockWorld {
        blocks: HashMap<[i32; 3], (String, Option<bool>)>,
        default_block: String,
    }

    impl MockWorld {
        fn new(default_block: &str) -> Self {
            Self {
                blocks: HashMap::new(),
                default_block: default_block.to_owned(),
            }
        }

        fn set(&mut self, pos: [i32; 3], block: &str, open: Option<bool>) {
            self.blocks.insert(pos, (block.to_owned(), open));
        }

        fn build_crying_box(&mut self, min: [i32; 3], max: [i32; 3]) {
            for x in min[0]..=max[0] {
                for y in min[1]..=max[1] {
                    for z in min[2]..=max[2] {
                        let on_shell = x == min[0]
                            || x == max[0]
                            || y == min[1]
                            || y == max[1]
                            || z == min[2]
                            || z == max[2];
                        if on_shell {
                            self.set([x, y, z], "minecraft:crying_obsidian", None);
                        } else {
                            self.set([x, y, z], "minecraft:air", None);
                        }
                    }
                }
            }
        }
    }

    impl ContainmentView for MockWorld {
        fn block_name(&self, pos: [i32; 3]) -> Option<String> {
            Some(
                self.blocks
                    .get(&pos)
                    .map(|(n, _)| n.clone())
                    .unwrap_or_else(|| self.default_block.clone()),
            )
        }

        fn is_open(&self, pos: [i32; 3]) -> Option<bool> {
            self.blocks.get(&pos).and_then(|(_, open)| *open)
        }
    }

    #[test]
    fn unenclosed_obsidian_portal_is_breached() {
        let mut w = MockWorld::new("minecraft:air");
        // Standard plain obsidian frame does not contain corruption.
        for x in -1..=2 {
            w.set([x, 64, 0], "minecraft:obsidian", None);
            w.set([x, 68, 0], "minecraft:obsidian", None);
        }
        for y in 65..=67 {
            w.set([-1, y, 0], "minecraft:obsidian", None);
            w.set([2, y, 0], "minecraft:obsidian", None);
            w.set([0, y, 0], "minecraft:nether_portal", None);
            w.set([1, y, 0], "minecraft:nether_portal", None);
        }
        let (lit, seeds, _) = inspect_portal_cluster(&w, [0, 65, 0]);
        assert!(lit);
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Breached);
    }

    #[test]
    fn crying_obsidian_chamber_seals_portal() {
        let mut w = MockWorld::new("minecraft:air");
        w.build_crying_box([-3, 63, -3], [4, 70, 3]);
        w.set([0, 65, 0], "minecraft:nether_portal", None);
        w.set([1, 65, 0], "minecraft:nether_portal", None);
        let (_, seeds, _) = inspect_portal_cluster(&w, [0, 65, 0]);
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Contained);

        // Removing a single roof block causes an immediate breach.
        w.set([0, 70, 0], "minecraft:obsidian", None);
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Breached);
    }

    #[test]
    fn crying_obsidian_airlock_works_with_two_iron_doors() {
        let mut w = MockWorld::new("minecraft:air");
        // Main chamber + outer airlock vestibule along +Z
        w.build_crying_box([-3, 63, -3], [3, 68, 2]);
        w.build_crying_box([-1, 63, 2], [1, 67, 6]);
        w.set([0, 65, 0], "minecraft:nether_portal", None);
        // Inner doorway at z = 2, outer doorway at z = 6
        w.set([0, 64, 2], "minecraft:iron_door", Some(false));
        w.set([0, 65, 2], "minecraft:iron_door", Some(false));
        w.set([0, 64, 6], "minecraft:iron_door", Some(false));
        w.set([0, 65, 6], "minecraft:iron_door", Some(false));

        let seeds = [[0, 65, 0]];
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Contained);

        // Open outer door while inner stays closed -> still contained!
        w.set([0, 64, 6], "minecraft:iron_door", Some(true));
        w.set([0, 65, 6], "minecraft:iron_door", Some(true));
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Contained);

        // Close outer door, open inner door -> still contained!
        w.set([0, 64, 6], "minecraft:iron_door", Some(false));
        w.set([0, 65, 6], "minecraft:iron_door", Some(false));
        w.set([0, 64, 2], "minecraft:iron_door", Some(true));
        w.set([0, 65, 2], "minecraft:iron_door", Some(true));
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Contained);

        // Open BOTH doors at the same time -> airlock breached!
        w.set([0, 64, 6], "minecraft:iron_door", Some(true));
        w.set([0, 65, 6], "minecraft:iron_door", Some(true));
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Breached);
    }

    #[test]
    fn scar_without_portal_still_requires_crying_obsidian_seal() {
        let mut w = MockWorld::new("minecraft:stone");
        w.build_crying_box([-1, 63, -1], [1, 66, 1]);
        w.set([0, 64, 0], "minecraft:air", None);
        let (lit, seeds, _) = inspect_portal_cluster(&w, [0, 64, 0]);
        assert!(!lit);
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Contained);

        // Break crying obsidian floor -> Scar keeps leaking!
        w.set([0, 63, 0], "minecraft:netherrack", None);
        assert_eq!(evaluate_chamber(&w, &seeds), ContainmentOutcome::Breached);
    }
}
