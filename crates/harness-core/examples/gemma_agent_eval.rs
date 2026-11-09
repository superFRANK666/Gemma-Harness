//! Real-model agent benchmark. Every task uses a fresh temporary workspace;
//! terminal, browser, desktop and MCP are disabled.
use anyhow::{Context, Result};
use harness_core::Harness;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("HARNESS_EVAL_URL").context("Set HARNESS_EVAL_URL")?;
    let model = std::env::var("HARNESS_EVAL_MODEL").context("Set HARNESS_EVAL_MODEL")?;
    let backend = std::env::var("HARNESS_EVAL_BACKEND").unwrap_or_else(|_| "auto".into());
    let family = std::env::var("HARNESS_EVAL_FAMILY").unwrap_or_else(|_| "auto".into());
    let thinking = std::env::var("HARNESS_EVAL_THINKING").unwrap_or_else(|_| "auto".into());
    let repeats = std::env::var("HARNESS_EVAL_REPEATS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 10);
    let context = std::env::var("HARNESS_EVAL_CONTEXT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(8192);
    let mut results = Vec::new();
    for run in 0..repeats {
        for case in ["read_exact", "edit_existing", "create_file", "find_file"] {
            let root = tempfile::tempdir()?;
            let workspace = root.path().join("workspace");
            std::fs::create_dir(&workspace)?;
            let prompt = match case {
                "read_exact" => {
                    std::fs::write(workspace.join("greeting.txt"), "marker_74219\n")?;
                    "Use file_read to read greeting.txt. Reply with only the exact file contents, without line numbers."
                }
                "edit_existing" => {
                    std::fs::write(workspace.join("settings.ini"), "mode=before\n")?;
                    "Read settings.ini and change mode=before to mode=after. Preserve the rest of the file."
                }
                "create_file" => {
                    "Create result.txt with exactly the text hello and a trailing newline."
                }
                _ => {
                    std::fs::write(workspace.join("alpha.txt"), "ordinary\n")?;
                    std::fs::write(workspace.join("target.txt"), "needle_85126\n")?;
                    "Find which file contains needle_85126 and report its path."
                }
            };
            let harness = Harness::open(root.path().join("data"))?;
            harness.command(json!({"action":"save_settings","config":{"connections":[{"id":"eval","name":"Synthetic evaluation","baseUrl":url,"protocol":"chat_completions","model":model,"backend":backend,"modelFamily":family,"thinkingMode":thinking,"contextLength":context}],"selectedConnectionId":"eval","workspace":workspace,"deviceId":"local","devices":[],"mcpServers":[],"toolPermissions":{"terminal":false,"browser":false,"desktop":false},"mode":"controller"}})).await?;
            let session = harness.command(json!({"action":"create_session"})).await?;
            let events = Arc::new(Mutex::new(Vec::<Value>::new()));
            let sink = events.clone();
            let started = Instant::now();
            let outcome = match tokio::time::timeout(
                std::time::Duration::from_secs(90),
                harness.turn(
                    session["id"].as_str().unwrap().into(),
                    prompt.into(),
                    Arc::new(move |event| sink.lock().unwrap().push(event)),
                ),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "Evaluation case timed out after 90 seconds"
                )),
            };
            let elapsed = started.elapsed().as_millis();
            let events = events.lock().unwrap().clone();
            let names: Vec<_> = events
                .iter()
                .filter(|e| e["type"] == "tool_started")
                .filter_map(|e| e["name"].as_str())
                .collect();
            let assistant_messages: Vec<String> = events
                .iter()
                .filter(|e| e["type"] == "assistant_completed")
                .filter_map(|e| e["text"].as_str())
                .map(str::to_owned)
                .collect();
            let answer = assistant_messages.last().cloned().unwrap_or_default();
            // These are synthetic temporary workspaces. Keep the actual call
            // arguments and results so an incorrect answer can be separated
            // from a rejected call or an unexpected file modification.
            let tool_events: Vec<Value> = events
                .iter()
                .filter_map(|event| match event["type"].as_str() {
                    Some("tool_started") => Some(json!({"type":"started","name":event["name"],"arguments":event["arguments"]})),
                    Some("tool_completed") => Some(json!({"type":"completed","name":event["name"],"result":event["result"]})),
                    Some("tool_batch_rejected") => Some(json!({"type":"batch_rejected","message":event["message"]})),
                    _ => None,
                })
                .collect();
            let observed_file = match case {
                "edit_existing" => std::fs::read_to_string(workspace.join("settings.ini")).ok(),
                "create_file" => std::fs::read_to_string(workspace.join("result.txt")).ok(),
                _ => None,
            };
            let passed = outcome.is_ok()
                && match case {
                    "read_exact" => names.contains(&"file_read") && answer.trim() == "marker_74219",
                    "edit_existing" => {
                        std::fs::read_to_string(workspace.join("settings.ini"))
                            .ok()
                            .as_deref()
                            == Some("mode=after\n")
                    }
                    "create_file" => {
                        std::fs::read_to_string(workspace.join("result.txt"))
                            .ok()
                            .as_deref()
                            == Some("hello\n")
                    }
                    _ => {
                        answer.contains("target.txt")
                            && (names.contains(&"file_search") || names.contains(&"file_read"))
                    }
                };
            results.push(json!({"run":run+1,"case":case,"passed":passed,"turnCompleted":outcome.is_ok(),"error":outcome.err().map(|e|format!("{e:#}")),"toolNames":names,"toolEvents":tool_events,"observedFile":observed_file,"answer":answer,"assistantMessages":assistant_messages,"elapsedMs":elapsed}));
            harness.shutdown().await;
        }
    }
    let passed = results.iter().filter(|r| r["passed"] == true).count();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"model":model,"backend":backend,"family":family,"thinkingMode":thinking,"context":context,"repeats":repeats,"passed":passed,"total":results.len(),"results":results,"scope":"Synthetic tasks in isolated temporary workspaces; terminal, browser, desktop and MCP disabled."})
        )?
    );
    Ok(())
}
