# CHECKLIST 5 — Collision + Character Controller Platform v1
**Rapier KCC + Quadtree2D Collision Chunks + Dual Motors (arena / rpg) + Future Octree Space**

## Units and conventions
- **Distance:** meters (world units are meters)
- **Time:** seconds
- **Angles:** radians in code; degrees are allowed in authored config (e.g., TOML) and must be converted at load
- **Velocity:** m/s
- **Acceleration:** m/s²
- **Gravity:** m/s² (project default should be stated in the controller/collision layer)
- **Axes:** right-handed, +Y up, planar XZ, forward = -Z

Rapier is unitless; this project standardizes on the scale above for numeric stability and consistency.


### Units note: BSP/map scale
- Imported map formats (Q1/Q3) must apply an explicit `map_to_world_scale` so geometry is expressed in **meters** before cooking collision.
- Store the chosen scale with cooked collision artifacts so runtime and tools remain consistent.


## Scope and constraints
- Collision/stepping uses **Rapier Character Controller (KCC)** stepping logic (no bespoke step/slide implementation).
- Player collider is **capsule**, **per-profile sizing**.
- Two movement “motors” (tuning profiles):
  - **arena**: high-responsiveness, skill movement emphasis
  - **rpg**: heavier accel/decel, stable navigation feel
- Maps: **Q1 BSP** and **Q3 BSP** supported for collision cooking.
- **No Quake entities ingestion** (clean-room); spawns/markers come from engine-owned sidecar data.
- Collision cooking targets a **format-agnostic triangle soup → chunked static colliders** pipeline that scales to future full-mesh maps.
- **Required now:** `chunk_bounds_bvh` (minimal) is implemented and exercised (debug selection / future streaming).

---

## Asset contract (must-haves)
The cooked collision asset stores these fields (explicitly):
- `partition_kind`: `"quadtree2d"` (upgrade path: `"octree3d"`)
- `space_origin`: vec3 (stable reference origin for the partition)
- `root_bounds`: AABB (min/max in engine space; stable per map build)
- `map_to_world_scale`: float (per-map scale used to convert map units to meters)
- `chunks[]`:
  - `chunk_id` (opaque)
  - `aabb_min/aabb_max`
  - `payload_ref` (static collider triangles/indices blob, or equivalent)
  - optional `partition_hint` (debug only; e.g., quadtree node key)
- **Required:** `chunk_bounds_bvh` (selection acceleration over *chunk AABBs*, not triangles)
  - used by dev/debug selection immediately
  - later extended for streaming and interest-volume selection

**Runtime must treat chunking as an implementation detail**: load colliders from `chunks[]`, select by bounds intersection, never by quadtree coordinates.

---

## Acceptance tests (v1 target)
These become your harness “must pass” gates:

### Arena suite (required)
1. **Crisp stop**
2. **Crisp redirect**
3. **Strafe jump build**
4. **Doorway test** at elevated speed
5. **Soft bhop retention** (required policy; see M4)
6. **Grounded stability** (avoid flicker across tiny edges / step-downs)

### RPG suite (required)
1. Predictable stopping distance (no “ice” unless configured)
2. Stable slopes/steps traversal (no jitter, no unintended hops)
3. Conservative air control behavior (per policy)
4. Grounded stability

---

## Milestones

### M0 — Repo layout, naming, and “clean-room safe” boundaries
- [x] Create module boundaries:
  - `physics_rapier/` (Rapier world integration)
  - `character_collision/` (Rapier KCC wrapper + collision profiles)
  - `character_motor_arena/` and `character_motor_rpg/`
  - `player_camera/` (view-only derivation)
  - `map_cook/` (Q1/Q3 → collision asset)
- [x] Establish naming conventions in code + console surface:
  - snake_case, lowercase, no dots
  - command names verb-prefixed; dev-only prefixed `dev_`
- [x] Add an explicit “no step/slide reimplementation” note in module docs (policy + rationale).

### M1 – Rapier integration baseline (world, stepping, debug)
- [x] Integrate Rapier with a stable “physics tick” entrypoint.
- [x] Stand up a minimal static-collider scene and verify:
  - capsule contact and movement via KCC
  - stepping over a test stair set
  - slope limits behaving as expected
- [x] Add dev debug draw hooks (colliders, character shape, contacts) behind `dev_` toggles.

### M2 – `character_collision`: Rapier KCC wrapper + collision profiles
- [x] Implement `character_collision` as the single owner of:
  - capsule geometry
  - KCC configuration (step height, slope limit, snap-to-ground, etc.)
  - move request + result
  - grounded/contact state output
- [x] Define `collision_profile` schema (data-driven):
  - `capsule_radius`, `capsule_height`
  - `step_height`
  - `max_slope_angle` (or equivalent Rapier parameters)
  - `ground_snap_distance`
  - any skin/margin values required by KCC
- [x] Support **per-profile capsule size** (arena vs rpg).
- [x] Provide results/events needed by motors/camera:
  - post-move pose
  - grounded boolean + ground normal (if available)
  - hit flags (wall, ceiling) as needed for tuning/telemetry

### M3 — Extract controller stack (input → motor → collision → camera)
- [x] Create a stable “player controller module” that composes:
  1) input adapter (raw input → intent)
  2) motor step (intent → desired velocity/displacement)
  3) collision step (`character_collision` move)
  4) camera update (pose → view)
- [x] Ensure camera is derived (no physics feedback loops beyond yaw/pitch).
- [x] Provide a minimal integration example map that demonstrates:
  - spawn
  - movement
  - stepping
  - slope walking

### M4 — `character_motor_arena` (tunable, stepping delegated to KCC)
- [x] Implement arena motor as **pure velocity intent logic** (no sweep/step math).
- [x] Implement tunables for responsiveness and skill movement (final details to be aligned with the arena spec revision).
- [x] **Required:** implement **jump buffering** as an arena motor policy:
  - `jump_buffer_enabled`
  - `jump_buffer_window` (seconds)
- [x] **Required:** implement **Soft bhop retention policy** (arena default):
  - Define “soft bhop” explicitly as a friction application rule, e.g.:
    - reduce/skip ground friction for a small grace window around landing when jump is held or buffered, and/or
    - skip/reduce friction on the tick a jump is initiated
  - Make the policy data-driven:
    - `frictionless_jump_mode` = `none|soft|hard` (arena default = `soft`)
    - grace window and friction scaling parameters
- [x] Add a dev movement overlay (crosshair + speed + golden angle feedback) behind `dbg_movement`.
- [x] Gate M4 with the Arena acceptance suite (including doorway-at-speed and soft bhop retention).
  - Use test maps: `flat_friction_lane`, `strafe_jump_build`, `corridors_and_doors`, `soft_bhop_course`.
  - Run with `cargo run -p pallet -- --map engine:test_map/<name>.toml` and verify behavior manually.

### M5 – `character_motor_rpg` (tunable, stability-first)
- [x] Implement RPG motor with:
  - heavier accel/decel curves
  - optional input smoothing / turn response controls
  - more conservative air control (or none by default)
  - stronger preference for ground stability (snap tuned via collision_profile)
- [x] Add distinct default capsule size + step/slope parameters for RPG.
- [x] Gate M5 with the RPG acceptance suite.
- Note: tiny stair stutter remains; revisit step tuning later if it persists.
- Note: downhill snap-to-ground pass improved ramps but was reverted; consider re-adding if needed.

### M6 — Collision asset format v1 (stores partition_kind, space_origin, root_bounds, chunk_bounds_bvh)
- [x] Define `collision_world` asset schema (versioned):
  - `partition_kind`, `space_origin`, `root_bounds`, `map_to_world_scale`
  - `chunks[]` (id, bounds, payload refs)
  - **required** `chunk_bounds_bvh` (AABB BVH over chunk bounds)
- [x] Implement minimal `chunk_bounds_bvh` builder at cook-time:
  - split by longest axis of the set bounds
  - split by median of chunk centers
  - emit flat node array + root index + leaf chunk index ranges
- [x] Register in your Asset Platform v1 “compat registry” for versioning/migrations.
- [x] Provide a validation tool:
  - checks bounds correctness
  - checks triangle counts and chunk budgets
  - checks `map_to_world_scale` presence and consistency
  - checks determinism of `space_origin/root_bounds` rules

### M7 — Quadtree2D cooker for Q1 + Q3 maps (collision-only, no entities)
- [x] Implement geometry extraction to a common triangle soup:
  - Q1 BSP → collision triangles
  - Q3 BSP → collision triangles
- [x] Build **quadtree2d** partitioner (columns):
  - stop conditions: `max_tris_per_leaf`, `min_leaf_size_xy`, `max_depth`
  - triangle assignment policy (centroid-in-XY recommended for v1)
- [x] Emit `collision_world` asset with required stored fields (M6).
- [x] Explicitly skip entity lumps; instead:
  - define/consume an engine-owned sidecar format for spawns/markers (map_id keyed).

### M8 — Runtime loader: instantiate chunk colliders in Rapier + selection via chunk_bounds_bvh
- [x] Load `collision_world` asset and instantiate static colliders per chunk.
- [x] Add a map switch path that:
  - unloads prior colliders cleanly
  - loads new chunk set
  - respawns controller with appropriate profile defaults
- [x] Implement `select_chunks(interest_volume)` abstraction:
  - in v1: may return all chunks for “always loaded” mode
  - **required:** implement BVH-backed selection path used by dev/debug tooling immediately
- [x] Add performance counters (collider count, triangle count loaded, KCC query cost).

### M9 — Console + debug surface + test harness
- [ ] Add commands (examples; adapt to your naming rules):
  - `dev_collision_draw` (toggle)
  - `dev_collision_dump_near_player radius=...` (uses `chunk_bounds_bvh`)
  - `player_set_profile arena|rpg`
  - `player_dump_state`
  - `player_tune_set <param> <value>` (dev-only) / `player_tune_list`
- [ ] Implement automated harness maps for acceptance tests:
  - crisp stop lane
  - 90° redirect pad
  - strafe-jump build course
  - doorway corridor at speed
  - **soft bhop retention course** (repeated landings with buffered/held jump)
- [ ] Record/replay input traces for regression (strongly recommended; minimal implementation acceptable).

### M10 — Convergence and documentation
- [ ] Migrate gameplay to the extracted controller module (remove/disable old movement path).
- [ ] Document:
  - the partition-agnostic collision asset contract
  - clean-room constraints (no Quake entities, no bespoke step logic)
  - how to add new map sources (future full-mesh path)
  - how to add a new movement profile
  - minimal `chunk_bounds_bvh` design and how to extend it for streaming
- [ ] Add “upgrade to octree3d” note:
  - same asset contract; only `partition_kind` and cooker partitioner change
  - runtime remains bounds-driven selection + chunk loading

---

## Explicit “space to upgrade to octree3d” requirements (baked into v1)
- [ ] `partition_kind` is an enum and stored in the asset (not implied).
- [ ] `space_origin` and `root_bounds` are stored and used as the stable reference for partitioning and debugging.
- [ ] `chunk_bounds_bvh` is present and used for selection (debug now; streaming later).
- [ ] Runtime selection uses **chunk AABB intersection**, not quadtree coordinates.
- [ ] Any quadtree node keys are **debug hints only** (never required for correctness).
