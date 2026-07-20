//! Coin Flip settlement service.
//!
//! This service exists for exactly one reason: the house authority key authorizes every
//! payout, so it can NEVER live in browser code. Anyone holding it can settle any session
//! as a win and drain the vaults. It stays server-side, here.
//!
//! Responsibilities:
//!   1. Read the session PDA on-chain and confirm it is genuinely Open.
//!   2. Flip the coin (server-side RNG — Arch exposes no on-chain randomness).
//!   3. Submit SettleSession, signed by the house authority.
//!
//! The player opens their own session directly from their wallet. This service never
//! touches player funds and cannot open sessions on their behalf.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use arch_program::account::AccountMeta;
use arch_program::instruction::Instruction;
use arch_program::pubkey::Pubkey;
use arch_program::sanitized::ArchMessage;
use arch_program::system_program::SYSTEM_PROGRAM_ID;
use arch_sdk::blocking::ArchRpcClient;
use arch_sdk::{build_and_sign_transaction, Config, Status};
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use bitcoin::key::UntweakedKeypair;
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use bitcoin::XOnlyPublicKey;
use borsh::BorshDeserialize;
use coinflip_escrow::{EscrowInstruction, GameSession, STATUS_OPEN, STATUS_WON};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

// ---------------------------------------------------------------------------
// Config / state
// ---------------------------------------------------------------------------

struct AppState {
    config: Config,
    program_id: Pubkey,
    authority_kp: UntweakedKeypair,
    authority_pk: Pubkey,
    /// Idempotency cache: (player, session_id) -> outcome already returned.
    /// The on-chain terminal-state guard is the real protection; this stops a
    /// retried request from returning a confusing error after a successful settle.
    settled: Mutex<HashMap<(String, u64), SettleResponse>>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Build the network config. Defaults to Arch testnet; `Config::testnet()` ships with
/// EMPTY endpoints in arch_sdk 0.6.7, so they must be filled in explicitly.
fn load_config() -> Config {
    let network = env_or("ARCH_NETWORK", "testnet");
    if network == "localnet" {
        return Config::localnet();
    }
    let mut c = Config::testnet();
    c.arch_node_url = env_or("ARCH_RPC_URL", "https://rpc.testnet.arch.network");
    c.titan_url = env_or("ARCH_TITAN_URL", "https://titan.testnet.arch.network");
    c.node_endpoint = env_or("BITCOIN_RPC_URL", "http://bitcoin-rpc.test.arch.network:80");
    c.node_username = env_or("BITCOIN_RPC_USER", "bitcoin");
    c.node_password = env_or(
        "BITCOIN_RPC_PASSWORD",
        "0F_Ed53o4kR7nxh3xNaSQx-2M3TY16L55mz5y9fjdrk",
    );
    c
}

/// Load the house authority key from the environment.
///
/// Accepts a 64-char hex secret key (HOUSE_AUTHORITY_SECRET_KEY) or, for local dev only,
/// a path to a keypair file (HOUSE_AUTHORITY_KEY_FILE). The env var is preferred because
/// it is what a hosting platform's secret store gives you.
fn load_authority() -> Result<(UntweakedKeypair, Pubkey), String> {
    let secp = Secp256k1::new();

    let secret = if let Ok(hex_key) = std::env::var("HOUSE_AUTHORITY_SECRET_KEY") {
        SecretKey::from_slice(
            &hex::decode(hex_key.trim()).map_err(|_| "HOUSE_AUTHORITY_SECRET_KEY is not valid hex")?,
        )
        .map_err(|_| "HOUSE_AUTHORITY_SECRET_KEY is not a valid secp256k1 key")?
    } else if let Ok(path) = std::env::var("HOUSE_AUTHORITY_KEY_FILE") {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
        // Same dual format the SDK accepts: hex string, or JSON array of bytes.
        match hex::decode(raw.trim()) {
            Ok(bytes) if bytes.len() >= 32 => SecretKey::from_slice(&bytes[..32])
                .map_err(|_| "key file did not contain a valid secp256k1 key")?,
            _ => {
                let bytes: Vec<u8> = serde_json::from_str(&raw)
                    .map_err(|_| "key file is neither hex nor a JSON byte array")?;
                if bytes.len() < 32 {
                    return Err("key file byte array shorter than 32 bytes".into());
                }
                SecretKey::from_slice(&bytes[..32])
                    .map_err(|_| "key file did not contain a valid secp256k1 key")?
            }
        }
    } else {
        return Err(
            "set HOUSE_AUTHORITY_SECRET_KEY (hex) or HOUSE_AUTHORITY_KEY_FILE (path)".into(),
        );
    };

    let kp = UntweakedKeypair::from_secret_key(&secp, &secret);
    let pk = Pubkey::from_slice(&XOnlyPublicKey::from_keypair(&kp).0.serialize());
    Ok((kp, pk))
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SettleRequest {
    /// Player pubkey, 64-char hex.
    player: String,
    session_id: u64,
}

#[derive(Serialize, Clone, Debug)]
struct SettleResponse {
    player_won: bool,
    status: String,
    session_id: u64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    network: String,
    program_id: String,
    house_authority: String,
    block_height: Option<u64>,
}

fn parse_pubkey(s: &str) -> Result<Pubkey, String> {
    let bytes = hex::decode(s.trim()).map_err(|_| "player must be hex".to_string())?;
    if bytes.len() != 32 {
        return Err(format!("player must be 32 bytes, got {}", bytes.len()));
    }
    Ok(Pubkey::from_slice(&bytes))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let cfg = state.config.clone();
    let height = tokio::task::spawn_blocking(move || {
        ArchRpcClient::new(&cfg).get_block_count().ok()
    })
    .await
    .ok()
    .flatten();

    Json(HealthResponse {
        ok: true,
        network: state.config.arch_node_url.clone(),
        program_id: state.program_id.to_string(),
        house_authority: state.authority_pk.to_string(),
        block_height: height,
    })
}

async fn settle(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SettleRequest>,
) -> Result<Json<SettleResponse>, (StatusCode, Json<ErrorResponse>)> {
    let bad = |code: StatusCode, msg: String| (code, Json(ErrorResponse { error: msg }));

    let player_pk =
        parse_pubkey(&req.player).map_err(|e| bad(StatusCode::BAD_REQUEST, e))?;
    let cache_key = (req.player.trim().to_lowercase(), req.session_id);

    // Idempotency: a retried request returns the original outcome rather than an error.
    if let Some(prev) = state.settled.lock().await.get(&cache_key) {
        return Ok(Json(prev.clone()));
    }

    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || settle_blocking(state2, player_pk, req.session_id))
        .await
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, format!("task panic: {e}")))?;

    match result {
        Ok(resp) => {
            state.settled.lock().await.insert(cache_key, resp.clone());
            Ok(Json(resp))
        }
        Err(SettleError::BadRequest(m)) => Err(bad(StatusCode::BAD_REQUEST, m)),
        Err(SettleError::Conflict(m)) => Err(bad(StatusCode::CONFLICT, m)),
        Err(SettleError::Internal(m)) => Err(bad(StatusCode::INTERNAL_SERVER_ERROR, m)),
    }
}

enum SettleError {
    BadRequest(String),
    Conflict(String),
    Internal(String),
}

/// The actual on-chain work. Blocking, so it runs on a blocking thread.
fn settle_blocking(
    state: Arc<AppState>,
    player_pk: Pubkey,
    session_id: u64,
) -> Result<SettleResponse, SettleError> {
    let client = ArchRpcClient::new(&state.config);
    let program_id = state.program_id;

    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &program_id);
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) =
        Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    // --- Verify the session exists and is genuinely Open, BEFORE flipping. ---
    // Without this we would happily flip a coin for a session that does not exist,
    // or one already settled, and only discover it when the chain rejects us.
    let session_account = client
        .read_account_info(session_pda)
        .map_err(|_| SettleError::BadRequest("session not found on-chain".into()))?;

    if session_account.data.len() < GameSession::LEN {
        return Err(SettleError::BadRequest("session account malformed".into()));
    }
    let session = GameSession::try_from_slice(&session_account.data[..GameSession::LEN])
        .map_err(|_| SettleError::BadRequest("session could not be decoded".into()))?;

    if session.player != player_pk {
        return Err(SettleError::BadRequest("session belongs to a different player".into()));
    }
    if session.status != STATUS_OPEN {
        return Err(SettleError::Conflict("session already settled".into()));
    }

    // --- The coin flip. Server-side RNG, because Arch has no on-chain randomness. ---
    // Trust assumption: the house is trusted to report this honestly. Documented, not hidden.
    let player_won: bool = rand::thread_rng().gen_bool(0.5);

    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(state.authority_pk, true),
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(session_pda, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new(player_pk, false),
            AccountMeta::new(state.authority_pk, true), // house treasury
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: borsh::to_vec(&EscrowInstruction::SettleSession { player_won })
            .map_err(|e| SettleError::Internal(format!("serialize: {e}")))?,
    };

    let blockhash = client
        .get_best_finalized_block_hash()
        .map_err(|e| SettleError::Internal(format!("blockhash: {e}")))?;

    let tx = build_and_sign_transaction(
        ArchMessage::new(&[ix], Some(state.authority_pk), blockhash),
        vec![state.authority_kp],
        state.config.network,
    )
    .map_err(|e| SettleError::Internal(format!("sign: {e}")))?;

    let txids = client
        .send_transactions(vec![tx])
        .map_err(|e| SettleError::Internal(format!("send: {e}")))?;
    let processed = client
        .wait_for_processed_transactions(txids)
        .map_err(|e| SettleError::Internal(format!("confirm: {e}")))?;

    match &processed[0].status {
        Status::Processed => {}
        other => return Err(SettleError::Internal(format!("settlement failed: {other:?}"))),
    }

    // Read back the authoritative on-chain status rather than trusting our own flip.
    let after = client
        .read_account_info(session_pda)
        .map_err(|e| SettleError::Internal(format!("read back: {e}")))?;
    let final_session = GameSession::try_from_slice(&after.data[..GameSession::LEN])
        .map_err(|e| SettleError::Internal(format!("decode read back: {e}")))?;

    Ok(SettleResponse {
        player_won: final_session.status == STATUS_WON,
        status: if final_session.status == STATUS_WON { "SettledWon" } else { "SettledLost" }
            .to_string(),
        session_id,
    })
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(env_or("RUST_LOG", "info"))
        .init();

    let config = load_config();

    let (authority_kp, authority_pk) = match load_authority() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    };

    let program_id_hex = match std::env::var("PROGRAM_ID") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("FATAL: set PROGRAM_ID (64-char hex)");
            std::process::exit(1);
        }
    };
    let program_id = match parse_pubkey(&program_id_hex) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FATAL: PROGRAM_ID invalid: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(rpc = %config.arch_node_url, "network");
    tracing::info!(program = %program_id, "program");
    tracing::info!(authority = %authority_pk, "house authority");

    let state = Arc::new(AppState {
        config,
        program_id,
        authority_kp,
        authority_pk,
        settled: Mutex::new(HashMap::new()),
    });

    // CORS is permissive so a local frontend can call this during development.
    // Restrict `allow_origin` to your real domain before exposing this publicly.
    let app = Router::new()
        .route("/health", get(health))
        .route("/settle", post(settle))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state);

    let port: u16 = env_or("PORT", "8080").parse().unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}
