use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use compat_quake::pak;
use zip::read::ZipArchive;

use crate::asset_id::AssetKey;
use crate::vfs::MountKind;

const INDEX_VERSION: u32 = 1;
const DEFAULT_INDEX_RELATIVE: &str = "build/compat/quake1/index.txt";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuakeAssetKind {
    Bsp,
    Texture,
    Sound,
    Model,
    Wad,
    Cfg,
    RawOther,
}

impl QuakeAssetKind {
    pub fn classify(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        let ext = lower.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
        match ext {
            "bsp" => QuakeAssetKind::Bsp,
            "lmp" | "pcx" | "tga" | "png" => QuakeAssetKind::Texture,
            "wav" | "ogg" | "mp3" => QuakeAssetKind::Sound,
            "mdl" | "md2" | "md3" => QuakeAssetKind::Model,
            "wad" => QuakeAssetKind::Wad,
            "cfg" => QuakeAssetKind::Cfg,
            _ => QuakeAssetKind::RawOther,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QuakeAssetKind::Bsp => "bsp",
            QuakeAssetKind::Texture => "texture",
            QuakeAssetKind::Sound => "sound",
            QuakeAssetKind::Model => "model",
            QuakeAssetKind::Wad => "wad",
            QuakeAssetKind::Cfg => "cfg",
            QuakeAssetKind::RawOther => "raw_other",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "bsp" => Some(QuakeAssetKind::Bsp),
            "texture" => Some(QuakeAssetKind::Texture),
            "sound" => Some(QuakeAssetKind::Sound),
            "model" => Some(QuakeAssetKind::Model),
            "wad" => Some(QuakeAssetKind::Wad),
            "cfg" => Some(QuakeAssetKind::Cfg),
            "raw_other" => Some(QuakeAssetKind::RawOther),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum QuakeSource {
    LooseFile {
        root: PathBuf,
    },
    Pak {
        pak_path: PathBuf,
        file_index: usize,
        offset: u32,
    },
    Pk3 {
        pk3_path: PathBuf,
        file_index: usize,
    },
}

impl QuakeSource {
    pub fn kind_label(&self) -> &'static str {
        match self {
            QuakeSource::LooseFile { .. } => "loose",
            QuakeSource::Pak { .. } => "pak",
            QuakeSource::Pk3 { .. } => "pk3",
        }
    }

    pub fn source_path(&self) -> &Path {
        match self {
            QuakeSource::LooseFile { root } => root,
            QuakeSource::Pak { pak_path, .. } => pak_path,
            QuakeSource::Pk3 { pk3_path, .. } => pk3_path,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QuakeEntry {
    pub path: String,
    pub kind: QuakeAssetKind,
    pub size: u64,
    pub source: QuakeSource,
    pub mount_order: usize,
    pub mount_kind: MountKind,
    pub hash: u64,
}

impl QuakeEntry {
    pub fn derived_asset_key(&self) -> Option<AssetKey> {
        derived_asset_key(&self.path, self.kind)
    }
}

#[derive(Clone, Debug)]
pub struct QuakeMount {
    pub order: usize,
    pub kind: MountKind,
    pub mount_point: String,
    pub source: PathBuf,
    pub size: u64,
    pub modified: u64,
}

#[derive(Clone, Debug)]
pub struct QuakeIndex {
    pub version: u32,
    pub fingerprint: String,
    pub mounts: Vec<QuakeMount>,
    pub entries: BTreeMap<String, Vec<QuakeEntry>>,
}

impl QuakeIndex {
    pub fn default_index_path(content_root: &Path) -> PathBuf {
        content_root.join(DEFAULT_INDEX_RELATIVE)
    }

    pub fn build_from_quake_dir(quake_dir: &Path) -> Result<Self, String> {
        let base_dir = quake_base_dir(quake_dir);
        let mounts = build_quake_mounts(&base_dir)?;
        let fingerprint = fingerprint_mounts(&mounts);
        let entries = build_entries(&mounts)?;
        Ok(Self {
            version: INDEX_VERSION,
            fingerprint,
            mounts,
            entries,
        })
    }

    pub fn load_cached(content_root: &Path, quake_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::default_index_path(content_root);
        if !path.is_file() {
            return Ok(None);
        }
        let index = Self::read_from(&path)?;
        let base_dir = quake_base_dir(quake_dir);
        let mounts = build_quake_mounts(&base_dir)?;
        let fingerprint = fingerprint_mounts(&mounts);
        if index.fingerprint == fingerprint {
            Ok(Some(index))
        } else {
            Ok(None)
        }
    }

    pub fn load_or_build(content_root: &Path, quake_dir: &Path) -> Result<Self, String> {
        if let Some(index) = Self::load_cached(content_root, quake_dir)? {
            return Ok(index);
        }
        Self::build_from_quake_dir(quake_dir)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.values().map(|items| items.len()).sum()
    }

    pub fn duplicates(&self) -> Vec<QuakeDuplicate> {
        let mut dupes = Vec::new();
        for (path, entries) in &self.entries {
            if entries.len() > 1 {
                let winner = entries[0].clone();
                let others = entries[1..].to_vec();
                dupes.push(QuakeDuplicate {
                    path: path.clone(),
                    winner,
                    others,
                });
            }
        }
        dupes
    }

    pub fn which(&self, path: &str) -> Option<QuakeWhich> {
        let key = normalize_entry_path(path)?;
        let entries = self.entries.get(&key)?;
        let winner = entries.first()?.clone();
        Some(QuakeWhich {
            path: key,
            winner,
            candidates: entries.clone(),
        })
    }

    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let mut lines = Vec::new();
        lines.push(format!("version={}", self.version));
        lines.push(format!("fingerprint={}", self.fingerprint));
        lines.push(format!("mount_count={}", self.mounts.len()));
        for mount in &self.mounts {
            lines.push(format!(
                "mount|{}|{}|{}|{}|{}|{}",
                mount.order,
                mount.kind,
                escape_field(&mount.mount_point),
                escape_field(&mount.source.display().to_string()),
                mount.size,
                mount.modified
            ));
        }
        lines.push(format!("entry_count={}", self.entry_count()));
        for (path_key, entries) in &self.entries {
            for entry in entries {
                let (source_kind, source_path, file_index, offset) = match &entry.source {
                    QuakeSource::LooseFile { root } => {
                        ("loose", root.display().to_string(), None, None)
                    }
                    QuakeSource::Pak {
                        pak_path,
                        file_index,
                        offset,
                    } => (
                        "pak",
                        pak_path.display().to_string(),
                        Some(*file_index),
                        Some(*offset),
                    ),
                    QuakeSource::Pk3 {
                        pk3_path,
                        file_index,
                    } => (
                        "pk3",
                        pk3_path.display().to_string(),
                        Some(*file_index),
                        None,
                    ),
                };
                lines.push(format!(
                    "entry|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    escape_field(path_key),
                    entry.kind.as_str(),
                    entry.size,
                    entry.hash,
                    entry.mount_order,
                    entry.mount_kind,
                    source_kind,
                    escape_field(&source_path),
                    file_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "".to_string()),
                    offset
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "".to_string())
                ));
            }
        }
        fs::write(path, lines.join("\n")).map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn read_from(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|err| err.to_string())?;
        let mut version = None;
        let mut fingerprint = None;
        let mut mounts = Vec::new();
        let mut entries: BTreeMap<String, Vec<QuakeEntry>> = BTreeMap::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some(value) = line.strip_prefix("version=") {
                version = Some(parse_u32(value, "version")?);
                continue;
            }
            if let Some(value) = line.strip_prefix("fingerprint=") {
                fingerprint = Some(value.trim().to_string());
                continue;
            }
            if line.starts_with("mount_count=") || line.starts_with("entry_count=") {
                continue;
            }
            let mut parts = line.split('|');
            let Some(tag) = parts.next() else {
                continue;
            };
            match tag {
                "mount" => {
                    let order = parse_usize(next_part(&mut parts, "mount order")?, "mount order")?;
                    let kind = parse_mount_kind(next_part(&mut parts, "mount kind")?)?;
                    let mount_point =
                        unescape_field(next_part(&mut parts, "mount point")?).to_string();
                    let source =
                        PathBuf::from(unescape_field(next_part(&mut parts, "mount source")?));
                    let size = parse_u64(next_part(&mut parts, "mount size")?, "mount size")?;
                    let modified =
                        parse_u64(next_part(&mut parts, "mount modified")?, "mount modified")?;
                    mounts.push(QuakeMount {
                        order,
                        kind,
                        mount_point,
                        source,
                        size,
                        modified,
                    });
                }
                "entry" => {
                    let path = unescape_field(next_part(&mut parts, "entry path")?).to_string();
                    let kind = QuakeAssetKind::from_str(next_part(&mut parts, "entry kind")?)
                        .ok_or_else(|| "unknown entry kind".to_string())?;
                    let size = parse_u64(next_part(&mut parts, "entry size")?, "entry size")?;
                    let hash = parse_u64(next_part(&mut parts, "entry hash")?, "entry hash")?;
                    let mount_order =
                        parse_usize(next_part(&mut parts, "entry order")?, "entry order")?;
                    let mount_kind = parse_mount_kind(next_part(&mut parts, "entry mount kind")?)?;
                    let source_kind = next_part(&mut parts, "source kind")?;
                    let source_path =
                        PathBuf::from(unescape_field(next_part(&mut parts, "source path")?));
                    let file_index = parse_optional_usize(next_part(&mut parts, "file index")?)?;
                    let offset = parse_optional_u32(next_part(&mut parts, "offset")?)?;
                    let source = match source_kind {
                        "loose" => QuakeSource::LooseFile { root: source_path },
                        "pak" => QuakeSource::Pak {
                            pak_path: source_path,
                            file_index: file_index.unwrap_or(0),
                            offset: offset.unwrap_or(0),
                        },
                        "pk3" => QuakeSource::Pk3 {
                            pk3_path: source_path,
                            file_index: file_index.unwrap_or(0),
                        },
                        _ => return Err("unknown source kind".to_string()),
                    };
                    let entry = QuakeEntry {
                        path: path.clone(),
                        kind,
                        size,
                        source,
                        mount_order,
                        mount_kind,
                        hash,
                    };
                    entries.entry(path).or_default().push(entry);
                }
                _ => {}
            }
        }
        let version = version.ok_or_else(|| "index missing version".to_string())?;
        let fingerprint = fingerprint.ok_or_else(|| "index missing fingerprint".to_string())?;
        if version != INDEX_VERSION {
            return Err(format!("unsupported index version {}", version));
        }
        for entries in entries.values_mut() {
            sort_entries(entries);
        }
        Ok(Self {
            version,
            fingerprint,
            mounts,
            entries,
        })
    }
}

#[derive(Clone, Debug)]
pub struct QuakeWhich {
    pub path: String,
    pub winner: QuakeEntry,
    pub candidates: Vec<QuakeEntry>,
}

#[derive(Clone, Debug)]
pub struct QuakeDuplicate {
    pub path: String,
    pub winner: QuakeEntry,
    pub others: Vec<QuakeEntry>,
}

fn quake_base_dir(quake_dir: &Path) -> PathBuf {
    let id1 = quake_dir.join("id1");
    if id1.is_dir() {
        id1
    } else {
        quake_dir.to_path_buf()
    }
}

fn build_quake_mounts(base_dir: &Path) -> Result<Vec<QuakeMount>, String> {
    if !base_dir.is_dir() {
        return Err(format!("quake dir not found: {}", base_dir.display()));
    }
    let mut mounts = Vec::new();
    mounts.push(QuakeMount {
        order: 0,
        kind: MountKind::Dir,
        mount_point: "raw/quake".to_string(),
        source: base_dir.to_path_buf(),
        size: 0,
        modified: modified_timestamp(base_dir).unwrap_or(0),
    });
    let mut pak_paths = Vec::new();
    for index in 0..10 {
        let path = base_dir.join(format!("pak{}.pak", index));
        if path.is_file() {
            pak_paths.push((index, path));
        }
    }
    pak_paths.sort_by(|a, b| b.0.cmp(&a.0));
    for (offset, (_, path)) in pak_paths.into_iter().enumerate() {
        let order = offset + 1;
        let size = file_size(&path).unwrap_or(0);
        let modified = modified_timestamp(&path).unwrap_or(0);
        mounts.push(QuakeMount {
            order,
            kind: MountKind::Pak,
            mount_point: "raw/quake".to_string(),
            source: path,
            size,
            modified,
        });
    }
    Ok(mounts)
}

fn build_entries(mounts: &[QuakeMount]) -> Result<BTreeMap<String, Vec<QuakeEntry>>, String> {
    let mut entries: BTreeMap<String, Vec<QuakeEntry>> = BTreeMap::new();
    for mount in mounts {
        match mount.kind {
            MountKind::Dir => collect_loose_entries(mount, &mut entries)?,
            MountKind::Pak => collect_pak_entries(mount, &mut entries)?,
            MountKind::Pk3 => collect_pk3_entries(mount, &mut entries)?,
        }
    }
    for entries in entries.values_mut() {
        sort_entries(entries);
    }
    Ok(entries)
}

fn sort_entries(entries: &mut [QuakeEntry]) {
    entries.sort_by(|a, b| {
        a.mount_order
            .cmp(&b.mount_order)
            .then_with(|| a.source.kind_label().cmp(b.source.kind_label()))
    });
}

fn collect_loose_entries(
    mount: &QuakeMount,
    entries: &mut BTreeMap<String, Vec<QuakeEntry>>,
) -> Result<(), String> {
    let mut files = Vec::new();
    walk_dir_files(&mount.source, &mount.source, &mut files)?;
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    for file in files {
        if is_container_asset(&file.rel) {
            continue;
        }
        let Some(path) = normalize_entry_path(&file.rel) else {
            continue;
        };
        let bytes = fs::read(&file.full).map_err(|err| err.to_string())?;
        let size = bytes.len() as u64;
        let hash = fnv1a64(&bytes);
        let kind = QuakeAssetKind::classify(&path);
        let entry = QuakeEntry {
            path: path.clone(),
            kind,
            size,
            source: QuakeSource::LooseFile {
                root: mount.source.clone(),
            },
            mount_order: mount.order,
            mount_kind: mount.kind,
            hash,
        };
        entries.entry(path).or_default().push(entry);
    }
    Ok(())
}

fn collect_pak_entries(
    mount: &QuakeMount,
    entries: &mut BTreeMap<String, Vec<QuakeEntry>>,
) -> Result<(), String> {
    let pak = pak::read_pak(&mount.source).map_err(|err| err.to_string())?;
    for (index, entry) in pak.entries().iter().enumerate() {
        let Some(path) = normalize_entry_path(&entry.name) else {
            continue;
        };
        let data = match pak.entry_data(&entry.name).map_err(|err| err.to_string())? {
            Some(bytes) => bytes,
            None => continue,
        };
        let size = data.len() as u64;
        let hash = fnv1a64(data);
        let kind = QuakeAssetKind::classify(&path);
        let entry = QuakeEntry {
            path: path.clone(),
            kind,
            size,
            source: QuakeSource::Pak {
                pak_path: mount.source.clone(),
                file_index: index,
                offset: entry.offset,
            },
            mount_order: mount.order,
            mount_kind: mount.kind,
            hash,
        };
        entries.entry(path).or_default().push(entry);
    }
    Ok(())
}

fn collect_pk3_entries(
    mount: &QuakeMount,
    entries: &mut BTreeMap<String, Vec<QuakeEntry>>,
) -> Result<(), String> {
    let file = fs::File::open(&mount.source).map_err(|err| err.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|err| err.to_string())?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| err.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let name = name.trim_end_matches('/');
        let Some(path) = normalize_entry_path(name) else {
            continue;
        };
        let mut buffer = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buffer)
            .map_err(|err| err.to_string())?;
        let size = buffer.len() as u64;
        let hash = fnv1a64(&buffer);
        let kind = QuakeAssetKind::classify(&path);
        let entry = QuakeEntry {
            path: path.clone(),
            kind,
            size,
            source: QuakeSource::Pk3 {
                pk3_path: mount.source.clone(),
                file_index: index,
            },
            mount_order: mount.order,
            mount_kind: mount.kind,
            hash,
        };
        entries.entry(path).or_default().push(entry);
    }
    Ok(())
}

struct LooseFile {
    full: PathBuf,
    rel: String,
}

fn walk_dir_files(root: &Path, current: &Path, files: &mut Vec<LooseFile>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(current)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            walk_dir_files(root, &path, files)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|err| err.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            files.push(LooseFile { full: path, rel });
        }
    }
    Ok(())
}

fn is_container_asset(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".pak") || lower.ends_with(".pk3")
}

fn normalize_entry_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/').trim_end_matches('/');
    if normalized.is_empty() {
        return None;
    }
    let lower = normalized.to_ascii_lowercase();
    let segments = lower.split('/');
    let mut out = Vec::new();
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        if segment.contains(':') {
            return None;
        }
        out.push(segment);
    }
    Some(out.join("/"))
}

fn derived_asset_key(path: &str, kind: QuakeAssetKind) -> Option<AssetKey> {
    match kind {
        QuakeAssetKind::Bsp => {
            let lower = path.to_ascii_lowercase();
            let map = lower.strip_prefix("maps/")?.strip_suffix(".bsp")?;
            AssetKey::from_parts("quake1", "bsp", map).ok()
        }
        QuakeAssetKind::Sound => {
            let lower = path.to_ascii_lowercase();
            let sound = lower.strip_prefix("sound/")?;
            let logical = sound.rsplit_once('.').map(|(stem, _)| stem)?;
            AssetKey::from_parts("quake1", "sound", logical).ok()
        }
        _ => None,
    }
}

fn fingerprint_mounts(mounts: &[QuakeMount]) -> String {
    let mut input = String::new();
    input.push_str(&format!("version={}\n", INDEX_VERSION));
    for mount in mounts {
        input.push_str(&format!(
            "{}|{}|{}|{}|{}|{}\n",
            mount.order,
            mount.kind,
            mount.mount_point,
            mount.source.display(),
            mount.size,
            mount.modified
        ));
    }
    format!("{:016x}", fnv1a64(input.as_bytes()))
}

fn modified_timestamp(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let elapsed = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(elapsed.as_secs())
}

fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|meta| meta.len())
}

fn parse_u32(value: &str, label: &str) -> Result<u32, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("invalid {}", label))
}

fn parse_u64(value: &str, label: &str) -> Result<u64, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("invalid {}", label))
}

fn parse_usize(value: &str, label: &str) -> Result<usize, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("invalid {}", label))
}

fn parse_optional_usize(value: &str) -> Result<Option<usize>, String> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        value
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| "invalid file index".to_string())
    }
}

fn parse_optional_u32(value: &str) -> Result<Option<u32>, String> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        value
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| "invalid offset".to_string())
    }
}

fn parse_mount_kind(value: &str) -> Result<MountKind, String> {
    match value {
        "dir" => Ok(MountKind::Dir),
        "pak" => Ok(MountKind::Pak),
        "pk3" => Ok(MountKind::Pk3),
        _ => Err("unknown mount kind".to_string()),
    }
}

fn next_part<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<&'a str, String> {
    parts.next().ok_or_else(|| format!("missing {}", label))
}

fn escape_field(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('|', "%7C")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
}

fn unescape_field(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let mut code = String::new();
            if let Some(a) = chars.next() {
                code.push(a);
            }
            if let Some(b) = chars.next() {
                code.push(b);
            }
            match code.as_str() {
                "25" => out.push('%'),
                "7C" => out.push('|'),
                "0A" => out.push('\n'),
                "0D" => out.push('\r'),
                _ => {
                    out.push('%');
                    out.push_str(&code);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
