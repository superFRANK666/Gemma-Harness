use anyhow::Result;
use harness_core::Harness;
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test]
async fn manual_terminal_streams_before_exit_and_interrupt_uses_operation_id() -> Result<()> {
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let harness = Harness::open(data.path().into())?;
    let mut snapshot = harness.command(json!({"action":"snapshot"})).await?;
    snapshot["config"]["workspace"] = json!(workspace.path());
    harness
        .command(json!({"action":"save_settings","config":snapshot["config"]}))
        .await?;
    let command = if cfg!(windows) {
        "Write-Output 'live-first-chunk'; Start-Sleep -Seconds 10; Write-Output 'must-not-run'"
    } else {
        "printf 'live-first-chunk\\n'; sleep 10; printf 'must-not-run\\n'"
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let h = harness.clone();
    let task = tokio::spawn(async move {
        h.manual_tool(json!({"operationId":"manual-stream-test","name":"terminal_run","args":{"command":command,"timeout_ms":20000}}),Arc::new(move|e|{let _=tx.send(e);})).await
    });
    let first = tokio::time::timeout(std::time::Duration::from_secs(4), rx.recv())
        .await?
        .unwrap();
    assert_eq!(first["type"], "tool_output");
    assert!(first["text"].as_str().unwrap().contains("live-first-chunk"));
    assert!(!task.is_finished());
    harness
        .command(json!({"action":"interrupt","sessionId":"manual-stream-test"}))
        .await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(4), task).await???;
    assert_eq!(result["cancelled"], true);
    assert!(
        !result.to_string().contains("must-not-run\\n"),
        "unexpected result: {result}"
    );
    Ok(())
}
