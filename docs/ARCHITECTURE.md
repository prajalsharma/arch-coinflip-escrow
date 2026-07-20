# Arch Network Escrow + Off-Chain Game MVP — Architecture

Research date: 2026-07-20. Primary source: `arch_program` v0.6.7 (published 2026-07-14).
Design constraint (from founder): **escrow contract on-chain, game results off-chain.**

## What changed vs. the original brief

The founder's constraint removes the hardest blocker. Because outcomes are decided off-chain and
submitted by an authorized house signer:

- **Randomness is a non-issue.** Arch has no VRF/`slot_hashes`/`recent_blockhashes` (verified: 35
  modules, none provide randomness). We no longer need any, because the program never generates the
  result — it only verifies who signed the settlement.
- **Commit–reveal is dropped.** Not needed. (It *was* viable — `hashing_functions::sha256` is
  callable in-program — but it's now dead weight.)
- **The program is a pure escrow**, which is exactly what Arch's own `escrow` example demonstrates.

Trust model: the house authority is trusted to report results honestly. That is the standard
MVP shape and is explicitly **non-trustless** — see Security.

## Feasibility Verdict: FEASIBLE

| # | Question | Verdict | Evidence |
|---|---|---|---|
| 1 | Game MVP supported? | Yes | Solana-compatible eBPF runtime (ArchVM) |
| 2 | Escrow funds? | Yes | `AccountInfo.lamports`, `system_instruction::transfer`, official `escrow` example |
| 3 | Store game state? | Yes | Borsh into `account.data` |
| 4 | PDA owns session state? | Yes | `Pubkey::find_program_address`, `invoke_signed` (used in `escrow` example) |
| 5 | Claim winnings? | Yes | PDA-signed payout |
| 6 | Partial off-chain logic? | Yes | This is now the core of the design |
| 7 | Randomness primitive? | No — **and no longer needed** | No VRF module in crate |
| 8 | Testnet compatible? | Yes | `https://rpc.testnet.arch.network` |

### Escrow is in lamports, not raw BTC

`AccountInfo` (verified verbatim from docs.rs):

```rust
#[repr(C)]
pub struct AccountInfo<'a> {
    pub key: &'a Pubkey,
    pub lamports: Rc<RefCell<&'a mut u64>>,   // value lives here
    pub utxo: &'a UtxoMeta,                    // BTC anchoring, separate concern
    pub data: Rc<RefCell<&'a mut [u8]>>,
    pub owner: &'a Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
    pub is_executable: bool,
}
```

`UtxoMeta` (36 bytes: 32-byte txid + 4-byte LE vout) anchors accounts to Bitcoin; BTC settlement
happens via validator FROST/ROAST threshold signing and is NOT an automatic per-instruction side
effect. So "Escrow PDA -> Bitcoin Settlement" per claim is wrong. Out of scope for MVP.

## RESOLVED RISK — native lamport escrow VERIFIED on-chain (2026-07-20)

The concern below was real but is now **settled empirically**, not by inference. Both integration
tests pass against a live local validator:

```
test test_escrow_lamport_round_trip ...
config initialized: Config { authority: cf173c42..., min_wager: 1000, max_wager: 1000000, bump: 255 }
vault lamports after open:   10256
vault lamports after settle:   256
PDA lamport escrow round-trip WORKS on Arch
ok
test test_player_wins_gets_paid ... winner paid: 984370 -> 1004370 (+20000)
ok

test result: ok. 2 passed; 0 failed
```

Confirmed working on Arch:
- `invoke(system_instruction::transfer)` — lamports INTO a PDA vault (player-signed)
- `invoke_signed(system_instruction::transfer)` — lamports OUT of a PDA vault (PDA-signed)
- `system_instruction::create_account` (plain, no UTXO anchor) for both program-owned and
  system-owned PDAs
- `get_clock()`, `minimum_rent()`, `find_program_address`, `ProgramError::Custom`

**No APL token fallback needed.** The design stands as written.

### Original risk assessment (kept for the record)

`system_instruction::transfer(from, to, lamports) -> Instruction` exists in the API, but a sweep of
every arch-example (helloworld, counter, escrow, oracle, stake, vote) found **zero uses of it**.
All value movement, including in the official `escrow` example, uses `apl_token::instruction::transfer`.

Reclaiming native lamports is likewise unverified: no `AccountInfo::close()`, no
`SystemInstruction::Close`, no example doing it. Direct `**acc.try_borrow_mut_lamports()? -= x` is
mechanically expressible and the runtime clearly validates lamport mutation (`NegativeAccountLamports`,
`ReadonlyLamportChange`, `ExecutableLamportChange` exist), but that is inference, not evidence.

**Mitigation — the design avoids direct lamport mutation entirely.** Split the escrow:

- **Vault PDA** — system-owned, zero data, holds only lamports. Payouts leave via
  `invoke_signed(system_instruction::transfer(...))`, a documented API call.
- **Session PDA** — program-owned, holds Borsh state, never holds stake.

If `system_instruction::transfer` turns out not to work from a PDA on Arch, the fallback is APL
token (mirroring the official escrow example exactly). **Step 0 of the plan is a throwaway spike
that tests exactly this on local regtest** — before writing the real program.

Also note: both examples create accounts via `create_account_with_anchor(..., txid, vout)`, requiring
a Bitcoin UTXO from the client. Plain `create_account` exists but is unused in examples — the spike
tests this too.

## Architecture

```
React frontend (one page)
        |
   Arch wallet  (@arch-network/arch-sdk)
        |
   Arch RPC  (local regtest :9002 -> testnet)
        |
   Escrow Program  (arch_program 0.6.x, eBPF)
        |
   +----+------------------------+
   |                             |
Session PDA               Vault PDA
["session", player, id]   ["vault", session_pda]
program-owned, Borsh      system-owned, lamports only
        ^
        |
House authority (off-chain) signs SettleSession
```

## Instructions (3)

| Instruction | Signer | Purpose |
|---|---|---|
| `InitializeConfig` | house authority | One-time; records authority pubkey + wager bounds |
| `OpenSession` | player | Create Session PDA + Vault PDA, transfer stake into vault |
| `SettleSession` | house authority + player | Off-chain result submitted; pay winner from vault, mark Settled |

Original `Play` / `RecordOutcome` collapse into `SettleSession` (result comes from off-chain).
`DepositStake` folds into `OpenSession`. No `Claim` — settlement pays out directly, which removes
an entire double-claim surface.

## State

```rust
#[derive(BorshSerialize, BorshDeserialize)]
pub struct Config {
    pub authority: Pubkey,   // 32  house signer for settlements
    pub min_wager: u64,      // 8
    pub max_wager: u64,      // 8
    pub bump: u8,            // 1
}                            // 49 bytes

#[derive(BorshSerialize, BorshDeserialize)]
pub struct GameSession {
    pub player: Pubkey,      // 32
    pub wager: u64,          // 8
    pub session_id: u64,     // 8  uniqueness / replay guard
    pub opened_at: i64,      // 8  from get_clock()
    pub status: u8,          // 1  0=Open 1=SettledWon 2=SettledLost
    pub bump: u8,            // 1
    pub vault_bump: u8,      // 1
}                            // 59 bytes
```

## State Machine

```
[none] --OpenSession--> Open --SettleSession(won)--> SettledWon   [vault -> player, 2x]
                             \-SettleSession(lost)-> SettledLost  [vault -> house]
```

Terminal states. No transition out of Settled — that is the double-claim guard.

## Security

| Risk | Mitigation |
|---|---|
| Double claim | `status != Open` -> reject. Terminal states, checked before payout |
| Duplicate session | PDA seeded with `session_id`; account creation fails if it exists |
| Unauthorized payout | Assert `config.authority == authority.key` AND `authority.is_signer` |
| Replay | Session PDA is single-use; settled sessions can never re-settle |
| Vault drain | Payout amount derived from `session.wager`, not from instruction data |
| Wrong player paid | Assert `session.player == player.key` |
| Fake config | Config PDA address is program-derived and verified, not passed in blind |
| Arithmetic | `checked_mul`/`checked_add` -> `ProgramError::ArithmeticOverflow` |

**Accepted, documented limitation:** the house authority is trusted to report results honestly.
This is not a trustless game. Making it trustless requires two-party commit–reveal or an external
VRF oracle — out of scope for an MVP, and worth stating plainly in any demo.

## Client SDK (verified)

Rust: **`arch_sdk = "0.6.7"`** (published 2026-07-14). Re-exports `arch_program` types, RPC clients,
helpers. Provides `ArchRpcClient` (async) / `BlockingArchRpcClient` (sync), and
`ProgramDeployer` / `BlockingProgramDeployer` — **programs can be deployed from Rust, no CLI needed.**

Config presets: `Config::localnet()` (Bitcoin RPC + Titan + Arch RPC on regtest), `Config::devnet()`,
`Config::testnet()`, `Config::mainnet()`.

TypeScript: **`@arch-network/arch-sdk`** (npm), repo `Arch-Network/arch-typescript-sdk`.
(Note: `@saturnbtcio/arch-sdk` also exists and surfaces in search results — it is NOT the one the
official docs point to.)

### Verified integration-test pattern (from `arch-examples/examples/helloworld/src/lib.rs`)

```rust
let config = Config::localnet();
let client = ArchRpcClient::new(&config);

let (authority_keypair, authority_pubkey, _) = generate_new_keypair(config.network);
client.create_and_fund_account_with_faucet(&authority_keypair).unwrap();

let (program_keypair, _) = with_secret_key_file(".program.json")
    .expect("getting caller info should not fail");

let transaction = build_and_sign_transaction(
    ArchMessage::new(
        &[/* instructions */],
        Some(authority_pubkey),
        client.get_best_finalized_block_hash().unwrap(),
    ),
    vec![first_account_keypair, authority_keypair],
    config.network,
);

let txids = client.send_transactions(vec![transaction]).unwrap();
let block_transactions = client.wait_for_processed_transactions(txids).unwrap();
```

Note that helloworld creates its account with `create_account_with_anchor(..., minimum_rent(0), 0,
&program_pubkey, hex::decode(txid)..., vout)` — i.e. **space 0 and a real Bitcoin txid/vout**. This
reinforces that UTXO-anchored account creation is the mainstream path.

## Development Plan

- [x] **0. Value-layer spike** — folded into the integration test instead of a throwaway program.
      `test_escrow_lamport_round_trip` IS the spike. PASSED.
- [x] **1. Escrow program** — 3 instructions, ~330 lines. Compiles and runs on localnet.
- [x] **2. Rust integration tests** — 2 tests passing against a live validator.
- [ ] **3. Single-page React frontend** — see the frontend reality check below.
- [ ] **4. Testnet deploy** — via `--profile testnet`.

## Environment bugs hit (and their workarounds)

These are upstream Arch issues as of 2026-07-20, not project issues. Documented so the next
person does not lose an hour to them.

1. **The documented CLI installer 404s.** `https://raw.githubusercontent.com/Arch-Network/arch-node/main/install.sh`
   returns `404: Not Found`. Workaround: install from the Homebrew formula URL, or download the
   release binary directly and verify against the formula's pinned sha256.
2. **`local_validator:latest` is broken.** The binary (built 2026-07-14) requires GLIBC 2.38 but the
   image ships Debian 12 bookworm with GLIBC 2.36, so the container exits instantly:
   `` /bin/local_validator: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found ``
   Pinning to `0.2.16` does NOT work — that build predates the `--titan-endpoint` flag arch-cli 0.6.7
   passes. Workaround: `docker/validator-fix.Dockerfile` rebuilds the same binary on `debian:trixie-slim`.
3. **`arch-cli validator-start` (native, non-Docker) panics** on startup:
   `failed to set global default subscriber: SetGlobalDefaultError("a global default trace dispatcher has already been set")`.
   Use the Docker path with the patched image instead.
4. **`entrypoint!` segfaults host test binaries.** The macro installs an SBF `BumpAllocator` global
   allocator; linking it into a host test process kills it with SIGSEGV before any test runs.
   Workaround: gate it behind a `no-entrypoint` feature and run
   `cargo test --features no-entrypoint`. `cargo build-sbf` uses default features and keeps it.
   This is not documented anywhere in the Arch docs.

## Frontend reality check — this is harder than a Solana dApp

Research finding that materially affects deliverable #3. Arch has **no wallet adapter and no
high-level TS client**. Quoting the official book:

> "The Arch TypeScript SDK is a low-level SDK that provides direct RPC access to Arch nodes.
> It does not include high-level abstractions like transaction builders or wallet management."

Concretely:
- `@arch-network/arch-sdk` is at **0.0.27** (last publish 2026-03-10). It exposes `RpcConnection`,
  `ArchConnection`, and byte-level utils — no transaction builder, no wallet management, and
  **no `findProgramAddress` helper** (PDA derivation must be reimplemented client-side).
- Signing is **secp256k1 / BIP-322 Schnorr with a Bitcoin wallet** (Xverse, Unisat) — NOT ed25519,
  NOT Phantom-style. There is no `@solana/wallet-adapter` equivalent.
- Documented gotchas: use `vite-plugin-node-polyfills` (injection order matters or you get a blank
  page); Arch testnet anchors to Bitcoin **Testnet4** (asking for testnet3 = network mismatch);
  `tapInternalKey` must be the wallet's **internal** x-only key, not the tweaked output key, or
  Arch rejects with `error checking transaction sigs`.
- `Arch-Network/arch-wallet-hub` is the org's most active repo and looks like the intended answer,
  but `@arch/wallet-hub-ui` does not appear to be published to npm — treat as internal/unreleased.

**Implication:** a "connect wallet and play" browser demo is a meaningful chunk of work, not an
afternoon. The honest MVP options are (a) a Rust CLI demo driving the program, which works today,
or (b) a React page that reimplements message construction + BIP-322 signing against Xverse.

## Folder Structure

```
arch-coinflip/
├── spike/                  # step 0, throwaway
├── program/
│   ├── Cargo.toml          # arch_program 0.6.x, borsh 1.5.1, crate-type cdylib+lib
│   └── src/lib.rs
├── app/                    # Vite + React, one page
│   └── src/App.tsx
└── docs/ARCHITECTURE.md
```

## Toolchain (verified)

Prereqs: Git 2.0+, Rust **1.94.0**+, Solana/Agave CLI **3.1.9+** (provides `cargo-build-sbf`),
Docker, `arch-cli`.

Install the CLI. NOTE: the documented `curl install.sh` URL **404s** as of 2026-07-20; use Homebrew:
```bash
curl -sSL https://raw.githubusercontent.com/Arch-Network/homebrew-tap/main/arch-cli.rb -o arch-cli.rb
brew install ./arch-cli.rb        # v0.6.7, pinned sha256
```

Local stack, build, deploy:
```bash
arch-cli orchestrate start        # Bitcoin regtest + Titan indexer + validator
cargo build-sbf
arch-cli deploy target/deploy/ --generate-if-missing --fund-authority
arch-cli show <PROGRAM_ID>
```

Funding: `arch-cli account airdrop --keypair-path <PATH> --amount <LAMPORTS>`

Testnet profile:
```bash
arch-cli config create-profile testnet \
    --bitcoin-node-endpoint http://bitcoin-rpc.test.arch.network:80 \
    --bitcoin-node-username bitcoin \
    --bitcoin-node-password 0F_Ed53o4kR7nxh3xNaSQx-2M3TY16L55mz5y9fjdrk \
    --bitcoin-network testnet \
    --arch-node-url https://rpc.testnet.arch.network \
    --titan-url https://titan.testnet.arch.network
```
Then `arch-cli --profile testnet deploy ...`. Local RPC `http://localhost:9002`.

### Traps to avoid

1. **`github.com/Arch-Network/arch-cli` is DEPRECATED** — README says so verbatim; its commands
   (`init`, `project create`, `validator start`) don't exist in the shipping CLI.
2. **Satellite's CLI binary is named `anchor`** (`satellite/cli/Cargo.toml` -> `[[bin]] name = "anchor"`).
   Confusable with real Solana Anchor. Reason enough to prefer raw `arch_program`.
3. No `arch-cli new`/`init` — scaffold with `cargo init --lib`.
4. No `arch-cli test` — use `cargo test -- --nocapture`.
5. No `Arch.toml`/`Satellite.toml`; config lives in CLI profiles.
6. No public faucet website found — fund via `arch-cli account airdrop`.
7. The documented `install.sh` URL 404s.

## Framework choice: raw `arch_program`

Every official example and book page uses raw `arch_program` with `entrypoint!` + Borsh. Satellite
(Anchor fork, `arch_satellite_lang` 0.31) exists and is ~95% Anchor-compatible, but no official Arch
example uses it and its CLI install is undocumented. At ~300 lines the Anchor ergonomics buy little.

Verified entrypoint contract:
```rust
pub type ProcessInstruction =
    fn(program_id: &Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult;
```
