#![forbid(unsafe_code)]

use std::fmt;

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

pub struct Renderer<'window> {
    window: &'window winit::window::Window,
    surface: wgpu::Surface<'window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    clear_color: wgpu::Color,
}

impl<'window> Renderer<'window> {
    pub fn new(window: &'window winit::window::Window) -> Result<Self, RenderInitError> {
        pollster::block_on(Self::new_async(window))
    }

    pub fn window_id(&self) -> winit::window::WindowId {
        self.window.id()
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
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
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
        })
    }
}
