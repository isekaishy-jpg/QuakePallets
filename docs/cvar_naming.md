# cvar_naming.md
## Naming rules (cvars + commands)

### Global rules
- snake_case only
- lowercase only
- no `.` characters
- no hyphens; no camelCase

### Cvars (domain-first)
Format:
- `domain_topic_detail[_unit]`

Examples:
- `dbg_overlay`
- `dbg_fps`
- `render_wireframe`
- `render_vsync`
- `audio_master_volume`
- `net_sim_latency_ms`
- `net_sim_loss_pct`

Units:
- Use explicit unit suffixes when applicable: `_ms`, `_pct`, `_hz`, `_mb`, `_bytes`.

Values:
- Boolean values use numeric `0`/`1` only.
- Mode/enumeration values use `0` = off, `1` = on, `2+` = additional modes.

Dev-only:
- Prefix dev-only cvars with `dev_` (allowed to be unstable and/or hidden in non-dev builds).
  - Examples: `dev_render_force_validation`, `dev_dbg_overlay_layout`

### Commands (verb-first)
Format:
- `verb_object[_detail]`

Examples:
- `help`
- `cvar_list`, `cvar_get`, `cvar_set`, `cvar_toggle`
- `cmd_list`
- `cfg_list`, `cfg_load`, `cfg_save`, `cfg_select`
- `capture_screenshot`, `capture_frame`

Values:
- Command toggles use numeric mode arguments: `0` = off, `1` = on, `2+` = additional modes.

Dev-only:
- Prefix dev-only commands with `dev_`.
  - Examples: `dev_exec`, `dev_dump_assets`

### File/path values
- Cvar/command *names* are always snake_case lowercase.
- String *values* that reference files/paths are treated as opaque strings and may be mixed-case.
