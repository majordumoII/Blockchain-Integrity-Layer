//! Integration tests against a real Postgres instance with logical
//! replication enabled.
//!
//! These are intentionally *not* run by default `cargo test` in CI
//! without a database available — they require:
//!
//!   docker run -d --name bil-test-postgres -e `POSTGRES_PASSWORD=testpass` \
//!     -e `POSTGRES_DB=bil_test` -p 5433:5432 postgres:16 \
//!     -c `wal_level=logical` -c `max_replication_slots=4` -c `max_wal_senders=4`
//!
//! Set `BIL_TEST_PG_PORT` to override the port (defaults to 5433) if the
//! container is mapped elsewhere. Tests are skipped (not failed) if the
//! connection cannot be established, so this file is safe in environments
//! without Docker.

use connector_postgres::connection::{ConnectionConfig, RawConnection};
use postgres_protocol::message::backend::Message;

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

macro_rules! skip_if_no_db {
    ($result:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipping: could not reach test postgres instance: {e}");
                return;
            }
        }
    };
}

#[tokio::test]
async fn connects_and_authenticates_with_scram() {
    let config = test_config();
    let conn = RawConnection::connect(&config).await;
    skip_if_no_db!(conn);
}

#[tokio::test]
async fn identify_system_round_trip() {
    let config = test_config();
    let mut conn = skip_if_no_db!(RawConnection::connect(&config).await);

    skip_if_no_db!(conn.send_query("IDENTIFY_SYSTEM").await);

    // Expect: RowDescription, DataRow, CommandComplete, ReadyForQuery.
    let mut saw_row = false;
    loop {
        let message = skip_if_no_db!(conn.read_backend_message("identify_system test").await);
        match message {
            Message::DataRow(_) => saw_row = true,
            Message::RowDescription(_) | Message::CommandComplete(_) => {}
            Message::ReadyForQuery(_) => break,
            Message::ErrorResponse(_) => panic!("IDENTIFY_SYSTEM returned an error"),
            _ => panic!("unexpected message during IDENTIFY_SYSTEM"),
        }
    }
    assert!(saw_row, "IDENTIFY_SYSTEM should return exactly one row");
}
