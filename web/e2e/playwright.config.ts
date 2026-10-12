import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.BASE_URL ?? "https://idol-appreciation.club";

export default defineConfig({
  testDir: ".",
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
