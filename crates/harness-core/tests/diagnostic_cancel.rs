use anyhow::Result;
use harness_core::Harness;
use serde_json::json;
use std::time::Duration;
use tokio::{io::AsyncReadExt, net::TcpListener};

#[tokio::test]
async fn interrupting_a_stalled_diagnostic_closes_http_and_releases_inference() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let (accepted, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let (closed, mut closures) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let tx = closed.clone();
            accepted.send(()).unwrap();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                while let Ok(n) = socket.read(&mut buffer).await {
                    if n == 0 {
                        break;
                    }
                }
                let _ = tx.send(());
            });
        }
    });
    let data = tempfile::tempdir()?;
    let harness = Harness::open(data.path().into())?;
    let mut config = harness.command(json!({"action":"snapshot"})).await?["config"].clone();
    config["connections"][0]["baseUrl"] = json!(address);
    config["connections"][0]["model"] = json!("explicit-stalled-diagnostic-fixture");
    harness
        .command(json!({"action":"save_settings","config":config}))
        .await?;
    for index in 0..2 {
        let operation = format!("diagnostic-fixture-{index}");
        let h = harness.clone();
        let op = operation.clone();
        let connection = config["connections"][0].clone();
        let task = tokio::spawn(async move {
            h.command(json!({"action":"diagnose","operationId":op,"connection":connection}))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), arrivals.recv())
            .await?
            .unwrap();
        harness
            .command(json!({"action":"interrupt","sessionId":operation}))
            .await?;
        let result = tokio::time::timeout(Duration::from_secs(2), task).await??;
        assert!(result.unwrap_err().to_string().contains("取消"));
        tokio::time::timeout(Duration::from_secs(2), closures.recv())
            .await?
            .unwrap();
        assert!(
            harness.command(json!({"action":"snapshot"})).await?["activeSessions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    server.await?;
    harness.shutdown().await;
    Ok(())
}
