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
        context_length: std::env::var("HARNESS_EVAL_CONTEXT")
            .ok()
            .map(|s| s.parse::<usize>())
            .transpose()?
            .unwrap_or(8192),
        max_output_tokens: std::env::var("HARNESS_EVAL_MAX_OUTPUT")
            .ok()
            .map(|s| s.parse::<usize>())
            .transpose()?,
        temperature: std::env::var("HARNESS_EVAL_TEMPERATURE")
            .ok()
            .map(|s| s.parse::<f64>())
            .transpose()?,
        top_p: std::env::var("HARNESS_EVAL_TOP_P")
            .ok()
            .map(|s| s.parse::<f64>())
            .transpose()?,
        top_k: std::env::var("HARNESS_EVAL_TOP_K")
            .ok()
            .map(|s| s.parse::<u32>())
            .transpose()?,
        min_p: std::env::var("HARNESS_EVAL_MIN_P")
            .ok()
            .map(|s| s.parse::<f64>())
            .transpose()?,
        seed: std::env::var("HARNESS_EVAL_SEED")
            .ok()
            .map(|s| s.parse::<u32>())
            .transpose()?,
        api_key: std::env::var("HARNESS_EVAL_API_KEY").ok(),
        ..Default::default()
    };
    let diagnosis = Provider::new(config)?.diagnose().await?;
    println!("{}", serde_json::to_string_pretty(&diagnosis)?);
    Ok(())
}
