import { defineMiddleware } from "astro/middleware";

const API_INTERNAL = process.env.API_INTERNAL_URL || "http://127.0.0.1:6401";
const JUICEBACK_URL = process.env.JUICEBACK_URL || "http://127.0.0.1:6401";
const JUICEHOST_URL = process.env.JUICEHOST_URL || "http://127.0.0.1:6402";

// Hop-by-hop headers that must not be forwarded through a proxy.
const HOP_BY_HOP = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailers",
  "transfer-encoding",
  "upgrade",
  "host",
  "content-length",
]);

/** Map a request path to the upstream service it should hit, or null for frontend routes. */
function proxyTarget(pathname: string): string | null {
  if (pathname.startsWith("/api/")) return JUICEBACK_URL;
  if (pathname === "/upload" || pathname.startsWith("/upload/")) return JUICEBACK_URL;
  if (pathname.startsWith("/file/")) return JUICEBACK_URL;
  if (pathname.startsWith("/f/")) return JUICEHOST_URL;
  return null;
}

function cleanHeaders(headers: Headers): Headers {
  const out = new Headers();
  for (const [key, value] of headers) {
    const lower = key.toLowerCase();
    if (HOP_BY_HOP.has(lower)) continue;
    if (lower === "set-cookie") continue;
    out.append(key, value);
  }
  // Set-Cookie must be copied per-value so multiple cookies survive.
  for (const cookie of headers.getSetCookie()) {
    out.append("Set-Cookie", cookie);
  }
  return out;
}

export const onRequest = defineMiddleware(async (context, next) => {
  // Pages read JUICEBACK_URL at render time; give it a local default so
  // server-side fetches work even when the var isn't set in the shell.
  process.env.JUICEBACK_URL ||= JUICEBACK_URL;

  const { request, redirect } = context;
  const url = new URL(request.url);

  if (url.pathname.startsWith("/admin/") || url.pathname === "/admin") {
    if (url.pathname.endsWith("/login")) {
      return next();
    }

    try {
      const res = await fetch(`${API_INTERNAL}/api/admin/check`, {
        headers: { cookie: request.headers.get("cookie") || "" },
      });
      if (!res.ok) {
        return redirect("/admin/login");
      }
    } catch {
      return redirect("/admin/login");
    }
  }

  const target = proxyTarget(url.pathname);
  if (!target) {
    // Clickjacking protection must be a real header: CSP frame-ancestors
    // is ignored when delivered via a <meta> element.
    const res = await next();
    res.headers.set("X-Frame-Options", "DENY");
    return res;
  }

  const headers = cleanHeaders(request.headers);
  headers.set("x-forwarded-host", url.host);
  headers.set("x-forwarded-proto", url.protocol.slice(0, -1));
  const clientIp = request.headers.get("x-forwarded-for");
  if (clientIp) {
    headers.set("x-forwarded-for", clientIp);
  }

  const init: RequestInit = {
    method: request.method,
    headers,
    redirect: "manual",
  };
  if (request.method !== "GET" && request.method !== "HEAD" && request.body) {
    init.body = request.body;
    // @ts-expect-error -- required for stream bodies in undici/node fetch
    init.duplex = "half";
  }

  try {
    const upstream = await fetch(target + url.pathname + url.search, init);
    return new Response(upstream.body, {
      status: upstream.status,
      headers: cleanHeaders(upstream.headers),
    });
  } catch {
    return new Response("Proxy error: upstream unavailable", {
      status: 502,
      headers: { "Content-Type": "text/plain" },
    });
  }
});
