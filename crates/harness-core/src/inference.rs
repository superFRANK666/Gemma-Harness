//! One generation at a time per server, shared by chat, summaries and diagnostics.
use crate::{EventSink, Harness};
use anyhow::{bail, Result};
use harness_model::ConnectionConfig;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

fn endpoint_key(base_url: &str) -> Result<String> {
    let url = reqwest::Url::parse(base_url)?;
    let host = url.host_str().unwrap_or_default();
    // Different API paths, connection IDs and loopback spellings share the KV cache.
    let host = if host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        "loopback"
    } else {
        host
    };
    Ok(format!(
        "{}://{}:{}",
        url.scheme(),
        host,
        url.port_or_known_default().unwrap_or(0)
    ))
}

impl Harness {
    pub(crate) async fn acquire_inference(
        &self,
        connection: &ConnectionConfig,
        cancel: &CancellationToken,
        sink: Option<&EventSink>,
    ) -> Result<(OwnedSemaphorePermit, u128)> {
        let endpoint = endpoint_key(&connection.base_url)?;
        let gate = self
            .endpoints
            .lock()
            .unwrap()
            .entry(endpoint.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();
        let started = std::time::Instant::now();
        if let Some(sink) = sink {
            sink(json!({"type":"inference_queued","endpoint":endpoint}));
        }
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("已停止等待推理资源"),
            permit = gate.acquire_owned() => permit?,
        };
        let wait_ms = started.elapsed().as_millis();
        if let Some(sink) = sink {
            sink(json!({"type":"inference_acquired","waitMs":wait_ms}));
        }
        Ok((permit, wait_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_paths_and_loopback_aliases_share_a_gate_without_leaking_credentials() {
        let key = endpoint_key("http://localhost:1234/v1/").unwrap();
        assert_eq!(key, endpoint_key("http://127.0.0.1:1234").unwrap());
        assert_eq!(
            key,
            endpoint_key("http://[::1]:1234/api/v1?secret=x").unwrap()
        );
        assert_eq!(
            key,
            endpoint_key("http://user:secret@LOCALHOST:1234/responses").unwrap()
        );
        assert_ne!(key, endpoint_key("https://localhost:1234").unwrap());
        assert_ne!(key, endpoint_key("http://localhost:1235").unwrap());
    }
    #[tokio::test]
    async fn aliases_serialize_cancellation_removes_waiter_and_release_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let h = Harness::open(dir.path().into()).unwrap();
        let first = ConnectionConfig::default();
        let mut alias = first.clone();
        alias.base_url = "http://localhost:1234".into();
        let cancel = CancellationToken::new();
        let (permit, _) = h.acquire_inference(&first, &cancel, None).await.unwrap();
        let waiter = h.acquire_inference(&alias, &cancel, None);
        tokio::pin!(waiter);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut waiter)
                .await
                .is_err()
        );
        cancel.cancel();
        assert!(waiter.await.is_err());
        drop(permit);
        assert!(h
            .acquire_inference(&alias, &CancellationToken::new(), None)
            .await
            .is_ok());
    }
}
