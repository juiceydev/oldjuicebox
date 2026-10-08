/**
 * SSE client for device presence.
 *
 * Connects to /api/presence and tracks connected devices.
 * Dispatches custom events so other parts of the UI can react.
 */

declare global {
  interface Window {
    __juiceboxAppMode: boolean;
  }
}

let eventSource: EventSource | null = null;
let connected = false;
let devices: Array<{ device_id: string; device_name: string }> = [];

/** Whether juicebox-plus is currently connected. */
export function isAppMode(): boolean {
  return connected && devices.length > 0;
}

/** Get the list of connected devices. */
export function getConnectedDevices(): Array<{ device_id: string; device_name: string }> {
  return devices;
}

/** Check if glow should be shown (device connected since last clear). */
export function shouldGlowHost(): boolean {
  if (!isAppMode()) return false;
  const cleared = localStorage.getItem("juicebox_host_glow_cleared");
  if (cleared === "true") return false;
  return true;
}

/** Mark the host glow as cleared (called when user opens host settings). */
export function clearHostGlow(): void {
  localStorage.setItem("juicebox_host_glow_cleared", "true");
  document.querySelector("[data-host-btn]")?.classList.remove("host-btn--glow");
}

/** Mark that a new device has connected. */
export function onDeviceConnected(): void {
  window.__juiceboxAppMode = true;
  if (shouldGlowHost()) {
    document.querySelector("[data-host-btn]")?.classList.add("host-btn--glow");
  }
  window.dispatchEvent(new CustomEvent("juicebox-app-mode", { detail: { connected: true, devices: [...devices] } }));
}

/** Mark that devices have disconnected. */
export function onDeviceDisconnected(): void {
  window.__juiceboxAppMode = devices.length > 0;
  window.dispatchEvent(new CustomEvent("juicebox-app-mode", { detail: { connected: isAppMode(), devices: [...devices] } }));
}

/** Connect to the SSE presence stream. */
export function connectPresence(): void {
  if (eventSource) return;

  eventSource = new EventSource("/api/presence");

  eventSource.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      switch (data.type) {
        case "device_list":
          devices = data.devices || [];
          connected = devices.length > 0;
          window.__juiceboxAppMode = connected;
          if (connected) onDeviceConnected();
          else onDeviceDisconnected();
          break;
        case "device_connected":
          devices.push({ device_id: data.device_id, device_name: data.device_name });
          connected = true;
          onDeviceConnected();
          break;
        case "device_disconnected":
          devices = devices.filter((d) => d.device_id !== data.device_id);
          connected = devices.length > 0;
          onDeviceDisconnected();
          break;
        case "ping_response":
          window.dispatchEvent(new CustomEvent("juicebox-ping-response", { detail: { device_id: data.device_id } }));
          break;
      }
    } catch {}
  };

  eventSource.onerror = () => {
    connected = false;
    eventSource = null;
    setTimeout(connectPresence, 5000);
  };
}

/** Disconnect from the SSE presence stream. */
export function disconnectPresence(): void {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
  connected = false;
  devices = [];
}

/** Read whether UltraFast upload is enabled from localStorage. */
export function isUltraFastEnabled(): boolean {
  return localStorage.getItem("juicebox_ultrafast_enabled") === "true";
}

/** Set whether UltraFast upload is enabled. */
export function setUltraFastEnabled(enabled: boolean): void {
  localStorage.setItem("juicebox_ultrafast_enabled", enabled ? "true" : "false");
}
