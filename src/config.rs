use regex::Regex;
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IoBackendKind {
    Auto,
    Pread,
    Uring,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub inputs: Vec<PathBuf>,
    pub io_backend: IoBackendKind,
    pub threads: usize,
    pub json: bool,
    pub by_file: bool,
    pub no_recurse: bool,
    pub follow_links: bool,
    pub max_file_size_bytes: u64,
    pub include_ext: BTreeSet<String>,
    pub exclude_ext: BTreeSet<String>,
    pub include_lang: BTreeSet<String>,
    pub exclude_lang: BTreeSet<String>,
    pub include_content: Option<Regex>,
    pub exclude_content: Option<Regex>,
    pub skip_uniqueness: bool,
    pub quiet: bool,
}
