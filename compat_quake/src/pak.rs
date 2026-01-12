use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum PakError {
    Io(std::io::Error),
    InvalidHeader,
    DirectoryOutOfBounds,
    DirectorySizeNotMultiple,
    TooManyEntries { entries: usize },
    EntryOutOfBounds { name: String },
    NameNotUtf8,
    UnsafePath(String),
}

impl fmt::Display for PakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PakError::Io(err) => write!(f, "io error: {}", err),
            PakError::InvalidHeader => write!(f, "invalid pak header"),
            PakError::DirectoryOutOfBounds => write!(f, "pak directory out of bounds"),
            PakError::DirectorySizeNotMultiple => {
                write!(f, "pak directory size is not a multiple of 64")
            }
            PakError::TooManyEntries { entries } => {
                write!(f, "pak directory has too many entries: {}", entries)
            }
            PakError::EntryOutOfBounds { name } => {
                write!(f, "pak entry out of bounds: {}", name)
            }
            PakError::NameNotUtf8 => write!(f, "pak entry name is not utf-8"),
            PakError::UnsafePath(name) => write!(f, "pak entry path is unsafe: {}", name),
        }
    }
}

impl std::error::Error for PakError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PakError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PakError {
    fn from(err: std::io::Error) -> Self {
        PakError::Io(err)
    }
}

#[derive(Debug, Clone)]
pub struct PakEntry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug)]
pub struct PakFile {
    data: Vec<u8>,
    entries: Vec<PakEntry>,
}

impl PakFile {
    pub fn entries(&self) -> &[PakEntry] {
        &self.entries
    }

    pub fn entry_by_name(&self, name: &str) -> Option<&PakEntry> {
        let needle = sanitize_name(name);
        self.entries.iter().find(|entry| entry.name == needle)
    }

    pub fn entry_data(&self, name: &str) -> Result<Option<&[u8]>, PakError> {
        let entry = match self.entry_by_name(name) {
            Some(entry) => entry,
            None => return Ok(None),
        };

        let offset = entry.offset as usize;
        let size = entry.size as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| PakError::EntryOutOfBounds {
                name: entry.name.clone(),
            })?;
        if end > self.data.len() {
            return Err(PakError::EntryOutOfBounds {
                name: entry.name.clone(),
            });
        }

        Ok(Some(&self.data[offset..end]))
    }

    pub fn extract_all(&self, out_dir: &Path) -> Result<(), PakError> {
        fs::create_dir_all(out_dir)?;
        for entry in &self.entries {
            let out_path = safe_join(out_dir, &entry.name)?;
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let offset = entry.offset as usize;
            let size = entry.size as usize;
            let end = offset
                .checked_add(size)
                .ok_or_else(|| PakError::EntryOutOfBounds {
                    name: entry.name.clone(),
                })?;
            if end > self.data.len() {
                return Err(PakError::EntryOutOfBounds {
                    name: entry.name.clone(),
                });
            }
            fs::write(&out_path, &self.data[offset..end])?;
        }
        Ok(())
    }
}

pub fn read_pak(path: &Path) -> Result<PakFile, PakError> {
    let data = fs::read(path)?;
    parse_pak(data)
}

pub fn parse_pak(data: Vec<u8>) -> Result<PakFile, PakError> {
    const MAX_PAK_ENTRIES: usize = 100_000;

    if data.len() < 12 {
        return Err(PakError::InvalidHeader);
    }
    if &data[0..4] != b"PACK" {
        return Err(PakError::InvalidHeader);
    }

    let dir_offset = read_u32_le(&data[4..8]) as usize;
    let dir_size = read_u32_le(&data[8..12]) as usize;
    if !dir_size.is_multiple_of(64) {
        return Err(PakError::DirectorySizeNotMultiple);
    }
    let dir_end = dir_offset
        .checked_add(dir_size)
        .ok_or(PakError::DirectoryOutOfBounds)?;
    if dir_end > data.len() {
        return Err(PakError::DirectoryOutOfBounds);
    }

    let entry_count = dir_size / 64;
    if entry_count > MAX_PAK_ENTRIES {
        return Err(PakError::TooManyEntries {
            entries: entry_count,
        });
    }
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let base = dir_offset + i * 64;
        let name_bytes = &data[base..base + 56];
        let name_len = name_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_bytes.len());
        let name_raw = &name_bytes[..name_len];
        let name = std::str::from_utf8(name_raw).map_err(|_| PakError::NameNotUtf8)?;
        let name = sanitize_name(name);

        let offset = read_u32_le(&data[base + 56..base + 60]);
        let size = read_u32_le(&data[base + 60..base + 64]);

        let end = (offset as usize)
            .checked_add(size as usize)
            .ok_or_else(|| PakError::EntryOutOfBounds { name: name.clone() })?;
        if end > data.len() {
            return Err(PakError::EntryOutOfBounds { name });
        }

        entries.push(PakEntry { name, offset, size });
    }

    Ok(PakFile { data, entries })
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn sanitize_name(name: &str) -> String {
    let trimmed = name.trim_matches(char::from(0));
    trimmed.replace('\\', "/")
}

fn safe_join(base: &Path, entry: &str) -> Result<PathBuf, PakError> {
    let rel = Path::new(entry);
    let mut safe = PathBuf::from(base);
    for component in rel.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            _ => return Err(PakError::UnsafePath(entry.to_string())),
        }
    }

    if safe.file_name() == Some(OsStr::new("")) {
        return Err(PakError::UnsafePath(entry.to_string()));
    }

    Ok(safe)
}
