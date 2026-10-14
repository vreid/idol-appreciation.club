import solid from "vite-plugin-solid";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite-plus";

// Same alias set as vite.config.ts: contract sources live at the repo level.
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
    // Unit tests live in src/lib, schema fixtures in the contract package,
    // component tests next to the components (DOM env via file docblock),
    // and the live-backend contract suite in test/contract (own task).
    include: [
      "src/**/*.test.ts",
      "src/**/*.test.tsx",
      "../contract/zod/*.test.ts",
    ],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov"],
      // Logic modules only; components are render-only glue and the
      // contract suite runs outside the coverage gate by design.
      include: ["src/lib/*.ts"],
      exclude: [
        "src/lib/**/*.test.ts",
        // DOM-only helpers, same rationale as worldMap/confetti/images:
        // Leaflet/DOM/window glue covered by e2e, not unitable logic.
        "src/lib/appActions.ts",
        "src/lib/worldMap.ts",
        "src/lib/ghost.ts",
        "src/lib/tileDom.ts",
        "src/lib/urlViewState.ts",
        "src/lib/confetti.ts",
        "src/lib/images.ts",
        // One-line canonical object guard consumed by excluded DOM glue.
        "src/lib/guards.ts",
      ],
      thresholds: { lines: 80 },
    },
  },
});
