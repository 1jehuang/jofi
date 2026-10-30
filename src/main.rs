mod catalog;
mod font;
mod scripts;
mod search;
mod startup;
mod ui;

use std::{collections::HashMap, env, fs, path::PathBuf};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn usage() {
    println!("jofi: minimal native Wayland app launcher\n\n  jofi                      Open launcher\n  jofi list                 List desktop apps and Rust scripts\n  jofi launch NAME [ARGS]   Launch an exact, unambiguous name\n  jofi build [--force]      Precompile Rust launchers\n  jofi paths                Show script and cache locations\n\nRust scripts: ~/.config/jofi/scripts/*.rs (or JOFI_SCRIPTS_DIR)\nOptional first-line metadata: // jofi-name: My app\nKeys: type to search, Up/Down or Tab to select, Enter to launch, Esc to close");
}
fn identity(entry: &catalog::Entry) -> String {
    match &entry.target {
        catalog::Target::Desktop(p) | catalog::Target::Rust(p) => p.to_string_lossy().into_owned(),
    }
}
fn history_path() -> PathBuf {
    catalog::home_dir("XDG_STATE_HOME", ".local/state").join("jofi/recent")
}
fn record(entry: &catalog::Entry) {
    let path = history_path();
    let id = identity(entry);
    if id.contains(['\n', '\r']) {
        return;
    }
    let previous = fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<_> = std::iter::once(id.as_str())
        .chain(previous.lines().filter(|s| *s != id))
        .take(100)
        .collect();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    if fs::write(&temporary, lines.join("\n")).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}
fn prepare_names(entries: &mut [catalog::Entry], recent: &str) -> Vec<String> {
    let mut ranks = HashMap::new();
    for (rank, id) in recent.lines().enumerate() {
        ranks.entry(id).or_insert(rank);
    }
    entries.sort_by_cached_key(|entry| {
        ranks
            .get(identity(entry).as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    let mut counts = HashMap::new();
    for entry in entries.iter() {
        *counts.entry(entry.name.as_str()).or_insert(0usize) += 1;
    }
    entries
        .iter()
        .map(|entry| {
            if counts[entry.name.as_str()] > 1 {
                format!(
                    "{} [{}]",
                    entry.name,
                    match &entry.target {
                        catalog::Target::Rust(_) => "Rust".into(),
                        catalog::Target::Desktop(path) => path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    }
                )
            } else {
                entry.name.clone()
            }
        })
        .collect()
}

fn run() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            usage();
            return Ok(());
        }
        Some("--version" | "-V") => {
            println!("jofi {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("paths") => {
            println!(
                "scripts: {}\ncache: {}",
                catalog::scripts_dir().display(),
                catalog::cache_dir().display()
            );
            return Ok(());
        }
        _ => {}
    }
    let gui = args.is_empty() || args.first().is_some_and(|arg| arg == "run");
    let lock = if gui {
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or("XDG_RUNTIME_DIR is required for Wayland")?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(runtime.join("jofi.lock"))?;
        match lock.try_lock() {
            Ok(()) => Some(lock),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let pending_font = gui.then(font::start);
    let pending_entries = gui.then(|| {
        std::thread::spawn(|| {
            startup::mark("catalog discovery begin");
            let entries = catalog::discover();
            startup::mark("catalog discovered");
            entries
        })
    });
    let mut entries = if gui { Vec::new() } else { catalog::discover() };
    match args.first().map(String::as_str) {
        Some("list") => {
            for entry in entries {
                println!(
                    "{}\t{}\t{}",
                    entry.name,
                    match entry.target {
                        catalog::Target::Desktop(_) => "desktop",
                        catalog::Target::Rust(_) => "rust",
                    },
                    identity(&entry)
                );
            }
        }
        Some("build") => {
            if args.iter().skip(1).any(|s| s != "--force") {
                return Err("usage: jofi build [--force]".into());
            }
            let mut failures = Vec::new();
            for entry in entries {
                if let catalog::Target::Rust(path) = entry.target {
                    match scripts::compile(
                        &path,
                        &catalog::cache_dir(),
                        args.iter().any(|s| s == "--force"),
                    ) {
                        Ok(binary) => println!("{}\t{}", entry.name, binary.display()),
                        Err(e) => failures.push(format!("{}: {e}", entry.name)),
                    }
                }
            }
            if !failures.is_empty() {
                return Err(failures.join("\n").into());
            }
        }
        Some("launch") => {
            let name = args.get(1).ok_or("usage: jofi launch NAME [ARGS]")?;
            let matches: Vec<_> = entries
                .iter()
                .filter(|e| e.name.eq_ignore_ascii_case(name))
                .collect();
            if matches.len() != 1 {
                return Err(format!(
                    "expected one entry named {name:?}, found {}. Use jofi list to inspect names",
                    matches.len()
                )
                .into());
            }
            record(matches[0]);
            catalog::launch(matches[0], &args[2..])?;
        }
        None | Some("run") => {
            // The advisory lock acquired above lives only while the overlay is open.
            let recent = fs::read_to_string(history_path()).unwrap_or_default();
            let selection = ui::choose(
                || {
                    entries = pending_entries
                        .expect("GUI catalog worker started")
                        .join()
                        .map_err(|_| "catalog worker panicked")?;
                    let names = prepare_names(&mut entries, &recent);
                    startup::mark("menu prepared");
                    Ok(names)
                },
                pending_font.expect("GUI font worker started"),
            )?;
            drop(lock);
            if let Some(index) = selection {
                record(&entries[index]);
                catalog::launch(&entries[index], &[])?;
            }
        }
        Some(other) => return Err(format!("unknown command {other:?}, see jofi --help").into()),
    }
    Ok(())
}
fn main() {
    startup::mark("entry");
    if let Err(error) = run() {
        eprintln!("jofi: {error}");
        // Compositor hotkey launches have no visible stderr. Notify without a shell.
        if env::args().len() == 1 {
            let _ = std::process::Command::new("notify-send")
                .args(["--app-name=jofi", "Jofi", &error.to_string()])
                .status();
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_preparation_preserves_recent_order_and_disambiguates() {
        let mut entries = vec![
            catalog::Entry {
                name: "Alpha".into(),
                target: catalog::Target::Desktop("/alpha.desktop".into()),
            },
            catalog::Entry {
                name: "Browser".into(),
                target: catalog::Target::Desktop("/browser.desktop".into()),
            },
            catalog::Entry {
                name: "Browser".into(),
                target: catalog::Target::Rust("/browser.rs".into()),
            },
            catalog::Entry {
                name: "Zulu".into(),
                target: catalog::Target::Desktop("/zulu.desktop".into()),
            },
        ];
        let labels = prepare_names(&mut entries, "/browser.rs\n/alpha.desktop\n/browser.rs");
        assert_eq!(
            labels,
            [
                "Browser [Rust]",
                "Alpha",
                "Browser [browser.desktop]",
                "Zulu"
            ]
        );
        assert_eq!(identity(&entries[0]), "/browser.rs");
        assert_eq!(identity(&entries[2]), "/browser.desktop");
    }

    #[test]
    fn empty_history_retains_input_order() {
        let mut entries = vec![
            catalog::Entry {
                name: "Alpha".into(),
                target: catalog::Target::Rust("/alpha.rs".into()),
            },
            catalog::Entry {
                name: "Zulu".into(),
                target: catalog::Target::Rust("/zulu.rs".into()),
            },
        ];
        assert_eq!(prepare_names(&mut entries, ""), ["Alpha", "Zulu"]);
    }
}
