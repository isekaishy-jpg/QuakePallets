//! Arena movement motor (velocity intent only).
#![forbid(unsafe_code)]

use rapier3d::math::Vector;
use rapier3d::prelude::Real;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrictionlessJumpMode {
    None,
    #[default]
    Soft,
    Hard,
}

#[derive(Clone, Copy, Debug)]
pub struct ArenaMotorConfig {
    pub max_speed_ground: Real,
    pub max_speed_air: Real,
    pub ground_accel: Real,
    pub air_accel: Real,
    pub friction: Real,
    pub stop_speed: Real,
    pub gravity: Real,
    pub jump_speed: Real,
    pub air_resistance: Real,
    pub air_resistance_speed_scale: Real,
    pub golden_angle_target: Real,
    pub golden_angle_gain_min: Real,
    pub golden_angle_gain_peak: Real,
    pub golden_angle_bonus_scale: Real,
    pub golden_angle_blend_speed_start: Real,
    pub golden_angle_blend_speed_end: Real,
    pub corridor_shaping_strength: Real,
    pub corridor_shaping_min_speed: Real,
    pub corridor_shaping_max_angle_per_tick: Real,
    pub corridor_shaping_min_alignment: Real,
    pub jump_buffer_enabled: bool,
    pub jump_buffer_window: Real,
    pub frictionless_jump_mode: FrictionlessJumpMode,
    pub frictionless_jump_grace: Real,
    pub frictionless_jump_friction_scale: Real,
    pub frictionless_jump_friction_scale_best_angle: Real,
}

impl Default for ArenaMotorConfig {
    fn default() -> Self {
        let max_speed_ground = 4.0;
        let max_speed_air = max_speed_ground;
        let golden_angle_target = 45.0_f32.to_radians();
        Self {
            max_speed_ground,
            max_speed_air,
            ground_accel: 18.0,
            air_accel: 18.0,
            friction: 8.0,
            stop_speed: 1.0,
            gravity: 9.81,
            jump_speed: 4.5,
            air_resistance: 0.0,
            air_resistance_speed_scale: max_speed_air * 2.0,
            golden_angle_target,
            golden_angle_gain_min: 1.0,
            golden_angle_gain_peak: 1.25,
            golden_angle_bonus_scale: 0.5,
            golden_angle_blend_speed_start: max_speed_ground,
            golden_angle_blend_speed_end: max_speed_ground * 1.4,
            corridor_shaping_strength: 0.0,
            corridor_shaping_min_speed: 0.0,
            corridor_shaping_max_angle_per_tick: 0.0,
            corridor_shaping_min_alignment: -1.0,
            jump_buffer_enabled: true,
            jump_buffer_window: 0.1,
            frictionless_jump_mode: FrictionlessJumpMode::Soft,
            frictionless_jump_grace: 0.1,
            frictionless_jump_friction_scale: 0.25,
            frictionless_jump_friction_scale_best_angle: 0.25,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ArenaMotorInput {
    pub move_axis: [Real; 2],
    pub jump: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ArenaMotorState {
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
pub struct GoldenAngleMetrics {
    pub theta: Real,
    pub gain: Real,
    pub quality: Real,
}

#[derive(Clone, Copy, Debug)]
pub struct ArenaMotorOutput {
    pub desired_translation: Vector<Real>,
    pub next_velocity: Vector<Real>,
    pub jumped: bool,
}

pub struct ArenaMotor {
    config: ArenaMotorConfig,
    jump_buffer_time: Real,
    bhop_grace_time: Real,
    was_grounded: bool,
}

impl ArenaMotor {
    pub fn new(config: ArenaMotorConfig) -> Self {
        Self {
            config,
            jump_buffer_time: 0.0,
            bhop_grace_time: 0.0,
            was_grounded: false,
        }
    }

    pub fn config(&self) -> ArenaMotorConfig {
        self.config
    }

    pub fn config_mut(&mut self) -> &mut ArenaMotorConfig {
        &mut self.config
    }

    pub fn reset_state(&mut self) {
        self.jump_buffer_time = 0.0;
        self.bhop_grace_time = 0.0;
        self.was_grounded = false;
    }

    pub fn step(
        &mut self,
        input: ArenaMotorInput,
        state: ArenaMotorState,
        dt: Real,
    ) -> ArenaMotorOutput {
        let dt = dt.max(0.0);
        let move_intent = build_move_intent(
            state.yaw,
            input.move_axis,
            state.grounded,
            state.ground_normal,
        );
        let max_speed = if state.grounded {
            self.config.max_speed_ground
        } else {
            self.config.max_speed_air
        };
        let planar_velocity = Vector::new(state.velocity.x, 0.0, state.velocity.z);
        let view_forward = Vector::new(state.yaw.sin(), 0.0, -state.yaw.cos());
        let (golden_gain, golden_quality) =
            match golden_angle_metrics(&self.config, planar_velocity, view_forward) {
                Some(metrics) => (metrics.gain, metrics.quality),
                None => (1.0, 0.0),
            };
        let base_intent_speed = move_intent.mag * max_speed;
        let move_intent_speed = if state.grounded {
            base_intent_speed
        } else {
            base_intent_speed * golden_gain
        };
        // TODO: Consider edge-triggered jump buffering instead of level-triggered.
        if self.config.jump_buffer_enabled && input.jump {
            self.jump_buffer_time = self.config.jump_buffer_window;
        } else if self.jump_buffer_time > 0.0 {
            self.jump_buffer_time = (self.jump_buffer_time - dt).max(0.0);
        }

        let just_landed = !self.was_grounded && state.grounded;
        if just_landed && (input.jump || self.jump_buffer_time > 0.0) {
            self.bhop_grace_time = self.config.frictionless_jump_grace;
        }
        if self.bhop_grace_time > 0.0 {
            self.bhop_grace_time = (self.bhop_grace_time - dt).max(0.0);
        }

        let mut velocity = state.velocity;
        let mut planar = Vector::new(velocity.x, 0.0, velocity.z);
        if state.grounded {
            let friction_scale = self.friction_scale(input, golden_quality);
            let friction = self.config.friction * friction_scale;
            planar = apply_friction(planar, friction, self.config.stop_speed, dt);
        }

        if move_intent_speed > 0.0 {
            let accel = if state.grounded {
                self.config.ground_accel
            } else {
                self.config.air_accel * golden_gain
            };
            planar = accelerate(planar, move_intent.dir, move_intent_speed, accel, dt);
            if !state.grounded && golden_gain > 1.0 {
                let bonus_scale = self.config.golden_angle_bonus_scale.max(0.0);
                let bonus = (golden_gain - 1.0)
                    * bonus_scale
                    * self.config.air_accel
                    * dt
                    * base_intent_speed;
                planar += move_intent.dir * bonus;
            }
        }

        if !state.grounded {
            planar = apply_corridor_shaping(planar, move_intent, &self.config, dt);
        }

        if !state.grounded && self.config.air_resistance > 0.0 {
            let speed_scale = self.config.air_resistance_speed_scale.max(1.0e-3);
            let resistance_scale = (planar.norm() / speed_scale).clamp(0.0, 1.0);
            let effective = self.config.air_resistance * resistance_scale;
            let scale = (1.0 - effective * dt).max(0.0);
            planar *= scale;
        }

        let wants_jump = input.jump || self.jump_buffer_time > 0.0;
        let mut jumped = false;
        if state.grounded {
            if wants_jump {
                velocity.y = self.config.jump_speed;
                self.jump_buffer_time = 0.0;
                jumped = true;
            } else {
                velocity.y = 0.0;
            }
        }
        if !state.grounded && !jumped {
            velocity.y -= self.config.gravity * dt;
        }
        velocity.x = planar.x;
        velocity.z = planar.z;

        self.was_grounded = state.grounded;
        ArenaMotorOutput {
            desired_translation: velocity * dt,
            next_velocity: velocity,
            jumped,
        }
    }

    fn friction_scale(&self, input: ArenaMotorInput, golden_quality: Real) -> Real {
        match self.config.frictionless_jump_mode {
            FrictionlessJumpMode::None => 1.0,
            FrictionlessJumpMode::Soft => {
                if input.jump || self.jump_buffer_time > 0.0 || self.bhop_grace_time > 0.0 {
                    let base = self.config.frictionless_jump_friction_scale.clamp(0.0, 1.0);
                    if self.bhop_grace_time > 0.0 {
                        let best = self
                            .config
                            .frictionless_jump_friction_scale_best_angle
                            .clamp(0.0, 1.0);
                        lerp(base, best, golden_quality)
                    } else {
                        base
                    }
                } else {
                    1.0
                }
            }
            FrictionlessJumpMode::Hard => {
                if input.jump || self.jump_buffer_time > 0.0 || self.bhop_grace_time > 0.0 {
                    0.0
                } else {
                    1.0
                }
            }
        }
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

pub fn golden_angle_metrics(
    config: &ArenaMotorConfig,
    planar_velocity: Vector<Real>,
    reference_dir: Vector<Real>,
) -> Option<GoldenAngleMetrics> {
    let speed = planar_velocity.norm();
    if speed <= 0.0 || reference_dir.norm_squared() <= 0.0 {
        return None;
    }
    let vel_dir = planar_velocity / speed;
    let reference_dir = reference_dir.normalize();
    let dot = vel_dir.dot(&reference_dir).clamp(-1.0, 1.0);
    let theta = dot.acos();
    let target = config.golden_angle_target.max(1.0e-4);
    let ratio = (theta / target).clamp(0.0, 1.0);
    let quality = ratio * ratio * (3.0 - 2.0 * ratio);
    let g = lerp(
        config.golden_angle_gain_min,
        config.golden_angle_gain_peak,
        quality,
    );
    let blend = if config.golden_angle_blend_speed_end <= config.golden_angle_blend_speed_start {
        1.0
    } else {
        ((speed - config.golden_angle_blend_speed_start)
            / (config.golden_angle_blend_speed_end - config.golden_angle_blend_speed_start))
            .clamp(0.0, 1.0)
    };
    let gain = lerp(1.0, g, blend);
    Some(GoldenAngleMetrics {
        theta,
        gain,
        quality,
    })
}

fn apply_corridor_shaping(
    velocity: Vector<Real>,
    intent: MoveIntent,
    config: &ArenaMotorConfig,
    dt: Real,
) -> Vector<Real> {
    if config.corridor_shaping_strength <= 0.0 || intent.mag <= 0.0 {
        return velocity;
    }
    let speed = velocity.norm();
    if speed <= config.corridor_shaping_min_speed {
        return velocity;
    }
    let mut intent_dir = Vector::new(intent.dir.x, 0.0, intent.dir.z);
    if intent_dir.norm_squared() <= 0.0 {
        return velocity;
    }
    intent_dir = intent_dir.normalize();
    let vel_dir = velocity / speed;
    let min_alignment = config.corridor_shaping_min_alignment.clamp(-1.0, 1.0);
    let dot = vel_dir.dot(&intent_dir).clamp(-1.0, 1.0);
    if dot < min_alignment {
        return velocity;
    }
    let angle = dot.acos();
    if angle <= 1.0e-6 {
        return velocity;
    }
    let mut max_angle = config.corridor_shaping_strength.max(0.0) * dt;
    let cap = config.corridor_shaping_max_angle_per_tick;
    if cap > 0.0 {
        max_angle = max_angle.min(cap);
    }
    if max_angle <= 0.0 {
        return velocity;
    }
    if angle <= max_angle {
        return intent_dir * speed;
    }
    let t = (max_angle / angle).clamp(0.0, 1.0);
    let blended = vel_dir * (1.0 - t) + intent_dir * t;
    if blended.norm_squared() <= 1.0e-6 {
        return velocity;
    }
    blended.normalize() * speed
}

fn accelerate(
    velocity: Vector<Real>,
    wish_dir: Vector<Real>,
    wish_speed: Real,
    accel: Real,
    dt: Real,
) -> Vector<Real> {
    if wish_speed <= 0.0 {
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
    let speed = (velocity.x * velocity.x + velocity.z * velocity.z).sqrt();
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
    Vector::new(velocity.x * scale, velocity.y, velocity.z * scale)
}

fn lerp(a: Real, b: Real, t: Real) -> Real {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_buffer_applies_on_landing() {
        let mut motor = ArenaMotor::new(ArenaMotorConfig::default());
        let mut state = ArenaMotorState {
            velocity: Vector::zeros(),
            grounded: false,
            ground_normal: None,
            yaw: 0.0,
        };

        let input = ArenaMotorInput {
            move_axis: [0.0, 0.0],
            jump: true,
        };
        let output = motor.step(input, state, 0.05);
        assert!(!output.jumped);

        state.grounded = true;
        let output = motor.step(
            ArenaMotorInput {
                move_axis: [0.0, 0.0],
                jump: false,
            },
            state,
            0.05,
        );
        assert!(output.jumped);
        assert!(output.next_velocity.y > 0.0);
    }

    #[test]
    fn soft_bhop_reduces_friction() {
        let config = ArenaMotorConfig {
            friction: 10.0,
            frictionless_jump_friction_scale: 0.2,
            ..Default::default()
        };
        let mut motor = ArenaMotor::new(config);
        let state = ArenaMotorState {
            velocity: Vector::new(2.0, 0.0, 0.0),
            grounded: true,
            ground_normal: None,
            yaw: 0.0,
        };

        let output_no_jump = motor.step(
            ArenaMotorInput {
                move_axis: [0.0, 0.0],
                jump: false,
            },
            state,
            0.1,
        );
        let output_jump = motor.step(
            ArenaMotorInput {
                move_axis: [0.0, 0.0],
                jump: true,
            },
            state,
            0.1,
        );

        assert!(output_jump.next_velocity.x.abs() > output_no_jump.next_velocity.x.abs());
    }
}
