//! Packages: the directories in a dotfiles repo that get linked into place.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::manifest::{Manifest, ManifestPathError, MANIFEST_FILE_NAME};

/// Directory under the repo root that holds the packages.
const PACKAGES_DIR_NAME: &str = "packages";

/// Names that are never linked, wherever they appear in a package.
const NEVER_LINKED: [&str; 2] = [".git", ".gitkeep"];

/// The validated name of a package, which is also its directory name under
/// `packages/` in the repo.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PackageName(String);

/// Why a string is not a valid [`PackageName`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackageNameError {
    /// The name was empty.
    #[error("package name is empty")]
    Empty,
    /// The name starts with a dot, which marks hidden directories such as `.git`.
    #[error("package name {0:?} starts with a dot")]
    LeadingDot(String),
    /// The name contains a path separator.
    #[error("package name {0:?} contains a path separator")]
    PathSeparator(String),
    /// The name contains whitespace or a control character.
    #[error("package name {0:?} contains whitespace or a control character")]
    InvalidCharacter(String),
}

impl PackageName {
    /// Validates `name` and wraps it.
    ///
    /// Errors if the name is empty, starts with a dot, contains a path
    /// separator, or contains whitespace or control characters.
    pub fn new(name: &str) -> Result<Self, PackageNameError> {
        if name.is_empty() {
            return Err(PackageNameError::Empty);
        }
        if name.starts_with('.') {
            return Err(PackageNameError::LeadingDot(name.to_string()));
        }
        if name.contains('/') {
            return Err(PackageNameError::PathSeparator(name.to_string()));
        }
        if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(PackageNameError::InvalidCharacter(name.to_string()));
        }
        Ok(Self(name.to_string()))
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A package: a directory of files to link, plus its optional manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The package name (its directory name).
    pub name: PackageName,
    /// The package directory.
    pub dir: PathBuf,
    /// The package manifest, or the default if there is no `manifest.toml`.
    pub manifest: Manifest,
}

/// A dotfiles repo: the packages found under its `packages/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The repo root directory.
    pub root: PathBuf,
    /// All packages, by name.
    pub packages: BTreeMap<PackageName, Package>,
}

/// Why a repo or package could not be read.
#[derive(Debug, Error)]
pub enum PackageError {
    /// A directory or file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A directory under `packages/` is not a valid package name.
    #[error("{path} is not a valid package directory: {source}")]
    InvalidName {
        /// The offending directory.
        path: PathBuf,
        /// Why the name is invalid.
        source: PackageNameError,
    },
    /// A directory name under `packages/` is not valid UTF-8.
    #[error("{0} has a name that is not valid UTF-8")]
    NonUtf8Name(PathBuf),
    /// A package's manifest is invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestPathError),
}

impl Repo {
    /// Discovers every package under `<root>/packages`.
    ///
    /// Plain files and hidden directories (such as `.git`) are ignored. Each
    /// package's `manifest.toml` is loaded if present.
    ///
    /// Errors if `packages/` cannot be read (including when it is missing), a
    /// directory name is not a valid package name, or a manifest is invalid.
    pub fn discover(root: &Path) -> Result<Self, PackageError> {
        let packages_dir = root.join(PACKAGES_DIR_NAME);
        let mut packages = BTreeMap::new();
        for entry in read_dir(&packages_dir)? {
            let path = entry.path();
            if !is_directory_following_symlink(&path)? {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                return Err(PackageError::NonUtf8Name(path));
            };
            if file_name.starts_with('.') {
                continue;
            }
            let name = PackageName::new(file_name).map_err(|source| PackageError::InvalidName {
                path: path.clone(),
                source,
            })?;
            let package = Package::load(name.clone(), path)?;
            packages.insert(name, package);
        }
        Ok(Self {
            root: root.to_path_buf(),
            packages,
        })
    }
}

impl Package {
    /// Loads the package `name` from `dir`, reading `dir/manifest.toml` if it
    /// exists.
    ///
    /// Errors if the manifest exists but cannot be read or is invalid.
    fn load(name: PackageName, dir: PathBuf) -> Result<Self, PackageError> {
        let manifest_path = dir.join(MANIFEST_FILE_NAME);
        let manifest = if manifest_path.is_file() {
            Manifest::load(&manifest_path)?
        } else {
            Manifest::default()
        };
        Ok(Self {
            name,
            dir,
            manifest,
        })
    }

    /// Lists the files to link, as sorted paths relative to the package
    /// directory.
    ///
    /// Symlinks are listed as files and never followed. The root
    /// `manifest.toml`, the hook file, and `.git`/`.gitkeep` anywhere in the
    /// tree are left out. A manifest named `manifest.toml` in a subdirectory
    /// is an ordinary file and is included.
    ///
    /// Errors if a directory in the package cannot be read.
    pub fn files(&self) -> Result<Vec<PathBuf>, PackageError> {
        let mut files = Vec::new();
        self.collect_files(Path::new(""), &mut files)?;
        files.sort();
        Ok(files)
    }

    fn collect_files(
        &self,
        relative_dir: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), PackageError> {
        for entry in read_dir(&self.dir.join(relative_dir))? {
            let relative_path = relative_dir.join(entry.file_name());
            if self.is_excluded(&relative_path) {
                continue;
            }
            if is_directory(&entry.path())? {
                self.collect_files(&relative_path, files)?;
            } else {
                files.push(relative_path);
            }
        }
        Ok(())
    }

    fn is_excluded(&self, relative_path: &Path) -> bool {
        let is_never_linked = relative_path
            .file_name()
            .is_some_and(|name| NEVER_LINKED.iter().any(|never| name == *never));
        is_never_linked
            || relative_path == Path::new(MANIFEST_FILE_NAME)
            || self
                .manifest
                .hook
                .as_deref()
                .is_some_and(|hook| normalize(hook) == relative_path)
    }
}

/// Drops `.` components so that `./build.sh` equals `build.sh`.
fn normalize(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}

fn read_dir(dir: &Path) -> Result<Vec<fs::DirEntry>, PackageError> {
    let read_error = |source| PackageError::Read {
        path: dir.to_path_buf(),
        source,
    };
    fs::read_dir(dir)
        .map_err(read_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(read_error)
}

/// True for a directory or a symlink to one. Used only for the entries
/// directly under `packages/`, so a package may itself be a symlink; nothing
/// inside a package is followed. A broken symlink is an error.
fn is_directory_following_symlink(path: &Path) -> Result<bool, PackageError> {
    fs::metadata(path)
        .map(|metadata| metadata.is_dir())
        .map_err(|source| PackageError::Read {
            path: path.to_path_buf(),
            source,
        })
}

/// True for real directories only; a symlink to a directory is not one, so
/// symlinks are never followed.
fn is_directory(path: &Path) -> Result<bool, PackageError> {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir())
        .map_err(|source| PackageError::Read {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_accepts_ordinary_names() {
        for name in ["zsh", "neo-vim", "sysctl_tweaks", "gcc12", "a.b"] {
            assert_eq!(PackageName::new(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn package_name_rejects_empty() {
        assert_eq!(PackageName::new(""), Err(PackageNameError::Empty));
    }

    #[test]
    fn package_name_rejects_leading_dot() {
        for name in [".git", ".", ".."] {
            assert_eq!(
                PackageName::new(name),
                Err(PackageNameError::LeadingDot(name.to_string()))
            );
        }
    }

    #[test]
    fn package_name_rejects_path_separators() {
        assert_eq!(
            PackageName::new("a/b"),
            Err(PackageNameError::PathSeparator("a/b".to_string()))
        );
    }

    #[test]
    fn package_name_rejects_whitespace_and_control_characters() {
        for name in ["a b", "a\tb", "a\nb", "a\u{0}b"] {
            assert_eq!(
                PackageName::new(name),
                Err(PackageNameError::InvalidCharacter(name.to_string()))
            );
        }
    }

    #[test]
    fn package_name_displays_as_its_string() {
        assert_eq!(PackageName::new("zsh").unwrap().to_string(), "zsh");
    }

    #[test]
    fn package_names_sort_alphabetically() {
        let mut names = [
            PackageName::new("zsh").unwrap(),
            PackageName::new("alacritty").unwrap(),
        ];
        names.sort();
        assert_eq!(names[0].as_str(), "alacritty");
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn repo_with_packages_dir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(PACKAGES_DIR_NAME)).unwrap();
        root
    }

    fn discovered_names(repo: &Repo) -> Vec<&str> {
        repo.packages.keys().map(PackageName::as_str).collect()
    }

    fn files_of(root: &Path, package: &str) -> Vec<PathBuf> {
        let repo = Repo::discover(root).unwrap();
        let name = PackageName::new(package).unwrap();
        repo.packages[&name].files().unwrap()
    }

    fn paths(relative: &[&str]) -> Vec<PathBuf> {
        relative.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn discover_finds_packages_sorted_by_name() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/zsh/.zshrc", "");
        write(root.path(), "packages/nvim/.config/nvim/init.lua", "");

        let repo = Repo::discover(root.path()).unwrap();

        assert_eq!(discovered_names(&repo), ["nvim", "zsh"]);
        assert_eq!(repo.root, root.path());
        let zsh = &repo.packages[&PackageName::new("zsh").unwrap()];
        assert_eq!(zsh.dir, root.path().join("packages/zsh"));
    }

    #[test]
    fn discover_returns_no_packages_for_an_empty_packages_dir() {
        let root = repo_with_packages_dir();
        assert!(Repo::discover(root.path()).unwrap().packages.is_empty());
    }

    #[test]
    fn discover_fails_when_the_packages_dir_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let error = Repo::discover(root.path()).unwrap_err();
        assert!(
            matches!(error, PackageError::Read { path, .. } if path == root.path().join("packages"))
        );
    }

    #[test]
    fn discover_ignores_plain_files_in_the_packages_dir() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/README.md", "");
        write(root.path(), "packages/zsh/.zshrc", "");

        let repo = Repo::discover(root.path()).unwrap();
        assert_eq!(discovered_names(&repo), ["zsh"]);
    }

    #[test]
    fn discover_skips_hidden_directories() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/.git/config", "");
        write(root.path(), "packages/zsh/.zshrc", "");

        let repo = Repo::discover(root.path()).unwrap();
        assert_eq!(discovered_names(&repo), ["zsh"]);
    }

    #[test]
    fn discover_rejects_a_directory_with_an_invalid_package_name() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/my pkg/file", "");

        let error = Repo::discover(root.path()).unwrap_err();
        assert!(matches!(
            error,
            PackageError::InvalidName {
                source: PackageNameError::InvalidCharacter(_),
                ..
            }
        ));
    }

    #[test]
    fn discover_follows_a_symlinked_package_directory() {
        let root = repo_with_packages_dir();
        write(root.path(), "elsewhere/.linkedrc", "");
        std::os::unix::fs::symlink(
            root.path().join("elsewhere"),
            root.path().join("packages/linked"),
        )
        .unwrap();

        let repo = Repo::discover(root.path()).unwrap();

        assert_eq!(discovered_names(&repo), ["linked"]);
        assert_eq!(files_of(root.path(), "linked"), paths(&[".linkedrc"]));
    }

    #[test]
    fn discover_reads_the_manifest_of_a_symlinked_package_directory() {
        let root = repo_with_packages_dir();
        write(root.path(), "elsewhere/manifest.toml", "needs_root = true");
        std::os::unix::fs::symlink(
            root.path().join("elsewhere"),
            root.path().join("packages/linked"),
        )
        .unwrap();

        let repo = Repo::discover(root.path()).unwrap();

        let linked = &repo.packages[&PackageName::new("linked").unwrap()];
        assert!(linked.manifest.needs_root);
    }

    #[test]
    fn discover_ignores_a_symlink_to_a_plain_file_in_the_packages_dir() {
        let root = repo_with_packages_dir();
        write(root.path(), "elsewhere/notes.txt", "");
        std::os::unix::fs::symlink(
            root.path().join("elsewhere/notes.txt"),
            root.path().join("packages/notes"),
        )
        .unwrap();

        assert!(Repo::discover(root.path()).unwrap().packages.is_empty());
    }

    #[test]
    fn discover_fails_on_a_broken_symlink_in_the_packages_dir() {
        let root = repo_with_packages_dir();
        let link = root.path().join("packages/broken");
        std::os::unix::fs::symlink(root.path().join("missing"), &link).unwrap();

        let error = Repo::discover(root.path()).unwrap_err();
        assert!(matches!(error, PackageError::Read { path, .. } if path == link));
    }

    #[test]
    fn discover_loads_the_manifest_of_each_package() {
        let root = repo_with_packages_dir();
        write(
            root.path(),
            "packages/sysctl/manifest.toml",
            "needs_root = true",
        );
        write(root.path(), "packages/zsh/.zshrc", "");

        let repo = Repo::discover(root.path()).unwrap();

        let sysctl = &repo.packages[&PackageName::new("sysctl").unwrap()];
        let zsh = &repo.packages[&PackageName::new("zsh").unwrap()];
        assert!(sysctl.manifest.needs_root);
        assert_eq!(zsh.manifest, Manifest::default());
    }

    #[test]
    fn discover_reports_an_invalid_manifest_with_its_path() {
        let root = repo_with_packages_dir();
        write(
            root.path(),
            "packages/zsh/manifest.toml",
            "target = \"relative\"",
        );

        let error = Repo::discover(root.path()).unwrap_err();
        let expected = root.path().join("packages/zsh/manifest.toml");
        assert!(matches!(error, PackageError::Manifest(inner) if inner.path == expected));
    }

    #[test]
    fn files_lists_nested_files_sorted_and_relative() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/nvim/.config/nvim/lua/a.lua", "");
        write(root.path(), "packages/nvim/.config/nvim/init.lua", "");
        write(root.path(), "packages/nvim/.nvimrc", "");

        assert_eq!(
            files_of(root.path(), "nvim"),
            paths(&[".config/nvim/init.lua", ".config/nvim/lua/a.lua", ".nvimrc"])
        );
    }

    #[test]
    fn files_of_an_empty_package_is_empty() {
        let root = repo_with_packages_dir();
        fs::create_dir(root.path().join("packages/empty")).unwrap();

        assert!(files_of(root.path(), "empty").is_empty());
    }

    #[test]
    fn files_excludes_the_root_manifest() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/zsh/manifest.toml", "");
        write(root.path(), "packages/zsh/.zshrc", "");

        assert_eq!(files_of(root.path(), "zsh"), paths(&[".zshrc"]));
    }

    #[test]
    fn files_includes_a_manifest_in_a_subdirectory() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/zsh/.config/zsh/manifest.toml", "");

        assert_eq!(
            files_of(root.path(), "zsh"),
            paths(&[".config/zsh/manifest.toml"])
        );
    }

    #[test]
    fn files_excludes_the_hook_file() {
        let root = repo_with_packages_dir();
        write(
            root.path(),
            "packages/dwm/manifest.toml",
            "hook = \"build.sh\"",
        );
        write(root.path(), "packages/dwm/build.sh", "");
        write(root.path(), "packages/dwm/.config/dwm/config.h", "");

        assert_eq!(
            files_of(root.path(), "dwm"),
            paths(&[".config/dwm/config.h"])
        );
    }

    #[test]
    fn files_excludes_a_hook_written_with_a_leading_dot_component() {
        let root = repo_with_packages_dir();
        write(
            root.path(),
            "packages/dwm/manifest.toml",
            "hook = \"./scripts/build.sh\"",
        );
        write(root.path(), "packages/dwm/scripts/build.sh", "");

        assert!(files_of(root.path(), "dwm").is_empty());
    }

    #[test]
    fn files_ignores_a_hook_that_does_not_exist() {
        let root = repo_with_packages_dir();
        write(
            root.path(),
            "packages/dwm/manifest.toml",
            "hook = \"build.sh\"",
        );
        write(root.path(), "packages/dwm/.dwmrc", "");

        assert_eq!(files_of(root.path(), "dwm"), paths(&[".dwmrc"]));
    }

    #[test]
    fn files_excludes_git_metadata_and_gitkeep_at_any_depth() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/zsh/.git/config", "");
        write(root.path(), "packages/zsh/.config/.gitkeep", "");
        write(root.path(), "packages/zsh/.config/zsh/.git/HEAD", "");
        write(root.path(), "packages/zsh/.zshrc", "");

        assert_eq!(files_of(root.path(), "zsh"), paths(&[".zshrc"]));
    }

    #[test]
    fn files_lists_symlinks_without_following_them() {
        let root = repo_with_packages_dir();
        write(root.path(), "elsewhere/inside", "");
        let package_dir = root.path().join("packages/zsh");
        fs::create_dir_all(&package_dir).unwrap();
        std::os::unix::fs::symlink(root.path().join("elsewhere"), package_dir.join("dir-link"))
            .unwrap();
        std::os::unix::fs::symlink(root.path().join("missing"), package_dir.join("broken-link"))
            .unwrap();

        assert_eq!(
            files_of(root.path(), "zsh"),
            paths(&["broken-link", "dir-link"])
        );
    }

    #[test]
    fn files_fails_when_the_package_directory_has_disappeared() {
        let root = repo_with_packages_dir();
        write(root.path(), "packages/zsh/.zshrc", "");
        let repo = Repo::discover(root.path()).unwrap();
        fs::remove_dir_all(root.path().join("packages/zsh")).unwrap();

        let package = &repo.packages[&PackageName::new("zsh").unwrap()];
        assert!(matches!(package.files(), Err(PackageError::Read { .. })));
    }
}
