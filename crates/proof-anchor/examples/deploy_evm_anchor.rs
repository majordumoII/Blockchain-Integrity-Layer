//! Deploys `solidity/src/ProofAnchor.sol` to any EVM JSON-RPC endpoint
//! and prints the resulting contract address.
//!
//! Reads bytecode from the checked-in Foundry build artifact
//! (`solidity/out/ProofAnchor.sol/ProofAnchor.json`) rather than
//! requiring `forge` at deploy time — only rebuilding the contract
//! itself requires Foundry, deploying the already-compiled bytecode does
//! not.
//!
//! ```bash
//! BIL_EVM_RPC_URL=https://sepolia.base.org \
//! BIL_EVM_DEPLOYER_PRIVATE_KEY=0x... \
//!   cargo run -p proof-anchor --example deploy_evm_anchor
//! ```
//!
//! The deployer key needs a small amount of the target chain's native
//! gas token (testnet ETH on Base Sepolia, available from a public
//! faucet) to pay for contract creation. Print the resulting address and
//! set it as `BIL_EVM_CONTRACT_ADDRESS` for `proof-service` and for
//! `EvmAnchor::connect` callers.

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rpc_url = std::env::var("BIL_EVM_RPC_URL")
        .map_err(|_| "set BIL_EVM_RPC_URL to the target chain's JSON-RPC endpoint")?;
    let private_key = std::env::var("BIL_EVM_DEPLOYER_PRIVATE_KEY")
        .map_err(|_| "set BIL_EVM_DEPLOYER_PRIVATE_KEY to a funded account's private key")?;

    let signer: PrivateKeySigner = private_key.parse()?;
    let deployer = signer.address();
    let wallet = EthereumWallet::from(signer);
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url.parse()?);

    let bytecode = load_bytecode()?;
    println!("deploying ProofAnchor.sol from {deployer} to {rpc_url}...");

    let deploy_tx = TransactionRequest::default().with_deploy_code(bytecode);
    let receipt = provider
        .send_transaction(deploy_tx)
        .await?
        .get_receipt()
        .await?;

    if !receipt.status() {
        return Err("deployment transaction reverted".into());
    }
    let address = receipt
        .contract_address
        .ok_or("deployment transaction receipt has no contract address")?;

    println!("deployed ProofAnchor contract at {address}");
    println!("transaction: {:?}", receipt.transaction_hash);
    println!("\nset this for proof-service / EvmAnchor::connect:");
    println!("  BIL_EVM_CONTRACT_ADDRESS={address}");

    Ok(())
}

/// Path is relative to this crate (`crates/proof-anchor`), matching
/// where `cargo run --example` sets the working directory.
fn load_bytecode() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let artifact_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../solidity/out/ProofAnchor.sol/ProofAnchor.json"
    );
    let artifact_bytes = std::fs::read(artifact_path).map_err(|e| {
        format!(
            "reading {artifact_path}: {e} (run `cd solidity && forge build` first if this is missing)"
        )
    })?;
    let artifact: serde_json::Value = serde_json::from_slice(&artifact_bytes)?;
    let hex_object = artifact["bytecode"]["object"]
        .as_str()
        .ok_or("artifact JSON missing bytecode.object")?;
    let hex_digits = hex_object.strip_prefix("0x").unwrap_or(hex_object);
    Ok(alloy::hex::decode(hex_digits)?)
}
