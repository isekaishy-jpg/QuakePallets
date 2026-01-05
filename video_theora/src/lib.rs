use std::ffi::CString;
use std::fmt;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

pub struct VideoFrame {
    pub play_ms: u32,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    y_offset: usize,
    u_offset: usize,
    v_offset: usize,
    y_len: usize,
    uv_len: usize,
    data: PooledBuffer,
}

impl VideoFrame {
    pub fn y_plane(&self) -> &[u8] {
        let start = self.y_offset;
        let end = start + self.y_len;
        &self.data.as_slice()[start..end]
    }

    pub fn u_plane(&self) -> &[u8] {
        let start = self.u_offset;
        let end = start + self.uv_len;
        &self.data.as_slice()[start..end]
    }

    pub fn v_plane(&self) -> &[u8] {
        let start = self.v_offset;
        let end = start + self.uv_len;
        &self.data.as_slice()[start..end]
    }
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("play_ms", &self.play_ms)
            .field("fps", &self.fps)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("y_len", &self.y_len)
            .field("uv_len", &self.uv_len)
            .finish()
    }
}

#[derive(Debug)]
pub struct AudioPacket {
    pub play_ms: u32,
    pub channels: u32,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub enum VideoError {
    InvalidPath,
    StartFailed,
    DecoderError,
}

const MAX_POOL_BUFFERS: usize = 16;

// Keep the internal TheoraPlay queue small to avoid stalling decode.
const THEORAPLAY_MAX_FRAMES: u32 = 30;

pub struct VideoPlayer {
    decoder: *mut THEORAPLAY_Decoder,
    max_frames: u32,
    pool: Arc<FramePool>,
}

impl VideoPlayer {
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        let path = path.to_str().ok_or(VideoError::InvalidPath)?.to_string();
        let path = CString::new(path).map_err(|_| VideoError::InvalidPath)?;
        let decoder = unsafe {
            THEORAPLAY_startDecodeFile(
                path.as_ptr(),
                THEORAPLAY_MAX_FRAMES,
                THEORAPLAY_VideoFormat::IYUV,
                ptr::null(),
                1,
            )
        };
        if decoder.is_null() {
            return Err(VideoError::StartFailed);
        }
        let pool = shared_frame_pool();
        Ok(Self {
            decoder,
            max_frames: THEORAPLAY_MAX_FRAMES,
            pool,
        })
    }

    pub fn pump(&mut self) {
        unsafe {
            THEORAPLAY_pumpDecode(self.decoder, self.max_frames as i32);
        }
    }

    pub fn take_video(&mut self) -> Result<Vec<VideoFrame>, VideoError> {
        self.take_video_limited(usize::MAX)
    }

    pub fn take_video_limited(&mut self, max_frames: usize) -> Result<Vec<VideoFrame>, VideoError> {
        if max_frames == 0 {
            return Ok(Vec::new());
        }
        let mut frames = Vec::new();
        loop {
            if frames.len() >= max_frames {
                break;
            }
            let ptr = unsafe { THEORAPLAY_getVideo(self.decoder) };
            if ptr.is_null() {
                break;
            }
            let frame = unsafe { &*ptr };
            let width = frame.width;
            let height = frame.height;
            let y_len = plane_len(width, height);
            let src_uv_width = width / 2;
            let src_uv_height = height / 2;
            let src_uv_len = plane_len(src_uv_width, src_uv_height);
            let src_total_len = y_len + src_uv_len.saturating_mul(2);
            let pixels = unsafe { std::slice::from_raw_parts(frame.pixels, src_total_len) };
            let uv_width = width.div_ceil(2);
            let uv_height = height.div_ceil(2);
            let uv_len = plane_len(uv_width, uv_height);
            let total_len = y_len + uv_len.saturating_mul(2);
            let mut buffer = PooledBuffer::new(Arc::clone(&self.pool), total_len);
            let data = buffer.as_mut_slice();
            data[..y_len].copy_from_slice(&pixels[..y_len]);
            let u_start = y_len;
            let v_start = u_start + uv_len;
            let src_u_start = y_len;
            let src_v_start = src_u_start + src_uv_len;
            if uv_len > 0 {
                copy_plane_with_padding(
                    &mut data[u_start..u_start + uv_len],
                    uv_width,
                    uv_height,
                    &pixels[src_u_start..src_u_start + src_uv_len],
                    src_uv_width,
                    src_uv_height,
                );
                copy_plane_with_padding(
                    &mut data[v_start..v_start + uv_len],
                    uv_width,
                    uv_height,
                    &pixels[src_v_start..src_v_start + src_uv_len],
                    src_uv_width,
                    src_uv_height,
                );
            }
            frames.push(VideoFrame {
                play_ms: frame.playms,
                fps: frame.fps,
                width,
                height,
                y_offset: 0,
                u_offset: u_start,
                v_offset: v_start,
                y_len,
                uv_len,
                data: buffer,
            });
            unsafe {
                THEORAPLAY_freeVideo(ptr);
            }
        }
        if self.has_error() {
            return Err(VideoError::DecoderError);
        }
        Ok(frames)
    }

    pub fn take_audio(&mut self) -> Result<Vec<AudioPacket>, VideoError> {
        self.take_audio_limited(usize::MAX)
    }

    pub fn take_audio_limited(
        &mut self,
        max_packets: usize,
    ) -> Result<Vec<AudioPacket>, VideoError> {
        if max_packets == 0 {
            return Ok(Vec::new());
        }
        let mut packets = Vec::new();
        loop {
            if packets.len() >= max_packets {
                break;
            }
            let ptr = unsafe { THEORAPLAY_getAudio(self.decoder) };
            if ptr.is_null() {
                break;
            }
            let packet = unsafe { &*ptr };
            let frames = packet.frames.max(0) as usize;
            let channels = packet.channels.max(0) as usize;
            let len = frames.saturating_mul(channels);
            let samples = unsafe { std::slice::from_raw_parts(packet.samples, len) };
            let mut out = Vec::with_capacity(len);
            out.extend_from_slice(samples);
            packets.push(AudioPacket {
                play_ms: packet.playms,
                channels: packet.channels as u32,
                sample_rate: packet.freq as u32,
                samples: out,
            });
            unsafe {
                THEORAPLAY_freeAudio(ptr);
            }
        }
        if self.has_error() {
            return Err(VideoError::DecoderError);
        }
        Ok(packets)
    }

    pub fn is_finished(&self) -> bool {
        if self.decoder.is_null() {
            return true;
        }
        let decoding = unsafe { THEORAPLAY_isDecoding(self.decoder) != 0 };
        let video_left = unsafe { THEORAPLAY_availableVideo(self.decoder) != 0 };
        let audio_left = unsafe { THEORAPLAY_availableAudio(self.decoder) != 0 };
        !decoding && !video_left && !audio_left
    }

    pub fn has_error(&self) -> bool {
        if self.decoder.is_null() {
            return true;
        }
        unsafe { THEORAPLAY_decodingError(self.decoder) != 0 }
    }

    pub fn stop(&mut self) {
        if !self.decoder.is_null() {
            unsafe {
                THEORAPLAY_stopDecode(self.decoder);
            }
            self.decoder = ptr::null_mut();
        }
    }
}

fn shared_frame_pool() -> Arc<FramePool> {
    static FRAME_POOL: OnceLock<Arc<FramePool>> = OnceLock::new();
    FRAME_POOL
        .get_or_init(|| Arc::new(FramePool::new(MAX_POOL_BUFFERS)))
        .clone()
}

fn plane_len(width: u32, height: u32) -> usize {
    width as usize * height as usize
}

fn copy_plane_with_padding(
    dest: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    src: &[u8],
    src_width: u32,
    src_height: u32,
) {
    if dest.is_empty() {
        return;
    }
    if src_width == 0 || src_height == 0 {
        dest.fill(128);
        return;
    }
    let dest_width = dest_width as usize;
    let dest_height = dest_height as usize;
    let src_width = src_width as usize;
    let src_height = src_height as usize;
    for row in 0..dest_height {
        let src_row = row.min(src_height.saturating_sub(1));
        let src_base = src_row * src_width;
        let dest_base = row * dest_width;
        for col in 0..dest_width {
            let src_col = col.min(src_width.saturating_sub(1));
            dest[dest_base + col] = src[src_base + src_col];
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct FramePool {
    buffers: Mutex<Vec<Vec<u8>>>,
    max_buffers: usize,
}

impl FramePool {
    fn new(max_buffers: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::new()),
            max_buffers,
        }
    }

    fn take(&self, len: usize) -> Vec<u8> {
        let mut buffers = self.buffers.lock().expect("frame pool poisoned");
        if let Some(mut buffer) = buffers.pop() {
            buffer.resize(len, 0);
            return buffer;
        }
        vec![0u8; len]
    }

    fn release(&self, mut buffer: Vec<u8>) {
        let mut buffers = self.buffers.lock().expect("frame pool poisoned");
        if buffers.len() >= self.max_buffers {
            return;
        }
        buffer.clear();
        buffers.push(buffer);
    }
}

struct PooledBuffer {
    data: Vec<u8>,
    pool: Arc<FramePool>,
}

impl PooledBuffer {
    fn new(pool: Arc<FramePool>, len: usize) -> Self {
        let data = pool.take(len);
        Self { data, pool }
    }

    fn as_slice(&self) -> &[u8] {
        &self.data
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        let mut buffer = Vec::new();
        std::mem::swap(&mut buffer, &mut self.data);
        self.pool.release(buffer);
    }
}

#[repr(C)]
struct THEORAPLAY_Decoder {
    _private: [u8; 0],
}

#[repr(C)]
struct THEORAPLAY_VideoFrame {
    seek_generation: u32,
    playms: u32,
    fps: f64,
    width: u32,
    height: u32,
    format: THEORAPLAY_VideoFormat,
    pixels: *const u8,
    next: *const THEORAPLAY_VideoFrame,
}

#[repr(C)]
struct THEORAPLAY_AudioPacket {
    seek_generation: u32,
    playms: u32,
    channels: i32,
    freq: i32,
    frames: i32,
    samples: *const f32,
    next: *const THEORAPLAY_AudioPacket,
}

#[repr(C)]
#[derive(Copy, Clone)]
#[allow(dead_code)]
#[allow(clippy::upper_case_acronyms)] // FFI enum mirrors THEORAPLAY names.
enum THEORAPLAY_VideoFormat {
    YV12 = 0,
    IYUV = 1,
    RGB = 2,
    RGBA = 3,
    BGRA = 4,
    RGB565 = 5,
}

extern "C" {
    fn THEORAPLAY_startDecodeFile(
        fname: *const i8,
        maxframes: u32,
        vidfmt: THEORAPLAY_VideoFormat,
        allocator: *const std::ffi::c_void,
        multithreaded: i32,
    ) -> *mut THEORAPLAY_Decoder;
    fn THEORAPLAY_stopDecode(decoder: *mut THEORAPLAY_Decoder);
    fn THEORAPLAY_pumpDecode(decoder: *mut THEORAPLAY_Decoder, maxframes: i32);
    fn THEORAPLAY_isDecoding(decoder: *mut THEORAPLAY_Decoder) -> i32;
    fn THEORAPLAY_decodingError(decoder: *mut THEORAPLAY_Decoder) -> i32;
    fn THEORAPLAY_availableVideo(decoder: *mut THEORAPLAY_Decoder) -> u32;
    fn THEORAPLAY_availableAudio(decoder: *mut THEORAPLAY_Decoder) -> u32;
    fn THEORAPLAY_getVideo(decoder: *mut THEORAPLAY_Decoder) -> *const THEORAPLAY_VideoFrame;
    fn THEORAPLAY_freeVideo(item: *const THEORAPLAY_VideoFrame);
    fn THEORAPLAY_getAudio(decoder: *mut THEORAPLAY_Decoder) -> *const THEORAPLAY_AudioPacket;
    fn THEORAPLAY_freeAudio(item: *const THEORAPLAY_AudioPacket);
}
