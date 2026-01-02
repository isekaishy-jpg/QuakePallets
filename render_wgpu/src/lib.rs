#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::fmt;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

pub use wgpu::SurfaceError as RenderError;

#[derive(Debug)]
pub enum RenderInitError {
    Surface(wgpu::CreateSurfaceError),
    NoAdapter,
    RequestDevice(wgpu::RequestDeviceError),
}

impl fmt::Display for RenderInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderInitError::Surface(err) => write!(f, "surface creation failed: {}", err),
            RenderInitError::NoAdapter => write!(f, "no suitable GPU adapter found"),
            RenderInitError::RequestDevice(err) => write!(f, "request device failed: {}", err),
        }
    }
}

impl std::error::Error for RenderInitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderInitError::Surface(err) => Some(err),
            RenderInitError::RequestDevice(err) => Some(err),
            RenderInitError::NoAdapter => None,
        }
    }
}

#[derive(Debug)]
pub enum ImageError {
    InvalidDimensions { width: u32, height: u32 },
    SizeOverflow,
    DataSizeMismatch { expected: usize, actual: usize },
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::InvalidDimensions { width, height } => {
                write!(f, "invalid image dimensions: {}x{}", width, height)
            }
            ImageError::SizeOverflow => write!(f, "image size overflow"),
            ImageError::DataSizeMismatch { expected, actual } => write!(
                f,
                "image data size mismatch: expected {} bytes, got {}",
                expected, actual
            ),
        }
    }
}

impl std::error::Error for ImageError {}

#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ImageData {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::InvalidDimensions { width, height });
        }
        let expected = image_data_len(width, height)?;
        if rgba.len() != expected {
            return Err(ImageError::DataSizeMismatch {
                expected,
                actual: rgba.len(),
            });
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }
}

#[derive(Debug)]
pub enum SceneError {
    EmptyMesh,
    IndexOutOfBounds { index: u32, vertex_count: u32 },
    SizeOverflow,
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SceneError::EmptyMesh => write!(f, "mesh is empty"),
            SceneError::IndexOutOfBounds {
                index,
                vertex_count,
            } => write!(
                f,
                "mesh index out of bounds: {} (vertex count {})",
                index, vertex_count
            ),
            SceneError::SizeOverflow => write!(f, "mesh size overflow"),
        }
    }
}

impl std::error::Error for SceneError {}

#[derive(Debug, Clone)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

#[derive(Debug, Clone)]
pub struct MeshData {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn new(vertices: Vec<MeshVertex>, indices: Vec<u32>) -> Result<Self, SceneError> {
        if vertices.is_empty() || indices.is_empty() {
            return Err(SceneError::EmptyMesh);
        }
        let vertex_count = u32::try_from(vertices.len()).map_err(|_| SceneError::SizeOverflow)?;
        for &index in &indices {
            if index >= vertex_count {
                return Err(SceneError::IndexOutOfBounds {
                    index,
                    vertex_count,
                });
            }
        }
        Ok(Self { vertices, indices })
    }
}

pub struct Renderer<'window> {
    window: &'window winit::window::Window,
    surface: wgpu::Surface<'window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    clear_color: wgpu::Color,
    textured_quad: Option<TexturedQuad>,
    scene: Option<SceneRenderer>,
}

impl<'window> Renderer<'window> {
    pub fn new(window: &'window winit::window::Window) -> Result<Self, RenderInitError> {
        pollster::block_on(Self::new_async(window))
    }

    pub fn window_id(&self) -> winit::window::WindowId {
        self.window.id()
    }

    pub fn window(&self) -> &winit::window::Window {
        self.window
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    pub fn window_inner_size(&self) -> PhysicalSize<u32> {
        self.window.inner_size()
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        if let Some(scene) = &mut self.scene {
            scene.resize(&self.device, &self.config);
        }
    }

    pub fn render(&mut self) -> Result<(), RenderError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pallet.render.encoder"),
            });
        if let Some(scene) = &self.scene {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pallet.render.scene.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: scene.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            scene.draw(&mut pass);
        } else {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pallet.render.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            if let Some(quad) = &self.textured_quad {
                quad.draw(&mut pass);
            }
        }

        if self.scene.is_some() {
            if let Some(quad) = &self.textured_quad {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("pallet.render.overlay.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
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
                quad.draw(&mut pass);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    pub fn set_image(&mut self, image: ImageData) -> Result<(), ImageError> {
        let quad = TexturedQuad::new(&self.device, &self.queue, &self.config, &image)?;
        self.textured_quad = Some(quad);
        Ok(())
    }

    pub fn set_scene(&mut self, mesh: MeshData) -> Result<(), SceneError> {
        let scene = SceneRenderer::new(&self.device, &self.config, &mesh)?;
        self.scene = Some(scene);
        Ok(())
    }

    pub fn update_camera(&mut self, view_proj: [[f32; 4]; 4]) {
        if let Some(scene) = &self.scene {
            scene.update_camera(&self.queue, view_proj);
        }
    }

    async fn new_async(window: &'window winit::window::Window) -> Result<Self, RenderInitError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance
            .create_surface(window)
            .map_err(RenderInitError::Surface)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RenderInitError::NoAdapter)?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("pallet.device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(RenderInitError::RequestDevice)?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .first()
            .copied()
            .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);
        let present_mode = caps
            .present_modes
            .first()
            .copied()
            .unwrap_or(wgpu::PresentMode::Fifo);
        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            size,
            clear_color: wgpu::Color {
                r: 0.04,
                g: 0.05,
                b: 0.06,
                a: 1.0,
            },
            textured_quad: None,
            scene: None,
        })
    }
}

struct TexturedQuad {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

impl TexturedQuad {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: &wgpu::SurfaceConfiguration,
        image: &ImageData,
    ) -> Result<Self, ImageError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pallet.quad.shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(QUAD_SHADER)),
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pallet.quad.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pallet.quad.texture"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pallet.quad.bind_group_layout"),
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pallet.quad.bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let vertex_data = quad_vertex_bytes();
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.quad.vertex_buffer"),
            contents: &vertex_data,
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_data = quad_index_bytes();
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.quad.index_buffer"),
            contents: &index_data,
            usage: wgpu::BufferUsages::INDEX,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pallet.quad.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pallet.quad.pipeline"),
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
                    format: config.format,
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

        upload_texture(queue, &texture, image)?;

        Ok(Self {
            pipeline,
            bind_group,
            vertex_buffer,
            index_buffer,
            index_count: 6,
        })
    }

    fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

impl SceneRenderer {
    fn new(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        mesh: &MeshData,
    ) -> Result<Self, SceneError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pallet.scene.shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SCENE_SHADER)),
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pallet.scene.camera_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.scene.camera_buffer"),
            contents: &identity_matrix_bytes(),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pallet.scene.camera_bind_group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pallet.scene.pipeline_layout"),
            bind_group_layouts: &[&camera_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pallet.scene.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 24,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let vertex_bytes = mesh_vertex_bytes(&mesh.vertices)?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.scene.vertex_buffer"),
            contents: &vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_bytes = mesh_index_bytes(&mesh.indices)?;
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.scene.index_buffer"),
            contents: &index_bytes,
            usage: wgpu::BufferUsages::INDEX,
        });

        let index_count =
            u32::try_from(mesh.indices.len()).map_err(|_| SceneError::SizeOverflow)?;
        let (depth_texture, depth_view) = create_depth_texture(device, config);

        Ok(Self {
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count,
            camera_buffer,
            camera_bind_group,
            depth_texture,
            depth_view,
        })
    }

    fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }

    fn update_camera(&self, queue: &wgpu::Queue, view_proj: [[f32; 4]; 4]) {
        let bytes = matrix_bytes(view_proj);
        queue.write_buffer(&self.camera_buffer, 0, &bytes);
    }

    fn resize(&mut self, device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) {
        let (depth_texture, depth_view) = create_depth_texture(device, config);
        self.depth_texture = depth_texture;
        self.depth_view = depth_view;
    }

    fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }
}

const QUAD_SHADER: &str = r#"
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

const SCENE_SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
}

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) color: vec3<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = camera.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
"#;

fn image_data_len(width: u32, height: u32) -> Result<usize, ImageError> {
    let width = usize::try_from(width).map_err(|_| ImageError::SizeOverflow)?;
    let height = usize::try_from(height).map_err(|_| ImageError::SizeOverflow)?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ImageError::SizeOverflow)
}

fn quad_vertex_bytes() -> Vec<u8> {
    let vertices = [
        [-1.0f32, 1.0f32, 0.0f32, 0.0f32],
        [1.0f32, 1.0f32, 1.0f32, 0.0f32],
        [1.0f32, -1.0f32, 1.0f32, 1.0f32],
        [-1.0f32, -1.0f32, 0.0f32, 1.0f32],
    ];
    let mut bytes = Vec::with_capacity(vertices.len() * 16);
    for vertex in vertices {
        for value in vertex {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

fn quad_index_bytes() -> Vec<u8> {
    let indices = [0u16, 1, 2, 0, 2, 3];
    let mut bytes = Vec::with_capacity(indices.len() * 2);
    for index in indices {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    bytes
}

fn mesh_vertex_bytes(vertices: &[MeshVertex]) -> Result<Vec<u8>, SceneError> {
    let mut bytes = Vec::with_capacity(vertices.len().saturating_mul(24));
    for vertex in vertices {
        for value in vertex.position {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in vertex.color {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(bytes)
}

fn mesh_index_bytes(indices: &[u32]) -> Result<Vec<u8>, SceneError> {
    let mut bytes = Vec::with_capacity(indices.len().saturating_mul(4));
    for index in indices {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    Ok(bytes)
}

fn identity_matrix_bytes() -> Vec<u8> {
    matrix_bytes([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn matrix_bytes(matrix: [[f32; 4]; 4]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    for col in matrix {
        for value in col {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

fn create_depth_texture(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pallet.scene.depth_texture"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth24Plus,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn upload_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    image: &ImageData,
) -> Result<(), ImageError> {
    let row_bytes = usize::try_from(image.width).map_err(|_| ImageError::SizeOverflow)?;
    let row_bytes = row_bytes.checked_mul(4).ok_or(ImageError::SizeOverflow)?;
    let padded = align_to(row_bytes, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)?;

    let height = usize::try_from(image.height).map_err(|_| ImageError::SizeOverflow)?;
    let data = if padded == row_bytes {
        image.rgba.clone()
    } else {
        let mut padded_data = vec![0u8; padded * height];
        for row in 0..height {
            let src_start = row * row_bytes;
            let src_end = src_start + row_bytes;
            let dst_start = row * padded;
            padded_data[dst_start..dst_start + row_bytes]
                .copy_from_slice(&image.rgba[src_start..src_end]);
        }
        padded_data
    };

    let bytes_per_row = u32::try_from(padded).map_err(|_| ImageError::SizeOverflow)?;
    let rows_per_image = u32::try_from(height).map_err(|_| ImageError::SizeOverflow)?;

    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(rows_per_image),
        },
        wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
    );

    Ok(())
}

fn align_to(value: usize, alignment: usize) -> Result<usize, ImageError> {
    if alignment == 0 {
        return Err(ImageError::SizeOverflow);
    }
    let add = alignment.saturating_sub(1);
    let sum = value.checked_add(add).ok_or(ImageError::SizeOverflow)?;
    Ok(sum / alignment * alignment)
}
