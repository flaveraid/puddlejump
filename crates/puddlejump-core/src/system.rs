//! System package managers.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use thiserror::Error;

use crate::graph::DependencyGraph;
use crate::package::{PackageName, Repo};

pub mod command;
pub mod privilege;
pub mod privileged_writer;
pub mod xbps;

/// Every package manager puddlejump knows, in the order auto-detection tries
/// them.
const KNOWN_MANAGERS: [ManagerId; 1] = [ManagerId::Xbps];

/// Identifies a system package manager. Manifests key their system package
/// lists by this id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum ManagerId {
    /// The X Binary Package System, used by Void Linux.
    Xbps,
}

/// A string that is not the id of a known package manager.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown package manager {0:?} (known: {known})", known = known_managers())]
pub struct UnknownManagerError(String);

/// The ids of every known manager, for error messages.
fn known_managers() -> String {
    KNOWN_MANAGERS
        .iter()
        .map(|manager| manager.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

impl ManagerId {
    /// The id as it is written in manifests and config files.
    pub fn as_str(self) -> &'static str {
        match self {
            ManagerId::Xbps => "xbps",
        }
    }

    /// The executable whose presence on `PATH` means this manager is
    /// available.
    pub fn binary(self) -> &'static str {
        match self {
            ManagerId::Xbps => "xbps-install",
        }
    }
}

/// The configured package manager is not available.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("package manager {manager} is configured but {} was not found on PATH", manager.binary())]
pub struct ConfiguredManagerMissingError {
    /// The manager that was configured.
    pub manager: ManagerId,
}

/// Creates the backend for `manager`, running its commands with `runner` and
/// getting root for installs the way `privilege` says.
///
/// This is the one place a [`ManagerId`] becomes an implementation, so adding
/// a manager means a new backend module plus one arm here and in
/// [`ManagerId`]'s conversions.
pub fn build_manager(
    manager: ManagerId,
    runner: impl command::CommandRunner + 'static,
    privilege: privilege::Privilege,
) -> Box<dyn PackageManager> {
    match manager {
        ManagerId::Xbps => Box::new(xbps::Xbps::new(runner, privilege)),
    }
}

/// Decides which package manager to use.
///
/// A `configured` manager wins, but must be available. Otherwise the first
/// known manager whose binary `is_on_path` is used. `None` means no manager
/// is available, which is not an error: linking still works.
///
/// Errors if the configured manager's binary is not available.
pub fn detect(
    configured: Option<ManagerId>,
    is_on_path: &dyn Fn(&str) -> bool,
) -> Result<Option<ManagerId>, ConfiguredManagerMissingError> {
    if let Some(manager) = configured {
        return if is_on_path(manager.binary()) {
            Ok(Some(manager))
        } else {
            Err(ConfiguredManagerMissingError { manager })
        };
    }
    Ok(KNOWN_MANAGERS
        .into_iter()
        .find(|manager| is_on_path(manager.binary())))
}

impl FromStr for ManagerId {
    type Err = UnknownManagerError;

    /// Parses a manager id. Errors if the string is not a known manager.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "xbps" => Ok(ManagerId::Xbps),
            other => Err(UnknownManagerError(other.to_string())),
        }
    }
}

impl fmt::Display for ManagerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A system package manager could not be queried or used.
#[derive(Debug, Error)]
pub enum SystemError {
    /// Asking whether a package is installed failed for a reason other than
    /// the package being absent.
    #[error("cannot check whether {package} is installed: {reason}")]
    Query {
        /// The package that was being checked.
        package: String,
        /// What went wrong.
        reason: String,
    },
    /// Installing packages failed.
    #[error("installing {} failed: {reason}", packages.join(", "))]
    Install {
        /// The packages that were being installed.
        packages: Vec<String>,
        /// What went wrong, such as the manager's error output.
        reason: String,
    },
}

/// A system package manager. Everything specific to one manager lives in its
/// implementation of this trait; nothing else in the crate may assume which
/// manager is in use.
pub trait PackageManager {
    /// Which manager this is, used to pick the right list in a manifest.
    fn id(&self) -> ManagerId;

    /// Whether `package` is installed.
    ///
    /// Errors if the manager cannot answer. A package that is simply not
    /// installed is `Ok(false)`, not an error.
    fn is_installed(&self, package: &str) -> Result<bool, SystemError>;

    /// Installs all of `packages` in a single call, handing each line the
    /// installer writes to `on_line` while it runs.
    ///
    /// Errors if the installation fails.
    fn install(
        &self,
        packages: &[String],
        on_line: command::LineCallback,
    ) -> Result<(), SystemError>;
}

/// What to do about the system packages that packages declare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemPlan {
    /// The manager in use, or `None` if none is available.
    pub manager: Option<ManagerId>,
    /// Packages to install, sorted and without duplicates. Only missing ones.
    pub to_install: Vec<String>,
    /// Declared packages that are already installed, sorted and without
    /// duplicates.
    pub already_installed: Vec<String>,
    /// Declared system packages that are being skipped, and why.
    pub notices: Vec<SystemNotice>,
}

/// A package's declared system packages that cannot be acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SystemNotice {
    /// The package declares system packages but no package manager is available.
    NoManagerAvailable {
        /// The package that declares them.
        package: PackageName,
        /// The managers its manifest has lists for.
        declared: Vec<ManagerId>,
    },
    /// The package declares system packages, but none for the manager in use.
    NoListForManager {
        /// The package that declares them.
        package: PackageName,
        /// The managers its manifest has lists for.
        declared: Vec<ManagerId>,
    },
}

/// Works out which of the system packages declared by the packages in `graph`
/// need installing with `manager`.
///
/// A package with no `[packages]` table declares nothing. If `manager` is
/// `None`, or the package has no list for it, the package's system packages
/// are skipped and reported as a [`SystemNotice`].
///
/// Errors if the manager cannot be queried.
pub fn plan_system(
    repo: &Repo,
    graph: &DependencyGraph,
    manager: Option<&dyn PackageManager>,
) -> Result<SystemPlan, SystemError> {
    let manager_id = manager.map(PackageManager::id);
    let mut wanted = BTreeSet::new();
    let mut notices = Vec::new();
    for name in graph.link_order() {
        let Some(declared) = repo
            .packages
            .get(name)
            .and_then(|package| package.manifest.system_packages.as_ref())
        else {
            continue;
        };
        let declared_managers: Vec<ManagerId> = declared.keys().copied().collect();
        match manager_id {
            None => notices.push(SystemNotice::NoManagerAvailable {
                package: name.clone(),
                declared: declared_managers,
            }),
            Some(id) => match declared.get(&id) {
                Some(packages) => wanted.extend(packages.iter().cloned()),
                None => notices.push(SystemNotice::NoListForManager {
                    package: name.clone(),
                    declared: declared_managers,
                }),
            },
        }
    }
    let mut to_install = Vec::new();
    let mut already_installed = Vec::new();
    if let Some(manager) = manager {
        for package in wanted {
            if manager.is_installed(&package)? {
                already_installed.push(package);
            } else {
                to_install.push(package);
            }
        }
    }
    Ok(SystemPlan {
        manager: manager_id,
        to_install,
        already_installed,
        notices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::package::Package;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// A package manager that knows which packages are installed and records
    /// every query.
    struct FakeManager {
        installed: BTreeSet<&'static str>,
        failing_query: Option<&'static str>,
        queries: RefCell<Vec<String>>,
    }

    fn manager_with(installed: &[&'static str]) -> FakeManager {
        FakeManager {
            installed: installed.iter().copied().collect(),
            failing_query: None,
            queries: RefCell::new(Vec::new()),
        }
    }

    impl PackageManager for FakeManager {
        fn id(&self) -> ManagerId {
            ManagerId::Xbps
        }

        fn is_installed(&self, package: &str) -> Result<bool, SystemError> {
            self.queries.borrow_mut().push(package.to_string());
            if self.failing_query == Some(package) {
                return Err(SystemError::Query {
                    package: package.to_string(),
                    reason: "database locked".to_string(),
                });
            }
            Ok(self.installed.contains(package))
        }

        fn install(
            &self,
            _packages: &[String],
            _on_line: command::LineCallback,
        ) -> Result<(), SystemError> {
            Ok(())
        }
    }

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn strings(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    /// What a package's `[packages]` table looks like in a test.
    enum Declared {
        /// No `[packages]` table at all.
        NoTable,
        /// A `[packages]` table with no manager keys.
        EmptyTable,
        /// A `[packages]` table with these names under `xbps`.
        Xbps(&'static [&'static str]),
    }

    fn repo_with(packages: &[(&str, Declared)]) -> Repo {
        let packages = packages
            .iter()
            .map(|(package, declared)| {
                let system_packages = match declared {
                    Declared::NoTable => None,
                    Declared::EmptyTable => Some(BTreeMap::new()),
                    Declared::Xbps(list) => {
                        Some(BTreeMap::from([(ManagerId::Xbps, strings(list))]))
                    }
                };
                let package = Package {
                    name: name(package),
                    dir: PathBuf::from("/repo/packages").join(package),
                    manifest: Manifest {
                        system_packages,
                        ..Manifest::default()
                    },
                };
                (package.name.clone(), package)
            })
            .collect();
        Repo {
            root: PathBuf::from("/repo"),
            packages,
        }
    }

    fn plan(
        repo: &Repo,
        roots: &[&str],
        manager: Option<&dyn PackageManager>,
    ) -> Result<SystemPlan, SystemError> {
        let roots: Vec<PackageName> = roots.iter().map(|root| name(root)).collect();
        let graph = DependencyGraph::build(repo, &roots).unwrap();
        plan_system(repo, &graph, manager)
    }

    #[test]
    fn missing_packages_are_to_install_and_present_ones_are_already_installed() {
        let repo = repo_with(&[("dwm", Declared::Xbps(&["base-devel", "fd"]))]);
        let manager = manager_with(&["fd"]);

        let plan = plan(&repo, &["dwm"], Some(&manager)).unwrap();

        assert_eq!(plan.manager, Some(ManagerId::Xbps));
        assert_eq!(plan.to_install, strings(&["base-devel"]));
        assert_eq!(plan.already_installed, strings(&["fd"]));
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn packages_declared_by_several_packages_are_listed_once_and_sorted() {
        let repo = repo_with(&[
            ("a", Declared::Xbps(&["git", "curl"])),
            ("b", Declared::Xbps(&["curl", "awk"])),
        ]);
        let manager = manager_with(&[]);

        let plan = plan(&repo, &["a", "b"], Some(&manager)).unwrap();

        assert_eq!(plan.to_install, strings(&["awk", "curl", "git"]));
    }

    #[test]
    fn each_package_is_checked_once_even_if_declared_twice() {
        let repo = repo_with(&[
            ("a", Declared::Xbps(&["curl"])),
            ("b", Declared::Xbps(&["curl"])),
        ]);
        let manager = manager_with(&[]);

        plan(&repo, &["a", "b"], Some(&manager)).unwrap();

        assert_eq!(*manager.queries.borrow(), strings(&["curl"]));
    }

    #[test]
    fn nothing_is_installed_when_everything_is_present() {
        let repo = repo_with(&[("a", Declared::Xbps(&["git"]))]);
        let manager = manager_with(&["git"]);

        let plan = plan(&repo, &["a"], Some(&manager)).unwrap();

        assert!(plan.to_install.is_empty());
        assert_eq!(plan.already_installed, strings(&["git"]));
    }

    #[test]
    fn only_packages_in_the_graph_are_considered() {
        let repo = repo_with(&[
            ("wanted", Declared::Xbps(&["git"])),
            ("unrelated", Declared::Xbps(&["emacs"])),
        ]);
        let manager = manager_with(&[]);

        let plan = plan(&repo, &["wanted"], Some(&manager)).unwrap();

        assert_eq!(plan.to_install, strings(&["git"]));
    }

    #[test]
    fn a_package_without_a_packages_table_declares_nothing() {
        let repo = repo_with(&[("zsh", Declared::NoTable)]);
        let manager = manager_with(&[]);

        let plan = plan(&repo, &["zsh"], Some(&manager)).unwrap();

        assert!(plan.to_install.is_empty());
        assert!(plan.notices.is_empty());
        assert!(manager.queries.borrow().is_empty());
    }

    #[test]
    fn an_empty_list_for_the_manager_installs_nothing_and_gives_no_notice() {
        let repo = repo_with(&[("zsh", Declared::Xbps(&[]))]);
        let manager = manager_with(&[]);

        let plan = plan(&repo, &["zsh"], Some(&manager)).unwrap();

        assert!(plan.to_install.is_empty());
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn a_table_with_no_list_for_the_manager_is_skipped_with_a_notice() {
        let repo = repo_with(&[("dwm", Declared::EmptyTable)]);
        let manager = manager_with(&[]);

        let plan = plan(&repo, &["dwm"], Some(&manager)).unwrap();

        assert!(plan.to_install.is_empty());
        assert_eq!(
            plan.notices,
            [SystemNotice::NoListForManager {
                package: name("dwm"),
                declared: Vec::new()
            }]
        );
    }

    #[test]
    fn no_manager_reports_every_package_that_declares_system_packages() {
        let repo = repo_with(&[
            ("dwm", Declared::Xbps(&["base-devel"])),
            ("zsh", Declared::NoTable),
            ("nvim", Declared::Xbps(&["neovim"])),
        ]);

        let plan = plan(&repo, &["dwm", "zsh", "nvim"], None).unwrap();

        assert_eq!(plan.manager, None);
        assert!(plan.to_install.is_empty());
        assert!(plan.already_installed.is_empty());
        assert_eq!(
            plan.notices,
            [
                SystemNotice::NoManagerAvailable {
                    package: name("dwm"),
                    declared: vec![ManagerId::Xbps]
                },
                SystemNotice::NoManagerAvailable {
                    package: name("nvim"),
                    declared: vec![ManagerId::Xbps]
                },
            ]
        );
    }

    #[test]
    fn a_skipped_package_does_not_stop_others_from_being_planned() {
        let repo = repo_with(&[
            ("skipped", Declared::EmptyTable),
            ("wanted", Declared::Xbps(&["git"])),
        ]);
        let manager = manager_with(&[]);

        let plan = plan(&repo, &["skipped", "wanted"], Some(&manager)).unwrap();

        assert_eq!(plan.to_install, strings(&["git"]));
        assert_eq!(plan.notices.len(), 1);
    }

    #[test]
    fn a_failing_query_is_an_error() {
        let repo = repo_with(&[("a", Declared::Xbps(&["git"]))]);
        let mut manager = manager_with(&[]);
        manager.failing_query = Some("git");

        let error = plan(&repo, &["a"], Some(&manager)).unwrap_err();

        assert!(matches!(error, SystemError::Query { package, .. } if package == "git"));
    }

    #[test]
    fn query_and_install_errors_say_what_failed() {
        let query = SystemError::Query {
            package: "git".to_string(),
            reason: "database locked".to_string(),
        };
        let install = SystemError::Install {
            packages: strings(&["git", "curl"]),
            reason: "no network".to_string(),
        };

        assert_eq!(
            query.to_string(),
            "cannot check whether git is installed: database locked"
        );
        assert_eq!(
            install.to_string(),
            "installing git, curl failed: no network"
        );
    }

    #[test]
    fn build_manager_returns_the_backend_for_the_id() {
        let manager = build_manager(
            ManagerId::Xbps,
            command::fake::FakeCommandRunner::new(),
            privilege::Privilege::AlreadyRoot,
        );

        assert_eq!(manager.id(), ManagerId::Xbps);
    }

    #[test]
    fn build_manager_gives_the_backend_the_runner_and_privilege() {
        use std::rc::Rc;

        /// Lets the test read what the backend ran after handing it over.
        struct Shared(Rc<command::fake::FakeCommandRunner>);
        impl command::CommandRunner for Shared {
            fn run(
                &self,
                program: &str,
                args: &[String],
            ) -> std::io::Result<command::CommandOutput> {
                self.0.run(program, args)
            }

            fn run_streaming(
                &self,
                program: &str,
                args: &[String],
                on_line: command::LineCallback,
            ) -> std::io::Result<command::CommandOutput> {
                self.0.run_streaming(program, args, on_line)
            }
        }
        let runner = Rc::new(command::fake::FakeCommandRunner::new());
        let manager = build_manager(
            ManagerId::Xbps,
            Shared(Rc::clone(&runner)),
            privilege::Privilege::UseSudo,
        );

        manager
            .install(&["git".to_string()], &mut |_, _| {})
            .unwrap();

        assert_eq!(runner.lines(), ["sudo xbps-install -y git"]);
        assert_eq!(runner.streamed(), [true], "the install streams its output");
    }

    #[test]
    fn the_unknown_manager_message_lists_every_known_manager() {
        let error = "nope".parse::<ManagerId>().unwrap_err();

        let expected: Vec<&str> = KNOWN_MANAGERS
            .iter()
            .map(|manager| manager.as_str())
            .collect();
        assert_eq!(
            error.to_string(),
            format!(
                "unknown package manager \"nope\" (known: {})",
                expected.join(", ")
            )
        );
    }

    #[test]
    fn every_known_manager_round_trips_through_its_id_and_has_a_binary() {
        for manager in KNOWN_MANAGERS {
            assert_eq!(manager.as_str().parse::<ManagerId>(), Ok(manager));
            assert!(!manager.binary().is_empty());
        }
    }

    #[test]
    fn auto_detection_finds_a_manager_whose_binary_is_on_path() {
        let found = detect(None, &|binary| binary == "xbps-install").unwrap();
        assert_eq!(found, Some(ManagerId::Xbps));
    }

    #[test]
    fn auto_detection_finds_nothing_when_no_binary_is_on_path() {
        assert_eq!(detect(None, &|_| false).unwrap(), None);
    }

    #[test]
    fn a_configured_manager_that_is_available_is_used() {
        let found = detect(Some(ManagerId::Xbps), &|_| true).unwrap();
        assert_eq!(found, Some(ManagerId::Xbps));
    }

    #[test]
    fn a_configured_manager_wins_without_looking_for_others() {
        let asked = RefCell::new(Vec::new());

        detect(Some(ManagerId::Xbps), &|binary| {
            asked.borrow_mut().push(binary.to_string());
            true
        })
        .unwrap();

        assert_eq!(*asked.borrow(), ["xbps-install"]);
    }

    #[test]
    fn a_configured_manager_that_is_missing_is_an_error_not_a_fallback() {
        let error = detect(Some(ManagerId::Xbps), &|_| false).unwrap_err();

        assert_eq!(error.manager, ManagerId::Xbps);
        assert_eq!(
            error.to_string(),
            "package manager xbps is configured but xbps-install was not found on PATH"
        );
    }

    #[test]
    fn manager_id_round_trips_through_its_string() {
        let id: ManagerId = "xbps".parse().unwrap();
        assert_eq!(id, ManagerId::Xbps);
        assert_eq!(id.to_string(), "xbps");
    }

    #[test]
    fn manager_id_rejects_unknown_names() {
        for name in ["xpbs", "default", "", "XBPS"] {
            assert_eq!(
                name.parse::<ManagerId>(),
                Err(UnknownManagerError(name.to_string()))
            );
        }
    }
}
