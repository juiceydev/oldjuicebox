// Production server entry for the Juicebox² frontend.
//
// Wraps the @astrojs/node standalone server and adds a WebSocket proxy for
// /api/device/ws -> juiceback (used by juicebox-plus device pairing). Astro
// middleware is HTTP-only and cannot forward WS upgrades, so we hook the raw
// Node http.Server `upgrade` event and pipe bytes straight through (same as
// the dev-mode Vite proxy's `ws: true`).

process.env.ASTRO_NODE_AUTOSTART = "disabled";

// Prefer the CI-bundled, self-contained entry (single-file, no node_modules).
// Falls back to the raw Astro build for local source-tree runs.
const { startServer } = await import("./dist/server/entry.bundle.mjs").catch(
  () => import("./dist/server/entry.mjs")
);
const net = await import("node:net");

const { server } = startServer();
const httpServer = server.server;

const JUICEBACK_URL = process.env.JUICEBACK_URL || "http://127.0.0.1:6401";

function upstreamTarget() {
  const u = new URL(JUICEBACK_URL);
  return {
    host: u.hostname,
    port: u.port || (u.protocol === "https:" ? 443 : 80),
  };
}

httpServer.on("upgrade", (req, socket, head) => {
  const url = new URL(req.url, "http://localhost");
  if (url.pathname !== "/api/device/ws") {
    socket.destroy();
    return;
  }

  const target = upstreamTarget();
  const upstream = net.connect(target.port, target.host, () => {
    const requestLine = `${req.method} ${url.pathname}${url.search} HTTP/1.1`;
    const headers = Object.entries(req.headers)
      .map(([key, value]) => `${key}: ${value}`)
      .join("\r\n");
    upstream.write(`${requestLine}\r\n${headers}\r\n\r\n`);
    if (head.length > 0) upstream.write(head);
  });

  upstream.on("error", () => socket.destroy());
  socket.on("error", () => upstream.destroy());
  upstream.on("end", () => socket.end());
  upstream.on("close", () => socket.destroy());
  socket.on("close", () => upstream.destroy());
  socket.pipe(upstream);
  upstream.pipe(socket);
});
