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

## Settlement backend

The house authority key authorizes every payout, so it **cannot** live in browser code —
anyone holding it could settle any session as a win and drain the vaults. It lives here.

```bash
cd backend
cargo build

HOUSE_AUTHORITY_KEY_FILE=../program/.testnet-authority.json \
PROGRAM_ID=e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4 \
ARCH_NETWORK=testnet PORT=8091 \
cargo run
```

In production use `HOUSE_AUTHORITY_SECRET_KEY` (64-char hex) from your host's secret store
instead of a file path. See [`.env.example`](.env.example).

### Endpoints

`GET /health`

```json
{ "ok": true, "network": "https://rpc.testnet.arch.network",
  "program_id": "e2c42f...", "house_authority": "ab8215...", "block_height": 35691436 }
```

`POST /settle` — `{"player": "<64-char hex>", "session_id": 12345}`

```json
{ "player_won": true, "status": "SettledWon", "session_id": 696602754 }
```

### Try the full split flow

```bash
# 1. player opens a session from their own key (stands in for a browser wallet)
cd program && cargo run --features no-entrypoint --example open_session -- testnet

# 2. settle it via the backend (prints the exact curl for you)
curl -X POST http://localhost:8091/settle -H 'Content-Type: application/json' \
  -d '{"player":"<pubkey>","session_id":<id>}'
```

Verified on testnet — player balance went `984370 → 1004370` (+20000 on a 10000 wager).

### What the backend guards

| Case | Response |
|---|---|
| Session not on-chain | `400 session not found on-chain` |
| Session belongs to another player | `400 session belongs to a different player` |
| Already settled | `409 session already settled` |
| Malformed pubkey | `400 player must be hex` / `must be 32 bytes` |
| Duplicate request | Returns the original result (idempotent) |

Two independent layers stop a double settle: an in-memory idempotency cache, **and** an
on-chain status check. Restarting the service clears the cache — the chain still returns 409.

The service verifies the session is genuinely `Open` **before** flipping, so it never flips
for a session that does not exist or is already closed. It also reads the status back from
chain after settling rather than trusting its own coin flip.

**Trust assumption, stated plainly:** the house decides the outcome. This is not a trustless
game — see "Why the result is off-chain".

## Frontend

A one-page React app. Playable on Arch testnet with **no wallet install required**.

```bash
# terminal 1 — settlement backend
cd backend
HOUSE_AUTHORITY_KEY_FILE=../program/.testnet-authority.json \
PROGRAM_ID=e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4 \
ARCH_NETWORK=testnet PORT=8091 cargo run

# terminal 2 — frontend
cd app
npm install
npm run dev        # http://localhost:5173
```

Click **Flip the coin**. The app opens a real session on testnet, escrows the stake in
a vault PDA, settles via the backend, and reads the result back from Arch RPC.

### How it splits work

| Operation | Where |
|---|---|
| Derive PDAs (`config`, `session`, `vault`) | Browser — `PubkeyUtil.findProgramAddress` |
| Read balances + session status | Browser — direct Arch RPC |
| Open session (stake escrow) | Backend (demo mode) |
| Settle (needs house authority key) | Backend — **never** the browser |

On-chain state shown in the UI is read straight from Arch RPC, not echoed back by the
backend, so it reflects what actually landed on chain.

### ⚠️ Pin the SDK to 0.0.26 — `latest` is a broken publish

`@arch-network/arch-sdk@0.0.27` (the `latest` tag) **publishes no code**. Its tarball is
only LICENSE, README.md and package.json — 3 files, 15 kB — and `main` points at a
`dist/index.js` that does not exist, so importing it throws `MODULE_NOT_FOUND`.

`0.0.26` and `0.0.25` are fine (7 files, ~250 kB, full `dist/`). This app pins **`0.0.26`
exactly**. Do not use a caret range without checking: `^0.0.26` happens to be safe because
npm's caret does not cross a `0.0.x` minor, but pinning is clearer about why.

`@saturnbtcio/arch-sdk@0.0.24` — the package the official one was forked from — also works
and is a viable fallback.

Verified working from `0.0.26`: `PubkeyUtil.findProgramAddress` (derives byte-identical
PDAs to the Rust program — checked against the live on-chain Config account),
`RpcConnection.readAccountInfo`, `MessageUtil.hash`, `SignatureUtil.adjustSignature`.

### Wallet connect

Arch signs with **secp256k1 / BIP-322** using a Bitcoin Taproot wallet — not ed25519 —
so Phantom and `@solana/wallet-adapter` do not apply, and no official Arch wallet adapter
exists. The app detects and connects **Unisat** and **Xverse** directly via their injected
providers, and shows the connected Bitcoin address.

**Wallet-signed staking is not wired up.** Opening a session from a wallet requires full
BIP-322 transaction signing in the browser: hash the message with
`SanitizedMessageUtil.hash`, pass the hex to `wallet.signMessage(hash, 'bip322-simple')`,
base64-decode, then strip the witness prefix via `SignatureUtil.adjustSignature`
(66 bytes → strip 2; 67 → strip 2 and trailing sighash byte; 64 → as-is).

Reference implementations, if you want to finish it:
`Arch-Network/arch-wallet-hub` (`packages/arch-swap-engine/src/lib/arch/signing.ts` — the
only one doing real witness-stack parsing rather than guessing byte offsets) and
`Arch-Network/arch-ide` (`frontend/src/utils/client-transaction-signer.ts` — full flow
including `recent_blockhash`, uses `version: 0`).

Be aware these repos disagree on how to extract the signature. That divergence has a cause:
a P2TR `bip322-simple` signature is 66 bytes, but a **P2WPKH one is 107 bytes with two
witness items** and will throw in `adjustSignature`, which accepts only 64/66/67. Fixed-offset
slicing only works if you already know the address is Taproot — so parse the witness stack,
and require a Taproot account.

Untested here: no browser wallet was available to verify against. Get one transaction
accepted end-to-end before building on top of this.

## Frontend status (background)

Not built. Arch has no wallet adapter and no high-level TS client:

> "The Arch TypeScript SDK is a low-level SDK... It does not include high-level abstractions like
> transaction builders or wallet management." — official docs

`@arch-network/arch-sdk` is at `0.0.27`, has no transaction builder and no `findProgramAddress`
helper. Signing is secp256k1/BIP-322 via Bitcoin wallets (Xverse/Unisat), not ed25519 — so there is
no Phantom-style connect flow. A browser demo means hand-rolling message construction and BIP-322
signing. The Rust demo above is the working proof today.

## Deploying

See [DEPLOY.md](DEPLOY.md). Vercel hosts the frontend only — the Rust backend needs a
Docker host (Railway/Fly/Render). Zero third-party API keys required.

## Docs

Full feasibility report, architecture, state machine, and the complete list of upstream bugs
encountered: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
