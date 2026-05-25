use crate::Result;
use crate::config::Config;
use crate::error::DlocError;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceItem {
    pub logical_path: String,
    pub max_size_bytes: Option<u64>,
    pub source: SourceRef,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SourceRef {
    Path(PathBuf),
}

impl SourceRef {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
        }
    }
}

#[derive(Debug, Default)]
pub struct FileSystemProvider;

impl FileSystemProvider {
    pub fn new() -> Self {
        Self
    }

    pub fn enumerate(&self, config: &Config) -> Result<Vec<SourceItem>> {
        let mut sources = Vec::new();
        let mut visited_dirs = BTreeSet::new();

        for input in &config.inputs {
            self.visit_path(input, config, true, &mut visited_dirs, &mut sources)?;
        }

        sources.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
        Ok(sources)
    }

    fn visit_path(
        &self,
        path: &Path,
        config: &Config,
        explicit: bool,
        visited_dirs: &mut BTreeSet<PathBuf>,
        sources: &mut Vec<SourceItem>,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|err| DlocError::io_path(path, err))?;

        if metadata.file_type().is_symlink() {
            return self.visit_symlink(path, config, explicit, visited_dirs, sources);
        }

        if metadata.is_dir() {
            return self.visit_dir(path, config, visited_dirs, sources);
        }

        if metadata.is_file() {
            self.add_file(path, config, explicit, sources);
        }

        Ok(())
    }

    fn visit_symlink(
        &self,
        path: &Path,
        config: &Config,
        explicit: bool,
        visited_dirs: &mut BTreeSet<PathBuf>,
        sources: &mut Vec<SourceItem>,
    ) -> Result<()> {
        let metadata = fs::metadata(path).map_err(|err| DlocError::io_path(path, err))?;

        if metadata.is_file() {
            self.add_file(path, config, explicit, sources);
        } else if metadata.is_dir() && config.follow_links {
            self.visit_dir(path, config, visited_dirs, sources)?;
        }

        Ok(())
    }

    fn visit_dir(
        &self,
        path: &Path,
        config: &Config,
        visited_dirs: &mut BTreeSet<PathBuf>,
        sources: &mut Vec<SourceItem>,
    ) -> Result<()> {
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && is_always_excluded_dir(name)
        {
            return Ok(());
        }

        if config.follow_links
            && let Ok(canonical) = fs::canonicalize(path)
            && !visited_dirs.insert(canonical)
        {
            return Ok(());
        }

        let mut entries = fs::read_dir(path)
            .map_err(|err| DlocError::io_path(path, err))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|err| DlocError::io_path(path, err))?;

        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let entry_path = entry.path();
            let file_name = entry.file_name();
            if let Some(name) = file_name.to_str()
                && is_always_excluded_dir(name)
            {
                continue;
            }

            let file_type = entry
                .file_type()
                .map_err(|err| DlocError::io_path(&entry_path, err))?;

            if file_type.is_symlink() {
                let target = fs::metadata(&entry_path)
                    .map_err(|err| DlocError::io_path(&entry_path, err))?;
                if target.is_file() {
                    self.add_file(&entry_path, config, false, sources);
                } else if target.is_dir() && config.follow_links && !config.no_recurse {
                    self.visit_dir(&entry_path, config, visited_dirs, sources)?;
                }
                continue;
            }

            if file_type.is_dir() {
                if !config.no_recurse {
                    self.visit_dir(&entry_path, config, visited_dirs, sources)?;
                }
            } else if file_type.is_file() {
                self.add_file(&entry_path, config, false, sources);
            }
        }

        Ok(())
    }

    fn add_file(
        &self,
        path: &Path,
        config: &Config,
        explicit: bool,
        sources: &mut Vec<SourceItem>,
    ) {
        if !matches_extension_filter(path, &config.include_ext, true) {
            return;
        }

        if !matches_extension_filter(path, &config.exclude_ext, false) {
            return;
        }

        sources.push(SourceItem {
            logical_path: path.to_string_lossy().into_owned(),
            max_size_bytes: (!explicit).then_some(config.max_file_size_bytes),
            source: SourceRef::Path(path.to_path_buf()),
        });
    }
}

fn matches_extension_filter(path: &Path, filters: &BTreeSet<String>, include_mode: bool) -> bool {
    if filters.is_empty() {
        return true;
    }

    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return !include_mode;
    };
    let filename = filename.to_ascii_lowercase();
    let matched = filters
        .iter()
        .any(|extension| filename == *extension || filename.ends_with(&format!(".{extension}")));

    if include_mode { matched } else { !matched }
}

fn is_always_excluded_dir(name: &str) -> bool {
    matches!(name, ".bzr" | ".cvs" | ".git" | ".hg" | ".svn")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IoBackendKind;
    use regex::Regex;
    use std::collections::BTreeSet;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_config(input: PathBuf) -> Config {
        Config {
            inputs: vec![input],
            io_backend: IoBackendKind::Pread,
            threads: 1,
            json: false,
            by_file: false,
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

    fn temp_tree() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dloc-source-test-{id}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn recursively_discovers_files_and_skips_vcs_dirs() {
        let root = temp_tree();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join(".git/config"), "ignored\n").unwrap();

        let sources = FileSystemProvider::new()
            .enumerate(&test_config(root.clone()))
            .unwrap();

        assert_eq!(sources.len(), 1);
        assert!(sources[0].logical_path.ends_with("src/lib.rs"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_extension_filters() {
        let root = temp_tree();
        fs::write(root.join("keep.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("skip.txt"), "text\n").unwrap();

        let mut config = test_config(root.clone());
        config.include_ext.insert("rs".to_string());

        let sources = FileSystemProvider::new().enumerate(&config).unwrap();

        assert_eq!(sources.len(), 1);
        assert!(sources[0].logical_path.ends_with("keep.rs"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marks_non_explicit_files_with_size_limit() {
        let root = temp_tree();
        fs::write(root.join("lib.rs"), "fn main() {}\n").unwrap();

        let mut config = test_config(root.clone());
        config.max_file_size_bytes = 8;

        let sources = FileSystemProvider::new().enumerate(&config).unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].max_size_bytes, Some(8));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn leaves_explicit_files_without_size_limit() {
        let root = temp_tree();
        let path = root.join("large.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        let mut config = test_config(path);
        config.max_file_size_bytes = 1;

        let sources = FileSystemProvider::new().enumerate(&config).unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].max_size_bytes, None);

        fs::remove_dir_all(root).unwrap();
    }
}
