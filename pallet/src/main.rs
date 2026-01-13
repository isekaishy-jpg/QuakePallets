use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::f32::consts::TAU;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use audio::AudioEngine;
use character_collision::CollisionProfile;
use character_motor_arena::{
    build_move_intent, golden_angle_metrics, ArenaMotor, ArenaMotorConfig, ArenaMotorInput,
    ArenaMotorState, FrictionlessJumpMode,
};
use character_motor_rpg::{
    build_move_intent as build_move_intent_rpg, RpgMotor, RpgMotorConfig, RpgMotorInput,
    RpgMotorState,
};
use client::{Client, ClientInput};
use collision_world::{Aabb as CollisionAabb, CollisionWorld};
use compat_quake::bsp::{self, Bsp, SpawnPoint};
use compat_quake::lmp;
use engine_core::asset_id::AssetKey;
use engine_core::asset_manager::{
    AssetBudgetTag, AssetEntrySnapshot, AssetManager, AssetPriority, AssetStatus, BlobAsset,
    CollisionWorldAsset, ConfigAsset, QuakeRawAsset, RequestOpts, ScriptAsset, TestMapAsset,
    TextAsset, TextureAsset,
};
use engine_core::asset_resolver::{
    AssetLayer, AssetResolver, AssetSource, ResolveReport, ResolvedLocation, ResolvedPath,
};
use engine_core::control_plane::{
    parse_command_line, register_core_commands, register_core_cvars, register_pallet_command_specs,
    CommandArgs, CommandOutput, CommandRegistry, CommandSpec, CoreCvars, CvarBounds, CvarDef,
    CvarFlags, CvarId, CvarRegistry, CvarValue, ExecPathResolver, ExecSource, ParsedCommand,
};
use engine_core::jobs::{JobQueue, Jobs};
use engine_core::level_manifest::{
    discover_level_manifests, load_level_manifest, resolve_level_manifest_path, LevelManifest,
    LevelManifestPath,
};
use engine_core::logging::{self, LogLevel};
use engine_core::mount_manifest::{load_mount_manifest, MountManifestEntry};
use engine_core::observability;
use engine_core::path_policy::{ConfigKind, PathOverrides, PathPolicy};
use engine_core::quake_index::{QuakeEntry, QuakeIndex};
use engine_core::vfs::{MountKind, Vfs, VfsError};
use map_cook::build_test_map_colliders;
use net_transport::{LoopbackTransport, Transport, TransportConfig};
use physics_rapier::PhysicsWorld;
use platform_winit::{
    create_window, ControlFlow, CursorGrabMode, DeviceEvent, ElementState, Event, Fullscreen, Ime,
    KeyCode, ModifiersState, MouseButton, MouseScrollDelta, PhysicalKey, PhysicalPosition,
    PhysicalSize, Window, WindowEvent,
};
use player_camera::PlayerCamera;
use player_controller::{
    DirectInputAdapter, InputIntent, Motor as ControllerMotor, MotorContext, MotorOutput,
    PlayerController, PlayerKinematics, RawInput,
};
use rapier3d::math::{Isometry, Vector};
use rapier3d::prelude::{ColliderHandle, Real};
use render_wgpu::{
    FrameCapture, ImageData, MeshData, MeshVertex, RenderCaptureError, RenderError, TextBounds,
    TextFontSystem, TextLayer, TextOverlay, TextOverlayTimings, TextPosition, TextSpan, TextStyle,
    TextViewport, UploadPriority, UploadQueue, YuvImageView,
};
use script_lua::{HostCallbacks, ScriptConfig, ScriptEngine, SpawnRequest};
use server::Server;
use test_map::{ResolvedSolid, SolidKind, TestMap};
use video::{
    advance_playlist, start_video_playback, PlaylistEntry, VideoDebugSnapshot, VideoDebugStats,
    VideoPlayback, VIDEO_AUDIO_PREBUFFER_MS, VIDEO_HOLD_LAST_FRAME_MS, VIDEO_INTERMISSION_MS,
    VIDEO_MAX_QUEUED_MS_PLAYBACK, VIDEO_MAX_QUEUED_MS_PREDECODE, VIDEO_PLAYBACK_WARM_MS,
    VIDEO_PLAYBACK_WARM_UP_MS, VIDEO_PREDECODE_MIN_AUDIO_MS, VIDEO_PREDECODE_MIN_ELAPSED_MS,
    VIDEO_PREDECODE_MIN_FRAMES, VIDEO_PREDECODE_RAMP_MS, VIDEO_PREDECODE_START_DELAY_MS,
    VIDEO_PREDECODE_WARM_MS, VIDEO_START_MIN_FRAMES,
};
use wgpu::util::DeviceExt;

use settings::{
    config_path_for_profile, default_profile_name, parse_resolution, settings_lines,
    write_config_lines, Settings, WindowMode,
};
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
const EXIT_SMOKE: i32 = 30;
const DEFAULT_SFX: &str = "sound/misc/menu1.wav";
const QUAKE_VROOT: &str = "raw/quake";
const HUD_FONT_SIZE: f32 = 16.0;
const HUD_FONT_SIZE_SMALL: f32 = 14.0;
const CONSOLE_FONT_SIZE: f32 = 14.0;
const HUD_TEXT_COLOR: [f32; 4] = [0.9, 0.95, 1.0, 1.0];
const BUILD_TEXT: &str = concat!("build: ", env!("CARGO_PKG_VERSION"));
const CONSOLE_TEXT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const CONSOLE_BG_COLOR: [f32; 4] = [0.05, 0.05, 0.08, 0.75];
const CONSOLE_SEPARATOR_COLOR: [f32; 4] = [0.95, 0.95, 0.95, 0.9];
const CONSOLE_SELECTION_COLOR: [f32; 4] = [0.2, 0.4, 0.8, 0.35];
const CONSOLE_MENU_BG_COLOR: [f32; 4] = [0.08, 0.1, 0.14, 0.95];
const CONSOLE_MENU_TEXT_COLOR: [f32; 4] = [0.95, 0.95, 0.98, 1.0];
const CONSOLE_COLOR_BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const CONSOLE_COLOR_RED: [f32; 4] = [0.95, 0.25, 0.25, 1.0];
const CONSOLE_COLOR_ORANGE: [f32; 4] = [0.95, 0.55, 0.2, 1.0];
const CONSOLE_COLOR_YELLOW: [f32; 4] = [0.95, 0.85, 0.2, 1.0];
const CONSOLE_COLOR_GREEN: [f32; 4] = [0.35, 0.9, 0.35, 1.0];
const CONSOLE_COLOR_BLUE: [f32; 4] = [0.35, 0.6, 0.95, 1.0];
const CONSOLE_COLOR_INDIGO: [f32; 4] = [0.45, 0.45, 0.95, 1.0];
const CONSOLE_COLOR_VIOLET: [f32; 4] = [0.8, 0.45, 0.95, 1.0];
const CONSOLE_MENU_PADDING: f32 = 4.0;
const CONSOLE_MENU_CHAR_WIDTH: f32 = 0.6;
const CONSOLE_TOAST_DURATION_MS: u64 = 1200;
const CONSOLE_HEIGHT_RATIO: f32 = 0.45;
const CONSOLE_PADDING: f32 = 6.0;
const CONSOLE_INPUT_PADDING: f32 = 0.5;
const CONSOLE_INPUT_BG_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const CONSOLE_SLIDE_MS: u64 = 500;
const CONSOLE_HISTORY_LIMIT: usize = 128;
const CONSOLE_SELECTION_LINE_LIMIT: usize = 128;
const ASSET_LIST_DEFAULT_LIMIT: usize = 200;
const ASSET_LIST_MAX_LIMIT: usize = 1000;
const TEST_MAP_CYLINDER_SEGMENTS: usize = 16;
const TEST_MAP_MOVE_SPEED: f32 = 4.5;
const TEST_MAP_ACCEL: f32 = 30.0;
const TEST_MAP_AIR_ACCEL: f32 = 18.0;
const TEST_MAP_FRICTION: f32 = 18.0;
const TEST_MAP_STOP_SPEED: f32 = 4.0;
const TEST_MAP_GRAVITY: f32 = 9.81;
const TEST_MAP_JUMP_SPEED: f32 = 4.5;
const TEST_MAP_MAX_AIR_SPEED: f32 = 6.0;
const TEST_MAP_AIR_RESISTANCE: f32 = 1.0;
const TEST_MAP_GOLDEN_ANGLE_TARGET: f32 = 45.0 * std::f32::consts::PI / 180.0;
const TEST_MAP_GOLDEN_ANGLE_GAIN_MIN: f32 = 1.0;
const TEST_MAP_GOLDEN_ANGLE_GAIN_PEAK: f32 = 1.25;
const TEST_MAP_GOLDEN_ANGLE_BLEND_START: f32 = TEST_MAP_MOVE_SPEED;
const TEST_MAP_GOLDEN_ANGLE_BLEND_END: f32 = TEST_MAP_MOVE_SPEED * 1.4;
const TEST_MAP_CORRIDOR_SHAPING_STRENGTH_DEG: f32 = 80.0;
const TEST_MAP_CORRIDOR_SHAPING_MIN_SPEED: f32 = 10.0;
const TEST_MAP_CORRIDOR_SHAPING_MAX_ANGLE_DEG: f32 = 6.0;
const TEST_MAP_CORRIDOR_SHAPING_MIN_ALIGNMENT: f32 = 0.2;
const TEST_MAP_JUMP_BUFFER_WINDOW: f32 = 0.1;
const TEST_MAP_BHOP_GRACE: f32 = 0.1;
const TEST_MAP_BHOP_FRICTION_SCALE: f32 = 0.25;
const TEST_MAP_BHOP_FRICTION_SCALE_BEST_ANGLE: f32 = 0.0;
const TEST_MAP_EYE_HEIGHT: f32 = 1.6;
const COLLISION_INTEREST_RADIUS: f32 = 12.0;
const KCC_QUERY_SMOOTHING: f32 = 0.1;
const UI_REGRESSION_MIN_FONT_PX: f32 = 9.0;
const UI_REGRESSION_LOG_LINE_COUNT: usize = 24;
const UI_REGRESSION_FPS: f32 = 144.0;
const UI_REGRESSION_SIM_RATE: f32 = 60.0;
const UI_REGRESSION_NET_RATE: f32 = 30.0;
const PERF_BUDGET_EGUI_MS: f32 = 2.0;
const PERF_BUDGET_GLYPHON_PREP_MS: f32 = 3.0;
const PERF_BUDGET_GLYPHON_RENDER_MS: f32 = 2.0;
const PERF_HUD_UPDATE_MS: u64 = 250;
const PERF_HUD_EPS_MS: f32 = 0.1;
const PERF_HUD_LINES: usize = 4;
const HUD_STATS_UPDATE_MS: u64 = 250;
const SMOKE_DEFAULT_TIMEOUT_MS: u64 = 60_000;
const SMOKE_DEFAULT_STEP_TIMEOUT_MS: u64 = 5_000;
const SMOKE_REPORT_DIR: &str = ".pallet/smoke_reports";
const STRESS_GLYPH_TARGET: usize = 50_000;
const STRESS_LOG_LINES: usize = 5_000;
const STRESS_EDIT_DURATION_MS: u64 = 5_000;
const STRESS_EDIT_INTERVAL_MS: u64 = 50;
const STRESS_FONT_BASE: f32 = 8.0;
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
    content_root: Option<PathBuf>,
    dev_root: Option<PathBuf>,
    user_config_root: Option<PathBuf>,
    mounts: Vec<MountSpec>,
    mount_manifests: Vec<String>,
    show_image: Option<String>,
    map: Option<String>,
    play_movie: Option<PathBuf>,
    playlist: Option<String>,
    script: Option<String>,
    input_script: bool,
    smoke_script: Option<String>,
    smoke_timeout_ms: Option<u64>,
    ui_regression: Option<UiRegressionArgs>,
    debug_resolution: bool,
    dev_motor: Option<i32>,
}

enum ArgParseError {
    Help,
    Message(String),
}

struct MountSpec {
    kind: MountKind,
    mount_point: String,
    path: PathBuf,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneKind {
    Bsp,
    TestMap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MotorKind {
    Arena,
    Rpg,
}

impl MotorKind {
    fn from_cvar(value: i32) -> Self {
        if value == 2 {
            MotorKind::Rpg
        } else {
            MotorKind::Arena
        }
    }
}

struct DualMotor {
    kind: MotorKind,
    arena: ArenaMotor,
    rpg: RpgMotor,
}

impl DualMotor {
    fn new(arena_config: ArenaMotorConfig, rpg_config: RpgMotorConfig) -> Self {
        Self {
            kind: MotorKind::Arena,
            arena: ArenaMotor::new(arena_config),
            rpg: RpgMotor::new(rpg_config),
        }
    }

    fn kind(&self) -> MotorKind {
        self.kind
    }

    fn set_kind(&mut self, kind: MotorKind) {
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.reset_states();
    }

    fn reset_states(&mut self) {
        self.arena.reset_state();
        self.rpg.reset_state();
    }

    fn arena_config(&self) -> ArenaMotorConfig {
        self.arena.config()
    }

    fn arena_config_mut(&mut self) -> &mut ArenaMotorConfig {
        self.arena.config_mut()
    }

    fn rpg_config(&self) -> RpgMotorConfig {
        self.rpg.config()
    }
}

impl ControllerMotor for DualMotor {
    fn step(
        &mut self,
        input: &InputIntent,
        state: &PlayerKinematics,
        ctx: MotorContext,
    ) -> MotorOutput {
        match self.kind {
            MotorKind::Arena => {
                let motor_state = ArenaMotorState {
                    velocity: state.velocity,
                    grounded: state.grounded,
                    ground_normal: state.ground_normal,
                    yaw: ctx.yaw,
                };
                let motor_output = self.arena.step(
                    ArenaMotorInput {
                        move_axis: input.move_axis,
                        jump: input.jump,
                    },
                    motor_state,
                    ctx.dt,
                );
                MotorOutput {
                    desired_translation: motor_output.desired_translation,
                    next_velocity: motor_output.next_velocity,
                }
            }
            MotorKind::Rpg => {
                let motor_state = RpgMotorState {
                    velocity: state.velocity,
                    grounded: state.grounded,
                    ground_normal: state.ground_normal,
                    yaw: ctx.yaw,
                };
                let motor_output = self.rpg.step(
                    RpgMotorInput {
                        move_axis: input.move_axis,
                        jump: input.jump,
                    },
                    motor_state,
                    ctx.dt,
                );
                MotorOutput {
                    desired_translation: motor_output.desired_translation,
                    next_velocity: motor_output.next_velocity,
                }
            }
        }
    }
}

struct LoadedScene {
    mesh: MeshData,
    bounds: Bounds,
    collision: Option<SceneCollision>,
    spawn: Option<SpawnPoint>,
    kind: SceneKind,
    test_map: Option<TestMapSceneData>,
}

struct TestMapSceneData {
    key: AssetKey,
    map: TestMap,
    collision_world_key: AssetKey,
    collision_world: CollisionWorld,
}

struct CollisionWorldRuntime {
    world: CollisionWorld,
    loaded_chunks: Vec<u32>,
    collider_handles: Vec<ColliderHandle>,
    triangle_count: u64,
}

struct TestMapRuntime {
    key: AssetKey,
    world: PhysicsWorld,
    collision_world: CollisionWorldRuntime,
    controller: PlayerController<DirectInputAdapter, DualMotor>,
    position: Isometry<Real>,
    prev_position: Isometry<Real>,
    velocity: Vec3,
    prev_velocity: Vec3,
    grounded: bool,
    ground_normal: Option<Vector<Real>>,
    capsule_offset: f32,
    kcc_query_ms: f32,
}

enum MapRequest {
    Bsp(String),
    TestMap(AssetKey),
}

#[allow(clippy::too_many_arguments)]
fn enter_map_scene(
    renderer: &mut render_wgpu::Renderer,
    window: &Window,
    asset_manager: &AssetManager,
    quake_vfs: Option<&Vfs>,
    map: &str,
    audio: Option<&Rc<AudioEngine>>,
    camera: &mut CameraState,
    collision: &mut Option<SceneCollision>,
    test_map_runtime: &mut Option<TestMapRuntime>,
    scene_active: &mut bool,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    fly_mode: &mut bool,
    loopback: &mut Option<LoopbackNet>,
) -> Result<(), ExitError> {
    let scene = load_scene(asset_manager, quake_vfs, map)?;

    renderer.clear_textured_quad();
    renderer
        .set_scene(scene.mesh)
        .map_err(|err| ExitError::new(EXIT_SCENE, format!("scene upload failed: {}", err)))?;

    *collision = scene.collision;
    *camera = CameraState::from_bounds(&scene.bounds, collision.as_ref());
    if let Some(spawn) = scene.spawn {
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
    *test_map_runtime = scene
        .test_map
        .map(|data| build_test_map_runtime(&data, &scene.bounds))
        .transpose()?;
    if let Some(runtime) = test_map_runtime.as_mut() {
        let tuning = match runtime.controller.motor().kind() {
            MotorKind::Arena => camera_tuning_from_arena(runtime.controller.motor().arena_config()),
            MotorKind::Rpg => camera_tuning_from_rpg(runtime.controller.motor().rpg_config()),
        };
        configure_test_map_camera(camera, &scene.bounds, tuning);
        snap_test_map_runtime_to_ground(runtime, &scene.bounds);
        runtime
            .controller
            .camera_mut()
            .set_look(camera.yaw, camera.pitch);
        let origin_y = runtime.position.translation.y - runtime.capsule_offset;
        camera.position = Vec3::new(
            runtime.position.translation.x,
            origin_y + camera.eye_height,
            runtime.position.translation.z,
        );
        camera.velocity = Vec3::zero();
        camera.vertical_velocity = 0.0;
        camera.on_ground = runtime.grounded;
        runtime.velocity = Vec3::zero();
        runtime.controller.motor_mut().reset_states();
        *fly_mode = false;
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

    if scene.kind == SceneKind::Bsp {
        if let Some(audio) = audio {
            let vfs = quake_vfs.ok_or_else(|| {
                ExitError::new(EXIT_QUAKE_DIR, "quake mounts not configured for map load")
            })?;
            match load_music_track(asset_manager, vfs) {
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
    }

    Ok(())
}

struct MusicTrack {
    name: String,
    data: Vec<u8>,
}

#[derive(Default, Clone, Copy)]
struct InputState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    jump_keyboard: bool,
    jump_mouse: bool,
    down: bool,
}

#[derive(Clone, Copy, Debug)]
struct MovementCvars {
    air_max_speed: CvarId,
    air_accel: CvarId,
    air_resistance: CvarId,
    air_resistance_speed_scale: CvarId,
    golden_target_deg: CvarId,
    golden_gain_min: CvarId,
    golden_gain_peak: CvarId,
    golden_bonus_scale: CvarId,
    golden_blend_start: CvarId,
    golden_blend_end: CvarId,
    corridor_shaping_strength: CvarId,
    corridor_shaping_min_speed: CvarId,
    corridor_shaping_max_angle: CvarId,
    corridor_shaping_min_alignment: CvarId,
    dev_motor: CvarId,
    dev_fixed_dt: CvarId,
    dev_substeps: CvarId,
}

#[derive(Clone, Copy, Debug)]
struct CollisionDebugCvars {
    dev_collision_draw: CvarId,
}

impl InputState {
    fn jump_active(&self) -> bool {
        self.jump_keyboard || self.jump_mouse
    }
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
    welcome_shown: bool,
    history: VecDeque<String>,
    history_cursor: Option<usize>,
    history_draft: Option<String>,
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
            welcome_shown: false,
            history: VecDeque::new(),
            history_cursor: None,
            history_draft: None,
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

    fn clear_history_nav(&mut self) {
        self.history_cursor = None;
        self.history_draft = None;
    }

    fn push_history(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.back().is_some_and(|entry| entry == trimmed) {
            return;
        }
        self.history.push_back(trimmed.to_string());
        while self.history.len() > CONSOLE_HISTORY_LIMIT {
            self.history.pop_front();
        }
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_index = match self.history_cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft = Some(self.buffer.clone());
                self.history.len().saturating_sub(1)
            }
        };
        self.history_cursor = Some(next_index);
        if let Some(entry) = self.history.get(next_index) {
            self.buffer = entry.clone();
        }
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        let next_index = index.saturating_add(1);
        if next_index >= self.history.len() {
            self.history_cursor = None;
            self.buffer = self.history_draft.take().unwrap_or_default();
        } else {
            self.history_cursor = Some(next_index);
            if let Some(entry) = self.history.get(next_index) {
                self.buffer = entry.clone();
            }
        }
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
    }

    fn clear_log(&mut self) {
        self.log.clear();
        self.bump_log_revision();
        self.clear_selection();
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

impl CommandOutput for ConsoleState {
    fn push_line(&mut self, line: String) {
        ConsoleState::push_line(self, line);
    }

    fn clear(&mut self) {
        self.clear_log();
    }

    fn set_max_lines(&mut self, max: usize) {
        self.max_lines = self.max_lines.max(max);
    }

    fn reset_scroll(&mut self) {
        self.scroll_offset = 0.0;
    }
}

#[derive(Clone)]
struct ConsoleAsyncSender {
    sender: mpsc::Sender<String>,
}

impl ConsoleAsyncSender {
    fn send_line(&self, line: impl Into<String>) {
        let _ = self.sender.send(line.into());
    }

    fn send_lines<I>(&self, lines: I)
    where
        I: IntoIterator<Item = String>,
    {
        for line in lines {
            let _ = self.sender.send(line);
        }
    }
}

fn drain_console_async(receiver: &mpsc::Receiver<String>, console: &mut ConsoleState) {
    while let Ok(line) = receiver.try_recv() {
        console.push_line(line);
    }
}

const CONSOLE_WELCOME_FILE: &str = "console_welcome.txt";

fn push_console_welcome(console: &mut ConsoleState, asset_manager: &AssetManager) {
    if console.welcome_shown {
        return;
    }
    console.welcome_shown = true;
    if let Some(lines) = load_console_welcome_lines(asset_manager) {
        for line in lines {
            console.push_line(line);
        }
        return;
    }
    console.push_line("^8Welcome to the Pallet console.".to_string());
    console.push_line(
        "^8Colors: ^0black ^1red ^2orange ^3yellow ^4green ^5blue ^6indigo ^7violet ^8white"
            .to_string(),
    );
    console.push_line("^8Use caret (^) + a digit 0-8 before text to change color.".to_string());
    console.push_line("^8Example: ^1ye^8s ^2or^3an^4ge".to_string());
}

fn load_console_welcome_lines(asset_manager: &AssetManager) -> Option<Vec<String>> {
    let key = AssetKey::from_parts(
        "engine",
        "config",
        &format!("console/{}", CONSOLE_WELCOME_FILE),
    )
    .ok()?;
    let handle = asset_manager.request::<ConfigAsset>(
        key,
        RequestOpts {
            priority: AssetPriority::High,
            budget_tag: AssetBudgetTag::Boot,
        },
    );
    let asset = asset_manager
        .await_ready(&handle, Duration::from_secs(2))
        .ok()?;
    Some(asset.text.lines().map(|line| line.to_string()).collect())
}

fn load_smoke_script(asset_manager: &AssetManager, input: &str) -> Result<SmokeScript, String> {
    let key = config_asset_key("scripts", input)?;
    let handle = asset_manager.request::<ConfigAsset>(
        key.clone(),
        RequestOpts {
            priority: AssetPriority::High,
            budget_tag: AssetBudgetTag::Boot,
        },
    );
    let asset = asset_manager
        .await_ready(&handle, Duration::from_secs(2))
        .map_err(|err| format!("smoke load failed ({}): {}", key.canonical(), err))?;
    let mut lines = Vec::new();
    for (index, line) in asset.text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        lines.push(SmokeLine {
            number: index + 1,
            text: trimmed.to_string(),
        });
    }
    if lines.is_empty() {
        return Err(format!("smoke script is empty: {}", key.canonical()));
    }
    Ok(SmokeScript {
        label: key.canonical().to_string(),
        lines,
    })
}

fn config_profiles_dir() -> PathBuf {
    config_path_for_profile(default_profile_name())
        .parent()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("config"))
}

fn normalize_profile_name(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("profile name is empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains(':') {
        return Err("profile name must be a file name (no path separators)".to_string());
    }
    let name = if trimmed.to_ascii_lowercase().ends_with(".cfg") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.cfg")
    };
    Ok(name)
}

fn list_config_profiles(path_policy: &PathPolicy) -> Vec<String> {
    let mut profiles = std::collections::BTreeSet::new();
    let user_dir = config_profiles_dir();
    if let Ok(entries) = std::fs::read_dir(&user_dir) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if !file_type.is_file() {
                    continue;
                }
            }
            if let Some(name) = entry.file_name().to_str() {
                if name.to_ascii_lowercase().ends_with(".cfg") {
                    profiles.insert(name.to_string());
                }
            }
        }
    }
    let shipped_dir = path_policy.content_root().join("config").join("cvars");
    if let Ok(entries) = std::fs::read_dir(&shipped_dir) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if !file_type.is_file() {
                    continue;
                }
            }
            if let Some(name) = entry.file_name().to_str() {
                if name.to_ascii_lowercase().ends_with(".cfg") {
                    profiles.insert(name.to_string());
                }
            }
        }
    }
    profiles.into_iter().collect()
}

fn resolve_profile_path(path_policy: &PathPolicy, profile: &str) -> Result<PathBuf, String> {
    let user_path = config_path_for_profile(profile);
    if user_path.is_file() {
        return Ok(user_path);
    }
    let shipped_path = path_policy
        .content_root()
        .join("config")
        .join("cvars")
        .join(profile);
    if shipped_path.is_file() {
        return Ok(shipped_path);
    }
    Err(format!("config profile not found: {}", profile))
}

fn build_smoke_report_path(label: &str) -> PathBuf {
    let sanitized: String = label
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect();
    let name = if sanitized.is_empty() {
        "smoke_report".to_string()
    } else {
        sanitized
    };
    PathBuf::from(SMOKE_REPORT_DIR).join(format!("{}_report.txt", name))
}

fn write_smoke_report(
    path: &Path,
    script_label: &str,
    duration: Duration,
    global_timeout_ms: u64,
    failure: Option<&SmokeFailure>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("smoke report dir create failed: {}", err))?;
    }
    let mut report = String::new();
    report.push_str("smoke report\n");
    report.push_str(&format!("script: {}\n", script_label));
    report.push_str(&format!("duration_ms: {}\n", duration.as_millis()));
    report.push_str(&format!("global_timeout_ms: {}\n", global_timeout_ms));
    match failure {
        Some(failure) => {
            report.push_str("status: failed\n");
            report.push_str(&format!("line: {}\n", failure.line));
            report.push_str(&format!("command: {}\n", failure.command));
            report.push_str(&format!("error: {}\n", failure.reason));
        }
        None => {
            report.push_str("status: success\n");
        }
    }
    std::fs::write(path, report).map_err(|err| format!("smoke report write failed: {}", err))
}

fn format_smoke_failure(failure: &SmokeFailure) -> String {
    format!(
        "smoke error: line {}: {} ({})",
        failure.line, failure.command, failure.reason
    )
}

fn config_asset_key(subdir: &str, input: &str) -> Result<AssetKey, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("config input is empty".to_string());
    }
    if trimmed.contains(':') {
        let key = AssetKey::parse(trimmed).map_err(|err| err.to_string())?;
        if key.namespace() != "engine" || key.kind() != "config" {
            return Err(format!(
                "expected engine:config asset id (got {})",
                key.canonical()
            ));
        }
        return Ok(key);
    }
    let normalized = trimmed.replace('\\', "/");
    let prefix = format!("{}/", subdir);
    let path = if normalized.starts_with(&prefix) {
        normalized
    } else {
        format!("{}/{}", subdir, normalized)
    };
    AssetKey::from_parts("engine", "config", &path).map_err(|err| err.to_string())
}

fn script_asset_key(input: &str) -> Result<AssetKey, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("script input is empty".to_string());
    }
    if trimmed.contains(':') {
        let key = AssetKey::parse(trimmed).map_err(|err| err.to_string())?;
        if key.namespace() != "engine" || key.kind() != "script" {
            return Err(format!(
                "expected engine:script asset id (got {})",
                key.canonical()
            ));
        }
        return Ok(key);
    }
    let normalized = trimmed.replace('\\', "/");
    let path = normalized.trim_start_matches('/');
    AssetKey::from_parts("engine", "script", path).map_err(|err| err.to_string())
}

fn resolve_config_base_dir(
    path_policy: &PathPolicy,
    key: &AssetKey,
) -> Result<Option<PathBuf>, ExitError> {
    let resolver = AssetResolver::new(path_policy, None);
    let location = resolver
        .resolve(key)
        .map_err(|err| ExitError::new(EXIT_USAGE, err))?;
    match location.path {
        ResolvedPath::File(path) => Ok(path.parent().map(|dir| dir.to_path_buf())),
        ResolvedPath::Vfs(_) | ResolvedPath::Bundle { .. } => Ok(None),
    }
}

#[derive(Clone, Debug)]
struct ConsoleSpan {
    text: String,
    color: [f32; 4],
}

fn console_color_from_code(code: char) -> Option<[f32; 4]> {
    match code {
        '0' => Some(CONSOLE_COLOR_BLACK),
        '1' => Some(CONSOLE_COLOR_RED),
        '2' => Some(CONSOLE_COLOR_ORANGE),
        '3' => Some(CONSOLE_COLOR_YELLOW),
        '4' => Some(CONSOLE_COLOR_GREEN),
        '5' => Some(CONSOLE_COLOR_BLUE),
        '6' => Some(CONSOLE_COLOR_INDIGO),
        '7' => Some(CONSOLE_COLOR_VIOLET),
        '8' => Some(CONSOLE_TEXT_COLOR),
        _ => None,
    }
}

fn push_console_span(spans: &mut Vec<ConsoleSpan>, text: String, color: [f32; 4]) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.color == color {
            last.text.push_str(&text);
            return;
        }
    }
    spans.push(ConsoleSpan { text, color });
}

fn append_console_spans_for_line(
    line: &str,
    default_color: [f32; 4],
    spans: &mut Vec<ConsoleSpan>,
) {
    let mut current_color = default_color;
    let mut current_text = String::new();
    let mut trailing_codes = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '^' {
            if let Some(next) = chars.peek().copied() {
                if let Some(color) = console_color_from_code(next) {
                    push_console_span(spans, std::mem::take(&mut current_text), current_color);
                    trailing_codes.push('^');
                    trailing_codes.push(next);
                    chars.next();
                    current_color = color;
                    continue;
                }
            }
        }
        if !trailing_codes.is_empty() {
            trailing_codes.clear();
        }
        current_text.push(ch);
    }
    push_console_span(spans, current_text, current_color);
    if !trailing_codes.is_empty() {
        push_console_span(spans, trailing_codes, default_color);
    }
}

fn finalize_console_spans(spans: Vec<ConsoleSpan>) -> Vec<TextSpan> {
    spans
        .into_iter()
        .map(|span| TextSpan {
            text: Arc::from(span.text),
            color: span.color,
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum CompletionSource {
    Command,
    Cvar,
    CommandOrCvar,
}

struct ConsoleToken {
    text: String,
    start: usize,
    end: usize,
}

fn tokenize_console_input(buffer: &str) -> (Vec<ConsoleToken>, bool) {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut token_start: Option<usize> = None;
    for (index, ch) in buffer.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                tokens.push(ConsoleToken {
                    text: std::mem::take(&mut current),
                    start,
                    end: index,
                });
            }
        } else {
            if token_start.is_none() {
                token_start = Some(index);
            }
            current.push(ch);
        }
    }
    if let Some(start) = token_start {
        tokens.push(ConsoleToken {
            text: current,
            start,
            end: buffer.len(),
        });
    }
    let trailing_space = buffer.chars().last().is_some_and(|ch| ch.is_whitespace());
    (tokens, trailing_space)
}

fn completion_source_for_input(
    tokens: &[ConsoleToken],
    trailing_space: bool,
) -> Option<CompletionSource> {
    let first = tokens
        .first()
        .map(|token| token.text.as_str())
        .unwrap_or("");
    let token_index = if trailing_space {
        tokens.len()
    } else {
        tokens.len().saturating_sub(1)
    };
    if token_index == 0 {
        return Some(CompletionSource::Command);
    }
    match first {
        "cvar_get" | "cvar_set" | "cvar_list" | "cvar_toggle" => Some(CompletionSource::Cvar),
        "help" => Some(CompletionSource::CommandOrCvar),
        "cmd_list" => Some(CompletionSource::Command),
        _ => None,
    }
}

fn collect_command_names() -> Result<Vec<String>, String> {
    let mut registry: CommandRegistry<()> = CommandRegistry::new();
    register_core_commands(&mut registry)?;
    register_pallet_command_specs(&mut registry)?;
    let mut names: Vec<String> = registry
        .list_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn collect_cvar_names(cvars: &CvarRegistry) -> Vec<String> {
    let mut names: Vec<String> = cvars
        .list()
        .into_iter()
        .map(|entry| entry.def.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn common_prefix(values: &[String]) -> Option<String> {
    let mut iter = values.iter();
    let mut prefix = iter.next()?.to_string();
    for value in iter {
        let max = prefix.len().min(value.len());
        let mut len = 0usize;
        for (left, right) in prefix.chars().zip(value.chars()).take(max) {
            if left != right {
                break;
            }
            len += left.len_utf8();
        }
        prefix.truncate(len);
        if prefix.is_empty() {
            break;
        }
    }
    Some(prefix)
}

fn apply_console_completion(console: &mut ConsoleState, cvars: &CvarRegistry) {
    let (tokens, trailing_space) = tokenize_console_input(&console.buffer);
    let Some(source) = completion_source_for_input(&tokens, trailing_space) else {
        return;
    };
    let (replace_start, replace_end, prefix) = if trailing_space || tokens.is_empty() {
        (console.buffer.len(), console.buffer.len(), String::new())
    } else {
        let token = &tokens[tokens.len().saturating_sub(1)];
        (token.start, token.end, token.text.clone())
    };

    let mut candidates = match source {
        CompletionSource::Command => collect_command_names().unwrap_or_default(),
        CompletionSource::Cvar => collect_cvar_names(cvars),
        CompletionSource::CommandOrCvar => {
            let mut names = collect_command_names().unwrap_or_default();
            names.extend(collect_cvar_names(cvars));
            names.sort();
            names.dedup();
            names
        }
    };
    if !prefix.is_empty() {
        candidates.retain(|name| name.starts_with(&prefix));
    }
    if candidates.is_empty() {
        return;
    }
    if candidates.len() == 1 {
        let replacement = &candidates[0];
        console.buffer = format!(
            "{}{}{}",
            &console.buffer[..replace_start],
            replacement,
            &console.buffer[replace_end..]
        );
        return;
    }
    if let Some(shared) = common_prefix(&candidates) {
        if shared.len() > prefix.len() {
            console.buffer = format!(
                "{}{}{}",
                &console.buffer[..replace_start],
                shared,
                &console.buffer[replace_end..]
            );
        }
    }
    console.push_line(format!("matches: {}", candidates.len()));
    for name in candidates {
        console.push_line(format!("  {}", name));
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
    ) -> TextOverlayTimings {
        let mut timings = TextOverlayTimings::default();
        if update.size[0] == 0 || update.size[1] == 0 {
            self.size = update.size;
            self.texture = None;
            self.view = None;
            self.bind_group = None;
            self.last_params = Some(update.params);
            return timings;
        }
        self.ensure_texture(device, update.size);
        let view = match self.view.as_ref() {
            Some(view) => view,
            None => return timings,
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
        timings = log_overlay.flush_layers_with_timings(
            &mut pass,
            update.viewport,
            device,
            queue,
            &[TextLayer::ConsoleLog],
        );
        self.last_params = Some(update.params);
        timings
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
    stats_text: Arc<str>,
    stats_last_update: Instant,
    stats_dirty: bool,
    stats_fps: f32,
    stats_sim: f32,
    stats_net: f32,
}

impl HudState {
    fn new(now: Instant) -> Self {
        Self {
            frame_count: 0,
            last_sample: now,
            fps: 0.0,
            stats_text: Arc::from(""),
            stats_last_update: now,
            stats_dirty: true,
            stats_fps: -1.0,
            stats_sim: -1.0,
            stats_net: -1.0,
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
            self.stats_dirty = true;
        }
        self.fps
    }

    fn mark_stats_dirty(&mut self) {
        self.stats_dirty = true;
    }

    fn stats_text(&mut self, now: Instant, fps: f32, sim_rate: f32, net_rate: f32) -> Arc<str> {
        let elapsed = now.saturating_duration_since(self.stats_last_update);
        if self.stats_dirty || elapsed >= Duration::from_millis(HUD_STATS_UPDATE_MS) {
            self.stats_last_update = now;
            self.stats_dirty = false;
            let fps_display = fps.round();
            let sim_display = sim_rate.round();
            let net_display = net_rate.round();
            if fps_display != self.stats_fps
                || sim_display != self.stats_sim
                || net_display != self.stats_net
            {
                self.stats_fps = fps_display;
                self.stats_sim = sim_display;
                self.stats_net = net_display;
                self.stats_text = Arc::from(format!(
                    "fps: {:>4.0}\nsim: {:>4.0} hz\nnet: {:>4.0} hz",
                    fps_display, sim_display, net_display
                ));
            }
        }
        Arc::clone(&self.stats_text)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PerfTimings {
    egui_build_ms: f32,
    glyphon_prepare_ms: f32,
    glyphon_render_ms: f32,
}

#[derive(Clone, Copy, Debug)]
struct PerfBudgets {
    egui_ms: f32,
    glyphon_prepare_ms: f32,
    glyphon_render_ms: f32,
}

impl Default for PerfBudgets {
    fn default() -> Self {
        Self {
            egui_ms: PERF_BUDGET_EGUI_MS,
            glyphon_prepare_ms: PERF_BUDGET_GLYPHON_PREP_MS,
            glyphon_render_ms: PERF_BUDGET_GLYPHON_RENDER_MS,
        }
    }
}

struct StressState {
    end_at: Instant,
    edit_end: Instant,
    next_edit: Instant,
    edit_index: usize,
    glyph_text: Arc<str>,
    font_px: f32,
    line_height: f32,
    cols: usize,
    rows: usize,
    glyphs: usize,
}

struct PerfState {
    show_overlay: bool,
    budgets: PerfBudgets,
    last: PerfTimings,
    stress: Option<StressState>,
    stress_requested: bool,
    hud_text: Arc<str>,
    hud_last_update: Instant,
    hud_dirty: bool,
    hud_snapshot: PerfHudSnapshot,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PerfHudSnapshot {
    egui_ms: f32,
    glyphon_prepare_ms: f32,
    glyphon_render_ms: f32,
    stress_glyphs: usize,
    stress_cols: usize,
    stress_rows: usize,
}

struct DebugOverlayState {
    text: Arc<str>,
    line_count: usize,
    last_update: Instant,
    enabled_last: bool,
}

impl DebugOverlayState {
    fn new(now: Instant) -> Self {
        Self {
            text: Arc::from(""),
            line_count: 0,
            last_update: now - Duration::from_millis(HUD_STATS_UPDATE_MS),
            enabled_last: false,
        }
    }

    fn update(
        &mut self,
        now: Instant,
        enabled: bool,
        build_lines: impl FnOnce() -> Vec<String>,
    ) -> Option<(Arc<str>, usize)> {
        if !enabled {
            self.enabled_last = false;
            self.line_count = 0;
            return None;
        }
        let mut refresh = false;
        if !self.enabled_last {
            self.enabled_last = true;
            refresh = true;
        }
        if now.saturating_duration_since(self.last_update)
            >= Duration::from_millis(HUD_STATS_UPDATE_MS)
        {
            refresh = true;
        }
        if refresh {
            self.last_update = now;
            let lines = build_lines();
            self.line_count = lines.len();
            self.text = Arc::from(lines.join("\n"));
        }
        if self.line_count == 0 {
            None
        } else {
            Some((Arc::clone(&self.text), self.line_count))
        }
    }
}

impl PerfState {
    fn new() -> Self {
        Self {
            show_overlay: false,
            budgets: PerfBudgets::default(),
            last: PerfTimings::default(),
            stress: None,
            stress_requested: false,
            hud_text: Arc::from(""),
            hud_last_update: Instant::now(),
            hud_dirty: true,
            hud_snapshot: PerfHudSnapshot::default(),
        }
    }

    fn update(&mut self, timings: PerfTimings) {
        self.last = timings;
    }

    fn hud_text(&mut self, now: Instant) -> Arc<str> {
        let elapsed = now.saturating_duration_since(self.hud_last_update);
        if self.hud_dirty || elapsed >= Duration::from_millis(PERF_HUD_UPDATE_MS) {
            self.hud_last_update = now;
            self.hud_dirty = false;
            let display = PerfTimings {
                egui_build_ms: quantize_ms(self.last.egui_build_ms),
                glyphon_prepare_ms: quantize_ms(self.last.glyphon_prepare_ms),
                glyphon_render_ms: quantize_ms(self.last.glyphon_render_ms),
            };
            let (stress_glyphs, stress_cols, stress_rows) =
                if let Some(stress) = self.stress.as_ref() {
                    (stress.glyphs, stress.cols, stress.rows)
                } else {
                    (0, 0, 0)
                };
            let snapshot = PerfHudSnapshot {
                egui_ms: display.egui_build_ms,
                glyphon_prepare_ms: display.glyphon_prepare_ms,
                glyphon_render_ms: display.glyphon_render_ms,
                stress_glyphs,
                stress_cols,
                stress_rows,
            };
            if snapshot != self.hud_snapshot {
                self.hud_snapshot = snapshot;
                let lines = perf_summary_lines_with(self.budgets, display, self.stress.as_ref());
                let mut text = lines[0].clone();
                for line in lines.iter().skip(1) {
                    text.push('\n');
                    text.push_str(line);
                }
                self.hud_text = Arc::from(text);
            }
        }
        Arc::clone(&self.hud_text)
    }

    fn request_stress_toggle(&mut self) -> bool {
        let enable = !self.stress_enabled();
        self.set_stress_enabled(enable);
        enable
    }

    fn stress_enabled(&self) -> bool {
        self.stress.is_some() || self.stress_requested
    }

    fn set_stress_enabled(&mut self, enabled: bool) -> bool {
        if enabled {
            if self.stress_enabled() {
                return false;
            }
            self.stress_requested = true;
            self.hud_dirty = true;
            true
        } else {
            if !self.stress_enabled() {
                return false;
            }
            self.stress = None;
            self.stress_requested = false;
            true
        }
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
const INPUT_TRACE_DIR: &str = ".pallet/input_traces";

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

#[derive(Clone)]
struct InputTraceFrame {
    dt: f32,
    yaw: f32,
    pitch: f32,
    input: InputState,
}

struct InputTraceRecorder {
    name: String,
    frames: Vec<InputTraceFrame>,
}

impl InputTraceRecorder {
    fn new(name: String) -> Self {
        Self {
            name,
            frames: Vec::new(),
        }
    }

    fn push_frame(&mut self, dt: f32, input: InputState, camera: &CameraState) {
        self.frames.push(InputTraceFrame {
            dt,
            yaw: camera.yaw,
            pitch: camera.pitch,
            input,
        });
    }
}

struct InputTracePlayback {
    name: String,
    frames: Vec<InputTraceFrame>,
    index: usize,
}

impl InputTracePlayback {
    fn new(name: String, frames: Vec<InputTraceFrame>) -> Self {
        Self {
            name,
            frames,
            index: 0,
        }
    }

    fn next_frame(&mut self) -> Option<InputTraceFrame> {
        if self.index >= self.frames.len() {
            None
        } else {
            let frame = self.frames[self.index].clone();
            self.index = self.index.saturating_add(1);
            Some(frame)
        }
    }
}

fn input_trace_path(name: &str) -> Result<PathBuf, String> {
    if name.trim().is_empty() {
        return Err("input trace name must not be empty".to_string());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
    {
        return Err("input trace name must be [a-z0-9_-]".to_string());
    }
    Ok(PathBuf::from(INPUT_TRACE_DIR).join(format!("{name}.trace")))
}

fn serialize_input_trace(frames: &[InputTraceFrame]) -> String {
    let mut text = String::from("# pallet_input_trace_v1\n");
    for frame in frames {
        text.push_str(&format!(
            "{:.6} {:.6} {:.6} {} {} {} {} {} {}\n",
            frame.dt,
            frame.yaw,
            frame.pitch,
            bool_to_bit(frame.input.forward),
            bool_to_bit(frame.input.back),
            bool_to_bit(frame.input.left),
            bool_to_bit(frame.input.right),
            bool_to_bit(frame.input.jump_active()),
            bool_to_bit(frame.input.down),
        ));
    }
    text
}

fn parse_input_trace(text: &str) -> Result<Vec<InputTraceFrame>, String> {
    let mut frames = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() != 9 {
            return Err(format!("trace line {} malformed", index + 1));
        }
        let dt = parts[0]
            .parse::<f32>()
            .map_err(|_| format!("trace line {} dt invalid", index + 1))?;
        let yaw = parts[1]
            .parse::<f32>()
            .map_err(|_| format!("trace line {} yaw invalid", index + 1))?;
        let pitch = parts[2]
            .parse::<f32>()
            .map_err(|_| format!("trace line {} pitch invalid", index + 1))?;
        let forward = parse_trace_bit(parts[3], index)?;
        let back = parse_trace_bit(parts[4], index)?;
        let left = parse_trace_bit(parts[5], index)?;
        let right = parse_trace_bit(parts[6], index)?;
        let jump = parse_trace_bit(parts[7], index)?;
        let down = parse_trace_bit(parts[8], index)?;
        let input = InputState {
            forward,
            back,
            left,
            right,
            jump_keyboard: jump,
            jump_mouse: false,
            down,
        };
        frames.push(InputTraceFrame {
            dt,
            yaw,
            pitch,
            input,
        });
    }
    Ok(frames)
}

fn parse_trace_bit(value: &str, line_index: usize) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("trace line {} bool invalid", line_index + 1)),
    }
}

fn bool_to_bit(value: bool) -> u8 {
    if value {
        1
    } else {
        0
    }
}

fn write_input_trace(recorder: InputTraceRecorder) -> Result<PathBuf, String> {
    let path = input_trace_path(&recorder.name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("trace dir create failed: {}", err))?;
    }
    let text = serialize_input_trace(&recorder.frames);
    std::fs::write(&path, text).map_err(|err| format!("trace write failed: {}", err))?;
    Ok(path)
}

fn load_input_trace(name: &str) -> Result<InputTracePlayback, String> {
    let path = input_trace_path(name)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("trace read failed ({}): {}", path.display(), err))?;
    let frames = parse_input_trace(&text)?;
    if frames.is_empty() {
        return Err("trace contains no frames".to_string());
    }
    Ok(InputTracePlayback::new(name.to_string(), frames))
}

#[derive(Default)]
struct SettingsChangeFlags {
    settings_changed: bool,
    display_changed: bool,
}

impl SettingsChangeFlags {
    fn mark_settings(&mut self, display_changed: bool) {
        self.settings_changed = true;
        if display_changed {
            self.display_changed = true;
        }
    }

    fn take_settings_changed(&mut self) -> bool {
        std::mem::take(&mut self.settings_changed)
    }

    fn take_display_changed(&mut self) -> bool {
        std::mem::take(&mut self.display_changed)
    }
}

#[derive(Clone, Debug)]
struct SmokeLine {
    number: usize,
    text: String,
}

#[derive(Clone, Debug)]
struct SmokeScript {
    label: String,
    lines: Vec<SmokeLine>,
}

#[derive(Clone, Debug)]
struct SmokeFailure {
    line: usize,
    command: String,
    reason: String,
}

enum SmokeState {
    Ready,
    Sleeping {
        until: Instant,
        deadline: Instant,
        line_index: usize,
    },
    WaitingCapture {
        target: u64,
        deadline: Instant,
        line_index: usize,
        failure_count: u64,
    },
}

enum SmokeTick {
    Running,
    Success,
    Failed(SmokeFailure),
}

struct SmokeRunner {
    script: SmokeScript,
    index: usize,
    start_at: Instant,
    global_deadline: Instant,
    global_timeout_ms: u64,
    step_timeout_ms: u64,
    report_path: PathBuf,
    state: SmokeState,
}

impl SmokeRunner {
    fn new(
        script: SmokeScript,
        now: Instant,
        global_timeout_ms: u64,
        report_path: PathBuf,
    ) -> Self {
        Self {
            script,
            index: 0,
            start_at: now,
            global_deadline: now + Duration::from_millis(global_timeout_ms),
            global_timeout_ms,
            step_timeout_ms: SMOKE_DEFAULT_STEP_TIMEOUT_MS,
            report_path,
            state: SmokeState::Ready,
        }
    }

    fn active_line_index(&self) -> Option<usize> {
        match &self.state {
            SmokeState::Ready => {
                if self.index < self.script.lines.len() {
                    Some(self.index)
                } else {
                    None
                }
            }
            SmokeState::Sleeping { line_index, .. } => Some(*line_index),
            SmokeState::WaitingCapture { line_index, .. } => Some(*line_index),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tick(
        &mut self,
        now: Instant,
        console: &mut ConsoleState,
        perf: &mut PerfState,
        cvars: &mut CvarRegistry,
        core_cvars: &CoreCvars,
        script: Option<&mut ScriptRuntime>,
        path_policy: &PathPolicy,
        asset_manager: &AssetManager,
        upload_queue: UploadQueue,
        quake_vfs: Option<Arc<Vfs>>,
        quake_dir: Option<PathBuf>,
        console_async: ConsoleAsyncSender,
        log_filter_state: &Arc<Mutex<LogFilterState>>,
        capture_requests: &mut VecDeque<CaptureRequest>,
        settings: &mut Settings,
        settings_flags: &mut SettingsChangeFlags,
        capture_completed: u64,
        capture_failures: u64,
        capture_last_error: Option<&str>,
    ) -> SmokeTick {
        if now >= self.global_deadline {
            let reason = format!("global timeout after {} ms", self.global_timeout_ms);
            return self.fail(self.active_line_index(), reason);
        }

        match &self.state {
            SmokeState::Sleeping {
                until,
                deadline,
                line_index,
            } => {
                if now >= *until {
                    self.state = SmokeState::Ready;
                    return SmokeTick::Running;
                }
                if now >= *deadline {
                    let reason = format!("step timeout after {} ms", self.step_timeout_ms);
                    return self.fail(Some(*line_index), reason);
                }
                return SmokeTick::Running;
            }
            SmokeState::WaitingCapture {
                target,
                deadline,
                line_index,
                failure_count,
            } => {
                if capture_failures > *failure_count {
                    let reason = capture_last_error
                        .map(|value| format!("capture failed: {}", value))
                        .unwrap_or_else(|| "capture failed".to_string());
                    return self.fail(Some(*line_index), reason);
                }
                if capture_completed >= *target {
                    self.state = SmokeState::Ready;
                    return SmokeTick::Running;
                }
                if now >= *deadline {
                    let reason = format!("capture timeout after {} ms", self.step_timeout_ms);
                    return self.fail(Some(*line_index), reason);
                }
                return SmokeTick::Running;
            }
            SmokeState::Ready => {}
        }

        if self.index >= self.script.lines.len() {
            return SmokeTick::Success;
        }

        let line_index = self.index;
        let line = &self.script.lines[line_index];
        let parsed = match parse_command_line(&line.text) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => {
                self.index = self.index.saturating_add(1);
                return SmokeTick::Running;
            }
            Err(err) => {
                return self.fail(Some(line_index), format!("parse error: {}", err));
            }
        };
        let name = parsed.name.as_str();
        if name == "ttimeout_ms" {
            match parse_smoke_timeout_ms(&parsed.args, "ttimeout_ms <ms>") {
                Ok(ms) => {
                    self.step_timeout_ms = ms;
                    self.index = self.index.saturating_add(1);
                    return SmokeTick::Running;
                }
                Err(err) => return self.fail(Some(line_index), err),
            }
        }
        if name == "sleep_ms" {
            match parse_smoke_sleep_ms(&parsed.args, "sleep_ms <ms>") {
                Ok(ms) => {
                    let until = now + Duration::from_millis(ms);
                    let deadline = now + Duration::from_millis(self.step_timeout_ms);
                    self.state = SmokeState::Sleeping {
                        until,
                        deadline,
                        line_index,
                    };
                    self.index = self.index.saturating_add(1);
                    return SmokeTick::Running;
                }
                Err(err) => return self.fail(Some(line_index), err),
            }
        }

        let is_capture = matches!(name, "capture_screenshot" | "capture_frame");
        let pending_before = capture_requests.len() as u64;
        if let Err(err) = dispatch_smoke_command(
            &parsed,
            console,
            perf,
            cvars,
            core_cvars,
            script,
            path_policy,
            asset_manager,
            upload_queue,
            quake_vfs,
            quake_dir,
            console_async,
            log_filter_state,
            capture_requests,
            settings,
            settings_flags,
        ) {
            return self.fail(Some(line_index), err);
        }
        if is_capture {
            let deadline = now + Duration::from_millis(self.step_timeout_ms);
            let target = capture_completed.saturating_add(pending_before + 1);
            self.state = SmokeState::WaitingCapture {
                target,
                deadline,
                line_index,
                failure_count: capture_failures,
            };
        }
        self.index = self.index.saturating_add(1);
        SmokeTick::Running
    }

    fn fail(&self, line_index: Option<usize>, reason: String) -> SmokeTick {
        let (line_number, command) = line_index
            .and_then(|idx| self.script.lines.get(idx))
            .map(|line| (line.number, line.text.clone()))
            .unwrap_or_else(|| (0, "<none>".to_string()));
        SmokeTick::Failed(SmokeFailure {
            line: line_number,
            command,
            reason,
        })
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
    quake_vfs: Option<Arc<Vfs>>,
    audio: Option<Rc<AudioEngine>>,
    asset_manager: AssetManager,
}

impl ScriptHostState {
    fn new(
        quake_vfs: Option<Arc<Vfs>>,
        audio: Option<Rc<AudioEngine>>,
        asset_manager: AssetManager,
    ) -> Self {
        Self {
            next_id: 1,
            entities: Vec::new(),
            quake_vfs,
            audio,
            asset_manager,
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
        if self.quake_vfs.is_none() {
            return Err("quake mounts are required for play_sound".to_string());
        }
        let data = load_wav_sfx(&self.asset_manager, &asset, AssetBudgetTag::Streaming)
            .map_err(|err| err.message)?;
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
        let buttons = if input.jump_active() { 1 } else { 0 };
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

        if self.on_ground && input.jump_active() {
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
        if input.jump_active() {
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
    observability::install_panic_hook();
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

    let path_policy = PathPolicy::from_overrides(PathOverrides {
        content_root: args.content_root.clone(),
        dev_override_root: args.dev_root.clone(),
        user_config_root: args.user_config_root.clone(),
    });

    let quake_vfs = match build_mounts(&args, &path_policy) {
        Ok(vfs) => vfs,
        Err(err) => {
            eprintln!("{}", err.message);
            std::process::exit(err.code);
        }
    };
    let asset_manager = AssetManager::new(path_policy.clone(), quake_vfs.clone(), None);
    let quake_dir = args.quake_dir.clone();

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
    let mut last_fullscreen_mode = if matches!(
        settings.window_mode,
        WindowMode::Fullscreen | WindowMode::Borderless
    ) {
        settings.window_mode
    } else {
        WindowMode::Fullscreen
    };
    let mut window_focused = true;
    let mut focus_resume_mouse_look = false;
    let mut console_fullscreen_override = false;
    let mut focus_fullscreen_override = false;
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
    let upload_queue = renderer.upload_queue();
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
    if audio.is_some() && quake_vfs.is_some() {
        match load_wav_sfx(&asset_manager, DEFAULT_SFX, AssetBudgetTag::Boot) {
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
    let mut playlist_entries = if let Some(playlist_name) = args.playlist.as_deref() {
        let key = match config_asset_key("playlists", playlist_name) {
            Ok(key) => key,
            Err(err) => {
                eprintln!("playlist asset id invalid: {}", err);
                std::process::exit(EXIT_USAGE);
            }
        };
        let resolver = AssetResolver::new(&path_policy, None);
        if let Ok(report) = resolver.explain(&key) {
            for line in format_resolve_report(&report) {
                println!("{}", line);
            }
        }
        let handle = asset_manager.request::<ConfigAsset>(
            key.clone(),
            RequestOpts {
                priority: AssetPriority::High,
                budget_tag: AssetBudgetTag::Boot,
            },
        );
        let asset = match asset_manager.await_ready(&handle, Duration::from_secs(2)) {
            Ok(asset) => asset,
            Err(err) => {
                eprintln!("playlist load failed ({}): {}", key.canonical(), err);
                std::process::exit(EXIT_USAGE);
            }
        };
        let base_dir = match resolve_config_base_dir(&path_policy, &key) {
            Ok(base_dir) => base_dir,
            Err(err) => {
                eprintln!("{}", err.message);
                std::process::exit(err.code);
            }
        };
        match parse_playlist_entries(&asset.text, base_dir.as_deref()) {
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

    let lua_command_queue: Rc<RefCell<VecDeque<String>>> = Rc::new(RefCell::new(VecDeque::new()));
    let mut script: Option<ScriptRuntime> = None;
    if let Some(script_name) = args.script.as_deref() {
        let key = match script_asset_key(script_name) {
            Ok(key) => key,
            Err(err) => {
                eprintln!("script asset id invalid: {}", err);
                std::process::exit(EXIT_USAGE);
            }
        };
        let resolver = AssetResolver::new(&path_policy, None);
        if let Ok(report) = resolver.explain(&key) {
            for line in format_resolve_report(&report) {
                println!("{}", line);
            }
        }
        let host_state = Rc::new(RefCell::new(ScriptHostState::new(
            quake_vfs.clone(),
            audio.clone(),
            asset_manager.clone(),
        )));
        let spawn_state = Rc::clone(&host_state);
        let sound_state = Rc::clone(&host_state);
        let cmd_queue = Rc::clone(&lua_command_queue);
        let callbacks = HostCallbacks {
            spawn_entity: Box::new(move |request| spawn_state.borrow_mut().spawn_entity(request)),
            play_sound: Box::new(move |asset| sound_state.borrow_mut().play_sound(asset)),
            log: Box::new(move |msg| {
                println!("[lua] {}", msg);
            }),
            run_command: Box::new(move |line| {
                cmd_queue.borrow_mut().push_back(line);
                Ok(())
            }),
        };
        let mut engine = match ScriptEngine::new(ScriptConfig::default(), callbacks) {
            Ok(engine) => engine,
            Err(err) => {
                eprintln!("script init failed: {}", err);
                std::process::exit(EXIT_USAGE);
            }
        };
        let handle = asset_manager.request::<ScriptAsset>(
            key,
            RequestOpts {
                priority: AssetPriority::High,
                budget_tag: AssetBudgetTag::Boot,
            },
        );
        let asset = match asset_manager.await_ready(&handle, Duration::from_secs(2)) {
            Ok(asset) => asset,
            Err(err) => {
                eprintln!("script load failed: {}", err);
                std::process::exit(EXIT_USAGE);
            }
        };
        if let Err(err) = engine.load_script(&asset.text) {
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
    let (console_async_tx, console_async_receiver) = mpsc::channel::<String>();
    let console_async_sender = ConsoleAsyncSender {
        sender: console_async_tx,
    };
    let log_filter_state = Arc::new(Mutex::new(LogFilterState::default()));
    let mut capture_requests: VecDeque<CaptureRequest> = VecDeque::new();
    let mut capture_sequence: u32 = 0;
    let mut capture_frame: Option<FrameCapture> = None;
    let mut capture_completed: u64 = 0;
    let mut capture_failures: u64 = 0;
    let mut capture_last_error: Option<String> = None;
    let mut current_map: Option<String> = None;
    let mut modifiers = ModifiersState::default();
    let mut last_cursor_pos = PhysicalPosition::new(0.0, 0.0);
    let mut input_router = InputRouter::new();
    let mut ui_facade = UiFacade::new(window, renderer.device(), renderer.surface_format());
    let mut ui_state = UiState::default();
    let mut settings_flags = SettingsChangeFlags::default();
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
    let mut perf = PerfState::new();
    let mut debug_overlay = DebugOverlayState::new(Instant::now());
    let mut cvars = CvarRegistry::new();
    let core_cvars = match register_core_cvars(&mut cvars) {
        Ok(core_cvars) => core_cvars,
        Err(err) => {
            eprintln!("cvar registry init failed: {}", err);
            std::process::exit(EXIT_USAGE);
        }
    };
    let movement_cvars = match register_movement_cvars(&mut cvars) {
        Ok(cvars) => cvars,
        Err(err) => {
            eprintln!("movement cvar init failed: {}", err);
            std::process::exit(EXIT_USAGE);
        }
    };
    let collision_debug_cvars = match register_collision_debug_cvars(&mut cvars) {
        Ok(cvars) => cvars,
        Err(err) => {
            eprintln!("collision debug cvar init failed: {}", err);
            std::process::exit(EXIT_USAGE);
        }
    };
    if let Some(value) = args.dev_motor {
        if let Err(err) = cvars.set(movement_cvars.dev_motor, CvarValue::Int(value)) {
            eprintln!("--dev-motor {}", err);
            std::process::exit(EXIT_USAGE);
        }
    }
    update_perf_overlay(&mut perf, &cvars, &core_cvars);
    update_log_filter_state(&cvars, &core_cvars, &log_filter_state);
    {
        let log_sender = console_async_sender.clone();
        let log_filter_state = Arc::clone(&log_filter_state);
        logging::set_logger(move |level, message| {
            if let Ok(state) = log_filter_state.lock() {
                if !state.allows(level, message) {
                    return;
                }
            }
            log_sender.send_line(format!("log {}: {}", level, message));
            eprintln!("[{}] {}", level, message);
        });
    }
    if let Some(value) = cvar_int(&cvars, core_cvars.asset_decode_budget_ms) {
        asset_manager.set_decode_budget_ms_per_tick(value.max(0) as u64);
    }
    let mut camera = CameraState::default();
    let mut collision: Option<SceneCollision> = None;
    let mut test_map_runtime: Option<TestMapRuntime> = None;
    let mut fly_mode = false;
    let mut scene_active = false;
    let mut loopback: Option<LoopbackNet> = None;
    let mut mouse_look = false;
    let mut mouse_grabbed = false;
    let mut ignore_cursor_move = false;
    let mut was_mouse_look = false;
    let mut pending_map: Option<String> = None;
    let mut test_map_reload_requests: VecDeque<AssetKey> = VecDeque::new();
    let mut fixed_dt_accum = 0.0_f32;

    if let Some(asset) = args.show_image.as_deref() {
        if asset.contains(':') {
            let key = match AssetKey::parse(asset) {
                Ok(key) => key,
                Err(err) => {
                    eprintln!("--show-image asset key invalid: {}", err);
                    std::process::exit(EXIT_USAGE);
                }
            };
            if key.namespace() != "engine" || key.kind() != "texture" {
                eprintln!(
                    "--show-image asset key must be engine:texture (got {})",
                    key.canonical()
                );
                std::process::exit(EXIT_USAGE);
            }
            let handle = asset_manager.request::<TextureAsset>(
                key,
                RequestOpts {
                    priority: AssetPriority::High,
                    budget_tag: AssetBudgetTag::Boot,
                },
            );
            let texture = match asset_manager.await_ready(&handle, Duration::from_secs(2)) {
                Ok(texture) => texture,
                Err(err) => {
                    eprintln!("asset load failed: {}", err);
                    std::process::exit(EXIT_IMAGE);
                }
            };
            let image = match ImageData::new(texture.width, texture.height, (*texture.rgba).clone())
            {
                Ok(image) => image,
                Err(err) => {
                    eprintln!("image decode failed: {}", err);
                    std::process::exit(EXIT_IMAGE);
                }
            };
            let upload_handle = renderer
                .upload_queue()
                .enqueue_image(image, UploadPriority::High);
            let _ = renderer.drain_uploads();
            let uploaded = match upload_handle.get() {
                Some(uploaded) => uploaded,
                None => {
                    let message = upload_handle
                        .error()
                        .unwrap_or_else(|| "image upload pending".to_string());
                    eprintln!("image upload failed: {}", message);
                    std::process::exit(EXIT_IMAGE);
                }
            };
            renderer.set_uploaded_image(&uploaded);
        } else {
            if quake_vfs.as_deref().is_none() {
                eprintln!(
                    "--show-image requires mounts (use --quake-dir, --mount-*, or --mount-manifest)"
                );
                print_usage();
                std::process::exit(EXIT_USAGE);
            }

            let image = match load_lmp_image(&asset_manager, asset) {
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
    }

    if let Some(map) = args.map.as_deref() {
        match parse_map_request(map) {
            Ok(MapRequest::Bsp(_)) => {
                if quake_vfs.is_none() {
                    eprintln!(
                        "--map requires mounts (use --quake-dir, --mount-*, or --mount-manifest)"
                    );
                    print_usage();
                    std::process::exit(EXIT_USAGE);
                }
            }
            Ok(MapRequest::TestMap(_)) => {}
            Err(err) => {
                eprintln!("--map {}", err);
                print_usage();
                std::process::exit(EXIT_USAGE);
            }
        }
        pending_map = Some(map.to_string());
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
    let mut input_trace_record: Option<InputTraceRecorder> = None;
    let mut input_trace_playback: Option<InputTracePlayback> = None;
    let mut smoke_runner = if let Some(script_name) = args.smoke_script.as_deref() {
        let script = match load_smoke_script(&asset_manager, script_name) {
            Ok(script) => script,
            Err(err) => {
                eprintln!("{}", err);
                std::process::exit(EXIT_SMOKE);
            }
        };
        let timeout_ms = args.smoke_timeout_ms.unwrap_or(SMOKE_DEFAULT_TIMEOUT_MS);
        let report_path = build_smoke_report_path(&script.label);
        println!(
            "smoke: running {} (timeout {} ms)",
            script.label, timeout_ms
        );
        console.push_line(format!(
            "smoke: running {} (timeout {} ms)",
            script.label, timeout_ms
        ));
        Some(SmokeRunner::new(
            script,
            Instant::now(),
            timeout_ms,
            report_path,
        ))
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
                        let alt_pressed = modifiers.alt_key();
                        if pressed
                            && !is_repeat
                            && alt_pressed
                            && matches!(code, KeyCode::Enter | KeyCode::NumpadEnter)
                            && ui_regression.is_none()
                        {
                            console_fullscreen_override = false;
                            focus_fullscreen_override = false;
                            if settings.window_mode == WindowMode::Windowed {
                                settings.window_mode = last_fullscreen_mode;
                            } else {
                                if settings.window_mode != WindowMode::Windowed {
                                    last_fullscreen_mode = settings.window_mode;
                                }
                                settings.window_mode = WindowMode::Windowed;
                            }
                            if settings.window_mode != WindowMode::Windowed {
                                last_fullscreen_mode = settings.window_mode;
                            }
                            apply_window_settings(window, &settings);
                            renderer.resize(renderer.window_inner_size());
                            pending_resize_clear = true;
                            if console.is_blocking()
                                && settings.window_mode == WindowMode::Fullscreen
                            {
                                enable_console_fullscreen_override(
                                    window,
                                    &settings,
                                    &mut console_fullscreen_override,
                                );
                            }
                            if ui_regression.is_none() {
                                if let Err(err) = settings.save() {
                                    eprintln!("settings save failed: {}", err);
                                }
                            }
                            last_window_mode = settings.window_mode;
                            return;
                        }
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
                            &mut perf,
                            &mut cvars,
                            &core_cvars,
                            &path_policy,
                            &asset_manager,
                            &upload_queue,
                            quake_vfs.as_ref(),
                            quake_dir.as_ref(),
                            &console_async_sender,
                            &log_filter_state,
                            &mut capture_requests,
                            &mut ui_state,
                            window,
                            &mut settings,
                            &mut settings_flags,
                            &mut test_map_reload_requests,
                            test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                            test_map_runtime.as_mut(),
                            &mut camera,
                            &mut input_trace_record,
                            &mut input_trace_playback,
                            &mut console_fullscreen_override,
                            &mut input,
                            &mut mouse_look,
                            &mut mouse_grabbed,
                            &mut was_mouse_look,
                            scene_active,
                            &mut fly_mode,
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
                                        | KeyCode::Tab
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
                    } else if input_router.active_layer(console.is_blocking(), ui_state.menu_open)
                        == InputLayer::Game
                        && button == MouseButton::Right
                    {
                        input.jump_mouse = state == ElementState::Pressed;
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
                        && input_trace_playback.is_none()
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
                    window_focused = false;
                    focus_resume_mouse_look = mouse_look;
                    mouse_look = false;
                    mouse_grabbed = set_cursor_mode(window, mouse_look);
                    if settings.window_mode == WindowMode::Fullscreen
                        && !focus_fullscreen_override
                    {
                        set_borderless_fullscreen(window);
                        focus_fullscreen_override = true;
                    }
                    if console.is_blocking() {
                        close_console(
                            &mut console,
                            &mut ui_state,
                            window,
                            &settings,
                            &mut console_fullscreen_override,
                            &mut mouse_look,
                            &mut mouse_grabbed,
                            scene_active,
                            false,
                            false,
                        );
                        console_fullscreen_override = false;
                    }
                }
                WindowEvent::Focused(true) => {
                    window_focused = true;
                    if focus_fullscreen_override
                        && settings.window_mode == WindowMode::Fullscreen
                        && !console_fullscreen_override
                    {
                        apply_window_settings(window, &settings);
                        renderer.resize(renderer.window_inner_size());
                        pending_resize_clear = true;
                    }
                    focus_fullscreen_override = false;
                    if console_fullscreen_override
                        && !console.is_blocking()
                        && settings.window_mode == WindowMode::Fullscreen
                    {
                        apply_window_settings(window, &settings);
                        renderer.resize(renderer.window_inner_size());
                        pending_resize_clear = true;
                        console_fullscreen_override = false;
                    }
                    if focus_resume_mouse_look
                        && scene_active
                        && !ui_state.menu_open
                        && !console.is_blocking()
                    {
                        mouse_look = true;
                        mouse_grabbed = set_cursor_mode(window, mouse_look);
                    }
                    focus_resume_mouse_look = false;
                }
                WindowEvent::RedrawRequested => {
                    let now = Instant::now();
                    asset_manager.begin_tick();
                    let _ = asset_manager.pump();
                    drain_console_async(&console_async_receiver, &mut console);
                    if pending_video_prewarm {
                        renderer.prewarm_yuv_pipeline();
                        pending_video_prewarm = false;
                    }
                    let mut dt = (now - last_frame).as_secs_f32().min(0.1);
                    last_frame = now;
                    if video.is_some() && console.is_blocking() {
                        console.force_closed();
                        console.buffer.clear();
                        ui_state.console_open = false;
                        window.set_ime_allowed(false);
                        if console_fullscreen_override
                            && window_focused
                            && settings.window_mode == WindowMode::Fullscreen
                            && !focus_fullscreen_override
                        {
                            apply_window_settings(window, &settings);
                            renderer.resize(renderer.window_inner_size());
                            pending_resize_clear = true;
                            console_fullscreen_override = false;
                        }
                    }
                    console.update(now);
                    update_text_stress(&mut perf, &mut console, now);
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
                                        if let Some(map) = pending_map.take() {
                                            match enter_map_scene(
                                                &mut renderer,
                                                window,
                                                &asset_manager,
                                                quake_vfs.as_deref(),
                                                &map,
                                                audio.as_ref(),
                                                &mut camera,
                                                &mut collision,
                                                &mut test_map_runtime,
                                                &mut scene_active,
                                                &mut mouse_look,
                                                &mut mouse_grabbed,
                                                &mut fly_mode,
                                                &mut loopback,
                                            ) {
                                                Ok(()) => {
                                                    ui_state.close_menu();
                                                    current_map = Some(map.clone());
                                                }
                                                Err(err) => {
                                                    eprintln!("{}", err.message);
                                                    pending_map = Some(map);
                                                    finish_input_script = true;
                                                }
                                            }
                                        } else {
                                            if !scripted.reported_missing_map {
                                                eprintln!(
                                                    "input script requires --map and mounts"
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
                                        &mut perf,
                                        &mut cvars,
                                        &core_cvars,
                                        &path_policy,
                                        &asset_manager,
                                        &upload_queue,
                                        quake_vfs.as_ref(),
                                        quake_dir.as_ref(),
                                        &console_async_sender,
                                        &log_filter_state,
                                        &mut capture_requests,
                                        &mut ui_state,
                                        window,
                                        &mut settings,
                                        &mut settings_flags,
                                        &mut test_map_reload_requests,
                                        test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                                        test_map_runtime.as_mut(),
                                        &mut camera,
                                        &mut input_trace_record,
                                        &mut input_trace_playback,
                                        &mut console_fullscreen_override,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
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
                                        &mut perf,
                                        &mut cvars,
                                        &core_cvars,
                                        &path_policy,
                                        &asset_manager,
                                        &upload_queue,
                                        quake_vfs.as_ref(),
                                        quake_dir.as_ref(),
                                        &console_async_sender,
                                        &log_filter_state,
                                        &mut capture_requests,
                                        &mut ui_state,
                                        window,
                                        &mut settings,
                                        &mut settings_flags,
                                        &mut test_map_reload_requests,
                                        test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                                        test_map_runtime.as_mut(),
                                        &mut camera,
                                        &mut input_trace_record,
                                        &mut input_trace_playback,
                                        &mut console_fullscreen_override,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
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
                                        &mut perf,
                                        &mut cvars,
                                        &core_cvars,
                                        &path_policy,
                                        &asset_manager,
                                        &upload_queue,
                                        quake_vfs.as_ref(),
                                        quake_dir.as_ref(),
                                        &console_async_sender,
                                        &log_filter_state,
                                        &mut capture_requests,
                                        &mut ui_state,
                                        window,
                                        &mut settings,
                                        &mut settings_flags,
                                        &mut test_map_reload_requests,
                                        test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                                        test_map_runtime.as_mut(),
                                        &mut camera,
                                        &mut input_trace_record,
                                        &mut input_trace_playback,
                                        &mut console_fullscreen_override,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
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
                                        &mut perf,
                                        &mut cvars,
                                        &core_cvars,
                                        &path_policy,
                                        &asset_manager,
                                        &upload_queue,
                                        quake_vfs.as_ref(),
                                        quake_dir.as_ref(),
                                        &console_async_sender,
                                        &log_filter_state,
                                        &mut capture_requests,
                                        &mut ui_state,
                                        window,
                                        &mut settings,
                                        &mut settings_flags,
                                        &mut test_map_reload_requests,
                                        test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                                        test_map_runtime.as_mut(),
                                        &mut camera,
                                        &mut input_trace_record,
                                        &mut input_trace_playback,
                                        &mut console_fullscreen_override,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
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
                                        &mut perf,
                                        &mut cvars,
                                        &core_cvars,
                                        &path_policy,
                                        &asset_manager,
                                        &upload_queue,
                                        quake_vfs.as_ref(),
                                        quake_dir.as_ref(),
                                        &console_async_sender,
                                        &log_filter_state,
                                        &mut capture_requests,
                                        &mut ui_state,
                                        window,
                                        &mut settings,
                                        &mut settings_flags,
                                        &mut test_map_reload_requests,
                                        test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                                        test_map_runtime.as_mut(),
                                        &mut camera,
                                        &mut input_trace_record,
                                        &mut input_trace_playback,
                                        &mut console_fullscreen_override,
                                        &mut input,
                                        &mut mouse_look,
                                        &mut mouse_grabbed,
                                        &mut was_mouse_look,
                                        scene_active,
                                        &mut fly_mode,
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
                    if let Some(runner) = smoke_runner.as_mut() {
                        match runner.tick(
                            now,
                            &mut console,
                            &mut perf,
                            &mut cvars,
                            &core_cvars,
                            script.as_mut(),
                            &path_policy,
                            &asset_manager,
                            upload_queue.clone(),
                            quake_vfs.clone(),
                            quake_dir.clone(),
                            console_async_sender.clone(),
                            &log_filter_state,
                            &mut capture_requests,
                            &mut settings,
                            &mut settings_flags,
                            capture_completed,
                            capture_failures,
                            capture_last_error.as_deref(),
                        ) {
                            SmokeTick::Running => {}
                            SmokeTick::Success => {
                                let duration = runner.start_at.elapsed();
                                if let Err(err) = write_smoke_report(
                                    &runner.report_path,
                                    &runner.script.label,
                                    duration,
                                    runner.global_timeout_ms,
                                    None,
                                ) {
                                    eprintln!("{}", err);
                                }
                                println!(
                                    "smoke: success (report: {})",
                                    runner.report_path.display()
                                );
                                console.push_line(format!(
                                    "smoke: success (report: {})",
                                    runner.report_path.display()
                                ));
                                exit_code_handle.set(EXIT_SUCCESS);
                                elwt.exit();
                                return;
                            }
                            SmokeTick::Failed(failure) => {
                                let duration = runner.start_at.elapsed();
                                let message = format_smoke_failure(&failure);
                                console.push_line(message.clone());
                                observability::set_sticky_error(message.clone());
                                if let Err(err) = write_smoke_report(
                                    &runner.report_path,
                                    &runner.script.label,
                                    duration,
                                    runner.global_timeout_ms,
                                    Some(&failure),
                                ) {
                                    eprintln!("{}", err);
                                }
                                eprintln!(
                                    "{} (report: {})",
                                    message,
                                    runner.report_path.display()
                                );
                                exit_code_handle.set(EXIT_SMOKE);
                                elwt.exit();
                                return;
                            }
                        }
                    }
                    let mut skip_render = false;
                    let dpi_scale = ui_regression
                        .as_ref()
                        .map(|regression| regression.dpi_scale as f64)
                        .unwrap_or_else(|| window.scale_factor());
                    let resolution =
                        ResolutionModel::new(renderer.size(), dpi_scale, settings.ui_scale);
                    if args.debug_resolution {
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
                    }
                    let frame_input = UiFrameInput {
                        dt_seconds: dt,
                        resolution,
                        audio_available: audio.is_some(),
                    };
                    let egui_start = Instant::now();
                    let mut ui_regression_checks =
                        ui_regression.as_ref().map(|_| UiRegressionChecks::new(resolution.ui_points));
                    let mut ui_ctx = ui_facade.begin_frame(frame_input);
                    ui_state.console_open = console.is_blocking();
                    let mut config_profiles = list_config_profiles(&path_policy);
                    if !config_profiles.contains(&settings.active_profile) {
                        config_profiles.push(settings.active_profile.clone());
                    }
                    config_profiles.sort();
                    ui_facade.build_ui(
                        &mut ui_ctx,
                        &mut ui_state,
                        &mut settings,
                        &config_profiles,
                    );
                    if let Some(checks) = ui_regression_checks.as_mut() {
                        checks.record_min_font(egui_min_font_px(&ui_ctx.egui_ctx));
                        checks.record_ui_bounds(ui_ctx.egui_ctx.used_rect());
                    }
                    let ui_draw = ui_facade.end_frame(ui_ctx);
                    let egui_build_ms = egui_start.elapsed().as_secs_f32() * 1000.0;
                    input_router.update_ui_focus(
                        ui_draw.output.wants_keyboard,
                        ui_draw.output.wants_pointer,
                    );
                    let settings_changed =
                        ui_draw.output.settings_changed || settings_flags.take_settings_changed();
                    let display_settings_changed = ui_draw.output.display_settings_changed
                        || settings_flags.take_display_changed();
                    if settings_changed {
                        if ui_regression.is_none() {
                            if let Err(err) = settings.save() {
                                eprintln!("settings save failed: {}", err);
                            }
                        }
                        if let Some(audio) = audio.as_ref() {
                            audio.set_master_volume(settings.master_volume);
                        }
                    }
                    if display_settings_changed {
                        apply_window_settings(window, &settings);
                        renderer.resize(renderer.window_inner_size());
                        if settings.window_mode != last_window_mode
                            || settings.resolution != last_resolution
                        {
                            window.set_visible(true);
                            pending_resize_clear = true;
                            last_window_mode = settings.window_mode;
                            last_resolution = settings.resolution;
                            if settings.window_mode != WindowMode::Windowed {
                                last_fullscreen_mode = settings.window_mode;
                            }
                        }
                        skip_render = true;
                    }
                    if ui_draw.output.start_requested {
                        if let Some(map) = pending_map.take() {
                            let result = enter_map_scene(
                                &mut renderer,
                                window,
                                &asset_manager,
                                quake_vfs.as_deref(),
                                &map,
                                audio.as_ref(),
                                &mut camera,
                                &mut collision,
                                &mut test_map_runtime,
                                &mut scene_active,
                                &mut mouse_look,
                                &mut mouse_grabbed,
                                &mut fly_mode,
                                &mut loopback,
                            );
                            match result {
                                Ok(()) => {
                                    ui_state.close_menu();
                                    mouse_look = true;
                                    mouse_grabbed = set_cursor_mode(window, mouse_look);
                                    current_map = Some(map.clone());
                                }
                                Err(err) => {
                                    eprintln!("{}", err.message);
                                    pending_map = Some(map);
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
                    drain_lua_command_queue(
                        &lua_command_queue,
                        &mut console,
                        &mut perf,
                        &mut cvars,
                        &core_cvars,
                        &path_policy,
                        &asset_manager,
                        upload_queue.clone(),
                        quake_vfs.clone(),
                        quake_dir.clone(),
                        console_async_sender.clone(),
                        &log_filter_state,
                        &mut capture_requests,
                        &mut settings,
                        &mut settings_flags,
                        &mut test_map_reload_requests,
                        test_map_runtime.as_ref().map(|runtime| runtime.key.clone()),
                        &mut test_map_runtime,
                        &mut camera,
                        &mut input_trace_record,
                        &mut input_trace_playback,
                        &mut script,
                    );

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

                    if let Some(key) = test_map_reload_requests.pop_front() {
                        let map_id = key.canonical().to_string();
                        match enter_map_scene(
                            &mut renderer,
                            window,
                            &asset_manager,
                            quake_vfs.as_deref(),
                            &map_id,
                            audio.as_ref(),
                            &mut camera,
                            &mut collision,
                            &mut test_map_runtime,
                            &mut scene_active,
                            &mut mouse_look,
                            &mut mouse_grabbed,
                            &mut fly_mode,
                            &mut loopback,
                        ) {
                            Ok(()) => {
                                current_map = Some(map_id);
                                console.push_line(format!(
                                    "test map reload complete: {}",
                                    key.canonical()
                                ));
                            }
                            Err(err) => {
                                console.push_line(format!(
                                    "test map reload failed: {}",
                                    err.message
                                ));
                            }
                        }
                    }

                    let mut finished_trace = None;
                    if let Some(playback) = input_trace_playback.as_mut() {
                        if let Some(frame) = playback.next_frame() {
                            dt = frame.dt.clamp(1.0e-4, 0.1);
                            input = frame.input;
                            camera.yaw = frame.yaw;
                            camera.pitch = frame.pitch;
                        } else {
                            finished_trace = Some(playback.name.clone());
                        }
                    }
                    if let Some(name) = finished_trace {
                        input_trace_playback = None;
                        console.push_line(format!("input replay complete: {}", name));
                    }
                    if input_trace_playback.is_none() {
                        if let Some(recorder) = input_trace_record.as_mut() {
                            if scene_active {
                                recorder.push_frame(dt, input, &camera);
                            }
                        }
                    }

                    if scene_active {
                if let Some(runtime) = test_map_runtime.as_mut() {
                    apply_movement_cvars(&cvars, &movement_cvars, runtime, &mut camera);
                    let fixed_dt = cvar_float(&cvars, movement_cvars.dev_fixed_dt)
                        .unwrap_or(0.0)
                        .max(0.0);
                    let substeps = cvar_int(&cvars, movement_cvars.dev_substeps)
                        .unwrap_or(1)
                        .clamp(1, 16) as u32;
                    if fixed_dt > 0.0 {
                        fixed_dt_accum = (fixed_dt_accum + dt).min(0.5);
                        let step_dt = fixed_dt.max(1.0e-4);
                        let mut steps = 0u32;
                        let max_steps = 8u32;
                        while fixed_dt_accum >= step_dt && steps < max_steps {
                            let sub_dt = step_dt / substeps as f32;
                            for _ in 0..substeps {
                                if fly_mode {
                                    camera.update(&input, sub_dt, None, true);
                                    sync_test_map_runtime_to_camera(runtime, &camera);
                                } else {
                                    update_test_map_runtime(runtime, &mut camera, &input, sub_dt);
                                }
                            }
                            fixed_dt_accum -= step_dt;
                            steps += 1;
                        }
                        if fly_mode {
                            if steps == 0 {
                                camera.update(&input, dt, None, true);
                                sync_test_map_runtime_to_camera(runtime, &camera);
                            }
                        } else if steps > 0 {
                            let alpha = (fixed_dt_accum / step_dt).clamp(0.0, 1.0);
                            apply_test_map_interpolation(runtime, &mut camera, alpha);
                        }
                    } else {
                        fixed_dt_accum = 0.0;
                        if fly_mode {
                            camera.update(&input, dt, None, true);
                            sync_test_map_runtime_to_camera(runtime, &camera);
                        } else {
                            update_test_map_runtime(runtime, &mut camera, &input, dt);
                        }
                    }
                } else {
                            camera.update(&input, dt, collision.as_ref(), fly_mode);
                        }
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
                    if perf.stress_requested {
                        start_text_stress(&mut perf, &mut console, resolution, font_scale, now);
                        perf.stress_requested = false;
                    }
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
                            hud.mark_stats_dirty();
                            (UI_REGRESSION_FPS, UI_REGRESSION_SIM_RATE, UI_REGRESSION_NET_RATE)
                        } else {
                            (
                                hud.update(now),
                                1.0 / dt.max(0.001),
                                if loopback.is_some() { 60.0 } else { 0.0 },
                            )
                        };
                        let hud_stats_text = hud.stats_text(now, fps, sim_rate, net_rate);
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
                            BUILD_TEXT,
                        );
                        if perf.show_overlay {
                            let perf_text = perf.hud_text(now);
                            let perf_line_height =
                                (hud_small_px * LINE_HEIGHT_SCALE).round().max(1.0);
                            let perf_height = perf_line_height * PERF_HUD_LINES as f32;
                            let perf_y = (hud_origin.y - perf_height - hud_margin)
                                .max(hud_margin)
                                .round();
                            hud_overlay.queue(
                                TextLayer::Hud,
                                hud_small_style,
                                TextPosition {
                                    x: hud_margin,
                                    y: perf_y,
                                },
                                TextBounds {
                                    width: resolution.physical_px[0] as f32,
                                    height: perf_height,
                                },
                                perf_text,
                            );
                        }
                        let dbg_overlay_enabled =
                            cvar_bool(&cvars, core_cvars.dbg_overlay).unwrap_or(false);
                        let show_collision = dbg_overlay_enabled
                            && cvar_bool(&cvars, collision_debug_cvars.dev_collision_draw)
                                .unwrap_or(false);
                        let show_movement = dbg_overlay_enabled
                            && cvar_bool(&cvars, core_cvars.dbg_movement).unwrap_or(false);
                        if let Some((dbg_text, dbg_lines)) = debug_overlay.update(
                            now,
                            dbg_overlay_enabled,
                            || {
                                let show_fps =
                                    cvar_bool(&cvars, core_cvars.dbg_fps).unwrap_or(true);
                                let show_frame =
                                    cvar_bool(&cvars, core_cvars.dbg_frame_time).unwrap_or(true);
                                let show_net =
                                    cvar_bool(&cvars, core_cvars.dbg_net).unwrap_or(false);
                                let mut lines = Vec::new();
                                if show_fps || show_frame {
                                    let fps_display = fps.round();
                                    let frame_ms = (dt * 1000.0).max(0.0);
                                    let frame_display = (frame_ms * 10.0).round() / 10.0;
                                    let line = match (show_fps, show_frame) {
                                        (true, true) => format!(
                                            "fps: {:>3} frame_ms: {:.1}",
                                            fps_display, frame_display
                                        ),
                                        (true, false) => format!("fps: {:>3}", fps_display),
                                        (false, true) => format!("frame_ms: {:.1}", frame_display),
                                        (false, false) => String::new(),
                                    };
                                    if !line.is_empty() {
                                        lines.push(line);
                                    }
                                }
                                if show_net {
                                    lines.push(format!(
                                        "net: {}",
                                        if loopback.is_some() { "loopback" } else { "offline" }
                                    ));
                                }
                                if show_collision {
                                    if let Some(runtime) = test_map_runtime.as_ref() {
                                        let collision_world = &runtime.collision_world;
                                        let interest = collision_interest_bounds(
                                            runtime.position.translation.vector,
                                            COLLISION_INTEREST_RADIUS,
                                        );
                                        let nearby_chunks = select_collision_chunks(
                                            &collision_world.world,
                                            CollisionChunkSelection::Bounds(interest),
                                        )
                                        .len();
                                        lines.push(format!(
                                            "collision: chunks={} loaded={} colliders={} tris={}",
                                            collision_world.world.chunks.len(),
                                            collision_world.loaded_chunks.len(),
                                            collision_world.collider_handles.len(),
                                            collision_world.triangle_count
                                        ));
                                        lines.push(format!(
                                            "collision_near={} kcc_ms={:.3}",
                                            nearby_chunks, runtime.kcc_query_ms
                                        ));
                                    } else {
                                        lines.push("collision: <inactive>".to_string());
                                    }
                                }
                                if show_movement {
                                    if let Some(runtime) = test_map_runtime.as_ref() {
                                        let move_axis = [
                                            bool_to_axis(input.right, input.left),
                                            bool_to_axis(input.forward, input.back),
                                        ];
                                        let (intent_mag, max_speed, golden_config) =
                                            match runtime.controller.motor().kind() {
                                                MotorKind::Arena => {
                                                    let config =
                                                        runtime.controller.motor().arena_config();
                                                    let intent = build_move_intent(
                                                        camera.yaw,
                                                        move_axis,
                                                        runtime.grounded,
                                                        runtime.ground_normal,
                                                    );
                                                    let max_speed = if runtime.grounded {
                                                        config.max_speed_ground
                                                    } else {
                                                        config.max_speed_air
                                                    };
                                                    (intent.mag, max_speed, Some(config))
                                                }
                                                MotorKind::Rpg => {
                                                    let config =
                                                        runtime.controller.motor().rpg_config();
                                                    let intent = build_move_intent_rpg(
                                                        camera.yaw,
                                                        move_axis,
                                                        runtime.grounded,
                                                        runtime.ground_normal,
                                                    );
                                                    let max_speed = if runtime.grounded {
                                                        config.max_speed_ground
                                                    } else {
                                                        config.max_speed_air
                                                    };
                                                    (intent.mag, max_speed, None)
                                                }
                                            };
                                        let intent_speed = intent_mag * max_speed;
                                        let planar_velocity = Vector::new(
                                            runtime.velocity.x,
                                            0.0,
                                            runtime.velocity.z,
                                        );
                                        let planar_speed = planar_velocity.norm();
                                        let total_speed = Vector::new(
                                            runtime.velocity.x,
                                            runtime.velocity.y,
                                            runtime.velocity.z,
                                        )
                                        .norm();
                                        lines.push(format!(
                                            "move: grounded={} speed={:.2} total={:.2}",
                                            runtime.grounded, planar_speed, total_speed
                                        ));
                                        lines.push(format!(
                                            "intent: mag={:.2} speed={:.2}",
                                            intent_mag, intent_speed
                                        ));
                                        let view_forward =
                                            Vector::new(camera.yaw.sin(), 0.0, -camera.yaw.cos());
                                        if let Some(config) = golden_config {
                                            if let Some(metrics) = golden_angle_metrics(
                                                &config,
                                                planar_velocity,
                                                view_forward,
                                            ) {
                                                lines.push(format!(
                                                    "golden: theta={:.1}deg gain={:.2} quality={:.0}%",
                                                    metrics.theta.to_degrees(),
                                                    metrics.gain,
                                                    metrics.quality * 100.0
                                                ));
                                            } else {
                                                lines.push("golden: theta=-- gain=--".to_string());
                                            }
                                        } else {
                                            lines.push("golden: -- (rpg motor)".to_string());
                                        }
                                    } else {
                                        lines.push("move: no test map runtime".to_string());
                                    }
                                }
                                let last_error = observability::sticky_error()
                                    .unwrap_or_else(|| "<none>".to_string());
                                lines.push(format!("last_error: {}", last_error));
                                lines
                            },
                        ) {
                            let dbg_line_height =
                                (hud_small_px * LINE_HEIGHT_SCALE).round().max(1.0);
                            let dbg_height = dbg_line_height * dbg_lines as f32;
                            hud_overlay.queue(
                                TextLayer::Hud,
                                hud_small_style,
                                TextPosition {
                                    x: hud_margin,
                                    y: hud_margin,
                                },
                                TextBounds {
                                    width: resolution.physical_px[0] as f32,
                                    height: dbg_height,
                                },
                                dbg_text,
                            );
                        }
                        if show_movement {
                            let cross_size = (6.0 * font_scale).round().max(2.0);
                            let cross_thickness = (1.0 * font_scale).round().max(1.0);
                            let center_x = resolution.physical_px[0] as f32 * 0.5;
                            let center_y = resolution.physical_px[1] as f32 * 0.5;
                            let cross_color = [0.95, 0.95, 0.95, 0.9];
                            hud_overlay.queue_rect(
                                TextLayer::Hud,
                                TextPosition {
                                    x: (center_x - cross_size).round(),
                                    y: (center_y - cross_thickness * 0.5).round(),
                                },
                                TextBounds {
                                    width: (cross_size * 2.0).round(),
                                    height: cross_thickness,
                                },
                                cross_color,
                            );
                            hud_overlay.queue_rect(
                                TextLayer::Hud,
                                TextPosition {
                                    x: (center_x - cross_thickness * 0.5).round(),
                                    y: (center_y - cross_size).round(),
                                },
                                TextBounds {
                                    width: cross_thickness,
                                    height: (cross_size * 2.0).round(),
                                },
                                cross_color,
                            );
                        }
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
                                        let mut raw_text = String::new();
                                        let mut spans = Vec::new();
                                        for line_offset in 0..draw_lines {
                                            if line_offset > 0 {
                                                raw_text.push('\n');
                                                push_console_span(
                                                    &mut spans,
                                                    "\n".to_string(),
                                                    CONSOLE_TEXT_COLOR,
                                                );
                                            }
                                            let line_index = start + line_offset;
                                            let line = console
                                                .log
                                                .get(line_index)
                                                .map(|value| value.as_str())
                                                .unwrap_or("");
                                            raw_text.push_str(line);
                                            append_console_spans_for_line(
                                                line,
                                                CONSOLE_TEXT_COLOR,
                                                &mut spans,
                                            );
                                        }
                                        if !raw_text.is_empty() {
                                            console_log_overlay.queue_rich(
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
                                                raw_text,
                                                finalize_console_spans(spans),
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
                                let mut raw_text = String::new();
                                raw_text.push_str("> ");
                                raw_text.push_str(&console.buffer);
                                raw_text.push_str(caret);
                                let mut spans = Vec::new();
                                push_console_span(
                                    &mut spans,
                                    "> ".to_string(),
                                    CONSOLE_TEXT_COLOR,
                                );
                                append_console_spans_for_line(
                                    &console.buffer,
                                    CONSOLE_TEXT_COLOR,
                                    &mut spans,
                                );
                                push_console_span(
                                    &mut spans,
                                    caret.to_string(),
                                    CONSOLE_TEXT_COLOR,
                                );
                                text_overlay.queue_rich(
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
                                    raw_text,
                                    finalize_console_spans(spans),
                                );
                            }
                        }
                    }
                    if !pre_video_blackout {
                        if let Some(stress) = perf.stress.as_ref() {
                            let stress_style = TextStyle {
                                font_size: stress.font_px,
                                color: [0.85, 0.85, 0.9, 0.35],
                            };
                            let stress_height = stress.line_height * stress.rows as f32;
                            text_overlay.queue(
                                TextLayer::Stress,
                                stress_style,
                                TextPosition { x: 0.0, y: 0.0 },
                                TextBounds {
                                    width: resolution.physical_px[0] as f32,
                                    height: stress_height.max(1.0),
                                },
                                stress.glyph_text.clone(),
                            );
                        }
                    }

                    let draw_ui = ui_state.menu_open;
                    let draw_text_overlay = !pre_video_blackout;
                    let mut glyphon_timings = TextOverlayTimings::default();
                    let render_overlay = |device: &wgpu::Device,
                                          queue: &wgpu::Queue,
                                          encoder: &mut wgpu::CommandEncoder,
                                          view: &wgpu::TextureView,
                                          _format: wgpu::TextureFormat| {
                        if let Some(update) = console_log_update {
                            let timings = console_log_cache.update(
                                device,
                                queue,
                                encoder,
                                &mut console_log_overlay,
                                update,
                            );
                            glyphon_timings.add(timings);
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
                                let timings = hud_overlay.flush_layers_with_timings(
                                    &mut pass,
                                    text_viewport,
                                    device,
                                    queue,
                                    &[TextLayer::Hud],
                                );
                                glyphon_timings.add(timings);
                                let timings = text_overlay.flush_layers_with_timings(
                                    &mut pass,
                                    text_viewport,
                                    device,
                                    queue,
                                    &[TextLayer::Stress, TextLayer::ConsoleBackground],
                                );
                                glyphon_timings.add(timings);
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
                            let timings = text_overlay.flush_layers_with_timings(
                                &mut pass,
                                text_viewport,
                                device,
                                queue,
                                &[TextLayer::Console, TextLayer::ConsoleMenu, TextLayer::Ui],
                            );
                            glyphon_timings.add(timings);
                        }
                    };

                    if let Some(capture) = ui_regression_capture.as_ref() {
                        if ui_regression_done {
                            return;
                        }
                        match renderer.render_with_overlay_and_capture(render_overlay, capture) {
                            Ok(()) => {
                                perf.update(PerfTimings {
                                    egui_build_ms,
                                    glyphon_prepare_ms: glyphon_timings.prepare_ms,
                                    glyphon_render_ms: glyphon_timings.render_ms,
                                });
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

                    if ui_regression_capture.is_none() {
                        if let Some(request) = capture_requests.pop_front() {
                            let include_overlays = request.include_overlays;
                            let capture_size = resolution.physical_px;
                            let needs_new_capture = capture_frame
                                .as_ref()
                                .map(|capture| capture.size() != capture_size)
                                .unwrap_or(true);
                            if needs_new_capture {
                                match FrameCapture::new(
                                    renderer.device(),
                                    capture_size,
                                    renderer.surface_format(),
                                ) {
                                    Ok(new_capture) => {
                                        capture_frame = Some(new_capture);
                                    }
                                    Err(err) => {
                                        record_capture_failure(
                                            &mut console,
                                            format!("capture error: capture init failed: {}", err),
                                            &mut capture_failures,
                                            &mut capture_last_error,
                                        );
                                        return;
                                    }
                                }
                            }
                            let Some(capture) = capture_frame.as_ref() else {
                                record_capture_failure(
                                    &mut console,
                                    "capture error: capture pipeline unavailable".to_string(),
                                    &mut capture_failures,
                                    &mut capture_last_error,
                                );
                                return;
                            };
                            let render_result = if include_overlays {
                                renderer.render_with_overlay_and_capture(render_overlay, capture)
                            } else {
                                renderer.render_with_overlay_and_capture(
                                    |_device, _queue, _encoder, _view, _format| {},
                                    capture,
                                )
                            };
                            match render_result {
                                Ok(()) => {
                                    perf.update(PerfTimings {
                                        egui_build_ms,
                                        glyphon_prepare_ms: glyphon_timings.prepare_ms,
                                        glyphon_render_ms: glyphon_timings.render_ms,
                                    });
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
                                        record_capture_failure(
                                            &mut console,
                                            "capture error: out of memory".to_string(),
                                            &mut capture_failures,
                                            &mut capture_last_error,
                                        );
                                        return;
                                    }
                                    _ => {
                                        record_capture_failure(
                                            &mut console,
                                            format!("capture error: render failed: {}", err),
                                            &mut capture_failures,
                                            &mut capture_last_error,
                                        );
                                        return;
                                    }
                                },
                                Err(RenderCaptureError::Capture(err)) => {
                                    record_capture_failure(
                                        &mut console,
                                        format!("capture error: capture encode failed: {}", err),
                                        &mut capture_failures,
                                        &mut capture_last_error,
                                    );
                                    return;
                                }
                            }
                            let rgba = match capture.read_rgba(renderer.device()) {
                                Ok(data) => data,
                                Err(err) => {
                                    record_capture_failure(
                                        &mut console,
                                        format!("capture error: readback failed: {}", err),
                                        &mut capture_failures,
                                        &mut capture_last_error,
                                    );
                                    return;
                                }
                            };
                            let path = request.path.unwrap_or_else(|| {
                                capture_sequence = capture_sequence.saturating_add(1);
                                build_default_capture_path(
                                    request.kind,
                                    capture_sequence,
                                    capture_size,
                                    settings.window_mode,
                                    current_map.as_deref(),
                                )
                            });
                            if let Err(err) = write_png(
                                &path,
                                capture_size[0],
                                capture_size[1],
                                &rgba,
                            ) {
                                record_capture_failure(
                                    &mut console,
                                    format!("capture error: write failed: {}", err),
                                    &mut capture_failures,
                                    &mut capture_last_error,
                                );
                                return;
                            }
                            capture_completed = capture_completed.saturating_add(1);
                            console.push_line(format!(
                                "{} saved: {}",
                                request.kind.label(),
                                path.display()
                            ));
                            return;
                        }
                    }

                    match renderer.render_with_overlay(render_overlay) {
                        Ok(()) => {
                            perf.update(PerfTimings {
                                egui_build_ms,
                                glyphon_prepare_ms: glyphon_timings.prepare_ms,
                                glyphon_render_ms: glyphon_timings.render_ms,
                            });
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
                    && input_trace_playback.is_none()
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
    let mut content_root = None;
    let mut dev_root = None;
    let mut user_config_root = None;
    let mut mounts = Vec::new();
    let mut mount_manifests = Vec::new();
    let mut show_image = None;
    let mut map = None;
    let mut play_movie = None;
    let mut playlist = None;
    let mut script = None;
    let mut input_script = false;
    let mut smoke_script = None;
    let mut smoke_timeout_ms = None;
    let mut ui_regression_shot = None;
    let mut ui_regression_res = None;
    let mut ui_regression_dpi = None;
    let mut ui_regression_ui_scale = None;
    let mut ui_regression_screen = None;
    let mut debug_resolution = false;
    let mut dev_motor = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quake-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--quake-dir expects a path".into()))?;
                quake_dir = Some(PathBuf::from(value));
            }
            "--content-root" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--content-root expects a path".into())
                })?;
                content_root = Some(PathBuf::from(value));
            }
            "--dev-root" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--dev-root expects a path".into()))?;
                dev_root = Some(PathBuf::from(value));
            }
            "--config-root" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--config-root expects a path".into()))?;
                user_config_root = Some(PathBuf::from(value));
            }
            "--mount-dir" => {
                let mount_point = args.next().ok_or_else(|| {
                    ArgParseError::Message("--mount-dir expects a mount point".into())
                })?;
                let path = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--mount-dir expects a path".into()))?;
                mounts.push(MountSpec {
                    kind: MountKind::Dir,
                    mount_point,
                    path: PathBuf::from(path),
                });
            }
            "--mount-pak" => {
                let mount_point = args.next().ok_or_else(|| {
                    ArgParseError::Message("--mount-pak expects a mount point".into())
                })?;
                let path = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--mount-pak expects a path".into()))?;
                mounts.push(MountSpec {
                    kind: MountKind::Pak,
                    mount_point,
                    path: PathBuf::from(path),
                });
            }
            "--mount-pk3" => {
                let mount_point = args.next().ok_or_else(|| {
                    ArgParseError::Message("--mount-pk3 expects a mount point".into())
                })?;
                let path = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--mount-pk3 expects a path".into()))?;
                mounts.push(MountSpec {
                    kind: MountKind::Pk3,
                    mount_point,
                    path: PathBuf::from(path),
                });
            }
            "--mount-manifest" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--mount-manifest expects a name or path".into())
                })?;
                mount_manifests.push(value);
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
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--playlist expects a name or path".into())
                })?;
                playlist = Some(value);
            }
            "--script" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--script expects a name or path".into())
                })?;
                script = Some(value);
            }
            "--input-script" => {
                input_script = true;
            }
            "--smoke" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--smoke expects a script".into()))?;
                smoke_script = Some(value);
            }
            "--gtimeout-ms" => {
                let value = args.next().ok_or_else(|| {
                    ArgParseError::Message("--gtimeout-ms expects milliseconds".into())
                })?;
                let parsed = value.parse::<u64>().map_err(|_| {
                    ArgParseError::Message("--gtimeout-ms expects milliseconds".into())
                })?;
                if parsed == 0 {
                    return Err(ArgParseError::Message("--gtimeout-ms must be > 0".into()));
                }
                smoke_timeout_ms = Some(parsed);
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
            "--debug-resolution" => {
                debug_resolution = true;
            }
            "--dev-motor" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--dev-motor expects 1 or 2".into()))?;
                let parsed = value
                    .parse::<i32>()
                    .map_err(|_| ArgParseError::Message("--dev-motor expects 1 or 2".into()))?;
                if !(parsed == 1 || parsed == 2) {
                    return Err(ArgParseError::Message("--dev-motor expects 1 or 2".into()));
                }
                dev_motor = Some(parsed);
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

    if smoke_timeout_ms.is_some() && smoke_script.is_none() {
        return Err(ArgParseError::Message(
            "--gtimeout-ms requires --smoke".into(),
        ));
    }
    if smoke_script.is_some() && input_script {
        return Err(ArgParseError::Message(
            "--smoke cannot be combined with --input-script".into(),
        ));
    }
    if ui_regression.is_some()
        && (show_image.is_some()
            || map.is_some()
            || play_movie.is_some()
            || playlist.is_some()
            || script.is_some()
            || input_script
            || smoke_script.is_some())
    {
        return Err(ArgParseError::Message(
            "--ui-regression-* cannot be combined with other modes".into(),
        ));
    }

    Ok(CliArgs {
        quake_dir,
        content_root,
        dev_root,
        user_config_root,
        mounts,
        mount_manifests,
        show_image,
        map,
        play_movie,
        playlist,
        script,
        input_script,
        smoke_script,
        smoke_timeout_ms,
        ui_regression,
        debug_resolution,
        dev_motor,
    })
}

fn print_usage() {
    eprintln!("usage: pallet [--quake-dir <path>] [--mount-dir <vroot> <path>] [--mount-pak <vroot> <path>] [--mount-pk3 <vroot> <path>] [--mount-manifest <name-or-path>] [--content-root <path>] [--dev-root <path>] [--config-root <path>] [--show-image <asset>] [--map <name|engine:test_map/...>] [--play-movie <file>] [--playlist <name>] [--script <name>] [--input-script] [--smoke <script> [--gtimeout-ms <ms>]] [--debug-resolution] [--dev-motor <1|2>] [--ui-regression-shot <path> --ui-regression-res <WxH> --ui-regression-dpi <scale> --ui-regression-ui-scale <scale> --ui-regression-screen <main|options>]");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --show-image gfx/conback.lmp");
    eprintln!("example: pallet --show-image engine:texture/ui/pallet_runner_gui_icon.png");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1 --script demo.lua");
    eprintln!("example: pallet --map engine:test_map/stairs_and_steps.toml");
    eprintln!("example: pallet --play-movie intro.ogv");
    eprintln!("example: pallet --playlist movies_playlist.txt");
    eprintln!("example: pallet --mount-pk3 raw/q3 \"C:\\\\Quake3\\\\baseq3\\\\pak0.pk3\" --show-image raw/q3/gfx/2d/console.tga");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1 --input-script");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1 --smoke smoke_capture.cfg --gtimeout-ms 60000");
    eprintln!(
        "example: pallet --mount-manifest default.txt --show-image raw/quake/gfx/conback.lmp"
    );
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

fn parse_smoke_timeout_ms(args: &CommandArgs, usage: &str) -> Result<u64, String> {
    if args.raw_tokens().len() != 1 {
        return Err(format!("usage: {}", usage));
    }
    let value = args
        .positional(0)
        .ok_or_else(|| format!("usage: {}", usage))?;
    let ms = value
        .parse::<u64>()
        .map_err(|_| format!("invalid timeout: {}", value))?;
    if ms == 0 {
        return Err("timeout must be > 0".to_string());
    }
    Ok(ms)
}

fn parse_smoke_sleep_ms(args: &CommandArgs, usage: &str) -> Result<u64, String> {
    if args.raw_tokens().len() != 1 {
        return Err(format!("usage: {}", usage));
    }
    let value = args
        .positional(0)
        .ok_or_else(|| format!("usage: {}", usage))?;
    let ms = value
        .parse::<u64>()
        .map_err(|_| format!("invalid sleep_ms: {}", value))?;
    Ok(ms)
}

fn settings_field_value(settings: &Settings, field: &str) -> Option<String> {
    match field {
        "ui_scale" => Some(format!("{:.3}", settings.ui_scale)),
        "vsync" => Some(format_settings_bool(settings.vsync).to_string()),
        "master_volume" => Some(format!("{:.3}", settings.master_volume)),
        "window_mode" => Some(settings.window_mode.as_str().to_string()),
        "resolution" => Some(format!(
            "{}x{}",
            settings.resolution[0], settings.resolution[1]
        )),
        "active_profile" => Some(settings.active_profile.clone()),
        _ => None,
    }
}

fn is_settings_field(field: &str) -> bool {
    matches!(
        field,
        "ui_scale" | "vsync" | "master_volume" | "window_mode" | "resolution" | "active_profile"
    )
}

fn format_settings_bool(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn parse_settings_bool(value: &str) -> Result<bool, String> {
    match value.trim() {
        "1" => Ok(true),
        "0" => Ok(false),
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("invalid bool value: {}", value)),
    }
}

fn apply_settings_field(
    settings: &mut Settings,
    field: &str,
    value: &str,
    flags: &mut SettingsChangeFlags,
) -> Result<Option<(String, String)>, String> {
    let before = settings_field_value(settings, field);
    let mut display_changed = false;
    match field {
        "ui_scale" => {
            let parsed = value
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("invalid ui_scale: {}", value))?;
            settings.ui_scale = parsed;
            settings.normalize();
        }
        "vsync" => {
            settings.vsync = parse_settings_bool(value)?;
        }
        "master_volume" => {
            let parsed = value
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("invalid master_volume: {}", value))?;
            settings.master_volume = parsed;
            settings.normalize();
        }
        "window_mode" => {
            let mode = WindowMode::parse(value)
                .ok_or_else(|| format!("invalid window_mode: {}", value))?;
            settings.window_mode = mode;
            display_changed = true;
        }
        "resolution" => {
            let parsed =
                parse_resolution(value).ok_or_else(|| format!("invalid resolution: {}", value))?;
            settings.resolution = parsed;
            settings.normalize();
            display_changed = true;
        }
        "active_profile" => {
            let profile = normalize_profile_name(value)?;
            if settings.active_profile != profile {
                settings.active_profile = profile;
            }
        }
        _ => return Ok(None),
    }
    let after = settings_field_value(settings, field);
    match (before, after) {
        (Some(old), Some(new)) if old != new => {
            flags.mark_settings(display_changed);
            Ok(Some((old, new)))
        }
        _ => Ok(None),
    }
}

fn is_persistable_cvar(entry: &engine_core::control_plane::CvarEntry) -> bool {
    let flags = entry.def.flags;
    !flags.contains(engine_core::control_plane::CvarFlags::NO_PERSIST)
        && !flags.contains(engine_core::control_plane::CvarFlags::READ_ONLY)
}

fn parse_config_kv_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() {
        None
    } else {
        Some((key.to_string(), value.to_string()))
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

fn fill_log_lines(console: &mut ConsoleState, count: usize) {
    let count = count.clamp(1, 20_000);
    console.max_lines = console.max_lines.max(count);
    console.clear_log();
    for index in 0..count {
        console.push_line(format!(
            "logfill {:>5}: The quick brown fox jumps over the lazy dog.",
            index + 1
        ));
    }
    console.scroll_offset = 0.0;
}

fn build_stress_text(cols: usize, rows: usize) -> Arc<str> {
    let pattern = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut text = String::with_capacity(rows.saturating_mul(cols + 1));
    for row in 0..rows {
        for col in 0..cols {
            let index = (row + col) % pattern.len();
            text.push(pattern[index] as char);
        }
        if row + 1 < rows {
            text.push('\n');
        }
    }
    Arc::from(text)
}

fn start_text_stress(
    perf: &mut PerfState,
    console: &mut ConsoleState,
    resolution: ResolutionModel,
    font_scale: f32,
    now: Instant,
) {
    fill_log_lines(console, STRESS_LOG_LINES);
    console.force_open(now);

    let width = resolution.physical_px[0].max(1) as f32;
    let height = resolution.physical_px[1].max(1) as f32;
    let mut font_px = (STRESS_FONT_BASE * font_scale).round().max(4.0);
    let (cols, rows, line_height, font_px) = loop {
        let char_width = (font_px * CONSOLE_MENU_CHAR_WIDTH).max(1.0);
        let line_height = (font_px * LINE_HEIGHT_SCALE).round().max(1.0);
        let cols = (width / char_width).floor().max(1.0) as usize;
        let mut rows = (height / line_height).floor().max(1.0) as usize;
        let total = cols.saturating_mul(rows);
        if total >= STRESS_GLYPH_TARGET || font_px <= 4.0 {
            if total < STRESS_GLYPH_TARGET {
                let needed_rows = STRESS_GLYPH_TARGET.div_ceil(cols);
                rows = rows.max(needed_rows);
            }
            break (cols, rows, line_height, font_px);
        }
        font_px = (font_px - 1.0).max(4.0);
    };
    let glyphs = cols.saturating_mul(rows);
    let glyph_text = build_stress_text(cols, rows);
    let end_at = now + Duration::from_millis(STRESS_EDIT_DURATION_MS);
    perf.stress = Some(StressState {
        end_at,
        edit_end: end_at,
        next_edit: now,
        edit_index: 0,
        glyph_text,
        font_px,
        line_height,
        cols,
        rows,
        glyphs,
    });
}

fn update_text_stress(perf: &mut PerfState, console: &mut ConsoleState, now: Instant) {
    let Some(stress) = perf.stress.as_mut() else {
        return;
    };
    if now >= stress.end_at {
        perf.stress = None;
        perf.hud_dirty = true;
        return;
    }
    if now >= stress.edit_end || now < stress.next_edit {
        return;
    }
    let ch = ((stress.edit_index % 26) as u8 + b'a') as char;
    console.buffer.push(ch);
    if console.buffer.len() > 48 {
        console.buffer.clear();
    }
    stress.edit_index = stress.edit_index.saturating_add(1);
    stress.next_edit = now + Duration::from_millis(STRESS_EDIT_INTERVAL_MS);
}

fn perf_summary_lines(perf: &PerfState) -> [String; 4] {
    perf_summary_lines_with(perf.budgets, perf.last, perf.stress.as_ref())
}

fn quantize_ms(value: f32) -> f32 {
    if PERF_HUD_EPS_MS <= 0.0 {
        return value;
    }
    (value / PERF_HUD_EPS_MS).round() * PERF_HUD_EPS_MS
}

fn perf_summary_lines_with(
    budgets: PerfBudgets,
    timings: PerfTimings,
    stress: Option<&StressState>,
) -> [String; 4] {
    [
        format!(
            "perf egui: {:>4.2} ms (<= {:.2})",
            timings.egui_build_ms, budgets.egui_ms
        ),
        format!(
            "perf glyphon prep: {:>4.2} ms (<= {:.2})",
            timings.glyphon_prepare_ms, budgets.glyphon_prepare_ms
        ),
        format!(
            "perf glyphon render: {:>4.2} ms (<= {:.2})",
            timings.glyphon_render_ms, budgets.glyphon_render_ms
        ),
        format!(
            "perf stress: {}",
            if let Some(stress) = stress {
                format!(
                    "on ({} glyphs, {}x{})",
                    stress.glyphs, stress.cols, stress.rows
                )
            } else {
                "off".to_string()
            }
        ),
    ]
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

fn parse_playlist_entries(
    contents: &str,
    base_dir: Option<&Path>,
) -> Result<VecDeque<PlaylistEntry>, ExitError> {
    let base = base_dir.unwrap_or_else(|| Path::new("."));
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

struct PalletCommandUser<'a> {
    perf: &'a mut PerfState,
    script: Option<&'a mut ScriptRuntime>,
    path_policy: &'a PathPolicy,
    asset_manager: &'a AssetManager,
    upload_queue: UploadQueue,
    quake_vfs: Option<Arc<Vfs>>,
    quake_dir: Option<PathBuf>,
    console_async: ConsoleAsyncSender,
    capture_requests: &'a mut VecDeque<CaptureRequest>,
    settings: &'a mut Settings,
    settings_flags: &'a mut SettingsChangeFlags,
    test_map_reload_requests: &'a mut VecDeque<AssetKey>,
    active_test_map: Option<AssetKey>,
    test_map_runtime: Option<&'a mut TestMapRuntime>,
    camera: Option<&'a mut CameraState>,
    input_trace_record: &'a mut Option<InputTraceRecorder>,
    input_trace_playback: &'a mut Option<InputTracePlayback>,
}

impl<'a> ExecPathResolver for PalletCommandUser<'a> {
    fn resolve_exec_path(&self, input: &str) -> Result<PathBuf, String> {
        let key = config_asset_key("scripts", input)?;
        let resolver = AssetResolver::new(self.path_policy, None);
        let location = resolver.resolve(&key)?;
        match location.path {
            ResolvedPath::File(path) => Ok(path),
            ResolvedPath::Vfs(_) | ResolvedPath::Bundle { .. } => Err(format!(
                "exec assets must resolve to files (got {})",
                key.canonical()
            )),
        }
    }

    fn resolve_exec_source(&self, input: &str) -> Result<ExecSource, String> {
        let key = config_asset_key("scripts", input)?;
        let handle = self.asset_manager.request::<ConfigAsset>(
            key.clone(),
            RequestOpts {
                priority: AssetPriority::High,
                budget_tag: AssetBudgetTag::Boot,
            },
        );
        let asset = self
            .asset_manager
            .await_ready(&handle, Duration::from_secs(2))
            .map_err(|err| format!("exec load failed ({}): {}", key.canonical(), err))?;
        Ok(ExecSource {
            label: key.canonical().to_string(),
            source: asset.text.clone(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_console_line(
    line: &str,
    console: &mut ConsoleState,
    perf: &mut PerfState,
    cvars: &mut CvarRegistry,
    core_cvars: &CoreCvars,
    script: Option<&mut ScriptRuntime>,
    path_policy: &PathPolicy,
    asset_manager: &AssetManager,
    upload_queue: UploadQueue,
    quake_vfs: Option<Arc<Vfs>>,
    quake_dir: Option<PathBuf>,
    console_async: ConsoleAsyncSender,
    log_filter_state: &Arc<Mutex<LogFilterState>>,
    capture_requests: &mut VecDeque<CaptureRequest>,
    settings: &mut Settings,
    settings_flags: &mut SettingsChangeFlags,
    test_map_reload_requests: &mut VecDeque<AssetKey>,
    active_test_map: Option<AssetKey>,
    test_map_runtime: Option<&mut TestMapRuntime>,
    camera: Option<&mut CameraState>,
    input_trace_record: &mut Option<InputTraceRecorder>,
    input_trace_playback: &mut Option<InputTracePlayback>,
) {
    let dispatch_result = match build_command_registry(core_cvars) {
        Ok(mut commands) => {
            let mut user = PalletCommandUser {
                perf,
                script,
                path_policy,
                asset_manager,
                upload_queue,
                quake_vfs,
                quake_dir,
                console_async,
                capture_requests,
                settings,
                settings_flags,
                test_map_reload_requests,
                active_test_map,
                test_map_runtime,
                camera,
                input_trace_record,
                input_trace_playback,
            };
            commands.dispatch_line(line, cvars, console, &mut user)
        }
        Err(err) => {
            let message = format!("error: {}", err);
            console.push_line(message.clone());
            observability::set_sticky_error(message);
            return;
        }
    };
    if let Err(err) = dispatch_result {
        let message = format!("error: {}", err);
        console.push_line(message.clone());
        observability::set_sticky_error(message);
    }
    apply_cvar_changes(cvars, perf, core_cvars, asset_manager, log_filter_state);
}

#[allow(clippy::too_many_arguments)]
fn dispatch_smoke_command(
    parsed: &ParsedCommand,
    console: &mut ConsoleState,
    perf: &mut PerfState,
    cvars: &mut CvarRegistry,
    core_cvars: &CoreCvars,
    script: Option<&mut ScriptRuntime>,
    path_policy: &PathPolicy,
    asset_manager: &AssetManager,
    upload_queue: UploadQueue,
    quake_vfs: Option<Arc<Vfs>>,
    quake_dir: Option<PathBuf>,
    console_async: ConsoleAsyncSender,
    log_filter_state: &Arc<Mutex<LogFilterState>>,
    capture_requests: &mut VecDeque<CaptureRequest>,
    settings: &mut Settings,
    settings_flags: &mut SettingsChangeFlags,
) -> Result<(), String> {
    let mut test_map_reload_requests = VecDeque::new();
    let mut input_trace_record = None;
    let mut input_trace_playback = None;
    let dispatch_result = match build_command_registry(core_cvars) {
        Ok(mut commands) => {
            let mut user = PalletCommandUser {
                perf,
                script,
                path_policy,
                asset_manager,
                upload_queue,
                quake_vfs,
                quake_dir,
                console_async,
                capture_requests,
                settings,
                settings_flags,
                test_map_reload_requests: &mut test_map_reload_requests,
                active_test_map: None,
                test_map_runtime: None,
                camera: None,
                input_trace_record: &mut input_trace_record,
                input_trace_playback: &mut input_trace_playback,
            };
            commands.dispatch(&parsed.name, &parsed.args, cvars, console, &mut user)
        }
        Err(err) => Err(err),
    };
    if let Err(err) = dispatch_result {
        let message = format!("error: {}", err);
        console.push_line(message.clone());
        observability::set_sticky_error(message);
        apply_cvar_changes(cvars, perf, core_cvars, asset_manager, log_filter_state);
        return Err(err);
    }
    apply_cvar_changes(cvars, perf, core_cvars, asset_manager, log_filter_state);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn drain_lua_command_queue(
    queue: &Rc<RefCell<VecDeque<String>>>,
    console: &mut ConsoleState,
    perf: &mut PerfState,
    cvars: &mut CvarRegistry,
    core_cvars: &CoreCvars,
    path_policy: &PathPolicy,
    asset_manager: &AssetManager,
    upload_queue: UploadQueue,
    quake_vfs: Option<Arc<Vfs>>,
    quake_dir: Option<PathBuf>,
    console_async: ConsoleAsyncSender,
    log_filter_state: &Arc<Mutex<LogFilterState>>,
    capture_requests: &mut VecDeque<CaptureRequest>,
    settings: &mut Settings,
    settings_flags: &mut SettingsChangeFlags,
    test_map_reload_requests: &mut VecDeque<AssetKey>,
    active_test_map: Option<AssetKey>,
    test_map_runtime: &mut Option<TestMapRuntime>,
    camera: &mut CameraState,
    input_trace_record: &mut Option<InputTraceRecorder>,
    input_trace_playback: &mut Option<InputTracePlayback>,
    script: &mut Option<ScriptRuntime>,
) {
    loop {
        let line = queue.borrow_mut().pop_front();
        let Some(line) = line else {
            break;
        };
        console.push_line(format!("> {}", line));
        dispatch_console_line(
            &line,
            console,
            perf,
            cvars,
            core_cvars,
            script.as_mut(),
            path_policy,
            asset_manager,
            upload_queue.clone(),
            quake_vfs.clone(),
            quake_dir.clone(),
            console_async.clone(),
            log_filter_state,
            capture_requests,
            settings,
            settings_flags,
            test_map_reload_requests,
            active_test_map.clone(),
            test_map_runtime.as_mut(),
            Some(camera),
            input_trace_record,
            input_trace_playback,
        );
    }
}

fn queue_capture_request(
    ctx: &mut engine_core::control_plane::CommandContext<'_, PalletCommandUser<'_>>,
    args: &CommandArgs,
    kind: CaptureKind,
    include_id: CvarId,
) -> Result<(), String> {
    if args.positionals().len() > 1 {
        return Err(format!("usage: {} [path]", kind.label()));
    }
    let path = args.positional(0).map(PathBuf::from);
    let include_overlays = cvar_bool(ctx.cvars, include_id).unwrap_or(true);
    ctx.user.capture_requests.push_back(CaptureRequest {
        kind,
        path: path.clone(),
        include_overlays,
    });
    if let Some(path) = path {
        ctx.output
            .push_line(format!("{}: scheduled {}", kind.label(), path.display()));
    } else {
        ctx.output.push_line(format!("{}: scheduled", kind.label()));
    }
    Ok(())
}

fn record_capture_failure(
    console: &mut ConsoleState,
    message: String,
    capture_failures: &mut u64,
    capture_last_error: &mut Option<String>,
) {
    console.push_line(message.clone());
    *capture_failures = capture_failures.saturating_add(1);
    *capture_last_error = Some(message);
}

fn register_cvar_alias_command<'a>(
    commands: &mut CommandRegistry<'a, PalletCommandUser<'a>>,
    alias: &'static str,
    cvar_name: &'static str,
    help: &'static str,
) -> Result<(), String> {
    let usage = format!("{alias} <value>");
    commands.register(
        CommandSpec::new(alias, help, usage),
        Box::new(move |ctx, args| {
            let value = args
                .positional(0)
                .ok_or_else(|| format!("usage: {alias} <value>"))?;
            let parsed = ctx.cvars.set_from_str(cvar_name, value)?;
            ctx.output
                .push_line(format!("{cvar_name} = {}", parsed.display()));
            Ok(())
        }),
    )?;
    Ok(())
}

struct PlayerTuneParam {
    name: &'static str,
    cvar_name: &'static str,
    help: &'static str,
}

const PLAYER_TUNE_PARAMS: &[PlayerTuneParam] = &[
    PlayerTuneParam {
        name: "air_max_speed",
        cvar_name: "arena_air_max_speed",
        help: "Arena air max speed.",
    },
    PlayerTuneParam {
        name: "air_accel",
        cvar_name: "arena_air_accel",
        help: "Arena air acceleration.",
    },
    PlayerTuneParam {
        name: "air_resist",
        cvar_name: "arena_air_resistance",
        help: "Arena air resistance.",
    },
    PlayerTuneParam {
        name: "air_resist_scale",
        cvar_name: "arena_air_resistance_speed_scale",
        help: "Arena air resistance speed scaling.",
    },
    PlayerTuneParam {
        name: "golden_target_deg",
        cvar_name: "arena_golden_target_deg",
        help: "Golden angle target (degrees).",
    },
    PlayerTuneParam {
        name: "golden_gain_min",
        cvar_name: "arena_golden_gain_min",
        help: "Golden angle min gain.",
    },
    PlayerTuneParam {
        name: "golden_gain_peak",
        cvar_name: "arena_golden_gain_peak",
        help: "Golden angle peak gain.",
    },
    PlayerTuneParam {
        name: "golden_bonus_scale",
        cvar_name: "arena_golden_bonus_scale",
        help: "Golden angle bonus scale.",
    },
    PlayerTuneParam {
        name: "golden_blend_start",
        cvar_name: "arena_golden_blend_start",
        help: "Golden angle blend start speed.",
    },
    PlayerTuneParam {
        name: "golden_blend_end",
        cvar_name: "arena_golden_blend_end",
        help: "Golden angle blend end speed.",
    },
    PlayerTuneParam {
        name: "cs_strength_deg",
        cvar_name: "arena_cs_strength_deg",
        help: "Corridor shaping strength (deg/sec).",
    },
    PlayerTuneParam {
        name: "cs_min_speed",
        cvar_name: "arena_cs_min_speed",
        help: "Corridor shaping minimum speed.",
    },
    PlayerTuneParam {
        name: "cs_max_angle_deg",
        cvar_name: "arena_cs_max_angle_deg",
        help: "Corridor shaping max angle per tick (degrees).",
    },
    PlayerTuneParam {
        name: "cs_min_align",
        cvar_name: "arena_cs_min_alignment",
        help: "Corridor shaping minimum alignment.",
    },
];

fn resolve_player_tune_param(name: &str) -> Option<&'static PlayerTuneParam> {
    PLAYER_TUNE_PARAMS
        .iter()
        .find(|param| param.name == name || param.cvar_name == name)
}

fn parse_motor_kind(input: &str) -> Result<MotorKind, String> {
    match input {
        "arena" => Ok(MotorKind::Arena),
        "rpg" => Ok(MotorKind::Rpg),
        "1" => Ok(MotorKind::Arena),
        "2" => Ok(MotorKind::Rpg),
        _ => Err("expected arena or rpg".to_string()),
    }
}

fn motor_kind_label(kind: MotorKind) -> &'static str {
    match kind {
        MotorKind::Arena => "arena",
        MotorKind::Rpg => "rpg",
    }
}

fn parse_radius_arg(args: &CommandArgs, default_radius: f32) -> Result<f32, String> {
    let Some(raw) = args.positional(0) else {
        return Ok(default_radius);
    };
    let value = raw.strip_prefix("radius=").unwrap_or(raw);
    let radius = value
        .parse::<f32>()
        .map_err(|_| format!("invalid radius: {}", raw))?;
    if radius <= 0.0 {
        return Err("radius must be > 0".to_string());
    }
    Ok(radius)
}

fn build_command_registry<'a>(
    core_cvars: &'a CoreCvars,
) -> Result<CommandRegistry<'a, PalletCommandUser<'a>>, String> {
    let mut commands: CommandRegistry<'a, PalletCommandUser<'a>> = CommandRegistry::new();
    register_core_commands(&mut commands)?;
    register_pallet_command_specs(&mut commands)?;
    register_cvar_alias_command(
        &mut commands,
        "arena_cs_strength_deg",
        "arena_cs_strength_deg",
        "Alias for cvar_set arena_cs_strength_deg.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "arena_cs_min_speed",
        "arena_cs_min_speed",
        "Alias for cvar_set arena_cs_min_speed.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "arena_cs_max_angle_deg",
        "arena_cs_max_angle_deg",
        "Alias for cvar_set arena_cs_max_angle_deg.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "arena_cs_min_alignment",
        "arena_cs_min_alignment",
        "Alias for cvar_set arena_cs_min_alignment.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "cs_strength",
        "arena_cs_strength_deg",
        "Alias for cvar_set arena_cs_strength_deg.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "cs_min_speed",
        "arena_cs_min_speed",
        "Alias for cvar_set arena_cs_min_speed.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "cs_max_angle",
        "arena_cs_max_angle_deg",
        "Alias for cvar_set arena_cs_max_angle_deg.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "cs_min_align",
        "arena_cs_min_alignment",
        "Alias for cvar_set arena_cs_min_alignment.",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "dev_motor",
        "dev_motor",
        "Alias for cvar_set dev_motor (1=arena, 2=rpg).",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "motor",
        "dev_motor",
        "Alias for cvar_set dev_motor (1=arena, 2=rpg).",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "dev_fixed_dt",
        "dev_fixed_dt",
        "Alias for cvar_set dev_fixed_dt (seconds).",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "fixed_dt",
        "dev_fixed_dt",
        "Alias for cvar_set dev_fixed_dt (seconds).",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "dev_substeps",
        "dev_substeps",
        "Alias for cvar_set dev_substeps (1..16).",
    )?;
    register_cvar_alias_command(
        &mut commands,
        "substeps",
        "dev_substeps",
        "Alias for cvar_set dev_substeps (1..16).",
    )?;

    let perf_hud_id = core_cvars.dbg_perf_hud;
    let asset_decode_budget_id = core_cvars.asset_decode_budget_ms;
    let asset_upload_budget_id = core_cvars.asset_upload_budget_ms;
    let asset_io_budget_id = core_cvars.asset_io_budget_kb;
    let capture_include_id = core_cvars.capture_include_overlays;
    commands.set_handler(
        "logfill",
        Box::new(|ctx, args| {
            let count = args
                .positional(0)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(5_000)
                .clamp(1, 20_000);
            ctx.output.set_max_lines(count);
            ctx.output.clear();
            for index in 0..count {
                ctx.output.push_line(format!(
                    "logfill {:>5}: The quick brown fox jumps over the lazy dog.",
                    index + 1
                ));
            }
            ctx.output.reset_scroll();
            Ok(())
        }),
    )?;
    commands.set_handler(
        "perf",
        Box::new(|ctx, _args| {
            for line in perf_summary_lines(ctx.user.perf) {
                ctx.output.push_line(line);
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "perf_hud",
        Box::new(move |ctx, args| {
            let current = cvar_bool(ctx.cvars, perf_hud_id).unwrap_or(false);
            let next = match parse_toggle_arg(args)? {
                Some(value) => value,
                None => !current,
            };
            ctx.cvars.set(
                perf_hud_id,
                engine_core::control_plane::CvarValue::Bool(next),
            )?;
            ctx.output.push_line(format!(
                "perf hud overlay: {}",
                if next { "on" } else { "off" }
            ));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "perf_stress",
        Box::new(|ctx, args| {
            let (enabled, changed) = match parse_toggle_arg(args)? {
                Some(value) => (value, ctx.user.perf.set_stress_enabled(value)),
                None => (ctx.user.perf.request_stress_toggle(), true),
            };
            if !changed {
                ctx.output.push_line(if enabled {
                    "stress: already running".to_string()
                } else {
                    "stress: already stopped".to_string()
                });
            } else if enabled {
                ctx.output.push_line("stress: starting (5s)".to_string());
            } else {
                ctx.output.push_line("stress: stopped".to_string());
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "stress_text",
        Box::new(|ctx, args| {
            let (enabled, changed) = match parse_toggle_arg(args)? {
                Some(value) => (value, ctx.user.perf.set_stress_enabled(value)),
                None => (ctx.user.perf.request_stress_toggle(), true),
            };
            if !changed {
                ctx.output.push_line(if enabled {
                    "stress: already running".to_string()
                } else {
                    "stress: already stopped".to_string()
                });
            } else if enabled {
                ctx.output.push_line("stress: starting (5s)".to_string());
            } else {
                ctx.output.push_line("stress: stopped".to_string());
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "sticky_error",
        Box::new(|ctx, _args| {
            match observability::sticky_error() {
                Some(message) => ctx.output.push_line(format!("sticky error: {}", message)),
                None => ctx.output.push_line("sticky error: <none>".to_string()),
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_asset_resolve",
        Box::new(|ctx, args| {
            let key = parse_asset_key_arg(args, "dev_asset_resolve <asset_id>")?;
            let path_policy = ctx.user.path_policy.clone();
            let vfs = ctx.user.quake_vfs.clone();
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            ctx.output
                .push_line(format!("dev_asset_resolve: scheduled ({})", key));
            schedule_console_job(jobs, sender, "dev_asset_resolve", JobQueue::Io, move || {
                let resolver = AssetResolver::new(&path_policy, vfs.as_deref());
                let location = resolver.resolve(&key)?;
                Ok(format_resolved_location(&location))
            })
        }),
    )?;
    commands.set_handler(
        "dev_asset_explain",
        Box::new(|ctx, args| {
            let key = parse_asset_key_arg(args, "dev_asset_explain <asset_id>")?;
            let path_policy = ctx.user.path_policy.clone();
            let vfs = ctx.user.quake_vfs.clone();
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            ctx.output
                .push_line(format!("dev_asset_explain: scheduled ({})", key));
            schedule_console_job(jobs, sender, "dev_asset_explain", JobQueue::Io, move || {
                let resolver = AssetResolver::new(&path_policy, vfs.as_deref());
                let report = resolver.explain(&key)?;
                Ok(format_resolve_report(&report))
            })
        }),
    )?;
    commands.set_handler(
        "dev_asset_stats",
        Box::new(move |ctx, _args| {
            let entries = ctx.user.asset_manager.list_assets();
            let mut status_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
            let mut kind_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
            let mut decoded_bytes = 0usize;
            for entry in &entries {
                let status = entry.metrics.status.as_str();
                *status_counts.entry(status).or_insert(0) += 1;
                let kind = entry.kind.as_str();
                *kind_counts.entry(kind).or_insert(0) += 1;
                decoded_bytes = decoded_bytes.saturating_add(entry.metrics.decoded_bytes);
            }
            let total = entries.len();
            let ready = status_counts.get("ready").copied().unwrap_or(0);
            let loading = status_counts.get("loading").copied().unwrap_or(0);
            let queued = status_counts.get("queued").copied().unwrap_or(0);
            let failed = status_counts.get("failed").copied().unwrap_or(0);
            let decode_budget = cvar_int(ctx.cvars, asset_decode_budget_id).unwrap_or(0);
            let upload_budget = cvar_int(ctx.cvars, asset_upload_budget_id).unwrap_or(0);
            let io_budget = cvar_int(ctx.cvars, asset_io_budget_id).unwrap_or(0);
            let telemetry = ctx.user.asset_manager.budget_telemetry();
            let upload_metrics = ctx.user.upload_queue.metrics();
            let last_drain_ms = upload_metrics
                .last_drain_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".to_string());
            ctx.output.push_line("asset stats:".to_string());
            ctx.output.push_line(format!(
                "budget: decode_ms={} upload_ms={} io_kb={}",
                decode_budget, upload_budget, io_budget
            ));
            ctx.output.push_line(format!(
                "spent ms: boot={} streaming={} background={}",
                telemetry.spent_boot_ms, telemetry.spent_streaming_ms, telemetry.spent_background_ms
            ));
            ctx.output.push_line(format!(
                "throttled: boot={} streaming={} background={}",
                telemetry.throttled_boot,
                telemetry.throttled_streaming,
                telemetry.throttled_background
            ));
            ctx.output.push_line(format!(
                "entries: total={} ready={} loading={} queued={} failed={}",
                total, ready, loading, queued, failed
            ));
            ctx.output
                .push_line(format!("decoded bytes: {}", decoded_bytes));
            ctx.output
                .push_line("by kind:".to_string());
            for (kind, count) in kind_counts {
                ctx.output.push_line(format!("  {} = {}", kind, count));
            }
            ctx.output.push_line(format!(
                "upload queue: queued_jobs={} queued_bytes={} last_drain_ms={} last_drain_jobs={} last_drain_bytes={}",
                upload_metrics.queued_jobs,
                upload_metrics.queued_bytes,
                last_drain_ms,
                upload_metrics.last_drain_jobs,
                upload_metrics.last_drain_bytes
            ));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_asset_status",
        Box::new(|ctx, args| {
            let key = parse_asset_key_arg(args, "dev_asset_status <asset_id>")?;
            match ctx.user.asset_manager.asset_snapshot(&key) {
                Some(entry) => {
                    for line in format_asset_status(&entry) {
                        ctx.output.push_line(line);
                    }
                }
                None => ctx
                    .output
                    .push_line(format!("asset not cached: {}", key.canonical())),
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_asset_list",
        Box::new(|ctx, args| {
            let options = parse_asset_list_args(args)?;
            let entries = ctx.user.asset_manager.list_assets();
            let mut filtered: Vec<_> = entries
                .into_iter()
                .filter(|entry| match_asset_list_entry(entry, &options))
                .collect();
            filtered.sort_by(|a, b| a.key.canonical().cmp(b.key.canonical()));
            let total = filtered.len();
            let limit = options.limit.min(ASSET_LIST_MAX_LIMIT);
            let shown = total.min(limit);
            ctx.output
                .push_line(format!("assets: total={} showing={}", total, shown));
            for entry in filtered.into_iter().take(limit) {
                ctx.output.push_line(format_asset_list_line(&entry));
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "quake_which",
        Box::new(|ctx, args| {
            let path = parse_string_arg(args, "quake_which <path>")?;
            let path_policy = ctx.user.path_policy.clone();
            let quake_dir = ctx.user.quake_dir.clone();
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            ctx.output
                .push_line(format!("quake_which: scheduled ({})", path));
            schedule_console_job(jobs, sender, "quake_which", JobQueue::Io, move || {
                let index = load_quake_index_for_console(&path_policy, quake_dir.as_deref())?;
                let report = index
                    .which(&path)
                    .ok_or_else(|| format!("quake path not found: {}", path))?;
                let mut lines = Vec::new();
                lines.push(format!("path: {}", report.path));
                if let Some(derived) = report.winner.derived_asset_key() {
                    lines.push(format!("derived_id: {}", derived));
                }
                lines.push(format!("winner: {}", format_quake_entry(&report.winner)));
                lines.push("candidates:".to_string());
                for entry in report.candidates {
                    lines.push(format!("- {}", format_quake_entry(&entry)));
                }
                Ok(lines)
            })
        }),
    )?;
    commands.set_handler(
        "quake_dupes",
        Box::new(|ctx, args| {
            let limit = parse_limit_flag(args, 20)?;
            let path_policy = ctx.user.path_policy.clone();
            let quake_dir = ctx.user.quake_dir.clone();
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            ctx.output.push_line("quake_dupes: scheduled".to_string());
            schedule_console_job(jobs, sender, "quake_dupes", JobQueue::Io, move || {
                let index = load_quake_index_for_console(&path_policy, quake_dir.as_deref())?;
                let dupes = index.duplicates();
                if dupes.is_empty() {
                    return Ok(vec!["no duplicates found".to_string()]);
                }
                let mut lines = Vec::new();
                lines.push(format!("duplicates: {}", dupes.len()));
                for dupe in dupes.into_iter().take(limit) {
                    lines.push(format!("path: {}", dupe.path));
                    lines.push(format!("winner: {}", format_quake_entry(&dupe.winner)));
                    for entry in dupe.others {
                        lines.push(format!("- {}", format_quake_entry(&entry)));
                    }
                }
                Ok(lines)
            })
        }),
    )?;
    commands.set_handler(
        "dev_asset_reload",
        Box::new(|ctx, args| {
            let key = parse_asset_key_arg(args, "dev_asset_reload <asset_id>")?;
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            let asset_manager = ctx.user.asset_manager.clone();
            let path_policy = ctx.user.path_policy.clone();
            let quake_vfs = ctx.user.quake_vfs.clone();
            ctx.output
                .push_line(format!("dev_asset_reload: scheduled ({})", key));
            schedule_console_job(jobs, sender, "dev_asset_reload", JobQueue::Io, move || {
                let mut lines = Vec::new();
                let before_version = asset_manager
                    .asset_snapshot(&key)
                    .map(|snapshot| snapshot.metrics.version)
                    .unwrap_or(0);
                let resolver = AssetResolver::new(&path_policy, quake_vfs.as_deref());
                match resolver.resolve(&key) {
                    Ok(location) => lines.extend(format_resolved_location(&location)),
                    Err(err) => lines.push(format!("resolve error: {}", err)),
                }
                request_asset_reload(&asset_manager, key.clone())?;
                lines.push(format!("reload requested: {}", key.canonical()));
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if let Some(snapshot) = asset_manager.asset_snapshot(&key) {
                        let metrics = snapshot.metrics;
                        if metrics.version > before_version {
                            let mut line =
                                format!("reloaded: {} v={}", key.canonical(), metrics.version);
                            if let Some(hash) = metrics.content_hash {
                                line.push_str(&format!(" hash={:016x}", hash));
                            }
                            lines.push(line);
                            break;
                        }
                        if metrics.status == AssetStatus::Failed {
                            lines.push(format!(
                                "reload failed: {}",
                                metrics.error.unwrap_or_else(|| "unknown".to_string())
                            ));
                            break;
                        }
                        if metrics.version == before_version
                            && metrics.status == AssetStatus::Ready
                            && metrics.error.is_some()
                        {
                            lines.push(format!(
                                "reload failed: {}",
                                metrics.error.unwrap_or_else(|| "unknown".to_string())
                            ));
                            break;
                        }
                    }
                    if Instant::now() >= deadline {
                        lines.push("reload pending".to_string());
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(lines)
            })
        }),
    )?;
    commands.set_handler(
        "dev_test_map_reload",
        Box::new(|ctx, args| {
            if args.positionals().len() > 1 {
                return Err("usage: dev_test_map_reload [engine:test_map/...]".to_string());
            }
            let key = match args.positional(0) {
                Some(value) => parse_test_map_key_arg(value)?,
                None => ctx
                    .user
                    .active_test_map
                    .clone()
                    .ok_or_else(|| "no active test map loaded".to_string())?,
            };
            ctx.user.test_map_reload_requests.push_back(key.clone());
            ctx.output
                .push_line(format!("test map reload queued: {}", key.canonical()));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_asset_purge",
        Box::new(|ctx, args| {
            let key = parse_asset_key_arg(args, "dev_asset_purge <asset_id>")?;
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            let asset_manager = ctx.user.asset_manager.clone();
            ctx.output
                .push_line(format!("dev_asset_purge: scheduled ({})", key));
            schedule_console_job(jobs, sender, "dev_asset_purge", JobQueue::Io, move || {
                let line = if asset_manager.purge(&key) {
                    format!("purged: {}", key.canonical())
                } else {
                    format!("asset not cached: {}", key.canonical())
                };
                Ok(vec![line])
            })
        }),
    )?;
    commands.set_handler(
        "dev_content_validate",
        Box::new(|ctx, _args| {
            let path_policy = ctx.user.path_policy.clone();
            let quake_dir = ctx.user.quake_dir.clone();
            let sender = ctx.user.console_async.clone();
            let jobs = ctx.user.asset_manager.jobs();
            ctx.output
                .push_line("dev_content_validate: scheduled".to_string());
            schedule_console_job(
                jobs,
                sender,
                "dev_content_validate",
                JobQueue::Io,
                move || run_content_validate(path_policy, quake_dir),
            )
        }),
    )?;
    commands.set_handler(
        "dev_collision_draw",
        Box::new(|ctx, args| {
            let current = match ctx.cvars.get_by_name("dev_collision_draw") {
                Some(entry) => match entry.value {
                    CvarValue::Bool(value) => value,
                    _ => false,
                },
                None => return Err("dev_collision_draw cvar missing".to_string()),
            };
            let next = match parse_toggle_arg(args)? {
                Some(value) => value,
                None => !current,
            };
            let value = if next { "1" } else { "0" };
            ctx.cvars.set_from_str("dev_collision_draw", value)?;
            ctx.output.push_line(format!(
                "collision draw: {}",
                if next { "on" } else { "off" }
            ));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_collision_dump_near_player",
        Box::new(|ctx, args| {
            let radius = parse_radius_arg(args, COLLISION_INTEREST_RADIUS)?;
            let runtime = ctx
                .user
                .test_map_runtime
                .as_ref()
                .ok_or_else(|| "no test map runtime loaded".to_string())?;
            let interest = collision_interest_bounds(runtime.position.translation.vector, radius);
            let selected = select_collision_chunks(
                &runtime.collision_world.world,
                CollisionChunkSelection::Bounds(interest),
            );
            ctx.output.push_line(format!(
                "collision near: radius={:.2} chunks={}",
                radius,
                selected.len()
            ));
            for index in selected {
                let chunk = match runtime
                    .collision_world
                    .world
                    .chunks
                    .get(index as usize)
                {
                    Some(chunk) => chunk,
                    None => continue,
                };
                let loaded = runtime.collision_world.loaded_chunks.contains(&index);
                ctx.output.push_line(format!(
                    "- {} loaded={} tris={} bounds=({:.2},{:.2},{:.2})..({:.2},{:.2},{:.2}) payload={}",
                    chunk.chunk_id,
                    if loaded { "yes" } else { "no" },
                    chunk.triangle_count,
                    chunk.aabb_min[0],
                    chunk.aabb_min[1],
                    chunk.aabb_min[2],
                    chunk.aabb_max[0],
                    chunk.aabb_max[1],
                    chunk.aabb_max[2],
                    chunk.payload_ref
                ));
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "player_set_profile",
        Box::new(|ctx, args| {
            let value = args
                .positional(0)
                .ok_or_else(|| "usage: player_set_profile <arena|rpg>".to_string())?;
            let kind = parse_motor_kind(value)?;
            let cvar_value = if matches!(kind, MotorKind::Arena) {
                "1"
            } else {
                "2"
            };
            ctx.cvars.set_from_str("dev_motor", cvar_value)?;
            if let (Some(runtime), Some(camera)) =
                (ctx.user.test_map_runtime.as_mut(), ctx.user.camera.as_mut())
            {
                switch_test_map_motor(runtime, camera, kind);
            }
            ctx.output
                .push_line(format!("player profile: {}", motor_kind_label(kind)));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "player_dump_state",
        Box::new(|ctx, _args| {
            let runtime = ctx
                .user
                .test_map_runtime
                .as_ref()
                .ok_or_else(|| "no test map runtime loaded".to_string())?;
            let pos = runtime.position.translation;
            let vel = runtime.velocity;
            let speed = (vel.x * vel.x + vel.y * vel.y + vel.z * vel.z).sqrt();
            ctx.output.push_line(format!(
                "player: motor={} grounded={}",
                motor_kind_label(runtime.controller.motor().kind()),
                runtime.grounded
            ));
            ctx.output
                .push_line(format!("pos: {:.3} {:.3} {:.3}", pos.x, pos.y, pos.z));
            ctx.output.push_line(format!(
                "vel: {:.3} {:.3} {:.3} speed={:.3}",
                vel.x, vel.y, vel.z, speed
            ));
            if let Some(normal) = runtime.ground_normal {
                ctx.output.push_line(format!(
                    "ground_normal: {:.3} {:.3} {:.3}",
                    normal.x, normal.y, normal.z
                ));
            }
            ctx.output.push_line(format!(
                "capsule_offset: {:.3} kcc_ms={:.3}",
                runtime.capsule_offset, runtime.kcc_query_ms
            ));
            if let Some(camera) = ctx.user.camera.as_ref() {
                ctx.output.push_line(format!(
                    "camera: yaw={:.2} pitch={:.2}",
                    camera.yaw.to_degrees(),
                    camera.pitch.to_degrees()
                ));
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "player_tune_set",
        Box::new(|ctx, args| {
            let param = args
                .positional(0)
                .ok_or_else(|| "usage: player_tune_set <param> <value>".to_string())?;
            let value = args
                .positional(1)
                .ok_or_else(|| "usage: player_tune_set <param> <value>".to_string())?;
            let cvar_name = resolve_player_tune_param(param)
                .map(|param| param.cvar_name)
                .unwrap_or(param);
            let parsed = ctx
                .cvars
                .set_from_str(cvar_name, value)
                .map_err(|err| format!("{err} (use player_tune_list)"))?;
            ctx.output
                .push_line(format!("{cvar_name} = {}", parsed.display()));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "player_tune_list",
        Box::new(|ctx, _args| {
            ctx.output.push_line("player tune params:".to_string());
            for param in PLAYER_TUNE_PARAMS {
                if let Some(entry) = ctx.cvars.get_by_name(param.cvar_name) {
                    ctx.output.push_line(format!(
                        "{} = {} ({}) - {}",
                        param.name,
                        entry.value.display(),
                        param.cvar_name,
                        param.help
                    ));
                }
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_input_record",
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: dev_input_record <name>".to_string())?;
            if ctx.user.input_trace_playback.is_some() {
                return Err("input replay active".to_string());
            }
            if ctx.user.input_trace_record.is_some() {
                return Err("input record already active".to_string());
            }
            *ctx.user.input_trace_record = Some(InputTraceRecorder::new(name.to_string()));
            ctx.output
                .push_line(format!("input record: started {}", name));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_input_record_stop",
        Box::new(|ctx, _args| {
            let recorder = ctx
                .user
                .input_trace_record
                .take()
                .ok_or_else(|| "input record not active".to_string())?;
            let path = write_input_trace(recorder)?;
            ctx.output
                .push_line(format!("input record: wrote {}", path.display()));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_input_replay",
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: dev_input_replay <name>".to_string())?;
            if ctx.user.input_trace_record.is_some() {
                return Err("input record active".to_string());
            }
            let playback = load_input_trace(name)?;
            *ctx.user.input_trace_playback = Some(playback);
            ctx.output
                .push_line(format!("input replay: started {}", name));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "dev_input_replay_stop",
        Box::new(|ctx, _args| {
            if ctx.user.input_trace_playback.take().is_some() {
                ctx.output.push_line("input replay: stopped".to_string());
            } else {
                ctx.output.push_line("input replay: not active".to_string());
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "capture_screenshot",
        Box::new(move |ctx, args| {
            queue_capture_request(ctx, args, CaptureKind::Screenshot, capture_include_id)
        }),
    )?;
    commands.set_handler(
        "capture_frame",
        Box::new(move |ctx, args| {
            queue_capture_request(ctx, args, CaptureKind::Frame, capture_include_id)
        }),
    )?;
    commands.set_handler(
        "settings_list",
        Box::new(|ctx, _args| {
            let fields = [
                "ui_scale",
                "vsync",
                "master_volume",
                "window_mode",
                "resolution",
                "active_profile",
            ];
            for field in fields {
                if let Some(value) = settings_field_value(ctx.user.settings, field) {
                    ctx.output.push_line(format!("{field} = {value}"));
                }
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "settings_get",
        Box::new(|ctx, args| {
            let field = args
                .positional(0)
                .ok_or_else(|| "usage: settings_get <field>".to_string())?;
            if let Some(value) = settings_field_value(ctx.user.settings, field) {
                ctx.output.push_line(format!("{field} = {value}"));
                Ok(())
            } else {
                Err(format!("unknown settings field: {field}"))
            }
        }),
    )?;
    commands.set_handler(
        "settings_set",
        Box::new(|ctx, args| {
            let field = args
                .positional(0)
                .ok_or_else(|| "usage: settings_set <field> <value>".to_string())?;
            let value = args
                .positional(1)
                .ok_or_else(|| "usage: settings_set <field> <value>".to_string())?;
            let change =
                apply_settings_field(ctx.user.settings, field, value, ctx.user.settings_flags)?;
            if let Some((old, new)) = change {
                ctx.output.push_line(format!("{field}: {old} -> {new}"));
            } else {
                ctx.output.push_line(format!("{field}: unchanged"));
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "settings_reset",
        Box::new(|ctx, _args| {
            let active_profile = ctx.user.settings.active_profile.clone();
            let previous = ctx.user.settings.clone();
            let mut next = Settings {
                active_profile,
                ..Settings::default()
            };
            next.normalize();
            let display_changed =
                previous.window_mode != next.window_mode || previous.resolution != next.resolution;
            let settings_changed = previous.ui_scale != next.ui_scale
                || previous.vsync != next.vsync
                || previous.master_volume != next.master_volume
                || previous.window_mode != next.window_mode
                || previous.resolution != next.resolution
                || previous.active_profile != next.active_profile;
            *ctx.user.settings = next;
            if settings_changed {
                ctx.user.settings_flags.mark_settings(display_changed);
                ctx.output.push_line("settings_reset: ok".to_string());
            } else {
                ctx.output
                    .push_line("settings_reset: no changes".to_string());
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "cfg_list",
        Box::new(|ctx, _args| {
            let mut profiles = list_config_profiles(ctx.user.path_policy);
            let active = ctx.user.settings.active_profile.clone();
            if !profiles.contains(&active) {
                profiles.push(active.clone());
            }
            profiles.sort();
            ctx.output
                .push_line(format!("cfg profiles (active: {active})"));
            for profile in profiles {
                if profile == active {
                    ctx.output.push_line(format!("* {profile}"));
                } else {
                    ctx.output.push_line(format!("- {profile}"));
                }
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "cfg_select",
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: cfg_select <name>".to_string())?;
            let profile = normalize_profile_name(name)?;
            if ctx.user.settings.active_profile != profile {
                let old = ctx.user.settings.active_profile.clone();
                ctx.user.settings.active_profile = profile.clone();
                ctx.user.settings_flags.mark_settings(false);
                ctx.output
                    .push_line(format!("active_profile: {old} -> {profile}"));
            } else {
                ctx.output
                    .push_line("active_profile: unchanged".to_string());
            }
            Ok(())
        }),
    )?;
    commands.set_handler(
        "cfg_save",
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: cfg_save <name>".to_string())?;
            let profile = normalize_profile_name(name)?;
            let path = config_path_for_profile(&profile);
            let mut settings_snapshot = ctx.user.settings.clone();
            settings_snapshot.active_profile = profile.clone();
            let mut lines = settings_lines(&settings_snapshot);
            let mut entries: Vec<_> = ctx
                .cvars
                .list()
                .into_iter()
                .filter(|entry| is_persistable_cvar(entry))
                .collect();
            entries.sort_by(|a, b| a.def.name.cmp(&b.def.name));
            for entry in entries {
                lines.push(format!("{}={}", entry.def.name, entry.value.display()));
            }
            write_config_lines(&path, &lines).map_err(|err| format!("cfg_save failed: {}", err))?;
            ctx.output
                .push_line(format!("cfg_save: {}", path.display()));
            Ok(())
        }),
    )?;
    commands.set_handler(
        "cfg_load",
        Box::new(|ctx, args| {
            let name = args
                .positional(0)
                .ok_or_else(|| "usage: cfg_load <name>".to_string())?;
            let profile = normalize_profile_name(name)?;
            let path = resolve_profile_path(ctx.user.path_policy, &profile)?;
            let contents = std::fs::read_to_string(&path)
                .map_err(|err| format!("cfg_load failed: {}", err))?;
            let mut staged_settings = ctx.user.settings.clone();
            let mut staged_cvars = ctx.cvars.clone();
            let mut staged_flags = SettingsChangeFlags::default();
            let mut settings_changes = Vec::new();
            let mut cvar_changes = Vec::new();
            let mut command_lines = Vec::new();
            let mut warnings = Vec::new();
            for (index, line) in contents.lines().enumerate() {
                let line_no = index + 1;
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                    continue;
                }
                if let Some((key, value)) = parse_config_kv_line(trimmed) {
                    if key == "active_profile" || key == "version" {
                        continue;
                    }
                    if is_settings_field(&key) {
                        match apply_settings_field(
                            &mut staged_settings,
                            &key,
                            &value,
                            &mut staged_flags,
                        ) {
                            Ok(Some(change)) => {
                                settings_changes
                                    .push(format!("settings {key}: {} -> {}", change.0, change.1));
                            }
                            Ok(None) => {}
                            Err(err) => warnings.push(format!(
                                "cfg_load warning: {} line {}: {}",
                                path.display(),
                                line_no,
                                err
                            )),
                        }
                        continue;
                    }
                    if let Some(entry) = staged_cvars.get_by_name(&key) {
                        let before = entry.value.display();
                        match staged_cvars.set_from_str(&key, &value) {
                            Ok(parsed) => {
                                let after = parsed.display();
                                if before != after {
                                    cvar_changes.push(format!("cvar {key}: {before} -> {after}"));
                                }
                            }
                            Err(err) => warnings.push(format!(
                                "cfg_load warning: {} line {}: {}",
                                path.display(),
                                line_no,
                                err
                            )),
                        }
                        continue;
                    }
                    warnings.push(format!(
                        "cfg_load warning: {} line {}: unknown key {}",
                        path.display(),
                        line_no,
                        key
                    ));
                    continue;
                }
                match parse_command_line(trimmed) {
                    Ok(Some(parsed)) => command_lines.push((line_no, parsed)),
                    Ok(None) => continue,
                    Err(err) => warnings.push(format!(
                        "cfg_load warning: {} line {}: {}",
                        path.display(),
                        line_no,
                        err
                    )),
                }
            }
            if staged_settings.active_profile != profile {
                let old = staged_settings.active_profile.clone();
                staged_settings.active_profile = profile.clone();
                staged_flags.mark_settings(false);
                settings_changes.push(format!("active_profile: {old} -> {profile}"));
            }

            *ctx.user.settings = staged_settings;
            *ctx.cvars = staged_cvars;

            let mut registry = build_command_registry(core_cvars)?;
            for (line_no, parsed) in command_lines {
                if let Err(err) =
                    registry.dispatch(&parsed.name, &parsed.args, ctx.cvars, ctx.output, ctx.user)
                {
                    warnings.push(format!(
                        "cfg_load warning: {} line {}: {}",
                        path.display(),
                        line_no,
                        err
                    ));
                }
            }

            for line in settings_changes {
                ctx.output.push_line(line);
            }
            for line in cvar_changes {
                ctx.output.push_line(line);
            }
            for warning in warnings {
                ctx.output.push_line(warning);
            }
            if staged_flags.settings_changed {
                ctx.user
                    .settings_flags
                    .mark_settings(staged_flags.display_changed);
            }
            ctx.output
                .push_line(format!("cfg_load: {}", path.display()));
            Ok(())
        }),
    )?;
    commands.set_fallback(Box::new(|ctx, name, args| {
        if let Some(script) = ctx.user.script.as_deref_mut() {
            match script.engine.run_command(name, args.raw_tokens()) {
                Ok(true) => Ok(()),
                Ok(false) => Err(format!("unknown command: {}", name)),
                Err(err) => Err(format!("lua command failed: {}", err)),
            }
        } else {
            Err(format!("unknown command: {}", name))
        }
    }));
    Ok(commands)
}

fn schedule_console_job<F>(
    jobs: Arc<Jobs>,
    sender: ConsoleAsyncSender,
    label: &str,
    queue: JobQueue,
    job: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<Vec<String>, String> + Send + 'static,
{
    let label = label.to_string();
    let error_label = label.clone();
    jobs.submit(queue, job, move |result| match result {
        Ok(lines) => sender.send_lines(lines),
        Err(err) => sender.send_line(format!("{} error: {}", label, err)),
    })
    .map_err(|err| format!("{} queue error: {}", error_label, err))?;
    Ok(())
}

fn parse_asset_key_arg(args: &CommandArgs, usage: &str) -> Result<AssetKey, String> {
    let value = args
        .positional(0)
        .ok_or_else(|| format!("usage: {}", usage))?;
    AssetKey::parse(value).map_err(|err| format!("invalid asset id: {}", err))
}

fn parse_string_arg(args: &CommandArgs, usage: &str) -> Result<String, String> {
    let value = args
        .positional(0)
        .ok_or_else(|| format!("usage: {}", usage))?;
    Ok(value.to_string())
}

fn parse_limit_flag(args: &CommandArgs, default_limit: usize) -> Result<usize, String> {
    let mut limit: Option<usize> = None;
    let mut iter = args.raw_tokens().iter().peekable();
    while let Some(token) = iter.next() {
        if let Some(flag) = token.strip_prefix("--") {
            match flag {
                "limit" => {
                    if limit.is_some() {
                        return Err("duplicate --limit".to_string());
                    }
                    let value = iter
                        .next()
                        .ok_or_else(|| "missing value for --limit".to_string())?;
                    limit = Some(parse_limit_value(value)?);
                }
                _ => return Err(format!("unknown flag: --{}", flag)),
            }
        } else if limit.is_none() {
            limit = Some(parse_limit_value(token)?);
        } else {
            return Err(format!("unexpected arg: {}", token));
        }
    }
    let limit = limit.unwrap_or(default_limit);
    Ok(limit.clamp(1, ASSET_LIST_MAX_LIMIT))
}

fn parse_limit_value(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid limit: {}", value))
}

struct AssetListOptions {
    ns: Option<String>,
    kind: Option<String>,
    limit: usize,
}

fn parse_asset_list_args(args: &CommandArgs) -> Result<AssetListOptions, String> {
    let mut ns = None;
    let mut kind = None;
    let mut limit: Option<usize> = None;
    let mut iter = args.raw_tokens().iter().peekable();
    while let Some(token) = iter.next() {
        let Some(flag) = token.strip_prefix("--") else {
            return Err(format!("unexpected arg: {}", token));
        };
        match flag {
            "ns" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "missing value for --ns".to_string())?;
                ns = Some(value.to_ascii_lowercase());
            }
            "kind" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "missing value for --kind".to_string())?;
                kind = Some(value.to_ascii_lowercase());
            }
            "limit" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "missing value for --limit".to_string())?;
                limit = Some(parse_limit_value(value)?);
            }
            _ => return Err(format!("unknown flag: --{}", flag)),
        }
    }
    let limit = match limit {
        Some(0) => ASSET_LIST_MAX_LIMIT,
        Some(value) => value,
        None => ASSET_LIST_DEFAULT_LIMIT,
    };
    Ok(AssetListOptions {
        ns,
        kind,
        limit: limit.clamp(1, ASSET_LIST_MAX_LIMIT),
    })
}

fn match_asset_list_entry(entry: &AssetEntrySnapshot, options: &AssetListOptions) -> bool {
    if let Some(ns) = options.ns.as_deref() {
        if entry.key.namespace() != ns {
            return false;
        }
    }
    if let Some(kind) = options.kind.as_deref() {
        if !match_asset_kind(entry, kind) {
            return false;
        }
    }
    true
}

fn match_asset_kind(entry: &AssetEntrySnapshot, kind: &str) -> bool {
    if kind.contains(':') {
        entry.kind.as_str() == kind
    } else {
        entry.key.kind() == kind
    }
}

fn format_asset_status(entry: &AssetEntrySnapshot) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("asset: {}", entry.key.canonical()));
    lines.push(format!("kind: {}", entry.kind.as_str()));
    lines.push(format!("status: {}", entry.metrics.status.as_str()));
    lines.push(format!("version: {}", entry.metrics.version));
    lines.push(format!("decoded_bytes: {}", entry.metrics.decoded_bytes));
    if let Some(ms) = entry.metrics.decode_ms {
        lines.push(format!("decode_ms: {}", ms));
    }
    if let Some(ms) = entry.metrics.load_ms {
        lines.push(format!("load_ms: {}", ms));
    }
    if let Some(hash) = entry.metrics.content_hash {
        lines.push(format!("hash: {:016x}", hash));
    }
    if let Some(err) = &entry.metrics.error {
        lines.push(format!("error: {}", err));
    }
    lines
}

fn format_asset_list_line(entry: &AssetEntrySnapshot) -> String {
    let mut line = format!(
        "- {} kind={} status={}",
        entry.key.canonical(),
        entry.kind.as_str(),
        entry.metrics.status.as_str()
    );
    line.push_str(&format!(" v={}", entry.metrics.version));
    if entry.metrics.decoded_bytes > 0 {
        line.push_str(&format!(" bytes={}", entry.metrics.decoded_bytes));
    }
    if let Some(ms) = entry.metrics.decode_ms {
        line.push_str(&format!(" decode_ms={}", ms));
    }
    if let Some(ms) = entry.metrics.load_ms {
        line.push_str(&format!(" load_ms={}", ms));
    }
    if let Some(hash) = entry.metrics.content_hash {
        line.push_str(&format!(" hash={:016x}", hash));
    }
    line
}

fn format_resolved_location(location: &ResolvedLocation) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("resolved: {}", location.key.canonical()));
    lines.push(format!(
        "mount: {} (order={})",
        location.mount_name, location.mount_order
    ));
    lines.push(format!("layer: {}", layer_label(location.layer)));
    match &location.path {
        ResolvedPath::File(path) => {
            lines.push(format!("path: {}", path.display()));
        }
        ResolvedPath::Vfs(path) => {
            lines.push(format!("vpath: {}", path));
        }
        ResolvedPath::Bundle {
            bundle_id,
            entry_id,
            offset,
        } => {
            lines.push(format!("bundle: {}", bundle_id));
            lines.push(format!("entry: {}", entry_id));
            if let Some(offset) = offset {
                lines.push(format!("offset: {}", offset));
            }
        }
    }
    lines.push(format!("source: {}", format_source_line(&location.source)));
    lines
}

fn format_resolve_report(report: &ResolveReport) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("explain: {}", report.key.canonical()));
    for candidate in &report.candidates {
        let layer = layer_label(candidate.layer);
        let hit = if candidate.exists { " [hit]" } else { "" };
        let mut line = format!(
            "- order={} layer={} mount={}",
            candidate.mount_order, layer, candidate.mount_name
        );
        match &candidate.path {
            ResolvedPath::File(path) => {
                line.push_str(&format!(" path={}{}", path.display(), hit));
            }
            ResolvedPath::Vfs(path) => {
                line.push_str(&format!(" vpath={}{}", path, hit));
            }
            ResolvedPath::Bundle {
                bundle_id,
                entry_id,
                offset,
            } => {
                if let Some(offset) = offset {
                    line.push_str(&format!(
                        " bundle={} entry={} offset={}{}",
                        bundle_id, entry_id, offset, hit
                    ));
                } else {
                    line.push_str(&format!(" bundle={} entry={}{}", bundle_id, entry_id, hit));
                }
            }
        }
        lines.push(line);
    }
    if let Some(location) = &report.winner {
        lines.push("winner:".to_string());
        lines.extend(format_resolved_location(location));
    }
    lines
}

fn layer_label(layer: AssetLayer) -> &'static str {
    match layer {
        AssetLayer::Shipped => "shipped",
        AssetLayer::Dev => "dev",
        AssetLayer::User => "user",
    }
}

fn format_source_line(source: &AssetSource) -> String {
    match source {
        AssetSource::EngineContent { root } => format!("engine_content ({})", root.display()),
        AssetSource::EngineBundle { bundle_id, source } => {
            format!("engine_bundle {} ({})", bundle_id, source.display())
        }
        AssetSource::Quake1 { mount_kind, source } => {
            format!("quake1 {} {}", mount_kind, source.display())
        }
        AssetSource::QuakeLive { mount_kind, source } => {
            format!("quakelive {} {}", mount_kind, source.display())
        }
    }
}

fn format_quake_entry(entry: &QuakeEntry) -> String {
    let source_path = match &entry.source {
        engine_core::quake_index::QuakeSource::LooseFile { root } => {
            root.join(&entry.path).display().to_string()
        }
        _ => entry.source.source_path().display().to_string(),
    };
    let mut extra = String::new();
    match &entry.source {
        engine_core::quake_index::QuakeSource::Pak {
            file_index, offset, ..
        } => {
            extra = format!(" index={} offset={}", file_index, offset);
        }
        engine_core::quake_index::QuakeSource::Pk3 { file_index, .. } => {
            extra = format!(" index={}", file_index);
        }
        _ => {}
    }
    format!(
        "{} order={} kind={} size={} hash={:016x} source={} {}{}",
        entry.mount_kind,
        entry.mount_order,
        entry.kind.as_str(),
        entry.size,
        entry.hash,
        entry.source.kind_label(),
        source_path,
        extra
    )
}

fn load_quake_index_for_console(
    path_policy: &PathPolicy,
    quake_dir: Option<&Path>,
) -> Result<QuakeIndex, String> {
    if let Some(quake_dir) = quake_dir {
        if !quake_dir.is_dir() {
            return Err(format!("quake dir not found: {}", quake_dir.display()));
        }
        return QuakeIndex::load_or_build(path_policy.content_root(), quake_dir);
    }
    let path = QuakeIndex::default_index_path(path_policy.content_root());
    if !path.is_file() {
        return Err(format!(
            "quake index not found: {} (run `tools quake index --quake-dir <path>`)",
            path.display()
        ));
    }
    QuakeIndex::read_from(&path)
}

fn request_asset_reload(asset_manager: &AssetManager, key: AssetKey) -> Result<(), String> {
    let opts = RequestOpts {
        priority: AssetPriority::High,
        budget_tag: AssetBudgetTag::Boot,
    };
    let canonical = key.canonical().to_string();
    match (key.namespace(), key.kind()) {
        ("engine", "config") => {
            let _ = asset_manager.reload::<ConfigAsset>(key, opts)?;
            Ok(())
        }
        ("engine", "script") => {
            let _ = asset_manager.reload::<ScriptAsset>(key, opts)?;
            Ok(())
        }
        ("engine", "text") => {
            let _ = asset_manager.reload::<TextAsset>(key, opts)?;
            Ok(())
        }
        ("engine", "test_map") => {
            let _ = asset_manager.reload::<TestMapAsset>(key, opts)?;
            Ok(())
        }
        ("engine", "blob") => {
            let _ = asset_manager.reload::<BlobAsset>(key, opts)?;
            Ok(())
        }
        ("engine", "texture") => {
            let _ = asset_manager.reload::<TextureAsset>(key, opts)?;
            Ok(())
        }
        ("quake1", "raw") => {
            let _ = asset_manager.reload::<QuakeRawAsset>(key, opts)?;
            Ok(())
        }
        _ => Err(format!("unsupported asset kind: {}", canonical)),
    }
}

fn run_content_validate(
    path_policy: PathPolicy,
    quake_dir: Option<PathBuf>,
) -> Result<Vec<String>, String> {
    let resolver = AssetResolver::new(&path_policy, None);
    let quake_index = match quake_dir.as_ref() {
        Some(dir) => {
            if !dir.is_dir() {
                return Err(format!("quake dir not found: {}", dir.display()));
            }
            Some(QuakeIndex::load_or_build(path_policy.content_root(), dir)?)
        }
        None => None,
    };
    let manifests = discover_level_manifests(&path_policy).map_err(|err| err.to_string())?;
    if manifests.is_empty() {
        return Ok(vec!["no level manifests found".to_string()]);
    }
    let mut lines = Vec::new();
    let mut errors = 0usize;
    for entry in manifests {
        match load_level_manifest(&entry.path) {
            Ok(manifest) => {
                errors += validate_level_manifest_lines(
                    &path_policy,
                    &entry,
                    &manifest,
                    &resolver,
                    quake_index.as_ref(),
                    &mut lines,
                );
            }
            Err(err) => {
                lines.push(err.to_string());
                errors += 1;
            }
        }
    }
    lines.push(format!("content validate: {} errors", errors));
    Ok(lines)
}

fn validate_level_manifest_lines(
    path_policy: &PathPolicy,
    entry: &LevelManifestPath,
    manifest: &LevelManifest,
    resolver: &AssetResolver,
    quake_index: Option<&QuakeIndex>,
    lines: &mut Vec<String>,
) -> usize {
    let mut errors = 0usize;
    let manifest_path = &entry.path;

    if let Some(geometry) = &manifest.geometry {
        if let Some(index) = quake_index {
            let path = quake_bsp_path(geometry);
            if index.which(&path).is_none() {
                lines.push(format!(
                    "{}{} [geometry]: missing quake asset {}",
                    manifest_path.display(),
                    format_line(manifest.lines.geometry),
                    geometry.canonical()
                ));
                errors += 1;
            }
        } else {
            lines.push(format!(
                "{}{} [geometry]: quake dir required to validate {}",
                manifest_path.display(),
                format_line(manifest.lines.geometry),
                geometry.canonical()
            ));
            errors += 1;
        }
    }

    for (field, items, line) in [
        ("assets", &manifest.assets, manifest.lines.assets),
        ("requires", &manifest.requires, manifest.lines.requires),
    ] {
        for key in items {
            if key.namespace() == "engine" && key.kind() == "level" {
                if resolve_level_manifest_path(path_policy, key).is_err() {
                    lines.push(format!(
                        "{}{} [{}]: missing level manifest {}",
                        manifest_path.display(),
                        format_line(line),
                        field,
                        key.canonical()
                    ));
                    errors += 1;
                }
                continue;
            }
            if resolver.resolve(key).is_err() {
                lines.push(format!(
                    "{}{} [{}]: missing asset {}",
                    manifest_path.display(),
                    format_line(line),
                    field,
                    key.canonical()
                ));
                errors += 1;
            }
        }
    }

    errors
}

fn format_line(line: Option<usize>) -> String {
    line.map(|value| format!(":{}", value)).unwrap_or_default()
}

fn quake_bsp_path(key: &AssetKey) -> String {
    format!("maps/{}.bsp", key.path())
}

#[derive(Clone, Debug)]
struct LogFilterState {
    min_level: LogLevel,
    filter: Option<String>,
}

impl Default for LogFilterState {
    fn default() -> Self {
        Self {
            min_level: LogLevel::Info,
            filter: None,
        }
    }
}

impl LogFilterState {
    fn allows(&self, level: LogLevel, message: &str) -> bool {
        if log_level_rank(level) > log_level_rank(self.min_level) {
            return false;
        }
        if let Some(filter) = self.filter.as_ref() {
            let needle = filter.as_str();
            if !message.to_ascii_lowercase().contains(needle) {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug)]
enum CaptureKind {
    Screenshot,
    Frame,
}

impl CaptureKind {
    fn label(self) -> &'static str {
        match self {
            CaptureKind::Screenshot => "capture_screenshot",
            CaptureKind::Frame => "capture_frame",
        }
    }

    fn file_prefix(self) -> &'static str {
        match self {
            CaptureKind::Screenshot => "screenshot",
            CaptureKind::Frame => "frame",
        }
    }
}

#[derive(Clone, Debug)]
struct CaptureRequest {
    kind: CaptureKind,
    path: Option<PathBuf>,
    include_overlays: bool,
}

fn log_level_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Error => 0,
        LogLevel::Warn => 1,
        LogLevel::Info => 2,
        LogLevel::Debug => 3,
    }
}

fn parse_log_level(value: &str) -> Option<LogLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" | "err" => Some(LogLevel::Error),
        "warn" | "warning" => Some(LogLevel::Warn),
        "info" => Some(LogLevel::Info),
        "debug" => Some(LogLevel::Debug),
        _ => None,
    }
}

fn update_log_filter_state(
    cvars: &CvarRegistry,
    core: &CoreCvars,
    log_filter_state: &Arc<Mutex<LogFilterState>>,
) {
    let level_value = cvar_string(cvars, core.log_level)
        .and_then(|value| parse_log_level(&value))
        .unwrap_or(LogLevel::Info);
    let filter_value = cvar_string(cvars, core.log_filter)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    if let Ok(mut state) = log_filter_state.lock() {
        state.min_level = level_value;
        state.filter = filter_value;
    }
}

fn update_perf_overlay(perf: &mut PerfState, cvars: &CvarRegistry, core: &CoreCvars) {
    let master = cvar_bool(cvars, core.dbg_overlay).unwrap_or(false);
    let perf_enabled = cvar_bool(cvars, core.dbg_perf_hud).unwrap_or(false);
    let next = master && perf_enabled;
    if perf.show_overlay != next {
        perf.show_overlay = next;
        perf.hud_dirty = true;
    }
}

fn sanitize_capture_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push('_');
        }
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_default_capture_path(
    kind: CaptureKind,
    seq: u32,
    resolution: [u32; 2],
    window_mode: WindowMode,
    map_name: Option<&str>,
) -> PathBuf {
    let mut name = format!(
        "{}_{:04}_{}x{}_{}",
        kind.file_prefix(),
        seq,
        resolution[0],
        resolution[1],
        sanitize_capture_segment(window_mode.as_str()),
    );
    if let Some(map) = map_name {
        let map = sanitize_capture_segment(map);
        name.push('_');
        name.push_str(&map);
    }
    name.push_str(".png");
    PathBuf::from("captures").join(name)
}

fn apply_cvar_changes(
    cvars: &mut CvarRegistry,
    perf: &mut PerfState,
    core: &CoreCvars,
    asset_manager: &AssetManager,
    log_filter_state: &Arc<Mutex<LogFilterState>>,
) {
    for id in cvars.take_dirty() {
        if id == core.dbg_perf_hud || id == core.dbg_overlay {
            update_perf_overlay(perf, cvars, core);
        }
        if id == core.log_level || id == core.log_filter {
            update_log_filter_state(cvars, core, log_filter_state);
        }
        if id == core.asset_decode_budget_ms {
            if let Some(value) = cvar_int(cvars, id) {
                asset_manager.set_decode_budget_ms_per_tick(value.max(0) as u64);
            }
        }
    }
}

fn register_collision_debug_cvars(
    registry: &mut CvarRegistry,
) -> Result<CollisionDebugCvars, String> {
    let mut flags = CvarFlags::DEV_ONLY;
    flags.insert(CvarFlags::NO_PERSIST);
    let dev_collision_draw = registry.register(
        CvarDef::new(
            "dev_collision_draw",
            CvarValue::Bool(false),
            "Draw collision debug overlay.",
        )
        .with_flags(flags),
    )?;
    Ok(CollisionDebugCvars { dev_collision_draw })
}

fn register_movement_cvars(registry: &mut CvarRegistry) -> Result<MovementCvars, String> {
    let air_max_speed = registry.register(
        CvarDef::new(
            "arena_air_max_speed",
            CvarValue::Float(TEST_MAP_MAX_AIR_SPEED),
            "Arena air max speed.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let air_accel = registry.register(
        CvarDef::new(
            "arena_air_accel",
            CvarValue::Float(TEST_MAP_AIR_ACCEL),
            "Arena air acceleration.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let air_resistance = registry.register(
        CvarDef::new(
            "arena_air_resistance",
            CvarValue::Float(TEST_MAP_AIR_RESISTANCE),
            "Arena air resistance (speed-scaled).",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: Some(10.0),
        }),
    )?;
    let air_resistance_speed_scale = registry.register(
        CvarDef::new(
            "arena_air_resistance_speed_scale",
            CvarValue::Float(TEST_MAP_MAX_AIR_SPEED * 16.0),
            "Arena air resistance reaches full strength at this speed.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.01),
            max: None,
        }),
    )?;
    let golden_target_deg = registry.register(
        CvarDef::new(
            "arena_golden_target_deg",
            CvarValue::Float(TEST_MAP_GOLDEN_ANGLE_TARGET.to_degrees()),
            "Golden angle target (degrees, view-forward relative).",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: Some(180.0),
        }),
    )?;
    let golden_gain_min = registry.register(
        CvarDef::new(
            "arena_golden_gain_min",
            CvarValue::Float(TEST_MAP_GOLDEN_ANGLE_GAIN_MIN),
            "Golden angle minimum gain.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let golden_gain_peak = registry.register(
        CvarDef::new(
            "arena_golden_gain_peak",
            CvarValue::Float(TEST_MAP_GOLDEN_ANGLE_GAIN_PEAK),
            "Golden angle peak gain.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let golden_bonus_scale = registry.register(
        CvarDef::new(
            "arena_golden_bonus_scale",
            CvarValue::Float(0.25),
            "Golden angle bonus scale for uncapped speed growth.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let golden_blend_start = registry.register(
        CvarDef::new(
            "arena_golden_blend_start",
            CvarValue::Float(TEST_MAP_GOLDEN_ANGLE_BLEND_START),
            "Golden angle blend start speed.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let golden_blend_end = registry.register(
        CvarDef::new(
            "arena_golden_blend_end",
            CvarValue::Float(TEST_MAP_GOLDEN_ANGLE_BLEND_END),
            "Golden angle blend end speed.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let corridor_shaping_strength = registry.register(
        CvarDef::new(
            "arena_cs_strength_deg",
            CvarValue::Float(TEST_MAP_CORRIDOR_SHAPING_STRENGTH_DEG),
            "Corridor shaping strength (degrees/sec).",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let corridor_shaping_min_speed = registry.register(
        CvarDef::new(
            "arena_cs_min_speed",
            CvarValue::Float(TEST_MAP_CORRIDOR_SHAPING_MIN_SPEED),
            "Corridor shaping minimum speed.",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: None,
        }),
    )?;
    let corridor_shaping_max_angle = registry.register(
        CvarDef::new(
            "arena_cs_max_angle_deg",
            CvarValue::Float(TEST_MAP_CORRIDOR_SHAPING_MAX_ANGLE_DEG),
            "Corridor shaping max angle per tick (degrees).",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: Some(180.0),
        }),
    )?;
    let corridor_shaping_min_alignment = registry.register(
        CvarDef::new(
            "arena_cs_min_alignment",
            CvarValue::Float(TEST_MAP_CORRIDOR_SHAPING_MIN_ALIGNMENT),
            "Corridor shaping minimum alignment (dot).",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(-1.0),
            max: Some(1.0),
        }),
    )?;
    let dev_motor = registry.register(
        CvarDef::new(
            "dev_motor",
            CvarValue::Int(1),
            "Test map motor selection (1=arena, 2=rpg).",
        )
        .with_bounds(CvarBounds::Int {
            min: Some(1),
            max: Some(2),
        })
        .with_flags(CvarFlags::NO_PERSIST),
    )?;
    let dev_fixed_dt = registry.register(
        CvarDef::new(
            "dev_fixed_dt",
            CvarValue::Float(0.0),
            "Test map fixed timestep in seconds (0 disables).",
        )
        .with_bounds(CvarBounds::Float {
            min: Some(0.0),
            max: Some(1.0),
        })
        .with_flags(CvarFlags::NO_PERSIST),
    )?;
    let dev_substeps = registry.register(
        CvarDef::new(
            "dev_substeps",
            CvarValue::Int(1),
            "Test map substeps per fixed tick (1..16).",
        )
        .with_bounds(CvarBounds::Int {
            min: Some(1),
            max: Some(16),
        })
        .with_flags(CvarFlags::NO_PERSIST),
    )?;
    Ok(MovementCvars {
        air_max_speed,
        air_accel,
        air_resistance,
        air_resistance_speed_scale,
        golden_target_deg,
        golden_gain_min,
        golden_gain_peak,
        golden_bonus_scale,
        golden_blend_start,
        golden_blend_end,
        corridor_shaping_strength,
        corridor_shaping_min_speed,
        corridor_shaping_max_angle,
        corridor_shaping_min_alignment,
        dev_motor,
        dev_fixed_dt,
        dev_substeps,
    })
}

fn apply_movement_cvars(
    cvars: &CvarRegistry,
    ids: &MovementCvars,
    runtime: &mut TestMapRuntime,
    camera: &mut CameraState,
) {
    let config = runtime.controller.motor_mut().arena_config_mut();
    if let Some(value) = cvar_float(cvars, ids.air_max_speed) {
        config.max_speed_air = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.air_accel) {
        config.air_accel = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.air_resistance) {
        config.air_resistance = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.air_resistance_speed_scale) {
        config.air_resistance_speed_scale = value.max(0.01);
    }
    if let Some(value) = cvar_float(cvars, ids.golden_target_deg) {
        config.golden_angle_target = value.max(0.0).to_radians();
    }
    if let Some(value) = cvar_float(cvars, ids.golden_gain_min) {
        config.golden_angle_gain_min = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.golden_gain_peak) {
        config.golden_angle_gain_peak = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.golden_bonus_scale) {
        config.golden_angle_bonus_scale = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.golden_blend_start) {
        config.golden_angle_blend_speed_start = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.golden_blend_end) {
        config.golden_angle_blend_speed_end = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.corridor_shaping_strength) {
        config.corridor_shaping_strength = value.max(0.0).to_radians();
    }
    if let Some(value) = cvar_float(cvars, ids.corridor_shaping_min_speed) {
        config.corridor_shaping_min_speed = value.max(0.0);
    }
    if let Some(value) = cvar_float(cvars, ids.corridor_shaping_max_angle) {
        config.corridor_shaping_max_angle_per_tick = value.max(0.0).to_radians();
    }
    if let Some(value) = cvar_float(cvars, ids.corridor_shaping_min_alignment) {
        config.corridor_shaping_min_alignment = value.clamp(-1.0, 1.0);
    }
    let motor_value = cvar_int(cvars, ids.dev_motor).unwrap_or(1);
    let motor_kind = MotorKind::from_cvar(motor_value);
    switch_test_map_motor(runtime, camera, motor_kind);
}

fn cvar_bool(cvars: &CvarRegistry, id: CvarId) -> Option<bool> {
    match cvars.get(id)?.value {
        engine_core::control_plane::CvarValue::Bool(value) => Some(value),
        _ => None,
    }
}

fn cvar_int(cvars: &CvarRegistry, id: CvarId) -> Option<i32> {
    match cvars.get(id)?.value {
        engine_core::control_plane::CvarValue::Int(value) => Some(value),
        _ => None,
    }
}

fn cvar_float(cvars: &CvarRegistry, id: CvarId) -> Option<f32> {
    match cvars.get(id)?.value {
        engine_core::control_plane::CvarValue::Float(value) => Some(value),
        _ => None,
    }
}

fn cvar_string(cvars: &CvarRegistry, id: CvarId) -> Option<String> {
    match cvars.get(id)?.value {
        engine_core::control_plane::CvarValue::String(ref value) => Some(value.clone()),
        _ => None,
    }
}

fn parse_toggle_arg(args: &CommandArgs) -> Result<Option<bool>, String> {
    match args.positional(0) {
        Some(value) => match value {
            "1" => Ok(Some(true)),
            "0" => Ok(Some(false)),
            _ => Err(format!("expected 0/1, got {}", value)),
        },
        None => Ok(None),
    }
}

fn load_quake_raw_asset(
    asset_manager: &AssetManager,
    asset_path: &str,
    budget_tag: AssetBudgetTag,
) -> Result<Arc<Vec<u8>>, ExitError> {
    let key = AssetKey::from_parts("quake1", "raw", asset_path).map_err(|err| {
        ExitError::new(
            EXIT_USAGE,
            format!("quake asset key invalid ({}): {}", asset_path, err),
        )
    })?;
    let handle = asset_manager.request::<QuakeRawAsset>(
        key.clone(),
        RequestOpts {
            priority: AssetPriority::High,
            budget_tag,
        },
    );
    let asset = asset_manager
        .await_ready(&handle, Duration::from_secs(2))
        .map_err(|err| {
            ExitError::new(
                EXIT_PAK,
                format!("quake asset load failed ({}): {}", key.canonical(), err),
            )
        })?;
    Ok(Arc::clone(&asset.bytes))
}

fn quake_asset_path_from_vpath(path: &str) -> &str {
    path.strip_prefix("raw/quake/").unwrap_or(path)
}

fn load_wav_sfx(
    asset_manager: &AssetManager,
    asset: &str,
    budget_tag: AssetBudgetTag,
) -> Result<Vec<u8>, ExitError> {
    let asset_name = normalize_asset_name(asset);
    let bytes = load_quake_raw_asset(asset_manager, &asset_name, budget_tag)?;
    println!("loaded {} via asset manager", asset_name);
    Ok((*bytes).clone())
}

fn load_music_track(
    asset_manager: &AssetManager,
    vfs: &Vfs,
) -> Result<Option<MusicTrack>, ExitError> {
    for dir in [quake_vpath("music")] {
        if let Some(track) = find_music_in_dir(asset_manager, vfs, &dir)? {
            return Ok(Some(track));
        }
    }
    Ok(None)
}

fn find_music_in_dir(
    asset_manager: &AssetManager,
    vfs: &Vfs,
    dir: &str,
) -> Result<Option<MusicTrack>, ExitError> {
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
        let vpath = format!("{}/{}", dir, name);
        let asset_path = quake_asset_path_from_vpath(&vpath);
        let data = load_quake_raw_asset(asset_manager, asset_path, AssetBudgetTag::Streaming)?;
        println!("loaded music {} via asset manager", asset_path);
        return Ok(Some(MusicTrack {
            name: vpath,
            data: (*data).clone(),
        }));
    }

    Ok(None)
}

fn parse_map_request(map: &str) -> Result<MapRequest, String> {
    let trimmed = map.trim();
    if trimmed.is_empty() {
        return Err("map name must not be empty".to_string());
    }
    if trimmed.contains(':') {
        let key = AssetKey::parse(trimmed)
            .map_err(|err| format!("invalid map asset id '{}': {}", trimmed, err))?;
        if key.namespace() == "engine" && key.kind() == "test_map" {
            return Ok(MapRequest::TestMap(key));
        }
        return Err(format!(
            "map asset id must be engine:test_map, got {}",
            key.canonical()
        ));
    }

    let maybe_path = if let Some(stripped) = trimmed.strip_prefix("test_map/") {
        Some(stripped)
    } else if let Some(stripped) = trimmed.strip_prefix("test_maps/") {
        Some(stripped)
    } else if trimmed.ends_with(".toml") {
        Some(trimmed)
    } else {
        None
    };

    if let Some(path) = maybe_path {
        let key = AssetKey::from_parts("engine", "test_map", path)
            .map_err(|err| format!("invalid test map path '{}': {}", path, err))?;
        return Ok(MapRequest::TestMap(key));
    }

    Ok(MapRequest::Bsp(trimmed.to_string()))
}

fn parse_test_map_key_arg(value: &str) -> Result<AssetKey, String> {
    match parse_map_request(value)? {
        MapRequest::TestMap(key) => Ok(key),
        MapRequest::Bsp(_) => Err("expected a test map id (engine:test_map/...)".to_string()),
    }
}

fn load_scene(
    asset_manager: &AssetManager,
    quake_vfs: Option<&Vfs>,
    map: &str,
) -> Result<LoadedScene, ExitError> {
    match parse_map_request(map).map_err(|err| ExitError::new(EXIT_USAGE, err))? {
        MapRequest::Bsp(name) => {
            if quake_vfs.is_none() {
                return Err(ExitError::new(
                    EXIT_QUAKE_DIR,
                    "quake mounts not configured for map load",
                ));
            }
            let (mesh, bounds, scene_collision, spawn) = load_bsp_scene(asset_manager, &name)?;
            Ok(LoadedScene {
                mesh,
                bounds,
                collision: Some(scene_collision),
                spawn,
                kind: SceneKind::Bsp,
                test_map: None,
            })
        }
        MapRequest::TestMap(key) => {
            let (mesh, bounds, test_map) = load_test_map_scene(asset_manager, &key, false)?;
            Ok(LoadedScene {
                mesh,
                bounds,
                collision: None,
                spawn: None,
                kind: SceneKind::TestMap,
                test_map: Some(test_map),
            })
        }
    }
}

fn load_lmp_image(asset_manager: &AssetManager, asset: &str) -> Result<ImageData, ExitError> {
    let palette_bytes =
        load_quake_raw_asset(asset_manager, "gfx/palette.lmp", AssetBudgetTag::Boot)?;
    let palette = lmp::parse_palette(&palette_bytes)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("palette parse failed: {}", err)))?;

    let asset_name = normalize_asset_name(asset);
    let image_bytes = load_quake_raw_asset(asset_manager, &asset_name, AssetBudgetTag::Boot)?;
    let image = lmp::parse_lmp_image(&image_bytes)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("image parse failed: {}", err)))?;
    let rgba = image.to_rgba8(&palette);
    let image = ImageData::new(image.width, image.height, rgba)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("image data failed: {}", err)))?;

    println!(
        "loaded {} via asset manager ({}x{})",
        asset_name, image.width, image.height
    );

    Ok(image)
}

fn load_test_map_scene(
    asset_manager: &AssetManager,
    key: &AssetKey,
    reload: bool,
) -> Result<(MeshData, Bounds, TestMapSceneData), ExitError> {
    let asset = fetch_test_map_asset(asset_manager, key, reload)?;
    println!(
        "loaded {} via asset manager ({} solids)",
        key.canonical(),
        asset.solids.len()
    );
    let map = asset.map.clone();
    let solids = asset.solids.clone();
    let (mesh, bounds) = build_test_map_mesh(&map, &solids)?;
    let collision_world_key = collision_world_key_for_test_map(key)?;
    let collision_world_asset =
        fetch_collision_world_asset(asset_manager, &collision_world_key, reload)?;
    let collision_world = collision_world_asset.world.clone();
    let map_scale = map.map_to_world_scale.unwrap_or(1.0);
    if (collision_world.map_to_world_scale - map_scale).abs() > 1.0e-3 {
        return Err(ExitError::new(
            EXIT_SCENE,
            format!(
                "collision world scale mismatch (map {:.3} vs {} {:.3})",
                map_scale,
                collision_world_key.canonical(),
                collision_world.map_to_world_scale
            ),
        ));
    }
    println!(
        "loaded {} via asset manager ({} chunks)",
        collision_world_key.canonical(),
        collision_world.chunks.len()
    );
    Ok((
        mesh,
        bounds,
        TestMapSceneData {
            key: key.clone(),
            map,
            collision_world_key,
            collision_world,
        },
    ))
}

fn fetch_test_map_asset(
    asset_manager: &AssetManager,
    key: &AssetKey,
    reload: bool,
) -> Result<Arc<TestMapAsset>, ExitError> {
    let opts = RequestOpts {
        priority: AssetPriority::High,
        budget_tag: AssetBudgetTag::Boot,
    };
    let handle = if reload {
        asset_manager
            .reload::<TestMapAsset>(key.clone(), opts)
            .map_err(|err| {
                ExitError::new(
                    EXIT_SCENE,
                    format!("test map reload failed ({}): {}", key.canonical(), err),
                )
            })?
    } else {
        asset_manager.request::<TestMapAsset>(key.clone(), opts)
    };
    asset_manager
        .await_ready(&handle, Duration::from_secs(2))
        .map_err(|err| {
            ExitError::new(
                EXIT_SCENE,
                format!("test map load failed ({}): {}", key.canonical(), err),
            )
        })
}

fn collision_world_key_for_test_map(key: &AssetKey) -> Result<AssetKey, ExitError> {
    if key.namespace() != "engine" || key.kind() != "test_map" {
        return Err(ExitError::new(
            EXIT_SCENE,
            format!("expected engine:test_map key, got {}", key.canonical()),
        ));
    }
    let path = if key.path().starts_with("test_maps/") {
        key.path().to_string()
    } else {
        format!("test_maps/{}", key.path())
    };
    AssetKey::from_parts("engine", "collision_world", &path).map_err(|err| {
        ExitError::new(
            EXIT_SCENE,
            format!("collision world key invalid ({}): {}", path, err),
        )
    })
}

fn fetch_collision_world_asset(
    asset_manager: &AssetManager,
    key: &AssetKey,
    reload: bool,
) -> Result<Arc<CollisionWorldAsset>, ExitError> {
    let opts = RequestOpts {
        priority: AssetPriority::High,
        budget_tag: AssetBudgetTag::Boot,
    };
    let handle = if reload {
        asset_manager
            .reload::<CollisionWorldAsset>(key.clone(), opts)
            .map_err(|err| {
                ExitError::new(
                    EXIT_SCENE,
                    format!(
                        "collision world reload failed ({}): {}",
                        key.canonical(),
                        err
                    ),
                )
            })?
    } else {
        asset_manager.request::<CollisionWorldAsset>(key.clone(), opts)
    };
    asset_manager
        .await_ready(&handle, Duration::from_secs(2))
        .map_err(|err| {
            ExitError::new(
                EXIT_SCENE,
                format!("collision world load failed ({}): {}", key.canonical(), err),
            )
        })
}

fn load_bsp_scene(
    asset_manager: &AssetManager,
    map: &str,
) -> Result<(MeshData, Bounds, SceneCollision, Option<SpawnPoint>), ExitError> {
    let map_name = normalize_map_asset(map);
    let bsp_bytes = load_quake_raw_asset(asset_manager, &map_name, AssetBudgetTag::Boot)?;
    let bsp = bsp::parse_bsp(&bsp_bytes)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("bsp parse failed: {}", err)))?;

    println!(
        "loaded {} via asset manager ({} vertices, {} faces)",
        map_name,
        bsp.vertices.len(),
        bsp.faces.len()
    );

    let spawn = bsp::parse_spawn(&bsp_bytes, &bsp.header)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("bsp spawn parse failed: {}", err)))?;

    let (mesh, bounds, collision) = build_scene_mesh(&bsp)?;
    Ok((mesh, bounds, collision, spawn))
}

// NOTE: Test maps are a graybox-only renderer path. We build simple per-triangle
// vertex colors from normals to keep this visualization deterministic and
// dependency-free. This is intentionally minimal because it is expected to be
// replaced by a proper render asset pipeline later, so keep changes local and
// well-documented if you refactor this path.
fn build_test_map_mesh(
    map: &TestMap,
    solids: &[ResolvedSolid],
) -> Result<(MeshData, Bounds), ExitError> {
    let scale = map.map_to_world_scale.unwrap_or(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(ExitError::new(
            EXIT_SCENE,
            "test map scale must be finite and > 0",
        ));
    }

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut bounds = Bounds::empty();

    for solid in solids {
        let size = [
            solid.size[0] * scale,
            solid.size[1] * scale,
            solid.size[2] * scale,
        ];
        let center = Vec3::from(solid.pos).scale(scale);
        match solid.kind {
            SolidKind::Box | SolidKind::BoxRot => add_test_map_box(
                &mut vertices,
                &mut indices,
                &mut bounds,
                center,
                size,
                solid.yaw_deg,
                solid.rot_euler_deg,
            )?,
            SolidKind::Ramp => add_test_map_ramp(
                &mut vertices,
                &mut indices,
                &mut bounds,
                center,
                size,
                solid.yaw_deg,
                solid.rot_euler_deg,
            )?,
            SolidKind::Cylinder => add_test_map_cylinder(
                &mut vertices,
                &mut indices,
                &mut bounds,
                center,
                size,
                solid.yaw_deg,
                solid.rot_euler_deg,
            )?,
        }
    }

    let mesh = MeshData::new(vertices, indices).map_err(|err| {
        ExitError::new(EXIT_SCENE, format!("test map mesh build failed: {}", err))
    })?;
    Ok((mesh, bounds))
}

fn test_map_motor_config() -> ArenaMotorConfig {
    ArenaMotorConfig {
        max_speed_ground: TEST_MAP_MOVE_SPEED,
        max_speed_air: TEST_MAP_MAX_AIR_SPEED,
        ground_accel: TEST_MAP_ACCEL,
        air_accel: TEST_MAP_AIR_ACCEL,
        friction: TEST_MAP_FRICTION,
        stop_speed: TEST_MAP_STOP_SPEED,
        gravity: TEST_MAP_GRAVITY,
        jump_speed: TEST_MAP_JUMP_SPEED,
        air_resistance: TEST_MAP_AIR_RESISTANCE,
        air_resistance_speed_scale: TEST_MAP_MAX_AIR_SPEED * 16.0,
        golden_angle_target: TEST_MAP_GOLDEN_ANGLE_TARGET,
        golden_angle_gain_min: TEST_MAP_GOLDEN_ANGLE_GAIN_MIN,
        golden_angle_gain_peak: TEST_MAP_GOLDEN_ANGLE_GAIN_PEAK,
        golden_angle_bonus_scale: 0.25,
        golden_angle_blend_speed_start: TEST_MAP_GOLDEN_ANGLE_BLEND_START,
        golden_angle_blend_speed_end: TEST_MAP_GOLDEN_ANGLE_BLEND_END,
        corridor_shaping_strength: TEST_MAP_CORRIDOR_SHAPING_STRENGTH_DEG.to_radians(),
        corridor_shaping_min_speed: TEST_MAP_CORRIDOR_SHAPING_MIN_SPEED,
        corridor_shaping_max_angle_per_tick: TEST_MAP_CORRIDOR_SHAPING_MAX_ANGLE_DEG.to_radians(),
        corridor_shaping_min_alignment: TEST_MAP_CORRIDOR_SHAPING_MIN_ALIGNMENT,
        jump_buffer_enabled: true,
        jump_buffer_window: TEST_MAP_JUMP_BUFFER_WINDOW,
        frictionless_jump_mode: FrictionlessJumpMode::Soft,
        frictionless_jump_grace: TEST_MAP_BHOP_GRACE,
        frictionless_jump_friction_scale: TEST_MAP_BHOP_FRICTION_SCALE,
        frictionless_jump_friction_scale_best_angle: TEST_MAP_BHOP_FRICTION_SCALE_BEST_ANGLE,
    }
}

fn test_map_rpg_motor_config() -> RpgMotorConfig {
    RpgMotorConfig::default()
}

#[derive(Clone, Copy, Debug)]
struct CameraMotorTuning {
    speed: f32,
    accel: f32,
    friction: f32,
    gravity: f32,
    jump_speed: f32,
}

fn camera_tuning_from_arena(config: ArenaMotorConfig) -> CameraMotorTuning {
    CameraMotorTuning {
        speed: config.max_speed_ground,
        accel: config.ground_accel,
        friction: config.friction,
        gravity: config.gravity,
        jump_speed: config.jump_speed,
    }
}

fn camera_tuning_from_rpg(config: RpgMotorConfig) -> CameraMotorTuning {
    CameraMotorTuning {
        speed: config.max_speed_ground,
        accel: config.ground_accel,
        friction: config.friction,
        gravity: config.gravity,
        jump_speed: config.jump_speed,
    }
}

fn apply_test_map_camera_tuning(camera: &mut CameraState, tuning: CameraMotorTuning) {
    camera.speed = tuning.speed;
    camera.accel = tuning.accel;
    camera.friction = tuning.friction;
    camera.gravity = tuning.gravity;
    camera.jump_speed = tuning.jump_speed;
}

fn snap_test_map_runtime_to_ground(runtime: &mut TestMapRuntime, bounds: &Bounds) {
    let extent = bounds.extent();
    let drop = (extent.y + extent.x.max(extent.z)).max(1.0) + runtime.capsule_offset + 2.0;
    let desired = Vector::new(0.0, -drop, 0.0);
    let result = runtime.controller.collision_mut().move_character(
        &runtime.world,
        runtime.position,
        desired,
        runtime.grounded,
        1.0 / 60.0,
    );
    runtime.position = result.position;
    runtime.prev_position = runtime.position;
    runtime.grounded = result.grounded;
    runtime.ground_normal = result.ground_normal;
    runtime.prev_velocity = runtime.velocity;
    let state = runtime.controller.state_mut();
    state.position = runtime.position;
    state.velocity = Vector::new(runtime.velocity.x, runtime.velocity.y, runtime.velocity.z);
    state.grounded = runtime.grounded;
    state.ground_normal = runtime.ground_normal;
}

enum CollisionChunkSelection {
    Bounds(CollisionAabb),
}

fn select_collision_chunks(world: &CollisionWorld, selection: CollisionChunkSelection) -> Vec<u32> {
    match selection {
        CollisionChunkSelection::Bounds(bounds) => world
            .chunk_bounds_bvh
            .select_intersecting(&world.chunks, &bounds),
    }
}

fn collision_interest_bounds(position: Vector<Real>, radius: f32) -> CollisionAabb {
    CollisionAabb {
        min: [
            position.x - radius,
            position.y - radius,
            position.z - radius,
        ],
        max: [
            position.x + radius,
            position.y + radius,
            position.z + radius,
        ],
    }
}

fn build_test_map_collision_runtime(
    world: &mut PhysicsWorld,
    data: &TestMapSceneData,
) -> Result<CollisionWorldRuntime, ExitError> {
    let colliders = build_test_map_colliders(&data.map).map_err(|err| {
        ExitError::new(
            EXIT_SCENE,
            format!("test map collider build failed: {}", err),
        )
    })?;
    let mut collider_by_id = HashMap::new();
    for collider in colliders.colliders {
        collider_by_id.insert(collider.id.clone(), collider.collider);
    }

    let selection = CollisionChunkSelection::Bounds(data.collision_world.root_bounds);
    let selected_chunks = select_collision_chunks(&data.collision_world, selection);
    if selected_chunks.is_empty() {
        return Err(ExitError::new(
            EXIT_SCENE,
            format!(
                "collision world has no selectable chunks ({})",
                data.collision_world_key.canonical()
            ),
        ));
    }
    let mut collider_handles = Vec::new();
    let mut triangle_count = 0u64;
    for chunk_index in &selected_chunks {
        let chunk = match data.collision_world.chunks.get(*chunk_index as usize) {
            Some(chunk) => chunk,
            None => continue,
        };
        let payload_id = chunk
            .payload_ref
            .strip_prefix("inline:test_map/")
            .unwrap_or(&chunk.chunk_id);
        let collider = match collider_by_id.remove(payload_id) {
            Some(collider) => collider,
            None => {
                eprintln!(
                    "collision world chunk missing collider: {} (payload {})",
                    chunk.chunk_id, chunk.payload_ref
                );
                continue;
            }
        };
        collider_handles.push(world.insert_static_collider(collider));
        triangle_count = triangle_count.saturating_add(chunk.triangle_count as u64);
    }
    if collider_handles.is_empty() {
        return Err(ExitError::new(
            EXIT_SCENE,
            format!(
                "collision world produced zero colliders ({})",
                data.collision_world_key.canonical()
            ),
        ));
    }

    Ok(CollisionWorldRuntime {
        world: data.collision_world.clone(),
        loaded_chunks: selected_chunks,
        collider_handles,
        triangle_count,
    })
}

// NOTE: Test map runtime routes through the shared controller module
// (input -> motor -> collision -> camera) so gameplay and tests stay aligned.
fn build_test_map_runtime(
    data: &TestMapSceneData,
    bounds: &Bounds,
) -> Result<TestMapRuntime, ExitError> {
    let arena_config = test_map_motor_config();
    let rpg_config = test_map_rpg_motor_config();
    let mut world = PhysicsWorld::new(Vector::new(0.0, -arena_config.gravity, 0.0));
    let collision_world = build_test_map_collision_runtime(&mut world, data)?;
    world.step(1.0 / 60.0);

    let profile = CollisionProfile::arena_default();
    let capsule_offset = profile.capsule_height * 0.5 + profile.capsule_radius;
    let center = bounds.center();
    let position = Isometry::translation(center.x, bounds.max.y + capsule_offset + 1.0, center.z);
    let motor = DualMotor::new(arena_config, rpg_config);
    let camera = PlayerCamera::new(TEST_MAP_EYE_HEIGHT);
    let controller = PlayerController::new(DirectInputAdapter, motor, profile, camera, position);
    let runtime = TestMapRuntime {
        key: data.key.clone(),
        world,
        collision_world,
        controller,
        position,
        prev_position: position,
        velocity: Vec3::zero(),
        prev_velocity: Vec3::zero(),
        grounded: false,
        ground_normal: None,
        capsule_offset,
        kcc_query_ms: 0.0,
    };
    Ok(runtime)
}

fn configure_test_map_camera(camera: &mut CameraState, bounds: &Bounds, tuning: CameraMotorTuning) {
    camera.eye_height = TEST_MAP_EYE_HEIGHT;
    apply_test_map_camera_tuning(camera, tuning);
    camera.velocity = Vec3::zero();
    camera.vertical_velocity = 0.0;
    camera.on_ground = false;

    let center = bounds.center();
    let extent = bounds.extent().length().max(1.0);
    let position = Vec3::new(
        center.x,
        center.y + camera.eye_height,
        center.z + extent * 0.25,
    );
    let dir = center.sub(position).normalize_or_zero();
    camera.pitch = dir.y.asin();
    camera.yaw = dir.x.atan2(-dir.z);
    camera.position = position;
}

fn switch_test_map_motor(
    runtime: &mut TestMapRuntime,
    camera: &mut CameraState,
    motor_kind: MotorKind,
) {
    if runtime.controller.motor().kind() == motor_kind {
        return;
    }
    runtime.controller.motor_mut().set_kind(motor_kind);
    let (profile, tuning) = match motor_kind {
        MotorKind::Arena => (
            CollisionProfile::arena_default(),
            camera_tuning_from_arena(runtime.controller.motor().arena_config()),
        ),
        MotorKind::Rpg => (
            CollisionProfile::rpg_default(),
            camera_tuning_from_rpg(runtime.controller.motor().rpg_config()),
        ),
    };
    let origin_y = runtime.position.translation.y - runtime.capsule_offset;
    runtime.controller.collision_mut().set_profile(profile);
    runtime.capsule_offset = profile.capsule_height * 0.5 + profile.capsule_radius;
    runtime.position.translation.y = origin_y + runtime.capsule_offset;
    runtime.prev_position = runtime.position;
    let state = runtime.controller.state_mut();
    state.position = runtime.position;
    state.velocity = Vector::new(runtime.velocity.x, runtime.velocity.y, runtime.velocity.z);
    state.grounded = runtime.grounded;
    state.ground_normal = runtime.ground_normal;
    apply_test_map_camera_tuning(camera, tuning);
}

fn sync_test_map_runtime_to_camera(runtime: &mut TestMapRuntime, camera: &CameraState) {
    let origin = Vec3::new(
        camera.position.x,
        camera.position.y - camera.eye_height,
        camera.position.z,
    );
    runtime.position = Isometry::translation(origin.x, origin.y + runtime.capsule_offset, origin.z);
    runtime.prev_position = runtime.position;
    runtime.velocity = Vec3::zero();
    runtime.prev_velocity = Vec3::zero();
    runtime.grounded = false;
    runtime.ground_normal = None;
    runtime
        .controller
        .camera_mut()
        .set_look(camera.yaw, camera.pitch);
    runtime.controller.motor_mut().reset_states();
    let state = runtime.controller.state_mut();
    state.position = runtime.position;
    state.velocity = Vector::zeros();
    state.grounded = false;
    state.ground_normal = None;
    runtime.kcc_query_ms = 0.0;
}

fn update_test_map_runtime(
    runtime: &mut TestMapRuntime,
    camera: &mut CameraState,
    input: &InputState,
    dt: f32,
) {
    runtime.world.step(dt);
    runtime.prev_position = runtime.position;
    runtime.prev_velocity = runtime.velocity;
    let move_x = bool_to_axis(input.right, input.left);
    let move_y = bool_to_axis(input.forward, input.back);
    let raw_input = RawInput {
        move_x,
        move_y,
        jump: input.jump_active(),
        look_delta: [0.0, 0.0],
    };
    runtime
        .controller
        .camera_mut()
        .set_look(camera.yaw, camera.pitch);
    let kcc_start = Instant::now();
    let frame = runtime.controller.tick(&runtime.world, raw_input, dt);
    runtime.kcc_query_ms = update_kcc_query_ms(runtime.kcc_query_ms, kcc_start.elapsed());
    runtime.position = frame.kinematics.position;
    runtime.grounded = frame.kinematics.grounded;
    runtime.ground_normal = frame.kinematics.ground_normal;

    let mut next_velocity = frame.kinematics.velocity;
    if frame.collision.hit_ceiling && next_velocity.y > 0.0 {
        next_velocity.y = 0.0;
    }
    if frame.kinematics.grounded && next_velocity.y < 0.0 {
        let allow_downhill = frame
            .kinematics
            .ground_normal
            .map(|normal| normal.y < 0.99)
            .unwrap_or(false);
        if !allow_downhill {
            next_velocity.y = 0.0;
        }
    }
    runtime.velocity = Vec3::new(next_velocity.x, next_velocity.y, next_velocity.z);
    let state = runtime.controller.state_mut();
    state.velocity = next_velocity;

    camera.position = Vec3::new(frame.camera.eye.x, frame.camera.eye.y, frame.camera.eye.z);
    camera.yaw = frame.camera.yaw;
    camera.pitch = frame.camera.pitch;
    camera.velocity = Vec3::new(runtime.velocity.x, runtime.velocity.y, runtime.velocity.z);
    camera.vertical_velocity = runtime.velocity.y;
    camera.on_ground = runtime.grounded;
}

fn update_kcc_query_ms(previous: f32, elapsed: Duration) -> f32 {
    let ms = elapsed.as_secs_f32() * 1000.0;
    if previous <= 0.0 {
        ms
    } else {
        previous + (ms - previous) * KCC_QUERY_SMOOTHING
    }
}

fn apply_test_map_interpolation(runtime: &TestMapRuntime, camera: &mut CameraState, alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let prev = runtime.prev_position.translation;
    let curr = runtime.position.translation;
    let interp_x = prev.x + (curr.x - prev.x) * alpha;
    let interp_y = prev.y + (curr.y - prev.y) * alpha;
    let interp_z = prev.z + (curr.z - prev.z) * alpha;
    let origin_y = interp_y - runtime.capsule_offset;
    camera.position = Vec3::new(interp_x, origin_y + camera.eye_height, interp_z);
    let prev_vel = runtime.prev_velocity;
    let curr_vel = runtime.velocity;
    let vel = Vec3::new(
        prev_vel.x + (curr_vel.x - prev_vel.x) * alpha,
        prev_vel.y + (curr_vel.y - prev_vel.y) * alpha,
        prev_vel.z + (curr_vel.z - prev_vel.z) * alpha,
    );
    camera.velocity = vel;
    camera.vertical_velocity = vel.y;
    camera.on_ground = runtime.grounded;
}

fn add_test_map_box(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    bounds: &mut Bounds,
    center: Vec3,
    size: [f32; 3],
    yaw_deg: Option<f32>,
    rot_euler_deg: Option<[f32; 3]>,
) -> Result<(), ExitError> {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let local = [
        Vec3::new(-hx, -hy, -hz),
        Vec3::new(hx, -hy, -hz),
        Vec3::new(hx, hy, -hz),
        Vec3::new(-hx, hy, -hz),
        Vec3::new(-hx, -hy, hz),
        Vec3::new(hx, -hy, hz),
        Vec3::new(hx, hy, hz),
        Vec3::new(-hx, hy, hz),
    ];
    let mut points = [Vec3::zero(); 8];
    for (index, value) in local.iter().enumerate() {
        points[index] = transform_test_map_point(*value, center, yaw_deg, rot_euler_deg);
    }

    push_quad(
        vertices, indices, bounds, points[4], points[5], points[6], points[7],
    )?;
    push_quad(
        vertices, indices, bounds, points[1], points[0], points[3], points[2],
    )?;
    push_quad(
        vertices, indices, bounds, points[0], points[4], points[7], points[3],
    )?;
    push_quad(
        vertices, indices, bounds, points[5], points[1], points[2], points[6],
    )?;
    push_quad(
        vertices, indices, bounds, points[3], points[7], points[6], points[2],
    )?;
    push_quad(
        vertices, indices, bounds, points[0], points[1], points[5], points[4],
    )?;
    Ok(())
}

fn add_test_map_ramp(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    bounds: &mut Bounds,
    center: Vec3,
    size: [f32; 3],
    yaw_deg: Option<f32>,
    rot_euler_deg: Option<[f32; 3]>,
) -> Result<(), ExitError> {
    let half_length = size[0] * 0.5;
    let half_height = size[1] * 0.5;
    let half_width = size[2] * 0.5;
    let local = [
        Vec3::new(-half_length, -half_height, -half_width),
        Vec3::new(half_length, -half_height, -half_width),
        Vec3::new(half_length, half_height, -half_width),
        Vec3::new(-half_length, -half_height, half_width),
        Vec3::new(half_length, -half_height, half_width),
        Vec3::new(half_length, half_height, half_width),
    ];
    let mut points = [Vec3::zero(); 6];
    for (index, value) in local.iter().enumerate() {
        points[index] = transform_test_map_point(*value, center, yaw_deg, rot_euler_deg);
    }
    let faces = [
        [0, 1, 2],
        [3, 5, 4],
        [0, 3, 4],
        [0, 4, 1],
        [0, 2, 5],
        [0, 5, 3],
        [1, 4, 5],
        [1, 5, 2],
    ];
    for face in faces {
        push_triangle(
            vertices,
            indices,
            bounds,
            points[face[0]],
            points[face[1]],
            points[face[2]],
        )?;
    }
    Ok(())
}

fn add_test_map_cylinder(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    bounds: &mut Bounds,
    center: Vec3,
    size: [f32; 3],
    yaw_deg: Option<f32>,
    rot_euler_deg: Option<[f32; 3]>,
) -> Result<(), ExitError> {
    let radius = size[0].max(size[2]) * 0.5;
    let half_height = size[1] * 0.5;
    let top_center = transform_test_map_point(
        Vec3::new(0.0, half_height, 0.0),
        center,
        yaw_deg,
        rot_euler_deg,
    );
    let bottom_center = transform_test_map_point(
        Vec3::new(0.0, -half_height, 0.0),
        center,
        yaw_deg,
        rot_euler_deg,
    );

    for segment in 0..TEST_MAP_CYLINDER_SEGMENTS {
        let t0 = segment as f32 / TEST_MAP_CYLINDER_SEGMENTS as f32;
        let t1 = (segment + 1) as f32 / TEST_MAP_CYLINDER_SEGMENTS as f32;
        let a0 = t0 * TAU;
        let a1 = t1 * TAU;
        let (sin0, cos0) = a0.sin_cos();
        let (sin1, cos1) = a1.sin_cos();
        let local_bottom0 = Vec3::new(radius * cos0, -half_height, radius * sin0);
        let local_bottom1 = Vec3::new(radius * cos1, -half_height, radius * sin1);
        let local_top0 = Vec3::new(radius * cos0, half_height, radius * sin0);
        let local_top1 = Vec3::new(radius * cos1, half_height, radius * sin1);

        let bottom0 = transform_test_map_point(local_bottom0, center, yaw_deg, rot_euler_deg);
        let bottom1 = transform_test_map_point(local_bottom1, center, yaw_deg, rot_euler_deg);
        let top0 = transform_test_map_point(local_top0, center, yaw_deg, rot_euler_deg);
        let top1 = transform_test_map_point(local_top1, center, yaw_deg, rot_euler_deg);

        push_triangle(vertices, indices, bounds, bottom0, bottom1, top1)?;
        push_triangle(vertices, indices, bounds, bottom0, top1, top0)?;

        push_triangle(vertices, indices, bounds, top_center, top0, top1)?;
        push_triangle(vertices, indices, bounds, bottom_center, bottom1, bottom0)?;
    }
    Ok(())
}

fn transform_test_map_point(
    local: Vec3,
    center: Vec3,
    yaw_deg: Option<f32>,
    rot_euler_deg: Option<[f32; 3]>,
) -> Vec3 {
    let rotated = apply_test_map_rotation(local, yaw_deg, rot_euler_deg);
    center.add(rotated)
}

fn apply_test_map_rotation(
    value: Vec3,
    yaw_deg: Option<f32>,
    rot_euler_deg: Option<[f32; 3]>,
) -> Vec3 {
    if let Some(euler) = rot_euler_deg {
        let pitch = euler[0].to_radians();
        let yaw = euler[1].to_radians();
        let roll = euler[2].to_radians();
        let value = rotate_x_axis(value, pitch);
        let value = rotate_y_axis(value, yaw);
        return rotate_z_axis(value, roll);
    }
    if let Some(yaw) = yaw_deg {
        return rotate_y_axis(value, yaw.to_radians());
    }
    value
}

fn rotate_x_axis(value: Vec3, angle: f32) -> Vec3 {
    let (sin, cos) = angle.sin_cos();
    Vec3::new(
        value.x,
        value.y * cos - value.z * sin,
        value.y * sin + value.z * cos,
    )
}

fn rotate_y_axis(value: Vec3, angle: f32) -> Vec3 {
    let (sin, cos) = angle.sin_cos();
    Vec3::new(
        value.x * cos + value.z * sin,
        value.y,
        -value.x * sin + value.z * cos,
    )
}

fn rotate_z_axis(value: Vec3, angle: f32) -> Vec3 {
    let (sin, cos) = angle.sin_cos();
    Vec3::new(
        value.x * cos - value.y * sin,
        value.x * sin + value.y * cos,
        value.z,
    )
}

fn push_quad(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    bounds: &mut Bounds,
    a: Vec3,
    b: Vec3,
    c: Vec3,
    d: Vec3,
) -> Result<(), ExitError> {
    push_triangle(vertices, indices, bounds, a, b, c)?;
    push_triangle(vertices, indices, bounds, a, c, d)?;
    Ok(())
}

fn push_triangle(
    vertices: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    bounds: &mut Bounds,
    a: Vec3,
    b: Vec3,
    c: Vec3,
) -> Result<(), ExitError> {
    let normal = b.sub(a).cross(c.sub(a));
    if normal.length() <= 0.0 {
        return Ok(());
    }
    let normal = normal.normalize_or_zero();
    let color = normal.abs().scale(0.8).add(Vec3::new(0.2, 0.2, 0.2));
    let base = u32::try_from(vertices.len())
        .map_err(|_| ExitError::new(EXIT_SCENE, "vertex count overflow building test map mesh"))?;
    vertices.push(MeshVertex {
        position: a.to_array(),
        color: color.to_array(),
    });
    vertices.push(MeshVertex {
        position: b.to_array(),
        color: color.to_array(),
    });
    vertices.push(MeshVertex {
        position: c.to_array(),
        color: color.to_array(),
    });
    indices.extend_from_slice(&[base, base + 1, base + 2]);
    bounds.include(a);
    bounds.include(b);
    bounds.include(c);
    Ok(())
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

fn quake_vpath(path: &str) -> String {
    let normalized = normalize_asset_name(path);
    if normalized.starts_with("raw/") {
        normalized
    } else {
        format!("{}/{}", QUAKE_VROOT, normalized)
    }
}

fn build_mounts(args: &CliArgs, path_policy: &PathPolicy) -> Result<Option<Arc<Vfs>>, ExitError> {
    if args.quake_dir.is_none() && args.mounts.is_empty() && args.mount_manifests.is_empty() {
        return Ok(None);
    }
    let mut vfs = Vfs::new();
    for spec in &args.mounts {
        add_mount_spec(&mut vfs, spec)?;
    }
    for manifest in &args.mount_manifests {
        load_manifest_mounts(&mut vfs, path_policy, manifest)?;
    }
    if let Some(quake_dir) = args.quake_dir.as_ref() {
        mount_quake_dir(&mut vfs, quake_dir)?;
    }
    Ok(Some(Arc::new(vfs)))
}

fn add_mount_spec(vfs: &mut Vfs, spec: &MountSpec) -> Result<(), ExitError> {
    let result = match spec.kind {
        MountKind::Dir => vfs.add_dir_mount(&spec.mount_point, &spec.path),
        MountKind::Pak => vfs.add_pak_mount(&spec.mount_point, &spec.path),
        MountKind::Pk3 => vfs.add_pk3_mount(&spec.mount_point, &spec.path),
    };
    result.map_err(|err| {
        let code = match spec.kind {
            MountKind::Dir => EXIT_USAGE,
            MountKind::Pak | MountKind::Pk3 => EXIT_PAK,
        };
        ExitError::new(
            code,
            format!(
                "mount {} {} from {} failed: {}",
                spec.kind,
                spec.mount_point,
                spec.path.display(),
                err
            ),
        )
    })
}

fn load_manifest_mounts(
    vfs: &mut Vfs,
    path_policy: &PathPolicy,
    manifest: &str,
) -> Result<(), ExitError> {
    let resolved = path_policy
        .resolve_config_file(ConfigKind::Mounts, manifest)
        .map_err(|err| ExitError::new(EXIT_USAGE, err))?;
    println!("{}", resolved.describe());
    let entries =
        load_mount_manifest(&resolved.path).map_err(|err| ExitError::new(EXIT_USAGE, err))?;
    for entry in &entries {
        add_manifest_entry(vfs, entry)?;
    }
    Ok(())
}

fn add_manifest_entry(vfs: &mut Vfs, entry: &MountManifestEntry) -> Result<(), ExitError> {
    let spec = MountSpec {
        kind: entry.kind,
        mount_point: entry.mount_point.clone(),
        path: entry.path.clone(),
    };
    add_mount_spec(vfs, &spec).map_err(|err| {
        ExitError::new(
            err.code,
            format!("mount manifest line {}: {}", entry.line, err.message),
        )
    })
}

fn mount_quake_dir(vfs: &mut Vfs, quake_dir: &Path) -> Result<(), ExitError> {
    if !quake_dir.is_dir() {
        return Err(ExitError::new(
            EXIT_QUAKE_DIR,
            format!("quake dir not found: {}", quake_dir.display()),
        ));
    }
    let base_dir = {
        let id1 = quake_dir.join("id1");
        if id1.is_dir() {
            id1
        } else {
            quake_dir.to_path_buf()
        }
    };

    vfs.add_dir_mount(QUAKE_VROOT, &base_dir).map_err(|err| {
        ExitError::new(
            EXIT_QUAKE_DIR,
            format!("quake dir mount failed ({}): {}", base_dir.display(), err),
        )
    })?;

    let mut pak_paths = Vec::new();
    for index in 0..10 {
        let path = base_dir.join(format!("pak{}.pak", index));
        if path.is_file() {
            pak_paths.push((index, path));
        }
    }
    pak_paths.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in pak_paths {
        vfs.add_pak_mount(QUAKE_VROOT, &path).map_err(|err| {
            ExitError::new(
                EXIT_PAK,
                format!("quake pak mount failed ({}): {}", path.display(), err),
            )
        })?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn open_console(
    console: &mut ConsoleState,
    ui_state: &mut UiState,
    window: &Window,
    input: &mut InputState,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    asset_manager: &AssetManager,
) {
    let now = Instant::now();
    console.caret_epoch = now;
    console.open(now);
    console.resume_mouse_look = *mouse_look;
    console.buffer.clear();
    push_console_welcome(console, asset_manager);
    ui_state.console_open = console.is_blocking();
    *input = InputState::default();
    *mouse_look = false;
    *mouse_grabbed = set_cursor_mode(window, *mouse_look);
    window.set_ime_allowed(false);
    println!("console: open");
}

#[allow(clippy::too_many_arguments)]
fn close_console(
    console: &mut ConsoleState,
    ui_state: &mut UiState,
    window: &Window,
    settings: &Settings,
    console_fullscreen_override: &mut bool,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    scene_active: bool,
    allow_recapture: bool,
    restore_fullscreen: bool,
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
    restore_console_fullscreen_override(
        window,
        settings,
        console_fullscreen_override,
        restore_fullscreen,
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_non_video_key_input(
    code: KeyCode,
    pressed: bool,
    is_repeat: bool,
    input_router: &InputRouter,
    console: &mut ConsoleState,
    perf: &mut PerfState,
    cvars: &mut CvarRegistry,
    core_cvars: &CoreCvars,
    path_policy: &PathPolicy,
    asset_manager: &AssetManager,
    upload_queue: &UploadQueue,
    quake_vfs: Option<&Arc<Vfs>>,
    quake_dir: Option<&PathBuf>,
    console_async: &ConsoleAsyncSender,
    log_filter_state: &Arc<Mutex<LogFilterState>>,
    capture_requests: &mut VecDeque<CaptureRequest>,
    ui_state: &mut UiState,
    window: &Window,
    settings: &mut Settings,
    settings_flags: &mut SettingsChangeFlags,
    test_map_reload_requests: &mut VecDeque<AssetKey>,
    active_test_map: Option<AssetKey>,
    test_map_runtime: Option<&mut TestMapRuntime>,
    camera: &mut CameraState,
    input_trace_record: &mut Option<InputTraceRecorder>,
    input_trace_playback: &mut Option<InputTracePlayback>,
    console_fullscreen_override: &mut bool,
    input: &mut InputState,
    mouse_look: &mut bool,
    mouse_grabbed: &mut bool,
    was_mouse_look: &mut bool,
    scene_active: bool,
    fly_mode: &mut bool,
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
                    settings,
                    console_fullscreen_override,
                    mouse_look,
                    mouse_grabbed,
                    scene_active,
                    true,
                    true,
                );
            } else {
                open_console(
                    console,
                    ui_state,
                    window,
                    input,
                    mouse_look,
                    mouse_grabbed,
                    asset_manager,
                );
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
                settings,
                console_fullscreen_override,
                mouse_look,
                mouse_grabbed,
                scene_active,
                true,
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
                        console.clear_history_nav();
                        if !line.is_empty() {
                            console.push_history(&line);
                            println!("> {}", line);
                            console.push_line(format!("> {}", line));
                            dispatch_console_line(
                                &line,
                                console,
                                perf,
                                cvars,
                                core_cvars,
                                script.as_mut(),
                                path_policy,
                                asset_manager,
                                upload_queue.clone(),
                                quake_vfs.cloned(),
                                quake_dir.cloned(),
                                console_async.clone(),
                                log_filter_state,
                                capture_requests,
                                settings,
                                settings_flags,
                                test_map_reload_requests,
                                active_test_map.clone(),
                                test_map_runtime,
                                Some(camera),
                                input_trace_record,
                                input_trace_playback,
                            );
                        }
                    }
                    KeyCode::Backspace => {
                        console.buffer.pop();
                    }
                    KeyCode::ArrowUp => {
                        console.history_previous();
                    }
                    KeyCode::ArrowDown => {
                        console.history_next();
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
                    KeyCode::Tab => {
                        apply_console_completion(console, cvars);
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
                KeyCode::Space => input.jump_keyboard = pressed,
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

fn set_borderless_fullscreen(window: &Window) {
    let monitor = window
        .current_monitor()
        .or_else(|| window.primary_monitor());
    window.set_fullscreen(Some(Fullscreen::Borderless(monitor)));
}

fn enable_console_fullscreen_override(
    window: &Window,
    settings: &Settings,
    console_fullscreen_override: &mut bool,
) {
    if settings.window_mode == WindowMode::Fullscreen && !*console_fullscreen_override {
        set_borderless_fullscreen(window);
        *console_fullscreen_override = true;
    }
}

fn restore_console_fullscreen_override(
    window: &Window,
    settings: &Settings,
    console_fullscreen_override: &mut bool,
    restore_fullscreen: bool,
) {
    if !*console_fullscreen_override {
        return;
    }
    if settings.window_mode != WindowMode::Fullscreen {
        *console_fullscreen_override = false;
        return;
    }
    if restore_fullscreen {
        apply_window_settings(window, settings);
        *console_fullscreen_override = false;
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
