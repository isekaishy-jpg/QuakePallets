#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use miniaudio::{
    DecoderConfig, Device, DeviceConfig, DeviceType, DitherMode, Format, Frames, FramesMut,
    SyncDecoder,
};

const OUTPUT_FORMAT: Format = Format::F32;
const OUTPUT_CHANNELS: u32 = 2;
const MAX_PCM_BUFFER_SECONDS: f32 = 4.0;

pub struct AudioEngine {
    _device: Device,
    state: Arc<Mutex<AudioState>>,
    sample_rate: u32,
    channels: u32,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    pcm_queued: Arc<AtomicUsize>,
    pcm_max_samples: usize,
    output_frames: Arc<AtomicU64>,
    pcm_underrun_frames: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct PcmWriter {
    tx: mpsc::Sender<Vec<f32>>,
    queued: Arc<AtomicUsize>,
    max_samples: usize,
    channels: usize,
}

#[derive(Clone)]
pub struct AudioClock {
    frames: Arc<AtomicU64>,
    sample_rate: u32,
}

struct AudioState {
    sfx: Vec<ActiveSound>,
    music: Option<ActiveSound>,
    pcm_stream: PcmStream,
    pcm_rx: mpsc::Receiver<Vec<f32>>,
    pcm_queued: Arc<AtomicUsize>,
    pcm_underrun_frames: Arc<AtomicU64>,
    scratch: Vec<f32>,
    mix_buffer: Vec<f32>,
}

struct MixInputs<'a> {
    music: &'a mut Option<ActiveSound>,
    sfx: &'a mut Vec<ActiveSound>,
    pcm_stream: &'a mut PcmStream,
    pcm_rx: &'a mpsc::Receiver<Vec<f32>>,
    pcm_queued: &'a Arc<AtomicUsize>,
    pcm_underrun_frames: &'a Arc<AtomicU64>,
    scratch: &'a mut Vec<f32>,
    channels: usize,
}

struct ActiveSound {
    decoder: SyncDecoder,
    volume: f32,
    looping: bool,
    finished: bool,
}

struct PcmStream {
    chunks: VecDeque<Vec<f32>>,
    offset: usize,
    queued_samples: usize,
    volume: f32,
    active: bool,
}

#[derive(Debug)]
pub enum AudioError {
    DeviceInit(miniaudio::Error),
    Decode(miniaudio::Error),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::DeviceInit(err) => write!(f, "audio device init failed: {}", err),
            AudioError::Decode(err) => write!(f, "audio decode failed: {}", err),
        }
    }
}

impl std::error::Error for AudioError {}

impl PcmWriter {
    pub fn push(&self, samples: Vec<f32>) {
        let _ = self.try_push(samples);
    }

    pub fn try_push(&self, samples: Vec<f32>) -> Result<(), Vec<f32>> {
        if samples.is_empty() || self.max_samples == 0 || self.channels == 0 {
            return Ok(());
        }
        let mut samples = samples;
        let aligned_len = samples.len() / self.channels * self.channels;
        if aligned_len == 0 {
            return Ok(());
        }
        if aligned_len < samples.len() {
            samples.truncate(aligned_len);
        }
        let in_frames = samples.len() / self.channels;
        loop {
            let current = self.queued.load(Ordering::Acquire);
            let available_samples = self.max_samples.saturating_sub(current);
            let available_frames = available_samples / self.channels;
            if available_frames == 0 {
                return Err(samples);
            }
            let write_frames = available_frames.min(in_frames);
            let write_len = write_frames * self.channels;
            if self
                .queued
                .compare_exchange(
                    current,
                    current + write_len,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                let remainder = if write_len < samples.len() {
                    samples.split_off(write_len)
                } else {
                    Vec::new()
                };
                let _ = self.tx.send(samples);
                if remainder.is_empty() {
                    return Ok(());
                }
                return Err(remainder);
            }
        }
    }
}

impl AudioClock {
    pub fn time_ms(&self) -> u64 {
        let frames = self.frames.load(Ordering::Acquire);
        if self.sample_rate == 0 {
            return 0;
        }
        frames.saturating_mul(1000) / self.sample_rate as u64
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl AudioEngine {
    pub fn new() -> Result<Self, AudioError> {
        let (pcm_tx, pcm_rx) = mpsc::channel();
        let pcm_queued = Arc::new(AtomicUsize::new(0));
        let output_frames = Arc::new(AtomicU64::new(0));
        let pcm_underrun_frames = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(AudioState::new(
            pcm_rx,
            Arc::clone(&pcm_queued),
            Arc::clone(&pcm_underrun_frames),
        )));
        let state_for_cb = Arc::clone(&state);
        let frames_for_cb = Arc::clone(&output_frames);

        let mut config = DeviceConfig::new(DeviceType::Playback);
        config.playback_mut().set_format(OUTPUT_FORMAT);
        config.playback_mut().set_channels(OUTPUT_CHANNELS);
        config.set_performance_profile(miniaudio::PerformanceProfile::Conservative);
        config.set_period_size_in_milliseconds(20);
        config.set_periods(4);
        config.set_data_callback(move |_device, output, _input| {
            mix_callback(&state_for_cb, output);
            frames_for_cb.fetch_add(output.frame_count() as u64, Ordering::AcqRel);
        });

        let device = Device::new(None, &config).map_err(AudioError::DeviceInit)?;
        device.start().map_err(AudioError::DeviceInit)?;

        let sample_rate = device.sample_rate();
        let channels = device.playback().channels();
        let pcm_max_samples =
            (sample_rate as f32 * channels as f32 * MAX_PCM_BUFFER_SECONDS) as usize;

        Ok(Self {
            _device: device,
            state,
            sample_rate,
            channels,
            pcm_tx,
            pcm_queued,
            pcm_max_samples,
            output_frames,
            pcm_underrun_frames,
        })
    }

    pub fn output_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn output_channels(&self) -> u32 {
        self.channels
    }

    pub fn queue_pcm(&self, samples: Vec<f32>) {
        self.pcm_writer().push(samples);
    }

    pub fn clear_pcm(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.clear_pcm();
        }
        self.pcm_queued.store(0, Ordering::Release);
        self.pcm_underrun_frames.store(0, Ordering::Release);
    }

    pub fn pcm_writer(&self) -> PcmWriter {
        PcmWriter {
            tx: self.pcm_tx.clone(),
            queued: Arc::clone(&self.pcm_queued),
            max_samples: self.pcm_max_samples,
            channels: self.channels as usize,
        }
    }

    pub fn clock(&self) -> AudioClock {
        AudioClock {
            frames: Arc::clone(&self.output_frames),
            sample_rate: self.sample_rate,
        }
    }

    pub fn output_frames(&self) -> u64 {
        self.output_frames.load(Ordering::Acquire)
    }

    pub fn pcm_buffered_ms(&self) -> u64 {
        if self.sample_rate == 0 || self.channels == 0 {
            return 0;
        }
        let samples = self.pcm_queued.load(Ordering::Acquire) as u64;
        let frames = samples / self.channels as u64;
        frames.saturating_mul(1000) / self.sample_rate as u64
    }

    pub fn pcm_underrun_frames(&self) -> u64 {
        self.pcm_underrun_frames.load(Ordering::Acquire)
    }

    pub fn play_wav(&self, data: Vec<u8>) -> Result<(), AudioError> {
        let decoder = self.decode(data)?;
        let mut state = self.state.lock().expect("audio state poisoned");
        state.sfx.push(ActiveSound::new(decoder, 1.0, false));
        Ok(())
    }

    pub fn play_music(&self, data: Vec<u8>) -> Result<(), AudioError> {
        let decoder = self.decode(data)?;
        let mut state = self.state.lock().expect("audio state poisoned");
        state.music = Some(ActiveSound::new(decoder, 0.6, false));
        Ok(())
    }

    pub fn stop_music(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.music = None;
        }
    }

    fn decode(&self, data: Vec<u8>) -> Result<SyncDecoder, AudioError> {
        let config = DecoderConfig::new(OUTPUT_FORMAT, self.channels, self.sample_rate);
        SyncDecoder::from_memory(data, Some(&config)).map_err(AudioError::Decode)
    }
}

impl AudioState {
    fn new(
        pcm_rx: mpsc::Receiver<Vec<f32>>,
        pcm_queued: Arc<AtomicUsize>,
        pcm_underrun_frames: Arc<AtomicU64>,
    ) -> Self {
        Self {
            sfx: Vec::new(),
            music: None,
            pcm_stream: PcmStream::new(1.0),
            pcm_rx,
            pcm_queued,
            pcm_underrun_frames,
            scratch: Vec::new(),
            mix_buffer: Vec::new(),
        }
    }

    fn mix_into_output(&mut self, output: &mut FramesMut) {
        let format = output.format();
        if format == Format::Unknown {
            output.as_bytes_mut().fill(0);
            return;
        }

        let channels = output.channels() as usize;
        let frame_count = output.frame_count();
        let sample_count = frame_count * channels;
        let AudioState {
            music,
            sfx,
            pcm_stream,
            pcm_rx,
            pcm_queued,
            pcm_underrun_frames,
            scratch,
            mix_buffer,
        } = self;

        if format == OUTPUT_FORMAT {
            let samples = output.as_samples_mut::<f32>();
            let mut inputs = MixInputs {
                music,
                sfx,
                pcm_stream,
                pcm_rx,
                pcm_queued,
                pcm_underrun_frames,
                scratch,
                channels,
            };
            Self::mix_into_samples(&mut inputs, samples);
            return;
        }

        if sample_count == 0 {
            output.as_bytes_mut().fill(0);
            return;
        }

        if mix_buffer.len() < sample_count {
            mix_buffer.resize(sample_count, 0.0);
        }
        let mix_samples = &mut mix_buffer[..sample_count];
        let mut inputs = MixInputs {
            music,
            sfx,
            pcm_stream,
            pcm_rx,
            pcm_queued,
            pcm_underrun_frames,
            scratch,
            channels,
        };
        Self::mix_into_samples(&mut inputs, mix_samples);

        let frames = Frames::wrap::<f32>(mix_samples, OUTPUT_FORMAT, channels as u32);
        frames.convert(output, DitherMode::None);
    }

    fn mix_into_samples(inputs: &mut MixInputs<'_>, output: &mut [f32]) {
        let channels = inputs.channels;
        output.fill(0.0);
        if output.is_empty() {
            return;
        }

        Self::ensure_scratch(inputs.scratch, output.len());
        let scratch = &mut inputs.scratch[..output.len()];
        if let Some(sound) = inputs.music.as_mut() {
            Self::mix_sound(sound, output, channels, scratch);
            if sound.finished {
                *inputs.music = None;
            }
        }

        for sound in inputs.sfx.iter_mut() {
            Self::mix_sound(sound, output, channels, scratch);
        }
        inputs.sfx.retain(|sound| !sound.finished);

        inputs.pcm_stream.drain_rx(inputs.pcm_rx);
        inputs.pcm_stream.mix_into(
            output,
            inputs.pcm_queued,
            inputs.pcm_underrun_frames,
            channels,
        );

        for sample in output.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    fn mix_sound(
        sound: &mut ActiveSound,
        output: &mut [f32],
        channels: usize,
        scratch: &mut [f32],
    ) {
        let frame_count = output.len() / channels;
        let scratch = &mut scratch[..output.len()];
        let mut frames_read = 0usize;
        while frames_read < frame_count {
            let sample_offset = frames_read * channels;
            let remaining_samples = output.len() - sample_offset;
            let scratch_slice = &mut scratch[sample_offset..sample_offset + remaining_samples];
            let mut frames = FramesMut::wrap::<f32>(scratch_slice, OUTPUT_FORMAT, channels as u32);
            let read_frames = sound.decoder.read_pcm_frames(&mut frames) as usize;
            if read_frames == 0 {
                if sound.looping {
                    let _ = sound.decoder.seek_to_pcm_frame(0);
                    if frames_read == 0 {
                        sound.finished = true;
                        break;
                    }
                    continue;
                } else {
                    sound.finished = true;
                }
                break;
            }

            let sample_count = read_frames * channels;
            for i in 0..sample_count {
                output[sample_offset + i] += scratch_slice[i] * sound.volume;
            }
            frames_read += read_frames;
        }
    }

    fn ensure_scratch(scratch: &mut Vec<f32>, len: usize) {
        if scratch.len() < len {
            scratch.resize(len, 0.0);
        }
    }

    fn clear_pcm(&mut self) {
        self.pcm_stream.clear();
        while self.pcm_rx.try_recv().is_ok() {}
    }
}

impl ActiveSound {
    fn new(decoder: SyncDecoder, volume: f32, looping: bool) -> Self {
        Self {
            decoder,
            volume,
            looping,
            finished: false,
        }
    }
}

impl PcmStream {
    fn new(volume: f32) -> Self {
        Self {
            chunks: VecDeque::new(),
            offset: 0,
            queued_samples: 0,
            volume,
            active: false,
        }
    }

    fn drain_rx(&mut self, rx: &mpsc::Receiver<Vec<f32>>) {
        while let Ok(samples) = rx.try_recv() {
            if samples.is_empty() {
                continue;
            }
            self.active = true;
            self.queued_samples = self.queued_samples.saturating_add(samples.len());
            self.chunks.push_back(samples);
        }
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.offset = 0;
        self.queued_samples = 0;
        self.active = false;
    }

    fn mix_into(
        &mut self,
        output: &mut [f32],
        queued: &AtomicUsize,
        underruns: &AtomicU64,
        channels: usize,
    ) {
        if output.is_empty() {
            return;
        }
        if self.queued_samples == 0 {
            if self.active && channels > 0 {
                let missing_frames = (output.len() / channels) as u64;
                if missing_frames > 0 {
                    underruns.fetch_add(missing_frames, Ordering::AcqRel);
                }
            }
            return;
        }
        let mut consumed = 0usize;
        let mut index = 0usize;
        while index < output.len() {
            let Some(front) = self.chunks.front() else {
                break;
            };
            if self.offset >= front.len() {
                self.chunks.pop_front();
                self.offset = 0;
                continue;
            }
            output[index] += front[self.offset] * self.volume;
            self.offset += 1;
            self.queued_samples = self.queued_samples.saturating_sub(1);
            consumed += 1;
            index += 1;
        }
        if consumed > 0 {
            queued.fetch_sub(consumed, Ordering::AcqRel);
        }
        if self.active && consumed < output.len() && channels > 0 {
            let missing_samples = output.len() - consumed;
            let missing_frames = (missing_samples / channels) as u64;
            if missing_frames > 0 {
                underruns.fetch_add(missing_frames, Ordering::AcqRel);
            }
        }
    }
}

fn mix_callback(state: &Arc<Mutex<AudioState>>, output: &mut FramesMut) {
    let mut state = state.lock().expect("audio state poisoned");
    state.mix_into_output(output);
}
