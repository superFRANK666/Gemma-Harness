use crate::{
    decode_invitation, tls, validate_operation_id, validate_pin, validate_url, MAX_MESSAGE_SIZE,
    PROTOCOL_VERSION,
};
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{mpsc::UnboundedSender, Mutex},
};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub url: String,
    pub certificate_pin: String,
    /// Persist this only in the platform credential store, never in application logs.
    pub token: String,
}

impl fmt::Debug for PairedDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairedDevice")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("certificate_pin", &self.certificate_pin)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

type Socket = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

#[derive(Clone)]
pub struct RemoteClient {
    inner: Arc<ClientState>,
}

struct ClientState {
    device: PairedDevice,
    socket: Mutex<Option<Socket>>,
    next_id: AtomicU64,
}

#[derive(Debug)]
struct RemoteRpcError(String);
impl fmt::Display for RemoteRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "remote executor: {}", self.0)
    }
}
impl std::error::Error for RemoteRpcError {}

impl RemoteClient {
    pub async fn pair(invitation: &str) -> Result<PairedDevice> {
        let invitation = decode_invitation(invitation)?;
        let mut socket = connect(&invitation.url, &invitation.certificate_pin).await?;
        let value = request(
            &mut socket,
            1,
            "pair",
            json!({"protocolVersion":PROTOCOL_VERSION,"secret":invitation.secret}),
        )
        .await?;
        let device: PairedDevice =
            serde_json::from_value(value).context("invalid pairing reply")?;
        if device.url != invitation.url
            || !device
                .certificate_pin
                .eq_ignore_ascii_case(&invitation.certificate_pin)
            || device.id.is_empty()
            || device.token.is_empty()
        {
            bail!("pairing reply does not match its pinned invitation");
        }
        let _ = socket.close(None).await;
        Ok(device)
    }

    pub fn new(device: PairedDevice) -> Result<Self> {
        validate_url(&device.url)?;
        validate_pin(&device.certificate_pin)?;
        if device.id.is_empty() || device.token.is_empty() {
            bail!("device identity and token are required");
        }
        Ok(Self {
            inner: Arc::new(ClientState {
                device,
                socket: Mutex::new(None),
                next_id: AtomicU64::new(1),
            }),
        })
    }

    pub async fn execute(
        &self,
        operation_id: &str,
        name: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        self.execute_inner(operation_id, name, args, cancel, None)
            .await
    }

    /// Stream a bounded preview of this operation's actual stdout/stderr while
    /// retaining the existing durable result and cancellation contract.
    pub async fn execute_streaming(
        &self,
        operation_id: &str,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        events: UnboundedSender<Value>,
    ) -> Result<Value> {
        self.execute_inner(operation_id, name, args, cancel, Some(events))
            .await
    }

    async fn execute_inner(
        &self,
        operation_id: &str,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        events: Option<UnboundedSender<Value>>,
    ) -> Result<Value> {
        validate_operation_id(operation_id)?;
        if cancel.is_cancelled() {
            bail!("operation cancelled before dispatch");
        }
        let mut operation = tokio::select! {
            result=self.rpc("operation.execute",json!({"operationId":operation_id,"name":name,"args":args})) => result
                .with_context(||format!("operation {operation_id}; query its remote status before issuing a new operation"))?,
            _=cancel.cancelled() => return Err(self.request_cancellation(operation_id).await),
        };
        let mut cursor = 0u64;
        let mut forwarded_bytes = 0usize;
        loop {
            if let Some(events) = events.as_ref() {
                // Filter server-side by this operation: starting at zero never
                // floods the caller with another task's historical output.
                for _ in 0..4 {
                    let page = tokio::select! {
                        result=self.rpc("events.after",json!({"cursor":cursor,"operationId":operation_id}))=>result?,
                        _=cancel.cancelled()=>return Err(self.request_cancellation(operation_id).await),
                    };
                    let entries = page["events"]
                        .as_array()
                        .context("remote event page has no events")?;
                    for event in entries {
                        if event["operationId"] != operation_id
                            || event["payload"]["type"] != "tool_output"
                        {
                            continue;
                        }
                        let mut payload = event["payload"].clone();
                        let Some(text) = payload["text"].as_str() else {
                            continue;
                        };
                        let remaining = (32usize * 1024).saturating_sub(forwarded_bytes);
                        let mut end = text.len().min(remaining);
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        if end == 0 {
                            continue;
                        }
                        forwarded_bytes += end;
                        payload["text"] = Value::String(text[..end].to_owned());
                        payload["operationId"] = json!(operation_id);
                        payload["cursor"] = event["cursor"].clone();
                        let _ = events.send(payload);
                    }
                    cursor = page["cursor"]
                        .as_u64()
                        .context("remote event page has no cursor")?;
                    if entries.len() < 256 {
                        break;
                    }
                }
            }
            match operation["state"]
                .as_str()
                .context("operation reply has no state")?
            {
                "completed" => return Ok(operation.get("result").cloned().unwrap_or(Value::Null)),
                "failed" | "cancelled" | "indeterminate" => bail!(
                    "remote operation {}: {}",
                    operation["state"].as_str().unwrap_or("failed"),
                    operation["error"]
                        .as_str()
                        .unwrap_or("inspect the remote operation status")
                ),
                "accepted" | "dispatching" => {}
                other => bail!("unsupported remote operation state: {other}"),
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(self.request_cancellation(operation_id).await);
                }
                _ = tokio::time::sleep(Duration::from_millis(150)) => {}
            }
            operation = tokio::select! {
                result=self.operation_status(operation_id) => result
                    .with_context(||format!("connection interrupted; operation {operation_id} may still be running remotely"))?,
                _=cancel.cancelled() => return Err(self.request_cancellation(operation_id).await),
            };
        }
    }

    pub async fn capabilities(&self) -> Result<Value> {
        self.rpc("capabilities", json!({})).await
    }
    pub async fn operation_status(&self, id: &str) -> Result<Value> {
        validate_operation_id(id)?;
        self.rpc("operation.status", json!({"operationId":id}))
            .await
    }
    pub async fn cancel(&self, id: &str) -> Result<Value> {
        validate_operation_id(id)?;
        self.rpc("operation.cancel", json!({"operationId":id}))
            .await
    }
    /// Durable operation transitions, resumable with the returned cursor.
    pub async fn events_after(&self, cursor: u64) -> Result<Value> {
        self.rpc("events.after", json!({"cursor":cursor})).await
    }

    /// Retrieve only files inside the host's dedicated artifact directory.
    pub async fn fetch_artifact(&self, path: &str) -> Result<Value> {
        self.rpc("artifact.fetch", json!({"path":path})).await
    }

    /// Hold one desktop workflow across observation, model thinking and input.
    /// Renew every 20 seconds and always release when the turn ends.
    pub async fn desktop_acquire(&self, session_id: &str) -> Result<Value> {
        validate_operation_id(session_id)?;
        self.rpc(
            "desktop.lease",
            json!({"action":"acquire","sessionId":session_id}),
        )
        .await
    }
    pub async fn desktop_renew(&self, session_id: &str) -> Result<Value> {
        validate_operation_id(session_id)?;
        self.rpc(
            "desktop.lease",
            json!({"action":"renew","sessionId":session_id}),
        )
        .await
    }
    pub async fn desktop_release(&self, session_id: &str) -> Result<Value> {
        validate_operation_id(session_id)?;
        self.rpc(
            "desktop.lease",
            json!({"action":"release","sessionId":session_id}),
        )
        .await
    }

    async fn request_cancellation(&self, operation_id: &str) -> anyhow::Error {
        // An interrupted RPC may have an unread response. Discard that stream
        // before sending cancellation on a freshly authenticated connection.
        let delivered = tokio::time::timeout(Duration::from_secs(3), async {
            self.inner.socket.lock().await.take();
            self.cancel(operation_id).await
        })
        .await;
        match delivered {
            Ok(Ok(_)) => anyhow::anyhow!("cancellation requested for remote operation {operation_id}; already delivered desktop input cannot be undone"),
            _ => anyhow::anyhow!("remote operation {operation_id} was interrupted; cancellation could not be confirmed; inspect remote status before issuing a new operation"),
        }
    }

    /// Forget the live transport; reconnecting keeps the same operation identity.
    pub async fn disconnect(&self) {
        if let Some(mut socket) = self.inner.socket.lock().await.take() {
            let _ = socket.close(None).await;
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let mut connection = self.inner.socket.lock().await;
        let mut last_error = None;
        // All retries preserve method arguments and operationId. The host's
        // durable journal decides whether an execute request may dispatch.
        for _ in 0..2 {
            if connection.is_none() {
                match self.authenticated_connection().await {
                    Ok(socket) => *connection = Some(socket),
                    Err(error) => return Err(error),
                }
            }
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            let result = request(connection.as_mut().unwrap(), id, method, params.clone()).await;
            match result {
                Ok(value) => return Ok(value),
                Err(error) if error.downcast_ref::<RemoteRpcError>().is_some() => {
                    return Err(error)
                }
                Err(error) => {
                    *connection = None;
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("remote connection failed")))
    }

    async fn authenticated_connection(&self) -> Result<Socket> {
        let device = &self.inner.device;
        let mut socket = connect(&device.url, &device.certificate_pin).await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let hello = request(
            &mut socket,
            id,
            "authenticate",
            json!({"protocolVersion":PROTOCOL_VERSION,"token":device.token}),
        )
        .await?;
        if hello["deviceId"].as_str() != Some(device.id.as_str())
            || hello["protocolVersion"].as_u64() != Some(PROTOCOL_VERSION as u64)
        {
            bail!("remote endpoint identity or protocol changed; pair this device again");
        }
        Ok(socket)
    }
}

async fn connect(endpoint: &str, pin: &str) -> Result<Socket> {
    let url = validate_url(endpoint)?;
    let host = url.host_str().context("remote URL has no host")?;
    // url::host_str retains brackets for IPv6; DNS and TLS need the literal.
    let network_host = host.trim_start_matches('[').trim_end_matches(']');
    let port = url.port_or_known_default().unwrap_or(443);
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((network_host, port)),
    )
    .await
    .context("remote TCP connect timed out")??;
    stream.set_nodelay(true)?;
    let server_name =
        ServerName::try_from(network_host.to_string()).context("invalid TLS server name")?;
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tls::client_tls(pin)?.connect(server_name, stream),
    )
    .await
    .context("remote TLS handshake timed out")?
    .context("remote TLS certificate/pin verification failed")?;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_SIZE);
    config.max_frame_size = Some(MAX_MESSAGE_SIZE);
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        client_async_with_config(endpoint, stream, Some(config)),
    )
    .await
    .context("remote WebSocket handshake timed out")??;
    Ok(socket)
}

async fn request(socket: &mut Socket, id: u64, method: &str, params: Value) -> Result<Value> {
    let payload =
        serde_json::to_string(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
    if payload.len() > MAX_MESSAGE_SIZE {
        bail!("remote request exceeds the 32 MiB message limit");
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        socket.send(Message::Text(payload.into())).await?;
        loop {
            let frame = socket
                .next()
                .await
                .context("remote connection closed before reply")??;
            match frame {
                Message::Text(text) => {
                    let reply: Value =
                        serde_json::from_str(text.as_str()).context("invalid JSON-RPC reply")?;
                    if reply["id"].as_u64() != Some(id) {
                        bail!("remote response id does not match request");
                    }
                    if let Some(error) = reply.get("error") {
                        return Err(RemoteRpcError(
                            error["message"]
                                .as_str()
                                .unwrap_or("request rejected")
                                .to_string(),
                        )
                        .into());
                    }
                    return reply
                        .get("result")
                        .cloned()
                        .context("remote reply has no result");
                }
                Message::Ping(payload) => socket.send(Message::Pong(payload)).await?,
                Message::Pong(_) => {}
                Message::Close(_) => bail!("remote closed the connection"),
                _ => bail!("unexpected remote binary message"),
            }
        }
    })
    .await
    .context("remote RPC timed out; query operation status before creating a new operation")?
}
