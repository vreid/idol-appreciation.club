import { defineConfig, devices } from "@playwright/test";

// Config for the fully-mocked quarantine-flow specs (docs
// scaling-architecture.md "Upload flow"). Unlike the nightly prod
// config, this serves the LOCAL build (vp preview) and lets the specs
// route-mock every API call — the 202 flow cannot run against a
// pre-flip backend, and must never write to whatever BASE_URL the
// nightly targets.

export default defineConfig({
  testDir: ".",
  testMatch: /quarantine-place\.spec\.ts/,
  timeout: 30_000,
  fullyParallel: true,
  retries: 1,
  outputDir: "e2e-results-quarantine",
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4173",
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "pnpm exec vp preview --port 4173 --strictPort --host 127.0.0.1",
    cwd: "..",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
    timeout: 60_000,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
