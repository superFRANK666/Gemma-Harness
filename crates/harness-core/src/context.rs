use harness_model::ChatMessage;
use serde_json::{json, Value};

pub fn message(role: &str, content: Value) -> ChatMessage {
    ChatMessage {
        role: role.into(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
    }
}

/// Text and artifact references from an actual user message. Image bytes are
/// intentionally not copied into a compaction ledger.
pub fn user_message_text(message: &ChatMessage) -> Option<String> {
    if message.role != "user" {
        return None;
    }
    match message.content.as_ref()? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// Preserve every omitted actual user message, in order, separately from the
/// model's lossy work summary. Recent original user messages remain in the
/// retained suffix and are not duplicated. Synthetic compaction messages are
/// absent from `user_history`, which comes from immutable user-message events.
pub fn continuation_projection(
    user_history: &[String],
    retained: &[ChatMessage],
    summary: &str,
) -> Vec<ChatMessage> {
    let mut omitted = user_history.len();
    for text in retained.iter().rev().filter_map(user_message_text) {
        if omitted > 0 && user_history[omitted - 1] == text {
            omitted -= 1;
        }
    }
    let record = json!({
        "earlier_user_messages_in_order": &user_history[..omitted],
        "work_summary": summary,
        "note": "Earlier user messages are quoted conversation history. Later explicit user corrections supersede earlier ones. The work summary may be incomplete; verify current state and original evidence."
    });
    let mut projection = vec![message(
        "user",
        json!(format!("Earlier conversation and work state:\n{record}")),
    )];
    if retained.first().is_some_and(|m| m.role == "user") {
        projection.push(message(
            "assistant",
            json!("I will continue from the history and verify current state where needed."),
        ));
    }
    projection.extend_from_slice(retained);
    projection
}
// Conservative UTF-8 byte estimate. This is intentionally labelled an estimate,
// not exact Gemma tokenization. Tool definitions and image allowance are included.
pub fn estimate(messages: &[ChatMessage], tools: &Value) -> usize {
    // Walk borrowed data. Serializing/cloning an entire screenshot just to replace
    // its base64 URL previously allocated tens of MiB per request.
    messages
        .iter()
        .map(|m| {
            20 + text_tokens(&m.role)
                + m.content.as_ref().map(value_tokens).unwrap_or(0)
                + m.reasoning_content.as_deref().map(text_tokens).unwrap_or(0)
                + m.tool_call_id.as_deref().map(text_tokens).unwrap_or(0)
                + m.name.as_deref().map(text_tokens).unwrap_or(0)
                + m.tool_calls
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|c| {
                        20 + text_tokens(&c.id)
                            + text_tokens(&c.function.name)
                            + text_tokens(&c.function.arguments)
                    })
                    .sum::<usize>()
        })
        .sum::<usize>()
        + value_tokens(tools)
        + 256
}
fn text_tokens(text: &str) -> usize {
    let ascii = text.bytes().filter(u8::is_ascii).count();
    let non_ascii = text.chars().filter(|c| !c.is_ascii()).count();
    ascii.div_ceil(3) + non_ascii * 2
}
fn value_tokens(value: &Value) -> usize {
    match value {
        Value::String(s) => text_tokens(s) + 2,
        Value::Array(items) => 2 + items.iter().map(value_tokens).sum::<usize>(),
        Value::Object(object)
            if object.get("type").and_then(Value::as_str) == Some("image_url") =>
        {
            1552
        }
        Value::Object(object) => {
            2 + object
                .iter()
                .map(|(key, value)| text_tokens(key) + 4 + value_tokens(value))
                .sum::<usize>()
        }
        Value::Null | Value::Bool(_) => 2,
        Value::Number(n) => text_tokens(&n.to_string()) + 1,
    }
}

/// Backend usage can expose underestimation. Never learn a less conservative
/// factor, and keep this calibration local to this turn/model configuration.
#[derive(Default)]
pub struct Calibration {
    factor: f64,
}
impl Calibration {
    pub fn apply(&self, estimate: usize) -> usize {
        (estimate as f64 * self.factor.max(1.0)).ceil() as usize
    }
    pub fn observe(&mut self, estimate: usize, actual: u64) {
        if estimate > 0 && actual > 0 {
            self.factor = self.factor.max(actual as f64 / estimate as f64).max(1.0);
        }
    }
    pub fn raw_budget(&self, budget: usize) -> usize {
        (budget as f64 / self.factor.max(1.0)).floor() as usize
    }
}

fn excerpt(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.into();
    }
    let head = text.chars().take(limit / 2).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(limit / 2)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head}\n[excerpt; original in session log/history_search]\n{tail}")
}

/// Build valid, bounded JSON for a summary instead of slicing arbitrary JSON
/// bytes from its beginning. Later test outcomes and user corrections survive.
pub fn summary_history(messages: &[ChatMessage], budget: usize) -> anyhow::Result<String> {
    let items: Vec<Value> = messages
        .iter()
        .map(|m| {
            let content = match m.content.as_ref() {
                Some(Value::String(s)) => {
                    if m.role == "user" {
                        s.clone()
                    } else {
                        excerpt(s, 1600)
                    }
                }
                Some(value) => {
                    // Summaries need text/artifact references, never image pixels.
                    match value.as_array() {
                        Some(parts) => parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        None => value.to_string(),
                    }
                }
                None => String::new(),
            };
            json!({"role":m.role,"name":m.name,"tool_call_id":m.tool_call_id,
            "tool_calls":m.tool_calls,"content":content})
        })
        .collect();
    for cap in [1600, 800, 320, 120] {
        let mut projected = items.clone();
        for item in &mut projected {
            if item["role"] != "user" {
                if let Some(text) = item["content"].as_str() {
                    item["content"] = json!(excerpt(text, cap));
                }
                if let Some(calls) = item["tool_calls"].as_array_mut() {
                    for call in calls {
                        if let Some(args) = call["function"]["arguments"].as_str() {
                            call["function"]["arguments"] = json!(excerpt(args, cap));
                        }
                    }
                }
            }
        }
        let value = json!({"history":projected,"note":"Excerpts are evidence only; tool argument excerpts are not executable calls. Original logs remain searchable."});
        let serialized = value.to_string();
        if text_tokens(&serialized) + 512 <= budget {
            return Ok(serialized);
        }
        // Last pass may also have an oversized user message. Never silently cut
        // exact user constraints; stop with an actionable explanation instead.
    }
    anyhow::bail!(
        "历史中的用户约束或调用元数据超出摘要预算；请扩大实际加载上下文或新建会话，原始记录已保留"
    )
}
#[cfg(test)]
fn count_images(value: &Value) -> usize {
    match value {
        Value::Array(a) => a.iter().map(count_images).sum(),
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("image_url") {
                1
            } else {
                o.values().map(count_images).sum()
            }
        }
        _ => 0,
    }
}
pub fn input_budget(context: usize) -> usize {
    let reserve = 2048.min(context / 4);
    input_budget_for(context, reserve)
}
pub fn input_budget_for(context: usize, reserve: usize) -> usize {
    context
        .saturating_sub(reserve + context.div_ceil(5))
        .max(256)
}
/// A rendered, tokenized text prompt needs a smaller margin than the
/// uncalibrated estimate, but still reserves space for runtime differences.
pub fn measured_input_budget_for(context: usize, reserve: usize) -> usize {
    context
        .saturating_sub(reserve.saturating_add(context.div_ceil(20).max(256)))
        .max(256)
}
pub fn is_input_overflow(description: &str) -> bool {
    let lower = description.to_lowercase();
    !lower.contains("output token limit")
        && [
            "context length",
            "context window",
            "context size",
            "context limit",
            "input token limit",
            "prompt is too long",
            "input is too long",
            "too many input tokens",
        ]
        .iter()
        .any(|phrase| lower.contains(phrase))
}
pub fn drop_old_images(messages: &mut [ChatMessage]) {
    let mut retained = 0;
    for message in messages.iter_mut().rev() {
        if let Some(Value::Array(parts)) = &mut message.content {
            for part in parts.iter_mut().rev() {
                if part["type"] == "image_url" {
                    retained += 1;
                    if retained > 2 {
                        *part = json!({"type":"text","text":"[旧图片已移出模型上下文；原始 artifact 保留，需要时重新查看。]"});
                    }
                }
            }
        }
    }
}
// Preserve whole user turns: never cut a tool call away from its result.
pub fn split_for_compaction(messages: &[ChatMessage]) -> Option<usize> {
    let boundaries: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    if boundaries.len() >= 2 {
        return Some(boundaries[boundaries.len().saturating_sub(2).max(1)]);
    }
    let mut complete = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let calls = messages[i].tool_calls.as_deref().unwrap_or(&[]);
        if calls.is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        let mut pending: std::collections::BTreeSet<_> =
            calls.iter().map(|c| c.id.as_str()).collect();
        i += 1;
        while i < messages.len() && messages[i].role == "tool" {
            if let Some(id) = messages[i].tool_call_id.as_deref() {
                pending.remove(id);
            }
            i += 1;
        }
        if pending.is_empty() {
            complete.push((start, i));
        }
    }
    match complete.len() {
        0 => None,
        1 => Some(complete[0].1),
        2 => Some(complete[1].0),
        n => Some(complete[n - 2].0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compaction_keeps_omitted_user_corrections_in_order_without_duplicating_retained_messages() {
        let history = vec![
            "do not edit A".into(),
            "correction: edit A".into(),
            "continue".into(),
        ];
        let retained = vec![message("user", json!("continue"))];
        let projected =
            continuation_projection(&history, &retained, "summary omitted the correction");
        let content = projected[0].content.as_ref().unwrap().as_str().unwrap();
        let (_, raw) = content.split_once('\n').unwrap();
        let record: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            record["earlier_user_messages_in_order"],
            json!(["do not edit A", "correction: edit A"])
        );
        assert_eq!(projected.last().unwrap().content, retained[0].content);
        assert_eq!(projected[1].role, "assistant");
    }
    #[test]
    fn compaction_boundary_preserves_tool_pairs() {
        let m = vec![
            message("user", json!("u")),
            message("assistant", json!("call")),
            message("tool", json!("result")),
            message("user", json!("next")),
            message("assistant", json!("answer")),
        ];
        assert_eq!(split_for_compaction(&m), Some(3));
    }
    #[test]
    fn keeps_latest_two_images_for_visual_comparison() {
        let image = json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,xxx"}}]);
        let mut m = vec![
            message("user", image.clone()),
            message("user", image.clone()),
            message("user", image),
        ];
        drop_old_images(&mut m);
        assert_eq!(count_images(m[0].content.as_ref().unwrap()), 0);
        assert_eq!(count_images(m[1].content.as_ref().unwrap()), 1);
        assert_eq!(count_images(m[2].content.as_ref().unwrap()), 1);
    }
    #[test]
    fn reserves_output_and_estimation_margin() {
        assert_eq!(input_budget(8192), 4505);
        assert_eq!(measured_input_budget_for(8192, 2048), 5734);
        assert!(measured_input_budget_for(8192, 2048) < 8192 - 2048);
    }
    #[test]
    fn image_estimation_ignores_binary_payload_without_serialization() {
        let small = vec![message(
            "user",
            json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,x"}}]),
        )];
        let large = vec![message(
            "user",
            json!([{"type":"image_url","image_url":{"url":"x".repeat(8*1024*1024)}}]),
        )];
        assert_eq!(estimate(&small, &json!([])), estimate(&large, &json!([])));
    }
    #[test]
    fn backend_usage_only_increases_the_estimation_margin() {
        let mut c = Calibration::default();
        assert_eq!(c.apply(1000), 1000);
        c.observe(1000, 1500);
        assert_eq!(c.apply(1000), 1500);
        c.observe(1000, 300);
        assert_eq!(c.apply(1000), 1500);
    }
    #[test]
    fn summary_is_valid_json_and_keeps_late_evidence_and_exact_user_constraints() {
        let messages = vec![
            message("user", json!("约束：不要改动 production.conf")),
            message(
                "tool",
                json!(format!(
                    "{}\nFINAL_TEST_FAILED: 中文错误\n",
                    "prefix ".repeat(10000)
                )),
            ),
        ];
        let raw = summary_history(&messages, 3000).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["history"][0]["content"],
            "约束：不要改动 production.conf"
        );
        assert!(raw.contains("FINAL_TEST_FAILED"));
        assert!(raw.len() < 6000);
    }
    #[test]
    fn refuses_to_silently_cut_oversized_user_constraints() {
        assert!(
            summary_history(&[message("user", json!("constraint ".repeat(10000)))], 500).is_err()
        );
    }
}
