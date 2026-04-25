use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DesktopEntry {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
    pub exec: String,
    pub icon: Option<String>,
    pub terminal: bool,
    pub categories: Vec<String>,
    pub source_rank: u8,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoveryOptions {
    pub include_hidden: bool,
}

#[derive(Debug, Clone)]
struct RawDesktopEntry {
    path: PathBuf,
    fields: HashMap<String, String>,
}

pub fn discover_desktop_entries(options: &DiscoveryOptions) -> Result<Vec<DesktopEntry>> {
    let dirs = applications_dirs();
    discover_desktop_entries_in_dirs(&dirs, options)
}

pub fn discover_desktop_entries_in_dirs(
    application_dirs: &[PathBuf],
    options: &DiscoveryOptions,
) -> Result<Vec<DesktopEntry>> {
    let mut entries = Vec::new();
    let mut seen_ids = HashSet::new();
    let current_desktop = current_desktops();

    for (source_rank, applications_dir) in application_dirs.iter().enumerate() {
        let source_rank = source_rank.min(u8::MAX as usize) as u8;
        if !applications_dir.is_dir() {
            continue;
        }

        for path in desktop_files(applications_dir)? {
            let id = desktop_id(applications_dir, &path);
            if !seen_ids.insert(id.clone()) {
                continue;
            }

            let raw = parse_desktop_file(&path)
                .with_context(|| format!("failed to parse desktop file {}", path.display()))?;
            if let Some(entry) =
                DesktopEntry::from_raw(id, raw, source_rank, options, &current_desktop)
            {
                entries.push(entry);
            }
        }
    }

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(entries)
}

impl DesktopEntry {
    fn from_raw(
        id: String,
        raw: RawDesktopEntry,
        source_rank: u8,
        options: &DiscoveryOptions,
        current_desktop: &[String],
    ) -> Option<Self> {
        let ty = raw.fields.get("Type")?;
        if ty != "Application" {
            return None;
        }
        if raw.fields.get("Hidden").is_some_and(|v| parse_bool(v)) {
            return None;
        }
        if !options.include_hidden && raw.fields.get("NoDisplay").is_some_and(|v| parse_bool(v)) {
            return None;
        }
        if !show_in_current_desktop(&raw.fields, current_desktop) {
            return None;
        }

        let name = raw.fields.get("Name")?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let exec = raw.fields.get("Exec")?.trim().to_string();
        if exec.is_empty() {
            return None;
        }

        Some(Self {
            id,
            path: raw.path,
            name,
            generic_name: optional_field(&raw.fields, "GenericName"),
            comment: optional_field(&raw.fields, "Comment"),
            keywords: split_semicolon_field(raw.fields.get("Keywords")),
            exec,
            icon: optional_field(&raw.fields, "Icon"),
            terminal: raw.fields.get("Terminal").is_some_and(|v| parse_bool(v)),
            categories: split_semicolon_field(raw.fields.get("Categories")),
            source_rank,
        })
    }
}

pub fn applications_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }

    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|dirs| {
            std::env::split_paths(&dirs)
                .map(|path| path.join("applications"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ]
        });
    dirs.extend(data_dirs);
    dirs
}

fn desktop_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut children = fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.path());

        for child in children {
            let path = child.path();
            let file_type = child.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "desktop") {
                out.push(path);
            }
        }
    }

    out.sort();
    Ok(out)
}

fn desktop_id(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('/', "-")
}

fn parse_desktop_file(path: &Path) -> Result<RawDesktopEntry> {
    let text = fs::read_to_string(path)?;
    let mut in_desktop_entry = false;
    let mut fields = HashMap::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.contains('[') {
            // Locale-specific fields can be added later. The non-localized base value
            // keeps indexing deterministic and cheap for v0.
            continue;
        }
        fields.insert(key.to_string(), unescape_value(value.trim()));
    }

    Ok(RawDesktopEntry {
        path: path.to_path_buf(),
        fields,
    })
}

fn unescape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some(next) => out.push(next),
            None => out.push('\\'),
        }
    }
    out
}

fn optional_field(fields: &HashMap<String, String>, key: &str) -> Option<String> {
    fields
        .get(key)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn split_semicolon_field(value: Option<&String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(';')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_bool(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
}

fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn show_in_current_desktop(fields: &HashMap<String, String>, current_desktop: &[String]) -> bool {
    let not_show_in = split_semicolon_field(fields.get("NotShowIn"));
    if not_show_in
        .iter()
        .any(|desktop| desktop_matches(desktop, current_desktop))
    {
        return false;
    }

    let only_show_in = split_semicolon_field(fields.get("OnlyShowIn"));
    if only_show_in.is_empty() {
        return true;
    }
    only_show_in
        .iter()
        .any(|desktop| desktop_matches(desktop, current_desktop))
}

fn desktop_matches(desktop: &str, current_desktop: &[String]) -> bool {
    current_desktop
        .iter()
        .any(|current| current.eq_ignore_ascii_case(desktop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn discovers_basic_desktop_entry() {
        let dir = tempfile::tempdir().unwrap();
        let applications = dir.path().join("applications");
        fs::create_dir_all(&applications).unwrap();
        let mut file = fs::File::create(applications.join("play-song.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Application\nName=Play Song\nExec=/home/jeremy/.local/bin/play-song\nKeywords=music;song;audio;\nTerminal=false"
        )
        .unwrap();

        let entries =
            discover_desktop_entries_in_dirs(&[applications], &DiscoveryOptions::default())
                .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "play-song.desktop");
        assert_eq!(entries[0].name, "Play Song");
        assert_eq!(entries[0].keywords, vec!["music", "song", "audio"]);
    }

    #[test]
    fn skips_no_display_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let applications = dir.path().join("applications");
        fs::create_dir_all(&applications).unwrap();
        fs::write(
            applications.join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=/bin/true\nNoDisplay=true\n",
        )
        .unwrap();
        let entries =
            discover_desktop_entries_in_dirs(&[applications], &DiscoveryOptions::default())
                .unwrap();
        assert!(entries.is_empty());
    }
}
