const UPLOAD_URL = import.meta.env.PUBLIC_API_URL || "";

let countdownInterval: ReturnType<typeof setInterval> | null = null;
let pollInterval: ReturnType<typeof setInterval> | null = null;

export function initPairModal() {
  const generateBtn = document.getElementById("pair-generate-btn");
  const unpairBtn = document.getElementById("pair-unpair-btn");
  const codeWrap = document.getElementById("pair-code-text");
  if (generateBtn) generateBtn.addEventListener("click", handleGenerate);
  if (unpairBtn) unpairBtn.addEventListener("click", handleUnpair);
  if (codeWrap) codeWrap.addEventListener("click", handleCopyCode);
  checkPairingStatus();
  updateDeviceStatus();

  window.addEventListener("juicebox-app-mode", () => updateDeviceStatus());
}

async function handleGenerate() {
  const generateBtn = document.getElementById("pair-generate-btn")!;
  const codeDisplay = document.getElementById("pair-code-display")!;
  const codeText = document.getElementById("pair-code-text")!.querySelector(".pair-code__code")!;
  const expiresText = document.getElementById("pair-expires-text")!;
  const statusEl = document.getElementById("pair-status")!;

  generateBtn.disabled = true;
  statusEl.hidden = true;

  try {
    const res = await fetch(`${UPLOAD_URL}/api/pair/generate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({}),
    });

    if (!res.ok) throw new Error(`HTTP ${res.status}`);

    const data = await res.json();
    codeText.textContent = data.code;
    codeDisplay.hidden = false;
    generateBtn.hidden = true;

    let remaining = data.expires_in;
    expiresText.textContent = `Expires in ${remaining}s`;

    countdownInterval = setInterval(() => {
      remaining--;
      expiresText.textContent = `Expires in ${remaining}s`;
      if (remaining <= 0) {
        clearInterval(countdownInterval!);
        codeDisplay.hidden = true;
        generateBtn.hidden = false;
        generateBtn.disabled = false;
        expiresText.textContent = "";
      }
    }, 1000);

    startPairingPoll();
  } catch {
    statusEl.textContent = "Failed to generate code. Try again.";
    statusEl.className = "pair-status pair-status--error";
    statusEl.hidden = false;
    generateBtn.disabled = false;
  }
}

function handleCopyCode() {
  const codeWrap = document.getElementById("pair-code-text");
  if (!codeWrap) return;
  const code = codeWrap.querySelector(".pair-code__code")?.textContent?.trim();
  if (!code || code === "----") return;
  navigator.clipboard.writeText(code).then(() => {
    codeWrap.classList.add("pair-code--copied");
    setTimeout(() => codeWrap.classList.remove("pair-code--copied"), 2000);
  });
}

function startPairingPoll() {
  let attempts = 0;
  const maxAttempts = 100;

  pollInterval = setInterval(async () => {
    attempts++;
    if (attempts >= maxAttempts) {
      clearInterval(pollInterval);
      return;
    }

    try {
      const res = await fetch(`${UPLOAD_URL}/api/device`, {
        signal: AbortSignal.timeout(3000),
      });
      if (res.ok) {
        const devices = await res.json();
        if (Array.isArray(devices) && devices.length > 0) {
          clearInterval(pollInterval);
          clearInterval(countdownInterval);
          const device = devices[devices.length - 1];
          showPairedState(device.device_name, device.paired_at);
          try { localStorage.setItem("juicebox_paired", "true"); } catch {}
          try { localStorage.setItem("juicebox_device_id", device.device_id); } catch {}
          try { localStorage.setItem("juicebox_device_name", device.device_name); } catch {}
        }
      }
    } catch {
    }
  }, 3000);
}

function showPairedState(deviceName: string, pairedAt: number) {
  const notPaired = document.getElementById("pair-not-paired")!;
  const paired = document.getElementById("pair-paired")!;
  const deviceNameEl = document.getElementById("pair-device-name")!;
  const pairedDateEl = document.getElementById("pair-paired-date")!;

  notPaired.hidden = true;
  paired.hidden = false;
  deviceNameEl.textContent = deviceName;
  pairedDateEl.textContent = `Paired since ${new Date(pairedAt * 1000).toLocaleDateString()}`;
}

async function handleUnpair() {
  const deviceId = localStorage.getItem("juicebox_device_id");
  if (!deviceId) return;
  if (!confirm("Unpair this device?")) return;

  try {
    const res = await fetch(`${UPLOAD_URL}/api/device/${deviceId}`, {
      method: "DELETE",
    });
    if (res.ok) {
      localStorage.removeItem("juicebox_device_id");
      localStorage.removeItem("juicebox_paired");
      localStorage.removeItem("juicebox_device_name");
      showNotPairedState();
    }
  } catch {}
}

function showNotPairedState() {
  const notPaired = document.getElementById("pair-not-paired")!;
  const paired = document.getElementById("pair-paired")!;
  const generateBtn = document.getElementById("pair-generate-btn")!;
  const codeDisplay = document.getElementById("pair-code-display")!;

  notPaired.hidden = false;
  paired.hidden = true;
  generateBtn.hidden = false;
  generateBtn.disabled = false;
  codeDisplay.hidden = true;
}

function checkPairingStatus() {
  const paired = localStorage.getItem("juicebox_paired") === "true";
  if (paired) {
    const deviceName = localStorage.getItem("juicebox_device_name") || "Device";
    showPairedState(deviceName, Date.now() / 1000);
  }
}

function updateDeviceStatus() {
  const label = document.getElementById("pair-status-label");
  if (!label) return;

  const active = window.__juiceboxAppMode === true;
  label.textContent = active ? "Connected!" : "Not Connected";
}
