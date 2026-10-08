/**
 * Shared retention helpers used both at SSR time (UploadCard.astro) and on
 * the client (enhance-retention.ts). Kept isomorphic: no Node/browser APIs.
 */
import { t, type Locale } from "../i18n";

export const DEFAULT_ALLOWED_TTL_HOURS = [0.5, 1, 6, 12, 24, 72, 168];
export const DEFAULT_TTL_HOURS = 24;

const KNOWN_KEYS: Record<number, `retention.${string}`> = {
  0.5: "retention.30m",
  1: "retention.1h",
  6: "retention.6h",
  12: "retention.12h",
  24: "retention.24h",
  72: "retention.3d",
  168: "retention.7d",
};

/** Localized label for an arbitrary allowed-TTL hour value. */
export function ttlLabel(locale: Locale, hours: number): string {
  const known = KNOWN_KEYS[hours];
  if (known) return t(locale, known);

  const h = Number.isFinite(hours) ? hours : DEFAULT_TTL_HOURS;

  if (h < 1) {
    const mins = Math.round(h * 60);
    return t(locale, mins === 1 ? "retention.minute" : "retention.minutes", {
      count: mins,
    });
  }

  const days = h / 24;
  if (Number.isInteger(days)) {
    return t(locale, days === 1 ? "retention.day" : "retention.days", {
      count: days,
    });
  }

  const num = Number.isInteger(h) ? h : Math.round(h * 100) / 100;
  return t(locale, num === 1 ? "retention.hour" : "retention.hours", {
    count: num,
  });
}
