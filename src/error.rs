use std::io;
use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, DlocError>;

#[derive(Debug, Error)]
pub enum DlocError {
    #[error("{path}: {source}")]
    PathIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Regex(#[from] regex::Error),
    #[error("worker thread panicked")]
    ThreadPanic,
    #[error("{0}")]
    Message(String),
}

impl DlocError {
    pub fn io_path(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::PathIo {
            path: path.into(),
            source,
        }
    }

    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}
