//! One function per command: ask core to plan, show it, run it, report.
//! Business decisions live in core; this only sequences and presents.

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use puddlejump_core::adopt::{plan_adopt, AdoptPlan};
use puddlejump_core::execute::{
    execute_adopt, execute_link, execute_relink, execute_unlink, file_verdict, ExecuteOptions,
    RefusalError, Verdict,
};
use puddlejump_core::graph::DependencyGraph;
use puddlejump_core::hook::{run_hooks, HookObserver, HookOutputStream, HookResult};
use puddlejump_core::init::{execute_init, init_verdict, plan_init, InitError, InitPlan};
use puddlejump_core::link::RealFileSystem;
use puddlejump_core::package::PackageName;
use puddlejump_core::plan::{
    plan_full_link, plan_hooks, plan_relink, plan_unlink, root_reasons_for, HookPlan,
    PackageActions, Plan,
};
use puddlejump_core::session::{PackageChoice, Session, SessionInputs};
use puddlejump_core::status::package_statuses;
use puddlejump_core::system::command::SystemCommandRunner;
use puddlejump_core::system::privilege::Privilege;
use puddlejump_core::system::{build_manager, detect, ManagerId, PackageManager};
use puddlejump_core::user_config::{user_config_path, UserConfig};

use crate::cli::{AdoptArgs, Cli, Command, HookArgs, InitArgs, LinkArgs, StatusArgs, UnlinkArgs};
use crate::{environment, prompt, render};

/// How a command ended, which decides the process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everything went as asked.
    Success,
    /// Something failed while running: an install, a hook, or a file action.
    Failed,
    /// The request was refused before anything was changed.
    Refused,
}

impl Outcome {
    /// The process exit status: 0, 1, or 2.
    pub fn exit_code(self) -> u8 {
        match self {
            Outcome::Success => 0,
            Outcome::Failed => 1,
            Outcome::Refused => 2,
        }
    }
}

impl From<Verdict> for Outcome {
    fn from(verdict: Verdict) -> Self {
        match verdict {
            Verdict::Success => Outcome::Success,
            Verdict::Failed => Outcome::Failed,
            Verdict::Refused => Outcome::Refused,
        }
    }
}

/// The terminal a command talks to, so tests can supply their own.
pub struct Terminal<'a> {
    /// Where results go.
    pub out: &'a mut dyn Write,
    /// Where problems go.
    pub err: &'a mut dyn Write,
    /// Where answers come from.
    pub input: &'a mut dyn BufRead,
}

impl Terminal<'_> {
    /// Writes text to stdout.
    fn show(&mut self, text: &str) -> Result<()> {
        write!(self.out, "{text}")?;
        Ok(())
    }

    /// Writes one line to stderr.
    fn warn(&mut self, line: &str) -> Result<()> {
        writeln!(self.err, "{line}")?;
        Ok(())
    }

    /// Writes `value` to stdout as one pretty JSON document.
    fn show_json<T: serde::Serialize>(&mut self, value: &T) -> Result<()> {
        serde_json::to_writer_pretty(&mut *self.out, value)?;
        writeln!(self.out)?;
        Ok(())
    }

    /// Lists `items` under `heading` on stderr and asks `question`.
    fn confirm_list(&mut self, heading: &str, items: &[String], question: &str) -> Result<bool> {
        self.warn(heading)?;
        for item in items {
            self.warn(&format!("  {item}"))?;
        }
        Ok(prompt::confirm(question, &mut *self.input, &mut *self.err)?)
    }

    /// Says nothing was changed and returns the refused outcome.
    fn declined(&mut self) -> Result<Outcome> {
        self.warn("nothing was changed")?;
        Ok(Outcome::Refused)
    }

    /// Reports `error` as the reason a request was refused.
    fn refuse(&mut self, error: impl std::error::Error) -> Result<Outcome> {
        self.warn(&format!("error: {error}"))?;
        Ok(Outcome::Refused)
    }

    /// Reports why a plan was refused when it was about to run.
    fn refuse_run(&mut self, error: &RefusalError) -> Result<Outcome> {
        match error {
            RefusalError::Conflicts(conflicts) => {
                self.warn(&format!("error: {error}:"))?;
                for (path, _) in conflicts {
                    self.warn(&format!("  {}", path.display()))?;
                }
            }
            RefusalError::RootUnavailable(reasons) => {
                self.warn("error: root is required but neither root nor sudo is available:")?;
                for reason in reasons {
                    self.warn(&format!("  - {}", render::root_reason(reason)))?;
                }
            }
            RefusalError::AdoptNeedsRoot(_) => self.warn(&format!("error: {error}"))?,
        }
        Ok(Outcome::Refused)
    }
}

/// The process environment a command needs, passed in rather than read.
pub struct Environment {
    /// The current directory.
    pub current_dir: PathBuf,
    /// The user's home directory.
    pub home: PathBuf,
    /// The `PUDDLEJUMP_REPO` value.
    pub repo: Option<PathBuf>,
    /// The `PUDDLEJUMP_HOST` value.
    pub host: Option<String>,
    /// The `XDG_CONFIG_HOME` value, which says where the user config is.
    pub xdg_config_home: Option<PathBuf>,
    /// The `SUDO_USER` value, set when run through `sudo`.
    pub sudo_user: Option<String>,
    /// The machine's hostname.
    pub hostname: Option<String>,
    /// The `PATH` value, for finding `sudo` and the package manager.
    pub path: Option<OsString>,
    /// Whether the process runs as root.
    pub is_root: bool,
}

impl Environment {
    /// Reads the real process environment.
    ///
    /// Errors if the current directory or `HOME` cannot be determined.
    pub fn from_process() -> Result<Self> {
        Ok(Self {
            current_dir: std::env::current_dir()
                .context("cannot determine the current directory")?,
            home: environment::home_directory()?,
            repo: environment::repo_from_environment(),
            host: environment::host_from_environment(),
            xdg_config_home: environment::xdg_config_home_from_environment(),
            sudo_user: environment::sudo_user_from_environment(),
            hostname: puddlejump_core::host::machine_hostname(),
            path: std::env::var_os("PATH"),
            is_root: environment::running_as_root(),
        })
    }

    fn on_path(&self, program: &str) -> bool {
        environment::is_on_path(program, self.path.as_deref())
    }

    fn privilege(&self) -> Option<Privilege> {
        Privilege::resolve(self.is_root, self.on_path("sudo"))
    }
}

/// Runs the command the user asked for.
///
/// Errors only for problems that are not a refusal or a failure of the work
/// itself, such as an unreadable repo or a broken terminal.
pub fn run(cli: &Cli, environment: &Environment, terminal: &mut Terminal) -> Result<Outcome> {
    environment::refuse_sudo(environment.is_root, environment.sudo_user.as_deref())?;
    let json = cli.json;
    // Only `init` runs without an existing repo, so it must not open one. Every
    // other command opens the session lazily, here, once.
    let session = || open_session(cli, environment);
    match &cli.command {
        Command::Init(args) => init(args, environment, json, terminal),
        Command::List => list(&session()?, json, terminal),
        Command::Status(args) => status(&session()?, args, environment, json, terminal),
        Command::Link(args) => link(&session()?, args, environment, json, terminal),
        Command::Unlink(args) => unlink(&session()?, args, environment, json, terminal),
        Command::Relink(args) => relink(&session()?, args, environment, json, terminal),
        Command::Hook(args) => hook(&session()?, args, json, terminal),
        Command::Adopt(args) => adopt(&session()?, args, environment, json, terminal),
    }
}

fn open_session(cli: &Cli, environment: &Environment) -> Result<Session> {
    let config_path = user_config_path(environment.xdg_config_home.as_deref(), &environment.home);
    let user_config = UserConfig::load(&config_path, &environment.home)?;
    Session::open(&SessionInputs {
        repo_flag: cli.repo.as_deref(),
        repo_environment: environment.repo.as_deref(),
        repo_config: user_config.repo.as_deref(),
        current_dir: &environment.current_dir,
        home: &environment.home,
        target_override: cli.target.as_deref(),
        host_flag: cli.host.as_deref(),
        host_environment: environment.host.as_deref(),
        host_config: user_config.host.as_deref(),
        hostname: environment.hostname.as_deref(),
    })
    .context("cannot open the repo")
}

/// Picks the package manager: the configured one, else whichever is on `PATH`.
fn find_manager(session: &Session, environment: &Environment) -> Result<Option<ManagerId>> {
    detect(session.config.package_manager, &|binary| {
        environment.on_path(binary)
    })
    .map_err(Into::into)
}

/// Builds the detected package manager, if there is one.
///
/// Queries never use root; only installs do, and a plan that installs is
/// refused before it runs when no privilege is available. So when there is
/// none, the manager is built to run directly, and is never asked to install.
fn manager_for(
    session: &Session,
    environment: &Environment,
) -> Result<Option<Box<dyn PackageManager>>> {
    let privilege = environment.privilege().unwrap_or(Privilege::AlreadyRoot);
    Ok(find_manager(session, environment)?
        .map(|id| build_manager(id, SystemCommandRunner, privilege)))
}

fn list(session: &Session, as_json: bool, terminal: &mut Terminal) -> Result<Outcome> {
    let packages: Vec<&PackageName> = session.repo.packages.keys().collect();
    if as_json {
        let hosts: Vec<&str> = session.available_hosts.iter().map(|h| h.as_str()).collect();
        let names: Vec<&str> = packages.iter().map(|p| p.as_str()).collect();
        terminal.show_json(&serde_json::json!({ "packages": names, "hosts": hosts }))?;
    } else {
        let selected = session.host.as_ref().map(|host| &host.name);
        let text = render::listing(&packages, &session.available_hosts, selected);
        terminal.show(&text)?;
    }
    Ok(Outcome::Success)
}

fn status(
    session: &Session,
    args: &StatusArgs,
    environment: &Environment,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let choice = session.choose_packages(&args.packages, true)?;
    let manager = manager_for(session, environment)?;
    let statuses = package_statuses(
        &session.plan_context(),
        choice.packages(),
        &RealFileSystem,
        manager.as_deref(),
    )?;
    if as_json {
        terminal.show_json(&statuses)?;
    } else {
        terminal.show(&render::statuses(&statuses))?;
    }
    Ok(Outcome::Success)
}

/// Where live hook output goes.
enum HookSink<'a> {
    /// A hook's stdout goes to stdout and its stderr to stderr.
    Split {
        out: &'a mut dyn Write,
        err: &'a mut dyn Write,
    },
    /// Everything goes to stderr, leaving stdout free for one JSON document.
    AllToStderr(&'a mut dyn Write),
}

/// Prints hook output as it arrives.
struct LiveHookOutput<'a> {
    sink: HookSink<'a>,
}

impl<'a> LiveHookOutput<'a> {
    /// Output for a run whose results are printed as text, or as JSON when
    /// `as_json`, in which case stdout must stay clean.
    fn new(terminal: &'a mut Terminal, as_json: bool) -> Self {
        let sink = if as_json {
            HookSink::AllToStderr(&mut *terminal.err)
        } else {
            HookSink::Split {
                out: &mut *terminal.out,
                err: &mut *terminal.err,
            }
        };
        Self { sink }
    }

    fn writer(&mut self, stream: HookOutputStream) -> &mut dyn Write {
        match (&mut self.sink, stream) {
            (HookSink::Split { out, .. }, HookOutputStream::Stdout) => &mut **out,
            (HookSink::Split { err, .. }, HookOutputStream::Stderr) => &mut **err,
            (HookSink::AllToStderr(err), _) => &mut **err,
        }
    }
}

impl HookObserver for LiveHookOutput<'_> {
    fn started(&mut self, hook: &HookPlan) {
        let _ = writeln!(
            self.writer(HookOutputStream::Stdout),
            "running hook for {}...",
            hook.package
        );
    }

    fn output(&mut self, hook: &HookPlan, stream: HookOutputStream, line: &str) {
        let _ = writeln!(self.writer(stream), "[{}] {line}", hook.package);
    }
}

/// Everything `link` needs once the packages are chosen.
struct LinkRun {
    plan: Plan,
    manager: Option<Box<dyn PackageManager>>,
    privilege: Option<Privilege>,
}

/// Chooses the packages, asking first if that means every package.
///
/// Returns `None`, after saying so, if the user declined.
fn choose_packages_to_link(
    session: &Session,
    args: &LinkArgs,
    terminal: &mut Terminal,
) -> Result<Option<PackageChoice>> {
    let choice = session.choose_packages(&args.packages, true)?;
    if let PackageChoice::Everything(packages) = &choice {
        if !args.yes && !args.dry_run && !confirm_everything(session, packages, terminal)? {
            terminal.declined()?;
            return Ok(None);
        }
    }
    Ok(Some(choice))
}

/// Builds the plan for linking `choice`, or reports why it cannot be built.
fn plan_link_run(
    session: &Session,
    choice: &PackageChoice,
    environment: &Environment,
    terminal: &mut Terminal,
) -> Result<std::result::Result<LinkRun, Outcome>> {
    let graph = match DependencyGraph::build(&session.repo, choice.packages()) {
        Ok(graph) => graph,
        Err(error) => return Ok(Err(terminal.refuse(error)?)),
    };
    let manager = manager_for(session, environment)?;
    let plan = match plan_full_link(
        &session.plan_context(),
        &graph,
        &RealFileSystem,
        manager.as_deref(),
    ) {
        Ok(plan) => plan,
        Err(error) => return Ok(Err(terminal.refuse(error)?)),
    };
    Ok(Ok(LinkRun {
        plan,
        manager,
        privilege: environment.privilege(),
    }))
}

fn link(
    session: &Session,
    args: &LinkArgs,
    environment: &Environment,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let Some(choice) = choose_packages_to_link(session, args, terminal)? else {
        return Ok(Outcome::Refused);
    };
    let run = match plan_link_run(session, &choice, environment, terminal)? {
        Ok(run) => run,
        Err(outcome) => return Ok(outcome),
    };
    if args.dry_run {
        if as_json {
            terminal.show_json(&run.plan)?;
        } else {
            terminal.show(&render::plan(&run.plan))?;
        }
        return Ok(Outcome::Success);
    }
    if !as_json {
        write!(terminal.err, "{}", render::plan_notes(&run.plan))?;
    }
    let confirmed = args.yes || !run.plan.needs_hook_confirmation(!args.no_hooks);
    if !confirmed && !confirm_hooks_again(&run.plan.hooks, terminal)? {
        return terminal.declined();
    }
    let options = ExecuteOptions {
        install: !args.no_install,
        hooks: !args.no_hooks,
        privilege: run.privilege,
    };
    let result = execute_link(
        &run.plan,
        &options,
        &RealFileSystem,
        &SystemCommandRunner,
        run.manager.as_deref(),
        &mut LiveHookOutput::new(terminal, as_json),
    );
    let report = match result {
        Ok(report) => report,
        Err(error) => return terminal.refuse_run(&error),
    };
    if as_json {
        terminal.show_json(&report)?;
    } else {
        terminal.show(&render::report(&report))?;
    }
    Ok(report.verdict().into())
}

fn confirm_everything(
    session: &Session,
    packages: &[PackageName],
    terminal: &mut Terminal,
) -> Result<bool> {
    let items: Vec<String> = packages
        .iter()
        .map(|package| {
            let needs_root = session
                .repo
                .packages
                .get(package)
                .is_some_and(|package| package.manifest.needs_root);
            let note = if needs_root { "  (needs root)" } else { "" };
            format!("{package}{note}")
        })
        .collect();
    let heading = format!(
        "No host profile selected and no packages named, so this links all {} packages:",
        packages.len()
    );
    terminal.confirm_list(&heading, &items, "Link all of them?")
}

fn hook_names(hooks: &[HookPlan]) -> Vec<String> {
    hooks.iter().map(|hook| hook.package.to_string()).collect()
}

fn confirm_hooks(hooks: &[HookPlan], terminal: &mut Terminal) -> Result<bool> {
    terminal.confirm_list("These hooks will run:", &hook_names(hooks), "Run them?")
}

fn confirm_hooks_again(hooks: &[HookPlan], terminal: &mut Terminal) -> Result<bool> {
    terminal.confirm_list(
        "Everything is already linked, but these hooks would run again:",
        &hook_names(hooks),
        "Run them?",
    )
}

/// The packages `unlink` and `relink` act on. Never everything: these remove
/// links, so with nothing named and no host profile they refuse.
///
/// Returns `None`, after saying so, if there are none.
fn packages_to_change(
    session: &Session,
    named: &[String],
    terminal: &mut Terminal,
) -> Result<Option<Vec<PackageName>>> {
    let choice = session.choose_packages(named, false)?;
    if choice.packages().is_empty() {
        terminal.warn("error: no packages named and no host profile selected")?;
        return Ok(None);
    }
    Ok(Some(choice.packages().to_vec()))
}

fn unlink(
    session: &Session,
    args: &UnlinkArgs,
    environment: &Environment,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let Some(packages) = packages_to_change(session, &args.packages, terminal)? else {
        return Ok(Outcome::Refused);
    };
    let context = session.plan_context();
    let plan = match plan_unlink(&context, &packages, args.prune, &RealFileSystem) {
        Ok(plan) => plan,
        Err(error) => return terminal.refuse(error),
    };
    if args.dry_run {
        return show_file_plans(&plan, None, as_json, terminal);
    }
    let reasons = match root_reasons_for(&context, &packages, &RealFileSystem) {
        Ok(reasons) => reasons,
        Err(error) => return terminal.refuse(error),
    };
    let outcomes = match execute_unlink(
        &plan,
        &reasons,
        environment.privilege(),
        &RealFileSystem,
        &SystemCommandRunner,
    ) {
        Ok(outcomes) => outcomes,
        Err(error) => return terminal.refuse_run(&error),
    };
    show_file_outcomes(&outcomes, as_json, terminal)
}

fn relink(
    session: &Session,
    args: &UnlinkArgs,
    environment: &Environment,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let Some(packages) = packages_to_change(session, &args.packages, terminal)? else {
        return Ok(Outcome::Refused);
    };
    let context = session.plan_context();
    let plan = match plan_relink(&context, &packages, args.prune, &RealFileSystem) {
        Ok(plan) => plan,
        Err(error) => return terminal.refuse(error),
    };
    if args.dry_run {
        return show_file_plans(&plan.unlink, Some(&plan.link.packages), as_json, terminal);
    }
    let reasons = match root_reasons_for(&context, &packages, &RealFileSystem) {
        Ok(reasons) => reasons,
        Err(error) => return terminal.refuse(error),
    };
    let outcomes = match execute_relink(
        &plan,
        &reasons,
        environment.privilege(),
        &RealFileSystem,
        &SystemCommandRunner,
    ) {
        Ok(outcomes) => outcomes,
        Err(error) => return terminal.refuse_run(&error),
    };
    show_file_outcomes(&outcomes, as_json, terminal)
}

/// Shows the plan of an unlink (and, for a relink, its link half).
fn show_file_plans(
    unlink: &[PackageActions],
    link: Option<&[PackageActions]>,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    if as_json {
        terminal.show_json(&serde_json::json!({ "unlink": unlink, "link": link }))?;
    } else {
        terminal.show(&render::file_plan(unlink))?;
        if let Some(link) = link {
            terminal.show(&render::file_plan(link))?;
        }
    }
    Ok(Outcome::Success)
}

/// Shows what an unlink or relink did and judges it.
fn show_file_outcomes(
    outcomes: &[puddlejump_core::apply::PackageOutcome],
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    if as_json {
        terminal.show_json(&outcomes)?;
    } else {
        terminal.show(&render::file_report(outcomes))?;
    }
    Ok(file_verdict(outcomes).into())
}

fn hook(
    session: &Session,
    args: &HookArgs,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let packages = args
        .packages
        .iter()
        .map(|name| PackageName::new(name))
        .collect::<Result<Vec<_>, _>>()
        .context("invalid package name")?;
    let hooks = match plan_hooks(&session.plan_context(), &packages) {
        Ok(hooks) => hooks,
        Err(error) => return terminal.refuse(error),
    };
    if hooks.is_empty() {
        terminal.warn("error: none of those packages has a hook")?;
        return Ok(Outcome::Refused);
    }
    if args.dry_run {
        if as_json {
            terminal.show_json(&hooks)?;
        } else {
            for hook in &hooks {
                let line = format!("hook: {} ({})\n", hook.package, hook.script.display());
                terminal.show(&line)?;
            }
        }
        return Ok(Outcome::Success);
    }
    if !args.yes && !confirm_hooks(&hooks, terminal)? {
        return terminal.declined();
    }
    let outcomes = run_hooks(&hooks, &mut LiveHookOutput::new(terminal, as_json));
    if as_json {
        terminal.show_json(&outcomes)?;
    } else {
        terminal.show(&render::hook_outcomes(&outcomes))?;
    }
    let failed = outcomes
        .iter()
        .any(|outcome| outcome.result != HookResult::Succeeded);
    Ok(if failed {
        Outcome::Failed
    } else {
        Outcome::Success
    })
}

fn adopt(
    session: &Session,
    args: &AdoptArgs,
    environment: &Environment,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let package = PackageName::new(&args.package).context("invalid package name")?;
    let context = session.plan_context();
    let plan: AdoptPlan = match plan_adopt(&context, &package, &args.paths, &RealFileSystem) {
        Ok(plan) => plan,
        Err(error) => return terminal.refuse(error),
    };
    if args.dry_run {
        if as_json {
            terminal.show_json(&plan)?;
        } else {
            terminal.show(&render::adopt_plan(&plan))?;
        }
        return Ok(Outcome::Success);
    }
    let reasons = match root_reasons_for(&context, std::slice::from_ref(&package), &RealFileSystem)
    {
        Ok(reasons) => reasons,
        Err(error) => return terminal.refuse(error),
    };
    let report = match execute_adopt(
        &plan,
        &reasons,
        &RealFileSystem,
        &SystemCommandRunner,
        environment.privilege(),
    ) {
        Ok(report) => report,
        Err(error) => return terminal.refuse_run(&error),
    };
    if as_json {
        terminal.show_json(&report)?;
    } else {
        terminal.show(&render::adopt_outcomes(&report.moved))?;
        terminal.show(&render::file_report(&report.linked))?;
    }
    Ok(report.verdict().into())
}

fn init(
    args: &InitArgs,
    environment: &Environment,
    as_json: bool,
    terminal: &mut Terminal,
) -> Result<Outcome> {
    let plan = match plan_init(&args.path, environment.hostname.as_deref(), &RealFileSystem) {
        Ok(plan) => plan,
        Err(error) => return terminal.refuse(error),
    };
    if args.dry_run {
        if as_json {
            terminal.show_json(&plan)?;
        } else {
            terminal.show(&render::init_plan(&plan))?;
        }
        return Ok(Outcome::Success);
    }
    let outcomes = match execute_init(&plan, &RealFileSystem) {
        Ok(outcomes) => outcomes,
        Err(error) => return refuse_init(&plan, &error, terminal),
    };
    if as_json {
        terminal.show_json(&outcomes)?;
    } else {
        terminal.show(&render::init_outcomes(&plan, &outcomes))?;
    }
    Ok(init_verdict(&outcomes).into())
}

/// Reports why a repo was not created, naming each path in the way. Only the
/// conflicts are shown: nothing else was created, so listing the rest of the
/// plan would read as if it had been.
fn refuse_init(plan: &InitPlan, error: &InitError, terminal: &mut Terminal) -> Result<Outcome> {
    terminal.warn(&format!("error: {error}:"))?;
    terminal.warn(render::init_conflicts(plan).trim_end())?;
    terminal.warn("nothing was created")?;
    Ok(Outcome::Refused)
}
