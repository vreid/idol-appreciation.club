# idol-appreciation.club infrastructure.
#
# All resources land in UpCloud zone fi-hel2 (Helsinki #2):
#   - Managed Kubernetes (Development tier control plane) + 1 worker node
#   - Managed PostgreSQL, smallest development shape
#   - Managed Object Storage (S3-compatible) + bucket + app credentials
#
# DNS records are deliberately NOT managed here; see scripts/sync-dns.sh
# and docs/infra.md for why.

# ---------------------------------------------------------------- kubernetes

resource "upcloud_network" "k8s" {
  name = "${var.cluster_name}-net"
  zone = var.zone

  ip_network {
    address = var.network_cidr
    dhcp    = true
    family  = "IPv4"
  }

  # The Managed Kubernetes service attaches its own router to this network;
  # keep tofu from reverting it.
  lifecycle {
    ignore_changes = [router]
  }
}

resource "upcloud_kubernetes_cluster" "this" {
  name = var.cluster_name
  zone = var.zone

  network = upcloud_network.k8s.id

  # Development tier control plane (single node, cheapest).
  plan = var.k8s_control_plane_plan

  version = var.k8s_version != "" ? var.k8s_version : null

  # Public control-plane API: CI runners and local kubectl need access and
  # GitHub-hosted runners have no stable egress ranges to allowlist.
  control_plane_ip_filter = ["0.0.0.0/0"]

  labels = {
    project    = "idol-appreciation.club"
    managed-by = "tofu"
  }
}


# Baseline pool (docs/scaling-architecture.md "Cluster layout", phase 5):
# 3 x 2 vCPU / 4 GB carrying PG, the API and the edge. Replaces the 4 vCPU
# / 8 GB `this` group — ONLY via the staged `task infra:retune-baseline`
# runbook (create -> drain -> destroy), never by in-place plan changes:
# UpCloud node-group plan updates roll every node in the group, which is
# exactly the vertical churn this stack forbids.
# GATED on the Phase 4 signal (API image without libvips): the 2 GiB
# libvips burst headroom no longer fits a 4 GB node beside PG.
resource "upcloud_kubernetes_node_group" "baseline" {
  cluster    = upcloud_kubernetes_cluster.this.id
  name       = var.baseline_node_group_name
  plan       = var.baseline_node_plan
  node_count = var.baseline_node_count

  # CNPG keeps one instance per node via required pod anti-affinity, so
  # three nodes is the floor; the Cluster Autoscaler must NOT touch this
  # group (its --nodes bounds cover only `default` and `workers`).
  lifecycle {
    ignore_changes = [node_count]
  }

  labels = {
    project    = "idol-appreciation.club"
    managed-by = "tofu"
    pool       = "baseline"
  }

  # New group, so no in-place node churn: this label is what the
  # descheduler's DefaultEvictor nodeSelector scopes the baseline pool
  # with (chart/ descheduler.* values).
  kubelet_args {
    key   = "node-labels"
    value = "pool=baseline"
  }

  utility_network_access = true
}

# ----------------------------------------------------------------- postgres
# RETIRED (2026-10-14 CNPG migration): the managed PostgreSQL service is
# replaced by an in-cluster CloudNativePG Cluster (chart/, cnpg.* values;
# operator + backup plugin via the cnpg:* tasks). Destroying it is a
# user-approved cost call — the data it held is disposable (the seeder
# re-creates the pile), durability now rides on Barman Cloud backups to
# the object storage below.

# ---------------------------------------------------- object storage (S3)

# NOTE: bucket capacity follows the account-level Managed Object Storage
# subscription (250 GB tier); the API does not expose a size attribute.

resource "upcloud_managed_object_storage" "this" {
  name              = var.objsto_name
  region            = var.objsto_region
  configured_status = "started"

  # Without an attached network the API exposes no S3 endpoint at all
  # (observed: endpoints = []); the public network provides the default
  # public S3 endpoint.
  network {
    family = "IPv4"
    name   = "public"
    type   = "public"
  }

  labels = {
    project    = "idol-appreciation.club"
    managed-by = "tofu"
  }
}

resource "upcloud_managed_object_storage_bucket" "canvas" {
  service_uuid = upcloud_managed_object_storage.this.id
  name         = var.bucket_name
}

resource "upcloud_managed_object_storage_user" "app" {
  service_uuid = upcloud_managed_object_storage.this.id
  username     = var.s3_user
}

# Application key, scoped to the canvas bucket only.
resource "upcloud_managed_object_storage_policy" "app" {
  service_uuid = upcloud_managed_object_storage.this.id
  name         = "${var.s3_user}-bucket-access"
  description  = "Full S3 access to the ${var.bucket_name} bucket"

  document = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "AppBucketFull"
        Effect = "Allow"
        Action = ["s3:*"]
        Resource = [
          "arn:aws:s3:::${var.bucket_name}",
          "arn:aws:s3:::${var.bucket_name}/*",
        ]
      },
    ]
  })
}

resource "upcloud_managed_object_storage_user_policy" "app" {
  service_uuid = upcloud_managed_object_storage.this.id
  username     = upcloud_managed_object_storage_user.app.username
  name         = upcloud_managed_object_storage_policy.app.name
}

# Address is "s3" (was "app"): renaming forces a one-time key rotation after
# the upstream key secret was regenerated out-of-band on 2026-10-13 while the
# stored secret stayed stale. Do not rename back; that would rotate again.
# RETIRED 2026-10-15 (SEV follow-up): the state-tracked access key no
# longer exists upstream, so every apply planned a reissue — and the
# create 400s ERROR_INVALID_REQUEST with the quota already empty
# (UpCloud ticket, correlation_id=01M4ZPQ9YWM01GCRKM42YN9DKM). The LIVE
# key held in the app-env secret works (caddy re-issued certs through
# the app bucket post-restore), so tofu stops tracking keys until the
# create-shape question is answered upstream. Re-add this resource (and
# the two secret outputs) with the ticket's answer; deploy's
# verify-or-rotate owns the live key meanwhile.
# resource "upcloud_managed_object_storage_user_access_key" "s3" {
#   service_uuid = upcloud_managed_object_storage.this.id
#   username     = upcloud_managed_object_storage_user.app.username
#   status       = "Active"
# }

# -------------------------------------------------------------- cloudflare

# Read-only lookup; records themselves are synced by scripts/sync-dns.sh so
# that destroy/recreate cycles never fight a stale LB IP in state.
data "cloudflare_zone" "apex" {
  filter = {
    name = var.domain
  }
}

# ------------------------------------------------------------------ SEV
# CAPACITY-FIRST restore (2026-10-15, Main-authorized): the 08:55
# aborted destroy left the `default` group's last two VMs terminating
# server-side (open operation, invisible to tofu state; PATCH count→3
# rejected HTTP 500). This group adds fresh baseline-shaped capacity
# ALONGSIDE the dying one so DB quorum and app pods land before any
# reaping — resource creation is not the gated retune; the drain/
# destroy of the stuck group is. `this` stays untouched; consolidating
# the two groups back to the documented baseline (and the name) is the
# phase-4-gated #206 retune's job, via its staged create→drain→destroy.
resource "upcloud_kubernetes_node_group" "baseline_capacity" {
  cluster    = upcloud_kubernetes_cluster.this.id
  name       = "default-new"
  plan       = var.node_plan
  node_count = var.node_count

  lifecycle {
    ignore_changes = [node_count]
  }

  labels = {
    project    = "idol-appreciation.club"
    managed-by = "tofu"
    pool       = "baseline-new"
  }

  utility_network_access = true
}
