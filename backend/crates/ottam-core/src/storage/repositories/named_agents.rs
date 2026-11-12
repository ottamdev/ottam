use crate::storage::error::StorageError;
use crate::storage::schema::named_agents;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

/// A named agent a conversation can delegate to. `engine_id` is an agent
/// instance's id; it is not checked here, since an instance can be removed
/// after the named agent was saved.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = named_agents)]
pub struct NamedAgentRecord {
    pub id: String,
    pub name: String,
    pub engine_id: String,
    pub model_id: Option<String>,
    pub mode_id: Option<String>,
    pub instructions: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The seed its tile is drawn from; none draws from its name.
    pub avatar_seed: Option<i64>,
}

pub struct NamedAgentRepository;

impl NamedAgentRepository {
    /// Every named agent, by name.
    pub fn list(connection: &mut SqliteConnection) -> Result<Vec<NamedAgentRecord>, StorageError> {
        Ok(named_agents::table
            .order((named_agents::name, named_agents::id))
            .select(NamedAgentRecord::as_select())
            .load(connection)?)
    }

    pub fn get(
        connection: &mut SqliteConnection,
        id: &str,
    ) -> Result<Option<NamedAgentRecord>, StorageError> {
        Ok(named_agents::table
            .find(id)
            .select(NamedAgentRecord::as_select())
            .first(connection)
            .optional()?)
    }

    /// The named agent called `name`, ignoring case.
    pub fn by_name(
        connection: &mut SqliteConnection,
        name: &str,
    ) -> Result<Option<NamedAgentRecord>, StorageError> {
        Ok(named_agents::table
            .filter(named_agents::name.eq(name))
            .select(NamedAgentRecord::as_select())
            .first(connection)
            .optional()?)
    }

    /// Fails with `namedAgentNameTaken` when another has the name.
    pub fn insert(
        connection: &mut SqliteConnection,
        record: &NamedAgentRecord,
    ) -> Result<(), StorageError> {
        diesel::insert_into(named_agents::table)
            .values(record)
            .execute(connection)
            .map_err(name_taken)?;
        Ok(())
    }

    /// Replaces everything but the id and creation time. False when it is
    /// gone.
    pub fn update(
        connection: &mut SqliteConnection,
        record: &NamedAgentRecord,
    ) -> Result<bool, StorageError> {
        let updated = diesel::update(named_agents::table.find(&record.id))
            .set((
                named_agents::name.eq(&record.name),
                named_agents::engine_id.eq(&record.engine_id),
                named_agents::model_id.eq(&record.model_id),
                named_agents::mode_id.eq(&record.mode_id),
                named_agents::instructions.eq(&record.instructions),
                named_agents::updated_at.eq(record.updated_at),
                named_agents::avatar_seed.eq(record.avatar_seed),
            ))
            .execute(connection)
            .map_err(name_taken)?;
        Ok(updated > 0)
    }

    /// False when it was gone.
    pub fn delete(connection: &mut SqliteConnection, id: &str) -> Result<bool, StorageError> {
        Ok(diesel::delete(named_agents::table.find(id)).execute(connection)? > 0)
    }
}

fn name_taken(error: diesel::result::Error) -> StorageError {
    use diesel::result::{DatabaseErrorKind, Error};
    match error {
        Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => StorageError::new(
            "namedAgentNameTaken",
            "another named agent already has this name",
        )
        .recoverable(true),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel_migrations::MigrationHarness;

    /// A named agent's tile seed is stored, replaced by an update, and may
    /// be cleared, which draws the tile from the name again.
    #[test]
    fn a_seed_round_trips_through_insert_and_update() {
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        connection
            .run_pending_migrations(crate::storage::database::MIGRATIONS)
            .unwrap();
        let record = NamedAgentRecord {
            id: "a".into(),
            name: "review-bot".into(),
            engine_id: "codex".into(),
            model_id: None,
            mode_id: None,
            instructions: String::new(),
            created_at: 0,
            updated_at: 0,
            avatar_seed: Some(u32::MAX.into()),
        };
        NamedAgentRepository::insert(&mut connection, &record).unwrap();
        let stored = NamedAgentRepository::get(&mut connection, "a").unwrap();
        assert_eq!(stored.unwrap().avatar_seed, Some(u32::MAX.into()));
        for seed in [Some(11), None] {
            let shuffled = NamedAgentRecord {
                avatar_seed: seed,
                ..record.clone()
            };
            NamedAgentRepository::update(&mut connection, &shuffled).unwrap();
            let stored = NamedAgentRepository::get(&mut connection, "a").unwrap();
            assert_eq!(stored.unwrap().avatar_seed, seed);
        }
    }
}
