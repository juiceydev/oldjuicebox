/** Upload configuration constants. */
export const UPLOAD_URL = import.meta.env.PUBLIC_API_URL || "";

/** Uploader engine build marker, echoed in netdebug exports so a capture
 * proves which client logic produced it. Bump on any upload-engine/config
 * behavior change. */
export const UPLOAD_ENGINE_BUILD = "2026-09-13F";

/** Read max file size in bytes from the server config embedded in the DOM. */
export function readMaxFileSize(): number {
  if (typeof document === "undefined") return 524288000;
  const el = document.getElementById("server-config");
  const val = el?.getAttribute("data-max-file-size-bytes");
  const n = val ? Number(val) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 524288000;
}

/** Read allowed TTL hours from the server config embedded in the DOM. */
export function readAllowedTtlHours(): number[] {
  if (typeof document === "undefined") return [0.5, 1, 6, 12, 24, 72, 168];
  const el = document.getElementById("server-config");
  const val = el?.getAttribute("data-allowed-ttl-hours");
  if (!val) return [0.5, 1, 6, 12, 24, 72, 168];
  try {
    const arr = JSON.parse(val);
    if (Array.isArray(arr) && arr.length > 0) return arr;
  } catch {}
  return [0.5, 1, 6, 12, 24, 72, 168];
}

/** Read default TTL hours from the server config embedded in the DOM. */
export function readDefaultTtlHours(): number {
  if (typeof document === "undefined") return 24;
  const el = document.getElementById("server-config");
  const val = el?.getAttribute("data-default-ttl-hours");
  const n = val ? Number(val) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 24;
}

export function readUploadMode(): string {
  if (typeof document === "undefined") return "standard";
  return (
    document.getElementById("server-config")?.getAttribute("data-upload-mode") ||
    "standard"
  );
}

/** Files above this size (in bytes) use parallel uploads (TUS parts or
 * direct-parallel parts). Kept well under Cloudflare's 100 MB request body
 * cap so no single request ever hits it. */
export const TUS_THRESHOLD = 16 * 1024 * 1024;

/** Adaptive chunking: per-stream chunks resize after each completed chunk so
 * one lands roughly every TUS_TARGET_CHUNK_SECS (bounded for sanity).
 * The 1 MiB floor matters: slow uplinks must be able to shrink chunks until
 * a single chunk transmits well inside proxied-request time budgets
 * (Cloudflare kills idle-capped requests around ~100s); an 8 MiB floor
 * would doom every chunk on a ~100 KiB/s link. */
export const TUS_START_CHUNK = 16 * 1024 * 1024;
export const TUS_MIN_CHUNK = 1 * 1024 * 1024;
export const TUS_MAX_CHUNK = 128 * 1024 * 1024;
export const TUS_TARGET_CHUNK_SECS = 4;

/** Parallel streams adapt to throughput. Parts must stay worth their
 * overhead: never spawn a stream that gets less than TUS_MIN_PART bytes.
 * We create MORE parts than max streams so the realtime tuner has headroom
 * to add workers mid-upload without re-slicing anything. Big files scale
 * parts up aggressively: aggregate TCP throughput grows with concurrent
 * connections on high-RTT/lossy paths. */
export const PARALLEL_STREAMS = 10;
export const TUS_MIN_PART = 8 * 1024 * 1024;
export const TUS_MAX_PARTS = 64;

/** How many chunks stay in flight per worker. The server commits offsets
 * sequentially, so a pipelined chunk can briefly lose an offset race and
 * get a 409; patchChunk retries those. Window >1 keeps the wire busy while
 * the previous chunk's ack round-trips. */
export const PIPELINE_WINDOW = 2;

/** Last measured aggregate upload rate (bytes/sec), persisted between
 * uploads so stream count starts near-optimal instead of guessing. */
export const LAST_SPEED_KEY = "juicebox_last_upload_bps";

/** Realtime stream tuner cadence: aggregate throughput is sampled every
 * TUNER_INTERVAL_MS and extra streams are promoted while the measured rate
 * keeps climbing (with periodic re-probes once saturated). */
export const TUNER_INTERVAL_MS = 1200;

/** Workers spawned immediately per upload; the tuner adds the rest one per
 * tick so the link is measured before the pool fills. Starts at 4 so fast
 * links skip the slowest ramp steps; the tuner still sheds on thin pipes. */
export const INITIAL_WORKERS = 4;

/** Times a part may be attempted across recovery sweeps before its failure
 * is treated as fatal to the upload. */
export const MAX_PART_RUNS = 2;

/** Pick a stream count from an estimated rate in bytes/sec. Slow links get
 * more streams to fill the pipe; fast links need fewer (less overhead). */
export function streamsForRate(bytesPerSec: number): number {
  const mbps = (bytesPerSec * 8) / 1e6;
  if (mbps < 10) return 8;
  if (mbps < 40) return 6;
  if (mbps < 150) return 4;
  return 3;
}

/** Connection classification for cold-start tuning (no measurement yet).
 * NOTE: navigator.connection.type lies routinely (VPNs, Android reporting
 * wifi on LTE), so quality signals outrank medium claims, and the live
 * tuner corrects any wrong guess within seconds anyway. */
export type NetTier = "cellular" | "wifi" | "ethernet";

export function detectNetTier(): NetTier {
  try {
    const c = (
      navigator as unknown as {
        connection?: {
          type?: string;
          effectiveType?: string;
          downlink?: number;
          saveData?: boolean;
        };
      }
    ).connection;
    if (!c) return "wifi";
    if (c.saveData) return "cellular";
    // Quality first: effectiveType is measured, not claimed.
    if (
      c.effectiveType === "slow-2g" ||
      c.effectiveType === "2g" ||
      c.effectiveType === "3g"
    )
      return "cellular";
    if (c.type === "cellular" || c.type === "wimax") return "cellular";
    if (c.type === "ethernet") return "ethernet";
    // Claimed wifi (or unknown) but throughput looks like mobile data.
    if (typeof c.downlink === "number" && c.downlink > 0 && c.downlink < 8)
      return "cellular";
    return "wifi";
  } catch {
    return "wifi";
  }
}

/** Cold-start rate guesses (bytes/sec) per tier when nothing is measured yet. */
export const TIER_DEFAULT_BPS: Record<NetTier, number> = {
  cellular: 1500 * 1024,
  wifi: 12 * 1024 * 1024,
  ethernet: 40 * 1024 * 1024,
};

/** Chunk profile per tier: cellular keeps chunks smaller (loss granularity,
 * flaky radios); ethernet goes big (cheap bandwidth, tiny RTT). */
export const TIER_CHUNK: Record<NetTier, { start: number; max: number }> = {
  cellular: { start: 8 * 1024 * 1024, max: 32 * 1024 * 1024 },
  wifi: { start: 16 * 1024 * 1024, max: 96 * 1024 * 1024 },
  ethernet: { start: 32 * 1024 * 1024, max: 128 * 1024 * 1024 },
};

/** Endpoint for reserving an upload slot before streaming. */
export const RESERVE_URL = `${UPLOAD_URL}/upload/reserve`;

/** Endpoint for UltraFast reserve (delegates to juicebox-plus device). */
export const ULTRAFAST_RESERVE_URL = `${UPLOAD_URL}/upload/ultrafast/reserve`;

/** Endpoint for reserving a browser-direct upload ticket. */
export const DIRECT_RESERVE_URL = `${UPLOAD_URL}/upload/direct/reserve`;

/** Endpoint for completing a browser-direct upload ticket. */
export const DIRECT_COMPLETE_URL = `${UPLOAD_URL}/upload/direct/complete`;

/** Endpoint for reserving a parallel browser-direct upload (one ticket per
 * part, bytes go straight to juicehost). */
export const DIRECT_PARALLEL_RESERVE_URL = `${UPLOAD_URL}/upload/direct/parallel/reserve`;

/** Endpoint for completing a parallel browser-direct upload (server-side
 * concat + finalize). */
export const DIRECT_PARALLEL_COMPLETE_URL = `${UPLOAD_URL}/upload/direct/parallel/complete`;

/** Files at/above this size use parallel direct parts in direct-prefer
 * mode (below it, a single direct PUT is cheaper than the reserve/concat
 * round trips). Parts stay small enough for proxied upload caps. */
export const DIRECT_PARALLEL_THRESHOLD = 16 * 1024 * 1024;

/** Target part size for parallel direct uploads; the server rebalances to
 * keep every part under its proxied-upload part cap. */
export const DIRECT_PART_TARGET = 32 * 1024 * 1024;

/** Max parts per parallel direct upload (server hard cap is 64). */
export const DIRECT_MAX_PARTS = 32;

/** Extensions eligible for gzip compression (text-like content). */
export const TEXT_LIKE_RE = /\.(txt|json|csv|tsv|xml|html|css|js|ts|jsx|tsx|md|yaml|yml|toml|ini|conf|sh|bash|zsh|fish|py|rb|pl|php|java|c|h|cpp|hpp|rs|go|swift|kt|sql|log|diff|patch|srt|vtt|sub|cfg|env|gitignore|dockerignore|editorconfig|prettierrc|eslintrc|babelrc|Makefile|CMakeLists|Gemfile|Podfile|Dockerfile|svg|wasm|epub|tex|rst|graphql|proto)$/i;

/** MIME types that indicate compressible content even when the extension
 * is missing or unusual. */
const TEXT_MIME_RE = /^text\/|application\/(json|xml|javascript|x-yaml|yaml|svg\+xml|wasm)/i;

/** Single source of truth for gzip eligibility (extension OR mime). */
export function gzipEligible(filename: string, mimeType?: string): boolean {
  return (
    TEXT_LIKE_RE.test(filename) || (!!mimeType && TEXT_MIME_RE.test(mimeType))
  );
}

/** MIME types that are already compressed where gzip gains nothing, so the
 * first-chunk probe can be skipped and chunk 0 goes out raw immediately. */
const COMPRESSED_MIME_RE =
  /^(image|video|audio)\/|application\/(pdf|x-7z-compressed|zip|gzip|x-rar-compressed|x-tar|x-bzip2|x-compressed-tar|xz)$/i;

/** Known-compressed file extensions (media/archives) for when no mime type
 * was provided. */
const COMPRESSED_EXT_RE =
  /\.(jpe?g|jfif|png|gif|webp|heic|heif|avif|bmp|tiff?|mp4|m4v|mkv|mov|webm|avi|wmv|flv|m4a|mp3|aac|oga|ogg|opus|wav|flac|zip|7z|rar|gz|bz2|xz|tgz|pdf|woff2?|ttf|otf|eot)$/i;

/** Media/archives are known-compressed: skip the gzip probe entirely. */
export function isKnownCompressed(filename: string, mimeType?: string): boolean {
  return (
    (!!mimeType && COMPRESSED_MIME_RE.test(mimeType)) ||
    (!!filename && COMPRESSED_EXT_RE.test(filename))
  );
}
