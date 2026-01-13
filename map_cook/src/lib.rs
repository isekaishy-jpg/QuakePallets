//! Map cooking for collision assets (Q1/Q3 triangle soup to chunked colliders).
#![forbid(unsafe_code)]

mod bsp_cook;
mod quadtree;
mod sidecar;

use collision_world::{
    build_chunk_bounds_bvh, Aabb, CollisionChunk, CollisionWorld, PartitionKind,
};
use rapier3d::na::UnitQuaternion;
use rapier3d::prelude::{Collider, ColliderBuilder, Isometry, Point, Real, Translation, Vector};
use test_map::{ResolvedSolid, SolidKind, TestMap};

pub use bsp_cook::{build_bsp_collision_world, BspCookConfig, BspKind};
pub use quadtree::Quadtree2dConfig;
pub use sidecar::{MapSidecar, MapSidecarValidation, MarkerSpec, SpawnSpec};

#[derive(Clone, Debug)]
pub struct TestMapCollider {
    pub id: String,
    pub tags: Vec<String>,
    pub collider: Collider,
}

#[derive(Clone, Debug)]
pub struct TestMapColliderSet {
    pub colliders: Vec<TestMapCollider>,
}

pub fn build_test_map_collision_world(map: &TestMap) -> Result<CollisionWorld, String> {
    let solids = map.expanded_solids()?;
    let scale = map.map_to_world_scale.unwrap_or(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err("test map scale must be finite and > 0".to_string());
    }
    let space_origin = map.space_origin.unwrap_or([0.0, 0.0, 0.0]);
    if !vector_is_finite(space_origin) {
        return Err("test map space_origin must be finite".to_string());
    }

    let mut chunks = Vec::new();
    for solid in solids {
        let scaled = scale_solid(&solid, scale);
        let bounds = solid_bounds(&scaled);
        let triangle_count = solid_triangle_count(&scaled);
        chunks.push(CollisionChunk {
            chunk_id: scaled.id.clone(),
            aabb_min: bounds.min,
            aabb_max: bounds.max,
            payload_ref: format!("inline:test_map/{}", scaled.id),
            triangle_count,
            partition_hint: None,
        });
    }

    let root_bounds = union_chunk_bounds(&chunks)
        .ok_or_else(|| "test map collision world requires at least one chunk".to_string())?;
    let chunk_bounds_bvh = build_chunk_bounds_bvh(&chunks)?;

    Ok(CollisionWorld {
        version: 1,
        partition_kind: PartitionKind::Quadtree2d,
        space_origin,
        root_bounds,
        map_to_world_scale: scale,
        chunks,
        chunk_bounds_bvh,
    })
}

pub fn build_test_map_colliders(map: &TestMap) -> Result<TestMapColliderSet, String> {
    let solids = map.expanded_solids()?;
    let scale = map.map_to_world_scale.unwrap_or(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err("test map scale must be finite and > 0".to_string());
    }
    let mut colliders = Vec::new();
    for solid in solids {
        let scaled = ResolvedSolid {
            id: solid.id,
            kind: solid.kind,
            pos: [
                solid.pos[0] * scale,
                solid.pos[1] * scale,
                solid.pos[2] * scale,
            ],
            size: [
                solid.size[0] * scale,
                solid.size[1] * scale,
                solid.size[2] * scale,
            ],
            yaw_deg: solid.yaw_deg,
            rot_euler_deg: solid.rot_euler_deg,
            tags: solid.tags,
        };
        let collider = build_collider(&scaled)?;
        colliders.push(TestMapCollider {
            id: scaled.id.clone(),
            tags: scaled.tags.clone(),
            collider,
        });
    }
    Ok(TestMapColliderSet { colliders })
}

fn build_collider(solid: &ResolvedSolid) -> Result<Collider, String> {
    let size = solid.size;
    let half = [size[0] * 0.5, size[1] * 0.5, size[2] * 0.5];
    let translation = Translation::from(Vector::new(solid.pos[0], solid.pos[1], solid.pos[2]));
    let rotation = solid_rotation(solid.yaw_deg, solid.rot_euler_deg);
    let iso = Isometry::from_parts(translation, rotation);

    let collider = match solid.kind {
        SolidKind::Box | SolidKind::BoxRot => ColliderBuilder::cuboid(half[0], half[1], half[2])
            .position(iso)
            .build(),
        SolidKind::Cylinder => {
            let radius = half[0].max(half[2]);
            ColliderBuilder::cylinder(half[1], radius)
                .position(iso)
                .build()
        }
        SolidKind::Ramp => {
            let (vertices, indices) = ramp_mesh(size[0], size[1], size[2]);
            ColliderBuilder::trimesh(vertices, indices)
                .position(iso)
                .build()
        }
    };
    Ok(collider)
}

fn scale_solid(solid: &ResolvedSolid, scale: f32) -> ResolvedSolid {
    ResolvedSolid {
        id: solid.id.clone(),
        kind: solid.kind,
        pos: [
            solid.pos[0] * scale,
            solid.pos[1] * scale,
            solid.pos[2] * scale,
        ],
        size: [
            solid.size[0] * scale,
            solid.size[1] * scale,
            solid.size[2] * scale,
        ],
        yaw_deg: solid.yaw_deg,
        rot_euler_deg: solid.rot_euler_deg,
        tags: solid.tags.clone(),
    }
}

fn solid_bounds(solid: &ResolvedSolid) -> Aabb {
    let half = [
        solid.size[0] * 0.5,
        solid.size[1] * 0.5,
        solid.size[2] * 0.5,
    ];
    let rotation = solid_rotation(solid.yaw_deg, solid.rot_euler_deg);
    let center = Vector::new(solid.pos[0], solid.pos[1], solid.pos[2]);
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for corner in corners(half) {
        let rotated = rotation.transform_vector(&corner);
        let world = rotated + center;
        min[0] = min[0].min(world.x);
        min[1] = min[1].min(world.y);
        min[2] = min[2].min(world.z);
        max[0] = max[0].max(world.x);
        max[1] = max[1].max(world.y);
        max[2] = max[2].max(world.z);
    }
    Aabb { min, max }
}

fn corners(half: [f32; 3]) -> [Vector<Real>; 8] {
    let (hx, hy, hz) = (half[0], half[1], half[2]);
    [
        Vector::new(-hx, -hy, -hz),
        Vector::new(hx, -hy, -hz),
        Vector::new(-hx, hy, -hz),
        Vector::new(hx, hy, -hz),
        Vector::new(-hx, -hy, hz),
        Vector::new(hx, -hy, hz),
        Vector::new(-hx, hy, hz),
        Vector::new(hx, hy, hz),
    ]
}

fn solid_triangle_count(solid: &ResolvedSolid) -> u32 {
    match solid.kind {
        SolidKind::Box | SolidKind::BoxRot => 12,
        SolidKind::Ramp => 8,
        SolidKind::Cylinder => 64,
    }
}

fn union_chunk_bounds(chunks: &[CollisionChunk]) -> Option<Aabb> {
    let mut iter = chunks.iter();
    let first = iter.next()?;
    let mut bounds = Aabb {
        min: first.aabb_min,
        max: first.aabb_max,
    };
    for chunk in iter {
        bounds = bounds.union(&Aabb {
            min: chunk.aabb_min,
            max: chunk.aabb_max,
        });
    }
    Some(bounds)
}

fn solid_rotation(yaw_deg: Option<f32>, rot_euler_deg: Option<[f32; 3]>) -> UnitQuaternion<Real> {
    if let Some(euler) = rot_euler_deg {
        let pitch = euler[0].to_radians();
        let yaw = euler[1].to_radians();
        let roll = euler[2].to_radians();
        UnitQuaternion::from_euler_angles(pitch, yaw, roll)
    } else if let Some(yaw) = yaw_deg {
        UnitQuaternion::from_euler_angles(0.0, yaw.to_radians(), 0.0)
    } else {
        UnitQuaternion::identity()
    }
}

fn ramp_mesh(length: f32, height: f32, width: f32) -> (Vec<Point<Real>>, Vec<[u32; 3]>) {
    let half_length = length * 0.5;
    let half_height = height * 0.5;
    let half_width = width * 0.5;
    let verts = vec![
        Point::new(-half_length, -half_height, -half_width),
        Point::new(half_length, -half_height, -half_width),
        Point::new(half_length, half_height, -half_width),
        Point::new(-half_length, -half_height, half_width),
        Point::new(half_length, -half_height, half_width),
        Point::new(half_length, half_height, half_width),
    ];
    let indices = vec![
        [0, 1, 2],
        [3, 5, 4],
        [0, 3, 4],
        [0, 4, 1],
        [0, 2, 5],
        [0, 5, 3],
        [1, 4, 5],
        [1, 5, 2],
    ];
    (verts, indices)
}

fn vector_is_finite(value: [f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn load_and_build_colliders() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root")
            .to_path_buf();
        let map_path = repo_root
            .join("content")
            .join("test_maps")
            .join("flat_friction_lane.toml");
        let text = std::fs::read_to_string(&map_path).expect("read test map");
        let map = TestMap::parse_toml(&text).expect("parse map");
        let colliders = build_test_map_colliders(&map).expect("build colliders");
        assert!(!colliders.colliders.is_empty());
    }

    #[test]
    fn build_collision_world_for_test_map() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root")
            .to_path_buf();
        let map_path = repo_root
            .join("content")
            .join("test_maps")
            .join("flat_friction_lane.toml");
        let text = std::fs::read_to_string(&map_path).expect("read test map");
        let map = TestMap::parse_toml(&text).expect("parse map");
        let world = build_test_map_collision_world(&map).expect("build collision world");
        assert!(!world.chunks.is_empty());
    }
}
