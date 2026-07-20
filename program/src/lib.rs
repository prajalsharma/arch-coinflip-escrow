//! Coin Flip Escrow — minimal Arch Network MVP.
//!
//! Design constraint: the ESCROW is on-chain, the GAME RESULT is decided OFF-CHAIN.
//! The program never generates randomness (Arch has no VRF / slot_hashes primitive).
//! It only holds the stake and pays out when a trusted house authority signs the outcome.
//!
//! Three instructions:
//!   0. InitializeConfig — one-time, records the house authority + wager bounds
//!   1. OpenSession      — player stakes; lamports move into a per-session vault PDA
//!   2. SettleSession    — house authority reports the off-chain result; vault pays out
//!
//! TRUST MODEL (non-trustless, by design): the house authority is trusted to report
//! results honestly. Making this trustless needs two-party commit-reveal or a VRF oracle.

use arch_program::{
    account::{AccountInfo, next_account_info},
    program::{invoke, invoke_signed, get_clock},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::minimum_rent,
    system_instruction,
};
use borsh::{BorshDeserialize, BorshSerialize};

// The `entrypoint!` macro also installs a custom global allocator (BumpAllocator) and
// panic handler intended for the SBF VM. Linking those into a HOST binary (integration
// tests) segfaults the test process at startup, so the entrypoint is feature-gated out
// when building for the host. `cargo build-sbf` uses the default features and keeps it.
#[cfg(not(feature = "no-entrypoint"))]
arch_program::entrypoint!(process_instruction);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Custom program errors, surfaced as `ProgramError::Custom(u32)`.
#[repr(u32)]
pub enum EscrowError {
    AlreadyInitialized = 0,
    NotInitialized = 1,
    BadAuthority = 2,
    WagerOutOfBounds = 3,
    SessionNotOpen = 4,
    WrongPlayer = 5,
    BadPda = 6,
    InsufficientVault = 7,
    BadTreasury = 8,
    HouseUnderfunded = 9,
    TooEarlyToReclaim = 10,
}

impl From<EscrowError> for ProgramError {
    fn from(e: EscrowError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Session lifecycle. Settled states are TERMINAL, which is the double-claim guard.
pub const STATUS_OPEN: u8 = 0;
pub const STATUS_WON: u8 = 1;
pub const STATUS_LOST: u8 = 2;
pub const STATUS_RECLAIMED: u8 = 3;

/// How long the house has to settle before the player can take their stake back.
/// Without this the house can lock a player's funds forever by doing nothing.
pub const RECLAIM_TIMEOUT_SECS: i64 = 3600;

/// The only key allowed to run InitializeConfig.
///
/// Pinned at build time on purpose. If any signer could initialize, whoever called first
/// on a fresh deployment would own the config permanently and could drain every vault.
/// Override at build time: ARCH_COINFLIP_AUTHORITY=<64-char hex> cargo build-sbf
pub const EXPECTED_AUTHORITY: [u8; 32] = match option_env!("ARCH_COINFLIP_AUTHORITY") {
    Some(_) => parse_authority_hex(),
    // Default is the testnet house key this repo deploys with.
    None => [
        0xab, 0x82, 0x15, 0x5f, 0xd2, 0xc6, 0x66, 0xef, 0x5c, 0x66, 0x26, 0x0b, 0xf0, 0xb9,
        0x72, 0x8f, 0xb4, 0x23, 0xa4, 0x02, 0x34, 0x51, 0x4e, 0xce, 0xb6, 0x52, 0x90, 0x42,
        0x2f, 0x00, 0x59, 0x23,
    ],
};

/// const-fn hex parse so the authority can be pinned without a build script.
const fn parse_authority_hex() -> [u8; 32] {
    let hex = match option_env!("ARCH_COINFLIP_AUTHORITY") {
        Some(h) => h.as_bytes(),
        None => panic!("unreachable"),
    };
    assert!(hex.len() == 64, "ARCH_COINFLIP_AUTHORITY must be 64 hex chars");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = hex_nibble(hex[i * 2]) * 16 + hex_nibble(hex[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("ARCH_COINFLIP_AUTHORITY contains a non-hex character"),
    }
}

/// Global house config. PDA seeds: ["config"]
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Config {
    /// The only key allowed to settle sessions.
    pub authority: Pubkey,
    /// The only account losing stakes may be paid into, and the account that funds wins.
    /// Stored so settlement cannot redirect funds to an attacker-chosen address.
    pub house_treasury: Pubkey,
    pub min_wager: u64,
    pub max_wager: u64,
    pub bump: u8,
}

impl Config {
    pub const LEN: usize = 32 + 32 + 8 + 8 + 1; // 81
}

/// One game session. PDA seeds: ["session", player, session_id_le]
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct GameSession {
    pub player: Pubkey,
    pub wager: u64,
    /// Client-supplied nonce. Makes the PDA unique, so a replayed OpenSession
    /// for the same id fails at account creation.
    pub session_id: u64,
    pub opened_at: i64,
    pub status: u8,
    pub bump: u8,
    pub vault_bump: u8,
}

impl GameSession {
    pub const LEN: usize = 32 + 8 + 8 + 8 + 1 + 1 + 1; // 59
}

// ---------------------------------------------------------------------------
// Instructions
// ---------------------------------------------------------------------------

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum EscrowInstruction {
    /// Accounts: [0] authority (signer, writable), [1] config PDA (writable), [2] system program
    InitializeConfig { min_wager: u64, max_wager: u64 },

    /// Accounts: [0] player (signer, writable), [1] config PDA, [2] session PDA (writable),
    ///           [3] vault PDA (writable), [4] system program
    OpenSession { session_id: u64, wager: u64 },

    /// Accounts: [0] authority (signer), [1] config PDA, [2] session PDA (writable),
    ///           [3] vault PDA (writable), [4] player (writable), [5] house treasury (writable),
    ///           [6] system program
    ///
    /// `player_won` is the OFF-CHAIN game result, attested by the authority's signature.
    SettleSession { player_won: bool },

    /// Accounts: [0] player (signer, writable), [1] session PDA (writable),
    ///           [2] vault PDA (writable), [3] system program
    ///
    /// Player-initiated escape hatch. After RECLAIM_TIMEOUT_SECS an unsettled session can be
    /// refunded by the player alone, so the house cannot lock funds by going silent.
    ReclaimSession,
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

pub fn process_instruction<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> Result<(), ProgramError> {
    let instruction = EscrowInstruction::try_from_slice(instruction_data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match instruction {
        EscrowInstruction::InitializeConfig { min_wager, max_wager } => {
            initialize_config(program_id, accounts, min_wager, max_wager)
        }
        EscrowInstruction::OpenSession { session_id, wager } => {
            open_session(program_id, accounts, session_id, wager)
        }
        EscrowInstruction::SettleSession { player_won } => {
            settle_session(program_id, accounts, player_won)
        }
        EscrowInstruction::ReclaimSession => reclaim_session(program_id, accounts),
    }
}

// ---------------------------------------------------------------------------
// 0. InitializeConfig
// ---------------------------------------------------------------------------

fn initialize_config<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    min_wager: u64,
    max_wager: u64,
) -> Result<(), ProgramError> {
    let account_iter = &mut accounts.iter();
    let authority = next_account_info(account_iter)?;
    let config_info = next_account_info(account_iter)?;
    let system_program = next_account_info(account_iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // C-1: without this, whoever calls first on a fresh deployment owns the config
    // forever and can drain every vault. The authority is pinned at build time.
    if authority.key.serialize() != EXPECTED_AUTHORITY {
        return Err(EscrowError::BadAuthority.into());
    }
    if min_wager == 0 || max_wager < min_wager {
        return Err(EscrowError::WagerOutOfBounds.into());
    }

    // Derive and verify the config PDA rather than trusting the caller's account.
    let (expected_config, bump) = Pubkey::find_program_address(&[b"config"], program_id);
    if config_info.key != &expected_config {
        return Err(EscrowError::BadPda.into());
    }
    // A config account that already holds data is already initialized.
    if !config_info.data_is_empty() {
        return Err(EscrowError::AlreadyInitialized.into());
    }

    let space = Config::LEN as u64;
    invoke_signed(
        &system_instruction::create_account(
            authority.key,
            config_info.key,
            minimum_rent(Config::LEN),
            space,
            program_id,
        ),
        &[authority.clone(), config_info.clone(), system_program.clone()],
        &[&[b"config", &[bump]]],
    )?;

    // The treasury is fixed at initialization so settlement cannot redirect funds later.
    let config = Config {
        authority: *authority.key,
        house_treasury: *authority.key,
        min_wager,
        max_wager,
        bump,
    };
    write_state(config_info, &config)
}

// ---------------------------------------------------------------------------
// 1. OpenSession
// ---------------------------------------------------------------------------

fn open_session<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    session_id: u64,
    wager: u64,
) -> Result<(), ProgramError> {
    let account_iter = &mut accounts.iter();
    let player = next_account_info(account_iter)?;
    let config_info = next_account_info(account_iter)?;
    let session_info = next_account_info(account_iter)?;
    let vault_info = next_account_info(account_iter)?;
    let system_program = next_account_info(account_iter)?;
    let house_treasury = next_account_info(account_iter)?;

    if !player.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let config = read_config(program_id, config_info)?;
    if wager < config.min_wager || wager > config.max_wager {
        return Err(EscrowError::WagerOutOfBounds.into());
    }

    // M-6: refuse a bet the house cannot pay out. Otherwise a winning settlement fails
    // and the player's stake sits in an open session until the reclaim timeout.
    if config.house_treasury != *house_treasury.key {
        return Err(EscrowError::BadTreasury.into());
    }
    if **house_treasury.try_borrow_lamports()? < wager {
        return Err(EscrowError::HouseUnderfunded.into());
    }

    // Session PDA — unique per (player, session_id), so a duplicate id can't be reused.
    let id_bytes = session_id.to_le_bytes();
    let session_seeds: &[&[u8]] = &[b"session", player.key.as_ref(), &id_bytes];
    let (expected_session, session_bump) =
        Pubkey::find_program_address(session_seeds, program_id);
    if session_info.key != &expected_session {
        return Err(EscrowError::BadPda.into());
    }

    // Vault PDA — system-owned, zero data, holds ONLY lamports. Keeping the stake
    // separate from program-owned state means payouts can use a plain system
    // transfer signed by the PDA, instead of mutating lamports directly.
    let vault_seeds: &[&[u8]] = &[b"vault", session_info.key.as_ref()];
    let (expected_vault, vault_bump) = Pubkey::find_program_address(vault_seeds, program_id);
    if vault_info.key != &expected_vault {
        return Err(EscrowError::BadPda.into());
    }

    // Create the session state account (program-owned).
    invoke_signed(
        &system_instruction::create_account(
            player.key,
            session_info.key,
            minimum_rent(GameSession::LEN),
            GameSession::LEN as u64,
            program_id,
        ),
        &[player.clone(), session_info.clone(), system_program.clone()],
        &[&[b"session", player.key.as_ref(), &id_bytes, &[session_bump]]],
    )?;

    // Move the stake into the vault. Player signs directly, so this is a plain
    // `invoke` — no PDA signature needed on the way IN.
    // Vault also needs rent for a 0-byte system account to stay alive.
    let vault_funding = wager
        .checked_add(minimum_rent(0))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    invoke(
        &system_instruction::transfer(player.key, vault_info.key, vault_funding),
        &[player.clone(), vault_info.clone(), system_program.clone()],
    )?;

    let clock = get_clock();
    let session = GameSession {
        player: *player.key,
        wager,
        session_id,
        opened_at: clock.unix_timestamp,
        status: STATUS_OPEN,
        bump: session_bump,
        vault_bump,
    };
    write_state(session_info, &session)
}

// ---------------------------------------------------------------------------
// 2. SettleSession
// ---------------------------------------------------------------------------

fn settle_session<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    player_won: bool,
) -> Result<(), ProgramError> {
    let account_iter = &mut accounts.iter();
    let authority = next_account_info(account_iter)?;
    let config_info = next_account_info(account_iter)?;
    let session_info = next_account_info(account_iter)?;
    let vault_info = next_account_info(account_iter)?;
    let player = next_account_info(account_iter)?;
    let house_treasury = next_account_info(account_iter)?;
    let system_program = next_account_info(account_iter)?;

    // Only the configured house authority may report an outcome.
    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let config = read_config(program_id, config_info)?;
    if config.authority != *authority.key {
        return Err(EscrowError::BadAuthority.into());
    }

    // Session must be owned by this program and still open.
    if session_info.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let mut session = read_session(session_info)?;

    // M-5: re-derive the session PDA from stored state instead of trusting the caller.
    // The vault is derived from this key, so it must be proven, not assumed.
    let expected_session = Pubkey::find_program_address(
        &[b"session", session.player.as_ref(), &session.session_id.to_le_bytes()],
        program_id,
    )
    .0;
    if session_info.key != &expected_session {
        return Err(EscrowError::BadPda.into());
    }

    // C-2: the treasury is fixed in config. Without this, settlement could pay losing
    // stakes into any account the caller chose.
    if config.house_treasury != *house_treasury.key {
        return Err(EscrowError::BadTreasury.into());
    }

    if session.status != STATUS_OPEN {
        // Terminal state — blocks double settlement / replay.
        return Err(EscrowError::SessionNotOpen.into());
    }
    if session.player != *player.key {
        return Err(EscrowError::WrongPlayer.into());
    }

    // Re-derive the vault to be sure we're draining the right one.
    let vault_seeds: &[&[u8]] = &[b"vault", session_info.key.as_ref()];
    let (expected_vault, vault_bump) = Pubkey::find_program_address(vault_seeds, program_id);
    if vault_info.key != &expected_vault {
        return Err(EscrowError::BadPda.into());
    }

    // Payout is derived from stored state, never from instruction data.
    let stake = session.wager;
    let vault_balance = **vault_info.try_borrow_lamports()?;
    if vault_balance < stake {
        return Err(EscrowError::InsufficientVault.into());
    }

    // Winner: player gets their stake back plus a matching amount from the house.
    // Loser: the stake goes to the house treasury.
    //
    // NOTE: for MVP simplicity the house's matching half is paid by the treasury in a
    // separate transfer, so the vault only ever pays out what it actually holds.
    let vault_signer_seeds: &[&[u8]] = &[b"vault", session_info.key.as_ref(), &[vault_bump]];

    if player_won {
        // Return the stake from the vault to the player.
        invoke_signed(
            &system_instruction::transfer(vault_info.key, player.key, stake),
            &[vault_info.clone(), player.clone(), system_program.clone()],
            &[vault_signer_seeds],
        )?;
        // House pays the winnings (1:1 payout).
        if !house_treasury.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }
        invoke(
            &system_instruction::transfer(house_treasury.key, player.key, stake),
            &[house_treasury.clone(), player.clone(), system_program.clone()],
        )?;
        session.status = STATUS_WON;
    } else {
        // House takes the stake.
        invoke_signed(
            &system_instruction::transfer(vault_info.key, house_treasury.key, stake),
            &[vault_info.clone(), house_treasury.clone(), system_program.clone()],
            &[vault_signer_seeds],
        )?;
        session.status = STATUS_LOST;
    }

    write_state(session_info, &session)
}

// ---------------------------------------------------------------------------
// 3. ReclaimSession  (H-3: the player's escape hatch)
// ---------------------------------------------------------------------------

/// Refund an unsettled session to its player once the house has had long enough.
///
/// Without this the house can lock a player's stake forever simply by never settling:
/// go offline, lose the key, run out of funds, or just decline to pay a winner. The
/// player had no recourse at all. This gives them a unilateral exit that needs no
/// cooperation from the house.
fn reclaim_session<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> Result<(), ProgramError> {
    let account_iter = &mut accounts.iter();
    let player = next_account_info(account_iter)?;
    let session_info = next_account_info(account_iter)?;
    let vault_info = next_account_info(account_iter)?;
    let system_program = next_account_info(account_iter)?;

    // Only the player can reclaim, and only for their own session.
    if !player.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if session_info.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    let mut session = read_session(session_info)?;
    if session.player != *player.key {
        return Err(EscrowError::WrongPlayer.into());
    }
    // Terminal states are shared with settlement, so a settled session can never be
    // reclaimed and a reclaimed session can never be settled.
    if session.status != STATUS_OPEN {
        return Err(EscrowError::SessionNotOpen.into());
    }

    // The house must have had a fair chance to settle first.
    let now = get_clock().unix_timestamp;
    if now < session.opened_at.saturating_add(RECLAIM_TIMEOUT_SECS) {
        return Err(EscrowError::TooEarlyToReclaim.into());
    }

    let expected_session = Pubkey::find_program_address(
        &[b"session", session.player.as_ref(), &session.session_id.to_le_bytes()],
        program_id,
    )
    .0;
    if session_info.key != &expected_session {
        return Err(EscrowError::BadPda.into());
    }

    let vault_seeds: &[&[u8]] = &[b"vault", session_info.key.as_ref()];
    let (expected_vault, vault_bump) = Pubkey::find_program_address(vault_seeds, program_id);
    if vault_info.key != &expected_vault {
        return Err(EscrowError::BadPda.into());
    }

    let stake = session.wager;
    if **vault_info.try_borrow_lamports()? < stake {
        return Err(EscrowError::InsufficientVault.into());
    }

    invoke_signed(
        &system_instruction::transfer(vault_info.key, player.key, stake),
        &[vault_info.clone(), player.clone(), system_program.clone()],
        &[&[b"vault", session_info.key.as_ref(), &[vault_bump]]],
    )?;

    session.status = STATUS_RECLAIMED;
    write_state(session_info, &session)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode a GameSession without panicking on a short account.
/// H-4: slicing `[..LEN]` panics when the account is smaller, which the 49-byte config
/// account triggered. A panic is never an acceptable error path.
fn read_session(info: &AccountInfo) -> Result<GameSession, ProgramError> {
    let data = info.try_borrow_data()?;
    let slice = data
        .get(..GameSession::LEN)
        .ok_or(ProgramError::InvalidAccountData)?;
    GameSession::try_from_slice(slice).map_err(|_| ProgramError::InvalidAccountData)
}

/// Load config from its PDA, verifying the address is program-derived.
fn read_config(program_id: &Pubkey, config_info: &AccountInfo) -> Result<Config, ProgramError> {
    let (expected_config, _) = Pubkey::find_program_address(&[b"config"], program_id);
    if config_info.key != &expected_config {
        return Err(EscrowError::BadPda.into());
    }
    if config_info.owner != program_id || config_info.data_is_empty() {
        return Err(EscrowError::NotInitialized.into());
    }
    let data = config_info.try_borrow_data()?;
    let slice = data
        .get(..Config::LEN)
        .ok_or(ProgramError::InvalidAccountData)?;
    Config::try_from_slice(slice).map_err(|_| ProgramError::InvalidAccountData)
}

/// Borsh-serialize state into an account's data buffer.
fn write_state<T: BorshSerialize>(account: &AccountInfo, state: &T) -> Result<(), ProgramError> {
    let serialized = borsh::to_vec(state).map_err(|_| ProgramError::InvalidAccountData)?;
    let mut data = account.try_borrow_mut_data()?;
    if data.len() < serialized.len() {
        return Err(ProgramError::AccountDataTooSmall);
    }
    data[..serialized.len()].copy_from_slice(&serialized);
    Ok(())
}
