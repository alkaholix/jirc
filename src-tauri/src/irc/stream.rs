//! Network transport: a unified stream over plain TCP or TLS, optionally via a
//! SOCKS4/SOCKS5 proxy and optional local-address binding.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{lookup_host, TcpSocket, TcpStream};
use tokio::time::{timeout, Duration};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_socks::tcp::{Socks4Stream, Socks5Stream};

use crate::config::{ProxyKind, SaslMechanism, ServerProfile};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A connection stream that is either plain TCP or TLS. Both inner types are
/// `Unpin`, so delegation is straightforward.
pub enum NetStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    pub peer_ip: String,
    pub tls_version: String,
    pub tls_peer_certificate: Vec<u8>,
    pub tls_cert_valid: bool,
}

impl NetStream {
    pub fn connection_info(&self) -> ConnectionInfo {
        match self {
            Self::Plain(stream) => ConnectionInfo {
                peer_ip: stream
                    .peer_addr()
                    .map(|addr| addr.ip().to_string())
                    .unwrap_or_default(),
                ..Default::default()
            },
            Self::Tls(stream) => {
                let (tcp, session) = stream.get_ref();
                let tls_version = session
                    .protocol_version()
                    .map(|version| format!("{version:?}").replace("TLSv1_", "TLSv1."))
                    .unwrap_or_default();
                let tls_peer_certificate = session
                    .peer_certificates()
                    .and_then(|certificates| certificates.first())
                    .map(|certificate| certificate.as_ref().to_vec())
                    .unwrap_or_default();
                ConnectionInfo {
                    peer_ip: tcp
                        .peer_addr()
                        .map(|addr| addr.ip().to_string())
                        .unwrap_or_default(),
                    tls_version,
                    tls_peer_certificate,
                    tls_cert_valid: !session.is_handshaking(),
                }
            }
        }
    }
}

impl AsyncRead for NetStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            NetStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            NetStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for NetStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            NetStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            NetStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            NetStream::Plain(s) => Pin::new(s).poll_flush(cx),
            NetStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            NetStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            NetStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

async fn connect_socket(host: &str, port: u16, local_address: Option<&str>) -> io::Result<TcpStream> {
    let Some(local) = local_address.map(str::trim).filter(|value| !value.is_empty()) else {
        return TcpStream::connect((host, port)).await;
    };
    let local_ip = local.parse::<std::net::IpAddr>().map_err(|_| {
        invalid_input(format!("invalid local address '{local}'"))
    })?;
    let remote = lookup_host((host, port))
        .await?
        .find(|address| address.is_ipv4() == local_ip.is_ipv4())
        .ok_or_else(|| invalid_input("local address family does not match the remote host"))?;
    let socket = if local_ip.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    socket.bind(std::net::SocketAddr::new(local_ip, 0))?;
    socket.connect(remote).await
}

/// Establishes a TCP connection directly or through a SOCKS4/SOCKS5 proxy.
async fn connect_tcp(profile: &ServerProfile) -> io::Result<TcpStream> {
    let target = (profile.host.as_str(), profile.port);
    match &profile.proxy {
        Some(proxy) => {
            let bound = connect_socket(
                proxy.host.as_str(),
                proxy.port,
                profile.local_address.as_deref(),
            )
            .await?;
            match proxy.kind {
                ProxyKind::Socks4 => {
                    let stream = match proxy.username.as_deref().filter(|value| !value.is_empty()) {
                        Some(user) => Socks4Stream::connect_with_userid_and_socket(bound, target, user).await,
                        None => Socks4Stream::connect_with_socket(bound, target).await,
                    }
                    .map_err(|e| io::Error::other(format!("SOCKS4 proxy error: {e}")))?;
                    Ok(stream.into_inner())
                }
                ProxyKind::Socks5 => {
                    let stream = match (&proxy.username, &proxy.password) {
                        (Some(u), Some(p)) => Socks5Stream::connect_with_password_and_socket(bound, target, u, p).await,
                        _ => Socks5Stream::connect_with_socket(bound, target).await,
                    }
                    .map_err(|e| io::Error::other(format!("SOCKS5 proxy error: {e}")))?;
                    Ok(stream.into_inner())
                }
            }
        }
        None => connect_socket(&profile.host, profile.port, profile.local_address.as_deref()).await,
    }
}

/// Connects and, if requested, performs the TLS handshake (with a timeout).
pub async fn connect(profile: &ServerProfile) -> io::Result<NetStream> {
    let client_identity = client_identity(profile)?;
    let tcp = timeout(CONNECT_TIMEOUT, connect_tcp(profile))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))??;
    tcp.set_nodelay(true).ok();

    if !profile.tls {
        return Ok(NetStream::Plain(tcp));
    }

    let config = tls_config(profile.tls_insecure, client_identity)?;
    let connector = TlsConnector::from(Arc::new(config));
    let domain = rustls::pki_types::ServerName::try_from(profile.host.clone())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
    let tls = timeout(CONNECT_TIMEOUT, connector.connect(domain, tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    Ok(NetStream::Tls(Box::new(tls)))
}

/// Wraps an already-connected TCP stream in a (verified) TLS client connection.
/// Used by script sockets opened with `/sockopen -e`.
pub async fn tls_client(host: &str, tcp: TcpStream) -> io::Result<NetStream> {
    let connector = TlsConnector::from(Arc::new(tls_config(false, None)?));
    let domain = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
    let tls = timeout(CONNECT_TIMEOUT, connector.connect(domain, tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    Ok(NetStream::Tls(Box::new(tls)))
}

type ClientIdentity = (
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
);

/// Validates and loads the optional TLS client identity before opening the TCP
/// socket, so incomplete EXTERNAL profiles fail immediately with a clear error.
fn client_identity(profile: &ServerProfile) -> io::Result<Option<ClientIdentity>> {
    let cert = nonempty_path(profile.tls_client_cert_path.as_deref());
    let key = nonempty_path(profile.tls_client_key_path.as_deref());
    if profile.sasl && profile.sasl_mechanism == SaslMechanism::OAuthBearer && !profile.tls {
        return Err(invalid_input("SASL OAUTHBEARER requires TLS"));
    }
    if profile.sasl && profile.sasl_mechanism == SaslMechanism::External {
        if !profile.tls {
            return Err(invalid_input("SASL EXTERNAL requires TLS"));
        }
        if cert.is_none() || key.is_none() {
            return Err(invalid_input(
                "SASL EXTERNAL requires both a TLS client certificate and private-key path",
            ));
        }
    }
    let (cert, key) = match (cert, key) {
        (None, None) => return Ok(None),
        (Some(cert), Some(key)) => (cert, key),
        _ => {
            return Err(invalid_input(
                "TLS client authentication requires both certificate and private-key paths",
            ));
        }
    };
    if !profile.tls {
        return Err(invalid_input(
            "TLS client certificate paths cannot be used when TLS is disabled",
        ));
    }
    load_client_identity(&cert, &key).map(Some)
}

fn nonempty_path(value: Option<&str>) -> Option<PathBuf> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn load_client_identity(cert_path: &Path, key_path: &Path) -> io::Result<ClientIdentity> {
    let cert_file = File::open(cert_path).map_err(|error| {
        invalid_input(format!(
            "could not open TLS client certificate '{}': {error}",
            cert_path.display()
        ))
    })?;
    let certs = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            invalid_input(format!(
                "could not parse TLS client certificate '{}': {error}",
                cert_path.display()
            ))
        })?;
    if certs.is_empty() {
        return Err(invalid_input(format!(
            "TLS client certificate '{}' contains no PEM certificates",
            cert_path.display()
        )));
    }

    let key_file = File::open(key_path).map_err(|error| {
        invalid_input(format!(
            "could not open TLS client private key '{}': {error}",
            key_path.display()
        ))
    })?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|error| {
            invalid_input(format!(
                "could not parse TLS client private key '{}': {error}",
                key_path.display()
            ))
        })?
        .ok_or_else(|| {
            invalid_input(format!(
                "TLS client private key '{}' contains no unencrypted PKCS#1, PKCS#8, or SEC1 PEM key",
                key_path.display()
            ))
        })?;
    Ok((certs, key))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn tls_config(
    insecure: bool,
    client_identity: Option<ClientIdentity>,
) -> io::Result<rustls::ClientConfig> {
    let builder = if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(danger::NoVerifier))
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder().with_root_certificates(roots)
    };
    match client_identity {
        Some((certs, key)) => builder
            .with_client_auth_cert(certs, key)
            .map_err(|error| invalid_input(format!("invalid TLS client identity: {error}"))),
        None => Ok(builder.with_no_client_auth()),
    }
}

mod danger {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, Error, SignatureScheme};

    /// Accepts any certificate. Only used when the user opts into insecure TLS.
    #[derive(Debug)]
    pub struct NoVerifier;

    impl ServerCertVerifier for NoVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PKCS1_SHA256,
                SignatureScheme::RSA_PKCS1_SHA384,
                SignatureScheme::RSA_PKCS1_SHA512,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ECDSA_NISTP384_SHA384,
                SignatureScheme::RSA_PSS_SHA256,
                SignatureScheme::RSA_PSS_SHA384,
                SignatureScheme::RSA_PSS_SHA512,
                SignatureScheme::ED25519,
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> ServerProfile {
        serde_json::from_str(
            r#"{
                "name":"test","host":"irc.example.test","port":6697,
                "nick":"me","tls":true,"autojoin":[]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn external_requires_tls_and_both_identity_paths_before_connecting() {
        let mut profile = profile();
        profile.sasl = true;
        profile.sasl_mechanism = SaslMechanism::External;
        profile.tls = false;
        let error = client_identity(&profile).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("EXTERNAL requires TLS"));

        profile.tls = true;
        let error = client_identity(&profile).unwrap_err();
        assert!(error.to_string().contains("both a TLS client certificate"));
    }

    #[test]
    fn oauth_bearer_requires_tls_before_connecting() {
        let mut profile = profile();
        profile.sasl = true;
        profile.sasl_mechanism = SaslMechanism::OAuthBearer;
        profile.tls = false;
        let error = client_identity(&profile).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("OAUTHBEARER requires TLS"));
    }

    #[test]
    fn client_identity_rejects_one_sided_or_non_tls_paths() {
        let mut profile = profile();
        profile.tls_client_cert_path = Some("client.pem".into());
        let error = client_identity(&profile).unwrap_err();
        assert!(error.to_string().contains("requires both certificate"));

        profile.tls_client_key_path = Some("client.key".into());
        profile.tls = false;
        let error = client_identity(&profile).unwrap_err();
        assert!(error.to_string().contains("when TLS is disabled"));
    }

    #[test]
    fn identity_files_are_loaded_before_any_tcp_connection() {
        let mut profile = profile();
        profile.tls_client_cert_path = Some("definitely-missing-client.pem".into());
        profile.tls_client_key_path = Some("definitely-missing-client.key".into());
        let error = client_identity(&profile).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("could not open TLS client certificate"));
    }

    #[tokio::test]
    async fn direct_connection_binds_the_selected_local_address() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().1 });
        let stream = connect_socket("127.0.0.1", port, Some("127.0.0.1"))
            .await
            .unwrap();
        assert_eq!(stream.local_addr().unwrap().ip(), std::net::Ipv4Addr::LOCALHOST);
        assert_eq!(accept.await.unwrap().ip(), std::net::Ipv4Addr::LOCALHOST);
    }

    #[tokio::test]
    async fn socks4a_connect_uses_the_configured_proxy_and_user_id() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let proxy_port = listener.local_addr().unwrap().port();
        let proxy = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 256];
            let length = socket.read(&mut request).await.unwrap();
            request.truncate(length);
            assert_eq!(&request[..2], &[4, 1]);
            assert!(request.windows(5).any(|value| value == b"jirc\0"));
            assert!(request.windows(13).any(|value| value == b"example.test\0"));
            socket.write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });
        let mut profile = profile();
        profile.host = "example.test".into();
        profile.port = 6667;
        profile.tls = false;
        profile.local_address = Some("127.0.0.1".into());
        profile.proxy = Some(crate::config::Proxy {
            kind: ProxyKind::Socks4,
            host: "127.0.0.1".into(),
            port: proxy_port,
            username: Some("jirc".into()),
            password: None,
        });
        connect_tcp(&profile).await.unwrap();
        proxy.await.unwrap();
    }
}
