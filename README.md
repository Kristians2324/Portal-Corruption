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
- **Circular Non-Stop Overworld & Underground Terraforming**:
  - Expands evenly in concentric 360° circular rings (`build_ring_columns`) across the five Nether flavors (`crimson_forest`, `warped_forest`, `soul_sand_valley`, `basalt_deltas`, `nether_wastes`), updating full bedrock-to-sky biome columns on `minecraft:overworld` chunks.
  - Holds rolling chunk-load tickets (`rift_bleed:core` + up to 4 sliding `rift_bleed:frontier` chunks) so uncontained Rifts keep corrupting non-stop even when no player is nearby.
  - Corrupted columns warp surface terrain, trees, and underground stone/caves down to `Y = -63`, leaving ~12% clustered Overworld stone/deepslate/cobblestone/coarse-dirt/scarred soil/stripped-log remnants while keeping ~88% overwhelmingly corrupted.
  - Protects Bedrock, containers, beds, beacons, workstations, and portal/containment blocks, and preserves `diamond_ore`, `redstone_ore`, `lapis_ore`, and `emerald_ore` (plus deepslate variants).
  - **Caves stay breathable**: open cave air never floods with lava; water (oceans, rivers, kelp, seagrass, buckets, fluid spread) vanishes silently on contact without infinite refill loops; deep corrupted Deepslate (`Y = -32..-60`) in mature rifts rarely forms `minecraft:ancient_debris`.
  - **Beds stay safe** to use in corrupted Overworld zones, while water vanishes cleanly (`block_spread` / `fill_bucket` / `fluid_level_change` blocked) and lava receives faster tick scheduling.
- **Volcanic Smog up to the Clouds & Vanilla Nether Music**:
  - Dense volcanic smog shrouds vision (`env_end ~ 20–36`, `sky_end = 0.5`, `cloud_end = 0.5`) at ground level and medium flight height, keeping sun and clouds blocked until climbing above the cloud deck (`Y ~ 188..222`).
  - Plays vanilla Nether biome music (`minecraft:music.nether.*`) inside corrupted areas.
- **Nether Mob Hunting, Kill Conversions & Bastions**:
  - Prevents Piglin/Hoglin zombification inside corrupted Overworld zones.
  - Nether mobs (`Piglin`, `Piglin Brute`, `Hoglin`, `Wither Skeleton`, `Blaze`) actively hunt Overworld creatures; victims killed by Nether mobs or inside corruption convert into Nether versions:
    - Pigs, Cows, Sheep, Goats, Horses, Donkeys, Llamas → **Hoglin**
    - Villagers, Wandering Traders, Pillagers, Evokers → **Piglin**
    - Iron Golems, Vindicators, Ravagers → **Piglin Brute**
    - Zombies, Husks, Drowned → **Zombified Piglin**
    - Skeletons, Strays, Spiders, Phantoms → **Wither Skeleton**
    - Chickens, Rabbits, Slimes, Wolves, Foxes, Cats → **Magma Cube**
    - Witches, Creepers → **Blaze**
  - Surviving Overworld hostiles gain Fire Resistance + Strength (**Nether-Touched**) with bonus Nether drops, and **Hoglins** near the border trample soils into crimson nylium/netherrack.
  - Mature, large uncontained rifts spawn vanilla **Bastion Remnant** assemblies guarded by Piglin Brutes.

## Commands

- `/rift status` — Show all tracked Portals/Rift Scars, containment status, flavor, ring radius, block count, and Bastions.
- `/rift scan` — Scan nearby loaded blocks for active Nether Portals.
- `/rift speed <4..1200>` *(OP level 2)* — Adjust block spread budget per tick (default `14`, giving ~10 minutes before spreading beyond ~30 blocks).
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
