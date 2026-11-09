use crate::functiongemma;
use crate::image_probe::ImageProbe;
use crate::normalize::Normalizer;
use crate::normalize_tool_schema;
use crate::sse::SseDecoder;
use crate::{
    target_model_profile, ChatMessage, ConnectionConfig, ProviderEvent, ToolCall, ToolDefinition,
};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "provider_audit_tests.rs"]
mod audit_tests;

#[derive(Clone)]
pub struct Provider {
    config: ConnectionConfig,
    client: Client,
    base: Url,
    omitted_optional_parameters: Arc<Mutex<BTreeSet<String>>>,
    tokenizer_available: Arc<Mutex<Option<bool>>>,
}

impl Provider {
    pub fn new(config: ConnectionConfig) -> Result<Self> {
        if !matches!(
            config.protocol.as_str(),
            "chat_completions" | "responses" | "lmstudio"
        ) {
            bail!("Unsupported protocol; expected chat_completions, responses, or lmstudio");
        }
        if !matches!(
            config.model_family.as_str(),
            "auto" | "gemma4" | "gemma3" | "functiongemma" | "other"
        ) || !matches!(
            config.backend.as_str(),
            "auto" | "llama_cpp" | "lmstudio" | "other"
        ) || !matches!(config.thinking_mode.as_str(), "auto" | "on" | "off")
        {
            bail!("Invalid model family, backend, or thinking mode");
        }
        if let Some(output) = config.max_output_tokens {
            if output < 256 || output > config.context_length / 2 {
                bail!(
                    "Output token limit must be at least 256 and at most half the loaded context"
                );
            }
        }
        if config.thinking_mode != "auto"
            && (config.resolved_family() != "gemma4"
                || config.resolved_backend() != "llama_cpp"
                || config.protocol == "responses")
        {
            bail!("Explicit thinking control requires Gemma 4 on the llama.cpp Chat Completions backend; use auto for other backends");
        }
        if config.resolved_family() == "functiongemma"
            && config.resolved_backend() == "llama_cpp"
            && config.protocol == "responses"
        {
            bail!("FunctionGemma on llama.cpp requires Chat Completions for native tool calls");
        }
        let mut base = Url::parse(config.base_url.trim()).context("Invalid provider base URL")?;
        if !matches!(base.scheme(), "http" | "https") {
            bail!("Provider URL must use HTTP or HTTPS");
        }
        if base.host_str().is_none() {
            bail!("Provider URL needs a host");
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!("Provider URL must not contain credentials, query parameters, or a fragment; use the API key field");
        }
        base.set_path(base.path().trim_end_matches('/').to_owned().as_str());
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            config,
            client,
            base,
            omitted_optional_parameters: Arc::new(Mutex::new(BTreeSet::new())),
            tokenizer_available: Arc::new(Mutex::new(None)),
        })
    }

    fn route(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        let base_path = url.path().trim_end_matches('/');
        let endpoint = if base_path.ends_with("/v1") {
            format!("{base_path}/{path}")
        } else {
            format!("{base_path}/v1/{path}")
        };
        url.set_path(&endpoint);
        url
    }

    fn lmstudio_route(&self, suffix: &str) -> Url {
        let mut url = self.base.clone();
        let prefix = url
            .path()
            .trim_end_matches('/')
            .strip_suffix("/v1")
            .unwrap_or(url.path().trim_end_matches('/'));
        let endpoint = format!("{prefix}/api/v1/{suffix}");
        url.set_path(&endpoint);
        url
    }

    fn native_route(&self, suffix: &str) -> Url {
        let mut url = self.base.clone();
        let path = url.path().trim_end_matches('/');
        let prefix = path.strip_suffix("/v1").unwrap_or(path);
        url.set_path(&format!("{prefix}/{suffix}"));
        url
    }

    /// llama.cpp can render the actual chat template and tokenize the rendered
    /// text without generation. Image embeddings cannot be measured this way.
    pub async fn count_input_tokens(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        max_output_tokens: usize,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>> {
        if self.config.resolved_backend() != "llama_cpp"
            || self.config.protocol == "responses"
            || messages
                .iter()
                .any(|m| m.content.as_ref().is_some_and(contains_image))
            || *self.tokenizer_available.lock().unwrap() == Some(false)
        {
            return Ok(None);
        }
        let mut body = self.request_body(messages, tools, max_output_tokens)?;
        for parameter in self.omitted_optional_parameters.lock().unwrap().iter() {
            body.as_object_mut().unwrap().remove(parameter);
        }
        let measure = async {
            // Native template/tokenizer endpoints may target a server's default
            // model. Only claim an exact count when there is one matching model.
            let response = tokio::select! { biased; _ = cancel.cancelled() => bail!("Token counting cancelled"), result = self.authorize(self.client.get(self.route("models"))).timeout(Duration::from_secs(4)).send() => result? };
            if !response.status().is_success() { bail!("Model identity unavailable for tokenizer"); }
            let bytes = read_bounded(response, cancel, 4 * 1024 * 1024).await?;
            let models: Value = serde_json::from_slice(&bytes)?;
            let models = models["data"].as_array().context("Model list missing data")?;
            if models.len() != 1 || models[0]["id"].as_str() != Some(self.config.model.as_str()) {
                bail!("Tokenizer model identity is ambiguous");
            }
            let response = tokio::select! { biased; _ = cancel.cancelled() => bail!("Token counting cancelled"), result = self.authorize(self.client.post(self.native_route("apply-template"))).timeout(Duration::from_secs(4)).json(&body).send() => result? };
            if !response.status().is_success() { bail!("Template counting unavailable"); }
            let bytes = read_bounded(response, cancel, 8 * 1024 * 1024).await?;
            let value: Value = serde_json::from_slice(&bytes)?;
            let prompt = value["prompt"].as_str().context("Template response missing prompt")?;
            let request = json!({"model":self.config.model,"content":prompt,"add_special":false,"parse_special":true});
            let response = tokio::select! { biased; _ = cancel.cancelled() => bail!("Token counting cancelled"), result = self.authorize(self.client.post(self.native_route("tokenize"))).timeout(Duration::from_secs(4)).json(&request).send() => result? };
            if !response.status().is_success() { bail!("Tokenizer unavailable"); }
            let bytes = read_bounded(response, cancel, 16 * 1024 * 1024).await?;
            let value: Value = serde_json::from_slice(&bytes)?;
            let count = value["tokens"].as_array().context("Tokenizer response missing tokens")?.len();
            if count == 0 { bail!("Tokenizer returned no tokens for a rendered prompt"); }
            Ok::<_,anyhow::Error>(count)
        }.await;
        match measure {
            Ok(count) => {
                *self.tokenizer_available.lock().unwrap() = Some(true);
                Ok(Some(count))
            }
            Err(error) if cancel.is_cancelled() => Err(error),
            Err(_) => {
                *self.tokenizer_available.lock().unwrap() = Some(false);
                Ok(None)
            }
        }
    }

    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = self.config.api_key.as_deref().filter(|s| !s.is_empty()) {
            builder.bearer_auth(key)
        } else {
            builder
        }
    }

    fn safe_error(&self, message: impl AsRef<str>) -> String {
        let mut message = message.as_ref().to_string();
        if let Some(key) = self.config.api_key.as_deref().filter(|s| !s.is_empty()) {
            message = message.replace(key, "[redacted]");
        }
        // Remote error bodies are untrusted and should not fill logs with a full echoed prompt.
        if message.chars().count() > 1000 {
            message = message.chars().take(1000).collect::<String>() + "…";
        }
        message
    }

    fn request_body(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        max_output_tokens: usize,
    ) -> Result<Value> {
        if self.config.model.trim().is_empty() {
            bail!("Select a model before generating");
        }
        if max_output_tokens == 0 {
            bail!("Output token budget must be positive");
        }
        if self.config.context_length > 0 && max_output_tokens >= self.config.context_length {
            bail!("Output token budget must leave room for input context");
        }
        if self.config.protocol == "responses" {
            let input = response_input(messages)?;
            let mut body = json!({"model": self.config.model, "input": input, "stream": true, "store": false, "max_output_tokens": max_output_tokens});
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.iter().map(|tool| {
                    let parameters = normalize_tool_schema(&tool.parameters).with_context(|| format!("Invalid schema for tool {}", tool.name))?;
                    Ok(json!({"type":"function", "name":tool.name, "description":tool.description, "parameters":parameters}))
                }).collect::<Result<Vec<_>>>()?);
                body["parallel_tool_calls"] = json!(false);
            }
            Ok(body)
        } else {
            let mut wire_messages = serde_json::to_value(messages)?;
            let functiongemma_native = self.config.resolved_family() == "functiongemma";
            if functiongemma_native {
                let messages = wire_messages
                    .as_array_mut()
                    .context("Messages must be an array")?;
                let instruction =
                    "You are a model that can do function calling with the following functions";
                if let Some(system) = messages.first_mut().filter(|m| m["role"] == "system") {
                    let existing = system["content"].as_str().unwrap_or("");
                    system["content"] = json!(format!("{instruction}\n{existing}"));
                } else {
                    messages.insert(0, json!({"role":"system","content":instruction}));
                }
                if self.config.resolved_backend() == "llama_cpp" {
                    for message in messages {
                        if let Some(calls) =
                            message.get_mut("tool_calls").and_then(Value::as_array_mut)
                        {
                            for call in calls {
                                let arguments = call
                                    .pointer("/function/arguments")
                                    .and_then(Value::as_str)
                                    .context("FunctionGemma history needs JSON arguments")?;
                                let encoded = functiongemma::encode_arguments(arguments)?;
                                call["function"]["arguments"] = json!(encoded);
                            }
                        }
                    }
                }
            }
            let mut body = json!({"model":self.config.model,"messages":wire_messages,"stream":true,"stream_options":{"include_usage":true},"max_tokens":max_output_tokens});
            if functiongemma_native {
                body["stop"] = json!([functiongemma::RESPONSE_START]);
            }
            if self.config.thinking_mode != "auto" {
                body["chat_template_kwargs"] =
                    json!({"enable_thinking":self.config.thinking_mode == "on"});
                body["reasoning_format"] = json!("deepseek");
            }
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.iter().map(|tool| {
                    let parameters = normalize_tool_schema(&tool.parameters).with_context(|| format!("Invalid schema for tool {}", tool.name))?;
                    Ok(json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":parameters}}))
                }).collect::<Result<Vec<_>>>()?);
                body["parallel_tool_calls"] = json!(false);
            }
            Ok(body)
        }
    }

    /// Streams validated complete calls. A malformed or interrupted tool call is never emitted.
    pub async fn generate(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: usize,
        tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let result = self
            .generate_inner(messages, tools, max_output_tokens, &tx, &cancel)
            .await;
        if let Err(error) = &result {
            let message = self.safe_error(format!("{error:#}"));
            // Cancellation must not wait for an absent/blocked receiver.
            if !cancel.is_cancelled() {
                let _ = tx.try_send(ProviderEvent::Error { message });
            }
        }
        result.map_err(|error| anyhow!(self.safe_error(format!("{error:#}"))))
    }

    async fn generate_inner(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: usize,
        tx: &mpsc::Sender<ProviderEvent>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let responses = self.config.protocol == "responses";
        let mut body = self.request_body(&messages, &tools, max_output_tokens)?;
        for parameter in self.omitted_optional_parameters.lock().unwrap().iter() {
            body.as_object_mut().unwrap().remove(parameter);
        }
        let mut compatibility_retries = 0;
        let response = loop {
            let request = self
                .authorize(self.client.post(self.route(if responses {
                    "responses"
                } else {
                    "chat/completions"
                })))
                .json(&body);
            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("Generation cancelled"),
                response = request.send() => response.context("Could not connect to inference server")?,
            };
            let status = response.status();
            if status.is_success() {
                break response;
            }
            let bytes = read_bounded(response, cancel, 64 * 1024).await?;
            let error = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
            // Retry only a rejected request, before any successful response stream is consumed.
            // store:false, token limits, tools and model identity are never removed or changed.
            if compatibility_retries < 2 {
                if let Some(parameter) =
                    unsupported_optional_parameter(status.as_u16(), &error, &body)
                {
                    body.as_object_mut().unwrap().remove(parameter);
                    self.omitted_optional_parameters
                        .lock()
                        .unwrap()
                        .insert(parameter.into());
                    compatibility_retries += 1;
                    continue;
                }
            }
            let detail = error
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Inference server rejected the request");
            bail!("HTTP {}: {}", status.as_u16(), self.safe_error(detail));
        };
        let is_json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("application/json"));
        let mut normalizer = Normalizer::default();
        let mut explicit_done = false;
        let mut pending_calls = Vec::new();
        let mut native_text =
            (self.config.resolved_family() == "functiongemma" && !responses).then(String::new);
        let allowed_tools: HashSet<String> = tools.iter().map(|tool| tool.name.clone()).collect();
        if is_json {
            let bytes = read_bounded(response, cancel, 8 * 1024 * 1024).await?;
            let value: Value =
                serde_json::from_slice(&bytes).context("Invalid JSON from inference server")?;
            stage_events(
                tx,
                cancel,
                normalizer.consume(&value, responses)?,
                &mut pending_calls,
                &allowed_tools,
                &mut native_text,
            )
            .await?;
        } else {
            let mut stream = response.bytes_stream();
            let mut decoder = SseDecoder::default();
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("Generation cancelled"),
                    chunk = stream.next() => chunk,
                };
                match chunk {
                    Some(Ok(bytes)) => {
                        let mut stop = false;
                        for event in decoder.push(&bytes)? {
                            if event.trim().is_empty() {
                                continue;
                            }
                            if event.trim() == "[DONE]" {
                                explicit_done = true;
                                stop = true;
                                break;
                            }
                            let value: Value = serde_json::from_str(&event)
                                .context("Invalid JSON in inference SSE event")?;
                            stage_events(
                                tx,
                                cancel,
                                normalizer.consume(&value, responses)?,
                                &mut pending_calls,
                                &allowed_tools,
                                &mut native_text,
                            )
                            .await?;
                            if normalizer.terminal {
                                stop = true;
                                break;
                            }
                        }
                        if stop {
                            break;
                        }
                    }
                    Some(Err(error)) => return Err(error).context("Inference stream disconnected"),
                    None => {
                        for event in decoder.finish()? {
                            if event.trim().is_empty() {
                                continue;
                            }
                            if event.trim() == "[DONE]" {
                                explicit_done = true;
                                break;
                            }
                            let value: Value = serde_json::from_str(&event)
                                .context("Invalid JSON at end of inference stream")?;
                            stage_events(
                                tx,
                                cancel,
                                normalizer.consume(&value, responses)?,
                                &mut pending_calls,
                                &allowed_tools,
                                &mut native_text,
                            )
                            .await?;
                        }
                        break;
                    }
                }
            }
        }
        normalizer.end(explicit_done)?;
        if let Some(text) = native_text.take() {
            if text.contains(functiongemma::CALL_START) {
                if !pending_calls.is_empty() {
                    bail!("FunctionGemma returned both structured and native tool calls");
                }
                match functiongemma::parse_calls(&text) {
                    Ok(calls) => {
                        let events = calls
                            .into_iter()
                            .map(|call| ProviderEvent::ToolCall { call })
                            .collect();
                        stage_events(
                            tx,
                            cancel,
                            events,
                            &mut pending_calls,
                            &allowed_tools,
                            &mut None,
                        )
                        .await?;
                    }
                    Err(error) => pending_calls.push(ProviderEvent::ToolBatchRejected {
                        message: format!("Invalid FunctionGemma call: {error:#}"),
                    }),
                }
            } else if text.contains("<start_function_") || text.contains("<end_function_") {
                pending_calls.push(ProviderEvent::ToolBatchRejected {
                    message: "Incomplete FunctionGemma control tokens in model output.".into(),
                });
            } else if !text.is_empty() {
                send_all(tx, cancel, vec![ProviderEvent::TextDelta { text }]).await?;
            }
        }
        send_all(tx, cancel, pending_calls).await?;
        send_all(tx, cancel, vec![ProviderEvent::Completed]).await
    }

    /// Native LM Studio metadata is optional. Only GET requests are used here.
    pub async fn discover(&self) -> Result<Value> {
        let models = self.get_json(self.route("models")).await?;
        let native = if self.config.protocol == "lmstudio" {
            match self.get_json(self.lmstudio_route("models")).await {
                Ok(value) => json!({"status":"available","data":value}),
                Err(error) => {
                    json!({"status":"unavailable","message":self.safe_error(error.to_string())})
                }
            }
        } else {
            json!({"status":"not_requested"})
        };
        Ok(
            json!({"models":models,"native":native,"selectedModel":self.config.model,"configuredContextLength":self.config.context_length}),
        )
    }

    async fn get_json(&self, url: Url) -> Result<Value> {
        let response = self
            .authorize(self.client.get(url))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("Model discovery connection failed")?;
        if !response.status().is_success() {
            bail!("Model discovery HTTP {}", response.status().as_u16());
        }
        let bytes = read_bounded(response, &CancellationToken::new(), 4 * 1024 * 1024).await?;
        serde_json::from_slice(&bytes).context("Model discovery returned invalid JSON")
    }

    /// Runs a harmless, real function-call/result/answer cycle. It never invokes user tools.
    pub async fn diagnose(&self) -> Result<Value> {
        let started = Instant::now();
        let discovery = match self.discover().await {
            Ok(value) => json!({"status":"passed","data":value}),
            Err(error) => json!({"status":"failed","message":self.safe_error(error.to_string())}),
        };
        let nonce = format!(
            "harness_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut messages = vec![
            ChatMessage::text("system", "You are performing a connection diagnostic. Follow the user's tool instructions exactly. Do not invent tool results."),
            ChatMessage::text("user", format!("Call harness_probe once with nonce equal to {nonce} and value equal to 17. Then report the exact result marker returned by the tool.")),
        ];
        let tools = vec![ToolDefinition {
            name: "harness_probe".into(),
            description:
                "Harmless connection check returning a marker; no computer or files are accessed."
                    .into(),
            parameters: json!({"type":"object","properties":{"nonce":{"type":"string"},"value":{"type":"integer"}},"required":["nonce","value"],"additionalProperties":false}),
        }];
        let tool_probe = async {
            let events = self.collect(messages.clone(), tools, self.probe_budget()).await?;
            let reasoning = reasoning_text(&events);
            let reasoning_seen = reasoning.is_some();
            let calls: Vec<ToolCall> = events.into_iter().filter_map(|event| if let ProviderEvent::ToolCall { call } = event { Some(call) } else { None }).collect();
            if calls.len() != 1 { bail!("Expected one harness_probe call; received {}", calls.len()); }
            let call = &calls[0];
            validate_probe_arguments(call,&nonce)?;
            let marker = format!("verified_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
            messages.push(ChatMessage { role:"assistant".into(), content:None, tool_calls:Some(calls.clone()), tool_call_id:None, name:None, reasoning_content:reasoning });
            messages.push(ChatMessage { role:"tool".into(), content:Some(json!({"marker":marker}).to_string().into()), tool_calls:None, tool_call_id:Some(call.id.clone()), name:Some("harness_probe".into()), reasoning_content:None });
            let final_events = self.collect(messages, vec![], self.probe_budget()).await?;
            let answer = final_events.into_iter().filter_map(|event| if let ProviderEvent::TextDelta { text } = event { Some(text) } else { None }).collect::<String>();
            if !answer.contains(&marker) { bail!("Tool call parsed, but final answer did not contain the actual tool result marker"); }
            Ok::<_,anyhow::Error>(json!({"status":"passed","callCount":1,"resultRoundtrip":true,"reasoningObserved":reasoning_seen}))
        }.await;
        let (tool_result, reasoning_observed) = match tool_probe {
            Ok(result) => (result.clone(), result["reasoningObserved"].as_bool()),
            Err(error) => (diagnostic_result(Err(error)), None),
        };
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let image_input = diagnostic_result(self.probe_image_input(seed).await);
        let image_tool = if image_input["status"] == "passed" {
            diagnostic_result(self.probe_image_tool(seed.wrapping_add(1)).await)
        } else {
            json!({"status":"unknown","tested":false,"message":"Image tool results were not tested because image input did not pass."})
        };
        let vision_passed = image_input["status"] == "passed" && image_tool["status"] == "passed";
        Ok(json!({
            "status":if tool_result["status"] == "passed" && vision_passed { "passed" } else if tool_result["status"] == "passed" { "partial" } else { "failed" },
            "discovery":discovery,"toolCalling":tool_result,
            "thinking":{"requested":self.config.thinking_mode,"observedInToolProbe":reasoning_observed,"status":match (self.config.thinking_mode.as_str(),reasoning_observed) {("off",Some(true))=>"mismatch",(_,Some(true))=>"observed",(_,Some(false))=>"not_observed",_=>"unknown"},"message":"A missing thought in one probe does not prove the backend disabled thinking; verify the actual template and runtime."},
            "vision":{"status":if vision_passed { "passed" } else { "failed" },"tested":true,"imageInput":image_input,"imageToolResult":image_tool,"message":"Diagnostic tests verify shuffled color quadrants, not desktop-task accuracy."},
            "targetProfile":target_model_profile(),
            "configuredCapabilities":{"modelFamily":self.config.resolved_family(),"familySource":if self.config.model_family == "auto" {"inferred_from_model_id"} else {"configured"},"backend":self.config.resolved_backend(),"thinkingMode":self.config.thinking_mode,"templateOwner":"inference_server","capabilitiesVerifiedByProfile":false},
            "exactModelArtifact":{"status":"unknown","message":"Repository profile is known; the endpoint's exact GGUF hash, embedded template, projector and runtime version have not been verified."},
            "transport":self.transport_diagnostics(),
            "elapsedMs":started.elapsed().as_millis() as u64
        }))
    }

    async fn probe_image_input(&self, seed: u128) -> Result<()> {
        let probe = ImageProbe::new(seed);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type":"text","text":ImageProbe::instruction()},
                {"type":"image_url","image_url":{"url":probe.data_url}}
            ])),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        }];
        let events = self.collect(messages, vec![], self.probe_budget()).await?;
        probe.validate(&answer_text(events))
    }

    /// Recheck both ordinary image input and image-bearing tool responses after
    /// a remotely managed endpoint may have changed behind the same model ID.
    pub async fn revalidate_vision(&self) -> Result<()> {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.probe_image_input(seed).await?;
        self.probe_image_tool(seed.wrapping_add(1)).await
    }

    async fn probe_image_tool(&self, seed: u128) -> Result<()> {
        let probe = ImageProbe::new(seed);
        let mut messages = vec![ChatMessage::text("user", format!("Call harness_capture_probe once with no arguments to get a diagnostic image. After receiving it: {}", ImageProbe::instruction()))];
        let tools = vec![ToolDefinition {
            name: "harness_capture_probe".into(),
            description:
                "Returns a generated diagnostic image. No desktop or user data is accessed.".into(),
            parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
        }];
        let events = self
            .collect(messages.clone(), tools, self.probe_budget())
            .await?;
        let reasoning = reasoning_text(&events);
        let calls: Vec<ToolCall> = events
            .into_iter()
            .filter_map(|event| {
                if let ProviderEvent::ToolCall { call } = event {
                    Some(call)
                } else {
                    None
                }
            })
            .collect();
        if calls.len() != 1
            || calls[0].function.name != "harness_capture_probe"
            || serde_json::from_str::<Value>(&calls[0].function.arguments)? != json!({})
        {
            bail!("Image tool probe did not return the expected empty-argument function call");
        }
        let call_id = calls[0].id.clone();
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
            name: None,
            reasoning_content: reasoning,
        });
        messages.push(ChatMessage {
            role: "tool".into(),
            content: Some(json!([
                {"type":"text","text":ImageProbe::instruction()},
                {"type":"image_url","image_url":{"url":probe.data_url}}
            ])),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: Some("harness_capture_probe".into()),
            reasoning_content: None,
        });
        let events = self.collect(messages, vec![], self.probe_budget()).await?;
        probe.validate(&answer_text(events))
    }

    async fn collect(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        budget: usize,
    ) -> Result<Vec<ProviderEvent>> {
        self.collect_with_timeout(messages, tools, budget, Duration::from_secs(90))
            .await
    }

    fn probe_budget(&self) -> usize {
        self.config
            .output_budget()
            .max(1024)
            .min(self.config.context_length / 2)
            .max(128)
    }

    async fn collect_with_timeout(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        budget: usize,
        timeout: Duration,
    ) -> Result<Vec<ProviderEvent>> {
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let generation = self.generate(messages, tools, budget, tx, cancel.clone());
        let collect = async move {
            let mut result = Vec::new();
            while let Some(event) = rx.recv().await {
                result.push(event);
            }
            result
        };
        let combined = async {
            let (result, events) = tokio::join!(generation, collect);
            result?;
            Ok(events)
        };
        match tokio::time::timeout(timeout, combined).await {
            Ok(result) => result,
            Err(_) => {
                cancel.cancel();
                bail!(
                    "Diagnostic generation timed out after {} milliseconds",
                    timeout.as_millis()
                );
            }
        }
    }

    fn transport_diagnostics(&self) -> Value {
        json!({"protocol":self.config.protocol,"omittedOptionalParameters":self.omitted_optional_parameters.lock().unwrap().iter().cloned().collect::<Vec<_>>(),
            "compatibilityDetection":"explicit_http_400_or_422_parameter_rejection_only",
            "timeoutSeconds":{"connect":10,"request":600,"diagnosticGeneration":90}})
    }
}

fn contains_image(value: &Value) -> bool {
    match value {
        Value::Array(parts) => parts.iter().any(contains_image),
        Value::Object(object) => {
            matches!(
                object.get("type").and_then(Value::as_str),
                Some("image_url" | "input_image")
            ) || object.values().any(contains_image)
        }
        _ => false,
    }
}

fn validate_probe_arguments(call: &ToolCall, nonce: &str) -> Result<()> {
    let args: Value = serde_json::from_str(&call.function.arguments)?;
    if call.function.name != "harness_probe"
        || args.as_object().map(|a| a.len()) != Some(2)
        || args.get("nonce").and_then(Value::as_str) != Some(nonce)
        || args.get("value").and_then(Value::as_i64) != Some(17)
    {
        bail!("Model did not return the exact probe schema, tool name and arguments");
    }
    Ok(())
}

fn unsupported_optional_parameter(
    status: u16,
    error: &Value,
    body: &Value,
) -> Option<&'static str> {
    if !matches!(status, 400 | 422) {
        return None;
    }
    const OPTIONAL: [&str; 2] = ["stream_options", "parallel_tool_calls"];
    let code = error
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let unsupported = matches!(
        code,
        "unsupported_parameter" | "unknown_parameter" | "unrecognized_parameter"
    ) || [
        "not supported",
        "unsupported",
        "unrecognized",
        "unrecognised",
        "unknown parameter",
        "unexpected keyword",
        "extra inputs are not permitted",
    ]
    .iter()
    .any(|word| message.contains(word));
    if unsupported {
        if let Some(param) = error.pointer("/error/param").and_then(Value::as_str) {
            let top = param.split('.').next().unwrap_or(param);
            if let Some(name) = OPTIONAL
                .into_iter()
                .find(|name| *name == top && body.get(*name).is_some())
            {
                return Some(name);
            }
        }
    }
    // FastAPI/Pydantic rejects optional top-level fields with structured extra_forbidden details.
    if let Some(details) = error.get("detail").and_then(Value::as_array) {
        for detail in details {
            if detail.get("type").and_then(Value::as_str) != Some("extra_forbidden") {
                continue;
            }
            let location = detail.get("loc").and_then(Value::as_array)?;
            if location.first().and_then(Value::as_str) != Some("body") {
                continue;
            }
            let parameter = location.get(1).and_then(Value::as_str)?;
            if let Some(name) = OPTIONAL
                .into_iter()
                .find(|name| *name == parameter && body.get(*name).is_some())
            {
                return Some(name);
            }
        }
    }
    None
}

async fn stage_events(
    tx: &mpsc::Sender<ProviderEvent>,
    cancel: &CancellationToken,
    events: Vec<ProviderEvent>,
    pending_calls: &mut Vec<ProviderEvent>,
    allowed_tools: &HashSet<String>,
    native_text: &mut Option<String>,
) -> Result<()> {
    let mut immediate = Vec::new();
    for event in events {
        match event {
            ProviderEvent::TextDelta { text } if native_text.is_some() => {
                let buffer = native_text.as_mut().unwrap();
                if buffer.len().saturating_add(text.len()) > 4 * 1024 * 1024 {
                    bail!("FunctionGemma output exceeds buffer limit");
                }
                buffer.push_str(&text);
            }
            ProviderEvent::ToolCall { ref call }
                if !allowed_tools.contains(&call.function.name) =>
            {
                pending_calls.clear();
                pending_calls.push(ProviderEvent::ToolBatchRejected { message:format!("Tool {} was not advertised in this request. Use an available tool or tool_search.", call.function.name.chars().take(200).collect::<String>()) });
            }
            ProviderEvent::ToolCall { .. } => {
                if !pending_calls
                    .iter()
                    .any(|e| matches!(e, ProviderEvent::ToolBatchRejected { .. }))
                {
                    pending_calls.push(event);
                }
            }
            ProviderEvent::ToolBatchRejected { .. } => {
                pending_calls.clear();
                pending_calls.push(event);
            }
            _ => immediate.push(event),
        }
    }
    send_all(tx, cancel, immediate).await
}

fn answer_text(events: Vec<ProviderEvent>) -> String {
    events
        .into_iter()
        .filter_map(|event| {
            if let ProviderEvent::TextDelta { text } = event {
                Some(text)
            } else {
                None
            }
        })
        .collect()
}

fn reasoning_text(events: &[ProviderEvent]) -> Option<String> {
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
    if reasoning.is_empty() {
        None
    } else {
        Some(reasoning)
    }
}

fn diagnostic_result(result: Result<()>) -> Value {
    match result {
        Ok(()) => json!({"status":"passed","tested":true}),
        Err(error) => json!({"status":"failed","tested":true,"message":format!("{error:#}")}),
    }
}

async fn send_all(
    tx: &mpsc::Sender<ProviderEvent>,
    cancel: &CancellationToken,
    events: Vec<ProviderEvent>,
) -> Result<()> {
    for event in events {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Generation cancelled"),
            result = tx.send(event) => result.context("Generation receiver was closed")?,
        }
    }
    Ok(())
}

async fn read_bounded(
    response: reqwest::Response,
    cancel: &CancellationToken,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! { biased; _ = cancel.cancelled() => bail!("Request cancelled"), chunk = stream.next() => chunk };
        match chunk {
            Some(Ok(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > limit {
                    bail!("Inference server response exceeded size limit");
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Err(error)) => {
                return Err(error).context("Could not read inference server response")
            }
            None => return Ok(bytes),
        }
    }
}

fn response_input(messages: &[ChatMessage]) -> Result<Vec<Value>> {
    let mut input = Vec::new();
    for message in messages {
        if message.role == "assistant" {
            if let Some(reasoning) = message
                .reasoning_content
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                input.push(json!({"type":"reasoning","summary":[{"type":"summary_text","text":reasoning}]}));
            }
        }
        if message.role == "tool" {
            let call_id = message
                .tool_call_id
                .as_ref()
                .ok_or_else(|| anyhow!("Tool response needs tool_call_id"))?;
            let content = message
                .content
                .clone()
                .unwrap_or(Value::String(String::new()));
            let output = if content.is_array() {
                convert_content(&content)?
            } else if let Some(text) = content.as_str() {
                json!(text)
            } else {
                json!(content.to_string())
            };
            input.push(json!({"type":"function_call_output","call_id":call_id,"output":output}));
            continue;
        }
        if let Some(content) = message.content.as_ref().filter(|v| !v.is_null()) {
            let content = if content.is_array() {
                convert_content(content)?
            } else {
                content.clone()
            };
            if content.as_str() != Some("") {
                input.push(json!({"role":message.role,"content":content}));
            }
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                input.push(json!({"type":"function_call","call_id":call.id,"name":call.function.name,"arguments":call.function.arguments}));
            }
        }
    }
    Ok(input)
}

fn convert_content(content: &Value) -> Result<Value> {
    let mut parts = Vec::new();
    for part in content
        .as_array()
        .ok_or_else(|| anyhow!("Expected content parts array"))?
    {
        match part.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" | "input_text" | "output_text" => {
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Text content part needs text"))?;
                parts.push(json!({"type":"input_text","text":text}));
            }
            "image_url" => {
                let url = part
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("image_url").and_then(Value::as_str))
                    .ok_or_else(|| anyhow!("Image part needs image_url"))?;
                parts.push(json!({"type":"input_image","image_url":url}));
            }
            "input_image" => parts.push(part.clone()),
            kind => bail!("Unsupported Responses content part: {kind}"),
        }
    }
    Ok(Value::Array(parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemma4_thinking_control_is_backend_specific_and_never_silently_applied_elsewhere() {
        let config = ConnectionConfig {
            model: "google/gemma-4-26B-A4B-it".into(),
            backend: "llama_cpp".into(),
            thinking_mode: "on".into(),
            ..Default::default()
        };
        assert_eq!(config.resolved_family(), "gemma4");
        let provider = Provider::new(config.clone()).unwrap();
        let body = provider
            .request_body(&[ChatMessage::text("user", "hello")], &[], 512)
            .unwrap();
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["reasoning_format"], "deepseek");
        let mut disabled = config.clone();
        disabled.thinking_mode = "off".into();
        assert_eq!(
            Provider::new(disabled)
                .unwrap()
                .request_body(&[], &[], 512)
                .unwrap()["chat_template_kwargs"]["enable_thinking"],
            false
        );
        let mut other = config.clone();
        other.backend = "lmstudio".into();
        assert!(Provider::new(other).is_err());
        let mut legacy = config;
        legacy.model = "google/gemma-3-4b-it".into();
        assert_eq!(legacy.resolved_family(), "gemma3");
        assert!(Provider::new(legacy).is_err());
        let function = ConnectionConfig {
            model: "google/functiongemma-270m-it".into(),
            ..Default::default()
        };
        assert_eq!(function.resolved_family(), "functiongemma");
        let auto = Provider::new(function)
            .unwrap()
            .request_body(&[], &[], 512)
            .unwrap();
        assert!(auto.get("chat_template_kwargs").is_none());
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn mock_http(
        body: String,
        content_type: &str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        mock_http_with_extra_length(body, content_type, 0).await
    }

    async fn mock_http_with_extra_length(
        body: String,
        content_type: &str,
        extra_length: usize,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let content_type = content_type.to_owned();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]);
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let header = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len() + extra_length);
            stream.write_all(header.as_bytes()).await.unwrap();
            // Write one byte at a time to cover transport boundaries within UTF-8 and JSON.
            for byte in body.bytes() {
                stream.write_all(&[byte]).await.unwrap();
            }
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}/v1"), task)
    }

    #[tokio::test]
    async fn chat_http_stream_normalizes_unicode_and_actual_usage() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"你好🙂\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n";
        let (base_url, task) = mock_http(body.into(), "text/event-stream").await;
        let provider = Provider::new(ConnectionConfig {
            base_url,
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        let events = provider
            .collect(vec![ChatMessage::text("user", "hi")], vec![], 128)
            .await
            .unwrap();
        assert!(events.contains(&ProviderEvent::TextDelta {
            text: "你好🙂".into()
        }));
        assert!(events.contains(&ProviderEvent::Usage {
            input_tokens: 8,
            output_tokens: 3
        }));
        assert_eq!(events.last(), Some(&ProviderEvent::Completed));
        assert!(task
            .await
            .unwrap()
            .starts_with("POST /v1/chat/completions HTTP/1.1"));
    }

    #[tokio::test]
    async fn abrupt_stream_is_reported_as_error() {
        let (base_url, task) = mock_http(
            "data: {\"choices\":[{\"delta\":{\"content\":\"unfinished\"}}]}\n\n".into(),
            "text/event-stream",
        )
        .await;
        let provider = Provider::new(ConnectionConfig {
            base_url,
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(provider
            .collect(vec![ChatMessage::text("user", "hi")], vec![], 128)
            .await
            .is_err());
        task.await.unwrap();
    }

    #[tokio::test]
    async fn transport_failure_after_finish_never_releases_tool_calls() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
        );
        let (base_url, task) =
            mock_http_with_extra_length(body.into(), "text/event-stream", 100).await;
        let provider = Provider::new(ConnectionConfig {
            base_url,
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let generation = provider.generate(
            vec![ChatMessage::text("user", "lookup")],
            vec![ToolDefinition {
                name: "lookup".into(),
                description: String::new(),
                parameters: json!({"type":"object"}),
            }],
            128,
            tx,
            CancellationToken::new(),
        );
        let receive = async {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        };
        let (result, events) = tokio::join!(generation, receive);
        assert!(result.is_err());
        assert!(!events.iter().any(|event| matches!(
            event,
            ProviderEvent::ToolCall { .. } | ProviderEvent::Completed
        )));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn functiongemma_native_call_is_not_released_after_transport_failure() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<start_function_call>call:lookup{}<end_function_call>\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"
        );
        let (base_url, task) =
            mock_http_with_extra_length(body.into(), "text/event-stream", 100).await;
        let provider = Provider::new(ConnectionConfig {
            base_url,
            model: "functiongemma-test".into(),
            backend: "llama_cpp".into(),
            ..Default::default()
        })
        .unwrap();
        let tools = vec![ToolDefinition {
            name: "lookup".into(),
            description: String::new(),
            parameters: json!({"type":"object"}),
        }];
        let (tx, mut rx) = mpsc::channel(16);
        let generation = provider.generate(
            vec![ChatMessage::text("user", "lookup")],
            tools,
            128,
            tx,
            CancellationToken::new(),
        );
        let receive = async {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        };
        let (result, events) = tokio::join!(generation, receive);
        assert!(result.is_err());
        assert!(!events.iter().any(|e| matches!(
            e,
            ProviderEvent::ToolCall { .. }
                | ProviderEvent::ToolBatchRejected { .. }
                | ProviderEvent::TextDelta { .. }
        )));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn unadvertised_tool_is_rejected() {
        let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"unregistered\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
        let (base_url, task) = mock_http(body.into(), "text/event-stream").await;
        let provider = Provider::new(ConnectionConfig {
            base_url,
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        let events = provider
            .collect(vec![ChatMessage::text("user", "hi")], vec![], 128)
            .await
            .unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, ProviderEvent::ToolBatchRejected { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, ProviderEvent::ToolCall { .. })));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn cancellation_stops_before_request() {
        let provider = Provider::new(ConnectionConfig {
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, mut rx) = mpsc::channel(2);
        assert!(provider
            .generate(vec![], vec![], 10, tx, cancel)
            .await
            .is_err());
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn responses_http_call_is_complete_and_never_duplicated() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_r\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"value\\\":7}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_r\",\"name\":\"lookup\",\"arguments\":\"{\\\"value\\\":7}\"}],\"usage\":{\"input_tokens\":11,\"output_tokens\":5}}}\n\n"
        );
        let (base_url, task) = mock_http(body.into(), "text/event-stream").await;
        let provider = Provider::new(ConnectionConfig {
            base_url,
            protocol: "responses".into(),
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        let events = provider
            .collect(
                vec![ChatMessage::text("user", "lookup")],
                vec![ToolDefinition {
                    name: "lookup".into(),
                    description: String::new(),
                    parameters: json!({"type":"object"}),
                }],
                128,
            )
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ProviderEvent::ToolCall { .. }))
                .count(),
            1
        );
        assert!(events.contains(&ProviderEvent::Usage {
            input_tokens: 11,
            output_tokens: 5
        }));
        let request = task.await.unwrap();
        assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert!(body["tools"][0].get("function").is_none());
    }

    #[tokio::test]
    async fn unreachable_endpoint_never_claims_diagnostic_pass() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        drop(listener);
        let provider = Provider::new(ConnectionConfig {
            base_url,
            model: "test".into(),
            ..Default::default()
        })
        .unwrap();
        let report = provider.diagnose().await.unwrap();
        assert_eq!(report["status"], "failed");
        assert_eq!(report["toolCalling"]["status"], "failed");
        assert_eq!(report["vision"]["imageInput"]["status"], "failed");
        assert_eq!(report["vision"]["imageToolResult"]["tested"], false);
        assert_eq!(report["exactModelArtifact"]["status"], "unknown");
    }

    #[test]
    fn api_key_is_redacted_from_debug_and_serialization() {
        let config = ConnectionConfig {
            api_key: Some("private-test-token".into()),
            ..Default::default()
        };
        assert!(!format!("{config:?}").contains("private-test-token"));
        assert!(serde_json::to_value(&config)
            .unwrap()
            .get("apiKey")
            .is_none());
    }

    #[test]
    fn responses_tool_result_keeps_call_id_and_image_parts() {
        let messages = vec![ChatMessage {
            role: "tool".into(),
            content: Some(
                json!([{"type":"text","text":"screen"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]),
            ),
            tool_calls: None,
            tool_call_id: Some("call_screen".into()),
            name: None,
            reasoning_content: None,
        }];
        let input = response_input(&messages).unwrap();
        assert_eq!(input[0]["call_id"], "call_screen");
        assert_eq!(input[0]["output"][1]["type"], "input_image");
    }

    #[test]
    fn credentials_are_not_allowed_inside_url() {
        assert!(Provider::new(ConnectionConfig {
            base_url: "http://user:secret@localhost:1234/v1".into(),
            ..Default::default()
        })
        .is_err());
    }
}
