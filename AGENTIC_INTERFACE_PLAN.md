# RapidRAW Agentic Interface — Implementation Plan

Goal: a programmatic interface that lets an AI agent work **side-by-side** with a human on RAW
editing, in a **lossless read → tune → read → tune** loop, exposing **all sliders** and
**all mask operations**.

Status: research complete. This document is the blueprint. Implementation starts at Layer 1.

---

## 0. Research findings (verified against source)

### 0.1 Lossless read chain

```
RAW file → memmap → develop_raw_image() → DynamicImage::ImageRgba32F → Arc → AppState.original_image
```

- The editable base image is a **linear, demosaiced, floating-point, 4-channel** `ImageRgba32F`.
  This is the genuinely lossless representation; every adjustment renders against this same Arc.
- Decoding happens **once**. `load_image` stores `Arc<DynamicImage>` in `decoded_image_cache`
  (key = source path). Subsequent adjustments reuse the Arc — never re-decode.
- `load_image_generation` (AtomicUsize) cancels / supersedes stale loads.
- Non-RAW (JPG/PNG/etc.) is normalized to `ImageRgb32F` (linearized).

**Implication:** the "lossless read → tune → read → tune" loop is already how the app works.
"Read" = hold the Arc; "tune" = pass a new adjustments JSON to the render pipeline.

Key files:
- `src-tauri/src/raw_processing.rs` — `develop_raw_image` (outputs `ImageRgba32F`)
- `src-tauri/src/image_loader.rs` — `load_image` (Tauri command), `load_base_image_from_bytes`
- `src-tauri/src/app_state.rs` — `LoadedImage { path, image: Arc<DynamicImage>, is_raw }`

### 0.2 Adjustments = a loose JSON object (key design fact)

Backend `apply_adjustments` takes `serde_json::Value`, **not** a typed struct. Fields are
camelCase, read via `adj["exposure"].as_f64().unwrap_or(0.0)`. Missing fields fall back to
defaults — the schema is extremely fault-tolerant, which is ideal for an agent.

**Authoritative schema** lives in the frontend: `src/utils/adjustments.ts` → `INITIAL_ADJUSTMENTS`.

#### All slider groups (agent-controllable)

| Group | Fields (camelCase) |
|---|---|
| **Basic** | `exposure`, `brightness`, `contrast`, `highlights`, `shadows`, `whites`, `blacks`, `toneMapper` (`'basic'`\|`'agx'`) |
| **Color** | `temperature`, `tint`, `saturation`, `vibrance`, `hue`, `colorGrading` (shadows/midtones/highlights/global HueSatLum + balance/blending), `hsl` (8-color: reds/oranges/yellows/greens/aquas/blues/purples/magentas, each HueSatLum), `colorCalibration` (shadowsTint + RGB hue/sat) |
| **Curves** | `curves` / `pointCurves` (luma/red/green/blue control-point arrays `{x,y}`), `parametricCurve`, `curveMode` (`'point'`\|`'parametric'`) |
| **Details** | `clarity`, `dehaze`, `structure`, `centré`, `sharpness`, `sharpnessThreshold`, `lumaNoiseReduction`, `colorNoiseReduction`, `chromaticAberrationRedCyan`, `chromaticAberrationBlueYellow` |
| **Effects** | `glowAmount`, `halationAmount`, `flareAmount`, `grainAmount`/`grainRoughness`/`grainSize`, `vignetteAmount`/`vignetteFeather`/`vignetteMidpoint`/`vignetteRoundness`, `lutIntensity`/`lutName`/`lutPath`/`lutSize`/`lutData` |
| **Geometry** | `crop` (`{x,y,width,height}`), `rotation`, `flipHorizontal`/`flipVertical`, `orientationSteps`, `transformDistortion`/`transformVertical`/`transformHorizontal`/`transformRotate`/`transformAspect`/`transformScale`/`transformXOffset`/`transformYOffset`, lens correction (`lensCorrectionMode`, `lensMaker`, `lensModel`, `lensDistortionAmount`, `lensVignetteAmount`, `lensTcaAmount`, `lensDistortionParams`, `lens*Enabled`) |

### 0.3 Masks (verified controllable)

Masks are `adjustments.masks: MaskContainer[]`. Each container has:
- `adjustments`: a **per-region** sub-`MaskAdjustments` (almost all sliders except geometry) — so an
  agent can "darken just the sky" or "brighten just the face."
- `subMasks: SubMask[]`: define the mask shape/source.
- `invert`, `opacity`, `visible`, `name`, `id`.

#### SubMask types (`Mask` enum — all programmatically generatable)

| `type` | Shape / source | `parameters` |
|---|---|---|
| `linear` | linear gradient | `{startX,startY,endX,endY,feather}` |
| `radial` | radial gradient | `{centerX,centerY,radiusX,radiusY,rotation,feather}` |
| `brush` | brush strokes | point sequence |
| `color` | color range | color params |
| `luminance` | luminance range | luminance params |
| `flow` | flow | — |
| `ai-subject` / `ai-sky` / `ai-foreground` / `ai-depth` | **AI auto-mask** (ONNX) | none — backend computes the bitmap automatically |
| `clone` / `heal` | healing | — |

- `mode`: `additive` / `subtractive` / `intersect` (boolean ops between masks).
- **Render path**: backend `generate_mask_bitmap()` (`mask_generation.rs:1320`) converts each
  SubMask to a `GrayImage`, cached in `state.mask_cache`; the GPU pipeline feeds mask bitmaps in as
  textures, applies each mask region's `adjustments`, then composites.
- GPU parallel mask limit: `MAX_MASKS` (~2–16, see `gpu_processing.rs:1102`).

**Implication:** agents can create/modify masks declaratively. AI masks (`ai-sky`, `ai-subject`)
need only the `type` — the backend computes the region; the agent does not paint geometry.

### 0.4 Render / preview pipeline (the "tune" exit)

```
apply_adjustments(adj, is_interactive, target_resolution, roi, ...)
  → PreviewJob enqueued → preview worker
  → reuse original_image Arc + compute mask_bitmaps + GPU pipeline
  → Response(Vec<u8>)   (bytes returned to caller)
```

- **Supersede**: worker uses `oneshot::channel`; a new job supersedes an in-flight one — ideal for
  rapid parameter iteration.
- `target_resolution`: output size cap (small = fast). Use low-res while iterating, full-res on commit.
- `roi`: `(x,y,w,h)` — render only a sub-region (e.g. just the face) for fast feedback.
- `is_interactive: true` → skip analytics (waveform/histogram) for speed.

### 0.5 Programmatic entry point (revised after source verification)

Two findings from reading the source reshaped the design:

1. **`apply_adjustments` is already a thin stub.** At `src-tauri/src/lib.rs:671` it is ~30 lines
   that do *not* use `AppHandle` at all — it packs a `PreviewJob` and sends it down
   `state.preview_worker_tx` (a `mpsc::Sender`), then awaits a `oneshot`. The real work is in
   `process_preview_job(&app_handle, state, ...)`. **So the "extract `render_preview`" refactor
   from the original draft is almost unnecessary** — an HTTP handler can grab the same
   `Sender<PreviewJob>` from `AppState` and reuse the worker verbatim.

2. **The frontend Zustand store is the single source of truth for current adjustments.**
   `useEditorStore.adjustments` (`src/store/useEditorStore.ts:34`) holds live edits. The backend is
   *stateless* about adjustments — it only ever receives them as an ephemeral JSON param per
   render. Persistence happens on slider-release (300ms debounce) → `save_metadata_and_update_thumbnail`
   → `.rrdata` sidecar.

**Chosen architecture: "agent as virtual user" (option B1).** The agent does **not** bypass the
frontend. It drives the frontend through the *same entry points the UI already uses*, so human and
agent literally watch the same store — zero state divergence by construction, and the "side-by-side"
UX (agent edits appear live → human clicks and tweaks → agent reads merged result) comes for free.

Two distinct paths, matching the plan's "preview vs commit" split:

| Agent action | Path | GUI effect | Sidecar |
|---|---|---|---|
| `POST /preview` (explore) | HTTP → grab `preview_worker_tx` from state → push `PreviewJob` directly → await `oneshot` → JPEG bytes | **Silent** (no flicker while agent iterates) | None |
| `POST /adjust` (commit) | HTTP → `app_handle.emit("agent://adjustments-apply", adj)` → frontend listener calls existing `setAdjustments` + `applyAdjustments` | **Live update** ✅ | Written ✅ |
| `POST /mask/*` | Same emit pattern (`agent://mask-added` etc.) → existing mask CRUD setters | **Live update** ✅ | Written ✅ |
| `GET /state` | Reads current adjustments via a tiny new Tauri command backed by `useEditorStore.getState()` | — | — |

Integration: a **localhost HTTP server spawned in Tauri `setup()`** (axum, `127.0.0.1:0`), plus a
small (~30-line) event listener added to `src/hooks/useTauriListeners.ts`. All image-processing
logic is reused unchanged.

---

## 0.6 Event protocol (agent ↔ frontend)

New Tauri events (frontend listens in `useTauriListeners.ts`, alongside the existing 33 listeners):

| Event | Payload | Frontend action |
|---|---|---|
| `agent://adjustments-apply` | full `Adjustments` JSON | `setAdjustments(payload)` then `applyAdjustments(payload, false, targetRes)` (existing hook) → triggers render + debounced autosave |
| `agent://mask-added` | `MaskContainer` | append to `adjustments.masks` via existing `setAdjustments` |
| `agent://mask-updated` | `{id, patch}` | merge patch into matching container |
| `agent://mask-removed` | `{id}` | filter out container |
| `agent://image-loaded` | `{path, width, height, is_raw}` | set `selectedImage` + load metadata (so GUI shows the agent-loaded image) |

A symmetric command `get_agent_state` (new `#[tauri::command]`) lets the backend read
`useEditorStore.getState().adjustments` for `GET /state` — this is the only place the agent reads
the *merged* human+agent state.

---

## 1. Layer 1 — Rust control server (core)

### 1.1 Dependencies

Add to `src-tauri/Cargo.toml`:
```toml
axum = "0.7"
tower-http = { version = "0.6", features = ["cors"] }
```
(tokio is already a dependency with `features = ["full"]`.)

### 1.2 Reuse the preview worker directly (no extraction needed)

In `src-tauri/src/lib.rs`, `apply_adjustments` (line 671) is already a thin stub: it locks
`state.preview_worker_tx`, packs a `PreviewJob`, sends it, and awaits the `oneshot` receiver.
**No extraction is required.** The HTTP `/preview` handler does the same thing — clone the
`AppHandle`, get `AppState`, lock `preview_worker_tx`, send a `PreviewJob`, await the bytes.

The existing `#[tauri::command] async fn apply_adjustments` stays as-is. The only Rust addition is
the `agent_server` module + a tiny `get_agent_state` command for `/state`.

### 1.3 Agent server module

New file `src-tauri/src/agent_server.rs`:
- `pub fn start(app_handle: tauri::AppHandle)` — spawned from `setup()`.
- Binds `127.0.0.1:0` (OS-assigned port), writes the port to
  `<app cache>/rapidraw-agent-port` so the agent client can discover it.
- axum routes (all `127.0.0.1` only):

| Method | Path | Body / Return | Purpose |
|---|---|---|---|
| `GET`  | `/health` | → `{status:"ok", version}` | liveness |
| `GET`  | `/schema` | → full slider+mask schema JSON (mirrors `INITIAL_ADJUSTMENTS`) | agent self-discovery |
| `GET`  | `/state` | → `{path, width, height, is_raw, adjustments}` (adjustments read from frontend store via `get_agent_state` command) | current image + **merged human+agent** edits |
| `POST` | `/load` | `{path}` → `{width, height, is_raw}` | load an image (lossless decode) |
| `POST` | `/preview` | `{adjustments, target_resolution?, roi?, format?}` → rendered image bytes (default JPEG) | the **tune** step of the loop |
| `POST` | `/adjust` | `{adjustments}` → applies & persists to `.rrdata` sidecar | commit an edit |
| `GET`  | `/export` | `{path, adjustments, format, quality, width}` → exported bytes | full-res output |
| `POST` | `/mask/add` | `{type, adjustments, subMasks, name?, opacity?, invert?}` → `{maskId}` | add a mask |
| `POST` | `/mask/:id` | partial MaskContainer patch | update a mask |
| `DELETE` | `/mask/:id` | — | remove a mask |

All handlers clone the `AppHandle`, pull `AppState` via `app_handle.state::<AppState>()`, and call
the existing worker (`/preview`), emit Tauri events (`/adjust`, `/mask/*`), or call existing
commands (`/load`, `/export`). **No new image-processing code.**

### 1.4 Minimal proof loop

Before building everything, wire just enough to validate:
1. `GET /health`
2. `POST /load {path}`
3. `POST /preview {adjustments:{exposure:0.5}, target_resolution:512}` → JPEG bytes saved to disk
4. Confirm round-trip latency and image fidelity.

Once that loop is fast and correct, add `/schema`, masks, export.

---

## 2. Layer 2 — Agent client library

A thin client (`scripts/agent_client.py` or `scripts/agent_client.ts`) that wraps the HTTP API into
typed methods:

```python
client = RapidRAWClient.discover()   # reads rapidraw-agent-port
img = client.load("/path/to/IMG.CR3")
preview = client.preview({"exposure": 0.5, "highlights": -30}, resolution=512)
client.add_mask(type="ai-sky", adjustments={"exposure": -1.0, "saturation": -40})
client.commit()                       # writes .rrdata
client.export("/out/IMG.jpg", quality=92)
```

Includes a baked-in schema (slider names, ranges, defaults) generated from `INITIAL_ADJUSTMENTS`
so the agent gets type hints and valid ranges without calling `/schema`.

---

## 3. Layer 3 — Mask operations API

Handled by the `/mask/*` routes in §1.3, plus client helpers:
- AI masks (`ai-sky`, `ai-subject`, `ai-foreground`, `ai-depth`): declare `type` only; backend
  computes the bitmap (reuses `ai_commands::generate_ai_*_mask`).
- Geometric masks (`linear`, `radial`, `brush`, `color`, `luminance`): client builds `parameters`.
- Compose with `mode: additive|subtractive|intersect`.

---

## 4. Open decisions (resolved)

1. **Protocol**: HTTP REST first (simple, curl-friendly). WebSocket deferred unless latency demands.
2. **Render result delivery**: byte stream (cleanest), with optional `?save=/path` for convenience.
3. **GUI sync**: ✅ **DECIDED — live sync (option B).** Agent commits (`/adjust`, `/mask/*`) emit
   Tauri events the frontend already-style listens to; the agent drives the same Zustand store the
   UI uses, so human and agent share one source of truth and see each other's edits live.
   `/preview` stays silent (render-only) so exploration doesn't flicker the GUI.
4. **Source of truth**: ✅ **Frontend Zustand (`useEditorStore.adjustments`)** — the existing design.
   The backend never stores current adjustments; the agent reads merged state via a new
   `get_agent_state` command for `GET /state`.
5. **Fork strategy**: edit local clone directly, keep changes isolated in `agent_server.rs` + one
   tiny frontend listener + one tiny `get_agent_state` command so it's easy to upstream later.

---

## 5. Implementation order (TDD)

1. **[ ] Step 1** — Add deps, create `agent_server.rs` skeleton, `GET /health`, port discovery file, spawn from `setup()`. Test: server starts, `/health` responds.
2. **[ ] Step 2** — `/load` + `/preview` (silent direct-to-worker path). **Validate latency/quality — this is the go/no-go gate.** Test: curl `/load` then `/preview {exposure:0.5}`, save JPEG, confirm valid.
3. **[ ] Step 3** — `/adjust` (emit `agent://adjustments-apply` + frontend listener), `/state` (`get_agent_state` command), `/schema`, `/export`. Test: curl `/adjust`, watch GUI update live.
4. **[ ] Step 4** — `/mask/add`, `/mask/:id`, `DELETE /mask/:id` (emit mask events). Test: add `ai-sky` mask, confirm GUI + effect.
5. **[ ] Step 5** — Layer 2 Python client library.
6. **[ ] Step 6** — End-to-end agent demo + latency measurement.

Each step is independently testable with `curl` before moving on.
