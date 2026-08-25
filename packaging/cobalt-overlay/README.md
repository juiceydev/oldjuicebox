# cobalt overlay: per-video content-bound PO tokens

Patches `api/src/processing/services/youtube.js` so that when
`YOUTUBE_SESSION_SERVER` is configured and `YOUTUBE_POT_PER_VIDEO=1`,
cobalt mints a **per-video-ID** PO token from the session provider
(bgutil-ytdlp-pot-provider compatible `POST /get_pot` with
`content_binding`) and:

1. passes it as `serviceIntegrityDimensions.poToken` in the innertube
   player request, and
2. appends it as `&pot=` to every googlevideo stream URL.

This satisfies YouTube's rolling per-video streaming-token enforcement,
which otherwise yields empty (0-byte) tunnels for affected videos.

Build & deploy:
    docker build -t cobalt:11-pot .
    # point the cobalt systemd units at cobalt:11-pot and set
    # YOUTUBE_SESSION_SERVER=<bgutil>/ plus YOUTUBE_POT_PER_VIDEO=1

Based on the mechanism from imputnet/cobalt PR #1581 (ANDROID_VR visitor
bootstrap); this variant binds tokens per video ID via bgutil instead.
