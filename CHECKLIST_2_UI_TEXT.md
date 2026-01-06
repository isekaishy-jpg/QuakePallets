# CHECKLIST 2 — UI + High-Quality Text + Resolution/DPI Verification (egui + glyphon)

## Objective
Deliver a stable UI and text stack that enables:
- Main menu + Options menu (player-facing usability)
- Developer console overlay (Quake-style) rendered with **glyphon**
- HUD overlays (FPS, ticks, net stats placeholder) rendered with **glyphon**
- Robust input focus routing (menu/console/game) with correct mouse capture behavior
- Repeatable validation across a resolution/DPI/UI-scale matrix, including screenshot capture

This checklist is intentionally a single vertical slice to avoid coupling with lighting/Theora/compat work.

---

## Licensing constraints (hard requirements)
- All code dependencies used for UI/text must be permissive (MIT/Apache-2.0/BSD/Zlib/ISC).
- Any bundled font must be redistributable and shipped with its license text.

**Bundled font for this checklist**
- **Fira Sans** (recommended set: Regular + Italic + Bold).
- Package the font files and the **SIL Open Font License 1.1** text in-repo under `third_party/fonts/FiraSans/`.

> Policy note: This project treats OFL-1.1 fonts as acceptable third-party assets when shipped with the license text. Code dependencies remain restricted to permissive code licenses.

---

## Scope boundaries (explicitly out of scope)
- Theora/video
- Lightmaps, advanced lighting, texture/material overhaul
- Gamepad navigation
- Full keybind remapping UI (only minimal proof-of-flow)

---

# Deliverables

## D0 — UI/Text architecture interfaces (no UI yet)
- [x] Define a stable `UiFacade` API (lives in `pallet` or a `ui` crate):
  - `begin_frame(input: UiFrameInput) -> UiFrameContext`
  - `build_ui(ctx: &mut UiFrameContext, state: &mut UiState, settings: &mut Settings)`
  - `end_frame(ctx) -> UiDrawData`
- [x] Define `TextOverlay` API (lives in `render_wgpu` or `text_glyphon` module):
  - `queue(layer, style, position, bounds, text)`
  - `flush(render_pass, viewport)`
- [x] Define `ResolutionModel` (single source of truth):
  - `physical_px` (swapchain size)
  - `dpi_scale` (from windowing)
  - `ui_scale` (user setting)
  - `logical_px = physical_px / dpi_scale`
  - `ui_points = logical_px * ui_scale`

**DoD**
- [x] One module owns these transforms; no ad-hoc scaling elsewhere.
- [x] A debug print shows all four values each frame (for early diagnosis).

---

## D1 — Integrate egui for menus/options and general UI widgets
- [x] Integrate `egui` with winit + wgpu (`egui-winit` + `egui-wgpu` or equivalent backend).
- [x] Render ordering is fixed:
  1) 3D scene (or placeholder clear)
  2) glyphon text overlays (HUD/console)
  3) egui UI (menus/options)
- [x] Implement menu skeleton:
  - Main menu: Start, Options, Quit
  - Options:
    - UI Scale (0.75–2.0)
    - VSync toggle (or placeholder if swapchain recreate not ready)
    - Master volume slider (must be wired if audio exists)
    - Display mode (windowed/borderless/fullscreen) with resolution selection
- [x] Implement persistent settings:
  - load on startup
  - save on change or on “Apply”
  - versioned settings struct with defaulting

**DoD**
- [x] UI scale changes take effect immediately and persist across restart.
- [x] Display mode/resolution changes apply immediately and persist across restart.
- [x] Options menu is usable at 720p, 1080p, 4K without overlap/clipping.

---

## D2 — Input focus routing (menu/console/game) and mouse capture policy
- [x] Implement a single `InputRouter` in `pallet` with this strict priority:
  1) If console open: keyboard to console; pointer optional; game does not receive gameplay input.
  2) Else if menu open: egui receives pointer/keyboard; game does not receive gameplay input.
  3) Else: game receives input; mouse captured/relative; cursor hidden.
- [x] Ensure “escape stack” behavior:
  - Esc closes console first; then menus; then returns to game.
- [x] Ensure toggle keys behave predictably:
  - backtick (or chosen key): toggle console (unless an egui text field has focus)

**DoD**
- [x] A scripted input sequence produces deterministic UI state transitions:
  - open menu → change slider → close
  - open console → type command → close
  - resume mouse-look

---

## D3 — High-quality text overlays with glyphon (HUD + console rendering)
- [x] Add glyphon renderer module (wgpu pass integration).
- [x] Use cosmic-text shaping/layout via glyphon (no custom shaper).
- [x] Font packaging:
  - `third_party/fonts/FiraSans/`
    - `FiraSans-Regular.ttf`
    - `FiraSans-Italic.ttf` (optional)
    - `FiraSans-Bold.ttf` (optional)
    - `LICENSE.txt` (OFL-1.1 text)
    - `SOURCE.txt` (where the files were obtained)
- [x] Canonical font sizes (avoid churn):
  - UI: 14pt, 16pt, 18pt
  - HUD: 14pt, 16pt
  - Console: 14pt or 16pt (pick one initially)
- [x] HUD overlay (glyphon):
  - FPS, sim tick rate, net tick rate, build string
- [x] Console overlay (glyphon):
  - translucent background quad (can be egui panel or a simple wgpu quad)
  - scrollback region with clipping (render only visible lines)
  - input line with caret
  - bounded ring buffer for log lines (prevent unbounded memory)

**DoD**
- [ ] Console and HUD are crisp and correctly aligned at:
  - 1280×720, 1920×1080, 3840×2160
  - DPI scale 1.0 and 2.0 (simulated or real)
- [ ] No per-frame allocations proportional to scrollback size.
- [ ] Scrolling a long log remains responsive.

---

## D4 — Resolution/DPI/UI-scale verification harness (repeatable, not eyeballed)
### Core requirement
You must be able to validate UI/text across a matrix without manual resizing.

- [ ] Implement a “UI verification scene” that renders:
  - main menu (open)
  - options menu (open; sliders set to midpoints)
  - console overlay with a known fixed set of log lines
  - HUD overlay with fixed numeric values
- [ ] Implement a matrix runner:
  - resolutions: 720p, 1080p, 1440p, 4K
  - dpi scales: 1.0, 1.5, 2.0 (simulated acceptable)
  - ui scales: 0.85, 1.0, 1.25, 1.5
- [ ] Implement screenshot capture to PNG for each case:
  - file naming includes all parameters
  - output manifest JSON lists each run and file path

**DoD**
- [ ] One command produces a “UI regression pack”:
  - `ui_regression/<timestamp>/manifest.json`
  - `ui_regression/<timestamp>/*.png`
- [ ] Invariants are checked per case:
  - minimum font height >= configured px threshold
  - no UI panel exceeds safe area
  - console input line visible
  - HUD text not clipped

---

## D5 — Performance measurement and budgets (so “not slow” is provable)
- [ ] Add CPU timing scopes:
  - egui build time
  - glyphon queue/layout time
  - glyphon render time
- [ ] Establish initial budgets (documented, adjustable):
  - egui: < X ms at 1080p typical scene
  - glyphon: < Y ms for HUD+console with N glyphs
- [ ] Add stress mode:
  - console contains 5k+ lines
  - 50k+ glyphs on screen (worst case)
  - rapid text edits for 5 seconds (typing simulation)

**DoD**
- [ ] Under stress mode, frame time remains stable (no unbounded growth).
- [ ] Atlas churn is bounded (no continuous re-upload of the same glyphs).

---

# Implementation notes (practical guidance)

## Why egui + glyphon is the recommended split
- egui is a full widget/UI system (menus/options/debug panels).
- glyphon is focused on efficient 2D text rendering with wgpu, cosmic-text shaping, and atlas packing.

## Avoid common performance traps
- Do not change font sizes every frame (destroys atlas reuse).
- Keep a small set of canonical font sizes (UI, HUD, console).
- Limit re-layout of huge scroll areas; render only visible lines for the console.

---

# Checklist 2 completion criteria (single-line acceptance)
Checklist 2 is complete when:
1) menus/options work and persist, 
2) console/HUD render via glyphon with a bundled redistributable font + license text,
3) input routing is correct, and 
4) the resolution/DPI/UI-scale matrix runner produces a screenshot pack with invariant checks, 
all while staying within documented performance budgets.
