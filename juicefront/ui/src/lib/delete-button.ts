import { iconHTML, announce } from "./format";

export interface DeleteButtonOpts {
    fileId: string;
    deleteToken: string;
    apiBase: string;
    /** Called after successful delete to remove the item from the DOM and check empty state. */
    onDeleted: (item: HTMLElement) => void;
    /** If true, skip the confirmation step and delete immediately on first click. */
    immediate?: boolean;
}

// WeakMap to store confirm timers without polluting DOM with expandos
const confirmTimers = new WeakMap<HTMLButtonElement, ReturnType<typeof setTimeout>>();

/**
 * Creates a delete button with shift-to-confirm behavior.
 * First click enters "confirming" state (3s timeout), second click (or shift+click) deletes.
 * If `immediate` is set, deletes on first click without confirmation.
 */
export function createDeleteButton(opts: DeleteButtonOpts): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "delete-btn";
    btn.setAttribute("aria-label", "Delete file");
    btn.innerHTML = iconHTML("trash", 24);

    btn.addEventListener("click", async (e) => {
        if (opts.immediate || e.shiftKey || btn.dataset.confirming) {
            try {
                const res = await fetch(
                    `${opts.apiBase}/file/${opts.fileId}`,
                    {
                        method: "DELETE",
                        headers: {
                            "X-Delete-Token": opts.deleteToken,
                        },
                    },
                );
                if (res.ok) {
                    const item = btn.closest(".file-item, .file-card") as HTMLElement | null;
                    if (item) opts.onDeleted(item);
                    announce("File deleted");
                }
            } catch (err) {
                    console.error("Failed to delete file:", err);
                    announce("Failed to delete file");
                }
        } else {
            btn.dataset.confirming = "true";
            const clr = setTimeout(() => {
                btn.dataset.confirming = "";
            }, 10000);
            confirmTimers.set(btn, clr);
        }
    });

    return btn;
}
