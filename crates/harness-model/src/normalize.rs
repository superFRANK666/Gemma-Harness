use crate::{FunctionCall, ProviderEvent, ToolCall};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ResponsePartKind {
    Text,
    Refusal,
    ReasoningSummary,
    ReasoningText,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
    done: bool,
}

pub(crate) struct Normalizer {
    calls: BTreeMap<usize, PartialCall>,
    emitted: HashSet<String>,
    pub terminal: bool,
    pub finished: bool,
    response_parts: BTreeMap<(usize, usize, ResponsePartKind), String>,
    response_text_bytes: usize,
    fallback_prefix: String,
}

impl Default for Normalizer {
    fn default() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            calls: BTreeMap::new(),
            emitted: HashSet::new(),
            terminal: false,
            finished: false,
            response_parts: BTreeMap::new(),
            response_text_bytes: 0,
            fallback_prefix: format!("{time:x}_{:x}", SEQUENCE.fetch_add(1, Ordering::Relaxed)),
        }
    }
}

fn text(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.into());
    }
    let parts = value.as_array()?;
    let mut result = String::new();
    for part in parts {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            result.push_str(t);
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn append_fragment(target: &mut String, fragment: &str) -> Result<()> {
    if target.len().saturating_add(fragment.len()) > MAX_TOOL_ARGUMENT_BYTES {
        bail!("Tool call arguments exceed 4 MiB");
    }
    target.push_str(fragment);
    Ok(())
}

fn identity_fragment(target: &mut String, fragment: &str) -> Result<()> {
    if target.len().saturating_add(fragment.len()) > 4096 {
        bail!("Tool call identifier is too long");
    }
    if target != fragment {
        target.push_str(fragment);
    }
    Ok(())
}

impl Normalizer {
    fn call_mut(&mut self, index: usize) -> Result<&mut PartialCall> {
        if !self.calls.contains_key(&index) && self.calls.len() >= 64 {
            bail!("Backend exceeded the limit of 64 tool calls in one generation");
        }
        Ok(self.calls.entry(index).or_default())
    }

    pub fn consume(&mut self, value: &Value, responses: bool) -> Result<Vec<ProviderEvent>> {
        if let Some(error) = value.get("error").filter(|v| !v.is_null()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Backend returned an error");
            bail!("{message}");
        }
        if responses {
            self.responses(value)
        } else {
            self.chat(value)
        }
    }

    fn chat(&mut self, value: &Value) -> Result<Vec<ProviderEvent>> {
        let mut events = Vec::new();
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            if let (Some(input_tokens), Some(output_tokens)) = (
                usage.get("prompt_tokens").and_then(Value::as_u64),
                usage.get("completion_tokens").and_then(Value::as_u64),
            ) {
                events.push(ProviderEvent::Usage {
                    input_tokens,
                    output_tokens,
                });
            }
        }
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
                    continue;
                }
                let delta = choice.get("delta").or_else(|| choice.get("message"));
                if let Some(delta) = delta {
                    if let Some(content) = delta
                        .get("content")
                        .and_then(text)
                        .filter(|s| !s.is_empty())
                    {
                        self.response_text_bytes = self
                            .response_text_bytes
                            .checked_add(content.len())
                            .context("Chat output size overflow")?;
                        if self.response_text_bytes > MAX_RESPONSE_TEXT_BYTES {
                            bail!("Chat response exceeds size limit");
                        }
                        events.push(ProviderEvent::TextDelta { text: content });
                    }
                    if let Some(reasoning) = delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .and_then(text)
                        .filter(|s| !s.is_empty())
                    {
                        self.response_text_bytes = self
                            .response_text_bytes
                            .checked_add(reasoning.len())
                            .context("Chat reasoning size overflow")?;
                        if self.response_text_bytes > MAX_RESPONSE_TEXT_BYTES {
                            bail!("Chat response exceeds size limit");
                        }
                        events.push(ProviderEvent::ReasoningDelta { text: reasoning });
                    }
                    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                        for (fallback_index, call) in calls.iter().enumerate() {
                            let index = call
                                .get("index")
                                .and_then(Value::as_u64)
                                .unwrap_or(fallback_index as u64)
                                as usize;
                            let partial = self.call_mut(index)?;
                            if let Some(id) = call.get("id").and_then(Value::as_str) {
                                identity_fragment(&mut partial.id, id)?;
                            }
                            if let Some(kind) = call.get("type").and_then(Value::as_str) {
                                if kind != "function" {
                                    bail!("Unsupported tool call type: {kind}");
                                }
                            }
                            if let Some(function) = call.get("function") {
                                if let Some(name) = function.get("name").and_then(Value::as_str) {
                                    append_fragment(&mut partial.name, name)?;
                                }
                                if partial.name.len() > 4096 {
                                    bail!("Tool function name is too long");
                                }
                                if let Some(args) = function.get("arguments") {
                                    if let Some(args) = args.as_str() {
                                        append_fragment(&mut partial.arguments, args)?;
                                    } else if args.is_object() {
                                        append_fragment(&mut partial.arguments, &args.to_string())?;
                                    } else if !args.is_null() {
                                        bail!("Function arguments must be a JSON object or encoded JSON string");
                                    }
                                }
                            }
                        }
                    }
                    // Some otherwise-compatible endpoints still use the deprecated function_call field.
                    if let Some(function) = delta.get("function_call") {
                        let partial = self.call_mut(0)?;
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            append_fragment(&mut partial.name, name)?;
                        }
                        if let Some(args) = function.get("arguments").and_then(Value::as_str) {
                            append_fragment(&mut partial.arguments, args)?;
                        }
                    }
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    if reason == "content_filter" {
                        bail!("Backend stopped generation with content_filter");
                    }
                    if reason == "length" {
                        bail!("Output token limit reached; generation is incomplete and no tool was emitted");
                    }
                    if !matches!(reason, "stop" | "tool_calls" | "function_call") {
                        bail!("Backend stopped generation with unsupported reason: {reason}");
                    }
                    if matches!(reason, "tool_calls" | "function_call") && self.calls.is_empty() {
                        bail!("Backend reported tool calls without returning a tool call");
                    }
                    self.finished = true;
                    events.extend(self.complete_calls()?);
                }
            }
        }
        Ok(events)
    }

    fn responses(&mut self, value: &Value) -> Result<Vec<ProviderEvent>> {
        let mut events = Vec::new();
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let index = value
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let content_index = value
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let summary_index = value
            .get("summary_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    self.response_part(
                        index,
                        content_index,
                        ResponsePartKind::Text,
                        delta,
                        false,
                        &mut events,
                    )?;
                }
            }
            "response.output_text.done" => {
                if let Some(text) = value.get("text").and_then(Value::as_str) {
                    self.response_part(
                        index,
                        content_index,
                        ResponsePartKind::Text,
                        text,
                        true,
                        &mut events,
                    )?;
                }
            }
            "response.refusal.delta" | "response.refusal.done" => {
                let complete = event_type.ends_with(".done");
                if let Some(text) = value
                    .get(if complete { "refusal" } else { "delta" })
                    .and_then(Value::as_str)
                {
                    self.response_part(
                        index,
                        content_index,
                        ResponsePartKind::Refusal,
                        text,
                        complete,
                        &mut events,
                    )?;
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    let (part_index, kind) =
                        if event_type == "response.reasoning_summary_text.delta" {
                            (summary_index, ResponsePartKind::ReasoningSummary)
                        } else {
                            (content_index, ResponsePartKind::ReasoningText)
                        };
                    self.response_part(index, part_index, kind, delta, false, &mut events)?;
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
                if let Some(text) = value.get("text").and_then(Value::as_str) {
                    let (part_index, kind) = if event_type == "response.reasoning_summary_text.done"
                    {
                        (summary_index, ResponsePartKind::ReasoningSummary)
                    } else {
                        (content_index, ResponsePartKind::ReasoningText)
                    };
                    self.response_part(index, part_index, kind, text, true, &mut events)?;
                }
            }
            "response.content_part.done" => {
                if let Some(part) = value.get("part") {
                    self.response_content_part(index, content_index, part, &mut events)?;
                }
            }
            "response.output_item.added" => {
                if let Some(item) = value.get("item") {
                    self.response_item(index, item, false)?;
                }
            }
            "response.function_call_arguments.delta" => {
                let call = self.call_mut(index)?;
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    append_fragment(&mut call.arguments, delta)?;
                }
            }
            "response.function_call_arguments.done" => {
                let call = self.call_mut(index)?;
                if let Some(args) = value.get("arguments").and_then(Value::as_str) {
                    if args.len() > MAX_TOOL_ARGUMENT_BYTES {
                        bail!("Tool call arguments exceed 4 MiB");
                    }
                    call.arguments = args.into();
                }
                if let Some(name) = value.get("name").and_then(Value::as_str) {
                    call.name = name.into();
                }
            }
            "response.output_item.done" => {
                if let Some(item) = value.get("item") {
                    self.response_item(index, item, true)?;
                    self.response_output_item(index, item, &mut events)?;
                }
            }
            "response.completed" => {
                let response = value
                    .get("response")
                    .context("Responses completion event is missing its response object")?;
                if response.get("status").and_then(Value::as_str) != Some("completed") {
                    bail!("Responses stream completed with a non-completed status");
                }
                self.response_snapshot(response, &mut events)?;
                self.finished = true;
                self.terminal = true;
                events.extend(self.complete_calls()?);
            }
            "response.failed" | "response.incomplete" | "error" => {
                let response = value.get("response").unwrap_or(value);
                let message = response
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("message").and_then(Value::as_str))
                    .or_else(|| {
                        response
                            .pointer("/incomplete_details/reason")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("Responses generation did not complete");
                bail!("{message}");
            }
            _ => {
                // Non-streaming JSON response from servers which ignore stream=true.
                if value.get("object").and_then(Value::as_str) == Some("response") {
                    if value.get("status").and_then(Value::as_str) != Some("completed") {
                        bail!("Responses generation did not complete");
                    }
                    self.response_snapshot(value, &mut events)?;
                    self.finished = true;
                    self.terminal = true;
                    events.extend(self.complete_calls()?);
                }
            }
        }
        Ok(events)
    }

    fn response_snapshot(
        &mut self,
        response: &Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<()> {
        if let Some(output) = response.get("output").and_then(Value::as_array) {
            for (index, item) in output.iter().enumerate() {
                self.response_item(index, item, true)?;
                self.response_output_item(index, item, events)?;
            }
        }
        if let Some(usage) = response.get("usage").filter(|v| !v.is_null()) {
            if let (Some(input_tokens), Some(output_tokens)) = (
                usage.get("input_tokens").and_then(Value::as_u64),
                usage.get("output_tokens").and_then(Value::as_u64),
            ) {
                events.push(ProviderEvent::Usage {
                    input_tokens,
                    output_tokens,
                });
            }
        }
        Ok(())
    }

    fn response_part(
        &mut self,
        output_index: usize,
        part_index: usize,
        kind: ResponsePartKind,
        text: &str,
        complete: bool,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<()> {
        let key = (output_index, part_index, kind);
        if !self.response_parts.contains_key(&key) && self.response_parts.len() >= 256 {
            bail!("Responses output exceeds the content-part limit");
        }
        let previous = self
            .response_parts
            .get(&key)
            .map(String::as_str)
            .unwrap_or("");
        let suffix = if complete {
            text.strip_prefix(previous)
                .context("Responses final content differs from its streamed prefix")?
        } else {
            text
        };
        if suffix.is_empty() {
            return Ok(());
        }
        let total = self
            .response_text_bytes
            .checked_add(suffix.len())
            .context("Responses output size overflow")?;
        if total > MAX_RESPONSE_TEXT_BYTES {
            bail!("Responses text output exceeds 8 MiB");
        }
        self.response_text_bytes = total;
        self.response_parts.entry(key).or_default().push_str(suffix);
        if matches!(kind, ResponsePartKind::Text | ResponsePartKind::Refusal) {
            events.push(ProviderEvent::TextDelta {
                text: suffix.into(),
            });
        } else {
            events.push(ProviderEvent::ReasoningDelta {
                text: suffix.into(),
            });
        }
        Ok(())
    }

    fn response_content_part(
        &mut self,
        index: usize,
        part_index: usize,
        part: &Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<()> {
        let (kind, field) = match part.get("type").and_then(Value::as_str) {
            Some("output_text") => (ResponsePartKind::Text, "text"),
            Some("refusal") => (ResponsePartKind::Refusal, "refusal"),
            Some("reasoning_text") => (ResponsePartKind::ReasoningText, "text"),
            _ => return Ok(()),
        };
        if let Some(text) = part.get(field).and_then(Value::as_str) {
            self.response_part(index, part_index, kind, text, true, events)?;
        }
        Ok(())
    }

    fn response_output_item(
        &mut self,
        index: usize,
        item: &Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<()> {
        match item.get("type").and_then(Value::as_str) {
            Some("message") | Some("reasoning") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for (part_index, part) in parts.iter().enumerate() {
                        self.response_content_part(index, part_index, part, events)?;
                    }
                }
                if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                    for (part_index, part) in parts.iter().enumerate() {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            self.response_part(
                                index,
                                part_index,
                                ResponsePartKind::ReasoningSummary,
                                text,
                                true,
                                events,
                            )?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn response_item(&mut self, index: usize, item: &Value, done: bool) -> Result<()> {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Ok(());
        }
        let call = self.call_mut(index)?;
        if let Some(id) = item.get("call_id").and_then(Value::as_str) {
            call.id = id.into();
        }
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            call.name = name.into();
        }
        if let Some(args) = item.get("arguments").and_then(Value::as_str) {
            if args.len() > MAX_TOOL_ARGUMENT_BYTES {
                bail!("Tool call arguments exceed 4 MiB");
            }
            if done || call.arguments.is_empty() {
                call.arguments = args.into();
            }
        }
        call.done = done;
        Ok(())
    }

    fn complete_calls(&mut self) -> Result<Vec<ProviderEvent>> {
        // Validate the complete batch first, so one malformed call cannot partially execute its siblings.
        let mut validated = Vec::new();
        for (index, partial) in &self.calls {
            if partial.name.is_empty() {
                return Ok(vec![ProviderEvent::ToolBatchRejected {
                    message: "The tool call has no function name.".into(),
                }]);
            }
            let args: Value = match crate::parse_tool_arguments(&partial.arguments) {
                Ok(args) => args,
                Err(error) => return Ok(vec![ProviderEvent::ToolBatchRejected {
                    message: format!("Tool {} arguments are invalid JSON or contain ambiguous keys at line {}, column {}: {}. Return one complete object without duplicate keys.", partial.name.chars().take(200).collect::<String>(), error.line(), error.column(), error.to_string().chars().take(300).collect::<String>()),
                }]),
            };
            if !args.is_object() {
                return Ok(vec![ProviderEvent::ToolBatchRejected {
                    message: format!(
                        "Tool {} arguments must be a JSON object.",
                        partial.name.chars().take(200).collect::<String>()
                    ),
                }]);
            }
            let id = if partial.id.is_empty() {
                format!("call_{}_{index}", self.fallback_prefix)
            } else {
                partial.id.clone()
            };
            if !self.emitted.contains(&id) {
                validated.push(ToolCall {
                    id,
                    kind: "function".into(),
                    function: FunctionCall {
                        name: partial.name.clone(),
                        arguments: partial.arguments.clone(),
                    },
                });
            }
        }
        let mut ids = HashSet::new();
        for call in &validated {
            if !ids.insert(&call.id) {
                return Ok(vec![ProviderEvent::ToolBatchRejected {
                    message: "The tool batch contains duplicate call identifiers.".into(),
                }]);
            }
        }
        Ok(validated
            .into_iter()
            .map(|call| {
                self.emitted.insert(call.id.clone());
                ProviderEvent::ToolCall { call }
            })
            .collect())
    }

    pub fn end(&mut self, _explicit_done: bool) -> Result<()> {
        // [DONE] is a transport sentinel, not evidence that the model completed its output.
        if !self.finished {
            bail!("Backend stream ended before a valid model completion status");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bare_transport_done_does_not_complete_chat_or_responses() {
        let mut chat = Normalizer::default();
        chat.consume(
            &json!({"choices":[{"delta":{"content":"unfinished"}}]}),
            false,
        )
        .unwrap();
        assert!(chat.end(true).is_err());
        let mut responses = Normalizer::default();
        responses.consume(&json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"unfinished"}),true).unwrap();
        assert!(responses.end(true).is_err());
    }

    #[test]
    fn response_completion_requires_explicit_completed_status() {
        for event in [
            json!({"type":"response.completed"}),
            json!({"type":"response.completed","response":{}}),
            json!({"type":"response.completed","response":{"status":"in_progress"}}),
        ] {
            assert!(Normalizer::default().consume(&event, true).is_err());
        }
    }

    #[test]
    fn responses_reconcile_each_text_part_without_losing_text_or_duplicating_reasoning() {
        let mut n = Normalizer::default();
        let first=n.consume(&json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"plan"}),true).unwrap();
        assert_eq!(
            first,
            vec![ProviderEvent::ReasoningDelta {
                text: "plan".into()
            }]
        );
        n.consume(&json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"hello"}),true).unwrap();
        let final_events=n.consume(&json!({"type":"response.completed","response":{"status":"completed","output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},
            {"type":"message","content":[{"type":"output_text","text":"hello world"},{"type":"output_text","text":"tail"}]}
        ]}}),true).unwrap();
        assert_eq!(
            final_events,
            vec![
                ProviderEvent::TextDelta {
                    text: " world".into()
                },
                ProviderEvent::TextDelta {
                    text: "tail".into()
                }
            ]
        );
    }

    #[test]
    fn done_only_text_and_refusals_are_rendered_once() {
        let mut n = Normalizer::default();
        let first=n.consume(&json!({"type":"response.output_text.done","output_index":0,"content_index":0,"text":"whole text"}),true).unwrap();
        assert_eq!(
            first,
            vec![ProviderEvent::TextDelta {
                text: "whole text".into()
            }]
        );
        let repeat=n.consume(&json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","content":[{"type":"output_text","text":"whole text"}]}}),true).unwrap();
        assert!(repeat.is_empty());
        let refusal=n.consume(&json!({"type":"response.content_part.done","output_index":0,"content_index":1,"part":{"type":"refusal","refusal":"Cannot complete this request"}}),true).unwrap();
        assert_eq!(
            refusal,
            vec![ProviderEvent::TextDelta {
                text: "Cannot complete this request".into()
            }]
        );
    }

    #[test]
    fn conflicting_response_snapshot_is_not_silently_accepted() {
        let mut n = Normalizer::default();
        n.consume(&json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"original"}),true).unwrap();
        assert!(n.consume(&json!({"type":"response.output_text.done","output_index":0,"content_index":0,"text":"different"}),true).is_err());
    }

    #[test]
    fn chat_fragmented_call_is_not_emitted_before_finish() {
        let mut n = Normalizer::default();
        assert!(n.consume(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read","arguments":"{\"path\":"}}]}}]}), false).unwrap().is_empty());
        assert!(n.consume(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"你好.txt\"}"}}]}}]}), false).unwrap().is_empty());
        let events = n
            .consume(
                &json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
                false,
            )
            .unwrap();
        assert!(
            matches!(&events[0], ProviderEvent::ToolCall { call } if call.function.arguments == "{\"path\":\"你好.txt\"}")
        );
        assert!(n.end(false).is_ok());
    }

    #[test]
    fn malformed_or_truncated_calls_fail_without_execution() {
        let mut n = Normalizer::default();
        n.consume(&json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"read","arguments":"{"}}]}}]}), false).unwrap();
        assert!(n.end(true).is_err());
        assert!(n
            .consume(&json!({"choices":[{"finish_reason":"length"}]}), false)
            .is_err());
    }

    #[test]
    fn responses_arguments_done_and_item_done_do_not_duplicate() {
        let mut n = Normalizer::default();
        for value in [
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call_z","name":"lookup","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"x\":"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"3}"}),
            json!({"type":"response.function_call_arguments.done","output_index":1,"arguments":"{\"x\":3}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call_z","name":"lookup","arguments":"{\"x\":3}"}}),
        ] {
            assert!(n.consume(&value, true).unwrap().is_empty());
        }
        let events = n.consume(&json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":12,"output_tokens":4}}}), true).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|v| matches!(v, ProviderEvent::ToolCall { .. }))
                .count(),
            1
        );
        assert!(events.contains(&ProviderEvent::Usage {
            input_tokens: 12,
            output_tokens: 4
        }));
    }

    #[test]
    fn failed_responses_never_emit_tools() {
        let mut n = Normalizer::default();
        assert!(n.consume(&json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}), true).is_err());
    }

    #[test]
    fn text_length_limit_is_not_reported_complete() {
        let mut n = Normalizer::default();
        assert!(n
            .consume(
                &json!({"choices":[{"delta":{"content":"partial"},"finish_reason":"length"}]}),
                false
            )
            .is_err());
        assert!(!n.finished);
    }

    #[test]
    fn missing_usage_is_not_fabricated_as_zero() {
        let mut n = Normalizer::default();
        let events = n
            .consume(&json!({"choices":[],"usage":{"prompt_tokens":12}}), false)
            .unwrap();
        assert!(!events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Usage { .. })));
    }
}
