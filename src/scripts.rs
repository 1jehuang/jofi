use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

// Include all regular files below the source directory in the fingerprint, so
// local modules and include_str! assets invalidate the cache as well.
fn fingerprint_tree(dir: &Path, hash: &mut DefaultHasher) -> io::Result<()> {
    let mut paths: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    paths.sort_by_key(|e| e.file_name());
    for entry in paths {
        entry.file_name().hash(hash);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fingerprint_tree(&entry.path(), hash)?;
        } else if kind.is_file() {
            fs::read(entry.path())?.hash(hash);
        } else if kind.is_symlink() {
            fs::read_link(entry.path())?.hash(hash);
            if entry.path().is_file() {
                fs::read(entry.path())?.hash(hash);
            }
        }
    }
    Ok(())
}
pub fn compile(
    source: &Path,
    cache: &Path,
    force: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let source = source.canonicalize()?;
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    env!("CARGO_PKG_VERSION").hash(&mut hash);
    fingerprint_tree(
        source.parent().ok_or("script has no parent directory")?,
        &mut hash,
    )?;
    let key = format!("{:016x}", hash.finish());
    let cache = cache.join("bin");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cache)?;
    let metadata = fs::symlink_metadata(&cache)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
        return Err("jofi cache must be a real directory, not writable by other users".into());
    }
    let binary = cache.join(&key);
    if !force && binary.is_file() {
        return Ok(binary);
    }
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary = cache.join(format!(".{key}-{}-{unique}", std::process::id()));
    eprintln!("jofi: compiling {}", source.display());
    let result = Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-name",
            "jofi_launcher",
            "-C",
            "debuginfo=0",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&temporary)
        .status();
    match result {
        Ok(status) if status.success() => {
            fs::rename(&temporary, &binary)?;
            Ok(binary)
        }
        result => {
            let _ = fs::remove_file(&temporary);
            match result {
                Ok(status) => Err(format!("Rust launcher compilation failed ({status})").into()),
                Err(e) => Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_changes_with_assets() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("jofi-fingerprint-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("a.rs"), "fn main() {}")?;
        let mut first = DefaultHasher::new();
        fingerprint_tree(&dir, &mut first)?;
        fs::write(dir.join("asset"), "changed")?;
        let mut second = DefaultHasher::new();
        fingerprint_tree(&dir, &mut second)?;
        assert_ne!(first.finish(), second.finish());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
