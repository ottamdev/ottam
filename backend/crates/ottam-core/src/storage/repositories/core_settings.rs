use crate::storage::error::StorageError;
use crate::storage::schema::core_settings;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

/// The always-allow value last set in any conversation; new conversations
/// start from it.
pub const ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT: &str = "toolPermissions.alwaysAllowToolActions";

/// Core-owned settings that belong to no workspace, one text value per key.
pub struct CoreSettingsRepository;

impl CoreSettingsRepository {
    pub fn get_bool(
        connection: &mut SqliteConnection,
        key: &str,
    ) -> Result<Option<bool>, StorageError> {
        let value = core_settings::table
            .find(key)
            .select(core_settings::value)
            .first::<String>(connection)
            .optional()?;
        Ok(value.map(|value| value == "true"))
    }

    pub fn set_bool(
        connection: &mut SqliteConnection,
        key: &str,
        value: bool,
    ) -> Result<(), StorageError> {
        let value = if value { "true" } else { "false" };
        diesel::insert_into(core_settings::table)
            .values((core_settings::key.eq(key), core_settings::value.eq(value)))
            .on_conflict(core_settings::key)
            .do_update()
            .set(core_settings::value.eq(value))
            .execute(connection)?;
        Ok(())
    }
}
