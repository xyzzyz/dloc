use crate::Result;
use crate::config::IoBackendKind;
use crate::error::DlocError;
use crate::source::SourceMeta;
use std::fs;

pub trait ReadBackend: Send {
    fn name(&self) -> &'static str;
    fn read(&mut self, meta: &SourceMeta) -> Result<Vec<u8>>;
}

pub fn create(kind: IoBackendKind) -> Result<Box<dyn ReadBackend>> {
    match kind {
        IoBackendKind::Auto | IoBackendKind::Pread => Ok(Box::new(PreadBackend)),
        IoBackendKind::Uring => Err(DlocError::message(
            "io_uring backend is not implemented yet; use --io-backend=auto or pread",
        )),
    }
}

#[derive(Debug)]
struct PreadBackend;

impl ReadBackend for PreadBackend {
    fn name(&self) -> &'static str {
        "pread"
    }

    fn read(&mut self, meta: &SourceMeta) -> Result<Vec<u8>> {
        fs::read(&meta.path).map_err(|err| DlocError::io_path(&meta.path, err))
    }
}
