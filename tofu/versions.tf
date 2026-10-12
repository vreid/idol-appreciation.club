# Provider and OpenTofu version pins.
#
# UpCloud's provider lives under the `UpCloudLtd` namespace on the OpenTofu
# registry (https://registry.opentofu.org/v1/providers/upcloudltd/upcloud);
# there is no `UpCloudProvider/upcloud`.

terraform {
  required_version = "~> 1.13"

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
