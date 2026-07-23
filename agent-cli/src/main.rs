//! agent-cli — a CLI for the RapidRAW agentic control server.
//!
//! Wraps the loopback HTTP API into ergonomic, fully-typed subcommands so you
//! can drive RapidRAW from the terminal: load, preview, adjust (live GUI sync),
//! masks, state, export. See AGENTIC_INTERFACE_PLAN.md for the server side.

mod adjustments;
mod client;
mod discover;

use std::io::Write;

use adjustments::{AdjustmentsFlags, AdjustmentsPatch, ExtraAdjustments};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use client::Client;

#[derive(Parser)]
#[command(
    name = "agent-cli",
    version,
    about = "Drive RapidRAW side-by-side with a human via its agent control server",
    long_about = "Wrap the RapidRAW loopback HTTP API. Run `agent-cli health` first to confirm RapidRAW is running.",
)]
#[clap(allow_negative_numbers = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the server is up and report its version.
    Health,

    /// Discover and print the port the server is listening on.
    Port,

    /// Load an image (lossless decode). By default also navigates the live GUI
    /// to it so you and the agent work on the same image.
    Load {
        /// Path to the image file.
        path: String,
        /// Don't navigate the GUI; load into the backend only.
        #[arg(long)]
        no_navigate: bool,
    },

    /// Render a JPEG of the **current** committed look (sliders + masks as live).
    /// No adjustment flags — to change the look, `adjust` first, then `preview` again.
    Preview {
        /// Max output resolution (long edge).
        #[arg(long)]
        resolution: Option<u32>,
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        out: Option<String>,
    },

    /// Commit adjustments — live-updates the GUI and writes the .rrdata sidecar.
    /// Only the flags you pass are sent (deep-merged onto current edits).
    Adjust {
        #[command(flatten)]
        flags: AdjustmentsFlags,
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },

    /// Print the current merged human+agent state (image info + adjustments).
    #[command(visible_alias = "st")]
    State {
        /// Print only the adjustments object, pretty.
        #[arg(long)]
        adjustments_only: bool,
    },

    /// Print the slider/mask schema the agent can use.
    Schema,

    /// Export a JPEG of the **current** committed look (same as preview, for a file).
    /// No adjustment flags — `adjust` first if you want a different look.
    Export {
        #[arg(long)]
        resolution: Option<u32>,
        /// Output file path.
        #[arg(short, long)]
        out: String,
    },

    /// Mask operations: add / update / remove / list.
    #[command(subcommand)]
    Mask(MaskCmd),

    /// AI/BM3D denoise (writes *_Denoised.tiff/png next to the source).
    Denoise {
        path: String,
        #[arg(long, default_value_t = 0.5)]
        intensity: f32,
        /// "ai" or "bm3d".
        #[arg(long, default_value = "ai")]
        method: String,
    },

    /// Merge bracketed exposures into an HDR image.
    Hdr {
        /// Input paths (2+).
        paths: Vec<String>,
    },

    /// Stitch overlapping images into a panorama.
    Panorama {
        /// Input paths (2+).
        paths: Vec<String>,
    },

    /// Invert film-scan negatives.
    Negative {
        /// Input paths.
        paths: Vec<String>,
        #[arg(long, default_value_t = 1.0)]
        red_weight: f32,
        #[arg(long, default_value_t = 1.0)]
        green_weight: f32,
        #[arg(long, default_value_t = 1.0)]
        blue_weight: f32,
        #[arg(long, default_value_t = 0.0)]
        exposure: f32,
        #[arg(long, default_value_t = 1.0)]
        contrast: f32,
    },

    /// Analyze a batch for blurry / similar images (culling suggestions).
    Cull {
        paths: Vec<String>,
        #[arg(long, default_value_t = 5)]
        similarity_threshold: u32,
        #[arg(long, default_value_t = 10.0)]
        blur_threshold: f64,
        #[arg(long, default_value_t = true)]
        group_similar: bool,
        #[arg(long, default_value_t = true)]
        filter_blurry: bool,
    },

    /// Generative fill / inpaint a masked region (needs AI connector or LAMA).
    Inpaint {
        path: String,
        /// Patch definition JSON (AiPatchDefinition).
        #[arg(long, value_name = "JSON")]
        patch: String,
        /// Current adjustments JSON (optional).
        #[arg(long, value_name = "JSON")]
        adjustments: Option<String>,
        /// Prefer the local fast LAMA model.
        #[arg(long)]
        fast: bool,
    },

    /// Suggest exposure/WB/etc from the current image histogram.
    AutoAdjust,

    /// Lensfun lookups.
    #[command(subcommand)]
    Lens(LensCmd),

    /// List installed LUTs.
    Luts,

    /// List saved user presets.
    Presets,

    /// List images in a folder (filmstrip source). Default: folder of current image.
    #[command(visible_alias = "list")]
    Ls {
        /// Directory to list. Omit to use the currently loaded image's folder.
        dir: Option<String>,
        /// Print full JSON instead of a compact path list.
        #[arg(long)]
        json: bool,
    },

    /// Open the next image in the current folder filmstrip.
    Next {
        #[arg(long)]
        wrap: bool,
    },

    /// Open the previous image in the current folder filmstrip.
    Prev {
        #[arg(long)]
        wrap: bool,
    },

    /// Set star rating 0–5 on the current image (or --path).
    Rate {
        /// Stars 0–5.
        rating: u8,
        #[arg(long)]
        path: Option<String>,
    },

    /// Set or clear a color label on the current image (or --path).
    Label {
        /// Color name, or "clear" / "none" to remove.
        color: String,
        #[arg(long)]
        path: Option<String>,
    },
}

#[derive(Subcommand)]
enum LensCmd {
    /// List lens makers in the Lensfun database.
    Makers,
    /// Autodetect the best lens match for EXIF maker/model.
    Autodetect {
        maker: String,
        model: String,
    },
}

#[derive(Subcommand)]
enum MaskCmd {
    /// Add a mask. AI types (ai-sky, ai-subject, ai-foreground, ai-depth) need
    /// only the type; geometric types (radial, linear, brush, color, luminance)
    /// take parameters via --json.
    Add {
        /// Mask type, e.g. ai-sky, radial, linear, brush, color, luminance, ai-subject.
        mask_type: String,
        #[command(flatten)]
        flags: AdjustmentsFlags,
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        opacity: Option<f32>,
        #[arg(long)]
        invert: bool,
        /// Sub-mask mode: additive | subtractive | intersect.
        #[arg(long, default_value = "additive")]
        mode: String,
        /// For ai-subject: bounding box x1,y1,x2,y2 in image pixels.
        #[arg(long, value_name = "x1,y1,x2,y2")]
        r#box: Option<String>,
        /// Skip server-side ONNX generation for ai-* types.
        #[arg(long)]
        skip_generate: bool,
    },
    /// Update a mask by id (patch fields via --json).
    Update {
        id: String,
        #[arg(long, value_name = "JSON", required = true)]
        json: String,
    },
    /// Remove a mask by id.
    Remove { id: String },
    /// List current masks.
    List,
}

/// Build a typed patch from flags + optional `--json`. Errors up front if the
/// JSON is malformed.
fn build_patch(flags: AdjustmentsFlags, json: Option<String>) -> Result<AdjustmentsPatch> {
    match json {
        Some(s) => {
            let extra: ExtraAdjustments =
                serde_json::from_str(&s).context("invalid --json (expected a JSON object)")?;
            Ok(flags.merge_json(extra))
        }
        None => Ok(flags.into_patch()),
    }
}

/// True if the patch is empty (nothing to send).
fn patch_is_empty(patch: &AdjustmentsPatch) -> bool {
    patch.fields.is_empty()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Port => match discover::read_port() {
            Some(p) => {
                println!("{p}");
                Ok(())
            }
            None => bail!("Port file not found. Is RapidRAW running?"),
        },

        // Health uses the blocking discover() path internally; keep it simple.
        Command::Health => {
            let c = Client::discover().await?;
            let h = c.health().await?;
            println!("{}", serde_json::to_string_pretty(&h)?);
            Ok(())
        }

        Command::Load { path, no_navigate } => {
            let c = Client::discover().await?;
            let r = c.load(&path, !no_navigate).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Preview { resolution, out } => {
            let c = Client::discover().await?;
            let bytes = c.preview(resolution, None).await?;
            write_bytes(&bytes, out.as_deref())?;
            Ok(())
        }

        Command::Adjust { flags, json } => {
            let c = Client::discover().await?;
            let patch = build_patch(flags, json)?;
            if patch_is_empty(&patch) {
                bail!("No adjustments given. Pass flags like --exposure 0.5 or --json '{{...}}'.");
            }
            let r = c.adjust(patch).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::State { adjustments_only } => {
            let c = Client::discover().await?;
            let s = c.state().await?;
            if adjustments_only {
                println!("{}", serde_json::to_string_pretty(&s.adjustments)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&s)?);
            }
            Ok(())
        }

        Command::Schema => {
            let c = Client::discover().await?;
            let s = c.schema().await?;
            println!("{}", serde_json::to_string_pretty(&s)?);
            Ok(())
        }

        Command::Export { resolution, out } => {
            let c = Client::discover().await?;
            let bytes = c.export(resolution, Some(&out)).await?;
            eprintln!("wrote {} bytes to {out}", bytes.len());
            Ok(())
        }

        Command::Mask(mask) => match mask {
            MaskCmd::Add {
                mask_type,
                flags,
                json,
                name,
                opacity,
                invert,
                mode,
                r#box,
                skip_generate,
            } => {
                let c = Client::discover().await?;
                let patch = build_patch(flags, json)?;
                let mut body = serde_json::json!({
                    "type": mask_type,
                    "adjustments": patch,
                    "mode": mode,
                    "invert": invert,
                    "skipGenerate": skip_generate,
                });
                if let Some(n) = name {
                    body["name"] = serde_json::json!(n);
                }
                if let Some(o) = opacity {
                    body["opacity"] = serde_json::json!(o);
                }
                if let Some(b) = r#box {
                    let parts: Vec<f64> = b
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    if parts.len() != 4 {
                        bail!("--box must be x1,y1,x2,y2");
                    }
                    body["box"] = serde_json::json!([parts[0], parts[1], parts[2], parts[3]]);
                }
                let r = c.mask_add_full(body).await?;
                println!("{}", serde_json::to_string_pretty(&r)?);
                Ok(())
            }
            MaskCmd::Update { id, json } => {
                let c = Client::discover().await?;
                let patch: serde_json::Value =
                    serde_json::from_str(&json).context("invalid --json")?;
                let r = c.mask_update(&id, patch).await?;
                println!("{}", serde_json::to_string_pretty(&r)?);
                Ok(())
            }
            MaskCmd::Remove { id } => {
                let c = Client::discover().await?;
                let r = c.mask_remove(&id).await?;
                println!("{}", serde_json::to_string_pretty(&r)?);
                Ok(())
            }
            MaskCmd::List => {
                let c = Client::discover().await?;
                let s = c.state().await?;
                let masks = s.adjustments.get("masks").and_then(|m| m.as_array());
                match masks {
                    Some(arr) if !arr.is_empty() => {
                        for m in arr {
                            println!(
                                "{}\t{}\topacity={}\tsubmasks={}",
                                m["id"].as_str().unwrap_or("?"),
                                m["name"].as_str().unwrap_or("?"),
                                m["opacity"].as_f64().unwrap_or(1.0),
                                m["subMasks"].as_array().map(|a| a.len()).unwrap_or(0),
                            );
                        }
                        Ok(())
                    }
                    _ => {
                        println!("(no masks)");
                        Ok(())
                    }
                }
            }
        },

        Command::Denoise { path, intensity, method } => {
            let c = Client::discover().await?;
            let r = c.denoise(&path, intensity, &method).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Hdr { paths } => {
            let c = Client::discover().await?;
            let r = c.hdr_merge(&paths).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Panorama { paths } => {
            let c = Client::discover().await?;
            let r = c.panorama_stitch(&paths).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Negative {
            paths,
            red_weight,
            green_weight,
            blue_weight,
            exposure,
            contrast,
        } => {
            let c = Client::discover().await?;
            let r = c
                .negative_convert(
                    &paths,
                    red_weight,
                    green_weight,
                    blue_weight,
                    exposure,
                    contrast,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Cull {
            paths,
            similarity_threshold,
            blur_threshold,
            group_similar,
            filter_blurry,
        } => {
            let c = Client::discover().await?;
            let r = c
                .cull(
                    &paths,
                    similarity_threshold,
                    blur_threshold,
                    group_similar,
                    filter_blurry,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Inpaint {
            path,
            patch,
            adjustments,
            fast,
        } => {
            let c = Client::discover().await?;
            let patch_def: serde_json::Value =
                serde_json::from_str(&patch).context("invalid --patch JSON")?;
            let adj: serde_json::Value = match adjustments {
                Some(s) => serde_json::from_str(&s).context("invalid --adjustments JSON")?,
                None => serde_json::json!({}),
            };
            let r = c.inpaint(&path, patch_def, adj, fast).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::AutoAdjust => {
            let c = Client::discover().await?;
            let r = c.auto_adjust().await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Lens(cmd) => match cmd {
            LensCmd::Makers => {
                let c = Client::discover().await?;
                let r = c.lens_makers().await?;
                println!("{}", serde_json::to_string_pretty(&r)?);
                Ok(())
            }
            LensCmd::Autodetect { maker, model } => {
                let c = Client::discover().await?;
                let r = c.lens_autodetect(&maker, &model).await?;
                println!("{}", serde_json::to_string_pretty(&r)?);
                Ok(())
            }
        },

        Command::Luts => {
            let c = Client::discover().await?;
            let r = c.luts().await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Presets => {
            let c = Client::discover().await?;
            let r = c.presets().await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Ls { dir, json } => {
            let c = Client::discover().await?;
            let r = c.images(dir.as_deref()).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                let dir_s = r["dir"].as_str().unwrap_or("?");
                let count = r["count"].as_u64().unwrap_or(0);
                let cur_idx = r["currentIndex"].as_u64();
                println!("# dir={dir_s}  count={count}  currentIndex={cur_idx:?}");
                if let Some(arr) = r["images"].as_array() {
                    for (i, img) in arr.iter().enumerate() {
                        let path = img["path"].as_str().unwrap_or("?");
                        let rating = img["rating"].as_u64().unwrap_or(0);
                        let edited = img["is_edited"].as_bool().unwrap_or(false);
                        let mark = if cur_idx == Some(i as u64) { ">" } else { " " };
                        let edit = if edited { "*" } else { " " };
                        println!("{mark}{i:4}  ★{rating} {edit}  {path}");
                    }
                }
            }
            Ok(())
        }

        Command::Next { wrap } => {
            let c = Client::discover().await?;
            let r = c.navigate("next", wrap).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Prev { wrap } => {
            let c = Client::discover().await?;
            let r = c.navigate("prev", wrap).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Rate { rating, path } => {
            if rating > 5 {
                bail!("rating must be 0–5");
            }
            let c = Client::discover().await?;
            let paths = path.map(|p| vec![p]);
            let r = c.set_rating(rating, paths).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }

        Command::Label { color, path } => {
            let c = Client::discover().await?;
            let paths = path.map(|p| vec![p]);
            let color_opt = match color.to_lowercase().as_str() {
                "clear" | "none" | "" => None,
                other => Some(other.to_string()),
            };
            let r = c
                .set_color_label(color_opt.as_deref(), paths)
                .await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }
    }
}

/// Write bytes to a file path, or to stdout if `out` is None.
fn write_bytes(bytes: &[u8], out: Option<&str>) -> Result<()> {
    match out {
        Some(path) => {
            std::fs::write(path, bytes).with_context(|| format!("write {path}"))?;
            eprintln!("wrote {} bytes to {path}", bytes.len());
        }
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(bytes)?;
        }
    }
    Ok(())
}
