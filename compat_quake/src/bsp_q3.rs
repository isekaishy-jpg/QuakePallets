use std::fmt;

// Minimal id Tech 3 BSP (IBSP v46) parsing for collision cooking.

const LUMP_COUNT: usize = 17;
const Q3_BSP_SUPPORTED_VERSIONS: [u32; 2] = [46, 47];

#[derive(Debug)]
pub enum BspError {
    InvalidHeader,
    Truncated,
    UnsupportedVersion(u32),
    LumpOutOfBounds {
        lump: LumpType,
    },
    InvalidLumpSize {
        lump: LumpType,
        size: u32,
        stride: u32,
    },
    LumpTooLarge {
        lump: LumpType,
        count: usize,
    },
}

impl fmt::Display for BspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BspError::InvalidHeader => write!(f, "invalid q3 bsp header"),
            BspError::Truncated => write!(f, "q3 bsp data is truncated"),
            BspError::UnsupportedVersion(version) => {
                write!(f, "unsupported q3 bsp version {}", version)
            }
            BspError::LumpOutOfBounds { lump } => {
                write!(f, "q3 bsp lump out of bounds: {}", lump.name())
            }
            BspError::InvalidLumpSize { lump, size, stride } => write!(
                f,
                "q3 bsp lump has invalid size: {} (size {}, stride {})",
                lump.name(),
                size,
                stride
            ),
            BspError::LumpTooLarge { lump, count } => {
                write!(
                    f,
                    "q3 bsp lump is too large: {} (count {})",
                    lump.name(),
                    count
                )
            }
        }
    }
}

impl std::error::Error for BspError {}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BspHeader {
    pub version: u32,
    pub lumps: [Lump; LUMP_COUNT],
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Lump {
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LumpType {
    Entities = 0,
    Shaders = 1,
    Planes = 2,
    Nodes = 3,
    Leafs = 4,
    LeafFaces = 5,
    LeafBrushes = 6,
    Models = 7,
    Brushes = 8,
    BrushSides = 9,
    Vertices = 10,
    MeshVerts = 11,
    Effects = 12,
    Faces = 13,
    Lightmaps = 14,
    LightVols = 15,
    VisData = 16,
}

impl LumpType {
    fn from_index(index: usize) -> Self {
        match index {
            0 => LumpType::Entities,
            1 => LumpType::Shaders,
            2 => LumpType::Planes,
            3 => LumpType::Nodes,
            4 => LumpType::Leafs,
            5 => LumpType::LeafFaces,
            6 => LumpType::LeafBrushes,
            7 => LumpType::Models,
            8 => LumpType::Brushes,
            9 => LumpType::BrushSides,
            10 => LumpType::Vertices,
            11 => LumpType::MeshVerts,
            12 => LumpType::Effects,
            13 => LumpType::Faces,
            14 => LumpType::Lightmaps,
            15 => LumpType::LightVols,
            16 => LumpType::VisData,
            _ => LumpType::Entities,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LumpType::Entities => "entities",
            LumpType::Shaders => "shaders",
            LumpType::Planes => "planes",
            LumpType::Nodes => "nodes",
            LumpType::Leafs => "leafs",
            LumpType::LeafFaces => "leaf_faces",
            LumpType::LeafBrushes => "leaf_brushes",
            LumpType::Models => "models",
            LumpType::Brushes => "brushes",
            LumpType::BrushSides => "brush_sides",
            LumpType::Vertices => "vertices",
            LumpType::MeshVerts => "meshverts",
            LumpType::Effects => "effects",
            LumpType::Faces => "faces",
            LumpType::Lightmaps => "lightmaps",
            LumpType::LightVols => "lightvols",
            LumpType::VisData => "visdata",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Face {
    pub face_type: i32,
    pub vertex_start: i32,
    pub vertex_count: i32,
    pub meshvert_start: i32,
    pub meshvert_count: i32,
}

#[derive(Debug, Clone)]
pub struct Bsp {
    pub header: BspHeader,
    pub vertices: Vec<[f32; 3]>,
    pub meshverts: Vec<i32>,
    pub faces: Vec<Face>,
}

pub fn parse_bsp(data: &[u8]) -> Result<Bsp, BspError> {
    let header = parse_header(data)?;

    let vertices = parse_vertices(data, &header.lumps)?;
    let meshverts = parse_meshverts(data, &header.lumps)?;
    let faces = parse_faces(data, &header.lumps)?;

    Ok(Bsp {
        header,
        vertices,
        meshverts,
        faces,
    })
}

fn parse_header(data: &[u8]) -> Result<BspHeader, BspError> {
    if data.len() < 8 {
        return Err(BspError::Truncated);
    }
    if &data[0..4] != b"IBSP" {
        return Err(BspError::InvalidHeader);
    }
    let version = read_u32_le(&data[4..8]);
    if !Q3_BSP_SUPPORTED_VERSIONS.contains(&version) {
        return Err(BspError::UnsupportedVersion(version));
    }

    let header_len = 8 + LUMP_COUNT * 8;
    if data.len() < header_len {
        return Err(BspError::Truncated);
    }

    let mut lumps = [Lump {
        offset: 0,
        length: 0,
    }; LUMP_COUNT];
    for (i, lump) in lumps.iter_mut().enumerate() {
        let base = 8 + i * 8;
        let offset = read_u32_le(&data[base..base + 4]);
        let length = read_u32_le(&data[base + 4..base + 8]);
        let end = offset
            .checked_add(length)
            .ok_or(BspError::LumpOutOfBounds {
                lump: LumpType::from_index(i),
            })?;
        if end as usize > data.len() {
            return Err(BspError::LumpOutOfBounds {
                lump: LumpType::from_index(i),
            });
        }
        *lump = Lump { offset, length };
    }

    Ok(BspHeader { version, lumps })
}

fn parse_vertices(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<[f32; 3]>, BspError> {
    const MAX_LUMP_ELEMENTS: usize = 2_000_000;
    const VERTEX_STRIDE: usize = 44;

    let lump = lumps[LumpType::Vertices as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(VERTEX_STRIDE as u32) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::Vertices,
            size: lump.length,
            stride: VERTEX_STRIDE as u32,
        });
    }
    let slice = lump_slice(data, lump);
    let count = slice.len() / VERTEX_STRIDE;
    if count > MAX_LUMP_ELEMENTS {
        return Err(BspError::LumpTooLarge {
            lump: LumpType::Vertices,
            count,
        });
    }
    let mut vertices = Vec::with_capacity(count);
    for chunk in slice.chunks_exact(VERTEX_STRIDE) {
        vertices.push([
            read_f32_le(&chunk[0..4]),
            read_f32_le(&chunk[4..8]),
            read_f32_le(&chunk[8..12]),
        ]);
    }
    Ok(vertices)
}

fn parse_meshverts(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<i32>, BspError> {
    const MAX_LUMP_ELEMENTS: usize = 4_000_000;

    let lump = lumps[LumpType::MeshVerts as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(4) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::MeshVerts,
            size: lump.length,
            stride: 4,
        });
    }
    let slice = lump_slice(data, lump);
    let count = slice.len() / 4;
    if count > MAX_LUMP_ELEMENTS {
        return Err(BspError::LumpTooLarge {
            lump: LumpType::MeshVerts,
            count,
        });
    }
    let mut meshverts = Vec::with_capacity(count);
    for chunk in slice.chunks_exact(4) {
        meshverts.push(read_i32_le(&chunk[0..4]));
    }
    Ok(meshverts)
}

fn parse_faces(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<Face>, BspError> {
    const MAX_LUMP_ELEMENTS: usize = 1_000_000;
    const FACE_STRIDE: usize = 104;

    let lump = lumps[LumpType::Faces as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(FACE_STRIDE as u32) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::Faces,
            size: lump.length,
            stride: FACE_STRIDE as u32,
        });
    }
    let slice = lump_slice(data, lump);
    let count = slice.len() / FACE_STRIDE;
    if count > MAX_LUMP_ELEMENTS {
        return Err(BspError::LumpTooLarge {
            lump: LumpType::Faces,
            count,
        });
    }
    let mut faces = Vec::with_capacity(count);
    for chunk in slice.chunks_exact(FACE_STRIDE) {
        let face_type = read_i32_le(&chunk[8..12]);
        let vertex_start = read_i32_le(&chunk[12..16]);
        let vertex_count = read_i32_le(&chunk[16..20]);
        let meshvert_start = read_i32_le(&chunk[20..24]);
        let meshvert_count = read_i32_le(&chunk[24..28]);
        faces.push(Face {
            face_type,
            vertex_start,
            vertex_count,
            meshvert_start,
            meshvert_count,
        });
    }
    Ok(faces)
}

fn lump_slice(data: &[u8], lump: Lump) -> &[u8] {
    let start = lump.offset as usize;
    let end = start + lump.length as usize;
    &data[start..end]
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_i32_le(bytes: &[u8]) -> i32 {
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_f32_le(bytes: &[u8]) -> f32 {
    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_header() {
        let mut data = vec![0u8; 8 + LUMP_COUNT * 8];
        data[0..4].copy_from_slice(b"IBSP");
        data[4..8].copy_from_slice(&Q3_BSP_SUPPORTED_VERSIONS[0].to_le_bytes());
        let bsp = parse_bsp(&data).expect("parse ok");
        assert_eq!(bsp.header.version, Q3_BSP_SUPPORTED_VERSIONS[0]);
        assert!(bsp.vertices.is_empty());
        assert!(bsp.faces.is_empty());
        assert!(bsp.meshverts.is_empty());
    }

    #[test]
    fn parse_unsupported_version() {
        let mut data = vec![0u8; 8 + LUMP_COUNT * 8];
        data[0..4].copy_from_slice(b"IBSP");
        data[4..8].copy_from_slice(&45u32.to_le_bytes());
        let err = parse_bsp(&data).expect_err("should fail");
        assert!(matches!(err, BspError::UnsupportedVersion(45)));
    }
}
