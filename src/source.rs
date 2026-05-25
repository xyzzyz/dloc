use crate::Result;
use crate::config::Config;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SourceMeta {
    pub path: PathBuf,
    pub logical_path: String,
    pub size: u64,
}

#[derive(Debug, Default)]
pub struct FileSystemProvider;

impl FileSystemProvider {
    pub fn new() -> Self {
        Self
    }

    pub fn enumerate(&self, _config: &Config) -> Result<Vec<SourceMeta>> {
        Ok(Vec::new())
    }
}
