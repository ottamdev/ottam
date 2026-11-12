//! Bootstrap-only backup: the Core ownership lock is held and writers have not started.
use super::*;
use diesel::sql_types::Text;
use std::path::Path;

#[derive(QueryableByName)]
struct Integrity {
    #[diesel(sql_type = Text)]
    value: String,
}

pub(crate) fn verify_database(path: &Path) -> Result<()> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("backup database is not a regular file"));
    }
    let mut connection =
        SqliteConnection::establish(path.to_str().ok_or_else(|| invalid("backup path"))?)?;
    let values = diesel::sql_query("SELECT integrity_check AS value FROM pragma_integrity_check")
        .load::<Integrity>(&mut connection)?;
    if values.len() != 1 || values[0].value != "ok" {
        return Err(invalid("backup integrity check failed"));
    }
    Ok(())
}

pub(super) fn verify_snapshot(path: &Path) -> Result<()> {
    let marker = std::fs::read_to_string(path.join("verified"))?;
    if !matches!(
        marker.as_str(),
        "ottam.sqlite\n" | "ottam.sqlite\npayloads.sqlite\n"
    ) {
        return Err(invalid("invalid coordinated backup manifest"));
    }
    for database in marker.lines() {
        verify_database(&path.join(database))?;
    }
    Ok(())
}

/// Complete backups are marked only after both SQLite snapshots
/// have been verified. Incomplete directories are retained for diagnosis.
pub(crate) fn create(home: &Path, main: &mut SqliteConnection) -> Result<PathBuf> {
    if std::fs::symlink_metadata(home.join("backups")).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("unsafe backup directory"));
    }
    let target = home
        .join("backups")
        .join(format!("database-upgrade-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&target)?;
    let snapshot = target.join("ottam.sqlite");
    diesel::sql_query("VACUUM INTO ?")
        .bind::<Text, _>(snapshot.to_string_lossy().as_ref())
        .execute(main)?;
    verify_database(&snapshot)?;
    let has_payload_database = home.join("payloads.sqlite").is_file();
    if has_payload_database {
        let mut connection = SqliteConnection::establish(
            home.join("payloads.sqlite")
                .to_str()
                .ok_or_else(|| invalid("payload path"))?,
        )?;
        let snapshot = target.join("payloads.sqlite");
        diesel::sql_query("VACUUM INTO ?")
            .bind::<Text, _>(snapshot.to_string_lossy().as_ref())
            .execute(&mut connection)?;
        verify_database(&snapshot)?;
    }

    std::fs::write(
        target.join("verified"),
        if has_payload_database {
            "ottam.sqlite\npayloads.sqlite\n"
        } else {
            "ottam.sqlite\n"
        },
    )?;
    verify_snapshot(&target)?;
    Ok(target)
}
