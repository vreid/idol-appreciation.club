# Development Guide — Toolchain (mise + task)

All project tooling is provisioned by [mise](https://mise.jdx.dev); `task`
recipes are the local entrypoints for every CI job. If a gate is red on CI, run
the matching task locally first.

## Setup

```sh
mise install
task --list
```

`mise install` provisions everything pinned in `mise.toml`: rust, task,
betterleaks, jactionlint, opentofu, helm, and kubectl. Put mise's shims on PATH
once per shell so those tools (and the right node/pnpm) resolve:

```sh
eval "$(mise activate bash)"  # or zsh/fish equivalents
```

## Who owns which version

| Tool                                  | Pinned in                                   |
| ------------------------------------- | ------------------------------------------- |
| node, pnpm                            | `web/package.json` `devEngines`             |
| rust, task, betterleaks, jactionlint, | `mise.toml` `[tools]`                       |
| oxfmt, opentofu, helm, kubectl        | `mise.toml` `[tools]`                       |
| vite-plus (`vp`), vitest, Playwright  | `web/package.json` (pnpm-managed; never     |
|                                       | duplicated in `mise.toml`)                  |
| libvips                               | `backend/Dockerfile` container (never mise) |

mise reads node/pnpm from `devEngines` because
`settings.idiomatic_version_file_enable_tools = ["node", "pnpm"]` in
`mise.toml`. `devEngines` uses `onFail: download`, so plain pnpm/npm users get
the same versions automatically — exactly one pin per tool, no duplication.

### rustup retirement

mise is the only rust version source for this repo (1.99.0, matching the
`rust:trixie` gate image). Do not `rustup install` or `rustup default`
separately; if an old rustup toolchain shadows mise's, remove rustup's env
(`. "$HOME/.cargo/env"`) from your shell profile so mise's shims win.

## Tasks

| Task                  | ci.yml job     | Purpose                                      |
| --------------------- | -------------- | -------------------------------------------- |
| `task lint:md`        | md-lint        | oxfmt check over all Markdown                |
| `task lint:workflows` | workflows-lint | jactionlint over `.github/workflows`         |
| `task web:lint`       | web-lint       | `vp check` (oxfmt + oxlint + tsgo)           |
| `task web:test`       | web-test       | `vp test` (vitest, >=80% coverage gate)      |
| `task backend:lint`   | backend-lint   | `cargo fmt --check` + clippy `-D warnings`   |
| `task backend:test`   | backend-test   | `cargo test`                                 |
| `task leaks`          | (leaks gate)   | betterleaks git-history + filesystem scan    |
| `task helm:lint`      | helm-lint      | `helm lint` + `helm template`                |
| `task tofu:fmt`       | tofu           | `tofu fmt -check -recursive`                 |
| `task tofu:validate`  | tofu           | `tofu init -backend=false` + `tofu validate` |
| `task ci`             | (all of it)    | full local gate set                          |

`task ci` runs the lint/test/leaks tasks in parallel and the backend gates
serially at the end. `task <name>` reruns any single gate.

## Backend container parity

Backend gates run inside the pinned libvips builder image from
`backend/Dockerfile` (Debian trixie, libvips-dev 8.16.1-1+deb13u2, rust 1.99.0),
plus a cache-stable overlay that adds the rustfmt/clippy components
rust:trixie's minimal profile omits (`task backend:image` builds both), so local
results match CI and production. Do not run backend gates against the host
libvips (Ubuntu 24.04 ships 8.15.1; image behavior is libvips-version
sensitive). The runner defaults to podman; override where only docker exists:

```sh
RUNNER=docker task backend:test
```

## Secret scanning (hard gate)

`task leaks` scans git history and the working tree with
[betterleaks](https://github.com/betterleaks/betterleaks), configured in
`.betterleaks.toml` (built-in rules plus path exclusions for generated image
fixtures and dependency/build trees). Output is redacted; a finding fails the
gate. Known committed test fixtures (bot-account passwords, docs examples) are
allowlisted by fingerprint in `.betterleaksignore` — never by value; see the
regeneration commands in `.betterleaks.toml`. A genuinely leaked credential
requires rotation, not an allowlist entry; never weaken the scan to pass it.

## Markdown formatting

Docs are gated by oxfmt (80 columns, prose wrap). Run `oxfmt --write` (mise
provides the pinned 0.72.0 binary; do not route it through `npx` — npm's
devEngines enforcement breaks on this repo) or check with `task lint:md` before
committing prose changes.
