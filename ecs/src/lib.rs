#![forbid(unsafe_code)]

use std::hash::{Hash, Hasher};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::{Schedule, ScheduleLabel};

#[derive(Component, Copy, Clone, Debug, Default, PartialEq)]
pub struct Transform {
    pub position: Vec3,
}

#[derive(Component, Copy, Clone, Debug, Default, PartialEq)]
pub struct Velocity {
    pub linear: Vec3,
}

#[derive(Component, Copy, Clone, Debug, Default, PartialEq)]
pub struct Camera {
    pub fov_y_degrees: f32,
}

#[derive(Component, Copy, Clone, Debug, Default, PartialEq)]
pub struct PlayerTag;

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
}

impl std::ops::Add for Vec3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl std::ops::AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
        self.z += rhs.z;
    }
}

impl std::ops::Mul<f32> for Vec3 {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
        }
    }
}

#[derive(Resource, Copy, Clone, Debug)]
pub struct FixedTimeStep {
    pub dt_seconds: f32,
}

impl Default for FixedTimeStep {
    fn default() -> Self {
        Self {
            dt_seconds: 1.0 / 60.0,
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct InputCommand {
    pub move_axis: Vec3,
}

#[derive(Resource, Debug, Default)]
pub struct InputStream {
    commands: Vec<InputCommand>,
    cursor: usize,
}

impl InputStream {
    pub fn new(commands: Vec<InputCommand>) -> Self {
        Self {
            commands,
            cursor: 0,
        }
    }

    fn next(&mut self) -> Option<InputCommand> {
        let command = self.commands.get(self.cursor).copied();
        if command.is_some() {
            self.cursor += 1;
        }
        command
    }
}

#[derive(Resource, Copy, Clone, Debug, Default)]
pub struct CurrentInput(pub InputCommand);

#[derive(ScheduleLabel, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct FixedUpdate;

#[derive(ScheduleLabel, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Update;

pub struct EcsSchedules {
    fixed: Schedule,
    update: Schedule,
}

impl EcsSchedules {
    pub fn new() -> Self {
        let mut fixed = Schedule::new(FixedUpdate);
        fixed.add_systems((apply_input, integrate_velocity));
        let update = Schedule::new(Update);
        Self { fixed, update }
    }

    pub fn run_fixed(&mut self, world: &mut World) {
        self.fixed.run(world);
    }

    pub fn run_update(&mut self, world: &mut World) {
        self.update.run(world);
    }
}

impl Default for EcsSchedules {
    fn default() -> Self {
        Self::new()
    }
}

pub fn new_world() -> World {
    let mut world = World::new();
    world.insert_resource(FixedTimeStep::default());
    world.insert_resource(CurrentInput::default());
    world.insert_resource(InputStream::default());
    world
}

fn apply_input(
    mut velocities: Query<&mut Velocity, With<PlayerTag>>,
    mut input_stream: ResMut<InputStream>,
    mut current_input: ResMut<CurrentInput>,
) {
    if let Some(command) = input_stream.next() {
        current_input.0 = command;
    }
    for mut velocity in &mut velocities {
        velocity.linear = current_input.0.move_axis;
    }
}

fn integrate_velocity(mut query: Query<(&mut Transform, &Velocity)>, time: Res<FixedTimeStep>) {
    for (mut transform, velocity) in &mut query {
        transform.position += velocity.linear * time.dt_seconds;
    }
}

pub fn hash_entity_state(world: &World, entity: Entity) -> Option<u64> {
    let transform = world.get::<Transform>(entity)?;
    let velocity = world.get::<Velocity>(entity)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_vec3(&transform.position, &mut hasher);
    hash_vec3(&velocity.linear, &mut hasher);
    Some(hasher.finish())
}

fn hash_vec3(vec: &Vec3, hasher: &mut impl Hasher) {
    vec.x.to_bits().hash(hasher);
    vec.y.to_bits().hash(hasher);
    vec.z.to_bits().hash(hasher);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_produces_identical_hash() {
        let inputs = vec![
            InputCommand {
                move_axis: Vec3 {
                    x: 1.0,
                    y: 0.0,
                    z: 0.0,
                },
            },
            InputCommand {
                move_axis: Vec3 {
                    x: 0.0,
                    y: 1.0,
                    z: 0.0,
                },
            },
            InputCommand {
                move_axis: Vec3 {
                    x: -1.0,
                    y: 0.0,
                    z: 0.0,
                },
            },
        ];

        let hash_a = run_replay(inputs.clone(), 120);
        let hash_b = run_replay(inputs, 120);

        assert_eq!(hash_a, hash_b);
    }

    fn run_replay(inputs: Vec<InputCommand>, ticks: u32) -> u64 {
        let mut world = new_world();
        world.insert_resource(InputStream::new(inputs));

        let entity = world
            .spawn((Transform::default(), Velocity::default(), PlayerTag))
            .id();

        let mut schedules = EcsSchedules::new();
        for _ in 0..ticks {
            schedules.run_fixed(&mut world);
        }

        hash_entity_state(&world, entity).unwrap_or_default()
    }
}
