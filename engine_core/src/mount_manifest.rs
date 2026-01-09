use std::path::{Path, PathBuf};

use crate::vfs::MountKind;

#[derive(Clone, Debug)]
pub struct MountManifestEntry {
    pub kind: MountKind,
    pub mount_point: String,
    pub path: PathBuf,
    pub line: usize,
}

pub fn load_mount_manifest(path: &Path) -> Result<Vec<MountManifestEntry>, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("mount manifest read failed ({}): {}", path.display(), err))?;
    parse_mount_manifest(&contents)
}

pub fn parse_mount_manifest(contents: &str) -> Result<Vec<MountManifestEntry>, String> {
    let mut entries = Vec::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let tokens = tokenize_line(line)
            .map_err(|err| format!("mount manifest line {} parse failed: {}", line_no, err))?;
        if tokens.is_empty() {
            continue;
        }
        if tokens.len() < 3 {
            return Err(format!(
                "mount manifest line {} expects: <dir|pak|pk3> <vroot> <path>",
                line_no
            ));
        }
        let kind = parse_mount_kind(&tokens[0]).ok_or_else(|| {
            format!(
                "mount manifest line {} unknown mount kind: {}",
                line_no, tokens[0]
            )
        })?;
        let mount_point = tokens[1].clone();
        let path = PathBuf::from(tokens[2..].join(" "));
        entries.push(MountManifestEntry {
            kind,
            mount_point,
            path,
            line: line_no,
        });
    }
    Ok(entries)
}

fn tokenize_line(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        if ch == '#' && !in_quotes {
            break;
        }
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if in_quotes {
        return Err("unterminated quote".to_string());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn parse_mount_kind(value: &str) -> Option<MountKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dir" | "directory" => Some(MountKind::Dir),
        "pak" => Some(MountKind::Pak),
        "pk3" => Some(MountKind::Pk3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest_lines() {
        let input = r#"
# comment
dir raw/quake "C:\Quake\id1"
pak raw/quake C:\Quake\id1\pak0.pak
pk3 raw/q3 "D:\Quake3\baseq3\pak0.pk3"
"#;
        let entries = parse_mount_manifest(input).expect("parse ok");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, MountKind::Dir);
        assert_eq!(entries[1].kind, MountKind::Pak);
        assert_eq!(entries[2].kind, MountKind::Pk3);
        assert_eq!(entries[0].mount_point, "raw/quake");
    }
}
