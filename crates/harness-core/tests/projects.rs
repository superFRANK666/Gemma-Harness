use anyhow::Result;
use harness_core::Harness;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

#[cfg(unix)]
#[tokio::test]
async fn restarting_managed_engine_invalidates_old_vision_diagnostics() -> Result<()> {
    let data = tempfile::tempdir()?;
    let model = data.path().join("fixture.gguf");
    let mut header = b"GGUF".to_vec();
    header.extend_from_slice(&3u32.to_le_bytes());
    header.extend_from_slice(&0u64.to_le_bytes());
    header.extend_from_slice(&0u64.to_le_bytes());
    std::fs::write(&model, header)?;
    let store = harness_core::store::Store::open(&data.path().join("harness.sqlite"))?;
    store.save_setting(&json!({"connections":[{"id":"managed-llama","baseUrl":"http://127.0.0.1:1235","protocol":"chat_completions","model":"gemma-local","contextLength":16384,"backend":"llama_cpp","modelFamily":"gemma4","thinkingMode":"on","maxOutputTokens":8192}],"workspace":"","deviceId":"local","devices":[],"mcpServers":[],"diagnostics":{"managed-llama":{"vision":{"status":"passed"}},"other":{"status":"passed"}}}))?;
    drop(store);
    let harness = Harness::open(data.path().into())?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    harness.command(json!({"action":"engine_start","config":{"binaryPath":"/usr/bin/true","modelPath":model,"projectorPath":"","contextLength":8192,"port":port,"gpuLayers":0}})).await?;
    let config = harness.command(json!({"action":"snapshot"})).await?["config"].clone();
    assert!(config["diagnostics"].get("managed-llama").is_none());
    assert_eq!(config["diagnostics"]["other"]["status"], "passed");
    let connection = &config["connections"][0];
    assert_eq!(connection["baseUrl"], format!("http://127.0.0.1:{port}"));
    assert_eq!(connection["contextLength"], 8192);
    assert_eq!(connection["modelFamily"], "auto");
    assert_eq!(connection["thinkingMode"], "auto");
    assert!(connection["maxOutputTokens"].is_null());
    harness.shutdown().await;
    Ok(())
}

async fn snapshot(harness: &Arc<Harness>) -> Result<Value> {
    harness.command(json!({"action":"snapshot"})).await
}

#[tokio::test]
async fn projects_are_aliases_and_removal_preserves_files_sessions_and_restart() -> Result<()> {
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let file = workspace.path().join("keep.txt");
    std::fs::write(&file, "original bytes")?;
    let harness = Harness::open(data.path().into())?;
    assert!(snapshot(&harness).await?["projects"]
        .as_array()
        .unwrap()
        .is_empty());
    let opened = harness
        .command(json!({"action":"open_project","workspace":workspace.path()}))
        .await?;
    let id = opened["projects"][0]["id"].as_str().unwrap().to_owned();
    let binding = opened["config"]["workspace"].clone();
    let session = harness.command(json!({"action":"create_session"})).await?;
    let renamed = harness
        .command(
            json!({"action":"rename_project","projectId":id,"name":"  显示名称 / not a path  "}),
        )
        .await?;
    assert_eq!(renamed["projects"][0]["name"], "显示名称 / not a path");
    assert_eq!(renamed["config"]["workspace"], binding);
    assert_eq!(std::fs::read_to_string(&file)?, "original bytes");
    // A generic settings save registers/touches the directory without losing its alias.
    let mut config = renamed["config"].clone();
    config["projects"] = json!([{"id":"injected","workspace":"/untrusted"}]);
    let saved = harness
        .command(json!({"action":"save_settings","config":config}))
        .await?;
    assert!(saved.get("projects").is_none());
    assert_eq!(
        snapshot(&harness).await?["projects"][0]["name"],
        "显示名称 / not a path"
    );
    let removed = harness
        .command(json!({"action":"remove_project","projectId":id}))
        .await?;
    assert_eq!(removed["config"]["workspace"], "");
    assert!(removed["projects"].as_array().unwrap().is_empty());
    let loaded = harness
        .command(json!({"action":"load_session","sessionId":session["id"]}))
        .await?;
    assert_eq!(loaded["session"], session);
    assert_eq!(std::fs::read_to_string(&file)?, "original bytes");
    drop(harness);
    let reopened = Harness::open(data.path().into())?;
    assert!(snapshot(&reopened).await?["projects"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        snapshot(&reopened).await?["sessions"][0]["id"],
        session["id"]
    );
    Ok(())
}

#[tokio::test]
async fn opening_canonicalizes_local_directories_and_rejects_invalid_targets() -> Result<()> {
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let harness = Harness::open(data.path().into())?;
    let joined = workspace.path().join(".");
    let first = harness
        .command(json!({"action":"open_project","workspace":joined}))
        .await?;
    let second = harness
        .command(json!({"action":"open_project","workspace":workspace.path()}))
        .await?;
    assert_eq!(second["projects"].as_array().unwrap().len(), 1);
    assert_eq!(first["projects"][0]["id"], second["projects"][0]["id"]);
    assert_eq!(
        second["config"]["workspace"],
        json!(workspace.path().canonicalize()?)
    );
    let file = workspace.path().join("ordinary.txt");
    std::fs::write(&file, "x")?;
    for request in [
        json!({"action":"open_project","workspace":file}),
        json!({"action":"open_project","workspace":"C:\\remote\\project","deviceId":"unpaired"}),
        json!({"action":"open_project","projectId":"missing"}),
    ] {
        assert!(harness.command(request).await.is_err());
    }
    assert_eq!(snapshot(&harness).await?["projects"], second["projects"]);
    Ok(())
}

#[tokio::test]
async fn appearance_is_canonical_and_preserves_other_configuration_and_sessions() -> Result<()> {
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let harness = Harness::open(data.path().into())?;
    harness
        .command(json!({"action":"open_project","workspace":workspace.path()}))
        .await?;
    let session = harness.command(json!({"action":"create_session"})).await?;
    let before = snapshot(&harness).await?;
    let saved=harness.command(json!({"action":"save_appearance","appearance":{"theme":"light","accent":"violet","fontFamily":"serif","fontSize":99,"codeFontFamily":"mono","codeFontSize":0,"secret":"discard"},"workspace":"must ignore"})).await?;
    assert_eq!(
        saved["appearance"],
        json!({"theme":"light","accent":"violet","fontFamily":"serif","fontSize":18,"codeFontFamily":"mono","codeFontSize":10})
    );
    for key in [
        "workspace",
        "deviceId",
        "connections",
        "selectedConnectionId",
        "mcpServers",
        "toolPermissions",
    ] {
        assert_eq!(saved[key], before["config"][key]);
    }
    // Legacy settings clients do not know appearance; their save must preserve it.
    let mut legacy = saved.clone();
    legacy.as_object_mut().unwrap().remove("appearance");
    let legacy = harness
        .command(json!({"action":"save_settings","config":legacy}))
        .await?;
    assert_eq!(legacy["appearance"], saved["appearance"]);
    // Mixed settings edits can explicitly update appearance too.
    let mut mixed = legacy;
    mixed["appearance"] = json!({"theme":"system","fontSize":14});
    let mixed = harness
        .command(json!({"action":"save_settings","config":mixed}))
        .await?;
    assert_eq!(mixed["appearance"]["theme"], "system");
    assert_eq!(mixed["appearance"]["fontSize"], 14);
    assert_eq!(snapshot(&harness).await?["sessions"][0], session);
    drop(harness);
    assert_eq!(
        snapshot(&Harness::open(data.path().into())?).await?["config"]["appearance"],
        mixed["appearance"]
    );
    Ok(())
}

#[tokio::test]
async fn manual_and_legacy_tool_bindings_block_project_mutations_until_cancelled() -> Result<()> {
    for legacy in [false, true] {
        let data = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let harness = Harness::open(data.path().into())?;
        let opened = harness
            .command(json!({"action":"open_project","workspace":workspace.path()}))
            .await?;
        let id = opened["projects"][0]["id"].clone();
        let command = if cfg!(windows) {
            "Write-Output 'binding-ready'; Start-Sleep -Seconds 20"
        } else {
            "printf 'binding-ready\\n'; sleep 20"
        };
        let h = harness.clone();
        let task = tokio::spawn(async move {
            let request = json!({"action":"execute_tool","operationId":"held-terminal","name":"terminal_run","args":{"command":command,"timeout_ms":30000}});
            if legacy {
                h.command(request).await
            } else {
                h.manual_tool(request, Arc::new(|_| {})).await
            }
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if snapshot(&harness).await.unwrap()["activeSessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id == "held-terminal")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        for request in [
            json!({"action":"open_project","workspace":other.path()}),
            json!({"action":"remove_project","projectId":id}),
            json!({"action":"save_settings","config":opened["config"]}),
        ] {
            assert!(harness.command(request).await.is_err());
        }
        // Display aliases and appearance do not change the execution binding.
        harness
            .command(json!({"action":"rename_project","projectId":id,"name":"Allowed alias"}))
            .await?;
        harness
            .command(json!({"action":"save_appearance","appearance":{"theme":"light"}}))
            .await?;
        assert_eq!(
            snapshot(&harness).await?["config"]["workspace"],
            opened["config"]["workspace"]
        );
        harness
            .command(json!({"action":"interrupt","sessionId":"held-terminal"}))
            .await?;
        let result = tokio::time::timeout(Duration::from_secs(3), task).await???;
        assert_eq!(result["cancelled"], true);
        assert!(snapshot(&harness).await?["activeSessions"]
            .as_array()
            .unwrap()
            .is_empty());
        harness
            .command(json!({"action":"open_project","workspace":other.path()}))
            .await?;
    }
    Ok(())
}
