import { useEffect, useState } from 'react'
import {
  ARCH_RPC_URL,
  BACKEND_URL,
  PROGRAM_ID_HEX,
  connectWallet,
  detectWallets,
  getBalance,
  health,
  openDemoSession,
  readSession,
  sessionPda,
  settleSession,
  vaultPda,
  hexOf,
  type WalletKind,
} from './arch'

const WAGER = 10_000

type Phase = 'idle' | 'opening' | 'flipping' | 'settled'

type HistoryRow = {
  sessionId: number
  player: string
  won: boolean
}

const short = (s: string) => `${s.slice(0, 6)}…${s.slice(-4)}`
const fmt = (n: number) => n.toLocaleString('en-US')

export default function App() {
  const [phase, setPhase] = useState<Phase>('idle')
  const [error, setError] = useState<string | null>(null)
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [blockHeight, setBlockHeight] = useState<number | null>(null)

  const [player, setPlayer] = useState<string | null>(null)
  const [sessionId, setSessionId] = useState<number | null>(null)
  const [balance, setBalance] = useState<number | null>(null)
  const [escrowed, setEscrowed] = useState<number>(0)
  const [onChainStatus, setOnChainStatus] = useState<number | null>(null)
  const [result, setResult] = useState<boolean | null>(null)
  const [history, setHistory] = useState<HistoryRow[]>([])

  const [wallets, setWallets] = useState<WalletKind[]>([])
  const [walletAddr, setWalletAddr] = useState<string | null>(null)

  useEffect(() => {
    health()
      .then((h) => {
        setBackendOk(true)
        setBlockHeight(h.block_height ?? null)
      })
      .catch(() => setBackendOk(false))
    setWallets(detectWallets())
  }, [])

  /**
   * Re-read state from Arch RPC. Never throws: by the time this runs the
   * settlement has already landed on-chain, so a failed read must not be
   * allowed to discard a real result.
   */
  async function refreshChain(p: string, sid: number) {
    try {
      const [bal, sess] = await Promise.all([
        getBalance(p),
        readSession(p, BigInt(sid)),
      ])
      setBalance(bal)
      if (sess) setOnChainStatus(sess.status)
      const spda = sessionPda(p, BigInt(sid))
      setEscrowed(await getBalance(hexOf(vaultPda(spda))))
    } catch {
      // Leave the last known values in place rather than blanking the panel.
    }
  }

  async function handlePlay() {
    setError(null)
    setResult(null)
    setOnChainStatus(null)
    setPhase('opening')

    try {
      const opened = await openDemoSession(WAGER)
      setPlayer(opened.player)
      setSessionId(opened.session_id)
      setBalance(opened.balance)
      setEscrowed(opened.escrowed)
      setOnChainStatus(0)

      setPhase('flipping')
      const settled = await settleSession(opened.player, opened.session_id)

      // Refresh chain state before revealing the result, so the panel never
      // shows "Open" next to "You lost".
      await refreshChain(opened.player, opened.session_id)

      setResult(settled.player_won)
      setPhase('settled')
      setHistory((h) =>
        [
          { sessionId: opened.session_id, player: opened.player, won: settled.player_won },
          ...h,
        ].slice(0, 6),
      )
    } catch (e: any) {
      setError(e.message ?? String(e))
      setPhase('idle')
    }
  }

  async function handleConnect(kind: WalletKind) {
    setError(null)
    try {
      setWalletAddr(await connectWallet(kind))
    } catch (e: any) {
      setError(`Wallet connect failed: ${e.message ?? e}`)
    }
  }

  const busy = phase === 'opening' || phase === 'flipping'
  const disabled = busy || backendOk === false

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
            Tried <code>{BACKEND_URL}</code>. Check that it is running, that{' '}
            <code>VITE_BACKEND_URL</code> is correct, and that its{' '}
            <code>ALLOWED_ORIGIN</code> matches{' '}
            <code>{typeof window !== 'undefined' ? window.location.origin : ''}</code>.
          </span>
        </div>
      )}

      <main className="table">
        <div className="odds">
          <div className="odd">
            <span className="oddlabel">Your bet</span>
            <span className="oddvalue">{fmt(WAGER)}</span>
          </div>
          <span className="oddsep" aria-hidden="true" />
          <div className="odd">
            <span className="oddlabel">You win</span>
            <span className="oddvalue win">{fmt(WAGER * 2)}</span>
          </div>
        </div>
        <p className="unitnote">test sats, funded from the Arch faucet</p>

        <button className="flip" onClick={handlePlay} disabled={disabled}>
          {phase === 'opening'
            ? 'Placing your bet…'
            : phase === 'flipping'
              ? 'Flipping…'
              : result !== null
                ? 'Flip again'
                : 'Flip the coin'}
        </button>

        <div className="stage" aria-live="polite">
          {busy && (
            <ol className="progress">
              <li className={phase === 'opening' ? 'on' : 'done'}>Bet locked in escrow</li>
              <li className={phase === 'flipping' ? 'on' : ''}>Result settled on-chain</li>
            </ol>
          )}

          {!busy && result !== null && (
            <div className={`verdict ${result ? 'won' : 'lost'}`}>
              <span className="verdicttitle">{result ? 'You won' : 'You lost'}</span>
              <span className="verdictsub">
                {result
                  ? `${fmt(WAGER * 2)} test sats released from escrow`
                  : `${fmt(WAGER)} test sats went to the house`}
              </span>
            </div>
          )}

          {!busy && result === null && !error && (
            <p className="hint">
              Each round creates a fresh testnet key, locks your bet in an on-chain
              escrow, then settles the outcome. No wallet needed, no real funds.
            </p>
          )}

          {error && <p className="errline">{error}</p>}
        </div>
      </main>

      {player && sessionId !== null && (
        <section className="detail">
          <div className="detailhead">
            <h2>On-chain</h2>
            <span className="src">read from Arch RPC</span>
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
                <span className="rkey">{short(h.player)}</span>
                <span className={`rres ${h.won ? 'won' : 'lost'}`}>
                  {h.won ? 'Won' : 'Lost'}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <footer className="footer">
        {wallets.length > 0 &&
          (walletAddr ? (
            <p className="wallet">
              Wallet connected: <code>{short(walletAddr)}</code>
            </p>
          ) : (
            <p className="wallet">
              {wallets.map((w) => (
                <button key={w} className="link" onClick={() => handleConnect(w)}>
                  Connect {w === 'unisat' ? 'Unisat' : 'Xverse'}
                </button>
              ))}
            </p>
          ))}
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
