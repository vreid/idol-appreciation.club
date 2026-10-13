import solid from "vite-plugin-solid";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite-plus";

/**
 * Contract integration suite: runs ONLY via `task web:contract` (the task
 * boots a real backend + Postgres and exports BASE_URL); kept out of the
 * default `vp test` run so unit gates never depend on containers.
 */
const contractDir = fileURLToPath(new URL("../contract/zod", import.meta.url));

export default defineConfig({
  resolve: {
    alias: {
      "@contract": contractDir,
    },
  },
  plugins: [solid()],
  test: {
    environment: "node",
    include: ["test/contract/**/*.test.ts"],
    testTimeout: 30_000,
    hookTimeout: 60_000,
    maxConcurrency: 1,
    fileParallelism: false,
  },
});
