//! Small bounded log files in `%APPDATA%\majihanaya`. Each file is at most
//! `max_bytes`; when full it rotates to `.1`, `.2`, … keeping `keep` old
//! files, so a log never takes more than `(keep + 1) × max_bytes`. Write
//! errors are ignored: logs are a convenience, never a reason to fail.

use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct LogFile {
    name: &'static str,
    max_bytes: u64,
    keep: usize,
    /// Serialises writers (workers, the UI thread, the panic hook).
    lock: Mutex<()>,
}

/// Errors, panics and start-ups: `majihanaya.log`, 128 KB × 3 files.
pub static ERRORS: LogFile = LogFile::new("majihanaya.log", 128 * 1024, 2);

impl LogFile {
    pub const fn new(name: &'static str, max_bytes: u64, keep: usize) -> Self {
        Self { name, max_bytes, keep, lock: Mutex::new(()) }
    }

    fn path(&self) -> Option<PathBuf> {
        crate::config::path().and_then(|p| p.parent().map(|d| d.join(self.name)))
    }

    /// Appends `msg` as one timestamped line.
    pub fn line(&self, msg: &str) {
        let text = format!("{}  {}\n", Local::now().format("%Y-%m-%d %H:%M:%S %:z"), msg.replace(['\r', '\n'], " "));
        if let Some(path) = self.path() {
            self.append_at(&path, &text);
        }
    }

    pub(crate) fn append_at(&self, path: &Path, text: &str) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if fs::metadata(path).is_ok_and(|m| m.len() + text.len() as u64 > self.max_bytes) {
            self.rotate(path);
        }
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// Shifts `log` → `log.1` → … → `log.keep` (the oldest is dropped).
    fn rotate(&self, path: &Path) {
        let numbered = |n: usize| PathBuf::from(format!("{}.{n}", path.display()));
        let _ = fs::remove_file(numbered(self.keep));
        for n in (1..self.keep).rev() {
            let _ = fs::rename(numbered(n), numbered(n + 1));
        }
        let _ = fs::rename(path, numbered(1));
    }
}

/// Logs a source's error when it changes, and when it clears ("recovered"),
/// so a source failing every minute writes one line, not one a minute.
#[derive(Default)]
pub struct ErrorWatch(std::collections::HashMap<&'static str, String>);

impl ErrorWatch {
    pub fn check(&mut self, source: &'static str, error: Option<&str>) {
        match (self.0.get(source), error) {
            (Some(prev), Some(e)) if prev == e => {}
            (_, Some(e)) => {
                ERRORS.line(&format!("{source}: {e}"));
                self.0.insert(source, e.to_owned());
            }
            (Some(_), None) => {
                ERRORS.line(&format!("{source}: recovered"));
                self.0.remove(source);
            }
            (None, None) => {}
        }
    }
}

/// Logs panics from any thread (workers catch and survive them; the log is
/// how they're noticed), then runs the default hook.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        ERRORS.line(&format!("panic in {}: {info}", thread.name().unwrap_or("?")));
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_and_file_count_are_capped() {
        let d = std::env::temp_dir().join(format!("majihanaya-logfile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let log = LogFile::new("t.log", 4096, 2);
        let p = d.join("t.log");
        for i in 0..2000 {
            log.append_at(&p, &format!("line {i:05} with some padding text\n"));
        }
        let files: Vec<_> = fs::read_dir(&d).unwrap().flatten().collect();
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|f| f.metadata().unwrap().len() <= 4096));
        // The newest line is in the live file.
        assert!(fs::read_to_string(&p).unwrap().contains("line 01999"));
        let _ = fs::remove_dir_all(d);
    }
}
