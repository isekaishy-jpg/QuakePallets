use std::fs;
use std::path::PathBuf;

use engine_core::path_policy::user_config_root;

const SETTINGS_VERSION: u32 = 1;
const MIN_UI_SCALE: f32 = 0.75;
const MAX_UI_SCALE: f32 = 2.0;
const DEFAULT_RESOLUTION: [u32; 2] = [1280, 720];
const MIN_RESOLUTION: [u32; 2] = [640, 480];
const MAX_RESOLUTION: [u32; 2] = [7680, 4320];

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
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::parse(&contents).unwrap_or_else(Self::default)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = format!(
            "version={}\nui_scale={:.3}\nvsync={}\nmaster_volume={:.3}\nwindow_mode={}\nresolution={}x{}\n",
            SETTINGS_VERSION,
            self.ui_scale,
            self.vsync,
            self.master_volume,
            self.window_mode.as_str(),
            self.resolution[0],
            self.resolution[1],
        );
        fs::write(path, data)
    }

    fn parse(contents: &str) -> Option<Self> {
        let mut settings = Self::default();
        let mut version = None;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=')?;
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
                    if let Ok(value) = value.parse::<bool>() {
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
                _ => {}
            }
        }
        if let Some(version) = version {
            if version != SETTINGS_VERSION {
                return Some(Self::default());
            }
        }
        settings.ui_scale = settings.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE);
        settings.master_volume = settings.master_volume.clamp(0.0, 1.0);
        settings.resolution = clamp_resolution(settings.resolution);
        settings.version = SETTINGS_VERSION;
        Some(settings)
    }
}

fn settings_path() -> PathBuf {
    user_config_root().join("settings.cfg")
}

fn parse_resolution(value: &str) -> Option<[u32; 2]> {
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
