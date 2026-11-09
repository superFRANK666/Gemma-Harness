//! Authenticated, certificate-pinned remote execution with durable deduplication.
//!
//! A host owns one explicitly chosen workspace. Inference stays with the caller;
//! this crate transports executor tools and never runs a second agent loop.

mod client;
mod host;
mod journal;
mod tls;

pub use client::{PairedDevice, RemoteClient};
pub use host::{RemoteHost, RemoteHostHandle};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

pub const PROTOCOL_VERSION: u32 = 1;
const INVITATION_PREFIX: &str = "harness-pair:v1:";
const MAX_MESSAGE_SIZE: usize = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invitation {
    protocol_version: u32,
    url: String,
    certificate_pin: String,
    secret: String,
    expires_at: u64,
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn validate_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid remote endpoint")?;
    if url.scheme() != "wss"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("remote endpoint must be a wss:// URL without credentials, query or fragment");
    }
    Ok(url)
}

fn validate_pin(pin: &str) -> Result<()> {
    if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("certificate pin must be a SHA-256 hexadecimal digest");
    }
    Ok(())
}

fn encode_invitation(value: &Invitation) -> Result<String> {
    Ok(format!(
        "{INVITATION_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?)
    ))
}

fn decode_invitation(value: &str) -> Result<Invitation> {
    let encoded = value
        .trim()
        .strip_prefix(INVITATION_PREFIX)
        .context("invalid pairing invitation")?;
    if encoded.len() > 8192 {
        bail!("pairing invitation is too large");
    }
    let invitation: Invitation = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
    validate_url(&invitation.url)?;
    validate_pin(&invitation.certificate_pin)?;
    if invitation.protocol_version != PROTOCOL_VERSION {
        bail!("incompatible remote protocol version");
    }
    if invitation.expires_at <= now_ms() {
        bail!("pairing invitation has expired");
    }
    if URL_SAFE_NO_PAD.decode(&invitation.secret)?.len() != 32 {
        bail!("invalid pairing secret");
    }
    Ok(invitation)
}

fn operation_digest(workspace: &str, name: &str, args: &Value) -> Result<String> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            _ => value.clone(),
        }
    }
    Ok(digest(&serde_json::to_vec(
        &serde_json::json!({"workspace":workspace,"name":name,"args":canonical(args)}),
    )?))
}

fn validate_operation_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 160
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
    {
        bail!("operation id must contain 1–160 ASCII letters, digits, '.', '_', ':' or '-'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitations_are_versioned_expiring_and_require_tls_pins() -> Result<()> {
        let mut invitation = Invitation {
            protocol_version: PROTOCOL_VERSION,
            url: "wss://127.0.0.1:1234/".into(),
            certificate_pin: "a".repeat(64),
            secret: random_secret(),
            expires_at: now_ms() + 300_000,
        };
        assert_eq!(
            decode_invitation(&encode_invitation(&invitation)?)?.secret,
            invitation.secret
        );
        invitation.expires_at = now_ms() - 1;
        assert!(decode_invitation(&encode_invitation(&invitation)?).is_err());
        invitation.expires_at = now_ms() + 300_000;
        invitation.url = "ws://localhost:1234/".into();
        assert!(decode_invitation(&encode_invitation(&invitation)?).is_err());
        Ok(())
    }
}
