use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct MapSidecar {
    pub version: u32,
    pub map_id: String,
    pub map_to_world_scale: f32,
    #[serde(default)]
    pub space_origin: Option<[f32; 3]>,
    #[serde(default)]
    pub spawns: Vec<SpawnSpec>,
    #[serde(default)]
    pub markers: Vec<MarkerSpec>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SpawnSpec {
    #[serde(default)]
    pub id: Option<String>,
    pub origin: [f32; 3],
    #[serde(default)]
    pub yaw_deg: Option<f32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MarkerSpec {
    pub id: String,
    pub kind: String,
    pub origin: [f32; 3],
    #[serde(default)]
    pub yaw_deg: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct MapSidecarValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl MapSidecarValidation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

impl MapSidecar {
    pub fn parse_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|err| err.to_string())
    }

    pub fn validate(&self) -> MapSidecarValidation {
        let mut validation = MapSidecarValidation::default();
        if self.version != 1 {
            validation
                .errors
                .push(format!("unsupported sidecar version {}", self.version));
        }
        if self.map_id.trim().is_empty() {
            validation
                .errors
                .push("map_id must not be empty".to_string());
        }
        if !self.map_to_world_scale.is_finite() || self.map_to_world_scale <= 0.0 {
            validation
                .errors
                .push("map_to_world_scale must be finite and > 0".to_string());
        }
        if let Some(origin) = self.space_origin {
            if !vector_is_finite(origin) {
                validation
                    .errors
                    .push("space_origin must be finite".to_string());
            }
        }
        if self.spawns.is_empty() && self.markers.is_empty() {
            validation
                .warnings
                .push("sidecar contains no spawns or markers".to_string());
        }
        for spawn in &self.spawns {
            if !vector_is_finite(spawn.origin) {
                validation
                    .errors
                    .push("spawn origin must be finite".to_string());
            }
        }
        for marker in &self.markers {
            if marker.id.trim().is_empty() {
                validation
                    .errors
                    .push("marker id must not be empty".to_string());
            }
            if marker.kind.trim().is_empty() {
                validation
                    .errors
                    .push("marker kind must not be empty".to_string());
            }
            if !vector_is_finite(marker.origin) {
                validation
                    .errors
                    .push("marker origin must be finite".to_string());
            }
        }
        validation
    }
}

fn vector_is_finite(value: [f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}
