# WARP egress namespace + yt-dlp tier-3 fallback

Rescues YouTube videos blocked by content-bound streaming-token
enforcement (empty cobalt tunnels).

Components:
- setup_warp.sh: netns `warpns` with Cloudflare WARP WireGuard up +
  tinyproxy on 10.200.0.2:8889 (nft masquerade for bootstrap).
  Installed as warp-egress.service.
- cobalt.service / cobalt-session.service: current production units.
  Primary = IOS cookieless; session = MWEB+cookies+bgutil per-video POT
  (overlay image cobalt:11-pot, see ../cobalt-overlay).
- juiceback env (env.example): YTDLP_* tier-3 — yt-dlp + bgutil plugin
  via the WARP proxy when both cobalt tiers return empty/refuse.
