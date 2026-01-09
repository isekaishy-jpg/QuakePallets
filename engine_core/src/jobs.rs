use std::collections::VecDeque;
use std::fmt;
use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::observability;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobQueue {
    Io,
    Cpu,
}

impl fmt::Display for JobQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            JobQueue::Io => "io",
            JobQueue::Cpu => "cpu",
        };
        write!(f, "{}", label)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobsMode {
    Threaded,
    Inline,
}

#[derive(Clone, Copy, Debug)]
pub struct JobsConfig {
    pub mode: JobsMode,
    pub io_workers: usize,
    pub cpu_workers: usize,
    pub io_queue_capacity: usize,
    pub cpu_queue_capacity: usize,
}

impl JobsConfig {
    pub fn threaded(io_workers: usize, cpu_workers: usize, queue_capacity: usize) -> Self {
        Self {
            mode: JobsMode::Threaded,
            io_workers: io_workers.max(1),
            cpu_workers: cpu_workers.max(1),
            io_queue_capacity: queue_capacity.max(1),
            cpu_queue_capacity: queue_capacity.max(1),
        }
    }

    pub fn inline() -> Self {
        Self {
            mode: JobsMode::Inline,
            io_workers: 0,
            cpu_workers: 0,
            io_queue_capacity: 0,
            cpu_queue_capacity: 0,
        }
    }
}

// Backpressure policy: fail fast when a queue is at capacity.
#[derive(Debug)]
pub enum JobError {
    QueueFull(JobQueue),
    QueueClosed(JobQueue),
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::QueueFull(queue) => write!(f, "{} queue full", queue),
            JobError::QueueClosed(queue) => write!(f, "{} queue closed", queue),
        }
    }
}

impl std::error::Error for JobError {}

#[derive(Clone, Debug)]
pub struct JobsTelemetry {
    pub io_queue_depth: usize,
    pub cpu_queue_depth: usize,
    pub io_workers_active: usize,
    pub cpu_workers_active: usize,
}

#[derive(Clone)]
pub struct JobHandle {
    cancelled: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

type Completion = Box<dyn FnOnce() + Send + 'static>;
type JobRunner = Box<dyn FnOnce(&mpsc::Sender<Completion>) + Send + 'static>;

struct Job {
    run: JobRunner,
    cancelled: Arc<AtomicBool>,
}

pub struct Jobs {
    inner: Arc<JobsInner>,
}

struct JobsInner {
    mode: JobsMode,
    io_queue: JobQueueState,
    cpu_queue: JobQueueState,
    completion_sender: mpsc::Sender<Completion>,
    completion_receiver: Mutex<mpsc::Receiver<Completion>>,
    io_depth: AtomicUsize,
    cpu_depth: AtomicUsize,
    io_active: AtomicUsize,
    cpu_active: AtomicUsize,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

struct JobQueueState {
    queue: Mutex<QueueState>,
    wake: Condvar,
    capacity: usize,
    queue_kind: JobQueue,
}

struct QueueState {
    entries: VecDeque<Job>,
    shutdown: bool,
}

impl Jobs {
    pub fn new(config: JobsConfig) -> Self {
        let (completion_sender, completion_receiver) = mpsc::channel();
        let io_queue = JobQueueState::new(JobQueue::Io, config.io_queue_capacity.max(1));
        let cpu_queue = JobQueueState::new(JobQueue::Cpu, config.cpu_queue_capacity.max(1));
        let inner = Arc::new(JobsInner {
            mode: config.mode,
            io_queue,
            cpu_queue,
            completion_sender,
            completion_receiver: Mutex::new(completion_receiver),
            io_depth: AtomicUsize::new(0),
            cpu_depth: AtomicUsize::new(0),
            io_active: AtomicUsize::new(0),
            cpu_active: AtomicUsize::new(0),
            workers: Mutex::new(Vec::new()),
        });
        if config.mode == JobsMode::Threaded {
            spawn_workers(&inner, JobQueue::Io, config.io_workers.max(1));
            spawn_workers(&inner, JobQueue::Cpu, config.cpu_workers.max(1));
        }
        Self { inner }
    }

    pub fn submit<R, F, C>(
        &self,
        queue: JobQueue,
        job: F,
        on_complete: C,
    ) -> Result<JobHandle, JobError>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
        C: FnOnce(R) + Send + 'static,
    {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_for_job = Arc::clone(&cancel_flag);
        let completion_sender = self.inner.completion_sender.clone();
        let run = Box::new(move |sender: &mpsc::Sender<Completion>| {
            if cancel_for_job.load(Ordering::Relaxed) {
                return;
            }
            let result = panic::catch_unwind(panic::AssertUnwindSafe(job));
            match result {
                Ok(value) => {
                    if cancel_for_job.load(Ordering::Relaxed) {
                        return;
                    }
                    let completion = Box::new(move || on_complete(value));
                    let _ = sender.send(completion);
                }
                Err(payload) => {
                    let message = format!("job panic ({})", panic_payload_to_string(&payload));
                    observability::set_sticky_error(message);
                }
            }
        });
        let job = Job {
            run,
            cancelled: Arc::clone(&cancel_flag),
        };

        if self.inner.mode == JobsMode::Inline {
            (job.run)(&completion_sender);
            return Ok(JobHandle {
                cancelled: cancel_flag,
            });
        }

        self.enqueue(queue, job)?;
        Ok(JobHandle {
            cancelled: cancel_flag,
        })
    }

    pub fn pump_completions(&self) -> usize {
        let receiver_guard = self
            .inner
            .completion_receiver
            .lock()
            .expect("completion receiver poisoned");
        let mut count = 0usize;
        loop {
            match receiver_guard.try_recv() {
                Ok(task) => {
                    task();
                    count = count.saturating_add(1);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        count
    }

    pub fn telemetry(&self) -> JobsTelemetry {
        JobsTelemetry {
            io_queue_depth: self.inner.io_depth.load(Ordering::Relaxed),
            cpu_queue_depth: self.inner.cpu_depth.load(Ordering::Relaxed),
            io_workers_active: self.inner.io_active.load(Ordering::Relaxed),
            cpu_workers_active: self.inner.cpu_active.load(Ordering::Relaxed),
        }
    }

    fn enqueue(&self, queue: JobQueue, job: Job) -> Result<(), JobError> {
        match queue {
            JobQueue::Io => {
                self.inner.io_queue.push(job)?;
                self.inner.io_depth.fetch_add(1, Ordering::Relaxed);
            }
            JobQueue::Cpu => {
                self.inner.cpu_queue.push(job)?;
                self.inner.cpu_depth.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

impl JobsInner {
    fn pop(&self, queue: JobQueue) -> Option<Job> {
        let job = match queue {
            JobQueue::Io => self.io_queue.pop(),
            JobQueue::Cpu => self.cpu_queue.pop(),
        };
        if job.is_some() {
            match queue {
                JobQueue::Io => {
                    self.io_depth.fetch_sub(1, Ordering::Relaxed);
                }
                JobQueue::Cpu => {
                    self.cpu_depth.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
        job
    }

    fn set_active(&self, queue: JobQueue, active: bool) {
        match queue {
            JobQueue::Io => {
                if active {
                    self.io_active.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.io_active.fetch_sub(1, Ordering::Relaxed);
                }
            }
            JobQueue::Cpu => {
                if active {
                    self.cpu_active.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.cpu_active.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn shutdown(&self) {
        self.io_queue.shutdown();
        self.cpu_queue.shutdown();
    }
}

impl JobQueueState {
    fn new(queue_kind: JobQueue, capacity: usize) -> Self {
        Self {
            queue: Mutex::new(QueueState {
                entries: VecDeque::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
            capacity,
            queue_kind,
        }
    }

    fn push(&self, job: Job) -> Result<(), JobError> {
        let mut guard = self.queue.lock().expect("job queue lock poisoned");
        if guard.shutdown {
            return Err(JobError::QueueClosed(self.queue_kind));
        }
        if guard.entries.len() >= self.capacity {
            return Err(JobError::QueueFull(self.queue_kind));
        }
        guard.entries.push_back(job);
        self.wake.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<Job> {
        let mut guard = self.queue.lock().expect("job queue lock poisoned");
        loop {
            if guard.shutdown {
                return None;
            }
            if let Some(job) = guard.entries.pop_front() {
                return Some(job);
            }
            guard = self.wake.wait(guard).expect("job queue lock poisoned");
        }
    }

    fn shutdown(&self) {
        let mut guard = self.queue.lock().expect("job queue lock poisoned");
        guard.shutdown = true;
        self.wake.notify_all();
    }
}

impl Drop for JobsInner {
    fn drop(&mut self) {
        self.shutdown();
        let mut workers = self.workers.lock().expect("workers lock poisoned");
        for handle in workers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn spawn_workers(inner: &Arc<JobsInner>, queue: JobQueue, count: usize) {
    let mut handles = inner.workers.lock().expect("workers lock poisoned");
    for index in 0..count {
        let inner = Arc::clone(inner);
        let name = format!("jobs-{}-{}", queue, index);
        let handle = thread::Builder::new()
            .name(name)
            .spawn(move || worker_loop(&inner, queue))
            .expect("spawn worker failed");
        handles.push(handle);
    }
}

fn worker_loop(inner: &JobsInner, queue: JobQueue) {
    loop {
        let Some(job) = inner.pop(queue) else {
            break;
        };
        if job.cancelled.load(Ordering::Relaxed) {
            continue;
        }
        inner.set_active(queue, true);
        (job.run)(&inner.completion_sender);
        inner.set_active(queue, false);
    }
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn jobs_inline_mode_completes() {
        let jobs = Jobs::new(JobsConfig::inline());
        let result = Arc::new(AtomicUsize::new(0));
        let result_clone = Arc::clone(&result);
        jobs.submit(
            JobQueue::Cpu,
            || 42usize,
            move |value| {
                result_clone.store(value, Ordering::Relaxed);
            },
        )
        .unwrap();
        jobs.pump_completions();
        assert_eq!(result.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn jobs_threaded_completion_pump() {
        let jobs = Jobs::new(JobsConfig::threaded(1, 1, 8));
        let (tx, rx) = mpsc::channel();
        jobs.submit(
            JobQueue::Io,
            || "ok",
            move |value| {
                let _ = tx.send(value);
            },
        )
        .unwrap();
        for _ in 0..100 {
            jobs.pump_completions();
            if rx.try_recv().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "ok");
    }

    #[test]
    fn jobs_panic_sets_sticky_error() {
        observability::clear_sticky_error();
        let jobs = Jobs::new(JobsConfig::inline());
        jobs.submit(JobQueue::Cpu, || -> () { panic!("boom") }, |_| {})
            .unwrap();
        let sticky = observability::sticky_error();
        assert!(sticky.is_some());
    }
}
