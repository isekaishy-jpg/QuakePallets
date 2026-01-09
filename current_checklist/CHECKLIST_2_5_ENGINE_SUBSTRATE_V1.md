# CHECKLIST 2.5 — Engine Substrate v1
## Raw Content Mounts + Path Policy + Jobs/Workers + Failure Policy

### Objective
Create a minimal engine substrate that supports **raw content sources** (directory / Quake PAK / id Tech 3 PK3) with deterministic mount precedence, a single path/config policy (no CWD dependence), and an engine-wide Jobs/Workers v1 with explicit failure behavior.

### Scope notes
- This checklist is for **raw content** only. Engine-native packages/bundles are **TBD**.
- **Dir mounts remain first-class forever** for development and for engine-native content that is not packaged yet.
- Existing Quake package loading already works today; this checklist **migrates** it behind the unified mount/VFS surface.

---

## M0 — Establish substrate invariants and minimal repo layout

- [x] Define and document the invariant: *No runtime-critical file access depends on current working directory (CWD).*
- [x] Create or confirm a canonical on-disk content tree for development builds (e.g., `content/` with `content/config/`).
- [x] Create or confirm a gitignored dev override root (e.g., `.pallet/`).
- [x] Identify current “config-like” files living in repo root and plan their canonical location under `content/config/...` (move now or explicitly defer with rationale).

**DoD**
- [x] A developer can run the app/tools from a different working directory and still resolve content/config correctly (or there is a documented short-term exception list with owners).

---

## M1 — Path Policy v1 (content root + config locations + precedence)

### Required behaviors
- [x] Implement a single shared “paths/policy” module (engine-owned) that provides:
  - [x] `content_root` resolution (packaged default relative to executable, dev default, CLI override)
  - [x] `dev_override_root` (optional, gitignored)
  - [x] `user_config_root` (OS config dir; reuse existing Settings logic)
- [x] Define config categories and canonical roots:
  - [x] **Shipped defaults** under `content_root/config/...`
  - [x] **Dev overrides** under `.pallet/config/...` (or equivalent)
  - [x] **User config** under OS config dir
- [x] Define deterministic precedence for resolving config-like inputs (playlist/scripts/cvars):
  - [x] CLI explicit override
  - [x] Environment override (optional; debug-only)
  - [x] Dev override
  - [x] Shipped defaults
  - [x] User config
  - [x] Built-in defaults
- [x] Provide resolver diagnostics (“why this file was chosen”) suitable for logs and console output.

**DoD**
- [x] Both the main app and tools resolve config-like inputs by **name/virtual path**, not by repo-relative raw paths.
- [x] There is a clear, testable precedence rule and a way to see resolution provenance.

---

## M2 — VFS Mount System v1 (raw content mounts, deterministic precedence)

### Mount model (engine-owned)
- [x] Introduce or formalize a mount abstraction that supports multiple sources with precedence:
  - [x] Ordered mount table (explicit precedence)
  - [x] Open/read via virtual path
  - [x] Optional: “provenance” reporting (which mount satisfied the open)
  - [x] Path traversal safety (no `..` escape, normalized virtual paths)

### Mount types (raw content only)
- [x] Directory mount (dev and unpackaged engine-native content)
- [x] Quake PAK mount (adapter over existing functionality)
- [x] PK3 mount (zip) for id Tech 3 testbed readiness

**DoD**
- [x] A caller can open the same virtual path via one unified VFS API regardless of whether the bytes come from a directory, PAK, or PK3.
- [x] Precedence is deterministic and test-covered (same file exists in multiple mounts → higher precedence wins).
- [x] “Mount provenance” is visible (log/console) for debugging.
- [x] Path traversal safety is enforced and test-covered.

---

## M3 — Migrate existing Quake package loading behind VFS mounts

- [x] Identify all current code paths that read Quake package contents directly (bypassing VFS).
- [x] Replace direct package reads with VFS opens through the mount table.
- [x] Ensure Quake content paths map cleanly into the chosen virtual namespace (see M4).

**DoD**
- [x] There are no remaining “special case” Quake package file opens outside the VFS/mount system (exceptions must be documented and time-bounded).
  - Note: `tools pak` is a package inspector and still parses PAK directly (intentional tooling exception).

---

## M4 — Raw content namespaces and mount plan (external testbeds)

### Namespace conventions (avoid future collisions)
- [x] Define virtual roots for raw test content, e.g.:
  - [x] `raw/quake/...`
  - [x] `raw/q3/...`
- [x] Ensure the convention leaves room for future engine-native content roots (e.g., `content/...`).

### Mount plan (not a package manager)
- [x] Define how the app/tools specify raw mounts:
  - [x] CLI flags (dir/pak/pk3) and/or
- [x] A simple mount manifest under `content/config/`
- [x] Make mount ordering explicit and reproducible.

**DoD**
- [x] A user can point the engine at external test content (raw dir/PAK/PK3) and the engine can read bytes via stable virtual paths.

---

## M5 — Jobs/Workers v1 (engine-wide execution substrate)

### Required behaviors
- [x] Implement a minimal Jobs system with:
  - [x] At least two classes/queues (**IO** vs **CPU**) or a clearly documented alternative
  - [x] Bounded queues with an explicit backpressure policy (choose one: block / fail / drop-low-priority)
  - [x] Main-thread completion pump (results delivered and applied on main thread)
  - [x] Deterministic **inline mode** for tests/harness (no threads, stable ordering)
  - [x] Minimal telemetry (queue depth, active workers)

### Failure and cancellation (v1)
- [x] Define a v1 cancellation mechanism (best-effort is fine).
- [x] Define worker failure behavior:
  - [x] What happens on worker panic?
  - [x] How is the failure surfaced (log + sticky error + command to inspect)?
  - [x] Does the system restart workers or transition to a degraded mode?

**DoD**
- [x] A small test proves: async job runs → completion delivered via pump.
- [x] Inline deterministic mode is used in at least one test.
- [x] Worker failure behavior is deterministic and visible (no silent hangs).

---

## M6 — Minimal observability: logging + panic policy

- [x] Define a single logging façade used consistently across crates (even if simple).
- [x] Install a panic hook that makes failures visible and actionable:
  - [x] logs panic details
  - [x] marks a “sticky error state” that can be queried (console/log)
- [x] Ensure Jobs integrates with this policy (panic in worker is surfaced, not ignored).

**DoD**
- [x] A deliberate induced failure in a worker produces a clear diagnostic and an explicit engine state change (not a silent stall).

---

## M7 — End-to-end proof: “open bytes from mounts” tool/command

- [x] Add a minimal debug command or tool:
  - [x] `vfs.stat <vpath>` or `vfs.hash <vpath>` (size + hash is sufficient)
- [x] Demonstrate it works against:
  - [x] Directory mount
  - [x] Quake PAK mount
  - [x] PK3 mount

**DoD**
- [x] One command/tool can report size/hash for a file coming from each mount type.
- [x] Output includes mount provenance when in verbose/debug mode.

---

## Completion criteria

- [x] Path/config resolution is deterministic, cross-app/tool, and not CWD-dependent.
- [x] Raw content mounts (dir/PAK/PK3) are unified under the same VFS surface with precedence and tests.
- [x] Existing Quake package loading is migrated behind VFS mounts.
- [x] Jobs/Workers v1 exists with deterministic inline mode and defined panic/failure behavior.
- [x] Observability exists to debug mount resolution and job failures.
