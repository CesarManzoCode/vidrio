//! The log of the `vidrio` binary: one line per event on stderr, with the
//! seconds since the process started.

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Fixes the start of the clock of the log (the first call wins).
pub fn init() {
    START.get_or_init(Instant::now);
}

/// The line [`info`] writes for `message` after `secs` seconds.
pub fn format_line(secs: f64, message: &str) -> String {
    format!("[vidrio {secs:9.3}] {message}")
}

/// Writes `message` to stderr.
pub fn info(message: &str) {
    let start = *START.get_or_init(Instant::now);
    eprintln!("{}", format_line(start.elapsed().as_secs_f64(), message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_the_time_and_message() {
        assert_eq!(format_line(1.5, "hello"), "[vidrio     1.500] hello");
    }
}
