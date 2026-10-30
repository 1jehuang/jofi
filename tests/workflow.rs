use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jofi-integration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for sub in [
            "scripts",
            "data/applications",
            "system/applications",
            "cache",
            "state",
        ] {
            fs::create_dir_all(path.join(sub)).unwrap();
        }
        Self(path)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_jofi"));
        c.env("JOFI_SCRIPTS_DIR", self.0.join("scripts"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_DATA_DIRS", self.0.join("system"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_STATE_HOME", self.0.join("state"));
        c
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn discover_compile_cache_launch_and_invalidate() {
    let f = Fixture::new();
    fs::write(f.0.join("scripts/hello.rs"), "// jofi-name: Jofi test launcher\nfn main() { println!(\"hello {}\", std::env::args().nth(1).unwrap_or_default()); }").unwrap();
    fs::write(
        f.0.join("system/applications/app.desktop"),
        "[Desktop Entry]\nType=Application\nName=Fixture desktop\nExec=true\n",
    )
    .unwrap();
    let hidden = f.0.join("data/applications/.old-backups");
    fs::create_dir_all(&hidden).unwrap();
    fs::write(
        hidden.join("old.desktop"),
        "[Desktop Entry]\nType=Application\nName=Do not resurrect backup\nExec=true\n",
    )
    .unwrap();
    let listing = f.command().arg("list").output().unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("Jofi test launcher\trust\t"));
    assert!(listing.contains("Fixture desktop\tdesktop\t"));
    assert!(!listing.contains("Do not resurrect backup"));
    let first = f.command().arg("build").output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stderr).contains("compiling"));
    let cached = f.command().arg("build").output().unwrap();
    assert!(cached.status.success());
    assert!(!String::from_utf8_lossy(&cached.stderr).contains("compiling"));
    assert_eq!(first.stdout, cached.stdout);
    let launch = f
        .command()
        .args(["launch", "Jofi test launcher", "argument with spaces;$HOME"])
        .output()
        .unwrap();
    assert!(launch.status.success());
    assert_eq!(
        String::from_utf8_lossy(&launch.stdout),
        "hello argument with spaces;$HOME\n"
    );
    fs::write(
        f.0.join("scripts/hello.rs"),
        "// jofi-name: Jofi test launcher\nfn main() { println!(\"changed\"); }",
    )
    .unwrap();
    let changed = f
        .command()
        .args(["launch", "Jofi test launcher"])
        .output()
        .unwrap();
    assert!(changed.status.success());
    assert_eq!(changed.stdout, b"changed\n");
    fs::write(f.0.join("scripts/hello.rs"), "fn main( broken").unwrap();
    let counts_before = fs::read(f.0.join("state/jofi/usage.tsv")).unwrap();
    let broken_launch = f.command().args(["launch", "hello"]).output().unwrap();
    assert!(!broken_launch.status.success());
    assert_eq!(
        fs::read(f.0.join("state/jofi/usage.tsv")).unwrap(),
        counts_before
    );
    let usage = f.command().arg("usage").output().unwrap();
    assert!(String::from_utf8_lossy(&usage.stdout)
        .lines()
        .any(|l| l.starts_with("2\t") && l.contains("/scripts/hello.rs\t")));
    let broken = f.command().arg("build").output().unwrap();
    assert!(!broken.status.success());
}

#[test]
fn import_is_explicit_idempotent_and_ambiguity_safe() {
    let f = Fixture::new();
    for (file, name) in [
        ("app", "Unique"),
        ("one", "Duplicate"),
        ("two", "Duplicate"),
    ] {
        fs::write(
            f.0.join(format!("data/applications/{file}.desktop")),
            format!("[Desktop Entry]\nType=Application\nName={name}\nExec=true\n"),
        )
        .unwrap();
    }
    let history = f.0.join("tofi");
    let full = f.0.join("data/applications/app.desktop");
    fs::write(
        &history,
        format!(
            "8 Unique\n8 app.desktop\n8 {}\n20 Duplicate\nbad\n-1 Unique\n",
            full.display()
        ),
    )
    .unwrap();
    for _ in 0..2 {
        let output = f
            .command()
            .arg("import-tofi")
            .arg(&history)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("matched 1 entries, skipped 3"));
    }
    assert!(f
        .command()
        .args(["launch", "Unique"])
        .status()
        .unwrap()
        .success());
    assert!(f
        .command()
        .arg("import-tofi")
        .arg(&history)
        .status()
        .unwrap()
        .success());
    let output = f.command().arg("usage").output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(text.lines().count(), 2);
    assert!(text
        .lines()
        .any(|l| l.starts_with("9\t") && l.contains("\t8\t")));
    fs::write(&history, "10 Unique\n").unwrap();
    assert!(f
        .command()
        .arg("import-tofi")
        .arg(&history)
        .status()
        .unwrap()
        .success());
    assert!(
        String::from_utf8_lossy(&f.command().arg("usage").output().unwrap().stdout)
            .lines()
            .any(|l| l.starts_with("11\t") && l.contains("\t10\t"))
    );
}

#[test]
fn desktop_success_failure_and_concurrent_counts() {
    let f = Fixture::new();
    fs::write(
        f.0.join("data/applications/good.desktop"),
        "[Desktop Entry]\nType=Application\nName=Good\nExec=/bin/true\n",
    )
    .unwrap();
    fs::write(
        f.0.join("data/applications/bad.desktop"),
        "[Desktop Entry]\nType=Application\nName=Bad\nExec=/nonexistent/jofi-do-not-run\n",
    )
    .unwrap();
    assert!(!f
        .command()
        .args(["launch", "Bad"])
        .status()
        .unwrap()
        .success());
    assert!(!f.0.join("state/jofi/usage.tsv").exists());
    assert!(!f
        .command()
        .env("PATH", "")
        .args(["launch", "Good"])
        .status()
        .unwrap()
        .success());
    let children: Vec<_> = (0..12)
        .map(|_| f.command().args(["launch", "Good"]).spawn().unwrap())
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let output = f.command().arg("usage").output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines()
            .any(|line| line.starts_with("12\t") && line.contains("good.desktop\tGood")),
        "{text}"
    );
    assert!(!text.contains("bad.desktop"));
}

#[test]
fn ambiguous_names_are_not_launched() {
    let f = Fixture::new();
    for name in ["one", "two"] {
        fs::write(
            f.0.join(format!("scripts/{name}.rs")),
            "// jofi-name: Duplicate\nfn main() { panic!(\"must not run\"); }",
        )
        .unwrap();
    }
    let output = f.command().args(["launch", "Duplicate"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("found 2"));
    assert!(!f.0.join("cache/jofi/bin").exists());
}
