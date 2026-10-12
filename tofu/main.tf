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

resource "upcloud_kubernetes_node_group" "this" {
  cluster    = upcloud_kubernetes_cluster.this.id
  name       = var.node_group_name
  plan       = var.node_plan
  node_count = var.node_count

  labels = {
    project    = "idol-appreciation.club"
    managed-by = "tofu"
  }

  utility_network_access = true
}

# ----------------------------------------------------------------- postgres

resource "upcloud_managed_database_postgresql" "this" {
  name  = var.pg_name
  zone  = var.zone
  title = "idol-appreciation.club PostgreSQL"

  # plan is deprecated for PostgreSQL; plan_compute takes the new
  # rdb.* shape names. Smallest development shape = 1 vCPU / 1 GiB,
  # single node, 10 GiB storage, "mini" backup tier.
  plan_compute     = var.pg_plan_compute
  plan_node_count  = var.pg_node_count
  plan_storage_gib = var.pg_storage_gib
  plan_backups     = var.pg_plan_backups

  labels = {
    project    = "idol-appreciation.club"
    managed-by = "tofu"
  }
}

resource "upcloud_managed_database_logical_database" "app" {
  service = upcloud_managed_database_postgresql.this.id
  name    = var.pg_database
}

resource "upcloud_managed_database_user" "app" {
  service  = upcloud_managed_database_postgresql.this.id
  username = var.pg_user

  # password is omitted on purpose: the provider generates one and exposes
  # it read-only, so it never appears in configuration.
}

# ---------------------------------------------------- object storage (S3)

# NOTE: bucket capacity follows the account-level Managed Object Storage
# subscription (250 GB tier); the API does not expose a size attribute.

resource "upcloud_managed_object_storage" "this" {
  name              = var.objsto_name
  region            = var.objsto_region
  configured_status = "started"

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

resource "upcloud_managed_object_storage_user_access_key" "app" {
  service_uuid = upcloud_managed_object_storage.this.id
  username     = upcloud_managed_object_storage_user.app.username
  status       = "Active"
}

# -------------------------------------------------------------- cloudflare

# Read-only lookup; records themselves are synced by scripts/sync-dns.sh so
# that destroy/recreate cycles never fight a stale LB IP in state.
data "cloudflare_zone" "apex" {
  filter = {
    name = var.domain
  }
}
