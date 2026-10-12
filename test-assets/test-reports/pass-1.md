# Pass 1 — live site test report

- Site: https://idol-appreciation.club
- Date: 2026-10-12, ~21:25–21:29 UTC
- Tester: `tester` agent (report-only; no repo files touched)
- Total time spent: ~5 minutes wall clock
- prod footprint (no cleanup endpoints exist; all coords > 134M px
  from reserved center, well outside the 8192 px zone):
  - suite bots: 2 placements at (4096,4096) size 64
  - mine: 1 placement at (8731,8192) size 64; 1 rejected 429 attempt
  - accounts created: `e2e_bot_*` x2 (suite), `tester_p19153` (mine)

## 1. Playwright suite (`BASE_URL=... pnpm test:e2e`)

Result: 5 passed / 1 failed (1 test, retried once, same failure).

| Test | Result |
|---|---|
| @smoke root page loads | PASS |
| @smoke hashed assets immutable caching | PASS |
| @smoke SPA fallback 200 | PASS |
| @smoke tile endpoint reachable | PASS (backend serving, see §2) |
| @full auth: register + login bot | PASS |
| @full place flow (place/near/429/center) | FAIL |

FAIL evidence (`web/e2e/full.spec.ts:103`): the place, near-visibility,
and cooldown-429 assertions inside the test all passed; only the final
`/api/center` shape check failed:

```
expected current: anything(), all_time: any(Array)
received: {"all_time":[],"current":null}
```

Repro: `curl https://idol-appreciation.club/api/center` -> empty.
Root cause: canvas has zero seeded images. The 100-image seeding has
NOT landed; "current holder" is null, all_time list empty.

## 2. Page loads / deploy hygiene

- `GET /` (https): PASS — 200, `text/html`, 704 bytes, real app HTML
  (title "idol-appreciation.club — hold the 0/0", hashed asset
  `/assets/index-B1VACTFp.js`).
- Bogus SPA route `/bogus-route-probe-<rand>`: PASS — 200 text/html,
  same 704-byte index.html (fallback works).
- Hashed asset headers: PASS — `cache-control: public, max-age=31536000,
  immutable`, 200, 215242 bytes.

## 3. HTTP -> HTTPS redirect  [known-issue check]

PASS — the "redirect may be missing" issue is NOT present:

```
curl -sI http://idol-appreciation.club/
HTTP/1.1 308 Permanent Redirect
Location: https://idol-appreciation.club/
Server: Caddy
```

## 4. Tiles

`GET /api/tiles/{z}/{mid}/{mid}.webp`, mid = 2^(z-1) (z=0 -> 0/0):

| z | mid | status | content-type | bytes | ETag |
|---|---|---|---|---|---|
| 0 | 0 | 200 | image/webp | 394 | `"5"` |
| 4 | 8 | 200 | image/webp | 394 | `"0"` |
| 10 | 512 | 200 | image/webp | 394 | `"0"` |
| 16 | 32768 | 200 | image/webp | 394 | `"0"` |
| 20 | 524288 | 200 | image/webp | 394 | `"0"` |

- ETag consistency: PASS — two consecutive GETs (z=10) both `"0"`.
- 304 revalidation: PASS — `If-None-Match: "0"` -> 304, 0 bytes.
- Note: every non-center tile is the same 394-byte placeholder with
  ETag `"0"` (= 0 placements). Consistent with an unseeded canvas.

## 5. API shape

- `/api/health`: PASS — 200 `application/json`, `{"ok":true}`.
- `/api/center`: 200 but EMPTY — `{"all_time":[],"current":null}`
  (see FAIL in §1; seeding gap, endpoint itself healthy).
- `/api/placements/near?x=134217728&y=134217728&r=4096`: PASS —
  200, `[]`. Reserved center zone is correctly empty.

## 6. User journey (API)

Account: `tester_p19153`. Observed: **instant session, NO
pending-approval gate** (registration-approval gate not observable on
prod):

- `POST /api/auth/register` -> 201
  `{"id":"01a13203-...","is_admin":false,"username":"tester_p19153"}`
  — no `approved`/`pending` field, account usable immediately.
- `POST /api/auth/login` -> 200 `{"id":...}` + `set-cookie: session=...`.
- `GET /api/auth/me` -> 200 `{username, is_admin:false,
  cooldown_until:null}`.
- `POST /api/auth/logout` -> 204.
- `GET /api/auth/me` after logout -> 401 `{"error":"unauthorized"}`.

Product gap flag: repo contains `migrations/0002_account_approval.sql`
and the deployed router has no admin-approval route, so the gate flow
(register -> pending -> admin approves -> login) is UNTESTABLE on prod
— it is not deployed. If the gate was in tonight's merge train, it
hasn't shipped.

## 7. Placement flow

Fixture: `web/test-assets/e2e-tile.png` (76-byte PNG), size 64, at
far coords (8731, 8192) — ~134M px from center.

- Place #1: PASS — 201 `{id:"01a13203-...", x:8731, y:8192, w:64,
  h:64, seq:6, duplicate_count:5}`.
- Immediate overlapping #2: PASS — 429 `{"error":"cooldown"}` with
  `retry-after: 299`.

## 8. UI smoke (Playwright chromium, throwaway /tmp script)

- Desktop 1280x800, fresh load: PASS — header brand
  "idol-appreciation.club", badge button "👑 0/0 unclaimed", Leaflet
  map with 24 tile imgs, "Jump to 0/0" button, reticle anchored at
  viewport center (640,400).
- Full zoom-out (wheel to MIN_ZOOM=0; single z=0 tile rendered):
  PASS — 0/0 reticle sits at (640,400), dead center of viewport and
  centered on the lone tile. The known "reticle drifts top-left when
  zoomed out" bug does NOT reproduce on this deploy. Pixel evidence:
  /tmp/ui-shots/desktop-zoomed-out.png (also desktop-initial.png).
- Mobile (iPhone 13 emulation, 390x664): PASS — no horizontal scroll
  (`scrollWidth` 390 == viewport 390, doc and body), header + badge +
  map render, reticle at (195,332) ≈ centered. Shot:
  /tmp/ui-shots/mobile.png.
- Console noise (benign, noted for completeness): one 401 on
  `/api/auth/me` while logged out (expected logged-out probe); two
  `ERR_ABORTED` tile fetches (`/api/tiles/{4,8}/0/0.webp`) from
  normal Leaflet zoom churn.

## Summary

- FAIL: 1 — @full place flow, solely the `/api/center` seeding
  assertion. Canvas is empty; tonight's 100-image seed did not land.
- Known issues checked: http->https redirect present (308); reticle
  top-left bug not observed; approval gate not deployed (instant
  registration) — flagged above if it was expected.
- Everything else exercised: PASS.
