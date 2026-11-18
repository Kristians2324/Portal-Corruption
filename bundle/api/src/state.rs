use crate::types::{BastionSite, NetherFlavor, RiftSource, build_ring_columns, quart_coords};
use horizon_client::mod_api::serde_json::{self, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const SAVE_FILE_NAME: &str = "rift_bleed.json";

#[derive(Default)]
pub struct RiftState {
    pub world_dir: Option<PathBuf>,
    pub seed: u64,
    pub next_id: u64,
    pub tick: u64,
    pub paused: bool,
    pub spread_budget: usize,
    pub dirty: bool,
    pub rifts: Vec<RiftSource>,
    pub bastions: Vec<BastionSite>,
    pub corrupted_quarts: HashSet<[i32; 3]>,
    pub corrupted_quart_cols: HashSet<[i32; 2]>,
    /// Tracks Overworld mobs buffed as Nether-Touched so loot & flames persist.
    pub nether_touched_mobs: HashSet<u32>,
    /// Tracks known living Nether predator entity IDs so `living_death` recognizes attackers cleanly.
    pub known_nether_mobs: HashSet<u32>,
    /// Active chunk tickets held by Rift Bleed (`chunk_load_request` / `chunk_load_release`).
    pub core_tickets: HashSet<[i32; 2]>,
    pub frontier_tickets: VecDeque<[i32; 2]>,
    /// Queues filled by read-only hooks and drained in `level_tick_post`.
    pub pending_portals: VecDeque<[i32; 3]>,
    pub pending_containment_checks: Vec<[i32; 3]>,
    pub pending_water_evap: VecDeque<[i32; 3]>,
    pub pending_water_evap_set: HashSet<[i32; 3]>,
    pub pending_frontier_columns: VecDeque<[i32; 2]>,
    pub water_sweep_cursor: usize,
}

impl RiftState {
    pub fn new() -> Self {
        Self {
            world_dir: None,
            seed: 0x51F7_2630,
            next_id: 1,
            tick: 0,
            paused: false,
            spread_budget: 14,
            dirty: false,
            rifts: Vec::new(),
            bastions: Vec::new(),
            corrupted_quarts: HashSet::new(),
            corrupted_quart_cols: HashSet::new(),
            nether_touched_mobs: HashSet::new(),
            known_nether_mobs: HashSet::new(),
            core_tickets: HashSet::new(),
            frontier_tickets: VecDeque::new(),
            pending_portals: VecDeque::new(),
            pending_containment_checks: Vec::new(),
            pending_water_evap: VecDeque::new(),
            pending_water_evap_set: HashSet::new(),
            pending_frontier_columns: VecDeque::new(),
            water_sweep_cursor: 0,
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
        self.rifts.iter().any(|r| !r.contained)
    }

    pub fn is_quart_corrupted(&self, block_pos: [i32; 3]) -> bool {
        let q = quart_coords(block_pos);
        self.corrupted_quarts.contains(&q) || self.corrupted_quart_cols.contains(&[q[0], q[2]])
    }

    pub fn mark_quart_corrupted(&mut self, block_pos: [i32; 3]) -> bool {
        let q = quart_coords(block_pos);
        self.corrupted_quarts.insert(q);
        self.corrupted_quart_cols.insert([q[0], q[2]])
    }

    pub fn find_near_source_mut(&mut self, pos: [i32; 3], max_dist: i32) -> Option<&mut RiftSource> {
        let limit = i64::from(max_dist) * i64::from(max_dist);
        self.rifts
            .iter_mut()
            .filter(|r| crate::types::dist_sq(r.center, pos) <= limit)
            .min_by_key(|r| crate::types::dist_sq(r.center, pos))
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
        root.insert("version".into(), Value::from(4));
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
                obj.insert("bastions_placed".into(), Value::from(r.bastions_placed));
                let ring_cols: Vec<Value> = r.ring_columns.iter().copied().map(col_to_json).collect();
                obj.insert("ring_columns".into(), Value::Array(ring_cols));
                let deep_cols: Vec<Value> =
                    r.deepen_columns.iter().copied().map(col_to_json).collect();
                obj.insert("deepen_columns".into(), Value::Array(deep_cols));
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

        let cols: Vec<Value> = self
            .corrupted_quart_cols
            .iter()
            .copied()
            .map(col_to_json)
            .collect();
        root.insert("corrupted_quart_cols".into(), Value::Array(cols));

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
            let upgraded = if version < 4 && matches!(b, 140 | 420) {
                14
            } else {
                b
            };
            self.spread_budget = (upgraded as usize).clamp(4, 1200);
        }

        self.rifts.clear();
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
                    .and_then(|v| v.as_array())
                    .map(|list| list.iter().filter_map(col_from_json).collect())
                    .unwrap_or_default();
                if ring_columns.is_empty() {
                    ring_columns = build_ring_columns(center[0], center[2], ring_radius);
                }
                let deepen_radius = item
                    .get("deepen_radius")
                    .and_then(|v| v.as_i64())
                    .map(|v| (v as i32).max(1))
                    .unwrap_or(1);
                let deepen_columns: Vec<[i32; 2]> = item
                    .get("deepen_columns")
                    .and_then(|v| v.as_array())
                    .map(|list| list.iter().filter_map(col_from_json).collect())
                    .unwrap_or_default();
                let active_column = item.get("active_column").and_then(active_col_from_json);

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
                    corrupted_blocks: item
                        .get("corrupted_blocks")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    max_radius_sq,
                    ring_radius,
                    ring_columns,
                    active_column,
                    deepen_radius,
                    deepen_columns,
                    bastions_placed: item
                        .get("bastions_placed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
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

        self.corrupted_quarts.clear();
        self.corrupted_quart_cols.clear();
        if let Some(arr) = doc.get("corrupted_quarts").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(q) = pos_from_json(item) {
                    self.corrupted_quarts.insert(q);
                    self.corrupted_quart_cols.insert([q[0], q[2]]);
                }
            }
        }
        if let Some(arr) = doc.get("corrupted_quart_cols").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(c) = col_from_json(item) {
                    self.corrupted_quart_cols.insert(c);
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

fn col_to_json(col: [i32; 2]) -> Value {
    Value::Array(vec![Value::from(col[0]), Value::from(col[1])])
}

fn col_from_json(val: &Value) -> Option<[i32; 2]> {
    let arr = val.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    Some([arr[0].as_i64()? as i32, arr[1].as_i64()? as i32])
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
            bastions_placed: 1,
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
        assert_eq!(loaded.rifts[0].flavor, NetherFlavor::SoulSandValley);
        assert_eq!(loaded.rifts[0].ring_radius, 14);
        assert_eq!(loaded.rifts[0].active_column, Some(([121, -88], 12, -63)));
        assert_eq!(loaded.bastions.len(), 1);
        assert_eq!(loaded.bastions[0].center, [210, 70, -40]);
        assert!(loaded.is_quart_corrupted([120, 64, -88]));
        // Full vertical column is covered too.
        assert!(loaded.is_quart_corrupted([120, 180, -88]));
        assert!(loaded.is_quart_corrupted([120, -52, -88]));
    }
}
