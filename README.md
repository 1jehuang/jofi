# Jofi

A small, native Rust application launcher for Wayland. A fresh implementation,
not a wrapper around tofi. Uses a layer-shell overlay, shared-memory software
rendering, and no browser engine, GPU toolkit, or background daemon.

## What it launches

- Normal `.desktop` apps from `$XDG_DATA_HOME/applications` and
  `$XDG_DATA_DIRS/applications`, including Flatpak directories when present in
  `XDG_DATA_DIRS`. User overrides, hidden entries, desktop visibility, and
  `TryExec` are respected. Desktop launch semantics are delegated to `gio launch`
  rather than interpreting `Exec` as a shell command.
- Standalone Rust programs from `~/.config/jofi/scripts/*.rs`. They do **not**
  need a `.desktop` file or installation into a standard app directory.
  Override the directory with `JOFI_SCRIPTS_DIR`.

The default UI matches the existing tofi setup: fullscreen translucent black,
monospace white text, tomato selection, orange-red matches, 35% left/top inset,
and five visible results. Uses the `launcher` layer namespace so an existing
niri blur rule matching `^(tofi|launcher)$` also applies to Jofi.

## Build and install

Requires Rust, Wayland, fontconfig (`fc-match`), a monospace font, and development
libraries for Wayland and xkbcommon. `gio` is required to launch desktop entries.
On Arch: `pacman -S rust wayland libxkbcommon fontconfig glib2`.

```sh
justrust check
justrust test
justrust build --release
install -Dm755 target/release/jofi ~/.local/bin/jofi
mkdir -p ~/.config/jofi/scripts
jofi
```

Plain Cargo also works outside this development environment. The compositor must
support `wlr-layer-shell`, as niri does. No X11 fallback is provided.

Optional niri binding, replacing the existing launcher line when ready:

```kdl
Alt+Space hotkey-overlay-title="App Launcher" { spawn "jofi"; }
```

## Rust launchers

Save this as `~/.config/jofi/scripts/browser.rs`:

```rust
// jofi-name: Browser work profile
use std::{io, os::unix::process::CommandExt, process::Command};

fn main() -> io::Result<()> {
    Err(Command::new("firefox").arg("-P").arg("work").exec())
}
```

The optional `// jofi-name:` comment controls the displayed name. Otherwise the
filename is used, with hyphens and underscores converted to spaces.

```sh
jofi build                    # Precompile all scripts after edits
jofi build --force            # Rebuild, e.g. after changing Rust toolchains
jofi launch 'Browser work profile'
jofi list                     # Names, source types, and paths
jofi paths                    # Resolved source/cache directories
```

Compilation happens only when explicitly building or selecting an uncached
script, never while opening the menu or searching. A cache hit runs the existing
binary. Source-directory contents are fingerprinted so edits to local modules
and assets also invalidate the cache. Binaries are published with atomic rename,
so failed or concurrent builds cannot leave a partially written cached binary.
The cache lives under `$XDG_CACHE_HOME/jofi/bin` (default `~/.cache/jofi/bin`).

Scripts use Rust 2021 and `std` only, without a Cargo manifest or third-party
crate dependencies. Local `mod` and relative include files work. Put helper `.rs`
files in a subdirectory so they are not indexed as separate launchers. External
files and symlinked directories are not dependency-tracked. Run `jofi build
--force` after changing those or changing the compiler. Keep the scripts directory
small because its contents are read when checking a script cache entry.

Scripts are **trusted local executable code**, not sandboxed. Do not install
untrusted launchers. Jofi never executes a script merely to discover its name.
GUI launch failures use `notify-send` when available, with details also on stderr.

## Interaction and state

Type to filter, arrows or Tab to select, Enter to launch, Escape to dismiss, and
Ctrl+U to clear the query. Recent launches are promoted on the next opening and
stored in `$XDG_STATE_HOME/jofi/recent`. An advisory runtime lock prevents duplicate
overlays. There is no telemetry or persistent background process.

This intentionally starts small: no icon grid, theme language, dependency
resolver for scripts, stdin menu mode, or tofi compatibility layer. Existing
compositor-specific shell wrapper behavior is not automatically imported.
