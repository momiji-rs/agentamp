//! Start-up timing. With `AGENTAMP_TRACE=<file>`, each step marks when it
//! finished, in microseconds of wall-clock time, so a harness can line the
//! marks up with when it launched the process. Without it, a mark costs
//! one check.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// When a step finished, in µs since 1970, and the step.
type Marks = Mutex<Vec<(u128, String)>>;

/// Marks kept until `flush`: writing each at once would slow what it times.
static MARKS: OnceLock<Option<Marks>> = OnceLock::new();

fn marks() -> Option<&'static Marks> {
    MARKS.get_or_init(|| std::env::var_os("AGENTAMP_TRACE").map(|_| Mutex::new(Vec::with_capacity(256)))).as_ref()
}

pub fn enabled() -> bool {
    marks().is_some()
}

/// Notes that `step` just finished.
pub fn mark(step: impl Into<String>) {
    if let Some(marks) = marks() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros();
        if let Ok(mut marks) = marks.lock() {
            marks.push((now, step.into()));
        }
    }
}

/// Appends the marks to the trace file, one `<µs since 1970> <step>` per line.
pub fn flush() {
    let (Some(marks), Some(path)) = (marks(), std::env::var_os("AGENTAMP_TRACE")) else { return };
    let Ok(mut marks) = marks.lock() else { return };
    let text = lines(marks.drain(..));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

fn lines(marks: impl Iterator<Item = (u128, String)>) -> String {
    marks.map(|(at, step)| format!("{at} {step}\n")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn marks_are_written_one_per_line() {
        let marks = [(1_000, "main".to_string()), (2_500, "frame 1 600us idle".to_string())];
        assert_eq!(super::lines(marks.into_iter()), "1000 main\n2500 frame 1 600us idle\n");
    }
}
