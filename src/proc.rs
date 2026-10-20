//! Child processes and process facts. Children never flash a console:
//! `CREATE_NO_WINDOW`, args as an array, no shell.

use crate::model::{Shared, lock};
use std::sync::mpsc;
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

/// Kills a child's whole process tree when dropped, and when we exit however
/// we exit (the OS closes the handle). Each child gets its own kill-on-close
/// Job Object, so a `.cmd` shim's node/native descendants die with it.
///
/// Known gap: the child runs briefly before it is assigned, so a descendant
/// it spawns in that instant is not in the job. The CLIs we start don't.
pub struct JobGuard(isize);

impl Drop for JobGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        // SAFETY: we own this handle; closing it ends every process in the job.
        unsafe {
            let _ = CloseHandle(HANDLE(self.0 as *mut _));
        }
    }
}

/// Puts `child` (and anything it starts later) in a new kill-on-close job.
/// `None` if Windows refused; the child then only gets a plain `kill`.
pub fn adopt(child: &Child) -> Option<JobGuard> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    // SAFETY: creating and configuring a job we own; the info struct is a
    // plain value of the documented size; the child handle is valid here.
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        )
        .is_ok();
        if !configured || AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
            let _ = CloseHandle(job);
            return None;
        }
        Some(JobGuard(job.0 as isize))
    }
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
    // Dropped on return: also ends any descendant still holding our pipes.
    let _job = adopt(&child);
    let pid = child.id();
    lock(state).own_pids.insert(pid);
    let result = wait_capture(&mut child, timeout);
    lock(state).own_pids.remove(&pid);
    result
}

fn wait_capture(child: &mut Child, timeout: Duration) -> io::Result<Captured> {
    // Read both pipes on threads so a chatty child can't block on a full pipe.
    let out_rx = drain(child.stdout.take().expect("piped"));
    let err_rx = drain(child.stderr.take().expect("piped"));
    let deadline = Instant::now() + timeout;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "child timed out"));
        }
        thread::sleep(Duration::from_millis(50));
    }
    // A grandchild that inherited the pipe can keep it open after the child
    // exits; don't wait on it for more than a moment.
    let grace = deadline.max(Instant::now() + Duration::from_secs(2));
    let take = |rx: mpsc::Receiver<Vec<u8>>| {
        let left = grace.saturating_duration_since(Instant::now());
        String::from_utf8_lossy(&rx.recv_timeout(left).unwrap_or_default()).into_owned()
    };
    let stdout = take(out_rx);
    let stderr = take(err_rx);
    Ok(Captured { stdout, stderr })
}

/// Most of a child's output we keep per pipe; `/usage` prints a few KB.
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;

/// Reads a pipe to the end on its own thread; the result arrives on the
/// channel. Keeps the first `MAX_CAPTURE_BYTES` and discards the rest, so a
/// runaway child can't grow our memory (its timeout still ends it).
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut pipe).take(MAX_CAPTURE_BYTES).read_to_end(&mut buf);
        let _ = io::copy(&mut pipe, &mut io::sink());
        let _ = tx.send(buf);
    });
    rx
}

/// A whole text file, or an error when it is larger than `max` bytes: a huge
/// or corrupt file must not exhaust memory.
pub fn read_text_capped(path: &Path, max: u64) -> io::Result<String> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
    }
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// First match for `name` on `PATH`: directories in PATH order, and within
/// each `.exe` then `.cmd` (an exact filename with an extension is tried as
/// is). Empty and relative entries are skipped: they resolve against the
/// current directory, which may be Downloads or wherever we were started.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    find_in(&env::var_os("PATH")?, name)
}

fn find_in(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if Path::new(name).extension().is_some() { &[""] } else { &[".exe", ".cmd"] };
    for dir in env::split_paths(path).filter(|d| d.is_absolute()) {
        for ext in exts {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("majihanaya-proc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn joined(dirs: &[&Path]) -> OsString {
        env::join_paths(dirs).unwrap()
    }

    #[test]
    fn path_order_beats_extension_order() {
        let (first, later) = (temp_dir("first"), temp_dir("later"));
        fs::write(first.join("tool.cmd"), "").unwrap();
        fs::write(later.join("tool.exe"), "").unwrap();
        assert_eq!(find_in(&joined(&[&first, &later]), "tool"), Some(first.join("tool.cmd")));
        // Within one directory, .exe still wins.
        fs::write(first.join("tool.exe"), "").unwrap();
        assert_eq!(find_in(&joined(&[&first, &later]), "tool"), Some(first.join("tool.exe")));
        let _ = (fs::remove_dir_all(first), fs::remove_dir_all(later));
    }

    #[test]
    fn empty_and_relative_entries_are_skipped() {
        let real = temp_dir("real");
        fs::write(real.join("tool.cmd"), "").unwrap();
        // A planted tool.exe in the current directory must not be found
        // through "", "." or a relative entry.
        let cwd = env::current_dir().unwrap();
        let planted = cwd.join("majihanaya-planted.exe");
        fs::write(&planted, "").unwrap();
        let path = OsString::from(format!(";.;sub;{}", real.display()));
        assert_eq!(find_in(&path, "majihanaya-planted"), None);
        assert_eq!(find_in(&path, "tool"), Some(real.join("tool.cmd")));
        let _ = (fs::remove_file(planted), fs::remove_dir_all(real));
    }

    #[test]
    fn capped_read_rejects_large_files() {
        let d = temp_dir("cap");
        let f = d.join("x.json");
        fs::write(&f, "12345").unwrap();
        assert_eq!(read_text_capped(&f, 5).unwrap(), "12345");
        assert_eq!(read_text_capped(&f, 4).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(d);
    }
}
