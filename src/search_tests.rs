//! Independent behavioral corpus for the indexed search API.
// A harness=false benchmark includes search.rs with cfg(test) but does not run
// unit tests. Keep those helper-only dead-code warnings out of strict bench lint.
#![allow(dead_code)]
use super::SearchIndex;

// Deliberately unbanded, allocation-heavy full matrix reference. This oracle
// shares no scratch rows, pruning, or recurrence storage with the implementation.
fn reference_prefix(query: &[char], target: &[char], limit: usize) -> Option<(usize, usize)> {
    let mut matrix = vec![vec![0usize; target.len() + 1]; query.len() + 1];
    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in matrix[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=query.len() {
        for j in 1..=target.len() {
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + usize::from(query[i - 1] != target[j - 1]));
            if i > 1 && j > 1 && query[i - 1] == target[j - 2] && query[i - 2] == target[j - 1] {
                matrix[i][j] = matrix[i][j].min(matrix[i - 2][j - 2] + 1);
            }
        }
    }
    (0..=target.len())
        .filter_map(|j| {
            let distance = matrix[query.len()][j];
            (distance <= limit).then_some((distance, j))
        })
        .min_by_key(|&(distance, consumed)| (distance, std::cmp::Reverse(consumed)))
}

#[test]
fn exhaustive_banded_prefix_matches_full_matrix_and_scratch_reuse() {
    let mut words = Vec::new();
    for len in 0..=6 {
        for bits in 0..(1usize << len) {
            words.push(
                (0..len)
                    .map(|i| if bits & (1 << i) == 0 { 'a' } else { 'b' })
                    .collect::<Vec<_>>(),
            );
        }
    }
    let mut scratch = super::EditScratch::default();
    for query in &words {
        // Reverse targets and edit budgets repeatedly grow/shrink the recycled
        // rows and follow early-exit failures with successful transpositions.
        for target in words.iter().rev() {
            for budget in [2, 0, 1] {
                assert_eq!(
                    scratch.prefix_distance(query, target, budget),
                    reference_prefix(query, target, budget),
                    "query={query:?} target={target:?} budget={budget}"
                );
            }
        }
    }
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}
fn hits(items: &[&str], query: &str) -> Vec<(usize, Vec<usize>, i64)> {
    let original = names(items);
    let mut index = SearchIndex::new(&original);
    let result = index.filter(query);
    for (id, highlights, _) in &result {
        assert!(*id < original.len(), "invalid original index for {query:?}");
        assert!(
            highlights
                .iter()
                .all(|&p| p < original[*id].chars().count()),
            "non-character highlight for {query:?}: {highlights:?}"
        );
        assert!(
            highlights.windows(2).all(|p| p[0] < p[1]),
            "highlights must be sorted and unique: {highlights:?}"
        );
    }
    assert!(
        result
            .windows(2)
            .all(|r| (r[0].2, r[0].0) <= (r[1].2, r[1].0)),
        "score/index order violated for {query:?}"
    );
    result
}
fn includes(name: &str, query: &str) {
    assert!(
        !hits(&[name], query).is_empty(),
        "{query:?} should find {name:?}"
    );
}
fn excludes(name: &str, query: &str) {
    assert!(
        hits(&[name], query).is_empty(),
        "{query:?} must not find {name:?}"
    );
}

#[test]
fn exact_and_prefix_outrank_scattered() {
    let r = hits(
        &["The Error Recovery Manager", "Terminal Emulator", "term"],
        "term",
    );
    assert_eq!(r[0].0, 2);
    assert!(r.iter().position(|r| r.0 == 1) < r.iter().position(|r| r.0 == 0));
}
#[test]
fn initials_and_camel_case() {
    for (name, query) in [
        ("Visual Studio Code", "vsc"),
        ("GitHubDesktop", "ghd"),
        ("Firefox", "ffx"),
        ("NetworkManager", "nm"),
    ] {
        includes(name, query);
    }
}
#[test]
fn tokens_are_anded_in_any_order() {
    includes("Visual Studio Code", "code visual");
    includes("Google Chrome", "chrmoe google");
    excludes("Visual Studio Code", "code absent");
    excludes("Google Chrome", "google zzzzz");
}
#[test]
fn punctuation_is_literal_not_query_language() {
    for q in ["!", "^", "$", "'", "|", "(", ")", "[", "]", "*", "?"] {
        excludes("Firefox Terminal", q);
        includes(&format!("Tool{q}Name"), q);
    }
    includes("C++ Editor", "c++");
    excludes("C Editor", "c++");
}
#[test]
fn unicode_case_accents_and_original_highlights() {
    assert_eq!(hits(&["🦀 Éclair"], "écl")[0].1, vec![2, 3, 4]);
    includes("ÉCLAIR", "éclair");
    includes("Café", "cafe");
    includes("Cafe\u{301} Tools", "café");
    let r = hits(&["🦀 Cafe\u{301} Tools"], "cafe");
    assert!(r[0].1.contains(&3));
    assert!(r[0].1.iter().all(|&i| (2..=6).contains(&i)));
}

#[test]
fn complex_emoji_and_non_latin_names_remain_distinct() {
    includes("👩‍💻 Developer Tools", "👩‍💻");
    excludes("👩‍🔬 Science Tools", "👩‍💻");
    includes("ЦИФРЫ", "циф");
    includes("日本語入力", "日入");
    let r = hits(&["🦀 👩‍💻 Developer Tools"], "👩‍💻");
    assert_eq!(r[0].1, vec![2, 3, 4]);
}
#[test]
fn conservative_typo_edit_budget_and_prefixes() {
    for q in ["chrxome", "chome", "chrume", "chrmoe", "chroem"] {
        includes("Google Chrome", q);
    }
    includes("Chromium Browser", "chrx");
    includes("Calculator", "caluclat");
    includes("Calculator", "calxulatxr");
    excludes("Chrome", "cxrxme");
    excludes("Chrome", "zzzzzz");
}

#[test]
fn screenshot_typos_rank_first_in_realistic_catalog() {
    let catalog = [
        "Chromium Browser",
        "Discord",
        "Desktop Settings",
        "VLC media player",
        "Visual Studio Code",
        "File Roller",
        "Firewall Configuration",
        "FileZilla",
        "Firefox Profile Manager",
        "Vesktop",
        "Firefox",
        "Xfce Terminal",
    ];
    for (query, expected) in [
        ("vekstpo", 9),
        ("fireofv", 10),
        ("fireofv x", 10),
        ("x fireofv", 10),
    ] {
        let result = hits(&catalog, query);
        assert_eq!(
            result.first().map(|hit| hit.0),
            Some(expected),
            "{query:?}: {result:?}"
        );
        assert_eq!(result[0].1, (0..7).collect::<Vec<_>>());
    }
    // AND terms are independent: the x is already inside the corrected word.
    excludes("Firefox", "fireofv z");
    excludes("Vesktop", "vekstpo x");
}

#[test]
fn two_edit_threshold_is_seven_and_remains_bounded() {
    // Direct typo checks avoid confusing intentional literal subsequences with
    // typo eligibility. Verify both the edit count and the threshold boundary.
    for (target, query, expected) in [
        ("abc", "abx", None),
        ("abcd", "abcx", Some(1)),
        ("abcde", "abcxx", None),
        ("abcdef", "abcxex", None),
        ("abcdefg", "abcxexg", Some(2)),
        ("abcdefgh", "abcxexgh", Some(2)),
        ("abcdefg", "axcxexg", None),
        ("Vesktop", "vekstpo", Some(2)),
        ("Firefox", "fireofv", Some(2)),
        ("Chrome", "cxrxme", None),
        ("Firefox", "zzzzzzz", None),
        ("Firefox", "fireo++", None),
    ] {
        let entry = super::Prepared::new(target);
        let query_chars = super::Prepared::query(query).folded;
        let hit = super::typo_prefix(&entry, &query_chars, &mut super::EditScratch::default());
        assert_eq!(
            hit.map(|hit| hit.distance),
            expected,
            "{query:?} -> {target:?}"
        );
    }
    for query in ["zzzzzzz", "axcxexg", "fireo++"] {
        excludes("Firefox Vesktop abcdefg", query);
    }
}

#[test]
fn seven_character_recovery_preserves_dense_and_boundary_quality_tiers() {
    // Strong dense and boundary subsequences remain above a two-edit hit.
    for literal in [
        "vekstpo",
        "vekstpox",
        "Very Excellent Kind Spacious Tall Product Organizer",
        "VeryExcellentKindSpaciousTallProductOrganizer",
    ] {
        let result = hits(&["Vesktop", literal], "vekstpo");
        assert_eq!(result[0].0, 1, "{literal:?}: {result:?}");
    }
    // Conversely, an incidental long cross-word scatter stays below recovery.
    let result = hits(&["Overtake stock photo organizer", "Vesktop"], "vekstpo");
    assert_eq!(result[0].0, 1, "{result:?}");
}
#[test]
fn short_queries_do_not_trigger_typo_fallback() {
    for q in ["x", "xr", "xro", "chr"] {
        excludes("abc", q);
    }
    excludes("Chrome", "xro");
    includes("Chrome", "crm");
}
#[test]
fn strong_literal_fuzzy_precedes_typo_fallback() {
    let r = hits(&["Chrome", "a very long chrmoe subsequence"], "chrmoe");
    assert_eq!(r.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 0]);
}

#[test]
fn chrome_typo_beats_weak_scatter_in_chromium_catalog() {
    let candidates = ["Chromium Browser", "Chromium Profile", "Google Chrome"];
    let r = hits(&candidates, "chrmoe");
    assert_eq!(
        r[0].0, 2,
        "real catalog typo should choose Google Chrome: {r:?}"
    );
    let with_chrome = hits(
        &[
            "Chrome",
            "Chromium Browser",
            "Chromium Profile",
            "Google Chrome",
        ],
        "chrmoe",
    );
    let position = |id| with_chrome.iter().position(|r| r.0 == id).unwrap();
    for corrected in [0, 3] {
        for scattered in [1, 2] {
            assert!(
                position(corrected) < position(scattered),
                "bounded correction should outrank weak scatter: {with_chrome:?}"
            );
        }
    }
}

#[test]
fn long_boundary_and_camel_acronyms_remain_strong_fuzzy() {
    // The literal acronym spans far more than twice the query length, but each
    // skipped segment ends at a word or camel-case boundary. A compact typo
    // candidate must not suppress those intentional abbreviation matches.
    for name in ["Very Spacious Coding System", "VerySpaciousCodingSystem"] {
        let r = hits(&["vscx", name], "vscs");
        assert_eq!(
            r[0].0, 1,
            "boundary acronym must outrank typo for {name}: {r:?}"
        );
    }
}
#[test]
fn repeated_tokens_have_safe_deduplicated_highlights() {
    includes("Code Code Editor", "code code");
    let r = hits(&["Code Code Editor"], "code code");
    assert!(!r[0].1.is_empty());
}
#[test]
fn original_recent_history_order_breaks_ties() {
    let original = names(&["Terminal", "Terminal", "Terminal"]);
    let mut index = SearchIndex::new(&original);
    for q in ["", "term", "trm", "terimnal", "", "term"] {
        let r = index.filter(q);
        assert_eq!(
            r.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "history order for {q:?}"
        );
        assert!(r.windows(2).all(|r| r[0].2 == r[1].2));
    }
}
#[test]
fn empty_catalog_and_query_are_safe() {
    assert!(hits(&[], "anything").is_empty());
    assert_eq!(
        hits(&["One", "Two"], "")
            .iter()
            .map(|r| r.0)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}
