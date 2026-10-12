//! TLS 配置：自签名证书生成、PEM 加载、服务端 / 客户端配置。
//!
//! 仅启用 TLS 1.3。
//!
//! 与 frp 保持一致：客户端未配置 `trustedCaFile` 时**不校验服务端证书**（frp 的默认行为），
//! 安全性由 TLS 加密 + 令牌鉴权保证；配置了 CA 则严格校验。

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};

use crate::error::TunnelError;

/// ALPN 协议名。
pub const ALPN: &[u8] = b"rfrp/1";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

fn tls_err(e: impl std::fmt::Display) -> TunnelError {
    TunnelError::Tls(e.to_string())
}

/// 证书链 + 私钥。
pub struct Identity {
    /// 证书链。
    pub certs: Vec<CertificateDer<'static>>,
    /// 私钥。
    pub key: PrivateKeyDer<'static>,
}

/// 生成自签名证书（边缘节点未配置证书时使用）。
pub fn self_signed(names: Vec<String>) -> Result<Identity, TunnelError> {
    let ck = rcgen::generate_simple_self_signed(names).map_err(tls_err)?;
    let key = PrivateKeyDer::try_from(ck.signing_key.serialize_der()).map_err(tls_err)?;
    Ok(Identity {
        certs: vec![ck.cert.der().clone()],
        key,
    })
}

/// 生成可持久化的自签名证书和私钥 PEM。
pub fn self_signed_pem(names: Vec<String>) -> Result<(String, String), TunnelError> {
    let ck = rcgen::generate_simple_self_signed(names).map_err(tls_err)?;
    Ok((ck.cert.pem(), ck.signing_key.serialize_pem()))
}

/// 从 PEM 文本解析证书链与私钥。
pub fn identity_from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Identity, TunnelError> {
    let certs = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(tls_err)?;
    if certs.is_empty() {
        return Err(TunnelError::Tls("no certificate found in PEM".into()));
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(tls_err)?;
    Ok(Identity { certs, key })
}

/// 解析 CA 证书 PEM。
pub fn certs_from_pem(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, TunnelError> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(tls_err)?;
    if certs.is_empty() {
        return Err(TunnelError::Tls("no certificate found in PEM".into()));
    }
    Ok(certs)
}

/// 服务端 TLS 配置。
pub fn server_config(id: Identity) -> Result<Arc<ServerConfig>, TunnelError> {
    let mut cfg = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_err)?
        .with_no_client_auth()
        .with_single_cert(id.certs, id.key)
        .map_err(tls_err)?;
    cfg.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(cfg))
}

/// 客户端 TLS 配置：给出 CA 则校验，否则接受任意证书（frp 默认行为）。
pub fn client_config(
    trusted: Option<Vec<CertificateDer<'static>>>,
) -> Result<Arc<ClientConfig>, TunnelError> {
    let builder = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_err)?;
    let mut cfg = match trusted {
        Some(cas) => {
            let mut roots = RootCertStore::empty();
            for ca in cas {
                roots.add(ca).map_err(tls_err)?;
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider(), None, None)))
            .with_no_client_auth(),
    };
    cfg.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(cfg))
}

/// 叶证书 DER 的 SHA-256（与 openssl x509 指纹口径一致，不含分隔符）。
pub fn certificate_fingerprint(cert: &CertificateDer<'_>) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(cert.as_ref()) {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// 同时验证可选 CA 与固定叶证书；无 pin 时沿用 frp 默认行为。
pub fn client_config_pinned(
    trusted: Option<Vec<CertificateDer<'static>>>,
    pin: Option<&str>,
) -> Result<Arc<ClientConfig>, TunnelError> {
    let Some(pin) = pin else {
        return client_config(trusted);
    };
    if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(tls_err("TLS pin must be 64 hexadecimal SHA256 characters"));
    }
    let verifier: Option<Arc<dyn ServerCertVerifier>> = match &trusted {
        Some(certs) => {
            let mut roots = RootCertStore::empty();
            for cert in certs {
                roots.add(cert.clone()).map_err(tls_err)?;
            }
            Some(
                rustls::client::WebPkiServerVerifier::builder_with_provider(
                    Arc::new(roots),
                    provider(),
                )
                .build()
                .map_err(tls_err)?,
            )
        }
        None => None,
    };
    let mut cfg = client_config(trusted)?.as_ref().clone();
    cfg.dangerous().set_certificate_verifier(Arc::new(AcceptAny(
        provider(),
        Some(pin.to_ascii_lowercase()),
        verifier,
    )));
    Ok(Arc::new(cfg))
}

/// 应用层（本地插件）服务端配置：TLS 1.2 / 1.3，ALPN 为 `http/1.1`。
pub fn app_server_config(id: Identity) -> Result<Arc<ServerConfig>, TunnelError> {
    let mut cfg = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .with_no_client_auth()
        .with_single_cert(id.certs, id.key)
        .map_err(tls_err)?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// 应用层（本地插件）客户端配置：不校验证书（与 frp 插件的 `InsecureSkipVerify` 一致），TLS 1.2 / 1.3。
pub fn app_client_config() -> Result<Arc<ClientConfig>, TunnelError> {
    app_client_config_pinned(None, None)
}

/// 管理接口 / 仪表盘使用的 HTTP/1.1 客户端 TLS 配置，可同时固定证书或信任 CA。
pub fn app_client_config_pinned(
    trusted: Option<Vec<CertificateDer<'static>>>,
    pin: Option<&str>,
) -> Result<Arc<ClientConfig>, TunnelError> {
    if let Some(pin) = pin {
        if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(tls_err("TLS pin must be 64 hexadecimal SHA256 characters"));
        }
    }
    let trusted_verifier: Option<Arc<dyn ServerCertVerifier>> = match &trusted {
        Some(certs) => {
            let mut roots = RootCertStore::empty();
            for cert in certs {
                roots.add(cert.clone()).map_err(tls_err)?;
            }
            Some(
                rustls::client::WebPkiServerVerifier::builder_with_provider(
                    Arc::new(roots),
                    provider(),
                )
                .build()
                .map_err(tls_err)?,
            )
        }
        None => None,
    };
    let verifier = AcceptAnyApp {
        provider: provider(),
        pin: pin.map(str::to_ascii_lowercase),
        trusted_verifier,
    };
    let mut cfg = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// 接受任意证书、同时支持 TLS 1.2 / 1.3 签名校验。
#[derive(Debug)]
struct AcceptAnyApp {
    provider: Arc<CryptoProvider>,
    pin: Option<String>,
    trusted_verifier: Option<Arc<dyn ServerCertVerifier>>,
}

impl ServerCertVerifier for AcceptAnyApp {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(verifier) = &self.trusted_verifier {
            verifier.verify_server_cert(end_entity, intermediates, server_name, ocsp, now)?;
        }
        if self
            .pin
            .as_ref()
            .is_some_and(|pin| certificate_fingerprint(end_entity) != *pin)
        {
            return Err(rustls::Error::General(
                "server certificate pin mismatch".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct AcceptAny(
    Arc<CryptoProvider>,
    Option<String>,
    Option<Arc<dyn ServerCertVerifier>>,
);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(verifier) = &self.2 {
            verifier.verify_server_cert(end_entity, intermediates, server_name, ocsp, now)?;
        }
        if self
            .1
            .as_ref()
            .is_some_and(|pin| certificate_fingerprint(end_entity) != *pin)
        {
            return Err(rustls::Error::General(
                "server certificate pin mismatch".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    async fn handshake(
        client: Arc<ClientConfig>,
        server: Arc<ServerConfig>,
    ) -> Result<Vec<u8>, std::io::Error> {
        let (c, s) = duplex(64 * 1024);
        let acceptor = TlsAcceptor::from(server);
        let srv = tokio::spawn(async move {
            let mut t = acceptor.accept(s).await?;
            t.write_all(b"pong").await?;
            t.flush().await?;
            Ok::<_, std::io::Error>(())
        });
        let name = ServerName::try_from("localhost").unwrap();
        let mut t = TlsConnector::from(client).connect(name, c).await?;
        let mut buf = vec![0u8; 4];
        t.read_exact(&mut buf).await?;
        srv.await.unwrap()?;
        Ok(buf)
    }

    #[tokio::test]
    async fn accepts_any_certificate_by_default_like_frp() {
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let got = handshake(client_config(None).unwrap(), server_config(id).unwrap())
            .await
            .unwrap();
        assert_eq!(got, b"pong");
    }

    #[tokio::test]
    async fn app_configs_interoperate_and_accept_any_certificate() {
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let got = handshake(app_client_config().unwrap(), app_server_config(id).unwrap())
            .await
            .unwrap();
        assert_eq!(got, b"pong");
        // TLS 1.2 only 的服务端也能握手（本地被代理服务可能很旧）。
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let mut old = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(id.certs, id.key)
            .unwrap();
        old.alpn_protocols = vec![b"http/1.1".to_vec()];
        let got = handshake(app_client_config().unwrap(), Arc::new(old))
            .await
            .unwrap();
        assert_eq!(got, b"pong");
    }

    #[tokio::test]
    async fn pinned_app_client_uses_http11_and_verifies_the_certificate_pin() {
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let pin = certificate_fingerprint(&id.certs[0]);
        let client = app_client_config_pinned(None, Some(&pin)).unwrap();
        assert_eq!(client.alpn_protocols, vec![b"http/1.1".to_vec()]);
        assert_eq!(
            handshake(client, app_server_config(id).unwrap())
                .await
                .unwrap(),
            b"pong"
        );
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let cert = id.certs[0].clone();
        let pin = certificate_fingerprint(&cert);
        let mut tls12 = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(id.certs, id.key)
            .unwrap();
        tls12.alpn_protocols = vec![b"http/1.1".to_vec()];
        assert!(handshake(
            app_client_config_pinned(Some(vec![cert]), Some(&pin)).unwrap(),
            Arc::new(tls12)
        )
        .await
        .is_ok());
        let id = self_signed(vec!["localhost".into()]).unwrap();
        assert!(handshake(
            app_client_config_pinned(None, Some(&"0".repeat(64))).unwrap(),
            app_server_config(id).unwrap()
        )
        .await
        .is_err());
        assert!(app_client_config_pinned(None, Some("invalid")).is_err());
        assert!(app_client_config_pinned(None, None).is_ok());
    }

    #[tokio::test]
    async fn verifies_when_ca_is_configured() {
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let ca = vec![id.certs[0].clone()];
        let ok = handshake(client_config(Some(ca)).unwrap(), server_config(id).unwrap()).await;
        assert!(ok.is_ok());

        let other = self_signed(vec!["localhost".into()]).unwrap();
        let wrong_ca = self_signed(vec!["localhost".into()]).unwrap().certs;
        let bad = handshake(
            client_config(Some(wrong_ca)).unwrap(),
            server_config(other).unwrap(),
        )
        .await;
        assert!(bad.is_err());
    }

    #[test]
    fn pem_parsing() {
        let ck = rcgen::generate_simple_self_signed(vec!["x".to_string()]).unwrap();
        let (cert, key) = (ck.cert.pem(), ck.signing_key.serialize_pem());
        let id = identity_from_pem(cert.as_bytes(), key.as_bytes()).unwrap();
        assert_eq!(id.certs.len(), 1);
        assert_eq!(certs_from_pem(cert.as_bytes()).unwrap().len(), 1);
        assert!(server_config(id).is_ok());
        assert!(identity_from_pem(b"", key.as_bytes()).is_err());
        assert!(identity_from_pem(cert.as_bytes(), b"garbage").is_err());
        assert!(certs_from_pem(b"").is_err());
        assert!(
            certs_from_pem(b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n")
                .is_err()
        );
        assert!(client_config(Some(vec![CertificateDer::from(vec![1u8, 2, 3])])).is_err());
    }

    #[test]
    fn mismatched_key_is_rejected() {
        let a = self_signed(vec!["a".into()]).unwrap();
        let b = self_signed(vec!["b".into()]).unwrap();
        let mixed = Identity {
            certs: a.certs,
            key: b.key,
        };
        assert!(server_config(mixed).is_err());
    }
    #[tokio::test]
    async fn pinned_handshake_never_falls_back_on_mismatch() {
        let id = self_signed(vec!["localhost".into()]).unwrap();
        let cert = id.certs[0].clone();
        let pin = certificate_fingerprint(&cert);
        assert_eq!(pin.len(), 64);
        let server = server_config(id).unwrap();
        assert_eq!(
            handshake(
                client_config_pinned(None, Some(&pin)).unwrap(),
                server.clone()
            )
            .await
            .unwrap(),
            b"pong"
        );
        assert!(handshake(
            client_config_pinned(None, Some(&"0".repeat(64))).unwrap(),
            server.clone()
        )
        .await
        .is_err());
        assert!(handshake(
            client_config_pinned(Some(vec![cert]), Some(&pin)).unwrap(),
            server
        )
        .await
        .is_ok());
        assert!(client_config_pinned(None, Some("invalid")).is_err());
        assert!(client_config_pinned(None, Some(&"z".repeat(64))).is_err());
        assert!(client_config_pinned(Some(vec![]), Some(&pin)).is_err());
        assert!(
            client_config_pinned(Some(vec![CertificateDer::from(vec![1])]), Some(&pin)).is_err()
        );
        assert!(client_config_pinned(None, None).is_ok());

        let id = self_signed(vec!["localhost".into()]).unwrap();
        let tls12_pin = certificate_fingerprint(&id.certs[0]);
        let mut tls12 = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(id.certs, id.key)
            .unwrap();
        tls12.alpn_protocols = vec![b"http/1.1".to_vec()];
        assert!(handshake(
            client_config_pinned(None, Some(&tls12_pin)).unwrap(),
            Arc::new(tls12)
        )
        .await
        .is_err());
    }
    #[test]
    fn persisted_pem_roundtrips_identity() {
        let (cert, key) = self_signed_pem(vec!["localhost".into()]).unwrap();
        let id = identity_from_pem(cert.as_bytes(), key.as_bytes()).unwrap();
        assert_eq!(id.certs.len(), 1);
        assert!(server_config(id).is_ok());
    }
}
