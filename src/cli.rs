use crate::config::{Config, IoBackendKind};
use crate::{DlocError, Result};
use clap::{Parser, ValueEnum};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread;

#[derive(Debug, Parser)]
#[command(
    name = "dloc",
    version,
    about = "Fast line-of-code counter."
)]
pub struct Args {
    #[arg(value_name = "FILE|DIR", required = true)]
    inputs: Vec<PathBuf>,

    #[arg(long, value_enum, default_value_t = IoBackendArg::Auto)]
    io_backend: IoBackendArg,

    #[arg(short = 'j', long = "threads")]
    threads: Option<usize>,

    #[arg(long)]
    json: bool,

    #[arg(long)]
    by_file: bool,

    #[arg(long)]
    no_recurse: bool,

    #[arg(long)]
    follow_links: bool,

    #[arg(long, default_value_t = 100)]
    max_file_size: u64,

    #[arg(long, value_name = "EXT1,EXT2")]
    include_ext: Option<String>,

    #[arg(long, value_name = "EXT1,EXT2")]
    exclude_ext: Option<String>,

    #[arg(long, value_name = "LANG1,LANG2")]
    include_lang: Option<String>,

    #[arg(long, value_name = "LANG1,LANG2")]
    exclude_lang: Option<String>,

    #[arg(long, value_name = "REGEX")]
    include_content: Option<String>,

    #[arg(long, value_name = "REGEX")]
    exclude_content: Option<String>,

    #[arg(long)]
    skip_uniqueness: bool,

    #[arg(long)]
    quiet: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum IoBackendArg {
    Auto,
    Pread,
    Uring,
}

pub fn parse() -> Result<Config> {
    Args::parse().try_into()
}

impl TryFrom<Args> for Config {
    type Error = DlocError;

    fn try_from(args: Args) -> Result<Self> {
        let threads = args
            .threads
            .unwrap_or_else(|| thread::available_parallelism().map_or(1, usize::from))
            .max(1);

        Ok(Self {
            inputs: args.inputs,
            io_backend: args.io_backend.into(),
            threads,
            json: args.json,
            by_file: args.by_file,
            no_recurse: args.no_recurse,
            follow_links: args.follow_links,
            max_file_size_bytes: args.max_file_size.saturating_mul(1024 * 1024),
            include_ext: parse_csv_set(args.include_ext),
            exclude_ext: parse_csv_set(args.exclude_ext),
            include_lang: parse_csv_set(args.include_lang),
            exclude_lang: parse_csv_set(args.exclude_lang),
            include_content: compile_optional_regex(args.include_content)?,
            exclude_content: compile_optional_regex(args.exclude_content)?,
            skip_uniqueness: args.skip_uniqueness,
            quiet: args.quiet,
        })
    }
}

impl From<IoBackendArg> for IoBackendKind {
    fn from(value: IoBackendArg) -> Self {
        match value {
            IoBackendArg::Auto => Self::Auto,
            IoBackendArg::Pread => Self::Pread,
            IoBackendArg::Uring => Self::Uring,
        }
    }
}

fn compile_optional_regex(pattern: Option<String>) -> Result<Option<Regex>> {
    pattern.map(|pattern| Regex::new(&pattern)).transpose().map_err(Into::into)
}

fn parse_csv_set(value: Option<String>) -> BTreeSet<String> {
    value
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(|item| item.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}
