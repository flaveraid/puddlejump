//! Repo-level configuration: the optional `puddlejump.toml` at the repo root.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::system::{ManagerId, UnknownManagerError};

/// The filename of the repo config, at the repo root.
pub const CONFIG_FILE_NAME: &str = "puddlejump.toml";

/// Repo-level settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoConfig {
    /// The package manager to use instead of auto-detecting one.
    pub package_manager: Option<ManagerId>,
}

/// Why the repo config could not be read.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The config path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The file is not valid TOML, or has unknown or mistyped keys.
    #[error("invalid config {path}: {source}")]
    Syntax {
        /// The config path.
        path: PathBuf,
        /// What is wrong with it.
        source: toml::de::Error,
    },
    /// `package_manager` is not a known manager.
    #[error("invalid config {path}: {source}")]
    UnknownManager {
        /// The config path.
        path: PathBuf,
        /// Which value is wrong.
        source: UnknownManagerError,
    },
}

/// Every key `puddlejump.toml` accepts. Keep this in step with `RawConfig`
/// below, and document each key in the starter config that `init` writes; a
/// test fails until the starter mentions every key listed here.
pub const CONFIG_KEYS: [&str; 1] = ["package_manager"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    package_manager: Option<String>,
}

impl RepoConfig {
    /// Reads `<repo_root>/puddlejump.toml`. A missing file is not an error and
    /// gives the defaults.
    ///
    /// Errors if the file exists but cannot be read, is not valid TOML, has
    /// unknown keys, or names an unknown package manager.
    pub fn load(repo_root: &Path) -> Result<Self, ConfigError> {
        let path = repo_root.join(CONFIG_FILE_NAME);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        Self::parse(&text, &path)
    }

    fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|source| ConfigError::Syntax {
            path: path.to_path_buf(),
            source,
        })?;
        let package_manager = raw
            .package_manager
            .map(|manager| manager.parse::<ManagerId>())
            .transpose()
            .map_err(|source| ConfigError::UnknownManager {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self { package_manager })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_text(text: &str) -> Result<RepoConfig, ConfigError> {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(CONFIG_FILE_NAME), text).unwrap();
        RepoConfig::load(root.path())
    }

    #[test]
    fn a_missing_config_gives_the_defaults() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            RepoConfig::load(root.path()).unwrap(),
            RepoConfig::default()
        );
    }

    #[test]
    fn the_default_config_has_no_package_manager_override() {
        assert_eq!(RepoConfig::default().package_manager, None);
    }

    #[test]
    fn an_empty_config_gives_the_defaults() {
        assert_eq!(load_text("").unwrap(), RepoConfig::default());
    }

    #[test]
    fn a_package_manager_override_is_read() {
        let config = load_text("package_manager = \"xbps\"").unwrap();
        assert_eq!(config.package_manager, Some(ManagerId::Xbps));
    }

    #[test]
    fn an_unknown_package_manager_is_an_error_naming_the_file() {
        let error = load_text("package_manager = \"xpbs\"").unwrap_err();

        assert!(matches!(error, ConfigError::UnknownManager { .. }));
        assert!(error.to_string().contains(CONFIG_FILE_NAME));
        assert!(error.to_string().contains("xpbs"));
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let error = load_text("colour = \"red\"").unwrap_err();
        assert!(matches!(error, ConfigError::Syntax { .. }));
    }

    #[test]
    fn invalid_toml_is_an_error_naming_the_file() {
        let error = load_text("package_manager = ").unwrap_err();

        assert!(matches!(error, ConfigError::Syntax { .. }));
        assert!(error.to_string().contains(CONFIG_FILE_NAME));
    }

    #[test]
    fn a_wrongly_typed_value_is_an_error() {
        let error = load_text("package_manager = 3").unwrap_err();
        assert!(matches!(error, ConfigError::Syntax { .. }));
    }

    #[test]
    fn a_config_that_is_a_directory_is_a_read_error() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(CONFIG_FILE_NAME)).unwrap();

        let error = RepoConfig::load(root.path()).unwrap_err();

        assert!(matches!(error, ConfigError::Read { .. }));
    }
}
