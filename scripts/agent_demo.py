#!/usr/bin/env python3
"""End-to-end demo: agent edits a photo side-by-side with the GUI.

Run with RapidRAW open. The script:
  1. discovers the agent port
  2. loads an image
  3. iterates exposure via /preview (silent — no GUI flicker) and saves each
  4. commits a final adjustment via /adjust (GUI updates live)
  5. adds an AI sky mask
  6. reads the merged state back via /state
  7. exports the result

Usage:
    python scripts/agent_demo.py /path/to/IMG.CR3 [out_dir]
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

# allow running from repo root without install
sys.path.insert(0, str(Path(__file__).resolve().parent))
from agent_client import RapidRAWClient, RapidRAWError  # noqa: E402


def main(image_path: str, out_dir: str) -> None:
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)

    print("→ discovering RapidRAW agent server …")
    client = RapidRAWClient.discover(timeout=15)
    print(f"  connected: {client.health()}")

    print(f"→ loading {image_path}")
    info = client.load(image_path)
    print(f"  {info}")

    # ---- silent exploration: sweep exposure, save each preview ----
    print("→ iterating exposure via /preview (silent, no GUI flicker) …")
    for ev in [-1.0, -0.5, 0.0, 0.5, 1.0]:
        t0 = time.perf_counter()
        jpeg = client.preview({"exposure": ev}, resolution=1024)
        dt = (time.perf_counter() - t0) * 1000
        p = out / f"preview_ev{ev:+.1f}.jpg"
        p.write_bytes(jpeg)
        print(f"  ev={ev:+.1f}  {len(jpeg):>7} bytes  {dt:6.1f} ms  → {p.name}")

    # ---- commit: drives the live GUI ----
    print("→ committing exposure=+0.5 via /adjust (watch the GUI update live) …")
    res = client.adjust({"exposure": 0.5, "temperature": 15, "vibrance": 10})
    print(f"  {res}")

    # ---- add an AI sky mask ----
    print("→ adding ai-sky mask (exposure -1.0, saturation -30) …")
    try:
        res = client.add_mask(
            mask_type="ai-sky",
            adjustments={"exposure": -1.0, "saturation": -30},
            name="Agent: darken sky",
        )
        print(f"  {res}")
    except RapidRAWError as e:
        print(f"  (mask route not wired yet or failed: {e})")

    # ---- read merged state back (human + agent) ----
    try:
        state = client.get_state()
        adj = state.get("adjustments", {})
        print(f"→ merged /state: exposure={adj.get('exposure')} temp={adj.get('temperature')}")
    except RapidRAWError as e:
        print(f"  (/state not wired yet: {e})")

    # ---- export ----
    export_path = out / "export.jpg"
    print(f"→ exporting to {export_path} …")
    try:
        data = client.export(out_path=str(export_path), quality=92)
        export_path.write_bytes(data)
        print(f"  wrote {len(data)} bytes")
    except RapidRAWError as e:
        print(f"  (/export not wired yet: {e})")

    print("✓ done. Open the previews in an image viewer to compare exposures.")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    img = sys.argv[1]
    outd = sys.argv[2] if len(sys.argv) > 2 else "/tmp/rapidraw-demo"
    main(img, outd)
