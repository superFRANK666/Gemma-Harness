use crate::{required, Harness};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn normalize_appearance(value: &Value) -> Value {
    fn choice<'a>(value: &'a Value, key: &str, allowed: &[&str], default: &'a str) -> &'a str {
        value[key]
            .as_str()
            .filter(|v| allowed.contains(v))
            .unwrap_or(default)
    }
    fn size(value: &Value, key: &str, low: i64, high: i64, default: i64) -> i64 {
        value[key]
            .as_i64()
            .map(|n| n.clamp(low, high))
            .unwrap_or(default)
    }
    json!({
        "theme":choice(value,"theme", &["dark","light","system"],"dark"),
        "accent":choice(value,"accent", &["sage","blue","violet","amber"],"sage"),
        "fontFamily":choice(value,"fontFamily", &["system","sans","serif"],"system"),
        "fontSize":size(value,"fontSize",11,18,13),
        "codeFontFamily":choice(value,"codeFontFamily", &["system","mono"],"system"),
        "codeFontSize":size(value,"codeFontSize",10,18,12)
    })
}

impl Harness {
    // The registry binding and selected config are one durable state transition.
    // Publish in-memory settings only after SQLite has committed both writes.
    pub(crate) fn persist_project_config(&self, cfg: Value) -> Result<()> {
        if let Some(workspace) = cfg["workspace"].as_str().filter(|s| !s.trim().is_empty()) {
            self.store.register_project_and_save_setting(
                workspace,
                cfg["deviceId"].as_str().unwrap_or("local"),
                &cfg,
            )?;
            *self.settings.lock().unwrap() = cfg;
            Ok(())
        } else {
            self.persist(cfg)
        }
    }
    pub(crate) fn project_binding(&self, workspace: &str, device_id: &str) -> Result<String> {
        if workspace.trim().is_empty() {
            bail!("请选择项目目录")
        }
        if device_id == "local" {
            let path = Path::new(workspace)
                .canonicalize()
                .context("项目目录不存在或无法访问")?;
            if !path.is_dir() {
                bail!("项目路径必须是目录")
            }
            Ok(path
                .to_str()
                .context("项目目录路径必须是有效 UTF-8")?
                .into())
        } else {
            // Remote paths belong to the paired execution device, including its
            // separators and case rules; never canonicalize them on this host.
            let cfg = self.config();
            if !cfg["devices"]
                .as_array()
                .is_some_and(|devices| devices.iter().any(|d| d["id"] == device_id))
            {
                bail!("远端设备不存在或已取消配对")
            }
            Ok(workspace.into())
        }
    }

    pub(crate) fn stable_project_binding(
        &self,
        workspace: &str,
        device_id: &str,
    ) -> Result<String> {
        let canonical = self.project_binding(workspace, device_id)?;
        if device_id != "local" {
            return Ok(canonical);
        }
        let projects = self.store.projects()?;
        // Exact bindings take precedence when migration retained multiple aliases.
        if projects
            .iter()
            .any(|p| p.device_id == device_id && p.workspace == workspace)
        {
            return Ok(workspace.into());
        }
        Ok(projects
            .into_iter()
            .find(|project| {
                project.device_id == "local"
                    && Path::new(&project.workspace)
                        .canonicalize()
                        .ok()
                        .is_some_and(|path| path == Path::new(&canonical))
            })
            .map(|project| project.workspace)
            .unwrap_or(canonical))
    }

    pub(crate) async fn open_project(&self, request: &Value) -> Result<Value> {
        let _binding = self.binding_gate.lock().await;
        self.require_idle()?;
        let mut cfg = self.config();
        let (workspace, device_id) = if let Some(id) = request["projectId"].as_str() {
            let project = self.store.project(id)?;
            (project.workspace, project.device_id)
        } else {
            (
                required(request, "workspace")?.to_owned(),
                request["deviceId"].as_str().unwrap_or("local").to_owned(),
            )
        };
        // Preserve an existing registry binding (including legacy symlink paths)
        // so opening it cannot change its ID/alias or detach historical sessions.
        // New local projects use canonical paths; matching old aliases reuse the
        // original stable binding after validating that they resolve identically.
        let workspace = self.stable_project_binding(&workspace, &device_id)?;
        cfg["workspace"] = json!(workspace);
        cfg["deviceId"] = json!(device_id);
        self.persist_project_config(cfg.clone())?;
        Ok(json!({"config":cfg,"projects":self.store.projects()?}))
    }

    pub(crate) async fn rename_project(&self, request: &Value) -> Result<Value> {
        let _binding = self.binding_gate.lock().await;
        self.store
            .rename_project(required(request, "projectId")?, required(request, "name")?)?;
        Ok(json!({"config":self.config(),"projects":self.store.projects()?}))
    }

    pub(crate) async fn remove_project(&self, request: &Value) -> Result<Value> {
        let _binding = self.binding_gate.lock().await;
        self.require_idle()?;
        let id = required(request, "projectId")?;
        let project = self.store.project(id)?;
        let mut cfg = self.config();
        if cfg["workspace"] == project.workspace
            && cfg["deviceId"].as_str().unwrap_or("local") == project.device_id
        {
            cfg["workspace"] = json!("");
        }
        self.store.remove_project_and_save_setting(id, &cfg)?;
        *self.settings.lock().unwrap() = cfg.clone();
        Ok(json!({"config":cfg,"projects":self.store.projects()?}))
    }

    pub(crate) async fn save_appearance(&self, request: &Value) -> Result<Value> {
        let _binding = self.binding_gate.lock().await;
        let mut cfg = self.config();
        cfg["appearance"] = normalize_appearance(&request["appearance"]);
        self.persist(cfg.clone())?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appearance_values_are_bounded_and_unknown_keys_are_ignored() {
        let value = normalize_appearance(
            &json!({"theme":"neon","accent":"blue","fontFamily":"serif","fontSize":-10,"codeFontFamily":"mono","codeFontSize":99,"custom":"discard"}),
        );
        assert_eq!(
            value,
            json!({"theme":"dark","accent":"blue","fontFamily":"serif","fontSize":11,"codeFontFamily":"mono","codeFontSize":18})
        );
        assert_eq!(
            normalize_appearance(&json!({"fontSize":12.5}))["fontSize"],
            13
        );
    }

    #[tokio::test]
    async fn project_switch_cannot_pass_an_admitted_operation_and_dropped_guards_release_it(
    ) -> Result<()> {
        let data = tempfile::tempdir()?;
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let harness = Harness::open(data.path().into())?;
        harness
            .open_project(&json!({"workspace":first.path()}))
            .await?;
        let gate = harness.binding_gate.lock().await;
        let cancel = tokio_util::sync::CancellationToken::new();
        let operation = harness.register_operation("deterministic-admission", cancel.clone())?;
        let h = harness.clone();
        let target = second.path().to_owned();
        let switch =
            tokio::spawn(async move { h.open_project(&json!({"workspace":target})).await });
        tokio::task::yield_now().await;
        assert!(
            !switch.is_finished(),
            "binding changes wait for the atomic admission gate"
        );
        drop(gate);
        assert!(
            switch.await?.is_err(),
            "the admitted operation blocks a queued switch"
        );
        assert_eq!(
            harness.config()["workspace"],
            json!(first.path().canonicalize()?)
        );
        drop(operation);
        assert!(cancel.is_cancelled());
        assert!(harness.active.lock().unwrap().is_empty());
        harness
            .open_project(&json!({"workspace":second.path()}))
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn remote_projects_keep_device_path_semantics_without_touching_controller_disk(
    ) -> Result<()> {
        let data = tempfile::tempdir()?;
        let harness = Harness::open(data.path().into())?;
        let mut cfg = harness.config();
        cfg["devices"] = json!([{"id":"win-a"},{"id":"win-b"}]);
        harness.persist(cfg)?;
        let path = r"C:\Users\User\Work Space\";
        harness
            .open_project(&json!({"workspace":path,"deviceId":"win-a"}))
            .await?;
        let opened = harness
            .open_project(&json!({"workspace":path,"deviceId":"win-b"}))
            .await?;
        assert_eq!(opened["projects"].as_array().unwrap().len(), 2);
        assert_eq!(opened["config"]["workspace"], path);
        assert_eq!(opened["config"]["deviceId"], "win-b");
        let first = harness
            .store
            .projects()?
            .into_iter()
            .find(|p| p.device_id == "win-a")
            .unwrap();
        harness
            .remove_project(&json!({"projectId":first.id}))
            .await?;
        assert_eq!(
            harness.config()["workspace"],
            path,
            "removing another device's project leaves selection intact"
        );
        harness
            .command(json!({"action":"remove_device","deviceId":"win-b"}))
            .await?;
        assert_eq!(harness.config()["deviceId"], "local");
        assert_eq!(
            harness.config()["workspace"],
            "",
            "a removed remote binding must never fall back to a local directory"
        );
        Ok(())
    }

    #[tokio::test]
    async fn opening_a_legacy_noncanonical_binding_keeps_its_alias_and_id() -> Result<()> {
        let data = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let harness = Harness::open(data.path().into())?;
        let legacy = workspace.path().join(".").to_string_lossy().into_owned();
        let project = harness.store.register_project(&legacy, "local")?;
        harness.store.rename_project(&project.id, "Legacy alias")?;
        for request in [
            json!({"projectId":project.id}),
            json!({"workspace":workspace.path()}),
        ] {
            let result = harness.open_project(&request).await?;
            assert_eq!(result["projects"].as_array().unwrap().len(), 1);
            assert_eq!(result["projects"][0]["id"], project.id);
            assert_eq!(result["projects"][0]["name"], "Legacy alias");
            assert_eq!(result["config"]["workspace"], legacy);
            harness
                .command(json!({"action":"save_settings","config":result["config"]}))
                .await?;
            assert_eq!(harness.store.projects()?.len(), 1);
            assert_eq!(harness.store.project(&project.id)?.name, "Legacy alias");
        }
        Ok(())
    }

    #[tokio::test]
    async fn failed_settings_write_rolls_back_registry_and_in_memory_selection() -> Result<()> {
        let data = tempfile::tempdir()?;
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let harness = Harness::open(data.path().into())?;
        let opened = harness
            .open_project(&json!({"workspace":first.path()}))
            .await?;
        let id = opened["projects"][0]["id"].as_str().unwrap();
        let old_config = harness.config();
        let old_projects = serde_json::to_value(harness.store.projects()?)?;
        let db = rusqlite::Connection::open(data.path().join("harness.sqlite"))?;
        db.execute_batch("CREATE TRIGGER reject_settings_write BEFORE UPDATE ON settings BEGIN SELECT RAISE(FAIL,'fixture settings write failed'); END;")?;
        let mut changed = old_config.clone();
        changed["workspace"] = json!(second.path());
        // Cover deleting an existing project, creating a new one, touching an
        // existing one, and the generic settings route that also registers paths.
        for request in [
            json!({"action":"remove_project","projectId":id}),
            json!({"action":"open_project","workspace":second.path()}),
            json!({"action":"open_project","projectId":id}),
            json!({"action":"save_settings","config":changed}),
        ] {
            let error = harness.command(request).await.unwrap_err();
            assert!(format!("{error:#}").contains("fixture settings write failed"));
            assert_eq!(harness.config(), old_config);
            assert_eq!(harness.store.setting()?, Some(old_config.clone()));
            assert_eq!(
                serde_json::to_value(harness.store.projects()?)?,
                old_projects
            );
        }
        db.execute_batch("DROP TRIGGER reject_settings_write;")?;
        let opened = harness
            .open_project(&json!({"workspace":second.path()}))
            .await?;
        assert_eq!(opened["projects"].as_array().unwrap().len(), 2);
        assert_eq!(harness.store.setting()?, Some(harness.config()));
        let selected = harness
            .store
            .projects()?
            .into_iter()
            .find(|p| json!(p.workspace) == harness.config()["workspace"])
            .unwrap();
        harness
            .remove_project(&json!({"projectId":selected.id}))
            .await?;
        assert_eq!(harness.config()["workspace"], "");
        assert_eq!(harness.store.projects()?.len(), 1);
        assert_eq!(harness.store.setting()?, Some(harness.config()));
        Ok(())
    }
}
