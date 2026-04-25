use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::telemetry::state_home;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    counts: HashMap<String, u32>,
}

impl History {
    pub fn load_with_tofi_fallback() -> Result<Self> {
        let jofi_path = jofi_history_path();
        if jofi_path.is_file() {
            return Self::load_from_path(&jofi_path);
        }

        let tofi_path = tofi_history_path();
        if tofi_path.is_file() {
            let history = Self::load_from_path(&tofi_path)?;
            history.save_to_path(&jofi_path)?;
            return Ok(history);
        }

        Ok(Self::default())
    }

    pub fn load_from_path(path: &PathBuf) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read history {}", path.display()))?;
        Ok(Self {
            counts: parse_history(&text),
        })
    }

    pub fn save(&self) -> Result<()> {
        self.save_to_path(&jofi_history_path())
    }

    pub fn save_to_path(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create history directory {}", parent.display())
            })?;
        }
        let mut entries = self.counts.iter().collect::<Vec<_>>();
        entries.sort_by(|(name_a, count_a), (name_b, count_b)| {
            count_b.cmp(count_a).then_with(|| name_a.cmp(name_b))
        });
        let mut out = String::new();
        for (name, count) in entries {
            out.push_str(&format!("{count} {name}\n"));
        }
        fs::write(path, out).with_context(|| format!("failed to write history {}", path.display()))
    }

    pub fn count_for(&self, name: &str) -> u32 {
        self.counts.get(name).copied().unwrap_or(0)
    }

    pub fn increment(&mut self, name: &str) {
        let count = self.counts.entry(name.to_string()).or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

pub fn jofi_history_path() -> PathBuf {
    state_home().join("jofi").join("history")
}

pub fn tofi_history_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("tofi-drun-history")
}

fn parse_history(text: &str) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((count, name)) = line.split_once(' ') else {
            continue;
        };
        let Ok(count) = count.parse::<u32>() else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() {
            counts.insert(name.to_string(), count);
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tofi_history_lines() {
        let counts = parse_history("881 Google Chrome\n62 Play Song\nnot valid\n");
        assert_eq!(counts.get("Google Chrome"), Some(&881));
        assert_eq!(counts.get("Play Song"), Some(&62));
        assert_eq!(counts.len(), 2);
    }

    #[test]
    fn increments_saturating_count() {
        let mut history = History::default();
        history.increment("Firefox");
        history.increment("Firefox");
        assert_eq!(history.count_for("Firefox"), 2);
    }
}
