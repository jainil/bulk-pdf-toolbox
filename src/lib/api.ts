import { invoke } from "@tauri-apps/api/core";
import type { AddResult, JobStatus, RuntimeInfo, Settings, Snapshot } from "./types";

export const api = {
  snapshot: () => invoke<Snapshot>("get_snapshot"),
  runtimeInfo: () => invoke<RuntimeInfo>("runtime_info"),
  addPaths: (paths: string[]) => invoke<AddResult>("add_paths", { paths }),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  start: () => invoke<void>("start"),
  pause: () => invoke<void>("pause"),
  stop: () => invoke<void>("stop"),
  cancel: (ids: number[]) => invoke<void>("cancel_jobs", { ids }),
  retry: (ids: number[]) => invoke<void>("retry_jobs", { ids }),
  retryFailed: () => invoke<void>("retry_failed"),
  remove: (ids: number[]) => invoke<void>("remove_jobs", { ids }),
  prioritize: (ids: number[]) => invoke<void>("prioritize_jobs", { ids }),
  clear: (statuses: JobStatus[]) => invoke<void>("clear_jobs", { statuses }),
  jobLog: (id: number) => invoke<string[]>("job_log", { id }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  quit: () => invoke<void>("quit_app"),
};
