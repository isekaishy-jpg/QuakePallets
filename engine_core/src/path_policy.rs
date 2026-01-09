use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigKind {
    Playlist,
    Script,
    Cvars,
    Mounts,
}

impl ConfigKind {
    fn dir_name(self) -> &'static str {
        match self {
            ConfigKind::Playlist => "playlists",
            ConfigKind::Script => "scripts",
            ConfigKind::Cvars => "cvars",
            ConfigKind::Mounts => "mounts",
        }
    }

    fn env_override_key(self) -> &'static str {
        match self {
            ConfigKind::Playlist => "PALLET_CONFIG_OVERRIDE_PLAYLIST",
            ConfigKind::Script => "PALLET_CONFIG_OVERRIDE_SCRIPT",
            ConfigKind::Cvars => "PALLET_CONFIG_OVERRIDE_CVARS",
            ConfigKind::Mounts => "PALLET_CONFIG_OVERRIDE_MOUNTS",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    CliOverride,
    EnvOverride,
    DevOverride,
    ShippedDefault,
    UserConfig,
    BuiltInDefault,
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ConfigSource::CliOverride => "cli override",
            ConfigSource::EnvOverride => "env override",
            ConfigSource::DevOverride => "dev override",
            ConfigSource::ShippedDefault => "shipped default",
            ConfigSource::UserConfig => "user config",
            ConfigSource::BuiltInDefault => "built-in default",
        };
        write!(f, "{}", label)
    }
}

#[derive(Clone, Debug)]
pub struct ResolutionCandidate {
    pub source: ConfigSource,
    pub path: PathBuf,
    pub exists: bool,
}

#[derive(Clone, Debug)]
pub struct ResolvedPath {
    pub path: PathBuf,
    pub source: ConfigSource,
    pub candidates: Vec<ResolutionCandidate>,
}

impl ResolvedPath {
    pub fn describe(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "config resolved ({}) -> {}",
            self.source,
            self.path.display()
        ));
        lines.push(format_candidates(&self.candidates, true));
        lines.join("\n")
    }

    pub fn is_builtin(&self) -> bool {
        self.source == ConfigSource::BuiltInDefault
    }
}

#[derive(Clone, Debug, Default)]
pub struct PathOverrides {
    pub content_root: Option<PathBuf>,
    pub dev_override_root: Option<PathBuf>,
    pub user_config_root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct PathPolicy {
    content_root: PathBuf,
    dev_override_root: Option<PathBuf>,
    user_config_root: PathBuf,
    exe_dir: PathBuf,
}

impl PathPolicy {
    pub fn from_overrides(overrides: PathOverrides) -> Self {
        let exe_dir = current_exe_dir().unwrap_or_else(|| PathBuf::from("."));
        let repo_root = find_repo_root(&exe_dir);
        let content_root = overrides.content_root.unwrap_or_else(|| {
            repo_root
                .as_ref()
                .map(|root| root.join("content"))
                .filter(|path| path.is_dir())
                .unwrap_or_else(|| exe_dir.join("content"))
        });
        let dev_override_root = overrides
            .dev_override_root
            .or_else(|| repo_root.map(|root| root.join(".pallet")));
        let user_config_root = overrides.user_config_root.unwrap_or_else(user_config_root);
        Self {
            content_root,
            dev_override_root,
            user_config_root,
            exe_dir,
        }
    }

    pub fn content_root(&self) -> &Path {
        &self.content_root
    }

    pub fn dev_override_root(&self) -> Option<&Path> {
        self.dev_override_root.as_deref()
    }

    pub fn user_config_root(&self) -> &Path {
        &self.user_config_root
    }

    pub fn resolve_config_file(
        &self,
        kind: ConfigKind,
        input: &str,
    ) -> Result<ResolvedPath, String> {
        self.resolve_config_file_with_fallback(kind, input, false)
    }

    pub fn resolve_config_file_with_fallback(
        &self,
        kind: ConfigKind,
        input: &str,
        allow_builtin: bool,
    ) -> Result<ResolvedPath, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("config input is empty".to_string());
        }

        let mut candidates = Vec::new();

        if let Some(path) = cli_override_path(trimmed) {
            let exists = path.is_file();
            candidates.push(ResolutionCandidate {
                source: ConfigSource::CliOverride,
                path: path.clone(),
                exists,
            });
            if exists {
                return Ok(ResolvedPath {
                    path,
                    source: ConfigSource::CliOverride,
                    candidates,
                });
            }
        }

        if let Ok(value) = env::var(kind.env_override_key()) {
            let path = PathBuf::from(value);
            let exists = path.is_file();
            candidates.push(ResolutionCandidate {
                source: ConfigSource::EnvOverride,
                path: path.clone(),
                exists,
            });
            if exists {
                return Ok(ResolvedPath {
                    path,
                    source: ConfigSource::EnvOverride,
                    candidates,
                });
            }
        }

        if let Some(root) = self.dev_override_root.as_ref() {
            let path = root.join("config").join(kind.dir_name()).join(trimmed);
            let exists = path.is_file();
            candidates.push(ResolutionCandidate {
                source: ConfigSource::DevOverride,
                path: path.clone(),
                exists,
            });
            if exists {
                return Ok(ResolvedPath {
                    path,
                    source: ConfigSource::DevOverride,
                    candidates,
                });
            }
        }

        let shipped = self
            .content_root
            .join("config")
            .join(kind.dir_name())
            .join(trimmed);
        let shipped_exists = shipped.is_file();
        candidates.push(ResolutionCandidate {
            source: ConfigSource::ShippedDefault,
            path: shipped.clone(),
            exists: shipped_exists,
        });
        if shipped_exists {
            return Ok(ResolvedPath {
                path: shipped,
                source: ConfigSource::ShippedDefault,
                candidates,
            });
        }

        let user = self
            .user_config_root
            .join("config")
            .join(kind.dir_name())
            .join(trimmed);
        let user_exists = user.is_file();
        candidates.push(ResolutionCandidate {
            source: ConfigSource::UserConfig,
            path: user.clone(),
            exists: user_exists,
        });
        if user_exists {
            return Ok(ResolvedPath {
                path: user,
                source: ConfigSource::UserConfig,
                candidates,
            });
        }

        candidates.push(ResolutionCandidate {
            source: ConfigSource::BuiltInDefault,
            path: PathBuf::from("<built-in>"),
            exists: false,
        });

        if allow_builtin {
            return Ok(ResolvedPath {
                path: PathBuf::from("<built-in>"),
                source: ConfigSource::BuiltInDefault,
                candidates,
            });
        }

        Err(format!(
            "config resolve failed ({}): no file found\n{}",
            kind.dir_name(),
            format_candidates(&candidates, false)
        ))
    }

    pub fn describe_roots(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("content_root={}", self.content_root.display()));
        lines.push(format!(
            "dev_override_root={}",
            self.dev_override_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<none>".to_string())
        ));
        lines.push(format!(
            "user_config_root={}",
            self.user_config_root.display()
        ));
        lines.push(format!("exe_dir={}", self.exe_dir.display()));
        lines.join("\n")
    }
}

fn cli_override_path(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Some(path)
    } else {
        None
    }
}

fn current_exe_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.to_path_buf()))
}

fn format_candidates(candidates: &[ResolutionCandidate], mark_hits: bool) -> String {
    let mut lines = Vec::new();
    for candidate in candidates {
        let hit = if mark_hits && candidate.exists {
            " [hit]"
        } else {
            ""
        };
        lines.push(format!(
            "- {}: {}{}",
            candidate.source,
            candidate.path.display(),
            hit
        ));
    }
    lines.join("\n")
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cursor = Some(start);
    while let Some(dir) = cursor {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        cursor = dir.parent();
    }
    None
}

pub fn user_config_root() -> PathBuf {
    if let Some(appdata) = env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("Pallet");
    }
    if let Some(config) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config).join("pallet");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("pallet");
    }
    PathBuf::from("pallet_config")
}
