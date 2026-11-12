//! Live descriptors and publication leases; byte queues belong exclusively to Worker.
use super::*;
use crate::storage::payload::{self, worker::OutputId, Reader, RevisionKey};
use std::time::Duration;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    conversation: String,
    item: String,
    role: String,
}

struct Entry {
    generation: u64,
    leases: Vec<RevisionKey>,
    previous: Option<TranscriptPayload>,
    key: RevisionKey,
    run: String,
    descriptor: TranscriptPayload,
    output: Option<OutputId>,
    completion: Option<tokio::sync::oneshot::Receiver<payload::Result<payload::Receipt>>>,
    terminal: bool,
    pending: bool,
    delivering: bool,
    accepted: usize,
}

fn admission_reason(error: payload::worker::AdmissionError) -> &'static str {
    match error {
        payload::worker::AdmissionError::StorageFull => "storageFull",
        payload::worker::AdmissionError::StorageUnavailable => "writeFailed",
        _ => "queueFull",
    }
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, Entry>,
    deleted: HashSet<String>,
    run_bytes: HashMap<(String, String), usize>,
}

pub(in crate::server::chat) struct PayloadService {
    database: Database,
    owner: Weak<TranscriptCoordinator>,
    state: Mutex<State>,
    wake: Arc<Notify>,
    observer: Mutex<Option<tokio::task::AbortHandle>>,
    started: AtomicBool,
    #[cfg(test)]
    maintenance_runs: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    refresh_runs: std::sync::atomic::AtomicU64,
}

impl PayloadService {
    pub(in crate::server::chat) fn new(
        database: Database,
        owner: Weak<TranscriptCoordinator>,
    ) -> Arc<Self> {
        Arc::new(Self {
            database,
            owner,
            state: Mutex::new(State::default()),
            wake: Arc::new(Notify::new()),
            observer: Mutex::new(None),
            started: AtomicBool::new(false),
            #[cfg(test)]
            maintenance_runs: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            refresh_runs: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub(in crate::server::chat) fn start(self: &Arc<Self>) {
        if !self.started.swap(true, Ordering::AcqRel) {
            let weak = Arc::downgrade(self);
            let task = tokio::spawn(async move {
                Self::observe(weak).await;
            });
            *self.observer.lock().unwrap() = Some(task.abort_handle());
        }
    }

    #[cfg(test)]
    pub(in crate::server::chat) fn block_next_write(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (wait, release) = self.database.payload_worker().block_next_write();
        let (entered, receiver) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = wait.recv();
            let _ = entered.send(());
        });
        (receiver, release)
    }

    pub(in crate::server::chat) fn submit(
        self: &Arc<Self>,
        workspace: &str,
        conversation: &str,
        run: &str,
        item: &str,
        updates: Vec<Update>,
    ) -> Vec<TranscriptPayload> {
        let worker = self.database.payload_worker();
        let mut state = self.state.lock().expect("payload state");
        if state.deleted.contains(conversation) {
            return Vec::new();
        }
        let mut descriptors = Vec::new();
        for mut update in updates {
            let key = Key {
                conversation: conversation.into(),
                item: item.into(),
                role: update.role.clone(),
            };
            if !state.entries.contains_key(&key)
                && (update.parts.is_none()
                    || (update.role == "fileChangeInput"
                        && update.parts.as_ref().is_some_and(Vec::is_empty)))
            {
                continue;
            }
            let run_key = (run.to_owned(), update.role.clone());
            let mut run_used = state.run_bytes.get(&run_key).copied().unwrap_or(0);
            let run_limit: usize = if update.role == "fileChangeInput" {
                64 * 1024 * 1024
            } else {
                128 * 1024 * 1024
            };
            let entry = state.entries.entry(key.clone()).or_insert_with(|| Entry {
                leases: Vec::new(),
                previous: None,
                generation: 0,
                key: RevisionKey {
                    workspace_id: workspace.into(),
                    conversation_id: conversation.into(),
                    item_id: item.into(),
                    payload_id: uuid::Uuid::now_v7().to_string(),
                    revision: 0,
                },
                run: run.into(),
                descriptor: TranscriptPayload {
                    role: update.role.clone(),
                    availability: "pending".into(),
                    reason: None,
                    payload_id: None,
                    byte_count: Some(0),
                    upstream_truncated: update.upstream,
                    locally_truncated: false,
                    revision: 0,
                    parts: Vec::new(),
                },
                output: None,
                completion: None,
                terminal: false,
                pending: false,
                delivering: false,
                accepted: 0,
            });
            entry.generation += 1;
            entry.terminal |= update.publish;
            entry.descriptor.upstream_truncated =
                update.upstream.or(entry.descriptor.upstream_truncated);
            let replacement = update.parts.is_some()
                && (!update.append || entry.output.is_none() || entry.completion.is_some());
            if replacement {
                entry.delivering = false;
                if let Some(output) = entry.output.take() {
                    let _ = worker.cancel(output);
                }
                if matches!(
                    entry.descriptor.availability.as_str(),
                    "available" | "partial"
                ) {
                    entry.previous = Some(entry.descriptor.clone());
                }
                entry.completion = None;
                run_used = run_used.saturating_sub(entry.accepted);
                entry.accepted = 0;
                entry.key.revision += 1;
                entry.descriptor.revision = entry.key.revision;
                entry.descriptor.payload_id = Some(entry.key.payload_id.clone());
                entry.descriptor.parts.clear();
                entry.descriptor.byte_count = Some(0);
                entry.descriptor.availability = "pending".into();
                entry.descriptor.reason = None;
                entry.descriptor.locally_truncated = false;
                let parts = update.parts.as_mut().expect("replacement parts");
                let too_many = parts.len() > 128;
                parts.truncate(128);
                if too_many {
                    if let Some(part) = parts.last_mut() {
                        part.reason = Some("captureLimit".into());
                    }
                }
                // The worker records part format when an output is created. An
                // image that cannot fit must therefore become unavailable before
                // registering metadata, rather than retaining an image format for
                // an empty part. Text still keeps its accepted prefix below.
                let mut available = LIMIT
                    .saturating_sub(entry.accepted)
                    .min(run_limit.saturating_sub(run_used));
                for part in parts.iter_mut() {
                    if part.bytes.len() > available && part.format.starts_with("image/") {
                        part.bytes.clear();
                        part.format = "unavailable".into();
                        part.reason = Some("captureLimit".into());
                    }
                    let accepted = if part.format == "unavailable" {
                        0
                    } else {
                        part.bytes.len().min(available)
                    };
                    available = available.saturating_sub(accepted);
                }
                let metadata = payload::Revision {
                    payload_id: entry.key.payload_id.clone(),
                    revision: entry.key.revision,
                    workspace_id: workspace.into(),
                    conversation_id: conversation.into(),
                    item_id: item.into(),
                    run_id: run.into(),
                    role: update.role.clone(),
                    upstream_truncated: entry.descriptor.upstream_truncated,
                    parts: parts
                        .iter()
                        .map(|p| payload::Part {
                            name: p.name.clone(),
                            format: p.format.clone(),
                            source_path: p.path.clone(),
                            unavailable_reason: (p.format == "unavailable")
                                .then(|| p.reason.clone().unwrap_or_else(|| "unavailable".into())),
                        })
                        .collect(),
                };
                self.database
                    .payload_leases()
                    .lock()
                    .unwrap()
                    .insert(entry.key.clone());
                entry.leases.push(entry.key.clone());
                match worker.begin(metadata) {
                    Ok(output) => {
                        entry.output = Some(output);
                        entry.pending = true;
                    }
                    Err(error) => {
                        entry.pending = false;
                        entry.descriptor.availability = "unavailable".into();
                        entry.descriptor.locally_truncated = true;
                        entry.descriptor.reason = Some(admission_reason(error).into());
                        if let Some(previous) = entry.previous.as_mut() {
                            previous.locally_truncated = true;
                            previous.reason = entry.descriptor.reason.clone();
                            previous.availability = "partial".into();
                        }
                    }
                }
            }
            if let Some(output) = entry.output {
                if let Some(parts) = update.parts {
                    for (index, mut part) in parts.into_iter().enumerate() {
                        let index = if update.append { 0 } else { index };
                        let remaining = LIMIT
                            .saturating_sub(entry.accepted)
                            .min(run_limit.saturating_sub(run_used));
                        if part.bytes.len() > remaining {
                            if part.format.starts_with("image/") {
                                part.bytes.clear();
                            } else {
                                part.bytes.truncate(remaining);
                            }
                            part.reason = Some("captureLimit".into());
                        }
                        if let Some(reason) = part.reason {
                            entry.descriptor.locally_truncated = true;
                            if part.format != "unavailable" {
                                worker.mark_loss(output, "captureLimit");
                            }
                            entry.descriptor.reason = Some(reason);
                        }
                        if part.format == "unavailable" {
                            continue;
                        }
                        let count = part.bytes.len();
                        match worker.enqueue(output, index, part.bytes) {
                            Ok(()) => {
                                entry.accepted += count;
                                run_used += count;
                                entry.pending = true;
                            }
                            Err(error) => {
                                entry.descriptor.locally_truncated = true;
                                let reason = admission_reason(error);
                                entry.descriptor.reason.get_or_insert(reason.into());
                                if reason == "writeFailed" {
                                    if let Some(previous) = entry.previous.as_mut() {
                                        previous.locally_truncated = true;
                                        previous.reason = Some(reason.into());
                                        previous.availability = "partial".into();
                                    }
                                }
                            }
                        }
                    }
                }
                if entry.terminal && entry.completion.is_none() {
                    match worker.complete(output) {
                        Ok(completion) => {
                            entry.completion = Some(completion);
                            entry.pending = true;
                        }
                        Err(_) => {
                            entry.descriptor.availability = "failed".into();
                            entry.descriptor.reason = Some("writeFailed".into());
                            entry.pending = false;
                        }
                    }
                }
            }
            descriptors.push(entry.previous.as_ref().unwrap_or(&entry.descriptor).clone());
            state.run_bytes.insert(run_key, run_used);
        }
        drop(state);
        self.start();
        self.wake.notify_one();
        descriptors
    }

    pub(in crate::server::chat) fn pending(&self, run: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.run == run && (e.pending || e.delivering))
    }

    pub(in crate::server::chat) fn finish_run(
        &self,
        run: &str,
    ) -> Vec<(String, TranscriptPayload)> {
        let mut state = self.state.lock().unwrap();
        let mut lost = Vec::new();
        for (key, entry) in state
            .entries
            .iter_mut()
            .filter(|(_, e)| e.run == run && !e.terminal)
        {
            entry.generation += 1;
            entry.terminal = true;
            entry.pending = false;
            entry.delivering = false;
            if let Some(output) = entry.output.take() {
                let _ = self.database.payload_worker().cancel(output);
            }
            entry.descriptor.availability = "unavailable".into();
            entry.descriptor.reason = Some("unfinishedOutput".into());
            entry.descriptor.locally_truncated = true;
            lost.push((key.item.clone(), entry.descriptor.clone()));
        }
        lost
    }

    pub(in crate::server::chat) async fn fence(&self, conversation: &str) {
        {
            let mut state = self.state.lock().unwrap();
            state.deleted.insert(conversation.into());
            for entry in state
                .entries
                .values()
                .filter(|e| e.key.conversation_id == conversation)
            {
                if let Some(output) = entry.output {
                    let _ = self.database.payload_worker().cancel(output);
                }
            }
        }
        loop {
            let active = self
                .state
                .lock()
                .unwrap()
                .entries
                .values()
                .filter(|e| e.key.conversation_id == conversation)
                .any(|e| {
                    e.output
                        .is_some_and(|id| self.database.payload_worker().contains(id))
                });
            if !active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub(in crate::server::chat) fn unfence(&self, conversation: &str) {
        self.state.lock().unwrap().deleted.remove(conversation);
    }

    pub(in crate::server::chat) fn forget_conversation(&self, conversation: &str) {
        let mut state = self.state.lock().unwrap();
        state.entries.retain(|_, entry| {
            if entry.key.conversation_id != conversation {
                return true;
            }
            for key in &entry.leases {
                self.database.payload_leases().lock().unwrap().remove(key);
            }
            false
        });
        let runs: HashSet<_> = state.entries.values().map(|e| e.run.clone()).collect();
        state.run_bytes.retain(|(run, _), _| runs.contains(run));
        self.database.payload_worker().notify_changed();
        self.wake.notify_one();
    }

    /// Called only after the sole publisher commits this immutable descriptor.
    /// A replacement may already be in flight, so retain its keys and fallback.
    pub(in crate::server::chat) fn checkpointed(
        &self,
        conversation: &str,
        item: &str,
        descriptor: &TranscriptPayload,
    ) {
        let mut state = self.state.lock().unwrap();
        let Some(entry) = state.entries.get_mut(&Key {
            conversation: conversation.into(),
            item: item.into(),
            role: descriptor.role.clone(),
        }) else {
            return;
        };
        if descriptor.payload_id.as_deref() != Some(entry.key.payload_id.as_str()) {
            return;
        }
        let mut leases = self.database.payload_leases().lock().unwrap();
        let previous_count = entry.leases.len();
        entry.leases.retain(|key| {
            let keep = key.revision >= descriptor.revision
                || *key == entry.key
                || key.revision == entry.descriptor.revision
                || entry
                    .previous
                    .as_ref()
                    .is_some_and(|p| p.revision == key.revision);
            if !keep {
                leases.remove(key);
            }
            keep
        });
        let released = previous_count != entry.leases.len();
        drop(leases);
        drop(state);
        if released {
            self.database.payload_worker().notify_changed();
        }
    }

    pub(in crate::server::chat) fn retire_run(&self, run: &str) {
        let mut state = self.state.lock().unwrap();
        state.entries.retain(|_, entry| {
            if entry.run != run || entry.pending || entry.delivering {
                return true;
            }
            if let Some(output) = entry.output {
                let _ = self.database.payload_worker().cancel(output);
            }
            for key in &entry.leases {
                self.database.payload_leases().lock().unwrap().remove(key);
            }
            false
        });
        state.run_bytes.retain(|(id, _), _| id != run);
        self.database.payload_worker().notify_changed();
        self.wake.notify_one();
    }

    pub(in crate::server::chat) fn live(
        &self,
        workspace: &str,
        conversation: &str,
        id: &str,
    ) -> Option<TranscriptPayload> {
        let state = self.state.lock().unwrap();
        if state.deleted.contains(conversation) {
            return None;
        }
        state
            .entries
            .values()
            .find(|e| {
                e.key.workspace_id == workspace
                    && e.key.conversation_id == conversation
                    && e.key.payload_id == id
            })
            .map(|e| e.previous.as_ref().unwrap_or(&e.descriptor).clone())
    }

    pub(in crate::server::chat) fn live_key(
        &self,
        workspace: &str,
        conversation: &str,
        id: &str,
    ) -> Option<RevisionKey> {
        let state = self.state.lock().unwrap();
        if state.deleted.contains(conversation) {
            return None;
        }
        state
            .entries
            .values()
            .find(|e| {
                e.key.workspace_id == workspace
                    && e.key.conversation_id == conversation
                    && e.key.payload_id == id
            })
            .map(|e| {
                let mut key = e.key.clone();
                key.revision = e.previous.as_ref().unwrap_or(&e.descriptor).revision;
                key
            })
    }

    async fn refresh(&self) {
        #[cfg(test)]
        self.refresh_runs.fetch_add(1, Ordering::Relaxed);
        let pending: Vec<_> = {
            let mut state = self.state.lock().unwrap();
            let deleted = state.deleted.clone();
            state
                .entries
                .iter_mut()
                .filter(|(_, e)| e.pending && !deleted.contains(&e.key.conversation_id))
                .map(|(key, entry)| {
                    let result = entry.completion.as_mut().map(|rx| rx.try_recv());
                    let sealed = matches!(result, Some(Ok(Ok(_))));
                    let failed = matches!(
                        result,
                        Some(Ok(Err(_)))
                            | Some(Err(tokio::sync::oneshot::error::TryRecvError::Closed))
                    );
                    if sealed || failed {
                        entry.completion = None;
                        entry.output = None;
                    }
                    (key.clone(), entry.key.clone(), sealed, failed)
                })
                .collect()
        };
        for (key, revision, sealed, failed) in pending {
            let home = self.database.home().to_path_buf();
            let lookup = revision.clone();
            let manifest = tokio::task::spawn_blocking(move || {
                let mut reader = Reader::open(&home)?;
                if sealed {
                    reader.manifest(&lookup)
                } else {
                    reader.active_manifest(&lookup)
                }
            })
            .await;
            let descriptor = {
                let mut state = self.state.lock().unwrap();
                if state.deleted.contains(&revision.conversation_id) {
                    continue;
                }
                let Some(entry) = state
                    .entries
                    .get_mut(&key)
                    .filter(|e| e.key == revision && e.pending)
                else {
                    continue;
                };
                // Streaming captures have no completion receiver yet. A failed
                // worker retires their IDs too; surviving staging metadata must
                // not leave their descriptor permanently pending.
                let failed = failed
                    || (!sealed
                        && entry.completion.is_none()
                        && entry
                            .output
                            .is_none_or(|id| !self.database.payload_worker().contains(id)));
                if failed {
                    entry.descriptor = entry.previous.take().unwrap_or_else(|| {
                        let mut d = entry.descriptor.clone();
                        d.availability = "failed".into();
                        d
                    });
                    entry.descriptor.reason = Some("writeFailed".into());
                    entry.descriptor.locally_truncated = true;
                    entry.pending = false;
                } else if let Ok(Ok(manifest)) = manifest {
                    let mut descriptor = super::sqlite::descriptor(manifest);
                    if !sealed {
                        descriptor.availability = "pending".into();
                    }
                    if entry.descriptor.locally_truncated {
                        descriptor.locally_truncated = true;
                        descriptor.reason = entry.descriptor.reason.clone();
                        if sealed {
                            descriptor.availability = "partial".into();
                        }
                    }
                    descriptor.upstream_truncated = entry.descriptor.upstream_truncated;
                    entry.pending = !sealed
                        && (entry.completion.is_some()
                            || (descriptor.byte_count.unwrap_or(0) as usize) < entry.accepted);
                    entry.descriptor = descriptor;
                    if sealed {
                        entry.previous = None;
                    }
                } else if sealed
                    || (entry.completion.is_none()
                        && entry
                            .output
                            .is_none_or(|id| !self.database.payload_worker().contains(id)))
                {
                    entry.descriptor = entry.previous.take().unwrap_or_else(|| {
                        let mut d = entry.descriptor.clone();
                        d.availability = "failed".into();
                        d
                    });
                    entry.descriptor.reason = Some("writeFailed".into());
                    entry.descriptor.locally_truncated = true;
                    entry.pending = false;
                }
                entry.delivering = true;
                (
                    entry.previous.as_ref().unwrap_or(&entry.descriptor).clone(),
                    entry.generation,
                )
            };
            if let Some(owner) = self.owner.upgrade() {
                owner.payload_updated(
                    &key.conversation,
                    &key.item,
                    descriptor.0,
                    Some(descriptor.1),
                );
            }
            if let Some(entry) = self
                .state
                .lock()
                .unwrap()
                .entries
                .get_mut(&key)
                .filter(|e| e.key == revision)
            {
                entry.delivering = false;
            }
            if let Some(owner) = self.owner.upgrade() {
                owner.payload_work_finished();
            }
        }
    }

    pub(in crate::server::chat) fn delivery_is_current(
        &self,
        conversation: &str,
        item: &str,
        role: &str,
        generation: u64,
    ) -> bool {
        let state = self.state.lock().unwrap();
        !state.deleted.contains(conversation)
            && state
                .entries
                .get(&Key {
                    conversation: conversation.into(),
                    item: item.into(),
                    role: role.into(),
                })
                .is_some_and(|entry| entry.generation == generation && entry.delivering)
    }

    async fn maintain(&self, cursor: &mut i64) -> bool {
        let cache_pending =
            match crate::server::storage_cache::sweep(&self.database, false, None, false).await {
                Ok((_, pending)) => pending,
                Err(error) => {
                    tracing::warn!(error = %error, "payload cache maintenance failed");
                    return true;
                }
            };
        let home = self.database.home().to_path_buf();
        let after = *cursor;
        let Ok(Ok(candidates)) = tokio::task::spawn_blocking(move || {
            payload::maintenance::Maintenance::open(&home)?.candidates(after)
        })
        .await
        else {
            return true;
        };
        let next_cursor = if candidates.len() < payload::maintenance::REVISION_BATCH as usize {
            0
        } else {
            candidates.last().map_or(0, |c| c.id)
        };
        let mut protected = self.database.payload_leases().lock().unwrap().clone();
        protected.extend(self.database.payload_worker().active_keys());
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|c| !protected.contains(&c.key))
            .collect();
        let Ok(candidates) = self
            .database
            .run("transcript.payloadMaintenance", move |main| {
                let mut remove = Vec::new();
                for candidate in candidates {
                    if !payload::maintenance::referenced(main, &candidate.key)
                        .map_err(|e| StorageError::new("payloadMaintenanceFailed", e.to_string()))?
                    {
                        remove.push(candidate);
                    }
                }
                Ok(remove)
            })
            .await
        else {
            return true;
        };
        let mut protected = self.database.payload_leases().lock().unwrap().clone();
        protected.extend(self.database.payload_worker().active_keys());
        let home = self.database.home().to_path_buf();
        let result = tokio::task::spawn_blocking(move || -> payload::Result<bool> {
            let mut maintenance = payload::maintenance::Maintenance::open(&home)?;
            for candidate in candidates {
                if !protected.contains(&candidate.key) {
                    maintenance.remove(&candidate)?;
                }
            }
            maintenance.idle()?;
            maintenance.log_usage(&home)?;
            maintenance.pending()
        })
        .await;
        #[cfg(test)]
        self.maintenance_runs.fetch_add(1, Ordering::Relaxed);
        // A failed batch must be retried before moving on. Otherwise a later
        // successful final batch could disarm maintenance with orphans left.
        if matches!(result, Ok(Ok(_))) {
            *cursor = next_cursor;
        }
        cache_pending || *cursor != 0 || !matches!(result, Ok(Ok(false)))
    }

    async fn observe(weak: Weak<Self>) {
        let mut next_maintenance = Some(tokio::time::Instant::now());
        let mut cursor = 0;
        let mut scan_generation = None;
        loop {
            let Some(service) = weak.upgrade() else {
                break;
            };
            // Own only the notification sources while sleeping, not the
            // service. Its last owner can then drop it and abort this task.
            let database = service.database.clone();
            let wake = service.wake.clone();
            let changed = database.payload_worker().changed();
            let requested = wake.notified();
            tokio::pin!(changed);
            tokio::pin!(requested);
            changed.as_mut().enable();
            requested.as_mut().enable();
            service.refresh().await;
            if next_maintenance.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                // Changes behind either bounded scan's cursor must survive
                // until the entire scan completes, not just this one batch.
                let generation = *scan_generation
                    .get_or_insert_with(|| database.payload_worker().change_generation());
                let unfinished = service.maintain(&mut cursor).await;
                next_maintenance = (unfinished
                    || generation != database.payload_worker().change_generation())
                .then(|| tokio::time::Instant::now() + Duration::from_secs(30));
                if !unfinished {
                    scan_generation = None;
                }
            }
            drop(service);
            tokio::select! {
                _ = changed => { next_maintenance.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(30)); },
                _ = requested => { next_maintenance.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(30)); },
                _ = async {
                    match next_maintenance {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {},
            }
        }
    }
}

impl Drop for PayloadService {
    fn drop(&mut self) {
        if let Some(observer) = self.observer.get_mut().unwrap().take() {
            observer.abort();
        }
        for entry in self.state.get_mut().unwrap().entries.values() {
            if let Some(output) = entry.output {
                let _ = self.database.payload_worker().cancel(output);
            }
            for key in &entry.leases {
                self.database.payload_leases().lock().unwrap().remove(key);
            }
        }
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
