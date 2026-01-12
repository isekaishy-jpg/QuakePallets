//! RPG movement motor (stability-first velocity intent).
#![forbid(unsafe_code)]

use rapier3d::math::Vector;
use rapier3d::prelude::Real;

#[derive(Clone, Copy, Debug)]
pub struct RpgMotorConfig {
    pub max_speed_ground: Real,
    pub max_speed_air: Real,
    pub ground_accel: Real,
    pub air_accel: Real,
    pub friction: Real,
    pub stop_speed: Real,
    pub gravity: Real,
    pub jump_speed: Real,
    pub air_control_scale: Real,
    /// Smoothing time constant in seconds (0 disables).
    pub input_smoothing: Real,
    /// Max turn rate in radians/sec (0 disables).
    pub turn_rate: Real,
}

impl Default for RpgMotorConfig {
    fn default() -> Self {
        Self {
            max_speed_ground: 3.5,
            max_speed_air: 2.5,
            ground_accel: 10.0,
            air_accel: 3.0,
            friction: 8.0,
            stop_speed: 1.0,
            gravity: 9.81,
            jump_speed: 4.0,
            air_control_scale: 0.25,
            input_smoothing: 0.08,
            turn_rate: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RpgMotorInput {
    pub move_axis: [Real; 2],
    pub jump: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct RpgMotorState {
    pub velocity: Vector<Real>,
    pub grounded: bool,
    pub ground_normal: Option<Vector<Real>>,
    pub yaw: Real,
}

#[derive(Clone, Copy, Debug)]
pub struct MoveIntent {
    pub dir: Vector<Real>,
    pub mag: Real,
}

#[derive(Clone, Copy, Debug)]
pub struct RpgMotorOutput {
    pub desired_translation: Vector<Real>,
    pub next_velocity: Vector<Real>,
    pub jumped: bool,
}

pub struct RpgMotor {
    config: RpgMotorConfig,
    smoothed_axis: [Real; 2],
}

impl RpgMotor {
    pub fn new(config: RpgMotorConfig) -> Self {
        Self {
            config,
            smoothed_axis: [0.0, 0.0],
        }
    }

    pub fn config(&self) -> RpgMotorConfig {
        self.config
    }

    pub fn config_mut(&mut self) -> &mut RpgMotorConfig {
        &mut self.config
    }

    pub fn reset_state(&mut self) {
        self.smoothed_axis = [0.0, 0.0];
    }

    pub fn step(&mut self, input: RpgMotorInput, state: RpgMotorState, dt: Real) -> RpgMotorOutput {
        let dt = dt.max(0.0);
        let axis = normalize_axis(input.move_axis);
        let axis = self.apply_input_smoothing(axis, dt);
        let mut intent = build_move_intent(state.yaw, axis, state.grounded, state.ground_normal);

        let max_speed = if state.grounded {
            self.config.max_speed_ground
        } else {
            self.config.max_speed_air
        };
        let control_scale = if state.grounded {
            1.0
        } else {
            self.config.air_control_scale.clamp(0.0, 1.0)
        };
        let mut velocity = state.velocity;
        let mut planar = if state.grounded {
            project_onto_ground(velocity, state.ground_normal)
        } else {
            Vector::new(velocity.x, 0.0, velocity.z)
        };

        if self.config.turn_rate > 0.0 && intent.mag > 0.0 {
            let max_turn = self.config.turn_rate.max(0.0) * dt;
            intent.dir = apply_turn_limit(planar, intent.dir, max_turn);
        }

        if state.grounded {
            let (friction, stop_speed) = slope_friction_tuning(
                self.config.friction,
                self.config.stop_speed,
                state.ground_normal,
            );
            planar = apply_friction(planar, friction, stop_speed, dt);
        }

        if intent.mag > 0.0 && control_scale > 0.0 {
            let wish_speed = intent.mag * max_speed * control_scale;
            let accel = if state.grounded {
                self.config.ground_accel
            } else {
                self.config.air_accel * control_scale
            };
            planar = accelerate(planar, intent.dir, wish_speed, accel, dt);
        }

        let mut jumped = false;
        if state.grounded && !input.jump {
            if let Some(normal) = state.ground_normal {
                if intent.mag > 0.0 || planar.norm_squared() > 1.0e-4 {
                    let gravity = Vector::new(0.0, -self.config.gravity, 0.0);
                    let slope_gravity = gravity - normal * gravity.dot(&normal);
                    planar += slope_gravity * dt;
                }
            }
        }
        if state.grounded {
            if input.jump {
                velocity.x = planar.x;
                velocity.y = self.config.jump_speed;
                velocity.z = planar.z;
                jumped = true;
            } else {
                velocity = planar;
            }
        }
        if !state.grounded && !jumped {
            velocity.y -= self.config.gravity * dt;
            velocity.x = planar.x;
            velocity.z = planar.z;
        }

        RpgMotorOutput {
            desired_translation: velocity * dt,
            next_velocity: velocity,
            jumped,
        }
    }

    fn apply_input_smoothing(&mut self, axis: [Real; 2], dt: Real) -> [Real; 2] {
        if self.config.input_smoothing <= 0.0 {
            self.smoothed_axis = axis;
            return axis;
        }
        let denom = self.config.input_smoothing.max(1.0e-6) + dt;
        let alpha = (dt / denom).clamp(0.0, 1.0);
        self.smoothed_axis[0] = lerp(self.smoothed_axis[0], axis[0], alpha);
        self.smoothed_axis[1] = lerp(self.smoothed_axis[1], axis[1], alpha);
        self.smoothed_axis
    }
}

fn normalize_axis(axis: [Real; 2]) -> [Real; 2] {
    let len = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
    if len > 1.0 {
        [axis[0] / len, axis[1] / len]
    } else {
        axis
    }
}

pub fn build_move_intent(
    yaw: Real,
    axis: [Real; 2],
    grounded: bool,
    ground_normal: Option<Vector<Real>>,
) -> MoveIntent {
    let forward = Vector::new(yaw.sin(), 0.0, -yaw.cos());
    let right = Vector::new(yaw.cos(), 0.0, yaw.sin());
    let intent = right * axis[0] + forward * axis[1];
    let mag = intent.norm().min(1.0);
    if mag <= 0.0 {
        return MoveIntent {
            dir: Vector::zeros(),
            mag: 0.0,
        };
    }
    let mut dir = intent / mag;
    if grounded {
        if let Some(normal) = ground_normal {
            let projected = dir - normal * dir.dot(&normal);
            if projected.norm_squared() > 0.0 {
                dir = projected.normalize();
            }
        }
    }
    MoveIntent { dir, mag }
}

fn apply_turn_limit(
    velocity: Vector<Real>,
    desired_dir: Vector<Real>,
    max_turn: Real,
) -> Vector<Real> {
    let speed = velocity.norm();
    if speed <= 0.0 || desired_dir.norm_squared() <= 0.0 || max_turn <= 0.0 {
        return desired_dir;
    }
    let vel_dir = velocity / speed;
    let desired_dir = desired_dir.normalize();
    let dot = vel_dir.dot(&desired_dir).clamp(-1.0, 1.0);
    let angle = dot.acos();
    if angle <= max_turn {
        return desired_dir;
    }
    let t = (max_turn / angle).clamp(0.0, 1.0);
    let blended = vel_dir * (1.0 - t) + desired_dir * t;
    if blended.norm_squared() <= 1.0e-6 {
        desired_dir
    } else {
        blended.normalize()
    }
}

fn accelerate(
    velocity: Vector<Real>,
    wish_dir: Vector<Real>,
    wish_speed: Real,
    accel: Real,
    dt: Real,
) -> Vector<Real> {
    if wish_speed <= 0.0 || accel <= 0.0 {
        return velocity;
    }
    let current_speed = velocity.dot(&wish_dir);
    let add_speed = wish_speed - current_speed;
    if add_speed <= 0.0 {
        return velocity;
    }
    let accel_speed = (accel * dt * wish_speed).min(add_speed);
    velocity + wish_dir * accel_speed
}

fn apply_friction(
    velocity: Vector<Real>,
    friction: Real,
    stop_speed: Real,
    dt: Real,
) -> Vector<Real> {
    let speed = velocity.norm();
    if speed <= 0.0 || friction <= 0.0 {
        return velocity;
    }
    let control = speed.max(stop_speed);
    let drop = control * friction * dt;
    let new_speed = (speed - drop).max(0.0);
    if new_speed == speed {
        return velocity;
    }
    let scale = new_speed / speed;
    velocity * scale
}

fn project_onto_ground(
    velocity: Vector<Real>,
    ground_normal: Option<Vector<Real>>,
) -> Vector<Real> {
    let Some(normal) = ground_normal else {
        return Vector::new(velocity.x, 0.0, velocity.z);
    };
    let projected = velocity - normal * velocity.dot(&normal);
    if projected.norm_squared() <= 1.0e-6 {
        Vector::new(velocity.x, 0.0, velocity.z)
    } else {
        projected
    }
}

fn slope_friction_tuning(
    friction: Real,
    stop_speed: Real,
    ground_normal: Option<Vector<Real>>,
) -> (Real, Real) {
    let Some(normal) = ground_normal else {
        return (friction, stop_speed);
    };
    let slope_dir = Vector::new(normal.x, 0.0, normal.z);
    let slope_len2 = slope_dir.norm_squared();
    if slope_len2 <= 1.0e-6 {
        return (friction, stop_speed);
    }
    let slope_factor = (1.0 - normal.y.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    let scale = (1.0 - slope_factor * 4.0).clamp(0.15, 1.0);
    (friction * scale, stop_speed * scale)
}

fn lerp(a: Real, b: Real, t: Real) -> Real {
    a + (b - a) * t
}
