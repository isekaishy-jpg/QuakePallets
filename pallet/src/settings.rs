use std::fs;
use std::path::PathBuf;

use engine_core::path_policy::user_config_root;

const SETTINGS_VERSION: u32 = 1;
const MIN_UI_SCALE: f32 = 0.75;
const MAX_UI_SCALE: f32 = 2.0;
const DEFAULT_RESOLUTION: [u32; 2] = [1280, 720];
const MIN_RESOLUTION: [u32; 2] = [640, 480];
const MAX_RESOLUTION: [u32; 2] = [7680, 4320];
const DEFAULT_PROFILE_NAME: &str = "config.cfg";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMode {
    Windowed,
    Borderless,
    Fullscreen,
}

impl WindowMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowMode::Windowed => "windowed",
            WindowMode::Borderless => "borderless",
            WindowMode::Fullscreen => "fullscreen",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WindowMode::Windowed => "Windowed",
            WindowMode::Borderless => "Borderless",
            WindowMode::Fullscreen => "Fullscreen",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "windowed" => Some(WindowMode::Windowed),
            "borderless" => Some(WindowMode::Borderless),
            "fullscreen" | "exclusive" => Some(WindowMode::Fullscreen),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub version: u32,
    pub ui_scale: f32,
    pub vsync: bool,
    pub master_volume: f32,
    pub window_mode: WindowMode,
    pub resolution: [u32; 2],
    pub active_profile: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            ui_scale: 1.0,
            vsync: true,
            master_volume: 1.0,
            window_mode: WindowMode::Windowed,
            resolution: DEFAULT_RESOLUTION,
            active_profile: DEFAULT_PROFILE_NAME.to_string(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Some(settings) = Self::parse(&contents) {
                return settings;
            }
        }
        let config_path = config_path();
        if let Ok(contents) = fs::read_to_string(&config_path) {
            if let Some(settings) = Self::parse(&contents) {
                return settings;
            }
        }
        let legacy_path = legacy_config_path();
        if let Ok(contents) = fs::read_to_string(&legacy_path) {
            if let Some(settings) = Self::parse(&contents) {
                return settings;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let data = settings_lines(self);
        write_config_lines(&settings_path(), &data)?;
        merge_config_lines(&config_path_for_profile(&self.active_profile), &data)?;
        Ok(())
    }

    fn parse(contents: &str) -> Option<Self> {
        let mut settings = Self::default();
        let mut version = None;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "version" => {
                    version = value.parse::<u32>().ok();
                }
                "ui_scale" => {
                    if let Ok(value) = value.parse::<f32>() {
                        settings.ui_scale = value;
                    }
                }
                "vsync" => {
                    if let Some(value) = parse_bool(value) {
                        settings.vsync = value;
                    }
                }
                "master_volume" => {
                    if let Ok(value) = value.parse::<f32>() {
                        settings.master_volume = value;
                    }
                }
                "window_mode" => {
                    if let Some(value) = WindowMode::parse(value) {
                        settings.window_mode = value;
                    }
                }
                "resolution" => {
                    if let Some(value) = parse_resolution(value) {
                        settings.resolution = value;
                    }
                }
                "active_profile" => {
                    if let Some(profile) = sanitize_profile_name(value) {
                        settings.active_profile = profile;
                    }
                }
                _ => {}
            }
        }
        if let Some(version) = version {
            if version != SETTINGS_VERSION {
                return Some(Self::default());
            }
        }
        settings.normalize();
        settings.version = SETTINGS_VERSION;
        Some(settings)
    }

    pub fn normalize(&mut self) {
        self.ui_scale = self.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE);
        self.master_volume = self.master_volume.clamp(0.0, 1.0);
        self.resolution = clamp_resolution(self.resolution);
        self.active_profile = sanitize_profile_name(&self.active_profile)
            .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_string());
    }
}

fn settings_path() -> PathBuf {
    user_config_root().join("settings.cfg")
}

fn config_path() -> PathBuf {
    config_path_for_profile(DEFAULT_PROFILE_NAME)
}

fn legacy_config_path() -> PathBuf {
    user_config_root().join("config.cfg")
}

pub fn default_profile_name() -> &'static str {
    DEFAULT_PROFILE_NAME
}

pub fn config_path_for_profile(profile: &str) -> PathBuf {
    user_config_root()
        .join("config")
        .join("cvars")
        .join(profile)
}

pub fn settings_lines(settings: &Settings) -> Vec<String> {
    vec![
        format!("version={}", SETTINGS_VERSION),
        format!("ui_scale={:.3}", settings.ui_scale),
        format!("vsync={}", format_bool(settings.vsync)),
        format!("master_volume={:.3}", settings.master_volume),
        format!("window_mode={}", settings.window_mode.as_str()),
        format!(
            "resolution={}x{}",
            settings.resolution[0], settings.resolution[1]
        ),
        format!("active_profile={}", settings.active_profile),
    ]
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn format_bool(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn sanitize_profile_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains(':') {
        return None;
    }
    let name = if trimmed.to_ascii_lowercase().ends_with(".cfg") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.cfg")
    };
    Some(name)
}

pub fn write_config_lines(path: &PathBuf, lines: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut data = lines.join("\n");
    data.push('\n');
    fs::write(path, data)
}

pub fn merge_config_lines(path: &PathBuf, settings_lines: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut merged = Vec::new();
    let contents = fs::read_to_string(path).unwrap_or_default();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            merged.push(line.to_string());
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            merged.push(line.to_string());
            continue;
        };
        let key = key.trim();
        if let Some(replacement) = settings_lines
            .iter()
            .find(|candidate| candidate.starts_with(&format!("{key}=")))
        {
            if seen.insert(key.to_string()) {
                merged.push(replacement.clone());
            }
        } else {
            merged.push(line.to_string());
        }
    }
    for line in settings_lines {
        let key = line.split('=').next().unwrap_or("").trim();
        if !key.is_empty() && seen.insert(key.to_string()) {
            merged.push(line.clone());
        }
    }
    write_config_lines(path, &merged)
}

pub fn parse_resolution(value: &str) -> Option<[u32; 2]> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once(','))
        .or_else(|| value.split_once('X'))?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some([width, height])
}

fn clamp_resolution(resolution: [u32; 2]) -> [u32; 2] {
    let width = resolution[0].clamp(MIN_RESOLUTION[0], MAX_RESOLUTION[0]);
    let height = resolution[1].clamp(MIN_RESOLUTION[1], MAX_RESOLUTION[1]);
    [width, height]
}
