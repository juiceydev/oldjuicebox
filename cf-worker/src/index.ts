import { Hono } from "hono";

interface Env {
  JUICEBACK_RELATIONS: KVNamespace;
  JUICEBACK_URL: string;
  DEFAULT_ORIGIN: string;
  REGION_HOSTS: string;
  COLO_REGIONS: string;
}

type Bindings = { Bindings: Env };

const KV_MAX_TTL_SECS = 86_400;
const KV_MIN_TTL_SECS = 60;
const INFO_TIMEOUT_MS = 5_000;

const REGION_HOSTS = new Map<string, string>();
const COLO_TO_REGION = new Map<string, string>();

function parseMaps(env: Env): void {
  if (REGION_HOSTS.size === 0 && env.REGION_HOSTS) {
    for (const pair of env.REGION_HOSTS.split(",")) {
      const idx = pair.indexOf("=");
      if (idx <= 0) continue;
      const region = pair.slice(0, idx).trim().toLowerCase();
      const host = pair.slice(idx + 1).trim().replace(/\/+$/, "");
      if (region && host) REGION_HOSTS.set(region, host);
    }
  }
  if (COLO_TO_REGION.size === 0 && env.COLO_REGIONS) {
    for (const group of env.COLO_REGIONS.split(";")) {
      const idx = group.indexOf("=");
      if (idx <= 0) continue;
      const region = group.slice(0, idx).trim().toLowerCase();
      for (const colo of group.slice(idx + 1).split(",")) {
        const c = colo.trim().toUpperCase();
        if (region && c) COLO_TO_REGION.set(c, region);
      }
    }
  }
}

function coloToRegion(colo: unknown): string | null {
  if (typeof colo !== "string") return null;
  return COLO_TO_REGION.get(colo.toUpperCase()) ?? null;
}

function fileIdFromPath(pathname: string): string | null {
  const m = pathname.match(/^\/f\/([A-Za-z0-9_-]+)(?:\.[^/]*)?$/);
  return m ? m[1] : null;
}

const app = new Hono<Bindings>();

app.on(["GET", "HEAD"], "/f/*", async (c) => {
  parseMaps(c.env);
  const url = new URL(c.req.raw.url);
  const id = fileIdFromPath(url.pathname);
  if (!id) {
    return fetch(c.req.raw);
  }

  let host = await c.env.JUICEBACK_RELATIONS.get(`region:${id}`);
  if (host === null) {
    let info: { storage_host?: string | null; expires_at?: number } | null = null;
    let infoStatus = 0;
    try {
      const res = await fetch(
        `${c.env.JUICEBACK_URL.replace(/\/+$/, "")}/file/${encodeURIComponent(id)}/info`,
        { signal: AbortSignal.timeout(INFO_TIMEOUT_MS) },
      );
      infoStatus = res.status;
      if (res.ok) info = await res.json();
    } catch {
      infoStatus = 0;
    }

    if (infoStatus === 404 || infoStatus === 410) {
      return c.json(
        { error: "FILE_NOT_FOUND", message: "File not found" },
        infoStatus === 410 ? 410 : 404,
      );
    }
    if (!info) {
      return new Response(null, {
        status: 307,
        headers: { Location: `${c.env.DEFAULT_ORIGIN}${url.pathname}${url.search}` },
      });
    }

    host = typeof info.storage_host === "string" ? info.storage_host : "";
    const expiresAt = typeof info.expires_at === "number" ? info.expires_at : 0;
    const ttl = Math.min(
      Math.max(expiresAt - Math.floor(Date.now() / 1000), KV_MIN_TTL_SECS),
      KV_MAX_TTL_SECS,
    );
    c.executionCtx.waitUntil(
      c.env.JUICEBACK_RELATIONS.put(`region:${id}`, host, { expirationTtl: ttl }).catch(() => {}),
    );
  }

  const target = host !== "" ? host : c.env.DEFAULT_ORIGIN;
  return new Response(null, {
    status: 307,
    headers: {
      Location: `${target.replace(/\/+$/, "")}${url.pathname}${url.search}`,
      "Cache-Control": "no-store",
    },
  });
});

async function forwardWithRegion(c: { env: Env; req: { raw: Request } }): Promise<Response> {
  parseMaps(c.env);
  const req = c.req.raw;
  const url = new URL(req.url);
  const region = coloToRegion((req as Request & { cf?: { colo?: unknown } }).cf?.colo);

  const headers = new Headers(req.headers);
  if (region) {
    headers.set("x-juicebox-region", region);
  } else {
    headers.delete("x-juicebox-region");
  }

  const hasBody = req.method !== "GET" && req.method !== "HEAD";
  return fetch(`${c.env.JUICEBACK_URL.replace(/\/+$/, "")}${url.pathname}${url.search}`, {
    method: req.method,
    headers,
    body: hasBody ? req.body : undefined,
    duplex: hasBody ? "half" : undefined,
  } as RequestInit);
}

app.on("OPTIONS", "/upload/*", (c) => fetch(c.req.raw));
app.on("OPTIONS", "/api/tus/*", (c) => fetch(c.req.raw));

app.on(["POST", "PUT", "PATCH", "HEAD"], "/upload/*", (c) => forwardWithRegion(c));
app.on(["POST", "PUT", "PATCH", "HEAD"], "/api/tus/*", (c) => forwardWithRegion(c));

app.all("*", (c) => fetch(c.req.raw));

export default app;
