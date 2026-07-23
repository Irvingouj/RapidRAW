#!/usr/bin/env python3
"""End-to-end test for the RapidRAW agent control server.

Run with RapidRAW open. Validates:
  /health       — responds with version
  /load         — decodes an image
  /preview      — returns a valid clean JPEG (no interactive header), fast
  /adjust       — commits a partial patch
  /state        — reflects the merged adjustments (optimistic + frontend push)

Exits non-zero on any failure. Prints a summary.

Usage: python scripts/test_agent_e2e.py [path/to/image]
"""

from __future__ import annotations

import io
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from agent_client import RapidRAWClient, RapidRAWError  # noqa: E402

DEFAULT_IMAGE = "/Users/oujunyi/code/RapidRAW/public/splash-grey.jpg"


def ok(msg: str) -> None:
    print(f"  \033[32m✓\033[0m {msg}")


def fail(msg: str) -> None:
    print(f"  \033[31m✗\033[0m {msg}")


def main() -> int:
    image = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_IMAGE
    failures = 0

    print("→ discovering server …")
    try:
        c = RapidRAWClient.discover(timeout=20)
        ok(f"connected: {c.health()}")
    except Exception as e:
        fail(f"discovery failed: {e}")
        return 1

    # /load
    print("→ POST /load")
    try:
        r = c.load(image)
        assert r["width"] > 0 and r["height"] > 0, "no dimensions"
        ok(f"{r['width']}x{r['height']} isRaw={r['isRaw']}")
    except Exception as e:
        fail(str(e)); failures += 1

    # /preview — must be a clean, decodable JPEG, reasonably fast
    print("→ POST /preview (exposure +0.5, res 512)")
    try:
        t0 = time.perf_counter()
        jpeg = c.preview({"exposure": 0.5}, resolution=512)
        dt_ms = (time.perf_counter() - t0) * 1000

        # validate JPEG
        from PIL import Image
        img = Image.open(io.BytesIO(jpeg))
        img.load()
        assert img.format == "JPEG", f"not JPEG: {img.format}"

        ok(f"{len(jpeg)} bytes, {img.size[0]}x{img.size[1]} JPEG in {dt_ms:.0f} ms")
        if dt_ms > 1500:
            fail(f"slow: {dt_ms:.0f}ms > 1500ms"); failures += 1
    except Exception as e:
        fail(f"preview invalid: {e}"); failures += 1

    # /adjust — partial patch
    print("→ POST /adjust {exposure: 0.7, temperature: 20}")
    try:
        r = c.adjust({"exposure": 0.7, "temperature": 20})
        assert r.get("ok") is True, f"not ok: {r}"
        ok(f"acknowledged, merged exposure={r.get('adjustments', {}).get('exposure')}")
    except Exception as e:
        fail(str(e)); failures += 1

    # /state — should reflect the merged adjustment
    print("→ GET /state")
    try:
        s = c.get_state()
        ev = s.get("adjustments", {}).get("exposure")
        assert ev == 0.7, f"exposure not merged: got {ev!r}"
        ok(f"path={s.get('path')} exposure={ev} (merged view)")
    except Exception as e:
        fail(str(e)); failures += 1

    print()
    if failures:
        print(f"\033[31m{failures} failure(s)\033[0m")
    else:
        print("\033[32mALL CHECKS PASSED\033[0m")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
