//! Synthetic, effect-free tool-calling benchmark for an explicitly selected model.
//! Set HARNESS_EVAL_URL and HARNESS_EVAL_MODEL; see docs/gemma-adaptation.md.
use anyhow::{Context, Result};
use harness_model::{ChatMessage, ConnectionConfig, Provider, ProviderEvent, ToolDefinition};
use serde_json::{json, Value};
use std::{collections::BTreeSet, time::Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Case {
    name: &'static str,
    prompt: &'static str,
    tool: ToolDefinition,
    expected: Value,
}
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}
async fn generate_with_partial(
    provider: &Provider,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
) -> (Result<()>, Vec<ProviderEvent>) {
    let (tx, mut rx) = mpsc::channel(256);
    let max_output = match std::env::var("HARNESS_EVAL_MAX_OUTPUT")
        .ok()
        .map(|s| s.parse::<usize>())
        .transpose()
    {
        Ok(value) => value.unwrap_or(512),
        Err(error) => return (Err(error.into()), Vec::new()),
    };
    let request = provider.generate(messages, tools, max_output, tx, CancellationToken::new());
    let collect = async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    };
    tokio::join!(request, collect)
}

async fn generate(
    provider: &Provider,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
) -> Result<Vec<ProviderEvent>> {
    let (result, events) = generate_with_partial(provider, messages, tools).await;
    result?;
    Ok(events)
}

#[tokio::main]
async fn main() -> Result<()> {
    let base_url = std::env::var("HARNESS_EVAL_URL")
        .context("Set HARNESS_EVAL_URL to the explicitly selected inference server")?;
    let model = std::env::var("HARNESS_EVAL_MODEL")
        .context("Set HARNESS_EVAL_MODEL to the loaded model ID")?;
    let backend = std::env::var("HARNESS_EVAL_BACKEND").unwrap_or_else(|_| "auto".into());
    let model_family = std::env::var("HARNESS_EVAL_FAMILY").unwrap_or_else(|_| "auto".into());
    let thinking_mode = std::env::var("HARNESS_EVAL_THINKING").unwrap_or_else(|_| "auto".into());
    let repeats = std::env::var("HARNESS_EVAL_REPEATS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 10);
    let config = ConnectionConfig {
        base_url,
        model,
        backend,
        model_family,
        thinking_mode,
        context_length: std::env::var("HARNESS_EVAL_CONTEXT")
            .ok()
            .map(|s| s.parse::<usize>())
            .transpose()?
            .unwrap_or(8192),
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
    let provider = Provider::new(config.clone())?;
    let cases = vec![
        Case {
            name: "english_string",
            prompt: "Call get_weather for the city Tokyo.",
            tool: tool(
                "get_weather",
                "Get current weather for a city.",
                json!({"city":{"type":"string","description":"City name."}}),
                &["city"],
            ),
            expected: json!({"city":"Tokyo"}),
        },
        Case {
            name: "chinese_string",
            prompt: "请调用 store_text，text 参数必须是 你好世界。",
            tool: tool(
                "store_text",
                "Store the supplied exact text.",
                json!({"text":{"type":"string","description":"Exact text to store."}}),
                &["text"],
            ),
            expected: json!({"text":"你好世界"}),
        },
        Case {
            name: "punctuation",
            prompt: "Call store_text with text exactly equal to the following quoted value (without the quotes): \"a,b {x}\"",
            tool: tool(
                "store_text",
                "Store the supplied exact text.",
                json!({"text":{"type":"string","description":"Exact text to store."}}),
                &["text"],
            ),
            expected: json!({"text":"a,b {x}"}),
        },
        Case {
            name: "integer_boolean",
            prompt: "Call set_options with count 17 and enabled true.",
            tool: tool(
                "set_options",
                "Set count and enabled state.",
                json!({"count":{"type":"integer","description":"Number of items."},"enabled":{"type":"boolean","description":"Whether enabled."}}),
                &["count", "enabled"],
            ),
            expected: json!({"count":17,"enabled":true}),
        },
        Case {
            name: "nullable",
            prompt: "Call save_file with path note.txt and expected_sha256 set to null.",
            tool: tool(
                "save_file",
                "Save a new file; use null hash for a new file.",
                json!({"path":{"type":"string","description":"File path."},"expected_sha256":{"type":["string","null"],"description":"Existing file hash, or null for a new file."}}),
                &["path", "expected_sha256"],
            ),
            expected: json!({"path":"note.txt","expected_sha256":null}),
        },
        Case {
            name: "enum",
            prompt: "Call set_color with color blue.",
            tool: tool(
                "set_color",
                "Set a color.",
                json!({"color":{"type":"string","enum":["red","blue"],"description":"Color choice."}}),
                &["color"],
            ),
            expected: json!({"color":"blue"}),
        },
        Case {
            name: "array",
            prompt: "Call tag_items with tags alpha and beta, in that order.",
            tool: tool(
                "tag_items",
                "Set a list of tags.",
                json!({"tags":{"type":"array","items":{"type":"string"},"description":"Ordered tags."}}),
                &["tags"],
            ),
            expected: json!({"tags":["alpha","beta"]}),
        },
        Case {
            name: "nested",
            prompt: "Call set_point with point x 2 and y 3.",
            tool: tool(
                "set_point",
                "Set a point with x and y coordinates.",
                json!({"point":{"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"}},"required":["x","y"],"description":"Point coordinates."}}),
                &["point"],
            ),
            expected: json!({"point":{"x":2,"y":3}}),
        },
        Case {
            name: "reserved_property_names",
            prompt: "Call configure with type alpha, description brief, and nested point type point with x 2.",
            tool: tool(
                "configure",
                "Configure the supplied settings.",
                json!({"type":{"type":"string","description":"Requested kind."},"description":{"type":"string","description":"Description text."},"nested":{"type":"object","properties":{"type":{"type":"string"},"x":{"type":"integer"}},"required":["type","x"]}}),
                &["type", "description", "nested"],
            ),
            expected: json!({"type":"alpha","description":"brief","nested":{"type":"point","x":2}}),
        },
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
    let known: BTreeSet<&str> = cases
        .iter()
        .map(|case| case.name)
        .chain([
            "tool_selection",
            "multi_tool_selection",
            "no_tool",
            "tool_roundtrip",
        ])
        .collect();
    if let Some(requested) = &selected {
        if requested.is_empty() || requested.iter().any(|name| !known.contains(name.as_str())) {
            anyhow::bail!(
                "HARNESS_EVAL_CASES must contain known comma-separated case names: {}",
                known.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
    }
    let include = |name: &str| selected.as_ref().is_none_or(|names| names.contains(name));
    let mut results = Vec::new();
    for run in 0..repeats {
        for case in &cases {
            if !include(case.name) {
                continue;
            }
            let started = Instant::now();
            let (response, events) = generate_with_partial(
                &provider,
                vec![
                    ChatMessage::text("system", "Follow the user request exactly."),
                    ChatMessage::text("user", case.prompt),
                ],
                vec![case.tool.clone()],
            )
            .await;
            let elapsed = started.elapsed().as_millis();
            let text = events
                .iter()
                .filter_map(|e| {
                    if let ProviderEvent::TextDelta { text } = e {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>();
            let reasoning_bytes = events
                .iter()
                .filter_map(|e| {
                    if let ProviderEvent::ReasoningDelta { text } = e {
                        Some(text.len())
                    } else {
                        None
                    }
                })
                .sum::<usize>();
            match response {
                Ok(()) => {
                    let calls:Vec<_>=events.iter().filter_map(|e| if let ProviderEvent::ToolCall{call}=e {Some(call)}else{None}).collect();
                    let args=calls.first().and_then(|call|serde_json::from_str::<Value>(&call.function.arguments).ok());
                    let passed=calls.len()==1 && calls[0].function.name==case.tool.name && args.as_ref()==Some(&case.expected);
                    let usage=events.iter().find_map(|e|if let ProviderEvent::Usage{input_tokens,output_tokens}=e {Some(json!({"input":input_tokens,"output":output_tokens}))}else{None});
                    let rejected=events.iter().find_map(|e|if let ProviderEvent::ToolBatchRejected{message}=e {Some(message)}else{None});
                    results.push(json!({"run":run+1,"case":case.name,"passed":passed,"callCount":calls.len(),"actualTool":calls.first().map(|c|&c.function.name),"actualArguments":args,"rejected":rejected,"answer":text,"reasoningBytes":reasoning_bytes,"elapsedMs":elapsed,"usage":usage}));
                }
                Err(error) => results.push(json!({"run":run+1,"case":case.name,"passed":false,"error":format!("{error:#}"),"answer":text,"reasoningBytes":reasoning_bytes,"elapsedMs":elapsed})),
            }
        }
        let weather = tool(
            "get_weather",
            "Get current weather for a city.",
            json!({"city":{"type":"string","description":"City name."}}),
            &["city"],
        );
        let clock = tool(
            "get_time",
            "Get the current time for a city.",
            json!({"city":{"type":"string","description":"City name."}}),
            &["city"],
        );
        if include("tool_selection") {
            let started = Instant::now();
            let selection = generate(
                &provider,
                vec![
                    ChatMessage::text("system", "Follow the user request exactly."),
                    ChatMessage::text("user", "Get the current time in Tokyo."),
                ],
                vec![weather.clone(), clock],
            )
            .await;
            match selection {
            Ok(events) => {
                let calls:Vec<_>=events.iter().filter_map(|e|if let ProviderEvent::ToolCall{call}=e{Some(call)}else{None}).collect();
                let args=calls.first().and_then(|c|serde_json::from_str::<Value>(&c.function.arguments).ok());
                let passed=calls.len()==1 && calls[0].function.name=="get_time" && args.as_ref()==Some(&json!({"city":"Tokyo"}));
                results.push(json!({"run":run+1,"case":"tool_selection","passed":passed,"callCount":calls.len(),"actualTool":calls.first().map(|c|&c.function.name),"actualArguments":args,"elapsedMs":started.elapsed().as_millis()}));
            }
            Err(error) => results.push(json!({"run":run+1,"case":"tool_selection","passed":false,"error":format!("{error:#}"),"elapsedMs":started.elapsed().as_millis()})),
        }
        }
        if include("multi_tool_selection") {
            let selection_tools = vec![
                tool(
                    "read_file",
                    "Read a file starting at a requested line.",
                    json!({"path":{"type":"string"},"start_line":{"type":"integer"}}),
                    &["path", "start_line"],
                ),
                tool(
                    "search_files",
                    "Search text across files.",
                    json!({"query":{"type":"string"}}),
                    &["query"],
                ),
                tool(
                    "write_file",
                    "Write a text file.",
                    json!({"path":{"type":"string"},"content":{"type":"string"}}),
                    &["path", "content"],
                ),
                tool(
                    "list_files",
                    "List files in a directory.",
                    json!({"path":{"type":"string"}}),
                    &["path"],
                ),
                tool(
                    "delete_file",
                    "Delete a file.",
                    json!({"path":{"type":"string"}}),
                    &["path"],
                ),
                tool(
                    "read_history",
                    "Read a prior session event.",
                    json!({"event_id":{"type":"integer"}}),
                    &["event_id"],
                ),
            ];
            let started = Instant::now();
            let multi = generate(
                &provider,
                vec![
                    ChatMessage::text("system", "Follow the user request exactly."),
                    ChatMessage::text(
                        "user",
                        "请调用 read_file，读取 notes/报表.txt，从第 1 行开始。不要修改文件。",
                    ),
                ],
                selection_tools,
            )
            .await;
            match multi {
            Ok(events) => {
                let calls: Vec<_> = events.iter().filter_map(|e| if let ProviderEvent::ToolCall { call } = e { Some(call) } else { None }).collect();
                let args = calls.first().and_then(|c| serde_json::from_str::<Value>(&c.function.arguments).ok());
                results.push(json!({"run":run+1,"case":"multi_tool_selection","passed":calls.len()==1 && calls[0].function.name=="read_file" && args.as_ref()==Some(&json!({"path":"notes/报表.txt","start_line":1})),"actualTool":calls.first().map(|c|&c.function.name),"actualArguments":args,"callCount":calls.len(),"elapsedMs":started.elapsed().as_millis()}));
            }
            Err(error) => results.push(json!({"run":run+1,"case":"multi_tool_selection","passed":false,"error":format!("{error:#}"),"elapsedMs":started.elapsed().as_millis()})),
        }
        }
        if include("no_tool") {
            let started = Instant::now();
            let negative = generate(
                &provider,
                vec![
                    ChatMessage::text("system", "Follow the user request exactly."),
                    ChatMessage::text("user", "Reply exactly READY. Do not call any tool."),
                ],
                vec![weather.clone()],
            )
            .await;
            match negative {
            Ok(events) => {
                let calls=events.iter().filter(|e|matches!(e,ProviderEvent::ToolCall { .. } | ProviderEvent::ToolBatchRejected { .. })).count();
                let text=events.iter().filter_map(|e|if let ProviderEvent::TextDelta{text}=e{Some(text.as_str())}else{None}).collect::<String>();
                results.push(json!({"run":run+1,"case":"no_tool","passed":calls==0 && text.trim()=="READY","callCount":calls,"answer":text,"elapsedMs":started.elapsed().as_millis()}));
            }
            Err(error) => results.push(json!({"run":run+1,"case":"no_tool","passed":false,"error":format!("{error:#}"),"elapsedMs":started.elapsed().as_millis()})),
        }
        }
        if include("tool_roundtrip") {
            let started = Instant::now();
            let mut history = vec![
                ChatMessage::text("system", "You are a helpful assistant."),
                ChatMessage::text("user", "Get weather for Tokyo."),
            ];
            let first = generate(&provider, history.clone(), vec![weather.clone()]).await;
            let roundtrip = async {
                let events = first.context("Initial tool-call generation failed")?;
                let reasoning: String = events
                    .iter()
                    .filter_map(|event| {
                        if let ProviderEvent::ReasoningDelta { text } = event {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                let call = events
                    .iter()
                    .find_map(|e| {
                        if let ProviderEvent::ToolCall { call } = e {
                            Some(call.clone())
                        } else {
                            None
                        }
                    })
                    .context("No tool call")?;
                if call.function.name != "get_weather"
                    || serde_json::from_str::<Value>(&call.function.arguments)?
                        != json!({"city":"Tokyo"})
                {
                    anyhow::bail!("Wrong tool call or arguments");
                }
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![call.clone()]),
                    tool_call_id: None,
                    name: None,
                    reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
                });
                history.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(json!("{\"temperature\":15}")),
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                    name: Some("get_weather".into()),
                    reasoning_content: None,
                });
                let events = generate(&provider, history, vec![weather])
                    .await
                    .context("Tool-result follow-up generation failed")?;
                if events.iter().any(|e| {
                    matches!(
                        e,
                        ProviderEvent::ToolCall { .. } | ProviderEvent::ToolBatchRejected { .. }
                    )
                }) {
                    anyhow::bail!("Unexpected additional tool call");
                }
                let answer = events
                    .iter()
                    .filter_map(|e| {
                        if let ProviderEvent::TextDelta { text } = e {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<String>();
                Ok::<_, anyhow::Error>(answer)
            }
            .await;
            match roundtrip {
            Ok(answer) => results.push(json!({"run":run+1,"case":"tool_roundtrip","passed":answer.contains("15"),"answer":answer,"elapsedMs":started.elapsed().as_millis()})),
            Err(error) => results.push(json!({"run":run+1,"case":"tool_roundtrip","passed":false,"error":format!("{error:#}"),"elapsedMs":started.elapsed().as_millis()})),
        }
        }
    }
    let passed = results.iter().filter(|r| r["passed"] == true).count();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"model":config.model,"modelFamily":config.resolved_family(),"backend":config.resolved_backend(),"thinkingMode":config.thinking_mode,"context":config.context_length,"sampling":{"temperature":config.temperature,"topP":config.top_p,"topK":config.top_k,"minP":config.min_p,"seed":config.seed},"maxOutputTokens":std::env::var("HARNESS_EVAL_MAX_OUTPUT").ok().and_then(|s|s.parse::<usize>().ok()).unwrap_or(512),"selectedCases":selected,"repeats":repeats,"passed":passed,"total":results.len(),"results":results,"scope":"Synthetic tool-call arguments only; no tool effects, vision, or target-model certification."})
        )?
    );
    Ok(())
}
