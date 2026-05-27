import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { sentryVitePlugin } from "@sentry/vite-plugin";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const pkg = JSON.parse(
  readFileSync(
    resolve(dirname(fileURLToPath(import.meta.url)), "package.json"),
    "utf8",
  ),
);

// Sentry sourcemap upload runs only in CI/release builds where the auth
// token is provided. Dev builds (`vite dev` / `tauri dev`) skip it even
// when the token is present so the plugin doesn't sit in memory or run
// uploads during normal development.
const sentryAuthToken = process.env.SENTRY_AUTH_TOKEN;
const isProdBuild = process.env.NODE_ENV === "production";

export default defineConfig({
  define: {
    "import.meta.env.VITE_APP_VERSION": JSON.stringify(pkg.version),
  },
  plugins: [
    react(),
    ...(sentryAuthToken && isProdBuild
      ? [
          sentryVitePlugin({
            org: process.env.SENTRY_ORG,
            project: process.env.SENTRY_PROJECT,
            authToken: sentryAuthToken,
            release: { name: pkg.version },
          }),
        ]
      : []),
  ],
  clearScreen: false,
  server: {
    // Off Vite's universal default 5173 — workers commonly scaffold Vite
    // apps that also default to 5173. With strictPort, Hyvemind fails loud
    // instead of silently sharing the port and getting its webview hijacked.
    port: 1430,
    strictPort: true,
    hmr: false,
    watch: { ignored: ["**/*"] },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
});
