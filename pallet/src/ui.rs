use platform_winit::PhysicalSize;

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
}

#[derive(Clone, Copy, Debug)]
pub struct UiFrameContext {
    pub dt_seconds: f32,
    pub resolution: ResolutionModel,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub menu_open: bool,
    pub console_open: bool,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub ui_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { ui_scale: 1.0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UiDrawData;

#[derive(Default)]
pub struct UiFacade;

impl UiFacade {
    pub fn new() -> Self {
        Self
    }

    pub fn begin_frame(&mut self, input: UiFrameInput) -> UiFrameContext {
        UiFrameContext {
            dt_seconds: input.dt_seconds,
            resolution: input.resolution,
        }
    }

    pub fn build_ui(
        &mut self,
        ctx: &mut UiFrameContext,
        state: &mut UiState,
        settings: &mut Settings,
    ) {
        let _ = (
            ctx.dt_seconds,
            ctx.resolution,
            state.menu_open,
            state.console_open,
            settings.ui_scale,
        );
    }

    pub fn end_frame(&mut self, _ctx: UiFrameContext) -> UiDrawData {
        UiDrawData
    }
}
