//! Script-controlled TCP/UDP sockets for mSL (`/sockopen`, `/sockudp`,
//! `/socklisten`, `on SOCKREAD`, `on UDPREAD`, …).
//!
//! Each connected socket runs as an async task with mIRC-style receive/send
//! queues. Incoming bytes trigger `on SOCKREAD`, whose repeated `/sockread`
//! calls drain the shared queue; outgoing writes fire `on SOCKWRITE` when the
//! send buffer drains. Per-socket stats back `$sock(name).property`. A listening
//! socket (`/socklisten`) is bound
//! synchronously — so its port is immediately readable via `$sock(name).port` —
//! and its accept loop is started separately ([`start_listener`]) with the
//! owning connection's context, so an incoming connection fires `on SOCKLISTEN`;
//! the handler's `/sockaccept <name>` then turns the pending connection into a
//! named connected socket.
//!
//! Stored as Tauri managed state, mirroring [`crate::irc::ConnectionManager`].
//! Sockets belong to the connection that created them. TCP can be plain or
//! direct TLS (`/sockopen -e`); UDP supports mIRC's keep/read event model.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use socket2::{Domain, Protocol, Socket, Type};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpListener, TcpSocket, TcpStream, UdpSocket};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot, Notify,
};
use tokio::time::{timeout, Duration};
use tokio_rustls::TlsConnector;

use super::eval::{
    wildcard_match, EventVars, SocketReadOptions, SocketReadResult, SocketWriteResult,
};
use super::{apply_actions, script_data_dir, RunCtx, ScriptEngine};
use crate::irc::stream::NetStream;

struct SockHandle {
    /// Shared live name used by the I/O task. `/sockrename` updates this so
    /// subsequent SOCKREAD/SOCKWRITE/SOCKCLOSE events use the renamed socket.
    name: Arc<Mutex<String>>,
    /// Outgoing channel — `None` for a pure listening socket.
    outgoing: Option<UnboundedSender<TcpCommand>>,
    /// Datagram queue — present only for UDP sockets.
    udp_outgoing: Option<UnboundedSender<UdpWrite>>,
    /// Receive/send queues and pause coordination shared with the I/O task.
    io: Arc<SockIo>,
    task: tauri::async_runtime::JoinHandle<()>,
    port: u16,
    mark: String,
    listening: bool,
    udp: bool,
    udp_keep: bool,
    udp_dual_stack: bool,
    tls: bool,
    starttls: bool,
    /// Named address passed to `/sockopen` (`$sock().addr`).
    addr: String,
    /// Peer IP (`$sock().ip`).
    ip: String,
    bind_ip: String,
    bind_port: u16,
    saddr: String,
    sport: u16,
    sent: u64,
    rcvd: u64,
    opened: Instant,
    last_sent: Instant,
    last_rcvd: Instant,
    status: SockStatus,
    /// Last socket error number (`$sock().wserr`).
    wserr: i32,
    /// Last error message (`$sock().wsmsg`).
    wsmsg: String,
}

struct UdpWrite {
    destination: SocketAddr,
    data: Vec<u8>,
    close_after: bool,
}

struct PendingAccept {
    stream: Option<TcpStream>,
    app: AppHandle,
    server_id: String,
    network: String,
    nick: String,
    peer_ip: String,
    peer_port: u16,
    bind_ip: String,
    bind_port: u16,
    listener_nodelay: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReservedKind {
    Tcp,
    Udp,
}

struct ReservedSocket {
    name: String,
    kind: ReservedKind,
    addr: String,
    port: u16,
    bind_ip: String,
    bind_port: u16,
    tls: bool,
    mark: String,
    paused: bool,
}

impl ReservedSocket {
    fn prop(&self, property: &str) -> String {
        match property.to_ascii_lowercase().as_str() {
            "name" => self.name.clone(),
            "addr" => self.addr.clone(),
            "ip" => {
                if self.kind == ReservedKind::Udp {
                    self.addr.clone()
                } else {
                    String::new()
                }
            }
            "port" => self.port.to_string(),
            "status" => (if self.kind == ReservedKind::Udp {
                "active"
            } else {
                "connecting"
            })
            .to_string(),
            "type" => (if self.kind == ReservedKind::Udp {
                "UDP"
            } else {
                "TCP"
            })
            .to_string(),
            "mark" => self.mark.clone(),
            "pause" => bool_id(self.paused),
            "ssl" => bool_id(self.tls),
            "starttls" => bool_id(false),
            "bindip" => self.bind_ip.clone(),
            "bindport" => self.bind_port.to_string(),
            "sent" | "rcvd" | "sq" | "rq" | "ls" | "lr" | "to" | "wserr" => "0".to_string(),
            "wsmsg" | "saddr" | "sport" => String::new(),
            "upnp" => bool_id(false),
            _ => String::new(),
        }
    }
}

struct BoundListener {
    id: u64,
    listener: std::net::TcpListener,
    port: u16,
    bind_ip: String,
    nodelay: bool,
    mark: String,
}

enum TcpCommand {
    Data(Vec<u8>),
    StartTls,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SockStatus {
    Connecting,
    Active,
    Listening,
}

struct SockIo {
    recv: Mutex<VecDeque<u8>>,
    send_queued: AtomicUsize,
    /// Unlike TCP, a retained UDP socket may be enabled by a later `/sockudp
    /// -k` that reuses the same name. Keep this live instead of capturing the
    /// switch used by the command that originally created the task.
    udp_read_enabled: AtomicBool,
    paused: AtomicBool,
    pause_notify: Notify,
    read_attempts: AtomicU64,
    last_read_binary: AtomicBool,
}

impl Default for SockIo {
    fn default() -> Self {
        Self {
            recv: Mutex::new(VecDeque::new()),
            send_queued: AtomicUsize::new(0),
            udp_read_enabled: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            pause_notify: Notify::new(),
            read_attempts: AtomicU64::new(0),
            last_read_binary: AtomicBool::new(false),
        }
    }
}

impl SockHandle {
    fn new(
        name: Arc<Mutex<String>>,
        outgoing: Option<UnboundedSender<TcpCommand>>,
        io: Arc<SockIo>,
        task: tauri::async_runtime::JoinHandle<()>,
        port: u16,
        listening: bool,
    ) -> Self {
        let now = Instant::now();
        SockHandle {
            name,
            outgoing,
            udp_outgoing: None,
            io,
            task,
            port,
            mark: String::new(),
            listening,
            udp: false,
            udp_keep: false,
            udp_dual_stack: false,
            tls: false,
            starttls: false,
            addr: String::new(),
            ip: String::new(),
            bind_ip: String::new(),
            bind_port: 0,
            saddr: String::new(),
            sport: 0,
            sent: 0,
            rcvd: 0,
            opened: now,
            last_sent: now,
            last_rcvd: now,
            status: if listening {
                SockStatus::Listening
            } else {
                SockStatus::Connecting
            },
            wserr: 0,
            wsmsg: String::new(),
        }
    }

    /// Resolves a `$sock(name).property`.
    fn prop(&self, name: &str, property: &str) -> String {
        let secs = |i: Instant| i.elapsed().as_secs().to_string();
        match property.to_ascii_lowercase().as_str() {
            "name" => name.to_string(),
            "port" => self.port.to_string(),
            "ip" => self.ip.clone(),
            "addr" => self.addr.clone(),
            "mark" => self.mark.clone(),
            "status" => match self.status {
                SockStatus::Connecting => "connecting",
                SockStatus::Active => "active",
                SockStatus::Listening => "listening",
            }
            .to_string(),
            "type" => if self.udp { "UDP" } else { "TCP" }.to_string(),
            "ssl" => bool_id(self.tls),
            "starttls" => bool_id(self.starttls),
            "pause" => bool_id(self.io.paused.load(Ordering::SeqCst)),
            "sent" => self.sent.to_string(),
            "rcvd" => self.rcvd.to_string(),
            "sq" => self.io.send_queued.load(Ordering::SeqCst).to_string(),
            "rq" => self.io.recv.lock().unwrap().len().to_string(),
            "ls" => secs(self.last_sent),
            "lr" => secs(self.last_rcvd),
            "to" => secs(self.opened),
            "wserr" => self.wserr.to_string(),
            "wsmsg" => self.wsmsg.clone(),
            "saddr" => self.saddr.clone(),
            "sport" => {
                if self.sport == 0 {
                    String::new()
                } else {
                    self.sport.to_string()
                }
            }
            "bindip" => self.bind_ip.clone(),
            "bindport" => self.bind_port.to_string(),
            // jIRC does not currently ask the router to create UPnP mappings,
            // but mIRC exposes this property on every socket.
            "upnp" => bool_id(false),
            _ => String::new(),
        }
    }
}

fn bool_id(b: bool) -> String {
    if b { "$true" } else { "$false" }.to_string()
}

const SEND_QUEUE_LIMIT: usize = 16_384;
const WSA_ACCESS_DENIED: i32 = 10_013;
const WSA_INVALID_ARGUMENT: i32 = 10_022;
const WSA_WOULD_BLOCK: i32 = 10_035;
const WSA_NOT_A_SOCKET: i32 = 10_038;
const WSA_ADDRESS_IN_USE: i32 = 10_048;
const WSA_ADDRESS_NOT_AVAILABLE: i32 = 10_049;
const WSA_NETWORK_DOWN: i32 = 10_050;
const WSA_NETWORK_UNREACHABLE: i32 = 10_051;
const WSA_CONNECTION_ABORTED: i32 = 10_053;
const WSA_CONNECTION_RESET: i32 = 10_054;
const WSA_NO_BUFFER_SPACE: i32 = 10_055;
const WSA_NOT_CONNECTED: i32 = 10_057;
const WSA_TIMED_OUT: i32 = 10_060;
const WSA_CONNECTION_REFUSED: i32 = 10_061;
const WSA_HOST_UNREACHABLE: i32 = 10_065;

fn subtract_queued(value: &AtomicUsize, amount: usize) {
    let _ = value.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |queued| {
        Some(queued.saturating_sub(amount))
    });
}

fn clear_error(handle: &mut SockHandle) {
    handle.wserr = 0;
    handle.wsmsg.clear();
}

fn queue_write(handle: &mut SockHandle, data: Vec<u8>) -> i32 {
    let Some(tx) = handle.outgoing.clone() else {
        handle.wserr = WSA_NOT_A_SOCKET;
        handle.wsmsg = "socket is not connected".to_string();
        return handle.wserr;
    };
    let len = data.len();
    if handle
        .io
        .send_queued
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |queued| {
            queued
                .checked_add(len)
                .filter(|total| *total <= SEND_QUEUE_LIMIT)
        })
        .is_err()
    {
        handle.wserr = WSA_NO_BUFFER_SPACE;
        handle.wsmsg = format!("socket send queue exceeds {SEND_QUEUE_LIMIT} bytes");
        return handle.wserr;
    }
    if tx.send(TcpCommand::Data(data)).is_err() {
        subtract_queued(&handle.io.send_queued, len);
        handle.wserr = WSA_CONNECTION_RESET;
        handle.wsmsg = "socket send queue is closed".to_string();
        return handle.wserr;
    }
    clear_error(handle);
    0
}

fn queue_udp(handle: &mut SockHandle, out: UdpWrite) -> i32 {
    let Some(tx) = handle.udp_outgoing.clone() else {
        handle.wserr = WSA_NOT_A_SOCKET;
        handle.wsmsg = "socket is not a UDP socket".to_string();
        return handle.wserr;
    };
    let len = out.data.len();
    if handle
        .io
        .send_queued
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |queued| {
            queued
                .checked_add(len)
                .filter(|total| *total <= SEND_QUEUE_LIMIT)
        })
        .is_err()
    {
        handle.wserr = WSA_NO_BUFFER_SPACE;
        handle.wsmsg = format!("socket send queue exceeds {SEND_QUEUE_LIMIT} bytes");
        return handle.wserr;
    }
    if tx.send(out).is_err() {
        subtract_queued(&handle.io.send_queued, len);
        handle.wserr = WSA_CONNECTION_RESET;
        handle.wsmsg = "socket send queue is closed".to_string();
        return handle.wserr;
    }
    clear_error(handle);
    0
}

fn read_queued(io: &SockIo, options: SocketReadOptions) -> SocketReadResult {
    io.read_attempts.fetch_add(1, Ordering::SeqCst);
    io.last_read_binary.store(options.binary, Ordering::SeqCst);
    let mut queue = io.recv.lock().unwrap();

    if options.line {
        let newline = queue.iter().position(|byte| *byte == b'\n');
        let forced_text_nul = if options.force && !options.binary {
            queue.iter().position(|byte| *byte == 0)
        } else {
            None
        };
        if let Some(nul) = forced_text_nul.filter(|nul| newline.map_or(true, |lf| *nul < lf)) {
            let consumed = nul + 1;
            let mut data: Vec<u8> = queue.drain(..consumed).collect();
            data.pop(); // NUL cannot be represented in a mIRC text variable.
            return SocketReadResult {
                data,
                bytes_read: consumed,
            };
        }
        if let Some(end) = newline {
            let consumed = end + 1;
            let mut data: Vec<u8> = queue.drain(..consumed).collect();
            data.pop(); // LF
            if data.last() == Some(&b'\r') {
                data.pop();
            }
            return SocketReadResult {
                data,
                bytes_read: consumed,
            };
        }
        if !options.force || queue.is_empty() {
            return SocketReadResult::default();
        }

        // `-f` on a text destination stops at the first NUL. Consume the NUL as
        // well so a malformed/binary stream cannot leave a permanent zero-byte
        // read at the front of the queue.
        let consumed = if !options.binary {
            queue
                .iter()
                .position(|byte| *byte == 0)
                .map(|i| i + 1)
                .unwrap_or(queue.len())
        } else {
            queue.len()
        };
        let mut data: Vec<u8> = queue.drain(..consumed).collect();
        if !options.binary && data.last() == Some(&0) {
            data.pop();
        }
        return SocketReadResult {
            data,
            bytes_read: consumed,
        };
    }

    let consumed = queue.len().min(options.max_bytes);
    let data = queue.drain(..consumed).collect();
    SocketReadResult {
        data,
        bytes_read: consumed,
    }
}

#[derive(Default)]
pub struct SocketManager {
    /// Serialises operations that create, move, or consume socket names. Event
    /// dispatch is never performed while this lock is held because handlers can
    /// re-enter the socket manager.
    name_ops: Mutex<()>,
    next_reservation_id: AtomicU64,
    reservations: Mutex<HashMap<u64, ReservedSocket>>,
    /// One stable live-handle target per deferred `/sockudp` reuse. The weak
    /// identity follows `/sockrename`; failure to resolve it means `/sockclose`
    /// cancelled that action and must not recreate the old name.
    deferred_udp_targets: Mutex<HashMap<String, VecDeque<Weak<Mutex<String>>>>>,
    socks: Mutex<HashMap<String, SockHandle>>,
    /// Listeners bound by `/socklisten`, awaiting their accept loop to start.
    bound: Mutex<HashMap<String, BoundListener>>,
    /// Connections awaiting `/sockaccept`. An entry exists only while that
    /// listener's `on SOCKLISTEN` handler is running; accepting consumes its
    /// stream and registers the new socket immediately for later handler lines.
    accept_names: Mutex<HashMap<String, PendingAccept>>,
}

fn matching_key<T>(map: &HashMap<String, T>, name: &str) -> Option<String> {
    map.keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
}

fn current_name(name: &Arc<Mutex<String>>) -> String {
    name.lock().unwrap().clone()
}

fn identity_key(socks: &HashMap<String, SockHandle>, name: &Arc<Mutex<String>>) -> Option<String> {
    socks
        .iter()
        .find(|(_, handle)| Arc::ptr_eq(&handle.name, name))
        .map(|(key, _)| key.clone())
}

fn mapped_socket_addr(address: SocketAddr, dual_stack: bool) -> SocketAddr {
    if !dual_stack {
        return address;
    }
    match address {
        SocketAddr::V4(address) => {
            SocketAddr::new(IpAddr::V6(address.ip().to_ipv6_mapped()), address.port())
        }
        SocketAddr::V6(_) => address,
    }
}

fn display_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        IpAddr::V4(_) => ip,
    }
}

fn dual_stack_bind_ip(bind_ip: &str) -> io::Result<Ipv6Addr> {
    if bind_ip.is_empty() {
        return Ok(Ipv6Addr::UNSPECIFIED);
    }
    match bind_ip.parse::<IpAddr>() {
        Ok(IpAddr::V6(ip)) => Ok(ip),
        Ok(IpAddr::V4(ip)) if ip.is_unspecified() => Ok(Ipv6Addr::UNSPECIFIED),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a dual-stack socket must bind to an IPv6 address",
        )),
    }
}

fn bind_tcp_listener(
    bind_ip: &str,
    port: u16,
    dual_stack: bool,
) -> io::Result<std::net::TcpListener> {
    if !dual_stack {
        let address = if bind_ip.is_empty() {
            "0.0.0.0"
        } else {
            bind_ip
        };
        return std::net::TcpListener::bind((address, port));
    }

    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(false)?;
    let address = SocketAddr::new(IpAddr::V6(dual_stack_bind_ip(bind_ip)?), port);
    socket.bind(&address.into())?;
    socket.listen(128)?;
    Ok(socket.into())
}

fn bind_udp_socket(
    bind_ip: &str,
    port: u16,
    destination: IpAddr,
    dual_stack: bool,
) -> io::Result<std::net::UdpSocket> {
    if !dual_stack {
        let address = if bind_ip.is_empty() {
            if destination.is_ipv4() {
                "0.0.0.0"
            } else {
                "::"
            }
        } else {
            bind_ip
        };
        return std::net::UdpSocket::bind((address, port));
    }

    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(false)?;
    let address = SocketAddr::new(IpAddr::V6(dual_stack_bind_ip(bind_ip)?), port);
    socket.bind(&address.into())?;
    Ok(socket.into())
}

async fn connect_tcp(
    host: &str,
    port: u16,
    bind_ip: &str,
    ip_version: u8,
) -> std::io::Result<TcpStream> {
    if bind_ip.is_empty() && ip_version == 0 {
        return TcpStream::connect((host, port)).await;
    }
    let bind = if bind_ip.is_empty() {
        None
    } else {
        Some(bind_ip.parse::<IpAddr>().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid socket bind address",
            )
        })?)
    };
    let ipv4 = bind.map(|ip| ip.is_ipv4()).unwrap_or(ip_version != 6);
    if (ip_version == 4 && !ipv4) || (ip_version == 6 && ipv4) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket bind address conflicts with the requested address family",
        ));
    }
    let socket = if ipv4 {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    if let Some(ip) = bind {
        socket.bind(SocketAddr::new(ip, 0))?;
    }
    let destination = lookup_host((host, port))
        .await?
        .find(|addr| addr.is_ipv4() == ipv4)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no destination address matches the bound address family",
            )
        })?;
    socket.connect(destination).await
}

async fn upgrade_starttls_stream(host: &str, stream: NetStream) -> std::io::Result<NetStream> {
    match stream {
        NetStream::Plain(tcp) => crate::irc::stream::tls_client(host, tcp).await,
        NetStream::Tls(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "socket is already using TLS",
        )),
    }
}

async fn tls_socket_client(
    host: &str,
    tcp: TcpStream,
    accept_invalid: bool,
) -> io::Result<NetStream> {
    if !accept_invalid {
        return crate::irc::stream::tls_client(host, tcp).await;
    }
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(insecure_tls::NoVerifier))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let domain = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
    let tls = timeout(Duration::from_secs(20), connector.connect(domain, tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    Ok(NetStream::Tls(Box::new(tls)))
}

mod insecure_tls {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, Error, SignatureScheme};

    /// mIRC's `/sockopen -ea`/`-es` opt-in: accept an invalid certificate for
    /// this script socket only. Verified TLS remains the default.
    #[derive(Debug)]
    pub(super) struct NoVerifier;

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

impl SocketManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn name_in_use(&self, name: &str) -> bool {
        if matching_key(&self.socks.lock().unwrap(), name).is_some()
            || matching_key(&self.bound.lock().unwrap(), name).is_some()
        {
            return true;
        }
        self.reservations
            .lock()
            .unwrap()
            .values()
            .any(|reserved| reserved.name.eq_ignore_ascii_case(name))
    }

    fn next_id(&self) -> u64 {
        self.next_reservation_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn reservation_id_by_name(&self, name: &str) -> Option<u64> {
        self.reservations
            .lock()
            .unwrap()
            .iter()
            .find(|(_, reserved)| reserved.name.eq_ignore_ascii_case(name))
            .map(|(id, _)| *id)
    }

    fn take_deferred_udp_target(&self, name: &str) -> Option<Weak<Mutex<String>>> {
        let mut targets = self.deferred_udp_targets.lock().unwrap();
        let key = matching_key(&targets, name)?;
        let target = targets.get_mut(&key)?.pop_front();
        if targets.get(&key).is_some_and(VecDeque::is_empty) {
            targets.remove(&key);
        }
        target
    }

    /// Reserves a TCP socket name synchronously so `$sock()` and state commands
    /// observe it before the deferred connect action receives its IRC context.
    pub fn reserve_open(
        &self,
        name: &str,
        host: &str,
        port: u16,
        tls: bool,
        bind_ip: &str,
    ) -> Result<u64, i32> {
        if name.is_empty() || host.is_empty() || port == 0 {
            return Err(WSA_INVALID_ARGUMENT);
        }
        let _name_guard = self.name_ops.lock().unwrap();
        if self.name_in_use(name) {
            return Err(WSA_ADDRESS_IN_USE);
        }
        let id = self.next_id();
        self.reservations.lock().unwrap().insert(
            id,
            ReservedSocket {
                name: name.to_string(),
                kind: ReservedKind::Tcp,
                addr: host.to_string(),
                port,
                bind_ip: bind_ip.to_string(),
                bind_port: 0,
                tls,
                mark: String::new(),
                paused: false,
            },
        );
        Ok(id)
    }

    /// Reserves a newly-created UDP socket. Reusing either a live UDP socket or
    /// an earlier UDP reservation needs no new ID because actions apply in order.
    pub fn reserve_udp(
        &self,
        name: &str,
        bind_ip: &str,
        local_port: u16,
        dest_ip: &str,
        dest_port: u16,
    ) -> Result<u64, i32> {
        if name.is_empty() || dest_ip.parse::<IpAddr>().is_err() || dest_port == 0 {
            return Err(WSA_INVALID_ARGUMENT);
        }
        let _name_guard = self.name_ops.lock().unwrap();
        let existing = {
            let socks = self.socks.lock().unwrap();
            matching_key(&socks, name).and_then(|key| {
                socks
                    .get(&key)
                    .map(|handle| (handle.udp, Arc::downgrade(&handle.name)))
            })
        };
        if let Some((udp, target)) = existing {
            if !udp {
                return Err(WSA_ADDRESS_IN_USE);
            }
            let mut targets = self.deferred_udp_targets.lock().unwrap();
            if let Some(key) = matching_key(&targets, name) {
                targets.get_mut(&key).unwrap().push_back(target);
            } else {
                targets.insert(name.to_string(), VecDeque::from([target]));
            }
            return Ok(0);
        }
        if matching_key(&self.bound.lock().unwrap(), name).is_some() {
            return Err(WSA_ADDRESS_IN_USE);
        }
        if let Some(id) = self.reservation_id_by_name(name) {
            let mut reservations = self.reservations.lock().unwrap();
            let reserved = reservations.get_mut(&id).unwrap();
            if reserved.kind != ReservedKind::Udp {
                return Err(WSA_ADDRESS_IN_USE);
            }
            reserved.addr = dest_ip.to_string();
            reserved.port = dest_port;
            if !bind_ip.is_empty() {
                reserved.bind_ip = bind_ip.to_string();
            }
            if local_port != 0 {
                reserved.bind_port = local_port;
            }
            return Ok(0);
        }
        let id = self.next_id();
        self.reservations.lock().unwrap().insert(
            id,
            ReservedSocket {
                name: name.to_string(),
                kind: ReservedKind::Udp,
                addr: dest_ip.to_string(),
                port: dest_port,
                bind_ip: bind_ip.to_string(),
                bind_port: local_port,
                tls: false,
                mark: String::new(),
                paused: false,
            },
        );
        Ok(id)
    }

    /// Opens a TCP socket named `name` to `host:port`. mIRC treats names as
    /// unique and leaves an existing socket intact when a duplicate is opened.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        app: AppHandle,
        server_id: String,
        network: String,
        nick: String,
        name: String,
        host: String,
        port: u16,
        tls: bool,
        accept_invalid: bool,
        bind_ip: String,
        nodelay: bool,
        ip_version: u8,
        reservation_id: u64,
    ) {
        let name_guard = self.name_ops.lock().unwrap();
        let reservation = if reservation_id == 0 {
            None
        } else {
            self.reservations.lock().unwrap().remove(&reservation_id)
        };
        if reservation_id != 0 && reservation.is_none() {
            return;
        }
        if reservation
            .as_ref()
            .is_some_and(|reserved| reserved.kind != ReservedKind::Tcp)
        {
            return;
        }
        let actual_name = reservation
            .as_ref()
            .map(|reserved| reserved.name.clone())
            .unwrap_or(name);
        if self.name_in_use(&actual_name) {
            drop(name_guard);
            fire_with_error(
                &app,
                &server_id,
                &network,
                &nick,
                "SOCKOPEN",
                &actual_name,
                "",
                WSA_ADDRESS_IN_USE,
            );
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel::<TcpCommand>();
        let key = actual_name.clone();
        let live_name = Arc::new(Mutex::new(actual_name));
        let io = Arc::new(SockIo::default());
        let task_io = io.clone();
        let task_name = live_name.clone();
        let host_for_task = host.clone();
        let bind_for_task = bind_ip.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let tcp = match connect_tcp(&host_for_task, port, &bind_for_task, ip_version).await {
                Ok(s) => s,
                Err(e) => {
                    let code = io_error_code(&e);
                    set_error(&app, &task_name, code, &e.to_string());
                    let name = current_name(&task_name);
                    fire_with_error(
                        &app, &server_id, &network, &nick, "SOCKOPEN", &name, "", code,
                    );
                    forget(&app, &task_name);
                    return;
                }
            };
            if let Err(e) = tcp.set_nodelay(nodelay) {
                let code = io_error_code(&e);
                set_error(&app, &task_name, code, &e.to_string());
                let name = current_name(&task_name);
                fire_with_error(
                    &app, &server_id, &network, &nick, "SOCKOPEN", &name, "", code,
                );
                forget(&app, &task_name);
                return;
            }
            let peer = tcp
                .peer_addr()
                .map(|a| a.ip().to_string())
                .unwrap_or_default();
            set_ip(&app, &task_name, &peer);
            if let Ok(local) = tcp.local_addr() {
                set_binding(&app, &task_name, &local.ip().to_string(), local.port());
            }
            let stream = if tls {
                match tls_socket_client(&host_for_task, tcp, accept_invalid).await {
                    Ok(s) => s,
                    Err(e) => {
                        let code = io_error_code(&e);
                        set_error(&app, &task_name, code, &e.to_string());
                        let name = current_name(&task_name);
                        fire_with_error(
                            &app, &server_id, &network, &nick, "SOCKOPEN", &name, "", code,
                        );
                        forget(&app, &task_name);
                        return;
                    }
                }
            } else {
                NetStream::Plain(tcp)
            };
            set_active(&app, &task_name);
            let name = current_name(&task_name);
            fire(&app, &server_id, &network, &nick, "SOCKOPEN", &name, "");
            run_connected(
                app,
                server_id,
                network,
                nick,
                task_name,
                task_io,
                stream,
                rx,
                host_for_task,
            )
            .await;
        });
        let mut h = SockHandle::new(live_name, Some(tx), io, task, port, false);
        h.addr = host;
        h.tls = tls;
        h.bind_ip = bind_ip;
        if let Some(reserved) = reservation {
            h.mark = reserved.mark;
            h.io.paused.store(reserved.paused, Ordering::SeqCst);
        }
        self.socks.lock().unwrap().insert(key, h);
        drop(name_guard);
        let _ = start_tx.send(());
    }

    /// Sends a UDP datagram. With `keep`, the socket remains open and incoming
    /// datagrams trigger `on UDPREAD`; otherwise it is removed after the send.
    #[allow(clippy::too_many_arguments)]
    pub fn udp(
        &self,
        app: AppHandle,
        server_id: String,
        network: String,
        nick: String,
        name: String,
        bind_ip: String,
        local_port: u16,
        dest_ip: String,
        dest_port: u16,
        data: Vec<u8>,
        keep: bool,
        dual_stack: bool,
        reservation_id: u64,
    ) {
        let name_guard = self.name_ops.lock().unwrap();
        let reservation = if reservation_id == 0 {
            None
        } else {
            self.reservations.lock().unwrap().remove(&reservation_id)
        };
        if reservation_id != 0 && reservation.is_none() {
            return;
        }
        if reservation
            .as_ref()
            .is_some_and(|reserved| reserved.kind != ReservedKind::Udp)
        {
            return;
        }
        let mut name = reservation
            .as_ref()
            .map(|reserved| reserved.name.clone())
            .unwrap_or(name);
        if reservation_id == 0 {
            if let Some(target) = self.take_deferred_udp_target(&name) {
                let Some(identity) = target.upgrade() else {
                    return;
                };
                let current_key = {
                    let socks = self.socks.lock().unwrap();
                    identity_key(&socks, &identity)
                };
                let Some(current_key) = current_key else {
                    return;
                };
                name = current_key;
            }
        }
        let Ok(destination_ip) = dest_ip.parse::<IpAddr>() else {
            drop(name_guard);
            fire_with_error(
                &app,
                &server_id,
                &network,
                &nick,
                "SOCKWRITE",
                &name,
                "",
                WSA_INVALID_ARGUMENT,
            );
            return;
        };
        let destination = SocketAddr::new(destination_ip, dest_port);

        {
            let mut socks = self.socks.lock().unwrap();
            if let Some(key) = matching_key(&socks, &name) {
                let h = socks.get_mut(&key).unwrap();
                if h.udp {
                    h.addr = dest_ip.clone();
                    h.ip = dest_ip;
                    h.port = dest_port;
                    h.udp_keep |= keep;
                    if keep {
                        h.io.udp_read_enabled.store(true, Ordering::SeqCst);
                    }
                    let close_after = !h.udp_keep;
                    let destination = mapped_socket_addr(destination, h.udp_dual_stack);
                    let error = queue_udp(
                        h,
                        UdpWrite {
                            destination,
                            data,
                            close_after,
                        },
                    );
                    drop(socks);
                    drop(name_guard);
                    if error != 0 {
                        fire_with_error(
                            &app,
                            &server_id,
                            &network,
                            &nick,
                            "SOCKWRITE",
                            &name,
                            "",
                            error,
                        );
                    }
                    return;
                }
                drop(socks);
                drop(name_guard);
                fire_with_error(
                    &app,
                    &server_id,
                    &network,
                    &nick,
                    "SOCKWRITE",
                    &name,
                    "",
                    WSA_ADDRESS_IN_USE,
                );
                return;
            }
        }
        if self.name_in_use(&name) {
            drop(name_guard);
            fire_with_error(
                &app,
                &server_id,
                &network,
                &nick,
                "SOCKWRITE",
                &name,
                "",
                WSA_ADDRESS_IN_USE,
            );
            return;
        }

        let std_socket = match bind_udp_socket(&bind_ip, local_port, destination_ip, dual_stack) {
            Ok(socket) => socket,
            Err(error) => {
                drop(name_guard);
                fire_with_error(
                    &app,
                    &server_id,
                    &network,
                    &nick,
                    "SOCKWRITE",
                    &name,
                    "",
                    io_error_code(&error),
                );
                return;
            }
        };
        let local = match std_socket.local_addr() {
            Ok(local) => local,
            Err(error) => {
                drop(name_guard);
                fire_with_error(
                    &app,
                    &server_id,
                    &network,
                    &nick,
                    "SOCKWRITE",
                    &name,
                    "",
                    io_error_code(&error),
                );
                return;
            }
        };
        if let Err(error) = std_socket.set_nonblocking(true) {
            drop(name_guard);
            fire_with_error(
                &app,
                &server_id,
                &network,
                &nick,
                "SOCKWRITE",
                &name,
                "",
                io_error_code(&error),
            );
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<UdpWrite>();
        let key = name.clone();
        let live_name = Arc::new(Mutex::new(name));
        let io = Arc::new(SockIo::default());
        let task_name = live_name.clone();
        let task_io = io.clone();
        task_io.udp_read_enabled.store(keep, Ordering::SeqCst);
        let error_app = app.clone();
        let error_server_id = server_id.clone();
        let error_network = network.clone();
        let error_nick = nick.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let socket = match UdpSocket::from_std(std_socket) {
                Ok(socket) => socket,
                Err(error) => {
                    let code = io_error_code(&error);
                    set_error(&app, &task_name, code, &error.to_string());
                    let socket_name = current_name(&task_name);
                    fire_with_error(
                        &app,
                        &server_id,
                        &network,
                        &nick,
                        "SOCKWRITE",
                        &socket_name,
                        "",
                        code,
                    );
                    forget(&app, &task_name);
                    return;
                }
            };
            let mut buf = [0u8; 65_507];
            loop {
                tokio::select! {
                    _ = task_io.pause_notify.notified() => {},
                    out = rx.recv() => match out {
                        Some(out) => {
                            let len = out.data.len();
                            match socket.send_to(&out.data, out.destination).await {
                                Ok(sent) => {
                                    subtract_queued(&task_io.send_queued, len);
                                    bump(&app, &task_name, sent as u64, 0);
                                    set_error(&app, &task_name, 0, "");
                                    let socket_name = current_name(&task_name);
                                    fire(&app, &server_id, &network, &nick, "SOCKWRITE", &socket_name, "");
                                }
                                Err(error) => {
                                    subtract_queued(&task_io.send_queued, len);
                                    let code = io_error_code(&error);
                                    set_error(&app, &task_name, code, &error.to_string());
                                    let socket_name = current_name(&task_name);
                                    fire_with_error(&app, &server_id, &network, &nick, "SOCKWRITE", &socket_name, "", code);
                                }
                            }
                            if out.close_after {
                                forget(&app, &task_name);
                                return;
                            }
                        }
                        None => return,
                    },
                    received = socket.recv_from(&mut buf), if task_io.udp_read_enabled.load(Ordering::SeqCst) && !task_io.paused.load(Ordering::SeqCst) => {
                        match received {
                            Ok((n, source)) => {
                                bump(&app, &task_name, 0, n as u64);
                                set_udp_source(&app, &task_name, source);
                                set_error(&app, &task_name, 0, "");
                                task_io.recv.lock().unwrap().extend(&buf[..n]);
                                deliver_queued(&app, &server_id, &network, &nick, &task_name, &task_io, "UDPREAD");
                            }
                            Err(error) => {
                                let code = io_error_code(&error);
                                set_error(&app, &task_name, code, &error.to_string());
                                let socket_name = current_name(&task_name);
                                fire_with_error(&app, &server_id, &network, &nick, "UDPREAD", &socket_name, "", code);
                            }
                        }
                    },
                }
            }
        });
        let mut h = SockHandle::new(live_name, None, io, task, dest_port, false);
        h.udp = true;
        h.udp_keep = keep;
        h.udp_dual_stack = dual_stack;
        h.udp_outgoing = Some(tx.clone());
        h.status = SockStatus::Active;
        h.addr = dest_ip.clone();
        h.ip = dest_ip;
        h.bind_ip = local.ip().to_string();
        h.bind_port = local.port();
        if let Some(reserved) = reservation {
            h.mark = reserved.mark;
            h.io.paused.store(reserved.paused, Ordering::SeqCst);
        }
        let queue_key = key.clone();
        self.socks.lock().unwrap().insert(key, h);
        let initial_error = if let Some(h) = self.socks.lock().unwrap().get_mut(&queue_key) {
            queue_udp(
                h,
                UdpWrite {
                    destination: mapped_socket_addr(destination, dual_stack),
                    data,
                    close_after: !keep,
                },
            )
        } else {
            WSA_NOT_A_SOCKET
        };
        drop(name_guard);
        let _ = start_tx.send(());
        if initial_error != 0 {
            fire_with_error(
                &error_app,
                &error_server_id,
                &error_network,
                &error_nick,
                "SOCKWRITE",
                &queue_key,
                "",
                initial_error,
            );
        }
    }

    /// Binds a listening socket synchronously (so `$sock(name).port` is readable
    /// on the same line, like mIRC). `port == 0` lets the OS assign one.
    pub fn listen(&self, bind_ip: &str, name: &str, port: u16) -> Option<u16> {
        self.listen_with_options(bind_ip, name, port, false, false)
            .ok()
    }

    /// Variant used by the evaluator so bind errors can update `$sockerr` and
    /// the `-n`/`-u` switches are not discarded.
    pub fn listen_with_options(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Result<u16, i32> {
        self.listen_reserved(bind_ip, name, port, nodelay, dual_stack)
            .map(|(port, _)| port)
    }

    pub fn listen_reserved(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Result<(u16, u64), i32> {
        let _name_guard = self.name_ops.lock().unwrap();
        if name.is_empty() {
            return Err(WSA_INVALID_ARGUMENT);
        }
        if self.name_in_use(name) {
            return Err(WSA_ADDRESS_IN_USE);
        }
        let listener =
            bind_tcp_listener(bind_ip, port, dual_stack).map_err(|error| io_error_code(&error))?;
        let local = listener
            .local_addr()
            .map_err(|error| io_error_code(&error))?;
        let bound_port = local.port();
        listener
            .set_nonblocking(true)
            .map_err(|error| io_error_code(&error))?;
        let id = self.next_id();
        self.bound.lock().unwrap().insert(
            name.to_string(),
            BoundListener {
                id,
                listener,
                port: bound_port,
                bind_ip: local.ip().to_string(),
                nodelay,
                mark: String::new(),
            },
        );
        Ok((bound_port, id))
    }

    /// Starts the accept loop for a listener bound by [`listen`], with the owning
    /// connection's context. Called from apply-time.
    pub fn start_listener(
        &self,
        app: AppHandle,
        server_id: String,
        network: String,
        nick: String,
        name: String,
        listener_id: u64,
    ) {
        let name_guard = self.name_ops.lock().unwrap();
        let bound = {
            let mut bound = self.bound.lock().unwrap();
            let key = if listener_id == 0 {
                matching_key(&bound, &name)
            } else {
                bound
                    .iter()
                    .find(|(_, listener)| listener.id == listener_id)
                    .map(|(key, _)| key.clone())
            };
            key.and_then(|key| bound.remove(&key).map(|listener| (key, listener)))
        };
        let Some((visible_name, bound)) = bound else {
            return;
        };
        let BoundListener {
            listener: std_listener,
            port,
            bind_ip,
            nodelay: listener_nodelay,
            mark,
            ..
        } = bound;
        if matching_key(&self.socks.lock().unwrap(), &visible_name).is_some() {
            drop(name_guard);
            fire_with_error(
                &app,
                &server_id,
                &network,
                &nick,
                "SOCKLISTEN",
                &visible_name,
                "",
                WSA_ADDRESS_IN_USE,
            );
            return;
        }
        let key = visible_name.clone();
        let live_name = Arc::new(Mutex::new(visible_name));
        let io = Arc::new(SockIo::default());
        let task_name = live_name.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let listener = match TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    let code = io_error_code(&e);
                    set_error(&app, &task_name, code, &e.to_string());
                    let name = current_name(&task_name);
                    fire_with_error(
                        &app,
                        &server_id,
                        &network,
                        &nick,
                        "SOCKLISTEN",
                        &name,
                        "",
                        code,
                    );
                    forget(&app, &task_name);
                    return;
                }
            };
            loop {
                let (stream, _addr) = match listener.accept().await {
                    Ok(s) => s,
                    Err(e) => {
                        let code = io_error_code(&e);
                        set_error(&app, &task_name, code, &e.to_string());
                        let name = current_name(&task_name);
                        fire_with_error(
                            &app,
                            &server_id,
                            &network,
                            &nick,
                            "SOCKLISTEN",
                            &name,
                            "",
                            code,
                        );
                        forget(&app, &task_name);
                        return;
                    }
                };
                if let Err(error) = stream.set_nodelay(listener_nodelay) {
                    let code = io_error_code(&error);
                    set_error(&app, &task_name, code, &error.to_string());
                    let listener_name = current_name(&task_name);
                    fire_with_error(
                        &app,
                        &server_id,
                        &network,
                        &nick,
                        "SOCKLISTEN",
                        &listener_name,
                        "",
                        code,
                    );
                    continue;
                }
                let listener_name = current_name(&task_name);
                let peer = stream.peer_addr().ok();
                let local = stream.local_addr().ok();
                if let Some(m) = app.try_state::<SocketManager>() {
                    let mut names = m.accept_names.lock().unwrap();
                    if let Some(key) = matching_key(&names, &listener_name) {
                        names.remove(&key);
                    }
                    names.insert(
                        listener_name.clone(),
                        PendingAccept {
                            stream: Some(stream),
                            app: app.clone(),
                            server_id: server_id.clone(),
                            network: network.clone(),
                            nick: nick.clone(),
                            peer_ip: peer
                                .map(|a| display_ip(a.ip()).to_string())
                                .unwrap_or_default(),
                            peer_port: peer.map(|a| a.port()).unwrap_or(0),
                            bind_ip: local
                                .map(|a| display_ip(a.ip()).to_string())
                                .unwrap_or_default(),
                            bind_port: local.map(|a| a.port()).unwrap_or(0),
                            listener_nodelay,
                        },
                    );
                } else {
                    continue;
                }
                fire(
                    &app,
                    &server_id,
                    &network,
                    &nick,
                    "SOCKLISTEN",
                    &listener_name,
                    "",
                );
                if let Some(m) = app.try_state::<SocketManager>() {
                    let mut names = m.accept_names.lock().unwrap();
                    if let Some(key) = matching_key(&names, &listener_name) {
                        names.remove(&key);
                    }
                }
            }
        });
        let mut handle = SockHandle::new(live_name, None, io, task, port, true);
        handle.bind_ip = bind_ip;
        handle.bind_port = port;
        handle.mark = mark;
        self.socks.lock().unwrap().insert(key, handle);
        drop(name_guard);
        let _ = start_tx.send(());
    }

    /// Records the name a `/sockaccept` assigned to a listener's pending connection.
    pub fn accept(&self, listener: &str, name: &str) -> i32 {
        self.accept_with_options(listener, name, false)
    }

    pub fn accept_with_options(&self, listener: &str, name: &str, nodelay: bool) -> i32 {
        if name.is_empty() {
            return WSA_INVALID_ARGUMENT;
        }
        let _name_guard = self.name_ops.lock().unwrap();
        if self.name_in_use(name) {
            return WSA_ADDRESS_IN_USE;
        }
        let mut names = self.accept_names.lock().unwrap();
        let Some(key) = matching_key(&names, listener) else {
            return WSA_INVALID_ARGUMENT;
        };
        let Some(pending) = names.get_mut(&key) else {
            return WSA_INVALID_ARGUMENT;
        };
        let Some(stream) = pending.stream.as_ref() else {
            return WSA_INVALID_ARGUMENT;
        };
        if nodelay && !pending.listener_nodelay {
            if let Err(error) = stream.set_nodelay(true) {
                return io_error_code(&error);
            }
        }
        let stream = pending.stream.take().unwrap();
        let app = pending.app.clone();
        let server_id = pending.server_id.clone();
        let network = pending.network.clone();
        let nick = pending.nick.clone();
        let peer_ip = pending.peer_ip.clone();
        let peer_port = pending.peer_port;
        let bind_ip = pending.bind_ip.clone();
        let bind_port = pending.bind_port;
        drop(names);
        spawn_connected(
            app,
            server_id,
            network,
            nick,
            name.to_string(),
            NetStream::Plain(stream),
            peer_ip,
            peer_port,
            bind_ip,
            bind_port,
        );
        0
    }

    /// Writes `data` to `name`, or every socket matching it when `name` is a wildcard.
    pub fn write(&self, name: &str, data: Vec<u8>) -> i32 {
        self.write_with_failures(name, data).error
    }

    pub fn write_with_failures(&self, name: &str, data: Vec<u8>) -> SocketWriteResult {
        let mut socks = self.socks.lock().unwrap();
        if let Some(key) = matching_key(&socks, name) {
            let error = queue_write(socks.get_mut(&key).unwrap(), data);
            return SocketWriteResult {
                error,
                failures: if error == 0 {
                    Vec::new()
                } else {
                    vec![(key, error)]
                },
            };
        }
        if name.contains(['*', '?']) {
            let mut matched = false;
            let mut first_error = 0;
            let mut failures = Vec::new();
            for (k, h) in socks.iter_mut() {
                if wildcard_match(name, k) {
                    matched = true;
                    let error = queue_write(h, data.clone());
                    if error != 0 {
                        failures.push((k.clone(), error));
                    }
                    if first_error == 0 && error != 0 {
                        first_error = error;
                    }
                }
            }
            return SocketWriteResult {
                error: if matched {
                    first_error
                } else {
                    WSA_NOT_A_SOCKET
                },
                failures,
            };
        }
        SocketWriteResult {
            error: WSA_NOT_A_SOCKET,
            failures: Vec::new(),
        }
    }

    /// Requests an in-place TLS handshake on an already-connected plain TCP
    /// socket (`/sockopen -t name`). The task fires SOCKOPEN again on success.
    pub fn starttls(&self, name: &str) -> i32 {
        let mut socks = self.socks.lock().unwrap();
        let Some(h) = matching_key(&socks, name).and_then(|key| socks.get_mut(&key)) else {
            return WSA_NOT_A_SOCKET;
        };
        if h.listening || h.udp || h.tls {
            h.wserr = WSA_NOT_A_SOCKET;
            h.wsmsg = "socket cannot be upgraded with STARTTLS".to_string();
            return h.wserr;
        }
        let Some(tx) = h.outgoing.as_ref() else {
            h.wserr = WSA_NOT_CONNECTED;
            h.wsmsg = "socket is not connected".to_string();
            return h.wserr;
        };
        if tx.send(TcpCommand::StartTls).is_err() {
            h.wserr = WSA_CONNECTION_RESET;
            h.wsmsg = "socket command queue is closed".to_string();
            return h.wserr;
        }
        clear_error(h);
        0
    }

    /// Closes sockets whose name matches `pattern` (plain name or wildcard).
    pub fn close(&self, pattern: &str) -> i32 {
        let _name_guard = self.name_ops.lock().unwrap();
        let before = self.bound.lock().unwrap().len();
        self.bound
            .lock()
            .unwrap()
            .retain(|k, _| !wildcard_match(pattern, k));
        let bound_matched = self.bound.lock().unwrap().len() != before;
        let reserved_before = self.reservations.lock().unwrap().len();
        self.reservations
            .lock()
            .unwrap()
            .retain(|_, reserved| !wildcard_match(pattern, &reserved.name));
        let reserved_matched = self.reservations.lock().unwrap().len() != reserved_before;
        let mut socks = self.socks.lock().unwrap();
        let matched: Vec<String> = socks
            .keys()
            .filter(|k| wildcard_match(pattern, k))
            .cloned()
            .collect();
        let live_matched = !matched.is_empty();
        for name in matched {
            if let Some(h) = socks.remove(&name) {
                h.task.abort();
            }
        }
        if bound_matched || reserved_matched || live_matched {
            0
        } else {
            WSA_NOT_A_SOCKET
        }
    }

    /// `/sockrename <name> <newname>`.
    pub fn rename(&self, name: &str, newname: &str) -> i32 {
        if name.is_empty() || newname.is_empty() {
            return WSA_INVALID_ARGUMENT;
        }
        let _name_guard = self.name_ops.lock().unwrap();
        let old_live_key = matching_key(&self.socks.lock().unwrap(), name);
        let old_bound_key = matching_key(&self.bound.lock().unwrap(), name);
        let old_reservation_id = self.reservation_id_by_name(name);
        if old_live_key.is_none() && old_bound_key.is_none() && old_reservation_id.is_none() {
            return WSA_NOT_A_SOCKET;
        }
        let same_socket = old_live_key
            .as_deref()
            .or(old_bound_key.as_deref())
            .is_some_and(|key| key.eq_ignore_ascii_case(newname))
            || old_reservation_id.is_some_and(|id| {
                self.reservations
                    .lock()
                    .unwrap()
                    .get(&id)
                    .is_some_and(|reserved| reserved.name.eq_ignore_ascii_case(newname))
            });
        if !same_socket && self.name_in_use(newname) {
            return WSA_ADDRESS_IN_USE;
        }
        if let Some(key) = old_live_key {
            let mut socks = self.socks.lock().unwrap();
            let Some(mut h) = socks.remove(&key) else {
                return WSA_NOT_A_SOCKET;
            };
            *h.name.lock().unwrap() = newname.to_string();
            clear_error(&mut h);
            socks.insert(newname.to_string(), h);
            return 0;
        }
        if let Some(key) = old_bound_key {
            let mut bound = self.bound.lock().unwrap();
            let Some(value) = bound.remove(&key) else {
                return WSA_NOT_A_SOCKET;
            };
            bound.insert(newname.to_string(), value);
            return 0;
        }
        if let Some(id) = old_reservation_id {
            if let Some(reserved) = self.reservations.lock().unwrap().get_mut(&id) {
                reserved.name = newname.to_string();
                return 0;
            }
        }
        WSA_NOT_A_SOCKET
    }

    /// `/sockpause [-r] <name>` — pause or (with `resume`) restart reading.
    pub fn pause(&self, name: &str, resume: bool) -> i32 {
        let _name_guard = self.name_ops.lock().unwrap();
        let mut socks = self.socks.lock().unwrap();
        let mut matched = false;
        for (key, h) in socks.iter_mut() {
            if wildcard_match(name, key) {
                matched = true;
                h.io.paused.store(!resume, Ordering::SeqCst);
                clear_error(h);
                h.io.pause_notify.notify_one();
            }
        }
        drop(socks);
        for reserved in self.reservations.lock().unwrap().values_mut() {
            if wildcard_match(name, &reserved.name) {
                matched = true;
                reserved.paused = !resume;
            }
        }
        if matched {
            0
        } else {
            WSA_NOT_A_SOCKET
        }
    }

    /// Consumes data from a socket's receive queue using mIRC `/sockread`
    /// semantics. The socket remains locked only long enough to clone its shared
    /// queue state; event execution can therefore safely perform repeated reads.
    pub fn read(
        &self,
        name: &str,
        mut options: SocketReadOptions,
    ) -> Result<SocketReadResult, i32> {
        let (io, udp) = {
            let socks = self.socks.lock().unwrap();
            matching_key(&socks, name)
                .and_then(|key| socks.get(&key))
                .map(|h| (h.io.clone(), h.udp))
                .ok_or(WSA_NOT_A_SOCKET)?
        };
        if udp && options.line {
            options.force = true;
        }
        Ok(read_queued(&io, options))
    }

    pub fn set_mark(&self, name: &str, mark: &str) -> i32 {
        let _name_guard = self.name_ops.lock().unwrap();
        let mut matched = false;
        {
            let mut socks = self.socks.lock().unwrap();
            for (key, h) in socks.iter_mut() {
                if wildcard_match(name, key) {
                    matched = true;
                    h.mark = mark.to_string();
                    clear_error(h);
                }
            }
        }
        {
            let mut bound = self.bound.lock().unwrap();
            for (key, value) in bound.iter_mut() {
                if wildcard_match(name, key) {
                    matched = true;
                    value.mark = mark.to_string();
                }
            }
        }
        {
            let mut reservations = self.reservations.lock().unwrap();
            for reserved in reservations.values_mut() {
                if wildcard_match(name, &reserved.name) {
                    matched = true;
                    reserved.mark = mark.to_string();
                }
            }
        }
        if matched {
            0
        } else {
            WSA_NOT_A_SOCKET
        }
    }

    /// `$sock(name).property` value (empty for unknown name/property).
    pub fn prop(&self, name: &str, property: &str) -> String {
        let socks = self.socks.lock().unwrap();
        if let Some(key) = matching_key(&socks, name) {
            if let Some(h) = socks.get(&key) {
                return h.prop(&key, property);
            }
        }
        drop(socks);
        // A bound-but-not-yet-started listener.
        let bound = self.bound.lock().unwrap();
        if let Some(key) = matching_key(&bound, name) {
            if let Some(listener) = bound.get(&key) {
                return match property.to_ascii_lowercase().as_str() {
                    "name" => key,
                    "port" => listener.port.to_string(),
                    "status" => "listening".to_string(),
                    "type" => "TCP".to_string(),
                    "bindip" => listener.bind_ip.clone(),
                    "bindport" => listener.port.to_string(),
                    "mark" => listener.mark.clone(),
                    "ssl" | "starttls" | "pause" => "$false".to_string(),
                    "sent" | "rcvd" | "sq" | "rq" | "ls" | "lr" | "to" | "wserr" => "0".to_string(),
                    "ip" | "addr" | "saddr" | "sport" | "wsmsg" => String::new(),
                    "upnp" => "$false".to_string(),
                    _ => String::new(),
                };
            }
        }
        drop(bound);
        if let Some(reserved) = self
            .reservations
            .lock()
            .unwrap()
            .values()
            .find(|reserved| reserved.name.eq_ignore_ascii_case(name))
        {
            return reserved.prop(property);
        }
        String::new()
    }

    /// Names of sockets for `/socklist` — `filter` may carry `-l` (listening only)
    /// and/or a trailing name/wildcard.
    pub fn list(&self, filter: &str) -> Vec<String> {
        let switches: String = filter
            .split_whitespace()
            .filter_map(|t| t.strip_prefix('-'))
            .collect();
        let has_type_filter = switches.chars().any(|c| matches!(c, 't' | 'u' | 'l'));
        let pat = filter
            .split_whitespace()
            .find(|t| !t.starts_with('-'))
            .unwrap_or("*");
        let mut out: Vec<String> = Vec::new();
        for (name, h) in self.socks.lock().unwrap().iter() {
            let included = !has_type_filter
                || (switches.contains('l') && h.listening)
                || (switches.contains('u') && h.udp)
                || (switches.contains('t') && !h.listening && !h.udp);
            if !included {
                continue;
            }
            if wildcard_match(pat, name) {
                let status = match h.status {
                    SockStatus::Connecting => "connecting",
                    SockStatus::Active => "active",
                    SockStatus::Listening => "listening",
                };
                out.push(format!("{name}  {status}  port {}", h.port));
            }
        }
        for (name, listener) in self.bound.lock().unwrap().iter() {
            if (!has_type_filter || switches.contains('l')) && wildcard_match(pat, name) {
                out.push(format!("{name}  listening  port {}", listener.port));
            }
        }
        for reserved in self.reservations.lock().unwrap().values() {
            let included = !has_type_filter
                || (switches.contains('u') && reserved.kind == ReservedKind::Udp)
                || (switches.contains('t') && reserved.kind == ReservedKind::Tcp);
            if included && wildcard_match(pat, &reserved.name) {
                let status = if reserved.kind == ReservedKind::Udp {
                    "active"
                } else {
                    "connecting"
                };
                out.push(format!(
                    "{}  {status}  port {}",
                    reserved.name, reserved.port
                ));
            }
        }
        out.sort();
        out
    }

    pub fn exists(&self, name: &str) -> bool {
        if self
            .socks
            .lock()
            .unwrap()
            .keys()
            .any(|k| wildcard_match(name, k))
        {
            return true;
        }
        if self
            .bound
            .lock()
            .unwrap()
            .keys()
            .any(|k| wildcard_match(name, k))
        {
            return true;
        }
        self.reservations
            .lock()
            .unwrap()
            .values()
            .any(|reserved| wildcard_match(name, &reserved.name))
    }

    /// All open socket names (incl. bound listeners), sorted — for the frontend
    /// socket list (`script_sockets`).
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.socks.lock().unwrap().keys().cloned().collect();
        names.extend(self.bound.lock().unwrap().keys().cloned());
        names.extend(
            self.reservations
                .lock()
                .unwrap()
                .values()
                .map(|reserved| reserved.name.clone()),
        );
        names.sort();
        names.dedup();
        names
    }
}

/// Updates a socket's I/O stats (called by the read/write loop).
fn bump(app: &AppHandle, name: &Arc<Mutex<String>>, sent: u64, rcvd: u64) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            if sent > 0 {
                h.sent += sent;
                h.last_sent = Instant::now();
            }
            if rcvd > 0 {
                h.rcvd += rcvd;
                h.last_rcvd = Instant::now();
            }
        }
    }
}

fn set_ip(app: &AppHandle, name: &Arc<Mutex<String>>, ip: &str) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            h.ip = ip.to_string();
        }
    }
}

fn set_binding(app: &AppHandle, name: &Arc<Mutex<String>>, ip: &str, port: u16) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            h.bind_ip = ip.to_string();
            h.bind_port = port;
        }
    }
}

fn set_udp_source(app: &AppHandle, name: &Arc<Mutex<String>>, source: SocketAddr) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            h.saddr = display_ip(source.ip()).to_string();
            h.sport = source.port();
        }
    }
}

fn set_active(app: &AppHandle, name: &Arc<Mutex<String>>) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            h.status = SockStatus::Active;
            clear_error(h);
        }
    }
}

fn set_starttls_active(app: &AppHandle, name: &Arc<Mutex<String>>) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            mark_starttls_active(h);
        }
    }
}

fn mark_starttls_active(handle: &mut SockHandle) {
    handle.tls = true;
    handle.starttls = true;
    handle.wserr = 0;
    handle.wsmsg.clear();
}

fn set_error(app: &AppHandle, name: &Arc<Mutex<String>>, code: i32, msg: &str) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(h) = socks.values_mut().find(|h| Arc::ptr_eq(&h.name, name)) {
            h.wserr = code;
            h.wsmsg = msg.to_string();
        }
    }
}

fn io_error_code(error: &std::io::Error) -> i32 {
    if let Some(raw) = error
        .raw_os_error()
        .filter(|raw| (10_000..12_000).contains(raw))
    {
        return raw;
    }
    match error.kind() {
        io::ErrorKind::PermissionDenied => WSA_ACCESS_DENIED,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => WSA_INVALID_ARGUMENT,
        io::ErrorKind::WouldBlock => WSA_WOULD_BLOCK,
        io::ErrorKind::NotFound => WSA_NOT_A_SOCKET,
        io::ErrorKind::AddrInUse | io::ErrorKind::AlreadyExists => WSA_ADDRESS_IN_USE,
        io::ErrorKind::AddrNotAvailable => WSA_ADDRESS_NOT_AVAILABLE,
        io::ErrorKind::ConnectionAborted => WSA_CONNECTION_ABORTED,
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => WSA_CONNECTION_RESET,
        io::ErrorKind::NotConnected => WSA_NOT_CONNECTED,
        io::ErrorKind::TimedOut => WSA_TIMED_OUT,
        io::ErrorKind::ConnectionRefused => WSA_CONNECTION_REFUSED,
        _ => match error.raw_os_error() {
            // Common POSIX errno values whose ErrorKind classification is not
            // equally specific on every supported Rust target.
            Some(50 | 100) => WSA_NETWORK_DOWN,
            Some(51 | 101) => WSA_NETWORK_UNREACHABLE,
            Some(65 | 113) => WSA_HOST_UNREACHABLE,
            _ => 1,
        },
    }
}

/// Spawns a connected-socket task for an already-open stream (used by `/sockaccept`).
fn spawn_connected(
    app: AppHandle,
    server_id: String,
    network: String,
    nick: String,
    name: String,
    stream: NetStream,
    peer_ip: String,
    peer_port: u16,
    bind_ip: String,
    bind_port: u16,
) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let (tx, rx) = mpsc::unbounded_channel::<TcpCommand>();
        let key = name.clone();
        let live_name = Arc::new(Mutex::new(name));
        let io = Arc::new(SockIo::default());
        let task_io = io.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task_name = live_name.clone();
        let task_app = app.clone();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            set_active(&task_app, &task_name);
            run_connected(
                task_app,
                server_id,
                network,
                nick,
                task_name,
                task_io,
                stream,
                rx,
                String::new(),
            )
            .await;
        });
        let mut h = SockHandle::new(live_name, Some(tx), io, task, peer_port, false);
        h.ip = peer_ip;
        h.bind_ip = bind_ip;
        h.bind_port = bind_port;
        m.socks.lock().unwrap().insert(key, h);
        let _ = start_tx.send(());
    }
}

/// The read/write loop shared by connect and accepted sockets.
async fn run_connected(
    app: AppHandle,
    server_id: String,
    network: String,
    nick: String,
    name: Arc<Mutex<String>>,
    io: Arc<SockIo>,
    stream: NetStream,
    mut rx: UnboundedReceiver<TcpCommand>,
    tls_host: String,
) {
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let mut buf = [0u8; 8192];
    let mut fire_close = true;
    'outer: loop {
        macro_rules! upgrade_starttls {
            () => {{
                let stream = read_half.unsplit(write_half);
                match upgrade_starttls_stream(&tls_host, stream).await {
                    Ok(stream) => {
                        (read_half, write_half) = tokio::io::split(stream);
                        set_starttls_active(&app, &name);
                        let socket_name = current_name(&name);
                        fire(
                            &app,
                            &server_id,
                            &network,
                            &nick,
                            "SOCKOPEN",
                            &socket_name,
                            "",
                        );
                    }
                    Err(error) => {
                        let code = io_error_code(&error);
                        set_error(&app, &name, code, &error.to_string());
                        let socket_name = current_name(&name);
                        fire_with_error(
                            &app,
                            &server_id,
                            &network,
                            &nick,
                            "SOCKOPEN",
                            &socket_name,
                            "",
                            code,
                        );
                        fire_close = false;
                        break 'outer;
                    }
                }
            }};
        }
        tokio::select! {
            _ = io.pause_notify.notified() => {
                if !io.paused.load(Ordering::SeqCst) {
                    deliver_queued(&app, &server_id, &network, &nick, &name, &io, "SOCKREAD");
                }
            },
            out = rx.recv() => match out {
                Some(TcpCommand::Data(mut data)) => {
                    // Drain and send everything currently queued, then fire
                    // on SOCKWRITE once (mIRC: "finished sending all queued data").
                    let mut starttls_after_write = false;
                    loop {
                        let len = data.len();
                        if let Err(error) = write_half.write_all(&data).await {
                            subtract_queued(&io.send_queued, len);
                            io.send_queued.store(0, Ordering::SeqCst);
                            let code = io_error_code(&error);
                            set_error(&app, &name, code, &error.to_string());
                            let socket_name = current_name(&name);
                            fire_with_error(
                                &app,
                                &server_id,
                                &network,
                                &nick,
                                "SOCKWRITE",
                                &socket_name,
                                "",
                                code,
                            );
                            fire_close = false;
                            break 'outer;
                        }
                        subtract_queued(&io.send_queued, len);
                        bump(&app, &name, len as u64, 0);
                        set_error(&app, &name, 0, "");
                        match rx.try_recv() {
                            Ok(TcpCommand::Data(more)) => data = more,
                            Ok(TcpCommand::StartTls) => {
                                starttls_after_write = true;
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    let socket_name = current_name(&name);
                    fire(&app, &server_id, &network, &nick, "SOCKWRITE", &socket_name, "");
                    if starttls_after_write {
                        upgrade_starttls!();
                    }
                }
                Some(TcpCommand::StartTls) => upgrade_starttls!(),
                None => break,
            },
            // `/sockpause` pauses only receiving. Writes and their SOCKWRITE
            // events must continue while this branch is disabled.
            res = read_half.read(&mut buf), if !io.paused.load(Ordering::SeqCst) => match res {
                Ok(0) => break, // EOF
                Ok(n) => {
                    bump(&app, &name, 0, n as u64);
                    set_error(&app, &name, 0, "");
                    io.recv.lock().unwrap().extend(&buf[..n]);
                    if !io.paused.load(Ordering::SeqCst) {
                        deliver_queued(&app, &server_id, &network, &nick, &name, &io, "SOCKREAD");
                    }
                }
                Err(error) => {
                    let code = io_error_code(&error);
                    set_error(&app, &name, code, &error.to_string());
                    let socket_name = current_name(&name);
                    fire_with_error(
                        &app,
                        &server_id,
                        &network,
                        &nick,
                        "SOCKREAD",
                        &socket_name,
                        "",
                        code,
                    );
                    fire_close = false;
                    break;
                }
            },
        }
    }
    if fire_close {
        let socket_name = current_name(&name);
        fire(
            &app,
            &server_id,
            &network,
            &nick,
            "SOCKCLOSE",
            &socket_name,
            "",
        );
    }
    forget(&app, &name);
}

/// Runs SOCKREAD handlers while the buffered data meets mIRC's re-trigger
/// rules. A handler that never attempts `/sockread` causes the unread data to be
/// discarded; an attempted line read on a partial line keeps it for the next
/// network arrival.
fn deliver_queued(
    app: &AppHandle,
    server_id: &str,
    network: &str,
    nick: &str,
    name: &Arc<Mutex<String>>,
    io: &SockIo,
    kind: &str,
) {
    loop {
        if io.paused.load(Ordering::SeqCst) {
            return;
        }
        let (preview, before_len) = {
            let queue = io.recv.lock().unwrap();
            if queue.is_empty() {
                return;
            }
            let before_len = queue.len();
            let take = queue
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|i| i + 1)
                .unwrap_or(queue.len());
            let mut bytes: Vec<u8> = queue.iter().take(take).copied().collect();
            while matches!(bytes.last(), Some(b'\r' | b'\n')) {
                bytes.pop();
            }
            (bytes, before_len)
        };
        let attempts = io.read_attempts.load(Ordering::SeqCst);
        let socket_name = current_name(name);
        let text = decode_line(&preview);
        fire_read(
            app,
            server_id,
            network,
            nick,
            kind,
            &socket_name,
            &text,
            &preview,
        );

        if io.read_attempts.load(Ordering::SeqCst) == attempts {
            io.recv.lock().unwrap().clear();
            return;
        }
        if io.paused.load(Ordering::SeqCst) {
            return;
        }
        let queue = io.recv.lock().unwrap();
        if queue.is_empty() {
            return;
        }
        if queue.len() == before_len {
            return;
        }
        let retrigger =
            io.last_read_binary.load(Ordering::SeqCst) || queue.iter().any(|byte| *byte == b'\n');
        drop(queue);
        if !retrigger {
            return;
        }
    }
}

fn forget(app: &AppHandle, name: &Arc<Mutex<String>>) {
    if let Some(m) = app.try_state::<SocketManager>() {
        let mut socks = m.socks.lock().unwrap();
        if let Some(key) = identity_key(&socks, name) {
            socks.remove(&key);
        }
    }
}

fn decode_line(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Fires a SOCK* script event and applies the resulting actions. The socket name
/// is exposed as `$sockname` (and matched by the event's target), the line as `$1-`.
fn fire(
    app: &AppHandle,
    server_id: &str,
    network: &str,
    nick: &str,
    kind: &str,
    name: &str,
    line: &str,
) {
    fire_with_error(app, server_id, network, nick, kind, name, line, 0);
}

/// Applies a socket event carrying an explicit `$sockerr`. Used by the action
/// layer for command-time failures that occur before an async I/O task exists.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fire_error(
    app: &AppHandle,
    server_id: &str,
    network: &str,
    nick: &str,
    kind: &str,
    name: &str,
    line: &str,
    sock_error: i32,
) {
    fire_with_error(app, server_id, network, nick, kind, name, line, sock_error);
}

#[allow(clippy::too_many_arguments)]
fn fire_with_error(
    app: &AppHandle,
    server_id: &str,
    network: &str,
    nick: &str,
    kind: &str,
    name: &str,
    line: &str,
    sock_error: i32,
) {
    let Some(engine) = app.try_state::<ScriptEngine>() else {
        return;
    };
    let ctx = RunCtx {
        my_nick: nick,
        network,
        server: "",
        data_dir: script_data_dir(app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(server_id))
            .unwrap_or_default(),
    };
    let vars = EventVars {
        chan: name.to_string(),
        target: name.to_string(),
        params: line.split_whitespace().map(String::from).collect(),
        text: line.to_string(),
        sock_error,
        ..Default::default()
    };
    let actions = engine.dispatch_event(&ctx, kind, vars);
    apply_actions(app, server_id, nick, network, "", actions);
}

/// Fires `on SOCKREAD` carrying both a decoded text view (`$1-` / `sockread %var`)
/// and the exact line bytes (`sockread &binvar`), so binary protocols read
/// byte-for-byte with no UTF-8 round-trip.
fn fire_read(
    app: &AppHandle,
    server_id: &str,
    network: &str,
    nick: &str,
    kind: &str,
    name: &str,
    text: &str,
    bytes: &[u8],
) {
    let Some(engine) = app.try_state::<ScriptEngine>() else {
        return;
    };
    let ctx = RunCtx {
        my_nick: nick,
        network,
        server: "",
        data_dir: script_data_dir(app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(server_id))
            .unwrap_or_default(),
    };
    let vars = EventVars {
        chan: name.to_string(),
        target: name.to_string(),
        params: text.split_whitespace().map(String::from).collect(),
        text: text.to_string(),
        sock_bytes: bytes.to_vec(),
        ..Default::default()
    };
    let actions = engine.dispatch_event(&ctx, kind, vars);
    apply_actions(app, server_id, nick, network, "", actions);
}

/// Production [`super::eval::ScriptSockets`] backend, backed by the
/// [`SocketManager`]. Installed on the engine at startup.
pub struct EngineSockets {
    app: AppHandle,
}

impl EngineSockets {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
    fn mgr(&self) -> Option<tauri::State<'_, SocketManager>> {
        self.app.try_state::<SocketManager>()
    }

    pub fn reserve_open(
        &self,
        name: &str,
        host: &str,
        port: u16,
        tls: bool,
        bind_ip: &str,
    ) -> Option<Result<u64, i32>> {
        Some(self.mgr()?.reserve_open(name, host, port, tls, bind_ip))
    }

    pub fn reserve_udp(
        &self,
        name: &str,
        bind_ip: &str,
        local_port: u16,
        dest_ip: &str,
        dest_port: u16,
    ) -> Option<Result<u64, i32>> {
        Some(
            self.mgr()?
                .reserve_udp(name, bind_ip, local_port, dest_ip, dest_port),
        )
    }

    pub fn listen_reserved(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Option<Result<(u16, u64), i32>> {
        Some(
            self.mgr()?
                .listen_reserved(bind_ip, name, port, nodelay, dual_stack),
        )
    }
}

impl super::eval::ScriptSockets for EngineSockets {
    fn reserve_open(
        &self,
        name: &str,
        host: &str,
        port: u16,
        tls: bool,
        bind_ip: &str,
    ) -> Option<Result<u64, i32>> {
        EngineSockets::reserve_open(self, name, host, port, tls, bind_ip)
    }

    fn reserve_udp(
        &self,
        name: &str,
        bind_ip: &str,
        local_port: u16,
        dest_ip: &str,
        dest_port: u16,
    ) -> Option<Result<u64, i32>> {
        EngineSockets::reserve_udp(self, name, bind_ip, local_port, dest_ip, dest_port)
    }

    fn listen_reserved(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Option<Result<(u16, u64), i32>> {
        EngineSockets::listen_reserved(self, bind_ip, name, port, nodelay, dual_stack)
    }

    fn listen(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Option<Result<u16, i32>> {
        Some(
            self.mgr()?
                .listen_with_options(bind_ip, name, port, nodelay, dual_stack),
        )
    }
    fn accept(&self, name: &str, listener: &str, nodelay: bool) -> Option<i32> {
        Some(self.mgr()?.accept_with_options(listener, name, nodelay))
    }
    fn close(&self, pattern: &str) -> Option<i32> {
        Some(self.mgr()?.close(pattern))
    }
    fn set_mark(&self, name: &str, mark: &str) -> Option<i32> {
        Some(self.mgr()?.set_mark(name, mark))
    }
    fn rename(&self, name: &str, newname: &str) -> Option<i32> {
        Some(self.mgr()?.rename(name, newname))
    }
    fn pause(&self, name: &str, resume: bool) -> Option<i32> {
        Some(self.mgr()?.pause(name, resume))
    }
    fn write(&self, name: &str, data: &[u8]) -> Option<SocketWriteResult> {
        Some(self.mgr()?.write_with_failures(name, data.to_vec()))
    }
    fn starttls(&self, name: &str) -> Option<i32> {
        Some(self.mgr()?.starttls(name))
    }
    fn read(
        &self,
        name: &str,
        options: SocketReadOptions,
    ) -> Option<Result<SocketReadResult, i32>> {
        Some(self.mgr()?.read(name, options))
    }
    fn exists(&self, name: &str) -> bool {
        self.mgr().map(|m| m.exists(name)).unwrap_or(false)
    }
    fn matching_names(&self, pattern: &str) -> Vec<String> {
        self.mgr()
            .map(|m| {
                m.names()
                    .into_iter()
                    .filter(|name| wildcard_match(pattern, name))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn prop(&self, name: &str, property: &str) -> String {
        self.mgr()
            .map(|m| m.prop(name, property))
            .unwrap_or_default()
    }
    fn list(&self, filter: &str) -> Vec<String> {
        self.mgr().map(|m| m.list(filter)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_connected(
        manager: &SocketManager,
        name: &str,
    ) -> (Arc<SockIo>, UnboundedReceiver<TcpCommand>) {
        let live_name = Arc::new(Mutex::new(name.to_string()));
        let io = Arc::new(SockIo::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tauri::async_runtime::spawn(std::future::pending::<()>());
        manager.socks.lock().unwrap().insert(
            name.to_string(),
            SockHandle::new(live_name, Some(tx), io.clone(), task, 6667, false),
        );
        (io, rx)
    }

    fn command_data(command: TcpCommand) -> Vec<u8> {
        match command {
            TcpCommand::Data(data) => data,
            TcpCommand::StartTls => panic!("expected socket data"),
        }
    }

    fn line(binary: bool, force: bool) -> SocketReadOptions {
        SocketReadOptions {
            binary,
            force,
            line: true,
            max_bytes: 4096,
        }
    }

    #[test]
    fn reserved_open_is_visible_and_state_follows_rename_until_close() {
        let m = SocketManager::new();
        let id = m
            .reserve_open("pending", "example.test", 6697, true, "127.0.0.1")
            .unwrap();
        assert!(id > 0);
        assert!(m.exists("pending"));
        assert_eq!(m.prop("pending", "status"), "connecting");
        assert_eq!(m.prop("pending", "addr"), "example.test");
        assert_eq!(m.prop("pending", "ssl"), "$true");

        assert_eq!(m.set_mark("pending", "kept"), 0);
        assert_eq!(m.pause("pending", false), 0);
        assert_eq!(m.rename("pending", "renamed"), 0);
        assert!(!m.exists("pending"));
        assert_eq!(m.prop("renamed", "mark"), "kept");
        assert_eq!(m.prop("renamed", "pause"), "$true");
        assert_eq!(m.reservation_id_by_name("renamed"), Some(id));

        assert_eq!(m.close("renamed"), 0);
        assert!(!m.exists("renamed"));
        assert!(!m.reservations.lock().unwrap().contains_key(&id));
    }

    #[test]
    fn repeated_reserved_udp_reuses_the_first_stable_reservation() {
        let m = SocketManager::new();
        let id = m
            .reserve_udp("dns", "127.0.0.1", 0, "127.0.0.2", 53)
            .unwrap();
        assert!(id > 0);
        assert_eq!(m.reserve_udp("DNS", "", 0, "127.0.0.3", 5353), Ok(0));
        assert_eq!(m.prop("dns", "type"), "UDP");
        assert_eq!(m.prop("dns", "ip"), "127.0.0.3");
        assert_eq!(m.prop("dns", "port"), "5353");
        assert_eq!(m.reservation_id_by_name("dns"), Some(id));
    }

    #[test]
    fn deferred_existing_udp_reuse_follows_rename_without_recreating_old() {
        let m = SocketManager::new();
        let (_io, _tcp_rx) = add_connected(&m, "existing");
        let (udp_tx, mut udp_rx) = mpsc::unbounded_channel();
        {
            let mut socks = m.socks.lock().unwrap();
            let handle = socks.get_mut("existing").unwrap();
            handle.udp = true;
            handle.udp_outgoing = Some(udp_tx);
        }

        assert_eq!(m.reserve_udp("existing", "", 0, "127.0.0.2", 9000), Ok(0));
        assert_eq!(m.rename("existing", "renamed"), 0);

        let target = m
            .take_deferred_udp_target("existing")
            .unwrap()
            .upgrade()
            .unwrap();
        let key = {
            let socks = m.socks.lock().unwrap();
            identity_key(&socks, &target).unwrap()
        };
        assert_eq!(key, "renamed");
        let destination = "127.0.0.2:9000".parse().unwrap();
        let error = {
            let mut socks = m.socks.lock().unwrap();
            queue_udp(
                socks.get_mut(&key).unwrap(),
                UdpWrite {
                    destination,
                    data: b"payload".to_vec(),
                    close_after: false,
                },
            )
        };
        assert_eq!(error, 0);
        assert_eq!(udp_rx.try_recv().unwrap().data, b"payload");
        assert!(!m.exists("existing"));
        assert!(m.exists("renamed"));
    }

    #[test]
    fn bound_listener_keeps_its_stable_id_when_renamed_before_start() {
        let m = SocketManager::new();
        let (port, id) = m
            .listen_reserved("127.0.0.1", "before", 0, false, false)
            .unwrap();
        assert!(port > 0);
        assert!(id > 0);
        assert_eq!(m.rename("before", "after"), 0);
        assert!(!m.exists("before"));
        assert_eq!(m.prop("after", "port"), port.to_string());
        assert_eq!(m.bound.lock().unwrap().get("after").unwrap().id, id);
    }

    #[test]
    fn listen_bind_props_rename_close() {
        let m = SocketManager::new();
        let port = m
            .listen("127.0.0.1", "relay", 0)
            .expect("bind a local listener");
        assert!(port > 0);
        assert_eq!(m.prop("RELAY", "port"), port.to_string());
        assert_eq!(m.prop("relay", "status"), "listening");
        assert_eq!(m.prop("relay", "name"), "relay");
        assert_eq!(m.prop("relay", "type"), "TCP");
        assert_eq!(m.prop("relay", "bindip"), "127.0.0.1");
        assert_eq!(m.prop("relay", "bindport"), port.to_string());
        assert!(m.exists("rel*"));
        assert!(m.list("*").iter().any(|l| l.contains("relay")));
        m.rename("relay", "rl2");
        assert!(!m.exists("relay"));
        assert_eq!(m.prop("rl2", "port"), port.to_string());
        m.close("rl2");
        assert!(!m.exists("rl2"));
    }

    #[test]
    fn connected_lookup_write_and_rename_are_case_insensitive() {
        let m = SocketManager::new();
        let live_name = Arc::new(Mutex::new(r"i7.%#the\blobby".to_string()));
        let io = Arc::new(SockIo::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tauri::async_runtime::spawn(std::future::pending::<()>());
        let mut handle =
            SockHandle::new(live_name.clone(), Some(tx), io.clone(), task, 6667, false);
        handle.status = SockStatus::Active;
        m.socks
            .lock()
            .unwrap()
            .insert(current_name(&live_name), handle);

        assert_eq!(m.prop(r"I7.%#The\bLobby", "status"), "active");
        assert_eq!(m.write(r"I7.%#The\bLobby", b"PRIVMSG test\r\n".to_vec()), 0);
        assert_eq!(m.prop(r"I7.%#The\bLobby", "sq"), "14");
        assert_eq!(command_data(rx.try_recv().unwrap()), b"PRIVMSG test\r\n");
        subtract_queued(&io.send_queued, 14);

        m.rename(r"I7.%#THE\BLOBBY", r"i7.%#The\bLobby");
        assert_eq!(current_name(&live_name), r"i7.%#The\bLobby");
        assert_eq!(m.prop(r"i7.%#the\blobby", "name"), r"i7.%#The\bLobby");
        m.close(r"I7.%#THE\BLOBBY");
        assert!(!m.exists(r"i7.%#The\bLobby"));
    }

    #[test]
    fn send_queue_enforces_mirc_limit_and_reports_write_errors() {
        let m = SocketManager::new();
        let (_io, mut rx) = add_connected(&m, "limited");
        assert_eq!(m.write("missing", vec![1]), WSA_NOT_A_SOCKET);

        assert_eq!(m.write("limited", vec![7; SEND_QUEUE_LIMIT]), 0);
        assert_eq!(m.prop("limited", "sq"), SEND_QUEUE_LIMIT.to_string());
        assert_eq!(m.write("limited", vec![8]), WSA_NO_BUFFER_SPACE);
        assert_eq!(m.prop("limited", "wserr"), WSA_NO_BUFFER_SPACE.to_string());
        assert!(m.prop("limited", "wsmsg").contains("16384"));
        assert_eq!(command_data(rx.try_recv().unwrap()).len(), SEND_QUEUE_LIMIT);
        assert!(rx.try_recv().is_err(), "overflow data must not be queued");
    }

    #[test]
    fn send_queue_accepts_exactly_16384_bytes_and_rejects_the_next_byte() {
        let m = SocketManager::new();
        let (_io, mut rx) = add_connected(&m, "boundary");

        assert_eq!(m.write("boundary", vec![0x41; 16_384]), 0);
        assert_eq!(m.prop("boundary", "sq"), "16384");
        assert_eq!(m.write("boundary", vec![0x42]), WSA_NO_BUFFER_SPACE);
        assert_eq!(m.prop("boundary", "sq"), "16384");
        assert_eq!(m.prop("boundary", "wserr"), "10055");
        assert_eq!(command_data(rx.try_recv().unwrap()).len(), 16_384);
        assert!(rx.try_recv().is_err(), "byte 16385 must not be queued");
    }

    #[test]
    fn io_error_kinds_map_to_stable_mirc_winsock_codes() {
        let cases = [
            (io::ErrorKind::PermissionDenied, WSA_ACCESS_DENIED),
            (io::ErrorKind::InvalidInput, WSA_INVALID_ARGUMENT),
            (io::ErrorKind::InvalidData, WSA_INVALID_ARGUMENT),
            (io::ErrorKind::WouldBlock, WSA_WOULD_BLOCK),
            (io::ErrorKind::NotFound, WSA_NOT_A_SOCKET),
            (io::ErrorKind::AddrInUse, WSA_ADDRESS_IN_USE),
            (io::ErrorKind::AlreadyExists, WSA_ADDRESS_IN_USE),
            (io::ErrorKind::AddrNotAvailable, WSA_ADDRESS_NOT_AVAILABLE),
            (io::ErrorKind::ConnectionAborted, WSA_CONNECTION_ABORTED),
            (io::ErrorKind::ConnectionReset, WSA_CONNECTION_RESET),
            (io::ErrorKind::BrokenPipe, WSA_CONNECTION_RESET),
            (io::ErrorKind::UnexpectedEof, WSA_CONNECTION_RESET),
            (io::ErrorKind::NotConnected, WSA_NOT_CONNECTED),
            (io::ErrorKind::TimedOut, WSA_TIMED_OUT),
            (io::ErrorKind::ConnectionRefused, WSA_CONNECTION_REFUSED),
        ];

        for (kind, expected) in cases {
            let error = io::Error::new(kind, "socket compatibility test");
            assert_eq!(
                io_error_code(&error),
                expected,
                "unexpected WSA mapping for {kind:?}"
            );
        }
    }

    #[test]
    fn starttls_is_queued_in_place_and_properties_change_only_after_success() {
        let m = SocketManager::new();
        let (_io, mut rx) = add_connected(&m, "mail");
        assert_eq!(m.starttls("MAIL"), 0);
        assert!(matches!(rx.try_recv().unwrap(), TcpCommand::StartTls));
        assert_eq!(m.prop("mail", "ssl"), "$false");
        assert_eq!(m.prop("mail", "starttls"), "$false");

        let mut socks = m.socks.lock().unwrap();
        let handle = socks.get_mut("mail").unwrap();
        handle.wserr = 1;
        handle.wsmsg = "old error".into();
        mark_starttls_active(handle);
        drop(socks);
        assert_eq!(m.prop("mail", "ssl"), "$true");
        assert_eq!(m.prop("mail", "starttls"), "$true");
        assert_eq!(m.prop("mail", "wserr"), "0");
        assert_eq!(m.starttls("mail"), WSA_NOT_A_SOCKET);
    }

    #[tokio::test]
    async fn failed_starttls_handshake_does_not_fall_back_to_plaintext() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut client_hello = [0u8; 512];
            let _ = stream.read(&mut client_hello).await;
            // Closing during the handshake must be reported as a TLS failure;
            // the client must never continue on the original plaintext stream.
        });
        let tcp = TcpStream::connect(address).await.unwrap();
        assert!(upgrade_starttls_stream("localhost", NetStream::Plain(tcp))
            .await
            .is_err());
        peer.await.unwrap();
    }

    #[test]
    fn receive_queue_drains_lines_and_binary_without_duplicates() {
        let m = SocketManager::new();
        let (io, _rx) = add_connected(&m, "sock");
        io.recv.lock().unwrap().extend(b"one\r\ntwo\r\n\0tail");
        assert_eq!(m.prop("sock", "rq"), "15");

        assert_eq!(
            m.read("sock", line(false, false)).unwrap(),
            SocketReadResult {
                data: b"one".to_vec(),
                bytes_read: 5
            }
        );
        // A binary line read consumes the next line, not a duplicate copy of
        // the text line that was already consumed.
        assert_eq!(
            m.read("SOCK", line(true, false)).unwrap(),
            SocketReadResult {
                data: b"two".to_vec(),
                bytes_read: 5
            }
        );
        assert_eq!(m.prop("sock", "rq"), "5");

        let raw = SocketReadOptions {
            binary: true,
            force: false,
            line: false,
            max_bytes: 3,
        };
        assert_eq!(
            m.read("sock", raw).unwrap(),
            SocketReadResult {
                data: vec![0, b't', b'a'],
                bytes_read: 3
            }
        );
        assert_eq!(m.prop("sock", "rq"), "2");
        assert_eq!(
            m.read("sock", line(false, true)).unwrap(),
            SocketReadResult {
                data: b"il".to_vec(),
                bytes_read: 2
            }
        );
        assert_eq!(
            m.read("sock", line(false, true)).unwrap(),
            SocketReadResult::default()
        );
        assert_eq!(m.prop("sock", "rq"), "0");

        io.recv.lock().unwrap().extend(b"abc\0hidden\r\n");
        assert_eq!(
            m.read("sock", line(false, true)).unwrap(),
            SocketReadResult {
                data: b"abc".to_vec(),
                bytes_read: 4,
            }
        );
        assert_eq!(
            m.read("sock", line(false, false)).unwrap(),
            SocketReadResult {
                data: b"hidden".to_vec(),
                bytes_read: 8,
            }
        );
    }

    #[test]
    fn binary_read_defaults_can_drain_more_than_one_event_chunk() {
        let m = SocketManager::new();
        let (io, _rx) = add_connected(&m, "binary");
        io.recv
            .lock()
            .unwrap()
            .extend(std::iter::repeat(7).take(5000));
        let options = SocketReadOptions {
            binary: true,
            force: false,
            line: false,
            max_bytes: 4096,
        };
        let first = m.read("binary", options).unwrap();
        assert_eq!(first.data.len(), 4096);
        assert_eq!(first.bytes_read, 4096);
        assert_eq!(m.prop("binary", "rq"), "904");
        assert_eq!(m.read("binary", options).unwrap().bytes_read, 904);
    }

    #[test]
    fn pause_status_and_error_properties_reflect_live_state() {
        let m = SocketManager::new();
        let (_io, _rx) = add_connected(&m, "state");
        assert_eq!(m.prop("state", "status"), "connecting");
        m.pause("STATE", false);
        assert_eq!(m.prop("state", "pause"), "$true");
        m.pause("state", true);
        assert_eq!(m.prop("state", "pause"), "$false");

        let mut socks = m.socks.lock().unwrap();
        let handle = socks.get_mut("state").unwrap();
        handle.status = SockStatus::Active;
        handle.wserr = 10054;
        handle.wsmsg = "connection reset by peer".to_string();
        drop(socks);
        assert_eq!(m.prop("state", "status"), "active");
        assert_eq!(m.prop("state", "wserr"), "10054");
        assert_eq!(m.prop("state", "wsmsg"), "connection reset by peer");
    }

    #[test]
    fn udp_properties_and_socklist_type_filters_match_mirc() {
        let m = SocketManager::new();
        let (_tcp_io, _tcp_rx) = add_connected(&m, "tcp");
        let (_udp_io, _udp_rx) = add_connected(&m, "udp");
        {
            let mut socks = m.socks.lock().unwrap();
            let udp = socks.get_mut("udp").unwrap();
            udp.udp = true;
            udp.ip = "127.0.0.2".into();
            udp.bind_ip = "127.0.0.1".into();
            udp.bind_port = 4567;
            udp.saddr = "127.0.0.3".into();
            udp.sport = 7654;
        }
        let listener_port = m.listen("127.0.0.1", "listener", 0).unwrap();

        assert_eq!(m.prop("udp", "type"), "UDP");
        assert_eq!(m.prop("udp", "saddr"), "127.0.0.3");
        assert_eq!(m.prop("udp", "sport"), "7654");
        assert_eq!(m.prop("udp", "bindip"), "127.0.0.1");
        assert_eq!(m.prop("udp", "bindport"), "4567");
        assert_eq!(m.prop("listener", "bindport"), listener_port.to_string());

        let tcp = m.list("-t");
        assert!(tcp.iter().any(|line| line.starts_with("tcp  ")));
        assert!(!tcp.iter().any(|line| line.starts_with("udp  ")));
        assert!(!tcp.iter().any(|line| line.starts_with("listener  ")));
        let udp = m.list("-u");
        assert_eq!(udp.len(), 1);
        assert!(udp[0].starts_with("udp  "));
        let listeners = m.list("-l");
        assert_eq!(listeners.len(), 1);
        assert!(listeners[0].starts_with("listener  "));
        assert_eq!(m.list("-tul").len(), 3);
    }

    #[test]
    fn udp_text_reads_consume_a_complete_datagram_without_crlf() {
        let m = SocketManager::new();
        let (io, _rx) = add_connected(&m, "udp");
        m.socks.lock().unwrap().get_mut("udp").unwrap().udp = true;
        io.recv.lock().unwrap().extend(b"datagram");

        assert_eq!(
            m.read("udp", line(false, false)).unwrap(),
            SocketReadResult {
                data: b"datagram".to_vec(),
                bytes_read: 8,
            }
        );
    }
}
