//! Shared bounded worker: payload transactions never hold main-database handles.
use super::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

const RUN_BUDGET: usize = 32 * 1024 * 1024;
const GLOBAL_BUDGET: usize = 64 * 1024 * 1024;
const MAX_OUTPUTS: usize = 128;
const MAX_JOBS: usize = 4096;
const AGE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OutputId(u64);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    Closed,
    UnknownOutput,
    InvalidPart,
    CaptureLimit,
    QueuePressure,
    InvalidMetadata,
    StorageFull,
    StorageUnavailable,
}

#[cfg(test)]
#[derive(Default, Clone, Debug)]
pub(crate) struct WorkerStats {
    pub max_batch_chunks: usize,
    pub max_batch_raw_bytes: usize,
    pub transactions: u64,
    pub chunks: u64,
    pub last_wait: Option<Duration>,
    pub quota_refreshes: u64,
}

#[derive(Default)]
struct Budget {
    used: usize,
    #[cfg(test)]
    input: usize,
    runs: HashMap<String, usize>,
}

struct Reservation {
    budget: Arc<Mutex<Budget>>,
    run: String,
    bytes: usize,
    #[cfg(test)]
    input: bool,
}
impl Reservation {
    fn acquire(budget: &Arc<Mutex<Budget>>, run: &str, bytes: usize, _input: bool) -> Option<Self> {
        let mut state = budget.lock().unwrap();
        let run_used = state.runs.get(run).copied().unwrap_or(0);
        if bytes > GLOBAL_BUDGET.saturating_sub(state.used)
            || bytes > RUN_BUDGET.saturating_sub(run_used)
        {
            return None;
        }
        state.used += bytes;
        state.runs.insert(run.into(), run_used + bytes);
        #[cfg(test)]
        {
            state.input += if _input { bytes } else { 0 };
            assert!(state.used <= GLOBAL_BUDGET && run_used + bytes <= RUN_BUDGET);
        }
        Some(Self {
            budget: budget.clone(),
            run: run.into(),
            bytes,
            #[cfg(test)]
            input: _input,
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.budget.lock().unwrap();
        state.used -= self.bytes;
        #[cfg(test)]
        {
            state.input -= if self.input { self.bytes } else { 0 };
        }
        let count = state.runs.get_mut(&self.run).expect("reserved run");
        *count -= self.bytes;
        if *count == 0 {
            state.runs.remove(&self.run);
        }
    }
}

struct Input {
    part: usize,
    bytes: Vec<u8>,
    position: usize,
    admitted: Duration,
    _reservation: Reservation,
}
struct Admission {
    key: RevisionKey,
    storage: quota::Reservation,
    metadata: Option<Revision>,
    run: String,
    available_parts: Vec<bool>,
    accepted: usize,
    input: VecDeque<Input>,
    closed: bool,
    cancelled: bool,
    finishing: bool,
    loss: Option<&'static str>,
    completion: Option<oneshot::Sender<Result<Receipt>>>,
    _headroom: Reservation,
}
#[derive(Default)]
struct State {
    outputs: HashMap<OutputId, Admission>,
    order: VecDeque<OutputId>,
    next_id: u64,
    jobs: usize,
    stopping: bool,
    recovering: bool,
}

struct Clock {
    origin: Instant,
    #[cfg(test)]
    manual: Option<Arc<std::sync::atomic::AtomicU64>>,
}
impl Clock {
    fn now(&self) -> Duration {
        #[cfg(test)]
        if let Some(clock) = &self.manual {
            return Duration::from_millis(clock.load(std::sync::atomic::Ordering::SeqCst));
        }
        self.origin.elapsed()
    }
}

struct Shared {
    generation: std::sync::atomic::AtomicU64,
    changed: tokio::sync::Notify,
    state: Mutex<State>,
    wake: Condvar,
    budget: Arc<Mutex<Budget>>,
    storage: Arc<Mutex<quota::Budget>>,
    clock: Clock,
    #[cfg(test)]
    stats: Mutex<WorkerStats>,
    #[cfg(test)]
    gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

pub(crate) struct Worker {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}

impl Shared {
    fn notify_changed(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.changed.notify_waiters();
    }
}

impl Worker {
    pub(crate) fn changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.shared.changed.notified()
    }

    pub(crate) fn notify_changed(&self) {
        self.shared.notify_changed();
    }
    pub(crate) fn change_generation(&self) -> u64 {
        self.shared
            .generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn check_storage_space(&self, extra_bytes: u64) -> Result<()> {
        quota::check_write(&self.shared.storage, extra_bytes)
    }
    pub(crate) fn active_keys(&self) -> Vec<RevisionKey> {
        self.shared
            .state
            .lock()
            .unwrap()
            .outputs
            .values()
            .map(|e| e.key.clone())
            .collect()
    }
    pub(crate) fn open_blocking(home: PathBuf) -> Result<Self> {
        Self::start(
            home,
            Clock {
                origin: Instant::now(),
                #[cfg(test)]
                manual: None,
            },
        )
    }

    pub(crate) fn contains(&self, id: OutputId) -> bool {
        self.shared.state.lock().unwrap().outputs.contains_key(&id)
    }

    pub(crate) fn mark_loss(&self, id: OutputId, reason: &'static str) {
        if let Some(entry) = self.shared.state.lock().unwrap().outputs.get_mut(&id) {
            entry.loss.get_or_insert(reason);
        }
    }

    #[cfg(test)]
    pub(crate) fn block_next_write(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered, wait) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        *self.shared.gate.lock().unwrap() = Some((entered, blocked));
        (wait, release)
    }
    #[cfg(test)]
    pub(crate) async fn open(home: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            Self::start(
                home,
                Clock {
                    origin: Instant::now(),
                    #[cfg(test)]
                    manual: None,
                },
            )
        })
        .await?
    }

    fn start(home: PathBuf, clock: Clock) -> Result<Self> {
        let shared = Arc::new(Shared {
            generation: std::sync::atomic::AtomicU64::new(0),
            changed: tokio::sync::Notify::new(),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            budget: Arc::new(Mutex::new(Budget::default())),
            storage: Arc::new(Mutex::new(quota::Budget::default())),
            clock,
            #[cfg(test)]
            stats: Mutex::new(WorkerStats::default()),
            #[cfg(test)]
            gate: Mutex::new(None),
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let actor_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("payload-storage".into())
            .spawn(move || {
                let setup = Repository::open(&home).and_then(|mut repo| {
                    quota::refresh(&mut repo, &home, &actor_shared.storage)?;
                    Ok((repo, zstd::bulk::Compressor::new(1)?))
                });
                let (repository, compressor) = match setup {
                    Ok(setup) => {
                        let _ = ready_tx.send(Ok(()));
                        setup
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return Err(invalid("payload open failed"));
                    }
                };
                let mut actor = Actor {
                    home,
                    shared: actor_shared,
                    repository: Some(repository),
                    compressor,
                    outputs: HashMap::new(),
                    pending: Vec::with_capacity(BATCH),
                    pending_since: None,
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    actor.run_resilient()
                }))
                .unwrap_or_else(|_| Err(invalid("payload worker panicked")));
                if let Err(error) = &result {
                    tracing::warn!(error = %error, "payload worker stopped after panic");
                    actor.abandon_all(&error.to_string(), true);
                }
                result
            })?;
        match ready_rx.recv()? {
            Ok(()) => Ok(Self {
                shared,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }

    pub(crate) fn begin(
        &self,
        metadata: Revision,
    ) -> std::result::Result<OutputId, AdmissionError> {
        metadata
            .validate()
            .map_err(|_| AdmissionError::InvalidMetadata)?;
        let mut state = self.shared.state.lock().unwrap();
        if state.stopping {
            return Err(AdmissionError::Closed);
        }
        if state.recovering {
            return Err(AdmissionError::StorageUnavailable);
        }
        if state.outputs.len() >= MAX_OUTPUTS {
            return Err(AdmissionError::QueuePressure);
        }
        // Reserve all partial buffers, a worst-case full batch owned by this output,
        // compression destination and validation scratch BEFORE admitting any input.
        // Headroom remains charged even when unused; this trades concurrency for a
        // simple progress guarantee at the queue limit.
        let bytes = metadata.parts.len() * PAGE
            + BATCH * (PAGE + 32)
            + zstd::zstd_safe::compress_bound(PAGE)
            + PAGE;
        let reservation = Reservation::acquire(&self.shared.budget, &metadata.run_id, bytes, false)
            .ok_or(AdmissionError::QueuePressure)?;
        let storage = quota::Reservation::new(self.shared.storage.clone(), &metadata)
            .ok_or(AdmissionError::StorageFull)?;
        state.next_id += 1;
        let id = OutputId(state.next_id);
        state.outputs.insert(
            id,
            Admission {
                key: RevisionKey {
                    workspace_id: metadata.workspace_id.clone(),
                    conversation_id: metadata.conversation_id.clone(),
                    item_id: metadata.item_id.clone(),
                    payload_id: metadata.payload_id.clone(),
                    revision: metadata.revision,
                },
                storage,
                run: metadata.run_id.clone(),
                available_parts: metadata
                    .parts
                    .iter()
                    .map(|p| p.unavailable_reason.is_none())
                    .collect(),
                metadata: Some(metadata),
                accepted: 0,
                input: VecDeque::new(),
                closed: false,
                cancelled: false,
                finishing: false,
                loss: None,
                completion: None,
                _headroom: reservation,
            },
        );
        state.order.push_back(id);
        self.shared.wake.notify_one();
        Ok(id)
    }

    /// Ownership transfers only on success; a rejected Vec is dropped immediately.
    /// Queue/capture rejection also marks this revision's eventual receipt as lossy.
    pub(crate) fn enqueue(
        &self,
        id: OutputId,
        part: usize,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), AdmissionError> {
        let mut state = self.shared.state.lock().unwrap();
        if state.stopping {
            return Err(AdmissionError::Closed);
        }
        if state.recovering {
            return Err(AdmissionError::StorageUnavailable);
        }
        let full = state.jobs >= MAX_JOBS;
        let entry = state
            .outputs
            .get_mut(&id)
            .ok_or(AdmissionError::UnknownOutput)?;
        if entry.closed {
            return Err(AdmissionError::Closed);
        }
        if !entry.available_parts.get(part).copied().unwrap_or(false) {
            return Err(AdmissionError::InvalidPart);
        }
        if bytes.len() > CAPTURE_LIMIT - entry.accepted {
            entry.loss.get_or_insert("captureLimit");
            return Err(AdmissionError::CaptureLimit);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        if full {
            entry.loss.get_or_insert("queueFull");
            return Err(AdmissionError::QueuePressure);
        }
        let Some(reservation) =
            Reservation::acquire(&self.shared.budget, &entry.run, bytes.capacity(), true)
        else {
            entry.loss.get_or_insert("queueFull");
            return Err(AdmissionError::QueuePressure);
        };
        if !entry
            .storage
            .add(bytes.len() as u64 + bytes.len().div_ceil(PAGE) as u64 * 256)
        {
            entry.loss.get_or_insert("storageFull");
            return Err(AdmissionError::StorageFull);
        }
        entry.accepted += bytes.len();
        entry.input.push_back(Input {
            part,
            bytes,
            position: 0,
            admitted: self.shared.clock.now(),
            _reservation: reservation,
        });
        state.jobs += 1;
        self.shared.wake.notify_one();
        Ok(())
    }

    pub(crate) fn complete(
        &self,
        id: OutputId,
    ) -> std::result::Result<oneshot::Receiver<Result<Receipt>>, AdmissionError> {
        let mut state = self.shared.state.lock().unwrap();
        if state.stopping {
            return Err(AdmissionError::Closed);
        }
        let entry = state
            .outputs
            .get_mut(&id)
            .ok_or(AdmissionError::UnknownOutput)?;
        if entry.closed {
            return Err(AdmissionError::Closed);
        }
        let (tx, rx) = oneshot::channel();
        entry.closed = true;
        entry.completion = Some(tx);
        self.shared.wake.notify_one();
        Ok(rx)
    }

    pub(crate) fn cancel(&self, id: OutputId) -> std::result::Result<(), AdmissionError> {
        let mut state = self.shared.state.lock().unwrap();
        let entry = state
            .outputs
            .get_mut(&id)
            .ok_or(AdmissionError::UnknownOutput)?;
        if entry.finishing {
            return Err(AdmissionError::Closed);
        }
        entry.cancelled = true;
        entry.closed = true;
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Stop after the in-flight operation. Unacknowledged captures may be lost.
    #[cfg(test)]
    pub(crate) async fn shutdown(mut self) -> Result<()> {
        self.stop();
        let thread = self.thread.take().expect("worker thread");
        tokio::task::spawn_blocking(move || {
            thread
                .join()
                .map_err(|_| invalid("payload worker panicked"))?
        })
        .await?
    }

    fn stop(&self) {
        let mut state = self.shared.state.lock().unwrap();
        state.stopping = true;
        self.shared.wake.notify_one();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct BufferedPart {
    id: i64,
    buffer: Vec<u8>,
    since: Option<Duration>,
    length: u64,
    index: i64,
    hash: Sha256,
}
struct Output {
    receipt: Receipt,
    parts: Vec<BufferedPart>,
    compressed: Vec<u8>,
}

enum Action {
    Begin(OutputId, Revision),
    Data(OutputId, Input),
    Finish(OutputId),
    Cancel(OutputId),
}

struct Actor {
    home: PathBuf,
    shared: Arc<Shared>,
    repository: Option<Repository>,
    compressor: zstd::bulk::Compressor<'static>,
    outputs: HashMap<OutputId, Output>,
    pending: Vec<Chunk>,
    pending_since: Option<Duration>,
}
impl Actor {
    fn run_resilient(&mut self) -> Result<()> {
        loop {
            match self.run() {
                Ok(()) => return Ok(()),
                Err(error) => {
                    // Batches may span captures. Fail the currently admitted set
                    // explicitly, release its reservations, and never replay bytes
                    // whose transaction outcome could be uncertain.
                    tracing::warn!(error = %error, "payload captures failed; reopening storage");
                    self.abandon_all(&error.to_string(), false);
                    if !self.recover() {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn recover(&mut self) -> bool {
        // Drop even a broken transaction/connection before trying to reopen;
        // otherwise its locks could prevent its own replacement from succeeding.
        drop(self.repository.take());
        loop {
            if self.shared.state.lock().unwrap().stopping {
                return false;
            }
            let reopened = Repository::open(&self.home).and_then(|mut repository| {
                quota::refresh(&mut repository, &self.home, &self.shared.storage)?;
                Ok(repository)
            });
            if let Ok(repository) = reopened {
                self.repository = Some(repository);
                let mut state = self.shared.state.lock().unwrap();
                if state.stopping {
                    return false;
                }
                state.recovering = false;
                self.shared.notify_changed();
                tracing::info!("payload storage reopened; capture available");
                return true;
            }
            // Bound retry frequency and let shutdown interrupt recovery waits.
            let state = self.shared.state.lock().unwrap();
            if state.stopping {
                return false;
            }
            drop(self.shared.wake.wait_timeout(state, AGE).unwrap());
        }
    }

    fn run(&mut self) -> Result<()> {
        loop {
            if self.shared.state.lock().unwrap().stopping {
                self.abandon_all("payload worker stopped", true);
                return Ok(());
            }
            self.flush_due()?;
            #[cfg(test)]
            self.observe();
            let action = {
                let mut state = self.shared.state.lock().unwrap();
                if state.stopping {
                    continue;
                }
                let mut action = None;
                for _ in 0..state.order.len() {
                    let id = state.order.pop_front().expect("round robin");
                    state.order.push_back(id);
                    let entry = state.outputs.get_mut(&id).expect("active output");
                    action = if entry.cancelled {
                        Some(Action::Cancel(id))
                    } else if let Some(metadata) = entry.metadata.take() {
                        Some(Action::Begin(id, metadata))
                    } else if let Some(input) = entry.input.pop_front() {
                        Some(Action::Data(id, input))
                    } else if entry.closed {
                        entry.finishing = true;
                        Some(Action::Finish(id))
                    } else {
                        None
                    };
                    if action.is_some() {
                        break;
                    }
                }
                if action.is_none() {
                    let wait = self
                        .deadline()
                        .map(|deadline| deadline.saturating_sub(self.shared.clock.now()));
                    #[cfg(test)]
                    {
                        self.shared.stats.lock().unwrap().last_wait =
                            Some(wait.unwrap_or(Duration::MAX));
                    }
                    if let Some(wait) = wait {
                        drop(self.shared.wake.wait_timeout(state, wait).unwrap());
                    } else {
                        drop(self.shared.wake.wait(state).unwrap());
                    }
                    continue;
                }
                action.expect("selected")
            };
            #[cfg(test)]
            {
                let gate = self.shared.gate.lock().unwrap().take();
                if let Some((entered, release)) = gate {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
            }
            match action {
                Action::Begin(id, metadata) => {
                    quota::check_write(&self.shared.storage, 0)?;
                    let receipt = self
                        .repository
                        .as_mut()
                        .expect("connected")
                        .create_revision(&metadata)?;
                    let parts = receipt
                        .part_ids
                        .iter()
                        .map(|id| BufferedPart {
                            id: *id,
                            buffer: Vec::with_capacity(PAGE),
                            since: None,
                            length: 0,
                            index: 0,
                            hash: Sha256::new(),
                        })
                        .collect();
                    self.outputs.insert(
                        id,
                        Output {
                            receipt,
                            parts,
                            compressed: vec![0; zstd::zstd_safe::compress_bound(PAGE)],
                        },
                    );
                    self.shared.notify_changed();
                }
                Action::Data(id, mut input) => {
                    let part = &mut self.outputs.get_mut(&id).expect("started").parts[input.part];
                    let take = (input.bytes.len() - input.position).min(PAGE - part.buffer.len());
                    let bytes = &input.bytes[input.position..input.position + take];
                    part.buffer.extend_from_slice(bytes);
                    part.hash.update(bytes);
                    part.length += take as u64;
                    part.since.get_or_insert(input.admitted);
                    input.position += take;
                    let full = part.buffer.len() == PAGE;
                    if full {
                        self.encode(id, input.part)?;
                    }
                    #[cfg(test)]
                    self.observe();
                    let mut state = self.shared.state.lock().unwrap();
                    let entry = state.outputs.get_mut(&id).expect("admitted");
                    if input.position < input.bytes.len() && !entry.cancelled {
                        entry.input.push_front(input);
                    } else {
                        state.jobs -= 1;
                    }
                }
                Action::Finish(id) => {
                    for part in 0..self.outputs[&id].parts.len() {
                        self.encode(id, part)?;
                    }
                    self.flush_batch()?;
                    let output = self.outputs.get(&id).expect("started");
                    let manifests: Vec<_> = output
                        .parts
                        .iter()
                        .map(|p| FinalPart {
                            id: p.id,
                            length: p.length,
                            digest: p.hash.clone().finalize().into(),
                        })
                        .collect();
                    let loss = self.shared.state.lock().unwrap().outputs[&id].loss;
                    self.repository
                        .as_mut()
                        .expect("connected")
                        .seal_with_loss(output.receipt.revision_id, &manifests, loss)?;
                    let mut output = self.outputs.remove(&id).expect("started");
                    output.receipt.capture_reason = loss.map(str::to_owned);
                    drop(output.parts);
                    drop(output.compressed);
                    self.retire(id, Ok(output.receipt));
                }
                Action::Cancel(id) => {
                    // Flush other outputs before releasing any batch reservations.
                    // The cancelled staging revision is then explicitly deleted.
                    self.flush_batch()?;
                    if let Some(output) = self.outputs.remove(&id) {
                        self.repository
                            .as_mut()
                            .expect("connected")
                            .delete(output.receipt.revision_id)?;
                    }
                    self.retire(id, Err(invalid("capture cancelled")));
                }
            }
            #[cfg(test)]
            self.observe();
        }
    }

    fn encode(&mut self, id: OutputId, index: usize) -> Result<()> {
        let output = self.outputs.get_mut(&id).expect("started");
        let part = &mut output.parts[index];
        if part.buffer.is_empty() {
            return Ok(());
        }
        let written = self
            .compressor
            .compress_to_buffer(&part.buffer, &mut output.compressed[..])?;
        let saving = part.buffer.len().saturating_sub(written);
        let compressed = saving >= 64 && saving * 10 >= part.buffer.len();
        let bytes = if compressed {
            &output.compressed[..written]
        } else {
            &part.buffer[..]
        };
        self.pending.push(Chunk {
            part_id: part.id,
            chunk_index: part.index,
            raw_offset: (part.length - part.buffer.len() as u64) as i64,
            raw_length: part.buffer.len() as i64,
            codec: if compressed { "zstd-v1" } else { "raw-v1" }.into(),
            raw_sha256: Sha256::digest(&part.buffer).to_vec(),
            encoded_bytes: bytes.to_vec(),
        });
        part.index += 1;
        let since = part.since.take().expect("buffer age");
        self.pending_since = Some(self.pending_since.map_or(since, |prior| prior.min(since)));
        part.buffer.clear();
        #[cfg(test)]
        self.observe();
        if self.pending.len() == BATCH {
            self.flush_batch()?;
        }
        Ok(())
    }

    fn deadline(&self) -> Option<Duration> {
        self.outputs
            .values()
            .flat_map(|o| &o.parts)
            .filter_map(|p| p.since)
            .chain(self.pending_since)
            .min()
            .map(|since| since + AGE)
    }

    fn flush_due(&mut self) -> Result<()> {
        let now = self.shared.clock.now();
        let mut due: Vec<_> = self
            .outputs
            .iter()
            .flat_map(|(id, output)| {
                output
                    .parts
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, part)| {
                        part.since
                            .filter(|since| now.saturating_sub(*since) >= AGE)
                            .map(|_| (*id, index))
                    })
            })
            .collect();
        // Stable ordering makes equal-deadline service reproducible in tests.
        due.sort_by_key(|(id, part)| (id.0, *part));
        for (id, part) in due {
            self.encode(id, part)?;
        }
        if self
            .pending_since
            .is_some_and(|since| now.saturating_sub(since) >= AGE)
        {
            self.flush_batch()?;
        }
        Ok(())
    }

    fn flush_batch(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        quota::check_write(&self.shared.storage, 0)?;
        #[cfg(test)]
        self.observe();
        self.repository
            .as_mut()
            .expect("connected")
            .append(&self.pending)?;
        #[cfg(test)]
        {
            let mut stats = self.shared.stats.lock().unwrap();
            stats.max_batch_chunks = stats.max_batch_chunks.max(self.pending.len());
            stats.max_batch_raw_bytes = stats
                .max_batch_raw_bytes
                .max(self.pending.iter().map(|c| c.raw_length as usize).sum());
        }
        self.pending_since = None;
        self.pending.clear();
        self.shared.notify_changed();
        Ok(())
    }

    fn retire(&self, id: OutputId, result: Result<Receipt>) {
        let mut state = self.shared.state.lock().unwrap();
        let mut entry = state.outputs.remove(&id).expect("active");
        state.jobs -= entry.input.len();
        state.order.retain(|other| *other != id);
        // Reservation release precedes acknowledgement, including queued cancellation bytes.
        let completion = entry.completion.take();
        drop(entry);
        if let Some(completion) = completion {
            let _ = completion.send(result);
        }
        self.shared.notify_changed();
    }

    #[cfg(test)]
    fn observe(&self) {
        let budget = self.shared.budget.lock().unwrap();
        let owned = budget.input
            + self
                .outputs
                .values()
                .map(|o| {
                    o.compressed.capacity()
                        + o.parts.iter().map(|p| p.buffer.capacity()).sum::<usize>()
                })
                .sum::<usize>()
            + self
                .pending
                .iter()
                .map(|c| c.encoded_bytes.capacity() + c.raw_sha256.capacity())
                .sum::<usize>();
        assert!(
            owned <= budget.used,
            "every owned byte buffer must be reserved"
        );
        let mut stats = self.shared.stats.lock().unwrap();
        let repository = self.repository.as_ref().expect("connected");
        stats.transactions = repository.metrics.transactions;
        stats.chunks = repository.metrics.chunks;
    }

    fn abandon_all(&mut self, message: &str, stopping: bool) {
        // No writes on failure/shutdown. Staging rows remain unreadable and can
        // be reclaimed later; missing content can be recovered from session logs.
        self.pending.clear();
        self.pending_since = None;
        self.outputs.clear();
        let mut state = self.shared.state.lock().unwrap();
        state.stopping |= stopping;
        state.recovering = !state.stopping;
        for (_, mut entry) in state.outputs.drain() {
            let completion = entry.completion.take();
            drop(entry);
            if let Some(completion) = completion {
                let _ = completion.send(Err(invalid(message)));
            }
        }
        state.jobs = 0;
        state.order.clear();
        self.shared.notify_changed();
    }
}

#[cfg(test)]
pub(super) mod testing {
    use super::*;
    pub(in crate::storage::payload) fn start(
        home: &std::path::Path,
    ) -> (Worker, Arc<std::sync::atomic::AtomicU64>) {
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker = Worker::start(
            home.to_owned(),
            Clock {
                origin: Instant::now(),
                manual: Some(clock.clone()),
            },
        )
        .unwrap();
        (worker, clock)
    }
    pub(in crate::storage::payload) fn advance(
        worker: &Worker,
        clock: &std::sync::atomic::AtomicU64,
        millis: u64,
    ) {
        clock.fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
        let _state = worker.shared.state.lock().unwrap();
        worker.shared.wake.notify_one();
    }
    pub(in crate::storage::payload) fn gate(
        worker: &Worker,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *worker.shared.gate.lock().unwrap() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }
    pub(in crate::storage::payload) fn used(worker: &Worker) -> usize {
        worker.shared.budget.lock().unwrap().used
    }
    pub(in crate::storage::payload) fn stop(worker: &Worker) {
        worker.stop();
    }
    pub(in crate::storage::payload) fn shutdown(mut worker: Worker) {
        worker.stop();
        worker.thread.take().unwrap().join().unwrap().unwrap();
    }
    pub(in crate::storage::payload) fn jobs(worker: &Worker) -> usize {
        worker.shared.state.lock().unwrap().jobs
    }
    pub(in crate::storage::payload) fn stats(worker: &Worker) -> WorkerStats {
        worker.shared.stats.lock().unwrap().clone()
    }
}
