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

GitHub Actions workflows run these same tasks via `jdx/mise-action` — CI and
local share one entrypoint set.

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

CI gate entrypoints (one per ci.yml job):

| Task                  | ci.yml job     | Purpose                                      |
| --------------------- | -------------- | -------------------------------------------- |
| `task lint:md`        | md-lint        | oxfmt check over all Markdown                |
| `task lint:workflows` | workflows-lint | jactionlint over `.github/workflows`         |
| `task lint:pr-title`  | lint:pr-title  | Conventional Commits check on the PR title   |
| `task release`        | release.yml    | semantic-release: tag + GitHub Release       |
| `task web:lint`       | web-lint       | `vp check` (oxfmt + oxlint + tsgo)           |
| `task web:test`       | web-test       | `vp test` (vitest, >=80% coverage gate)      |
| `task backend:lint`   | backend-lint   | `cargo fmt --check` + clippy `-D warnings`   |
| `task backend:test`   | backend-test   | `cargo test`                                 |
| `task leaks`          | leaks          | betterleaks git-history + filesystem scan    |
| `task helm:lint`      | helm-lint      | `helm lint` + `helm template`                |
| `task tofu:fmt`       | tofu           | `tofu fmt -check -recursive`                 |
| `task tofu:validate`  | tofu           | `tofu init -backend=false` + `tofu validate` |
| `task ci`             | (all of it)    | full local gate set                          |

`task ci` runs the lint/test/leaks tasks in parallel and the backend gates
serially at the end. `task <name>` reruns any single gate.

Workflow operation entrypoints (deploy.yml / infra-\* / e2e.yml; CI-only — they
need secrets, registry credentials, or a live cluster):

| Task                 | Workflow job          | Purpose                               |
| -------------------- | --------------------- | ------------------------------------- |
| `task lint:fast`     | deploy lint           | md + workflows + helm + tofu fmt      |
| `task backend:ci`    | deploy backend-checks | backend lint then test, serial        |
| `task web:ci`        | deploy web-checks     | web lint then test                    |
| `task tofu:ci`       | ci.yml tofu           | fmt + validate                        |
| `task tofu:plan`     | ci.yml tofu-plan      | init (real backend) + plan            |
| `task tofu:apply`    | infra-apply           | validate + apply (one-retry converge) |
| `task tofu:destroy`  | infra-destroy         | destroy + deferred LB cleanup         |
| `task tofu:recreate` | infra-recreate        | LB cleanup + destroy + apply          |
| `task image:backend` | deploy backend-build  | buildx build + push, prints digest    |
| `task image:web`     | deploy web-build      | buildx build + push, prints digest    |
| `task deploy:k8s`    | deploy deploy         | kubeconfig, helm, app-env secret, DNS |
| `task web:e2e`       | e2e.yml               | chromium install + smoke + full run   |

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

`task leaks` scans the git history of the checked-out ref and the working tree
with [betterleaks](https://github.com/betterleaks/betterleaks), configured in
`.betterleaks.toml` (built-in rules plus path exclusions for generated image
fixtures and dependency/build trees). Output is redacted; a finding fails the
gate. Known committed test fixtures (bot-account passwords, docs examples) are
allowlisted by fingerprint in `.betterleaksignore` — never by value; see the
regeneration commands in `.betterleaks.toml`. A genuinely leaked credential
requires rotation, not an allowlist entry; never weaken the scan to pass it.

The history scan is limited to commits reachable from `HEAD` (`--log-opts=HEAD`;
in CI that is the PR merge commit, or the pushed `main` commit). Other branches
are scanned by their own PR and push runs, so a finding on one branch no longer
fails every open PR.

## Markdown formatting

Docs are gated by oxfmt (80 columns, prose wrap). Run `oxfmt --write` (mise
provides the pinned 0.72.0 binary; do not route it through `npx` — npm's
devEngines enforcement breaks on this repo) or check with `task lint:md` before
committing prose changes.
