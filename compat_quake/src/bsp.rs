use std::fmt;

const LUMP_COUNT: usize = 15;

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
}

impl fmt::Display for BspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BspError::InvalidHeader => write!(f, "invalid bsp header"),
            BspError::Truncated => write!(f, "bsp data is truncated"),
            BspError::UnsupportedVersion(version) => {
                write!(f, "unsupported bsp version {}", version)
            }
            BspError::LumpOutOfBounds { lump } => {
                write!(f, "bsp lump out of bounds: {}", lump.name())
            }
            BspError::InvalidLumpSize { lump, size, stride } => write!(
                f,
                "bsp lump has invalid size: {} (size {}, stride {})",
                lump.name(),
                size,
                stride
            ),
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
    Planes = 1,
    Textures = 2,
    Vertices = 3,
    Visibility = 4,
    Nodes = 5,
    TexInfo = 6,
    Faces = 7,
    Lighting = 8,
    ClipNodes = 9,
    Leaves = 10,
    MarkSurfaces = 11,
    Edges = 12,
    SurfEdges = 13,
    Models = 14,
}

impl LumpType {
    fn from_index(index: usize) -> Self {
        match index {
            0 => LumpType::Entities,
            1 => LumpType::Planes,
            2 => LumpType::Textures,
            3 => LumpType::Vertices,
            4 => LumpType::Visibility,
            5 => LumpType::Nodes,
            6 => LumpType::TexInfo,
            7 => LumpType::Faces,
            8 => LumpType::Lighting,
            9 => LumpType::ClipNodes,
            10 => LumpType::Leaves,
            11 => LumpType::MarkSurfaces,
            12 => LumpType::Edges,
            13 => LumpType::SurfEdges,
            14 => LumpType::Models,
            _ => LumpType::Entities,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LumpType::Entities => "entities",
            LumpType::Planes => "planes",
            LumpType::Textures => "textures",
            LumpType::Vertices => "vertices",
            LumpType::Visibility => "visibility",
            LumpType::Nodes => "nodes",
            LumpType::TexInfo => "texinfo",
            LumpType::Faces => "faces",
            LumpType::Lighting => "lighting",
            LumpType::ClipNodes => "clipnodes",
            LumpType::Leaves => "leaves",
            LumpType::MarkSurfaces => "marksurfaces",
            LumpType::Edges => "edges",
            LumpType::SurfEdges => "surfedges",
            LumpType::Models => "models",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Face {
    pub plane_id: u16,
    pub side: u16,
    pub first_edge: i32,
    pub num_edges: u16,
    pub texinfo: u16,
    pub styles: [u8; 4],
    pub light_offset: i32,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub first_face: i32,
    pub num_faces: i32,
}

#[derive(Debug, Clone)]
pub struct Bsp {
    pub header: BspHeader,
    pub vertices: Vec<[f32; 3]>,
    pub edges: Vec<[u16; 2]>,
    pub surfedges: Vec<i32>,
    pub faces: Vec<Face>,
    pub models: Vec<Model>,
}

impl Bsp {
    pub fn world_face_range(&self) -> Option<std::ops::Range<usize>> {
        let model = self.models.first()?;
        let first = usize::try_from(model.first_face).ok()?;
        let count = usize::try_from(model.num_faces).ok()?;
        Some(first..first.saturating_add(count))
    }
}

pub fn parse_bsp(data: &[u8]) -> Result<Bsp, BspError> {
    let header = parse_header(data)?;

    let vertices = parse_vertices(data, &header.lumps)?;
    let edges = parse_edges(data, &header.lumps)?;
    let surfedges = parse_surfedges(data, &header.lumps)?;
    let faces = parse_faces(data, &header.lumps)?;
    let models = parse_models(data, &header.lumps)?;

    Ok(Bsp {
        header,
        vertices,
        edges,
        surfedges,
        faces,
        models,
    })
}

fn parse_header(data: &[u8]) -> Result<BspHeader, BspError> {
    if data.len() < 4 {
        return Err(BspError::Truncated);
    }

    let (version, lump_start) = if data.len() >= 8 && &data[0..4] == b"IBSP" {
        (read_u32_le(&data[4..8]), 8)
    } else {
        (read_u32_le(&data[0..4]), 4)
    };

    if version != 29 {
        return Err(BspError::UnsupportedVersion(version));
    }

    let header_len = lump_start + LUMP_COUNT * 8;
    if data.len() < header_len {
        return Err(BspError::Truncated);
    }

    let mut lumps = [Lump {
        offset: 0,
        length: 0,
    }; LUMP_COUNT];
    for (i, lump) in lumps.iter_mut().enumerate() {
        let base = lump_start + i * 8;
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
    let lump = lumps[LumpType::Vertices as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(12) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::Vertices,
            size: lump.length,
            stride: 12,
        });
    }

    let slice = lump_slice(data, lump);
    let mut vertices = Vec::with_capacity(slice.len() / 12);
    for chunk in slice.chunks_exact(12) {
        vertices.push([
            read_f32_le(&chunk[0..4]),
            read_f32_le(&chunk[4..8]),
            read_f32_le(&chunk[8..12]),
        ]);
    }
    Ok(vertices)
}

fn parse_edges(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<[u16; 2]>, BspError> {
    let lump = lumps[LumpType::Edges as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(4) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::Edges,
            size: lump.length,
            stride: 4,
        });
    }

    let slice = lump_slice(data, lump);
    let mut edges = Vec::with_capacity(slice.len() / 4);
    for chunk in slice.chunks_exact(4) {
        edges.push([read_u16_le(&chunk[0..2]), read_u16_le(&chunk[2..4])]);
    }
    Ok(edges)
}

fn parse_surfedges(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<i32>, BspError> {
    let lump = lumps[LumpType::SurfEdges as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(4) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::SurfEdges,
            size: lump.length,
            stride: 4,
        });
    }

    let slice = lump_slice(data, lump);
    let mut surfedges = Vec::with_capacity(slice.len() / 4);
    for chunk in slice.chunks_exact(4) {
        surfedges.push(read_i32_le(&chunk[0..4]));
    }
    Ok(surfedges)
}

fn parse_faces(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<Face>, BspError> {
    let lump = lumps[LumpType::Faces as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(20) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::Faces,
            size: lump.length,
            stride: 20,
        });
    }

    let slice = lump_slice(data, lump);
    let mut faces = Vec::with_capacity(slice.len() / 20);
    for chunk in slice.chunks_exact(20) {
        let plane_id = read_u16_le(&chunk[0..2]);
        let side = read_u16_le(&chunk[2..4]);
        let first_edge = read_i32_le(&chunk[4..8]);
        let num_edges = read_u16_le(&chunk[8..10]);
        let texinfo = read_u16_le(&chunk[10..12]);
        let styles = [chunk[12], chunk[13], chunk[14], chunk[15]];
        let light_offset = read_i32_le(&chunk[16..20]);
        faces.push(Face {
            plane_id,
            side,
            first_edge,
            num_edges,
            texinfo,
            styles,
            light_offset,
        });
    }
    Ok(faces)
}

fn parse_models(data: &[u8], lumps: &[Lump; LUMP_COUNT]) -> Result<Vec<Model>, BspError> {
    let lump = lumps[LumpType::Models as usize];
    if lump.length == 0 {
        return Ok(Vec::new());
    }
    if !lump.length.is_multiple_of(64) {
        return Err(BspError::InvalidLumpSize {
            lump: LumpType::Models,
            size: lump.length,
            stride: 64,
        });
    }

    let slice = lump_slice(data, lump);
    let mut models = Vec::with_capacity(slice.len() / 64);
    for chunk in slice.chunks_exact(64) {
        let first_face = read_i32_le(&chunk[56..60]);
        let num_faces = read_i32_le(&chunk[60..64]);
        models.push(Model {
            first_face,
            num_faces,
        });
    }
    Ok(models)
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

fn read_u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
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
        data[4..8].copy_from_slice(&29u32.to_le_bytes());

        let bsp = parse_bsp(&data).expect("parse ok");
        assert_eq!(bsp.header.version, 29);
        assert!(bsp.vertices.is_empty());
        assert!(bsp.edges.is_empty());
        assert!(bsp.surfedges.is_empty());
        assert!(bsp.faces.is_empty());
        assert!(bsp.models.is_empty());
    }

    #[test]
    fn parse_quake1_header() {
        let mut data = vec![0u8; 4 + LUMP_COUNT * 8];
        data[0..4].copy_from_slice(&29u32.to_le_bytes());
        let bsp = parse_bsp(&data).expect("parse ok");
        assert_eq!(bsp.header.version, 29);
    }

    #[test]
    fn parse_unsupported_version() {
        let mut data = vec![0u8; 8 + LUMP_COUNT * 8];
        data[0..4].copy_from_slice(b"IBSP");
        data[4..8].copy_from_slice(&28u32.to_le_bytes());
        let err = parse_bsp(&data).expect_err("should fail");
        assert!(matches!(err, BspError::UnsupportedVersion(28)));
    }
}
