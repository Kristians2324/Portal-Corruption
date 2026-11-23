pub const OVERWORLD: &str = "minecraft:overworld";
pub const OVERWORLD_MIN_Y: i32 = -64;
pub const OVERWORLD_MAX_Y: i32 = 319;
pub const BEDROCK_FLOOR_Y: i32 = -63;

/// Vanilla Nether biome themes projected into Overworld chunks.
/// All visuals, fog hues, mob spawns, and music use existing vanilla assets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetherFlavor {
    CrimsonForest = 0,
    WarpedForest = 1,
    SoulSandValley = 2,
    BasaltDeltas = 3,
    NetherWastes = 4,
}

impl NetherFlavor {
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::CrimsonForest,
            1 => Self::WarpedForest,
            2 => Self::SoulSandValley,
            3 => Self::BasaltDeltas,
            _ => Self::NetherWastes,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let short = strip_ns(name);
        match short {
            "crimson_forest" => Some(Self::CrimsonForest),
            "warped_forest" => Some(Self::WarpedForest),
            "soul_sand_valley" => Some(Self::SoulSandValley),
            "basalt_deltas" => Some(Self::BasaltDeltas),
            "nether_wastes" => Some(Self::NetherWastes),
            _ => None,
        }
    }

    pub fn biome_id(self) -> &'static str {
        match self {
            Self::CrimsonForest => "minecraft:crimson_forest",
            Self::WarpedForest => "minecraft:warped_forest",
            Self::SoulSandValley => "minecraft:soul_sand_valley",
            Self::BasaltDeltas => "minecraft:basalt_deltas",
            Self::NetherWastes => "minecraft:nether_wastes",
        }
    }

    pub fn music_event(self) -> &'static str {
        match self {
            Self::CrimsonForest => "minecraft:music.nether.crimson_forest",
            Self::WarpedForest => "minecraft:music.nether.warped_forest",
            Self::SoulSandValley => "minecraft:music.nether.soul_sand_valley",
            Self::BasaltDeltas => "minecraft:music.nether.basalt_deltas",
            Self::NetherWastes => "minecraft:music.nether.nether_wastes",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::CrimsonForest => "Crimson Bleed",
            Self::WarpedForest => "Warped Haze",
            Self::SoulSandValley => "Soul Ash Valley",
            Self::BasaltDeltas => "Basalt Scorch",
            Self::NetherWastes => "Hellfire Wastes",
        }
    }

    /// Deterministically picks a core flavor for a portal at `center`, or
    /// shifts seamlessly across ~96-block regions as a large rift expands.
    pub fn pick_for_region(x: i32, z: i32, salt: u64) -> Self {
        let rx = x.div_euclid(96);
        let rz = z.div_euclid(96);
        let h = spatial_hash(rx, 0, rz, salt ^ 0x51F7_B1ED);
        match (h % 10) as u8 {
            0 | 1 | 2 => Self::CrimsonForest,
            3 | 4 => Self::WarpedForest,
            5 | 6 => Self::SoulSandValley,
            7 | 8 => Self::BasaltDeltas,
            _ => Self::NetherWastes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiftSource {
    pub id: u64,
    pub center: [i32; 3],
    /// True while lit `minecraft:nether_portal` blocks exist here.
    /// False means the portal was broken/extinguished, leaving a persistent Rift Scar.
    pub active_portal: bool,
    /// True only when sealed inside a 3D Crying Obsidian + closed iron door chamber.
    pub contained: bool,
    pub flavor: NetherFlavor,
    pub age_ticks: u64,
    pub corrupted_blocks: u64,
    pub max_radius_sq: i64,
    /// Current expanding circular shell radius (1, 2, 3, ... indefinitely while uncontained).
    pub ring_radius: i32,
    /// Remaining `[x, z]` columns in the current circular shell.
    pub ring_columns: Vec<[i32; 2]>,
    /// Active column vertical sweep: `([x, z], next_y_down, min_y)`.
    pub active_column: Option<([i32; 2], i32, i32)>,
    /// Inner-ring deepening pass so underground zones sink all the way to bedrock `-63`.
    pub deepen_radius: i32,
    pub deepen_columns: Vec<[i32; 2]>,
    /// Highest `deepen_radius` ever reached by this Rift (preserved when `deepen_radius` wraps to 1).
    pub max_deepen_reached: i32,
    pub bastions_placed: u32,
    pub outposts_placed: u32,
    /// True once this Rift's unique central Citadel has erupted around the portal.
    pub citadel_spawned: bool,
    /// Persistent UUID of the Wither Sovereign guarding this Rift's Citadel.
    pub boss_uuid: Option<u128>,
    /// True once that Wither Sovereign is slain: spread halts forever without needing a seal.
    pub pacified: bool,
    /// True while ~80% Overworld regression is actively healing this Rift's territory.
    pub regressing: bool,
    pub regress_radius: i32,
    pub regress_columns: Vec<[i32; 2]>,
    pub regress_active_column: Option<([i32; 2], i32, i32)>,
}

impl RiftSource {
    pub fn is_actively_spreading(&self) -> bool {
        !self.contained && !self.pacified
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BastionSite {
    pub center: [i32; 3],
    pub flavor: NetherFlavor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutpostKind {
    HoglinVillage = 0,
    InfernalFoundry = 1,
    SiegeWatchtower = 2,
}

impl OutpostKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::InfernalFoundry,
            2 => Self::SiegeWatchtower,
            _ => Self::HoglinVillage,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::HoglinVillage => "Hoglin War-Kraal",
            Self::InfernalFoundry => "Hellforge Foundry",
            Self::SiegeWatchtower => "Mordor Siege Garrison",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutpostSite {
    pub center: [i32; 3],
    pub kind: OutpostKind,
    pub flavor: NetherFlavor,
}

pub fn strip_ns(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

pub fn quart_coords(pos: [i32; 3]) -> [i32; 3] {
    [pos[0] >> 2, pos[1] >> 2, pos[2] >> 2]
}

pub fn spatial_hash(x: i32, y: i32, z: i32, salt: u64) -> u64 {
    let mut h = salt
        ^ (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ (z as i64 as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

pub fn dist_sq(a: [i32; 3], b: [i32; 3]) -> i64 {
    let dx = i64::from(a[0] - b[0]);
    let dy = i64::from(a[1] - b[1]);
    let dz = i64::from(a[2] - b[2]);
    dx * dx + dy * dy + dz * dz
}

pub fn horiz_dist_sq(a: [i32; 3], b: [i32; 3]) -> i64 {
    let dx = i64::from(a[0] - b[0]);
    let dz = i64::from(a[2] - b[2]);
    dx * dx + dz * dz
}

pub fn neighbors_6(pos: [i32; 3]) -> [[i32; 3]; 6] {
    [
        [pos[0] + 1, pos[1], pos[2]],
        [pos[0] - 1, pos[1], pos[2]],
        [pos[0], pos[1] + 1, pos[2]],
        [pos[0], pos[1] - 1, pos[2]],
        [pos[0], pos[1], pos[2] + 1],
        [pos[0], pos[1], pos[2] - 1],
    ]
}

/// Generates all integer `(x, z)` columns in the exact annulus `(R - 1)^2 < d^2 <= R^2`
/// around `(cx, cz)`, sorted radially/by-chunk so the entire 360-degree circle
/// expands evenly with minimal chunk-loading thrash.
pub fn build_ring_columns(cx: i32, cz: i32, radius: i32) -> Vec<[i32; 2]> {
    if radius <= 0 {
        return vec![[cx, cz]];
    }
    let r2 = i64::from(radius) * i64::from(radius);
    let prev_r2 = if radius == 1 {
        -1i64
    } else {
        let p = i64::from(radius - 1);
        p * p
    };

    let mut cols = Vec::with_capacity((radius as usize) * 7 + 8);
    for dx in -radius..=radius {
        let dx2 = i64::from(dx) * i64::from(dx);
        let max_dz = ((r2 - dx2) as f64).sqrt().floor() as i32;
        if prev_r2 < dx2 {
            for dz in -max_dz..=max_dz {
                cols.push([cx + dx, cz + dz]);
            }
        } else {
            let mut min_dz = (((prev_r2 - dx2) as f64).sqrt().floor() as i32) + 1;
            while i64::from(min_dz) * i64::from(min_dz) + dx2 <= prev_r2 {
                min_dz += 1;
            }
            for dz in min_dz..=max_dz {
                cols.push([cx + dx, cz + dz]);
                if dz != 0 {
                    cols.push([cx + dx, cz - dz]);
                }
            }
        }
    }

    // Group by chunk while ordering around the ring angle so loaded chunks are
    // reused cleanly before moving to the next chunk along the circle.
    cols.sort_by(|a, b| {
        let ac = (a[0] >> 4, a[1] >> 4);
        let bc = (b[0] >> 4, b[1] >> 4);
        let a_ang = f64::from(ac.1 * 16 + 8 - cz).atan2(f64::from(ac.0 * 16 + 8 - cx));
        let b_ang = f64::from(bc.1 * 16 + 8 - cz).atan2(f64::from(bc.0 * 16 + 8 - cx));
        a_ang
            .total_cmp(&b_ang)
            .then_with(|| ac.cmp(&bc))
            .then_with(|| a[0].cmp(&b[0]))
            .then_with(|| a[1].cmp(&b[1]))
    });
    cols
}
