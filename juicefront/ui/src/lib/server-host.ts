/**
 * Server-side (SSR) resolution of juicehost configs with caching.
 *
 * The default provider (juiceback) and any configured known juicehost are
 * fetched on the server and cached, so a returning user who has a custom host
 * saved (juicebox_host cookie) gets that host's settings baked into the
 * server-rendered HTML - no need to hit "Apply" on every visit.
 *
 * Only reachable from `.astro` frontmatter (never shipped to the browser).
 */

export interface HostConfig {
  max_file_size_bytes?: number;
  max_ttl_hours?: number;
  default_ttl_hours?: number;
  allowed_ttl_hours?: number[];
  danger_level?: string;
  quick_link?: boolean;
  custom_id?: boolean;
  ultrafast?: boolean;
  quic?: boolean;
  cobalt?: boolean;
  /** "direct-prefer" routes uploads through direct tickets; "standard" uses TUS/multipart. */
  upload_mode?: string;
  fetch_rate_limit_per_minute?: number;
  public_base_url?: string;
}

export interface EffectiveHostConfig {
  config: HostConfig;
  custom: boolean;
  host: string;
}

const CACHE_TTL_MS = 5 * 60 * 1000;
const FETCH_TIMEOUT_MS = 3000;

interface CacheEntry {
  cfg: HostConfig;
  at: number;
}

const configCache = new Map<string, CacheEntry>();

function juicebackUrl(): string {
  return (
    process.env.JUICEBACK_URL ||
    import.meta.env.JUICEBACK_URL ||
    "http://127.0.0.1:6401"
  );
}

/**
 * Fetch the cobalt service-domain list from juiceback (server-side).
 * Returns [] when cobalt is disabled or unreachable; never throws.
 */
export async function fetchCobaltServices(): Promise<string[]> {
  try {
    const res = await fetch(`${juicebackUrl()}/api/fetch/services`, {
      signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      headers: { Accept: "application/json" },
    });
    if (!res.ok) return [];
    const data = (await res.json()) as { services?: unknown };
    if (!Array.isArray(data.services)) return [];
    return data.services.filter(
      (s): s is string => typeof s === "string" && s.length > 0,
    );
  } catch {
    return [];
  }
}

/** Normalize a user-supplied host into a base URL, or null when invalid. */
export function normalizeHost(host: string): string | null {
  let h = host.trim();
  if (!h) return null;
  if (!/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(h)) h = `https://${h}`;
  let url: URL;
  try {
    url = new URL(h);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  url.pathname = "/";
  url.search = "";
  url.hash = "";
  return url.toString().replace(/\/$/, "");
}

const PRIVATE_HOSTNAME_RE =
  /^(localhost|.*\.local|.*\.internal|127\.\d+\.\d+\.\d+|10\.\d+\.\d+\.\d+|172\.(1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+|169\.254\.\d+\.\d+)$/i;

/** Guard against SSR fetches to loopback / link-local / private addresses. */
function isBlockedHost(base: string): boolean {
  try {
    const url = new URL(base);
    const hostname = url.hostname.replace(/^\[|\]$/g, "");
    return PRIVATE_HOSTNAME_RE.test(hostname);
  } catch {
    return true;
  }
}

function sanitizeConfig(raw: unknown): HostConfig | null {
  if (typeof raw !== "object" || raw === null) return null;
  const o = raw as Record<string, unknown>;
  const cfg: HostConfig = {};

  if (typeof o.max_file_size_bytes === "number" && Number.isFinite(o.max_file_size_bytes))
    cfg.max_file_size_bytes = o.max_file_size_bytes;
  if (typeof o.max_ttl_hours === "number" && Number.isFinite(o.max_ttl_hours))
    cfg.max_ttl_hours = o.max_ttl_hours;
  if (typeof o.default_ttl_hours === "number" && Number.isFinite(o.default_ttl_hours))
    cfg.default_ttl_hours = o.default_ttl_hours;
  if (Array.isArray(o.allowed_ttl_hours)) {
    const allowed = o.allowed_ttl_hours.filter(
      (v): v is number => typeof v === "number" && Number.isFinite(v),
    );
    if (allowed.length > 0) cfg.allowed_ttl_hours = allowed;
  }
  if (typeof o.danger_level === "string") cfg.danger_level = o.danger_level;
  if (typeof o.quick_link === "boolean") cfg.quick_link = o.quick_link;
  if (typeof o.custom_id === "boolean") cfg.custom_id = o.custom_id;
  if (typeof o.ultrafast === "boolean") cfg.ultrafast = o.ultrafast;
  if (typeof o.quic === "boolean") cfg.quic = o.quic;
  if (typeof o.cobalt === "boolean") cfg.cobalt = o.cobalt;
  if (typeof o.upload_mode === "string") cfg.upload_mode = o.upload_mode;
  if (typeof o.public_base_url === "string") cfg.public_base_url = o.public_base_url;

  if (
    cfg.max_file_size_bytes === undefined &&
    cfg.allowed_ttl_hours === undefined &&
    cfg.default_ttl_hours === undefined
  ) {
    return null;
  }
  return cfg;
}

/**
 * Fetch (with cache) `/api/config` from a juicehost. `allowPrivate` is only
 * set for the configured default provider; user-supplied hosts must be public.
 */
export async function fetchHostConfig(
  host: string,
  allowPrivate = false,
): Promise<HostConfig | null> {
  const base = normalizeHost(host);
  if (!base) return null;
  if (!allowPrivate && isBlockedHost(base)) return null;

  const cached = configCache.get(base);
  if (cached && Date.now() - cached.at < CACHE_TTL_MS) return cached.cfg;

  try {
    const res = await fetch(`${base}/api/config`, {
      signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      headers: { Accept: "application/json" },
    });
    if (!res.ok) return cached?.cfg ?? null;
    const cfg = sanitizeConfig(await res.json());
    if (!cfg) return cached?.cfg ?? null;
    configCache.set(base, { cfg, at: Date.now() });
    return cfg;
  } catch {
    return cached?.cfg ?? null;
  }
}

/** Read the saved custom juicehost from the request's Cookie header. */
export function readCookieHost(cookieHeader?: string | null): string | null {
  if (!cookieHeader) return null;
  for (const part of cookieHeader.split(";")) {
    const eq = part.indexOf("=");
    if (eq === -1) continue;
    const name = part.slice(0, eq).trim();
    if (name !== "juicebox_host") continue;
    const value = part.slice(eq + 1).trim();
    if (!value) return null;
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }
  return null;
}

/**
 * Resolve the config that should drive SSR: the user's saved custom juicehost
 * when available, otherwise the default juiceback provider.
 */
export async function resolveEffectiveConfig(
  cookieHost?: string | null,
): Promise<EffectiveHostConfig> {
  const def = await fetchHostConfig(juicebackUrl(), true);
  if (cookieHost) {
    const custom = await fetchHostConfig(cookieHost);
    if (custom) {
      // Cobalt transcoding is a service of THIS box (juiceback /api/fetch), not
      // of the storage host. When a custom host's config omits the flag, inherit
      // the default provider's value instead of silently dropping the feature
      // (which hid the Video Upload tab for users with a saved host cookie).
      if (custom.cobalt === undefined && def?.cobalt !== undefined) {
        custom.cobalt = def.cobalt;
      }
      // Same for the public base URL: juicehost configs don't carry it, but
      // the page needs the TRUE public edge for share links and for the
      // self-host warning in the host modal. Without this, a saved custom
      // host blanks the baseline the warning compares against.
      if (custom.public_base_url === undefined && def?.public_base_url !== undefined) {
        custom.public_base_url = def.public_base_url;
      }
      // NOTE: upload_mode is deliberately NOT inherited here. It describes
      // the default provider's ticket capability; a genuine third-party
      // storage host may not speak direct tickets, and TUS (standard) is
      // the only mode that honors custom hosts.
      return {
        config: custom,
        custom: true,
        host: normalizeHost(cookieHost) ?? cookieHost,
      };
    }
  }
  return { config: def ?? {}, custom: false, host: juicebackUrl() };
}

const KNOWN_PROVIDERS: string[] = (() => {
  const extras = (process.env.JUICEFRONT_KNOWN_HOSTS || "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  return [...new Set([juicebackUrl(), ...extras])];
})();

/** Warm the cache for known providers (called once at server boot). */
export function prewarmKnownProviders(): void {
  void Promise.allSettled(
    KNOWN_PROVIDERS.map((host) => fetchHostConfig(host, true)),
  );
}

// Fire-and-forget prewarm when this module loads inside the Node server
// (it is never imported on the client, and never executed at build time).
if (typeof window === "undefined") {
  prewarmKnownProviders();
}
