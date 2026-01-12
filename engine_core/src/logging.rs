use std::fmt;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        };
        write!(f, "{}", label)
    }
}

type Logger = Box<dyn Fn(LogLevel, &str) + Send + Sync + 'static>;

fn default_logger(level: LogLevel, message: &str) {
    eprintln!("[{}] {}", level, message);
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn logger_cell() -> &'static Mutex<Logger> {
    static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();
    LOGGER.get_or_init(|| Mutex::new(Box::new(default_logger)))
}

pub fn set_logger(logger: impl Fn(LogLevel, &str) + Send + Sync + 'static) {
    let mut guard = lock_unpoisoned(logger_cell());
    *guard = Box::new(logger);
}

pub fn log(level: LogLevel, message: impl AsRef<str>) {
    let guard = lock_unpoisoned(logger_cell());
    (guard)(level, message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    log(LogLevel::Error, message);
}

pub fn warn(message: impl AsRef<str>) {
    log(LogLevel::Warn, message);
}

pub fn info(message: impl AsRef<str>) {
    log(LogLevel::Info, message);
}

pub fn debug(message: impl AsRef<str>) {
    log(LogLevel::Debug, message);
}
