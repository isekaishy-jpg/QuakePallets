#![forbid(unsafe_code)]

use std::fmt;

pub use winit::dpi::{PhysicalPosition, PhysicalSize};
pub use winit::event::{DeviceEvent, ElementState, Event, Ime, MouseButton, WindowEvent};
pub use winit::event_loop::{ControlFlow, EventLoop};
pub use winit::keyboard::{KeyCode, PhysicalKey};
pub use winit::window::{CursorGrabMode, Fullscreen, Window};

#[derive(Debug)]
pub enum WindowInitError {
    EventLoop(String),
    Window(winit::error::OsError),
}

impl fmt::Display for WindowInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WindowInitError::EventLoop(message) => {
                write!(f, "event loop initialization failed: {}", message)
            }
            WindowInitError::Window(err) => write!(f, "window creation failed: {}", err),
        }
    }
}

impl std::error::Error for WindowInitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WindowInitError::EventLoop(_) => None,
            WindowInitError::Window(err) => Some(err),
        }
    }
}

pub fn create_window(
    title: &str,
    width: u32,
    height: u32,
) -> Result<(EventLoop<()>, Window), WindowInitError> {
    let event_loop = EventLoop::new().map_err(|err| WindowInitError::EventLoop(err.to_string()))?;
    let window = winit::window::WindowBuilder::new()
        .with_title(title)
        .with_inner_size(PhysicalSize::new(width, height))
        .build(&event_loop)
        .map_err(WindowInitError::Window)?;
    Ok((event_loop, window))
}
