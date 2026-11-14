//! Package manifests: the optional `manifest.toml` at a package's root.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::package::{PackageName, PackageNameError};
use crate::system::{ManagerId, UnknownManagerError};

/// The only filename a package manifest may have, at the package root.
pub const MANIFEST_FILE_NAME: &str = "manifest.toml";

/// Where a package's files are linked to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The user's home directory (`~` in the manifest). This is the default.
    Home,
    /// An absolute path such as `/etc`.
    Absolute(PathBuf),
}

/// A validated package manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Where the package's files are linked to.
    pub target: Target,
    /// Whether the package must be linked with root privileges.
    pub needs_root: bool,
    /// Other packages that must be linked before this one.
    pub depends: Vec<PackageName>,
    /// Post-link script, relative to the package directory.
    pub hook: Option<PathBuf>,
    /// System packages keyed by package manager. `None` means the manifest
    /// has no `[packages]` table, which is different from an empty table.
    pub system_packages: Option<BTreeMap<ManagerId, Vec<String>>>,
}

/// Why a manifest could not be read or is invalid.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// The manifest file could not be read.
    #[error("cannot read file: {0}")]
    Read(#[from] io::Error),
    /// The manifest is not valid TOML, or has unknown or mistyped keys.
    #[error("invalid manifest: {0}")]
    Syntax(#[from] toml::de::Error),
    /// `target` is neither `~` nor an absolute path.
    #[error("target {0:?} must be \"~\" or an absolute path")]
    InvalidTarget(String),
    /// `hook` is empty, absolute, or escapes the package directory.
    #[error("hook {0:?} must be a non-empty path inside the package directory")]
    InvalidHook(String),
    /// A `depends` entry is not a valid package name.
    #[error("invalid dependency: {0}")]
    InvalidDependency(#[from] PackageNameError),
    /// A key under `[packages]` is not a known package manager.
    #[error("invalid [packages] key: {0}")]
    UnknownManager(#[from] UnknownManagerError),
}

/// A [`ManifestError`] together with the path of the manifest it came from.
#[derive(Debug, Error)]
#[error("{}: {source}", path.display())]
pub struct ManifestPathError {
    /// The manifest path.
    pub path: PathBuf,
    /// What is wrong with it.
    pub source: ManifestError,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    target: Option<String>,
    #[serde(default)]
    needs_root: bool,
    #[serde(default)]
    depends: Vec<String>,
    hook: Option<String>,
    packages: Option<BTreeMap<String, Vec<String>>>,
}

impl Default for Manifest {
    /// The manifest of a package that has no `manifest.toml`.
    fn default() -> Self {
        Self {
            target: Target::Home,
            needs_root: false,
            depends: Vec::new(),
            hook: None,
            system_packages: None,
        }
    }
}

impl Manifest {
    /// Parses and validates manifest text.
    ///
    /// Errors on invalid TOML, unknown keys, a `target` that is not `~` or
    /// absolute, a `hook` that is empty, absolute, or leaves the package
    /// directory, an invalid `depends` name, or an unknown manager id under
    /// `[packages]`.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(text)?;
        Ok(Self {
            target: parse_target(raw.target)?,
            needs_root: raw.needs_root,
            depends: parse_depends(&raw.depends)?,
            hook: raw.hook.map(|hook| parse_hook(&hook)).transpose()?,
            system_packages: raw.packages.map(parse_system_packages).transpose()?,
        })
    }

    /// Reads and parses the manifest file at `path`.
    ///
    /// Errors if the file cannot be read or its contents are invalid; the
    /// error names `path`.
    pub fn load(path: &Path) -> Result<Self, ManifestPathError> {
        let at_path = |source: ManifestError| ManifestPathError {
            path: path.to_path_buf(),
            source,
        };
        let text = fs::read_to_string(path).map_err(|error| at_path(error.into()))?;
        Self::parse(&text).map_err(at_path)
    }
}

fn parse_target(target: Option<String>) -> Result<Target, ManifestError> {
    match target {
        None => Ok(Target::Home),
        Some(text) if text == "~" => Ok(Target::Home),
        Some(text) if Path::new(&text).is_absolute() => Ok(Target::Absolute(PathBuf::from(text))),
        Some(text) => Err(ManifestError::InvalidTarget(text)),
    }
}

fn parse_depends(names: &[String]) -> Result<Vec<PackageName>, ManifestError> {
    names
        .iter()
        .map(|name| PackageName::new(name).map_err(ManifestError::from))
        .collect()
}

fn parse_hook(hook: &str) -> Result<PathBuf, ManifestError> {
    let path = Path::new(hook);
    let stays_inside_package = path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    let has_a_file_name = path
        .components()
        .any(|component| matches!(component, Component::Normal(_)));
    if stays_inside_package && has_a_file_name {
        Ok(path.to_path_buf())
    } else {
        Err(ManifestError::InvalidHook(hook.to_string()))
    }
}

fn parse_system_packages(
    raw: BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<ManagerId, Vec<String>>, ManifestError> {
    raw.into_iter()
        .map(|(manager, names)| Ok((manager.parse::<ManagerId>()?, names)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_manifest_has_defaults() {
        assert_eq!(Manifest::parse("").unwrap(), Manifest::default());
    }

    #[test]
    fn default_manifest_targets_home_with_nothing_declared() {
        let manifest = Manifest::default();
        assert_eq!(manifest.target, Target::Home);
        assert!(!manifest.needs_root);
        assert!(manifest.depends.is_empty());
        assert_eq!(manifest.hook, None);
        assert_eq!(manifest.system_packages, None);
    }

    #[test]
    fn full_manifest_parses_every_field() {
        let manifest = Manifest::parse(
            r#"
            target = "/etc"
            needs_root = true
            depends = ["zsh", "fonts"]
            hook = "scripts/build.sh"

            [packages]
            xbps = ["base-devel", "fd"]
            "#,
        )
        .unwrap();

        assert_eq!(manifest.target, Target::Absolute(PathBuf::from("/etc")));
        assert!(manifest.needs_root);
        assert_eq!(
            manifest.depends,
            vec![
                PackageName::new("zsh").unwrap(),
                PackageName::new("fonts").unwrap()
            ]
        );
        assert_eq!(manifest.hook, Some(PathBuf::from("scripts/build.sh")));
        assert_eq!(
            manifest.system_packages,
            Some(BTreeMap::from([(
                ManagerId::Xbps,
                vec!["base-devel".to_string(), "fd".to_string()]
            )]))
        );
    }

    #[test]
    fn tilde_target_means_home() {
        let manifest = Manifest::parse(r#"target = "~""#).unwrap();
        assert_eq!(manifest.target, Target::Home);
    }

    #[test]
    fn relative_target_is_rejected() {
        let error = Manifest::parse(r#"target = "etc""#).unwrap_err();
        assert!(matches!(error, ManifestError::InvalidTarget(text) if text == "etc"));
    }

    #[test]
    fn tilde_subdirectory_target_is_rejected() {
        let error = Manifest::parse(r#"target = "~/bin""#).unwrap_err();
        assert!(matches!(error, ManifestError::InvalidTarget(_)));
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let error = Manifest::parse("colour = \"red\"").unwrap_err();
        assert!(matches!(error, ManifestError::Syntax(_)));
    }

    #[test]
    fn wrongly_typed_key_is_rejected() {
        let error = Manifest::parse(r#"needs_root = "yes""#).unwrap_err();
        assert!(matches!(error, ManifestError::Syntax(_)));
    }

    #[test]
    fn invalid_toml_is_rejected() {
        let error = Manifest::parse("target = ").unwrap_err();
        assert!(matches!(error, ManifestError::Syntax(_)));
    }

    #[test]
    fn unknown_manager_key_is_rejected() {
        let error = Manifest::parse("[packages]\nxpbs = [\"git\"]").unwrap_err();
        assert!(matches!(error, ManifestError::UnknownManager(_)));
    }

    #[test]
    fn default_key_under_packages_is_rejected() {
        let error = Manifest::parse("[packages]\ndefault = [\"git\"]").unwrap_err();
        assert!(matches!(error, ManifestError::UnknownManager(_)));
    }

    #[test]
    fn empty_manager_list_is_valid() {
        let manifest = Manifest::parse("[packages]\nxbps = []").unwrap();
        assert_eq!(
            manifest.system_packages,
            Some(BTreeMap::from([(ManagerId::Xbps, Vec::new())]))
        );
    }

    #[test]
    fn empty_packages_table_differs_from_no_packages_table() {
        let with_table = Manifest::parse("[packages]").unwrap();
        assert_eq!(with_table.system_packages, Some(BTreeMap::new()));
        assert_eq!(Manifest::parse("").unwrap().system_packages, None);
    }

    #[test]
    fn invalid_dependency_name_is_rejected() {
        let error = Manifest::parse(r#"depends = ["a/b"]"#).unwrap_err();
        assert!(matches!(error, ManifestError::InvalidDependency(_)));
    }

    #[test]
    fn hook_may_be_a_relative_path_inside_the_package() {
        for hook in ["build.sh", "scripts/build.sh", "./build.sh"] {
            let manifest = Manifest::parse(&format!("hook = {hook:?}")).unwrap();
            assert_eq!(manifest.hook, Some(PathBuf::from(hook)));
        }
    }

    #[test]
    fn hook_that_is_empty_absolute_or_escapes_is_rejected() {
        for hook in ["", "/usr/bin/true", "../build.sh", "a/../../b.sh", "."] {
            let error = Manifest::parse(&format!("hook = {hook:?}")).unwrap_err();
            assert!(
                matches!(error, ManifestError::InvalidHook(_)),
                "hook {hook:?} should be rejected"
            );
        }
    }

    #[test]
    fn load_reads_a_manifest_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(MANIFEST_FILE_NAME);
        fs::write(&path, "needs_root = true").unwrap();

        assert!(Manifest::load(&path).unwrap().needs_root);
    }

    #[test]
    fn load_reports_the_path_of_a_missing_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(MANIFEST_FILE_NAME);

        let error = Manifest::load(&path).unwrap_err();
        assert_eq!(error.path, path);
        assert!(matches!(error.source, ManifestError::Read(_)));
        assert!(error.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn load_reports_the_path_of_an_invalid_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(MANIFEST_FILE_NAME);
        fs::write(&path, "target = \"relative\"").unwrap();

        let error = Manifest::load(&path).unwrap_err();
        assert_eq!(error.path, path);
        assert!(matches!(error.source, ManifestError::InvalidTarget(_)));
    }
}
