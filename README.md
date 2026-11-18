# Rift Bleed (Horizon Native Mod)

**Rift Bleed** (`rift_bleed`) turns every active **Overworld Nether Portal** into a creeping dimensional infection that terraforms surrounding terrain into scarred Nether biomes, mutates nearby mobs, shrouds the sky in smoothly blended volcanic smog up to the clouds, plays vanilla Nether music, and births rare Bastion Remnants—all using vanilla blocks, mobs, particles, sounds, and music (zero custom 3D models or textures).

## Core Mechanics

- **Lit Portals Only**: Unfinished Ruined Portals remain dormant until ignited (`minecraft:nether_portal`).
- **Crying Obsidian Containment & Airlocks**:
  - Plain obsidian leaks corruption. Surround a portal (floor, walls, ceiling) with **`minecraft:crying_obsidian`** to halt all spreading.
  - Closed **`minecraft:iron_door`** / **`minecraft:iron_trapdoor`** seal doorways. Building a two-door vestibule lets you open one door at a time as a working **airlock** without breaking containment; opening both doors breaches the chamber and resumes spreading.
- **Breaking Portals Leaves a Rift Scar**:
  - Breaking or extinguishing an uncontained portal leaves an active **Rift Scar** until the Scar itself is sealed inside Crying Obsidian.
  - Sealing a Rift halts new growth permanently (while the seal remains intact), leaving already-corrupted terrain permanently scarred.
- **Circular Non-Stop Overworld & Underground Terraforming**:
  - Expands evenly in concentric 360° circular rings across the five Nether flavors (`crimson_forest`, `warped_forest`, `soul_sand_valley`, `basalt_deltas`, `nether_wastes`), painting bedrock-to-sky biome columns (`Y = -64..319`) on `minecraft:overworld` chunks.
  - Holds bounded force-loaded chunks for active rift cores and expanding rings so uncontained Rifts keep creeping outward even when players roam away or enter the Nether.
  - Warps surface terrain, trees, and underground stone/caves down to `Y = -63`, leaving ~10–12% clustered Overworld stone/deepslate/cobblestone/coarse-dirt/stripped-log remnants while keeping ~88–90% decisively Nether-corrupted.
  - Protects Bedrock, containers, beds, beacons, workstations, and portal/containment blocks, and preserves `diamond_ore`, `redstone_ore`, `lapis_ore`, and `emerald_ore` (plus deepslate variants).
  - **Caves stay breathable**: open cave air never floods with lava; water (oceans, rivers, kelp, seagrass, waterlogged blocks, buckets, fluid spread) vanishes silently on contact without infinite refill loops; deep corrupted Deepslate (`Y = -32..-60`) in mature rifts rarely yields `minecraft:ancient_debris`.
- **Smoothly Blended Volcanic Smog up to the Clouds & Vanilla Nether Music**:
  - Dense volcanic smog shrouds vision at ground level and medium flight height, keeping Overworld sky and clouds obscured until rising into the cloud deck (`Y = 188..222`).
  - Samples a 5×5 Gaussian-weighted biome neighborhood around the player and smooths RGB fog color transitions over time so walking or straddling borders between Nether biomes glides smoothly without snapping.
  - Plays vanilla Nether biome music (`minecraft:music.nether.*`) inside corrupted areas.
- **Nether Mob Hunting, Kill Conversions & Rare Grounded Bastions**:
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
  - Rare, massive, deeply corrupted regions (`>= 150k` blocks, radius `>= 52`, `>= 80%` local surface corrupted, and `>= 112` blocks away from all Overworld players) can form full vanilla Jigsaw **Bastion Remnants** with all Jigsaw/Structure/Command/Barrier technical blocks scrubbed and blackstone/basalt foundations extruded down to solid terrain.

## Commands

- `/rift status` — Show tracked Rifts, containment status, corrupted block count, Bastions, and spread speed.
- `/rift scan` — Scan nearby loaded blocks (`+-24` around you) for active Nether Portals.
- `/rift sealcheck` — Force an immediate containment evaluation on all tracked Rifts.
- `/rift speed <4..1200>` *(OP level 2)* — Adjust block spread budget per tick (default `14` ops/tick for calm early progression).
- `/rift pause` / `/rift resume` *(OP level 2)* — Pause or resume Rift Bleed expansion.

## Package Layout & Building (Horizon Launcher `>= 0.2.78`)

Built as a flat native `.hmod` package targeting `horizon-mod-api` (`nightly-2026-11-18-0b14f8a`, zero mixins):
- Manifest: [`horizon-mod.toml`](horizon-mod.toml) (`id = "rift_bleed"`, `api = "0.2"`, `horizon = "0b14f8acfd97fa2c750e42795249dcc981178ca4"`)
- Crate & Mod Entry: [`Cargo.toml`](Cargo.toml), [`src/lib.rs`](src/lib.rs)

```powershell
& "$env:LOCALAPPDATA\Programs\Horizon Launcher\horizon-launcher.exe" native-build . --instance Horizon --version nightly-2026-11-18-0b14f8a --trust-native-code --profile optimized
```
