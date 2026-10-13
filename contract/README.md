# contract/ — the web↔backend wire contract

Single source of truth for every JSON payload that crosses the web↔backend
boundary: `zod/zod/schemas.ts` (schemas + inferred types, re-exported from
`zod/index.ts`).

The web parses **everything** it receives against these schemas the moment it
arrives (`web/src/lib/api.ts` for REST, `web/src/lib/sse.ts` for the event
stream). A payload that fails parsing never crashes the app: it is logged with
`console.warn` and surfaced as a toast. The contract integration suite
(`web/test/contract/`) boots the real backend and asserts that every endpoint
and live event parses clean, so schema drift fails in CI instead of in a
browser.

## Rules

- Backend PRs that change a response or SSE event shape **must** update the
  matching schema here in the same PR. `web:test` runs the schema unit tests;
  the contract suite runs them against a live backend.
- Unknown keys are stripped by zod, so additive fields are non-breaking.
  Removing or renaming a field is breaking for the web and must be flagged.
- IDs stay opaque strings: the wire format does not promise a UUID scheme.

## Why a repo-level package

The web↔backend boundary is exactly where staggered deploys bite: after a
backend rollout, old web bundles still run against new payloads (and vice versa
until the web image rolls). A typed, tested contract makes that window explicit
— additive changes are provably safe across it, breaking ones are caught before
merge instead of mid-rollout.

## Layout

- `zod/schemas.ts` — hand-written schemas, one section per endpoint/stream.
- `zod/index.ts` — public surface.
- `zod/schemas.test.ts` — valid/invalid fixtures per schema (runs under the web
  vitest gate, `task web:test`).
- `generated/` — reserved.

## Phase 2 (planned): schemars codegen

Post stack-wipe follow-up, not part of the Solid rewrite: derive schemas from
the Rust types with `schemars`, emit them into `contract/generated/`, and diff
generated vs. hand-written schemas in CI. From then on the Rust types are the
primary source and `zod/schemas.ts` becomes the reviewed projection — drift
fails the backend PR instead of the deploy.
