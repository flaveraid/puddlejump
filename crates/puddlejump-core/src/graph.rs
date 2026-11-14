//! The package dependency graph: closure, cycle detection, and link order.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::package::{PackageName, Repo};

/// The packages to link (the requested roots plus everything they depend on,
/// transitively) and the dependencies between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    dependencies: BTreeMap<PackageName, Vec<PackageName>>,
}

/// Why a dependency graph could not be built.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    /// A requested package, or a dependency of one, is not in the repo.
    #[error("{}", missing_message(.package, .required_by))]
    MissingPackage {
        /// The package that does not exist.
        package: PackageName,
        /// The package that depends on it, or `None` if it was requested directly.
        required_by: Option<PackageName>,
    },
    /// Packages depend on each other in a loop.
    #[error("dependency cycle: {}", join_cycle(.path))]
    Cycle {
        /// The packages in the cycle, in order. The first and last are the same.
        path: Vec<PackageName>,
    },
}

/// A requested package is not in the repo.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("package {0} does not exist")]
pub struct UnknownPackageError(pub PackageName);

fn missing_message(package: &PackageName, required_by: &Option<PackageName>) -> String {
    match required_by {
        Some(parent) => format!("package {package} (required by {parent}) does not exist"),
        None => format!("package {package} does not exist"),
    }
}

fn join_cycle(path: &[PackageName]) -> String {
    path.iter()
        .map(PackageName::as_str)
        .collect::<Vec<_>>()
        .join(" -> ")
}

impl DependencyGraph {
    /// Builds the graph for `roots` from the dependencies declared in the
    /// repo's manifests. Duplicate roots are collapsed.
    ///
    /// Errors with [`GraphError::MissingPackage`] if a root or a dependency is
    /// not in the repo, and with [`GraphError::Cycle`] if dependencies loop
    /// (including a package depending on itself).
    pub fn build(repo: &Repo, roots: &[PackageName]) -> Result<Self, GraphError> {
        let declared = repo
            .packages
            .iter()
            .map(|(name, package)| (name.clone(), package.manifest.depends.clone()))
            .collect();
        Self::from_declared(&declared, roots)
    }

    /// Builds a graph of exactly `packages`, ignoring their declared
    /// dependencies, for operations that must not pull in anything else.
    /// Duplicates are collapsed.
    ///
    /// Errors with [`UnknownPackageError`] if a package is not in the repo.
    pub fn without_dependencies(
        repo: &Repo,
        packages: &[PackageName],
    ) -> Result<Self, UnknownPackageError> {
        let mut dependencies = BTreeMap::new();
        for package in packages {
            if !repo.packages.contains_key(package) {
                return Err(UnknownPackageError(package.clone()));
            }
            dependencies.insert(package.clone(), Vec::new());
        }
        Ok(Self { dependencies })
    }

    /// Builds the graph from a map of every known package to its declared
    /// dependencies. Kept separate from [`DependencyGraph::build`] so the
    /// algorithm needs no repo.
    fn from_declared(
        declared: &BTreeMap<PackageName, Vec<PackageName>>,
        roots: &[PackageName],
    ) -> Result<Self, GraphError> {
        let mut dependencies = BTreeMap::new();
        let mut pending: Vec<(PackageName, Option<PackageName>)> =
            roots.iter().map(|root| (root.clone(), None)).collect();
        while let Some((name, required_by)) = pending.pop() {
            if dependencies.contains_key(&name) {
                continue;
            }
            let Some(declared_dependencies) = declared.get(&name) else {
                return Err(GraphError::MissingPackage {
                    package: name,
                    required_by,
                });
            };
            for dependency in declared_dependencies {
                pending.push((dependency.clone(), Some(name.clone())));
            }
            dependencies.insert(name, declared_dependencies.clone());
        }
        let graph = Self { dependencies };
        graph.check_for_cycles()?;
        Ok(graph)
    }

    /// The package names in the graph, sorted.
    pub fn packages(&self) -> impl Iterator<Item = &PackageName> {
        self.dependencies.keys()
    }

    /// The direct dependencies of `package`, in declaration order, or `None`
    /// if the package is not in the graph.
    pub fn dependencies_of(&self, package: &PackageName) -> Option<&[PackageName]> {
        self.dependencies.get(package).map(Vec::as_slice)
    }

    /// Every package in the graph with its dependencies first. Packages that
    /// do not constrain each other are ordered by name, so the result is
    /// deterministic.
    pub fn link_order(&self) -> Vec<&PackageName> {
        let mut unmet: BTreeMap<&PackageName, BTreeSet<&PackageName>> = self
            .dependencies
            .iter()
            .map(|(name, dependencies)| (name, dependencies.iter().collect()))
            .collect();
        let mut order = Vec::with_capacity(unmet.len());
        // The graph is acyclic (checked at build time), so some package always
        // has no unmet dependencies until none are left.
        while let Some(next) = unmet
            .iter()
            .find(|(_, dependencies)| dependencies.is_empty())
            .map(|(name, _)| *name)
        {
            unmet.remove(next);
            for dependencies in unmet.values_mut() {
                dependencies.remove(next);
            }
            order.push(next);
        }
        order
    }

    /// Depth-first search that tracks the current path so a cycle can be
    /// reported as `a -> b -> a`.
    fn check_for_cycles(&self) -> Result<(), GraphError> {
        let mut finished = BTreeSet::new();
        for start in self.dependencies.keys() {
            let mut path = Vec::new();
            self.visit(start, &mut path, &mut finished)?;
        }
        Ok(())
    }

    fn visit<'a>(
        &'a self,
        package: &'a PackageName,
        path: &mut Vec<&'a PackageName>,
        finished: &mut BTreeSet<&'a PackageName>,
    ) -> Result<(), GraphError> {
        if finished.contains(package) {
            return Ok(());
        }
        if let Some(position) = path.iter().position(|visited| *visited == package) {
            let mut cycle: Vec<PackageName> = path[position..]
                .iter()
                .map(|name| (*name).clone())
                .collect();
            cycle.push(package.clone());
            return Err(GraphError::Cycle { path: cycle });
        }
        path.push(package);
        for dependency in &self.dependencies[package] {
            self.visit(dependency, path, finished)?;
        }
        path.pop();
        finished.insert(package);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::package::Package;
    use std::path::PathBuf;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn names(texts: &[&str]) -> Vec<PackageName> {
        texts.iter().map(|text| name(text)).collect()
    }

    /// Describes a repo as `(package, dependencies)` pairs.
    fn declared(packages: &[(&str, &[&str])]) -> BTreeMap<PackageName, Vec<PackageName>> {
        packages
            .iter()
            .map(|(package, dependencies)| (name(package), names(dependencies)))
            .collect()
    }

    fn build(packages: &[(&str, &[&str])], roots: &[&str]) -> Result<DependencyGraph, GraphError> {
        DependencyGraph::from_declared(&declared(packages), &names(roots))
    }

    fn order(graph: &DependencyGraph) -> Vec<&str> {
        graph
            .link_order()
            .into_iter()
            .map(PackageName::as_str)
            .collect()
    }

    #[test]
    fn a_package_with_no_dependencies_is_its_own_order() {
        let graph = build(&[("zsh", &[])], &["zsh"]).unwrap();
        assert_eq!(order(&graph), ["zsh"]);
    }

    #[test]
    fn dependencies_come_before_dependents_in_a_chain() {
        let graph = build(
            &[("app", &["lib"]), ("lib", &["base"]), ("base", &[])],
            &["app"],
        )
        .unwrap();
        assert_eq!(order(&graph), ["base", "lib", "app"]);
    }

    #[test]
    fn a_shared_dependency_appears_once_in_a_diamond() {
        let graph = build(
            &[
                ("top", &["left", "right"]),
                ("left", &["base"]),
                ("right", &["base"]),
                ("base", &[]),
            ],
            &["top"],
        )
        .unwrap();
        assert_eq!(order(&graph), ["base", "left", "right", "top"]);
    }

    #[test]
    fn independent_roots_are_ordered_by_name() {
        let graph = build(
            &[("zsh", &[]), ("nvim", &[]), ("dwm", &[])],
            &["zsh", "dwm", "nvim"],
        )
        .unwrap();
        assert_eq!(order(&graph), ["dwm", "nvim", "zsh"]);
    }

    #[test]
    fn order_does_not_depend_on_the_order_of_the_roots() {
        let packages: &[(&str, &[&str])] = &[("a", &["c"]), ("b", &[]), ("c", &[])];
        let forwards = build(packages, &["a", "b"]).unwrap();
        let backwards = build(packages, &["b", "a"]).unwrap();
        assert_eq!(forwards, backwards);
        assert_eq!(order(&forwards), order(&backwards));
    }

    #[test]
    fn a_dependency_that_sorts_later_still_comes_first() {
        let graph = build(&[("a", &["z"]), ("z", &[])], &["a"]).unwrap();
        assert_eq!(order(&graph), ["z", "a"]);
    }

    #[test]
    fn only_the_closure_of_the_roots_is_included() {
        let graph = build(
            &[("app", &["lib"]), ("lib", &[]), ("unrelated", &[])],
            &["app"],
        )
        .unwrap();
        assert_eq!(order(&graph), ["lib", "app"]);
        assert_eq!(graph.dependencies_of(&name("unrelated")), None);
    }

    #[test]
    fn duplicate_roots_are_collapsed() {
        let graph = build(&[("zsh", &[])], &["zsh", "zsh"]).unwrap();
        assert_eq!(order(&graph), ["zsh"]);
    }

    #[test]
    fn a_root_that_is_also_a_dependency_appears_once() {
        let graph = build(&[("app", &["lib"]), ("lib", &[])], &["app", "lib"]).unwrap();
        assert_eq!(order(&graph), ["lib", "app"]);
    }

    #[test]
    fn no_roots_gives_an_empty_graph() {
        let graph = build(&[("zsh", &[])], &[]).unwrap();
        assert!(order(&graph).is_empty());
        assert_eq!(graph.packages().count(), 0);
    }

    #[test]
    fn dependencies_of_lists_direct_dependencies_in_declaration_order() {
        let graph = build(
            &[("app", &["zlib", "alib"]), ("zlib", &[]), ("alib", &[])],
            &["app"],
        )
        .unwrap();
        assert_eq!(
            graph.dependencies_of(&name("app")),
            Some(names(&["zlib", "alib"]).as_slice())
        );
    }

    #[test]
    fn a_missing_root_is_reported_without_a_parent() {
        let error = build(&[("zsh", &[])], &["nvim"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::MissingPackage {
                package: name("nvim"),
                required_by: None
            }
        );
        assert_eq!(error.to_string(), "package nvim does not exist");
    }

    #[test]
    fn a_missing_dependency_names_the_package_that_requires_it() {
        let error = build(&[("app", &["lib"])], &["app"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::MissingPackage {
                package: name("lib"),
                required_by: Some(name("app"))
            }
        );
        assert_eq!(
            error.to_string(),
            "package lib (required by app) does not exist"
        );
    }

    #[test]
    fn a_missing_transitive_dependency_is_reported() {
        let error = build(&[("app", &["lib"]), ("lib", &["base"])], &["app"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::MissingPackage {
                package: name("base"),
                required_by: Some(name("lib"))
            }
        );
    }

    #[test]
    fn a_package_depending_on_itself_is_a_cycle() {
        let error = build(&[("a", &["a"])], &["a"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::Cycle {
                path: names(&["a", "a"])
            }
        );
    }

    #[test]
    fn two_packages_depending_on_each_other_are_a_cycle() {
        let error = build(&[("a", &["b"]), ("b", &["a"])], &["a"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::Cycle {
                path: names(&["a", "b", "a"])
            }
        );
        assert_eq!(error.to_string(), "dependency cycle: a -> b -> a");
    }

    #[test]
    fn a_three_package_cycle_is_reported_in_order() {
        let error = build(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])], &["a"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::Cycle {
                path: names(&["a", "b", "c", "a"])
            }
        );
    }

    #[test]
    fn a_cycle_reached_through_a_non_cyclic_package_omits_the_entry_path() {
        let error = build(&[("app", &["a"]), ("a", &["b"]), ("b", &["a"])], &["app"]).unwrap_err();
        assert_eq!(
            error,
            GraphError::Cycle {
                path: names(&["a", "b", "a"])
            }
        );
    }

    #[test]
    fn a_cycle_outside_the_closure_of_the_roots_is_ignored() {
        let graph = build(&[("app", &[]), ("x", &["y"]), ("y", &["x"])], &["app"]).unwrap();
        assert_eq!(order(&graph), ["app"]);
    }

    #[test]
    fn a_diamond_is_not_mistaken_for_a_cycle() {
        let result = build(
            &[
                ("top", &["left", "right"]),
                ("left", &["base"]),
                ("right", &["base"]),
                ("base", &[]),
            ],
            &["top"],
        );
        assert!(result.is_ok());
    }

    fn repo_with(packages: &[(&str, &[&str])]) -> Repo {
        let packages = packages
            .iter()
            .map(|(package, dependencies)| {
                let manifest = Manifest {
                    depends: names(dependencies),
                    ..Manifest::default()
                };
                let package = Package {
                    name: name(package),
                    dir: PathBuf::from("/repo/packages").join(package),
                    manifest,
                };
                (package.name.clone(), package)
            })
            .collect();
        Repo {
            root: PathBuf::from("/repo"),
            packages,
        }
    }

    #[test]
    fn build_reads_dependencies_from_the_repo_manifests() {
        let repo = repo_with(&[("app", &["lib"]), ("lib", &[]), ("other", &[])]);

        let graph = DependencyGraph::build(&repo, &names(&["app"])).unwrap();

        assert_eq!(order(&graph), ["lib", "app"]);
    }

    #[test]
    fn without_dependencies_includes_only_the_named_packages() {
        let repo = repo_with(&[("app", &["lib"]), ("lib", &[]), ("other", &[])]);

        let graph = DependencyGraph::without_dependencies(&repo, &names(&["app"])).unwrap();

        assert_eq!(order(&graph), ["app"]);
        assert_eq!(graph.dependencies_of(&name("app")), Some(&[][..]));
    }

    #[test]
    fn without_dependencies_orders_by_name_and_collapses_duplicates() {
        let repo = repo_with(&[("a", &["b"]), ("b", &[]), ("c", &[])]);

        let graph =
            DependencyGraph::without_dependencies(&repo, &names(&["c", "b", "a", "b"])).unwrap();

        assert_eq!(order(&graph), ["a", "b", "c"]);
    }

    #[test]
    fn without_dependencies_reports_a_package_not_in_the_repo() {
        let repo = repo_with(&[("a", &[])]);

        let error = DependencyGraph::without_dependencies(&repo, &names(&["missing"])).unwrap_err();

        assert_eq!(error, UnknownPackageError(name("missing")));
    }

    #[test]
    fn build_reports_a_missing_dependency_from_the_repo() {
        let repo = repo_with(&[("app", &["lib"])]);

        let error = DependencyGraph::build(&repo, &names(&["app"])).unwrap_err();

        assert!(matches!(error, GraphError::MissingPackage { .. }));
    }
}
