//! Opens a session as a PLAYER would from their wallet, then stops.
//!
//! Settlement is deliberately NOT done here — that is the backend's job, because it
//! requires the house authority key. This is the client half of the split.
//!
//!   cargo run --features no-entrypoint --example open_session -- testnet
//!
//! Prints the player pubkey and session id, which you POST to the backend /settle.

use arch_program::account::AccountMeta;
use arch_program::instruction::Instruction;
use arch_program::pubkey::Pubkey;
use arch_program::sanitized::ArchMessage;
use arch_program::system_program::SYSTEM_PROGRAM_ID;
use arch_sdk::blocking::ArchRpcClient;
use arch_sdk::{build_and_sign_transaction, generate_new_keypair, Config, Status};
use coinflip_escrow::EscrowInstruction;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn testnet_config() -> Config {
    let mut c = Config::testnet();
    c.arch_node_url = env_or("ARCH_RPC_URL", "https://rpc.testnet.arch.network");
    c.titan_url = env_or("ARCH_TITAN_URL", "https://titan.testnet.arch.network");
    c.node_endpoint = env_or("BITCOIN_RPC_URL", "http://bitcoin-rpc.test.arch.network:80");
    c.node_username = env_or("BITCOIN_RPC_USER", "bitcoin");
    c.node_password = env_or("BITCOIN_RPC_PASSWORD", "0F_Ed53o4kR7nxh3xNaSQx-2M3TY16L55mz5y9fjdrk");
    c
}

fn main() {
    let net = std::env::args().nth(1).unwrap_or_else(|| "testnet".into());
    let config = if net == "localnet" { Config::localnet() } else { testnet_config() };

    let program_id_hex = env_or(
        "PROGRAM_ID",
        "e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4",
    );
    let program_id = Pubkey::from_slice(&hex::decode(&program_id_hex).expect("PROGRAM_ID hex"));

    let client = ArchRpcClient::new(&config);

    // A fresh player, funded by the faucet — stands in for a browser wallet.
    let (player_kp, player_pk, _) = generate_new_keypair(config.network);
    client
        .create_and_fund_account_with_faucet(&player_kp)
        .expect("fund player");

    let session_id: u64 = std::process::id() as u64 * 7919;
    let wager: u64 = 10_000;

    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &program_id);
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(player_pk, true),
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(session_pda, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: borsh::to_vec(&EscrowInstruction::OpenSession { session_id, wager }).unwrap(),
    };

    let tx = build_and_sign_transaction(
        ArchMessage::new(
            &[ix],
            Some(player_pk),
            client.get_best_finalized_block_hash().unwrap(),
        ),
        vec![player_kp],
        config.network,
    )
    .expect("sign");

    let txids = client.send_transactions(vec![tx]).expect("send");
    let processed = client.wait_for_processed_transactions(txids).expect("confirm");
    assert!(matches!(processed[0].status, Status::Processed), "open failed: {:?}", processed[0].status);

    let vault = client.read_account_info(vault_pda).unwrap().lamports;
    let balance = client.read_account_info(player_pk).unwrap().lamports;

    println!("session opened");
    println!("  player      : {}", player_pk);
    println!("  session_id  : {}", session_id);
    println!("  wager       : {}", wager);
    println!("  escrowed    : {} lamports", vault);
    println!("  balance     : {} lamports", balance);
    println!();
    println!("settle it with:");
    println!(
        "  curl -s -X POST http://localhost:8091/settle -H 'Content-Type: application/json' \\\n    -d '{{\"player\":\"{}\",\"session_id\":{}}}'",
        player_pk, session_id
    );
}
