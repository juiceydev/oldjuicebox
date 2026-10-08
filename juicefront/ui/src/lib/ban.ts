/** Extract the client IP from proxy headers on an incoming request. */
function extractClientIp(request: Request): string | null {
  const xff = request.headers.get("x-forwarded-for");
  if (xff) {
    const ip = xff.split(",")[0]?.trim();
    if (ip) return ip;
  }
  const xri = request.headers.get("x-real-ip");
  if (xri) {
    const ip = xri.trim();
    if (ip) return ip;
  }
  return null;
}

export interface BanStatus {
  banned: boolean;
  reason?: string;
  banned_at?: number;
}

/** SSR ban check -- queries juiceback with the user's real IP. */
export async function checkUserBan(request: Request): Promise<BanStatus> {
  const BACKEND_URL =
    process.env.JUICEBACK_URL || "";
  if (!BACKEND_URL) return { banned: false };
  const ip = extractClientIp(request);
  const url = ip
    ? `${BACKEND_URL}/api/ban-status?ip=${encodeURIComponent(ip)}`
    : `${BACKEND_URL}/api/ban-status`;
  try {
    const res = await fetch(url);
    if (res.ok) {
      const data = await res.json();
      if (data && data.banned) {
        return {
          banned: true,
          reason: data.reason,
          banned_at: data.banned_at,
        };
      }
    }
  } catch (err) {
    console.error("Ban check failed, treating user as not banned:", err);
  }
  return { banned: false };
}
