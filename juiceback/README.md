# juiceback

API backend for [Juicebox²](https://github.com/juiceboxdev/juicebox). Handles authentication, SQLite database, routing, TUS resumable uploads, device pairing, admin panel, reports, and file management.

## Configuration

All via environment variables.

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `127.0.0.1` | Bind address |
| `PORT` | `6401` | HTTP port |
| `QUIC_PORT` | PORT + 1 | QUIC/HTTP3 port |
| `DATABASE_PATH` | `./delta.db` | SQLite database path |
| `RATE_LIMIT_PER_MINUTE` | `10` | Per-IP rate limit |
| `DB_POOL_SIZE` | `8` | SQLite connection pool size |
| `PUBLIC_BASE_URL` | `http://localhost:6402` | Public-facing base URL |
| `LOG_LEVEL` | `info` | Logging verbosity |
| `CLEANUP_INTERVAL_MINUTES` | `30` | Expired file cleanup interval |
| `JUICEHOST_API_KEY` | (empty) | Shared secret with juicehost |
| `JUICEHOST_URL` | `http://127.0.0.1:6402` | Storage server URL |
| `PUBLIC_JUICEHOST_URL` | `PUBLIC_BASE_URL` | Public-facing juicehost URL handed to clients for direct uploads. Empty disables direct upload |
| `JUICEBACK_ORIGIN` | `http://{HOST}:{PORT}` | Origin used for callbacks |
| `JWT_SECRET` | random (warn) | JWT signing key |
| `IP_ENCRYPTION_KEY` | random (warn) | AES-256-GCM key for IPs |
| `IP_PEPPER` | random (warn) | HMAC key for ban lookups |
| `CORS_ORIGINS` | `http://localhost:6400` | Allowed CORS origins |
| `REPORT_WEBHOOK_URL` | (none) | Discord webhook for reports |
| `SMTP_HOST` | (none) | SMTP server for email reports |
| `SMTP_PORT` | `465` | SMTP port |
| `SMTP_USERNAME` | (none) | SMTP username |
| `SMTP_PASSWORD` | (none) | SMTP password |
| `REPORT_EMAIL_RECIPIENT` | (none) | Where reports get emailed |
| `REPORT_EMAIL_SENDER` | (none) | From address for report emails |
| `TRUST_PROXY` | `false` | Trust X-Forwarded-For / X-Real-IP |
| `CF_API_TOKEN` | (none) | Cloudflare API token for cache purge |
| `CF_ZONE_ID` | (none) | Cloudflare zone ID for cache purge |
| `TICKET_JWT_SECRET` | falls back to JWT_SECRET | JWT secret for one-time file tickets |
| `QUIC_CERT_PATH` | `./quic-cert.der` | TLS cert for QUIC |
| `DTE` | `false` | Enables dynamic size-based ticket TTL + per-IP mint limiting |
| `DTE_ASSUMED_BPS` | `2 MiB/s` | Assumed upload throughput for TTL estimation |
| `DTE_SAFETY_MULT` | `2.0` | Safety multiplier on estimated upload time |
| `DTE_BASE_OVERHEAD_SECS` | `120` | Fixed overhead added to every dynamic TTL |
| `DTE_MIN_TTL_SECS` | `600` | Minimum ticket TTL (10 min) |
| `DTE_MAX_TTL_SECS` | `21600` | Maximum ticket TTL (6 h) |
| `DTE_MINT_LIMIT` | `60` | Max tickets an IP may mint per window |
| `DTE_MINT_WINDOW_SECS` | `600` | Mint limiting window (10 min) |
| `DTE_MINT_BURST` | `15` | Immediate mint burst allowed per IP |
| `REGION_PUBLIC_JUICEHOSTS` | (none) | Regional juicehost URLs, `region:url` pairs, e.g. `us:https://f-us.juicey.dev,eu:https://f-eu.juicey.dev`. Set only via the edge worker's `x-juicebox-region` header (trusted proxies) |

Startup is rejected if `JWT_SECRET` is empty or set to `change_this_to_a_random_value`.

### Dynamic ticket expiry

When `DTE=true`, the `/upload/direct/reserve` and
`/upload/ultrafast/reserve` endpoints issue size-based ticket TTLs instead of
their fixed ~15m/~5m legacy values, and enforce a per-IP mint limit.

- TTL is deterministic and hard-clamped: `clamp(MIN_TTL, ceil(size / ASSUMED_BPS) * SAFETY_MULT + BASE_OVERHEAD, MAX_TTL)`.
- When the toggle is `false` (default), legacy fixed expiry is preserved unchanged.
- The mint limit is per-IP only (no login system); exceeding it returns HTTP 429.
- Behavior is global (enforced in central juiceback) and identical across regions.

### Regions

With `REGION_PUBLIC_JUICEHOSTS` configured, the edge worker can inject
`x-juicebox-region` on `/upload/*` and `/api/tus/*` requests (the header is
honored only from `TRUSTED_PROXY_CIDRS` peers). Reserved files then get their
`storage_host` pinned to the regional juicehost: direct-ticket `upload_url`s
and share URLs point at that region, and complete handlers verify bytes on the
file's own host. Cloudflare purges use the file's regional URL and its
`juicebox:{id}` cache tag.

## Admin Management

Use `scripts/manage-admin.sh` to manage admin accounts. It reads `DATABASE_PATH` from your `.env` and uses the juiceback binary's `hash-password` subcommand.

```bash
# add an admin (prompts for password)
scripts/manage-admin.sh add <username>

# remove an admin
scripts/manage-admin.sh remove <username>

# list all admins
scripts/manage-admin.sh list
```

The script auto-detects the binary in `target/release/` or `target/debug/`. Override with `JUICEBACK_BIN`.

## Routes

- `POST /api/upload` - multipart file upload
- `POST /api/tus/upload` - TUS create
- `PATCH /api/tus/upload/:id` - TUS upload chunk
- `GET /api/file/:id` - file metadata
- `DELETE /api/file/:id` - delete file (requires delete token)
- `POST /api/file/:id/rename` - rename file (requires delete token)
- `POST /api/owned-files` - bulk file lookup by (id, token) pairs
- `POST /api/pair/generate` - generate device pairing code
- `GET /api/device/ws` - device WebSocket connection
- `GET /api/device/events` - SSE for browser pairing status
- `POST /api/report` - file report
- `POST /api/feedback` - user feedback
- `/admin/*` - admin panel (JWT-authenticated)
- `/docs/` - OpenAPI documentation

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
