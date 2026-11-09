mod agent;
pub mod artifact;
pub mod context;
pub mod engine;
mod inference;
pub mod mcp;
mod projects;
pub mod store;

use anyhow::{bail, Context, Result};
use harness_executor::Executor;
use harness_model::{ConnectionConfig, Provider};
use harness_remote::{PairedDevice, RemoteClient, RemoteHost, RemoteHostHandle};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use store::{Session, Store};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct UncertainRemoteOperation {
    pub id: String,
    pub reason: String,
}
impl std::fmt::Display for UncertainRemoteOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "远端操作结果待确认（{}）：{}；禁止自动重复，请查询 operation_status 或观察实际状态。",
            self.id, self.reason
        )
    }
}
impl std::error::Error for UncertainRemoteOperation {}
pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;
struct OperationRegistration<'a> {
    harness: &'a Harness,
    id: String,
    cancel: CancellationToken,
}
impl Drop for OperationRegistration<'_> {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.harness.active.lock().unwrap().remove(&self.id);
    }
}
pub struct Harness {
    pub(crate) store: Store,
    pub(crate) settings: Mutex<Value>,
    pub(crate) dir: PathBuf,
    pub(crate) active: Mutex<HashMap<String, CancellationToken>>,
    pub(crate) binding_gate: AsyncMutex<()>,
    pub(crate) endpoints: Mutex<HashMap<String, Arc<Semaphore>>>,
    executors: AsyncMutex<HashMap<String, Arc<Executor>>>,
    pub(crate) mcp: mcp::McpManager,
    engine: engine::Engine,
    host: AsyncMutex<Option<RemoteHostHandle>>,
    desktop_owners: Mutex<HashMap<String, String>>,
    desktop_leases: AsyncMutex<HashMap<String, (RemoteClient, CancellationToken)>>,
}
impl Harness {
    pub fn open(dir: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let store = Store::open(&dir.join("harness.sqlite"))?;
        let mut settings=store.setting()?.unwrap_or_else(||json!({
            "connections":[{"id":"lmstudio","name":"LM Studio","baseUrl":"http://127.0.0.1:1234","protocol":"lmstudio","model":"","contextLength":8192}],
            "selectedConnectionId":"lmstudio","workspace":"","deviceId":"local","devices":[],"mcpServers":[],
            "toolPermissions":{"terminal":true,"browser":true,"desktop":false},"mode":"controller"
        }));
        settings["appearance"] = projects::normalize_appearance(&settings["appearance"]);
        if let Some(object) = settings.as_object_mut() {
            object.remove("projects");
        }
        store.migrate_projects(
            settings["workspace"].as_str().unwrap_or(""),
            settings["deviceId"].as_str().unwrap_or("local"),
        )?;
        store.save_setting(&settings)?;
        Ok(Arc::new(Self {
            store,
            settings: Mutex::new(settings),
            engine: engine::Engine::new(dir.clone()),
            dir,
            active: Mutex::new(HashMap::new()),
            binding_gate: AsyncMutex::new(()),
            endpoints: Mutex::new(HashMap::new()),
            executors: AsyncMutex::new(HashMap::new()),
            mcp: Default::default(),
            host: AsyncMutex::new(None),
            desktop_owners: Mutex::new(HashMap::new()),
            desktop_leases: AsyncMutex::new(HashMap::new()),
        }))
    }
    fn config(&self) -> Value {
        self.settings.lock().unwrap().clone()
    }
    fn persist(&self, config: Value) -> Result<()> {
        self.store.save_setting(&config)?;
        *self.settings.lock().unwrap() = config;
        Ok(())
    }
    // Callers hold binding_gate while capturing a binding and admitting work.
    fn register_operation<'a>(
        &'a self,
        id: &str,
        cancel: CancellationToken,
    ) -> Result<OperationRegistration<'a>> {
        let mut active = self.active.lock().unwrap();
        if active.contains_key(id) {
            bail!("操作 ID 已经运行")
        }
        active.insert(id.into(), cancel.clone());
        Ok(OperationRegistration {
            harness: self,
            id: id.into(),
            cancel,
        })
    }
    fn require_idle(&self) -> Result<()> {
        if !self.active.lock().unwrap().is_empty() {
            bail!("请先停止运行中的任务或终端命令，再切换或移除项目")
        }
        Ok(())
    }
    pub(crate) fn connection(&self, id: &str) -> Result<ConnectionConfig> {
        let cfg = self.config();
        let value = cfg["connections"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["id"] == id))
            .context("模型连接不存在")?
            .clone();
        let mut connection: ConnectionConfig = serde_json::from_value(value)?;
        connection.api_key = read_secret(&format!("connection:{id}")).ok();
        if connection.model.trim().is_empty() {
            bail!("请先在设置中连接推理服务并选择模型")
        }
        self.resolve_managed_family(&mut connection);
        Ok(connection)
    }
    fn resolve_managed_family(&self, connection: &mut ConnectionConfig) {
        if connection.id != "managed-llama" || connection.model_family != "auto" {
            return;
        }
        let config = self.config();
        if let Some(architecture) =
            config["modelProfiles"]["managed-llama"]["identity"]["architecture"].as_str()
        {
            let mut probe = connection.clone();
            probe.model = architecture.into();
            let family = probe.resolved_family();
            if family != "unknown" {
                connection.model_family = family.into();
            }
        }
    }
    pub(crate) async fn executor(&self, workspace: &str) -> Result<Arc<Executor>> {
        let mut all = self.executors.lock().await;
        if let Some(ex) = all.get(workspace) {
            return Ok(ex.clone());
        }
        if workspace.trim().is_empty() {
            bail!("请先选择工作目录")
        }
        let ex = Arc::new(Executor::new(
            PathBuf::from(workspace),
            self.dir.join("artifacts"),
        )?);
        all.insert(workspace.into(), ex.clone());
        Ok(ex)
    }
    fn remote_device(&self, id: &str) -> Result<PairedDevice> {
        let config = self.config();
        let v = config["devices"]
            .as_array()
            .and_then(|a| a.iter().find(|d| d["id"] == id))
            .context("远端设备不存在")?;
        let mut device: PairedDevice = serde_json::from_value(v.clone())?;
        device.token = read_secret(&format!("device:{id}"))?;
        Ok(device)
    }
    pub(crate) async fn execute_with_sink(
        self: &Arc<Self>,
        session: &Session,
        identifiers: (&str, &str),
        name: &str,
        args: Value,
        cancel: CancellationToken,
        sink: EventSink,
    ) -> Result<Value> {
        let (operation_id, call_id) = identifiers;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let harness = self.clone();
        let session_id = session.id.clone();
        let device_id = session.device_id.clone();
        let call_id = call_id.to_string();
        let operation_id_copy = operation_id.to_string();
        let forward = tokio::spawn(async move {
            while let Some(mut event) = rx.recv().await {
                event["callId"] = json!(&call_id);
                event["operationId"] = json!(&operation_id_copy);
                event["deviceId"] = json!(&device_id);
                if harness.store.session(&session_id).is_ok() {
                    let _ = harness.store.append(&session_id, "activity", &event);
                }
                sink(event);
            }
        });
        let result = self
            .execute_streamed(session, operation_id, name, args, cancel, Some(tx))
            .await;
        let _ = forward.await;
        result
    }
    pub async fn manual_tool(self: &Arc<Self>, request: Value, sink: EventSink) -> Result<Value> {
        let binding = self.binding_gate.lock().await;
        let config = self.config();
        let mut session = if let Some(id) = request["sessionId"].as_str() {
            self.store.session(id)?
        } else {
            Session {
                id: String::new(),
                title: "manual".into(),
                workspace: config["workspace"].as_str().unwrap_or("").into(),
                device_id: config["deviceId"].as_str().unwrap_or("local").into(),
                connection_id: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            }
        };
        let operation_id = request["operationId"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        session.id = format!("manual-{operation_id}");
        let cancel = CancellationToken::new();
        let _operation = self.register_operation(&operation_id, cancel.clone())?;
        drop(binding);
        let result = match request["name"].as_str() {
            Some(name) => {
                self.execute_with_sink(
                    &session,
                    (&operation_id, &operation_id),
                    name,
                    request["args"].clone(),
                    cancel,
                    sink,
                )
                .await
            }
            None => Err(anyhow::anyhow!("缺少工具 name")),
        };
        self.release_desktop(&session.id).await;
        result
    }
    pub(crate) async fn execute_streamed(
        &self,
        session: &Session,
        operation_id: &str,
        name: &str,
        mut args: Value,
        cancel: CancellationToken,
        events: Option<tokio::sync::mpsc::UnboundedSender<Value>>,
    ) -> Result<Value> {
        let cfg = self.config();
        let permission = if name.starts_with("desktop_") {
            Some("desktop")
        } else if name.starts_with("browser_") {
            Some("browser")
        } else if name.starts_with("terminal_") {
            Some("terminal")
        } else {
            None
        };
        if let Some(p) = permission {
            if cfg["toolPermissions"][p] != true {
                bail!("{p} 工具能力未启用")
            }
        }
        if name == "operation_status" {
            if session.device_id == "local" {
                bail!("operation_status 用于远端执行设备")
            }
            let status = RemoteClient::new(self.remote_device(&session.device_id)?)?
                .operation_status(args["operation_id"].as_str().context("缺少 operation_id")?)
                .await?;
            return Ok(
                json!({"operationId":args["operation_id"],"state":status["state"],"error":status["error"],"resultSummary":without_binary(status["result"].clone()).to_string().chars().take(3000).collect::<String>()}),
            );
        }
        if name == "artifact_read" {
            let path = args["path"].as_str().context("缺少 artifact path")?;
            return self
                .read_session_artifact(
                    session,
                    path,
                    args["offset"].as_u64().unwrap_or(0) as usize,
                    args["limit"].as_u64().unwrap_or(6000).min(32000) as usize,
                )
                .await;
        }
        if name == "artifact_view" {
            let path = args["path"].as_str().context("缺少 artifact path")?;
            let fetched = self.fetch_session_artifact(session, path).await?;
            return Ok(
                json!({"artifact":path,"content":[{"type":"text","text":format!("Saved artifact: {path}")},{"type":"image","data":fetched["base64"],"mimeType":fetched["mimeType"]}]}),
            );
        }
        if name.starts_with("mcp_") {
            if session.device_id != "local" {
                bail!("MCP stdio 服务运行在主控，不能冒充远端工具")
            }
            let configs: Vec<mcp::McpConfig> =
                serde_json::from_value(cfg["mcpServers"].clone()).unwrap_or_default();
            return self.mcp.call(&configs, name, args, cancel).await;
        }
        if name.starts_with("desktop_") {
            self.acquire_desktop(session, cancel.clone()).await?;
        }
        if session.device_id == "local" {
            let executor = self.executor(&session.workspace).await?;
            if let Some(events) = events {
                executor.execute_streaming(name, args, cancel, events).await
            } else {
                executor.execute(name, args, cancel).await
            }
        } else {
            let client = RemoteClient::new(self.remote_device(&session.device_id)?)?;
            let caps = tokio::select! {_=cancel.cancelled()=>bail!("已停止"),caps=client.capabilities()=>caps?};
            if caps["workspace"].as_str() != Some(&session.workspace) {
                bail!(
                    "远端共享目录与会话绑定不一致（当前 {}）。请创建绑定正确目录的新会话。",
                    caps["workspace"]
                )
            }
            if name.starts_with("desktop_") {
                args["__harness_session_id"] = json!(&session.id);
            }
            let outcome = if let Some(events) = events {
                client
                    .execute_streaming(operation_id, name, args, cancel.clone(), events)
                    .await
            } else {
                client
                    .execute(operation_id, name, args, cancel.clone())
                    .await
            };
            match outcome {
                Ok(value) => Ok(value),
                Err(error) if cancel.is_cancelled() => Err(error),
                Err(error) => {
                    let state = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        client.operation_status(operation_id),
                    )
                    .await;
                    match state {
                        Ok(Ok(status)) if status["state"] == "completed" => {
                            Ok(status["result"].clone())
                        }
                        Ok(Ok(status))
                            if status["state"] == "failed" || status["state"] == "cancelled" =>
                        {
                            Err(error)
                        }
                        _ => {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                client.cancel(operation_id),
                            )
                            .await;
                            Err(UncertainRemoteOperation {
                                id: operation_id.into(),
                                reason: error.to_string(),
                            }
                            .into())
                        }
                    }
                }
            }
        }
    }
    async fn fetch_session_artifact(&self, session: &Session, path: &str) -> Result<Value> {
        if let Ok(value) = artifact::fetch_artifact(&self.dir.join("artifacts"), path) {
            return Ok(value);
        }
        if session.device_id == "local" {
            return artifact::fetch_artifact(&self.dir.join("artifacts"), path);
        }
        let value = RemoteClient::new(self.remote_device(&session.device_id)?)?
            .fetch_artifact(path)
            .await?;
        Ok(
            json!({"path":path,"base64":value.get("data").or_else(||value.get("base64")),"mimeType":value["mimeType"],"sha256":value["sha256"],"deviceId":session.device_id}),
        )
    }
    async fn read_session_artifact(
        &self,
        session: &Session,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        let root = self.dir.join("artifacts");
        match artifact::read_artifact(&root, path, offset, limit) {
            Ok(value) => return Ok(value),
            Err(error) if session.device_id == "local" => return Err(error),
            Err(_) => {}
        }
        let fetched = RemoteClient::new(self.remote_device(&session.device_id)?)?
            .fetch_artifact(path)
            .await?;
        use base64::Engine as _;
        use sha2::Digest;
        let data = fetched
            .get("data")
            .or_else(|| fetched.get("base64"))
            .and_then(Value::as_str)
            .context("远端 artifact 缺少数据")?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
        if bytes.len() > 16 * 1024 * 1024 {
            bail!("artifact 超过 16 MiB")
        }
        let name = format!("remote-{}.txt", hex::encode(sha2::Sha256::digest(&bytes)));
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join(&name), bytes)?;
        let mut value = artifact::read_artifact(&root, &name, offset, limit)?;
        value["deviceId"] = json!(&session.device_id);
        value["sourcePath"] = json!(path);
        Ok(value)
    }
    async fn acquire_desktop(
        &self,
        session: &Session,
        parent_cancel: CancellationToken,
    ) -> Result<()> {
        {
            let mut owners = self.desktop_owners.lock().unwrap();
            if let Some(owner) = owners.get(&session.device_id) {
                if owner != &session.id {
                    bail!("此设备的桌面正在被另一会话使用")
                }
            }
            owners.insert(session.device_id.clone(), session.id.clone());
        }
        if session.device_id == "local" {
            return Ok(());
        }
        if self.desktop_leases.lock().await.contains_key(&session.id) {
            return Ok(());
        }
        let client = RemoteClient::new(self.remote_device(&session.device_id)?)?;
        let acquisition = tokio::select! {_=parent_cancel.cancelled()=>Err(anyhow::anyhow!("桌面会话获取已取消")),result=client.desktop_acquire(&session.id)=>result};
        if let Err(error) = acquisition {
            self.desktop_owners
                .lock()
                .unwrap()
                .remove(&session.device_id);
            return Err(error);
        }
        let stop = CancellationToken::new();
        let heartbeat_stop = stop.clone();
        let heartbeat_client = client.clone();
        let id = session.id.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(20));
            timer.tick().await;
            loop {
                tokio::select! {_=heartbeat_stop.cancelled()=>break,_=timer.tick()=>{
                    let renewed=tokio::select!{_=heartbeat_stop.cancelled()=>break,_=parent_cancel.cancelled()=>break,r=heartbeat_client.desktop_renew(&id)=>r};
                    if renewed.is_err(){parent_cancel.cancel();break}
                }}
            }
        });
        self.desktop_leases
            .lock()
            .await
            .insert(session.id.clone(), (client, stop));
        Ok(())
    }
    pub(crate) async fn release_desktop(&self, session_id: &str) {
        let lease = { self.desktop_leases.lock().await.remove(session_id) };
        if let Some((client, stop)) = lease {
            stop.cancel();
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                client.desktop_release(session_id),
            )
            .await;
        }
        self.desktop_owners
            .lock()
            .unwrap()
            .retain(|_, owner| owner != session_id);
    }
    fn record_model_profile(&self, path: &Path) -> Result<Value> {
        let profile = harness_model::inspect_gguf(path)?;
        let fingerprint = profile["identity"]["metadataFingerprint"]["sha256"]
            .as_str()
            .context("GGUF 元数据缺少指纹")?;
        let dir = self.dir.join("model-profiles");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(format!("{fingerprint}.json")),
            serde_json::to_vec_pretty(&profile)?,
        )?;
        Ok(profile)
    }
    pub async fn command(self: &Arc<Self>, request: Value) -> Result<Value> {
        let action = request["action"].as_str().context("缺少 action")?;
        match action {
            "snapshot" => {
                let _binding = self.binding_gate.lock().await;
                Ok(
                    json!({"config":self.config(),"projects":self.store.projects()?,"sessions":self.store.sessions()?,"engine":self.engine.status()?,"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"dataDir":self.dir,"activeSessions":self.active.lock().unwrap().keys().cloned().collect::<Vec<_>>(),"targetModel":{"repository":"mradermacher/gemma-4-26B-A4B-it-ultra-uncensored-heretic-i1-GGUF","revision":"9cada68ea11a8f361e4b16a7a97e53d99b0918c0","certification":"not_yet_verified"}}),
                )
            }
            "open_project" => self.open_project(&request).await,
            "rename_project" => self.rename_project(&request).await,
            "remove_project" => self.remove_project(&request).await,
            "save_appearance" => self.save_appearance(&request).await,
            "save_settings" => {
                let _binding = self.binding_gate.lock().await;
                self.require_idle()?;
                let mut cfg = request["config"].clone();
                if !cfg.is_object() {
                    bail!("设置必须为对象")
                }
                let mut credential_changes = Vec::new();
                if let Some(connections) = cfg["connections"].as_array_mut() {
                    for c in connections {
                        let id = c["id"].as_str().context("连接缺少 id")?.to_string();
                        if let Some(secret) = c.as_object_mut().unwrap().remove("apiKey") {
                            if let Some(s) = secret.as_str() {
                                if !s.is_empty() {
                                    write_secret(&format!("connection:{id}"), s)?;
                                    credential_changes.push(id.clone());
                                }
                            }
                        }
                    }
                }
                let old = self.config();
                cfg.as_object_mut().unwrap().remove("projects");
                cfg["appearance"] = projects::normalize_appearance(
                    cfg.get("appearance").unwrap_or(&old["appearance"]),
                );
                cfg["diagnostics"] = old["diagnostics"].clone();
                cfg["engineConfig"] = old["engineConfig"].clone();
                cfg["modelProfiles"] = old["modelProfiles"].clone();
                if let Some(connections) = cfg["connections"].as_array() {
                    let changed: Vec<String> = connections
                        .iter()
                        .filter_map(|new| {
                            let previous = old["connections"]
                                .as_array()
                                .and_then(|a| a.iter().find(|c| c["id"] == new["id"]));
                            let same = previous.is_some_and(|p| same_connection(p, new));
                            if same && !credential_changes.iter().any(|id| new["id"] == *id) {
                                None
                            } else {
                                new["id"].as_str().map(String::from)
                            }
                        })
                        .collect();
                    for id in changed {
                        if let Some(map) = cfg["diagnostics"].as_object_mut() {
                            map.remove(&id);
                        }
                    }
                }
                // Existing paired identities are changed only through pair/revoke commands.
                cfg["devices"] = self.config()["devices"].clone();
                if let Some(workspace) = cfg["workspace"].as_str().filter(|s| !s.trim().is_empty())
                {
                    let device = cfg["deviceId"].as_str().unwrap_or("local");
                    let workspace = self.stable_project_binding(workspace, device)?;
                    cfg["workspace"] = json!(workspace);
                }
                self.mcp.reset().await;
                self.persist_project_config(cfg.clone())?;
                Ok(cfg)
            }
            "discover" | "diagnose" => {
                let mut c: ConnectionConfig =
                    serde_json::from_value(request["connection"].clone())?;
                if c.api_key.as_deref().unwrap_or("").is_empty() {
                    c.api_key = read_secret(&format!("connection:{}", c.id)).ok()
                }
                let connection_id = c.id.clone();
                let connection_snapshot = serde_json::to_value(&c)?;
                self.resolve_managed_family(&mut c);
                let provider = Provider::new(c.clone())?;
                let mut _diagnostic_operation = None;
                let mut result = if action == "discover" {
                    provider.discover().await?
                } else {
                    let operation_id = request["operationId"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| format!("diagnostic-{}", uuid::Uuid::new_v4()));
                    let cancel = CancellationToken::new();
                    let binding = self.binding_gate.lock().await;
                    _diagnostic_operation =
                        Some(self.register_operation(&operation_id, cancel.clone())?);
                    drop(binding);
                    let outcome = async {
                        let (_permit, _) = self.acquire_inference(&c, &cancel, None).await?;
                        // Provider diagnostics join generation and receive inline;
                        // dropping this future closes HTTP rather than leaving a
                        // detached generation that could outlive its permit.
                        tokio::select! {
                            biased;
                            _=cancel.cancelled()=>bail!("兼容性诊断已取消"),
                            result=provider.diagnose()=>result,
                        }
                    }
                    .await;
                    outcome?
                };
                if action == "diagnose" {
                    let _binding = self.binding_gate.lock().await;
                    result["connection"] = connection_snapshot;
                    result["actualModelProfile"] =
                        self.config()["modelProfiles"][&connection_id].clone();
                    result["serverVersion"] = json!("unknown_unless_recorded_in_profile");
                    let mut config = self.config();
                    let current = config["connections"]
                        .as_array()
                        .and_then(|a| a.iter().find(|item| item["id"] == connection_id));
                    if !current.is_some_and(|item| same_connection(item, &result["connection"])) {
                        result["applied"] = json!(false);
                        result["note"] = json!(
                            "诊断配置与已保存连接不同；本次结果未用于启用能力，请保存后重新诊断。"
                        );
                        return Ok(result);
                    }
                    result["applied"] = json!(true);
                    config["diagnostics"][&connection_id] = result.clone();
                    self.persist(config)?;
                    std::fs::create_dir_all(self.dir.join("diagnostics"))?;
                    std::fs::write(
                        self.dir
                            .join("diagnostics")
                            .join(format!("{}.json", uuid::Uuid::new_v4())),
                        serde_json::to_vec_pretty(&result)?,
                    )?;
                }
                Ok(result)
            }
            "create_session" => {
                let _binding = self.binding_gate.lock().await;
                let cfg = self.config();
                let workspace = request["workspace"]
                    .as_str()
                    .unwrap_or(cfg["workspace"].as_str().unwrap_or(""));
                let device_id = request["deviceId"]
                    .as_str()
                    .unwrap_or(cfg["deviceId"].as_str().unwrap_or("local"));
                if workspace.trim().is_empty() {
                    bail!("请选择执行设备上的工作目录")
                }
                if device_id == "local" {
                    self.executor(workspace).await?;
                } else {
                    let caps = RemoteClient::new(self.remote_device(device_id)?)?
                        .capabilities()
                        .await?;
                    if caps["workspace"].as_str() != Some(workspace) {
                        bail!(
                            "执行设备共享目录为 {}；请在设备设置中读取共享目录后创建会话。",
                            caps["workspace"]
                        )
                    }
                }
                let now = chrono::Utc::now().to_rfc3339();
                let session = Session {
                    id: uuid::Uuid::new_v4().to_string(),
                    title: "新会话".into(),
                    workspace: workspace.into(),
                    device_id: device_id.into(),
                    connection_id: request["connectionId"]
                        .as_str()
                        .unwrap_or(cfg["selectedConnectionId"].as_str().unwrap_or("lmstudio"))
                        .into(),
                    created_at: now.clone(),
                    updated_at: now,
                };
                self.store.create_session(&session)?;
                Ok(serde_json::to_value(session)?)
            }
            "load_session" => {
                let id = required(&request, "sessionId")?;
                Ok(json!({"session":self.store.session(id)?,"events":self.store.events(id)?}))
            }
            "rename_session" => {
                self.store.rename(
                    required(&request, "sessionId")?,
                    required(&request, "title")?,
                )?;
                Ok(json!({"ok":true}))
            }
            "interrupt" => {
                let id = required(&request, "sessionId")?;
                if let Some(cancel) = self.active.lock().unwrap().get(id) {
                    cancel.cancel();
                }
                Ok(json!({"ok":true}))
            }
            "execute_tool" => self.manual_tool(request.clone(), Arc::new(|_| {})).await,
            "capabilities" => {
                let cfg = self.config();
                if cfg["deviceId"].as_str().unwrap_or("local") == "local" {
                    self.executor(cfg["workspace"].as_str().unwrap_or(""))
                        .await?
                        .capabilities()
                        .await
                } else {
                    RemoteClient::new(self.remote_device(cfg["deviceId"].as_str().unwrap())?)?
                        .capabilities()
                        .await
                }
            }
            "device_capabilities" => {
                RemoteClient::new(self.remote_device(required(&request, "deviceId")?)?)?
                    .capabilities()
                    .await
            }
            "browser_setup" => {
                let cfg = self.config();
                if cfg["deviceId"].as_str().unwrap_or("local") != "local" {
                    bail!("请在远端执行设备本机完成浏览器安装")
                }
                self.executor(cfg["workspace"].as_str().unwrap_or(""))
                    .await?
                    .setup_browser(CancellationToken::new())
                    .await
            }
            "remote_operation_status" => {
                let session = self.store.session(required(&request, "sessionId")?)?;
                let value = RemoteClient::new(self.remote_device(&session.device_id)?)?
                    .operation_status(required(&request, "operationId")?)
                    .await?;
                Ok(without_binary(value))
            }
            "artifact_fetch" => {
                let session = self.store.session(required(&request, "sessionId")?)?;
                self.fetch_session_artifact(&session, required(&request, "path")?)
                    .await
            }
            "engine_manifest" => Ok(serde_json::from_str(engine::RUNTIME_MANIFEST)?),
            "engine_install" => {
                self.engine
                    .install(
                        request["backend"]
                            .as_str()
                            .unwrap_or(if cfg!(target_os = "macos") {
                                "metal"
                            } else {
                                "cpu"
                            }),
                    )
                    .await
            }
            "inspect_gguf" => {
                let path = PathBuf::from(required(&request, "path")?);
                let harness = self.clone();
                tokio::task::spawn_blocking(move || harness.record_model_profile(&path)).await?
            }
            "engine_start" => {
                let _binding = self.binding_gate.lock().await;
                self.require_idle()?;
                let engine_config: engine::EngineConfig =
                    serde_json::from_value(request["config"].clone())?;
                let mut profile =
                    self.record_model_profile(Path::new(&engine_config.model_path))?;
                profile["runtime"] = json!({"pinnedInstallerVersion":"b12211","actualVersion":"unknown_until_probed","binaryPath":engine_config.binary_path,"loadedContextRequested":engine_config.context_length,"projectorPath":engine_config.projector_path});
                let result = self.engine.start(&engine_config)?;
                let mut config = self.config();
                config["modelProfiles"]["managed-llama"] = profile;
                config["engineConfig"] = request["config"].clone();
                // The managed alias is reused across starts. Never retain a
                // family/thinking choice or budget for the previous weights.
                if let Some(managed) =
                    config["connections"]
                        .as_array_mut()
                        .and_then(|connections| {
                            connections.iter_mut().find(|c| c["id"] == "managed-llama")
                        })
                {
                    managed["baseUrl"] = json!(format!("http://127.0.0.1:{}", engine_config.port));
                    managed["protocol"] = json!("chat_completions");
                    managed["model"] = json!("gemma-local");
                    managed["contextLength"] = json!(engine_config.context_length);
                    managed["backend"] = json!("llama_cpp");
                    managed["modelFamily"] = json!("auto");
                    managed["thinkingMode"] = json!("auto");
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
                self.persist(config)?;
                Ok(result)
            }
            "engine_stop" => self.engine.stop(),
            "engine_status" => self.engine.status(),
            "engine_log" => Ok(json!({"text":read_tail(&self.dir.join("engine.log"),24000)})),
            "pair_device" => {
                let device = RemoteClient::pair(required(&request, "invitation")?).await?;
                let _binding = self.binding_gate.lock().await;
                self.require_idle()?;
                write_secret(&format!("device:{}", device.id), &device.token)?;
                let mut public = serde_json::to_value(&device)?;
                public["token"] = json!("");
                let mut cfg = self.config();
                let devices = cfg["devices"].as_array_mut().context("devices 无效")?;
                devices.retain(|d| d["id"] != device.id);
                devices.push(public.clone());
                self.persist(cfg)?;
                Ok(public)
            }
            "remove_device" => {
                let _binding = self.binding_gate.lock().await;
                self.require_idle()?;
                let id = required(&request, "deviceId")?;
                let mut cfg = self.config();
                cfg["devices"]
                    .as_array_mut()
                    .context("devices 无效")?
                    .retain(|d| d["id"] != id);
                if cfg["deviceId"] == id {
                    cfg["deviceId"] = json!("local");
                    cfg["workspace"] = json!("");
                }
                let _ = keyring::Entry::new("dev.gemmaharness.desktop", &format!("device:{id}"))
                    .and_then(|e| e.delete_credential());
                self.persist(cfg)?;
                Ok(json!({"ok":true}))
            }
            "host_start" => {
                let mut host = self.host.lock().await;
                if host.is_some() {
                    bail!("执行设备服务已经运行")
                }
                let cfg = self.config();
                let workspace = request["workspace"]
                    .as_str()
                    .unwrap_or(cfg["workspace"].as_str().unwrap_or(""));
                if workspace.is_empty() {
                    bail!("先选择允许远端访问的工作目录")
                }
                let h = RemoteHost::start(
                    request["bindAddress"]
                        .as_str()
                        .unwrap_or("0.0.0.0:45455")
                        .into(),
                    required(&request, "advertisedUrl")?.into(),
                    self.dir.join("host"),
                    workspace.into(),
                )
                .await?;
                let invitation = h.pairing_invitation().await?;
                let status = h.status().await?;
                *host = Some(h);
                Ok(json!({"invitation":invitation,"status":status}))
            }
            "host_status" => {
                let host = self.host.lock().await;
                if let Some(h) = host.as_ref() {
                    h.status().await
                } else {
                    Ok(json!({"running":false}))
                }
            }
            "host_invitation" => {
                let host = self.host.lock().await;
                Ok(
                    json!({"invitation":host.as_ref().context("服务未启动")?.pairing_invitation().await?}),
                )
            }
            "host_stop" => {
                if let Some(host) = self.host.lock().await.take() {
                    host.stop().await?;
                }
                Ok(json!({"running":false}))
            }
            "host_revoke" => {
                let host = self.host.lock().await;
                host.as_ref()
                    .context("服务未启动")?
                    .revoke_device(required(&request, "deviceId")?)
                    .await
            }
            "host_devices" => {
                let host = self.host.lock().await;
                host.as_ref().context("服务未启动")?.devices().await
            }
            "host_cancel_all" => {
                let host = self.host.lock().await;
                host.as_ref().context("服务未启动")?.cancel_all().await
            }
            "export_session" => {
                let id = required(&request, "sessionId")?;
                Ok(json!({"session":self.store.session(id)?,"events":self.store.events(id)?}))
            }
            _ => bail!("未知操作 {action}"),
        }
    }
    pub async fn shutdown(&self) {
        for cancel in self.active.lock().unwrap().values() {
            cancel.cancel();
        }
        let ids: Vec<String> = self.desktop_leases.lock().await.keys().cloned().collect();
        for id in ids {
            self.release_desktop(&id).await;
        }
        let _ = self.engine.stop();
        if let Some(host) = self.host.lock().await.take() {
            let _ = host.stop().await;
        }
        self.mcp.reset().await;
    }
}
fn same_connection(a: &Value, b: &Value) -> bool {
    let (Ok(a), Ok(b)) = (
        serde_json::from_value::<ConnectionConfig>(a.clone()),
        serde_json::from_value::<ConnectionConfig>(b.clone()),
    ) else {
        return false;
    };
    a.id == b.id
        && a.base_url == b.base_url
        && a.protocol == b.protocol
        && a.model == b.model
        && a.context_length == b.context_length
        && a.model_family == b.model_family
        && a.backend == b.backend
        && a.thinking_mode == b.thinking_mode
        && a.max_output_tokens == b.max_output_tokens
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("缺少 {k}"))
}
fn read_secret(id: &str) -> Result<String> {
    Ok(keyring::Entry::new("dev.gemmaharness.desktop", id)?.get_password()?)
}
fn write_secret(id: &str, secret: &str) -> Result<()> {
    keyring::Entry::new("dev.gemmaharness.desktop", id)?
        .set_password(secret)
        .context("无法写入系统凭据库")
}
fn read_tail(path: &Path, max: usize) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(max)..]).into()
}

fn without_binary(mut value: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(o) => {
                if o.get("type").and_then(Value::as_str) == Some("image") {
                    o.remove("data");
                    o.insert("offloaded".into(), json!(true));
                }
                for v in o.values_mut() {
                    walk(v)
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v)
                }
            }
            _ => {}
        }
    }
    walk(&mut value);
    value
}
