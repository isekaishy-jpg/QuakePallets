use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use audio::{AudioClock, AudioEngine, PcmWriter};
use miniaudio::{DataConverter, DataConverterConfig, Format, Frames, FramesMut};
use video_theora::{AudioPacket, VideoFrame, VideoPlayer};

pub const VIDEO_START_MIN_FRAMES: usize = 3;
pub const VIDEO_PLAYBACK_WARM_MS: u64 = 500;
pub const VIDEO_PLAYBACK_WARM_UP_MS: u64 = 1500;
pub const VIDEO_MAX_QUEUED_MS_PLAYBACK: u64 = 2000;
pub const VIDEO_PREDECODE_WARM_MS: u64 = 200;
pub const VIDEO_PREDECODE_RAMP_MS: u64 = 1500;
pub const VIDEO_MAX_QUEUED_MS_PREDECODE: u64 = 500;
pub const VIDEO_DECODE_VIDEO_BUDGET_FRAMES: usize = 4;
pub const VIDEO_DECODE_AUDIO_BUDGET_PACKETS: usize = 8;
pub const VIDEO_AUDIO_PREBUFFER_MS: u64 = 100;
pub const VIDEO_AUDIO_PREBUFFER_MAX_MS: u64 = 500;
pub const VIDEO_INTERMISSION_MS: u64 = 150;
pub const VIDEO_HOLD_LAST_FRAME_MS: u64 = 350;
pub const VIDEO_PREDECODE_START_DELAY_MS: u64 = 500;
pub const VIDEO_PREDECODE_MIN_ELAPSED_MS: u64 = 1000;
pub const VIDEO_PREDECODE_MIN_AUDIO_MS: u64 = 250;
pub const VIDEO_PREDECODE_MIN_FRAMES: usize = 6;
pub const VIDEO_PREDECODE_MAX_MS: u64 = 1000;

enum VideoEvent {
    Video(VideoFrame),
    AudioEnded,
    End,
    Error(String),
}

pub struct VideoDebugStats {
    audio_packets: AtomicU64,
    audio_frames_in: AtomicU64,
    audio_frames_out: AtomicU64,
    audio_frames_queued: AtomicU64,
    pending_audio_frames: AtomicU64,
    audio_sample_rate: AtomicU64,
    audio_channels: AtomicU64,
    last_audio_ms: AtomicU64,
    last_video_ms: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct PlaylistEntry {
    pub path: PathBuf,
    pub hold_ms: u64,
}

impl PlaylistEntry {
    pub fn new(path: PathBuf, hold_ms: u64) -> Self {
        Self { path, hold_ms }
    }
}

#[derive(Clone, Copy)]
pub struct VideoDebugSnapshot {
    pub audio_packets: u64,
    pub audio_frames_in: u64,
    pub audio_frames_out: u64,
    pub audio_frames_queued: u64,
    pub pending_audio_frames: u64,
    pub audio_sample_rate: u64,
    pub audio_channels: u64,
    pub last_audio_ms: u64,
    pub last_video_ms: u64,
}

impl VideoDebugStats {
    pub fn new() -> Self {
        Self {
            audio_packets: AtomicU64::new(0),
            audio_frames_in: AtomicU64::new(0),
            audio_frames_out: AtomicU64::new(0),
            audio_frames_queued: AtomicU64::new(0),
            pending_audio_frames: AtomicU64::new(0),
            audio_sample_rate: AtomicU64::new(0),
            audio_channels: AtomicU64::new(0),
            last_audio_ms: AtomicU64::new(0),
            last_video_ms: AtomicU64::new(0),
        }
    }

    pub fn reset(&self) {
        self.audio_packets.store(0, Ordering::Release);
        self.audio_frames_in.store(0, Ordering::Release);
        self.audio_frames_out.store(0, Ordering::Release);
        self.audio_frames_queued.store(0, Ordering::Release);
        self.pending_audio_frames.store(0, Ordering::Release);
        self.audio_sample_rate.store(0, Ordering::Release);
        self.audio_channels.store(0, Ordering::Release);
        self.last_audio_ms.store(0, Ordering::Release);
        self.last_video_ms.store(0, Ordering::Release);
    }

    pub fn snapshot(&self) -> VideoDebugSnapshot {
        VideoDebugSnapshot {
            audio_packets: self.audio_packets.load(Ordering::Acquire),
            audio_frames_in: self.audio_frames_in.load(Ordering::Acquire),
            audio_frames_out: self.audio_frames_out.load(Ordering::Acquire),
            audio_frames_queued: self.audio_frames_queued.load(Ordering::Acquire),
            pending_audio_frames: self.pending_audio_frames.load(Ordering::Acquire),
            audio_sample_rate: self.audio_sample_rate.load(Ordering::Acquire),
            audio_channels: self.audio_channels.load(Ordering::Acquire),
            last_audio_ms: self.last_audio_ms.load(Ordering::Acquire),
            last_video_ms: self.last_video_ms.load(Ordering::Acquire),
        }
    }
}

pub struct VideoPlayback {
    rx: mpsc::Receiver<VideoEvent>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    start_clock_ms: Option<u64>,
    start_clock_shared: Arc<AtomicU64>,
    start_instant: Option<Instant>,
    clock: Option<AudioClock>,
    video_base_ms: Option<u32>,
    frames: VecDeque<VideoFrame>,
    queued_frames: Arc<AtomicUsize>,
    queued_video_ms: Arc<AtomicU64>,
    max_queued_video_ms: Arc<AtomicU64>,
    audio_enabled: Arc<AtomicBool>,
    prebuffered_audio_frames: Arc<AtomicU64>,
    previewed: bool,
    first_frame_uploaded: bool,
    finished: bool,
    audio_finished: bool,
    debug: Option<Arc<VideoDebugStats>>,
}
impl VideoPlayback {
    fn start(
        path: PathBuf,
        audio: Option<(PcmWriter, AudioClock, u32)>,
        debug: Option<Arc<VideoDebugStats>>,
        max_queued_video_ms: u64,
        audio_enabled: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let clock = audio.as_ref().map(|(_, clock, _)| clock.clone());
        let stats = debug.as_ref().map(Arc::clone);
        let start_clock_shared = Arc::new(AtomicU64::new(u64::MAX));
        let start_clock_thread = Arc::clone(&start_clock_shared);
        let queued_frames = Arc::new(AtomicUsize::new(0));
        let queued_frames_thread = Arc::clone(&queued_frames);
        let queued_video_ms = Arc::new(AtomicU64::new(0));
        let queued_video_ms_thread = Arc::clone(&queued_video_ms);
        let max_queued_video_ms = Arc::new(AtomicU64::new(max_queued_video_ms));
        let max_queued_video_ms_thread = Arc::clone(&max_queued_video_ms);
        let audio_enabled = Arc::new(AtomicBool::new(audio_enabled));
        let audio_enabled_thread = Arc::clone(&audio_enabled);
        let prebuffered_audio_frames = Arc::new(AtomicU64::new(0));
        let prebuffered_audio_frames_thread = Arc::clone(&prebuffered_audio_frames);
        let worker = thread::spawn(move || {
            let mut player = match VideoPlayer::open(&path) {
                Ok(player) => player,
                Err(err) => {
                    let _ = tx.send(VideoEvent::Error(format!("video init failed: {:?}", err)));
                    return;
                }
            };
            let mut converter: Option<AudioConverter> = None;
            let mut pending_audio: VecDeque<AudioPacket> = VecDeque::new();
            let mut pending_samples: VecDeque<Vec<f32>> = VecDeque::new();
            let mut pending_sample_frames: usize = 0;
            let mut audio_base_ms: Option<u32> = None;
            let mut last_audio_ms: Option<u64> = None;
            let mut decoded_audio_frames: u64 = 0;
            let mut audio_end_sent = false;
            let mut predecode_video_base_ms: Option<u32> = None;
            let mut predecode_video_ms: u64 = 0;
            let mut predecode_audio_ms: u64 = 0;
            let mut decoder_finished = false;
            let audio_out_rate = audio
                .as_ref()
                .map(|(_, clock, _)| clock.sample_rate())
                .unwrap_or(0);
            let prebuffer_max_frames = if audio_out_rate == 0 {
                0
            } else {
                (audio_out_rate as u64).saturating_mul(VIDEO_AUDIO_PREBUFFER_MAX_MS) / 1000
            } as usize;
            let prebuffer_min_frames = if audio_out_rate == 0 {
                0
            } else {
                (audio_out_rate as u64).saturating_mul(VIDEO_AUDIO_PREBUFFER_MS) / 1000
            } as usize;
            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    player.stop();
                    break;
                }

                let max_ms = max_queued_video_ms_thread.load(Ordering::Acquire);
                let queued_ms = queued_video_ms_thread.load(Ordering::Acquire);
                let video_queue_full = max_ms > 0 && queued_ms >= max_ms;
                let queued_frames = queued_frames_thread.load(Ordering::Acquire);
                let audio_started = start_clock_thread.load(Ordering::Acquire) != u64::MAX;
                let pending_frames =
                    prebuffered_audio_frames_thread.load(Ordering::Acquire) as usize;
                let prebuffer_ready =
                    prebuffer_min_frames == 0 || pending_frames >= prebuffer_min_frames;
                let predecode_ms = predecode_video_ms.max(predecode_audio_ms);
                if !audio_started
                    && predecode_ms >= VIDEO_PREDECODE_MAX_MS
                    && prebuffer_ready
                    && queued_frames >= VIDEO_START_MIN_FRAMES
                {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                if video_queue_full
                    && !audio_started
                    && prebuffer_max_frames > 0
                    && pending_frames >= prebuffer_max_frames
                {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }

                let mut did_work = false;
                if !decoder_finished {
                    let mut allow_audio_decode = audio_enabled_thread.load(Ordering::Acquire);
                    if allow_audio_decode && !audio_started && prebuffer_max_frames > 0 {
                        let pending_frames =
                            prebuffered_audio_frames_thread.load(Ordering::Acquire) as usize;
                        if pending_frames >= prebuffer_max_frames {
                            allow_audio_decode = false;
                        }
                    }
                    let audio_priority =
                        allow_audio_decode && pending_sample_frames < prebuffer_min_frames;
                    let mut allow_video_decode = !video_queue_full && !audio_priority;

                    let pump_needed = (allow_video_decode && VIDEO_DECODE_VIDEO_BUDGET_FRAMES > 0)
                        || (allow_audio_decode && VIDEO_DECODE_AUDIO_BUDGET_PACKETS > 0);
                    if pump_needed {
                        player.pump();
                    }

                    if allow_audio_decode && VIDEO_DECODE_AUDIO_BUDGET_PACKETS > 0 {
                        let mut audio_decoded_any = false;
                        match player.take_audio_limited(VIDEO_DECODE_AUDIO_BUDGET_PACKETS) {
                            Ok(packets) => {
                                if let Some((_writer, _clock, _out_channels)) = audio.as_ref() {
                                    if !packets.is_empty() {
                                        audio_decoded_any = true;
                                        did_work = true;
                                        if let Some(stats) = stats.as_ref() {
                                            if stats.audio_sample_rate.load(Ordering::Acquire) == 0
                                            {
                                                let sample_rate = packets[0].sample_rate as u64;
                                                let channels = packets[0].channels.max(1) as u64;
                                                stats
                                                    .audio_sample_rate
                                                    .compare_exchange(
                                                        0,
                                                        sample_rate,
                                                        Ordering::AcqRel,
                                                        Ordering::Relaxed,
                                                    )
                                                    .ok();
                                                stats
                                                    .audio_channels
                                                    .compare_exchange(
                                                        0,
                                                        channels,
                                                        Ordering::AcqRel,
                                                        Ordering::Relaxed,
                                                    )
                                                    .ok();
                                            }
                                            let mut frames = 0u64;
                                            for packet in packets.iter() {
                                                let channels = packet.channels.max(1) as usize;
                                                if channels > 0 {
                                                    frames +=
                                                        (packet.samples.len() / channels) as u64;
                                                }
                                            }
                                            stats
                                                .audio_packets
                                                .fetch_add(packets.len() as u64, Ordering::AcqRel);
                                            stats
                                                .audio_frames_in
                                                .fetch_add(frames, Ordering::AcqRel);
                                        }
                                        pending_audio.extend(packets);
                                    }
                                }
                            }
                            Err(_) => {
                                let _ = tx.send(VideoEvent::Error("audio decode failed".into()));
                                return;
                            }
                        }
                        if audio_priority && !audio_decoded_any && !video_queue_full {
                            allow_video_decode = true;
                        }
                    }

                    if allow_video_decode && VIDEO_DECODE_VIDEO_BUDGET_FRAMES > 0 {
                        match player.take_video_limited(VIDEO_DECODE_VIDEO_BUDGET_FRAMES) {
                            Ok(frames) => {
                                if !frames.is_empty() {
                                    did_work = true;
                                }
                                for frame in frames {
                                    if predecode_video_base_ms.is_none() {
                                        predecode_video_base_ms = Some(frame.play_ms);
                                    }
                                    if let Some(base_ms) = predecode_video_base_ms {
                                        let frame_ms = frame.play_ms.saturating_sub(base_ms) as u64;
                                        if frame_ms > predecode_video_ms {
                                            predecode_video_ms = frame_ms;
                                        }
                                    }
                                    if let Some(stats) = stats.as_ref() {
                                        stats
                                            .last_video_ms
                                            .store(frame.play_ms as u64, Ordering::Release);
                                    }
                                    if tx.send(VideoEvent::Video(frame)).is_err() {
                                        return;
                                    }
                                    queued_frames_thread.fetch_add(1, Ordering::AcqRel);
                                }
                            }
                            Err(_) => {
                                let _ = tx.send(VideoEvent::Error("video decode failed".into()));
                                return;
                            }
                        }
                    }
                }
                if let Some((writer, clock, out_channels)) = audio.as_ref() {
                    let start_clock_ms = start_clock_thread.load(Ordering::Acquire);
                    let audio_started = start_clock_ms != u64::MAX;
                    let audio_now_ms = if audio_started {
                        Some(clock.time_ms().saturating_sub(start_clock_ms))
                    } else {
                        None
                    };
                    let out_rate = clock.sample_rate();
                    let out_channels = (*out_channels).max(1);
                    let out_channels_usize = out_channels as usize;
                    let mut released_any = false;
                    if audio_started {
                        while let Some(samples) = pending_samples.pop_front() {
                            let samples_len = samples.len();
                            let frames = samples_len / out_channels_usize;
                            pending_sample_frames = pending_sample_frames.saturating_sub(frames);
                            match writer.try_push(samples) {
                                Ok(()) => {
                                    released_any = true;
                                    did_work = true;
                                }
                                Err(samples) => {
                                    if samples.len() < samples_len {
                                        released_any = true;
                                        did_work = true;
                                    }
                                    let remaining_frames = samples.len() / out_channels_usize;
                                    pending_sample_frames =
                                        pending_sample_frames.saturating_add(remaining_frames);
                                    pending_samples.push_front(samples);
                                    break;
                                }
                            }
                        }
                    }

                    if pending_samples.is_empty() || !audio_started {
                        while let Some(packet) = pending_audio.pop_front() {
                            if !audio_started
                                && prebuffer_max_frames > 0
                                && pending_sample_frames >= prebuffer_max_frames
                            {
                                pending_audio.push_front(packet);
                                break;
                            }
                            let base_ms = audio_base_ms.get_or_insert(packet.play_ms);
                            let packet_ms = packet.play_ms.saturating_sub(*base_ms) as u64;
                            if let Some(now_ms) = audio_now_ms {
                                const AUDIO_MAX_AHEAD_MS: u64 = 2000;
                                if packet_ms > now_ms + AUDIO_MAX_AHEAD_MS {
                                    pending_audio.push_front(packet);
                                    break;
                                }
                            }
                            let in_channels = packet.channels.max(1);
                            let mut samples = packet.samples;
                            let packet_frames = samples.len() / in_channels.max(1) as usize;
                            let packet_duration_ms = if packet.sample_rate > 0 {
                                (packet_frames as u64).saturating_mul(1000)
                                    / packet.sample_rate as u64
                            } else {
                                0
                            };
                            let packet_end_ms = packet_ms.saturating_add(packet_duration_ms);
                            if packet_end_ms > predecode_audio_ms {
                                predecode_audio_ms = packet_end_ms;
                            }

                            if packet.sample_rate == out_rate && in_channels == out_channels {
                                if !samples.is_empty() {
                                    if let Some(stats) = stats.as_ref() {
                                        let out_frames = samples.len() / out_channels_usize;
                                        stats
                                            .audio_frames_out
                                            .fetch_add(out_frames as u64, Ordering::AcqRel);
                                    }
                                    if out_rate > 0 {
                                        let out_frames = samples.len() / out_channels_usize;
                                        decoded_audio_frames =
                                            decoded_audio_frames.saturating_add(out_frames as u64);
                                        let decoded_ms = decoded_audio_frames.saturating_mul(1000)
                                            / out_rate as u64;
                                        last_audio_ms = Some(decoded_ms);
                                        if let Some(stats) = stats.as_ref() {
                                            stats
                                                .last_audio_ms
                                                .store(decoded_ms, Ordering::Release);
                                        }
                                    }
                                    if audio_started {
                                        let total_frames = samples.len() / out_channels_usize;
                                        let samples_len = samples.len();
                                        match writer.try_push(samples) {
                                            Ok(()) => {
                                                if let Some(stats) = stats.as_ref() {
                                                    let queued_frames = total_frames;
                                                    stats.audio_frames_queued.fetch_add(
                                                        queued_frames as u64,
                                                        Ordering::AcqRel,
                                                    );
                                                }
                                                released_any = true;
                                            }
                                            Err(samples) => {
                                                if let Some(stats) = stats.as_ref() {
                                                    let remaining_frames =
                                                        samples.len() / out_channels_usize;
                                                    let queued_frames = total_frames
                                                        .saturating_sub(remaining_frames);
                                                    stats.audio_frames_queued.fetch_add(
                                                        queued_frames as u64,
                                                        Ordering::AcqRel,
                                                    );
                                                }
                                                if samples.len() < samples_len {
                                                    released_any = true;
                                                }
                                                let remaining_frames =
                                                    samples.len() / out_channels_usize;
                                                pending_sample_frames = pending_sample_frames
                                                    .saturating_add(remaining_frames);
                                                pending_samples.push_front(samples);
                                                break;
                                            }
                                        }
                                    } else {
                                        let frames = samples.len() / out_channels_usize;
                                        pending_sample_frames =
                                            pending_sample_frames.saturating_add(frames);
                                        pending_samples.push_back(samples);
                                    }
                                }
                                continue;
                            }

                            let converter_ref = match converter.as_mut() {
                                Some(existing)
                                    if existing.matches(
                                        packet.sample_rate,
                                        out_rate,
                                        in_channels,
                                        out_channels,
                                    ) =>
                                {
                                    existing
                                }
                                _ => {
                                    match AudioConverter::new(
                                        packet.sample_rate,
                                        out_rate,
                                        in_channels,
                                        out_channels,
                                    ) {
                                        Ok(new_converter) => {
                                            converter = Some(new_converter);
                                            converter.as_mut().unwrap()
                                        }
                                        Err(err) => {
                                            let _ = tx.send(VideoEvent::Error(err));
                                            return;
                                        }
                                    }
                                }
                            };
                            samples = converter_ref.process(samples);
                            if !samples.is_empty() {
                                if let Some(stats) = stats.as_ref() {
                                    let out_frames = samples.len() / out_channels_usize;
                                    stats
                                        .audio_frames_out
                                        .fetch_add(out_frames as u64, Ordering::AcqRel);
                                }
                                if out_rate > 0 {
                                    let out_frames = samples.len() / out_channels_usize;
                                    decoded_audio_frames =
                                        decoded_audio_frames.saturating_add(out_frames as u64);
                                    let decoded_ms =
                                        decoded_audio_frames.saturating_mul(1000) / out_rate as u64;
                                    last_audio_ms = Some(decoded_ms);
                                    if let Some(stats) = stats.as_ref() {
                                        stats.last_audio_ms.store(decoded_ms, Ordering::Release);
                                    }
                                }
                                if audio_started {
                                    let total_frames = samples.len() / out_channels_usize;
                                    let samples_len = samples.len();
                                    match writer.try_push(samples) {
                                        Ok(()) => {
                                            if let Some(stats) = stats.as_ref() {
                                                let queued_frames = total_frames;
                                                stats.audio_frames_queued.fetch_add(
                                                    queued_frames as u64,
                                                    Ordering::AcqRel,
                                                );
                                            }
                                            released_any = true;
                                        }
                                        Err(samples) => {
                                            if let Some(stats) = stats.as_ref() {
                                                let remaining_frames =
                                                    samples.len() / out_channels_usize;
                                                let queued_frames =
                                                    total_frames.saturating_sub(remaining_frames);
                                                stats.audio_frames_queued.fetch_add(
                                                    queued_frames as u64,
                                                    Ordering::AcqRel,
                                                );
                                            }
                                            if samples.len() < samples_len {
                                                released_any = true;
                                            }
                                            let remaining_frames =
                                                samples.len() / out_channels_usize;
                                            pending_sample_frames = pending_sample_frames
                                                .saturating_add(remaining_frames);
                                            pending_samples.push_front(samples);
                                            break;
                                        }
                                    }
                                } else {
                                    let frames = samples.len() / out_channels_usize;
                                    pending_sample_frames =
                                        pending_sample_frames.saturating_add(frames);
                                    pending_samples.push_back(samples);
                                }
                            }
                        }
                    }
                    if let (Some(now_ms), Some(last_ms)) = (audio_now_ms, last_audio_ms) {
                        if !audio_end_sent
                            && pending_audio.is_empty()
                            && pending_samples.is_empty()
                            && now_ms > last_ms.saturating_add(500)
                        {
                            let _ = tx.send(VideoEvent::AudioEnded);
                            audio_end_sent = true;
                        }
                    }
                    if released_any {
                        did_work = true;
                    }
                    prebuffered_audio_frames_thread
                        .store(pending_sample_frames as u64, Ordering::Release);
                    if let Some(stats) = stats.as_ref() {
                        stats
                            .pending_audio_frames
                            .store(pending_sample_frames as u64, Ordering::Release);
                    }
                }

                if !decoder_finished && player.is_finished() {
                    decoder_finished = true;
                    let _ = tx.send(VideoEvent::End);
                }
                if decoder_finished
                    && pending_audio.is_empty()
                    && pending_samples.is_empty()
                    && (!audio_enabled_thread.load(Ordering::Acquire) || audio_end_sent)
                {
                    return;
                }

                if !did_work {
                    thread::sleep(Duration::from_millis(5));
                } else {
                    thread::yield_now();
                }
            }
        });

        Self {
            rx,
            stop,
            worker: Some(worker),
            start_clock_ms: None,
            start_clock_shared,
            start_instant: None,
            clock,
            video_base_ms: None,
            frames: VecDeque::new(),
            queued_frames,
            queued_video_ms,
            max_queued_video_ms,
            audio_enabled,
            prebuffered_audio_frames,
            previewed: false,
            first_frame_uploaded: false,
            finished: false,
            audio_finished: false,
            debug,
        }
    }
    fn update_queue_duration(&self) {
        let queued_ms = match (self.frames.front(), self.frames.back()) {
            (Some(front), Some(back)) => {
                let mut duration = back.play_ms.saturating_sub(front.play_ms) as u64;
                if back.fps > 0.0 {
                    let frame_ms = (1000.0 / back.fps).round() as u64;
                    duration = duration.saturating_add(frame_ms);
                }
                duration
            }
            _ => 0,
        };
        self.queued_video_ms.store(queued_ms, Ordering::Release);
    }
    pub fn drain_events(&mut self) -> Result<(), String> {
        let mut updated = false;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                VideoEvent::Video(frame) => {
                    if self.clock.is_none() && self.start_instant.is_none() {
                        self.start_instant = Some(Instant::now());
                    }
                    if self.video_base_ms.is_none() {
                        self.video_base_ms = Some(frame.play_ms);
                    }
                    self.frames.push_back(frame);
                    updated = true;
                }
                VideoEvent::AudioEnded => self.audio_finished = true,
                VideoEvent::End => self.finished = true,
                VideoEvent::Error(err) => return Err(err),
            }
        }
        if updated {
            self.update_queue_duration();
        }
        Ok(())
    }

    pub fn elapsed_ms(&self) -> u64 {
        if let (Some(clock), Some(start_ms)) = (&self.clock, self.start_clock_ms) {
            return clock.time_ms().saturating_sub(start_ms);
        }
        self.start_instant
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn is_started(&self) -> bool {
        if self.clock.is_some() {
            self.start_clock_ms.is_some()
        } else {
            self.start_instant.is_some()
        }
    }

    pub fn has_frames(&self) -> bool {
        !self.frames.is_empty()
    }

    pub fn prebuffered_audio_ms(&self) -> u64 {
        let rate = self
            .clock
            .as_ref()
            .map(|clock| clock.sample_rate())
            .unwrap_or(0);
        if rate == 0 {
            return 0;
        }
        let frames = self.prebuffered_audio_frames.load(Ordering::Acquire);
        frames.saturating_mul(1000) / rate as u64
    }

    pub fn preview_frame(&mut self) -> Option<&VideoFrame> {
        if self.previewed || self.frames.is_empty() {
            return None;
        }
        self.previewed = true;
        self.frames.front()
    }

    pub fn mark_frame_uploaded(&mut self) {
        if !self.first_frame_uploaded {
            self.first_frame_uploaded = true;
        }
    }

    pub fn is_ready_to_start(&self) -> bool {
        self.first_frame_uploaded
    }

    pub fn set_max_queued_video_ms(&self, max_ms: u64) {
        self.max_queued_video_ms.store(max_ms, Ordering::Release);
    }

    pub fn set_audio_enabled(&self, enabled: bool) {
        self.audio_enabled.store(enabled, Ordering::Release);
    }

    pub fn start_with_clock(&mut self, clock_ms: u64) {
        if self.start_clock_ms.is_none() {
            self.start_clock_ms = Some(clock_ms);
            self.start_clock_shared.store(clock_ms, Ordering::Release);
        }
    }

    pub fn start_now(&mut self) {
        if self.start_instant.is_none() {
            self.start_instant = Some(Instant::now());
        }
    }

    pub fn next_frame(&mut self, elapsed_ms: u64) -> Option<VideoFrame> {
        if self.clock.is_some() {
            self.start_clock_ms?;
        } else {
            self.start_instant?;
        }
        const LATE_FRAME_MS: u64 = 50;
        let base_ms = self.video_base_ms.unwrap_or(0);
        while let Some(frame) = self.frames.front() {
            let frame_ms = frame.play_ms.saturating_sub(base_ms) as u64;
            if frame_ms + LATE_FRAME_MS < elapsed_ms {
                self.frames.pop_front();
                self.queued_frames.fetch_sub(1, Ordering::AcqRel);
                self.update_queue_duration();
                continue;
            }
            break;
        }
        if let Some(frame) = self.frames.front() {
            let frame_ms = frame.play_ms.saturating_sub(base_ms) as u64;
            if frame_ms <= elapsed_ms {
                let frame = self.frames.pop_front();
                if frame.is_some() {
                    self.queued_frames.fetch_sub(1, Ordering::AcqRel);
                }
                self.update_queue_duration();
                return frame;
            }
        }
        None
    }

    pub fn frame_queue_len(&self) -> usize {
        self.frames.len()
    }

    pub fn debug_snapshot(&self) -> Option<VideoDebugSnapshot> {
        self.debug.as_ref().map(|stats| stats.snapshot())
    }

    pub fn is_finished(&self) -> bool {
        self.finished && self.frames.is_empty()
    }

    pub fn take_audio_finished(&mut self) -> bool {
        if self.audio_finished {
            self.audio_finished = false;
            return true;
        }
        false
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.frames.clear();
        self.queued_frames.store(0, Ordering::Release);
        self.queued_video_ms.store(0, Ordering::Release);
        self.prebuffered_audio_frames.store(0, Ordering::Release);
        self.finished = true;
    }
}
pub fn start_video_playback(
    path: PathBuf,
    audio: Option<&Rc<AudioEngine>>,
    debug: Option<Arc<VideoDebugStats>>,
    max_queued_video_ms: u64,
    audio_enabled: bool,
) -> VideoPlayback {
    let audio_config = build_audio_config(audio);
    VideoPlayback::start(
        path,
        audio_config,
        debug,
        max_queued_video_ms,
        audio_enabled,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn advance_playlist(
    current: &mut Option<VideoPlayback>,
    next: &mut Option<VideoPlayback>,
    remaining: &mut VecDeque<PlaylistEntry>,
    audio: Option<&Rc<AudioEngine>>,
    debug: Option<Arc<VideoDebugStats>>,
    next_entry_out: &mut Option<PlaylistEntry>,
    current_entry_out: &mut Option<PlaylistEntry>,
    defer_predecode: bool,
) -> bool {
    if let Some(stats) = debug.as_ref() {
        stats.reset();
    }
    if let Some(video) = current.as_mut() {
        video.stop();
    }
    *current = None;
    if let Some(audio) = audio {
        audio.clear_pcm();
    }

    let mut promoted_entry = None;
    if let Some(video) = next.take() {
        video.set_audio_enabled(true);
        video.set_max_queued_video_ms(VIDEO_PLAYBACK_WARM_MS);
        *current = Some(video);
        promoted_entry = next_entry_out.take();
    } else if let Some(entry) = remaining.pop_front() {
        *current = Some(start_video_playback(
            entry.path.clone(),
            audio,
            debug.clone(),
            VIDEO_PLAYBACK_WARM_MS,
            true,
        ));
        promoted_entry = Some(entry);
    }

    if current.is_none() {
        *current_entry_out = None;
        return false;
    }
    *current_entry_out = promoted_entry;

    if let Some(entry) = remaining.pop_front() {
        if defer_predecode {
            *next = None;
            *next_entry_out = Some(entry);
        } else {
            *next = Some(start_video_playback(
                entry.path.clone(),
                audio,
                debug.clone(),
                VIDEO_PREDECODE_WARM_MS,
                true,
            ));
            *next_entry_out = Some(entry);
        }
    } else {
        *next = None;
        *next_entry_out = None;
    }
    true
}

fn build_audio_config(audio: Option<&Rc<AudioEngine>>) -> Option<(PcmWriter, AudioClock, u32)> {
    audio.map(|engine| {
        (
            engine.pcm_writer(),
            engine.clock(),
            engine.output_channels(),
        )
    })
}

struct AudioConverter {
    converter: DataConverter,
    in_rate: u32,
    out_rate: u32,
    in_channels: u32,
    out_channels: u32,
    pending: Vec<f32>,
}

impl AudioConverter {
    fn new(
        in_rate: u32,
        out_rate: u32,
        in_channels: u32,
        out_channels: u32,
    ) -> Result<Self, String> {
        let converter = Self::build_converter(in_rate, out_rate, in_channels, out_channels)?;
        Ok(Self {
            converter,
            in_rate,
            out_rate,
            in_channels,
            out_channels,
            pending: Vec::new(),
        })
    }

    fn matches(&self, in_rate: u32, out_rate: u32, in_channels: u32, out_channels: u32) -> bool {
        self.in_rate == in_rate
            && self.out_rate == out_rate
            && self.in_channels == in_channels
            && self.out_channels == out_channels
    }

    fn build_converter(
        in_rate: u32,
        out_rate: u32,
        in_channels: u32,
        out_channels: u32,
    ) -> Result<DataConverter, String> {
        let config = DataConverterConfig::new(
            Format::F32,
            Format::F32,
            in_channels,
            out_channels,
            in_rate,
            out_rate,
        );
        DataConverter::new(&config).map_err(|err| format!("audio converter init failed: {}", err))
    }

    fn process(&mut self, samples: Vec<f32>) -> Vec<f32> {
        if samples.is_empty() || self.in_channels == 0 || self.out_channels == 0 {
            return Vec::new();
        }

        let mut samples = if self.pending.is_empty() {
            samples
        } else {
            let mut combined = std::mem::take(&mut self.pending);
            combined.extend_from_slice(&samples);
            combined
        };

        let in_channels = self.in_channels as usize;
        let aligned_len = samples.len() / in_channels * in_channels;
        if aligned_len == 0 {
            self.pending = samples;
            return Vec::new();
        }

        let mut tail = Vec::new();
        if aligned_len < samples.len() {
            tail = samples.split_off(aligned_len);
        }

        let in_frames = aligned_len / in_channels;
        let expected_out_frames =
            self.converter.expected_output_frame_count(in_frames as u64) as usize;
        if expected_out_frames == 0 {
            self.pending = tail;
            return Vec::new();
        }

        let mut out = vec![0.0f32; expected_out_frames * self.out_channels as usize];
        let input = Frames::wrap::<f32>(&samples, Format::F32, self.in_channels);
        let mut output = FramesMut::wrap::<f32>(&mut out, Format::F32, self.out_channels);
        let (out_frames, in_frames_used) =
            match self.converter.process_pcm_frames(&mut output, &input) {
                Ok(result) => result,
                Err(_) => {
                    self.pending = tail;
                    return Vec::new();
                }
            };

        let used_samples = in_frames_used as usize * in_channels;
        if used_samples < samples.len() {
            let mut remainder = samples.split_off(used_samples);
            remainder.extend_from_slice(&tail);
            self.pending = remainder;
        } else {
            self.pending = tail;
        }

        out.truncate(out_frames as usize * self.out_channels as usize);
        out
    }
}
