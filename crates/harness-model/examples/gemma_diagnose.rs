//! Run the same harmless tool and visual probes used by connection diagnostics.
//! Set HARNESS_EVAL_URL and HARNESS_EVAL_MODEL; see docs/gemma-adaptation.md.
use anyhow::{Context, Result};
use harness_model::{ConnectionConfig, Provider};

#[tokio::main]
async fn main() -> Result<()> {
    let config = ConnectionConfig {
        base_url: std::env::var("HARNESS_EVAL_URL").context("Set HARNESS_EVAL_URL")?,
        model: std::env::var("HARNESS_EVAL_MODEL").context("Set HARNESS_EVAL_MODEL")?,
        backend: std::env::var("HARNESS_EVAL_BACKEND").unwrap_or_else(|_| "auto".into()),
        model_family: std::env::var("HARNESS_EVAL_FAMILY").unwrap_or_else(|_| "auto".into()),
        thinking_mode: std::env::var("HARNESS_EVAL_THINKING").unwrap_or_else(|_| "auto".into()),
        api_key: std::env::var("HARNESS_EVAL_API_KEY").ok(),
        ..Default::default()
    };
    let diagnosis = Provider::new(config)?.diagnose().await?;
    println!("{}", serde_json::to_string_pretty(&diagnosis)?);
    Ok(())
}
