# COLLISION PLATFORM (v1)

This document defines the collision platform contract and how to extend it
without breaking the clean-room boundaries.

## Clean-room constraints
- Do not ingest Quake entities or QuakeC; use engine-owned sidecars for spawns.
- Do not reimplement step/slide logic outside Rapier KCC.
- Treat Quake formats as a content harness only.

## Collision world asset contract
Collision assets are partition-agnostic; the runtime selects chunks by bounds.

Required fields:
- `partition_kind`: `"quadtree2d"` today; `"octree3d"` later.
- `space_origin`: vec3, stable reference origin for partitioning.
- `root_bounds`: AABB (min/max) in engine meters.
- `map_to_world_scale`: float, applied before cooking.
- `chunks[]`:
  - `chunk_id` (opaque identifier)
  - `aabb_min` / `aabb_max`
  - `payload_ref` (triangle payload id)
  - optional `partition_hint` (debug only)
- `chunk_bounds_bvh`: AABB BVH over chunk bounds.

Asset mapping:
- `engine:collision_world/<path>` -> `content/collision_world/<path>`

## chunk_bounds_bvh v1 design
- Build over chunk AABBs, not triangles.
- Split by longest axis of chunk centers, median pivot.
- Emit a flat node array and leaf chunk index ranges.
- Runtime selection uses bounds intersection only.
- Debug commands must use BVH selection immediately.

Streaming extension (later):
- keep the BVH; stream chunks based on interest volume AABB queries.
- partition_kind remains metadata; selection is still bounds-based.

## Map source integration
To add a new map format:
1. Implement triangle soup extraction in `map_cook` (units in meters).
2. Add a cooker that emits the collision world asset schema.
3. Add/consume an engine-owned sidecar for spawns/markers.
4. Register the new asset kind in the asset platform rules.
5. Add validation and a minimal tool command to cook + verify.

## Movement profile additions
The shared controller module owns input -> motor -> collision -> camera.
To add a new profile:
1. Add a motor config and defaults.
2. Wire it through the controller motor selector.
3. Add cvars for tuning and a profile switch command.
4. Add an acceptance map and regression trace.

## Upgrade to octree3d
- Same asset contract; only `partition_kind` and cooker partitioner change.
- Runtime selection remains bounds intersection on chunks.
- Any partition node keys are debug hints only.
