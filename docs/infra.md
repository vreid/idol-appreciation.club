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
  redirect), ClusterIssuer; `deploy/placeholder.yaml` is a bootstrap-only static
  page (deleted by the deploy workflow).
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
| `ci`             | PR + push to main                         | tofu fmt/validate/plan (PRs), oxfmt markdown, backend cargo gates, web vp gates                              |
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

State stays on the **local backend**, kept out of git (`*.tfstate*` is ignored)
and shared between CI runs through the Actions cache
(`.github/actions/tofu-setup`). The state contains secrets (DB password, S3
keys) — treat it accordingly.

If the cache is ever evicted, CI has no state and an apply would fail on name
conflicts rather than silently duplicating (all names are unique per account).
Recovery is the recreate runbook above. For a durable backend, migrate to the S3
backend on Managed Object Storage itself (`upctl`-style docs: "Managed Object
Storage as S3 State Backend"):

```hcl
backend "s3" {
  bucket                      = "tofu-state"
  key                         = "idol-appreciation.club"
  region                      = "europe-1"
  endpoint                    = "https://<objsto-endpoint>"
  access_key                  = var.state_access_key
  secret_key                  = var.state_secret_key
  skip_credentials_validation = true
  skip_region_validation      = true
  skip_requesting_account_id  = true
  force_path_style            = true
}
```

Keep the state bucket and its credentials OUT of the managed state to avoid a
circular dependency. `tofu init -migrate-state` moves existing state.

To restore state locally from a CI run: `actions/cache` keys are
`tofu-state-<run-id>`; restore with `tofu-state-` prefix fallback.

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

## Scaling: the backend chunk writer (MUST read before scaling)

Chunk mutation takes PG advisory locks (session-level, sorted lock order — see
the backend chunk path) so multi-writer safety does not depend on a single
process; the in-process lock tier is retained in front of them. Render locks
stay in-process only — cross-instance duplicate renders are benign.

N>1 is gated on the advisory-lock path being in place: replicas MUST stay 1
until then (the paint-order == seq guarantee depends on it). The constraint is
annotated on the deployment (`idol.app/scaling: single-writer-required`, see
`chart/templates/backend-deployment.yaml`) so it is greppable from the live
cluster. RollingUpdate `maxSurge=1, maxUnavailable=0` provides zero-downtime
rollouts regardless of replica count — the surge pod is not in Service until
`/api/health` passes, and the old pod drains gracefully (SSE clients reconnect
with backoff).

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
- DRIFT: once CA is enabled the live count varies 1..2; a real `tofu apply`
  (infra-apply) would snap it back to `var.node_count` and destroy the extra
  node. deploy.yml only refreshes state (`-refresh-only`), so the routine path
  is safe; before flipping the chart flag, either raise `var.node_count` or add
  `lifecycle { ignore_changes = [node_count] }`.
- The `dev-md` control-plane plan caps the cluster at 30 nodes — not a binding
  constraint at max 2.

**Cost**: the worker plan `STARTER-4xCPU-8GB` is ~€14/mo per node
(hourly-billed), so the autoscaling ceiling adds at most ~€14/mo (2 nodes) over
today's single node; HPA replicas cost nothing extra until a second node is
actually needed.

## Marker removal checklist (do not forget)

The `backend/.ci-ready` / `web/.ci-ready` markers are a trust-based bootstrap
gate. Once (a) protection + auto-merge are on and (b) the first green deploy has
landed via a merged PR:

- [ ] delete both marker files and their `ready`-job references
- [ ] switch ci.yml/deploy.yml jobs to standard path filters (`backend/**`,
      `web/**`)
- [ ] confirm skipped-vs-pending behavior under protection
- [ ] tell all agents: PRs only; broken code fails its own PR

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
- `web/Caddyfile` (baked into the web image): automatic Let's Encrypt for
  `idol-appreciation.club` + www->apex redirect, `route` block so `/api/*`
  proxies to `backend.idol.svc.cluster.local:8080` BEFORE `try_files` can
  rewrite it, `flush_interval -1` for SSE, immutable asset caching.
- ingress-nginx and cert-manager are **not installed** by deploy.yml anymore;
  `deploy/20-ingress.yaml` and `deploy/30-issuer.yaml` are dead files kept only
  for history.
- Bootstrap without the web image: `deploy/placeholder.yaml` becomes the edge
  (plain HTTP LoadBalancer) and is deleted once web is ready.
- Because images are tag-pinned (`:main`), all Deployments set
  `imagePullPolicy: Always` (a node-cached `:main` otherwise masks new pushes;
  the deploy job additionally pins exact digests via
  `kubectl set image ...@digest`).

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

The caddy PVC uses the chart-owned `idol-caddy-data` StorageClass (reclaimPolicy
Delete, WaitForFirstConsumer): deleting a release or recreating the cluster
removes its backing disk automatically — no manual volume cleanup after destroy.
(The CCM-provided upcloud classes are Retain and their policy is immutable; do
not use them for stateful chart volumes.)

### DNS during recreate

`scripts/sync-dns.sh` is idempotent and re-points A/AAAA to whatever the current
edge LB hostname resolves to; it runs in the deploy flow after the LB exists. If
the LB is mid-provisioning (or was deleted by an atomic-uninstall churn), re-run
the deploy once the Service has an ingress — or resolve `<lb-id>.upcloudlb.com`
and run the script manually.
