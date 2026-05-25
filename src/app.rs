use crate::config::Config;
use crate::lang::LanguageRegistry;
use crate::pipeline;
use crate::report::RunReport;
use crate::source::FileSystemProvider;
use crate::Result;

pub fn run(config: &Config) -> Result<RunReport> {
    let registry = LanguageRegistry::new();
    let provider = FileSystemProvider::new();
    let sources = provider.enumerate(config)?;
    let output = pipeline::run(config, &registry, sources)?;
    Ok(RunReport::from_output(output))
}
