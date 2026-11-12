use std::process::{Command, Stdio};

use ottam_protocol::{CoreExitErrorNotification, METHOD_CORE_EXIT_ERROR};
use diesel::{Connection, RunQueryDsl, SqliteConnection};

#[test]
fn startup_exit_emits_structured_error_before_initialize() {
    let home = tempfile::tempdir().unwrap();
    drop(ottam_core::storage::Database::open(home.path().to_path_buf()).unwrap());
    let database_path = home.path().join("ottam.sqlite");
    let mut database = SqliteConnection::establish(database_path.to_str().unwrap()).unwrap();
    diesel::sql_query("INSERT INTO __diesel_schema_migrations (version) VALUES ('99999999999999')")
        .execute(&mut database)
        .unwrap();
    drop(database);

    let output = Command::new(env!("CARGO_BIN_EXE_ottam-core"))
        .env("OTTAM_HOME", home.path())
        .env("OTTAM_LOG_DIR", home.path().join("logs"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "exit output must be one protocol message: {stdout}"
    );
    let notification: CoreExitErrorNotification = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(notification.method, METHOD_CORE_EXIT_ERROR);
    assert_eq!(notification.params.kind, "storageMigrationFailed");
    assert!(notification
        .params
        .message
        .contains("newer Ottam version"));
}
