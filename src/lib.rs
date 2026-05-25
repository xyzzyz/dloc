pub mod app;
pub mod cli;
pub mod config;
pub mod count;
pub mod error;
pub mod io_backend;
pub mod lang;
pub mod pipeline;
pub mod report;
pub mod source;

pub use error::{DlocError, Result};

pub fn main_entry() -> Result<()> {
    let config = cli::parse()?;
    let report = app::run(&config)?;
    report::write(&config, &report)
}
