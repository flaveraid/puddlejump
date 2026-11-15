//! The user's own config file, `~/.config/puddlejump/config.toml`, which says
//! where their repo is and which host profile they use. It lives outside the
//! repo because it is needed to find the repo.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// The directory under the user's config directory that holds the config.
const CONFIG_DIR_NAME: &str = "puddlejump";

/// The filename of the user config.
const USER_CONFIG_FILE_NAME: &str = "config.toml";

/// Settings that belong to the user rather than to a repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserConfig {
    /// The dotfiles repo to use when `--repo` and `PUDDLEJUMP_REPO` are not
    /// given. Always absolute: a leading `~/` has been expanded.
    pub repo: Option<PathBuf>,
    /// The host profile to use when `--host` and `PUDDLEJUMP_HOST` are not
    /// given.
    pub host: Option<String>,
}

/// Why the user config could not be read.
#[derive(Debug, Error)]
pub enum UserConfigError {
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
    /// `repo` is neither absolute nor under `~/`, so it would mean a different
    /// place depending on where the command is run.
    #[error("invalid config {path}: repo must be an absolute path or start with `~/`, not {repo}")]
    RelativeRepo {
        /// The config path.
        path: PathBuf,
        /// The value as written.
        repo: String,
    },
}

/// Every key the user config accepts. Keep this in step with `RawUserConfig`
/// below; a test fails if they drift apart.
pub const USER_CONFIG_KEYS: [&str; 2] = ["repo", "host"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserConfig {
    repo: Option<String>,
    host: Option<String>,
}

/// Where the user config lives: `$XDG_CONFIG_HOME/puddlejump/config.toml`, or
/// `<home>/.config/puddlejump/config.toml` when `xdg_config_home` is unset,
/// empty, or not absolute (the XDG rule is to ignore a relative value).
pub fn user_config_path(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
    let base = xdg_config_home
        .filter(|directory| directory.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".config"));
    base.join(CONFIG_DIR_NAME).join(USER_CONFIG_FILE_NAME)
}

impl UserConfig {
    /// Reads the user config at `path`. A missing file is not an error and
    /// gives the defaults. `home` is what a leading `~/` in `repo` means.
    ///
    /// Errors if the file exists but cannot be read, is not valid TOML, has
    /// unknown keys, or has a `repo` that is neither absolute nor under `~/`.
    pub fn load(path: &Path, home: &Path) -> Result<Self, UserConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(UserConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        Self::parse(&text, path, home)
    }

    fn parse(text: &str, path: &Path, home: &Path) -> Result<Self, UserConfigError> {
        let raw: RawUserConfig =
            toml::from_str(text).map_err(|source| UserConfigError::Syntax {
                path: path.to_path_buf(),
                source,
            })?;
        let repo = raw
            .repo
            .map(|repo| {
                expand_repo(&repo, home).ok_or_else(|| UserConfigError::RelativeRepo {
                    path: path.to_path_buf(),
                    repo,
                })
            })
            .transpose()?;
        Ok(Self {
            repo,
            host: raw.host,
        })
    }
}

/// Expands a leading `~/` to `home`, and accepts an absolute path as is.
fn expand_repo(repo: &str, home: &Path) -> Option<PathBuf> {
    if let Some(rest) = repo.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    let path = Path::new(repo);
    path.is_absolute().then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/me";

    fn load_text(text: &str) -> Result<UserConfig, UserConfigError> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(USER_CONFIG_FILE_NAME);
        fs::write(&path, text).unwrap();
        UserConfig::load(&path, Path::new(HOME))
    }

    #[test]
    fn the_path_defaults_to_dot_config_under_home() {
        assert_eq!(
            user_config_path(None, Path::new(HOME)),
            Path::new("/home/me/.config/puddlejump/config.toml")
        );
    }

    #[test]
    fn the_path_follows_xdg_config_home_when_it_is_absolute() {
        assert_eq!(
            user_config_path(Some(Path::new("/xdg")), Path::new(HOME)),
            Path::new("/xdg/puddlejump/config.toml")
        );
    }

    #[test]
    fn an_empty_xdg_config_home_is_ignored() {
        assert_eq!(
            user_config_path(Some(Path::new("")), Path::new(HOME)),
            Path::new("/home/me/.config/puddlejump/config.toml")
        );
    }

    #[test]
    fn a_relative_xdg_config_home_is_ignored() {
        assert_eq!(
            user_config_path(Some(Path::new("config")), Path::new(HOME)),
            Path::new("/home/me/.config/puddlejump/config.toml")
        );
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent.toml");

        assert_eq!(
            UserConfig::load(&path, Path::new(HOME)).unwrap(),
            UserConfig::default()
        );
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(load_text("").unwrap(), UserConfig::default());
    }

    #[test]
    fn both_keys_are_read() {
        let config = load_text("repo = \"/srv/dotfiles\"\nhost = \"lenovo\"").unwrap();

        assert_eq!(config.repo, Some(PathBuf::from("/srv/dotfiles")));
        assert_eq!(config.host.as_deref(), Some("lenovo"));
    }

    #[test]
    fn a_leading_tilde_slash_in_repo_means_the_home_directory() {
        let config = load_text("repo = \"~/dotfiles\"").unwrap();

        assert_eq!(config.repo, Some(PathBuf::from("/home/me/dotfiles")));
    }

    #[test]
    fn a_relative_repo_is_an_error_naming_the_file_and_the_value() {
        let error = load_text("repo = \"dotfiles\"").unwrap_err();

        assert!(matches!(error, UserConfigError::RelativeRepo { .. }));
        assert!(error.to_string().contains(USER_CONFIG_FILE_NAME));
        assert!(error.to_string().contains("dotfiles"));
    }

    #[test]
    fn a_bare_tilde_is_not_expanded_and_is_an_error() {
        let error = load_text("repo = \"~\"").unwrap_err();

        assert!(matches!(error, UserConfigError::RelativeRepo { .. }));
    }

    #[test]
    fn another_users_tilde_is_an_error() {
        let error = load_text("repo = \"~bob/dotfiles\"").unwrap_err();

        assert!(matches!(error, UserConfigError::RelativeRepo { .. }));
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let error = load_text("colour = \"red\"").unwrap_err();

        assert!(matches!(error, UserConfigError::Syntax { .. }));
    }

    #[test]
    fn invalid_toml_is_an_error_naming_the_file() {
        let error = load_text("repo = ").unwrap_err();

        assert!(matches!(error, UserConfigError::Syntax { .. }));
        assert!(error.to_string().contains(USER_CONFIG_FILE_NAME));
    }

    #[test]
    fn a_wrongly_typed_value_is_an_error() {
        let error = load_text("host = 3").unwrap_err();

        assert!(matches!(error, UserConfigError::Syntax { .. }));
    }

    #[test]
    fn a_config_that_is_a_directory_is_a_read_error() {
        let directory = tempfile::tempdir().unwrap();

        let error = UserConfig::load(directory.path(), Path::new(HOME)).unwrap_err();

        assert!(matches!(error, UserConfigError::Read { .. }));
    }

    #[test]
    fn the_key_list_matches_what_the_parser_accepts() {
        for key in USER_CONFIG_KEYS {
            let value = if key == "repo" { "\"/srv\"" } else { "\"x\"" };
            assert!(
                load_text(&format!("{key} = {value}")).is_ok(),
                "{key} should be a key"
            );
        }
        assert!(load_text("other = \"x\"").is_err());
    }
}
