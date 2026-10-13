import solid from "vite-plugin-solid";
import { fileURLToPath } from "node:url";
import { defineConfig, lazyPlugins } from "vite-plus";

// The wire contract lives at the repo level (contract/zod) and is imported
// as source by both the app and the tests. It resolves its own zod from
// contract/node_modules — no alias needed here.
const contractDir = fileURLToPath(new URL("../contract/zod", import.meta.url));

export default defineConfig({
  resolve: {
    alias: {
      "@contract": contractDir,
    },
  },
  server: {
    proxy: {
      "/api": { target: "http://localhost:8080", changeOrigin: true },
    },
  },
  preview: {
    proxy: {
      "/api": { target: "http://localhost:8080", changeOrigin: true },
    },
  },
  fmt: {
    printWidth: 80,
    proseWrap: "always",
    semi: true,
    singleQuote: false,
  },
  lint: {
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: {
      "vite-plus/prefer-vite-plus-imports": "error",
      // Leaflet's rendered-tile index is the documented-but-internal
      // `_tiles`; the dangling underscore is the library's name, not ours.
      "no-underscore-dangle": ["error", { allow: ["_tiles"] }],
      // Factory functions build the map and the API client; 50 lines is
      // unworkable for DI-heavy glue, 120 is still a hard cap. Files cap at
      // 400: these modules are cohesive single-responsibility units.
      "max-lines-per-function": ["error", { max: 120 }],
      "max-lines": ["error", { max: 400 }],
      // DOM/Leaflet/Solid event params have no readonly counterparts.
      "typescript/prefer-readonly-parameter-types": "off",
    },
    categories: {
      correctness: "error",
      suspicious: "error",
      pedantic: "warn",
      perf: "warn",
      style: "off",
      restriction: "off",
      nursery: "off",
    },
    options: { typeAware: true, typeCheck: true, denyWarnings: true },
    overrides: [
      {
        // Solid template refs assign `let x` variables from compiled JSX
        // (`ref={x}`); oxlint cannot see the generated assignment.
        files: ["src/**/*.tsx"],
        rules: { "no-unassigned-vars": "off" },
      },
      {
        // createAppStore is one cohesive reactive state machine; splitting
        // the getters/actions apart would obscure the shared state.
        files: ["src/lib/state.ts"],
        rules: { "max-lines-per-function": ["error", { max: 400 }] },
      },
    ],
  },
  plugins: lazyPlugins(() => [solid()]),
});
