//! Collision world asset format and validation.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

const COLLISION_WORLD_VERSION: u32 = 1;
const DEFAULT_MAX_TRIANGLES_PER_CHUNK: u32 = 250_000;
const DEFAULT_MAX_CHUNKS: usize = 10_000;
const DEFAULT_MAX_LEAF_CHUNKS: usize = 4;
const BOUNDS_EPS: f32 = 1.0e-4;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionWorld {
    pub version: u32,
    pub partition_kind: PartitionKind,
    pub space_origin: [f32; 3],
    pub root_bounds: Aabb,
    pub map_to_world_scale: f32,
    pub chunks: Vec<CollisionChunk>,
    pub chunk_bounds_bvh: ChunkBoundsBvh,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionKind {
    Quadtree2d,
    Octree3d,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionChunk {
    pub chunk_id: String,
    pub aabb_min: [f32; 3],
    pub aabb_max: [f32; 3],
    pub payload_ref: String,
    pub triangle_count: u32,
    #[serde(default)]
    pub partition_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChunkBoundsBvh {
    pub root: u32,
    pub nodes: Vec<ChunkBoundsBvhNode>,
    pub leaf_indices: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChunkBoundsBvhNode {
    pub bounds: Aabb,
    #[serde(default)]
    pub left: Option<u32>,
    #[serde(default)]
    pub right: Option<u32>,
    #[serde(default)]
    pub leaf: Option<ChunkLeafRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChunkLeafRange {
    pub first: u32,
    pub count: u32,
}

impl ChunkBoundsBvh {
    pub fn select_intersecting(&self, chunks: &[CollisionChunk], bounds: &Aabb) -> Vec<u32> {
        if self.nodes.is_empty() || chunks.is_empty() {
            return Vec::new();
        }
        let mut selected = Vec::new();
        let mut stack = vec![self.root];
        while let Some(node_index) = stack.pop() {
            let node = match self.nodes.get(node_index as usize) {
                Some(node) => node,
                None => continue,
            };
            if !node.bounds.intersects(bounds) {
                continue;
            }
            if let Some(leaf) = node.leaf {
                let start = leaf.first as usize;
                let end = start.saturating_add(leaf.count as usize);
                if end > self.leaf_indices.len() {
                    continue;
                }
                for &chunk_index in &self.leaf_indices[start..end] {
                    let chunk_index = chunk_index as usize;
                    let chunk = match chunks.get(chunk_index) {
                        Some(chunk) => chunk,
                        None => continue,
                    };
                    let chunk_bounds = Aabb {
                        min: chunk.aabb_min,
                        max: chunk.aabb_max,
                    };
                    if chunk_bounds.intersects(bounds) {
                        selected.push(chunk_index as u32);
                    }
                }
            } else {
                if let Some(left) = node.left {
                    stack.push(left);
                }
                if let Some(right) = node.right {
                    stack.push(right);
                }
            }
        }
        selected
    }
}

#[derive(Clone, Debug, Default)]
pub struct CollisionWorldValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl CollisionWorldValidation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CollisionWorldValidationConfig {
    pub max_triangles_per_chunk: u32,
    pub max_chunks: usize,
}

impl Default for CollisionWorldValidationConfig {
    fn default() -> Self {
        Self {
            max_triangles_per_chunk: DEFAULT_MAX_TRIANGLES_PER_CHUNK,
            max_chunks: DEFAULT_MAX_CHUNKS,
        }
    }
}

impl CollisionWorld {
    pub fn parse_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|err| err.to_string())
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|err| err.to_string())
    }

    pub fn validate(&self, config: CollisionWorldValidationConfig) -> CollisionWorldValidation {
        let mut validation = CollisionWorldValidation::default();

        if self.version != COLLISION_WORLD_VERSION {
            validation
                .errors
                .push(format!("unsupported version {}", self.version));
        }

        if !vector_is_finite(self.space_origin) {
            validation
                .errors
                .push("space_origin must be finite".to_string());
        }

        if !self.map_to_world_scale.is_finite() || self.map_to_world_scale <= 0.0 {
            validation
                .errors
                .push("map_to_world_scale must be finite and > 0".to_string());
        }

        if self.chunks.is_empty() {
            validation
                .warnings
                .push("collision world contains no chunks".to_string());
        }

        if self.chunks.len() > config.max_chunks {
            validation.warnings.push(format!(
                "chunk count {} exceeds budget {}",
                self.chunks.len(),
                config.max_chunks
            ));
        }

        if !aabb_is_valid(&self.root_bounds) {
            validation
                .errors
                .push("root_bounds must be finite with min <= max".to_string());
        }

        let mut union: Option<Aabb> = None;
        for chunk in &self.chunks {
            if chunk.chunk_id.trim().is_empty() {
                validation
                    .errors
                    .push("chunk_id must not be empty".to_string());
            }
            let bounds = chunk_bounds(chunk);
            if !aabb_is_valid(&bounds) {
                validation
                    .errors
                    .push(format!("chunk '{}' has invalid bounds", chunk.chunk_id));
            }
            if chunk.payload_ref.trim().is_empty() {
                validation.errors.push(format!(
                    "chunk '{}' payload_ref must not be empty",
                    chunk.chunk_id
                ));
            }
            if chunk.triangle_count == 0 {
                validation
                    .warnings
                    .push(format!("chunk '{}' has zero triangles", chunk.chunk_id));
            } else if chunk.triangle_count > config.max_triangles_per_chunk {
                validation.warnings.push(format!(
                    "chunk '{}' triangle_count {} exceeds budget {}",
                    chunk.chunk_id, chunk.triangle_count, config.max_triangles_per_chunk
                ));
            }
            union = Some(match union {
                Some(existing) => existing.union(&bounds),
                None => bounds,
            });
        }

        if let Some(expected) = union {
            if !self.root_bounds.contains(&expected, BOUNDS_EPS) {
                validation.errors.push(format!(
                    "root_bounds does not contain all chunk bounds (expected min={:?} max={:?})",
                    expected.min, expected.max
                ));
            }
            if !aabb_near_eq(&self.root_bounds, &expected, BOUNDS_EPS) {
                validation.warnings.push(format!(
                    "root_bounds differs from union of chunks (expected min={:?} max={:?})",
                    expected.min, expected.max
                ));
            }
        }

        validate_chunk_bounds_bvh(&self.chunk_bounds_bvh, &self.chunks, &mut validation);

        validation
    }
}

pub fn build_chunk_bounds_bvh(chunks: &[CollisionChunk]) -> Result<ChunkBoundsBvh, String> {
    if chunks.is_empty() {
        return Err("chunk_bounds_bvh requires at least one chunk".to_string());
    }

    let mut nodes = Vec::new();
    let mut leaf_indices = Vec::new();
    let indices: Vec<u32> = (0..chunks.len())
        .map(|index| u32::try_from(index).unwrap_or(u32::MAX))
        .collect();
    let root = build_bvh_node(
        chunks,
        &indices,
        &mut nodes,
        &mut leaf_indices,
        DEFAULT_MAX_LEAF_CHUNKS,
    );
    Ok(ChunkBoundsBvh {
        root,
        nodes,
        leaf_indices,
    })
}

fn build_bvh_node(
    chunks: &[CollisionChunk],
    indices: &[u32],
    nodes: &mut Vec<ChunkBoundsBvhNode>,
    leaf_indices: &mut Vec<u32>,
    max_leaf: usize,
) -> u32 {
    let bounds = union_bounds(chunks, indices);
    if indices.len() <= max_leaf {
        let first = leaf_indices.len();
        leaf_indices.extend(indices.iter().copied());
        let node = ChunkBoundsBvhNode {
            bounds,
            left: None,
            right: None,
            leaf: Some(ChunkLeafRange {
                first: u32::try_from(first).unwrap_or(u32::MAX),
                count: u32::try_from(indices.len()).unwrap_or(u32::MAX),
            }),
        };
        let node_index = nodes.len() as u32;
        nodes.push(node);
        return node_index;
    }

    let axis = longest_axis(bounds);
    let mut sorted: Vec<(u32, f32)> = indices
        .iter()
        .map(|index| {
            let center = chunk_bounds(&chunks[*index as usize]).center();
            (*index, center[axis])
        })
        .collect();
    sorted.sort_by(|a, b| {
        let ord = a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal);
        if ord == std::cmp::Ordering::Equal {
            a.0.cmp(&b.0)
        } else {
            ord
        }
    });
    let mid = sorted.len() / 2;
    let (left_indices, right_indices) = sorted.split_at(mid);
    let left_indices: Vec<u32> = left_indices.iter().map(|pair| pair.0).collect();
    let right_indices: Vec<u32> = right_indices.iter().map(|pair| pair.0).collect();

    let left = build_bvh_node(chunks, &left_indices, nodes, leaf_indices, max_leaf);
    let right = build_bvh_node(chunks, &right_indices, nodes, leaf_indices, max_leaf);

    let node = ChunkBoundsBvhNode {
        bounds,
        left: Some(left),
        right: Some(right),
        leaf: None,
    };
    let node_index = nodes.len() as u32;
    nodes.push(node);
    node_index
}

fn union_bounds(chunks: &[CollisionChunk], indices: &[u32]) -> Aabb {
    let mut iter = indices.iter();
    let first = iter
        .next()
        .map(|index| chunk_bounds(&chunks[*index as usize]))
        .unwrap_or_else(|| Aabb {
            min: [0.0, 0.0, 0.0],
            max: [0.0, 0.0, 0.0],
        });
    iter.fold(first, |acc, index| {
        acc.union(&chunk_bounds(&chunks[*index as usize]))
    })
}

fn longest_axis(bounds: Aabb) -> usize {
    let extent = [
        bounds.max[0] - bounds.min[0],
        bounds.max[1] - bounds.min[1],
        bounds.max[2] - bounds.min[2],
    ];
    if extent[0] >= extent[1] && extent[0] >= extent[2] {
        0
    } else if extent[1] >= extent[2] {
        1
    } else {
        2
    }
}

fn chunk_bounds(chunk: &CollisionChunk) -> Aabb {
    Aabb {
        min: chunk.aabb_min,
        max: chunk.aabb_max,
    }
}

impl Aabb {
    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }

    pub fn center(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    pub fn contains(&self, other: &Aabb, eps: f32) -> bool {
        for axis in 0..3 {
            if self.min[axis] - eps > other.min[axis] {
                return false;
            }
            if self.max[axis] + eps < other.max[axis] {
                return false;
            }
        }
        true
    }

    pub fn intersects(&self, other: &Aabb) -> bool {
        for axis in 0..3 {
            if self.max[axis] < other.min[axis] {
                return false;
            }
            if self.min[axis] > other.max[axis] {
                return false;
            }
        }
        true
    }
}

fn aabb_is_valid(bounds: &Aabb) -> bool {
    bounds.min.iter().all(|value| value.is_finite())
        && bounds.max.iter().all(|value| value.is_finite())
        && bounds.min[0] <= bounds.max[0]
        && bounds.min[1] <= bounds.max[1]
        && bounds.min[2] <= bounds.max[2]
}

fn aabb_near_eq(left: &Aabb, right: &Aabb, eps: f32) -> bool {
    for axis in 0..3 {
        if (left.min[axis] - right.min[axis]).abs() > eps {
            return false;
        }
        if (left.max[axis] - right.max[axis]).abs() > eps {
            return false;
        }
    }
    true
}

fn vector_is_finite(value: [f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}

fn validate_chunk_bounds_bvh(
    bvh: &ChunkBoundsBvh,
    chunks: &[CollisionChunk],
    validation: &mut CollisionWorldValidation,
) {
    if bvh.nodes.is_empty() {
        validation
            .errors
            .push("chunk_bounds_bvh must contain at least one node".to_string());
        return;
    }
    if bvh.root as usize >= bvh.nodes.len() {
        validation
            .errors
            .push("chunk_bounds_bvh root is out of range".to_string());
    }

    let mut coverage = vec![false; chunks.len()];
    for (node_index, node) in bvh.nodes.iter().enumerate() {
        if !aabb_is_valid(&node.bounds) {
            validation
                .errors
                .push(format!("bvh node {} has invalid bounds", node_index));
        }
        match (&node.left, &node.right, &node.leaf) {
            (Some(left), Some(right), None) => {
                if *left as usize >= bvh.nodes.len() || *right as usize >= bvh.nodes.len() {
                    validation
                        .errors
                        .push(format!("bvh node {} child index out of range", node_index));
                    continue;
                }
                let left_bounds = bvh.nodes[*left as usize].bounds;
                let right_bounds = bvh.nodes[*right as usize].bounds;
                let expected = left_bounds.union(&right_bounds);
                if !node.bounds.contains(&expected, BOUNDS_EPS) {
                    validation.errors.push(format!(
                        "bvh node {} bounds do not contain children",
                        node_index
                    ));
                }
            }
            (None, None, Some(leaf)) => {
                let start = leaf.first as usize;
                let count = leaf.count as usize;
                if start + count > bvh.leaf_indices.len() {
                    validation
                        .errors
                        .push(format!("bvh leaf {} range out of bounds", node_index));
                    continue;
                }
                let mut union: Option<Aabb> = None;
                for &chunk_index in &bvh.leaf_indices[start..start + count] {
                    let chunk_index = chunk_index as usize;
                    if chunk_index >= chunks.len() {
                        validation.errors.push(format!(
                            "bvh leaf {} references missing chunk index {}",
                            node_index, chunk_index
                        ));
                        continue;
                    }
                    if coverage.get(chunk_index) == Some(&true) {
                        validation.errors.push(format!(
                            "bvh chunk {} appears in multiple leaves",
                            chunk_index
                        ));
                    }
                    if let Some(slot) = coverage.get_mut(chunk_index) {
                        *slot = true;
                    }
                    let bounds = chunk_bounds(&chunks[chunk_index]);
                    union = Some(match union {
                        Some(existing) => existing.union(&bounds),
                        None => bounds,
                    });
                }
                if let Some(expected) = union {
                    if !node.bounds.contains(&expected, BOUNDS_EPS) {
                        validation.errors.push(format!(
                            "bvh leaf {} bounds do not contain chunks",
                            node_index
                        ));
                    }
                }
            }
            _ => {
                validation.errors.push(format!(
                    "bvh node {} must have either two children or a leaf range",
                    node_index
                ));
            }
        }
    }

    for (index, covered) in coverage.iter().enumerate() {
        if !covered {
            validation
                .errors
                .push(format!("chunk {} missing from bvh leaves", index));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(id: &str, min: [f32; 3], max: [f32; 3]) -> CollisionChunk {
        CollisionChunk {
            chunk_id: id.to_string(),
            aabb_min: min,
            aabb_max: max,
            payload_ref: format!("inline:{}", id),
            triangle_count: 12,
            partition_hint: None,
        }
    }

    #[test]
    fn bvh_is_deterministic() {
        let chunks = vec![
            make_chunk("a", [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            make_chunk("b", [2.0, 0.0, 0.0], [3.0, 1.0, 1.0]),
            make_chunk("c", [-2.0, 0.0, 0.0], [-1.0, 1.0, 1.0]),
        ];
        let bvh_a = build_chunk_bounds_bvh(&chunks).expect("bvh a");
        let bvh_b = build_chunk_bounds_bvh(&chunks).expect("bvh b");
        assert_eq!(bvh_a, bvh_b);
        assert!(!bvh_a.nodes.is_empty());
    }

    #[test]
    fn validation_catches_missing_chunks() {
        let chunks = vec![make_chunk("a", [0.0, 0.0, 0.0], [1.0, 1.0, 1.0])];
        let mut bvh = build_chunk_bounds_bvh(&chunks).expect("bvh");
        bvh.leaf_indices.clear();
        let world = CollisionWorld {
            version: 1,
            partition_kind: PartitionKind::Quadtree2d,
            space_origin: [0.0, 0.0, 0.0],
            root_bounds: Aabb {
                min: [0.0, 0.0, 0.0],
                max: [1.0, 1.0, 1.0],
            },
            map_to_world_scale: 1.0,
            chunks,
            chunk_bounds_bvh: bvh,
        };
        let validation = world.validate(CollisionWorldValidationConfig::default());
        assert!(!validation.is_ok());
    }

    #[test]
    fn bvh_selects_intersecting_chunks() {
        let chunks = vec![
            make_chunk("a", [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            make_chunk("b", [5.0, 0.0, 5.0], [6.0, 1.0, 6.0]),
        ];
        let bvh = build_chunk_bounds_bvh(&chunks).expect("bvh");
        let query = Aabb {
            min: [0.5, -1.0, 0.5],
            max: [1.5, 2.0, 1.5],
        };
        let selected = bvh.select_intersecting(&chunks, &query);
        assert_eq!(selected, vec![0]);
    }
}
