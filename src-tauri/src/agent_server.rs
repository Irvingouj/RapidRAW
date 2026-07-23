//! Localhost HTTP control server for AI agents.
//!
//! Lets an AI agent work side-by-side with a human on RAW editing by exposing
//! the existing render pipeline and frontend adjustments store over a loopback
//! HTTP API. See `AGENTIC_INTERFACE_PLAN.md`.
//!
//! Design (verified against source):
//! - `/preview` is silent: it grabs `state.preview_worker_tx` and pushes a
//!   `PreviewJob` directly, exactly like the `apply_adjustments` Tauri command.
//! - `/adjust` and `/mask/*` emit Tauri events (`agent://...`) that the frontend
//!   already listens for; the agent drives the same Zustand store the UI uses,
//!   so human and agent share one source of truth ("agent as virtual user").
//! - Port is OS-assigned (`127.0.0.1:0`) and published to a discovery file under
//!   the app data dir so the agent client can find it.

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use image::GenericImageView;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

use crate::app_state::{AppState, PreviewJob};
use crate::image_loader::load_image_core;

/// Filename (inside the app data dir) where the chosen port is published.
pub const PORT_FILENAME: &str = "rapidraw-agent-port";

/// Shared state injected into every route handler.
#[derive(Clone)]
struct AgentServerState {
    app_handle: AppHandle,
}

impl AgentServerState {
    #[allow(dead_code)] // used by /load, /state, /export in the next steps
    fn app_state(&self) -> tauri::State<'_, AppState> {
        // Cheap: tauri::State is a borrow of an Arc stored on the app.
        self.app_handle.state::<AppState>()
    }

    fn mirror(&self) -> Option<Value> {
        self.app_handle
            .try_state::<AgentStateMirror>()
            .map(|m| m.0.lock().unwrap().clone())
            .flatten()
    }

    fn set_mirror(&self, value: Value) {
        if let Some(m) = self.app_handle.try_state::<AgentStateMirror>() {
            *m.0.lock().unwrap() = Some(value);
        }
    }
}

// ----------------------------------------------------------------------------
// Public entry point
// ----------------------------------------------------------------------------

/// Spawn the agent control server on a background tokio task.
///
/// Binds `127.0.0.1:0` (loopback only, OS-assigned port), writes the port to
/// `<app_data_dir>/rapidraw-agent-port` for client discovery, then serves the
/// router forever. Called from `lib.rs` `setup()`.
///
/// Returns the bound address (mainly useful for tests).
pub async fn start(app_handle: AppHandle) -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("agent_server: failed to bind 127.0.0.1:0: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;

    // Publish the port so an external agent client can discover it.
    if let Err(e) = publish_port(&app_handle, addr.port()) {
        // Discovery is best-effort; don't abort the whole server over it.
        log::warn!("agent_server: could not write port discovery file: {e}");
    }

    let shared = Arc::new(AgentServerState { app_handle: app_handle.clone() });
    let app = router(shared);

    log::info!("agent_server: listening on http://{addr}");

    tokio::spawn(async move {
        // `axum::serve` takes ownership of the listener; this task runs forever.
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("agent_server: serve exited with error: {e}");
        }
    });

    Ok(addr)
}

/// Build the router. Pure (no I/O) so it can be exercised in isolation.
fn router(shared: Arc<AgentServerState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/load", post(load))
        .route("/preview", post(preview))
        .route("/adjust", post(adjust))
        .route("/state", get(state))
        .route("/schema", get(schema))
        .route("/export", post(export_image))
        .route("/mask/add", post(mask_add))
        .route("/mask/:id", post(mask_update).delete(mask_remove))
        .with_state(shared)
        // Loopback-only server, but enable a permissive CORS layer so that
        // browser-based dev tools / notebooks can also poke at it.
        .layer(CorsLayer::very_permissive())
}

// ----------------------------------------------------------------------------
// Discovery file
// ----------------------------------------------------------------------------

/// Path to the port discovery file inside the app data directory.
pub fn port_file_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("agent_server: app_data_dir unavailable: {e}"))?;
    Ok(dir.join(PORT_FILENAME))
}

fn publish_port(app_handle: &AppHandle, port: u16) -> Result<(), String> {
    let path = port_file_path(app_handle)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Atomic-ish write via temp + rename to avoid partial reads by clients.
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, port.to_string()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------------------

/// `GET /health` → `{ "status": "ok", "version": "1.5.9" }`
///
/// `version` is read from the Tauri package info (which mirrors
/// `tauri.conf.json`), not `CARGO_PKG_VERSION` (which is `0.0.0` for the lib
/// crate).
async fn health(State(shared): State<Arc<AgentServerState>>) -> impl IntoResponse {
    let version = shared.app_handle.package_info().version.to_string();
    health_body(&version)
}

/// Pure body builder for `/health`, factored out so tests can exercise it
/// without a Tauri `AppHandle`.
fn health_body(version: &str) -> axum::Json<serde_json::Value> {
    axum::Json(json!({ "status": "ok", "version": version }))
}

/// `POST /load` body.
#[derive(Deserialize)]
struct LoadRequest {
    path: String,
    /// When true (default), navigate the live GUI to this image too, so human
    /// and agent work side-by-side on the same image. Set false for silent
    /// backend-only loading.
    #[serde(default = "default_navigate")]
    navigate: bool,
}

fn default_navigate() -> bool {
    true
}

/// `POST /load` response.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadResponse {
    path: String,
    width: u32,
    height: u32,
    is_raw: bool,
}

/// `POST /load {path}` — lossless decode into `AppState.original_image`.
///
/// Reuses the exact same code path as the frontend `load_image` command
/// (`load_image_core`), so cache invalidation, EXIF, and RAW demosaic are
/// identical. Does NOT touch the GUI (the agent may load an image the human
/// hasn't navigated to); use `/adjust` to drive the live window if desired.
async fn load(
    State(shared): State<Arc<AgentServerState>>,
    axum::Json(req): axum::Json<LoadRequest>,
) -> Response {
    let app_state = shared.app_state();
    let result = load_image_core(&app_state, &shared.app_handle, req.path.clone()).await;
    match result {
        Ok(r) => {
            // By default, navigate the live GUI to this image so human and
            // agent work side-by-side on the same image. The agent can opt out
            // with `{ "navigate": false }` for silent backend-only loading.
            if req.navigate {
                emit_event(
                    &shared.app_handle,
                    "agent://navigate-to-image",
                    &json!({ "path": req.path }),
                );
            }
            emit_event(
                &shared.app_handle,
                "agent://image-loaded",
                &json!({
                    "path": req.path,
                    "width": r.width,
                    "height": r.height,
                    "isRaw": r.is_raw,
                }),
            );
            (
                StatusCode::OK,
                axum::Json(LoadResponse {
                    path: req.path,
                    width: r.width,
                    height: r.height,
                    is_raw: r.is_raw,
                }),
            )
                .into_response()
        }
        Err(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({ "error": msg })),
        )
            .into_response(),
    }
}

/// `POST /preview` body.
#[derive(Deserialize)]
struct PreviewRequest {
    adjustments: serde_json::Value,
    #[serde(default)]
    target_resolution: Option<u32>,
    /// Region of interest, normalized `(x, y, w, h)` in 0..1. Constrains the
    /// rendered region but does NOT prepend an interactive header (we always
    /// return clean JPEG bytes for agent consumption).
    #[serde(default)]
    roi: Option<(f32, f32, f32, f32)>,
}

/// `POST /preview {adjustments, target_resolution?, roi?}` → JPEG bytes.
///
/// **Silent exploration path.** Pushes a `PreviewJob` directly to the worker
/// (just like `apply_adjustments`) and returns the rendered bytes. Does NOT
/// touch the GUI or write a sidecar — ideal for the agent to iterate on many
/// parameter values without flickering the human's window.
///
/// Returns a clean JPEG (no header). Internally we set `is_interactive=false`
/// because the interactive path prepends a 24-byte ROI/dimension header that
/// only the GUI's interactive zoom renderer needs; agents want raw JPEG bytes.
/// The optional `roi` still constrains the *rendered region* (normalized rect),
/// it just doesn't get a header prepended.
async fn preview(
    State(shared): State<Arc<AgentServerState>>,
    axum::Json(req): axum::Json<PreviewRequest>,
) -> Response {
    let app_state = shared.app_state();
    match enqueue_preview(
        &app_state,
        req.adjustments,
        /* is_interactive = */ false,
        req.target_resolution,
        req.roi,
    )
    .await
    {
        Ok(bytes) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "image/jpeg",
            )],
            bytes,
        )
            .into_response(),
        Err(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({ "error": msg })),
        )
            .into_response(),
    }
}

/// `POST /adjust` body. The adjustments here are a *partial* patch — the
/// frontend deep-merges it onto its current store value, so the agent can send
/// just `{"exposure":0.5}` without wiping masks or other edits.
#[derive(Deserialize)]
struct AdjustRequest {
    adjustments: serde_json::Value,
}

/// `POST /adjust {adjustments}` — commit + live GUI sync.
///
/// Emits `agent://adjustments-apply`; the frontend listener deep-merges the
/// patch onto its Zustand store, which fires the existing render + debounced
/// autosave effect. The human sees the change live and can tweak it; the agent
/// can later read the merged result via `GET /state`.
async fn adjust(
    State(shared): State<Arc<AgentServerState>>,
    axum::Json(req): axum::Json<AdjustRequest>,
) -> Response {
    emit_event(
        &shared.app_handle,
        "agent://adjustments-apply",
        &req.adjustments,
    );
    // Optimistically update our local mirror too, so a racing /state right
    // after /adjust is consistent (the frontend's authoritative push will
    // overwrite this momentarily). If we have no mirror yet (frontend hasn't
    // pushed), seed it with the patch as-is.
    let merged = match shared.mirror() {
        Some(existing) => merge_json(existing, req.adjustments.clone()),
        None => req.adjustments.clone(),
    };
    shared.set_mirror(merged.clone());
    (StatusCode::OK, axum::Json(json!({ "ok": true, "adjustments": merged })))
        .into_response()
}

/// `GET /state` — the merged human+agent view.
///
/// Returns the currently-loaded image (path/dimensions/is_raw from
/// `AppState.original_image`) plus the frontend's current adjustments (kept
/// fresh by the `update_agent_state` Tauri command).
async fn state(State(shared): State<Arc<AgentServerState>>) -> Response {
    let app_state = shared.app_state();
    let loaded = app_state.original_image.lock().unwrap().clone();
    let (path, width, height, is_raw) = match &loaded {
        Some(img) => {
            let (w, h) = img.image.dimensions();
            (img.path.clone(), w, h, img.is_raw)
        }
        None => (String::new(), 0u32, 0u32, false),
    };
    let adjustments = shared.mirror().unwrap_or_else(|| json!({}));
    (
        StatusCode::OK,
        axum::Json(json!({
            "path": path,
            "width": width,
            "height": height,
            "isRaw": is_raw,
            "adjustments": adjustments,
        })),
    )
        .into_response()
}

/// Replace the cached frontend adjustments. Called by the `update_agent_state`
/// Tauri command whenever the frontend's store changes, so `GET /state` always
/// reflects the live merged human+agent view.
pub fn update_state_mirror(app_handle: &AppHandle, adjustments: Value) {
    // Reach into the same Arc<AgentServerState> the server holds. We stash it on
    // AppState as a weak ref so the Tauri command can find it without a global.
    if let Some(mirror) = app_handle.try_state::<AgentStateMirror>() {
        *mirror.0.lock().unwrap() = Some(adjustments);
    }
}

/// Tauri-managed holder for the adjustments mirror, so the `update_agent_state`
/// command can reach the same cell the HTTP `/state` handler reads.
#[derive(Default)]
pub struct AgentStateMirror(pub StdMutex<Option<Value>>);

// ----------------------------------------------------------------------------
// Mask routes
// ----------------------------------------------------------------------------

/// `POST /export` body.
#[derive(Deserialize)]
struct ExportRequest {
    adjustments: serde_json::Value,
    /// Optional output resolution cap. Omit for the backend's full preview
    /// resolution (typically 1920 on the long edge). For pixel-exact full-res
    /// export, use the GUI's export panel — this route reuses the preview path.
    #[serde(default)]
    target_resolution: Option<u32>,
    /// If set, write the JPEG to this path (created/overwritten) in addition to
    /// returning the bytes.
    #[serde(default)]
    path: Option<String>,
}

/// `POST /export {adjustments, target_resolution?, path?}` → high-quality JPEG
/// bytes (and optionally written to `path`).
///
/// This reuses the same render pipeline as `/preview` (offscreen encode) but is
/// meant for committing a final result rather than iterating. For multi-image
/// batch export with the full GUI export settings, use the app's export panel.
async fn export_image(
    State(shared): State<Arc<AgentServerState>>,
    axum::Json(req): axum::Json<ExportRequest>,
) -> Response {
    let app_state = shared.app_state();
    // Non-interactive → full-quality encode (no ROI header), matching /preview.
    match enqueue_preview(
        &app_state,
        req.adjustments,
        /* is_interactive = */ false,
        req.target_resolution,
        /* roi = */ None,
    )
    .await
    {
        Ok(bytes) => {
            if let Some(out) = req.path {
                if let Err(e) = fs::write(&out, &bytes) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(json!({ "error": format!("failed to write {out}: {e}") })),
                    )
                        .into_response();
                }
            }
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
                bytes,
            )
                .into_response()
        }
        Err(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({ "error": msg })),
        )
            .into_response(),
    }
}

/// `POST /mask/add` body. `mask_type` is one of: linear, radial, brush, color,
/// luminance, flow, ai-sky, ai-subject, ai-foreground, ai-depth.
/// For AI masks the backend computes the bitmap automatically — the agent only
/// needs to declare the `type`.
#[derive(Deserialize)]
struct MaskAddRequest {
    /// Mask shape/source type (e.g. "ai-sky", "radial", "brush").
    #[serde(rename = "type")]
    mask_type: String,
    /// Per-region adjustments to apply inside the mask (exposure, saturation, …).
    #[serde(default)]
    adjustments: Value,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    opacity: Option<f32>,
    #[serde(default)]
    invert: Option<bool>,
    /// Explicit sub-mask definitions for geometric masks. For AI masks this
    /// can be omitted; the frontend's AI generation hook will populate the
    /// bitmap when it processes the `ai-*` type.
    #[serde(default)]
    sub_masks: Option<Vec<Value>>,
}

/// `POST /mask/add` — emit `agent://mask-added`; the frontend listener appends
/// a container to `adjustments.masks` (driving the same render + autosave path).
/// Returns the new mask id so the agent can later update or remove it.
async fn mask_add(
    State(shared): State<Arc<AgentServerState>>,
    axum::Json(req): axum::Json<MaskAddRequest>,
) -> Response {
    let id = format!("agent-mask-{}", uuid::Uuid::new_v4());
    let payload = json!({
        "id": id,
        "type": req.mask_type,
        "name": req.name.unwrap_or_else(|| format!("Agent: {}", req.mask_type)),
        "opacity": req.opacity.unwrap_or(1.0),
        "invert": req.invert.unwrap_or(false),
        "adjustments": req.adjustments,
        "subMasks": req.sub_masks.unwrap_or_else(|| vec![
            // default single sub-mask carrying the declared type
            json!({ "id": format!("{id}-0"), "type": req.mask_type, "mode": "additive",
                     "visible": true, "invert": false, "opacity": 1.0 })
        ]),
    });
    emit_event(&shared.app_handle, "agent://mask-added", &payload);
    (StatusCode::OK, axum::Json(json!({ "maskId": id, "ok": true }))).into_response()
}

/// `POST /mask/:id` body — a partial patch merged into the matching container.
#[derive(Deserialize)]
struct MaskUpdateRequest {
    #[serde(flatten)]
    patch: Value,
}

/// `POST /mask/:id` — emit `agent://mask-updated` with `{id, patch}`.
async fn mask_update(
    State(shared): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<MaskUpdateRequest>,
) -> Response {
    emit_event(
        &shared.app_handle,
        "agent://mask-updated",
        &json!({ "id": id, "patch": req.patch }),
    );
    (StatusCode::OK, axum::Json(json!({ "ok": true }))).into_response()
}

/// `DELETE /mask/:id` — emit `agent://mask-removed` with `{id}`.
async fn mask_remove(
    State(shared): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
) -> Response {
    emit_event(
        &shared.app_handle,
        "agent://mask-removed",
        &json!({ "id": id }),
    );
    (StatusCode::OK, axum::Json(json!({ "ok": true }))).into_response()
}

// ----------------------------------------------------------------------------
// Schema route
// ----------------------------------------------------------------------------

/// `GET /schema` — a static description of the adjustments schema, slider
/// groups, and mask types so an agent can self-discover the API. Generated from
/// `src/utils/adjustments.ts` (`INITIAL_ADJUSTMENTS`).
///
/// Kept deliberately compact (groups + ranges) rather than a full dump, since
/// the authoritative schema is the frontend's `INITIAL_ADJUSTMENTS` and the
/// backend is fault-tolerant (`unwrap_or` defaults on every field).
async fn schema() -> impl IntoResponse {
    axum::Json(schema_value())
}

/// Pure schema builder so tests can assert its shape without a server.
pub(crate) fn schema_value() -> Value {
    json!({
        "sliderGroups": {
            "basic": ["exposure", "brightness", "contrast", "highlights", "shadows",
                      "whites", "blacks", { "toneMapper": ["basic", "agx"] }],
            "color": ["temperature", "tint", "saturation", "vibrance", "hue"],
            "details": ["clarity", "dehaze", "structure", "centré", "sharpness",
                        "sharpnessThreshold", "lumaNoiseReduction", "colorNoiseReduction",
                        "chromaticAberrationRedCyan", "chromaticAberrationBlueYellow"],
            "effects": ["glowAmount", "halationAmount", "flareAmount",
                        "grainAmount", "grainRoughness", "grainSize",
                        "vignetteAmount", "vignetteFeather", "vignetteMidpoint", "vignetteRoundness"],
            "geometry": ["rotation", "flipHorizontal", "flipVertical", "orientationSteps",
                         "transformDistortion", "transformVertical", "transformHorizontal",
                         "transformRotate", "transformAspect", "transformScale",
                         "transformXOffset", "transformYOffset"]
        },
        "curves": {
            "mode": ["point", "parametric"],
            "channels": ["luma", "red", "green", "blue"]
        },
        "maskTypes": [
            "linear", "radial", "brush", "color", "luminance", "flow",
            "ai-sky", "ai-subject", "ai-foreground", "ai-depth",
            "clone", "heal"
        ],
        "maskModes": ["additive", "subtractive", "intersect"],
        "notes": [
            "All fields are optional; the backend applies unwrap_or defaults.",
            "AI masks (ai-sky/ai-subject/ai-foreground/ai-depth) need only the 'type' — the backend computes the bitmap.",
            "POST /adjust takes a partial patch that is deep-merged onto the current edits.",
            "POST /preview is render-only (no GUI update, no sidecar). POST /adjust commits + syncs the live GUI."
        ]
    })
}

/// Deep-merge `patch` onto `base` (arrays replaced, objects merged).
fn merge_json(mut base: Value, patch: Value) -> Value {
    match (&mut base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match (b.get(&k).cloned(), v) {
                    (Some(bv), pv @ Value::Object(_)) => {
                        b.insert(k, merge_json(bv, pv));
                    }
                    (_, pv) => {
                        b.insert(k, pv);
                    }
                }
            }
            base
        }
        (_, pv) => pv,
    }
}

// ----------------------------------------------------------------------------

/// Pack and send a `PreviewJob` to the worker, awaiting the rendered bytes.
///
/// Mirrors `apply_adjustments` in `lib.rs` exactly. Returns the JPEG bytes or
/// an error string suitable for an HTTP 500/503 body.
pub(crate) async fn enqueue_preview(
    state: &AppState,
    adjustments: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    roi: Option<(f32, f32, f32, f32)>,
) -> Result<Vec<u8>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let tx_guard = state.preview_worker_tx.lock().unwrap();
        match &*tx_guard {
            Some(worker_tx) => {
                let job = PreviewJob {
                    adjustments,
                    is_interactive,
                    target_resolution,
                    roi,
                    compute_waveform: false,
                    active_waveform_channel: None,
                    force_offscreen: true,
                    responder: tx,
                };
                worker_tx
                    .send(job)
                    .map_err(|e| format!("Failed to send to preview worker: {e}"))?;
            }
            None => return Err("Preview worker not running".to_string()),
        }
    }
    rx.await.map_err(|_| "Superseded or worker failed".to_string())
}

/// Emit a Tauri event to the frontend. Used by `/adjust` and `/mask/*`.
pub(crate) fn emit_event(app_handle: &AppHandle, name: &str, payload: &serde_json::Value) {
    if let Err(e) = app_handle.emit(name, payload.clone()) {
        log::warn!("agent_server: emit `{name}` failed: {e}");
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The router builds and is wired with the documented routes. We can't
    /// easily spin a full Tauri `AppHandle` in a unit test, so we assert the
    /// shape of the pure pieces instead.
    #[test]
    fn port_filename_is_stable() {
        assert_eq!(PORT_FILENAME, "rapidraw-agent-port");
    }

    #[test]
    fn health_payload_shape() {
        // The /health contract is `{ status: "ok", version: <string> }`.
        let body = health_body("9.9.9");
        let value = body.0;
        assert_eq!(value["status"], "ok");
        assert_eq!(value["version"], "9.9.9");
    }

    /// Spin a real loopback server on an ephemeral port and curl `/health`
    /// end-to-end. This validates binding, routing, and JSON response without
    /// touching Tauri state (uses the pure `health_body` helper).
    #[tokio::test]
    async fn health_route_serves_over_tcp() {
        async fn test_health() -> impl IntoResponse {
            health_body("test-1.0.0")
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Stateless router: no AppHandle needed for /health's pure path.
        let app: Router = Router::new().route("/health", get(test_health));

        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // Give the spawned task a moment to start accepting.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let body = tokio::time::timeout(
            Duration::from_secs(2),
            reqwest::get(format!("http://{addr}/health")),
        )
        .await
        .expect("health request timed out")
        .expect("health request failed")
        .json::<serde_json::Value>()
        .await
        .expect("health body was not JSON");

        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], "test-1.0.0");
    }
}
