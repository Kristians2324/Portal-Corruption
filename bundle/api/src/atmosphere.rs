use crate::types::{NetherFlavor, OVERWORLD, quart_coords, spatial_hash};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    ComputeFogColorEvent, FogType, Host, Music, MusicEvent, ParticleOptions, ParticleType,
    RenderFogEvent,
};

const CLOUD_FADE_START_Y: f64 = 188.0;
const CLOUD_FADE_END_Y: f64 = 222.0;

#[derive(Default)]
pub struct ClientAtmosphere {
    pub smog_blend: f32,
    pub player_y: f64,
    pub height_above_ground: f32,
    pub active_flavor: Option<NetherFlavor>,
    pub tick_counter: u64,
    pub world_loaded: bool,
}

impl ClientAtmosphere {
    pub fn reset(&mut self) {
        self.smog_blend = 0.0;
        self.player_y = 64.0;
        self.height_above_ground = 0.0;
        self.active_flavor = None;
        self.tick_counter = 0;
        self.world_loaded = false;
    }
}

pub fn fog_target_rgb(flavor: NetherFlavor, time_phase: f32) -> [f32; 3] {
    let shimmer = (time_phase * 0.07).sin() * 0.015;
    let base = match flavor {
        NetherFlavor::CrimsonForest => [0.24, 0.055, 0.045],
        NetherFlavor::WarpedForest => [0.065, 0.145, 0.165],
        NetherFlavor::SoulSandValley => [0.095, 0.145, 0.19],
        NetherFlavor::BasaltDeltas => [0.165, 0.14, 0.145],
        NetherFlavor::NetherWastes => [0.22, 0.075, 0.045],
    };
    [
        (base[0] + shimmer).clamp(0.0, 1.0),
        (base[1] + shimmer * 0.45).clamp(0.0, 1.0),
        base[2],
    ]
}

/// Scales horizontal biome proximity against player altitude so vision remains
/// shrouded in volcanic smog all the way up to the cloud layer (`Y ~ 188..222`).
pub fn altitude_smog_factor(player_y: f64) -> f32 {
    if player_y <= CLOUD_FADE_START_Y {
        1.0
    } else if player_y >= CLOUD_FADE_END_Y {
        0.0
    } else {
        ((CLOUD_FADE_END_Y - player_y) / (CLOUD_FADE_END_Y - CLOUD_FADE_START_Y)) as f32
    }
}

fn sample_player_corruption(host: &dyn Host, pos: [i32; 3], py: f64) -> (f32, f32, Option<NetherFlavor>) {
    let alt_factor = altitude_smog_factor(py);
    let height_above_ground = estimate_height_above_ground(host, pos, py);
    if alt_factor <= 0.001 {
        return (0.0, height_above_ground, None);
    }

    if let Some(flavor) = read_column_biome_flavor(host, pos[0], pos[1], pos[2]) {
        return (alt_factor, height_above_ground, Some(flavor));
    }

    // Smooth smog gradient around corrupted borders (+/- 20 blocks).
    let offsets = [
        [10, 0],
        [-10, 0],
        [0, 10],
        [0, -10],
        [18, 18],
        [-18, -18],
        [18, -18],
        [-18, 18],
    ];
    for [dx, dz] in offsets {
        if let Some(flavor) = read_column_biome_flavor(host, pos[0] + dx, pos[1], pos[2] + dz) {
            let weight = if dx.abs() <= 10 && dz.abs() <= 10 {
                0.7
            } else {
                0.35
            };
            return (weight * alt_factor, height_above_ground, Some(flavor));
        }
    }

    (0.0, height_above_ground, None)
}

fn estimate_height_above_ground(host: &dyn Host, pos: [i32; 3], py: f64) -> f32 {
    let cx = pos[0] >> 4;
    let cz = pos[2] >> 4;
    if let (Ok(chunks), Some(air)) = (
        host.client_chunks([cx, cz], [cx, cz]),
        host.default_state("minecraft:air"),
    ) {
        let start_y = pos[1].clamp(-60, 250);
        for y in (-63..=start_y).rev() {
            if chunks.block([pos[0], y, pos[2]]) != air {
                return (py - f64::from(y + 1)).max(0.0) as f32;
            }
        }
    }
    (py - 64.0).max(0.0) as f32
}

fn read_column_biome_flavor(host: &dyn Host, x: i32, y: i32, z: i32) -> Option<NetherFlavor> {
    let cx = x >> 4;
    let cz = z >> 4;
    let sample_ys = [y.clamp(-56, 200), 64, 32, 96, 0, -32, 140];

    if let (Ok(chunks), Some(biomes)) = (host.client_chunks([cx, cz], [cx, cz]), host.client_biomes())
    {
        for sy in sample_ys {
            if let Some(id) = chunks.biome([x, sy, z])
                && let Some(flavor) = NetherFlavor::from_name(biomes.name(id))
            {
                return Some(flavor);
            }
        }
    }

    for sy in [y.clamp(-56, 200), 64, 0, 128] {
        if let Some(name) = host.biome([x, sy, z])
            && let Some(flavor) = NetherFlavor::from_name(&name)
        {
            return Some(flavor);
        }
    }

    let q = quart_coords([x, y, z]);
    if rift_bridge::is_quart_col_corrupted(q[0], q[2]) {
        return Some(NetherFlavor::CrimsonForest);
    }

    None
}

pub fn tick_client_atmosphere(host: &mut dyn Host, atmo: &mut ClientAtmosphere) {
    atmo.tick_counter += 1;
    let Some(player) = host.player() else {
        atmo.smog_blend = 0.0;
        atmo.active_flavor = None;
        return;
    };

    if player.dimension != OVERWORLD {
        atmo.smog_blend = 0.0;
        atmo.active_flavor = None;
        return;
    }

    atmo.player_y = player.y;
    let bp = [
        player.x.floor() as i32,
        player.y.floor() as i32,
        player.z.floor() as i32,
    ];
    let (target_weight, height_above, detected_flavor) =
        sample_player_corruption(host, bp, player.y);
    atmo.height_above_ground = height_above;

    if let Some(flavor) = detected_flavor {
        atmo.active_flavor = Some(flavor);
    }

    if target_weight > atmo.smog_blend {
        atmo.smog_blend = (atmo.smog_blend + 0.045).min(target_weight);
    } else {
        atmo.smog_blend = (atmo.smog_blend - 0.03).max(target_weight);
        if atmo.smog_blend <= 0.01 {
            atmo.active_flavor = None;
        }
    }

    if atmo.smog_blend >= 0.2 {
        let flavor = atmo.active_flavor.unwrap_or(NetherFlavor::CrimsonForest);
        spawn_heat_haze_particles(
            host,
            player.x,
            player.y,
            player.z,
            atmo.tick_counter,
            atmo.smog_blend,
            flavor,
        );
    }
}

fn spawn_heat_haze_particles(
    host: &mut dyn Host,
    px: f64,
    py: f64,
    pz: f64,
    tick: u64,
    blend: f32,
    flavor: NetherFlavor,
) {
    let bursts = if blend > 0.7 { 5 } else { 2 };
    for i in 0..bursts {
        let h = spatial_hash(px as i32, py as i32, pz as i32, tick * 17 + i as u64);
        let ox = ((h & 255) as f64 / 255.0 - 0.5) * 18.0;
        let oy = (((h >> 8) & 255) as f64 / 255.0) * 8.0 - 2.0;
        let oz = (((h >> 16) & 255) as f64 / 255.0) * 18.0;

        let ty = match (flavor, (h >> 24) % 8) {
            (NetherFlavor::CrimsonForest, 0..=3) => ParticleType::CrimsonSpore,
            (NetherFlavor::CrimsonForest, 4..=5) => ParticleType::Ash,
            (NetherFlavor::WarpedForest, 0..=4) => ParticleType::WarpedSpore,
            (NetherFlavor::WarpedForest, _) => ParticleType::WhiteAsh,
            (NetherFlavor::SoulSandValley, 0..=5) => ParticleType::Ash,
            (NetherFlavor::SoulSandValley, _) => ParticleType::Soul,
            (NetherFlavor::BasaltDeltas, 0..=4) => ParticleType::WhiteAsh,
            (NetherFlavor::BasaltDeltas, _) => ParticleType::Ash,
            (NetherFlavor::NetherWastes, 0..=4) => ParticleType::Ash,
            _ => ParticleType::Smoke,
        };
        let _ = host.add_particle(
            &ParticleOptions::simple(ty),
            [px + ox, py + oy, pz + oz],
            [0.0, 0.015, 0.0],
        );

        if i == 0 && (h >> 28) % 5 == 0 {
            let _ = host.add_particle(
                &ParticleOptions::simple(ParticleType::SmallFlame),
                [px + ox * 0.6, py + oy * 0.5, pz + oz * 0.6],
                [0.0, 0.04, 0.0],
            );
        }
    }
}

pub fn apply_fog_color(atmo: &ClientAtmosphere, event: &mut ComputeFogColorEvent) {
    if event.fog_type != FogType::Atmospheric || atmo.smog_blend <= 0.005 {
        return;
    }
    let flavor = atmo.active_flavor.unwrap_or(NetherFlavor::CrimsonForest);
    let target = fog_target_rgb(flavor, atmo.tick_counter as f32 + event.partial);
    let blend = (atmo.smog_blend * 0.98).clamp(0.0, 0.98);
    for i in 0..3 {
        event.rgb[i] = lerp(event.rgb[i], target[i], blend);
    }
}

pub fn compute_fog_ends(atmo: &ClientAtmosphere, partial: f32) -> (f32, f32, f32, f32) {
    let wave = ((atmo.tick_counter as f32 + partial) * 0.09).sin() * 2.0;
    // Low-to-medium height above corrupted ground: blind choking volcanic smog.
    // High below the clouds: still heavily tainted & obscures sky/clouds until Y >= 190..222.
    let high_relax = if atmo.height_above_ground <= 42.0 {
        0.0
    } else {
        ((atmo.height_above_ground - 42.0) / 80.0).clamp(0.0, 1.0)
    };
    let env_start = lerp(0.0, 3.0, high_relax);
    let env_end = (lerp(20.0, 36.0, high_relax) + wave).clamp(16.0, 44.0);
    let sky_end = lerp(0.5, 2.0, high_relax);
    let cloud_end = lerp(0.5, 1.5, high_relax);
    (env_start, env_end, sky_end, cloud_end)
}

pub fn apply_render_fog(atmo: &ClientAtmosphere, event: &mut RenderFogEvent) {
    if event.fog_type != FogType::Atmospheric || atmo.smog_blend <= 0.005 {
        return;
    }
    let b = atmo.smog_blend.clamp(0.0, 1.0);
    let (env_start, env_end, sky_end, cloud_end) = compute_fog_ends(atmo, event.partial);

    // Swallow sky dome, sun glow, and clouds below cloud altitude, and pull in both
    // environmental and render-distance fog ends so low/medium vision is thickly obscured.
    event.fog.color[3] = lerp(event.fog.color[3], 1.0, b);
    event.fog.sky_end = lerp(event.fog.sky_end, sky_end, b);
    event.fog.cloud_end = lerp(event.fog.cloud_end, cloud_end, b);
    event.fog.environmental_start = lerp(event.fog.environmental_start, env_start, b);
    event.fog.environmental_end = lerp(event.fog.environmental_end, env_end, b);
    event.fog.render_distance_start = lerp(event.fog.render_distance_start, env_start, b);
    event.fog.render_distance_end = lerp(event.fog.render_distance_end, env_end + 4.0, b);
}

pub fn select_nether_music(atmo: &ClientAtmosphere) -> Option<Music> {
    if atmo.smog_blend < 0.35 {
        return None;
    }
    let flavor = atmo.active_flavor.unwrap_or(NetherFlavor::CrimsonForest);
    Some(Music::new(flavor.music_event(), 40, 320, true))
}

pub fn apply_nether_music(atmo: &ClientAtmosphere, event: &mut MusicEvent) {
    if let Some(music) = select_nether_music(atmo) {
        *event.music = Some(music);
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_tracks_match_vanilla_nether_biome_events() {
        assert_eq!(NetherFlavor::CrimsonForest.music_event(), "minecraft:music.nether.crimson_forest");
        assert_eq!(NetherFlavor::WarpedForest.music_event(), "minecraft:music.nether.warped_forest");
        assert_eq!(NetherFlavor::SoulSandValley.music_event(), "minecraft:music.nether.soul_sand_valley");
        assert_eq!(NetherFlavor::BasaltDeltas.music_event(), "minecraft:music.nether.basalt_deltas");
        assert_eq!(NetherFlavor::NetherWastes.music_event(), "minecraft:music.nether.nether_wastes");
    }

    #[test]
    fn smog_stays_thick_below_clouds_and_clears_above_cloud_layer() {
        assert_eq!(altitude_smog_factor(64.0), 1.0);
        assert_eq!(altitude_smog_factor(140.0), 1.0);
        assert_eq!(altitude_smog_factor(185.0), 1.0);
        assert!(altitude_smog_factor(205.0) > 0.3 && altitude_smog_factor(205.0) < 0.7);
        assert_eq!(altitude_smog_factor(225.0), 0.0);

        let low = ClientAtmosphere {
            smog_blend: 1.0,
            player_y: 80.0,
            height_above_ground: 15.0,
            active_flavor: Some(NetherFlavor::CrimsonForest),
            tick_counter: 100,
            world_loaded: true,
        };
        let (_, env_end, sky_end, cloud_end) = compute_fog_ends(&low, 0.0);
        assert!(env_end <= 24.0);
        assert!(sky_end <= 1.0);
        assert!(cloud_end <= 1.0);
        assert_eq!(
            select_nether_music(&low).map(|m| m.sound),
            Some("minecraft:music.nether.crimson_forest".to_owned())
        );

        let above_clouds = ClientAtmosphere {
            smog_blend: 0.0,
            player_y: 230.0,
            height_above_ground: 165.0,
            active_flavor: None,
            tick_counter: 100,
            world_loaded: true,
        };
        assert_eq!(select_nether_music(&above_clouds), None);
    }
}
