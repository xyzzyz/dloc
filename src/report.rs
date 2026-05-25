use crate::config::Config;
use crate::count::LineCounts;
use crate::pipeline::PipelineOutput;
use crate::Result;
use serde::Serialize;
use std::io::{self, Write};

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub backend: String,
    pub files_found: usize,
    pub totals: LineCounts,
}

impl RunReport {
    pub fn from_output(output: PipelineOutput) -> Self {
        Self {
            backend: output.backend,
            files_found: output.files_found,
            totals: LineCounts::default(),
        }
    }
}

pub fn write(config: &Config, report: &RunReport) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if config.json {
        serde_json::to_writer_pretty(&mut out, report)?;
        writeln!(out)?;
        return Ok(());
    }

    writeln!(
        out,
        "{:<24} {:>8} {:>8} {:>8} {:>8}",
        "Language", "files", "blank", "comment", "code"
    )?;
    writeln!(
        out,
        "{:<24} {:>8} {:>8} {:>8} {:>8}",
        "SUM:", report.totals.files, report.totals.blank, report.totals.comment, report.totals.code
    )?;
    Ok(())
}
