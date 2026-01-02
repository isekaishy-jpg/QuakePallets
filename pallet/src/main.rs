use std::path::{Path, PathBuf};
use std::time::Instant;

use compat_quake::bsp;
use compat_quake::bsp::Bsp;
use compat_quake::lmp;
use compat_quake::pak::{self, PakFile};
use engine_core::vfs::Vfs;
use platform_winit::{
    create_window, ControlFlow, CursorGrabMode, DeviceEvent, ElementState, Event, KeyCode,
    MouseButton, PhysicalKey, PhysicalPosition, PhysicalSize, Window, WindowEvent,
};
use render_wgpu::{ImageData, MeshData, MeshVertex, RenderError};

const EXIT_USAGE: i32 = 2;
const EXIT_QUAKE_DIR: i32 = 10;
const EXIT_PAK: i32 = 11;
const EXIT_IMAGE: i32 = 12;
const EXIT_BSP: i32 = 13;
const EXIT_SCENE: i32 = 14;

const CAMERA_UP: Vec3 = Vec3::new(0.0, 1.0, 0.0);
const CAMERA_FOV_Y: f32 = 70.0f32.to_radians();
const CAMERA_NEAR: f32 = 1.0;
const CAMERA_FAR: f32 = 8192.0;
const OPENGL_TO_WGPU: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 0.5, 0.0],
    [0.0, 0.0, 0.5, 1.0],
];

struct CliArgs {
    quake_dir: Option<PathBuf>,
    show_image: Option<String>,
    map: Option<String>,
}

enum ArgParseError {
    Help,
    Message(String),
}

struct ExitError {
    code: i32,
    message: String,
}

impl ExitError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Default)]
struct InputState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

struct CameraState {
    position: Vec3,
    yaw: f32,
    pitch: f32,
    speed: f32,
    sensitivity: f32,
}

impl CameraState {
    fn from_bounds(bounds: &Bounds) -> Self {
        let center = bounds.center();
        let extent = bounds.extent().length().max(1.0);
        let position = Vec3::new(center.x, center.y + extent * 0.25, center.z + extent);
        let dir = center.sub(position).normalize_or_zero();
        let pitch = dir.y.asin();
        let yaw = dir.x.atan2(-dir.z);
        Self {
            position,
            yaw,
            pitch,
            speed: extent * 0.6,
            sensitivity: 0.0025,
        }
    }

    fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            -self.yaw.cos() * self.pitch.cos(),
        )
    }

    fn right(&self) -> Vec3 {
        self.forward().cross(CAMERA_UP).normalize_or_zero()
    }

    fn update(&mut self, input: &InputState, dt: f32) {
        let mut direction = Vec3::zero();
        let forward = self.forward();
        let right = self.right();
        if input.forward {
            direction = direction.add(forward);
        }
        if input.back {
            direction = direction.sub(forward);
        }
        if input.right {
            direction = direction.add(right);
        }
        if input.left {
            direction = direction.sub(right);
        }
        if input.up {
            direction = direction.add(CAMERA_UP);
        }
        if input.down {
            direction = direction.sub(CAMERA_UP);
        }

        let direction = direction.normalize_or_zero();
        self.position = self.position.add(direction.scale(self.speed * dt));
    }

    fn apply_mouse(&mut self, delta_x: f64, delta_y: f64) {
        let dx = delta_x as f32;
        let dy = delta_y as f32;
        self.yaw += dx * self.sensitivity;
        self.pitch = (self.pitch - dy * self.sensitivity).clamp(-1.54, 1.54);
    }

    fn view_proj(&self, aspect: f32) -> [[f32; 4]; 4] {
        let view = self.view_matrix();
        let proj = perspective(CAMERA_FOV_Y, aspect, CAMERA_NEAR, CAMERA_FAR);
        mat4_mul(proj, view)
    }

    fn view_matrix(&self) -> [[f32; 4]; 4] {
        let forward = self.forward();
        let right = forward.cross(CAMERA_UP).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        [
            [right.x, up.x, -forward.x, 0.0],
            [right.y, up.y, -forward.y, 0.0],
            [right.z, up.z, -forward.z, 0.0],
            [
                -right.dot(self.position),
                -up.dot(self.position),
                forward.dot(self.position),
                1.0,
            ],
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    const fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }

    fn scale(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor, self.z * factor)
    }

    fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    fn normalize_or_zero(self) -> Self {
        let len = self.length();
        if len > 0.0 {
            self.scale(1.0 / len)
        } else {
            Self::zero()
        }
    }

    fn abs(self) -> Self {
        Self::new(self.x.abs(), self.y.abs(), self.z.abs())
    }

    fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    min: Vec3,
    max: Vec3,
    valid: bool,
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: Vec3::new(f32::MAX, f32::MAX, f32::MAX),
            max: Vec3::new(f32::MIN, f32::MIN, f32::MIN),
            valid: false,
        }
    }

    fn include(&mut self, value: Vec3) {
        self.min = Vec3::new(
            self.min.x.min(value.x),
            self.min.y.min(value.y),
            self.min.z.min(value.z),
        );
        self.max = Vec3::new(
            self.max.x.max(value.x),
            self.max.y.max(value.y),
            self.max.z.max(value.z),
        );
        self.valid = true;
    }

    fn center(&self) -> Vec3 {
        self.min.add(self.max).scale(0.5)
    }

    fn extent(&self) -> Vec3 {
        self.max.sub(self.min)
    }
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(ArgParseError::Help) => {
            print_usage();
            return;
        }
        Err(ArgParseError::Message(message)) => {
            eprintln!("{}", message);
            print_usage();
            std::process::exit(EXIT_USAGE);
        }
    };

    let (event_loop, window) = match create_window("Pallet", 1280, 720) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("window init failed: {}", err);
            std::process::exit(1);
        }
    };
    let window: &'static Window = Box::leak(Box::new(window));

    let mut renderer = match render_wgpu::Renderer::new(window) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("renderer init failed: {}", err);
            std::process::exit(1);
        }
    };
    let main_window_id = renderer.window_id();

    let mut input = InputState::default();
    let mut camera = CameraState {
        position: Vec3::zero(),
        yaw: 0.0,
        pitch: 0.0,
        speed: 320.0,
        sensitivity: 0.0025,
    };
    let mut scene_active = false;
    let mut mouse_look = false;
    let mut mouse_grabbed = false;
    let mut ignore_cursor_move = false;

    if let Some(asset) = args.show_image.as_deref() {
        let quake_dir = match args.quake_dir.as_ref() {
            Some(path) => path,
            None => {
                eprintln!("--quake-dir is required when using --show-image");
                print_usage();
                std::process::exit(EXIT_USAGE);
            }
        };

        let image = match load_lmp_image(quake_dir, asset) {
            Ok(image) => image,
            Err(err) => {
                eprintln!("{}", err.message);
                std::process::exit(err.code);
            }
        };

        if let Err(err) = renderer.set_image(image) {
            eprintln!("image upload failed: {}", err);
            std::process::exit(EXIT_IMAGE);
        }
    }

    if let Some(map) = args.map.as_deref() {
        let quake_dir = match args.quake_dir.as_ref() {
            Some(path) => path,
            None => {
                eprintln!("--quake-dir is required when using --map");
                print_usage();
                std::process::exit(EXIT_USAGE);
            }
        };

        let (mesh, bounds) = match load_bsp_scene(quake_dir, map) {
            Ok(result) => result,
            Err(err) => {
                eprintln!("{}", err.message);
                std::process::exit(err.code);
            }
        };

        if let Err(err) = renderer.set_scene(mesh) {
            eprintln!("scene upload failed: {}", err);
            std::process::exit(EXIT_SCENE);
        }

        camera = CameraState::from_bounds(&bounds);
        scene_active = true;
        mouse_look = false;
        mouse_grabbed = set_cursor_mode(window, mouse_look);
        let aspect = aspect_ratio(renderer.size());
        renderer.update_camera(camera.view_proj(aspect));
    }

    let mut last_frame = Instant::now();

    if let Err(err) = event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Poll);
        match event {
            Event::WindowEvent { event, window_id } if window_id == main_window_id => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(size) => renderer.resize(size),
                WindowEvent::ScaleFactorChanged { .. } => {
                    renderer.resize(renderer.window_inner_size());
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let pressed = event.state == ElementState::Pressed;
                        match code {
                            KeyCode::KeyW => input.forward = pressed,
                            KeyCode::KeyS => input.back = pressed,
                            KeyCode::KeyA => input.left = pressed,
                            KeyCode::KeyD => input.right = pressed,
                            KeyCode::Space => input.up = pressed,
                            KeyCode::ShiftLeft => input.down = pressed,
                            KeyCode::Escape if pressed => {
                                mouse_look = false;
                                mouse_grabbed = set_cursor_mode(window, mouse_look);
                            }
                            _ => {}
                        }
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if button == MouseButton::Right && state == ElementState::Pressed {
                        mouse_look = !mouse_look;
                        mouse_grabbed = set_cursor_mode(window, mouse_look);
                        if mouse_look && !mouse_grabbed {
                            ignore_cursor_move = center_cursor(window);
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    if scene_active && mouse_look && !mouse_grabbed {
                        if ignore_cursor_move {
                            ignore_cursor_move = false;
                        } else {
                            let center = cursor_center(window);
                            let dx = position.x - center.x;
                            let dy = position.y - center.y;
                            if dx != 0.0 || dy != 0.0 {
                                camera.apply_mouse(dx, dy);
                            }
                            ignore_cursor_move = center_cursor(window);
                        }
                    }
                }
                WindowEvent::Focused(false) => {
                    mouse_look = false;
                    mouse_grabbed = set_cursor_mode(window, mouse_look);
                }
                WindowEvent::RedrawRequested => {
                    let now = Instant::now();
                    let dt = (now - last_frame).as_secs_f32().min(0.1);
                    last_frame = now;

                    if scene_active {
                        camera.update(&input, dt);
                        let aspect = aspect_ratio(renderer.size());
                        renderer.update_camera(camera.view_proj(aspect));
                    }

                    match renderer.render() {
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
                    }
                }
                _ => {}
            },
            Event::DeviceEvent { event, .. } => {
                if scene_active && mouse_look && mouse_grabbed {
                    if let DeviceEvent::MouseMotion { delta } = event {
                        camera.apply_mouse(delta.0, delta.1);
                    }
                }
            }
            Event::AboutToWait => {
                renderer.request_redraw();
            }
            _ => {}
        }
    }) {
        eprintln!("event loop exited with error: {}", err);
    }
}

fn parse_args() -> Result<CliArgs, ArgParseError> {
    let mut quake_dir = None;
    let mut show_image = None;
    let mut map = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quake-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--quake-dir expects a path".into()))?;
                quake_dir = Some(PathBuf::from(value));
            }
            "--show-image" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--show-image expects a path".into()))?;
                show_image = Some(value);
            }
            "--map" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--map expects a name".into()))?;
                map = Some(value);
            }
            "-h" | "--help" => return Err(ArgParseError::Help),
            _ => {
                return Err(ArgParseError::Message(format!(
                    "unexpected argument: {}",
                    arg
                )))
            }
        }
    }

    if show_image.is_some() && map.is_some() {
        return Err(ArgParseError::Message(
            "--show-image and --map cannot be used together".into(),
        ));
    }

    Ok(CliArgs {
        quake_dir,
        show_image,
        map,
    })
}

fn print_usage() {
    eprintln!("usage: pallet [--quake-dir <path>] [--show-image <asset>] [--map <name>]");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --show-image gfx/conback.lmp");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1");
}

fn load_lmp_image(quake_dir: &Path, asset: &str) -> Result<ImageData, ExitError> {
    let (pak, pak_path) = load_pak_from_quake_dir(quake_dir)?;
    let palette_bytes = pak
        .entry_data("gfx/palette.lmp")
        .map_err(|err| ExitError::new(EXIT_PAK, format!("palette lookup failed: {}", err)))?
        .ok_or_else(|| ExitError::new(EXIT_PAK, "palette not found in pak0.pak"))?;
    let palette = lmp::parse_palette(palette_bytes)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("palette parse failed: {}", err)))?;

    let asset_name = normalize_asset_name(asset);
    let image_bytes = pak
        .entry_data(&asset_name)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("asset lookup failed: {}", err)))?
        .ok_or_else(|| {
            ExitError::new(
                EXIT_PAK,
                format!("asset not found in pak0.pak: {}", asset_name),
            )
        })?;
    let image = lmp::parse_lmp_image(image_bytes)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("image parse failed: {}", err)))?;
    let rgba = image.to_rgba8(&palette);
    let image = ImageData::new(image.width, image.height, rgba)
        .map_err(|err| ExitError::new(EXIT_IMAGE, format!("image data failed: {}", err)))?;

    println!(
        "loaded {} from {} ({}x{})",
        asset_name,
        pak_path.display(),
        image.width,
        image.height
    );

    Ok(image)
}

fn load_bsp_scene(quake_dir: &Path, map: &str) -> Result<(MeshData, Bounds), ExitError> {
    let (pak, pak_path) = load_pak_from_quake_dir(quake_dir)?;
    let map_name = normalize_map_asset(map);
    let bsp_bytes = pak
        .entry_data(&map_name)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("map lookup failed: {}", err)))?
        .ok_or_else(|| {
            ExitError::new(EXIT_PAK, format!("map not found in pak0.pak: {}", map_name))
        })?;
    let bsp = bsp::parse_bsp(bsp_bytes)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("bsp parse failed: {}", err)))?;

    println!(
        "loaded {} from {} ({} vertices, {} faces)",
        map_name,
        pak_path.display(),
        bsp.vertices.len(),
        bsp.faces.len()
    );

    build_scene_mesh(&bsp)
}

fn build_scene_mesh(bsp: &Bsp) -> Result<(MeshData, Bounds), ExitError> {
    let face_range = bsp.world_face_range().unwrap_or(0..bsp.faces.len());

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut bounds = Bounds::empty();

    for face_index in face_range {
        let face = match bsp.faces.get(face_index) {
            Some(face) => face,
            None => {
                return Err(ExitError::new(
                    EXIT_BSP,
                    format!("face index out of bounds: {}", face_index),
                ));
            }
        };

        let num_edges = face.num_edges as usize;
        if num_edges < 3 {
            continue;
        }
        let first_edge = usize::try_from(face.first_edge).map_err(|_| {
            ExitError::new(
                EXIT_BSP,
                format!("face has negative first_edge: {}", face.first_edge),
            )
        })?;
        let end = first_edge
            .checked_add(num_edges)
            .ok_or_else(|| ExitError::new(EXIT_BSP, "face edge range overflow"))?;
        if end > bsp.surfedges.len() {
            return Err(ExitError::new(
                EXIT_BSP,
                format!(
                    "face edge range out of bounds: {}..{} (surfedges {})",
                    first_edge,
                    end,
                    bsp.surfedges.len()
                ),
            ));
        }

        let mut polygon = Vec::with_capacity(num_edges);
        for &surfedge in &bsp.surfedges[first_edge..end] {
            let edge_index = if surfedge < 0 { -surfedge } else { surfedge } as usize;
            let reversed = surfedge < 0;
            let edge = match bsp.edges.get(edge_index) {
                Some(edge) => *edge,
                None => {
                    return Err(ExitError::new(
                        EXIT_BSP,
                        format!("edge index out of bounds: {}", edge_index),
                    ));
                }
            };
            let vertex_index = if reversed { edge[1] } else { edge[0] } as usize;
            let vertex = match bsp.vertices.get(vertex_index) {
                Some(vertex) => *vertex,
                None => {
                    return Err(ExitError::new(
                        EXIT_BSP,
                        format!("vertex index out of bounds: {}", vertex_index),
                    ));
                }
            };
            polygon.push(vertex);
        }

        if polygon.len() < 3 {
            continue;
        }

        let v0 = quake_to_render(polygon[0]);
        for i in 1..polygon.len() - 1 {
            let v1 = quake_to_render(polygon[i]);
            let v2 = quake_to_render(polygon[i + 1]);
            let normal = v1.sub(v0).cross(v2.sub(v0)).normalize_or_zero();
            let color = normal.abs().scale(0.8).add(Vec3::new(0.2, 0.2, 0.2));

            let base = u32::try_from(vertices.len())
                .map_err(|_| ExitError::new(EXIT_BSP, "vertex count overflow building mesh"))?;
            vertices.push(MeshVertex {
                position: v0.to_array(),
                color: color.to_array(),
            });
            vertices.push(MeshVertex {
                position: v1.to_array(),
                color: color.to_array(),
            });
            vertices.push(MeshVertex {
                position: v2.to_array(),
                color: color.to_array(),
            });
            indices.extend_from_slice(&[base, base + 1, base + 2]);

            bounds.include(v0);
            bounds.include(v1);
            bounds.include(v2);
        }
    }

    if !bounds.valid {
        return Err(ExitError::new(
            EXIT_BSP,
            "mesh contained no drawable triangles",
        ));
    }

    let mesh = MeshData::new(vertices, indices)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("mesh build failed: {}", err)))?;
    Ok((mesh, bounds))
}

fn load_pak_from_quake_dir(quake_dir: &Path) -> Result<(PakFile, PathBuf), ExitError> {
    if !quake_dir.is_dir() {
        return Err(ExitError::new(
            EXIT_QUAKE_DIR,
            format!("quake dir not found: {}", quake_dir.display()),
        ));
    }

    let mut vfs = Vfs::new();
    vfs.add_root(quake_dir);

    let (virtual_path, pak_path) = if vfs.exists("id1/pak0.pak") {
        ("id1/pak0.pak", quake_dir.join("id1").join("pak0.pak"))
    } else if vfs.exists("pak0.pak") {
        ("pak0.pak", quake_dir.join("pak0.pak"))
    } else {
        return Err(ExitError::new(
            EXIT_PAK,
            format!("pak0.pak not found under {}", quake_dir.display()),
        ));
    };

    let data = vfs
        .read(virtual_path)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("pak read failed: {}", err)))?;
    let pak = pak::parse_pak(data)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("pak parse failed: {}", err)))?;

    Ok((pak, pak_path))
}

fn normalize_asset_name(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    normalized
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn normalize_map_asset(name: &str) -> String {
    let mut normalized = normalize_asset_name(name);
    if let Some(stripped) = normalized.strip_prefix("maps/") {
        normalized = stripped.to_string();
    }
    if !normalized.ends_with(".bsp") {
        normalized.push_str(".bsp");
    }
    format!("maps/{}", normalized)
}

fn set_cursor_mode(window: &Window, enabled: bool) -> bool {
    if enabled {
        let grabbed = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            .is_ok();
        window.set_cursor_visible(!grabbed);
        grabbed
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        false
    }
}

fn cursor_center(window: &Window) -> PhysicalPosition<f64> {
    let size = window.inner_size();
    PhysicalPosition::new(size.width as f64 * 0.5, size.height as f64 * 0.5)
}

fn center_cursor(window: &Window) -> bool {
    let center = cursor_center(window);
    window.set_cursor_position(center).is_ok()
}

fn aspect_ratio(size: PhysicalSize<u32>) -> f32 {
    let width = size.width.max(1) as f32;
    let height = size.height.max(1) as f32;
    width / height
}

fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fovy * 0.5).tan();
    let nf = 1.0 / (near - far);
    let gl = [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, (far + near) * nf, -1.0],
        [0.0, 0.0, (2.0 * far * near) * nf, 0.0],
    ];
    mat4_mul(OPENGL_TO_WGPU, gl)
}

fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        let v = b[col];
        out[col] = mat4_mul_vec4(a, v);
    }
    out
}

fn mat4_mul_vec4(m: [[f32; 4]; 4], v: [f32; 4]) -> [f32; 4] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2] + m[3][0] * v[3],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2] + m[3][1] * v[3],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2] + m[3][2] * v[3],
        m[0][3] * v[0] + m[1][3] * v[1] + m[2][3] * v[2] + m[3][3] * v[3],
    ]
}

fn quake_to_render(value: [f32; 3]) -> Vec3 {
    Vec3::new(value[0], value[2], -value[1])
}

impl From<[f32; 3]> for Vec3 {
    fn from(value: [f32; 3]) -> Self {
        Vec3::new(value[0], value[1], value[2])
    }
}
