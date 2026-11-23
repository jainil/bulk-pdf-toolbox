//! Locates the bundled OCR runtime (Python + ocrmypdf, Tesseract) and builds
//! the environment the worker processes run with.
//!
//! Ghostscript is deliberately not shipped: pages are rasterized with pdfium
//! (pypdfium2) and PDF/A is produced by pikepdf (see `Settings::ocrmypdf_options`).
//!
//! Layout (produced by `scripts/bundle-runtime-macos.sh` / `-windows.ps1`):
//! ```text
//! runtime/
//!   python/            python-build-standalone with ocrmypdf installed
//!                      (python/bin/python3 on macOS, python/python.exe on Windows)
//!   bin/               tesseract, pngquant (+ their DLLs on Windows)
//!   lib/               relocated dylibs (macOS)
//!   tessdata/          *.traineddata
//! worker/              bpt_ocr_worker.py + progress plugin
//! ```

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone)]
pub struct OcrRuntime {
    pub root: PathBuf,
    pub python: PathBuf,
    pub worker: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub ok: bool,
    pub root: Option<String>,
    pub error: Option<String>,
    pub ocrmypdf: Option<String>,
    pub tesseract: Option<String>,
    pub languages: Vec<String>,
    pub supports_suspend: bool,
}

const PY_REL: &str = if cfg!(windows) {
    "python/python.exe"
} else {
    "python/bin/python3"
};

fn candidate_roots(app: &AppHandle) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("BPT_RUNTIME_DIR") {
        v.push(PathBuf::from(p));
    }
    if let Ok(res) = app.path().resource_dir() {
        v.push(res);
    }
    // `tauri dev`: fall back to the source tree so a fresh checkout works
    // without waiting for resources to be copied next to the binary.
    if cfg!(debug_assertions) {
        v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources"));
    }
    v
}

pub fn locate(app: &AppHandle) -> Result<OcrRuntime, String> {
    let mut tried = Vec::new();
    for base in candidate_roots(app) {
        // BPT_RUNTIME_DIR may point at the runtime dir itself or its parent.
        for root in [base.join("runtime"), base.clone()] {
            let python = root.join(PY_REL);
            if !python.is_file() {
                tried.push(root.display().to_string());
                continue;
            }
            let worker_dir = [
                root.join("worker"),
                base.join("worker"),
                root.join("../worker"),
            ]
            .into_iter()
            .find(|d| d.join("bpt_ocr_worker.py").is_file());
            let Some(worker_dir) = worker_dir else {
                return Err(format!(
                    "OCR worker script not found near {}",
                    root.display()
                ));
            };
            return Ok(OcrRuntime::from_dirs(&root, &worker_dir));
        }
    }
    Err(format!(
        "Bundled OCR runtime not found. Run scripts/bundle-runtime-{}. Looked in: {}",
        if cfg!(windows) { "windows.ps1" } else { "macos.sh" },
        tried.join(", ")
    ))
}

fn build_env(root: &Path) -> Vec<(OsString, OsString)> {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let join = |parts: &[PathBuf]| -> OsString {
        let mut s = OsString::new();
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                s.push(sep);
            }
            s.push(p.as_os_str());
        }
        s
    };

    let mut path_parts = vec![root.join("bin")];
    if cfg!(windows) {
        path_parts.push(root.join("python"));
        path_parts.push(root.join("python").join("Scripts"));
    }
    let mut path = join(&path_parts);
    if let Some(sys) = std::env::var_os("PATH") {
        path.push(sep);
        path.push(sys);
    }

    vec![
        ("PATH".into(), path),
        ("PYTHONHOME".into(), root.join("python").into()),
        ("PYTHONNOUSERSITE".into(), "1".into()),
        // Never write .pyc into the (signed, possibly read-only) app bundle.
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PYTHONUTF8".into(), "1".into()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("TESSDATA_PREFIX".into(), root.join("tessdata").into()),
        // Tesseract's own threading fights with ocrmypdf's page-level parallelism.
        ("OMP_THREAD_LIMIT".into(), "1".into()),
    ]
}

impl OcrRuntime {
    pub fn from_dirs(root: &Path, worker_dir: &Path) -> Self {
        let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        OcrRuntime {
            env: build_env(&root),
            python: root.join(PY_REL),
            worker: worker_dir.join("bpt_ocr_worker.py"),
            root,
        }
    }

    pub fn command(&self, program: &Path) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(program);
        cmd.env_remove("PYTHONPATH");
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    /// Blocking variant of [`command`], for scripts and tests.
    pub fn std_command(&self, program: &Path) -> std::process::Command {
        let mut cmd = std::process::Command::new(program);
        cmd.env_remove("PYTHONPATH");
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd
    }

    pub fn languages(&self) -> Vec<String> {
        let mut langs: Vec<String> = std::fs::read_dir(self.root.join("tessdata"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.strip_suffix(".traineddata").map(str::to_string)
            })
            .filter(|l| l != "osd")
            .collect();
        langs.sort();
        langs
    }

    async fn version_of(&self, program: PathBuf, args: &[&str]) -> Option<String> {
        let out = self.command(&program).args(args).output().await.ok()?;
        let text = String::from_utf8_lossy(if out.stdout.is_empty() {
            &out.stderr
        } else {
            &out.stdout
        })
        .lines()
        .next()?
        .trim()
        .to_string();
        out.status.success().then_some(text)
    }

    pub async fn info(&self) -> RuntimeInfo {
        let tess_name = if cfg!(windows) {
            "tesseract.exe"
        } else {
            "tesseract"
        };
        let (ocr, tess) = tokio::join!(
            self.version_of(
                self.python.clone(),
                &["-c", "import ocrmypdf; print(ocrmypdf.__version__)"]
            ),
            self.version_of(self.root.join("bin").join(tess_name), &["--version"]),
        );
        let missing: Vec<&str> = [("ocrmypdf", &ocr), ("Tesseract", &tess)]
        .iter()
        .filter(|(_, v)| v.is_none())
        .map(|(n, _)| *n)
        .collect();
        RuntimeInfo {
            ok: missing.is_empty(),
            root: Some(self.root.display().to_string()),
            error: (!missing.is_empty()).then(|| format!("Failed to run: {}", missing.join(", "))),
            ocrmypdf: ocr,
            tesseract: tess.map(|t| t.trim_start_matches("tesseract ").to_string()),
            languages: self.languages(),
            supports_suspend: crate::proc::SUPPORTS_SUSPEND,
        }
    }
}

pub fn missing_info(error: String) -> RuntimeInfo {
    RuntimeInfo {
        ok: false,
        root: None,
        error: Some(error),
        ocrmypdf: None,
        tesseract: None,
        languages: vec![],
        supports_suspend: crate::proc::SUPPORTS_SUSPEND,
    }
}
