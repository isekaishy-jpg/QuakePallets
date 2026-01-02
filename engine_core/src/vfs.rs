use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum VfsError {
    Io(std::io::Error),
    NotFound(String),
    UnsafePath(String),
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VfsError::Io(err) => write!(f, "io error: {}", err),
            VfsError::NotFound(path) => write!(f, "path not found: {}", path),
            VfsError::UnsafePath(path) => write!(f, "unsafe path: {}", path),
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

#[derive(Clone, Debug, Default)]
pub struct Vfs {
    roots: Vec<PathBuf>,
    mounts: Vec<VfsMount>,
}

#[derive(Clone, Debug)]
struct VfsMount {
    name: String,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsEntry {
    pub name: String,
    pub is_dir: bool,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_root(&mut self, path: impl Into<PathBuf>) {
        self.roots.push(path.into());
    }

    pub fn add_mount(&mut self, name: impl Into<String>, path: impl Into<PathBuf>) {
        self.mounts.push(VfsMount {
            name: name.into(),
            path: path.into(),
        });
    }

    pub fn read(&self, virtual_path: &str) -> Result<Vec<u8>, VfsError> {
        let path = self.find_file(virtual_path)?;
        Ok(fs::read(path)?)
    }

    pub fn read_to_string(&self, virtual_path: &str) -> Result<String, VfsError> {
        let path = self.find_file(virtual_path)?;
        Ok(fs::read_to_string(path)?)
    }

    pub fn exists(&self, virtual_path: &str) -> bool {
        self.resolve_candidates(virtual_path)
            .ok()
            .and_then(|candidates| candidates.into_iter().find(|p| p.exists()))
            .is_some()
    }

    pub fn list_dir(&self, virtual_path: &str) -> Result<Vec<VfsEntry>, VfsError> {
        let candidates = self.resolve_candidates(virtual_path)?;
        let mut seen = HashSet::new();
        let mut entries = Vec::new();

        for base in candidates {
            let Ok(read_dir) = fs::read_dir(&base) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(_) => continue,
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if seen.insert(name.clone()) {
                    entries.push(VfsEntry {
                        name,
                        is_dir: file_type.is_dir(),
                    });
                }
            }
        }

        if entries.is_empty() {
            return Err(VfsError::NotFound(virtual_path.to_string()));
        }

        Ok(entries)
    }

    fn find_file(&self, virtual_path: &str) -> Result<PathBuf, VfsError> {
        let candidates = self.resolve_candidates(virtual_path)?;
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(VfsError::NotFound(virtual_path.to_string()))
    }

    fn resolve_candidates(&self, virtual_path: &str) -> Result<Vec<PathBuf>, VfsError> {
        let normalized = virtual_path.replace('\\', "/");
        if normalized.is_empty() {
            return Err(VfsError::UnsafePath(virtual_path.to_string()));
        }

        for mount in &self.mounts {
            if normalized == mount.name {
                return Ok(vec![mount.path.clone()]);
            }
            let prefix = format!("{}/", mount.name);
            if normalized.starts_with(&prefix) {
                let rel = &normalized[prefix.len()..];
                return Ok(vec![safe_join(&mount.path, rel)?]);
            }
        }

        if self.roots.is_empty() {
            return Err(VfsError::NotFound(virtual_path.to_string()));
        }

        let mut candidates = Vec::with_capacity(self.roots.len());
        for root in &self.roots {
            candidates.push(safe_join(root, &normalized)?);
        }
        Ok(candidates)
    }
}

fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, VfsError> {
    let rel_path = Path::new(rel);
    let mut out = PathBuf::from(base);
    for component in rel_path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(VfsError::UnsafePath(rel.to_string())),
        }
    }

    if out.file_name() == Some(OsStr::new("")) {
        return Err(VfsError::UnsafePath(rel.to_string()));
    }

    Ok(out)
}
