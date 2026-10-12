import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig, lazyPlugins } from "vite-plus";

export default defineConfig({
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
    svelte: true,
    ignorePatterns: ["**/*.md"],
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
      // DOM/Leaflet/Svelte event params have no readonly counterparts.
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
        // createAppStore is one cohesive reactive state machine; splitting
        // the getters/actions apart would obscure the shared state.
        files: ["src/lib/state.svelte.ts"],
        rules: { "max-lines-per-function": ["error", { max: 400 }] },
      },
    ],
  },
  plugins: lazyPlugins(() => [svelte()]),
});
