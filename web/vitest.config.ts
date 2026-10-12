import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite-plus";

export default defineConfig({
  plugins: [svelte()],
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      include: ["src/lib/**"],
      exclude: [
        "src/lib/**/*.test.ts",
        "src/lib/worldMap.ts",
        "src/lib/confetti.ts",
        "src/lib/images.ts",
      ],
      thresholds: { lines: 80 },
    },
  },
});
