/**
 * juicebox-bun-fetch: SABR-proof YouTube fetch sidecar.
 *
 * Runs on Bun. POST /fetch {url, audio_only?, max_height?} runs yt-dlp
 * locally with Bun as the JS challenge runtime + a bgutil PO-token provider
 * and streams the finished file back. juiceback uses this as its last-resort
 * YouTube tier when both cobalt instances and its own yt-dlp tier fail.
 *
 * Why this beats the in-juiceback yt-dlp tier for SABR enforcement:
 *  - On the SECONDARY (home/residential) box it egresses directly: no proxy
 *    hop, POT provider on localhost (no tailscale timeouts), residential IP
 *    that YouTube's bot checks trust.
 *  - Bun is yt-dlp's fastest EJS challenge solver (sig + n-throttle), so
 *    fewer "Signature solving failed / n challenge solving failed" drops
 *    that end in "Only images are available".
 *
 * Zero npm dependencies: only Bun builtins + a yt-dlp binary on PATH.
 *
 * Callers POST the video job to `/fetch` (or `/`); juiceback's BUN_FETCH_URL
 * is the base URL (e.g. http://127.0.0.1:7673) and it posts to the root.
 *
 * Env:
 *   HOST=127.0.0.1 PORT=7673
 *   BUN_FETCH_KEY=<bearer, required>
 *   YTDLP_BIN=yt-dlp
 *   YTDLP_POT_BASE_URL=http://127.0.0.1:4416   (empty = no PO provider)
 *   YTDLP_COOKIES=/path/to/cookies.txt          (empty = no cookies)
 *   YTDLP_TMPDIR=/var/lib/juicebox/bun-fetch-tmp
 *   YTDLP_JS_RUNTIME=bun,deno                  (comma-separated, PATH lookup;
 *                                               or runtime:/abs/path entries)
 *   FETCH_PROXY=                                 (empty = direct egress;
 *                                               set to a proxy URL on the primary)
 *   FETCH_TIMEOUT_SECS=1500
 *   SERVICE_MAX_BYTES=2147483648
 *   MAX_JOBS=2
 */

const HOST = process.env.HOST ?? "127.0.0.1";
const PORT = Number(process.env.PORT ?? "7673");
const API_KEY = process.env.BUN_FETCH_KEY ?? "";
const YTDLP_BIN = process.env.YTDLP_BIN ?? "yt-dlp";
const POT_BASE_URL = process.env.YTDLP_POT_BASE_URL ?? "";
const COOKIES = process.env.YTDLP_COOKIES ?? "";
const TMPDIR = process.env.YTDLP_TMPDIR ?? "/tmp/bun-fetch-tmp";
const JS_RUNTIME = process.env.YTDLP_JS_RUNTIME ?? "bun,deno";
const FETCH_PROXY = process.env.FETCH_PROXY ?? "";
const FETCH_TIMEOUT_SECS = Number(process.env.FETCH_TIMEOUT_SECS ?? "1500");
const SERVICE_MAX_BYTES = Number(process.env.SERVICE_MAX_BYTES ?? "2147483648");
const MAX_JOBS = Number(process.env.MAX_JOBS ?? "2");

if (!API_KEY) {
  console.error("FATAL: BUN_FETCH_KEY is required");
  process.exit(1);
}

let inFlight = 0;

function authed(req: Request): boolean {
  const got = req.headers.get("authorization") ?? "";
  if (got.length !== API_KEY.length + 7) return false;
  // Constant-time-ish compare to avoid trivial timing leaks.
  let diff = 0;
  const want = `Bearer ${API_KEY}`;
  for (let i = 0; i < want.length; i++) diff |= got.charCodeAt(i) ^ want.charCodeAt(i);
  return diff === 0;
}

/** SSRF guard: only public http(s) video-page URLs, no creds, no local hosts. */
function validateSourceUrl(raw: unknown): string {
  if (typeof raw !== "string" || !raw.trim() || raw.length > 2048) {
    throw new HttpError(400, "url is required");
  }
  let u: URL;
  try {
    u = new URL(raw.trim());
  } catch {
    throw new HttpError(400, "invalid url");
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") {
    throw new HttpError(400, "url scheme must be http or https");
  }
  if (u.username || u.password) {
    throw new HttpError(400, "url must not contain credentials");
  }
  const host = u.hostname.toLowerCase().replace(/\.$/, "");
  if (!host) throw new HttpError(400, "url must include a hostname");
  if (
    host === "localhost" ||
    host === "metadata.google.internal" ||
    host === "instance-data" ||
    host.endsWith(".localhost") ||
    host.endsWith(".local") ||
    host.endsWith(".internal") ||
    host.endsWith(".corp")
  ) {
    throw new HttpError(400, `refusing to fetch from local host: ${host}`);
  }
  if (isPrivateIpLiteral(host)) {
    throw new HttpError(400, `refusing to fetch from non-public address: ${host}`);
  }
  return u.toString();
}

function isPrivateIpLiteral(host: string): boolean {
  const h = host.replace(/^\[|\]$/g, "");
  const v4 = h.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (v4) {
    const o = v4.slice(1).map(Number);
    if (o.some((n) => n > 255)) return true; // not a real IP -> treat as hostile
    const [a, b] = o;
    return (
      a === 10 ||
      a === 127 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254) ||
      a === 0
    );
  }
  if (h.includes(":")) {
    const l = h.toLowerCase();
    return (
      l === "::1" ||
      l === "::" ||
      l.startsWith("fe80:") ||
      l.startsWith("fec0:") ||
      l.startsWith("fc00:") ||
      l.startsWith("fd00:") ||
      l.startsWith("ff00:") ||
      l.startsWith("::ffff:127.") ||
      l.startsWith("::ffff:10.") ||
      l.startsWith("::ffff:192.168.")
    );
  }
  return false;
}

class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

function sanitizeFilename(name: string, audioOnly: boolean): string {
  const fallback = audioOnly ? "audio.mp3" : "video.mp4";
  let base = name.split("/").pop()?.split("\\").pop() ?? fallback;
  base = [...base].filter((c) => {
    const code = c.codePointAt(0) ?? 0;
    return code >= 32 && code !== 127;
  }).join("").trim().slice(0, 180);
  if (!base || base === "." || base === "..") return fallback;
  if (!base.includes(".")) base += audioOnly ? ".mp3" : ".mp4";
  return base;
}

function contentTypeFor(filename: string, audioOnly: boolean): string {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "mp4": return "video/mp4";
    case "webm": return audioOnly ? "audio/webm" : "video/webm";
    case "mkv": return "video/x-matroska";
    case "mov": return "video/quicktime";
    case "m4a": return "audio/mp4";
    case "mp3": return "audio/mpeg";
    case "opus": return "audio/ogg";
    case "ogg": return "audio/ogg";
    case "wav": return "audio/wav";
    case "flv": return "video/x-flv";
    default: return audioOnly ? "audio/mpeg" : "video/mp4";
  }
}

async function fileExists(path: string): Promise<boolean> {
  const f = Bun.file(path);
  try {
    return (await f.stat()).isFile();
  } catch {
    return false;
  }
}

async function sweepStaleDirs(): Promise<void> {
  // Remove job dirs older than 2h (crashed-job leftovers).
  try {
    const { readdir, stat, rm } = await import("node:fs/promises");
    const { join } = await import("node:path");
    const entries = await readdir(TMPDIR).catch(() => [] as string[]);
    const cutoff = Date.now() - 2 * 3600 * 1000;
    for (const e of entries) {
      const p = join(TMPDIR, e);
      try {
        const st = await stat(p);
        if (st.isDirectory() && st.mtimeMs < cutoff) await rm(p, { recursive: true, force: true });
      } catch { /* ignore */ }
    }
  } catch { /* ignore */ }
}

async function handleFetch(body: any): Promise<Response> {
  // Validation throws HttpError; it must live inside the try so callers
  // get JSON errors instead of Bun's exception overlay page.
  try {
    const sourceUrl = validateSourceUrl(body?.url);
    const audioOnly = body?.audio_only === true;
    let maxHeight = 0;
    if (typeof body?.max_height === "number" && Number.isFinite(body.max_height)) {
      maxHeight = Math.max(0, Math.min(4320, Math.floor(body.max_height)));
    }

    if (inFlight >= MAX_JOBS) {
      return Response.json({ error: "busy, try again later" }, { status: 429 });
    }
    inFlight++;
    try {
      return await runJob(sourceUrl, audioOnly, maxHeight);
    } finally {
      inFlight--;
    }
  } catch (e) {
    if (e instanceof HttpError) {
      return Response.json({ error: e.message }, { status: e.status });
    }
    const msg = e instanceof Error ? e.message : String(e);
    console.error(`[${new Date().toISOString()}] fetch error: ${msg}`);
    return Response.json({ error: msg.slice(0, 500) }, { status: 500 });
  }
}

async function runJob(sourceUrl: string, audioOnly: boolean, maxHeight: number): Promise<Response> {
  const jobId = crypto.randomUUID();
  const jobDir = `${TMPDIR}/${jobId}`;
  try {
    await Bun.$`mkdir -p ${jobDir}`.quiet();

    const format = audioOnly
      ? "ba/b"
      : maxHeight > 0
        ? `bv*[height<=${maxHeight}]+ba/b`
        : "bv*+ba/b";

    const args: string[] = [
      ...(FETCH_PROXY ? ["--proxy", FETCH_PROXY] : []),
      ...(POT_BASE_URL ? ["--extractor-args", `youtubepot-bgutilhttp:base_url=${POT_BASE_URL}`] : []),
      "-f", format,
      ...(audioOnly
        ? ["--extract-audio", "--audio-format", "mp3"]
        : ["--merge-output-format", "mp4"]),
      "--no-playlist",
      "--no-progress",
      "--no-warnings",
      ...JS_RUNTIME.split(",").flatMap((r) => {
        const t = r.trim();
        return t ? ["--js-runtimes", t] : [];
      }),
      "--remote-components", "ejs:github",
      ...((COOKIES && await fileExists(COOKIES)) ? ["--cookies", COOKIES] : []),
      "-o", `${jobDir}/%(title).180B-%(id)s.%(ext)s`,
      "--print", "after_move:filepath",
      sourceUrl,
    ];

    console.log(`[${new Date().toISOString()}] job ${jobId}: fetching ${sourceUrl} (audio_only=${audioOnly})`);
    const proc = Bun.spawn([YTDLP_BIN, ...args], {
      stdout: "pipe",
      stderr: "pipe",
      timeout: FETCH_TIMEOUT_SECS * 1000,
    });
    const [stdout, stderr, exitCode] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);

    if (exitCode !== 0) {
      const tail = stderr.trim().split("\n").slice(-3).join(" | ") || "yt-dlp failed";
      console.warn(`[${new Date().toISOString()}] job ${jobId}: exit ${exitCode}: ${tail}`);
      // 410 Gone signals "no downloadable streams" (SABR walled) so the
      // caller can tell it apart from proxy/POT transport failures (502).
      const walled = /only images are available|requested format is not available|no video formats found|sabr/i.test(stderr);
      return Response.json({ error: tail.slice(0, 500) }, { status: walled ? 410 : 502 });
    }

    const candidates = stdout.split("\n").map((l) => l.trim()).filter(Boolean).reverse();
    let produced: string | undefined;
    for (const c of candidates) {
      if (await fileExists(c)) { produced = c; break; }
    }
    if (!produced) {
      // Fallback: largest media file in the job dir (template embeds the id).
      const { readdir } = await import("node:fs/promises");
      const { join } = await import("node:path");
      let best: { p: string; s: number } | undefined;
      for (const e of await readdir(jobDir).catch(() => [] as string[]) as string[]) {
        if (/\.(part|temp|ytdl|info\.json)$/i.test(e)) continue;
        if (!/\.(mp4|webm|mkv|m4a|mp3|opus|ogg|wav|mov|flv)$/i.test(e)) continue;
        const p = join(jobDir, e);
        const st = await Bun.file(p).stat().catch(() => undefined);
        if (st?.isFile() && (!best || st.size > best.s)) best = { p, s: st.size };
      }
      produced = best?.p;
    }
    if (!produced) {
      return Response.json({ error: "yt-dlp produced no file (SABR enforcement)" }, { status: 410 });
    }

    const f = Bun.file(produced);
    const st = await f.stat();
    if (st.size === 0) {
      return Response.json({ error: "yt-dlp produced an empty file" }, { status: 410 });
    }
    if (st.size > SERVICE_MAX_BYTES) {
      return Response.json({ error: "file exceeds service maximum" }, { status: 413 });
    }

    const rawName = produced.split("/").pop() ?? (audioOnly ? "audio.mp3" : "video.mp4");
    const filename = sanitizeFilename(rawName, audioOnly);
    console.log(`[${new Date().toISOString()}] job ${jobId}: done ${filename} (${st.size} bytes)`);

    // Deferred cleanup: the response streams lazily, so don't unlink now.
    const dir = jobDir;
    setTimeout(() => {
      Bun.$`rm -rf ${dir}`.quiet().catch(() => {});
    }, 5 * 60 * 1000);

    return new Response(f, {
      status: 200,
      headers: {
        "Content-Type": contentTypeFor(filename, audioOnly),
        "Content-Length": String(st.size),
        // Header values must be Latin1: strip everything outside printable
        // ASCII (unicode titles 500'd here before). The true name travels
        // ASCII-safely in X-Bun-Fetch-Filename (percent-encoded).
        "Content-Disposition":
          `attachment; filename="${filename.replace(/[^\x20-\x7E]/g, "_").replace(/["]/g, "_")}"`,
        "X-Bun-Fetch-Filename": encodeURIComponent(filename),
      },
    });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    console.error(`[${new Date().toISOString()}] job ${jobId}: error: ${msg}`);
    return Response.json({ error: msg.slice(0, 500) }, { status: 500 });
  }
}

async function potOk(): Promise<boolean> {
  if (!POT_BASE_URL) return false;
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 5000);
    const r = await fetch(`${POT_BASE_URL.replace(/\/$/, "")}/get_pot`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content_binding: "healthcheck" }),
      signal: ctrl.signal,
    }).catch(() => undefined);
    clearTimeout(t);
    return !!r?.ok;
  } catch {
    return false;
  }
}

await sweepStaleDirs();
setInterval(sweepStaleDirs, 30 * 60 * 1000);

Bun.serve({
  hostname: HOST,
  port: PORT,
  async fetch(req) {
    const u = new URL(req.url);
    if (req.method === "GET" && u.pathname === "/health") {
      let ytdlp = "unknown";
      try {
        const p = Bun.spawn([YTDLP_BIN, "--version"], { stdout: "pipe", stderr: "ignore" });
        ytdlp = (await new Response(p.stdout).text()).trim() || "unknown";
        await p.exited;
      } catch { /* ignore */ }
      return Response.json({
        ok: true,
        service: "juicebox-bun-fetch",
        bun: Bun.version,
        ytdlp,
        pot_ok: await potOk(),
        in_flight: inFlight,
      });
    }
    if (req.method === "POST" && (u.pathname === "/fetch" || u.pathname === "/")) {
      if (!authed(req)) {
        return Response.json({ error: "unauthorized" }, { status: 401 });
      }
      let body: any;
      try {
        body = await req.json();
      } catch {
        return Response.json({ error: "invalid json" }, { status: 400 });
      }
      return handleFetch(body);
    }
    return Response.json({ error: "not found" }, { status: 404 });
  },
});

console.log(`juicebox-bun-fetch listening on ${HOST}:${PORT} (bun ${Bun.version})`);
