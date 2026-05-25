use crate::Result;
use crate::config::IoBackendKind;
use crate::error::DlocError;
use crate::source::{SourceItem, SourceRef};
use std::fs;

#[derive(Debug)]
pub struct ReadRequest {
    pub item: SourceItem,
}

#[derive(Debug)]
pub struct ReadFile {
    pub item: SourceItem,
    pub bytes: Vec<u8>,
}

pub trait ReadBackend: Send {
    fn name(&self) -> &'static str;

    fn read_many(
        &mut self,
        requests: Vec<ReadRequest>,
        emit: &mut dyn FnMut(Result<ReadFile>),
    ) -> Result<()>;
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

    fn read_many(
        &mut self,
        requests: Vec<ReadRequest>,
        emit: &mut dyn FnMut(Result<ReadFile>),
    ) -> Result<()> {
        for request in requests {
            let SourceRef::Path(path) = &request.item.source;
            let bytes = fs::read(path).map_err(|err| DlocError::io_path(path, err));
            emit(bytes.map(|bytes| ReadFile {
                item: request.item,
                bytes,
            }));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SourceItem, SourceRef};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pread_read_many_emits_files() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dloc-read-backend-test-{id}"));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("lib.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        let mut backend = create(IoBackendKind::Pread).unwrap();
        let requests = vec![ReadRequest {
            item: SourceItem {
                logical_path: "src/lib.rs".to_string(),
                size: 13,
                source: SourceRef::Path(path),
            },
        }];
        let mut emitted = Vec::new();
        backend
            .read_many(requests, &mut |result| emitted.push(result.unwrap()))
            .unwrap();

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].item.logical_path, "src/lib.rs");
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }
}
