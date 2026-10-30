mod catalog;
mod clipboard;
mod font;
mod input;
mod scripts;
mod search;
mod startup;
mod ui;
mod usage;

use std::{collections::HashMap, env, fs, path::PathBuf};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn usage() {
    println!("jofi: minimal native Wayland app launcher\n\n  jofi                      Open launcher\n  jofi list                 List desktop apps and Rust scripts\n  jofi launch NAME [ARGS]   Launch an exact, unambiguous name\n  jofi build [--force]      Precompile Rust launchers\n  jofi usage                Inspect local usage counts (TSV)\n  jofi import-tofi PATH     Explicitly import Tofi frequency history\n  jofi paths                Show script and cache locations\n\nRust scripts: ~/.config/jofi/scripts/*.rs (or JOFI_SCRIPTS_DIR)\nOptional first-line metadata: // jofi-name: My app\nKeys: type to search, Up/Down or Tab to select, Enter to launch, Esc to close\nAlt/Ctrl+A select all, C copy, X cut, V paste; Alt/Ctrl+Backspace delete word; Ctrl+U clear");
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
    if let Err(error) = usage::record(&identity(entry)) {
        eprintln!("jofi: could not record usage: {error}");
    }
}
fn launch(entry: &catalog::Entry, args: &[String], wait: bool) -> Result<()> {
    let mut command = catalog::prepare_launch(entry, args)?;
    let mut child = command.spawn()?;
    if matches!(entry.target, catalog::Target::Rust(_)) {
        record(entry);
        if !wait {
            return Ok(());
        }
    }
    let status = child.wait()?;
    if status.success() && matches!(entry.target, catalog::Target::Desktop(_)) {
        record(entry);
    }
    if !status.success() {
        return Err(format!("launcher process exited {status}").into());
    }
    Ok(())
}
fn prepare_names(entries: &mut [catalog::Entry], recent: &str) -> Vec<String> {
    prepare_names_with_usage(entries, recent, &usage::read())
}
fn prepare_names_with_usage(
    entries: &mut [catalog::Entry],
    recent: &str,
    usage: &usage::Records,
) -> Vec<String> {
    let mut ranks = HashMap::new();
    for (rank, id) in recent.lines().enumerate() {
        ranks.entry(id).or_insert(rank);
    }
    entries.sort_by_cached_key(|entry| {
        let id = identity(entry);
        let record = usage.get(&id);
        (
            std::cmp::Reverse(record.map_or(0, |r| r.count)),
            std::cmp::Reverse(record.map_or(0, |r| r.last)),
            ranks.get(id.as_str()).copied().unwrap_or(usize::MAX),
        )
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
        Some("usage") => {
            let names: HashMap<_, _> = catalog::discover()
                .into_iter()
                .map(|e| (identity(&e), e.name))
                .collect();
            println!("count\tlast_used_unix_ms\timported_count\tidentity\tname");
            for (id, r) in usage::read() {
                let name = names
                    .get(&id)
                    .map(String::as_str)
                    .unwrap_or("")
                    .replace(['\t', '\n', '\r'], " ");
                println!("{}\t{}\t{}\t{id}\t{name}", r.count, r.last, r.imported);
            }
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
        Some("import-tofi") => {
            if args.len() != 2 {
                return Err("usage: jofi import-tofi PATH".into());
            }
            let (matched, skipped) = usage::import(std::path::Path::new(&args[1]), &entries)?;
            println!("matched {matched} entries, skipped {skipped} unmatched, ambiguous or malformed records");
        }
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
            launch(matches[0], &args[2..], true)?;
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
                launch(&entries[index], &[], false)?;
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
        let labels = prepare_names_with_usage(
            &mut entries,
            "/browser.rs\n/alpha.desktop\n/browser.rs",
            &usage::Records::new(),
        );
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
    fn frequency_then_timestamp_then_legacy_recency() {
        let mut entries: Vec<_> = ["Alpha", "Beta", "Gamma", "Delta"]
            .into_iter()
            .map(|name| catalog::Entry {
                name: name.into(),
                target: catalog::Target::Rust(format!("/{name}").into()),
            })
            .collect();
        let mut records = usage::Records::new();
        records.insert(
            "/Alpha".into(),
            usage::Record {
                count: 2,
                last: 10,
                imported: 0,
            },
        );
        records.insert(
            "/Beta".into(),
            usage::Record {
                count: 3,
                last: 1,
                imported: 0,
            },
        );
        records.insert(
            "/Gamma".into(),
            usage::Record {
                count: 2,
                last: 20,
                imported: 0,
            },
        );
        records.insert(
            "/Delta".into(),
            usage::Record {
                count: 2,
                last: 20,
                imported: 0,
            },
        );
        assert_eq!(
            prepare_names_with_usage(&mut entries, "/Delta\n/Gamma", &records),
            ["Beta", "Delta", "Gamma", "Alpha"]
        );
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
        assert_eq!(
            prepare_names_with_usage(&mut entries, "", &usage::Records::new()),
            ["Alpha", "Zulu"]
        );
    }
}
