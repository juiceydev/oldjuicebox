// @ts-check
import { defineConfig } from "astro/config";
import solid from "@astrojs/solid-js";
import node from "@astrojs/node";

import sentry from "@sentry/astro";
import spotlightjs from "@spotlightjs/astro";
const integrations = [
  solid({ include: ["**/*.tsx"] }),
];
if (process.env.NODE_ENV !== "production") {
  integrations.push(spotlightjs());
}
const sentryDsn = process.env.SENTRY_DSN_UI || process.env.SENTRY_DSN;
if (sentryDsn) {
  integrations.push(
    sentry({
      dsn: sentryDsn,
      release: "juicebox-epsilon@0.3",
      environment: process.env.SENTRY_ENVIRONMENT || "production",
      tracesSampleRate: 0.1,
      enabled: { client: false, server: true },
    }),
  );
}

integrations.push((await import("@playform/inline")).default());

integrations.push((await import("@playform/compress")).default({
  CSS: { csso: { comments: false, restructure: false } },
  HTML: {
    "html-minifier-terser": {
      collapseWhitespace: true,
      removeComments: true,
      removeRedundantAttributes: true,
      removeScriptTypeAttributes: true,
      removeStyleLinkTypeAttributes: true,
      minifyCSS: true,
      minifyJS: true,
    },
  },
  JavaScript: { terser: { ecma: 2020, format: { comments: false } } },
  Image: { sharp: { webp: { quality: 80 }, jpeg: { quality: 80 }, png: { compressionLevel: 9 } } },
  SVG: { svgo: { multipass: true, plugins: ["preset-default"] } },
  JSON: true,
}));

/** @param {string} target */
const proxyTo = (target) => ({
  target,
  selfHandleResponse: true,
  ws: true,
  // @ts-ignore -- http-proxy types not installed
  configure(proxy) {
    proxy.on("error", (/** @type {Error} */ _err, /** @type {import('http').IncomingMessage} */ _req, /** @type {import('http').ServerResponse} */ res) => {
      if (res && "writeHead" in res && !res.headersSent) {
        res.writeHead(502, { "Content-Type": "application/json" });
      }
      if (res && "end" in res) {
        res.end(
          JSON.stringify({
            message: "Upload rejected or upstream unavailable",
          }),
        );
      }
    });
    proxy.on("proxyReq", (/** @type {import('http').ClientRequest} */ proxyReq, /** @type {import('http').IncomingMessage} */ req) => {
      const clientIp =
        req.headers["x-forwarded-for"] || req.socket?.remoteAddress;
      if (clientIp) {
        proxyReq.setHeader("x-forwarded-for", clientIp);
      }
    });
    proxy.on("proxyRes", (/** @type {import('http').IncomingMessage} */ proxyRes, /** @type {import('http').IncomingMessage} */ req, /** @type {import('http').ServerResponse} */ res) => {
      if (res.writeHead && !res.headersSent) {
        const headers = { ...proxyRes.headers };
        delete headers["transfer-encoding"];
        res.writeHead(proxyRes.statusCode || 200, headers);
      }
      proxyRes.pipe(res);
    });
  },
});

export default defineConfig({
  output: "server",
  // Astro's built-in origin check compares the Origin header against the
  // request URL. Behind a TLS-terminating proxy the standalone node adapter
  // reconstructs the URL with an http:// scheme (socket is plain HTTP), so
  // every browser Origin (https://...) is treated as cross-site and all
  // POSTs to the /api proxy would be rejected with a 403. The API endpoints
  // perform their own auth/IP checks, so the CSRF check is disabled here.
  security: { checkOrigin: false },
  integrations,
  adapter: node({ mode: "standalone" }),
  image: {
    service: { entrypoint: "astro/assets/services/sharp" },
  },
  build: {
    format: "file",
  },
  trailingSlash: "ignore",
  server: {
    host: "0.0.0.0",
    port: 6400,
  },
  i18n: {
    locales: ["en", "fr", "ru", "es", "ja"],
    defaultLocale: "en",
    fallback: {
      fr: "en",
      ru: "en",
      es: "en",
      ja: "en",
    },
    routing: {
      prefixDefaultLocale: false,
      fallbackType: "rewrite",
    },
  },
  vite: {
    envDir: "../..",
    build: {
      sourcemap: false,
      cssTarget: "chrome61",
    },
    server: {
      allowedHosts: ["box.juicey.dev"],
      proxy: {
        "^/upload": proxyTo("http://127.0.0.1:6401"),
        "^/file/": proxyTo("http://127.0.0.1:6401"),
        "^/api/": proxyTo("http://127.0.0.1:6401"),
        "^/f/": proxyTo("http://127.0.0.1:6402"),
      },
      // @ts-ignore -- Vite's preview server config
      preview: {
        allowedHosts: ["box.juicey.dev"],
      },
    },
  },
});
