# Pallet (Rust) — Engine/Client/Server Checklist (Quake-as-Testbed)

## Purpose
Build a clean-room, modern Rust engine/client/server stack using Quake assets as a **compatibility harness** to validate:
- asset IO + decoding
- BSP/world rendering
- player movement/collision
- audio playback (WAV + OGG/Vorbis)
- modern networking (authoritative server + replication)
- full ECS-based data model

This is **not** a perfect Quake recreation. Quake is a dated proving ground.

---

## Non-negotiables (clean-room + legal)
- No consultation of Quake source code (GPL releases, forks, ports) for implementation details.
- No decompilation of Quake binaries to derive algorithms.
- Repository must ship **zero** copyrighted assets.
- Quake harness loads assets only from a user-supplied, legally acquired installation directory.
- All asset parsers must be resilient: **no panics on untrusted input** (bounds checks, checked arithmetic, graceful errors).

See also: `CLEAN_ROOM_PROTOCOL.md`.

---

## Locked decisions
- Pallet integration crate: `pallet/`
- ECS: full ECS from day one (recommended: `bevy_ecs` standalone).
- Window/input: winit
- Rendering: wgpu
- Audio backend: miniaudio; decode OGG/Vorbis (music) + WAV (SFX)
- Scripting: Lua via `mlua`
- Networking: modern authoritative client/server with snapshot replication
  - Start: UDP with reliability channels (e.g., renet-style)
  - Later: QUIC adapter optional

---

## Workspace layout (normative)
- `pallet/` — integration root (only crate allowed to depend on everything)
- `platform_winit/` — window/event loop/input capture
- `render_wgpu/` — rendering backend
- `audio/` — miniaudio + streaming mixer + decode adapters
- `script_lua/` — Lua VM + sandbox + bindings
- `ecs/` — ECS integration + schedule conventions
- `engine_core/` — time, console/cvars, logging, config, VFS, asset registry
- `engine_game/` — Quake harness gameplay systems (not QuakeC emulation)
- `net/`
  - `net_transport/` — UDP adapter (later QUIC adapter)
  - `net_protocol/` — serialization + snapshots + replication rules
  - `client/`
  - `server/`
- `compat_quake/` — Quake-specific parsing (PAK/BSP/etc.) as an isolated plugin
- `tools/` — CLI utilities implementing the contract in `tools/README.md`

---

## Operational commands (contract)
These commands must exist and remain stable once the repo is implemented:

- Quality gates:
  - `just ci`  (fmt + clippy + test + deny)
- CI-friendly smoke test (no external assets):
  - `just smoke`
- Local Quake harness smoke test:
  - `just smoke-quake "<QUAKE_DIR>" <MAP>`

These commands are defined in `justfile`.

---

## Global Definition of Done (merge gates)
A PR is mergeable only if:
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -D warnings`
- `cargo test --all-features`
- `cargo deny check`
- Any parsing/decoding change adds tests and a fuzz/property target.
- Parsers return `Result` errors; no panics on malformed inputs.

---

# Milestones

## M0 — Workspace scaffold + CI + Pallet window
- [x] Workspace builds on Windows + Linux (macOS optional).
- [x] CI wired (fmt/clippy/test/deny).
- [x] `pallet` boots: creates a window + clears a frame via wgpu.
- [x] Structured logging + error conventions.

**DoD evidence**
- [x] `cargo run -p pallet` opens a window and renders a clear color, prints timing.

---

## M1 — ECS baseline (full ECS)
- [x] `ecs` integrates bevy_ecs standalone.
- [x] Core components: `Transform`, `Velocity`, `Camera`, `PlayerTag`.
- [x] Schedules:
  - `FixedUpdate` (authoritative sim tick)
  - `Update` (presentation, audio decisions)
- [x] Determinism harness: record input stream → replay → identical state hash.

**DoD evidence**
- [x] A “moving entity” demo where replay yields identical final state hash.

---

## M1.5 ??" Net interface and loopback transport
- [x] `net_protocol`:
  - `InputCommand`
  - `Snapshot` (single entity transform ok)
  - `Connect`/`Disconnect` control messages
- [x] `net_transport`:
  - `Transport` trait (send/recv + time + MTU budget)
  - `LoopbackTransport` (in-memory queues)
- [x] `client`/`server` crates:
  - server processes input, advances fixed tick, emits snapshots
  - client sends input, applies snapshots (no prediction yet)

**DoD evidence**
- [x] Loopback client connects, moves one entity, receives snapshots for N ticks, exits.
- [x] Client can swap transports without gameplay changes.

---


## M2 — VFS + Quake probing (no assets committed)
- [x] `engine_core::vfs`:
  - search paths (base + mod dirs)
  - mount points
  - file open/read/list
- [x] `compat_quake::pak` parses PAK safely (directory table, offsets).
- [x] `tools pak list` lists contents of user-supplied `id1/pak0.pak`.

**DoD evidence**
- [x] `cargo run -p tools -- pak list --quake-dir <path>` prints entries and exits 0.
- [x] Fuzz target: PAK header/directory parsing.

---

## M3 – Minimal decode pipeline: show content on screen
- [x] Implement minimum image path (PCX or other Quake-friendly images).
- [x] Palette handling if required.
- [x] Render decoded image as texture on a quad (wgpu).

**DoD evidence**
- [x] `cargo run -p pallet -- --quake-dir <path> --show-image <asset>` shows it.

---

## M4 – BSP (subset) parse + render static world geometry
- [x] `compat_quake::bsp` parses:
  - header + lump table
  - vertices/edges/faces sufficient to triangulate and draw
- [x] Build static mesh buffers and render in `render_wgpu`.
- [x] Fly camera: WASD + mouse look.

**DoD evidence**
- [x] `--map e1m1` (or similar) renders geometry and allows free-fly.

---

## M5 — Collision + basic player movement
- [x] World collision queries (initially simplified).
- [x] Player controller system (tunable accel/friction/gravity).

**DoD evidence**
- [x] Walk around without falling through floors in typical cases.

---

## M6 — Audio: WAV + OGG/Vorbis streaming
- [x] miniaudio device init + mixer.
- [x] WAV decode + playback.
- [x] OGG/Vorbis decode + streaming (for rerelease music if present).
- [x] Restart/stop track.

**DoD evidence**
- [x] Keypress plays WAV.
- [x] Map start streams an OGG track if present.

---

## M7 — Modern networking baseline (authoritative server + replication)
Goal: modern replication, not Quake’s protocol.

### Architecture
- Server is authoritative and runs fixed tick.
- Client sends input commands with sequence numbers.
- Server emits snapshots; client interpolates remote entities and predicts local player.

### Implementation
- [x] `net_transport`: UDP + reliability channels (renet-style).
- [x] `net_protocol`:
  - snapshot schema for entities + replicated components
  - baseline/delta mechanism (start simple)
- [x] Single-player uses loopback client/server with identical codepaths.

**DoD evidence**
- [ ] Headless dedicated server accepts one client.
- [ ] Client connects, receives snapshots, and moves around.
- [ ] Replay test yields stable authoritative results.

---

## M8 — Lua scripting hooks (game glue, not QuakeC emulation)
- [ ] Lua VM + sandbox:
  - no filesystem by default
  - no OS calls
  - CPU budget per frame
- [ ] Expose:
  - console commands
  - spawn entity + transform
  - play sound
  - callbacks (on_tick, on_key, on_spawn)

**DoD evidence**
- [ ] Script spawns an entity and binds a key to play sound.

---

## M9 — Theora video (later / optional)
- [ ] Ogg container parsing for `.ogv`.
- [ ] Theora decode.
- [ ] Upload frames to GPU and render.

**DoD evidence**
- [ ] `--play-movie <file>` renders frames smoothly with sound.

---

# First big test (overall acceptance)
Given a user path to a legally acquired Quake install:
- [ ] Mount `id1/pak0.pak` (and additional paks if present)
- [ ] Load and render a BSP map
- [ ] Walk/fly with collision
- [x] Play WAV SFX and stream OGG music if present
- [ ] Works in:
  - single-process loopback client/server
  - dedicated server + network client

---

# Modern engine add-ons (recommended)
See `MODERN_ENGINE_FEATURES.md` for detail. At minimum, plan for:
- asset registry + (later) hot reload
- debug UI/console + entity inspection
- profiling/telemetry standardization
- background job system (non-deterministic work off the sim thread)
- input action mapping + gamepad support
- replay artifacts in CI
- shader workflow and (later) hot reload



