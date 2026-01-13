//! Player controller composition (input + motor + collision + camera).
#![forbid(unsafe_code)]

use character_collision::{CharacterCollision, CollisionMoveResult, CollisionProfile};
use physics_rapier::PhysicsWorld;
use player_camera::{CameraPose, PlayerCamera};
use rapier3d::math::{Isometry, Vector};
use rapier3d::prelude::Real;

#[derive(Clone, Copy, Debug, Default)]
pub struct RawInput {
    pub move_x: Real,
    pub move_y: Real,
    pub jump: bool,
    pub look_delta: [Real; 2],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InputIntent {
    pub move_axis: [Real; 2],
    pub jump: bool,
    pub look_delta: [Real; 2],
}

pub trait InputAdapter {
    fn intent(&mut self, raw: RawInput) -> InputIntent;
}

#[derive(Default)]
pub struct DirectInputAdapter;

impl DirectInputAdapter {
    fn normalize_axis(axis: [Real; 2]) -> [Real; 2] {
        let len = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
        if len > 1.0 {
            [axis[0] / len, axis[1] / len]
        } else {
            axis
        }
    }
}

impl InputAdapter for DirectInputAdapter {
    fn intent(&mut self, raw: RawInput) -> InputIntent {
        let move_axis = Self::normalize_axis([raw.move_x, raw.move_y]);
        InputIntent {
            move_axis,
            jump: raw.jump,
            look_delta: raw.look_delta,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlayerKinematics {
    pub position: Isometry<Real>,
    pub velocity: Vector<Real>,
    pub grounded: bool,
    pub ground_normal: Option<Vector<Real>>,
}

impl PlayerKinematics {
    pub fn new(position: Isometry<Real>) -> Self {
        Self {
            position,
            velocity: Vector::zeros(),
            grounded: false,
            ground_normal: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MotorContext {
    pub dt: Real,
    pub yaw: Real,
}

#[derive(Clone, Copy, Debug)]
pub struct MotorOutput {
    pub desired_translation: Vector<Real>,
    pub next_velocity: Vector<Real>,
}

pub trait Motor {
    fn step(
        &mut self,
        input: &InputIntent,
        state: &PlayerKinematics,
        ctx: MotorContext,
    ) -> MotorOutput;
}

#[derive(Clone, Copy, Debug)]
pub struct SimpleMotor {
    pub move_speed: Real,
}

impl Default for SimpleMotor {
    fn default() -> Self {
        Self { move_speed: 4.0 }
    }
}

impl Motor for SimpleMotor {
    fn step(
        &mut self,
        input: &InputIntent,
        _state: &PlayerKinematics,
        ctx: MotorContext,
    ) -> MotorOutput {
        let yaw = ctx.yaw;
        let forward = Vector::new(yaw.sin(), 0.0, -yaw.cos());
        let right = Vector::new(yaw.cos(), 0.0, yaw.sin());
        let wish = right * input.move_axis[0] + forward * input.move_axis[1];
        let wish = if wish.norm_squared() > 0.0 {
            wish.normalize()
        } else {
            wish
        };
        let next_velocity = wish * self.move_speed;
        let desired_translation = next_velocity * ctx.dt;
        MotorOutput {
            desired_translation,
            next_velocity,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlayerFrame {
    pub kinematics: PlayerKinematics,
    pub collision: CollisionMoveResult,
    pub camera: CameraPose,
}

pub struct PlayerController<A: InputAdapter, M: Motor> {
    input: A,
    motor: M,
    collision: CharacterCollision,
    camera: PlayerCamera,
    state: PlayerKinematics,
}

impl<A: InputAdapter, M: Motor> PlayerController<A, M> {
    pub fn new(
        input: A,
        motor: M,
        profile: CollisionProfile,
        camera: PlayerCamera,
        position: Isometry<Real>,
    ) -> Self {
        Self {
            input,
            motor,
            collision: CharacterCollision::new(profile),
            camera,
            state: PlayerKinematics::new(position),
        }
    }

    pub fn state(&self) -> &PlayerKinematics {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut PlayerKinematics {
        &mut self.state
    }

    pub fn collision(&self) -> &CharacterCollision {
        &self.collision
    }

    pub fn collision_mut(&mut self) -> &mut CharacterCollision {
        &mut self.collision
    }

    pub fn motor(&self) -> &M {
        &self.motor
    }

    pub fn motor_mut(&mut self) -> &mut M {
        &mut self.motor
    }

    pub fn camera(&self) -> &PlayerCamera {
        &self.camera
    }

    pub fn camera_mut(&mut self) -> &mut PlayerCamera {
        &mut self.camera
    }

    pub fn tick(&mut self, world: &PhysicsWorld, raw: RawInput, dt: Real) -> PlayerFrame {
        let intent = self.input.intent(raw);
        self.camera.apply_look_delta(intent.look_delta);
        let motor_output = self.motor.step(
            &intent,
            &self.state,
            MotorContext {
                dt,
                yaw: self.camera.yaw(),
            },
        );
        let allow_step = self.state.grounded && !intent.jump;
        let collision = self.collision.move_character(
            world,
            self.state.position,
            motor_output.desired_translation,
            allow_step,
            dt,
        );
        self.state.position = collision.position;
        let mut next_velocity = motor_output.next_velocity;
        if dt > 0.0 && collision.hit_wall {
            next_velocity.x = collision.translation.x / dt;
            next_velocity.z = collision.translation.z / dt;
            if let Some(wall_normal) = collision.wall_normal {
                let dot = next_velocity.dot(&wall_normal);
                if dot < 0.0 {
                    next_velocity -= wall_normal * dot;
                }
            }
        }
        self.state.velocity = next_velocity;
        self.state.grounded = collision.grounded;
        self.state.ground_normal = collision.ground_normal;
        let camera = self
            .camera
            .update_from_origin(self.state.position.translation.vector);
        PlayerFrame {
            kinematics: self.state.clone(),
            collision,
            camera,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapier3d::prelude::ColliderBuilder;

    fn build_scene(world: &mut PhysicsWorld) {
        let floor = ColliderBuilder::cuboid(8.0, 0.1, 8.0)
            .translation(Vector::new(0.0, -0.1, 0.0))
            .build();
        world.insert_static_collider(floor);
        for i in 0..3 {
            let height = 0.15 * (i as f32 + 1.0);
            let step = ColliderBuilder::cuboid(0.35, height * 0.5, 0.6)
                .translation(Vector::new(1.0 + i as f32 * 0.8, height * 0.5, 0.0))
                .build();
            world.insert_static_collider(step);
        }
        let slope = ColliderBuilder::cuboid(1.2, 0.1, 1.2)
            .rotation(Vector::new(0.4, 0.0, 0.0))
            .translation(Vector::new(0.0, 0.4, -2.0))
            .build();
        world.insert_static_collider(slope);
    }

    #[test]
    fn controller_moves_over_steps() {
        let mut world = PhysicsWorld::new(Vector::new(0.0, -9.81, 0.0));
        build_scene(&mut world);
        world.step(1.0 / 60.0);

        let input = DirectInputAdapter;
        let motor = SimpleMotor { move_speed: 4.0 };
        let mut profile = CollisionProfile::arena_default();
        profile.step_height = 0.6;
        profile.step_min_width = 0.2;
        profile.ground_snap_distance = 0.3;
        let camera = PlayerCamera::new(0.9);
        let position = Isometry::translation(0.0, 1.0, 0.0);
        let mut controller = PlayerController::new(input, motor, profile, camera, position);

        for _ in 0..120 {
            controller.tick(
                &world,
                RawInput {
                    move_x: 1.0,
                    ..Default::default()
                },
                1.0 / 60.0,
            );
            world.step(1.0 / 60.0);
        }

        assert!(controller.state().position.translation.x > 1.5);
        assert!(controller.state().position.translation.y > 0.9);
    }
}
