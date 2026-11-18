//! The command-line interface definition: arguments only, no behavior.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// A dotfiles manager for Linux: Stow-style linking with Make-style dependencies.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// The dotfiles repo (default: $PUDDLEJUMP_REPO, else `repo` in the user
    /// config, else the current directory).
    #[arg(long, global = true, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// Link every package into DIR instead of its own target (for staging).
    #[arg(long, global = true, value_name = "DIR")]
    pub target: Option<PathBuf>,

    /// The host profile to use (default: $PUDDLEJUMP_HOST, else `host` in the
    /// user config, else the hostname).
    #[arg(long, global = true, value_name = "NAME")]
    pub host: Option<String>,

    /// Print the plan or report as JSON and nothing else.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// What puddlejump should do.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Link packages: resolve dependencies, link files, install system
    /// packages, and run hooks.
    Link(LinkArgs),
    /// Remove the links that point at the named packages.
    Unlink(UnlinkArgs),
    /// Remove and re-create links (files only: no installs or hooks).
    Relink(UnlinkArgs),
    /// Run the hooks of the named packages.
    Hook(HookArgs),
    /// Move existing files into a package and link them.
    Adopt(AdoptArgs),
    /// Show what is linked, what is not, and what is in the way.
    Status(StatusArgs),
    /// List the packages and host profiles in the repo.
    List,
    /// Create a new dotfiles repo at PATH (ignores --repo).
    Init(InitArgs),
}

/// Arguments of `link`.
#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Packages to link (default: the host profile, else every package).
    pub packages: Vec<String>,

    /// Show what would happen without changing anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Do not install missing system packages.
    #[arg(long)]
    pub no_install: bool,

    /// Do not run hooks.
    #[arg(long, conflicts_with = "force_hooks")]
    pub no_hooks: bool,

    /// Run every hook even if it already succeeded with its current script.
    #[arg(long)]
    pub force_hooks: bool,

    /// Answer yes to confirmation questions.
    #[arg(short, long)]
    pub yes: bool,
}

/// Arguments of `unlink` and `relink`.
#[derive(Debug, Args)]
pub struct UnlinkArgs {
    /// Packages to act on (default: the host profile).
    pub packages: Vec<String>,

    /// Show what would happen without changing anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Also remove directories left empty by removing links.
    #[arg(long)]
    pub prune: bool,
}

/// Arguments of `hook`.
#[derive(Debug, Args)]
pub struct HookArgs {
    /// Packages whose hooks to run.
    #[arg(required = true)]
    pub packages: Vec<String>,

    /// Show which hooks would run without running them.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Answer yes to confirmation questions.
    #[arg(short, long)]
    pub yes: bool,
}

/// Arguments of `adopt`.
#[derive(Debug, Args)]
pub struct AdoptArgs {
    /// The package to move the files into.
    pub package: String,

    /// Files or directories in the package's target to adopt.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Show what would happen without changing anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

/// Arguments of `init`.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Where to create the repo. Missing directories are created.
    pub path: PathBuf,

    /// Show what would be created without creating anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

/// Arguments of `status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Packages to report on (default: the host profile, else every package).
    pub packages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("puddlejump").chain(arguments.iter().copied()))
    }

    #[test]
    fn link_takes_packages_and_flags() {
        let cli = parse(&[
            "link",
            "zsh",
            "nvim",
            "-n",
            "--no-install",
            "--no-hooks",
            "-y",
        ])
        .unwrap();

        let Command::Link(args) = cli.command else {
            panic!("expected link");
        };
        assert_eq!(args.packages, ["zsh", "nvim"]);
        assert!(args.dry_run && args.no_install && args.no_hooks && args.yes);
    }

    #[test]
    fn link_flags_default_to_off() {
        let Command::Link(args) = parse(&["link"]).unwrap().command else {
            panic!("expected link");
        };

        assert!(args.packages.is_empty());
        assert!(!args.dry_run && !args.no_install && !args.no_hooks && !args.yes);
        assert!(!args.force_hooks);
    }

    #[test]
    fn link_takes_force_hooks() {
        let Command::Link(args) = parse(&["link", "--force-hooks"]).unwrap().command else {
            panic!("expected link");
        };

        assert!(args.force_hooks);
    }

    #[test]
    fn force_hooks_and_no_hooks_contradict_each_other() {
        assert!(parse(&["link", "--force-hooks", "--no-hooks"]).is_err());
    }

    #[test]
    fn global_flags_work_before_or_after_the_subcommand() {
        let before = parse(&["--repo", "/r", "--json", "status"]).unwrap();
        let after = parse(&["status", "--repo", "/r", "--json"]).unwrap();

        for cli in [before, after] {
            assert_eq!(cli.repo, Some(PathBuf::from("/r")));
            assert!(cli.json);
        }
    }

    #[test]
    fn the_target_and_host_flags_are_read() {
        let cli = parse(&["--target", "/stage", "--host", "lenovo", "list"]).unwrap();

        assert_eq!(cli.target, Some(PathBuf::from("/stage")));
        assert_eq!(cli.host.as_deref(), Some("lenovo"));
    }

    #[test]
    fn unlink_and_relink_take_prune() {
        for command in ["unlink", "relink"] {
            let cli = parse(&[command, "zsh", "--prune", "-n"]).unwrap();
            let args = match cli.command {
                Command::Unlink(args) | Command::Relink(args) => args,
                other => panic!("unexpected {other:?}"),
            };
            assert!(args.prune && args.dry_run);
            assert_eq!(args.packages, ["zsh"]);
        }
    }

    #[test]
    fn hook_requires_at_least_one_package() {
        assert!(parse(&["hook"]).is_err());
        assert!(parse(&["hook", "dwm"]).is_ok());
    }

    #[test]
    fn adopt_requires_a_package_and_at_least_one_path() {
        assert!(parse(&["adopt"]).is_err());
        assert!(parse(&["adopt", "zsh"]).is_err());

        let Command::Adopt(args) = parse(&["adopt", "zsh", "/a", "/b", "-n"]).unwrap().command
        else {
            panic!("expected adopt");
        };
        assert_eq!(args.package, "zsh");
        assert_eq!(args.paths, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(args.dry_run);
    }

    #[test]
    fn status_and_list_parse() {
        assert!(matches!(
            parse(&["status", "zsh"]).unwrap().command,
            Command::Status(_)
        ));
        assert!(matches!(parse(&["list"]).unwrap().command, Command::List));
    }

    #[test]
    fn init_requires_a_path() {
        assert!(parse(&["init"]).is_err());
    }

    #[test]
    fn init_takes_a_path_and_dry_run() {
        let Command::Init(args) = parse(&["init", "/new/dotfiles", "-n"]).unwrap().command else {
            panic!("expected init");
        };

        assert_eq!(args.path, PathBuf::from("/new/dotfiles"));
        assert!(args.dry_run);
    }

    #[test]
    fn init_takes_only_one_path() {
        assert!(parse(&["init", "/a", "/b"]).is_err());
    }

    #[test]
    fn init_accepts_but_does_not_need_the_global_repo_flag() {
        assert!(parse(&["--repo", "/ignored", "init", "/new"]).is_ok());
    }

    #[test]
    fn a_subcommand_is_required() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn unknown_commands_and_flags_are_rejected() {
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["link", "--no-such-flag"]).is_err());
    }

    #[test]
    fn mutating_commands_accept_dry_run_but_status_and_list_do_not() {
        for command in [
            &["link", "--dry-run"][..],
            &["unlink", "--dry-run"],
            &["relink", "--dry-run"],
            &["hook", "x", "--dry-run"],
            &["adopt", "x", "/p", "--dry-run"],
            &["init", "/p", "--dry-run"],
        ] {
            assert!(
                parse(command).is_ok(),
                "{command:?} should accept --dry-run"
            );
        }
        assert!(parse(&["status", "--dry-run"]).is_err());
        assert!(parse(&["list", "--dry-run"]).is_err());
    }
}
