use std::path::PathBuf;

/// Errors produced by the core crate.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("could not serialize data for {path}: {message}")]
    Serialize { path: PathBuf, message: String },
    #[error("no suitable data directory could be determined for this platform")]
    NoDataDir,
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;
