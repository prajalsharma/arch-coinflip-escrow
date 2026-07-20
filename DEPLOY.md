# Deploying

Two pieces, two hosts. **Vercel can only host the frontend.**

```
Browser ──> Vercel (React app)  ──> Railway/Fly/Render (Rust backend) ──> Arch testnet
                                     holds the house authority key
```

## Why not everything on Vercel

Vercel is serverless functions + static hosting. It has **no Docker support**, and Rust is
not a supported runtime. The settlement backend is a long-running axum server that holds a
signing key, so it needs a normal container host.

If you deploy only the frontend, the app loads but shows *"Backend unreachable"* and the
Flip button stays disabled. **Deploy the backend first.**

---

## Step 1 — Backend (do this first)

The `Dockerfile` lives at the **repo root** (not in `backend/`) for two reasons: the backend
depends on the `program` crate by path so the build context must include both, and Railway's
auto-detector only looks at the root.

### Railway — no build settings needed

1. New Project → Deploy from GitHub → pick this repo
2. Add the variables below
3. Deploy, then Settings → Networking → **Generate Domain**

That's it. Railway sees the root `Dockerfile` and uses it. `railway.json` pins
`builder: DOCKERFILE` explicitly so it can't fall back to auto-detection.

> **If you see `Railpack could not determine how to build the app`**, Railway is trying to
> auto-detect instead of using Docker. That happens when the Dockerfile isn't at the root, or
> when a stale service was created before it was. Fix: redeploy, or set
> Settings → Build → Builder = **Dockerfile**.

### Fly.io

```bash
fly launch --dockerfile Dockerfile --no-deploy
fly secrets set HOUSE_AUTHORITY_SECRET_KEY=<hex>
fly deploy
```

### Render

New → Web Service → **Runtime: Docker**. Dockerfile path `./Dockerfile`, context `.`.

### Environment variables

| Variable | Value | Notes |
|---|---|---|
| `HOUSE_AUTHORITY_SECRET_KEY` | 64-char hex | **Secret.** See below |
| `PROGRAM_ID` | `8ea69ca483247ded86a152bc809e05caf1f0326c604877f8071947420053c635` | Public |
| `ARCH_NETWORK` | `testnet` | |
| `ALLOWED_ORIGIN` | `https://your-app.vercel.app` | Set this in production |
| `PORT` | usually injected by the host | Defaults to 8080 |

Get the authority key (run locally, never paste it into a chat or commit it):

```bash
cd program
python3 -c "import json;print(bytes(json.load(open('.testnet-authority.json'))).hex())"
```

Verify the deploy:

```bash
curl https://your-backend.up.railway.app/health
# {"ok":true,"network":"https://rpc.testnet.arch.network","block_height":35699242,...}
```

> **Keep the authority funded.** On a win it pays the matching half from its own balance,
> so an empty authority means settlement fails.
> `arch-cli --profile testnet account airdrop --keypair-path ./.testnet-authority.json`

---

## Step 2 — Frontend on Vercel

1. Add New → Project → import this repo
2. **Root Directory: `app`** ← this is the setting people miss; without it you get a 404
   because Vercel finds no web app at the repo root
3. Framework Preset: **Vite** (auto-detected)
4. Build Command `npm run build`, Output Directory `dist` (both default)
5. Environment Variables:

| Variable | Value |
|---|---|
| `VITE_BACKEND_URL` | `https://your-backend.up.railway.app` — no trailing slash |
| `VITE_ARCH_RPC_URL` | `https://rpc.testnet.arch.network` |
| `VITE_PROGRAM_ID` | `8ea69ca483247ded86a152bc809e05caf1f0326c604877f8071947420053c635` |

6. Deploy.

**No secrets belong here.** Everything above is public. Anything prefixed `VITE_` is
compiled into the browser bundle and readable by anyone — never put the authority key in
a `VITE_` variable.

## Step 3 — Close the loop

Once Vercel gives you a domain, set `ALLOWED_ORIGIN` on the backend to that exact origin
and redeploy. Until you do, the backend logs:

```
WARN ALLOWED_ORIGIN not set — CORS is open to any origin (dev only)
```

Then open the site and click **Flip the coin**.

---

## Troubleshooting

| Symptom | Cause |
|---|---|
| `404: NOT_FOUND` on Vercel | Root Directory not set to `app` |
| "Backend unreachable" | Backend not deployed, or `VITE_BACKEND_URL` wrong/trailing slash |
| CORS error in console | `ALLOWED_ORIGIN` missing or doesn't match the Vercel origin exactly |
| Settle returns 500 | Authority out of funds — airdrop to it |
| Env var change didn't apply | `VITE_*` vars are baked in at build time — **redeploy**, don't just restart |

## Cost

Arch testnet RPC needs no API key and no account. The faucet is free. Railway and Vercel
both have free tiers sufficient for this. **Total third-party API keys required: zero.**
