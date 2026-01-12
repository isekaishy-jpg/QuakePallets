use std::panic::{self, PanicHookInfo};
use std::sync::{Mutex, OnceLock};

use crate::logging;

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn sticky_cell() -> &'static Mutex<Option<String>> {
    static STICKY: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    STICKY.get_or_init(|| Mutex::new(None))
}

pub fn set_sticky_error(message: impl Into<String>) {
    let message = message.into();
    let mut guard = lock_unpoisoned(sticky_cell());
    *guard = Some(message.clone());
    logging::error(message);
}

pub fn clear_sticky_error() {
    let mut guard = lock_unpoisoned(sticky_cell());
    *guard = None;
}

pub fn sticky_error() -> Option<String> {
    let guard = lock_unpoisoned(sticky_cell());
    guard.clone()
}

pub fn install_panic_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        set_sticky_error(format_panic(info));
        default_hook(info);
    }));
}

fn format_panic(info: &PanicHookInfo<'_>) -> String {
    let payload = if let Some(text) = info.payload().downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = info.payload().downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic payload".to_string()
    };
    let location = info
        .location()
        .map(|loc| format!("{}:{}", loc.file(), loc.line()))
        .unwrap_or_else(|| "<unknown>".to_string());
    format!("panic at {}: {}", location, payload)
}
