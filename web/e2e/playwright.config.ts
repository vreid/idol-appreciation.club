import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.BASE_URL ?? "https://idol-appreciation.club";

export default defineConfig({
  testDir: ".",
  // The quarantine-flow specs route-mock every API call and only make
  // sense against the local build (e2e/playwright.quarantine.config.ts);
  // against prod the deployed bundle may predate the client feature and
  // the mocks would fight the real backend.
  testIgnore: /quarantine-place\.spec\.ts/u,
  timeout: 30_000,
  fullyParallel: true,
  retries: 1,
  outputDir: "e2e-results",
  reporter: [["list"], ["html", { open: "never", outputFolder: "e2e-report" }]],
  use: {
    baseURL,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
