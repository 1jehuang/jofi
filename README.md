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

The default UI follows the existing tofi setup: fullscreen translucent black,
24-point monospace white text, tomato selection, orange-red matches, 35%
left/top inset, and five visible results with 25 logical pixels between line
boxes. Font sizes use point-to-em conversion, not ab_glyph's line-height scale.
Fractional display scaling renders directly at the preferred scale, with an
integer-scale fallback. Selection backgrounds and alternating result colors
follow the tofi theme. Uses the `launcher` layer namespace so an existing
niri blur rule matching `^(tofi|launcher)$` also applies to Jofi.

`bench/compare-appearance.py` captures both launchers without selecting an app
and measures foreground pixel bounds. Screenshots remain private under
`~/.jcode/scratch`. On the 1.75× display, the calibrated same-label comparison
matched tofi's cursor bounds exactly (1008, 630 through 1012, 708), and matched
all five result rows' horizontal ink bounds. Rasterization is not pixel-identical:
ab_glyph and tofi's Pango/Cairo differ in hinting and antialiasing, with measured
vertical ink-bound differences of up to two physical pixels.

## Build and install

Requires Rust, Wayland, Fontconfig, a monospace font, and development libraries
for Wayland and xkbcommon. Normal launching does not spawn `fc-match`. `gio` is required to launch desktop entries.
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
support `wlr-layer-shell`, `wl_subcompositor`, and `wp_viewporter`, as niri does.
No X11 fallback is provided.

Optional niri binding, replacing the existing launcher line when ready:

```kdl
Alt+Space hotkey-overlay-title="App Launcher" { spawn "jofi"; }
```

For local desktop helpers that use `JSTACK_ORIGIN_*` context or a launcher that
restores the original niri workspace after launching, install the optional
wrapper (requires `bash`, `niri`, and `jq`):

```sh
install -Dm755 contrib/jofi-workspace ~/.local/bin/jofi-workspace
```

Bind it using an absolute path so the compositor's `PATH` cannot select an old
launcher:

```kdl
Alt+Space hotkey-overlay-title="App Launcher" { spawn "/home/YOU/.local/bin/jofi-workspace"; }
```

The wrapper forwards arguments without a shell, preserves launch-origin window
and workspace identifiers, and does not kill Tofi prompts. Back up your niri
configuration before replacing its app-launcher binding. Other Tofi-based menus
(power, window selection, calculator) are independent and can remain installed.
Your existing visible `.desktop` launchers, including ones that invoke scripts,
are discovered directly and do not need copying or conversion to Rust.

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

Type to filter with subsequence matching and conservative typo tolerance, arrows
or Tab to select, Enter to launch, Escape to dismiss, and Ctrl+U to clear the query.
Alt+A selects all query text, Alt+C copies the selection, Alt+X cuts it, and
Alt+V pastes. Ctrl+A/C/X/V work too, including when a compositor translates the
Alt keys into Ctrl keys. Typing or Backspace replaces/deletes selected text.
Alt+Backspace (or Ctrl+Backspace) deletes the previous whitespace-delimited
query word. Ordinary Backspace deletes one complete Unicode grapheme.

Clipboard shortcuts optionally require `wl-clipboard` (`wl-copy` and `wl-paste`,
Arch package `wl-clipboard`). Helpers start only on a clipboard shortcut and run
off the UI thread, with a two-second timeout and a 64 KiB paste limit. Failed
copy/cut preserves the selected query, and delayed clipboard results never
overwrite newer edits. Enter is ignored while a clipboard operation is pending
so it cannot launch a stale result. Copy/cut uses the normal system clipboard,
which may also be retained by your clipboard manager. The clipboard-owner
helper can remain alive after Jofi exits to keep copied text available.

Frequently launched entries are promoted on the next opening, with last-use
time and legacy recent ordering breaking ties. Search relevance still takes
priority over frequency. Desktop apps, desktop entries that invoke scripts, and
native Rust launchers all use the same path-identified usage tracking.
An advisory runtime lock prevents duplicate overlays.

Usage stays **local only** in `$XDG_STATE_HOME/jofi/usage.tsv` (default
`~/.local/state/jofi/usage.tsv`). The file stores total launch count, last-launch
Unix milliseconds, imported count, and launcher identity. The legacy
`jofi/recent` file remains available. Storage updates use an advisory lock and
atomic rename, with private directory/file permissions. There is no telemetry,
network reporting, or persistent Jofi daemon. Search text and script arguments
are never saved to usage state. Only explicit copy/cut writes selected search
text to the system clipboard.

```sh
jofi usage                         # Inspect stored counts and last-launch times
jofi import-tofi ~/.local/state/tofi-drun-history
```

Import explicitly matches Tofi's `COUNT Display Name` history to unique visible
desktop entries (full desktop paths and filenames also work). Repeating import
does not duplicate counts, and newer larger source counts contribute only their
increase. Local Jofi launches are preserved. Ambiguous, unavailable, hidden, or
malformed source records are skipped and reported, not guessed or resurrected.
The source history is never modified, and imported counts do not invent past
launch timestamps. Existing Jofi recent ordering remains a tie-breaker even
when no count history exists.

Desktop usage is recorded after `gio launch` reports success. Rust usage is
recorded after successful compilation and process creation, not after script
completion. A script that later exits unsuccessfully still counts as launched.
Compilation errors, process-creation failures, cancellation, and empty-result
Enter do not add launches. Script execution is not monitored after the GUI
launcher exits.

Search uses an indexed optimal fuzzy matcher with word/camel-case bonuses,
case/accent normalization, and whitespace-separated terms in any order.
Punctuation is literal. Word-prefix typo recovery allows one edit from four
characters and two from seven, including adjacent transpositions. Strong fuzzy
matches stay first, but a good typo correction beats a weak scattered match:
`chrmoe` finds Google Chrome before Chromium Browser. Short queries stay strict
to limit noise. Two-error examples `vekstpo` and `fireofv x` find Vesktop and
Firefox respectively. Ranking, filtering benchmarks, and isolated keyboard-to-launch
acceptance are documented in `bench/SEARCH.md`.

## Startup design

The fullscreen dim background is a single stretched pixel. Only the small text
region is rasterized, with synchronized parent/child commits so the first visible
frame includes the menu, not just an empty overlay. Catalog scanning, font
loading, and compositor initialization overlap. Desktop files are still scanned
live on each opening, with borrowed-field parsing rather than a stale app index.

Fontconfig matching is cached in `$XDG_CACHE_HOME/jofi/font-match`. Cache hits
validate recursive config, font, and font-cache metadata, symlinks, relevant
environment variables, and the selected face. Font files are still opened and
parsed. Fonts/config changes, corruption, or unsupported configuration details
fall back to fresh native matching. Cache misses do more work than warm opens.
Fontconfig and its native dependencies load lazily only on a miss. No daemon is
required, and there is no hardcoded font path.

Set `JOFI_PROFILE_STARTUP=1 jofi` for opt-in stage timings. For repeatable warm
startup and memory comparisons, use `bench/compare-launchers.py`. This measures
real text-buffer submission, including the synchronized parent commit, not just
the one-pixel background. It includes process launch and Wayland debug overhead,
but is not a measurement of physical screen presentation.

On October 30, 2026, before correcting the font from the undersized 24-pixel
line-height scale to tofi's 24-point size, 30 interleaved warm trials per launcher
(three warmups) on the current niri session measured:

| Launcher | Median content submission | p95 | Median RSS |
| --- | ---: | ---: | ---: |
| Optimized Jofi | 4.32 ms | 7.02 ms | 5.46 MiB |
| Original Jofi | 21.95 ms | 25.48 ms | 31.00 MiB |
| tofi-drun | 18.31 ms | 22.01 ms | 57.27 MiB |

The earlier warm median met the 5 ms content-submission target, not a 5 ms
worst-case or input-readiness guarantee. It does not describe the corrected,
larger typography. After matching tofi's sizing and integrating indexed search,
a new 30-trial comparison measured 7.92 ms median Jofi content submission versus
26.44 ms for tofi-drun on the same running desktop (p95 25.67 ms versus 36.15 ms).
Median content plus keyboard focus was 12.86 ms. These runs were noisy, so they
are observations rather than a guarantee or an isolated attribution of cost.
A first launch after cache invalidation must perform native font matching and
write the dependency manifest, so these warm figures do not describe cold opens.

This intentionally starts small: no icon grid, theme language, dependency
resolver for scripts, stdin menu mode, or tofi compatibility layer. Existing
compositor-specific shell wrapper behavior is not automatically imported.
