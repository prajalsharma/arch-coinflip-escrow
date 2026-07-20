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

A `backend/Dockerfile` is included. Build context must be the **repo root**, not `backend/`,
because the backend depends on the `program` crate by path.

### Railway

1. New Project → Deploy from GitHub → pick this repo
2. Settings → **Root Directory: `/`** (leave at repo root)
3. Settings → **Dockerfile Path: `backend/Dockerfile`**
4. Add the variables below
5. Deploy, then Settings → Networking → **Generate Domain**

### Fly.io

```bash
fly launch --dockerfile backend/Dockerfile --no-deploy
fly secrets set HOUSE_AUTHORITY_SECRET_KEY=<hex>
fly deploy
```

### Environment variables

| Variable | Value | Notes |
|---|---|---|
| `HOUSE_AUTHORITY_SECRET_KEY` | 64-char hex | **Secret.** See below |
| `PROGRAM_ID` | `e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4` | Public |
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
| `VITE_PROGRAM_ID` | `e2c42f6caec4783e4573085e10c7125edaf182fda4b0f8cbb96f17ae72a141c4` |

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
