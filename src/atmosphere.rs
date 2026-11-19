use crate::types::{NetherFlavor, OVERWORLD, OVERWORLD_MAX_Y, OVERWORLD_MIN_Y, spatial_hash};
use mod_api::{
    ClientHost, ComputeFogColorEvent, FogData, FogType, Heightmap, LevelRef, Music, MusicEvent,
    ParticleKey, ParticleOptions, RenderFogEvent,
};

const CLOUD_FADE_START_Y: f32 = 188.0;
const CLOUD_FADE_END_Y: f32 = 222.0;
const RGB_TEMPORAL_RATE: f32 = 0.04;
const INTENSITY_TEMPORAL_RATE: f32 = 0.05;
const NEIGHBORHOOD_OFFSETS: [i32; 5] = [-8, -4, 0, 4, 8];

#[derive(Clone, Copy, Debug)]
pub struct ParticlePalette {
    pub ash: ParticleKey,
    pub crimson_spore: ParticleKey,
    pub warped_spore: ParticleKey,
    pub white_ash: ParticleKey,
    pub flame: ParticleKey,
    pub small_flame: ParticleKey,
}

#[derive(Clone, Debug, Default)]
pub struct ClientAtmosphere {
    pub client_level: Option<LevelRef>,
    pub in_overworld: bool,
    pub intensity: f32,
    pub target_intensity: f32,
    pub blended_rgb: [f32; 3],
    pub current_rgb: [f32; 3],
    pub has_rgb: bool,
    pub dominant_flavor: Option<NetherFlavor>,
    pub camera_y: f32,
    pub height_above_ground: f32,
    pub tick_counter: u64,
}

pub fn fog_target_rgb(flavor: NetherFlavor) -> [f32; 3] {
    match flavor {
        NetherFlavor::CrimsonForest => [0.24, 0.04, 0.05],
        NetherFlavor::WarpedForest => [0.05, 0.09, 0.16],
        NetherFlavor::SoulSandValley => [0.11, 0.16, 0.19],
        NetherFlavor::BasaltDeltas => [0.21, 0.17, 0.20],
        NetherFlavor::NetherWastes => [0.22, 0.06, 0.04],
    }
}

/// Keeps thick Nether sky/fog taint active while the player is below the cloud layer,
/// only clearing smoothly between `Y = 188` and `Y = 222` (up in the clouds).
pub fn altitude_smog_factor(camera_y: f32, height_above_ground: f32) -> f32 {
    if camera_y <= CLOUD_FADE_START_Y {
        return 1.0;
    }
    if camera_y >= CLOUD_FADE_END_Y {
        return 0.0;
    }
    let _ = height_above_ground;
    let t = (camera_y - CLOUD_FADE_START_Y) / (CLOUD_FADE_END_Y - CLOUD_FADE_START_Y);
    let smooth = t * t * (3.0 - 2.0 * t);
    (1.0 - smooth).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NeighborhoodSample {
    pub corrupt_fraction: f32,
    pub blended_rgb: [f32; 3],
    pub dominant_flavor: Option<NetherFlavor>,
}

/// Samples a 5x5 Gaussian-weighted neighborhood (4-block steps matching Minecraft biome
/// quart resolution) so transitions between Nether biomes or across the Overworld edge
/// blend smoothly without sudden color snaps.
pub fn blend_biome_neighborhood<F>(
    center_x: i32,
    center_z: i32,
    previous_flavor: Option<NetherFlavor>,
    mut sample_fn: F,
) -> NeighborhoodSample
where
    F: FnMut(i32, i32) -> Option<NetherFlavor>,
{
    let mut total_weight = 0.0f32;
    let mut corrupt_weight = 0.0f32;
    let mut rgb_sum = [0.0f32; 3];
    let mut flavor_weights = [0.0f32; 5];

    for &dx in &NEIGHBORHOOD_OFFSETS {
        for &dz in &NEIGHBORHOOD_OFFSETS {
            let d2 = (dx * dx + dz * dz) as f32;
            let weight = (-d2 / 72.0).exp();
            total_weight += weight;

            if let Some(flavor) = sample_fn(center_x + dx, center_z + dz) {
                corrupt_weight += weight;
                let c = fog_target_rgb(flavor);
                rgb_sum[0] += c[0] * weight;
                rgb_sum[1] += c[1] * weight;
                rgb_sum[2] += c[2] * weight;
                flavor_weights[flavor.to_u8() as usize] += weight;
            }
        }
    }

    if corrupt_weight <= 0.0 || total_weight <= 0.0 {
        return NeighborhoodSample {
            corrupt_fraction: 0.0,
            blended_rgb: [0.0; 3],
            dominant_flavor: None,
        };
    }

    let blended_rgb = [
        rgb_sum[0] / corrupt_weight,
        rgb_sum[1] / corrupt_weight,
        rgb_sum[2] / corrupt_weight,
    ];

    let mut best_idx = 0usize;
    let mut best_w = -1.0f32;
    for (idx, &w) in flavor_weights.iter().enumerate() {
        if w > best_w {
            best_w = w;
            best_idx = idx;
        }
    }

    // Apply mild hysteresis so dominant music/particle flavor never flips rapidly on a 50/50 seam.
    let dominant_flavor = if let Some(prev) = previous_flavor {
        let prev_w = flavor_weights[prev.to_u8() as usize];
        if prev_w > 0.0 && prev_w * 1.15 >= best_w {
            Some(prev)
        } else {
            Some(NetherFlavor::from_u8(best_idx as u8))
        }
    } else {
        Some(NetherFlavor::from_u8(best_idx as u8))
    };

    NeighborhoodSample {
        corrupt_fraction: (corrupt_weight / total_weight).clamp(0.0, 1.0),
        blended_rgb,
        dominant_flavor,
    }
}

pub fn step_smoothed_rgb(current: &mut [f32; 3], initialized: &mut bool, target: [f32; 3], rate: f32) {
    if !*initialized {
        *current = target;
        *initialized = true;
        return;
    }
    for i in 0..3 {
        current[i] += (target[i] - current[i]) * rate;
    }
}

pub fn step_client_atmosphere_values(
    atmo: &mut ClientAtmosphere,
    sample: NeighborhoodSample,
    camera_y: f32,
    height_above_ground: f32,
) {
    atmo.camera_y = camera_y;
    atmo.height_above_ground = height_above_ground;

    let alt_factor = altitude_smog_factor(camera_y, height_above_ground);
    atmo.target_intensity = (sample.corrupt_fraction * alt_factor).clamp(0.0, 1.0);
    atmo.intensity += (atmo.target_intensity - atmo.intensity) * INTENSITY_TEMPORAL_RATE;

    if sample.corrupt_fraction > 0.0 {
        atmo.blended_rgb = sample.blended_rgb;
        atmo.dominant_flavor = sample.dominant_flavor;
        step_smoothed_rgb(
            &mut atmo.current_rgb,
            &mut atmo.has_rgb,
            atmo.blended_rgb,
            RGB_TEMPORAL_RATE,
        );
    } else if atmo.intensity < 0.005 {
        atmo.intensity = 0.0;
        atmo.has_rgb = false;
        atmo.dominant_flavor = None;
    }
}

pub fn tick_client_atmosphere(
    atmo: &mut ClientAtmosphere,
    host: &mut dyn ClientHost,
    particles: Option<&ParticlePalette>,
) {
    atmo.tick_counter = atmo.tick_counter.wrapping_add(1);

    if atmo.client_level.is_none() {
        atmo.client_level = host.local_player().and_then(|p| p.level(host));
    }

    let Some(level) = atmo.client_level else {
        atmo.in_overworld = false;
        atmo.intensity = 0.0;
        atmo.target_intensity = 0.0;
        atmo.has_rgb = false;
        atmo.dominant_flavor = None;
        return;
    };

    atmo.in_overworld = level.dimension_id() == OVERWORLD;
    if !atmo.in_overworld {
        atmo.intensity = 0.0;
        atmo.target_intensity = 0.0;
        atmo.has_rgb = false;
        atmo.dominant_flavor = None;
        return;
    }

    let Some(camera) = host.camera() else {
        return;
    };

    let px = camera.position[0].floor() as i32;
    let py = camera.position[1].floor() as i32;
    let pz = camera.position[2].floor() as i32;

    let ground_y = host
        .client_surface_heights(&[(px, pz)], Heightmap::MotionBlocking)
        .ok()
        .and_then(|v| v.first().copied().flatten())
        .unwrap_or(py - 4);
    let height_above_ground = (camera.position[1] as f32 - ground_y as f32).max(0.0);

    let probe_y = py.clamp(OVERWORLD_MIN_Y + 4, OVERWORLD_MAX_Y - 4);
    let sample = blend_biome_neighborhood(px, pz, atmo.dominant_flavor, |sx, sz| {
        level
            .client_biome(host, [sx, probe_y, sz])
            .or_else(|| level.client_biome(host, [sx, 64, sz]))
            .and_then(|b| NetherFlavor::from_name(&b))
    });

    step_client_atmosphere_values(
        atmo,
        sample,
        camera.position[1] as f32,
        height_above_ground,
    );

    if atmo.intensity > 0.15 {
        spawn_heat_haze_particles(host, camera.position, atmo, particles);
    }
}

fn spawn_heat_haze_particles(
    host: &mut dyn ClientHost,
    cam_pos: [f64; 3],
    atmo: &ClientAtmosphere,
    particles: Option<&ParticlePalette>,
) {
    let Some(flavor) = atmo.dominant_flavor else {
        return;
    };

    let burst = ((atmo.intensity * 6.0).round() as usize).clamp(1, 6);
    let seed = atmo.tick_counter;

    for i in 0..burst {
        let h = spatial_hash(
            cam_pos[0].floor() as i32 + i as i32,
            (seed & 0x7FFF_FFFF) as i32,
            cam_pos[2].floor() as i32 - i as i32,
            0x4A2E_0001 ^ (i as u64),
        );
        let dx = ((h & 0xFF) as f64 / 255.0 - 0.5) * 20.0;
        let dz = (((h >> 8) & 0xFF) as f64 / 255.0 - 0.5) * 20.0;
        let dy = (((h >> 16) & 0xFF) as f64 / 255.0 - 0.35) * 10.0;
        let at = [cam_pos[0] + dx, cam_pos[1] + dy, cam_pos[2] + dz];
        let vel = [0.0, 0.018, 0.0];

        if let Some(pal) = particles {
            let key = match (flavor, h % 3) {
                (NetherFlavor::CrimsonForest, 0..=1) => Some(pal.crimson_spore),
                (NetherFlavor::WarpedForest, 0..=1) => Some(pal.warped_spore),
                (NetherFlavor::BasaltDeltas, 0..=1) => Some(pal.white_ash),
                (NetherFlavor::SoulSandValley, _) => Some(pal.ash),
                _ => None,
            };
            if let Some(k) = key {
                let _ = host.add_particle(&ParticleOptions::simple(k), at, vel);
                continue;
            }
        }

        if (h % 5) == 0 {
            let soul = flavor == NetherFlavor::SoulSandValley;
            host.flame_particle(at, soul);
        }
    }
}

pub fn apply_fog_color(atmo: &ClientAtmosphere, event: &mut ComputeFogColorEvent<'_>) {
    if !atmo.in_overworld
        || !atmo.has_rgb
        || atmo.intensity <= 0.002
        || event.fog_type != FogType::Atmospheric
    {
        return;
    }

    let weight = atmo.intensity.clamp(0.0, 0.94);
    for i in 0..3 {
        event.rgb[i] = event.rgb[i] * (1.0 - weight) + atmo.current_rgb[i] * weight;
    }
}

pub fn compute_fog_targets(intensity: f32) -> (f32, f32, f32, f32, f32, f32) {
    let t = intensity.clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    (
        lerp(64.0, 4.0),   // environmental_start
        lerp(240.0, 44.0), // environmental_end
        lerp(80.0, 8.0),   // render_distance_start
        lerp(256.0, 58.0), // render_distance_end
        lerp(200.0, 34.0), // sky_end
        lerp(256.0, 48.0), // cloud_end
    )
}

pub fn apply_render_fog(atmo: &ClientAtmosphere, event: &mut RenderFogEvent<'_>) {
    if !atmo.in_overworld || atmo.intensity <= 0.005 || event.fog_type != FogType::Atmospheric {
        return;
    }

    let (env_s, env_e, rd_s, rd_e, sky_e, cloud_e) = compute_fog_targets(atmo.intensity);
    let fog: &mut FogData = event.fog;
    fog.environmental_start = fog.environmental_start.min(env_s);
    fog.environmental_end = fog.environmental_end.min(env_e);
    fog.render_distance_start = fog.render_distance_start.min(rd_s);
    fog.render_distance_end = fog.render_distance_end.min(rd_e);
    fog.sky_end = fog.sky_end.min(sky_e);
    fog.cloud_end = fog.cloud_end.min(cloud_e);
}

pub fn select_nether_music(atmo: &ClientAtmosphere, current: Option<&str>) -> Option<Music> {
    if !atmo.in_overworld || atmo.intensity < 0.20 {
        return None;
    }
    if let Some(cur) = current
        && (cur == "minecraft:music.menu"
            || cur.starts_with("minecraft:music.dragon")
            || cur.starts_with("minecraft:music.under_water"))
    {
        return None;
    }
    let flavor = atmo.dominant_flavor?;
    let desired = flavor.music_event();
    let replace = current != Some(desired);
    Some(Music::new(desired, 900, 3200, replace))
}

pub fn apply_nether_music(atmo: &ClientAtmosphere, event: &mut MusicEvent<'_>) {
    let cur_id = event.music.as_ref().map(|m| m.sound.as_str());
    if let Some(desired) = select_nether_music(atmo, cur_id) {
        *event.music = Some(desired);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_biome_border_blends_sky_color_smoothly_without_snap() {
        // West of x=0 is Crimson Forest, East of x=0 is Warped Forest.
        let crimson = fog_target_rgb(NetherFlavor::CrimsonForest);
        let warped = fog_target_rgb(NetherFlavor::WarpedForest);

        let mut atmo = ClientAtmosphere {
            in_overworld: true,
            ..ClientAtmosphere::default()
        };

        let sample_at = |x: i32, prev: Option<NetherFlavor>| {
            blend_biome_neighborhood(x, 0, prev, |sx, _sz| {
                if sx < 0 {
                    Some(NetherFlavor::CrimsonForest)
                } else {
                    Some(NetherFlavor::WarpedForest)
                }
            })
        };

        // Stabilize inside Crimson Forest.
        for _ in 0..40 {
            let s = sample_at(-12, atmo.dominant_flavor);
            step_client_atmosphere_values(&mut atmo, s, 70.0, 2.0);
        }
        let start_rgb = atmo.current_rgb;
        assert!((start_rgb[0] - crimson[0]).abs() < 0.01);

        // Step across the seam block by block: consecutive red/blue deltas must be small & smooth.
        let mut prev_r = start_rgb[0];
        let mut prev_b = start_rgb[2];
        for x in -10..=12 {
            let s = sample_at(x, atmo.dominant_flavor);
            step_client_atmosphere_values(&mut atmo, s, 70.0, 2.0);
            let dr = (atmo.current_rgb[0] - prev_r).abs();
            let db = (atmo.current_rgb[2] - prev_b).abs();
            assert!(
                dr < 0.02 && db < 0.02,
                "sky color snapped at x={x}: dr={dr}, db={db}"
            );
            prev_r = atmo.current_rgb[0];
            prev_b = atmo.current_rgb[2];
        }

        // Continuing inside Warped Forest smoothly converges toward warped RGB.
        for _ in 0..120 {
            let s = sample_at(12, atmo.dominant_flavor);
            step_client_atmosphere_values(&mut atmo, s, 70.0, 2.0);
        }
        assert!((atmo.current_rgb[0] - warped[0]).abs() < 0.01);
        assert!((atmo.current_rgb[2] - warped[2]).abs() < 0.01);
    }

    #[test]
    fn smog_stays_thick_well_above_terrain_until_reaching_clouds() {
        assert_eq!(altitude_smog_factor(70.0, 6.0), 1.0);
        assert_eq!(altitude_smog_factor(140.0, 76.0), 1.0);
        assert_eq!(altitude_smog_factor(185.0, 120.0), 1.0);
        let mid_cloud = altitude_smog_factor(205.0, 140.0);
        assert!(mid_cloud > 0.1 && mid_cloud < 0.9);
        assert_eq!(altitude_smog_factor(225.0, 160.0), 0.0);
    }

    #[test]
    fn nether_music_overrides_overworld_but_preserves_menu_and_dragon() {
        let atmo = ClientAtmosphere {
            in_overworld: true,
            intensity: 0.8,
            dominant_flavor: Some(NetherFlavor::BasaltDeltas),
            ..ClientAtmosphere::default()
        };
        let picked = select_nether_music(&atmo, Some("minecraft:music.overworld.plains")).unwrap();
        assert_eq!(picked.sound, "minecraft:music.nether.basalt_deltas");
        assert!(select_nether_music(&atmo, Some("minecraft:music.menu")).is_none());
        assert!(select_nether_music(&atmo, Some("minecraft:music.dragon")).is_none());
    }
}
