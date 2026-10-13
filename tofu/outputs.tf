# Outputs consumed by workflows (deploy.yml) and humans.
# Secrets are marked sensitive; deploy.yml reads them with -raw.

output "cluster_name" {
  description = "Managed Kubernetes cluster name."
  value       = upcloud_kubernetes_cluster.this.name
}

output "cluster_id" {
  description = "Managed Kubernetes cluster UUID (for the kubeconfig API)."
  value       = upcloud_kubernetes_cluster.this.id
}

output "cluster_zone" {
  description = "UpCloud zone of the cluster."
  value       = upcloud_kubernetes_cluster.this.zone
}

output "node_plan" {
  description = "Worker node plan actually in use."
  value       = upcloud_kubernetes_node_group.this.plan
}

output "node_count" {
  description = "Worker node count."
  value       = upcloud_kubernetes_node_group.this.node_count
}

output "pg_host" {
  description = "Managed PostgreSQL primary host."
  value       = upcloud_managed_database_postgresql.this.service_host
}

output "pg_port" {
  description = "Managed PostgreSQL port."
  value       = upcloud_managed_database_postgresql.this.service_port
}

output "pg_admin_uri" {
  description = "Admin connection URI (root user, keep private)."
  value       = upcloud_managed_database_postgresql.this.service_uri
  sensitive   = true
}

output "app_database_uri" {
  description = "Application DATABASE_URL (app user on the app database)."
  value = format(
    "postgres://%s:%s@%s:%s/%s?sslmode=require",
    upcloud_managed_database_user.app.username,
    upcloud_managed_database_user.app.password,
    upcloud_managed_database_postgresql.this.service_host,
    upcloud_managed_database_postgresql.this.service_port,
    upcloud_managed_database_logical_database.app.name,
  )
  sensitive = true
}

output "s3_endpoint" {
  description = "Public S3 endpoint of the object storage service."
  value = format(
    "https://%s",
    [
      for e in upcloud_managed_object_storage.this.endpoint : e.domain_name
      if e.type == "public"
    ][0],
  )
}

output "s3_bucket" {
  description = "Canvas bucket name."
  value       = upcloud_managed_object_storage_bucket.canvas.name
}

output "s3_access_key_id" {
  description = "Application S3 access key id."
  value       = upcloud_managed_object_storage_user_access_key.s3.access_key_id
  sensitive   = true
}

output "s3_secret_access_key" {
  description = "Application S3 secret access key."
  value       = upcloud_managed_object_storage_user_access_key.s3.secret_access_key
  sensitive   = true
}

output "cloudflare_zone_id" {
  description = "Cloudflare zone id (consumed by scripts/sync-dns.sh)."
  value       = data.cloudflare_zone.apex.id
}

output "cloudflare_zone_name" {
  description = "Cloudflare zone name."
  value       = var.domain
}
