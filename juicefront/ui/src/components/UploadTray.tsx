import {
  createEffect,
  createSignal,
  For,
  Show,
  onCleanup,
  onMount,
} from "solid-js";
import { t, type Locale } from "../i18n";
import { iconSvgHtml } from "../lib/icons";
import { UPLOAD_URL } from "../lib/upload-config";
import {
  getUploads,
  subscribe,
  connectUploads,
  wasRemoved,
  cancelUpload,
  removeUpload,
  updateUpload,
} from "../lib/upload-client";
import type { UploadItem } from "../lib/upload-engine";
import { extractServerId } from "../lib/upload-engine";
import {
  onFileDeleted,
  emitFileDeleted,
  removeLocalFile,
  renameLocalFile,
} from "../lib/file-events";
import FileCard from "./FileCard";

// better entrance anim
function useEntrance(
  ref: () => HTMLElement | undefined,
  active: () => boolean,
  className: string,
) {
  createEffect(() => {
    const el = ref();
    if (!el || el.dataset.entrance !== undefined) return;
    el.dataset.entrance = "1";
    if (!active()) return;
    const onEnd = () => el.classList.remove(className);
    el.addEventListener("animationend", onEnd);
    el.classList.add(className);
  });
}

interface TrayCard {
  id: string;
  item: UploadItem;
}

const TRAY_OPEN_KEY = "juicebox_tray_open";
const TRAY_ITEMS_KEY = "juicebox_tray_items";
const MAX_PERSISTED_ITEMS = 100;

const TERMINAL_STATES = new Set(["done", "error", "cancelled"]);

function loadTrayOpen(): boolean {
  try {
    return sessionStorage.getItem(TRAY_OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

function saveTrayOpen(open: boolean) {
  try {
    sessionStorage.setItem(TRAY_OPEN_KEY, open ? "1" : "0");
  } catch {}
}

function loadPersistedItems(): UploadItem[] {
  try {
    const raw = sessionStorage.getItem(TRAY_ITEMS_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return [];
    return arr.filter(
      (i) =>
        i &&
        typeof i.id === "string" &&
        typeof i.filename === "string" &&
        typeof i.createdAt === "number",
    ) as UploadItem[];
  } catch {
    return [];
  }
}

let lastPersisted = "";

function savePersistedItems(items: UploadItem[]) {
  try {
    const terminal = items
      .filter((i) => TERMINAL_STATES.has(i.state))
      .sort((a, b) => b.createdAt - a.createdAt)
      .slice(0, MAX_PERSISTED_ITEMS);
    const json = JSON.stringify(terminal);
    if (json === lastPersisted) return;
    lastPersisted = json;
    sessionStorage.setItem(TRAY_ITEMS_KEY, json);
  } catch {}
}

function mergeItems(persisted: UploadItem[], live: UploadItem[]): UploadItem[] {
  const byId = new Map<string, UploadItem>();
  for (const p of persisted) byId.set(p.id, p);
  for (const l of live) byId.set(l.id, l);
  return [...byId.values()];
}

function TrayFileCard(props: {
  id: string;
  locale: Locale;
  exiting: () => boolean;
  item: () => UploadItem;
  animActive: () => boolean;
  onRename: (data: { id: string; url: string }) => void;
  onCancel: () => void;
  onRemove: () => void;
  onDelete: () => void;
}) {
  let wrapRef: HTMLDivElement | undefined;
  const [wrapSignal, setWrapSignal] = createSignal<HTMLDivElement>();

  useEntrance(wrapSignal, props.animActive, "upload-tray__card--enter");

  createEffect(() => {
    if (!props.exiting() || !wrapRef) return;
    const el = wrapRef;
    el.style.height = `${el.offsetHeight}px`;
    el.style.overflow = "hidden";
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        el.style.height = "0px";
        el.style.marginBottom = "-1rem";
        el.style.opacity = "0";
      }),
    );
  });

  const item = () => props.item();

  return (
    <div
      ref={(el) => {
        wrapRef = el;
        setWrapSignal(el);
      }}
      class="upload-tray__card"
      classList={{ "upload-tray__card--leaving": props.exiting() }}
    >
      <FileCard
        locale={props.locale}
        mode="upload"
        id={item().id}
        filename={item().filename}
        mimeType={item().mimeType}
        size={item().size}
        url={item().url}
        serverId={item().serverId}
        deleteToken={item().deleteToken}
        reserveUrl={item().reserveUrl}
        state={item().state}
        progress={item().progress}
        errorCode={item().errorCode}
        errorMessage={item().errorMessage}
        errorStatus={item().errorStatus}
        errorStage={item().errorStage}
        expiresAt={item().expiresAt}
        uploadedAt={Math.floor(item().createdAt / 1000)}
        onRename={props.onRename}
        onCancel={props.onCancel}
        onRemove={props.onRemove}
        onDelete={props.onDelete}
      />
    </div>
  );
}

export default function UploadTray(props: { locale?: Locale }) {
  const locale = () => props.locale || "en";
  const [items, setItems] = createSignal<UploadItem[]>([]);
  const [open, setOpen] = createSignal(loadTrayOpen());
  const [closing, setClosing] = createSignal(false);
  const [gone, setGone] = createSignal(true);
  const [leaving, setLeaving] = createSignal(false);
  let leaveTimer: number | null = null;
  const [suppressAnim, setSuppressAnim] = createSignal(true);
  const [cards, setCards] = createSignal<TrayCard[]>([]);
  const [exitingIds, setExitingIds] = createSignal<Set<string>>(new Set());
  const exitTimers = new Map<string, number>();

  onMount(() => {
    const unsub = subscribe((list) => {
      const merged = mergeItems(
        loadPersistedItems().filter((i) => !wasRemoved(i.id)),
        list,
      );
      setItems(merged);
      savePersistedItems(merged);
      // Live feed for the network debug overlay (no-op unless listening).
      try {
        let done = 0;
        let active = 0;
        for (const it of merged) {
          if (it.state === "uploading" || it.state === "finalizing") {
            done += ((it.size || 0) * (it.progress || 0)) / 100;
            active++;
          }
        }
        window.dispatchEvent(
          new CustomEvent("jb-net-sample", {
            detail: {
              done,
              active,
              items: merged.slice(0, 8).map((i) => ({
                id: i.id,
                n: i.filename,
                s: i.state,
                p: i.progress,
                m: i.method,
                z: i.size,
                d: i.dbg,
                o: {
                  mode: i.uploadMode,
                  quick: i.quickLink,
                  ttl: i.ttlHours,
                  host: i.customHost || "",
                },
              })),
            },
          }),
        );
      } catch {}
    });
    const unsubDeleted = onFileDeleted((fileId) => {
      const match = items().find(
        (i) =>
          (i.serverId || extractServerId(i.url || "")) === fileId,
      );
      if (match) removeUpload(match.id);
    });
    connectUploads();
    const initial = mergeItems(loadPersistedItems(), getUploads());
    setItems(initial);
    savePersistedItems(initial);
    requestAnimationFrame(() => setSuppressAnim(false));
    onCleanup(() => {
      unsub();
      unsubDeleted();
    });
  });

  createEffect(() => {
    const current = items();
    const currentIds = new Set(current.map((i) => i.id));

    const prevExiting = exitingIds();
    let exitingChanged = false;
    const newExiting = new Set(prevExiting);
    for (const c of cards()) {
      if (!currentIds.has(c.id) && !newExiting.has(c.id)) {
        newExiting.add(c.id);
        exitingChanged = true;
      }
    }
    for (const id of newExiting) {
      if (currentIds.has(id)) {
        newExiting.delete(id);
        exitingChanged = true;
      }
    }
    if (exitingChanged) setExitingIds(newExiting);

    setCards((prev) => {
      const byId = new Map(prev.map((c) => [c.id, c]));
      const next: TrayCard[] = [];
      for (const it of current) {
        const existing = byId.get(it.id);
        if (existing) {
          existing.item = it;
          next.push(existing);
        } else {
          next.push({ id: it.id, item: it });
        }
      }
      for (const c of prev) {
        if (!currentIds.has(c.id)) next.push(c);
      }
      if (
        next.length === prev.length &&
        next.every((c, i) => c === prev[i])
      ) {
        return prev;
      }
      return next;
    });
  });

  // Remove cards once their exit animation has finished.
  createEffect(() => {
    for (const id of exitingIds()) {
      if (exitTimers.has(id)) continue;
      exitTimers.set(
        id,
        window.setTimeout(() => {
          exitTimers.delete(id);
          setCards((prev) => prev.filter((c) => c.id !== id));
          setExitingIds((prev) => {
            const next = new Set(prev);
            next.delete(id);
            return next;
          });
        }, 320),
      );
    }
  });

  createEffect(() => {
    if (items().length > 0) {
      if (leaveTimer != null) {
        clearTimeout(leaveTimer);
        leaveTimer = null;
      }
      setLeaving(false);
      setGone(false);
    } else if (!gone() && !leaving()) {
      setLeaving(true);
      if (open()) {
        setClosing(true);
        window.setTimeout(() => {
          setOpen(false);
          setClosing(false);
          saveTrayOpen(false);
        }, 250);
      }
      leaveTimer = window.setTimeout(() => {
        leaveTimer = null;
        setGone(true);
        setLeaving(false);
      }, 1500);
    }
  });

  onCleanup(() => {
    if (leaveTimer != null) clearTimeout(leaveTimer);
    for (const timer of exitTimers.values()) clearTimeout(timer);
    exitTimers.clear();
  });

  const toggle = () => {
    if (closing()) return;
    if (open()) {
      setClosing(true);
      setTimeout(() => {
        setOpen(false);
        setClosing(false);
        saveTrayOpen(false);
      }, 250);
    } else {
      setOpen(true);
      saveTrayOpen(true);
    }
  };

  const activeCount = () =>
    items().filter(
      (i) =>
        i.state !== "done" &&
        i.state !== "error" &&
        i.state !== "cancelled",
    ).length;

  const animActive = () => !suppressAnim();
  const [toggleWrapSignal, setToggleWrapSignal] =
    createSignal<HTMLSpanElement>();
  const [panelSignal, setPanelSignal] = createSignal<HTMLDivElement>();
  useEntrance(
    toggleWrapSignal,
    animActive,
    "upload-tray__toggle-wrap--enter",
  );
  useEntrance(panelSignal, animActive, "upload-tray__panel--enter");

  const deleteFile = async (item: UploadItem) => {
    const fileId = item.serverId || extractServerId(item.url || "");
    if (!fileId || !item.deleteToken) return;
    try {
      const res = await fetch(`${UPLOAD_URL}/file/${fileId}`, {
        method: "DELETE",
        headers: { "X-Delete-Token": item.deleteToken },
      });
      if (res.ok) {
        removeLocalFile(fileId);
        emitFileDeleted(fileId);
        removeUpload(item.id);
      }
    } catch {}
  };

  return (
    <Show when={!gone()}>
      <div class="upload-tray" data-upload-tray="">
        <span
          ref={setToggleWrapSignal}
          class="upload-tray__toggle-wrap"
          classList={{ "upload-tray__toggle-wrap--leaving": leaving() }}
          style={{ "view-transition-name": "upload-tray-toggle" }}
        >
          <button
            type="button"
            class="upload-tray__toggle"
            data-no-ripple
            aria-expanded={open()}
            aria-label={
              activeCount() > 0
                ? t(locale(), "tray.toggle_aria_active", { count: activeCount() })
                : t(locale(), "tray.toggle_aria")
            }
            title={t(locale(), "tray.title")}
            onClick={toggle}
          >
            <span class="upload-tray__icon" innerHTML={iconSvgHtml("upload", 24)} />
            <Show when={activeCount() > 0}>
              <span class="upload-tray__badge">{activeCount()}</span>
            </Show>
            <span
              class="upload-tray__chevron"
              classList={{
                "upload-tray__chevron--open": open() && !closing(),
              }}
              innerHTML={iconSvgHtml("chevron-up", 24)}
            />
          </button>
        </span>

        <Show when={open()}>
          <div
            ref={setPanelSignal}
            class="upload-tray__panel"
            classList={{ "upload-tray__panel--closing": closing() }}
            role="region"
            aria-label={t(locale(), "tray.title")}
            style={{ "view-transition-name": "upload-tray" }}
          >
            <div class="upload-tray__header">
              <span>{t(locale(), "tray.title")}</span>
              {activeCount() > 0 && (
                <span class="upload-tray__count">
                  {t(locale(), "tray.active", { count: activeCount() })}
                </span>
              )}
            </div>
            <div class="upload-tray__list">
              <For each={cards()}>
                {(card) => (
                  <TrayFileCard
                    id={card.id}
                    locale={locale()}
                    exiting={() => exitingIds().has(card.id)}
                    animActive={animActive}
                    item={() =>
                      items().find((i) => i.id === card.id) ?? card.item
                    }
                    onRename={({ id, url }) => {
                      const item = items().find((i) => i.id === card.id);
                      if (item) {
                        renameLocalFile(
                          item.serverId || item.id,
                          id,
                          url,
                          item.deleteToken,
                        );
                      }
                      updateUpload(card.id, { serverId: id, url });
                    }}
                    onCancel={() => cancelUpload(card.id)}
                    onRemove={() => removeUpload(card.id)}
                    onDelete={() => deleteFile(card.item)}
                  />
                )}
              </For>
            </div>
          </div>
          </Show>
      </div>
    </Show>
  );
}
