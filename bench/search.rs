//! End-to-end filter benchmark, including result allocation, sorting and highlights.
//! Baseline helpers below are copied verbatim from 259149f:src/ui.rs (138..229).
#[path = "../src/search.rs"]
mod search;
use search::SearchIndex;
use std::{hint::black_box, time::Instant};
type Matches = Vec<(usize, Vec<usize>, i64)>;

fn baseline(names: &[String], query: &str) -> Matches {
    let mut matches: Matches = names
        .iter()
        .enumerate()
        .filter_map(|(i, n)| fuzzy(n, query).map(|(p, s)| (i, p, s)))
        .collect();
    matches.sort_by_key(|m| (m.2, m.0));
    matches
}
fn percentile(samples: &mut [f64], p: f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[((samples.len() - 1) as f64 * p).ceil() as usize]
}
fn measure(mut f: impl FnMut(), batches: usize, repeats: usize) -> (f64, f64) {
    for _ in 0..3 {
        f();
    }
    let mut samples = Vec::with_capacity(batches);
    for _ in 0..batches {
        let start = Instant::now();
        for _ in 0..repeats {
            f();
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / repeats as f64);
    }
    let median = percentile(&mut samples, 0.50);
    (median, percentile(&mut samples, 0.95))
}
fn synthetic(n: usize) -> Vec<String> {
    let products = [
        "Visual Studio Code",
        "Google Chrome",
        "Firefox",
        "Terminal",
        "Calculator",
        "NetworkManager",
        "Document Viewer",
        "System Settings",
        "Café Notes",
        "GitHub Desktop",
        "Bitwarden",
        "Signal",
        "Music Player",
        "File Manager",
        "Screenshot",
        "Thunderbird",
    ];
    let editions = [
        "",
        " Beta",
        " Developer",
        " Workspace",
        " Personal",
        " Enterprise",
        " Preview",
        " Remote",
    ];
    (0..n)
        .map(|i| {
            format!(
                "{}{} {}",
                products[i % products.len()],
                editions[(i / products.len()) % editions.len()],
                i / (products.len() * editions.len()) + 1
            )
        })
        .collect()
}
fn run(label: &str, names: &[String], batches: usize, repeats: usize) {
    let build = measure(
        || {
            black_box(SearchIndex::new(black_box(names)));
        },
        batches,
        repeats,
    );
    println!(
        "\n{label}: {} names | index build p50 {:.1} us p95 {:.1} us",
        names.len(),
        build.0,
        build.1
    );
    let mut first_keys = Vec::with_capacity(batches);
    for _ in 0..batches {
        let mut fresh = SearchIndex::new(names);
        let start = Instant::now();
        black_box(fresh.filter(black_box("c")));
        first_keys.push(start.elapsed().as_secs_f64() * 1e6);
    }
    let first_p50 = percentile(&mut first_keys, 0.50);
    let first_p95 = percentile(&mut first_keys, 0.95);
    println!("  first nonempty key p50 {first_p50:.1} us p95 {first_p95:.1} us (fresh index, lazy matcher included)");
    let groups: &[(&str, &[&str])] = &[
        (
            "prefix typing",
            &["", "c", "ch", "chr", "chro", "chrom", "chrome"],
        ),
        (
            "initials/tokens",
            &[
                "vsc",
                "ffx",
                "nm",
                "code visual",
                "chrome google",
                "file man",
            ],
        ),
        (
            "typos",
            &[
                "chrmoe",
                "chrxome",
                "chome",
                "chrm",
                "caluclat",
                "calxulatxr",
                "chrmoe google",
            ],
        ),
        (
            "unmatched/literal",
            &[
                "zzzzzz",
                "qzxw",
                "!",
                "^chrome",
                "absent token",
                "xxxxxxxxxxxx",
            ],
        ),
    ];
    let mut index = SearchIndex::new(names);
    for (group, queries) in groups {
        let old = measure(
            || {
                for q in *queries {
                    black_box(baseline(black_box(names), black_box(q)));
                }
            },
            batches,
            repeats,
        );
        let new = measure(
            || {
                for q in *queries {
                    black_box(index.filter(black_box(q)));
                }
            },
            batches,
            repeats,
        );
        let count = queries.len() as f64;
        println!(
            "  {group:18} baseline {:9.1}/{:9.1} us | indexed {:9.1}/{:9.1} us (p50/p95 per key)",
            old.0 / count,
            old.1 / count,
            new.0 / count,
            new.1 / count
        );
    }
    println!("  Individual-key latency (101 samples, no batch averaging):");
    for query in ["term", "chrmoe", "zzzzzz", "vekstpo", "fireofv x"] {
        let old = measure(
            || {
                black_box(baseline(black_box(names), black_box(query)));
            },
            101,
            1,
        );
        let new = measure(
            || {
                black_box(index.filter(black_box(query)));
            },
            101,
            1,
        );
        println!(
            "    {query:8} baseline {:9.1}/{:9.1} us | indexed {:9.1}/{:9.1} us (p50/p95)",
            old.0, old.1, new.0, new.1
        );
    }
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("JOFI_SEARCH_NAMES").ok())
        .unwrap_or_else(|| {
            format!(
                "{}/.jcode/scratch/jofi-search-apps.tsv",
                std::env::var("HOME").unwrap_or_default()
            )
        });
    let batches = std::env::var("JOFI_SEARCH_BATCHES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(31usize)
        .max(2);
    let repeats = std::env::var("JOFI_SEARCH_REPEATS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3usize)
        .max(1);
    println!("Search benchmark: {batches} repeated batches x {repeats}; build separate; full sorted results + highlights.\nRankings/recall intentionally differ; this is workload timing, not semantic equivalence.");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let names: Vec<String> = text.lines().filter_map(|l| l.split('\t').next()).filter(|n| !n.is_empty()).map(str::to_owned).collect();
            if names.is_empty() { panic!("catalog is empty: {path}"); }
            println!("Catalog: {path}");
            run("actual app catalog", &names, batches, repeats);
        }
        Err(e) => eprintln!("Actual catalog unavailable ({path}: {e}); synthetic results are NOT a substitute for actual catalog evidence."),
    }
    run("synthetic 1k", &synthetic(1_000), batches, repeats);
    run("synthetic 10k", &synthetic(10_000), batches, repeats);
}

fn fuzzy(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
    subsequence(name, query).or_else(|| typo_match(name, query))
}

/// Case-insensitive Unicode subsequence match. Positions are character indices, never byte offsets.
fn subsequence(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
    let hay: Vec<char> = name.chars().collect();
    let needle: Vec<char> = query.chars().collect();
    let mut positions = Vec::new();
    let mut next = 0;
    let mut score = 0;
    for wanted in needle {
        let found = (next..hay.len()).find(|&i| hay[i].to_lowercase().eq(wanted.to_lowercase()))?;
        score += (found - next) as i64 * 4;
        if positions.is_empty() {
            score += found as i64 * 3;
        }
        if found == 0 || !hay[found - 1].is_alphanumeric() {
            score -= 5;
        }
        positions.push(found);
        next = found + 1;
    }
    Some((positions, score))
}

/// A conservative word-level edit fallback, including adjacent transpositions.
/// Short queries remain subsequence-only to avoid flooding the result list.
fn typo_match(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
    let needle: Vec<char> = query
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    if needle.len() < 4 {
        return None;
    }
    let limit = if needle.len() >= 8 { 2 } else { 1 };
    let chars: Vec<char> = name.chars().collect();
    let mut ranges = vec![(0, chars.len())];
    let mut start = 0;
    for end in 0..=chars.len() {
        if end == chars.len() || !chars[end].is_alphanumeric() {
            if start < end {
                ranges.push((start, end));
            }
            start = end + 1;
        }
    }
    ranges
        .into_iter()
        .filter_map(|(start, end)| {
            if (end - start).abs_diff(needle.len()) > limit {
                return None;
            }
            let word: Vec<char> = chars[start..end]
                .iter()
                .map(|c| c.to_lowercase().next().unwrap_or(*c))
                .collect();
            let distance = edit_distance(&word, &needle);
            if distance > limit {
                return None;
            }
            let positions = (start..end)
                .filter(|&i| needle.contains(&chars[i].to_lowercase().next().unwrap_or(chars[i])))
                .collect();
            Some((
                positions,
                i64::MAX / 2 + distance as i64 * 100 + start as i64,
            ))
        })
        .min_by_key(|entry| entry.1)
}

fn edit_distance(left: &[char], right: &[char]) -> usize {
    let mut previous_previous = Vec::new();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut row = vec![i + 1; right.len() + 1];
        for (j, b) in right.iter().enumerate() {
            row[j + 1] = (previous[j + 1] + 1)
                .min(row[j] + 1)
                .min(previous[j] + usize::from(a != b));
            if i > 0 && j > 0 && *a == right[j - 1] && left[i - 1] == *b {
                row[j + 1] = row[j + 1].min(previous_previous[j - 1] + 1);
            }
        }
        previous_previous = previous;
        previous = row;
    }
    previous[right.len()]
}
