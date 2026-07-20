/**
 * Arch chain helpers for the browser.
 *
 * Reads go straight to the Arch RPC so the UI shows real on-chain state.
 * Writes go through the backend, because settlement needs the house authority key.
 */
import { PubkeyUtil, RpcConnection } from '@arch-network/arch-sdk'

export const BACKEND_URL =
  import.meta.env.VITE_BACKEND_URL ?? 'http://localhost:8091'
export const ARCH_RPC_URL =
  import.meta.env.VITE_ARCH_RPC_URL ?? 'https://rpc.testnet.arch.network'
export const PROGRAM_ID_HEX =
  import.meta.env.VITE_PROGRAM_ID ??
  'e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4'

export const rpc = new RpcConnection(ARCH_RPC_URL)
const programId = PubkeyUtil.fromHex(PROGRAM_ID_HEX)
const enc = new TextEncoder()

/** u64 -> 8 little-endian bytes, matching Rust's `to_le_bytes()`. */
function u64le(n: bigint): Uint8Array {
  const out = new Uint8Array(8)
  let v = n
  for (let i = 0; i < 8; i++) {
    out[i] = Number(v & 0xffn)
    v >>= 8n
  }
  return out
}

export function hexOf(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/**
 * PDA derivations — these mirror the program's seeds exactly.
 * Verified against the Rust implementation: the JS-derived config PDA reads the
 * same on-chain account the Rust program wrote.
 */
export function configPda(): Uint8Array {
  return PubkeyUtil.findProgramAddress([enc.encode('config')], programId)[0]
}

export function sessionPda(playerHex: string, sessionId: bigint): Uint8Array {
  return PubkeyUtil.findProgramAddress(
    [enc.encode('session'), PubkeyUtil.fromHex(playerHex), u64le(sessionId)],
    programId,
  )[0]
}

export function vaultPda(sessionPdaBytes: Uint8Array): Uint8Array {
  return PubkeyUtil.findProgramAddress(
    [enc.encode('vault'), sessionPdaBytes],
    programId,
  )[0]
}

/** Lamport balance of an account, or 0 if it does not exist yet. */
export async function getBalance(pubkeyHex: string): Promise<number> {
  try {
    const info = await rpc.readAccountInfo(PubkeyUtil.fromHex(pubkeyHex))
    return Number((info as any).lamports ?? 0)
  } catch {
    return 0
  }
}

/** Decodes the on-chain GameSession struct (59 bytes, Borsh, all fixed-width). */
export type SessionState = {
  player: string
  wager: number
  sessionId: bigint
  status: number // 0 Open, 1 Won, 2 Lost
}

export async function readSession(
  playerHex: string,
  sessionId: bigint,
): Promise<SessionState | null> {
  try {
    const pda = sessionPda(playerHex, sessionId)
    const info: any = await rpc.readAccountInfo(pda)
    const data: Uint8Array = Uint8Array.from(info.data)
    if (data.length < 59) return null
    const view = new DataView(data.buffer, data.byteOffset, data.byteLength)
    return {
      player: hexOf(data.slice(0, 32)),
      wager: Number(view.getBigUint64(32, true)),
      sessionId: view.getBigUint64(40, true),
      status: data[56],
    }
  } catch {
    return null
  }
}

export const statusLabel = (s: number) =>
  s === 0 ? 'Open' : s === 1 ? 'Won' : s === 2 ? 'Lost' : 'Unknown'

// ---------------------------------------------------------------------------
// Backend API
// ---------------------------------------------------------------------------

export type DemoOpen = {
  player: string
  session_id: number
  wager: number
  escrowed: number
  balance: number
}

export type SettleResult = {
  player_won: boolean
  status: string
  session_id: number
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(`${BACKEND_URL}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  const json = await res.json()
  if (!res.ok) throw new Error(json.error ?? `HTTP ${res.status}`)
  return json as T
}

export const openDemoSession = (wager: number) =>
  post<DemoOpen>('/demo/open', { wager })

export const settleSession = (player: string, sessionId: number) =>
  post<SettleResult>('/settle', { player, session_id: sessionId })

export async function health() {
  const res = await fetch(`${BACKEND_URL}/health`)
  if (!res.ok) throw new Error(`backend unreachable (HTTP ${res.status})`)
  return res.json()
}

// ---------------------------------------------------------------------------
// Bitcoin wallet detection
// ---------------------------------------------------------------------------
//
// Arch signs with secp256k1 / BIP-322 using a Bitcoin Taproot wallet — NOT ed25519,
// so Phantom/Solflare do not apply and there is no wallet-adapter package.
// There is no official Arch wallet adapter, so this talks to injected providers directly.

export type WalletKind = 'unisat' | 'xverse'

export function detectWallets(): WalletKind[] {
  const found: WalletKind[] = []
  const w = window as any
  if (w.unisat) found.push('unisat')
  if (w.XverseProviders?.BitcoinProvider || w.BitcoinProvider) found.push('xverse')
  return found
}

export async function connectWallet(kind: WalletKind): Promise<string> {
  const w = window as any
  if (kind === 'unisat') {
    const accounts: string[] = await w.unisat.requestAccounts()
    if (!accounts?.length) throw new Error('no accounts returned')
    return accounts[0]
  }
  // Xverse exposes a request-based provider.
  const provider = w.XverseProviders?.BitcoinProvider ?? w.BitcoinProvider
  if (!provider) throw new Error('Xverse provider not found')
  const res = await provider.request('getAccounts', {
    purposes: ['ordinals', 'payment'],
  })
  const addr = res?.result?.[0]?.address
  if (!addr) throw new Error('no accounts returned')
  return addr
}
