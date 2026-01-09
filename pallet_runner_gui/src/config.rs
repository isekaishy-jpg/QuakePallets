use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const CONFIG_VERSION: u32 = 1;
const DEFAULT_QUAKE_DIR: &str =
    "C:\\Program Files (x86)\\Steam\\steamapps\\common\\Quake\\rerelease";
const DEFAULT_MAP: &str = "e1m1";
const DEFAULT_DEBUG_PRESET: &str = "Default";
const DEFAULT_SMOKE_MODE: &str = "no-assets";
const DEFAULT_NET_BIND: &str = "0.0.0.0:40000";
const DEFAULT_CLIENT_BIND: &str = "0.0.0.0:0";
const DEFAULT_CLIENT_SERVER: &str = "127.0.0.1:40000";

#[derive(Clone, Debug)]
pub struct DebugPresetConfig {
    pub name: String,
    pub description: String,
    pub extra_args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct RunnerConfig {
    pub version: u32,
    pub repo_root: Option<String>,
    pub quake_dir: String,
    pub map: String,
    pub playlist_enabled: bool,
    pub playlist_path: Option<String>,
    pub debug_preset: String,
    pub debug_presets: Vec<DebugPresetConfig>,
    pub video_debug: bool,
    pub show_image: Option<String>,
    pub play_movie: Option<String>,
    pub script_path: Option<String>,
    pub mount_manifest: Option<String>,
    pub pallet_mount_dir_vroot: Option<String>,
    pub pallet_mount_dir_path: Option<String>,
    pub pallet_mount_pak_vroot: Option<String>,
    pub pallet_mount_pak_path: Option<String>,
    pub pallet_mount_pk3_vroot: Option<String>,
    pub pallet_mount_pk3_path: Option<String>,
    pub input_script: bool,
    pub smoke_mode: String,
    pub smoke_ticks: Option<u32>,
    pub smoke_headless: bool,
    pub pak_out_dir: Option<String>,
    pub vfs_vpath: String,
    pub vfs_mount_manifest: Option<String>,
    pub vfs_use_quake_dir: bool,
    pub vfs_mount_dir_vroot: Option<String>,
    pub vfs_mount_dir_path: Option<String>,
    pub vfs_mount_pak_vroot: Option<String>,
    pub vfs_mount_pak_path: Option<String>,
    pub vfs_mount_pk3_vroot: Option<String>,
    pub vfs_mount_pk3_path: Option<String>,
    pub server_bind: String,
    pub server_tick_ms: u64,
    pub server_snapshot_stride: u32,
    pub server_max_ticks: Option<u64>,
    pub client_bind: String,
    pub client_server: String,
    pub client_tick_ms: u64,
    pub client_ticks: u64,
    pub client_id: u32,
    pub client_move_enabled: bool,
    pub client_move_x: f32,
    pub client_move_y: f32,
    pub client_yaw_step: f32,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            repo_root: None,
            quake_dir: DEFAULT_QUAKE_DIR.to_string(),
            map: DEFAULT_MAP.to_string(),
            playlist_enabled: false,
            playlist_path: None,
            debug_preset: DEFAULT_DEBUG_PRESET.to_string(),
            debug_presets: default_debug_presets(),
            video_debug: false,
            show_image: None,
            play_movie: None,
            script_path: None,
            mount_manifest: None,
            pallet_mount_dir_vroot: None,
            pallet_mount_dir_path: None,
            pallet_mount_pak_vroot: None,
            pallet_mount_pak_path: None,
            pallet_mount_pk3_vroot: None,
            pallet_mount_pk3_path: None,
            input_script: false,
            smoke_mode: DEFAULT_SMOKE_MODE.to_string(),
            smoke_ticks: None,
            smoke_headless: false,
            pak_out_dir: None,
            vfs_vpath: String::new(),
            vfs_mount_manifest: None,
            vfs_use_quake_dir: false,
            vfs_mount_dir_vroot: None,
            vfs_mount_dir_path: None,
            vfs_mount_pak_vroot: None,
            vfs_mount_pak_path: None,
            vfs_mount_pk3_vroot: None,
            vfs_mount_pk3_path: None,
            server_bind: DEFAULT_NET_BIND.to_string(),
            server_tick_ms: 16,
            server_snapshot_stride: 1,
            server_max_ticks: None,
            client_bind: DEFAULT_CLIENT_BIND.to_string(),
            client_server: DEFAULT_CLIENT_SERVER.to_string(),
            client_tick_ms: 16,
            client_ticks: 120,
            client_id: 1,
            client_move_enabled: false,
            client_move_x: 0.0,
            client_move_y: 1.0,
            client_yaw_step: 0.02,
        }
    }
}

impl RunnerConfig {
    pub fn load() -> Self {
        let path = config_path();
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut config = parse_config(&contents).unwrap_or_default();
        config.ensure_debug_presets();
        config
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = self.to_json();
        fs::write(path, data)
    }

    pub fn ensure_debug_presets(&mut self) {
        if self.debug_presets.is_empty() {
            self.debug_presets = default_debug_presets();
        }
        if !self
            .debug_presets
            .iter()
            .any(|preset| preset.name == self.debug_preset)
        {
            if let Some(preset) = self.debug_presets.first() {
                self.debug_preset = preset.name.clone();
            }
        }
    }

    fn to_json(&self) -> String {
        let mut body = String::new();
        body.push_str("{\n");
        push_number(&mut body, "version", self.version, true);
        push_opt_string(&mut body, "repo_root", self.repo_root.as_deref(), true);
        push_string(&mut body, "quake_dir", &self.quake_dir, true);
        push_string(&mut body, "map", &self.map, true);
        push_bool(&mut body, "playlist_enabled", self.playlist_enabled, true);
        push_opt_string(
            &mut body,
            "playlist_path",
            self.playlist_path.as_deref(),
            true,
        );
        push_string(&mut body, "debug_preset", &self.debug_preset, true);
        push_debug_presets(&mut body, &self.debug_presets, true);
        push_bool(&mut body, "video_debug", self.video_debug, true);
        push_opt_string(&mut body, "show_image", self.show_image.as_deref(), true);
        push_opt_string(&mut body, "play_movie", self.play_movie.as_deref(), true);
        push_opt_string(&mut body, "script_path", self.script_path.as_deref(), true);
        push_opt_string(
            &mut body,
            "mount_manifest",
            self.mount_manifest.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "pallet_mount_dir_vroot",
            self.pallet_mount_dir_vroot.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "pallet_mount_dir_path",
            self.pallet_mount_dir_path.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "pallet_mount_pak_vroot",
            self.pallet_mount_pak_vroot.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "pallet_mount_pak_path",
            self.pallet_mount_pak_path.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "pallet_mount_pk3_vroot",
            self.pallet_mount_pk3_vroot.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "pallet_mount_pk3_path",
            self.pallet_mount_pk3_path.as_deref(),
            true,
        );
        push_bool(&mut body, "input_script", self.input_script, true);
        push_string(&mut body, "smoke_mode", &self.smoke_mode, true);
        push_opt_number(&mut body, "smoke_ticks", self.smoke_ticks, true);
        push_bool(&mut body, "smoke_headless", self.smoke_headless, true);
        push_opt_string(&mut body, "pak_out_dir", self.pak_out_dir.as_deref(), true);
        push_string(&mut body, "vfs_vpath", &self.vfs_vpath, true);
        push_opt_string(
            &mut body,
            "vfs_mount_manifest",
            self.vfs_mount_manifest.as_deref(),
            true,
        );
        push_bool(&mut body, "vfs_use_quake_dir", self.vfs_use_quake_dir, true);
        push_opt_string(
            &mut body,
            "vfs_mount_dir_vroot",
            self.vfs_mount_dir_vroot.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "vfs_mount_dir_path",
            self.vfs_mount_dir_path.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "vfs_mount_pak_vroot",
            self.vfs_mount_pak_vroot.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "vfs_mount_pak_path",
            self.vfs_mount_pak_path.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "vfs_mount_pk3_vroot",
            self.vfs_mount_pk3_vroot.as_deref(),
            true,
        );
        push_opt_string(
            &mut body,
            "vfs_mount_pk3_path",
            self.vfs_mount_pk3_path.as_deref(),
            true,
        );
        push_string(&mut body, "server_bind", &self.server_bind, true);
        push_number(&mut body, "server_tick_ms", self.server_tick_ms, true);
        push_number(
            &mut body,
            "server_snapshot_stride",
            self.server_snapshot_stride,
            true,
        );
        push_opt_number(&mut body, "server_max_ticks", self.server_max_ticks, true);
        push_string(&mut body, "client_bind", &self.client_bind, true);
        push_string(&mut body, "client_server", &self.client_server, true);
        push_number(&mut body, "client_tick_ms", self.client_tick_ms, true);
        push_number(&mut body, "client_ticks", self.client_ticks, true);
        push_number(&mut body, "client_id", self.client_id, true);
        push_bool(
            &mut body,
            "client_move_enabled",
            self.client_move_enabled,
            true,
        );
        push_float(&mut body, "client_move_x", self.client_move_x, true);
        push_float(&mut body, "client_move_y", self.client_move_y, true);
        push_float(&mut body, "client_yaw_step", self.client_yaw_step, false);
        body.push_str("}\n");
        body
    }
}

fn config_path() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata)
            .join("Pallet")
            .join("runner_gui.json");
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config).join("pallet").join("runner_gui.json");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("pallet")
            .join("runner_gui.json");
    }
    PathBuf::from("runner_gui.json")
}

fn parse_config(contents: &str) -> Option<RunnerConfig> {
    let mut config = RunnerConfig::default();
    if let Some(version) = parse_json_u32(contents, "version") {
        if version != CONFIG_VERSION {
            return Some(RunnerConfig::default());
        }
        config.version = version;
    }
    if let Some(value) = parse_json_optional_string(contents, "repo_root") {
        config.repo_root = value;
    }
    if let Some(value) = parse_json_string(contents, "quake_dir") {
        config.quake_dir = value;
    }
    if let Some(value) = parse_json_string(contents, "map") {
        config.map = value;
    }
    if let Some(value) = parse_json_bool(contents, "playlist_enabled") {
        config.playlist_enabled = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "playlist_path") {
        config.playlist_path = value;
    }
    if let Some(value) = parse_json_string(contents, "debug_preset") {
        config.debug_preset = value;
    }
    if let Some(value) = parse_debug_presets(contents) {
        config.debug_presets = value;
    }
    if let Some(value) = parse_json_bool(contents, "video_debug") {
        config.video_debug = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "show_image") {
        config.show_image = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "play_movie") {
        config.play_movie = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "script_path") {
        config.script_path = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "mount_manifest") {
        config.mount_manifest = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pallet_mount_dir_vroot") {
        config.pallet_mount_dir_vroot = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pallet_mount_dir_path") {
        config.pallet_mount_dir_path = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pallet_mount_pak_vroot") {
        config.pallet_mount_pak_vroot = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pallet_mount_pak_path") {
        config.pallet_mount_pak_path = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pallet_mount_pk3_vroot") {
        config.pallet_mount_pk3_vroot = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pallet_mount_pk3_path") {
        config.pallet_mount_pk3_path = value;
    }
    if let Some(value) = parse_json_bool(contents, "input_script") {
        config.input_script = value;
    }
    if let Some(value) = parse_json_string(contents, "smoke_mode") {
        config.smoke_mode = value;
    }
    if let Some(value) = parse_json_optional_u32(contents, "smoke_ticks") {
        config.smoke_ticks = value;
    }
    if let Some(value) = parse_json_bool(contents, "smoke_headless") {
        config.smoke_headless = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "pak_out_dir") {
        config.pak_out_dir = value;
    }
    if let Some(value) = parse_json_string(contents, "vfs_vpath") {
        config.vfs_vpath = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_manifest") {
        config.vfs_mount_manifest = value;
    }
    if let Some(value) = parse_json_bool(contents, "vfs_use_quake_dir") {
        config.vfs_use_quake_dir = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_dir_vroot") {
        config.vfs_mount_dir_vroot = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_dir_path") {
        config.vfs_mount_dir_path = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_pak_vroot") {
        config.vfs_mount_pak_vroot = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_pak_path") {
        config.vfs_mount_pak_path = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_pk3_vroot") {
        config.vfs_mount_pk3_vroot = value;
    }
    if let Some(value) = parse_json_optional_string(contents, "vfs_mount_pk3_path") {
        config.vfs_mount_pk3_path = value;
    }
    if let Some(value) = parse_json_string(contents, "server_bind") {
        config.server_bind = value;
    }
    if let Some(value) = parse_json_u64(contents, "server_tick_ms") {
        config.server_tick_ms = value.max(1);
    }
    if let Some(value) = parse_json_u32(contents, "server_snapshot_stride") {
        config.server_snapshot_stride = value.max(1);
    }
    if let Some(value) = parse_json_optional_u64(contents, "server_max_ticks") {
        config.server_max_ticks = value;
    }
    if let Some(value) = parse_json_string(contents, "client_bind") {
        config.client_bind = value;
    }
    if let Some(value) = parse_json_string(contents, "client_server") {
        config.client_server = value;
    }
    if let Some(value) = parse_json_u64(contents, "client_tick_ms") {
        config.client_tick_ms = value.max(1);
    }
    if let Some(value) = parse_json_u64(contents, "client_ticks") {
        config.client_ticks = value.max(1);
    }
    if let Some(value) = parse_json_u32(contents, "client_id") {
        config.client_id = value.max(1);
    }
    if let Some(value) = parse_json_bool(contents, "client_move_enabled") {
        config.client_move_enabled = value;
    }
    if let Some(value) = parse_json_f32(contents, "client_move_x") {
        config.client_move_x = value;
    }
    if let Some(value) = parse_json_f32(contents, "client_move_y") {
        config.client_move_y = value;
    }
    if let Some(value) = parse_json_f32(contents, "client_yaw_step") {
        config.client_yaw_step = value;
    }
    config.ensure_debug_presets();
    Some(config)
}

fn default_debug_presets() -> Vec<DebugPresetConfig> {
    vec![
        DebugPresetConfig {
            name: "Default".to_string(),
            description: "No extra args or env vars.".to_string(),
            extra_args: Vec::new(),
            env: BTreeMap::new(),
        },
        DebugPresetConfig {
            name: "Video Debug".to_string(),
            description: "Enable video/audio debug stats.".to_string(),
            extra_args: Vec::new(),
            env: [("CRUSTQUAKE_VIDEO_DEBUG".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
        },
        DebugPresetConfig {
            name: "Intro Playlist + E1M1".to_string(),
            description: "Play the intro playlist then load e1m1.".to_string(),
            extra_args: vec![
                "--playlist".to_string(),
                "movies_playlist.txt".to_string(),
                "--map".to_string(),
                "e1m1".to_string(),
            ],
            env: BTreeMap::new(),
        },
    ]
}

fn parse_debug_presets(contents: &str) -> Option<Vec<DebugPresetConfig>> {
    let value = find_json_value(contents, "debug_presets")?;
    parse_debug_presets_value(value)
}

fn parse_debug_presets_value(value: &str) -> Option<Vec<DebugPresetConfig>> {
    let value = value.trim_start();
    if !value.starts_with('[') {
        return None;
    }
    let bytes = value.as_bytes();
    let mut idx = 1;
    let mut presets = Vec::new();
    while idx < bytes.len() {
        idx = skip_whitespace(bytes, idx);
        if idx >= bytes.len() {
            return None;
        }
        match bytes[idx] {
            b']' => return Some(presets),
            b',' => {
                idx += 1;
                continue;
            }
            b'{' => {
                let slice = &value[idx..];
                let (object, consumed) = parse_json_object_token(slice)?;
                let preset = parse_debug_preset_object(&object)?;
                presets.push(preset);
                idx += consumed;
            }
            _ => return None,
        }
    }
    None
}

fn parse_debug_preset_object(contents: &str) -> Option<DebugPresetConfig> {
    let name = parse_json_string_value(find_json_value(contents, "name")?)?;
    let description = parse_json_string_value(find_json_value(contents, "description")?)?;
    let extra_args = parse_json_string_array(find_json_value(contents, "extra_args")?)?;
    let env = parse_json_string_map(find_json_value(contents, "env")?)?;
    Some(DebugPresetConfig {
        name,
        description,
        extra_args,
        env,
    })
}

fn parse_json_optional_string(contents: &str, key: &str) -> Option<Option<String>> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    if token.starts_with("null") {
        return Some(None);
    }
    parse_json_string_value(value).map(Some)
}

fn parse_json_string(contents: &str, key: &str) -> Option<String> {
    let value = find_json_value(contents, key)?;
    parse_json_string_value(value)
}

fn parse_json_string_value(value: &str) -> Option<String> {
    let value = value.trim_start();
    if !value.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for ch in value[1..].chars() {
        if escaped {
            match ch {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                _ => out.push(ch),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            _ => out.push(ch),
        }
    }
    None
}

fn parse_json_bool(contents: &str, key: &str) -> Option<bool> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    match token {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_json_u32(contents: &str, key: &str) -> Option<u32> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    token.parse::<u32>().ok()
}

fn parse_json_u64(contents: &str, key: &str) -> Option<u64> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    token.parse::<u64>().ok()
}

fn parse_json_optional_u32(contents: &str, key: &str) -> Option<Option<u32>> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    if token == "null" {
        return Some(None);
    }
    token.parse::<u32>().ok().map(Some)
}

fn parse_json_optional_u64(contents: &str, key: &str) -> Option<Option<u64>> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    if token == "null" {
        return Some(None);
    }
    token.parse::<u64>().ok().map(Some)
}

fn parse_json_f32(contents: &str, key: &str) -> Option<f32> {
    let value = find_json_value(contents, key)?;
    let token = parse_json_token(value)?;
    token.parse::<f32>().ok()
}

fn find_json_value<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    let start = contents.find(&needle)?;
    let rest = &contents[start + needle.len()..];
    let colon = rest.find(':')?;
    Some(rest[colon + 1..].trim_start())
}

fn parse_json_token(value: &str) -> Option<&str> {
    let token = value
        .split(|ch| [',', '}', '\n', '\r'].contains(&ch))
        .next()?
        .trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn parse_json_string_token(value: &str) -> Option<(String, usize)> {
    let value = value.strip_prefix('"')?;
    let mut out = String::new();
    let mut escaped = false;
    for (offset, ch) in value.char_indices() {
        if escaped {
            match ch {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                _ => out.push(ch),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some((out, offset + 2)),
            _ => out.push(ch),
        }
    }
    None
}

fn parse_json_string_array(value: &str) -> Option<Vec<String>> {
    let value = value.trim_start();
    if !value.starts_with('[') {
        return None;
    }
    let bytes = value.as_bytes();
    let mut idx = 1;
    let mut items = Vec::new();
    while idx < bytes.len() {
        idx = skip_whitespace(bytes, idx);
        if idx >= bytes.len() {
            return None;
        }
        match bytes[idx] {
            b']' => return Some(items),
            b',' => idx += 1,
            b'"' => {
                let (item, consumed) = parse_json_string_token(&value[idx..])?;
                items.push(item);
                idx += consumed;
            }
            _ => return None,
        }
    }
    None
}

fn parse_json_string_map(value: &str) -> Option<BTreeMap<String, String>> {
    let value = value.trim_start();
    if !value.starts_with('{') {
        return None;
    }
    let bytes = value.as_bytes();
    let mut idx = 1;
    let mut map = BTreeMap::new();
    while idx < bytes.len() {
        idx = skip_whitespace(bytes, idx);
        if idx >= bytes.len() {
            return None;
        }
        match bytes[idx] {
            b'}' => return Some(map),
            b',' => idx += 1,
            b'"' => {
                let (key, consumed) = parse_json_string_token(&value[idx..])?;
                idx += consumed;
                idx = skip_whitespace(bytes, idx);
                if bytes.get(idx)? != &b':' {
                    return None;
                }
                idx += 1;
                idx = skip_whitespace(bytes, idx);
                let (val, consumed) = parse_json_string_token(&value[idx..])?;
                idx += consumed;
                map.insert(key, val);
            }
            _ => return None,
        }
    }
    None
}

fn parse_json_object_token(value: &str) -> Option<(String, usize)> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = idx + 1;
                    return Some((value[..end].to_string(), end));
                }
            }
            _ => {}
        }
    }
    None
}

fn skip_whitespace(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    idx
}

fn push_string(output: &mut String, key: &str, value: &str, comma: bool) {
    let escaped = json_escape(value);
    output.push_str(&format!("  \"{}\": \"{}\"", key, escaped));
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_opt_string(output: &mut String, key: &str, value: Option<&str>, comma: bool) {
    output.push_str(&format!("  \"{}\": ", key));
    match value {
        Some(value) => {
            let escaped = json_escape(value);
            output.push('"');
            output.push_str(&escaped);
            output.push('"');
        }
        None => output.push_str("null"),
    }
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_bool(output: &mut String, key: &str, value: bool, comma: bool) {
    output.push_str(&format!("  \"{}\": {}", key, value));
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_number<T: std::fmt::Display>(output: &mut String, key: &str, value: T, comma: bool) {
    output.push_str(&format!("  \"{}\": {}", key, value));
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_opt_number<T: std::fmt::Display>(
    output: &mut String,
    key: &str,
    value: Option<T>,
    comma: bool,
) {
    output.push_str(&format!("  \"{}\": ", key));
    match value {
        Some(value) => output.push_str(&format!("{}", value)),
        None => output.push_str("null"),
    }
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_float(output: &mut String, key: &str, value: f32, comma: bool) {
    output.push_str(&format!("  \"{}\": {}", key, format_float(value)));
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_debug_presets(output: &mut String, presets: &[DebugPresetConfig], comma: bool) {
    output.push_str("  \"debug_presets\": [\n");
    for (index, preset) in presets.iter().enumerate() {
        output.push_str("    {\n");
        push_string_indented(output, "name", &preset.name, true, "      ");
        push_string_indented(output, "description", &preset.description, true, "      ");
        push_string_array_indented(output, "extra_args", &preset.extra_args, true, "      ");
        push_env_map_indented(output, "env", &preset.env, false, "      ");
        output.push_str("    }");
        if index + 1 < presets.len() {
            output.push(',');
        }
        output.push('\n');
    }
    output.push_str("  ]");
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_string_indented(output: &mut String, key: &str, value: &str, comma: bool, indent: &str) {
    let escaped = json_escape(value);
    output.push_str(&format!("{indent}\"{}\": \"{}\"", key, escaped));
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_string_array_indented(
    output: &mut String,
    key: &str,
    values: &[String],
    comma: bool,
    indent: &str,
) {
    output.push_str(&format!("{indent}\"{}\": [", key));
    if values.is_empty() {
        output.push(']');
    } else {
        output.push('\n');
        for (index, value) in values.iter().enumerate() {
            output.push_str(indent);
            output.push_str("  \"");
            output.push_str(&json_escape(value));
            output.push('"');
            if index + 1 < values.len() {
                output.push(',');
            }
            output.push('\n');
        }
        output.push_str(indent);
        output.push(']');
    }
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn push_env_map_indented(
    output: &mut String,
    key: &str,
    env: &BTreeMap<String, String>,
    comma: bool,
    indent: &str,
) {
    output.push_str(&format!("{indent}\"{}\": {{", key));
    if env.is_empty() {
        output.push('}');
    } else {
        output.push('\n');
        for (index, (env_key, env_value)) in env.iter().enumerate() {
            output.push_str(indent);
            output.push_str("  \"");
            output.push_str(&json_escape(env_key));
            output.push_str("\": \"");
            output.push_str(&json_escape(env_value));
            output.push('"');
            if index + 1 < env.len() {
                output.push(',');
            }
            output.push('\n');
        }
        output.push_str(indent);
        output.push('}');
    }
    if comma {
        output.push(',');
    }
    output.push('\n');
}

fn format_float(value: f32) -> String {
    let mut text = format!("{:.4}", value);
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text.is_empty() {
        text.push('0');
    }
    text
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
