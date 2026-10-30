# Indexed search quality and performance evidence

Measured October 30, 2026, 02:20:47 PDT, on a Dell XPS 14 DA14260, Intel Core Ultra X9 388H (16 logical CPUs), Linux x86_64. This is a busy development laptop, not an isolated benchmark machine. Sequential baseline/new measurements can experience different scheduler load. These are observed workload timings, not a universal speedup claim or end-to-end GUI latency.

## Reproduce

```sh
justrust check --tests
justrust test search::tests -- --nocapture
justrust build --remote --bench search --release
# If the hosted export does not bring back the bench executable, as observed here:
justrust build --local --bench search --release
# Select the fresh executable produced by this build, not a stale local artifact.
cp target/release/deps/search-1c5bcb8319457b09 ~/.jcode/scratch/jofi-search-bench-verified-bin
~/.jcode/scratch/jofi-search-bench-verified-bin /path/to/names.tsv
```

The hash suffix is toolchain/build dependent. The harness also accepts `JOFI_SEARCH_NAMES`; default is `~/.jcode/scratch/jofi-search-apps.tsv`. TSV column one is the name. Missing real input is explicitly reported, not silently replaced by synthetic input. `JOFI_SEARCH_BATCHES` and `JOFI_SEARCH_REPEATS` override the defaults of 31 and 3.

Hosted release build receipt `20261030-021813672-772915` confirms `remote on hosted`. Its artifact export returned the app, not the bench executable, leaving a stale local bench in place. Those stale-binary trial results were discarded. A controlled local build was required to execute the laptop benchmark against its local catalog: receipt `20261030-021944261-781729`, release profile, pinned compiler `jr-ac20de5e7921`. The immutable executable used for the recorded run was copied before execution to avoid shared-target replacement races.

* Binary SHA256: `90c40b7d4572d88c4120e4542e476c8e1254b346690ecb27be3fa57a9959c1c8`
* Real TSV SHA256: `7bf76ddf1b013969239aa9d7e341f3b68d1eee53fec2c2cf5de82cd37b9a950f`
* Search source SHA256: `670b4fea394832abbe70a82dcad7f1b8822a06b911df52b31be2dd76adc5bfa9`
* Harness SHA256: `5770c78473afb4437a1aa6bba8b68c843172d507506fdb879a81518552a7564e`
* Raw local output: `~/.jcode/scratch/jofi-search-bench-20261030-verified.txt`

## Method

The baseline helpers are copied from `259149f:src/ui.rs` (original lines 138..229), with its original score/index sorting recreated in the benchmark only. Both implementations run in the same executable with identical optimization settings. Every filter allocates full results, produces highlights, and sorts them. The index is built once before warmed filtering. There is no result cache and no GUI launch.

Index construction timing includes destruction of the newly built index. First nonempty-key timing builds the index outside the timer and measures `filter("c")`, including lazy matcher allocation and result destruction. Warmed workloads receive three warmup batches. Each workload has 31 timed batches of three repetitions of its query sequence. The group p50/p95 values below are distributions of **batch-average time per key**, not individual-key tail latency. Individual-key results separately measure 101 timed single calls after three warmup calls.

Queries cover prefix typing (`"", c, ch, chr, chro, chrom, chrome`), initials/swapped tokens (`vsc, ffx, nm, code visual, chrome google, file man`), typos (`chrmoe, chrxome, chome, chrm, caluclat, calxulatxr, chrmoe google`), and unmatched/literal punctuation (`zzzzzz, qzxw, !, ^chrome, absent token, xxxxxxxxxxxx`). Synthetic catalogs cycle through 16 realistic product names and eight edition labels, with numeric workspace suffixes, at 1,000 and 10,000 names. They are intentionally repeatable, not a claim to model every installed catalog.

New rankings and recall legitimately differ: optimal fuzzy scoring, literal punctuation, normalized accents, unordered AND tokens, and partial-prefix typo fallback are new semantics. Thus these timings compare the same query workload, not identical result sets. The baseline performs normalization/segmentation work on each filter while the new index moves candidate preparation to startup.

## Startup, microseconds (p50 / p95)

| Catalog | Build + drop | First nonempty key, fresh index |
|---|---:|---:|
| Actual 130 apps | 33.1 / 38.2 | 21.1 / 68.6 |
| Synthetic 1k | 383.5 / 779.2 | 86.1 / 153.1 |
| Synthetic 10k | 3207.0 / 3304.5 | 745.5 / 936.6 |

## Warmed groups, microseconds per key (batch-average p50 / p95)

| Catalog | Workload | Old baseline | Indexed |
|---|---|---:|---:|
| Actual 130 | Prefix typing | 85.4 / 268.0 | 14.2 / 30.6 |
| Actual 130 | Initials/tokens | 99.9 / 210.6 | 15.2 / 73.1 |
| Actual 130 | Typos | 185.7 / 311.8 | 18.7 / 22.6 |
| Actual 130 | Unmatched/literal | 126.5 / 448.8 | 11.8 / 13.5 |
| Synthetic 1k | Prefix typing | 663.0 / 884.5 | 112.4 / 123.5 |
| Synthetic 1k | Initials/tokens | 737.9 / 767.5 | 127.2 / 138.7 |
| Synthetic 1k | Typos | 841.8 / 968.5 | 139.5 / 141.8 |
| Synthetic 1k | Unmatched/literal | 540.7 / 560.7 | 69.3 / 71.6 |
| Synthetic 10k | Prefix typing | 6370.0 / 10477.1 | 797.5 / 889.8 |
| Synthetic 10k | Initials/tokens | 5306.9 / 6301.0 | 949.2 / 1064.1 |
| Synthetic 10k | Typos | 6088.8 / 6861.4 | 1224.7 / 1350.6 |
| Synthetic 10k | Unmatched/literal | 4457.2 / 5916.9 | 435.6 / 588.0 |

## Individual-key latency, microseconds (101 samples, p50 / p95)

| Catalog | Query | Old baseline | Indexed |
|---|---|---:|---:|
| Actual 130 | term | 78.8 / 103.0 | 21.9 / 22.7 |
| Actual 130 | chrmoe | 119.9 / 142.7 | 16.9 / 17.2 |
| Actual 130 | zzzzzz | 112.1 / 117.6 | 15.8 / 16.2 |
| Synthetic 1k | term | 488.7 / 509.9 | 138.0 / 143.1 |
| Synthetic 1k | chrmoe | 632.5 / 655.9 | 108.0 / 111.6 |
| Synthetic 1k | zzzzzz | 542.6 / 547.6 | 94.8 / 97.8 |
| Synthetic 10k | term | 4869.3 / 5321.0 | 1326.5 / 1346.6 |
| Synthetic 10k | chrmoe | 5870.8 / 6061.6 | 1112.0 / 1164.6 |
| Synthetic 10k | zzzzzz | 5489.6 / 6196.4 | 915.8 / 1057.9 |

In this run indexed filtering was lower-latency for every recorded workload and individual query. Actual-catalog individual p95 values were 16.2 to 22.7 microseconds. Synthetic 10k individual p95 values were 1.06 to 1.35 milliseconds. Startup cost is additional and reported separately. Large group tails on the busy machine are visible rather than trimmed.

## Independent quality corpus

`src/search_tests.rs` contains 13 executable tests covering exact/prefix ranking, initials/camel case, unordered AND tokens including mixed fuzzy/typo terms, literal punctuation, Unicode case/accent/decomposed highlights, distinct complex emoji, Cyrillic/Japanese matching, four edit types and partial prefixes, short-query noise, fuzzy-over-typo priority, repeated words, stable original recent/history tie order, and empty input. Helpers validate original indices, sorted unique character-index highlights, and ascending score/index ordering.

The edit-distance oracle uses an independent unbanded full matrix. It compares all 127 binary strings of lengths zero through six against all 127 targets, at budgets two, zero, and one: **48,387 comparisons**, reusing one implementation scratch buffer throughout to exercise growth, shrinkage, early exits, and transpositions. An earlier 12-test corpus passed hosted receipt `20261030-021537276-758629`. Final integrated validation, including the additional complex-emoji/non-Latin test and optimized DP guard handling, passed on hosted: `justrust check` receipt `20261030-022006905-785665`, `justrust test` receipt `20261030-022007596-785902` (31 passed, zero failed, one ignored), and `justrust clippy --all-targets -- -D warnings` receipt `20261030-022008534-785659`. The launcher uses this same SearchIndex API for live filtering.
