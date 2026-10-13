use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::ScanError;
use crate::progress::Progress;
use crate::tree::Tree;

/// Backend requested by the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Walk,
    Mft,
}

/// Backend actually used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Used {
    Walk,
    Mft,
}

impl Used {
    pub fn as_str(self) -> &'static str {
        match self {
            Used::Walk => "walk",
            Used::Mft => "mft",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub used: Used,
    pub reason: String,
}

/// Wall-clock (or, where marked, per-thread) time of the phases of a scan, in order.
pub type Timings = Vec<(&'static str, Duration)>;

pub struct ScanOutput {
    pub tree: Tree,
    pub selection: Selection,
    pub errors: u64,
    pub error_samples: Vec<String>,
    pub elapsed: Duration,
    /// Hard-link names that were not counted (MFT only).
    pub extra_links: u64,
    pub timings: Timings,
}

/// Drive letter if `path` is a bare volume root (`C:`, `C:\`, `\?\C:\`).
pub fn drive_of_root(path: &Path) -> Option<char> {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\?\").unwrap_or(&s);
    let mut it = s.chars();
    let letter = it.next().filter(char::is_ascii_alphabetic)?;
    if it.next() != Some(':') {
        return None;
    }
    let rest: String = it.collect();
    matches!(rest.as_str(), "" | "\\" | "/").then(|| letter.to_ascii_uppercase())
}

pub fn select_backend(path: &Path, requested: Backend) -> Result<Selection, ScanError> {
    let sel = |used, reason: &str| {
        Ok(Selection {
            used,
            reason: reason.to_owned(),
        })
    };
    match requested {
        Backend::Walk => sel(Used::Walk, "выбран явно"),
        Backend::Mft => {
            probe_mft(path)?;
            sel(Used::Mft, "выбран явно")
        }
        Backend::Auto => {
            if !cfg!(windows) {
                return sel(Used::Walk, "не Windows: чтение MFT недоступно");
            }
            match probe_mft(path) {
                Ok(()) => sel(Used::Mft, "Windows, NTFS, права администратора есть"),
                Err(e) => Ok(Selection {
                    used: Used::Walk,
                    reason: format!("MFT недоступен: {e}"),
                }),
            }
        }
    }
}

fn probe_mft(path: &Path) -> Result<(), ScanError> {
    #[cfg(windows)]
    {
        let drive = drive_of_root(path)
            .ok_or_else(|| ScanError::Unsupported("MFT читается только для корня тома (например C:\\)".into()))?;
        crate::mft::probe(drive).map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(ScanError::Unsupported("бэкенд mft доступен только в Windows".into()))
    }
}

pub fn scan(path: &Path, requested: Backend, progress: &Progress) -> Result<ScanOutput, ScanError> {
    let selection = select_backend(path, requested)?;
    let start = Instant::now();
    match selection.used {
        Used::Walk => {
            let w = crate::walk::scan(path, progress)?;
            Ok(ScanOutput {
                tree: w.tree,
                selection,
                errors: w.errors,
                error_samples: w.error_samples,
                elapsed: start.elapsed(),
                extra_links: 0,
                timings: w.timings,
            })
        }
        Used::Mft => scan_mft(path, selection, progress, start),
    }
}

#[cfg(windows)]
fn scan_mft(path: &Path, selection: Selection, progress: &Progress, start: Instant) -> Result<ScanOutput, ScanError> {
    let drive = drive_of_root(path).expect("checked by select_backend");
    let m = crate::mft::scan(drive, progress)?;
    Ok(ScanOutput {
        tree: m.tree,
        selection,
        errors: m.errors,
        error_samples: m.error_samples,
        elapsed: start.elapsed(),
        extra_links: m.extra_links,
        timings: m.timings,
    })
}

#[cfg(not(windows))]
fn scan_mft(_: &Path, _: Selection, _: &Progress, _: Instant) -> Result<ScanOutput, ScanError> {
    Err(ScanError::Unsupported("бэкенд mft доступен только в Windows".into()))
}
