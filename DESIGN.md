# puddlejump design (v1)

Decided design for v1. `CLAUDE.md` holds the standing rules (architecture,
coding standards, testing); this file records what we are building.

## Concept

- **Stow-style linking.** A repo holds package directories mirroring the tree
  they should appear in. Linking symlinks files into place; unlinking removes
  only links that point back into the repo.
- **Make-style dependencies.** Packages declare system packages and other
  puddlejump packages they need. Linking a package resolves the graph,
  installs what is missing, and links everything in order. There is no master
  list; what is installed follows from what is linked.

## Out of scope for v1

Templating, secrets/encryption, git integration, snapshots/rollback, the GUI
(it comes later as a thin frontend over `puddlejump-core`, so core returns
structured data, never printed text), package managers other than xbps.

## Repo layout

```
dotfiles/
  puddlejump.toml            # optional repo config
  hosts/<name>.toml          # optional host profiles
  packages/<name>/
    manifest.toml            # optional; the only manifest filename; never linked
    <files mirroring the target tree>
```

- A package is a directory in `packages/`; its name is the directory name.
- The manifest must be named exactly `manifest.toml`, at the package root. A
  `manifest.toml` in a subdirectory is an ordinary file and is linked.
- Never linked: the root `manifest.toml`, the file named by `hook`, `.git`,
  `.gitkeep`.
- Symlinks: a symlinked package directory directly under `packages/` is
  followed and treated as a package (a broken one is an error). Nothing
  inside a package is ever followed. A symlink inside a package is linked by
  **copying the symlink**: the target gets a new symlink with the same
  destination (a single hop, not a link to the package's link). How relative
  destinations are handled is decided in the planning step.
- Linking is **per file**. Parent directories are real directories. Directory
  symlinks are never created (no Stow-style folding/unfolding), so packages
  cannot interfere with each other and apps writing into a directory write to
  the real one, not the repo. Consequence: files added to a package need a
  `relink` (or `link`) to appear in the target; `status` reports them.

## Manifest (`packages/<name>/manifest.toml`)

```toml
target = "~"            # default "~"; or an absolute path ("/etc")
needs_root = false      # default false; true forces the up-front root check
depends = ["zsh"]       # other puddlejump packages, linked first
hook = "build.sh"       # post-link script, path inside the package dir

[packages]              # system packages, keyed by package manager id
xbps = ["base-devel", "fd"]
apt  = ["build-essential", "fd-find"]
```

- All fields optional. A package with no manifest is just files to link.
- Unknown keys are errors. Keys under `[packages]` must be known manager ids
  (a typo such as `xpbs` is an error). There is no `default` key.
- `hook` must stay inside the package directory (no `..`, not absolute).
- For the current manager `M`:
  - `[packages]` has key `M`: install the missing ones from that list.
  - `[packages]` exists but has no key for `M`: skip all of that package's
    system dependencies and add a notice to the plan. Linking proceeds.
  - No `[packages]` table: nothing to do, no notice.
  - No package manager detected: every declared dependency is reported as
    unresolved; linking proceeds.
  - `xbps = []` is valid ("considered; nothing needed").
- Only missing packages are installed, batched into one call, deduplicated
  across packages.

## Repo config and host profiles

```toml
# puddlejump.toml (repo root)
package_manager = "xbps"    # optional; overrides auto-detection
```

```toml
# hosts/<name>.toml
packages = ["zsh", "nvim", "dwm"]   # roots only; dependencies come along
```

- Host selection: `--host`, else `$PUDDLEJUMP_HOST`, else the machine hostname,
  if a matching profile exists. Profiles are optional.
- `link` with no package arguments and no profile lists every package in the
  repo (calling out root-requiring ones) and asks for confirmation. `--yes`
  skips the prompt. No profile includes/inheritance in v1.

## Root handling (model B)

puddlejump runs as the invoking user. It shells out to `sudo` only for system
package installs and for links into root-owned targets. Hooks run as the user.

A package needs root if `needs_root = true` OR its target is not writable by
the current user. System installs also need root. If the manifest says
`false`/omits it but inference says root is needed, the plan carries a warning
("add `needs_root = true`"). The plan lists every `RootReason`
(`PackageDeclared`, `TargetNotWritable`, `SystemInstall`). If root is required
and unavailable (no `sudo`), `link` refuses up front, listing all reasons,
before applying anything. Dry-run works without root and shows the same
reasons.

## Pipeline

`link` is phase-wise over the whole resolved set. The plan is built up front,
so conflicts, cycles, missing packages, and root problems abort before
anything runs.

1. **Filesystem:** link all packages in dependency order.
2. **System packages:** one batched install of everything missing.
3. **Hooks:** run in dependency order (`depends` determines hook order).

A failed hook is reported; nothing is rolled back; later hooks still run;
links stay in place. Success is the process exit status (0 = success). The
report records exit status, stdout, and stderr. Hook scripts should use
`set -e`. To retry, the user fixes the hook and runs `link` again.

Hooks run with the package directory as the working directory, as the
invoking user (never through sudo), with `PUDDLEJUMP_PACKAGE`,
`PUDDLEJUMP_TARGET` (the effective target directory), and `PUDDLEJUMP_REPO`
set on top of the inherited environment. stdin is closed. Output is streamed
live through a `HookObserver` callback (core never prints) and also collected
in the report. Output is piped, not a TTY, so tools may switch to plain
output; a PTY is a post-v1 follow-up. There is no timeout in v1.

`link` runs hooks every time it runs. When nothing was newly linked and a hook
is about to run (the "running it again" case), it prompts first; `--yes` skips.
Hooks are expected to be idempotent.

## CLI

Global flags: `--repo <dir>` (default `$PUDDLEJUMP_REPO`, else cwd),
`--target <dir>` (replaces all package targets; for staging/tests),
`--host <name>`, `--json`.

| Command | Behavior | Flags |
|---|---|---|
| `link [PACKAGE...]` | Resolve deps, link, install, run hooks. | `-n/--dry-run`, `--no-install`, `--no-hooks`, `-y/--yes` |
| `unlink [PACKAGE...]` | Remove links pointing into the repo. Named packages only. Never uninstalls. | `-n`, `--prune` |
| `relink [PACKAGE...]` | Unlink then link. **Filesystem only**: no installs, hooks, or dependency resolution. | `-n`, `--prune` |
| `hook PACKAGE...` | Run the named packages' hooks in dependency order, after confirmation. | `-n`, `-y` |
| `adopt PACKAGE PATH...` | Move real files into the package, then link them. | `-n` |
| `status [PACKAGE...]` | Per-package state; missing system deps. | |
| `list` | Packages and host profiles. | |
| `init PATH` | Create a new repo at PATH. PATH is required; missing parents are created. `--repo` is ignored. | `-n` |

- Every mutating command supports `--dry-run` (build the plan, do not apply).
- `--prune` removes directories left empty by the unlink, only on the path of
  removed links, never the target root. Empty directories are left by default.
- Exit codes: `0` success; `1` runtime failure (install or hook failed);
  `2` plan refused (conflicts, root required, cycle, missing package, invalid
  manifest).
- Conflicts never overwrite real files. `adopt` is the explicit way to move an
  existing file into a package.

### `init`

`init PATH` creates `PATH/`, `puddlejump.toml`, `hosts/`, `packages/`, and
`hosts/<hostname>.toml`. The config is all comments and the host profile lists
no packages, so a new repo behaves as if it had no config. It never overwrites:
anything that already exists is skipped, which makes it safe to run again, in a
directory that already has a README or `.git`, or on a second machine to add its
profile. A path in the way (a file where a directory is needed, or the reverse)
refuses the whole plan before anything is created. If the hostname is unknown or
cannot be a profile name, only the profile is left out and a note says why. It
does not add `.gitkeep` files, so an empty `packages/` is not tracked by git
until it has something in it, and it does not run `git init`.

### CLI behavior decisions

- **Output.** Results go to stdout; warnings, notices, prompts, and errors go to
  stderr. Plain text, no colour. A real run prints one report (a line per
  action with its outcome); the plan is printed only for `--dry-run`.
- **`--json`.** Prints exactly one JSON document to stdout: the plan for a
  dry run, the report for a real run. Live hook output moves to stderr so
  stdout stays parseable.
- **Prompts.** Only `y`/`yes` means yes; an empty answer or no terminal input
  means no, so a script cannot confirm by accident. Prompts: linking every
  package (no packages named, no host profile), and running hooks again when
  nothing was newly linked. `hook` always asks unless `--yes`.
- **Defaults when nothing is named.** `link` uses the host profile, else asks
  about every package. `status` uses the host profile, else every package.
  `unlink` and `relink` refuse rather than act on everything.
- **Exit codes.** `0` success. `1` something failed while running (a file
  action, the install, or a hook). `2` refused before changing anything
  (conflicts, root unavailable, cycle, missing package, invalid input or
  manifest, declined prompt). `adopt` exits 2 when it refused paths and moved
  nothing, and 0 if at least one file moved.
- **Root.** Privilege is `AlreadyRoot` if the process is root, else `UseSudo`
  if `sudo` is on `PATH`, else none. With none, a plan needing root is
  refused up front; `--no-install` removes the install's need for root.
- **Root for unlink, relink, and adopt.** `unlink` and `relink` change a
  package that needs root through the same `sudo`-wrapped writer as `link`, and
  refuse up front, changing nothing, if root is needed and unavailable.
  `relink` also checks the link half for conflicts *before* removing anything,
  so a conflict can never leave links removed and not re-created. `adopt`
  refuses a package that needs root rather than risk a half-moved file; a
  privileged move is a possible follow-up.
- **Errors.** Printed as one line; a cause whose text the level above already
  contains is not repeated.
- **Known quirk.** A hook's stdout and stderr are piped separately, so their
  relative order in the live output is not guaranteed (order within each
  stream is).

## Core modules (`puddlejump-core`)

Organized by responsibility; each module has its own `thiserror` enum.

```
config.rs      repo config
manifest.rs    Manifest, parsing/validation
package.rs     PackageName, Package, Repo; discovery; file enumeration
host.rs        host profiles and selection
session.rs     open a repo from raw inputs; choose which packages
graph.rs       DependencyGraph: cycles, missing packages, link order
system.rs      PackageManager trait, ManagerId, detection
system/command.rs           CommandRunner seam
system/privilege.rs         how a command gets root
system/privileged_writer.rs file changes through sudo
system/xbps.rs the only place that knows xbps commands
plan.rs        Plan, Action: pure planning over FileSystemState
link.rs        FileSystemState: observe paths, writability
apply.rs       apply file actions (re-checking each path)
adopt.rs       adopt plan + apply
hook.rs        run hooks with live output, collect outcomes
execute.rs     run a whole plan: files, install, hooks; Report
status.rs      Status per package
```

```rust
pub struct Plan {
    pub file_actions: Vec<PackageActions>,   // link order
    pub system: SystemPlan,
    pub hooks: Vec<HookPlan>,                // dependency order
    pub needs_root: Vec<RootReason>,
    pub warnings: Vec<PlanWarning>,
}

pub enum Action {
    Link { source: PathBuf, link: PathBuf, create_parents: bool },
    Unlink { link: PathBuf },
    Skip { link: PathBuf, reason: SkipReason },
    Conflict { path: PathBuf, kind: ConflictKind },
    Adopt { from: PathBuf, into: PathBuf },
    RemoveDirectory { path: PathBuf },
}

pub struct SystemPlan {
    pub manager: Option<ManagerId>,
    pub to_install: Vec<String>,
    pub already_installed: Vec<String>,
    pub notices: Vec<SystemNotice>,   // NoManagerAvailable, NoListForManager
}

pub trait PackageManager {
    fn id(&self) -> ManagerId;
    fn is_installed(&self, name: &str) -> Result<bool, SystemError>;
    fn install(&self, names: &[String]) -> Result<(), SystemError>;   // one batched call
}
```

- Plans and reports are plain serializable data; printing lives in the CLI.
- Planning takes a `TargetState` abstraction (path -> missing/symlink/file/dir)
  so most tests need no filesystem.
- Any conflict blocks applying the whole plan; apply re-checks each path so a
  real file is never overwritten even if the filesystem changed after planning.
- Unlink removes only symlinks whose destination is inside the repo's package
  dir; foreign symlinks and real files are skipped with a reason.
- xbps runs commands through a small internal `CommandRunner` seam so tests
  assert exact argv. Installed check: `xbps-query -p pkgver <name>`.

## Dependencies

| Crate | Where | Reason |
|---|---|---|
| `thiserror` | core | typed errors |
| `serde` | core | (de)serialize manifests, profiles, plans, reports |
| `toml` | core | parse manifests, config, profiles |
| `serde_json` | cli | `--json` output |
| `clap` | cli | argument parsing |
| `anyhow` | cli | top-level error reporting in the binary |
| `tempfile` | dev | temp repos/targets |
| `assert_cmd` | dev, cli | run the real binary |
| `predicates` | dev, cli | assert on output |

Crates are added in the step that first uses them. Not used: `walkdir`,
`which`, `nix`, `dialoguer`, `gethostname`.

## Build steps

Each step ships with unit tests and must pass `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test`; test output is
shown at the end of each step.

1. Workspace skeleton
2. Manifests and package discovery
3. Dependency resolution
4. Planning
5. Applying (links, installs, hooks)
6. CLI

## Test plan

**Core unit tests** (next to the code):

- `manifest`: defaults; full manifest; unknown key; unknown manager id;
  `default` key rejected; empty list valid; `hook` with `..`/absolute path;
  invalid TOML names the file; `needs_root` parses.
- `package`: discovery; non-directories ignored; missing `packages/`; empty
  repo; nested file enumeration; excludes root `manifest.toml`, the hook file,
  `.git`, `.gitkeep`; nested `manifest.toml` is linked; empty package; invalid
  names; symlinks inside a package are linked as-is.
- `config`/`host`: defaults; invalid manager id; unknown keys; selection
  precedence (flag, env, hostname) with environment passed as arguments.
- `graph`: chain, diamond, independent roots (deterministic order); self,
  2-cycle, 3-cycle with ordered path; missing dependency names both sides;
  transitive closure only; duplicate roots deduplicated.
- `system`: no manager gives notices; manager with no list gives
  `NoListForManager`; absent/empty `[packages]` gives no notice; dedup and a
  single batched install; only missing installed; xbps exact argv; failure
  carries stderr; detection (override beats auto-detect, unknown override
  errors, nothing on `PATH` gives `None`).
- `plan`: fresh target; already linked skips; real file/real directory/foreign
  symlink/broken symlink conflicts; package-vs-package collisions; unlink only
  repo-pointing links; `needs_root` reasons and the declaration/inference
  warning; empty package; order matches `link_order`; `relink` plan has no
  installs, hooks, or dependency resolution; `--prune` produces
  `RemoveDirectory` only on removed-link paths, never the target root.
- `link`/`apply` (temp dirs): links and parents created; idempotent; refuses
  plans with conflicts; never overwrites a real file even if it appeared after
  planning; permission errors typed; hooks run in dependency order with
  explicit environment; failing hook does not stop later hooks or remove links;
  missing/non-executable hook is a clear error; install failure stops before
  hooks.
- `adopt`: moves and links; refuses existing destination; refuses symlink or
  missing path; directory adopts its files; path outside target errors;
  dry-run changes nothing.
- `status`: linked, unlinked, partial, broken, conflict, wrong-target; missing
  system deps; no backend reports unknown, not missing; files in the package
  not yet linked.

**CLI integration tests** (`crates/puddlejump/tests/`, temp repo and target,
fake `xbps-install`/`xbps-query`/`sudo` scripts first on `PATH`):

- `link` creates links; `--dry-run` changes nothing; conflict exits 2 and
  changes nothing; dependencies linked first; cycle/missing exit non-zero.
- `unlink` removes only repo links; `--prune` removes empty directories only;
  `relink` picks up new files and never installs or runs hooks.
- `adopt` moves and links; `--dry-run` doesn't.
- `status` states and `--json` parses.
- Missing system deps reported with no backend while link succeeds; fake xbps
  gets one batched install of only the missing packages.
- Failing hook: exit 1, links remain, later hooks still run; re-running `link`
  prompts when nothing was newly linked, `--yes` skips.
- Root-required package as non-root: refuses up front, exit 2, nothing linked.
- `link` with no arguments and no profile prompts; `--yes` skips; host
  profile selection; unknown host errors.
- `hook` runs after confirmation, refuses a package without a hook.

## Applying (step 5)

- Applying refuses a plan containing any conflict, and a plan whose root
  requirements cannot be met (not root and no `sudo`), before touching
  anything, listing every reason.
- Each file action re-checks its path immediately before acting: a link is
  created only if the path is still missing, and an unlink only removes a
  symlink that still has the expected destination. A real file is never
  overwritten even if it appeared after planning.
- A failed file action is recorded and the remaining actions still run.
  `RemoveDirectoryIfEmpty` on a non-empty directory is a skip, not a failure.
- Order: files, then system install, then hooks. An install failure stops
  before hooks; links already made stay.
- Root under model B: system installs run `sudo xbps-install ...` unless the
  process is already root. Links into root-owned targets go through a
  `sudo`-wrapped writer (`ln -s`, `rm`, `mkdir -p`) behind the same writer
  trait as the plain one, so apply logic is identical. Whether to use sudo is
  passed in as a `Privilege` value, not detected inside a package manager.
- Package managers run commands through a `CommandRunner` seam so tests assert
  exact argv. xbps: `xbps-query -p pkgver <name>` exits 0 when installed and 2
  when not (verified on xbps 0.60.7); any other exit is a query error.
  Install is one `xbps-install -y <names...>` call.
- Detection: the config override wins (error if its binary is not on `PATH`),
  else the first known manager found on `PATH`, else none. `PATH` is passed in.

## Where logic lives

The binaries only parse arguments, print, prompt, and set the exit status.
Everything a second frontend would need is in core: choosing packages
(`session`), building the package manager (`system::build_manager`), deciding
how to get root (`Privilege::resolve`), the hook-confirmation rule
(`Plan::needs_hook_confirmation`), running each operation end to end
(`execute_link`, `execute_unlink`, `execute_relink`, `execute_adopt`), and
judging a result (`Report::verdict`, `AdoptReport::verdict`, `file_verdict`).
Adding a package manager means a new backend module plus one arm in
`system.rs` (the `ManagerId` conversions, `KNOWN_MANAGERS`, and
`build_manager`); no other file names a concrete manager.

## Post-v1 follow-ups

1. Should `unlink` ever uninstall system packages? (needs tracking of what
   puddlejump installed)
2. Persisted hook state (last run/result) so `link` can offer to retry failed
   hooks without prompting heuristics.
3. Additional package managers (apt, pacman, dnf): one new implementation each.
4. Optional `link_directories` escape hatch for directories that must be
   linked whole.
5. Host profile `include` for shared package lists.
6. The GUI (`puddlejump-gui`) as a thin frontend over core.
7. Run hooks under a PTY so tools keep colour and progress output.
8. A hook timeout.
9. Privileged `adopt` (a root-owned file moved through `sudo`), which needs a
   move that cannot leave a file half-moved.
10. Bundle `state`, `runner`, and `privilege` into one type if a second
    frontend ends up constructing the `execute_*` arguments.
