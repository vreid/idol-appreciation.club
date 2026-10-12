# idol-appreciation.club — web

Collaborative infinite idol-image canvas (Svelte 5 + Vite+ SPA). Hold the world
center **0/0**.

## Commands (run inside `web/`)

| Command                      | What it does                                   |
| ---------------------------- | ---------------------------------------------- |
| `vp dev`                     | Dev server, proxies `/api` to `localhost:8080` |
| `vp build`                   | Production build → `dist/`                     |
| `vp check`                   | oxfmt + oxlint + tsgo type checks (must pass)  |
| `npm run check`              | svelte-check + tsc (extra type net)            |
| `vp test`                    | Vitest unit tests                              |
| `vp test --coverage`         | Tests + v8 coverage, fails below 80% lines     |
| `docker build -t idol-web .` | Image (Caddy serves `dist/`, port 8080)        |

Runbook, coordinate-system notes, and API mapping live in
[`docs/frontend.md`](../docs/frontend.md) at the repo root.
