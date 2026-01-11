use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::asset_id::AssetKey;
use crate::path_policy::PathPolicy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelManifestSource {
    DevOverride,
    ShippedDefault,
}

#[derive(Clone, Debug)]
pub struct LevelManifestPath {
    pub key: AssetKey,
    pub path: PathBuf,
    pub source: LevelManifestSource,
}

#[derive(Clone, Debug, Default)]
pub struct LevelManifestLines {
    pub version: Option<usize>,
    pub geometry: Option<usize>,
    pub assets: Option<usize>,
    pub requires: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct LevelManifest {
    pub version: u32,
    pub geometry: Option<AssetKey>,
    pub assets: Vec<AssetKey>,
    pub requires: Vec<AssetKey>,
    pub lines: LevelManifestLines,
}

impl Default for LevelManifest {
    fn default() -> Self {
        Self {
            version: 1,
            geometry: None,
            assets: Vec::new(),
            requires: Vec::new(),
            lines: LevelManifestLines::default(),
        }
    }
}

impl LevelManifest {
    pub fn dependencies(&self) -> Vec<AssetKey> {
        let mut deps = Vec::new();
        if let Some(geometry) = &self.geometry {
            deps.push(geometry.clone());
        }
        deps.extend(self.assets.iter().cloned());
        deps.extend(self.requires.iter().cloned());
        deps
    }
}

#[derive(Clone, Debug)]
pub struct LevelManifestError {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub field: Option<String>,
    pub message: String,
}

impl fmt::Display for LevelManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "level manifest error ({})", self.path.display())?;
        if let Some(line) = self.line {
            write!(f, ":{}", line)?;
        }
        if let Some(field) = &self.field {
            write!(f, " [{}]", field)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for LevelManifestError {}

pub fn load_level_manifest(path: &Path) -> Result<LevelManifest, LevelManifestError> {
    let contents = std::fs::read_to_string(path).map_err(|err| LevelManifestError {
        path: path.to_path_buf(),
        line: None,
        field: None,
        message: format!("manifest read failed: {}", err),
    })?;
    parse_level_manifest(path, &contents)
}

pub fn resolve_level_manifest_path(
    path_policy: &PathPolicy,
    key: &AssetKey,
) -> Result<LevelManifestPath, LevelManifestError> {
    if key.namespace() != "engine" || key.kind() != "level" {
        return Err(LevelManifestError {
            path: PathBuf::from("<level>"),
            line: None,
            field: Some("level".to_string()),
            message: format!("expected engine:level, got {}", key.canonical()),
        });
    }
    let relative = level_relative_path(key.path());
    if let Some(root) = path_policy.dev_override_root() {
        let candidate = root.join("content").join("levels").join(&relative);
        if candidate.is_file() {
            return Ok(LevelManifestPath {
                key: key.clone(),
                path: candidate,
                source: LevelManifestSource::DevOverride,
            });
        }
    }
    let shipped = path_policy.content_root().join("levels").join(&relative);
    if shipped.is_file() {
        return Ok(LevelManifestPath {
            key: key.clone(),
            path: shipped,
            source: LevelManifestSource::ShippedDefault,
        });
    }
    Err(LevelManifestError {
        path: shipped,
        line: None,
        field: Some("level".to_string()),
        message: format!("level manifest not found for {}", key.canonical()),
    })
}

pub fn discover_level_manifests(
    path_policy: &PathPolicy,
) -> Result<Vec<LevelManifestPath>, LevelManifestError> {
    let mut found: BTreeMap<String, LevelManifestPath> = BTreeMap::new();
    let shipped_root = path_policy.content_root().join("levels");
    collect_level_paths(
        &shipped_root,
        LevelManifestSource::ShippedDefault,
        &mut found,
    )?;
    if let Some(root) = path_policy.dev_override_root() {
        let dev_root = root.join("content").join("levels");
        collect_level_paths(&dev_root, LevelManifestSource::DevOverride, &mut found)?;
    }
    Ok(found.into_values().collect())
}

fn collect_level_paths(
    root: &Path,
    source: LevelManifestSource,
    found: &mut BTreeMap<String, LevelManifestPath>,
) -> Result<(), LevelManifestError> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut files = Vec::new();
    walk_level_files(root, &mut files).map_err(|err| LevelManifestError {
        path: root.to_path_buf(),
        line: None,
        field: None,
        message: err,
    })?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in files {
        let level_path =
            level_path_from_manifest(root, &file.path).map_err(|err| LevelManifestError {
                path: file.path.clone(),
                line: None,
                field: Some("level".to_string()),
                message: err,
            })?;
        let key = AssetKey::from_parts("engine", "level", &level_path).map_err(|err| {
            LevelManifestError {
                path: file.path.clone(),
                line: None,
                field: Some("level".to_string()),
                message: err.to_string(),
            }
        })?;
        found.insert(
            key.canonical().to_string(),
            LevelManifestPath {
                key,
                path: file.path.clone(),
                source,
            },
        );
    }
    Ok(())
}

struct LevelFile {
    path: PathBuf,
}

fn walk_level_files(root: &Path, out: &mut Vec<LevelFile>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            walk_level_files(&path, out)?;
        } else if file_type.is_file()
            && path.file_name().map(|name| name == "level.toml") == Some(true)
        {
            out.push(LevelFile { path });
        }
    }
    Ok(())
}

fn level_path_from_manifest(root: &Path, manifest: &Path) -> Result<String, String> {
    let parent = manifest
        .parent()
        .ok_or_else(|| "manifest has no parent directory".to_string())?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| "manifest not under levels root".to_string())?;
    let rel = relative.to_string_lossy().replace('\\', "/");
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return Err("level path must not be empty".to_string());
    }
    Ok(rel.to_string())
}

fn level_relative_path(level_path: &str) -> PathBuf {
    Path::new(level_path).join("level.toml")
}

fn parse_level_manifest(path: &Path, contents: &str) -> Result<LevelManifest, LevelManifestError> {
    let mut manifest = LevelManifest::default();
    let mut seen = HashSet::new();
    let mut pending: Option<(String, usize, String)> = None;

    for (idx, raw_line) in contents.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_comment(raw_line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && !line.contains('=') {
            continue;
        }

        if let Some((key, start_line, mut buffer)) = pending.take() {
            buffer.push('\n');
            buffer.push_str(line);
            if let Some(balance) = bracket_balance(&buffer) {
                if balance > 0 {
                    pending = Some((key, start_line, buffer));
                    continue;
                }
                if balance < 0 {
                    return Err(LevelManifestError {
                        path: path.to_path_buf(),
                        line: Some(line_no),
                        field: Some(key),
                        message: "array has too many closing brackets".to_string(),
                    });
                }
            }
            apply_field(
                &mut manifest,
                &mut seen,
                path,
                start_line,
                &key,
                buffer.trim(),
            )?;
            continue;
        }

        let (key, value) = split_assignment(line).map_err(|message| LevelManifestError {
            path: path.to_path_buf(),
            line: Some(line_no),
            field: None,
            message,
        })?;
        if let Some(balance) = bracket_balance(&value) {
            if balance > 0 {
                pending = Some((key, line_no, value));
                continue;
            }
            if balance < 0 {
                return Err(LevelManifestError {
                    path: path.to_path_buf(),
                    line: Some(line_no),
                    field: Some(key),
                    message: "array has too many closing brackets".to_string(),
                });
            }
        }
        apply_field(&mut manifest, &mut seen, path, line_no, &key, value.trim())?;
    }

    if let Some((key, line_no, _)) = pending {
        return Err(LevelManifestError {
            path: path.to_path_buf(),
            line: Some(line_no),
            field: Some(key),
            message: "unterminated array".to_string(),
        });
    }

    Ok(manifest)
}

fn apply_field(
    manifest: &mut LevelManifest,
    seen: &mut HashSet<String>,
    path: &Path,
    line_no: usize,
    key: &str,
    value: &str,
) -> Result<(), LevelManifestError> {
    if !seen.insert(key.to_string()) {
        return Err(LevelManifestError {
            path: path.to_path_buf(),
            line: Some(line_no),
            field: Some(key.to_string()),
            message: "duplicate field".to_string(),
        });
    }
    match key {
        "version" => {
            let version = parse_u32(value).map_err(|message| LevelManifestError {
                path: path.to_path_buf(),
                line: Some(line_no),
                field: Some(key.to_string()),
                message,
            })?;
            if version != 1 {
                return Err(LevelManifestError {
                    path: path.to_path_buf(),
                    line: Some(line_no),
                    field: Some(key.to_string()),
                    message: format!("unsupported version {}", version),
                });
            }
            manifest.version = version;
            manifest.lines.version = Some(line_no);
        }
        "geometry" => {
            let value = parse_string_value(value).map_err(|message| LevelManifestError {
                path: path.to_path_buf(),
                line: Some(line_no),
                field: Some(key.to_string()),
                message,
            })?;
            let key_value = AssetKey::parse(&value).map_err(|err| LevelManifestError {
                path: path.to_path_buf(),
                line: Some(line_no),
                field: Some(key.to_string()),
                message: err.to_string(),
            })?;
            if key_value.namespace() != "quake1" || key_value.kind() != "bsp" {
                return Err(LevelManifestError {
                    path: path.to_path_buf(),
                    line: Some(line_no),
                    field: Some(key.to_string()),
                    message: "geometry must be quake1:bsp/<map>".to_string(),
                });
            }
            manifest.geometry = Some(key_value);
            manifest.lines.geometry = Some(line_no);
        }
        "assets" => {
            let values = parse_array_strings(value).map_err(|message| LevelManifestError {
                path: path.to_path_buf(),
                line: Some(line_no),
                field: Some(key.to_string()),
                message,
            })?;
            let mut assets = Vec::new();
            for item in values {
                let key_value = AssetKey::parse(&item).map_err(|err| LevelManifestError {
                    path: path.to_path_buf(),
                    line: Some(line_no),
                    field: Some(key.to_string()),
                    message: err.to_string(),
                })?;
                if key_value.namespace() != "engine" {
                    return Err(LevelManifestError {
                        path: path.to_path_buf(),
                        line: Some(line_no),
                        field: Some(key.to_string()),
                        message: format!("asset must be engine namespace: {}", key_value),
                    });
                }
                assets.push(key_value);
            }
            manifest.assets = assets;
            manifest.lines.assets = Some(line_no);
        }
        "requires" => {
            let values = parse_array_strings(value).map_err(|message| LevelManifestError {
                path: path.to_path_buf(),
                line: Some(line_no),
                field: Some(key.to_string()),
                message,
            })?;
            let mut requires = Vec::new();
            for item in values {
                let key_value = AssetKey::parse(&item).map_err(|err| LevelManifestError {
                    path: path.to_path_buf(),
                    line: Some(line_no),
                    field: Some(key.to_string()),
                    message: err.to_string(),
                })?;
                if key_value.namespace() != "engine" {
                    return Err(LevelManifestError {
                        path: path.to_path_buf(),
                        line: Some(line_no),
                        field: Some(key.to_string()),
                        message: format!("dependency must be engine namespace: {}", key_value),
                    });
                }
                requires.push(key_value);
            }
            manifest.requires = requires;
            manifest.lines.requires = Some(line_no);
        }
        _ => {}
    }
    Ok(())
}

fn split_assignment(line: &str) -> Result<(String, String), String> {
    let mut parts = line.splitn(2, '=');
    let key = parts
        .next()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "missing key".to_string())?;
    let value = parts
        .next()
        .map(|text| text.trim().to_string())
        .ok_or_else(|| "missing value".to_string())?;
    if value.is_empty() {
        return Err("missing value".to_string());
    }
    Ok((key, value))
}

fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut in_quotes = false;
    let chars = line.chars().peekable();
    for ch in chars {
        if ch == '"' {
            in_quotes = !in_quotes;
            out.push(ch);
            continue;
        }
        if ch == '#' && !in_quotes {
            break;
        }
        out.push(ch);
    }
    out
}

fn bracket_balance(value: &str) -> Option<i32> {
    let mut depth: i32 = 0;
    let mut in_quotes = false;
    let mut saw_bracket = false;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            in_quotes = !in_quotes;
        }
        if in_quotes {
            if ch == '\\' {
                let _ = chars.next();
            }
            continue;
        }
        if ch == '[' {
            depth += 1;
            saw_bracket = true;
        } else if ch == ']' {
            depth -= 1;
        }
    }
    if saw_bracket {
        Some(depth)
    } else {
        None
    }
}

fn parse_string_value(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.starts_with('"') {
        parse_quoted_string(trimmed)
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_array_strings(value: &str) -> Result<Vec<String>, String> {
    let trimmed = value.trim();
    let mut chars = trimmed.chars().peekable();
    match chars.next() {
        Some('[') => {}
        _ => return Err("array must start with '['".to_string()),
    }
    let mut values = Vec::new();
    loop {
        skip_ws_and_commas(&mut chars);
        match chars.peek() {
            Some(']') => {
                chars.next();
                break;
            }
            Some('"') => {
                let value = parse_quoted_chars(&mut chars)?;
                values.push(value);
            }
            Some(_) => {
                let value = parse_bare_value(&mut chars)?;
                if value.is_empty() {
                    return Err("empty array item".to_string());
                }
                values.push(value);
            }
            None => return Err("unterminated array".to_string()),
        }
        skip_ws_and_commas(&mut chars);
        if let Some(']') = chars.peek() {
            chars.next();
            break;
        }
    }
    Ok(values)
}

fn skip_ws_and_commas(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(ch) = chars.peek() {
        if ch.is_whitespace() || *ch == ',' {
            chars.next();
        } else {
            break;
        }
    }
}

fn parse_quoted_string(value: &str) -> Result<String, String> {
    let mut chars = value.chars().peekable();
    parse_quoted_chars(&mut chars)
}

fn parse_quoted_chars(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<String, String> {
    match chars.next() {
        Some('"') => {}
        _ => return Err("string must start with '\"'".to_string()),
    }
    let mut out = String::new();
    loop {
        let Some(ch) = chars.next() else {
            break;
        };
        match ch {
            '"' => return Ok(out),
            '\\' => {
                if let Some(next) = chars.next() {
                    match next {
                        '\\' => out.push('\\'),
                        '"' => out.push('"'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        _ => {
                            out.push('\\');
                            out.push(next);
                        }
                    }
                } else {
                    return Err("unterminated escape".to_string());
                }
            }
            _ => out.push(ch),
        }
    }
    Err("unterminated string".to_string())
}

fn parse_bare_value(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<String, String> {
    let mut out = String::new();
    while let Some(ch) = chars.peek().copied() {
        if ch == ',' || ch == ']' || ch.is_whitespace() {
            break;
        }
        out.push(ch);
        chars.next();
    }
    Ok(out.trim().to_string())
}

fn parse_u32(value: &str) -> Result<u32, String> {
    value
        .trim()
        .parse()
        .map_err(|_| "invalid integer".to_string())
}
