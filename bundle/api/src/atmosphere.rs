use crate::types::{NetherFlavor, OVERWORLD, quart_coords, spatial_hash};
use horizon_client::entry::rift_bridge;
use horizon_client::mod_api::{
    ComputeFogColorEvent, FogType, Host, Music, MusicEvent, ParticleOptions, ParticleType,
    RenderFogEvent,
};

#[derive(Default)]
pub struct ClientAtmosphere {
    pub smog_blend: f32,
    pub active_flavor: Option<NetherFlavor>,
    pub tick_counter: u64,
    pub world_loaded: bool,
}

impl ClientAtmosphere {
    pub fn reset(&mut self) {
        self.smog_blend = 0.0;
        self.active_flavor = None;
        self.tick_counter = 0;
        self.world_loaded = false;
    }
}

pub fn fog_target_rgb(flavor: NetherFlavor, time_phase: f32) -> [f32; 3] {
    // Subtle shimmering furnace heat pulse across fog colors.
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

fn sample_player_corruption(host: &dyn Host, pos: [i32; 3]) -> (f32, Option<NetherFlavor>) {
    if let Some(flavor) = read_biome_flavor_at(host, pos) {
        return (1.0, Some(flavor));
    }

    // Smooth smog ramp right at the corrupted Overworld edge (+/- 10 blocks).
    let offsets = [
        [6, 0, 0],
        [-6, 0, 0],
        [0, 0, 6],
        [0, 0, -6],
        [0, -4, 0],
        [10, 0, 10],
        [-10, 0, -10],
    ];
    for off in offsets {
        let p = [pos[0] + off[0], pos[1] + off[1], pos[2] + off[2]];
        if let Some(flavor) = read_biome_flavor_at(host, p) {
            let weight = if off[0].abs() <= 6 && off[2].abs() <= 6 {
                0.55
            } else {
                0.28
            };
            return (weight, Some(flavor));
        }
    }

    (0.0, None)
}

fn read_biome_flavor_at(host: &dyn Host, pos: [i32; 3]) -> Option<NetherFlavor> {
    let cx = pos[0] >> 4;
    let cz = pos[2] >> 4;
    if let (Ok(chunks), Some(biomes)) = (host.client_chunks([cx, cz], [cx, cz]), host.client_biomes())
        && let Some(id) = chunks.biome(pos)
    {
        let name = biomes.name(id);
        if let Some(flavor) = NetherFlavor::from_name(name) {
            return Some(flavor);
        }
    }

    if let Some(name) = host.biome(pos)
        && let Some(flavor) = NetherFlavor::from_name(&name)
    {
        return Some(flavor);
    }

    let q = quart_coords(pos);
    if rift_bridge::is_quart_corrupted(q[0], q[1], q[2]) {
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

    let bp = [
        player.x.floor() as i32,
        player.y.floor() as i32,
        player.z.floor() as i32,
    ];
    let (target_weight, detected_flavor) = sample_player_corruption(host, bp);
    if let Some(flavor) = detected_flavor {
        atmo.active_flavor = Some(flavor);
    }

    if target_weight > atmo.smog_blend {
        atmo.smog_blend = (atmo.smog_blend + 0.032).min(target_weight);
    } else {
        atmo.smog_blend = (atmo.smog_blend - 0.025).max(target_weight);
        if atmo.smog_blend <= 0.01 {
            atmo.active_flavor = None;
        }
    }

    if atmo.smog_blend >= 0.25 {
        let flavor = atmo.active_flavor.unwrap_or(NetherFlavor::CrimsonForest);
        spawn_heat_haze_particles(host, player.x, player.y, player.z, atmo.tick_counter, atmo.smog_blend, flavor);
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
    let bursts = if blend > 0.75 { 4 } else { 2 };
    for i in 0..bursts {
        let h = spatial_hash(px as i32, py as i32, pz as i32, tick * 17 + i as u64);
        let ox = ((h & 255) as f64 / 255.0 - 0.5) * 18.0;
        let oy = (((h >> 8) & 255) as f64 / 255.0) * 8.0 - 1.5;
        let oz = (((h >> 16) & 255) as f64 / 255.0 - 0.5) * 18.0;

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
    let blend = (atmo.smog_blend * 0.94).clamp(0.0, 0.94);
    for i in 0..3 {
        event.rgb[i] = lerp(event.rgb[i], target[i], blend);
    }
}

pub fn apply_render_fog(atmo: &ClientAtmosphere, event: &mut RenderFogEvent) {
    if event.fog_type != FogType::Atmospheric || atmo.smog_blend <= 0.005 {
        return;
    }
    let b = atmo.smog_blend.clamp(0.0, 1.0);
    let wave = ((atmo.tick_counter as f32 + event.partial) * 0.09).sin() * 2.5;
    let target_end = (46.0 + wave).clamp(32.0, 60.0);

    // Choking volcanic smog swallows the sky dome and clouds inside corrupted biomes.
    event.fog.sky_end = lerp(event.fog.sky_end, 3.0, b);
    event.fog.cloud_end = lerp(event.fog.cloud_end, 2.0, b);
    event.fog.environmental_start = lerp(event.fog.environmental_start, 3.5, b);
    event.fog.environmental_end = lerp(event.fog.environmental_end, target_end, b);
    event.fog.render_distance_start = lerp(event.fog.render_distance_start, 2.0, b * 0.85);
    event.fog.render_distance_end = lerp(event.fog.render_distance_end, target_end + 10.0, b * 0.85);
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
    fn smog_blend_selects_nether_music_and_red_crimson_haze() {
        let mut atmo = ClientAtmosphere {
            smog_blend: 1.0,
            active_flavor: Some(NetherFlavor::CrimsonForest),
            tick_counter: 100,
            world_loaded: true,
        };
        assert_eq!(
            select_nether_music(&atmo).map(|m| m.sound),
            Some("minecraft:music.nether.crimson_forest".to_owned())
        );

        let rgb = fog_target_rgb(NetherFlavor::CrimsonForest, 0.0);
        assert!(rgb[0] > rgb[1] && rgb[0] > rgb[2]);

        atmo.smog_blend = 0.0;
        assert_eq!(select_nether_music(&atmo), None);
    }
}
