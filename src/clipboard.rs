//! Synchronous clipboard helpers, intended for a UI worker rather than the event loop.
use std::{
    io::{Read, Write},
    net::Shutdown,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PASTE_BYTES: usize = 64 * 1024;

pub fn copy(text: &str) -> Result<(), String> {
    copy_with(Path::new("wl-copy"), text, TIMEOUT)
}

pub fn paste() -> Result<String, String> {
    paste_with(Path::new("wl-paste"), TIMEOUT)
}

fn copy_with(program: &Path, text: &str, timeout: Duration) -> Result<(), String> {
    transfer(program, Some(text.as_bytes()), timeout).map(|_| ())
}

fn paste_with(program: &Path, timeout: Duration) -> Result<String, String> {
    let bytes = transfer(program, None, timeout)?;
    String::from_utf8(bytes).map_err(|error| format!("clipboard text is not valid UTF-8: {error}"))
}

fn transfer(program: &Path, input: Option<&[u8]>, timeout: Duration) -> Result<Vec<u8>, String> {
    let label = program.display();
    let started = Instant::now();
    // A nonblocking parent socket keeps both writes and reads inside the deadline,
    // without reader/writer threads that could outlive a failed helper.
    let (mut stream, child_stream) = UnixStream::pair()
        .map_err(|error| format!("cannot create clipboard I/O for {label}: {error}"))?;
    stream
        .set_nonblocking(true)
        .map_err(|error| format!("cannot configure clipboard I/O for {label}: {error}"))?;
    let mut command = Command::new(program);
    command.stderr(Stdio::null());
    let child_io = Stdio::from(OwnedFd::from(child_stream));
    if input.is_some() {
        command
            .args(["--type", "text/plain;charset=utf-8"])
            .stdin(child_io)
            // wl-copy forks by default. Its daemon must not inherit a captured
            // output pipe, and must survive the launcher's exit to own the text.
            .stdout(Stdio::null());
    } else {
        command
            .args(["--no-newline", "--type", "text"])
            .stdin(Stdio::null())
            .stdout(child_io);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start clipboard helper {label}: {error}"))?;
    // Command retains configured file descriptors even after spawn.
    drop(command);
    let result = (|| {
        let mut written = 0;
        let mut io_done = false;
        let mut output = Vec::new();
        loop {
            if started.elapsed() >= timeout {
                return Err(format!(
                    "clipboard helper {label} timed out after {timeout:?}"
                ));
            }
            if !io_done {
                if let Some(bytes) = input {
                    if written == bytes.len() {
                        stream.shutdown(Shutdown::Write).map_err(|error| {
                            format!("cannot finish clipboard input to {label}: {error}")
                        })?;
                        io_done = true;
                    } else {
                        match stream.write(&bytes[written..]) {
                            Ok(0) => {
                                return Err(format!("clipboard helper {label} closed its input"))
                            }
                            Ok(count) => written += count,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue
                            }
                            Err(error) => {
                                return Err(format!(
                                    "cannot write clipboard input to {label}: {error}"
                                ))
                            }
                        }
                    }
                } else {
                    let mut buffer = [0; 8192];
                    match stream.read(&mut buffer) {
                        Ok(0) => io_done = true,
                        Ok(count) => {
                            if output.len() + count > MAX_PASTE_BYTES {
                                return Err(format!(
                                    "clipboard paste exceeds {MAX_PASTE_BYTES} bytes"
                                ));
                            }
                            output.extend_from_slice(&buffer[..count]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            return Err(format!(
                                "cannot read clipboard output from {label}: {error}"
                            ))
                        }
                    }
                }
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("cannot wait for clipboard helper {label}: {error}"))?
            {
                if !status.success() {
                    return Err(format!("clipboard helper {label} exited with {status}"));
                }
                if io_done {
                    return Ok(output);
                }
                if input.is_some() {
                    return Err(format!(
                        "clipboard helper {label} exited before accepting all input"
                    ));
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() {
        // Always reap the direct helper, including early I/O and size failures.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(body: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "jofi-clipboard-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let path = directory.join("helper");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn assert_reaped(&self) {
            // If scheduling delayed the child until after its deadline, it may
            // have been killed before writing a PID at all.
            if let Ok(pid) = fs::read_to_string(self.0.with_extension("pid")) {
                assert!(!Path::new("/proc").join(pid.trim()).exists());
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn missing_helpers_are_descriptive() {
        let fixture = Fixture::new("exit 0");
        let missing = fixture.0.with_file_name("does-not-exist");
        assert!(copy_with(&missing, "text", TIMEOUT)
            .unwrap_err()
            .contains("cannot start"));
        assert!(paste_with(&missing, TIMEOUT)
            .unwrap_err()
            .contains("cannot start"));
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let fixture = Fixture::new("cat >/dev/null\nexit 17");
        assert!(copy_with(fixture.path(), "text", TIMEOUT)
            .unwrap_err()
            .contains("17"));
        assert!(paste_with(fixture.path(), TIMEOUT)
            .unwrap_err()
            .contains("17"));
    }

    #[test]
    fn unicode_copy_uses_stdin_and_only_fixed_arguments() {
        let fixture = Fixture::new(
            r#"[ "$#" = 2 ] || exit 11
[ "$1" = --type ] || exit 12
[ "$2" = 'text/plain;charset=utf-8' ] || exit 13
cat > "${0}.input""#,
        );
        let text = "秘密 🦀 ' $(touch should-not-exist)\nsecond line\n";
        copy_with(fixture.path(), text, TIMEOUT).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.0.with_extension("input")).unwrap(),
            text
        );
    }

    #[test]
    fn unicode_paste_preserves_newlines_and_checks_arguments() {
        let fixture = Fixture::new(
            r#"[ "$#" = 3 ] || exit 11
[ "$1" = --no-newline ] || exit 12
[ "$2" = --type ] || exit 13
[ "$3" = text ] || exit 14
printf '秘密 🦀\n\n'"#,
        );
        assert_eq!(paste_with(fixture.path(), TIMEOUT).unwrap(), "秘密 🦀\n\n");
    }

    #[test]
    fn timeout_covers_blocked_copy_and_paste_and_reaps_helper() {
        // A shell builtin loop has no descendants that could retain descriptors.
        let fixture = Fixture::new("printf '%s' \"$$\" > \"${0}.pid\"\nwhile :; do :; done");
        let deadline = Duration::from_millis(40);
        let started = Instant::now();
        let large = "x".repeat(1024 * 1024);
        assert!(copy_with(fixture.path(), &large, deadline)
            .unwrap_err()
            .contains("timed out"));
        fixture.assert_reaped();
        assert!(paste_with(fixture.path(), deadline)
            .unwrap_err()
            .contains("timed out"));
        fixture.assert_reaped();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn oversized_paste_is_rejected() {
        let fixture = Fixture::new("printf '%s' \"$$\" > \"${0}.pid\"\nwhile :; do printf '0123456789abcdef0123456789abcdef'; done");
        assert!(paste_with(fixture.path(), TIMEOUT)
            .unwrap_err()
            .contains("exceeds 65536 bytes"));
        fixture.assert_reaped();
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let fixture = Fixture::new("printf '\\377'");
        assert!(paste_with(fixture.path(), TIMEOUT)
            .unwrap_err()
            .contains("not valid UTF-8"));
    }
}
