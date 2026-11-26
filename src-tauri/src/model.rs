use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Paused,
    Done,
    Skipped,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_active(self) -> bool {
        matches!(self, JobStatus::Running | JobStatus::Paused)
    }
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Skipped | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Encrypted,
    InvalidInput,
    MissingInput,
    AlreadyHasText,
    /// Digitally signed; skipped unless the user allows invalidating it.
    DigitallySigned,
    OutputExists,
    OutputError,
    MissingDependency,
    Timeout,
    Crashed,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: u64,
    pub input: PathBuf,
    /// Directory the file was added relative to; used to mirror folder
    /// structure when writing into an output folder.
    pub base_dir: PathBuf,
    pub size_bytes: u64,
    pub status: JobStatus,
    pub stage: Option<String>,
    pub stage_current: f64,
    pub stage_total: Option<f64>,
    pub pages: Option<u32>,
    /// Overall 0..1 estimate across ocrmypdf's stages.
    pub progress: f64,
    pub output: Option<PathBuf>,
    pub output_size: Option<u64>,
    pub message: Option<String>,
    pub error_kind: Option<ErrorKind>,
    pub attempts: u32,
    pub warnings: u32,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    /// Processing time excluding pauses, for the current/last attempt.
    pub active_ms: u64,
}

impl Job {
    pub fn new(id: u64, input: PathBuf, base_dir: PathBuf, size_bytes: u64) -> Self {
        Job {
            id,
            input,
            base_dir,
            size_bytes,
            status: JobStatus::Queued,
            stage: None,
            stage_current: 0.0,
            stage_total: None,
            pages: None,
            progress: 0.0,
            output: None,
            output_size: None,
            message: None,
            error_kind: None,
            attempts: 0,
            warnings: 0,
            started_at: None,
            finished_at: None,
            active_ms: 0,
        }
    }

    /// Put the job back in the queue, forgetting transient run state.
    pub fn reset_to_queued(&mut self) {
        self.status = JobStatus::Queued;
        self.stage = None;
        self.stage_current = 0.0;
        self.stage_total = None;
        self.progress = 0.0;
        self.active_ms = 0;
        self.started_at = None;
        self.finished_at = None;
        self.warnings = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrMode {
    /// Error on pages that already have text (ocrmypdf default).
    Default,
    /// OCR only pages without text.
    Skip,
    /// Replace existing OCR text layers.
    Redo,
    /// Rasterize everything and OCR again.
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Next to the original, with a suffix.
    Suffix,
    /// Into a chosen folder, mirroring the added folder structure.
    Folder,
    /// Replace the original (original goes to the Trash).
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Rename,
    Overwrite,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    // --- OCR
    pub languages: Vec<String>,
    pub mode: OcrMode,
    /// auto | pdfa | pdf
    pub output_type: String,
    pub optimize: u8,
    pub deskew: bool,
    pub rotate_pages: bool,
    pub sidecar: bool,
    /// OCR digitally signed PDFs anyway, invalidating their signatures.
    pub invalidate_signatures: bool,
    // --- Output
    pub output_mode: OutputMode,
    pub suffix: String,
    pub output_dir: Option<PathBuf>,
    pub conflict: ConflictPolicy,
    // --- Engine
    pub parallel_files: u32,
    pub jobs_per_file: u32,
    pub max_retries: u32,
    /// Kill a file if it makes no progress for this many minutes of
    /// (unpaused) processing. 0 disables.
    pub stall_timeout_min: u32,
}

impl Default for Settings {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) as u32;
        // ocrmypdf docs: start with ~sqrt(cpu) processes x sqrt(cpu) jobs.
        let parallel = (cpus as f64).sqrt().round().max(1.0) as u32;
        let per_file = (cpus / parallel).max(1);
        Settings {
            languages: vec!["eng".into()],
            mode: OcrMode::Skip,
            output_type: "auto".into(),
            optimize: 1,
            deskew: false,
            rotate_pages: false,
            sidecar: false,
            invalidate_signatures: false,
            output_mode: OutputMode::Suffix,
            suffix: "_ocr".into(),
            output_dir: None,
            conflict: ConflictPolicy::Rename,
            parallel_files: parallel,
            jobs_per_file: per_file,
            max_retries: 1,
            stall_timeout_min: 15,
        }
    }
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.parallel_files = self.parallel_files.clamp(1, 64);
        self.jobs_per_file = self.jobs_per_file.clamp(1, 64);
        self.optimize = self.optimize.min(3);
        self.max_retries = self.max_retries.min(10);
        if !matches!(self.output_type.as_str(), "auto" | "pdfa" | "pdf") {
            self.output_type = "auto".into();
        }
        self.languages.retain(|l| !l.trim().is_empty());
        if self.languages.is_empty() {
            self.languages.push("eng".into());
        }
        self.suffix = self.suffix.replace(['/', '\\'], "");
        if self.suffix.is_empty() && self.output_mode == OutputMode::Suffix {
            self.suffix = "_ocr".into();
        }
        self
    }

    pub fn validate_for_start(&self) -> Result<(), String> {
        if self.output_mode == OutputMode::Folder {
            match &self.output_dir {
                None => return Err("Choose an output folder first.".into()),
                Some(d) if !d.is_dir() => {
                    return Err(format!("Output folder does not exist: {}", d.display()));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Keyword arguments for `ocrmypdf.ocr()`.
    pub fn ocrmypdf_options(&self, sidecar: Option<&std::path::Path>) -> Value {
        let mode = match self.mode {
            OcrMode::Default => "default",
            OcrMode::Skip => "skip",
            OcrMode::Redo => "redo",
            OcrMode::Force => "force",
        };
        let mut o = json!({
            "language": self.languages,
            "mode": mode,
            "output_type": self.output_type,
            "optimize": self.optimize,
            "jobs": self.jobs_per_file,
            "rotate_pages": self.rotate_pages,
            // Ghostscript is not shipped: rasterize with pdfium and build
            // PDF/A with pikepdf. Auto falls back to a plain PDF when the
            // result can't be validated; strict PDF/A reports a failure.
            "rasterizer": "pypdfium",
            "pdfa_backend": "internal",
        });
        // ocrmypdf rejects --deskew together with --redo-ocr.
        if self.deskew && self.mode != OcrMode::Redo {
            o["deskew"] = json!(true);
        }
        if self.invalidate_signatures {
            o["invalidate_digital_signatures"] = json!(true);
        }
        if let Some(s) = sidecar {
            o["sidecar"] = json!(s);
        }
        o
    }
}
