//! End-to-end tests of the OCR queue against the real bundled runtime.
//!
//! Requires `scripts/bundle-runtime-macos.sh` to have been run; tests are
//! skipped (pass trivially, with a note) if the runtime is missing.
//!
//!     cargo test --test queue -- --test-threads=1

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bulk_pdf_toolbox_lib::engine::{Engine, EventSink, QueueState};
use bulk_pdf_toolbox_lib::model::{ErrorKind, Job, JobStatus, OcrMode, Settings};
use bulk_pdf_toolbox_lib::runtime::OcrRuntime;
use serde_json::Value;

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<(String, Value)>>>);
impl EventSink for Recorder {
    fn emit(&self, event: &str, payload: Value) {
        self.0.lock().unwrap().push((event.to_string(), payload));
    }
}
impl Recorder {
    fn count(&self, event: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, _)| e == event)
            .count()
    }
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn runtime() -> Option<OcrRuntime> {
    let root = manifest().join("resources/runtime");
    let py = if cfg!(windows) { "python/python.exe" } else { "python/bin/python3" };
    if !root.join(py).is_file() {
        eprintln!("SKIP: bundled runtime missing; run scripts/bundle-runtime-macos.sh / -windows.ps1");
        return None;
    }
    Some(OcrRuntime::from_dirs(
        &root,
        &manifest().join("resources/worker"),
    ))
}

/// Generate scanned-looking PDFs with the bundled Python + Pillow.
fn make_pdfs(
    rt: &OcrRuntime,
    dir: &Path,
    count: usize,
    pages: usize,
    dpi: u32,
    prefix: &str,
) -> Vec<PathBuf> {
    let script = r#"
import sys
from PIL import Image, ImageDraw, ImageFont
out, n, pages, dpi, prefix = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
font = ImageFont.load_default(size=int(dpi/4))
w, h = int(8.5*dpi), int(11*dpi)
for i in range(n):
    imgs = []
    for p in range(pages):
        img = Image.new("L", (w, h), 255); d = ImageDraw.Draw(img)
        for line in range(10):
            d.text((dpi, dpi + line*dpi*0.8), f"{prefix} document {i} page {p+1} line {line} quick brown fox", fill=0, font=font)
        imgs.append(img)
    imgs[0].save(f"{out}/{prefix}_{i:03d}.pdf", save_all=True, append_images=imgs[1:], resolution=dpi)
"#;
    let st = rt
        .std_command(&rt.python)
        .args(["-c", script])
        .arg(dir)
        .args([
            count.to_string(),
            pages.to_string(),
            dpi.to_string(),
            prefix.to_string(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    (0..count)
        .map(|i| dir.join(format!("{prefix}_{i:03}.pdf")))
        .collect()
}

fn pdf_text(rt: &OcrRuntime, pdf: &Path) -> String {
    let out = rt
        .std_command(&rt.python)
        .args(["-c", "import sys; from pdfminer.high_level import extract_text; print(extract_text(sys.argv[1]))"])
        .arg(pdf)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

struct Harness {
    engine: Arc<Engine>,
    events: Recorder,
    state_dir: tempfile::TempDir,
    files: tempfile::TempDir,
    _rt: OcrRuntime,
}

impl Harness {
    fn new(rt: OcrRuntime, settings: Settings) -> Self {
        let state_dir = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let events = Recorder::default();
        let engine = Engine::new(
            events.clone(),
            state_dir.path().to_path_buf(),
            Ok(rt.clone()),
        );
        engine.set_settings(settings);
        engine.spawn_loops();
        Harness {
            engine,
            events,
            state_dir,
            files,
            _rt: rt,
        }
    }

    fn jobs(&self) -> Vec<Job> {
        self.engine.snapshot().jobs
    }

    fn wait(&self, what: &str, secs: u64, f: impl Fn(&[Job], QueueState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let s = self.engine.snapshot();
            if f(&s.jobs, s.meta.state) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what}: state={:?} jobs={:#?}",
                    s.meta.state,
                    s.jobs
                        .iter()
                        .map(|j| (j.id, j.status, j.message.clone()))
                        .collect::<Vec<_>>()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn count(jobs: &[Job], s: JobStatus) -> usize {
    jobs.iter().filter(|j| j.status == s).count()
}

fn partials_in(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".bpt-partial"))
        .map(|e| e.into_path())
        .collect()
}

/// Whether the worker's process tree is frozen (Some(true)), running
/// (Some(false)) or gone (None).
#[cfg(unix)]
fn tree_frozen(pid: u32) -> Option<bool> {
    tree_states(pid).0
}

/// [`tree_frozen`] plus the raw per-process states, for failure messages.
#[cfg(unix)]
fn tree_states(pid: u32) -> (Option<bool>, Vec<String>) {
    let st = group_states(pid);
    ((!st.is_empty()).then(|| st.iter().all(|s| s.starts_with('T'))), st)
}

/// Windows: every thread of the worker process is in the Suspended wait state.
#[cfg(windows)]
fn tree_frozen(pid: u32) -> Option<bool> {
    tree_states(pid).0
}

/// [`tree_frozen`] plus each thread's `ThreadState:WaitReason`, for failure
/// messages. `WaitReason` throws for threads that are not waiting, so it is
/// only read for `Wait` threads.
#[cfg(windows)]
fn tree_states(pid: u32) -> (Option<bool>, Vec<String>) {
    let script = format!(
        "$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; if ($p) {{ $p.Threads | ForEach-Object {{ if ([string]$_.ThreadState -eq 'Wait') {{ 'Wait:' + [string]$_.WaitReason }} else {{ [string]$_.ThreadState + ':-' }} }} }}"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .unwrap();
    let states: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    (
        (!states.is_empty()).then(|| states.iter().all(|r| r == "Wait:Suspended")),
        states,
    )
}

/// Polls until the worker's tree is frozen; suspension is asynchronous (a
/// thread inside a kernel critical region only stops once it leaves it).
fn assert_frozen_soon(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (frozen, states) = tree_states(pid);
        if frozen == Some(true) {
            return;
        }
        if Instant::now() > deadline {
            panic!("worker {pid} not frozen: {frozen:?} states={states:?}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Process states of every process in a process group (`T` = stopped).
#[cfg(unix)]
fn group_states(pgid: u32) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pgid=,stat="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            (it.next()?.parse::<u32>().ok()? == pgid).then(|| it.next().unwrap_or("").to_string())
        })
        .collect()
}

fn base_settings() -> Settings {
    Settings {
        parallel_files: 3,
        jobs_per_file: 1,
        output_type: "pdf".into(),
        optimize: 0,
        ..Settings::default()
    }
}

#[test]
fn bulk_happy_path() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(rt.clone(), base_settings());
    let inputs = make_pdfs(&rt, h.files.path(), 8, 2, 150, "ok");
    // Add the folder (recursive scan) plus one duplicate and one non-PDF.
    std::fs::write(h.files.path().join("notes.txt"), "x").unwrap();
    let r = h
        .engine
        .add_paths(vec![h.files.path().to_path_buf(), inputs[0].clone()]);
    assert_eq!((r.added, r.duplicates), (8, 1), "{r:?}");

    h.engine.start().unwrap();
    h.wait("all done", 180, |j, s| {
        count(j, JobStatus::Done) == 8 && s == QueueState::Idle
    });

    let jobs = h.jobs();
    for j in &jobs {
        let out = j.output.as_ref().unwrap();
        assert!(
            out.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_ocr.pdf")
        );
        assert_eq!(j.pages, Some(2));
        assert_eq!(j.progress, 1.0);
    }
    let text = pdf_text(&rt, jobs[3].output.as_ref().unwrap());
    assert!(text.contains("quick brown fox"), "OCR text missing: {text}");
    assert!(partials_in(h.files.path()).is_empty());
    assert!(
        h.events.count("engine://finished") >= 1 || {
            std::thread::sleep(Duration::from_millis(400));
            h.events.count("engine://finished") >= 1
        }
    );

    // Re-running with default conflict policy keeps both files.
    h.engine.retry(&[jobs[0].id]);
    h.engine.start().unwrap();
    h.wait("rerun", 60, |j, s| {
        j[0].status == JobStatus::Done && s == QueueState::Idle
    });
    assert!(
        h.jobs()[0]
            .output
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .ends_with("_ocr (1).pdf")
    );
}

#[test]
fn pause_suspends_process_tree_and_resume_continues() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            parallel_files: 2,
            ..base_settings()
        },
    );
    make_pdfs(&rt, h.files.path(), 3, 6, 300, "big");
    h.engine.add_paths(vec![h.files.path().to_path_buf()]);
    h.engine.start().unwrap();
    h.wait("OCR running", 60, |j, _| {
        j.iter()
            .filter(|j| j.stage.as_deref() == Some("OCR"))
            .count()
            >= 1
    });

    h.engine.pause();
    let pids = h.engine.active_pids();
    assert_eq!(pids.len(), 2, "two files in flight");
    // Paused status means proc::suspend succeeded (the worker is in its job).
    assert_eq!(
        count(&h.jobs(), JobStatus::Paused),
        2,
        "suspend failed for a worker"
    );
    for pid in &pids {
        assert_frozen_soon(*pid);
    }
    let jobs = h.jobs();
    assert_eq!(
        count(&jobs, JobStatus::Queued),
        1,
        "no new file dispatched while paused"
    );
    let before: Vec<(f64, u64)> = jobs.iter().map(|j| (j.progress, j.active_ms)).collect();
    std::thread::sleep(Duration::from_secs(3));
    let after: Vec<(f64, u64)> = h.jobs().iter().map(|j| (j.progress, j.active_ms)).collect();
    assert_eq!(
        before, after,
        "progress/active time must freeze while paused"
    );

    h.engine.start().unwrap(); // Resume
    for pid in &pids {
        assert_eq!(tree_frozen(*pid), Some(false), "worker {pid} not resumed");
    }
    h.wait("all done after resume", 300, |j, s| {
        count(j, JobStatus::Done) == 3 && s == QueueState::Idle
    });
    for j in h.jobs() {
        assert_eq!(j.pages, Some(6));
        assert!(pdf_text(&rt, j.output.as_ref().unwrap()).contains("page 6"));
    }
}

#[test]
fn stop_requeues_in_flight_and_cleans_partials() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            parallel_files: 2,
            ..base_settings()
        },
    );
    make_pdfs(&rt, h.files.path(), 4, 4, 300, "stop");
    h.engine.add_paths(vec![h.files.path().to_path_buf()]);
    h.engine.start().unwrap();
    h.wait("running", 60, |j, _| count(j, JobStatus::Running) == 2);
    // Stop while paused too: stopped processes must still die.
    h.engine.pause();
    h.engine.stop();
    h.wait("requeued", 20, |j, s| {
        count(j, JobStatus::Queued) == 4 && s == QueueState::Idle
    });
    assert!(h.engine.active_pids().is_empty());
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        partials_in(h.files.path()).is_empty(),
        "partials: {:?}",
        partials_in(h.files.path())
    );
    assert!(
        h.jobs()
            .iter()
            .all(|j| j.attempts == 0 && j.output.is_none())
    );

    h.engine.start().unwrap();
    h.wait("done", 300, |j, _| count(j, JobStatus::Done) == 4);
}

#[test]
fn cancel_running_and_queued() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            parallel_files: 1,
            ..base_settings()
        },
    );
    let inputs = make_pdfs(&rt, h.files.path(), 3, 4, 300, "cancel");
    h.engine.add_paths(inputs);
    h.engine.start().unwrap();
    h.wait("first running", 60, |j, _| {
        j[0].status == JobStatus::Running
    });
    let ids: Vec<u64> = h.jobs().iter().map(|j| j.id).collect();
    h.engine.cancel(&[ids[0], ids[1]]);
    h.wait("finished", 120, |j, s| {
        s == QueueState::Idle && j[2].status == JobStatus::Done
    });
    let jobs = h.jobs();
    assert_eq!(jobs[0].status, JobStatus::Cancelled);
    assert_eq!(jobs[1].status, JobStatus::Cancelled);
    assert!(jobs[0].output.is_none());
    assert!(!h.files.path().join("cancel_000_ocr.pdf").exists());
    assert!(partials_in(h.files.path()).is_empty());

    // Removing works for finished jobs, not active ones.
    h.engine.remove(&[ids[0]]);
    assert_eq!(h.jobs().len(), 2);
}

#[test]
fn error_states_are_classified() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            mode: OcrMode::Default,
            max_retries: 1,
            ..base_settings()
        },
    );
    let d = h.files.path();
    let good = make_pdfs(&rt, d, 1, 1, 150, "good").remove(0);

    // Encrypted PDF
    let enc = d.join("encrypted.pdf");
    let st = rt
        .std_command(&rt.python)
        .args(["-c", "import sys,pikepdf; p=pikepdf.open(sys.argv[1]); p.save(sys.argv[2], encryption=pikepdf.Encryption(owner='o', user='u'))"])
        .arg(&good)
        .arg(&enc)
        .status()
        .unwrap();
    assert!(st.success());
    // Corrupt PDF: valid header, garbage body
    let bad = d.join("corrupt.pdf");
    std::fs::write(&bad, b"%PDF-1.7\n garbage garbage \n%%EOF").unwrap();
    // Already has text (mode=default => skipped)
    let texty = d.join("has_text.pdf");
    let st = rt
        .std_command(&rt.python)
        .args(["-m", "ocrmypdf", "-q", "--output-type", "pdf"])
        .arg(&good)
        .arg(&texty)
        .status()
        .unwrap();
    assert!(st.success());
    // Input that disappears after being queued
    let vanish = make_pdfs(&rt, d, 1, 1, 100, "vanish").remove(0);

    let r = h.engine.add_paths(vec![
        enc.clone(),
        bad.clone(),
        texty.clone(),
        vanish.clone(),
        good.clone(),
    ]);
    assert_eq!(r.added, 5);
    std::fs::remove_file(&vanish).unwrap();

    h.engine.start().unwrap();
    h.wait("drained", 180, |_, s| s == QueueState::Idle);
    let by = |p: &Path| {
        h.jobs()
            .into_iter()
            .find(|j| j.input.file_name() == p.file_name())
            .unwrap()
    };

    let j = by(&enc);
    assert_eq!(
        (j.status, j.error_kind),
        (JobStatus::Failed, Some(ErrorKind::Encrypted)),
        "{j:?}"
    );
    assert_eq!(j.attempts, 1, "not retried");
    let j = by(&bad);
    assert_eq!(j.status, JobStatus::Failed, "{j:?}");
    assert!(!h.engine.job_log(j.id).is_empty());
    let j = by(&texty);
    assert_eq!(
        (j.status, j.error_kind),
        (JobStatus::Skipped, Some(ErrorKind::AlreadyHasText)),
        "{j:?}"
    );
    let j = by(&vanish);
    assert_eq!(
        (j.status, j.error_kind),
        (JobStatus::Failed, Some(ErrorKind::MissingInput))
    );
    let j = by(&good);
    assert_eq!(
        j.status,
        JobStatus::Done,
        "a bad file must not stop the rest: {j:?}"
    );
    assert!(partials_in(d).is_empty());

    // Retry-failed requeues only the failures and resets attempts.
    h.engine.retry_failed();
    let jobs = h.jobs();
    assert_eq!(count(&jobs, JobStatus::Queued), 3);
    assert!(
        jobs.iter()
            .filter(|j| j.status == JobStatus::Queued)
            .all(|j| j.attempts == 0)
    );
}

#[test]
fn missing_language_pauses_queue_with_fatal_error() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            languages: vec!["klingon".into()],
            ..base_settings()
        },
    );
    make_pdfs(&rt, h.files.path(), 3, 1, 100, "lang");
    h.engine.add_paths(vec![h.files.path().to_path_buf()]);
    h.engine.start().unwrap();
    h.wait("fatal pause", 60, |_, s| s == QueueState::Paused);
    let snap = h.engine.snapshot();
    assert!(snap.meta.fatal.is_some(), "fatal error should be reported");
    std::thread::sleep(Duration::from_millis(500));
    let jobs = h.jobs();
    assert_eq!(
        count(&jobs, JobStatus::Queued),
        3,
        "jobs kept for after the fix: {jobs:#?}"
    );
    assert!(jobs.iter().all(|j| j.attempts == 0));
}

#[test]
fn queue_survives_restart() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            parallel_files: 1,
            ..base_settings()
        },
    );
    make_pdfs(&rt, h.files.path(), 3, 3, 300, "persist");
    h.engine.add_paths(vec![h.files.path().to_path_buf()]);
    h.engine.start().unwrap();
    h.wait("one running", 60, |j, _| count(j, JobStatus::Running) == 1);
    h.engine.shutdown(); // what the app does on quit
    assert!(h.engine.active_pids().is_empty());

    let events = Recorder::default();
    let e2 = Engine::new(events, h.state_dir.path().to_path_buf(), Ok(rt.clone()));
    let s = e2.snapshot();
    assert_eq!(s.jobs.len(), 3);
    assert_eq!(
        s.meta.state,
        QueueState::Paused,
        "offers Resume, never auto-starts"
    );
    assert!(s.jobs.iter().all(|j| j.status == JobStatus::Queued));
    assert_eq!(s.meta.settings.parallel_files, 1, "settings persisted");
    assert!(partials_in(h.files.path()).is_empty());
}

#[test]
fn start_without_runtime_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::new(
        Recorder::default(),
        dir.path().to_path_buf(),
        Err("no runtime".into()),
    );
    assert_eq!(e.start(), Err("no runtime".into()));
}

#[test]
fn folder_mode_requires_output_dir() {
    let dir = tempfile::tempdir().unwrap();
    let Some(rt) = runtime() else { return };
    let e = Engine::new(Recorder::default(), dir.path().to_path_buf(), Ok(rt));
    e.set_settings(Settings {
        output_mode: bulk_pdf_toolbox_lib::model::OutputMode::Folder,
        ..Settings::default()
    });
    assert!(e.start().is_err());
}

/// Unix only: on Windows the kill-on-close job object ends the tree with the
/// app process, so there is nothing left to reap.
#[cfg(unix)]
#[test]
fn reaps_workers_left_stopped_by_a_crash() {
    let Some(rt) = runtime() else { return };
    let h = Harness::new(
        rt.clone(),
        Settings {
            parallel_files: 1,
            ..base_settings()
        },
    );
    make_pdfs(&rt, h.files.path(), 1, 4, 300, "crash");
    h.engine.add_paths(vec![h.files.path().to_path_buf()]);
    h.engine.start().unwrap();
    h.wait("running", 60, |j, _| count(j, JobStatus::Running) == 1);
    h.engine.pause();
    let pid = h.engine.active_pids()[0];
    std::thread::sleep(Duration::from_millis(2500)); // let the periodic save run
    assert!(!group_states(pid).is_empty());

    // Simulate a crash: no shutdown(), just a new engine on the same state dir.
    let e2 = Engine::new(
        Recorder::default(),
        h.state_dir.path().to_path_buf(),
        Ok(rt.clone()),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        group_states(pid).is_empty(),
        "stopped worker group should be reaped"
    );
    assert!(partials_in(h.files.path()).is_empty());
    let s = e2.snapshot();
    assert_eq!(s.meta.state, QueueState::Paused);
    assert_eq!(s.jobs[0].status, JobStatus::Queued);
    // Old engine's supervisor sees its child die; don't let it confuse the test.
    h.engine.stop();
}
