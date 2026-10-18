//! Child processes and process facts. Children never flash a console:
//! `CREATE_NO_WINDOW`, args as an array, no shell.

use crate::model::Shared;
use std::env;
use std::io::{self, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A `Command` with the flags every child must have.
pub fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// Output of a finished child. Callers judge success by what they parse.
pub struct Captured {
    pub stdout: String,
    pub stderr: String,
}

/// Runs to completion, killing the child after `timeout`. The child's PID is
/// held in `own_pids` while it runs so `sessions` skips it.
pub fn run_capture(state: &Shared, program: &Path, args: &[&str], timeout: Duration) -> io::Result<Captured> {
    let mut child = command(program).args(args).spawn()?;
    let pid = child.id();
    state.lock().unwrap().own_pids.insert(pid);
    let result = wait_capture(&mut child, timeout);
    state.lock().unwrap().own_pids.remove(&pid);
    result
}

fn wait_capture(child: &mut Child, timeout: Duration) -> io::Result<Captured> {
    // Read both pipes on threads so a chatty child can't block on a full pipe.
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_t = thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let err_t = thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "child timed out"));
        }
        thread::sleep(Duration::from_millis(50));
    }
    let stdout = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    Ok(Captured { stdout, stderr })
}

/// First match for `name` on `PATH`, trying `.exe` then `.cmd` (an exact
/// filename with an extension is tried as is).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let exts: &[&str] = if Path::new(name).extension().is_some() { &[""] } else { &[".exe", ".cmd"] };
    for ext in exts {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Process creation time as a FILETIME (100 ns ticks since 1601, UTC), or
/// `None` if the process is gone or not ours to query.
pub fn process_start_filetime(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: the handle is checked and closed; FILETIMEs are plain out-params.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }
}
