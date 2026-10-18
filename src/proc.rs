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

/// Ties a child to our lifetime: every child joins one Job Object with
/// kill-on-close, so Windows ends it when we exit, however we exit
/// (worker threads never get to run their `Drop`s).
pub fn adopt(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    // The handle is stored as an integer so the static is Sync; it stays open
    // until the process exits, which is what closes the job.
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    let job = JOB.get_or_init(|| {
        // SAFETY: creating and configuring a job we own; the info struct is a
        // plain out-of-band value of the documented size.
        unsafe {
            let job = CreateJobObjectW(None, None).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
            .ok()?;
            Some(job.0 as isize)
        }
    });
    if let Some(job) = job {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe {
            let _ = AssignProcessToJobObject(HANDLE(*job as *mut _), HANDLE(child.as_raw_handle()));
        }
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
    adopt(&child);
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

/// Reads a pipe to the end on its own thread; the result arrives on the channel.
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
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
