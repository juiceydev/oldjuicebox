import { createSignal, Show } from "solid-js";
import { t, type Locale } from "../i18n";
import {
  formatSize,
  iconForMime,
  announce,
  pctRemaining,
} from "../lib/format";
import { iconSvgHtml } from "../lib/icons";
import { openQrModal } from "../lib/qr";
import { vanityMsg, errorDetail } from "../lib/errors";
import type { UploadState } from "../lib/upload-engine";

const ACTIVE_STATES: ReadonlySet<UploadState> = new Set([
  "queued",
  "compressing",
  "uploading",
  "finalizing",
]);

export function isActiveState(state: UploadState): boolean {
  return ACTIVE_STATES.has(state);
}

export function remainingLabel(expiresAtSec: number, locale: Locale): string {
  if (!expiresAtSec || !Number.isFinite(expiresAtSec)) return "???";
  const left = expiresAtSec * 1000 - Date.now();
  if (!Number.isFinite(left)) return "???";
  if (left <= 0) return t(locale, "files.expired");
  const secs = Math.floor(left / 1000);
  const mins = Math.floor(secs / 60);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);
  if (mins < 1) return t(locale, "files.expires", { time: `${secs}s` });
  if (mins < 60) return t(locale, "files.expires", { time: `${mins}m` });
  if (hours < 24) return t(locale, "files.expires", { time: `${hours}h` });
  return t(locale, "files.expires", { time: `${days}d` });
}

export interface FileCardProps {
  locale: Locale;
  mode: "upload" | "file";
  id: string;
  filename: string;
  mimeType: string;
  size: number;
  url?: string;
  serverId?: string;
  deleteToken?: string;
  reserveUrl?: string;
  state?: UploadState;
  progress?: number;
  errorCode?: string;
  errorMessage?: string;
  errorStatus?: number;
  errorStage?: string;
  storageHost?: string;
  defaultHost?: string;
  expiresAt?: number;
  uploadedAt?: number;
  staggerIndex?: number;
  onCancel?: () => void;
  onRemove?: () => void;
  onDelete?: () => void;
  onRename?: (data: { id: string; url: string }) => void;
}

export default function FileCard(props: FileCardProps) {
  const locale = () => props.locale;
  const [renameOpen, setRenameOpen] = createSignal(false);
  const [renaming, setRenaming] = createSignal(false);
  const [renameError, setRenameError] = createSignal("");
  let renameInput: HTMLInputElement | undefined;

  const statusText = (): string => {
    switch (props.state) {
      case "queued":
      case "compressing":
        return t(locale(), "upload.initializing");
      case "uploading":
        return t(locale(), "upload.uploading", {
          percent: (props.progress ?? 0).toFixed(1),
        });
      case "finalizing":
        return t(locale(), "upload.finalizing");
      case "done":
        return t(locale(), "upload.complete");
      case "cancelled":
        return t(locale(), "upload.cancelled");
      case "error": {
        const friendly = vanityMsg(locale(), props.errorCode, props.errorMessage);
        const detail = errorDetail(props.errorCode, props.errorMessage, {
          status: props.errorStatus,
          stage: props.errorStage,
        });
        return detail ? `${friendly} — ${detail}` : friendly;
      }
      default:
        return "";
    }
  };

  const statusTitle = (): string =>
    props.state === "error"
      ? errorDetail(props.errorCode, props.errorMessage, {
          status: props.errorStatus,
          stage: props.errorStage,
        }) || statusText()
      : statusText();

  const isActive = () => props.state != null && isActiveState(props.state);
  const isError = () =>
    props.state === "error" || props.state === "cancelled";

  const hasHostTag = () => {
    if (!props.storageHost) return false;
    return !isDefaultHost(props.storageHost, props.defaultHost);
  };

  function isDefaultHost(host: string, defaultHost?: string): boolean {
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

  const ttlPct = () =>
    props.expiresAt && props.uploadedAt
      ? pctRemaining(props.expiresAt, props.uploadedAt)
      : 0;

  const currentId = () => {
    const m = props.url?.match(/\/f\/([A-Za-z0-9_-]+)/);
    return m ? m[1] : "";
  };

  const copy = async (e: Event, url: string) => {
    try {
      await navigator.clipboard.writeText(url);
    } catch {}
    const btn = e.currentTarget as HTMLButtonElement;
    btn.classList.add("copy-bar--copied");
    setTimeout(() => btn.classList.remove("copy-bar--copied"), 2000);
    announce(t(locale(), "upload.copy_bar_announce"));
  };

  const action = () => {
    if (isActive()) {
      props.onCancel?.();
    } else if (isError()) {
      props.onRemove?.();
    } else {
      props.onDelete?.();
    }
  };

  const canRename = () =>
    props.mode === "upload" &&
    props.state === "done" &&
    !!props.serverId &&
    !!props.deleteToken;

  const openQr = (url: string) => {
    if (!url) return;
    openQrModal(url, {
      title: t(locale(), "files.qr_title"),
      hint: t(locale(), "files.qr_hint"),
      copy: t(locale(), "files.qr_copy"),
      copied: t(locale(), "files.qr_copied"),
      download: t(locale(), "files.qr_download"),
      close: t(locale(), "modal.close"),
    });
  };

  const openQrFromButton = (e: Event, fallbackUrl: string) => {
    const btn = e.currentTarget as HTMLButtonElement | null;
    openQr(btn?.getAttribute("data-qr-url") || fallbackUrl);
  };

  const openRename = () => {
    setRenameError("");
    setRenameOpen(true);
  };

  const closeRename = () => setRenameOpen(false);

  const saveRename = async (e: Event) => {
    e.preventDefault();
    if (renaming()) return;
    setRenaming(true);
    setRenameError("");
    try {
      const res = await fetch(`/file/${props.serverId}/renew`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Delete-Token": props.deleteToken ?? "",
        },
        body: JSON.stringify({ custom_id: renameInput?.value.trim() ?? "" }),
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setRenameError(data.message || t(locale(), "files.rename_invalid"));
        return;
      }
      const data = await res.json();
      props.onRename?.({ id: data.id, url: data.url });
      setRenameOpen(false);
      announce(t(locale(), "files.rename_success"));
    } catch {
      setRenameError(t(locale(), "files.rename_invalid"));
    } finally {
      setRenaming(false);
    }
  };

  return (
    <div
      class="file-card"
      classList={{
        error: isError(),
      }}
      style={
        props.mode === "file" && props.staggerIndex != null
          ? { "--file-card-index": String(props.staggerIndex) }
          : undefined
      }
      role={props.mode === "file" ? "listitem" : undefined}
      data-file-id={props.mode === "file" ? props.id : undefined}
      data-upload-id={props.mode === "upload" ? props.id : undefined}
      data-filename={props.mode === "file" ? props.filename : undefined}
      data-size={props.mode === "file" ? props.size : undefined}
      data-uploaded={props.mode === "file" ? props.uploadedAt : undefined}
      data-expires={props.mode === "file" ? props.expiresAt : undefined}
      data-url={props.mode === "file" ? props.url : undefined}
      data-delete-token={props.mode === "file" ? props.deleteToken : undefined}
      data-storage-host={props.mode === "file" ? props.storageHost : undefined}
      data-state={props.mode === "upload" ? props.state : undefined}
      data-quick-link={
        props.mode === "upload" &&
        props.reserveUrl &&
        props.state !== "done" &&
        props.state != null
          ? ""
          : undefined
      }
    >
      <div class="file-card-header">
        <div class="file-card-icon">
          <span
            style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
            aria-hidden="true"
            innerHTML={iconSvgHtml(iconForMime(props.mimeType), 24)}
          />
        </div>
        <div class="file-card-info">
          <h3 class="file-card-name" title={props.filename}>
            {props.filename}
          </h3>
          <p class="file-card-meta">
            {props.mode === "file"
              ? `${formatSize(props.size)} - ${props.mimeType}`
              : formatSize(props.size)}
          </p>
        </div>
        {hasHostTag() &&
          (props.mode === "file" || props.state === "done") && (
            <span class="storage-host-tag">
              {t(locale(), "files.stored_on", {
                host: props.storageHost!.replace(/^https?:\/\//, ""),
              })}
            </span>
          )}
        <Show when={props.mode === "upload" && props.state !== "done"}>
          <button
            type="button"
            class="file-action-btn file-action-btn--danger file-card-upload-action"
            aria-label={
              isActive()
                ? t(locale(), "upload.remove_from_queue")
                : t(locale(), "tray.dismiss")
            }
            title={
              isActive()
                ? t(locale(), "tray.cancel")
                : t(locale(), "tray.dismiss")
            }
            onClick={action}
          >
            <span
              style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
              aria-hidden="true"
              innerHTML={iconSvgHtml("trash", 24)}
            />
          </button>
        </Show>
      </div>

      <Show when={props.mode === "file" && props.url}>
        <div class="copy-bar-wrapper">
          <a
            href={props.url}
            class="file-card-link"
            target="_blank"
            rel="noopener noreferrer"
          >
            <span class="file-card-link-text">{props.url}</span>
          </a>
          <button
            type="button"
            class="copy-bar-qr"
            data-qr-url={props.url}
            title={t(locale(), "files.qr")}
            aria-label={t(locale(), "files.qr_aria")}
            onClick={(e) => openQrFromButton(e, props.url!)}
          >
            <span
              style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
              aria-hidden="true"
              innerHTML={iconSvgHtml("qr-code", 24, "copy-bar-qr__icon")}
            />
          </button>
          <a
            href={`#rename-${props.id}`}
            class="copy-bar-edit"
            title={t(locale(), "files.rename")}
            aria-label={t(locale(), "files.rename_aria")}
          >
            <span
              style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
              aria-hidden="true"
              innerHTML={iconSvgHtml("edit", 24, "copy-bar-edit__icon")}
            />
          </a>
        </div>
      </Show>

      <Show
        when={
          props.mode === "upload" &&
          props.state === "done" &&
          props.url
        }
      >
        <div class="copy-bar-wrapper">
          <button
            type="button"
            class="copy-bar"
            aria-label={t(locale(), "upload.copy_bar_aria")}
            title={t(locale(), "upload.copy_bar_title")}
            onClick={(e) => copy(e, props.url!)}
          >
            <div class="copy-bar__text-wrapper">
              <span class="copy-bar__copied-text">
                {t(locale(), "upload.copy_bar_copied")}
              </span>
              <span class="copy-bar__url">{props.url}</span>
            </div>
            <span
              innerHTML={iconSvgHtml("copy", 24, "copy-bar__copy-icon")}
            />
          </button>
          <button
            type="button"
            class="copy-bar-qr"
            data-qr-url={props.url}
            title={t(locale(), "files.qr")}
            aria-label={t(locale(), "files.qr_aria")}
            onClick={(e) => openQrFromButton(e, props.url!)}
          >
            <span
              style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
              aria-hidden="true"
              innerHTML={iconSvgHtml("qr-code", 24, "copy-bar-qr__icon")}
            />
          </button>
          <Show when={canRename()}>
            <button
              type="button"
              class="copy-bar-edit"
              title={t(locale(), "files.rename")}
              aria-label={t(locale(), "files.rename_aria")}
              onClick={openRename}
            >
              <span
                style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
                aria-hidden="true"
                innerHTML={iconSvgHtml("edit", 24, "copy-bar-edit__icon")}
              />
            </button>
          </Show>
        </div>
      </Show>

      <Show when={props.mode === "upload" && isActive()}>
        <div
          class="progress-divider"
          role="progressbar"
          aria-valuenow={(props.progress ?? 0).toFixed(1)}
          aria-valuemin="0"
          aria-valuemax="100"
          aria-label={t(locale(), "upload.progress_aria")}
        >
          <div
            class="progress-fill"
            classList={{
              finalizing: props.state === "finalizing",
              error: props.state === "error",
            }}
            style={`--progress:${props.progress ?? 0}%`}
          />
        </div>
        <div
          class="status-pill"
          classList={{ error: isError() }}
          role="status"
        >
          <div class="status-content">
            <img
              src="/loading.webp"
              alt=""
              class="spinner"
              aria-hidden="true"
              width="16"
              height="16"
            />
            <span class="status-text">{statusText()}</span>
          </div>
        </div>
      </Show>

      <Show when={props.mode === "upload" && isError()}>
        <div
          class="status-pill error"
          role="status"
          title={statusTitle()}
          data-error-code={props.errorCode}
          data-error-message={props.errorMessage}
          data-error-status={props.errorStatus}
          data-error-stage={props.errorStage}
        >
          <div class="status-content">
            <span class="status-text">{statusText()}</span>
          </div>
        </div>
      </Show>

      <Show
        when={
          props.mode === "upload" &&
          props.reserveUrl &&
          props.state !== "done" &&
          props.state != null
        }
      >
        <button
          type="button"
          class="copy-bar"
          aria-label={t(locale(), "upload.copy_bar_aria")}
          title={t(locale(), "upload.copy_bar_title")}
          onClick={(e) => copy(e, props.reserveUrl!)}
        >
          <div class="copy-bar__text-wrapper">
            <span class="copy-bar__copied-text">
              {t(locale(), "upload.copy_bar_copied")}
            </span>
            <span class="copy-bar__url">{props.reserveUrl}</span>
          </div>
          <span
            innerHTML={iconSvgHtml("copy", 24, "copy-bar__copy-icon")}
          />
        </button>
      </Show>

      <Show when={props.mode === "file" && props.expiresAt}>
        <div class="file-card-actions">
          <div
            class="file-card-ttl"
            classList={{ expired: ttlPct() === 0 }}
            title={t(locale(), "files.time_remaining")}
          >
            <span class="file-card-ttl-label">
              {remainingLabel(props.expiresAt!, locale())}
            </span>
            <span class="file-card-ttl-bar">
              <span
                class="file-card-ttl-fill"
                style={{ width: `${ttlPct().toFixed(0)}%` }}
              />
            </span>
          </div>
          <div class="file-card-buttons">
            <noscript>
              <form method="post" action={`/file/${props.id}/delete`}>
                <input type="hidden" name="token" value={props.deleteToken} />
                <button
                  type="submit"
                  class="file-action-btn file-action-btn--danger"
                  aria-label={`${t(locale(), "files.delete")} file`}
                >
                  <span
                    style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
                    aria-hidden="true"
                    innerHTML={iconSvgHtml("trash", 24)}
                  />
                </button>
              </form>
            </noscript>
          </div>
        </div>
      </Show>

      <Show when={props.mode === "upload" && props.state === "done"}>
        <div class="file-card-actions">
          <Show when={props.expiresAt}>
            <div
              class="file-card-ttl"
              classList={{ expired: ttlPct() === 0 }}
              title={t(locale(), "files.time_remaining")}
            >
              <span class="file-card-ttl-label">
                {remainingLabel(props.expiresAt!, locale())}
              </span>
              <span class="file-card-ttl-bar">
                <span
                  class="file-card-ttl-fill"
                  style={{ width: `${ttlPct().toFixed(0)}%` }}
                />
              </span>
            </div>
          </Show>
          <div class="file-card-buttons">
            <button
              type="button"
              class="file-action-btn file-action-btn--danger"
              aria-label={t(locale(), "files.delete")}
              title={t(locale(), "files.delete")}
              onClick={props.onDelete}
            >
              <span
                style="width:24px;height:24px;display:inline-flex;align-items:center;justify-content:center;shape-rendering:crispEdges"
                aria-hidden="true"
                innerHTML={iconSvgHtml("trash", 24)}
              />
            </button>
          </div>
        </div>
      </Show>

      <Show when={renameOpen()}>
        <div
          class="mdloverlay rename-modal"
          role="dialog"
          aria-modal="true"
          aria-labelledby={`rename-title-${props.serverId}`}
          onClick={(e) => {
            if (e.target === e.currentTarget) closeRename();
          }}
        >
          <div class="mdlcontent">
            <div class="mdlheader">
              <h2 id={`rename-title-${props.serverId}`} class="mdltitle">
                <span
                  innerHTML={iconSvgHtml("edit", 24, "mdltitle-icon")}
                />
                {t(locale(), "files.rename")}
              </h2>
              <button
                type="button"
                class="mdlclose"
                aria-label={t(locale(), "modal.close")}
                onClick={closeRename}
              >
                <span innerHTML={iconSvgHtml("close", 24)} />
              </button>
            </div>
            <form class="rename-form" onSubmit={saveRename}>
              <div class="mdlbody">
                <input type="hidden" name="token" value={props.deleteToken} />
                <div class="form-field">
                  <label
                    class="form-label"
                    for={`rename-input-${props.serverId}`}
                  >
                    {t(locale(), "files.rename_placeholder")}
                  </label>
                  <div class="rename-input-group">
                    <span class="rename-input-prefix">/f/</span>
                    <input
                      ref={renameInput}
                      type="text"
                      id={`rename-input-${props.serverId}`}
                      name="custom_id"
                      class="form-input rename-input"
                      value={currentId()}
                      maxlength="32"
                      minlength="3"
                      pattern="[-A-Za-z0-9_]+"
                      autocomplete="off"
                      spellcheck="false"
                    />
                  </div>
                  <Show when={renameError()}>
                    <p class="rename-error">{renameError()}</p>
                  </Show>
                </div>
              </div>
              <div class="mdlfooter rename-actions">
                <button
                  type="button"
                  class="mdlbtn mdlbtn--secondary"
                  onClick={closeRename}
                >
                  {t(locale(), "files.rename_cancel")}
                </button>
                <button
                  type="submit"
                  class="mdlbtn mdlbtn--primary"
                  disabled={renaming()}
                >
                  {t(locale(), "files.rename_save")}
                </button>
              </div>
            </form>
          </div>
        </div>
      </Show>
    </div>
  );
}
