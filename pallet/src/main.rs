use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::AudioEngine;
use client::{Client, ClientInput};
use compat_quake::bsp::{self, Bsp, SpawnPoint};
use compat_quake::lmp;
use compat_quake::pak::{self, PakFile};
use engine_core::vfs::{Vfs, VfsError};
use net_transport::{LoopbackTransport, Transport, TransportConfig};
use platform_winit::{
    create_window, ControlFlow, CursorGrabMode, DeviceEvent, ElementState, Event, Fullscreen, Ime,
    KeyCode, ModifiersState, MouseButton, MouseScrollDelta, PhysicalKey, PhysicalPosition,
    PhysicalSize, Window, WindowEvent,
};
use render_wgpu::{
    FrameCapture, ImageData, MeshData, MeshVertex, RenderCaptureError, RenderError, TextBounds,
    TextFontSystem, TextLayer, TextOverlay, TextPosition, TextStyle, TextViewport, YuvImageView,
};
use script_lua::{HostCallbacks, ScriptConfig, ScriptEngine, SpawnRequest};
use server::Server;
use video::{
    advance_playlist, start_video_playback, PlaylistEntry, VideoDebugSnapshot, VideoDebugStats,
    VideoPlayback, VIDEO_AUDIO_PREBUFFER_MS, VIDEO_HOLD_LAST_FRAME_MS, VIDEO_INTERMISSION_MS,
    VIDEO_MAX_QUEUED_MS_PLAYBACK, VIDEO_MAX_QUEUED_MS_PREDECODE, VIDEO_PLAYBACK_WARM_MS,
    VIDEO_PLAYBACK_WARM_UP_MS, VIDEO_PREDECODE_MIN_AUDIO_MS, VIDEO_PREDECODE_MIN_ELAPSED_MS,
    VIDEO_PREDECODE_MIN_FRAMES, VIDEO_PREDECODE_RAMP_MS, VIDEO_PREDECODE_START_DELAY_MS,
    VIDEO_PREDECODE_WARM_MS, VIDEO_START_MIN_FRAMES,
};
use wgpu::util::DeviceExt;

use settings::{Settings, WindowMode};
use ui::{MenuMode, MenuScreen, ResolutionModel, UiFacade, UiFrameInput, UiState};

mod settings;
mod ui;
mod video;

const EXIT_SUCCESS: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_QUAKE_DIR: i32 = 10;
const EXIT_PAK: i32 = 11;
const EXIT_IMAGE: i32 = 12;
const EXIT_BSP: i32 = 13;
const EXIT_SCENE: i32 = 14;
const EXIT_UI_REGRESSION: i32 = 20;
const DEFAULT_SFX: &str = "sound/misc/menu1.wav";
const HUD_FONT_SIZE: f32 = 16.0;
const HUD_FONT_SIZE_SMALL: f32 = 14.0;
const CONSOLE_FONT_SIZE: f32 = 14.0;
const HUD_TEXT_COLOR: [f32; 4] = [0.9, 0.95, 1.0, 1.0];
const CONSOLE_TEXT_COLOR: [f32; 4] = [0.9, 0.9, 0.9, 1.0];
const CONSOLE_BG_COLOR: [f32; 4] = [0.05, 0.05, 0.08, 0.75];
const CONSOLE_SEPARATOR_COLOR: [f32; 4] = [0.95, 0.95, 0.95, 0.9];
const CONSOLE_SELECTION_COLOR: [f32; 4] = [0.2, 0.4, 0.8, 0.35];
const CONSOLE_MENU_BG_COLOR: [f32; 4] = [0.08, 0.1, 0.14, 0.95];
const CONSOLE_MENU_TEXT_COLOR: [f32; 4] = [0.95, 0.95, 0.98, 1.0];
const CONSOLE_MENU_PADDING: f32 = 4.0;
const CONSOLE_MENU_CHAR_WIDTH: f32 = 0.6;
const CONSOLE_TOAST_DURATION_MS: u64 = 1200;
const CONSOLE_HEIGHT_RATIO: f32 = 0.45;
const CONSOLE_PADDING: f32 = 6.0;
const CONSOLE_INPUT_PADDING: f32 = 0.5;
const CONSOLE_INPUT_BG_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const CONSOLE_SLIDE_MS: u64 = 500;
const CONSOLE_SELECTION_LINE_LIMIT: usize = 128;
const UI_REGRESSION_MIN_FONT_PX: f32 = 9.0;
const UI_REGRESSION_LOG_LINE_COUNT: usize = 24;
const UI_REGRESSION_FPS: f32 = 144.0;
const UI_REGRESSION_SIM_RATE: f32 = 60.0;
const UI_REGRESSION_NET_RATE: f32 = 30.0;
const BOOT_FULLSCREEN_STARTUP_MS: u64 = 300;
const BOOT_FULLSCREEN_SETTLE_MS: u64 = 120;
const BOOT_PRESENT_WARMUP: usize = 2;
const CONSOLE_CARET_BLINK_MS: u64 = 500;
const LINE_HEIGHT_SCALE: f32 = 1.2;
const CONSOLE_LOG_SHADER: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0)
var t_color: texture_2d<f32>;
@group(0) @binding(1)
var s_color: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(t_color, s_color, in.uv);
}
"#;

const CAMERA_UP: Vec3 = Vec3::new(0.0, 1.0, 0.0);
const CAMERA_FOV_Y: f32 = 70.0f32.to_radians();
const CAMERA_NEAR: f32 = 0.25;
const CAMERA_FAR: f32 = 8192.0;
const PLAYER_SPEED: f32 = 320.0;
const PLAYER_ACCEL: f32 = 12.0;
const PLAYER_FRICTION: f32 = 14.0;
const PLAYER_STOP_SPEED: f32 = 100.0;
const PLAYER_GRAVITY: f32 = 800.0;
const PLAYER_JUMP_SPEED: f32 = 270.0;
const PLAYER_EYE_HEIGHT: f32 = 22.0;
const PLAYER_STEP_HEIGHT: f32 = 18.0;
const PLAYER_MAX_DROP: f32 = 256.0;
const FLOOR_NORMAL_MIN: f32 = 0.7;
const DIST_EPSILON: f32 = 0.03125;
const CONTENTS_SOLID: i32 = -2;
const OPENGL_TO_WGPU: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 0.5, 0.0],
    [0.0, 0.0, 0.5, 1.0],
];

/// Controls startup visibility to avoid compositor flashes.
/// Fullscreen modes warm up hidden presents before showing the window.
enum BootState {
    Visible,
    Hidden {
        settle_deadline: Instant,
        presents_left: usize,
    },
}

impl BootState {
    fn new(window_mode: WindowMode, now: Instant) -> Self {
        match window_mode {
            WindowMode::Windowed => BootState::Visible,
            WindowMode::Borderless | WindowMode::Fullscreen => BootState::Hidden {
                settle_deadline: now + Duration::from_millis(BOOT_FULLSCREEN_STARTUP_MS),
                presents_left: BOOT_PRESENT_WARMUP,
            },
        }
    }

    fn is_hidden(&self) -> bool {
        matches!(self, BootState::Hidden { .. })
    }

    fn on_resize(&mut self, now: Instant) {
        if let BootState::Hidden {
            settle_deadline,
            presents_left,
        } = self
        {
            *settle_deadline = now + Duration::from_millis(BOOT_FULLSCREEN_SETTLE_MS);
            *presents_left = BOOT_PRESENT_WARMUP;
        }
    }

    fn on_initial_render(&self, window: &Window) {
        if matches!(self, BootState::Visible) {
            window.set_visible(true);
        }
    }

    /// Returns true when the hidden warmup completes and the window becomes visible.
    fn on_present(&mut self, now: Instant, window: &Window) -> bool {
        if let BootState::Hidden {
            settle_deadline,
            presents_left,
        } = self
        {
            if now < *settle_deadline {
                return false;
            }
            if *presents_left > 0 {
                *presents_left = presents_left.saturating_sub(1);
            }
            if *presents_left == 0 {
                window.set_visible(true);
                *self = BootState::Visible;
                return true;
            }
        }
        false
    }

    fn needs_warmup(&self, now: Instant) -> bool {
        match self {
            BootState::Hidden {
                settle_deadline,
                presents_left,
            } => now >= *settle_deadline && *presents_left > 0,
            BootState::Visible => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiRegressionScreen {
    Main,
    Options,
}

impl UiRegressionScreen {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "main" => Some(UiRegressionScreen::Main),
            "options" => Some(UiRegressionScreen::Options),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct UiRegressionArgs {
    shot_path: PathBuf,
    resolution: [u32; 2],
    dpi_scale: f32,
    ui_scale: f32,
    screen: UiRegressionScreen,
}

struct UiRegressionChecks {
    min_font_px: f32,
    ui_bounds_ok: bool,
    ui_used_min: [f32; 2],
    ui_used_max: [f32; 2],
    ui_limit: [f32; 2],
    console_input_visible: bool,
    hud_bounds_ok: bool,
}

impl UiRegressionChecks {
    fn new(ui_limit: [f32; 2]) -> Self {
        Self {
            min_font_px: f32::INFINITY,
            ui_bounds_ok: true,
            ui_used_min: [0.0, 0.0],
            ui_used_max: [0.0, 0.0],
            ui_limit,
            console_input_visible: true,
            hud_bounds_ok: true,
        }
    }

    fn record_min_font(&mut self, value: f32) {
        if value.is_finite() {
            self.min_font_px = self.min_font_px.min(value);
        }
    }

    fn record_ui_bounds(&mut self, used_rect: egui::Rect) {
        let min = used_rect.min;
        let max = used_rect.max;
        self.ui_used_min = [min.x, min.y];
        self.ui_used_max = [max.x, max.y];
        self.ui_bounds_ok =
            min.x >= 0.0 && min.y >= 0.0 && max.x <= self.ui_limit[0] && max.y <= self.ui_limit[1];
    }

    fn record_console_input(&mut self, visible: bool) {
        self.console_input_visible = self.console_input_visible && visible;
    }

    fn record_hud_bounds(&mut self, ok: bool) {
        self.hud_bounds_ok = self.hud_bounds_ok && ok;
    }

    fn validate(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        let min_font = if self.min_font_px.is_finite() {
            self.min_font_px
        } else {
            0.0
        };
        if min_font < UI_REGRESSION_MIN_FONT_PX {
            errors.push(format!(
                "min font {:.2}px below threshold {:.2}px",
                min_font, UI_REGRESSION_MIN_FONT_PX
            ));
        }
        if !self.ui_bounds_ok {
            errors.push(format!(
                "ui bounds exceeded (min {:.2},{:.2} max {:.2},{:.2} limit {:.2},{:.2})",
                self.ui_used_min[0],
                self.ui_used_min[1],
                self.ui_used_max[0],
                self.ui_used_max[1],
                self.ui_limit[0],
                self.ui_limit[1]
            ));
        }
        if !self.console_input_visible {
            errors.push("console input line not visible".to_string());
        }
        if !self.hud_bounds_ok {
            errors.push("hud text is clipped".to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

struct CliArgs {
    quake_dir: Option<PathBuf>,
    show_image: Option<String>,
    map: Option<String>,
    play_movie: Option<PathBuf>,
    playlist: Option<PathBuf>,
    script: Option<PathBuf>,
    input_script: bool,
    ui_regression: Option<UiRegressionArgs>,
}

enum ArgParseError {
    Help,
    Message(String),
}

struct ExitError {
    code: i32,
    message: String,
}

impl ExitError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn enter_map_scene(
    renderer: &mut render_wgpu::Renderer,
    window: &Window,
    quake_dir: &Path,
    map: &str,
    audio: Option<&Rc<AudioEngine>>,
    camera: &mut CameraState,
    collision: &mut Option<SceneCollision>,
    scene_active: &mut bool,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    loopback: &mut Option<LoopbackNet>,
) -> Result<(), ExitError> {
    let (mesh, bounds, scene_collision, spawn) = load_bsp_scene(quake_dir, map)?;

    renderer.clear_textured_quad();
    renderer
        .set_scene(mesh)
        .map_err(|err| ExitError::new(EXIT_SCENE, format!("scene upload failed: {}", err)))?;

    *collision = Some(scene_collision);
    *camera = CameraState::from_bounds(&bounds, collision.as_ref());
    if let Some(spawn) = spawn {
        let base = quake_to_render(spawn.origin);
        camera.position = camera.camera_from_origin(base);
        if let Some(angle) = spawn.angle {
            camera.yaw = (90.0 - angle).to_radians();
            camera.pitch = 0.0;
        }
        // Test-only spawn from BSP entities; M8 Lua spawning will replace this path.
        if let Some(scene) = collision.as_ref() {
            camera.snap_to_floor(scene);
        }
    }
    *scene_active = true;
    *mouse_look = false;
    *mouse_grabbed = set_cursor_mode(window, *mouse_look);
    let aspect = aspect_ratio(renderer.size());
    renderer.update_camera(camera.view_proj(aspect));

    *loopback = match LoopbackNet::start() {
        Ok(net) => Some(net),
        Err(err) => {
            eprintln!("loopback init failed: {}", err);
            None
        }
    };

    if let Some(audio) = audio {
        match load_music_track(quake_dir) {
            Ok(Some(track)) => {
                if let Err(err) = audio.play_music(track.data) {
                    eprintln!("{}", err);
                } else {
                    println!("streaming {}", track.name);
                }
            }
            Ok(None) => {}
            Err(err) => eprintln!("{}", err.message),
        }
    }

    Ok(())
}

struct MusicTrack {
    name: String,
    data: Vec<u8>,
}

#[derive(Default)]
struct InputState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    jump: bool,
    down: bool,
}

struct ConsoleState {
    phase: ConsolePhase,
    progress: f32,
    anim_started: Instant,
    anim_from: f32,
    anim_to: f32,
    scroll_offset: f32,
    visible_lines: usize,
    line_height: f32,
    log_area: Option<ConsoleLogArea>,
    selection: Option<ConsoleSelection>,
    selecting: bool,
    resume_mouse_look: bool,
    context_menu: Option<ConsoleContextMenu>,
    toast: Option<ConsoleToast>,
    caret_epoch: Instant,
    buffer: String,
    log: VecDeque<String>,
    max_lines: usize,
    log_revision: u64,
    visible_cache: Option<ConsoleVisibleCache>,
}

#[derive(Clone, Copy, Debug)]
struct ConsoleLogArea {
    y: f32,
    height: f32,
}

#[derive(Clone, Copy, Debug)]
struct ConsoleSelection {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug)]
struct ConsoleContextMenu {
    position: TextPosition,
    bounds: Option<ConsoleMenuBounds>,
}

#[derive(Clone, Copy, Debug)]
struct ConsoleMenuBounds {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl ConsoleMenuBounds {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.width && y <= self.y + self.height
    }
}

#[derive(Clone, Debug)]
struct ConsoleToast {
    text: String,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct ConsoleVisibleCache {
    start: usize,
    lines: usize,
    revision: u64,
    text: Arc<str>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ConsoleLogParams {
    revision: u64,
    start: usize,
    draw_lines: usize,
    scroll_px: u32,
    line_height_px: u32,
    font_px: u32,
    selection: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ConsoleLogRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

struct ConsoleLogUpdate {
    params: ConsoleLogParams,
    size: [u32; 2],
    viewport: TextViewport,
}

struct ConsoleLogCache {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    format: wgpu::TextureFormat,
    texture: Option<wgpu::Texture>,
    view: Option<wgpu::TextureView>,
    bind_group: Option<wgpu::BindGroup>,
    size: [u32; 2],
    last_params: Option<ConsoleLogParams>,
    last_rect: Option<ConsoleLogRect>,
    last_surface: [u32; 2],
}

impl Default for ConsoleState {
    fn default() -> Self {
        Self {
            phase: ConsolePhase::Closed,
            progress: 0.0,
            anim_started: Instant::now(),
            anim_from: 0.0,
            anim_to: 0.0,
            scroll_offset: 0.0,
            visible_lines: 0,
            line_height: 0.0,
            log_area: None,
            selection: None,
            selecting: false,
            resume_mouse_look: false,
            context_menu: None,
            toast: None,
            caret_epoch: Instant::now(),
            buffer: String::new(),
            log: VecDeque::new(),
            max_lines: 256,
            log_revision: 0,
            visible_cache: None,
        }
    }
}

impl ConsoleState {
    fn update(&mut self, now: Instant) {
        if let Some(toast) = &self.toast {
            if now >= toast.expires_at {
                self.toast = None;
            }
        }
        match self.phase {
            ConsolePhase::Opening | ConsolePhase::Closing => {
                let duration = Duration::from_millis(CONSOLE_SLIDE_MS).as_secs_f32();
                let elapsed = (now - self.anim_started).as_secs_f32();
                let t = (elapsed / duration).clamp(0.0, 1.0);
                let eased = t * t * (3.0 - 2.0 * t);
                self.progress = self.anim_from + (self.anim_to - self.anim_from) * eased;
                if t >= 1.0 {
                    self.progress = self.anim_to;
                    self.phase = if self.progress >= 1.0 {
                        ConsolePhase::Open
                    } else {
                        ConsolePhase::Closed
                    };
                }
            }
            ConsolePhase::Closed => {
                self.progress = 0.0;
            }
            ConsolePhase::Open => {
                self.progress = 1.0;
            }
        }
    }

    fn open(&mut self, now: Instant) {
        if self.phase == ConsolePhase::Open {
            return;
        }
        self.anim_from = self.progress;
        self.anim_to = 1.0;
        self.anim_started = now;
        self.phase = ConsolePhase::Opening;
        self.scroll_offset = 0.0;
        self.toast = None;
        self.clear_selection();
    }

    fn close(&mut self, now: Instant) {
        if self.phase == ConsolePhase::Closed {
            return;
        }
        self.anim_from = self.progress;
        self.anim_to = 0.0;
        self.anim_started = now;
        self.phase = ConsolePhase::Closing;
        self.scroll_offset = 0.0;
        self.toast = None;
        self.clear_selection();
    }

    fn force_closed(&mut self) {
        self.phase = ConsolePhase::Closed;
        self.progress = 0.0;
        self.anim_from = 0.0;
        self.anim_to = 0.0;
        self.scroll_offset = 0.0;
        self.resume_mouse_look = false;
        self.toast = None;
        self.clear_selection();
    }

    fn force_open(&mut self, now: Instant) {
        self.phase = ConsolePhase::Open;
        self.progress = 1.0;
        self.anim_from = 1.0;
        self.anim_to = 1.0;
        self.anim_started = now;
        self.scroll_offset = 0.0;
        self.toast = None;
        self.clear_selection();
    }

    fn is_blocking(&self) -> bool {
        self.phase != ConsolePhase::Closed
    }

    fn is_visible(&self) -> bool {
        self.progress > 0.0
    }

    fn is_interactive(&self) -> bool {
        self.phase == ConsolePhase::Open
    }

    fn is_opening(&self) -> bool {
        self.phase == ConsolePhase::Opening
    }

    fn height_ratio(&self) -> f32 {
        self.progress
    }

    fn caret_visible(&self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.caret_epoch);
        (elapsed.as_millis() / CONSOLE_CARET_BLINK_MS as u128).is_multiple_of(2)
    }

    fn set_log_area(&mut self, area: Option<ConsoleLogArea>) {
        self.log_area = area;
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.selecting = false;
        self.close_menu();
    }

    fn is_selecting(&self) -> bool {
        self.selecting
    }

    fn open_menu(&mut self, position: TextPosition) {
        self.context_menu = Some(ConsoleContextMenu {
            position,
            bounds: None,
        });
    }

    fn close_menu(&mut self) {
        self.context_menu = None;
    }

    fn menu_bounds(&self) -> Option<ConsoleMenuBounds> {
        self.context_menu.and_then(|menu| menu.bounds)
    }

    fn set_menu_bounds(&mut self, bounds: Option<ConsoleMenuBounds>) {
        if let Some(menu) = self.context_menu.as_mut() {
            menu.bounds = bounds;
        }
    }

    fn menu_position(&self) -> Option<TextPosition> {
        self.context_menu.map(|menu| menu.position)
    }

    fn show_toast(&mut self, text: impl Into<String>, now: Instant) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.toast = Some(ConsoleToast {
            text,
            expires_at: now + Duration::from_millis(CONSOLE_TOAST_DURATION_MS),
        });
    }

    fn start_selection(&mut self, line: usize) {
        self.selection = Some(ConsoleSelection {
            start: line,
            end: line,
        });
        self.selecting = true;
    }

    fn update_selection(&mut self, line: usize) {
        if let Some(selection) = &mut self.selection {
            selection.end = line;
        } else {
            self.start_selection(line);
        }
    }

    fn finish_selection(&mut self) {
        self.selecting = false;
    }

    fn selection_range_limited(&self, max_lines: usize) -> Option<(usize, usize)> {
        let selection = self.selection?;
        if max_lines == 0 {
            return None;
        }
        let limit = max_lines.saturating_sub(1);
        if selection.start <= selection.end {
            let end = selection.end.min(selection.start.saturating_add(limit));
            Some((selection.start, end))
        } else {
            let end = selection.end.max(selection.start.saturating_sub(limit));
            Some((end, selection.start))
        }
    }

    fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection_range_limited(CONSOLE_SELECTION_LINE_LIMIT)?;
        if self.log.is_empty() || start >= self.log.len() {
            return None;
        }
        let end = end.min(self.log.len().saturating_sub(1));
        let mut text = String::new();
        for (index, line) in self
            .log
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start) + 1)
        {
            if index > start {
                text.push('\n');
            }
            text.push_str(line);
        }
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn log_line_at(&self, y: f32, clamp: bool) -> Option<usize> {
        let area = self.log_area?;
        if self.log.is_empty() || self.line_height <= 0.0 {
            return None;
        }
        let log_height = area.height.max(0.0);
        if log_height <= 0.0 {
            return None;
        }
        if !clamp && (y < area.y || y > area.y + log_height) {
            return None;
        }
        let line_height = self.line_height.max(1.0);
        let max_lines = (log_height / line_height).floor() as usize;
        if max_lines == 0 {
            return None;
        }
        let content_height = line_height * self.log.len() as f32;
        let max_offset = (content_height - log_height).max(0.0);
        let scroll_px = self.scroll_offset.round().clamp(0.0, max_offset);
        let scroll_lines = (scroll_px / line_height).floor() as usize;
        let scroll_remainder = scroll_px - scroll_lines as f32 * line_height;
        let extra_line = if scroll_remainder > 0.0 {
            1usize
        } else {
            0usize
        };
        let draw_lines = (max_lines + extra_line).min(self.log.len());
        if draw_lines == 0 {
            return None;
        }
        let y_offset = if extra_line == 1 {
            (line_height - scroll_remainder).round()
        } else {
            0.0
        };
        let y_clamped = y.clamp(area.y, area.y + log_height - 1.0).max(area.y);
        let top = area.y - y_offset;
        let line_in_view = ((y_clamped - top) / line_height)
            .floor()
            .clamp(0.0, (draw_lines - 1) as f32) as usize;
        let start = self.log.len().saturating_sub(draw_lines + scroll_lines);
        let line_index = start + line_in_view;
        if line_index < self.log.len() {
            Some(line_index)
        } else {
            None
        }
    }

    fn scroll_by(&mut self, delta_px: f32) {
        if delta_px.is_finite() {
            self.scroll_offset = (self.scroll_offset + delta_px).max(0.0);
        }
    }

    fn bump_log_revision(&mut self) {
        self.log_revision = self.log_revision.wrapping_add(1);
        self.visible_cache = None;
    }

    fn clear_log(&mut self) {
        self.log.clear();
        self.bump_log_revision();
        self.clear_selection();
    }

    fn visible_text(&mut self, start: usize, lines: usize) -> Arc<str> {
        if self.log.is_empty() || lines == 0 || start >= self.log.len() {
            return Arc::from("");
        }
        let lines = lines.min(self.log.len().saturating_sub(start));
        if let Some(cache) = &self.visible_cache {
            if cache.start == start && cache.lines == lines && cache.revision == self.log_revision {
                return cache.text.clone();
            }
        }
        let mut text = String::new();
        for index in start..start + lines {
            if let Some(line) = self.log.get(index) {
                if index > start {
                    text.push('\n');
                }
                text.push_str(line);
            }
        }
        let text: Arc<str> = Arc::from(text);
        self.visible_cache = Some(ConsoleVisibleCache {
            start,
            lines,
            revision: self.log_revision,
            text: text.clone(),
        });
        text
    }

    fn push_line(&mut self, line: impl Into<String>) {
        let line = line.into();
        if line.is_empty() {
            return;
        }
        self.log.push_back(line);
        if self.scroll_offset > 0.0 {
            self.scroll_offset += self.line_height.max(1.0);
        }
        let mut removed = 0usize;
        while self.log.len() > self.max_lines {
            self.log.pop_front();
            removed = removed.saturating_add(1);
        }
        if removed > 0 {
            if let Some(selection) = &mut self.selection {
                if selection.start < removed && selection.end < removed {
                    self.selection = None;
                    self.selecting = false;
                } else {
                    selection.start = selection.start.saturating_sub(removed);
                    selection.end = selection.end.saturating_sub(removed);
                }
            }
        }
        self.bump_log_revision();
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ConsoleQuadVertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

impl ConsoleLogCache {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pallet.console_log.shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(CONSOLE_LOG_SHADER)),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pallet.console_log.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pallet.console_log.bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pallet.console_log.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pallet.console_log.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pallet.console_log.vertex_buffer"),
            size: (std::mem::size_of::<ConsoleQuadVertex>() * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_data = console_quad_index_bytes(&[0, 1, 2, 0, 2, 3]);
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.console_log.index_buffer"),
            contents: &index_data,
            usage: wgpu::BufferUsages::INDEX,
        });
        Self {
            pipeline,
            bind_group_layout,
            sampler,
            vertex_buffer,
            index_buffer,
            index_count: 6,
            format,
            texture: None,
            view: None,
            bind_group: None,
            size: [0, 0],
            last_params: None,
            last_rect: None,
            last_surface: [0, 0],
        }
    }

    fn needs_update(&self, params: ConsoleLogParams, size: [u32; 2]) -> bool {
        if size[0] == 0 || size[1] == 0 {
            return false;
        }
        if self.size != size {
            return true;
        }
        match self.last_params {
            Some(last) => last != params,
            None => true,
        }
    }

    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        log_overlay: &mut TextOverlay,
        update: ConsoleLogUpdate,
    ) {
        if update.size[0] == 0 || update.size[1] == 0 {
            self.size = update.size;
            self.texture = None;
            self.view = None;
            self.bind_group = None;
            self.last_params = Some(update.params);
            return;
        }
        self.ensure_texture(device, update.size);
        let view = match self.view.as_ref() {
            Some(view) => view,
            None => return,
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pallet.console_log.cache.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        log_overlay.flush_layers(
            &mut pass,
            update.viewport,
            device,
            queue,
            &[TextLayer::ConsoleLog],
        );
        self.last_params = Some(update.params);
    }

    fn draw<'pass>(
        &'pass mut self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'pass>,
        rect: ConsoleLogRect,
        surface_size: [u32; 2],
    ) {
        let bind_group = match self.bind_group.as_ref() {
            Some(bind_group) => bind_group,
            None => return,
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let surface_size = [surface_size[0].max(1), surface_size[1].max(1)];
        let rect = ConsoleLogRect {
            x: rect.x.round(),
            y: rect.y.round(),
            width: rect.width.round().max(1.0),
            height: rect.height.round().max(1.0),
        };
        if self.last_rect != Some(rect) || self.last_surface != surface_size {
            let texture_width = self.size[0].max(1) as f32;
            let texture_height = self.size[1].max(1) as f32;
            let uv_max_x = (rect.width / texture_width).clamp(0.0, 1.0);
            let uv_max_y = (rect.height / texture_height).clamp(0.0, 1.0);
            let vertices = console_quad_vertices(rect, surface_size, [uv_max_x, uv_max_y]);
            let bytes = console_quad_vertex_bytes(&vertices);
            queue.write_buffer(&self.vertex_buffer, 0, &bytes);
            self.last_rect = Some(rect);
            self.last_surface = surface_size;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }

    fn ensure_texture(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.size == size && self.view.is_some() {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pallet.console_log.texture"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pallet.console_log.bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.texture = Some(texture);
        self.view = Some(view);
        self.bind_group = Some(bind_group);
        self.size = size;
        self.last_rect = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsolePhase {
    Closed,
    Opening,
    Open,
    Closing,
}

struct HudState {
    frame_count: u32,
    last_sample: Instant,
    fps: f32,
}

impl HudState {
    fn new(now: Instant) -> Self {
        Self {
            frame_count: 0,
            last_sample: now,
            fps: 0.0,
        }
    }

    fn update(&mut self, now: Instant) -> f32 {
        self.frame_count = self.frame_count.saturating_add(1);
        let elapsed = now.saturating_duration_since(self.last_sample);
        if elapsed >= Duration::from_millis(500) {
            let secs = elapsed.as_secs_f32().max(0.001);
            self.fps = self.frame_count as f32 / secs;
            self.frame_count = 0;
            self.last_sample = now;
        }
        self.fps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputLayer {
    Console,
    Menu,
    Game,
}

struct InputRouter;

impl InputRouter {
    fn new() -> Self {
        Self
    }

    fn update_ui_focus(&mut self, _wants_keyboard: bool, _wants_pointer: bool) {
        // Console toggle is always allowed when videos are not playing.
    }

    fn active_layer(&self, console_open: bool, menu_open: bool) -> InputLayer {
        if console_open {
            InputLayer::Console
        } else if menu_open {
            InputLayer::Menu
        } else {
            InputLayer::Game
        }
    }

    fn allow_console_toggle(&self, _menu_open: bool) -> bool {
        true
    }
}

const INPUT_SCRIPT_STEP_DELAY_MS: u64 = 200;

struct InputScript {
    step: usize,
    next_at: Instant,
    ui_scale_before: f32,
    reported_missing_map: bool,
}

impl InputScript {
    fn new(now: Instant, settings: &Settings) -> Self {
        Self {
            step: 0,
            next_at: now,
            ui_scale_before: settings.ui_scale,
            reported_missing_map: false,
        }
    }

    fn ready(&self, now: Instant) -> bool {
        now >= self.next_at
    }

    fn advance(&mut self, now: Instant) {
        self.step = self.step.saturating_add(1);
        self.next_at = now + Duration::from_millis(INPUT_SCRIPT_STEP_DELAY_MS);
    }
}

struct ScriptEntity {
    id: u32,
    position: Vec3,
    yaw: f32,
}

struct ScriptHostState {
    next_id: u32,
    entities: Vec<ScriptEntity>,
    quake_dir: Option<PathBuf>,
    audio: Option<Rc<AudioEngine>>,
}

impl ScriptHostState {
    fn new(quake_dir: Option<PathBuf>, audio: Option<Rc<AudioEngine>>) -> Self {
        Self {
            next_id: 1,
            entities: Vec::new(),
            quake_dir,
            audio,
        }
    }

    fn spawn_entity(&mut self, request: SpawnRequest) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let entity = ScriptEntity {
            id,
            position: Vec3::new(
                request.position[0],
                request.position[1],
                request.position[2],
            ),
            yaw: request.yaw,
        };
        println!(
            "lua spawn entity {} at ({:.2}, {:.2}, {:.2}) yaw {:.2}",
            entity.id, entity.position.x, entity.position.y, entity.position.z, entity.yaw
        );
        self.entities.push(entity);
        let count = self.entities.len();
        println!("lua entities: {}", count);
        id
    }

    fn play_sound(&mut self, asset: String) -> Result<(), String> {
        let audio = self
            .audio
            .as_ref()
            .ok_or_else(|| "audio disabled".to_string())?;
        let quake_dir = self
            .quake_dir
            .as_ref()
            .ok_or_else(|| "quake dir is required for play_sound".to_string())?;
        let data = load_wav_sfx(quake_dir, &asset).map_err(|err| err.message)?;
        audio
            .play_wav(data)
            .map_err(|err| format!("play_sound failed: {}", err))?;
        Ok(())
    }
}

struct ScriptRuntime {
    engine: ScriptEngine,
    _host: Rc<RefCell<ScriptHostState>>,
}

struct LoopbackNet {
    client: Client,
    server: Server,
    saw_snapshot: bool,
}

impl LoopbackNet {
    fn start() -> Result<Self, String> {
        let transport = TransportConfig::default();
        let mut server_transport =
            LoopbackTransport::bind(transport.clone()).map_err(|err| err.to_string())?;
        let mut client_transport =
            LoopbackTransport::bind(transport).map_err(|err| err.to_string())?;
        let server_addr = server_transport
            .local_addr()
            .map_err(|err: net_transport::TransportError| err.to_string())?;
        let client_addr = client_transport
            .local_addr()
            .map_err(|err: net_transport::TransportError| err.to_string())?;
        server_transport.connect_peer(client_addr);
        client_transport.connect_peer(server_addr);

        let server = Server::bind(Box::new(server_transport), 1).map_err(|err| err.to_string())?;
        let client = Client::connect(Box::new(client_transport), server_addr, 1)
            .map_err(|err| err.to_string())?;
        Ok(Self {
            client,
            server,
            saw_snapshot: false,
        })
    }

    fn tick(&mut self, input: &InputState, camera: &CameraState) -> Result<(), String> {
        let move_x = bool_to_axis(input.forward, input.back);
        let move_y = bool_to_axis(input.right, input.left);
        let buttons = if input.jump { 1 } else { 0 };
        self.client
            .send_input(ClientInput {
                move_x,
                move_y,
                yaw: camera.yaw,
                pitch: camera.pitch,
                buttons,
            })
            .map_err(|err| err.to_string())?;
        let _ = self.server.tick().map_err(|err| err.to_string())?;
        self.client.poll().map_err(|err| err.to_string())?;
        if !self.saw_snapshot && self.client.last_snapshot().is_some() {
            println!("loopback snapshot received");
            self.saw_snapshot = true;
        }
        Ok(())
    }
}

struct CameraState {
    position: Vec3,
    yaw: f32,
    pitch: f32,
    velocity: Vec3,
    vertical_velocity: f32,
    on_ground: bool,
    speed: f32,
    accel: f32,
    friction: f32,
    gravity: f32,
    jump_speed: f32,
    eye_height: f32,
    step_height: f32,
    max_drop: f32,
    sensitivity: f32,
}

impl CameraState {
    fn from_bounds(bounds: &Bounds, collision: Option<&SceneCollision>) -> Self {
        let mut camera = Self::default();
        let center = bounds.center();
        let extent = bounds.extent().length().max(1.0);
        let mut position = Vec3::new(
            center.x,
            center.y + camera.eye_height,
            center.z + extent * 0.25,
        );
        if let Some(collision) = collision {
            if let Some(spawn) = collision.spawn_point(bounds, camera.eye_height) {
                position = spawn;
                camera.on_ground = true;
            } else {
                camera.position = position;
                camera.snap_to_floor(collision);
                position = camera.position;
            }
        }
        let dir = center.sub(position).normalize_or_zero();
        camera.pitch = dir.y.asin();
        camera.yaw = dir.x.atan2(-dir.z);
        camera.position = position;
        camera.speed = (extent * 0.15).max(PLAYER_SPEED);
        camera
    }

    fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            -self.yaw.cos() * self.pitch.cos(),
        )
    }

    fn forward_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.sin(), 0.0, -self.yaw.cos())
    }

    fn right_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.cos(), 0.0, self.yaw.sin())
    }

    fn update(
        &mut self,
        input: &InputState,
        dt: f32,
        collision: Option<&SceneCollision>,
        fly_mode: bool,
    ) {
        if fly_mode {
            self.update_fly(input, dt);
            return;
        }
        let mut wish_dir = Vec3::zero();
        let forward = self.forward_flat();
        let right = self.right_flat();
        if input.forward {
            wish_dir = wish_dir.add(forward);
        }
        if input.back {
            wish_dir = wish_dir.sub(forward);
        }
        if input.right {
            wish_dir = wish_dir.add(right);
        }
        if input.left {
            wish_dir = wish_dir.sub(right);
        }

        if self.on_ground {
            let speed = self.velocity.length();
            if speed > 0.0 {
                let control = speed.max(PLAYER_STOP_SPEED);
                let drop = control * self.friction * dt;
                let new_speed = (speed - drop).max(0.0);
                self.velocity = self.velocity.scale(new_speed / speed);
            }
        }
        if wish_dir.length() > 0.0 {
            let wish_dir = wish_dir.normalize_or_zero();
            let current_speed = self.velocity.dot(wish_dir);
            let add_speed = self.speed - current_speed;
            if add_speed > 0.0 {
                let accel_speed = (self.accel * dt * self.speed).min(add_speed);
                self.velocity = self.velocity.add(wish_dir.scale(accel_speed));
            }
        }

        if self.on_ground && input.jump {
            self.vertical_velocity = self.jump_speed;
            self.on_ground = false;
        }
        self.vertical_velocity -= self.gravity * dt;

        let origin = self.collision_origin();
        let velocity = Vec3::new(self.velocity.x, self.vertical_velocity, self.velocity.z);
        if let Some(scene) = collision {
            let origin = scene.try_unstuck(origin);
            let (mut new_origin, new_velocity, mut on_ground) =
                scene.move_with_step(origin, velocity, dt, self.step_height);
            if scene.hull_point_contents(scene.headnode, new_origin) == CONTENTS_SOLID {
                let unstuck = scene.try_unstuck(new_origin);
                if scene.hull_point_contents(scene.headnode, unstuck) != CONTENTS_SOLID {
                    new_origin = unstuck;
                }
            }
            let check = scene.trace(new_origin, new_origin.add(Vec3::new(0.0, -2.0, 0.0)));
            if !check.start_solid && check.fraction < 1.0 && check.plane_normal.y > FLOOR_NORMAL_MIN
            {
                new_origin = check.end;
                on_ground = true;
            }
            self.on_ground = on_ground;
            self.vertical_velocity = if on_ground && new_velocity.y < 0.0 {
                0.0
            } else {
                new_velocity.y
            };
            self.velocity.x = new_velocity.x;
            self.velocity.z = new_velocity.z;
            self.position = self.camera_from_origin(new_origin);
        } else {
            self.position = self.position.add(velocity.scale(dt));
            self.velocity.x = velocity.x;
            self.velocity.z = velocity.z;
        }
    }

    fn update_fly(&mut self, input: &InputState, dt: f32) {
        let mut direction = Vec3::zero();
        let forward = self.forward();
        let right = forward.cross(CAMERA_UP).normalize_or_zero();
        if input.forward {
            direction = direction.add(forward);
        }
        if input.back {
            direction = direction.sub(forward);
        }
        if input.right {
            direction = direction.add(right);
        }
        if input.left {
            direction = direction.sub(right);
        }
        if input.jump {
            direction = direction.add(CAMERA_UP);
        }
        if input.down {
            direction = direction.sub(CAMERA_UP);
        }

        let direction = direction.normalize_or_zero();
        self.position = self.position.add(direction.scale(self.speed * dt));
        self.velocity = Vec3::zero();
        self.vertical_velocity = 0.0;
        self.on_ground = false;
    }

    fn snap_to_floor(&mut self, collision: &SceneCollision) {
        let origin = self.collision_origin();
        let target = origin.sub(Vec3::new(0.0, self.max_drop, 0.0));
        let trace = collision.trace(origin, target);
        if trace.start_solid {
            return;
        }
        if trace.fraction < 1.0 {
            self.position = self.camera_from_origin(trace.end);
            self.on_ground = true;
            self.vertical_velocity = 0.0;
        }
    }

    fn collision_origin(&self) -> Vec3 {
        Vec3::new(
            self.position.x,
            self.position.y - self.eye_height,
            self.position.z,
        )
    }

    fn camera_from_origin(&self, origin: Vec3) -> Vec3 {
        Vec3::new(origin.x, origin.y + self.eye_height, origin.z)
    }

    fn apply_mouse(&mut self, delta_x: f64, delta_y: f64) {
        let dx = delta_x as f32;
        let dy = delta_y as f32;
        self.yaw += dx * self.sensitivity;
        self.pitch = (self.pitch - dy * self.sensitivity).clamp(-1.54, 1.54);
    }

    fn view_proj(&self, aspect: f32) -> [[f32; 4]; 4] {
        let view = self.view_matrix();
        let proj = perspective(CAMERA_FOV_Y, aspect, CAMERA_NEAR, CAMERA_FAR);
        mat4_mul(proj, view)
    }

    fn view_matrix(&self) -> [[f32; 4]; 4] {
        let forward = self.forward();
        let right = forward.cross(CAMERA_UP).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        [
            [right.x, up.x, -forward.x, 0.0],
            [right.y, up.y, -forward.y, 0.0],
            [right.z, up.z, -forward.z, 0.0],
            [
                -right.dot(self.position),
                -up.dot(self.position),
                forward.dot(self.position),
                1.0,
            ],
        ]
    }
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            position: Vec3::zero(),
            yaw: 0.0,
            pitch: 0.0,
            velocity: Vec3::zero(),
            vertical_velocity: 0.0,
            on_ground: false,
            speed: PLAYER_SPEED,
            accel: PLAYER_ACCEL,
            friction: PLAYER_FRICTION,
            gravity: PLAYER_GRAVITY,
            jump_speed: PLAYER_JUMP_SPEED,
            eye_height: PLAYER_EYE_HEIGHT,
            step_height: PLAYER_STEP_HEIGHT,
            max_drop: PLAYER_MAX_DROP,
            sensitivity: 0.0025,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    const fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }

    fn scale(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor, self.z * factor)
    }

    fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    fn normalize_or_zero(self) -> Self {
        let len = self.length();
        if len > 0.0 {
            self.scale(1.0 / len)
        } else {
            Self::zero()
        }
    }

    fn abs(self) -> Self {
        Self::new(self.x.abs(), self.y.abs(), self.z.abs())
    }

    fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    min: Vec3,
    max: Vec3,
    valid: bool,
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: Vec3::new(f32::MAX, f32::MAX, f32::MAX),
            max: Vec3::new(f32::MIN, f32::MIN, f32::MIN),
            valid: false,
        }
    }

    fn include(&mut self, value: Vec3) {
        self.min = Vec3::new(
            self.min.x.min(value.x),
            self.min.y.min(value.y),
            self.min.z.min(value.z),
        );
        self.max = Vec3::new(
            self.max.x.max(value.x),
            self.max.y.max(value.y),
            self.max.z.max(value.z),
        );
        self.valid = true;
    }

    fn center(&self) -> Vec3 {
        self.min.add(self.max).scale(0.5)
    }

    fn extent(&self) -> Vec3 {
        self.max.sub(self.min)
    }
}

#[derive(Clone, Copy)]
struct Triangle {
    a: Vec3,
    b: Vec3,
    c: Vec3,
    normal: Vec3,
}

impl Triangle {
    fn from_normal(a: Vec3, b: Vec3, c: Vec3, normal: Vec3) -> Option<Self> {
        if normal.length() == 0.0 {
            return None;
        }
        let normal = if normal.y < 0.0 {
            normal.scale(-1.0)
        } else {
            normal
        };
        Some(Self { a, b, c, normal })
    }

    fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let ny = self.normal.y;
        if ny.abs() < 1e-6 {
            return None;
        }
        let d = -self.normal.dot(self.a);
        Some(-(self.normal.x * x + self.normal.z * z + d) / ny)
    }

    fn center(&self) -> Vec3 {
        self.a.add(self.b).add(self.c).scale(1.0 / 3.0)
    }
}

struct SceneCollision {
    floors: Vec<Triangle>,
    planes: Vec<CollisionPlane>,
    clipnodes: Vec<ClipNode>,
    headnode: i32,
}

struct CollisionPlane {
    normal: Vec3,
    dist: f32,
}

struct ClipNode {
    plane_id: i32,
    children: [i32; 2],
}

struct Trace {
    fraction: f32,
    end: Vec3,
    plane_normal: Vec3,
    start_solid: bool,
    all_solid: bool,
}

fn clip_velocity(velocity: Vec3, normal: Vec3, overbounce: f32) -> Vec3 {
    let backoff = velocity.dot(normal) * overbounce;
    velocity.sub(normal.scale(backoff))
}

impl SceneCollision {
    fn spawn_point(&self, bounds: &Bounds, eye_height: f32) -> Option<Vec3> {
        let center = bounds.center();
        let mut best: Option<(f32, Vec3)> = None;
        for tri in &self.floors {
            if tri.normal.y < FLOOR_NORMAL_MIN {
                continue;
            }
            let tri_center = tri.center();
            let y = match tri.height_at(tri_center.x, tri_center.z) {
                Some(y) => y,
                None => continue,
            };
            let dx = tri_center.x - center.x;
            let dz = tri_center.z - center.z;
            let dist2 = dx * dx + dz * dz;
            let candidate = Vec3::new(tri_center.x, y + eye_height, tri_center.z);
            let replace = match best.as_ref() {
                Some((best_dist, _)) => dist2 < *best_dist,
                None => true,
            };
            if replace {
                best = Some((dist2, candidate));
            }
        }
        best.map(|(_, position)| position)
    }

    fn move_with_step(
        &self,
        start: Vec3,
        velocity: Vec3,
        dt: f32,
        step_height: f32,
    ) -> (Vec3, Vec3, bool) {
        let (down_pos, down_vel, down_ground, down_blocked) = self.slide_move(start, velocity, dt);
        if !down_ground || !down_blocked {
            return (down_pos, down_vel, down_ground);
        }

        let up = start.add(Vec3::new(0.0, step_height, 0.0));
        if !self.has_headroom(start, up) {
            return (down_pos, down_vel, down_ground);
        }

        let (step_pos, step_vel, _, _) = self.slide_move(up, velocity, dt);
        let down = step_pos.add(Vec3::new(0.0, -step_height, 0.0));
        let down_trace = self.trace(step_pos, down);
        let step_end = down_trace.end;

        let down_delta = Vec3::new(down_pos.x - start.x, 0.0, down_pos.z - start.z);
        let step_delta = Vec3::new(step_end.x - start.x, 0.0, step_end.z - start.z);
        let down_dist = down_delta.length();
        let step_dist = step_delta.length();
        if step_dist > down_dist {
            let landed = !down_trace.start_solid
                && down_trace.fraction < 1.0
                && down_trace.plane_normal.y > FLOOR_NORMAL_MIN;
            if landed {
                let mut final_vel = step_vel;
                if final_vel.y < 0.0 {
                    final_vel.y = 0.0;
                }
                return (step_end, final_vel, true);
            }
        }

        (down_pos, down_vel, down_ground)
    }

    fn slide_move(&self, start: Vec3, velocity: Vec3, dt: f32) -> (Vec3, Vec3, bool, bool) {
        let mut pos = start;
        let mut vel = velocity;
        let mut time_left = dt;
        let mut planes: Vec<Vec3> = Vec::new();
        let mut on_ground = false;
        let mut blocked = false;

        for _ in 0..4 {
            if vel.length() <= 0.0 {
                break;
            }
            let end = pos.add(vel.scale(time_left));
            let trace = self.trace(pos, end);
            if trace.all_solid {
                return (pos, Vec3::zero(), false, true);
            }
            if trace.fraction > 0.0 {
                pos = trace.end;
            }
            if trace.fraction == 1.0 {
                break;
            }
            blocked = true;

            if trace.plane_normal.y > FLOOR_NORMAL_MIN {
                on_ground = true;
            }
            time_left *= 1.0 - trace.fraction;

            planes.push(trace.plane_normal);
            let original = vel;
            let primal = vel;
            let mut new_vel = Vec3::zero();
            let mut found = false;
            for i in 0..planes.len() {
                let test = clip_velocity(original, planes[i], 1.0);
                let mut ok = true;
                for (j, plane) in planes.iter().enumerate() {
                    if j != i && test.dot(*plane) < 0.0 {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    new_vel = test;
                    found = true;
                    break;
                }
            }
            if !found {
                if planes.len() == 2 {
                    let dir = planes[0].cross(planes[1]).normalize_or_zero();
                    new_vel = dir.scale(dir.dot(primal));
                } else {
                    new_vel = Vec3::zero();
                }
            }
            vel = new_vel;
            if trace.plane_normal.y < -FLOOR_NORMAL_MIN && vel.y > 0.0 {
                vel.y = 0.0;
            }
            if vel.length() <= 1e-6 {
                vel = Vec3::zero();
                break;
            }
        }

        if on_ground && vel.y < 0.0 {
            vel.y = 0.0;
        }
        (pos, vel, on_ground, blocked)
    }

    fn trace(&self, start: Vec3, end: Vec3) -> Trace {
        let mut trace = Trace {
            fraction: 1.0,
            end,
            plane_normal: Vec3::zero(),
            start_solid: false,
            all_solid: true,
        };
        let _ = self.recursive_hull_check(self.headnode, 0.0, 1.0, start, end, &mut trace);
        if trace.start_solid {
            trace.fraction = 0.0;
            trace.end = start;
            return trace;
        }
        if trace.fraction < 1.0 {
            trace.end = start.add(end.sub(start).scale(trace.fraction));
        } else {
            trace.end = end;
        }
        trace
    }

    fn has_headroom(&self, origin: Vec3, up: Vec3) -> bool {
        let trace = self.trace(origin, up);
        !trace.start_solid && trace.fraction >= 1.0
    }

    fn try_unstuck(&self, position: Vec3) -> Vec3 {
        if self.hull_point_contents(self.headnode, position) != CONTENTS_SOLID {
            return position;
        }
        for dy in 1..=64 {
            let candidate = position.add(Vec3::new(0.0, -(dy as f32), 0.0));
            if self.hull_point_contents(self.headnode, candidate) != CONTENTS_SOLID {
                return candidate;
            }
        }
        let steps = [0.0, -2.0, -4.0, -8.0, -16.0, -24.0, -32.0, 1.0, 2.0, 4.0];
        let radii = [1.0, 2.0, 4.0, 8.0, 12.0, 16.0];
        for dy in steps {
            for radius in radii {
                for dx in [-1.0, 0.0, 1.0] {
                    for dz in [-1.0, 0.0, 1.0] {
                        if dx == 0.0 && dz == 0.0 && dy == 0.0 {
                            continue;
                        }
                        let candidate = position.add(Vec3::new(dx * radius, dy, dz * radius));
                        if self.hull_point_contents(self.headnode, candidate) != CONTENTS_SOLID {
                            return candidate;
                        }
                    }
                }
            }
        }
        position
    }

    fn recursive_hull_check(
        &self,
        node: i32,
        start_frac: f32,
        end_frac: f32,
        start: Vec3,
        end: Vec3,
        trace: &mut Trace,
    ) -> bool {
        if node < 0 {
            if node != CONTENTS_SOLID {
                trace.all_solid = false;
                return true;
            }
            trace.start_solid = true;
            return false;
        }
        let clipnode = match self.clipnodes.get(node as usize) {
            Some(node) => node,
            None => return false,
        };
        let plane = match self.planes.get(clipnode.plane_id as usize) {
            Some(plane) => plane,
            None => return false,
        };

        let start_dist = plane.normal.dot(start) - plane.dist;
        let end_dist = plane.normal.dot(end) - plane.dist;

        if start_dist >= 0.0 && end_dist >= 0.0 {
            return self.recursive_hull_check(
                clipnode.children[0],
                start_frac,
                end_frac,
                start,
                end,
                trace,
            );
        }
        if start_dist < 0.0 && end_dist < 0.0 {
            return self.recursive_hull_check(
                clipnode.children[1],
                start_frac,
                end_frac,
                start,
                end,
                trace,
            );
        }

        let mut frac = if start_dist < 0.0 {
            (start_dist + DIST_EPSILON) / (start_dist - end_dist)
        } else {
            (start_dist - DIST_EPSILON) / (start_dist - end_dist)
        };
        frac = frac.clamp(0.0, 1.0);
        let mid_frac = start_frac + (end_frac - start_frac) * frac;
        let mid = start.add(end.sub(start).scale(frac));
        let side = if start_dist < 0.0 { 1 } else { 0 };

        if !self.recursive_hull_check(
            clipnode.children[side],
            start_frac,
            mid_frac,
            start,
            mid,
            trace,
        ) {
            return false;
        }

        if self.hull_point_contents(clipnode.children[side ^ 1], mid) != CONTENTS_SOLID {
            return self.recursive_hull_check(
                clipnode.children[side ^ 1],
                mid_frac,
                end_frac,
                mid,
                end,
                trace,
            );
        }

        if trace.all_solid {
            return false;
        }

        trace.fraction = mid_frac;
        trace.plane_normal = if side == 0 {
            plane.normal
        } else {
            plane.normal.scale(-1.0)
        };
        false
    }

    fn hull_point_contents(&self, node: i32, point: Vec3) -> i32 {
        let mut node = node;
        loop {
            if node < 0 {
                return node;
            }
            let clipnode = match self.clipnodes.get(node as usize) {
                Some(node) => node,
                None => return CONTENTS_SOLID,
            };
            let plane = match self.planes.get(clipnode.plane_id as usize) {
                Some(plane) => plane,
                None => return CONTENTS_SOLID,
            };
            let dist = plane.normal.dot(point) - plane.dist;
            node = if dist >= 0.0 {
                clipnode.children[0]
            } else {
                clipnode.children[1]
            };
        }
    }
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(ArgParseError::Help) => {
            print_usage();
            return;
        }
        Err(ArgParseError::Message(message)) => {
            eprintln!("{}", message);
            print_usage();
            std::process::exit(EXIT_USAGE);
        }
    };

    let ui_regression = args.ui_regression.clone();
    let mut settings = Settings::load();
    if let Some(regression) = ui_regression.as_ref() {
        settings.window_mode = WindowMode::Windowed;
        settings.resolution = regression.resolution;
        settings.ui_scale = regression.ui_scale;
        settings.master_volume = 0.5;
    }
    let mut last_window_mode = settings.window_mode;
    let mut last_resolution = settings.resolution;
    let mut pending_video_prewarm = args.play_movie.is_some() || args.playlist.is_some();
    if ui_regression.is_some() {
        pending_video_prewarm = false;
    }
    let mut boot_settings = settings.clone();
    let mut boot_apply_fullscreen = false;
    if settings.window_mode == WindowMode::Fullscreen {
        boot_settings.window_mode = WindowMode::Borderless;
        boot_apply_fullscreen = true;
    }
    let mut boot = BootState::new(boot_settings.window_mode, Instant::now());
    let (event_loop, window) =
        match create_window("Pallet", settings.resolution[0], settings.resolution[1]) {
            Ok(result) => result,
            Err(err) => {
                eprintln!("window init failed: {}", err);
                std::process::exit(1);
            }
        };
    let window: &'static Window = Box::leak(Box::new(window));
    apply_window_settings(window, &boot_settings);

    let mut renderer = match render_wgpu::Renderer::new(window) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("renderer init failed: {}", err);
            std::process::exit(1);
        }
    };
    renderer.resize(renderer.window_inner_size());
    if let Some(regression) = ui_regression.as_ref() {
        let actual = renderer.size();
        if actual.width != regression.resolution[0] || actual.height != regression.resolution[1] {
            eprintln!(
                "ui regression size mismatch: expected {}x{}, got {}x{}",
                regression.resolution[0], regression.resolution[1], actual.width, actual.height
            );
            std::process::exit(EXIT_USAGE);
        }
    }
    let mut pending_resize_clear = pending_video_prewarm || boot.is_hidden();
    if pending_video_prewarm {
        renderer.set_clear_color_rgba(0.0, 0.0, 0.0, 1.0);
    }
    let mut initial_render_ok = false;
    match renderer.render() {
        Ok(()) => {
            initial_render_ok = true;
        }
        Err(RenderError::Lost | RenderError::Outdated) => {
            renderer.resize(renderer.size());
            if renderer.render().is_ok() {
                initial_render_ok = true;
            }
        }
        Err(RenderError::OutOfMemory) => {
            eprintln!("render error: out of memory");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("render error: {}", err);
        }
    }
    if initial_render_ok {
        boot.on_initial_render(window);
    }
    let main_window_id = renderer.window_id();

    let audio = match AudioEngine::new() {
        Ok(audio) => Some(Rc::new(audio)),
        Err(err) => {
            eprintln!("{}", err);
            None
        }
    };
    let mut sfx_data = None;
    if let (Some(_), Some(quake_dir)) = (audio.as_ref(), args.quake_dir.as_ref()) {
        match load_wav_sfx(quake_dir, DEFAULT_SFX) {
            Ok(data) => sfx_data = Some(data),
            Err(err) => eprintln!("{}", err.message),
        }
    }
    if let Some(audio) = audio.as_ref() {
        audio.set_master_volume(settings.master_volume);
    }

    let mut video: Option<VideoPlayback> = None;
    let mut next_video: Option<VideoPlayback> = None;
    let mut next_video_entry: Option<PlaylistEntry> = None;
    let mut current_video_entry: Option<PlaylistEntry> = None;
    let mut next_video_start_at: Option<Instant> = None;
    let mut next_video_created_at: Option<Instant> = None;
    let mut video_frame_visible = false;
    let mut video_start_delay_until: Option<Instant> = None;
    let mut video_hold_until: Option<Instant> = None;
    let video_debug = std::env::var_os("CRUSTQUAKE_VIDEO_DEBUG").is_some();
    let video_debug_stats = if video_debug {
        Some(Arc::new(VideoDebugStats::new()))
    } else {
        None
    };
    let mut last_video_debug = Instant::now();
    let mut last_underrun_frames = 0u64;
    let mut last_output_frames = 0u64;
    let mut last_video_stats = VideoDebugSnapshot {
        audio_packets: 0,
        audio_frames_in: 0,
        audio_frames_out: 0,
        audio_frames_queued: 0,
        pending_audio_frames: 0,
        audio_sample_rate: 0,
        audio_channels: 0,
        last_audio_ms: 0,
        last_video_ms: 0,
    };
    let mut playlist_entries = if let Some(playlist_path) = args.playlist.as_ref() {
        match load_playlist(playlist_path) {
            Ok(list) => list,
            Err(err) => {
                eprintln!("{}", err.message);
                std::process::exit(err.code);
            }
        }
    } else {
        let mut list = VecDeque::new();
        if let Some(movie_path) = args.play_movie.as_ref() {
            list.push_back(PlaylistEntry::new(
                movie_path.clone(),
                VIDEO_HOLD_LAST_FRAME_MS,
            ));
        }
        list
    };
    if !playlist_entries.is_empty() {
        if let Some(audio) = audio.as_ref() {
            audio.clear_pcm();
        }
        if let Some(entry) = playlist_entries.pop_front() {
            let path = entry.path.clone();
            current_video_entry = Some(entry);
            video = Some(start_video_playback(
                path,
                audio.as_ref(),
                video_debug_stats.clone(),
                VIDEO_PLAYBACK_WARM_MS,
                true,
            ));
            video_frame_visible = false;
        }
        next_video_entry = playlist_entries.pop_front();
        if video.is_some() {
            video_start_delay_until =
                Some(Instant::now() + Duration::from_millis(VIDEO_INTERMISSION_MS));
            if next_video_entry.is_some() {
                next_video_start_at =
                    Some(Instant::now() + Duration::from_millis(VIDEO_PREDECODE_START_DELAY_MS));
            }
            renderer.clear_textured_quad();
            video_frame_visible = false;
        }
    }

    let mut script: Option<ScriptRuntime> = None;
    if let Some(script_path) = args.script.as_ref() {
        let host_state = Rc::new(RefCell::new(ScriptHostState::new(
            args.quake_dir.clone(),
            audio.clone(),
        )));
        let spawn_state = Rc::clone(&host_state);
        let sound_state = Rc::clone(&host_state);
        let callbacks = HostCallbacks {
            spawn_entity: Box::new(move |request| spawn_state.borrow_mut().spawn_entity(request)),
            play_sound: Box::new(move |asset| sound_state.borrow_mut().play_sound(asset)),
            log: Box::new(move |msg| {
                println!("[lua] {}", msg);
            }),
        };
        let mut engine = match ScriptEngine::new(ScriptConfig::default(), callbacks) {
            Ok(engine) => engine,
            Err(err) => {
                eprintln!("script init failed: {}", err);
                std::process::exit(EXIT_USAGE);
            }
        };
        if let Err(err) = engine.load_file(script_path) {
            eprintln!("script load failed: {}", err);
            std::process::exit(EXIT_USAGE);
        }
        script = Some(ScriptRuntime {
            engine,
            _host: host_state,
        });
    }

    let mut input = InputState::default();
    let mut console = ConsoleState::default();
    let mut modifiers = ModifiersState::default();
    let mut last_cursor_pos = PhysicalPosition::new(0.0, 0.0);
    let mut input_router = InputRouter::new();
    let mut ui_facade = UiFacade::new(window, renderer.device(), renderer.surface_format());
    let mut ui_state = UiState::default();
    let text_fonts = TextFontSystem::new();
    let mut text_overlay = TextOverlay::new_with_font_system(
        renderer.device(),
        renderer.queue(),
        renderer.surface_format(),
        &text_fonts,
    );
    let mut hud_overlay = TextOverlay::new_with_font_system(
        renderer.device(),
        renderer.queue(),
        renderer.surface_format(),
        &text_fonts,
    );
    let mut console_log_overlay = TextOverlay::new_with_font_system(
        renderer.device(),
        renderer.queue(),
        renderer.surface_format(),
        &text_fonts,
    );
    let mut console_log_cache = ConsoleLogCache::new(renderer.device(), renderer.surface_format());
    let ui_regression_capture = ui_regression.as_ref().map(|regression| {
        FrameCapture::new(
            renderer.device(),
            regression.resolution,
            renderer.surface_format(),
        )
        .unwrap_or_else(|err| {
            eprintln!("ui regression capture init failed: {}", err);
            std::process::exit(EXIT_UI_REGRESSION);
        })
    });
    let mut ui_regression_done = false;
    let mut hud = HudState::new(Instant::now());
    let mut camera = CameraState::default();
    let mut collision: Option<SceneCollision> = None;
    let mut fly_mode = false;
    let mut scene_active = false;
    let mut loopback: Option<LoopbackNet> = None;
    let mut mouse_look = false;
    let mut mouse_grabbed = false;
    let mut ignore_cursor_move = false;
    let mut was_mouse_look = false;
    let mut pending_map: Option<(PathBuf, String)> = None;

    if let Some(asset) = args.show_image.as_deref() {
        let quake_dir = match args.quake_dir.as_ref() {
            Some(path) => path,
            None => {
                eprintln!("--quake-dir is required when using --show-image");
                print_usage();
                std::process::exit(EXIT_USAGE);
            }
        };

        let image = match load_lmp_image(quake_dir, asset) {
            Ok(image) => image,
            Err(err) => {
                eprintln!("{}", err.message);
                std::process::exit(err.code);
            }
        };

        if let Err(err) = renderer.set_image(image) {
            eprintln!("image upload failed: {}", err);
            std::process::exit(EXIT_IMAGE);
        }
    }

    if let Some(map) = args.map.as_deref() {
        let quake_dir = match args.quake_dir.as_ref() {
            Some(path) => path,
            None => {
                eprintln!("--quake-dir is required when using --map");
                print_usage();
                std::process::exit(EXIT_USAGE);
            }
        };
        pending_map = Some((quake_dir.to_path_buf(), map.to_string()));
    }

    if video.is_none() && args.show_image.is_none() {
        ui_state.open_title();
    } else {
        ui_state.menu_open = false;
    }

    if let Some(regression) = ui_regression.as_ref() {
        setup_ui_regression(
            &mut ui_state,
            &mut console,
            &mut settings,
            regression,
            Instant::now(),
        );
    }

    let mut input_script = if args.input_script {
        Some(InputScript::new(Instant::now(), &settings))
    } else {
        None
    };

    let mut last_frame = Instant::now();
    let exit_code = Rc::new(Cell::new(EXIT_SUCCESS));
    let exit_code_handle = Rc::clone(&exit_code);

    if let Err(err) = event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == main_window_id => {
                let _ = if ui_state.menu_open && !console.is_blocking() {
                    ui_facade.handle_window_event(&event)
                } else {
                    false
                };
                match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    boot.on_resize(Instant::now());
                    if boot.is_hidden() {
                        pending_resize_clear = true;
                    }
                    if pending_resize_clear {
                        match renderer.render() {
                            Ok(()) => {}
                            Err(RenderError::Lost | RenderError::Outdated) => {
                                renderer.resize(renderer.size());
                            }
                            Err(RenderError::OutOfMemory) => {
                                eprintln!("render error: out of memory");
                                elwt.exit();
                            }
                            Err(err) => {
                                eprintln!("render error: {}", err);
                            }
                        }
                        pending_resize_clear = false;
                    }
                }
                WindowEvent::ScaleFactorChanged { .. } => {
                    renderer.resize(renderer.window_inner_size());
                    boot.on_resize(Instant::now());
                    if boot.is_hidden() {
                        pending_resize_clear = true;
                    }
                    if pending_resize_clear {
                        match renderer.render() {
                            Ok(()) => {}
                            Err(RenderError::Lost | RenderError::Outdated) => {
                                renderer.resize(renderer.size());
                            }
                            Err(RenderError::OutOfMemory) => {
                                eprintln!("render error: out of memory");
                                elwt.exit();
                            }
                            Err(err) => {
                                eprintln!("render error: {}", err);
                            }
                        }
                        pending_resize_clear = false;
                    }
                }
                WindowEvent::ModifiersChanged(mods) => {
                    modifiers = mods.state();
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let pressed = event.state == ElementState::Pressed;
                        let is_repeat = event.repeat;
                        if pressed && code == KeyCode::Space && video.is_some() {
                            let advanced = advance_playlist(
                                &mut video,
                                &mut next_video,
                                &mut playlist_entries,
                                audio.as_ref(),
                                video_debug_stats.clone(),
                                &mut next_video_entry,
                                &mut current_video_entry,
                                true,
                            );
                            if !advanced {
                                video_hold_until = None;
                                next_video_entry = None;
                                next_video_start_at = None;
                                next_video_created_at = None;
                                video_start_delay_until = None;
                                video = None;
                                ui_state.open_title();
                                renderer.clear_textured_quad();
                                video_frame_visible = false;
                                return;
                            }
                            video_hold_until = None;
                            video_start_delay_until = Some(
                                Instant::now() + Duration::from_millis(VIDEO_INTERMISSION_MS),
                            );
                            next_video_start_at = next_video_entry
                                .as_ref()
                                .map(|_| Instant::now() + Duration::from_millis(
                                    VIDEO_PREDECODE_START_DELAY_MS,
                                ));
                            next_video_created_at = None;
                            renderer.clear_textured_quad();
                            video_frame_visible = false;
                            if video_debug {
                                last_underrun_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_underrun_frames())
                                    .unwrap_or(0);
                                last_output_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.output_frames())
                                    .unwrap_or(0);
                                last_video_stats = VideoDebugSnapshot {
                                    audio_packets: 0,
                                    audio_frames_in: 0,
                                    audio_frames_out: 0,
                                    audio_frames_queued: 0,
                                    pending_audio_frames: 0,
                                    audio_sample_rate: 0,
                                    audio_channels: 0,
                                    last_audio_ms: 0,
                                    last_video_ms: 0,
                                };
                            }
                            return;
                        }
                        let video_active = video.is_some();
                        let _ = handle_non_video_key_input(
                            code,
                            pressed,
                            is_repeat,
                            &input_router,
                            &mut console,
                            &mut ui_state,
                            window,
                            &mut input,
                            &mut mouse_look,
                            &mut mouse_grabbed,
                            &mut was_mouse_look,
                            scene_active,
                            &mut fly_mode,
                            &mut camera,
                            collision.as_ref(),
                            audio.as_ref(),
                            sfx_data.as_ref(),
                            &mut script,
                            video_active,
                        );
                        let copy_combo = modifiers.control_key() || modifiers.super_key();
                        if pressed
                            && !is_repeat
                            && copy_combo
                            && code == KeyCode::KeyC
                            && !video_active
                            && console.is_interactive()
                            && input_router.active_layer(
                                console.is_blocking(),
                                ui_state.menu_open,
                            ) == InputLayer::Console
                        {
                            if let Some(text) = console.selection_text() {
                                ui_facade.set_clipboard_text(text);
                                console.show_toast("Copied", Instant::now());
                            }
                            return;
                        }
                        if pressed
                            && input_router.active_layer(console.is_blocking(), ui_state.menu_open)
                                == InputLayer::Console
                            && !video_active
                            && console.is_interactive()
                            && !copy_combo
                        {
                            if let Some(text) = event.text.as_deref() {
                                if !matches!(
                                    code,
                                    KeyCode::Backquote
                                        | KeyCode::Escape
                                        | KeyCode::Enter
                                        | KeyCode::NumpadEnter
                                ) {
                                    for ch in text.chars() {
                                        if !ch.is_control() {
                                            console.buffer.push(ch);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                WindowEvent::Ime(Ime::Commit(text)) => {
                    if console.is_interactive() && video.is_none() {
                        console.buffer.push_str(&text);
                    }
                }
                WindowEvent::Ime(_) => {}
                WindowEvent::MouseInput { state, button, .. } => {
                    if console.is_interactive() && button == MouseButton::Left {
                        let click_x = last_cursor_pos.x as f32;
                        let click_y = last_cursor_pos.y as f32;
                        let mut handled_menu = false;
                        if console.menu_position().is_some() {
                            if let Some(bounds) = console.menu_bounds() {
                                if bounds.contains(click_x, click_y) {
                                    if let Some(text) = console.selection_text() {
                                        ui_facade.set_clipboard_text(text);
                                        console.show_toast("Copied", Instant::now());
                                    }
                                    handled_menu = true;
                                }
                            }
                            console.close_menu();
                        }
                        if !handled_menu {
                            match state {
                                ElementState::Pressed => {
                                    if let Some(line) =
                                        console.log_line_at(last_cursor_pos.y as f32, false)
                                    {
                                        console.start_selection(line);
                                    } else {
                                        console.clear_selection();
                                    }
                                }
                                ElementState::Released => {
                                    console.finish_selection();
                                }
                            }
                        }
                    } else if console.is_interactive()
                        && button == MouseButton::Right
                        && state == ElementState::Released
                    {
                        if let Some(line) =
                            console.log_line_at(last_cursor_pos.y as f32, false)
                        {
                            if console.selection_text().is_none() {
                                console.start_selection(line);
                                console.finish_selection();
                            }
                            console.open_menu(TextPosition {
                                x: last_cursor_pos.x as f32,
                                y: last_cursor_pos.y as f32,
                            });
                        } else {
                            console.close_menu();
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    if console.is_interactive() {
                        let delta_px = match delta {
                            MouseScrollDelta::LineDelta(_, y) => {
                                y * console.line_height.max(1.0)
                            }
                            MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                        };
                        if delta_px != 0.0 {
                            console.scroll_by(delta_px);
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    last_cursor_pos = position;
                    if console.is_interactive() && console.is_selecting() {
                        if let Some(line) = console.log_line_at(position.y as f32, true) {
                            console.update_selection(line);
                        }
                    }
                    if input_router.active_layer(console.is_blocking(), ui_state.menu_open)
                        == InputLayer::Game
                        && scene_active
                        && mouse_look
                        && !mouse_grabbed
                    {
                        if ignore_cursor_move {
                            ignore_cursor_move = false;
                        } else {
                            let center = cursor_center(window);
                            let dx = position.x - center.x;
                            let dy = position.y - center.y;
                            if dx != 0.0 || dy != 0.0 {
                                camera.apply_mouse(dx, dy);
                            }
                            ignore_cursor_move = center_cursor(window);
                        }
                    }
                }
                WindowEvent::Focused(false) => {
                    mouse_look = false;
                    mouse_grabbed = set_cursor_mode(window, mouse_look);
                    if console.is_blocking() {
                        close_console(
                            &mut console,
                            &mut ui_state,
                            window,
                            &mut mouse_look,
                            &mut mouse_grabbed,
                            scene_active,
                            false,
                        );
                    }
                }
                WindowEvent::RedrawRequested => {
                    let now = Instant::now();
                    if pending_video_prewarm {
                        renderer.prewarm_yuv_pipeline();
                        pending_video_prewarm = false;
                    }
                    let dt = (now - last_frame).as_secs_f32().min(0.1);
                    last_frame = now;
                    if video.is_some() && console.is_blocking() {
                        console.force_closed();
                        console.buffer.clear();
                        ui_state.console_open = false;
                        window.set_ime_allowed(false);
                    }
                    console.update(now);
                    window.set_ime_allowed(console.is_interactive());
                    let mut finish_input_script = false;
                    if let Some(scripted) = input_script.as_mut() {
                        if scripted.ready(now) {
                            match scripted.step {
                                0 => {
                                    if video.is_some() {
                                        scripted.next_at =
                                            now + Duration::from_millis(INPUT_SCRIPT_STEP_DELAY_MS);
                                    } else if !scene_active {
                                        if let Some((quake_dir, map)) = pending_map.take() {
                                            match enter_map_scene(
                                                &mut renderer,
                                                window,
                                                &quake_dir,
                                                &map,
                                                audio.as_ref(),
                                                &mut camera,
                                                &mut collision,
                                                &mut scene_active,
                                                &mut mouse_look,
                                                &mut mouse_grabbed,
                                                &mut loopback,
                                            ) {
                                                Ok(()) => {
                                                    ui_state.close_menu();
                                                }
                                                Err(err) => {
                                                    eprintln!("{}", err.message);
                                                    pending_map = Some((quake_dir, map));
                                                    finish_input_script = true;
                                                }
                                            }
                                        } else {
                                            if !scripted.reported_missing_map {
                                                eprintln!(
                                                    "input script requires --map and --quake-dir"
                                                );
                                                scripted.reported_missing_map = true;
                                            }
                                            finish_input_script = true;
                                        }
                                        if scene_active {
                                            mouse_look = true;
                                            mouse_grabbed = set_cursor_mode(window, mouse_look);
                                            println!("input script: ready");
                                        }
                                        scripted.advance(now);
                                    } else {
                                        mouse_look = true;
                                        mouse_grabbed = set_cursor_mode(window, mouse_look);
                                        println!("input script: ready");
                                        scripted.advance(now);
                                    }
                                }
                                1 => {
                                    let _ = handle_non_video_key_input(
                                        KeyCode::Escape,
                                        true,
                                        false,
                                        &input_router,
                                        &mut console,
                                        &mut ui_state,
                                        window,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
                                        &mut camera,
                                        collision.as_ref(),
                                        audio.as_ref(),
                                        sfx_data.as_ref(),
                                        &mut script,
                                        video.is_some(),
                                    );
                                    println!("input script: open menu");
                                    scripted.advance(now);
                                }
                                2 => {
                                    ui_state.menu_screen = MenuScreen::Options;
                                    let target_scale = if (settings.ui_scale - 1.25).abs() < 0.01 {
                                        1.0
                                    } else {
                                        1.25
                                    };
                                    settings.ui_scale = target_scale;
                                    if let Err(err) = settings.save() {
                                        eprintln!("settings save failed: {}", err);
                                    }
                                    println!(
                                        "input script: ui scale changed from {:.2} to {:.2}",
                                        scripted.ui_scale_before, settings.ui_scale
                                    );
                                    scripted.ui_scale_before = settings.ui_scale;
                                    scripted.advance(now);
                                }
                                3 => {
                                    let _ = handle_non_video_key_input(
                                        KeyCode::Escape,
                                        true,
                                        false,
                                        &input_router,
                                        &mut console,
                                        &mut ui_state,
                                        window,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
                                        &mut camera,
                                        collision.as_ref(),
                                        audio.as_ref(),
                                        sfx_data.as_ref(),
                                        &mut script,
                                        video.is_some(),
                                    );
                                    println!("input script: close menu");
                                    scripted.advance(now);
                                }
                                4 => {
                                    let _ = handle_non_video_key_input(
                                        KeyCode::Backquote,
                                        true,
                                        false,
                                        &input_router,
                                        &mut console,
                                        &mut ui_state,
                                        window,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
                                        &mut camera,
                                        collision.as_ref(),
                                        audio.as_ref(),
                                        sfx_data.as_ref(),
                                        &mut script,
                                        video.is_some(),
                                    );
                                    println!("input script: open console");
                                    scripted.advance(now);
                                }
                                5 => {
                                    if console.is_interactive() {
                                        console.buffer.push_str("status");
                                        println!("input script: type command");
                                        scripted.advance(now);
                                    }
                                }
                                6 => {
                                    let _ = handle_non_video_key_input(
                                        KeyCode::Enter,
                                        true,
                                        false,
                                        &input_router,
                                        &mut console,
                                        &mut ui_state,
                                        window,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
                                        &mut camera,
                                        collision.as_ref(),
                                        audio.as_ref(),
                                        sfx_data.as_ref(),
                                        &mut script,
                                        video.is_some(),
                                    );
                                    println!("input script: submit command");
                                    scripted.advance(now);
                                }
                                7 => {
                                    let _ = handle_non_video_key_input(
                                        KeyCode::Escape,
                                        true,
                                        false,
                                        &input_router,
                                        &mut console,
                                        &mut ui_state,
                                        window,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
                                        &mut camera,
                                        collision.as_ref(),
                                        audio.as_ref(),
                                        sfx_data.as_ref(),
                                        &mut script,
                                        video.is_some(),
                                    );
                                    println!("input script: close console");
                                    scripted.advance(now);
                                }
                                8 => {
                                    if scene_active {
                                        mouse_look = true;
                                        mouse_grabbed = set_cursor_mode(window, mouse_look);
                                        println!("input script: resume mouse-look");
                                    }
                                    finish_input_script = true;
                                    scripted.advance(now);
                                }
                                _ => {
                                    finish_input_script = true;
                                }
                            }
                        }
                    }
                    if finish_input_script {
                        input_script = None;
                        println!("input script: done");
                    }
                    let mut skip_render = false;
                    let dpi_scale = ui_regression
                        .as_ref()
                        .map(|regression| regression.dpi_scale as f64)
                        .unwrap_or_else(|| window.scale_factor());
                    let resolution =
                        ResolutionModel::new(renderer.size(), dpi_scale, settings.ui_scale);
                    println!(
                        "resolution: physical={}x{} dpi_scale={:.3} ui_scale={:.3} logical={:.2}x{:.2} ui_points={:.2}x{:.2}",
                        resolution.physical_px[0],
                        resolution.physical_px[1],
                        resolution.dpi_scale,
                        resolution.ui_scale,
                        resolution.logical_px[0],
                        resolution.logical_px[1],
                        resolution.ui_points[0],
                        resolution.ui_points[1]
                    );
                    let frame_input = UiFrameInput {
                        dt_seconds: dt,
                        resolution,
                        audio_available: audio.is_some(),
                    };
                    let mut ui_regression_checks =
                        ui_regression.as_ref().map(|_| UiRegressionChecks::new(resolution.ui_points));
                    let mut ui_ctx = ui_facade.begin_frame(frame_input);
                    ui_state.console_open = console.is_blocking();
                    ui_facade.build_ui(&mut ui_ctx, &mut ui_state, &mut settings);
                    if let Some(checks) = ui_regression_checks.as_mut() {
                        checks.record_min_font(egui_min_font_px(&ui_ctx.egui_ctx));
                        checks.record_ui_bounds(ui_ctx.egui_ctx.used_rect());
                    }
                    let ui_draw = ui_facade.end_frame(ui_ctx);
                    input_router.update_ui_focus(
                        ui_draw.output.wants_keyboard,
                        ui_draw.output.wants_pointer,
                    );
                    if ui_draw.output.settings_changed {
                        if ui_regression.is_none() {
                            if let Err(err) = settings.save() {
                                eprintln!("settings save failed: {}", err);
                            }
                        }
                        if let Some(audio) = audio.as_ref() {
                            audio.set_master_volume(settings.master_volume);
                        }
                    }
                    if ui_draw.output.display_settings_changed {
                        apply_window_settings(window, &settings);
                        renderer.resize(renderer.window_inner_size());
                        if settings.window_mode != last_window_mode
                            || settings.resolution != last_resolution
                        {
                            window.set_visible(true);
                            pending_resize_clear = true;
                            last_window_mode = settings.window_mode;
                            last_resolution = settings.resolution;
                        }
                        skip_render = true;
                    }
                    if ui_draw.output.start_requested {
                        if let Some((quake_dir, map)) = pending_map.take() {
                            let result = enter_map_scene(
                                &mut renderer,
                                window,
                                &quake_dir,
                                &map,
                                audio.as_ref(),
                                &mut camera,
                                &mut collision,
                                &mut scene_active,
                                &mut mouse_look,
                                &mut mouse_grabbed,
                                &mut loopback,
                            );
                            match result {
                                Ok(()) => {
                                    ui_state.close_menu();
                                }
                                Err(err) => {
                                    eprintln!("{}", err.message);
                                    pending_map = Some((quake_dir, map));
                                }
                            }
                        } else {
                            eprintln!("start requested but no map specified");
                        }
                    }
                    if ui_draw.output.resume_requested {
                        ui_state.close_menu();
                        mouse_look = was_mouse_look;
                        mouse_grabbed = set_cursor_mode(window, mouse_look);
                    }
                    if ui_draw.output.quit_requested {
                        elwt.exit();
                        return;
                    }
                    if skip_render {
                        renderer.request_redraw();
                        return;
                    }
                    let text_viewport = TextViewport {
                        physical_px: resolution.physical_px,
                        dpi_scale: resolution.dpi_scale,
                        ui_scale: settings.ui_scale,
                    };
                    if let Some(until) = video_start_delay_until {
                        if now >= until {
                            video_start_delay_until = None;
                        }
                    }

                    if let Some(script) = script.as_mut() {
                        if let Err(err) = script.engine.on_tick(dt) {
                            eprintln!("lua on_tick failed: {}", err);
                        }
                    }

                    if next_video.is_none() {
                        let should_start = next_video_start_at
                            .map(|start_at| now >= start_at)
                            .unwrap_or(false);
                        if should_start {
                            if let (Some(entry), Some(current)) =
                                (next_video_entry.as_ref(), video.as_ref())
                            {
                                let buffered_audio_ms = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_buffered_ms())
                                    .unwrap_or(0);
                                let ready_for_predecode = current.is_started()
                                    && current.elapsed_ms() >= VIDEO_PREDECODE_MIN_ELAPSED_MS
                                    && current.frame_queue_len() >= VIDEO_PREDECODE_MIN_FRAMES
                                    && buffered_audio_ms >= VIDEO_PREDECODE_MIN_AUDIO_MS;
                                if ready_for_predecode {
                                    next_video = Some(start_video_playback(
                                        entry.path.clone(),
                                        audio.as_ref(),
                                        video_debug_stats.clone(),
                                        VIDEO_PREDECODE_WARM_MS,
                                        true,
                                    ));
                                    next_video_created_at = Some(now);
                                    next_video_start_at = None;
                                }
                            }
                        }
                    }

                    if let Some(preload) = next_video.as_mut() {
                        if let Some(created_at) = next_video_created_at {
                            let elapsed = now.saturating_duration_since(created_at);
                            let elapsed_ms = elapsed.as_millis() as u64;
                            let start_ms =
                                VIDEO_PREDECODE_WARM_MS.min(VIDEO_MAX_QUEUED_MS_PREDECODE);
                            let target_ms = if elapsed_ms >= VIDEO_PREDECODE_RAMP_MS {
                                VIDEO_MAX_QUEUED_MS_PREDECODE
                            } else {
                                let full_ms = VIDEO_MAX_QUEUED_MS_PREDECODE;
                                let ramp = (full_ms - start_ms)
                                    .saturating_mul(elapsed_ms)
                                    / VIDEO_PREDECODE_RAMP_MS.max(1);
                                start_ms + ramp
                            };
                            preload.set_max_queued_video_ms(target_ms);
                        }
                        if let Err(err) = preload.drain_events() {
                            eprintln!("video preload failed: {}", err);
                            preload.stop();
                            next_video = None;
                            next_video_created_at = None;
                        }
                    }

                    let delay_active = video_start_delay_until.is_some();
                    let mut advance_video = false;
                    if let Some(video) = video.as_mut() {
                        if !video_frame_visible {
                            renderer.clear_textured_quad();
                        }
                        if let Err(err) = video.drain_events() {
                            eprintln!("{}", err);
                            video.stop();
                            if let Some(audio) = audio.as_ref() {
                                audio.clear_pcm();
                            }
                        }
                        if let Some(audio) = audio.as_ref() {
                            if video.take_audio_finished() {
                                audio.clear_pcm();
                            }
                        }
                        if video.is_started() {
                            let elapsed = video.elapsed_ms();
                            let target_ms = if elapsed >= VIDEO_PLAYBACK_WARM_UP_MS {
                                VIDEO_MAX_QUEUED_MS_PLAYBACK
                            } else {
                                let start_ms = VIDEO_PLAYBACK_WARM_MS;
                                let full_ms = VIDEO_MAX_QUEUED_MS_PLAYBACK;
                                let ramp = (full_ms - start_ms)
                                    .saturating_mul(elapsed)
                                    / VIDEO_PLAYBACK_WARM_UP_MS.max(1);
                                start_ms + ramp
                            };
                            video.set_max_queued_video_ms(target_ms);
                        }
                        if video_debug {
                            let now = Instant::now();
                            if now.duration_since(last_video_debug) >= Duration::from_secs(1) {
                                let buffered_ms = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_buffered_ms())
                                    .unwrap_or(0);
                                let underrun_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_underrun_frames())
                                    .unwrap_or(0);
                                let output_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.output_frames())
                                    .unwrap_or(0);
                                let delta_output_frames =
                                    output_frames.saturating_sub(last_output_frames);
                                last_output_frames = output_frames;
                                let delta_underrun =
                                    underrun_frames.saturating_sub(last_underrun_frames);
                                last_underrun_frames = underrun_frames;
                                if let Some(snapshot) = video.debug_snapshot() {
                                    let delta_packets = snapshot
                                        .audio_packets
                                        .saturating_sub(last_video_stats.audio_packets);
                                    let delta_in = snapshot
                                        .audio_frames_in
                                        .saturating_sub(last_video_stats.audio_frames_in);
                                    let delta_out = snapshot
                                        .audio_frames_out
                                        .saturating_sub(last_video_stats.audio_frames_out);
                                    let delta_queued = snapshot
                                        .audio_frames_queued
                                        .saturating_sub(last_video_stats.audio_frames_queued);
                                    last_video_stats = snapshot;
                                    let device_rate = audio
                                        .as_ref()
                                        .map(|engine| engine.output_sample_rate())
                                        .unwrap_or(0);
                                    let device_channels = audio
                                        .as_ref()
                                        .map(|engine| engine.output_channels())
                                        .unwrap_or(0);
                                    println!(
                                        "video dbg: elapsed={}ms frames={} buffered={}ms underrun_frames+={} total_underrun_frames={} device_frames+={} device_rate={} device_channels={} audio_packets+={} audio_frames_in+={} audio_frames_out+={} audio_frames_queued+={} pending_frames={} packet_rate={} packet_channels={} last_audio_ms={} last_video_ms={}",
                                        video.elapsed_ms(),
                                        video.frame_queue_len(),
                                        buffered_ms,
                                        delta_underrun,
                                        underrun_frames,
                                        delta_output_frames,
                                        device_rate,
                                        device_channels,
                                        delta_packets,
                                        delta_in,
                                        delta_out,
                                        delta_queued,
                                        last_video_stats.pending_audio_frames,
                                        last_video_stats.audio_sample_rate,
                                        last_video_stats.audio_channels,
                                        last_video_stats.last_audio_ms,
                                        last_video_stats.last_video_ms
                                    );
                                } else {
                                    println!(
                                        "video dbg: elapsed={}ms frames={} buffered={}ms underrun_frames+={} total_underrun_frames={} device_frames+={}",
                                        video.elapsed_ms(),
                                        video.frame_queue_len(),
                                        buffered_ms,
                                        delta_underrun,
                                        underrun_frames,
                                        delta_output_frames
                                    );
                                }
                                last_video_debug = now;
                            }
                        }
                        if !delay_active {
                            if !video.is_started() {
                                let _ = video.preview_frame();
                                if let Some(audio) = audio.as_ref() {
                                    if video.has_frames()
                                        && video.frame_queue_len() >= VIDEO_START_MIN_FRAMES
                                        && video.is_ready_to_start()
                                        && video.prebuffered_audio_ms()
                                            >= VIDEO_AUDIO_PREBUFFER_MS
                                    {
                                        video.start_with_clock(audio.clock().time_ms());
                                    }
                                } else if video.has_frames()
                                    && video.is_ready_to_start()
                                    && video.frame_queue_len() >= VIDEO_START_MIN_FRAMES
                                {
                                    video.start_now();
                                }
                            }
                            if video.is_started() {
                                let elapsed_ms = video.elapsed_ms();
                                if let Some(frame) = video.next_frame(elapsed_ms) {
                                    if let Ok(image) = YuvImageView::new(
                                        frame.width,
                                        frame.height,
                                        frame.y_plane(),
                                        frame.u_plane(),
                                        frame.v_plane(),
                                    ) {
                                        if let Err(err) = renderer.update_yuv_image_view(&image) {
                                            eprintln!("video frame upload failed: {}", err);
                                            video_frame_visible = false;
                                            renderer.clear_textured_quad();
                                        } else {
                                            video.mark_frame_uploaded();
                                            video_frame_visible = true;
                                        }
                                    } else {
                                        video_frame_visible = false;
                                        renderer.clear_textured_quad();
                                    }
                                }
                            }
                        }
                        if video.is_finished() {
                            let hold_ms = current_video_entry
                                .as_ref()
                                .map(|entry| entry.hold_ms)
                                .unwrap_or(VIDEO_HOLD_LAST_FRAME_MS);
                            if hold_ms > 0 {
                                match video_hold_until {
                                    Some(until) => {
                                        if now >= until {
                                            advance_video = true;
                                        }
                                    }
                                    None => {
                                        video_hold_until = Some(
                                            now + Duration::from_millis(hold_ms),
                                        );
                                    }
                                }
                            } else {
                                advance_video = true;
                            }
                        }
                    }
                    if advance_video {
                        let advanced = advance_playlist(
                            &mut video,
                            &mut next_video,
                            &mut playlist_entries,
                            audio.as_ref(),
                            video_debug_stats.clone(),
                            &mut next_video_entry,
                            &mut current_video_entry,
                            true,
                        );
                        if !advanced {
                            video_hold_until = None;
                            next_video_entry = None;
                            next_video_start_at = None;
                            next_video_created_at = None;
                            video_start_delay_until = None;
                            video = None;
                            ui_state.open_title();
                            renderer.clear_textured_quad();
                            video_frame_visible = false;
                            return;
                        }
                        video_hold_until = None;
                        video_start_delay_until = Some(
                            Instant::now() + Duration::from_millis(VIDEO_INTERMISSION_MS),
                        );
                        next_video_start_at = next_video_entry
                            .as_ref()
                            .map(|_| Instant::now() + Duration::from_millis(
                                VIDEO_PREDECODE_START_DELAY_MS,
                            ));
                        next_video_created_at = None;
                        renderer.clear_textured_quad();
                        video_frame_visible = false;
                        if video_debug {
                            last_underrun_frames = audio
                                .as_ref()
                                .map(|engine| engine.pcm_underrun_frames())
                                .unwrap_or(0);
                            last_output_frames = audio
                                .as_ref()
                                .map(|engine| engine.output_frames())
                                .unwrap_or(0);
                            last_video_stats = VideoDebugSnapshot {
                                audio_packets: 0,
                                audio_frames_in: 0,
                                audio_frames_out: 0,
                                audio_frames_queued: 0,
                                pending_audio_frames: 0,
                                audio_sample_rate: 0,
                                audio_channels: 0,
                                last_audio_ms: 0,
                                last_video_ms: 0,
                            };
                        }
                    }

                    if scene_active {
                        camera.update(&input, dt, collision.as_ref(), fly_mode);
                        let aspect = aspect_ratio(renderer.size());
                        renderer.update_camera(camera.view_proj(aspect));
                        if let Some(loopback_net) = loopback.as_mut() {
                            if let Err(err) = loopback_net.tick(&input, &camera) {
                                eprintln!("loopback tick failed: {}", err);
                                loopback = None;
                            }
                        }
                    }

                    let font_scale = (resolution.dpi_scale * settings.ui_scale).max(0.1);
                    let pre_video_blackout = video.is_some() && !video_frame_visible;
                    let show_hud = if ui_regression.is_some() {
                        true
                    } else {
                        scene_active && !ui_state.menu_open
                    };
                    if !pre_video_blackout && show_hud {
                        let hud_font_px = (HUD_FONT_SIZE * font_scale).round().max(1.0);
                        let hud_small_px = (HUD_FONT_SIZE_SMALL * font_scale).round().max(1.0);
                        let hud_style = TextStyle {
                            font_size: hud_font_px,
                            color: HUD_TEXT_COLOR,
                        };
                        let hud_small_style = TextStyle {
                            font_size: hud_small_px,
                            color: HUD_TEXT_COLOR,
                        };
                        let (fps, sim_rate, net_rate) = if ui_regression.is_some() {
                            (UI_REGRESSION_FPS, UI_REGRESSION_SIM_RATE, UI_REGRESSION_NET_RATE)
                        } else {
                            (
                                hud.update(now),
                                1.0 / dt.max(0.001),
                                if loopback.is_some() { 60.0 } else { 0.0 },
                            )
                        };
                        let hud_stats_text = format!(
                            "fps: {:>4.0}\nsim: {:>4.0} hz\nnet: {:>4.0} hz",
                            fps, sim_rate, net_rate
                        );
                        let hud_margin = (16.0 * font_scale).round().max(1.0);
                        let hud_line_height =
                            (hud_font_px * LINE_HEIGHT_SCALE).round().max(1.0);
                        let build_line_height =
                            (hud_small_px * LINE_HEIGHT_SCALE).round().max(1.0);
                        let hud_total_height = hud_line_height * 3.0 + build_line_height;
                        let hud_origin = TextPosition {
                            x: hud_margin,
                            y: (resolution.physical_px[1] as f32
                                - hud_margin
                                - hud_total_height)
                                .max(hud_margin)
                                .round(),
                        };
                        hud_overlay.queue(
                            TextLayer::Hud,
                            hud_style,
                            hud_origin,
                            TextBounds {
                                width: resolution.physical_px[0] as f32,
                                height: hud_line_height * 3.0,
                            },
                            hud_stats_text,
                        );
                        let build_text = format!("build: {}", env!("CARGO_PKG_VERSION"));
                        hud_overlay.queue(
                            TextLayer::Hud,
                            hud_small_style,
                            TextPosition {
                                x: hud_origin.x,
                                y: hud_origin.y + hud_line_height * 3.0,
                            },
                            TextBounds {
                                width: resolution.physical_px[0] as f32,
                                height: build_line_height,
                            },
                            build_text,
                        );
                        if let Some(checks) = ui_regression_checks.as_mut() {
                            checks.record_min_font(hud_font_px);
                            checks.record_min_font(hud_small_px);
                            let hud_ok = hud_origin.x >= 0.0
                                && hud_origin.y >= 0.0
                                && hud_origin.y + hud_total_height
                                    <= resolution.physical_px[1] as f32;
                            checks.record_hud_bounds(hud_ok);
                        }
                    }

                    let mut console_log_update: Option<ConsoleLogUpdate> = None;
                    let mut console_log_draw: Option<ConsoleLogRect> = None;
                    console.set_log_area(None);
                    console.set_menu_bounds(None);
                    if !pre_video_blackout && console.is_visible() {
                        let console_font_px = (CONSOLE_FONT_SIZE * font_scale).round().max(1.0);
                        let console_width = resolution.physical_px[0] as f32;
                        let full_height = (resolution.physical_px[1] as f32
                            * CONSOLE_HEIGHT_RATIO)
                            .max(console_font_px * 2.0)
                            .round();
                        let console_height =
                            (full_height * console.height_ratio()).max(1.0).round();
                        let layout_console_height = if console.is_interactive() {
                            console_height
                        } else {
                            full_height
                        };
                        if let Some(checks) = ui_regression_checks.as_mut() {
                            checks.record_min_font(console_font_px);
                        }
                        text_overlay.queue_rect(
                            TextLayer::ConsoleBackground,
                            TextPosition { x: 0.0, y: 0.0 },
                            TextBounds {
                                width: console_width,
                                height: console_height,
                            },
                            CONSOLE_BG_COLOR,
                        );
                        let padding = (CONSOLE_PADDING * font_scale).round().max(1.0);
                        let input_padding =
                            (CONSOLE_INPUT_PADDING * font_scale).round().max(0.0);
                        let text_left = (padding * 0.25).round().max(1.0);
                        let text_width =
                            (console_width - text_left - padding).max(1.0).round();
                        let text_height = (console_height - padding * 2.0).max(0.0);
                        if text_height > 0.0 {
                            let console_style = TextStyle {
                                font_size: console_font_px,
                                color: CONSOLE_TEXT_COLOR,
                            };
                            let line_height =
                                (console_font_px * LINE_HEIGHT_SCALE).round().max(1.0);
                            console.line_height = line_height;
                            let log_y = padding;
                            let input_y =
                                (console_height - input_padding - line_height).max(log_y).round();
                            let layout_input_y =
                                (layout_console_height - input_padding - line_height)
                                    .max(log_y)
                                    .round();
                            let input_visible = console_height > input_y + line_height;
                            if let Some(checks) = ui_regression_checks.as_mut() {
                                checks.record_console_input(input_visible);
                            }
                            let separator_thickness = font_scale.round().max(1.0);
                            let separator_gap = (0.25 * font_scale).round().max(0.0);
                            let separator_y = (input_y - separator_gap - separator_thickness)
                                .max(log_y)
                                .round();
                            let layout_separator_y = (layout_input_y
                                - separator_gap
                                - separator_thickness)
                                .max(log_y)
                                .round();
                            if console_height > input_y + line_height {
                                let input_box_top =
                                    (separator_y + separator_thickness).min(console_height);
                                let input_box_height =
                                    (console_height - input_box_top).max(0.0);
                                if input_box_height > 0.0 {
                                    text_overlay.queue_rect(
                                        TextLayer::ConsoleBackground,
                                        TextPosition {
                                            x: 0.0,
                                            y: input_box_top,
                                        },
                                        TextBounds {
                                            width: console_width,
                                            height: input_box_height,
                                        },
                                        CONSOLE_INPUT_BG_COLOR,
                                    );
                                }
                                text_overlay.queue_rect(
                                    TextLayer::ConsoleBackground,
                                    TextPosition {
                                        x: 0.0,
                                        y: separator_y,
                                    },
                                    TextBounds {
                                        width: console_width,
                                        height: separator_thickness,
                                    },
                                    CONSOLE_SEPARATOR_COLOR,
                                );
                            }
                            if console_height >= separator_thickness {
                                text_overlay.queue_rect(
                                    TextLayer::ConsoleBackground,
                                    TextPosition {
                                        x: 0.0,
                                        y: console_height - separator_thickness,
                                    },
                                    TextBounds {
                                        width: console_width,
                                        height: separator_thickness,
                                    },
                                    CONSOLE_SEPARATOR_COLOR,
                                );
                            }

                            let log_height = (separator_y - log_y).max(0.0);
                            let max_lines = (log_height / line_height).floor() as usize;
                            let log_text_height = line_height * max_lines as f32;
                            let layout_log_height = (layout_separator_y - log_y).max(0.0);
                            let layout_max_lines =
                                (layout_log_height / line_height).floor() as usize;
                            let layout_log_text_height = line_height * layout_max_lines as f32;
                            if log_text_height > 0.0 {
                                console.set_log_area(Some(ConsoleLogArea {
                                    y: log_y,
                                    height: log_text_height,
                                }));
                            }
                            console.visible_lines = max_lines;
                            let content_height = line_height * console.log.len() as f32;
                            let max_offset =
                                (content_height - layout_log_text_height).max(0.0);
                            console.scroll_offset =
                                console.scroll_offset.clamp(0.0, max_offset);
                            if layout_max_lines > 0 && layout_console_height > log_y {
                                let scroll_px = console.scroll_offset.round();
                                let scroll_lines = (scroll_px / line_height).floor() as usize;
                                let scroll_remainder =
                                    scroll_px - scroll_lines as f32 * line_height;
                                let extra_line = if scroll_remainder > 0.0 {
                                    1usize
                                } else {
                                    0usize
                                };
                                let draw_lines =
                                    (layout_max_lines + extra_line).min(console.log.len());
                                let start = console
                                    .log
                                    .len()
                                    .saturating_sub(draw_lines + scroll_lines);
                                let y_offset = if extra_line == 1 {
                                    (line_height - scroll_remainder).round()
                                } else {
                                    0.0
                                };
                                if draw_lines > 0 {
                                    let scroll_px_u32 = scroll_px.max(0.0) as u32;
                                    let line_height_px = line_height.round().max(1.0) as u32;
                                    let font_px = console_font_px.round().max(1.0) as u32;
                                    let log_size = [
                                        text_width.round().max(1.0) as u32,
                                        layout_log_text_height.round().max(1.0) as u32,
                                    ];
                                    let selection = console
                                        .selection_range_limited(CONSOLE_SELECTION_LINE_LIMIT);
                                    let params = ConsoleLogParams {
                                        revision: console.log_revision,
                                        start,
                                        draw_lines,
                                        scroll_px: scroll_px_u32,
                                        line_height_px,
                                        font_px,
                                        selection,
                                    };
                                    if console_log_cache.needs_update(params, log_size) {
                                        if let Some((sel_start, sel_end)) = selection {
                                            let visible_start = start;
                                            let visible_end =
                                                start + draw_lines.saturating_sub(1);
                                            if sel_start <= visible_end
                                                && sel_end >= visible_start
                                            {
                                                let highlight_start =
                                                    sel_start.max(visible_start);
                                                let highlight_end =
                                                    sel_end.min(visible_end);
                                                for line_index in
                                                    highlight_start..=highlight_end
                                                {
                                                    let line_offset =
                                                        line_index.saturating_sub(start);
                                                    let rect_top =
                                                        -y_offset
                                                            + line_offset as f32 * line_height;
                                                    let rect_bottom =
                                                        rect_top + line_height;
                                                    let clipped_top = rect_top.max(0.0);
                                                    let clipped_bottom =
                                                        rect_bottom.min(layout_log_text_height);
                                                    if clipped_bottom > clipped_top {
                                                        console_log_overlay.queue_rect(
                                                            TextLayer::ConsoleLog,
                                                            TextPosition {
                                                                x: 0.0,
                                                                y: clipped_top,
                                                            },
                                                            TextBounds {
                                                                width: text_width,
                                                                height: clipped_bottom
                                                                    - clipped_top,
                                                            },
                                                            CONSOLE_SELECTION_COLOR,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        let visible_text =
                                            console.visible_text(start, draw_lines);
                                        if !visible_text.is_empty() {
                                            console_log_overlay.queue(
                                                TextLayer::ConsoleLog,
                                                console_style,
                                                TextPosition {
                                                    x: 0.0,
                                                    y: -y_offset,
                                                },
                                                TextBounds {
                                                    width: text_width,
                                                    height: (layout_log_text_height + y_offset)
                                                        .max(1.0),
                                                },
                                                visible_text,
                                            );
                                        }
                                        console_log_update = Some(ConsoleLogUpdate {
                                            params,
                                            size: log_size,
                                            viewport: TextViewport {
                                                physical_px: log_size,
                                                dpi_scale: 1.0,
                                                ui_scale: 1.0,
                                            },
                                        });
                                    }
                                    if log_text_height > 0.0 {
                                        console_log_draw = Some(ConsoleLogRect {
                                            x: text_left,
                                            y: log_y,
                                            width: text_width,
                                            height: log_text_height,
                                        });
                                    }
                                }
                            }
                            if let Some(menu_pos) = console.menu_position() {
                                let menu_label = "Copy";
                                let menu_padding =
                                    (CONSOLE_MENU_PADDING * font_scale).round().max(1.0);
                                let menu_text_width = (menu_label.len() as f32
                                    * console_font_px
                                    * CONSOLE_MENU_CHAR_WIDTH)
                                    .round()
                                    .max(1.0);
                                let menu_width = (menu_text_width + menu_padding * 2.0)
                                    .max(line_height * 2.0)
                                    .round();
                                let menu_height =
                                    (line_height + menu_padding * 2.0).round().max(1.0);
                                let menu_x = menu_pos
                                    .x
                                    .clamp(0.0, (console_width - menu_width).max(0.0))
                                    .round();
                                let menu_y = menu_pos
                                    .y
                                    .clamp(0.0, (console_height - menu_height).max(0.0))
                                    .round();
                                console.set_menu_bounds(Some(ConsoleMenuBounds {
                                    x: menu_x,
                                    y: menu_y,
                                    width: menu_width,
                                    height: menu_height,
                                }));
                                text_overlay.queue_rect(
                                    TextLayer::ConsoleMenu,
                                    TextPosition { x: menu_x, y: menu_y },
                                    TextBounds {
                                        width: menu_width,
                                        height: menu_height,
                                    },
                                    CONSOLE_MENU_BG_COLOR,
                                );
                                text_overlay.queue(
                                    TextLayer::ConsoleMenu,
                                    TextStyle {
                                        font_size: console_font_px,
                                        color: CONSOLE_MENU_TEXT_COLOR,
                                    },
                                    TextPosition {
                                        x: (menu_x + menu_padding).round(),
                                        y: (menu_y + menu_padding).round(),
                                    },
                                    TextBounds {
                                        width: (menu_width - menu_padding * 2.0).max(1.0),
                                        height: line_height,
                                    },
                                    menu_label,
                                );
                            }
                            if let Some(toast) = console.toast.as_ref() {
                                if now < toast.expires_at {
                                    let toast_padding =
                                        (CONSOLE_MENU_PADDING * font_scale).round().max(1.0);
                                    let toast_text_width = (toast.text.len() as f32
                                        * console_font_px
                                        * CONSOLE_MENU_CHAR_WIDTH)
                                        .round()
                                        .max(1.0);
                                    let toast_width =
                                        (toast_text_width + toast_padding * 2.0).round();
                                    let toast_height =
                                        (line_height + toast_padding * 2.0).round().max(1.0);
                                    let toast_x = (console_width - toast_width - padding)
                                        .max(0.0)
                                        .round();
                                    let toast_y = (log_y + padding)
                                        .min((console_height - toast_height).max(0.0))
                                        .round();
                                    text_overlay.queue_rect(
                                        TextLayer::ConsoleMenu,
                                        TextPosition {
                                            x: toast_x,
                                            y: toast_y,
                                        },
                                        TextBounds {
                                            width: toast_width,
                                            height: toast_height,
                                        },
                                        CONSOLE_MENU_BG_COLOR,
                                    );
                                    text_overlay.queue(
                                        TextLayer::ConsoleMenu,
                                        TextStyle {
                                            font_size: console_font_px,
                                            color: CONSOLE_MENU_TEXT_COLOR,
                                        },
                                        TextPosition {
                                            x: (toast_x + toast_padding).round(),
                                            y: (toast_y + toast_padding).round(),
                                        },
                                        TextBounds {
                                            width: (toast_width - toast_padding * 2.0).max(1.0),
                                            height: line_height,
                                        },
                                        toast.text.clone(),
                                    );
                                }
                            }
                            if console_height > input_y + line_height {
                                let caret = if console.is_interactive()
                                    && console.caret_visible(now)
                                {
                                    "|"
                                } else {
                                    " "
                                };
                                let input_line = format!("> {}{}", console.buffer, caret);
                                text_overlay.queue(
                                    TextLayer::Console,
                                    console_style,
                                    TextPosition {
                                        x: text_left,
                                        y: input_y,
                                    },
                                    TextBounds {
                                        width: text_width,
                                        height: line_height,
                                    },
                                    input_line,
                                );
                            }
                        }
                    }

                    let draw_ui = ui_state.menu_open;
                    let draw_text_overlay = !pre_video_blackout;
                    let render_overlay = |device: &wgpu::Device,
                                          queue: &wgpu::Queue,
                                          encoder: &mut wgpu::CommandEncoder,
                                          view: &wgpu::TextureView,
                                          _format: wgpu::TextureFormat| {
                        if let Some(update) = console_log_update {
                            console_log_cache.update(
                                device,
                                queue,
                                encoder,
                                &mut console_log_overlay,
                                update,
                            );
                        }
                        ui_facade.render(device, queue, encoder, view, &ui_draw, draw_ui);
                        if draw_text_overlay {
                            {
                                let mut pass =
                                    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                        label: Some("pallet.text_overlay.background.pass"),
                                        color_attachments: &[Some(
                                            wgpu::RenderPassColorAttachment {
                                                view,
                                                resolve_target: None,
                                                ops: wgpu::Operations {
                                                    load: wgpu::LoadOp::Load,
                                                    store: wgpu::StoreOp::Store,
                                                },
                                            },
                                        )],
                                        depth_stencil_attachment: None,
                                        occlusion_query_set: None,
                                        timestamp_writes: None,
                                    });
                                hud_overlay.flush_layers(
                                    &mut pass,
                                    text_viewport,
                                    device,
                                    queue,
                                    &[TextLayer::Hud],
                                );
                                text_overlay.flush_layers(
                                    &mut pass,
                                    text_viewport,
                                    device,
                                    queue,
                                    &[TextLayer::ConsoleBackground],
                                );
                            }
                            let mut pass =
                                encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                    label: Some("pallet.text_overlay.foreground.pass"),
                                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                        view,
                                        resolve_target: None,
                                        ops: wgpu::Operations {
                                            load: wgpu::LoadOp::Load,
                                            store: wgpu::StoreOp::Store,
                                        },
                                    })],
                                    depth_stencil_attachment: None,
                                    occlusion_query_set: None,
                                    timestamp_writes: None,
                                });
                            if let Some(log_rect) = console_log_draw {
                                console_log_cache.draw(
                                    queue,
                                    &mut pass,
                                    log_rect,
                                    resolution.physical_px,
                                );
                            }
                            text_overlay.flush_layers(
                                &mut pass,
                                text_viewport,
                                device,
                                queue,
                                &[TextLayer::Console, TextLayer::ConsoleMenu, TextLayer::Ui],
                            );
                        }
                    };

                    if let Some(capture) = ui_regression_capture.as_ref() {
                        if ui_regression_done {
                            return;
                        }
                        match renderer.render_with_overlay_and_capture(render_overlay, capture) {
                            Ok(()) => {
                                if boot.on_present(now, window) && boot_apply_fullscreen {
                                    apply_window_settings(window, &settings);
                                    renderer.resize(renderer.window_inner_size());
                                    pending_resize_clear = true;
                                    boot_apply_fullscreen = false;
                                }
                            }
                            Err(RenderCaptureError::Surface(err)) => match err {
                                RenderError::Lost | RenderError::Outdated => {
                                    renderer.resize(renderer.size());
                                }
                                RenderError::OutOfMemory => {
                                    eprintln!("ui regression render failed: out of memory");
                                    exit_code_handle.set(EXIT_UI_REGRESSION);
                                    elwt.exit();
                                    return;
                                }
                                _ => {
                                    eprintln!("ui regression render failed: {}", err);
                                    exit_code_handle.set(EXIT_UI_REGRESSION);
                                    elwt.exit();
                                    return;
                                }
                            },
                            Err(RenderCaptureError::Capture(err)) => {
                                eprintln!("ui regression capture encode failed: {}", err);
                                exit_code_handle.set(EXIT_UI_REGRESSION);
                                elwt.exit();
                                return;
                            }
                        }
                        let rgba = match capture.read_rgba(renderer.device()) {
                            Ok(data) => data,
                            Err(err) => {
                                eprintln!("ui regression capture failed: {}", err);
                                exit_code_handle.set(EXIT_UI_REGRESSION);
                                elwt.exit();
                                return;
                            }
                        };
                        let shot_path = ui_regression
                            .as_ref()
                            .map(|regression| regression.shot_path.clone())
                            .unwrap_or_else(|| PathBuf::from("ui_regression.png"));
                        if let Err(err) = write_png(
                            &shot_path,
                            resolution.physical_px[0],
                            resolution.physical_px[1],
                            &rgba,
                        ) {
                            eprintln!("ui regression write failed: {}", err);
                            exit_code_handle.set(EXIT_UI_REGRESSION);
                            elwt.exit();
                            return;
                        }
                        if let Some(checks) = ui_regression_checks.as_ref() {
                            if let Err(err) = checks.validate() {
                                eprintln!("ui regression invariant failed: {}", err);
                                exit_code_handle.set(EXIT_UI_REGRESSION);
                            }
                        }
                        ui_regression_done = true;
                        elwt.exit();
                        return;
                    }

                    match renderer.render_with_overlay(render_overlay) {
                        Ok(()) => {
                            if boot.on_present(now, window) && boot_apply_fullscreen {
                                apply_window_settings(window, &settings);
                                renderer.resize(renderer.window_inner_size());
                                pending_resize_clear = true;
                                boot_apply_fullscreen = false;
                            }
                        }
                        Err(RenderError::Lost | RenderError::Outdated) => {
                            renderer.resize(renderer.size());
                        }
                        Err(RenderError::OutOfMemory) => {
                            eprintln!("render error: out of memory");
                            elwt.exit();
                        }
                        Err(err) => {
                            eprintln!("render error: {}", err);
                        }
                    }
                }
                    _ => {}
                }
            }
            Event::DeviceEvent { event, .. } => {
                if input_router.active_layer(console.is_blocking(), ui_state.menu_open)
                    == InputLayer::Game
                    && scene_active
                    && mouse_look
                    && mouse_grabbed
                {
                    if let DeviceEvent::MouseMotion { delta } = event {
                        camera.apply_mouse(delta.0, delta.1);
                    }
                }
            }
            Event::AboutToWait => {
                renderer.request_redraw();
                if boot.is_hidden() {
                    let now = Instant::now();
                    if boot.needs_warmup(now) {
                        match renderer.render() {
                            Ok(()) => {
                                if boot.on_present(now, window) && boot_apply_fullscreen {
                                    apply_window_settings(window, &settings);
                                    renderer.resize(renderer.window_inner_size());
                                    pending_resize_clear = true;
                                    boot_apply_fullscreen = false;
                                }
                            }
                            Err(RenderError::Lost | RenderError::Outdated) => {
                                renderer.resize(renderer.size());
                            }
                            Err(RenderError::OutOfMemory) => {
                                eprintln!("render error: out of memory");
                                elwt.exit();
                            }
                            Err(err) => {
                                eprintln!("render error: {}", err);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }) {
        eprintln!("event loop exited with error: {}", err);
    }
    let code = exit_code.get();
    if code != EXIT_SUCCESS {
        std::process::exit(code);
    }
}

fn parse_args() -> Result<CliArgs, ArgParseError> {
    let mut quake_dir = None;
    let mut show_image = None;
    let mut map = None;
    let mut play_movie = None;
    let mut playlist = None;
    let mut script = None;
    let mut input_script = false;
    let mut ui_regression_shot = None;
    let mut ui_regression_res = None;
    let mut ui_regression_dpi = None;
    let mut ui_regression_ui_scale = None;
    let mut ui_regression_screen = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quake-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--quake-dir expects a path".into()))?;
                quake_dir = Some(PathBuf::from(value));
            }
            "--show-image" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--show-image expects a path".into()))?;
                show_image = Some(value);
            }
            "--map" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--map expects a name".into()))?;
                map = Some(value);
            }
            "--play-movie" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--play-movie expects a path".into()))?;
                play_movie = Some(PathBuf::from(value));
            }
            "--playlist" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--playlist expects a path".into()))?;
                playlist = Some(PathBuf::from(value));
            }
            "--script" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--script expects a path".into()))?;
                script = Some(PathBuf::from(value));
            }
            "--input-script" => {
                input_script = true;
            }
            "--ui-regression-shot" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--ui-regression-shot expects a path".into())
                })?;
                ui_regression_shot = Some(PathBuf::from(value));
            }
            "--ui-regression-res" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--ui-regression-res expects WxH".into())
                })?;
                ui_regression_res = Some(parse_resolution_arg(&value).ok_or_else(|| {
                    ArgParseError::Message("--ui-regression-res expects WxH".into())
                })?);
            }
            "--ui-regression-dpi" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--ui-regression-dpi expects a number".into())
                })?;
                ui_regression_dpi = Some(parse_scale_arg(&value).map_err(|_| {
                    ArgParseError::Message("--ui-regression-dpi expects a number".into())
                })?);
            }
            "--ui-regression-ui-scale" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--ui-regression-ui-scale expects a number".into())
                })?;
                ui_regression_ui_scale = Some(parse_scale_arg(&value).map_err(|_| {
                    ArgParseError::Message("--ui-regression-ui-scale expects a number".into())
                })?);
            }
            "--ui-regression-screen" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--ui-regression-screen expects a value".into())
                })?;
                ui_regression_screen =
                    Some(UiRegressionScreen::parse(&value).ok_or_else(|| {
                        ArgParseError::Message(
                            "--ui-regression-screen must be main or options".into(),
                        )
                    })?);
            }
            "-h" | "--help" => return Err(ArgParseError::Help),
            _ => {
                return Err(ArgParseError::Message(format!(
                    "unexpected argument: {}",
                    arg
                )))
            }
        }
    }

    if show_image.is_some() && map.is_some() {
        return Err(ArgParseError::Message(
            "--show-image and --map cannot be used together".into(),
        ));
    }
    if play_movie.is_some() && playlist.is_some() {
        return Err(ArgParseError::Message(
            "--play-movie cannot be used with --playlist".into(),
        ));
    }
    if play_movie.is_some() && show_image.is_some() {
        return Err(ArgParseError::Message(
            "--play-movie cannot be used with --show-image".into(),
        ));
    }
    if playlist.is_some() && show_image.is_some() {
        return Err(ArgParseError::Message(
            "--playlist cannot be used with --show-image".into(),
        ));
    }

    let ui_regression = if ui_regression_shot.is_some()
        || ui_regression_res.is_some()
        || ui_regression_dpi.is_some()
        || ui_regression_ui_scale.is_some()
        || ui_regression_screen.is_some()
    {
        let shot_path = ui_regression_shot
            .ok_or_else(|| ArgParseError::Message("--ui-regression-shot is required".into()))?;
        let resolution = ui_regression_res
            .ok_or_else(|| ArgParseError::Message("--ui-regression-res is required".into()))?;
        let dpi_scale = ui_regression_dpi
            .ok_or_else(|| ArgParseError::Message("--ui-regression-dpi is required".into()))?;
        let ui_scale = ui_regression_ui_scale
            .ok_or_else(|| ArgParseError::Message("--ui-regression-ui-scale is required".into()))?;
        if dpi_scale <= 0.0 {
            return Err(ArgParseError::Message(
                "--ui-regression-dpi must be > 0".into(),
            ));
        }
        if ui_scale <= 0.0 {
            return Err(ArgParseError::Message(
                "--ui-regression-ui-scale must be > 0".into(),
            ));
        }
        let screen = ui_regression_screen.unwrap_or(UiRegressionScreen::Main);
        Some(UiRegressionArgs {
            shot_path,
            resolution,
            dpi_scale,
            ui_scale,
            screen,
        })
    } else {
        None
    };

    if ui_regression.is_some()
        && (show_image.is_some()
            || map.is_some()
            || play_movie.is_some()
            || playlist.is_some()
            || script.is_some()
            || input_script)
    {
        return Err(ArgParseError::Message(
            "--ui-regression-* cannot be combined with other modes".into(),
        ));
    }

    Ok(CliArgs {
        quake_dir,
        show_image,
        map,
        play_movie,
        playlist,
        script,
        input_script,
        ui_regression,
    })
}

fn print_usage() {
    eprintln!("usage: pallet [--quake-dir <path>] [--show-image <asset>] [--map <name>] [--play-movie <file>] [--playlist <file>] [--script <path>] [--input-script] [--ui-regression-shot <path> --ui-regression-res <WxH> --ui-regression-dpi <scale> --ui-regression-ui-scale <scale> --ui-regression-screen <main|options>]");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --show-image gfx/conback.lmp");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1 --script scripts/demo.lua");
    eprintln!("example: pallet --play-movie intro.ogv");
    eprintln!("example: pallet --playlist movies_playlist.txt");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1 --input-script");
    eprintln!("example: pallet --ui-regression-shot ui_regression/shot.png --ui-regression-res 1920x1080 --ui-regression-dpi 1.0 --ui-regression-ui-scale 1.0 --ui-regression-screen main");
}

fn parse_resolution_arg(value: &str) -> Option<[u32; 2]> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .or_else(|| value.split_once(','))?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some([width, height])
}

fn parse_scale_arg(value: &str) -> Result<f32, ()> {
    let scale = value.trim().parse::<f32>().map_err(|_| ())?;
    if scale.is_finite() {
        Ok(scale)
    } else {
        Err(())
    }
}

fn setup_ui_regression(
    ui_state: &mut UiState,
    console: &mut ConsoleState,
    settings: &mut Settings,
    regression: &UiRegressionArgs,
    now: Instant,
) {
    settings.window_mode = WindowMode::Windowed;
    settings.resolution = regression.resolution;
    settings.ui_scale = regression.ui_scale;
    settings.master_volume = 0.5;
    ui_state.menu_open = true;
    ui_state.menu_mode = MenuMode::Title;
    ui_state.menu_screen = match regression.screen {
        UiRegressionScreen::Main => MenuScreen::Main,
        UiRegressionScreen::Options => MenuScreen::Options,
    };
    ui_state.console_open = true;
    console.force_open(now);
    console.caret_epoch = now;
    console.buffer.clear();
    console.clear_log();
    console.push_line("ui regression: console log");
    for index in 0..UI_REGRESSION_LOG_LINE_COUNT {
        console.push_line(format!(
            "log {:03}: quick brown fox jumps over line {}",
            index + 1,
            index + 1
        ));
    }
}

fn egui_min_font_px(ctx: &egui::Context) -> f32 {
    let ppp = ctx.pixels_per_point().max(0.001);
    ctx.style()
        .text_styles
        .values()
        .map(|font_id| font_id.size * ppp)
        .fold(f32::INFINITY, f32::min)
}

fn write_png(path: &Path, width: u32, height: u32, data: &[u8]) -> Result<(), String> {
    let expected = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| "png size overflow".to_string())?;
    if data.len() != expected as usize {
        return Err(format!(
            "png data size mismatch (expected {}, got {})",
            expected,
            data.len()
        ));
    }
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return Err(format!("png create dir failed: {}", err));
        }
    }
    let file = std::fs::File::create(path).map_err(|err| format!("png create failed: {}", err))?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|err| format!("png header failed: {}", err))?;
    writer
        .write_image_data(data)
        .map_err(|err| format!("png write failed: {}", err))?;
    Ok(())
}

fn load_playlist(path: &Path) -> Result<VecDeque<PlaylistEntry>, ExitError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| ExitError::new(EXIT_USAGE, format!("playlist read failed: {}", err)))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = VecDeque::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let (path_part, meta_part) = line.split_once('|').unwrap_or((line, ""));
        let path_part = path_part.trim();
        if path_part.is_empty() {
            continue;
        }
        let mut hold_ms = VIDEO_HOLD_LAST_FRAME_MS;
        let meta_part = meta_part.trim();
        if !meta_part.is_empty() {
            for token in meta_part.split(',') {
                let token = token.trim();
                if token.is_empty() {
                    continue;
                }
                let value = token
                    .strip_prefix("hold=")
                    .or_else(|| token.strip_prefix("hold_ms="))
                    .unwrap_or(token);
                if value.chars().all(|ch| ch.is_ascii_digit()) {
                    hold_ms = value.parse::<u64>().map_err(|_| {
                        ExitError::new(
                            EXIT_USAGE,
                            format!("invalid playlist hold value: {}", token),
                        )
                    })?;
                } else {
                    return Err(ExitError::new(
                        EXIT_USAGE,
                        format!("invalid playlist option: {}", token),
                    ));
                }
            }
        }
        let entry = PathBuf::from(path_part);
        let entry = if entry.is_relative() {
            base.join(entry)
        } else {
            entry
        };
        entries.push_back(PlaylistEntry::new(entry, hold_ms));
    }
    if entries.is_empty() {
        return Err(ExitError::new(EXIT_USAGE, "playlist is empty".to_string()));
    }
    Ok(entries)
}

fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::KeyW => "W".to_string(),
        KeyCode::KeyA => "A".to_string(),
        KeyCode::KeyS => "S".to_string(),
        KeyCode::KeyD => "D".to_string(),
        KeyCode::KeyF => "F".to_string(),
        KeyCode::KeyK => "K".to_string(),
        KeyCode::KeyP => "P".to_string(),
        KeyCode::Space => "Space".to_string(),
        KeyCode::ShiftLeft => "ShiftLeft".to_string(),
        KeyCode::Escape => "Escape".to_string(),
        _ => format!("{:?}", code),
    }
}

fn parse_command_line(line: &str) -> Option<(String, Vec<String>)> {
    let mut parts = line.split_whitespace();
    let command = parts.next()?.to_string();
    let args = parts.map(|part| part.to_string()).collect();
    Some((command, args))
}

fn console_quad_vertices(
    rect: ConsoleLogRect,
    surface_size: [u32; 2],
    uv_max: [f32; 2],
) -> [ConsoleQuadVertex; 4] {
    let width = surface_size[0].max(1) as f32;
    let height = surface_size[1].max(1) as f32;
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let (x0, y0) = console_to_ndc(left, top, width, height);
    let (x1, y1) = console_to_ndc(right, bottom, width, height);
    [
        ConsoleQuadVertex {
            pos: [x0, y0],
            uv: [0.0, 0.0],
        },
        ConsoleQuadVertex {
            pos: [x1, y0],
            uv: [uv_max[0], 0.0],
        },
        ConsoleQuadVertex {
            pos: [x1, y1],
            uv: [uv_max[0], uv_max[1]],
        },
        ConsoleQuadVertex {
            pos: [x0, y1],
            uv: [0.0, uv_max[1]],
        },
    ]
}

fn console_to_ndc(x: f32, y: f32, width: f32, height: f32) -> (f32, f32) {
    let nx = (x / width) * 2.0 - 1.0;
    let ny = 1.0 - (y / height) * 2.0;
    (nx, ny)
}

fn console_quad_vertex_bytes(vertices: &[ConsoleQuadVertex]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vertices));
    for vertex in vertices {
        for value in vertex.pos {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in vertex.uv {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

fn console_quad_index_bytes(indices: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(indices));
    for index in indices {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    bytes
}

fn handle_console_command(console: &mut ConsoleState, command: &str, args: &[String]) -> bool {
    match command {
        "logfill" => {
            let count = args
                .first()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(5000)
                .clamp(1, 20000);
            console.max_lines = console.max_lines.max(count);
            console.clear_log();
            for index in 0..count {
                console.push_line(format!(
                    "logfill {:>5}: The quick brown fox jumps over the lazy dog.",
                    index + 1
                ));
            }
            console.scroll_offset = 0.0;
            true
        }
        _ => false,
    }
}

fn load_wav_sfx(quake_dir: &Path, asset: &str) -> Result<Vec<u8>, ExitError> {
    let (pak, pak_path) = load_pak_from_quake_dir(quake_dir)?;
    let asset_name = normalize_asset_name(asset);
    let wav_bytes = pak
        .entry_data(&asset_name)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("asset lookup failed: {}", err)))?
        .ok_or_else(|| {
            ExitError::new(
                EXIT_PAK,
                format!("asset not found in pak0.pak: {}", asset_name),
            )
        })?;
    println!("loaded {} from {}", asset_name, pak_path.display());
    Ok(wav_bytes.to_vec())
}

fn load_music_track(quake_dir: &Path) -> Result<Option<MusicTrack>, ExitError> {
    let mut vfs = Vfs::new();
    vfs.add_root(quake_dir);
    for dir in ["id1/music", "music"] {
        if let Some(track) = find_music_in_dir(&vfs, dir)? {
            return Ok(Some(track));
        }
    }

    let (pak, _) = load_pak_from_quake_dir(quake_dir)?;
    Ok(find_music_in_pak(&pak))
}

fn find_music_in_dir(vfs: &Vfs, dir: &str) -> Result<Option<MusicTrack>, ExitError> {
    let entries = match vfs.list_dir(dir) {
        Ok(entries) => entries,
        Err(VfsError::NotFound(_)) => return Ok(None),
        Err(err) => {
            return Err(ExitError::new(
                EXIT_PAK,
                format!("music scan failed: {}", err),
            ))
        }
    };

    let mut candidates: Vec<String> = entries
        .into_iter()
        .filter(|entry| !entry.is_dir && entry.name.to_lowercase().ends_with(".ogg"))
        .map(|entry| entry.name)
        .collect();
    candidates.sort();

    if let Some(name) = candidates.into_iter().next() {
        let path = format!("{}/{}", dir, name);
        return match vfs.read(&path) {
            Ok(data) => Ok(Some(MusicTrack { name: path, data })),
            Err(err) => Err(ExitError::new(
                EXIT_PAK,
                format!("music read failed: {}", err),
            )),
        };
    }

    Ok(None)
}

fn find_music_in_pak(pak: &PakFile) -> Option<MusicTrack> {
    let mut candidates: Vec<String> = pak
        .entries()
        .iter()
        .filter(|entry| entry.name.starts_with("music/"))
        .map(|entry| entry.name.clone())
        .filter(|name| name.to_lowercase().ends_with(".ogg"))
        .collect();
    candidates.sort();

    for name in candidates {
        if let Ok(Some(bytes)) = pak.entry_data(&name) {
            return Some(MusicTrack {
                name,
                data: bytes.to_vec(),
            });
        }
    }
    None
}

fn load_lmp_image(quake_dir: &Path, asset: &str) -> Result<ImageData, ExitError> {
    let (pak, pak_path) = load_pak_from_quake_dir(quake_dir)?;
    let palette_bytes = pak
        .entry_data("gfx/palette.lmp")
        .map_err(|err| ExitError::new(EXIT_PAK, format!("palette lookup failed: {}", err)))?
        .ok_or_else(|| ExitError::new(EXIT_PAK, "palette not found in pak0.pak"))?;
    let palette = lmp::parse_palette(palette_bytes)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("palette parse failed: {}", err)))?;

    let asset_name = normalize_asset_name(asset);
    let image_bytes = pak
        .entry_data(&asset_name)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("asset lookup failed: {}", err)))?
        .ok_or_else(|| {
            ExitError::new(
                EXIT_PAK,
                format!("asset not found in pak0.pak: {}", asset_name),
            )
        })?;
    let image = lmp::parse_lmp_image(image_bytes)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("image parse failed: {}", err)))?;
    let rgba = image.to_rgba8(&palette);
    let image = ImageData::new(image.width, image.height, rgba)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("image data failed: {}", err)))?;

    println!(
        "loaded {} from {} ({}x{})",
        asset_name,
        pak_path.display(),
        image.width,
        image.height
    );

    Ok(image)
}

fn load_bsp_scene(
    quake_dir: &Path,
    map: &str,
) -> Result<(MeshData, Bounds, SceneCollision, Option<SpawnPoint>), ExitError> {
    let (pak, pak_path) = load_pak_from_quake_dir(quake_dir)?;
    let map_name = normalize_map_asset(map);
    let bsp_bytes = pak
        .entry_data(&map_name)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("map lookup failed: {}", err)))?
        .ok_or_else(|| {
            ExitError::new(EXIT_PAK, format!("map not found in pak0.pak: {}", map_name))
        })?;
    let bsp = bsp::parse_bsp(bsp_bytes)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("bsp parse failed: {}", err)))?;

    println!(
        "loaded {} from {} ({} vertices, {} faces)",
        map_name,
        pak_path.display(),
        bsp.vertices.len(),
        bsp.faces.len()
    );

    let spawn = bsp::parse_spawn(bsp_bytes, &bsp.header)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("bsp spawn parse failed: {}", err)))?;

    let (mesh, bounds, collision) = build_scene_mesh(&bsp)?;
    Ok((mesh, bounds, collision, spawn))
}

fn build_scene_mesh(bsp: &Bsp) -> Result<(MeshData, Bounds, SceneCollision), ExitError> {
    let face_range = bsp.world_face_range().unwrap_or(0..bsp.faces.len());

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut floors = Vec::new();
    let mut bounds = Bounds::empty();

    for face_index in face_range {
        let face = match bsp.faces.get(face_index) {
            Some(face) => face,
            None => {
                return Err(ExitError::new(
                    EXIT_BSP,
                    format!("face index out of bounds: {}", face_index),
                ));
            }
        };

        let num_edges = face.num_edges as usize;
        if num_edges < 3 {
            continue;
        }
        let first_edge = usize::try_from(face.first_edge).map_err(|_| {
            ExitError::new(
                EXIT_BSP,
                format!("face has negative first_edge: {}", face.first_edge),
            )
        })?;
        let end = first_edge
            .checked_add(num_edges)
            .ok_or_else(|| ExitError::new(EXIT_BSP, "face edge range overflow"))?;
        if end > bsp.surfedges.len() {
            return Err(ExitError::new(
                EXIT_BSP,
                format!(
                    "face edge range out of bounds: {}..{} (surfedges {})",
                    first_edge,
                    end,
                    bsp.surfedges.len()
                ),
            ));
        }

        let mut polygon = Vec::with_capacity(num_edges);
        for &surfedge in &bsp.surfedges[first_edge..end] {
            let edge_index = if surfedge < 0 { -surfedge } else { surfedge } as usize;
            let reversed = surfedge < 0;
            let edge = match bsp.edges.get(edge_index) {
                Some(edge) => *edge,
                None => {
                    return Err(ExitError::new(
                        EXIT_BSP,
                        format!("edge index out of bounds: {}", edge_index),
                    ));
                }
            };
            let vertex_index = if reversed { edge[1] } else { edge[0] } as usize;
            let vertex = match bsp.vertices.get(vertex_index) {
                Some(vertex) => *vertex,
                None => {
                    return Err(ExitError::new(
                        EXIT_BSP,
                        format!("vertex index out of bounds: {}", vertex_index),
                    ));
                }
            };
            polygon.push(vertex);
        }

        if polygon.len() < 3 {
            continue;
        }

        let v0 = quake_to_render(polygon[0]);
        for i in 1..polygon.len() - 1 {
            let v1 = quake_to_render(polygon[i]);
            let v2 = quake_to_render(polygon[i + 1]);
            let normal = v1.sub(v0).cross(v2.sub(v0)).normalize_or_zero();
            if let Some(triangle) = Triangle::from_normal(v0, v1, v2, normal) {
                if triangle.normal.y >= FLOOR_NORMAL_MIN {
                    floors.push(triangle);
                }
            }
            let color = normal.abs().scale(0.8).add(Vec3::new(0.2, 0.2, 0.2));

            let base = u32::try_from(vertices.len())
                .map_err(|_| ExitError::new(EXIT_BSP, "vertex count overflow building mesh"))?;
            vertices.push(MeshVertex {
                position: v0.to_array(),
                color: color.to_array(),
            });
            vertices.push(MeshVertex {
                position: v1.to_array(),
                color: color.to_array(),
            });
            vertices.push(MeshVertex {
                position: v2.to_array(),
                color: color.to_array(),
            });
            indices.extend_from_slice(&[base, base + 1, base + 2]);

            bounds.include(v0);
            bounds.include(v1);
            bounds.include(v2);
        }
    }

    if !bounds.valid {
        return Err(ExitError::new(
            EXIT_BSP,
            "mesh contained no drawable triangles",
        ));
    }

    let mesh = MeshData::new(vertices, indices)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("mesh build failed: {}", err)))?;
    let planes = bsp
        .planes
        .iter()
        .map(|plane| CollisionPlane {
            normal: quake_to_render(plane.normal),
            dist: plane.dist,
        })
        .collect();
    let clipnodes = bsp
        .clipnodes
        .iter()
        .map(|node| ClipNode {
            plane_id: node.plane_id,
            children: node.children,
        })
        .collect();
    let headnode = if bsp.clipnodes.is_empty() {
        -1
    } else {
        bsp.models
            .first()
            .map(|model| model.headnode[1])
            .unwrap_or(0)
    };
    Ok((
        mesh,
        bounds,
        SceneCollision {
            floors,
            planes,
            clipnodes,
            headnode,
        },
    ))
}

fn load_pak_from_quake_dir(quake_dir: &Path) -> Result<(PakFile, PathBuf), ExitError> {
    if !quake_dir.is_dir() {
        return Err(ExitError::new(
            EXIT_QUAKE_DIR,
            format!("quake dir not found: {}", quake_dir.display()),
        ));
    }

    let mut vfs = Vfs::new();
    vfs.add_root(quake_dir);

    let (virtual_path, pak_path) = if vfs.exists("id1/pak0.pak") {
        ("id1/pak0.pak", quake_dir.join("id1").join("pak0.pak"))
    } else if vfs.exists("pak0.pak") {
        ("pak0.pak", quake_dir.join("pak0.pak"))
    } else {
        return Err(ExitError::new(
            EXIT_PAK,
            format!("pak0.pak not found under {}", quake_dir.display()),
        ));
    };

    let data = vfs
        .read(virtual_path)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("pak read failed: {}", err)))?;
    let pak = pak::parse_pak(data)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("pak parse failed: {}", err)))?;

    Ok((pak, pak_path))
}

fn normalize_asset_name(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    normalized
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn normalize_map_asset(name: &str) -> String {
    let mut normalized = normalize_asset_name(name);
    if let Some(stripped) = normalized.strip_prefix("maps/") {
        normalized = stripped.to_string();
    }
    if !normalized.ends_with(".bsp") {
        normalized.push_str(".bsp");
    }
    format!("maps/{}", normalized)
}

fn open_console(
    console: &mut ConsoleState,
    ui_state: &mut UiState,
    window: &Window,
    input: &mut InputState,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
) {
    let now = Instant::now();
    console.caret_epoch = now;
    console.open(now);
    console.resume_mouse_look = *mouse_look;
    console.buffer.clear();
    ui_state.console_open = console.is_blocking();
    *input = InputState::default();
    *mouse_look = false;
    *mouse_grabbed = set_cursor_mode(window, *mouse_look);
    window.set_ime_allowed(false);
    println!("console: open");
}

fn close_console(
    console: &mut ConsoleState,
    ui_state: &mut UiState,
    window: &Window,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    scene_active: bool,
    allow_recapture: bool,
) {
    console.close(Instant::now());
    console.buffer.clear();
    ui_state.console_open = console.is_blocking();
    if allow_recapture && scene_active && console.resume_mouse_look && !ui_state.menu_open {
        *mouse_look = true;
        *mouse_grabbed = set_cursor_mode(window, *mouse_look);
    }
    console.resume_mouse_look = false;
    window.set_ime_allowed(false);
    println!("console: closed");
}

#[allow(clippy::too_many_arguments)]
fn handle_non_video_key_input(
    code: KeyCode,
    pressed: bool,
    is_repeat: bool,
    input_router: &InputRouter,
    console: &mut ConsoleState,
    ui_state: &mut UiState,
    window: &Window,
    input: &mut InputState,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    was_mouse_look: &mut bool,
    scene_active: bool,
    fly_mode: &mut bool,
    camera: &mut CameraState,
    collision: Option<&SceneCollision>,
    audio: Option<&Rc<AudioEngine>>,
    sfx_data: Option<&Vec<u8>>,
    script: &mut Option<ScriptRuntime>,
    video_active: bool,
) -> bool {
    if pressed && !is_repeat && code == KeyCode::Backquote {
        if video_active {
            return true;
        }
        if input_router.allow_console_toggle(ui_state.menu_open) {
            if console.is_opening() || console.is_interactive() {
                close_console(
                    console,
                    ui_state,
                    window,
                    mouse_look,
                    mouse_grabbed,
                    scene_active,
                    true,
                );
            } else {
                open_console(console, ui_state, window, input, mouse_look, mouse_grabbed);
            }
        }
        return true;
    }
    if pressed && !is_repeat && code == KeyCode::Escape {
        if console.is_blocking() {
            close_console(
                console,
                ui_state,
                window,
                mouse_look,
                mouse_grabbed,
                scene_active,
                true,
            );
            return true;
        }
        if ui_state.menu_open {
            if ui_state.menu_mode == MenuMode::Pause {
                ui_state.close_menu();
                *mouse_look = *was_mouse_look;
                *mouse_grabbed = set_cursor_mode(window, *mouse_look);
            }
            return true;
        }
        if scene_active {
            *was_mouse_look = *mouse_look;
            ui_state.open_pause();
            *input = InputState::default();
            *mouse_look = false;
            *mouse_grabbed = set_cursor_mode(window, *mouse_look);
        }
        return true;
    }
    match input_router.active_layer(console.is_blocking(), ui_state.menu_open) {
        InputLayer::Console => {
            if pressed && console.is_interactive() {
                match code {
                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        let line = console.buffer.trim().to_string();
                        console.buffer.clear();
                        if !line.is_empty() {
                            println!("> {}", line);
                            console.push_line(format!("> {}", line));
                            if let Some((command, args)) = parse_command_line(&line) {
                                if !handle_console_command(console, &command, &args) {
                                    if let Some(script) = script.as_mut() {
                                        match script.engine.run_command(&command, &args) {
                                            Ok(true) => {}
                                            Ok(false) => {
                                                eprintln!("unknown script command: {}", command);
                                                console.push_line(format!(
                                                    "unknown script command: {}",
                                                    command
                                                ));
                                            }
                                            Err(err) => {
                                                eprintln!("lua command failed: {}", err);
                                                console.push_line(format!(
                                                    "lua command failed: {}",
                                                    err
                                                ));
                                            }
                                        }
                                    } else {
                                        eprintln!("no script loaded");
                                        console.push_line("no script loaded");
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        console.buffer.pop();
                    }
                    KeyCode::PageUp => {
                        let page =
                            console.visible_lines.max(1) as f32 * console.line_height.max(1.0);
                        console.scroll_by(page);
                    }
                    KeyCode::PageDown => {
                        let page =
                            console.visible_lines.max(1) as f32 * console.line_height.max(1.0);
                        console.scroll_by(-page);
                    }
                    KeyCode::Home => {
                        console.scroll_offset = f32::INFINITY;
                    }
                    KeyCode::End => {
                        console.scroll_offset = 0.0;
                    }
                    _ => {}
                }
            }
        }
        InputLayer::Menu => {}
        InputLayer::Game => {
            match code {
                KeyCode::KeyW => input.forward = pressed,
                KeyCode::KeyS => input.back = pressed,
                KeyCode::KeyA => input.left = pressed,
                KeyCode::KeyD => input.right = pressed,
                KeyCode::Space => input.jump = pressed,
                KeyCode::ShiftLeft => input.down = pressed,
                KeyCode::KeyF if pressed => {
                    *fly_mode = !*fly_mode;
                    if *fly_mode {
                        camera.velocity = Vec3::zero();
                        camera.vertical_velocity = 0.0;
                        camera.on_ground = false;
                    } else if let Some(scene) = collision {
                        camera.snap_to_floor(scene);
                    }
                }
                KeyCode::KeyP if pressed => {
                    if let (Some(audio), Some(data)) = (audio, sfx_data) {
                        if let Err(err) = audio.play_wav(data.clone()) {
                            eprintln!("{}", err);
                        }
                    }
                }
                _ => {}
            }
            if let Some(script) = script.as_mut() {
                let key = key_name(code);
                if let Err(err) = script.engine.on_key(&key, pressed) {
                    eprintln!("lua on_key failed: {}", err);
                }
            }
        }
    }
    false
}

fn apply_window_settings(window: &Window, settings: &Settings) {
    match settings.window_mode {
        WindowMode::Windowed => {
            window.set_fullscreen(None);
            window.set_decorations(true);
            let _ = window.request_inner_size(PhysicalSize::new(
                settings.resolution[0],
                settings.resolution[1],
            ));
        }
        WindowMode::Borderless => {
            let monitor = window
                .current_monitor()
                .or_else(|| window.primary_monitor());
            window.set_fullscreen(Some(Fullscreen::Borderless(monitor)));
        }
        WindowMode::Fullscreen => {
            let monitor = window
                .current_monitor()
                .or_else(|| window.primary_monitor());
            if let Some(monitor) = monitor {
                let target = settings.resolution;
                let best_mode = monitor
                    .video_modes()
                    .filter(|mode| {
                        let size = mode.size();
                        size.width == target[0] && size.height == target[1]
                    })
                    .max_by_key(|mode| mode.refresh_rate_millihertz());
                if let Some(mode) = best_mode {
                    window.set_fullscreen(Some(Fullscreen::Exclusive(mode)));
                } else {
                    window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor))));
                }
            } else {
                window.set_fullscreen(None);
            }
        }
    }
}

fn set_cursor_mode(window: &Window, enabled: bool) -> bool {
    if enabled {
        let grabbed = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            .is_ok();
        window.set_cursor_visible(!grabbed);
        grabbed
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        false
    }
}

fn cursor_center(window: &Window) -> PhysicalPosition<f64> {
    let size = window.inner_size();
    PhysicalPosition::new(size.width as f64 * 0.5, size.height as f64 * 0.5)
}

fn center_cursor(window: &Window) -> bool {
    let center = cursor_center(window);
    window.set_cursor_position(center).is_ok()
}

fn aspect_ratio(size: PhysicalSize<u32>) -> f32 {
    let width = size.width.max(1) as f32;
    let height = size.height.max(1) as f32;
    width / height
}

fn bool_to_axis(positive: bool, negative: bool) -> f32 {
    (positive as i32 - negative as i32) as f32
}

fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fovy * 0.5).tan();
    let nf = 1.0 / (near - far);
    let gl = [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, (far + near) * nf, -1.0],
        [0.0, 0.0, (2.0 * far * near) * nf, 0.0],
    ];
    mat4_mul(OPENGL_TO_WGPU, gl)
}

fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        let v = b[col];
        out[col] = mat4_mul_vec4(a, v);
    }
    out
}

fn mat4_mul_vec4(m: [[f32; 4]; 4], v: [f32; 4]) -> [f32; 4] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2] + m[3][0] * v[3],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2] + m[3][1] * v[3],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2] + m[3][2] * v[3],
        m[0][3] * v[0] + m[1][3] * v[1] + m[2][3] * v[2] + m[3][3] * v[3],
    ]
}

fn quake_to_render(value: [f32; 3]) -> Vec3 {
    Vec3::new(value[0], value[2], -value[1])
}

impl From<[f32; 3]> for Vec3 {
    fn from(value: [f32; 3]) -> Self {
        Vec3::new(value[0], value[1], value[2])
    }
}
