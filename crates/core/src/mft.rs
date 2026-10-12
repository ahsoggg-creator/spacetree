use crate::error::ScanError;
use crate::progress::Progress;
use crate::tree::Tree;

pub struct MftOutput {
    pub tree: Tree,
    pub errors: u64,
    pub error_samples: Vec<String>,
    pub extra_links: u64,
}
pub fn probe(_: char) -> Result<(), ScanError> {
    Err(ScanError::Unsupported("todo".into()))
}
pub fn scan(_: char, _: &Progress) -> Result<MftOutput, ScanError> {
    Err(ScanError::Unsupported("todo".into()))
}
