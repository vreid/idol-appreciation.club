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
  },
  lint: {
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: { "vite-plus/prefer-vite-plus-imports": "error" },
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
  },
  plugins: lazyPlugins(() => [svelte()]),
});
