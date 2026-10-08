// Subtle haptic feedback tuned for modern LRA motors (Pixel-class devices):
// every pattern is a short, crisp tick stack - never a long coarse buzz.
// Web Vibration API only fires on Android; iOS ignores it silently.

const KEY = "jb_haptics_v1";

export type HapticName = "tick" | "tap" | "send" | "success" | "error";

/** Durations in ms; arrays are vibrate/pause/vibrate... */
const PATTERNS: Record<HapticName, number | number[]> = {
  tick: 8,
  tap: 16,
  send: [12, 30, 16],
  success: [14, 60, 14], // two short pulses on finished uploads
  error: [26, 70, 26, 70, 40],
};

export function hapticsSupported(): boolean {
  return typeof navigator !== "undefined" && "vibrate" in navigator;
}

export function hapticsEnabled(): boolean {
  try {
    return localStorage.getItem(KEY) !== "0";
  } catch {
    return true;
  }
}

export function setHapticsEnabled(v: boolean): void {
  try {
    localStorage.setItem(KEY, v ? "1" : "0");
  } catch {}
}

export function haptic(name: HapticName): void {
  if (!hapticsEnabled() || !hapticsSupported()) return;
  try {
    navigator.vibrate(PATTERNS[name]);
  } catch {}
}

/** Wire global press feedback + upload lifecycle cues. */
export function initHaptics(): void {
  if (!hapticsSupported() || (window as unknown as { __jbBuzz?: boolean }).__jbBuzz)
    return;
  (window as unknown as { __jbBuzz?: boolean }).__jbBuzz = true;

  // Crisp press feedback via capture-phase pointerdown (fires before focus,
  // works for dynamically rendered cards, and can't be blocked by overlays).
  const INTERACTIVE =
    'button:not(:disabled),a[href],[role="button"],summary,label,input[type="checkbox"],input[type="radio"],select';
  document.addEventListener(
    "pointerdown",
    (e) => {
      if (!(e.target instanceof Element)) return;
      const t = e.target.closest(INTERACTIVE);
      if (!t || t.hasAttribute("data-no-buzz")) return;
      haptic(t.matches("a[href]") ? "tick" : "tap");
    },
    { capture: true, passive: true },
  );

  // Upload outcomes: fire once per state transition to success/error.
  const seen = new Map<string, string>();
  window.addEventListener("jb-net-sample", (ev) => {
    const detail = (ev as CustomEvent).detail as {
      items?: { id?: string; s?: string }[];
    };
    for (const it of detail?.items ?? []) {
      if (!it.id || !it.s) continue;
      const prev = seen.get(it.id);
      if (prev !== it.s) {
        if (it.s === "done") haptic("success");
        else if (it.s === "error") haptic("error");
        else if (!prev && it.s === "uploading") haptic("send");
        seen.set(it.id, it.s);
      }
    }
    if (seen.size > 200) seen.clear();
  });
}
