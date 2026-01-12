#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use egui::{Color32, Context, TextEdit};
use egui_wgpu::{Renderer as EguiRenderer, ScreenDescriptor};
use egui_winit::State as EguiState;
use platform_winit::{create_window, ControlFlow, Event, PhysicalPosition, Window, WindowEvent};
use render_wgpu::RenderError;
use winit::window::Icon;
use winit::window::{CursorIcon, ResizeDirection};

use config::{DebugPresetConfig, RunnerConfig};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const WINDOW_TITLE: &str = concat!("Pallet Runner GUI ", env!("CARGO_PKG_VERSION"));
const WINDOW_WIDTH: u32 = 960;
const WINDOW_HEIGHT: u32 = 640;
const TITLE_BAR_HEIGHT: f32 = 28.0;
const RESIZE_BORDER: f32 = 6.0;

const STATUS_OK: Color32 = Color32::from_rgb(70, 200, 120);
const STATUS_WARN: Color32 = Color32::from_rgb(220, 190, 80);
const STATUS_ERR: Color32 = Color32::from_rgb(230, 90, 90);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Ok,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
struct StatusLine {
    kind: StatusKind,
    message: String,
}

impl StatusLine {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Ok,
            message: message.into(),
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Warning,
            message: message.into(),
        }
    }

    fn err(message: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Error,
            message: message.into(),
        }
    }

    fn color(&self) -> Color32 {
        match self.kind {
            StatusKind::Ok => STATUS_OK,
            StatusKind::Warning => STATUS_WARN,
            StatusKind::Error => STATUS_ERR,
        }
    }
}

#[derive(Clone, Debug)]
struct BuildManifestUi {
    version: Option<String>,
    tool_version: Option<String>,
    profile: Option<String>,
    build_id: Option<String>,
    platform: Option<String>,
    timestamp: Option<String>,
    mount_count: Option<usize>,
    input_count: Option<usize>,
    output_count: Option<usize>,
    mounts: Vec<ManifestMountUi>,
    stages: Vec<ManifestStageUi>,
    quake_index: Option<ManifestQuakeIndexUi>,
}

#[derive(Clone, Debug)]
struct ManifestMountUi {
    namespace: String,
    order: String,
    layer: String,
    kind: String,
    name: String,
    source: String,
}

#[derive(Clone, Debug)]
struct ManifestStageUi {
    name: String,
    status: String,
    duration_ms: String,
}

#[derive(Clone, Debug)]
struct ManifestQuakeIndexUi {
    version: String,
    fingerprint: String,
    entry_count: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppTab {
    Pallet,
    Tools,
    Net,
    Checks,
}

#[derive(Clone, Copy, Debug)]
enum ChecksAction {
    Fmt,
    Clippy,
    Test,
}

#[derive(Clone, Copy, Debug)]
enum WindowAction {
    Minimize,
    MaximizeToggle,
    Close,
}

const LOG_MAX_LINES: usize = 500;

const SMOKE_MODES: [&str; 2] = ["no-assets", "quake"];

struct RunnerApp {
    config: RunnerConfig,
    active_tab: AppTab,
    pallet_process: ProcessLane,
    tools_process: ProcessLane,
    server_process: ProcessLane,
    client_process: ProcessLane,
    checks_process: ProcessLane,
    content_manifest: Option<BuildManifestUi>,
    content_manifest_error: Option<String>,
    tools_last_running: bool,
    asset_stats_lines: Vec<String>,
    asset_stats_log_len: usize,
    pending_window_action: Option<WindowAction>,
    repo_root_input: String,
    repo_root: Option<PathBuf>,
    repo_root_status: StatusLine,
    browse_notice: Option<String>,
    metadata_status: Option<StatusLine>,
    metadata_details: Option<String>,
    custom_maximized: bool,
    last_windowed_size: winit::dpi::PhysicalSize<u32>,
    last_windowed_pos: PhysicalPosition<i32>,
}

impl RunnerApp {
    fn new() -> Self {
        let mut config = RunnerConfig::load();
        let repo_root_input = config.repo_root.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        });
        if config.repo_root.is_none() && !repo_root_input.trim().is_empty() {
            config.repo_root = Some(repo_root_input.clone());
        }
        config.ensure_debug_presets();
        let mut app = Self {
            config,
            active_tab: AppTab::Pallet,
            pallet_process: ProcessLane::new(LOG_MAX_LINES),
            tools_process: ProcessLane::new(LOG_MAX_LINES),
            server_process: ProcessLane::new(LOG_MAX_LINES),
            client_process: ProcessLane::new(LOG_MAX_LINES),
            checks_process: ProcessLane::new(LOG_MAX_LINES),
            content_manifest: None,
            content_manifest_error: None,
            tools_last_running: false,
            asset_stats_lines: Vec::new(),
            asset_stats_log_len: 0,
            pending_window_action: None,
            repo_root_input,
            repo_root: None,
            repo_root_status: StatusLine::warn("Repo root not validated."),
            browse_notice: None,
            metadata_status: None,
            metadata_details: None,
            custom_maximized: false,
            last_windowed_size: winit::dpi::PhysicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT),
            last_windowed_pos: PhysicalPosition::new(0, 0),
        };
        app.validate_repo_root();
        app
    }

    fn set_repo_root_path(&mut self, path: PathBuf) {
        self.repo_root_input = path.display().to_string();
        self.validate_repo_root();
    }

    fn validate_repo_root(&mut self) {
        self.repo_root = None;
        self.metadata_status = None;
        self.metadata_details = None;
        self.content_manifest = None;
        self.content_manifest_error = None;
        let trimmed = self.repo_root_input.trim();
        self.config.repo_root = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        if trimmed.is_empty() {
            self.repo_root_status = StatusLine::err("Enter a repo root directory.");
            return;
        }
        let path = PathBuf::from(trimmed);
        if !path.exists() {
            self.repo_root_status = StatusLine::err("Repo root path does not exist.");
            return;
        }
        if !path.is_dir() {
            self.repo_root_status = StatusLine::err("Repo root path is not a directory.");
            return;
        }
        let cargo_toml = path.join("Cargo.toml");
        if !cargo_toml.is_file() {
            self.repo_root_status = StatusLine::err("Cargo.toml not found in repo root.");
            return;
        }
        let canonical = path.canonicalize().unwrap_or(path);
        self.repo_root = Some(canonical);
        self.repo_root_status = StatusLine::ok("Repo root validated.");
    }

    fn command_in_repo_root(&self, program: &str) -> Option<Command> {
        let repo_root = self.repo_root.as_ref()?;
        let mut command = Command::new(program);
        command.current_dir(repo_root);
        Some(command)
    }

    fn check_cargo_metadata(&mut self) {
        self.metadata_details = None;
        let mut command = match self.command_in_repo_root("cargo") {
            Some(command) => command,
            None => {
                self.metadata_status = Some(StatusLine::err(
                    "Select a valid repo root before running metadata.",
                ));
                return;
            }
        };
        command
            .arg("metadata")
            .arg("--no-deps")
            .arg("--format-version")
            .arg("1");
        match command.output() {
            Ok(output) => {
                if output.status.success() {
                    self.metadata_status = Some(StatusLine::ok("cargo metadata succeeded."));
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let details = format_output_excerpt(&stdout, &stderr);
                    self.metadata_status = Some(StatusLine::err("cargo metadata failed."));
                    if !details.is_empty() {
                        self.metadata_details = Some(details);
                    }
                }
            }
            Err(err) => {
                self.metadata_status = Some(StatusLine::err("cargo metadata failed to run."));
                self.metadata_details = Some(err.to_string());
            }
        }
    }

    fn build_tools_smoke_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let mode = if SMOKE_MODES
            .iter()
            .any(|value| value == &self.config.smoke_mode.as_str())
        {
            self.config.smoke_mode.as_str()
        } else {
            "no-assets"
        };
        command
            .arg("run")
            .arg("-p")
            .arg("tools")
            .arg("--")
            .arg("smoke")
            .arg("--mode")
            .arg(mode);
        if let Some(ticks) = self.config.smoke_ticks {
            command.arg("--ticks").arg(ticks.to_string());
        }
        if mode == "quake" {
            let quake_dir = self.config.quake_dir.trim();
            if quake_dir.is_empty() {
                return Err("--quake-dir is required for quake smoke.".to_string());
            }
            let map = self.config.map.trim();
            if map.is_empty() {
                return Err("--map is required for quake smoke.".to_string());
            }
            command.arg("--quake-dir").arg(quake_dir);
            command.arg("--map").arg(map);
            if self.config.smoke_headless {
                command.arg("--headless");
            }
        } else if self.config.smoke_headless {
            command.arg("--headless");
        }
        Ok(command)
    }

    fn build_tools_pak_list_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let quake_dir = self.config.quake_dir.trim();
        if quake_dir.is_empty() {
            return Err("--quake-dir is required for pak list.".to_string());
        }
        command
            .arg("run")
            .arg("-p")
            .arg("tools")
            .arg("--")
            .arg("pak")
            .arg("list")
            .arg("--quake-dir")
            .arg(quake_dir);
        Ok(command)
    }

    fn build_tools_pak_extract_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let quake_dir = self.config.quake_dir.trim();
        if quake_dir.is_empty() {
            return Err("--quake-dir is required for pak extract.".to_string());
        }
        let out_dir = self
            .config
            .pak_out_dir
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        if out_dir.is_empty() {
            return Err("--out is required for pak extract.".to_string());
        }
        command
            .arg("run")
            .arg("-p")
            .arg("tools")
            .arg("--")
            .arg("pak")
            .arg("extract")
            .arg("--quake-dir")
            .arg(quake_dir)
            .arg("--out")
            .arg(out_dir);
        Ok(command)
    }

    fn build_tools_vfs_stat_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let vpath = self.config.vfs_vpath.trim();
        if vpath.is_empty() {
            return Err("VFS vpath is required.".to_string());
        }
        command.arg("run").arg("-p").arg("tools").arg("--");
        command.arg("vfs").arg("stat");
        if self.config.vfs_use_quake_dir {
            let quake_dir = self.config.quake_dir.trim();
            if quake_dir.is_empty() {
                return Err("--quake-dir is required when enabled.".to_string());
            }
            command.arg("--quake-dir").arg(quake_dir);
        }
        if let Some(manifest) = self.config.vfs_mount_manifest.as_ref() {
            let manifest = manifest.trim();
            if !manifest.is_empty() {
                command.arg("--mount-manifest").arg(manifest);
            }
        }
        push_mount_pair(
            &mut command,
            "--mount-dir",
            self.config.vfs_mount_dir_vroot.as_deref(),
            self.config.vfs_mount_dir_path.as_deref(),
        )?;
        push_mount_pair(
            &mut command,
            "--mount-pak",
            self.config.vfs_mount_pak_vroot.as_deref(),
            self.config.vfs_mount_pak_path.as_deref(),
        )?;
        push_mount_pair(
            &mut command,
            "--mount-pk3",
            self.config.vfs_mount_pk3_vroot.as_deref(),
            self.config.vfs_mount_pk3_path.as_deref(),
        )?;
        command.arg(vpath);
        Ok(command)
    }

    fn append_content_common_args(&self, command: &mut Command) {
        let quake_dir = self.config.quake_dir.trim();
        if !quake_dir.is_empty() {
            command.arg("--quake-dir").arg(quake_dir);
        }
        if let Some(manifest) = self.config.mount_manifest.as_ref() {
            let manifest = manifest.trim();
            if !manifest.is_empty() {
                command.arg("--mount-manifest").arg(manifest);
            }
        }
    }

    fn build_tools_content_lint_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let files = self.collect_content_lint_files()?;
        command.arg("run").arg("-p").arg("tools").arg("--");
        command.arg("content");
        self.append_content_common_args(&mut command);
        command.arg("lint-ids");
        for file in files {
            command.arg("--file").arg(file);
        }
        Ok(command)
    }

    fn build_tools_content_validate_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        command.arg("run").arg("-p").arg("tools").arg("--");
        command.arg("content");
        self.append_content_common_args(&mut command);
        command.arg("validate");
        Ok(command)
    }

    fn build_tools_content_build_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        command.arg("run").arg("-p").arg("tools").arg("--");
        command.arg("content");
        self.append_content_common_args(&mut command);
        command.arg("build");
        Ok(command)
    }

    fn build_tools_content_clean_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        command.arg("run").arg("-p").arg("tools").arg("--");
        command.arg("content");
        self.append_content_common_args(&mut command);
        command.arg("clean");
        Ok(command)
    }

    fn build_tools_content_doctor_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        command.arg("run").arg("-p").arg("tools").arg("--");
        command.arg("content");
        self.append_content_common_args(&mut command);
        command.arg("doctor");
        Ok(command)
    }

    fn build_tools_quake_index_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let quake_dir = self.config.quake_dir.trim();
        if quake_dir.is_empty() {
            return Err("--quake-dir is required for quake index.".to_string());
        }
        command
            .arg("run")
            .arg("-p")
            .arg("tools")
            .arg("--")
            .arg("quake")
            .arg("index")
            .arg("--quake-dir")
            .arg(quake_dir);
        Ok(command)
    }

    fn run_tools_smoke(&mut self) {
        match self.build_tools_smoke_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_pak_list(&mut self) {
        match self.build_tools_pak_list_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_pak_extract(&mut self) {
        match self.build_tools_pak_extract_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_vfs_stat(&mut self) {
        match self.build_tools_vfs_stat_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_content_lint(&mut self) {
        match self.build_tools_content_lint_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_content_validate(&mut self) {
        match self.build_tools_content_validate_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_content_build(&mut self) {
        match self.build_tools_content_build_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_content_clean(&mut self) {
        match self.build_tools_content_clean_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_content_doctor(&mut self) {
        match self.build_tools_content_doctor_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn run_tools_quake_index(&mut self) {
        match self.build_tools_quake_index_command() {
            Ok(command) => {
                if let Err(err) = self.tools_process.start(command) {
                    self.tools_process.push_system(err);
                }
            }
            Err(err) => {
                self.tools_process.push_system(err);
            }
        }
    }

    fn collect_content_lint_files(&self) -> Result<Vec<PathBuf>, String> {
        let repo_root = self
            .repo_root
            .as_ref()
            .ok_or_else(|| "Repo root is required to run tools.".to_string())?;
        let content_root = repo_root.join("content");
        if !content_root.is_dir() {
            return Err("content root directory not found.".to_string());
        }
        let mut files = Vec::new();
        collect_level_manifest_paths(&content_root, &mut files).map_err(|err| err.to_string())?;
        if files.is_empty() {
            return Err("No level manifests found under content.".to_string());
        }
        files.sort();
        Ok(files)
    }

    fn build_manifest_path(&self) -> Option<PathBuf> {
        self.repo_root.as_ref().map(|root| {
            root.join("content")
                .join("build")
                .join("build_manifest.txt")
        })
    }

    fn refresh_build_manifest_cache(&mut self) {
        let path = match self.build_manifest_path() {
            Some(path) => path,
            None => {
                self.content_manifest = None;
                self.content_manifest_error = Some("Repo root is required.".to_string());
                return;
            }
        };
        match read_build_manifest_ui(&path) {
            Ok(summary) => {
                self.content_manifest = Some(summary);
                self.content_manifest_error = None;
            }
            Err(err) => {
                self.content_manifest = None;
                self.content_manifest_error = Some(err);
            }
        }
    }

    fn update_asset_stats_cache(&mut self) {
        let log_len = self.pallet_process.logs.len();
        if log_len == self.asset_stats_log_len {
            return;
        }
        self.asset_stats_log_len = log_len;
        self.asset_stats_lines = extract_dev_asset_stats(&self.pallet_process.logs);
    }

    fn build_server_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run the server.".to_string())?;
        let bind = self.config.server_bind.trim();
        if bind.is_empty() {
            return Err("--bind is required.".to_string());
        }
        command
            .arg("run")
            .arg("-p")
            .arg("server")
            .arg("--bin")
            .arg("dedicated")
            .arg("--")
            .arg("--bind")
            .arg(bind)
            .arg("--tick-ms")
            .arg(self.config.server_tick_ms.max(1).to_string())
            .arg("--snapshot-stride")
            .arg(self.config.server_snapshot_stride.max(1).to_string());
        if let Some(max_ticks) = self.config.server_max_ticks {
            command.arg("--max-ticks").arg(max_ticks.max(1).to_string());
        }
        Ok(command)
    }

    fn build_client_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run the client.".to_string())?;
        let bind = self.config.client_bind.trim();
        if bind.is_empty() {
            return Err("--bind is required.".to_string());
        }
        let server = self.config.client_server.trim();
        if server.is_empty() {
            return Err("--server is required.".to_string());
        }
        command
            .arg("run")
            .arg("-p")
            .arg("client")
            .arg("--bin")
            .arg("headless")
            .arg("--")
            .arg("--bind")
            .arg(bind)
            .arg("--server")
            .arg(server)
            .arg("--tick-ms")
            .arg(self.config.client_tick_ms.max(1).to_string())
            .arg("--ticks")
            .arg(self.config.client_ticks.max(1).to_string())
            .arg("--client-id")
            .arg(self.config.client_id.max(1).to_string());
        if self.config.client_move_enabled {
            command
                .arg("--move-x")
                .arg(format_float(self.config.client_move_x))
                .arg("--move-y")
                .arg(format_float(self.config.client_move_y))
                .arg("--yaw-step")
                .arg(format_float(self.config.client_yaw_step));
        }
        Ok(command)
    }

    fn run_server(&mut self) {
        match self.build_server_command() {
            Ok(command) => {
                if let Err(err) = self.server_process.start(command) {
                    self.server_process.push_system(err);
                }
            }
            Err(err) => {
                self.server_process.push_system(err);
            }
        }
    }

    fn run_client(&mut self) {
        match self.build_client_command() {
            Ok(command) => {
                if let Err(err) = self.client_process.start(command) {
                    self.client_process.push_system(err);
                }
            }
            Err(err) => {
                self.client_process.push_system(err);
            }
        }
    }

    fn run_checks_fmt(&mut self) {
        self.run_checks(ChecksAction::Fmt);
    }

    fn run_checks_clippy(&mut self) {
        self.run_checks(ChecksAction::Clippy);
    }

    fn run_checks_test(&mut self) {
        self.run_checks(ChecksAction::Test);
    }

    fn run_checks(&mut self, action: ChecksAction) {
        match self.build_checks_command(action) {
            Ok(command) => {
                if let Err(err) = self.checks_process.start(command) {
                    self.checks_process.push_system(err);
                }
            }
            Err(err) => {
                self.checks_process.push_system(err);
            }
        }
    }

    fn build_checks_command(&self, action: ChecksAction) -> Result<Command, String> {
        let repo_root = self
            .repo_root
            .as_ref()
            .ok_or_else(|| "Repo root is required to run checks.".to_string())?;
        if self.should_use_just(repo_root) {
            let mut command = Command::new("just");
            command.current_dir(repo_root);
            command.arg(match action {
                ChecksAction::Fmt => "fmt",
                ChecksAction::Clippy => "clippy",
                ChecksAction::Test => "test",
            });
            return Ok(command);
        }
        let mut command = Command::new("cargo");
        command.current_dir(repo_root);
        match action {
            ChecksAction::Fmt => {
                command.arg("fmt");
            }
            ChecksAction::Clippy => {
                command
                    .arg("clippy")
                    .arg("--workspace")
                    .arg("--all-targets")
                    .arg("--")
                    .arg("-D")
                    .arg("warnings");
            }
            ChecksAction::Test => {
                command.arg("test").arg("--workspace");
            }
        }
        Ok(command)
    }

    fn should_use_just(&self, repo_root: &Path) -> bool {
        if !repo_root.join("justfile").is_file() && !repo_root.join("Justfile").is_file() {
            return false;
        }
        Command::new("just")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn pallet_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let quake_dir = self.config.quake_dir.trim();
        if !quake_dir.is_empty() {
            args.push("--quake-dir".to_string());
            args.push(quake_dir.to_string());
        }
        let map = self.config.map.trim();
        if !map.is_empty() {
            args.push("--map".to_string());
            args.push(map.to_string());
        }
        if self.config.playlist_enabled {
            if let Some(path) = self.config.playlist_path.as_ref() {
                let path = path.trim();
                if !path.is_empty() {
                    args.push("--playlist".to_string());
                    args.push(path.to_string());
                }
            }
        }
        if let Some(path) = self.config.show_image.as_ref() {
            let path = path.trim();
            if !path.is_empty() {
                args.push("--show-image".to_string());
                args.push(path.to_string());
            }
        }
        if let Some(path) = self.config.play_movie.as_ref() {
            let path = path.trim();
            if !path.is_empty() {
                args.push("--play-movie".to_string());
                args.push(path.to_string());
            }
        }
        if let Some(path) = self.config.script_path.as_ref() {
            let path = path.trim();
            if !path.is_empty() {
                args.push("--script".to_string());
                args.push(path.to_string());
            }
        }
        if let Some(path) = self.config.mount_manifest.as_ref() {
            let path = path.trim();
            if !path.is_empty() {
                args.push("--mount-manifest".to_string());
                args.push(path.to_string());
            }
        }
        push_mount_pair_args(
            &mut args,
            "--mount-dir",
            &self.config.pallet_mount_dir_vroot,
            &self.config.pallet_mount_dir_path,
        );
        push_mount_pair_args(
            &mut args,
            "--mount-pak",
            &self.config.pallet_mount_pak_vroot,
            &self.config.pallet_mount_pak_path,
        );
        push_mount_pair_args(
            &mut args,
            "--mount-pk3",
            &self.config.pallet_mount_pk3_vroot,
            &self.config.pallet_mount_pk3_path,
        );
        if self.config.input_script {
            args.push("--input-script".to_string());
        }
        if let Some(preset) =
            find_debug_preset(&self.config.debug_presets, &self.config.debug_preset)
        {
            for arg in &preset.extra_args {
                args.push(arg.to_string());
            }
        }
        args
    }

    fn pallet_env(&self) -> Vec<(String, String)> {
        let mut envs = BTreeMap::new();
        if self.config.video_debug {
            envs.insert("CRUSTQUAKE_VIDEO_DEBUG".to_string(), "1".to_string());
        }
        if let Some(preset) =
            find_debug_preset(&self.config.debug_presets, &self.config.debug_preset)
        {
            for (key, value) in &preset.env {
                envs.insert(key.to_string(), value.to_string());
            }
        }
        envs.into_iter().collect()
    }

    fn build_pallet_command(&self) -> Result<Command, String> {
        let mut command = self
            .command_in_repo_root("cargo")
            .ok_or_else(|| "Repo root is required to run pallet.".to_string())?;
        command.arg("run").arg("-p").arg("pallet");
        let args = self.pallet_args();
        if !args.is_empty() {
            command.arg("--");
            for arg in args {
                command.arg(arg);
            }
        }
        for (key, value) in self.pallet_env() {
            command.env(key, value);
        }
        Ok(command)
    }

    fn build_pallet_command_line(&self) -> Option<String> {
        let args = self.pallet_args();
        let envs = self.pallet_env();
        let mut line = String::new();
        for (key, value) in envs {
            line.push_str(&format!("$env:{}='{}'; ", key, value));
        }
        line.push_str("cargo run -p pallet");
        if !args.is_empty() {
            line.push_str(" --");
            for arg in args {
                line.push(' ');
                line.push_str(&quote_arg(&arg));
            }
        }
        Some(line)
    }

    fn run_pallet(&mut self) {
        match self.build_pallet_command() {
            Ok(command) => {
                if let Err(err) = self.pallet_process.start(command) {
                    self.pallet_process.push_system(err);
                }
            }
            Err(err) => {
                self.pallet_process.push_system(err);
            }
        }
    }

    fn pallet_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.repo_root.is_none() {
            warnings.push("Repo root is not valid; commands will fail.".to_string());
        }
        let quake_dir = self.config.quake_dir.trim();
        if quake_dir.is_empty() {
            warnings.push("Quake dir is required.".to_string());
        } else if !Path::new(quake_dir).exists() {
            warnings.push("Quake dir path does not exist.".to_string());
        }
        if self.config.playlist_enabled {
            match self
                .config
                .playlist_path
                .as_ref()
                .map(|path| path.trim())
                .filter(|path| !path.is_empty())
            {
                Some(path) => {
                    if !Path::new(path).is_file() {
                        warnings.push("Playlist path does not exist.".to_string());
                    }
                }
                None => {
                    warnings.push("Playlist enabled but no playlist file set.".to_string());
                }
            }
        }
        for warning in [
            mount_pair_warning(
                "--mount-dir",
                &self.config.pallet_mount_dir_vroot,
                &self.config.pallet_mount_dir_path,
            ),
            mount_pair_warning(
                "--mount-pak",
                &self.config.pallet_mount_pak_vroot,
                &self.config.pallet_mount_pak_path,
            ),
            mount_pair_warning(
                "--mount-pk3",
                &self.config.pallet_mount_pk3_vroot,
                &self.config.pallet_mount_pk3_path,
            ),
        ]
        .into_iter()
        .flatten()
        {
            warnings.push(warning);
        }
        warnings
    }

    fn stop_all_processes(&mut self) {
        self.pallet_process.stop();
        self.tools_process.stop();
        self.server_process.stop();
        self.client_process.stop();
        self.checks_process.stop();
    }

    fn take_window_action(&mut self) -> Option<WindowAction> {
        self.pending_window_action.take()
    }

    fn ui(&mut self, ctx: &Context, window: &Window) {
        self.pallet_process.poll();
        self.update_asset_stats_cache();
        self.tools_process.poll();
        let tools_running = self.tools_process.is_running();
        if self.tools_last_running && !tools_running {
            self.refresh_build_manifest_cache();
        }
        self.tools_last_running = tools_running;
        self.server_process.poll();
        self.client_process.poll();
        self.checks_process.poll();
        self.handle_resize_drag(ctx, window);
        self.ui_title_bar(ctx, window);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(format!("Pallet Runner GUI {}", APP_VERSION));
            ui.add_space(8.0);
            ui.label("Repo Root");
            ui.horizontal(|ui| {
                let response =
                    ui.add(TextEdit::singleline(&mut self.repo_root_input).desired_width(420.0));
                if response.changed() {
                    self.validate_repo_root();
                }
                if ui.button("Browse...").clicked() {
                    self.browse_notice = None;
                    if let Some(path) = browse_for_repo_root(self.repo_root.as_deref()) {
                        self.set_repo_root_path(path);
                    } else {
                        self.browse_notice = Some("Browse canceled or unavailable.".to_string());
                    }
                }
                if ui.button("Use current dir").clicked() {
                    if let Ok(path) = std::env::current_dir() {
                        self.set_repo_root_path(path);
                    }
                }
            });
            ui.colored_label(
                self.repo_root_status.color(),
                self.repo_root_status.message.as_str(),
            );
            if let Some(path) = self.repo_root.as_ref() {
                ui.label(format!("Resolved repo root: {}", path.display()));
            }
            if let Some(message) = self.browse_notice.as_ref() {
                ui.small(message);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Check cargo metadata").clicked() {
                    self.check_cargo_metadata();
                }
                if let Some(status) = self.metadata_status.as_ref() {
                    ui.colored_label(status.color(), status.message.as_str());
                }
            });
            if let Some(details) = self.metadata_details.as_ref() {
                ui.small(details);
            }
            ui.add_space(8.0);
            ui.separator();
            ui.small(
                "All runner commands will execute with this repo root as the working directory.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, AppTab::Pallet, "Pallet");
                ui.selectable_value(&mut self.active_tab, AppTab::Tools, "Tools");
                ui.selectable_value(&mut self.active_tab, AppTab::Net, "Net");
                ui.selectable_value(&mut self.active_tab, AppTab::Checks, "Checks");
            });
            ui.separator();
            match self.active_tab {
                AppTab::Pallet => self.ui_pallet(ui, ctx),
                AppTab::Tools => {
                    self.ui_tools(ui);
                }
                AppTab::Net => {
                    self.ui_net(ui);
                }
                AppTab::Checks => {
                    self.ui_checks(ui, ctx);
                }
            }
        });
    }

    fn ui_title_bar(&mut self, ctx: &Context, window: &Window) {
        egui::TopBottomPanel::top("title_bar")
            .exact_height(TITLE_BAR_HEIGHT)
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                let mut block_drag = false;
                ui.allocate_ui_at_rect(rect, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(WINDOW_TITLE);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let close = ui.add_sized([28.0, 20.0], egui::Button::new("X"));
                            if close.clicked() {
                                self.pending_window_action = Some(WindowAction::Close);
                            }
                            block_drag |= close.hovered();
                            let max = ui.add_sized([28.0, 20.0], egui::Button::new("[ ]"));
                            if max.clicked() {
                                self.pending_window_action = Some(WindowAction::MaximizeToggle);
                            }
                            block_drag |= max.hovered();
                            let min = ui.add_sized([28.0, 20.0], egui::Button::new("_"));
                            if min.clicked() {
                                self.pending_window_action = Some(WindowAction::Minimize);
                            }
                            block_drag |= min.hovered();
                        });
                    });
                });
                if response.drag_started() && !block_drag {
                    let _ = window.drag_window();
                }
            });
    }

    fn handle_resize_drag(&self, ctx: &Context, window: &Window) {
        if window.is_maximized() || self.custom_maximized {
            window.set_cursor_icon(CursorIcon::Default);
            return;
        }
        let pos = ctx.input(|input| input.pointer.hover_pos());
        let Some(pos) = pos else {
            window.set_cursor_icon(CursorIcon::Default);
            return;
        };
        let rect = ctx.screen_rect();
        let direction = resize_direction(rect, pos);
        window.set_cursor_icon(match direction {
            Some(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => CursorIcon::NwseResize,
            Some(ResizeDirection::NorthEast | ResizeDirection::SouthWest) => CursorIcon::NeswResize,
            Some(ResizeDirection::West | ResizeDirection::East) => CursorIcon::EwResize,
            Some(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            None => CursorIcon::Default,
        });
        let pressed = ctx.input(|input| input.pointer.primary_pressed());
        if !pressed {
            return;
        }
        let Some(direction) = direction else {
            return;
        };
        let _ = window.drag_resize_window(direction);
    }

    fn ui_pallet(&mut self, ui: &mut egui::Ui, ctx: &Context) {
        ui.heading("Run Pallet");
        ui.add_space(6.0);
        ui.label("Quake dir");
        let mut quake_dir = self.config.quake_dir.clone();
        if ui.text_edit_singleline(&mut quake_dir).changed() {
            self.config.quake_dir = quake_dir;
        }
        ui.add_space(4.0);
        ui.label("Map");
        let mut map = self.config.map.clone();
        if ui.text_edit_singleline(&mut map).changed() {
            self.config.map = map;
        }
        ui.add_space(6.0);
        let mut playlist_enabled = self.config.playlist_enabled;
        if ui.checkbox(&mut playlist_enabled, "Use playlist").changed() {
            self.config.playlist_enabled = playlist_enabled;
        }
        let playlist_initial = self.initial_dir_for_path(self.config.playlist_path.as_deref());
        let mut playlist_path = self.config.playlist_path.clone().unwrap_or_default();
        let mut playlist_pick = None;
        ui.add_enabled_ui(playlist_enabled, |ui| {
            if ui.text_edit_singleline(&mut playlist_path).changed() {
                update_optional_string(&mut self.config.playlist_path, &playlist_path);
            }
            if ui.button("Browse...").clicked() {
                playlist_pick = browse_for_file(
                    playlist_initial.as_deref(),
                    "Select playlist",
                    Some("Playlist files (*.txt)|*.txt|All files (*.*)|*.*"),
                );
            }
        });
        if let Some(path) = playlist_pick {
            playlist_path = path.display().to_string();
            update_optional_string(&mut self.config.playlist_path, &playlist_path);
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
            ui.label("Show image asset");
            let mut show_image = self.config.show_image.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut show_image).changed() {
                update_optional_string(&mut self.config.show_image, &show_image);
            }
            ui.label("Play movie file");
            let play_movie_initial = self.initial_dir_for_path(self.config.play_movie.as_deref());
            let mut play_movie = self.config.play_movie.clone().unwrap_or_default();
            let mut play_movie_pick = None;
            ui.horizontal(|ui| {
                if ui.text_edit_singleline(&mut play_movie).changed() {
                    update_optional_string(&mut self.config.play_movie, &play_movie);
                }
                if ui.button("Browse...").clicked() {
                    play_movie_pick = browse_for_file(
                        play_movie_initial.as_deref(),
                        "Select movie file",
                        Some(
                            "Video files (*.ogv;*.mp4;*.mkv)|*.ogv;*.mp4;*.mkv|All files (*.*)|*.*",
                        ),
                    );
                }
            });
            if let Some(path) = play_movie_pick {
                play_movie = path.display().to_string();
                update_optional_string(&mut self.config.play_movie, &play_movie);
            }
            ui.label("Script path");
            let script_initial = self.initial_dir_for_path(self.config.script_path.as_deref());
            let mut script_path = self.config.script_path.clone().unwrap_or_default();
            let mut script_pick = None;
            ui.horizontal(|ui| {
                if ui.text_edit_singleline(&mut script_path).changed() {
                    update_optional_string(&mut self.config.script_path, &script_path);
                }
                if ui.button("Browse...").clicked() {
                    script_pick = browse_for_file(
                        script_initial.as_deref(),
                        "Select script",
                        Some("Lua scripts (*.lua)|*.lua|All files (*.*)|*.*"),
                    );
                }
            });
            if let Some(path) = script_pick {
                script_path = path.display().to_string();
                update_optional_string(&mut self.config.script_path, &script_path);
            }
            ui.label("Mount manifest");
            let manifest_initial = self.initial_dir_for_path(self.config.mount_manifest.as_deref());
            let mut mount_manifest = self.config.mount_manifest.clone().unwrap_or_default();
            let mut manifest_pick = None;
            ui.horizontal(|ui| {
                if ui.text_edit_singleline(&mut mount_manifest).changed() {
                    update_optional_string(&mut self.config.mount_manifest, &mount_manifest);
                }
                if ui.button("Browse...").clicked() {
                    manifest_pick = browse_for_file(
                        manifest_initial.as_deref(),
                        "Select mount manifest",
                        Some("Manifest files (*.txt)|*.txt|All files (*.*)|*.*"),
                    );
                }
            });
            if let Some(path) = manifest_pick {
                mount_manifest = path.display().to_string();
                update_optional_string(&mut self.config.mount_manifest, &mount_manifest);
            }
            ui.label("Mount dir");
            vfs_mount_row(
                ui,
                &mut self.config.pallet_mount_dir_vroot,
                &mut self.config.pallet_mount_dir_path,
                "Select mount directory",
                None,
            );
            ui.label("Mount pak");
            vfs_mount_row(
                ui,
                &mut self.config.pallet_mount_pak_vroot,
                &mut self.config.pallet_mount_pak_path,
                "Select pak file",
                Some("Pak files (*.pak)|*.pak|All files (*.*)|*.*"),
            );
            ui.label("Mount pk3");
            vfs_mount_row(
                ui,
                &mut self.config.pallet_mount_pk3_vroot,
                &mut self.config.pallet_mount_pk3_path,
                "Select pk3 file",
                Some("PK3 files (*.pk3)|*.pk3|All files (*.*)|*.*"),
            );
            let mut input_script = self.config.input_script;
            if ui.checkbox(&mut input_script, "Input script").changed() {
                self.config.input_script = input_script;
            }
        });
        ui.add_space(8.0);
        ui.label("Debug controls");
        let mut video_debug = self.config.video_debug;
        if ui.checkbox(&mut video_debug, "Video debug stats").changed() {
            self.config.video_debug = video_debug;
        }
        egui::ComboBox::from_label("Debug preset")
            .selected_text(self.config.debug_preset.clone())
            .show_ui(ui, |ui| {
                for preset in &self.config.debug_presets {
                    ui.selectable_value(
                        &mut self.config.debug_preset,
                        preset.name.clone(),
                        preset.name.as_str(),
                    );
                }
            });
        if let Some(preset) =
            find_debug_preset(&self.config.debug_presets, &self.config.debug_preset)
        {
            ui.small(&preset.description);
        }
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            let can_run = self.repo_root.is_some();
            if ui
                .add_enabled(can_run, egui::Button::new("Run Pallet"))
                .clicked()
            {
                self.run_pallet();
            }
            if ui.button("Copy command").clicked() {
                if let Some(line) = self.build_pallet_command_line() {
                    ctx.output_mut(|output| output.copied_text = line);
                }
            }
            if ui
                .add_enabled(self.pallet_process.is_running(), egui::Button::new("Stop"))
                .clicked()
            {
                self.pallet_process.stop();
            }
        });
        ui.add_space(6.0);
        for warning in self.pallet_warnings() {
            ui.colored_label(STATUS_WARN, warning);
        }
        self.ui_asset_stats_summary(ui);
        ui.label(self.pallet_process.status_line());
        ui.separator();
        log_header(ui, "Pallet log", &mut self.pallet_process);
        egui::ScrollArea::vertical()
            .id_source("pallet_log")
            .max_height(200.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                render_log_lines(ui, &self.pallet_process.logs);
            });
    }

    fn ui_asset_stats_summary(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label("Asset stats (last dev_asset_stats)");
        if self.asset_stats_lines.is_empty() {
            ui.small("Run dev_asset_stats in the console to populate this panel.");
            return;
        }
        for line in &self.asset_stats_lines {
            ui.small(line);
        }
    }

    fn ui_tools(&mut self, ui: &mut egui::Ui) {
        ui.heading("Tools");
        ui.add_space(6.0);
        ui.label("Content toolchain");
        let can_run_tools = self.repo_root.is_some() && !self.tools_process.is_running();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_run_tools, egui::Button::new("Lint IDs"))
                .clicked()
            {
                self.run_tools_content_lint();
            }
            if ui
                .add_enabled(can_run_tools, egui::Button::new("Validate"))
                .clicked()
            {
                self.run_tools_content_validate();
            }
            if ui
                .add_enabled(can_run_tools, egui::Button::new("Build"))
                .clicked()
            {
                self.run_tools_content_build();
            }
            if ui
                .add_enabled(can_run_tools, egui::Button::new("Clean"))
                .clicked()
            {
                self.run_tools_content_clean();
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_run_tools, egui::Button::new("Doctor"))
                .clicked()
            {
                self.run_tools_content_doctor();
            }
            if ui
                .add_enabled(can_run_tools, egui::Button::new("Quake Index"))
                .clicked()
            {
                self.run_tools_quake_index();
            }
        });
        ui.add_space(6.0);
        self.ui_content_manifest_summary(ui);
        ui.add_space(6.0);
        ui.separator();
        ui.label("Smoke");
        let mut smoke_mode = self.config.smoke_mode.clone();
        egui::ComboBox::from_label("Mode")
            .selected_text(smoke_mode.clone())
            .show_ui(ui, |ui| {
                for mode in SMOKE_MODES {
                    ui.selectable_value(&mut smoke_mode, mode.to_string(), mode);
                }
            });
        self.config.smoke_mode = smoke_mode;
        ui.label("Ticks (optional)");
        let mut ticks_text = self
            .config
            .smoke_ticks
            .map(|ticks| ticks.to_string())
            .unwrap_or_default();
        if ui
            .text_edit_singleline(&mut ticks_text)
            .on_hover_text("Optional tick count")
            .changed()
        {
            let trimmed = ticks_text.trim();
            if trimmed.is_empty() {
                self.config.smoke_ticks = None;
            } else if let Ok(value) = trimmed.parse::<u32>() {
                self.config.smoke_ticks = Some(value.max(1));
            }
        }
        let mut headless = self.config.smoke_headless;
        if ui.checkbox(&mut headless, "Headless").changed() {
            self.config.smoke_headless = headless;
        }
        ui.add_space(4.0);
        ui.label("Quake dir");
        let mut quake_dir = self.config.quake_dir.clone();
        if ui.text_edit_singleline(&mut quake_dir).changed() {
            self.config.quake_dir = quake_dir;
        }
        ui.label("Map");
        let mut map = self.config.map.clone();
        if ui.text_edit_singleline(&mut map).changed() {
            self.config.map = map;
        }
        ui.horizontal(|ui| {
            let can_run = self.repo_root.is_some();
            if ui
                .add_enabled(can_run, egui::Button::new("Run Smoke"))
                .clicked()
            {
                self.run_tools_smoke();
            }
            if ui
                .add_enabled(self.tools_process.is_running(), egui::Button::new("Stop"))
                .clicked()
            {
                self.tools_process.stop();
            }
        });
        ui.add_space(8.0);
        ui.separator();
        ui.label("Pak");
        let pak_initial = self.initial_dir_for_path(self.config.pak_out_dir.as_deref());
        let mut pak_out = self.config.pak_out_dir.clone().unwrap_or_default();
        let mut pak_pick = None;
        ui.horizontal(|ui| {
            if ui.text_edit_singleline(&mut pak_out).changed() {
                update_optional_string(&mut self.config.pak_out_dir, &pak_out);
            }
            if ui.button("Browse...").clicked() {
                pak_pick =
                    browse_for_folder(pak_initial.as_deref(), "Select pak extract output folder");
            }
        });
        if let Some(path) = pak_pick {
            pak_out = path.display().to_string();
            update_optional_string(&mut self.config.pak_out_dir, &pak_out);
        }
        ui.horizontal(|ui| {
            let can_run = self.repo_root.is_some();
            if ui
                .add_enabled(can_run, egui::Button::new("Pak List"))
                .clicked()
            {
                self.run_tools_pak_list();
            }
            if ui
                .add_enabled(can_run, egui::Button::new("Pak Extract"))
                .clicked()
            {
                self.run_tools_pak_extract();
            }
        });
        ui.add_space(6.0);
        ui.separator();
        ui.label("VFS stat");
        let mut vfs_vpath = self.config.vfs_vpath.clone();
        if ui.text_edit_singleline(&mut vfs_vpath).changed() {
            self.config.vfs_vpath = vfs_vpath;
        }
        let mut use_quake_dir = self.config.vfs_use_quake_dir;
        if ui.checkbox(&mut use_quake_dir, "Mount Quake dir").changed() {
            self.config.vfs_use_quake_dir = use_quake_dir;
        }
        ui.label("Mount manifest");
        let manifest_initial = self.initial_dir_for_path(self.config.vfs_mount_manifest.as_deref());
        let mut vfs_manifest = self.config.vfs_mount_manifest.clone().unwrap_or_default();
        let mut manifest_pick = None;
        ui.horizontal(|ui| {
            if ui.text_edit_singleline(&mut vfs_manifest).changed() {
                update_optional_string(&mut self.config.vfs_mount_manifest, &vfs_manifest);
            }
            if ui.button("Browse...").clicked() {
                manifest_pick = browse_for_file(
                    manifest_initial.as_deref(),
                    "Select mount manifest",
                    Some("Manifest files (*.txt)|*.txt|All files (*.*)|*.*"),
                );
            }
        });
        if let Some(path) = manifest_pick {
            vfs_manifest = path.display().to_string();
            update_optional_string(&mut self.config.vfs_mount_manifest, &vfs_manifest);
        }
        ui.label("Mount dir");
        vfs_mount_row(
            ui,
            &mut self.config.vfs_mount_dir_vroot,
            &mut self.config.vfs_mount_dir_path,
            "Select mount directory",
            None,
        );
        ui.label("Mount pak");
        vfs_mount_row(
            ui,
            &mut self.config.vfs_mount_pak_vroot,
            &mut self.config.vfs_mount_pak_path,
            "Select pak file",
            Some("Pak files (*.pak)|*.pak|All files (*.*)|*.*"),
        );
        ui.label("Mount pk3");
        vfs_mount_row(
            ui,
            &mut self.config.vfs_mount_pk3_vroot,
            &mut self.config.vfs_mount_pk3_path,
            "Select pk3 file",
            Some("PK3 files (*.pk3)|*.pk3|All files (*.*)|*.*"),
        );
        ui.horizontal(|ui| {
            let can_run = self.repo_root.is_some();
            if ui
                .add_enabled(can_run, egui::Button::new("Run VFS Stat"))
                .clicked()
            {
                self.run_tools_vfs_stat();
            }
        });
        ui.add_space(6.0);
        ui.label(self.tools_process.status_line());
        ui.separator();
        log_header(ui, "Tools log", &mut self.tools_process);
        egui::ScrollArea::vertical()
            .id_source("tools_log")
            .max_height(200.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                render_log_lines(ui, &self.tools_process.logs);
            });
    }

    fn ui_content_manifest_summary(&mut self, ui: &mut egui::Ui) {
        if self.content_manifest.is_none() && self.content_manifest_error.is_none() {
            self.refresh_build_manifest_cache();
        }
        ui.label("Build manifest");
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.refresh_build_manifest_cache();
            }
            if let Some(path) = self.build_manifest_path() {
                ui.small(path.display().to_string());
            }
        });
        if let Some(error) = self.content_manifest_error.as_ref() {
            ui.colored_label(STATUS_ERR, error);
        }
        let Some(summary) = self.content_manifest.as_ref() else {
            ui.small("No build manifest loaded yet.");
            return;
        };
        egui::Grid::new("build_manifest_summary")
            .striped(true)
            .show(ui, |ui| {
                ui.label("Build ID");
                ui.label(summary.build_id.as_deref().unwrap_or("n/a"));
                ui.end_row();
                ui.label("Version");
                ui.label(summary.version.as_deref().unwrap_or("n/a"));
                ui.end_row();
                ui.label("Tool version");
                ui.label(summary.tool_version.as_deref().unwrap_or("n/a"));
                ui.end_row();
                ui.label("Profile");
                ui.label(summary.profile.as_deref().unwrap_or("n/a"));
                ui.end_row();
                ui.label("Platform");
                ui.label(summary.platform.as_deref().unwrap_or("n/a"));
                ui.end_row();
                ui.label("Timestamp");
                ui.label(summary.timestamp.as_deref().unwrap_or("n/a"));
                ui.end_row();
                ui.label("Mounts");
                ui.label(
                    summary
                        .mount_count
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| summary.mounts.len().to_string()),
                );
                ui.end_row();
                ui.label("Inputs");
                ui.label(
                    summary
                        .input_count
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "n/a".to_string()),
                );
                ui.end_row();
                ui.label("Outputs");
                ui.label(
                    summary
                        .output_count
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "n/a".to_string()),
                );
                ui.end_row();
            });
        if !summary.stages.is_empty() {
            ui.add_space(4.0);
            ui.label("Stages");
            egui::Grid::new("build_manifest_stages")
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.label("Status");
                    ui.label("Duration");
                    ui.end_row();
                    for stage in &summary.stages {
                        ui.label(&stage.name);
                        ui.label(&stage.status);
                        ui.label(&stage.duration_ms);
                        ui.end_row();
                    }
                });
        }
        if let Some(quake) = summary.quake_index.as_ref() {
            ui.add_space(4.0);
            ui.label(format!(
                "Quake index: v{} entries={} fingerprint={}",
                quake.version, quake.entry_count, quake.fingerprint
            ));
        }
        if !summary.mounts.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Mount table summary")
                .default_open(false)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_source("build_manifest_mounts")
                        .max_height(160.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("build_manifest_mounts_grid")
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("NS");
                                    ui.label("Order");
                                    ui.label("Layer");
                                    ui.label("Kind");
                                    ui.label("Name");
                                    ui.label("Source");
                                    ui.end_row();
                                    for mount in &summary.mounts {
                                        ui.label(&mount.namespace);
                                        ui.label(&mount.order);
                                        ui.label(&mount.layer);
                                        ui.label(&mount.kind);
                                        ui.label(&mount.name);
                                        ui.label(&mount.source);
                                        ui.end_row();
                                    }
                                });
                        });
                });
        }
    }

    fn ui_net(&mut self, ui: &mut egui::Ui) {
        ui.heading("Net");
        ui.add_space(6.0);
        let available = ui.available_width();
        let column_width = (available - 12.0).max(0.0) * 0.5;
        let row_height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(column_width, row_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label("Dedicated server");
                    ui.horizontal(|ui| {
                        ui.label("Bind");
                        ui.text_edit_singleline(&mut self.config.server_bind);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Tick ms");
                        ui.add(
                            egui::DragValue::new(&mut self.config.server_tick_ms).range(1..=1000),
                        );
                        ui.label("Snapshot stride");
                        ui.add(
                            egui::DragValue::new(&mut self.config.server_snapshot_stride)
                                .range(1..=64),
                        );
                    });
                    let mut server_max_enabled = self.config.server_max_ticks.is_some();
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut server_max_enabled, "Max ticks").changed() {
                            if !server_max_enabled {
                                self.config.server_max_ticks = None;
                            } else if self.config.server_max_ticks.is_none() {
                                self.config.server_max_ticks = Some(120);
                            }
                        }
                        if server_max_enabled {
                            let mut value = self.config.server_max_ticks.unwrap_or(1);
                            ui.add(egui::DragValue::new(&mut value).range(1..=1000000));
                            self.config.server_max_ticks = Some(value);
                        }
                    });
                    ui.horizontal(|ui| {
                        let can_run = self.repo_root.is_some();
                        if ui
                            .add_enabled(can_run, egui::Button::new("Run Server"))
                            .clicked()
                        {
                            self.run_server();
                        }
                        if ui
                            .add_enabled(
                                self.server_process.is_running(),
                                egui::Button::new("Stop"),
                            )
                            .clicked()
                        {
                            self.server_process.stop();
                        }
                    });
                    ui.label(self.server_process.status_line());
                    log_header(ui, "Server log", &mut self.server_process);
                    let log_height = ui.available_height().max(0.0);
                    egui::ScrollArea::vertical()
                        .id_source("net_server_log")
                        .max_height(log_height)
                        .min_scrolled_height(log_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            render_log_lines(ui, &self.server_process.logs);
                        });
                },
            );
            ui.add(egui::Separator::default().vertical());
            ui.allocate_ui_with_layout(
                egui::vec2(column_width, row_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label("Headless client");
                    ui.horizontal(|ui| {
                        ui.label("Bind");
                        ui.text_edit_singleline(&mut self.config.client_bind);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Server");
                        ui.text_edit_singleline(&mut self.config.client_server);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Tick ms");
                        ui.add(
                            egui::DragValue::new(&mut self.config.client_tick_ms).range(1..=1000),
                        );
                        ui.label("Ticks");
                        ui.add(
                            egui::DragValue::new(&mut self.config.client_ticks).range(1..=1000000),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Client id");
                        ui.add(egui::DragValue::new(&mut self.config.client_id).range(1..=64));
                    });
                    let mut move_enabled = self.config.client_move_enabled;
                    if ui
                        .checkbox(&mut move_enabled, "Movement overrides")
                        .changed()
                    {
                        self.config.client_move_enabled = move_enabled;
                    }
                    ui.add_enabled_ui(move_enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Move X");
                            ui.add(
                                egui::DragValue::new(&mut self.config.client_move_x)
                                    .speed(0.05)
                                    .range(-10.0..=10.0),
                            );
                            ui.label("Move Y");
                            ui.add(
                                egui::DragValue::new(&mut self.config.client_move_y)
                                    .speed(0.05)
                                    .range(-10.0..=10.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Yaw step");
                            ui.add(
                                egui::DragValue::new(&mut self.config.client_yaw_step)
                                    .speed(0.01)
                                    .range(-1.0..=1.0),
                            );
                        });
                    });
                    ui.horizontal(|ui| {
                        let can_run = self.repo_root.is_some();
                        if ui
                            .add_enabled(can_run, egui::Button::new("Run Client"))
                            .clicked()
                        {
                            self.run_client();
                        }
                        if ui
                            .add_enabled(
                                self.client_process.is_running(),
                                egui::Button::new("Stop"),
                            )
                            .clicked()
                        {
                            self.client_process.stop();
                        }
                    });
                    ui.label(self.client_process.status_line());
                    log_header(ui, "Client log", &mut self.client_process);
                    let log_height = ui.available_height().max(0.0);
                    egui::ScrollArea::vertical()
                        .id_source("net_client_log")
                        .max_height(log_height)
                        .min_scrolled_height(log_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            render_log_lines(ui, &self.client_process.logs);
                        });
                },
            );
        });
    }

    fn ui_checks(&mut self, ui: &mut egui::Ui, ctx: &Context) {
        ui.heading("Checks");
        ui.add_space(6.0);
        let can_run = self.repo_root.is_some();
        let checks_running = self.checks_process.is_running();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_run && !checks_running, egui::Button::new("fmt"))
                .clicked()
            {
                self.run_checks_fmt();
            }
            if ui
                .add_enabled(can_run && !checks_running, egui::Button::new("clippy"))
                .clicked()
            {
                self.run_checks_clippy();
            }
            if ui
                .add_enabled(can_run && !checks_running, egui::Button::new("test"))
                .clicked()
            {
                self.run_checks_test();
            }
            if ui
                .add_enabled(checks_running, egui::Button::new("Stop"))
                .clicked()
            {
                self.checks_process.stop();
            }
        });
        if !can_run {
            ui.colored_label(STATUS_WARN, "Select a valid repo root to run checks.");
        }
        ui.add_space(6.0);
        ui.label(self.checks_process.status_line());
        ui.separator();
        log_header(ui, "Checks log", &mut self.checks_process);
        egui::ScrollArea::vertical()
            .id_source("checks_log")
            .max_height(220.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                render_log_lines(ui, &self.checks_process.logs);
            });
        ui.add_space(8.0);
        ui.separator();
        ui.label("In-game console notes");
        ui.horizontal(|ui| {
            ui.monospace("logfill [count]");
            ui.label("(1-20000)");
            if ui.button("Copy").clicked() {
                ctx.output_mut(|output| {
                    output.copied_text = "logfill [count]".to_string();
                });
            }
        });
        ui.small("Use logfill to stress the in-game console log rendering.");
    }

    fn save_config(&self) {
        if let Err(err) = self.config.save() {
            eprintln!("runner gui config save failed: {}", err);
        }
    }

    fn initial_dir_for_path(&self, value: Option<&str>) -> Option<PathBuf> {
        if let Some(path) = value {
            let path = Path::new(path);
            if path.is_absolute() {
                if let Some(parent) = path.parent() {
                    return Some(parent.to_path_buf());
                }
            }
        }
        self.repo_root.clone()
    }
}

fn update_optional_string(target: &mut Option<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        *target = None;
    } else {
        *target = Some(trimmed.to_string());
    }
}

fn push_mount_pair_args(
    args: &mut Vec<String>,
    flag: &str,
    vroot: &Option<String>,
    path: &Option<String>,
) {
    let vroot = vroot
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let path = path
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if let (Some(vroot), Some(path)) = (vroot, path) {
        args.push(flag.to_string());
        args.push(vroot.to_string());
        args.push(path.to_string());
    }
}

fn mount_pair_warning(flag: &str, vroot: &Option<String>, path: &Option<String>) -> Option<String> {
    let vroot_set = vroot
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .is_some();
    let path_set = path
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .is_some();
    match (vroot_set, path_set) {
        (true, false) | (false, true) => Some(format!("{} expects both vroot and path.", flag)),
        _ => None,
    }
}

fn push_mount_pair(
    command: &mut Command,
    flag: &str,
    vroot: Option<&str>,
    path: Option<&str>,
) -> Result<(), String> {
    let vroot = vroot
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let path = path
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    match (vroot, path) {
        (Some(vroot), Some(path)) => {
            command.arg(flag).arg(vroot).arg(path);
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(format!("{} expects both vroot and path.", flag)),
    }
}

fn vfs_mount_row(
    ui: &mut egui::Ui,
    vroot: &mut Option<String>,
    path: &mut Option<String>,
    picker_title: &str,
    picker_filter: Option<&str>,
) {
    let mut vroot_value = vroot.clone().unwrap_or_default();
    let mut path_value = path.clone().unwrap_or_default();
    let mut pick = None;
    ui.horizontal(|ui| {
        if ui
            .add(egui::TextEdit::singleline(&mut vroot_value).desired_width(120.0))
            .changed()
        {
            update_optional_string(vroot, &vroot_value);
        }
        if ui
            .add(egui::TextEdit::singleline(&mut path_value).desired_width(320.0))
            .changed()
        {
            update_optional_string(path, &path_value);
        }
        if ui.button("Browse...").clicked() {
            pick = match picker_filter {
                Some(filter) => browse_for_file(None, picker_title, Some(filter)),
                None => browse_for_folder(None, picker_title),
            };
        }
    });
    if let Some(path_pick) = pick {
        path_value = path_pick.display().to_string();
        update_optional_string(path, &path_value);
    }
}

fn render_log_lines(ui: &mut egui::Ui, logs: &VecDeque<String>) {
    for line in logs {
        ui.add(egui::Label::new(egui::RichText::new(line).monospace()).wrap());
    }
}

fn resize_direction(rect: egui::Rect, pos: egui::Pos2) -> Option<ResizeDirection> {
    let left = pos.x <= rect.left() + RESIZE_BORDER;
    let right = pos.x >= rect.right() - RESIZE_BORDER;
    let top = pos.y <= rect.top() + RESIZE_BORDER;
    let bottom = pos.y >= rect.bottom() - RESIZE_BORDER;
    if left && top {
        Some(ResizeDirection::NorthWest)
    } else if right && top {
        Some(ResizeDirection::NorthEast)
    } else if left && bottom {
        Some(ResizeDirection::SouthWest)
    } else if right && bottom {
        Some(ResizeDirection::SouthEast)
    } else if left {
        Some(ResizeDirection::West)
    } else if right {
        Some(ResizeDirection::East)
    } else if top {
        Some(ResizeDirection::North)
    } else if bottom {
        Some(ResizeDirection::South)
    } else {
        None
    }
}

fn log_header(ui: &mut egui::Ui, title: &str, lane: &mut ProcessLane) {
    ui.horizontal(|ui| {
        ui.label(title);
        if ui.button("Clear log").clicked() {
            lane.clear();
        }
    });
}

#[derive(Clone, Copy, Debug)]
enum LogStream {
    Stdout,
    Stderr,
}

struct LogEvent {
    stream: LogStream,
    line: String,
}

struct ProcessLane {
    logs: VecDeque<String>,
    max_lines: usize,
    child: Option<Child>,
    events: Option<Receiver<LogEvent>>,
    exit_code: Option<i32>,
    started_at: Option<Instant>,
    ended_at: Option<Instant>,
}

impl ProcessLane {
    fn new(max_lines: usize) -> Self {
        Self {
            logs: VecDeque::new(),
            max_lines,
            child: None,
            events: None,
            exit_code: None,
            started_at: None,
            ended_at: None,
        }
    }

    fn is_running(&self) -> bool {
        self.child.is_some()
    }

    fn status_line(&self) -> String {
        if self.is_running() {
            let elapsed = self
                .started_at
                .map(|start| format_duration(start.elapsed()))
                .unwrap_or_else(|| "running".to_string());
            return format!("Status: running ({}).", elapsed);
        }
        if let Some(code) = self.exit_code {
            let duration = self
                .ended_at
                .zip(self.started_at)
                .map(|(end, start)| format_duration(end - start))
                .unwrap_or_else(|| "unknown".to_string());
            return format!("Status: exited with code {} ({}).", code, duration);
        }
        "Status: idle.".to_string()
    }

    fn push_system(&mut self, message: impl Into<String>) {
        self.push_line(format!("[system] {}", message.into()));
    }

    fn push_line(&mut self, line: String) {
        self.logs.push_back(line);
        while self.logs.len() > self.max_lines {
            self.logs.pop_front();
        }
    }

    fn clear(&mut self) {
        self.logs.clear();
    }

    fn start(&mut self, mut command: Command) -> Result<(), String> {
        if self.is_running() {
            return Err("Process already running.".to_string());
        }
        self.clear();
        configure_child_process(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| format!("Failed to start process: {}", err))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = mpsc::channel();
        if let Some(stream) = stdout {
            spawn_log_reader(stream, LogStream::Stdout, tx.clone());
        }
        if let Some(stream) = stderr {
            spawn_log_reader(stream, LogStream::Stderr, tx.clone());
        }
        self.child = Some(child);
        self.events = Some(rx);
        self.exit_code = None;
        self.started_at = Some(Instant::now());
        self.ended_at = None;
        Ok(())
    }

    fn poll(&mut self) {
        if let Some(events) = self.events.as_ref() {
            let mut pending = Vec::new();
            while let Ok(event) = events.try_recv() {
                pending.push(event);
            }
            for event in pending {
                let prefix = match event.stream {
                    LogStream::Stdout => "[stdout]",
                    LogStream::Stderr => "[stderr]",
                };
                self.push_line(format!("{} {}", prefix, event.line));
            }
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_code = status.code().or(Some(-1));
                    self.ended_at = Some(Instant::now());
                    self.child = None;
                }
                Ok(None) => {}
                Err(err) => {
                    self.push_system(format!("Process wait failed: {}", err));
                    self.child = None;
                }
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            match child.wait() {
                Ok(status) => {
                    self.exit_code = status.code().or(Some(-1));
                }
                Err(err) => {
                    self.push_system(format!("Process stop failed: {}", err));
                }
            }
            self.ended_at = Some(Instant::now());
        }
    }
}

fn spawn_log_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    stream: LogStream,
    sender: mpsc::Sender<LogEvent>,
) {
    thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            match line {
                Ok(line) => {
                    let _ = sender.send(LogEvent { stream, line });
                }
                Err(_) => break,
            }
        }
    });
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f32();
    format!("{:.1}s", seconds)
}

fn find_debug_preset<'a>(
    presets: &'a [DebugPresetConfig],
    name: &str,
) -> Option<&'a DebugPresetConfig> {
    presets.iter().find(|preset| preset.name == name)
}

fn quote_arg(value: &str) -> String {
    if value.contains(' ') || value.contains('\t') {
        let escaped = value.replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        value.to_string()
    }
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

struct RunnerUi {
    egui_ctx: Context,
    egui_state: EguiState,
    egui_renderer: EguiRenderer,
    frame_size: winit::dpi::PhysicalSize<u32>,
}

struct RunnerDrawData {
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_delta: egui::TexturesDelta,
    screen_descriptor: ScreenDescriptor,
}

impl RunnerUi {
    fn new(window: &Window, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let egui_ctx = Context::default();
        let egui_state = EguiState::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
        );
        let egui_renderer = EguiRenderer::new(device, format, None, 1);
        Self {
            egui_ctx,
            egui_state,
            egui_renderer,
            frame_size: window.inner_size(),
        }
    }

    fn handle_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        self.egui_state.on_window_event(window, event).consumed
    }

    fn begin_frame(
        &mut self,
        window: &Window,
        time_seconds: f64,
        frame_size: winit::dpi::PhysicalSize<u32>,
    ) -> Context {
        let mut raw_input = self.egui_state.take_egui_input(window);
        let pixels_per_point = egui_winit::pixels_per_point(&self.egui_ctx, window);
        let screen_size = egui::vec2(
            frame_size.width as f32 / pixels_per_point,
            frame_size.height as f32 / pixels_per_point,
        );
        raw_input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen_size));
        if let Some(viewport) = raw_input.viewports.get_mut(&egui::ViewportId::ROOT) {
            viewport.native_pixels_per_point = Some(pixels_per_point);
            viewport.inner_rect = raw_input.screen_rect;
        }
        raw_input.time = Some(time_seconds);
        self.frame_size = frame_size;
        self.egui_ctx.begin_frame(raw_input);
        self.egui_ctx.clone()
    }

    fn end_frame(&mut self, window: &Window) -> RunnerDrawData {
        let full_output = self.egui_ctx.end_frame();
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        RunnerDrawData {
            paint_jobs,
            textures_delta: full_output.textures_delta,
            screen_descriptor: ScreenDescriptor {
                size_in_pixels: [self.frame_size.width, self.frame_size.height],
                pixels_per_point: egui_winit::pixels_per_point(&self.egui_ctx, window),
            },
        }
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        draw_data: &RunnerDrawData,
    ) {
        for (id, image_delta) in &draw_data.textures_delta.set {
            self.egui_renderer
                .update_texture(device, queue, *id, image_delta);
        }
        self.egui_renderer.update_buffers(
            device,
            queue,
            encoder,
            &draw_data.paint_jobs,
            &draw_data.screen_descriptor,
        );
        if !draw_data.paint_jobs.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("runner_gui.egui.pass"),
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
            self.egui_renderer.render(
                &mut pass,
                &draw_data.paint_jobs,
                &draw_data.screen_descriptor,
            );
        }
        for id in &draw_data.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
    }
}

fn main() {
    let (event_loop, window) = create_window(WINDOW_TITLE, WINDOW_WIDTH, WINDOW_HEIGHT)
        .unwrap_or_else(|err| {
            eprintln!("window init failed: {}", err);
            std::process::exit(1);
        });
    let window: &'static Window = Box::leak(Box::new(window));
    window.set_decorations(false);
    if let Some(icon) = load_window_icon() {
        window.set_window_icon(Some(icon));
    }
    let main_window_id = window.id();

    let mut renderer = render_wgpu::Renderer::new(window).unwrap_or_else(|err| {
        eprintln!("renderer init failed: {}", err);
        std::process::exit(1);
    });
    renderer.set_clear_color_rgba(0.07, 0.08, 0.1, 1.0);

    let mut ui = RunnerUi::new(window, renderer.device(), renderer.surface_format());
    let mut app = RunnerApp::new();
    let start_time = Instant::now();
    let mut last_frame = start_time;

    window.set_visible(true);

    if let Err(err) = event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == main_window_id => {
                let _ = ui.handle_window_event(window, &event);
                match event {
                    WindowEvent::CloseRequested => {
                        app.stop_all_processes();
                        app.save_config();
                        elwt.exit();
                    }
                    WindowEvent::Resized(size) => {
                        if size.width > 0 && size.height > 0 {
                            if !window.is_maximized() && !app.custom_maximized {
                                app.last_windowed_size = size;
                            }
                            renderer.resize(size);
                        }
                    }
                    WindowEvent::ScaleFactorChanged { .. } => {
                        let size = renderer.window_inner_size();
                        if size.width > 0 && size.height > 0 {
                            renderer.resize(size);
                        }
                    }
                    WindowEvent::Moved(position) => {
                        if !window.is_maximized() && !app.custom_maximized {
                            app.last_windowed_pos = position;
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        let size = window.inner_size();
                        if size.width == 0 || size.height == 0 {
                            return;
                        }
                        if size != renderer.size() {
                            renderer.resize(size);
                        }
                        let now = Instant::now();
                        let time_seconds = (now - start_time).as_secs_f64();
                        let _dt = now - last_frame;
                        last_frame = now;

                        let ctx = ui.begin_frame(window, time_seconds, size);
                        app.ui(&ctx, window);
                        if let Some(action) = app.take_window_action() {
                            match action {
                                WindowAction::Minimize => window.set_minimized(true),
                                WindowAction::MaximizeToggle => {
                                    if app.custom_maximized {
                                        app.custom_maximized = false;
                                        let _ = window.request_inner_size(app.last_windowed_size);
                                        window.set_outer_position(app.last_windowed_pos);
                                    } else {
                                        app.custom_maximized = true;
                                        app.last_windowed_size = window.inner_size();
                                        if let Ok(position) = window.outer_position() {
                                            app.last_windowed_pos = position;
                                        }
                                        if let Some(monitor) = window
                                            .current_monitor()
                                            .or_else(|| window.primary_monitor())
                                        {
                                            let target_size = monitor.size();
                                            window.set_outer_position(monitor.position());
                                            let _ = window.request_inner_size(target_size);
                                        }
                                    }
                                }
                                WindowAction::Close => {
                                    app.stop_all_processes();
                                    app.save_config();
                                    elwt.exit();
                                }
                            }
                        }
                        let draw_data = ui.end_frame(window);

                        let render_result = renderer.render_with_overlay(
                            |device, queue, encoder, view, _format| {
                                ui.render(device, queue, encoder, view, &draw_data);
                            },
                        );
                        match render_result {
                            Ok(()) => {}
                            Err(RenderError::Lost | RenderError::Outdated) => {
                                let size = renderer.window_inner_size();
                                if size.width > 0 && size.height > 0 {
                                    renderer.resize(size);
                                }
                            }
                            Err(RenderError::OutOfMemory) => {
                                eprintln!("render error: out of memory");
                                elwt.exit();
                            }
                            Err(RenderError::Timeout) => {}
                        }
                    }
                    _ => {}
                }
            }
            Event::AboutToWait => {
                window.request_redraw();
            }
            Event::LoopExiting => {
                app.stop_all_processes();
                app.save_config();
            }
            _ => {}
        }
    }) {
        eprintln!("event loop exited with error: {}", err);
    }
}

fn load_window_icon() -> Option<Icon> {
    let bytes = include_bytes!("../../pallet_runner_gui_icon.png");
    let (rgba, width, height) = decode_png_icon(bytes)?;
    Icon::from_rgba(rgba, width, height).ok()
}

fn decode_png_icon(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let data = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(data.len() / 3 * 4);
            for chunk in data.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(data.len() * 4);
            for value in data {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(data.len() / 2 * 4);
            for chunk in data.chunks_exact(2) {
                rgba.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            rgba
        }
        _ => return None,
    };
    Some((rgba, info.width, info.height))
}

#[cfg(windows)]
fn configure_powershell(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
    command.arg("-NoProfile").arg("-WindowStyle").arg("Hidden");
}

#[cfg(windows)]
fn configure_child_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_child_process(_command: &mut Command) {}

#[cfg(windows)]
fn browse_for_repo_root(current_dir: Option<&Path>) -> Option<PathBuf> {
    const SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = 'Select Pallet repo root'
$dialog.ShowNewFolderButton = $false
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        Write-Output $dialog.SelectedPath
    }
"#;
    let mut command = Command::new("powershell");
    configure_powershell(&mut command);
    command.arg("-Command").arg(SCRIPT);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let selected = String::from_utf8_lossy(&output.stdout);
    let path = selected.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(not(windows))]
fn browse_for_repo_root(_current_dir: Option<&Path>) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn browse_for_file(
    current_dir: Option<&Path>,
    title: &str,
    filter: Option<&str>,
) -> Option<PathBuf> {
    const SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$dialog = New-Object System.Windows.Forms.OpenFileDialog
$dialog.Multiselect = $false
if ($env:RUNNER_PICKER_TITLE) { $dialog.Title = $env:RUNNER_PICKER_TITLE }
if ($env:RUNNER_PICKER_FILTER) { $dialog.Filter = $env:RUNNER_PICKER_FILTER }
if ($env:RUNNER_PICKER_INITIAL_DIR) { $dialog.InitialDirectory = $env:RUNNER_PICKER_INITIAL_DIR }
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        Write-Output $dialog.FileName
    }
"#;
    let mut command = Command::new("powershell");
    configure_powershell(&mut command);
    command
        .arg("-Command")
        .arg(SCRIPT)
        .env("RUNNER_PICKER_TITLE", title);
    if let Some(filter) = filter {
        command.env("RUNNER_PICKER_FILTER", filter);
    }
    if let Some(dir) = current_dir {
        command.env("RUNNER_PICKER_INITIAL_DIR", dir);
        command.current_dir(dir);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let selected = String::from_utf8_lossy(&output.stdout);
    let path = selected.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(not(windows))]
fn browse_for_file(
    _current_dir: Option<&Path>,
    _title: &str,
    _filter: Option<&str>,
) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn browse_for_folder(current_dir: Option<&Path>, title: &str) -> Option<PathBuf> {
    const SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
if ($env:RUNNER_PICKER_TITLE) { $dialog.Description = $env:RUNNER_PICKER_TITLE }
if ($env:RUNNER_PICKER_INITIAL_DIR) { $dialog.SelectedPath = $env:RUNNER_PICKER_INITIAL_DIR }
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        Write-Output $dialog.SelectedPath
    }
"#;
    let mut command = Command::new("powershell");
    configure_powershell(&mut command);
    command
        .arg("-Command")
        .arg(SCRIPT)
        .env("RUNNER_PICKER_TITLE", title);
    if let Some(dir) = current_dir {
        command.env("RUNNER_PICKER_INITIAL_DIR", dir);
        command.current_dir(dir);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let selected = String::from_utf8_lossy(&output.stdout);
    let path = selected.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(not(windows))]
fn browse_for_folder(_current_dir: Option<&Path>, _title: &str) -> Option<PathBuf> {
    None
}

fn format_output_excerpt(stdout: &str, stderr: &str) -> String {
    let mut text = String::new();
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    if !stdout.is_empty() {
        text.push_str("stdout: ");
        text.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push_str(" | ");
        }
        text.push_str("stderr: ");
        text.push_str(stderr);
    }
    truncate_text(&text, 300)
}

fn truncate_text(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut text = value.chars().take(max).collect::<String>();
    text.push_str("...");
    text
}

fn collect_level_manifest_paths(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_level_manifest_paths(&path, out)?;
        } else if path
            .file_name()
            .map(|name| name == "level.toml")
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
    Ok(())
}

fn read_build_manifest_ui(path: &Path) -> Result<BuildManifestUi, String> {
    let text =
        fs::read_to_string(path).map_err(|err| format!("build manifest read failed: {}", err))?;
    let mut summary = BuildManifestUi {
        version: None,
        tool_version: None,
        profile: None,
        build_id: None,
        platform: None,
        timestamp: None,
        mount_count: None,
        input_count: None,
        output_count: None,
        mounts: Vec::new(),
        stages: Vec::new(),
        quake_index: None,
    };
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("version=") {
            summary.version = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("tool_version=") {
            summary.tool_version = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("profile=") {
            summary.profile = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("build_id=") {
            summary.build_id = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("platform=") {
            summary.platform = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("timestamp=") {
            summary.timestamp = Some(value.to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("mount_count=") {
            summary.mount_count = value.parse::<usize>().ok();
            continue;
        }
        if let Some(value) = line.strip_prefix("input_count=") {
            summary.input_count = value.parse::<usize>().ok();
            continue;
        }
        if let Some(value) = line.strip_prefix("output_count=") {
            summary.output_count = value.parse::<usize>().ok();
            continue;
        }
        if let Some(rest) = line.strip_prefix("mount|") {
            let parts: Vec<_> = rest.split('|').collect();
            if parts.len() >= 6 {
                summary.mounts.push(ManifestMountUi {
                    namespace: unescape_manifest_field(parts[0]),
                    order: parts[1].to_string(),
                    layer: unescape_manifest_field(parts[2]),
                    kind: unescape_manifest_field(parts[3]),
                    name: unescape_manifest_field(parts[4]),
                    source: unescape_manifest_field(parts[5]),
                });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("stage|") {
            let parts: Vec<_> = rest.split('|').collect();
            if parts.len() >= 4 {
                summary.stages.push(ManifestStageUi {
                    name: unescape_manifest_field(parts[0]),
                    status: parts[2].to_string(),
                    duration_ms: format!("{} ms", parts[3]),
                });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("quake_index|") {
            let parts: Vec<_> = rest.split('|').collect();
            if parts.len() >= 3 {
                summary.quake_index = Some(ManifestQuakeIndexUi {
                    version: parts[0].to_string(),
                    fingerprint: unescape_manifest_field(parts[1]),
                    entry_count: parts[2].to_string(),
                });
            }
            continue;
        }
    }
    Ok(summary)
}

fn unescape_manifest_field(value: &str) -> String {
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
                "3B" => out.push(';'),
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

fn extract_dev_asset_stats(logs: &VecDeque<String>) -> Vec<String> {
    let mut start_idx = None;
    for (idx, line) in logs.iter().enumerate().rev() {
        let line = strip_log_prefix(line);
        if line.starts_with("asset stats:") {
            start_idx = Some(idx);
            break;
        }
    }
    let Some(start_idx) = start_idx else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for line in logs.iter().skip(start_idx) {
        let line = strip_log_prefix(line);
        if !is_asset_stats_line(line) {
            break;
        }
        lines.push(line.to_string());
    }
    lines
}

fn strip_log_prefix(line: &str) -> &str {
    line.strip_prefix("[stdout] ")
        .or_else(|| line.strip_prefix("[stderr] "))
        .unwrap_or(line)
}

fn is_asset_stats_line(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with("asset stats:")
        || line.starts_with("budget:")
        || line.starts_with("spent ms:")
        || line.starts_with("throttled:")
        || line.starts_with("entries:")
        || line.starts_with("decoded bytes:")
        || line.starts_with("by kind:")
        || line.starts_with("upload queue:")
        || line.starts_with("  ")
}
