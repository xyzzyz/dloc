use crate::Result;
use crate::config::IoBackendKind;
use crate::error::DlocError;
use crate::source::{SourceItem, SourceRef};
use io_uring::{IoUring, opcode, types};
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendSelection {
    kind: SelectedBackendKind,
}

impl BackendSelection {
    pub fn name(self) -> &'static str {
        match self.kind {
            SelectedBackendKind::Pread => "pread",
            SelectedBackendKind::Uring => "io_uring",
        }
    }
}

pub fn create(kind: IoBackendKind) -> Result<Box<dyn ReadBackend>> {
    match kind {
        IoBackendKind::Auto => match UringBackend::new() {
            Ok(backend) => Ok(Box::new(backend)),
            Err(_) => Ok(Box::new(PreadBackend)),
        },
        IoBackendKind::Pread => Ok(Box::new(PreadBackend)),
        IoBackendKind::Uring => Ok(Box::new(UringBackend::new()?)),
    }
}

pub fn create_selected(selection: BackendSelection) -> Result<Box<dyn ReadBackend>> {
    match selection.kind {
        SelectedBackendKind::Pread => Ok(Box::new(PreadBackend)),
        SelectedBackendKind::Uring => Ok(Box::new(UringBackend::new()?)),
    }
}

pub fn select(kind: IoBackendKind) -> Result<BackendSelection> {
    match kind {
        IoBackendKind::Auto => match UringBackend::new() {
            Ok(_) => Ok(BackendSelection {
                kind: SelectedBackendKind::Uring,
            }),
            Err(_) => Ok(BackendSelection {
                kind: SelectedBackendKind::Pread,
            }),
        },
        IoBackendKind::Pread => Ok(BackendSelection {
            kind: SelectedBackendKind::Pread,
        }),
        IoBackendKind::Uring => {
            UringBackend::new()?;
            Ok(BackendSelection {
                kind: SelectedBackendKind::Uring,
            })
        }
    }
}

pub fn name(kind: IoBackendKind) -> Result<&'static str> {
    match kind {
        IoBackendKind::Auto => Ok("auto"),
        IoBackendKind::Pread => Ok("pread"),
        IoBackendKind::Uring => Ok("io_uring"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectedBackendKind {
    Pread,
    Uring,
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

struct UringBackend {
    ring: IoUring,
}

impl UringBackend {
    fn new() -> Result<Self> {
        Ok(Self {
            ring: IoUring::new(IORING_QUEUE_DEPTH as u32)?,
        })
    }

    fn read_chunk(
        &mut self,
        requests: Vec<ReadRequest>,
        emit: &mut dyn FnMut(Result<ReadFile>),
    ) -> Result<()> {
        let mut reads = Vec::with_capacity(requests.len());
        let mut outputs = Vec::with_capacity(requests.len());
        let mut pending = 0;

        for request in requests {
            let SourceRef::Path(path) = &request.item.source;
            let file = match File::open(path) {
                Ok(file) => file,
                Err(err) => {
                    outputs.push(Err(DlocError::io_path(path, err)));
                    continue;
                }
            };
            let size = match file_len(path, &file) {
                Ok(size) => size,
                Err(err) => {
                    outputs.push(Err(err));
                    continue;
                }
            };
            if size == 0 {
                outputs.push(Ok(ReadFile {
                    item: request.item,
                    bytes: Vec::new(),
                }));
                continue;
            }

            let read = InFlightRead {
                item: request.item,
                file,
                buffer: vec![0; size],
                offset: 0,
            };
            reads.push(Some(read));
            let slot = reads.len() - 1;
            let mut read = reads[slot].take().expect("read was just inserted");
            self.submit_read(slot, &mut read)?;
            reads[slot] = Some(read);
            pending += 1;
        }

        if pending == 0 {
            for output in outputs {
                emit(output);
            }
            return Ok(());
        }

        self.submit_pending()?;
        while pending > 0 {
            self.submit_and_wait_one()?;
            let completions = self.drain_completions();

            for completion in completions {
                pending -= 1;
                let slot = completion.user_data as usize;
                let Some(mut read) = reads.get_mut(slot).and_then(Option::take) else {
                    return Err(DlocError::message(
                        "io_uring returned an unknown completion",
                    ));
                };

                if completion.result < 0 {
                    let err = io::Error::from_raw_os_error(-completion.result);
                    outputs.push(Err(io_error_for_item(&read.item, err)));
                    continue;
                }

                let bytes_read = completion.result as usize;
                if bytes_read > read.buffer.len() - read.offset {
                    outputs.push(Err(DlocError::message(
                        "io_uring completed more bytes than requested",
                    )));
                    continue;
                }

                read.offset += bytes_read;
                if bytes_read == 0 || read.offset == read.buffer.len() {
                    read.buffer.truncate(read.offset);
                    outputs.push(Ok(ReadFile {
                        item: read.item,
                        bytes: read.buffer,
                    }));
                    continue;
                }

                self.submit_read(slot, &mut read)?;
                reads[slot] = Some(read);
                pending += 1;
            }

            self.submit_pending()?;
        }

        for output in outputs {
            emit(output);
        }

        Ok(())
    }

    fn submit_read(&mut self, slot: usize, read: &mut InFlightRead) -> Result<()> {
        let len = (read.buffer.len() - read.offset).min(u32::MAX as usize) as u32;
        let entry = opcode::Read::new(
            types::Fd(read.file.as_raw_fd()),
            read.buffer[read.offset..].as_mut_ptr(),
            len,
        )
        .offset(read.offset as u64)
        .build()
        .user_data(slot as u64);

        // The file and buffer live in InFlightRead until the matching CQE is drained.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| DlocError::message("io_uring submission queue is full"))?;
        }

        Ok(())
    }

    fn drain_completions(&mut self) -> Vec<UringCompletion> {
        self.ring
            .completion()
            .map(|completion| UringCompletion {
                user_data: completion.user_data(),
                result: completion.result(),
            })
            .collect()
    }

    fn submit_pending(&self) -> Result<()> {
        loop {
            match self.ring.submit() {
                Ok(_) => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn submit_and_wait_one(&self) -> Result<()> {
        loop {
            match self.ring.submit_and_wait(1) {
                Ok(_) => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
            }
        }
    }
}

impl ReadBackend for UringBackend {
    fn name(&self) -> &'static str {
        "io_uring"
    }

    fn read_many(
        &mut self,
        requests: Vec<ReadRequest>,
        emit: &mut dyn FnMut(Result<ReadFile>),
    ) -> Result<()> {
        let mut chunk = Vec::with_capacity(IORING_QUEUE_DEPTH);
        for request in requests {
            chunk.push(request);
            if chunk.len() == IORING_QUEUE_DEPTH {
                self.read_chunk(std::mem::take(&mut chunk), emit)?;
            }
        }

        if !chunk.is_empty() {
            self.read_chunk(chunk, emit)?;
        }

        Ok(())
    }
}

#[derive(Debug)]
struct InFlightRead {
    item: SourceItem,
    file: File,
    buffer: Vec<u8>,
    offset: usize,
}

#[derive(Debug)]
struct UringCompletion {
    user_data: u64,
    result: i32,
}

fn file_len(path: &Path, file: &File) -> Result<usize> {
    let len = file
        .metadata()
        .map_err(|err| DlocError::io_path(path, err))?
        .len();
    len.try_into().map_err(|_| {
        DlocError::io_path(
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("file is too large to read into memory ({len} bytes)"),
            ),
        )
    })
}

fn io_error_for_item(item: &SourceItem, source: io::Error) -> DlocError {
    match &item.source {
        SourceRef::Path(path) => DlocError::io_path(path, source),
    }
}

const IORING_QUEUE_DEPTH: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SourceItem, SourceRef};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_source(prefix: &str, logical_path: &str, contents: &str) -> (PathBuf, ReadRequest) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{prefix}-{id}"));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("lib.rs");
        fs::write(&path, contents).unwrap();

        let request = ReadRequest {
            item: SourceItem {
                logical_path: logical_path.to_string(),
                size: contents.len() as u64,
                source: SourceRef::Path(path),
            },
        };
        (root, request)
    }

    fn read_one(kind: IoBackendKind, contents: &str) -> Option<(PathBuf, Vec<ReadFile>)> {
        let (root, request) = temp_source("dloc-read-backend-test", "src/lib.rs", contents);
        let mut backend = match create(kind) {
            Ok(backend) => backend,
            Err(_) if kind == IoBackendKind::Uring => return None,
            Err(err) => panic!("{err}"),
        };
        let mut emitted = Vec::new();
        backend
            .read_many(vec![request], &mut |result| emitted.push(result.unwrap()))
            .unwrap();

        Some((root, emitted))
    }

    #[test]
    fn pread_read_many_emits_files() {
        let (root, emitted) = read_one(IoBackendKind::Pread, "fn main() {}\n").unwrap();

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].item.logical_path, "src/lib.rs");
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn auto_read_many_emits_files() {
        let (root, emitted) = read_one(IoBackendKind::Auto, "fn main() {}\n").unwrap();

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uring_read_many_emits_files_when_available() {
        let Some((root, emitted)) = read_one(IoBackendKind::Uring, "fn main() {}\n") else {
            return;
        };

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }
}
