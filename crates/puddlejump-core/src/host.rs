//! Host profiles: which packages a given machine should have.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::package::{PackageName, PackageNameError};

/// Directory under the repo root that holds host profiles.
pub const HOSTS_DIR_NAME: &str = "hosts";

/// Where the kernel exposes the machine's hostname.
const KERNEL_HOSTNAME_PATH: &str = "/proc/sys/kernel/hostname";

/// The fallback when the kernel file cannot be read.
const ETC_HOSTNAME_PATH: &str = "/etc/hostname";

/// The name of a host profile, which is also its filename under `hosts/`.
///
/// Validated like a package name, because it becomes part of a path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostName(PackageName);

impl HostName {
    /// Validates `name` and wraps it.
    ///
    /// Errors if the name is empty, starts with a dot, contains a path
    /// separator, or contains whitespace or control characters.
    pub fn new(name: &str) -> Result<Self, PackageNameError> {
        PackageName::new(name).map(Self)
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A machine's profile: the packages it wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProfile {
    /// The profile's name.
    pub name: HostName,
    /// The packages to link. Their dependencies are added automatically.
    pub packages: Vec<PackageName>,
}

/// Why a host profile could not be found or read.
#[derive(Debug, Error)]
pub enum HostError {
    /// The requested profile does not exist.
    #[error("no host profile named {name} (expected {path})")]
    NotFound {
        /// The profile that was asked for.
        name: HostName,
        /// Where it would be.
        path: PathBuf,
    },
    /// A requested host name is not a valid profile name.
    #[error("invalid host name: {0}")]
    InvalidName(#[from] PackageNameError),
    /// The profile exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The profile path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The profile is not valid TOML, or has unknown or mistyped keys.
    #[error("invalid host profile {path}: {source}")]
    Syntax {
        /// The profile path.
        path: PathBuf,
        /// What is wrong with it.
        source: toml::de::Error,
    },
    /// A package listed in the profile is not a valid package name.
    #[error("invalid host profile {path}: {source}")]
    InvalidPackage {
        /// The profile path.
        path: PathBuf,
        /// Why the name is invalid.
        source: PackageNameError,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    #[serde(default)]
    packages: Vec<String>,
}

fn profile_path(repo_root: &Path, name: &HostName) -> PathBuf {
    repo_root.join(HOSTS_DIR_NAME).join(format!("{name}.toml"))
}

impl HostProfile {
    /// Loads the profile `name` from `<repo_root>/hosts/<name>.toml`.
    ///
    /// Package names are checked for validity here; whether they exist in the
    /// repo is checked when the dependency graph is built.
    ///
    /// Errors if the profile does not exist, cannot be read, is not valid TOML,
    /// has unknown keys, or lists an invalid package name.
    pub fn load(repo_root: &Path, name: &HostName) -> Result<Self, HostError> {
        let path = profile_path(repo_root, name);
        let text = fs::read_to_string(&path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                HostError::NotFound {
                    name: name.clone(),
                    path: path.clone(),
                }
            } else {
                HostError::Read {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        let raw: RawProfile = toml::from_str(&text).map_err(|source| HostError::Syntax {
            path: path.clone(),
            source,
        })?;
        let packages = raw
            .packages
            .iter()
            .map(|package| PackageName::new(package))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| HostError::InvalidPackage { path, source })?;
        Ok(Self {
            name: name.clone(),
            packages,
        })
    }
}

/// Where a selected host name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSource {
    /// The `--host` flag.
    Flag,
    /// The `PUDDLEJUMP_HOST` environment variable.
    Environment,
    /// The machine's hostname.
    Hostname,
}

/// A host profile chosen for this run, and how it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedHost {
    /// The profile name.
    pub name: HostName,
    /// How it was chosen.
    pub source: HostSource,
}

/// Decides which host profile to use, if any.
///
/// In order: the `flag`, then the `environment` value, then the machine's
/// `hostname`. A host named explicitly (flag or environment) must have a
/// profile in `available`, otherwise that is an error. The hostname is only a
/// guess, so it is used only if a profile with that name exists; with no
/// match there is simply no profile, which is allowed.
///
/// Errors if an explicit name is invalid or has no profile.
pub fn select_host(
    flag: Option<&str>,
    environment: Option<&str>,
    hostname: Option<&str>,
    available: &[HostName],
    repo_root: &Path,
) -> Result<Option<SelectedHost>, HostError> {
    let explicit = flag
        .map(|name| (name, HostSource::Flag))
        .or(environment.map(|name| (name, HostSource::Environment)));
    if let Some((name, source)) = explicit {
        let name = HostName::new(name)?;
        if !available.contains(&name) {
            return Err(HostError::NotFound {
                path: profile_path(repo_root, &name),
                name,
            });
        }
        return Ok(Some(SelectedHost { name, source }));
    }
    let guessed = hostname
        .and_then(|name| HostName::new(name).ok())
        .filter(|name| available.contains(name));
    Ok(guessed.map(|name| SelectedHost {
        name,
        source: HostSource::Hostname,
    }))
}

/// Lists the host profiles in `<repo_root>/hosts`, sorted by name.
///
/// A missing `hosts/` directory means no profiles. Files that are not
/// `<valid-name>.toml` are ignored.
///
/// Errors if the directory exists but cannot be read.
pub fn available_hosts(repo_root: &Path) -> Result<Vec<HostName>, HostError> {
    let directory = repo_root.join(HOSTS_DIR_NAME);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(HostError::Read {
                path: directory,
                source,
            })
        }
    };
    let mut hosts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| HostError::Read {
            path: directory.clone(),
            source,
        })?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".toml"))
            .and_then(|name| HostName::new(name).ok());
        if let Some(name) = name.filter(|_| path.is_file()) {
            hosts.push(name);
        }
    }
    hosts.sort();
    Ok(hosts)
}

/// The machine's hostname, or `None` if it cannot be determined.
///
/// Reads the kernel's value and falls back to `/etc/hostname`. An unreadable
/// or empty hostname is not an error, because it only matters as a guess.
pub fn machine_hostname() -> Option<String> {
    [KERNEL_HOSTNAME_PATH, ETC_HOSTNAME_PATH]
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .find(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> HostName {
        HostName::new(name).unwrap()
    }

    fn package(name: &str) -> PackageName {
        PackageName::new(name).unwrap()
    }

    fn write_profile(root: &Path, name: &str, text: &str) {
        let directory = root.join("hosts");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(format!("{name}.toml")), text).unwrap();
    }

    fn select(
        flag: Option<&str>,
        environment: Option<&str>,
        hostname: Option<&str>,
        available: &[&str],
    ) -> Result<Option<SelectedHost>, HostError> {
        let available: Vec<HostName> = available.iter().map(|name| host(name)).collect();
        select_host(flag, environment, hostname, &available, Path::new("/repo"))
    }

    fn selected(name: &str, source: HostSource) -> Option<SelectedHost> {
        Some(SelectedHost {
            name: host(name),
            source,
        })
    }

    #[test]
    fn host_names_accept_dots_inside_the_name() {
        assert_eq!(host("lenovo.local").as_str(), "lenovo.local");
        assert_eq!(host("laptop").to_string(), "laptop");
    }

    #[test]
    fn host_names_reject_anything_that_could_escape_the_hosts_directory() {
        for name in ["", ".hidden", "a/b", "../etc", "a b"] {
            assert!(HostName::new(name).is_err(), "{name:?} should be rejected");
        }
    }

    #[test]
    fn a_profile_lists_its_packages_in_order() {
        let root = tempfile::tempdir().unwrap();
        write_profile(
            root.path(),
            "laptop",
            "packages = [\"zsh\", \"nvim\", \"dwm\"]",
        );

        let profile = HostProfile::load(root.path(), &host("laptop")).unwrap();

        assert_eq!(profile.name, host("laptop"));
        assert_eq!(
            profile.packages,
            [package("zsh"), package("nvim"), package("dwm")]
        );
    }

    #[test]
    fn a_profile_with_no_packages_key_has_no_packages() {
        let root = tempfile::tempdir().unwrap();
        write_profile(root.path(), "bare", "");

        let profile = HostProfile::load(root.path(), &host("bare")).unwrap();

        assert!(profile.packages.is_empty());
    }

    #[test]
    fn a_missing_profile_is_not_found_and_names_the_expected_path() {
        let root = tempfile::tempdir().unwrap();

        let error = HostProfile::load(root.path(), &host("ghost")).unwrap_err();

        assert!(matches!(error, HostError::NotFound { .. }));
        assert!(error.to_string().contains("ghost"));
        assert!(error.to_string().contains("hosts/ghost.toml"));
    }

    #[test]
    fn a_profile_with_an_unknown_key_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        write_profile(root.path(), "laptop", "include = [\"common\"]");

        let error = HostProfile::load(root.path(), &host("laptop")).unwrap_err();

        assert!(matches!(error, HostError::Syntax { .. }));
    }

    #[test]
    fn a_profile_with_invalid_toml_is_an_error_naming_the_file() {
        let root = tempfile::tempdir().unwrap();
        write_profile(root.path(), "laptop", "packages = ");

        let error = HostProfile::load(root.path(), &host("laptop")).unwrap_err();

        assert!(matches!(error, HostError::Syntax { .. }));
        assert!(error.to_string().contains("laptop.toml"));
    }

    #[test]
    fn a_profile_listing_an_invalid_package_name_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        write_profile(root.path(), "laptop", "packages = [\"zsh\", \"a/b\"]");

        let error = HostProfile::load(root.path(), &host("laptop")).unwrap_err();

        assert!(matches!(error, HostError::InvalidPackage { .. }));
    }

    #[test]
    fn a_profile_that_is_a_directory_is_a_read_error() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("hosts/laptop.toml")).unwrap();

        let error = HostProfile::load(root.path(), &host("laptop")).unwrap_err();

        assert!(matches!(error, HostError::Read { .. }));
    }

    #[test]
    fn the_flag_wins_over_the_environment_and_the_hostname() {
        let chosen = select(
            Some("desktop"),
            Some("laptop"),
            Some("server"),
            &["desktop", "laptop", "server"],
        )
        .unwrap();

        assert_eq!(chosen, selected("desktop", HostSource::Flag));
    }

    #[test]
    fn the_environment_wins_over_the_hostname() {
        let chosen = select(None, Some("laptop"), Some("server"), &["laptop", "server"]).unwrap();

        assert_eq!(chosen, selected("laptop", HostSource::Environment));
    }

    #[test]
    fn the_hostname_is_used_when_a_profile_matches_it() {
        let chosen = select(None, None, Some("lenovo"), &["lenovo", "other"]).unwrap();

        assert_eq!(chosen, selected("lenovo", HostSource::Hostname));
    }

    #[test]
    fn a_hostname_with_no_matching_profile_selects_nothing() {
        let chosen = select(None, None, Some("lenovo"), &["other"]).unwrap();

        assert_eq!(chosen, None);
    }

    #[test]
    fn an_unknown_hostname_selects_nothing_when_there_are_no_profiles() {
        assert_eq!(select(None, None, None, &[]).unwrap(), None);
    }

    #[test]
    fn a_hostname_that_is_not_a_valid_profile_name_is_ignored_not_an_error() {
        let chosen = select(None, None, Some("bad name"), &["lenovo"]).unwrap();

        assert_eq!(chosen, None);
    }

    #[test]
    fn a_flag_naming_a_missing_profile_is_an_error() {
        let error = select(Some("ghost"), None, Some("lenovo"), &["lenovo"]).unwrap_err();

        assert!(matches!(error, HostError::NotFound { ref name, .. } if name.as_str() == "ghost"));
    }

    #[test]
    fn an_environment_value_naming_a_missing_profile_is_an_error() {
        let error = select(None, Some("ghost"), None, &[]).unwrap_err();

        assert!(matches!(error, HostError::NotFound { .. }));
    }

    #[test]
    fn an_explicit_name_that_is_invalid_is_an_error() {
        let error = select(Some("../etc"), None, None, &[]).unwrap_err();

        assert!(matches!(error, HostError::InvalidName(_)));
    }

    #[test]
    fn available_hosts_lists_toml_files_sorted_by_name() {
        let root = tempfile::tempdir().unwrap();
        write_profile(root.path(), "zeta", "");
        write_profile(root.path(), "alpha", "");

        let hosts = available_hosts(root.path()).unwrap();

        assert_eq!(hosts, [host("alpha"), host("zeta")]);
    }

    #[test]
    fn available_hosts_ignores_other_files_and_directories() {
        let root = tempfile::tempdir().unwrap();
        write_profile(root.path(), "laptop", "");
        let hosts_dir = root.path().join("hosts");
        fs::write(hosts_dir.join("README.md"), "").unwrap();
        fs::write(hosts_dir.join(".hidden.toml"), "").unwrap();
        fs::create_dir(hosts_dir.join("folder.toml")).unwrap();

        let hosts = available_hosts(root.path()).unwrap();

        assert_eq!(hosts, [host("laptop")]);
    }

    #[test]
    fn available_hosts_is_empty_when_there_is_no_hosts_directory() {
        let root = tempfile::tempdir().unwrap();

        assert!(available_hosts(root.path()).unwrap().is_empty());
    }

    #[test]
    fn available_hosts_fails_when_hosts_is_a_file() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("hosts"), "").unwrap();

        let error = available_hosts(root.path()).unwrap_err();

        assert!(matches!(error, HostError::Read { .. }));
    }

    #[test]
    fn the_machine_hostname_is_a_non_empty_trimmed_string_when_known() {
        if let Some(name) = machine_hostname() {
            assert!(!name.is_empty());
            assert_eq!(name, name.trim());
        }
    }
}
