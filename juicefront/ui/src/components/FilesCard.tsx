import { onMount, onCleanup, For } from "solid-js";
import {
  formatSize,
  iconForMime,
  iconHTML,
  makeCopyBar,
  pctRemaining,
  announce,
} from "../lib/format";
import { iconSvgHtml } from "../lib/icons";
import { openQrModal, type QrLabels } from "../lib/qr";
import { t, type Locale } from "../i18n";
import { getMoreMenuItems } from "../lib/more-menu";
import {
  onFileDeleted,
  emitFileDeleted,
  removeLocalFile,
  renameLocalFile,
} from "../lib/file-events";
import type { ServerFile } from "../lib/types";
import FileCard, { remainingLabel } from "./FileCard";

/** Escape HTML entities to prevent XSS when interpolating user data into innerHTML. */
function esc(s: string | null | undefined): string {
  if (s == null) return "";
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// WeakMap-backed shift-delete state (avoids `document as any` expandos)
const shiftState = { held: false, listenersAttached: false };
function isShiftHeld(): boolean { return shiftState.held; }
function attachShiftListeners(onChange: () => void) {
  if (shiftState.listenersAttached) return;
  shiftState.listenersAttached = true;
  document.addEventListener("keydown", (e) => {
    if (e.key === "Shift" && !shiftState.held) {
      shiftState.held = true;
      onChange();
    }
  });
  document.addEventListener("keyup", (e) => {
    if (e.key === "Shift") {
      shiftState.held = false;
      onChange();
    }
  });
  window.addEventListener("blur", () => {
    shiftState.held = false;
    onChange();
  });
}

interface Props {
  id?: string;
  locale?: string;
  title?: string;
  subtitle?: string;
  footerCommit?: string;
  footerMessage?: string;
  footerRepoUrl?: string;
  footerLinks?: {
    label: string;
    href: string;
    external?: boolean;
    ariaLabel?: string;
  }[];
  emptyMessage?: string;
  actions?: {
    id?: string;
    label: string;
    icon?: string;
    ariaLabel?: string;
    href?: string;
    disabled?: boolean;
  }[];
  initialFiles?: ServerFile[];
  /** This instance's default host (public_base_url). Files stored there are
   *  treated as "local" and don't get a "Stored on" tag. */
  defaultHost?: string;
}

function SvgIcon(props: { name: string; size?: number; class?: string }) {
  const size = props.size ?? 24;
  return (
    <span
      style={{ width: `${size}px`, height: `${size}px`, display: "inline-flex", "align-items": "center", "justify-content": "center", "flex-shrink": "0" }}
      class={props.class}
      aria-hidden="true"
      innerHTML={iconSvgHtml(props.name, size)}
    />
  );
}

/** Labels for the shared file-URL QR modal. */
function qrLabelsFor(l: Locale): QrLabels {
  return {
    title: t(l, "files.qr_title"),
    hint: t(l, "files.qr_hint"),
    copy: t(l, "files.qr_copy"),
    copied: t(l, "files.qr_copied"),
    download: t(l, "files.qr_download"),
    close: t(l, "modal.close"),
  };
}

const QR_ICON_WRAP_STYLE =
  "width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges";

/** Create a QR button that looks like the edit button (opens the QR modal). */
function makeQrButton(url: string, l: Locale): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "copy-bar-qr";
  btn.title = t(l, "files.qr");
  btn.setAttribute("aria-label", t(l, "files.qr_aria"));
  btn.setAttribute("data-qr-url", url);
  btn.innerHTML = `<span style="${QR_ICON_WRAP_STYLE}" aria-hidden="true">${iconSvgHtml("qr-code", 24, "copy-bar-qr__icon")}</span>`;
  btn.addEventListener("click", () => {
    const current = btn.getAttribute("data-qr-url") || url;
    if (current) openQrModal(current, qrLabelsFor(l));
  });
  return btn;
}

function normalizeFile(f: any): any {
  let expires_at = f.expires_at;
  if (expires_at == null) {
    const old = f.expiresAt;
    expires_at = old ? (old > 1e11 ? Math.floor(old / 1000) : old) : 0;
  }
  let uploaded_at = f.uploaded_at;
  if (uploaded_at == null) {
    const old = f.uploadedAt;
    uploaded_at = old ? (old > 1e11 ? Math.floor(old / 1000) : old) : 0;
  }
  return {
    id: f.id,
    filename: f.filename ?? f.name ?? "",
    mime_type: f.mime_type ?? "",
    size_bytes: f.size_bytes ?? f.size ?? 0,
    uploaded_at,
    expires_at,
    url: f.url ?? "",
    delete_token: f.delete_token ?? "",
    storage_host: f.storage_host ?? "",
  };
}

function getLocalFiles(): any[] {
  try {
    const raw = localStorage.getItem("juicebox_uploads");
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) return parsed.map(normalizeFile);
    }
  } catch {}
  return [];
}

function saveLocalFiles(files: any[]) {
  try {
    localStorage.setItem("juicebox_uploads", JSON.stringify(files));
  } catch {}
}

function isDefaultHost(host: string, defaultHost?: string): boolean {
  if (!host) return true;
  const stripped = host.replace(/^https?:\/\//, "").replace(/\/$/, "");
  if (
    stripped === "localhost:6402" ||
    stripped === "127.0.0.1:6402" ||
    stripped === "localhost:6400" ||
    stripped === "127.0.0.1:6400"
  ) {
    return true;
  }
  if (defaultHost) {
    const defStripped = defaultHost
      .replace(/^https?:\/\//, "")
      .replace(/\/$/, "");
    return stripped === defStripped;
  }
  return false;
}

/** Reads this instance's default host from the SSR server-config element. */
function readDefaultHost(): string {
  try {
    return (
      document
        .getElementById("server-config")
        ?.getAttribute("data-public-base-url") || ""
    );
  } catch {
    return "";
  }
}

export default function FilesCard(props: Props) {
  const locale = (props.locale ?? "en") as Locale;
  const cardId = props.id ?? "files-card";
  const headerId = `${cardId}-header`;
  const actionsGridId = `${cardId}-actions`;
  const localePrefix = locale === "en" ? "" : `/${locale}`;
  const lp = (path: string) => `${localePrefix}${path}`;
  const moreItems = getMoreMenuItems(locale, lp);

  let cardRef!: HTMLDivElement;
  let gridRef!: HTMLDivElement;
  let emptyRef!: HTMLDivElement;
  let offlineRef!: HTMLDivElement;
  let liveUpdateTimer: ReturnType<typeof setInterval> | null = null;
  let unsubDeleted: (() => void) | null = null;

  function updateEmptyState() {
    if (!gridRef) return;
    const hasCards = gridRef.querySelectorAll(".file-card").length > 0;
    if (emptyRef) emptyRef.hidden = hasCards;
    gridRef.hidden = !hasCards;
  }

  function flipRemainingCards(removedEl: HTMLElement, container: HTMLElement) {
    const siblings = Array.from(
      container.querySelectorAll(".file-card:not(.exiting)"),
    ) as HTMLElement[];
    const firstRects = siblings.map((el) => el.getBoundingClientRect());
    removedEl.remove();
    requestAnimationFrame(() => {
      const lastRects = siblings.map((el) => el.getBoundingClientRect());
      siblings.forEach((el, i) => {
        const dx = firstRects[i].left - lastRects[i].left;
        const dy = firstRects[i].top - lastRects[i].top;
        if (dx === 0 && dy === 0) return;
        el.style.transition = "none";
        el.style.transform = `translate(${dx}px, ${dy}px)`;
        requestAnimationFrame(() => {
          el.style.transition = "transform 0.25s cubic-bezier(0.4, 0, 0.2, 1)";
          el.style.transform = "";
          el.addEventListener(
            "transitionend",
            () => {
              el.style.transition = "";
            },
            { once: true },
          );
        });
      });
      updateEmptyState();
    });
  }

  function makeConfirmDeleteHandler(
    el: HTMLElement,
    f: any,
  ): (e: MouseEvent) => Promise<void> {
    return async (e: MouseEvent) => {
      const btn = e.currentTarget as HTMLButtonElement;
      if (e.shiftKey) {
        btn.dataset.confirming = "true";
        btn.classList.add("confirming");
      } else if (!btn.dataset.confirming) {
        btn.dataset.confirming = "true";
        btn.classList.add("confirming");
        const textSpan = btn.querySelector(
          ".confirm-text",
        ) as HTMLElement | null;
        if (textSpan) textSpan.textContent = t(locale, "files.confirm");
        announce(t(locale, "files.confirm_sr"));
        return;
      }
      try {
        const textSpan = btn.querySelector(
          ".confirm-text",
        ) as HTMLElement | null;
        btn.innerHTML = `<img src="/loading.webp" alt="" class="spinner-icon" width="18" height="18"><span class="confirm-text"></span>`;
        if (textSpan)
          (btn.querySelector(".confirm-text") as HTMLElement).textContent =
            textSpan.textContent;
        btn.classList.add("spinning");
        if (f.delete_token) {
          const res = await fetch(`/file/${f.id}`, {
            method: "DELETE",
            headers: { "X-Delete-Token": f.delete_token },
          });
          if (!res.ok) throw new Error("delete failed");
          removeLocalFile(f.id);
          emitFileDeleted(f.id);
        }
        el.classList.add("exiting");
        announce(t(locale, "sr.file_deleted"));
        setTimeout(() => {
          const container = el.parentElement;
          if (container) flipRemainingCards(el, container);
          else {
            el.remove();
            updateEmptyState();
          }
        }, 300);
      } catch {
        btn.dataset.confirming = "";
        btn.classList.remove("confirming", "spinning");
      btn.innerHTML = `${iconHTML("trash", 24)}<span class="confirm-text"></span>`;
        alert(t(locale, "files.delete_error"));
      }
    };
  }

  function renderCard(f: any): HTMLElement {
    const el = document.createElement("div");
    el.className = "file-card";
    el.setAttribute("role", "listitem");
    el.setAttribute("data-file-id", f.id ?? "");
    el.setAttribute("data-filename", f.filename ?? "");
    el.setAttribute("data-size", String(f.size_bytes ?? 0));
    el.setAttribute(
      "data-uploaded",
      String(f.uploaded_at ?? f.expires_at - 86400),
    );
    el.setAttribute("data-expires", String(f.expires_at));
    el.setAttribute("data-url", f.url || "");
    el.setAttribute("data-delete-token", f.delete_token ?? "");
    el.setAttribute("data-storage-host", f.storage_host ?? "");
    const hostTag = f.storage_host && !isDefaultHost(f.storage_host, readDefaultHost())
      ? `<span class="storage-host-tag">${t(locale, "files.stored_on", { host: f.storage_host.replace(/^https?:\/\//, "") })}</span>`
      : "";
    const url = f.url || "";
    const ttl = remainingLabel(f.expires_at, locale);
    const pct = f.expires_at
      ? Math.max(
          0,
          Math.min(
            100,
            ((f.expires_at * 1000 - Date.now()) /
              ((f.expires_at - (f.uploaded_at || f.expires_at - 86400)) * 1000)) *
              100,
          ),
        )
      : 0;
    el.innerHTML = `
      <div class="file-card-header">
        <div class="file-card-icon">${iconHTML(iconForMime(f.mime_type ?? ""), 24)}</div>
        <div class="file-card-info">
          <h3 class="file-card-name">${esc(f.filename || f.id || "file")}</h3>
          <p class="file-card-meta">${formatSize(f.size_bytes || 0)}</p>
        </div>
        ${hostTag}
      </div>
      ${url ? `<div class="copy-bar-wrapper"><a href="${esc(url)}" class="file-card-link" target="_blank" rel="noopener noreferrer"><span class="file-card-link-text">${esc(url)}</span></a><button type="button" class="copy-bar-qr" data-qr-url="${esc(url)}" title="${t(locale, "files.qr")}" aria-label="${t(locale, "files.qr_aria")}"><span style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges" aria-hidden="true">${iconSvgHtml("qr-code", 24, "copy-bar-qr__icon")}</span></button><a href="#rename-${esc(f.id)}" class="copy-bar-edit" title="${t(locale, "files.rename")}" aria-label="${t(locale, "files.rename_aria")}"><span style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges" aria-hidden="true">${iconSvgHtml("edit", 24)}</span></a></div>` : ""}
      <div class="file-card-actions">
        ${f.expires_at ? `<div class="file-card-ttl" title="${t(locale, "files.time_remaining")}"><span class="file-card-ttl-label">${ttl}</span><span class="file-card-ttl-bar"><span class="file-card-ttl-fill" style="width:${pct.toFixed(0)}%"></span></span></div>` : ""}
        <div class="file-card-buttons">
          <noscript>
            <form method="POST" action="/file/${esc(f.id)}/delete">
              <input type="hidden" name="token" value="${esc(f.delete_token ?? "")}">
              <button type="submit" class="file-action-btn file-action-btn--danger" aria-label="${t(locale, "files.delete")} file">${iconHTML("trash", 24)}</button>
            </form>
          </noscript>
        </div>
      </div>
      ${makeRenameModalHtml(f)}`;
    el.querySelector(".copy-bar-qr")?.addEventListener("click", (e) => {
      const btn = e.currentTarget as HTMLButtonElement;
      const current = btn.getAttribute("data-qr-url") || url;
      if (current) openQrModal(current, qrLabelsFor(locale));
    });
    if (ttl === t(locale, "files.expired"))
      el.querySelector(".file-card-ttl")?.classList.add("expired");
    return el;
  }

  function enhanceExistingCard(el: HTMLElement, f: any) {
    if (el.querySelector(".copy-bar")) return;
    const storageHost =
      el.getAttribute("data-storage-host") || f.storage_host || "";
    const headerEl = el.querySelector(".file-card-header");
    if (storageHost && !isDefaultHost(storageHost, readDefaultHost()) && headerEl && !headerEl.querySelector(".storage-host-tag")) {
      const tag = document.createElement("span");
      tag.className = "storage-host-tag";
      tag.textContent = t(locale, "files.stored_on", { host: storageHost.replace(/^https?:\/\//, "") });
      headerEl.appendChild(tag);
    }

    const customIdEnabled = document.getElementById("server-config")
      ?.getAttribute("data-custom-id") !== "false";

    const linkEl = el.querySelector(".file-card-link");
    const existingWrapper = el.querySelector(".copy-bar-wrapper");
    const copyBar = makeCopyBar(f.url || "");

    if (existingWrapper) {
      const existingLink = existingWrapper.querySelector(".file-card-link");
      if (existingLink) existingLink.replaceWith(copyBar);
    } else if (linkEl) {
      const wrapper = document.createElement("div");
      wrapper.className = "copy-bar-wrapper";
      linkEl.replaceWith(wrapper);
      wrapper.appendChild(copyBar);
    } else {
      const header = el.querySelector(".file-card-header");
      if (header) header.after(copyBar);
    }

    if (customIdEnabled && f.delete_token) {
      const wrapper = el.querySelector(".copy-bar-wrapper") as HTMLElement;
      if (wrapper && !wrapper.querySelector(".copy-bar-edit")) {
        const editBtn = document.createElement("a");
        editBtn.href = `#rename-${f.id}`;
        editBtn.className = "copy-bar-edit";
        editBtn.title = t(locale, "files.rename");
        editBtn.setAttribute("aria-label", t(locale, "files.rename_aria"));
        editBtn.innerHTML = iconSvgHtml("edit", 24, "copy-bar-edit__icon");
        wrapper.appendChild(editBtn);
      }

      const editBtn = wrapper?.querySelector(".copy-bar-edit") as HTMLAnchorElement;
      if (editBtn && !editBtn.dataset.wired) {
        editBtn.dataset.wired = "1";
        editBtn.addEventListener("click", (e) => {
          e.preventDefault();
          showRenameModal(f);
        });
      }

      if (!el.querySelector(`#rename-${f.id}`)) {
        el.insertAdjacentHTML("beforeend", makeRenameModalHtml(f));
      }

      const renameModal = el.querySelector(`#rename-${f.id}`) as HTMLElement;
      if (renameModal) {
        wireRenameModal(renameModal, f);
      }
    }

    // Ensure every file card has a QR button that looks like the edit
    // button (opens a QR modal for the file URL, logo in the middle).
    const qrWrapper = el.querySelector(".copy-bar-wrapper") as HTMLElement | null;
    if (qrWrapper && f.url && !qrWrapper.querySelector(".copy-bar-qr")) {
      const qrBtn = makeQrButton(f.url, locale);
      const editEl = qrWrapper.querySelector(".copy-bar-edit");
      if (editEl) editEl.before(qrBtn);
      else qrWrapper.appendChild(qrBtn);
    }
    const existingQr = el.querySelector(".copy-bar-qr") as HTMLButtonElement | null;
    if (existingQr && f.url) existingQr.setAttribute("data-qr-url", f.url);

    function makeDeleteBtn(): HTMLButtonElement {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "file-action-btn file-action-btn--danger";
      btn.title = t(locale, "files.delete");
      btn.setAttribute("aria-label", `${t(locale, "files.delete")} file`);
      btn.innerHTML = `${iconHTML("trash", 24)}<span class="confirm-text"></span>`;
      btn.addEventListener("click", makeConfirmDeleteHandler(el, f));
      return btn;
    }
    const existingBtn = el.querySelector(".file-action-btn--danger");
    if (existingBtn) existingBtn.replaceWith(makeDeleteBtn());
    else {
      const btnContainer = el.querySelector(".file-card-buttons");
      if (btnContainer) btnContainer.append(makeDeleteBtn());
    }

    const ttlLabel = el.querySelector(".file-card-ttl-label");
    const ttlEl = el.querySelector(".file-card-ttl");
    const fill = el.querySelector(".file-card-ttl-fill") as HTMLElement | null;
    const ttl = remainingLabel(f.expires_at, locale);
    if (ttlLabel) ttlLabel.textContent = ttl;
    if (fill)
      fill.style.width = `${pctRemaining(f.expires_at, f.uploaded_at || f.expires_at - 86400)}%`;
    if (ttlEl) ttlEl.classList.toggle("expired", ttl === t(locale, "files.expired"));
  }

  function extractIdFromUrl(url: string, fallbackId: string): string {
    if (!url) return fallbackId;
    try {
      const path = new URL(url).pathname;
      const segment = path.split("/").pop() || "";
      const dotIdx = segment.lastIndexOf(".");
      return dotIdx > 0 ? segment.slice(0, dotIdx) : segment || fallbackId;
    } catch {
      const segment = url.split("/").pop() || "";
      const dotIdx = segment.lastIndexOf(".");
      return dotIdx > 0 ? segment.slice(0, dotIdx) : segment || fallbackId;
    }
  }

  function makeRenameModalHtml(f: any): string {
    const currentId = extractIdFromUrl(f.url, f.id || "");
    return `
      <div id="rename-${esc(f.id)}" class="mdloverlay mdl-target rename-modal" role="dialog" aria-modal="true" aria-labelledby="rename-title-${esc(f.id)}">
        <a href="#!" class="mdl-target__backdrop" aria-label="${t(locale, "modal.close")}"></a>
        <div class="mdlcontent">
          <div class="mdlheader">
            <h2 id="rename-title-${esc(f.id)}" class="mdltitle">${iconSvgHtml("edit", 24, "mdltitle-icon")} ${t(locale, "files.rename")}</h2>
            <a href="#!" class="mdlclose" aria-label="${t(locale, "modal.close")}">${iconSvgHtml("close", 24)}</a>
          </div>
          <form class="rename-form">
            <div class="mdlbody">
              <input type="hidden" name="token" value="${esc(f.delete_token)}" />
              <div class="form-field">
                <label class="form-label" for="rename-input-${esc(f.id)}">${t(locale, "files.rename_placeholder")}</label>
                <div class="rename-input-group">
                  <span class="rename-input-prefix">/f/</span>
                  <input type="text" id="rename-input-${esc(f.id)}" name="custom_id" class="form-input rename-input" value="${esc(currentId)}" maxlength="32" minlength="3" pattern="[-A-Za-z0-9_]+" autocomplete="off" spellcheck="false" />
                </div>
                <p class="rename-error" hidden></p>
              </div>
            </div>
            <div class="mdlfooter rename-actions">
              <button type="button" class="mdlbtn mdlbtn--secondary">${t(locale, "files.rename_cancel")}</button>
              <button type="submit" class="mdlbtn mdlbtn--primary">${t(locale, "files.rename_save")}</button>
            </div>
          </form>
        </div>
      </div>`;
  }

  function showRenameModal(f: any) {
    const existing = document.getElementById(`rename-${f.id}`);
    if (existing) {
      location.hash = `#rename-${f.id}`;
      return;
    }
    const card = gridRef?.querySelector(`[data-file-id="${f.id}"]`) as HTMLElement;
    if (!card) return;
    card.insertAdjacentHTML("beforeend", makeRenameModalHtml(f));
    const modal = document.getElementById(`rename-${f.id}`) as HTMLElement;
    if (modal) {
      wireRenameModal(modal, f);
      location.hash = `#rename-${f.id}`;
    }
  }

  function wireRenameModal(modal: HTMLElement, f: any) {
    const form = modal.querySelector(".rename-form") as HTMLFormElement;
    const input = modal.querySelector(".rename-input") as HTMLInputElement;
    const errorEl = modal.querySelector(".rename-error") as HTMLElement;
    if (!form || !input) return;

    const closeRenameModal = () => {
      modal.remove();
      if (location.hash.startsWith("#rename-")) {
        history.replaceState(null, "", location.pathname + location.search);
      }
    };

    modal.querySelector(".mdlbtn--secondary")?.addEventListener("click", (e) => {
      e.preventDefault();
      closeRenameModal();
    });
    modal.querySelector(".mdlclose")?.addEventListener("click", (e) => {
      e.preventDefault();
      closeRenameModal();
    });
    modal.querySelector(".mdl-target__backdrop")?.addEventListener("click", (e) => {
      e.preventDefault();
      closeRenameModal();
    });

    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (errorEl) errorEl.hidden = true;

      const customId = input.value.trim();
      try {
        const res = await fetch(`/file/${f.id}/renew`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            "X-Delete-Token": f.delete_token,
          },
          body: JSON.stringify({ custom_id: customId }),
        });
        if (!res.ok) {
          const data = await res.json().catch(() => ({}));
          const msg = data.message || t(locale, "files.rename_invalid");
          if (errorEl) {
            errorEl.textContent = msg;
            errorEl.hidden = false;
          }
          return;
        }
        const data = await res.json();
        renameLocalFile(f.id, data.id, data.url, f.delete_token);
        f.id = data.id;
        f.url = data.url;
        const card = modal.closest(".file-card") as HTMLElement;
        if (card) {
          card.setAttribute("data-file-id", data.id);
          card.setAttribute("data-url", data.url);
          const copyBar = card.querySelector(".copy-bar");
          if (copyBar) {
            const urlEl = copyBar.querySelector(".copy-bar__url");
            if (urlEl) urlEl.textContent = data.url;
          }
          const editLink = card.querySelector(".copy-bar-edit") as HTMLAnchorElement;
          if (editLink) editLink.href = `#rename-${data.id}`;
          const qrBtn = card.querySelector(".copy-bar-qr") as HTMLButtonElement | null;
          if (qrBtn) qrBtn.setAttribute("data-qr-url", data.url);
        }
        modal.remove();
        announce(t(locale, "files.rename_success"));
      } catch {
        if (errorEl) {
          errorEl.textContent = t(locale, "files.rename_invalid");
          errorEl.hidden = false;
        }
      }
    });
  }

  async function validateWithServer(localFiles: any[]) {
    const pairs = localFiles
      .filter((f: any) => f.id && f.delete_token)
      .map((f: any) => ({ id: f.id, token: f.delete_token }));
    if (pairs.length === 0) return;

    try {
      const res = await fetch("/api/owned-files", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ pairs }),
      });
      if (!res.ok) return;
      const data = await res.json();
      const validIds = new Set<string>((data.files ?? []).map((f: any) => f.id));
      const staleIds = pairs.filter((p) => !validIds.has(p.id)).map((p) => p.id);
      if (staleIds.length === 0) return;

      staleIds.forEach((id) => {
        const el = gridRef?.querySelector(`[data-file-id="${id}"]`);
        if (el) el.remove();
      });

      const filtered = localFiles.filter((f: any) => !staleIds.includes(f.id));
      saveLocalFiles(filtered);
      updateEmptyState();
    } catch {}
  }

  function init() {
    if (!cardRef || cardRef.hasAttribute("data-enhanced")) return;
    cardRef.setAttribute("data-enhanced", "");
    if (!gridRef) return;

    const localFiles = getLocalFiles();
    const seen = new Set<string>();
    const existingIds = new Set<string>();
    gridRef.querySelectorAll(".file-card").forEach((el) => {
      const id = (el as HTMLElement).getAttribute("data-file-id");
      if (id) {
        existingIds.add(id);
        seen.add(id);
      }
    });

    let cardIndex = gridRef.querySelectorAll(".file-card").length;
    [...localFiles]
      .sort((a: any, b: any) => (b.uploaded_at ?? 0) - (a.uploaded_at ?? 0))
      .forEach((f: any) => {
        if (f.id && !seen.has(f.id) && f.expires_at * 1000 > Date.now()) {
          seen.add(f.id);
          const card = renderCard(f);
          card.style.setProperty("--file-card-index", String(cardIndex++));
          gridRef.append(card);
        }
      });

    validateWithServer(localFiles);

    const totalCards = gridRef.querySelectorAll(".file-card").length;
    if (totalCards === 0) {
      if (emptyRef) emptyRef.hidden = false;
      gridRef.hidden = true;
    } else {
      if (emptyRef) emptyRef.hidden = true;
      gridRef.hidden = false;
      gridRef.querySelectorAll(".file-card").forEach((el) => {
        const id = (el as HTMLElement).getAttribute("data-file-id");
        if (id) {
          const match = localFiles.find((m: any) => m.id === id);
          const elData: any = match || { id };
          if (!match) {
            const domToken = (el as HTMLElement).getAttribute(
              "data-delete-token",
            );
            if (domToken) {
              elData.delete_token = domToken;
              elData.filename =
                (el as HTMLElement).getAttribute("data-filename") || "";
              elData.size_bytes =
                Number((el as HTMLElement).getAttribute("data-size")) || 0;
              elData.uploaded_at =
                Number((el as HTMLElement).getAttribute("data-uploaded")) || 0;
              elData.expires_at =
                Number((el as HTMLElement).getAttribute("data-expires")) || 0;
              elData.url = (el as HTMLElement).getAttribute("data-url") || "";
              elData.storage_host =
                (el as HTMLElement).getAttribute("data-storage-host") || "";
            }
          }
          enhanceExistingCard(el as HTMLElement, elData);
        }
      });
    }

    function onShiftChange() {
      if (cardRef)
        cardRef.classList.toggle("shift-delete", isShiftHeld());
    }
    attachShiftListeners(onShiftChange);
    if (isShiftHeld()) cardRef.classList.add("shift-delete");
  }

  function startLiveUpdates() {
    if (liveUpdateTimer != null) clearInterval(liveUpdateTimer);
    liveUpdateTimer = setInterval(() => {
      if (!gridRef || gridRef.hidden) return;
      gridRef.querySelectorAll(".file-card").forEach((el) => {
        const expires = Number(
          (el as HTMLElement).getAttribute("data-expires"),
        );
        const uploaded =
          Number((el as HTMLElement).getAttribute("data-uploaded")) ||
          expires - 86400;
        if (!expires) return;
        if (expires * 1000 <= Date.now()) {
          (el as HTMLElement).remove();
          updateEmptyState();
          return;
        }
        const ttl = remainingLabel(expires, locale);
        const ttlLabel = el.querySelector(".file-card-ttl-label");
        const fill = el.querySelector(
          ".file-card-ttl-fill",
        ) as HTMLElement | null;
        const ttlEl = el.querySelector(".file-card-ttl");
        if (ttlLabel) ttlLabel.textContent = ttl;
        if (fill) fill.style.width = `${pctRemaining(expires, uploaded)}%`;
        if (ttlEl) ttlEl.classList.toggle("expired", ttl === t(locale, "files.expired"));
      });
    }, 30000);
  }

  onMount(() => {
    init();
    startLiveUpdates();
    document.addEventListener("astro:page-load", onPageLoad);
    unsubDeleted = onFileDeleted((fileId) => {
      const el = gridRef?.querySelector(`[data-file-id="${fileId}"]`);
      if (el) {
        el.remove();
        updateEmptyState();
      }
    });
  });

  onCleanup(() => {
    if (typeof document === "undefined") return;
    document.removeEventListener("astro:page-load", onPageLoad);
    if (liveUpdateTimer != null) clearInterval(liveUpdateTimer);
    unsubDeleted?.();
  });

  function onPageLoad() {
    init();
    startLiveUpdates();
  }

  const activeFiles = (props.initialFiles ?? []).filter(
    (f) => f.expires_at * 1000 > Date.now(),
  );
  const hasServerFiles = activeFiles.length > 0;
  const emptyMessage = props.emptyMessage ?? t(locale, "files.empty");

  const list = props.actions ?? [
    { id: "files", label: t(locale, "nav.files"), icon: "files" },
    { id: "report", label: t(locale, "nav.report"), icon: "report" },
    { id: "more", label: t(locale, "nav.more"), icon: "more" },
  ];

  return (
    <div
      id={cardId}
      ref={cardRef}
      class="upload-card files-card"
      data-files-card=""
      data-component="files-card"
      aria-labelledby={headerId}
    >
      <header id={headerId} class="upload-header upload-header--centered">
        <h1 class="upload-header__title">
          <img src="/logo_big.webp?v=2" alt="Juicebox" class="upload-header__logo" loading="eager" fetchpriority="high" />
          {props.title ?? t(locale, "files.title")}
        </h1>
        {(props.subtitle ?? t(locale, "files.subtitle")) && (
          <p class="upload-header__subtitle">
            {props.subtitle ?? t(locale, "files.subtitle")}
          </p>
        )}
      </header>

      <section class="upldfs" data-upldfs="" aria-label="Uploaded files">
        <div class="scroll-element scroll-cover-top" aria-hidden="true"></div>
        <div class="scroll-element sticky-shadow-top" aria-hidden="true"></div>

        <div class="uploaded-files-content">
          <div
            ref={offlineRef}
            class="files-offline"
            data-files-offline=""
            hidden
          >
            <SvgIcon name="x" size={48} class="offline-icon" />
            <p class="offline-primary">{t(locale, "files.offline")}</p>
            <p class="offline-secondary">
              {t(locale, "files.offline_desc")}
            </p>
          </div>
          <div
            ref={emptyRef}
            class="nothin"
            data-files-empty=""
            hidden={hasServerFiles}
          >
            <SvgIcon name="file" size={48} class="nothin-icon" />
            <p class="nothin-text">{emptyMessage}</p>
            <p class="nothin-hint">
              <a href="/">{t(locale, "files.upload_hint")}</a>
            </p>
          </div>
          <div
            ref={gridRef}
            class="files-grid"
            role="list"
            data-files-grid=""
            hidden={!hasServerFiles}
          >
            <For each={activeFiles}>
              {(f, i) => (
                <FileCard
                  locale={locale}
                  mode="file"
                  id={f.id}
                  filename={f.filename}
                  mimeType={f.mime_type}
                  size={f.size_bytes}
                  url={f.url}
                  deleteToken={f.delete_token}
                  storageHost={f.storage_host ?? ""}
                  defaultHost={props.defaultHost}
                  expiresAt={f.expires_at}
                  uploadedAt={f.uploaded_at}
                  staggerIndex={i()}
                />
              )}
            </For>
          </div>
        </div>

        <div
          class="scroll-element sticky-shadow-bottom"
          aria-hidden="true"
        ></div>
        <div
          class="scroll-element scroll-cover-bottom"
          aria-hidden="true"
        ></div>
      </section>

      {props.actions && props.actions.length > 0 && (
        <>
          <span
            id="actions-area"
            tabindex="-1"
            aria-hidden="true"
            style={{ position: "absolute" }}
          ></span>
          <nav
            id={actionsGridId}
            class="act"
            aria-label={t(locale, "actions.file_actions")}
            data-act=""
          >
            {list.map((action) =>
              action.id === "more" ? (
                <details class="select-details" data-select-details="">
                  <summary
                    class="action-btn"
                    aria-haspopup="menu"
                    aria-label={action.ariaLabel ?? action.label}
                    aria-describedby={`${cardId}-more-desc`}
                    title={action.label}
                  >
                    <span class="action-btn__icon">
                      <SvgIcon name={action.icon ?? "more"} />
                    </span>
                    <span>{action.label}</span>
                  </summary>
                  <span id={`${cardId}-more-desc`} class="sr-only">
                    {t(locale, "sr.more_menu_sr")}
                  </span>
                  <summary class="select-backdrop" data-select-backdrop=""></summary>
                  <summary class="select-close-btn nojs-only">{t(locale, "sr.close_btn")}</summary>
                  <div
                    class="select-menu select-menu--details"
                    role="menu"
                    aria-label={t(locale, "sr.more_options")}
                    aria-describedby={`${cardId}-more-desc`}
                  >
                    <div class="select-options-list">
                      {moreItems.map((item) =>
                        item === null ? (
                          <div class="select-option--divider" />
                        ) : (
                          <a
                            class={
                              item.jsOnly
                                ? "select-option js-only"
                                : "select-option"
                            }
                            role="menuitem"
                            href={item.href}
                          >
                            <span class="select-option__icon">
                              <SvgIcon name={item.icon} />
                            </span>
                            <span class="select-option__label">
                              {item.label}
                            </span>
                          </a>
                        ),
                      )}
                    </div>
                  </div>
                </details>
              ) : (
                <a
                  class="action-btn"
                  href={
                    action.href ??
                    { files: "/files", report: "/report", back: "/" }[
                      action.id ?? ""
                    ] ?? "/"
                  }
                  aria-label={action.ariaLabel ?? action.label}
                  title={action.label}
                  data-action-id={action.id}
                >
                  <span class="action-btn__icon">
                    <SvgIcon name={action.icon ?? "more"} />
                  </span>
                  <span>{action.label}</span>
                </a>
              ),
            )}
          </nav>
        </>
      )}

      <footer class="footr footr--divd" data-footr="">
        <div class="footr__primary">
          {(props.footerMessage ?? "juicebox2-epsilon") && (
            <p class="footr__message">
              {props.footerMessage ?? "juicebox2-epsilon"}
            </p>
          )}
          <a
            class="footr__commit footr__link"
            href={
              props.footerRepoUrl ??
              "https://github.com/create-juicey-app/juicebox2"
            }
            target="_blank"
            rel="noopener noreferrer"
          >
            {props.footerCommit ?? "main@a3f8c2d"}
          </a>
        </div>
        {props.footerLinks && props.footerLinks.length > 0 && (
          <nav class="footr__links" aria-label="Additional footer links">
            {props.footerLinks.map((link) => (
              <a
                href={link.href}
                target={link.external ? "_blank" : undefined}
                rel={link.external ? "noopener noreferrer" : undefined}
                aria-label={link.ariaLabel ?? link.label}
                class="footr__link footr__link--nav"
              >
                {link.label}
              </a>
            ))}
          </nav>
        )}
      </footer>
    </div>
  );
}
