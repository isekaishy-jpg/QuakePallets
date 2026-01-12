//! Rapier KCC wrapper and collision profiles.
//!
//! Policy: collision/stepping must use Rapier KCC; do not reimplement step/slide logic.
//! Console naming uses lowercase snake_case; dev-only commands are prefixed with `dev_`.
#![forbid(unsafe_code)]

use physics_rapier::PhysicsWorld;
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::math::{Isometry, Point, Translation, UnitVector, Vector};
use rapier3d::prelude::{Capsule, QueryFilter, Ray, Real};

#[derive(Clone, Copy, Debug)]
pub struct CollisionProfile {
    /// Capsule radius in meters.
    pub capsule_radius: Real,
    /// Capsule cylinder height in meters (distance between sphere centers).
    pub capsule_height: Real,
    /// Maximum step height for auto-stepping in meters.
    pub step_height: Real,
    /// Minimum width of free space required after stepping.
    pub step_min_width: Real,
    /// Maximum climbable slope angle in radians.
    pub max_slope_angle: Real,
    /// Minimum slope angle where sliding begins (>= max_slope_angle).
    pub min_slope_slide_angle: Real,
    /// Distance to snap to ground in meters.
    pub ground_snap_distance: Real,
    /// Small separation to preserve between character and environment.
    pub offset: Real,
    /// Small nudge applied along contact normals to prevent sticking.
    pub normal_nudge_factor: Real,
    /// Wall slide damping factor (0 = none, 1 = full stop along wall).
    pub wall_slide_damping: Real,
    /// Minimum horizontal progress required to keep a step-up while hitting a wall.
    pub wall_step_min_forward: Real,
}

impl CollisionProfile {
    pub fn arena_default() -> Self {
        Self {
            capsule_radius: 0.4,
            capsule_height: 1.8,
            step_height: 0.45,
            step_min_width: 0.2,
            max_slope_angle: 45.0_f32.to_radians(),
            min_slope_slide_angle: 50.0_f32.to_radians(),
            ground_snap_distance: 0.2,
            offset: 0.02,
            normal_nudge_factor: 1.0e-4,
            wall_slide_damping: 0.2,
            wall_step_min_forward: 0.005,
        }
    }

    pub fn rpg_default() -> Self {
        Self {
            capsule_radius: 0.45,
            capsule_height: 1.7,
            step_height: 0.32,
            step_min_width: 0.15,
            max_slope_angle: 45.0_f32.to_radians(),
            min_slope_slide_angle: 45.0_f32.to_radians(),
            ground_snap_distance: 0.2,
            offset: 0.045,
            normal_nudge_factor: 1.0e-4,
            wall_slide_damping: 0.35,
            wall_step_min_forward: 0.005,
        }
    }

    fn capsule(&self) -> Capsule {
        Capsule::new_y(self.capsule_height * 0.5, self.capsule_radius)
    }

    fn apply_to(&self, controller: &mut KinematicCharacterController) {
        controller.autostep = if self.step_height > 0.0 {
            Some(CharacterAutostep {
                max_height: CharacterLength::Absolute(self.step_height),
                min_width: CharacterLength::Absolute(self.step_min_width),
                include_dynamic_bodies: false,
            })
        } else {
            None
        };
        controller.max_slope_climb_angle = self.max_slope_angle;
        controller.min_slope_slide_angle = self.min_slope_slide_angle.max(self.max_slope_angle);
        controller.snap_to_ground = if self.ground_snap_distance > 0.0 {
            Some(CharacterLength::Absolute(self.ground_snap_distance))
        } else {
            None
        };
        controller.offset = CharacterLength::Absolute(self.offset);
        controller.normal_nudge_factor = self.normal_nudge_factor;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CollisionMoveResult {
    pub position: Isometry<Real>,
    pub translation: Vector<Real>,
    pub grounded: bool,
    pub ground_normal: Option<Vector<Real>>,
    pub hit_wall: bool,
    pub wall_normal: Option<Vector<Real>>,
    pub hit_ceiling: bool,
    pub sliding: bool,
}

pub struct CharacterCollision {
    profile: CollisionProfile,
    controller: KinematicCharacterController,
    capsule: Capsule,
}

#[derive(Clone, Copy, Debug)]
struct GroundProbeHit {
    normal: Vector<Real>,
    sliding: bool,
}

impl CharacterCollision {
    fn world_up(world: &PhysicsWorld) -> Vector<Real> {
        if world.gravity.norm_squared() > 1.0e-6 {
            -world.gravity.normalize()
        } else {
            Vector::y()
        }
    }

    pub fn new(profile: CollisionProfile) -> Self {
        let capsule = profile.capsule();
        let mut controller = KinematicCharacterController::default();
        profile.apply_to(&mut controller);
        Self {
            profile,
            controller,
            capsule,
        }
    }

    pub fn profile(&self) -> CollisionProfile {
        self.profile
    }

    pub fn set_profile(&mut self, profile: CollisionProfile) {
        self.profile = profile;
        self.capsule = profile.capsule();
        profile.apply_to(&mut self.controller);
    }

    pub fn capsule(&self) -> &Capsule {
        &self.capsule
    }

    pub fn move_character(
        &mut self,
        world: &PhysicsWorld,
        position: Isometry<Real>,
        desired_translation: Vector<Real>,
        allow_step: bool,
        dt: Real,
    ) -> CollisionMoveResult {
        let mut hit_wall = false;
        let mut hit_ceiling = false;
        let mut contact_ground_normal = None;
        let mut best_ground_dot = -1.0;
        let mut support_normal = None;
        let mut best_support_dot = -1.0;
        let desired_dir = if desired_translation.norm_squared() > 1.0e-6 {
            Some(desired_translation.normalize())
        } else {
            None
        };
        let desired_horiz = Vector::new(desired_translation.x, 0.0, desired_translation.z);
        let desired_horiz_len = desired_horiz.norm();
        let mut wall_normal = None;
        let mut best_wall_dot = 1.0;
        let mut best_wall_oppose = -1.0;
        let moving_up = desired_translation.y > 0.0 && !allow_step;
        let up_vec = Self::world_up(world);
        let up = UnitVector::new_normalize(up_vec);
        self.controller.up = up;
        let wall_dot = self.controller.max_slope_climb_angle.cos();
        let original_autostep = self.controller.autostep;
        let original_snap = self.controller.snap_to_ground;
        if !allow_step {
            self.controller.autostep = None;
        }
        if desired_translation.y > 0.0 && !allow_step {
            self.controller.snap_to_ground = None;
        }

        let output = self.controller.move_shape(
            dt,
            world.bodies(),
            world.colliders(),
            world.query_pipeline(),
            &self.capsule,
            &position,
            desired_translation,
            QueryFilter::default(),
            |collision| {
                let normal = collision.hit.normal1;
                let up_dot = normal.dot(&up);
                if moving_up && up_dot < -0.1 {
                    hit_ceiling = true;
                } else {
                    if up_dot <= wall_dot {
                        hit_wall = true;
                        let oppose = desired_dir.map(|dir| -normal.dot(&dir)).unwrap_or(0.0);
                        let prefer = if oppose > 0.0 {
                            oppose > best_wall_oppose
                        } else if best_wall_oppose <= 0.0 {
                            up_dot < best_wall_dot
                        } else {
                            false
                        };
                        if prefer {
                            best_wall_oppose = oppose;
                            best_wall_dot = up_dot;
                            wall_normal = Some(normal.into_inner());
                        }
                    } else if up_dot > best_ground_dot {
                        best_ground_dot = up_dot;
                        contact_ground_normal = Some(normal.into_inner());
                    }
                    if up_dot > best_support_dot {
                        best_support_dot = up_dot;
                        support_normal = Some(normal.into_inner());
                    }
                }
            },
        );
        self.controller.autostep = original_autostep;
        self.controller.snap_to_ground = original_snap;
        let mut adjusted_translation = output.translation;
        let horiz = Vector::new(adjusted_translation.x, 0.0, adjusted_translation.z);
        let min_forward = self
            .profile
            .wall_step_min_forward
            .max(desired_horiz_len * 0.05)
            .max(0.0);
        let mut stepped = allow_step
            && hit_wall
            && output.grounded
            && output.translation.y > 0.0
            && output.translation.y <= self.profile.step_height + 1.0e-3
            && desired_translation.y <= 0.0
            && horiz.norm_squared() >= min_forward * min_forward;
        if stepped {
            let step_position = Translation::from(output.translation) * position;
            let step_support_distance = self.profile.step_height + self.profile.offset + 1.0e-3;
            if self
                .probe_ground_with_distance(world, step_position, step_support_distance)
                .is_none()
            {
                stepped = false;
            }
        }
        if hit_wall && !stepped {
            if let Some(normal) = wall_normal {
                let dot = adjusted_translation.dot(&normal);
                if dot < 0.0 {
                    adjusted_translation -= normal * dot;
                }
                let damping = self.profile.wall_slide_damping.clamp(0.0, 1.0);
                if damping > 0.0 {
                    let tangential =
                        adjusted_translation - normal * adjusted_translation.dot(&normal);
                    adjusted_translation -= tangential * damping;
                }
            }
        }
        if moving_up && !hit_ceiling && adjusted_translation.y < desired_translation.y {
            adjusted_translation.y = desired_translation.y;
        }
        let next_position = Translation::from(adjusted_translation) * position;
        let mut grounded = false;
        let mut ground_normal = None;
        let mut sliding = false;
        if !moving_up {
            if let Some(hit) = self.probe_ground(world, next_position) {
                grounded = true;
                ground_normal = Some(hit.normal);
                sliding = hit.sliding;
            }
        }
        if output.is_sliding_down_slope {
            if let Some(normal) = support_normal.or(contact_ground_normal) {
                let up_dot = normal.dot(&up);
                if up_dot > 0.05 {
                    grounded = true;
                    sliding = true;
                    ground_normal = Some(normal);
                }
            }
        }
        if !allow_step && desired_translation.y > 0.0 {
            grounded = false;
            ground_normal = None;
            sliding = false;
        }
        CollisionMoveResult {
            position: next_position,
            translation: adjusted_translation,
            grounded,
            ground_normal,
            hit_wall,
            wall_normal,
            hit_ceiling,
            sliding,
        }
    }

    fn probe_ground(
        &self,
        world: &PhysicsWorld,
        position: Isometry<Real>,
    ) -> Option<GroundProbeHit> {
        let snap_distance = self.profile.ground_snap_distance.max(0.0);
        if snap_distance <= 0.0 {
            return None;
        }
        self.probe_ground_with_distance(world, position, snap_distance)
    }

    fn probe_ground_with_distance(
        &self,
        world: &PhysicsWorld,
        position: Isometry<Real>,
        snap_distance: Real,
    ) -> Option<GroundProbeHit> {
        let up = Self::world_up(world);
        let direction = -up;
        // Use a smaller foot probe to stabilize grounding without wall bias.
        let foot_radius = self.profile.capsule_radius * 0.75;
        let foot_offset =
            -(self.profile.capsule_height * 0.5 + self.profile.capsule_radius) + foot_radius;
        let foot_center = position.translation.vector + up * foot_offset;
        let ray_origin = Point::from(foot_center);
        let ray = Ray::new(ray_origin, direction);
        let max_toi = foot_radius + snap_distance + self.profile.offset + 1.0e-3;
        let hit = world
            .query_pipeline()
            .cast_ray_and_get_normal(
                world.bodies(),
                world.colliders(),
                &ray,
                max_toi,
                true,
                QueryFilter::default(),
            )
            .or_else(|| {
                world.query_pipeline().cast_ray_and_get_normal(
                    world.bodies(),
                    world.colliders(),
                    &ray,
                    max_toi,
                    false,
                    QueryFilter::default(),
                )
            })?;
        let normal = hit.1.normal;
        let up_dot = normal.dot(&up);
        if up_dot <= 0.0 {
            return None;
        }
        if up_dot < self.controller.max_slope_climb_angle.cos() {
            return None;
        }
        let sliding = up_dot <= self.controller.min_slope_slide_angle.cos();
        Some(GroundProbeHit { normal, sliding })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapier3d::prelude::*;

    fn build_floor(world: &mut PhysicsWorld) {
        let floor = ColliderBuilder::cuboid(5.0, 0.1, 5.0)
            .translation(vector![0.0, -0.1, 0.0])
            .build();
        world.insert_static_collider(floor);
    }

    #[test]
    fn reports_ground_contact() {
        let mut world = PhysicsWorld::new(vector![0.0, -9.81, 0.0]);
        build_floor(&mut world);
        world.step(1.0 / 60.0);

        let collision = CharacterCollision::new(CollisionProfile::arena_default());
        let position = Isometry::translation(0.0, 1.5, 0.0);
        let mut collision = collision;
        let result =
            collision.move_character(&world, position, vector![0.0, -1.0, 0.0], true, 1.0 / 60.0);

        assert!(result.grounded);
        assert!(result.position.translation.y > 1.0);
    }
}
