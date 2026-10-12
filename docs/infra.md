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
| `ci`             | PR + push to main                         | tofu fmt/validate/plan (PRs), prettier markdown, backend cargo gates, web vp gates                           |
| `infra-apply`    | push to main touching `tofu/`, manual     | `tofu apply -auto-approve`                                                                                   |
| `infra-destroy`  | manual, input `confirm=destroy`           | `tofu destroy -auto-approve`                                                                                 |
| `infra-recreate` | manual                                    | destroy (best effort) then apply                                                                             |
| `deploy`         | push to main (backend/web/deploy), manual | cargo/vp gates, image build + push to GHCR, kubeconfig fetch, kubectl apply, secret patch, DNS sync, rollout |

Secrets live in the GitHub **environment `production`** (not repo-level):
`UPCLOUD_API_TOKEN`, `CLOUDFLARE_API_TOKEN`, `SESSION_SECRET`. Every job
touching them declares `environment: production`. `GITHUB_TOKEN` gets
`packages: write` for GHCR; no static kubeconfig exists anywhere.

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

### Rotating SESSION_SECRET

`gh secret set SESSION_SECRET --env production` (64 hex chars). The deploy
workflow pushes it into the `app-env` secret on the next run.

## Environment protection (later)

`production` currently has no protection rules — destroy must stay friction-free
for solo dev. When the team grows, add a gate on the dangerous paths only:

Settings → Environments → **production** → Required reviewers → add the
maintainer account. That gates `infra-destroy`/`infra-recreate`/`deploy` waits.
If that friction is unwanted for apply/deploy, move infra-destroy's job to a
dedicated environment `production-dangerous` with the reviewer rule, leaving
`production` free.

## Costs (UpCloud list prices, EUR, verified 2026-10)

| item                        | plan / shape               | price      |
| --------------------------- | -------------------------- | ---------- |
| k8s control plane           | `dev-md` (Development)     | €0/mo      |
| worker node (1)             | `STARTER-2xCPU-4GB`        | €12/mo     |
| managed PostgreSQL          | `rdb.development.1CPU-1GB` | €9/mo      |
| managed object storage      | 250 GB tier                | €5/mo      |
| load balancer (UpCloud CCM) | `essentials`               | €0/mo      |
| **total steady state**      |                            | **€26/mo** |

All hourly-billed; destroy drops the burn to zero. One destroy/recreate cycle
costs cents.

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
- Plan listing lives at `GET /1.3/plan` (singular).
