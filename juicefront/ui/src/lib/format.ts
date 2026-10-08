/**
 * Shared utility functions for formatting, icons, and UI components.
 * Used by both SSR (Astro frontmatter) and client-side scripts.
 */

import { iconSvgHtml } from "./icons";

let _announcer: HTMLElement | null = null;

/** Announce a message to screen readers via a live region. */
export function announce(message: string): void {
  if (typeof document === "undefined") return;
  if (!_announcer) {
    _announcer = document.createElement("div");
    _announcer.setAttribute("role", "status");
    _announcer.setAttribute("aria-live", "polite");
    _announcer.setAttribute("aria-atomic", "true");
    _announcer.style.cssText =
      "position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0";
    document.body.appendChild(_announcer);
  }
  _announcer.textContent = message;
}

/** Format bytes into a human-readable string (e.g. "1.5 MB"). */
export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1,
  );
  const v = bytes / Math.pow(1024, i);
  return `${v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** Format a max upload size (bytes) as whole MB/GB (e.g. "500 MB", "5 GB"). */
export function formatMaxSize(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  if (mb >= 1024) {
    const gb = mb / 1024;
    return gb % 1 === 0 ? `${gb} GB` : `${gb.toFixed(1)} GB`;
  }
  return mb % 1 === 0 ? `${mb} MB` : `${mb.toFixed(1)} MB`;
}

/** Map a MIME type to an icon name. */
export function iconForMime(mime: string): string {
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "audio-waveform";
  if (/spreadsheet|csv|excel/.test(mime)) return "presentation";
  if (/archive|zip|gzip|rar|7z|tar|bzip|xz|compress|zstd/.test(mime))
    return "archive";
  if (/wordprocessingml|presentationml/.test(mime)) return "file-text";
  if (/json|xml|javascript|ecmascript|typescript|html|css/.test(mime))
    return "code";
  return "file-text";
}

/** Render an inline SVG icon. */
export function iconHTML(name: string, size = 18): string {
  return iconSvgHtml(name, size);
}

/** Human-readable label for time remaining until expiry. */
export function remainingLabel(expiresAtSec: number): string {
  const left = expiresAtSec * 1000 - Date.now();
  if (left <= 0) return "Expired";
  const mins = Math.floor(left / 60000);
  const hours = Math.floor(left / 3600000);
  const days = Math.floor(left / 86400000);
  if (mins < 60) return `${mins}m left`;
  if (hours < 24) return `${hours}h left`;
  return `${days}d left`;
}

/** Percentage of TTL remaining (0-100). */
export function pctRemaining(expiresAt: number, uploadedAt: number): number {
  const total = (expiresAt - uploadedAt) * 1000;
  if (total <= 0) return 0;
  const left = expiresAt * 1000 - Date.now();
  return Math.max(0, Math.min(100, (left / total) * 100));
}

/** Create a copy-to-clipboard button for a URL. */
export function makeCopyBar(url: string): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "copy-bar";
  btn.title = "Click to copy link";
  btn.setAttribute("aria-label", "Copy link to clipboard");
  btn.innerHTML =
    '<div class="copy-bar__text-wrapper"><span class="copy-bar__copied-text">Copied!</span><span class="copy-bar__url"></span></div>' +
    iconSvgHtml("copy", 24, "copy-bar__copy-icon");
  const urlEl = btn.querySelector(".copy-bar__url");
  if (urlEl) urlEl.textContent = url;
  btn.addEventListener("click", async () => {
    try {
      const currentUrl = urlEl?.textContent?.trim() || url;
      await navigator.clipboard.writeText(currentUrl);
      btn.classList.add("copy-bar--copied");
      announce("Link copied to clipboard");
      setTimeout(() => btn.classList.remove("copy-bar--copied"), 2000);
    } catch {}
  });
  return btn;
}
