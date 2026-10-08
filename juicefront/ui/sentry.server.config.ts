import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: import.meta.env.SENTRY_DSN_UI || import.meta.env.SENTRY_DSN,
  release: "juicebox-epsilon@0.3",
  environment: import.meta.env.SENTRY_ENVIRONMENT || "production",
  tracesSampleRate: 0.1,
});
