use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_LIST_PAGES: usize = 128;
const MAX_TOOLS: usize = 512;
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// A tools/call exchange was interrupted after its write began. Never replay it automatically.
#[derive(Debug)]
pub(crate) struct McpIndeterminateOperation {
    pub server_id: String,
    pub method: String,
    pub message: String,
}
impl std::fmt::Display for McpIndeterminateOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MCP {} {} 结果待确认：{}；操作可能已执行，请先观察实际状态，不要自动重复调用",
            self.server_id, self.method, self.message
        )
    }
}
impl std::error::Error for McpIndeterminateOperation {}

enum Reply {
    Success(Value),
    RemoteError(Value),
}
struct Peer {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    server_id: String,
    poisoned: bool,
    timeout: Duration,
}
impl Peer {
    fn start(config: &McpConfig) -> Result<Self> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("无法启动 MCP 服务器")?;
        let input = child.stdin.take().context("MCP stdin 不可用")?;
        let output = BufReader::new(child.stdout.take().context("MCP stdout 不可用")?);
        Ok(Self {
            child,
            input,
            output,
            next_id: 0,
            server_id: config.id.clone(),
            poisoned: false,
            timeout: REQUEST_TIMEOUT,
        })
    }
    async fn terminate(&mut self) {
        self.poisoned = true;
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
    }
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        cancel: CancellationToken,
        reset: CancellationToken,
    ) -> Result<Value> {
        if self.poisoned {
            bail!("MCP 连接已失效，需要重新初始化");
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("MCP 请求 id 超出范围")?;
        let id = self.next_id;
        let line =
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string() + "\n";
        if line.len() > MAX_FRAME_BYTES {
            bail!("MCP 请求超过 16 MiB 限制，未发送");
        }
        let mut write_started = false;
        let timeout = self.timeout;
        let outcome: Result<Reply> = {
            let exchange = async {
                write_started = true;
                self.input
                    .write_all(line.as_bytes())
                    .await
                    .context("MCP 请求写入失败")?;
                self.input.flush().await.context("MCP 请求 flush 失败")?;
                for _ in 0..4096 {
                    let line = read_frame(&mut self.output).await?;
                    let value: Value =
                        serde_json::from_slice(&line).context("MCP stdio 必须输出完整 JSONL")?;
                    if !value.is_object() || value["jsonrpc"] != "2.0" {
                        bail!("无效的 MCP JSON-RPC 消息");
                    }
                    if value.get("method").is_some() {
                        if value["method"].as_str().is_none_or(str::is_empty) {
                            bail!("无效的 MCP method");
                        }
                        if let Some(server_id) = value.get("id") {
                            if !valid_id(server_id) {
                                bail!("无效的 MCP 服务端请求 id");
                            }
                            let response=json!({"jsonrpc":"2.0","id":server_id,"error":{"code":-32601,"message":"Server-initiated requests are not supported"}}).to_string()+"\n";
                            self.input.write_all(response.as_bytes()).await?;
                            self.input.flush().await?;
                        }
                        continue;
                    }
                    if value.get("id") != Some(&json!(id)) {
                        bail!("MCP 响应 id 与当前请求不匹配");
                    }
                    match (value.get("result"), value.get("error")) {
                        (Some(result), None) => {
                            if !result.is_object() {
                                bail!("MCP result 必须是对象");
                            }
                            return Ok(Reply::Success(result.clone()));
                        }
                        (None, Some(error)) => {
                            if !error.is_object()
                                || error["code"].as_i64().is_none()
                                || error["message"].as_str().is_none()
                            {
                                bail!("无效的 MCP JSON-RPC error");
                            }
                            return Ok(Reply::RemoteError(error.clone()));
                        }
                        _ => bail!("MCP 响应必须且只能包含 result 或 error"),
                    }
                }
                bail!("MCP 通知/服务端请求数量超过单次交换限制")
            };
            tokio::select! {
                biased;
                _=cancel.cancelled()=>Err(anyhow!("MCP 调用已取消")),
                _=reset.cancelled()=>Err(anyhow!("MCP 配置已重置")),
                result=tokio::time::timeout(timeout,exchange)=>match result {
                    Ok(result)=>result,
                    Err(_)=>Err(anyhow!("MCP 整次交换超时（{} ms）",timeout.as_millis())),
                }
            }
        };
        match outcome {
            Ok(Reply::Success(value)) => Ok(value),
            Ok(Reply::RemoteError(error)) => bail!(
                "MCP: {}",
                error.to_string().chars().take(2048).collect::<String>()
            ),
            Err(error) => {
                self.terminate().await;
                if method == "tools/call" && write_started {
                    Err(McpIndeterminateOperation {
                        server_id: self.server_id.clone(),
                        method: method.into(),
                        message: format!("{error:#}"),
                    }
                    .into())
                } else {
                    Err(error)
                }
            }
        }
    }
    async fn initialized(
        &mut self,
        cancel: CancellationToken,
        reset: CancellationToken,
    ) -> Result<()> {
        let outcome = {
            let write = async {
                self.input
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                    .await?;
                self.input.flush().await?;
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! {
                biased;
                _=cancel.cancelled()=>Err(anyhow!("MCP 初始化已取消")),
                _=reset.cancelled()=>Err(anyhow!("MCP 配置已重置")),
                result=tokio::time::timeout(self.timeout,write)=>result.context("MCP initialized 通知超时")?,
            }
        };
        if outcome.is_err() {
            self.terminate().await;
        }
        outcome
    }
}
fn valid_id(value: &Value) -> bool {
    value.as_u64().is_some()
        || value.as_i64().is_some()
        || value
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 1024)
}
async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await.context("无法读取 MCP 输出")?;
        if available.is_empty() {
            if frame.is_empty() {
                bail!("MCP 进程退出，未返回响应");
            }
            bail!("MCP 输出在换行前截断");
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(available.len());
        if frame.len().saturating_add(count) > MAX_FRAME_BYTES {
            bail!("MCP 响应超过 16 MiB 限制");
        }
        let done = available[count - 1] == b'\n';
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if done {
            return Ok(frame);
        }
    }
}
#[derive(Default)]
struct Slot {
    peer: Mutex<Option<Peer>>,
    reset: CancellationToken,
}
#[derive(Default)]
pub struct McpManager {
    peers: Mutex<HashMap<String, Arc<Slot>>>,
    aliases: Mutex<HashMap<String, (String, String)>>,
}
impl McpManager {
    async fn slot(&self, id: &str) -> Arc<Slot> {
        self.peers
            .lock()
            .await
            .entry(id.into())
            .or_insert_with(|| Arc::new(Slot::default()))
            .clone()
    }
    async fn exchange(
        &self,
        c: &McpConfig,
        method: &str,
        params: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let slot = self.slot(&c.id).await;
        let mut peer = tokio::select! {
            biased;
            _=cancel.cancelled()=>bail!("MCP 等待已取消，操作未发送"),
            _=slot.reset.cancelled()=>bail!("MCP 配置已重置，操作未发送"),
            peer=slot.peer.lock()=>peer,
        };
        if let Some(existing) = peer.as_mut() {
            if existing.poisoned || existing.child.try_wait()?.is_some() {
                *peer = None;
            }
        }
        if peer.is_none() {
            let mut started = Peer::start(c)?;
            let initialization=async {
                let init=started.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gemma-harness","version":env!("CARGO_PKG_VERSION")}}),cancel.clone(),slot.reset.clone()).await?;
                let version=init["protocolVersion"].as_str().unwrap_or("");
                if !["2025-11-25","2025-06-18","2025-03-26","2024-11-05"].contains(&version) { bail!("不支持的 MCP 协议版本 {version}"); }
                started.initialized(cancel.clone(),slot.reset.clone()).await
            }.await;
            if let Err(error) = initialization {
                started.terminate().await;
                return Err(error);
            }
            *peer = Some(started);
        }
        let result = peer
            .as_mut()
            .unwrap()
            .request(method, params, cancel, slot.reset.clone())
            .await;
        let poisoned = peer.as_ref().is_some_and(|p| p.poisoned);
        if poisoned {
            *peer = None;
        }
        drop(peer);
        if poisoned {
            self.aliases
                .lock()
                .await
                .retain(|_, (server, _)| server != &c.id);
        }
        result
    }
    async fn invalidate(&self, id: &str) {
        let slot = self.peers.lock().await.get(id).cloned();
        if let Some(slot) = slot {
            let mut peer = slot.peer.lock().await;
            if let Some(mut peer) = peer.take() {
                peer.terminate().await;
            }
        }
        self.aliases
            .lock()
            .await
            .retain(|_, (server, _)| server != id);
    }
    pub async fn tools(&self, configs: &[McpConfig]) -> Result<Vec<Value>> {
        self.tools_with_cancel(configs, CancellationToken::new())
            .await
    }
    pub(crate) async fn tools_with_cancel(
        &self,
        configs: &[McpConfig],
        cancel: CancellationToken,
    ) -> Result<Vec<Value>> {
        Ok(self.discover(configs, cancel, false).await?.0)
    }
    /// Optional MCP servers are discovered only when requested by the agent.
    /// Incompatible tools and unavailable servers are reported individually.
    pub(crate) async fn tools_for_agent(
        &self,
        configs: &[McpConfig],
        cancel: CancellationToken,
    ) -> Result<(Vec<Value>, Vec<Value>)> {
        self.discover(configs, cancel, true).await
    }
    async fn discover(
        &self,
        configs: &[McpConfig],
        cancel: CancellationToken,
        tolerant: bool,
    ) -> Result<(Vec<Value>, Vec<Value>)> {
        let mut tools = Vec::new();
        let mut warnings = Vec::new();
        let mut catalog_bytes = 0usize;
        let mut catalog_tools = 0usize;
        for config in configs.iter().filter(|c| c.enabled) {
            self.aliases
                .lock()
                .await
                .retain(|_, (server, _)| server != &config.id);
            let listing_future = async {
                let mut cursor: Option<String> = None;
                let mut seen = HashSet::new();
                let mut names = HashSet::new();
                let mut registered = Vec::new();
                let mut registration = HashMap::new();
                for page in 0..MAX_LIST_PAGES {
                    let params = cursor
                        .as_ref()
                        .map(|c| json!({"cursor":c}))
                        .unwrap_or(json!({}));
                    let result = self
                        .exchange(config, "tools/list", params, cancel.clone())
                        .await?;
                    for tool in result["tools"]
                        .as_array()
                        .context("MCP tools/list 缺少 tools 数组")?
                    {
                        let name = tool["name"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .context("MCP 工具缺少名称")?;
                        if !names.insert(name.to_owned()) {
                            bail!("MCP 分页返回重复工具名称 {name}");
                        }
                        catalog_tools += 1;
                        if catalog_tools > MAX_TOOLS {
                            bail!("MCP 工具目录超过 {MAX_TOOLS} 个工具，未静默截断");
                        }
                        catalog_bytes = catalog_bytes
                            .checked_add(tool.to_string().len())
                            .context("MCP 工具目录长度溢出")?;
                        if catalog_bytes > MAX_CATALOG_BYTES {
                            bail!("MCP 工具目录超过 16 MiB 限制");
                        }
                        if let Err(error) = validate_schema(&tool["inputSchema"]) {
                            if !tolerant {
                                return Err(error);
                            }
                            warnings.push(json!({"server":config.name,"tool":name,"error":format!("{error:#}").chars().take(500).collect::<String>()}));
                            continue;
                        }
                        let hash =
                            hex::encode(Sha256::digest(format!("{}:{name}", config.id).as_bytes()));
                        let alias = format!("mcp_{}", &hash[..20]);
                        registration.insert(alias.clone(), name.to_owned());
                        registered.push(json!({"name":alias,"description":format!("[{}] {}: {}",config.name,name,tool["description"].as_str().unwrap_or("")),"parameters":tool["inputSchema"]}));
                    }
                    cursor = match result.get("nextCursor").filter(|v| !v.is_null()) {
                        Some(value) => Some(
                            value
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .context("MCP nextCursor 必须是非空字符串")?
                                .to_owned(),
                        ),
                        None => None,
                    };
                    if let Some(next) = &cursor {
                        if !seen.insert(next.clone()) {
                            bail!("MCP tools/list 重复 nextCursor，已停止无效分页");
                        }
                        if page + 1 == MAX_LIST_PAGES {
                            bail!("MCP tools/list 超过 {MAX_LIST_PAGES} 页，未静默截断");
                        }
                    } else {
                        break;
                    }
                }
                Ok((registered, registration))
            };
            let listing: Result<(Vec<Value>, HashMap<String, String>)> = if tolerant {
                match tokio::time::timeout(Duration::from_secs(5), listing_future).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow!("MCP 工具发现超时")),
                }
            } else {
                listing_future.await
            };
            let (registered, registration) = match listing {
                Ok(value) => value,
                Err(error) => {
                    self.invalidate(&config.id).await;
                    if cancel.is_cancelled() || !tolerant {
                        return Err(error);
                    }
                    warnings.push(json!({"server":config.name,"error":format!("{error:#}").chars().take(500).collect::<String>()}));
                    continue;
                }
            };
            let mut aliases = self.aliases.lock().await;
            for tool in &registered {
                let alias = tool["name"].as_str().unwrap();
                let name = registration.get(alias).context("MCP 别名注册失败")?.clone();
                aliases.insert(alias.into(), (config.id.clone(), name));
            }
            tools.extend(registered);
        }
        Ok((tools, warnings))
    }
    pub async fn call(
        &self,
        configs: &[McpConfig],
        alias: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let (server, name) = self
            .aliases
            .lock()
            .await
            .get(alias)
            .cloned()
            .context("未知的 MCP 工具别名")?;
        let config = configs
            .iter()
            .find(|c| c.id == server && c.enabled)
            .context("MCP 服务已禁用")?;
        self.exchange(
            config,
            "tools/call",
            json!({"name":name,"arguments":args}),
            cancel,
        )
        .await
    }
    pub async fn reset(&self) {
        let slots: Vec<_> = std::mem::take(&mut *self.peers.lock().await)
            .into_values()
            .collect();
        for slot in &slots {
            slot.reset.cancel();
        }
        self.aliases.lock().await.clear();
        for slot in slots {
            if let Some(mut peer) = slot.peer.lock().await.take() {
                peer.terminate().await;
            }
        }
    }
}

pub fn validate_schema(schema: &Value) -> Result<()> {
    harness_model::normalize_tool_schema(schema).context("Gemma 工具 schema 无法安全转换")?;
    jsonschema::validator_for(schema).context("无效的 JSON Schema")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn config(script: &str) -> McpConfig {
        McpConfig {
            id: "fixture".into(),
            name: "Fixture server".into(),
            command: "node".into(),
            args: vec!["-e".into(), script.into()],
            enabled: true,
        }
    }
    const GOOD_RPC: &str = r#"const rl=require('node:readline').createInterface({input:process.stdin});rl.on('line',line=>{const m=JSON.parse(line);if(m.id===undefined)return;let reply;if(m.params.fail)reply={error:{code:-32000,message:'legitimate fixture error'}};else reply={result:m.params.toolError?{isError:true,content:[{type:'text',text:'tool failed'}]}:{ok:true,pid:process.pid}};process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,...reply})+'\n');});"#;

    #[tokio::test]
    async fn legal_rpc_and_tool_errors_keep_the_same_process_alive() {
        let mut peer = Peer::start(&config(GOOD_RPC)).unwrap();
        let pid = peer.child.id();
        let error = peer
            .request(
                "tools/call",
                json!({"fail":true}),
                CancellationToken::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<McpIndeterminateOperation>().is_none());
        assert!(!peer.poisoned);
        assert!(peer.child.try_wait().unwrap().is_none());
        let result = peer
            .request(
                "tools/call",
                json!({"toolError":true}),
                CancellationToken::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(peer.child.id(), pid);
        assert!(!peer.poisoned);
        let result = peer
            .request(
                "tools/call",
                json!({}),
                CancellationToken::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
        peer.terminate().await;
    }

    #[tokio::test]
    async fn cancellation_and_timeout_cover_a_blocked_stdin_write_and_reap_process() {
        for cancel_request in [false, true] {
            let mut peer =
                Peer::start(&config("process.stdin.pause();setInterval(()=>{},1000)")).unwrap();
            peer.timeout = if cancel_request {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(30)
            };
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            let task = tokio::spawn(async move {
                if cancel_request {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    trigger.cancel();
                }
            });
            let result = peer
                .request(
                    "tools/call",
                    json!({"payload":"x".repeat(2*1024*1024)}),
                    cancel,
                    CancellationToken::new(),
                )
                .await;
            task.await.unwrap();
            let error = result.unwrap_err();
            assert!(
                error.downcast_ref::<McpIndeterminateOperation>().is_some(),
                "{error:#}"
            );
            assert!(peer.poisoned);
            assert!(peer.child.try_wait().unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn malformed_frame_is_indeterminate_and_is_never_reused() {
        let mut peer=Peer::start(&config("process.stdin.once('data',()=>{process.stdout.write('not json\\n');});setInterval(()=>{},1000)")).unwrap();
        let error = peer
            .request(
                "tools/call",
                json!({}),
                CancellationToken::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<McpIndeterminateOperation>().is_some());
        assert!(peer.poisoned);
        assert!(peer.child.try_wait().unwrap().is_some());
        assert!(peer
            .request(
                "tools/call",
                json!({}),
                CancellationToken::new(),
                CancellationToken::new()
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn frame_reader_rejects_oversized_unterminated_output_and_partial_eof() {
        let (mut writer, reader) = tokio::io::duplex(8192);
        let task = tokio::spawn(async move {
            let block = vec![b'x'; 8192];
            for _ in 0..(MAX_FRAME_BYTES / 8192 + 1) {
                if writer.write_all(&block).await.is_err() {
                    break;
                }
            }
        });
        assert!(read_frame(&mut BufReader::new(reader)).await.is_err());
        task.abort();
        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(b"{\"partial\":true}").await.unwrap();
        drop(writer);
        assert!(read_frame(&mut BufReader::new(reader)).await.is_err());
    }

    const MANAGED_RPC: &str = r#"const fs=require('node:fs');const mode=process.argv[1];if(process.argv[2])fs.appendFileSync(process.argv[2],process.pid+'\n');let page=0;const rl=require('node:readline').createInterface({input:process.stdin});rl.on('line',line=>{const m=JSON.parse(line);if(m.id===undefined)return;let result;if(m.method==='initialize'){result={protocolVersion:'2025-11-25',capabilities:{}};}else if(m.method==='tools/list'){page++;if(mode==='cursor')result={tools:[],nextCursor:'repeat'};else if(mode==='pages')result={tools:[],nextCursor:String(page)};else if(mode==='many')result={tools:Array.from({length:513},(_,i)=>({name:'tool_'+i,inputSchema:{type:'object'}}))};else result={tools:[{name:'original_touch_name',description:'Important action',inputSchema:{type:'object'}}]};}else if(m.method==='tools/call'){process.stdout.write('broken protocol\n');return;}process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result})+'\n');});"#;

    #[tokio::test]
    async fn transport_failure_evicts_peer_and_next_discovery_starts_a_fresh_process() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("pids");
        let mut c = config(MANAGED_RPC);
        c.args
            .extend(["normal".into(), marker.to_string_lossy().into_owned()]);
        let manager = McpManager::default();
        let tools = manager.tools(&[c.clone()]).await.unwrap();
        assert!(tools[0]["description"]
            .as_str()
            .unwrap()
            .contains("original_touch_name: Important action"));
        let error = manager
            .call(
                &[c.clone()],
                tools[0]["name"].as_str().unwrap(),
                json!({}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<McpIndeterminateOperation>().is_some());
        let slot = manager.slot(&c.id).await;
        assert!(slot.peer.lock().await.is_none());
        assert!(manager.aliases.lock().await.is_empty());
        manager.tools(&[c]).await.unwrap();
        let pids = std::fs::read_to_string(marker).unwrap();
        let pids: Vec<_> = pids.lines().collect();
        assert_eq!(pids.len(), 2);
        assert_ne!(pids[0], pids[1]);
        manager.reset().await;
    }

    #[tokio::test]
    async fn repeated_cursor_page_limit_and_catalog_limit_are_errors_not_truncation() {
        for mode in ["cursor", "pages", "many"] {
            let mut c = config(MANAGED_RPC);
            c.args.push(mode.into());
            let manager = McpManager::default();
            let error = manager.tools(&[c.clone()]).await.unwrap_err();
            let text = error.to_string();
            assert!(
                text.contains("nextCursor") || text.contains("128") || text.contains("512"),
                "{text}"
            );
            assert!(manager.aliases.lock().await.is_empty());
            assert!(manager.slot(&c.id).await.peer.lock().await.is_none());
            manager.reset().await;
        }
    }

    #[tokio::test]
    async fn waiting_for_a_busy_peer_is_cancellable_and_reset_terminates_inflight_exchange() {
        let mut c = config(MANAGED_RPC);
        c.args.push("normal".into());
        let manager = Arc::new(McpManager::default());
        manager.tools(&[c.clone()]).await.unwrap();
        let slot = manager.slot(&c.id).await;
        let guard = slot.peer.lock().await;
        let cancel = CancellationToken::new();
        let task = {
            let manager = manager.clone();
            let c = c.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { manager.exchange(&c, "tools/list", json!({}), cancel).await })
        };
        cancel.cancel();
        assert!(task.await.unwrap().is_err());
        drop(guard);
        manager.reset().await;
        assert!(manager.peers.lock().await.is_empty());
    }
}
