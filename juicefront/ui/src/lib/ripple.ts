/**
 * Adds a material-style ripple effect to buttons on click.
 * - Starts contracting on mousedown (pinch/hold), expands on mouseup.
 * - Ripple color adapts to the element's `color` CSS property.
 * - Noise overlay stays fixed-size (doesn't scale with the ripple).
 * - Skips action buttons (which have their own 3D press animation).
 * - Only active when <html> has the .js class.
 * Re-injects styles on Astro View Transitions via astro:page-load.
 * Call once per page.
 */
export function initRipple() {
    const NOISE_BG = `url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='200'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.65' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='200' height='200' filter='url(%23n)' opacity='0.18'/%3E%3C/svg%3E")`;

    function injectStyles() {
        if (document.getElementById("ripple-styles")) return;
        const style = document.createElement("style");
        style.id = "ripple-styles";
        style.textContent = `
            .ripple-ready {
                position: relative !important;
                overflow: hidden !important;
            }
            .ripple-wave {
                position: absolute;
                border-radius: 50%;
                pointer-events: none;
                transform: scale(0);
                background: radial-gradient(circle, var(--ripple-color, rgba(255,255,255,0.35)) 0%, transparent 70%);
                z-index: 1;
                transition: none;
            }
            .ripple-wave--hold {
                transform: scale(0.35);
                transition: transform 0.25s cubic-bezier(0.4, 0, 0.2, 1);
            }
            .ripple-wave--release {
                transform: scale(4);
                opacity: 0;
                transition: transform 0.4s cubic-bezier(0, 0, 0.2, 1), opacity 0.4s ease-out;
            }
            .ripple-noise {
                position: absolute;
                inset: 0;
                border-radius: inherit;
                pointer-events: none;
                background: ${NOISE_BG};
                background-size: 200px 200px;
                opacity: 0;
                z-index: 2;
                mix-blend-mode: overlay;
                transition: opacity 0.3s ease-out;
            }
            .ripple-noise--visible {
                opacity: 1;
            }
        `;
        document.head.appendChild(style);
    }

    let currentRipple: HTMLSpanElement | null = null;
    let currentNoise: HTMLSpanElement | null = null;
    let currentBtn: HTMLElement | null = null;

    function createRipple(btn: HTMLElement, e: MouseEvent) {
        btn.classList.add("ripple-ready");

        const computed = getComputedStyle(btn);
        const color = computed.color || "rgba(255,255,255,0.35)";
        const match = color.match(/[\d.]+/g);
        const rippleColor = match
            ? `rgba(${match[0]},${match[1]},${match[2]},0.35)`
            : "rgba(255,255,255,0.35)";

        const rect = btn.getBoundingClientRect();
        const size = Math.max(rect.width, rect.height) * 2;
        const x = e.clientX - rect.left - size / 2;
        const y = e.clientY - rect.top - size / 2;

        const ripple = document.createElement("span");
        ripple.className = "ripple-wave";
        ripple.style.setProperty("--ripple-color", rippleColor);
        ripple.style.width = `${size}px`;
        ripple.style.height = `${size}px`;
        ripple.style.left = `${x}px`;
        ripple.style.top = `${y}px`;
        btn.appendChild(ripple);

        const noise = document.createElement("span");
        noise.className = "ripple-noise";
        btn.appendChild(noise);

        ripple.offsetHeight;
        ripple.classList.add("ripple-wave--hold");
        requestAnimationFrame(() => noise.classList.add("ripple-noise--visible"));

        currentRipple = ripple;
        currentNoise = noise;
        currentBtn = btn;
    }

    function releaseRipple() {
        if (currentRipple) {
            currentRipple.classList.remove("ripple-wave--hold");
            currentRipple.classList.add("ripple-wave--release");
            const r = currentRipple;
            let cleaned = false;
            const cleanup = () => {
                if (cleaned) return;
                cleaned = true;
                r.remove();
            };
            r.addEventListener("transitionend", cleanup, { once: true });
            setTimeout(cleanup, 500);
        }
        if (currentNoise) {
            currentNoise.classList.remove("ripple-noise--visible");
            const n = currentNoise;
            let cleaned = false;
            const cleanup = () => {
                if (cleaned) return;
                cleaned = true;
                n.remove();
            };
            n.addEventListener("transitionend", cleanup, { once: true });
            setTimeout(cleanup, 400);
        }
        currentRipple = null;
        currentNoise = null;
        currentBtn = null;
    }

    function isJsActive() {
        return document.documentElement.classList.contains("js");
    }

    function findButton(target: HTMLElement) {
        const btn = target.closest(
            "button, [role='button'], .delete-btn, .file-action-btn, .file-confirm-btn, .report-btn, .mdlbtn, .upload-submit, [data-drop-zone], .select-option, .faq-question, .copy-bar-edit, .copy-bar-qr",
        ) as HTMLElement | null;
        if (btn?.hasAttribute("data-no-ripple")) return null;
        return btn;
    }

    document.addEventListener("mousedown", (e) => {
        if (!isJsActive()) return;
        const btn = findButton(e.target as HTMLElement);
        if (!btn) return;
        createRipple(btn, e);
    });

    document.addEventListener("mouseup", () => {
        if (!isJsActive()) return;
        releaseRipple();
    });

    // Release ripple if cursor leaves the button while held
    document.addEventListener("mouseleave", (e) => {
        if (!isJsActive() || !currentBtn) return;
        const related = e.relatedTarget as HTMLElement | null;
        if (!related || !currentBtn.contains(related)) {
            releaseRipple();
        }
    });

    // Release ripple if a drag starts (text selection, native drag)
    document.addEventListener("dragstart", () => {
        if (!isJsActive()) return;
        releaseRipple();
    });

    // Release ripple on pointer cancel (touch interruption, etc.)
    document.addEventListener("pointercancel", () => {
        if (!isJsActive()) return;
        releaseRipple();
    });

    document.addEventListener("astro:page-load", () => {
        currentRipple = null;
        currentNoise = null;
        currentBtn = null;
        injectStyles();
    });

    injectStyles();
}
