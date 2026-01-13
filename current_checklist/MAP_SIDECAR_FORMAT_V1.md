# Map Sidecar Format v1
Engine-owned spawn/marker data for BSP maps (clean-room, no entity lumps).

## Location
- Default lookup: `content/map_sidecars/<namespace>/<path>.toml`
- `map_id` must match the BSP asset id (example: `quake1:bsp/e1m1`).

## Schema
```toml
version = 1
map_id = "quake1:bsp/e1m1"
map_to_world_scale = 0.0254
space_origin = [0.0, 0.0, 0.0]

[[spawns]]
id = "start"
origin = [0.0, 1.0, 0.0]
yaw_deg = 90

[[markers]]
id = "doorway_hint"
kind = "doorway"
origin = [4.0, 1.0, -2.0]
```

## Notes
- `map_to_world_scale` is required so BSP units become meters.
- `space_origin` is optional metadata for partitioning/debug; it does not offset geometry.
- `spawns` and `markers` are optional; omit if unused.
