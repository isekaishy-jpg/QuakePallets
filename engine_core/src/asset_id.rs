use std::fmt;
use std::hash::{Hash, Hasher};

pub const MAX_ASSET_ID_LEN: usize = 512;

#[derive(Clone, Debug)]
pub struct AssetKey {
    namespace: String,
    kind: String,
    path: String,
    canonical: String,
    hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetKeyError {
    message: String,
}

impl AssetKeyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AssetKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AssetKeyError {}

impl AssetKey {
    pub fn parse(input: &str) -> Result<Self, AssetKeyError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(AssetKeyError::new("asset id is empty"));
        }
        if trimmed.contains('\\') {
            return Err(AssetKeyError::new("asset id must use '/' separators"));
        }
        let (namespace_raw, remainder) = trimmed
            .split_once(':')
            .ok_or_else(|| AssetKeyError::new("asset id must include namespace:kind/path"))?;
        let (kind_raw, path_raw) = remainder
            .split_once('/')
            .ok_or_else(|| AssetKeyError::new("asset id must include kind/path"))?;
        AssetKey::from_parts(namespace_raw, kind_raw, path_raw)
    }

    pub fn from_parts(namespace: &str, kind: &str, path: &str) -> Result<Self, AssetKeyError> {
        if namespace.trim().is_empty() || kind.trim().is_empty() || path.trim().is_empty() {
            return Err(AssetKeyError::new("asset id contains empty segment"));
        }

        let namespace = namespace.trim().to_ascii_lowercase();
        let kind = kind.trim().to_ascii_lowercase();
        let path = path.trim().to_ascii_lowercase();

        if !is_valid_namespace(&namespace) {
            return Err(AssetKeyError::new(format!(
                "invalid namespace '{}'",
                namespace
            )));
        }
        if !is_valid_kind(&kind) {
            return Err(AssetKeyError::new(format!("invalid kind '{}'", kind)));
        }
        if !is_valid_path(&path) {
            return Err(AssetKeyError::new(format!("invalid path '{}'", path)));
        }
        if !is_known_namespace(&namespace) {
            return Err(AssetKeyError::new(format!(
                "unknown namespace '{}'",
                namespace
            )));
        }
        if !is_known_kind(&namespace, &kind) {
            return Err(AssetKeyError::new(format!(
                "unknown kind '{}' for namespace '{}'",
                kind, namespace
            )));
        }

        let canonical = format!("{}:{}/{}", namespace, kind, path);
        if canonical.len() > MAX_ASSET_ID_LEN {
            return Err(AssetKeyError::new(format!(
                "asset id length {} exceeds max {}",
                canonical.len(),
                MAX_ASSET_ID_LEN
            )));
        }
        let hash = fnv1a64(canonical.as_bytes());
        Ok(Self {
            namespace,
            kind,
            path,
            canonical,
            hash,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    pub fn hash64(&self) -> u64 {
        self.hash
    }
}

impl PartialEq for AssetKey {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl Eq for AssetKey {}

impl Hash for AssetKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl fmt::Display for AssetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.canonical)
    }
}

pub struct EngineTextureId(AssetKey);
pub struct EngineConfigId(AssetKey);
pub struct EngineScriptId(AssetKey);
pub struct EngineLevelId(AssetKey);
pub struct EngineCollisionWorldId(AssetKey);
pub struct EngineTestMapId(AssetKey);
pub struct Quake1RawId(AssetKey);

impl EngineTextureId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("engine", "texture", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

impl EngineConfigId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("engine", "config", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

impl EngineScriptId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("engine", "script", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

impl EngineLevelId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("engine", "level", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

impl EngineCollisionWorldId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("engine", "collision_world", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

impl EngineTestMapId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("engine", "test_map", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

impl Quake1RawId {
    pub fn new(path: &str) -> Result<Self, AssetKeyError> {
        AssetKey::from_parts("quake1", "raw", path).map(Self)
    }

    pub fn key(&self) -> &AssetKey {
        &self.0
    }
}

const ENGINE_KINDS: [&str; 8] = [
    "blob",
    "collision_world",
    "config",
    "level",
    "script",
    "test_map",
    "text",
    "texture",
];
const QUAKE1_KINDS: [&str; 8] = [
    "bsp",
    "cfg",
    "model",
    "raw",
    "raw_other",
    "sound",
    "texture",
    "wad",
];
const QUAKELIVE_KINDS: [&str; 11] = [
    "bsp",
    "cfg",
    "font",
    "model",
    "raw",
    "raw_other",
    "script",
    "shader",
    "sound",
    "texture",
    "ui",
];
const KNOWN_NAMESPACES: [&str; 3] = ["engine", "quake1", "quakelive"];

fn is_known_namespace(namespace: &str) -> bool {
    KNOWN_NAMESPACES.contains(&namespace)
}

fn is_known_kind(namespace: &str, kind: &str) -> bool {
    match namespace {
        "engine" => ENGINE_KINDS.contains(&kind),
        "quake1" => QUAKE1_KINDS.contains(&kind),
        "quakelive" => QUAKELIVE_KINDS.contains(&kind),
        _ => false,
    }
}

fn is_valid_namespace(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

fn is_valid_kind(value: &str) -> bool {
    is_valid_namespace(value)
}

fn is_valid_path(value: &str) -> bool {
    for segment in value.split('/') {
        if segment.is_empty() {
            return false;
        }
        if segment == ".." {
            return false;
        }
    }
    value.chars().all(|ch| {
        ch.is_ascii_lowercase()
            || ch.is_ascii_digit()
            || ch == '/'
            || ch == '_'
            || ch == '-'
            || ch == '.'
    })
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical_key() {
        let key = AssetKey::parse("engine:texture/ui/console_bg").unwrap();
        assert_eq!(key.namespace(), "engine");
        assert_eq!(key.kind(), "texture");
        assert_eq!(key.path(), "ui/console_bg");
        assert_eq!(key.canonical(), "engine:texture/ui/console_bg");
    }

    #[test]
    fn parse_lowercases_input() {
        let key = AssetKey::parse("Engine:Texture/UI/Console_BG").unwrap();
        assert_eq!(key.canonical(), "engine:texture/ui/console_bg");
    }

    #[test]
    fn parse_rejects_unknown_namespace() {
        let err = AssetKey::parse("mod:texture/ui/test").unwrap_err();
        assert!(err.to_string().contains("unknown namespace"));
    }

    #[test]
    fn parse_accepts_quakelive_raw() {
        let key = AssetKey::parse("quakelive:raw/scripts/ui.shader").unwrap();
        assert_eq!(key.canonical(), "quakelive:raw/scripts/ui.shader");
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        let err = AssetKey::parse("engine:unknown/ui/test").unwrap_err();
        assert!(err.to_string().contains("unknown kind"));
    }

    #[test]
    fn parse_rejects_double_slash() {
        let err = AssetKey::parse("engine:texture/ui//bg").unwrap_err();
        assert!(err.to_string().contains("invalid path"));
    }

    #[test]
    fn parse_rejects_dot_dot_segment() {
        let err = AssetKey::parse("engine:texture/ui/../bg").unwrap_err();
        assert!(err.to_string().contains("invalid path"));
    }

    #[test]
    fn roundtrip_display() {
        let key = AssetKey::parse("quake1:raw/gfx/conback.lmp").unwrap();
        assert_eq!(key.to_string(), "quake1:raw/gfx/conback.lmp");
    }

    #[test]
    fn engine_texture_id_builds_key() {
        let id = EngineTextureId::new("ui/console_bg").unwrap();
        assert_eq!(id.key().canonical(), "engine:texture/ui/console_bg");
    }

    #[test]
    fn engine_collision_world_id_builds_key() {
        let id = EngineCollisionWorldId::new("fixtures/arena.toml").unwrap();
        assert_eq!(
            id.key().canonical(),
            "engine:collision_world/fixtures/arena.toml"
        );
    }

    #[test]
    fn rejects_overlong_ids() {
        let long_path = "a".repeat(MAX_ASSET_ID_LEN);
        let err = AssetKey::from_parts("engine", "text", &long_path).unwrap_err();
        assert!(err.to_string().contains("exceeds max"));
    }
}
