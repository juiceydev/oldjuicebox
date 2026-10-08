/** SSR health check -- so NOJS users see the offline state too. */
export async function checkBackendHealth(): Promise<boolean> {
  const BACKEND_URL =
    process.env.JUICEBACK_URL || "http://127.0.0.1:6401";
  try {
    const res = await fetch(`${BACKEND_URL}/api/health`);
    if (!res.ok) throw new Error("not ok");
    const data = await res.json();
    return data.status === "ok";
  } catch (err) {
    console.error("Health check failed:", err);
    return false;
  }
}
