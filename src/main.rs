mod catalog;
mod scripts;
mod ui;

use std::{env, fs, path::PathBuf};
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
    let mut entries = catalog::discover();
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
            // An advisory lock lives only while the overlay is open. Crashes release it.
            let runtime = env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .ok_or("XDG_RUNTIME_DIR is required for Wayland")?;
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(runtime.join("jofi.lock"))?;
            if lock.try_lock().is_err() {
                return Ok(());
            }
            let recent = fs::read_to_string(history_path()).unwrap_or_default();
            entries.sort_by_cached_key(|e| {
                recent
                    .lines()
                    .position(|line| line == identity(e))
                    .unwrap_or(usize::MAX)
            });
            let mut names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
            for (i, entry) in entries.iter().enumerate() {
                if entries.iter().filter(|e| e.name == entry.name).count() > 1 {
                    names[i] = format!(
                        "{} [{}]",
                        entry.name,
                        match &entry.target {
                            catalog::Target::Rust(_) => "Rust".into(),
                            catalog::Target::Desktop(p) => p
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                        }
                    );
                }
            }
            let selection = ui::choose(&names)?;
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
