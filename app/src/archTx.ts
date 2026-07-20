/**
 * Build, sign and submit an Arch transaction from the browser.
 *
 * The flow Arch actually verifies:
 *   1. Compile instructions into a sanitized message (SDK does the key ordering).
 *   2. Hash it. `SanitizedMessageUtil.hash` returns 64 BYTES that are the UTF-8 of a
 *      64-char lowercase hex string. Verified locally.
 *   3. Hand that hex STRING to the wallet verbatim. No hex-decode, no re-encode.
 *      Double-encoding it is a known failure: Arch rejects with
 *      "BIP322 signature verification failed: Invalid signature".
 *   4. The wallet returns a base64 BIP-322 witness blob. Parse the witness stack and
 *      pull out the 64-byte Schnorr signature.
 *   5. Submit { version: 0, signatures: [number[]], message } to `send_transaction`.
 */
import { SanitizedMessageUtil, SignatureUtil } from '@arch-network/arch-sdk'
import { ARCH_RPC_URL } from './arch'
import type { SignChallenge } from './wallet'

export type SdkInstruction = {
  program_id: Uint8Array
  accounts: { pubkey: Uint8Array; is_signer: boolean; is_writable: boolean }[]
  data: Uint8Array
}

// ---------------------------------------------------------------------------
// RPC
// ---------------------------------------------------------------------------

let rpcId = 0

async function rpc<T>(method: string, params: unknown): Promise<T> {
  const res = await fetch(ARCH_RPC_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++rpcId, method, params }),
  })
  const json = await res.json()
  if (json.error) throw new Error(`${method}: ${json.error.message ?? JSON.stringify(json.error)}`)
  return json.result as T
}

export const getBestBlockHash = () => rpc<string>('get_best_finalized_block_hash', [])

/**
 * Fund a pubkey. Works for ANY key with no signature from its owner, verified against
 * testnet: an unknown pubkey came back holding 1,000,000 lamports, system-owned.
 */
export const requestAirdrop = (pubkey: Uint8Array) =>
  rpc<string>('request_airdrop', Array.from(pubkey))

export const readAccount = (pubkey: Uint8Array) =>
  rpc<{ lamports: number; owner: number[]; data: number[] }>(
    'read_account_info',
    Array.from(pubkey),
  )

export const sendTransaction = (tx: unknown) => rpc<string>('send_transaction', tx)

export const getProcessedTransaction = (txid: string) =>
  rpc<any>('get_processed_transaction', txid)

// ---------------------------------------------------------------------------
// BIP-322 signature extraction
// ---------------------------------------------------------------------------

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64.trim())
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/** Bitcoin CompactSize varint. */
function readCompactSize(
  bytes: Uint8Array,
  offset: number,
): { value: number; nextOffset: number } | null {
  if (offset >= bytes.length) return null
  const first = bytes[offset]
  if (first < 0xfd) return { value: first, nextOffset: offset + 1 }
  if (first === 0xfd) {
    if (offset + 3 > bytes.length) return null
    return { value: bytes[offset + 1] | (bytes[offset + 2] << 8), nextOffset: offset + 3 }
  }
  // 0xfe / 0xff are absurd for a witness stack; reject rather than guess.
  return null
}

/**
 * Parse a BIP-322 witness stack. Returns null if the blob is not a well-formed stack,
 * which is the signal to fall back to treating the bytes as a bare signature.
 */
function parseWitnessStack(bytes: Uint8Array): Uint8Array[] | null {
  const count = readCompactSize(bytes, 0)
  if (!count || count.value <= 0 || count.value > 16) return null

  let offset = count.nextOffset
  const items: Uint8Array[] = []

  for (let i = 0; i < count.value; i++) {
    const size = readCompactSize(bytes, offset)
    if (!size) return null
    offset = size.nextOffset
    if (size.value < 0 || offset + size.value > bytes.length) return null
    items.push(bytes.slice(offset, offset + size.value))
    offset += size.value
  }

  // A trailing byte means we misparsed; do not return a half-read stack.
  return offset === bytes.length ? items : null
}

/**
 * Pull the 64-byte Schnorr signature out of whatever the wallet returned.
 *
 * A P2TR `bip322-simple` blob is 66 bytes: 0x01 (one item) 0x40 (64 long) + signature.
 * A P2WPKH blob is ~107 bytes with TWO items (a ~71-byte DER sig and a 33-byte pubkey)
 * and can never validate, because Arch verifies x-only Schnorr. We detect that case and
 * say so, rather than letting `adjustSignature` throw "Invalid signature length".
 */
export function extractSchnorrSignature(walletSignatureBase64: string): Uint8Array {
  const raw = base64ToBytes(walletSignatureBase64)

  const stack = parseWitnessStack(raw)
  if (stack) {
    if (stack.length >= 2) {
      throw new Error(
        'That account is not Taproot: the wallet returned a 2-item witness (ECDSA + pubkey). ' +
          'Arch only verifies Schnorr signatures. Switch the wallet to a Taproot (P2TR) address.',
      )
    }
    const item = stack[0]
    if (item?.length === 64) return item
    // 65 bytes = 64-byte Schnorr + an explicit sighash flag byte. Observed from a real
    // BIP-322 signer; `adjustSignature` does not cover this length and throws on it.
    if (item?.length === 65) return item.slice(0, 64)
    if (item) {
      try {
        const adjusted = SignatureUtil.adjustSignature(item)
        if (adjusted.length === 64) return adjusted
      } catch {
        // Fall through to the raw-blob path rather than failing here.
      }
    }
  }

  // Fallback: some wallets hand back the bare 64/66/67-byte form.
  const adjusted = SignatureUtil.adjustSignature(raw)
  if (adjusted.length === 64) return adjusted

  throw new Error(`Could not extract a 64-byte signature (got ${raw.length} raw bytes)`)
}

// ---------------------------------------------------------------------------
// Build / sign / send
// ---------------------------------------------------------------------------

function hexToBytes(hex: string): Uint8Array {
  const clean = hex.replace(/^0x/, '')
  const out = new Uint8Array(clean.length / 2)
  for (let i = 0; i < out.length; i++) out[i] = parseInt(clean.substr(i * 2, 2), 16)
  return out
}

/** Compile instructions into the sanitized message the validator will reproduce. */
export async function buildMessage(instructions: SdkInstruction[], payer: Uint8Array) {
  const blockhash = hexToBytes(await getBestBlockHash())
  const message = SanitizedMessageUtil.createSanitizedMessage(
    instructions as any,
    payer,
    blockhash,
  )
  // createSanitizedMessage returns an error STRING rather than throwing.
  if (typeof message === 'string') throw new Error(`Failed to compile message: ${message}`)
  return message
}

/**
 * The challenge is the message hash as a 64-char hex STRING, passed to the wallet
 * verbatim. `hash()` already returns the UTF-8 bytes of that string, so decoding is
 * the correct step; hex-decoding it here would sign the wrong bytes.
 */
export function signingChallenge(message: any): string {
  return new TextDecoder().decode(SanitizedMessageUtil.hash(message))
}

export async function signAndSend(
  instructions: SdkInstruction[],
  payer: Uint8Array,
  sign: SignChallenge,
): Promise<string> {
  const message = await buildMessage(instructions, payer)
  const challenge = signingChallenge(message)

  const walletSig = await sign(challenge)
  const signature = extractSchnorrSignature(walletSig)

  const tx = {
    version: 0,
    signatures: [Array.from(signature)],
    message: {
      header: (message as any).header,
      account_keys: (message as any).account_keys.map((k: Uint8Array) => Array.from(k)),
      recent_blockhash: Array.from((message as any).recent_blockhash),
      instructions: (message as any).instructions.map((ix: any) => ({
        program_id_index: ix.program_id_index,
        accounts: Array.from(ix.accounts),
        data: Array.from(ix.data),
      })),
    },
  }

  return await sendTransaction(tx)
}

/**
 * A fee payer must exist, be system-owned and be rent-exempt. Airdrop and wait,
 * because "airdrop returned a txid" does not mean the validator sees the account yet.
 */
export async function ensureFunded(pubkey: Uint8Array, minLamports = 100_000): Promise<number> {
  try {
    const acct = await readAccount(pubkey)
    if (acct && acct.lamports >= minLamports) return acct.lamports
  } catch {
    // Account does not exist yet; fall through and fund it.
  }

  await requestAirdrop(pubkey)

  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 2000))
    try {
      const acct = await readAccount(pubkey)
      if (acct && acct.lamports >= minLamports) return acct.lamports
    } catch {
      // keep polling
    }
  }
  throw new Error('Funding did not land in time. The testnet faucet may be rate limiting.')
}

/** Poll until the transaction is processed, so the UI never reports a result too early. */
export async function waitProcessed(txid: string, attempts = 40): Promise<any> {
  for (let i = 0; i < attempts; i++) {
    try {
      const p = await getProcessedTransaction(txid)
      if (p && p.status) {
        const status = typeof p.status === 'string' ? p.status : Object.keys(p.status)[0]
        if (status === 'Processed' || status === 'Finalized') return p
        if (status === 'Failed') {
          throw new Error(
            `Transaction failed on-chain: ${JSON.stringify(p.status)}`,
          )
        }
      }
    } catch (e: any) {
      if (String(e.message).includes('failed on-chain')) throw e
    }
    await new Promise((r) => setTimeout(r, 1500))
  }
  throw new Error('Timed out waiting for the transaction to be processed')
}
