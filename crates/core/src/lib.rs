//! spacetree core: arena tree model, scanners (parallel walk, NTFS MFT), aggregation, CSV export.

pub mod error;
pub mod export;
pub mod ntfs;
pub mod progress;
pub mod scan;
pub mod stats;
pub mod tree;
pub mod walk;

#[cfg(windows)]
pub mod mft;
#[cfg(windows)]
mod winapi;

pub use error::ScanError;
pub use progress::Progress;
pub use scan::{Backend, ScanOutput, Selection, Used, drive_of_root, scan, select_backend};
pub use tree::{Kind, NodeId, ROOT, Tree};
