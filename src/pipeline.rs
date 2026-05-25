use crate::Result;
use crate::config::Config;
use crate::count::{CounterSet, LineCounts};
use crate::io_backend::{self, ReadFile, ReadRequest};
use crate::lang::LanguageRegistry;
use crate::source::SourceItem;
use crossbeam_channel::{Receiver, Sender, bounded};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::thread;

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
    let backend = io_backend::create(config.io_backend)?;
    let backend_name = backend.name().to_string();
    drop(backend);

    if sources.is_empty() {
        return Ok(PipelineOutput {
            backend: backend_name,
            files_found,
            files_counted: 0,
            languages: BTreeMap::new(),
            files: Vec::new(),
        });
    }

    let worker_count = config.threads.max(1);
    let channel_bound = (worker_count * 4).max(4);
    let (request_tx, request_rx) = bounded::<ReadRequest>(channel_bound);
    let (read_tx, read_rx) = bounded::<Result<ReadFile>>(channel_bound);
    let (count_tx, count_rx) = bounded::<Result<Option<CountedFile>>>(channel_bound);

    let config = Arc::new(config.clone());
    let registry = Arc::new(registry.clone());
    let io_workers = worker_count.min(files_found.max(1));
    let cpu_workers = worker_count;
    let mut handles = Vec::with_capacity(io_workers + cpu_workers);

    for _ in 0..io_workers {
        let request_rx = request_rx.clone();
        let read_tx = read_tx.clone();
        let config = Arc::clone(&config);
        handles.push(thread::spawn(move || {
            run_io_worker(config, request_rx, read_tx)
        }));
    }
    drop(read_tx);

    for _ in 0..cpu_workers {
        let read_rx = read_rx.clone();
        let count_tx = count_tx.clone();
        let config = Arc::clone(&config);
        let registry = Arc::clone(&registry);
        handles.push(thread::spawn(move || {
            run_count_worker(config, registry, read_rx, count_tx)
        }));
    }
    drop(count_tx);

    for item in sources {
        request_tx
            .send(ReadRequest { item })
            .map_err(|_| crate::DlocError::message("read request channel closed"))?;
    }
    drop(request_tx);

    let (languages, mut files, first_error) = reduce_counts(count_rx);

    let mut join_error = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if join_error.is_none() {
                    join_error = Some(err);
                }
            }
            Err(_) => {
                if join_error.is_none() {
                    join_error = Some(crate::DlocError::ThreadPanic);
                }
            }
        }
    }

    if let Some(err) = first_error.or(join_error) {
        return Err(err);
    }

    files.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.language.cmp(&right.language))
    });
    let files_counted = languages.values().map(|counts| counts.files as usize).sum();

    Ok(PipelineOutput {
        backend: backend_name,
        files_found,
        files_counted,
        languages,
        files,
    })
}

fn run_io_worker(
    config: Arc<Config>,
    request_rx: Receiver<ReadRequest>,
    read_tx: Sender<Result<ReadFile>>,
) -> Result<()> {
    let mut backend = io_backend::create(config.io_backend)?;
    while let Ok(first) = request_rx.recv() {
        let mut batch = vec![first];
        while batch.len() < READ_BATCH_SIZE {
            match request_rx.try_recv() {
                Ok(request) => batch.push(request),
                Err(_) => break,
            }
        }

        let mut send_error = None;
        backend.read_many(batch, &mut |read_result| {
            if send_error.is_some() {
                return;
            }
            if read_tx.send(read_result).is_err() {
                send_error = Some(crate::DlocError::message("read result channel closed"));
            }
        })?;
        if let Some(err) = send_error {
            return Err(err);
        }
    }

    Ok(())
}

fn run_count_worker(
    config: Arc<Config>,
    registry: Arc<LanguageRegistry>,
    read_rx: Receiver<Result<ReadFile>>,
    count_tx: Sender<Result<Option<CountedFile>>>,
) -> Result<()> {
    let counters = CounterSet::new()?;

    for read_result in read_rx {
        let count_result = count_read_file(&config, &registry, &counters, read_result);
        count_tx
            .send(count_result)
            .map_err(|_| crate::DlocError::message("count result channel closed"))?;
    }

    Ok(())
}

fn reduce_counts(
    count_rx: Receiver<Result<Option<CountedFile>>>,
) -> (
    BTreeMap<String, LineCounts>,
    Vec<FileCount>,
    Option<crate::DlocError>,
) {
    let mut languages = BTreeMap::new();
    let mut files = Vec::new();
    let mut first_error = None;

    for count_result in count_rx {
        match count_result {
            Ok(Some(counted)) => {
                languages
                    .entry(counted.language)
                    .or_insert_with(LineCounts::default)
                    .add_assign(counted.counts);
                if let Some(file) = counted.file {
                    files.push(file);
                }
            }
            Ok(None) => {}
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }

    (languages, files, first_error)
}

fn count_read_file(
    config: &Config,
    registry: &LanguageRegistry,
    counters: &CounterSet,
    read_result: Result<ReadFile>,
) -> Result<Option<CountedFile>> {
    let read_file = read_result?;
    if is_binary(&read_file.bytes) {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&read_file.bytes);
    if let Some(pattern) = &config.include_content
        && !pattern.is_match(&text)
    {
        return Ok(None);
    }
    if let Some(pattern) = &config.exclude_content
        && pattern.is_match(&text)
    {
        return Ok(None);
    }

    let first_line = text.lines().next();
    let logical_path = Path::new(&read_file.item.logical_path);
    let Some(language) = registry.detect(logical_path, first_line) else {
        return Ok(None);
    };

    if !config.include_lang.is_empty() && !config.include_lang.contains(language.normalized_name) {
        return Ok(None);
    }
    if config.exclude_lang.contains(language.normalized_name) {
        return Ok(None);
    }

    let counts = counters.count(language, &read_file.bytes);
    let file = if config.by_file {
        Some(FileCount {
            path: read_file.item.logical_path,
            language: language.name.to_string(),
            counts,
        })
    } else {
        None
    };

    Ok(Some(CountedFile {
        language: language.name.to_string(),
        counts,
        file,
    }))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

const READ_BATCH_SIZE: usize = 32;

#[derive(Debug)]
struct CountedFile {
    language: String,
    counts: LineCounts,
    file: Option<FileCount>,
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

    #[test]
    fn parallel_counts_match_single_thread_counts() {
        let (root_a, source_a) = temp_source("a.rs", "a", "fn a() {}\n// a\n\n");
        let (root_b, source_b) = temp_source("b.py", "b", "def b():\n    pass\n# b\n");
        let sources = vec![source_b, source_a];

        let mut single = test_config();
        single.threads = 1;
        let mut parallel = test_config();
        parallel.threads = 4;

        let single_output = run(&single, &LanguageRegistry::new(), sources.clone()).unwrap();
        let parallel_output = run(&parallel, &LanguageRegistry::new(), sources).unwrap();

        assert_eq!(single_output.files_found, parallel_output.files_found);
        assert_eq!(single_output.files_counted, parallel_output.files_counted);
        assert_eq!(single_output.languages, parallel_output.languages);
        assert_eq!(single_output.files, parallel_output.files);

        fs::remove_dir_all(root_a).unwrap();
        fs::remove_dir_all(root_b).unwrap();
    }
}
