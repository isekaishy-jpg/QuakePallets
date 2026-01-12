//! Test map format parsing and expansion.
#![forbid(unsafe_code)]

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct TestMap {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub map_to_world_scale: Option<f32>,
    #[serde(default)]
    pub space_origin: Option<[f32; 3]>,
    #[serde(default)]
    pub chunking: Option<Chunking>,
    #[serde(default)]
    pub solids: Vec<SolidSpec>,
    #[serde(default)]
    pub generators: Vec<GeneratorSpec>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Chunking {
    pub enabled: bool,
    pub chunk_size: f32,
    #[serde(default)]
    pub padding: f32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SolidSpec {
    pub id: String,
    pub kind: SolidKind,
    pub pos: [f32; 3],
    #[serde(default)]
    pub size: Option<[f32; 3]>,
    #[serde(default)]
    pub radius: Option<f32>,
    #[serde(default)]
    pub height: Option<f32>,
    #[serde(default)]
    pub yaw_deg: Option<f32>,
    #[serde(default)]
    pub rot_euler_deg: Option<[f32; 3]>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub angle_deg: Option<f32>,
    #[serde(default)]
    pub length: Option<f32>,
    #[serde(default)]
    pub width: Option<f32>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SolidKind {
    Box,
    BoxRot,
    Ramp,
    Cylinder,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratorSpec {
    Stairs {
        id: String,
        pos: [f32; 3],
        #[serde(default)]
        yaw_deg: f32,
        step_count: u32,
        step_rise: f32,
        step_run: f32,
        width: f32,
        #[serde(default = "default_stairs_variant_gap")]
        variant_gap: f32,
        #[serde(default)]
        variants: Vec<StairsVariant>,
        #[serde(default)]
        tags: Vec<String>,
    },
    Ramps {
        id: String,
        pos: [f32; 3],
        #[serde(default)]
        yaw_deg: f32,
        width: f32,
        length: f32,
        angles_deg: Vec<f32>,
        #[serde(default)]
        align_base: bool,
        #[serde(default)]
        gap: f32,
        #[serde(default)]
        tags: Vec<String>,
    },
    Corridors {
        id: String,
        pos: [f32; 3],
        #[serde(default)]
        yaw_deg: f32,
        length: f32,
        height: f32,
        capsule_radius: f32,
        margins: Vec<f32>,
        #[serde(default = "default_wall_thickness")]
        wall_thickness: f32,
        #[serde(default)]
        gap: f32,
        #[serde(default)]
        tags: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct StairsVariant {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub step_count: Option<u32>,
    #[serde(default)]
    pub step_rise: Option<f32>,
    #[serde(default)]
    pub step_run: Option<f32>,
    #[serde(default)]
    pub width: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct ResolvedSolid {
    pub id: String,
    pub kind: SolidKind,
    pub pos: [f32; 3],
    pub size: [f32; 3],
    pub yaw_deg: Option<f32>,
    pub rot_euler_deg: Option<[f32; 3]>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TestMapValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl TestMapValidation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

impl TestMap {
    pub fn parse_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|err| err.to_string())
    }

    pub fn validate(&self) -> TestMapValidation {
        let mut validation = TestMapValidation::default();
        if self.version != 1 {
            validation
                .errors
                .push(format!("unsupported version {}", self.version));
        }
        if let Some(scale) = self.map_to_world_scale {
            if !scale.is_finite() || scale <= 0.0 {
                validation
                    .errors
                    .push("map_to_world_scale must be > 0".to_string());
            }
        }
        if let Some(chunking) = &self.chunking {
            if !chunking.chunk_size.is_finite() || chunking.chunk_size <= 0.0 {
                validation.errors.push("chunk_size must be > 0".to_string());
            }
            if !chunking.padding.is_finite() || chunking.padding < 0.0 {
                validation
                    .errors
                    .push("chunking padding must be >= 0".to_string());
            }
        }
        if self.solids.is_empty() && self.generators.is_empty() {
            validation
                .warnings
                .push("test map contains no solids or generators".to_string());
        }
        for solid in &self.solids {
            if solid.id.trim().is_empty() {
                validation
                    .errors
                    .push("solid id must not be empty".to_string());
            }
            if !vector_is_finite(solid.pos) {
                validation
                    .errors
                    .push(format!("solid '{}' has invalid pos", solid.id));
            }
            if let Some(size) = solid.effective_size() {
                if !vector_is_finite(size) || size.iter().any(|value| *value <= 0.0) {
                    validation
                        .errors
                        .push(format!("solid '{}' has invalid size", solid.id));
                }
            } else {
                validation
                    .errors
                    .push(format!("solid '{}' missing size/radius/height", solid.id));
            }
            if solid.kind == SolidKind::Ramp {
                if let Some(angle) = solid.angle_deg {
                    if angle <= 0.0 || angle >= 89.0 {
                        validation.errors.push(format!(
                            "solid '{}' ramp angle_deg must be between 0 and 89",
                            solid.id
                        ));
                    }
                }
            }
        }
        for generator in &self.generators {
            validate_generator(generator, &mut validation);
        }
        validation
    }

    pub fn expanded_solids(&self) -> Result<Vec<ResolvedSolid>, String> {
        let validation = self.validate();
        if !validation.is_ok() {
            return Err(validation.errors.join("; "));
        }
        let mut solids = Vec::new();
        for solid in &self.solids {
            solids.push(solid.resolve()?);
        }
        for generator in &self.generators {
            solids.extend(generator.expand()?);
        }
        let mut seen = std::collections::HashSet::new();
        for solid in &solids {
            if !seen.insert(solid.id.clone()) {
                return Err(format!("duplicate solid id '{}'", solid.id));
            }
        }
        Ok(solids)
    }
}

impl SolidSpec {
    fn resolve(&self) -> Result<ResolvedSolid, String> {
        let size = self
            .effective_size()
            .ok_or_else(|| format!("solid '{}' missing size", self.id))?;
        Ok(ResolvedSolid {
            id: self.id.clone(),
            kind: self.kind,
            pos: self.pos,
            size,
            yaw_deg: self.yaw_deg,
            rot_euler_deg: self.rot_euler_deg,
            tags: self.tags.clone(),
        })
    }

    fn effective_size(&self) -> Option<[f32; 3]> {
        if let Some(size) = self.size {
            return Some(size);
        }
        if self.kind == SolidKind::Cylinder {
            let radius = self.radius?;
            let height = self.height?;
            return Some([radius * 2.0, height, radius * 2.0]);
        }
        if self.kind == SolidKind::Ramp {
            if let (Some(length), Some(width), Some(angle)) =
                (self.length, self.width, self.angle_deg)
            {
                let height = angle.to_radians().tan() * length;
                return Some([length, height, width]);
            }
        }
        None
    }
}

impl GeneratorSpec {
    fn expand(&self) -> Result<Vec<ResolvedSolid>, String> {
        match self {
            GeneratorSpec::Stairs {
                id,
                pos,
                yaw_deg,
                step_count,
                step_rise,
                step_run,
                width,
                variant_gap,
                variants,
                tags,
            } => build_stairs(
                StairsParams {
                    base_id: id,
                    pos: *pos,
                    yaw_deg: *yaw_deg,
                    step_count: *step_count,
                    step_rise: *step_rise,
                    step_run: *step_run,
                    width: *width,
                    variant_gap: *variant_gap,
                    tags,
                },
                variants,
            ),
            GeneratorSpec::Ramps {
                id,
                pos,
                yaw_deg,
                width,
                length,
                angles_deg,
                align_base,
                gap,
                tags,
            } => build_ramps(RampsParams {
                base_id: id,
                pos: *pos,
                yaw_deg: *yaw_deg,
                width: *width,
                length: *length,
                angles_deg,
                align_base: *align_base,
                gap: *gap,
                tags,
            }),
            GeneratorSpec::Corridors {
                id,
                pos,
                yaw_deg,
                length,
                height,
                capsule_radius,
                margins,
                wall_thickness,
                gap,
                tags,
            } => build_corridors(CorridorsParams {
                base_id: id,
                pos: *pos,
                yaw_deg: *yaw_deg,
                length: *length,
                height: *height,
                capsule_radius: *capsule_radius,
                margins,
                wall_thickness: *wall_thickness,
                gap: *gap,
                tags,
            }),
        }
    }
}

fn validate_generator(generator: &GeneratorSpec, validation: &mut TestMapValidation) {
    match generator {
        GeneratorSpec::Stairs {
            id,
            step_count,
            step_rise,
            step_run,
            width,
            variant_gap,
            ..
        } => {
            if id.trim().is_empty() {
                validation
                    .errors
                    .push("stairs generator id must not be empty".to_string());
            }
            if *step_count == 0 {
                validation
                    .errors
                    .push(format!("stairs '{}' step_count must be > 0", id));
            }
            if *step_rise <= 0.0 || *step_run <= 0.0 || *width <= 0.0 {
                validation
                    .errors
                    .push(format!("stairs '{}' dimensions must be > 0", id));
            }
            if *variant_gap < 0.0 {
                validation
                    .errors
                    .push(format!("stairs '{}' variant_gap must be >= 0", id));
            }
        }
        GeneratorSpec::Ramps {
            id,
            width,
            length,
            angles_deg,
            ..
        } => {
            if id.trim().is_empty() {
                validation
                    .errors
                    .push("ramps generator id must not be empty".to_string());
            }
            if *width <= 0.0 || *length <= 0.0 {
                validation
                    .errors
                    .push(format!("ramps '{}' dimensions must be > 0", id));
            }
            if angles_deg.is_empty() {
                validation
                    .errors
                    .push(format!("ramps '{}' angles_deg must not be empty", id));
            }
            if angles_deg
                .iter()
                .any(|angle| *angle <= 0.0 || *angle >= 89.0)
            {
                validation
                    .errors
                    .push(format!("ramps '{}' angle out of range", id));
            }
        }
        GeneratorSpec::Corridors {
            id,
            length,
            height,
            capsule_radius,
            margins,
            ..
        } => {
            if id.trim().is_empty() {
                validation
                    .errors
                    .push("corridors generator id must not be empty".to_string());
            }
            if *length <= 0.0 || *height <= 0.0 {
                validation
                    .errors
                    .push(format!("corridors '{}' dimensions must be > 0", id));
            }
            if *capsule_radius <= 0.0 {
                validation
                    .errors
                    .push(format!("corridors '{}' capsule_radius must be > 0", id));
            }
            if margins.is_empty() {
                validation
                    .errors
                    .push(format!("corridors '{}' margins must not be empty", id));
            }
            if margins.iter().any(|margin| *margin < 0.0) {
                validation
                    .errors
                    .push(format!("corridors '{}' margins must be >= 0", id));
            }
        }
    }
}

struct StairsParams<'a> {
    base_id: &'a str,
    pos: [f32; 3],
    yaw_deg: f32,
    step_count: u32,
    step_rise: f32,
    step_run: f32,
    width: f32,
    variant_gap: f32,
    tags: &'a [String],
}

fn build_stairs(
    params: StairsParams<'_>,
    variants: &[StairsVariant],
) -> Result<Vec<ResolvedSolid>, String> {
    let mut solids = Vec::new();
    let yaw = params.yaw_deg.to_radians();
    let side = rotate_y([0.0, 0.0, 1.0], yaw);
    let stride = params.width + params.variant_gap;
    solids.extend(stairs_variant(
        StairsParams {
            base_id: params.base_id,
            pos: params.pos,
            yaw_deg: params.yaw_deg,
            step_count: params.step_count,
            step_rise: params.step_rise,
            step_run: params.step_run,
            width: params.width,
            variant_gap: params.variant_gap,
            tags: params.tags,
        },
        "base",
    )?);
    for (index, variant) in variants.iter().enumerate() {
        let offset = stride * (index as f32 + 1.0);
        let pos = [
            params.pos[0] + side[0] * offset,
            params.pos[1] + side[1] * offset,
            params.pos[2] + side[2] * offset,
        ];
        let variant_id = variant
            .id
            .clone()
            .unwrap_or_else(|| format!("variant_{}", index + 1));
        let count = variant.step_count.unwrap_or(params.step_count);
        let rise = variant.step_rise.unwrap_or(params.step_rise);
        let run = variant.step_run.unwrap_or(params.step_run);
        let variant_width = variant.width.unwrap_or(params.width);
        solids.extend(stairs_variant(
            StairsParams {
                base_id: params.base_id,
                pos,
                yaw_deg: params.yaw_deg,
                step_count: count,
                step_rise: rise,
                step_run: run,
                width: variant_width,
                variant_gap: params.variant_gap,
                tags: params.tags,
            },
            &variant_id,
        )?);
    }
    Ok(solids)
}

fn stairs_variant(params: StairsParams<'_>, label: &str) -> Result<Vec<ResolvedSolid>, String> {
    if params.step_count == 0 {
        return Ok(Vec::new());
    }
    let yaw = params.yaw_deg.to_radians();
    let mut solids = Vec::new();
    for index in 0..params.step_count {
        let step_index = index as f32;
        let local = [
            params.step_run * (step_index + 0.5),
            params.step_rise * (step_index + 0.5),
            0.0,
        ];
        let rotated = rotate_y(local, yaw);
        let center = [
            params.pos[0] + rotated[0],
            params.pos[1] + rotated[1],
            params.pos[2] + rotated[2],
        ];
        solids.push(ResolvedSolid {
            id: format!("{}/{}/step_{:02}", params.base_id, label, index + 1),
            kind: SolidKind::Box,
            pos: center,
            size: [params.step_run, params.step_rise, params.width],
            yaw_deg: Some(params.yaw_deg),
            rot_euler_deg: None,
            tags: params.tags.to_vec(),
        });
    }
    Ok(solids)
}

struct RampsParams<'a> {
    base_id: &'a str,
    pos: [f32; 3],
    yaw_deg: f32,
    width: f32,
    length: f32,
    angles_deg: &'a [f32],
    align_base: bool,
    gap: f32,
    tags: &'a [String],
}

fn build_ramps(params: RampsParams<'_>) -> Result<Vec<ResolvedSolid>, String> {
    let mut solids = Vec::new();
    let yaw = params.yaw_deg.to_radians();
    for (index, angle) in params.angles_deg.iter().enumerate() {
        let height = angle.to_radians().tan() * params.length;
        let offset = (params.length + params.gap) * index as f32;
        let local = [offset, 0.0, 0.0];
        let rotated = rotate_y(local, yaw);
        let base_y = if params.align_base {
            params.pos[1] + height * 0.5
        } else {
            params.pos[1]
        };
        let center = [
            params.pos[0] + rotated[0],
            base_y,
            params.pos[2] + rotated[2],
        ];
        solids.push(ResolvedSolid {
            id: format!("{}/ramp_{:02}", params.base_id, index + 1),
            kind: SolidKind::Ramp,
            pos: center,
            size: [params.length, height, params.width],
            yaw_deg: Some(params.yaw_deg),
            rot_euler_deg: None,
            tags: params.tags.to_vec(),
        });
    }
    Ok(solids)
}

struct CorridorsParams<'a> {
    base_id: &'a str,
    pos: [f32; 3],
    yaw_deg: f32,
    length: f32,
    height: f32,
    capsule_radius: f32,
    margins: &'a [f32],
    wall_thickness: f32,
    gap: f32,
    tags: &'a [String],
}

fn build_corridors(params: CorridorsParams<'_>) -> Result<Vec<ResolvedSolid>, String> {
    let mut solids = Vec::new();
    let yaw = params.yaw_deg.to_radians();
    for (index, margin) in params.margins.iter().enumerate() {
        let corridor_width = params.capsule_radius * 2.0 + margin * 2.0;
        let corridor_id = format!("{}/corridor_{:02}", params.base_id, index + 1);
        let offset = (params.length + params.gap) * index as f32;
        let local = [offset, 0.0, 0.0];
        let rotated = rotate_y(local, yaw);
        let center = [
            params.pos[0] + rotated[0],
            params.pos[1] + rotated[1],
            params.pos[2] + rotated[2],
        ];

        let floor = ResolvedSolid {
            id: format!("{}/floor", corridor_id),
            kind: SolidKind::Box,
            pos: center,
            size: [
                params.length,
                params.wall_thickness,
                corridor_width + params.wall_thickness * 2.0,
            ],
            yaw_deg: Some(params.yaw_deg),
            rot_euler_deg: None,
            tags: params.tags.to_vec(),
        };
        let ceiling = ResolvedSolid {
            id: format!("{}/ceiling", corridor_id),
            kind: SolidKind::Box,
            pos: [
                center[0],
                center[1] + params.height + params.wall_thickness * 0.5,
                center[2],
            ],
            size: [
                params.length,
                params.wall_thickness,
                corridor_width + params.wall_thickness * 2.0,
            ],
            yaw_deg: Some(params.yaw_deg),
            rot_euler_deg: None,
            tags: params.tags.to_vec(),
        };
        let wall_offset = corridor_width * 0.5 + params.wall_thickness * 0.5;
        let left_offset = rotate_y([0.0, params.height * 0.5, -wall_offset], yaw);
        let left = ResolvedSolid {
            id: format!("{}/wall_left", corridor_id),
            kind: SolidKind::Box,
            pos: [
                center[0] + left_offset[0],
                center[1] + left_offset[1],
                center[2] + left_offset[2],
            ],
            size: [params.length, params.height, params.wall_thickness],
            yaw_deg: Some(params.yaw_deg),
            rot_euler_deg: None,
            tags: params.tags.to_vec(),
        };
        let right_offset = rotate_y([0.0, params.height * 0.5, wall_offset], yaw);
        let right = ResolvedSolid {
            id: format!("{}/wall_right", corridor_id),
            kind: SolidKind::Box,
            pos: [
                center[0] + right_offset[0],
                center[1] + right_offset[1],
                center[2] + right_offset[2],
            ],
            size: [params.length, params.height, params.wall_thickness],
            yaw_deg: Some(params.yaw_deg),
            rot_euler_deg: None,
            tags: params.tags.to_vec(),
        };
        solids.extend([floor, ceiling, left, right]);
    }
    Ok(solids)
}

fn rotate_y(value: [f32; 3], yaw: f32) -> [f32; 3] {
    let (sin, cos) = yaw.sin_cos();
    [
        value[0] * cos + value[2] * sin,
        value[1],
        -value[0] * sin + value[2] * cos,
    ]
}

fn vector_is_finite(value: [f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}

fn default_wall_thickness() -> f32 {
    0.2
}

fn default_stairs_variant_gap() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_map() {
        let text = r#"
version = 1
name = "test"
map_to_world_scale = 1.0

[[solids]]
id = "floor"
kind = "box"
pos = [0.0, 0.0, 0.0]
size = [10.0, 1.0, 10.0]
"#;
        let map = TestMap::parse_toml(text).expect("parse");
        let solids = map.expanded_solids().expect("expand");
        assert_eq!(solids.len(), 1);
        assert_eq!(solids[0].id, "floor");
    }
}
