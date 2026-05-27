import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    globals: true,
    exclude: [
      "**/node_modules/**",
      "**/dist/**",
      "src-tauri/binaries/**",
      // Pi extensions get bundled into target/ during `tauri build`; their
      // own .test.mjs files belong to the Pi project, not Hyvemind.
      "src-tauri/target/**",
    ],
  },
});
