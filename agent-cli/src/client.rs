//! Typed HTTP client for the RapidRAW agent control server.
//!
//! Every response is a `#[derive(Deserialize)]` struct — no untyped
//! `serde_json::Value` leaking into the CLI layer. Requests are typed too.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::adjustments::AdjustmentsPatch;

pub struct Client {
    base: String,
    http: reqwest::Client,
}

// ----------------------------------------------------------------------------
// Typed responses
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct Health {
    pub status: String,
    pub version: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadResponse {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub is_raw: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AdjustOk {
    pub ok: bool,
    /// Present when the server could merge onto an existing mirror.
    #[serde(default)]
    pub adjustments: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub is_raw: bool,
    pub adjustments: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskAddResponse {
    pub ok: bool,
    pub mask_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

// ----------------------------------------------------------------------------
// Typed requests
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct LoadRequest<'a> {
    path: &'a str,
    navigate: bool,
}

#[derive(Serialize)]
struct AdjustmentsBody {
    adjustments: AdjustmentsPatch,
}

/// Preview/export never send adjustments — server always uses current state.
#[derive(Serialize, Default)]
struct PreviewBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    target_resolution: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    roi: Option<(f32, f32, f32, f32)>,
}

#[derive(Serialize, Default)]
struct ExportBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    target_resolution: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MaskAddBody<'a> {
    #[serde(rename = "type")]
    mask_type: &'a str,
    adjustments: AdjustmentsPatch,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    opacity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invert: Option<bool>,
}

// ----------------------------------------------------------------------------
// Error from the server (JSON `{ "error": "..." }`)
// ----------------------------------------------------------------------------

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

impl Client {
    /// Discover the running RapidRAW instance and return a client for it.
    /// Fails fast if `/health` doesn't respond.
    ///
    /// Async because callers run inside the tokio main runtime; use
    /// `discover_blocking()` from a non-async context.
    pub async fn discover() -> Result<Self> {
        let port = crate::discover::read_port_wait(10).context(
            "Could not find RapidRAW agent port. Is RapidRAW running?",
        )?;
        let client = Self {
            base: format!("http://127.0.0.1:{port}"),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
        };
        // Fail fast with a typed health check.
        let _h = client.health().await.context("RapidRAW server not responding at /health")?;
        Ok(client)
    }

    /// Blocking discover for non-async contexts (not used by main, which is async).
    #[allow(dead_code)]
    pub fn discover_blocking() -> Result<Self> {
        let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
        rt.block_on(Self::discover())
    }

    /// Build a client against an explicit port (useful for tests / debugging).
    pub fn with_port(port: u16) -> Self {
        Self {
            base: format!("http://127.0.0.1:{port}"),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client build"),
        }
    }

    async fn decode_error(status: reqwest::StatusCode, body: String) -> anyhow::Error {
        if let Ok(e) = serde_json::from_str::<ErrorBody>(&body) {
            return anyhow::anyhow!("HTTP {status}: {}", e.error);
        }
        anyhow::anyhow!("HTTP {status}: {body}")
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.http.get(format!("{}{path}", self.base)).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.json().await?)
        } else {
            let body = resp.text().await?;
            Err(Self::decode_error(status, body).await)
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.json().await?)
        } else {
            let text = resp.text().await?;
            Err(Self::decode_error(status, text).await)
        }
    }

    async fn post_bytes<B: Serialize>(&self, path: &str, body: &B) -> Result<Vec<u8>> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?.to_vec();
        if status.is_success() {
            Ok(bytes)
        } else {
            let text = String::from_utf8_lossy(&bytes).to_string();
            Err(Self::decode_error(status, text).await)
        }
    }

    async fn delete<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.http.delete(format!("{}{path}", self.base)).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.json().await?)
        } else {
            let body = resp.text().await?;
            Err(Self::decode_error(status, body).await)
        }
    }

    // ---- routes (fully typed) ----

    pub async fn health(&self) -> Result<Health> {
        self.get::<Health>("/health").await
    }

    pub async fn load(&self, path: &str, navigate: bool) -> Result<LoadResponse> {
        self.post_json("/load", &LoadRequest { path, navigate }).await
    }

    /// Render a JPEG of the **current** committed look (server-side mirror).
    /// No what-if patch — call `adjust` first, then `preview` again.
    pub async fn preview(
        &self,
        resolution: Option<u32>,
        roi: Option<(f32, f32, f32, f32)>,
    ) -> Result<Vec<u8>> {
        self.post_bytes(
            "/preview",
            &PreviewBody {
                target_resolution: resolution,
                roi,
            },
        )
        .await
    }

    pub async fn adjust(&self, adjustments: AdjustmentsPatch) -> Result<AdjustOk> {
        self.post_json("/adjust", &AdjustmentsBody { adjustments }).await
    }

    pub async fn state(&self) -> Result<State> {
        self.get::<State>("/state").await
    }

    pub async fn schema(&self) -> Result<serde_json::Value> {
        // Schema is a free-form descriptor; Value is acceptable here.
        self.get::<serde_json::Value>("/schema").await
    }

    /// Export a JPEG of the **current** committed look.
    pub async fn export(
        &self,
        resolution: Option<u32>,
        out_path: Option<&str>,
    ) -> Result<Vec<u8>> {
        self.post_bytes(
            "/export",
            &ExportBody {
                target_resolution: resolution,
                path: out_path.map(String::from),
            },
        )
        .await
    }

    pub async fn mask_add(
        &self,
        mask_type: &str,
        adjustments: AdjustmentsPatch,
        name: Option<&str>,
        opacity: Option<f32>,
        invert: Option<bool>,
    ) -> Result<MaskAddResponse> {
        self.post_json(
            "/mask/add",
            &MaskAddBody {
                mask_type,
                adjustments,
                name,
                opacity,
                invert,
            },
        )
        .await
    }

    pub async fn mask_update(&self, id: &str, patch: serde_json::Value) -> Result<OkResponse> {
        self.post_json(&format!("/mask/{id}"), &patch).await
    }

    pub async fn mask_remove(&self, id: &str) -> Result<OkResponse> {
        self.delete::<OkResponse>(&format!("/mask/{id}")).await
    }

    // ---- pipeline tools / lookups (free-form JSON responses) ----

    pub async fn denoise(&self, path: &str, intensity: f32, method: &str) -> Result<serde_json::Value> {
        self.post_json(
            "/denoise",
            &serde_json::json!({ "path": path, "intensity": intensity, "method": method }),
        )
        .await
    }

    pub async fn hdr_merge(&self, paths: &[String]) -> Result<serde_json::Value> {
        self.post_json("/hdr/merge", &serde_json::json!({ "paths": paths })).await
    }

    pub async fn panorama_stitch(&self, paths: &[String]) -> Result<serde_json::Value> {
        self.post_json("/panorama/stitch", &serde_json::json!({ "paths": paths })).await
    }

    pub async fn negative_convert(
        &self,
        paths: &[String],
        red_weight: f32,
        green_weight: f32,
        blue_weight: f32,
        exposure: f32,
        contrast: f32,
    ) -> Result<serde_json::Value> {
        self.post_json(
            "/negative/convert",
            &serde_json::json!({
                "paths": paths,
                "redWeight": red_weight,
                "greenWeight": green_weight,
                "blueWeight": blue_weight,
                "exposure": exposure,
                "contrast": contrast,
            }),
        )
        .await
    }

    pub async fn cull(
        &self,
        paths: &[String],
        similarity_threshold: u32,
        blur_threshold: f64,
        group_similar: bool,
        filter_blurry: bool,
    ) -> Result<serde_json::Value> {
        self.post_json(
            "/cull",
            &serde_json::json!({
                "paths": paths,
                "similarityThreshold": similarity_threshold,
                "blurThreshold": blur_threshold,
                "groupSimilar": group_similar,
                "filterBlurry": filter_blurry,
            }),
        )
        .await
    }

    pub async fn inpaint(
        &self,
        path: &str,
        patch_definition: serde_json::Value,
        current_adjustments: serde_json::Value,
        use_fast_inpaint: bool,
    ) -> Result<serde_json::Value> {
        self.post_json(
            "/inpaint",
            &serde_json::json!({
                "path": path,
                "patch_definition": patch_definition,
                "current_adjustments": current_adjustments,
                "use_fast_inpaint": use_fast_inpaint,
            }),
        )
        .await
    }

    pub async fn auto_adjust(&self) -> Result<serde_json::Value> {
        self.get("/auto-adjust").await
    }

    pub async fn lens_makers(&self) -> Result<serde_json::Value> {
        self.get("/lens/makers").await
    }

    pub async fn lens_autodetect(&self, maker: &str, model: &str) -> Result<serde_json::Value> {
        self.post_json(
            "/lens/autodetect",
            &serde_json::json!({ "maker": maker, "model": model }),
        )
        .await
    }

    pub async fn luts(&self) -> Result<serde_json::Value> {
        self.get("/luts").await
    }

    pub async fn presets(&self) -> Result<serde_json::Value> {
        self.get("/presets").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_port_builds_without_network() {
        // No I/O; confirms the constructor is cheap and panic-free.
        let _c = Client::with_port(12345);
    }

    #[test]
    fn error_body_parses() {
        let e: ErrorBody = serde_json::from_str(r#"{"error":"boom"}"#).unwrap();
        assert_eq!(e.error, "boom");
    }

    #[test]
    fn load_response_camel_case() {
        let j = r#"{"path":"/x.jpg","width":100,"height":200,"isRaw":true}"#;
        let r: LoadResponse = serde_json::from_str(j).unwrap();
        assert_eq!(r.path, "/x.jpg");
        assert_eq!(r.width, 100);
        assert!(r.is_raw);
    }
}
