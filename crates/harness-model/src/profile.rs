use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    pub id: String,
    pub repository: String,
    pub revision: String,
    pub source_repository: String,
    pub source_revision: String,
    pub architecture_context_limit: usize,
    pub default_context_length: usize,
    pub native_tool_format: String,
    pub vision_requires_projector: bool,
    pub identity_fingerprint: String,
}

/// Identifies the requested quantization family; the exact local GGUF still needs verification.
pub fn target_model_profile() -> ModelProfile {
    let repo = "mradermacher/gemma-4-26B-A4B-it-ultra-uncensored-heretic-i1-GGUF";
    let revision = "9cada68ea11a8f361e4b16a7a97e53d99b0918c0";
    ModelProfile {
        id: "gemma4-26b-a4b-heretic-i1".into(),
        repository: repo.into(),
        revision: revision.into(),
        source_repository: "llmfan46/gemma-4-26B-A4B-it-ultra-uncensored-heretic".into(),
        source_revision: "f5970e2eee74ef654b7ee97722b3e67a7ec6adda".into(),
        architecture_context_limit: 262144,
        default_context_length: 8192,
        native_tool_format: "gemma4".into(),
        vision_requires_projector: true,
        identity_fingerprint: format!(
            "{:x}",
            Sha256::digest(format!("{repo}@{revision}").as_bytes())
        ),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextBudget {
    pub context_length: usize,
    pub max_input_tokens: usize,
    pub max_output_tokens: usize,
    pub safety_tokens: usize,
}

/// Reserve generation and template overhead before packing history. No token estimate is a measured count.
pub fn context_budget(
    context_length: usize,
    requested_output_tokens: usize,
    safety_tokens: usize,
) -> ContextBudget {
    let safety = safety_tokens.min(context_length);
    let output = requested_output_tokens.min(context_length.saturating_sub(safety));
    ContextBudget {
        context_length,
        max_input_tokens: context_length.saturating_sub(safety + output),
        max_output_tokens: output,
        safety_tokens: safety,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_never_exceeds_context_or_underflows() {
        for context in [0, 1, 256, 8192] {
            let b = context_budget(context, 4096, 512);
            assert_eq!(
                b.max_input_tokens + b.max_output_tokens + b.safety_tokens,
                context
            );
        }
    }
}
