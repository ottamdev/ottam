//! Unit-test setup for SQLite.
//!
//! The unit-test binary runs hundreds of tests in parallel, each with its own
//! database. Two things made that slow:
//!
//! - With SQLite's memory statistics on (its default), every SQLite
//!   allocation in the process takes one global mutex. The app keeps SQLite's
//!   defaults; only this binary turns the statistics off. So unit tests never
//!   see that mutex's contention: concurrency and latency bounds they check
//!   hold without it, and production numbers come from the app's settings.
//! - Each test's `Database::open` parsed and ran every migration. Tests whose
//!   subject isn't opening or migrating start from a copy of a home that was
//!   opened once ([`Database::open_test_home`]).

use crate::storage::{Database, StorageError};
use diesel::sql_types::Text;
use diesel::{Connection, RunQueryDsl, SqliteConnection};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Runs before `main`, so before anything opens a connection:
/// `sqlite3_config` is refused once SQLite has initialized.
#[ctor::ctor(unsafe)]
fn disable_sqlite_memory_statistics() {
    // SAFETY: nothing has initialized SQLite yet, and MEMSTATUS takes one int.
    unsafe {
        libsqlite3_sys::sqlite3_config(libsqlite3_sys::SQLITE_CONFIG_MEMSTATUS, 0);
    }
}

/// Each database a fresh `Database::open` leaves in a home, by file name, as
/// it left it. Built once per test process and held in memory; its scratch
/// home is deleted at once.
fn template() -> &'static [(String, Vec<u8>)] {
    static TEMPLATE: OnceLock<Vec<(String, Vec<u8>)>> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let scratch = tempfile::TempDir::new().expect("template scratch");
        let home = scratch.path().join("home");
        let database = Database::open(home.clone()).expect("opens the template home");
        // `VACUUM INTO` takes a consistent snapshot, WAL included, while the
        // home is still open; it keeps `auto_vacuum` and the page size.
        let template = databases(&home)
            .into_iter()
            .map(|name| {
                let copy = scratch.path().join(&name);
                diesel::sql_query("VACUUM INTO ?")
                    .bind::<Text, _>(copy.to_string_lossy().as_ref())
                    .execute(&mut connect(&home.join(&name)))
                    .expect("snapshots the template database");
                let bytes = std::fs::read(copy).expect("reads the template database");
                (name, bytes)
            })
            .collect();
        drop(database);
        template
    })
}

/// The SQLite databases in `home`, by file name.
fn databases(home: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(home)
        .expect("lists the home")
        .map(|entry| entry.expect("reads the home").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".sqlite"))
        .collect();
    names.sort();
    names
}

fn connect(path: &Path) -> SqliteConnection {
    let mut connection =
        SqliteConnection::establish(&path.to_string_lossy()).expect("opens a database file");
    diesel::sql_query("PRAGMA busy_timeout = 5000")
        .execute(&mut connection)
        .expect("sets a busy timeout");
    connection
}

impl Database {
    /// Opens `home` like [`Database::open`], starting from a copy of a home
    /// that was opened once, so no migration runs. Everything else open does
    /// still runs. Tests about opening, migrating, or reopening use `open`.
    pub(crate) fn open_test_home(home: PathBuf) -> Result<Self, StorageError> {
        let failed = |message: &str| StorageError::new("testHomeUnavailable", message);
        std::fs::create_dir_all(&home).map_err(|_| failed("could not create the test home"))?;
        for (name, bytes) in template() {
            let path = home.join(name);
            // A copy over an existing home would erase it.
            if path.exists() {
                return Err(failed(
                    "the home already has a database; reopen it with `open`",
                ));
            }
            std::fs::write(path, bytes).map_err(|_| failed("could not copy the template"))?;
        }
        Self::open(home)
    }
}

mod tests {
    use super::{connect, databases};
    use crate::storage::database::TextValue;
    use crate::storage::Database;
    use diesel::{RunQueryDsl, SqliteConnection};
    use std::path::Path;

    /// With statistics off SQLite counts no memory, so a no-op config (it ran
    /// too late, or not at all) fails here rather than only slowing tests.
    #[test]
    fn sqlite_memory_statistics_are_off() {
        let home = tempfile::TempDir::new().expect("temporary home");
        let _database = Database::open(home.path().to_owned()).expect("opens database");
        // SAFETY: a read of SQLite's global counter.
        assert_eq!(unsafe { libsqlite3_sys::sqlite3_memory_used() }, 0);
    }

    /// A copied home is the home a fresh open makes: the same databases,
    /// schema, file settings, migrations, and rows. Fails if `open` creates a
    /// database the template lacks, or writes data that a copy would carry
    /// into its own open.
    #[test]
    fn a_copied_home_matches_a_fresh_one() {
        let fresh = tempfile::TempDir::new().expect("fresh home");
        let copied = tempfile::TempDir::new().expect("copied home");
        let _fresh = Database::open(fresh.path().to_owned()).expect("opens fresh");
        let _copied = Database::open_test_home(copied.path().to_owned()).expect("opens copy");
        let names = databases(fresh.path());
        assert_eq!(names, databases(copied.path()));
        for name in names {
            assert_eq!(
                describe(&fresh.path().join(&name)),
                describe(&copied.path().join(&name)),
                "{name}"
            );
        }
    }

    fn text(connection: &mut SqliteConnection, query: &str) -> String {
        diesel::sql_query(query)
            .get_result::<TextValue>(connection)
            .unwrap_or_else(|error| panic!("{query}: {error}"))
            .value
    }

    fn identifier(name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    /// Everything a test could observe about a database file.
    fn describe(path: &Path) -> Vec<String> {
        let mut connection = connect(path);
        let mut description: Vec<String> = [
            "auto_vacuum",
            "page_size",
            "journal_mode",
            "user_version",
            "application_id",
        ]
        .iter()
        .map(|pragma| {
            let value = text(
                &mut connection,
                &format!("SELECT CAST({pragma} AS TEXT) AS value FROM pragma_{pragma}"),
            );
            format!("{pragma} = {value}")
        })
        .collect();
        description.push(text(
            &mut connection,
            "SELECT json_group_array(json_array(type, name, tbl_name, sql)) AS value \
             FROM (SELECT * FROM sqlite_schema ORDER BY type, name)",
        ));
        // SQLite's own tables too: `sqlite_sequence` and `sqlite_stat*`.
        let tables = text(
            &mut connection,
            "SELECT json_group_array(name) AS value \
             FROM (SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name)",
        );
        let tables: Vec<String> = serde_json::from_str(&tables).expect("table names");
        for table in tables {
            // A migration's `run_on` is when it ran, which differs between
            // the two homes; the versions applied are what counts.
            let columns: Vec<String> = if table == "__diesel_schema_migrations" {
                vec!["version".to_owned()]
            } else {
                let columns = text(
                    &mut connection,
                    &format!(
                        "SELECT json_group_array(name) AS value FROM pragma_table_info('{}')",
                        table.replace('\'', "''")
                    ),
                );
                serde_json::from_str(&columns).expect("column names")
            };
            // `quote` renders every value as SQL text, blobs included.
            let values = columns
                .iter()
                .map(|column| format!("quote({})", identifier(column)))
                .collect::<Vec<_>>()
                .join(", ");
            let rows = text(
                &mut connection,
                &format!(
                    "SELECT json_group_array(row) AS value FROM \
                     (SELECT json_array({values}) AS row FROM {} ORDER BY row)",
                    identifier(&table)
                ),
            );
            description.push(format!("{table}: {rows}"));
        }
        description
    }
}
