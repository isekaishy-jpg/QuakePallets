use platform_winit::{create_window, ControlFlow, Event, WindowEvent};
use render_wgpu::RenderError;

fn main() {
    let (event_loop, window) = match create_window("Pallet", 1280, 720) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("window init failed: {}", err);
            std::process::exit(1);
        }
    };
    // wgpu surfaces require the window to outlive the renderer for the app lifetime.
    let window = Box::leak(Box::new(window));

    let mut renderer = match render_wgpu::Renderer::new(window) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("renderer init failed: {}", err);
            std::process::exit(1);
        }
    };
    let main_window_id = renderer.window_id();

    if let Err(err) = event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == main_window_id => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(size) => renderer.resize(size),
                WindowEvent::ScaleFactorChanged { .. } => {
                    renderer.resize(renderer.window_inner_size());
                }
                WindowEvent::RedrawRequested => match renderer.render() {
                    Ok(()) => {}
                    Err(RenderError::Lost | RenderError::Outdated) => {
                        renderer.resize(renderer.size());
                    }
                    Err(RenderError::OutOfMemory) => {
                        eprintln!("render error: out of memory");
                        elwt.exit();
                    }
                    Err(err) => {
                        eprintln!("render error: {}", err);
                    }
                },
                _ => {}
            },
            Event::AboutToWait => {
                renderer.request_redraw();
            }
            _ => {}
        }
    }) {
        eprintln!("event loop exited with error: {}", err);
    }
}
