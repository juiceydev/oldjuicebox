# cf-worker

Cloudflare Worker for JuiceBox multi-region edge routing (Hono).

Two jobs:

1. **Downloads** — `f.juicey.dev/f/{id}[.{ext}]`: resolves the file's storage
   host (KV-cached, backed by juiceback's public `/file/{id}/info`) and 307s to
   the regional origin. Legacy files (no `storage_host`) 307 to
   `DEFAULT_ORIGIN`. No bytes are relayed.
2. **Upload routing** — `box.juicey.dev/upload*` and `/api/tus*`: maps the
   serving colo to a region, injects `x-juicebox-region`, and streams the
   request to central juiceback, which pins the file's `storage_host` to that
   region. All other paths fall through to origin untouched.

## Loop hazard (read before deploying)

Every fetch target must bypass this worker or requests will loop until the
subrequest cap kills them:

- `JUICEBACK_URL` (central juiceback)
- `DEFAULT_ORIGIN` (legacy files)
- every host listed in `REGION_HOSTS` (`f-us.` / `f-eu.` / `f-asia...`)

None of these hostnames may carry a worker route. Point them at the origin
servers (or worker-free DNS records).

## Deploy

1. Create a KV namespace and put its id in `wrangler.toml`
   (`JUICEBACK_RELATIONS`). KV writes are 1/day/file (free-tier budget);
   entries expire with the file's remaining TTL.
2. Fill in the vars in `wrangler.toml`.
3. `bun install && bun run deploy` (or `npx wrangler deploy`).
