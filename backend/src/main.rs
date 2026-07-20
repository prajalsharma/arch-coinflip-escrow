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
    http::{header, HeaderValue, Method, StatusCode},
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
    /// Resolved CORS origins, surfaced on /health so a misconfigured deploy is
    /// diagnosable from outside without shell access to the container.
    cors_origins: Vec<String>,
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

#[derive(Deserialize)]
struct DemoOpenRequest {
    /// Optional; defaults to the config's min wager range midpoint.
    wager: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
struct DemoOpenResponse {
    player: String,
    session_id: u64,
    wager: u64,
    escrowed: u64,
    balance: u64,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    network: String,
    program_id: String,
    house_authority: String,
    block_height: Option<u64>,
    cors: CorsInfo,
}

#[derive(Serialize)]
struct CorsInfo {
    /// false == ALLOWED_ORIGIN was not seen by the process at all.
    restricted: bool,
    allowed_origins: Vec<String>,
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
        cors: CorsInfo {
            restricted: !state.cors_origins.is_empty(),
            allowed_origins: state.cors_origins.clone(),
        },
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

/// DEMO MODE. Creates a throwaway player, funds it from the faucet, and opens a session
/// on its behalf — so the game is playable with no wallet extension installed.
///
/// This is a TESTNET CONVENIENCE, not how a real player should work. The server holds the
/// throwaway key for the length of one request; a real player signs with their own wallet
/// and the server never sees their key. Disable with DEMO_MODE=off.
async fn demo_open(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DemoOpenRequest>,
) -> Result<Json<DemoOpenResponse>, (StatusCode, Json<ErrorResponse>)> {
    let bad = |c: StatusCode, m: String| (c, Json(ErrorResponse { error: m }));

    if env_or("DEMO_MODE", "on") != "on" {
        return Err(bad(StatusCode::FORBIDDEN, "demo mode disabled".into()));
    }

    let wager = req.wager.unwrap_or(10_000);
    let state2 = state.clone();

    let result = tokio::task::spawn_blocking(move || demo_open_blocking(state2, wager))
        .await
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, format!("task panic: {e}")))?;

    match result {
        Ok(r) => Ok(Json(r)),
        Err(SettleError::BadRequest(m)) => Err(bad(StatusCode::BAD_REQUEST, m)),
        Err(SettleError::Conflict(m)) => Err(bad(StatusCode::CONFLICT, m)),
        Err(SettleError::Internal(m)) => Err(bad(StatusCode::INTERNAL_SERVER_ERROR, m)),
    }
}

fn demo_open_blocking(
    state: Arc<AppState>,
    wager: u64,
) -> Result<DemoOpenResponse, SettleError> {
    use arch_sdk::generate_new_keypair;

    let client = ArchRpcClient::new(&state.config);
    let program_id = state.program_id;

    let (player_kp, player_pk, _) = generate_new_keypair(state.config.network);
    client
        .create_and_fund_account_with_faucet(&player_kp)
        .map_err(|e| SettleError::Internal(format!("faucet: {e}")))?;

    // Session id derived from the player key so it is unique without extra state.
    let session_id = u64::from_le_bytes(player_pk.as_ref()[..8].try_into().unwrap()) & 0x0000_FFFF_FFFF_FFFF;

    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &program_id);
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) =
        Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(player_pk, true),
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(session_pda, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(state.authority_pk, false), // house treasury
        ],
        data: borsh::to_vec(&EscrowInstruction::OpenSession { session_id, wager })
            .map_err(|e| SettleError::Internal(format!("serialize: {e}")))?,
    };

    let blockhash = client
        .get_best_finalized_block_hash()
        .map_err(|e| SettleError::Internal(format!("blockhash: {e}")))?;

    let tx = build_and_sign_transaction(
        ArchMessage::new(&[ix], Some(player_pk), blockhash),
        vec![player_kp],
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
        other => return Err(SettleError::Internal(format!("open failed: {other:?}"))),
    }

    let escrowed = client.read_account_info(vault_pda).map(|a| a.lamports).unwrap_or(0);
    let balance = client.read_account_info(player_pk).map(|a| a.lamports).unwrap_or(0);

    Ok(DemoOpenResponse {
        player: player_pk.to_string(),
        session_id,
        wager,
        escrowed,
        balance,
    })
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
    let session_account = client.read_account_info(session_pda).map_err(|_| {
        // Name the program and the derived PDA. The usual cause is that the frontend and
        // this service are configured with different PROGRAM_IDs, so the session was
        // opened under a different program and this derivation points at nothing.
        SettleError::BadRequest(format!(
            "session not found on-chain. Derived session PDA {} for player {} \
             (session_id {}) under program {}. If the frontend uses a different \
             PROGRAM_ID, sessions it opens are invisible here.",
            session_pda, player_pk, session_id, program_id
        ))
    })?;

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


    // CORS. Defaults to permissive for local development, but set ALLOWED_ORIGIN in
    // production (e.g. https://your-app.vercel.app) so only your frontend can call
    // the settlement endpoint. Comma-separate for multiple origins.
    let mut resolved_origins: Vec<String> = Vec::new();
    let cors = match std::env::var("ALLOWED_ORIGIN") {
        Ok(raw) if !raw.trim().is_empty() => {
            let mut list: Vec<HeaderValue> = Vec::new();

            for entry in raw.split(',') {
                let mut o = entry.trim().to_string();
                if o.is_empty() {
                    continue;
                }

                // A browser Origin header is scheme://host[:port] — no trailing slash and
                // no path. Pasting the URL straight from the address bar usually includes
                // a trailing slash, which would silently match nothing. Fix it and say so
                // rather than failing closed in a way that looks like the server is down.
                if o.ends_with('/') {
                    let fixed = o.trim_end_matches('/').to_string();
                    tracing::warn!(given = %o, using = %fixed, "ALLOWED_ORIGIN had a trailing slash — stripped it");
                    o = fixed;
                }
                if !o.starts_with("http://") && !o.starts_with("https://") {
                    let fixed = format!("https://{o}");
                    tracing::warn!(given = %o, using = %fixed, "ALLOWED_ORIGIN missing scheme — assuming https");
                    o = fixed;
                }
                if let Some(idx) = o[8..].find('/').map(|i| i + 8) {
                    let fixed = o[..idx].to_string();
                    tracing::warn!(given = %o, using = %fixed, "ALLOWED_ORIGIN contained a path — trimmed to origin");
                    o = fixed;
                }

                match o.parse::<HeaderValue>() {
                    Ok(v) => {
                        tracing::info!(origin = %o, "CORS allowing origin");
                        resolved_origins.push(o.clone());
                        list.push(v);
                    }
                    Err(_) => {
                        tracing::error!(origin = %o, "ALLOWED_ORIGIN entry is not a valid header value — IGNORED");
                    }
                }
            }

            if list.is_empty() {
                // Failing closed here would look exactly like the backend being down.
                tracing::error!(
                    "ALLOWED_ORIGIN was set but no valid origin parsed — falling back to OPEN CORS \
                     so the app still works. Fix the value; expected form: https://your-app.vercel.app"
                );
                resolved_origins.clear();
                CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)
            } else {
                CorsLayer::new()
                    .allow_origin(list)
                    .allow_methods([Method::GET, Method::POST])
                    .allow_headers([header::CONTENT_TYPE])
            }
        }
        _ => {
            tracing::warn!("ALLOWED_ORIGIN not set — CORS is open to any origin (dev only)");
            CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)
        }
    };

    // NOTE: built after the CORS block so the resolved origin list can be stored.
    let state = Arc::new(AppState {
        config,
        program_id,
        authority_kp,
        authority_pk,
        settled: Mutex::new(HashMap::new()),
        cors_origins: resolved_origins.clone(),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/settle", post(settle))
        .route("/demo/open", post(demo_open))
        .layer(cors)
        .with_state(state);

    let port: u16 = env_or("PORT", "8080").parse().unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}
