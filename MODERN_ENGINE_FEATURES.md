# Modern Engine Features — Gap Analysis and Add-on Roadmap

This project already covers the “core four” (render, input, audio, net) plus ECS and scripting.
The items below are common additions that become required quickly in a modern engine.

## 1) Asset pipeline and hot reload
**Why it matters:** iteration speed and correctness (dependency tracking, versioning).
- Asset IDs (content-addressed hashes or stable GUIDs)
- Import/cook step (platform-specific output)
- Dependency graph (texture → material → mesh, etc.)
- Optional hot reload (file watcher; live asset swap)

Suggested direction:
- Start with a minimal `asset_registry` in `engine_core` (IDs + cache).
- Add hot reload later via a file watcher (`notify`) once rendering and IO stabilize.

## 2) In-engine debug UI and console
**Why it matters:** inspection and tuning of net graphs, entity state, frame times.
- Quake-style console: commands + cvars (already planned)
- Debug overlay/HUD: frame time, memory, bandwidth
- Optional immediate-mode UI (e.g., egui) for inspectors

Suggested direction:
- Ship the console early; add egui after the renderer is stable.

## 3) Profiling and telemetry
**Why it matters:** you cannot optimize what you cannot measure.
- Instrumentation via `tracing` spans (CPU)
- GPU timing queries (wgpu supports timestamp queries on supported backends)
- Optional integration with an external profiler (Tracy or similar)

Suggested direction:
- Standardize on `tracing` everywhere immediately; decide on a profiler integration early.

## 4) Job system / background work
**Why it matters:** streaming IO/decode without stalling sim/render.
- Background jobs for: asset IO, audio decode, map preprocessing
- Deterministic handoff into authoritative simulation (fixed tick stays synchronous)

Suggested direction:
- Keep server simulation deterministic and synchronous.
- Use a bounded job queue (rayon or a custom worker pool) for non-sim tasks.

## 5) Input mapping and gamepads
**Why it matters:** modern expectation and clean abstraction.
- Action mapping layer (bindings, rebind UI)
- Gamepad support (e.g., via gilrs)

Suggested direction:
- Implement `InputAction` mapping now even if only keyboard/mouse at first.

## 6) Physics strategy (custom vs library)
**Why it matters:** time-to-fun and long-term maintainability.
- Quake harness can use BSP collision + custom controller.
- Wider game scope may benefit from a physics engine (Rapier) for rigid bodies and queries.

Suggested direction:
- Start custom for BSP walk; revisit after networking and replication stabilize.

## 7) Replication framework (ECS-integrated)
**Why it matters:** modern networking is mostly replication design.
- Explicit “replicated components” set
- Per-component serializers
- Snapshot delta/baselines, acking, bandwidth budgets
- Interest management (later)

Suggested direction:
- Make replication opt-in: marker component + registry of serializers.

## 8) Replays and determinism
**Why it matters:** debugging, testing, regression control.
- Input record/replay as a test artifact
- Deterministic state hashing across ticks

Suggested direction:
- Treat replay as a first-class test: run it in CI in no-assets mode.

## 9) Shader workflow
**Why it matters:** iteration speed and correctness (validation).
- Shader source management (WGSL)
- Optional hot reload
- Validation errors surfaced clearly

Suggested direction:
- Keep shaders minimal initially; add reload once pipelines stabilize.
