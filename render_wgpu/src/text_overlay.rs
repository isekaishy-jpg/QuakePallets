use wgpu::RenderPass;

#[derive(Clone, Copy, Debug)]
pub enum TextLayer {
    Hud,
    Console,
    Ui,
}

#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    pub font_size: f32,
    pub color: [f32; 4],
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TextPosition {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct TextBounds {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct TextViewport {
    pub physical_px: [u32; 2],
    pub dpi_scale: f32,
    pub ui_scale: f32,
}

#[derive(Clone, Debug)]
struct QueuedText {
    layer: TextLayer,
    style: TextStyle,
    position: TextPosition,
    bounds: TextBounds,
    text: String,
}

#[derive(Default)]
pub struct TextOverlay {
    queued: Vec<QueuedText>,
}

impl TextOverlay {
    pub fn new() -> Self {
        Self { queued: Vec::new() }
    }

    pub fn queue(
        &mut self,
        layer: TextLayer,
        style: TextStyle,
        position: TextPosition,
        bounds: TextBounds,
        text: impl Into<String>,
    ) {
        self.queued.push(QueuedText {
            layer,
            style,
            position,
            bounds,
            text: text.into(),
        });
    }

    pub fn flush<'pass>(&'pass mut self, _pass: &mut RenderPass<'pass>, _viewport: TextViewport) {
        for item in self.queued.drain(..) {
            let _ = (
                item.layer,
                item.style,
                item.position,
                item.bounds,
                item.text,
            );
        }
    }
}
