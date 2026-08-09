import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export const conduitApiTarget =
  process.env.VITE_CONDUIT_API_TARGET ?? "http://127.0.0.1:8080";
export const conduitVoxTarget =
  process.env.VITE_CONDUIT_VOX_TARGET ?? conduitApiTarget;
export const conduitMemoriaTarget =
  process.env.VITE_CONDUIT_MEMORIA_TARGET ?? conduitApiTarget;
const voxNeedsRewrite = conduitVoxTarget !== conduitApiTarget;
const memoriaNeedsRewrite = conduitMemoriaTarget !== conduitApiTarget;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/v1": {
        target: conduitApiTarget,
        changeOrigin: true,
        secure: false,
        ws: true,
      },
      "/linked-services": {
        target: conduitApiTarget,
        changeOrigin: true,
        secure: false,
        ws: true,
      },
      "/vox": {
        target: conduitVoxTarget,
        changeOrigin: true,
        secure: false,
        ws: true,
        rewrite: voxNeedsRewrite
          ? (path) => path.replace(/^\/vox/, "")
          : undefined,
      },
      "/memoria": {
        target: conduitMemoriaTarget,
        changeOrigin: true,
        secure: false,
        ws: true,
        rewrite: memoriaNeedsRewrite
          ? (path) => path.replace(/^\/memoria/, "")
          : undefined,
      },
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/setupTests.ts",
    /// e2e/ is driven by Playwright against a real browser, so vitest must
    /// not try to collect it.
    exclude: ["node_modules/**", "dist/**", "e2e/**"],
  },
});
