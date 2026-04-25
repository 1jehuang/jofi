# jofi

`jofi` is a fast, typo-resistant application launcher focused only on `.desktop` entries.

The first milestone is the engine and profiler:

- discover launchable `.desktop` files
- treat user scripts wrapped by `.desktop` files as first-class launcher entries
- rank with typo-resistant search
- build launch commands from `Exec` safely without a shell
- log performance and memory telemetry as JSONL

The Wayland UI comes next. This repo intentionally does **not** aim to be a full `tofi`, `rofi`, or `dmenu` replacement.

## Scope

Included:

- `.desktop` application discovery
- `Name`, `GenericName`, `Comment`, `Keywords`, `Categories`, `Exec`, `Icon`, `Terminal`
- typo-resistant search with prefix, acronym, substring, subsequence, and Damerau-Levenshtein scoring
- direct launching
- first-class profiling/telemetry

Excluded from the product vision:

- stdin/dmenu mode
- raw `$PATH` executable launcher
- rofi/dmenu compatibility flags
- stdout selection protocol
- giant theming/config system

## Usage

```sh
cargo run -- profile
cargo run -- search chrmoe
cargo run -- launch firefox --dry-run
cargo run -- list --limit 20
```

Telemetry is on by default and logs JSONL to:

```text
$XDG_STATE_HOME/jofi/telemetry.jsonl
```

Disable it with:

```sh
jofi --no-telemetry search firefox
```

or choose another path:

```sh
jofi --telemetry-log /tmp/jofi.jsonl profile
```

## Profiling

```sh
cargo run --release -- profile \
  --query firefox \
  --query chrmoe \
  --query hotspot \
  --runs 1000
```

The profile command prints discovery time, index-build time, search timing, RSS memory, virtual memory, and the telemetry log path.

Each telemetry span records:

- `duration_ns`
- `rss_kib`
- `vm_size_kib`
- `rss_delta_kib`
- `vm_size_delta_kib`
- span-specific fields, such as entry counts and query result counts

## Script launcher entries

Recommended script workflow:

```text
~/.local/bin/play-song
~/.local/share/applications/play-song.desktop
```

Example `.desktop` file:

```ini
[Desktop Entry]
Type=Application
Name=Play Song
Comment=Play my default music
Exec=/home/jeremy/.local/bin/play-song
Icon=media-playback-start
Terminal=false
Categories=Utility;
Keywords=music;song;audio;
```

`jofi` indexes this exactly like a normal app, with strong `Name` and `Keywords` ranking.
