use crate::types::{
    BastionSite, NetherFlavor, OutpostKind, OutpostSite, RiftSource, build_ring_columns,
    quart_coords,
};
use mod_api::serde_json::{self, Map, Value};
use mod_api::{BlockState, ConfigPath, ServerHost, WorldEpoch};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const SAVE_FILE_NAME: &str = "rift_bleed.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectedWasteland {
    pub total_corrupted_blocks: u64,
    pub total_area_radius_sq: i64,
    pub max_deepen_radius: i32,
    pub max_ring_radius: i32,
    pub total_bastions_placed: u32,
    pub total_outposts_placed: u32,
    pub member_count: usize,
}

fn rifts_touch(a: &RiftSource, b: &RiftSource) -> bool {
    let ra = (a.max_radius_sq.max(0) as f64).sqrt();
    let rb = (b.max_radius_sq.max(0) as f64).sqrt();
    let sum_r = ra + rb + 4.0;
    (crate::types::horiz_dist_sq(a.center, b.center) as f64) <= sum_r * sum_r
}

#[derive(Clone, Debug)]
pub struct PendingBastion {
    pub rift_id: u64,
    pub origin: [i32; 3],
    pub flavor: NetherFlavor,
    pub roll: u64,
    pub forced_chunks: Vec<[i32; 2]>,
    pub queued_tick: u64,
}

#[derive(Clone, Debug)]
pub struct PendingCitadel {
    pub rift_id: u64,
    pub center: [i32; 3],
    pub flavor: NetherFlavor,
    pub roll: u64,
    pub forced_chunks: Vec<[i32; 2]>,
    pub queued_tick: u64,
}

#[derive(Clone, Debug)]
pub struct PendingOutpost {
    pub rift_id: u64,
    pub center: [i32; 3],
    pub kind: OutpostKind,
    pub flavor: NetherFlavor,
    pub roll: u64,
    pub forced_chunks: Vec<[i32; 2]>,
    pub queued_tick: u64,
}

#[derive(Clone, Debug)]
pub struct PendingNetherFob {
    pub rift_id: u64,
    pub center_xz: [i32; 2],
    pub ow_y: i32,
    pub flavor: NetherFlavor,
    pub roll: u64,
    pub forced_chunks: Vec<[i32; 2]>,
    pub queued_tick: u64,
}

pub struct RiftState {
    pub world_epoch: Option<WorldEpoch>,
    pub world_dir: Option<PathBuf>,
    pub seed: u64,
    pub next_id: u64,
    pub tick: u64,
    pub paused: bool,
    pub spread_budget: usize,
    pub dirty: bool,
    pub rifts: Vec<RiftSource>,
    pub bastions: Vec<BastionSite>,
    pub outposts: Vec<OutpostSite>,
    pub citadels: Vec<[i32; 3]>,
    /// Rift IDs whose Nether-side Forward Operating Base has already been linked/built.
    pub nether_fob_rifts: HashSet<u64>,
    /// Built Nether Forward Operating Base centers in `minecraft:the_nether`.
    pub nether_fobs: Vec<[i32; 3]>,
    pub corrupted_quart_cols: HashSet<[i32; 2]>,
    pub corrupted_quart_cols_list: Vec<[i32; 2]>,
    pub corrupted_quart_index: HashMap<[i32; 2], usize>,
    /// Tracks Overworld mobs buffed as Nether-Touched so loot & flames persist.
    pub nether_touched_mobs: HashSet<u32>,
    /// Tracks known living Nether predator entity IDs so `living_death` recognizes attackers cleanly.
    pub known_nether_mobs: HashSet<u32>,
    /// Tracks Magma Cubes that already received their melee attack goal.
    pub magma_melee_ready: HashSet<u32>,
    /// Tracks living Sovereign/Citadel Wither entity IDs for friendly-fire ceasefire checks.
    pub known_withers: HashSet<u32>,
    /// Tracks allied Citadel/Outpost garrison entity IDs (`rift_bleed:nether_ally`).
    pub citadel_guards: HashSet<u32>,
    /// Tracks Piglins/Brutes/Hoglins already stabilized with `IsImmuneToZombification: 1b`
    /// so vanilla Overworld zombification never triggers repeated conversion sounds.
    pub zombification_immune_mobs: HashSet<u32>,
    /// Cached vanilla `BlockState` handles for hot-loop block placement without registry lookups.
    pub block_state_cache: HashMap<&'static str, BlockState>,
    /// Active chunk tickets held by Rift Bleed via `ChunkRef::set_forced`.
    pub core_chunks: HashSet<[i32; 2]>,
    pub frontier_chunks: VecDeque<[i32; 2]>,
    pub regress_chunks: VecDeque<[i32; 2]>,
    pub pending_bastion: Option<PendingBastion>,
    pub bastion_release_chunks: Vec<[i32; 2]>,
    pub bastion_release_ttl: u8,
    pub pending_outpost: Option<PendingOutpost>,
    pub outpost_release_chunks: Vec<[i32; 2]>,
    pub outpost_release_ttl: u8,
    pub pending_citadel: Option<PendingCitadel>,
    pub citadel_release_chunks: Vec<[i32; 2]>,
    pub citadel_release_ttl: u8,
    pub pending_nether_fob: Option<PendingNetherFob>,
    pub nether_fob_release_chunks: Vec<[i32; 2]>,
    pub nether_fob_release_ttl: u8,
    /// Queues filled by read-only hooks and drained in `level_tick_post`.
    pub pending_portals: VecDeque<[i32; 3]>,
    pub pending_containment_checks: Vec<[i32; 3]>,
    pub pending_water_evap: VecDeque<[i32; 3]>,
    pub pending_water_evap_set: HashSet<[i32; 3]>,
    pub pending_frontier_columns: VecDeque<[i32; 2]>,
    pub water_sweep_cursor: usize,
    /// Active oceanic coastal berm columns queued for step-down / boiling erosion
    /// as the rift front pushes deeper out to sea.
    pub shore_columns: VecDeque<[i32; 2]>,
    pub shore_columns_set: HashSet<[i32; 2]>,
    /// Independent runtime vertical cursor `([x, z], next_y_down, min_y)` for each
    /// rift's simultaneous underground deepening pass, keyed by rift id.
    pub active_deepen_columns: HashMap<u64, ([i32; 2], i32, i32)>,
    /// Runtime priority deep-bore cursor & queue `(col, top_y, bottom_y, cave_band)` around
    /// loaded players / epicenter (`cave_band == i32::MIN` indicates a full surface-to-bedrock col).
    pub active_priority_deepen: HashMap<u64, ([i32; 2], i32, i32, i32)>,
    pub priority_deepen_cols: HashMap<u64, VecDeque<([i32; 2], i32, i32, i32)>>,
    pub session_deepened_cols: HashSet<[i32; 2]>,
    pub session_cave_band_cols: HashMap<[i32; 2], u32>,
    pub last_deepen_probe_tick: HashMap<u64, u64>,
    pub last_priority_focus: HashMap<u64, [i32; 3]>,
    pub needs_forced_chunk_sync: bool,
    /// Runtime fractional spread accumulators per rift so every corrupted block smoothly
    /// accelerates growth from a very slow crawl up to full speed.
    pub surface_budget_accum: HashMap<u64, f64>,
    pub deepen_budget_accum: HashMap<u64, f64>,
    /// Runtime queue of recently evaporated/melted water, ice, or snow positions
    /// for emitting throttled black smoke effects without saving to disk.
    pub pending_melt_fx: Vec<[i32; 3]>,
    pub last_melt_sound_tick: u64,
}

impl Default for RiftState {
    fn default() -> Self {
        Self::new()
    }
}

impl RiftState {
    pub fn new() -> Self {
        Self {
            world_epoch: None,
            world_dir: None,
            seed: 0x51F7_2630,
            next_id: 1,
            tick: 0,
            paused: false,
            spread_budget: 14,
            dirty: false,
            rifts: Vec::new(),
            bastions: Vec::new(),
            outposts: Vec::new(),
            citadels: Vec::new(),
            nether_fob_rifts: HashSet::new(),
            nether_fobs: Vec::new(),
            corrupted_quart_cols: HashSet::new(),
            corrupted_quart_cols_list: Vec::new(),
            corrupted_quart_index: HashMap::new(),
            nether_touched_mobs: HashSet::new(),
            known_nether_mobs: HashSet::new(),
            magma_melee_ready: HashSet::new(),
            known_withers: HashSet::new(),
            citadel_guards: HashSet::new(),
            zombification_immune_mobs: HashSet::new(),
            block_state_cache: HashMap::new(),
            core_chunks: HashSet::new(),
            frontier_chunks: VecDeque::new(),
            regress_chunks: VecDeque::new(),
            pending_bastion: None,
            bastion_release_chunks: Vec::new(),
            bastion_release_ttl: 0,
            pending_outpost: None,
            outpost_release_chunks: Vec::new(),
            outpost_release_ttl: 0,
            pending_citadel: None,
            citadel_release_chunks: Vec::new(),
            citadel_release_ttl: 0,
            pending_nether_fob: None,
            nether_fob_release_chunks: Vec::new(),
            nether_fob_release_ttl: 0,
            pending_portals: VecDeque::new(),
            pending_containment_checks: Vec::new(),
            pending_water_evap: VecDeque::new(),
            pending_water_evap_set: HashSet::new(),
            pending_frontier_columns: VecDeque::new(),
            water_sweep_cursor: 0,
            shore_columns: VecDeque::new(),
            shore_columns_set: HashSet::new(),
            active_deepen_columns: HashMap::new(),
            active_priority_deepen: HashMap::new(),
            priority_deepen_cols: HashMap::new(),
            session_deepened_cols: HashSet::new(),
            session_cave_band_cols: HashMap::new(),
            last_deepen_probe_tick: HashMap::new(),
            last_priority_focus: HashMap::new(),
            needs_forced_chunk_sync: true,
            surface_budget_accum: HashMap::new(),
            deepen_budget_accum: HashMap::new(),
            pending_melt_fx: Vec::new(),
            last_melt_sound_tick: 0,
        }
    }

    pub fn mark_session_col_deepened(&mut self, col: [i32; 2]) {
        if self.session_deepened_cols.len() >= 32_768 {
            self.session_deepened_cols.clear();
        }
        self.session_deepened_cols.insert(col);
    }

    pub fn has_session_cave_col_deepened(&self, col: [i32; 2], band: i32) -> bool {
        let bit = band.saturating_add(4);
        if !(0..=31).contains(&bit) {
            return false;
        }
        self.session_cave_band_cols
            .get(&col)
            .is_some_and(|&mask| (mask & (1u32 << bit)) != 0)
    }

    pub fn mark_session_cave_col_deepened(&mut self, col: [i32; 2], band: i32) {
        if self.session_cave_band_cols.len() >= 32_768 {
            self.session_cave_band_cols.clear();
        }
        let bit = band.saturating_add(4);
        if (0..=31).contains(&bit) {
            *self.session_cave_band_cols.entry(col).or_insert(0) |= 1u32 << bit;
        }
    }

    pub fn queue_melt_fx(&mut self, pos: [i32; 3]) {
        if self.pending_melt_fx.len() < 16 && !self.pending_melt_fx.contains(&pos) {
            self.pending_melt_fx.push(pos);
        }
    }

    pub fn queue_water_evap(&mut self, pos: [i32; 3]) {
        if self.pending_water_evap.len() < 4096 && self.pending_water_evap_set.insert(pos) {
            self.pending_water_evap.push_back(pos);
        }
    }

    pub fn pop_water_evap(&mut self) -> Option<[i32; 3]> {
        let pos = self.pending_water_evap.pop_front()?;
        self.pending_water_evap_set.remove(&pos);
        Some(pos)
    }

    pub fn queue_shore_col(&mut self, col: [i32; 2]) {
        if self.shore_columns.len() < 16_384 && self.shore_columns_set.insert(col) {
            self.shore_columns.push_back(col);
            self.dirty = true;
        }
    }

    pub fn pop_shore_col(&mut self) -> Option<[i32; 2]> {
        let col = self.shore_columns.pop_front()?;
        self.shore_columns_set.remove(&col);
        Some(col)
    }

    pub fn reset_for_world(&mut self, dir: Option<PathBuf>, seed: Option<i64>) {
        *self = Self::new();
        self.world_dir = dir;
        if let Some(s) = seed {
            self.seed = s as u64;
        }
        self.load_from_disk();
    }

    pub fn save_path(&self) -> Option<PathBuf> {
        self.world_dir.as_ref().map(|d| d.join(SAVE_FILE_NAME))
    }

    pub fn has_uncontained(&self) -> bool {
        self.rifts.iter().any(|r| r.is_actively_spreading())
    }

    pub fn has_regressing(&self) -> bool {
        self.rifts.iter().any(|r| r.regressing)
    }

    pub fn is_quart_corrupted(&self, block_pos: [i32; 3]) -> bool {
        let q = quart_coords(block_pos);
        self.corrupted_quart_cols.contains(&[q[0], q[2]])
    }

    pub fn mark_quart_corrupted(&mut self, block_pos: [i32; 3]) -> bool {
        let q = quart_coords(block_pos);
        let col = [q[0], q[2]];
        if self.corrupted_quart_cols.insert(col) {
            let idx = self.corrupted_quart_cols_list.len();
            self.corrupted_quart_cols_list.push(col);
            self.corrupted_quart_index.insert(col, idx);
            true
        } else {
            false
        }
    }

    pub fn unmark_quart_col(&mut self, x: i32, z: i32) {
        let qx = x >> 2;
        let qz = z >> 2;
        let col = [qx, qz];
        if !self.corrupted_quart_cols.remove(&col) {
            return;
        }
        if let Some(idx) = self.corrupted_quart_index.remove(&col) {
            let last_idx = self.corrupted_quart_cols_list.len() - 1;
            self.corrupted_quart_cols_list.swap_remove(idx);
            if idx < last_idx {
                let moved = self.corrupted_quart_cols_list[idx];
                self.corrupted_quart_index.insert(moved, idx);
            }
        }
        let bx = qx << 2;
        let bz = qz << 2;
        for ox in 0..4 {
            for oz in 0..4 {
                let p = [bx + ox, bz + oz];
                self.session_deepened_cols.remove(&p);
                self.session_cave_band_cols.remove(&p);
            }
        }
    }

    /// True if any *other* active (unpacified) Rift still reaches `(x, z)` so one
    /// purified portal's regression never erases another portal's live wasteland.
    pub fn other_active_rift_covers(&self, excluding_rift_id: u64, x: i32, z: i32) -> bool {
        self.rifts.iter().any(|r| {
            if r.id == excluding_rift_id || r.pacified {
                return false;
            }
            let r_limit = i64::from(r.ring_radius.max(16) + 4);
            crate::types::horiz_dist_sq(r.center, [x, 64, z]) <= r_limit * r_limit
        })
    }

    pub fn find_near_source_mut(&mut self, pos: [i32; 3], max_dist: i32) -> Option<&mut RiftSource> {
        let limit = i64::from(max_dist) * i64::from(max_dist);
        self.rifts
            .iter_mut()
            .filter(|r| crate::types::dist_sq(r.center, pos) <= limit)
            .min_by_key(|r| crate::types::dist_sq(r.center, pos))
    }

    /// Computes combined territory stats across all unpacified rifts whose
    /// corrupted circles touch or overlap with `self.rifts[seed_idx]`.
    /// Pacified rifts never donate blocks or Bastion credit to a new portal's cycle.
    pub fn connected_wasteland_for(&self, seed_idx: usize) -> ConnectedWasteland {
        if seed_idx >= self.rifts.len() || self.rifts[seed_idx].pacified {
            return ConnectedWasteland::default();
        }
        let mut visited = vec![false; self.rifts.len()];
        let mut queue = VecDeque::new();
        visited[seed_idx] = true;
        queue.push_back(seed_idx);

        let mut summary = ConnectedWasteland::default();
        while let Some(idx) = queue.pop_front() {
            let r = &self.rifts[idx];
            if r.pacified {
                continue;
            }
            summary.total_corrupted_blocks =
                summary.total_corrupted_blocks.saturating_add(r.corrupted_blocks);
            summary.total_area_radius_sq =
                summary.total_area_radius_sq.saturating_add(r.max_radius_sq);
            summary.max_deepen_radius = summary
                .max_deepen_radius
                .max(r.deepen_radius)
                .max(r.max_deepen_reached);
            summary.max_ring_radius = summary.max_ring_radius.max(r.ring_radius);
            summary.total_bastions_placed = summary
                .total_bastions_placed
                .saturating_add(r.bastions_placed);
            summary.total_outposts_placed = summary
                .total_outposts_placed
                .saturating_add(r.outposts_placed);
            summary.member_count += 1;

            for next in 0..self.rifts.len() {
                if !visited[next] && !self.rifts[next].pacified && rifts_touch(r, &self.rifts[next]) {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }
        summary
    }

    pub fn load_from_host(&mut self, host: &dyn ServerHost) {
        if let Ok(path) = ConfigPath::new(SAVE_FILE_NAME)
            && let Ok(bytes) = host.world_data_files().read(&path)
            && let Ok(doc) = serde_json::from_slice::<Value>(&bytes)
        {
            self.apply_json(&doc);
            return;
        }
        self.load_from_disk();
    }

    pub fn save_to_host(&mut self, host: &dyn ServerHost) {
        if !self.dirty {
            return;
        }
        let json = self.to_json();
        if let Ok(path) = ConfigPath::new(SAVE_FILE_NAME)
            && let Ok(bytes) = serde_json::to_vec(&json)
            && host.world_data_files().write(&path, &bytes).is_ok()
        {
            self.dirty = false;
            return;
        }
        self.save_to_disk();
    }

    pub fn load_from_disk(&mut self) {
        let Some(path) = self.save_path() else {
            return;
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        self.apply_json(&doc);
    }

    pub fn save_to_disk(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.save_path() else {
            return;
        };
        if let Some(parent) = Path::new(&path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = self.to_json();
        if let Ok(text) = serde_json::to_string(&json)
            && fs::write(path, text).is_ok()
        {
            self.dirty = false;
        }
    }

    pub fn to_json(&self) -> Value {
        let mut root = Map::new();
        root.insert("version".into(), Value::from(10));
        root.insert("seed".into(), Value::from(self.seed));
        root.insert("next_id".into(), Value::from(self.next_id));
        root.insert("paused".into(), Value::from(self.paused));
        root.insert("spread_budget".into(), Value::from(self.spread_budget as u64));

        let rifts: Vec<Value> = self
            .rifts
            .iter()
            .map(|r| {
                let mut obj = Map::new();
                obj.insert("id".into(), Value::from(r.id));
                obj.insert("center".into(), pos_to_json(r.center));
                obj.insert("active_portal".into(), Value::from(r.active_portal));
                obj.insert("contained".into(), Value::from(r.contained));
                obj.insert("flavor".into(), Value::from(r.flavor.to_u8()));
                obj.insert("age_ticks".into(), Value::from(r.age_ticks));
                obj.insert("corrupted_blocks".into(), Value::from(r.corrupted_blocks));
                obj.insert("max_radius_sq".into(), Value::from(r.max_radius_sq));
                obj.insert("ring_radius".into(), Value::from(r.ring_radius));
                obj.insert("deepen_radius".into(), Value::from(r.deepen_radius));
                obj.insert("max_deepen_reached".into(), Value::from(r.max_deepen_reached));
                obj.insert("bastions_placed".into(), Value::from(r.bastions_placed));
                obj.insert("outposts_placed".into(), Value::from(r.outposts_placed));
                obj.insert("citadel_spawned".into(), Value::from(r.citadel_spawned));
                if let Some(u) = r.boss_uuid {
                    obj.insert("boss_uuid".into(), Value::String(format!("{u:032x}")));
                }
                obj.insert("pacified".into(), Value::from(r.pacified));
                obj.insert("regressing".into(), Value::from(r.regressing));
                obj.insert("regress_radius".into(), Value::from(r.regress_radius));
                obj.insert(
                    "ring_columns".into(),
                    flat_cols_iter_to_json(r.ring_columns.iter().copied()),
                );
                obj.insert(
                    "deepen_columns".into(),
                    flat_cols_iter_to_json(r.deepen_columns.iter().copied()),
                );
                obj.insert(
                    "regress_columns".into(),
                    flat_cols_iter_to_json(r.regress_columns.iter().copied()),
                );
                if let Some(([x, z], y, min_y)) = r.active_column {
                    obj.insert(
                        "active_column".into(),
                        Value::Array(vec![
                            Value::from(x),
                            Value::from(z),
                            Value::from(y),
                            Value::from(min_y),
                        ]),
                    );
                }
                if let Some(([x, z], y, min_y)) = r.regress_active_column {
                    obj.insert(
                        "regress_active_column".into(),
                        Value::Array(vec![
                            Value::from(x),
                            Value::from(z),
                            Value::from(y),
                            Value::from(min_y),
                        ]),
                    );
                }
                Value::Object(obj)
            })
            .collect();
        root.insert("rifts".into(), Value::Array(rifts));

        let bastions: Vec<Value> = self
            .bastions
            .iter()
            .map(|b| {
                let mut obj = Map::new();
                obj.insert("center".into(), pos_to_json(b.center));
                obj.insert("flavor".into(), Value::from(b.flavor.to_u8()));
                Value::Object(obj)
            })
            .collect();
        root.insert("bastions".into(), Value::Array(bastions));

        let citadels: Vec<Value> = self.citadels.iter().copied().map(pos_to_json).collect();
        root.insert("citadels".into(), Value::Array(citadels));

        let outposts: Vec<Value> = self
            .outposts
            .iter()
            .map(|o| {
                let mut obj = Map::new();
                obj.insert("center".into(), pos_to_json(o.center));
                obj.insert("kind".into(), Value::from(o.kind.to_u8()));
                obj.insert("flavor".into(), Value::from(o.flavor.to_u8()));
                Value::Object(obj)
            })
            .collect();
        root.insert("outposts".into(), Value::Array(outposts));

        root.insert(
            "corrupted_quart_cols".into(),
            flat_cols_iter_to_json(self.corrupted_quart_cols_list.iter().copied()),
        );
        root.insert(
            "shore_columns".into(),
            flat_cols_iter_to_json(self.shore_columns.iter().copied()),
        );
        root.insert(
            "nether_fob_rifts".into(),
            Value::Array(
                self.nether_fob_rifts
                    .iter()
                    .copied()
                    .map(Value::from)
                    .collect(),
            ),
        );
        root.insert(
            "nether_fobs".into(),
            Value::Array(self.nether_fobs.iter().copied().map(pos_to_json).collect()),
        );

        Value::Object(root)
    }

    pub fn apply_json(&mut self, doc: &Value) {
        if let Some(s) = doc.get("seed").and_then(|v| v.as_u64()) {
            self.seed = s;
        }
        if let Some(n) = doc.get("next_id").and_then(|v| v.as_u64()) {
            self.next_id = n.max(1);
        }
        if let Some(p) = doc.get("paused").and_then(|v| v.as_bool()) {
            self.paused = p;
        }
        let version = doc.get("version").and_then(|v| v.as_u64()).unwrap_or(1);
        if let Some(b) = doc.get("spread_budget").and_then(|v| v.as_u64()) {
            let upgraded = if version < 5 && matches!(b, 28 | 140 | 420) {
                14
            } else {
                b
            };
            self.spread_budget = (upgraded as usize).clamp(4, 1200);
        }

        self.rifts.clear();
        self.magma_melee_ready.clear();
        self.active_deepen_columns.clear();
        self.active_priority_deepen.clear();
        self.priority_deepen_cols.clear();
        self.session_deepened_cols.clear();
        self.session_cave_band_cols.clear();
        self.last_deepen_probe_tick.clear();
        self.surface_budget_accum.clear();
        self.deepen_budget_accum.clear();
        if let Some(arr) = doc.get("rifts").and_then(|v| v.as_array()) {
            for item in arr {
                let Some(center) = item.get("center").and_then(pos_from_json) else {
                    continue;
                };
                let id = item.get("id").and_then(|v| v.as_u64()).unwrap_or_else(|| {
                    let id = self.next_id;
                    self.next_id += 1;
                    id
                });
                let max_radius_sq = item
                    .get("max_radius_sq")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let inferred_r = ((max_radius_sq as f64).sqrt() as i32).max(1);
                let ring_radius = item
                    .get("ring_radius")
                    .and_then(|v| v.as_i64())
                    .map(|v| (v as i32).max(1))
                    .unwrap_or(inferred_r);
                let mut ring_columns: Vec<[i32; 2]> = item
                    .get("ring_columns")
                    .map(cols_from_json_any)
                    .unwrap_or_default();
                if ring_columns.is_empty() {
                    ring_columns = build_ring_columns(center[0], center[2], ring_radius);
                }
                let is_pacified = item
                    .get("pacified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let raw_deepen_radius = item
                    .get("deepen_radius")
                    .and_then(|v| v.as_i64())
                    .map(|v| (v as i32).max(1))
                    .unwrap_or(1);
                let corrupted_blocks = item
                    .get("corrupted_blocks")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let bastions_placed = item
                    .get("bastions_placed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let (deepen_radius, deepen_columns) = if version < 9 && !is_pacified {
                    // Older saves either stopped shallow (~10 blocks), stalled on low deepen
                    // throughput, or bypassed inner rings; re-sweep active rifts from ring 1
                    // all the way down to Bedrock (-63).
                    self.dirty = true;
                    (1, build_ring_columns(center[0], center[2], 1))
                } else {
                    let dc: Vec<[i32; 2]> = item
                        .get("deepen_columns")
                        .map(cols_from_json_any)
                        .unwrap_or_default();
                    (raw_deepen_radius, dc)
                };
                let active_column = item.get("active_column").and_then(active_col_from_json);
                let citadel_spawned = item
                    .get("citadel_spawned")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let mut max_deepen_reached = item
                    .get("max_deepen_reached")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32)
                    .unwrap_or(raw_deepen_radius)
                    .max(raw_deepen_radius)
                    .max(deepen_radius);
                // Worlds saved at v8->v9 reset `deepen_radius` to 1 without preserving prior
                // deepen progress on already-huge rifts; restore earned depth so Bastions,
                // Citadels, and Nether FOBs unlock immediately.
                if version <= 9
                    && !is_pacified
                    && (corrupted_blocks >= 160_000 || bastions_placed > 0 || citadel_spawned)
                {
                    let earned = (ring_radius / 2).clamp(24, 48).min(ring_radius.max(1));
                    if max_deepen_reached < earned {
                        max_deepen_reached = earned;
                        self.dirty = true;
                    }
                }
                let boss_uuid = item
                    .get("boss_uuid")
                    .and_then(|v| v.as_str())
                    .and_then(|s| u128::from_str_radix(s, 16).ok());
                let pacified = item
                    .get("pacified")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let regressing = item
                    .get("regressing")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let regress_radius = item
                    .get("regress_radius")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32)
                    .unwrap_or(0);
                let regress_columns: Vec<[i32; 2]> = item
                    .get("regress_columns")
                    .map(cols_from_json_any)
                    .unwrap_or_default();
                let regress_active_column = item
                    .get("regress_active_column")
                    .and_then(active_col_from_json);

                self.rifts.push(RiftSource {
                    id,
                    center,
                    active_portal: item
                        .get("active_portal")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                    contained: item
                        .get("contained")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    flavor: NetherFlavor::from_u8(
                        item.get("flavor").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
                    ),
                    age_ticks: item.get("age_ticks").and_then(|v| v.as_u64()).unwrap_or(0),
                    corrupted_blocks,
                    max_radius_sq,
                    ring_radius,
                    ring_columns,
                    active_column,
                    deepen_radius,
                    deepen_columns,
                    max_deepen_reached,
                    bastions_placed,
                    outposts_placed: item
                        .get("outposts_placed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    citadel_spawned,
                    boss_uuid,
                    pacified,
                    regressing,
                    regress_radius,
                    regress_columns,
                    regress_active_column,
                });
            }
        }

        self.bastions.clear();
        if let Some(arr) = doc.get("bastions").and_then(|v| v.as_array()) {
            for item in arr {
                let Some(center) = item.get("center").and_then(pos_from_json) else {
                    continue;
                };
                let flavor = NetherFlavor::from_u8(
                    item.get("flavor").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
                );
                self.bastions.push(BastionSite { center, flavor });
            }
        }

        self.citadels.clear();
        if let Some(arr) = doc.get("citadels").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(center) = pos_from_json(item) {
                    self.citadels.push(center);
                }
            }
        }

        self.outposts.clear();
        if let Some(arr) = doc.get("outposts").and_then(|v| v.as_array()) {
            for item in arr {
                let Some(center) = item.get("center").and_then(pos_from_json) else {
                    continue;
                };
                let kind = OutpostKind::from_u8(
                    item.get("kind").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
                );
                let flavor = NetherFlavor::from_u8(
                    item.get("flavor").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
                );
                self.outposts.push(OutpostSite {
                    center,
                    kind,
                    flavor,
                });
            }
        }

        self.corrupted_quart_cols.clear();
        self.corrupted_quart_cols_list.clear();
        self.corrupted_quart_index.clear();
        if let Some(arr) = doc.get("corrupted_quarts").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(q) = pos_from_json(item) {
                    let c = [q[0], q[2]];
                    if self.corrupted_quart_cols.insert(c) {
                        let idx = self.corrupted_quart_cols_list.len();
                        self.corrupted_quart_cols_list.push(c);
                        self.corrupted_quart_index.insert(c, idx);
                    }
                }
            }
        }
        if let Some(val) = doc.get("corrupted_quart_cols") {
            for c in cols_from_json_any(val) {
                if self.corrupted_quart_cols.insert(c) {
                    let idx = self.corrupted_quart_cols_list.len();
                    self.corrupted_quart_cols_list.push(c);
                    self.corrupted_quart_index.insert(c, idx);
                }
            }
        }
        self.shore_columns.clear();
        self.shore_columns_set.clear();
        if let Some(val) = doc.get("shore_columns") {
            for c in cols_from_json_any(val) {
                if self.shore_columns_set.insert(c) {
                    self.shore_columns.push_back(c);
                }
            }
        }
        self.nether_fob_rifts.clear();
        if let Some(arr) = doc.get("nether_fob_rifts").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(id) = item.as_u64() {
                    self.nether_fob_rifts.insert(id);
                }
            }
        }
        self.nether_fobs.clear();
        if let Some(arr) = doc.get("nether_fobs").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(center) = pos_from_json(item) {
                    self.nether_fobs.push(center);
                }
            }
        }
        self.dirty = false;
    }
}

fn pos_to_json(pos: [i32; 3]) -> Value {
    Value::Array(vec![
        Value::from(pos[0]),
        Value::from(pos[1]),
        Value::from(pos[2]),
    ])
}

fn pos_from_json(val: &Value) -> Option<[i32; 3]> {
    let arr = val.as_array()?;
    if arr.len() != 3 {
        return None;
    }
    Some([
        arr[0].as_i64()? as i32,
        arr[1].as_i64()? as i32,
        arr[2].as_i64()? as i32,
    ])
}

fn flat_cols_iter_to_json(iter: impl Iterator<Item = [i32; 2]>) -> Value {
    let (_, upper) = iter.size_hint();
    let mut vals = Vec::with_capacity(upper.unwrap_or(0) * 2);
    for col in iter {
        vals.push(Value::from(col[0]));
        vals.push(Value::from(col[1]));
    }
    Value::Array(vals)
}

fn col_from_json(val: &Value) -> Option<[i32; 2]> {
    let arr = val.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    Some([arr[0].as_i64()? as i32, arr[1].as_i64()? as i32])
}

fn cols_from_json_any(val: &Value) -> Vec<[i32; 2]> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    if arr.first().is_some_and(|v| v.is_i64()) {
        let mut out = Vec::with_capacity(arr.len() / 2);
        for pair in arr.chunks_exact(2) {
            if let (Some(x), Some(z)) = (pair[0].as_i64(), pair[1].as_i64()) {
                out.push([x as i32, z as i32]);
            }
        }
        return out;
    }
    arr.iter().filter_map(col_from_json).collect()
}

fn active_col_from_json(val: &Value) -> Option<([i32; 2], i32, i32)> {
    let arr = val.as_array()?;
    if arr.len() != 4 {
        return None;
    }
    Some((
        [arr[0].as_i64()? as i32, arr[1].as_i64()? as i32],
        arr[2].as_i64()? as i32,
        arr[3].as_i64()? as i32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_preserves_rifts_bastions_and_biomes() {
        let mut s = RiftState::new();
        s.seed = 4242;
        s.paused = true;
        s.spread_budget = 180;
        s.rifts.push(RiftSource {
            id: 3,
            center: [120, 64, -88],
            active_portal: false,
            contained: true,
            flavor: NetherFlavor::SoulSandValley,
            age_ticks: 900,
            corrupted_blocks: 5432,
            max_radius_sq: 3600,
            ring_radius: 14,
            ring_columns: vec![[125, -80], [130, -85]],
            active_column: Some(([121, -88], 12, -63)),
            deepen_radius: 6,
            deepen_columns: vec![[122, -88]],
            max_deepen_reached: 24,
            bastions_placed: 1,
            outposts_placed: 2,
            citadel_spawned: true,
            boss_uuid: Some(0x1234_5678_9abc_def0_1122_3344_5566_7788),
            pacified: false,
            regressing: true,
            regress_radius: 12,
            regress_columns: vec![[128, -80]],
            regress_active_column: Some(([120, -88], 40, -63)),
        });
        s.citadels.push([120, 64, -88]);
        s.nether_fob_rifts.insert(1);
        s.nether_fobs.push([15, 52, -11]);
        s.outposts.push(OutpostSite {
            center: [185, 68, -20],
            kind: OutpostKind::HoglinVillage,
            flavor: NetherFlavor::CrimsonForest,
        });
        s.bastions.push(BastionSite {
            center: [210, 70, -40],
            flavor: NetherFlavor::BasaltDeltas,
        });
        s.mark_quart_corrupted([120, 64, -88]);

        let json = s.to_json();
        let mut loaded = RiftState::new();
        loaded.apply_json(&json);

        assert_eq!(loaded.seed, 4242);
        assert!(loaded.paused);
        assert_eq!(loaded.spread_budget, 180);
        assert_eq!(loaded.rifts.len(), 1);
        assert!(!loaded.rifts[0].active_portal);
        assert!(loaded.rifts[0].contained);
        assert!(loaded.rifts[0].citadel_spawned);
        assert_eq!(
            loaded.rifts[0].boss_uuid,
            Some(0x1234_5678_9abc_def0_1122_3344_5566_7788)
        );
        assert!(!loaded.rifts[0].pacified);
        assert!(loaded.rifts[0].regressing);
        assert_eq!(loaded.rifts[0].regress_radius, 12);
        assert_eq!(loaded.citadels, vec![[120, 64, -88]]);
        assert!(loaded.nether_fob_rifts.contains(&1));
        assert_eq!(loaded.nether_fobs, vec![[15, 52, -11]]);
        assert_eq!(loaded.outposts.len(), 1);
        assert_eq!(loaded.outposts[0].kind, OutpostKind::HoglinVillage);
        assert_eq!(loaded.rifts[0].max_deepen_reached, 24);
        assert_eq!(loaded.rifts[0].outposts_placed, 2);
        assert_eq!(loaded.rifts[0].flavor, NetherFlavor::SoulSandValley);
        assert_eq!(loaded.rifts[0].ring_radius, 14);
        assert_eq!(loaded.rifts[0].active_column, Some(([121, -88], 12, -63)));
        assert_eq!(
            loaded.rifts[0].regress_active_column,
            Some(([120, -88], 40, -63))
        );
        assert_eq!(loaded.bastions.len(), 1);
        assert_eq!(loaded.bastions[0].center, [210, 70, -40]);
        assert_eq!(loaded.rifts[0].ring_columns, vec![[125, -80], [130, -85]]);
        assert_eq!(
            cols_from_json_any(&serde_json::json!([[10, 20], [-5, 8]])),
            vec![[10, 20], [-5, 8]]
        );
        assert!(loaded.is_quart_corrupted([120, 64, -88]));
        // Full vertical column is covered too.
        assert!(loaded.is_quart_corrupted([120, 180, -88]));
        assert!(loaded.is_quart_corrupted([120, -52, -88]));

        // Legacy v1 saves with spread_budget=28 migrate cleanly down to calm 14 ops/tick,
        // while explicit custom speeds on v5 stay preserved.
        let legacy = serde_json::json!({ "version": 1, "spread_budget": 28 });
        loaded.apply_json(&legacy);
        assert_eq!(loaded.spread_budget, 14);

        // Pre-v9 saves reset active rifts' deepen_radius back to ring 1 so un-deepened stone
        // around existing epicenters re-sweeps all the way down to Bedrock (-63).
        let v8_shallow = serde_json::json!({
            "version": 8,
            "rifts": [{
                "id": 7,
                "center": [0, 64, 0],
                "ring_radius": 80,
                "deepen_radius": 45,
                "pacified": false
            }]
        });
        loaded.apply_json(&v8_shallow);
        assert_eq!(loaded.rifts.len(), 1);
        assert_eq!(loaded.rifts[0].deepen_radius, 1);

        // Huge v9 worlds whose max_deepen_reached stayed stuck at 1 after v8->v9 reset
        // recover earned depth immediately so Bastions/Citadels/FOBs aren't permanently starved.
        let v9_stalled = serde_json::json!({
            "version": 9,
            "rifts": [{
                "id": 9,
                "center": [0, 64, 0],
                "corrupted_blocks": 700_000,
                "ring_radius": 140,
                "deepen_radius": 1,
                "max_deepen_reached": 1,
                "pacified": false
            }]
        });
        loaded.apply_json(&v9_stalled);
        assert!(loaded.rifts[0].max_deepen_reached >= 40);
    }

    #[test]
    fn default_state_has_nonzero_spread_budget_and_seed() {
        let d = RiftState::default();
        assert_eq!(d.spread_budget, 14);
        assert_eq!(d.next_id, 1);
        assert_ne!(d.seed, 0);
    }

    #[test]
    fn quart_index_and_cave_band_bitmask_unmark_cleanly_in_constant_time() {
        let mut s = RiftState::new();
        assert!(s.mark_quart_corrupted([0, 64, 0]));
        assert!(s.mark_quart_corrupted([16, 64, 0]));
        assert!(s.mark_quart_corrupted([32, 64, 0]));
        assert_eq!(s.corrupted_quart_cols_list.len(), 3);

        s.mark_session_col_deepened([17, 2]);
        s.mark_session_cave_col_deepened([17, 2], -2);
        s.mark_session_cave_col_deepened([17, 2], 3);
        assert!(s.has_session_cave_col_deepened([17, 2], -2));
        assert!(s.has_session_cave_col_deepened([17, 2], 3));
        assert!(!s.has_session_cave_col_deepened([17, 2], 1));

        // Remove middle quart -> swapped element updates its index & cave caches clear cleanly.
        s.unmark_quart_col(16, 0);
        assert!(!s.is_quart_corrupted([16, 64, 0]));
        assert!(s.is_quart_corrupted([0, 64, 0]));
        assert!(s.is_quart_corrupted([32, 64, 0]));
        assert_eq!(s.corrupted_quart_cols_list.len(), 2);
        assert!(!s.session_deepened_cols.contains(&[17, 2]));
        assert!(!s.has_session_cave_col_deepened([17, 2], -2));

        s.unmark_quart_col(32, 0);
        s.unmark_quart_col(0, 0);
        assert!(s.corrupted_quart_cols_list.is_empty());
        assert!(s.corrupted_quart_index.is_empty());
    }
}
