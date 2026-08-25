# Secondary server egress + YouTube rescue ladder

Resolution order for every YouTube fetch (first match wins):

1. **Secondary server egress** (tinyproxy on the home box, port 8888):
   cobalt primary + session tiers, and yt-dlp's first proxy candidate.
2. **Cloudflare WARP egress** (netns `warpns`, tinyproxy 10.200.0.2:8889):
   second proxy candidate for yt-dlp, and cobalt-session's escape hatch
   when the home IP is flagged.

Components:
- `setup_warp.sh`: netns `warpns` with Cloudflare WARP WireGuard up +
  tinyproxy on 10.200.0.2:8889 (nft masquerade for bootstrap).
  Installed as warp-egress.service.
- `cobalt.service` / `cobalt-session.service`: current production units.
  Primary = IOS cookieless; session = MWEB+cookies+bgutil per-video POT
  (overlay image cobalt:11-pot, see ../cobalt-overlay).
- juiceback env (env.example): YTDLP_* tier-3, i.e. yt-dlp + bgutil plugin
  via the proxies above when both cobalt tiers return empty or refuse.
