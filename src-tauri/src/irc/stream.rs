//! Network transport: a unified stream over plain TCP or TLS, optionally via a
//! SOCKS5 proxy.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_socks::tcp::Socks5Stream;

use crate::config::ServerProfile;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A connection stream that is either plain TCP or TLS. Both inner types are
/// `Unpin`, so delegation is straightforward.
pub enum NetStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
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

/// Establishes a TCP connection (directly or through a SOCKS5 proxy).
async fn connect_tcp(profile: &ServerProfile) -> io::Result<TcpStream> {
    let target = (profile.host.as_str(), profile.port);
    match &profile.proxy {
        Some(proxy) => {
            let proxy_addr = (proxy.host.as_str(), proxy.port);
            let stream = match (&proxy.username, &proxy.password) {
                (Some(u), Some(p)) => {
                    Socks5Stream::connect_with_password(proxy_addr, target, u, p).await
                }
                _ => Socks5Stream::connect(proxy_addr, target).await,
            }
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("proxy error: {e}")))?;
            Ok(stream.into_inner())
        }
        None => TcpStream::connect(target).await,
    }
}

/// Connects and, if requested, performs the TLS handshake (with a timeout).
pub async fn connect(profile: &ServerProfile) -> io::Result<NetStream> {
    let tcp = timeout(CONNECT_TIMEOUT, connect_tcp(profile))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))??;
    tcp.set_nodelay(true).ok();

    if !profile.tls {
        return Ok(NetStream::Plain(tcp));
    }

    let config = tls_config(profile.tls_insecure);
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
    let connector = TlsConnector::from(Arc::new(tls_config(false)));
    let domain = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
    let tls = timeout(CONNECT_TIMEOUT, connector.connect(domain, tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    Ok(NetStream::Tls(Box::new(tls)))
}

fn tls_config(insecure: bool) -> rustls::ClientConfig {
    if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(danger::NoVerifier))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
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
