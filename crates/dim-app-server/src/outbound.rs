use crate::protocol::Outbound;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_QUEUED_FRAMES: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROGRESS_FRAMES: usize = 256;
const MAX_PROGRESS_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct Frame {
    pub bytes: Vec<u8>,
    pub written: Option<oneshot::Sender<()>>,
    _slot: OwnedSemaphorePermit,
    _budget: OwnedSemaphorePermit,
}
/// Why a frame was not admitted. A frame over the limit leaves the
/// connection open; one the allowance cannot take makes the client a slow
/// consumer, as any reliable frame does.
pub(crate) enum Admission {
    TooLarge,
    Full,
}
/// The frames and encoded bytes one class of frames may hold until written.
struct Allowance {
    frames: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}
impl Allowance {
    fn new(frames: usize, bytes: usize) -> Self {
        Self {
            frames: Arc::new(Semaphore::new(frames)),
            bytes: Arc::new(Semaphore::new(bytes)),
        }
    }
}
/// A connection's single ordered outbound queue. Replies and notices share
/// one allowance; a call's held notifications have their own, so they never
/// take a reply's room; call progress has its own too.
///
/// ```text
/// send(reply | notice) --> reliable allowance --+--> FIFO queue --> writer
///                          full: close          |
/// hold(notification) ----> held allowance ------+
///                          full: close          |
/// offer(progress) -------> progress allowance --+
///                          full: drop the frame
/// ```
pub(crate) struct Sender {
    queue: mpsc::UnboundedSender<Frame>,
    reliable: Allowance,
    /// The room unfinished calls' held notifications take until they are
    /// queued behind their calls' replies: the same limits as `reliable`,
    /// separate, so a held frame never refuses a reply.
    held: Allowance,
    progress: Allowance,
    closed: CancellationToken,
}
impl Sender {
    pub fn new(closed: CancellationToken) -> (Self, mpsc::UnboundedReceiver<Frame>) {
        let (queue, receiver) = mpsc::unbounded_channel();
        (
            Self {
                queue,
                reliable: Allowance::new(MAX_QUEUED_FRAMES, MAX_OUTPUT_BYTES),
                held: Allowance::new(MAX_QUEUED_FRAMES, MAX_OUTPUT_BYTES),
                progress: Allowance::new(MAX_PROGRESS_FRAMES, MAX_PROGRESS_BYTES),
                closed,
            },
            receiver,
        )
    }
    /// Admission is synchronous: slow consumers close rather than parking
    /// arbitrarily many producers behind a full queue. Durable notices are
    /// never silently dropped while leaving the connection healthy.
    pub fn send(
        &self,
        value: Outbound,
        acknowledge: bool,
    ) -> Result<Option<oneshot::Receiver<()>>, ()> {
        let result = match self.admit(&self.reliable, &value, acknowledge) {
            Ok((frame, receiver)) => self.push(frame).then_some(receiver).ok_or(()),
            Err(_) => Err(()),
        };
        if result.is_err() {
            self.closed.cancel();
        }
        result
    }
    /// Queues call progress (methods contract A.2), which is best effort: a
    /// frame its allowance cannot take, including one over the frame limit,
    /// is dropped and the connection stays open. A dropped frame is never
    /// queued, so it cannot reorder the frames around it.
    pub fn offer(&self, value: Outbound) {
        if let Ok((frame, _)) = self.admit(&self.progress, &value, false) {
            self.push(frame);
        }
    }
    /// Encodes `value` once and takes room in the held allowance now,
    /// without queueing it: a call's held notifications (methods contract
    /// A.2) never take the room a reply is admitted against. A frame the
    /// held allowance cannot take is a slow consumer and closes the
    /// connection; one over the frame limit leaves it open.
    pub fn hold(&self, value: Outbound) -> Result<Frame, Admission> {
        let result = self
            .admit(&self.held, &value, false)
            .map(|(frame, _)| frame);
        if matches!(result, Err(Admission::Full)) {
            self.closed.cancel();
        }
        result
    }
    /// Queues a frame [`hold`](Self::hold) admitted; false when the writer is
    /// gone. The frame is dropped then, releasing its room.
    pub fn push(&self, frame: Frame) -> bool {
        self.queue.send(frame).is_ok()
    }
    /// One encode per frame, under the allowance's byte budget: the frame
    /// limit and the budget refuse before a larger copy is built, and the
    /// bytes are then moved into the frame.
    fn admit(
        &self,
        allowance: &Allowance,
        value: &Outbound,
        acknowledge: bool,
    ) -> Result<(Frame, Option<oneshot::Receiver<()>>), Admission> {
        if self.closed.is_cancelled() {
            return Err(Admission::Full);
        }
        let slot = allowance
            .frames
            .clone()
            .try_acquire_owned()
            .map_err(|_| Admission::Full)?;
        let permit = allowance
            .bytes
            .clone()
            .try_acquire_many_owned(0)
            .map_err(|_| Admission::Full)?;
        let mut bytes = LimitedBytes {
            bytes: Vec::new(),
            budget: allowance.bytes.clone(),
            permit,
        };
        serde_json::to_writer(&mut bytes, value).map_err(|error| match error.io_error_kind() {
            Some(std::io::ErrorKind::InvalidInput) => Admission::TooLarge,
            _ => Admission::Full,
        })?;
        let (written, receiver) = if acknowledge {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        Ok((
            Frame {
                bytes: bytes.bytes,
                written,
                _slot: slot,
                _budget: bytes.permit,
            },
            receiver,
        ))
    }
}

struct LimitedBytes {
    bytes: Vec<u8>,
    budget: Arc<Semaphore>,
    permit: OwnedSemaphorePermit,
}
impl std::io::Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES - self.bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "outbound frame exceeds 16 MiB",
            ));
        }
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(bytes.len() as u32)
            .map_err(|_| std::io::Error::other("connection output budget exhausted"))?;
        self.permit.merge(permit);
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
