# Provider and OpenTofu version pins.
#
# UpCloud's provider lives under the `UpCloudLtd` namespace on the OpenTofu
# registry (https://registry.opentofu.org/v1/providers/upcloudltd/upcloud);
# there is no `UpCloudProvider/upcloud`.

terraform {
  required_version = "~> 1.13"

  # State lives in the S3 backend on Managed Object Storage (docs/infra.md
  # "State handling"). The endpoint and the bucket credentials are injected
  # per-run via `tofu init -backend-config=...` from repo secrets (see the
  # tofu-setup composite); the bucket itself is provisioned OUTSIDE this
  # configuration so state management never depends on managed resources.
  # Stateless runs (`-backend=false`, PR gates) never touch any of this.
  backend "s3" {
    bucket                      = "tofu-state"
    key                         = "idol-appreciation.club"
    region                      = "europe-1"
    skip_credentials_validation = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
    force_path_style            = true
  }

  required_providers {
    upcloud = {
      source  = "UpCloudLtd/upcloud"
      version = "5.47.0"
    }
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "5.27.0"
    }
  }
}

provider "upcloud" {
  # Auth: UPCLOUD_TOKEN environment variable (bearer token).
  # Never put the token in code or state-adjacent files.
}

provider "cloudflare" {
  # Auth: CLOUDFLARE_API_TOKEN environment variable.
}
