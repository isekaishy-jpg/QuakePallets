#![forbid(unsafe_code)]

use std::fmt;
use std::sync::{Arc, Mutex};

use miniaudio::{DecoderConfig, Device, DeviceConfig, DeviceType, Format, FramesMut, SyncDecoder};

const OUTPUT_FORMAT: Format = Format::F32;
const OUTPUT_CHANNELS: u32 = 2;

pub struct AudioEngine {
    _device: Device,
    state: Arc<Mutex<AudioState>>,
    sample_rate: u32,
    channels: u32,
}

struct AudioState {
    sfx: Vec<ActiveSound>,
    music: Option<ActiveSound>,
    scratch: Vec<f32>,
}

struct ActiveSound {
    decoder: SyncDecoder,
    volume: f32,
    looping: bool,
    finished: bool,
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

impl AudioEngine {
    pub fn new() -> Result<Self, AudioError> {
        let state = Arc::new(Mutex::new(AudioState::new()));
        let state_for_cb = Arc::clone(&state);

        let mut config = DeviceConfig::new(DeviceType::Playback);
        config.playback_mut().set_format(OUTPUT_FORMAT);
        config.playback_mut().set_channels(OUTPUT_CHANNELS);
        config.set_data_callback(move |_device, output, _input| {
            mix_callback(&state_for_cb, output);
        });

        let device = Device::new(None, &config).map_err(AudioError::DeviceInit)?;
        device.start().map_err(AudioError::DeviceInit)?;

        let sample_rate = device.sample_rate();
        let channels = device.playback().channels();

        Ok(Self {
            _device: device,
            state,
            sample_rate,
            channels,
        })
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
    fn new() -> Self {
        Self {
            sfx: Vec::new(),
            music: None,
            scratch: Vec::new(),
        }
    }

    fn mix_into(&mut self, output: &mut [f32], channels: usize) {
        output.fill(0.0);
        if output.is_empty() {
            return;
        }

        self.ensure_scratch(output.len());
        let AudioState {
            music,
            sfx,
            scratch,
        } = self;
        if let Some(sound) = music.as_mut() {
            Self::mix_sound(sound, output, channels, scratch);
            if sound.finished {
                *music = None;
            }
        }

        for sound in sfx.iter_mut() {
            Self::mix_sound(sound, output, channels, scratch);
        }
        sfx.retain(|sound| !sound.finished);

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
        let scratch = &mut scratch[..output.len()];
        scratch.fill(0.0);
        let frame_count = output.len() / channels;
        let mut frames = FramesMut::wrap::<f32>(scratch, OUTPUT_FORMAT, channels as u32);
        let read_frames = sound.decoder.read_pcm_frames(&mut frames) as usize;
        let sample_count = read_frames * channels;
        for i in 0..sample_count {
            output[i] += scratch[i] * sound.volume;
        }

        if read_frames < frame_count {
            if sound.looping {
                let _ = sound.decoder.seek_to_pcm_frame(0);
            } else {
                sound.finished = true;
            }
        }
    }

    fn ensure_scratch(&mut self, len: usize) {
        if self.scratch.len() < len {
            self.scratch.resize(len, 0.0);
        }
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

fn mix_callback(state: &Arc<Mutex<AudioState>>, output: &mut FramesMut) {
    let channels = output.channels() as usize;
    let samples = output.as_samples_mut::<f32>();
    match state.try_lock() {
        Ok(mut state) => state.mix_into(samples, channels),
        Err(_) => samples.fill(0.0),
    }
}
