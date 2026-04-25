use crate::desktop::DesktopEntry;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::process::{Child, Command};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl LaunchCommand {
    pub fn as_vec(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.args.len() + 1);
        out.push(self.program.clone());
        out.extend(self.args.clone());
        out
    }
}

pub fn build_launch_command(entry: &DesktopEntry) -> Result<LaunchCommand> {
    let expanded = expand_exec(&entry.exec, entry)?;
    if expanded.is_empty() {
        bail!("desktop entry {} produced an empty Exec command", entry.id);
    }

    if entry.terminal {
        wrap_terminal(expanded)
    } else {
        Ok(LaunchCommand {
            program: expanded[0].clone(),
            args: expanded[1..].to_vec(),
        })
    }
}

pub fn launch(entry: &DesktopEntry) -> Result<Child> {
    let launch_command = build_launch_command(entry)?;
    Command::new(&launch_command.program)
        .args(&launch_command.args)
        .spawn()
        .with_context(|| format!("failed to launch {}", entry.name))
}

fn wrap_terminal(command: Vec<String>) -> Result<LaunchCommand> {
    let terminal = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".to_string());
    let mut terminal_args = shell_words::split(&terminal)
        .with_context(|| format!("failed to parse TERMINAL={terminal:?}"))?;
    if terminal_args.is_empty() {
        bail!("TERMINAL is empty");
    }

    let program = terminal_args.remove(0);
    terminal_args.push("-e".to_string());
    terminal_args.extend(command);
    Ok(LaunchCommand {
        program,
        args: terminal_args,
    })
}

fn expand_exec(exec: &str, entry: &DesktopEntry) -> Result<Vec<String>> {
    let raw_args = shell_words::split(exec)
        .with_context(|| format!("failed to parse Exec field for {}: {}", entry.id, exec))?;
    let mut out = Vec::with_capacity(raw_args.len());

    for raw_arg in raw_args {
        if raw_arg == "%i" {
            continue;
        }
        let mut expanded = String::with_capacity(raw_arg.len());
        let mut chars = raw_arg.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                expanded.push(ch);
                continue;
            }
            match chars.next() {
                Some('%') => expanded.push('%'),
                Some('c') => expanded.push_str(&entry.name),
                Some('k') => expanded.push_str(&entry.path.to_string_lossy()),
                Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => {
                    // Launching without files/URLs, so file/url field codes disappear.
                    // Deprecated field codes are also ignored.
                }
                Some(other) => {
                    // Unknown field code: preserve enough information for debugging
                    // rather than silently changing the command semantics.
                    expanded.push('%');
                    expanded.push(other);
                }
                None => expanded.push('%'),
            }
        }
        if !expanded.is_empty() {
            out.push(expanded);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::DesktopEntry;
    use std::path::PathBuf;

    fn entry(exec: &str) -> DesktopEntry {
        DesktopEntry {
            id: "example.desktop".to_string(),
            path: PathBuf::from("/tmp/example.desktop"),
            name: "Example App".to_string(),
            generic_name: None,
            comment: None,
            keywords: Vec::new(),
            exec: exec.to_string(),
            icon: None,
            terminal: false,
            categories: Vec::new(),
            source_rank: 0,
        }
    }

    #[test]
    fn strips_file_field_codes() {
        let command = build_launch_command(&entry("/usr/bin/app --open %U --name %c %%")).unwrap();
        assert_eq!(
            command.as_vec(),
            vec!["/usr/bin/app", "--open", "--name", "Example App", "%"]
        );
    }

    #[test]
    fn preserves_quoted_arguments() {
        let command =
            build_launch_command(&entry("/usr/bin/app --profile 'Default User'")).unwrap();
        assert_eq!(
            command.as_vec(),
            vec!["/usr/bin/app", "--profile", "Default User"]
        );
    }
}
