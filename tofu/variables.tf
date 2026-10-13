# Knobs for the idol-appreciation.club stack.
# Defaults reflect the cheapest viable configuration in fi-hel2.

variable "zone" {
  description = "UpCloud zone (legacy id format, e.g. fi-hel2). Helsinki #2 per requirement."
  type        = string
  default     = "fi-hel2"
}

variable "cluster_name" {
  description = "Managed Kubernetes cluster name."
  type        = string
  default     = "idol-canvas"
}

variable "k8s_control_plane_plan" {
  description = <<-EOT
    Kubernetes control plane plan. `dev-md` is the current Development tier
    (single-node control plane, max 30 worker nodes); `development` is the
    deprecated predecessor.
  EOT
  type        = string
  default     = "dev-md"
}

variable "k8s_version" {
  description = "Kubernetes version (empty = provider/account default)."
  type        = string
  default     = ""
}

variable "network_cidr" {
  description = "Private network CIDR for cluster nodes and the control plane."
  type        = string
  default     = "10.10.0.0/24"
}

variable "node_group_name" {
  description = "Worker node group name."
  type        = string
  default     = "default"
}

variable "node_plan" {
  description = <<-EOT
    Worker node plan. `STARTER-4xCPU-8GB` (4 vCPU / 8 GiB, ~€14/mo) per the
    approved vertical bump from STARTER-2xCPU-4GB; Starter plans are valid
    for worker nodes.
  EOT
  type        = string
  default     = "STARTER-4xCPU-8GB"
}

variable "node_count" {
  description = <<-EOT
    Worker node count — the autoscaling FLOOR (budget guardrail: 1).
    UpCloud node groups have no min/max autoscaling fields (provider
    v5.47.0 / API 1.3: a fixed count only); while the Cluster Autoscaler
    is disabled this is the exact count, once enabled CA scales between
    this floor and 2 (chart/ values clusterAutoscaler.maxSize).
  EOT
  type        = number
  default     = 1
}

variable "pg_name" {
  description = "Managed PostgreSQL service name."
  type        = string
  default     = "idol-canvas-db"
}

variable "pg_plan_compute" {
  description = <<-EOT
    PostgreSQL compute shape. `rdb.development.1CPU-1GB` is the smallest
    managed-DB shape (1 vCPU / 1 GiB, single node).
  EOT
  type        = string
  default     = "rdb.development.1CPU-1GB"
}

variable "pg_database" {
  description = "Application logical database name."
  type        = string
  default     = "app"
}

variable "pg_user" {
  description = "Application database user name."
  type        = string
  default     = "app"
}

variable "pg_node_count" {
  description = "PostgreSQL node count (1 = development shapes only)."
  type        = number
  default     = 1
}

variable "pg_storage_gib" {
  description = "PostgreSQL base storage in GiB."
  type        = number
  default     = 10
}

variable "pg_plan_backups" {
  description = "PostgreSQL backup tier (mini is the smallest)."
  type        = string
  default     = "mini"
}

variable "objsto_name" {
  description = "Managed Object Storage (S3) service name."
  type        = string
  default     = "idol-canvas-objsto"
}

variable "objsto_region" {
  description = <<-EOT
    Managed Object Storage region. `europe-1` is the Helsinki region
    (primary zone fi-hel2) — the object storage API uses logical region
    names, not zone ids.
  EOT
  type        = string
  default     = "europe-1"
}

variable "bucket_name" {
  description = "S3 bucket holding canvas image blobs."
  type        = string
  default     = "idol-canvas"
}

variable "s3_user" {
  description = "Object storage user for the application."
  type        = string
  default     = "app"
}

variable "domain" {
  description = "Apex domain served by the stack."
  type        = string
  default     = "idol-appreciation.club"
}
