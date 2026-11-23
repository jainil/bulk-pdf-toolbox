import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "../../lib/api";
import { useEngine, type FinishedEvent } from "../../lib/store";
import type { Job, JobStatus, RuntimeInfo } from "../../lib/types";
import { duration, plural } from "../../lib/format";
import { Icon } from "../../components/Icon";
import { JobTable } from "./JobTable";
import { SettingsPanel } from "./SettingsPanel";
import { LogModal } from "./LogModal";

type Filter = "all" | "queued" | "active" | "done" | "failed" | "skipped";

const FILTERS: { id: Filter; label: string; match: (s: JobStatus) => boolean }[] = [
  { id: "all", label: "All", match: () => true },
  { id: "queued", label: "Queued", match: (s) => s === "queued" },
  { id: "active", label: "Processing", match: (s) => s === "running" || s === "paused" },
  { id: "done", label: "Done", match: (s) => s === "done" },
  { id: "failed", label: "Failed", match: (s) => s === "failed" },
  { id: "skipped", label: "Skipped", match: (s) => s === "skipped" || s === "cancelled" },
];

interface Toast {
  id: number;
  text: string;
  tone: "info" | "ok" | "warn" | "error";
}

export function OcrTool() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toast = useCallback((text: string, tone: Toast["tone"] = "info") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, text, tone }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 5000);
  }, []);

  const onFinished = useCallback(
    (e: FinishedEvent) => {
      const parts = [`${e.done} done`];
      if (e.failed) parts.push(`${e.failed} failed`);
      if (e.skipped) parts.push(`${e.skipped} skipped`);
      toast(`Queue finished — ${parts.join(", ")}`, e.failed ? "warn" : "ok");
    },
    [toast],
  );

  const { meta, setMeta, list, version, resync } = useEngine(onFinished);
  const jobs = list();
  const [runtime, setRuntime] = useState<RuntimeInfo | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [logJob, setLogJob] = useState<number | null>(null);
  const [showSettings, setShowSettings] = useState(true);
  const [confirmQuit, setConfirmQuit] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [adding, setAdding] = useState(false);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    api.runtimeInfo().then(setRuntime);
  }, []);

  // ---- adding files
  const addPaths = useCallback(
    async (paths: string[]) => {
      if (!paths.length) return;
      setAdding(true);
      try {
        const r = await api.addPaths(paths);
        const parts = [`Added ${plural(r.added, "PDF")}`];
        if (r.duplicates) parts.push(`${r.duplicates} already in list`);
        if (r.ignored) parts.push(`${r.ignored} not PDFs`);
        toast(parts.join(" · "), r.added ? "ok" : "warn");
      } catch (e) {
        toast(String(e), "error");
      } finally {
        setAdding(false);
      }
    },
    [toast],
  );

  const pickFiles = async () => {
    const r = await open({ multiple: true, filters: [{ name: "PDF", extensions: ["pdf", "PDF"] }] });
    if (r) addPaths(Array.isArray(r) ? r : [r]);
  };
  const pickFolder = async () => {
    const r = await open({ directory: true, multiple: true });
    if (r) addPaths(Array.isArray(r) ? r : [r]);
  };

  useEffect(() => {
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type === "enter" || e.payload.type === "over") setDragging(true);
      else if (e.payload.type === "leave") setDragging(false);
      else if (e.payload.type === "drop") {
        setDragging(false);
        addPaths(e.payload.paths);
      }
    });
    return () => {
      p.then((u) => u());
    };
  }, [addPaths]);

  useEffect(() => {
    const p = listen("app://confirm-quit", () => setConfirmQuit(true));
    return () => {
      p.then((u) => u());
    };
  }, []);

  // ---- derived stats
  const stats = useMemo(() => {
    const c: Record<JobStatus, number> = {
      queued: 0, running: 0, paused: 0, done: 0, skipped: 0, failed: 0, cancelled: 0,
    };
    let progressSum = 0;
    let doneMs = 0;
    let doneBytes = 0;
    let remainingBytes = 0;
    for (const j of jobs) {
      c[j.status]++;
      if (j.status === "done") {
        doneMs += j.activeMs;
        doneBytes += j.sizeBytes;
      }
      if (j.status === "queued") remainingBytes += j.sizeBytes;
      if (j.status === "running" || j.status === "paused") remainingBytes += j.sizeBytes * (1 - j.progress);
      progressSum += j.status === "queued" ? 0 : j.status === "running" || j.status === "paused" ? j.progress : 1;
    }
    const total = jobs.length;
    const finished = c.done + c.failed + c.skipped + c.cancelled;
    const parallel = meta?.settings.parallelFiles ?? 1;
    const eta =
      c.done >= 2 && doneBytes > 0 && remainingBytes > 0
        ? ((doneMs / doneBytes) * remainingBytes) / parallel
        : null;
    return { c, total, finished, pct: total ? progressSum / total : 0, eta };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, meta?.settings.parallelFiles]);

  const visible = useMemo(() => {
    const f = FILTERS.find((x) => x.id === filter)!;
    const q = query.trim().toLowerCase();
    return jobs.filter((j) => f.match(j.status) && (!q || j.input.toLowerCase().includes(q)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, filter, query]);

  // Drop selection entries that disappeared.
  useEffect(() => {
    setSelected((s) => {
      const ids = new Set(visible.map((j) => j.id));
      const next = new Set([...s].filter((id) => ids.has(id)));
      return next.size === s.size ? s : next;
    });
  }, [visible]);

  const state = meta?.state ?? "idle";
  const canStart = stats.c.queued > 0 || stats.c.running + stats.c.paused > 0;
  const suspendable = runtime?.supportsSuspend ?? true;

  const run = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      toast(String(e), "error");
    }
  };

  const selectedJobs = jobs.filter((j) => selected.has(j.id));
  const selIds = [...selected];

  // Keyboard shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      if ((e.metaKey || e.ctrlKey) && e.key === "a") {
        e.preventDefault();
        setSelected(new Set(visible.map((j) => j.id)));
      } else if (e.key === "Escape") {
        setSelected(new Set());
      } else if ((e.key === "Backspace" || e.key === "Delete") && selected.size) {
        api.remove([...selected]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [visible, selected]);

  return (
    <div className="tool">
      <header className="tool-header">
        <div>
          <h1>Bulk OCR</h1>
          <p className="subtitle">Make scanned PDFs searchable</p>
        </div>
        <div className="header-right">
          <button className={`btn ghost icon ${showSettings ? "on" : ""}`} onClick={() => setShowSettings((s) => !s)} title="Settings">
            <Icon name="settings" />
          </button>
        </div>
      </header>

      {runtime && !runtime.ok && (
        <div className="banner error">
          <Icon name="alert" />
          <div>
            <strong>OCR engine not available.</strong> {runtime.error}
          </div>
        </div>
      )}
      {meta?.fatal && (
        <div className="banner error">
          <Icon name="alert" />
          <div>
            <strong>Queue paused:</strong> {meta.fatal}
          </div>
        </div>
      )}
      {state === "paused" && !meta?.fatal && stats.c.running + stats.c.paused === 0 && stats.c.queued > 0 && (
        <div className="banner info">
          <Icon name="pause" />
          <div>
            {plural(stats.c.queued, "file")} waiting from your last session. Press <strong>Resume</strong> to continue.
          </div>
        </div>
      )}

      <div className="workspace">
        <div className="main-col">
          <section className="island controls">
            <div className="toolbar">
              <div className="group">
                <button className="btn" onClick={pickFiles} disabled={adding}>
                  <Icon name="file" /> Add files
                </button>
                <button className="btn" onClick={pickFolder} disabled={adding}>
                  <Icon name="folder" /> Add folder
                </button>
              </div>
              <div className="group">
                {state === "running" ? (
                  <button className="btn primary" onClick={() => run(api.pause)} title={suspendable ? "Suspend all work immediately" : "Finish current files, then pause"}>
                    <Icon name="pause" /> Pause
                  </button>
                ) : (
                  <button
                    className="btn primary"
                    disabled={!canStart || !runtime?.ok}
                    onClick={() =>
                      run(async () => {
                        // Make sure a just-edited setting reaches the engine first.
                        if (saveTimer.current && meta) {
                          clearTimeout(saveTimer.current);
                          saveTimer.current = null;
                          await api.setSettings(meta.settings);
                        }
                        await api.start();
                      })
                    }
                  >
                    <Icon name="play" /> {state === "paused" ? "Resume" : "Start"}
                  </button>
                )}
                <button
                  className="btn"
                  disabled={state === "idle" && stats.c.running + stats.c.paused === 0}
                  onClick={() => run(api.stop)}
                  title="Stop processing; files in progress go back to the queue"
                >
                  <Icon name="stop" /> Stop
                </button>
              </div>
              <div className="spacer" />
              <div className="group">
                <button className="btn ghost" disabled={!stats.c.failed} onClick={() => run(api.retryFailed)}>
                  <Icon name="retry" /> Retry failed
                </button>
                <button
                  className="btn ghost"
                  disabled={!(stats.c.done + stats.c.skipped + stats.c.cancelled)}
                  onClick={() => run(() => api.clear(["done", "skipped", "cancelled"]))}
                >
                  Clear finished
                </button>
                <button
                  className="btn ghost danger"
                  disabled={!stats.total || stats.c.running + stats.c.paused === stats.total}
                  onClick={() => run(() => api.clear(["queued", "done", "skipped", "cancelled", "failed"]))}
                  title="Remove every file that is not being processed"
                >
                  Clear all
                </button>
              </div>
            </div>

            <div className="summary">
              <div className="summary-bar">
                <div className={`summary-fill ${state}`} style={{ width: `${(stats.pct * 100).toFixed(2)}%` }} />
              </div>
              <div className="summary-text">
                <span>
                  <strong>{stats.finished.toLocaleString()}</strong> / {stats.total.toLocaleString()} files
                </span>
                {stats.c.running + stats.c.paused > 0 && (
                  <span className="pill running">
                    {state === "paused" && suspendable ? "paused" : "processing"} {stats.c.running + stats.c.paused}
                  </span>
                )}
                {stats.c.failed > 0 && <span className="pill failed">{stats.c.failed} failed</span>}
                {state === "running" && stats.eta != null && <span className="muted">~{duration(stats.eta)} left</span>}
                {state === "paused" && !suspendable && stats.c.running > 0 && (
                  <span className="muted">Pausing — waiting for current files to finish</span>
                )}
              </div>
            </div>
          </section>

          <section className="island list-pane">
            {/* While files are selected, the action bar takes the filter row's place
                (same height) so rows never shift under the cursor between clicks. */}
            {selected.size > 0 ? (
              <div className="list-controls selection-bar">
                <span>{selected.size} selected</span>
                <button className="btn small" disabled={!selectedJobs.some((j) => j.status === "queued")} onClick={() => run(() => api.prioritize(selIds).then(resync))}>
                  <Icon name="up" /> Process next
                </button>
                <button className="btn small" disabled={!selectedJobs.some((j) => ["done", "failed", "skipped", "cancelled"].includes(j.status))} onClick={() => run(() => api.retry(selIds))}>
                  <Icon name="retry" /> Re-run
                </button>
                <button className="btn small" disabled={!selectedJobs.some((j) => ["queued", "running", "paused"].includes(j.status))} onClick={() => run(() => api.cancel(selIds))}>
                  <Icon name="x" /> Cancel
                </button>
                <button className="btn small danger" disabled={!selectedJobs.some((j) => j.status !== "running" && j.status !== "paused")} onClick={() => run(() => api.remove(selIds))}>
                  <Icon name="trash" /> Remove
                </button>
                <div className="spacer" />
                <button className="btn small ghost" onClick={() => setSelected(new Set())} title="Esc">
                  Clear selection
                </button>
              </div>
            ) : (
              <div className="list-controls">
                <div className="segmented">
                  {FILTERS.map((f) => {
                    const n = jobs.filter((j) => f.match(j.status)).length;
                    return (
                      <button key={f.id} className={filter === f.id ? "on" : ""} onClick={() => setFilter(f.id)}>
                        {f.label}
                        <span className="count">{n}</span>
                      </button>
                    );
                  })}
                </div>
                <input className="search" placeholder="Filter by name…" value={query} onChange={(e) => setQuery(e.target.value)} />
              </div>
            )}

            {jobs.length === 0 ? (
              <div className={`dropzone ${dragging ? "drag" : ""}`}>
                <Icon name="ocr" size={40} />
                <h2>Drop PDFs or folders here</h2>
                <p>Folders are scanned recursively. Hundreds of files are fine — the queue survives pauses, errors and restarts.</p>
                <div className="group">
                  <button className="btn primary" onClick={pickFiles}>
                    <Icon name="file" /> Add files
                  </button>
                  <button className="btn" onClick={pickFolder}>
                    <Icon name="folder" /> Add folder
                  </button>
                </div>
              </div>
            ) : (
              <>
                <JobTable
                  jobs={visible}
                  selected={selected}
                  setSelected={setSelected}
                  onShowLog={setLogJob}
                  onError={(e) => toast(e, "error")}
                  queueState={state}
                />
                {dragging && <div className="drop-overlay">Drop to add PDFs</div>}
              </>
            )}
          </section>
        </div>

        {showSettings && meta && (
          <SettingsPanel
            settings={meta.settings}
            languages={runtime?.languages ?? []}
            locked={stats.c.running + stats.c.paused > 0}
            onClose={() => setShowSettings(false)}
            onChange={(s) => {
              setMeta({ ...meta, settings: s });
              if (saveTimer.current) clearTimeout(saveTimer.current);
              saveTimer.current = setTimeout(() => api.setSettings(s), 300);
            }}
          />
        )}
      </div>

      {logJob != null && <LogModal job={jobs.find((j) => j.id === logJob) as Job | undefined} onClose={() => setLogJob(null)} />}

      {confirmQuit && (
        <div className="modal-backdrop">
          <div className="modal small">
            <h3>OCR is still running</h3>
            <p>
              Files in progress will be restarted next time you open the app. Queued files are kept.
            </p>
            <div className="modal-actions">
              <button className="btn" onClick={() => setConfirmQuit(false)}>
                Keep working
              </button>
              <button className="btn danger-solid" onClick={() => api.quit()}>
                Quit anyway
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.tone}`}>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}
