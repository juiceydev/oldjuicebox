/**
 * Transforms server-rendered retention radio buttons into an animated slider UI.
 * Self-contained -- takes a card element and enhances the [data-rttoolbar] within it.
 */
import { t, type Locale } from "../i18n";
import { readAllowedTtlHours, readDefaultTtlHours } from "./upload-config";
import { ttlLabel } from "./retention";

/**
 * The user's selected retention (in hours) is persisted across page loads so
 * the toolbar remembers the choice after a refresh, even though the slider
 * itself is rebuilt from the current host's SSR config each time.
 */
const TTL_STORAGE_KEY = "juicebox_ttl_hours";

function readStoredTtl(): string | null {
    try {
        return localStorage.getItem(TTL_STORAGE_KEY);
    } catch {
        return null;
    }
}

function storeTtl(hours: string) {
    try {
        localStorage.setItem(TTL_STORAGE_KEY, hours);
    } catch {
        /* storage unavailable; ignore */
    }
}

/**
 * (Re)build the retention radio options from the effective server config
 * embedded in #server-config, so the slider always reflects the current
 * host's allowed/default TTL values (including after Apply swaps hosts).
 * Falls back to the existing SSR radios when no config is embedded.
 */
function resolveRadios(card: Element, locale: Locale): HTMLInputElement[] | null {
    const fieldset = card.querySelector(".rtt-options") as HTMLElement | null;
    if (!fieldset) return null;

    const allowed = readAllowedTtlHours();
    const def = readDefaultTtlHours();
    if (allowed.length === 0) {
        return Array.from(
            fieldset.querySelectorAll('input[type="radio"]'),
        ) as HTMLInputElement[];
    }

    fieldset.innerHTML = "";
    for (const hours of allowed) {
        const label = document.createElement("label");
        label.className = "rtt-option";
        const input = document.createElement("input");
        input.type = "radio";
        input.name = "ttl_hours";
        input.value = String(hours);
        input.checked = hours === def;
        const span = document.createElement("span");
        span.textContent = ttlLabel(locale, hours);
        label.append(input, span);
        fieldset.appendChild(label);
    }
    return Array.from(
        fieldset.querySelectorAll('input[type="radio"]'),
    ) as HTMLInputElement[];
}

export function enhanceRetention(card: Element, locale: Locale = "en"): { destroy: () => void } | void {
    const toolbar = card.querySelector(
        "[data-rttoolbar]",
    ) as HTMLElement | null;
    const fieldset = toolbar?.querySelector(
        ".rtt-options",
    ) as HTMLElement | null;
    if (!toolbar || !fieldset) return;

    const radios = resolveRadios(card, locale);
    if (!radios || radios.length === 0) return;

    // Restore the previously selected retention if the current host still
    // allows it; otherwise fall back to the host default.
    const stored = readStoredTtl();
    if (stored !== null && radios.some((r) => r.value === stored)) {
        radios.forEach((r) => (r.checked = r.value === stored));
    }

    const labels = radios.map(
        (r) => r.nextElementSibling?.textContent?.trim() ?? "",
    );
    let index = radios.findIndex((r) => r.checked);
    if (index < 0) index = 0;

    toolbar.dataset.enhanced = "";
    const height = toolbar.offsetHeight;
    toolbar.style.minHeight = `${height}px`;
    fieldset.hidden = true;

    const info = toolbar.querySelector(".rttinfo");
    const valueWrap = document.createElement("div");
    valueWrap.className = "rttvalue-container";
    info?.appendChild(valueWrap);

    let lastIndex = index;
    let cleanupTimer = 0;

    const splitLabel = (label: string) => {
        const m = label.match(/^(\d+(?:\.\d+)?)(.*)$/);
        const num = m ? m[1] : "";
        const unit = m ? m[2].trim() : "";
        const hasS = unit.endsWith("s") && unit.length > 1;
        return {
            number: num,
            unit,
            base: hasS ? unit.slice(0, -1) : unit,
            hasS,
        };
    };

    const setValue = (i: number) => {
        const nw = splitLabel(labels[i]);

        let wrapper = valueWrap.querySelector(
            ".rttvalue-wrapper",
        ) as HTMLElement | null;

        if (!wrapper) {
            wrapper = document.createElement("div");
            wrapper.className = "rttvalue-wrapper";
            wrapper.innerHTML =
                '<span class="rttnumber"></span><span class="rttunit-base"></span>';
            wrapper.querySelector(".rttnumber")!.textContent = nw.number;
            wrapper.querySelector(".rttunit-base")!.textContent = nw.base;
            if (nw.hasS) {
                const s = document.createElement("span");
                s.className = "rttunit-s";
                s.textContent = "s";
                wrapper.appendChild(s);
            }
            valueWrap.appendChild(wrapper);
            lastIndex = i;
            return;
        }

        if (i === lastIndex) return;

        clearTimeout(cleanupTimer);
        wrapper
            .querySelectorAll("[data-exit-clone]")
            .forEach((el) => el.remove());

        const isUp = i > lastIndex;
        const exitClass = isUp ? "animate-up-exit" : "animate-down-exit";
        const enterClass = isUp ? "animate-up-enter" : "animate-down-enter";

        const old = splitLabel(labels[lastIndex]);

        const numberEl = wrapper.querySelector(".rttnumber") as HTMLElement;
        const baseEl = wrapper.querySelector(
            ".rttunit-base",
        ) as HTMLElement;
        let suffixEl = wrapper.querySelector(
            ".rttunit-s",
        ) as HTMLElement | null;

        const baseChanged = old.base !== nw.base;

        const wr = wrapper.getBoundingClientRect();
        const toRemove: HTMLElement[] = [];

        const nRect = numberEl.getBoundingClientRect();
        const exitN = numberEl.cloneNode(true) as HTMLElement;
        exitN.dataset.exitClone = "";
        exitN.style.position = "absolute";
        exitN.style.left = `${nRect.left - wr.left}px`;
        exitN.style.top = `${nRect.top - wr.top}px`;
        exitN.style.width = `${nRect.width}px`;
        exitN.style.margin = "0";
        exitN.style.pointerEvents = "none";
        exitN.className = `rttnumber ${exitClass}`;
        wrapper.appendChild(exitN);
        toRemove.push(exitN);

        if (baseChanged) {
            const bRect = baseEl.getBoundingClientRect();
            const exitB = baseEl.cloneNode(true) as HTMLElement;
            exitB.dataset.exitClone = "";
            exitB.style.position = "absolute";
            exitB.style.left = `${bRect.left - wr.left}px`;
            exitB.style.top = `${bRect.top - wr.top}px`;
            exitB.style.width = `${bRect.width}px`;
            exitB.style.margin = "0";
            exitB.style.pointerEvents = "none";
            exitB.className = `rttunit-base ${exitClass}`;
            wrapper.appendChild(exitB);
            toRemove.push(exitB);
        }

        const suffixChanges = old.hasS !== nw.hasS || old.base !== nw.base;
        if (suffixChanges && suffixEl) {
            const sRect = suffixEl.getBoundingClientRect();
            const exitS = suffixEl.cloneNode(true) as HTMLElement;
            exitS.dataset.exitClone = "";
            exitS.style.position = "absolute";
            exitS.style.left = `${sRect.left - wr.left}px`;
            exitS.style.top = `${sRect.top - wr.top}px`;
            exitS.style.width = `${sRect.width}px`;
            exitS.style.margin = "0";
            exitS.style.pointerEvents = "none";
            exitS.className = `rttunit-s ${exitClass}`;
            wrapper.appendChild(exitS);
            toRemove.push(exitS);

            suffixEl.remove();
            suffixEl = null;
        }

        numberEl.textContent = nw.number;
        numberEl.className = "rttnumber";

        baseEl.textContent = nw.base;
        baseEl.className = "rttunit-base";

        if (suffixChanges && nw.hasS) {
            const s = document.createElement("span");
            s.className = `rttunit-s ${enterClass}`;
            s.textContent = "s";
            wrapper.appendChild(s);
            window.setTimeout(() => {
                s.className = "rttunit-s";
            }, 200);
        }

        requestAnimationFrame(() => {
            numberEl.classList.add(enterClass);
            if (baseChanged) baseEl.classList.add(enterClass);
        });

        cleanupTimer = window.setTimeout(() => {
            toRemove.forEach((el) => el.remove());
        }, 200);

        lastIndex = i;
    };

    const sliderWrap = document.createElement("div");
    sliderWrap.className = "slider-wrapper";
    const slider = document.createElement("input");
    slider.type = "range";
    slider.className = "rttslider";
    slider.setAttribute("aria-label", t(locale, "accessibility.retention_period"));
    slider.min = "0";
    slider.max = String(radios.length - 1);
    slider.step = "1";
    slider.value = String(index);

    const ticks = document.createElement("div");
    ticks.className = "slider-ticks";
    ticks.setAttribute("aria-hidden", "true");

    radios.forEach((_, i) => {
        const max = radios.length - 1;
        const p = max === 0 ? 0 : i / max;
        const offset = 10 - p * 16;
        const tick = document.createElement("div");
        tick.className = "tick" + (i === index ? " active" : "");
        tick.style.setProperty(
            "--tick-left",
            `calc(${p * 100}% + ${offset}px)`,
        );
        ticks.appendChild(tick);
    });
    sliderWrap.append(slider, ticks);
    toolbar.appendChild(sliderWrap);

    slider.addEventListener("input", () => {
        const i = Number(slider.value);
        radios[i].checked = true;
        ticks
            .querySelectorAll(".tick")
            .forEach((t, j) => t.classList.toggle("active", j === i));
        setValue(i);
        storeTtl(radios[i].value);
    });

    radios.forEach((r, i) => {
        r.addEventListener("change", () => {
            if (r.checked) {
                slider.value = String(i);
                ticks
                    .querySelectorAll(".tick")
                    .forEach((t, j) =>
                        t.classList.toggle("active", j === i),
                    );
                setValue(i);
                storeTtl(r.value);
            }
        });
    });

    setValue(index);

    return {
        destroy() {
            sliderWrap.remove();
            ticks.remove();
            const wrapper = valueWrap.querySelector(".rttvalue-wrapper");
            if (wrapper) wrapper.remove();
            valueWrap.querySelectorAll("[data-exit-clone]").forEach(el => el.remove());
            fieldset.hidden = false;
            toolbar.dataset.enhanced = "";
            toolbar.style.minHeight = "";
            valueWrap.remove();
            radios.forEach(r => r.checked = false);
            if (radios.length > 0) radios[0].checked = true;
            clearTimeout(cleanupTimer);
        }
    };
}

/**
 * Destroys the old retention slider and rebuilds it with fresh radio options
 * regenerated from the effective server config embedded in #server-config.
 * Call this after the server-config data attributes change
 * (e.g. when the user validates a custom juicehost).
 */
export function rebuildRetention(card: Element, locale: Locale = "en", destroyFn?: () => void) {
    if (destroyFn) destroyFn();
    return enhanceRetention(card, locale);
}
