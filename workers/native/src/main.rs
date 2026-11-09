use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod wgc;
#[cfg(windows)]
mod windows;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    #[cfg(target_os = "linux")]
    let mut desktop = linux::Desktop::new();
    #[cfg(windows)]
    let mut desktop = windows::Desktop::new()?;
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        anyhow::bail!("Use the bundled Swift helper on macOS");
    }
    #[cfg(any(target_os = "linux", windows))]
    let mut input = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    #[cfg(any(target_os = "linux", windows))]
    let mut output = tokio::io::stdout();
    #[cfg(any(target_os = "linux", windows))]
    while let Some(line) = input.next_line().await? {
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let id = request["id"].clone();
                match desktop
                    .handle(request["method"].as_str().unwrap_or(""), &request["params"])
                    .await
                {
                    Ok(result) => json!({"id":id,"result":result}),
                    Err(error) => json!({"id":id,"error":{"message":format!("{error:#}")}}),
                }
            }
            Err(error) => json!({"id":null,"error":{"message":error.to_string()}}),
        };
        output.write_all(format!("{response}\n").as_bytes()).await?;
        output.flush().await?;
    }
    Ok(())
}

pub fn save_png(bytes: Vec<u8>, width: u32, height: u32, snapshot_id: &str) -> Result<Value> {
    use base64::Engine;
    let directory = std::env::var_os("HARNESS_ARTIFACT_DIR")
        .ok_or_else(|| anyhow::anyhow!("HARNESS_ARTIFACT_DIR is required"))?;
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory)?;
    let artifact = directory.join(format!("desktop-{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&artifact, &bytes)?;
    Ok(
        json!({"snapshot_id":snapshot_id,"artifact":artifact,"width":width,"height":height,"content":[{"type":"text","text":"Real native desktop screenshot"},{"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes)}]}),
    )
}
