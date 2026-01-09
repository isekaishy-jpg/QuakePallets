# Runner GUI Inventory (Current)

This file enumerates all runner-GUI-relevant commands and knobs currently available.

## Environment variables

- `CRUSTQUAKE_VIDEO_DEBUG=1`
  Enables periodic video/audio debug stats in the main pallet app. (pallet/src/main.rs)

## Pallet CLI flags (main app)
- `--quake-dir <path>`
- `--show-image <asset>`
- `--map <name>`
- `--play-movie <file>`
- `--playlist <file>`
- `--script <path>`
- `--input-script` (scripted input sequence)
- `--debug-resolution` (prints per-frame resolution diagnostics)
- `--ui-regression-shot <path>`
- `--ui-regression-res <WxH>`
- `--ui-regression-dpi <scale>`
- `--ui-regression-ui-scale <scale>`
- `--ui-regression-screen <main|options>`

## Tools subcommands

- `tools smoke --mode <no-assets|quake> [--ticks <n>] [--quake-dir <path>] [--map <name>] [--headless]`
- `tools pak list --quake-dir <path>`
- `tools pak extract --quake-dir <path> --out <dir>`
- `tools ui-regression [--out-dir <dir>]`

## Other binaries with CLI args

### net/server dedicated server
Run:
- `cargo run -p server --bin dedicated -- [flags]`

Flags:
- `--bind <ip:port>`
- `--tick-ms <ms>`
- `--snapshot-stride <n>`
- `--max-ticks <n>`

### net/client headless client
Run:
- `cargo run -p client --bin headless -- [flags]`

Flags:
- `--bind <ip:port>`
- `--server <ip:port>`
- `--tick-ms <ms>`
- `--ticks <n>`
- `--client-id <n>`
- `--move-x <float>`
- `--move-y <float>`
- `--yaw-step <float>`

## Console commands (in-game)

- `logfill [count]`
  (1-20000) to stress log rendering.
- `perf`
  Prints perf timings (egui + glyphon) in console.
- `perf_hud [on|off]`
  Toggles the perf HUD overlay.
- `stress_text` / `perf_stress`
  Starts/stops the 5s stress run (5k log lines + ~50k glyphs + typing sim).

## Runtime hotkeys

- `Alt+Enter`: toggle windowed <-> last fullscreen mode.
- `Backquote (~)`: toggle console.
- `Esc`: open/close menu or pause.
