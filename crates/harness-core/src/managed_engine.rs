//! Managed model startup is shared by settings and the first turn after restart.
use crate::{engine::EngineConfig, EventSink, Harness};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl Harness {
    pub(crate) async fn prepare_managed_engine(
        self: &Arc<Self>,
        connection_id: &str,
        cancel: &CancellationToken,
        sink: &EventSink,
    ) -> Result<()> {
        if connection_id != "managed-llama" {
            return Ok(());
        }
        // The binding gate serializes first-turn startup across sessions and
        // settings changes. Loading waits happen outside it and are cancellable.
        let binding = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("已停止"),
            binding = self.binding_gate.lock() => binding,
        };
        if self.engine.status()?["running"] != true {
            let config: EngineConfig =
                serde_json::from_value(self.config()["engineConfig"].clone())
                    .context("本地模型尚未配置；请在设置 → 模型与推理中选择 GGUF 和推理引擎")?;
            sink(json!({"type":"status","text":"正在按保存的配置启动本地推理引擎"}));
            tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("已停止"),
                result = self.start_managed_engine(&config, true) => result?,
            };
        }
        drop(binding);
        if self.engine.status()?["ready"] != true {
            sink(json!({"type":"status","text":"正在等待本地模型加载，可随时停止等待"}));
        }
        self.engine.wait_until_ready(cancel).await
    }

    // Callers hold binding_gate; only the explicit settings command requires
    // idle sessions. First-turn startup already has an operation registration.
    pub(crate) async fn start_managed_engine(
        self: &Arc<Self>,
        engine_config: &EngineConfig,
        preserve_tuning: bool,
    ) -> Result<Value> {
        let harness = self.clone();
        let path = std::path::PathBuf::from(&engine_config.model_path);
        let mut profile =
            tokio::task::spawn_blocking(move || harness.record_model_profile(&path)).await??;
        let previous = self.config();
        let preserve_tuning = preserve_tuning
            && previous["engineConfig"] == serde_json::to_value(engine_config)?
            && previous["modelProfiles"]["managed-llama"]["identity"] == profile["identity"]
            && previous["modelProfiles"]["managed-llama"]["file"] == profile["file"];
        let mut result = self.engine.start(engine_config)?;
        if engine_config.chat_template_path.trim().is_empty() {
            result["chatTemplate"]["knownIssues"] =
                profile["identity"]["chatTemplate"]["knownIssues"].clone();
        }
        profile["runtime"] = json!({"pinnedInstallerVersion":"b12211","actualVersion":"unknown_until_probed","binaryPath":engine_config.binary_path,"loadedContextRequested":engine_config.context_length,"projectorPath":engine_config.projector_path,"chatTemplate":result["chatTemplate"]});
        let mut config = self.config();
        config["modelProfiles"]["managed-llama"] = profile;
        config["engineConfig"] = serde_json::to_value(engine_config)?;
        let connections = config["connections"]
            .as_array_mut()
            .context("模型连接列表无效")?;
        if !connections.iter().any(|c| c["id"] == "managed-llama") {
            connections.push(json!({"id":"managed-llama","name":"本地模型"}));
        }
        config["selectedConnectionId"] = json!("managed-llama");
        // A restart of the saved model retains user tuning. A different
        // model or an explicit reconfiguration must not inherit that tuning.
        if let Some(managed) = config["connections"]
            .as_array_mut()
            .and_then(|connections| connections.iter_mut().find(|c| c["id"] == "managed-llama"))
        {
            managed["baseUrl"] = json!(format!("http://127.0.0.1:{}", engine_config.port));
            managed["protocol"] = json!("chat_completions");
            managed["model"] = json!("gemma-local");
            managed["contextLength"] = json!(engine_config.context_length);
            managed["backend"] = json!("llama_cpp");
            if !preserve_tuning {
                managed["modelFamily"] = json!("auto");
                managed["thinkingMode"] = json!("auto");
            }
            if managed["maxOutputTokens"]
                .as_u64()
                .is_some_and(|limit| limit > (engine_config.context_length / 2) as u64)
            {
                managed["maxOutputTokens"] = Value::Null;
            }
        }
        // A restarted engine may use different weights, template,
        // projector or runtime even when its public alias is unchanged.
        if let Some(diagnostics) = config["diagnostics"].as_object_mut() {
            diagnostics.remove("managed-llama");
        }
        if let Err(error) = self.persist(config) {
            let _ = self.engine.stop();
            return Err(error);
        }
        Ok(result)
    }
}
