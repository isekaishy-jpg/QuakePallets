use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::asset_id::AssetKey;
use crate::asset_resolver::{AssetResolver, ResolvedPath};
use crate::jobs::{JobError, JobHandle, JobQueue, Jobs, JobsConfig};
use crate::logging;
use crate::path_policy::PathPolicy;
use crate::vfs::Vfs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetStatus {
    Queued,
    Loading,
    Ready,
    Failed,
}

impl AssetStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetStatus::Queued => "queued",
            AssetStatus::Loading => "loading",
            AssetStatus::Ready => "ready",
            AssetStatus::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetPriority {
    High,
    Normal,
    Low,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssetBudgetTag {
    Boot,
    Streaming,
    Background,
}

#[derive(Clone, Copy, Debug)]
pub enum SyncLoadPolicy {
    Allow,
    Warn,
    Panic,
}

#[derive(Clone, Copy, Debug)]
pub struct RequestOpts {
    pub priority: AssetPriority,
    pub budget_tag: AssetBudgetTag,
}

impl Default for RequestOpts {
    fn default() -> Self {
        Self {
            priority: AssetPriority::Normal,
            budget_tag: AssetBudgetTag::Streaming,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    EngineConfig,
    EngineScript,
    EngineText,
    EngineBlob,
    Quake1Raw,
    EngineTexture,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::EngineConfig => "engine:config",
            AssetKind::EngineScript => "engine:script",
            AssetKind::EngineText => "engine:text",
            AssetKind::EngineBlob => "engine:blob",
            AssetKind::Quake1Raw => "quake1:raw",
            AssetKind::EngineTexture => "engine:texture",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssetBudgetTelemetry {
    pub spent_boot_ms: u64,
    pub spent_streaming_ms: u64,
    pub spent_background_ms: u64,
    pub throttled_boot: u64,
    pub throttled_streaming: u64,
    pub throttled_background: u64,
}

#[derive(Clone, Debug)]
pub struct AssetMetricsSnapshot {
    pub status: AssetStatus,
    pub decoded_bytes: usize,
    pub decode_ms: Option<u64>,
    pub load_ms: Option<u64>,
    pub error: Option<String>,
    pub version: u64,
    pub content_hash: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct AssetEntrySnapshot {
    pub key: AssetKey,
    pub kind: AssetKind,
    pub metrics: AssetMetricsSnapshot,
}

pub struct TextAsset {
    pub text: String,
}

pub struct ConfigAsset {
    pub text: String,
}

pub struct ScriptAsset {
    pub text: String,
}

pub struct BlobAsset {
    pub bytes: Arc<Vec<u8>>,
}

pub struct QuakeRawAsset {
    pub bytes: Arc<Vec<u8>>,
}

pub struct TextureAsset {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

pub trait AssetPayload: Sized + Any + Send + Sync + 'static {
    const KIND: AssetKind;
    fn accepts(key: &AssetKey) -> bool;
    fn decode(key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String>;
    fn decoded_size(&self) -> usize;
}

impl AssetPayload for TextAsset {
    const KIND: AssetKind = AssetKind::EngineText;

    fn accepts(key: &AssetKey) -> bool {
        key.namespace() == "engine" && key.kind() == "text"
    }

    fn decode(_key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String> {
        let text = String::from_utf8(bytes).map_err(|err| err.to_string())?;
        Ok(Self { text })
    }

    fn decoded_size(&self) -> usize {
        self.text.len()
    }
}

impl AssetPayload for ConfigAsset {
    const KIND: AssetKind = AssetKind::EngineConfig;

    fn accepts(key: &AssetKey) -> bool {
        key.namespace() == "engine" && key.kind() == "config"
    }

    fn decode(_key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String> {
        let text = String::from_utf8(bytes).map_err(|err| err.to_string())?;
        Ok(Self { text })
    }

    fn decoded_size(&self) -> usize {
        self.text.len()
    }
}

impl AssetPayload for ScriptAsset {
    const KIND: AssetKind = AssetKind::EngineScript;

    fn accepts(key: &AssetKey) -> bool {
        key.namespace() == "engine" && key.kind() == "script"
    }

    fn decode(_key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String> {
        let text = String::from_utf8(bytes).map_err(|err| err.to_string())?;
        Ok(Self { text })
    }

    fn decoded_size(&self) -> usize {
        self.text.len()
    }
}

impl AssetPayload for BlobAsset {
    const KIND: AssetKind = AssetKind::EngineBlob;

    fn accepts(key: &AssetKey) -> bool {
        key.namespace() == "engine" && key.kind() == "blob"
    }

    fn decode(_key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            bytes: Arc::new(bytes),
        })
    }

    fn decoded_size(&self) -> usize {
        self.bytes.len()
    }
}

impl AssetPayload for QuakeRawAsset {
    const KIND: AssetKind = AssetKind::Quake1Raw;

    fn accepts(key: &AssetKey) -> bool {
        key.namespace() == "quake1" && key.kind() == "raw"
    }

    fn decode(_key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            bytes: Arc::new(bytes),
        })
    }

    fn decoded_size(&self) -> usize {
        self.bytes.len()
    }
}

impl AssetPayload for TextureAsset {
    const KIND: AssetKind = AssetKind::EngineTexture;

    fn accepts(key: &AssetKey) -> bool {
        key.namespace() == "engine" && key.kind() == "texture"
    }

    fn decode(key: &AssetKey, bytes: Vec<u8>) -> Result<Self, String> {
        let extension = key
            .path()
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .unwrap_or("");
        if extension != "png" {
            return Err(format!("unsupported texture extension '{}'", extension));
        }
        decode_png(bytes)
    }

    fn decoded_size(&self) -> usize {
        self.rgba.len()
    }
}

pub struct Handle<T> {
    slot: Arc<AssetSlot>,
    marker: PhantomData<T>,
}

impl<T> Handle<T> {
    pub fn status(&self) -> AssetStatus {
        let guard = self.slot.state.lock().expect("asset slot lock poisoned");
        guard.status
    }

    pub fn key(&self) -> AssetKey {
        self.slot.key.clone()
    }

    pub fn get(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let guard = self.slot.state.lock().expect("asset slot lock poisoned");
        let value = guard.value.as_ref()?;
        let value = Arc::clone(value);
        Arc::downcast::<T>(value).ok()
    }

    pub fn error(&self) -> Option<String> {
        let guard = self.slot.state.lock().expect("asset slot lock poisoned");
        guard.error.clone()
    }

    pub fn metrics(&self) -> AssetMetricsSnapshot {
        let guard = self.slot.state.lock().expect("asset slot lock poisoned");
        guard.snapshot()
    }

    pub fn cancel(&self) {
        self.slot.cancel();
    }
}

#[derive(Clone)]
pub struct AssetManager {
    inner: Arc<AssetManagerInner>,
}

struct AssetManagerInner {
    jobs: Arc<Jobs>,
    path_policy: PathPolicy,
    vfs: Option<Arc<Vfs>>,
    config: Mutex<AssetManagerConfig>,
    state: Mutex<AssetManagerState>,
    sim_tick_active: AtomicBool,
}

#[derive(Clone, Debug)]
struct AssetManagerConfig {
    decode_budget_ms_per_tick: u64,
    sync_policy: SyncLoadPolicy,
}

#[derive(Default)]
struct AssetManagerState {
    entries: HashMap<AssetKey, Arc<AssetSlot>>,
    pending_high: VecDeque<PendingRequest>,
    pending_normal: VecDeque<PendingRequest>,
    pending_low: VecDeque<PendingRequest>,
    budget: BudgetTracker,
}

#[derive(Default)]
struct BudgetTracker {
    spent_boot_ms: u64,
    spent_streaming_ms: u64,
    spent_background_ms: u64,
    throttled_boot: u64,
    throttled_streaming: u64,
    throttled_background: u64,
}

impl BudgetTracker {
    fn reset(&mut self) {
        *self = BudgetTracker::default();
    }

    fn record_spent(&mut self, tag: AssetBudgetTag, ms: u64) {
        match tag {
            AssetBudgetTag::Boot => self.spent_boot_ms = self.spent_boot_ms.saturating_add(ms),
            AssetBudgetTag::Streaming => {
                self.spent_streaming_ms = self.spent_streaming_ms.saturating_add(ms)
            }
            AssetBudgetTag::Background => {
                self.spent_background_ms = self.spent_background_ms.saturating_add(ms)
            }
        }
    }

    fn record_throttle(&mut self, tag: AssetBudgetTag) {
        match tag {
            AssetBudgetTag::Boot => self.throttled_boot = self.throttled_boot.saturating_add(1),
            AssetBudgetTag::Streaming => {
                self.throttled_streaming = self.throttled_streaming.saturating_add(1)
            }
            AssetBudgetTag::Background => {
                self.throttled_background = self.throttled_background.saturating_add(1)
            }
        }
    }
}

struct AssetSlot {
    key: AssetKey,
    kind: AssetKind,
    cancelled: AtomicBool,
    state: Mutex<AssetSlotState>,
}

struct AssetSlotState {
    status: AssetStatus,
    value: Option<Arc<dyn Any + Send + Sync>>,
    error: Option<String>,
    decode_ms: Option<u64>,
    load_started: Option<Instant>,
    load_finished: Option<Instant>,
    decoded_bytes: usize,
    version: u64,
    content_hash: Option<u64>,
    pending: bool,
    in_flight: bool,
    job_handle: Option<JobHandle>,
    resolved_path: Option<ResolvedPath>,
    retain_on_failure: bool,
}

impl AssetSlotState {
    fn snapshot(&self) -> AssetMetricsSnapshot {
        let load_ms = match (self.load_started, self.load_finished) {
            (Some(start), Some(end)) => Some(end.duration_since(start).as_millis() as u64),
            _ => None,
        };
        AssetMetricsSnapshot {
            status: self.status,
            decoded_bytes: self.decoded_bytes,
            decode_ms: self.decode_ms,
            load_ms,
            error: self.error.clone(),
            version: self.version,
            content_hash: self.content_hash,
        }
    }
}

impl AssetSlot {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let guard = self.state.lock().expect("asset slot lock poisoned");
        if let Some(handle) = guard.job_handle.as_ref() {
            handle.cancel();
        }
        if guard.status != AssetStatus::Ready {
            drop(guard);
            self.fail("asset load cancelled");
        }
    }
}

struct PendingRequest {
    slot: Arc<AssetSlot>,
    opts: RequestOpts,
}

pub struct RequestOutcome<T> {
    pub handle: Handle<T>,
    pub cache_hit: bool,
}

impl AssetManager {
    pub fn new(path_policy: PathPolicy, vfs: Option<Arc<Vfs>>, jobs: Option<Arc<Jobs>>) -> Self {
        let jobs = jobs.unwrap_or_else(|| Arc::new(Jobs::new(JobsConfig::threaded(2, 2, 64))));
        let inner = AssetManagerInner {
            jobs,
            path_policy,
            vfs,
            config: Mutex::new(AssetManagerConfig {
                decode_budget_ms_per_tick: 8,
                sync_policy: SyncLoadPolicy::Warn,
            }),
            state: Mutex::new(AssetManagerState::default()),
            sim_tick_active: AtomicBool::new(false),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn jobs(&self) -> Arc<Jobs> {
        Arc::clone(&self.inner.jobs)
    }

    pub fn set_decode_budget_ms_per_tick(&self, budget_ms: u64) {
        let mut guard = self
            .inner
            .config
            .lock()
            .expect("asset config lock poisoned");
        guard.decode_budget_ms_per_tick = budget_ms;
    }

    pub fn set_sync_load_policy(&self, policy: SyncLoadPolicy) {
        let mut guard = self
            .inner
            .config
            .lock()
            .expect("asset config lock poisoned");
        let effective = if matches!(policy, SyncLoadPolicy::Panic) && !cfg!(debug_assertions) {
            logging::warn("sync load panic policy is disabled in release builds; using warn");
            SyncLoadPolicy::Warn
        } else {
            policy
        };
        guard.sync_policy = effective;
    }

    pub fn begin_tick(&self) {
        let mut state = self.inner.state.lock().expect("asset state lock poisoned");
        state.budget.reset();
    }

    pub fn enter_sim_tick(&self) {
        self.inner.sim_tick_active.store(true, Ordering::Relaxed);
    }

    pub fn exit_sim_tick(&self) {
        self.inner.sim_tick_active.store(false, Ordering::Relaxed);
    }

    pub fn budget_telemetry(&self) -> AssetBudgetTelemetry {
        let state = self.inner.state.lock().expect("asset state lock poisoned");
        AssetBudgetTelemetry {
            spent_boot_ms: state.budget.spent_boot_ms,
            spent_streaming_ms: state.budget.spent_streaming_ms,
            spent_background_ms: state.budget.spent_background_ms,
            throttled_boot: state.budget.throttled_boot,
            throttled_streaming: state.budget.throttled_streaming,
            throttled_background: state.budget.throttled_background,
        }
    }

    pub fn list_assets(&self) -> Vec<AssetEntrySnapshot> {
        let slots: Vec<Arc<AssetSlot>> = {
            let state = self.inner.state.lock().expect("asset state lock poisoned");
            state.entries.values().cloned().collect()
        };
        let mut entries = Vec::new();
        for slot in slots {
            let guard = slot.state.lock().expect("asset slot lock poisoned");
            entries.push(AssetEntrySnapshot {
                key: slot.key.clone(),
                kind: slot.kind,
                metrics: guard.snapshot(),
            });
        }
        entries.sort_by(|a, b| a.key.canonical().cmp(b.key.canonical()));
        entries
    }

    pub fn asset_snapshot(&self, key: &AssetKey) -> Option<AssetEntrySnapshot> {
        let slot = {
            let state = self.inner.state.lock().expect("asset state lock poisoned");
            state.entries.get(key).cloned()
        }?;
        let guard = slot.state.lock().expect("asset slot lock poisoned");
        Some(AssetEntrySnapshot {
            key: slot.key.clone(),
            kind: slot.kind,
            metrics: guard.snapshot(),
        })
    }

    pub fn request<T: AssetPayload>(&self, key: AssetKey, opts: RequestOpts) -> Handle<T> {
        self.request_with_outcome::<T>(key, opts).handle
    }

    pub fn request_with_outcome<T: AssetPayload>(
        &self,
        key: AssetKey,
        opts: RequestOpts,
    ) -> RequestOutcome<T> {
        let mut state = self.inner.state.lock().expect("asset state lock poisoned");
        if let Some(slot) = state.entries.get(&key) {
            if slot.kind != T::KIND {
                return RequestOutcome {
                    handle: failed_handle::<T>(
                        key,
                        "asset key already cached with a different kind",
                    ),
                    cache_hit: false,
                };
            }
            let guard = slot.state.lock().expect("asset slot lock poisoned");
            let cache_hit = guard.status == AssetStatus::Ready;
            return RequestOutcome {
                handle: Handle {
                    slot: Arc::clone(slot),
                    marker: PhantomData,
                },
                cache_hit,
            };
        }

        let slot = Arc::new(AssetSlot {
            key: key.clone(),
            kind: T::KIND,
            cancelled: AtomicBool::new(false),
            state: Mutex::new(AssetSlotState {
                status: AssetStatus::Queued,
                value: None,
                error: None,
                decode_ms: None,
                load_started: None,
                load_finished: None,
                decoded_bytes: 0,
                version: 0,
                content_hash: None,
                pending: false,
                in_flight: false,
                job_handle: None,
                resolved_path: None,
                retain_on_failure: false,
            }),
        });
        state.entries.insert(key.clone(), Arc::clone(&slot));

        if !T::accepts(&key) {
            {
                let mut guard = slot.state.lock().expect("asset slot lock poisoned");
                guard.status = AssetStatus::Failed;
                guard.error = Some(format!("asset key not valid for this type: {}", key));
            }
            return RequestOutcome {
                handle: Handle {
                    slot,
                    marker: PhantomData,
                },
                cache_hit: false,
            };
        }

        let resolver = AssetResolver::new(&self.inner.path_policy, self.inner.vfs.as_deref());
        let resolved = match resolver.resolve(&key) {
            Ok(location) => location,
            Err(err) => {
                {
                    let mut guard = slot.state.lock().expect("asset slot lock poisoned");
                    guard.status = AssetStatus::Failed;
                    guard.error = Some(err);
                }
                return RequestOutcome {
                    handle: Handle {
                        slot,
                        marker: PhantomData,
                    },
                    cache_hit: false,
                };
            }
        };

        {
            let mut guard = slot.state.lock().expect("asset slot lock poisoned");
            guard.pending = true;
            guard.status = AssetStatus::Queued;
            guard.resolved_path = Some(resolved.path);
        }

        enqueue_request(&mut state, PendingRequest { slot, opts });

        RequestOutcome {
            handle: Handle {
                slot: Arc::clone(state.entries.get(&key).expect("slot missing")),
                marker: PhantomData,
            },
            cache_hit: false,
        }
    }

    pub fn reload<T: AssetPayload>(
        &self,
        key: AssetKey,
        opts: RequestOpts,
    ) -> Result<Handle<T>, String> {
        if !T::accepts(&key) {
            return Err(format!("asset key not valid for this type: {}", key));
        }
        let mut state = self.inner.state.lock().expect("asset state lock poisoned");
        if let Some(slot) = state.entries.get(&key).cloned() {
            if slot.kind != T::KIND {
                return Err("asset key already cached with a different kind".to_string());
            }
            {
                let guard = slot.state.lock().expect("asset slot lock poisoned");
                if guard.pending || guard.in_flight {
                    return Ok(Handle {
                        slot: Arc::clone(&slot),
                        marker: PhantomData,
                    });
                }
            }
            let resolver = AssetResolver::new(&self.inner.path_policy, self.inner.vfs.as_deref());
            let resolved = resolver.resolve(&key)?;
            {
                let mut guard = slot.state.lock().expect("asset slot lock poisoned");
                guard.pending = true;
                guard.in_flight = false;
                guard.status = AssetStatus::Queued;
                guard.error = None;
                guard.decode_ms = None;
                guard.load_started = None;
                guard.load_finished = None;
                guard.job_handle = None;
                guard.resolved_path = Some(resolved.path);
                guard.retain_on_failure = guard.value.is_some();
            }
            slot.cancelled.store(false, Ordering::Relaxed);
            enqueue_request(
                &mut state,
                PendingRequest {
                    slot: Arc::clone(&slot),
                    opts,
                },
            );
            return Ok(Handle {
                slot,
                marker: PhantomData,
            });
        }
        drop(state);
        Ok(self.request::<T>(key, opts))
    }

    pub fn pump(&self) -> usize {
        let completed = self.inner.jobs.pump_completions();
        self.drain_pending();
        completed
    }

    pub fn await_ready<T: AssetPayload>(
        &self,
        handle: &Handle<T>,
        timeout: Duration,
    ) -> Result<Arc<T>, String> {
        self.guard_sync_load();
        let start = Instant::now();
        loop {
            if let Some(value) = handle.get() {
                return Ok(value);
            }
            if handle.status() == AssetStatus::Failed {
                return Err(handle
                    .error()
                    .unwrap_or_else(|| "asset load failed".to_string()));
            }
            if start.elapsed() > timeout {
                return Err("asset load timeout".to_string());
            }
            self.pump();
            std::thread::yield_now();
        }
    }

    pub fn purge(&self, key: &AssetKey) -> bool {
        let mut state = self.inner.state.lock().expect("asset state lock poisoned");
        if let Some(slot) = state.entries.remove(key) {
            let mut guard = slot.state.lock().expect("asset slot lock poisoned");
            guard.status = AssetStatus::Failed;
            guard.error = Some("asset purged".to_string());
            return true;
        }
        false
    }

    fn drain_pending(&self) {
        let mut state = self.inner.state.lock().expect("asset state lock poisoned");
        let config = self
            .inner
            .config
            .lock()
            .expect("asset config lock poisoned");
        let mut pending_high = std::mem::take(&mut state.pending_high);
        schedule_queue(
            &self.inner,
            &mut state,
            &config,
            AssetPriority::High,
            &mut pending_high,
        );
        state.pending_high = pending_high;

        let mut pending_normal = std::mem::take(&mut state.pending_normal);
        schedule_queue(
            &self.inner,
            &mut state,
            &config,
            AssetPriority::Normal,
            &mut pending_normal,
        );
        state.pending_normal = pending_normal;

        let mut pending_low = std::mem::take(&mut state.pending_low);
        schedule_queue(
            &self.inner,
            &mut state,
            &config,
            AssetPriority::Low,
            &mut pending_low,
        );
        state.pending_low = pending_low;
    }

    fn guard_sync_load(&self) {
        if !self.inner.sim_tick_active.load(Ordering::Relaxed) {
            return;
        }
        let policy = {
            let guard = self
                .inner
                .config
                .lock()
                .expect("asset config lock poisoned");
            guard.sync_policy
        };
        match policy {
            SyncLoadPolicy::Allow => {}
            SyncLoadPolicy::Warn => {
                logging::warn("sync asset load attempted during sim tick");
            }
            SyncLoadPolicy::Panic => {
                panic!("sync asset load attempted during sim tick");
            }
        }
    }
}

fn failed_handle<T: AssetPayload>(key: AssetKey, error: &str) -> Handle<T> {
    let slot = Arc::new(AssetSlot {
        key,
        kind: T::KIND,
        cancelled: AtomicBool::new(false),
        state: Mutex::new(AssetSlotState {
            status: AssetStatus::Failed,
            value: None,
            error: Some(error.to_string()),
            decode_ms: None,
            load_started: None,
            load_finished: Some(Instant::now()),
            decoded_bytes: 0,
            version: 0,
            content_hash: None,
            pending: false,
            in_flight: false,
            job_handle: None,
            resolved_path: None,
            retain_on_failure: false,
        }),
    });
    Handle {
        slot,
        marker: PhantomData,
    }
}

fn schedule_queue(
    inner: &Arc<AssetManagerInner>,
    state: &mut AssetManagerState,
    config: &AssetManagerConfig,
    priority: AssetPriority,
    queue: &mut VecDeque<PendingRequest>,
) {
    let mut remaining = queue.len();
    while remaining > 0 {
        remaining -= 1;
        let Some(request) = queue.pop_front() else {
            break;
        };
        if should_throttle(config, &state.budget, priority, request.opts.budget_tag) {
            state.budget.record_throttle(request.opts.budget_tag);
            queue.push_back(request);
            continue;
        }
        if !dispatch_request(inner, &request) {
            queue.push_back(request);
        }
    }
}

fn should_throttle(
    config: &AssetManagerConfig,
    budget: &BudgetTracker,
    priority: AssetPriority,
    tag: AssetBudgetTag,
) -> bool {
    // Policy: High priority and Boot-tagged work bypass budgets. When the global budget
    // is exceeded, Background and Low priority requests are throttled first.
    if priority == AssetPriority::High {
        return false;
    }
    if tag == AssetBudgetTag::Boot {
        return false;
    }
    let budget_ms = config.decode_budget_ms_per_tick;
    if budget_ms == 0 {
        return true;
    }
    let total_spent = budget
        .spent_boot_ms
        .saturating_add(budget.spent_streaming_ms)
        .saturating_add(budget.spent_background_ms);
    if total_spent >= budget_ms {
        return tag == AssetBudgetTag::Background || priority == AssetPriority::Low;
    }
    false
}

fn dispatch_request(inner: &Arc<AssetManagerInner>, request: &PendingRequest) -> bool {
    let resolved = {
        let guard = request.slot.state.lock().expect("asset slot lock poisoned");
        guard.resolved_path.clone()
    };
    let resolved = match resolved {
        Some(path) => path,
        None => {
            request.slot.fail("missing resolved path");
            return true;
        }
    };

    if request.slot.cancelled.load(Ordering::Relaxed) {
        request.slot.fail("asset load cancelled");
        return true;
    }

    request.slot.mark_loading();
    let jobs = Arc::clone(&inner.jobs);
    let jobs_for_cpu = Arc::clone(&jobs);
    let vfs = inner.vfs.clone();
    let slot = Arc::clone(&request.slot);
    let key = request.slot.key.clone();
    let budget_tag = request.opts.budget_tag;
    let inner_for_budget = Arc::clone(inner);

    let io_result = jobs.submit(
        JobQueue::Io,
        move || read_bytes(&resolved, vfs.as_deref()),
        move |result| {
            let jobs = Arc::clone(&jobs_for_cpu);
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(err) => {
                    slot.fail(&err);
                    return;
                }
            };
            if slot.cancelled.load(Ordering::Relaxed) {
                slot.fail("asset load cancelled");
                return;
            }
            let slot_for_cpu = Arc::clone(&slot);
            let slot_for_complete = Arc::clone(&slot_for_cpu);
            let slot_for_decode = Arc::clone(&slot_for_cpu);
            let key_for_decode = key.clone();
            let decode_started = Instant::now();
            let inner_for_budget = Arc::clone(&inner_for_budget);
            let cpu_result = jobs.submit(
                JobQueue::Cpu,
                move || decode_for_kind(slot_for_decode.kind, &key_for_decode, bytes),
                move |result| {
                    let decode_ms = decode_started.elapsed().as_millis() as u64;
                    match result {
                        Ok(decoded) => slot_for_complete.finish(decoded, decode_ms),
                        Err(err) => slot_for_complete.fail(&err),
                    }
                    let mut state = inner_for_budget
                        .state
                        .lock()
                        .expect("asset state lock poisoned");
                    state.budget.record_spent(budget_tag, decode_ms);
                },
            );
            match cpu_result {
                Ok(handle) => slot_for_cpu.set_job_handle(handle),
                Err(err) => slot_for_cpu.fail(&format!("cpu queue error: {}", err)),
            }
        },
    );
    match io_result {
        Ok(handle) => {
            request.slot.set_job_handle(handle);
            true
        }
        Err(JobError::QueueFull(_)) => {
            request.slot.mark_queued();
            false
        }
        Err(err) => {
            request.slot.fail(&format!("io queue error: {}", err));
            true
        }
    }
}

fn read_bytes(resolved: &ResolvedPath, vfs: Option<&Vfs>) -> Result<Vec<u8>, String> {
    match resolved {
        ResolvedPath::File(path) => std::fs::read(path).map_err(|err| err.to_string()),
        ResolvedPath::Vfs(path) => {
            let vfs = vfs.ok_or_else(|| "vfs not configured".to_string())?;
            vfs.read(path).map_err(|err| err.to_string())
        }
        ResolvedPath::Bundle { .. } => Err("bundle assets not implemented".to_string()),
    }
}

struct DecodedPayload {
    value: Arc<dyn Any + Send + Sync>,
    decoded_bytes: usize,
    content_hash: u64,
}

fn decode_for_kind(
    kind: AssetKind,
    key: &AssetKey,
    bytes: Vec<u8>,
) -> Result<DecodedPayload, String> {
    let content_hash = fnv1a64(&bytes);
    match kind {
        AssetKind::EngineConfig => {
            let asset = ConfigAsset::decode(key, bytes)?;
            let bytes = asset.decoded_size();
            Ok(DecodedPayload {
                value: Arc::new(asset),
                decoded_bytes: bytes,
                content_hash,
            })
        }
        AssetKind::EngineScript => {
            let asset = ScriptAsset::decode(key, bytes)?;
            let bytes = asset.decoded_size();
            Ok(DecodedPayload {
                value: Arc::new(asset),
                decoded_bytes: bytes,
                content_hash,
            })
        }
        AssetKind::EngineText => {
            let asset = TextAsset::decode(key, bytes)?;
            let bytes = asset.decoded_size();
            Ok(DecodedPayload {
                value: Arc::new(asset),
                decoded_bytes: bytes,
                content_hash,
            })
        }
        AssetKind::EngineBlob => {
            let asset = BlobAsset::decode(key, bytes)?;
            let bytes = asset.decoded_size();
            Ok(DecodedPayload {
                value: Arc::new(asset),
                decoded_bytes: bytes,
                content_hash,
            })
        }
        AssetKind::Quake1Raw => {
            let asset = QuakeRawAsset::decode(key, bytes)?;
            let bytes = asset.decoded_size();
            Ok(DecodedPayload {
                value: Arc::new(asset),
                decoded_bytes: bytes,
                content_hash,
            })
        }
        AssetKind::EngineTexture => {
            let asset = TextureAsset::decode(key, bytes)?;
            let bytes = asset.decoded_size();
            Ok(DecodedPayload {
                value: Arc::new(asset),
                decoded_bytes: bytes,
                content_hash,
            })
        }
    }
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl AssetSlot {
    fn mark_loading(&self) {
        let mut guard = self.state.lock().expect("asset slot lock poisoned");
        guard.status = AssetStatus::Loading;
        guard.pending = false;
        guard.in_flight = true;
        guard.load_started = Some(Instant::now());
    }

    fn mark_queued(&self) {
        let mut guard = self.state.lock().expect("asset slot lock poisoned");
        guard.status = AssetStatus::Queued;
        guard.pending = true;
        guard.in_flight = false;
    }

    fn set_job_handle(&self, handle: JobHandle) {
        let mut guard = self.state.lock().expect("asset slot lock poisoned");
        guard.job_handle = Some(handle);
    }

    fn finish(&self, decoded: DecodedPayload, decode_ms: u64) {
        let mut guard = self.state.lock().expect("asset slot lock poisoned");
        guard.value = Some(decoded.value);
        guard.decoded_bytes = decoded.decoded_bytes;
        guard.decode_ms = Some(decode_ms);
        guard.content_hash = Some(decoded.content_hash);
        guard.version = guard.version.saturating_add(1);
        guard.error = None;
        guard.status = AssetStatus::Ready;
        guard.in_flight = false;
        guard.pending = false;
        guard.retain_on_failure = false;
        guard.load_finished = Some(Instant::now());
    }

    fn fail(&self, message: &str) {
        let mut guard = self.state.lock().expect("asset slot lock poisoned");
        let keep_previous = guard.retain_on_failure && guard.value.is_some();
        guard.error = Some(message.to_string());
        guard.status = if keep_previous {
            AssetStatus::Ready
        } else {
            AssetStatus::Failed
        };
        guard.in_flight = false;
        guard.pending = false;
        guard.retain_on_failure = false;
        guard.load_finished = Some(Instant::now());
    }
}

fn enqueue_request(state: &mut AssetManagerState, request: PendingRequest) {
    match request.opts.priority {
        AssetPriority::High => state.pending_high.push_back(request),
        AssetPriority::Normal => state.pending_normal.push_back(request),
        AssetPriority::Low => state.pending_low.push_back(request),
    }
}

fn decode_png(bytes: Vec<u8>) -> Result<TextureAsset, String> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|err| err.to_string())?;
    let info = reader.info();
    if info.bit_depth != png::BitDepth::Eight {
        return Err("png bit depth must be 8".to_string());
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let output = reader.next_frame(&mut buf).map_err(|err| err.to_string())?;
    let bytes = &buf[..output.buffer_size()];
    let rgba = match output.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(output.width as usize * output.height as usize * 4);
            for chunk in bytes.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(output.width as usize * output.height as usize * 4);
            for value in bytes {
                out.extend_from_slice(&[*value, *value, *value, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(output.width as usize * output.height as usize * 4);
            for chunk in bytes.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            out
        }
        _ => return Err("unsupported png color type".to_string()),
    };
    Ok(TextureAsset {
        width: output.width,
        height: output.height,
        rgba: Arc::new(rgba),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Jobs, JobsConfig};
    use crate::path_policy::{PathOverrides, PathPolicy};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    fn fixture_policy() -> PathPolicy {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root")
            .to_path_buf();
        let fixture_root = repo_root.join("content").join("fixtures").join("golden");
        PathPolicy::from_overrides(PathOverrides {
            content_root: Some(fixture_root),
            dev_override_root: None,
            user_config_root: None,
        })
    }

    #[test]
    fn coalesces_requests_and_cache_hits() {
        let jobs = Arc::new(Jobs::new(JobsConfig::inline()));
        let asset_manager = AssetManager::new(fixture_policy(), None, Some(jobs));
        let key = AssetKey::parse("engine:text/fixtures/golden.cfg").expect("asset id");

        let first =
            asset_manager.request_with_outcome::<TextAsset>(key.clone(), RequestOpts::default());
        let second =
            asset_manager.request_with_outcome::<TextAsset>(key.clone(), RequestOpts::default());

        assert!(!first.cache_hit);
        assert!(!second.cache_hit);
        assert_eq!(asset_manager.list_assets().len(), 1);

        asset_manager
            .await_ready(&first.handle, Duration::from_secs(1))
            .expect("asset load");

        let third = asset_manager.request_with_outcome::<TextAsset>(key, RequestOpts::default());
        assert!(third.cache_hit);
    }

    #[test]
    fn backpressure_keeps_request_queued() {
        let jobs = Arc::new(Jobs::new(JobsConfig::threaded(1, 1, 1)));
        let (started_tx, started_rx) = mpsc::channel();
        let (block_tx, block_rx) = mpsc::channel();
        let (block_tx2, block_rx2) = mpsc::channel();

        jobs.submit(
            JobQueue::Io,
            move || {
                let _ = started_tx.send(());
                let _ = block_rx.recv();
            },
            |_| {},
        )
        .expect("queue job");

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker started");

        jobs.submit(
            JobQueue::Io,
            move || {
                let _ = block_rx2.recv();
            },
            |_| {},
        )
        .expect("queue job");

        let asset_manager = AssetManager::new(fixture_policy(), None, Some(Arc::clone(&jobs)));
        let key = AssetKey::parse("engine:text/fixtures/golden.cfg").expect("asset id");
        let handle = asset_manager.request::<TextAsset>(key, RequestOpts::default());
        asset_manager.pump();

        assert_eq!(handle.status(), AssetStatus::Queued);

        let _ = block_tx.send(());
        let _ = block_tx2.send(());
        let _ = jobs.pump_completions();
    }
}
