# Runner GUI Checklist (egui) — Pallet Dev Runner v1

## Objective
Build a small egui desktop app that can:
1) Launch the main `pallet` app with common flags and presets,
2) Run tool subcommands (`tools smoke`, `tools pak …`),
3) Launch net dedicated/headless binaries,
4) Provide a debug preset dropdown (including env-var toggles),
5) Provide one-click `fmt/clippy/test` (repo-root aware).

## Non-goals
- Full editor
- Cross-platform polish beyond basic operation
- Deep command scripting / runtime console injection (v1)

---

## D0 — Project structure and repo-root awareness
  - [x] Create binary crate `pallet_runner_gui` (workspace member or standalone).
  - [x] UI field: **Repo Root** (directory)
  - Text box + “Browse…” + “Use current dir”
  - [x] Validation:
  - `Cargo.toml` exists at repo root
  - Optional: `cargo metadata` succeeds
  - [x] All subprocesses launched with `current_dir = repo_root`.

**DoD**
  - [x] Runner can be launched from anywhere and still correctly run cargo commands against the chosen repo.

---

## D1 — Config persistence
Persist config to `%APPDATA%/Pallet/runner_gui.json` (or platform equivalent later). Store:
- repo root
- quake dir
- last map
- last playlist path + enabled state
- selected debug preset
- last show-image / play-movie / script / input-script settings
- last smoke mode/ticks/headless
- last pak out dir
- last net bind/server/ticks settings

**DoD**
- [x] Relaunch restores previous selections.

---

## D2 — Main Pallet “Run” tab
### Inputs
- [x] Quake dir (default: `C:\Program Files (x86)\Steam\steamapps\common\Quake\rerelease`)
- [x] Map (default `e1m1`)
- [x] Playlist checkbox + file path (`--playlist <file>`)
- [x] Collapsible “Advanced”:
  - `--show-image <asset>`
  - `--play-movie <file>`
  - `--script <path>`
  - `--input-script` toggle (enables scripted input sequence)

### Debug controls
- [x] Checkbox: “Video debug stats”
  - Implements `CRUSTQUAKE_VIDEO_DEBUG=1` env var for launched process
- [x] Debug preset dropdown (D5)

### Actions
- [x] **Run Pallet**
  - `cargo run -p pallet -- <args>`
- [x] **Copy command** (copies equivalent CLI)
- [x] **Stop** (terminates child process)
- [x] Log panel streaming stdout/stderr + exit code

**DoD**
- [x] One click reproduces your PowerShell workflow (quake-dir + map + optional playlist) and supports video debug env-var.

---

## D3 — Tools tab
### 3.1 Smoke
UI:
  - [x] Mode dropdown: `no-assets` | `quake`
  - [x] `--ticks <n>` (optional)
  - [x] `--quake-dir <path>` (defaults from main tab)
  - [x] `--map <name>` (defaults from main tab)
  - [x] `--headless` checkbox
Action:
  - [x] `cargo run -p tools -- smoke --mode <...> [--ticks N] [--quake-dir ...] [--map ...] [--headless]`

**DoD**
  - [x] Smoke runs without manual terminal pasting; output visible in tool logs.

### 3.2 Pak
UI:
  - [x] `pak list` button (requires quake-dir)
  - [x] `pak extract` button + out dir picker
Actions:
  - [x] `cargo run -p tools -- pak list --quake-dir <path>`
  - [x] `cargo run -p tools -- pak extract --quake-dir <path> --out <dir>`

**DoD**
  - [x] Pak list/extract work reliably with spaces in paths.

---

## D4 — Net tab
### 4.1 Dedicated server
Fields:
  - [x] `--bind <ip:port>`
  - [x] `--tick-ms <ms>`
  - [x] `--snapshot-stride <n>`
  - [x] `--max-ticks <n>` (optional)
Action:
  - [x] `cargo run -p server --bin dedicated -- --bind ... --tick-ms ... --snapshot-stride ... [--max-ticks ...]`

### 4.2 Headless client
Fields:
  - [x] `--bind <ip:port>`
  - [x] `--server <ip:port>`
  - [x] `--tick-ms <ms>`
  - [x] `--ticks <n>`
  - [x] `--client-id <n>`
  - [x] Optional movement:
  - `--move-x <float>`
  - `--move-y <float>`
  - `--yaw-step <float>`
Action:
- [x] `cargo run -p client --bin headless -- --bind ... --server ... --tick-ms ... --ticks ... --client-id ... [--move-x ...] ...`

**DoD**
- [x] Dedicated and headless client can be launched, stopped, and their logs viewed.

---

## D5 – Debug preset dropdown (data-driven)
### Preset model
- [x] Persist `DebugPreset` in config:
  - `name`
  - `description` (short)
  - `extra_args: Vec<String>` (appended after `--`)
  - `env: HashMap<String,String>` (e.g., `CRUSTQUAKE_VIDEO_DEBUG=1`)

### Initial presets (based on current inventory)
- [x] `Default` (no extras)
- [x] `Video Debug` (env `CRUSTQUAKE_VIDEO_DEBUG=1`)
- [x] `Intro Playlist + E1M1` (adds `--playlist movies_playlist.txt --map e1m1`)

**DoD**
- [x] Selecting a preset deterministically changes the launch args/env; “Copy command” reflects it.

---

## D6 – fmt / clippy / test buttons (repo-root aware)
Buttons run from `repo_root` and stream output:
- [x] `fmt` (prefer `just fmt`; fallback `cargo fmt`)
- [x] `clippy` (prefer `just clippy`; fallback `cargo clippy --workspace --all-targets -D warnings`)
- [x] `test` (prefer `just test`; fallback `cargo test --workspace`)
- [x] Enforce one “build command” at a time (disable others while running)

**DoD**
- [x] One-click fmt/clippy/test works regardless of where the GUI is launched from.

---

## D7 – Process management
- [x] Choose process concurrency model:
  - Either one global active process at a time, **or**
  - One active process per lane (Pallet / Tools / Server / Client)
- [x] Stop button terminates the correct lane process
- [x] On app exit: terminate any children (or prompt)

**DoD**
- [x] No orphan processes remain after exit.

---

## D8 – Logging and UX
- [x] Central log panel with tabs (Pallet/Tools/Server/Client) or per-tab logs
- [x] Log ring buffer (bounded)
- [x] “Clear log” button
- [x] Display last exit code + duration
- [x] Non-blocking validation warnings:
  - quake dir missing
  - playlist missing when enabled
  - repo root invalid

**DoD**
- [x] Output is readable and tool remains responsive during subprocess execution.

---

## D9 – Console command notes (v1)
- [x] Add a small “In-game console notes” panel with copyable snippets:
  - `logfill [count]` (1–20000) to stress log rendering.

**DoD**
- [x] Runner remains useful without runtime console injection.

---

## Acceptance criteria
- [x] Main tab: one-click run of `pallet` with `--quake-dir`, `--map`, optional `--playlist`, optional `CRUSTQUAKE_VIDEO_DEBUG=1`.
- [x] Tools tab: one-click `smoke` and `pak list/extract`.
- [x] Net tab: one-click dedicated server + headless client.
- [x] Debug preset dropdown modifies args/env deterministically.
- [x] fmt/clippy/test buttons work from repo root and show output.
