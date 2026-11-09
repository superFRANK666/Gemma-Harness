use anyhow::Result;
use harness_core::Harness;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn interrupt_during_unresponsive_mcp_initialize_does_not_wait_for_server_timeout(
) -> Result<()> {
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let marker = workspace.path().join("mcp-started.txt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..n]);
            if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let length: usize = headers
            .lines()
            .filter_map(|s| s.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        while request.len() < header_end + length {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..n]);
        }
        let event = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"search","type":"function","function":{"name":"tool_search","arguments":"{\"query\":\"mcp\"}"}}]},"finish_reason":"tool_calls"}]});
        let body = format!("data: {event}\n\ndata: [DONE]\n\n");
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let harness = Harness::open(data.path().into())?;
    let mut snapshot = harness.command(json!({"action":"snapshot"})).await?;
    snapshot["config"]["workspace"] = json!(workspace.path());
    snapshot["config"]["connections"][0]["model"] = json!("explicit-scripted-test-fixture");
    snapshot["config"]["connections"][0]["baseUrl"] = json!(url);
    snapshot["config"]["mcpServers"] = json!([{"id":"unresponsive-fixture","name":"Explicit MCP timeout fixture","command":"node","args":["-e","require('node:fs').writeFileSync(process.argv[1],String(process.pid));process.stdin.resume();setTimeout(()=>{},30000)",marker],"enabled":true}]);
    harness
        .command(json!({"action":"save_settings","config":snapshot["config"]}))
        .await?;
    let session = harness.command(json!({"action":"create_session"})).await?;
    let id = session["id"].as_str().unwrap().to_string();
    let running_id = id.clone();
    let h = harness.clone();
    let task = tokio::spawn(async move {
        h.turn(
            running_id,
            "Discover the optional MCP tool".into(),
            Arc::new(|_| {}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await
        }
    })
    .await?;
    assert!(!task.is_finished());
    harness
        .command(json!({"action":"interrupt","sessionId":id}))
        .await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task).await??;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("取消"));
    let stored = harness
        .command(json!({"action":"load_session","sessionId":id}))
        .await?;
    assert!(
        stored["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "message"
                && event["payload"]["content"] == "Discover the optional MCP tool"),
        "cancelled MCP initialization must not lose the user's task"
    );
    Ok(())
}
