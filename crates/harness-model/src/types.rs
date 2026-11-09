use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: String,
    pub model: String,
    pub context_length: usize,
    /// `auto` is an inference from the name, not certification of the weights.
    pub model_family: String,
    pub backend: String,
    pub thinking_mode: String,
    pub max_output_tokens: Option<usize>,
    /// Never included in connection debug output or diagnostics.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("protocol", &self.protocol)
            .field("model", &self.model)
            .field("context_length", &self.context_length)
            .field("model_family", &self.model_family)
            .field("backend", &self.backend)
            .field("thinking_mode", &self.thinking_mode)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            id: "local".into(),
            name: "Local model".into(),
            base_url: "http://127.0.0.1:1234/v1".into(),
            protocol: "chat_completions".into(),
            model: String::new(),
            context_length: 8192,
            model_family: "auto".into(),
            backend: "auto".into(),
            thinking_mode: "auto".into(),
            max_output_tokens: None,
            api_key: None,
        }
    }
}

impl ConnectionConfig {
    pub fn resolved_family(&self) -> &'static str {
        if self.model_family != "auto" {
            return match self.model_family.as_str() {
                "gemma4" => "gemma4",
                "gemma3" => "gemma3",
                "functiongemma" => "functiongemma",
                "other" => "other",
                _ => "unknown",
            };
        }
        let name: String = self
            .model
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        if name.contains("functiongemma") {
            "functiongemma"
        } else if name.contains("gemma4") {
            "gemma4"
        } else if name.contains("gemma3") || name.contains("codegemma") {
            "gemma3"
        } else {
            "unknown"
        }
    }
    pub fn resolved_backend(&self) -> &'static str {
        match self.backend.as_str() {
            "llama_cpp" => "llama_cpp",
            "lmstudio" => "lmstudio",
            "other" => "other",
            "auto" if self.id == "managed-llama" => "llama_cpp",
            "auto" if self.protocol == "lmstudio" => "lmstudio",
            _ => "unknown",
        }
    }
    pub fn output_budget(&self) -> usize {
        self.max_output_tokens
            .unwrap_or_else(|| 2048.min(self.context_length / 4).max(256))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(Value::String(text.into())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        }
    }
}

fn function_kind() -> String {
    "function".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCall {
        call: ToolCall,
    },
    /// A complete, fully received generation contained a non-executable batch.
    /// No sibling call in that batch is emitted.
    ToolBatchRejected {
        message: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    Completed,
    Error {
        message: String,
    },
}
