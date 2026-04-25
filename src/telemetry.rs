use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Process memory snapshot in KiB. Linux-first because jofi's first target is Wayland/Linux.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub rss_kib: Option<u64>,
    pub vm_size_kib: Option<u64>,
}

impl MemorySnapshot {
    pub fn current() -> Self {
        memory_snapshot_from_proc_status(Path::new("/proc/self/status")).unwrap_or(Self {
            rss_kib: None,
            vm_size_kib: None,
        })
    }
}

#[derive(Debug)]
pub struct Telemetry {
    writer: Mutex<Option<File>>,
    path: Option<PathBuf>,
}

impl Telemetry {
    pub fn new(path: Option<PathBuf>) -> Result<Self> {
        let writer = match path.as_ref() {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create telemetry directory {}", parent.display())
                    })?;
                }
                Some(
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .with_context(|| {
                            format!("failed to open telemetry log {}", path.display())
                        })?,
                )
            }
            None => None,
        };
        Ok(Self {
            writer: Mutex::new(writer),
            path,
        })
    }

    pub fn enabled(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn span<'a>(&'a self, name: impl Into<String>) -> Span<'a> {
        Span {
            telemetry: self,
            name: name.into(),
            start: Instant::now(),
            start_mem: MemorySnapshot::current(),
            fields: Map::new(),
        }
    }

    pub fn event(&self, name: &str, fields: Map<String, Value>) {
        if !self.enabled() {
            return;
        }
        let mut record = base_record("event", name);
        record.extend(fields);
        self.write_record(record);
    }

    fn write_record(&self, record: Map<String, Value>) {
        let Ok(mut guard) = self.writer.lock() else {
            return;
        };
        let Some(writer) = guard.as_mut() else {
            return;
        };
        if serde_json::to_writer(&mut *writer, &record).is_ok() {
            let _ = writer.write_all(b"\n");
        }
    }
}

#[derive(Debug)]
pub struct Span<'a> {
    telemetry: &'a Telemetry,
    name: String,
    start: Instant,
    start_mem: MemorySnapshot,
    fields: Map<String, Value>,
}

impl<'a> Span<'a> {
    pub fn field(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        self.fields.insert(key.into(), json!(value));
        self
    }

    pub fn set_field(&mut self, key: impl Into<String>, value: impl Serialize) {
        self.fields.insert(key.into(), json!(value));
    }
}

impl Drop for Span<'_> {
    fn drop(&mut self) {
        if !self.telemetry.enabled() {
            return;
        }

        let end_mem = MemorySnapshot::current();
        let mut record = base_record("span", &self.name);
        record.insert(
            "duration_ns".to_string(),
            json!(self.start.elapsed().as_nanos() as u64),
        );
        record.insert("rss_kib".to_string(), json!(end_mem.rss_kib));
        record.insert("vm_size_kib".to_string(), json!(end_mem.vm_size_kib));
        record.insert(
            "rss_delta_kib".to_string(),
            json!(delta(end_mem.rss_kib, self.start_mem.rss_kib)),
        );
        record.insert(
            "vm_size_delta_kib".to_string(),
            json!(delta(end_mem.vm_size_kib, self.start_mem.vm_size_kib)),
        );
        record.extend(std::mem::take(&mut self.fields));
        self.telemetry.write_record(record);
    }
}

pub fn default_telemetry_path() -> PathBuf {
    state_home().join("jofi").join("telemetry.jsonl")
}

pub fn state_home() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".jofi-state"))
}

fn base_record(kind: &str, name: &str) -> Map<String, Value> {
    let ts_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let mut record = Map::new();
    record.insert("ts_unix_ms".to_string(), json!(ts_unix_ms));
    record.insert("kind".to_string(), json!(kind));
    record.insert("name".to_string(), json!(name));
    record.insert("pid".to_string(), json!(std::process::id()));
    record
}

fn delta(end: Option<u64>, start: Option<u64>) -> Option<i64> {
    Some(end? as i64 - start? as i64)
}

fn memory_snapshot_from_proc_status(path: &Path) -> Result<MemorySnapshot> {
    let status =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(memory_snapshot_from_status_text(&status))
}

fn memory_snapshot_from_status_text(status: &str) -> MemorySnapshot {
    let mut rss_kib = None;
    let mut vm_size_kib = None;

    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_kib = parse_kib(value);
        } else if let Some(value) = line.strip_prefix("VmSize:") {
            vm_size_kib = parse_kib(value);
        }
    }

    MemorySnapshot {
        rss_kib,
        vm_size_kib,
    }
}

fn parse_kib(value: &str) -> Option<u64> {
    value.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_status_memory() {
        let snapshot = memory_snapshot_from_status_text(
            "Name:\tjofi\nVmSize:\t 123456 kB\nVmRSS:\t   7890 kB\n",
        );
        assert_eq!(snapshot.rss_kib, Some(7890));
        assert_eq!(snapshot.vm_size_kib, Some(123456));
    }

    #[test]
    fn telemetry_can_be_disabled() {
        let telemetry = Telemetry::new(None).unwrap();
        assert!(!telemetry.enabled());
        let _span = telemetry.span("disabled");
    }
}
