//! Indexed launcher search: optimal fuzzy alignment plus bounded word-prefix typos.
//!
//! Nucleo supplies fzf-style boundary/consecutive/camel-case scoring. Its low-level
//! API is deliberately used instead of the query language: punctuation is literal.
//! Candidate segmentation is cached, queries are prepared once per keystroke, and
//! the matcher and three banded Damerau-Levenshtein rows are reused. No threads,
//! app launching, catalog cache, or background service are involved.
use nucleo_matcher::{chars, Config, Matcher, Utf32Str};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub type SearchMatch = (usize, Vec<usize>, i64);

const EXACT_BONUS: i64 = 1 << 42;
const PREFIX_BONUS: i64 = 1 << 41;
const TYPO_PENALTY: i64 = 1 << 50;

#[derive(Debug)]
enum Text {
    Ascii(Vec<u8>),
    Unicode(Vec<char>),
}
impl Text {
    fn as_utf32(&self) -> Utf32Str<'_> {
        match self {
            Self::Ascii(bytes) => Utf32Str::Ascii(bytes),
            Self::Unicode(chars) => Utf32Str::Unicode(chars),
        }
    }
}

#[derive(Debug)]
struct Prepared {
    text: Text,
    folded: Vec<char>,
    // Matcher offsets index the prepared text. Rendering uses original Rust
    // char indices, including any combining accents collapsed during matching.
    offsets: Vec<Range<usize>>,
    words: Vec<Range<usize>>,
}
impl Prepared {
    fn new(value: &str) -> Self {
        let (text, folded, offsets) = if value.is_ascii() {
            (
                Text::Ascii(value.as_bytes().to_vec()),
                value
                    .bytes()
                    .map(|b| b.to_ascii_lowercase() as char)
                    .collect(),
                Vec::new(),
            )
        } else {
            let mut original = Vec::new();
            let mut folded = Vec::new();
            let mut offsets = Vec::new();
            let mut offset = 0;
            for grapheme in value.graphemes(true) {
                let ch = grapheme.chars().next().unwrap();
                let normalized = chars::normalize(ch);
                let end = offset + grapheme.chars().count();
                // Collapse Latin combining accents for accent-insensitive
                // search, but retain complete emoji/Indic/ZWJ sequences. Taking
                // only the first codepoint would confuse 👩‍💻 with 👩‍🔬.
                let latin_accents = normalized.is_ascii_alphabetic()
                    && grapheme.chars().skip(1).all(|mark| {
                        matches!(mark as u32,
                        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff
                        | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
                    });
                if latin_accents {
                    original.push(ch);
                    folded.push(chars::to_lower_case(normalized));
                    offsets.push(offset..end);
                } else {
                    for (i, point) in grapheme.chars().enumerate() {
                        original.push(point);
                        folded.push(chars::to_lower_case(chars::normalize(point)));
                        offsets.push(offset + i..offset + i + 1);
                    }
                }
                offset = end;
            }
            (Text::Unicode(original), folded, offsets)
        };
        let original = text.as_utf32();
        let mut words = Vec::new();
        let mut start = 0;
        while start < folded.len() {
            if !folded[start].is_alphanumeric() {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < folded.len() && folded[end].is_alphanumeric() {
                end += 1;
            }
            words.push(start..end);
            // Include both the complete word and camel/acronym components:
            // VSCode -> VS, Code, VSCode. This helps typo recovery as well as
            // preserving whole product names such as LibreOffice.
            let mut part = start;
            for i in start + 1..end {
                let prev = original.get((i - 1) as u32);
                let here = original.get(i as u32);
                let next_lower = i + 1 < end && original.get((i + 1) as u32).is_lowercase();
                if (prev.is_lowercase() && here.is_uppercase())
                    || (prev.is_uppercase() && here.is_uppercase() && next_lower)
                {
                    words.push(part..i);
                    part = i;
                }
            }
            if part != start {
                words.push(part..end);
            }
            start = end;
        }
        Self {
            text,
            folded,
            offsets,
            words,
        }
    }
    fn query(value: &str) -> Self {
        let mut prepared = Self::new(value);
        // The low-level matcher treats Ascii and Unicode as correctness tags:
        // ASCII needles must use the ASCII variant, even after accent folding.
        prepared.text = if prepared.folded.iter().all(char::is_ascii) {
            Text::Ascii(prepared.folded.iter().map(|&ch| ch as u8).collect())
        } else {
            Text::Unicode(prepared.folded.clone())
        };
        prepared
    }
    fn append_positions(&self, positions: impl Iterator<Item = usize>, output: &mut Vec<usize>) {
        if self.offsets.is_empty() {
            output.extend(positions);
        } else {
            for position in positions {
                output.extend(self.offsets[position].clone());
            }
        }
    }
}

/// A launcher-local index, in the same order as the names supplied to `new`.
/// Original order is the final tie-breaker, preserving the caller's recency order.
pub struct SearchIndex {
    entries: Vec<Prepared>,
    // Empty-query startup never allocates Nucleo's matching matrix.
    matcher: Option<Matcher>,
    indices: Vec<u32>,
    edit: EditScratch,
}
impl SearchIndex {
    pub fn new(names: &[String]) -> Self {
        Self {
            entries: names.iter().map(|name| Prepared::new(name)).collect(),
            matcher: None,
            indices: Vec::new(),
            edit: EditScratch::default(),
        }
    }

    /// Return all matches, sorted best first. Positions index the original name's
    /// Unicode characters, not bytes or the normalized/segmented search text.
    /// Whitespace-separated terms are ANDed in any order. Other punctuation is
    /// literal, not an undocumented fzf operator language.
    pub fn filter(&mut self, query: &str) -> Vec<SearchMatch> {
        let terms: Vec<_> = query.split_whitespace().map(Prepared::query).collect();
        if terms.is_empty() {
            return (0..self.entries.len())
                .map(|i| (i, Vec::new(), 0))
                .collect();
        }
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        let matcher = self.matcher.get_or_insert_with(|| Matcher::new(config));
        let mut matches = Vec::with_capacity(self.entries.len());
        // Compare a single whole-name query without whitespace syntax tricks.
        let whole = Prepared::new(query.trim());
        for (index, entry) in self.entries.iter().enumerate() {
            let mut positions = Vec::new();
            let mut score = 0i64;
            let mut errors = 0;
            let mut found = true;
            for term in &terms {
                self.indices.clear();
                // Nucleo expects the needle to have already been case-folded
                // and normalized. Candidate case is retained for camel bonuses.
                let needle = term.text.as_utf32();
                if let Some(fuzzy_score) =
                    matcher.fuzzy_indices(entry.text.as_utf32(), needle, &mut self.indices)
                {
                    score -= i64::from(fuzzy_score) * 1024;
                    entry
                        .append_positions(self.indices.iter().map(|&p| p as usize), &mut positions);
                } else if let Some(hit) = typo_prefix(entry, &term.folded, &mut self.edit) {
                    errors += hit.distance;
                    score += hit.completion as i64 * 1024 + hit.start as i64;
                    entry.append_positions(hit.start..hit.end, &mut positions);
                } else {
                    found = false;
                    break;
                }
            }
            if !found {
                continue;
            }
            if errors != 0 {
                // Never let speculative typo recovery displace a literal fuzzy
                // match. Prefer fewer edits, then stronger matching other terms.
                score += TYPO_PENALTY + errors as i64 * (1 << 30);
            } else if entry.folded == whole.folded {
                score -= EXACT_BONUS;
            } else if entry.folded.starts_with(&whole.folded) {
                score -= PREFIX_BONUS;
            }
            score += entry.folded.len().min(1023) as i64;
            positions.sort_unstable();
            positions.dedup();
            matches.push((index, positions, score));
        }
        matches.sort_unstable_by_key(|entry| (entry.2, entry.0));
        matches
    }
}

#[derive(Debug, Clone, Copy)]
struct TypoHit {
    start: usize,
    end: usize,
    distance: usize,
    completion: usize,
}
fn typo_prefix(entry: &Prepared, query: &[char], scratch: &mut EditScratch) -> Option<TypoHit> {
    // One-letter abbreviation queries must stay precise. Long terms tolerate
    // two independent mistakes, but there is never unbounded fuzzy edit search.
    let limit = match query.len() {
        0..=3 => return None,
        4..=7 => 1,
        _ => 2,
    };
    // Never repair punctuation: C++, paths, brackets, and similar names remain
    // literal queries rather than degenerating into unrelated alphanumeric hits.
    if !query.iter().all(|ch| ch.is_alphanumeric()) {
        return None;
    }
    let mut best = None;
    for word in &entry.words {
        let target = &entry.folded[word.clone()];
        if query.len() > target.len().saturating_add(limit) {
            continue;
        }
        if let Some((distance, consumed)) = scratch.prefix_distance(query, target, limit) {
            let hit = TypoHit {
                start: word.start,
                end: word.start + consumed,
                distance,
                completion: target.len() - consumed,
            };
            let key = |h: TypoHit| (h.distance, h.completion, h.start);
            if best.is_none_or(|previous| key(hit) < key(previous)) {
                best = Some(hit);
            }
        }
    }
    best
}

/// Three recycled rows, a width-2k+1 diagonal band, and an early exit once no
/// path can meet the edit budget. Adjacent transpositions cost one edit (OSA).
#[derive(Default)]
struct EditScratch {
    rows: [Vec<usize>; 3],
}
impl EditScratch {
    fn prefix_distance(
        &mut self,
        query: &[char],
        target: &[char],
        limit: usize,
    ) -> Option<(usize, usize)> {
        let columns = target.len().min(query.len().saturating_add(limit));
        if columns + limit < query.len() {
            return None;
        }
        let outside = limit + 1;
        for row in &mut self.rows {
            row.resize(columns + 1, outside);
            row.fill(outside);
        }
        for (j, cell) in self.rows[0]
            .iter_mut()
            .enumerate()
            .take(columns.min(limit) + 1)
        {
            *cell = j;
        }
        for i in 1..=query.len() {
            let row = i % 3;
            let prev = (i + 2) % 3;
            let prev_prev = (i + 1) % 3;
            self.rows[row][0] = if i <= limit { i } else { outside };
            let start = i.saturating_sub(limit).max(1);
            let end = columns.min(i.saturating_add(limit));
            // Only reset the band's guards, not the entire row. Filling every
            // column per row would silently make long queries quadratic again.
            if start > 1 {
                self.rows[row][start - 1] = outside;
            }
            if end < columns {
                self.rows[row][end + 1] = outside;
            }
            let mut minimum = self.rows[row][0];
            for j in start..=end {
                let mut distance = (self.rows[prev][j] + 1)
                    .min(self.rows[row][j - 1] + 1)
                    .min(self.rows[prev][j - 1] + usize::from(query[i - 1] != target[j - 1]));
                if i > 1 && j > 1 && query[i - 1] == target[j - 2] && query[i - 2] == target[j - 1]
                {
                    distance = distance.min(self.rows[prev_prev][j - 2] + 1);
                }
                self.rows[row][j] = distance.min(outside);
                minimum = minimum.min(distance);
            }
            if minimum > limit {
                return None;
            }
        }
        let final_row = &self.rows[query.len() % 3];
        (query.len().saturating_sub(limit)..=columns)
            .filter_map(|j| (final_row[j] <= limit).then_some((final_row[j], j)))
            // With the same edit count prefer the longest corrected prefix.
            .min_by_key(|&(distance, consumed)| (distance, std::cmp::Reverse(consumed)))
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
