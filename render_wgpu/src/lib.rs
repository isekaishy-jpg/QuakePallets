#![forbid(unsafe_code)]

use std::any::Any;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

pub use text_overlay::{
    TextBounds, TextFontSystem, TextLayer, TextOverlay, TextOverlayTimings, TextPosition, TextSpan,
    TextStyle, TextViewport,
};
pub use wgpu::SurfaceError as RenderError;

mod text_overlay;

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
    InvalidDimensions {
        width: u32,
        height: u32,
    },
    SizeOverflow,
    DataSizeMismatch {
        expected: usize,
        actual: usize,
    },
    PlaneSizeMismatch {
        plane: &'static str,
        expected: usize,
        actual: usize,
    },
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
            ImageError::PlaneSizeMismatch {
                plane,
                expected,
                actual,
            } => write!(
                f,
                "image {} plane size mismatch: expected {} bytes, got {}",
                plane, expected, actual
            ),
        }
    }
}

#[derive(Debug)]
pub enum CaptureError {
    SizeMismatch {
        expected: [u32; 2],
        actual: [u32; 2],
    },
    UnsupportedFormat(wgpu::TextureFormat),
    MapFailed,
    BufferOverflow,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::SizeMismatch { expected, actual } => write!(
                f,
                "capture size mismatch (expected {}x{}, got {}x{})",
                expected[0], expected[1], actual[0], actual[1]
            ),
            CaptureError::UnsupportedFormat(format) => {
                write!(f, "capture unsupported format: {:?}", format)
            }
            CaptureError::MapFailed => write!(f, "capture buffer map failed"),
            CaptureError::BufferOverflow => write!(f, "capture buffer size overflow"),
        }
    }
}

impl std::error::Error for CaptureError {}

#[derive(Debug)]
pub enum RenderCaptureError {
    Surface(RenderError),
    Capture(CaptureError),
}

impl fmt::Display for RenderCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderCaptureError::Surface(err) => write!(f, "render error: {}", err),
            RenderCaptureError::Capture(err) => write!(f, "capture error: {}", err),
        }
    }
}

impl std::error::Error for RenderCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderCaptureError::Surface(err) => Some(err),
            RenderCaptureError::Capture(err) => Some(err),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadStatus {
    Queued,
    Uploading,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadPriority {
    High,
    Normal,
    Low,
}

#[derive(Clone, Copy, Debug)]
pub struct UploadLimits {
    pub jobs_per_frame: usize,
    pub bytes_per_frame: u64,
}

impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            jobs_per_frame: 8,
            bytes_per_frame: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct UploadQueueMetrics {
    pub queued_jobs: usize,
    pub queued_bytes: u64,
    pub last_drain_ms: Option<u64>,
    pub last_drain_jobs: usize,
    pub last_drain_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct UploadDrainStats {
    pub drained_jobs: usize,
    pub drained_bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Clone)]
pub struct UploadQueue {
    inner: Arc<UploadQueueInner>,
}

struct UploadQueueInner {
    state: Mutex<UploadQueueState>,
}

struct UploadQueueState {
    pending_high: VecDeque<UploadJob>,
    pending_normal: VecDeque<UploadJob>,
    pending_low: VecDeque<UploadJob>,
    queued_bytes: u64,
    last_drain_ms: Option<u64>,
    last_drain_jobs: usize,
    last_drain_bytes: u64,
    limits: UploadLimits,
}

impl UploadQueueState {
    fn new() -> Self {
        Self {
            pending_high: VecDeque::new(),
            pending_normal: VecDeque::new(),
            pending_low: VecDeque::new(),
            queued_bytes: 0,
            last_drain_ms: None,
            last_drain_jobs: 0,
            last_drain_bytes: 0,
            limits: UploadLimits::default(),
        }
    }

    fn queued_jobs(&self) -> usize {
        self.pending_high.len() + self.pending_normal.len() + self.pending_low.len()
    }

    fn pop_next(&mut self) -> Option<UploadJob> {
        if let Some(job) = self.pending_high.pop_front() {
            return Some(job);
        }
        if let Some(job) = self.pending_normal.pop_front() {
            return Some(job);
        }
        self.pending_low.pop_front()
    }

    fn push_job(&mut self, job: UploadJob) {
        match job.priority {
            UploadPriority::High => self.pending_high.push_back(job),
            UploadPriority::Normal => self.pending_normal.push_back(job),
            UploadPriority::Low => self.pending_low.push_back(job),
        }
    }
}

impl UploadQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(UploadQueueInner {
                state: Mutex::new(UploadQueueState::new()),
            }),
        }
    }

    pub fn set_limits(&self, limits: UploadLimits) {
        let mut state = self.inner.state.lock().expect("upload queue lock poisoned");
        state.limits = limits;
    }

    pub fn metrics(&self) -> UploadQueueMetrics {
        let state = self.inner.state.lock().expect("upload queue lock poisoned");
        UploadQueueMetrics {
            queued_jobs: state.queued_jobs(),
            queued_bytes: state.queued_bytes,
            last_drain_ms: state.last_drain_ms,
            last_drain_jobs: state.last_drain_jobs,
            last_drain_bytes: state.last_drain_bytes,
        }
    }

    pub fn enqueue_image(
        &self,
        image: ImageData,
        priority: UploadPriority,
    ) -> UploadHandle<UploadedImage> {
        let bytes = image.rgba.len() as u64;
        let slot = Arc::new(UploadSlot::new());
        let job = UploadJob {
            slot: Arc::clone(&slot),
            priority,
            bytes,
            payload: UploadPayload::Image { image },
        };

        let mut state = self.inner.state.lock().expect("upload queue lock poisoned");
        state.push_job(job);
        state.queued_bytes = state.queued_bytes.saturating_add(bytes);

        UploadHandle {
            slot,
            marker: PhantomData,
        }
    }

    pub fn drain(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: &wgpu::SurfaceConfiguration,
    ) -> UploadDrainStats {
        let start = Instant::now();
        let mut drained_jobs = 0usize;
        let mut drained_bytes = 0u64;

        loop {
            let job = {
                let mut state = self.inner.state.lock().expect("upload queue lock poisoned");
                let Some(job) = state.pop_next() else {
                    break;
                };
                let limits = state.limits;
                if limits.jobs_per_frame == 0 {
                    state.push_job(job);
                    break;
                }
                if drained_jobs >= limits.jobs_per_frame {
                    state.push_job(job);
                    break;
                }
                if limits.bytes_per_frame == 0 {
                    state.push_job(job);
                    break;
                }
                if drained_jobs > 0
                    && drained_bytes.saturating_add(job.bytes) > limits.bytes_per_frame
                {
                    state.push_job(job);
                    break;
                }
                state.queued_bytes = state.queued_bytes.saturating_sub(job.bytes);
                job
            };

            job.slot.mark_uploading();
            let upload_start = Instant::now();
            let result = match job.payload {
                UploadPayload::Image { image } => UploadedImage::new(device, queue, config, &image)
                    .map(Arc::new)
                    .map_err(|err| err.to_string()),
            };

            let upload_ms = upload_start.elapsed().as_millis() as u64;
            match result {
                Ok(uploaded) => job.slot.finish(uploaded, upload_ms),
                Err(err) => job.slot.fail(&err),
            }

            drained_jobs = drained_jobs.saturating_add(1);
            drained_bytes = drained_bytes.saturating_add(job.bytes);
        }

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let mut state = self.inner.state.lock().expect("upload queue lock poisoned");
        state.last_drain_ms = Some(elapsed_ms);
        state.last_drain_jobs = drained_jobs;
        state.last_drain_bytes = drained_bytes;

        UploadDrainStats {
            drained_jobs,
            drained_bytes,
            elapsed_ms,
        }
    }
}

impl Default for UploadQueue {
    fn default() -> Self {
        Self::new()
    }
}

pub struct UploadHandle<T> {
    slot: Arc<UploadSlot>,
    marker: PhantomData<T>,
}

impl<T> UploadHandle<T> {
    pub fn status(&self) -> UploadStatus {
        let guard = self.slot.state.lock().expect("upload slot lock poisoned");
        guard.status
    }

    pub fn get(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let guard = self.slot.state.lock().expect("upload slot lock poisoned");
        let value = guard.value.as_ref()?;
        let value = Arc::clone(value);
        Arc::downcast::<T>(value).ok()
    }

    pub fn error(&self) -> Option<String> {
        let guard = self.slot.state.lock().expect("upload slot lock poisoned");
        guard.error.clone()
    }
}

pub struct UploadedImage {
    quad: Arc<TexturedQuad>,
    width: u32,
    height: u32,
}

impl UploadedImage {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: &wgpu::SurfaceConfiguration,
        image: &ImageData,
    ) -> Result<Self, ImageError> {
        let quad = Arc::new(TexturedQuad::new(device, queue, config, image)?);
        Ok(Self {
            quad,
            width: image.width,
            height: image.height,
        })
    }

    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }
}

struct UploadSlot {
    state: Mutex<UploadSlotState>,
}

struct UploadSlotState {
    status: UploadStatus,
    value: Option<Arc<dyn Any + Send + Sync>>,
    error: Option<String>,
    upload_ms: Option<u64>,
}

impl UploadSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(UploadSlotState {
                status: UploadStatus::Queued,
                value: None,
                error: None,
                upload_ms: None,
            }),
        }
    }

    fn mark_uploading(&self) {
        let mut guard = self.state.lock().expect("upload slot lock poisoned");
        guard.status = UploadStatus::Uploading;
    }

    fn finish(&self, value: Arc<dyn Any + Send + Sync>, upload_ms: u64) {
        let mut guard = self.state.lock().expect("upload slot lock poisoned");
        guard.status = UploadStatus::Ready;
        guard.value = Some(value);
        guard.upload_ms = Some(upload_ms);
    }

    fn fail(&self, message: &str) {
        let mut guard = self.state.lock().expect("upload slot lock poisoned");
        guard.status = UploadStatus::Failed;
        guard.error = Some(message.to_string());
        guard.upload_ms = Some(0);
    }
}

struct UploadJob {
    slot: Arc<UploadSlot>,
    priority: UploadPriority,
    bytes: u64,
    payload: UploadPayload,
}

enum UploadPayload {
    Image { image: ImageData },
}

pub struct FrameCapture {
    size: [u32; 2],
    format: wgpu::TextureFormat,
    bytes_per_row: u32,
    padded_bytes_per_row: u32,
    buffer: wgpu::Buffer,
}

impl FrameCapture {
    pub fn new(
        device: &wgpu::Device,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) -> Result<Self, CaptureError> {
        let bytes_per_row = size[0].checked_mul(4).ok_or(CaptureError::BufferOverflow)?;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = bytes_per_row.div_ceil(align) * align;
        let buffer_size = (padded_bytes_per_row as u64)
            .checked_mul(size[1] as u64)
            .ok_or(CaptureError::BufferOverflow)?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pallet.frame_capture.buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok(Self {
            size,
            format,
            bytes_per_row,
            padded_bytes_per_row,
            buffer,
        })
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> Result<(), CaptureError> {
        let texture_size = texture.size();
        let actual = [texture_size.width, texture_size.height];
        if actual != self.size {
            return Err(CaptureError::SizeMismatch {
                expected: self.size,
                actual,
            });
        }
        let layout = wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(self.padded_bytes_per_row),
            rows_per_image: Some(self.size[1]),
        };
        let copy_size = wgpu::Extent3d {
            width: self.size[0],
            height: self.size[1],
            depth_or_array_layers: 1,
        };
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.buffer,
                layout,
            },
            copy_size,
        );
        Ok(())
    }

    pub fn read_rgba(&self, device: &wgpu::Device) -> Result<Vec<u8>, CaptureError> {
        let buffer_slice = self.buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device.poll(wgpu::Maintain::Wait);
        match receiver.recv() {
            Ok(Ok(())) => {}
            _ => return Err(CaptureError::MapFailed),
        }
        let mapped = buffer_slice.get_mapped_range();
        let mut rgba =
            vec![0u8; (self.bytes_per_row as usize).saturating_mul(self.size[1] as usize)];
        for y in 0..self.size[1] {
            let src = (y * self.padded_bytes_per_row) as usize;
            let dst = (y * self.bytes_per_row) as usize;
            let row = &mapped[src..src + self.bytes_per_row as usize];
            rgba[dst..dst + self.bytes_per_row as usize].copy_from_slice(row);
        }
        drop(mapped);
        self.buffer.unmap();

        match self.format {
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {}
            other => return Err(CaptureError::UnsupportedFormat(other)),
        }

        Ok(rgba)
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

#[derive(Debug, Clone)]
pub struct YuvImageData {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl YuvImageData {
    pub fn new(
        width: u32,
        height: u32,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    ) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::InvalidDimensions { width, height });
        }
        let y_expected = plane_len(width, height)?;
        if y.len() != y_expected {
            return Err(ImageError::PlaneSizeMismatch {
                plane: "y",
                expected: y_expected,
                actual: y.len(),
            });
        }
        let uv_width = width.div_ceil(2);
        let uv_height = height.div_ceil(2);
        let uv_expected = plane_len(uv_width, uv_height)?;
        if u.len() != uv_expected {
            return Err(ImageError::PlaneSizeMismatch {
                plane: "u",
                expected: uv_expected,
                actual: u.len(),
            });
        }
        if v.len() != uv_expected {
            return Err(ImageError::PlaneSizeMismatch {
                plane: "v",
                expected: uv_expected,
                actual: v.len(),
            });
        }
        Ok(Self {
            width,
            height,
            y,
            u,
            v,
        })
    }

    pub fn as_view(&self) -> YuvImageView<'_> {
        YuvImageView {
            width: self.width,
            height: self.height,
            y: &self.y,
            u: &self.u,
            v: &self.v,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct YuvImageView<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
}

impl<'a> YuvImageView<'a> {
    pub fn new(
        width: u32,
        height: u32,
        y: &'a [u8],
        u: &'a [u8],
        v: &'a [u8],
    ) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::InvalidDimensions { width, height });
        }
        let y_expected = plane_len(width, height)?;
        if y.len() != y_expected {
            return Err(ImageError::PlaneSizeMismatch {
                plane: "y",
                expected: y_expected,
                actual: y.len(),
            });
        }
        let uv_width = width.div_ceil(2);
        let uv_height = height.div_ceil(2);
        let uv_expected = plane_len(uv_width, uv_height)?;
        if u.len() != uv_expected {
            return Err(ImageError::PlaneSizeMismatch {
                plane: "u",
                expected: uv_expected,
                actual: u.len(),
            });
        }
        if v.len() != uv_expected {
            return Err(ImageError::PlaneSizeMismatch {
                plane: "v",
                expected: uv_expected,
                actual: v.len(),
            });
        }
        Ok(Self {
            width,
            height,
            y,
            u,
            v,
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
    textured_quad: Option<Quad>,
    yuv_pipeline: Option<Arc<YuvPipeline>>,
    scene: Option<SceneRenderer>,
    upload_queue: UploadQueue,
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

    pub fn set_clear_color_rgba(&mut self, r: f64, g: f64, b: f64, a: f64) {
        self.clear_color = wgpu::Color { r, g, b, a };
    }

    pub fn clear_textured_quad(&mut self) {
        self.textured_quad = None;
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
        self.render_with_overlay(|_, _, _, _, _| {})
    }

    pub fn render_with_overlay<F>(&mut self, overlay: F) -> Result<(), RenderError>
    where
        F: FnOnce(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::TextureView,
            wgpu::TextureFormat,
        ),
    {
        let _ = self
            .upload_queue
            .drain(&self.device, &self.queue, &self.config);
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
        overlay(
            &self.device,
            &self.queue,
            &mut encoder,
            &view,
            self.config.format,
        );
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    pub fn render_with_overlay_and_capture<F>(
        &mut self,
        overlay: F,
        capture: &FrameCapture,
    ) -> Result<(), RenderCaptureError>
    where
        F: FnOnce(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::TextureView,
            wgpu::TextureFormat,
        ),
    {
        let _ = self
            .upload_queue
            .drain(&self.device, &self.queue, &self.config);
        let frame = self
            .surface
            .get_current_texture()
            .map_err(RenderCaptureError::Surface)?;
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
        overlay(
            &self.device,
            &self.queue,
            &mut encoder,
            &view,
            self.config.format,
        );
        capture
            .encode(&mut encoder, &frame.texture)
            .map_err(RenderCaptureError::Capture)?;
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn upload_queue(&self) -> UploadQueue {
        self.upload_queue.clone()
    }

    pub fn drain_uploads(&mut self) -> UploadDrainStats {
        self.upload_queue
            .drain(&self.device, &self.queue, &self.config)
    }

    pub fn set_uploaded_image(&mut self, uploaded: &UploadedImage) {
        self.textured_quad = Some(Quad::Rgba(Arc::clone(&uploaded.quad)));
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn set_image(&mut self, image: ImageData) -> Result<(), ImageError> {
        let quad = Arc::new(TexturedQuad::new(
            &self.device,
            &self.queue,
            &self.config,
            &image,
        )?);
        self.textured_quad = Some(Quad::Rgba(quad));
        Ok(())
    }

    pub fn update_image(&mut self, image: &ImageData) -> Result<(), ImageError> {
        if let Some(Quad::Rgba(quad)) = &self.textured_quad {
            quad.update(&self.queue, image)?;
            return Ok(());
        }
        let quad = Arc::new(TexturedQuad::new(
            &self.device,
            &self.queue,
            &self.config,
            image,
        )?);
        self.textured_quad = Some(Quad::Rgba(quad));
        Ok(())
    }

    pub fn set_yuv_image(&mut self, image: YuvImageData) -> Result<(), ImageError> {
        let view = image.as_view();
        self.set_yuv_image_view(&view)
    }

    pub fn set_yuv_image_view(&mut self, image: &YuvImageView) -> Result<(), ImageError> {
        let quad = YuvQuad::new(
            &self.device,
            &self.queue,
            &self.config,
            image,
            &mut self.yuv_pipeline,
        )?;
        self.textured_quad = Some(Quad::Yuv(Box::new(quad)));
        Ok(())
    }

    pub fn update_yuv_image(&mut self, image: &YuvImageData) -> Result<(), ImageError> {
        let view = image.as_view();
        self.update_yuv_image_view(&view)
    }

    pub fn update_yuv_image_view(&mut self, image: &YuvImageView) -> Result<(), ImageError> {
        if let Some(Quad::Yuv(quad)) = &self.textured_quad {
            quad.update(&self.queue, image)?;
            return Ok(());
        }
        let quad = YuvQuad::new(
            &self.device,
            &self.queue,
            &self.config,
            image,
            &mut self.yuv_pipeline,
        )?;
        self.textured_quad = Some(Quad::Yuv(Box::new(quad)));
        Ok(())
    }

    pub fn prewarm_yuv_pipeline(&mut self) {
        let _ = self.ensure_yuv_pipeline();
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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
            yuv_pipeline: None,
            scene: None,
            upload_queue: UploadQueue::new(),
        })
    }

    fn ensure_yuv_pipeline(&mut self) -> Arc<YuvPipeline> {
        let needs_new = match self.yuv_pipeline.as_ref() {
            Some(pipeline) => pipeline.format != self.config.format,
            None => true,
        };
        if needs_new {
            self.yuv_pipeline = Some(Arc::new(YuvPipeline::new(&self.device, &self.config)));
        }
        Arc::clone(self.yuv_pipeline.as_ref().expect("yuv pipeline must exist"))
    }
}

enum Quad {
    Rgba(Arc<TexturedQuad>),
    Yuv(Box<YuvQuad>),
}

impl Quad {
    fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        match self {
            Quad::Rgba(quad) => quad.draw(pass),
            Quad::Yuv(quad) => quad.draw(pass),
        }
    }
}

struct TexturedQuad {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

struct YuvPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    format: wgpu::TextureFormat,
}

struct YuvQuad {
    pipeline: Arc<YuvPipeline>,
    bind_group: wgpu::BindGroup,
    texture_y: wgpu::Texture,
    texture_u: wgpu::Texture,
    texture_v: wgpu::Texture,
    width: u32,
    height: u32,
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
            texture,
            width: image.width,
            height: image.height,
            vertex_buffer,
            index_buffer,
            index_count: 6,
        })
    }

    fn update(&self, queue: &wgpu::Queue, image: &ImageData) -> Result<(), ImageError> {
        if image.width != self.width || image.height != self.height {
            return Err(ImageError::InvalidDimensions {
                width: image.width,
                height: image.height,
            });
        }
        upload_texture(queue, &self.texture, image)
    }

    fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

impl YuvPipeline {
    fn new(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> Self {
        let shader_source = if config.format.is_srgb() {
            YUV_QUAD_SHADER_SRGB
        } else {
            YUV_QUAD_SHADER
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pallet.yuv.shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shader_source)),
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pallet.yuv.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pallet.yuv.bind_group_layout"),
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
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let vertex_data = quad_vertex_bytes();
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.yuv.vertex_buffer"),
            contents: &vertex_data,
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_data = quad_index_bytes();
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pallet.yuv.index_buffer"),
            contents: &index_data,
            usage: wgpu::BufferUsages::INDEX,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pallet.yuv.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pallet.yuv.pipeline"),
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

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            vertex_buffer,
            index_buffer,
            index_count: 6,
            format: config.format,
        }
    }
}

impl YuvQuad {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: &wgpu::SurfaceConfiguration,
        image: &YuvImageView<'_>,
        pipeline_cache: &mut Option<Arc<YuvPipeline>>,
    ) -> Result<Self, ImageError> {
        let needs_new = match pipeline_cache.as_ref() {
            Some(pipeline) => pipeline.format != config.format,
            None => true,
        };
        if needs_new {
            *pipeline_cache = Some(Arc::new(YuvPipeline::new(device, config)));
        }
        let pipeline = pipeline_cache
            .as_ref()
            .expect("yuv pipeline must exist")
            .clone();

        let texture_y = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pallet.yuv.texture_y"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let uv_width = image.width.div_ceil(2);
        let uv_height = image.height.div_ceil(2);
        let texture_u = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pallet.yuv.texture_u"),
            size: wgpu::Extent3d {
                width: uv_width,
                height: uv_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texture_v = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pallet.yuv.texture_v"),
            size: wgpu::Extent3d {
                width: uv_width,
                height: uv_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view_y = texture_y.create_view(&wgpu::TextureViewDescriptor::default());
        let view_u = texture_u.create_view(&wgpu::TextureViewDescriptor::default());
        let view_v = texture_v.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pallet.yuv.bind_group"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view_y),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view_u),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view_v),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
            ],
        });

        upload_plane(queue, &texture_y, image.width, image.height, image.y)?;
        upload_plane(queue, &texture_u, uv_width, uv_height, image.u)?;
        upload_plane(queue, &texture_v, uv_width, uv_height, image.v)?;

        Ok(Self {
            pipeline,
            bind_group,
            texture_y,
            texture_u,
            texture_v,
            width: image.width,
            height: image.height,
        })
    }

    fn update(&self, queue: &wgpu::Queue, image: &YuvImageView<'_>) -> Result<(), ImageError> {
        if image.width != self.width || image.height != self.height {
            return Err(ImageError::InvalidDimensions {
                width: image.width,
                height: image.height,
            });
        }
        let uv_width = image.width.div_ceil(2);
        let uv_height = image.height.div_ceil(2);
        upload_plane(queue, &self.texture_y, image.width, image.height, image.y)?;
        upload_plane(queue, &self.texture_u, uv_width, uv_height, image.u)?;
        upload_plane(queue, &self.texture_v, uv_width, uv_height, image.v)?;
        Ok(())
    }

    fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.pipeline.vertex_buffer.slice(..));
        pass.set_index_buffer(
            self.pipeline.index_buffer.slice(..),
            wgpu::IndexFormat::Uint16,
        );
        pass.draw_indexed(0..self.pipeline.index_count, 0, 0..1);
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

const YUV_QUAD_SHADER: &str = r#"
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
var t_y: texture_2d<f32>;
@group(0) @binding(1)
var t_u: texture_2d<f32>;
@group(0) @binding(2)
var t_v: texture_2d<f32>;
@group(0) @binding(3)
var s_tex: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let y = textureSample(t_y, s_tex, in.uv).r;
    let u = textureSample(t_u, s_tex, in.uv).r - 0.5;
    let v = textureSample(t_v, s_tex, in.uv).r - 0.5;
    let y_adj = max(y - (16.0 / 255.0), 0.0) * 1.164;
    let r = y_adj + 1.596 * v;
    let g = y_adj - 0.392 * u - 0.813 * v;
    let b = y_adj + 2.017 * u;
    return vec4<f32>(clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

const YUV_QUAD_SHADER_SRGB: &str = r#"
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
var t_y: texture_2d<f32>;
@group(0) @binding(1)
var t_u: texture_2d<f32>;
@group(0) @binding(2)
var t_v: texture_2d<f32>;
@group(0) @binding(3)
var s_tex: sampler;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = vec3<f32>(0.04045);
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= cutoff);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let y = textureSample(t_y, s_tex, in.uv).r;
    let u = textureSample(t_u, s_tex, in.uv).r - 0.5;
    let v = textureSample(t_v, s_tex, in.uv).r - 0.5;
    let y_adj = max(y - (16.0 / 255.0), 0.0) * 1.164;
    let r = y_adj + 1.596 * v;
    let g = y_adj - 0.392 * u - 0.813 * v;
    let b = y_adj + 2.017 * u;
    let rgb = clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(srgb_to_linear(rgb), 1.0);
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

fn plane_len(width: u32, height: u32) -> Result<usize, ImageError> {
    let width = usize::try_from(width).map_err(|_| ImageError::SizeOverflow)?;
    let height = usize::try_from(height).map_err(|_| ImageError::SizeOverflow)?;
    width.checked_mul(height).ok_or(ImageError::SizeOverflow)
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

fn upload_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<(), ImageError> {
    let row_bytes = usize::try_from(width).map_err(|_| ImageError::SizeOverflow)?;
    let padded = align_to(row_bytes, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)?;
    let height = usize::try_from(height).map_err(|_| ImageError::SizeOverflow)?;
    let expected = row_bytes
        .checked_mul(height)
        .ok_or(ImageError::SizeOverflow)?;
    if data.len() != expected {
        return Err(ImageError::DataSizeMismatch {
            expected,
            actual: data.len(),
        });
    }
    let data = if padded == row_bytes {
        data.to_vec()
    } else {
        let mut padded_data = vec![0u8; padded * height];
        for row in 0..height {
            let src_start = row * row_bytes;
            let src_end = src_start + row_bytes;
            let dst_start = row * padded;
            padded_data[dst_start..dst_start + row_bytes]
                .copy_from_slice(&data[src_start..src_end]);
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
            width,
            height: height as u32,
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
