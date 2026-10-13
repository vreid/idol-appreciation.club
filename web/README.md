# idol-appreciation.club — web

Collaborative infinite idol-image canvas (SolidJS + Vite+ SPA). Hold the world
center **0/0**.

## Commands (run inside `web/`)

| Task                            | What it does                                    |
| ------------------------------- | ----------------------------------------------- |
| `vp dev`                        | Dev server, proxies `/api` to `localhost:8080`  |
| `vp build`                      | Production build → `dist/`                      |
| `vp check`                      | oxfmt + oxlint + tsgo type checks (must pass)   |
| `vp test`                       | Vitest unit + component tests                   |
| `vp test --coverage`            | Tests + v8 coverage, fails below 80% lines      |
| `task web:contract` (repo root) | Contract suite vs. a real booted backend        |
| `task image:web` (repo root)    | Image (repo-root context; Caddy serves `dist/`) |

Runbook, coordinate-system notes, design tokens, and the contract layout live in
[`docs/frontend.md`](../docs/frontend.md) at the repo root. The wire contract
itself is `contract/zod/` (repo level) — backend PRs that change a response or
event shape update it in the same PR.
