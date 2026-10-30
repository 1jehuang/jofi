//! Opt-in startup timing, with no clock reads on the normal path after initialization.
use std::{sync::OnceLock, time::Instant};
static START: OnceLock<Option<Instant>> = OnceLock::new();

pub fn mark(stage: &str) {
    if let Some(start) =
        START.get_or_init(|| std::env::var_os("JOFI_PROFILE_STARTUP").map(|_| Instant::now()))
    {
        eprintln!(
            "jofi startup: {:>8.3} ms {stage}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
