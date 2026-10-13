# Support ticket draft — Managed Object Storage (S3) usage metering shows 0

**Status:** DRAFT for the user to paste into an UpCloud support ticket.
**Prepared:** 2026-10-13 ~11:15 UTC by infra automation.

## Summary

The UpCloud console (and the API `usage` field) report **0 objects / 0 B** for
our Managed Object Storage service, while the bucket provably contains **2503
objects / 103,888,524 bytes** as of 11:10 UTC on 2026-10-13 (verified with
SigV4-signed `ListObjectsV2` + `GetObject` against the service's own S3
endpoint). Metering has read 0 since the console was first checked ~09:00 UTC,
while objects were first written ~01:43 UTC.

## Instance

- Service: Managed Object Storage (object-storage-2), endpoint
  `https://nrsia.upcloudobjects.com`
- Name: `idol-canvas-objsto`
- UUID: `12bef925-7350-4c48-ad30-e94e141d540b`
- Created: 2026-10-12T22:52:24Z
- Bucket: `idol-canvas` (created via the S3 API shortly after instance creation)
- Plan: 250 GB

## Evidence of actual usage (S3 ground truth)

Probe at 2026-10-13 ~11:10 UTC, SigV4-signed requests against
`nrsia.upcloudobjects.com`, user `app`:

- `ListObjectsV2` (paginated): **2503 objects** across 3 pages (1000 + 1000 +
  503), summed `Size` = **103,888,524 bytes (~99.1 MiB)**
- `GetObject` `chunks/0/0.webp` (object size 2266 B), `Range: bytes=0-255`: HTTP
  206, 256 B read — objects are readable
- Writes have been ongoing since ~01:43 UTC by our application (Rust backend
  writing image tiles continuously; the bucket receives regular PUTs of
  `chunks/…` keys)

## What UpCloud reports

- Console: 0 B used / 0 objects (checked ~09:00, ~10:30 and ~11:05 UTC)
- `GET /1.3/object-storage-2/12bef925-7350-4c48-ad30-e94e141d540b`:
  `"usage": { "total_objects": 0, "total_size_bytes": 0 }` (`total_objects` and
  `total_size_bytes` are the only usage fields exposed by the object-storage-2
  detail endpoint; no metrics sub-endpoint exists)

## Request

Please investigate the usage metering for this instance. The data is present and
readable through the S3 API; the console/API usage counters have been stuck at 0
since creation. If billing is driven by the same counters we would like this
corrected before the month closes.

## Secondary observation — RESOLVED (kept for the record)

At 2026-10-13T11:01:57Z the `app` user's access key record (same key ID
`AKIAEDFF47402BE801E6`) was regenerated server-side — no API call from our side
between 10:57:36 UTC and 11:08 UTC — so in-flight credentials began failing
`SignatureDoesNotMatch`. We minted a temporary key at ~11:08 UTC to restore
writes, then converged everything onto the platform-generated secret: OpenTofu
state now owns the current key/secret pair, the app's k8s secret `app-env`
matches it, and the temporary key is gone. Net result: exactly one key ID
(`AKIAEDFF47402BE801E6`), fully managed by our infrastructure code. We would
still appreciate confirmation of what touched the key at 11:01:57Z.
