# puddlejump

A dotfiles manager for Linux, written in Rust: Stow-style symlinking plus
Make-style dependency resolution that installs what each package needs. A
CLI (`puddlejump`) and a thin GUI frontend (`puddlejump-gui`) over one
shared library.

## Philosophy

- Do exactly what is needed, nothing more. No speculative features, no
  "might be useful later" abstractions, no config options nobody asked for.
- Small, readable, boring code beats clever code. A newcomer should be able
  to read any module top to bottom and understand it.
- Prefer fewer dependencies. Every new crate needs a one-line justification
  in the PR/commit message. Prefer std when it's reasonable.
- Never touch the user's filesystem in a surprising way. Every mutating
  operation must support a dry run and must refuse to overwrite real files
  unless explicitly told to.

## Workspace layout

```
crates/
  puddlejump-core/   # all logic: config, package discovery, planning, linking
  puddlejump/        # CLI binary: argument parsing + output only
  puddlejump-gui/    # GUI binary: presentation only
```

- `core` has no knowledge of terminals or windows. It returns data and
  errors; frontends decide how to show them.
- The CLI and GUI must never contain business logic. If both frontends need
  it, it lives in `core`.
- Model operations as **plan, then apply**: `core` builds a plan (a list of
  actions: link, unlink, skip, conflict) that can be printed, reviewed, or
  executed. Dry run = build the plan and don't apply it.

## Coding standards

- Rust stable, edition 2021 or newer. Must pass with no warnings:
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
- Run all three before saying a task is done.
- Errors: `thiserror` for typed errors in `core`; `anyhow` allowed only in
  the binaries. No `unwrap()`/`expect()` outside tests unless the invariant
  is truly impossible to break, with a comment saying why.
- No `unsafe`.
- Functions do one thing. If a function needs a comment explaining its
  sections, split it.
- Modules are organized by responsibility (e.g. `config`, `package`, `plan`,
  `link`), not by type (no `utils.rs`, `helpers.rs`, `types.rs` grab bags).
- Names are full words: `package_dir`, not `pkg_d`.
- Doc comments (`///`) on every public item in `core`, saying what it does
  and when it errors. Inline comments explain *why*, never *what*.
- Code comments must reflect the working state of the code. When behavior
  changes, update or remove the comments that describe it.
- No dead code, commented-out code, or TODOs left behind without an issue.
- Paths: use `std::path::Path`/`PathBuf` throughout, never string paths.

## Testing

Tests are part of every change, not a follow-up task. A change without tests
is not done.

- **Unit tests** live next to the code in a `#[cfg(test)] mod tests` block
  at the bottom of each file. Every `core` module has them.
- Write the test first when fixing a bug: reproduce it with a failing test,
  then fix it.
- Cover the unhappy paths, not just the happy one: missing directories,
  existing real files (conflicts), broken symlinks, links pointing outside
  the repo, permission errors, empty packages, invalid config.
- Keep logic pure where possible (e.g. building a plan from a description
  of the tree) so most tests don't need the filesystem at all.
- Filesystem behavior is tested against temp directories (`tempfile`),
  never the real home directory or system paths.
- Test the plan, not just the result: assert on the exact actions produced
  for a given tree, including conflicts and skips.
- **Integration tests** for the CLI go in `crates/puddlejump/tests/`, run the
  real binary against a temp repo and temp target, and check the exit code,
  output, and resulting filesystem state.
- Test names describe the behavior: `unlink_leaves_foreign_symlinks_alone`,
  not `test_unlink_2`.
- One behavior per test. Shared setup goes in small helper functions inside
  the test module, not in a sprawling fixture framework.
- Tests must be deterministic and independent: no ordering, no network, no
  sleeps, no reliance on the host's environment variables (set them
  explicitly in the test).
- Dev-dependencies follow the same rule as normal ones: justify each.
  Expected: `tempfile`; for CLI tests, `assert_cmd` and `predicates`.

## Working style

- For anything non-trivial, propose a short plan and wait for approval
  before writing code.
- Make small, focused changes. One concern per commit.
- When a feature or bug has been addressed, ask the user whether they want
  to commit the changes. Don't commit without their go-ahead.
- If a request is ambiguous or conflicts with these rules, ask. Don't guess.
- Don't add features, flags, or crates beyond what the task asks for. If you
  think something is missing, suggest it instead of building it.
