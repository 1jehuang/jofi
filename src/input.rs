//! Search-field select-all editing, independent of the Wayland UI.
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct Query {
    pub text: String,
    pub selected: bool,
    pub revision: u64,
}
impl Query {
    pub fn selection(&self) -> Option<&str> {
        (self.selected && !self.text.is_empty()).then_some(self.text.as_str())
    }
    pub fn select_all(&mut self) {
        self.selected = !self.text.is_empty();
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn deselect(&mut self) {
        if self.selected {
            self.selected = false;
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.selected = false;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        if self.selected {
            self.text.clear();
        }
        self.text.push_str(&text);
        self.selected = false;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn backspace(&mut self) {
        if self.selected {
            self.clear();
        } else if let Some((index, _)) = self.text.grapheme_indices(true).next_back() {
            self.text.truncate(index);
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn delete_word(&mut self) {
        if self.selected {
            self.clear();
            return;
        }
        let trimmed = self.text.trim_end();
        let end = trimmed
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        self.text.truncate(end);
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn cut_if_current(&mut self, revision: u64) -> bool {
        if self.revision == revision && self.selection().is_some() {
            self.clear();
            true
        } else {
            false
        }
    }
    pub fn paste_if_current(&mut self, revision: u64, text: &str) -> bool {
        if self.revision == revision {
            self.insert(text);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_replaces_text_and_backspace_deletes_all() {
        let mut query = Query::default();
        query.insert("Firefox");
        query.select_all();
        assert_eq!(query.selection(), Some("Firefox"));
        query.insert("Chrome");
        assert_eq!(query.text, "Chrome");
        assert!(query.selection().is_none());
        query.select_all();
        query.backspace();
        assert_eq!(query.text, "");
        query.select_all();
        assert!(query.selection().is_none());
    }
    #[test]
    fn asynchronous_clipboard_cannot_delete_or_replace_newer_edits() {
        let mut query = Query::default();
        query.insert("old");
        query.select_all();
        let revision = query.revision;
        query.insert("new");
        assert!(!query.cut_if_current(revision));
        assert!(!query.paste_if_current(revision, "stale"));
        assert_eq!(query.text, "new");
        query.select_all();
        assert!(query.cut_if_current(query.revision));
        assert_eq!(query.text, "");
        assert!(query.paste_if_current(query.revision, "café\n\0世界"));
        assert_eq!(query.text, "café世界");
    }
    #[test]
    fn backspace_deletes_whole_graphemes_and_deselect_preserves_query() {
        let mut query = Query::default();
        query.insert("a👨‍👩‍👧‍👦e\u{301}");
        query.backspace();
        assert_eq!(query.text, "a👨‍👩‍👧‍👦");
        query.backspace();
        assert_eq!(query.text, "a");
        query.select_all();
        query.deselect();
        assert!(query.selection().is_none());
        query.insert("b");
        assert_eq!(query.text, "ab");
    }
    #[test]
    fn word_deletion_handles_trailing_spaces_unicode_and_selection() {
        let mut query = Query::default();
        query.insert("visual café 世界   ");
        query.delete_word();
        assert_eq!(query.text, "visual café ");
        query.delete_word();
        assert_eq!(query.text, "visual ");
        query.select_all();
        query.delete_word();
        assert!(query.text.is_empty());
        query.delete_word();
        assert!(query.text.is_empty());
    }
}
