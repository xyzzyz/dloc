use crate::Result;
use crate::config::Config;
use crate::count::LineCounts;
use crate::pipeline::{FileCount, PipelineOutput};
use serde::Serialize;
use std::io::{self, Write};

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub backend: String,
    pub files_found: usize,
    pub files_counted: usize,
    pub languages: Vec<LanguageReport>,
    pub files: Vec<FileCount>,
    pub totals: LineCounts,
}

#[derive(Debug, Serialize)]
pub struct LanguageReport {
    pub language: String,
    pub counts: LineCounts,
}

impl RunReport {
    pub fn from_output(output: PipelineOutput) -> Self {
        let mut totals = LineCounts::default();
        let languages = output
            .languages
            .into_iter()
            .map(|(language, counts)| {
                totals.add_assign(counts);
                LanguageReport { language, counts }
            })
            .collect();

        Self {
            backend: output.backend,
            files_found: output.files_found,
            files_counted: output.files_counted,
            languages,
            files: output.files,
            totals,
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
    for language in &report.languages {
        writeln!(
            out,
            "{:<24} {:>8} {:>8} {:>8} {:>8}",
            language.language,
            language.counts.files,
            language.counts.blank,
            language.counts.comment,
            language.counts.code
        )?;
    }
    writeln!(
        out,
        "{:<24} {:>8} {:>8} {:>8} {:>8}",
        "SUM:", report.totals.files, report.totals.blank, report.totals.comment, report.totals.code
    )?;

    if config.by_file && !report.files.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "{:<48} {:<16} {:>8} {:>8} {:>8}",
            "File", "Language", "blank", "comment", "code"
        )?;
        for file in &report.files {
            writeln!(
                out,
                "{:<48} {:<16} {:>8} {:>8} {:>8}",
                file.path, file.language, file.counts.blank, file.counts.comment, file.counts.code
            )?;
        }
    }
    Ok(())
}
