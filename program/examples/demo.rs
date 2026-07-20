//! End-to-end coin flip demo — runs the whole user flow against localnet or testnet.
//!
//!   cargo run --features no-entrypoint --example demo -- localnet
//!   cargo run --features no-entrypoint --example demo -- testnet
//!
//! Flow: fund player -> open session (stake escrowed) -> flip OFF-CHAIN -> settle on-chain.
//!
//! The coin flip itself happens in this client, not in the program. That is the whole point
//! of the design: Arch exposes no randomness primitive, so the result is produced off-chain
//! and attested on-chain by the house authority's signature.

use arch_program::account::AccountMeta;
use arch_program::instruction::Instruction;
use arch_program::pubkey::Pubkey;
use arch_program::sanitized::ArchMessage;
use arch_program::system_program::SYSTEM_PROGRAM_ID;
use arch_sdk::blocking::{ArchRpcClient, ProgramDeployer};
use arch_sdk::{
    build_and_sign_transaction, generate_new_keypair, with_secret_key_file, Config, Status,
};
use bitcoin::key::UntweakedKeypair;
use borsh::BorshDeserialize;
use coinflip_escrow::{
    Config as EscrowConfig, EscrowInstruction, GameSession, STATUS_LOST, STATUS_WON,
};

const ELF_PATH: &str = "./target/deploy/coinflip_escrow.so";

/// Testnet preset ships with EMPTY endpoints in arch_sdk 0.6.7 — fill them in.
///
/// The Bitcoin RPC credentials below are Arch's own SHARED PUBLIC testnet credentials,
/// published verbatim in their quick-start docs. They are not a private secret. They are
/// still env-overridable so nothing sensitive ever has to be edited into source.
fn testnet_config() -> Config {
    fn env_or(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.to_string())
    }

    let mut c = Config::testnet();
    c.arch_node_url = env_or("ARCH_RPC_URL", "https://rpc.testnet.arch.network");
    c.titan_url = env_or("ARCH_TITAN_URL", "https://titan.testnet.arch.network");
    c.node_endpoint = env_or("BITCOIN_RPC_URL", "http://bitcoin-rpc.test.arch.network:80");
    c.node_username = env_or("BITCOIN_RPC_USER", "bitcoin");
    c.node_password = env_or("BITCOIN_RPC_PASSWORD", "0F_Ed53o4kR7nxh3xNaSQx-2M3TY16L55mz5y9fjdrk");
    c
}

fn send(
    client: &ArchRpcClient,
    config: &Config,
    ix: Instruction,
    payer: Pubkey,
    signers: Vec<UntweakedKeypair>,
) -> Result<(), String> {
    let tx = build_and_sign_transaction(
        ArchMessage::new(
            &[ix],
            Some(payer),
            client.get_best_finalized_block_hash().map_err(|e| e.to_string())?,
        ),
        signers,
        config.network,
    )
    .map_err(|e| e.to_string())?;

    let txids = client.send_transactions(vec![tx]).map_err(|e| e.to_string())?;
    let processed = client
        .wait_for_processed_transactions(txids)
        .map_err(|e| e.to_string())?;

    match &processed[0].status {
        Status::Processed => Ok(()),
        other => Err(format!("{:?}", other)),
    }
}

/// Deterministic pseudo-flip for the demo. NOT SECURE — a real product needs a
/// two-party commit-reveal or an external VRF. Labeled loudly on purpose.
fn off_chain_flip(seed: u64) -> bool {
    let h = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (h >> 33) & 1 == 0
}

fn main() {
    let net = std::env::args().nth(1).unwrap_or_else(|| "localnet".into());
    let (config, authority_file, program_file) = match net.as_str() {
        "testnet" => (testnet_config(), ".testnet-authority.json", ".testnet-program.json"),
        _ => (Config::localnet(), ".authority.json", ".program.json"),
    };

    println!("network        : {} ({:?})", net, config.network);
    println!("arch rpc       : {}", config.arch_node_url);

    let client = ArchRpcClient::new(&config);

    // --- house authority ---
    let (authority_kp, authority_pk) =
        with_secret_key_file(authority_file).expect("load/create authority keypair");
    let _ = client.create_and_fund_account_with_faucet(&authority_kp);
    println!("house authority: {}", authority_pk);

    // --- program ---
    let (program_kp, _) = with_secret_key_file(program_file).expect("load/create program keypair");
    let program_id = ProgramDeployer::new(&config)
        .try_deploy_program(
            "coinflip_escrow".to_string(),
            program_kp,
            authority_kp,
            &ELF_PATH.to_string(),
        )
        .expect("deploy program");
    println!("program id     : {}", program_id);

    // --- config PDA (idempotent) ---
    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &program_id);
    let needs_init = match client.read_account_info(config_pda) {
        Ok(a) => a.data.is_empty(),
        Err(_) => true,
    };
    if needs_init {
        send(
            &client,
            &config,
            Instruction {
                program_id,
                accounts: vec![
                    AccountMeta::new(authority_pk, true),
                    AccountMeta::new(config_pda, false),
                    AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
                ],
                data: borsh::to_vec(&EscrowInstruction::InitializeConfig {
                    min_wager: 1_000,
                    max_wager: 1_000_000,
                })
                .unwrap(),
            },
            authority_pk,
            vec![authority_kp],
        )
        .expect("initialize config");
        println!("config         : initialized");
    } else {
        println!("config         : already initialized");
    }

    let cfg_acct = client.read_account_info(config_pda).expect("read config");
    let cfg = EscrowConfig::try_from_slice(&cfg_acct.data[..EscrowConfig::LEN]).unwrap();
    println!("wager bounds   : {} .. {}", cfg.min_wager, cfg.max_wager);

    // --- player ---
    let (player_kp, player_pk, _) = generate_new_keypair(config.network);
    client
        .create_and_fund_account_with_faucet(&player_kp)
        .expect("fund player");
    let start_balance = client.read_account_info(player_pk).unwrap().lamports;
    println!("\nplayer         : {}", player_pk);
    println!("start balance  : {} lamports", start_balance);

    // --- open session (stake goes into escrow) ---
    let session_id: u64 = std::process::id() as u64;
    let wager: u64 = 10_000;
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    send(
        &client,
        &config,
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(player_pk, true),
                AccountMeta::new_readonly(config_pda, false),
                AccountMeta::new(session_pda, false),
                AccountMeta::new(vault_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data: borsh::to_vec(&EscrowInstruction::OpenSession { session_id, wager }).unwrap(),
        },
        player_pk,
        vec![player_kp],
    )
    .expect("open session");

    let vault_balance = client.read_account_info(vault_pda).unwrap().lamports;
    println!("\nsession opened : id={} wager={}", session_id, wager);
    println!("escrow vault   : {} lamports held", vault_balance);

    // --- the coin flip happens OFF-CHAIN ---
    let player_won = off_chain_flip(session_id);
    println!("\ncoin flip      : {} (decided off-chain)", if player_won { "WIN" } else { "LOSE" });

    // --- settle on-chain ---
    send(
        &client,
        &config,
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(authority_pk, true),
                AccountMeta::new_readonly(config_pda, false),
                AccountMeta::new(session_pda, false),
                AccountMeta::new(vault_pda, false),
                AccountMeta::new(player_pk, false),
                AccountMeta::new(authority_pk, true),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data: borsh::to_vec(&EscrowInstruction::SettleSession { player_won }).unwrap(),
        },
        authority_pk,
        vec![authority_kp],
    )
    .expect("settle session");

    let session_acct = client.read_account_info(session_pda).unwrap();
    let session = GameSession::try_from_slice(&session_acct.data[..GameSession::LEN]).unwrap();
    let end_balance = client.read_account_info(player_pk).unwrap().lamports;

    let status = match session.status {
        STATUS_WON => "SettledWon",
        STATUS_LOST => "SettledLost",
        _ => "Open (unexpected)",
    };

    println!("\nsettled        : {}", status);
    println!("end balance    : {} lamports", end_balance);
    if end_balance >= start_balance {
        println!("net            : +{}", end_balance - start_balance);
    } else {
        println!("net            : -{}", start_balance - end_balance);
    }
    println!("\ndone.");
}
