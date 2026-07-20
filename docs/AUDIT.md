# Architecture audit

Written 2026-07-20 after the criticism that the app "works without anyone connecting their
wallet." That criticism is correct. This document states the problem plainly, then the target.

## What is actually wrong

The app today is **not a dApp**. It is a demo of a program, with the server standing in for
the user.

```
CURRENT (wrong)
  Browser ──"play"──> Backend
                        ├── generates a throwaway keypair
                        ├── funds it from the faucet
                        ├── signs OpenSession with it      <-- server is the "player"
                        └── signs SettleSession as house   <-- server is also the house
  Browser <──result───┘
```

The consequences, stated without softening:

1. **No user identity.** The player key is created and discarded per round. Nothing ties a
   round to a person. Connecting a wallet changes nothing: it displays an address and is
   otherwise inert.
2. **No user funds.** The bet is faucet money the server requested seconds earlier. The user
   risks nothing, so "escrow" is theatre.
3. **The server signs both sides.** It is simultaneously the player and the house. An escrow
   whose two parties are the same process is not an escrow.
4. **Nothing is verifiable by the user.** They cannot check that the key that bet was theirs,
   because it never was.

The on-chain program is fine. `OpenSession` already requires `player.is_signer` and derives
the session PDA from the player pubkey. It has always been capable of accepting a real user.
**The failure is entirely client-side**: nobody was building and signing a transaction in the
browser, so the backend did it instead.

## What it should be

```
TARGET
  Browser
    ├── connect Bitcoin wallet (Xverse / Unisat)
    ├── derive the user's Arch pubkey from their taproot internal key
    ├── ensure that pubkey is funded (request_airdrop, testnet)
    ├── build the OpenSession instruction client-side
    ├── hash the message, wallet signs it via BIP-322
    └── submit the RuntimeTransaction to Arch RPC directly
                                    │
                                    ▼
                          user's own key is the signer
                          user's own lamports are escrowed
                                    │
  Backend ──SettleSession──────────►┘   (house authority only; cannot open sessions)
```

The backend keeps exactly one job: hold the house authority key and settle. It never touches
a player key.

## Verified facts that make this possible

| Question | Answer | Evidence |
|---|---|---|
| Can an arbitrary (wallet-derived) pubkey be funded? | **Yes** | `request_airdrop` on an unknown pubkey returned a txid; the account then held 1,000,000 lamports, system-owned, 0 data. No signature from the key owner required. |
| Does the program need changing? | **No** | `OpenSession` already asserts `player.is_signer`; the session PDA is seeded with the player pubkey. |
| Can the browser derive PDAs identically to Rust? | **Yes** | JS `PubkeyUtil.findProgramAddress` produced the same config PDA the Rust program wrote, verified against live chain state. |
| Can the browser read chain state directly? | **Yes** | `RpcConnection.readAccountInfo` against testnet returns the real account. |

`create_account_with_faucet` behaves differently and is worth knowing: it returns a
**partially signed transaction** with `num_required_signatures: 2` (faucet signed, user must
add theirs) which you then submit. `request_airdrop` is the simpler path and needs no user
signature at all.

## The one genuinely hard part

Signing. Arch verifies **BIP-322** signatures over the message hash, using secp256k1 Schnorr
with a Bitcoin Taproot key. Not ed25519. There is no wallet adapter, no official browser
signing guide, and the Rust `sign_message_bip322` builds two Bitcoin transactions
(`to_spend` / `to_sign`) and signs a taproot sighash.

In the browser the wallet does that work: `wallet.signMessage(hash, 'bip322-simple')` returns
a BIP-322 signature whose witness stack contains the 64-byte Schnorr signature Arch wants.
`SignatureUtil.adjustSignature` exists precisely to strip the witness prefix
(66 bytes → drop 2; 67 → drop 2 and a trailing sighash byte; 64 → as-is).

**Known trap:** those lengths hold for **P2TR**. A P2WPKH `bip322-simple` signature is
107 bytes with two witness items and will throw in `adjustSignature`. So the connected
account must be Taproot, and the extraction should parse the witness stack rather than slice
at a fixed offset.

## Honest limitation

I cannot execute a browser-wallet signature in this environment: there is no wallet extension
to sign with. Everything else in the target flow is verified against live testnet. The signing
step is implemented from the Arch team's own reference implementations, and is the one part
that needs a human with Xverse or Unisat to confirm.

Demo mode is kept, clearly labelled, so the program remains demonstrable without a wallet.
It is a fallback, not the headline.

---

# Implementation: verified 2026-07-20

The wallet path is built and proven on live testnet. `app/verify-wallet-path.mjs` reproduces
it: it runs the exact browser code, substituting `bip322-js` for the wallet extension (same
BIP-322 spec, same base64 witness blob).

```
1. Taproot identity  tb1pck0m2y35q0x4u48vgy5fcy4wjrln23k9w6amzwnr32j6mr7kglksv7rcvr
2. request_airdrop   funded 1,000,000 lamports, no signature needed
3. OpenSession built client-side
4. sanitized message  1 signer, 6 account keys
5. challenge          8ada7e7d…57e0   (64-char hex, verified)
6. signed by wallet
7. witness stack      1 item, 65 bytes -> 64-byte Schnorr
8. txid               8a9e3fe6d868c692132eabe0e0719d3499fa2fee37ad6cb729bc5353f2217fd4
9. status             processed
   vault holds        10,256 lamports
   session.player     matches the signing key
   session.status     0 (Open)
```

## What this proves

The user's own Taproot key signs, and the escrow account records **that key** as the player.
No server key is involved in opening a session.

## Bug this caught

The witness item came back as **65 bytes**: a 64-byte Schnorr signature plus a trailing
sighash flag. `SignatureUtil.adjustSignature` handles 64, 66 and 67 and **throws on 65**, so
the first implementation failed at extraction.

Both published reference implementations would hit this too: `arch-ide` slices blindly, and
`arch-wallet-hub` routes a 65-byte item into `adjustSignature`. The fix is to strip the
trailing sighash byte, and to fall through to the raw-blob path rather than letting
`adjustSignature` throw.

Only a real signature exposes this. It is the strongest argument for keeping the
verification script in the repo.

## Confirmed against Arch internals

| Claim | Status |
|---|---|
| Arch pubkey is the taproot **internal** x-only key | Confirmed. Pass the tweaked output key and you double-tweak; Arch rejects with `error checking transaction sigs`. |
| Challenge is the message hash as a 64-char hex **string**, passed verbatim | Confirmed. `SanitizedMessageUtil.hash` returns the UTF-8 bytes of that string; hex-decoding it signs the wrong bytes. |
| `signatures[i]` maps positionally to `account_keys[i]` | Confirmed. |
| Fee payer may differ from the instruction signer | True, but the payer is forced to signer+writable at `account_keys[0]`, so a sponsor must co-sign. No unilateral sponsorship. |
| Multi-signer transactions | Supported, `MAX_SIGNERS = 16`. Both parties must sign the same serialized message with the same blockhash. |

## Still unverified

The literal `window.unisat.signMessage(...)` / Xverse `request('signMessage', ...)` call.
`bip322-js` implements the same specification and produces the same witness format, so the
parsing and submission path is proven, but only a human with the extension installed can
confirm the browser handshake.

Arch's own Squads V4 port carries the same caveat: *"Wallet signing (Xverse/Unisat) could not
be exercised headlessly here."*
