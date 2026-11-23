export type JobStatus =
  | "queued"
  | "running"
  | "paused"
  | "done"
  | "skipped"
  | "failed"
  | "cancelled";

export type ErrorKind =
  | "encrypted"
  | "invalid_input"
  | "missing_input"
  | "already_has_text"
  | "output_exists"
  | "output_error"
  | "missing_dependency"
  | "timeout"
  | "crashed"
  | "other";

export interface Job {
  id: number;
  input: string;
  baseDir: string;
  sizeBytes: number;
  status: JobStatus;
  stage: string | null;
  stageCurrent: number;
  stageTotal: number | null;
  pages: number | null;
  progress: number;
  output: string | null;
  outputSize: number | null;
  message: string | null;
  errorKind: ErrorKind | null;
  attempts: number;
  warnings: number;
  startedAt: number | null;
  finishedAt: number | null;
  activeMs: number;
}

export type QueueState = "idle" | "running" | "paused";
export type OcrMode = "default" | "skip" | "redo" | "force";
export type OutputMode = "suffix" | "folder" | "replace";
export type ConflictPolicy = "rename" | "overwrite" | "skip";

export interface Settings {
  languages: string[];
  mode: OcrMode;
  outputType: "auto" | "pdfa" | "pdf";
  optimize: number;
  deskew: boolean;
  rotatePages: boolean;
  sidecar: boolean;
  outputMode: OutputMode;
  suffix: string;
  outputDir: string | null;
  conflict: ConflictPolicy;
  parallelFiles: number;
  jobsPerFile: number;
  maxRetries: number;
  stallTimeoutMin: number;
}

export interface Meta {
  state: QueueState;
  fatal: string | null;
  settings: Settings;
}

export interface Snapshot extends Meta {
  jobs: Job[];
}

export interface RuntimeInfo {
  ok: boolean;
  root: string | null;
  error: string | null;
  ocrmypdf: string | null;
  tesseract: string | null;
  languages: string[];
  supportsSuspend: boolean;
}

export interface AddResult {
  added: number;
  duplicates: number;
  ignored: number;
}
