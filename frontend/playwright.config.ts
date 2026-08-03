import { defineConfig, devices } from "@playwright/test";

/// Screenshot runs serve the production build, so what is captured is what
/// ships rather than what the dev server rewrites.
export default defineConfig({
  testDir: "./e2e",
  outputDir: "../output/playwright/results",
  fullyParallel: true,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4319",
    trace: "off",
  },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"] } },
    { name: "mobile", use: { ...devices["Pixel 7"] } },
  ],
  webServer: {
    command: "npm run build && npx vite preview --port 4319 --host 127.0.0.1",
    url: "http://127.0.0.1:4319",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
