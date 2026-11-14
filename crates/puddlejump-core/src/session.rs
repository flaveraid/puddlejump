//! A session: the repo, its config, and the host, resolved from what the user
//! asked for. Frontends pass in raw values and never read the environment
//! themselves, so everything here is deterministic.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{ConfigError, RepoConfig};
use crate::host::{available_hosts, select_host, HostError, HostName, HostProfile, SelectedHost};
use crate::package::{PackageError, PackageName, PackageNameError, Repo};
use crate::plan::PlanContext;

/// What the user gave, and what the environment says, for opening a session.
#[derive(Debug, Clone, Copy)]
pub struct SessionInputs<'a> {
    /// The `--repo` flag.
    pub repo_flag: Option<&'a Path>,
    /// The `PUDDLEJUMP_REPO` environment variable.
    pub repo_environment: Option<&'a Path>,
    /// The current directory, used when no repo is given.
    pub current_dir: &'a Path,
    /// What `~` means in a manifest target.
    pub home: &'a Path,
    /// The `--target` flag, which replaces every package's target.
    pub target_override: Option<&'a Path>,
    /// The `--host` flag.
    pub host_flag: Option<&'a str>,
    /// The `PUDDLEJUMP_HOST` environment variable.
    pub host_environment: Option<&'a str>,
    /// The machine's hostname, if known.
    pub hostname: Option<&'a str>,
}

/// Why a session could not be opened or packages could not be chosen.
#[derive(Debug, Error)]
pub enum SessionError {
    /// The repo's packages could not be read.
    #[error(transparent)]
    Repo(#[from] PackageError),
    /// The repo config could not be read.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The host profile could not be found or read.
    #[error(transparent)]
    Host(#[from] HostError),
    /// A package argument is not a valid package name.
    #[error("invalid package name: {0}")]
    InvalidPackage(#[from] PackageNameError),
}

/// Which packages a command applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageChoice {
    /// The packages named on the command line.
    Named(Vec<PackageName>),
    /// The packages listed by the host profile.
    FromHost {
        /// The profile they came from.
        host: HostName,
        /// Its packages.
        packages: Vec<PackageName>,
    },
    /// Every package in the repo. Linking everything needs confirmation.
    Everything(Vec<PackageName>),
}

impl PackageChoice {
    /// The chosen packages.
    pub fn packages(&self) -> &[PackageName] {
        match self {
            PackageChoice::Named(packages)
            | PackageChoice::FromHost { packages, .. }
            | PackageChoice::Everything(packages) => packages,
        }
    }
}

/// A repo opened for use, with its config and selected host.
#[derive(Debug)]
pub struct Session {
    /// The repo and its packages.
    pub repo: Repo,
    /// The repo config.
    pub config: RepoConfig,
    /// The host profile in use, if one was selected.
    pub host: Option<SelectedHost>,
    /// The host profiles in the repo.
    pub available_hosts: Vec<HostName>,
    home: PathBuf,
    target_override: Option<PathBuf>,
}

impl Session {
    /// Opens the repo from `--repo`, else `PUDDLEJUMP_REPO`, else the current
    /// directory, and reads its packages, config, and host selection.
    ///
    /// Errors if the packages or config cannot be read, or a host named by
    /// flag or environment has no profile.
    pub fn open(inputs: &SessionInputs) -> Result<Self, SessionError> {
        let root = inputs
            .repo_flag
            .or(inputs.repo_environment)
            .unwrap_or(inputs.current_dir);
        let repo = Repo::discover(root)?;
        let config = RepoConfig::load(root)?;
        let available_hosts = available_hosts(root)?;
        let host = select_host(
            inputs.host_flag,
            inputs.host_environment,
            inputs.hostname,
            &available_hosts,
            root,
        )?;
        Ok(Self {
            repo,
            config,
            host,
            available_hosts,
            home: inputs.home.to_path_buf(),
            target_override: inputs.target_override.map(Path::to_path_buf),
        })
    }

    /// The planning context for this session.
    pub fn plan_context(&self) -> PlanContext<'_> {
        PlanContext {
            repo: &self.repo,
            home: &self.home,
            target_override: self.target_override.as_deref(),
        }
    }

    /// Decides which packages a command applies to: those named, else the
    /// host profile's, else (only if `allow_everything`) every package.
    ///
    /// Errors if a name is not a valid package name or the host profile cannot
    /// be read. With nothing named, no host profile, and `allow_everything`
    /// false, the result is an empty [`PackageChoice::Named`].
    pub fn choose_packages(
        &self,
        named: &[String],
        allow_everything: bool,
    ) -> Result<PackageChoice, SessionError> {
        if !named.is_empty() {
            let packages = named
                .iter()
                .map(|name| PackageName::new(name))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(PackageChoice::Named(packages));
        }
        if let Some(selected) = &self.host {
            let profile = HostProfile::load(&self.repo.root, &selected.name)?;
            return Ok(PackageChoice::FromHost {
                host: profile.name,
                packages: profile.packages,
            });
        }
        if allow_everything {
            return Ok(PackageChoice::Everything(
                self.repo.packages.keys().cloned().collect(),
            ));
        }
        Ok(PackageChoice::Named(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn names(texts: &[&str]) -> Vec<PackageName> {
        texts.iter().map(|text| name(text)).collect()
    }

    fn strings(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    /// A repo directory with the given packages (each with one file).
    fn repo_with(packages: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("packages")).unwrap();
        for package in packages {
            let dir = root.path().join("packages").join(package);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join(".rc"), "").unwrap();
        }
        root
    }

    fn write_host(root: &Path, host: &str, packages: &[&str]) {
        fs::create_dir_all(root.join("hosts")).unwrap();
        let list: Vec<String> = packages.iter().map(|p| format!("{p:?}")).collect();
        fs::write(
            root.join(format!("hosts/{host}.toml")),
            format!("packages = [{}]", list.join(", ")),
        )
        .unwrap();
    }

    struct Inputs {
        repo_flag: Option<PathBuf>,
        repo_environment: Option<PathBuf>,
        current_dir: PathBuf,
        host_flag: Option<String>,
        host_environment: Option<String>,
        hostname: Option<String>,
        target_override: Option<PathBuf>,
    }

    fn inputs(current_dir: &Path) -> Inputs {
        Inputs {
            repo_flag: None,
            repo_environment: None,
            current_dir: current_dir.to_path_buf(),
            host_flag: None,
            host_environment: None,
            hostname: None,
            target_override: None,
        }
    }

    fn open(inputs: &Inputs) -> Result<Session, SessionError> {
        Session::open(&SessionInputs {
            repo_flag: inputs.repo_flag.as_deref(),
            repo_environment: inputs.repo_environment.as_deref(),
            current_dir: &inputs.current_dir,
            home: Path::new("/home/me"),
            target_override: inputs.target_override.as_deref(),
            host_flag: inputs.host_flag.as_deref(),
            host_environment: inputs.host_environment.as_deref(),
            hostname: inputs.hostname.as_deref(),
        })
    }

    #[test]
    fn the_repo_defaults_to_the_current_directory() {
        let root = repo_with(&["zsh"]);

        let session = open(&inputs(root.path())).unwrap();

        assert_eq!(session.repo.root, root.path());
        assert_eq!(session.repo.packages.len(), 1);
    }

    #[test]
    fn the_environment_repo_beats_the_current_directory() {
        let cwd = repo_with(&["from-cwd"]);
        let env = repo_with(&["from-env"]);
        let mut given = inputs(cwd.path());
        given.repo_environment = Some(env.path().to_path_buf());

        let session = open(&given).unwrap();

        assert!(session.repo.packages.contains_key(&name("from-env")));
    }

    #[test]
    fn the_repo_flag_beats_the_environment_and_the_current_directory() {
        let cwd = repo_with(&["from-cwd"]);
        let env = repo_with(&["from-env"]);
        let flag = repo_with(&["from-flag"]);
        let mut given = inputs(cwd.path());
        given.repo_environment = Some(env.path().to_path_buf());
        given.repo_flag = Some(flag.path().to_path_buf());

        let session = open(&given).unwrap();

        assert!(session.repo.packages.contains_key(&name("from-flag")));
    }

    #[test]
    fn a_directory_that_is_not_a_repo_is_an_error() {
        let root = tempfile::tempdir().unwrap();

        let error = open(&inputs(root.path())).unwrap_err();

        assert!(matches!(error, SessionError::Repo(_)));
    }

    #[test]
    fn the_config_is_read_from_the_repo() {
        let root = repo_with(&["zsh"]);
        fs::write(
            root.path().join("puddlejump.toml"),
            "package_manager = \"xbps\"",
        )
        .unwrap();

        let session = open(&inputs(root.path())).unwrap();

        assert!(session.config.package_manager.is_some());
    }

    #[test]
    fn an_invalid_config_is_an_error() {
        let root = repo_with(&["zsh"]);
        fs::write(root.path().join("puddlejump.toml"), "nope = 1").unwrap();

        let error = open(&inputs(root.path())).unwrap_err();

        assert!(matches!(error, SessionError::Config(_)));
    }

    #[test]
    fn the_host_is_selected_from_the_hostname_when_a_profile_matches() {
        let root = repo_with(&["zsh"]);
        write_host(root.path(), "lenovo", &["zsh"]);
        let mut given = inputs(root.path());
        given.hostname = Some("lenovo".to_string());

        let session = open(&given).unwrap();

        assert_eq!(session.host.unwrap().name.as_str(), "lenovo");
        assert_eq!(session.available_hosts.len(), 1);
    }

    #[test]
    fn a_host_flag_naming_a_missing_profile_is_an_error() {
        let root = repo_with(&["zsh"]);
        let mut given = inputs(root.path());
        given.host_flag = Some("ghost".to_string());

        let error = open(&given).unwrap_err();

        assert!(matches!(error, SessionError::Host(_)));
    }

    #[test]
    fn the_plan_context_carries_the_home_and_target_override() {
        let root = repo_with(&["zsh"]);
        let mut given = inputs(root.path());
        given.target_override = Some(PathBuf::from("/stage"));
        let session = open(&given).unwrap();

        let context = session.plan_context();

        assert_eq!(context.home, Path::new("/home/me"));
        assert_eq!(context.target_override, Some(Path::new("/stage")));
        assert_eq!(context.repo.root, root.path());
    }

    #[test]
    fn named_packages_win_over_the_host_and_everything() {
        let root = repo_with(&["zsh", "nvim"]);
        write_host(root.path(), "lenovo", &["nvim"]);
        let mut given = inputs(root.path());
        given.hostname = Some("lenovo".to_string());
        let session = open(&given).unwrap();

        let choice = session.choose_packages(&strings(&["zsh"]), true).unwrap();

        assert_eq!(choice, PackageChoice::Named(names(&["zsh"])));
    }

    #[test]
    fn with_nothing_named_the_host_profile_supplies_the_packages() {
        let root = repo_with(&["zsh", "nvim"]);
        write_host(root.path(), "lenovo", &["nvim", "zsh"]);
        let mut given = inputs(root.path());
        given.hostname = Some("lenovo".to_string());
        let session = open(&given).unwrap();

        let choice = session.choose_packages(&[], true).unwrap();

        assert_eq!(
            choice,
            PackageChoice::FromHost {
                host: HostName::new("lenovo").unwrap(),
                packages: names(&["nvim", "zsh"])
            }
        );
    }

    #[test]
    fn with_no_host_and_everything_allowed_every_package_is_chosen_sorted() {
        let root = repo_with(&["zsh", "nvim", "dwm"]);
        let session = open(&inputs(root.path())).unwrap();

        let choice = session.choose_packages(&[], true).unwrap();

        assert_eq!(
            choice,
            PackageChoice::Everything(names(&["dwm", "nvim", "zsh"]))
        );
    }

    #[test]
    fn with_no_host_and_everything_not_allowed_nothing_is_chosen() {
        let root = repo_with(&["zsh"]);
        let session = open(&inputs(root.path())).unwrap();

        let choice = session.choose_packages(&[], false).unwrap();

        assert_eq!(choice, PackageChoice::Named(Vec::new()));
        assert!(choice.packages().is_empty());
    }

    #[test]
    fn a_host_profile_is_used_even_when_everything_is_not_allowed() {
        let root = repo_with(&["zsh"]);
        write_host(root.path(), "lenovo", &["zsh"]);
        let mut given = inputs(root.path());
        given.hostname = Some("lenovo".to_string());
        let session = open(&given).unwrap();

        let choice = session.choose_packages(&[], false).unwrap();

        assert_eq!(choice.packages(), names(&["zsh"]));
    }

    #[test]
    fn an_invalid_package_argument_is_an_error() {
        let root = repo_with(&["zsh"]);
        let session = open(&inputs(root.path())).unwrap();

        let error = session
            .choose_packages(&strings(&["a/b"]), false)
            .unwrap_err();

        assert!(matches!(error, SessionError::InvalidPackage(_)));
    }

    #[test]
    fn a_host_profile_that_cannot_be_parsed_is_an_error_when_packages_are_chosen() {
        let root = repo_with(&["zsh"]);
        fs::create_dir(root.path().join("hosts")).unwrap();
        fs::write(root.path().join("hosts/lenovo.toml"), "packages = ").unwrap();
        let mut given = inputs(root.path());
        given.hostname = Some("lenovo".to_string());
        let session = open(&given).unwrap();

        let error = session.choose_packages(&[], true).unwrap_err();

        assert!(matches!(error, SessionError::Host(_)));
    }

    #[test]
    fn package_choice_exposes_its_packages_for_every_variant() {
        let packages = names(&["a", "b"]);
        let host = HostName::new("h").unwrap();

        assert_eq!(PackageChoice::Named(packages.clone()).packages(), packages);
        assert_eq!(
            PackageChoice::FromHost {
                host,
                packages: packages.clone()
            }
            .packages(),
            packages
        );
        assert_eq!(
            PackageChoice::Everything(packages.clone()).packages(),
            packages
        );
    }
}
