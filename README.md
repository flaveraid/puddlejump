# puddlejump

Dotfiles manager for Linux: Stow-style symlinking with Make-style dependency resolution that installs what each package needs.

## Install

```
cargo install --path crates/puddlejump
```

## Repo layout

```
dotfiles/
  packages/
    zsh/
      .zshrc                 # linked to ~/.zshrc
    dwm/
      manifest.toml          # optional
      build.sh
      .config/dwm/config.h
  hosts/
    laptop.toml              # optional: packages for one machine
```

Each package mirrors the tree it should appear in. Only files are linked.

## manifest.toml

All fields are optional.

```toml
target = "~"            # default "~"; or an absolute path like "/etc"
needs_root = false      # true if linking needs root
depends = ["zsh"]       # packages linked first
hook = "build.sh"       # script run after linking

[packages]              # system packages, per package manager
xbps = ["base-devel", "fd"]
```

Only xbps (Void Linux) is supported. If no package manager is found, linking still works and the missing packages are reported.

## Host profiles

`hosts/<name>.toml` lists the packages a machine wants. Dependencies come along automatically.

```toml
packages = ["zsh", "dwm"]
```

The host is chosen by `--host`, then `$PUDDLEJUMP_HOST`, then the machine's hostname.

## Usage

Run from the repo directory, or pass `--repo <dir>`.

```
puddlejump link [packages...]     # link, install system packages, run hooks
puddlejump unlink [packages...]   # remove links
puddlejump relink [packages...]   # unlink then link (files only)
puddlejump adopt <package> <paths...>   # move existing files into a package and link them
puddlejump hook <packages...>     # run hooks only
puddlejump status [packages...]   # what is linked, missing, or in the way
puddlejump list                   # packages and host profiles
```

With no packages named, `link` uses the host profile, or asks before linking everything.

Flags: `-n/--dry-run` (everything that changes files), `-y/--yes`, `--no-install`, `--no-hooks`, `--prune` (unlink, relink), `--target <dir>`, `--json`.

Existing files are never overwritten. Conflicts are reported and nothing is changed; use `adopt` to take over a file.

Exit codes: `0` success, `1` something failed while running, `2` refused before changing anything.
