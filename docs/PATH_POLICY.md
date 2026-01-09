# PATH POLICY - Content + Config Resolution

## Invariant
No runtime-critical file access depends on the current working directory (CWD).
All content/config paths resolve from an explicit policy rooted at the executable,
the repo content tree, or explicit overrides.

## Canonical content tree (dev and packaged)
```
content/
  config/
    playlists/
    scripts/
    cvars/
    mounts/
```

Config-like files belong under `content/config/...`. For example:
- `content/config/playlists/movies_playlist.txt`
- `content/config/scripts/demo.lua`
- `content/config/mounts/default.txt`

## Dev override root
`/.pallet/` (gitignored)
```
.pallet/
  config/
    playlists/
    scripts/
    cvars/
    mounts/
```

## User config root
Resolved from OS config locations:
- Windows: `%APPDATA%/Pallet`
- Linux: `$XDG_CONFIG_HOME/pallet` or `$HOME/.config/pallet`
- Fallback: `pallet_config/`

## Content root resolution
The engine resolves `content_root` in this order:
1. CLI override: `--content-root <path>`
2. Repo dev tree: `repo_root/content` when a `Cargo.toml` is found above the exe
3. Packaged default: `<exe_dir>/content`

## Config resolution order (playlist/scripts/cvars/mounts)
For a given config name (or absolute path), resolution is deterministic:
1. CLI absolute path (explicit override)
2. Environment override (debug-only)
3. Dev override root (`.pallet/config/...`)
4. Shipped defaults (`content/config/...`)
5. User config (`<user_config_root>/config/...`)
6. Built-in defaults (when explicitly allowed by the caller)

Resolver diagnostics are available via `ResolvedPath::describe()` to show which
candidate matched and why.

## CLI overrides
- `--content-root <path>`: force content root
- `--dev-root <path>`: force dev override root
- `--config-root <path>`: force user config root
