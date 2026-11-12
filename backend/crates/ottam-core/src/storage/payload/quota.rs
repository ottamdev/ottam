//! Conservative admission covers queued bytes plus replacement/WAL maintenance space.
use super::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

// Four maximum captures remain available for WAL/checkpoint/reclamation work.
pub(crate) const MAINTENANCE_RESERVE: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Budget {
    pub home: PathBuf,
    pub physical: u64,
    pub conversations: HashMap<(String, String), u64>,
    reserved: u64,
    owners: HashMap<(String, String), u64>,
}

pub(super) struct Reservation {
    budget: Arc<Mutex<Budget>>,
    owner: (String, String),
    bytes: u64,
}

impl Reservation {
    pub(super) fn new(budget: Arc<Mutex<Budget>>, revision: &Revision) -> Option<Self> {
        let mut value = Self {
            budget,
            owner: (
                revision.workspace_id.clone(),
                revision.conversation_id.clone(),
            ),
            bytes: 0,
        };
        let metadata = 512
            + revision.payload_id.len()
            + revision.workspace_id.len()
            + revision.conversation_id.len()
            + revision.item_id.len()
            + revision.run_id.len()
            + revision.role.len()
            + revision
                .parts
                .iter()
                .map(|p| {
                    256 + p.name.len()
                        + p.format.len()
                        + p.source_path.as_ref().map_or(0, String::len)
                })
                .sum::<usize>();
        value.add(metadata as u64).then_some(value)
    }
    pub(super) fn add(&mut self, bytes: u64) -> bool {
        let mut budget = self.budget.lock().unwrap();
        // Both the database pages and WAL may grow, and index/page splitting needs
        // headroom. Compression savings never enlarge the logical capture limit.
        let available = fs2::available_space(&budget.home).unwrap_or(0);
        if available
            < MAINTENANCE_RESERVE
                .saturating_add(budget.reserved.saturating_add(bytes).saturating_mul(3))
        {
            return false;
        }
        budget.reserved += bytes;
        *budget.owners.entry(self.owner.clone()).or_default() += bytes;
        self.bytes += bytes;
        true
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut budget = self.budget.lock().unwrap();
        budget.reserved -= self.bytes;
        if let Some(value) = budget.owners.get_mut(&self.owner) {
            *value -= self.bytes;
            if *value == 0 {
                budget.owners.remove(&self.owner);
            }
        }
    }
}

pub(super) fn check_write(budget: &Mutex<Budget>, extra_bytes: u64) -> Result<()> {
    let budget = budget.lock().unwrap();
    let required = MAINTENANCE_RESERVE.saturating_add(
        budget
            .reserved
            .saturating_add(extra_bytes)
            .saturating_mul(3),
    );
    if fs2::available_space(&budget.home)? < required {
        return Err(invalid(
            "insufficient free disk space for pending payload writes",
        ));
    }
    Ok(())
}

#[derive(QueryableByName)]
struct Usage {
    #[diesel(sql_type = diesel::sql_types::Text)]
    workspace_id: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    conversation_id: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    bytes: i64,
}

pub(super) fn refresh(
    repo: &mut Repository,
    home: &std::path::Path,
    budget: &Mutex<Budget>,
) -> Result<()> {
    let rows = diesel::sql_query("SELECT workspace_id, conversation_id, encoded_bytes + overhead_bytes AS bytes FROM payload_usage").load::<Usage>(&mut repo.connection)?;
    let mut conversations = HashMap::new();
    for row in rows {
        conversations.insert(
            (row.workspace_id, row.conversation_id),
            row.bytes.try_into()?,
        );
    }
    let mut physical = 0u64;
    for name in ["payloads.sqlite", "payloads.sqlite-wal"] {
        match std::fs::metadata(home.join(name)) {
            Ok(metadata) => physical = physical.saturating_add(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut budget = budget.lock().unwrap();
    budget.physical = physical;
    budget.home = home.to_path_buf();
    budget.conversations = conversations;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_metadata_usage_counts_utf8_bytes_and_reclaims_exactly() {
        let home = tempfile::tempdir().unwrap();
        let mut repo = Repository::open(home.path()).unwrap();
        let mut metadata = super::super::worker_tests::metadata("日本語", 1, "run");
        metadata.parts[0].name = "出力".into();
        metadata.parts[0].source_path = Some("資料/本文.txt".into());
        let budget = Arc::new(Mutex::new(Budget::default()));
        budget.lock().unwrap().home = home.path().to_path_buf();
        let reservation = Reservation::new(budget.clone(), &metadata).unwrap();
        let expected = reservation.bytes;
        drop(reservation);
        let receipt = repo.create_revision(&metadata).unwrap();
        refresh(&mut repo, home.path(), &budget).unwrap();
        assert_eq!(
            budget
                .lock()
                .unwrap()
                .conversations
                .values()
                .copied()
                .sum::<u64>(),
            expected
        );
        repo.delete(receipt.revision_id).unwrap();
        refresh(&mut repo, home.path(), &budget).unwrap();
        assert!(budget.lock().unwrap().conversations.is_empty());
    }

    #[test]
    fn payload_disk_admission_uses_free_space_not_history_size() {
        let home = tempfile::tempdir().unwrap();
        let metadata = super::super::worker_tests::metadata("quota", 1, "run");
        let budget = Arc::new(Mutex::new(Budget::default()));
        budget.lock().unwrap().home = home.path().join("missing");
        assert!(Reservation::new(budget.clone(), &metadata).is_none());
        budget.lock().unwrap().home = home.path().to_path_buf();
        budget.lock().unwrap().physical = 10 * 1024 * 1024 * 1024;
        budget.lock().unwrap().conversations.insert(
            (
                metadata.workspace_id.clone(),
                metadata.conversation_id.clone(),
            ),
            1024 * 1024 * 1024,
        );
        assert!(Reservation::new(budget.clone(), &metadata).is_some());
        let mut other = metadata.clone();
        other.conversation_id = "other".into();
        let mut reservation = Reservation::new(budget.clone(), &other).unwrap();
        assert!(reservation.add(CAPTURE_LIMIT as u64));
        assert!(!reservation.add(u64::MAX));
        assert!(check_write(&budget, u64::MAX).is_err());
        assert!(check_write(&budget, 0).is_ok());
        assert!(budget.lock().unwrap().reserved > CAPTURE_LIMIT as u64);
        drop(reservation);
        assert_eq!(budget.lock().unwrap().reserved, 0);
        assert!(budget.lock().unwrap().owners.is_empty());
        budget.lock().unwrap().conversations.clear();
        assert!(Reservation::new(budget, &metadata).is_some());
    }
}
