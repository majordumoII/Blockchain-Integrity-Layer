//! `proof-service`: single binary running a connector's observe→hash→sign
//! pipeline alongside a live-activity UI and Prometheus metrics.
//!
//! See `CLAUDE.md`'s "deployment topology decision" for why this is one
//! process rather than split services at this stage, and
//! `proof_connectors::ProofSink` for the trait boundary that keeps a
//! later split cheap if/when it's warranted.

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use clap::Parser;
use connector_postgres::PostgresSource;
use connector_postgres::connection::{ConnectionConfig, RawConnection};
use proof_anchor::{EvmAnchor, LocalLogAnchor, ProofAnchor};
use proof_connectors::{InProcessProofSink, ProofSink, SourceId};
use proof_core::sign::{SignatureAlgorithm, SigningPrivateKey};
use rand_core::OsRng;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Command-line / environment configuration.
///
/// Every field is also readable from an environment variable (`clap`'s
/// `env` feature) since a service like this is normally configured via
/// its deployment environment, not a hand-typed command line.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Postgres host to connect to for logical replication.
    #[arg(long, env = "BIL_PG_HOST", default_value = "localhost")]
    pg_host: String,

    #[arg(long, env = "BIL_PG_PORT", default_value_t = 5432)]
    pg_port: u16,

    #[arg(long, env = "BIL_PG_USER", default_value = "postgres")]
    pg_user: String,

    #[arg(long, env = "BIL_PG_PASSWORD")]
    pg_password: String,

    #[arg(long, env = "BIL_PG_DBNAME")]
    pg_dbname: String,

    /// Pre-created logical replication slot name (see CLAUDE.md for the
    /// one-time `CREATE PUBLICATION` / slot setup this connector requires).
    #[arg(long, env = "BIL_PG_SLOT", default_value = "bil_slot")]
    pg_slot: String,

    /// Pre-created publication name(s), comma-separated.
    #[arg(long, env = "BIL_PG_PUBLICATION", default_value = "bil_pub")]
    pg_publication: String,

    /// Label for this source in metrics/proofs, e.g. "postgres:patients".
    #[arg(long, env = "BIL_SOURCE_ID", default_value = "postgres:default")]
    source_id: String,

    /// Address the web UI listens on.
    #[arg(long, env = "BIL_WEB_ADDR", default_value = "127.0.0.1:8080")]
    web_addr: SocketAddr,

    /// Address the Prometheus `/metrics` endpoint listens on.
    #[arg(long, env = "BIL_METRICS_ADDR", default_value = "127.0.0.1:9090")]
    metrics_addr: SocketAddr,

    /// Which `ProofAnchor` backend to anchor proofs to.
    #[arg(long, env = "BIL_ANCHOR_BACKEND", value_enum, default_value_t = AnchorBackend::Local)]
    anchor_backend: AnchorBackend,

    /// Path to the local hash-chained anchor log. Only used when
    /// `anchor_backend = local`. See `proof-anchor`'s `LocalLogAnchor`.
    #[arg(long, env = "BIL_ANCHOR_LOG_PATH", default_value = "./anchor.log")]
    anchor_log_path: PathBuf,

    /// EVM JSON-RPC endpoint to anchor to. Required when
    /// `anchor_backend = evm`. Works against any EVM-compatible chain —
    /// a public testnet (e.g. Base Sepolia), mainnet, or a permissioned
    /// EVM chain — `EvmAnchor` has no chain-specific logic.
    #[arg(long, env = "BIL_EVM_RPC_URL")]
    evm_rpc_url: Option<String>,

    /// Private key (hex, `0x`-prefixed) used to sign anchoring
    /// transactions. Required when `anchor_backend = evm`. This key only
    /// needs enough of the chain's native gas token to pay for
    /// transactions — it never touches record data or `proof-core`
    /// signing keys.
    #[arg(long, env = "BIL_EVM_PRIVATE_KEY")]
    evm_private_key: Option<String>,

    /// Address of a deployed `solidity/src/ProofAnchor.sol` contract.
    /// Required when `anchor_backend = evm`. See
    /// `crates/proof-anchor/examples/deploy_evm_anchor.rs` for deploying
    /// one.
    #[arg(long, env = "BIL_EVM_CONTRACT_ADDRESS")]
    evm_contract_address: Option<String>,
}

/// Which [`ProofAnchor`] implementation `proof-service` anchors proofs
/// to. See CLAUDE.md's "deployment topology decision" and `proof-anchor`'s
/// own docs for why this is a trait-object seam rather than a compile-time
/// choice — swapping backends must never require touching the pipeline.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum AnchorBackend {
    /// `LocalLogAnchor`: a hash-chained local file. No network/consensus
    /// dependency, but trust rests on whoever holds that file.
    Local,
    /// `EvmAnchor`: a deployed EVM smart contract. Genuinely chain-backed,
    /// third-party verifiable without trusting this service.
    Evm,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let metrics_handle = proof_service::metrics::install(args.metrics_addr);
    info!(addr = %args.metrics_addr, "Prometheus metrics listening");

    // Generated fresh at startup: this stage of the project has no key
    // persistence/rotation story yet, so every proof produced by a given
    // run is attested by the same in-memory key, and a restart begins a
    // new signer identity. Proofs already produced remain independently
    // verifiable against the public key logged below regardless of what
    // the service does afterward.
    let signing_key = Arc::new(SigningPrivateKey::generate(
        SignatureAlgorithm::Ed25519,
        &mut OsRng,
    ));
    info!(
        public_key = ?signing_key.public_key(),
        "generated signing key for this run"
    );

    let source_id = SourceId::new(args.source_id);
    let conn = RawConnection::connect(&ConnectionConfig {
        host: args.pg_host,
        port: args.pg_port,
        user: args.pg_user,
        password: args.pg_password,
        dbname: args.pg_dbname,
    })
    .await?;
    let source = PostgresSource::connect(
        source_id.clone(),
        conn,
        &args.pg_slot,
        &args.pg_publication,
        0,
    )
    .await?;
    info!(source = %source_id, "connected Postgres logical replication source");

    let sink = Arc::new(InProcessProofSink::new(200, 64));
    let sink_dyn: Arc<dyn ProofSink> = sink.clone();

    let anchor_dyn: Arc<dyn ProofAnchor> = match args.anchor_backend {
        AnchorBackend::Local => {
            let anchor = Arc::new(LocalLogAnchor::open(&args.anchor_log_path)?);
            info!(
                ledger = %anchor.ledger_id(),
                "anchoring proofs to a local hash-chained log"
            );
            anchor
        }
        AnchorBackend::Evm => {
            let rpc_url = args
                .evm_rpc_url
                .ok_or("BIL_EVM_RPC_URL is required when --anchor-backend=evm")?;
            let private_key = args
                .evm_private_key
                .ok_or("BIL_EVM_PRIVATE_KEY is required when --anchor-backend=evm")?;
            let contract_address = args
                .evm_contract_address
                .ok_or("BIL_EVM_CONTRACT_ADDRESS is required when --anchor-backend=evm")?;

            let signer: PrivateKeySigner = private_key
                .parse()
                .map_err(|e| format!("invalid BIL_EVM_PRIVATE_KEY: {e}"))?;
            let contract_address: Address = contract_address
                .parse()
                .map_err(|e| format!("invalid BIL_EVM_CONTRACT_ADDRESS: {e}"))?;

            let anchor = Arc::new(EvmAnchor::connect(&rpc_url, signer, contract_address).await?);
            info!(
                ledger = %anchor.ledger_id(),
                "anchoring proofs to an EVM smart contract"
            );
            anchor
        }
    };

    tokio::spawn(proof_service::pipeline::run(
        source,
        sink_dyn,
        anchor_dyn.clone(),
        signing_key,
        source_id,
    ));

    let app = proof_service::web::router(proof_service::web::AppState {
        sink,
        anchor: anchor_dyn,
        metrics_handle,
    });
    info!(addr = %args.web_addr, "web UI listening");
    let listener = tokio::net::TcpListener::bind(args.web_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
