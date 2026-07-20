/**
 * Proves the REAL browser flow end to end, using bip322-js as a stand-in for the
 * wallet extension. Every step below is what the browser does; only the literal
 * `wallet.signMessage()` call is replaced by `Signer.sign()`, which implements the
 * same BIP-322 spec and returns the same base64 witness blob.
 */
import { SanitizedMessageUtil, SignatureUtil, PubkeyUtil } from '@arch-network/arch-sdk'
import bip322 from 'bip322-js'
import * as bitcoin from 'bitcoinjs-lib'
import ecpairPkg from 'ecpair'
import ecc from '@bitcoinerlab/secp256k1'

const { Signer } = bip322
bitcoin.initEccLib(ecc)
const ECPair = (ecpairPkg.ECPairFactory || ecpairPkg.default || ecpairPkg)(ecc)

const RPC = 'https://rpc.testnet.arch.network'
const PROGRAM = '8ea69ca483247ded86a152bc809e05caf1f0326c604877f8071947420053c635'
const NET = bitcoin.networks.testnet

let id = 0
async function rpc(method, params) {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }),
  })
  const j = await r.json()
  if (j.error) throw new Error(`${method}: ${JSON.stringify(j.error)}`)
  return j.result
}

const hexToBytes = (h) => Uint8Array.from(Buffer.from(h.replace(/^0x/, ''), 'hex'))
const bytesToHex = (b) => Buffer.from(b).toString('hex')

// ---- the exact witness parser shipped in the app -------------------------

function readCompactSize(bytes, offset) {
  if (offset >= bytes.length) return null
  const first = bytes[offset]
  if (first < 0xfd) return { value: first, nextOffset: offset + 1 }
  if (first === 0xfd) {
    if (offset + 3 > bytes.length) return null
    return { value: bytes[offset + 1] | (bytes[offset + 2] << 8), nextOffset: offset + 3 }
  }
  return null
}

function parseWitnessStack(bytes) {
  const count = readCompactSize(bytes, 0)
  if (!count || count.value <= 0 || count.value > 16) return null
  let offset = count.nextOffset
  const items = []
  for (let i = 0; i < count.value; i++) {
    const size = readCompactSize(bytes, offset)
    if (!size) return null
    offset = size.nextOffset
    if (size.value < 0 || offset + size.value > bytes.length) return null
    items.push(bytes.slice(offset, offset + size.value))
    offset += size.value
  }
  return offset === bytes.length ? items : null
}

function extractSchnorrSignature(b64) {
  const raw = Uint8Array.from(Buffer.from(b64, 'base64'))
  console.log(`   raw blob: ${raw.length} bytes`)
  const stack = parseWitnessStack(raw)
  if (stack) {
    console.log(`   witness stack: ${stack.length} item(s), sizes [${stack.map((i) => i.length)}]`)
    if (stack.length >= 2) throw new Error('non-Taproot account (2-item witness)')
    const item = stack[0]
    if (item?.length === 64) return item
    if (item?.length === 65) { console.log('   (65-byte item: dropping trailing sighash flag)'); return item.slice(0, 64) }
    if (item) {
      try {
        const adj = SignatureUtil.adjustSignature(item)
        if (adj.length === 64) return adj
      } catch {}
    }
  }
  const adj = SignatureUtil.adjustSignature(raw)
  if (adj.length === 64) return adj
  throw new Error(`could not extract 64-byte signature from ${raw.length} bytes`)
}

// ---- game instruction ----------------------------------------------------

function u64le(n) {
  const o = new Uint8Array(8)
  let v = n
  for (let i = 0; i < 8; i++) {
    o[i] = Number(v & 0xffn)
    v >>= 8n
  }
  return o
}

const HOUSE_TREASURY = Uint8Array.from(
  Buffer.from('ab82155fd2c666ef5c66260bf0b9728fb423a40234514eceb65290422f005923', 'hex'),
)
const enc = new TextEncoder()
const programId = PubkeyUtil.fromHex(PROGRAM)

const pda = (seeds) => PubkeyUtil.findProgramAddress(seeds, programId)[0]

function openSessionIx(playerHex, sessionId, wager) {
  const player = PubkeyUtil.fromHex(playerHex)
  const session = pda([enc.encode('session'), player, u64le(sessionId)])
  const vault = pda([enc.encode('vault'), session])
  const data = new Uint8Array(17)
  data[0] = 1
  data.set(u64le(sessionId), 1)
  data.set(u64le(wager), 9)
  return {
    instruction: {
      program_id: programId,
      accounts: [
        { pubkey: player, is_signer: true, is_writable: true },
        { pubkey: pda([enc.encode('config')]), is_signer: false, is_writable: false },
        { pubkey: session, is_signer: false, is_writable: true },
        { pubkey: vault, is_signer: false, is_writable: true },
        { pubkey: new Uint8Array(32), is_signer: false, is_writable: false },
        { pubkey: HOUSE_TREASURY, is_signer: false, is_writable: false },
      ],
      data,
    },
    session,
    vault,
  }
}

// ---- run -----------------------------------------------------------------

console.log('1. create a Taproot identity (stands in for the wallet account)')
const kp = ECPair.makeRandom({ network: NET })
const wif = kp.toWIF()
const xonly = Buffer.from(kp.publicKey).subarray(1, 33) // drop 02/03 prefix
const { address } = bitcoin.payments.p2tr({ internalPubkey: xonly, network: NET })
const archPubkeyHex = bytesToHex(xonly)
console.log('   taproot address :', address)
console.log('   arch pubkey     :', archPubkeyHex.slice(0, 20) + '…')

console.log('2. fund the Arch account (request_airdrop, no signature needed)')
const pkBytes = hexToBytes(archPubkeyHex)
await rpc('request_airdrop', Array.from(pkBytes))
let funded = 0
for (let i = 0; i < 30; i++) {
  await new Promise((r) => setTimeout(r, 2000))
  try {
    const a = await rpc('read_account_info', Array.from(pkBytes))
    if (a?.lamports > 0) { funded = a.lamports; break }
  } catch {}
}
if (!funded) throw new Error('funding never landed')
console.log('   funded          :', funded, 'lamports')

console.log('3. build OpenSession instruction in "browser" code')
const sessionId = BigInt(Math.floor(Math.random() * 2 ** 40))
const wager = 10000n
const { instruction, session, vault } = openSessionIx(archPubkeyHex, sessionId, wager)
console.log('   session id      :', sessionId.toString())
console.log('   session pda     :', bytesToHex(session).slice(0, 20) + '…')

console.log('4. compile sanitized message')
const blockhash = hexToBytes(await rpc('get_best_finalized_block_hash', []))
const message = SanitizedMessageUtil.createSanitizedMessage([instruction], pkBytes, blockhash)
if (typeof message === 'string') throw new Error(`compile failed: ${message}`)
console.log('   signers required:', message.header.num_required_signatures)
console.log('   account keys    :', message.account_keys.length)

console.log('5. derive the signing challenge')
const challenge = new TextDecoder().decode(SanitizedMessageUtil.hash(message))
console.log('   challenge       :', challenge)
console.log('   is 64-char hex  :', /^[0-9a-f]{64}$/.test(challenge))

console.log('6. sign it (bip322-js standing in for the wallet)')
const walletSig = Signer.sign(wif, address, challenge)
const b64 = typeof walletSig === 'string' ? walletSig : walletSig.toString('base64')
console.log('   wallet returned :', b64.slice(0, 32) + '…')

console.log('7. extract the 64-byte Schnorr signature')
const sig = extractSchnorrSignature(b64)
console.log('   extracted       :', sig.length, 'bytes')

console.log('8. submit to Arch')
const tx = {
  version: 0,
  signatures: [Array.from(sig)],
  message: {
    header: message.header,
    account_keys: message.account_keys.map((k) => Array.from(k)),
    recent_blockhash: Array.from(message.recent_blockhash),
    instructions: message.instructions.map((ix) => ({
      program_id_index: ix.program_id_index,
      accounts: Array.from(ix.accounts),
      data: Array.from(ix.data),
    })),
  },
}
const txid = await rpc('send_transaction', tx)
console.log('   txid            :', txid)

console.log('9. confirm on-chain')
let processed = null
for (let i = 0; i < 40; i++) {
  await new Promise((r) => setTimeout(r, 1500))
  try {
    const p = await rpc('get_processed_transaction', txid)
    if (p?.status) { processed = p; break }
  } catch {}
}
console.log('   status          :', JSON.stringify(processed?.status))

const vaultAcct = await rpc('read_account_info', Array.from(vault))
const sessionAcct = await rpc('read_account_info', Array.from(session))
console.log('   vault holds     :', vaultAcct.lamports, 'lamports')
console.log('   session data    :', sessionAcct.data.length, 'bytes')
const player32 = Buffer.from(sessionAcct.data.slice(0, 32)).toString('hex')
console.log('   session.player  :', player32.slice(0, 20) + '…')
console.log('   matches my key  :', player32 === archPubkeyHex)
console.log('   session.status  :', sessionAcct.data[56], '(0=Open)')

console.log('\nRESULT: wallet-signed OpenSession landed on Arch testnet.')
