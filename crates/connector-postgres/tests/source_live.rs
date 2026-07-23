//! Integration test driving the full `PostgresSource` (the
//! `proof_connectors::RecordSource` impl) end to end against a live
//! Postgres instance: connect, read a `SourceRecord` for a real INSERT,
//! decode its canonical bytes back into a `RowChange`, and acknowledge
//! it.
//!
//! Requires the same container/table/publication/slot as
//! `replication_live.rs`.

use connector_postgres::PostgresSource;
use connector_postgres::change::RowChange;
use connector_postgres::connection::{ConnectionConfig, RawConnection};
use proof_connectors::{RecordSource, SourceId};
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
        Err(e) => panic!("could not run psql: {e}"),
    }
}

#[tokio::test]
async fn source_yields_decodable_ackable_record_for_insert() {
    let config = test_config();
    let conn = match RawConnection::connect(&config).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping: raw connection unavailable: {e}");
            return;
        }
    };

    let mut source = match PostgresSource::connect(
        SourceId::new("postgres:patients"),
        conn,
        "bil_slot",
        "bil_pub",
        0,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "skipping: PostgresSource::connect failed (has the slot/publication been created?): {e}"
            );
            return;
        }
    };

    let marker = format!("source-live-test-{}", std::process::id());
    run_sql(&format!(
        "INSERT INTO patients (patient_ref, diagnosis_code) VALUES ('{marker}', 'Z00.00');"
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() <= deadline,
            "timed out waiting for our INSERT to appear as a SourceRecord"
        );
        let (record, ack) = match tokio::time::timeout(Duration::from_secs(2), source.next()).await
        {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => panic!("source error: {e}"),
            Err(_) => continue,
        };

        let change: RowChange = bincode::deserialize(&record.bytes)
            .expect("SourceRecord.bytes must decode as a RowChange");

        let patient_ref_matches = change.columns.iter().any(|(name, value)| {
            name == "patient_ref"
                && matches!(
                    value,
                    connector_postgres::change::OwnedColumnValue::Text(bytes)
                        if bytes == marker.as_bytes()
                )
        });
        if !patient_ref_matches {
            // Some other change (from a prior test run, etc.); ack it so
            // the slot advances and keep looking for ours.
            ack.ack().await.expect("ack should succeed");
            continue;
        }

        assert_eq!(record.metadata_value("table"), Some("patients"));
        assert_eq!(change.table, "patients");
        assert!(!record.source_position.is_empty());

        ack.ack().await.expect("ack should succeed for our record");
        break;
    }
}
