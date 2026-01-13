use collision_world::{Aabb, CollisionChunk};

#[derive(Clone, Copy, Debug)]
pub struct Triangle {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub c: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
pub struct Quadtree2dConfig {
    pub max_tris_per_leaf: usize,
    pub min_leaf_size_xy: f32,
    pub max_depth: u32,
}

impl Default for Quadtree2dConfig {
    fn default() -> Self {
        Self {
            max_tris_per_leaf: 5000,
            min_leaf_size_xy: 16.0,
            max_depth: 10,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QuadtreeBuild {
    pub chunks: Vec<CollisionChunk>,
    pub root_bounds: Aabb,
}

#[derive(Clone, Copy, Debug)]
struct TriangleRef {
    bounds: Aabb,
    centroid_x: f32,
    centroid_z: f32,
}

pub fn build_quadtree_chunks(
    triangles: &[Triangle],
    config: Quadtree2dConfig,
    label: &str,
) -> Result<QuadtreeBuild, String> {
    if triangles.is_empty() {
        return Err("quadtree requires at least one triangle".to_string());
    }
    if !config.min_leaf_size_xy.is_finite() || config.min_leaf_size_xy <= 0.0 {
        return Err("min_leaf_size_xy must be finite and > 0".to_string());
    }

    let refs = build_refs(triangles);
    let root_bounds = union_bounds(&refs).ok_or_else(|| "triangles have no bounds".to_string())?;
    let mut chunks = Vec::new();
    let mut chunk_index = 0usize;
    split_node(
        &refs,
        root_bounds,
        0,
        &config,
        label,
        &mut chunk_index,
        &mut chunks,
    );
    Ok(QuadtreeBuild {
        chunks,
        root_bounds,
    })
}

fn build_refs(triangles: &[Triangle]) -> Vec<TriangleRef> {
    triangles
        .iter()
        .map(|tri| {
            let bounds = triangle_bounds(tri);
            let centroid_x = (tri.a[0] + tri.b[0] + tri.c[0]) / 3.0;
            let centroid_z = (tri.a[2] + tri.b[2] + tri.c[2]) / 3.0;
            TriangleRef {
                bounds,
                centroid_x,
                centroid_z,
            }
        })
        .collect()
}

fn split_node(
    triangles: &[TriangleRef],
    bounds: Aabb,
    depth: u32,
    config: &Quadtree2dConfig,
    label: &str,
    chunk_index: &mut usize,
    chunks: &mut Vec<CollisionChunk>,
) {
    if triangles.is_empty() {
        return;
    }
    let size_x = bounds.max[0] - bounds.min[0];
    let size_z = bounds.max[2] - bounds.min[2];
    if triangles.len() <= config.max_tris_per_leaf
        || depth >= config.max_depth
        || size_x <= config.min_leaf_size_xy
        || size_z <= config.min_leaf_size_xy
    {
        emit_chunk(triangles, label, chunk_index, chunks);
        return;
    }

    let mid_x = (bounds.min[0] + bounds.max[0]) * 0.5;
    let mid_z = (bounds.min[2] + bounds.max[2]) * 0.5;
    let mut buckets: [Vec<TriangleRef>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for tri in triangles {
        let east = tri.centroid_x >= mid_x;
        let north = tri.centroid_z >= mid_z;
        let index = match (east, north) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        };
        buckets[index].push(*tri);
    }

    let child_bounds = [
        Aabb {
            min: [bounds.min[0], bounds.min[1], bounds.min[2]],
            max: [mid_x, bounds.max[1], mid_z],
        },
        Aabb {
            min: [mid_x, bounds.min[1], bounds.min[2]],
            max: [bounds.max[0], bounds.max[1], mid_z],
        },
        Aabb {
            min: [bounds.min[0], bounds.min[1], mid_z],
            max: [mid_x, bounds.max[1], bounds.max[2]],
        },
        Aabb {
            min: [mid_x, bounds.min[1], mid_z],
            max: [bounds.max[0], bounds.max[1], bounds.max[2]],
        },
    ];

    for (bucket, bounds) in buckets.into_iter().zip(child_bounds.into_iter()) {
        if bucket.is_empty() {
            continue;
        }
        split_node(
            &bucket,
            bounds,
            depth + 1,
            config,
            label,
            chunk_index,
            chunks,
        );
    }
}

fn emit_chunk(
    triangles: &[TriangleRef],
    label: &str,
    chunk_index: &mut usize,
    chunks: &mut Vec<CollisionChunk>,
) {
    let bounds = union_bounds(triangles).unwrap_or(Aabb {
        min: [0.0, 0.0, 0.0],
        max: [0.0, 0.0, 0.0],
    });
    let index = *chunk_index;
    *chunk_index = (*chunk_index).saturating_add(1);
    let chunk_id = format!("{}/chunk_{:04}", label, index);
    let triangle_count = u32::try_from(triangles.len()).unwrap_or(u32::MAX);
    chunks.push(CollisionChunk {
        chunk_id,
        aabb_min: bounds.min,
        aabb_max: bounds.max,
        payload_ref: format!("inline:{}/chunk_{:04}", label, index),
        triangle_count,
        partition_hint: None,
    });
}

fn triangle_bounds(tri: &Triangle) -> Aabb {
    let min_x = tri.a[0].min(tri.b[0]).min(tri.c[0]);
    let min_y = tri.a[1].min(tri.b[1]).min(tri.c[1]);
    let min_z = tri.a[2].min(tri.b[2]).min(tri.c[2]);
    let max_x = tri.a[0].max(tri.b[0]).max(tri.c[0]);
    let max_y = tri.a[1].max(tri.b[1]).max(tri.c[1]);
    let max_z = tri.a[2].max(tri.b[2]).max(tri.c[2]);
    Aabb {
        min: [min_x, min_y, min_z],
        max: [max_x, max_y, max_z],
    }
}

fn union_bounds(triangles: &[TriangleRef]) -> Option<Aabb> {
    let mut iter = triangles.iter();
    let first = iter.next()?.bounds;
    Some(iter.fold(first, |acc, tri| acc.union(&tri.bounds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadtree_build_is_deterministic() {
        let triangles = vec![
            Triangle {
                a: [0.0, 0.0, 0.0],
                b: [1.0, 0.0, 0.0],
                c: [0.0, 0.0, 1.0],
            },
            Triangle {
                a: [5.0, 0.0, 5.0],
                b: [6.0, 0.0, 5.0],
                c: [5.0, 0.0, 6.0],
            },
        ];
        let config = Quadtree2dConfig {
            max_tris_per_leaf: 1,
            min_leaf_size_xy: 0.5,
            max_depth: 4,
        };
        let build_a = build_quadtree_chunks(&triangles, config, "test").expect("build a");
        let build_b = build_quadtree_chunks(&triangles, config, "test").expect("build b");
        assert_eq!(build_a.chunks, build_b.chunks);
        assert_eq!(build_a.root_bounds, build_b.root_bounds);
    }
}
