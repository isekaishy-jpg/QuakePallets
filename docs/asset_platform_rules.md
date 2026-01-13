# Asset Platform Rules

## Policy
- Runtime asset reads go through `AssetManager` and `AssetResolver` using asset IDs.
- Avoid direct `std::fs` reads in runtime code paths.
- Use `engine:config` for shipped config files under `content/config`.

## Allowed exemptions (v1)
- User config bootstrap reads/writes (`pallet/src/settings.rs`).
- Mount manifest bootstrap before VFS/AssetManager exists.
- Log/profiling output writes.
- Dev-only experiments gated by `DevOnly` and clearly labeled.

## Asset kind mapping
- `engine:config/<path>` -> `content/config/<path>` (dev override: `.pallet/config`).
- `engine:text/<path>` -> `content/text/<path>`.
- `engine:script/<path>` -> `content/script/<path>` (dev override: `.pallet/content/script`).
- `engine:texture/<path>` -> `content/texture/<path>`.
- `engine:blob/<path>` -> `content/blob/<path>`.
- `engine:collision_world/<path>` -> `content/collision_world/<path>`.
- `quake1:raw/<path>` -> `raw/quake/<path>` via VFS.

## Adding a new asset kind
1. Update `engine_core/src/asset_id.rs` (known kinds).
2. Add `AssetKind` + `AssetPayload` decode in `engine_core/src/asset_manager.rs`.
3. Map resolver behavior in `engine_core/src/asset_resolver.rs`.
4. Add parse/resolve tests.
5. Update tools/build manifest handling if needed.

## Diagnostics
- `tools content mounts/resolve/explain`
- `dev_asset_*` console commands in Pallet
- `tools content build` -> `content/build/build_manifest.txt`
