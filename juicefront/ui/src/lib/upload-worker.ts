import {
  startUpload,
  generateId,
  type UploadHandle,
  type UploadItem,
  type UploadOptions,
} from "./upload-engine";
import { UPLOAD_URL, TUS_THRESHOLD } from "./upload-config";

interface Entry {
  item: UploadItem;
  handle: UploadHandle | null;
  tusIds: string[];
}

const ctx = self as unknown as {
  onconnect: ((event: MessageEvent) => void) | null;
};

const entries = new Map<string, Entry>();
const dismissed = new Set<string>();
const ports = new Set<MessagePort>();
const MAX_ITEMS = 100;

function emit(message: unknown) {
  for (const port of ports) {
    try {
      port.postMessage(message);
    } catch {}
  }
}

function entryItem(item: UploadItem) {
  if (dismissed.has(item.id)) return;
  emit({ type: "item", item });
}

function upsert(patch: Partial<UploadItem> & { id: string }) {
  let entry = entries.get(patch.id);
  if (!entry) {
    entry = { item: {} as UploadItem, handle: null, tusIds: [] };
    entries.set(patch.id, entry);
  }
  Object.assign(entry.item, patch);
  entryItem(entry.item);
  trim();
}

function trim() {
  if (entries.size <= MAX_ITEMS) return;
  const finished = [...entries.values()]
    .filter(
      (e) =>
        e.item.state === "done" ||
        e.item.state === "error" ||
        e.item.state === "cancelled",
    )
    .sort((a, b) => a.item.createdAt - b.item.createdAt);
  while (entries.size > MAX_ITEMS && finished.length) {
    const oldest = finished.shift()!;
    entries.delete(oldest.item.id);
  }
}

function handleEnqueue(
  msg: {
    id: string;
    filename: string;
    mimeType: string;
    size: number;
    ttlHours: number;
    options: UploadOptions;
    file: File;
  },
  _port: MessagePort,
) {
  if (entries.has(msg.id)) return;
  const item: UploadItem = {
    id: msg.id,
    filename: msg.filename,
    mimeType: msg.mimeType,
    size: msg.size,
    method: msg.size > TUS_THRESHOLD ? "tus" : "direct",
    state: "queued",
    progress: 0,
    ttlHours: msg.ttlHours,
    quickLink: msg.options.quickLink,
    customHost: msg.options.customHost,
    uploadMode: msg.options.uploadMode,
    createdAt: Date.now(),
  };
  const entry: Entry = { item, handle: null, tusIds: [] };
  entries.set(item.id, entry);
  entryItem(item);

  entry.handle = startUpload(item, msg.file, {
    update: upsert,
    onTusCreate: (tusId) => {
      entry.tusIds.push(tusId);
    },
    onTusDelete: (tusId) => {
      fetch(`${UPLOAD_URL}/api/tus/${tusId}`, { method: "DELETE" }).catch(
        () => {},
      );
    },
  });
}

function handleCancel(id: string) {
  const entry = entries.get(id);
  if (!entry) return;
  entry.handle?.abort();
  for (const tusId of entry.tusIds) {
    fetch(`${UPLOAD_URL}/api/tus/${tusId}`, { method: "DELETE" }).catch(
      () => {},
    );
  }
  if (
    entry.item.state === "queued" ||
    entry.item.state === "compressing"
  ) {
    upsert({ id, state: "cancelled" });
  }
}

function handleRemove(id: string) {
  dismissed.add(id);
  entries.delete(id);
  emit({ type: "removed", id });
}

ctx.onconnect = (event) => {
  const port = event.ports[0];
  ports.add(port);

  port.onmessage = (e: MessageEvent) => {
    const msg = (e.data || {}) as Record<string, unknown> & {
      type?: string;
      id?: string;
      patch?: Partial<UploadItem>;
    };
    if (msg.type === "enqueue") {
      handleEnqueue(
        msg as unknown as Parameters<typeof handleEnqueue>[0],
        port,
      );
    } else if (msg.type === "cancel" && msg.id) {
      handleCancel(msg.id);
    } else if (msg.type === "remove" && msg.id) {
      handleRemove(msg.id);
    } else if (msg.type === "patch" && msg.id && msg.patch) {
      upsert({ id: msg.id, ...(msg.patch as Record<string, unknown>) });
    }
  };
  port.start();

  port.postMessage({
    type: "snapshot",
    items: [...entries.values()]
      .filter((e) => !dismissed.has(e.item.id))
      .map((e) => e.item),
  });

  port.addEventListener("close", () => {
    ports.delete(port);
  });
};
