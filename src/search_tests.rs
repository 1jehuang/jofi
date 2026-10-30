//! Independent behavioral corpus for the indexed search API.
use super::SearchIndex;

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
fn short_queries_do_not_trigger_typo_fallback() {
    for q in ["x", "xr", "xro", "chr"] {
        excludes("abc", q);
    }
    excludes("Chrome", "xro");
    includes("Chrome", "crm");
}
#[test]
fn fuzzy_always_precedes_typo_fallback() {
    let r = hits(&["Chrome", "a very long chrmoe subsequence"], "chrmoe");
    assert_eq!(r.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 0]);
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
