/**
 * The OpenSession instruction, built client-side.
 *
 * This must match the Rust program byte for byte:
 *
 *   pub enum EscrowInstruction {
 *       InitializeConfig { min_wager: u64, max_wager: u64 },   // variant 0
 *       OpenSession { session_id: u64, wager: u64 },           // variant 1
 *       SettleSession { player_won: bool },                    // variant 2
 *   }
 *
 * Borsh encodes an enum as a single u8 discriminant followed by the variant's fields
 * in declaration order, each little-endian. So OpenSession is 17 bytes:
 * [1][session_id u64 LE][wager u64 LE].
 *
 * Account order matches `open_session` in the program:
 *   0 player   (signer, writable)
 *   1 config   (readonly)
 *   2 session  (writable)
 *   3 vault    (writable)
 *   4 system program (readonly)
 */
import { PubkeyUtil } from '@arch-network/arch-sdk'
import { PROGRAM_ID_HEX, configPda, sessionPda, vaultPda } from './arch'
import type { SdkInstruction } from './archTx'

const SYSTEM_PROGRAM = new Uint8Array(32) // all zeroes

function u64le(n: bigint): Uint8Array {
  const out = new Uint8Array(8)
  let v = n
  for (let i = 0; i < 8; i++) {
    out[i] = Number(v & 0xffn)
    v >>= 8n
  }
  return out
}

export function openSessionInstruction(
  playerHex: string,
  sessionId: bigint,
  wager: bigint,
): SdkInstruction {
  const program = PubkeyUtil.fromHex(PROGRAM_ID_HEX)
  const player = PubkeyUtil.fromHex(playerHex)
  const session = sessionPda(playerHex, sessionId)
  const vault = vaultPda(session)

  const data = new Uint8Array(17)
  data[0] = 1 // OpenSession discriminant
  data.set(u64le(sessionId), 1)
  data.set(u64le(wager), 9)

  return {
    program_id: program,
    accounts: [
      { pubkey: player, is_signer: true, is_writable: true },
      { pubkey: configPda(), is_signer: false, is_writable: false },
      { pubkey: session, is_signer: false, is_writable: true },
      { pubkey: vault, is_signer: false, is_writable: true },
      { pubkey: SYSTEM_PROGRAM, is_signer: false, is_writable: false },
    ],
    data,
  }
}

/**
 * Session ids must be unique per player, because the session PDA is seeded with one.
 * Reusing an id makes account creation fail. Random 48 bits is plenty and avoids
 * needing any stored counter.
 */
export function newSessionId(): bigint {
  const buf = new Uint8Array(8)
  crypto.getRandomValues(buf)
  buf[6] = 0
  buf[7] = 0
  let v = 0n
  for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(buf[i])
  return v
}
