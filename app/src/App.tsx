import { useEffect, useState } from 'react'
import {
  ARCH_RPC_URL,
  PROGRAM_ID_HEX,
  connectWallet,
  detectWallets,
  getBalance,
  health,
  openDemoSession,
  readSession,
  sessionPda,
  settleSession,
  statusLabel,
  vaultPda,
  hexOf,
  type WalletKind,
} from './arch'

const WAGER = 10_000

type Phase = 'idle' | 'opening' | 'open' | 'flipping' | 'settled'

type HistoryRow = {
  sessionId: number
  player: string
  won: boolean
  wager: number
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

  async function refreshChain(p: string, sid: number) {
    const [bal, sess] = await Promise.all([
      getBalance(p),
      readSession(p, BigInt(sid)),
    ])
    setBalance(bal)
    if (sess) setOnChainStatus(sess.status)
    const spda = sessionPda(p, BigInt(sid))
    const vbal = await getBalance(hexOf(vaultPda(spda)))
    setEscrowed(vbal)
  }

  async function handlePlay() {
    setError(null)
    setResult(null)
    setOnChainStatus(null)
    setPhase('opening')

    try {
      // 1. Open a session. The stake moves into the escrow vault PDA on-chain.
      const opened = await openDemoSession(WAGER)
      setPlayer(opened.player)
      setSessionId(opened.session_id)
      setBalance(opened.balance)
      setEscrowed(opened.escrowed)
      setOnChainStatus(0)
      setPhase('open')

      // 2. Settle. The coin flip happens off-chain; the house authority signs it.
      setPhase('flipping')
      const settled = await settleSession(opened.player, opened.session_id)
      setResult(settled.player_won)
      setPhase('settled')

      setHistory((h) =>
        [
          {
            sessionId: opened.session_id,
            player: opened.player,
            won: settled.player_won,
            wager: WAGER,
          },
          ...h,
        ].slice(0, 8),
      )

      await refreshChain(opened.player, opened.session_id)
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
      setError(`wallet connect failed: ${e.message ?? e}`)
    }
  }

  const busy = phase === 'opening' || phase === 'flipping'

  return (
    <div className="page">
      <header className="header">
        <div>
          <h1>Arch Coin Flip</h1>
          <p className="sub">
            On-chain escrow on Bitcoin via Arch · results settled off-chain
          </p>
        </div>
        <div className="netbox">
          <span className={`dot ${backendOk ? 'ok' : backendOk === false ? 'bad' : ''}`} />
          <div>
            <div className="netname">Arch Testnet</div>
            {blockHeight && <div className="netmeta">block {fmt(blockHeight)}</div>}
          </div>
        </div>
      </header>

      {backendOk === false && (
        <div className="banner error">
          Backend unreachable. Start it with{' '}
          <code>cd backend && cargo run</code> — settlement needs the house
          authority key, which cannot live in the browser.
        </div>
      )}

      <section className="card wallet">
        <div className="cardhead">
          <h2>Wallet</h2>
          {walletAddr && <span className="pill ok">connected</span>}
        </div>

        {walletAddr ? (
          <div className="kv">
            <span>Bitcoin address</span>
            <code>{short(walletAddr)}</code>
          </div>
        ) : wallets.length > 0 ? (
          <div className="row">
            {wallets.map((w) => (
              <button key={w} className="btn ghost" onClick={() => handleConnect(w)}>
                Connect {w === 'unisat' ? 'Unisat' : 'Xverse'}
              </button>
            ))}
          </div>
        ) : (
          <p className="muted">
            No Bitcoin wallet detected. Arch signs with secp256k1 / BIP-322, so it
            needs a Taproot wallet like Unisat or Xverse — not Phantom.
          </p>
        )}

        <p className="note">
          Demo mode below uses a fresh faucet-funded testnet key so you can play
          without installing anything. Connecting a wallet shows your Bitcoin
          identity; wallet-signed staking is not wired up yet.
        </p>
      </section>

      <section className="card play">
        <div className="stakebox">
          <div>
            <div className="label">Stake</div>
            <div className="stake">{fmt(WAGER)}</div>
            <div className="unit">lamports</div>
          </div>
          <div className="arrow">→</div>
          <div>
            <div className="label">Payout if you win</div>
            <div className="stake win">{fmt(WAGER * 2)}</div>
            <div className="unit">lamports</div>
          </div>
        </div>

        <button className="btn primary" onClick={handlePlay} disabled={busy || backendOk === false}>
          {phase === 'opening'
            ? 'Escrowing stake…'
            : phase === 'flipping'
              ? 'Flipping…'
              : 'Flip the coin'}
        </button>

        {busy && (
          <div className="steps">
            <div className={`step ${phase === 'opening' ? 'active' : 'done'}`}>
              1 · Stake escrowed in vault PDA
            </div>
            <div className={`step ${phase === 'flipping' ? 'active' : ''}`}>
              2 · House settles result on-chain
            </div>
          </div>
        )}

        {result !== null && phase === 'settled' && (
          <div className={`result ${result ? 'won' : 'lost'}`}>
            <div className="resulttitle">{result ? 'You won' : 'You lost'}</div>
            <div className="resultsub">
              {result
                ? `${fmt(WAGER * 2)} lamports paid from escrow + house`
                : `${fmt(WAGER)} lamports went to the house`}
            </div>
          </div>
        )}

        {error && <div className="banner error">{error}</div>}
      </section>

      {player && sessionId !== null && (
        <section className="card">
          <div className="cardhead">
            <h2>On-chain state</h2>
            {onChainStatus !== null && (
              <span className={`pill ${onChainStatus === 1 ? 'ok' : onChainStatus === 2 ? 'bad' : ''}`}>
                {statusLabel(onChainStatus)}
              </span>
            )}
          </div>
          <div className="kv"><span>Player</span><code>{short(player)}</code></div>
          <div className="kv"><span>Session ID</span><code>{sessionId}</code></div>
          <div className="kv"><span>Escrow vault</span><code>{fmt(escrowed)} lamports</code></div>
          {balance !== null && (
            <div className="kv"><span>Player balance</span><code>{fmt(balance)} lamports</code></div>
          )}
          <p className="note">Read directly from Arch RPC, not from the backend.</p>
        </section>
      )}

      {history.length > 0 && (
        <section className="card">
          <div className="cardhead"><h2>Recent sessions</h2></div>
          <div className="history">
            {history.map((h) => (
              <div key={h.sessionId} className="hrow">
                <code>{short(h.player)}</code>
                <span className="hid">#{h.sessionId}</span>
                <span className={`pill ${h.won ? 'ok' : 'bad'}`}>
                  {h.won ? 'Won' : 'Lost'}
                </span>
              </div>
            ))}
          </div>
        </section>
      )}

      <footer className="footer">
        <div className="kv"><span>Program</span><code>{short(PROGRAM_ID_HEX)}</code></div>
        <div className="kv"><span>RPC</span><code>{ARCH_RPC_URL.replace('https://', '')}</code></div>
        <p className="note">
          The house decides the outcome — Arch has no on-chain randomness primitive.
          This is a testnet MVP, not a trustless game.
        </p>
      </footer>
    </div>
  )
}
