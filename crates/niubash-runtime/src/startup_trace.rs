//! Opt-in startup timing diagnostics.
//!
//! Set `NIU_TRACE_STARTUP=1` (also `true`/`on`) to print per-stage elapsed
//! timings to **stderr**. Output is silent unless explicitly enabled, which
//! keeps `niu -c` and script runs deterministic and banner-free by default.

use std::cell::Cell;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

thread_local! {
    static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn is_enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("NIU_TRACE_STARTUP")
            .map(|value| {
                ["1", "true", "on"]
                    .iter()
                    .any(|name| value.eq_ignore_ascii_case(name))
            })
            .unwrap_or(false)
    })
}

fn start() -> &'static Instant {
    START.get_or_init(Instant::now)
}

/// Record a stage boundary. Prints cumulative and per-stage milliseconds.
pub fn tick(label: &str) {
    if !is_enabled() {
        return;
    }
    let now = Instant::now();
    let total = now.duration_since(*start()).as_micros() as f64 / 1000.0;
    let delta = LAST.with(|last| {
        last.get()
            .map(|previous| (now.duration_since(previous).as_micros() as f64 / 1000.0).to_string())
            .unwrap_or_else(|| "-".to_string())
    });
    let _ = writeln!(
        std::io::stderr(),
        "NIU_TRACE_STARTUP: {:>8.1} ms  (delta {:>8})  {}",
        total, delta, label
    );
    LAST.with(|last| last.set(Some(now)));
}
