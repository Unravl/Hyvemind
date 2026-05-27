import React from "react";
import ReactDOM from "react-dom/client";
import * as Sentry from "@sentry/react";
import { defaultOptions } from "tauri-plugin-sentry-api";
import { App } from "./App";
import "./index.css";

// Sentry events go through the Rust SDK via the tauri-plugin-sentry IPC
// bridge. The DSN, scrubbing, and rate limiting are all owned by Rust;
// `defaultOptions` provides the renderer-side transport stub. Initialised
// before React renders so render-phase crashes are captured.
Sentry.init({
  ...defaultOptions,
  release: import.meta.env.VITE_APP_VERSION,
  environment: import.meta.env.DEV ? "dev" : "release",
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
