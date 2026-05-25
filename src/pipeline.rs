use crate::config::Config;
use crate::lang::LanguageRegistry;
use crate::source::SourceMeta;
use crate::Result;

#[derive(Debug, Default)]
pub struct PipelineOutput {
    pub backend: String,
    pub files_found: usize,
}

pub fn run(
    _config: &Config,
    _registry: &LanguageRegistry,
    sources: Vec<SourceMeta>,
) -> Result<PipelineOutput> {
    Ok(PipelineOutput {
        backend: "pread".to_string(),
        files_found: sources.len(),
    })
}
