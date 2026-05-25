use crate::Result;
use crate::config::Config;
use crate::count::{CounterSet, LineCounts};
use crate::io_backend::{self, ReadFile, ReadRequest};
use crate::lang::LanguageRegistry;
use crate::source::SourceItem;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default)]
pub struct PipelineOutput {
    pub backend: String,
    pub files_found: usize,
    pub files_counted: usize,
    pub languages: BTreeMap<String, LineCounts>,
    pub files: Vec<FileCount>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileCount {
    pub path: String,
    pub language: String,
    pub counts: LineCounts,
}

pub fn run(
    config: &Config,
    registry: &LanguageRegistry,
    sources: Vec<SourceItem>,
) -> Result<PipelineOutput> {
    let files_found = sources.len();
    let mut backend = io_backend::create(config.io_backend)?;
    let backend_name = backend.name().to_string();
    let counters = CounterSet::new()?;
    let mut languages = BTreeMap::new();
    let mut files = Vec::new();

    let requests = sources
        .into_iter()
        .map(|item| ReadRequest { item })
        .collect();
    let mut first_error = None;
    backend.read_many(requests, &mut |read_result| {
        if first_error.is_some() {
            return;
        }
        if let Err(err) = process_read_file(
            config,
            registry,
            &counters,
            &mut languages,
            &mut files,
            read_result,
        ) {
            first_error = Some(err);
        }
    })?;

    if let Some(err) = first_error {
        return Err(err);
    }

    let files_counted = languages.values().map(|counts| counts.files as usize).sum();

    Ok(PipelineOutput {
        backend: backend_name,
        files_found,
        files_counted,
        languages,
        files,
    })
}

fn process_read_file(
    config: &Config,
    registry: &LanguageRegistry,
    counters: &CounterSet,
    languages: &mut BTreeMap<String, LineCounts>,
    files: &mut Vec<FileCount>,
    read_result: Result<ReadFile>,
) -> Result<()> {
    let read_file = read_result?;
    if is_binary(&read_file.bytes) {
        return Ok(());
    }

    let text = String::from_utf8_lossy(&read_file.bytes);
    if let Some(pattern) = &config.include_content
        && !pattern.is_match(&text)
    {
        return Ok(());
    }
    if let Some(pattern) = &config.exclude_content
        && pattern.is_match(&text)
    {
        return Ok(());
    }

    let first_line = text.lines().next();
    let logical_path = Path::new(&read_file.item.logical_path);
    let Some(language) = registry.detect(logical_path, first_line) else {
        return Ok(());
    };

    if !config.include_lang.is_empty() && !config.include_lang.contains(language.normalized_name) {
        return Ok(());
    }
    if config.exclude_lang.contains(language.normalized_name) {
        return Ok(());
    }

    let counts = counters.count(language, &read_file.bytes);
    languages
        .entry(language.name.to_string())
        .or_insert_with(LineCounts::default)
        .add_assign(counts);

    if config.by_file {
        files.push(FileCount {
            path: read_file.item.logical_path,
            language: language.name.to_string(),
            counts,
        });
    }

    Ok(())
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IoBackendKind;
    use crate::source::{SourceItem, SourceRef};
    use regex::Regex;
    use std::collections::BTreeSet;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str, contents: &str) -> (std::path::PathBuf, SourceItem) {
        temp_source(name, name, contents)
    }

    fn temp_source(
        logical_path: &str,
        disk_name: &str,
        contents: &str,
    ) -> (std::path::PathBuf, SourceItem) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dloc-pipeline-test-{id}"));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(disk_name);
        fs::write(&path, contents).unwrap();
        let source = SourceItem {
            logical_path: logical_path.to_string(),
            size: contents.len() as u64,
            source: SourceRef::Path(path),
        };
        (root, source)
    }

    fn test_config() -> Config {
        Config {
            inputs: Vec::new(),
            io_backend: IoBackendKind::Pread,
            threads: 1,
            json: false,
            by_file: true,
            no_recurse: false,
            follow_links: false,
            max_file_size_bytes: 1024 * 1024,
            include_ext: BTreeSet::new(),
            exclude_ext: BTreeSet::new(),
            include_lang: BTreeSet::new(),
            exclude_lang: BTreeSet::new(),
            include_content: None::<Regex>,
            exclude_content: None::<Regex>,
            skip_uniqueness: false,
            quiet: true,
        }
    }

    #[test]
    fn counts_sources_serially() {
        let (root, source) = temp_file("lib.rs", "fn main() {}\n// comment\n\n");
        let output = run(&test_config(), &LanguageRegistry::new(), vec![source]).unwrap();

        assert_eq!(output.files_found, 1);
        assert_eq!(output.files_counted, 1);
        assert_eq!(
            output.languages.get("Rust"),
            Some(&LineCounts {
                files: 1,
                blank: 1,
                comment: 1,
                code: 1,
            })
        );
        assert_eq!(output.files.len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_language_from_logical_path() {
        let (root, source) = temp_source("src/lib.rs", "blob-without-extension", "fn main() {}\n");
        let output = run(&test_config(), &LanguageRegistry::new(), vec![source]).unwrap();

        assert_eq!(output.files_counted, 1);
        assert!(output.languages.contains_key("Rust"));

        fs::remove_dir_all(root).unwrap();
    }
}
