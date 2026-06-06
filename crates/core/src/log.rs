//! A tiny global log buffer + download progress. Every line is mirrored to the
//! terminal (via sheen) and kept in memory so the in-app Console can show it.
//! Download progress is tracked separately for the UI bar and a CLI bar.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

#[derive(Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Warn,
    Error,
    Game,
}

#[derive(Clone)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

const MAX_LINES: usize = 5000;

static BUS: OnceLock<Mutex<Vec<Line>>> = OnceLock::new();
fn bus() -> MutexGuard<'static, Vec<Line>> {
    // Recover from poisoning instead of cascading panics across threads.
    BUS.get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub fn push(level: Level, text: impl Into<String>) {
    let text = text.into();

    let logger = sheen::global::logger();
    match level {
        Level::Info | Level::Game => logger.info(&text, &[]),
        Level::Warn => logger.warn(&text, &[]),
        Level::Error => logger.error(&text, &[]),
    }

    let mut b = bus();
    b.push(Line { level, text });
    if b.len() > MAX_LINES {
        let overflow = b.len() - MAX_LINES;
        b.drain(0..overflow);
    }
}

pub fn info(text: impl Into<String>) { push(Level::Info, text) }
pub fn warn(text: impl Into<String>) { push(Level::Warn, text) }
pub fn error(text: impl Into<String>) { push(Level::Error, text) }
pub fn game(text: impl Into<String>) { push(Level::Game, text) }

/// Clone at most `max` of the most recent lines (cheap enough for the UI).
pub fn tail(max: usize) -> Vec<Line> {
    let b = bus();
    let start = b.len().saturating_sub(max);
    b[start..].to_vec()
}

pub fn len() -> usize {
    bus().len()
}

pub fn all_text() -> String {
    bus()
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn clear() {
    bus().clear();
}

// ---- download progress --------------------------------------------------
static P_DONE: AtomicUsize = AtomicUsize::new(0);
static P_TOTAL: AtomicUsize = AtomicUsize::new(0);
static P_LAST_PCT: AtomicUsize = AtomicUsize::new(usize::MAX);
static P_LABEL: OnceLock<Mutex<String>> = OnceLock::new();

fn label_cell() -> &'static Mutex<String> {
    P_LABEL.get_or_init(|| Mutex::new(String::new()))
}

pub fn progress_start(label: impl Into<String>, total: usize) {
    *label_cell().lock().unwrap_or_else(|e| e.into_inner()) = label.into();
    P_TOTAL.store(total, Ordering::Relaxed);
    P_DONE.store(0, Ordering::Relaxed);
    P_LAST_PCT.store(usize::MAX, Ordering::Relaxed);
    print_bar();
}

pub fn progress_inc() {
    P_DONE.fetch_add(1, Ordering::Relaxed);
    print_bar();
}

pub fn progress_finish() {
    if P_TOTAL.load(Ordering::Relaxed) > 0 {
        eprintln!(); // end the \r line
    }
    P_TOTAL.store(0, Ordering::Relaxed);
}

/// (done, total, label) when a download is active.
pub fn progress() -> Option<(usize, usize, String)> {
    let total = P_TOTAL.load(Ordering::Relaxed);
    if total == 0 {
        return None;
    }
    let done = P_DONE.load(Ordering::Relaxed).min(total);
    let label = label_cell().lock().unwrap_or_else(|e| e.into_inner()).clone();
    Some((done, total, label))
}

fn print_bar() {
    let total = P_TOTAL.load(Ordering::Relaxed);
    if total == 0 {
        return;
    }
    let done = P_DONE.load(Ordering::Relaxed).min(total);
    let pct = done * 100 / total;
    // only redraw on whole-percent changes to avoid spamming the terminal
    if P_LAST_PCT.swap(pct, Ordering::Relaxed) == pct {
        return;
    }
    let filled = pct / 5; // 20-char bar
    let bar: String = (0..20).map(|i| if i < filled { '#' } else { '-' }).collect();
    let label = label_cell().lock().unwrap_or_else(|e| e.into_inner()).clone();
    eprint!("\r  {label} [{bar}] {pct:>3}% ({done}/{total})");
    use std::io::Write;
    let _ = std::io::stderr().flush();
}
