# juicebox-bun-fetch — Bun SABR-proof YouTube fetch sidecar

`server.ts` runs on Bun with **zero npm dependencies**: it exposes an
authenticated HTTP API that runs `yt-dlp` locally with Bun as the JS
challenge runtime plus a bgutil PO-token provider, and streams the finished
file back. juiceback calls it as its last-resort YouTube tier (`BUN_FETCH_URL`
/ `BUN_FETCH_KEY`) when both cobalt instances and its own yt-dlp tier fail.

## Why this defeats SABR enforcement

YouTube's 2026 enforcement stack needs three things at once:

1. **Residential egress** — datacenter IPs get SABR-only formats / 403s on
   `*.googlevideo.com`. The secondary (home) box has this natively.
2. **Per-video PO tokens** — minted from the bgutil provider. On the
   secondary the provider is `localhost:4416` (no tailscale timeouts like
   the `ConnectTimeout ... 100.81.219.120:4416` flakes in cobalt-session's
   log); on the primary it goes over tailscale.
3. **Working EJS challenge solving** (signature + `n`-throttle) — Bun is
   yt-dlp's fastest JS runtime; without it you get `Signature solving
   failed` → `Only images are available for download`.

The service also reports the **exact** post-merge path via
`--print after_move:filepath` (no tmpdir guessing — the bug that marked a
real 153 MB download as "SABR enforcement" in `run_ytdlp_tier`).

## API

- `GET /health` → `{ok, service, bun, ytdlp, pot_ok, in_flight}`
- `POST /fetch` (or `POST /` — juiceback posts to the base-URL root)
  (needs `Authorization: Bearer <BUN_FETCH_KEY>`)
  ```json
  { "url": "https://www.youtube.com/watch?v=...", "audio_only": false, "max_height": 1080 }
  ```
  - `200` + file bytes (`Content-Disposition`, `X-Bun-Fetch-Filename`).
  - `400` bad/unsafe URL (SSRF guard: no creds, no local/private hosts).
  - `401` bad token. `429` all `MAX_JOBS` slots busy.
  - `410` YouTube yielded no downloadable streams (truly walled).
  - `502` yt-dlp transport failure (proxy/POT down) — retryable.
  - `413` file over `SERVICE_MAX_BYTES`.

## Files

| file | purpose |
| ---- | ------- |
| `server.ts` | the service (Bun, no deps) |
| `package.json` | `bun start` script |
| `bun-fetch.env.example` | all env vars, primary + secondary values |
| `bun-fetch.service` | systemd unit for the **primary** (egress via secondary proxy) |
| `bun-fetch-secondary.service` | systemd unit for the **secondary** (direct egress) |
| `install-secondary.sh` | one-shot installer to run **on the secondary** as root |
| `secondary.Caddyfile.snippet` | optional TLS fronting on the secondary |

## Primary deploy (this box)

```sh
sudo mkdir -p /var/lib/juicebox/bun-fetch /var/lib/juicebox/bun-fetch-tmp
sudo cp packaging/bun-fetch/server.ts /var/lib/juicebox/bun-fetch/
sudo chown -R juicebox:juicebox /var/lib/juicebox/bun-fetch /var/lib/juicebox/bun-fetch-tmp
# create /var/lib/juicebox/bun-fetch.env from bun-fetch.env.example
# (BUN_FETCH_KEY must match juiceback's), then:
sudo cp packaging/bun-fetch/bun-fetch.service /etc/systemd/system/
sudo systemctl enable --now bun-fetch
curl -s http://127.0.0.1:7673/health
```

juiceback config (`/var/lib/juicebox/.env`):

```sh
BUN_FETCH_URL=http://127.0.0.1:7673
BUN_FETCH_KEY=<same key>
```

After the secondary is deployed, just repoint `BUN_FETCH_URL` at it
(`http://100.81.219.120:7673`, or the Caddy hostname) — no juiceback
rebuild needed.

## Secondary deploy (home box)

Copy this directory over (USB/scp/tailscale file drop — the box has no SSH),
then as root:

```sh
./install-secondary.sh
```

Then drop a Netscape `cookies.txt` at `/var/lib/bun-fetch/cookies.txt`
(`chown bunfetch:bunfetch`, mode 600 — yt-dlp writes session state back,
so it must be writable).

## Rescue-ladder position

1. cobalt primary (IOS, cookieless) — serves the vast majority of links
2. cobalt-session (MWEB + cookies + per-video POT via tailscale)
3. **bun-fetch** (this service — secondary-direct when deployed there)
4. local yt-dlp tier (secondary proxy → WARP proxy, Bun + deno solvers)
5. timed rescue-loop repeats until the job budget runs out
