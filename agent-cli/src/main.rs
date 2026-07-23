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

    /// Render a preview JPEG (silent: no GUI update, no sidecar). Great for
    /// iterating on values without flickering the window.
    Preview {
        #[command(flatten)]
        flags: AdjustmentsFlags,
        /// Extra adjustments as a JSON object, merged on top of the flags.
        /// e.g. --json '{"hsl":{"reds":{"saturation":20}}}'
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
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

    /// Export a full-quality JPEG for the given (or current) adjustments.
    Export {
        #[command(flatten)]
        flags: AdjustmentsFlags,
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
        #[arg(long)]
        resolution: Option<u32>,
        /// Output file path. Required (the CLI doesn't write image bytes to stdout).
        #[arg(short, long)]
        out: String,
    },

    /// Mask operations: add / update / remove / list.
    #[command(subcommand)]
    Mask(MaskCmd),
}

#[derive(Subcommand)]
enum MaskCmd {
    /// Add a mask. AI types (ai-sky, ai-subject, ai-foreground, ai-depth) need
    /// only the type; geometric types (radial, linear, brush, color, luminance)
    /// take parameters via --json.
    Add {
        /// Mask type, e.g. ai-sky, radial, linear, brush, color, luminance.
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

        Command::Preview { flags, json, resolution, out } => {
            let c = Client::discover().await?;
            let patch = build_patch(flags, json)?;
            let bytes = c.preview(patch, resolution, None).await?;
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

        Command::Export { flags, json, resolution, out } => {
            let c = Client::discover().await?;
            let patch = build_patch(flags, json)?;
            let bytes = c.export(patch, resolution, Some(&out)).await?;
            eprintln!("wrote {} bytes to {out}", bytes.len());
            Ok(())
        }

        Command::Mask(mask) => match mask {
            MaskCmd::Add { mask_type, flags, json, name, opacity, invert } => {
                let c = Client::discover().await?;
                let patch = build_patch(flags, json)?;
                let r = c
                    .mask_add(
                        &mask_type,
                        patch,
                        name.as_deref(),
                        opacity,
                        if invert { Some(true) } else { None },
                    )
                    .await?;
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
