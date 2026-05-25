use crate::config::IoBackendKind;
use crate::source::SourceMeta;
use crate::Result;

pub trait ReadBackend: Send {
    fn name(&self) -> &'static str;
    fn read(&mut self, meta: &SourceMeta) -> Result<Vec<u8>>;
}

pub fn create(kind: IoBackendKind) -> Result<Box<dyn ReadBackend>> {
    match kind {
        IoBackendKind::Auto | IoBackendKind::Pread | IoBackendKind::Uring => {
            Ok(Box::new(PreadBackend))
        }
    }
}

#[derive(Debug)]
struct PreadBackend;

impl ReadBackend for PreadBackend {
    fn name(&self) -> &'static str {
        "pread"
    }

    fn read(&mut self, _meta: &SourceMeta) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}
