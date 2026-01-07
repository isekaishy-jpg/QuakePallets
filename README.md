# Pallet — Rust Engine/Client/Server (Quake-as-Testbed)

This folder is a **starter documentation bundle** (not a full repository yet). It provides:
- A milestone checklist (clean-room, ECS-first, modern networking, Quake compatibility harness).
- CI workflows (fmt/clippy/test/cargo-deny) and fuzz workflow (nightly + cargo-fuzz).
- A `justfile` with standardized operational commands.
- Policies: clean-room protocol and licensing policy.
- A tools contract (CLI interface specification) that future code should implement.

## Intended next step
Create a Cargo workspace matching the crate layout in `CHECKLIST.md`, then implement the `tools` binary contract described in `tools/README.md`.

## Notes
- No copyrighted assets are included or expected. The Quake harness loads from a user-supplied, legally acquired installation directory.
- The docs deliberately avoid “Quake-accurate” protocol behavior. The networking design is modern, authoritative client/server with snapshots.

## UI regression pack
Generate a resolution/DPI/UI scale capture pack via the tools CLI:
- `cargo run -p tools -- ui-regression`

Outputs are written to `ui_regression/<timestamp>/` with `manifest.json` plus PNGs.

## Added design docs
- `docs/ARCHITECTURE.md` — ECS scheduling and modern netcode reference architecture.
