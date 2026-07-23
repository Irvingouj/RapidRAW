"""RapidRAW agent client.

A thin HTTP client for RapidRAW's loopback agent control server. Lets an AI
agent (or a human via a notebook) drive RapidRAW side-by-side with the GUI:

    >>> from agent_client import RapidRAWClient
    >>> client = RapidRAWClient.discover()
    >>> client.load("/path/to/IMG.CR3")
    >>> client.preview({"exposure": 0.5}, resolution=512)   # silent explore
    >>> client.adjust({"exposure": 0.5})                     # commits + live GUI sync
    >>> client.add_mask(mask_type="ai-sky", adjustments={"exposure": -1.0})
    >>> client.export("/out/IMG.jpg", quality=92)

Design notes (see AGENTIC_INTERFACE_PLAN.md):
  * `/preview` is render-only and silent — no GUI flicker, no sidecar. Use it
    to iterate.
  * `/adjust` and `/mask/*` commit: they drive the same Zustand store the GUI
    uses, so a human sees the agent's edits live and can tweak them; the agent
    can then `get_state()` to read the merged result.
"""

from __future__ import annotations

import json
import time
import urllib.request
import urllib.error
from pathlib import Path
from typing import Any, Optional

# ---------------------------------------------------------------------------
# Port discovery
# ---------------------------------------------------------------------------

# Where RapidRAW writes its OS-assigned port. `app_data_dir` differs per OS:
#   macOS: ~/Library/Application Support/io.github.CyberTimon.RapidRAW/
#   linux: ~/.local/share/io.github.CyberTimon.RapidRAW/
#   windows: %APPDATA%\\io.github.CyberTimon.RapidRAW\\
PORT_FILENAME = "rapidraw-agent-port"

_DEFAULT_APP_DATA_DIRS = [
    # macOS
    Path.home() / "Library" / "Application Support" / "io.github.CyberTimon.RapidRAW",
    # Linux
    Path.home() / ".local" / "share" / "io.github.CyberTimon.RapidRAW",
    # Windows (best-effort)
    Path.home() / "AppData" / "Roaming" / "io.github.CyberTimon.RapidRAW",
]


def discover_port(timeout: float = 10.0) -> int:
    """Read the published port from the app data dir.

    Polls for up to `timeout` seconds so callers can call this right after
    launching RapidRAW without racing the server startup.
    """
    deadline = time.monotonic() + timeout
    last_err: Optional[str] = None
    while time.monotonic() < deadline:
        for d in _DEFAULT_APP_DATA_DIRS:
            port_file = d / PORT_FILENAME
            if port_file.exists():
                try:
                    return int(port_file.read_text().strip())
                except (ValueError, OSError) as e:
                    last_err = f"unreadable port file {port_file}: {e}"
        time.sleep(0.1)
    raise RuntimeError(
        f"Could not find RapidRAW agent port file '{PORT_FILENAME}' in "
        f"{[str(d) for d in _DEFAULT_APP_DATA_DIRS]}. "
        f"Is RapidRAW running? ({last_err or 'no file found'})"
    )


# ---------------------------------------------------------------------------
# Client
# ---------------------------------------------------------------------------


class RapidRAWError(RuntimeError):
    """Raised when the server returns a non-2xx response."""


class RapidRAWClient:
    def __init__(self, base_url: str, timeout: float = 60.0):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    # ---- construction ----

    @classmethod
    def discover(cls, timeout: float = 10.0, request_timeout: float = 60.0) -> "RapidRAWClient":
        """Find the running RapidRAW instance and return a client for it."""
        port = discover_port(timeout=timeout)
        client = cls(f"http://127.0.0.1:{port}", timeout=request_timeout)
        client.health()  # fail fast if the server isn't actually responding
        return client

    # ---- low-level HTTP ----

    def _request(
        self,
        method: str,
        path: str,
        *,
        json_body: Optional[Any] = None,
        expect: str = "json",
    ) -> Any:
        url = f"{self.base_url}{path}"
        data = None
        headers = {"Accept": "application/json"}
        if json_body is not None:
            data = json.dumps(json_body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                body = resp.read()
                if expect == "bytes":
                    return body
                if expect == "json":
                    return json.loads(body) if body else None
                return None
        except urllib.error.HTTPError as e:
            detail = e.read().decode("utf-8", "replace")
            raise RapidRAWError(f"{method} {path} -> {e.code}: {detail}") from None
        except urllib.error.URLError as e:
            raise RapidRAWError(f"{method} {path} -> connection error: {e.reason}") from None

    # ---- routes ----

    def health(self) -> dict:
        return self._request("GET", "/health")

    def load(self, path: str) -> dict:
        """Lossless-decode an image into the backend. Returns {width,height,isRaw}."""
        return self._request("POST", "/load", json_body={"path": str(path)})

    def preview(
        self,
        resolution: Optional[int] = None,
        roi: Optional[tuple[float, float, float, float]] = None,
    ) -> bytes:
        """JPEG of the **current** committed look (server mirror). No what-if params.

        To try a change: adjust(...) then preview() again.
        """
        body: dict[str, Any] = {}
        if resolution is not None:
            body["targetResolution"] = resolution
        if roi is not None:
            body["roi"] = list(roi)
        return self._request("POST", "/preview", json_body=body, expect="bytes")

    def preview_to_file(self, out_path: str, **kw) -> str:
        """Convenience: current-look preview written to `out_path`."""
        jpeg = self.preview(**kw)
        Path(out_path).write_bytes(jpeg)
        return str(out_path)

    def adjust(self, adjustments: dict) -> dict:
        """Commit adjustments: drives the live GUI (Zustand store) + writes sidecar."""
        return self._request("POST", "/adjust", json_body={"adjustments": adjustments})

    def add_mask(
        self,
        mask_type: str,
        adjustments: Optional[dict] = None,
        name: Optional[str] = None,
        opacity: float = 1.0,
        invert: bool = False,
        sub_masks: Optional[list[dict]] = None,
    ) -> dict:
        body: dict[str, Any] = {
            "type": mask_type,
            "adjustments": adjustments or {},
            "opacity": opacity,
            "invert": invert,
        }
        if name is not None:
            body["name"] = name
        if sub_masks is not None:
            body["subMasks"] = sub_masks
        return self._request("POST", "/mask/add", json_body=body)

    def update_mask(self, mask_id: str, patch: dict) -> dict:
        return self._request("POST", f"/mask/{mask_id}", json_body=patch)

    def remove_mask(self, mask_id: str) -> dict:
        return self._request("DELETE", f"/mask/{mask_id}")

    def get_state(self) -> dict:
        return self._request("GET", "/state")

    def get_schema(self) -> dict:
        return self._request("GET", "/schema")

    def export(
        self,
        out_path: Optional[str] = None,
        resolution: Optional[int] = None,
    ) -> bytes:
        """JPEG of the **current** committed look; optionally also write to out_path."""
        body: dict[str, Any] = {}
        if out_path is not None:
            body["path"] = str(out_path)
        if resolution is not None:
            body["targetResolution"] = resolution
        return self._request("POST", "/export", json_body=body, expect="bytes")


__all__ = ["RapidRAWClient", "RapidRAWError", "discover_port"]
