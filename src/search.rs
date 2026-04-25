use crate::desktop::DesktopEntry;
use crate::history::History;
use serde::Serialize;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub score: i32,
    pub reason: String,
    pub entry: DesktopEntry,
}

#[derive(Debug, Clone)]
pub struct SearchIndex {
    docs: Vec<SearchDoc>,
}

#[derive(Debug, Clone)]
struct SearchDoc {
    entry: DesktopEntry,
    name_norm: String,
    id_norm: String,
    acronym: String,
    weighted_fields: Vec<WeightedField>,
    history_count: u32,
}

#[derive(Debug, Clone)]
struct WeightedField {
    text: String,
    weight: i32,
    label: &'static str,
}

impl SearchIndex {
    pub fn new(entries: Vec<DesktopEntry>) -> Self {
        Self::with_history(entries, &History::default())
    }

    pub fn with_history(entries: Vec<DesktopEntry>, history: &History) -> Self {
        let mut docs = entries
            .into_iter()
            .map(|entry| SearchDoc::new(entry, history))
            .collect::<Vec<_>>();
        docs.sort_by(sort_docs_for_empty_query);
        Self { docs }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query_norm = normalize(query);
        if query_norm.is_empty() {
            return self
                .docs
                .iter()
                .take(limit)
                .map(|doc| SearchResult {
                    score: doc.history_score(),
                    reason: if doc.history_count > 0 {
                        format!("history:{}", doc.history_count)
                    } else {
                        "empty-query".to_string()
                    },
                    entry: doc.entry.clone(),
                })
                .collect();
        }

        let query_tokens = tokenize(&query_norm);
        let mut results = self
            .docs
            .iter()
            .filter_map(|doc| score_doc(doc, &query_norm, &query_tokens))
            .collect::<Vec<_>>();

        results.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.entry.source_rank.cmp(&b.entry.source_rank))
                .then_with(|| {
                    a.entry
                        .name
                        .to_lowercase()
                        .cmp(&b.entry.name.to_lowercase())
                })
        });
        results.truncate(limit);
        results
    }

    pub fn entries(&self) -> impl Iterator<Item = &DesktopEntry> {
        self.docs.iter().map(|doc| &doc.entry)
    }
}

impl SearchDoc {
    fn new(entry: DesktopEntry, history: &History) -> Self {
        let history_count = history.count_for(&entry.name);
        let name_norm = normalize(&entry.name);
        let id_norm = normalize(&entry.id);
        let acronym = acronym(&name_norm);
        let mut weighted_fields = vec![
            WeightedField {
                text: name_norm.clone(),
                weight: 10,
                label: "name",
            },
            WeightedField {
                text: id_norm.clone(),
                weight: 4,
                label: "id",
            },
        ];

        if let Some(generic_name) = &entry.generic_name {
            weighted_fields.push(WeightedField {
                text: normalize(generic_name),
                weight: 6,
                label: "generic-name",
            });
        }
        if let Some(comment) = &entry.comment {
            weighted_fields.push(WeightedField {
                text: normalize(comment),
                weight: 2,
                label: "comment",
            });
        }
        for keyword in &entry.keywords {
            weighted_fields.push(WeightedField {
                text: normalize(keyword),
                weight: 8,
                label: "keyword",
            });
        }
        for category in &entry.categories {
            weighted_fields.push(WeightedField {
                text: normalize(category),
                weight: 2,
                label: "category",
            });
        }

        Self {
            entry,
            name_norm,
            id_norm,
            acronym,
            weighted_fields,
            history_count,
        }
    }

    fn history_score(&self) -> i32 {
        if self.history_count == 0 {
            0
        } else {
            20_000 + (self.history_count.min(10_000) as i32 * 10)
        }
    }
}

pub fn normalize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = true;

    for ch in input.nfkd().filter(|ch| !is_combining_mark(*ch)) {
        for lower in ch.to_lowercase() {
            if lower.is_alphanumeric() {
                out.push(lower);
                last_space = false;
            } else if !last_space {
                out.push(' ');
                last_space = true;
            }
        }
    }

    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn tokenize(normalized: &str) -> Vec<&str> {
    normalized.split_whitespace().collect()
}

fn acronym(normalized_name: &str) -> String {
    normalized_name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .collect()
}

fn sort_docs_for_empty_query(a: &SearchDoc, b: &SearchDoc) -> std::cmp::Ordering {
    b.history_count
        .cmp(&a.history_count)
        .then_with(|| a.entry.source_rank.cmp(&b.entry.source_rank))
        .then_with(|| {
            a.entry
                .name
                .to_lowercase()
                .cmp(&b.entry.name.to_lowercase())
        })
}

fn score_doc(doc: &SearchDoc, query_norm: &str, query_tokens: &[&str]) -> Option<SearchResult> {
    let mut score = 0;
    let mut reasons = Vec::new();

    if doc.name_norm == query_norm {
        score += 120_000;
        reasons.push("exact-name".to_string());
    } else if doc.name_norm.starts_with(query_norm) {
        score += 100_000 - doc.name_norm.len() as i32;
        reasons.push("prefix-name".to_string());
    } else if doc.id_norm.starts_with(query_norm) {
        score += 70_000 - doc.id_norm.len() as i32;
        reasons.push("prefix-id".to_string());
    } else if doc.acronym.starts_with(query_norm) {
        score += 85_000 - doc.acronym.len() as i32;
        reasons.push("acronym".to_string());
    }

    for token in query_tokens {
        let mut best: Option<(i32, &'static str)> = None;
        for field in &doc.weighted_fields {
            if field.text.is_empty() {
                continue;
            }
            if let Some(raw_score) = score_token(token, &field.text) {
                let weighted = raw_score * field.weight;
                if best.is_none_or(|(best_score, _)| weighted > best_score) {
                    best = Some((weighted, field.label));
                }
            }
        }
        let (token_score, label) = best?;
        score += token_score;
        reasons.push(format!("{token}:{label}"));
    }

    // Personal entries in ~/.local/share/applications are discovered first and get
    // a small boost. This makes user scripts feel first-class without special casing.
    score += (10_i32 - doc.entry.source_rank as i32).max(0) * 25;

    if doc.history_count > 0 {
        score += doc.history_score();
        reasons.push(format!("history:{}", doc.history_count));
    }

    Some(SearchResult {
        score,
        reason: reasons.join(","),
        entry: doc.entry.clone(),
    })
}

fn score_token(token: &str, field: &str) -> Option<i32> {
    if field == token {
        return Some(10_000);
    }
    if field.starts_with(token) {
        return Some(9_000 - field.len() as i32);
    }
    if let Some(pos) = field.find(token) {
        return Some(7_500 - pos as i32 * 10);
    }

    let words = tokenize(field);
    let mut best = None;
    for word in words {
        let max_distance = typo_budget(token);
        if let Some(distance) = bounded_damerau_levenshtein(token, word, max_distance) {
            let candidate = 6_500 - (distance as i32 * 1_000) - word.len() as i32;
            best = Some(best.map_or(candidate, |current: i32| current.max(candidate)));
        }
    }
    if best.is_some() {
        return best;
    }

    subsequence_score(token, field)
}

fn typo_budget(token: &str) -> usize {
    match token.chars().count() {
        0 | 1 => 0,
        2..=4 => 1,
        _ => 2,
    }
}

fn subsequence_score(token: &str, field: &str) -> Option<i32> {
    let mut pos = 0;
    let mut gaps = 0;
    for ch in token.chars() {
        let rest = &field[pos..];
        let found = rest.find(ch)?;
        gaps += found;
        pos += found + ch.len_utf8();
    }
    Some(4_500 - gaps as i32 * 20 - field.len() as i32)
}

/// Optimal-string-alignment Damerau-Levenshtein with an early cutoff.
pub fn bounded_damerau_levenshtein(a: &str, b: &str, max_distance: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = a.len();
    let m = b.len();

    if n.abs_diff(m) > max_distance {
        return None;
    }
    if n == 0 {
        return (m <= max_distance).then_some(m);
    }
    if m == 0 {
        return (n <= max_distance).then_some(n);
    }

    let mut prev_prev = vec![usize::MAX / 4; m + 1];
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];

    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut value = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                value = value.min(prev_prev[j - 2] + 1);
            }
            curr[j] = value;
            row_min = row_min.min(value);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut prev_prev, &mut prev);
        std::mem::swap(&mut prev, &mut curr);
    }

    (prev[m] <= max_distance).then_some(prev[m])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::DesktopEntry;
    use std::path::PathBuf;

    fn entry(name: &str, keywords: &[&str]) -> DesktopEntry {
        DesktopEntry {
            id: format!("{}.desktop", name.to_lowercase().replace(' ', "-")),
            path: PathBuf::from("/tmp/fake.desktop"),
            name: name.to_string(),
            generic_name: None,
            comment: None,
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            exec: "/bin/true".to_string(),
            icon: None,
            terminal: false,
            categories: Vec::new(),
            source_rank: 0,
        }
    }

    #[test]
    fn normalizes_case_accents_and_punctuation() {
        assert_eq!(normalize("FiRé-Fox!!"), "fire fox");
    }

    #[test]
    fn handles_transposition_typo() {
        let index = SearchIndex::new(vec![entry("Chrome", &[])]);
        let results = index.search("chrmoe", 5);
        assert_eq!(results[0].entry.name, "Chrome");
    }

    #[test]
    fn ranks_exact_prefix_above_typo() {
        let index = SearchIndex::new(vec![entry("Firefox", &[]), entry("Fireplace", &[])]);
        let results = index.search("fire", 5);
        assert_eq!(results[0].entry.name, "Firefox");
    }

    #[test]
    fn matches_keywords_for_scripts() {
        let index = SearchIndex::new(vec![entry(
            "Disconnect iPhone Hotspot",
            &["wifi", "tether"],
        )]);
        let results = index.search("wifi", 5);
        assert_eq!(results[0].entry.name, "Disconnect iPhone Hotspot");
    }

    #[test]
    fn typo_checks_all_words_in_field() {
        let index = SearchIndex::new(vec![entry("Google Chrome", &[])]);
        let results = index.search("crome", 5);
        assert_eq!(results[0].entry.name, "Google Chrome");
        assert!(results[0].reason.contains("crome:name"));
    }

    #[test]
    fn empty_query_uses_history_order() {
        let mut history = History::default();
        history.increment("Play Song");
        history.increment("Play Song");
        history.increment("Firefox");
        let index = SearchIndex::with_history(
            vec![entry("Firefox", &[]), entry("Play Song", &[])],
            &history,
        );
        let results = index.search("", 5);
        assert_eq!(results[0].entry.name, "Play Song");
        assert_eq!(results[0].reason, "history:2");
    }

    #[test]
    fn bounded_damerau_supports_adjacent_swap() {
        assert_eq!(bounded_damerau_levenshtein("chrmoe", "chrome", 2), Some(1));
        assert_eq!(bounded_damerau_levenshtein("abcdef", "ghijkl", 2), None);
    }
}
