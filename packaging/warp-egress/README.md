# Secondary server egress + YouTube rescue ladder

Resolution order for every YouTube fetch (first match wins):

1. **cobalt primary** (IOS, cookieless) — serves the vast majority of links.
2. **cobalt-session** (MWEB + cookies + per-video POT from bgutil), both
   egressing via the **secondary server** (tinyproxy on the home box, port
   8888).
3. **juicebox-bun-fetch** (see ../bun-fetch): Bun-powered yt-dlp with per-
   video PO tokens. Best on the secondary itself (direct residential egress,
   localhost POT); on the primary it egresses via the secondary proxy.
   Configured via `BUN_FETCH_URL` / `BUN_FETCH_KEY`.
4. **local yt-dlp tier** (`YTDLP_*`): secondary proxy first, then
   **Cloudflare WARP egress** (netns `warpns`, tinyproxy 10.200.0.2:8889)
   as fallback. Bun + deno JS runtimes for EJS challenge solving.

Enforcement windows flap, so tiers 2–4 repeat every
`FETCH_EMPTY_RETRY_DELAY_SECS` until the job budget runs out.

Components:
- `setup_warp.sh`: netns `warpns` with Cloudflare WARP WireGuard up +
  tinyproxy on 10.200.0.2:8889 (nft masquerade for bootstrap).
  Installed as warp-egress.service.
- `cobalt.service` / `cobalt-session.service`: current production units.
  Primary = IOS cookieless; session = MWEB+cookies+bgutil per-video POT
  (overlay image cobalt:11-pot, see ../cobalt-overlay).
- juiceback env (env.example): YTDLP_* tier-4, i.e. yt-dlp + bgutil plugin
  via the proxies above when both cobalt tiers return empty or refuse.
- `bun-fetch.service`: the sidecar on this box (see ../bun-fetch).
