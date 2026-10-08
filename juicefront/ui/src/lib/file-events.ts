type FileDeletedListener = (fileId: string) => void;

const listeners = new Set<FileDeletedListener>();

export function onFileDeleted(listener: FileDeletedListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function emitFileDeleted(fileId: string) {
  for (const listener of listeners) {
    try {
      listener(fileId);
    } catch {}
  }
}

export function removeLocalFile(fileId: string) {
  if (typeof localStorage === "undefined") return;
  try {
    const raw = localStorage.getItem("juicebox_uploads");
    if (!raw) return;
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return;
    const next = arr.filter((f) => f && f.id !== fileId);
    if (next.length !== arr.length) {
      localStorage.setItem("juicebox_uploads", JSON.stringify(next));
    }
  } catch {}
}

/**
 * Swap a stored upload's id/url after a rename/renew. A rename keeps the
 * delete token, so we match by token first (single entry per file), falling
 * back to the old id. Persists only when something actually changed.
 */
export function renameLocalFile(
  oldId: string,
  newId: string,
  url: string,
  deleteToken?: string,
) {
  if (typeof localStorage === "undefined") return;
  try {
    const raw = localStorage.getItem("juicebox_uploads");
    if (!raw) return;
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return;
    let changed = false;
    for (const f of arr) {
      if (!f || typeof f.id !== "string") continue;
      const matchByToken =
        !!deleteToken && !!f.delete_token && f.delete_token === deleteToken;
      const matchById = f.id === oldId;
      if (matchByToken || matchById) {
        f.id = newId;
        f.url = url;
        changed = true;
      }
    }
    if (changed) {
      localStorage.setItem("juicebox_uploads", JSON.stringify(arr));
    }
  } catch {}
}
