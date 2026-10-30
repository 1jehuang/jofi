# Jofi versus tofi: measured comparison

Measured October 30, 2026 on the user's Dell XPS 14, Intel Core Ultra X9 388H,
niri Wayland session, 2880×1800 at 120 Hz, display scale 1.75.

## Results

| Metric | Jofi | tofi-drun |
| --- | ---: | ---: |
| Startup median | 20.95 ms | 18.45 ms |
| Startup p95 | 26.31 ms | 28.19 ms |
| Startup mean | 21.84 ms | 19.60 ms |
| Startup min–max | 19.96–27.10 ms | 15.38–28.58 ms |
| Resident memory, median | 31.00 MiB | 57.22 MiB |
| Proportional memory, median | 15.46 MiB | 34.20 MiB |
| Executable size | 2,072,182 bytes | 167,816 bytes |

In this run, Jofi's median startup was **2.49 ms / 13.5% slower**, with
**45.8% lower resident memory**. The slightly better p95 is an observation from
this sample, not evidence of a generally better latency tail. The programs are
close in startup time, but Jofi is not faster on the measured median.

## Method and limitations

- 30 measured launches per app, after 3 warmups each. Order was deterministically
  randomized within each pair, in the same running desktop session.
- Invoked `~/.local/bin/jofi` and `/usr/bin/tofi-drun` directly, using the existing
  tofi config and Jofi's matching built-in theme. No source or config changes
  were made to either launcher for the benchmark.
- Startup is measured from process spawn until the first **content buffer** is
  attached and its Wayland surface committed. Both dimensions must exceed 1px.
  This deliberately excludes tofi's early 1×1 background placeholder, which
  otherwise makes its apparent startup look misleadingly short, around 4ms.
- This is **buffer submission**, not actual screen presentation, keyboard-ready
  latency, or search latency. Compositor scheduling and display refresh add delay.
- Uses `WAYLAND_DEBUG=client` and a parent process to observe protocol writes.
  Instrumentation adds overhead and C/Rust logging implementations differ.
- Memory is sampled from `/proc/PID/status` and `/proc/PID/smaps_rollup` 100ms
  after content submission. RSS includes shared pages; PSS apportions them.
  Short-lived startup helpers, such as Jofi's `fc-match`, are not included in the
  settled-memory figure. Compositor-side memory is not included.
- Warm filesystem/application/font caches, not a cold boot. No system caches
  were dropped. Other normal desktop processes remained running.
- Fractional scaling implementations differ: tofi's content buffer was
  2881×1801, while Jofi's integer-scale buffer was 3292×2058. This compares the
  actual installed behavior, not identical pixel workloads.
- The existing shell hotkey wrapper for tofi is **not** included. That wrapper
  does extra niri/jq/process-management work before invoking tofi.
- No Rust launcher is compiled while opening Jofi. First-time script compilation
  and cached script execution are separate workloads and are not timed here.

## Reproduce

```sh
python3 bench/compare-launchers.py --runs 30 --warmup 3 --output results.json
```

This briefly opens/dismisses each overlay, injects no keyboard input, selects no
applications, and terminates only the launched child process. Do not run while
another instance of either launcher is open. Requires Python 3 and an active
Wayland session. Results use KiB internally and MiB in this table.

Jofi revision: `fd2cedac4eb83910cb9183b3f28b3e5b73677637`.
Installed tofi package: `tofi-jstack r265.bb5617b-2`.

SHA-256 of benchmarked binaries:

```text
jofi 8172144162687e3d81bffb9809a25bc2216bf3c6805de09de038493553f2ee40
tofi 1374b9a5fb2cdce0701e4619bd7bf437912f1f3b220b45738c4d3c7ff4a4a6fd
```

SHA-256 of tofi config:
`c5349de02875a28c0f33cabfc37626e5601cb9be9132d47ff464991d88b3c95e`.

Raw observations are saved locally in
`~/.jcode/scratch/jofi-tofi-benchmark.json`.
