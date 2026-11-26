/**
 * Dev-only fake backend so the UI can be previewed in a plain browser
 * (`pnpm dev`, open http://localhost:1420). Never bundled into the app:
 * main.tsx only imports it when there is no real Tauri runtime.
 */
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import type { Job, JobStatus, Meta, Settings } from "../lib/types";

const settings: Settings = {
  languages: ["eng"], mode: "skip", outputType: "auto", optimize: 1, deskew: false, rotatePages: false,
  sidecar: false, invalidateSignatures: false, outputMode: "suffix", suffix: "_ocr", outputDir: null, conflict: "rename",
  parallelFiles: 3, jobsPerFile: 3, maxRetries: 1, stallTimeoutMin: 15,
};
let state: Meta["state"] = "idle";
let nextId = 1;
const jobs = new Map<number, Job>();

function mk(name: string, status: JobStatus, extra: Partial<Job> = {}): Job {
  const id = nextId++;
  return {
    id, input: `/Users/demo/Scans/2024/${name}`, baseDir: "/Users/demo/Scans", sizeBytes: 400_000 + ((id * 7919) % 9_000_000),
    status, stage: null, stageCurrent: 0, stageTotal: null, pages: status === "queued" ? null : 1 + (id % 14),
    progress: status === "done" ? 1 : 0, output: null, outputSize: null, message: null, errorKind: null,
    attempts: status === "queued" ? 0 : 1, warnings: 0, startedAt: null, finishedAt: null, activeMs: 0, ...extra,
  };
}

function seed() {
  const add = (j: Job) => jobs.set(j.id, j);
  for (let i = 0; i < 9; i++) {
    const j = mk(`invoice_${String(i).padStart(3, "0")}.pdf`, "done");
    j.output = j.input.replace(".pdf", "_ocr.pdf");
    j.outputSize = Math.round(j.sizeBytes * 0.82);
    j.activeMs = 4000 + i * 900;
    j.warnings = i === 4 ? 2 : 0;
    add(j);
  }
  add(mk("contract_signed.pdf", "failed", { errorKind: "encrypted", message: "Encrypted or password-protected PDF" }));
  add(mk("statement_march.pdf", "skipped", { errorKind: "already_has_text", message: "Already has text (choose Skip/Redo/Force mode to process it)" }));
  add(mk("nda_signed.pdf", "skipped", { errorKind: "digitally_signed", message: "Digitally signed — turn on “Process signed PDFs” to OCR it (breaks the signature)" }));
  add(mk("scan_broken.pdf", "failed", { attempts: 2, errorKind: "invalid_input", message: "Invalid or unsupported PDF: unable to find trailer dictionary" }));
  for (let i = 0; i < 3; i++) add(mk(`board_minutes_${i + 1}.pdf`, "running", { stage: "OCR", stageTotal: 12, pages: 12 }));
  for (let i = 0; i < 160; i++) add(mk(`archive/box_${Math.floor(i / 20)}/page_scan_${String(i).padStart(4, "0")}.pdf`, "queued"));
}

const meta = (): Meta => ({ state, fatal: null, settings });
const pushJobs = (list: Job[], removed: number[] = []) => emit("engine://jobs", { upserts: list, removed });
const pushMeta = () => emit("engine://meta", meta());

function tick() {
  if (state !== "running") return;
  const changed: Job[] = [];
  const running = [...jobs.values()].filter((j) => j.status === "running");
  for (const j of running) {
    j.activeMs += 500;
    j.stageCurrent = Math.min((j.stageTotal ?? 1), j.stageCurrent + 0.35);
    j.progress = Math.min(1, 0.08 + 0.77 * (j.stageCurrent / (j.stageTotal ?? 1)));
    if (j.progress >= 0.85) {
      j.status = "done"; j.progress = 1; j.stage = null;
      j.output = j.input.replace(".pdf", "_ocr.pdf"); j.outputSize = Math.round(j.sizeBytes * 0.9);
    }
    changed.push(j);
  }
  const active = [...jobs.values()].filter((j) => j.status === "running").length;
  const next = [...jobs.values()].filter((j) => j.status === "queued").slice(0, settings.parallelFiles - active);
  for (const j of next) {
    Object.assign(j, { status: "running", stage: "OCR", stageTotal: 3 + (j.id % 9), pages: 3 + (j.id % 9), stageCurrent: 0, attempts: 1 });
    changed.push(j);
  }
  if (!active && !next.length) {
    state = "idle";
    pushMeta();
  }
  pushJobs(changed);
}

export function installMockBackend() {
  mockWindows("main");
  seed();
  setInterval(tick, 500);
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      const ids = (a.ids as number[]) ?? [];
      switch (cmd) {
        case "get_snapshot":
          return { ...meta(), jobs: [...jobs.values()] };
        case "runtime_info":
          return { ok: true, root: "/Applications/Bulk PDF Toolbox.app/Contents/Resources/runtime", error: null, ocrmypdf: "17.14.2", tesseract: "5.5.4", languages: ["deu", "eng", "fra", "hin", "spa"], supportsSuspend: true };
        case "start":
          state = "running";
          [...jobs.values()].filter((j) => j.status === "paused").forEach((j) => (j.status = "running"));
          pushMeta();
          return;
        case "pause": {
          state = "paused";
          const r = [...jobs.values()].filter((j) => j.status === "running");
          r.forEach((j) => (j.status = "paused"));
          pushJobs(r);
          pushMeta();
          return;
        }
        case "stop": {
          state = "idle";
          const r = [...jobs.values()].filter((j) => j.status === "running" || j.status === "paused");
          r.forEach((j) => Object.assign(j, { status: "queued", progress: 0, stage: null, activeMs: 0 }));
          pushJobs(r);
          pushMeta();
          return;
        }
        case "set_settings":
          Object.assign(settings, a.settings);
          return;
        case "retry_jobs":
        case "retry_failed": {
          const r = [...jobs.values()].filter((j) => (cmd === "retry_failed" ? j.status === "failed" : ids.includes(j.id)));
          r.forEach((j) => Object.assign(j, { status: "queued", message: null, errorKind: null, attempts: 0, progress: 0 }));
          pushJobs(r);
          return;
        }
        case "cancel_jobs": {
          const r = ids.map((id) => jobs.get(id)!).filter(Boolean);
          r.forEach((j) => Object.assign(j, { status: "cancelled", message: "Cancelled", stage: null }));
          pushJobs(r);
          return;
        }
        case "remove_jobs":
        case "clear_jobs": {
          const st = (a.statuses as JobStatus[]) ?? [];
          const gone = [...jobs.values()].filter((j) => (cmd === "clear_jobs" ? st.includes(j.status) : ids.includes(j.id))).map((j) => j.id);
          gone.forEach((id) => jobs.delete(id));
          pushJobs([], gone);
          return;
        }
        case "add_paths":
          return { added: 0, duplicates: 0, ignored: 0 };
        case "job_log":
          return ["── attempt 1: /Users/demo/Scans/2024/scan_broken.pdf", "[INFO] Scanning contents", "[WARNING] page 3: image DPI is very low", "[ERROR] unable to find trailer dictionary", "[FAILED] Invalid or unsupported PDF"];
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}
