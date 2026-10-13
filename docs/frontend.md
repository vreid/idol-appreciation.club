# Frontend Runbook — idol-appreciation.club/web

Svelte 5 (runes) + Leaflet SPA on the Vite+ (`vp`) toolchain. Static SPA, no
SSR: `vp build` emits `web/dist` (index.html + hashed assets).

## Commands

Run inside `web/` (pnpm is the enforced package manager via `devEngines`; use
`vp` wrappers, not bare `npm run`):

| Command              | Purpose                                         |
| -------------------- | ----------------------------------------------- |
| `vp dev`             | Dev server, proxies `/api` to localhost:8080    |
| `vp build`           | Production build into `web/dist`                |
| `vp preview`         | Serve `dist`, also proxies `/api`               |
| `vp check`           | oxfmt + oxlint + tsgo type check (gate, exit 0) |
| `vpr check`          | npm script: svelte-check + tsc (second net)     |
| `vp test`            | Vitest unit tests                               |
| `vp test --coverage` | Tests + v8 coverage, fails below 80% lines      |
| `vp fmt`             | Format code and Markdown (oxfmt)                |

CI runs `vp check` and `vp test --coverage` on every PR that touches `web/`
(path-filtered; no markers — broken code fails its own PR).

## Toolchain notes (vp)

- Installed via `curl -fsSL https://vite.plus | bash`; `vp` v1.1.0. Project
  scaffolded with `vp create vite -- web --template svelte-ts` (plain Vite
  Svelte SPA; the `svelte`/`sv create` template yields SvelteKit + adapter-auto,
  which does not match the "static SPA in web/dist" requirement, so the closest
  supported path was used — `vp install` / `vp build` remain the canonical
  commands).
- Bundled toolchain (see `vp toolchain`): vite 8.3.3 (rolldown/oxc), vitest
  5.0.3, oxlint 1.87.0, oxfmt 0.72.0, **oxlint-tsgolint 7.0.2003**.
- **Type checking uses tsgo (TypeScript 7 native preview)**: `vp check` with
  `lint.options.typeAware + typeCheck` routes type checks through tsgolint, the
  oxc-project's TypeScript-Go power checker. There is no
  `@typescript/native-preview` devDep because vite-plus bundles the binary.
  tsgolint version: **7.0.2003**.
- A second, independent type net runs svelte-check + tsc (`vpr check`); both are
  clean.
- tsconfig.app.json: `strict`, `noUncheckedIndexedAccess`,
  `exactOptionalPropertyTypes` (on top of `@tsconfig/svelte`). No `any`, no
  non-null `!` bypasses; library-internal access goes through runtime-validated
  narrowing (see `src/lib/tileDom.ts`).

## Lint policy (aggressive)

Configured in `web/vite.config.ts` under `lint`:

- Categories: `correctness: error`, `suspicious: error`, `pedantic: warn`,
  `perf: warn`; `options.denyWarnings: true` — any warning fails `vp check`.
- Documented deviations, each with an inline comment in vite.config.ts:
  - `typescript/prefer-readonly-parameter-types: off` — DOM, Leaflet and Svelte
    event/props params have no readonly types; everything else stays strict.
  - `max-lines-per-function: 120` (50 is unworkable for DI-heavy factories),
    `max-lines: 400` per file.
  - `no-underscore-dangle` allows `_tiles` — Leaflet's internal tile index,
    accessed through validated widening (`worldMap.ts`).
  - One inline `eslint-disable-next-line unicorn/no-array-callback-reference` on
    `L.map(...)` — Leaflet's map factory trips the Array#map heuristic.
- oxlint has no `unsafe` category (that is Clippy vocabulary); the strictest
  supported groups are enabled as described above.
- Formatting: oxfmt everywhere, `printWidth 80`, `semi`, double quotes,
  `proseWrap: always` — including Markdown. Verified empirically against
  Prettier on prose-heavy samples: outputs are byte-identical, so the repo-wide
  Prettier markdown gate and oxfmt agree; Prettier is being retired as the
  markdown authority (infra removes the root gate).

## Tests & coverage

`vp test` (Vitest, node env). Test files: `src/lib/*.test.ts`.

- Coordinate helpers (`world.ts`): world↔latLng round-trips, CRS.Simple y-flip,
  scale at z=0/16/20, tile bboxes at z=0/16/20, tile URL building,
  `parseTileKey` (Leaflet's `x:y:z`), rotated-AABB → tile selection, viewport
  bbox conversion, query-radius clamps, world clamps.
- Tile refresh (`tileRefresh.ts`): fake timers — ~500 ms coalescing window,
  tile-coord dedupe across overlapping events, viewport filtering, manual flush,
  dispose semantics.
- API client (`api.ts`): mock-fetch tests for register/login/logout, me (200 +
  401→null), place() multipart construction (x/y/size/rot FormData), 429 with
  Retry-After, 400 message, near/center parsing (incl. null current, rot),
  report/DELETE admin removal, network-failure → status 0.
- SSE (`sse.ts`): fake EventSource — event parsing, garbage dropping,
  bbox→center check, reconnect scheduling with doubling backoff capped at 30 s,
  backoff reset on open, browser-retry passthrough (non-CLOSED errors), stop()
  semantics.
- Cooldown (`cooldown.ts`): remaining-ms math, countdown formatting, live
  Countdown ticking/expiry with fake timers.
- Sizing (`sizing.ts`): long-edge clamp 16..4096, aspect math (ghost rect), 10
  MB upload check.
- Rotation (`rotAABB` in `world.ts`): cardinal identity, 90° swap, 45° √2
  growth, area/edge invariants.
- Format (`format.ts`): time-ago buckets, duration formatting.
- Store (`state.svelte.ts`): auth flows, toasts lifecycle, full place flow incl.
  429 cooldown toast, auth prompts, report/remove rules.

Coverage gate: `vp test --coverage` fails below **80% lines** on the logic
modules (`src/lib/*.ts`; `.svelte` components are render-only glue). DOM-only
helpers excluded: `worldMap.ts`, `confetti.ts`, `images.ts`. Latest run: **93.5%
lines**.

## Coordinate system (read before touching the map)

- Native world: `2^28 x 2^28` px (268,435,456), 256 px slippy tiles, zooms
  0..20, tiles at `/api/tiles/{z}/{x}/{y}.webp`. y grows DOWN (image convention,
  y=0 is the top row). Center "0/0" is `(134217728, 134217728)`.
- **The CRS.Simple y-flip pitfall**: Leaflet's `L.CRS.Simple` maps screen px as
  `x = lng * 256 * 2^z`, `y = -lat * 256 * 2^z` (lat grows UP). We keep all app
  math in native world px and convert only at the Leaflet boundary
  (`src/lib/world.ts`):

  ```
  worldToLatLng(x, y) => { lat: -y / 2^28, lng: x / 2^28 }
  latLngToWorld(lat, lng) => { x: lng * 2^28, y: -lat * 2^28 }
  ```

  which renders native px one-to-one at zoom 20 (scale `2^(z-20)`). Map bounds:
  latLng `[-1, 0]..[0, 1]` with viscosity 1.0; tile layer bounds match, so no
  wrap and no out-of-world tiles.

- SSE invalidation: `place`/`remove` events carry `bbox: [x, y, w, h]` (the
  rotated AABB). Events queue in a ~500 ms coalescing window (`tileRefresh.ts`);
  each flush intersects the buffered bboxes against visible tile keys (Leaflet's
  internal `_tiles`, format `x:y:z`) restricted to the current viewport and
  reassigns each matching `<img>.src` once, deduped. The refetch URL is
  cache-busted (`?r=`): same-URL reloads would be answered from the HTTP cache's
  stale-while-revalidate grace and lag a refresh behind, while a unique query
  gets 200-with-current-bytes immediately — the old bitmap stays on screen until
  the new bytes decode, so tiles never blank or flicker. `invalidateBBox`
  remains the immediate single-shot path for optimistic local changes (e.g.
  admin removal).
- Popup query radius: `256 * 2^(16 - z)` clamped to `[256, 65536]`.

## URL view state (`#z/x/y`)

- The map view is linkable: `#16/0/0` = zoom 16 centred on the world centre;
  `#18/-1280/512` = 1280 px left of and 512 px below 0/0 (signed world-px
  offsets, y down). Encoder/decoder: `src/lib/urlView.ts`.
- On load a valid hash beats the default centred view; malformed/missing falls
  back to it. Zoom is clamped to 0..20 on both parse and encode.
- After move/zoom settles (250 ms debounce) the view is written back with
  `history.replaceState` — integer px, no history spam.
- `hashchange`/`popstate` (manual hash edits, Back/Forward) re-apply the hash to
  the map; the write-back skips the view it just applied. Wiring lives in
  `src/lib/urlViewState.ts` (`wireUrlViewState(map)`), invoked from
  `createWorldMap`.

## Rotation semantics

- `Placement.rot` is degrees clockwise, 0 = upright; `x, y, w, h` stay the
  UNROTATED rect (top-left + long-edge dims). The rect rotates about its own
  centre, so the centre is always `(x + w/2, y + h/2)`.
- Overlays that need a screen box (ghost footprint outline, placement highlight)
  use the axis-aligned footprint `rotAABB(w, h, rot)` — centred on the rect
  centre, sized `w·|cos|+h·|sin| × w·|sin|+h·|cos|`. Hit-testing is server-side
  (`placements/near` bbox), so the client never re-implements it.
- Ghost rendering: CSS `translate(pointer) rotate(rot)` on the `<img>`
  (transform-origin centre = rect centre); the outline is a sibling div sized to
  the AABB at the current zoom.

## API usage map

Cookie auth, same-origin. `src/lib/api.ts` is the only fetch site (fetch
injectable for tests). Endpoints per contract: auth register/login/logout/me,
place (multipart x/y/size/rot), placements/near, center, events (SSE), reports,
admin placements delete. The store (`state.svelte.ts`) owns UX rules: 429 →
cooldown toast with Retry-After; 401/403 → login popover; center chip polls
`/api/center` every 30 s and refreshes early when an SSE bbox covers the center.

Dev/demo mocking: none — `vp dev` proxies `/api` to `http://localhost:8080`
(`server.proxy` + `preview.proxy` in vite.config.ts). Point the backend there.

## Docker

`docker build -t idol-web . && docker run -p 8080:8080 idol-web` (from `web/`;
context is `web/`).

- Multi-stage: `node:24-alpine` installs vp, `vp install --frozen-lockfile`,
  `vp build`; `caddy:2-alpine` serves `/app/dist` from `/usr/share/caddy` as uid
  1000 (writable `/data`, `/config`).
- `Caddyfile`: `encode zstd gzip`; `try_files {path} /index.html` SPA fallback;
  `/assets/*` → `Cache-Control: public, max-age=31536000, immutable` (hashed
  filenames); `/index.html` → `no-cache`. Port 8080.

## Bundle size (vp build, this tree)

| Asset             | Raw      | Gzip    |
| ----------------- | -------- | ------- |
| JS (index-*.js)   | 216.7 kB | 67.6 kB |
| CSS (index-*.css) | 25.1 kB  | 8.8 kB  |
| index.html        | 0.7 kB   | 0.4 kB  |

JS gzipped is 68 kB against the 150 kB target; the only runtime deps are Svelte
5 and Leaflet 1.9.4.

## Verified

- `vp check` exit 0 (fmt + oxlint + tsgo type check); `vpr check`: svelte-check
  0 errors/0 warnings + tsc clean.
- `vp test`: 104 passing; `vp test --coverage`: 85.7% lines (gate: 80%).
- `vp build` produces `web/dist` with hashed assets (sizes above).
- `vp dev` smoke: page compiles and boots with zero console errors; browser
  check confirmed topbar, 0/0 chip, center reticle, modals, auth popover,
  Escape-to-close. Rotation: place flow DOM-verified against a static replica
  (ghost `rotate(45deg)` at the pointer, dashed AABB outline √2-scaled at 45°,
  Rotation slider label live) and pixel-verified via magnified screenshot.
  `/api` 502s with no backend running, surfaced as toasts — expected without the
  backend.
- Docker: image builds (multi-stage, 93.6 MB); HTTP-verified — `/` and
  `/index.html` send `no-cache`, `/assets/*` send
  `public, max-age=31536000, immutable` + gzip, unknown routes fall back to
  index.html with 200.
