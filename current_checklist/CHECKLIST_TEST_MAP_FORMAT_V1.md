# CHECKLIST - Test Map Format v1
**Engine-native graybox test maps for collision + movement validation**

## Goals
- Deterministic, agent-friendly TOML format for test maps.
- Authoritative Rapier colliders (required).
- Optional graybox rendering later (not required in v1).
- Forward-compatible with chunked collision cooking.

## File location and asset id
- Files live under `content/test_maps/<name>.toml`.
- Asset id form: `engine:test_map/<name>.toml`.
- Units: meters, seconds, radians in code. Degrees allowed in TOML.
- `map_to_world_scale` defaults to 1.0 if omitted.

## Format v1
```
version = 1
name = "arena_course"
map_to_world_scale = 1.0
space_origin = [0.0, 0.0, 0.0]
notes = "Movement + camera + collision course"

[chunking]
enabled = true
chunk_size = 32.0
padding = 1.0

[[solids]]
id = "floor"
kind = "box"
pos  = [0.0, -0.5, 0.0]
size = [80.0, 1.0, 80.0]
tags = ["ground"]

[[generators]]
id = "stairs_pack"
kind = "stairs"
pos = [-20.0, 0.0, -10.0]
yaw_deg = 90.0
step_count = 12
step_rise = 0.25
step_run  = 0.40
width     = 3.0
variants = [
  { step_rise = 0.20, step_run = 0.35, step_count = 12 },
  { step_rise = 0.30, step_run = 0.45, step_count = 12 },
]
```

### Solids (explicit primitives)
Supported `kind` values (v1):
- `box` (axis-aligned)
- `box_rot` (OBB via rotation)
- `ramp` (wedge mesh)
- `cylinder`

Fields:
- `id` (string, required, unique)
- `pos` (vec3, center)
- `size` (vec3 full extents)
- `yaw_deg` (optional, rotates around +Y)
- `rot_euler_deg = [pitch, yaw, roll]` (optional, degrees; overrides yaw)
- `tags` (optional list)

Notes:
- `cylinder` may specify `radius` and `height` instead of `size`.
- `ramp` may specify `length`, `width`, and `angle_deg` instead of `size`.

### Generators (parametric packs)
Supported `kind` values (v1):
- `stairs`
- `ramps`
- `corridors`

Determinism rules:
- Generator outputs must be stable for identical inputs.
- Variants are expanded in input order.
- Generated ids must be stable and unique.

#### Stairs
```
kind = "stairs"
pos = [x, y, z]     # base origin, first step center is offset from here
yaw_deg = 0.0
step_count = 12
step_rise = 0.25
step_run  = 0.40
width     = 3.0
variant_gap = 1.0   # optional; spacing between variants along local +Z
variants = [{...}]
```
Variants are offset along local +Z by `width + variant_gap` to avoid overlaps.

#### Ramps
```
kind = "ramps"
pos = [x, y, z]
yaw_deg = 0.0
width = 4.0
length = 10.0
angles_deg = [10.0, 20.0, 30.0]
align_base = false
gap = 3.0
```

#### Corridors
```
kind = "corridors"
pos = [x, y, z]
yaw_deg = 0.0
length = 20.0
height = 3.0
capsule_radius = 0.35
margins = [0.05, 0.15, 0.30, 0.50]
wall_thickness = 0.2
gap = 3.0
```

## Loader outputs
- Parsed `TestMap` (format schema).
- Expanded solids list with generated entries.
- Rapier colliders for each solid.

## Validator rules (v1)
- schema + version validation
- no NaNs or non-positive sizes
- `step_count > 0`, `angles_deg` non-empty, margins non-empty
- duplicate id detection (explicit and generated)

## Checklist
### M0 - Spec and asset integration
- [x] Define format v1 and document it.
- [x] Add `engine:test_map` asset id kind and resolver mapping to `content/test_maps/`.
- [x] Register `TestMapAsset` payload in the asset manager.

### M1 - Parser + expansion
- [x] Implement TOML parsing and validation.
- [x] Implement deterministic generator expansion (stairs, ramps, corridors).

### M2 - Loader
- [x] Build Rapier colliders from expanded solids.
- [x] Provide a minimal test to load a map and build colliders.

### M3 - Sample maps
- [x] Add flat lane, stairs, ramps, corridors, ledges, and pillar slalom maps.
- [x] Add arena-focused maps: strafe-jump build lane and soft-bhop landing pads.

### M4 - Renderer integration (graybox)
- [x] Load `engine:test_map/<name>.toml` as a renderable graybox scene (same flow as levels).
- [x] Provide a minimal renderer path for test map solids (boxes, ramps, cylinders).
- [x] Add thorough code comments around this integration so it is easy to rework later.

## Definition of done (DoD)
- All sample maps parse and expand without errors.
- Colliders can be constructed for each sample map.
- Asset id resolution works for `engine:test_map/...`.
- Format and rules are documented in this file.
- Renderer can load a test map into a visible graybox scene.
