# ARCHITECTURE — Pallet Engine / Client / Server (ECS-first, Modern Netcode)

This document defines a “known working” architecture for a fast-paced multiplayer game stack:
- authoritative server simulation
- client prediction + reconciliation (local player)
- snapshot interpolation (remote entities)
- ECS-first gameplay model
- transport-agnostic networking

Quake is used only as a *content harness* (assets/maps) to validate the pipeline. It is not a behavior-accurate reimplementation.

---

## 1) Core design principles

### 1.1 Authoritative simulation
- The authoritative world state exists only on the server.
- Clients send *input commands* (intent), not state.
- The server produces *snapshots* of the authoritative state for clients.

### 1.2 Determinism boundaries
- The authoritative simulation loop is deterministic *within a build* (same binary, same config, same inputs).
- Non-deterministic work (asset IO, audio decode, shader compilation) is off the sim thread and handed over through explicit queues.

### 1.3 Layering
Maintain strict layering for maintainability:
1. **Transport**: packet IO, connection management (UDP now; QUIC later).
2. **Protocol**: message schemas, serialization, versioning, compression.
3. **Replication**: ECS component replication rules, snapshots/deltas, relevancy.
4. **Game**: ECS systems, rules, scripting.

---

## 2) Repository / crate responsibilities

### 2.1 `pallet` (integration root)
Owns lifecycle and wiring:
- create window/input (via `platform_winit`)
- init renderer (via `render_wgpu`)
- init audio (via `audio`)
- init scripting (via `script_lua`)
- start client/server (loopback or networked)
- run main loop: collect input → run ticks → render

Hard rule:
- `pallet` is the only crate permitted to depend on *all* subsystems.

### 2.2 `ecs`
- Defines schedules/stages and shared component types
- Encodes the “tick model” (fixed vs variable)
- Provides common resources (Time, InputState, NetState, AssetDb handles)

### 2.3 `server`
- Owns the authoritative ECS `World`
- Runs only the deterministic, fixed-tick systems (plus replication emission)
- Accepts input commands; outputs snapshots

### 2.4 `client`
- Owns a presentation ECS `World` (or two Worlds; see §4.4)
- Runs:
  - prediction systems (fixed tick) for locally controlled entities
  - interpolation/presentation systems (variable update) for remote entities
- Applies snapshots and reconciles predicted state

### 2.5 `net_protocol`
- Pure data definitions and codec:
  - InputCommand
  - Snapshot
  - DeltaSnapshot (optional)
  - Reliable control messages (connect/sign-on/config)
- No dependencies on winit/wgpu/audio/lua.

### 2.6 `net_transport`
- Adapters:
  - UDP + channels (initial)
  - QUIC adapter (later)
- Exposes a stable trait `Transport` to `client/server`.

### 2.7 `engine_core`
- Console/cvars, logging, config
- Virtual filesystem + asset registry
- Time and tick configuration
- Replay harness plumbing

### 2.8 `engine_game`
- The “game rules” plugin:
  - movement controller
  - spawn logic
  - map load orchestration
  - interaction systems
- Explicitly *not* QuakeC emulation.

### 2.9 `compat_quake`
- Isolated format parsing: PAK, BSP subset, etc.
- Must not leak Quake-specific types into `engine_core` or `engine_game`.
- Exposes a “content adapter” interface:
  - mount pak
  - load map mesh + collision data
  - enumerate sounds/music paths

---

## 3) The tick model

### 3.1 Time domains
Define three clocks:
- **Sim tick**: fixed timestep (e.g., 60 Hz).
- **Net tick**: fixed cadence for snapshot emission (often <= sim tick; e.g., 20–30 Hz).
- **Render/update**: variable per-frame loop; consumes interpolation buffers and builds draw lists.

### 3.2 Server tick loop (authoritative)
At each sim tick:
1. Drain input commands per client for this tick (or last-known).
2. Run deterministic ECS systems (movement, collision, game rules).
3. Advance `server_tick += 1`.
4. If `server_tick % snapshot_stride == 0`, build and send snapshots/deltas.

### 3.3 Client tick loop (prediction)
At each local sim tick:
1. Sample local input, build `InputCommand{tick, seq, actions}`.
2. Send to server; buffer the command locally.
3. Apply command to predicted local entity (prediction systems).
4. Advance `client_tick += 1`.

### 3.4 Client render/update loop (interpolation)
Per rendered frame:
1. Read latest received snapshots.
2. Maintain a snapshot buffer keyed by server tick.
3. Choose a render time `t = now - interp_delay`.
4. Interpolate remote entities between snapshots around `t`.
5. Build render/audio/UI outputs.

---

## 4) State flow: commands, snapshots, reconciliation

### 4.1 Inputs
Input command fields (typical minimum):
- `client_seq`: monotonic per-client sequence
- `client_tick`: local tick index
- `dt`: fixed timestep (implicit by tick rate)
- `actions`: movement vector, buttons, view angles
- Optional: `checksum` for debugging

### 4.2 Snapshots
Snapshot fields:
- `server_tick`
- `ack_client_seq` (what server has processed)
- `entities`:
  - `net_id` (stable identifier)
  - component state (replicated subset only)
- Optional: `baseline_id` for delta compression

### 4.3 Prediction and reconciliation (local player)
When a snapshot arrives:
1. Identify the snapshot tick `T`.
2. Overwrite local predicted entity state with authoritative state at `T`.
3. Replay buffered input commands for ticks `(T+1 .. current_client_tick)` to catch up.
4. If error is large, optionally “smooth” correction over a short window.

### 4.4 ECS world organization options
Two workable patterns:

**Option A (recommended initially): Single client world**
- Local predicted entity is marked `Predicted`.
- Remote entities are marked `Interpolated`.
- Systems are gated by these markers.

Pros: simpler.
Cons: stricter care needed to avoid mixing predicted/interpolated paths.

**Option B: Dual worlds**
- `world_predicted`: only predicted entities (local player).
- `world_render`: interpolated view of everything for rendering.

Pros: very clean separation.
Cons: more complexity and data bridging.

Recommendation: start with Option A; move to Option B if complexity warrants.

---

## 5) Replication design for ECS

### 5.1 Replicated component registry
Replication is opt-in:
- A component is replicated only if registered in a `ReplicationRegistry`.
- Each component has:
  - serializer
  - deserializer
  - quantization rules (e.g., position precision)
  - change detection strategy (dirty bit, threshold)

### 5.2 Entity identity
- Assign a stable `NetId` to replicated entities (server-owned).
- NetId reuse rules must be explicit (avoid rapid reuse; use generation counters).

### 5.3 Interest management (later milestone)
Start with “send everything” for one client. Then add:
- distance-based relevancy
- area/zone partitioning
- per-entity update rates

---

## 6) Transport and reliability

### 6.1 Channels
A practical baseline uses channels:
- **Unreliable sequenced**: snapshots (newer supersedes older)
- **Reliable ordered**: connect/sign-on, config, chat/admin
- **Unreliable**: transient events (optional; can be reliable at first)

### 6.2 MTU and fragmentation
- Define a maximum packet size (MTU budget) and enforce it in `net_protocol`.
- If snapshots exceed MTU:
  - split into fragments with sequence/fragment IDs
  - or reduce replication set by interest management

---

## 7) Testing and verification (AI and CI aligned)

### 7.1 Required automated tests
- Unit tests:
  - codecs (serialize/deserialize roundtrip)
  - deterministic movement step (pure functions)
  - replication change detection
- Property/fuzz tests:
  - parsers (PAK/BSP)
  - snapshot decode (bounds and validity)
- Integration tests (no-assets):
  - loopback server+client can connect
  - exchange snapshots for N ticks
  - deterministic replay produces identical final hash

### 7.2 Operational commands
Once implemented, these commands must remain stable:
- `just ci`  (fmt + clippy + test + deny)
- `just smoke`  (no-assets smoke test)
- `just smoke-quake "<QUAKE_DIR>" <MAP>`  (local Quake harness smoke test)

---

## 8) Security posture (baseline)
- Treat all network input as untrusted.
- Treat all asset input as untrusted.
- No panics on malformed input; return structured errors.
- Add explicit limits:
  - maximum entity count per snapshot
  - maximum component payload sizes
  - maximum fragment counts per message

---

## 9) Roadmap notes
- QUIC transport adapter is a later enhancement; do not couple protocol to transport.
- Add video (Theora) as a separate feature-gated pipeline once audio+render are stable.
- Add debug UI (e.g., an inspector) after M4/M5 when world rendering exists.

End of document.
