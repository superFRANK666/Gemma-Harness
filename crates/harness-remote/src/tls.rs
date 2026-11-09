use crate::{digest, validate_pin};
use anyhow::{bail, Context, Result};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, Error, ServerConfig, SignatureScheme,
};
use std::{path::Path, sync::Arc};
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub(crate) fn server_tls(directory: &Path) -> Result<(TlsAcceptor, String)> {
    let certificate_path = directory.join("host-certificate.der");
    let key_path = directory.join("host-private-key.der");
    if certificate_path.exists() != key_path.exists() {
        bail!(
            "remote TLS identity is incomplete; restore its matching certificate and private key"
        );
    }
    if !certificate_path.exists() {
        let identity = rcgen::generate_simple_self_signed(vec![
            "localhost".into(),
            "127.0.0.1".into(),
            "::1".into(),
        ])?;
        // Create the private key with restrictive permissions from its first byte.
        write_new_private(&key_path, &identity.key_pair.serialize_der())?;
        if let Err(error) = write_new_private(&certificate_path, identity.cert.der().as_ref()) {
            let _ = std::fs::remove_file(&key_path);
            return Err(error);
        }
    }
    let certificate = std::fs::read(&certificate_path)?;
    let key = std::fs::read(&key_path)?;
    let pin = digest(&certificate);
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .context("invalid persisted remote TLS identity")?;
    Ok((TlsAcceptor::from(Arc::new(config)), pin))
}

pub(crate) fn client_tls(pin: &str) -> Result<TlsConnector> {
    validate_pin(pin)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinnedCertificate {
        pin: pin.to_ascii_lowercase(),
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// The invitation's exact certificate pin is the trust anchor for a self-signed
/// host. TLS CertificateVerify signatures are still verified by rustls/ring;
/// knowing a public certificate never suffices to impersonate its private key.
#[derive(Debug)]
struct PinnedCertificate {
    pin: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, Error> {
        if digest(end_entity.as_ref()) != self.pin {
            return Err(Error::General("remote certificate pin mismatch".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
