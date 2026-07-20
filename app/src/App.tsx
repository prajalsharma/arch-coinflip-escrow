import { useEffect, useState } from 'react'
import {
  ARCH_RPC_URL,
  DEMO_ENABLED,
  readHouseTreasury,
  BACKEND_URL,
  PROGRAM_ID_HEX,
  getBalance,
  health,
  openDemoSession,
  readSession,
  sessionPda,
  settleSession,
  vaultPda,
  hexOf,
} from './arch'
import { ensureFunded, signAndSend, waitProcessed } from './archTx'
import { newSessionId, openSessionInstruction } from './game'
import {
  connect,
  detectWallets,
  signerFor,
  walletLabel,
  type WalletAccount,
  type WalletKind,
} from './wallet'
import { PubkeyUtil } from '@arch-network/arch-sdk'

const WAGER = 10_000n

type Phase = 'idle' | 'funding' | 'faucet' | 'signing' | 'opening' | 'flipping' | 'settled'
type Mode = 'wallet' | 'demo'

type HistoryRow = { sessionId: string; player: string; won: boolean }

const short = (s: string) => `${s.slice(0, 6)}…${s.slice(-4)}`
const fmt = (n: number | bigint) => Number(n).toLocaleString('en-US')

export default function App() {
  const [phase, setPhase] = useState<Phase>('idle')
  const [error, setError] = useState<string | null>(null)
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [blockHeight, setBlockHeight] = useState<number | null>(null)

  const [programMismatch, setProgramMismatch] = useState<string | null>(null)
  const [wallets, setWallets] = useState<WalletKind[]>([])
  const [account, setAccount] = useState<WalletAccount | null>(null)
  const [connecting, setConnecting] = useState(false)

  const [player, setPlayer] = useState<string | null>(null)
  const [sessionId, setSessionId] = useState<string | null>(null)
  const [balance, setBalance] = useState<number | null>(null)
  const [escrowed, setEscrowed] = useState<number>(0)
  const [onChainStatus, setOnChainStatus] = useState<number | null>(null)
  const [result, setResult] = useState<boolean | null>(null)
  const [mode, setMode] = useState<Mode | null>(null)
  const [history, setHistory] = useState<HistoryRow[]>([])

  useEffect(() => {
    health()
      .then((h) => {
        setBackendOk(true)
        setBlockHeight(h.block_height ?? null)
        // The backend derives session PDAs from ITS program id. If it differs from
        // ours, every session we open is invisible to it and settlement fails with
        // "session not found on-chain". Catch it up front rather than mid-round.
        const theirs = String(h.program_id ?? '').toLowerCase()
        if (theirs && theirs !== PROGRAM_ID_HEX.toLowerCase()) {
          setProgramMismatch(theirs)
        }
      })
      .catch(() => setBackendOk(false))
    setWallets(detectWallets())
  }, [])

  async function refreshChain(p: string, sid: bigint) {
    try {
      const [bal, sess] = await Promise.all([getBalance(p), readSession(p, sid)])
      setBalance(bal)
      if (sess) setOnChainStatus(sess.status)
      setEscrowed(await getBalance(hexOf(vaultPda(sessionPda(p, sid)))))
    } catch {
      // A failed read must not discard a settlement that already landed.
    }
  }

  async function handleConnect(kind: WalletKind) {
    setError(null)
    setConnecting(true)
    try {
      setAccount(await connect(kind))
    } catch (e: any) {
      setError(e.message ?? String(e))
    } finally {
      setConnecting(false)
    }
  }

  /** The real flow: the user's own key signs, and their own lamports are escrowed. */
  async function playWithWallet() {
    if (!account) return
    setError(null)
    setResult(null)
    setOnChainStatus(null)
    setMode('wallet')

    const pubkeyBytes = PubkeyUtil.fromHex(account.pubkeyHex)
    const sid = newSessionId()

    try {
      // A fee payer must exist, be system-owned and be rent-exempt.
      setPhase('funding')
      await ensureFunded(pubkeyBytes, 100_000, () => setPhase('faucet'))

      setPhase('signing')
      const treasury = await readHouseTreasury()
      const ix = openSessionInstruction(account.pubkeyHex, sid, WAGER, treasury)
      const txid = await signAndSend([ix], pubkeyBytes, signerFor(account.kind))

      setPhase('opening')
      await waitProcessed(txid)

      setPlayer(account.pubkeyHex)
      setSessionId(sid.toString())
      setOnChainStatus(0)
      await refreshChain(account.pubkeyHex, sid)

      // Only settlement goes through the backend, because it needs the house key.
      setPhase('flipping')
      const settled = await settleSession(account.pubkeyHex, Number(sid))
      await refreshChain(account.pubkeyHex, sid)

      setResult(settled.player_won)
      setPhase('settled')
      setHistory((h) =>
        [{ sessionId: sid.toString(), player: account.pubkeyHex, won: settled.player_won }, ...h].slice(0, 6),
      )
    } catch (e: any) {
      setError(e.message ?? String(e))
      setPhase('idle')
    }
  }

  /** Fallback so the program stays demonstrable with no wallet installed. */
  async function playDemo() {
    setError(null)
    setResult(null)
    setOnChainStatus(null)
    setMode('demo')
    setPhase('opening')

    try {
      const opened = await openDemoSession(Number(WAGER))
      const sid = BigInt(opened.session_id)
      setPlayer(opened.player)
      setSessionId(opened.session_id.toString())
      setBalance(opened.balance)
      setEscrowed(opened.escrowed)
      setOnChainStatus(0)

      setPhase('flipping')
      const settled = await settleSession(opened.player, opened.session_id)
      await refreshChain(opened.player, sid)

      setResult(settled.player_won)
      setPhase('settled')
      setHistory((h) =>
        [{ sessionId: opened.session_id.toString(), player: opened.player, won: settled.player_won }, ...h].slice(0, 6),
      )
    } catch (e: any) {
      setError(e.message ?? String(e))
      setPhase('idle')
    }
  }

  const busy = phase !== 'idle' && phase !== 'settled'
  const stepLabel: Record<Phase, string> = {
    idle: '',
    funding: 'Checking your Arch account…',
    faucet: 'Funding your account from the faucet, this can take up to a minute…',
    signing: 'Waiting for your wallet signature…',
    opening: 'Locking your bet in escrow…',
    flipping: 'Flipping…',
    settled: '',
  }

  return (
    <div className="page">
      <header className="header">
        <div className="brand">
          <h1>Coin Flip</h1>
          <p className="sub">Bitcoin escrow on Arch</p>
        </div>
        <div className="net">
          <span className={`dot ${backendOk ? 'ok' : backendOk === false ? 'bad' : ''}`} />
          <span className="netname">Testnet</span>
          {blockHeight && <span className="netblock">{fmt(blockHeight)}</span>}
        </div>
      </header>

      {backendOk === false && (
        <div className="alert" role="alert">
          <strong>Can’t reach the settlement service</strong>
          <span>
            Tried <code>{BACKEND_URL}</code>. Check it is running and that{' '}
            <code>ALLOWED_ORIGIN</code> matches{' '}
            <code>{typeof window !== 'undefined' ? window.location.origin : ''}</code>.
          </span>
        </div>
      )}

      {programMismatch && (
        <div className="alert" role="alert">
          <strong>Frontend and backend are on different programs</strong>
          <span>
            This app uses <code>{PROGRAM_ID_HEX.slice(0, 12)}…</code> but the settlement
            service uses <code>{programMismatch.slice(0, 12)}…</code>. Sessions opened here
            are invisible to it, so settlement fails with “session not found on-chain”.
            Set both <code>VITE_PROGRAM_ID</code> and the backend’s <code>PROGRAM_ID</code>
            to the same value, then redeploy both.
          </span>
        </div>
      )}

      <section className="identity">
        {account ? (
          <>
            <div className="idrow">
              <span className="idlabel">{walletLabel(account.kind)}</span>
              <code className="idaddr">{short(account.address)}</code>
            </div>
            <div className="idrow">
              <span className="idlabel">Arch key</span>
              <code className="idaddr">{short(account.pubkeyHex)}</code>
            </div>
            <p className="idnote">
              Your Taproot key signs the bet. Your own balance is escrowed on-chain.
            </p>
          </>
        ) : wallets.length > 0 ? (
          <>
            <p className="idprompt">Connect a Taproot Bitcoin wallet to bet with your own key.</p>
            <div className="idbuttons">
              {wallets.map((w) => (
                <button
                  key={w}
                  className="btn"
                  onClick={() => handleConnect(w)}
                  disabled={connecting}
                >
                  {connecting ? 'Connecting…' : `Connect ${walletLabel(w)}`}
                </button>
              ))}
            </div>
          </>
        ) : (
          <p className="idprompt">
            No Bitcoin wallet found. Arch signs with secp256k1 BIP-322, so it needs a
            Taproot wallet such as Unisat or Xverse. You can still try the demo below.
          </p>
        )}
      </section>

      <main className="table">
        <div className="odds">
          <div className="odd">
            <span className="oddlabel">Your bet</span>
            <span className="oddvalue">{fmt(WAGER)}</span>
          </div>
          <span className="oddsep" aria-hidden="true" />
          <div className="odd">
            <span className="oddlabel">You win</span>
            <span className="oddvalue win">{fmt(WAGER * 2n)}</span>
          </div>
        </div>
        <p className="unitnote">test sats on Arch testnet</p>

        <button
          className="flip"
          onClick={account ? playWithWallet : playDemo}
          disabled={busy || backendOk === false || programMismatch !== null || (!account && !DEMO_ENABLED)}
        >
          {busy
            ? stepLabel[phase]
            : account
              ? result !== null
                ? 'Flip again'
                : 'Sign and flip'
              : DEMO_ENABLED
                ? result !== null
                  ? 'Flip again (demo)'
                  : 'Try the demo'
                : 'Connect a wallet to play'}
        </button>

        {account && DEMO_ENABLED && !busy && (
          <button className="ghostlink" onClick={playDemo} disabled={busy}>
            or run a demo round without signing
          </button>
        )}

        <div className="stage" aria-live="polite">
          {busy && (
            <ol className="progress">
              {mode === 'wallet' && (
                <>
                  <li
                    className={
                      phase === 'funding' || phase === 'faucet' ? 'on' : 'done'
                    }
                  >
                    {phase === 'faucet' ? 'Funding from faucet (first play only)' : 'Arch account ready'}
                  </li>
                  <li
                    className={
                      phase === 'signing'
                        ? 'on'
                        : phase === 'funding' || phase === 'faucet'
                          ? ''
                          : 'done'
                    }
                  >
                    Signed by your wallet
                  </li>
                </>
              )}
              <li
                className={
                  phase === 'opening' ? 'on' : phase === 'flipping' ? 'done' : ''
                }
              >
                Bet locked in escrow
              </li>
              <li className={phase === 'flipping' ? 'on' : ''}>Result settled on-chain</li>
            </ol>
          )}

          {!busy && result !== null && (
            <div className={`verdict ${result ? 'won' : 'lost'}`}>
              <span className="verdicttitle">{result ? 'You won' : 'You lost'}</span>
              <span className="verdictsub">
                {result
                  ? `${fmt(WAGER * 2n)} test sats released from escrow`
                  : `${fmt(WAGER)} test sats went to the house`}
                {mode === 'demo' && ' · demo round'}
              </span>
            </div>
          )}

          {!busy && result === null && !error && (
            <p className="hint">
              {account
                ? 'Your wallet signs the bet, your key escrows it on-chain, then the house settles the outcome.'
                : DEMO_ENABLED
                  ? 'Demo mode uses a server-held throwaway key, not yours. Connect a wallet to bet with your own key.'
                  : 'Every bet is signed by the player’s own Taproot key, so a wallet is required.'}
            </p>
          )}

          {error && <p className="errline">{error}</p>}
        </div>
      </main>

      {player && sessionId !== null && (
        <section className="detail">
          <div className="detailhead">
            <h2>On-chain</h2>
            <span className="src">{mode === 'wallet' ? 'your key' : 'demo key'}</span>
          </div>
          <dl className="facts">
            <div>
              <dt>Round</dt>
              <dd>#{sessionId}</dd>
            </div>
            <div>
              <dt>Key</dt>
              <dd>{short(player)}</dd>
            </div>
            <div>
              <dt>In escrow</dt>
              <dd>{fmt(escrowed)}</dd>
            </div>
            <div>
              <dt>Balance</dt>
              <dd>{balance !== null ? fmt(balance) : '···'}</dd>
            </div>
          </dl>
          {onChainStatus !== null && (
            <div className={`chainstatus s${onChainStatus}`}>
              {onChainStatus === 0
                ? 'Escrow open'
                : onChainStatus === 1
                  ? 'Settled: player won'
                  : 'Settled: house won'}
            </div>
          )}
        </section>
      )}

      {history.length > 0 && (
        <section className="detail">
          <div className="detailhead">
            <h2>Recent rounds</h2>
            <span className="src">
              {history.filter((h) => h.won).length}W / {history.filter((h) => !h.won).length}L
            </span>
          </div>
          <ul className="rounds">
            {history.map((h) => (
              <li key={h.sessionId}>
                {/* The round id is what differs between rows. In wallet mode the player
                    key is the same every round by definition, so showing it here just
                    repeated one string down the list. */}
                <span className="rkey">#{h.sessionId}</span>
                <span className={`rdelta ${h.won ? 'won' : 'lost'}`}>
                  {h.won ? `+${fmt(WAGER)}` : `-${fmt(WAGER)}`}
                </span>
                <span className={`rres ${h.won ? 'won' : 'lost'}`}>
                  {h.won ? 'Won' : 'Lost'}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <footer className="footer">
        <p>
          The house decides each outcome, because Arch has no on-chain randomness
          primitive. This is a testnet demo, not a trustless game.
        </p>
        <p className="meta">
          <span>Program {short(PROGRAM_ID_HEX)}</span>
          <span>{ARCH_RPC_URL.replace('https://', '')}</span>
        </p>
      </footer>
    </div>
  )
}
