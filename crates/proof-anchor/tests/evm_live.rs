//! Live tests for [`EvmAnchor`] against a real local Anvil node (Foundry's
//! EVM simulator) — the same "skip, don't fail, if the dependency isn't
//! available" pattern `connector-postgres`'s `tests/*_live.rs` uses for a
//! real Postgres instance.
//!
//! Anvil is spawned fresh per test via `alloy`'s `node-bindings` feature,
//! so no external setup beyond having `anvil` on `PATH` (`brew install
//! foundry` or see <https://getfoundry.sh>) is required. Deploys the
//! actual checked-in `solidity/src/ProofAnchor.sol` build artifact — the
//! same one `examples/deploy_evm_anchor.rs` deploys to a real chain — so
//! these tests exercise the real contract bytecode, not a hand-rolled
//! stand-in.

use alloy::network::TransactionBuilder;
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use proof_anchor::{AnchorError, EvmAnchor, ProofAnchor};
use proof_core::ProofBuilder;
use proof_core::hash::{self as core_hash, HashAlgorithm};
use proof_core::sign::{SignatureAlgorithm, SigningPrivateKey};
use rand_core::OsRng;

macro_rules! skip_if_no_anvil {
    () => {
        if which::which("anvil").is_err() {
            eprintln!("skipping: `anvil` not found on PATH (install via `brew install foundry`)");
            return;
        }
    };
}

fn sample_proof(seed: &str) -> proof_core::Proof {
    let key = SigningPrivateKey::generate(SignatureAlgorithm::Ed25519, &mut OsRng);
    let digest = core_hash::hash(HashAlgorithm::Blake3, seed.as_bytes());
    ProofBuilder::new(digest, 1_753_270_000)
        .attest("test-signer", &key)
        .build()
        .unwrap()
}

fn contract_artifact_bytecode() -> Vec<u8> {
    let artifact_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../solidity/out/ProofAnchor.sol/ProofAnchor.json"
    );
    let bytes = std::fs::read(artifact_path).unwrap_or_else(|e| {
        panic!("reading {artifact_path}: {e} (run `cd solidity && forge build` first)")
    });
    let artifact: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let hex_object = artifact["bytecode"]["object"].as_str().unwrap();
    alloy::hex::decode(hex_object.strip_prefix("0x").unwrap_or(hex_object)).unwrap()
}

/// Spins up Anvil, deploys `ProofAnchor.sol` to it, and returns
/// `(EvmAnchor, rpc_url string, AnvilInstance)` — the instance must stay
/// alive for the RPC endpoint to keep responding, and the RPC URL is
/// kept so the caller can build a second, independent `EvmAnchor`
/// against the same chain if a test needs that.
async fn deploy_test_anchor() -> (EvmAnchor, String, alloy::node_bindings::AnvilInstance) {
    let anvil = alloy::node_bindings::Anvil::new().try_spawn().unwrap();
    let rpc_url = anvil.endpoint();
    let funder_signer: alloy::signers::local::PrivateKeySigner = anvil.keys()[0].clone().into();
    let provider = ProviderBuilder::new()
        .wallet(alloy::network::EthereumWallet::from(funder_signer))
        .connect_http(rpc_url.parse().unwrap());

    let bytecode = contract_artifact_bytecode();
    let deploy_tx = TransactionRequest::default().with_deploy_code(bytecode);
    let receipt = provider
        .send_transaction(deploy_tx)
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status(), "contract deployment must succeed");
    let address: Address = receipt.contract_address.unwrap();

    // EvmAnchor::connect takes ownership of a signer + RPC URL and
    // builds its own provider rather than reusing the anvil-wallet
    // provider above, exercising the exact construction path
    // `proof-service` will use in production.
    let signer = alloy::signers::local::PrivateKeySigner::random();
    fund_account(&provider, signer.address()).await;
    let anchor = EvmAnchor::connect(&rpc_url, signer, address).await.unwrap();

    (anchor, rpc_url, anvil)
}

/// `EvmAnchor::connect`'s signer needs gas funds to send transactions;
/// anvil's default accounts start funded, but the fresh random signer
/// used here does not, so it's topped up from anvil account 0.
async fn fund_account(provider: &impl Provider, to: Address) {
    let funder = provider.get_accounts().await.unwrap()[0];
    let tx = TransactionRequest::default()
        .with_from(funder)
        .with_to(to)
        .with_value(alloy::primitives::utils::parse_ether("1").unwrap());
    provider
        .send_transaction(tx)
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
}

#[tokio::test]
async fn anchor_and_verify_round_trip() {
    skip_if_no_anvil!();
    let (anchor, _rpc_url, _anvil) = deploy_test_anchor().await;

    let proof = sample_proof("record-1");
    let receipt = anchor.anchor(&proof).await.unwrap();

    anchor.verify(&receipt, &proof).await.unwrap();
}

#[tokio::test]
async fn verify_rejects_a_mismatched_proof() {
    skip_if_no_anvil!();
    let (anchor, _rpc_url, _anvil) = deploy_test_anchor().await;

    let proof_a = sample_proof("record-a");
    let proof_b = sample_proof("record-b");
    let receipt = anchor.anchor(&proof_a).await.unwrap();

    let result = anchor.verify(&receipt, &proof_b).await;
    assert!(matches!(
        result,
        Err(AnchorError::IntegrityViolation { .. })
    ));
}

#[tokio::test]
async fn verify_rejects_a_receipt_from_a_different_ledger() {
    skip_if_no_anvil!();
    let (anchor, _rpc_url, _anvil) = deploy_test_anchor().await;

    let proof = sample_proof("record-1");
    let mut receipt = anchor.anchor(&proof).await.unwrap();
    receipt.ledger_id = "evm:1:0x0000000000000000000000000000000000000000".to_string();

    let result = anchor.verify(&receipt, &proof).await;
    assert!(matches!(result, Err(AnchorError::WrongLedger { .. })));
}

#[tokio::test]
async fn verify_rejects_a_nonexistent_transaction() {
    skip_if_no_anvil!();
    let (anchor, _rpc_url, _anvil) = deploy_test_anchor().await;

    let proof = sample_proof("record-1");
    let bogus_receipt = proof_anchor::AnchorReceipt {
        ledger_id: anchor.ledger_id().to_string(),
        position_hex: "0".repeat(64),
    };

    let result = anchor.verify(&bogus_receipt, &proof).await;
    assert!(matches!(result, Err(AnchorError::NotFound)));
}

#[tokio::test]
async fn multiple_anchors_are_independently_verifiable() {
    skip_if_no_anvil!();
    let (anchor, _rpc_url, _anvil) = deploy_test_anchor().await;

    let proof_a = sample_proof("record-a");
    let proof_b = sample_proof("record-b");
    let receipt_a = anchor.anchor(&proof_a).await.unwrap();
    let receipt_b = anchor.anchor(&proof_b).await.unwrap();

    assert_ne!(receipt_a.position_hex, receipt_b.position_hex);
    anchor.verify(&receipt_a, &proof_a).await.unwrap();
    anchor.verify(&receipt_b, &proof_b).await.unwrap();

    // Cross-checking must fail: proof_a's digest was never anchored at
    // receipt_b's transaction.
    let result = anchor.verify(&receipt_b, &proof_a).await;
    assert!(matches!(
        result,
        Err(AnchorError::IntegrityViolation { .. })
    ));
}
