//! spacetree core: arena tree model, scanners (parallel walk, NTFS MFT), aggregation, CSV export.

pub mod cleanup;
pub mod dupes;
pub mod error;
pub mod export;
pub mod ntfs;
pub mod progress;
pub mod scan;
pub mod snapshot;
pub mod stats;
pub mod tree;
pub mod walk;

#[cfg(windows)]
pub mod carve;
#[cfg(windows)]
pub mod mft;
#[cfg(windows)]
pub mod undelete;
#[cfg(windows)]
mod winapi;

pub use error::ScanError;
pub use progress::Progress;
pub use scan::{Backend, ScanOutput, Selection, Timings, Used, drive_of_root, scan, select_backend};
pub use tree::{Kind, NodeId, ROOT, Tree};

/// `(total, free)` bytes of the volume that contains `path`; `None` when unknown (or not on Windows).
pub fn disk_space(path: &std::path::Path) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        winapi::disk_space(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}
