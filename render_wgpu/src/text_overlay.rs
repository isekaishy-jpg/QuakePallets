use std::sync::Arc;

use glyphon::{
    fontdb, Attrs, Buffer, Color, Family, FontSystem, Metrics, Resolution as GlyphonResolution,
    Shaping, SwashCache, TextArea, TextAtlas, TextBounds as GlyphonBounds, TextRenderer,
};
use wgpu::{
    Buffer as WgpuBuffer, BufferDescriptor, BufferUsages, Device, FragmentState, MultisampleState,
    PipelineLayoutDescriptor, PrimitiveState, Queue, RenderPass, RenderPipeline,
    RenderPipelineDescriptor, ShaderModuleDescriptor, ShaderSource, TextureFormat, VertexAttribute,
    VertexBufferLayout, VertexFormat, VertexState,
};

const LINE_HEIGHT_SCALE: f32 = 1.2;

const FIRA_SANS_REGULAR: &[u8] =
    include_bytes!("../../third_party/fonts/FiraSans/FiraSans-Regular.ttf");
const FIRA_SANS_ITALIC: &[u8] =
    include_bytes!("../../third_party/fonts/FiraSans/FiraSans-Italic.ttf");
const FIRA_SANS_BOLD: &[u8] = include_bytes!("../../third_party/fonts/FiraSans/FiraSans-Bold.ttf");

const RECT_SHADER: &str = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(input.pos, 0.0, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug)]
struct QueuedRect {
    layer: TextLayer,
    position: TextPosition,
    bounds: TextBounds,
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct RectVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

pub struct TextOverlay {
    font_system: FontSystem,
    cache: SwashCache,
    atlas: TextAtlas,
    renderer: TextRenderer,
    queued: Vec<QueuedText>,
    rects: Vec<QueuedRect>,
    buffers: Vec<Buffer>,
    rect_pipeline: Arc<RenderPipeline>,
    rect_vertex_buffer: Arc<WgpuBuffer>,
    rect_vertex_capacity: u64,
}

impl TextOverlay {
    pub fn new(device: &Device, queue: &Queue, format: TextureFormat) -> Self {
        let mut font_system = FontSystem::new();
        let db = font_system.db_mut();
        db.load_font_source(fontdb::Source::Binary(Arc::new(FIRA_SANS_REGULAR.to_vec())));
        db.load_font_source(fontdb::Source::Binary(Arc::new(FIRA_SANS_ITALIC.to_vec())));
        db.load_font_source(fontdb::Source::Binary(Arc::new(FIRA_SANS_BOLD.to_vec())));

        let mut atlas = TextAtlas::new(device, queue, format);
        let renderer = TextRenderer::new(&mut atlas, device, MultisampleState::default(), None);

        let rect_pipeline = Arc::new(create_rect_pipeline(device, format));
        let rect_vertex_capacity = 1024;
        let rect_vertex_buffer = Arc::new(device.create_buffer(&BufferDescriptor {
            label: Some("pallet.text_overlay.rect_vertices"),
            size: rect_vertex_capacity,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));

        Self {
            font_system,
            cache: SwashCache::new(),
            atlas,
            renderer,
            queued: Vec::new(),
            rects: Vec::new(),
            buffers: Vec::new(),
            rect_pipeline,
            rect_vertex_buffer,
            rect_vertex_capacity,
        }
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

    pub fn queue_rect(
        &mut self,
        layer: TextLayer,
        position: TextPosition,
        bounds: TextBounds,
        color: [f32; 4],
    ) {
        self.rects.push(QueuedRect {
            layer,
            position,
            bounds,
            color,
        });
    }

    pub fn flush<'pass>(
        &'pass mut self,
        pass: &mut RenderPass<'pass>,
        viewport: TextViewport,
        device: &Device,
        queue: &Queue,
    ) {
        self.flush_layers(
            pass,
            viewport,
            device,
            queue,
            &[TextLayer::Hud, TextLayer::Console, TextLayer::Ui],
        );
    }

    pub fn flush_layers<'pass>(
        &'pass mut self,
        pass: &mut RenderPass<'pass>,
        viewport: TextViewport,
        device: &Device,
        queue: &Queue,
        layers: &[TextLayer],
    ) {
        if viewport.physical_px[0] == 0 || viewport.physical_px[1] == 0 {
            self.queued.clear();
            self.rects.clear();
            return;
        }

        let mut queued = Vec::new();
        let mut remaining = Vec::with_capacity(self.queued.len());
        for item in self.queued.drain(..) {
            if layers.contains(&item.layer) {
                queued.push(item);
            } else {
                remaining.push(item);
            }
        }
        self.queued = remaining;

        let mut rects = Vec::new();
        let mut remaining_rects = Vec::with_capacity(self.rects.len());
        for rect in self.rects.drain(..) {
            if layers.contains(&rect.layer) {
                rects.push(rect);
            } else {
                remaining_rects.push(rect);
            }
        }
        self.rects = remaining_rects;

        if queued.is_empty() && rects.is_empty() {
            return;
        }

        let mut rect_vertex_count = 0u32;
        if !rects.is_empty() {
            let mut vertices = Vec::with_capacity(rects.len() * 6);
            for rect in &rects {
                vertices.extend_from_slice(&rect_vertices(rect, viewport.physical_px));
            }
            let bytes = rect_vertex_bytes(&vertices);
            let byte_len = bytes.len() as u64;
            if byte_len > self.rect_vertex_capacity {
                let new_size = next_buffer_size(byte_len);
                self.rect_vertex_buffer = Arc::new(device.create_buffer(&BufferDescriptor {
                    label: Some("pallet.text_overlay.rect_vertices"),
                    size: new_size,
                    usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.rect_vertex_capacity = new_size;
            }
            queue.write_buffer(&self.rect_vertex_buffer, 0, &bytes);
            rect_vertex_count = vertices.len() as u32;
        }

        queued.sort_by(|a, b| {
            layer_order(a.layer)
                .cmp(&layer_order(b.layer))
                .then_with(|| a.position.y.total_cmp(&b.position.y))
        });

        self.buffers.clear();
        self.buffers.reserve(queued.len());

        for item in &queued {
            let font_size = item.style.font_size.max(1.0);
            let metrics = Metrics::new(font_size, font_size * LINE_HEIGHT_SCALE);
            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            let width = item.bounds.width.max(1.0);
            let height = item.bounds.height.max(1.0);
            buffer.set_size(&mut self.font_system, width, height);

            let attrs = Attrs::new()
                .family(Family::SansSerif)
                .color(color_from_f32(item.style.color))
                .metadata(layer_order(item.layer) as usize);
            buffer.set_text(&mut self.font_system, &item.text, attrs, Shaping::Advanced);

            self.buffers.push(buffer);
        }

        let mut text_areas = Vec::with_capacity(queued.len());
        for (item, buffer_ref) in queued.iter().zip(self.buffers.iter()) {
            let width = item.bounds.width.max(1.0);
            let height = item.bounds.height.max(1.0);
            let bounds = GlyphonBounds {
                left: item.position.x as i32,
                top: item.position.y as i32,
                right: (item.position.x + width) as i32,
                bottom: (item.position.y + height) as i32,
            };

            text_areas.push(TextArea {
                buffer: buffer_ref,
                left: item.position.x,
                top: item.position.y,
                scale: 1.0,
                bounds,
                default_color: color_from_f32(item.style.color),
            });
        }

        let mut text_ready = false;
        if !text_areas.is_empty() {
            let resolution = GlyphonResolution {
                width: viewport.physical_px[0],
                height: viewport.physical_px[1],
            };
            if self
                .renderer
                .prepare(
                    device,
                    queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    resolution,
                    text_areas,
                    &mut self.cache,
                )
                .is_ok()
            {
                text_ready = true;
            }
        }

        self.buffers.clear();

        if rect_vertex_count > 0 {
            pass.set_pipeline(&self.rect_pipeline);
            pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
            pass.draw(0..rect_vertex_count, 0..1);
        }
        if text_ready {
            let _ = self.renderer.render(&self.atlas, pass);
        }
    }
}

fn create_rect_pipeline(device: &Device, format: TextureFormat) -> RenderPipeline {
    let shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("pallet.text_overlay.rect_shader"),
        source: ShaderSource::Wgsl(RECT_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("pallet.text_overlay.rect_pipeline_layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("pallet.text_overlay.rect_pipeline"),
        layout: Some(&layout),
        vertex: VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[VertexBufferLayout {
                array_stride: std::mem::size_of::<RectVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    VertexAttribute {
                        format: VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    VertexAttribute {
                        format: VertexFormat::Float32x4,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview: None,
    })
}

fn layer_order(layer: TextLayer) -> u8 {
    match layer {
        TextLayer::Hud => 0,
        TextLayer::Ui => 1,
        TextLayer::Console => 2,
    }
}

fn rect_vertices(rect: &QueuedRect, resolution: [u32; 2]) -> [RectVertex; 6] {
    let width = resolution[0].max(1) as f32;
    let height = resolution[1].max(1) as f32;
    let left = rect.position.x;
    let top = rect.position.y;
    let right = rect.position.x + rect.bounds.width;
    let bottom = rect.position.y + rect.bounds.height;
    let (x0, y0) = to_ndc(left, top, width, height);
    let (x1, y1) = to_ndc(right, bottom, width, height);
    let color = rect.color;
    [
        RectVertex {
            pos: [x0, y0],
            color,
        },
        RectVertex {
            pos: [x1, y0],
            color,
        },
        RectVertex {
            pos: [x1, y1],
            color,
        },
        RectVertex {
            pos: [x0, y0],
            color,
        },
        RectVertex {
            pos: [x1, y1],
            color,
        },
        RectVertex {
            pos: [x0, y1],
            color,
        },
    ]
}

fn to_ndc(x: f32, y: f32, width: f32, height: f32) -> (f32, f32) {
    let nx = (x / width) * 2.0 - 1.0;
    let ny = 1.0 - (y / height) * 2.0;
    (nx, ny)
}

fn rect_vertex_bytes(vertices: &[RectVertex]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vertices));
    for vertex in vertices {
        for value in vertex.pos {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in vertex.color {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

fn next_buffer_size(size: u64) -> u64 {
    let base = size.next_power_of_two();
    base.max(256)
}

fn color_from_f32(color: [f32; 4]) -> Color {
    let clamp = |value: f32| -> u8 { (value.clamp(0.0, 1.0) * 255.0).round() as u8 };
    Color::rgba(
        clamp(color[0]),
        clamp(color[1]),
        clamp(color[2]),
        clamp(color[3]),
    )
}
