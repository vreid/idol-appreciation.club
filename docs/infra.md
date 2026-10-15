# Infrastructure — idol-appreciation.club

Everything runs in UpCloud zone `fi-hel2` (Helsinki #2), all hourly-billed, all
recreatable on demand. OpenTofu (`tofu/`) declares the resources; GitHub Actions
drives apply/destroy/recreate/deploy; Cloudflare (DNS only) points the domain at
the Kubernetes LoadBalancer.

## How it fits together

```
Cloudflare (DNS ONLY, proxied=false)
  idol-appreciation.club / www ──> UpCloud Load Balancer (Essentials, €0)
                                     |  created by the UpCloud CCM for the
                                     |  ingress-nginx Service (type
                                     |  LoadBalancer)
                                     v
  ingress-nginx (TLS via cert-manager, Let's Encrypt HTTP-01)
                                     v
  ns `idol`: Deployment backend (Rust, /api/health)
             ClusterIP Service backend:80 -> :8080
             secret app-env (DATABASE_URL, S3_*, SESSION_SECRET)
                                     |
        +----------------------------+---------------------+
        v                            v                     v
  Managed PostgreSQL          Managed Object Storage   (canvas blobs)
  rdb.development.1CPU-1GB    europe-1 (fi-hel2), bucket
  db `app`, user `app`        `idol-canvas`, user `app`
```

- `tofu/` — network, Managed Kubernetes cluster (`dev-md` Development control
  plane), 1 worker node, managed PostgreSQL, managed object storage + bucket +
  app credentials.
- `deploy/` — namespace, backend Deployment/Service, Ingress (apex + www
  redirect), ClusterIssuer; legacy bootstrap manifests kept for history only
  (the chart is the deploy surface; the placeholder edge is retired).
- `scripts/sync-dns.sh` — idempotent Cloudflare record upsert.

### DNS is deliberately not in tofu

The LoadBalancer IP exists only after ingress-nginx is applied to the cluster,
which happens after `tofu apply`. Baking that IP into tofu state would make
destroy/recreate fight a stale address. Instead the deploy workflow calls
`scripts/sync-dns.sh <ipv4> [ipv6]` (curl against the Cloudflare API) once the
LB IP is known. Records are always created with `proxied=false` — DNS only, per
project requirement.

## Workflows

| workflow         | trigger                                   | does                                                                                                         |
| ---------------- | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `ci`             | PR + push to main                         | tofu fmt/validate (PRs, credential-less), oxfmt markdown, backend cargo gates, web vp gates                  |
| `infra-apply`    | push to main touching `tofu/`, manual     | `tofu apply -auto-approve`                                                                                   |
| `infra-destroy`  | manual, input `confirm=destroy`           | `tofu destroy -auto-approve`                                                                                 |
| `infra-recreate` | manual                                    | destroy (best effort) then apply                                                                             |
| `deploy`         | push to main (backend/web/deploy), manual | cargo/vp gates, image build + push to GHCR, kubeconfig fetch, kubectl apply, secret patch, DNS sync, rollout |

Secrets live in the GitHub **environment `production`** (not repo-level):
`UPCLOUD_API_TOKEN`, `CLOUDFLARE_API_TOKEN`, `SESSION_SECRET`. Every job
touching them declares `environment: production`. `GITHUB_TOKEN` gets
`packages: write` for GHCR; no static kubeconfig exists anywhere.

### Workflow lint discipline

One stack mutation at a time: `infra-apply`, `infra-destroy`, `infra-recreate`
and `deploy` share the `idol-stack` concurrency group — runs queue instead of
overlapping (no tofu state locks, no kubeconfig for a half-Ready cluster).
`infra-apply` and `infra-recreate` chain deploy automatically on success
(`gh workflow run deploy.yml`), so apply -> deploy is deterministic; pushes to
main auto-queue behind any in-flight apply.

`jactionlint` (jdx fork, v2.1.0 release binary) is the first CI job and MUST
pass locally before every push:

```
curl -sSLf -o /tmp/jl.tar.gz https://github.com/jdx/jactionlint/releases/download/v2.1.0/jactionlint_2.1.0_linux_amd64.tar.gz
tar xzf /tmp/jl.tar.gz -C /tmp jactionlint
/tmp/jactionlint .github/workflows/*.yml
```

Its shellcheck rules only run when a `shellcheck` binary exists locally (CI
runners have one) — without it the local run is silently weaker than CI. Actions
are pinned by full-length SHA; jobs carry timeout-minutes.

Known trap: `hashFiles()` is illegal in job-level `if:` — owner gating uses the
`ready` job's `test -f` outputs instead.

## Image pipeline: builds decoupled from deploys

Merges to main do not build or deploy images directly. `images.yml` (runs after
CI completes on main) diffs each component against the merge SHA recorded for
its last build and pushes only changed images, then records
`component -> {digest, mergeSha}` into `images.lock.json` via an auto-merge
`chore(images)` PR. Merging that PR is what triggers `deploy.yml`, which is a
pure consumer of the lock file: no build steps, no empty-digest state by
construction, and manual dispatches valid without a preceding build. Code merges
reach the cluster only when their image lands in the lock file; chart and script
changes redeploy in place with unchanged digests.

Policy:

- **Default pair = same merge SHA.** Both lock entries are normally updated by
  the same run, preserving today's atomic deploy semantics. Independent
  per-component picks are deferred until the web<->backend API/SSE contract has
  explicit graceful-degradation rules — that contract is expected to widen when
  fastingest's batch SSE events land; coordinate the tag pair at that point
  rather than before.
- **Fewer pod rolls per merge.** Deployments are digest-pinned, so a deploy
  whose lock entry is unchanged for a component leaves that Deployment
  untouched: a backend-only merge rolls only the backend pod, and a chart-only
  merge rolls neither. Soak and maintenance windows shrink accordingly.
- **Releases are TAGS, nothing else.** `release.yml` (on every push to main)
  cuts the `vX.Y.Z` tag from conventional commits — no changelog file, no GitHub
  Release; git log IS the changelog. The tag is pushed with the `OMP_PAT` secret
  (a GITHUB_TOKEN push does not trigger tag workflows), so the tag event itself
  fires `images.yml`, which builds from the tag's commit, tags the images
  `vX.Y.Z` + digest, and updates the lock — version -> image is causal, and a
  version can never strand without its images. If `OMP_PAT` is unset,
  release.yml falls back to explicitly dispatching `images.yml` for the tag
  (with a loud warning).
- **`main` rolling guard.** Pushes touching `backend/**` or `web/**` also build
  the `main` rolling tag + lock update without a semver — src-touching commits
  that strict conventional rules don't bump still get fresh images.
- **One build per tree, warm layer cache.** A release tag and the `main` push
  for the same commit fire together, and a second merge can land before the
  first lock PR does. Each `images.yml` run first waits for earlier runs of its
  commit, then reuses any image (current `main` or the lock pin) whose
  `club.idol.build.tree` label matches the build-context tree and only re-tags
  it. Real builds import and export the BuildKit layer cache through
  `<package>:buildcache` (`mode=max`). The backend Dockerfile compiles
  dependencies in their own cargo-chef layer, so a source-only change rebuilds
  just the crate. A lock job whose digests the open lock PR already records does
  not re-push (that would restart the PR's checks).
- **GHCR retention.** After every images build and nightly (`ghcr-prune.yml`),
  `task image:prune` keeps the `main` and `buildcache` tags + the last 10
  `vX.Y.Z` tags per package and prunes the rest; digests pinned by
  `images.lock.json` are verified protected before any deletion, and untagged
  versions are kept (their digest can't be verified against the pin).
- **tofu is decoupled** on its own `infra/**` paths (apply/destroy/ recreate
  workflows); `deploy.yml` only runs the harmless state-settle refresh, with its
  output kept out of the (public) Actions log.

### OMP_PAT secret (one-time, user-added)

`release.yml` needs a fine-grained personal access token named `OMP_PAT`:

1. GitHub → Settings → Developer settings → Personal access tokens →
   **Fine-grained tokens** → Generate new token.
2. Repository access: **Only select repositories** →
   `vreid/idol-appreciation.club`.
3. Permissions: **Contents: Read and write** — nothing else.
4. Expiration: at most 1 year.
5. Repo → Settings → Secrets and variables → **Actions** → New repository secret
   → name `OMP_PAT`, value = the token.

Without it, releases still work via the explicit-dispatch fallback; with it, the
tag push triggers the image build directly on the tag event.

## Destroy / recreate runbook

1. Actions → **infra-destroy** → Run workflow → `confirm: destroy`. Everything
   (cluster, node, DB, object storage, LB, LE certs) is deleted; hourly billing
   stops. DNS records remain but go stale (harmless).
2. Actions → **infra-apply** (or **infra-recreate** to do both in one run).
3. Actions → **deploy** (rebuilds image if needed, reapplies k8s resources,
   re-syncs DNS, waits for rollout).
4. Verify: `kubectl get nodes`, LB IP assigned,
   `dig +short idol-appreciation.club`, https load with a fresh LE cert.

One full cycle costs pennies; leave the stack up otherwise.

### Known destroy blocker: orphan LoadBalancer

The UpCloud CCM creates the LoadBalancer outside tofu state. Deleting the
cluster does not delete it, and its network interface keeps the tofu network
alive (`NETWORK_NOT_EMPTY` 409). If destroy fails with that error, delete the LB
by hand, wait ~2 minutes for the interface to clear, then retry:

```
curl -X DELETE -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  https://api.upcloud.com/1.3/load-balancer/<uuid>
```

### State handling

State lives in the **S3 backend on Managed Object Storage** (bucket
`tofu-state`, key `idol-appreciation.club`, region `europe-1` — see the
`backend "s3"` block in `tofu/versions.tf`). The state contains secrets (DB
password, S3 keys) and therefore never touches Actions caches: any PR —
including from a fork — can read caches created on the base branch, so the old
cache-based local backend was a credential-exposure hole (issue #88). The
`tofu-setup` composite caches provider plugins only; the state bucket
credentials travel per-run as repo secrets.

Bucket and credentials are provisioned **outside** the managed state (no
circular dependency). One-time bootstrap, done manually in the UpCloud console
or via `upctl`:

1. Create a Managed Object Storage in `europe-1` with a bucket `tofu-state`
   (private, no public access).
2. Create an object-storage access key scoped to that bucket (read-write).
3. Repo secrets (Settings → Secrets → Actions): `TOFU_STATE_ENDPOINT` (the
   bucket's S3 endpoint URL), `TOFU_STATE_ACCESS_KEY`, `TOFU_STATE_SECRET_KEY`.

Migrating the pre-existing state off the Actions cache (one-time, while the old
cache entry still exists):

```
gh cache download --repo vreid/idol-appreciation.club --key tofu-state-
unzip '*.zip'   # yields tofu/terraform.tfstate
cp terraform.tfstate tofu/
cd tofu && tofu init -migrate-state -input=false \
  -backend-config="endpoint=$TOFU_STATE_ENDPOINT" \
  -backend-config="access_key=$TOFU_STATE_ACCESS_KEY" \
  -backend-config="secret_key=$TOFU_STATE_SECRET_KEY"
```

Rotation after landing the S3 backend (issue #88 remediation): rotate the
managed-DB password and the S3 secret access key that the state leaked exposure
window covered; update the matching repo secrets in the same change
(`deploy.yml` patches cluster secrets on the next deploy).

Where plan/apply may run with credentials, and where they may not:

| flow                               | credentials | state | trigger                    |
| ---------------------------------- | ----------- | ----- | -------------------------- |
| `deploy.yml`                       | yes         | yes   | push to `main` (trusted)   |
| `infra-apply/destroy/recreate.yml` | yes         | yes   | manual `workflow_dispatch` |
| `ci.yml` `tofu-plan`               | **none**    | none  | any PR touching tofu paths |

The PR gate runs `task tofu:validate` (`init -backend=false` + `validate`):
PR-authored recipes are executed with no cloud tokens, no environment, and no
state — a rewritten recipe cannot reach production (issue #87). The `production`
environment additionally requires a reviewer approval for any job that declares
it.

### State drift rebuild + pre-merge gate (2026-10-13, validated)

Two findings from the autoscaling PR's merge gate:

- `ci.yml`'s tofu-plan job is **structurally stateless** — it never touches the
  Actions cache, so every PR plans against empty state ("12 to add"). That check
  proves config validity, nothing about live infra. The only state-aware CI
  paths are `infra-apply`/`deploy` (they use `.github/actions/tofu-setup`, which
  restores `tofu-state-*` and saves it back).
- The shared checkout's tfstate AND the CI cache had drifted: the live cluster
  (`0d4c74ac-…`) was not the state's cluster (`0d25605e-…`); state refresh
  showed the live node group as deleted and planned a duplicate node group on
  the dead one.

Validated rebuild (read-only against live; do it in a scratch worktree, never
the shared checkout):

1. `tofu init`, then delete local `terraform.tfstate*`.
2. Import all 12 resources. IDs: network / k8s cluster / PG / object storage =
   their UUIDs (cluster's network UUID comes from `GET /1.3/kubernetes/{uuid}` →
   `network`); node group = `{cluster_uuid}/default`; logical db + db user =
   `{db_uuid}/app`; bucket = `{objsto_uuid}/idol-canvas`; objsto user =
   `{objsto_uuid}/app`; policy = `{objsto_uuid}/app-bucket-access`;
   user_policy = `{objsto_uuid}/app/app-bucket-access`; access key =
   `{objsto_uuid}/app/{access_key_id}`.
3. `tofu apply -refresh-only -auto-approve` (reconciles computed fields).
4. Expected plan: **0 to add, 0 to destroy**, 3 benign in-place lines — PG
   `components` (importer populates a computed list), objsto policy `document`
   (re-encode of identical JSON), db user `password` (unreadable via API —
   documented above). None are node-related. A real apply of the password line
   rotates the app DB password once; the deploy chain self-heals it (deploy.yml
   always re-reads the password from the API and patches `app-env`).
5. A/B gate for infra-relevant PRs: plan the branch AND main against the same
   rebuilt state — they must be byte-identical (proves the PR adds no
   provider-level behavior).

Before merging any PR that touches `tofu/**`: reconcile the CI cache by
dispatching `deploy.yml` once (no code change needed — its refresh-only step
reconciles and re-saves `tofu-state-*`), check its "tofu state settle" step
shows 0 add / 0 destroy, only then merge; the chained infra-apply then plans
zero against live.

### Retrieving account credentials

`scripts/account-password.sh [admin-account|seed-account]` prints the stored
username + password for either account secret (namespace idol; passwords are
initial-only until the change-password endpoint lands).

### Rotating SESSION_SECRET

`gh secret set SESSION_SECRET --env production` (64 hex chars). The deploy
workflow pushes it into the `app-env` secret on the next run.

## Ship loop (post-protection)

Once branch protection is on, every change ships via short-lived branch + PR;
`gh pr create --fill --auto-merge=squash` queues the merge and deploy fires on
merge. main stays green by construction. Agents stop pushing to main directly.

## Deploy image digests (2026-10-13 incident note)

A workflow-dispatch deploy whose image builds had been skipped rolled pods with
malformed image refs (`InvalidImageName`) and had to be rolled back. Two guards
now make that state unreachable: `deploy:k8s` fails fast before any cluster
contact when an image that would roll has an empty digest, and the chart's image
helper refuses to render — at `helm template` time, so `task helm:lint` catches
it pre-merge — any `images.*` entry with a missing, malformed digest, or
malformed tag. Digests remain the source of truth for what actually runs; the
semver `tag` is an audit trail only, never concatenated into the ref.

## Deploy secret choreography (S3 auth invariant)

`deploy:k8s` provisions `app-env` (DB URL, S3 creds, session secret) **before**
the helm upgrade, and never blindly rotates: the cluster's current S3 creds are
probed with a cheap signed ListObjects — HTTP 200 keeps them (idempotent
deploys, zero key churn), HTTP 403 rotates, and anything else aborts the deploy
with prod untouched. Rotation therefore happens only while no pod has rolled, so
the upgrade itself brings every pod up on a live key: **deploys never invalidate
working S3 auth** (no `SignatureDoesNotMatch` windows). A trailing
`rollout restart` runs only on the rotate branch, and only to cover upgrades
that were a no-op for pods. The DB password stays fresh-from-API on every deploy
(UpCloud may rotate managed-DB user passwords; state is not trusted for it).

## Deploy secret choreography (S3 auth invariant)

`deploy:k8s` provisions `app-env` (DB URL, S3 creds, session secret) **before**
the helm upgrade, and never blindly rotates: the cluster's current S3 creds are
probed with a cheap signed ListObjects — HTTP 200 keeps them (idempotent
deploys, zero key churn), HTTP 403 rotates, and anything else aborts the deploy
with prod untouched. Rotation therefore happens only while no pod has rolled, so
the upgrade itself brings every pod up on a live key: **deploys never invalidate
working S3 auth** (no `SignatureDoesNotMatch` windows). A trailing
`rollout restart` runs only on the rotate branch, and only to cover upgrades
that were a no-op for pods. The DB password stays fresh-from-API on every deploy
(UpCloud may rotate managed-DB user passwords; state is not trusted for it).

## Scaling: the backend chunk writer (MUST read before scaling)

CNPG note (2026-10-14): the worker floor is 3 (`tofu/ var.node_count`, live
count bumped by `task cnpg:node-scale` — the node group's `ignore_changes` means
tofu never snaps the live count back). The CNPG Cluster pins one database
replica per node via required pod anti-affinity, so 3 workers is the minimum
viable fleet; the Cluster Autoscaler bounds (chart/ values) must stay
`minSize: 3, maxSize: 4` when enabled.

Chunk mutation takes PG advisory locks (session-level, sorted lock order — see
the backend chunk path) so multi-writer safety does not depend on a single
process; the in-process lock tier is retained in front of them. Render locks
stay in-process only — cross-instance duplicate renders are benign.

The advisory-lock path is in place, so N>1 is an availability knob, not a
correctness gate: the fleet runs `backend.replicaCount: 2` (zero-blip rolls —
the surge pod is not in Service until `/api/health` passes — and half the blast
radius per pod). HPA is deliberately off: the CPU signal does not match the
actual bottleneck (pool/latency; issue #120), revisit after it lands. The old
pod drains gracefully (SSE clients reconnect with backoff).

Decision record — the committed scale path, in order, when traffic justifies
(trigger evidence comes from `/api/metrics` (#51), not forecasts):

1. Advisory locks on chunk mutation (in-tx, atomic with the write — chosen over
   Valkey/Redis distributed locks: no second consistency domain, no lock-TTL/tx
   mismatch, no write-path SPOF).
2. Then validate `backend.replicaCount = 2`.
3. Flip the backend HPA (`backend.autoscaling.enabled`, below).
4. CDN when read traffic dominates (tile GETs are immutable and
   content-addressed — ideal cache candidates) — before any further compute.
5. Managed Valkey (1C/1GB) ONLY if the metrics endpoint shows tile-read p95
   dominated by S3-GET latency (role: shared tile LRU across replicas).

### Autoscaling, staged (HPA + node group)

Both halves are wired but **disabled by default**; the flags are values-only
flips once their gates pass.

**Backend HPA** — `chart/templates/backend-hpa.yaml` behind
`backend.autoscaling.enabled` (default `false`): min 1 / max 3 replicas, CPU
target 65%, 5-minute scale-down stabilization (scale-up stays immediate). When
enabled, the Deployment omits `replicas` so the HPA owns the count. Hard gate:
the backend is a single writer — PG advisory locks on the chunk path are not
validated at >1 replica, and auto-scaling a single-writer to 3 replicas is a
data-ordering bug factory. Flip only AFTER: locks merged (order step 1) →
`backend.replicaCount = 2` soak-validated (step 2).

Scale-on-noise caveat: cold tile renders and prewarm bursts spike CPU (libvips
webp decode/encode) for a couple of minutes. A utilization-based HPA will read
those bursts as load; the 5m stabilization window stops them driving
down-scales, but scale-UPS fire immediately — expect an occasional replica added
for a prewarm and judge the 65% target from `/api/metrics`, not the first noisy
hour.

**Node group** — UpCloud managed Kubernetes has NO native node-group min/max
autoscaling: the 1.3 API and provider v5.47.0 schema expose only a fixed count
(`node_count`; API JSON field `count`), so "set max>min" in tofu is impossible.
Autoscaling is the Kubernetes Cluster Autoscaler (UpCloud's cloud-provider
build, `ghcr.io/upcloudltd/autoscaler`) running in-cluster: it PATCHes the
node-group count within `--nodes=<min>:<max>:<nodeGroup>` when pods are pending,
and scales down idle nodes (~10–15 min default). Shipped as
`chart/templates/cluster-autoscaler.yaml` behind `clusterAutoscaler.enabled`
(default `false`), bounds 1..2 on the `default` group. The deploy workflow
provisions the `upcloud-autoscaler` secret (account token) and passes the
cluster uuid on every deploy, so flipping is a values-only change. Flip notes:

- tofu's `node_count` stays the floor (1). FIELD-NAME TRAP that caused the
  2026-10-13 node-roll outage: bare `count` in tofu is the meta-argument — it
  clones the node-group resource and rolls every node; the attribute is
  `node_count`.
- DRIFT: CA owns the live count, so the node group carries
  `lifecycle { ignore_changes = [node_count] }` in tofu — a real `tofu apply`
  (infra-apply) can no longer destroy a scaled-up node. deploy.yml only
  refreshes state (`-refresh-only`); raise capacity via the CA bounds in
  `chart/values.yaml`, not `var.node_count`.
- The `dev-md` control-plane plan caps the cluster at 30 nodes — not a binding
  constraint at max 2.
- FIRST-HOUR WATCH (flip day): the pending-pod signal can be spoofed by
  kube-system pods that can never schedule on a 1-node group — coredns (topology
  spread wants a second node) and cilium-operator (hostPort conflicts) sit
  Pending forever (live-verified). Cluster Autoscaler does scheduling simulation
  — it only scales for pods that would fit post-scale-up — but watch its first
  hour against exactly these two; a group pinned at max with no app load is this
  failure mode.

**Cost**: the worker plan `STARTER-4xCPU-8GB` is €20/mo per node (hourly-billed,
~€0.03/h; billed hours cap at a 28-day month), so the autoscaling ceiling adds
at most €20/mo (2 nodes) over today's single node; HPA replicas cost nothing
extra until a second node is actually needed.

## Readiness markers: removed

The `backend/.ci-ready` / `web/.ci-ready` trust-based bootstrap gate and the
placeholder edge were retired once branch protection + auto-merge were on: CI
gates run unconditionally (tier-2 jobs path-filtered by the `changed` job in
ci.yml), the chart has no `web.enabled` split, and `deploy/placeholder.yaml` is
gone. Broken code fails its own PR; nothing waits on a marker file.

## Environment protection (later)

`production` currently has no protection rules — destroy must stay friction-free
for solo dev. When the team grows, add a gate on the dangerous paths only:

Settings → Environments → **production** → Required reviewers → add the
maintainer account. That gates `infra-destroy`/`infra-recreate`/`deploy` waits.
If that friction is unwanted for apply/deploy, move infra-destroy's job to a
dedicated environment `production-dangerous` with the reviewer rule, leaving
`production` free.

## Provider quirks worth remembering

- The UpCloud provider's registry namespace is **`UpCloudLtd/upcloud`** (not
  `UpCloudProvider/upcloud`).
- API auth is a **bearer token** (`UPCLOUD_TOKEN`), not basic auth.
- Zone ids differ per product: k8s/DB/object storage network use legacy
  `fi-hel2`; Managed Object Storage uses logical region `europe-1` (primary zone
  fi-hel2).
- PostgreSQL `plan` is deprecated; use `plan_compute` + `plan_node_count` +
  `plan_storage_gib` + `plan_backups` (here: `rdb.development.1CPU-1GB`, 1, 10,
  `mini`).
- Bucket capacity is a subscription concern — the API resource has no size
  attribute; the account's 250 GB tier covers it.
- The CCM's default LoadBalancer plan is `development` (€10/mo); the Service
  annotation `service.beta.kubernetes.io/upcloud-load-balancer-config` pins
  `{"plan":"essentials"}` (€0/mo).
- The CCM's default LoadBalancer config is broken for this stack: the generated
  `health_check_url` points at a nodePort that does not exist (all backends
  report down, LB answers nothing), and its 443 frontend TLS-terminates with the
  controller's self-signed cert (invalid in browsers). deploy.yml therefore pins
  the Service nodePorts (30859 / 30875) and overrides the annotation:
  `frontends: 443 mode=tcp (passthrough, nginx serves the LE cert)`,
  `backends: http -> http://:30859/healthz`, `https -> tcp health check`.
- The CCM assigns the LB a DNS hostname (not an IP) in
  `.status.loadBalancer.ingress[].hostname`; scripts/sync-dns.sh callers must
  resolve it (getent) before upserting records.
- The LB list endpoint is `GET /1.3/load-balancer` (dash), not `/load_balancer`.
- Plan listing lives at `GET /1.3/plan` (singular).

## Serving-path transition: Caddy-direct edge (2026-10-12)

The ingress-nginx + cert-manager serving path was replaced by a Caddy-direct
edge. Winning reason: the UpCloud CCM load balancer for ingress-nginx fought the
TLS chain three ways (default HTTP health checks killing members, stale member
ports after nodePort re-pinning, and the LB auto-terminating TLS with its own
*.upcloudlb.com certificate), while Caddy does HTTP-01, SPA fallback, API
proxying and SSE unbuffering in one place.

Current edge:

- `web` Service is **type=LoadBalancer** (Essentials plan): port 80 -> Caddy
  8080, port 443 (TCP passthrough) -> Caddy 8443. TCP health checks.
- `chart/caddy/Caddyfile` (helm-templated into the `caddy-config` ConfigMap —
  **the chart is the source of truth now**; `web/Caddyfile` only covers
  stock-caddy local dev and is no longer synced at deploy time): automatic Let's
  Encrypt for `idol-appreciation.club` + www->apex redirect, `handle` groups so
  `/api/tiles/*` passes through the cache handler while the rest of `/api/*`
  proxies untouched with `flush_interval -1` for SSE, immutable asset caching.

### Horizontal edge: 2+ replicas, shared TLS, tile cache (phase 5)

The single-mount TLS volume (`caddy-data` PVC) is gone; certificates and ACME
state live in object storage via a CertMagic S3 storage module, so the edge runs
`web.replicas: 2` with a PodDisruptionBudget (`minAvailable: 1`) and topology
spread across nodes (docs/scaling-architecture.md, "Caddy, horizontally").

- **Custom build** (`caddy/Dockerfile`, `task image:caddy`, digest-pinned in
  `images.lock.json` like backend/web): Caddy 2.11.7 + xcaddy-pinned
  `caddyserver/cache-handler` v0.17.0 with the in-memory otter storage plugin
  (RFC 7234 strict, `default_cache_control no-store` — only responses carrying
  explicit cache headers are stored, i.e. the versioned tile responses; each
  replica keeps its own cache) + `darkweak/storages/otter/caddy` v0.0.20.
- **CertMagic S3 storage** (`caddy/certmagic-s3/`, vendored): upstream
  `techknowlogick/certmagic-s3` v1.5.0 carries a broken not-found path (its
  `Stat`/`Load` only match `*types.NoSuchKey`; `HeadObject` 404s surface as
  `*types.NotFound`, so certmagic can't tell "not stored yet" from "storage
  error") and logs credentials at INFO. The vendored copy carries the four local
  fixes a production deployment of the same module needed (not-found reporting,
  log redaction, reload refresher cleanup, endpoint validation) — see
  `caddy/certmagic-s3/VENDORED.md`. The lock is S3-native: atomic
  `If-None-Match: *` creation, heartbeat, staleness via the server's `Date` —
  one replica renews, the other reads the renewed object. The lock's store
  requirements (conditional writes honoured) and the missing-key error shape
  were probed against the live bucket before this design was locked in —
  receipts in the phase 5 vet run (PR #202). Credentials ride the `AWS_`
  standard chain from the existing `app-env` secret; endpoint/bucket are
  helm-injected by deploy.yml from the same API-verified values app-env uses,
  prefix `caddy-acme/`.
- **Binary delivery**: the caddy image is an initContainer that copies the
  custom binary into an emptyDir; the web image (static assets) execs it.
  `web/Dockerfile` and its pipeline are untouched; a module bump rolls only the
  initContainer digest.
- **ACME boot note**: the first horizontal rollout starts from an empty storage
  prefix — both replicas coordinate through the shared lock and one fresh
  certificate is issued (1 of the 5/week Let's Encrypt duplicate budget). The
  old PVC cert is unrecoverable by design; the rollback path is reverting the
  chart (replicas 1 + stock binary) and re-running ACME, which the same budget
  covers.
- ingress-nginx and cert-manager are **not installed** by deploy.yml anymore;
  `deploy/20-ingress.yaml` and `deploy/30-issuer.yaml` are dead files kept only
  for history (the issuer's nginx solver is obsolete — if the S3 module ever has
  to be abandoned, the documented fallback is cert-manager with a DNS-01 solver
  writing a Secret every replica mounts, plus a restart on renewal).
- Because images are tag-pinned (`:main`), all Deployments set
  `imagePullPolicy: Always` (a node-cached `:main` otherwise masks new pushes;
  the deploy job additionally pins exact digests via helm
  `--set-string images.*.digest`).

Cluster-level-by-hand changes made during the outage were repo-ified:
securityContext relaxations (`deploy/10-backend.yaml`: image USER idol is
non-numeric so kubelet cannot verify runAsNonRoot; `deploy/15-web.yaml`:
seccomp/capability hardening broke caddy exec), imagePullPolicy Always, and the
Caddyfile itself.

### PR mode (2026-10-12)

Main is now protected: squash-merge only, auto-merge enabled,
delete-branch-on-merge, required checks = ci.yml jobs (tofu fmt+validate,
jactionlint, oxfmt markdown, owner readiness markers, backend cargo gates, web
vp gates, tofu plan). Direct pushes to main are being phased out; agents work on
branches and use `gh pr merge --auto --squash`.

## State-loss recovery (2026-10-12 incident, runbook)

The local tofu state file was clobbered externally (0 resources) while all
UpCloud objects stayed alive. Recovery that worked, in order:

1. `tofu import` every live resource (creates nothing). Import IDs used:
   network/cluster/PG/objsto = UUID; node group = `{cluster}/default`; logical
   db + db user = `{service}/{name}`; objsto bucket = `{svc}/{name}`; objsto
   user = `{svc}/{username}`; policy = `{svc}/{name}`; user_policy =
   `{svc}/{username}/{policy}`; access key = skipped (recreated instead).
2. `tofu apply` with `-replace` semantics for the two secret-bearing resources
   whose generated values are unreadable via API:
   `upcloud_managed_database_user.app` (password) and
   `upcloud_managed_object_storage_user_access_key.app` (secret key).
3. Re-apply the `app-env` secret from fresh `tofu output -raw` values.

PG15 gotcha: the provider-created `app` role owns nothing - first backend boot
fails on `permission denied for schema public`. One-time grant:
`ALTER DATABASE app OWNER TO app; GRANT ALL ON SCHEMA public TO app;` as the
admin URI.

## Costs (monthly, Helsinki fi-hel2)

| Item                             | Plan                              | Price       |
| -------------------------------- | --------------------------------- | ----------- |
| Managed Kubernetes control plane | Development tier                  | EUR 0       |
| Worker node                      | STARTER-4xCPU-8GB                 | ~EUR 14     |
| Managed PostgreSQL               | rdb.development.1CPU-1GB          | ~EUR 9      |
| Managed Object Storage           | 250 GB tier                       | EUR 5       |
| Load balancer                    | Essentials (via CCM, web Service) | EUR 0       |
| **Total**                        |                                   | **~EUR 28** |

Hourly billing everywhere; a destroy/recreate cycle costs pennies.

## Debugging the backend gate environment locally

Podman can reproduce the CI gate environment exactly: the backend gates run
inside the pinned `rust:trixie` + libvips 8.16.1 builder image, so
`podman build --target builder -t backend-builder backend` +
`podman run --rm backend-builder bash -c 'cargo test'` reproduces CI test
behavior locally (the backend agent used this to chase the webp determinism
failure).

## UpCloud S3 quirk

Streamed SigV4 PUTs (boto3 default signed payload) fail with
`XAmzContentSHA256Mismatch` through UpCloud's object-storage LB; presigned URLs
and header-signed PUTs work. Backend blob uploads must be verified early; if
streamed PUTs fail in Rust too, switch to buffered/presigned writes.

## Contributing / PR checklist (ALL agents)

Before EVERY PR, from the repo root:

```
npx --yes oxfmt@0.72.0 --write "**/*.md"   # markdown (second-md-failure rule)
cd web && vp check && vp test; cd ..       # web: typecheck+lint+fmt, tests
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings; cd ..
helm lint chart                            # chart changes
/tmp/jactionlint .github/workflows/*.yml   # workflow changes (or the release binary)
git diff --check                           # whitespace
```

CI is fail-fast: `lint (all areas)` aggregates every linter; one red lint skips
ALL slow test jobs within ~90s. `ci-ok` fails on any skipped gate, so skipping
is never a merge path.

## Admin account

`scripts/bootstrap-admin.sh` is idempotent: credentials live only in the k8s
secret `admin-account` (namespace idol); first run generates a random 32-char
password, later runs reuse it. It registers via the public API, promotes the
account (`is_admin=true`) and demotes every other admin (e2e bots used to win
the first-registered race; 9 were demoted on 2026-10-12), then verifies via
`/api/auth/me`.

- retrieval:
  `kubectl -n idol get secret admin-account -o jsonpath='{.data.username}' | base64 -d`
  (same for `.data.password`)
- approve a user (approval feature):
  `./scripts/bootstrap-admin.sh --approve USERNAME` (psql `approved=true`; if
  the column/migration hasn't landed yet the temporary fallback is
  `psql "$ADMIN_URI_APP" -c "UPDATE accounts SET approved=true WHERE username='<seed>'"`)
- rotation (v1, no change-password endpoint yet): generate a new password,
  update the secret, then `UPDATE accounts SET password_hash=...` via psql with
  an argon2 hash in the backend's format. Backend TODO: proper change-password
  endpoint.

Seed runbook: register seed account -> approve it -> SEED_ALL.

## Recreate proof cycle 3 (2026-10-12/13, full destroy → apply → site-up)

The on-demand destroy/recreate requirement was proven end-to-end via
`infra-recreate` (workflow_dispatch) followed by `deploy`. Four real defects
were found and fixed in the process — each one only shows on a **cold** cluster:

1. **helm timeout vs CCM LB provisioning (PR #13).** A fresh cluster's CCM takes
   15–20 min to allocate and attach the edge LoadBalancer. helm
   `--atomic --wait --timeout 15m` expired mid-wait and _uninstalled_ the
   release, which in turn made CCM delete the half-built LB (churn loop). Budget
   is now `--timeout 30m`. Rule of thumb: the helm wait budget must exceed cold
   LB provisioning by a wide margin.
2. **Namespace collision (PR #12).** The chart shipped a Namespace document AND
   the deploy used `--create-namespace`; first install failed with
   `namespaces "idol" already exists`. Chart no longer ships a namespace; helm
   owns the release namespace.
3. **Secret-before-helm ordering (PR #14).** On a fresh cluster the backend
   Deployment cannot start without the `app-env` secret, but the deploy workflow
   created that secret _after_ helm. helm `--wait` then blocked on a backend
   stuck in `CreateContainerConfigError`. The secret step now runs before
   `helm upgrade --install`.
4. **Managed-DB password divergence (PR #16).** The tofu state's recorded
   `app_database_uri` did not authenticate against the recreated database
   service: UpCloud may rotate managed-DB user passwords at creation time
   relative to what the provider records. Deploy now fetches the authoritative
   `app` user password from the UpCloud API (`GET /1.3/database/<uuid>/users` —
   passwords are returned in the response) and splices it into the URI;
   host/port still come from tofu output.

Also exercised during the cycle (manual, one-time): PG 15+ denies `CREATE` on
schema `public` to non-owners, so migrations need
`ALTER SCHEMA public OWNER TO app; GRANT ALL ON SCHEMA public TO app;` executed
once per recreated database (done via a `postgres:16-alpine` Job using the
`upadmin` credentials from the same API endpoint).

### Data loss warning (destroy/recreate)

**Managed PostgreSQL is destroyed and recreated EMPTY by the recreate flow.**
All canvas/user data is lost unless backed up first (managed-DB daily backups
exist but a full re-import is manual). The recreate capability is for dev; run
`tofu destroy`/`apply` against prod data only with explicit acknowledgment.

### PV cleanup after recreate

The caddy-data PVC was removed with the horizontal edge (phase 5): TLS state
lives in object storage, so no edge volume exists to clean up. The remaining
chart-owned StorageClasses (`idol-pg-data` for CloudNativePG, reclaimPolicy
Delete, WaitForFirstConsumer) still mean deleting a release or recreating the
cluster removes their backing disks automatically — no manual volume cleanup
after destroy. (The CCM-provided upcloud classes are Retain and their policy is
immutable; do not use them for stateful chart volumes.)

### DNS during recreate

`scripts/sync-dns.sh` is idempotent and re-points A/AAAA to whatever the current
edge LB hostname resolves to; it runs in the deploy flow after the LB exists. If
the LB is mid-provisioning (or was deleted by an atomic-uninstall churn), re-run
the deploy once the Service has an ingress — or resolve `<lb-id>.upcloudlb.com`
and run the script manually.
