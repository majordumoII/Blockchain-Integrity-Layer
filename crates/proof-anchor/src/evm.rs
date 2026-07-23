//! [`EvmAnchor`]: a [`ProofAnchor`] backed by a minimal EVM smart
//! contract (`solidity/src/ProofAnchor.sol`), the first genuinely
//! chain-backed anchoring implementation.
//!
//! Only a [`Proof`]'s 32-byte digest is ever sent on-chain — never the
//! full canonical proof bytes — matching the README's explicit "what the
//! blockchain never stores" stance. This is why [`ProofAnchor::verify`]
//! takes the `Proof` being checked as an argument rather than
//! reconstructing one from ledger data: an EVM anchor has nothing to
//! reconstruct a full proof from, only enough to confirm a given proof's
//! digest was actually committed at a receipt's position.
//!
//! Deliberately chain-agnostic beyond "some EVM JSON-RPC endpoint" — the
//! same code works against Base Sepolia, any other EVM testnet/mainnet,
//! or a permissioned EVM chain (e.g. Hyperledger Besu), since `alloy`
//! only needs an RPC URL and a signer. Nothing here is Base-specific.

use crate::anchor::{AnchorError, AnchorReceipt, ProofAnchor};
use alloy::network::{Ethereum, EthereumWallet};
use alloy::primitives::{Address, B256, TxHash};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use proof_core::Proof;
use std::str::FromStr;

sol! {
    #[sol(rpc)]
    contract ProofAnchorContract {
        event ProofAnchored(bytes32 indexed digest, uint256 indexed index);

        function anchor(bytes32 digest) external returns (uint256 index);
    }
}

/// A [`ProofAnchor`] that commits proof digests to a deployed
/// `ProofAnchor.sol` contract on any EVM-compatible chain.
///
/// `ledger_id` is `"evm:<chain_id>:<contract_address>"`, so a receipt is
/// self-describing about which chain and contract to check it against
/// without the caller needing out-of-band knowledge.
pub struct EvmAnchor {
    ledger_id: String,
    contract_address: Address,
    provider: DynProvider<Ethereum>,
}

impl EvmAnchor {
    /// Connects to `rpc_url` and prepares to anchor against
    /// `contract_address`, signing transactions with `signer`.
    ///
    /// # Errors
    ///
    /// Returns [`AnchorError::Storage`] if `rpc_url` cannot be parsed or
    /// the chain ID cannot be fetched from the endpoint.
    pub async fn connect(
        rpc_url: &str,
        signer: PrivateKeySigner,
        contract_address: Address,
    ) -> Result<Self, AnchorError> {
        let url = rpc_url
            .parse()
            .map_err(|e| AnchorError::Storage(format!("invalid RPC URL {rpc_url:?}: {e}")))?;
        let wallet = EthereumWallet::from(signer);
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(url)
            .erased();

        let chain_id = provider
            .get_chain_id()
            .await
            .map_err(|e| AnchorError::Storage(format!("fetching chain id: {e}")))?;

        Ok(Self {
            ledger_id: format!("evm:{chain_id}:{contract_address:?}"),
            contract_address,
            provider,
        })
    }

    fn digest_to_word(proof: &Proof) -> B256 {
        B256::from(*proof.digest().as_bytes())
    }
}

#[async_trait::async_trait]
impl ProofAnchor for EvmAnchor {
    fn ledger_id(&self) -> &str {
        &self.ledger_id
    }

    async fn anchor(&self, proof: &Proof) -> Result<AnchorReceipt, AnchorError> {
        // Anchoring bytecode is fixed and validated at compile time by
        // `sol!`; the only way encoding the call can fail is if the
        // digest itself is malformed, which `Digest` never produces.
        let _ = proof.to_canonical_bytes()?;

        let contract = ProofAnchorContract::new(self.contract_address, &self.provider);
        let digest_word = Self::digest_to_word(proof);

        let pending = contract
            .anchor(digest_word)
            .send()
            .await
            .map_err(|e| AnchorError::Storage(format!("sending anchor transaction: {e}")))?;
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|e| AnchorError::Storage(format!("awaiting anchor receipt: {e}")))?;

        if !receipt.status() {
            return Err(AnchorError::Storage(format!(
                "anchor transaction {:?} reverted",
                receipt.transaction_hash
            )));
        }

        Ok(AnchorReceipt {
            ledger_id: self.ledger_id.clone(),
            position_hex: format!("{:x}", receipt.transaction_hash),
        })
    }

    async fn verify(&self, receipt: &AnchorReceipt, proof: &Proof) -> Result<(), AnchorError> {
        if receipt.ledger_id != self.ledger_id {
            return Err(AnchorError::WrongLedger {
                receipt_ledger: receipt.ledger_id.clone(),
                this_ledger: self.ledger_id.clone(),
            });
        }

        let tx_hash = TxHash::from_str(&receipt.position_hex).map_err(|e| {
            AnchorError::Storage(format!(
                "malformed receipt position {:?}: {e}",
                receipt.position_hex
            ))
        })?;

        let tx_receipt = self
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|e| AnchorError::Storage(format!("fetching transaction receipt: {e}")))?
            .ok_or(AnchorError::NotFound)?;

        if !tx_receipt.status() {
            return Err(AnchorError::IntegrityViolation {
                position: 0,
                reason: "anchoring transaction reverted on-chain".to_string(),
            });
        }

        let expected_digest = Self::digest_to_word(proof);
        let anchored = tx_receipt
            .logs()
            .iter()
            .filter_map(|log| log.log_decode::<ProofAnchorContract::ProofAnchored>().ok())
            .any(|decoded| decoded.inner.data.digest == expected_digest);

        if !anchored {
            return Err(AnchorError::IntegrityViolation {
                position: 0,
                reason:
                    "no ProofAnchored event in this transaction matches the supplied proof's digest"
                        .to_string(),
            });
        }

        Ok(())
    }
}
