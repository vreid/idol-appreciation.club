# Frontend Runbook — idol-appreciation.club/web

SolidJS + Leaflet SPA on the Vite+ (`vp`) toolchain, styled with Chota on top of
the app's own design tokens, with every wire payload validated by the shared Zod
contract (`contract/zod`, repo level). Static SPA, no SSR: `vp build` emits
`web/dist` (index.html + hashed assets).

## Architecture

- **SolidJS 1.9** — `src/index.tsx` mounts `App.tsx`. State lives in two plain
  factories over Solid signals, not in components:
  - `src/lib/state.ts` — the `AppStore` (auth, place flow, toasts, admin). The
    full type is in `src/lib/stateTypes.ts`; every behavior is covered by
    `state.test.ts`. Components read the store through its getters — signal
    tracking works through the plain-property getters.
  - `src/lib/toasts.ts` — the toast hub: dedupe by (kind, message), hard cap of
    3 visible pills, auto-dismiss (4s info / 6s error).
- **Leaflet** — untouched. `src/lib/worldMap.ts` (controller), `world.ts`
  (coordinates), `tileRefresh.ts`/`tileDom.ts` (SSE-driven live tile refetch),
  `urlView.ts`/`urlViewState.ts` (`#z/x/y` links) and `ghost.ts` (rotated
  placement ghost + AABB outline) are framework-neutral modules carried over
  verbatim from the Svelte app; Solid only mounts the map container via a ref
  and feeds callbacks in `App.tsx`.
- **Components** (`src/lib/components/`): `TopBar` (brand + auth), with
  `AuthPopover`/`AdminPopover` split out, `PlacePanel` (long-edge + rotation
  sliders, client 4096px long-edge cap, live rotated ghost with AABB outline on
  the map), `Toasts`, `Modal`/`AboutModal` (native keydown listeners so Escape
  closes while other keys are swallowed).
- **SSE** — `src/lib/sse.ts`: reconnect with capped exponential backoff; the
  purple `pulse-rect` blip on every place/remove event is unchanged. Events that
  fail the contract schema are dropped with a console warning and an error toast
  ("Live update ignored — bad payload") — never a crash. Empty-canvas clicks
  stay silent (no toast).

## The wire contract (`contract/zod`, repo level)

`contract/zod/schemas.ts` is the single source of truth for every JSON payload
crossing the web↔backend boundary: SSE place/remove events (with a documented
TODO slot for the upcoming batch event), `/api/center`, `/api/placements/near`,
`/api/health`, plus auth/place/admin shapes. See `contract/README.md` for the
update rules and the phase-2 schemars-codegen plan (`contract/generated/`).

- Parse-on-receive: `src/lib/api.ts` runs every REST response through a schema
  (`parseOk`) and `src/lib/sse.ts` runs every event through `worldEventSchema`.
  Failure ⇒ `console.warn` + toast, never a crash.
- Because staggered deploys run old bundles against new payloads, the contract
  makes that window explicit: additive fields are stripped and safe; breaking
  ones are caught by the gates below before merge.
- **Unit fixtures**: `contract/zod/schemas.test.ts` (valid/invalid per schema)
  runs inside `task web:test`.
- **Integration suite**: `web/test/contract/contract.test.ts` boots a REAL
  backend (pinned libvips gate image + Postgres via
  `web/scripts/contract-harness.sh`, task `web:contract`, RUNNER=docker in CI)
  and asserts that every endpoint response and ≥3 live SSE events parse clean,
  plus a negative case. It runs as its own ci.yml job (`web-contract`) alongside
  web-test and is part of the `ci-ok` gate.

## Commands

Run inside `web/` (pnpm is the enforced package manager via `devEngines`; use
`vp` wrappers, not bare `npm run`). The Taskfile entrypoints are the same ones
CI runs:

| Task                 | Purpose                                             |
| -------------------- | --------------------------------------------------- |
| `task web:lint`      | `vp check`: oxfmt + oxlint + tsgo type check (gate) |
| `task web:test`      | `vp test --coverage` incl. contract fixtures (gate) |
| `task web:contract`  | Real-backend contract suite (gate, `web-contract`)  |
| `task web:e2e`       | Playwright e2e: @smoke, then the full suite         |
| `vp dev`             | Dev server, proxies `/api` to localhost:8080        |
| `vp build`           | Production build into `web/dist`                    |
| `vp preview`         | Serve `dist`, also proxies `/api`                   |
| `vp test`            | Vitest unit + component tests                       |
| `vp test --coverage` | Tests + v8 coverage, fails below 80% lines          |

CI runs `vp check` and `vp test --coverage` on every PR that touches `web/`
(path-filtered; no markers — broken code fails its own PR).

## Toolchain notes (vp)

- `vp` v1.1.0; bundled toolchain: vite 8.3.3 (rolldown/oxc), vitest 5.0.3,
  oxlint 1.87.0, oxfmt 0.72.0, oxlint-tsgolint 7.0.2003.
- **Type checking is tsgo (TypeScript 7 native preview)** via `vp check`
  (`lint.options.typeAware + typeCheck`). No `@typescript/native-preview` devDep
  — vite-plus bundles the binary.
- `tsconfig.app.json`: `strict`, `noUncheckedIndexedAccess`,
  `exactOptionalPropertyTypes`, `jsx: preserve` + `jsxImportSource: solid-js`.
  `@contract/*` maps to `../contract/zod/*`.
- vite/vitest resolve `@contract` with a `resolve.alias` to the contract
  sources; the contract package resolves its own `zod` from
  `contract/node_modules` (no alias, no copy). `vp build` therefore requires
  `pnpm install` in `contract/` too (the Taskfile web gates and web/Dockerfile
  both do it).

## Lint policy (aggressive)

Configured in `web/vite.config.ts` under `lint`:

- Categories: `correctness: error`, `suspicious: error`, `pedantic: warn`,
  `perf: warn`; `options.denyWarnings: true` — any warning fails `vp check`.
- Documented deviations, each with an inline comment in vite.config.ts:
  - `typescript/prefer-readonly-parameter-types: off` — DOM, Leaflet and Solid
    event/props params have no readonly types.
  - `max-lines-per-function: 120`, `max-lines: 400` (file cap); `createAppStore`
    has a 400-line override as one cohesive state machine.
  - `no-underscore-dangle` allows `_tiles` (Leaflet internal tile index).
  - tsx override `no-unassigned-vars: off` — Solid template refs assign `let x`
    variables from compiled JSX; oxlint cannot see that write.
- Formatting: oxfmt everywhere, `printWidth 80`, `semi`, double quotes,
  `proseWrap: always` (also enforced repo-wide over Markdown).

## Tests & coverage

`vp test` (Vitest). Node environment by default; component tests opt into
happy-dom via a `// @vitest-environment happy-dom` docblock.

- Logic modules (`world`, `tileRefresh`, `api`, `sse`, `cooldown`, `sizing`,
  `format`, `toasts`, `state`) unchanged in behavior; `state.ts` and `toasts.ts`
  are Solid-signal ports of the former runes stores with the same observable
  API, and their original tests pass untouched.
- Component tests (`@solidjs/testing-library` + happy-dom): `Toasts.test.tsx`
  (render/dismiss/dedupe/cap/auto-dismiss), `Modal.test.tsx`
  (Escape/backdrop/focus), `PlacePanel.test.tsx` (sliders, rotation clamp ±180°,
  choose/adjust/cancel flow).
- Contract fixtures: `contract/zod/schemas.test.ts`.
- Coverage gate: `vp test --coverage` fails below **80% lines** on the logic
  modules (`src/lib/*.ts`; components are render-only glue). DOM-only helpers
  excluded like `worldMap.ts`: `ghost.ts`, `tileDom.ts`, `urlViewState.ts`,
  `confetti.ts`, `images.ts`, `guards.ts`. Latest run: **92.87% lines**.

## Coordinate system (read before touching the map)

- Native world: `2^28 x 2^28` px (268,435,456), 256 px slippy tiles, zooms
  0..20, tiles at `/api/tiles/{z}/{x}/{y}.webp`. y grows DOWN (image convention,
  y=0 is the top row). Center "0/0" is `(134217728, 134217728)`.
- **The CRS.Simple y-flip pitfall**: Leaflet's `L.CRS.Simple` maps screen px as
  `x = lng * 256 * 2^z`, `y = -lat * 256 * 2^z` (lat grows UP). All app math
  stays in native world px and converts only at the Leaflet boundary
  (`src/lib/world.ts`):

  ```
  worldToLatLng(x, y) => { lat: -y / 2^28, lng: x / 2^28 }
  latLngToWorld(lat, lng) => { x: lng * 2^28, y: -lat * 2^28 }
  ```

  which renders native px one-to-one at zoom 20 (scale `2^(z-20)`).

- SSE invalidation: `place`/`remove` events carry `bbox: [x, y, w, h]` (the
  rotated AABB). Events queue in a ~500 ms coalescing window (`tileRefresh.ts`);
  each flush intersects the buffered bboxes against visible tile keys (Leaflet's
  internal `_tiles`, format `x:y:z`), restricted to the current viewport, and
  reassigns each matching `<img>.src` once, deduped. The refetch URL is
  cache-busted (`?r=`) so stale-while-revalidate cannot serve the old bitmap —
  the old pixels stay on screen until the new bytes decode: no blanking, no
  flicker.
- Popup query radius: `256 * 2^(16 - z)` clamped to `[256, 65536]`.

## URL view state (`#z/x/y`)

- `#16/0/0` = zoom 16 centred on the world centre; `#18/-1280/512` = 1280 px
  left of and 512 px below 0/0 (signed world-px offsets, y down).
  Encoder/decoder: `src/lib/urlView.ts`; wiring: `src/lib/urlViewState.ts`.
- Valid hash beats the default view on load; move/zoom writes back with a
  debounced `history.replaceState`; `hashchange`/`popstate` re-apply.

## Rotation semantics

- `Placement.rot` is degrees clockwise, 0 = upright; `x, y, w, h` stay the
  UNROTATED rect. Overlays size to `rotAABB(w, h, rot)` — the axis-aligned
  footprint, centred on the rect centre.
- Ghost rendering (place flow): CSS `translate(pointer) rotate(rot)` on the
  `<img>`; the dashed outline is a sibling div sized to the AABB at the current
  zoom. The long edge is clamped client-side to 16..4096 px.

## Selection & the anchored detail panel

- Clicking a placement selects it: the dashed AABB highlight stays on the image
  and `PlaceDetail` (a Solid-rendered `.card`) docks BESIDE the selection —
  right side preferred, flipping to the left near the right viewport edge,
  clamped to the top/bottom. It never covers the selected image (`PANEL_GAP`
  clearance; `src/lib/panelLayout.ts` is the pure layout math and the unit tests
  pin the invariant; the e2e asserts the DOM bboxes stay disjoint).
- Deselect: `Escape`, clicking empty canvas (still a silent no-op apart from the
  deselect), the panel's ✕, or a successful Remove. Panning and zooming
  reposition the panel live (`onViewportChange` → layout effect).
- This replaced the old Leaflet popup (`world-popup`), which rendered on top of
  the image it described.

### Dig: wheeling through the pile

Selecting a placement also queries `GET /api/dig?x=&y=` at the click point
(`DigEntry[]`/`DigStack` schemas, parsed at the boundary like every other
response). While a selection is held:

- Leaflet's scroll-wheel zoom is disabled; wheel events route to `store.flipDig`
  instead (`WorldMapController.setDigMode`). Wheel down peels deeper (older
  strata), wheel up returns toward the newest; the flip clamps at both ends.
- The panel's depth cue (`3 of 12`) tracks the visible stratum (`digIndex + 1`
  of `digTotal`; hidden for single-stratum selections), and the panel content +
  AABB highlight follow the flipped placement (`wireSelectionSync`).
- Deselecting (any path) restores normal wheel-zoom.
- The stack is a snapshot: a `place` mid-session doesn't refresh it, and an SSE
  `remove` of the visible entry deselects via the existing selection watcher.

### Tags on the selection

Each placement carries a normalized label set (`tags` on `Placement`/`DigEntry`;
optional in the schemas so older cached payloads parse). The panel renders the
labels as chips under the metadata line; the owner or an admin additionally gets
chip removal (✕) and an add-tag input (Enter or the button commits).

- Editing goes through `store.updateTags` → `PUT /api/placements/{id}/tags`
  (`tagsUpdateSchema` body, `tagsSchema` response). The backend is the authority
  on the rules (at most 8 tags of 1..=32 chars of `[a-z0-9_-]`, lowercased,
  trimmed, deduped); the client only fails fast on empty input.
- On success the authoritative list replaces the selection's `tags` and the
  matching dig stratum, so wheel-flips stay consistent. Failures surface as a
  toast (`describeApiError`); 401/403 open the login popover like every other
  authed call.

## API usage map

Cookie auth, same-origin. `src/lib/api.ts` is the only fetch site (fetch +
optional `base` injectable; the contract suite passes the booted backend's
origin). Endpoints: auth register/login/logout/me, place (multipart
x/y/size/rot), placements/near, dig (stack under a point, newest-first with
1-based depths; zod-parsed `digStackSchema`), center, events (SSE), reports,
placements/{id}/tags PUT (owner/admin label edit), admin placements delete,
admin accounts list/approve. UX rules live in `state.ts`: 429 → cooldown toast
with Retry-After; 401/403 → login popover; `/api/center` polls every 30 s and
refreshes early when an SSE bbox covers the center.

## Long-press to place (touch + right-click)

Long-pressing the map (or right-clicking on desktop) pins the pressed world
point and opens the existing upload flow against it:

- `src/lib/longPress.ts` owns the input: Leaflet `contextmenu` for desktop, a
  500 ms touch timer with 10 px move-slop for touch (iOS Safari never fires
  `contextmenu` for touches). A fired long-press eats the synthetic `click` that
  the following `touchend` would produce. The map root suppresses the native
  callout (`-webkit-touch-callout: none`) and text selection.
- The store holds the pin (`pendingSpot`); the place panel's primary becomes
  **📍 Place at pinned spot** (`confirmPinned`), which rides the exact
  `confirmAt` path as tap-to-choose — auth prompt, 429 cooldown toast and
  clamping are shared, not forked. Cancelling anywhere clears the pin.
- A held selection is released first: long-press never fights the wheel-dig
  session.
- e2e: `e2e/touch-place.spec.ts` — right-click pin, synthetic-touch long-press,
  and a drag-cancels case (no chooser opens).

## Design tokens (Chota)

`chota/dist/chota.min.css` is imported before `src/app.css` and supplies the
structural layer (base reset, forms, `.card`). `app.css` keeps the design tokens
as CSS variables and re-points Chota's own variables at them
(`--color-primary: var(--accent)`, `--bg-color: var(--bg)`, …):

- flat pink `--accent: #ff5fa2` — the single accent, no gradients anywhere (the
  old checkerboard conic-gradient preview became a flat surface);
- exactly ONE radius token, `--radius: 8px`, used by `.card` and controls;
- dark flat surfaces (`--bg`, `--bg-2`, `--bg-3`), hairline `--line`;
- one floating control cluster per corner: topbar, place panel (bottom right),
  footer chip (bottom left), toasts (top center), zoom (map).

App CSS retains component rules (`.btn`, `.topbar`, `.place-panel`, the anchored
`.place-detail` panel, toasts, keyframes); Chota owns base typography, inputs,
and the card box (restyled with the tokens above). Never introduce a second
radius or a gradient.

## Docker

`task image:web` builds from the REPO ROOT with `-f web/Dockerfile` (the bundle
imports `contract/zod`, so the image build copies web/ + contract/ and installs
the contract lockfile). Multi-stage: node:24-alpine (vp install/build) →
`caddy:2-alpine` serving `/app/dist`.

- `Caddyfile`: `encode zstd gzip`; SPA fallback; `/assets/*` immutable;
  `/index.html` no-cache. Port 8080.

## Bundle size (vp build)

Measured on the final tree (`gzip -9`, same method both sides):

| Asset             | Svelte 5 (before)             | Solid + Zod + Chota (after)   |
| ----------------- | ----------------------------- | ----------------------------- |
| JS (index-*.js)   | 218,524 B raw / 69,356 B gzip | 214,085 B raw / 65,162 B gzip |
| CSS (index-*.css) | 25,099 B raw / 8,781 B gzip   | 41,582 B raw / 11,627 B gzip  |
| index.html        | 0.7 kB raw / 0.4 kB gzip      | 0.7 kB raw / 0.4 kB gzip      |

JS gzip: **65.2 kB** against the 76 kB budget (69.4 kB before) — Solid's core
replaces the Svelte 5 runtime and `zod/mini` contributes only the validators
used. CSS grows by Chota's ~3 kB gzip. Leaflet is unchanged by this rewrite.
Chunk attribution (single JS chunk; share of bundled source bytes per
sourcemap): Leaflet ~51%, `zod/mini` ~30% raw (a few kB after min+gzip — only
used validators survive tree-shaking), app ~10%, solid-js ~10%.

## Verified

- `task web:lint` → `vp check` exit 0 (oxfmt + oxlint + tsgo, denyWarnings).
- `task web:test` → `vp test --coverage`: 153 tests passing (unit + component +
  contract fixtures), 92.87% lines (gate 80%).
- `task web:contract` → real-backend suite green (health, auth, place, near,
  center, ≥3 SSE events, negative case) against the pinned gate image.
- `vp build` produces `web/dist` with hashed assets (sizes above).
- Playwright: all existing specs green against the rewrite served by
  `vp preview` + the harness backend, plus the new `e2e/malformed-sse.spec.ts`
  (malformed injected event → toast + warning, app alive, map navigable).
