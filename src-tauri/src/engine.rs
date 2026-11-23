//! The bulk OCR job queue.
//!
//! State machine:
//! ```text
//!   Idle --start--> Running --pause--> Paused --resume--> Running
//!     ^                |  \                |
//!     +------stop------+   +-(drained)-> Idle
//! ```
//! Jobs: Queued -> Running <-> Paused -> Done | Skipped | Failed | Cancelled.
//! Transient failures are retried automatically up to `max_retries`.
//!
//! All mutable state lives in one `Mutex<Inner>`; locks are held only for
//! short synchronous sections (never across `.await`). UI updates are batched
//! and flushed every 150 ms so hundreds of files don't flood the webview.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::Notify;

use crate::model::{ErrorKind, Job, JobStatus, Settings};
use crate::output::{self, OutputPlan, PlanError};
use crate::proc::{self, KeepAwake};
use crate::runtime::OcrRuntime;

const LOG_CAP: usize = 400;
const FLUSH_EVERY: Duration = Duration::from_millis(150);
const SAVE_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    Idle,
    Running,
    Paused,
}

#[derive(Debug, Clone, Copy)]
enum KillReason {
    /// Stop/quit: put the file back in the queue.
    Requeue,
    Cancel,
    Timeout,
}

struct Proc {
    pid: u32,
    plan: OutputPlan,
    kill: Option<KillReason>,
    /// `job.active_ms` at the last sign of life from the worker.
    last_activity_ms: u64,
    suspended: bool,
    done: Option<(i32, String)>,
    last_errors: VecDeque<String>,
}

struct Inner {
    jobs: IndexMap<u64, Job>,
    logs: HashMap<u64, VecDeque<String>>,
    procs: HashMap<u64, Proc>,
    state: QueueState,
    settings: Settings,
    next_id: u64,
    fatal: Option<String>,
    changed: HashSet<u64>,
    removed: Vec<u64>,
    meta_changed: bool,
    finished_pending: bool,
    dirty: bool,
    keep_awake: KeepAwake,
    /// Set on app exit: never launch another worker after this.
    shutting_down: bool,
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    version: u32,
    settings: Settings,
    jobs: Vec<Job>,
    next_id: u64,
    state: QueueState,
    /// Workers alive at save time (pgid, partial output). If the app dies
    /// while they are SIGSTOPped their own watchdog can't run, so the next
    /// launch reaps them.
    #[serde(default)]
    in_flight: Vec<(u32, PathBuf)>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub state: QueueState,
    pub fatal: Option<String>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(flatten)]
    pub meta: Meta,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobsEvent {
    upserts: Vec<Job>,
    removed: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub added: usize,
    pub duplicates: usize,
    pub ignored: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FinishedEvent {
    done: usize,
    failed: usize,
    skipped: usize,
}

/// Where engine events go. The app forwards them to the webview; tests
/// record them.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: Value);
    /// Bounce the dock icon / flash the taskbar when the queue finishes.
    fn attention(&self) {}
}

pub struct Engine {
    inner: Mutex<Inner>,
    notify: Notify,
    sink: Box<dyn EventSink>,
    runtime: RwLock<Result<OcrRuntime, String>>,
    state_file: PathBuf,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn push_capped(q: &mut VecDeque<String>, line: String, cap: usize) {
    if q.len() >= cap {
        q.pop_front();
    }
    q.push_back(line);
}

/// Map ocrmypdf stage progress onto one overall 0..1 bar.
fn stage_span(desc: &str) -> (f64, f64) {
    match desc {
        "Scanning contents" => (0.0, 0.08),
        "OCR" | "Image processing" | "hOCR" => (0.08, 0.85),
        "Grafting hOCR to PDF" => (0.85, 0.9),
        _ => (0.9, 0.99),
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        && !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        && !output::is_partial_file(path)
}

fn has_pdf_magic(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 1024];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let n = f.read(&mut buf).unwrap_or(0);
    // The header may be preceded by junk; PDF readers accept it within 1 KB.
    buf[..n].windows(5).any(|w| w == b"%PDF-")
}

impl Engine {
    pub fn new(
        sink: impl EventSink,
        dir: PathBuf,
        runtime: Result<OcrRuntime, String>,
    ) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&dir);
        let state_file = dir.join("ocr-queue.json");

        let mut inner = Inner {
            jobs: IndexMap::new(),
            logs: HashMap::new(),
            procs: HashMap::new(),
            state: QueueState::Idle,
            settings: Settings::default(),
            next_id: 1,
            fatal: None,
            changed: HashSet::new(),
            removed: Vec::new(),
            meta_changed: true,
            finished_pending: false,
            dirty: false,
            keep_awake: KeepAwake::default(),
            shutting_down: false,
        };

        if let Some(p) = std::fs::read(&state_file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Persisted>(&b).ok())
        {
            for (pgid, partial) in &p.in_flight {
                proc::reap_stale_worker(*pgid);
                let _ = std::fs::remove_file(partial);
                let _ = std::fs::remove_file(partial.with_extension("txt"));
            }
            inner.settings = p.settings.sanitized();
            inner.next_id = p.next_id.max(1);
            let mut pending = false;
            for mut j in p.jobs {
                if j.status.is_active() {
                    // Interrupted by quit/crash: run it again from scratch.
                    j.reset_to_queued();
                    j.message = Some("Interrupted — will restart".into());
                }
                pending |= j.status == JobStatus::Queued;
                inner.next_id = inner.next_id.max(j.id + 1);
                inner.jobs.insert(j.id, j);
            }
            // Never auto-start on launch; offer Resume instead.
            if pending && p.state != QueueState::Idle {
                inner.state = QueueState::Paused;
            }
        }

        Arc::new(Engine {
            inner: Mutex::new(inner),
            notify: Notify::new(),
            sink: Box::new(sink),
            runtime: RwLock::new(runtime),
            state_file,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn runtime(&self) -> Result<OcrRuntime, String> {
        self.runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn spawn_loops(self: &Arc<Self>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut last_tick = Instant::now();
            let mut last_save = Instant::now();
            loop {
                me.dispatch();
                tokio::select! {
                    _ = me.notify.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
                let now = Instant::now();
                if now - last_tick >= Duration::from_millis(900) {
                    me.tick(now - last_tick);
                    last_tick = now;
                }
                if now - last_save >= SAVE_EVERY {
                    me.save_if_dirty();
                    last_save = now;
                }
            }
        });
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(FLUSH_EVERY).await;
                me.flush();
            }
        });
    }

    // ------------------------------------------------------------ public API

    pub fn snapshot(&self) -> Snapshot {
        let g = self.lock();
        Snapshot {
            meta: meta_of(&g),
            jobs: g.jobs.values().cloned().collect(),
        }
    }

    pub fn job_log(&self, id: u64) -> Vec<String> {
        self.lock()
            .logs
            .get(&id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Process-group ids of in-flight workers.
    pub fn active_pids(&self) -> Vec<u32> {
        self.lock().procs.values().map(|p| p.pid).collect()
    }

    pub fn is_busy(&self) -> bool {
        !self.lock().procs.is_empty()
    }

    pub fn set_settings(&self, s: Settings) {
        let mut g = self.lock();
        g.settings = s.sanitized();
        // No meta echo: the UI already holds these values and an echo would
        // fight with in-progress typing (e.g. a briefly empty suffix field).
        g.dirty = true;
        drop(g);
        // Parallelism may have increased.
        self.notify.notify_one();
    }

    /// Scan files/folders (recursively) and enqueue every PDF not already queued.
    pub fn add_paths(&self, paths: Vec<PathBuf>) -> AddResult {
        let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut ignored = 0;
        for p in paths {
            if p.is_dir() {
                let base = p
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| p.clone());
                for e in walkdir::WalkDir::new(&p)
                    .follow_links(true)
                    .into_iter()
                    .filter_entry(|e| {
                        e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.')
                    })
                {
                    let Ok(e) = e else { continue };
                    if e.file_type().is_file() && is_pdf(e.path()) {
                        found.push((e.into_path(), base.clone()));
                    }
                }
            } else if p.is_file() {
                if is_pdf(&p) {
                    let base = p.parent().map(Path::to_path_buf).unwrap_or_default();
                    found.push((p, base));
                } else {
                    ignored += 1;
                }
            }
        }

        // Validate outside the lock: this touches disk for every file.
        let candidates: Vec<(PathBuf, PathBuf, u64)> = found
            .into_iter()
            .filter_map(|(path, base)| {
                let path = dunce::canonicalize(&path).unwrap_or(path);
                let base = dunce::canonicalize(&base).unwrap_or(base);
                if !has_pdf_magic(&path) {
                    ignored += 1;
                    return None;
                }
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Some((path, base, size))
            })
            .collect();

        let mut g = self.lock();
        let mut existing: HashSet<PathBuf> = g.jobs.values().map(|j| j.input.clone()).collect();
        let (mut added, mut duplicates) = (0, 0);
        for (path, base, size) in candidates {
            if !existing.insert(path.clone()) {
                duplicates += 1;
                continue;
            }
            let id = g.next_id;
            g.next_id += 1;
            g.jobs.insert(id, Job::new(id, path, base, size));
            g.changed.insert(id);
            added += 1;
        }
        if added > 0 {
            g.dirty = true;
        }
        drop(g);
        self.notify.notify_one();
        AddResult {
            added,
            duplicates,
            ignored,
        }
    }

    pub fn start(&self) -> Result<(), String> {
        self.runtime()?;
        let mut g = self.lock();
        g.settings.validate_for_start()?;
        g.fatal = None;
        match g.state {
            QueueState::Running => {}
            QueueState::Paused => resume_locked(&mut g),
            QueueState::Idle => g.state = QueueState::Running,
        }
        g.meta_changed = true;
        g.dirty = true;
        drop(g);
        self.notify.notify_one();
        Ok(())
    }

    pub fn pause(&self) {
        let mut g = self.lock();
        if g.state != QueueState::Running {
            return;
        }
        g.state = QueueState::Paused;
        let ids: Vec<u64> = g.procs.keys().copied().collect();
        for id in ids {
            let p = g.procs.get_mut(&id).unwrap();
            if !p.suspended && proc::suspend(p.pid) {
                p.suspended = true;
                if let Some(j) = g.jobs.get_mut(&id) {
                    j.status = JobStatus::Paused;
                }
                g.changed.insert(id);
            }
        }
        g.keep_awake.set(false);
        g.meta_changed = true;
        g.dirty = true;
    }

    /// Halt everything; in-flight files go back to the queue.
    pub fn stop(&self) {
        let mut g = self.lock();
        g.state = QueueState::Idle;
        for p in g.procs.values_mut() {
            p.kill = Some(KillReason::Requeue);
            proc::kill_tree(p.pid);
        }
        g.keep_awake.set(false);
        g.meta_changed = true;
        g.dirty = true;
    }

    pub fn cancel(&self, ids: &[u64]) {
        let mut g = self.lock();
        for id in ids {
            if let Some(p) = g.procs.get_mut(id) {
                p.kill = Some(KillReason::Cancel);
                proc::kill_tree(p.pid);
            } else if let Some(j) = g.jobs.get_mut(id)
                && j.status == JobStatus::Queued
            {
                j.status = JobStatus::Cancelled;
                j.message = Some("Cancelled".into());
                g.changed.insert(*id);
            }
        }
        g.dirty = true;
    }

    /// Re-queue finished jobs (failed, cancelled, skipped, or done).
    pub fn retry(&self, ids: &[u64]) {
        let mut g = self.lock();
        for id in ids {
            if let Some(j) = g.jobs.get_mut(id)
                && j.status.is_finished()
            {
                j.reset_to_queued();
                j.attempts = 0;
                j.message = None;
                j.error_kind = None;
                j.output = None;
                j.output_size = None;
                g.changed.insert(*id);
            }
        }
        g.dirty = true;
        drop(g);
        self.notify.notify_one();
    }

    pub fn retry_failed(&self) {
        let ids: Vec<u64> = self
            .lock()
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Failed)
            .map(|j| j.id)
            .collect();
        self.retry(&ids);
    }

    /// Remove jobs from the list. Active jobs are left alone (cancel first).
    pub fn remove(&self, ids: &[u64]) {
        let mut g = self.lock();
        for id in ids {
            if g.jobs.get(id).is_some_and(|j| !j.status.is_active()) {
                g.jobs.shift_remove(id);
                g.logs.remove(id);
                g.changed.remove(id);
                g.removed.push(*id);
            }
        }
        g.dirty = true;
    }

    pub fn remove_with_status(&self, statuses: &[JobStatus]) {
        let ids: Vec<u64> = self
            .lock()
            .jobs
            .values()
            .filter(|j| statuses.contains(&j.status))
            .map(|j| j.id)
            .collect();
        self.remove(&ids);
    }

    /// Move queued jobs to the front so they are processed next.
    pub fn prioritize(&self, ids: &[u64]) {
        let mut g = self.lock();
        let mut target = 0;
        for id in ids {
            if let Some(idx) = g.jobs.get_index_of(id)
                && g.jobs[idx].status == JobStatus::Queued
            {
                g.jobs.move_index(idx, target);
                target += 1;
            }
        }
        g.dirty = true;
        // Order changed: resend everything so the UI re-sorts.
        let all: Vec<u64> = g.jobs.keys().copied().collect();
        g.changed.extend(all);
    }

    /// Kill workers, requeue in-flight files and persist. Called on app exit.
    pub fn shutdown(&self) {
        let mut g = self.lock();
        g.shutting_down = true;
        let ids: Vec<u64> = g.procs.keys().copied().collect();
        for id in ids {
            let p = g.procs.remove(&id).unwrap();
            proc::kill_tree(p.pid);
            p.plan.discard();
            if let Some(j) = g.jobs.get_mut(&id) {
                j.reset_to_queued();
                j.attempts = j.attempts.saturating_sub(1);
            }
        }
        g.keep_awake.set(false);
        let data = persist_json(&g);
        drop(g);
        self.write_state(data);
    }

    // ------------------------------------------------------------ scheduling

    fn dispatch(self: &Arc<Self>) {
        let rt = self.runtime();
        let mut g = self.lock();
        if g.state != QueueState::Running || g.shutting_down {
            return;
        }
        let rt = match rt {
            Ok(rt) => rt,
            Err(e) => {
                g.fatal = Some(e);
                g.state = QueueState::Paused;
                g.meta_changed = true;
                return;
            }
        };
        while (g.procs.len() as u32) < g.settings.parallel_files {
            let Some(id) = g
                .jobs
                .values()
                .find(|j| j.status == JobStatus::Queued)
                .map(|j| j.id)
            else {
                break;
            };
            self.launch(&mut g, &rt, id);
            if g.state != QueueState::Running {
                break;
            }
        }
        if g.procs.is_empty()
            && g.state == QueueState::Running
            && !g.jobs.values().any(|j| j.status == JobStatus::Queued)
        {
            g.state = QueueState::Idle;
            g.meta_changed = true;
            g.finished_pending = true;
            g.dirty = true;
        }
        let awake = g.state == QueueState::Running && !g.procs.is_empty();
        g.keep_awake.set(awake);
    }

    fn launch(self: &Arc<Self>, g: &mut Inner, rt: &OcrRuntime, id: u64) {
        let job = g.jobs[&id].clone();
        g.changed.insert(id);

        let finish = |g: &mut Inner, status: JobStatus, kind: Option<ErrorKind>, msg: String| {
            let j = g.jobs.get_mut(&id).unwrap();
            j.status = status;
            j.error_kind = kind;
            j.message = Some(msg);
            j.finished_at = Some(now_ms());
            g.dirty = true;
        };

        if !job.input.is_file() {
            return finish(
                g,
                JobStatus::Failed,
                Some(ErrorKind::MissingInput),
                "Input file no longer exists".into(),
            );
        }
        let taken: HashSet<PathBuf> = g
            .procs
            .values()
            .map(|p| p.plan.final_path.clone())
            .collect();
        let plan = match output::plan(&job, &g.settings, &taken) {
            Ok(p) => p,
            Err(PlanError::Exists(p)) => {
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                return finish(
                    g,
                    JobStatus::Skipped,
                    Some(ErrorKind::OutputExists),
                    format!("Output already exists: {name}"),
                );
            }
            Err(PlanError::Other(m)) => {
                return finish(g, JobStatus::Failed, Some(ErrorKind::OutputError), m);
            }
        };
        if let Err(e) = plan.prepare() {
            return finish(
                g,
                JobStatus::Failed,
                Some(ErrorKind::OutputError),
                format!("Cannot write output folder: {e}"),
            );
        }

        let spec = json!({
            "input": job.input,
            "output": plan.partial_path,
            "options": g.settings.ocrmypdf_options(plan.sidecar_partial.as_deref()),
        });
        let mut cmd = rt.command(&rt.python);
        cmd.arg(&rt.worker)
            .arg(spec.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        proc::configure(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                plan.discard();
                return finish(
                    g,
                    JobStatus::Failed,
                    Some(ErrorKind::Crashed),
                    format!("Could not start OCR worker: {e}"),
                );
            }
        };
        proc::attach(&child);
        let pid = child.id().unwrap_or(0);

        let j = g.jobs.get_mut(&id).unwrap();
        j.reset_to_queued();
        j.status = JobStatus::Running;
        j.attempts += 1;
        j.started_at = Some(now_ms());
        j.stage = Some("Starting".into());
        j.message = None;
        j.error_kind = None;
        j.output = None;
        j.output_size = None;
        let attempt = j.attempts;

        let log = g.logs.entry(id).or_default();
        push_capped(
            log,
            format!("── attempt {attempt}: {}", job.input.display()),
            LOG_CAP,
        );
        push_capped(
            log,
            format!("── output: {}", plan.final_path.display()),
            LOG_CAP,
        );

        g.dirty = true;
        g.procs.insert(
            id,
            Proc {
                pid,
                plan,
                kill: None,
                last_activity_ms: 0,
                suspended: false,
                done: None,
                last_errors: VecDeque::new(),
            },
        );

        let me = self.clone();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid_for_release = child.id();
        tauri::async_runtime::spawn(async move {
            let out = {
                let me = me.clone();
                async move {
                    if let Some(s) = stdout {
                        read_lines(s, |l| me.on_stdout(id, l)).await;
                    }
                }
            };
            let err = {
                let me = me.clone();
                async move {
                    if let Some(s) = stderr {
                        read_lines(s, |l| me.on_stderr(id, l)).await;
                    }
                }
            };
            tokio::join!(out, err);
            let status = child.wait().await.ok();
            if let Some(pid) = pid_for_release {
                proc::release(pid);
            }
            me.on_exit(id, status);
            me.notify.notify_one();
        });
    }

    // ------------------------------------------------------------ worker I/O

    fn on_stdout(&self, id: u64, line: String) {
        let mut g = self.lock();
        let g = &mut *g;
        let Some(p) = g.procs.get_mut(&id) else {
            return;
        };
        let Some(j) = g.jobs.get_mut(&id) else { return };
        p.last_activity_ms = j.active_ms;
        g.changed.insert(id);

        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            push_capped(g.logs.entry(id).or_default(), line, LOG_CAP);
            return;
        };
        match v["t"].as_str().unwrap_or("") {
            "stage" => {
                let desc = v["desc"].as_str().unwrap_or("Working").to_string();
                let total = v["total"].as_f64();
                if v["unit"].as_str() == Some("page")
                    && let Some(t) = total.filter(|t| *t >= 1.0)
                {
                    j.pages = Some(j.pages.unwrap_or(0).max(t as u32));
                }
                let (start, _) = stage_span(&desc);
                j.progress = j.progress.max(start);
                j.stage = Some(desc);
                j.stage_total = total.filter(|t| *t > 0.0);
                j.stage_current = 0.0;
            }
            "progress" => {
                let cur = v["current"].as_f64().unwrap_or(0.0);
                let total = v["total"].as_f64().filter(|t| *t > 0.0).or(j.stage_total);
                j.stage_current = cur;
                if let (Some(t), Some(desc)) = (total, j.stage.as_deref()) {
                    let (a, b) = stage_span(desc);
                    j.progress = j.progress.max(a + (b - a) * (cur / t).clamp(0.0, 1.0));
                }
            }
            "log" => {
                let level = v["level"].as_str().unwrap_or("INFO");
                let msg = v["msg"].as_str().unwrap_or("").to_string();
                if level == "WARNING" {
                    j.warnings += 1;
                }
                if matches!(level, "ERROR" | "CRITICAL") {
                    push_capped(&mut p.last_errors, msg.clone(), 5);
                }
                let log = g.logs.entry(id).or_default();
                for l in msg.lines() {
                    push_capped(log, format!("[{level}] {l}"), LOG_CAP);
                }
            }
            "done" => {
                let code = v["code"].as_i64().unwrap_or(15) as i32;
                let msg = v["msg"].as_str().unwrap_or("").to_string();
                p.done = Some((code, msg));
            }
            _ => {}
        }
    }

    fn on_stderr(&self, id: u64, line: String) {
        if line.trim().is_empty() {
            return;
        }
        let mut g = self.lock();
        let g = &mut *g;
        if let (Some(p), Some(j)) = (g.procs.get_mut(&id), g.jobs.get(&id)) {
            p.last_activity_ms = j.active_ms;
            push_capped(&mut p.last_errors, line.clone(), 5);
        }
        push_capped(
            g.logs.entry(id).or_default(),
            format!("[stderr] {line}"),
            LOG_CAP,
        );
    }

    fn on_exit(&self, id: u64, status: Option<ExitStatus>) {
        let mut g = self.lock();
        let g = &mut *g;
        let Some(p) = g.procs.remove(&id) else { return };
        g.changed.insert(id);
        g.dirty = true;
        let max_retries = g.settings.max_retries;
        let stall = g.settings.stall_timeout_min;
        let Some(j) = g.jobs.get_mut(&id) else {
            p.plan.discard();
            return;
        };
        j.finished_at = Some(now_ms());
        j.stage = None;

        let detail = |fallback: &str| -> String {
            let from_done = p
                .done
                .as_ref()
                .map(|d| d.1.clone())
                .filter(|m| !m.is_empty());
            from_done
                .or_else(|| p.last_errors.back().cloned())
                .unwrap_or_else(|| fallback.to_string())
        };

        // (retryable, kind, message)
        let failure: Option<(bool, ErrorKind, String)> = match p.kill {
            Some(KillReason::Requeue) => {
                p.plan.discard();
                j.reset_to_queued();
                j.attempts = j.attempts.saturating_sub(1);
                None
            }
            Some(KillReason::Cancel) => {
                p.plan.discard();
                j.status = JobStatus::Cancelled;
                j.message = Some("Cancelled".into());
                None
            }
            Some(KillReason::Timeout) => Some((
                true,
                ErrorKind::Timeout,
                format!("No progress for {stall} min — stopped"),
            )),
            None => {
                let code = p
                    .done
                    .as_ref()
                    .map(|d| d.0)
                    .or_else(|| status.and_then(|s| s.code()));
                // Strict PDF/A with the internal backend exits 10 without
                // writing anything; without a partial file it is a failure.
                let pdfa_unconverted =
                    code == Some(10) && !p.plan.partial_path.is_file();
                match code {
                    Some(10) if pdfa_unconverted => Some((
                        false,
                        ErrorKind::Other,
                        "Can't be made into valid PDF/A — choose Auto or Plain PDF format".into(),
                    )),
                    Some(0) | Some(10) => match p.plan.commit(&j.input) {
                        Ok(note) => {
                            j.status = JobStatus::Done;
                            j.progress = 1.0;
                            j.output = Some(p.plan.final_path.clone());
                            j.output_size =
                                std::fs::metadata(&p.plan.final_path).ok().map(|m| m.len());
                            j.message = if code == Some(10) {
                                Some("Saved as regular PDF (PDF/A conversion failed)".into())
                            } else {
                                note
                            };
                            None
                        }
                        Err(e) => Some((true, ErrorKind::OutputError, e)),
                    },
                    Some(6) => {
                        p.plan.discard();
                        j.status = JobStatus::Skipped;
                        j.error_kind = Some(ErrorKind::AlreadyHasText);
                        j.message = Some(
                            "Already has text (choose Skip/Redo/Force mode to process it)".into(),
                        );
                        None
                    }
                    Some(8) => Some((
                        false,
                        ErrorKind::Encrypted,
                        "Encrypted or password-protected PDF".into(),
                    )),
                    Some(2) => Some((
                        false,
                        ErrorKind::InvalidInput,
                        detail("Invalid or unsupported PDF"),
                    )),
                    Some(1) | Some(9) => Some((
                        false,
                        ErrorKind::Other,
                        format!("Invalid OCR settings: {}", detail("bad arguments")),
                    )),
                    Some(3) => {
                        let m = detail("A required OCR component is missing");
                        // Every remaining file would fail the same way: stop and tell the user.
                        p.plan.discard();
                        j.reset_to_queued();
                        j.attempts = j.attempts.saturating_sub(1);
                        j.message = Some(m.clone());
                        if g.state == QueueState::Running {
                            g.state = QueueState::Paused;
                        }
                        g.fatal = Some(m);
                        g.meta_changed = true;
                        None
                    }
                    Some(4) => Some((
                        true,
                        ErrorKind::OutputError,
                        detail("Output PDF failed validation"),
                    )),
                    Some(5) => Some((true, ErrorKind::OutputError, detail("File access error"))),
                    Some(c) => Some((
                        true,
                        ErrorKind::Other,
                        format!("{} (exit {c})", detail("OCR failed")),
                    )),
                    None => {
                        #[cfg(unix)]
                        let sig = {
                            use std::os::unix::process::ExitStatusExt;
                            status.and_then(|s| s.signal())
                        };
                        #[cfg(not(unix))]
                        let sig: Option<i32> = None;
                        Some((
                            true,
                            ErrorKind::Crashed,
                            match sig {
                                Some(s) => {
                                    format!("Worker crashed (signal {s}) — possibly out of memory")
                                }
                                None => "Worker exited unexpectedly".into(),
                            },
                        ))
                    }
                }
            }
        };

        if let Some((retryable, kind, msg)) = failure {
            p.plan.discard();
            push_capped(
                g.logs.entry(id).or_default(),
                format!("[FAILED] {msg}"),
                LOG_CAP,
            );
            if retryable && j.attempts <= max_retries {
                let attempts = j.attempts;
                j.reset_to_queued();
                j.attempts = attempts;
                j.message = Some(format!("Retrying after: {msg}"));
            } else {
                j.status = JobStatus::Failed;
                j.error_kind = Some(kind);
                j.message = Some(msg);
            }
        }
    }

    // ------------------------------------------------------------ housekeeping

    fn tick(&self, dt: Duration) {
        let mut g = self.lock();
        let g = &mut *g;
        let dt_ms = dt.as_millis() as u64;
        let limit_ms = g.settings.stall_timeout_min as u64 * 60_000;
        for (id, p) in g.procs.iter_mut() {
            let Some(j) = g.jobs.get_mut(id) else {
                continue;
            };
            if p.suspended || j.status != JobStatus::Running || p.kill.is_some() {
                continue;
            }
            j.active_ms += dt_ms;
            g.changed.insert(*id);
            if limit_ms > 0 && j.active_ms.saturating_sub(p.last_activity_ms) > limit_ms {
                p.kill = Some(KillReason::Timeout);
                proc::kill_tree(p.pid);
            }
        }
    }

    fn flush(&self) {
        let mut g = self.lock();
        let upserts: Vec<Job> = g
            .changed
            .drain()
            .collect::<Vec<_>>()
            .iter()
            .filter_map(|id| g.jobs.get(id).cloned())
            .collect();
        let removed = std::mem::take(&mut g.removed);
        let meta = std::mem::take(&mut g.meta_changed).then(|| meta_of(&g));
        let finished = std::mem::take(&mut g.finished_pending).then(|| {
            let count = |s| g.jobs.values().filter(|j| j.status == s).count();
            FinishedEvent {
                done: count(JobStatus::Done),
                failed: count(JobStatus::Failed),
                skipped: count(JobStatus::Skipped),
            }
        });
        drop(g);

        if !upserts.is_empty() || !removed.is_empty() {
            self.sink
                .emit("engine://jobs", json_of(&JobsEvent { upserts, removed }));
        }
        if let Some(m) = meta {
            self.sink.emit("engine://meta", json_of(&m));
        }
        if let Some(f) = finished {
            self.sink.emit("engine://finished", json_of(&f));
            self.sink.attention();
        }
    }

    fn save_if_dirty(&self) {
        let mut g = self.lock();
        if !std::mem::take(&mut g.dirty) {
            return;
        }
        let data = persist_json(&g);
        drop(g);
        self.write_state(data);
    }

    fn write_state(&self, data: Vec<u8>) {
        let tmp = self.state_file.with_extension("json.tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, &self.state_file);
        }
    }
}

fn resume_locked(g: &mut Inner) {
    g.state = QueueState::Running;
    let ids: Vec<u64> = g.procs.keys().copied().collect();
    for id in ids {
        let p = g.procs.get_mut(&id).unwrap();
        if p.suspended {
            proc::resume(p.pid);
            p.suspended = false;
            if let Some(j) = g.jobs.get_mut(&id) {
                j.status = JobStatus::Running;
            }
            g.changed.insert(id);
        }
    }
}

fn meta_of(g: &Inner) -> Meta {
    Meta {
        state: g.state,
        fatal: g.fatal.clone(),
        settings: g.settings.clone(),
    }
}

fn persist_json(g: &Inner) -> Vec<u8> {
    let p = Persisted {
        version: 1,
        settings: g.settings.clone(),
        jobs: g.jobs.values().cloned().collect(),
        next_id: g.next_id,
        state: g.state,
        in_flight: g
            .procs
            .values()
            .map(|p| (p.pid, p.plan.partial_path.clone()))
            .collect(),
    };
    serde_json::to_vec(&p).unwrap_or_default()
}

async fn read_lines<R: AsyncRead + Unpin>(r: R, mut f: impl FnMut(String)) {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                f(line);
            }
        }
    }
}

fn json_of<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or_default()
}
