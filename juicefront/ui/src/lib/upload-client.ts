import {
  startUpload,
  generateId,
  extractServerId,
  type UploadHandle,
  type UploadItem,
  type UploadOptions,
} from "./upload-engine";
import { UPLOAD_URL, TUS_THRESHOLD } from "./upload-config";

type Listener = (items: UploadItem[]) => void;

const listeners = new Set<Listener>();
let items: UploadItem[] = [];
const persistedIds = new Set<string>();
const removedIds = new Set<string>();

export function getUploads(): UploadItem[] {
  return items;
}

export function subscribe(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function publish() {
  const snapshot = items;
  for (const listener of listeners) {
    try {
      listener(snapshot);
    } catch {}
  }
}

function upsertItem(item: UploadItem) {
  const idx = items.findIndex((i) => i.id === item.id);
  items = idx >= 0 ? items.map((i, n) => (n === idx ? item : i)) : [...items, item];
  maybePersist(item);
  publish();
}

function removeItem(id: string) {
  items = items.filter((i) => i.id !== id);
  removedIds.add(id);
  publish();
}

export function wasRemoved(id: string): boolean {
  return removedIds.has(id);
}

function reset(snapshot: UploadItem[]) {
  items = snapshot;
  for (const it of snapshot) maybePersist(it);
  publish();
}

export interface EnqueueInput {
  file: File;
  ttlHours: number;
  customHost: string;
  uploadMode: string;
  quickLink: boolean;
}

let port: MessagePort | null = null;

function onWorkerMessage(e: MessageEvent) {
  const msg = (e.data || {}) as {
    type?: string;
    items?: UploadItem[];
    item?: UploadItem;
    id?: string;
  };
  if (msg.type === "snapshot" && Array.isArray(msg.items)) {
    reset(msg.items);
  } else if (msg.type === "item" && msg.item) {
    upsertItem(msg.item);
  } else if (msg.type === "removed" && msg.id) {
    removeItem(msg.id);
  }
}

function ensurePort(): MessagePort | null {
  if (typeof window === "undefined") return null;
  if (port) return port;
  try {
    if (typeof SharedWorker === "undefined") return null;
    const sw = new SharedWorker(
      new URL("./upload-worker.ts", import.meta.url),
      { name: "juicebox-uploads", type: "module" },
    );
    sw.port.onmessage = onWorkerMessage;
    sw.port.start();
    port = sw.port;
  } catch {
    port = null;
  }
  return port;
}

const localItems = new Map<string, UploadItem>();
const localHandles = new Map<string, UploadHandle>();

function localUpsert(patch: Partial<UploadItem> & { id: string }) {
  let it = localItems.get(patch.id);
  if (!it) {
    it = { ...(patch as UploadItem) };
    localItems.set(patch.id, it);
  }
  Object.assign(it, patch);
  upsertItem({ ...it });
}

function startLocal(item: UploadItem, input: EnqueueInput) {
  localItems.set(item.id, { ...item });
  const handle = startUpload(item, input.file, {
    update: localUpsert,
    onTusCreate: () => {},
    onTusDelete: (tusId) => {
      fetch(`${UPLOAD_URL}/api/tus/${tusId}`, { method: "DELETE" }).catch(
        () => {},
      );
    },
  });
  localHandles.set(item.id, handle);
}

export function enqueueUpload(input: EnqueueInput): string {
  const id = generateId();
  const item: UploadItem = {
    id,
    filename: input.file.name,
    mimeType: input.file.type || "application/octet-stream",
    size: input.file.size,
    method: input.file.size > TUS_THRESHOLD ? "tus" : "direct",
    state: "queued",
    progress: 0,
    ttlHours: input.ttlHours,
    quickLink: input.quickLink,
    customHost: input.customHost,
    uploadMode: input.uploadMode,
    createdAt: Date.now(),
  };
  upsertItem({ ...item });

  const p = ensurePort();
  if (p) {
    try {
      const options: UploadOptions = {
        ttlHours: input.ttlHours,
        customHost: input.customHost,
        uploadMode: input.uploadMode,
        quickLink: input.quickLink,
      };
      p.postMessage({
        type: "enqueue",
        id,
        filename: item.filename,
        mimeType: item.mimeType,
        size: item.size,
        ttlHours: input.ttlHours,
        options,
        file: input.file,
      });
    } catch {
      startLocal(item, input);
    }
  } else {
    startLocal(item, input);
  }
  return id;
}

export function connectUploads(): void {
  ensurePort();
}

export function cancelUpload(id: string) {
  const p = ensurePort();
  if (p) {
    try {
      p.postMessage({ type: "cancel", id });
    } catch {}
    return;
  }
  localHandles.get(id)?.abort();
}

export function removeUpload(id: string) {
  const p = ensurePort();
  if (p) {
    try {
      p.postMessage({ type: "remove", id });
    } catch {}
  }
  removeItem(id);
}

export function updateUpload(id: string, patch: Partial<UploadItem>) {
  const p = ensurePort();
  if (p) {
    try {
      p.postMessage({ type: "patch", id, patch });
    } catch {}
    return;
  }
  localUpsert({ id, ...patch });
}

function maybePersist(item: UploadItem) {
  if (item.state !== "done" || !item.url) return;
  if (persistedIds.has(item.id)) return;
  persistedIds.add(item.id);

  const write = () => {
    try {
      const stored = JSON.parse(localStorage.getItem("juicebox_uploads") || "[]");
      const arr = Array.isArray(stored) ? stored : [];
      const id =
        item.serverId ||
        extractServerId(item.url ?? "") ||
        item.id ||
        Math.random().toString(36).slice(2);
      const entry = {
        id,
        filename: item.filename,
        mime_type: item.mimeType,
        size_bytes: item.size,
        uploaded_at: Math.floor(Date.now() / 1000),
        url: item.url,
        expires_at: Math.floor(Date.now() / 1000) + item.ttlHours * 3600,
        delete_token: item.deleteToken || "",
      };
      const deduped = arr.filter(
        (e) =>
          !(
            e &&
            (e.id === id ||
              (e.id === "" &&
                e.filename === item.filename &&
                e.size_bytes === item.size))
          ),
      );
      deduped.push(entry);
      localStorage.setItem("juicebox_uploads", JSON.stringify(deduped));
    } catch {}
  };

  if (typeof navigator !== "undefined" && navigator.locks?.request) {
    navigator.locks
      .request("juicebox-uploads", () => {
        write();
        return Promise.resolve();
      })
      .catch(() => write());
  } else if (typeof localStorage !== "undefined") {
    write();
  }
}
