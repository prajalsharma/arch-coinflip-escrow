/**
 * Bitcoin wallet adapters for Arch.
 *
 * Arch verifies BIP-322 Schnorr signatures made with a secp256k1 Taproot key, not
 * ed25519, so Phantom and the Solana wallet adapters do not apply and no official
 * Arch wallet adapter exists. This talks to the injected providers directly.
 *
 * A user's Arch pubkey is their Taproot INTERNAL x-only public key: take the wallet's
 * 33-byte compressed key for the ordinals/Taproot address and drop the 02/03 prefix.
 * There is no tweak step.
 */

export type WalletKind = 'unisat' | 'xverse'

export type WalletAccount = {
  kind: WalletKind
  /** Taproot address (tb1p... on testnet). */
  address: string
  /** 64-char x-only hex. This IS the Arch pubkey. */
  pubkeyHex: string
}

/** Signs a 64-char hex challenge, returning the wallet's base64 BIP-322 blob. */
export type SignChallenge = (challenge: string) => Promise<string>

export function detectWallets(): WalletKind[] {
  const w = window as any
  const found: WalletKind[] = []
  if (w.unisat) found.push('unisat')
  if (w.XverseProviders?.BitcoinProvider || w.BitcoinProvider) found.push('xverse')
  return found
}

/**
 * Arch pubkeys are 32-byte x-only. Wallets hand back 33-byte compressed keys.
 * Dropping the parity prefix is the whole conversion.
 */
export function toXOnlyPubkey(pubkeyHex: string): string {
  const hex = pubkeyHex.trim().toLowerCase().replace(/^0x/, '')
  if (hex.length === 66) return hex.slice(2)
  if (hex.length === 64) return hex
  throw new Error(`Unexpected public key length: ${hex.length} (want 64 or 66 hex chars)`)
}

/** Taproot addresses start bc1p (mainnet) or tb1p (testnet/signet/testnet4). */
function isTaproot(address: string): boolean {
  return address.startsWith('bc1p') || address.startsWith('tb1p')
}

// ---------------------------------------------------------------------------
// Unisat
// ---------------------------------------------------------------------------

async function connectUnisat(): Promise<WalletAccount> {
  const w = window as any
  const accounts: string[] = await w.unisat.requestAccounts()
  if (!accounts?.length) throw new Error('Unisat returned no accounts')

  const address = accounts[0]
  if (!isTaproot(address)) {
    throw new Error(
      `Arch needs a Taproot account. Unisat is on ${address.slice(0, 4)}…; ` +
        'switch its address type to Taproot (P2TR) and reconnect.',
    )
  }

  const pubkey: string = await w.unisat.getPublicKey()
  return { kind: 'unisat', address, pubkeyHex: toXOnlyPubkey(pubkey) }
}

const signUnisat: SignChallenge = async (challenge) => {
  const w = window as any
  // 'bip322-simple' is what produces the Schnorr signature Arch verifies.
  return await w.unisat.signMessage(challenge, 'bip322-simple')
}

// ---------------------------------------------------------------------------
// Xverse
// ---------------------------------------------------------------------------

function xverseProvider(): any {
  const w = window as any
  const p = w.XverseProviders?.BitcoinProvider ?? w.BitcoinProvider
  if (!p) throw new Error('Xverse provider not found')
  return p
}

async function connectXverse(): Promise<WalletAccount> {
  const provider = xverseProvider()
  const res = await provider.request('getAccounts', {
    purposes: ['ordinals', 'payment'],
    message: 'Connect to Arch Coin Flip',
  })

  const list = res?.result ?? res?.addresses ?? []
  // The ordinals account is the Taproot one, and its key is the internal pubkey.
  const acct =
    list.find((a: any) => a.purpose === 'ordinals') ??
    list.find((a: any) => isTaproot(a.address ?? ''))

  if (!acct?.address) throw new Error('Xverse returned no Taproot account')
  if (!acct.publicKey) throw new Error('Xverse did not return a public key')

  return {
    kind: 'xverse',
    address: acct.address,
    pubkeyHex: toXOnlyPubkey(acct.publicKey),
  }
}

const signXverse: SignChallenge = async (challenge) => {
  const provider = xverseProvider()
  const account = currentAccount
  if (!account) throw new Error('Wallet not connected')

  const res = await provider.request('signMessage', {
    address: account.address,
    message: challenge,
    protocol: 'BIP322',
  })

  if (res?.status === 'error') {
    throw new Error(res.error?.message ?? 'Xverse refused to sign')
  }
  const sig =
    res?.status === 'success' ? res.result?.signature : (res?.result?.signature ?? res?.signature)
  if (!sig) throw new Error('Xverse did not return a signature')
  return sig
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

let currentAccount: WalletAccount | null = null

export async function connect(kind: WalletKind): Promise<WalletAccount> {
  currentAccount = kind === 'unisat' ? await connectUnisat() : await connectXverse()
  return currentAccount
}

export function signerFor(kind: WalletKind): SignChallenge {
  return kind === 'unisat' ? signUnisat : signXverse
}

export function walletLabel(kind: WalletKind): string {
  return kind === 'unisat' ? 'Unisat' : 'Xverse'
}
