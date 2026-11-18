use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct BridgeState {
    pending_biomes: Vec<([i32; 3], u8)>,
    pending_fast_lava: Vec<([i32; 3], String)>,
    pending_mob_buffs: Vec<(u32, i32, i32, i32)>,
    pending_mob_targets: Vec<(u32, u32)>,
    pending_bastions: Vec<(String, [i32; 3], u8)>,
    corrupted_quart_cells: HashSet<(i32, i32, i32)>,
    corrupted_quart_columns: HashSet<(i32, i32)>,
}

fn state() -> &'static Mutex<BridgeState> {
    static STATE: OnceLock<Mutex<BridgeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(BridgeState::default()))
}

pub fn reset_world_state() {
    if let Ok(mut guard) = state().lock() {
        guard.pending_biomes.clear();
        guard.pending_fast_lava.clear();
        guard.pending_mob_buffs.clear();
        guard.pending_mob_targets.clear();
        guard.pending_bastions.clear();
        guard.corrupted_quart_cells.clear();
        guard.corrupted_quart_columns.clear();
    }
}

pub fn record_quart_cells(cells: &[[i32; 3]]) {
    if cells.is_empty() {
        return;
    }
    if let Ok(mut guard) = state().lock() {
        for &[qx, qy, qz] in cells {
            guard.corrupted_quart_cells.insert((qx, qy, qz));
            guard.corrupted_quart_columns.insert((qx, qz));
        }
    }
}

pub fn is_quart_corrupted(qx: i32, qy: i32, qz: i32) -> bool {
    state()
        .lock()
        .map(|g| g.corrupted_quart_cells.contains(&(qx, qy, qz)) || g.corrupted_quart_columns.contains(&(qx, qz)))
        .unwrap_or(false)
}

pub fn is_quart_col_corrupted(qx: i32, qz: i32) -> bool {
    state()
        .lock()
        .map(|g| g.corrupted_quart_columns.contains(&(qx, qz)))
        .unwrap_or(false)
}

pub fn enqueue_biomes(entries: &[([i32; 3], u8)]) {
    if entries.is_empty() {
        return;
    }
    if let Ok(mut guard) = state().lock() {
        guard.pending_biomes.extend_from_slice(entries);
    }
}

pub fn drain_biomes() -> Vec<([i32; 3], u8)> {
    state()
        .lock()
        .map(|mut g| std::mem::take(&mut g.pending_biomes))
        .unwrap_or_default()
}

pub fn enqueue_fast_lava(pos: [i32; 3], fluid: String) {
    if let Ok(mut guard) = state().lock() {
        guard.pending_fast_lava.push((pos, fluid));
    }
}

pub fn drain_fast_lava() -> Vec<([i32; 3], String)> {
    state()
        .lock()
        .map(|mut g| std::mem::take(&mut g.pending_fast_lava))
        .unwrap_or_default()
}

pub fn enqueue_mob_buff(entity_id: u32, fire_res_ticks: i32, strength_ticks: i32, strength_amp: i32) {
    if let Ok(mut guard) = state().lock() {
        guard
            .pending_mob_buffs
            .push((entity_id, fire_res_ticks, strength_ticks, strength_amp));
    }
}

pub fn drain_mob_buffs() -> Vec<(u32, i32, i32, i32)> {
    state()
        .lock()
        .map(|mut g| std::mem::take(&mut g.pending_mob_buffs))
        .unwrap_or_default()
}

pub fn enqueue_mob_targets(pairs: &[(u32, u32)]) {
    if pairs.is_empty() {
        return;
    }
    if let Ok(mut guard) = state().lock() {
        guard.pending_mob_targets.extend_from_slice(pairs);
    }
}

pub fn drain_mob_targets() -> Vec<(u32, u32)> {
    state()
        .lock()
        .map(|mut g| std::mem::take(&mut g.pending_mob_targets))
        .unwrap_or_default()
}

pub fn enqueue_bastion_piece(template_id: String, pos: [i32; 3], rotation: u8) {
    if let Ok(mut guard) = state().lock() {
        guard.pending_bastions.push((template_id, pos, rotation));
    }
}

pub fn drain_bastion_pieces() -> Vec<(String, [i32; 3], u8)> {
    state()
        .lock()
        .map(|mut g| std::mem::take(&mut g.pending_bastions))
        .unwrap_or_default()
}
