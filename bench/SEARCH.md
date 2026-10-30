# Indexed search quality and performance evidence

Measured October 30, 2026, 02:35:17 PDT, on a Dell XPS 14 DA14260, Intel Core Ultra X9 388H (16 logical CPUs), Linux x86_64. This is a busy development laptop, not an isolated benchmark machine. Sequential baseline/new measurements can experience different scheduler load. These are observed workload timings, not a universal speedup claim or end-to-end GUI latency.

## Reproduce

```sh
justrust check --tests
justrust test search::tests -- --nocapture
justrust build --remote --bench search --release
# If the hosted export does not bring back the bench executable, as observed here:
justrust build --local --bench search --release
# Select the fresh executable produced by this build, not a stale local artifact.
cp target/release/deps/search-1c5bcb8319457b09 ~/.jcode/scratch/jofi-search-bench-tiered-bin
~/.jcode/scratch/jofi-search-bench-tiered-bin /path/to/names.tsv
```

The hash suffix is toolchain/build dependent. The harness also accepts `JOFI_SEARCH_NAMES`; default is `~/.jcode/scratch/jofi-search-apps.tsv`. TSV column one is the name. Missing real input is explicitly reported, not silently replaced by synthetic input. `JOFI_SEARCH_BATCHES` and `JOFI_SEARCH_REPEATS` override the defaults of 31 and 3.

Hosted release build receipt `20261030-021813672-772915` confirms `remote on hosted`. Its artifact export returned the app, not the bench executable, leaving a stale local bench in place. Those stale-binary trial results were discarded. A controlled local build was required to execute the laptop benchmark against its local catalog: receipt `20261030-023509282-939255`, release profile, pinned compiler `jr-ac20de5e7921`. The immutable executable used for the recorded run was copied before execution to avoid shared-target replacement races.

* Binary SHA256: `8f679b7c33b1a364bae132c1223b29af483f96990e42822c8f1d9917a3eafcf8`
* Real TSV SHA256: `7bf76ddf1b013969239aa9d7e341f3b68d1eee53fec2c2cf5de82cd37b9a950f`
* Search source SHA256: `ea544eb6cdd745de9552e12715c0f249153f781837362221e52b31fb8eecec46`
* Harness SHA256: `5770c78473afb4437a1aa6bba8b68c843172d507506fdb879a81518552a7564e`
* Raw local output: `~/.jcode/scratch/jofi-search-bench-20261030-tiered.txt`

## Method

The baseline helpers are copied from `259149f:src/ui.rs` (original lines 138..229), with its original score/index sorting recreated in the benchmark only. Both implementations run in the same executable with identical optimization settings. Every filter allocates full results, produces highlights, and sorts them. The index is built once before warmed filtering. There is no result cache and no GUI launch.

Index construction timing includes destruction of the newly built index. First nonempty-key timing builds the index outside the timer and measures `filter("c")`, including lazy matcher allocation and result destruction. Warmed workloads receive three warmup batches. Each workload has 31 timed batches of three repetitions of its query sequence. The group p50/p95 values below are distributions of **batch-average time per key**, not individual-key tail latency. Individual-key results separately measure 101 timed single calls after three warmup calls.

Queries cover prefix typing (`"", c, ch, chr, chro, chrom, chrome`), initials/swapped tokens (`vsc, ffx, nm, code visual, chrome google, file man`), typos (`chrmoe, chrxome, chome, chrm, caluclat, calxulatxr, chrmoe google`), and unmatched/literal punctuation (`zzzzzz, qzxw, !, ^chrome, absent token, xxxxxxxxxxxx`). Synthetic catalogs cycle through 16 realistic product names and eight edition labels, with numeric workspace suffixes, at 1,000 and 10,000 names. They are intentionally repeatable, not a claim to model every installed catalog.

New rankings and recall legitimately differ: optimal fuzzy scoring, literal punctuation, normalized accents, unordered AND tokens, and partial-prefix typo fallback are new semantics. Strong fuzzy alignments rank above bounded typos, which rank above weak widely scattered alignments. A query of at least four characters is weak only when its matched span exceeds twice its length and not every gap lands at a word/camel boundary. Weak fuzzy candidates also check bounded typo alignment. Thus `chrmoe` correctly selects Google Chrome rather than the distant subsequence in Chromium Browser, while contiguous literal matches and deliberate long boundary acronyms stay protected. Thus these timings compare the same query workload, not identical result sets. The baseline performs normalization/segmentation work on each filter while the new index moves candidate preparation to startup.

## Startup, microseconds (p50 / p95)

| Catalog | Build + drop | First nonempty key, fresh index |
|---|---:|---:|
| Actual 130 apps | 12.4 / 15.2 | 7.5 / 35.6 |
| Synthetic 1k | 168.5 / 192.5 | 34.5 / 79.6 |
| Synthetic 10k | 2115.1 / 2307.4 | 438.3 / 619.2 |

## Warmed groups, microseconds per key (batch-average p50 / p95)

| Catalog | Workload | Old baseline | Indexed |
|---|---|---:|---:|
| Actual 130 | Prefix typing | 32.3 / 34.8 | 5.6 / 7.2 |
| Actual 130 | Initials/tokens | 35.3 / 36.7 | 6.0 / 8.5 |
| Actual 130 | Typos | 60.6 / 62.4 | 7.8 / 10.2 |
| Actual 130 | Unmatched/literal | 45.2 / 46.8 | 4.6 / 6.3 |
| Synthetic 1k | Prefix typing | 364.9 / 374.8 | 67.5 / 68.4 |
| Synthetic 1k | Initials/tokens | 457.9 / 475.2 | 78.7 / 81.2 |
| Synthetic 1k | Typos | 582.1 / 601.9 | 103.3 / 104.7 |
| Synthetic 1k | Unmatched/literal | 416.8 / 443.7 | 52.3 / 52.4 |
| Synthetic 10k | Prefix typing | 2651.4 / 3074.0 | 453.3 / 470.8 |
| Synthetic 10k | Initials/tokens | 3166.6 / 4311.6 | 741.1 / 771.7 |
| Synthetic 10k | Typos | 4762.9 / 5138.8 | 967.3 / 1013.0 |
| Synthetic 10k | Unmatched/literal | 3967.3 / 4204.5 | 608.7 / 693.3 |

## Individual-key latency, microseconds (101 samples, p50 / p95)

| Catalog | Query | Old baseline | Indexed |
|---|---|---:|---:|
| Actual 130 | term | 31.2 / 39.9 | 8.4 / 8.7 |
| Actual 130 | chrmoe | 48.6 / 51.5 | 6.7 / 7.2 |
| Actual 130 | zzzzzz | 45.6 / 49.1 | 5.9 / 6.1 |
| Synthetic 1k | term | 396.4 / 405.0 | 108.5 / 110.7 |
| Synthetic 1k | chrmoe | 485.1 / 488.5 | 86.3 / 87.4 |
| Synthetic 1k | zzzzzz | 424.3 / 426.8 | 70.9 / 73.3 |
| Synthetic 10k | term | 4282.9 / 4466.4 | 1331.8 / 1519.0 |
| Synthetic 10k | chrmoe | 5785.9 / 6152.6 | 1173.1 / 1406.0 |
| Synthetic 10k | zzzzzz | 5040.3 / 5324.6 | 910.6 / 1064.8 |

In this run indexed filtering was lower-latency for every recorded workload and individual query. Actual-catalog individual p95 values were 6.1 to 8.7 microseconds. Synthetic 10k individual p95 values were 1.06 to 1.52 milliseconds. Startup cost is additional and reported separately. Large group tails on the busy machine are visible rather than trimmed.

## Independent quality corpus

`src/search_tests.rs` contains 15 executable tests covering exact/prefix ranking, initials/camel case, unordered AND tokens including mixed fuzzy/typo terms, literal punctuation, Unicode case/accent/decomposed highlights, distinct complex emoji, Cyrillic/Japanese matching, four edit types and partial prefixes, short-query noise, strong-literal-fuzzy-over-typo priority, Google Chrome typo correction over weak Chromium scatter, long word/camel-boundary acronym protection, repeated words, stable original recent/history tie order, and empty input. Helpers validate original indices, sorted unique character-index highlights, and ascending score/index ordering.

The edit-distance oracle uses an independent unbanded full matrix. It compares all 127 binary strings of lengths zero through six against all 127 targets, at budgets two, zero, and one: **48,387 comparisons**, reusing one implementation scratch buffer throughout to exercise growth, shrinkage, early exits, and transpositions. An earlier 12-test corpus passed hosted receipt `20261030-021537276-758629`. Final integrated validation, including complex-emoji/non-Latin handling, optimized DP guards, and the Chrome/Chromium weak-scatter regression, passed on hosted: `justrust check` receipt `20261030-023447556-937025`, `justrust test` receipt `20261030-023448353-937177` (33 passed, zero failed, one ignored), and `justrust clippy --all-targets -- -D warnings` receipt `20261030-023449265-937346`. The launcher uses this same SearchIndex API for live filtering.

## Real installed-launcher workflow acceptance

Run the actual launcher against real niri, in a completely private offscreen
Xvfb display, with real XTest keyboard events and fixture-only desktop entries:

```sh
python3 bench/search-workflow.py --jofi ~/.local/bin/jofi
```

Requires installed `Xvfb`, `xdotool`, `niri`, and `gio`. The harness creates a
private Xauthority cookie, disables X11 TCP listening, and uses private runtime,
Wayland/IPC sockets, config, data, scripts, history and cache directories. Nested
niri is never started with `--session`. No key is sent to the user's active
desktop, no real app is launched, and no compositor config is changed. Fixture
entries invoke only a Python marker through actual `gio launch`, so the test
checks the real keyboard, filtering, selection, desktop identity, execution and
history paths rather than a mocked launcher callback. Only owned child PIDs are
stopped. Raw protocol logs and results stay in the scratch artifact directory.

The first real interactive run exposed an important ranking defect: `chrmoe`
selected Chromium Browser instead of Google Chrome because any subsequence had
previously outranked every typo correction. The revised weak-scatter tier fixes
that case while preserving strong literal matches and boundary acronyms.

All **13 cases passed** against both the built release and the atomically
installed executable on October 30, 2026: transposed typo, unordered mixed
fuzzy/typo terms, two substitutions, abbreviation, accent normalization,
Backspace repair, Down selection, Up wrapping, Tab, Shift+Tab, empty-query
recency, Enter with no results followed by Ctrl+U recovery, and literal
punctuation followed by Escape cancellation. Selections were checked against
fixture launch markers and saved desktop-file history identities. The installed
executable's SHA256 matched the validated release:

`49f83d2e4f04ac98b29b244dcf90fa4d03c62a1b115e8a76ef219cf3c9334941`

Installed-path acceptance artifacts:
`~/.jcode/scratch/jofi-search-workflow-t0qytyfj/results.json`.
The previous installed binary was preserved at
`~/.jcode/scratch/jofi-before-search-correction-20261030-0937`.
These functional checks are separate from the filter microbenchmarks above.

## Launch-frequency tracking follow-up

The October 30, 2026 usage-tracking follow-up adds locked, private local counts
and timestamps for desktop entries and Rust launchers. It does not change the
matcher or remeasure the filtering performance tables above. Empty-query
ordering now promotes frequency, followed by last-use time and legacy recency.

Validation after integrating the usage implementation (`f1963da`):

- Hosted check: `20261030-025923443-1153109`, passed.
- Hosted complete tests: `20261030-025805394-1139726`, 38 passed, 1 ignored.
- Hosted strict all-target Clippy: `20261030-030026791-1158509`, passed.
- Hosted release build: `20261030-025924178-1153219`, passed.
- `shellcheck contrib/jofi-workspace` and `python3 bench/test-workspace.py`,
  passed. The wrapper test uses fixture commands, not the user's compositor.
- All 13 real nested-compositor cases passed against the installed release,
  now also asserting exact persisted per-entry usage counts after launches and
  unchanged usage on cancellation. Installed executable SHA256:
  `4d66d984420874d2e118fea9594097d9a065479aebb4851cd7afa45cf619276d`.

Integration tests additionally exercise idempotent Tofi import by unique name,
desktop filename and full path, preserving local increments, rejecting
ambiguous/malformed records, missing executable and compilation failures,
12 concurrent desktop launches, and 160 threaded locked updates with private
permissions. No actual user history, imported data, or compositor configuration
is checked into this repository.

## Reported typo examples and editing shortcuts follow-up

The later October 30 follow-up (`603535c`, `f8beb1a`, `d13aa3b`) addresses the
actual reported queries `vekstpo` → Vesktop and `fireofv x` → Firefox. Two OSA
edits are now allowed from **seven** normalized alphanumeric characters, rather
than eight. Four-to-six-character terms still allow only one edit and shorter
terms remain strict. Independent AND terms may match overlapping text in an
entry. Exact/prefix, dense fuzzy, boundary-acronym, typo, and weak-scatter
ranking policy remains intact. The quality corpus now contains 18 tests,
including screenshot-example first-place/highlight checks and threshold/noise
regressions. The independent 48,387-comparison oracle remains unchanged.

Alt/Ctrl+A/C/X/V, selected-text replacement/deletion, Unicode-grapheme Backspace,
and Alt/Ctrl+Backspace word deletion are tested with actual keys. Clipboard
operations use bounded worker-thread helpers. A deliberately failing helper
also verifies that a cut cannot erase the query or launch stale results while
pending. All **26 workflow cases** passed both directly and with the desktop's
existing Alt-to-Ctrl `wtype` bindings, and passed again against the installed
hosted release with those bindings. Clipboard tests address only the private
nested compositor, never the user's real clipboard.

Final validation receipts:

- Hosted check: `20261030-031615583-1259122`, passed.
- Hosted full tests: `20261030-031616242-1259247`, **52 passed, 1 ignored**.
- Hosted strict all-target Clippy: `20261030-031618073-1259399`, passed.
- Explicit hosted release: `20261030-031820431-1279669`, passed. The earlier
  automatic release route chose a busy local machine, so it was replaced by
  this explicitly hosted artifact before installation.
- Installed SHA256:
  `3d052b9742b96f95993cea18c65d0d45a08649fcdc2b3d08aa01aa403badef19`.

Reproduce the desktop-remapping workflow with:

```sh
python3 bench/search-workflow.py --remap-alt --jofi ~/.local/bin/jofi
```

Fresh filtering measurements used the same original baseline and actual
130-name TSV as above, with the two reported queries added to the individual-key
101-sample suite. Controlled local benchmark build receipt:
`20261030-031330310-1235376`. The executable was copied immutably before running,
with SHA256 `ce1767b408c7f584eea92d0775dd8acbf9c8a64e3a6491db9190bbcc1310984d`.
Matcher source SHA256:
`7fae0d65006a65a57b98f3df53723a4e4b4e830f583901d4533e924d10df07af`.
Harness SHA256:
`1826a81e6669c945a1d96ea082a71f9f087e3920cc7ec4b510c7b7a1b009afc1`.
The same local-only benchmark requirement and busy-laptop caveats above apply.

| Query | Actual 130 indexed p95 (µs) | Original baseline p95 (µs) | Synthetic 10k indexed p95 (ms) |
| --- | ---: | ---: | ---: |
| term | 12.4 | 46.5 | 1.859 |
| chrmoe | 9.5 | 68.5 | 1.104 |
| zzzzzz | 8.6 | 64.4 | 0.909 |
| vekstpo | 16.0 | 65.0 | 1.694 |
| fireofv x | 17.3 | 78.5 | 1.796 |

These are filter timings, not clipboard or GUI latency. The original baseline
does not recover the reported two-error examples, so output sets differ.
The earlier tables remain historical measurements of the earlier policy, not
claims about the current executable or controlled before/after attribution.
