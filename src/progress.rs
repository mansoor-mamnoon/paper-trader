//! Live progress lines on stderr while a run works.
//!
//! The telemetry store is only read after a run ends, and GitHub shows a step's log while it
//! runs, so each stage, model download, model start and model call also prints one line as it
//! happens. A heartbeat prints what the run is doing every minute, so a quiet log means a
//! stalled run and not a slow one. Printing is off until [`enable`] is called, which keeps
//! tests and the crypto commands quiet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();
/// What the run is doing now, outermost first.
static DOING: Mutex<Vec<(u64, String, Instant)>> = Mutex::new(Vec::new());
static NEXT_ID: Mutex<u64> = Mutex::new(0);

/// Turn progress lines on and start a heartbeat that prints every `every`.
pub fn enable(every: Duration) {
    START.get_or_init(Instant::now);
    if ENABLED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(every);
            if let Some(line) = heartbeat_line() {
                log(&line);
            }
        }
    });
}

/// `1h02m`, `4m05s` or `12s`.
pub fn human(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

fn line(message: &str) -> String {
    let clock = chrono::Utc::now().format("%H:%M:%SZ");
    let since = START.get().map_or(Duration::ZERO, Instant::elapsed);
    format!("[{clock} +{}] {message}", human(since))
}

/// Print one progress line, if progress is on.
pub fn log(message: &str) {
    if ENABLED.load(Ordering::Relaxed) {
        eprintln!("{}", line(message));
    }
}

/// The heartbeat text: every open activity with how long it has run, innermost last.
fn heartbeat_line() -> Option<String> {
    let doing = DOING.lock().unwrap_or_else(PoisonError::into_inner);
    if doing.is_empty() {
        return None;
    }
    let chain: Vec<String> = doing
        .iter()
        .map(|(_, what, since)| format!("{what} ({})", human(since.elapsed())))
        .collect();
    Some(format!("still working: {}", chain.join(" > ")))
}

/// An open activity; it leaves the heartbeat when dropped.
#[derive(Debug)]
pub struct Activity {
    id: u64,
}

impl Drop for Activity {
    fn drop(&mut self) {
        DOING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _, _)| *id != self.id);
    }
}

/// Start an activity: print it and keep it in the heartbeat until the guard drops.
pub fn begin(what: &str) -> Activity {
    log(&format!("start {what}"));
    track(what)
}

/// Keep an activity in the heartbeat without printing its start, for frequent short steps
/// such as model calls that print one line when they finish.
pub fn track(what: &str) -> Activity {
    let id = {
        let mut next = NEXT_ID.lock().unwrap_or_else(PoisonError::into_inner);
        *next += 1;
        *next
    };
    DOING.lock().unwrap_or_else(PoisonError::into_inner).push((
        id,
        what.to_string(),
        Instant::now(),
    ));
    Activity { id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human(Duration::from_secs(12)), "12s");
        assert_eq!(human(Duration::from_secs(245)), "4m05s");
        assert_eq!(human(Duration::from_secs(3720)), "1h02m");
    }

    #[test]
    fn heartbeat_lists_open_activities_until_they_end() {
        let outer = begin("test-outer");
        let inner = begin("test-inner");
        let text = heartbeat_line().unwrap();
        let (o, i) = (
            text.find("test-outer").unwrap(),
            text.find("test-inner").unwrap(),
        );
        assert!(o < i, "{text}");
        drop(inner);
        let text = heartbeat_line().unwrap();
        assert!(text.contains("test-outer") && !text.contains("test-inner"));
        drop(outer);
        assert!(heartbeat_line().is_none_or(|t| !t.contains("test-outer")));
    }

    #[test]
    fn lines_carry_the_clock_and_the_run_time() {
        START.get_or_init(Instant::now);
        let text = line("hello");
        assert!(text.starts_with('[') && text.contains("Z +") && text.ends_with("] hello"));
    }
}
