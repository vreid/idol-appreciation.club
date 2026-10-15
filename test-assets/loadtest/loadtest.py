#!/usr/bin/env python3
"""Phase 1 load-test verdict harness (docs/scaling-architecture.md § Phase 1).

STAGED — do NOT run until pipeline3's Phase 3 flip receipt lands: the
queue-based render pipeline is what 50 place/s must flow through, and the
doc's target numbers are only meaningful against it. Coordinate with
pipeline3; the flip receipt is the gate.

Target (#163): 5,000 concurrent SSE viewers · 50 place/s sustained ·
200/s burst · verdict table = SSE held, place/s achieved, place p95,
tile GET p95, origin tile req/s vs viewer count (Phase 2's gate metric),
errors, node/pod resource peaks (kubectl top sampled out-of-band).

Design:
  - SSE storm: asyncio + raw httpx/anyio-style GETs held open against
    /api/events (no bbox = everything, like today's clients). 5,000 is
    file-descriptor bound: raise ulimit -n (>= 8192) before running.
    Connections report dropped/reconnected counts — "SSE held" = the
    still-open count at the end of each phase.
  - Placement generator: separate asyncio pool on cooldown-exempt seed
    accounts (SEED_ACCOUNTS_FILE format, see seed.sh). Steady phase
    targets 50 place/s for 120s; burst phase 200/s for 30s; then the
    placement p95 is computed from per-call latencies.
  - Tile watcher: a small sample of viewers periodically GETs a random
    low-z tile (z0/z4) with ?v=<held> — the origin tile req/s vs viewer
    count is Phase 2's gate metric (immutable hits should never reach
    the origin).
  - Resource peaks: kubectl top nodes/pods sampled every 10s by the
    wrapper (loadtest-wrapper.sh), reported alongside.

Usage:
  ulimit -n 16384
  python3 loadtest.py https://idol-appreciation.club \
      --accounts fanout.json --viewers 5000 --steady 50 --burst 200
"""

import argparse
import asyncio
import json
import random
import statistics
import ssl
import time

import aiohttp


def accounts(path):
    with open(path) as f:
        return json.load(f)


async def sse_viewer(base, jar, stats, stop, watcher_tiles):
    """Hold one /api/events connection open; report drops."""
    timeout = aiohttp.ClientTimeout(total=None, sock_read=30)
    try:
        async with aiohttp.ClientSession(timeout=timeout, cookie_jar=jar) as s:
            async with s.get(f"{base}/api/events") as r:
                if r.status != 200:
                    stats["sse_failed"] += 1
                    return
                stats["sse_held"] += 1
                async for _ in r.content:
                    if stop.is_set():
                        break
                    stats["sse_events"] += 1
    except Exception:
        stats["sse_dropped"] += 1
    finally:
        stats["sse_held"] = max(0, stats["sse_held"] - 1)


async def tile_watcher(base, stats, stop, every=6.0):
    """Phase 2 gate metric: immutable-versioned tile GETs from a viewer."""
    timeout = aiohttp.ClientTimeout(total=15)
    async with aiohttp.ClientSession(timeout=timeout) as s:
        while not stop.is_set():
            z, x, y = random.choice([(0, 0, 0), (4, 8, 8), (9, 1, 1), (18, 512, 512)])
            t0 = time.perf_counter()
            try:
                async with s.get(f"{base}/api/tiles/{z}/{x}/{y}.webp") as r:
                    await r.read()
                    dt = time.perf_counter() - t0
                    stats["tile_lat"].append(dt)
                    stats["tile_codes"][r.status] = stats["tile_codes"].get(r.status, 0) + 1
            except Exception:
                stats["tile_errors"] += 1
            await asyncio.sleep(every)


async def placer(base, acct, art, coords, stats, stop, target_per_s, worker_n):
    """Place in a loop, pacing this worker to target/worker_n per second."""
    interval = worker_n / target_per_s
    jar = aiohttp.CookieJar(quote_cookie=False)
    timeout = aiohttp.ClientTimeout(total=60)
    async with aiohttp.ClientSession(timeout=timeout, cookie_jar=jar) as s:
        login = await s.post(
            f"{base}/api/auth/login", json={"username": acct["username"], "password": acct["password"]}
        )
        if login.status != 200:
            stats["placer_login_failed"] += 1
            return
        i = 0
        while not stop.is_set():
            t0 = time.perf_counter()
            x, y = coords[i % len(coords)]
            i += 1
            data = aiohttp.FormData()
            data.add_field("image", art, filename="a.png", content_type="image/png")
            data.add_field("x", str(x))
            data.add_field("y", str(y))
            data.add_field("size", "16")
            data.add_field("rot", "0")
            try:
                async with s.post(f"{base}/api/place", data=data) as r:
                    await r.read()
                    dt = time.perf_counter() - t0
                    stats["place_codes"][r.status] = stats["place_codes"].get(r.status, 0) + 1
                    if r.status == 201:
                        stats["place_lat"].append(dt)
            except Exception:
                stats["place_errors"] += 1
            wait = interval - (time.perf_counter() - t0)
            if wait > 0:
                await asyncio.sleep(wait)


def pct(vals, p):
    if not vals:
        return float("nan")
    vals = sorted(vals)
    return vals[min(len(vals) - 1, int(len(vals) * p))]


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("base")
    ap.add_argument("--accounts", required=True)
    ap.add_argument("--art", default="art.png", help="16px PNG placed by the generator")
    ap.add_argument("--viewers", type=int, default=5000)
    ap.add_argument("--placers", type=int, default=25)
    ap.add_argument("--steady", type=int, default=50, help="sustained place/s")
    ap.add_argument("--burst", type=int, default=200, help="burst place/s")
    ap.add_argument("--steady-seconds", type=int, default=120)
    ap.add_argument("--burst-seconds", type=int, default=30)
    ap.add_argument("--radius", type=int, default=20000)
    args = ap.parse_args()

    accs = accounts(args.accounts)
    try:
        art = open(args.art, "rb").read()
    except FileNotFoundError:
        # Generate the 16px art so the harness is self-contained.
        import struct, zlib
        raw = b"".join(b"\x00" + bytes([120, 40, 200] * 16) for _ in range(16))
        def ch(t, d):
            return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
        art = (b"\x89PNG\r\n\x1a\n" + ch(b"IHDR", struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0))
               + ch(b"IDAT", zlib.compress(raw)) + ch(b"IEND", b""))
        open(args.art, "wb").write(art)
    CX = 134217728
    coords = [
        (CX + random.randint(-args.radius, args.radius), CX + random.randint(-args.radius, args.radius))
        for _ in range(2000)
    ]

    stats = {
        "sse_held": 0,
        "sse_failed": 0,
        "sse_dropped": 0,
        "sse_events": 0,
        "place_lat": [],
        "place_codes": {},
        "place_errors": 0,
        "placer_login_failed": 0,
        "tile_lat": [],
        "tile_codes": {},
        "tile_errors": 0,
    }
    stop = asyncio.Event()

    # Phase 0: ramp viewers
    jars = [aiohttp.CookieJar(quote_cookie=False) for _ in range(args.viewers)]
    viewers = [
        asyncio.create_task(sse_viewer(args.base, j, stats, stop, None)) for j in jars
    ]
    watchers = [asyncio.create_task(tile_watcher(args.base, stats, stop)) for _ in range(4)]
    ramp_deadline = time.monotonic() + 120
    while time.monotonic() < ramp_deadline and stats["sse_held"] < args.viewers * 0.95:
        await asyncio.sleep(5)
    print(f"[ramp] held={stats['sse_held']} failed={stats['sse_failed']}")

    # Phase 1: steady
    per = args.steady / max(1, args.placers)
    placers = [
        asyncio.create_task(
            placer(args.base, accs[i % len(accs)], art, coords, stats, stop, args.steady, args.placers)
        )
        for i in range(args.placers)
    ]
    n0 = len(stats["place_lat"])
    await asyncio.sleep(args.steady_seconds)
    steady_n = len(stats["place_lat"]) - n0
    print(
        f"[steady] target={args.steady}/s achieved={steady_n / args.steady_seconds:.1f}/s "
        f"p50={pct(stats['place_lat'], 0.5):.2f}s p95={pct(stats['place_lat'], 0.95):.2f}s "
        f"codes={stats['place_codes']}"
    )

    # Phase 2: burst
    for t in placers:
        t.cancel()
    per = args.burst / max(1, args.placers)
    placers = [
        asyncio.create_task(
            placer(args.base, accs[i % len(accs)], art, coords, stats, stop, args.burst, args.placers)
        )
        for i in range(args.placers)
    ]
    n0 = len(stats["place_lat"])
    await asyncio.sleep(args.burst_seconds)
    burst_n = len(stats["place_lat"]) - n0
    print(f"[burst] target={args.burst}/s achieved={burst_n / args.burst_seconds:.1f}/s codes={stats['place_codes']}")

    stop.set()
    for t in placers + viewers + watchers:
        t.cancel()

    print("== verdict table ==")
    print(f"sse_held={stats['sse_held']} dropped={stats['sse_dropped']} failed={stats['sse_failed']} "
          f"events={stats['sse_events']}")
    print(f"place_p50={pct(stats['place_lat'], 0.5):.2f}s place_p95={pct(stats['place_lat'], 0.95):.2f}s "
          f"errors={stats['place_errors']}")
    print(f"tile_p50={pct(stats['tile_lat'], 0.5):.3f}s tile_p95={pct(stats['tile_lat'], 0.95):.3f}s "
          f"codes={stats['tile_codes']} errors={stats['tile_errors']}")


if __name__ == "__main__":
    asyncio.run(main())
