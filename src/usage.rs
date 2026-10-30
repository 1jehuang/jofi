//! Private, advisory-locked usage storage. All timestamps are Unix milliseconds.
use crate::{catalog, Result};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Default)]
pub struct Record {
    pub count: u64,
    pub last: u64,
    pub imported: u64,
}
pub type Records = BTreeMap<String, Record>;
pub fn directory() -> PathBuf {
    catalog::home_dir("XDG_STATE_HOME", ".local/state").join("jofi")
}
pub fn read() -> Records {
    read_at(&directory().join("usage.tsv"))
}
fn read_at(path: &Path) -> Records {
    let mut records = Records::new();
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || fields[3].is_empty() {
            continue;
        }
        if let (Ok(count), Ok(last), Ok(imported)) = (
            fields[0].parse::<u64>(),
            fields[1].parse::<u64>(),
            fields[2].parse::<u64>(),
        ) {
            if imported <= count {
                records.insert(
                    fields[3].into(),
                    Record {
                        count,
                        last,
                        imported,
                    },
                );
            }
        }
    }
    records
}
fn valid(id: &str) -> bool {
    !id.is_empty() && !id.contains(['\t', '\n', '\r'])
}
fn atomic(dir: &Path, name: &str, text: &str) -> Result<()> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary = dir.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, dir.join(name))?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn update_at<T>(dir: &Path, change: impl FnOnce(&mut Records) -> Result<T>) -> Result<T> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("usage.lock"))?;
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    lock.lock()?;
    let mut records = read_at(&dir.join("usage.tsv"));
    let result = change(&mut records)?;
    let text: String = records
        .iter()
        .map(|(id, r)| format!("{}\t{}\t{}\t{id}\n", r.count, r.last, r.imported))
        .collect();
    atomic(dir, "usage.tsv", &text)?;
    Ok(result)
}
pub fn record(id: &str) -> Result<()> {
    if !valid(id) {
        return Err("launch identity cannot contain TSV separators".into());
    }
    let dir = directory();
    update_at(&dir, |records| {
        let r = records.entry(id.into()).or_default();
        r.count = r.count.checked_add(1).ok_or("usage counter overflow")?;
        r.last = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        // Keep the legacy recent file under the same lock for existing workflows.
        let previous = fs::read_to_string(dir.join("recent")).unwrap_or_default();
        let text = std::iter::once(id)
            .chain(previous.lines().filter(|line| *line != id))
            .take(100)
            .collect::<Vec<_>>()
            .join("\n");
        atomic(&dir, "recent", &text)?;
        Ok(())
    })
}
pub fn import(path: &Path, entries: &[catalog::Entry]) -> Result<(usize, usize)> {
    let text = fs::read_to_string(path)?;
    let mut incoming = BTreeMap::<String, u64>::new();
    let mut skipped = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Some((count, name)) = line.split_once(' ') else {
            skipped += 1;
            continue;
        };
        let Ok(count) = count.parse::<u64>() else {
            skipped += 1;
            continue;
        };
        let matches: Vec<_> = entries
            .iter()
            .filter(|entry| match &entry.target {
                catalog::Target::Desktop(path) => {
                    name == entry.name
                        || name == path.to_string_lossy()
                        || path.file_name().is_some_and(|f| f == name)
                }
                catalog::Target::Rust(_) => false,
            })
            .collect();
        if matches.len() != 1 {
            skipped += 1;
            continue;
        }
        let id = crate::identity(matches[0]);
        if !valid(&id) {
            skipped += 1;
            continue;
        }
        incoming
            .entry(id)
            .and_modify(|n| *n = (*n).max(count))
            .or_insert(count);
    }
    let matched = incoming.len();
    update_at(&directory(), |records| {
        for (id, count) in incoming {
            let r = records.entry(id).or_default();
            r.count = r
                .count
                .checked_add(count.saturating_sub(r.imported))
                .ok_or("usage counter overflow")?;
            r.imported = r.imported.max(count);
        }
        Ok(())
    })?;
    Ok((matched, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_records_are_ignored() {
        let dir = std::env::temp_dir().join(format!("jofi-usage-parser-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("usage.tsv"),
            "bad\n-1\t2\t0\tx\n1\t2\t3\tx\n4\t5\t2\t/good\n",
        )
        .unwrap();
        let records = read_at(&dir.join("usage.tsv"));
        assert_eq!(records.len(), 1);
        assert_eq!(records["/good"].count, 4);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn locked_updates_are_lossless_and_private() {
        let dir = std::env::temp_dir().join(format!("jofi-usage-lock-{}", std::process::id()));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        update_at(&dir, |r| {
                            r.entry("app".into()).or_default().count += 1;
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(read_at(&dir.join("usage.tsv"))["app"].count, 160);
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.join("usage.tsv"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
