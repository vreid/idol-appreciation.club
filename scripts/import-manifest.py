#!/usr/bin/env python3
"""Deterministic scrapbook-pile manifest generator for POST /api/admin/import.

Reuses the seed.sh pile rng discipline exactly: per entry k, the file is
`FILES[1 + (k % (N_FILES - 1))]` (never the first idol), and the geometry
comes from `random.Random(f"pile:{k}:{basename}")` — uniform point in the
disc of RADIUS around (CX, CY), image centered on it, truncated-gaussian
rotation (sigma SIGMA, clamped ±15°, rounded to 2 decimals). Same (seed
discipline, k, file) ⇒ same coordinates, so a regenerated manifest resumes
byte-identically against a killed import.

Requires a preupload mapping (the response of POST
/api/admin/import/originals) extended with the source basename:
    [{"basename": "02-....jpg", "source_sha": ..., "derivative_sha": ...,
      "w": ..., "h": ...}, ...]

Usage:
  import-manifest.py --map map.json --offset 0 --count 10000 \
      --batch-id <uuid> [--seed-tag tag] [--radius 100000] \
      [--sigma 7.5] [--cx 134217728] [--cy 134217728] [--out manifest.json]

Prints a one-line summary to stderr; the manifest JSON goes to --out or stdout.
"""

import argparse
import json
import math
import random
import sys


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--map", required=True, help="preupload mapping JSON")
    ap.add_argument("--offset", type=int, default=0, help="first pile index k")
    ap.add_argument("--count", type=int, required=True, help="entries to emit")
    ap.add_argument("--batch-id", required=True)
    ap.add_argument("--seed-tag", default="", help="extra rng discriminator")
    ap.add_argument("--radius", type=int, default=100000)
    ap.add_argument("--sigma", type=float, default=7.5)
    ap.add_argument("--cx", type=int, default=134217728)
    ap.add_argument("--cy", type=int, default=134217728)
    ap.add_argument("--prewarm", action="store_true")
    ap.add_argument("--out")
    args = ap.parse_args()

    with open(args.map) as fh:
        mapping = json.load(fh)
    files = [m["basename"] for m in mapping]
    by_name = {m["basename"]: m for m in mapping}
    if len(files) < 2:
        sys.exit("need >=2 preuploaded images")
    n_files = len(files)

    entries = []
    for k in range(args.offset, args.offset + args.count):
        name = files[1 + (k % (n_files - 1))]
        meta = by_name[name]
        rng = random.Random(f"pile:{k}:{name}" + (f":{args.seed_tag}" if args.seed_tag else ""))
        r = args.radius * math.sqrt(rng.random())
        theta = 2 * math.pi * rng.random()
        x = int(args.cx + r * math.cos(theta) - meta["w"] / 2)
        y = int(args.cy + r * math.sin(theta) - meta["h"] / 2)
        rot = round(max(-15.0, min(15.0, rng.gauss(0.0, args.sigma))), 2)
        entries.append(
            {
                "source_sha": meta["source_sha"],
                "derivative_sha": meta["derivative_sha"],
                "x": x,
                "y": y,
                "w": meta["w"],
                "h": meta["h"],
                "rot": rot,
            }
        )

    manifest = {"batch_id": args.batch_id, "prewarm": args.prewarm, "entries": entries}
    payload = json.dumps(manifest, separators=(",", ":"))
    out = open(args.out, "w") if args.out else sys.stdout
    out.write(payload)
    if args.out:
        out.close()
    print(
        f"manifest: {len(entries)} entries, batch {args.batch_id}, "
        f"k {args.offset}..{args.offset + args.count - 1}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
