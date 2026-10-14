# fix(ci): actually run the backend test suites in CI — COPY tests/ + docker for testcontainers

## The gap

`backend-checks` claimed "fmt+clippy+test" but the pinned gate image never
copied `tests/` — `cargo test` saw zero test targets — and testcontainers had
no docker socket anyway. The container suites (`integration`, `tile_queue`)
have **never run in CI**. Two latent reds sat invisible on main because of
exactly this:

1. `import_metrics_move_on_batch` (red on clean main, verified at f92b34e):
   a pure resume-skip (re-POST of a fully-committed batch_id) bumped
   `import_batches_total`. `finish_metrics` now counts only batches that
   committed rows (mid-batch failures with partial commits still count).
2. Import tests race each other's **absolute** import_* counter assertions on
   the process-global registry under `--test-threads 2` — serialized on
   `IMPORT_LOCK`.

Plus a clippy rider: `manual_range_contains` in integration.rs (invisible to
CI for the same reason).

## Wiring

- `backend/Dockerfile`: `COPY tests ./tests` — also makes
  `clippy --all-targets` lint the test code in CI for the first time.
- `Taskfile.yml backend:test`: `--network host` + docker socket mount so
  testcontainers can start sibling containers and reach their published ports
  on host loopback. `DOCKER_ARGS` overridable empty where no socket exists —
  suites then skip loudly (their documented no-docker behavior); podman local
  default unchanged.
- `ci.yml`: backend-checks timeout 25 → 40 min (dev-profile compile of the
  test binaries + the suites dominate; the suites take ~5 min together after
  compile).

Local verification on this branch: clippy `-D warnings` clean; lib 65/65;
`integration` 33/33 and `tile_queue` 6/6 with the gates actually running.

Sequenced after #134 (core render fix).
