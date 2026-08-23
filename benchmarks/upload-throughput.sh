#!/usr/bin/env bash
set -euo pipefail

# Compare raw Juicehost ticket upload against the existing HTTP endpoint. The
# caller supplies a real ticket and file; this script never prints credentials.
JUICEHOST_URL=${JUICEHOST_URL:-http://127.0.0.1:6402}
FILE=${1:?usage: $0 FILE TICKET FILE_ID}
TICKET=${2:?usage: $0 FILE TICKET FILE_ID}
FILE_ID=${3:?usage: $0 FILE TICKET FILE_ID}

test -f "$FILE"

bytes=$(wc -c < "$FILE")
printf 'file_bytes=%s target=%s\n' "$bytes" "$JUICEHOST_URL"

started=$(date +%s%N)
curl --fail --silent --show-error \
  --request POST \
  --header "Authorization: Bearer $TICKET" \
  --header 'Content-Type: application/octet-stream' \
  --header "Content-Length: $bytes" \
  --data-binary "@$FILE" \
  "$JUICEHOST_URL/internal/file/upload/$FILE_ID" >/dev/null
finished=$(date +%s%N)
elapsed_ns=$((finished - started))
mbps=$(awk -v bytes="$bytes" -v ns="$elapsed_ns" 'BEGIN { printf "%.2f", (bytes * 8000.0 / ns) }')
printf 'elapsed_ms=%s throughput_mbps=%s\n' "$((elapsed_ns / 1000000))" "$mbps"
