import {
  UPLOAD_URL,
  TUS_THRESHOLD,
  TUS_MIN_CHUNK,
  TUS_TARGET_CHUNK_SECS,
  PARALLEL_STREAMS,
  TUS_MIN_PART,
  TUS_MAX_PARTS,
  PIPELINE_WINDOW,
  TUNER_INTERVAL_MS,
  INITIAL_WORKERS,
  MAX_PART_RUNS,
  LAST_SPEED_KEY,
  streamsForRate,
  detectNetTier,
  TIER_DEFAULT_BPS,
  TIER_CHUNK,
  type NetTier,
  gzipEligible,
  isKnownCompressed,
  RESERVE_URL,
  DIRECT_RESERVE_URL,
  DIRECT_COMPLETE_URL,
  DIRECT_PARALLEL_RESERVE_URL,
  DIRECT_PARALLEL_COMPLETE_URL,
  DIRECT_PARALLEL_THRESHOLD,
  DIRECT_PART_TARGET,
  DIRECT_MAX_PARTS,
  readUploadMode,
} from "./upload-config";
import { tryAcquireStream, releaseStream } from "./stream-limiter";
import { compressFile } from "./compress";
import { encodeTusMeta } from "./tus";
import {
  logUploadError,
  parseErrorBody,
  classifyFetchError,
} from "./errors";

export type UploadMethod = "direct" | "tus";

export type UploadState =
  | "queued"
  | "compressing"
  | "uploading"
  | "finalizing"
  | "done"
  | "error"
  | "cancelled";

export interface UploadOptions {
  ttlHours: number;
  customHost: string;
  uploadMode: string;
  quickLink: boolean;
}

/** Live tuner telemetry attached by the TUS orchestrator (debug overlay). */
export interface TunerDbg {
  w: number; // active workers
  t: number; // target workers
  sat: boolean; // link saturated?
  r: number; // last tuner window rate, bytes/sec
  c: number; // parts claimed so far
  n: number; // total parts
  k: number; // chunk size currently in effect, bytes
  g: boolean | null; // gzip active? null while first-chunk probe undecided
}

export interface UploadItem {
  id: string;
  filename: string;
  mimeType: string;
  size: number;
  method: UploadMethod;
  state: UploadState;
  progress: number;
  ttlHours: number;
  quickLink: boolean;
  customHost: string;
  uploadMode: string;
  serverId?: string;
  url?: string;
  deleteToken?: string;
  reserveUrl?: string;
  expiresAt?: number;
  errorCode?: string;
  errorMessage?: string;
  /** HTTP status that produced the error, when known. */
  errorStatus?: number;
  /** Pipeline stage that failed (direct-upload, direct-reserve, tus-create, ...). */
  errorStage?: string;
  createdAt: number;
  dbg?: TunerDbg;
}

/** Report an upload failure with a structured console log + state update.
 * Every error path must go through here so failures are never silent and
 * never collapse to a bare "Network error" without context. */
function failUpload(
  update: UploadControls["update"],
  item: Pick<UploadItem, "id" | "filename" | "size" | "method">,
  opts: {
    code?: string;
    message?: string;
    status?: number;
    stage: string;
    url?: string;
    cause?: unknown;
  },
): void {
  const code = opts.code || "UPLOAD_FAILED";
  logUploadError({
    code,
    message: opts.message,
    status: opts.status,
    stage: opts.stage,
    filename: item.filename,
    size: item.size,
    method: item.method,
    url: opts.url,
    cause: opts.cause,
  });
  update({
    id: item.id,
    errorCode: code,
    errorMessage: opts.message,
    errorStatus: opts.status,
    errorStage: opts.stage,
    state: "error",
  });
}

export function extractServerId(url: string): string {
  try {
    const m = url.match(/\/f\/([A-Za-z0-9_-]+)/);
    return m ? m[1] : "";
  } catch {
    return "";
  }
}

export interface UploadControls {
  update: (patch: Partial<UploadItem> & { id: string }) => void;
  onTusCreate: (tusId: string) => void;
  onTusDelete: (tusId: string) => void;
}

export interface UploadHandle {
  abort: () => void;
}

export function generateId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

function isTransientStatus(status: number): boolean {
  return status === 0 || (status >= 520 && status <= 524) || status === 502 || status === 503 || status === 504;
}

async function fetchWithRetry(
  input: RequestInfo | URL,
  init: RequestInit,
  maxRetries = 3,
  baseDelayMs = 1000,
): Promise<Response> {
  let lastErr: unknown;
  for (let attempt = 0; attempt <= maxRetries; attempt++) {
    try {
      const res = await fetch(input, init);
      if (res.ok || !isTransientStatus(res.status) || attempt === maxRetries) {
        return res;
      }
      lastErr = new Error(`HTTP ${res.status}`);
    } catch (err) {
      lastErr = err;
      if (attempt === maxRetries) throw err;
    }
    const delay = baseDelayMs * Math.pow(2, attempt) + Math.random() * 500;
    await new Promise((r) => setTimeout(r, delay));
  }
  throw lastErr;
}

/** Create a TUS session; retries once past a stale "part index already
 * exists" rejection and reports whether the part is already registered. */
async function createTusSession(
  input: RequestInfo | URL,
  init: RequestInit,
): Promise<{ res: Response; alreadyExists: boolean }> {
  const readText = async (r: Response) => r.text().catch(() => "");
  const echo = (r: Response, text: string) =>
    new Response(text, { status: r.status, headers: r.headers });
  let res = await fetch(input, init);
  if (res.status === 400) {
    const text = await readText(res);
    if (!/already exists/i.test(text)) {
      return { res: echo(res, text), alreadyExists: false };
    }
    // Server releases dead slots asynchronously; one short retry wins that race.
    await new Promise((r) => setTimeout(r, 500));
    res = await fetch(input, init);
    if (res.status === 400) {
      const text2 = await readText(res);
      return {
        res: echo(res, text2),
        alreadyExists: /already exists/i.test(text2),
      };
    }
  }
  return { res, alreadyExists: false };
}

interface RunState {
  cancelled: boolean;
}

export function startUpload(
  item: UploadItem,
  file: File,
  controls: UploadControls,
): UploadHandle {
  if (readUploadMode() === "direct-prefer") {
    return file.size >= DIRECT_PARALLEL_THRESHOLD
      ? startDirectParallelUpload(item, file, controls)
      : startDirectTicketUpload(item, file, controls);
  }
  return file.size > TUS_THRESHOLD
    ? startTusUpload(item, file, controls)
    : startDirectUpload(item, file, controls);
}

const _raf = typeof requestAnimationFrame !== "undefined" ? requestAnimationFrame : (cb: FrameRequestCallback) => setTimeout(cb, 16) as unknown as number;
const _caf = typeof cancelAnimationFrame !== "undefined" ? cancelAnimationFrame : clearTimeout;

/** Live upload progress: measures real throughput from network events and
 * projects the byte count forward every frame, so the bar moves continuously
 * at the actual transfer speed instead of stepping per backend report. */
class LiveProgress {
  private loaded = 0;
  private total = 0;
  private shown = 0;
  private lastTime = 0;
  private speed = 0;
  private raf: number | null = null;
  private callback: (pct: number) => void;

  constructor(callback: (pct: number) => void) {
    this.callback = callback;
  }

  /** Start tracking with the total byte count. */
  begin(totalBytes: number) {
    this.total = totalBytes;
    this.loaded = 0;
    this.shown = 0;
    this.speed = 0;
    this.lastTime = performance.now();
    this.emit(this.loaded);
    if (this.raf === null) this.startLoop();
  }

  /** Refine the total (e.g. estimated compressed size). The monotonic
   * shown-guard keeps the displayed percentage from ever regressing. */
  setTotal(bytes: number) {
    if (bytes > 0 && Number.isFinite(bytes)) this.total = bytes;
  }

  /** Feed the real cumulative byte count whenever the network reports. */
  set(bytes: number) {
    const now = performance.now();
    const dt = (now - this.lastTime) / 1000;
    if (dt >= 0.15 && bytes > this.loaded) {
      const inst = (bytes - this.loaded) / dt;
      // Clamp bursts (parallel chunk acks landing together) so one sample
      // can't inflate the estimate and launch the bar to 100%.
      const clamped = Math.min(inst, Math.max(this.speed * 3, 64 * 1024 * 1024));
      this.speed = this.speed ? this.speed * 0.7 + clamped * 0.3 : clamped;
    }
    if (bytes > this.loaded) {
      this.loaded = bytes;
      this.lastTime = now;
    }
    this.emit(this.loaded);
  }

  private emit(bytes: number) {
    if (this.total <= 0) return;
    const pct = Math.min((bytes / this.total) * 100, 100);
    if (!Number.isFinite(pct) || pct <= this.shown) return;
    this.shown = pct;
    this.callback(pct);
  }

  private startLoop() {
    const tick = () => {
      const dt = (performance.now() - this.lastTime) / 1000;
      if (this.speed > 0 && this.total > 0) {
        // Project at measured speed, capped ~2s past the last real report,
        // so a stalled connection freezes honestly instead of racing ahead;
        // the longer horizon bridges the gap between chunk dispatches.
        const projected = Math.min(
          this.loaded + this.speed * dt,
          this.loaded + this.speed * 2,
          this.total,
        );
        this.emit(projected);
      }
      this.raf = _raf(tick);
    };
    this.raf = _raf(tick);
  }

  destroy() {
    if (this.raf !== null) {
      _caf(this.raf);
      this.raf = null;
    }
  }
}

async function reserveQuickLink(item: UploadItem, signal?: AbortSignal) {
  const reserveRes = await fetch(RESERVE_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      filename: item.filename,
      mime_type: item.mimeType || "application/octet-stream",
      ttl_hours: Number(item.ttlHours),
      host: item.customHost || undefined,
    }),
    ...(signal ? { signal } : {}),
  });
  if (!reserveRes.ok) return null;
  const reserve = await reserveRes.json();
  return {
    id: reserve.id || "",
    url: reserve.url || "",
    deleteToken: reserve.delete_token || "",
  };
}

function startDirectUpload(
  item: UploadItem,
  file: File,
  controls: UploadControls,
): UploadHandle {
  const { update } = controls;
  const runState: RunState = { cancelled: false };
  let xhr: XMLHttpRequest | null = null;

  const run = (async () => {
    let reserveId = "";
    let reserveUrl = "";
    let reserveDeleteToken = "";
    if (item.quickLink) {
      try {
        const reserve = await reserveQuickLink(item);
        if (reserve) {
          reserveId = reserve.id;
          reserveUrl = reserve.url;
          reserveDeleteToken = reserve.deleteToken;
          if (reserveUrl) update({ id: item.id, reserveUrl, serverId: reserveId });
        }
      } catch (err) {
        // Non-fatal: fall back to a non-reserved upload, but log it so a
        // broken reserve endpoint is visible instead of silent.
        logUploadError({
          code: classifyFetchError(err),
          message: err instanceof Error ? err.message : String(err),
          stage: "quicklink-reserve",
          filename: item.filename,
          size: item.size,
          method: item.method,
          url: RESERVE_URL,
          cause: err,
        });
      }
      if (runState.cancelled) throw new Error("Upload cancelled");
    }

    update({ id: item.id, state: "compressing" });
    const { file: uploadFile, isGzip } = await compressFile(file);
    if (runState.cancelled) throw new Error("Upload cancelled");
    update({ id: item.id, state: "uploading", progress: 0 });

    const fd = new FormData();
    fd.append("ttl_hours", String(item.ttlHours));
    if (item.customHost) fd.append("host", item.customHost);
    fd.append("upload_mode", item.uploadMode);
    if (reserveId) fd.append("reserve_id", reserveId);
    fd.append("file", uploadFile);

    const progress = new LiveProgress((pct: number) => {
      // Hold just under 100% until the server responds: onprogress counts
      // socket-buffered bytes, and finalize work happens after the last one.
      // The done-state update sets exactly 100.
      update({ id: item.id, progress: Math.min(pct, 99.4) });
    });
    progress.begin(uploadFile.size);

    await new Promise<void>((resolve) => {
      const finish = () => {
        progress.destroy();
        resolve();
      };
      xhr = new XMLHttpRequest();
      xhr.open("POST", `${UPLOAD_URL}/upload`, true);
      if (isGzip) xhr.setRequestHeader("X-File-Encoding", "gzip");
      if (reserveId && reserveDeleteToken) {
        xhr.setRequestHeader("X-Delete-Token", reserveDeleteToken);
      }
      xhr.timeout = 5 * 60 * 1000;
      xhr.upload.onprogress = (ev) => {
        if (!ev.lengthComputable || !ev.total || ev.total <= 0) return;
        progress.set(ev.loaded);
      };
      xhr.onload = () => {
        if (xhr!.status >= 200 && xhr!.status < 300) {
          let res: any = {};
          try {
            res = JSON.parse(xhr!.responseText);
          } catch (err) {
            failUpload(update, item, {
              code: "BAD_RESPONSE",
              message: `Server returned ${xhr!.status} with unreadable body`,
              status: xhr!.status,
              stage: "direct-upload",
              url: `${UPLOAD_URL}/upload`,
              cause: err,
            });
            finish();
            return;
          }
          if (reserveUrl && !res.url) res.url = reserveUrl;
          if (reserveDeleteToken && !res.delete_token) res.delete_token = reserveDeleteToken;
          update({
            id: item.id,
            serverId: res.id || reserveId || "",
            url: res.url || "",
            deleteToken: res.delete_token || "",
            expiresAt: res.expires_at
              ? Number(res.expires_at)
              : Math.round(Date.now() / 1000) + item.ttlHours * 3600,
            state: "done",
            progress: 100,
          });
        } else {
          const parsed = parseErrorBody(xhr!.responseText || "");
          let code = parsed.code;
          let msg = parsed.message;
          if (xhr!.status >= 520 && xhr!.status <= 524) {
            code = code || "UPLOAD_TIMEOUT";
            msg = msg || "Connection timed out (Cloudflare). Try a smaller file or try again later.";
          }
          failUpload(update, item, {
            code,
            message: msg,
            status: xhr!.status,
            stage: "direct-upload",
            url: `${UPLOAD_URL}/upload`,
          });
        }
        finish();
      };
      xhr.onerror = () => {
        failUpload(update, item, {
          code: "NETWORK_ERROR",
          message: "Connection failed during direct upload",
          stage: "direct-upload",
          url: `${UPLOAD_URL}/upload`,
        });
        finish();
      };
      xhr.ontimeout = () => {
        failUpload(update, item, {
          code: "UPLOAD_TIMEOUT",
          message: "Upload timed out. Try a smaller file.",
          stage: "direct-upload",
          url: `${UPLOAD_URL}/upload`,
        });
        finish();
      };
      xhr.onabort = () => {
        update({ id: item.id, state: "cancelled" });
        finish();
      };
      xhr.send(fd);
    });
  })().catch((err) => {
    if (runState.cancelled || /cancel/i.test(String(err?.message ?? ""))) {
      update({ id: item.id, state: "cancelled" });
      return;
    }
    failUpload(update, item, {
      code: classifyFetchError(err),
      message: err?.message || "Direct upload failed before a response",
      stage: "direct-upload",
      url: `${UPLOAD_URL}/upload`,
      cause: err,
    });
  });

  return {
    abort: () => {
      runState.cancelled = true;
      xhr?.abort();
    },
  };
}

/** Compress a blob with streaming gzip. */
async function gzipBlob(blob: Blob): Promise<Blob> {
  return new Response(
    blob.stream().pipeThrough(new CompressionStream("gzip")),
  ).blob();
}

function xhrSendBytes(
  uploadUrl: string,
  blob: Blob,
  ticket: string,
  onBytes: (loaded: number) => void,
  runState: RunState,
  xhrRef: { current: XMLHttpRequest | null },
): Promise<{ status: number; ok: boolean; body: string }> {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    xhrRef.current = request;
    request.open("POST", uploadUrl, true);
    request.setRequestHeader("Authorization", `Bearer ${ticket}`);
    request.setRequestHeader("Content-Type", "application/octet-stream");
    // Same stall watchdog as TUS chunks: without it a dead middlebox parks
    // the PUT forever with no progress and no error. Aborting surfaces
    // NETWORK_ERROR so callers retry (parallel parts) or show an error
    // (single PUT) instead of freezing the progress bar.
    let lastActivity = performance.now();
    let done = false;
    const stallTimer = setInterval(() => {
      if (done || runState.cancelled) return;
      if (performance.now() - lastActivity > 60_000) {
        // Don't settle here: aborting routes into onabort below, which
        // rejects so callers retry or surface an error.
        try {
          request.abort();
        } catch {}
      }
    }, 5000);
    const finish = () => {
      clearInterval(stallTimer);
      if (xhrRef.current === request) xhrRef.current = null;
    };
    request.upload.onprogress = (ev) => {
      lastActivity = performance.now();
      if (!ev.lengthComputable || !ev.total || ev.total <= 0) return;
      if (Number.isFinite(ev.loaded)) onBytes(ev.loaded);
    };
    request.onload = () => {
      if (done) return;
      done = true;
      finish();
      resolve({
        status: request.status,
        ok: request.status >= 200 && request.status < 300,
        body: request.responseText,
      });
    };
    request.onerror = () => {
      if (done) return;
      done = true;
      finish();
      reject(new Error("NETWORK_ERROR"));
    };
    request.onabort = () => {
      if (done) return;
      done = true;
      finish();
      if (runState.cancelled) reject(new Error("Upload cancelled"));
      else reject(new Error("NETWORK_ERROR"));
    };
    request.send(blob);
  });
}

function startDirectTicketUpload(
  item: UploadItem,
  file: File,
  controls: UploadControls,
): UploadHandle {
  const { update } = controls;
  const runState: RunState = { cancelled: false };
  const xhrRef: { current: XMLHttpRequest | null } = { current: null };

  const run = (async () => {
    const attempt = async (isRetry: boolean): Promise<void> => {
      // 1. Reserve a direct upload ticket (size-based TTL under DTE).
      let reserveRes: Response;
      try {
        reserveRes = await fetch(DIRECT_RESERVE_URL, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            filename: item.filename,
            mime_type: item.mimeType || "application/octet-stream",
            file_size: file.size,
            ttl_hours: Number(item.ttlHours),
          }),
        });
      } catch (err) {
        if (runState.cancelled) throw err;
        failUpload(update, item, {
          code: classifyFetchError(err),
          message: err instanceof Error ? err.message : "Reserve request failed",
          stage: "direct-reserve",
          url: DIRECT_RESERVE_URL,
          cause: err,
        });
        return;
      }
      if (runState.cancelled) throw new Error("Upload cancelled");
      if (!reserveRes.ok) {
        const text = await reserveRes.text().catch(() => "");
        const parsed = parseErrorBody(text);
        failUpload(update, item, {
          code: parsed.code,
          message: parsed.message,
          status: reserveRes.status,
          stage: "direct-reserve",
          url: DIRECT_RESERVE_URL,
        });
        return;
      }
      const reserve = await reserveRes.json();
      const ticket: string = reserve.ticket || "";
      const uploadUrl: string = reserve.upload_url || "";
      const fileId: string = reserve.file_id || "";
      const shareUrl: string = reserve.url || "";
      if (!ticket || !uploadUrl || !fileId) {
        failUpload(update, item, {
          code: "RESERVE_FAILED",
          message: "Reserve response was missing ticket/upload_url/file_id",
          stage: "direct-reserve",
          url: DIRECT_RESERVE_URL,
        });
        return;
      }
      if (shareUrl) update({ id: item.id, reserveUrl: shareUrl, serverId: fileId });
      update({ id: item.id, state: "uploading", progress: 0 });

      const progress = new LiveProgress((pct: number) => {
        update({ id: item.id, progress: Math.min(pct, 99.4) });
      });
      progress.begin(file.size);
      let uploadStatus: { status: number; ok: boolean; body: string };
      try {
        uploadStatus = await xhrSendBytes(
          uploadUrl,
          file,
          ticket,
          (loaded) => progress.set(loaded),
          runState,
          xhrRef,
        );
      } catch (err: any) {
        progress.destroy();
        if (runState.cancelled) throw err;
        failUpload(update, item, {
          code: classifyFetchError(err),
          message: err?.message || "Direct byte upload failed",
          stage: "direct-put",
          url: uploadUrl,
          cause: err,
        });
        return;
      }
      progress.destroy();
      if (runState.cancelled) throw new Error("Upload cancelled");

      // Ticket expired between reserve and upload start: re-reserve + retry once.
      if ((uploadStatus.status === 401 || uploadStatus.status === 403) && !isRetry) {
        attempt(true);
        return;
      }
      if (!uploadStatus.ok) {
        const parsed = parseErrorBody(uploadStatus.body || "");
        failUpload(update, item, {
          code: parsed.code,
          message: parsed.message,
          status: uploadStatus.status,
          stage: "direct-put",
          url: uploadUrl,
        });
        return;
      }

      // 3. Complete: resolve ownership server-side.
      update({ id: item.id, state: "finalizing" });
      let completeRes: Response;
      try {
        completeRes = await fetch(DIRECT_COMPLETE_URL, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ file_id: fileId, ticket }),
        });
      } catch (err) {
        if (runState.cancelled) throw err;
        failUpload(update, item, {
          code: classifyFetchError(err),
          message: err instanceof Error ? err.message : "Complete request failed",
          stage: "direct-complete",
          url: DIRECT_COMPLETE_URL,
          cause: err,
        });
        return;
      }
      if (runState.cancelled) throw new Error("Upload cancelled");

      // Complete-side ticket rejection: re-run the whole flow once.
      if ((completeRes.status === 401 || completeRes.status === 403) && !isRetry) {
        attempt(true);
        return;
      }
      if (!completeRes.ok) {
        const text = await completeRes.text().catch(() => "");
        const parsed = parseErrorBody(text);
        failUpload(update, item, {
          code: parsed.code,
          message: parsed.message,
          status: completeRes.status,
          stage: "direct-complete",
          url: DIRECT_COMPLETE_URL,
        });
        return;
      }
      const data = await completeRes.json();
      update({
        id: item.id,
        serverId: data.id || fileId,
        url: data.url || shareUrl,
        deleteToken: data.delete_token || "",
        expiresAt: data.expires_at
          ? Number(data.expires_at)
          : Math.round(Date.now() / 1000) + item.ttlHours * 3600,
        state: "done",
        progress: 100,
      });
    };

    await attempt(false);
  })().catch((err) => {
    if (runState.cancelled || /cancel/i.test(String(err?.message ?? ""))) {
      update({ id: item.id, state: "cancelled" });
      return;
    }
    failUpload(update, item, {
      code: classifyFetchError(err),
      message: err?.message || "Direct ticket upload failed",
      stage: "direct-ticket",
      cause: err,
    });
  });

  return {
    abort: () => {
      runState.cancelled = true;
      xhrRef.current?.abort();
    },
  };
}

interface DirectParallelPart {
  part_index: number;
  part_id: string;
  part_size: number;
  ticket: string;
  upload_url: string;
}

/** Parallel browser-direct upload: parts go straight to juicehost over
 * concurrent PUTs (no bytes via juiceback), then the server concats +
 * finalizes. Part sizes are authoritative from the reserve response; the
 * client slices with the same ceil-division the server used. A part PUT is
 * idempotent by content: 409 means the bytes are already stored.
 *
 * Bytes are always sent raw (the ticket endpoint enforces exact sizes, so
 * no gzip here unlike the TUS path). */
function startDirectParallelUpload(
  item: UploadItem,
  file: File,
  controls: UploadControls,
): UploadHandle {
  const { update } = controls;
  const runState: RunState = { cancelled: false };
  const liveXhr = new Set<XMLHttpRequest>();
  const trackRef = (ref: { current: XMLHttpRequest | null }) => {
    if (ref.current) {
      liveXhr.add(ref.current);
      ref.current.addEventListener("loadend", () => {
        if (ref.current) liveXhr.delete(ref.current);
      });
    }
  };

  const run = (async () => {
    // Size parts from the last measured rate when known: a part should
    // transmit in ~30s so no single PUT outlives proxied-request time
    // budgets on slow uplinks. Unknown links use the 32 MiB default.
    let hintBps = 0;
    try {
      const stored = Number(localStorage.getItem(LAST_SPEED_KEY));
      if (Number.isFinite(stored) && stored > 0) hintBps = stored;
    } catch {}
    const partTarget = hintBps > 0
      ? Math.min(
          DIRECT_PART_TARGET,
          Math.max(4 * 1024 * 1024, hintBps * 30),
        )
      : DIRECT_PART_TARGET;
    // 1. Reserve one ticket per part.
    const numParts = Math.max(
      2,
      Math.min(DIRECT_MAX_PARTS, Math.ceil(file.size / partTarget)),
    );
    let reserveRes: Response;
    try {
      reserveRes = await fetch(DIRECT_PARALLEL_RESERVE_URL, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          filename: item.filename,
          mime_type: item.mimeType || "application/octet-stream",
          file_size: file.size,
          ttl_hours: Number(item.ttlHours),
          num_parts: numParts,
        }),
      });
    } catch (err) {
      failUpload(update, item, {
        code: classifyFetchError(err),
        message: err instanceof Error ? err.message : "Parallel reserve request failed",
        stage: "direct-parallel-reserve",
        url: DIRECT_PARALLEL_RESERVE_URL,
        cause: err,
      });
      return;
    }
    if (runState.cancelled) throw new Error("Upload cancelled");
    if (!reserveRes.ok) {
      const text = await reserveRes.text().catch(() => "");
      const parsed = parseErrorBody(text);
      failUpload(update, item, {
        code: parsed.code,
        message: parsed.message,
        status: reserveRes.status,
        stage: "direct-parallel-reserve",
        url: DIRECT_PARALLEL_RESERVE_URL,
      });
      return;
    }
    const reserve = await reserveRes.json();
    const parentTicket: string = reserve.ticket || "";
    const fileId: string = reserve.file_id || "";
    const shareUrl: string = reserve.url || "";
    const parts: DirectParallelPart[] = Array.isArray(reserve.parts)
      ? reserve.parts
      : [];
    // The server rebalances part counts/sizes (upload caps); slice by what
    // it returned and verify the pieces cover the file exactly.
    let splitOk =
      !!parentTicket && !!fileId && parts.length >= 2 && parts.length <= 64;
    let cursor = 0;
    const slices: { start: number; end: number }[] = [];
    if (splitOk) {
      for (const p of parts) {
        if (
          !p ||
          typeof p.part_size !== "number" ||
          !(p.part_size > 0) ||
          !p.ticket ||
          !p.upload_url ||
          !p.part_id
        ) {
          splitOk = false;
          break;
        }
        slices.push({ start: cursor, end: cursor + p.part_size });
        cursor += p.part_size;
      }
      if (cursor !== file.size) splitOk = false;
    }
    if (!splitOk) {
      failUpload(update, item, {
        code: "RESERVE_FAILED",
        message: `Server returned ${parts.length} parts covering ${cursor}/${file.size} bytes`,
        stage: "direct-parallel-reserve",
        url: DIRECT_PARALLEL_RESERVE_URL,
      });
      return;
    }
    if (shareUrl) update({ id: item.id, reserveUrl: shareUrl, serverId: fileId });
    update({ id: item.id, state: "uploading", progress: 0 });

    // 2. PUT parts with a fixed worker pool sized from the last measured
    // rate (same heuristic as TUS). Reuses the hint read above for part
    // sizing; falls back to connection/tier guesses for pool sizing only.
    if (!hintBps) {
      try {
        const conn = (
          navigator as unknown as { connection?: { downlink?: number } }
        ).connection;
        if (conn?.downlink) hintBps = (conn.downlink * 1e6) / 8;
        else hintBps = TIER_DEFAULT_BPS[detectNetTier()];
      } catch {
        hintBps = TIER_DEFAULT_BPS.wifi;
      }
    }
    const workerCount = Math.max(
      2,
      Math.min(parts.length, streamsForRate(hintBps)),
    );

    const progress = new LiveProgress((pct: number) => {
      update({ id: item.id, progress: Math.min(pct, 99.4) });
    });
    progress.begin(file.size);
    const partLoaded = new Array<number>(parts.length).fill(0);
    const setPartLoaded = (i: number, loaded: number) => {
      if (loaded > partLoaded[i]) {
        partLoaded[i] = Math.min(loaded, parts[i].part_size);
        progress.set(partLoaded.reduce((a, b) => a + b, 0));
      }
    };

    const putPart = async (index: number): Promise<void> => {
      const part = parts[index];
      const blob = file.slice(slices[index].start, slices[index].end);
      let lastErr: { code: string; message: string; status?: number } = {
        code: "NETWORK_ERROR",
        message: `part ${index}: connection failed`,
      };
      for (let attempt = 0; attempt < 3; attempt++) {
        if (runState.cancelled) throw new Error("Upload cancelled");
        if (attempt > 0) {
          await new Promise((r) =>
            setTimeout(r, 500 * 2 ** (attempt - 1) + Math.random() * 250),
          );
        }
        const ref: { current: XMLHttpRequest | null } = { current: null };
        let res: { status: number; ok: boolean; body: string };
        try {
          const sent = xhrSendBytes(
            part.upload_url,
            blob,
            part.ticket,
            (loaded) => setPartLoaded(index, loaded),
            runState,
            ref,
          );
          trackRef(ref);
          res = await sent;
        } catch (err) {
          lastErr = {
            code: classifyFetchError(err),
            message: `part ${index}: ${err instanceof Error ? err.message : "connection failed"}`,
          };
          // Attach cause for the final log via Error.cause.
          (lastErr as { cause?: unknown }).cause = err;
          continue;
        }
        if (res.ok || res.status === 409) {
          // 409 = this exact part is already stored (idempotent retry).
          setPartLoaded(index, part.part_size);
          return;
        }
        const parsed = parseErrorBody(res.body || "");
        lastErr = {
          code: parsed.code || (res.status >= 400 && res.status < 500 ? "UPLOAD_FAILED" : "NETWORK_ERROR"),
          message: parsed.message || `part ${index} rejected: HTTP ${res.status}`,
          status: res.status,
        };
        if (
          res.status === 400 ||
          res.status === 401 ||
          res.status === 403 ||
          res.status === 413
        ) {
          break;
        }
      }
      const err = new Error(lastErr.message);
      (err as unknown as { code?: string; status?: number; cause?: unknown }).code = lastErr.code;
      (err as unknown as { status?: number }).status = lastErr.status;
      throw err;
    };

    let next = 0;
    const workers: Promise<void>[] = [];
    for (let w = 0; w < workerCount; w++) {
      workers.push(
        (async () => {
          for (;;) {
            if (runState.cancelled) throw new Error("Upload cancelled");
            const index = next++;
            if (index >= parts.length) return;
            await putPart(index);
          }
        })(),
      );
    }
    try {
      await Promise.all(workers);
    } catch (err: any) {
      progress.destroy();
      if (runState.cancelled) throw err;
      failUpload(update, item, {
        code: (err as { code?: string })?.code || classifyFetchError(err),
        message: err?.message || "Parallel part upload failed",
        status: (err as { status?: number })?.status,
        stage: "direct-parallel-put",
        cause: err,
      });
      return;
    }
    progress.destroy();
    if (runState.cancelled) throw new Error("Upload cancelled");

    // 3. Complete: server verifies part sizes, concats, finalizes.
    update({ id: item.id, state: "finalizing" });
    let completeRes: Response;
    try {
      completeRes = await fetchWithRetry(
        DIRECT_PARALLEL_COMPLETE_URL,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            file_id: fileId,
            ticket: parentTicket,
            part_ids: parts.map((p) => p.part_id),
          }),
        },
        3,
        1500,
      );
    } catch (err) {
      failUpload(update, item, {
        code: classifyFetchError(err),
        message: err instanceof Error ? err.message : "Parallel complete request failed",
        stage: "direct-parallel-complete",
        url: DIRECT_PARALLEL_COMPLETE_URL,
        cause: err,
      });
      return;
    }
    if (runState.cancelled) throw new Error("Upload cancelled");
    if (!completeRes.ok) {
      const text = await completeRes.text().catch(() => "");
      const parsed = parseErrorBody(text);
      failUpload(update, item, {
        code: parsed.code,
        message: parsed.message,
        status: completeRes.status,
        stage: "direct-parallel-complete",
        url: DIRECT_PARALLEL_COMPLETE_URL,
      });
      return;
    }
    const data = await completeRes.json();
    update({
      id: item.id,
      serverId: data.id || fileId,
      url: data.url || shareUrl,
      deleteToken: data.delete_token || "",
      expiresAt: data.expires_at
        ? Number(data.expires_at)
        : Math.round(Date.now() / 1000) + item.ttlHours * 3600,
      state: "done",
      progress: 100,
    });
  })().catch((err) => {
    if (runState.cancelled || /cancel/i.test(String(err?.message ?? ""))) {
      update({ id: item.id, state: "cancelled" });
      return;
    }
    failUpload(update, item, {
      code: (err as { code?: string })?.code || classifyFetchError(err),
      message: err?.message || "Parallel direct upload failed",
      status: (err as { status?: number })?.status,
      stage: "direct-parallel",
      cause: err,
    });
  });

  return {
    abort: () => {
      runState.cancelled = true;
      liveXhr.forEach((x) => {
        try {
          x.abort();
        } catch {}
      });
      liveXhr.clear();
    },
  };
}

async function uploadTusPart(
  item: UploadItem,
  file: File,
  sessionId: string,
  partIndex: number,
  totalParts: number,
  signal: AbortSignal,
  hooks: {
    onWire: (bytes: number) => void;
    onAcked: (bytes: number) => void;
    onReset: () => void;
    onChunk?: (size: number) => void;
    onGzip?: (on: boolean) => void;
    /** Called before dispatching each chunk; may delay for pacing. */
    beforeChunk?: () => Promise<void>;
    /** Wire bytes queued for a freshly dispatched chunk. */
    onDispatch?: (bytes: number) => void;
    /** Wire bytes that will never ack (chunk failed without a response).
     * Must mirror onDispatch: anything dispatched but neither acked nor
     * abandoned inflates the pacing estimate until workers park forever. */
    onAbandoned?: (bytes: number) => void;
    /** Current smoothed aggregate rate estimate, bytes/sec. */
    rateHint?: () => number;
    /** Measured rate from a previous upload (bytes/sec), if any. Used once
     * to size the opening chunk so slow links don't open with a chunk that
     * cannot fit proxied-request time budgets. */
    initialRateHint?: number;
  },
  reserveReady: Promise<void>,
  getReserveId: () => string,
  controls: UploadControls,
  netTier: NetTier,
  probeShared: { done: boolean; use: boolean },
): Promise<{ status: number; data?: any }> {
  const partSize = Math.ceil(file.size / totalParts);
  const start = partIndex * partSize;
  const end = Math.min(start + partSize, file.size);
  const partFile = file.slice(start, end);

  // Gzip policy: text-like ext/mime are known winners and compress every
  // chunk. Media/archives are already compressed: skip the probe and send
  // chunk 0 raw immediately. Unknown types run a first-chunk probe - if the
  // compressed body saved <10% vs raw, remaining chunks go raw. Offsets stay
  // logical either way.
  const knownText = gzipEligible(item.filename, item.mimeType);
  const knownCompressed = !knownText && isKnownCompressed(item.filename, item.mimeType);
  let probeDone = knownText || knownCompressed || probeShared.done;
  let useGzip = knownText || (probeShared.done && probeShared.use);

  const meta: Record<string, string> = {
    filename: item.filename,
    mimetype: item.mimeType || "application/octet-stream",
    ttl: String(item.ttlHours),
    session_id: sessionId,
    part_index: String(partIndex),
    total_parts: String(totalParts),
  };
  if (item.customHost) meta.host = item.customHost;
  meta.upload_mode = item.uploadMode;
  const metadata = encodeTusMeta(meta);

  /** One full create + chunk-loop lifecycle for this part. */
  const runSession = async (): Promise<{ status: number; data?: any }> => {
    // Local kill switch for this session attempt: when one chunk fails hard,
    // pipelined siblings get aborted so they stop burning bandwidth. Kept
    // distinct from the user-cancel signal so internal aborts never read
    // as cancellations downstream.
    const partCtrl = new AbortController();
    const partSignal = partCtrl.signal;
    const relayCancel = () => partCtrl.abort();
    signal.addEventListener("abort", relayCancel);

    interface Slot {
      p: Promise<{ status: number; newOffset: number; text: string }>;
      rawSize: number;
      wireSize: number;
    }
    let inflight = new Map<number, Slot>();

    try {
      const created = await createTusSession(`${UPLOAD_URL}/api/tus`, {
        method: "POST",
        headers: {
          "Upload-Length": String(partFile.size),
          "Upload-Metadata": metadata,
          "Tus-Resumable": "1.0.0",
        },
        signal,
      });
      if (created.alreadyExists) {
        // A prior attempt already registered (and almost certainly stored)
        // this part; let the final concat use it instead of failing.
        return { status: 202 };
      }
      if (!created.res.ok) {
        const status = created.res.status;
        const err = new Error(`TUS create failed: HTTP ${status}`);
        (err as unknown as { code?: string; status?: number }).code = "TUS_CREATE_FAILED";
        (err as unknown as { status?: number }).status = status;
        throw err;
      }

      const location = created.res.headers.get("Location");
      if (!location) {
        const err = new Error("No TUS location in create response");
        (err as unknown as { code?: string }).code = "TUS_CREATE_FAILED";
        throw err;
      }
      const uploadUrl = `${UPLOAD_URL}${location}`;
      const tusIdMatch = location.match(/\/api\/tus\/(.+)$/);
      if (tusIdMatch) controls.onTusCreate(tusIdMatch[1]);

      /** PATCH one chunk via XHR for real-time upload.onprogress bytes.
       * Retries transient failures with backoff. */
      const patchChunk = (
        body: Blob,
        chunkOffset: number,
      ): Promise<{ status: number; newOffset: number; text: string }> =>
        new Promise((resolve, reject) => {
          let last = 0;
          const attempt = (tries: number) => {
            const x = new XMLHttpRequest();
            x.open("PATCH", uploadUrl, true);
            x.setRequestHeader("Content-Type", "application/offset+octet-stream");
            x.setRequestHeader("Upload-Offset", String(chunkOffset));
            x.setRequestHeader("Tus-Resumable", "1.0.0");
            if (useGzip || !probeDone) x.setRequestHeader("X-File-Encoding", "gzip");
            const onAbort = () => x.abort();
            partSignal.addEventListener("abort", onAbort);
            // Stall watchdog: a bare XHR has no timeout of its own and can
            // hang forever (dead middlebox, collapsed uplink) with no
            // progress and no error, parking its worker behind head-of-line
            // completion while the upload reads as frozen. 60s without a
            // single progress event routes into the normal retry path
            // (adaptation shrinks the chunk by then on degraded links), so
            // a merely slow but moving upload never trips it.
            let lastActivity = performance.now();
            let settled = false;
            const stallTimer = setInterval(() => {
              if (settled || partSignal.aborted) return;
              if (performance.now() - lastActivity > 60_000) {
                settled = true;
                clearInterval(stallTimer);
                partSignal.removeEventListener("abort", onAbort);
                try {
                  x.abort();
                } catch {}
                retry();
              }
            }, 5000);
            const cleanup = () => {
              clearInterval(stallTimer);
              partSignal.removeEventListener("abort", onAbort);
            };
            const topUp = () => {
              if (body.size > last) {
                hooks.onWire(body.size - last);
                last = body.size;
              }
            };
            x.upload.onprogress = (ev) => {
              lastActivity = performance.now();
              if (ev.lengthComputable && ev.loaded > last) {
                hooks.onWire(ev.loaded - last);
                last = ev.loaded;
              }
            };
            const retry = () => {
              cleanup();
              if (tries < 3 && !partSignal.aborted) {
                setTimeout(
                  () => attempt(tries + 1),
                  500 * 2 ** tries + Math.random() * 250,
                );
              } else {
                reject(new Error(`TUS patch failed: ${x.status || "network"}`));
              }
            };
            x.onload = () => {
              if (settled) return;
              settled = true;
              topUp();
              cleanup();
              const transient =
                x.status === 0 || x.status === 408 || x.status === 429 || x.status >= 500;
              // 409 = offset race: with a pipelined window a chunk can reach
              // the server before its predecessor commits its offset. A short
              // retry wins that race; the offset itself is still valid.
              const offsetRace = x.status === 409 && tries < 6;
              if ((transient && tries < 3) || offsetRace) {
                if (!partSignal.aborted) {
                  setTimeout(
                    () => attempt(tries + 1),
                    (offsetRace ? 150 : 500) * 2 ** tries + Math.random() * 250,
                  );
                  return;
                }
              }
              resolve({
                status: x.status,
                newOffset: Number(x.getResponseHeader("Upload-Offset")) || 0,
                text: x.responseText || "",
              });
            };
            x.onerror = retry;
            x.ontimeout = retry;
            x.onabort = () => {
              if (settled) return;
              settled = true;
              cleanup();
              reject(new Error(signal.aborted ? "Upload cancelled" : "chunk dropped"));
            };
            x.send(body);
          };
          attempt(0);
        });

      // Adaptive chunk size: chunks resize so each takes ~TUS_TARGET_CHUNK_SECS
      // at the measured wire rate, bounded by this connection tier's profile.
      // With a pipelined window, per-chunk timing underestimates link rate
      // (siblings share bandwidth), so resizing samples aggregate acked bytes
      // over time instead of one chunk's round trip.
      const profile = TIER_CHUNK[netTier];
      // Keep >=4 resize points per part so adaptation stays live and no
      // single chunk hogs a thin pipe for tens of seconds.
      const partMaxChunk = Math.max(
        TUS_MIN_CHUNK,
        Math.min(profile.max, Math.ceil(partFile.size / 4)),
      );
      // When a previous upload measured this link, open with a chunk sized
      // for ~20s at that rate: a cold 16 MiB opener on a slow uplink would
      // outlive proxied-request time budgets and die before its first ack.
      const hintStart =
        hooks.initialRateHint && hooks.initialRateHint > 0
          ? Math.max(TUS_MIN_CHUNK, hooks.initialRateHint * 20)
          : profile.start;
      let chunkSize = Math.min(profile.start, hintStart);
      hooks.onChunk?.(chunkSize);

      let adaptBytes = 0;
      let adaptSince = performance.now();
      const noteAcked = (bytes: number) => {
        hooks.onAcked(bytes);
        adaptBytes += bytes;
        const dt = (performance.now() - adaptSince) / 1000;
        if (dt >= 1 && adaptBytes > 0) {
          const rate = adaptBytes / dt;
          chunkSize = Math.round(
            Math.min(
              partMaxChunk,
              Math.max(TUS_MIN_CHUNK, rate * TUS_TARGET_CHUNK_SECS),
            ),
          );
          hooks.onChunk?.(chunkSize);
          adaptBytes = 0;
          adaptSince = performance.now();
        }
      };

      let offset = 0;
      while (offset < partFile.size || inflight.size > 0) {
        if (signal.aborted) throw new Error("Upload cancelled");
        // The first chunk flies solo so the gzip probe verdict settles before
        // any sibling has to pick a codec; afterwards keep a window of chunks
        // on the wire so one uploads while the previous ack round-trips.
        // Thin pipes stay at 1: pipelining there only feeds bufferbloat.
        const windowSize =
          probeDone && (hooks.rateHint?.() ?? 0) > 4 * 1024 * 1024
            ? PIPELINE_WINDOW
            : 1;
        while (inflight.size < windowSize && offset < partFile.size) {
          await hooks.beforeChunk?.();
          const at = offset;
          const chunkEnd = Math.min(at + chunkSize, partFile.size);
          const chunk = partFile.slice(at, chunkEnd);
          if (probeShared.done && !probeDone) {
            probeDone = true;
            useGzip = probeShared.use;
          }
          // Compress before sending so retries reuse the same wire body.
          // Unknown types always compress their FIRST chunk as a probe.
          // Dispatch is accounted when the wire body exists (post-gzip), so
          // a failed compression never inflates the pacing estimate; any
          // later terminal failure without an ack releases it via onAbandoned.
          let wireSize = chunk.size;
          const slot: Slot = {
            p: null as unknown as Slot["p"],
            rawSize: chunk.size,
            wireSize: 0,
          };
          const bodyP =
            useGzip || !probeDone ? gzipBlob(chunk) : Promise.resolve(chunk);
          slot.p = bodyP
            .then((b) => {
              wireSize = b.size;
              slot.wireSize = b.size;
              hooks.onDispatch?.(b.size);
              return patchChunk(b, at);
            })
            .catch((err) => {
              hooks.onAbandoned?.(slot.wireSize);
              throw err;
            });
          inflight.set(at, slot);
          offset = chunkEnd;
        }
        const head = inflight.entries().next();
        if (head.done) break;
        const [headOffset, slot] = head.value;
        let r: { status: number; newOffset: number; text: string };
        try {
          r = await slot.p;
        } finally {
          inflight.delete(headOffset);
        }

        if (r.status === 204) {
          // Probe verdict once the first chunk lands.
          if (!probeDone) {
            probeDone = true;
            useGzip = slot.wireSize <= slot.rawSize * 0.9;
            probeShared.done = true;
            probeShared.use = useGzip;
            hooks.onGzip?.(useGzip);
          }
          noteAcked(slot.rawSize);
        } else if (r.status >= 200 && r.status < 300) {
          noteAcked(slot.rawSize);
          // Completion response: drain still-unacked siblings, then finish.
          // Settle each sibling's pacing estimate by its actual outcome so
          // nothing lingers in the gate after this part is done. The drain
          // itself is bounded: a hung sibling XHR (no ack, no error, timers
          // throttled) must not park this worker forever — release and move
          // on; its bytes can't complete this part twice.
          const pending = [...inflight.values()];
          inflight.clear();
          const DRAIN_TIMEOUT: unique symbol = Symbol("drain-timeout");
          const outcomes = await Promise.race([
            Promise.allSettled(pending.map((s) => s.p)),
            new Promise((resolve) =>
              setTimeout(() => resolve(DRAIN_TIMEOUT), 30_000),
            ),
          ]);
          if (outcomes === DRAIN_TIMEOUT) {
            pending.forEach((s) => hooks.onAbandoned?.(s.wireSize));
          } else {
            pending.forEach((s, idx) => {
              const o = (
                outcomes as PromiseSettledResult<{
                  status: number;
                  newOffset: number;
                  text: string;
                }>[]
              )[idx];
              if (o.status === "fulfilled" && o.value.status === 204) {
                noteAcked(s.rawSize);
              } else if (o.status === "fulfilled") {
                // Settled without an ack (409/5xx/200-double): release it.
                // Rejected siblings already released via their own catch.
                hooks.onAbandoned?.(s.wireSize);
              }
            });
          }
          let data: any;
          try {
            data = JSON.parse(r.text);
          } catch {}
          return { status: 200, data };
        } else {
          hooks.onAbandoned?.(slot.wireSize);
          throw new Error(`TUS patch failed: ${r.status}`);
        }
      }

      // Final patch to signal completion
      await reserveReady;
      const finalReserveId = getReserveId();
      const finalRes = await fetchWithRetry(uploadUrl, {
        method: "PATCH",
        headers: {
          "Content-Type": "application/offset+octet-stream",
          "Upload-Offset": String(offset),
          "Tus-Resumable": "1.0.0",
          ...(finalReserveId ? { "X-Reserve-Id": finalReserveId } : {}),
        },
        body: new Blob([]),
        signal,
      });

      if (finalRes.ok) {
        const data = await finalRes.json();
        return { status: 200, data };
      }
      // 202 = part complete, waiting for others
      return { status: finalRes.status };
    } finally {
      signal.removeEventListener("abort", relayCancel);
      partCtrl.abort();
      // Swallow sibling rejections so an aborted pipeline never surfaces as
      // an unhandled rejection; the real error (or success) is already in flight.
      for (const s of inflight.values()) s.p.catch(() => {});
    }
  };

  // Session-level resilience: servers restart and background tabs stall, so
  // sessions vanish mid-upload (404) or connections die ("Failed to fetch").
  // Rebuild the session from scratch and resend this whole part; counters are
  // reset first so progress stays truthful.
  let lastError: unknown;
  for (let attemptNo = 0; attemptNo < 3; attemptNo++) {
    try {
      return await runSession();
    } catch (err) {
      if (
        signal.aborted ||
        /cancel/i.test(String((err as Error)?.message ?? ""))
      ) {
        throw err;
      }
      lastError = err;
      console.warn(
        `[juicebox:upload:tus-part-${partIndex}] attempt ${attemptNo + 1} failed (${(err as Error)?.message}); retrying with fresh session`,
        {
          file: item.filename,
          size: file.size,
          part: partIndex,
          attempt: attemptNo + 1,
          cause: err,
        },
      );
      hooks.onReset();
      await new Promise((r) =>
        setTimeout(r, 500 * 2 ** attemptNo + Math.random() * 250),
      );
    }
  }
  throw lastError;
}

function startTusUpload(
  item: UploadItem,
  file: File,
  controls: UploadControls,
): UploadHandle {
  const { update } = controls;
  const controller = new AbortController();
  const signal = controller.signal;

  const run = (async () => {
    const sessionId = generateId();
    // Shared across parts: once an unknown-type first chunk settles the gzip
    // verdict, sibling parts adopt it instead of re-running their own probe.
    const probeState = { done: false, use: false };
    // Actual gzip decision arrives per-part via onGzip after the probe.
    let curGzip: boolean | null = null;

    // Adaptive stream count. Cold start: connection tier estimate (mobile
    // / wifi / ethernet). Warm: last upload's measured rate wins.
    const tier = detectNetTier();
    let hintBps = 0;
    try {
      const stored = Number(localStorage.getItem(LAST_SPEED_KEY));
      if (Number.isFinite(stored) && stored > 0) hintBps = stored;
      else {
        const conn = (
          navigator as unknown as { connection?: { downlink?: number } }
        ).connection;
        if (conn?.downlink) hintBps = (conn.downlink * 1e6) / 8;
      }
    } catch {}
    const wantedStreams = streamsForRate(hintBps || TIER_DEFAULT_BPS[tier]);
    // Parts are pre-sliced (backend contract: indexed sessions, ordered
    // concat), but we create MORE parts than starting streams so the
    // realtime tuner can promote idle parts to live workers mid-upload.
    const numParts = Math.max(
      1,
      Math.min(
        TUS_MAX_PARTS,
        PARALLEL_STREAMS * 3,
        Math.floor(file.size / TUS_MIN_PART),
      ),
    );
    let targetWorkers = Math.min(wantedStreams, numParts);
    const uploadStart = performance.now();

    const progress = new LiveProgress((pct: number) => {
      // Hold just under 100% until the merge response lands; the done-state
      // update sets exactly 100. A bar parked at fake 100% reads as broken.
      update({ id: item.id, progress: Math.min(pct, 99.4) });
    });
    progress.begin(file.size);

    // Per-part wire/logical byte trackers; wire bytes drive progress, the
    // acked ratio shrinks the estimated compressed total as evidence lands.
    // partInflight backs the pacing gate: bytes dispatched but not yet acked.
    // It MUST be released on reset, or dead dispatches inflate the gate's
    // estimate forever and every worker parks in paceGate (frozen progress
    // with no error, since parked workers never trip the retry budget).
    const partSent = new Array<number>(numParts).fill(0);
    const partAcked = new Array<number>(numParts).fill(0);
    const partInflight = new Array<number>(numParts).fill(0);
    let learnedAcked = 0;
    const sum = (a: number[]) => a.reduce((x, y) => x + y, 0);
    const refreshTotal = () => {
      if (curGzip !== true) return;
      const ackedSum = sum(partAcked);
      const ratio = ackedSum > 0 ? sum(partSent) / ackedSum : 1;
      progress.setTotal(
        Math.min(file.size, Math.max(sum(partSent), Math.round(file.size * ratio))),
      );
    };

    update({ id: item.id, state: "uploading", progress: 0 });

    let reserveId = "";
    let reserveUrl = "";
    let reserveDeleteToken = "";
    let reserveReady: Promise<void> = Promise.resolve();
    if (item.quickLink) {
      reserveReady = (async () => {
        try {
          const reserve = await reserveQuickLink(item, signal);
          if (reserve) {
            reserveId = reserve.id;
            reserveUrl = reserve.url;
            reserveDeleteToken = reserve.deleteToken;
            if (reserveUrl)
              update({ id: item.id, reserveUrl, serverId: reserveId });
          }
        } catch (err) {
          // Non-fatal: fall back to a non-reserved upload, but log it.
          logUploadError({
            code: classifyFetchError(err),
            message: err instanceof Error ? err.message : String(err),
            stage: "quicklink-reserve",
            filename: item.filename,
            size: item.size,
            method: item.method,
            url: RESERVE_URL,
            cause: err,
          });
        }
      })();
    }

    const getReserveId = () => reserveId;

    // ---- adaptive worker pool -------------------------------------------
    // Workers claim the next unstarted part index; the tuner promotes spare
    // parts to live workers while aggregate ACKED throughput is still
    // climbing, and lets surplus workers exit when the link backs off.
    const results: ({ status: number; data?: any } | undefined)[] = new Array(
      numParts,
    );
    let cursor = 0;
    const runCounts = new Array<number>(numParts).fill(0);
    const retryQueue: number[] = [];
    let activeWorkers = 0;
    let saturated = false;
    let ticksSinceAdd = 99;
    let lastWindowRate = 0;
    let windowBytes = 0;
    let workerError: unknown;
    let curChunk = 0;
    let respawnTimer: ReturnType<typeof setTimeout> | null = null;
    // Pacing: bytes queued to the wire but not acked yet, capped at ~6s of
    // measured bandwidth so bursts never outrun the pipe's drain rate.
    let inflightEst = 0;
    let rateNow = 0;
    const paceGate = async () => {
      const cap = Math.max(
        24 * 1024 * 1024,
        Math.min(96 * 1024 * 1024, rateNow * 6),
      );
      while (!signal.aborted && inflightEst > cap) {
        await new Promise((r) => setTimeout(r, 200));
      }
    };

    const runWorker = async (): Promise<void> => {
      activeWorkers++;
      try {
        for (;;) {
          if (signal.aborted) throw new Error("Upload cancelled");
          // Shed surplus workers, but never strand work: the last live
          // worker keeps claiming while parts (or retries) remain
          // unstarted, so a simultaneous shed can't park the upload with
          // zero workers and no error. (The tuner probe would eventually
          // respawn, but a throttled background tab can delay its ticks
          // for a minute or more.)
          const workWaiting = cursor < numParts || retryQueue.length > 0;
          if (
            activeWorkers > Math.max(1, targetWorkers) &&
            (activeWorkers > 1 || !workWaiting)
          )
            return;
          const i =
            retryQueue.length > 0
              ? retryQueue.shift()!
              : cursor < numParts
                ? cursor++
                : undefined;
          if (i === undefined) return;
          runCounts[i]++;
          try {
            results[i] = await uploadTusPart(
              item,
              file,
              sessionId,
              i,
              numParts,
              signal,
              {
                onWire: (bytes) => {
                  partSent[i] += bytes;
                  // Drive the bar from bytes leaving the client. onWire fires
                  // continuously via XHR upload.onprogress, so progress moves
                  // smoothly instead of snapping whole parts at a time. The
                  // denominator is refined by refreshTotal() for gzip, and
                  // onAcked stays as a monotonic backstop near the end.
                  progress.set(sum(partSent));
                },
                onAcked: (bytes) => {
                  partAcked[i] += bytes;
                  windowBytes += bytes;
                  learnedAcked += bytes;
                  inflightEst = Math.max(0, inflightEst - bytes);
                  partInflight[i] = Math.max(0, partInflight[i] - bytes);
                  refreshTotal();
                  // Progress tracks SERVER-CONFIRMED bytes: wire bytes hit
                  // Cloudflare's buffer instantly and then freeze, which read
                  // as a stuck percentage while the real transfer continued.
                  progress.set(sum(partAcked));
                },
                onReset: () => {
                  partSent[i] = 0;
                  partAcked[i] = 0;
                  // Release this part's unacked estimate: those bytes will
                  // never ack (the session is being rebuilt), and leaving
                  // them inflates the pacing gate until workers stall.
                  inflightEst = Math.max(0, inflightEst - partInflight[i]);
                  partInflight[i] = 0;
                  refreshTotal();
                },
                onChunk: (size) => {
                  curChunk = size;
                },
                onGzip: (on) => {
                  curGzip = on;
                  refreshTotal();
                },
                beforeChunk: paceGate,
                onDispatch: (bytes) => {
                  inflightEst += bytes;
                  partInflight[i] += bytes;
                },
                onAbandoned: (bytes) => {
                  inflightEst = Math.max(0, inflightEst - bytes);
                  partInflight[i] = Math.max(0, partInflight[i] - bytes);
                },
                rateHint: () => rateNow,
                initialRateHint: hintBps,
              },
              reserveReady,
              getReserveId,
              controls,
              tier,
              probeState,
            ).catch((err) => {
              if (signal.aborted) return { status: 0 } as const;
              throw err;
            });
          } catch (err) {
            if (signal.aborted) return;
            if (runCounts[i] < MAX_PART_RUNS) {
              retryQueue.push(i);
              scheduleRespawn();
            } else {
              workerError ??= err;
            }
          }
        }
      } finally {
        activeWorkers--;
        releaseStream();
      }
    };

    const workerPromises: Promise<void>[] = [];
    const emitTuner = () =>
      update({
        id: item.id,
        dbg: {
          w: activeWorkers,
          t: targetWorkers,
          sat: saturated,
          r: lastWindowRate,
          c: Math.min(cursor, numParts),
          n: numParts,
          k: curChunk,
          g: curGzip,
        },
      });
    const spawnWorker = (): boolean => {
      if (!tryAcquireStream()) return false;
      workerPromises.push(
        runWorker().catch((err) => {
          workerError ??= err;
        }),
      );
      emitTuner();
      return true;
    };
    const scheduleRespawn = () => {
      if (respawnTimer !== null || signal.aborted) return;
      respawnTimer = setTimeout(() => {
        respawnTimer = null;
        if (signal.aborted) return;
        if (cursor < numParts || retryQueue.length > 0) spawnWorker();
      }, 800 + Math.random() * 400);
    };
    const initialWorkers = Math.max(
      1,
      Math.min(targetWorkers, numParts, INITIAL_WORKERS),
    );
    for (let k = 0; k < initialWorkers; k++) spawnWorker();

    // Realtime tuner: samples aggregate ACKED byte rate (server-confirmed
    // offsets - immune to socket-buffer bursts), grows the pool while the
    // EWMA holds near baseline, sheds workers after a sustained collapse,
    // and periodically probes for spare capacity once saturated. A new
    // stream costs a little at first (TCP slow start, shared buffers), so
    // each addition skips one tick before it's judged.
    const PROBE_EVERY_TICKS = 10; // ~12s between trials while saturated
    const SHRINK_TICKS = 3; // sustained collapse before shedding a worker
    const WARMUP_TICKS = 2; // a fresh stream is judged only after this many
    let probeTicks = 0;
    let shrinkTicks = 0;
    let preTrialRate = 0;
    let evaluatingTrial = false;
    let rateEwma = 0;
    let baselineRate = 0;
    const recentRates: number[] = [];
    const tuner = setInterval(() => {
      const rate = (windowBytes * 1000) / TUNER_INTERVAL_MS;
      windowBytes = 0;
      rateEwma = rateEwma ? rateEwma * 0.6 + rate * 0.4 : rate;
      rateNow = rateEwma;
      recentRates.push(rate);
      if (recentRates.length > 3) recentRates.shift();
      if (baselineRate === 0 && rateEwma > 0) baselineRate = rateEwma;

      // A freshly spawned stream gets a couple of ticks to ramp up.
      if (ticksSinceAdd < WARMUP_TICKS) {
        ticksSinceAdd++;
        lastWindowRate = rateEwma;
        emitTuner();
        return;
      }

      const maxConc = Math.min(PARALLEL_STREAMS, numParts);
      const workLeft = cursor < numParts || retryQueue.length > 0;
      // Judge growth on the WORST of the last few windows: a buffer-drain
      // spike that immediately decays must not read as sustainable capacity.
      const steadyMin =
        recentRates.length >= 3 ? Math.min(...recentRates) : rateEwma;

      if (evaluatingTrial) {
        // Trial verdict: keep the extra stream only if it bought real
        // throughput (>5% over pre-trial); otherwise shed it and stay
        // saturated until the next probe.
        evaluatingTrial = false;
        probeTicks = 0;
        if (rateEwma >= preTrialRate * 1.05) {
          saturated = false;
          baselineRate = rateEwma;
        } else {
          saturated = true;
          targetWorkers = Math.max(1, activeWorkers - 1);
        }
        lastWindowRate = rateEwma;
        ticksSinceAdd++;
        emitTuner();
        return;
      }

      if (
        workLeft &&
        activeWorkers < maxConc &&
        baselineRate > 0 &&
        steadyMin >= baselineRate * 0.9
      ) {
        // Climbing or holding near baseline: promote one more stream.
        shrinkTicks = 0;
        if (spawnWorker()) {
          saturated = false;
          targetWorkers = activeWorkers + 1;
          ticksSinceAdd = 0;
          baselineRate = rateEwma;
        } else {
          // Another upload holds the global permits; back off quietly.
          saturated = true;
          probeTicks = 0;
        }
      } else if (
        baselineRate > 0 &&
        rateEwma < baselineRate * 0.7 &&
        activeWorkers > 1
      ) {
        // Sustained collapse: shed a worker every few ticks so survivors
        // get a larger share instead of everyone thrashing together.
        saturated = true;
        if (++shrinkTicks >= SHRINK_TICKS) {
          shrinkTicks = 0;
          targetWorkers = activeWorkers - 1;
          baselineRate = rateEwma;
        }
      } else {
        // Flat or idle: mark saturated, but schedule periodic probes so a
        // conservative verdict never caps growth for the rest of the upload.
        shrinkTicks = 0;
        if (
          workLeft &&
          activeWorkers < maxConc &&
          baselineRate > 0 &&
          ++probeTicks >= PROBE_EVERY_TICKS
        ) {
          probeTicks = 0;
          preTrialRate = rateEwma;
          evaluatingTrial = true;
          if (spawnWorker()) {
            targetWorkers = activeWorkers + 1;
            ticksSinceAdd = 0;
          } else {
            evaluatingTrial = false;
          }
        }
      }

      lastWindowRate = rateEwma;
      ticksSinceAdd++;
      emitTuner();
    }, TUNER_INTERVAL_MS);

    for (;;) {
      const batch = workerPromises.slice();
      await Promise.all(batch);
      if (workerPromises.length <= batch.length) break;
    }
    if (respawnTimer !== null) {
      clearTimeout(respawnTimer);
      respawnTimer = null;
    }
    clearInterval(tuner);
    progress.destroy();
    if (workerError && !signal.aborted) throw workerError;

    const settled = results.filter((r): r is { status: number; data?: any } => !!r);

    // Learn: persist aggregate ACKED rate so the next upload starts with a
    // near-optimal stream count (wire bytes would bake in buffer bursts).
    const elapsedSec = (performance.now() - uploadStart) / 1000;
    if (elapsedSec > 1 && learnedAcked > 0) {
      try {
        localStorage.setItem(LAST_SPEED_KEY, String(learnedAcked / elapsedSec));
      } catch {}
    }

    // Find charlie (200)
    //
    // I know this is horrendous code.
    const merged = settled.find((r) => r.status === 200 && r.data?.url);
    if (merged && "data" in merged && merged.data) {
      const data = merged.data;
      if (reserveUrl && !data.url) data.url = reserveUrl;
      if (reserveDeleteToken && !data.delete_token) data.delete_token = reserveDeleteToken;
      update({ id: item.id, state: "finalizing" });
      update({
        id: item.id,
        serverId: data.id || reserveId || "",
        url: data.url,
        deleteToken: data.delete_token || "",
        expiresAt: data.expires_at
          ? Number(data.expires_at)
          : Math.round(Date.now() / 1000) + item.ttlHours * 3600,
        state: "done",
        progress: 100,
      });
      return;
    }

    // All returned 202..... which shouldn't happen if the server works correctly.
    failUpload(update, item, {
      code: "UPLOAD_FAILED",
      message: "Parallel upload completed but no merge response",
      stage: "tus-merge",
    });
  })().catch((err) => {
    if (signal.aborted) {
      update({ id: item.id, state: "cancelled" });
      return;
    }
    const msg = String(err?.message ?? "");
    const attachedCode = (err as { code?: string })?.code;
    // A TUS protocol failure (create/patch/offset/session) is NOT a network
    // failure: surface it as TUS_ERROR so the UI doesn't say "Network error".
    const code = attachedCode
      || (/TUS|tus|offset|session|chunk dropped|merge response/i.test(msg) ? "TUS_ERROR" : classifyFetchError(err));
    const status = (err as { status?: number })?.status;
    failUpload(update, item, {
      code,
      message: err?.message,
      status: typeof status === "number" ? status : undefined,
      stage: "tus-upload",
      cause: err,
    });
  });

  return {
    abort: () => controller.abort(),
  };
}
