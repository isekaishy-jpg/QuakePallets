use std::fmt;

#[derive(Debug)]
pub enum BspError {
    InvalidHeader,
    Truncated,
    UnsupportedVersion(u32),
}

impl fmt::Display for BspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BspError::InvalidHeader => write!(f, "invalid bsp header"),
            BspError::Truncated => write!(f, "bsp data is truncated"),
            BspError::UnsupportedVersion(version) => {
                write!(f, "unsupported bsp version {}", version)
            }
        }
    }
}

impl std::error::Error for BspError {}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BspHeader {
    pub version: u32,
}

pub fn parse_bsp(data: &[u8]) -> Result<BspHeader, BspError> {
    if data.len() < 8 {
        return Err(BspError::Truncated);
    }
    if &data[0..4] != b"IBSP" {
        return Err(BspError::InvalidHeader);
    }

    let version = read_u32_le(&data[4..8]);
    if version != 29 {
        return Err(BspError::UnsupportedVersion(version));
    }

    Ok(BspHeader { version })
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
