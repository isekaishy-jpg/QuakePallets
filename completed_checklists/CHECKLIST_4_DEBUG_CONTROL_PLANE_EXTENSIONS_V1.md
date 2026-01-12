# CHECKLIST 4 — Debug Control Plane Extensions v1 (Console UX + Overlays + Capture + Smoke + Settings Bridge)

## Objective
Extend the existing registry-backed control plane with developer-grade console UX, cvar-driven overlays, capture outputs, smoke automation, and a Settings/user-config bridge.

## Non-goals
- No new rendering features beyond debug visualization/capture surfaces.
- No new asset identity or loader architecture.
- No redefinition of where configs live, mount precedence, or jobs/workers behavior.
- No gameplay/Quake feature work beyond what’s required to validate control-plane flows.

---

## M1 — Console UX polish (developer-grade)

### Tasks
- [x] Command history:
  - [x] input history navigation (Up/Down cycles previously entered commands in the input field; in-memory for this session)
- [x] Autocomplete:
  - [x] tab completes command and cvar names
  - [x] show multiple matches in a readable list
- [x] Error/reporting ergonomics:
  - [x] consistent `error:` formatting for parse/unknown-command failures
  - [x] “last error” buffer suitable for overlays/diagnostics (reuse sticky error)

**DoD**
- [x] A developer can discover a command/cvar via autocomplete and `help` without source-diving.

---

## M2 — Debug overlays (cvar-controlled) + log routing

### Tasks
- [x] Overlay toggles:
  - [x] `dbg_overlay` master toggle
  - [x] `dbg_overlay` gates all overlay rendering (including `dbg_perf_hud`)
  - [x] sub-toggles (examples): `dbg_fps`, `dbg_frame_time`, `dbg_net`, `dbg_jobs`, `dbg_assets`, `dbg_mounts`
- [x] Implement overlay panels (minimal content is fine; must be stable and readable):
  - [x] FPS + frame time summary
  - [x] “last error” line
  - [x] net stats placeholder (even if minimal)
- [x] Log routing:
  - [x] route engine logs into the console buffer (with severity)
  - [x] filtering cvars: `log_level`, `log_filter`
- [x] Deterministic formatting:
  - [x] stable column alignment / key ordering where applicable
  - [x] stable output in deterministic runs (avoid nondeterministic timestamps/ids in log-to-console output)

**DoD**
- [x] Overlays can be toggled entirely from cvars/commands.
- [x] Logs show up in the console and are filterable.

---

## M3 — Capture hooks (scriptable) + regression-friendly outputs

### Tasks
- [x] Provide capture commands:
  - [x] `capture_screenshot [path]`
  - [x] `capture_frame [path]` (if distinct from screenshot)
- [x] Output policy:
  - [x] consistent default directory
  - [x] stable naming scheme
  - [x] capture includes enough context in filename or sidecar (resolution, mode, map name if available)
  - [x] align capture naming/location with UI regression outputs where possible
- [x] Integrate capture with console scripts:
  - [x] allow `exec` scripts to trigger capture
- [x] Ensure capture works while overlays are enabled.
- [x] Add cvar `capture_include_overlays` (default 1) to include/exclude overlays in capture output.

**DoD**
- [x] A single `dev_exec` script can produce at least one capture artifact with stable naming.

---

## M4 — Smoke automation (CLI-driven) using console scripts

### Goal
Enable non-interactive “run a scenario then exit” flows suitable for local automation and CI usage.

### Tasks
- [x] Add a CLI mode that runs a provided script and exits:
  - [x] `--smoke <script>`
  - [x] `--smoke` is distinct from `--input-script` (no overlapping semantics)
- [x] Script completion semantics:
  - [x] Define success vs failure (exit codes)
  - [x] Define timeouts:
    - [x] `--gtimeout-ms <ms>` global timeout for the whole smoke run
    - [x] `ttimeout_ms <ms>` per-step timeout control inside scripts
- [x] Add minimal script primitives:
  - [x] `sleep_ms <ms>` (sleeps for wall-clock time)
  - [x] `ttimeout_ms <ms>` (sets/overrides the per-script step timeout used by subsequent waits/captures)
- [x] Failure surfaces:
  - [x] on failure, print a clear reason (command + line number + error)
  - [x] write a summary report file

**DoD**
- [x] A smoke run can execute a script, produce a capture, and exit with:
  - [x] code 0 on success
  - [x] nonzero on script/command failure or timeout

---

## M5 — Settings + User Config bridge (console-driven)

### Model
- `settings.cfg` is the authoritative store for settings-backed fields (window mode, resolution, vsync, volumes, etc.).
- `config.cfg` is a user-facing config that includes settings-backed fields plus user cvars.
- Settings save merges settings-backed fields into the active user config (default: `config.cfg`).
- The Settings UI must allow selecting an active user config profile via dropdown; `settings.cfg` references the selected profile.

### Tasks
- [x] Expose settings fields via commands/cvars by routing through the existing Settings system:
  - [x] `settings_get <field>`
  - [x] `settings_set <field> <value>` (applies live where supported)
  - [x] `settings_list`
  - [x] `settings_reset`
- [x] Bind settings-backed cvars/commands to Settings (single source of truth):
  - [x] setting changes made through console update Settings immediately
  - [x] Settings persistence remains in `settings.cfg`
- [x] Implement user-facing config profiles (non-settings cvars):
  - [x] `cfg_list` (available profiles)
  - [x] `cfg_load <name>` (executes the cfg file into cvars/commands)
  - [x] `cfg_save <name>` (writes current non-settings cvars to cfg file)
  - [x] `cfg_select <name>` (sets active profile; updates Settings so it persists via `settings.cfg`)
  - [x] `cfg_load` restores settings-backed fields from the config (settings travel with the profile)
- [x] Saving rules:
  - [x] settings save continues to write `settings.cfg`
  - [x] settings save merges settings-backed fields into the active user config
  - [x] `cfg_save` writes settings-backed fields plus non-settings cvars to the target profile
- [x] Loading rules:
  - [x] `cfg_load` applies settings-backed fields via Settings (and persists them)
  - [x] `cfg_load` prints clear diffs for changed cvars/settings (name + old + new)
- [x] Hot-add behavior:
  - [x] newly saved config files are immediately loadable without restart (no stale failed cache)

**DoD**
- [x] A setting can be changed via console, applies live when supported, and persists via `settings.cfg`.
- [x] A non-settings cvar can be changed, saved to a chosen cfg profile, and reloaded on demand.
- [x] Selecting a cfg profile via console persists and is reflected in the Settings UI dropdown.

---

## Completion criteria
- [x] Overlays and capture are driven via the control plane (no hidden toggles).
- [x] A smoke run can execute a script, capture output, and exit deterministically.
