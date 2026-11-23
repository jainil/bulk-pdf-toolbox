import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import type { Job } from "../../lib/types";
import { fileName } from "../../lib/format";
import { Icon } from "../../components/Icon";

export function LogModal({ job, onClose }: { job: Job | undefined; onClose: () => void }) {
  const [lines, setLines] = useState<string[]>([]);
  const end = useRef<HTMLDivElement>(null);
  const live = job?.status === "running" || job?.status === "paused";

  useEffect(() => {
    if (!job) return;
    let alive = true;
    const load = () => api.jobLog(job.id).then((l) => alive && setLines(l));
    load();
    const t = live ? setInterval(load, 1000) : undefined;
    return () => {
      alive = false;
      if (t) clearInterval(t);
    };
  }, [job?.id, live]);

  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [lines.length]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);

  if (!job) return null;
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal log" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h3>{fileName(job.input)}</h3>
          <div className="group">
            <button className="btn small" onClick={() => navigator.clipboard.writeText(lines.join("\n"))}>
              <Icon name="copy" /> Copy
            </button>
            <button className="icon-btn" onClick={onClose} title="Close">
              <Icon name="x" />
            </button>
          </div>
        </div>
        {job.message && <div className={`banner ${job.status === "failed" ? "error" : "info"}`}>{job.message}</div>}
        <pre className="log-body">
          {lines.length ? lines.map((l, i) => <div key={i} className={lineClass(l)}>{l}</div>) : "No log output (logs are kept for this session only)."}
          <div ref={end} />
        </pre>
      </div>
    </div>
  );
}

function lineClass(l: string) {
  if (l.startsWith("[ERROR]") || l.startsWith("[FAILED]") || l.startsWith("[CRITICAL]")) return "l-err";
  if (l.startsWith("[WARNING]")) return "l-warn";
  if (l.startsWith("──")) return "l-head";
  return "";
}
