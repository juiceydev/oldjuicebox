#!/bin/bash
# Install juicebox-bun-fetch on the SECONDARY (home/residential) box.
# Run as root on the secondary, from the directory containing server.ts:
#
#   sudo ./install-secondary.sh
#
# What it does:
#  - installs bun (if missing), yt-dlp, ffmpeg, deno (challenge fallback)
#  - creates user `bunfetch` + /var/lib/bun-fetch (server, tmp, cookies)
#  - writes /var/lib/bun-fetch/bun-fetch.env (prompts for bearer key)
#  - installs + starts bun-fetch.service (direct egress, localhost POT)
#
# Prerequisites on the secondary:
#  - bgutil-ytdlp-pot-provider listening on 127.0.0.1:4416
#  - a YouTube cookies.txt (Netscape format) you can paste at the prompt
#    path, or copy to /var/lib/bun-fetch/cookies.txt afterwards.
set -euo pipefail

APP_DIR=/var/lib/bun-fetch
SERVICE_USER=bunfetch
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "==> installing OS deps (ffmpeg, python3-pip)"
if command -v pacman >/dev/null; then
  pacman -Sy --noconfirm ffmpeg python-pipx unzip 2>&1 | tail -n 2
elif command -v apt-get >/dev/null; then
  apt-get update && apt-get install -y ffmpeg python3-pip unzip
else
  echo "no supported package manager (pacman/apt); install ffmpeg+python3-pip manually" >&2
fi

echo "==> bun"
if ! command -v bun >/dev/null; then
  curl -fsSL https://bun.sh/install | bash
  install -m 755 "$HOME/.bun/bin/bun" /usr/local/bin/bun
fi
bun --version

echo "==> yt-dlp"
if ! command -v yt-dlp >/dev/null; then
  pipx install yt-dlp 2>/dev/null || pip install --break-system-packages -U yt-dlp
  YTDLP="$(command -v yt-dlp || echo "$HOME/.local/bin/yt-dlp")"
  install -m 755 "$YTDLP" /usr/local/bin/yt-dlp
fi
/usr/local/bin/yt-dlp --version

echo "==> deno (challenge-solver fallback)"
if ! command -v deno >/dev/null; then
  curl -fsSL https://deno.land/install.sh | sh
  install -m 755 "$HOME/.deno/bin/deno" /usr/local/bin/deno
fi
deno --version | head -n 1

echo "==> user + dirs"
id "$SERVICE_USER" >/dev/null 2>&1 || useradd -r -m -d "$APP_DIR" -s /usr/sbin/nologin "$SERVICE_USER"
mkdir -p "$APP_DIR/tmp"
install -m 644 "$SRC_DIR/server.ts" "$APP_DIR/server.ts"
touch "$APP_DIR/cookies.txt"
chown -R "$SERVICE_USER:$SERVICE_USER" "$APP_DIR"
chmod 600 "$APP_DIR/cookies.txt"

echo "==> env file"
if [ ! -s "$APP_DIR/bun-fetch.env" ]; then
  read -rsp "Bearer key for BUN_FETCH_KEY (empty = generate): " KEY; echo
  if [ -z "$KEY" ]; then KEY="$(openssl rand -hex 32)"; echo "generated key: $KEY"; fi
  cat > "$APP_DIR/bun-fetch.env" <<EOF
# 0.0.0.0 so the primary can reach this over tailscale (Bearer-gated).
HOST=0.0.0.0
PORT=7673
BUN_FETCH_KEY=$KEY
YTDLP_BIN=/usr/local/bin/yt-dlp
YTDLP_POT_BASE_URL=http://127.0.0.1:4416
YTDLP_COOKIES=$APP_DIR/cookies.txt
YTDLP_TMPDIR=$APP_DIR/tmp
YTDLP_JS_RUNTIME=bun:/usr/local/bin/bun,deno:/usr/local/bin/deno
FETCH_PROXY=
FETCH_TIMEOUT_SECS=1500
SERVICE_MAX_BYTES=2147483648
MAX_JOBS=2
EOF
  chmod 600 "$APP_DIR/bun-fetch.env"
  chown "$SERVICE_USER:$SERVICE_USER" "$APP_DIR/bun-fetch.env"
fi

echo "==> systemd"
install -m 644 "$SRC_DIR/bun-fetch-secondary.service" /etc/systemd/system/bun-fetch.service
systemctl daemon-reload
systemctl enable --now bun-fetch.service
sleep 2
curl -s --max-time 10 http://127.0.0.1:7673/health || echo "health check failed — journalctl -u bun-fetch"
echo
echo "done. Next:"
echo "  1. put YouTube cookies in $APP_DIR/cookies.txt (Netscape format, owned by $SERVICE_USER)"
echo "  2. verify POT: curl -X POST 127.0.0.1:4416/get_pot -H 'Content-Type: application/json' -d '{\"content_binding\":\"healthcheck\"}'"
echo "  3. on the PRIMARY set BUN_FETCH_URL=http://100.81.219.120:7673 (+ same BUN_FETCH_KEY) and restart juiceback"
