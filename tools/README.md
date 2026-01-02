# tools — CLI Contract (Specification)

This document specifies the interface for a future `tools` binary (Clap-based).
It is a **contract** for an automated agent (human or AI) to verify functionality.

## Subcommands

### `tools smoke`
Two modes:

#### 1) No-assets mode (CI-friendly)
Does not require external game data. Generates or uses non-copyright test data.

Validates:
- ECS schedule runs N ticks
- net loopback client/server connect and exchange snapshots
- renderer can initialize (headless/no-surface acceptable on CI)
- audio can initialize OR can be stubbed on CI

Command:
- `tools smoke --mode no-assets [--ticks N]`

Exit codes:
- 0 success
- 2 usage/config error
- 3 runtime init failure
- 4 invariant failure (e.g., desync, unexpected snapshot)

#### 2) Quake harness mode (manual/local)
Requires `--quake-dir` pointing to a legally acquired Quake install.

Validates:
- mounts PAKs
- loads a BSP map (static render)
- optional: starts streaming OGG music if present

Command:
- `tools smoke --mode quake --quake-dir <PATH> --map <MAPNAME> [--headless]`

Exit codes:
- 0 success
- 10 missing/invalid quake dir
- 11 pak mount failure
- 12 bsp load failure
- 13 render failure
- 14 audio failure
- 15 net failure

### `tools pak`
- `tools pak list --quake-dir <PATH>`
- `tools pak extract --quake-dir <PATH> --out <DIR>`  
  Extraction must write only to the user-provided output directory.

## Future expansions (non-breaking)
- `tools map inspect --quake-dir <PATH> --map <MAPNAME>`
- `tools assets validate --quake-dir <PATH>`
- `tools net replay --input <FILE>`
