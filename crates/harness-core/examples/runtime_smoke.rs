//! Downloads only the pinned inference executable, never model weights.
use anyhow::Result;
use harness_core::engine::Engine;
#[tokio::main]
async fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gemma-harness-runtime-smoke"));
    let backend = std::env::args().nth(2).unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "metal".into()
        } else {
            "cpu".into()
        }
    });
    let result = Engine::new(dir).install(&backend).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
