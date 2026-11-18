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
    pub const ALL: [Self; 5] = [
        Self::CrimsonForest,
        Self::WarpedForest,
        Self::SoulSandValley,
        Self::BasaltDeltas,
        Self::NetherWastes,
    ];

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

    pub fn key(self) -> &'static str {
        match self {
            Self::CrimsonForest => "crimson_forest",
            Self::WarpedForest => "warped_forest",
            Self::SoulSandValley => "soul_sand_valley",
            Self::BasaltDeltas => "basalt_deltas",
            Self::NetherWastes => "nether_wastes",
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
    /// shifts smoothly across ~96-block regions as a large rift expands.
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
    pub frontier: Vec<[i32; 3]>,
    pub bastions_placed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BastionSite {
    pub center: [i32; 3],
    pub flavor: NetherFlavor,
}

pub fn strip_ns(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

pub fn quart_coords(pos: [i32; 3]) -> [i32; 3] {
    [pos[0] >> 2, pos[1] >> 2, pos[2] >> 2]
}

pub fn quart_block_origin(pos: [i32; 3]) -> [i32; 3] {
    [(pos[0] >> 2) << 2, (pos[1] >> 2) << 2, (pos[2] >> 2) << 2]
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
