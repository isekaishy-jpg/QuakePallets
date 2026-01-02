# Clean-Room Protocol (Operational)

## Objective
Ensure the project is developed without referencing Quake source code or decompiling Quake binaries, while still supporting a Quake-asset compatibility harness for testing.

## Hard prohibitions
- Do not consult Quake source code (GPL releases, forks, ports, reimplementations) for implementation details.
- Do not decompile Quake binaries to derive algorithms or data structures.
- Do not copy/paste from any Quake-derived code.
- Do not ship copyrighted Quake assets in this repository.

## Allowed inputs
- Public, structural documentation of file formats (PAK/BSP/WAD/PCX/WAV/OGG container structures).
- Black-box testing with legally obtained files (e.g., “loader accepts this file and produces these counts/hashes”).
- Your own independently developed algorithms and data structures.

## Roles (recommended)
- Spec Gatherer: collects public format notes and writes repo-local “format notes”.
- Implementer: writes code based only on repo-local notes and independent testing.
- Verifier: runs tests and validates outputs; does not provide code.

For a small team, one person can play multiple roles, but the prohibitions still apply.

## Provenance artifacts (repo-local)
Create:
- `docs/provenance/inputs_allowed.md`
- `docs/provenance/inputs_forbidden.md`
- `docs/provenance/format_notes/` (structure-level notes only)
- `docs/provenance/test_vectors/` (hashes/counts/metadata only, never assets)

## Parser safety policy
All parsers for untrusted inputs must:
- Use bounded reads
- Validate offsets/lengths
- Prevent integer overflow (checked arithmetic)
- Return `Result` errors (no panics)

## AI agent rules
If an AI is used to write code:
- It must be instructed not to reproduce Quake code or consult Quake sources.
- It must run the verification gates (fmt/clippy/test/deny + smoke tests) before claiming completion.
