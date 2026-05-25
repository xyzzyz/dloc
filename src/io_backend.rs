use crate::Result;
use crate::config::IoBackendKind;
use crate::error::DlocError;
use crate::source::{SourceItem, SourceRef};
use io_uring::{IoUring, opcode, squeue::Entry, types};
use std::ffi::CString;
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
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
        let mut pending_files = Vec::with_capacity(requests.len());
        let mut reads = Vec::with_capacity(requests.len());
        let mut close_outputs = Vec::with_capacity(requests.len());
        let mut outputs = Vec::with_capacity(requests.len());
        let mut pending_metadata = 0;

        for request in requests {
            let SourceRef::Path(path) = &request.item.source;
            let path = match path_to_c_string(path) {
                Ok(path) => path,
                Err(err) => {
                    outputs.push(Err(err));
                    continue;
                }
            };

            let file = PendingFile {
                item: request.item,
                path,
                statx: Box::new(MaybeUninit::uninit()),
                fd: None,
                size: None,
                error: None,
            };
            pending_files.push(Some(file));
            let slot = pending_files.len() - 1;
            let mut file = pending_files[slot].take().expect("file was just inserted");
            self.submit_open(slot, &file)?;
            self.submit_statx(slot, &mut file)?;
            pending_files[slot] = Some(file);
            pending_metadata += 2;
        }

        if pending_metadata > 0 {
            self.submit_pending()?;
        }
        while pending_metadata > 0 {
            self.submit_and_wait_one()?;
            for completion in self.drain_completions()? {
                pending_metadata -= 1;
                let Some(file) = pending_files
                    .get_mut(completion.slot)
                    .and_then(Option::as_mut)
                else {
                    return Err(DlocError::message(
                        "io_uring returned an unknown completion",
                    ));
                };

                match completion.operation {
                    UringOperation::Open => {
                        if completion.result < 0 {
                            set_first_error(
                                &mut file.error,
                                io_error_for_item(
                                    &file.item,
                                    io::Error::from_raw_os_error(-completion.result),
                                ),
                            );
                        } else {
                            file.fd = Some(completion.result);
                        }
                    }
                    UringOperation::Statx => {
                        if completion.result < 0 {
                            set_first_error(
                                &mut file.error,
                                io_error_for_item(
                                    &file.item,
                                    io::Error::from_raw_os_error(-completion.result),
                                ),
                            );
                        } else {
                            match statx_size(&file.item, &file.statx) {
                                Ok(size) => file.size = Some(size),
                                Err(err) => set_first_error(&mut file.error, err),
                            }
                        }
                    }
                    UringOperation::Read | UringOperation::Close => {
                        return Err(DlocError::message(
                            "io_uring returned a data completion during metadata setup",
                        ));
                    }
                }
            }

            self.submit_pending()?;
        }

        let mut pending = 0;
        for file in pending_files {
            let Some(mut file) = file else {
                continue;
            };
            let slot = reads.len();

            if let Some(err) = file.error.take() {
                reads.push(None);
                if let Some(fd) = file.fd.take() {
                    set_close_output(&mut close_outputs, slot, Err(err));
                    self.submit_close(slot, fd)?;
                    pending += 1;
                } else {
                    outputs.push(Err(err));
                }
                continue;
            }

            let Some(fd) = file.fd.take() else {
                reads.push(None);
                outputs.push(Err(DlocError::message(
                    "io_uring open completed without a file descriptor",
                )));
                continue;
            };
            let Some(size) = file.size else {
                reads.push(None);
                set_close_output(
                    &mut close_outputs,
                    slot,
                    Err(DlocError::message(
                        "io_uring statx completed without a size",
                    )),
                );
                self.submit_close(slot, fd)?;
                pending += 1;
                continue;
            };

            if size == 0 {
                reads.push(None);
                set_close_output(
                    &mut close_outputs,
                    slot,
                    Ok(ReadFile {
                        item: file.item,
                        bytes: Vec::new(),
                    }),
                );
                self.submit_close(slot, fd)?;
                pending += 1;
                continue;
            }

            let read = InFlightRead {
                item: file.item,
                fd,
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
            let completions = self.drain_completions()?;

            for completion in completions {
                pending -= 1;
                match completion.operation {
                    UringOperation::Read => {
                        let Some(mut read) = reads.get_mut(completion.slot).and_then(Option::take)
                        else {
                            return Err(DlocError::message(
                                "io_uring returned an unknown read completion",
                            ));
                        };

                        if completion.result < 0 {
                            let err = io::Error::from_raw_os_error(-completion.result);
                            set_close_output(
                                &mut close_outputs,
                                completion.slot,
                                Err(io_error_for_item(&read.item, err)),
                            );
                            self.submit_close(completion.slot, read.fd)?;
                            pending += 1;
                            continue;
                        }

                        let bytes_read = completion.result as usize;
                        if bytes_read > read.buffer.len() - read.offset {
                            set_close_output(
                                &mut close_outputs,
                                completion.slot,
                                Err(DlocError::message(
                                    "io_uring completed more bytes than requested",
                                )),
                            );
                            self.submit_close(completion.slot, read.fd)?;
                            pending += 1;
                            continue;
                        }

                        read.offset += bytes_read;
                        if bytes_read == 0 || read.offset == read.buffer.len() {
                            read.buffer.truncate(read.offset);
                            let output = Ok(ReadFile {
                                item: read.item,
                                bytes: read.buffer,
                            });
                            set_close_output(&mut close_outputs, completion.slot, output);
                            self.submit_close(completion.slot, read.fd)?;
                            pending += 1;
                            continue;
                        }

                        self.submit_read(completion.slot, &mut read)?;
                        reads[completion.slot] = Some(read);
                        pending += 1;
                    }
                    UringOperation::Close => {
                        let output = close_outputs
                            .get_mut(completion.slot)
                            .and_then(Option::take)
                            .ok_or_else(|| {
                                DlocError::message("io_uring returned an unknown close completion")
                            })?;
                        if completion.result < 0 {
                            match output {
                                Ok(read_file) => outputs.push(Err(io_error_for_item(
                                    &read_file.item,
                                    io::Error::from_raw_os_error(-completion.result),
                                ))),
                                Err(err) => outputs.push(Err(err)),
                            }
                        } else {
                            outputs.push(output);
                        }
                    }
                    UringOperation::Open | UringOperation::Statx => {
                        return Err(DlocError::message(
                            "io_uring returned a metadata completion during reads",
                        ));
                    }
                }
            }

            self.submit_pending()?;
        }

        for output in outputs {
            emit(output);
        }

        Ok(())
    }

    fn submit_open(&mut self, slot: usize, file: &PendingFile) -> Result<()> {
        let entry = opcode::OpenAt::new(types::Fd(libc::AT_FDCWD), file.path.as_ptr())
            .flags(libc::O_RDONLY | libc::O_CLOEXEC)
            .build()
            .user_data(encode_user_data(slot, UringOperation::Open));
        self.push_entry(entry)
    }

    fn submit_statx(&mut self, slot: usize, file: &mut PendingFile) -> Result<()> {
        let entry = opcode::Statx::new(
            types::Fd(libc::AT_FDCWD),
            file.path.as_ptr(),
            file.statx.as_mut_ptr().cast::<types::statx>(),
        )
        .mask(libc::STATX_SIZE)
        .build()
        .user_data(encode_user_data(slot, UringOperation::Statx));
        self.push_entry(entry)
    }

    fn submit_read(&mut self, slot: usize, read: &mut InFlightRead) -> Result<()> {
        let len = (read.buffer.len() - read.offset).min(u32::MAX as usize) as u32;
        let entry = opcode::Read::new(
            types::Fd(read.fd),
            read.buffer[read.offset..].as_mut_ptr(),
            len,
        )
        .offset(read.offset as u64)
        .build()
        .user_data(encode_user_data(slot, UringOperation::Read));

        self.push_entry(entry)
    }

    fn submit_close(&mut self, slot: usize, fd: RawFd) -> Result<()> {
        let entry = opcode::Close::new(types::Fd(fd))
            .build()
            .user_data(encode_user_data(slot, UringOperation::Close));
        self.push_entry(entry)
    }

    fn push_entry(&mut self, entry: Entry) -> Result<()> {
        // Pointers inside SQEs point into per-slot state that is kept alive until
        // the matching CQE is drained.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| DlocError::message("io_uring submission queue is full"))?;
        }

        Ok(())
    }

    fn drain_completions(&mut self) -> Result<Vec<UringCompletion>> {
        let mut completions = Vec::new();
        for completion in self.ring.completion() {
            let (slot, operation) = decode_user_data(completion.user_data())?;
            completions.push(UringCompletion {
                slot,
                operation,
                result: completion.result(),
            });
        }
        Ok(completions)
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
        let mut chunk = Vec::with_capacity(IORING_FILE_BATCH_SIZE);
        for request in requests {
            chunk.push(request);
            if chunk.len() == IORING_FILE_BATCH_SIZE {
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
struct PendingFile {
    item: SourceItem,
    path: CString,
    statx: Box<MaybeUninit<libc::statx>>,
    fd: Option<RawFd>,
    size: Option<usize>,
    error: Option<DlocError>,
}

#[derive(Debug)]
struct InFlightRead {
    item: SourceItem,
    fd: RawFd,
    buffer: Vec<u8>,
    offset: usize,
}

#[derive(Debug)]
struct UringCompletion {
    slot: usize,
    operation: UringOperation,
    result: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
enum UringOperation {
    Open = 0,
    Statx = 1,
    Read = 2,
    Close = 3,
}

fn path_to_c_string(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        DlocError::io_path(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"),
        )
    })
}

fn statx_size(item: &SourceItem, statx: &MaybeUninit<libc::statx>) -> Result<usize> {
    let statx = unsafe { statx.assume_init_ref() };
    if statx.stx_mask & libc::STATX_SIZE == 0 {
        return Err(io_error_for_item(
            item,
            io::Error::new(io::ErrorKind::InvalidData, "statx did not return file size"),
        ));
    }

    let len = statx.stx_size;
    len.try_into().map_err(|_| {
        io_error_for_item(
            item,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("file is too large to read into memory ({len} bytes)"),
            ),
        )
    })
}

fn set_first_error(error: &mut Option<DlocError>, new_error: DlocError) {
    if error.is_none() {
        *error = Some(new_error);
    }
}

fn set_close_output(
    close_outputs: &mut Vec<Option<Result<ReadFile>>>,
    slot: usize,
    output: Result<ReadFile>,
) {
    if close_outputs.len() <= slot {
        close_outputs.resize_with(slot + 1, || None);
    }
    close_outputs[slot] = Some(output);
}

fn encode_user_data(slot: usize, operation: UringOperation) -> u64 {
    ((slot as u64) << URING_OPERATION_BITS) | operation as u64
}

fn decode_user_data(user_data: u64) -> Result<(usize, UringOperation)> {
    let operation = match user_data & URING_OPERATION_MASK {
        0 => UringOperation::Open,
        1 => UringOperation::Statx,
        2 => UringOperation::Read,
        3 => UringOperation::Close,
        _ => unreachable!("operation mask is two bits"),
    };
    let slot = (user_data >> URING_OPERATION_BITS)
        .try_into()
        .map_err(|_| {
            DlocError::message("io_uring completion slot does not fit in platform usize")
        })?;
    Ok((slot, operation))
}

fn io_error_for_item(item: &SourceItem, source: io::Error) -> DlocError {
    match &item.source {
        SourceRef::Path(path) => DlocError::io_path(path, source),
    }
}

const IORING_QUEUE_DEPTH: usize = 64;
const IORING_FILE_BATCH_SIZE: usize = IORING_QUEUE_DEPTH / 2;
const URING_OPERATION_BITS: u64 = 2;
const URING_OPERATION_MASK: u64 = (1 << URING_OPERATION_BITS) - 1;

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

    #[test]
    fn uring_read_many_emits_empty_files_when_available() {
        let Some((root, emitted)) = read_one(IoBackendKind::Uring, "") else {
            return;
        };

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].bytes, b"");

        fs::remove_dir_all(root).unwrap();
    }
}
