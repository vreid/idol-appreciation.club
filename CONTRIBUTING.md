# Contributing

## Commit & PR-title convention (strictly enforced)

Squash-merged PRs take their commit message from the PR title, so the PR title
IS the commit. Titles must be strict
[Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):

```text
<type>(<scope>)?!: <subject>
```

- `type` (required): `feat`, `fix`, `perf`, `refactor`, `docs`, `chore`, `test`,
  `ci`, `build`, or `style`.
- `scope` (optional): `backend`, `web`, `tofu`, `chart`, `ci`, `deps`,
  `release`, `api`, or `seed`.
- `!` before the colon marks a breaking change (major release).
- `subject`: non-empty, no trailing period, no trailing whitespace. Imperative
  mood ("add x", not "added x").

The legacy prefixes previously used as types (`backend:`, `tofu:`, `web:`,
`seed:`, `api:`) are now scopes: `backend: fix cooldown` is wrong, use
`fix(backend): cooldown` or `fix: cooldown`.

`task lint:pr-title` is the gate (ci.yml `lint:pr-title` on every PR).
Non-conforming types are ignored by the release analyzer, so a mis-titled merge
silently ships no release — the gate makes that impossible.

## Releases

[semantic-release](https://github.com/semantic-release/semantic-release) runs on
main after CI (`release.yml`, via `task release`; the tool is pinned in
`mise.toml`, plugins: commit-analyzer, release-notes-generator,
@semantic-release/github — no npm publishing):

| Commits on main                                             | Version | Example     |
| ----------------------------------------------------------- | ------- | ----------- |
| `fix` / `perf`                                              | patch   | 1.0.0→1.0.1 |
| `feat`                                                      | minor   | 1.0.0→1.1.0 |
| `feat!`/`fix!` or BREAKING CHANGE                           | major   | 1.0.0→2.0.0 |
| `docs`, `chore`, `test`, `ci`, `build`, `style`, `refactor` | none    | —           |

A release creates the `vX.Y.Z` tag and a GitHub Release with generated notes,
then dispatches `deploy.yml` with the version.

## Images: digest pins, semver tags

The deploy keeps **digest pinning** as the source of truth: GHCR tags are
mutable, digests are not. On a release, the image build additionally tags the
pushed image with the semver, and `deploy.yml` sets
`images.backend.tag`/`images.web.tag` to it. Helm records the semver in the
pod-template annotations (`idol-appreciation.club/release-tag`), which gives
every rollout an audit trail and makes redeploy-by-tag possible:

```sh
helm upgrade idol chart/ -n idol \
  --reuse-values \
  --set-string images.backend.digest="" --set-string images.backend.tag=v1.0.1 \
  --set-string images.web.digest="" --set-string images.web.tag=v1.0.1
```

Only do this deliberately: a tag can be re-pointed upstream, a digest cannot.

## Rollout behavior (zero-downtime notes)

- **backend**: RollingUpdate `maxSurge 1 / maxUnavailable 0`, readiness on
  `/api/health`. SIGTERM triggers axum graceful shutdown; long-lived SSE streams
  are drained up to `terminationGracePeriodSeconds: 30`, and web clients
  reconnect with exponential backoff (`web/src/lib/sse.ts`).
- **web (caddy edge)**: single replica on a 1-node cluster. The roll surges the
  new pod before the old one stops; the RWO `caddy-data` PVC pins scheduling to
  the volume's node, where same-node multi-pod mounts are allowed, and ACME
  certs persist on that PVC (no Let's Encrypt churn per roll). `preStop` sleeps
  5s so endpoint removal propagates before caddy drains. Residual exposure is a
  sub-second connection race during that propagation — accepted and documented
  here rather than solved with a second replica: on a one-node cluster a replica
  adds no availability, and per-replica certs would double ACME issuance for
  nothing. Revisit cert-manager + >1 node before revisiting this.
