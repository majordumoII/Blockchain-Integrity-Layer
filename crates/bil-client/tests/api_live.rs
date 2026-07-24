//! Live test for [`bil_client::Client`] against a real `proof-service`
//! router, running in-process on a real TCP listener (not mocked HTTP).
//!
//! Uses `proof_anchor::LocalLogAnchor` (a real `ProofAnchor` impl backed
//! by a temp file) rather than a hand-rolled fake anchor — no network
//! dependency, but still a genuine ledger with its own tamper-evidence
//! chain, matching this workspace's preference for testing against real
//! things over mocks wherever a real, no-setup-required option exists.
//! Does not require Postgres or Anvil: `proof-service`'s `web::router`
//! and `AppState` are constructed directly, bypassing the CDC pipeline
//! entirely, since this test is about the HTTP/JSON client contract, not
//! the ingestion pipeline (that's covered by `connector-postgres` and
//! `proof-anchor`'s own live tests).

use bil_client::Client;
use proof_anchor::{AnchorReceipt, LocalLogAnchor, ProofAnchor};
use proof_connectors::{InProcessProofSink, ProofSink, ProvedRecord, SourceId};
use proof_core::ProofBuilder;
use proof_core::hash::{self as core_hash, HashAlgorithm};
use proof_core::sign::{SignatureAlgorithm, SigningPrivateKey};
use rand_core::OsRng;
use std::sync::Arc;

async fn spawn_test_service() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir for anchor log");
    let anchor_path = dir.path().join("anchor.log");
    let anchor = Arc::new(LocalLogAnchor::open(&anchor_path).expect("open local log anchor"));
    let anchor_dyn: Arc<dyn ProofAnchor> = anchor;

    let sink = Arc::new(InProcessProofSink::new(64, 16));

    let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
    let digest = core_hash::hash(HashAlgorithm::Blake3, b"api-live-test-record");
    let proof = ProofBuilder::new(digest, 1_753_270_000)
        .attest("test-signer", &key)
        .build()
        .expect("build sample proof");

    let receipt = anchor_dyn
        .anchor(&proof)
        .await
        .expect("anchor sample proof");

    sink.submit(ProvedRecord {
        source_id: SourceId::new("test-source"),
        source_position: "1".to_string(),
        proof: proof.clone(),
        receipt: receipt.clone(),
    })
    .await
    .expect("submit to sink");

    // Not installed globally (this test doesn't need a real /metrics
    // listener) — just enough of a handle to satisfy AppState, since the
    // compliance dashboard's aggregate view isn't what this test exercises.
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .build_recorder()
        .handle();

    let app = proof_service::web::router(proof_service::web::AppState {
        sink,
        anchor: anchor_dyn,
        metrics_handle,
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    (format!("http://{addr}"), dir)
}

fn sample_proof_and_key() -> (proof_core::Proof, SigningPrivateKey) {
    let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
    let digest = core_hash::hash(HashAlgorithm::Blake3, b"api-live-test-record");
    let proof = ProofBuilder::new(digest, 1_753_270_000)
        .attest("test-signer", &key)
        .build()
        .expect("build sample proof");
    (proof, key)
}

#[tokio::test]
async fn get_proof_finds_a_seeded_digest() {
    let (base_url, _dir) = spawn_test_service().await;
    let client = Client::new(&base_url).expect("construct client");

    let (proof, _key) = sample_proof_and_key();
    let digest_hex = proof.digest().to_hex();

    let found = client
        .get_proof(&digest_hex)
        .await
        .expect("request should succeed");

    let record = found.expect("proof should be found");
    assert_eq!(record.proof.digest().to_hex(), digest_hex);
    assert_eq!(record.source_id, "test-source");
}

#[tokio::test]
async fn get_proof_returns_none_for_unknown_digest() {
    let (base_url, _dir) = spawn_test_service().await;
    let client = Client::new(&base_url).expect("construct client");

    let unknown_digest = "0".repeat(64);
    let found = client
        .get_proof(&unknown_digest)
        .await
        .expect("request should succeed");

    assert!(found.is_none());
}

#[tokio::test]
async fn verify_accepts_the_genuine_seeded_proof() {
    let (base_url, _dir) = spawn_test_service().await;
    let client = Client::new(&base_url).expect("construct client");

    let (proof, _key) = sample_proof_and_key();
    let digest_hex = proof.digest().to_hex();
    let record = client
        .get_proof(&digest_hex)
        .await
        .expect("request should succeed")
        .expect("proof should be found");

    let result = client
        .verify(record.proof, record.receipt)
        .await
        .expect("verify request should succeed");

    assert!(
        result.valid,
        "expected valid, got reason: {:?}",
        result.reason
    );
    assert_eq!(result.digest_hex, digest_hex);
}

#[tokio::test]
async fn verify_rejects_a_mismatched_receipt() {
    let (base_url, _dir) = spawn_test_service().await;
    let client = Client::new(&base_url).expect("construct client");

    let (proof, _key) = sample_proof_and_key();
    let digest_hex = proof.digest().to_hex();
    let record = client
        .get_proof(&digest_hex)
        .await
        .expect("request should succeed")
        .expect("proof should be found");

    let bogus_receipt = AnchorReceipt {
        ledger_id: record.receipt.ledger_id.clone(),
        position_hex: "9999".to_string(),
    };

    let result = client
        .verify(record.proof, bogus_receipt)
        .await
        .expect("verify request should succeed");

    assert!(!result.valid);
    assert!(result.reason.is_some());
}
