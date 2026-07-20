# Arch Coin Flip — Escrow MVP

A minimal demonstration that **Arch Network can support a game-like application**: stakes are
escrowed on-chain in a PDA vault, the game result is decided off-chain, and payout is settled
on-chain by a trusted house authority.

**Live on Arch testnet.** Program ID: `e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4`

```
network        : testnet (Testnet4)
session opened : id=84674 wager=10000
escrow vault   : 10256 lamports held
coin flip      : WIN (decided off-chain)
settled        : SettledWon
net            : +4370
```

## Why the result is off-chain

Arch exposes **no randomness primitive** — no VRF, no `slot_hashes`, no `recent_blockhashes`
(verified against `arch_program` 0.6.7, 35 modules). So the program never generates the outcome.
It escrows the stake and pays out based on the house authority's signature.

This is deliberately **not trustless**: the house is trusted to report results honestly. Making it
trustless needs two-party commit–reveal or an external VRF oracle. Out of scope for an MVP, but
state it plainly in any demo.

## Architecture

```
Player ──stake──> Vault PDA        ["vault", session_pda]   system-owned, lamports only
                  Session PDA      ["session", player, id]  program-owned, Borsh state
                  Config PDA       ["config"]               house authority + wager bounds
                       ^
House authority ──settles (off-chain result attested by signature)
```

Three instructions: `InitializeConfig`, `OpenSession`, `SettleSession`.

The vault is kept **separate from** and **system-owned**, unlike the program-owned session
account. That means payouts use a plain `invoke_signed(system_instruction::transfer)` rather than
mutating account lamports directly — an operation no Arch example demonstrates.

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | 1.94+ | 1.96 tested |
| Agave/Solana CLI | 3.1.9+ | provides `cargo-build-sbf`; 4.1.1 tested |
| `arch-cli` | 0.6.7 | see install note below |
| Docker | any recent | localnet only |

**`arch-cli` install:** the documented `install.sh` URL currently 404s. Install from the Homebrew
formula instead, or grab the release binary directly and verify its checksum:

```bash
curl -sSL https://raw.githubusercontent.com/Arch-Network/homebrew-tap/main/arch-cli.rb -o arch-cli.rb
brew install ./arch-cli.rb
```

## Run it on testnet (no Docker needed)

Testnet is the fastest path — the faucet funds accounts automatically, no Bitcoin wallet required.

```bash
# 1. one-time profile
arch-cli config create-profile testnet \
  --bitcoin-node-endpoint http://bitcoin-rpc.test.arch.network:80 \
  --bitcoin-node-username bitcoin \
  --bitcoin-node-password 0F_Ed53o4kR7nxh3xNaSQx-2M3TY16L55mz5y9fjdrk \
  --bitcoin-network testnet \
  --arch-node-url https://rpc.testnet.arch.network \
  --titan-url https://titan.testnet.arch.network

# 2. build
cd program
cargo build-sbf

# 3. deploy (note: pass the DIRECTORY, not the .so file)
arch-cli --profile testnet deploy target/deploy/ --generate-if-missing --fund-authority

# 4. run the full game flow
cargo run --features no-entrypoint --example demo -- testnet
```

## Run it on localnet

```bash
arch-cli orchestrate start          # bitcoind + titan + validator
cd program
cargo build-sbf
cargo run --features no-entrypoint --example demo -- localnet
```

### If the validator container dies instantly

`local_validator:latest` currently ships a binary built against GLIBC 2.38 on a Debian 12 base
providing only 2.36, so it exits with:

```
/bin/local_validator: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found
```

Pinning to `0.2.16` does not help — that build predates the `--titan-endpoint` flag arch-cli 0.6.7
passes. Rebuild the same binary on a newer base:

```bash
docker build --platform linux/amd64 -f docker/validator-fix.Dockerfile -t arch-validator-fixed:latest docker/
docker tag arch-validator-fixed:latest ghcr.io/arch-network/local_validator:latest
arch-cli orchestrate validator-start
```

## Tests

```bash
cd program
cargo test --features no-entrypoint --test integration -- --nocapture --ignored --test-threads=1
```

Arch has **no in-process test harness** (no `program-test` / litesvm equivalent), so these run
against a live validator. Start localnet first. Expected:

```
test test_escrow_lamport_round_trip ... ok
test test_player_wins_gets_paid ... winner paid: 984370 -> 1004370 (+20000)
test result: ok. 2 passed; 0 failed
```

> **`--features no-entrypoint` is required for anything host-side.** The `entrypoint!` macro
> installs an SBF bump allocator; linking it into a host binary segfaults the process before any
> test runs. `cargo build-sbf` uses default features and keeps the entrypoint.

## Security

| Risk | Mitigation |
|---|---|
| Double claim | Settled states are terminal; `status != Open` rejects |
| Duplicate session | PDA seeded with `session_id`; creation fails if it exists |
| Unauthorized payout | `config.authority == authority.key` **and** `authority.is_signer` |
| Replay | Session PDA single-use |
| Vault drain | Payout derived from stored `session.wager`, never from instruction data |
| Wrong player paid | `session.player == player.key` |
| Arithmetic | `checked_add` → `ArithmeticOverflow` |

**Accepted limitation:** the house authority is trusted. See "Why the result is off-chain".

## Secrets

`.program.json` / `.authority.json` / `.testnet-*.json` hold **raw private keys** written by
`arch_sdk`'s `with_secret_key_file()`. They are gitignored. Never commit them — anyone with the
file can drain the account.

## Frontend status

Not built. Arch has no wallet adapter and no high-level TS client:

> "The Arch TypeScript SDK is a low-level SDK... It does not include high-level abstractions like
> transaction builders or wallet management." — official docs

`@arch-network/arch-sdk` is at `0.0.27`, has no transaction builder and no `findProgramAddress`
helper. Signing is secp256k1/BIP-322 via Bitcoin wallets (Xverse/Unisat), not ed25519 — so there is
no Phantom-style connect flow. A browser demo means hand-rolling message construction and BIP-322
signing. The Rust demo above is the working proof today.

## Docs

Full feasibility report, architecture, state machine, and the complete list of upstream bugs
encountered: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
