//! Metadata-only lookup; public identities, never SQLite row IDs, cross the server boundary.
use super::*;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct RevisionKey {
    pub workspace_id: String,
    pub conversation_id: String,
    pub item_id: String,
    pub payload_id: String,
    pub revision: u64,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = parts)]
pub(crate) struct ManifestPart {
    pub part_index: i32,
    pub name: String,
    pub format: String,
    pub source_path: Option<String>,
    pub committed_raw_bytes: i64,
    pub final_sha256: Option<Vec<u8>>,
    pub unavailable_reason: Option<String>,
}

pub(crate) struct Manifest {
    pub key: RevisionKey,
    pub role: String,
    pub upstream_truncated: Option<bool>,
    pub locally_truncated: bool,
    pub capture_reason: Option<String>,
    pub parts: Vec<ManifestPart>,
}

impl Reader {
    pub(crate) fn manifest(&mut self, key: &RevisionKey) -> Result<Manifest> {
        self.manifest_state(key, false)
    }

    /// The caller must hold a live capture lease when requesting staging metadata.
    pub(crate) fn active_manifest(&mut self, key: &RevisionKey) -> Result<Manifest> {
        self.manifest_state(key, true)
    }

    fn manifest_state(&mut self, key: &RevisionKey, active: bool) -> Result<Manifest> {
        self.0.connection.transaction::<_, Error, _>(|c| {
            let (id, role, upstream, local, reason): (
                i64,
                String,
                Option<i32>,
                i32,
                Option<String>,
            ) = revisions::table
                .filter(revisions::workspace_id.eq(&key.workspace_id))
                .filter(revisions::conversation_id.eq(&key.conversation_id))
                .filter(revisions::item_id.eq(&key.item_id))
                .filter(revisions::payload_id.eq(&key.payload_id))
                .filter(revisions::revision.eq(integer(key.revision)?))
                .filter(revisions::state.eq_any(if active {
                    vec!["ready", "staging"]
                } else {
                    vec!["ready"]
                }))
                .select((
                    revisions::id,
                    revisions::role,
                    revisions::upstream_truncated,
                    revisions::locally_truncated,
                    revisions::capture_reason,
                ))
                .first(c)?;
            let parts = parts::table
                .filter(parts::revision_id.eq(id))
                .order(parts::part_index)
                .limit(129)
                .select(ManifestPart::as_select())
                .load::<ManifestPart>(c)?;
            let mut total = 0_u64;
            for (index, part) in parts.iter().enumerate() {
                if index >= 128
                    || part.part_index != index as i32
                    || part.committed_raw_bytes < 0
                    || (!active
                        && part
                            .final_sha256
                            .as_ref()
                            .is_none_or(|digest| digest.len() != 32))
                {
                    return Err(invalid("invalid sealed manifest"));
                }
                total = total
                    .checked_add(part.committed_raw_bytes as u64)
                    .ok_or_else(|| invalid("manifest length overflow"))?;
            }
            if total > CAPTURE_LIMIT as u64 {
                return Err(invalid("oversized manifest"));
            }
            Ok(Manifest {
                key: key.clone(),
                role,
                upstream_truncated: upstream.map(|v| v != 0),
                locally_truncated: local != 0,
                capture_reason: reason,
                parts,
            })
        })
    }

    pub(crate) fn read_sealed(
        &mut self,
        key: &RevisionKey,
        part: u32,
        size: u64,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        self.0
            .read_authorized(key, part, size, offset, length, false)
    }

    /// Reads only the committed prefix advertised by the live service.
    pub(crate) fn read_active(
        &mut self,
        key: &RevisionKey,
        part: u32,
        size: u64,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        self.0
            .read_authorized(key, part, size, offset, length, true)
    }
}
