#!/usr/bin/env python3
"""Generate the 100 fictional-idol test images via the NanoGPT Image API.

Stdlib only (urllib, json, base64). Reads NANO_GPT_API_KEY from ../.env
relative to this script. Writes idols/NN-slug.jpg and manifest.json.

Usage: python3 generate.py [--only NN]
"""
import base64
import json
import os
import struct
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
API = "https://api.nano-gpt.com/api/v1/images"
MODEL = "bytedance/seedream-v5.0-pro"
RESOLUTION = "1k"  # supported tiers per endpoints metadata: aspect ratios + "1k"/"2k"
HERO_RESOLUTION = "2k"  # occasional hero shots
HERO_NUMBERS = {"30", "59", "64", "65"}
BUDGET_TARGET_USD = 9.5  # hard lifetime NanoGPT cap for this asset set
PER_IMAGE_EST_USD = 0.10
HERO_EST_USD = 0.35      # conservative pre-flight estimate for 2k requests
MIN_BYTES = 30 * 1024

TAIL = ("professional photography, editorial quality, sharp focus, "
        "high detail, fictional idol, AI-generated fictional character, "
        "no real person")

CATEGORIES = [
    ("01", "solo-female-closeup", "3:4",
     "Photocard-style close-up portrait of a young Korean woman fictional idol, "
     "soft wavy hair, flawless dewy idol makeup, gentle confident smile, soft "
     "pastel lavender studio background, beauty lighting, 85mm lens, " + TAIL),
    ("02", "solo-male-stage", "16:9",
     "Full-body shot of a male Korean fictional idol mid-performance on a huge "
     "concert stage, stylish stage costume, dramatic spotlight beams and haze, "
     "singing into a headset mic, cinematic concert photography, 85mm lens, " + TAIL),
    ("03", "group-five", "16:9",
     "Five-member coed K-pop group of fictional idols lined up shoulder to "
     "shoulder on a music-show stage, coordinated black-and-white stage "
     "outfits, colorful LED backdrop, group promo photography, 50mm lens, " + TAIL),
    ("04", "lightstick-ocean", "16:9",
     "Concert crowd seen from the stage point of view at night, thousands of "
     "glowing pastel lightsticks waving in the dark like an ocean, distant "
     "silhouette of a fictional idol performing, cinematic wide arena "
     "photography, 24mm lens, " + TAIL),
    ("05", "dance-break", "3:4",
     "Dynamic dance-break freeze frame of a fictional K-pop idol mid-move, "
     "clothes and hair in motion, sweat and intensity, dramatic arena lighting "
     "with lens flare, high-speed sports photography look, " + TAIL),
    ("06", "airport-fashion", "3:4",
     "Candid paparazzi-style photo of a stylish female fictional idol walking "
     "through an airport terminal pulling a suitcase, oversized designer "
     "streetwear, face mask pulled down to her chin, flash photography "
     "aesthetic, editorial street style, " + TAIL),
    ("07", "rehearsal", "16:9",
     "Practice-room mirror rehearsal shot of a fictional idol group in casual "
     "sweats, late-night warm lighting, water bottles and phones on the floor, "
     "candid documentary photography, 35mm lens, " + TAIL),
    ("08", "music-show-win", "16:9",
     "A fictional idol group celebrating an encore win on a music-show stage, "
     "holding a shiny trophy while golden confetti rains down, joyful tears "
     "and laughter, bright stage lights, celebratory editorial photography, " + TAIL),
    ("09", "dark-concept", "3:4",
     "Cinematic dark-concept teaser shot of a fictional idol, moody black set "
     "with deep shadows and a single dramatic rim light, high-fashion avant "
     "garde styling, smoldering gaze, film-noir editorial photography, " + TAIL),
    ("10", "photobook-nature", "3:4",
     "Photobook concept photo of a female fictional idol standing in a "
     "wildflower field at golden hour, flowing white summer dress, warm "
     "backlit sun flare, dreamy soft focus background, editorial fashion "
     "photography, 85mm lens, " + TAIL),
    ("11", "fanmeet-heart", "1:1",
     "A cheerful female fictional idol making a finger-heart gesture toward "
     "the camera at a fan meeting, warm golden stage lights, soft bokeh crowd "
     "background, cute pastel stage outfit, heartwarming event photography, " + TAIL),
    ("12", "birthday-cafe", "1:1",
     "Fan-organized birthday cafe display for a fictional idol, pastel banners "
     "decorated with star symbols, a beautiful cream cake, photo wall with "
     "candid prints, cozy cafe interior, cozy lifestyle photography, " + TAIL),
    ("13", "variety-pose", "16:9",
     "Playful fictional idol striking a goofy variety-show pose on a bright "
     "colorful studio set, oversized novelty props, laughing expression, "
     "vibrant game-show lighting, energetic entertainment photography, " + TAIL),
    ("14", "album-flatlay", "1:1",
     "K-pop album unboxing flat lay photographed from directly above, generic "
     "pastel album sleeve with abstract star artwork, shiny photocard, CD with "
     "iridescent reflection, photocard sleeves and stickers on a clean desk, "
     "no readable text or logos, product photography, " + TAIL),
    ("15", "finale-fireworks", "16:9",
     "Wide shot of an outdoor festival stage finale at night, fictional idol "
     "group silhouetted on the main stage while huge colorful fireworks "
     "explode over a massive crowd holding lightsticks, epic cinematic concert "
     "photography, 24mm lens, " + TAIL),
    ("16", "beauty-rainbow", "1:1",
     "Extreme close-up beauty shot of a fictional idol's face, rainbow prism "
     "light refracting across flawless dewy skin, glitter freckles, glossy "
     "lips, iridescent highlighter, high-end beauty editorial photography, "
     "macro 100mm lens, " + TAIL),
]


def load_key():
    env = os.path.join(os.path.dirname(HERE), ".env")
    with open(env, encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("NANO_GPT_API_KEY="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    sys.exit("NANO_GPT_API_KEY not found in .env")


def jpeg_dims(raw):
    if raw[:2] != b"\xff\xd8":
        return None
    i = 2
    while i + 9 < len(raw):
        if raw[i] != 0xFF:
            i += 1
            continue
        m = raw[i + 1]
        if m in (0xD8, 0x01) or 0xD0 <= m <= 0xD7:
            i += 2
            continue
        if i + 4 > len(raw):
            return None
        seg = struct.unpack(">H", raw[i + 2:i + 4])[0]
        if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
            h, w = struct.unpack(">HH", raw[i + 5:i + 9])
            return w, h
        i += 2 + seg
    return None


def validate(raw, path):
    dims = jpeg_dims(raw)
    if not dims:
        sys.exit(f"FATAL: {path} is not a decodable JPEG")
    if len(raw) < MIN_BYTES:
        raise ValueError(f"file too small ({len(raw)} bytes)")
    return dims


def generate(key, prompt, aspect):
    body = json.dumps({
        "model": MODEL, "prompt": prompt, "n": 1,
        "resolution": RESOLUTION, "aspect_ratio": aspect,
    }).encode()
    last = None
    for attempt in range(4):  # initial try + up to 3 retries
        req = urllib.request.Request(
            API, data=body, method="POST",
            headers={"Content-Type": "application/json", "x-api-key": key})
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                return json.load(resp)
        except urllib.error.HTTPError as exc:
            last = exc
            if exc.code >= 500 or exc.code == 429:
                wait = 20 * (attempt + 1)
                print(f"    HTTP {exc.code}, retrying in {wait}s", flush=True)
                time.sleep(wait)
                continue
            raise
    raise RuntimeError(f"retries exhausted: {last}")


def main():
    only = None
    if len(sys.argv) > 2 and sys.argv[1] == "--only":
        only = sys.argv[2]
    key = load_key()
    manifest_path = os.path.join(HERE, "manifest.json")
    entries = []
    if os.path.exists(manifest_path):
        with open(manifest_path, encoding="utf-8") as fh:
            entries = json.load(fh)
    done = {e["category"] for e in entries}
    spend = sum(e.get("cost_if_known") or 0 for e in entries)

    for num, slug, aspect, prompt in CATEGORIES:
        if only and num != only:
            continue
        rel = f"idols/{num}-{slug}.jpg"
        if num in done:
            print(f"== {num}-{slug}: already in manifest, skipping")
            continue
        if spend + PER_IMAGE_EST_USD > BUDGET_TARGET_USD:
            sys.exit(f"ABORT: budget target ${BUDGET_TARGET_USD} would be exceeded "
                     f"(spent ${spend:.4f})")
        print(f"== {num}-{slug} [{aspect}] generating ...", flush=True)
        data = generate(key, prompt, aspect)
        item = data["data"][0]
        raw = base64.b64decode(item["b64_json"])
        out = os.path.join(HERE, rel)
        try:
            w, h = validate(raw, rel)
        except ValueError as exc:
            print(f"    invalid output ({exc}); regenerating once", flush=True)
            data = generate(key, prompt + ", different composition", aspect)
            raw = base64.b64decode(data["data"][0]["b64_json"])
            w, h = validate(raw, rel)
        with open(out, "wb") as fh:
            fh.write(raw)
        cost = data.get("cost")
        spend += cost or 0
        entries.append({
            "file": rel, "category": slug, "aspect_ratio": aspect,
            "width": w, "height": h, "bytes": len(raw), "prompt": prompt,
            "cost_if_known": cost,
        })
        entries.sort(key=lambda e: e["file"])
        with open(manifest_path, "w", encoding="utf-8") as fh:
            json.dump(entries, fh, indent=2)
            fh.write("\n")
        bal = data.get("remainingBalance")
        print(f"    saved {rel} {w}x{h} {len(raw)}B "
              f"cost=${cost} spend=${spend:.4f} balance=${bal}", flush=True)

    print(f"TOTAL SPEND: ${spend:.4f}")


if __name__ == "__main__":
    main()
