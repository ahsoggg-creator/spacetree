#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("скан отменён")]
    Cancelled,
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("ошибка разбора NTFS: {0}")]
    Ntfs(String),
}

impl From<std::io::Error> for ScanError {
    fn from(e: std::io::Error) -> Self {
        ScanError::Io(e.to_string())
    }
}
