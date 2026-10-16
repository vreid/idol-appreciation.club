# Restore runbook — full rebuild from an empty UpCloud account

Ordered path from a bare UpCloud account to the serving site with a reseeded
canvas. Every command runs from a clean checkout of `main` unless stated.
Verified live 2026-10-16 against cluster `idol-canvas` (fi-hel2) before the
planned teardown; this document is the restorability proof for that teardown.

Doctrine: destroy/apply over in-place surgery; no data preservation. The seeder
IS the verification tool — a fresh build serves from CNPG initdb plus a full
reseed, not from restored data.

## What rebuilds what (audit summary)

| Component            | Rebuilt by                                                                         | Notes                                                                                                        |
| -------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Network + cluster    | `tofu/` (`upcloud_network.k8s`, `upcloud_kubernetes_cluster.this`)                 | `${cluster}-net` 10.30.0.0/24, dev-md control plane, public API                                              |
| Node group (3 nodes) | `tofu/` (`baseline_capacity`, name `default-new`)                                  | 3× STARTER-4xCPU-8GB; CNPG anti-affinity floors count at 3                                                   |
| Object storage (app) | `tofu/` (`idol-canvas-objsto` + bucket `idol-canvas` + user `app` + bucket policy) | User/policy in state; the access KEY is not (see landmines)                                                  |
| State backend        | **manual** (`tofu-state-objsto` + bucket `tofu-state` + user `tofu-state`)         | Out-of-band by design; tofu cannot own its own backend                                                       |
| CNPG operator        | `task cnpg:operator:install` (digest-pinned manifest)                              | One-time, out-of-chart bootstrap                                                                             |
| CNPG Cluster + PVCs  | `task cnpg:cluster:install` (chart CR, `cnpg.enabled=true`)                        | initdb creates db `idol-canvas`, owner `app`, secret `cnpg-app-creds`                                        |
| App + edge workloads | `deploy.yml` → `task deploy:k8s` (helm release `idol`)                             | Backend 2×, worker 2×, edge 6×3Gi, LB `production-small`                                                     |
| First S3 access key  | `deploy:k8s` verify-or-rotate (key-create via API)                                 | Fresh user has 0 keys → mint succeeds                                                                        |
| DNS (apex + www)     | `scripts/sync-dns.sh` (called by deploy)                                           | Cloudflare, `proxied=false`, re-pointed on every deploy                                                      |
| Canvas data          | reseed Job / seed.sh (50k+ placements, twice-proven)                               | Bucket prefixes (`tiles/`, `quarantine/`, `cnpg-backups/`, `caddy-acme/`) are created implicitly by the apps |

Managed PostgreSQL is retired (CNPG migration, 2026-10-14): the tofu config
creates no database service, and `deploy:k8s` auto-detects the CNPG-native shape
by the absence of the `app_database_uri` output. The `cnpg:cutover:*` tasks are
legacy managed→CNPG migration steps; a fresh build needs none of them.

### Bucket layout (single app bucket)

One bucket, `idol-canvas`, four prefixes — nothing to create by hand:

- `tiles/{z}/{x}/{y}.webp` — rendered tiles (`world.rs tile_key`)
- `quarantine/{job_id}` — 202 upload stash; app-deleted, lifecycle rule backs it
  up (apply a 24h rule on the bucket after first deploy if wanted)
- `cnpg-backups/` — Barman destination path (declared in values; **not yet
  bound**, see audit notes)
- `caddy-acme/` — CertMagic cert/ACME state for the edge replicas

### Audit notes (2026-10-16, live-verified)

- **CNPG backups are scaffolded, not bound.** The `objectstores` CRD is
  installed and secret `cnpg-backup-s3` exists, but no `ObjectStore` CR is
  applied and the live Cluster has no `.spec.backup`/plugin stanza (the chart
  renders an empty block there). A fresh rebuild therefore has **no WAL/base
  backup chain** — acceptable under the disposable-data doctrine (the seeder
  re-creates the pile), and the binding is a deliberate follow-up (apply
  `templates/cnpg-objectstore.yaml` with `cnpg.enabled=true` + wire the plugin
  stanza into the Cluster).
- **LB lineage.** The CCM names the edge LB `{cluster-uuid}-idol-web` (`-idol-`
  suffix regex in `task infra:delete-lbs`); it survives cluster deletion
  (gen-2's `0ab39179` did) and must be swept after destroy.
- KEDA, descheduler, Cluster Autoscaler, backend HPA, telemetry collector are
  all values-gated OFF and none are installed in the live cluster.

## Prerequisites

- UpCloud API token (`UPCLOUD_API_TOKEN`, exported as `UPCLOUD_TOKEN` for the
  API/tf provider; the repo `.env` holds the live one).
- Cloudflare API token with DNS edit on the zone (`CLOUDFLARE_API_TOKEN`).
- GitHub environment `production` secrets (repo `vreid/idol-appreciation.club`):
  `UPCLOUD_API_TOKEN`, `CLOUDFLARE_API_TOKEN`, `SESSION_SECRET` (64 hex),
  `TOFU_STATE_ENDPOINT`, `TOFU_STATE_ACCESS_KEY`, `TOFU_STATE_SECRET_KEY`.
- Toolchain: `mise install` (provides tofu, helm, kubectl, task, kubeconform).
- `gh auth` for workflow dispatches; `kubectl` reachability is per-stage (the
  kubeconfig is fetched from the UpCloud API, never stored in the repo).

## Stage 0 — state backend (the one manual resource)

The tofu state lives in Managed Object Storage and is provisioned OUTSIDE the
tofu config (versions.tf backend stub + per-run `-backend-config`). On an empty
account:

```bash
export UPCLOUD_TOKEN="$(grep UPCLOUD_API_TOKEN .env | cut -d= -f2)"
# 1. service (region europe-1, public network for the S3 endpoint)
curl -sS -X POST -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"tofu-state-objsto","region":"europe-1","configured_status":"started",
       "network":{"family":"IPv4","name":"public","type":"public"}}' \
  https://api.upcloud.com/1.3/object-storage-2 | jq -r '.uuid'
# 2. bucket + user + key (wait for the service to reach "started" first)
#    bucket: PUT /1.3/object-storage-2/<uuid>/buckets  {"name":"tofu-state"}
#    user:   POST /1.3/object-storage-2/<uuid>/users   {"username":"tofu-state"}
#    key:    POST /1.3/object-storage-2/<uuid>/users/tofu-state/access-keys
# 3. endpoint: GET /1.3/object-storage-2/<uuid> -> .endpoints[0].domain_name
# 4. GitHub secrets:
gh secret set TOFU_STATE_ENDPOINT   --env production --body "https://<domain>"
gh secret set TOFU_STATE_ACCESS_KEY --env production --body "<access_key_id>"
gh secret set TOFU_STATE_SECRET_KEY --env production --body "<secret_access_key>"
```

The same four steps for the app bucket are NOT needed — tofu owns
`idol-canvas-objsto` (Stage 1) and deploy mints the app key (Stage 3).

## Stage 1 — network, cluster, nodes, object storage

Preferred (the blessed path; 90m timeout on purpose):

```bash
gh workflow run infra-apply.yml --ref main   # tofu apply + chains deploy
```

Local equivalent (scratch worktree, never the shared checkout):

```bash
cd tofu
tofu init -backend-config="endpoint=$TOFU_STATE_ENDPOINT" \
  -backend-config="access_key=$TOFU_STATE_ACCESS_KEY" \
  -backend-config="secret_key=$TOFU_STATE_SECRET_KEY"
tofu apply -auto-approve -input=false
```

Fresh build creates: network `idol-canvas-net` (10.30.0.0/24 — the config
default IS the live CIDR; see landmines), cluster `idol-canvas` (dev-md,
fi-hel2, `control_plane_ip_filter 0.0.0.0/0`), node group `default-new` 3×
STARTER-4xCPU-8GB, object storage `idol-canvas-objsto` (europe-1) with bucket
`idol-canvas`, user `app` and a bucket-scoped `s3:*` policy. No access key
(landmine below), no managed database.

The chained deploy is expected to fail on a first CNPG-native build (no
`cnpg-app-creds` yet) BEFORE touching helm — harmless; Stage 3 re-runs it.

## Stage 2 — kubeconfig + in-cluster platform

```bash
export KUBECONFIG=/tmp/idol-fresh-kubeconfig
uuid="$(curl -sS -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  https://api.upcloud.com/1.3/kubernetes | jq -r '.[] | select(.name=="idol-canvas") | .uuid')"
curl -sS -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  "https://api.upcloud.com/1.3/kubernetes/$uuid/kubeconfig" | jq -r .kubeconfig > "$KUBECONFIG"
chmod 600 "$KUBECONFIG"
kubectl get nodes   # wait for 3/3 Ready
```

Fresh clusters have no `idol` namespace and no `app-env`; the CNPG tasks and the
first deploy both need it (backup secret copies from app-env):

```bash
kubectl create namespace idol
kubectl -n idol create secret generic app-env \
  --from-literal="DATABASE_URL=postgresql://app:placeholder@idol-db-rw.idol.svc:5432/idol-canvas" \
  --from-literal="S3_ENDPOINT=https://$(curl -sS -H "Authorization: Bearer $UPCLOUD_TOKEN" \
      https://api.upcloud.com/1.3/object-storage-2 | jq -r '.[] | select(.name=="idol-canvas-objsto") | .endpoints[0].domain_name')" \
  --from-literal="S3_BUCKET=idol-canvas" \
  --from-literal="S3_ACCESS_KEY_ID=<from step 3b>" \
  --from-literal="S3_SECRET_ACCESS_KEY=<from step 3b>" \
  --from-literal="REGISTRATION_APPROVAL_REQUIRED=true" \
  --from-literal="SESSION_SECRET=<64 hex>"
```

3b — mint the first app access key (fresh user has zero keys, so the create
succeeds; verify before wiring):

```bash
osuuid="$(curl -sS -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  https://api.upcloud.com/1.3/object-storage-2 | jq -r '.[] | select(.name=="idol-canvas-objsto") | .uuid')"
curl -sS -X POST -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  "https://api.upcloud.com/1.3/object-storage-2/$osuuid/users/app/access-keys"
```

CNPG platform (order matters):

```bash
task cnpg:operator:install   # digest-pinned operator manifest -> cnpg-system
task cnpg:cluster:secret     # cnpg-pg-superuser (openssl rand, once)
task cnpg:backup:secret      # cnpg-backup-s3 <- live app-env S3 values
task cnpg:cluster:install    # Cluster idol-db, 3 replicas, idol-pg-data 5Gi
kubectl -n idol get pods -l cnpg.io/cluster=idol-db   # 3/3 Running
```

initdb creates database `idol-canvas` owned by `app` and writes the generated
password into secret `cnpg-app-creds` — that secret is the source of truth for
the DATABASE_URL the first deploy synthesizes.

## Stage 3 — deploy (images, release, LB, DNS)

```bash
gh workflow run deploy.yml --ref main    # or merge any chart/script path
gh run watch                             # 30m budget inside the job
```

What `task deploy:k8s` does, in order: consume digest pins from
`images.lock.json` (refuses empty digests), fetch the kubeconfig from the
UpCloud API, refresh-only tofu settle, API-verify the objsto endpoint, S3
verify-or-rotate (probe pod env → secret → signed ListObjects; rotates only on
403), synthesize `app-env` (CNPG path reads `cnpg-app-creds`, ignoring the
placeholder), provision `upcloud-autoscaler`, then
`helm upgrade --install idol chart/ --atomic --wait --timeout 30m --take-ownership`
with the digest pins, and finally resolve the CCM LB hostname and run
`scripts/sync-dns.sh <ipv4> [ipv6]` (apex + www, `proxied=false`).

## Stage 4 — admin account + reseed

```bash
./scripts/bootstrap-admin.sh             # registers+promotes `vreid`, idempotent
./scripts/account-password.sh            # print admin creds (secret admin-account)
```

Reseed (the in-cluster Job; idempotent by `RESEED_TAG`, resume-safe):

```bash
# edit deploy/reseed-job.yaml: SOURCE_SHA (pinned commit) + reseed-admin Secret
kubectl apply -f deploy/reseed-job.yaml
kubectl -n idol logs -f job/reseed       # one line per chunk
kubectl -n idol delete job reseed        # when done
```

Defaults: 100k placements, chunk 2500, radius 100000, prewarm on. For a smaller
proof:
`SEED_COUNT=... ./test-assets/seed.sh https://idol-appreciation.club USER PASS`
(classic 16-image layout without `SEED_COUNT`; the seeder flags its fan-out
accounts cooldown-exempt via the admin endpoint before placing — the `lt163-*`
loadtest accounts were the same mechanism at scale). The 202 ingest path needs
`backend.worker` pods up (chart default 2) — the Job drives
`backend-worker import-batch` in-cluster.

## Verify gates (in order)

```bash
# 1. fleet
kubectl -n idol get pods                    # backend 2/2, worker 2/2, web 6/6, cnpg 3/3
# 2. health + data API
curl -fsS https://idol-appreciation.club/api/health        # {"ok":true}
curl -fsS 'https://idol-appreciation.club/api/center'      # center holder exists after seed
# 3. gray-fraction spot check: tiles around 0/0 must be non-empty webp
for t in 16/32768/32768 20/134217728/134217728 0/0/0; do
  curl -fsS "https://idol-appreciation.club/api/tiles/$t.webp" | file -    # WebP, non-empty
done
# 4. LB plan (the deploy re-asserts production-small every upgrade)
curl -sS -H "Authorization: Bearer $UPCLOUD_TOKEN" \
  https://api.upcloud.com/1.3/load-balancer | jq -r '.[] | [.name,.plan] | @tsv'
#    expected: <cluster-uuid>-idol-web  production-small
# 5. TLS + www (fresh caddy-acme prefix -> exactly 2 fresh LE certs, budget 5/168h)
curl -fsSI https://idol-appreciation.club | head -1
curl -fsSI https://www.idol-appreciation.club | head -1
# 6. DNS
dig +short idol-appreciation.club A | grep -qE '^[0-9.]+$'
```

## Known landmines

- **network_cidr drift (SEV 2026-10-15).** The config default must equal the
  CIDR a fresh build creates (today `10.30.0.0/24`). Drift is invisible to
  refresh-only steps and reads as "forces replacement" on the network, cascading
  cluster + node-group replacement. Adopt-live-state discipline: any
  hand-touched live value gets adopted into the config (or explicitly reset by
  `infra-recreate`), never left to a converge apply.
- **S3 key-create 400 (ticket `01M4ZPQ9YWM01GCRKM42YN9DKM`).** Reissuing an
  access key can 400 `ERROR_INVALID_REQUEST` when the two-key quota is full of
  un-deletable ghost keys. `deploy:k8s` fails loudly with the verbatim body and
  leaves the working key intact. Workaround: mint the key via the UpCloud
  console (or delete ghost keys once the ticket answers), drop it into `app-env`
  with `kubectl create secret ... --dry-run=client -o yaml | apply`, and re-run
  deploy — verify-or-rotate keeps a working key forever.
- **Let's Encrypt budget (5 duplicates/168h).** A fresh `caddy-acme/` prefix
  issues 2 certs (apex + www, served directly). Every rollback that wipes the
  prefix re-burns them; never delete the prefix to "fix" TLS — 5 failures locks
  the domain out for a week.
- **Fresh-pod ClusterIP blackhole (Cilium, 2026-10-15).** After heavy node
  churn, new pods can hang on ClusterIP while pod-IP-direct answers. It
  self-converged within hours; the scratch-vet workflow diffs endpointSlices vs
  live pods and deletes stale slices.
- **RWO Multi-Attach.** CNPG PVCs are RWO with required per-node anti-affinity:
  one instance per node. A rogue second pod mounting the same PVC (or a
  reschedule onto a node still holding the old pod) dies with Multi-Attach
  errors — let CNPG/its PDB manage the instances, never hand-run a second mount.
- **30m job timeouts vs server-side ops.** UpCloud operations outlive a killed
  GitHub Actions job (the 08:55 aborted destroy left terminating VMs and a stuck
  open op; infra-apply now runs 90m). If a destroy/apply is killed:
  capacity-first recovery — create replacement capacity ALONGSIDE the stuck
  resource, restore service, then reap the corpse via follow-up API reads
  (404/absent), never assume. Also: the helm wait budget (30m) must exceed cold
  CCM LB provisioning (15–20m) or `--atomic` uninstalls the release and churns
  the half-built LB.
- **State restore.** State lives only in the `tofu-state` bucket. If it is lost
  while resources exist, rebuild it by import (IDs: network/cluster/ objsto =
  UUIDs; node group = `{cluster}/default-new`; bucket/user/policy/ user_policy =
  `{objsto_uuid}/{name|username}`; access keys are NOT tracked). Then
  `tofu apply -refresh-only` and expect 0 add / 0 destroy.
- **CCM LB outlives the cluster.** Cluster deletion does not delete the
  Service-provisioned LB; sweep `/1.3/load-balancer` by the `-idol-` suffix
  after any destroy (its network interface also blocks network deletion with
  `NETWORK_NOT_EMPTY` for ~2 minutes).

## Teardown (the inverse, zero remnants)

1. In-cluster first, if the cluster is reachable: `helm uninstall idol -n idol`
   (CNPG Cluster CR goes with the release; wait for PVCs to delete —
   `idol-pg-data` is Delete-reclaim), then delete `cnpg-system`.
2. `gh workflow run infra-destroy.yml -f confirm=destroy` (tofu destroy; LB
   cleanup runs deferred even on failure).
3. API sweep, verifying each with follow-up reads (never assume): load balancers
   (Service-UID lineage), storages (node disks + any detached `pvc-*`), BOTH
   object-storage services incl. bucket contents, servers, networks,
   floating/public IPs, node groups. The state backend (`tofu-state-objsto`) is
   out-of-band and dies last — with it, the state. `kubectl` contexts die with
   the cluster; `.env`/local kubeconfigs stay on disk (tokens are account-level,
   not cluster-level).
