We're starting a new Rust project called puddlejump: a dotfiles manager for
Linux. Read CLAUDE.md first. It defines the architecture and coding
standards, and they're non-negotiable.

## The idea

puddlejump combines two concepts:

- **Stow-style linking.** A repo holds package directories, each mirroring
  the tree it should appear in. Linking a package symlinks its files into
  place; unlinking removes only the links that point back into the repo.
- **Make-style dependencies.** Each package can declare what it depends on:
  system packages to install, and other puddlejump packages that must be
  linked first. When you link a package, puddlejump works out the full
  dependency graph, installs what's missing, and links everything in the
  right order. There is no separate master list of packages to maintain.
  What's installed follows from what's linked.

This is a fresh design. Don't copy GNU Stow's or Make's quirks; take the
good ideas and keep it simple.

## v1 scope

1. **Packages and linking**: link, unlink, and relink packages into a
   target directory (default `$HOME`). Some packages need to target system
   paths like `/etc`; those need root, and puddlejump must say so up front
   rather than fail halfway through.
2. **Package manifest**: an optional small file per package declaring its
   target, its system package dependencies, its dependencies on other
   puddlejump packages, and an optional post-link hook. A package with no
   manifest is just files to link.
3. **Dependency resolution**: build a dependency graph from the manifests,
   detect cycles and missing packages, and produce an ordered plan.
4. **System package installs**: xbps (Void Linux) is the primary target
   and the only backend to implement in v1, but nothing outside the xbps
   backend may assume xbps. Specifically:
   - Package managers sit behind a small trait (check installed, install
     missing). Adding apt, pacman, dnf, etc. later should mean writing one
     new implementation and nothing else.
   - Detect the system's package manager automatically, with a config
     override.
   - Package names differ between distros, so the manifest needs a simple
     way to give a default name plus per-manager overrides (e.g. a package
     called one thing in xbps and another in apt). Propose the format.
   - If no backend is available, or a dependency has no name for the
     current manager, linking still works; report the missing dependencies
     clearly instead of failing.
   - Only install what's missing, and batch installs into one call.
5. **Hooks**: optional post-link scripts, e.g. to build something from
   source. puddlejump runs them in dependency order and reports failures;
   it is not a build system itself.
6. **Conflicts**: never overwrite a real file. Report conflicts in the
   plan, and provide an explicit `adopt` operation that moves the existing
   file into the package and links it.
7. **Host profiles**: a way to say which packages a given machine should
   have, so one repo can serve several machines. Propose the simplest
   design that works; I'm open to ideas here.
8. **Status**: show what's linked, what isn't, and what's broken or
   conflicting, plus which declared system dependencies are missing.

Out of scope for v1: templating, secrets/encryption, git integration,
snapshots/rollback, and the GUI. The GUI comes later as a thin frontend
over `puddlejump-core`, so design the core API with that in mind: it must
return structured data, not printed text.

## How I want to work

Don't write any code yet. First give me:

1. A proposed on-disk layout for a dotfiles repo, and the manifest and host
   profile formats (keep them minimal; justify each field).
2. The CLI command surface (`link`, `unlink`, `status`, `adopt`, …) with
   flags. Every mutating command supports `--dry-run`.
3. The module breakdown for `puddlejump-core` and the main public types,
   especially the dependency graph and the plan/action types.
4. The dependencies you'd use, with a one-line reason for each.
5. A test plan: for each core module, the main behaviors and edge cases
   you'll cover with unit tests, plus the CLI integration tests.
6. Any open questions or places where my requirements are unclear or
   contradict each other.

Once I approve the design, we'll build it in small steps: workspace
skeleton, then manifests and package discovery, then dependency
resolution, then planning, then applying (links, installs, hooks), then the
CLI. Each step ships with its unit tests (following the Testing section of
CLAUDE.md) and must pass fmt, clippy, and test before we move on. Show me
the test output at the end of each step.
