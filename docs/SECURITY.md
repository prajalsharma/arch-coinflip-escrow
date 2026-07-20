# Security review

Adversarial review of `coinflip_escrow`, 2026-07-21. Findings are ordered by what an
attacker actually gains, not by how clever the bug is.

---

## C-1 · `InitializeConfig` is unauthenticated: first caller becomes the house

**Status: FIXED**

```rust
if !authority.is_signer { return Err(MissingRequiredSignature); }
// ...nothing else checks WHO the authority is
let config = Config { authority: *authority.key, .. };
```

Any key that calls `InitializeConfig` first on a freshly deployed program becomes
`config.authority` permanently. There is no upgrade path for the authority, so the
deployer loses the program to whoever front-runs them.

Chained with C-2 the attacker then drains every vault: settle every session as a loss and
direct the stakes to an address they control.

Not exploitable on the currently deployed instance, because the config PDA is already
initialized. It is fully exploitable on any redeploy, which makes it a real finding rather
than a theoretical one.

**Fix:** the authority is now pinned at build time. `InitializeConfig` rejects any caller
whose key does not match `EXPECTED_AUTHORITY`. Front-running gains nothing.

---

## C-2 · `house_treasury` was attacker-chosen

**Status: FIXED**

`settle_session` accepted `house_treasury` as an arbitrary account. On a loss it moved
`vault -> house_treasury` and that account **did not even need to sign**. Nothing tied it to
the config. A compromised or malicious authority could route every losing stake anywhere.

**Fix:** `Config` now stores `house_treasury`, and settlement asserts the passed account
matches it.

---

## H-3 · A player could be griefed forever, with no refund

**Status: FIXED**

This is the classic escrow flaw and was the most serious design problem.

Once `OpenSession` succeeded, the player's stake sat in the vault until the house chose to
settle. If the house went offline, lost its key, ran out of funds, or simply declined to
settle a winner, **the player's money was locked permanently.** There was no timeout, no
unilateral exit, no dispute path. The player had no recourse whatsoever.

Telling detail: `GameSession.opened_at` was written on every session and then never read.
The field existed for a timeout that was never implemented.

**Fix:** new `ReclaimSession` instruction. After `RECLAIM_TIMEOUT_SECS` (1 hour) the player
can sign for themselves and take their stake back from the vault. It cannot be used while
the house can still legitimately settle, and settling remains blocked once reclaimed,
because both are terminal states on the same status field.

---

## H-4 · Panic vector on a short account

**Status: FIXED**

```rust
GameSession::try_from_slice(&session_info.try_borrow_data()?[..GameSession::LEN])
```

Slicing `[..59]` on an account holding fewer than 59 bytes panics rather than returning an
error. The config PDA is 49 bytes and is program-owned, so passing it as `session_info`
aborted the program. Callable only by the authority, so the impact is limited, but a panic
is never an acceptable error path.

**Fix:** length-checked `get(..LEN)` with a proper `InvalidAccountData` error.

---

## M-5 · Session PDA was never re-derived during settlement

**Status: FIXED**

`settle_session` verified `session_info.owner == program_id` and `session.player ==
player.key`, but never re-derived the PDA from `["session", player, session_id]`. It trusted
the caller's account. The vault was then derived *from that unverified key*.

No theft path was found, because a program-owned account must have been created by this
program, but "no path found" is not a security property.

**Fix:** the session PDA is re-derived from stored state and compared.

---

## M-6 · House solvency was never checked before accepting a bet

**Status: FIXED**

`OpenSession` accepted any in-bounds bet without checking the house could cover a win. With
an underfunded treasury, a winning settlement fails and the player's stake stays locked in
an open session. The player is penalised for the house's accounting.

**Fix:** `OpenSession` now requires `house_treasury.lamports >= wager` before accepting, so
a bet is only taken when the payout is already covered. Combined with H-3, a player who
somehow still gets stuck can always reclaim.

---

## L-7 · Rent is stranded on every round

**Status: ACCEPTED, documented**

Each session permanently locks `minimum_rent(59) = 374` plus `minimum_rent(0) = 256` in the
session and vault accounts. Nothing closes them, so state grows without bound and ~630
lamports leak per round.

Not fixed because closing a program-owned account requires direct lamport mutation, which is
undocumented on Arch, and because keeping settled sessions readable is useful for a demo.
`ReclaimSession` does drain the vault, which recovers the larger part.

---

## Accepted by design: the house decides the outcome

Not a bug, but the dominant trust assumption and worth stating in a security document.

Arch exposes no randomness primitive: no VRF, no `slot_hashes`, no `recent_blockhashes`,
verified against all 35 modules of `arch_program` 0.6.7. The result is therefore produced
off-chain by the house and attested by its signature. **A dishonest house can decide every
outcome.**

What the on-chain program still guarantees, even against a dishonest house:

- it cannot pay a winner other than `session.player`
- it cannot settle the same session twice
- it cannot take more than `session.wager` from a vault
- it cannot keep a player's funds past the reclaim timeout

Making the *outcome* trustless needs two-party commit-reveal or an external VRF oracle, and
is out of scope for this MVP.
