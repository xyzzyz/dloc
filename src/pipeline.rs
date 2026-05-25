use crate::Result;
use crate::config::Config;
use crate::count::{CounterSet, LineCounts};
use crate::io_backend;
use crate::lang::LanguageRegistry;
use crate::source::SourceMeta;
use serde::Serialize;
use std::collections::BTreeMap;

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
    sources: Vec<SourceMeta>,
) -> Result<PipelineOutput> {
    let files_found = sources.len();
    let mut backend = io_backend::create(config.io_backend)?;
    let backend_name = backend.name().to_string();
    let counters = CounterSet::new()?;
    let mut languages = BTreeMap::new();
    let mut files = Vec::new();

    for source in sources {
        let bytes = backend.read(&source)?;
        if is_binary(&bytes) {
            continue;
        }

        let text = String::from_utf8_lossy(&bytes);
        if let Some(pattern) = &config.include_content
            && !pattern.is_match(&text)
        {
            continue;
        }
        if let Some(pattern) = &config.exclude_content
            && pattern.is_match(&text)
        {
            continue;
        }

        let first_line = text.lines().next();
        let Some(language) = registry.detect(&source.path, first_line) else {
            continue;
        };

        if !config.include_lang.is_empty()
            && !config.include_lang.contains(language.normalized_name)
        {
            continue;
        }
        if config.exclude_lang.contains(language.normalized_name) {
            continue;
        }

        let counts = counters.count(language, &bytes);
        languages
            .entry(language.name.to_string())
            .or_insert_with(LineCounts::default)
            .add_assign(counts);

        if config.by_file {
            files.push(FileCount {
                path: source.logical_path,
                language: language.name.to_string(),
                counts,
            });
        }
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

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IoBackendKind;
    use crate::source::SourceMeta;
    use regex::Regex;
    use std::collections::BTreeSet;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str, contents: &str) -> (std::path::PathBuf, SourceMeta) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dloc-pipeline-test-{id}"));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        let source = SourceMeta {
            logical_path: path.to_string_lossy().into_owned(),
            path,
            size: contents.len() as u64,
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
}
