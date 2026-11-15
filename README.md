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

The host is chosen by `--host`, then `$PUDDLEJUMP_HOST`, then `host` in your user config, then the machine's hostname.

## User config

To avoid passing `--repo` and `--host` every time, set them in `~/.config/puddlejump/config.toml` (or `$XDG_CONFIG_HOME/puddlejump/config.toml`). Both keys are optional.

```toml
repo = "~/dotfiles"     # absolute, or starting with ~/
host = "laptop"         # a profile in hosts/
```

The repo is chosen by `--repo`, then `$PUDDLEJUMP_REPO`, then `repo` here, then the current directory.

## Usage

Create a repo with `puddlejump init ~/dotfiles` (it writes starter files that document every option), then run from it, or pass `--repo <dir>`.

```
puddlejump link [packages...]     # link, install system packages, run hooks
puddlejump unlink [packages...]   # remove links
puddlejump relink [packages...]   # unlink then link (files only)
puddlejump adopt <package> <paths...>   # move existing files into a package and link them
puddlejump hook <packages...>     # run hooks only
puddlejump status [packages...]   # what is linked, missing, or in the way
puddlejump list                   # packages and host profiles
puddlejump init <path>            # create a new dotfiles repo at <path>
```

With no packages named, `link` uses the host profile, or asks before linking everything.

Flags: `-n/--dry-run` (everything that changes files), `-y/--yes`, `--no-install`, `--no-hooks`, `--prune` (unlink, relink), `--target <dir>`, `--json`.

Run puddlejump as your own user, not with `sudo`; it refuses to run under `sudo` and uses `sudo` itself when a package needs root.

Existing files are never overwritten. Conflicts are reported and nothing is changed; use `adopt` to take over a file.

Exit codes: `0` success, `1` something failed while running, `2` refused before changing anything.
