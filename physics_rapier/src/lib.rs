//! Rapier integration entrypoints and shared world setup.
#![forbid(unsafe_code)]

use rapier3d::prelude::*;

#[derive(Clone, Copy, Debug, Default)]
pub struct DebugDrawConfig {
    pub draw_colliders: bool,
    pub draw_character: bool,
    pub draw_contacts: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DebugLine {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub color: [f32; 4],
}

#[derive(Default)]
pub struct PhysicsDebugLines {
    pub lines: Vec<DebugLine>,
}

impl PhysicsDebugLines {
    fn push_line(&mut self, start: Point<Real>, end: Point<Real>, color: [f32; 4]) {
        self.lines.push(DebugLine {
            start: [start.x, start.y, start.z],
            end: [end.x, end.y, end.z],
            color,
        });
    }
}

impl rapier3d::pipeline::DebugRenderBackend for PhysicsDebugLines {
    fn draw_line(
        &mut self,
        object: rapier3d::pipeline::DebugRenderObject,
        a: Point<Real>,
        b: Point<Real>,
        _color: [f32; 4],
    ) {
        let color = match object {
            rapier3d::pipeline::DebugRenderObject::Collider(..)
            | rapier3d::pipeline::DebugRenderObject::ColliderAabb(..) => [0.2, 0.8, 0.9, 1.0],
            rapier3d::pipeline::DebugRenderObject::RigidBody(..) => [0.3, 0.7, 0.3, 1.0],
            rapier3d::pipeline::DebugRenderObject::ImpulseJoint(..)
            | rapier3d::pipeline::DebugRenderObject::MultibodyJoint(..) => [0.9, 0.7, 0.2, 1.0],
            rapier3d::pipeline::DebugRenderObject::ContactPair(..) => [0.9, 0.2, 0.2, 1.0],
        };
        self.push_line(a, b, color);
    }
}

pub struct PhysicsWorld {
    pub gravity: Vector<Real>,
    integration_parameters: IntegrationParameters,
    pipeline: PhysicsPipeline,
    island_manager: IslandManager,
    broad_phase: BroadPhaseMultiSap,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    query_pipeline: QueryPipeline,
    debug_pipeline: rapier3d::pipeline::DebugRenderPipeline,
}

impl PhysicsWorld {
    pub fn new(gravity: Vector<Real>) -> Self {
        Self {
            gravity,
            integration_parameters: IntegrationParameters::default(),
            pipeline: PhysicsPipeline::new(),
            island_manager: IslandManager::new(),
            broad_phase: BroadPhaseMultiSap::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            query_pipeline: QueryPipeline::new(),
            debug_pipeline: rapier3d::pipeline::DebugRenderPipeline::default(),
        }
    }

    pub fn bodies(&self) -> &RigidBodySet {
        &self.bodies
    }

    pub fn colliders(&self) -> &ColliderSet {
        &self.colliders
    }

    pub fn query_pipeline(&self) -> &QueryPipeline {
        &self.query_pipeline
    }

    pub fn step(&mut self, dt: Real) {
        self.integration_parameters.dt = dt;
        let physics_hooks = ();
        let event_handler = ();
        self.pipeline.step(
            &self.gravity,
            &self.integration_parameters,
            &mut self.island_manager,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            Some(&mut self.query_pipeline),
            &physics_hooks,
            &event_handler,
        );
        self.query_pipeline.update(&self.colliders);
    }

    pub fn insert_static_collider(&mut self, collider: Collider) -> ColliderHandle {
        self.colliders.insert(collider)
    }

    pub fn debug_lines(&mut self, config: DebugDrawConfig) -> PhysicsDebugLines {
        let mut lines = PhysicsDebugLines::default();
        let mut mode = rapier3d::pipeline::DebugRenderMode::empty();
        if config.draw_colliders {
            mode |= rapier3d::pipeline::DebugRenderMode::COLLIDER_SHAPES;
        }
        if config.draw_character {
            mode |= rapier3d::pipeline::DebugRenderMode::RIGID_BODY_AXES;
        }
        if config.draw_contacts {
            mode |= rapier3d::pipeline::DebugRenderMode::CONTACTS;
        }
        if !mode.is_empty() {
            self.debug_pipeline.mode = mode;
            self.debug_pipeline.render(
                &mut lines,
                &self.bodies,
                &self.colliders,
                &self.impulse_joints,
                &self.multibody_joints,
                &self.narrow_phase,
            );
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapier3d::control::KinematicCharacterController;

    fn build_floor(world: &mut PhysicsWorld) {
        let floor = ColliderBuilder::cuboid(5.0, 0.1, 5.0)
            .translation(vector![0.0, -0.1, 0.0])
            .build();
        world.insert_static_collider(floor);
    }

    #[test]
    fn kcc_detects_ground_contact() {
        let mut world = PhysicsWorld::new(vector![0.0, -9.81, 0.0]);
        build_floor(&mut world);
        world.step(1.0 / 60.0);

        let controller = KinematicCharacterController {
            snap_to_ground: Some(rapier3d::control::CharacterLength::Absolute(0.2)),
            ..Default::default()
        };
        let capsule = Capsule::new_y(0.9, 0.4);
        let mut position = Isometry::translation(0.0, 1.2, 0.0);
        let output = controller.move_shape(
            1.0 / 60.0,
            &world.bodies,
            &world.colliders,
            &world.query_pipeline,
            &capsule,
            &position,
            vector![0.0, -0.5, 0.0],
            QueryFilter::default(),
            |_| {},
        );
        position.translation.vector += output.translation;
        assert!(output.grounded);
        assert!(position.translation.y >= 0.9);
    }

    #[test]
    fn kcc_steps_over_stairs() {
        let mut world = PhysicsWorld::new(vector![0.0, -9.81, 0.0]);
        build_floor(&mut world);
        for i in 0..3 {
            let height = 0.2 * (i as f32 + 1.0);
            let step = ColliderBuilder::cuboid(0.3, height * 0.5, 0.5)
                .translation(vector![1.0 + i as f32 * 0.8, height * 0.5, 0.0])
                .build();
            world.insert_static_collider(step);
        }
        world.step(1.0 / 60.0);

        let controller = KinematicCharacterController {
            autostep: Some(rapier3d::control::CharacterAutostep {
                max_height: rapier3d::control::CharacterLength::Absolute(0.7),
                min_width: rapier3d::control::CharacterLength::Absolute(0.2),
                include_dynamic_bodies: false,
            }),
            snap_to_ground: Some(rapier3d::control::CharacterLength::Absolute(0.2)),
            ..Default::default()
        };
        let capsule = Capsule::new_y(0.9, 0.4);
        let mut position = Isometry::translation(0.0, 1.0, 0.0);
        for _ in 0..60 {
            let output = controller.move_shape(
                1.0 / 60.0,
                &world.bodies,
                &world.colliders,
                &world.query_pipeline,
                &capsule,
                &position,
                vector![0.05, 0.0, 0.0],
                QueryFilter::default(),
                |_| {},
            );
            position.translation.vector += output.translation;
            world.step(1.0 / 60.0);
        }
        assert!(position.translation.x > 2.0);
    }

    #[test]
    fn kcc_respects_slope_limit() {
        let mut world = PhysicsWorld::new(vector![0.0, -9.81, 0.0]);
        build_floor(&mut world);
        let slope = ColliderBuilder::cuboid(1.0, 0.1, 1.0)
            .rotation(vector![0.6, 0.0, 0.0])
            .translation(vector![0.0, 0.4, -1.2])
            .build();
        world.insert_static_collider(slope);
        world.step(1.0 / 60.0);

        let controller = KinematicCharacterController {
            max_slope_climb_angle: 0.3,
            snap_to_ground: Some(rapier3d::control::CharacterLength::Absolute(0.2)),
            ..Default::default()
        };
        let capsule = Capsule::new_y(0.9, 0.4);
        let mut position = Isometry::translation(0.0, 1.0, 0.0);
        let output = controller.move_shape(
            1.0 / 60.0,
            &world.bodies,
            &world.colliders,
            &world.query_pipeline,
            &capsule,
            &position,
            vector![0.0, 0.0, -1.0],
            QueryFilter::default(),
            |_| {},
        );
        position.translation.vector += output.translation;
        assert!(position.translation.z > -0.9);
    }
}
