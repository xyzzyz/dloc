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

    pub fn io_worker_count(self, worker_count: usize, files_found: usize) -> usize {
        let worker_count = match self.kind {
            SelectedBackendKind::Pread => worker_count,
            SelectedBackendKind::Uring => worker_count.min(max_uring_io_workers_for_fd_limit()),
        };
        worker_count.max(1).min(files_found.max(1))
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
            if let Some(max_size_bytes) = request.item.max_size_bytes {
                let metadata = fs::metadata(path).map_err(|err| DlocError::io_path(path, err))?;
                if metadata.len() > max_size_bytes {
                    continue;
                }
            }

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
                skip: false,
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
            self.submit_and_wait_for(pending_metadata)?;
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
                                Ok(size) => {
                                    if exceeds_size_limit(&file.item, size) {
                                        file.skip = true;
                                    } else {
                                        file.size = Some(size);
                                    }
                                }
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
        }

        let mut next_file = 0;
        while next_file < pending_files.len() {
            let mut pending = 0;
            let mut in_flight_bytes: usize = 0;
            let mut group_files = 0;
            reads.clear();
            close_outputs.clear();

            while next_file < pending_files.len() && group_files < IORING_FILE_BATCH_SIZE {
                let Some(mut file) = pending_files[next_file].take() else {
                    next_file += 1;
                    continue;
                };
                let slot = reads.len();

                if let Some(err) = file.error.take() {
                    reads.push(None);
                    if let Some(fd) = file.fd.take() {
                        set_close_output(&mut close_outputs, slot, CloseOutput::Emit(Err(err)));
                        self.submit_close(slot, fd)?;
                        pending += 1;
                        group_files += 1;
                    } else {
                        outputs.push(Err(err));
                    }
                    next_file += 1;
                    continue;
                }

                let Some(fd) = file.fd.take() else {
                    reads.push(None);
                    outputs.push(Err(DlocError::message(
                        "io_uring open completed without a file descriptor",
                    )));
                    next_file += 1;
                    continue;
                };
                if file.skip {
                    reads.push(None);
                    set_close_output(&mut close_outputs, slot, CloseOutput::Ignore);
                    self.submit_close(slot, fd)?;
                    pending += 1;
                    group_files += 1;
                    next_file += 1;
                    continue;
                }
                let Some(size) = file.size else {
                    reads.push(None);
                    set_close_output(
                        &mut close_outputs,
                        slot,
                        CloseOutput::Emit(Err(DlocError::message(
                            "io_uring statx completed without a size",
                        ))),
                    );
                    self.submit_close(slot, fd)?;
                    pending += 1;
                    group_files += 1;
                    next_file += 1;
                    continue;
                };

                if size == 0 {
                    reads.push(None);
                    set_close_output(
                        &mut close_outputs,
                        slot,
                        CloseOutput::Emit(Ok(ReadFile {
                            item: file.item,
                            bytes: Vec::new(),
                        })),
                    );
                    self.submit_close(slot, fd)?;
                    pending += 1;
                    group_files += 1;
                    next_file += 1;
                    continue;
                }

                if group_files > 0
                    && in_flight_bytes.saturating_add(size) > IORING_MAX_IN_FLIGHT_BYTES
                {
                    file.fd = Some(fd);
                    pending_files[next_file] = Some(file);
                    break;
                }

                let read = InFlightRead {
                    item: file.item,
                    fd,
                    buffer: Vec::with_capacity(size),
                    target_len: size,
                    offset: 0,
                };
                reads.push(Some(read));
                let slot = reads.len() - 1;
                let mut read = reads[slot].take().expect("read was just inserted");
                self.submit_read(slot, &mut read)?;
                reads[slot] = Some(read);
                in_flight_bytes = in_flight_bytes.saturating_add(size);
                pending += 1;
                group_files += 1;
                next_file += 1;
            }

            if pending == 0 {
                continue;
            }

            self.submit_pending()?;
            while pending > 0 {
                self.submit_and_wait_for(pending)?;
                let completions = self.drain_completions()?;
                let mut queued = false;

                for completion in completions {
                    pending -= 1;
                    match completion.operation {
                        UringOperation::Read => {
                            let Some(mut read) =
                                reads.get_mut(completion.slot).and_then(Option::take)
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
                                    CloseOutput::Emit(Err(io_error_for_item(&read.item, err))),
                                );
                                self.submit_close(completion.slot, read.fd)?;
                                queued = true;
                                pending += 1;
                                continue;
                            }

                            let bytes_read = completion.result as usize;
                            if bytes_read > read.target_len - read.offset {
                                set_close_output(
                                    &mut close_outputs,
                                    completion.slot,
                                    CloseOutput::Emit(Err(DlocError::message(
                                        "io_uring completed more bytes than requested",
                                    ))),
                                );
                                self.submit_close(completion.slot, read.fd)?;
                                queued = true;
                                pending += 1;
                                continue;
                            }

                            read.offset += bytes_read;
                            if bytes_read == 0 || read.offset == read.target_len {
                                // The kernel initialized exactly the bytes it reported.
                                unsafe {
                                    read.buffer.set_len(read.offset);
                                }
                                let output = Ok(ReadFile {
                                    item: read.item,
                                    bytes: read.buffer,
                                });
                                set_close_output(
                                    &mut close_outputs,
                                    completion.slot,
                                    CloseOutput::Emit(output),
                                );
                                self.submit_close(completion.slot, read.fd)?;
                                queued = true;
                                pending += 1;
                                continue;
                            }

                            self.submit_read(completion.slot, &mut read)?;
                            queued = true;
                            reads[completion.slot] = Some(read);
                            pending += 1;
                        }
                        UringOperation::Close => {
                            let output = close_outputs
                                .get_mut(completion.slot)
                                .and_then(Option::take)
                                .ok_or_else(|| {
                                    DlocError::message(
                                        "io_uring returned an unknown close completion",
                                    )
                                })?;
                            match output {
                                CloseOutput::Ignore => {}
                                CloseOutput::Emit(output) if completion.result < 0 => {
                                    match output {
                                        Ok(read_file) => outputs.push(Err(io_error_for_item(
                                            &read_file.item,
                                            io::Error::from_raw_os_error(-completion.result),
                                        ))),
                                        Err(err) => outputs.push(Err(err)),
                                    }
                                }
                                CloseOutput::Emit(output) => outputs.push(output),
                            }
                        }
                        UringOperation::Open | UringOperation::Statx => {
                            return Err(DlocError::message(
                                "io_uring returned a metadata completion during reads",
                            ));
                        }
                    }
                }

                if queued {
                    self.submit_pending()?;
                }
            }
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
        let len = (read.target_len - read.offset).min(u32::MAX as usize) as u32;
        // The vector length stays zero until completion; the spare capacity is
        // valid for the kernel to initialize.
        let buffer = unsafe { read.buffer.as_mut_ptr().add(read.offset) };
        let entry = opcode::Read::new(types::Fd(read.fd), buffer, len)
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

    fn submit_and_wait_for(&self, pending: usize) -> Result<()> {
        let wait_for = pending.min(IORING_COMPLETION_WAIT_BATCH).max(1);
        loop {
            match self.ring.submit_and_wait(wait_for) {
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
    skip: bool,
    error: Option<DlocError>,
}

#[derive(Debug)]
struct InFlightRead {
    item: SourceItem,
    fd: RawFd,
    buffer: Vec<u8>,
    target_len: usize,
    offset: usize,
}

#[derive(Debug)]
struct UringCompletion {
    slot: usize,
    operation: UringOperation,
    result: i32,
}

#[derive(Debug)]
enum CloseOutput {
    Emit(Result<ReadFile>),
    Ignore,
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

fn exceeds_size_limit(item: &SourceItem, size: usize) -> bool {
    item.max_size_bytes
        .is_some_and(|max_size_bytes| size as u64 > max_size_bytes)
}

fn set_first_error(error: &mut Option<DlocError>, new_error: DlocError) {
    if error.is_none() {
        *error = Some(new_error);
    }
}

fn set_close_output(
    close_outputs: &mut Vec<Option<CloseOutput>>,
    slot: usize,
    output: CloseOutput,
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

fn max_uring_io_workers_for_fd_limit() -> usize {
    let mut limit = MaybeUninit::<libc::rlimit>::uninit();
    let max_workers = unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) != 0 {
            return MAX_URING_IO_WORKERS;
        }
        let limit = limit.assume_init();
        if limit.rlim_cur == libc::RLIM_INFINITY {
            return MAX_URING_IO_WORKERS;
        }
        usize::try_from(limit.rlim_cur).unwrap_or(usize::MAX)
    };

    max_workers
        .saturating_sub(URING_FD_RESERVE)
        .checked_div(IORING_FILE_BATCH_SIZE)
        .unwrap_or(0)
        .clamp(1, MAX_URING_IO_WORKERS)
}

const IORING_QUEUE_DEPTH: usize = 128;
const IORING_FILE_BATCH_SIZE: usize = IORING_QUEUE_DEPTH / 2;
const IORING_COMPLETION_WAIT_BATCH: usize = IORING_FILE_BATCH_SIZE;
const IORING_MAX_IN_FLIGHT_BYTES: usize = 32 * 1024 * 1024;
const MAX_URING_IO_WORKERS: usize = 2;
const URING_FD_RESERVE: usize = 128;
const URING_OPERATION_BITS: u64 = 2;
const URING_OPERATION_MASK: u64 = (1 << URING_OPERATION_BITS) - 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SourceItem, SourceRef};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_source(
        prefix: &str,
        logical_path: &str,
        contents: &str,
        max_size_bytes: Option<u64>,
    ) -> (PathBuf, ReadRequest) {
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
                max_size_bytes,
                source: SourceRef::Path(path),
            },
        };
        (root, request)
    }

    fn read_one(
        kind: IoBackendKind,
        contents: &str,
        max_size_bytes: Option<u64>,
    ) -> Option<(PathBuf, Vec<ReadFile>)> {
        let (root, request) = temp_source(
            "dloc-read-backend-test",
            "src/lib.rs",
            contents,
            max_size_bytes,
        );
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
        let (root, emitted) = read_one(IoBackendKind::Pread, "fn main() {}\n", None).unwrap();

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].item.logical_path, "src/lib.rs");
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn auto_read_many_emits_files() {
        let (root, emitted) = read_one(IoBackendKind::Auto, "fn main() {}\n", None).unwrap();

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uring_read_many_emits_files_when_available() {
        let Some((root, emitted)) = read_one(IoBackendKind::Uring, "fn main() {}\n", None) else {
            return;
        };

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].bytes, b"fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uring_read_many_emits_empty_files_when_available() {
        let Some((root, emitted)) = read_one(IoBackendKind::Uring, "", None) else {
            return;
        };

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].bytes, b"");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pread_skips_files_over_size_limit() {
        let (root, emitted) = read_one(IoBackendKind::Pread, "fn main() {}\n", Some(1)).unwrap();

        assert!(emitted.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uring_skips_files_over_size_limit_when_available() {
        let Some((root, emitted)) = read_one(IoBackendKind::Uring, "fn main() {}\n", Some(1))
        else {
            return;
        };

        assert!(emitted.is_empty());

        fs::remove_dir_all(root).unwrap();
    }
}
