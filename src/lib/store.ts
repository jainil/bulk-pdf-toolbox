import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import type { Job, Meta } from "./types";

interface JobsEvent {
  upserts: Job[];
  removed: number[];
}

export interface FinishedEvent {
  done: number;
  failed: number;
  skipped: number;
}

/**
 * Mirrors the Rust engine. The backend sends batched diffs (~every 150 ms);
 * we keep a Map plus an order array and bump a version counter so React
 * re-renders once per batch, not once per job.
 */
export function useEngine(onFinished?: (e: FinishedEvent) => void) {
  const jobs = useRef(new Map<number, Job>());
  const order = useRef<number[]>([]);
  const [version, setVersion] = useState(0);
  const [meta, setMeta] = useState<Meta | null>(null);
  const finishedRef = useRef(onFinished);
  finishedRef.current = onFinished;

  const resync = useCallback(async () => {
    const s = await api.snapshot();
    jobs.current = new Map(s.jobs.map((j) => [j.id, j]));
    order.current = s.jobs.map((j) => j.id);
    setMeta({ state: s.state, fatal: s.fatal, settings: s.settings });
    setVersion((v) => v + 1);
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisten: Array<() => void> = [];
    (async () => {
      unlisten.push(
        await listen<JobsEvent>("engine://jobs", ({ payload }) => {
          const m = jobs.current;
          if (payload.removed.length) {
            const gone = new Set(payload.removed);
            payload.removed.forEach((id) => m.delete(id));
            order.current = order.current.filter((id) => !gone.has(id));
          }
          for (const j of payload.upserts) {
            if (!m.has(j.id)) order.current.push(j.id);
            m.set(j.id, j);
          }
          setVersion((v) => v + 1);
        }),
      );
      unlisten.push(await listen<Meta>("engine://meta", ({ payload }) => setMeta(payload)));
      unlisten.push(
        await listen<FinishedEvent>("engine://finished", ({ payload }) => finishedRef.current?.(payload)),
      );
      if (!disposed) await resync();
    })();
    return () => {
      disposed = true;
      unlisten.forEach((u) => u());
    };
  }, [resync]);

  const list = useCallback(
    () => order.current.map((id) => jobs.current.get(id)!).filter(Boolean),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [version],
  );

  return { version, meta, setMeta, list, resync };
}
