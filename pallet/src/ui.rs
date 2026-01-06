use crate::settings::{Settings, WindowMode};
use egui::Context;
use egui_wgpu::{Renderer as EguiRenderer, ScreenDescriptor};
use egui_winit::State as EguiState;
use platform_winit::{PhysicalSize, Window};
use std::collections::BTreeSet;
use wgpu::{CommandEncoder, Device, Queue, TextureFormat, TextureView};

const FALLBACK_RESOLUTIONS: &[[u32; 2]] = &[
    [1280, 720],
    [1600, 900],
    [1920, 1080],
    [2560, 1440],
    [3840, 2160],
];

#[derive(Clone, Copy, Debug)]
pub struct ResolutionModel {
    pub physical_px: [u32; 2],
    pub dpi_scale: f32,
    pub ui_scale: f32,
    pub logical_px: [f32; 2],
    pub ui_points: [f32; 2],
}

impl ResolutionModel {
    pub fn new(physical_px: PhysicalSize<u32>, dpi_scale: f64, ui_scale: f32) -> Self {
        let dpi_scale = (dpi_scale as f32).max(0.0001);
        let ui_scale = ui_scale.max(0.0001);
        let logical_px = [
            physical_px.width as f32 / dpi_scale,
            physical_px.height as f32 / dpi_scale,
        ];
        let ui_points = [logical_px[0] * ui_scale, logical_px[1] * ui_scale];
        Self {
            physical_px: [physical_px.width, physical_px.height],
            dpi_scale,
            ui_scale,
            logical_px,
            ui_points,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct UiFrameInput {
    pub dt_seconds: f32,
    pub resolution: ResolutionModel,
    pub audio_available: bool,
}

#[derive(Clone, Debug)]
pub struct UiFrameContext {
    pub dt_seconds: f32,
    pub resolution: ResolutionModel,
    pub audio_available: bool,
    pub egui_ctx: Context,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuScreen {
    Main,
    Options,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuMode {
    Title,
    Pause,
}

#[derive(Clone, Debug)]
pub struct UiState {
    pub menu_open: bool,
    pub menu_screen: MenuScreen,
    pub menu_mode: MenuMode,
    pub console_open: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            menu_open: true,
            menu_screen: MenuScreen::Main,
            menu_mode: MenuMode::Title,
            console_open: false,
        }
    }
}

impl UiState {
    pub fn open_title(&mut self) {
        self.menu_open = true;
        self.menu_mode = MenuMode::Title;
        self.menu_screen = MenuScreen::Main;
    }

    pub fn open_pause(&mut self) {
        self.menu_open = true;
        self.menu_mode = MenuMode::Pause;
        self.menu_screen = MenuScreen::Main;
    }

    pub fn close_menu(&mut self) {
        self.menu_open = false;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UiOutput {
    pub quit_requested: bool,
    pub start_requested: bool,
    pub resume_requested: bool,
    pub settings_changed: bool,
    pub display_settings_changed: bool,
    pub wants_pointer: bool,
    pub wants_keyboard: bool,
}

pub struct UiDrawData {
    pub paint_jobs: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
    pub screen_descriptor: ScreenDescriptor,
    pub output: UiOutput,
}

pub struct UiFacade {
    window: &'static Window,
    egui_ctx: Context,
    egui_state: EguiState,
    egui_renderer: EguiRenderer,
    output: UiOutput,
    available_resolutions: Vec<[u32; 2]>,
}

impl UiFacade {
    pub fn new(window: &'static Window, device: &Device, format: TextureFormat) -> Self {
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
            window,
            egui_ctx,
            egui_state,
            egui_renderer,
            output: UiOutput::default(),
            available_resolutions: Vec::new(),
        }
    }

    pub fn handle_window_event(&mut self, event: &platform_winit::WindowEvent) -> bool {
        let response = self.egui_state.on_window_event(self.window, event);
        response.consumed
    }

    pub fn begin_frame(&mut self, input: UiFrameInput) -> UiFrameContext {
        self.egui_ctx.set_zoom_factor(input.resolution.ui_scale);
        let mut raw_input = self.egui_state.take_egui_input(self.window);
        raw_input.time = raw_input.time.or(Some(0.0));
        self.egui_ctx.begin_frame(raw_input);
        UiFrameContext {
            dt_seconds: input.dt_seconds,
            resolution: input.resolution,
            audio_available: input.audio_available,
            egui_ctx: self.egui_ctx.clone(),
        }
    }

    pub fn build_ui(
        &mut self,
        ctx: &mut UiFrameContext,
        state: &mut UiState,
        settings: &mut Settings,
    ) {
        let _ = (ctx.dt_seconds, ctx.resolution, state.console_open);
        let mut output = UiOutput::default();
        if state.menu_open {
            egui::CentralPanel::default().show(&ctx.egui_ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(match state.menu_mode {
                        MenuMode::Title => "Pallet",
                        MenuMode::Pause => "Paused",
                    });
                    ui.add_space(12.0);
                    match state.menu_screen {
                        MenuScreen::Main => match state.menu_mode {
                            MenuMode::Title => {
                                if ui.button("Start").clicked() {
                                    output.start_requested = true;
                                }
                                if ui.button("Options").clicked() {
                                    state.menu_screen = MenuScreen::Options;
                                }
                                if ui.button("Quit").clicked() {
                                    output.quit_requested = true;
                                }
                            }
                            MenuMode::Pause => {
                                if ui.button("Return to Game").clicked() {
                                    output.resume_requested = true;
                                }
                                if ui.button("Options").clicked() {
                                    state.menu_screen = MenuScreen::Options;
                                }
                                if ui.button("Exit Game").clicked() {
                                    output.quit_requested = true;
                                }
                            }
                        },
                        MenuScreen::Options => {
                            ui.label("Options");
                            ui.add_space(6.0);
                            self.refresh_resolutions(settings.resolution);
                            let prev_mode = settings.window_mode;
                            egui::ComboBox::from_label("Display Mode")
                                .selected_text(settings.window_mode.label())
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut settings.window_mode,
                                        WindowMode::Windowed,
                                        WindowMode::Windowed.label(),
                                    );
                                    ui.selectable_value(
                                        &mut settings.window_mode,
                                        WindowMode::Borderless,
                                        WindowMode::Borderless.label(),
                                    );
                                    ui.selectable_value(
                                        &mut settings.window_mode,
                                        WindowMode::Fullscreen,
                                        WindowMode::Fullscreen.label(),
                                    );
                                });
                            let resolution_enabled = settings.window_mode != WindowMode::Borderless;
                            let prev_resolution = settings.resolution;
                            ui.add_enabled_ui(resolution_enabled, |ui| {
                                egui::ComboBox::from_label("Resolution")
                                    .selected_text(format_resolution(settings.resolution))
                                    .show_ui(ui, |ui| {
                                        for resolution in &self.available_resolutions {
                                            ui.selectable_value(
                                                &mut settings.resolution,
                                                *resolution,
                                                format_resolution(*resolution),
                                            );
                                        }
                                    });
                            });
                            if !resolution_enabled {
                                ui.label("Resolution uses the monitor size in borderless mode.");
                            }
                            let scale = ui
                                .add(
                                    egui::Slider::new(&mut settings.ui_scale, 0.75..=2.0)
                                        .text("UI Scale"),
                                )
                                .changed();
                            let vsync = ui
                                .checkbox(&mut settings.vsync, "VSync (restart required)")
                                .changed();
                            let mut volume_changed = false;
                            if ctx.audio_available {
                                volume_changed = ui
                                    .add(
                                        egui::Slider::new(&mut settings.master_volume, 0.0..=1.0)
                                            .text("Master Volume"),
                                    )
                                    .changed();
                            } else {
                                let mut stub_volume = 0.0;
                                ui.add_enabled(
                                    false,
                                    egui::Slider::new(&mut stub_volume, 0.0..=1.0)
                                        .text("Master Volume"),
                                );
                                ui.label("Master Volume (audio unavailable)");
                            }
                            if ui.button("Back").clicked() {
                                state.menu_screen = MenuScreen::Main;
                            }
                            let display_changed = prev_mode != settings.window_mode
                                || prev_resolution != settings.resolution;
                            output.settings_changed =
                                scale || vsync || volume_changed || display_changed;
                            output.display_settings_changed = display_changed;
                        }
                    }
                });
            });
        }
        output.wants_pointer = ctx.egui_ctx.wants_pointer_input();
        output.wants_keyboard = ctx.egui_ctx.wants_keyboard_input();
        self.output = output;
    }

    pub fn end_frame(&mut self, ctx: UiFrameContext) -> UiDrawData {
        let full_output = self.egui_ctx.end_frame();
        self.egui_state
            .handle_platform_output(self.window, full_output.platform_output);
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        UiDrawData {
            paint_jobs,
            textures_delta: full_output.textures_delta,
            screen_descriptor: ScreenDescriptor {
                size_in_pixels: ctx.resolution.physical_px,
                pixels_per_point: egui_winit::pixels_per_point(&self.egui_ctx, self.window),
            },
            output: self.output,
        }
    }

    pub fn render(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        view: &TextureView,
        draw_data: &UiDrawData,
        draw_ui: bool,
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
        if draw_ui && !draw_data.paint_jobs.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pallet.egui.pass"),
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

    fn refresh_resolutions(&mut self, include: [u32; 2]) {
        if self.available_resolutions.is_empty() {
            self.available_resolutions = collect_resolutions(self.window);
        }
        if !self.available_resolutions.contains(&include) {
            self.available_resolutions.push(include);
            self.available_resolutions.sort();
            self.available_resolutions.dedup();
        }
    }
}

fn collect_resolutions(window: &Window) -> Vec<[u32; 2]> {
    let mut set = BTreeSet::new();
    if let Some(monitor) = window
        .current_monitor()
        .or_else(|| window.primary_monitor())
    {
        for mode in monitor.video_modes() {
            let size = mode.size();
            if size.width > 0 && size.height > 0 {
                set.insert([size.width, size.height]);
            }
        }
    }
    if set.is_empty() {
        for resolution in FALLBACK_RESOLUTIONS {
            set.insert(*resolution);
        }
    }
    set.into_iter().collect()
}

fn format_resolution(resolution: [u32; 2]) -> String {
    format!("{}x{}", resolution[0], resolution[1])
}
