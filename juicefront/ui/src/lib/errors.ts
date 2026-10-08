import { t, type Locale } from "../i18n";
import type { TranslationKeys } from "../i18n/en";

const VANITY_ERROR_KEYS: Record<string, keyof TranslationKeys> = {
  // -- frontend lifecycle --
  FILE_TOO_LARGE: "error.file_too_large",
  FILE_BLOCKED: "error.file_blocked",
  BLOCKED_FILE_TYPE: "error.file_blocked",
  EXPIRED: "error.expired",
  FILE_EXPIRED: "error.expired",
  INVALID_ID: "error.invalid_id",
  NOT_FOUND: "error.not_found",
  FILE_NOT_FOUND: "error.not_found",
  RATE_LIMITED: "error.rate_limited",
  ACCOUNT_LOCKED: "error.rate_limited",
  UPSTREAM_ERROR: "error.upstream",
  STORAGE_UNREACHABLE: "error.upstream",
  STORAGE_REJECTED: "error.storage_rejected",
  STORAGE_FULL: "error.storage_full",
  INSUFFICIENT_STORAGE: "error.storage_full",
  NETWORK_ERROR: "error.network",
  UPLOAD_FAILED: "error.upload_failed",
  UPLOAD_TIMEOUT: "error.upload_timeout",
  TUS_ERROR: "error.tus",
  TUS_MISSING_LENGTH: "error.tus",
  TUS_MISSING_OFFSET: "error.tus",
  TUS_OFFSET_MISMATCH: "error.tus",
  TUS_SESSION_NOT_FOUND: "error.tus",
  TUS_CREATE_FAILED: "error.tus",
  TUS_PATCH_FAILED: "error.tus",
  INVALID_REQUEST: "error.invalid_request",
  BAD_REQUEST: "error.invalid_request",
  INVALID_MULTIPART: "error.invalid_request",
  GZIP_DECODE_FAILED: "error.invalid_request",
  UNAUTHORIZED: "error.unauthorized",
  FORBIDDEN: "error.unauthorized",
  CONFLICT: "error.conflict",
  RESERVE_FAILED: "error.reserve",
  BAD_RESPONSE: "error.bad_response",
  INTERNAL_ERROR: "error.server",
  DISK_ERROR: "error.server",
  DATABASE_ERROR: "error.server",
  DB_POOL_ERROR: "error.server",
  TASK_PANICKED: "error.server",
  SERVICE_UNAVAILABLE: "error.server",
  UNKNOWN: "error.unknown",
};

/** Friendly (translated) message for a code. Never includes raw server text. */
export function friendlyMsg(locale: Locale, errorCode?: string): string {
  if (errorCode && VANITY_ERROR_KEYS[errorCode]) return t(locale, VANITY_ERROR_KEYS[errorCode]);
  return t(locale, "error.upload_failed");
}

/** Resolve a user-facing error message from a code or raw message.
 * Prefers the friendly translation when the code is known so a backend
 * "STORAGE_UNREACHABLE" never renders as a bare "Network error"; the raw
 * server text is surfaced separately via errorDetail(). */
export function vanityMsg(locale: Locale, errorCode?: string, rawMessage?: string): string {
  if (errorCode && VANITY_ERROR_KEYS[errorCode]) return t(locale, VANITY_ERROR_KEYS[errorCode]);
  if (rawMessage && !looksLikeBareCode(rawMessage)) return rawMessage;
  if (rawMessage && VANITY_ERROR_KEYS[rawMessage]) return t(locale, VANITY_ERROR_KEYS[rawMessage]);
  return t(locale, "error.upload_failed");
}

function looksLikeBareCode(s: string): boolean {
  return /^[A-Z][A-Z0-9_]{2,40}$/.test(s.trim());
}

export interface ErrorDetailOpts {
  status?: number;
  stage?: string;
}

/** Debuggable detail line: `CODE: message (HTTP 502 @ direct-reserve)`.
 * Returns "" when there is nothing beyond the friendly message. */
export function errorDetail(
  errorCode?: string,
  rawMessage?: string,
  opts?: ErrorDetailOpts,
): string {
  const code = (errorCode || "").trim();
  const msg = (rawMessage || "").trim();
  // A raw message that is just a bare CODE adds no sentence-level info:
  // fold it into the code slot instead of "CODE: CODE".
  const msgIsBareCode = !!msg && looksLikeBareCode(msg);
  const parts: string[] = [];
  if (code) parts.push(code);
  if (msg && msg !== code && !msgIsBareCode) parts.push(msg);
  else if (msgIsBareCode && msg !== code && !code) parts.push(msg);
  else if (msgIsBareCode && msg !== code && code) parts.push(`(${msg})`);
  const where: string[] = [];
  if (opts?.status) where.push(`HTTP ${opts.status}`);
  if (opts?.stage) where.push(`@ ${opts.stage}`);
  let detail = parts.join(": ");
  if (where.length && detail) detail += ` (${where.join(" ")})`;
  else if (where.length) detail = `(${where.join(" ")})`;
  return detail;
}

/** Parse a `{ error, message }` JSON body without throwing. */
export function parseErrorBody(text: string): { code?: string; message?: string } {
  try {
    const err = JSON.parse(text);
    const out: { code?: string; message?: string } = {};
    if (typeof err?.error === "string" && err.error) out.code = err.error;
    if (typeof err?.message === "string" && err.message) out.message = err.message;
    return out;
  } catch {
    return {};
  }
}

export interface UploadErrorContext {
  code?: string;
  message?: string;
  status?: number;
  stage: string;
  filename?: string;
  size?: number;
  method?: string;
  url?: string;
  cause?: unknown;
}

/** Structured console log for every upload failure. Also stashes the last
 * error on window.__juiceboxLastError for copy-paste debugging. */
export function logUploadError(ctx: UploadErrorContext): void {
  const info = {
    code: ctx.code || "UNKNOWN",
    message: ctx.message || "",
    status: ctx.status ?? null,
    stage: ctx.stage,
    file: ctx.filename || "",
    size: ctx.size ?? null,
    method: ctx.method || "",
    url: ctx.url || "",
    cause: ctx.cause instanceof Error
      ? { name: ctx.cause.name, message: ctx.cause.message, stack: ctx.cause.stack }
      : (ctx.cause ?? null),
  };
  try {
    console.error(`[juicebox:upload:${ctx.stage}] ${info.code}${info.message ? `: ${info.message}` : ""}`, info);
  } catch {}
  try {
    (window as unknown as { __juiceboxLastError?: unknown }).__juiceboxLastError = {
      ...info,
      at: new Date().toISOString(),
    };
  } catch {}
}

/** Classify a fetch() rejection: aborts stay silent, TypeErrors are real
 * network failures, everything else is unknown. */
export function classifyFetchError(err: unknown): "CANCELLED" | "NETWORK_ERROR" | "UNKNOWN" {
  if (err instanceof DOMException && (err.name === "AbortError" || err.name === "TimeoutError")) {
    return "CANCELLED";
  }
  if (err instanceof Error && err.name === "AbortError") return "CANCELLED";
  if (err instanceof TypeError) return "NETWORK_ERROR";
  const msg = String((err as Error)?.message ?? err ?? "");
  if (/^\s*NETWORK_ERROR\s*$/.test(msg)) return "NETWORK_ERROR";
  if (/abort|cancel/i.test(msg)) return "CANCELLED";
  if (/failed to fetch|networkerror|load failed|network request failed/i.test(msg)) {
    return "NETWORK_ERROR";
  }
  return "UNKNOWN";
}
