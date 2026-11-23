//! Process-tree control for worker processes.
//!
//! Every worker can spawn tesseract / pngquant children (and ocrmypdf may use
//! a process pool), so pause, resume and kill must reach the whole tree.
//!
//! * **Unix**: each worker is the leader of its own process group, so one
//!   signal reaches every descendant. Pause is a real SIGSTOP of the group.
//! * **Windows**: each worker is started suspended, put into its own Job
//!   Object (with kill-on-close, so a crash of the app takes the tree down
//!   with it), then resumed. Pause suspends every process in the job with
//!   `NtSuspendProcess`; kill is `TerminateJobObject`.
//!
//! The API is keyed by the worker's pid: [`configure`] before spawning,
//! [`attach`] right after, [`release`] once the worker has exited.

pub fn configure(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(win::CREATE_NO_WINDOW | win::CREATE_SUSPENDED);
    }
}

/// Finish setting up a freshly spawned worker. On Windows this assigns it to
/// a job and lets it start running; it must be called exactly once.
pub fn attach(child: &tokio::process::Child) {
    #[cfg(windows)]
    win::attach(child);
    #[cfg(not(windows))]
    let _ = child;
}

/// Forget a worker that has exited (and kill anything it left behind).
pub fn release(pid: u32) {
    #[cfg(windows)]
    win::release(pid);
    #[cfg(not(windows))]
    let _ = pid;
}

/// Whether [`suspend`] really freezes running work on this platform.
pub const SUPPORTS_SUSPEND: bool = cfg!(any(unix, windows));

#[cfg(unix)]
fn signal_group(pid: u32, sig: libc::c_int) -> bool {
    // Negative pid => whole process group.
    unsafe { libc::kill(-(pid as libc::pid_t), sig) == 0 }
}

pub fn suspend(pid: u32) -> bool {
    #[cfg(unix)]
    return signal_group(pid, libc::SIGSTOP);
    #[cfg(windows)]
    return win::suspend(pid);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

pub fn resume(pid: u32) -> bool {
    #[cfg(unix)]
    return signal_group(pid, libc::SIGCONT);
    #[cfg(windows)]
    return win::resume(pid);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        // Continue first so a stopped group acts on the kill promptly.
        signal_group(pid, libc::SIGCONT);
        signal_group(pid, libc::SIGKILL);
    }
    #[cfg(windows)]
    win::kill_tree(pid);
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
}

/// Kill a worker group left over from a previous app run, but only if the
/// group leader is still one of our workers (pids get reused).
///
/// Only needed on Unix: on Windows the kill-on-close job already took the
/// whole tree down when the previous app process died.
pub fn reap_stale_worker(pgid: u32) {
    #[cfg(unix)]
    {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &pgid.to_string()])
            .output()
        else {
            return;
        };
        if String::from_utf8_lossy(&out.stdout).contains("bpt_ocr_worker.py") {
            kill_tree(pgid);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Keeps the computer from idle-sleeping while OCR runs.
#[derive(Default)]
pub struct KeepAwake {
    #[cfg(target_os = "macos")]
    child: Option<std::process::Child>,
    // SetThreadExecutionState is per thread, so a dedicated thread holds the
    // request; dropping the sender ends it.
    #[cfg(windows)]
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl KeepAwake {
    pub fn set(&mut self, on: bool) {
        #[cfg(target_os = "macos")]
        {
            if on && self.child.is_none() {
                self.child = std::process::Command::new("/usr/bin/caffeinate")
                    .args(["-i", "-w", &std::process::id().to_string()])
                    .spawn()
                    .ok();
            } else if !on && let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        #[cfg(windows)]
        {
            if on && self.stop.is_none() {
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let spawned = std::thread::Builder::new()
                    .name("keep-awake".into())
                    .spawn(move || {
                        win::keep_awake(true);
                        let _ = rx.recv();
                        win::keep_awake(false);
                    });
                if spawned.is_ok() {
                    self.stop = Some(tx);
                }
            } else if !on {
                self.stop = None;
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = on;
    }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::os::windows::io::RawHandle;
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SUSPEND_RESUME};

    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: HANDLE) -> i32;
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    struct Worker {
        /// Job handle, stored as an integer so the map is Send.
        job: usize,
        /// Pids we suspended, so resume undoes exactly what pause did.
        suspended: Vec<u32>,
    }

    static WORKERS: Mutex<Option<HashMap<u32, Worker>>> = Mutex::new(None);

    fn with<R>(f: impl FnOnce(&mut HashMap<u32, Worker>) -> R) -> R {
        let mut g = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
        f(g.get_or_insert_with(HashMap::new))
    }

    fn raw_handle(child: &tokio::process::Child) -> Option<RawHandle> {
        child.raw_handle()
    }

    pub fn attach(child: &tokio::process::Child) {
        let (Some(pid), Some(handle)) = (child.id(), raw_handle(child)) else {
            return;
        };
        let process = handle as HANDLE;
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if !job.is_null() {
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok_limits = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0;
                if ok_limits && AssignProcessToJobObject(job, process) != 0 {
                    with(|m| {
                        m.insert(
                            pid,
                            Worker {
                                job: job as usize,
                                suspended: Vec::new(),
                            },
                        )
                    });
                } else {
                    CloseHandle(job);
                }
            }
            // Started with CREATE_SUSPENDED: let it run now that it is in the
            // job, so none of its children can escape it.
            NtResumeProcess(process);
        }
    }

    fn job_pids(job: HANDLE) -> Vec<u32> {
        #[repr(C)]
        struct List {
            assigned: u32,
            in_list: u32,
            ids: [usize; 512],
        }
        let mut list: List = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                &mut list as *mut _ as *mut _,
                std::mem::size_of::<List>() as u32,
                std::ptr::null_mut(),
            )
        } != 0;
        if !ok {
            return Vec::new();
        }
        let n = (list.in_list as usize).min(list.ids.len());
        list.ids[..n].iter().map(|&p| p as u32).collect()
    }

    fn with_process(pid: u32, f: impl FnOnce(HANDLE) -> i32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
            if h.is_null() {
                return false;
            }
            let status = f(h);
            CloseHandle(h);
            status >= 0
        }
    }

    pub fn suspend(pid: u32) -> bool {
        with(|m| {
            let Some(w) = m.get_mut(&pid) else {
                return false;
            };
            // Two passes: a child being created during the first pass shows
            // up in the second, and nothing new can start once all are frozen.
            for _ in 0..2 {
                for p in job_pids(w.job as HANDLE) {
                    if !w.suspended.contains(&p)
                        && with_process(p, |h| unsafe { NtSuspendProcess(h) })
                    {
                        w.suspended.push(p);
                    }
                }
            }
            !w.suspended.is_empty()
        })
    }

    pub fn resume(pid: u32) -> bool {
        with(|m| {
            let Some(w) = m.get_mut(&pid) else {
                return false;
            };
            for p in w.suspended.drain(..) {
                with_process(p, |h| unsafe { NtResumeProcess(h) });
            }
            true
        })
    }

    pub fn kill_tree(pid: u32) {
        let job = with(|m| m.get(&pid).map(|w| w.job));
        match job {
            // Terminating a job also kills suspended members immediately.
            Some(job) => unsafe {
                TerminateJobObject(job as HANDLE, 1);
            },
            None => {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .status();
            }
        }
    }

    pub fn release(pid: u32) {
        if let Some(w) = with(|m| m.remove(&pid)) {
            unsafe {
                TerminateJobObject(w.job as HANDLE, 1);
                CloseHandle(w.job as HANDLE);
            }
        }
    }

    pub fn keep_awake(on: bool) {
        unsafe {
            SetThreadExecutionState(if on {
                ES_CONTINUOUS | ES_SYSTEM_REQUIRED
            } else {
                ES_CONTINUOUS
            });
        }
    }
}
