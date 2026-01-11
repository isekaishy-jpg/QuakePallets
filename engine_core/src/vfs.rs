use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use compat_quake::pak;
use zip::read::ZipArchive;

#[derive(Debug)]
pub enum VfsError {
    Io(std::io::Error),
    NotFound(String),
    UnsafePath(String),
    Pak(String),
    Pk3(String),
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VfsError::Io(err) => write!(f, "io error: {}", err),
            VfsError::NotFound(path) => write!(f, "path not found: {}", path),
            VfsError::UnsafePath(path) => write!(f, "unsafe path: {}", path),
            VfsError::Pak(message) => write!(f, "pak error: {}", message),
            VfsError::Pk3(message) => write!(f, "pk3 error: {}", message),
        }
    }
}

impl std::error::Error for VfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VfsError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for VfsError {
    fn from(err: std::io::Error) -> Self {
        VfsError::Io(err)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountKind {
    Dir,
    Pak,
    Pk3,
}

impl fmt::Display for MountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            MountKind::Dir => "dir",
            MountKind::Pak => "pak",
            MountKind::Pk3 => "pk3",
        };
        write!(f, "{}", label)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsProvenance {
    pub mount_point: String,
    pub kind: MountKind,
    pub source: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountSummary {
    pub mount_point: String,
    pub kind: MountKind,
    pub source: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsMountCandidate {
    pub order: usize,
    pub mount_point: String,
    pub kind: MountKind,
    pub source: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Default)]
pub struct Vfs {
    mounts: Vec<VfsMount>,
}

#[derive(Debug)]
struct VfsMount {
    root: VirtualRoot,
    kind: MountKind,
    source: PathBuf,
    payload: MountPayload,
}

#[derive(Debug)]
enum MountPayload {
    Dir(PathBuf),
    Pak(PakMount),
    Pk3(Pk3Mount),
}

#[derive(Debug)]
struct PakMount {
    pak: pak::PakFile,
}

#[derive(Debug)]
struct Pk3Mount {
    entries: Vec<String>,
    lookup: HashMap<String, String>,
}

#[derive(Debug)]
struct VirtualPath {
    components: Vec<String>,
}

#[derive(Debug)]
struct VirtualRoot {
    components: Vec<String>,
    display: String,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_dir_mount(
        &mut self,
        mount_point: &str,
        path: impl Into<PathBuf>,
    ) -> Result<(), VfsError> {
        let root = VirtualRoot::parse(mount_point)?;
        let path = path.into();
        self.mounts.push(VfsMount {
            root,
            kind: MountKind::Dir,
            source: path.clone(),
            payload: MountPayload::Dir(path),
        });
        Ok(())
    }

    pub fn add_pak_mount(
        &mut self,
        mount_point: &str,
        path: impl Into<PathBuf>,
    ) -> Result<(), VfsError> {
        let root = VirtualRoot::parse(mount_point)?;
        let path = path.into();
        let data = fs::read(&path)?;
        let pak = pak::parse_pak(data).map_err(|err| VfsError::Pak(err.to_string()))?;
        self.mounts.push(VfsMount {
            root,
            kind: MountKind::Pak,
            source: path.clone(),
            payload: MountPayload::Pak(PakMount { pak }),
        });
        Ok(())
    }

    pub fn add_pk3_mount(
        &mut self,
        mount_point: &str,
        path: impl Into<PathBuf>,
    ) -> Result<(), VfsError> {
        let root = VirtualRoot::parse(mount_point)?;
        let path = path.into();
        let file = fs::File::open(&path)?;
        let mut archive =
            ZipArchive::new(file).map_err(|err| VfsError::Pk3(format!("open failed: {}", err)))?;
        let mut entries = Vec::new();
        let mut lookup = HashMap::new();
        for index in 0..archive.len() {
            let entry = archive
                .by_index(index)
                .map_err(|err| VfsError::Pk3(format!("entry {} failed: {}", index, err)))?;
            let raw_name = entry.name().replace('\\', "/");
            let name = raw_name.trim_end_matches('/').to_string();
            if name.is_empty() {
                continue;
            }
            if normalize_virtual_path(&name).is_err() {
                continue;
            }
            if entry.is_dir() {
                continue;
            }
            lookup.insert(name.clone(), raw_name);
            entries.push(name);
        }
        self.mounts.push(VfsMount {
            root,
            kind: MountKind::Pk3,
            source: path.clone(),
            payload: MountPayload::Pk3(Pk3Mount { entries, lookup }),
        });
        Ok(())
    }

    pub fn mounts(&self) -> Vec<MountSummary> {
        self.mounts
            .iter()
            .map(|mount| MountSummary {
                mount_point: mount.root.display.clone(),
                kind: mount.kind,
                source: mount.source.clone(),
            })
            .collect()
    }

    pub fn explain_mounts(&self, virtual_path: &str) -> Result<Vec<VfsMountCandidate>, VfsError> {
        let vpath = VirtualPath::parse(virtual_path)?;
        if vpath.components.is_empty() {
            return Err(VfsError::UnsafePath(virtual_path.to_string()));
        }
        let mut candidates = Vec::new();
        for (order, mount) in self.mounts.iter().enumerate() {
            let Some(rel) = mount.root.match_relative(&vpath) else {
                continue;
            };
            let exists = mount.exists(&rel);
            candidates.push(VfsMountCandidate {
                order,
                mount_point: mount.root.display.clone(),
                kind: mount.kind,
                source: mount.source.clone(),
                exists,
            });
        }
        Ok(candidates)
    }

    pub fn resolve_mount(&self, virtual_path: &str) -> Result<VfsMountCandidate, VfsError> {
        let candidates = self.explain_mounts(virtual_path)?;
        for candidate in &candidates {
            if candidate.exists {
                return Ok(candidate.clone());
            }
        }
        Err(VfsError::NotFound(virtual_path.to_string()))
    }

    pub fn read(&self, virtual_path: &str) -> Result<Vec<u8>, VfsError> {
        self.read_with_provenance(virtual_path)
            .map(|(data, _)| data)
    }

    pub fn read_to_string(&self, virtual_path: &str) -> Result<String, VfsError> {
        let data = self.read(virtual_path)?;
        String::from_utf8(data)
            .map_err(|err| VfsError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err)))
    }

    pub fn read_with_provenance(
        &self,
        virtual_path: &str,
    ) -> Result<(Vec<u8>, VfsProvenance), VfsError> {
        let vpath = VirtualPath::parse(virtual_path)?;
        if vpath.components.is_empty() {
            return Err(VfsError::UnsafePath(virtual_path.to_string()));
        }
        for mount in &self.mounts {
            let Some(rel) = mount.root.match_relative(&vpath) else {
                continue;
            };
            let Some(data) = mount.read(&rel)? else {
                continue;
            };
            let provenance = VfsProvenance {
                mount_point: mount.root.display.clone(),
                kind: mount.kind,
                source: mount.source.clone(),
            };
            return Ok((data, provenance));
        }
        Err(VfsError::NotFound(virtual_path.to_string()))
    }

    pub fn exists(&self, virtual_path: &str) -> bool {
        let Ok(vpath) = VirtualPath::parse(virtual_path) else {
            return false;
        };
        if vpath.components.is_empty() {
            return false;
        }
        for mount in &self.mounts {
            let Some(rel) = mount.root.match_relative(&vpath) else {
                continue;
            };
            if mount.exists(&rel) {
                return true;
            }
        }
        false
    }

    pub fn list_dir(&self, virtual_path: &str) -> Result<Vec<VfsEntry>, VfsError> {
        let vpath = VirtualPath::parse(virtual_path)?;
        let mut merged: HashMap<String, bool> = HashMap::new();
        let mut found = false;
        for mount in &self.mounts {
            let Some(rel) = mount.root.match_relative(&vpath) else {
                continue;
            };
            let Some(entries) = mount.list_dir(&rel)? else {
                continue;
            };
            found = true;
            for entry in entries {
                merged.entry(entry.name).or_insert(entry.is_dir);
            }
        }
        if !found {
            return Err(VfsError::NotFound(virtual_path.to_string()));
        }
        let mut entries: Vec<VfsEntry> = merged
            .into_iter()
            .map(|(name, is_dir)| VfsEntry { name, is_dir })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}

impl VfsMount {
    fn read(&self, rel: &VirtualPath) -> Result<Option<Vec<u8>>, VfsError> {
        if rel.components.is_empty() {
            return Ok(None);
        }
        match &self.payload {
            MountPayload::Dir(root) => {
                let path = safe_join(root, rel)?;
                if path.is_file() {
                    Ok(Some(fs::read(path)?))
                } else {
                    Ok(None)
                }
            }
            MountPayload::Pak(pak) => {
                let key = rel.normalized();
                match pak
                    .pak
                    .entry_data(&key)
                    .map_err(|err| VfsError::Pak(err.to_string()))?
                {
                    Some(bytes) => Ok(Some(bytes.to_vec())),
                    None => Ok(None),
                }
            }
            MountPayload::Pk3(pk3) => {
                let key = rel.normalized();
                let Some(archive_name) = pk3.lookup.get(&key) else {
                    return Ok(None);
                };
                let mut file = fs::File::open(&self.source)?;
                let mut archive = ZipArchive::new(&mut file)
                    .map_err(|err| VfsError::Pk3(format!("open failed: {}", err)))?;
                let mut entry = archive
                    .by_name(archive_name)
                    .map_err(|err| VfsError::Pk3(format!("read failed: {}", err)))?;
                let mut buffer = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buffer)?;
                Ok(Some(buffer))
            }
        }
    }

    fn exists(&self, rel: &VirtualPath) -> bool {
        if rel.components.is_empty() {
            return false;
        }
        match &self.payload {
            MountPayload::Dir(root) => safe_join(root, rel)
                .ok()
                .map(|path| path.is_file())
                .unwrap_or(false),
            MountPayload::Pak(pak) => pak.pak.entry_by_name(&rel.normalized()).is_some(),
            MountPayload::Pk3(pk3) => pk3.lookup.contains_key(&rel.normalized()),
        }
    }

    fn list_dir(&self, rel: &VirtualPath) -> Result<Option<Vec<VfsEntry>>, VfsError> {
        match &self.payload {
            MountPayload::Dir(root) => {
                let path = safe_join(root, rel)?;
                let Ok(read_dir) = fs::read_dir(&path) else {
                    return Ok(None);
                };
                let mut entries = Vec::new();
                for entry in read_dir.flatten() {
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    entries.push(VfsEntry {
                        name,
                        is_dir: file_type.is_dir(),
                    });
                }
                Ok(Some(entries))
            }
            MountPayload::Pak(pak) => {
                let entries = list_entries_from_names(&pak_file_names(pak), rel);
                if entries.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(entries))
                }
            }
            MountPayload::Pk3(pk3) => {
                let entries = list_entries_from_names(&pk3.entries, rel);
                if entries.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(entries))
                }
            }
        }
    }
}

impl VirtualPath {
    fn parse(path: &str) -> Result<Self, VfsError> {
        let components = normalize_virtual_path(path)?;
        Ok(Self { components })
    }

    fn normalized(&self) -> String {
        join_components(&self.components)
    }
}

impl VirtualRoot {
    fn parse(path: &str) -> Result<Self, VfsError> {
        let components = normalize_virtual_path(path)?;
        let display = join_components(&components);
        Ok(Self {
            components,
            display,
        })
    }

    fn match_relative(&self, path: &VirtualPath) -> Option<VirtualPath> {
        if self.components.len() > path.components.len() {
            return None;
        }
        if !path.components.starts_with(&self.components) {
            return None;
        }
        let rel = path.components[self.components.len()..].to_vec();
        Some(VirtualPath { components: rel })
    }
}

fn normalize_virtual_path(path: &str) -> Result<Vec<String>, VfsError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let normalized = trimmed.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(VfsError::UnsafePath(path.to_string()));
    }
    let mut components = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() {
            return Err(VfsError::UnsafePath(path.to_string()));
        }
        if part == "." || part == ".." {
            return Err(VfsError::UnsafePath(path.to_string()));
        }
        if part.contains(':') {
            return Err(VfsError::UnsafePath(path.to_string()));
        }
        components.push(part.to_string());
    }
    Ok(components)
}

fn join_components(components: &[String]) -> String {
    components.join("/")
}

fn safe_join(base: &Path, rel: &VirtualPath) -> Result<PathBuf, VfsError> {
    if rel.components.is_empty() {
        return Ok(base.to_path_buf());
    }
    let rel_str = rel.normalized();
    let rel_path = Path::new(&rel_str);
    let mut out = PathBuf::from(base);
    for component in rel_path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(VfsError::UnsafePath(rel_str.clone())),
        }
    }
    Ok(out)
}

fn pak_file_names(pak: &PakMount) -> Vec<String> {
    pak.pak
        .entries()
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

fn list_entries_from_names(names: &[String], rel: &VirtualPath) -> Vec<VfsEntry> {
    let prefix = if rel.components.is_empty() {
        String::new()
    } else {
        format!("{}/", rel.normalized())
    };
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for name in names {
        if normalize_virtual_path(name).is_err() {
            continue;
        }
        let Some(stripped) = name.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = stripped.split('/');
        let Some(head) = parts.next() else {
            continue;
        };
        if head.is_empty() {
            continue;
        }
        if !seen.insert(head.to_string()) {
            continue;
        }
        let is_dir = parts.next().is_some();
        entries.push(VfsEntry {
            name: head.to_string(),
            is_dir,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "pallet_vfs_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn vfs_rejects_unsafe_paths() {
        let mut vfs = Vfs::new();
        let root = temp_dir("unsafe");
        vfs.add_dir_mount("raw/quake", &root).unwrap();
        assert!(matches!(
            vfs.read("../secrets"),
            Err(VfsError::UnsafePath(_))
        ));
        assert!(matches!(
            vfs.list_dir("raw/quake/../other"),
            Err(VfsError::UnsafePath(_))
        ));
    }

    #[test]
    fn vfs_dir_precedence() {
        let root_a = temp_dir("dir_a");
        let root_b = temp_dir("dir_b");
        fs::write(root_a.join("test.txt"), b"a").unwrap();
        fs::write(root_b.join("test.txt"), b"b").unwrap();

        let mut vfs = Vfs::new();
        vfs.add_dir_mount("raw/quake", &root_a).unwrap();
        vfs.add_dir_mount("raw/quake", &root_b).unwrap();

        let data = vfs.read("raw/quake/test.txt").unwrap();
        assert_eq!(data, b"a");
    }

    #[test]
    fn vfs_pk3_reads() {
        let root = temp_dir("pk3");
        let pk3_path = root.join("test.pk3");
        let file = File::create(&pk3_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        writer.start_file("docs/readme.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.finish().unwrap();

        let mut vfs = Vfs::new();
        vfs.add_pk3_mount("raw/q3", &pk3_path).unwrap();
        let data = vfs.read("raw/q3/docs/readme.txt").unwrap();
        assert_eq!(data, b"hello");
    }
}
