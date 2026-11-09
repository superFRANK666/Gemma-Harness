use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn scripted_http(
    replies: Vec<(u16, String, String)>,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, content_type, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let request_body = loop {
                let length = socket.read(&mut buffer).await.unwrap();
                assert!(length > 0);
                bytes.extend_from_slice(&buffer[..length]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break if length == 0 {
                            Value::Null
                        } else {
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap()
                        };
                    }
                }
            };
            requests.push(request_body);
            let header=format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        }
        requests
    });
    (format!("http://{address}/v1"), task)
}

fn successful_chat_body() -> String {
    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into()
}

#[tokio::test]
async fn llama_template_token_count_includes_tools_and_skips_multimodal() {
    let (base_url, task) = scripted_http(vec![
        (
            200,
            "application/json".into(),
            json!({"data":[{"id":"gemma-4-test"}]}).to_string(),
        ),
        (
            200,
            "application/json".into(),
            json!({"prompt":"<bos>rendered prompt"}).to_string(),
        ),
        (
            200,
            "application/json".into(),
            json!({"tokens":[1,2,3,4]}).to_string(),
        ),
    ])
    .await;
    let provider = Provider::new(ConnectionConfig {
        base_url,
        model: "gemma-4-test".into(),
        backend: "llama_cpp".into(),
        ..Default::default()
    })
    .unwrap();
    let tools = vec![ToolDefinition {
        name: "lookup".into(),
        description: "Look up a value".into(),
        parameters: json!({"type":"object","properties":{"key":{"type":"string"}}}),
    }];
    let count = provider
        .count_input_tokens(
            &[ChatMessage::text("user", "hello")],
            &tools,
            512,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(count, Some(4));
    let requests = task.await.unwrap();
    assert_eq!(requests[1]["tools"][0]["function"]["name"], "lookup");
    assert_eq!(requests[2]["content"], "<bos>rendered prompt");
    assert_eq!(requests[2]["parse_special"], true);
    let image = ChatMessage {
        role: "user".into(),
        content: Some(
            json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]),
        ),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
    };
    assert_eq!(
        provider
            .count_input_tokens(&[image], &tools, 512, &CancellationToken::new())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn tokenizer_count_is_not_claimed_for_an_ambiguous_server_model() {
    let (base_url, task) = scripted_http(vec![(
        200,
        "application/json".into(),
        json!({"data":[{"id":"gemma-4-test"},{"id":"other"}]}).to_string(),
    )])
    .await;
    let provider = Provider::new(ConnectionConfig {
        base_url,
        model: "gemma-4-test".into(),
        backend: "llama_cpp".into(),
        ..Default::default()
    })
    .unwrap();
    let messages = [ChatMessage::text("user", "hello")];
    assert_eq!(
        provider
            .count_input_tokens(&messages, &[], 512, &CancellationToken::new())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        provider
            .count_input_tokens(&messages, &[], 512, &CancellationToken::new())
            .await
            .unwrap(),
        None
    );
    assert_eq!(task.await.unwrap().len(), 1);
}

#[tokio::test]
async fn functiongemma_native_stream_is_buffered_parsed_and_validated_as_a_batch() {
    let frames = [
        json!({"choices":[{"delta":{"content":"<start_function_"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"content":"call>call:lookup{query:<escape>你好, {x}<escape>,n:17}<end_function_call>"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ];
    let body = frames
        .iter()
        .map(|v| format!("data: {v}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n";
    let (base_url, task) = scripted_http(vec![(200, "text/event-stream".into(), body)]).await;
    let provider = Provider::new(ConnectionConfig {
        base_url,
        model: "functiongemma-270m".into(),
        backend: "llama_cpp".into(),
        ..Default::default()
    })
    .unwrap();
    let tools = vec![ToolDefinition {
        name: "lookup".into(),
        description: "Look up".into(),
        parameters: json!({"type":"object","properties":{"query":{"type":"string"},"n":{"type":"integer"}}}),
    }];
    let events = provider
        .collect(
            vec![
                ChatMessage::text("system", "Follow instructions"),
                ChatMessage::text("user", "lookup"),
            ],
            tools,
            512,
        )
        .await
        .unwrap();
    assert!(!events
        .iter()
        .any(|e| matches!(e, ProviderEvent::TextDelta { .. })));
    let call = events
        .iter()
        .find_map(|e| {
            if let ProviderEvent::ToolCall { call } = e {
                Some(call)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&call.function.arguments).unwrap(),
        json!({"query":"你好, {x}","n":17})
    );
    let requests = task.await.unwrap();
    assert_eq!(requests[0]["stop"][0], "<start_function_response>");
    assert!(requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .starts_with("You are a model that can do function calling"));
}

#[tokio::test]
async fn missing_model_terminal_status_is_not_replaced_by_done() {
    for (protocol,body) in [
        ("chat_completions","data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: [DONE]\n\n"),
        ("responses","data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"partial\"}\n\ndata: [DONE]\n\n"),
    ] {
        let (base_url,task)=scripted_http(vec![(200,"text/event-stream".into(),body.into())]).await;
        let provider=Provider::new(ConnectionConfig{base_url,protocol:protocol.into(),model:"test".into(),..Default::default()}).unwrap();
        assert!(provider.collect(vec![ChatMessage::text("user","hello")],vec![],128).await.is_err());
        task.await.unwrap();
    }
}

#[tokio::test]
async fn unsupported_usage_option_is_retried_once_and_cached_without_fabricating_usage() {
    let rejection = json!({"error":{"code":"unsupported_parameter","param":"stream_options","message":"stream_options is not supported"}});
    let (base_url, task) = scripted_http(vec![
        (400, "application/json".into(), rejection.to_string()),
        (200, "text/event-stream".into(), successful_chat_body()),
        (200, "text/event-stream".into(), successful_chat_body()),
    ])
    .await;
    let provider = Provider::new(ConnectionConfig {
        base_url,
        model: "gemma-i1".into(),
        ..Default::default()
    })
    .unwrap();
    let first = provider
        .collect(vec![ChatMessage::text("user", "hello")], vec![], 128)
        .await
        .unwrap();
    provider
        .clone()
        .collect(vec![ChatMessage::text("user", "again")], vec![], 128)
        .await
        .unwrap();
    assert!(!first
        .iter()
        .any(|event| matches!(event, ProviderEvent::Usage { .. })));
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].get("stream_options").is_some());
    assert!(requests[1].get("stream_options").is_none());
    assert!(requests[2].get("stream_options").is_none());
    assert!(requests
        .iter()
        .all(|body| body["model"] == "gemma-i1" && body["max_tokens"] == 128));
    assert_eq!(
        provider.transport_diagnostics()["omittedOptionalParameters"],
        json!(["stream_options"])
    );
}

#[tokio::test]
async fn optional_parameter_negotiation_is_bounded_and_keeps_tools_and_limits() {
    let first = json!({"error":{"code":"unknown_parameter","param":"stream_options.include_usage","message":"unknown parameter"}});
    let second = json!({"detail":[{"type":"extra_forbidden","loc":["body","parallel_tool_calls"],"msg":"Extra inputs are not permitted"}]});
    let (base_url, task) = scripted_http(vec![
        (400, "application/json".into(), first.to_string()),
        (422, "application/json".into(), second.to_string()),
        (200, "text/event-stream".into(), successful_chat_body()),
    ])
    .await;
    let provider = Provider::new(ConnectionConfig {
        base_url,
        model: "gemma-i1".into(),
        ..Default::default()
    })
    .unwrap();
    let tools = vec![ToolDefinition {
        name: "lookup".into(),
        description: "fixture".into(),
        parameters: json!({"type":"object"}),
    }];
    provider
        .collect(vec![ChatMessage::text("user", "hello")], tools, 128)
        .await
        .unwrap();
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2].get("stream_options").is_none()
            && requests[2].get("parallel_tool_calls").is_none()
    );
    assert_eq!(requests[0]["tools"], requests[2]["tools"]);
    assert_eq!(requests[0]["max_tokens"], requests[2]["max_tokens"]);
}

#[test]
fn compatibility_retry_never_relaxes_retention_auth_or_semantic_fields() {
    let body =
        json!({"store":false,"model":"fixed","stream_options":{"include_usage":true},"tools":[]});
    for (status, param) in [
        (400, "store"),
        (400, "model"),
        (400, "tools"),
        (401, "stream_options"),
        (500, "stream_options"),
    ] {
        let error = json!({"error":{"code":"unsupported_parameter","param":param,"message":"not supported"}});
        assert!(unsupported_optional_parameter(status, &error, &body).is_none());
    }
    assert!(unsupported_optional_parameter(
        400,
        &json!({"error":{"param":"stream_options","message":"invalid token limit"}}),
        &body
    )
    .is_none());
}

#[tokio::test]
async fn diagnostic_timeout_drops_a_stalled_stream_instead_of_reporting_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 4096];
        let _ = socket.read(&mut buffer).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\n\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let provider = Provider::new(ConnectionConfig {
        base_url: format!("http://{address}/v1"),
        model: "test".into(),
        ..Default::default()
    })
    .unwrap();
    let result = provider
        .collect_with_timeout(
            vec![ChatMessage::text("user", "hello")],
            vec![],
            128,
            Duration::from_millis(30),
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("timed out"));
    task.abort();
}

#[test]
fn tool_probe_rejects_extra_fields_and_wrong_argument_types() {
    let call = ToolCall {
        id: "probe".into(),
        kind: "function".into(),
        function: crate::FunctionCall {
            name: "harness_probe".into(),
            arguments: json!({"nonce":"n","value":17}).to_string(),
        },
    };
    assert!(validate_probe_arguments(&call, "n").is_ok());
    for args in [
        json!({"nonce":"n","value":17,"extra":true}),
        json!({"nonce":"n","value":"17"}),
        json!({"nonce":"wrong","value":17}),
    ] {
        let mut bad = call.clone();
        bad.function.arguments = args.to_string();
        assert!(validate_probe_arguments(&bad, "n").is_err());
    }
}
