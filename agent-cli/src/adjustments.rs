//! Adjustments: typed slider flags -> serialized JSON.
//!
//! Each slider is an `Option<T>` flag. We serialize with `skip_serializing_if =
//! "Option::is_none"`, so only flags the user actually passed are emitted —
//! `agent-cli adjust --exposure 0.5` sends just `{"exposure":0.5}`, no risk of
//! wiping other edits. The server deep-merges the patch onto the current store.
//!
//! This mirrors the authoritative schema in
//! `src/utils/adjustments.ts` (`INITIAL_ADJUSTMENTS`) — camelCase field names,
//! Rust snake_case flag names mapped via `#[serde(rename)]`.

use serde::Serialize;

/// All agent-controllable scalar sliders as optional flags.
/// (Curves, HSL, color grading are nested objects — leave those to the `--json`
/// escape hatch in main.rs; this struct covers the common 80%.)
#[derive(clap::Args, Debug, Default, Clone, Serialize)]
#[command(allow_negative_numbers = true)]
pub struct AdjustmentsFlags {
    // --- basic ---
    /// Exposure, typically -4..4.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub exposure: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub brightness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub contrast: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub highlights: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub shadows: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub whites: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub blacks: Option<f64>,
    /// Tone mapper: "basic" or "agx".
    #[serde(rename = "toneMapper", skip_serializing_if = "Option::is_none")]
    #[arg(long = "tone-mapper", value_name = "basic|agx")]
    pub tone_mapper: Option<String>,

    // --- color ---
    /// White balance temperature, typically -100..100.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub tint: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub saturation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub vibrance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub hue: Option<f64>,

    // --- details ---
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub clarity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub dehaze: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub structure: Option<f64>,
    /// Centre (detail). serde name keeps the accented key the backend expects.
    #[serde(rename = "centré", skip_serializing_if = "Option::is_none")]
    #[arg(long = "centre")]
    pub centre: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub sharpness: Option<f64>,
    #[serde(rename = "sharpnessThreshold", skip_serializing_if = "Option::is_none")]
    #[arg(long = "sharpness-threshold")]
    pub sharpness_threshold: Option<f64>,
    #[serde(rename = "lumaNoiseReduction", skip_serializing_if = "Option::is_none")]
    #[arg(long = "luma-noise-reduction")]
    pub luma_noise_reduction: Option<f64>,
    #[serde(rename = "colorNoiseReduction", skip_serializing_if = "Option::is_none")]
    #[arg(long = "color-noise-reduction")]
    pub color_noise_reduction: Option<f64>,

    // --- effects ---
    #[serde(rename = "glowAmount", skip_serializing_if = "Option::is_none")]
    #[arg(long = "glow-amount")]
    pub glow_amount: Option<f64>,
    #[serde(rename = "halationAmount", skip_serializing_if = "Option::is_none")]
    #[arg(long = "halation-amount")]
    pub halation_amount: Option<f64>,
    #[serde(rename = "flareAmount", skip_serializing_if = "Option::is_none")]
    #[arg(long = "flare-amount")]
    pub flare_amount: Option<f64>,
    #[serde(rename = "grainAmount", skip_serializing_if = "Option::is_none")]
    #[arg(long = "grain-amount")]
    pub grain_amount: Option<f64>,
    #[serde(rename = "vignetteAmount", skip_serializing_if = "Option::is_none")]
    #[arg(long = "vignette-amount")]
    pub vignette_amount: Option<f64>,

    // --- geometry ---
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub rotation: Option<f64>,
    /// Flip horizontal. `--flip-horizontal` alone means true; `=false` to clear.
    #[serde(rename = "flipHorizontal", skip_serializing_if = "Option::is_none")]
    #[arg(long = "flip-horizontal", num_args = 0..=1, default_missing_value = "true")]
    pub flip_horizontal: Option<bool>,
    /// Flip vertical. Same convention as flip-horizontal.
    #[serde(rename = "flipVertical", skip_serializing_if = "Option::is_none")]
    #[arg(long = "flip-vertical", num_args = 0..=1, default_missing_value = "true")]
    pub flip_vertical: Option<bool>,
}

/// A nested adjustments patch supplied via `--json`, e.g. for HSL / curves /
/// color grading. Parsed from a JSON string into a typed map so we get a parse
/// error up front rather than at the server.
pub type ExtraAdjustments = serde_json::Map<String, serde_json::Value>;

impl AdjustmentsFlags {
    /// Merge an `ExtraAdjustments` map on top of the serialized flags. Flag
    /// values win on conflict only if the key collides AND the flag was set;
    /// in practice `--json` keys are nested objects the flags don't touch.
    pub fn merge_json(self, extra: ExtraAdjustments) -> AdjustmentsPatch {
        // Serialize self (skips None), then parse to a map so we can merge.
        let mut mine: serde_json::Map<String, serde_json::Value> =
            serde_json::to_value(&self)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();

        for (k, v) in extra {
            mine.insert(k, v);
        }
        AdjustmentsPatch { fields: mine }
    }

    pub fn into_patch(self) -> AdjustmentsPatch {
        let fields = serde_json::to_value(&self)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        AdjustmentsPatch { fields }
    }
}

/// A finalized adjustments patch ready to send. Guaranteed to contain only
/// fields the user expressed (via flags or `--json`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct AdjustmentsPatch {
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_set_flags_are_emitted() {
        let f = AdjustmentsFlags {
            exposure: Some(0.5),
            temperature: Some(15.0),
            ..Default::default()
        };
        let v = serde_json::to_value(&f.into_patch()).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 2, "only 2 flags were set");
        assert_eq!(obj["exposure"], 0.5);
        assert_eq!(obj["temperature"], 15.0);
    }

    #[test]
    fn camel_case_keys() {
        let f = AdjustmentsFlags {
            luma_noise_reduction: Some(10.0),
            sharpness_threshold: Some(20.0),
            ..Default::default()
        };
        let v = serde_json::to_value(&f.into_patch()).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("lumaNoiseReduction"));
        assert!(obj.contains_key("sharpnessThreshold"));
        assert!(!obj.contains_key("luma_noise_reduction"));
    }

    #[test]
    fn accentuated_key_preserved() {
        // The backend reads adj["centré"]; make sure serde rename survives.
        let f = AdjustmentsFlags {
            centre: Some(5.0),
            ..Default::default()
        };
        let v = serde_json::to_value(&f.into_patch()).unwrap();
        assert_eq!(v["centré"], 5.0);
        assert!(v.get("centre").is_none());
    }

    #[test]
    fn empty_flags_emit_empty_object() {
        let f = AdjustmentsFlags::default();
        let v = serde_json::to_value(&f.into_patch()).unwrap();
        assert!(v.as_object().unwrap().is_empty());
    }

    #[test]
    fn merge_json_adds_keys() {
        let f = AdjustmentsFlags {
            exposure: Some(0.5),
            ..Default::default()
        };
        let mut extra = ExtraAdjustments::new();
        extra.insert("vibrance".into(), 10.0.into());
        extra.insert(
            "hsl".into(),
            serde_json::json!({ "reds": { "saturation": 20 } }).into(),
        );
        let merged = serde_json::to_value(f.merge_json(extra)).unwrap();
        let obj = merged.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert_eq!(obj["exposure"], 0.5);
        assert_eq!(obj["vibrance"], 10.0);
        assert_eq!(obj["hsl"]["reds"]["saturation"], 20);
    }

    #[test]
    fn tone_mapper_serializes_as_string() {
        let f = AdjustmentsFlags {
            tone_mapper: Some("agx".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&f.into_patch()).unwrap();
        assert_eq!(v["toneMapper"], "agx");
    }

    #[test]
    fn bool_flags_default_missing_true() {
        // Simulates clap passing Some(true) for a bare --flip-horizontal.
        let f = AdjustmentsFlags {
            flip_horizontal: Some(true),
            ..Default::default()
        };
        let v = serde_json::to_value(&f.into_patch()).unwrap();
        assert_eq!(v["flipHorizontal"], true);
    }
}
