//! Integration tests against a LIVE localnet (`arch-cli orchestrate start`).
//!
//! Arch has no in-process test harness (no program-test / litesvm equivalent), so these
//! talk to a real validator + Bitcoin regtest node. That is the documented approach and
//! matches how arch-examples tests itself.
//!
//! Run with:  cargo test -- --nocapture --ignored --test-threads=1
//!
//! The FIRST test is deliberately the riskiest thing in the design: moving native lamports
//! out of a PDA via `invoke_signed(system_instruction::transfer)`. No Arch example does this
//! (they all use APL token transfers), so it is verified here before anything is built on it.

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
use serial_test::serial;

const ELF_PATH: &str = "./target/deploy/coinflip_escrow.so";
const PROGRAM_FILE_PATH: &str = ".program.json";
/// The house authority is persisted to disk so every test in the run shares ONE identity.
/// Config is a singleton PDA per program, so a fresh authority per test would fail
/// `BadAuthority` against the config written by whichever test ran first.
const AUTHORITY_FILE_PATH: &str = ".authority.json";

/// Deploy the program (idempotent) and return a funded, stable house authority.
fn setup() -> (ArchRpcClient, Config, Pubkey, UntweakedKeypair, Pubkey) {
    let config = Config::localnet();
    let client = ArchRpcClient::new(&config);

    let (authority_keypair, authority_pubkey) =
        with_secret_key_file(AUTHORITY_FILE_PATH).expect("load/create authority keypair");
    // Already-funded on a second run is fine — ignore the error rather than failing setup.
    let _ = client.create_and_fund_account_with_faucet(&authority_keypair);

    let (program_keypair, _) =
        with_secret_key_file(PROGRAM_FILE_PATH).expect("load/create program keypair");

    let program_pubkey = ProgramDeployer::new(&config)
        .try_deploy_program(
            "coinflip_escrow".to_string(),
            program_keypair,
            authority_keypair,
            &ELF_PATH.to_string(),
        )
        .expect("deploy program");

    (client, config, program_pubkey, authority_keypair, authority_pubkey)
}

/// Initialize the singleton config PDA, unless a previous test already did.
fn ensure_config(
    client: &ArchRpcClient,
    config: &Config,
    program_id: Pubkey,
    authority_kp: UntweakedKeypair,
    authority_pk: Pubkey,
) -> Pubkey {
    let (config_pda, _) = Pubkey::find_program_address(&[b"config"], &program_id);

    // If it already holds data, it's initialized — reuse it.
    if let Ok(existing) = client.read_account_info(config_pda) {
        if !existing.data.is_empty() {
            return config_pda;
        }
    }

    let init = EscrowInstruction::InitializeConfig { min_wager: 1_000, max_wager: 1_000_000 };
    send_ok(
        client,
        config,
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(authority_pk, true),
                AccountMeta::new(config_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data: borsh::to_vec(&init).unwrap(),
        },
        authority_pk,
        vec![authority_kp],
    );
    config_pda
}

/// Helper: submit one instruction signed by the given keypairs, assert it succeeded.
fn send_ok(
    client: &ArchRpcClient,
    config: &Config,
    ix: Instruction,
    fee_payer: Pubkey,
    signers: Vec<UntweakedKeypair>,
) {
    let tx = build_and_sign_transaction(
        ArchMessage::new(&[ix], Some(fee_payer), client.get_best_finalized_block_hash().unwrap()),
        signers,
        config.network,
    )
    .expect("build and sign");

    let txids = client.send_transactions(vec![tx]).expect("send");
    let processed = client.wait_for_processed_transactions(txids).expect("wait");
    assert!(
        matches!(processed[0].status, Status::Processed),
        "tx did not process cleanly: {:?}",
        processed[0].status
    );
}

/// THE RISK TEST. Open a session (lamports IN via plain transfer), then settle it as a
/// LOSS, which pays out of the vault PDA via invoke_signed. If native lamport movement
/// from a PDA does not work on Arch, this is where it fails.
#[ignore]
#[serial]
#[test]
fn test_escrow_lamport_round_trip() {
    let (client, config, program_id, authority_kp, authority_pk) = setup();

    // --- InitializeConfig ---
    let config_pda = ensure_config(&client, &config, program_id, authority_kp, authority_pk);

    let cfg_account = client.read_account_info(config_pda).expect("read config");
    let cfg = EscrowConfig::try_from_slice(&cfg_account.data[..EscrowConfig::LEN]).unwrap();
    assert_eq!(cfg.authority, authority_pk);
    assert_eq!(cfg.min_wager, 1_000);
    println!("config initialized: {:?}", cfg);

    // --- OpenSession (player stakes) ---
    let (player_kp, player_pk, _) = generate_new_keypair(config.network);
    client.create_and_fund_account_with_faucet(&player_kp).expect("fund player");

    let session_id: u64 = 1;
    let wager: u64 = 10_000;
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) =
        Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    let open = EscrowInstruction::OpenSession { session_id, wager };
    send_ok(
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
            data: borsh::to_vec(&open).unwrap(),
        },
        player_pk,
        vec![player_kp.clone()],
    );

    let vault_after_open = client.read_account_info(vault_pda).expect("read vault");
    println!("vault lamports after open: {}", vault_after_open.lamports);
    assert!(
        vault_after_open.lamports >= wager,
        "stake did not land in vault: {} < {}",
        vault_after_open.lamports,
        wager
    );

    // --- SettleSession as a LOSS: vault -> house, paid via invoke_signed ---
    let settle = EscrowInstruction::SettleSession { player_won: false };
    send_ok(
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
                AccountMeta::new(authority_pk, true), // house treasury == authority here
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data: borsh::to_vec(&settle).unwrap(),
        },
        authority_pk,
        vec![authority_kp.clone()],
    );

    let session_account = client.read_account_info(session_pda).expect("read session");
    let session = GameSession::try_from_slice(&session_account.data[..GameSession::LEN]).unwrap();
    assert_eq!(session.status, STATUS_LOST, "session should be settled as LOST");

    let vault_after_settle = client.read_account_info(vault_pda).expect("read vault");
    println!("vault lamports after settle: {}", vault_after_settle.lamports);
    assert!(
        vault_after_settle.lamports < vault_after_open.lamports,
        "PDA-signed lamport transfer OUT did not happen"
    );

    println!("PDA lamport escrow round-trip WORKS on Arch");
}

/// A winning settlement must pay the player 2x (stake back + house match).
#[ignore]
#[serial]
#[test]
fn test_player_wins_gets_paid() {
    let (client, config, program_id, authority_kp, authority_pk) = setup();
    let config_pda = ensure_config(&client, &config, program_id, authority_kp, authority_pk);

    let (player_kp, player_pk, _) = generate_new_keypair(config.network);
    client.create_and_fund_account_with_faucet(&player_kp).expect("fund player");

    let session_id: u64 = 42;
    let wager: u64 = 10_000;
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) =
        Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    send_ok(
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
        vec![player_kp.clone()],
    );

    let before = client.read_account_info(player_pk).expect("read player").lamports;

    send_ok(
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
            data: borsh::to_vec(&EscrowInstruction::SettleSession { player_won: true }).unwrap(),
        },
        authority_pk,
        vec![authority_kp.clone()],
    );

    let after = client.read_account_info(player_pk).expect("read player").lamports;
    let session_account = client.read_account_info(session_pda).expect("read session");
    let session = GameSession::try_from_slice(&session_account.data[..GameSession::LEN]).unwrap();

    assert_eq!(session.status, STATUS_WON);
    assert!(after > before, "winner was not paid: {} -> {}", before, after);
    println!("winner paid: {} -> {} (+{})", before, after, after - before);
}

// ---------------------------------------------------------------------------
// NEGATIVE TESTS — verify the security guards actually reject attacks.
// A happy-path-only suite proves nothing about safety.
// ---------------------------------------------------------------------------

/// Helper: submit and return Err on failure instead of panicking.
fn send_expect_failure(
    client: &ArchRpcClient,
    config: &Config,
    ix: Instruction,
    fee_payer: Pubkey,
    signers: Vec<UntweakedKeypair>,
) -> Result<(), String> {
    let tx = build_and_sign_transaction(
        ArchMessage::new(&[ix], Some(fee_payer), client.get_best_finalized_block_hash().unwrap()),
        signers,
        config.network,
    )
    .map_err(|e| e.to_string())?;
    let txids = client.send_transactions(vec![tx]).map_err(|e| e.to_string())?;
    let processed = client.wait_for_processed_transactions(txids).map_err(|e| e.to_string())?;
    match &processed[0].status {
        Status::Processed => Ok(()),
        other => Err(format!("{:?}", other)),
    }
}

/// Open a session and settle it, then try to settle AGAIN.
/// The second settle must fail — otherwise a winner could drain the vault repeatedly.
#[ignore]
#[serial]
#[test]
fn test_double_settle_is_rejected() {
    let (client, config, program_id, authority_kp, authority_pk) = setup();
    let config_pda = ensure_config(&client, &config, program_id, authority_kp, authority_pk);

    let (player_kp, player_pk, _) = generate_new_keypair(config.network);
    client.create_and_fund_account_with_faucet(&player_kp).expect("fund player");

    let session_id: u64 = 777_001;
    let wager: u64 = 10_000;
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    send_ok(&client, &config, Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(player_pk, true),
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(session_pda, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: borsh::to_vec(&EscrowInstruction::OpenSession { session_id, wager }).unwrap(),
    }, player_pk, vec![player_kp]);

    let settle_ix = || Instruction {
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
        data: borsh::to_vec(&EscrowInstruction::SettleSession { player_won: true }).unwrap(),
    };

    // First settle succeeds.
    send_ok(&client, &config, settle_ix(), authority_pk, vec![authority_kp]);

    // Second settle MUST fail (custom error 4 = SessionNotOpen).
    let balance_before_replay = client.read_account_info(player_pk).unwrap().lamports;
    let result = send_expect_failure(&client, &config, settle_ix(), authority_pk, vec![authority_kp]);
    let balance_after_replay = client.read_account_info(player_pk).unwrap().lamports;

    assert!(result.is_err(), "DOUBLE SETTLE SUCCEEDED — vault can be drained!");
    assert_eq!(
        balance_before_replay, balance_after_replay,
        "player balance changed on a rejected replay"
    );
    println!("double-settle correctly rejected: {}", result.unwrap_err());
}

/// A random key that is NOT the configured authority must not be able to settle.
#[ignore]
#[serial]
#[test]
fn test_unauthorized_authority_is_rejected() {
    let (client, config, program_id, authority_kp, authority_pk) = setup();
    let config_pda = ensure_config(&client, &config, program_id, authority_kp, authority_pk);

    let (player_kp, player_pk, _) = generate_new_keypair(config.network);
    client.create_and_fund_account_with_faucet(&player_kp).expect("fund player");

    // The attacker: a funded key with no relationship to the house.
    let (attacker_kp, attacker_pk, _) = generate_new_keypair(config.network);
    client.create_and_fund_account_with_faucet(&attacker_kp).expect("fund attacker");

    let session_id: u64 = 777_002;
    let wager: u64 = 10_000;
    let (session_pda, _) = Pubkey::find_program_address(
        &[b"session", player_pk.as_ref(), &session_id.to_le_bytes()],
        &program_id,
    );
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault", session_pda.as_ref()], &program_id);

    send_ok(&client, &config, Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(player_pk, true),
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(session_pda, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: borsh::to_vec(&EscrowInstruction::OpenSession { session_id, wager }).unwrap(),
    }, player_pk, vec![player_kp]);

    let vault_before = client.read_account_info(vault_pda).unwrap().lamports;

    // Attacker signs the settlement themselves, claiming the player won.
    let result = send_expect_failure(&client, &config, Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(attacker_pk, true),
            AccountMeta::new_readonly(config_pda, false),
            AccountMeta::new(session_pda, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new(player_pk, false),
            AccountMeta::new(attacker_pk, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: borsh::to_vec(&EscrowInstruction::SettleSession { player_won: true }).unwrap(),
    }, attacker_pk, vec![attacker_kp]);

    let vault_after = client.read_account_info(vault_pda).unwrap().lamports;

    assert!(result.is_err(), "UNAUTHORIZED SETTLE SUCCEEDED — anyone can drain vaults!");
    assert_eq!(vault_before, vault_after, "vault balance moved on a rejected settle");
    println!("unauthorized settle correctly rejected: {}", result.unwrap_err());
}
