//! Error log and crash reports, for working out what went wrong on somebody else's machine.
//!
//! * `%APPDATA%\SpaceTree\logs\spacetree.log` — scans, failed deletions, update problems; rotated at 512 KB.
//! * `%APPDATA%\SpaceTree\logs\crashes\crash-<time>.txt` — written by the panic hook: version, message,
//!   location, backtrace and the tail of the log. Nothing is ever sent anywhere; the user attaches the file.
//!
//! The log mentions paths only for errors that are about that path (failed deletion, unreadable folder);
//! as the crash report contains the log tail, it can mention them too.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::settings;

const MAX_LOG: u64 = 512 << 10;
const TAIL_LINES: usize = 60;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

pub fn logs_dir() -> Option<PathBuf> {
    settings::dir().map(|d| d.join("logs"))
}

fn crashes_dir() -> Option<PathBuf> {
    logs_dir().map(|d| d.join("crashes"))
}

fn log_file() -> Option<PathBuf> {
    logs_dir().map(|d| d.join("spacetree.log"))
}

/// `2026-10-13 18:04:09` (UTC).
pub fn timestamp(unix: u64) -> String {
    let (days, rem) = (unix / 86_400, unix % 86_400);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Appends a line to the log. Never fails loudly: a broken log must not break the program.
pub fn log(level: Level, msg: impl AsRef<str>) {
    if settings::is_demo() {
        return;
    }
    let (Some(dir), Some(file)) = (logs_dir(), log_file()) else {
        return;
    };
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(&file, dir.join("spacetree.log.1"));
    }
    let tag = match level {
        Level::Info => "INFO ",
        Level::Warn => "WARN ",
        Level::Error => "ERROR",
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        // one record per line: keep multi-line messages readable
        let msg = msg.as_ref().replace('\n', "\n    ");
        let _ = writeln!(f, "{} {tag} {msg}", timestamp(now()));
    }
}

pub fn info(msg: impl AsRef<str>) {
    log(Level::Info, msg);
}

pub fn warn(msg: impl AsRef<str>) {
    log(Level::Warn, msg);
}

pub fn error(msg: impl AsRef<str>) {
    log(Level::Error, msg);
}

/// The last lines of the log (and of the rotated one if the current file is short).
fn log_tail(lines: usize) -> String {
    let (Some(file), Some(dir)) = (log_file(), logs_dir()) else {
        return String::new();
    };
    let mut text = std::fs::read_to_string(dir.join("spacetree.log.1")).unwrap_or_default();
    text.push_str(&std::fs::read_to_string(file).unwrap_or_default());
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn header() -> String {
    format!(
        "SpaceTree {}\nOS: {} {}\nRelease build: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(not(debug_assertions)),
    )
}

/// Text for the "copy diagnostics" button: versions plus the log tail.
pub fn report() -> String {
    let mut s = header();
    let _ = write!(s, "\n--- last log lines ---\n{}\n", log_tail(TAIL_LINES));
    if let Some(c) = latest_crash() {
        let _ = write!(s, "\n--- latest crash report: {} ---\n", c.display());
        s.push_str(&std::fs::read_to_string(&c).unwrap_or_default());
    }
    s
}

/// Installs the panic hook. Call first thing in `main`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").to_owned();
        let location = info
            .location()
            .map_or("unknown".to_owned(), |l| format!("{}:{}", l.file(), l.line()));
        let message = info.payload_as_str().unwrap_or("(non-text panic payload)");
        let mut text = header();
        let _ = write!(
            text,
            "Time (UTC): {}\nThread: {name}\nPanic: {message}\nLocation: {location}\n\n--- backtrace ---\n{}\n\n--- last log lines ---\n{}\n",
            timestamp(now()),
            std::backtrace::Backtrace::force_capture(),
            log_tail(TAIL_LINES),
        );
        error(format!("PANIC in thread {name} at {location}: {message}"));
        let saved = save_crash(&text);
        // a panic in a worker is reported by the app itself; the main thread is gone, so say something
        if name == "main" {
            show_crash_dialog(saved.as_deref());
        }
        previous(info);
    }));
}

fn save_crash(text: &str) -> Option<PathBuf> {
    if settings::is_demo() {
        return None;
    }
    let dir = crashes_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("crash-{}.txt", now()));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

fn latest_crash() -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(crashes_dir()?)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    v.sort();
    v.pop()
}

/// A crash report that the user has not been told about yet (from the previous run). Marks it as seen.
pub fn take_unseen_crash() -> Option<PathBuf> {
    let latest = latest_crash()?;
    let marker = logs_dir()?.join("last_seen_crash");
    let name = latest.file_name()?.to_string_lossy().into_owned();
    if std::fs::read_to_string(&marker).is_ok_and(|s| s.trim() == name) {
        return None;
    }
    let _ = std::fs::write(&marker, &name);
    Some(latest)
}

fn show_crash_dialog(saved: Option<&std::path::Path>) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let body = match saved {
            Some(p) => format!(
                "SpaceTree аварийно завершилась.\n\nОтчёт о сбое сохранён:\n{}\n\nПришлите этот файл разработчику.",
                p.display()
            ),
            None => "SpaceTree аварийно завершилась.".to_owned(),
        };
        // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                w(&body).as_ptr(),
                w("SpaceTree — сбой").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = saved;
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(super::timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(super::timestamp(1_760_000_000), "2025-10-09 08:53:20");
        assert_eq!(super::timestamp(951_782_400 + 86_399), "2000-02-29 23:59:59");
    }
}
