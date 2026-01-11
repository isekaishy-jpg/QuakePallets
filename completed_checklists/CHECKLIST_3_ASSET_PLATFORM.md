# CHECKLIST 3 — Asset Platform v1
**Async Asset Graph + Namespaced IDs + Compat Registry + Tools + Console Surface**

## Objective
Create a durable asset platform so later checklists (maps/materials/rendering) can focus on their domains—without revisiting packaging, path rules, or loader design.

## Non-goals
No PBR/material compilation, no light baking/navmesh building (only slots), no editor integration, no perfect Quake recreation.

## Hard constraints
- No per-frame disk I/O or repeated parsing for the same asset.
- Async loading is first-class (handles + states).
- No `wgpu` device usage on worker threads (UploadQueue boundary).
- Cache keyed by `AssetKey` (namespace/kind/path), not filesystem path.
- No implicit cross-namespace fallback.
- Deterministic ordering for tool outputs and generated indices/manifests.
- Tools do not require GPU/windowing.

---

## A. Foundations

## M0 — Asset Identity

### Tasks
- [x] Define canonical ID grammar: `"<namespace>:<kind>/<path>"`
 - examples: `engine:texture/ui/console_bg`, `quake1:raw/gfx/conback.lmp`
- [x] Canonicalization rules (enforced at parse time):
 - [x] lowercase, `/`
 - [x] reject `..`, empty segments, leading `/`, duplicate `//`
 - [x] compat `raw` preserves extensions
- [x] Charset + length constraints:
 - [x] define allowed charset explicitly
 - [x] define max length explicitly
- [x] Implement `AssetKey { namespace, kind, path, hash }`
 - [x] stable hash from canonical form
 - [x] keep canonical string for diagnostics
- [x] Reserved namespaces/kinds policy:
 - [x] reserve: `engine`, `quake1` (future: `id3`, `id4`, `tool`, `test`)
 - [x] define unknown namespace/kind policy (reject vs allow w/ warning)
- [x] Typed wrappers:
 - [x] `EngineTextureId`, `EngineScriptId`, `EngineLevelId`
 - [x] `Quake1RawId`
- [x] Unit tests: canonicalization, invalid rejection, roundtrip formatting.

### Tools
- [x] `tools content lint-ids`

**DoD**
- [x] IDs normalize deterministically; invalid IDs are rejected with actionable errors.
- [x] `lint-ids` fails on violations and exits non-zero.

---

## M1 — Mounts and Resolution

### Tasks
- [x] Define a printable asset source table/view (derived from VFS mounts):
 - [x] engine `content_root` mount
 - [x] dev override mount (if supported)
 - [x] `quake1` mount(s) with explicit order
- [x] Resolver interface:
 - [x] `resolve(AssetKey) -> ResolvedLocation`
- [x] `ResolvedLocation` always includes:
 - [x] source (`EngineContentSource` vs `Quake1Source`)
 - [x] layer/provenance (shipped/dev/user)
 - [x] mount name/order
- [x] Deterministic resolver behavior (no CWD dependence).

### Tools
- [x] `tools content mounts`
- [x] `tools content resolve <assetkey>`
- [x] `tools content explain <assetkey>` (trace mounts searched, candidates, winner, reason)

### Notes Future engine-native bundling (VFS mount backend, not required here)
- [x] (Breadcrumb) Ensure the VFS mount system can support backend variants:
 - [x] `DirectoryMount(root_path)`
 - [x] `BundleMount(bundle_path)` *(not implemented in Checklist 3)*
- [x] (Breadcrumb) Design `ResolvedLocation` to represent:
 - [x] filesystem path, OR
 - [x] bundle reference (`bundle_id + entry_id/offset`)
- [x] (Breadcrumb) `tools content mounts` prints backend type.

**DoD**
- [x] `resolve` + `explain` outputs are stable across runs and independent of CWD.
- [x] (Breadcrumb) Adding bundled mounts later will not require breaking the resolver API.

---

## M2 — Async Asset Runtime

### Tasks
- [x] AssetManager API:
 - [x] `request<T>(AssetKey, RequestOpts) -> Handle<T>` (non-blocking)
 - [x] states: `Queued | Loading | Ready | Failed`
 - [x] `await_ready` helper for boot/tools only
 - [x] AssetManager submits IO/CPU stages as jobs
 - [x] main thread pumps completions and advances asset states
- [x] In-flight de-duplication:
 - [x] same-key requests coalesce while loading
- [x] Cancellation hooks:
 - [x] cancel-by-handle or cancel token (minimal behavior v1 acceptable)
- [x] Priority + budget hooks:
 - [x] `RequestOpts { priority: High|Normal|Low, budget_tag: Boot|Streaming|Background }`
 - [x] scheduler honors priority (even if simple)
- [x] Cache + lifetime semantics:
 - [x] cache keyed by `AssetKey`
 - [x] strong `Handle<T>` keeps alive
 - [x] Weak handles are deferred; the handle model must allow adding `WeakHandle<T>` later without breaking `Handle<T>` API.
 - [x] eviction v1: manual purge (`dev_asset_purge`) (LRU deferred)
- [x] Accounting/telemetry:
 - [x] per-asset load time
 - [x] decoded CPU size (where applicable)
- [x] “No sync loads during sim” guardrail (dev mode):
 - [x] warn/panics if sync/await loads occur during sim tick (configurable)

Supported kinds (v1):
- [x] `engine:text` (cfg/txt/lua)
- [x] `engine:blob` (raw bytes)
- [x] `quake1:raw` (raw bytes via registry + pak reader)
- [x] `engine:texture` CPU decode (GPU upload is M3)

Decode budget enforcement
- [x] Add decode budget controls:
 - [x] `asset_decode_budget_ms_per_tick` (or equivalent) configurable via cvar/config
 - [x] worker throttles low-priority/background work first when over budget
 - [x] explicit policy for High priority vs budgets (documented)
- [x] Budget telemetry:
 - [x] time spent per `budget_tag`
 - [x] “throttled due to budget” counters

**DoD**
- [x] Same-key requests coalesce; cache hits are observable.
- [x] Sync loads during sim can be detected in dev builds.
- [x] Lowering decode budget throttles background work without stalling sim.

---

## M3 — GPU Upload Boundary

### Tasks
- [x] Define renderer-owned `UploadQueue` + `UploadJob`:
 - [x] workers enqueue upload jobs only
 - [x] renderer/main thread drains jobs and creates GPU resources
- [x] Enforce rule: no `wgpu` device usage on worker threads.
- [x] Upload accounting:
 - [x] upload bytes queued
 - [x] last drain timing / queue depth

### Upload budget enforcement
- [x] Add upload budget controls:
 - [x] `asset_upload_jobs_per_frame`
 - [x] `asset_upload_bytes_per_frame`
 - [x] renderer drains UploadQueue respecting caps
- [x] Upload prioritization:
 - [x] propagate request priority into upload jobs so High drains first

**DoD**
- [x] Textures requested via AssetManager can reach GPU-ready via UploadJobs.
- [x] The “no wgpu off-thread” rule is mechanically enforced.
- [x] Upload caps prevent burst uploads from stalling rendering.

---

## C. Compat Test Bed

## M4 — Quake1 Source Index

### Tasks
- [x] Add compat taxonomy `QuakeAssetKind` to reduce ad hoc handling:
 - [x] initial variants cover currently-used assets (extend incrementally): `bsp`, `texture`, `sound`, `model`, `wad`, `cfg`, `raw_other`
 - [x] implement `classify(path) -> QuakeAssetKind` (simple suffix/path rules)
 - [x] wire classification into registry entries/diagnostics where useful
- [x] Registry builder:
 - [x] scans quake dir + paks; normalizes paths (lowercase + `/`)
 - [x] stores entries with metadata: size, source, offsets/indices, crc/hash
- [x] Types:
 - [x] `QuakeEntry { path, size, source, offset/index, ... }`
 - [x] `QuakeSource { LooseFile | Pak { pak_id, file_index } }`
- [x] Mount layering + overrides:
 - [x] explicit pak order
 - [x] loose overrides paks
 - [x] resolution chooses highest precedence entry
- [x] Cached index artifact:
 - [x] default output: `content/build/compat/quake1/index.*`
 - [x] runtime loads cached index if present
- [x] Registry versioning + invalidation:
 - [x] version + quake-dir fingerprint (pak names/sizes/mtimes)
 - [x] mismatch triggers deterministic rebuild
- [x] Duplicate path reporting:
 - [x] record duplicates and winner decisions

### Tools
- [x] `tools quake index --quake-dir ... [--out ...]`
- [x] `tools quake which <path>`
- [x] `tools quake dupes [--limit N]`

- [x] classification `QuakeAssetKind`
- [x] derived ID sugar: `quake1:bsp/<mapname>`, `quake1:sound/<logical>`

**DoD**
- [x] Index loads fast; invalidation prevents stale-cache bugs.
- [x] Override semantics match intended behavior (“which” explains winner).

---

## D. Engine-native Content Packages

## M5 – Manifests and Graph

### Tasks
- [x] Define `engine:level/<name>` manifest:
- [x] `content/levels/<name>/level.toml`
- [x] references engine assets by AssetKey
- [x] compat geometry refs by explicit compat ID
- [x] Schema/versioning:
- [x] version field
- [x] unknown fields ignored; missing fields defaulted
- [x] Actionable diagnostics:
- [x] validation errors include file path + field name (line info if feasible)
- [x] Dependency graph:
- [x] compute full reference closure deterministically
- [x] detect cycles

### Tools
- [x] `tools content validate` (CI-friendly, no GPU)
- [x] `tools content graph <engine:level/...>` (sorted closure + cycle report)
- [x] `requires = [...]` for explicit dependencies
- [x] include hashes in graph output (if available)

**DoD**
- [x] Validation errors are actionable without debugging.
- [x] Graph output is deterministic and cycle-safe.

---

## E. Tools and Build Artifacts

## M6 — Content Toolchain

### Tasks
- [x] Implement:
 - [x] `tools content build`
 - [x] `tools content clean`
 - [x] `tools content doctor` (sanity checks + actionable fixes)
- [x] CI-friendly behavior:
 - [x] stable text formatting
 - [x] standard exit codes (non-zero on failure)
 - [x] `--json` output
- [x] Build outputs:
 - [x] `content/build/build_manifest.*` includes:
 - [x] tool version (schema/version for the manifest itself)
 - [x] source fingerprint (mount list + precedence + file sizes/mtimes or hashes where available)
 - [x] per-entry content hash (xxhash64) where feasible; otherwise record a stable source locator
 - [x] build identity: `build_id/profile/platform`, timestamp
 - [x] deterministic inputs list (IDs + resolved locations)
 - [x] outputs produced
 - [x] per-asset hashes for referenced engine assets
 - [x] quake index hash/version

### Build stages
- [x] Build Stage Plugin Interface (baseline):
 - [x] stage registry: name, inputs, outputs under `content/build`, incremental key (hashes)

### Minimal incremental executor v0
- [x] Stage executor v0:
 - [x] computes dirty stages via hashes/incremental keys
 - [x] deterministic stage ordering (stable toposort)
 - [x] records per-stage cache state in build manifest (or sidecar)
- [x] Include one trivial stage now (e.g., `asset_index`) to exercise executor in CI.
- [x] `tools content diff-manifest <a> <b>`

### Packaging proof
- [x] `just package-dev` runs `tools content build`
- [x] packaged run loads:
 - [x] one engine asset by ID
 - [x] one engine level by ID
 - [x] quake raw asset by ID if quake dir/index provided

**DoD**
- [x] Build outputs are deterministic; doctor/validate/build are CI-friendly.
- [x] Packaged run works without repo-relative paths or CWD dependence.
- [x] Second build run does no work when inputs unchanged.

---

## F. Control Plane and Quality Gates

## M7 — Console Surface

### Required console commands (fast, bounded, non-blocking)
- [x] `dev_asset_resolve <assetkey>`
- [x] `dev_asset_explain <assetkey>`
- [x] `dev_asset_stats`
- [x] `dev_asset_status <assetkey>`
- [x] `dev_asset_list [--ns ...] [--kind ...] [--limit N]`
- [x] `quake_which <path>`
- [x] `quake_dupes [--limit N]`

### Dev-only / potentially heavy (must be async or gated)
- [x] `dev_asset_reload <assetkey>` (schedules async reload)
- [x] `dev_asset_purge`
- [x] `dev_content_validate` (schedules validate job)

### Budget visibility hooks
- [x] Expose budgets as cvars (telemetry-first; enforcement may be added later):
 - [x] `asset_decode_budget_ms`
 - [x] `asset_upload_budget_ms`
 - [x] `asset_io_budget_kb`
- [x] `dev_asset_stats` prints budgets + throttling counters (when enabled).

### Rules
- [x] Console commands never perform blocking disk I/O on sim tick.
- [x] Heavy actions schedule jobs and return immediately.

**DoD**
- [x] You can diagnose missing/overridden assets from inside the running game.
- [x] None of these commands can stall the sim tick.

---

## M8 — Tests and Fixtures

### Tasks
- [x] Golden fixture content pack:
 - [x] one texture, one script, one level manifest referencing them
- [x] Integration tests (no GPU):
 - [x] lint/validate/build/graph determinism
 - [x] async request coalescing/backpressure behavior
 - [x] registry override winner behavior
- [x] Micro-bench/timing harness:
 - [x] ID parse/canonicalize, resolve, cache-hit path, registry lookup

**DoD**
- [x] Core behaviors are covered by tests and stable across runs/OS.

---

## M9 — Runner GUI Wiring

### Tasks
- [x] Buttons: lint/validate/build/clean/doctor/index
- [x] Display: last build_manifest + mount table summary
- [x] dev_asset_stats summary panel

**DoD**
- [x] Runner can drive the toolchain and surface key diagnostics without duplicating logic.

---

## M10 — Migration and Convergence
**Move ad hoc implementations onto the asset platform**

### Goal
Eliminate ad hoc asset/config loading paths so future work automatically benefits from the asset platform (IDs, mounts, async loading, cache, tooling, and console diagnostics).

### Tasks

### Inventory and policy
- [x] Inventory current ad hoc loaders/usages (capture file/module locations):
 - [x] direct `std::fs` reads in runtime code paths (pallet settings bootstrap, png output, legacy script_lua load_file, control_plane default exec source)
 - [x] custom per-subsystem path resolution (pallet config asset helpers, path_policy mount manifest bootstrap)
 - [x] bespoke caching or "load-on-use" implementations (none; AssetManager handles cache/coalescing)
 - [x] Quake reads that bypass registry resolution (pallet bsp/lmp/wav/music now via `quake1:raw`)
- [x] Define and document a policy: "runtime asset reads must go through AssetManager unless explicitly exempt"
- [x] Define allowed exemptions (v1):
 - [x] OS/user config dir reads for bootstrap only
 - [x] log/profiling output writes
 - [x] temporary dev experiments gated by `DevOnly` and clearly labeled

### Migrate high-leverage offenders first
- [x] Movie playlist + startup playlist config -> `engine:config/...` via AssetManager
- [x] Startup configs / exec files -> `engine:config/...` via AssetManager
- [x] Lua scripts used by runtime -> `engine:script/...` via AssetManager
- [x] UI textures/logos/splash assets -> `engine:texture/...` via AssetManager
- [x] Any Quake file reads not already routed through compat -> `quake1:raw/...` resolved by Quake registry

### Remove bypasses and enforce the pathway
- [x] Replace direct file reads inside sim/render loops with AssetManager requests
- [x] Ensure async pathways are used (no hidden sync reads)
- [x] Use "no sync loads during sim" guardrail to catch missed migrations
- [x] Delete or quarantine old code paths:
 - [x] legacy note added for script_lua load_file (unused)
 - [x] remove duplicate caching layers that are now superseded by AssetManager

### Wire to toolchain + control plane
- [x] Ensure migrated assets are diagnosable at runtime:
 - [x] `dev_asset_resolve`, `dev_asset_explain`, `dev_asset_status`, `dev_asset_stats`
- [x] Ensure migrated engine assets participate in tooling:
 - [x] `tools content validate` catches missing/invalid references
 - [x] `tools content build` includes them in build manifest inputs
- [x] Update golden fixture (if applicable) to include at least one migrated "real" path (e.g., a startup exec or playlist)

### Policy doc for agents - [x] Add `docs/asset_platform_rules.md` (or equivalent) summarizing:
 - [x] the "no ad hoc loads" policy
 - [x] exemptions list
 - [x] how to add a new asset kind/namespace
 - [x] how to diagnose with tools + console

### DoD
- [x] No new runtime asset loads are performed via ad hoc file reads (policy followed).
- [x] The main offenders (playlist, exec, scripts, UI textures, quake raw reads) are served through the asset platform.
- [x] Any remaining ad hoc path is either explicitly exempted or isolated in `legacy_*` with a planned removal note.
---

---

## Hot Reload v1 (manual, asset-keyed)

### Scope
- Manual hot reload via dev-only commands.
- Reload is keyed by `AssetKey` and runs asynchronously (never blocks sim/frame).

### Handle + publish semantics
- `Handle<T>` remains stable across reloads (hot-swap in place).
- Reload pipeline: resolve -> read -> decode -> publish.
- Publish is atomic: consumers never observe partial state.

### Failure policy
- On reload failure, keep the last-known-good asset active.
- Record and surface the error (console + last-error buffer).

### Dependency bookkeeping (for future hot reload)
- The asset system must have a place to record `AssetKey` dependencies per loaded asset (even if invalidation is not implemented yet).

### Tasks
- [x] Implement `dev_asset_reload <assetkey>`:
  - [x] schedules an async reload job
  - [x] increments per-asset version on successful publish
  - [x] logs resolved source + new version/hash
- [x] Implement `dev_asset_purge <assetkey>`:
  - [x] removes cached entry (or marks stale) and forces next request to reload (exact key)
- [x] Ensure reload never blocks sim/frame; GPU upload remains on the main thread boundary where applicable.
- [x] Ensure failed reload does not poison the cache slot.

**DoD**
- [x] Modify a source asset on disk, run `dev_asset_reload <key>`, and observe the updated result without restart.
- [x] Introduce an invalid asset; reload fails gracefully and prior version remains active.

## Completion criteria
Checklist 3 is complete when:
- IDs are canonical, typed, linted, deterministic.
- Async AssetManager exists with coalescing/backpressure/cancellation and a renderer-owned upload boundary.
- Quake registry exists with layering, invalidation, dupes/which diagnostics.
- Level manifests validate and produce deterministic dependency graphs with actionable errors.
- Tools build/clean/doctor exist with deterministic build manifests/hashes and CI-friendly behavior.
- Console provides runtime introspection for resolution/cache/registry without stalling sim.
- Packaged folder run works without repo-relative paths or CWD dependence.
- Ad hoc asset implementations have been converged onto the asset platform (or explicitly exempted).
