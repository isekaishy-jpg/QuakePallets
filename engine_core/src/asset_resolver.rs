use std::path::{Path, PathBuf};

use crate::asset_id::AssetKey;
use crate::path_policy::PathPolicy;
use crate::vfs::{MountKind, Vfs, VfsMountCandidate};

const QUAKE1_VROOT: &str = "raw/quake";
const QUAKELIVE_VROOT: &str = "raw/quakelive";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetLayer {
    Shipped,
    Dev,
    User,
}

#[derive(Clone, Debug)]
pub enum AssetSource {
    EngineContent {
        root: PathBuf,
    },
    EngineBundle {
        bundle_id: String,
        source: PathBuf,
    },
    Quake1 {
        mount_kind: MountKind,
        source: PathBuf,
    },
    QuakeLive {
        mount_kind: MountKind,
        source: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub enum ResolvedPath {
    File(PathBuf),
    Vfs(String),
    Bundle {
        bundle_id: String,
        entry_id: String,
        offset: Option<u64>,
    },
}

#[derive(Clone, Debug)]
pub struct ResolvedLocation {
    pub key: AssetKey,
    pub source: AssetSource,
    pub layer: AssetLayer,
    pub mount_name: String,
    pub mount_order: usize,
    pub path: ResolvedPath,
}

#[derive(Clone, Debug)]
pub struct ResolveCandidate {
    pub mount_name: String,
    pub mount_order: usize,
    pub layer: AssetLayer,
    pub source: AssetSource,
    pub path: ResolvedPath,
    pub exists: bool,
}

#[derive(Clone, Debug)]
pub struct ResolveReport {
    pub key: AssetKey,
    pub candidates: Vec<ResolveCandidate>,
    pub winner: Option<ResolvedLocation>,
}

#[derive(Clone, Debug)]
pub struct AssetMountEntry {
    pub namespace: String,
    pub mount_name: String,
    pub mount_order: usize,
    pub layer: AssetLayer,
    pub kind: AssetMountKind,
}

#[derive(Clone, Debug)]
pub enum AssetMountKind {
    Directory {
        root: PathBuf,
    },
    Vfs {
        mount_point: String,
        mount_kind: MountKind,
        source: PathBuf,
    },
    Bundle {
        bundle_id: String,
        bundle_path: PathBuf,
    },
}

#[derive(Clone, Debug, Default)]
pub struct AssetMountTable {
    pub entries: Vec<AssetMountEntry>,
}

pub struct AssetResolver<'a> {
    path_policy: &'a PathPolicy,
    vfs: Option<&'a Vfs>,
    mounts: AssetMountTable,
}

impl<'a> AssetResolver<'a> {
    pub fn new(path_policy: &'a PathPolicy, vfs: Option<&'a Vfs>) -> Self {
        let mounts = build_mount_table(path_policy, vfs);
        Self {
            path_policy,
            vfs,
            mounts,
        }
    }

    pub fn mounts(&self) -> &AssetMountTable {
        &self.mounts
    }

    pub fn resolve(&self, key: &AssetKey) -> Result<ResolvedLocation, String> {
        let report = self.explain(key)?;
        report
            .winner
            .ok_or_else(|| format!("asset not found: {}", key.canonical()))
    }

    pub fn explain(&self, key: &AssetKey) -> Result<ResolveReport, String> {
        let mut candidates = Vec::new();
        let mut winner = None;

        match key.namespace() {
            "engine" => {
                if key.kind() == "config" {
                    let (dev_root, shipped_root) = engine_config_roots(self.path_policy);
                    let rel_path = Path::new(key.path());
                    if let Some(root) = dev_root {
                        let full = root.join(rel_path);
                        let exists = full.is_file();
                        let source = AssetSource::EngineContent { root: root.clone() };
                        let candidate = ResolveCandidate {
                            mount_name: "engine_config_dev_override".to_string(),
                            mount_order: 0,
                            layer: AssetLayer::Dev,
                            source: source.clone(),
                            path: ResolvedPath::File(full.clone()),
                            exists,
                        };
                        if winner.is_none() && exists {
                            winner = Some(ResolvedLocation {
                                key: key.clone(),
                                source,
                                layer: AssetLayer::Dev,
                                mount_name: "engine_config_dev_override".to_string(),
                                mount_order: 0,
                                path: ResolvedPath::File(full),
                            });
                        }
                        candidates.push(candidate);
                    }

                    let shipped_path = shipped_root.join(rel_path);
                    let shipped_exists = shipped_path.is_file();
                    let shipped_source = AssetSource::EngineContent {
                        root: shipped_root.clone(),
                    };
                    let candidate = ResolveCandidate {
                        mount_name: "engine_config".to_string(),
                        mount_order: 1,
                        layer: AssetLayer::Shipped,
                        source: shipped_source.clone(),
                        path: ResolvedPath::File(shipped_path.clone()),
                        exists: shipped_exists,
                    };
                    if winner.is_none() && shipped_exists {
                        winner = Some(ResolvedLocation {
                            key: key.clone(),
                            source: shipped_source,
                            layer: AssetLayer::Shipped,
                            mount_name: "engine_config".to_string(),
                            mount_order: 1,
                            path: ResolvedPath::File(shipped_path),
                        });
                    }
                    candidates.push(candidate);
                } else {
                    let (dev_root, shipped_root) = engine_roots(self.path_policy);
                    let rel_path = engine_relative_path(key);
                    if let Some(root) = dev_root {
                        let full = root.join(&rel_path);
                        let exists = full.is_file();
                        let source = AssetSource::EngineContent { root: root.clone() };
                        let candidate = ResolveCandidate {
                            mount_name: "engine_dev_override".to_string(),
                            mount_order: 0,
                            layer: AssetLayer::Dev,
                            source: source.clone(),
                            path: ResolvedPath::File(full.clone()),
                            exists,
                        };
                        if winner.is_none() && exists {
                            winner = Some(ResolvedLocation {
                                key: key.clone(),
                                source,
                                layer: AssetLayer::Dev,
                                mount_name: "engine_dev_override".to_string(),
                                mount_order: 0,
                                path: ResolvedPath::File(full),
                            });
                        }
                        candidates.push(candidate);
                    }

                    let shipped_path = shipped_root.join(&rel_path);
                    let shipped_exists = shipped_path.is_file();
                    let shipped_source = AssetSource::EngineContent {
                        root: shipped_root.clone(),
                    };
                    let candidate = ResolveCandidate {
                        mount_name: "engine_content".to_string(),
                        mount_order: 1,
                        layer: AssetLayer::Shipped,
                        source: shipped_source.clone(),
                        path: ResolvedPath::File(shipped_path.clone()),
                        exists: shipped_exists,
                    };
                    if winner.is_none() && shipped_exists {
                        winner = Some(ResolvedLocation {
                            key: key.clone(),
                            source: shipped_source,
                            layer: AssetLayer::Shipped,
                            mount_name: "engine_content".to_string(),
                            mount_order: 1,
                            path: ResolvedPath::File(shipped_path),
                        });
                    }
                    candidates.push(candidate);
                }
            }
            "quake1" => {
                let vfs = self
                    .vfs
                    .ok_or_else(|| "quake1 resolution requires a VFS mount".to_string())?;
                let vpath = quake_vpath(QUAKE1_VROOT, key.kind(), key.path());
                build_quake_candidates(key, vfs, &vpath, "quake1", &mut candidates, &mut winner)?;
            }
            "quakelive" => {
                let vfs = self
                    .vfs
                    .ok_or_else(|| "quakelive resolution requires a VFS mount".to_string())?;
                let vpath = quake_vpath(QUAKELIVE_VROOT, key.kind(), key.path());
                build_quake_candidates(
                    key,
                    vfs,
                    &vpath,
                    "quakelive",
                    &mut candidates,
                    &mut winner,
                )?;
            }
            _ => {
                return Err(format!(
                    "asset namespace '{}' not supported by resolver",
                    key.namespace()
                ))
            }
        }

        Ok(ResolveReport {
            key: key.clone(),
            candidates,
            winner,
        })
    }
}

fn build_mount_table(path_policy: &PathPolicy, vfs: Option<&Vfs>) -> AssetMountTable {
    let mut table = AssetMountTable::default();
    let (dev_root, shipped_root) = engine_roots(path_policy);
    if let Some(root) = dev_root {
        table.entries.push(AssetMountEntry {
            namespace: "engine".to_string(),
            mount_name: "engine_dev_override".to_string(),
            mount_order: 0,
            layer: AssetLayer::Dev,
            kind: AssetMountKind::Directory { root },
        });
    }
    table.entries.push(AssetMountEntry {
        namespace: "engine".to_string(),
        mount_name: "engine_content".to_string(),
        mount_order: 1,
        layer: AssetLayer::Shipped,
        kind: AssetMountKind::Directory { root: shipped_root },
    });

    let (dev_config_root, shipped_config_root) = engine_config_roots(path_policy);
    if let Some(root) = dev_config_root {
        table.entries.push(AssetMountEntry {
            namespace: "engine".to_string(),
            mount_name: "engine_config_dev_override".to_string(),
            mount_order: 0,
            layer: AssetLayer::Dev,
            kind: AssetMountKind::Directory { root },
        });
    }
    table.entries.push(AssetMountEntry {
        namespace: "engine".to_string(),
        mount_name: "engine_config".to_string(),
        mount_order: 1,
        layer: AssetLayer::Shipped,
        kind: AssetMountKind::Directory {
            root: shipped_config_root,
        },
    });

    if let Some(vfs) = vfs {
        table
            .entries
            .extend(quake_mount_entries(vfs, QUAKE1_VROOT, "quake1"));
        table
            .entries
            .extend(quake_mount_entries(vfs, QUAKELIVE_VROOT, "quakelive"));
    }

    table
}

fn quake_mount_entries(vfs: &Vfs, vroot: &str, namespace: &str) -> Vec<AssetMountEntry> {
    let mut entries = Vec::new();
    for (index, mount) in vfs.mounts().into_iter().enumerate() {
        if mount.mount_point != vroot {
            continue;
        }
        entries.push(AssetMountEntry {
            namespace: namespace.to_string(),
            mount_name: mount.mount_point.clone(),
            mount_order: index,
            layer: layer_for_mount_kind(mount.kind),
            kind: AssetMountKind::Vfs {
                mount_point: mount.mount_point,
                mount_kind: mount.kind,
                source: mount.source,
            },
        });
    }
    entries
}

fn layer_for_mount_kind(kind: MountKind) -> AssetLayer {
    match kind {
        MountKind::Dir => AssetLayer::User,
        MountKind::Pak | MountKind::Pk3 => AssetLayer::Shipped,
    }
}

fn engine_roots(path_policy: &PathPolicy) -> (Option<PathBuf>, PathBuf) {
    let shipped_root = path_policy.content_root().to_path_buf();
    let dev_root = path_policy
        .dev_override_root()
        .map(|root| root.join("content"))
        .filter(|root| root.is_dir());
    (dev_root, shipped_root)
}

fn engine_config_roots(path_policy: &PathPolicy) -> (Option<PathBuf>, PathBuf) {
    let shipped_root = path_policy.content_root().join("config");
    let dev_root = path_policy
        .dev_override_root()
        .map(|root| root.join("config"))
        .filter(|root| root.is_dir());
    (dev_root, shipped_root)
}

fn engine_relative_path(key: &AssetKey) -> PathBuf {
    if key.kind() == "test_map" {
        Path::new("test_maps").join(key.path())
    } else {
        Path::new(key.kind()).join(key.path())
    }
}

fn quake_vpath(root: &str, kind: &str, path: &str) -> String {
    let suffix = if kind == "raw" || kind == "raw_other" {
        path.to_string()
    } else {
        format!("{}/{}", kind, path)
    };
    format!("{}/{}", root, suffix)
}

fn build_quake_candidates(
    key: &AssetKey,
    vfs: &Vfs,
    vpath: &str,
    namespace: &str,
    candidates: &mut Vec<ResolveCandidate>,
    winner: &mut Option<ResolvedLocation>,
) -> Result<(), String> {
    let mount_candidates = vfs
        .explain_mounts(vpath)
        .map_err(|err| format!("vfs explain failed: {}", err))?;
    let filtered = filter_quake_candidates(namespace, mount_candidates);
    for candidate in filtered {
        let source = match namespace {
            "quake1" => AssetSource::Quake1 {
                mount_kind: candidate.kind,
                source: candidate.source.clone(),
            },
            "quakelive" => AssetSource::QuakeLive {
                mount_kind: candidate.kind,
                source: candidate.source.clone(),
            },
            _ => return Err("unsupported quake namespace".to_string()),
        };
        let entry = ResolveCandidate {
            mount_name: candidate.mount_point.clone(),
            mount_order: candidate.order,
            layer: layer_for_mount_kind(candidate.kind),
            source: source.clone(),
            path: ResolvedPath::Vfs(vpath.to_string()),
            exists: candidate.exists,
        };
        if winner.is_none() && candidate.exists {
            *winner = Some(ResolvedLocation {
                key: key.clone(),
                source,
                layer: layer_for_mount_kind(candidate.kind),
                mount_name: candidate.mount_point.clone(),
                mount_order: candidate.order,
                path: ResolvedPath::Vfs(vpath.to_string()),
            });
        }
        candidates.push(entry);
    }
    Ok(())
}

fn filter_quake_candidates(
    namespace: &str,
    candidates: Vec<VfsMountCandidate>,
) -> Vec<VfsMountCandidate> {
    let vroot = match namespace {
        "quake1" => QUAKE1_VROOT,
        "quakelive" => QUAKELIVE_VROOT,
        _ => "",
    };
    candidates
        .into_iter()
        .filter(|candidate| candidate.mount_point == vroot)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_policy::{PathOverrides, PathPolicy};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            path.push(format!("pallet_test_{}_{}", label, stamp));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn resolver_prefers_dev_override() {
        let temp = TempDir::new("resolver_override");
        let shipped_root = temp.path().join("content");
        let dev_root = temp.path().join("dev_override");

        let shipped_path = shipped_root.join("texture").join("fixtures");
        let dev_path = dev_root.join("content").join("texture").join("fixtures");
        fs::create_dir_all(&shipped_path).expect("create shipped path");
        fs::create_dir_all(&dev_path).expect("create dev path");

        let shipped_file = shipped_path.join("golden.png");
        let dev_file = dev_path.join("golden.png");
        fs::write(&shipped_file, [0u8, 1u8]).expect("write shipped file");
        fs::write(&dev_file, [2u8, 3u8]).expect("write dev file");

        let path_policy = PathPolicy::from_overrides(PathOverrides {
            content_root: Some(shipped_root),
            dev_override_root: Some(dev_root),
            user_config_root: None,
        });
        let resolver = AssetResolver::new(&path_policy, None);
        let key = AssetKey::parse("engine:texture/fixtures/golden.png").expect("asset key parse");
        let location = resolver.resolve(&key).expect("resolve");

        assert_eq!(location.layer, AssetLayer::Dev);
        assert_eq!(location.mount_name, "engine_dev_override");
        match location.path {
            ResolvedPath::File(path) => assert_eq!(path, dev_file),
            _ => panic!("expected file path"),
        }
    }
}
