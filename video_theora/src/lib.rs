use std::ffi::CString;
use std::path::Path;
use std::ptr;

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub play_ms: u32,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
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

// Keep the internal TheoraPlay queue small to avoid stalling decode.
const THEORAPLAY_MAX_FRAMES: u32 = 30;

pub struct VideoPlayer {
    decoder: *mut THEORAPLAY_Decoder,
    max_frames: u32,
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
        Ok(Self {
            decoder,
            max_frames: THEORAPLAY_MAX_FRAMES,
        })
    }

    pub fn pump(&mut self) {
        unsafe {
            THEORAPLAY_pumpDecode(self.decoder, self.max_frames as i32);
        }
    }

    pub fn take_video(&mut self) -> Result<Vec<VideoFrame>, VideoError> {
        let mut frames = Vec::new();
        loop {
            let ptr = unsafe { THEORAPLAY_getVideo(self.decoder) };
            if ptr.is_null() {
                break;
            }
            let frame = unsafe { &*ptr };
            let width = frame.width;
            let height = frame.height;
            let y_len = plane_len(width, height);
            let uv_width = width.div_ceil(2);
            let uv_height = height.div_ceil(2);
            let uv_len = plane_len(uv_width, uv_height);
            let total_len = y_len + uv_len.saturating_mul(2);
            let pixels = unsafe { std::slice::from_raw_parts(frame.pixels, total_len) };
            let mut y = vec![0u8; y_len];
            let mut u = vec![0u8; uv_len];
            let mut v = vec![0u8; uv_len];
            y.copy_from_slice(&pixels[..y_len]);
            let u_start = y_len;
            let v_start = u_start + uv_len;
            u.copy_from_slice(&pixels[u_start..u_start + uv_len]);
            v.copy_from_slice(&pixels[v_start..v_start + uv_len]);
            frames.push(VideoFrame {
                play_ms: frame.playms,
                fps: frame.fps,
                width,
                height,
                y,
                u,
                v,
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
        let mut packets = Vec::new();
        loop {
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

fn plane_len(width: u32, height: u32) -> usize {
    width as usize * height as usize
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
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
