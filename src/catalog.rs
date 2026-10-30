use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub enum Target {
    Desktop(PathBuf),
    Rust(PathBuf),
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub target: Target,
}

pub fn home_dir(variable: &str, fallback: &str) -> PathBuf {
    env::var_os(variable)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(fallback))
}
pub fn scripts_dir() -> PathBuf {
    env::var_os("JOFI_SCRIPTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir("XDG_CONFIG_HOME", ".config").join("jofi/scripts"))
}
pub fn cache_dir() -> PathBuf {
    home_dir("XDG_CACHE_HOME", ".cache").join("jofi")
}

fn fields(text: &str) -> BTreeMap<&str, &str> {
    let mut result = BTreeMap::new();
    let mut active = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            active = line == "[Desktop Entry]";
            continue;
        }
        if active && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if matches!(
                    key,
                    "Type"
                        | "Name"
                        | "Hidden"
                        | "NoDisplay"
                        | "TryExec"
                        | "OnlyShowIn"
                        | "NotShowIn"
                        | "Exec"
                        | "DBusActivatable"
                ) || key.starts_with("Name[")
                {
                    result.insert(key, value.trim());
                }
            }
        }
    }
    result
}
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => out.push(' '),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(x) => out.push(x),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn executable(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let valid = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        valid(Path::new(name))
    } else {
        env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|p| valid(&p.join(name)))
    }
}
fn name_keys() -> [String; 2] {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    let locale = locale.split('.').next().unwrap_or("");
    let language = locale.split('_').next().unwrap_or("");
    [format!("Name[{locale}]"), format!("Name[{language}]")]
}
fn display_name(map: &BTreeMap<&str, &str>, keys: &[String; 2]) -> Option<String> {
    map.get(keys[0].as_str())
        .or_else(|| map.get(keys[1].as_str()))
        .or_else(|| map.get("Name"))
        .map(|s| unescape(s))
}
fn visible(map: &BTreeMap<&str, &str>, desktops: &[&str]) -> bool {
    if map.get("Type").copied() != Some("Application")
        || ["Hidden", "NoDisplay"]
            .iter()
            .any(|k| map.get(*k).is_some_and(|v| *v == "true"))
    {
        return false;
    }
    if map
        .get("TryExec")
        .is_some_and(|v| !executable(&unescape(v)))
    {
        return false;
    }
    if map
        .get("OnlyShowIn")
        .is_some_and(|v| !v.split(';').any(|d| !d.is_empty() && desktops.contains(&d)))
    {
        return false;
    }
    if map
        .get("NotShowIn")
        .is_some_and(|v| v.split(';').any(|d| !d.is_empty() && desktops.contains(&d)))
    {
        return false;
    }
    map.contains_key("Exec") || map.get("DBusActivatable").is_some_and(|v| *v == "true")
}
fn desktop_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let p = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            desktop_files(root, &p, out);
        } else if p.extension().is_some_and(|e| e == "desktop") {
            if let Ok(relative) = p.strip_prefix(root) {
                out.push((relative.to_string_lossy().replace('/', "-"), p));
            }
        }
    }
}
pub fn discover_from(roots: &[PathBuf], scripts: &Path, desktops: &[&str]) -> Vec<Entry> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let keys = name_keys();
    for root in roots {
        let mut files = Vec::new();
        desktop_files(root, root, &mut files);
        for (id, path) in files {
            if !seen.insert(id) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let map = fields(&text);
            if visible(&map, desktops) {
                if let Some(name) = display_name(&map, &keys)
                    .filter(|s| !s.trim().is_empty() && !s.contains(['\n', '\r']))
                {
                    result.push(Entry {
                        name,
                        target: Target::Desktop(path),
                    });
                }
            }
        }
    }
    if let Ok(files) = fs::read_dir(scripts) {
        for file in files.flatten() {
            let path = file.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap_or_default();
                let name = text
                    .lines()
                    .find_map(|l| l.strip_prefix("// jofi-name:").map(str::trim))
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        path.file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .replace(['_', '-'], " ")
                    });
                result.push(Entry {
                    name,
                    target: Target::Rust(path),
                });
            }
        }
    }
    result.sort_by_cached_key(|e| e.name.to_lowercase());
    result
}
pub fn discover() -> Vec<Entry> {
    let mut roots = vec![home_dir("XDG_DATA_HOME", ".local/share").join("applications")];
    roots.extend(
        env::split_paths(
            &env::var_os("XDG_DATA_DIRS")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
        )
        .map(|p| p.join("applications")),
    );
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    discover_from(
        &roots,
        &scripts_dir(),
        &desktop.split(':').collect::<Vec<_>>(),
    )
}

pub fn prepare_launch(
    entry: &Entry,
    args: &[String],
) -> Result<Command, Box<dyn std::error::Error>> {
    let mut command = match &entry.target {
        Target::Desktop(path) => {
            let mut c = Command::new("gio");
            c.arg("launch").arg(path);
            c
        }
        Target::Rust(path) => Command::new(crate::scripts::compile(path, &cache_dir(), false)?),
    };
    command.args(args);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    #[test]
    fn desktop_group_only() {
        let m = fields("[Desktop Entry]\nName=Hello\\sWorld\nType=Application\nExec=hello\n[Desktop Action foo]\nName=Wrong\n");
        assert_eq!(display_name(&m, &name_keys()).unwrap(), "Hello World");
        assert!(visible(&m, &[]));
    }
    #[test]
    fn borrowed_fields_keep_localization_and_skip_unused_metadata() {
        let m = fields("[Desktop Entry]\nType=Application\nName=Base\nName[fr]=Français\nExec=app --profile work\nComment=unused\nIcon=unused\n");
        assert_eq!(
            display_name(&m, &["Name[fr_FR]".into(), "Name[fr]".into()]).unwrap(),
            "Français"
        );
        assert_eq!(m.get("Exec"), Some(&"app --profile work"));
        assert!(!m.contains_key("Comment"));
        assert!(!m.contains_key("Icon"));
        assert!(visible(&m, &[]));
    }
    #[test]
    fn visibility() {
        for key in [
            "Hidden=true",
            "NoDisplay=true",
            "OnlyShowIn=GNOME;",
            "NotShowIn=niri;",
            "TryExec=/nonexistent/jofi-test",
        ] {
            let text = format!("[Desktop Entry]\nType=Application\nExec=foo\n{key}");
            let m = fields(&text);
            assert!(!visible(&m, &["niri"]), "{key}");
        }
    }
    #[test]
    fn hidden_override() -> io::Result<()> {
        let temp = env::temp_dir().join(format!("jofi-discover-{}", std::process::id()));
        let user = temp.join("user");
        let system = temp.join("system");
        fs::create_dir_all(&user)?;
        fs::create_dir_all(&system)?;
        fs::write(user.join("app.desktop"), "[Desktop Entry]\nHidden=true")?;
        fs::write(
            system.join("app.desktop"),
            "[Desktop Entry]\nType=Application\nName=App\nExec=true",
        )?;
        assert!(discover_from(&[user, system], &temp.join("scripts"), &[]).is_empty());
        fs::remove_dir_all(temp)?;
        Ok(())
    }
}
