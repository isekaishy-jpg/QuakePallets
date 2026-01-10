# CHECKLIST 2.9 — Control Plane Core v1 (Cvars + Commands + Exec)

## Objective
Establish a single registry-backed control plane foundation for the engine runtime: typed cvars, command registry, and script execution. This checklist intentionally excludes overlays/capture/smoke and focuses on the core API and dispatch.

## Non-goals
- No debug overlay panels, capture surfaces, or smoke automation.
- No Settings/user-config bridging.
- No console UX features beyond what is required to invoke commands reliably.

---

## M0 — Registry-backed control plane (cvars + commands)

### Goals
- A single registry-backed API is the source of truth for:
  - cvars: typed values with metadata
  - commands: callable actions with help text and structured args
- Existing ad hoc console commands are migrated incrementally (parity first).

### Tasks
- [x] Define `CvarId` and canonical naming convention:
  - [x] snake_case only; lowercase only; no `.` in names
  - [x] cvars are domain-first (e.g., `dbg_overlay`, `render_wireframe`, `net_sim_latency_ms`)
  - [x] reserve `dev_` prefix for dev-only behavior (cvars and commands)
- [x] Write `docs/cvar_naming.md`: naming rules (domains, verb-first commands, prefixes), type/unit conventions, and examples.
- [x] Implement typed cvars:
  - [x] Types: `bool`, `i32`, `f32`, `String`
  - [x] Metadata: description/help, default, min/max (if relevant), flags (at least: `Cheat`, `ReadOnly`, `NoPersist`, `DevOnly`)
  - [x] String ↔ typed parsing with clear error messages
- [x] Implement `CvarRegistry`:
  - [x] register / lookup / enumerate
  - [x] change notification (callback or “dirty list”)
  - [x] deterministic iteration order (stable output)
- [x] Implement command registry:
  - [x] command names are verb-first snake_case (e.g., `cvar_set`, `cfg_load`, `capture_screenshot`)
  - [x] `dev_` prefix reserved for dev-only commands (e.g., `dev_exec`)
  - [x] `CommandId`/name, help/usage, handler function
  - [x] structured argument parsing (minimal v1: positional + `--flag` booleans)
  - [x] deterministic listing order
- [x] Implement baseline built-ins:
  - [x] `help` (commands + cvars)
  - [x] `cvar_list [prefix]`
  - [x] `cvar_get <name>`
  - [x] `cvar_set <name> <value>`
  - [x] `cmd_list [prefix]`
- [x] Implement `exec` and `dev_exec`:
  - [x] `exec <file>`: executes a command script line-by-line and stops on first error (prints `error: line N: ...`).
  - [x] `dev_exec <file>`: executes line-by-line and continues on error (prints `error: line N: ...`), then prints a summary of error count.
  - [x] Comment support: lines starting with `#` are comments (after leading whitespace).
  - [x] Blank line handling.
- [x] Incremental migration:
  - [x] Wrap existing console commands into the command registry (parity first)
  - [x] Remove old ad hoc dispatch paths once parity is reached
- [x] Standardize toggle inputs:
  - [x] Bool cvars accept only `0`/`1` values.
  - [x] Toggle commands accept `0`/`1` (and reserve `2+` for modes).
- [x] Console color codes:
  - [x] Add `^0`-`^8` color codes for console input and log output (white default).
  - [x] Color codes hide in rendered text but reappear if the trailing text is deleted.
- [x] Console welcome message:
  - [x] Load welcome text from `content/config/console/console_welcome.txt`.
  - [x] Support a `console` config kind for resolution and overrides.

### Tools
- [x] `tools console dump_cvars`
- [x] `tools console dump_cmds`

**DoD**
- [x] A new cvar can be registered, listed, read, and set.
- [x] A new command can be registered and invoked with arguments.
- [x] `exec` runs a script with at least 10 lines and stops on first error.
- [x] `dev_exec` runs the same script and continues on error, reporting the final error count.

---

## Completion criteria
Checklist 2.9 is complete when:
- [x] A new cvar can be registered, listed, read, and set.
- [x] A new command can be registered and invoked with arguments.
- [x] `exec` runs a script with at least 10 lines and stops on first error.
- [x] `dev_exec` runs the same script and continues on error, reporting the final error count.
- [x] Console dispatch uses the registries as the authoritative execution path (no parallel ad hoc dispatch).
