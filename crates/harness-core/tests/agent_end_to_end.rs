//! These tests use a scripted HTTP model but the real Harness, SQLite store,
//! schema validator and filesystem/process executor. They do not certify a GGUF.
use anyhow::{bail, Context, Result};
use harness_core::{store::Store, EventSink, Harness};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

struct MockModel {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    cancellation: CancellationToken,
}

impl MockModel {
    async fn start(
        handler: impl Fn(usize, &Value) -> String + Send + Sync + 'static,
    ) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let cancellation = CancellationToken::new();
        let stop = cancellation.clone();
        let captured = requests.clone();
        let handler = Arc::new(handler);
        let sequence = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    _=stop.cancelled()=>break,
                    accepted=listener.accept()=>{
                        let Ok((socket,_))=accepted else{break};
                        let requests=captured.clone();
                        let handler=handler.clone();
                        let sequence=sequence.clone();
                        tasks.spawn(async move {
                            let _=serve(socket,requests,handler,sequence).await;
                        });
                    },
                    Some(_)=tasks.join_next(),if !tasks.is_empty()=>{}
                }
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self {
            url,
            requests,
            cancellation,
        })
    }

    fn captured(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for MockModel {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

async fn serve(
    mut socket: TcpStream,
    requests: Arc<Mutex<Vec<Value>>>,
    handler: Arc<impl Fn(usize, &Value) -> String>,
    sequence: Arc<AtomicUsize>,
) -> Result<()> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let header_end = loop {
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            bail!("request closed before headers");
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() > 1024 * 1024 {
            bail!("oversized test request headers");
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .context("missing content-length")?
        .1
        .trim()
        .parse::<usize>()?;
    if length > 8 * 1024 * 1024 {
        bail!("oversized test request body");
    }
    while bytes.len() < header_end + length {
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            bail!("request closed before body");
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let request: Value = serde_json::from_slice(&bytes[header_end..header_end + length])?;
    let index = sequence.fetch_add(1, Ordering::SeqCst);
    requests.lock().unwrap().push(request.clone());
    let body = handler(index, &request);
    let header=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
    socket.write_all(header.as_bytes()).await?;
    // Deliberately fragment events across arbitrary byte boundaries as real
    // HTTP streams do, including UTF-8 user strings in tool arguments.
    for fragment in body.as_bytes().chunks(53) {
        socket.write_all(fragment).await?;
    }
    socket.shutdown().await?;
    Ok(())
}

fn sse(values: &[Value], done: bool) -> String {
    let mut output = String::new();
    for value in values {
        output.push_str(&format!("data: {value}\n\n"));
    }
    if done {
        output.push_str("data: [DONE]\n\n");
    }
    output
}

fn function(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

fn calls(mut calls: Vec<Value>) -> String {
    for (index, call) in calls.iter_mut().enumerate() {
        call["index"] = json!(index);
    }
    sse(
        &[
            json!({"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":calls},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ],
        true,
    )
}

fn answer(text: &str) -> String {
    sse(
        &[
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ],
        true,
    )
}

struct Fixture {
    temporary: TempDir,
    workspace: PathBuf,
    data: PathBuf,
    harness: Arc<Harness>,
    session_id: String,
    events: Arc<Mutex<Vec<Value>>>,
}
impl Fixture {
    async fn new(model: &MockModel, context: usize) -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let workspace = temporary.path().join("workspace");
        let data = temporary.path().join("app-data");
        std::fs::create_dir(&workspace)?;
        let harness = Harness::open(data.clone())?;
        harness.command(json!({"action":"save_settings","config":{
            "connections":[{"id":"mock-model","name":"Scripted test model","baseUrl":model.url,"protocol":"chat_completions","model":"test-gemma-fixture","contextLength":context}],
            "selectedConnectionId":"mock-model","workspace":workspace,"deviceId":"local","devices":[],"mcpServers":[],"toolPermissions":{"terminal":true,"browser":false,"desktop":false},"mode":"controller"
        }})).await?;
        let session = harness.command(json!({"action":"create_session"})).await?;
        Ok(Self {
            temporary,
            workspace,
            data,
            harness,
            session_id: session["id"].as_str().context("missing session id")?.into(),
            events: Arc::new(Mutex::new(vec![])),
        })
    }
    fn sink(&self) -> EventSink {
        let events = self.events.clone();
        Arc::new(move |event| events.lock().unwrap().push(event))
    }
    async fn turn(&self, input: &str) -> Result<()> {
        self.harness
            .turn(self.session_id.clone(), input.into(), self.sink())
            .await
    }
    async fn stored(&self) -> Result<Value> {
        self.harness
            .command(json!({"action":"load_session","sessionId":self.session_id}))
            .await
    }
}

fn tool_results(request: &Value) -> Vec<&Value> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect()
}

fn assert_tool_pairs(messages: &[Value]) {
    let mut pending = std::collections::BTreeSet::new();
    for message in messages {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                assert!(pending.insert(call["id"].as_str().unwrap().to_owned()));
            }
        }
        if message["role"] == "tool" {
            assert!(
                pending.remove(message["tool_call_id"].as_str().unwrap()),
                "orphan tool result: {message}"
            );
        }
        if message["role"] == "user" {
            assert!(
                pending.is_empty(),
                "user appeared before missing tool results were repaired"
            );
        }
    }
    assert!(
        pending.is_empty(),
        "request contains incomplete tool-call/result pairs"
    );
}

#[tokio::test]
async fn omitted_edit_version_uses_only_same_turn_read_and_still_checks_staleness() -> Result<()> {
    for scenario in ["same_turn", "stale", "earlier_turn"] {
        let workspace_file = Arc::new(Mutex::new(None::<PathBuf>));
        let path_for_model = workspace_file.clone();
        let model = MockModel::start(move |index, _| {
            if index == 0 {
                calls(vec![function(
                    "read",
                    "file_read",
                    json!({"path":"settings.ini"}),
                )])
            } else if scenario == "earlier_turn" && index == 1 {
                answer("Read complete.")
            } else if index == if scenario == "earlier_turn" { 2 } else { 1 } {
                if scenario == "stale" {
                    std::fs::write(
                        path_for_model.lock().unwrap().as_ref().unwrap(),
                        "mode=external\n",
                    )
                    .unwrap();
                }
                calls(vec![function(
                    "patch",
                    "file_apply_patch",
                    json!({"path":"settings.ini","old_text":"mode=before","new_text":"mode=after"}),
                )])
            } else {
                answer("Finished.")
            }
        })
        .await?;
        let fixture = Fixture::new(&model, 65536).await?;
        let path = fixture.workspace.join("settings.ini");
        std::fs::write(&path, "mode=before\n")?;
        *workspace_file.lock().unwrap() = Some(path.clone());
        fixture.turn("Read the file and change its mode.").await?;
        if scenario == "earlier_turn" {
            fixture.turn("Now apply the change.").await?;
        }
        let events = fixture.events.lock().unwrap();
        let started = events
            .iter()
            .find(|event| event["type"] == "tool_started" && event["name"] == "file_apply_patch")
            .unwrap();
        let completed = events
            .iter()
            .find(|event| event["type"] == "tool_completed" && event["name"] == "file_apply_patch")
            .unwrap();
        match scenario {
            "same_turn" => {
                assert_eq!(started["versionSource"], "same_turn_file_read");
                assert!(started["effectiveArguments"]["read_token"].is_string());
                assert_ne!(completed["result"]["isError"], true);
                assert_eq!(std::fs::read_to_string(&path)?, "mode=after\n");
            }
            "stale" => {
                assert_eq!(started["versionSource"], "same_turn_file_read");
                assert_eq!(completed["result"]["isError"], true);
                assert_eq!(std::fs::read_to_string(&path)?, "mode=external\n");
            }
            _ => {
                assert!(started["versionSource"].is_null());
                assert_eq!(completed["result"]["isError"], true);
                assert_eq!(std::fs::read_to_string(&path)?, "mode=before\n");
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn artifact_pages_seen_by_the_model_have_exact_utf8_cursors() -> Result<()> {
    let model = MockModel::start(|index, request| {
        let offset = if index == 0 {
            0
        } else {
            let previous: Value = serde_json::from_str(
                tool_results(request).last().unwrap()["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            if previous["hasMore"] != true {
                return answer("read complete");
            }
            previous["nextOffset"].as_u64().unwrap()
        };
        calls(vec![function(
            &format!("page-{index}"),
            "artifact_read",
            json!({"path":"page.txt","offset":offset,"limit":6000}),
        )])
    })
    .await?;
    let fixture = Fixture::new(&model, 65536).await?;
    std::fs::create_dir_all(fixture.data.join("artifacts"))?;
    let original = format!("{}{}{}", "A".repeat(9000), "\"\n中文".repeat(1500), "END");
    std::fs::write(fixture.data.join("artifacts/page.txt"), &original)?;
    fixture.turn("Read the full artifact").await?;
    let captured = model.captured();
    let final_request = captured.last().unwrap();
    let mut joined = String::new();
    for result in tool_results(final_request) {
        let text = result["content"].as_str().unwrap();
        assert!(text.chars().count() <= 6000);
        let page: Value = serde_json::from_str(text)?;
        assert_eq!(page["offset"].as_u64().unwrap() as usize, joined.len());
        let chunk = page["text"].as_str().unwrap();
        assert_eq!(
            page["returnedBytes"].as_u64().unwrap() as usize,
            chunk.len()
        );
        joined.push_str(chunk);
        assert_eq!(page["nextOffset"].as_u64().unwrap() as usize, joined.len());
    }
    assert_eq!(joined, original);
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn completed_invalid_json_batch_is_corrected_without_executing_siblings() -> Result<()> {
    let model = MockModel::start(|index, _| {
        if index == 0 {
            let valid = function(
                "must-not-run",
                "file_write",
                json!({"path":"never.txt","content":"bad","expected_sha256":null}),
            );
            let mut malformed = function("bad-json", "file_read", json!({}));
            malformed["function"]["arguments"] = json!("{");
            calls(vec![valid, malformed])
        } else {
            answer("No operation was executed.")
        }
    })
    .await?;
    let fixture = Fixture::new(&model, 65536).await?;
    fixture.turn("Validate the call").await?;
    assert!(!fixture.workspace.join("never.txt").exists());
    let requests = model.captured();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["messages"].to_string().contains("invalid JSON"));
    assert!(!fixture
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e["type"] == "tool_started"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn empty_final_is_incomplete_and_streamed_partial_survives_as_activity() -> Result<()> {
    let model = MockModel::start(|index, _| if index == 0 {
        sse(&[json!({"choices":[{"delta":{"reasoning_content":"synthetic"},"finish_reason":null}]}),json!({"choices":[{"delta":{},"finish_reason":"stop"}]})], true)
    } else {
        sse(&[json!({"choices":[{"delta":{"content":"visible partial"},"finish_reason":null}]}),json!({"choices":[{"delta":{},"finish_reason":"length"}]})], true)
    }).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    assert!(fixture.turn("Respond").await.is_err());
    assert!(fixture.turn("Try again").await.is_err());
    let stored = fixture.stored().await?;
    let events = stored["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["kind"] == "activity"
        && e["payload"]["type"] == "assistant_partial"
        && e["payload"]["text"] == "visible partial"));
    assert!(!events
        .iter()
        .any(|e| e["kind"] == "message" && e["payload"]["content"] == "visible partial"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn optional_mcp_is_lazy_isolates_bad_schemas_and_paginates_discovery() -> Result<()> {
    let model = MockModel::start(|index, _| match index {
        0 => answer("ordinary response"),
        1 => calls(vec![function(
            "search-1",
            "tool_search",
            json!({"query":"mcp"}),
        )]),
        2 => calls(vec![function(
            "search-2",
            "tool_search",
            json!({"query":"mcp","offset":6}),
        )]),
        _ => answer("discovery complete"),
    })
    .await?;
    let fixture = Fixture::new(&model, 65536).await?;
    let marker = fixture.temporary.path().join("mcp-started");
    let script = r#"const fs=require('node:fs');fs.writeFileSync(process.argv[1],'started');
const rl=require('node:readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(!r.id)return;let result={};
if(r.method==='initialize')result={protocolVersion:'2025-11-25',capabilities:{tools:{}}};
if(r.method==='tools/list')result={tools:[...Array.from({length:7},(_,i)=>({name:'item_'+i,description:'fixture tool',inputSchema:{type:'object',properties:{}}})),{name:'incompatible',inputSchema:{type:'object',properties:{x:{anyOf:[{type:'string'},{type:'number'}]}}}}]};
process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\n');});"#;
    let mut config = fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["config"]
        .clone();
    config["mcpServers"] = json!([{"id":"fixture","name":"Optional","command":"node","args":["-e",script,marker],"enabled":true}]);
    fixture
        .harness
        .command(json!({"action":"save_settings","config":config}))
        .await?;
    fixture.turn("ordinary task").await?;
    assert!(
        !marker.exists(),
        "ordinary task should not start optional MCP"
    );
    fixture.turn("discover MCP").await?;
    assert!(marker.exists());
    let requests = model.captured();
    let first: Value = serde_json::from_str(
        tool_results(&requests[2]).last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )?;
    assert_eq!(first["matchedCount"], 7);
    assert_eq!(first["nextOffset"], 6);
    assert_eq!(first["loaded"].as_array().unwrap().len(), 6);
    assert_eq!(first["unavailable"][0]["tool"], "incompatible");
    let second: Value = serde_json::from_str(
        tool_results(&requests[3]).last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )?;
    assert_eq!(second["offset"], 6);
    assert_eq!(second["loaded"].as_array().unwrap().len(), 1);
    assert!(second["nextOffset"].is_null());
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn gemma4_reasoning_is_preserved_within_tools_and_removed_on_next_user_turn() -> Result<()> {
    let model = MockModel::start(|index, _| match index {
        0 => sse(&[
            json!({"choices":[{"delta":{"reasoning_content":"synthetic same-turn reasoning"},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"list","type":"function","function":{"name":"file_list","arguments":"{}"}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})], true),
        1 => answer("first answer"),
        _ => answer("second answer"),
    }).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    fixture.turn("first task").await?;
    fixture.turn("second task").await?;
    let requests = model.captured();
    assert!(requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["reasoning_content"] == "synthetic same-turn reasoning"));
    assert!(!requests[2]["messages"]
        .to_string()
        .contains("synthetic same-turn reasoning"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn user_images_require_vision_and_are_offloaded_from_persisted_history() -> Result<()> {
    let model = MockModel::start(|_, request| {
        if request["tools"].as_array().is_some_and(|tools|tools.iter().any(|t|t["function"]["name"]=="harness_capture_probe")) {
            return calls(vec![function("vision-probe", "harness_capture_probe", json!({}))]);
        }
        let messages = request["messages"].as_array().unwrap();
        let probe = messages.iter().rev().find_map(|m| m["content"].as_array().and_then(|parts| {
            if !parts.iter().any(|p|p["text"].as_str().is_some_and(|s|s.contains("solid color in each quadrant"))) { return None; }
            parts.iter().find_map(|p|p["image_url"]["url"].as_str())
        }));
        if let Some(url) = probe {
            use base64::Engine as _;
            let png=base64::engine::general_purpose::STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
            let data=png.windows(4).position(|v|v==b"IDAT").unwrap()+4+7;
            let color=|x:usize,y:usize| match &png[data+y*385+1+x*3..data+y*385+4+x*3] {
                [255,0,0]=>"red",[0,255,0]=>"green",[0,0,255]=>"blue",[255,255,0]=>"yellow",_=>panic!("unexpected probe color"),
            };
            return answer(&json!({"topLeft":color(0,0),"topRight":color(64,0),"bottomLeft":color(0,64),"bottomRight":color(64,64)}).to_string());
        }
        answer("image received")
    }).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6n1cAAAAASUVORK5CYII=";
    assert!(fixture
        .harness
        .turn_with_images(
            fixture.session_id.clone(),
            "  exact request\n".into(),
            vec![png.into()],
            fixture.sink()
        )
        .await
        .is_err());
    assert!(model.captured().is_empty());
    let mut config = fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["config"]
        .clone();
    config["diagnostics"]["mock-model"]["vision"]["status"] = json!("passed");
    let store = Store::open(&fixture.data.join("harness.sqlite"))?;
    store.save_setting(&config)?;
    drop(store);
    let reopened = Harness::open(fixture.data.clone())?;
    reopened
        .turn_with_images(
            fixture.session_id.clone(),
            "  compare these\n".into(),
            vec![png.into(), png.into()],
            fixture.sink(),
        )
        .await?;
    let requests = model.captured();
    assert_eq!(
        requests.len(),
        4,
        "external vision is revalidated before sending the user's images"
    );
    let messages = requests.last().unwrap()["messages"].as_array().unwrap();
    let user = messages.iter().rev().find(|m| m["role"] == "user").unwrap();
    let parts = user["content"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "  compare these\n");
    assert_eq!(parts.iter().filter(|p| p["type"] == "image_url").count(), 2);
    let stored = reopened
        .command(json!({"action":"load_session","sessionId":fixture.session_id}))
        .await?;
    assert!(!stored["events"]
        .to_string()
        .contains("data:image/png;base64"));
    assert!(stored["events"]
        .to_string()
        .contains("Saved image artifact:"));
    reopened.shutdown().await;
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn stale_external_vision_diagnostic_cannot_send_user_pixels() -> Result<()> {
    let model = MockModel::start(|_, _| answer("not a vision result")).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    let mut config = fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["config"]
        .clone();
    config["diagnostics"]["mock-model"]["vision"]["status"] = json!("passed");
    let store = Store::open(&fixture.data.join("harness.sqlite"))?;
    store.save_setting(&config)?;
    drop(store);
    let reopened = Harness::open(fixture.data.clone())?;
    let png="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6n1cAAAAASUVORK5CYII=";
    assert!(reopened
        .turn_with_images(
            fixture.session_id.clone(),
            "private user image".into(),
            vec![png.into()],
            fixture.sink()
        )
        .await
        .is_err());
    assert_eq!(model.captured().len(), 1);
    assert!(!model.captured()[0].to_string().contains(png));
    let snapshot = reopened.command(json!({"action":"snapshot"})).await?;
    assert_eq!(
        snapshot["config"]["diagnostics"]["mock-model"]["vision"]["status"],
        "unknown"
    );
    let saved = reopened
        .command(json!({"action":"load_session","sessionId":fixture.session_id}))
        .await?;
    assert!(saved["events"]
        .to_string()
        .contains("Saved image artifact:"));
    reopened.shutdown().await;
    fixture.harness.shutdown().await;
    Ok(())
}

#[test]
fn image_payload_bytes_do_not_consume_text_context_budget() -> Result<()> {
    let image = |data: String| harness_model::ChatMessage {
        role: "tool".into(),
        content: Some(
            json!([{"type":"text","text":"current screenshot"},{"type":"image_url","image_url":{"url":format!("data:image/png;base64,{data}")}}]),
        ),
        tool_calls: None,
        tool_call_id: Some("screenshot".into()),
        name: Some("desktop_screenshot".into()),
        reasoning_content: None,
    };
    let small = harness_core::context::estimate(&[image("AAAA".into())], &json!([]));
    let large = harness_core::context::estimate(&[image("AAAA".repeat(50_000))], &json!([]));
    assert!(
        large.abs_diff(small) < 256,
        "encoded screenshot binary was counted as text tokens: small={small}, large={large}"
    );
    Ok(())
}

#[tokio::test]
async fn model_calls_real_filesystem_and_gets_result_before_answer_then_history_reopens(
) -> Result<()> {
    let model=MockModel::start(|index,_|match index {
        0=>calls(vec![function("create-file","file_write",json!({"path":"你好.txt","content":"verified from real executor","expected_sha256":null}))]),
        1=>calls(vec![function("read-file","file_read",json!({"path":"你好.txt","start_line":1,"line_count":10}))]),
        _=>answer("已读取实际文件内容。"),
    }).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    fixture.turn("创建文件并读回验证").await?;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("你好.txt"))?,
        "verified from real executor"
    );
    let requests = model.captured();
    assert_eq!(requests.len(), 3);
    assert_eq!(tool_results(&requests[1])[0]["tool_call_id"], "create-file");
    assert!(tool_results(&requests[2]).last().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains("verified from real executor"));
    for request in &requests {
        assert_tool_pairs(request["messages"].as_array().unwrap());
    }
    let stored = fixture.stored().await?;
    let events = stored["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "message" && event["payload"]["role"] == "tool")
            .count(),
        2
    );
    fixture.harness.shutdown().await;
    let reopened = Harness::open(fixture.data.clone())?;
    let replay = reopened
        .command(json!({"action":"load_session","sessionId":fixture.session_id}))
        .await?;
    assert_eq!(replay["events"], stored["events"]);
    assert_eq!(replay["session"]["deviceId"], "local");
    assert!(replay["session"]["title"]
        .as_str()
        .unwrap()
        .contains("创建文件"));
    reopened.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn schema_rejection_feeds_actual_error_then_allows_one_corrected_call() -> Result<()> {
    let model = MockModel::start(|index, _| match index {
        0 => calls(vec![function(
            "invalid",
            "file_write",
            json!({"path":"fixed.txt","content":17,"expected_sha256":null}),
        )]),
        1 => calls(vec![function(
            "corrected",
            "file_write",
            json!({"path":"fixed.txt","content":"corrected","expected_sha256":null}),
        )]),
        _ => answer("参数已纠正并执行。"),
    })
    .await?;
    let fixture = Fixture::new(&model, 65536).await?;
    fixture.turn("写入文本文件").await?;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("fixed.txt"))?,
        "corrected"
    );
    let requests = model.captured();
    let rejection: Value =
        serde_json::from_str(tool_results(&requests[1])[0]["content"].as_str().unwrap())?;
    assert_eq!(rejection["isError"], true);
    assert!(rejection["validationError"]
        .as_str()
        .unwrap()
        .contains("Schema"));
    let stored = fixture.stored().await?;
    assert_eq!(
        stored["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["payload"]["type"] == "tool_completed")
            .count(),
        2
    );
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn invalid_schema_sibling_prevents_the_entire_batch_from_mutating_files() -> Result<()> {
    let model=MockModel::start(|index,_|if index==0 {calls(vec![
        function("valid-first","file_write",json!({"path":"must-not-exist.txt","content":"would run before invalid sibling","expected_sha256":null})),
        function("invalid-second","file_write",json!({"path":"invalid.txt","content":42,"expected_sha256":null})),
    ])}else{answer("修正参数后再操作。")}).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    fixture.turn("检验完整批次参数").await?;
    assert!(
        !fixture.workspace.join("must-not-exist.txt").exists(),
        "valid first call executed before later invalid schema was discovered"
    );
    assert!(!fixture.workspace.join("invalid.txt").exists());
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn truncated_or_token_limited_model_stream_never_dispatches_a_complete_looking_tool(
) -> Result<()> {
    for finish_reason in [None, Some("length")] {
        let model=MockModel::start(move |_,_|{
            let mut frames=vec![json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                "index":0,"id":"uncommitted","type":"function","function":{"name":"file_write","arguments":json!({"path":"must-not-exist.txt","content":"do not execute","expected_sha256":null}).to_string()}
            }]},"finish_reason":null}]})];
            if let Some(reason)=finish_reason {frames.push(json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}));}
            sse(&frames,false)
        }).await?;
        let fixture = Fixture::new(&model, 65536).await?;
        assert!(fixture.turn("拒绝不完整生成中的工具调用").await.is_err());
        assert!(!fixture.workspace.join("must-not-exist.txt").exists());
        assert!(!fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "tool_started"));
        assert!(fixture.stored().await?["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["payload"]["type"] == "turn_completed"
                && event["payload"]["status"] == "failed"));
        fixture.harness.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn repeated_tool_loop_stops_at_its_documented_step_budget_and_saves_results() -> Result<()> {
    let model = MockModel::start(|index, _| {
        calls(vec![function(
            &format!("list-{index}"),
            "file_list",
            json!({"path":".","limit":1}),
        )])
    })
    .await?;
    let fixture = Fixture::new(&model, 262144).await?;
    let error = fixture.turn("持续列目录直到预算耗尽").await.unwrap_err();
    assert!(error.to_string().contains("48"));
    assert_eq!(model.captured().len(), 48);
    let stored = fixture.stored().await?;
    assert_eq!(
        stored["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "message" && event["payload"]["role"] == "tool")
            .count(),
        48
    );
    assert!(fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["activeSessions"]
        .as_array()
        .unwrap()
        .is_empty());
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn interrupt_kills_real_process_prevents_remaining_mutation_and_allows_next_turn(
) -> Result<()> {
    let model=MockModel::start(|index,_|if index==0 {calls(vec![
        function("long-command","terminal_run",json!({"command":if cfg!(windows){"Set-Content -Path started.flag -Value started; Start-Sleep -Seconds 20"}else{"printf started > started.flag; sleep 20"},"timeout_ms":30000})),
        function("late-write","file_write",json!({"path":"must-not-exist.txt","content":"late side effect","expected_sha256":null})),
    ])}else{answer("已恢复下一轮。")}).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    let harness = fixture.harness.clone();
    let id = fixture.session_id.clone();
    let sink = fixture.sink();
    let task = tokio::spawn(async move { harness.turn(id, "运行可取消命令".into(), sink).await });
    let started = tokio::time::timeout(Duration::from_secs(20), async {
        while !fixture.workspace.join("started.flag").exists() {
            if task.is_finished() {
                bail!("turn ended before the command created its start marker");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;
    match started {
        Ok(Ok(())) => {}
        other => bail!(
            "real command did not start: {other:?}; events: {:?}",
            fixture.events.lock().unwrap()
        ),
    }
    fixture
        .harness
        .command(json!({"action":"interrupt","sessionId":fixture.session_id}))
        .await?;
    assert!(tokio::time::timeout(Duration::from_secs(5), task)
        .await??
        .is_err());
    assert!(!fixture.workspace.join("must-not-exist.txt").exists());
    assert!(fixture
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event["type"] == "turn_completed" && event["status"] == "interrupted"));
    fixture.turn("继续").await?;
    let requests = model.captured();
    assert_eq!(requests.len(), 2);
    assert_tool_pairs(requests[1]["messages"].as_array().unwrap());
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn resumed_missing_tool_result_is_repaired_before_new_user_without_replaying_write(
) -> Result<()> {
    let model = MockModel::start(|_, _| answer("先核实状态。 ")).await?;
    let fixture = Fixture::new(&model, 65536).await?;
    let store = Store::open(&fixture.data.join("harness.sqlite"))?;
    store.append(
        &fixture.session_id,
        "message",
        &json!({"role":"user","content":"original goal"}),
    )?;
    store.append(&fixture.session_id,"message",&json!({"role":"assistant","tool_calls":[function("lost-result","file_write",json!({"path":"must-not-replay.txt","content":"unknown outcome","expected_sha256":null}))]}))?;
    store.append(
        &fixture.session_id,
        "message",
        &json!({"role":"user","content":"followup persisted after interrupted tool"}),
    )?;
    fixture.turn("继续，不要重复未知结果的副作用").await?;
    assert!(!fixture.workspace.join("must-not-replay.txt").exists());
    let requests = model.captured();
    assert_tool_pairs(requests[0]["messages"].as_array().unwrap());
    let repair = tool_results(&requests[0])[0]["content"].as_str().unwrap();
    assert!(
        repair.contains("禁止") || repair.contains("unknown") || repair.contains("indeterminate")
    );
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn compaction_preserves_explicit_original_constraint_even_when_summary_omits_it() -> Result<()>
{
    let model = MockModel::start(|_, request| {
        if request["tools"].is_null() {
            answer("Earlier a large file was inspected; continue current work.")
        } else {
            answer("保留原始约束。")
        }
    })
    .await?;
    let fixture = Fixture::new(&model, 16384).await?;
    let store = Store::open(&fixture.data.join("harness.sqlite"))?;
    let constraint = "EXACT_USER_CONSTRAINT: keep configuration production.conf unchanged";
    store.append(
        &fixture.session_id,
        "message",
        &json!({"role":"user","content":constraint}),
    )?;
    store.append(&fixture.session_id,"message",&json!({"role":"assistant","tool_calls":[function("historical-read","file_read",json!({"path":"large.txt"}))]}))?;
    store.append(&fixture.session_id,"message",&json!({"role":"tool","tool_call_id":"historical-read","name":"file_read","content":"historical fixture output ".repeat(4000)}))?;
    store.append(
        &fixture.session_id,
        "message",
        &json!({"role":"assistant","content":"The previous file was inspected."}),
    )?;
    store.append(
        &fixture.session_id,
        "message",
        &json!({"role":"user","content":"next task"}),
    )?;
    fixture.turn("继续当前任务").await?;
    let requests = model.captured();
    assert!(
        requests.len() >= 2,
        "expected real summarization and continuation requests"
    );
    let continuation = requests
        .iter()
        .find(|request| request["tools"].is_array())
        .context("no post-compaction generation")?;
    assert!(
        continuation["messages"].to_string().contains(constraint),
        "exact user constraint disappeared during lossy model-summary compaction"
    );
    assert_tool_pairs(continuation["messages"].as_array().unwrap());
    let raw = fixture.stored().await?;
    assert!(raw["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["kind"] == "message"
            && event["payload"]["content"]
                .as_str()
                .is_some_and(|text| text.len() > 50000)));
    fixture.harness.shutdown().await;
    // Keep fixture storage alive through every persisted-history assertion.
    assert!(fixture.temporary.path().exists());
    Ok(())
}

#[tokio::test]
async fn one_user_turn_can_compact_completed_tool_cycles_and_continue() -> Result<()> {
    let normal_requests = Arc::new(AtomicUsize::new(0));
    let counter = normal_requests.clone();
    let model = MockModel::start(move |_, request| {
        if request["tools"].is_null() {
            return answer("Large file reads completed. Continue from actual file state; detailed output remains in logs.");
        }
        let index = counter.fetch_add(1, Ordering::SeqCst);
        if index < 4 {
            calls(vec![function(&format!("inspect-{index}"), "file_read", json!({"path":"large.txt","start_line":1,"line_count":2}))])
        } else {
            answer("已完成多轮读取。")
        }
    }).await?;
    let fixture = Fixture::new(&model, 12288).await?;
    std::fs::write(
        fixture.workspace.join("large.txt"),
        format!("{}\n{}\n", "A".repeat(8000), "B".repeat(8000)),
    )?;
    fixture
        .turn("SINGLE_TURN_ORIGINAL_GOAL: inspect repeatedly and keep files unchanged")
        .await?;
    let requests = model.captured();
    assert!(
        requests.iter().any(|request| request["tools"].is_null()),
        "the fixture did not exercise context compaction"
    );
    assert_eq!(normal_requests.load(Ordering::SeqCst), 5);
    for request in requests
        .iter()
        .filter(|request| request["tools"].is_array())
    {
        assert_tool_pairs(request["messages"].as_array().unwrap());
        assert!(request["messages"]
            .to_string()
            .contains("SINGLE_TURN_ORIGINAL_GOAL"));
    }
    let stored = fixture.stored().await?;
    let events = stored["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "message" && event["payload"]["role"] == "user")
            .count(),
        1
    );
    assert!(events
        .iter()
        .any(|event| event["kind"] == "context_compacted"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn default_8k_context_can_complete_a_first_turn_with_a_real_file_tool() -> Result<()> {
    let model = MockModel::start(|index, _| {
        if index == 0 {
            calls(vec![function(
                "first-write",
                "file_write",
                json!({"path":"first-turn.txt","content":"8k works","expected_sha256":null}),
            )])
        } else {
            answer("已写入。")
        }
    })
    .await?;
    let fixture = Fixture::new(&model, 8192).await?;
    fixture.turn("创建一个小文本文件").await?;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("first-turn.txt"))?,
        "8k works"
    );
    assert_eq!(model.captured().len(), 2);
    assert!(!fixture
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event["type"] == "compaction_started"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn identical_failed_calls_stop_early_and_keep_all_error_evidence() -> Result<()> {
    let model = MockModel::start(|index, _| {
        calls(vec![function(
            &format!("missing-{index}"),
            "file_read",
            json!({"path":"does-not-exist.txt"}),
        )])
    })
    .await?;
    let fixture = Fixture::new(&model, 16384).await?;
    let error = fixture
        .turn("读取不存在的文件，若失败应改用其他方法")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("三次"));
    assert_eq!(model.captured().len(), 3);
    let stored = fixture.stored().await?;
    assert_eq!(
        stored["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "message" && e["payload"]["role"] == "tool")
            .count(),
        3
    );
    assert!(fixture
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e["type"] == "loop_guard"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn optional_tool_groups_replace_each_other_instead_of_accumulating_schemas() -> Result<()> {
    let model = MockModel::start(|index, _| match index {
        0 => calls(vec![function(
            "load-browser",
            "tool_search",
            json!({"query":"browser"}),
        )]),
        1 => calls(vec![function(
            "load-desktop",
            "tool_search",
            json!({"query":"desktop"}),
        )]),
        _ => answer("done"),
    })
    .await?;
    let fixture = Fixture::new(&model, 32768).await?;
    let mut config = fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["config"]
        .clone();
    config["toolPermissions"]["browser"] = json!(true);
    config["toolPermissions"]["desktop"] = json!(true);
    fixture
        .harness
        .command(json!({"action":"save_settings","config":config}))
        .await?;
    fixture
        .turn("Load tools only; do not interact with the desktop")
        .await?;
    let requests = model.captured();
    let names = |r: &Value| {
        r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert!(names(&requests[1])
        .iter()
        .any(|n| n.starts_with("browser_")));
    assert!(!names(&requests[1])
        .iter()
        .any(|n| n.starts_with("desktop_")));
    assert!(names(&requests[2])
        .iter()
        .any(|n| n.starts_with("desktop_")));
    assert!(!names(&requests[2])
        .iter()
        .any(|n| n.starts_with("browser_")));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mcp_image_evidence_is_archived_even_before_vision_is_certified() -> Result<()> {
    let model = MockModel::start(|index, request| match index {
        0 => calls(vec![function(
            "load-image",
            "tool_search",
            json!({"query":"mcp"}),
        )]),
        1 => {
            let name = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find_map(|tool| {
                    tool["function"]["name"]
                        .as_str()
                        .filter(|name| name.starts_with("mcp_"))
                })
                .unwrap();
            calls(vec![function("image-call", name, json!({}))])
        }
        _ => answer("Image evidence saved; vision is not certified."),
    })
    .await?;
    let fixture = Fixture::new(&model, 32768).await?;
    let script = r#"const readline=require('node:readline');
readline.createInterface({input:process.stdin}).on('line',line=>{
 const r=JSON.parse(line); if(!r.id)return;
 let result={};
 if(r.method==='initialize')result={protocolVersion:'2025-11-25',capabilities:{tools:{}}};
 if(r.method==='tools/list')result={tools:[{name:'image_evidence',description:'Return fixture pixels',inputSchema:{type:'object',properties:{},additionalProperties:false}}]};
 if(r.method==='tools/call')result={content:[{type:'image',mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6n1cAAAAASUVORK5CYII='}]};
 process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\n');
});"#;
    let mut config = fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["config"]
        .clone();
    config["mcpServers"] = json!([{"id":"image-fixture","name":"Explicit image fixture","command":"node","args":["-e",script],"enabled":true}]);
    fixture
        .harness
        .command(json!({"action":"save_settings","config":config}))
        .await?;
    fixture
        .turn("Archive the image without claiming to see it")
        .await?;
    let requests = model.captured();
    let last = requests.last().unwrap()["messages"].to_string();
    assert!(last.contains("Saved image artifact:"));
    assert!(!last.contains("data:image"));
    let pngs: Vec<_> = std::fs::read_dir(fixture.data.join("artifacts"))?
        .filter_map(|p| p.ok())
        .filter(|p| p.path().extension().is_some_and(|s| s == "png"))
        .collect();
    assert_eq!(pngs.len(), 1);
    assert!(std::fs::read(pngs[0].path())?.starts_with(b"\x89PNG\r\n\x1a\n"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn output_token_limit_never_triggers_input_history_overflow_recovery() -> Result<()> {
    let model=MockModel::start(|_,_|sse(&[json!({"choices":[{"index":0,"delta":{"content":"unfinished"},"finish_reason":"length"}]})],true)).await?;
    let fixture = Fixture::new(&model, 32768).await?;
    let store = Store::open(&fixture.data.join("harness.sqlite"))?;
    for _ in 0..3 {
        store.append(
            &fixture.session_id,
            "message",
            &json!({"role":"user","content":"retain this exact previous constraint"}),
        )?;
        store.append(
            &fixture.session_id,
            "message",
            &json!({"role":"assistant","content":"acknowledged"}),
        )?;
    }
    assert!(fixture.turn("continue").await.is_err());
    assert_eq!(
        model.captured().len(),
        1,
        "an output budget failure must not retry with omitted input history"
    );
    assert!(!store
        .events(&fixture.session_id)?
        .iter()
        .any(|e| e["kind"] == "context_compacted"));
    fixture.harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn lost_mcp_action_reply_halts_sibling_side_effects_without_replay() -> Result<()> {
    let model=MockModel::start(|index,request| {
        if index==0 {return calls(vec![function("load-mcp","tool_search",json!({"query":"mcp"}))]);}
        let name=request["tools"].as_array().unwrap().iter().find_map(|tool|tool["function"]["name"].as_str().filter(|name|name.starts_with("mcp_"))).unwrap();
        calls(vec![function("uncertain-mcp",name,json!({})),function("must-not-write","file_write",json!({"path":"must-not-exist.txt","content":"forbidden sibling","expected_sha256":null}))])
    }).await?;
    let fixture = Fixture::new(&model, 32768).await?;
    let marker = fixture.workspace.join("action-happened.txt");
    let script = r#"const readline=require('node:readline'),fs=require('node:fs');
readline.createInterface({input:process.stdin}).on('line',line=>{
 const r=JSON.parse(line);if(!r.id)return;
 if(r.method==='tools/call'){fs.writeFileSync(process.argv[1],'action happened once');process.exit(0);}
 const result=r.method==='initialize'?{protocolVersion:'2025-11-25',capabilities:{tools:{}}}:{tools:[{name:'act_then_disconnect',inputSchema:{type:'object',properties:{},additionalProperties:false}}]};
 process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\n');
});"#;
    let mut config = fixture
        .harness
        .command(json!({"action":"snapshot"}))
        .await?["config"]
        .clone();
    config["mcpServers"] = json!([{"id":"lost-reply-fixture","name":"Explicit indeterminate fixture","command":"node","args":["-e",script,marker],"enabled":true}]);
    fixture
        .harness
        .command(json!({"action":"save_settings","config":config}))
        .await?;
    let result = fixture.turn("Perform the test action once").await;
    assert!(result.unwrap_err().to_string().contains("结果待确认"));
    assert!(marker.exists());
    assert!(!fixture.workspace.join("must-not-exist.txt").exists());
    assert_eq!(model.captured().len(), 2);
    let stored = fixture.stored().await?;
    assert!(stored["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["payload"]["result"]["indeterminate"] == true));
    fixture.harness.shutdown().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn another_session_can_infer_while_first_runs_a_terminal_tool() -> Result<()> {
    let model = MockModel::start(|index, _| {
        if index == 0 {
            calls(vec![function(
                "slow-tool",
                "terminal_run",
                json!({"command":"sleep 1","timeout_ms":5000}),
            )])
        } else {
            answer("done")
        }
    })
    .await?;
    let fixture = Fixture::new(&model, 16384).await?;
    let other = fixture
        .harness
        .command(json!({"action":"create_session"}))
        .await?;
    let other_id = other["id"].as_str().unwrap().to_owned();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let h = fixture.harness.clone();
    let id = fixture.session_id.clone();
    let first = tokio::spawn(async move {
        h.turn(
            id,
            "run slow tool".into(),
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "tool_started" {
                break;
            }
        }
    })
    .await?;
    tokio::time::timeout(
        Duration::from_millis(700),
        fixture
            .harness
            .turn(other_id, "another session".into(), fixture.sink()),
    )
    .await??;
    assert!(
        !first.is_finished(),
        "tool should still be sleeping while another session finishes inference"
    );
    first.await??;
    fixture.harness.shutdown().await;
    Ok(())
}
