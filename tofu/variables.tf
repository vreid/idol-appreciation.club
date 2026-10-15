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
  # 2026-10-15 SEV: the live stack runs 10.30.0.0/24 while this default
  # said 10.10.0.0/24 — drift invisible to refresh-only steps, but the
  # first tofu-touching merge (#203) fired infra-apply, whose plan read
  # "network address forces replacement", cascading cluster + default
  # node group replacement. Adopt the live CIDR so a converge apply is
  # purely additive; infra-recreate continues to own any true rebuild.
  default = "10.30.0.0/24"
}

variable "node_group_name" {
  description = "Worker node group name."
  type        = string
  default     = "default"
}

variable "node_plan" {
  description = <<-EOT
    Worker node plan. `STARTER-4xCPU-8GB` (4 vCPU / 8 GiB, €20/mo, ~€0.03/h)
    per the approved vertical bump from STARTER-2xCPU-4GB; Starter plans are
    valid for worker nodes.
  EOT
  type        = string
  default     = "STARTER-4xCPU-8GB"
}

variable "node_count" {
  description = <<-EOT
    Worker node count — the autoscaling FLOOR (budget guardrail: 3).
    Raised 1→3 for the CloudNativePG migration: the CNPG Cluster runs
    3 replicas with required pod anti-affinity across nodes, so fewer
    than 3 workers cannot host the database. UpCloud node groups have
    no min/max autoscaling fields (provider v5.47.0 / API 1.3: a fixed
    count only); while the Cluster Autoscaler is disabled this is the
    exact count, once enabled CA scales between this floor and 4
    (chart/ values clusterAutoscaler.maxSize).
  EOT
  type        = number
  default     = 3
}

variable "baseline_node_group_name" {
  description = <<-EOT
    Baseline pool node group name. `default`: the canonical single-group
    name the cluster autoscaler bounds target (chart values nodeGroup)
    and the name docs/scaling-architecture.md documents. It has been
    free since the SEV consolidation deleted the stuck `default` group
    (#217), so the gated retune creates the replacement under the final
    name directly — no rename (a rename is a replacement, i.e. another
    node dance) after cutover.
  EOT
  type        = string
  default     = "default"
}

variable "baseline_node_plan" {
  description = <<-EOT
    Baseline pool node plan: 2 vCPU / 4 GiB per docs/scaling-architecture.md
    "Cluster layout". This is the plan the 4 vCPU / 8 GB `default` group
    shrinks to in the gated phase 5 retune (`task infra:retune-baseline`);
    STARTER-2xCPU-4GB was this group's plan before the approved vertical
    bump, so it is a known-valid k8s node plan.
  EOT
  type        = string
  default     = "STARTER-2xCPU-4GB"
}

variable "baseline_node_count" {
  description = <<-EOT
    Baseline pool node count. Three: one per CNPG instance (required pod
    anti-affinity). Fixed — the Cluster Autoscaler does not manage this
    group; do not wire it into chart/ clusterAutoscaler bounds.
  EOT
  type        = number
  default     = 3
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
