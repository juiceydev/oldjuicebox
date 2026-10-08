import { onMount, onCleanup } from "solid-js";
import { t, type Locale } from "../i18n";
import { formatSize, iconForMime, iconHTML, announce } from "../lib/format";
import { iconSvgHtml } from "../lib/icons";
import {
  UPLOAD_URL,
  readMaxFileSize,
  TUS_THRESHOLD,
  ULTRAFAST_RESERVE_URL,
} from "../lib/upload-config";
import { vanityMsg, errorDetail, logUploadError } from "../lib/errors";
import { enhanceRetention, rebuildRetention } from "../lib/enhance-retention";
import {
  enhanceHostSelector,
  readSelectedHost,
  readSelectedUploadMode,
  readQuickLinkEnabled,
  readDangerLevel,
  readUltraFastEnabled,
  readUltraFastSupported,
  initHostGlow,
} from "../lib/enhance-host";
import { isAppMode } from "../lib/device-ws";
import { validateFileClient, type ProtectionLevel } from "../lib/file-validation";
import { createDeleteButton } from "../lib/delete-button";
import {
  enqueueUpload,
  cancelUpload,
  removeUpload,
  subscribe,
  getUploads,
  wasRemoved,
} from "../lib/upload-client";
import type { UploadItem } from "../lib/upload-engine";
import { extractServerId } from "../lib/upload-engine";

function autoCopyUrl(url: string, copyBar?: HTMLElement | null) {
  if (!url) return;
  navigator.clipboard.writeText(url).then(
    () => {
      if (copyBar?.classList.contains("copy-bar") && copyBar.isConnected) {
        copyBar.classList.add("copy-bar--copied");
        setTimeout(() => copyBar.classList.remove("copy-bar--copied"), 2000);
      }
    },
    () => {},
  );
}

function makeCopyBarNode(url: string, locale: Locale): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "copy-bar";
  btn.title = t(locale, "upload.copy_bar_title");
  btn.setAttribute("aria-label", t(locale, "upload.copy_bar_aria"));
  btn.innerHTML =
    `<div class="copy-bar__text-wrapper"><span class="copy-bar__copied-text">${t(locale, "upload.copy_bar_copied")}</span><span class="copy-bar__url"></span></div>` +
    iconSvgHtml("copy", 24, "copy-bar__copy-icon");
  const urlEl = btn.querySelector(".copy-bar__url");
  if (urlEl) urlEl.textContent = url;
  btn.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(url);
      btn.classList.add("copy-bar--copied");
      announce(t(locale, "upload.copy_bar_announce"));
      setTimeout(() => btn.classList.remove("copy-bar--copied"), 2000);
    } catch {}
  });
  return btn;
}

export default function UploadCard(props: { uploadedFile?: string | null; locale?: Locale }) {
  const locale = () => props.locale || "en";
  let listContentRef!: HTMLDivElement;
  let emptyTextRef!: HTMLDivElement;

  const rowIds = new Map<string, HTMLElement>();
  let prevSeen = new Map<string, UploadItem>();

  function getFormRef(): HTMLFormElement | null {
    return document.querySelector("[data-upload-form]");
  }

  function getCardRef(): HTMLElement | null {
    return document.getElementById("upload-card");
  }

  function getInputRef(): HTMLInputElement | null {
    return document.getElementById("dropzone-input") as HTMLInputElement | null;
  }

  function getDropZoneRef(): HTMLLabelElement | null {
    return document.querySelector("[data-drop-zone]");
  }

  function getTextareaRef(): HTMLTextAreaElement | null {
    return document.querySelector("[data-text-textarea]");
  }

  function getCobaltTextareaRef(): HTMLTextAreaElement | null {
    return document.querySelector("[data-cobalt-textarea]");
  }

  function isCobaltEnabled(): boolean {
    return (
      document.getElementById("server-config")?.getAttribute("data-cobalt") ===
      "true"
    );
  }

  /** SSR'd service domains (comma-separated) that trigger cobalt mode. */
  function cobaltServiceDomains(): string[] {
    return (getFormRef()
      ?.getAttribute("data-cobalt-services")
      ?.split(",")
      .map((s) => s.trim().toLowerCase())
      .filter(Boolean)) ?? [];
  }

  function urlMatchesCobaltService(raw: string): boolean {
    let host: string;
    try {
      host = new URL(raw).hostname.toLowerCase();
    } catch {
      return false;
    }
    return cobaltServiceDomains().some(
      (domain) => host === domain || host.endsWith(`.${domain}`),
    );
  }

  function ttlHours(): number {
    const form = getFormRef();
    const checked = form?.querySelector(
      'input[name="ttl_hours"]:checked',
    ) as HTMLInputElement | null;
    return Number(checked?.value) || 24;
  }

  function setEmpty(visible: boolean) {
    emptyTextRef?.classList.toggle("visible", visible);
  }

  function removeItem(item: HTMLElement) {
    const id = item.getAttribute("data-upload-id");
    if (id) {
      cancelUpload(id);
      rowIds.delete(id);
    }
    item.classList.add("exiting");
    setTimeout(() => {
      item.remove();
      setEmpty(!listContentRef?.querySelector(".file-item:not(.exiting)"));
    }, 300);
  }

  function failItem(
    item: HTMLElement,
    pill: HTMLElement,
    fill: HTMLElement,
    status: HTMLElement,
    errorCode?: string,
    rawMessage?: string,
    opts?: { statusCode?: number; stage?: string; filename?: string },
  ) {
    item.classList.add("error");
    pill.classList.add("error");
    fill.classList.add("error");
    fill.classList.remove("finalizing");
    item.querySelector(".spinner")?.remove();
    const msg = vanityMsg(locale(), errorCode, rawMessage);
    const detail = errorDetail(errorCode, rawMessage, {
      status: opts?.statusCode,
      stage: opts?.stage,
    });
    status.textContent = detail ? `${msg} — ${detail}` : msg;
    status.title = detail || msg;
    if (errorCode) item.setAttribute("data-error-code", errorCode);
    if (rawMessage) item.setAttribute("data-error-message", rawMessage);
    if (opts?.statusCode) item.setAttribute("data-error-status", String(opts.statusCode));
    if (opts?.stage) item.setAttribute("data-error-stage", opts.stage);
    try {
      logUploadError({
        code: errorCode,
        message: rawMessage,
        status: opts?.statusCode,
        stage: opts?.stage ? `ui:${opts.stage}` : "ui:display",
        filename: opts?.filename,
        cause: detail,
      });
    } catch {}
    announce(t(locale(), "upload.announce_failed", { message: msg }));
  }

  function animateRowOut(item: HTMLElement) {
    item.classList.add("exiting");
    const ttl = setTimeout(() => {
      item.remove();
      if (!listContentRef?.querySelector(".file-item:not(.exiting)")) {
        const et = listContentRef?.querySelector(".empty-text");
        if (et) et.classList.add("visible");
      }
    }, 300);
    (item as any)._removeTimer = ttl;
  }

  function onDeleted(item: HTMLElement) {
    const uploadId = item.getAttribute("data-upload-id");
    const fileId = item.getAttribute("data-file-id") || "";
    if (uploadId) {
      rowIds.delete(uploadId);
      removeUpload(uploadId);
    } else if (fileId) {
      const match = getUploads().find(
        (u) => (u.serverId || extractServerId(u.url || "")) === fileId,
      );
      if (match) removeUpload(match.id);
    }
    animateRowOut(item);
  }

  interface RowParts {
    fill: HTMLElement;
    status: HTMLElement;
    pill: HTMLElement;
  }

  const rowPartsCache = new WeakMap<HTMLElement, RowParts>();

  function getRowParts(row: HTMLElement): RowParts {
    let parts = rowPartsCache.get(row);
    if (!parts) {
      parts = {
        fill: row.querySelector(".progress-fill") as HTMLElement,
        status: row.querySelector(".status-text") as HTMLElement,
        pill: row.querySelector(".status-pill") as HTMLElement,
      };
      rowPartsCache.set(row, parts);
    }
    return parts;
  }

  function showQuickLink(item: UploadItem, row: HTMLElement) {
    if (!item.reserveUrl || row.hasAttribute("data-quick-link")) return;
    if (item.state === "done") return;
    row.setAttribute("data-quick-link", "");
    getRowParts(row).pill.after(makeCopyBarNode(item.reserveUrl, locale()));
  }

  function completeRow(item: UploadItem, row: HTMLElement) {
    const { fill, status, pill } = getRowParts(row);
    fill.classList.remove("finalizing");
    fill.style.setProperty("--progress", "100%");
    const progressDivider = fill.closest('[role="progressbar"]') as HTMLElement | null;
    if (progressDivider) progressDivider.setAttribute("aria-valuenow", "100");
    const iconContainer = row.querySelector(".file-icon") as HTMLElement | null;
    if (iconContainer && item.mimeType) {
      iconContainer.innerHTML = iconHTML(iconForMime(item.mimeType), 24);
    }
    row.classList.add("complete");
    row.querySelector(".file-action-area")?.classList.add("complete");
    status.textContent = t(locale(), "upload.complete");
    pill.querySelector(".spinner")?.remove();
    const existingCopyBar = pill.nextElementSibling;
    let copyBar: HTMLElement | null;
    if (existingCopyBar?.classList.contains("copy-bar")) {
      existingCopyBar.replaceWith(makeCopyBarNode(item.url || "", locale()));
    } else {
      pill.after(makeCopyBarNode(item.url || "", locale()));
    }
    copyBar = pill.nextElementSibling as HTMLElement | null;

    if (row.hasAttribute("data-quick-link")) {
      setTimeout(() => {
        const copyBar = pill.nextElementSibling as HTMLElement | null;
        if (copyBar?.classList.contains("copy-bar") && row.isConnected) {
          pill.style.boxSizing = "border-box";
          pill.style.height = pill.offsetHeight + "px";
          pill.offsetHeight;
          pill.classList.add("sliding");
          copyBar.classList.add("copy-bar--rounded");
          requestAnimationFrame(() => {
            requestAnimationFrame(() => {
              pill.style.height = "";
            });
          });
        }
      }, 2000);
    } else {
      pill.style.display = "none";
      const copyBar = pill.nextElementSibling as HTMLElement | null;
      if (copyBar?.classList.contains("copy-bar")) {
        copyBar.style.marginTop = "0.75rem";
      }
    }

    announce(t(locale(), "upload.announce_complete", { filename: item.filename }));
    autoCopyUrl(item.url || "", copyBar);

    const delBtn = row.querySelector(".delete-btn") as HTMLButtonElement | null;
    const fileId = item.serverId || extractServerId(item.url || "") || item.id;
    if (delBtn && fileId && item.deleteToken) {
      delBtn.replaceWith(
        createDeleteButton({
          fileId,
          deleteToken: item.deleteToken,
          apiBase: UPLOAD_URL,
          onDeleted,
          immediate: true,
        }),
      );
    }
  }

  function syncRow(item: UploadItem) {
    const row = rowIds.get(item.id);
    if (!row || !row.isConnected) return;
    const { fill, status, pill } = getRowParts(row);

    showQuickLink(item, row);

    switch (item.state) {
      case "queued":
      case "compressing":
        status.textContent =
          item.method === "tus"
            ? t(locale(), "upload.initializing_tus")
            : t(locale(), "upload.initializing");
        break;
      case "uploading": {
        fill.style.setProperty("--progress", `${item.progress}%`);
        const progressDivider = fill.closest('[role="progressbar"]') as HTMLElement | null;
        if (progressDivider) {
          progressDivider.setAttribute("aria-valuenow", String(item.progress.toFixed(1)));
        }
        status.textContent = t(locale(), "upload.uploading", {
          percent: item.progress.toFixed(1),
        });
        break;
      }
      case "finalizing":
        fill.classList.add("finalizing");
        status.textContent = t(locale(), "upload.finalizing");
        break;
      case "done":
        if (!row.classList.contains("complete")) completeRow(item, row);
        break;
      case "error":
        if (!row.classList.contains("error")) {
          failItem(row, pill, fill, status, item.errorCode, item.errorMessage, {
            statusCode: item.errorStatus,
            stage: item.errorStage,
            filename: item.filename,
          });
        }
        break;
      case "cancelled":
        if (!row.classList.contains("error")) {
          row.classList.add("error");
          row.querySelector(".spinner")?.remove();
          status.textContent = t(locale(), "upload.cancelled");
        }
        break;
    }
  }

  function iconName(name: string): string {
    if (/\.(png|jpg|jpeg|gif|webp|svg|avif|bmp|ico)$/i.test(name))
      return "image";
    if (/\.(mp4|webm|avi|mov|mkv)$/i.test(name)) return "video";
    if (/\.(mp3|wav|ogg|flac|aac)$/i.test(name)) return "audio-waveform";
    if (/\.(xlsx?|csv)$/i.test(name)) return "presentation";
    if (/\.(zip|gz|tar|rar|7z|bz2|xz|zst)$/i.test(name)) return "archive";
    if (/\.(json|xml|js|ts|html|css)$/i.test(name)) return "code";
    return "file-text";
  }

  /** Reserve an UltraFast upload slot and delegate to juicebox-plus. */
  async function ultrafastReserve(
    file: File,
    item: HTMLElement,
    fill: HTMLElement,
    status: HTMLElement,
    pill: HTMLElement,
  ) {
    status.textContent = t(locale(), "upload.delegating_to_app");

    try {
      const ttl = String(ttlHours());
      const reserveRes = await fetch(ULTRAFAST_RESERVE_URL, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          filename: file.name,
          mime_type: file.type || "application/octet-stream",
          file_size: file.size,
          ttl_hours: Number(ttl),
        }),
      });

      if (!reserveRes.ok) {
        let code: string | undefined;
        let msg: string | undefined;
        try {
          const err = await reserveRes.json();
          if (err.error) code = err.error;
          if (err.message) msg = err.message;
        } catch {}
        failItem(item, pill, fill, status, code, msg, {
          statusCode: reserveRes.status,
          stage: "ultrafast-reserve",
          filename: file.name,
        });
        return;
      }

      const reserve = await reserveRes.json();
      const reserveUrl = reserve.url || "";

      // Store file info in localStorage so /files page can discover it
      try {
        const stored = JSON.parse(
          localStorage.getItem("juicebox_uploads") || "[]",
        );
        stored.push({
          id: reserve.file_id || "",
          filename: file.name,
          mime_type: file.type || "application/octet-stream",
          size_bytes: file.size,
          uploaded_at: Math.floor(Date.now() / 1000),
          url: reserveUrl,
          expires_at: Math.floor(Date.now() / 1000) + Number(ttlHours()) * 3600,
          delete_token: reserve.delete_token || "",
        });
        localStorage.setItem("juicebox_uploads", JSON.stringify(stored));
      } catch {}

      // Show shareable URL immediately if available
      if (reserveUrl) {
        item.setAttribute("data-quick-link", "");
        pill.after(makeCopyBarNode(reserveUrl, locale()));
      }

      fill.style.setProperty("--progress", "0%");
      status.textContent = t(locale(), "upload.waiting_for_app");
      item.classList.add("delegated");
    } catch (err) {
      failItem(item, pill, fill, status, "NETWORK_ERROR", err instanceof Error ? err.message : String(err), {
        stage: "ultrafast-reserve",
        filename: file.name,
      });
    }
  }

  /** In app mode, open the device file picker directly without browser file picker. */
  function startUltraFastFromDropzone() {
    setEmpty(false);
    const item = document.createElement("div");
    item.className = "file-item";
    item.setAttribute("role", "listitem");
    item.innerHTML = `
      <div class="file-item-header">
        <div class="file-card-icon">${iconHTML("upload", 24)}</div>
        <div class="file-info-left">
          <span class="file-name">Picking file on device...</span>
          <span class="file-size"></span>
        </div>
        <button type="button" class="delete-btn" aria-label="${t(locale(), "upload.remove_from_queue")}">${iconHTML("trash", 24)}</button>
      </div>
      <div class="file-action-area">
        <div class="progress-divider" role="progressbar" aria-valuenow="0" aria-valuemin="0" aria-valuemax="100" aria-label="${t(locale(), "upload.progress_aria")}">
          <div class="progress-fill" style="--progress:0%"></div>
        </div>
        <div class="status-pill" role="status">
          <div class="status-content">
            <img src="/loading.webp" alt="" class="spinner" aria-hidden="true" width="16" height="16" />
            <span class="status-text">${t(locale(), "upload.delegating_to_app")}</span>
          </div>
        </div>
      </div>`;
    item
      .querySelector(".delete-btn")!
      .addEventListener("click", () => removeItem(item));
    listContentRef.prepend(item);

    const fill = item.querySelector(".progress-fill") as HTMLElement;
    const status = item.querySelector(".status-text") as HTMLElement;
    const pill = item.querySelector(".status-pill") as HTMLElement;

    // Send ultrafast reserve with placeholder metadata - device will open native file picker
    ultrafastReservePlaceholder(item, fill, status, pill);
  }

  /** Reserve an UltraFast upload slot with placeholder metadata (device picks the real file). */
  async function ultrafastReservePlaceholder(
    item: HTMLElement,
    fill: HTMLElement,
    status: HTMLElement,
    pill: HTMLElement,
  ) {
    status.textContent = t(locale(), "upload.delegating_to_app");

    try {
      const ttl = String(ttlHours());
      const reserveRes = await fetch(ULTRAFAST_RESERVE_URL, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          filename: "upload",
          mime_type: "application/octet-stream",
          file_size: 0,
          ttl_hours: Number(ttl),
        }),
      });

      if (!reserveRes.ok) {
        let code: string | undefined;
        let msg: string | undefined;
        try {
          const err = await reserveRes.json();
          if (err.error) code = err.error;
          if (err.message) msg = err.message;
        } catch {}
        failItem(item, pill, fill, status, code, msg, {
          statusCode: reserveRes.status,
          stage: "ultrafast-reserve",
          filename: "upload",
        });
        return;
      }

      const reserve = await reserveRes.json();
      const reserveUrl = reserve.url || "";

      // Store file info in localStorage so /files page can discover it
      try {
        const stored = JSON.parse(
          localStorage.getItem("juicebox_uploads") || "[]",
        );
        stored.push({
          id: reserve.file_id || "",
          filename: "upload",
          mime_type: "application/octet-stream",
          size_bytes: 0,
          uploaded_at: Math.floor(Date.now() / 1000),
          url: reserveUrl,
          expires_at: Math.floor(Date.now() / 1000) + Number(ttlHours()) * 3600,
          delete_token: reserve.delete_token || "",
        });
        localStorage.setItem("juicebox_uploads", JSON.stringify(stored));
      } catch {}

      if (reserveUrl) {
        item.setAttribute("data-quick-link", "");
        pill.after(makeCopyBarNode(reserveUrl, locale()));
      }

      fill.style.setProperty("--progress", "0%");
      status.textContent = t(locale(), "upload.waiting_for_app");
      item.classList.add("delegated");

      // Poll for completion - the device uploads directly to juicehost,
      // then calls /upload/ultrafast/complete which sets status='ready'
      if (reserve.file_id) {
        pollUltrafastStatus(reserve.file_id, item, fill, status, pill, reserve.delete_token || "");
      }
    } catch (err) {
      failItem(item, pill, fill, status, "NETWORK_ERROR", err instanceof Error ? err.message : String(err), {
        stage: "ultrafast-reserve",
        filename: "upload",
      });
    }
  }

  /** Poll /file/:id/info until the device completes the ultrafast upload. */
  function pollUltrafastStatus(
    fileId: string,
    item: HTMLElement,
    fill: HTMLElement,
    status: HTMLElement,
    pill: HTMLElement,
    deleteToken: string,
  ) {
    const INTERVAL = 1000;
    const MAX_ATTEMPTS = 150; // 5 minutes max
    let attempts = 0;

    const poll = async () => {
      if (attempts++ >= MAX_ATTEMPTS || !item.isConnected) return;
      try {
        const res = await fetch(`${UPLOAD_URL}/file/${fileId}/info?t=${Date.now()}`);
        if (!res.ok) {
          setTimeout(poll, INTERVAL);
          return;
        }
        const data = await res.json();
        if (data.status === "ready") {
          const realName = data.filename || "upload";
          const realSize = data.size_bytes || 0;
          const realUrl = data.url || "";
          const realMime = data.mime_type || "";

          const nameEl = item.querySelector(".file-name");
          if (nameEl) nameEl.textContent = realName;
          const sizeEl = item.querySelector(".file-size");
          if (sizeEl) sizeEl.textContent = formatSize(realSize);

          const iconContainer = item.querySelector(".file-card-icon") as HTMLElement | null;
          if (iconContainer && realMime) {
            iconContainer.innerHTML = iconHTML(iconForMime(realMime), 24);
          }

          fill.classList.remove("finalizing");
          fill.style.setProperty("--progress", "100%");
          const progressDivider = fill.closest('[role="progressbar"]') as HTMLElement | null;
          if (progressDivider) progressDivider.setAttribute("aria-valuenow", "100");
          item.classList.add("complete");
          item.querySelector(".file-action-area")?.classList.add("complete");
          status.textContent = t(locale(), "upload.complete");
          pill.querySelector(".spinner")?.remove();

          // Show copy bar
          const existingCopyBar = pill.nextElementSibling;
          if (existingCopyBar?.classList.contains("copy-bar")) {
            existingCopyBar.replaceWith(makeCopyBarNode(realUrl, locale()));
          } else {
            pill.after(makeCopyBarNode(realUrl, locale()));
          }
          pill.style.display = "none";
          const copyBar = pill.nextElementSibling as HTMLElement | null;
          if (copyBar?.classList.contains("copy-bar")) {
            copyBar.style.marginTop = "0.75rem";
          }

          const delBtn = item.querySelector(".delete-btn") as HTMLButtonElement | null;
          if (delBtn && fileId && deleteToken) {
            delBtn.replaceWith(
              createDeleteButton({
                fileId,
                deleteToken,
                apiBase: UPLOAD_URL,
                onDeleted,
                immediate: true,
              }),
            );
          }

          try {
            const stored = JSON.parse(localStorage.getItem("juicebox_uploads") || "[]");
            const idx = stored.findIndex((e: any) => e.id === fileId);
            if (idx >= 0) {
              stored[idx].filename = realName;
              stored[idx].mime_type = realMime;
              stored[idx].size_bytes = realSize;
              stored[idx].url = realUrl;
            } else {
              stored.push({
                id: fileId,
                filename: realName,
                mime_type: realMime,
                size_bytes: realSize,
                uploaded_at: Math.floor(Date.now() / 1000),
                url: realUrl,
                expires_at: data.expires_at || Math.floor(Date.now() / 1000) + ttlHours() * 3600,
                delete_token: deleteToken,
              });
            }
            localStorage.setItem("juicebox_uploads", JSON.stringify(stored));
          } catch {}

          announce(t(locale(), "upload.announce_complete", { filename: realName }));
          autoCopyUrl(realUrl, copyBar);
          return;
        }
        setTimeout(poll, INTERVAL);
      } catch {
        setTimeout(poll, INTERVAL);
      }
    };
    setTimeout(poll, INTERVAL);
  }

  /** Queue a cobalt fetch job and poll it like a regular upload row. */
  function startCobaltFetch(url: string) {
    setEmpty(false);
    const item = document.createElement("div");
    item.className = "file-item";
    item.setAttribute("role", "listitem");
    item.innerHTML = `
      <div class="file-item-header">
        <div class="file-card-icon">${iconHTML("video", 24)}</div>
        <div class="file-info-left">
          <span class="file-name"></span>
          <span class="file-size"></span>
        </div>
        <button type="button" class="delete-btn" aria-label="${t(locale(), "upload.remove_from_queue")}">${iconHTML("trash", 24)}</button>
      </div>
      <div class="file-action-area">
        <div class="progress-divider" role="progressbar" aria-valuenow="0" aria-valuemin="0" aria-valuemax="100" aria-label="${t(locale(), "upload.progress_aria")}">
          <div class="progress-fill" style="--progress:0%"></div>
        </div>
        <div class="status-pill" role="status">
          <div class="status-content">
            <img src="/loading.webp" alt="" class="spinner" aria-hidden="true" width="16" height="16" />
            <span class="status-text">${t(locale(), "upload.cobalt_queued")}</span>
          </div>
        </div>
      </div>`;
    item.querySelector(".file-name")!.textContent =
      url.length > 48 ? `${url.slice(0, 45)}...` : url;
    item
      .querySelector(".delete-btn")!
      .addEventListener("click", () => removeItem(item));
    listContentRef.prepend(item);

    const fill = item.querySelector(".progress-fill") as HTMLElement;
    const status = item.querySelector(".status-text") as HTMLElement;
    const pill = item.querySelector(".status-pill") as HTMLElement;

    const audioOnly = !!(
      document.querySelector("[data-cobalt-audio-only]") as HTMLInputElement | null
    )?.checked;
    const videoQuality = (
      document.querySelector("[data-cobalt-quality]") as HTMLSelectElement | null
    )?.value;
    const videoContainer = (
      document.querySelector("[data-cobalt-container]") as HTMLSelectElement | null
    )?.value;
    const audioFormat = (
      document.querySelector("[data-cobalt-format]") as HTMLSelectElement | null
    )?.value;
    const audioQualityMode = (
      document.querySelector("[data-cobalt-better-audio-mode]") as HTMLSelectElement | null
    )?.value;
    const betterAudio = audioQualityMode === "enhanced";

    fetch(`${UPLOAD_URL}/api/fetch`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        url,
        audio_only: audioOnly,
        ...(audioOnly
          ? { audio_format: audioFormat, better_audio: betterAudio }
          : { video_quality: videoQuality, video_container: videoContainer }),
      }),
    })
      .then(async (res) => {
        if (!res.ok) {
          let code: string | undefined;
          let msg: string | undefined;
          try {
            const err = await res.json();
            if (err.error) code = err.error;
            if (err.message) msg = err.message;
          } catch {}
          failItem(item, pill, fill, status, code, msg, {
            statusCode: res.status,
            stage: "cobalt-start",
            filename: url,
          });
          return;
        }
        const start = await res.json();
        if (!start.job_id) {
          failItem(item, pill, fill, status, "BAD_RESPONSE", "Fetch start response was missing job_id", {
            stage: "cobalt-start",
            filename: url,
          });
          return;
        }
        fill.classList.add("finalizing");
        status.textContent = t(locale(), "upload.cobalt_fetching");
        pollCobaltJob(start.job_id, item, fill, status, pill);
      })
      .catch((err) => failItem(item, pill, fill, status, "NETWORK_ERROR", err instanceof Error ? err.message : String(err), {
        stage: "cobalt-start",
        filename: url,
      }));
  }

  function pollCobaltJob(
    jobId: string,
    item: HTMLElement,
    fill: HTMLElement,
    status: HTMLElement,
    pill: HTMLElement,
  ) {
    const INTERVAL = 1000;
    const MAX_ATTEMPTS = 900; // matches the server-side 30 min job timeout
    let attempts = 0;

    const poll = async () => {
      if (attempts++ >= MAX_ATTEMPTS || !item.isConnected) return;
      try {
        const res = await fetch(
          `${UPLOAD_URL}/api/fetch/${jobId}?t=${Date.now()}`,
        );
        if (res.status === 404) {
          failItem(item, pill, fill, status, "NOT_FOUND", t(locale(), "upload.cobalt_gone"), {
            statusCode: 404,
            stage: "cobalt-poll",
            filename: jobId,
          });
          return;
        }
        if (!res.ok) {
          setTimeout(poll, INTERVAL);
          return;
        }
        const data = await res.json();
        if (data.status === "done" && data.file) {
          completeCobaltRow(data.file, item, fill, status, pill);
          return;
        }
        if (data.status === "failed") {
          failItem(item, pill, fill, status, data.error_code || "UPLOAD_FAILED", data.error || t(locale(), "error.unknown"), {
            stage: "cobalt-job",
            filename: jobId,
          });
          return;
        }
        // Live progress: stage-aware status line with received bytes.
        if ((data.stage || "").startsWith("retry-")) {
          const n = data.stage.split("-")[1];
          status.textContent = t(locale(), "upload.cobalt_retry").replace("{n}", n);
        } else if (typeof data.bytes_received === "number" && data.bytes_received > 0) {
          status.textContent =
            t(locale(), "upload.cobalt_fetching") + " " + formatSize(data.bytes_received);
        } else if (data.stage === "session-fallback") {
          status.textContent = t(locale(), "upload.cobalt_relay");
        } else if (data.status !== "pending") {
          status.textContent = t(locale(), "upload.cobalt_fetching");
        }
        setTimeout(poll, INTERVAL);
      } catch {
        setTimeout(poll, INTERVAL);
      }
    };
    setTimeout(poll, INTERVAL);
  }

  function completeCobaltRow(
    file: {
      id: string;
      filename: string;
      mime_type: string;
      size_bytes: number;
      url: string;
      expires_at: number;
      delete_token: string;
    },
    item: HTMLElement,
    fill: HTMLElement,
    status: HTMLElement,
    pill: HTMLElement,
  ) {
    item.querySelector(".file-name")!.textContent = file.filename;
    item.querySelector(".file-size")!.textContent = formatSize(
      file.size_bytes,
    );
    const iconContainer = item.querySelector(".file-card-icon") as HTMLElement | null;
    if (iconContainer && file.mime_type) {
      iconContainer.innerHTML = iconHTML(iconForMime(file.mime_type), 24);
    }

    fill.classList.remove("finalizing");
    fill.style.setProperty("--progress", "100%");
    const progressDivider = fill.closest('[role="progressbar"]') as HTMLElement | null;
    if (progressDivider) progressDivider.setAttribute("aria-valuenow", "100");
    item.classList.add("complete");
    item.setAttribute("data-file-id", file.id);
    item.querySelector(".file-action-area")?.classList.add("complete");
    status.textContent = t(locale(), "upload.complete");
    // Mirror the upload completion styling exactly.
    pill.querySelector(".spinner")?.remove();
    pill.style.display = "none";
    pill.after(makeCopyBarNode(file.url, locale()));
    const copyBar = pill.nextElementSibling as HTMLElement | null;
    if (copyBar?.classList.contains("copy-bar")) {
      copyBar.style.marginTop = "0.75rem";
    }

    const delBtn = item.querySelector(".delete-btn") as HTMLButtonElement | null;
    if (delBtn && file.id && file.delete_token) {
      delBtn.replaceWith(
        createDeleteButton({
          fileId: file.id,
          deleteToken: file.delete_token,
          apiBase: UPLOAD_URL,
          onDeleted,
          immediate: true,
        }),
      );
    }

    try {
      const stored = JSON.parse(
        localStorage.getItem("juicebox_uploads") || "[]",
      );
      stored.push({
        id: file.id,
        filename: file.filename,
        mime_type: file.mime_type,
        size_bytes: file.size_bytes,
        uploaded_at: Math.floor(Date.now() / 1000),
        url: file.url,
        expires_at: file.expires_at,
        delete_token: file.delete_token,
      });
      localStorage.setItem("juicebox_uploads", JSON.stringify(stored));
    } catch {}

    announce(
      t(locale(), "upload.announce_complete", { filename: file.filename }),
    );
    autoCopyUrl(file.url, copyBar);
  }

  function startItem(file: File) {
    setEmpty(false);
    const item = document.createElement("div");
    item.className = "file-item";
    item.setAttribute("role", "listitem");
    const icon = iconName(file.name);
    item.innerHTML = `
      <div class="file-item-header">
        <div class="file-card-icon">${iconHTML(icon, 24)}</div>
        <div class="file-info-left">
          <span class="file-name"></span>
          <span class="file-size"></span>
        </div>
        <button type="button" class="delete-btn" aria-label="${t(locale(), "upload.remove_from_queue")}">${iconHTML("trash", 24)}</button>
      </div>
      <div class="file-action-area">
        <div class="progress-divider" role="progressbar" aria-valuenow="0" aria-valuemin="0" aria-valuemax="100" aria-label="${t(locale(), "upload.progress_aria")}">
          <div class="progress-fill" style="--progress:0%"></div>
        </div>
        <div class="status-pill" role="status">
          <div class="status-content">
            <img src="/loading.webp" alt="" class="spinner" aria-hidden="true" width="16" height="16" />
            <span class="status-text">${t(locale(), "upload.initializing")}</span>
          </div>
        </div>
      </div>`;
    item.querySelector(".file-name")!.textContent = file.name;
    item.querySelector(".file-size")!.textContent = formatSize(file.size);
    item
      .querySelector(".delete-btn")!
      .addEventListener("click", () => removeItem(item));
    listContentRef.prepend(item);

    const fill = item.querySelector(".progress-fill") as HTMLElement;
    const status = item.querySelector(".status-text") as HTMLElement;
    const pill = item.querySelector(".status-pill") as HTMLElement;

    if (file.size > readMaxFileSize()) {
      failItem(item, pill, fill, status, "FILE_TOO_LARGE", t(locale(), "error.file_too_large"), {
        stage: "client-validation",
        filename: file.name,
      });
      return;
    }

    // Client-side file type validation
    const dangerLevel = readDangerLevel() as ProtectionLevel;
    const validation = validateFileClient(file.name, dangerLevel);
    if (!validation.allowed) {
      failItem(item, pill, fill, status, "FILE_BLOCKED", t(locale(), "error.file_blocked", { reason: t(locale(), (validation.reason || "error.unknown") as any) }), {
        stage: "client-validation",
        filename: file.name,
      });
      return;
    }

    // App mode: delegate to juicebox-plus via UltraFast reserve
    if (isAppMode() && readUltraFastEnabled() && readUltraFastSupported()) {
      ultrafastReserve(file, item, fill, status, pill);
      return;
    }

    const id = enqueueUpload({
      file,
      ttlHours: ttlHours(),
      customHost: readSelectedHost(),
      uploadMode: readSelectedUploadMode(),
      quickLink: readQuickLinkEnabled(),
    });
    item.setAttribute("data-upload-id", id);
    rowIds.set(id, item);
    status.textContent =
      file.size > TUS_THRESHOLD
        ? t(locale(), "upload.initializing_tus")
        : t(locale(), "upload.initializing");
  }

  function addUploadedItem(data: {
    id: string;
    filename: string;
    mime_type: string;
    size_bytes: number;
    url: string;
    delete_token: string;
  }) {
    setEmpty(false);
    const item = document.createElement("div");
    item.className = "file-item complete";
    item.setAttribute("role", "listitem");
    const icon = iconForMime(data.mime_type || "");
    item.innerHTML = `
      <div class="file-item-header">
        <div class="file-card-icon">${iconHTML(icon, 24)}</div>
        <div class="file-info-left">
          <span class="file-name"></span>
          <span class="file-size"></span>
        </div>
      </div>
      <div class="file-action-area complete"></div>`;
    item.querySelector(".file-name")!.textContent = data.filename;
    item.querySelector(".file-size")!.textContent = formatSize(data.size_bytes);
    if (data.id) item.setAttribute("data-file-id", data.id);
    const actionArea = item.querySelector(".file-action-area")!;
    actionArea.appendChild(makeCopyBarNode(data.url, locale()));
    if (data.id && data.delete_token) {
      actionArea.appendChild(
        createDeleteButton({
          fileId: data.id,
          deleteToken: data.delete_token,
          apiBase: UPLOAD_URL,
          onDeleted,
          immediate: true,
        }),
      );
    }
    listContentRef.prepend(item);
  }

  function enhanceServerFiles() {
    listContentRef?.querySelectorAll("[data-server-rendered]").forEach((el) => {
      const item = el as HTMLElement;
      const id = item.getAttribute("data-file-id") || "";
      const token = item.getAttribute("data-delete-token") || "";
      const url = item.getAttribute("data-url") || "";

      const link = item.querySelector(".file-card-link");
      if (link) link.replaceWith(makeCopyBarNode(url, locale()));

      const noscript = item.querySelector("noscript");
      if (noscript) {
        noscript.replaceWith(
          createDeleteButton({
            fileId: id,
            deleteToken: token,
            apiBase: UPLOAD_URL,
            onDeleted,
          }),
        );
      }
    });
  }

  function activateTextMode() {
    const form = getFormRef();
    const textarea = getTextareaRef();
    form?.classList.add("text-mode");
    textarea?.focus();
  }

  function deactivateTextMode() {
    const form = getFormRef();
    const textarea = getTextareaRef();
    form?.classList.remove("text-mode");
    if (textarea) textarea.value = "";
    // Cancel = back to the file tab.
    const fileRadio = document.getElementById(
      "mode-file",
    ) as HTMLInputElement | null;
    if (fileRadio) fileRadio.checked = true;
  }

  onMount(() => {
    const unsubUploads = subscribe(() => {
      const uploads = getUploads();
      const present = new Map(uploads.map((u) => [u.id, u]));

      // We remove the GENUINE ones for fucks sake.
      const removed = [...prevSeen.entries()]
        .filter(([id, _]) => !present.has(id) && wasRemoved(id))
        .map(([, it]) => it);

      if (removed.length) {
        listContentRef
          ?.querySelectorAll<HTMLElement>(".file-item:not(.exiting)")
          .forEach((row) => {
            const uid = row.getAttribute("data-upload-id");
            const fid = row.getAttribute("data-file-id") || "";
            const hit = removed.some(
              (it) =>
                (uid && it.id === uid) ||
                (fid && (it.serverId || extractServerId(it.url || "")) === fid),
            );
            if (hit) {
              if (uid) rowIds.delete(uid);
              animateRowOut(row);
            }
          });
      }

      for (const item of uploads) syncRow(item);
      prevSeen = present;
    });
    onCleanup(unsubUploads);

    const card = getCardRef();
    if (!card || card.hasAttribute("data-enhanced")) return;
    card.setAttribute("data-enhanced", "");

    let retentionDestroy: (() => void) | undefined;
    const initRetention = () => {
      retentionDestroy = enhanceRetention(card, locale())?.destroy;
    };
    initRetention();
    enhanceHostSelector(locale());

    import("../lib/device-ws").then(({ connectPresence }) => {
      connectPresence();
    });
    initHostGlow();

    const onHostConfigUpdated = () => {
      retentionDestroy = rebuildRetention(card, locale(), retentionDestroy)?.destroy;
    };
    window.addEventListener("juicehost-config-updated", onHostConfigUpdated);
    onCleanup(() => window.removeEventListener("juicehost-config-updated", onHostConfigUpdated));

    try {
      if (props.uploadedFile) {
        const data = JSON.parse(props.uploadedFile);
        addUploadedItem(data);
        history.replaceState(null, "", location.pathname);
      }
    } catch {}

    const form = getFormRef();
    const input = getInputRef();
    const dropZone = getDropZoneRef();

    form?.addEventListener("submit", (e) => e.preventDefault());
    enhanceServerFiles();

    input?.addEventListener("change", () => {
      if (!input.files) return;
      Array.from(input.files).forEach(startItem);
      input.value = "";
    });

    // Always remove `for` to prevent the native label->input click.
    // We handle the click ourselves below and branch on app mode.
    dropZone?.removeAttribute("for");

    // The <input> is inside the <label>, so the browser natively triggers
    // the input's file dialog when the label is clicked, even with `for` removed.
    // Intercept the input click directly in app mode to prevent the dialog.
    input?.addEventListener("click", (e) => {
      if (isAppMode() && readUltraFastEnabled() && readUltraFastSupported()) {
        e.preventDefault();
        e.stopPropagation();
        startUltraFastFromDropzone();
      }
    }, true);

    dropZone?.addEventListener("click", (e) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.closest("input")) return;
      e.preventDefault();
      e.stopPropagation();
      if (isAppMode() && readUltraFastEnabled() && readUltraFastSupported()) {
        startUltraFastFromDropzone();
      } else {
        const input = getInputRef();
        if (input) {
          input.value = "";
          input.click();
        }
      }
    }, true);

    // Toggle app-mode class on dropzone for visual tint
    const isFullUltraFast = () => isAppMode() && readUltraFastEnabled() && readUltraFastSupported();
    const syncDropzoneAppMode = () => {
      dropZone?.classList.toggle("drop-zone--app-mode", isFullUltraFast());
    };
    syncDropzoneAppMode();
    window.addEventListener("juicebox-app-mode", syncDropzoneAppMode as EventListener);
    window.addEventListener("juicehost-config-updated", syncDropzoneAppMode);
    // Re-check when the UltraFast toggle changes in the host modal
    const ufToggle = document.querySelector("[data-ultrafast-toggle]") as HTMLInputElement | null;
    ufToggle?.addEventListener("change", syncDropzoneAppMode);

    ["dragenter", "dragover"].forEach((n) => {
      dropZone?.addEventListener(n, (e) => {
        e.preventDefault();
        dropZone.classList.add("dragover");
      });
    });
    ["dragleave", "drop"].forEach((n) => {
      dropZone?.addEventListener(n, (e) => {
        e.preventDefault();
        dropZone.classList.remove("dragover");
      });
    });
    dropZone?.addEventListener("drop", (e) => {
      if (!e.dataTransfer?.files) return;
      Array.from(e.dataTransfer.files).forEach(startItem);
    });

    card
      .querySelector("[data-text-cancel]")
      ?.addEventListener("click", deactivateTextMode);
    card
      .querySelector("[data-text-upload]")
      ?.addEventListener("click", () => {
        const textarea = getTextareaRef();
        const text = textarea?.value;
        if (!text?.trim()) return;
        startItem(new File([text], "paste.txt", { type: "text/plain" }));
        deactivateTextMode();
      });

    // JuiceBox x Cobalt.Tools mode wiring
    const audioToggle = document.querySelector(
      "[data-cobalt-audio-only]",
    ) as HTMLInputElement | null;
    const videoRows = [
      document.querySelector("[data-cobalt-quality-row]") as HTMLElement | null,
      document.querySelector("[data-cobalt-container-row]") as HTMLElement | null,
    ];
    const audioRows = [
      document.querySelector("[data-cobalt-format-row]") as HTMLElement | null,
      document.querySelector("[data-cobalt-better-audio-row]") as HTMLElement | null,
    ];
    // Audio-only swaps the video pickers for the audio ones.
    audioToggle?.addEventListener("change", () => {
      for (const row of videoRows) if (row) row.hidden = audioToggle.checked;
      for (const row of audioRows) if (row) row.hidden = !audioToggle.checked;
    });
    card
      .querySelector("[data-cobalt-cancel]")
      ?.addEventListener("click", () => {
        const textarea = getCobaltTextareaRef();
        if (textarea) textarea.value = "";
        // Cancel = leave the cobalt interface, back to the file tab.
        const fileRadio = document.getElementById(
          "mode-file",
        ) as HTMLInputElement | null;
        if (fileRadio) fileRadio.checked = true;
      });
    card
      .querySelector("[data-cobalt-fetch]")
      ?.addEventListener("click", () => {
        const url = getCobaltTextareaRef()?.value.trim();
        if (!url) return;
        startCobaltFetch(url);
        const textarea = getCobaltTextareaRef();
        if (textarea) textarea.value = "";
      });

    document.addEventListener("paste", onPaste);
    document.addEventListener("astro:page-load", reinit);
  });

  onCleanup(() => {
    if (typeof document === "undefined") return;
    document.removeEventListener("paste", onPaste);
    document.removeEventListener("astro:page-load", reinit);
  });

  function onPaste(e: ClipboardEvent) {
    if (document.documentElement.hasAttribute("data-backend-offline")) return;
    const target = e.target as HTMLElement;
    if (
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable)
    )
      return;
    const form = getFormRef();
    if (form?.classList.contains("text-mode")) return;

    const files = e.clipboardData?.files;
    if (files && files.length > 0) {
      e.preventDefault();
      Array.from(files).forEach(startItem);
      return;
    }

    const text = e.clipboardData?.getData("text");
    if (!text?.trim()) return;

    // A pasted URL opens the JuiceBox x Cobalt.Tools interface only when the
    // host matches one of the service domains supported by the instance.
    const trimmed = text.trim();
    if (
      /^https?:\/\/\S+$/i.test(trimmed) &&
      isCobaltEnabled() &&
      urlMatchesCobaltService(trimmed)
    ) {
      e.preventDefault();
      const cobaltRadio = document.getElementById(
        "mode-cobalt",
      ) as HTMLInputElement | null;
      const cobaltTextarea = getCobaltTextareaRef();
      if (cobaltRadio && cobaltTextarea) {
        cobaltTextarea.value = trimmed;
        cobaltRadio.checked = true;
        cobaltTextarea.focus();
        return;
      }
    }

    e.preventDefault();
    const textarea = getTextareaRef();
    if (textarea) textarea.value = text;
    activateTextMode();
  }

  function reinit() {
    const card = getCardRef();
    if (!card || card.hasAttribute("data-enhanced")) return;
    card.setAttribute("data-enhanced", "");
    enhanceRetention(card, locale());
    enhanceHostSelector(locale());
  }

  return (
    <div
      ref={listContentRef}
      class="file-list-content"
      role="group"
      aria-label={t(locale(), "upload.queued_aria")}
      data-component="upload-card"
    >
      <div ref={emptyTextRef} class="empty-text visible">
        {t(locale(), "upload.empty")}
      </div>
    </div>
  );
}
