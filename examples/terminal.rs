// jofi-name: Example terminal
// Copy into ~/.config/jofi/scripts/terminal.rs to make this appear in Jofi.
use std::{io, os::unix::process::CommandExt, process::Command};

fn main() -> io::Result<()> {
    // No shell interpolation. Replace "foot" with your preferred terminal.
    Err(Command::new("foot")
        .args(std::env::args_os().skip(1))
        .exec())
}
