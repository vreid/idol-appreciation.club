Truth check (origin/main, verified by reading the files — one receipt is wrong):

- `backend/src/lib.rs:33-36`:
  `PgPoolOptions::new().max_connections(cfg.database_pool_max).acquire_timeout(Duration::from_secs(10))`.
- `backend/src/config.rs:103`:
  `database_pool_max: env_or("DATABASE_POOL_MAX", "30")`.

So on current main the env knob IS genuinely honored (max_connections comes from
Config), and acquire_timeout is explicit (10s). The chart value 15 is live
config, not dead.

The stale part of the claim: main ALSO still defaults `DATABASE_POOL_MAX` to
"30" in config.rs when the env is unset — the chart always sets the env, so the
default is unreachable in prod, but it contradicts the 2x15 right-sizing. This
PR pins the default to "15" too and adds the regression test that proves the env
var moves the pool (parse-level: Config::from_env picks up DATABASE_POOL_MAX;
the pool wiring reads that exact field — a one-line grep test against
build_app's options would need a live DB, so the test pins the CONFIG surface,
which is the piece that drifted).

Receipts: #129's claim ("max_connections(cfg.database_pool_max) wired") is
CORRECT for current main; wipe's reading (hardcoded 20 / dead config) matches
only the pre-#129 history, not main.
