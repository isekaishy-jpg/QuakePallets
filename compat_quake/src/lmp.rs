use std::fmt;

pub const PALETTE_LEN: usize = 256 * 3;

#[derive(Debug)]
pub enum LmpError {
    TooShort,
    InvalidDimensions { width: u32, height: u32 },
    PixelCountOverflow,
    DataTooShort { expected: usize, actual: usize },
    PaletteTooShort { expected: usize, actual: usize },
}

impl fmt::Display for LmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LmpError::TooShort => write!(f, "lmp data too short for header"),
            LmpError::InvalidDimensions { width, height } => {
                write!(f, "invalid lmp dimensions: {}x{}", width, height)
            }
            LmpError::PixelCountOverflow => write!(f, "lmp pixel count overflow"),
            LmpError::DataTooShort { expected, actual } => write!(
                f,
                "lmp data too short: expected {} bytes, got {}",
                expected, actual
            ),
            LmpError::PaletteTooShort { expected, actual } => write!(
                f,
                "palette data too short: expected {} bytes, got {}",
                expected, actual
            ),
        }
    }
}

impl std::error::Error for LmpError {}

#[derive(Debug, Clone)]
pub struct Palette {
    colors: [u8; PALETTE_LEN],
}

impl Palette {
    pub fn colors(&self) -> &[u8; PALETTE_LEN] {
        &self.colors
    }

    pub fn rgba(&self, index: u8) -> [u8; 4] {
        let base = index as usize * 3;
        [
            self.colors[base],
            self.colors[base + 1],
            self.colors[base + 2],
            255,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct IndexedImage {
    pub width: u32,
    pub height: u32,
    pub indices: Vec<u8>,
}

impl IndexedImage {
    pub fn to_rgba8(&self, palette: &Palette) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.indices.len() * 4);
        for &index in &self.indices {
            let rgba = palette.rgba(index);
            out.extend_from_slice(&rgba);
        }
        out
    }
}

pub fn parse_palette(data: &[u8]) -> Result<Palette, LmpError> {
    if data.len() < PALETTE_LEN {
        return Err(LmpError::PaletteTooShort {
            expected: PALETTE_LEN,
            actual: data.len(),
        });
    }
    let mut colors = [0u8; PALETTE_LEN];
    colors.copy_from_slice(&data[..PALETTE_LEN]);
    Ok(Palette { colors })
}

pub fn parse_lmp_image(data: &[u8]) -> Result<IndexedImage, LmpError> {
    if data.len() < 8 {
        return Err(LmpError::TooShort);
    }
    let width = read_u32_le(&data[0..4]);
    let height = read_u32_le(&data[4..8]);
    if width == 0 || height == 0 {
        return Err(LmpError::InvalidDimensions { width, height });
    }

    let pixel_count = width
        .checked_mul(height)
        .ok_or(LmpError::PixelCountOverflow)?;
    let pixel_count = usize::try_from(pixel_count).map_err(|_| LmpError::PixelCountOverflow)?;
    let expected_len = 8usize
        .checked_add(pixel_count)
        .ok_or(LmpError::PixelCountOverflow)?;

    if data.len() < expected_len {
        return Err(LmpError::DataTooShort {
            expected: expected_len,
            actual: data.len(),
        });
    }

    let indices = data[8..8 + pixel_count].to_vec();
    Ok(IndexedImage {
        width,
        height,
        indices,
    })
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_palette_exact() {
        let mut data = vec![0u8; PALETTE_LEN];
        data[0] = 12;
        data[1] = 34;
        data[2] = 56;
        let palette = parse_palette(&data).expect("palette parse");
        assert_eq!(palette.colors()[0], 12);
        assert_eq!(palette.colors()[1], 34);
        assert_eq!(palette.colors()[2], 56);
        assert_eq!(palette.rgba(0), [12, 34, 56, 255]);
    }

    #[test]
    fn parse_palette_too_short() {
        let data = vec![0u8; PALETTE_LEN - 1];
        let err = parse_palette(&data).expect_err("palette should fail");
        assert!(matches!(err, LmpError::PaletteTooShort { .. }));
    }

    #[test]
    fn parse_lmp_image_ok() {
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&[0, 1, 2, 3]);
        let image = parse_lmp_image(&data).expect("image parse");
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 2);
        assert_eq!(image.indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn parse_lmp_image_too_short() {
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&[0, 1, 2]);
        let err = parse_lmp_image(&data).expect_err("image should fail");
        assert!(matches!(err, LmpError::DataTooShort { .. }));
    }

    #[test]
    fn parse_lmp_image_zero_dimension() {
        let mut data = Vec::new();
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes());
        let err = parse_lmp_image(&data).expect_err("image should fail");
        assert!(matches!(err, LmpError::InvalidDimensions { .. }));
    }

    #[test]
    fn indexed_image_to_rgba() {
        let mut palette_data = vec![0u8; PALETTE_LEN];
        palette_data[0] = 1;
        palette_data[1] = 2;
        palette_data[2] = 3;
        let palette = parse_palette(&palette_data).expect("palette parse");
        let image = IndexedImage {
            width: 1,
            height: 1,
            indices: vec![0],
        };
        assert_eq!(image.to_rgba8(&palette), vec![1, 2, 3, 255]);
    }
}
