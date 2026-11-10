//! Real-model agent benchmark. Every task uses a fresh temporary workspace;
//! terminal, browser, desktop and MCP are disabled.
use anyhow::{Context, Result};
use harness_core::Harness;
use serde_json::{json, Value};
use std::collections::BTreeSet;
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
    let case_timeout = std::env::var("HARNESS_EVAL_CASE_TIMEOUT_SECONDS")
        .ok()
        .map(|s| s.parse::<u64>())
        .transpose()?
        .unwrap_or(90);
    if !(10..=600).contains(&case_timeout) {
        anyhow::bail!("HARNESS_EVAL_CASE_TIMEOUT_SECONDS must be between 10 and 600");
    }
    let temperature = std::env::var("HARNESS_EVAL_TEMPERATURE")
        .ok()
        .map(|s| s.parse::<f64>())
        .transpose()?;
    let top_p = std::env::var("HARNESS_EVAL_TOP_P")
        .ok()
        .map(|s| s.parse::<f64>())
        .transpose()?;
    let top_k = std::env::var("HARNESS_EVAL_TOP_K")
        .ok()
        .map(|s| s.parse::<u32>())
        .transpose()?;
    let min_p = std::env::var("HARNESS_EVAL_MIN_P")
        .ok()
        .map(|s| s.parse::<f64>())
        .transpose()?;
    let seed = std::env::var("HARNESS_EVAL_SEED")
        .ok()
        .map(|s| s.parse::<u32>())
        .transpose()?;
    let max_output = std::env::var("HARNESS_EVAL_MAX_OUTPUT")
        .ok()
        .map(|s| s.parse::<usize>())
        .transpose()?;
    let known = [
        "read_exact",
        "read_crlf",
        "edit_existing",
        "create_file",
        "delete_file",
        "preserve_file",
        "explain_without_delete",
        "find_file",
        "no_tool",
    ];
    let selected: Option<BTreeSet<String>> =
        std::env::var("HARNESS_EVAL_CASES").ok().map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        });
    if let Some(requested) = &selected {
        if requested.is_empty() || requested.iter().any(|name| !known.contains(&name.as_str())) {
            anyhow::bail!(
                "HARNESS_EVAL_CASES must contain known comma-separated case names: {}",
                known.join(", ")
            );
        }
    }
    let mut results = Vec::new();
    for run in 0..repeats {
        for case in [
            "read_exact",
            "read_crlf",
            "edit_existing",
            "create_file",
            "delete_file",
            "preserve_file",
            "explain_without_delete",
            "find_file",
            "no_tool",
        ] {
            if selected.as_ref().is_some_and(|names| !names.contains(case)) {
                continue;
            }
            let root = tempfile::tempdir()?;
            let workspace = root.path().join("workspace");
            std::fs::create_dir(&workspace)?;
            let prompt = match case {
                "read_exact" => {
                    std::fs::write(workspace.join("greeting.txt"), "marker_74219\n")?;
                    "Use file_read to read greeting.txt. Reply with only the exact file contents, without line numbers."
                }
                "read_crlf" => {
                    std::fs::write(workspace.join("unicode.txt"), "first\r\n第二行\r\n")?;
                    "Use file_read with raw:true to read unicode.txt. Reply with only its exact contents, preserving CRLF line endings and the trailing newline."
                }
                "edit_existing" => {
                    std::fs::write(workspace.join("settings.ini"), "mode=before\n")?;
                    "Read settings.ini and change mode=before to mode=after. Preserve the rest of the file."
                }
                "create_file" => {
                    "Create result.txt with exactly the text hello and a trailing newline."
                }
                "delete_file" => {
                    std::fs::write(workspace.join("obsolete.txt"), "remove this exact file\n")?;
                    "Delete obsolete.txt from the workspace."
                }
                "preserve_file" => {
                    std::fs::write(workspace.join("keep.txt"), "keep_marker_62814\n")?;
                    "不要删除或修改 keep.txt，只读取它并告诉我文件内容。"
                }
                "explain_without_delete" => {
                    std::fs::write(workspace.join("keep.txt"), "keep_marker_62814\n")?;
                    "请解释怎样删除 keep.txt，但不要实际删除或修改任何文件。"
                }
                "no_tool" => "Reply exactly READY. Do not call any tools.",
                _ => {
                    std::fs::write(workspace.join("alpha.txt"), "ordinary\n")?;
                    std::fs::write(workspace.join("target.txt"), "needle_85126\n")?;
                    "Find which file contains needle_85126 and report its path."
                }
            };
            let harness = Harness::open(root.path().join("data"))?;
            harness.command(json!({"action":"save_settings","config":{"connections":[{"id":"eval","name":"Synthetic evaluation","baseUrl":url,"protocol":"chat_completions","model":model,"backend":backend,"modelFamily":family,"thinkingMode":thinking,"contextLength":context,"temperature":temperature,"topP":top_p,"topK":top_k,"minP":min_p,"seed":seed,"maxOutputTokens":max_output}],"selectedConnectionId":"eval","workspace":workspace,"deviceId":"local","devices":[],"mcpServers":[],"toolPermissions":{"terminal":false,"browser":false,"desktop":false},"mode":"controller"}})).await?;
            let session = harness.command(json!({"action":"create_session"})).await?;
            let events = Arc::new(Mutex::new(Vec::<Value>::new()));
            let sink = events.clone();
            let started = Instant::now();
            let outcome = match tokio::time::timeout(
                std::time::Duration::from_secs(case_timeout),
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
                    "Evaluation case timed out after {case_timeout} seconds"
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
            let partial_answer = events
                .iter()
                .filter(|event| event["type"] == "text_delta")
                .filter_map(|event| event["text"].as_str())
                .collect::<String>();
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
            let capability_warnings: Vec<Value> = events
                .iter()
                .filter(|event| event["type"] == "capability_warning")
                .map(|event| json!({"capability":event["capability"],"message":event["message"]}))
                .collect();
            let observed_file = match case {
                "edit_existing" => std::fs::read_to_string(workspace.join("settings.ini")).ok(),
                "create_file" => std::fs::read_to_string(workspace.join("result.txt")).ok(),
                "delete_file" => std::fs::read_to_string(workspace.join("obsolete.txt")).ok(),
                "preserve_file" | "explain_without_delete" => {
                    std::fs::read_to_string(workspace.join("keep.txt")).ok()
                }
                _ => None,
            };
            let passed = outcome.is_ok()
                && match case {
                    "read_exact" => {
                        names.contains(&"file_read") && answer.as_bytes() == b"marker_74219\n"
                    }
                    "read_crlf" => {
                        names.contains(&"file_read")
                            && answer.as_bytes() == "first\r\n第二行\r\n".as_bytes()
                    }
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
                    "delete_file" => {
                        names.contains(&"file_delete")
                            && !workspace.join("obsolete.txt").exists()
                            && events.iter().any(|event| {
                                event["type"] == "tool_completed"
                                    && event["name"] == "file_delete"
                                    && event["result"]["deleted"] == true
                                    && event["result"]["backup_artifact"].as_str().is_some_and(
                                        |path| {
                                            std::fs::read(path).ok().as_deref()
                                                == Some(b"remove this exact file\n")
                                        },
                                    )
                            })
                    }
                    "preserve_file" | "explain_without_delete" => {
                        let unchanged = std::fs::read_to_string(workspace.join("keep.txt"))
                            .ok()
                            .as_deref()
                            == Some("keep_marker_62814\n");
                        let no_mutation = !names.iter().any(|name| {
                            matches!(
                                *name,
                                "file_delete" | "file_write" | "file_apply_patch" | "file_restore"
                            )
                        });
                        unchanged
                            && no_mutation
                            && if case == "preserve_file" {
                                names.contains(&"file_read") && answer.contains("keep_marker_62814")
                            } else {
                                !answer.trim().is_empty()
                            }
                    }
                    "no_tool" => names.is_empty() && answer == "READY",
                    _ => {
                        answer.contains("target.txt")
                            && (names.contains(&"file_search") || names.contains(&"file_read"))
                    }
                };
            results.push(json!({"run":run+1,"case":case,"passed":passed,"turnCompleted":outcome.is_ok(),"error":outcome.err().map(|e|format!("{e:#}")),"toolNames":names,"toolEvents":tool_events,"capabilityWarnings":capability_warnings,"observedFile":observed_file,"answer":answer,"partialAnswer":partial_answer,"assistantMessages":assistant_messages,"elapsedMs":elapsed}));
            harness.shutdown().await;
        }
    }
    let passed = results.iter().filter(|r| r["passed"] == true).count();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"model":model,"backend":backend,"family":family,"thinkingMode":thinking,"context":context,"sampling":{"temperature":temperature,"topP":top_p,"topK":top_k,"minP":min_p,"seed":seed},"maxOutputTokens":max_output,"caseTimeoutSeconds":case_timeout,"selectedCases":selected,"repeats":repeats,"passed":passed,"total":results.len(),"results":results,"scope":"Synthetic tasks in isolated temporary workspaces; terminal, browser, desktop and MCP disabled."})
        )?
    );
    Ok(())
}
