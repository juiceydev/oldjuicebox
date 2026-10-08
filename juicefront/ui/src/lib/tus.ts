/** Base64-encode TUS metadata per the TUS protocol spec. */
export function encodeTusMeta(obj: Record<string, string>): string {
    return Object.entries(obj)
        .map(([k, v]) => `${k} ${btoa(unescape(encodeURIComponent(v)))}`)
        .join(",");
}
