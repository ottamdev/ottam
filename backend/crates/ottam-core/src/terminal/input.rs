//! Per-session terminal input: `terminal.write` queues bytes and returns; one
//! writer task per session drains them into the PTY master as it becomes
//! writable. A program that stops reading stdin fills the tty input queue, and
//! that must stall only its own session's input, never the request dispatcher.

use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Bytes a session may hold queued but unwritten. A write that would take the
/// queue past this is refused as busy, unless the queue is empty.
pub(super) const TERMINAL_INPUT_QUEUE_BYTES: usize = 1024 * 1024;

pub(super) enum EnqueueError {
    /// The queue is over budget; the client may retry once it drains.
    Busy,
    /// The writer stopped: the session closed or the PTY write failed.
    Closed,
}

/// A session's input side: the write fd until the first write, then the
/// writer task that owns it.
pub(super) struct SessionInput {
    fd: Option<OwnedFd>,
    writer: Option<TerminalInput>,
}

impl SessionInput {
    pub(super) fn new(fd: OwnedFd) -> Self {
        Self {
            fd: Some(fd),
            writer: None,
        }
    }

    /// The running writer, started on first use. It ends when this input is
    /// dropped, `cancel` fires, or a write fails.
    pub(super) fn writer(
        &mut self,
        cancel: &CancellationToken,
        tasks: &TaskTracker,
        handle: &Handle,
    ) -> io::Result<&TerminalInput> {
        if self.writer.is_none() {
            let fd = self
                .fd
                .take()
                .ok_or_else(|| io::Error::from(ErrorKind::BrokenPipe))?;
            let fd = {
                let _runtime = handle.enter();
                AsyncFd::new(fd)?
            };
            let (sender, receiver) = mpsc::unbounded_channel();
            let queued = Arc::new(AtomicUsize::new(0));
            tasks.spawn_on(
                run_writer(fd, receiver, Arc::clone(&queued), cancel.clone()),
                handle,
            );
            self.writer = Some(TerminalInput { sender, queued });
        }
        Ok(self.writer.as_ref().expect("writer started above"))
    }
}

pub(super) struct TerminalInput {
    sender: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

impl TerminalInput {
    pub(super) fn enqueue(&self, bytes: Vec<u8>) -> Result<(), EnqueueError> {
        let length = bytes.len();
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                (queued == 0 || queued + length <= TERMINAL_INPUT_QUEUE_BYTES)
                    .then_some(queued + length)
            })
            .map_err(|_| EnqueueError::Busy)?;
        self.sender.send(bytes).map_err(|_| {
            self.queued.fetch_sub(length, Ordering::AcqRel);
            EnqueueError::Closed
        })
    }
}

async fn run_writer(
    fd: AsyncFd<OwnedFd>,
    mut receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    cancel: CancellationToken,
) {
    loop {
        let bytes = tokio::select! {
            () = cancel.cancelled() => return,
            bytes = receiver.recv() => match bytes {
                Some(bytes) => bytes,
                None => return,
            },
        };
        let written = tokio::select! {
            () = cancel.cancelled() => return,
            written = write_all(&fd, &bytes) => written,
        };
        queued.fetch_sub(bytes.len(), Ordering::AcqRel);
        if let Err(error) = written {
            tracing::warn!(error = %error, "terminal input write failed; dropping queued input");
            return;
        }
    }
}

async fn write_all(fd: &AsyncFd<OwnedFd>, mut buffer: &[u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        let mut ready = fd.writable().await?;
        let written = ready.try_io(|fd| {
            // SAFETY: `fd` is the session's live, non-blocking PTY master dup;
            // `buffer` is a valid slice of `buffer.len()` bytes.
            let written = unsafe {
                libc::write(
                    fd.as_raw_fd(),
                    buffer.as_ptr() as *const libc::c_void,
                    buffer.len(),
                )
            };
            if written < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(written as usize)
            }
        });
        match written {
            // Not writable after all; readiness was cleared, so wait again.
            Err(_would_block) => continue,
            Ok(Err(error)) if error.kind() == ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error),
            // A zero-byte write would spin forever; treat it as a closed writer.
            Ok(Ok(0)) => return Err(io::Error::from(ErrorKind::WriteZero)),
            Ok(Ok(written)) => buffer = &buffer[written..],
        }
    }
    Ok(())
}
