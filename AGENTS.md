# AGENTS.md — Pallet (Full-Access Agent Operating Rules)

This repository is a clean-room, Rust-based engine/client/server effort using Quake as a validation harness. Agents operating with full repository and command execution access must follow the rules below to ensure correctness, maintainability, and licensing hygiene.

## 1) Operating principles (non-negotiable)

1. **Clean-room discipline**
   - Do not copy/paste code from Quake/id Software/third-party engines or proprietary SDK samples.
   - Implement from specifications, public documentation, or original work.
   - If you reference an external source, summarize the approach and cite the source in comments/notes (no large verbatim blocks).

2. **Layering and boundaries**
   - Keep subsystems modular. Do not introduce circular dependencies.
   - Avoid leaking implementation details (e.g., transport backend, renderer backend) into higher layers.
   - Prefer stable interfaces and adapters.

3. **Determinism and testability**
   - Add tests/harnesses where feasible.
   - Avoid “it seems to work” changes without at least a smoke path or invariant checks.

4. **Licensing**
   - Code dependencies must remain permissive (MIT/Apache/BSD/ISC/Zlib) unless explicitly approved.
   - Assets must follow the repo’s asset policy (fonts may be OFL if license text is included; other third-party assets must be explicitly allowed and tracked).
   - Any new third-party dependency requires updating the repo’s third-party notices or policy files as applicable.

## 2) Agent workflow expectations

### Before changing anything
- Read the relevant checklist doc(s) in `docs/` and follow the current milestone scope.
- Search the repo for existing patterns and conventions; do not introduce alternate frameworks when an existing solution is in place unless explicitly requested.

### When making changes
- Keep changes minimal and scoped to the requested milestone.
- Prefer small, reviewable commits (even if you do not actually commit, structure your work as if you would).
- If a change affects public APIs, update any affected documentation and examples.

### After changes
- Run formatting and checks:
  - `cargo fmt`
  - `cargo clippy -D warnings` (or the repo’s equivalent)
  - `cargo test`
- If changes touch runtime behavior (rendering/audio/net), run the repo’s smoke targets (if available) and document how to reproduce.

### Reporting back
When you report results:
- Summarize what changed and why.
- Call out any behavior changes, new flags/settings, and any known limitations.
- Include command lines used to validate.

## 3) Safety rails for full-access agents

1. **No destructive operations without explicit request**
   - Do not delete files, rewrite large directories, or reformat the entire repo unless asked.
   - Do not overwrite large assets or regenerate lockfiles unless required.

2. **No “drive-by” dependency additions**
   - Adding a crate is a design decision. Propose it first unless the checklist explicitly calls for it.
   - If you must add a dependency to complete the task, justify it in the PR notes and confirm its license.

3. **Avoid hidden behavior**
   - Do not add background threads, telemetry, network calls, or auto-update behavior.
   - If a background task is needed (e.g., decode thread), it must be documented and controlled via explicit configuration.

4. **Reproducibility**
   - Any generated outputs (captures, regression packs, etc.) should go under an ignored output directory and must not be committed unless requested.

## 4) Repository conventions (general)

- **Rust style**: prefer explicit types at module boundaries; keep `unsafe` minimal and documented.
- **Error handling**: use `thiserror`/`anyhow` patterns only if already present; otherwise follow existing conventions.
- **Logging**: route through the repo’s logging facade; do not print directly unless in a tool/test.
- **Config/Settings**: follow the typed settings + persistence approach; do not add ad-hoc config files.

## 5) Checklist-driven development

Work should track the repository checklists:
- Checklist 1: foundational runtime + client/server + asset harness (Quake).
- Checklist 2: UI + text (egui + glyphon) and resolution/DPI verification harness.
- Future checklists should preserve extensibility (especially debug control plane).

Agents must:
- Implement only what the active checklist requires.
- Avoid pulling future checklist scope into current work unless explicitly requested.

## 6) Third-party content policy (summary)

- Do not commit third-party character models/animations unless their license explicitly permits redistribution and the license text/attribution is included.
- If using external assets for local testing, keep them out of the repo and document how to obtain them.

## 7) Quick validation commands (adjust to repo)

Typical commands to run after changes:
- `cargo fmt`
- `cargo clippy --workspace --all-targets -D warnings`
- `cargo test --workspace`
- If a UI regression harness exists: `cargo run -p <tool_or_app> -- <ui_regression_args>`

## 8) If you are unsure

If any of the following are unclear:
- a license condition,
- an architectural boundary,
- a checklist scope boundary,
- or a change that may have broad impact,

then stop and propose options with tradeoffs rather than proceeding silently.

---
**Goal:** Deliver correct, reviewable, checklist-aligned changes with predictable licensing and reproducible verification.
