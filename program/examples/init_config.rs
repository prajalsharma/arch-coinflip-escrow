//! One-time config initialization for a freshly deployed program.
//!
//!   cargo run --features no-entrypoint --example init_config -- testnet <PROGRAM_ID_HEX>
//!
//! The program pins EXPECTED_AUTHORITY at build time, so only that key can do this.
//! Anyone else is rejected with BadAuthority (0x2), which is the fix for the
//! front-running vulnerability documented in docs/SECURITY.md.

use arch_program::account::AccountMeta;
use arch_program::instruction::Instruction;
use arch_program::pubkey::Pubkey;
use arch_program::sanitized::ArchMessage;
use arch_program::system_program::SYSTEM_PROGRAM_ID;
use arch_sdk::blocking::ArchRpcClient;
use arch_sdk::{build_and_sign_transaction, with_secret_key_file, Config, Status};
use borsh::BorshDeserialize;
use coinflip_escrow::{Config as EscrowConfig, EscrowInstruction};

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
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
    let program_hex = std::env::args()
        .nth(2)
        .expect("usage: init_config <network> <PROGRAM_ID_HEX>");

    let config = if net == "localnet" { Config::localnet() } else { testnet_config() };
    let client = ArchRpcClient::new(&config);

    let key_file = if net == "localnet" { ".authority.json" } else { ".testnet-authority.json" };
    let (authority_kp, authority_pk) = with_secret_key_file(key_file).expect("load authority");
    let _ = client.create_and_fund_account_with_faucet(&authority_kp);

    let program_id = Pubkey::from_slice(&hex::decode(&program_hex).expect("program id hex"));
    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &program_id);

    println!("program   : {}", program_id);
    println!("authority : {}", authority_pk);
    println!("config pda: {}", config_pda);

    if let Ok(existing) = client.read_account_info(config_pda) {
        if !existing.data.is_empty() {
            let cfg = EscrowConfig::try_from_slice(&existing.data[..EscrowConfig::LEN])
                .expect("decode existing config");
            println!("\nalready initialized:");
            println!("  authority : {}", cfg.authority);
            println!("  treasury  : {}", cfg.house_treasury);
            println!("  wager     : {} .. {}", cfg.min_wager, cfg.max_wager);
            return;
        }
    }

    let ix = Instruction {
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
    };

    let tx = build_and_sign_transaction(
        ArchMessage::new(&[ix], Some(authority_pk), client.get_best_finalized_block_hash().unwrap()),
        vec![authority_kp],
        config.network,
    )
    .expect("sign");

    let txids = client.send_transactions(vec![tx]).expect("send");
    let processed = client.wait_for_processed_transactions(txids).expect("confirm");
    assert!(
        matches!(processed[0].status, Status::Processed),
        "init failed: {:?}",
        processed[0].status
    );

    let acct = client.read_account_info(config_pda).expect("read back");
    let cfg = EscrowConfig::try_from_slice(&acct.data[..EscrowConfig::LEN]).expect("decode");
    println!("\ninitialized:");
    println!("  authority : {}", cfg.authority);
    println!("  treasury  : {}", cfg.house_treasury);
    println!("  wager     : {} .. {}", cfg.min_wager, cfg.max_wager);
    println!("  data len  : {} bytes", acct.data.len());
}
