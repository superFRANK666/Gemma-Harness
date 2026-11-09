use crate::{
    digest, encode_invitation, journal::Journal, now_ms, operation_digest, random_secret, tls,
    validate_operation_id, validate_url, Invitation, MAX_MESSAGE_SIZE, PROTOCOL_VERSION,
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use futures_util::{SinkExt, StreamExt};
use harness_executor::Executor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Notify, Semaphore},
    task::{JoinHandle, JoinSet},
};
use tokio_tungstenite::{
    accept_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub struct RemoteHost;

#[derive(Clone)]
pub struct RemoteHostHandle {
    state: Arc<HostState>,
    listener: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    name: String,
}

struct Running {
    owner: String,
    desktop_session: Option<String>,
    cancellation: CancellationToken,
}
struct DesktopLease {
    owner: String,
    session: String,
    expires: Instant,
}
const DESKTOP_LEASE_TTL: Duration = Duration::from_secs(60);

struct HostState {
    identity: Identity,
    url: String,
    certificate_pin: String,
    workspace: PathBuf,
    artifact_dir: PathBuf,
    journal: Mutex<Journal>,
    admission: Mutex<()>,
    invitations: Mutex<HashMap<String, u64>>,
    running: Mutex<HashMap<String, Running>>,
    desktop_lease: Mutex<Option<DesktopLease>>,
    journal_error: Mutex<Option<String>>,
    stopped: CancellationToken,
    settled: Notify,
    executor: Arc<Executor>,
    data_lock: Mutex<Option<File>>,
}

impl RemoteHost {
    pub async fn start(
        bind_addr: String,
        advertised_url: String,
        data_dir: PathBuf,
        workspace: PathBuf,
    ) -> Result<RemoteHostHandle> {
        let workspace = workspace
            .canonicalize()
            .context("remote workspace is unavailable")?;
        if !workspace.is_dir() {
            bail!("remote workspace must be a directory");
        }
        std::fs::create_dir_all(&data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join("host.lock"))?;
        lock.try_lock_exclusive()
            .context("another remote executor already uses this data directory")?;
        let (acceptor, certificate_pin) = tls::server_tls(&data_dir)?;
        let identity_path = data_dir.join("host-identity.json");
        let identity = if identity_path.exists() {
            serde_json::from_slice(&std::fs::read(&identity_path)?)?
        } else {
            let identity = Identity {
                id: Uuid::new_v4().to_string(),
                name: format!("Harness ({})", std::env::consts::OS),
            };
            std::fs::write(&identity_path, serde_json::to_vec(&identity)?)?;
            identity
        };
        let mut journal = Journal::open(&data_dir.join("operations.sqlite"))?;
        journal.recover()?;
        let executor = Arc::new(Executor::new(
            workspace.clone(),
            data_dir.join("artifacts"),
        )?);
        let artifact_dir = data_dir.join("artifacts").canonicalize()?;
        let listener = TcpListener::bind(&bind_addr)
            .await
            .context("cannot listen for remote execution")?;
        let mut endpoint = validate_url(&advertised_url)?;
        if endpoint.port() == Some(0) {
            endpoint
                .set_port(Some(listener.local_addr()?.port()))
                .map_err(|_| anyhow::anyhow!("invalid advertised port"))?;
        }
        let state = Arc::new(HostState {
            identity,
            url: endpoint.to_string(),
            certificate_pin,
            workspace,
            artifact_dir,
            journal: Mutex::new(journal),
            admission: Mutex::new(()),
            invitations: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            desktop_lease: Mutex::new(None),
            journal_error: Mutex::new(None),
            stopped: CancellationToken::new(),
            settled: Notify::new(),
            executor,
            data_lock: Mutex::new(Some(lock)),
        });
        let host = state.clone();
        let task = tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(64));
            let mut connections = JoinSet::new();
            let mut lease_watchdog = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = host.stopped.cancelled() => break,
                    _ = lease_watchdog.tick() => {
                        let _admission=host.admission.lock().unwrap();
                        expire_desktop_locked(&host);
                    },
                    result = listener.accept() => match result {
                        Ok((stream,_)) => {
                            let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                            let acceptor = acceptor.clone();
                            let state = host.clone();
                            connections.spawn(async move {
                                let _permit = permit;
                                let _ = serve_connection(stream,acceptor,state).await;
                            });
                        }
                        Err(error) => { *host.journal_error.lock().unwrap()=Some(format!("remote listener failed: {error}")); break; }
                    },
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            {
                let _admission = host.admission.lock().unwrap();
                host.stopped.cancel();
                host.invitations.lock().unwrap().clear();
                host.desktop_lease.lock().unwrap().take();
                for running in host.running.lock().unwrap().values() {
                    running.cancellation.cancel();
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(RemoteHostHandle {
            state,
            listener: Arc::new(tokio::sync::Mutex::new(Some(task))),
        })
    }
}

impl RemoteHostHandle {
    pub async fn pairing_invitation(&self) -> Result<String> {
        if self.state.stopped.is_cancelled() {
            bail!("remote host is stopped");
        }
        let secret = random_secret();
        let expires_at = now_ms() + 5 * 60 * 1000;
        let mut invitations = self.state.invitations.lock().unwrap();
        invitations.retain(|_, expiry| *expiry > now_ms());
        if invitations.len() >= 5 {
            bail!("too many active pairing invitations; wait five minutes");
        }
        invitations.insert(digest(secret.as_bytes()), expires_at);
        encode_invitation(&Invitation {
            protocol_version: PROTOCOL_VERSION,
            url: self.state.url.clone(),
            certificate_pin: self.state.certificate_pin.clone(),
            secret,
            expires_at,
        })
    }

    pub async fn status(&self) -> Result<Value> {
        let journal = self.state.journal.lock().unwrap();
        Ok(
            json!({"id":self.state.identity.id,"name":self.state.identity.name,"url":self.state.url,"certificatePin":self.state.certificate_pin,"workspace":self.state.workspace,"running":!self.state.stopped.is_cancelled(),"protocolVersion":PROTOCOL_VERSION,"pairings":journal.pairings()?,"operations":journal.recent()?,"desktopLease":desktop_lease_status(&self.state),"journalError":self.state.journal_error.lock().unwrap().clone()}),
        )
    }

    /// Revoke one local pairing and cancel its admitted work immediately.
    pub async fn revoke(&self, pairing_id: &str) -> Result<()> {
        let _admission = self.state.admission.lock().unwrap();
        self.state.journal.lock().unwrap().revoke(pairing_id)?;
        {
            let mut lease = self.state.desktop_lease.lock().unwrap();
            if lease
                .as_ref()
                .is_some_and(|lease| lease.owner == pairing_id)
            {
                lease.take();
            }
        }
        for operation in self.state.running.lock().unwrap().values() {
            if operation.owner == pairing_id {
                operation.cancellation.cancel();
            }
        }
        Ok(())
    }

    /// Local Companion management API. IDs here identify paired credentials;
    /// PairedDevice.id on a controller identifies this host itself.
    pub async fn devices(&self) -> Result<Value> {
        self.state.journal.lock().unwrap().pairings()
    }

    pub async fn revoke_device(&self, pairing_id: &str) -> Result<Value> {
        self.revoke(pairing_id).await?;
        self.devices().await
    }

    pub async fn cancel_all(&self) -> Result<Value> {
        let _admission = self.state.admission.lock().unwrap();
        self.state.desktop_lease.lock().unwrap().take();
        let operations: Vec<_> = self
            .state
            .running
            .lock()
            .unwrap()
            .iter()
            .map(|(id, running)| {
                running.cancellation.cancel();
                (id.clone(), running.owner.clone())
            })
            .collect();
        let journal = self.state.journal.lock().unwrap();
        for (id, owner) in &operations {
            journal.record_cancel(id, owner)?;
        }
        Ok(
            json!({"cancellationRequested":operations.into_iter().map(|(id,_)|id).collect::<Vec<_>>()}),
        )
    }

    pub async fn stop(&self) -> Result<()> {
        {
            let _admission = self.state.admission.lock().unwrap();
            self.state.stopped.cancel();
            self.state.invitations.lock().unwrap().clear();
            self.state.desktop_lease.lock().unwrap().take();
            for operation in self.state.running.lock().unwrap().values() {
                operation.cancellation.cancel();
            }
        }
        if let Some(task) = self.listener.lock().await.take() {
            task.await?;
        }
        let drained = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let changed = self.state.settled.notified();
                if self.state.running.lock().unwrap().is_empty() {
                    break;
                }
                changed.await;
            }
        })
        .await;
        if drained.is_err() {
            bail!("listener stopped; some remote operations are still cancelling; inspect their status");
        }
        self.state.data_lock.lock().unwrap().take();
        Ok(())
    }
}

async fn serve_connection(
    stream: TcpStream,
    acceptor: tokio_rustls::TlsAcceptor,
    state: Arc<HostState>,
) -> Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await??;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_SIZE);
    config.max_frame_size = Some(MAX_MESSAGE_SIZE);
    let mut socket = tokio::time::timeout(
        Duration::from_secs(10),
        accept_async_with_config(stream, Some(config)),
    )
    .await??;
    let mut owner = None;
    loop {
        let idle_limit = if owner.is_some() {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(10)
        };
        let Some(frame) = tokio::time::timeout(idle_limit, socket.next()).await? else {
            break;
        };
        let request = match frame? {
            Message::Text(text) => serde_json::from_str::<Value>(text.as_str())?,
            Message::Ping(payload) => {
                socket.send(Message::Pong(payload)).await?;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(_) => break,
            _ => bail!("only JSON text frames are supported"),
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let response = match dispatch(&state, &mut owner, &request).await {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error.to_string()}})
            }
        };
        socket
            .send(Message::Text(serde_json::to_string(&response)?.into()))
            .await?;
    }
    Ok(())
}

async fn dispatch(
    state: &Arc<HostState>,
    owner: &mut Option<String>,
    request: &Value,
) -> Result<Value> {
    if request["jsonrpc"] != "2.0" || request.get("id").is_none() {
        bail!("invalid JSON-RPC request");
    }
    let method = request["method"].as_str().context("missing RPC method")?;
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    if method == "pair" {
        if owner.is_some() {
            bail!("already authenticated");
        }
        require_version(&params)?;
        let secret = params["secret"]
            .as_str()
            .context("missing pairing secret")?;
        if secret.len() > 128 {
            bail!("invalid pairing secret");
        }
        let hash = digest(secret.as_bytes());
        let mut invitations = state.invitations.lock().unwrap();
        let expiry = invitations
            .get(&hash)
            .copied()
            .context("pairing invitation is invalid or already used")?;
        if expiry <= now_ms() {
            invitations.remove(&hash);
            bail!("pairing invitation has expired");
        }
        let token = random_secret();
        let pairing_id = Uuid::new_v4().to_string();
        state
            .journal
            .lock()
            .unwrap()
            .add_pairing(&pairing_id, &digest(token.as_bytes()))?;
        invitations.remove(&hash);
        return Ok(
            json!({"id":state.identity.id,"name":state.identity.name,"url":state.url,"certificatePin":state.certificate_pin,"token":token}),
        );
    }
    if method == "authenticate" {
        require_version(&params)?;
        let token = params["token"].as_str().context("missing device token")?;
        if token.len() > 128 {
            bail!("invalid device token");
        }
        *owner = Some(
            state
                .journal
                .lock()
                .unwrap()
                .authenticate(&digest(token.as_bytes()))?
                .context("device token is invalid or revoked")?,
        );
        return Ok(
            json!({"protocolVersion":PROTOCOL_VERSION,"deviceId":state.identity.id,"workspace":state.workspace}),
        );
    }
    let owner = owner.as_deref().context("authentication required")?;
    if !state.journal.lock().unwrap().pairing_active(owner)? {
        bail!("device token is revoked");
    }
    if state.stopped.is_cancelled() {
        bail!("remote host is stopping");
    }
    match method {
        "capabilities" => Ok(
            json!({"protocolVersion":PROTOCOL_VERSION,"deviceId":state.identity.id,"name":state.identity.name,"platform":std::env::consts::OS,"workspace":state.workspace,"artifactFetch":true,"desktopSessionLease":{"required":true,"ttlSeconds":60,"renewIntervalSeconds":20},"executor":state.executor.capabilities().await?}),
        ),
        "desktop.lease" => {
            let _admission = state.admission.lock().unwrap();
            if state.stopped.is_cancelled() {
                bail!("remote host is stopping");
            }
            if !state.journal.lock().unwrap().pairing_active(owner)? {
                bail!("device token is revoked");
            }
            expire_desktop_locked(state);
            let session = params["sessionId"]
                .as_str()
                .context("missing desktop session id")?;
            validate_operation_id(session)?;
            let action = params["action"]
                .as_str()
                .context("missing desktop lease action")?;
            let mut lease = state.desktop_lease.lock().unwrap();
            match action {
                "acquire" => {
                    if lease
                        .as_ref()
                        .is_some_and(|lease| lease.owner != owner || lease.session != session)
                    {
                        bail!("remote desktop is leased to another session");
                    }
                    *lease = Some(DesktopLease {
                        owner: owner.into(),
                        session: session.into(),
                        expires: Instant::now() + DESKTOP_LEASE_TTL,
                    });
                    Ok(json!({"acquired":true,"sessionId":session,"ttlSeconds":60}))
                }
                "renew" => {
                    let current = lease.as_mut().context(
                        "desktop lease expired or was released; acquire it before observing again",
                    )?;
                    if current.owner != owner || current.session != session {
                        bail!("desktop lease belongs to another pairing or session");
                    }
                    current.expires = Instant::now() + DESKTOP_LEASE_TTL;
                    Ok(json!({"renewed":true,"sessionId":session,"ttlSeconds":60}))
                }
                "release" => {
                    if lease
                        .as_ref()
                        .is_some_and(|lease| lease.owner != owner || lease.session != session)
                    {
                        bail!("desktop lease belongs to another pairing or session");
                    }
                    lease.take();
                    drop(lease);
                    cancel_desktop_session(state, owner, session);
                    Ok(json!({"released":true,"sessionId":session}))
                }
                _ => bail!("unknown desktop lease action"),
            }
        }
        "operation.execute" => {
            // Revocation/stop and operation admission share one short critical
            // section, so a newly inserted operation cannot miss cancellation.
            let _admission = state.admission.lock().unwrap();
            if state.stopped.is_cancelled() {
                bail!("remote host is stopping");
            }
            if !state.journal.lock().unwrap().pairing_active(owner)? {
                bail!("device token is revoked");
            }
            let id = params["operationId"]
                .as_str()
                .context("missing operation id")?;
            validate_operation_id(id)?;
            let name = params["name"].as_str().context("missing tool name")?;
            if name.is_empty() || name.len() > 200 {
                bail!("invalid tool name");
            }
            let mut args = params.get("args").cloned().unwrap_or_else(|| json!({}));
            let workspace = state.workspace.to_string_lossy();
            let desktop_session = if name.starts_with("desktop_") {
                let session = args["__harness_session_id"]
                    .as_str()
                    .context("desktop tool requires __harness_session_id and a live session lease")?
                    .to_owned();
                validate_operation_id(&session)?;
                Some(session)
            } else {
                None
            };
            let hash = if let Some(session) = &desktop_session {
                operation_digest(
                    &workspace,
                    name,
                    &json!({"args":args,"pairingId":owner,"desktopSessionId":session}),
                )?
            } else {
                operation_digest(&workspace, name, &args)?
            };
            if let Some(existing) = state.journal.lock().unwrap().existing(id, &hash, owner)? {
                return Ok(existing);
            }
            if let Some(session) = &desktop_session {
                expire_desktop_locked(state);
                let lease = state.desktop_lease.lock().unwrap();
                let lease = lease.as_ref().context(
                    "desktop session lease is required before remote observation or input",
                )?;
                if lease.owner != owner || lease.session != *session {
                    bail!("desktop tool session does not own this desktop lease");
                }
                args.as_object_mut()
                    .context("desktop tool args must be an object")?
                    .remove("__harness_session_id");
            }
            let accepted = state
                .journal
                .lock()
                .unwrap()
                .accept(id, &hash, owner, &workspace, name)?;
            if accepted {
                let cancellation = CancellationToken::new();
                state.running.lock().unwrap().insert(
                    id.to_string(),
                    Running {
                        owner: owner.to_string(),
                        desktop_session,
                        cancellation: cancellation.clone(),
                    },
                );
                let host = state.clone();
                let id = id.to_string();
                let name = name.to_string();
                tokio::spawn(async move {
                    run_operation(host, id, name, args, cancellation).await;
                });
            }
            state.journal.lock().unwrap().operation(id, Some(owner))
        }
        "operation.status" => state.journal.lock().unwrap().operation(
            params["operationId"]
                .as_str()
                .context("missing operation id")?,
            Some(owner),
        ),
        "operation.cancel" => {
            let id = params["operationId"]
                .as_str()
                .context("missing operation id")?;
            state.journal.lock().unwrap().record_cancel(id, owner)?;
            if let Some(running) = state.running.lock().unwrap().get(id) {
                running.cancellation.cancel();
            }
            state.journal.lock().unwrap().operation(id, Some(owner))
        }
        "events.after" => state.journal.lock().unwrap().events_for_operation(
            owner,
            params["cursor"].as_u64().unwrap_or(0),
            params["operationId"].as_str(),
        ),
        "artifact.fetch" => {
            let requested =
                PathBuf::from(params["path"].as_str().context("missing artifact path")?);
            let path = if requested.is_absolute() {
                requested
            } else {
                state.artifact_dir.join(requested)
            };
            let path = tokio::fs::canonicalize(path)
                .await
                .context("artifact is unavailable")?;
            if !path.starts_with(&state.artifact_dir)
                || !tokio::fs::metadata(&path).await?.is_file()
            {
                bail!("artifact must be a file inside this host's artifact directory");
            }
            let metadata = tokio::fs::metadata(&path).await?;
            if metadata.len() > 16 * 1024 * 1024 {
                bail!("artifact exceeds the 16 MiB transfer limit");
            }
            use tokio::io::AsyncReadExt;
            let file = tokio::fs::File::open(&path).await?;
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > 16 * 1024 * 1024 {
                bail!("artifact grew beyond the 16 MiB transfer limit");
            }
            use base64::Engine;
            let mime_type = match path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
            {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "txt" | "log" => "text/plain; charset=utf-8",
                _ => "application/octet-stream",
            };
            Ok(
                json!({"name":path.file_name().and_then(|value|value.to_str()),"data":base64::engine::general_purpose::STANDARD.encode(&bytes),"mimeType":mime_type,"sha256":digest(&bytes),"bytes":bytes.len(),"deviceId":state.identity.id}),
            )
        }
        _ => bail!("unknown RPC method"),
    }
}

fn require_version(params: &Value) -> Result<()> {
    if params["protocolVersion"].as_u64() != Some(PROTOCOL_VERSION as u64) {
        bail!("incompatible remote protocol version");
    }
    Ok(())
}

fn desktop_lease_status(state: &HostState) -> Value {
    let lease = state.desktop_lease.lock().unwrap();
    match lease
        .as_ref()
        .filter(|lease| lease.expires > Instant::now())
    {
        Some(lease) => {
            json!({"pairingId":lease.owner,"sessionId":lease.session,"expiresInMs":lease.expires.saturating_duration_since(Instant::now()).as_millis()})
        }
        None => Value::Null,
    }
}

fn cancel_desktop_session(state: &HostState, owner: &str, session: &str) {
    for running in state.running.lock().unwrap().values() {
        if running.owner == owner && running.desktop_session.as_deref() == Some(session) {
            running.cancellation.cancel();
        }
    }
}

/// Caller holds admission, so expiry cannot race a newly admitted desktop call.
fn expire_desktop_locked(state: &HostState) {
    let expired = {
        let mut lease = state.desktop_lease.lock().unwrap();
        if lease
            .as_ref()
            .is_some_and(|lease| lease.expires <= Instant::now())
        {
            lease.take()
        } else {
            None
        }
    };
    if let Some(lease) = expired {
        cancel_desktop_session(state, &lease.owner, &lease.session);
    }
}

async fn run_operation(
    state: Arc<HostState>,
    id: String,
    name: String,
    args: Value,
    cancellation: CancellationToken,
) {
    let result = async {
        if cancellation.is_cancelled() {
            bail!("operation cancelled before dispatch");
        }
        state.journal.lock().unwrap().dispatch(&id)?;
        if cancellation.is_cancelled() {
            bail!("operation cancelled before execution");
        }
        let (events, mut output) = tokio::sync::mpsc::unbounded_channel();
        let mut execution = Box::pin(state.executor.execute_streaming(
            &name,
            args,
            cancellation.clone(),
            events,
        ));
        let mut output_open = true;
        let mut preview_bytes = 0usize;
        let result = loop {
            tokio::select! {
                result=&mut execution=>break result,
                event=output.recv(),if output_open=>match event {
                    Some(event)=>journal_stream_output(&state,&id,event,&mut preview_bytes),
                    None=>output_open=false,
                }
            }
        };
        drop(execution);
        // Persist all queued preview chunks before the terminal state. A client
        // observing completed can then drain the complete bounded preview.
        while let Ok(event) = output.try_recv() {
            journal_stream_output(&state, &id, event, &mut preview_bytes);
        }
        result
    }
    .await;
    let settlement = match result {
        Ok(value) => state.journal.lock().unwrap().finish(
            &id,
            if value["cancelled"] == true {
                "cancelled"
            } else {
                "completed"
            },
            Some(&value),
            None,
        ),
        Err(error) => state.journal.lock().unwrap().finish(
            &id,
            if error
                .downcast_ref::<harness_executor::IndeterminateToolOperation>()
                .is_some()
            {
                "indeterminate"
            } else if cancellation.is_cancelled() {
                "cancelled"
            } else {
                "failed"
            },
            None,
            Some(&error.to_string()),
        ),
    };
    if let Err(error) = settlement {
        *state.journal_error.lock().unwrap() = Some(format!(
            "operation {id} could not be settled: {error}; do not replay it"
        ));
    }
    state.running.lock().unwrap().remove(&id);
    state.settled.notify_waiters();
}

fn journal_stream_output(state: &HostState, id: &str, event: Value, preview_bytes: &mut usize) {
    if event["type"] != "tool_output" {
        return;
    }
    let Some(stream) = event["stream"]
        .as_str()
        .filter(|stream| matches!(*stream, "stdout" | "stderr"))
    else {
        return;
    };
    let Some(text) = event["text"].as_str() else {
        return;
    };
    let remaining = (32usize * 1024).saturating_sub(*preview_bytes);
    if remaining == 0 {
        return;
    }
    let mut end = text.len().min(remaining);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return;
    }
    *preview_bytes += end;
    let mut payload = json!({"type":"tool_output","stream":stream,"text":&text[..end]});
    if event["truncated"] == true || end < text.len() {
        payload["truncated"] = Value::Bool(true);
    }
    if let Err(error) = state.journal.lock().unwrap().record_output(id, &payload) {
        *state.journal_error.lock().unwrap() = Some(format!(
            "operation {id} output preview could not be journaled: {error}"
        ));
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use crate::RemoteClient;

    #[tokio::test]
    async fn expired_lease_cancels_only_its_desktop_work_and_can_be_reacquired() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace)?;
        let host = RemoteHost::start(
            "127.0.0.1:0".into(),
            "wss://127.0.0.1:0/".into(),
            temporary.path().join("host"),
            workspace,
        )
        .await?;
        let client =
            RemoteClient::new(RemoteClient::pair(&host.pairing_invitation().await?).await?)?;
        client.desktop_acquire("expired-session").await?;
        let owner = host
            .state
            .desktop_lease
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .owner
            .clone();
        let desktop = CancellationToken::new();
        let terminal = CancellationToken::new();
        host.state.running.lock().unwrap().insert(
            "test-desktop".into(),
            Running {
                owner: owner.clone(),
                desktop_session: Some("expired-session".into()),
                cancellation: desktop.clone(),
            },
        );
        host.state.running.lock().unwrap().insert(
            "test-terminal".into(),
            Running {
                owner,
                desktop_session: None,
                cancellation: terminal.clone(),
            },
        );
        host.state
            .desktop_lease
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .expires = Instant::now() - Duration::from_millis(1);
        assert!(client.desktop_renew("expired-session").await.is_err());
        assert!(desktop.is_cancelled());
        assert!(!terminal.is_cancelled());
        client.desktop_acquire("replacement-session").await?;
        assert_eq!(
            host.status().await?["desktopLease"]["sessionId"],
            "replacement-session"
        );
        host.state.running.lock().unwrap().clear();
        host.stop().await?;
        Ok(())
    }
}
