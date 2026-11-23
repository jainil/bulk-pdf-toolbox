//! Where does each job's output go, and how is it committed safely.
//!
//! ocrmypdf always writes to a hidden `.partial` file in the destination
//! folder; only a fully successful run is renamed into place, so a crash,
//! kill or power loss never leaves a truncated PDF under the real name.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{ConflictPolicy, Job, OutputMode, Settings};

pub const PARTIAL_MARKER: &str = ".bpt-partial";

#[derive(Debug, Clone)]
pub struct OutputPlan {
    pub final_path: PathBuf,
    pub partial_path: PathBuf,
    pub sidecar_final: Option<PathBuf>,
    pub sidecar_partial: Option<PathBuf>,
    pub replace_original: bool,
}

#[derive(Debug)]
pub enum PlanError {
    /// Output already exists and the policy says skip.
    Exists(PathBuf),
    Other(String),
}

fn with_ext(p: &Path, ext: &str) -> PathBuf {
    p.with_extension(ext)
}

fn partial_for(final_path: &Path, job_id: u64, ext: &str) -> PathBuf {
    let stem = final_path.file_stem().unwrap_or_default().to_string_lossy();
    final_path.with_file_name(format!(".{stem}.{job_id}{PARTIAL_MARKER}.{ext}"))
}

pub fn is_partial_file(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().contains(PARTIAL_MARKER))
}

fn desired_path(job: &Job, s: &Settings) -> Result<PathBuf, PlanError> {
    let input = &job.input;
    let stem = input
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    match s.output_mode {
        OutputMode::Replace => Ok(input.clone()),
        OutputMode::Suffix => Ok(input.with_file_name(format!("{stem}{}.pdf", s.suffix))),
        OutputMode::Folder => {
            let dir = s
                .output_dir
                .as_ref()
                .ok_or_else(|| PlanError::Other("No output folder selected".into()))?;
            let rel = input
                .strip_prefix(&job.base_dir)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| PathBuf::from(input.file_name().unwrap_or_default()));
            let out = dir.join(rel).with_extension("pdf");
            if out == *input {
                // Output folder == source folder: don't clobber the original.
                return Ok(input.with_file_name(format!("{stem}{}.pdf", s.suffix_or_default())));
            }
            Ok(out)
        }
    }
}

impl Settings {
    fn suffix_or_default(&self) -> &str {
        if self.suffix.is_empty() {
            "_ocr"
        } else {
            &self.suffix
        }
    }
}

/// `taken` contains outputs reserved by jobs currently running, so two inputs
/// that map to the same name never race each other.
pub fn plan(job: &Job, s: &Settings, taken: &HashSet<PathBuf>) -> Result<OutputPlan, PlanError> {
    let desired = desired_path(job, s)?;
    let replace_original = s.output_mode == OutputMode::Replace;

    let final_path = if replace_original {
        desired
    } else {
        let busy = |p: &Path| taken.contains(p) || p.exists() || *p == job.input;
        if !busy(&desired) {
            desired
        } else {
            match s.conflict {
                ConflictPolicy::Overwrite if !taken.contains(&desired) && desired != job.input => {
                    desired
                }
                ConflictPolicy::Skip if !taken.contains(&desired) => {
                    return Err(PlanError::Exists(desired));
                }
                _ => {
                    let stem = desired
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    (1..10_000)
                        .map(|n| desired.with_file_name(format!("{stem} ({n}).pdf")))
                        .find(|p| !busy(p))
                        .ok_or_else(|| {
                            PlanError::Other("Could not find a free output name".into())
                        })?
                }
            }
        }
    };

    let (sidecar_final, sidecar_partial) = if s.sidecar {
        (
            Some(with_ext(&final_path, "txt")),
            Some(partial_for(&final_path, job.id, "txt")),
        )
    } else {
        (None, None)
    };
    Ok(OutputPlan {
        partial_path: partial_for(&final_path, job.id, "pdf"),
        final_path,
        sidecar_final,
        sidecar_partial,
        replace_original,
    })
}

impl OutputPlan {
    pub fn prepare(&self) -> std::io::Result<()> {
        if let Some(parent) = self.final_path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.discard();
        Ok(())
    }

    pub fn discard(&self) {
        let _ = fs::remove_file(&self.partial_path);
        if let Some(p) = &self.sidecar_partial {
            let _ = fs::remove_file(p);
        }
    }

    /// Atomically move the finished partial into place. Returns a note to log
    /// if something unusual happened (e.g. original kept instead of trashed).
    pub fn commit(&self, input: &Path) -> Result<Option<String>, String> {
        let meta = fs::metadata(&self.partial_path)
            .map_err(|e| format!("ocrmypdf reported success but output is missing: {e}"))?;
        if meta.len() == 0 {
            return Err("ocrmypdf produced an empty file".into());
        }
        let mut note = None;
        if self.replace_original
            && input.exists()
            && let Err(e) = trash::delete(input)
        {
            // Never destroy the original: keep it next to the output instead.
            let stem = input.file_stem().unwrap_or_default().to_string_lossy();
            let backup = (0..1000)
                .map(|n| {
                    let suffix = if n == 0 {
                        String::new()
                    } else {
                        format!(" ({n})")
                    };
                    input.with_file_name(format!("{stem}.original{suffix}.pdf"))
                })
                .find(|p| !p.exists())
                .ok_or("Could not back up original")?;
            fs::rename(input, &backup).map_err(|e2| {
                format!("Could not move original to Trash ({e}) or back it up ({e2})")
            })?;
            note = Some(format!(
                "Trash unavailable ({e}); original kept as {}",
                backup.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        fs::rename(&self.partial_path, &self.final_path)
            .map_err(|e| format!("Could not move output into place: {e}"))?;
        if let (Some(from), Some(to)) = (&self.sidecar_partial, &self.sidecar_final)
            && from.exists()
        {
            let _ = fs::rename(from, to);
        }
        Ok(note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(input: &Path, base: &Path) -> Job {
        Job::new(7, input.to_path_buf(), base.to_path_buf(), 0)
    }

    #[test]
    fn suffix_mode_and_rename_on_conflict() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("scan.pdf");
        fs::write(&input, b"%PDF").unwrap();
        let s = Settings::default();
        let p = plan(&job(&input, d.path()), &s, &HashSet::new()).unwrap();
        assert_eq!(p.final_path, d.path().join("scan_ocr.pdf"));
        assert_eq!(p.partial_path, d.path().join(".scan_ocr.7.bpt-partial.pdf"));

        fs::write(d.path().join("scan_ocr.pdf"), b"x").unwrap();
        let p = plan(&job(&input, d.path()), &s, &HashSet::new()).unwrap();
        assert_eq!(p.final_path, d.path().join("scan_ocr (1).pdf"));

        let mut taken = HashSet::new();
        taken.insert(d.path().join("scan_ocr (1).pdf"));
        let p = plan(&job(&input, d.path()), &s, &taken).unwrap();
        assert_eq!(p.final_path, d.path().join("scan_ocr (2).pdf"));
    }

    #[test]
    fn skip_and_overwrite_policies() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("a.pdf");
        fs::write(&input, b"%PDF").unwrap();
        fs::write(d.path().join("a_ocr.pdf"), b"x").unwrap();
        let mut s = Settings {
            conflict: ConflictPolicy::Skip,
            ..Settings::default()
        };
        assert!(matches!(
            plan(&job(&input, d.path()), &s, &HashSet::new()),
            Err(PlanError::Exists(_))
        ));
        s.conflict = ConflictPolicy::Overwrite;
        let p = plan(&job(&input, d.path()), &s, &HashSet::new()).unwrap();
        assert_eq!(p.final_path, d.path().join("a_ocr.pdf"));
    }

    #[test]
    fn folder_mode_mirrors_structure() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let sub = src.path().join("scans/2024");
        fs::create_dir_all(&sub).unwrap();
        let input = sub.join("x.PDF");
        fs::write(&input, b"%PDF").unwrap();
        let s = Settings {
            output_mode: OutputMode::Folder,
            output_dir: Some(out.path().to_path_buf()),
            ..Settings::default()
        };
        let p = plan(&job(&input, src.path()), &s, &HashSet::new()).unwrap();
        assert_eq!(p.final_path, out.path().join("scans/2024/x.pdf"));
    }

    #[test]
    fn folder_mode_same_as_source_never_clobbers() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("a.pdf");
        fs::write(&input, b"%PDF").unwrap();
        let s = Settings {
            output_mode: OutputMode::Folder,
            output_dir: Some(d.path().to_path_buf()),
            ..Settings::default()
        };
        let p = plan(&job(&input, d.path()), &s, &HashSet::new()).unwrap();
        assert_eq!(p.final_path, d.path().join("a_ocr.pdf"));
    }

    #[test]
    fn commit_moves_partial_into_place() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("a.pdf");
        fs::write(&input, b"%PDF").unwrap();
        let p = plan(
            &job(&input, d.path()),
            &Settings::default(),
            &HashSet::new(),
        )
        .unwrap();
        p.prepare().unwrap();
        fs::write(&p.partial_path, b"%PDF-ocr").unwrap();
        p.commit(&input).unwrap();
        assert_eq!(fs::read(&p.final_path).unwrap(), b"%PDF-ocr");
        assert!(!p.partial_path.exists());
        assert!(input.exists());
    }

    #[test]
    fn commit_rejects_missing_or_empty_output() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("a.pdf");
        fs::write(&input, b"%PDF").unwrap();
        let p = plan(
            &job(&input, d.path()),
            &Settings::default(),
            &HashSet::new(),
        )
        .unwrap();
        assert!(p.commit(&input).is_err());
        fs::write(&p.partial_path, b"").unwrap();
        assert!(p.commit(&input).is_err());
        assert!(!p.final_path.exists());
    }
}
