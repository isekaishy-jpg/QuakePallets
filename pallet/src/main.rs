use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::AudioEngine;
use client::{Client, ClientInput};
use compat_quake::bsp::{self, Bsp, SpawnPoint};
use compat_quake::lmp;
use compat_quake::pak::{self, PakFile};
use engine_core::vfs::{Vfs, VfsError};
use net_transport::{LoopbackTransport, Transport, TransportConfig};
use platform_winit::{
    create_window, ControlFlow, CursorGrabMode, DeviceEvent, ElementState, Event, Ime, KeyCode,
    MouseButton, PhysicalKey, PhysicalPosition, PhysicalSize, Window, WindowEvent,
};
use render_wgpu::{ImageData, MeshData, MeshVertex, RenderError, YuvImageView};
use script_lua::{HostCallbacks, ScriptConfig, ScriptEngine, SpawnRequest};
use server::Server;
use video::{
    advance_playlist, start_video_playback, VideoDebugSnapshot, VideoDebugStats, VideoPlayback,
    VIDEO_AUDIO_PREBUFFER_MS, VIDEO_INTERMISSION_MS, VIDEO_MAX_QUEUED_MS_PLAYBACK,
    VIDEO_MAX_QUEUED_MS_PREDECODE, VIDEO_PLAYBACK_WARM_MS, VIDEO_PLAYBACK_WARM_UP_MS,
    VIDEO_PREDECODE_MIN_AUDIO_MS, VIDEO_PREDECODE_MIN_ELAPSED_MS, VIDEO_PREDECODE_MIN_FRAMES,
    VIDEO_PREDECODE_RAMP_MS, VIDEO_PREDECODE_START_DELAY_MS, VIDEO_PREDECODE_WARM_MS,
    VIDEO_START_MIN_FRAMES,
};

mod video;

const EXIT_USAGE: i32 = 2;
const EXIT_QUAKE_DIR: i32 = 10;
const EXIT_PAK: i32 = 11;
const EXIT_IMAGE: i32 = 12;
const EXIT_BSP: i32 = 13;
const EXIT_SCENE: i32 = 14;
const DEFAULT_SFX: &str = "sound/misc/menu1.wav";

const CAMERA_UP: Vec3 = Vec3::new(0.0, 1.0, 0.0);
const CAMERA_FOV_Y: f32 = 70.0f32.to_radians();
const CAMERA_NEAR: f32 = 0.25;
const CAMERA_FAR: f32 = 8192.0;
const PLAYER_SPEED: f32 = 320.0;
const PLAYER_ACCEL: f32 = 12.0;
const PLAYER_FRICTION: f32 = 14.0;
const PLAYER_STOP_SPEED: f32 = 100.0;
const PLAYER_GRAVITY: f32 = 800.0;
const PLAYER_JUMP_SPEED: f32 = 270.0;
const PLAYER_EYE_HEIGHT: f32 = 22.0;
const PLAYER_STEP_HEIGHT: f32 = 18.0;
const PLAYER_MAX_DROP: f32 = 256.0;
const FLOOR_NORMAL_MIN: f32 = 0.7;
const DIST_EPSILON: f32 = 0.03125;
const CONTENTS_SOLID: i32 = -2;
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
    play_movie: Option<PathBuf>,
    playlist: Option<PathBuf>,
    script: Option<PathBuf>,
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

struct MusicTrack {
    name: String,
    data: Vec<u8>,
}

#[derive(Default)]
struct InputState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    jump: bool,
    down: bool,
}

#[derive(Default)]
struct ConsoleState {
    active: bool,
    buffer: String,
}

struct ScriptEntity {
    id: u32,
    position: Vec3,
    yaw: f32,
}

struct ScriptHostState {
    next_id: u32,
    entities: Vec<ScriptEntity>,
    quake_dir: Option<PathBuf>,
    audio: Option<Rc<AudioEngine>>,
}

impl ScriptHostState {
    fn new(quake_dir: Option<PathBuf>, audio: Option<Rc<AudioEngine>>) -> Self {
        Self {
            next_id: 1,
            entities: Vec::new(),
            quake_dir,
            audio,
        }
    }

    fn spawn_entity(&mut self, request: SpawnRequest) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let entity = ScriptEntity {
            id,
            position: Vec3::new(
                request.position[0],
                request.position[1],
                request.position[2],
            ),
            yaw: request.yaw,
        };
        println!(
            "lua spawn entity {} at ({:.2}, {:.2}, {:.2}) yaw {:.2}",
            entity.id, entity.position.x, entity.position.y, entity.position.z, entity.yaw
        );
        self.entities.push(entity);
        let count = self.entities.len();
        println!("lua entities: {}", count);
        id
    }

    fn play_sound(&mut self, asset: String) -> Result<(), String> {
        let audio = self
            .audio
            .as_ref()
            .ok_or_else(|| "audio disabled".to_string())?;
        let quake_dir = self
            .quake_dir
            .as_ref()
            .ok_or_else(|| "quake dir is required for play_sound".to_string())?;
        let data = load_wav_sfx(quake_dir, &asset).map_err(|err| err.message)?;
        audio
            .play_wav(data)
            .map_err(|err| format!("play_sound failed: {}", err))?;
        Ok(())
    }
}

struct ScriptRuntime {
    engine: ScriptEngine,
    _host: Rc<RefCell<ScriptHostState>>,
}

struct LoopbackNet {
    client: Client,
    server: Server,
    saw_snapshot: bool,
}

impl LoopbackNet {
    fn start() -> Result<Self, String> {
        let transport = TransportConfig::default();
        let mut server_transport =
            LoopbackTransport::bind(transport.clone()).map_err(|err| err.to_string())?;
        let mut client_transport =
            LoopbackTransport::bind(transport).map_err(|err| err.to_string())?;
        let server_addr = server_transport
            .local_addr()
            .map_err(|err: net_transport::TransportError| err.to_string())?;
        let client_addr = client_transport
            .local_addr()
            .map_err(|err: net_transport::TransportError| err.to_string())?;
        server_transport.connect_peer(client_addr);
        client_transport.connect_peer(server_addr);

        let server = Server::bind(Box::new(server_transport), 1).map_err(|err| err.to_string())?;
        let client = Client::connect(Box::new(client_transport), server_addr, 1)
            .map_err(|err| err.to_string())?;
        Ok(Self {
            client,
            server,
            saw_snapshot: false,
        })
    }

    fn tick(&mut self, input: &InputState, camera: &CameraState) -> Result<(), String> {
        let move_x = bool_to_axis(input.forward, input.back);
        let move_y = bool_to_axis(input.right, input.left);
        let buttons = if input.jump { 1 } else { 0 };
        self.client
            .send_input(ClientInput {
                move_x,
                move_y,
                yaw: camera.yaw,
                pitch: camera.pitch,
                buttons,
            })
            .map_err(|err| err.to_string())?;
        let _ = self.server.tick().map_err(|err| err.to_string())?;
        self.client.poll().map_err(|err| err.to_string())?;
        if !self.saw_snapshot && self.client.last_snapshot().is_some() {
            println!("loopback snapshot received");
            self.saw_snapshot = true;
        }
        Ok(())
    }
}

struct CameraState {
    position: Vec3,
    yaw: f32,
    pitch: f32,
    velocity: Vec3,
    vertical_velocity: f32,
    on_ground: bool,
    speed: f32,
    accel: f32,
    friction: f32,
    gravity: f32,
    jump_speed: f32,
    eye_height: f32,
    step_height: f32,
    max_drop: f32,
    sensitivity: f32,
}

impl CameraState {
    fn from_bounds(bounds: &Bounds, collision: Option<&SceneCollision>) -> Self {
        let mut camera = Self::default();
        let center = bounds.center();
        let extent = bounds.extent().length().max(1.0);
        let mut position = Vec3::new(
            center.x,
            center.y + camera.eye_height,
            center.z + extent * 0.25,
        );
        if let Some(collision) = collision {
            if let Some(spawn) = collision.spawn_point(bounds, camera.eye_height) {
                position = spawn;
                camera.on_ground = true;
            } else {
                camera.position = position;
                camera.snap_to_floor(collision);
                position = camera.position;
            }
        }
        let dir = center.sub(position).normalize_or_zero();
        camera.pitch = dir.y.asin();
        camera.yaw = dir.x.atan2(-dir.z);
        camera.position = position;
        camera.speed = (extent * 0.15).max(PLAYER_SPEED);
        camera
    }

    fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            -self.yaw.cos() * self.pitch.cos(),
        )
    }

    fn forward_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.sin(), 0.0, -self.yaw.cos())
    }

    fn right_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.cos(), 0.0, self.yaw.sin())
    }

    fn update(
        &mut self,
        input: &InputState,
        dt: f32,
        collision: Option<&SceneCollision>,
        fly_mode: bool,
    ) {
        if fly_mode {
            self.update_fly(input, dt);
            return;
        }
        let mut wish_dir = Vec3::zero();
        let forward = self.forward_flat();
        let right = self.right_flat();
        if input.forward {
            wish_dir = wish_dir.add(forward);
        }
        if input.back {
            wish_dir = wish_dir.sub(forward);
        }
        if input.right {
            wish_dir = wish_dir.add(right);
        }
        if input.left {
            wish_dir = wish_dir.sub(right);
        }

        if self.on_ground {
            let speed = self.velocity.length();
            if speed > 0.0 {
                let control = speed.max(PLAYER_STOP_SPEED);
                let drop = control * self.friction * dt;
                let new_speed = (speed - drop).max(0.0);
                self.velocity = self.velocity.scale(new_speed / speed);
            }
        }
        if wish_dir.length() > 0.0 {
            let wish_dir = wish_dir.normalize_or_zero();
            let current_speed = self.velocity.dot(wish_dir);
            let add_speed = self.speed - current_speed;
            if add_speed > 0.0 {
                let accel_speed = (self.accel * dt * self.speed).min(add_speed);
                self.velocity = self.velocity.add(wish_dir.scale(accel_speed));
            }
        }

        if self.on_ground && input.jump {
            self.vertical_velocity = self.jump_speed;
            self.on_ground = false;
        }
        self.vertical_velocity -= self.gravity * dt;

        let origin = self.collision_origin();
        let velocity = Vec3::new(self.velocity.x, self.vertical_velocity, self.velocity.z);
        if let Some(scene) = collision {
            let origin = scene.try_unstuck(origin);
            let (mut new_origin, new_velocity, mut on_ground) =
                scene.move_with_step(origin, velocity, dt, self.step_height);
            if scene.hull_point_contents(scene.headnode, new_origin) == CONTENTS_SOLID {
                let unstuck = scene.try_unstuck(new_origin);
                if scene.hull_point_contents(scene.headnode, unstuck) != CONTENTS_SOLID {
                    new_origin = unstuck;
                }
            }
            let check = scene.trace(new_origin, new_origin.add(Vec3::new(0.0, -2.0, 0.0)));
            if !check.start_solid && check.fraction < 1.0 && check.plane_normal.y > FLOOR_NORMAL_MIN
            {
                new_origin = check.end;
                on_ground = true;
            }
            self.on_ground = on_ground;
            self.vertical_velocity = if on_ground && new_velocity.y < 0.0 {
                0.0
            } else {
                new_velocity.y
            };
            self.velocity.x = new_velocity.x;
            self.velocity.z = new_velocity.z;
            self.position = self.camera_from_origin(new_origin);
        } else {
            self.position = self.position.add(velocity.scale(dt));
            self.velocity.x = velocity.x;
            self.velocity.z = velocity.z;
        }
    }

    fn update_fly(&mut self, input: &InputState, dt: f32) {
        let mut direction = Vec3::zero();
        let forward = self.forward();
        let right = forward.cross(CAMERA_UP).normalize_or_zero();
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
        if input.jump {
            direction = direction.add(CAMERA_UP);
        }
        if input.down {
            direction = direction.sub(CAMERA_UP);
        }

        let direction = direction.normalize_or_zero();
        self.position = self.position.add(direction.scale(self.speed * dt));
        self.velocity = Vec3::zero();
        self.vertical_velocity = 0.0;
        self.on_ground = false;
    }

    fn snap_to_floor(&mut self, collision: &SceneCollision) {
        let origin = self.collision_origin();
        let target = origin.sub(Vec3::new(0.0, self.max_drop, 0.0));
        let trace = collision.trace(origin, target);
        if trace.start_solid {
            return;
        }
        if trace.fraction < 1.0 {
            self.position = self.camera_from_origin(trace.end);
            self.on_ground = true;
            self.vertical_velocity = 0.0;
        }
    }

    fn collision_origin(&self) -> Vec3 {
        Vec3::new(
            self.position.x,
            self.position.y - self.eye_height,
            self.position.z,
        )
    }

    fn camera_from_origin(&self, origin: Vec3) -> Vec3 {
        Vec3::new(origin.x, origin.y + self.eye_height, origin.z)
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

impl Default for CameraState {
    fn default() -> Self {
        Self {
            position: Vec3::zero(),
            yaw: 0.0,
            pitch: 0.0,
            velocity: Vec3::zero(),
            vertical_velocity: 0.0,
            on_ground: false,
            speed: PLAYER_SPEED,
            accel: PLAYER_ACCEL,
            friction: PLAYER_FRICTION,
            gravity: PLAYER_GRAVITY,
            jump_speed: PLAYER_JUMP_SPEED,
            eye_height: PLAYER_EYE_HEIGHT,
            step_height: PLAYER_STEP_HEIGHT,
            max_drop: PLAYER_MAX_DROP,
            sensitivity: 0.0025,
        }
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

#[derive(Clone, Copy)]
struct Triangle {
    a: Vec3,
    b: Vec3,
    c: Vec3,
    normal: Vec3,
}

impl Triangle {
    fn from_normal(a: Vec3, b: Vec3, c: Vec3, normal: Vec3) -> Option<Self> {
        if normal.length() == 0.0 {
            return None;
        }
        let normal = if normal.y < 0.0 {
            normal.scale(-1.0)
        } else {
            normal
        };
        Some(Self { a, b, c, normal })
    }

    fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let ny = self.normal.y;
        if ny.abs() < 1e-6 {
            return None;
        }
        let d = -self.normal.dot(self.a);
        Some(-(self.normal.x * x + self.normal.z * z + d) / ny)
    }

    fn center(&self) -> Vec3 {
        self.a.add(self.b).add(self.c).scale(1.0 / 3.0)
    }
}

struct SceneCollision {
    floors: Vec<Triangle>,
    planes: Vec<CollisionPlane>,
    clipnodes: Vec<ClipNode>,
    headnode: i32,
}

struct CollisionPlane {
    normal: Vec3,
    dist: f32,
}

struct ClipNode {
    plane_id: i32,
    children: [i32; 2],
}

struct Trace {
    fraction: f32,
    end: Vec3,
    plane_normal: Vec3,
    start_solid: bool,
    all_solid: bool,
}

fn clip_velocity(velocity: Vec3, normal: Vec3, overbounce: f32) -> Vec3 {
    let backoff = velocity.dot(normal) * overbounce;
    velocity.sub(normal.scale(backoff))
}

impl SceneCollision {
    fn spawn_point(&self, bounds: &Bounds, eye_height: f32) -> Option<Vec3> {
        let center = bounds.center();
        let mut best: Option<(f32, Vec3)> = None;
        for tri in &self.floors {
            if tri.normal.y < FLOOR_NORMAL_MIN {
                continue;
            }
            let tri_center = tri.center();
            let y = match tri.height_at(tri_center.x, tri_center.z) {
                Some(y) => y,
                None => continue,
            };
            let dx = tri_center.x - center.x;
            let dz = tri_center.z - center.z;
            let dist2 = dx * dx + dz * dz;
            let candidate = Vec3::new(tri_center.x, y + eye_height, tri_center.z);
            let replace = match best.as_ref() {
                Some((best_dist, _)) => dist2 < *best_dist,
                None => true,
            };
            if replace {
                best = Some((dist2, candidate));
            }
        }
        best.map(|(_, position)| position)
    }

    fn move_with_step(
        &self,
        start: Vec3,
        velocity: Vec3,
        dt: f32,
        step_height: f32,
    ) -> (Vec3, Vec3, bool) {
        let (down_pos, down_vel, down_ground, down_blocked) = self.slide_move(start, velocity, dt);
        if !down_ground || !down_blocked {
            return (down_pos, down_vel, down_ground);
        }

        let up = start.add(Vec3::new(0.0, step_height, 0.0));
        if !self.has_headroom(start, up) {
            return (down_pos, down_vel, down_ground);
        }

        let (step_pos, step_vel, _, _) = self.slide_move(up, velocity, dt);
        let down = step_pos.add(Vec3::new(0.0, -step_height, 0.0));
        let down_trace = self.trace(step_pos, down);
        let step_end = down_trace.end;

        let down_delta = Vec3::new(down_pos.x - start.x, 0.0, down_pos.z - start.z);
        let step_delta = Vec3::new(step_end.x - start.x, 0.0, step_end.z - start.z);
        let down_dist = down_delta.length();
        let step_dist = step_delta.length();
        if step_dist > down_dist {
            let landed = !down_trace.start_solid
                && down_trace.fraction < 1.0
                && down_trace.plane_normal.y > FLOOR_NORMAL_MIN;
            if landed {
                let mut final_vel = step_vel;
                if final_vel.y < 0.0 {
                    final_vel.y = 0.0;
                }
                return (step_end, final_vel, true);
            }
        }

        (down_pos, down_vel, down_ground)
    }

    fn slide_move(&self, start: Vec3, velocity: Vec3, dt: f32) -> (Vec3, Vec3, bool, bool) {
        let mut pos = start;
        let mut vel = velocity;
        let mut time_left = dt;
        let mut planes: Vec<Vec3> = Vec::new();
        let mut on_ground = false;
        let mut blocked = false;

        for _ in 0..4 {
            if vel.length() <= 0.0 {
                break;
            }
            let end = pos.add(vel.scale(time_left));
            let trace = self.trace(pos, end);
            if trace.all_solid {
                return (pos, Vec3::zero(), false, true);
            }
            if trace.fraction > 0.0 {
                pos = trace.end;
            }
            if trace.fraction == 1.0 {
                break;
            }
            blocked = true;

            if trace.plane_normal.y > FLOOR_NORMAL_MIN {
                on_ground = true;
            }
            time_left *= 1.0 - trace.fraction;

            planes.push(trace.plane_normal);
            let original = vel;
            let primal = vel;
            let mut new_vel = Vec3::zero();
            let mut found = false;
            for i in 0..planes.len() {
                let test = clip_velocity(original, planes[i], 1.0);
                let mut ok = true;
                for (j, plane) in planes.iter().enumerate() {
                    if j != i && test.dot(*plane) < 0.0 {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    new_vel = test;
                    found = true;
                    break;
                }
            }
            if !found {
                if planes.len() == 2 {
                    let dir = planes[0].cross(planes[1]).normalize_or_zero();
                    new_vel = dir.scale(dir.dot(primal));
                } else {
                    new_vel = Vec3::zero();
                }
            }
            vel = new_vel;
            if trace.plane_normal.y < -FLOOR_NORMAL_MIN && vel.y > 0.0 {
                vel.y = 0.0;
            }
            if vel.length() <= 1e-6 {
                vel = Vec3::zero();
                break;
            }
        }

        if on_ground && vel.y < 0.0 {
            vel.y = 0.0;
        }
        (pos, vel, on_ground, blocked)
    }

    fn trace(&self, start: Vec3, end: Vec3) -> Trace {
        let mut trace = Trace {
            fraction: 1.0,
            end,
            plane_normal: Vec3::zero(),
            start_solid: false,
            all_solid: true,
        };
        let _ = self.recursive_hull_check(self.headnode, 0.0, 1.0, start, end, &mut trace);
        if trace.start_solid {
            trace.fraction = 0.0;
            trace.end = start;
            return trace;
        }
        if trace.fraction < 1.0 {
            trace.end = start.add(end.sub(start).scale(trace.fraction));
        } else {
            trace.end = end;
        }
        trace
    }

    fn has_headroom(&self, origin: Vec3, up: Vec3) -> bool {
        let trace = self.trace(origin, up);
        !trace.start_solid && trace.fraction >= 1.0
    }

    fn try_unstuck(&self, position: Vec3) -> Vec3 {
        if self.hull_point_contents(self.headnode, position) != CONTENTS_SOLID {
            return position;
        }
        for dy in 1..=64 {
            let candidate = position.add(Vec3::new(0.0, -(dy as f32), 0.0));
            if self.hull_point_contents(self.headnode, candidate) != CONTENTS_SOLID {
                return candidate;
            }
        }
        let steps = [0.0, -2.0, -4.0, -8.0, -16.0, -24.0, -32.0, 1.0, 2.0, 4.0];
        let radii = [1.0, 2.0, 4.0, 8.0, 12.0, 16.0];
        for dy in steps {
            for radius in radii {
                for dx in [-1.0, 0.0, 1.0] {
                    for dz in [-1.0, 0.0, 1.0] {
                        if dx == 0.0 && dz == 0.0 && dy == 0.0 {
                            continue;
                        }
                        let candidate = position.add(Vec3::new(dx * radius, dy, dz * radius));
                        if self.hull_point_contents(self.headnode, candidate) != CONTENTS_SOLID {
                            return candidate;
                        }
                    }
                }
            }
        }
        position
    }

    fn recursive_hull_check(
        &self,
        node: i32,
        start_frac: f32,
        end_frac: f32,
        start: Vec3,
        end: Vec3,
        trace: &mut Trace,
    ) -> bool {
        if node < 0 {
            if node != CONTENTS_SOLID {
                trace.all_solid = false;
                return true;
            }
            trace.start_solid = true;
            return false;
        }
        let clipnode = match self.clipnodes.get(node as usize) {
            Some(node) => node,
            None => return false,
        };
        let plane = match self.planes.get(clipnode.plane_id as usize) {
            Some(plane) => plane,
            None => return false,
        };

        let start_dist = plane.normal.dot(start) - plane.dist;
        let end_dist = plane.normal.dot(end) - plane.dist;

        if start_dist >= 0.0 && end_dist >= 0.0 {
            return self.recursive_hull_check(
                clipnode.children[0],
                start_frac,
                end_frac,
                start,
                end,
                trace,
            );
        }
        if start_dist < 0.0 && end_dist < 0.0 {
            return self.recursive_hull_check(
                clipnode.children[1],
                start_frac,
                end_frac,
                start,
                end,
                trace,
            );
        }

        let mut frac = if start_dist < 0.0 {
            (start_dist + DIST_EPSILON) / (start_dist - end_dist)
        } else {
            (start_dist - DIST_EPSILON) / (start_dist - end_dist)
        };
        frac = frac.clamp(0.0, 1.0);
        let mid_frac = start_frac + (end_frac - start_frac) * frac;
        let mid = start.add(end.sub(start).scale(frac));
        let side = if start_dist < 0.0 { 1 } else { 0 };

        if !self.recursive_hull_check(
            clipnode.children[side],
            start_frac,
            mid_frac,
            start,
            mid,
            trace,
        ) {
            return false;
        }

        if self.hull_point_contents(clipnode.children[side ^ 1], mid) != CONTENTS_SOLID {
            return self.recursive_hull_check(
                clipnode.children[side ^ 1],
                mid_frac,
                end_frac,
                mid,
                end,
                trace,
            );
        }

        if trace.all_solid {
            return false;
        }

        trace.fraction = mid_frac;
        trace.plane_normal = if side == 0 {
            plane.normal
        } else {
            plane.normal.scale(-1.0)
        };
        false
    }

    fn hull_point_contents(&self, node: i32, point: Vec3) -> i32 {
        let mut node = node;
        loop {
            if node < 0 {
                return node;
            }
            let clipnode = match self.clipnodes.get(node as usize) {
                Some(node) => node,
                None => return CONTENTS_SOLID,
            };
            let plane = match self.planes.get(clipnode.plane_id as usize) {
                Some(plane) => plane,
                None => return CONTENTS_SOLID,
            };
            let dist = plane.normal.dot(point) - plane.dist;
            node = if dist >= 0.0 {
                clipnode.children[0]
            } else {
                clipnode.children[1]
            };
        }
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
    if args.play_movie.is_some() || args.playlist.is_some() {
        renderer.set_clear_color_rgba(0.0, 0.0, 0.0, 1.0);
        renderer.prewarm_yuv_pipeline();
    }
    let main_window_id = renderer.window_id();

    let audio = match AudioEngine::new() {
        Ok(audio) => Some(Rc::new(audio)),
        Err(err) => {
            eprintln!("{}", err);
            None
        }
    };
    let mut sfx_data = None;
    if let (Some(_), Some(quake_dir)) = (audio.as_ref(), args.quake_dir.as_ref()) {
        match load_wav_sfx(quake_dir, DEFAULT_SFX) {
            Ok(data) => sfx_data = Some(data),
            Err(err) => eprintln!("{}", err.message),
        }
    }

    let mut video: Option<VideoPlayback> = None;
    let mut next_video: Option<VideoPlayback> = None;
    let mut next_video_path: Option<PathBuf> = None;
    let mut next_video_start_at: Option<Instant> = None;
    let mut next_video_created_at: Option<Instant> = None;
    let mut video_start_delay_until: Option<Instant> = None;
    let video_debug = std::env::var_os("CRUSTQUAKE_VIDEO_DEBUG").is_some();
    let video_debug_stats = if video_debug {
        Some(Arc::new(VideoDebugStats::new()))
    } else {
        None
    };
    let mut last_video_debug = Instant::now();
    let mut last_underrun_frames = 0u64;
    let mut last_output_frames = 0u64;
    let mut last_video_stats = VideoDebugSnapshot {
        audio_packets: 0,
        audio_frames_in: 0,
        audio_frames_out: 0,
        audio_frames_queued: 0,
        pending_audio_frames: 0,
        audio_sample_rate: 0,
        audio_channels: 0,
        last_audio_ms: 0,
        last_video_ms: 0,
    };
    let mut playlist_paths = if let Some(playlist_path) = args.playlist.as_ref() {
        match load_playlist(playlist_path) {
            Ok(list) => list,
            Err(err) => {
                eprintln!("{}", err.message);
                std::process::exit(err.code);
            }
        }
    } else {
        let mut list = VecDeque::new();
        if let Some(movie_path) = args.play_movie.as_ref() {
            list.push_back(movie_path.clone());
        }
        list
    };
    if !playlist_paths.is_empty() {
        if let Some(audio) = audio.as_ref() {
            audio.clear_pcm();
        }
        if let Some(path) = playlist_paths.pop_front() {
            video = Some(start_video_playback(
                path,
                audio.as_ref(),
                video_debug_stats.clone(),
                VIDEO_PLAYBACK_WARM_MS,
                true,
            ));
        }
        next_video_path = playlist_paths.pop_front();
        if video.is_some() {
            video_start_delay_until =
                Some(Instant::now() + Duration::from_millis(VIDEO_INTERMISSION_MS));
            if next_video_path.is_some() {
                next_video_start_at =
                    Some(Instant::now() + Duration::from_millis(VIDEO_PREDECODE_START_DELAY_MS));
            }
            renderer.clear_textured_quad();
        }
    }

    let mut script: Option<ScriptRuntime> = None;
    if let Some(script_path) = args.script.as_ref() {
        let host_state = Rc::new(RefCell::new(ScriptHostState::new(
            args.quake_dir.clone(),
            audio.clone(),
        )));
        let spawn_state = Rc::clone(&host_state);
        let sound_state = Rc::clone(&host_state);
        let callbacks = HostCallbacks {
            spawn_entity: Box::new(move |request| spawn_state.borrow_mut().spawn_entity(request)),
            play_sound: Box::new(move |asset| sound_state.borrow_mut().play_sound(asset)),
            log: Box::new(move |msg| {
                println!("[lua] {}", msg);
            }),
        };
        let mut engine = match ScriptEngine::new(ScriptConfig::default(), callbacks) {
            Ok(engine) => engine,
            Err(err) => {
                eprintln!("script init failed: {}", err);
                std::process::exit(EXIT_USAGE);
            }
        };
        if let Err(err) = engine.load_file(script_path) {
            eprintln!("script load failed: {}", err);
            std::process::exit(EXIT_USAGE);
        }
        script = Some(ScriptRuntime {
            engine,
            _host: host_state,
        });
    }

    let mut input = InputState::default();
    let mut console = ConsoleState::default();
    let mut camera = CameraState::default();
    let mut collision: Option<SceneCollision> = None;
    let mut fly_mode = false;
    let mut scene_active = false;
    let mut loopback: Option<LoopbackNet> = None;
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

        let (mesh, bounds, scene_collision, spawn) = match load_bsp_scene(quake_dir, map) {
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

        collision = Some(scene_collision);
        camera = CameraState::from_bounds(&bounds, collision.as_ref());
        if let Some(spawn) = spawn {
            let base = quake_to_render(spawn.origin);
            camera.position = camera.camera_from_origin(base);
            if let Some(angle) = spawn.angle {
                camera.yaw = (90.0 - angle).to_radians();
                camera.pitch = 0.0;
            }
            // Test-only spawn from BSP entities; M8 Lua spawning will replace this path.
            if let Some(scene) = collision.as_ref() {
                camera.snap_to_floor(scene);
            }
        }
        scene_active = true;
        mouse_look = false;
        mouse_grabbed = set_cursor_mode(window, mouse_look);
        let aspect = aspect_ratio(renderer.size());
        renderer.update_camera(camera.view_proj(aspect));

        loopback = match LoopbackNet::start() {
            Ok(net) => Some(net),
            Err(err) => {
                eprintln!("loopback init failed: {}", err);
                None
            }
        };

        if let (Some(audio), Some(quake_dir)) = (audio.as_ref(), args.quake_dir.as_ref()) {
            match load_music_track(quake_dir) {
                Ok(Some(track)) => {
                    if let Err(err) = audio.play_music(track.data) {
                        eprintln!("{}", err);
                    } else {
                        println!("streaming {}", track.name);
                    }
                }
                Ok(None) => {}
                Err(err) => eprintln!("{}", err.message),
            }
        }
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
                        let is_repeat = event.repeat;
                        if pressed && code == KeyCode::Space && video.is_some() {
                            let advanced = advance_playlist(
                                &mut video,
                                &mut next_video,
                                &mut playlist_paths,
                                audio.as_ref(),
                                video_debug_stats.clone(),
                                &mut next_video_path,
                                true,
                            );
                            if !advanced {
                                elwt.exit();
                            }
                            video_start_delay_until = Some(
                                Instant::now() + Duration::from_millis(VIDEO_INTERMISSION_MS),
                            );
                            next_video_start_at = next_video_path
                                .as_ref()
                                .map(|_| Instant::now() + Duration::from_millis(
                                    VIDEO_PREDECODE_START_DELAY_MS,
                                ));
                            next_video_created_at = None;
                            renderer.clear_textured_quad();
                            if video_debug {
                                last_underrun_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_underrun_frames())
                                    .unwrap_or(0);
                                last_output_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.output_frames())
                                    .unwrap_or(0);
                                last_video_stats = VideoDebugSnapshot {
                                    audio_packets: 0,
                                    audio_frames_in: 0,
                                    audio_frames_out: 0,
                                    audio_frames_queued: 0,
                                    pending_audio_frames: 0,
                                    audio_sample_rate: 0,
                                    audio_channels: 0,
                                    last_audio_ms: 0,
                                    last_video_ms: 0,
                                };
                            }
                            return;
                        }
                        if pressed && !is_repeat && code == KeyCode::Backquote {
                            console.active = !console.active;
                            window.set_ime_allowed(console.active);
                            if console.active {
                                console.buffer.clear();
                                input = InputState::default();
                                mouse_look = false;
                                mouse_grabbed = set_cursor_mode(window, mouse_look);
                                println!("console: open");
                            } else {
                                println!("console: closed");
                            }
                        } else if console.active {
                            if pressed {
                                match code {
                                    KeyCode::Enter | KeyCode::NumpadEnter => {
                                        let line = console.buffer.trim().to_string();
                                        console.buffer.clear();
                                        if !line.is_empty() {
                                            println!("> {}", line);
                                            if let Some((command, args)) = parse_command_line(&line)
                                            {
                                                if let Some(script) = script.as_mut() {
                                                    match script.engine.run_command(&command, &args)
                                                    {
                                                        Ok(true) => {}
                                                        Ok(false) => {
                                                            eprintln!(
                                                                "unknown script command: {}",
                                                                command
                                                            );
                                                        }
                                                        Err(err) => {
                                                            eprintln!(
                                                                "lua command failed: {}",
                                                                err
                                                            );
                                                        }
                                                    }
                                                } else {
                                                    eprintln!("no script loaded");
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Backspace => {
                                        console.buffer.pop();
                                    }
                                    KeyCode::Escape => {
                                        console.active = false;
                                        console.buffer.clear();
                                        window.set_ime_allowed(false);
                                        println!("console: closed");
                                    }
                                    _ => {}
                                }
                            }
                        } else {
                            match code {
                                KeyCode::KeyW => input.forward = pressed,
                                KeyCode::KeyS => input.back = pressed,
                                KeyCode::KeyA => input.left = pressed,
                                KeyCode::KeyD => input.right = pressed,
                                KeyCode::Space => input.jump = pressed,
                                KeyCode::ShiftLeft => input.down = pressed,
                                KeyCode::KeyF if pressed => {
                                    fly_mode = !fly_mode;
                                    if fly_mode {
                                        camera.velocity = Vec3::zero();
                                        camera.vertical_velocity = 0.0;
                                        camera.on_ground = false;
                                    } else if let Some(scene) = collision.as_ref() {
                                        camera.snap_to_floor(scene);
                                    }
                                }
                                KeyCode::KeyP if pressed => {
                                    if let (Some(audio), Some(data)) =
                                        (audio.as_ref(), sfx_data.as_ref())
                                    {
                                        if let Err(err) = audio.play_wav(data.clone()) {
                                            eprintln!("{}", err);
                                        }
                                    }
                                }
                                KeyCode::Escape if pressed => {
                                    mouse_look = false;
                                    mouse_grabbed = set_cursor_mode(window, mouse_look);
                                }
                                _ => {}
                            }
                            if let Some(script) = script.as_mut() {
                                let key = key_name(code);
                                if let Err(err) = script.engine.on_key(&key, pressed) {
                                    eprintln!("lua on_key failed: {}", err);
                                }
                            }
                        }
                    }
                }
                WindowEvent::Ime(Ime::Commit(text)) => {
                    if console.active {
                        console.buffer.push_str(&text);
                    }
                }
                WindowEvent::Ime(_) => {}
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
                    if console.active {
                        console.active = false;
                        console.buffer.clear();
                        window.set_ime_allowed(false);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let now = Instant::now();
                    let dt = (now - last_frame).as_secs_f32().min(0.1);
                    last_frame = now;
                    if let Some(until) = video_start_delay_until {
                        if now >= until {
                            video_start_delay_until = None;
                        }
                    }

                    if let Some(script) = script.as_mut() {
                        if let Err(err) = script.engine.on_tick(dt) {
                            eprintln!("lua on_tick failed: {}", err);
                        }
                    }

                    if next_video.is_none() {
                        let should_start = next_video_start_at
                            .map(|start_at| now >= start_at)
                            .unwrap_or(false);
                        if should_start {
                            if let (Some(path), Some(current)) =
                                (next_video_path.take(), video.as_ref())
                            {
                                let buffered_audio_ms = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_buffered_ms())
                                    .unwrap_or(0);
                                let ready_for_predecode = current.is_started()
                                    && current.elapsed_ms() >= VIDEO_PREDECODE_MIN_ELAPSED_MS
                                    && current.frame_queue_len() >= VIDEO_PREDECODE_MIN_FRAMES
                                    && buffered_audio_ms >= VIDEO_PREDECODE_MIN_AUDIO_MS;
                                if ready_for_predecode {
                                    next_video = Some(start_video_playback(
                                        path,
                                        audio.as_ref(),
                                        video_debug_stats.clone(),
                                        VIDEO_PREDECODE_WARM_MS,
                                        true,
                                    ));
                                    next_video_created_at = Some(now);
                                    next_video_start_at = None;
                                } else {
                                    next_video_path = Some(path);
                                }
                            }
                        }
                    }

                    if let Some(preload) = next_video.as_mut() {
                        if let Some(created_at) = next_video_created_at {
                            let elapsed = now.saturating_duration_since(created_at);
                            let elapsed_ms = elapsed.as_millis() as u64;
                            let start_ms =
                                VIDEO_PREDECODE_WARM_MS.min(VIDEO_MAX_QUEUED_MS_PREDECODE);
                            let target_ms = if elapsed_ms >= VIDEO_PREDECODE_RAMP_MS {
                                VIDEO_MAX_QUEUED_MS_PREDECODE
                            } else {
                                let full_ms = VIDEO_MAX_QUEUED_MS_PREDECODE;
                                let ramp = (full_ms - start_ms)
                                    .saturating_mul(elapsed_ms)
                                    / VIDEO_PREDECODE_RAMP_MS.max(1);
                                start_ms + ramp
                            };
                            preload.set_max_queued_video_ms(target_ms);
                        }
                        if let Err(err) = preload.drain_events() {
                            eprintln!("video preload failed: {}", err);
                            preload.stop();
                            next_video = None;
                            next_video_created_at = None;
                        }
                    }

                    let delay_active = video_start_delay_until.is_some();
                    let mut advance_video = false;
                    if let Some(video) = video.as_mut() {
                        if let Err(err) = video.drain_events() {
                            eprintln!("{}", err);
                            video.stop();
                            if let Some(audio) = audio.as_ref() {
                                audio.clear_pcm();
                            }
                        }
                        if let Some(audio) = audio.as_ref() {
                            if video.take_audio_finished() {
                                audio.clear_pcm();
                            }
                        }
                        if video.is_started() {
                            let elapsed = video.elapsed_ms();
                            let target_ms = if elapsed >= VIDEO_PLAYBACK_WARM_UP_MS {
                                VIDEO_MAX_QUEUED_MS_PLAYBACK
                            } else {
                                let start_ms = VIDEO_PLAYBACK_WARM_MS;
                                let full_ms = VIDEO_MAX_QUEUED_MS_PLAYBACK;
                                let ramp = (full_ms - start_ms)
                                    .saturating_mul(elapsed)
                                    / VIDEO_PLAYBACK_WARM_UP_MS.max(1);
                                start_ms + ramp
                            };
                            video.set_max_queued_video_ms(target_ms);
                        }
                        if video_debug {
                            let now = Instant::now();
                            if now.duration_since(last_video_debug) >= Duration::from_secs(1) {
                                let buffered_ms = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_buffered_ms())
                                    .unwrap_or(0);
                                let underrun_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.pcm_underrun_frames())
                                    .unwrap_or(0);
                                let output_frames = audio
                                    .as_ref()
                                    .map(|engine| engine.output_frames())
                                    .unwrap_or(0);
                                let delta_output_frames =
                                    output_frames.saturating_sub(last_output_frames);
                                last_output_frames = output_frames;
                                let delta_underrun =
                                    underrun_frames.saturating_sub(last_underrun_frames);
                                last_underrun_frames = underrun_frames;
                                if let Some(snapshot) = video.debug_snapshot() {
                                    let delta_packets = snapshot
                                        .audio_packets
                                        .saturating_sub(last_video_stats.audio_packets);
                                    let delta_in = snapshot
                                        .audio_frames_in
                                        .saturating_sub(last_video_stats.audio_frames_in);
                                    let delta_out = snapshot
                                        .audio_frames_out
                                        .saturating_sub(last_video_stats.audio_frames_out);
                                    let delta_queued = snapshot
                                        .audio_frames_queued
                                        .saturating_sub(last_video_stats.audio_frames_queued);
                                    last_video_stats = snapshot;
                                    let device_rate = audio
                                        .as_ref()
                                        .map(|engine| engine.output_sample_rate())
                                        .unwrap_or(0);
                                    let device_channels = audio
                                        .as_ref()
                                        .map(|engine| engine.output_channels())
                                        .unwrap_or(0);
                                    println!(
                                        "video dbg: elapsed={}ms frames={} buffered={}ms underrun_frames+={} total_underrun_frames={} device_frames+={} device_rate={} device_channels={} audio_packets+={} audio_frames_in+={} audio_frames_out+={} audio_frames_queued+={} pending_frames={} packet_rate={} packet_channels={} last_audio_ms={} last_video_ms={}",
                                        video.elapsed_ms(),
                                        video.frame_queue_len(),
                                        buffered_ms,
                                        delta_underrun,
                                        underrun_frames,
                                        delta_output_frames,
                                        device_rate,
                                        device_channels,
                                        delta_packets,
                                        delta_in,
                                        delta_out,
                                        delta_queued,
                                        last_video_stats.pending_audio_frames,
                                        last_video_stats.audio_sample_rate,
                                        last_video_stats.audio_channels,
                                        last_video_stats.last_audio_ms,
                                        last_video_stats.last_video_ms
                                    );
                                } else {
                                    println!(
                                        "video dbg: elapsed={}ms frames={} buffered={}ms underrun_frames+={} total_underrun_frames={} device_frames+={}",
                                        video.elapsed_ms(),
                                        video.frame_queue_len(),
                                        buffered_ms,
                                        delta_underrun,
                                        underrun_frames,
                                        delta_output_frames
                                    );
                                }
                                last_video_debug = now;
                            }
                        }
                        if !delay_active {
                            if !video.is_started() {
                                let mut preview_uploaded = false;
                                if let Some(frame) = video.preview_frame() {
                                    if let Ok(image) = YuvImageView::new(
                                        frame.width,
                                        frame.height,
                                        frame.y_plane(),
                                        frame.u_plane(),
                                        frame.v_plane(),
                                    ) {
                                        if let Err(err) = renderer.update_yuv_image_view(&image) {
                                            eprintln!("video frame upload failed: {}", err);
                                        } else {
                                            preview_uploaded = true;
                                        }
                                    }
                                }
                                if preview_uploaded {
                                    video.mark_frame_uploaded();
                                }
                            }
                            if !video.is_started() {
                                if let Some(audio) = audio.as_ref() {
                                    if video.has_frames()
                                        && video.frame_queue_len() >= VIDEO_START_MIN_FRAMES
                                        && video.is_ready_to_start()
                                        && video.prebuffered_audio_ms()
                                            >= VIDEO_AUDIO_PREBUFFER_MS
                                    {
                                        video.start_with_clock(audio.clock().time_ms());
                                    }
                                } else if video.has_frames()
                                    && video.is_ready_to_start()
                                    && video.frame_queue_len() >= VIDEO_START_MIN_FRAMES
                                {
                                    video.start_now();
                                }
                            }
                            let elapsed_ms = video.elapsed_ms();
                            if let Some(frame) = video.next_frame(elapsed_ms) {
                                if let Ok(image) = YuvImageView::new(
                                    frame.width,
                                    frame.height,
                                    frame.y_plane(),
                                    frame.u_plane(),
                                    frame.v_plane(),
                                ) {
                                    if let Err(err) =
                                        renderer.update_yuv_image_view(&image)
                                    {
                                        eprintln!("video frame upload failed: {}", err);
                                    } else {
                                        video.mark_frame_uploaded();
                                    }
                                }
                            }
                        }
                        if video.is_finished() {
                            advance_video = true;
                        }
                    }
                    if advance_video {
                        let advanced = advance_playlist(
                            &mut video,
                            &mut next_video,
                            &mut playlist_paths,
                            audio.as_ref(),
                            video_debug_stats.clone(),
                            &mut next_video_path,
                            true,
                        );
                        if !advanced {
                            elwt.exit();
                        }
                        video_start_delay_until = Some(
                            Instant::now() + Duration::from_millis(VIDEO_INTERMISSION_MS),
                        );
                        next_video_start_at = next_video_path
                            .as_ref()
                            .map(|_| Instant::now() + Duration::from_millis(
                                VIDEO_PREDECODE_START_DELAY_MS,
                            ));
                        next_video_created_at = None;
                        renderer.clear_textured_quad();
                        if video_debug {
                            last_underrun_frames = audio
                                .as_ref()
                                .map(|engine| engine.pcm_underrun_frames())
                                .unwrap_or(0);
                            last_output_frames = audio
                                .as_ref()
                                .map(|engine| engine.output_frames())
                                .unwrap_or(0);
                            last_video_stats = VideoDebugSnapshot {
                                audio_packets: 0,
                                audio_frames_in: 0,
                                audio_frames_out: 0,
                                audio_frames_queued: 0,
                                pending_audio_frames: 0,
                                audio_sample_rate: 0,
                                audio_channels: 0,
                                last_audio_ms: 0,
                                last_video_ms: 0,
                            };
                        }
                    }

                    if scene_active {
                        camera.update(&input, dt, collision.as_ref(), fly_mode);
                        let aspect = aspect_ratio(renderer.size());
                        renderer.update_camera(camera.view_proj(aspect));
                        if let Some(loopback_net) = loopback.as_mut() {
                            if let Err(err) = loopback_net.tick(&input, &camera) {
                                eprintln!("loopback tick failed: {}", err);
                                loopback = None;
                            }
                        }
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
    let mut play_movie = None;
    let mut playlist = None;
    let mut script = None;
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
            "--play-movie" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--play-movie expects a path".into()))?;
                play_movie = Some(PathBuf::from(value));
            }
            "--playlist" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--playlist expects a path".into()))?;
                playlist = Some(PathBuf::from(value));
            }
            "--script" => {
                let value = args
                    .next()
                    .ok_or_else(|| ArgParseError::Message("--script expects a path".into()))?;
                script = Some(PathBuf::from(value));
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
    if play_movie.is_some() && playlist.is_some() {
        return Err(ArgParseError::Message(
            "--play-movie cannot be used with --playlist".into(),
        ));
    }
    if play_movie.is_some() && (show_image.is_some() || map.is_some()) {
        return Err(ArgParseError::Message(
            "--play-movie cannot be used with --show-image or --map".into(),
        ));
    }
    if playlist.is_some() && (show_image.is_some() || map.is_some()) {
        return Err(ArgParseError::Message(
            "--playlist cannot be used with --show-image or --map".into(),
        ));
    }

    Ok(CliArgs {
        quake_dir,
        show_image,
        map,
        play_movie,
        playlist,
        script,
    })
}

fn print_usage() {
    eprintln!("usage: pallet [--quake-dir <path>] [--show-image <asset>] [--map <name>] [--play-movie <file>] [--playlist <file>] [--script <path>]");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --show-image gfx/conback.lmp");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1");
    eprintln!("example: pallet --quake-dir \"C:\\\\Quake\" --map e1m1 --script scripts/demo.lua");
    eprintln!("example: pallet --play-movie intro.ogv");
    eprintln!("example: pallet --playlist movies_playlist.txt");
}

fn load_playlist(path: &Path) -> Result<VecDeque<PathBuf>, ExitError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| ExitError::new(EXIT_USAGE, format!("playlist read failed: {}", err)))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = VecDeque::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let entry = PathBuf::from(line);
        let entry = if entry.is_relative() {
            base.join(entry)
        } else {
            entry
        };
        entries.push_back(entry);
    }
    if entries.is_empty() {
        return Err(ExitError::new(EXIT_USAGE, "playlist is empty".to_string()));
    }
    Ok(entries)
}

fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::KeyW => "W".to_string(),
        KeyCode::KeyA => "A".to_string(),
        KeyCode::KeyS => "S".to_string(),
        KeyCode::KeyD => "D".to_string(),
        KeyCode::KeyF => "F".to_string(),
        KeyCode::KeyK => "K".to_string(),
        KeyCode::KeyP => "P".to_string(),
        KeyCode::Space => "Space".to_string(),
        KeyCode::ShiftLeft => "ShiftLeft".to_string(),
        KeyCode::Escape => "Escape".to_string(),
        _ => format!("{:?}", code),
    }
}

fn parse_command_line(line: &str) -> Option<(String, Vec<String>)> {
    let mut parts = line.split_whitespace();
    let command = parts.next()?.to_string();
    let args = parts.map(|part| part.to_string()).collect();
    Some((command, args))
}

fn load_wav_sfx(quake_dir: &Path, asset: &str) -> Result<Vec<u8>, ExitError> {
    let (pak, pak_path) = load_pak_from_quake_dir(quake_dir)?;
    let asset_name = normalize_asset_name(asset);
    let wav_bytes = pak
        .entry_data(&asset_name)
        .map_err(|err| ExitError::new(EXIT_PAK, format!("asset lookup failed: {}", err)))?
        .ok_or_else(|| {
            ExitError::new(
                EXIT_PAK,
                format!("asset not found in pak0.pak: {}", asset_name),
            )
        })?;
    println!("loaded {} from {}", asset_name, pak_path.display());
    Ok(wav_bytes.to_vec())
}

fn load_music_track(quake_dir: &Path) -> Result<Option<MusicTrack>, ExitError> {
    let mut vfs = Vfs::new();
    vfs.add_root(quake_dir);
    for dir in ["id1/music", "music"] {
        if let Some(track) = find_music_in_dir(&vfs, dir)? {
            return Ok(Some(track));
        }
    }

    let (pak, _) = load_pak_from_quake_dir(quake_dir)?;
    Ok(find_music_in_pak(&pak))
}

fn find_music_in_dir(vfs: &Vfs, dir: &str) -> Result<Option<MusicTrack>, ExitError> {
    let entries = match vfs.list_dir(dir) {
        Ok(entries) => entries,
        Err(VfsError::NotFound(_)) => return Ok(None),
        Err(err) => {
            return Err(ExitError::new(
                EXIT_PAK,
                format!("music scan failed: {}", err),
            ))
        }
    };

    let mut candidates: Vec<String> = entries
        .into_iter()
        .filter(|entry| !entry.is_dir && entry.name.to_lowercase().ends_with(".ogg"))
        .map(|entry| entry.name)
        .collect();
    candidates.sort();

    if let Some(name) = candidates.into_iter().next() {
        let path = format!("{}/{}", dir, name);
        return match vfs.read(&path) {
            Ok(data) => Ok(Some(MusicTrack { name: path, data })),
            Err(err) => Err(ExitError::new(
                EXIT_PAK,
                format!("music read failed: {}", err),
            )),
        };
    }

    Ok(None)
}

fn find_music_in_pak(pak: &PakFile) -> Option<MusicTrack> {
    let mut candidates: Vec<String> = pak
        .entries()
        .iter()
        .filter(|entry| entry.name.starts_with("music/"))
        .map(|entry| entry.name.clone())
        .filter(|name| name.to_lowercase().ends_with(".ogg"))
        .collect();
    candidates.sort();

    for name in candidates {
        if let Ok(Some(bytes)) = pak.entry_data(&name) {
            return Some(MusicTrack {
                name,
                data: bytes.to_vec(),
            });
        }
    }
    None
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

fn load_bsp_scene(
    quake_dir: &Path,
    map: &str,
) -> Result<(MeshData, Bounds, SceneCollision, Option<SpawnPoint>), ExitError> {
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

    let spawn = bsp::parse_spawn(bsp_bytes, &bsp.header)
        .map_err(|err| ExitError::new(EXIT_BSP, format!("bsp spawn parse failed: {}", err)))?;

    let (mesh, bounds, collision) = build_scene_mesh(&bsp)?;
    Ok((mesh, bounds, collision, spawn))
}

fn build_scene_mesh(bsp: &Bsp) -> Result<(MeshData, Bounds, SceneCollision), ExitError> {
    let face_range = bsp.world_face_range().unwrap_or(0..bsp.faces.len());

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut floors = Vec::new();
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
            if let Some(triangle) = Triangle::from_normal(v0, v1, v2, normal) {
                if triangle.normal.y >= FLOOR_NORMAL_MIN {
                    floors.push(triangle);
                }
            }
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
    let planes = bsp
        .planes
        .iter()
        .map(|plane| CollisionPlane {
            normal: quake_to_render(plane.normal),
            dist: plane.dist,
        })
        .collect();
    let clipnodes = bsp
        .clipnodes
        .iter()
        .map(|node| ClipNode {
            plane_id: node.plane_id,
            children: node.children,
        })
        .collect();
    let headnode = if bsp.clipnodes.is_empty() {
        -1
    } else {
        bsp.models
            .first()
            .map(|model| model.headnode[1])
            .unwrap_or(0)
    };
    Ok((
        mesh,
        bounds,
        SceneCollision {
            floors,
            planes,
            clipnodes,
            headnode,
        },
    ))
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

fn bool_to_axis(positive: bool, negative: bool) -> f32 {
    (positive as i32 - negative as i32) as f32
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
