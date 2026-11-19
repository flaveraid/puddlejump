# puddlejump design

The design of puddlejump (v1, plus the persistent hook and link state described
under Persistent state). `CLAUDE.md` holds the standing project rules
(architecture, coding standards, testing); this file describes what puddlejump
does and how it is built.

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
  destination (a single hop, not a link to the package's link). A symlink with
  a relative destination is skipped with a warning, because the same relative
  path would resolve to something different from the target directory.
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

- Host selection: `--host`, else `$PUDDLEJUMP_HOST`, else `host` in the user
  config, else the machine hostname, if a matching profile exists. A host named
  by flag, environment, or user config must have a profile; the hostname is
  only a guess. Profiles are optional.
- `link` with no package arguments and no profile lists every package in the
  repo (calling out root-requiring ones) and asks for confirmation. `--yes`
  skips the prompt. No profile includes/inheritance in v1.

## User config

The repo config above lives inside the repo, so it cannot say where the repo
is. The user's own settings live in `$XDG_CONFIG_HOME/puddlejump/config.toml`,
or `~/.config/puddlejump/config.toml` when `XDG_CONFIG_HOME` is unset, empty, or
not absolute (the XDG rule is to ignore a relative value).

```toml
# ~/.config/puddlejump/config.toml
repo = "~/dotfiles"    # optional; absolute, or starting with ~/
host = "lenovo"        # optional; a profile name
```

- Repo: `--repo`, else `$PUDDLEJUMP_REPO`, else `repo`, else the current
  directory. A relative `repo` is an error, since it would mean a different
  place depending on where the command is run.
- Host: see above; `host` sits between the environment and the hostname.
- A missing file is the defaults. An unreadable file, bad TOML, or an unknown
  key is an error that names the file. `init` ignores the file, like it ignores
  `--repo`, so a broken config cannot stop a new repo being created.
- Only `repo` and `host` are accepted. The file is read by `core`
  (`user_config`) from paths the CLI passes in; `core` never reads the
  environment.

## Root handling (model B)

puddlejump runs as the invoking user. It shells out to `sudo` only for system
package installs and for links into root-owned targets. Hooks run as the user,
always: `needs_root` covers linking and system installs, never the hook. A hook
that needs root for some command calls `sudo` itself for that command, so what
runs privileged is visible in the script and the hook keeps the user's `$HOME`
and file ownership. Hook stdin is closed, but `sudo` reads its password prompt
from the terminal directly, so a `sudo` call in a hook can still ask for one.

It refuses to run *under* `sudo`: if the process is root and `$SUDO_USER` is
set, every command exits 2 before doing anything. Under `sudo` the home
directory, the user config, and file ownership are muddled between two users,
and puddlejump already uses `sudo` itself where root is needed. A real root
login is allowed, since its home and config are root's own. The check relies on
`SUDO_USER`, so tools that do not set it (`doas`, `su`) are not detected.

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
`set -e`. To retry, the user fixes the hook and runs `link` again: a hook that
failed is retried automatically (see Persistent state).

Hooks run with the package directory as the working directory, as the
invoking user (never through sudo), with `PUDDLEJUMP_PACKAGE`,
`PUDDLEJUMP_TARGET` (the effective target directory), and `PUDDLEJUMP_REPO`
set on top of the inherited environment. stdin is closed. Output is streamed
live through a `HookObserver` callback (core never prints) and also collected
in the report. Output is piped, not a TTY, so tools may switch to plain
output; a PTY is a post-v1 follow-up. There is no timeout in v1.

The install step is shown the same way. `xbps-install`'s output is streamed
through the same observer (`install_output`) while it runs, prefixed
`[install]`, and is complete before any hook starts. It follows the hook rule
for `--json`: output goes to stderr, so stdout stays one document. Only the
install is shown; the `xbps-query` checks stay silent. Because the installer
writes to a pipe rather than a TTY, you get its log lines but not a live
progress bar (the PTY follow-up covers both). Stdout and stderr are read on
separate threads, so lines from the two streams can interleave out of order.
A failure is shown live and also reported in the install result.

Which hooks `link` runs is decided from the remembered result of each hook's
last run, for named packages and dependencies alike (see Persistent state). A
hook runs when it has never run, failed last time, or its script changed since
it last succeeded; a hook that last succeeded with the same script is skipped.
`--force-hooks` runs every hook of the resolved set regardless. The `hook`
command always runs the hooks it names. Because the decision no longer depends
on file state, a hook-only package is skipped once it has succeeded, and there
is no "run again?" prompt. Hooks are still expected to be idempotent, since
`--force-hooks`, `hook`, and an edited script all run them again.

## Persistent state

One file per user remembers what puddlejump did, so a later run does not have
to guess from the filesystem: `$XDG_STATE_HOME/puddlejump/state.toml` (default
`~/.local/state/puddlejump/state.toml`; a relative `XDG_STATE_HOME` is ignored,
as the XDG rules say). It is state, not config: written by puddlejump, never
edited by hand, and safe to delete.

```toml
version = 1

[[repo]]
path = "/home/me/dotfiles"

  [[repo.link]]
  package = "zsh"
  link = "/home/me/.zshrc"
  destination = "/home/me/dotfiles/packages/zsh/.zshrc"
  linked_at = 1763380000     # seconds since the Unix epoch; 0 when not recorded

  [[repo.hook]]
  package = "dwm"
  status = "succeeded"       # succeeded | failed
  exit_code = 0              # omitted when killed by a signal or not runnable
  fingerprint = "af63dc4c8601ec8c"
  ran_at = 1763380000        # seconds since the Unix epoch
```

- **Keyed by repo root**, so two repos never share records.
- **A record, never an authority.** A hook record decides only whether a hook
  runs. A link record never removes anything by itself: it is checked against the
  disk first (see Link records). Deleting the file loses history only: every hook
  then counts as never run, and puddlejump forgets which links it made.
- **Fingerprint.** FNV-1a (64-bit) of the hook script's bytes, as 16 hex digits.
  It only has to notice an edit, and it is stable across Rust versions, which
  `DefaultHasher` is not. A script that cannot be read counts as changed, so
  running it reports the real problem.
- **What is stored.** Only the result, never the output. A hook that could not
  start is stored as failed with an empty fingerprint.
- **When it is written.** After a real `link`, `unlink`, `relink`, `adopt`, or
  `hook` run, once, with the links it created or removed and the results of the
  hooks that ran. A run that changed nothing worth remembering does not touch the
  file. `--dry-run`, `--no-hooks`, `status`, and an install failure (which stops
  hooks before they start) write nothing, so a hook that did not run is never
  remembered as having run.
- **Atomic.** Written to a temporary file in the same directory, synced, and
  renamed over the old one, so a crash leaves the old state or the new one.
- **Damaged files are refused, not replaced.** A file that is unreadable, not
  valid, has unknown keys, or has a different `version` refuses the command
  (exit 2) before anything runs, so history is never silently discarded.
- **A failed save is a warning.** The run already did its work, so an unwritable
  state file prints a warning and leaves the exit status alone.

The plan says why each hook runs (`never run`, `failed last time`, `script
changed since it last succeeded`, `requested`), in the text plan and in the JSON
plan's `reason`. `status` reports a hook that has never run, failed, or changed,
and says nothing about one that is up to date.

### Link records

A link record says puddlejump created a symlink, so it can tell its own links
from other people's and clean up after a package changes.

- **Only links puddlejump created.** A link that `link` found already in place
  is not recorded, because puddlejump did not make it. A link that failed or was
  skipped does not exist, so it is not recorded either. `LinkSymlinkCopy` links
  are recorded with the destination they were given.
- **When it was made.** `linked_at` is stamped when the link is created, and a
  link created again at the same path gets a fresh time. A record without one,
  such as one written before it was kept, reads as `0`, which means "unknown"
  and is never shown. The clock is clamped to `0` if it is before 1970, as
  `ran_at` is, so a wrong clock cannot fail a run.
- **Forgotten when removed.** A successful `unlink`, or one that found the link
  already gone, drops the record. A removal that failed keeps it. `relink` is an
  unlink followed by a link over the same outcomes, so its records end up current.
- **Stale links.** A record is *stale* when the link is still on disk exactly as
  recorded (a symlink to the recorded destination) but its package no longer
  provides it: the package was deleted from the repo, or the file was removed
  from the package. A link that is gone, was replaced by a real file, or now
  points elsewhere is never stale, so puddlejump never claims what it did not
  make. This is the one definition used by both `status` and `unlink`.
- **`status`** lists a package's stale links under it (and as `stale` in the
  JSON, an addition to each package's entry), and, in text, lists the stale links
  of packages that are no longer in the repo under `<package>: no longer in the
  repo`. Each stale link says how long ago it was made, such as `(linked 3 days
  ago)`.

### Times in `status`

A time is shown only for something that needs attention, so a healthy `status`
stays quiet: a stale link (`linked 3 days ago`), and a hook that failed (`exit
status 3, 2 hours ago`) or whose script changed (`5 days ago`). A hook that has
never run has no time, an up-to-date hook shows nothing, and a healthy link shows
no time.

Times are relative, in the largest whole unit: `just now` under a minute, then
minutes, hours, and days (never weeks or months, so `45 days ago`). A time that
is not in the past reads `just now`, and an unrecorded time (`0`) is left out.
Core returns raw seconds since the epoch; only the CLI formats them, against a
`now` it is handed so rendering stays deterministic. In JSON the raw seconds are
kept: each stale link has `linked_at`, and each package has `hook_ran_at`
(`null` when the hook has never run or the package has no hook). `hook_ran_at`
is a separate field so the shape of `hook` is unchanged.
- **`unlink`** removes a named package's stale links along with its current
  ones, and accepts a package that is no longer in the repo if it has stale
  links. Each removal re-checks the path when applied, like any other. A package
  that is neither in the repo nor has stale links is still "does not exist".
  Whether such a package needed root is unknown (it has no manifest), so it is
  judged by whether the directory holding each link is writable. `--prune`
  never prunes directories for a package that is no longer in the repo, since
  its target directory is unknown; for the others, pruning is planned after all
  removals so a directory is only pruned once the stale links in it are gone.
- **Needs the state file.** With the file deleted, a package that is gone from
  the repo cannot be unlinked by name, because nothing remembers its links.

## CLI

Global flags: `--repo <dir>` (default `$PUDDLEJUMP_REPO`, else the user
config's `repo`, else cwd),
`--target <dir>` (replaces all package targets; for staging/tests),
`--host <name>`, `--json`.

| Command | Behavior | Flags |
|---|---|---|
| `link [PACKAGE...]` | Resolve deps, link, install, run the hooks that need running. | `-n/--dry-run`, `--no-install`, `--no-hooks`, `--force-hooks`, `-y/--yes` |
| `unlink [PACKAGE...]` | Remove links pointing into the repo, and the stale links puddlejump made for the package. Named packages only; a package gone from the repo works if its links were remembered. Never uninstalls. | `-n`, `--prune` |
| `relink [PACKAGE...]` | Unlink then link. **Filesystem only**: no installs, hooks, or dependency resolution. | `-n`, `--prune` |
| `hook PACKAGE...` | Run the named packages' hooks in dependency order, after confirmation, whatever was remembered. | `-n`, `-y` |
| `adopt PACKAGE PATH...` | Move real files into the package, then link them. | `-n` |
| `status [PACKAGE...]` | Repo, target, host, package manager, state file, and set env vars; per-package state; missing system deps; hook state; stale links; a count by state. The context and count are text only; `--json` stays the package array. | |
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
`hosts/<hostname>.toml`. Each starter file lists every key its parser accepts,
with a comment and an example, so it can be edited without reading the docs. The
config key is commented out (setting `package_manager` would make a machine
without that manager an error) and the host profile has `packages = []`, so a
new repo behaves as if it had no config. It never overwrites: anything that
already exists is skipped, which makes it safe to run again, in a directory that
already has a README or `.git`, or on a second machine to add its profile. A path
in the way (a file where a directory is needed, or the reverse) refuses the whole
plan before anything is created.

The hostname becomes the profile's filename, so it is made safe first: path
separators, whitespace, and control characters become `-`, and leading dots and
dashes are dropped (never a hidden file, never a lookalike for a flag). A valid
name such as `lenovo.local` is unchanged. Host selection applies the same
conversion to the machine's hostname before matching, so a profile made for a
machine is found by that machine; `--host` and `$PUDDLEJUMP_HOST` are not
converted, since a name typed by the user means exactly that name. If nothing
usable is left (an empty hostname, or only dots, dashes, or whitespace) only the
profile is left out and a note says why. `init` does not add `.gitkeep` files,
so an empty `packages/` is not tracked by git until it has something in it, and
it does not run `git init`. When a parser gains a key, its `*_KEYS` constant
must list it, and a test fails until the starter documents it.

### CLI behavior decisions

- **Output.** Results go to stdout; warnings, notices, prompts, and errors go to
  stderr. Plain text, no colour. A real run prints one report (a line per
  action with its outcome); the plan is printed only for `--dry-run`.
- **`--json`.** Prints exactly one JSON document to stdout: the plan for a
  dry run, the report for a real run. Live hook output moves to stderr so
  stdout stays parseable.
- **Prompts.** Only `y`/`yes` means yes; an empty answer or no terminal input
  means no, so a script cannot confirm by accident. Prompts: linking every
  package (no packages named, no host profile). `hook` always asks unless
  `--yes`.
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
user_config.rs the user's own config file: repo and host
session.rs     open a repo from raw inputs; choose which packages
graph.rs       DependencyGraph: cycles, missing packages, link order
system.rs      PackageManager trait, ManagerId, detection
system/command.rs           CommandRunner seam
system/privilege.rs         how a command gets root
system/privileged_writer.rs file changes through sudo
system/xbps.rs the only place that knows xbps commands
plan.rs        Plan, Action: pure planning over FileSystemState
link.rs        FileSystemState: observe paths, writability
link_state.rs  remember and forget links; find stale links; plan unlinking them
apply.rs       apply file actions (re-checking each path)
adopt.rs       adopt plan + apply
hook.rs        run hooks with live output, collect outcomes
hook_state.rs  fingerprints; decide which hooks need running; record outcomes
state.rs       the persistent state file: load, atomic save, records
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

Not used: `walkdir`, `which`, `nix`, `dialoguer`, `gethostname`.

## Quality gates

Every change ships with tests and must pass `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test`.

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
  not yet linked; hook never run, up to date, failed (with exit code), or
  script changed.
- `state`: round trip; missing file is empty state; corrupt, unknown-key,
  versionless, and other-version files are errors that name the file; atomic
  save leaves no temporary file and replaces the old state; unwritable
  location is a typed error; repos are kept apart; records are sorted; XDG
  path rules (unset, empty, relative).
- `hook_state`: FNV-1a matches the reference vectors; editing changes the
  fingerprint; a decision for each of never run, failed, changed, up to date,
  and unreadable script; records for success, failure, signal, and
  not-runnable; a hook that did not run keeps its record.

**CLI integration tests** (`crates/puddlejump/tests/`, temp repo and target,
fake `xbps-install`/`xbps-query`/`sudo` scripts first on `PATH`):

- `link` creates links; `--dry-run` changes nothing; conflict exits 2 and
  changes nothing; dependencies linked first; cycle/missing exit non-zero.
- `unlink` removes only repo links; `--prune` removes empty directories only;
  `relink` picks up new files and never installs or runs hooks.
- `adopt` moves and links; `--dry-run` doesn't.
- `status` states and `--json` parses; text shows the context and a count, JSON stays a bare array.
- Missing system deps reported with no backend while link succeeds; fake xbps
  gets one batched install of only the missing packages.
- Failing hook: exit 1, links remain, later hooks still run; re-running `link`
  retries it without asking.
- Persistent state: a hook that succeeded is not re-run; a failed one is
  retried; an edited script runs again; `--force-hooks` and `hook` always run;
  `--dry-run`, `--no-hooks`, `status`, and an install failure write no state; a
  dependency's hook is skipped once it succeeded (including a hook-only
  package); `XDG_STATE_HOME` moves the file; a corrupt or newer-version file
  refuses `link`, `hook`, `status`, `unlink`, `relink`, and `adopt` (exit 2)
  without running or overwriting anything; an unwritable state file warns and
  keeps the exit status; deleting the file only loses history.
- Link records: `link`, `relink`, and `adopt` remember the links they create (and
  when) and not ones already in place; `unlink` forgets what it removes; `--dry-run` writes
  nothing; `status` reports a link whose file left the package (text and JSON)
  and the links of a package gone from the repo, and never claims a link the
  user replaced; `unlink` removes stale links of a package that still exists and
  of one that is gone, leaves a replaced link alone, and still errors for a
  package that never existed; with the state file deleted a gone package cannot
  be unlinked.
- Times in `status`: a stale link and a failed or edited hook say how long ago
  (aged by rewriting the state file, so the age is known without a clock); a
  healthy link, an up-to-date hook, and a hook that never ran show none; a record
  without a time shows none; JSON carries `linked_at` and `hook_ran_at` in
  seconds.
- Root-required package as non-root: refuses up front, exit 2, nothing linked.
- `link` with no arguments and no profile prompts; `--yes` skips; host
  profile selection; unknown host errors.
- `hook` runs after confirmation, refuses a package without a hook.

## Applying

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
  Install is one `xbps-install -y <names...>` call, run through
  `CommandRunner::run_streaming` so each output line reaches the caller as it
  is written. Queries use plain `run`. A runner that cannot stream falls back
  to replaying its buffered output.
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
2. ~~Persisted hook state~~ and ~~persisted link state~~ Done (see Persistent
   state): puddlejump tells the links it created from others, notices links
   orphaned by a file removed from a package, and unlinks a package that no
   longer exists in the repo.
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
11. Add shell completions (fish, zsh)
12. ~~`status` context~~ Done: `status` lists the repo, target, host, manager,
    state file, and set env vars, plus a count by state.
13. Consider adding a `copy` option that copies package to target instead of link

