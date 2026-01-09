# VFS MOUNTS - Raw Content Namespaces

## Virtual namespaces
- `raw/quake/...` for id Tech 1 content (Quake harness)
- `raw/q3/...` for id Tech 3 content (PK3 testbed)

These namespaces are reserved for **raw content** and are distinct from
engine-native packaged content (`content/...`).

## Mount ordering and precedence
Mounts are checked in the order they are added. The first matching mount that
contains the file wins.

### Quake mounts
`--quake-dir <path>` mounts:
1. `raw/quake` directory (highest precedence)
2. `raw/quake` pak mounts in descending order (`pak1.pak` before `pak0.pak`)

This matches the classic Quake rule: loose files override pak contents.

### Explicit mounts
Explicit `--mount-*` flags are added **before** the implicit `--quake-dir`
mounts, so they override baseline Quake content.

## CLI flags
`pallet` and `tools` accept:
- `--mount-dir <vroot> <path>`
- `--mount-pak <vroot> <path>`
- `--mount-pk3 <vroot> <path>`

Use `tools vfs stat <vpath>` to confirm size/hash and provenance.

For `tools vfs stat`, mount flags are applied in the order: dir → pak → pk3,
followed by any `--quake-dir` mounts.
