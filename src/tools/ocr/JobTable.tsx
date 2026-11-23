import { memo, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Job, QueueState } from "../../lib/types";
import { api } from "../../lib/api";
import { FILE_MANAGER, bytes, dirName, duration, fileName, tildify } from "../../lib/format";
import { Icon } from "../../components/Icon";

const ROW = 62;

interface Props {
  jobs: Job[];
  selected: Set<number>;
  setSelected: (s: Set<number>) => void;
  onShowLog: (id: number) => void;
  onError: (msg: string) => void;
  queueState: QueueState;
}

export function JobTable({ jobs, selected, setSelected, onShowLog, onError, queueState }: Props) {
  const parent = useRef<HTMLDivElement>(null);
  const anchor = useRef<number | null>(null);
  const v = useVirtualizer({
    count: jobs.length,
    getScrollElement: () => parent.current,
    estimateSize: () => ROW,
    overscan: 12,
    getItemKey: (i) => jobs[i].id,
  });

  // Checkbox semantics: a click toggles one row and keeps the rest; Shift-click
  // sets the whole range from the last clicked row to match that row's state.
  const onRowClick = (e: React.MouseEvent, index: number) => {
    const id = jobs[index].id;
    const a = anchor.current == null ? -1 : jobs.findIndex((j) => j.id === anchor.current);
    const next = new Set(selected);
    if (e.shiftKey && a >= 0) {
      const on = selected.has(jobs[a].id);
      const [lo, hi] = a < index ? [a, index] : [index, a];
      for (let i = lo; i <= hi; i++) {
        if (on) next.add(jobs[i].id);
        else next.delete(jobs[i].id);
      }
    } else {
      if (next.has(id)) next.delete(id);
      else next.add(id);
      anchor.current = id;
    }
    setSelected(next);
  };

  const visibleSelected = jobs.reduce((n, j) => n + (selected.has(j.id) ? 1 : 0), 0);
  const allState = visibleSelected === 0 ? "off" : visibleSelected === jobs.length ? "on" : "mixed";
  const toggleAll = () => {
    const next = new Set(selected);
    if (allState === "on") jobs.forEach((j) => next.delete(j.id));
    else jobs.forEach((j) => next.add(j.id));
    setSelected(next);
  };

  const safe = (p: Promise<unknown>) => p.catch((e) => onError(String(e)));

  return (
    <div className="table">
      <div className="thead">
        <Checkbox
          state={jobs.length ? allState : "off"}
          disabled={!jobs.length}
          label={allState === "on" ? "Deselect all" : "Select all"}
          onToggle={toggleAll}
        />
        <div>File</div>
        <div className="num">Pages</div>
        <div className="num">Size</div>
        <div>Status</div>
        <div />
      </div>
      <div className={`tbody ${selected.size ? "selecting" : ""}`} ref={parent}>
        {jobs.length === 0 && <div className="table-empty">No files match this filter.</div>}
        <div style={{ height: v.getTotalSize(), position: "relative" }}>
          {v.getVirtualItems().map((item) => {
            const j = jobs[item.index];
            return (
              <div
                key={item.key}
                className="row-wrap"
                style={{ transform: `translateY(${item.start}px)`, height: ROW }}
                onClick={(e) => onRowClick(e, item.index)}
                onDoubleClick={() => j.output && safe(api.openPath(j.output))}
              >
                <Row job={j} selected={selected.has(j.id)} onShowLog={onShowLog} safe={safe} queueState={queueState} />
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function Checkbox({
  state,
  label,
  disabled,
  onToggle,
}: {
  state: "on" | "off" | "mixed";
  label: string;
  disabled?: boolean;
  onToggle?: () => void;
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={state === "mixed" ? "mixed" : state === "on"}
      aria-label={label}
      title={label}
      disabled={disabled}
      className={`checkbox ${state}`}
      onClick={onToggle}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      {state === "on" && <Icon name="check" size={13} />}
      {state === "mixed" && <Icon name="minus" size={13} />}
    </button>
  );
}

const STATUS_LABEL: Record<Job["status"], string> = {
  queued: "Queued",
  running: "Processing",
  paused: "Paused",
  done: "Done",
  skipped: "Skipped",
  failed: "Failed",
  cancelled: "Cancelled",
};

const STATUS_ICON: Record<Job["status"], string> = {
  queued: "clock",
  running: "ocr",
  paused: "pause",
  done: "check",
  skipped: "skip",
  failed: "alert",
  cancelled: "x",
};

function stageText(j: Job): string {
  if (!j.stage) return "Starting…";
  if (j.stageTotal && j.stageTotal > 1 && (j.stage === "OCR" || j.stage === "Scanning contents")) {
    return `${j.stage} · page ${Math.min(Math.floor(j.stageCurrent) + (j.stageCurrent < j.stageTotal ? 1 : 0), j.stageTotal)}/${j.stageTotal}`;
  }
  return j.stage;
}

const Row = memo(function Row({
  job: j,
  selected,
  onShowLog,
  safe,
  queueState,
}: {
  job: Job;
  selected: boolean;
  onShowLog: (id: number) => void;
  safe: (p: Promise<unknown>) => void;
  queueState: QueueState;
}) {
  const active = j.status === "running" || j.status === "paused";
  const stop = (e: React.MouseEvent) => e.stopPropagation();
  const delta =
    j.status === "done" && j.outputSize != null && j.sizeBytes > 0
      ? Math.round(((j.outputSize - j.sizeBytes) / j.sizeBytes) * 100)
      : null;

  return (
    <div className={`row ${selected ? "selected" : ""} status-${j.status}`}>
      {/* The click bubbles to the row wrapper, which does the toggling. */}
      <Checkbox state={selected ? "on" : "off"} label={`Select ${fileName(j.input)}`} />
      <div className="cell file">
        <div className="name" title={j.input}>
          {fileName(j.input)}
        </div>
        <div className="path" title={j.output ?? j.input}>
          {/* LRM marks keep the path left-to-right while .path truncates from the left. */}
          {"\u200E" + (j.status === "done" && j.output ? `→ ${tildify(j.output)}` : tildify(dirName(j.input))) + "\u200E"}
        </div>
      </div>
      <div className="cell num">{j.pages ?? "—"}</div>
      <div className="cell num">
        {bytes(j.sizeBytes)}
        {delta != null && (
          <div className={`delta ${delta <= 0 ? "smaller" : "larger"}`}>
            {delta > 0 ? "+" : ""}
            {delta}%
          </div>
        )}
      </div>
      <div className="cell status">
        <div className="status-line">
          <span className={`status-chip ${j.status}`}>
            <Icon name={STATUS_ICON[j.status]} size={12} />
            {STATUS_LABEL[j.status]}
          </span>
          {active && <span className="stage">{stageText(j)}</span>}
          {(j.status === "done" || active) && j.activeMs > 0 && <span className="muted small">{duration(j.activeMs)}</span>}
          {j.attempts > 1 && <span className="muted small">try {j.attempts}</span>}
          {j.warnings > 0 && j.status === "done" && (
            <span className="warn small" title="See log for warnings">
              {j.warnings} warning{j.warnings === 1 ? "" : "s"}
            </span>
          )}
        </div>
        {active ? (
          <div className="bar">
            <div
              className={`bar-fill ${j.status === "paused" || queueState === "paused" ? "paused" : ""}`}
              style={{ width: `${Math.max(2, j.progress * 100).toFixed(1)}%` }}
            />
          </div>
        ) : (
          j.message && (
            <div className={`message ${j.status}`} title={j.message}>
              {j.message}
            </div>
          )
        )}
      </div>
      <div className="cell actions" onClick={stop}>
        {j.status === "done" && j.output && (
          <>
            <button className="icon-btn" title="Open" onClick={() => safe(api.openPath(j.output!))}>
              <Icon name="open" />
            </button>
            <button className="icon-btn" title={`Show in ${FILE_MANAGER}`} onClick={() => safe(api.revealPath(j.output!))}>
              <Icon name="reveal" />
            </button>
          </>
        )}
        {(j.status === "failed" || j.status === "cancelled" || j.status === "skipped") && (
          <button className="icon-btn" title="Retry" onClick={() => safe(api.retry([j.id]))}>
            <Icon name="retry" />
          </button>
        )}
        {(j.attempts > 0 || active) && (
          <button className="icon-btn" title="Log" onClick={() => onShowLog(j.id)}>
            <Icon name="log" />
          </button>
        )}
        {(active || j.status === "queued") && (
          <button className="icon-btn" title="Cancel" onClick={() => safe(api.cancel([j.id]))}>
            <Icon name="x" />
          </button>
        )}
        {!active && (
          <button className="icon-btn" title="Remove from list" onClick={() => safe(api.remove([j.id]))}>
            <Icon name="trash" />
          </button>
        )}
      </div>
    </div>
  );
});
