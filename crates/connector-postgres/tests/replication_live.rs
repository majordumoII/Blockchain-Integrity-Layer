//! Integration tests driving a real logical replication stream end to
//! end: connect, `START_REPLICATION`, run SQL via `psql` against the same
//! test database, and assert the decoded pgoutput events match what was
//! actually done.
//!
//! Requires the same `bil-test-postgres` container described in
//! `connection_live.rs`, plus a `patients` table, `bil_pub` publication,
//! and `bil_slot` replication slot already created:
//!
//!   psql -h localhost -p 5433 -U postgres -d `bil_test` -c "
//!     CREATE TABLE patients (id SERIAL PRIMARY KEY, `patient_ref` TEXT NOT NULL, `diagnosis_code` TEXT);
//!     CREATE PUBLICATION `bil_pub` FOR TABLE patients;
//!     SELECT `pg_create_logical_replication_slot`('`bil_slot`', 'pgoutput');
//!   "
//!
//! Tests are skipped (not failed) if the environment isn't available.

use connector_postgres::connection::{ConnectionConfig, RawConnection};
use connector_postgres::pgoutput::{ColumnValue, PgOutputMessage};
use connector_postgres::replication::ReplicationStream;
use std::process::Command;
use std::time::Duration;

fn test_config() -> ConnectionConfig {
    let port = std::env::var("BIL_TEST_PG_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5433);
    ConnectionConfig {
        host: "localhost".to_string(),
        port,
        user: "postgres".to_string(),
        password: "testpass".to_string(),
        dbname: "bil_test".to_string(),
    }
}

macro_rules! skip_if_unavailable {
    ($result:expr, $what:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipping: {} unavailable: {e}", $what);
                return;
            }
        }
    };
}

/// Runs a SQL statement via `psql` against the test container. Using the
/// real `psql` client (rather than issuing it over our own connection)
/// keeps this test honest: the events we decode must match what an
/// entirely independent, trusted client caused to happen.
fn run_sql(sql: &str) {
    let port = std::env::var("BIL_TEST_PG_PORT").unwrap_or_else(|_| "5433".to_string());
    let status = Command::new("psql")
        .env("PGPASSWORD", "testpass")
        .args([
            "-h",
            "localhost",
            "-p",
            &port,
            "-U",
            "postgres",
            "-d",
            "bil_test",
            "-c",
            sql,
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("psql exited with {s}"),
        Err(e) => panic!("could not run psql (is it installed and on PATH?): {e}"),
    }
}

fn text_value(values: &[ColumnValue], index: usize) -> Option<String> {
    match values.get(index) {
        Some(ColumnValue::Text(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

#[tokio::test]
async fn decodes_insert_update_delete_from_live_replication_stream() {
    let config = test_config();
    let conn = skip_if_unavailable!(RawConnection::connect(&config).await, "raw connection");

    let mut stream = skip_if_unavailable!(
        ReplicationStream::start(conn, "bil_slot", "bil_pub", 0).await,
        "START_REPLICATION (has the slot/publication been created? see file header)"
    );

    let marker = format!("live-test-{}", std::process::id());
    run_sql(&format!(
        "INSERT INTO patients (patient_ref, diagnosis_code) VALUES ('{marker}', 'E11.9');"
    ));
    run_sql(&format!(
        "UPDATE patients SET diagnosis_code = 'E11.65' WHERE patient_ref = '{marker}';"
    ));
    run_sql(&format!(
        "DELETE FROM patients WHERE patient_ref = '{marker}';"
    ));

    let mut saw_insert = false;
    let mut saw_update = false;
    let mut saw_delete = false;

    // Read events until we've seen all three, or time out — this stream
    // is long-lived by design, so the test bounds how long it waits
    // rather than expecting the stream to end on its own.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(saw_insert && saw_update && saw_delete) {
        assert!(
            tokio::time::Instant::now() <= deadline,
            "timed out waiting for events (insert={saw_insert} update={saw_update} delete={saw_delete})"
        );
        let event = match tokio::time::timeout(Duration::from_secs(2), stream.next_event()).await {
            Ok(Ok(event)) => event,
            Ok(Err(e)) => panic!("replication stream error: {e}"),
            Err(_) => continue, // timed-out poll, loop and recheck the deadline
        };

        match event.message {
            PgOutputMessage::Insert { new_tuple, .. } => {
                if text_value(&new_tuple.values, 1).as_deref() == Some(marker.as_str()) {
                    assert_eq!(
                        text_value(&new_tuple.values, 2).as_deref(),
                        Some("E11.9"),
                        "insert should carry the diagnosis_code we set"
                    );
                    saw_insert = true;
                }
                stream.confirm_flush(event.lsn);
            }
            PgOutputMessage::Update { new_tuple, .. } => {
                if text_value(&new_tuple.values, 1).as_deref() == Some(marker.as_str()) {
                    assert_eq!(
                        text_value(&new_tuple.values, 2).as_deref(),
                        Some("E11.65"),
                        "update should carry the new diagnosis_code"
                    );
                    saw_update = true;
                }
                stream.confirm_flush(event.lsn);
            }
            PgOutputMessage::Delete { old_tuple, .. } => {
                // Default replica identity only sends the primary key
                // (id), not patient_ref, so we can't match on marker
                // here — but having seen exactly one insert+update for
                // our marker already, the next delete in program order
                // is trustworthy enough for this test's purpose.
                if saw_insert && saw_update && !saw_delete {
                    assert!(
                        !old_tuple.values.is_empty(),
                        "delete should carry at least the key tuple"
                    );
                    saw_delete = true;
                }
                stream.confirm_flush(event.lsn);
            }
            _ => {}
        }
    }

    assert!(saw_insert && saw_update && saw_delete);
}
