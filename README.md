# Rift Bleed (Horizon Native Mod)

**Rift Bleed** (`rift_bleed`) turns every active **Overworld Nether Portal** into a creeping dimensional infection that terraforms surrounding terrain into scarred Nether biomes, mutates nearby mobs, blankets the sky in heat smog, swaps in vanilla Nether music, and spawns Bastion Remnants—all using vanilla blocks, mobs, particles, sounds, and music (zero custom 3D models or textures).

## Core Mechanics

- **Lit Portals Only**: Unfinished Ruined Portals remain dormant until ignited (`minecraft:nether_portal`).
- **Crying Obsidian Containment & Airlocks**:
  - Plain obsidian leaks corruption. Surround a portal (floor, walls, ceiling) with **`minecraft:crying_obsidian`** to halt all spreading.
  - Closed **`minecraft:iron_door`** / **`minecraft:iron_trapdoor`** seal doorways. Building a two-door vestibule lets you open one door at a time as a working **airlock** without breaking containment; opening both doors breaches the chamber and resumes spreading.
- **Breaking Portals Leaves a Rift Scar**:
  - Breaking or extinguishing an uncontained portal leaves an active **Rift Scar** emitting particles until the Scar itself is sealed inside Crying Obsidian.
  - Sealing a Rift stops new growth permanently (while the seal remains intact), leaving already-corrupted land permanently scarred.
- **Scarred Overworld Corruption & Caves**:
  - Converts terrain across the five Nether flavors (`crimson_forest`, `warped_forest`, `soul_sand_valley`, `basalt_deltas`, `nether_wastes`), updating chunk biome cells (`minecraft:overworld`) and syncing them to clients.
  - Leaves ~14% scarred Overworld stone/deepslate/cobblestone/coarse-dirt/stripped-log remnants so zones feel torn apart rather than cloned.
  - Protects Bedrock, containers, beds, beacons, workstations, and portal/containment blocks, and preserves `diamond_ore`, `redstone_ore`, `lapis_ore`, and `emerald_ore` (including deepslate variants).
  - **Caves stay breathable**: open cave air never floods with lava; water evaporates cleanly on contact; deep corrupted Deepslate (`Y = -32..-60`) in mature rifts rarely forms `minecraft:ancient_debris`.
  - **Beds stay safe** to use in corrupted Overworld zones, while water evaporates (`block.fire.extinguish` + smoke) and lava receives faster tick scheduling.
- **Atmosphere & Vanilla Nether Music**:
  - Volumetric heat-smog fog thickens inside corrupted zones, hiding the Overworld sun and clouds (`sky_end`/`cloud_end`) and drifting ash/spore/ember particles until you step back out.
  - Plays vanilla Nether biome music (`minecraft:music.nether.*`) inside corrupted areas.
- **Mobs & Bastions**:
  - Prevents Piglin/Hoglin zombification inside corrupted Overworld zones.
  - Mutates Overworld animals/undead caught in corruption (`pig`/`villager` → `piglin`, `cow`/`sheep`/`goat` → `hoglin`, `skeleton`/`stray` → `wither_skeleton`, `slime` → `magma_cube`) and buffs surviving Overworld hostiles as **Nether-Touched** (Fire Resistance + Strength + bonus Nether drops).
  - **Hoglins** near the corruption frontier trample adjacent Overworld grass/dirt into crimson nylium/netherrack to expand the infection outward.
  - Mature, large uncontained rifts spawn vanilla **Bastion Remnant** assemblies guarded by Piglin Brutes.

## Commands

- `/rift status` — Show all tracked Portals/Rift Scars, containment status, flavor, radius, block count, and Bastions.
- `/rift scan` — Scan nearby loaded blocks for active Nether Portals.
- `/rift speed <4..128>` *(OP level 2)* — Adjust frontier block spread budget per tick (default `28`).
- `/rift pause` / `/rift resume` *(OP level 2)* — Pause or resume Rift Bleed expansion.

## Layout & Building

- Self-contained native mod bundle: `bundle/`
  - Manifest: `bundle/horizon-mixin.toml` (`horizon_revision = "9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0"`)
  - Mixin bridge & runtime: `bundle/mixins/{bridge.rs, runtime.rs, level_tick_pre.rs}`
  - Trait mod crate: `bundle/api/`

Build & install into a Horizon instance (`Horizon Launcher >= 0.2.63`, Horizon `nightly-2026-11-13-9f42064`):

```powershell
& "$env:LOCALAPPDATA\Programs\Horizon Launcher\horizon-launcher.exe" native-build ".\bundle" --instance Horizon --version nightly-2026-11-13-9f42064 --trust-native-code --profile performance
```
