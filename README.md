# idol-appreciation.club

An infinite, collaborative idol-image canvas: think OpenStreetMap tile pyramid
meets r/place. Users pan across a huge pannable/zoomable canvas and place images
on it, overwriting each other's tiles subject to a per-user cooldown. Backend is
a Rust service on UpCloud Managed Kubernetes (fi-hel-2), with managed
PostgreSQL, S3-compatible object storage, and a DNS-only Cloudflare zone in
front of a Kubernetes LoadBalancer.

- `backend/` — Rust API service
- `web/` — Vite + Svelte frontend
- `tofu/` — OpenTofu infrastructure (cluster, PostgreSQL, object storage)
- `deploy/` — Kubernetes manifests for the `idol` namespace
- `scripts/` — operational helpers (DNS sync, admin bootstrap, credential
  retrieval)
- `.github/workflows/` — CI, infra apply/destroy/recreate, deploy
- `docs/infra.md` — infrastructure runbook, costs, destroy/recreate guide
- `docs/dev.md` — development toolchain (mise + task), local CI gates
- `CONTRIBUTING.md` — conventional-commit PR titles, releases, image tags
