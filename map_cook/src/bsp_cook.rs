use collision_world::{build_chunk_bounds_bvh, CollisionWorld, PartitionKind};
use compat_quake::bsp as quake1;
use compat_quake::bsp_q3 as quake3;

use crate::quadtree::{build_quadtree_chunks, Quadtree2dConfig, Triangle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BspKind {
    Quake1,
    Quake3,
}

#[derive(Clone, Debug)]
pub struct BspCookConfig {
    pub map_id: String,
    pub map_to_world_scale: f32,
    pub space_origin: [f32; 3],
    pub quadtree: Quadtree2dConfig,
}

pub fn build_bsp_collision_world(
    kind: BspKind,
    bytes: &[u8],
    config: &BspCookConfig,
) -> Result<CollisionWorld, String> {
    if config.map_id.trim().is_empty() {
        return Err("map_id must not be empty".to_string());
    }
    if !config.map_to_world_scale.is_finite() || config.map_to_world_scale <= 0.0 {
        return Err("map_to_world_scale must be finite and > 0".to_string());
    }
    if !vector_is_finite(config.space_origin) {
        return Err("space_origin must be finite".to_string());
    }

    let triangles = match kind {
        BspKind::Quake1 => {
            let bsp = quake1::parse_bsp(bytes).map_err(|err| err.to_string())?;
            triangles_from_quake1(&bsp, config.map_to_world_scale)?
        }
        BspKind::Quake3 => {
            let bsp = quake3::parse_bsp(bytes).map_err(|err| err.to_string())?;
            triangles_from_quake3(&bsp, config.map_to_world_scale)?
        }
    };
    let label = sanitize_label(&config.map_id);
    if label.is_empty() {
        return Err("map_id produced empty chunk label".to_string());
    }
    let build = build_quadtree_chunks(&triangles, config.quadtree, &label)?;
    let chunk_bounds_bvh = build_chunk_bounds_bvh(&build.chunks)?;

    Ok(CollisionWorld {
        version: 1,
        partition_kind: PartitionKind::Quadtree2d,
        space_origin: config.space_origin,
        root_bounds: build.root_bounds,
        map_to_world_scale: config.map_to_world_scale,
        chunks: build.chunks,
        chunk_bounds_bvh,
    })
}

fn triangles_from_quake1(bsp: &quake1::Bsp, scale: f32) -> Result<Vec<Triangle>, String> {
    let face_range = bsp.world_face_range().unwrap_or(0..bsp.faces.len());
    let mut triangles = Vec::new();
    for face_index in face_range {
        let face = bsp
            .faces
            .get(face_index)
            .ok_or_else(|| format!("face index out of bounds: {}", face_index))?;
        let num_edges = face.num_edges as usize;
        if num_edges < 3 {
            continue;
        }
        let first_edge = usize::try_from(face.first_edge)
            .map_err(|_| format!("face has negative first_edge: {}", face.first_edge))?;
        let end = first_edge
            .checked_add(num_edges)
            .ok_or_else(|| "face edge range overflow".to_string())?;
        if end > bsp.surfedges.len() {
            return Err(format!(
                "face edge range out of bounds: {}..{} (surfedges {})",
                first_edge,
                end,
                bsp.surfedges.len()
            ));
        }

        let mut polygon = Vec::with_capacity(num_edges);
        for &surfedge in &bsp.surfedges[first_edge..end] {
            let edge_index = if surfedge < 0 { -surfedge } else { surfedge } as usize;
            let reversed = surfedge < 0;
            let edge = bsp
                .edges
                .get(edge_index)
                .ok_or_else(|| format!("edge index out of bounds: {}", edge_index))?;
            let vertex_index = if reversed { edge[1] } else { edge[0] } as usize;
            let vertex = bsp
                .vertices
                .get(vertex_index)
                .ok_or_else(|| format!("vertex index out of bounds: {}", vertex_index))?;
            polygon.push(*vertex);
        }

        if polygon.len() < 3 {
            continue;
        }
        let v0 = quake_to_world(scale_vec(*polygon.first().unwrap(), scale));
        for i in 1..polygon.len() - 1 {
            let v1 = quake_to_world(scale_vec(polygon[i], scale));
            let v2 = quake_to_world(scale_vec(polygon[i + 1], scale));
            if triangle_is_degenerate(v0, v1, v2) {
                continue;
            }
            triangles.push(Triangle {
                a: v0,
                b: v1,
                c: v2,
            });
        }
    }
    if triangles.is_empty() {
        return Err("quake1 bsp contained no collision triangles".to_string());
    }
    Ok(triangles)
}

fn triangles_from_quake3(bsp: &quake3::Bsp, scale: f32) -> Result<Vec<Triangle>, String> {
    let mut triangles = Vec::new();
    for (face_index, face) in bsp.faces.iter().enumerate() {
        let vertex_start = usize::try_from(face.vertex_start)
            .map_err(|_| format!("face {} has negative vertex_start", face_index))?;
        let vertex_count = usize::try_from(face.vertex_count)
            .map_err(|_| format!("face {} has negative vertex_count", face_index))?;
        if vertex_count < 3 {
            continue;
        }
        if vertex_start + vertex_count > bsp.vertices.len() {
            return Err(format!("face {} vertex range out of bounds", face_index));
        }

        match face.face_type {
            1 => {
                let v0 = quake_to_world(scale_vec(bsp.vertices[vertex_start], scale));
                for i in 1..vertex_count - 1 {
                    let v1 = quake_to_world(scale_vec(bsp.vertices[vertex_start + i], scale));
                    let v2 = quake_to_world(scale_vec(bsp.vertices[vertex_start + i + 1], scale));
                    if triangle_is_degenerate(v0, v1, v2) {
                        continue;
                    }
                    triangles.push(Triangle {
                        a: v0,
                        b: v1,
                        c: v2,
                    });
                }
            }
            3 => {
                let meshvert_start = usize::try_from(face.meshvert_start)
                    .map_err(|_| format!("face {} has negative meshvert_start", face_index))?;
                let meshvert_count = usize::try_from(face.meshvert_count)
                    .map_err(|_| format!("face {} has negative meshvert_count", face_index))?;
                if meshvert_start + meshvert_count > bsp.meshverts.len() {
                    return Err(format!("face {} meshvert range out of bounds", face_index));
                }
                if meshvert_count % 3 != 0 {
                    return Err(format!(
                        "face {} meshvert_count {} is not divisible by 3",
                        face_index, meshvert_count
                    ));
                }
                for tri_index in 0..(meshvert_count / 3) {
                    let base = meshvert_start + tri_index * 3;
                    let idx0 = bsp.meshverts[base];
                    let idx1 = bsp.meshverts[base + 1];
                    let idx2 = bsp.meshverts[base + 2];
                    let v0 =
                        fetch_q3_vertex(bsp, vertex_start, vertex_count, idx0, face_index, scale)?;
                    let v1 =
                        fetch_q3_vertex(bsp, vertex_start, vertex_count, idx1, face_index, scale)?;
                    let v2 =
                        fetch_q3_vertex(bsp, vertex_start, vertex_count, idx2, face_index, scale)?;
                    if triangle_is_degenerate(v0, v1, v2) {
                        continue;
                    }
                    triangles.push(Triangle {
                        a: v0,
                        b: v1,
                        c: v2,
                    });
                }
            }
            _ => {
                continue;
            }
        }
    }
    if triangles.is_empty() {
        return Err("quake3 bsp contained no collision triangles".to_string());
    }
    Ok(triangles)
}

fn fetch_q3_vertex(
    bsp: &quake3::Bsp,
    vertex_start: usize,
    vertex_count: usize,
    meshvert: i32,
    face_index: usize,
    scale: f32,
) -> Result<[f32; 3], String> {
    let offset = usize::try_from(meshvert)
        .map_err(|_| format!("face {} has negative meshvert index", face_index))?;
    if offset >= vertex_count {
        return Err(format!(
            "face {} meshvert index {} out of face range {}",
            face_index, offset, vertex_count
        ));
    }
    let vertex = bsp.vertices[vertex_start + offset];
    Ok(quake_to_world(scale_vec(vertex, scale)))
}

fn scale_vec(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn quake_to_world(value: [f32; 3]) -> [f32; 3] {
    [value[0], value[2], -value[1]]
}

fn triangle_is_degenerate(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> bool {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let area2 = cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2];
    area2 <= 1.0e-10
}

fn sanitize_label(map_id: &str) -> String {
    map_id
        .trim()
        .replace(['\\', ':'], "/")
        .trim_matches('/')
        .to_string()
}

fn vector_is_finite(value: [f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}
