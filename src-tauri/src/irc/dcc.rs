//! DCC (Direct Client-to-Client) — peer-to-peer **chat** and **file transfer**,
//! negotiated over CTCP exactly like mIRC. This module is the protocol core:
//! parsing and formatting the CTCP DCC handshake, and encoding the IP the way
//! DCC does (a 32-bit integer). The TCP I/O, transfer state, and UI build on top
//! of this (later phases).
//!
//! Handshake (carried in a `PRIVMSG` to the peer, wrapped in `\x01`):
//! - `DCC CHAT chat <ip> <port>` — open a direct chat.
//! - `DCC SEND <filename> <ip> <port> <size>` — offer a file.
//!
//! `<ip>` is the IPv4 address as a big-endian `u32` written in decimal, and the
//! **offerer listens** on `<port>` while the **receiver connects** to it.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader, SeekFrom,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};
use tokio::time::timeout;

use super::event::{UiEvent, IRC_EVENT};
use super::ConnectionManager;

/// The kind of a DCC handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DccKind {
    Chat,
    Send,
}

/// A parsed incoming DCC offer (the `DCC …` text inside a CTCP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DccOffer {
    pub kind: DccKind,
    /// The offered filename for `SEND` (empty for `CHAT`).
    pub filename: String,
    pub ip: IpAddr,
    pub port: u16,
    /// The file size in bytes for `SEND` (`0` when absent or for `CHAT`).
    pub size: u64,
    /// mIRC passive/reverse DCC negotiation id. A port of zero requires this.
    pub token: Option<u64>,
}

/// Any CTCP DCC negotiation message understood by jIRC. `RESUME` and `ACCEPT`
/// do not carry an address, so they are represented separately from offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DccMessage {
    Offer(DccOffer),
    Resume {
        filename: String,
        port: u16,
        position: u64,
        token: Option<u64>,
    },
    Accept {
        filename: String,
        port: u16,
        position: u64,
        token: Option<u64>,
    },
}

/// Encodes an IPv4 address as the decimal 32-bit integer DCC uses.
// Part of the outgoing-offer API, wired in the DCC connect/send phase.
#[allow(dead_code)]
pub fn ip_to_dcc(ip: Ipv4Addr) -> u32 {
    u32::from(ip)
}

/// Decodes DCC's decimal 32-bit integer IP back into an address.
pub fn dcc_to_ip(n: u32) -> Ipv4Addr {
    Ipv4Addr::from(n)
}

/// Parses a DCC IP field: an IPv6 literal (has `:`) or DCC's decimal 32-bit
/// integer for IPv4.
fn parse_dcc_ip(s: &str) -> Option<IpAddr> {
    if s.contains(':') {
        s.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
    } else {
        s.parse::<u32>().ok().map(|n| IpAddr::V4(dcc_to_ip(n)))
    }
}

/// Formats an IP for a DCC offer: the 32-bit integer for IPv4, the literal for IPv6.
fn dcc_ip_str(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => ip_to_dcc(v4).to_string(),
        IpAddr::V6(v6) => v6.to_string(),
    }
}

/// Parses a CTCP DCC payload (the text between the `\x01` markers, already
/// stripped), e.g. `DCC CHAT chat 3232235521 1024` or
/// `DCC SEND "my file.txt" 3232235521 1024 5000`. Returns `None` if it isn't a
/// DCC offer we understand.
pub fn parse_dcc_message(payload: &str) -> Option<DccMessage> {
    let fields = split_dcc_fields(payload)?;
    if !fields.first()?.eq_ignore_ascii_case("DCC") {
        return None;
    }
    let kind = fields.get(1)?;

    if kind.eq_ignore_ascii_case("CHAT") {
        if !(fields.len() == 5 || fields.len() == 6) || !fields.get(2)?.eq_ignore_ascii_case("chat")
        {
            return None;
        }
        let ip = parse_dcc_ip(fields.get(3)?)?;
        let port = fields.get(4)?.parse::<u16>().ok()?;
        let token = fields
            .get(5)
            .map(|value| value.parse::<u64>())
            .transpose()
            .ok()?;
        if port == 0 && token.is_none() {
            return None;
        }
        return Some(DccMessage::Offer(DccOffer {
            kind: DccKind::Chat,
            filename: String::new(),
            ip,
            port,
            size: 0,
            token,
        }));
    }

    if kind.eq_ignore_ascii_case("SEND") {
        if !(5..=7).contains(&fields.len()) {
            return None;
        }
        let filename = fields.get(2)?.to_string();
        let ip = parse_dcc_ip(fields.get(3)?)?;
        let port = fields.get(4)?.parse::<u16>().ok()?;
        let size = fields
            .get(5)
            .map(|value| value.parse::<u64>())
            .transpose()
            .ok()?
            .unwrap_or(0);
        let token = fields
            .get(6)
            .map(|value| value.parse::<u64>())
            .transpose()
            .ok()?;
        if port == 0 && token.is_none() {
            return None;
        }
        return Some(DccMessage::Offer(DccOffer {
            kind: DccKind::Send,
            filename,
            ip,
            port,
            size,
            token,
        }));
    }

    if kind.eq_ignore_ascii_case("RESUME") || kind.eq_ignore_ascii_case("ACCEPT") {
        if !(fields.len() == 5 || fields.len() == 6) {
            return None;
        }
        let filename = fields.get(2)?.to_string();
        let port = fields.get(3)?.parse::<u16>().ok()?;
        let position = fields.get(4)?.parse::<u64>().ok()?;
        let token = fields
            .get(5)
            .map(|value| value.parse::<u64>())
            .transpose()
            .ok()?;
        if port == 0 && token.is_none() {
            return None;
        }
        return Some(if kind.eq_ignore_ascii_case("RESUME") {
            DccMessage::Resume {
                filename,
                port,
                position,
                token,
            }
        } else {
            DccMessage::Accept {
                filename,
                port,
                position,
                token,
            }
        });
    }

    None
}

/// Backwards-compatible offer-only parser used by existing callers/tests.
#[allow(dead_code)] // retained as the offer-only protocol API and used by tests
pub fn parse_dcc(payload: &str) -> Option<DccOffer> {
    match parse_dcc_message(payload)? {
        DccMessage::Offer(offer) => Some(offer),
        _ => None,
    }
}

/// Splits DCC fields while retaining spaces inside a quoted filename. DCC has
/// no escape syntax inside quoted names; this deliberately follows mIRC's
/// simple first-quote/last-quote convention.
fn split_dcc_fields(input: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in input.trim().chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if quoted {
        return None;
    }
    if !current.is_empty() {
        out.push(current);
    }
    Some(out)
}

/// Builds the CTCP payload for an outgoing DCC CHAT offer (caller wraps it in
/// `\x01` and a `PRIVMSG`).
#[allow(dead_code)] // wired in the DCC connect/send phase
pub fn format_chat_offer(ip: IpAddr, port: u16) -> String {
    format!("DCC CHAT chat {} {}", dcc_ip_str(ip), port)
}

pub fn format_chat_passive(ip: IpAddr, token: u64) -> String {
    format!("DCC CHAT chat {} 0 {}", dcc_ip_str(ip), token)
}

/// Builds the CTCP payload for an outgoing DCC SEND offer. Filenames containing
/// spaces are quoted.
#[allow(dead_code)] // wired in the DCC connect/send phase
pub fn format_send_offer(filename: &str, ip: IpAddr, port: u16, size: u64) -> String {
    let name = if filename.contains(' ') {
        format!("\"{filename}\"")
    } else {
        filename.to_string()
    };
    format!("DCC SEND {} {} {} {}", name, dcc_ip_str(ip), port, size)
}

pub fn format_send_passive(filename: &str, ip: IpAddr, size: u64, token: u64) -> String {
    format!("{} {}", format_send_offer(filename, ip, 0, size), token)
}

fn format_resume(
    kind: &str,
    filename: &str,
    port: u16,
    position: u64,
    token: Option<u64>,
) -> String {
    let name = if filename.contains(' ') {
        format!("\"{filename}\"")
    } else {
        filename.to_string()
    };
    match token {
        Some(token) => format!("DCC {kind} {name} {port} {position} {token}"),
        None => format!("DCC {kind} {name} {port} {position}"),
    }
}

/// A parsed `/dcc` subcommand (the part after `/dcc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DccCommand {
    /// `/dcc chat <nick>` — offer/open a direct chat.
    Chat { nick: String },
    /// `/dcc fserve <ip[:port]>` — connect to a DCC Server fileserver.
    Fserve { target: String },
    /// `/dcc send <nick> <file>` — offer a file.
    Send { nick: String, file: String },
    /// `/dcc get [nick]` — accept a pending incoming offer.
    Get { nick: Option<String> },
    /// `/dcc resume [nick]` — resume a partial incoming transfer.
    Resume { nick: Option<String> },
    /// `/dcc passive on|off` — control mIRC passive DCC offers.
    Passive { enabled: Option<bool> },
    /// `/dcc close [chat|send] [nick]` — close matching DCC session(s).
    Close {
        kind: Option<DccKind>,
        nick: Option<String>,
    },
}

/// Parses the arguments to `/dcc` (everything after the command word). Returns
/// `None` for an unknown/incomplete subcommand.
#[allow(dead_code)] // wired into the /dcc command + DCC manager next
pub fn parse_dcc_command(args: &str) -> Option<DccCommand> {
    let mut t = args.split_whitespace();
    match t.next()?.to_ascii_lowercase().as_str() {
        "chat" => {
            let first = t.next()?;
            let nick = if first.starts_with('-') {
                t.next()?
            } else {
                first
            };
            Some(DccCommand::Chat {
                nick: nick.to_string(),
            })
        }
        "fserve" => Some(DccCommand::Fserve {
            target: t.next()?.to_string(),
        }),
        "send" => {
            let first = t.next()?;
            let nick = if first.starts_with('-') {
                t.next()?
            } else {
                first
            }
            .to_string();
            let file = t.collect::<Vec<_>>().join(" ");
            (!file.is_empty()).then_some(DccCommand::Send { nick, file })
        }
        "get" | "accept" => Some(DccCommand::Get {
            nick: t.next().map(String::from),
        }),
        "resume" => Some(DccCommand::Resume {
            nick: t.next().map(String::from),
        }),
        "passive" => Some(DccCommand::Passive {
            enabled: t
                .next()
                .and_then(|v| match v.to_ascii_lowercase().as_str() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                }),
        }),
        "close" => {
            let mut kind = None;
            let mut nick = None;
            for w in t {
                match w.to_ascii_lowercase().as_str() {
                    "chat" => kind = Some(DccKind::Chat),
                    "send" => kind = Some(DccKind::Send),
                    other => nick = Some(other.to_string()),
                }
            }
            Some(DccCommand::Close { kind, nick })
        }
        _ => None,
    }
}

// ---- DCC chat connection manager ----

struct DccChat {
    /// Lines typed in the buffer, to send to the peer.
    tx: UnboundedSender<String>,
    task: tauri::async_runtime::JoinHandle<()>,
    server_id: String,
    nick: String,
    ip: String,
    opened: Instant,
    connected: Arc<AtomicBool>,
}

/// DCC networking config (for transfers across NAT): the IP to advertise to peers
/// and the listen-port range to bind, so the user can port-forward a known range.
#[derive(Default, Clone)]
pub struct DccConfig {
    /// IP advertised in offers; empty = auto-detect the local IPv4.
    pub ip: String,
    /// Listen-port range. `port_from == 0` means use an ephemeral port.
    pub port_from: u16,
    pub port_to: u16,
    /// Use mIRC's passive/reverse handshake for outgoing offers.
    pub passive: bool,
}

#[derive(Debug, Clone)]
enum RetrySpec {
    Send {
        server_id: String,
        nick: String,
        path: std::path::PathBuf,
    },
    Recv {
        server_id: String,
        nick: String,
        offer: DccOffer,
        path: std::path::PathBuf,
    },
    ServerDirect,
}

#[derive(Debug, Clone)]
pub struct DccTransferSnapshot {
    pub server_id: String,
    pub id: String,
    pub kind: String,
    pub nick: String,
    pub filename: String,
    pub path: String,
    pub ip: String,
    pub status: String,
    pub transferred: u64,
    pub size: u64,
    pub resume: u64,
    pub last_ack: u64,
    pub opened: Instant,
}

struct TransferRecord {
    snapshot: DccTransferSnapshot,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    retry: RetrySpec,
}

#[derive(Debug, Clone)]
pub struct DccChatSnapshot {
    pub nick: String,
    pub ip: String,
    pub status: String,
    pub opened: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DccKey {
    server_id: String,
    nick: String,
    kind: DccKind,
    port: u16,
    token: Option<u64>,
}

impl DccKey {
    fn new(server_id: &str, nick: &str, kind: DccKind, port: u16, token: Option<u64>) -> Self {
        Self {
            server_id: server_id.to_string(),
            nick: nick.to_ascii_lowercase(),
            kind,
            port,
            token,
        }
    }
}

struct OutgoingSend {
    xid: String,
    filename: String,
    size: u64,
    offset: Arc<AtomicU64>,
}

struct PendingResume {
    xid: String,
    server_id: String,
    nick: String,
    offer: DccOffer,
    base: String,
    path: std::path::PathBuf,
    position: u64,
}

struct PassiveEndpoint {
    owner_id: String,
    tx: oneshot::Sender<(IpAddr, u16)>,
    filename: String,
    size: u64,
}

struct DccServer {
    task: tauri::async_runtime::JoinHandle<()>,
    server_id: String,
    port: u16,
    chat: bool,
    send: bool,
    fserve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DccServerRequest {
    Chat {
        nick: String,
    },
    Fserve {
        nick: String,
    },
    Send {
        nick: String,
        size: u64,
        filename: String,
    },
    Get {
        nick: String,
        filename: String,
    },
}

fn parse_server_request(request: &str) -> Option<DccServerRequest> {
    let mut fields = request.trim().splitn(4, char::is_whitespace);
    let code = fields.next()?;
    let nick = fields.next()?.trim();
    if nick.is_empty() || nick.len() > 64 {
        return None;
    }
    match code {
        "100" => Some(DccServerRequest::Chat { nick: nick.into() }),
        "110" => Some(DccServerRequest::Fserve { nick: nick.into() }),
        "120" => {
            let size = fields.next()?.trim().parse::<u64>().ok()?;
            let filename = fields.next()?.trim();
            if size == 0 || filename.is_empty() {
                return None;
            }
            Some(DccServerRequest::Send {
                nick: nick.into(),
                size,
                filename: filename.into(),
            })
        }
        "130" => {
            let first = fields.next()?.trim();
            let second = fields.next().unwrap_or("").trim();
            let filename = if second.is_empty() {
                first.to_string()
            } else {
                format!("{first} {second}")
            };
            if filename.is_empty() {
                return None;
            }
            Some(DccServerRequest::Get {
                nick: nick.into(),
                filename,
            })
        }
        _ => None,
    }
}

const OFFER_TIMEOUT: Duration = Duration::from_secs(120);
const IO_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TRANSFER_HISTORY: usize = 256;

/// Manages active DCC chat sessions, keyed by their `=nick` buffer id.
#[derive(Default)]
pub struct DccManager {
    chats: Mutex<HashMap<String, DccChat>>,
    fserves: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
    config: Mutex<DccConfig>,
    transfers: Mutex<HashMap<String, TransferRecord>>,
    outgoing_sends: Mutex<HashMap<DccKey, OutgoingSend>>,
    pending_resumes: Mutex<HashMap<DccKey, PendingResume>>,
    passive_endpoints: Mutex<HashMap<DccKey, PassiveEndpoint>>,
    incoming_offers: Mutex<Vec<(String, String, DccOffer)>>,
    server: Mutex<Option<DccServer>>,
    server_gets: Mutex<HashMap<(IpAddr, String), (String, std::path::PathBuf, Instant)>>,
}

impl DccManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the advertised IP and listen-port range used for new offers.
    pub fn configure(&self, ip: String, port_from: u16, port_to: u16, passive: bool) {
        *self.config.lock().unwrap() = DccConfig {
            ip,
            port_from,
            port_to,
            passive,
        };
    }

    pub fn set_passive(&self, passive: bool) {
        self.config.lock().unwrap().passive = passive;
    }

    pub fn passive(&self) -> bool {
        self.config.lock().unwrap().passive
    }

    /// Starts or reconfigures mIRC's direct DCC Server protocol listener.
    pub fn configure_server(
        &self,
        app: AppHandle,
        server_id: String,
        enabled: bool,
        port: u16,
        chat: bool,
        send: bool,
        fserve: bool,
    ) -> Result<(), String> {
        let previous = self.server.lock().unwrap().take();
        let had_server = previous.is_some();
        if let Some(old) = previous {
            old.task.abort();
        }
        if !enabled {
            if had_server {
                dcc_notice(&app, &server_id, "DCC server is off");
            }
            return Ok(());
        }
        let listener = match std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
            Ok(listener) => listener,
            Err(error) => {
                let message = format!("cannot listen for DCC Server on port {port}: {error}");
                dcc_notice(&app, &server_id, &format!("DCC server: {message}"));
                return Err(message);
            }
        };
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let actual_port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let listener = TcpListener::from_std(listener).map_err(|e| e.to_string())?;
        let app2 = app.clone();
        let sid = server_id.clone();
        let task = tauri::async_runtime::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let app3 = app2.clone();
                        let sid2 = sid.clone();
                        tauri::async_runtime::spawn(async move {
                            handle_server_connection(
                                app3,
                                sid2,
                                stream,
                                peer.ip(),
                                chat,
                                send,
                                fserve,
                            )
                            .await;
                        });
                    }
                    Err(error) => {
                        dcc_notice(&app2, &sid, &format!("DCC server stopped: {error}"));
                        break;
                    }
                }
            }
        });
        *self.server.lock().unwrap() = Some(DccServer {
            task,
            server_id: server_id.clone(),
            port: actual_port,
            chat,
            send,
            fserve,
        });
        dcc_notice(
            &app,
            &server_id,
            &format!(
                "DCC server listening on port {actual_port} ({})",
                server_services(chat, send, fserve)
            ),
        );
        Ok(())
    }

    pub fn server_snapshot(&self) -> Option<(String, u16, bool, bool, bool)> {
        self.server.lock().unwrap().as_ref().map(|server| {
            (
                server.server_id.clone(),
                server.port,
                server.chat,
                server.send,
                server.fserve,
            )
        })
    }

    fn accept_server_chat(
        &self,
        app: AppHandle,
        server_id: String,
        nick: String,
        ip: IpAddr,
        stream: TcpStream,
    ) -> Result<(), String> {
        let id = format!("={nick}");
        let key = chat_key(&server_id, &id);
        if self.chats.lock().unwrap().contains_key(&key) {
            return Err(format!("DCC CHAT with {nick} is already open"));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let connected = Arc::new(AtomicBool::new(true));
        let app2 = app.clone();
        let sid = server_id.clone();
        let id2 = id.clone();
        let who = nick.clone();
        let connected2 = connected.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            run_chat(app2, sid, id2, who, stream, rx, connected2).await
        });
        let _ = app.emit(
            IRC_EVENT,
            UiEvent::DccChatOpen {
                server_id: server_id.clone(),
                id: id.clone(),
                nick: nick.clone(),
                outgoing: false,
            },
        );
        self.chats.lock().unwrap().insert(
            key,
            DccChat {
                tx,
                task,
                server_id,
                nick,
                ip: ip.to_string(),
                opened: Instant::now(),
                connected,
            },
        );
        let _ = start_tx.send(());
        Ok(())
    }

    /// `/dcc chat <nick>` — listen on an ephemeral port, send the peer a CHAT
    /// offer over IRC, and accept their connection.
    pub fn chat(&self, app: AppHandle, server_id: String, nick: String) -> Result<(), String> {
        if let Some((ip, port)) = parse_server_target(&nick) {
            return self.chat_server(app, server_id, nick, ip, port, 100);
        }
        let cfg = self.config.lock().unwrap().clone();
        let ip = resolve_dcc_ip(&cfg.ip)?;
        let id = format!("={nick}");
        let key = chat_key(&server_id, &id);
        if self.chats.lock().unwrap().contains_key(&key) {
            return Err(format!("DCC CHAT with {nick} is already open"));
        }
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let connected = Arc::new(AtomicBool::new(false));

        let (task, start) = if cfg.passive {
            let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
            let (endpoint_tx, endpoint_rx) = oneshot::channel();
            let endpoint_key = DccKey::new(&server_id, &nick, DccKind::Chat, 0, Some(token));
            self.passive_endpoints.lock().unwrap().insert(
                endpoint_key.clone(),
                PassiveEndpoint {
                    owner_id: key.clone(),
                    tx: endpoint_tx,
                    filename: String::new(),
                    size: 0,
                },
            );
            if let Err(error) = send_ctcp(&app, &server_id, &nick, &format_chat_passive(ip, token))
            {
                self.passive_endpoints.lock().unwrap().remove(&endpoint_key);
                return Err(error);
            }
            let app2 = app.clone();
            let sid = server_id.clone();
            let id2 = id.clone();
            let nick2 = nick.clone();
            let connected2 = connected.clone();
            let (start_tx, start_rx) = oneshot::channel();
            let task = tauri::async_runtime::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                match timeout(OFFER_TIMEOUT, endpoint_rx).await {
                    Ok(Ok((peer_ip, peer_port))) => {
                        match timeout(IO_TIMEOUT, TcpStream::connect((peer_ip, peer_port))).await {
                            Ok(Ok(stream)) => {
                                run_chat(app2.clone(), sid, id2, nick2, stream, rx, connected2)
                                    .await
                            }
                            _ => emit_closed_and_script(&app2, &sid, &id2, &nick2),
                        }
                    }
                    _ => emit_closed_and_script(&app2, &sid, &id2, &nick2),
                }
                if let Some(manager) = app2.try_state::<DccManager>() {
                    manager
                        .passive_endpoints
                        .lock()
                        .unwrap()
                        .remove(&endpoint_key);
                }
            });
            (task, start_tx)
        } else {
            let (listener, port) =
                bind_in_range(ip, cfg.port_from, cfg.port_to).ok_or("no free DCC port in range")?;
            send_ctcp(&app, &server_id, &nick, &format_chat_offer(ip, port))?;
            let app2 = app.clone();
            let sid = server_id.clone();
            let id2 = id.clone();
            let nick2 = nick.clone();
            let connected2 = connected.clone();
            let (start_tx, start_rx) = oneshot::channel();
            let task = tauri::async_runtime::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let listener = match TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(_) => return emit_closed_and_script(&app2, &sid, &id2, &nick2),
                };
                match timeout(OFFER_TIMEOUT, listener.accept()).await {
                    Ok(Ok((stream, _))) => {
                        run_chat(app2, sid, id2, nick2, stream, rx, connected2).await
                    }
                    _ => emit_closed_and_script(&app2, &sid, &id2, &nick2),
                }
            });
            (task, start_tx)
        };

        let _ = app.emit(
            IRC_EVENT,
            UiEvent::DccChatOpen {
                server_id: server_id.clone(),
                id: id.clone(),
                nick: nick.clone(),
                outgoing: true,
            },
        );
        self.chats.lock().unwrap().insert(
            key,
            DccChat {
                tx,
                task,
                server_id,
                nick,
                ip: ip.to_string(),
                opened: Instant::now(),
                connected,
            },
        );
        let _ = start.send(());
        Ok(())
    }

    fn chat_server(
        &self,
        app: AppHandle,
        server_id: String,
        target: String,
        ip: IpAddr,
        port: u16,
        request_code: u16,
    ) -> Result<(), String> {
        let id = format!("={target}");
        let key = chat_key(&server_id, &id);
        if self.chats.lock().unwrap().contains_key(&key) {
            return Err(format!("DCC CHAT with {target} is already open"));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let connected = Arc::new(AtomicBool::new(false));
        let connected2 = connected.clone();
        let app2 = app.clone();
        let sid = server_id.clone();
        let id2 = id.clone();
        let who = target.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let result = async {
                let mut stream = timeout(IO_TIMEOUT, TcpStream::connect((ip, port)))
                    .await
                    .map_err(|_| "DCC Server connection timed out".to_string())?
                    .map_err(|error| error.to_string())?;
                let nick = server_nick(&app2, &sid);
                server_line(&mut stream, &format!("{request_code} {nick}"))
                    .await
                    .map_err(|e| e.to_string())?;
                let reply = read_server_request(&mut stream)
                    .await
                    .map_err(|e| e.to_string())?;
                let expected = if request_code == 110 { "111 " } else { "101 " };
                if !reply.starts_with(expected) {
                    return Err(format!("DCC Server rejected request: {reply}"));
                }
                run_chat(
                    app2.clone(),
                    sid.clone(),
                    id2.clone(),
                    who.clone(),
                    stream,
                    rx,
                    connected2,
                )
                .await;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                dcc_notice(&app2, &sid, &error);
                emit_closed_and_script(&app2, &sid, &id2, &who);
            }
        });
        let _ = app.emit(
            IRC_EVENT,
            UiEvent::DccChatOpen {
                server_id: server_id.clone(),
                id: id.clone(),
                nick: target.clone(),
                outgoing: true,
            },
        );
        self.chats.lock().unwrap().insert(
            key,
            DccChat {
                tx,
                task,
                server_id,
                nick: target,
                ip: ip.to_string(),
                opened: Instant::now(),
                connected,
            },
        );
        let _ = start_tx.send(());
        Ok(())
    }

    /// Accept an incoming offer by connecting to `ip:port`.
    pub fn accept(
        &self,
        app: AppHandle,
        server_id: String,
        nick: String,
        ip: IpAddr,
        port: u16,
        token: Option<u64>,
    ) -> Result<(), String> {
        let id = format!("={nick}");
        let key = chat_key(&server_id, &id);
        if self.chats.lock().unwrap().contains_key(&key) {
            return Err(format!("DCC CHAT with {nick} is already open"));
        }
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let connected = Arc::new(AtomicBool::new(false));
        let cfg = self.config.lock().unwrap().clone();
        let (task, start) = if port == 0 {
            let token = token.ok_or("passive DCC CHAT is missing its token")?;
            let advertised = resolve_dcc_ip(&cfg.ip)?;
            let (listener, reply_port) = bind_in_range(advertised, cfg.port_from, cfg.port_to)
                .ok_or("no free DCC port in range")?;
            send_ctcp(
                &app,
                &server_id,
                &nick,
                &format!("{} {token}", format_chat_offer(advertised, reply_port)),
            )?;
            let app2 = app.clone();
            let sid = server_id.clone();
            let id2 = id.clone();
            let nick2 = nick.clone();
            let connected2 = connected.clone();
            let (start_tx, start_rx) = oneshot::channel();
            let task = tauri::async_runtime::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let listener = match TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(_) => return emit_closed_and_script(&app2, &sid, &id2, &nick2),
                };
                match timeout(OFFER_TIMEOUT, listener.accept()).await {
                    Ok(Ok((stream, _))) => {
                        run_chat(app2, sid, id2, nick2, stream, rx, connected2).await
                    }
                    _ => emit_closed_and_script(&app2, &sid, &id2, &nick2),
                }
            });
            (task, start_tx)
        } else {
            let app2 = app.clone();
            let sid = server_id.clone();
            let id2 = id.clone();
            let nick2 = nick.clone();
            let connected2 = connected.clone();
            let (start_tx, start_rx) = oneshot::channel();
            let task = tauri::async_runtime::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                match timeout(IO_TIMEOUT, TcpStream::connect((ip, port))).await {
                    Ok(Ok(stream)) => run_chat(app2, sid, id2, nick2, stream, rx, connected2).await,
                    _ => emit_closed_and_script(&app2, &sid, &id2, &nick2),
                }
            });
            (task, start_tx)
        };
        self.forget_offer(&server_id, &nick, DccKind::Chat, token);
        let _ = app.emit(
            IRC_EVENT,
            UiEvent::DccChatOpen {
                server_id: server_id.clone(),
                id: id.clone(),
                nick: nick.clone(),
                outgoing: false,
            },
        );
        self.chats.lock().unwrap().insert(
            key,
            DccChat {
                tx,
                task,
                server_id,
                nick,
                ip: ip.to_string(),
                opened: Instant::now(),
                connected,
            },
        );
        let _ = start.send(());
        Ok(())
    }

    /// Send a typed line to a DCC chat peer.
    pub fn send(&self, server_id: &str, id: &str, text: String) -> Result<(), String> {
        let chats = self.chats.lock().unwrap();
        let chat = chats
            .get(&chat_key(server_id, id))
            .ok_or("DCC chat is not open")?;
        if !chat.connected.load(Ordering::Acquire) {
            return Err("DCC chat is still connecting".into());
        }
        chat.tx
            .send(text)
            .map_err(|_| "DCC chat has closed".to_string())
    }

    /// Close a DCC chat session.
    pub fn close(&self, app: &AppHandle, server_id: &str, id: &str) {
        let key = chat_key(server_id, id);
        if let Some(c) = self.chats.lock().unwrap().remove(&key) {
            c.task.abort();
            self.passive_endpoints
                .lock()
                .unwrap()
                .retain(|_, endpoint| endpoint.owner_id != key);
            emit_closed(app, &c.server_id, id);
            fire_chat_event(app, &c.server_id, "CLOSE", &c.nick, "");
        }
    }

    /// `/fserve <nick> <maxgets> <homedir> [welcome]` — offer a DCC chat
    /// file-server session rooted inside scriptdata.
    pub fn fserve(
        &self,
        app: AppHandle,
        server_id: String,
        nick: String,
        max_gets: usize,
        sandbox: std::path::PathBuf,
        home: std::path::PathBuf,
        welcome: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        if max_gets == 0 {
            return Err("fserve maxgets must be greater than zero".into());
        }
        let sandbox = canonical_fserve_root(&sandbox)?;
        let home = canonical_fserve_path(&sandbox, &home)?;
        if !home.is_dir() {
            return Err("fserve home must be a directory".into());
        }
        let welcome_text = welcome
            .map(|path| {
                let path = canonical_fserve_path(&sandbox, &path)?;
                std::fs::read_to_string(path).map_err(|error| error.to_string())
            })
            .transpose()?;
        if let Some((ip, port)) = parse_server_target(&nick) {
            return self.chat_server(app, server_id, nick, ip, port, 110);
        }
        let key = chat_key(&server_id, &format!("fserve:{nick}"));
        if self.fserves.lock().unwrap().contains_key(&key) {
            return Err(format!("fserve for {nick} is already waiting"));
        }
        let cfg = self.config.lock().unwrap().clone();
        let ip = resolve_dcc_ip(&cfg.ip)?;
        let (listener, port) =
            bind_in_range(ip, cfg.port_from, cfg.port_to).ok_or("no free DCC port in range")?;
        send_ctcp(&app, &server_id, &nick, &format_chat_offer(ip, port))?;
        let app2 = app.clone();
        let sid = server_id.clone();
        let who = nick.clone();
        let key2 = key.clone();
        let task = tauri::async_runtime::spawn(async move {
            let result = async {
                let listener =
                    TcpListener::from_std(listener).map_err(|error| error.to_string())?;
                let (stream, _) = timeout(OFFER_TIMEOUT, listener.accept())
                    .await
                    .map_err(|_| "fserve offer timed out".to_string())?
                    .map_err(|error| error.to_string())?;
                run_fserve(
                    app2.clone(),
                    sid.clone(),
                    who.clone(),
                    stream,
                    home,
                    max_gets,
                    welcome_text,
                    None,
                )
                .await
            }
            .await;
            if let Err(error) = result {
                dcc_notice(&app2, &sid, &format!("DCC fserve for {who}: {error}"));
            }
            if let Some(manager) = app2.try_state::<DccManager>() {
                manager.fserves.lock().unwrap().remove(&key2);
            }
        });
        self.fserves.lock().unwrap().insert(key, task);
        dcc_notice(&app, &server_id, &format!("DCC fserve offered to {nick}"));
        Ok(())
    }

    /// Records or consumes an inbound DCC negotiation. Returns true when the
    /// message was an internal response and should not be shown as a new offer.
    pub fn handle_protocol(
        &self,
        app: &AppHandle,
        server_id: &str,
        nick: &str,
        message: &DccMessage,
    ) -> bool {
        match message {
            DccMessage::Offer(offer) => {
                if offer.port != 0 {
                    if let Some(token) = offer.token {
                        let key = DccKey::new(server_id, nick, offer.kind, 0, Some(token));
                        let endpoint = self.passive_endpoints.lock().unwrap().remove(&key);
                        if let Some(endpoint) = endpoint {
                            let valid = offer.kind == DccKind::Chat
                                || (endpoint.filename.eq_ignore_ascii_case(&offer.filename)
                                    && endpoint.size == offer.size);
                            if valid {
                                let _ = endpoint.tx.send((offer.ip, offer.port));
                            }
                            return true;
                        }
                    }
                }
                let mut offers = self.incoming_offers.lock().unwrap();
                offers.retain(|(sid, who, old)| {
                    !(sid == server_id && who.eq_ignore_ascii_case(nick) && old.kind == offer.kind)
                });
                offers.push((server_id.to_string(), nick.to_string(), offer.clone()));
                false
            }
            DccMessage::Resume {
                filename,
                port,
                position,
                token,
            } => {
                let key = DccKey::new(server_id, nick, DccKind::Send, *port, *token);
                let mut sends = self.outgoing_sends.lock().unwrap();
                let Some(send) = sends.get_mut(&key) else {
                    return false;
                };
                if !send.filename.eq_ignore_ascii_case(filename) || *position > send.size {
                    return true;
                }
                send.offset.store(*position, Ordering::Release);
                let xid = send.xid.clone();
                drop(sends);
                if let Err(error) = send_ctcp(
                    app,
                    server_id,
                    nick,
                    &format_resume("ACCEPT", filename, *port, *position, *token),
                ) {
                    self.abort_transfer_task(&xid);
                    fail_transfer(app, &xid, &error);
                }
                true
            }
            DccMessage::Accept {
                filename,
                port,
                position,
                token,
            } => {
                let key = DccKey::new(server_id, nick, DccKind::Send, *port, *token);
                let pending = self.pending_resumes.lock().unwrap().remove(&key);
                let Some(pending) = pending else { return false };
                if !pending.base.eq_ignore_ascii_case(filename) || pending.position != *position {
                    self.abort_transfer_task(&pending.xid);
                    fail_transfer(app, &pending.xid, "invalid DCC ACCEPT response");
                    return true;
                }
                self.spawn_receive_transport(app.clone(), pending);
                true
            }
        }
    }

    /// Accept an incoming DCC SEND offer: connect, download into the `dcc/`
    /// folder, and acknowledge bytes as they arrive.
    pub fn recv_file(
        &self,
        app: AppHandle,
        server_id: String,
        nick: String,
        filename: String,
        ip: IpAddr,
        port: u16,
        size: u64,
        token: Option<u64>,
        resume: bool,
    ) -> Result<(), String> {
        self.forget_offer(&server_id, &nick, DccKind::Send, token);
        self.recv_offer(
            app,
            server_id,
            nick,
            DccOffer {
                kind: DccKind::Send,
                filename,
                ip,
                port,
                size,
                token,
            },
            resume,
            None,
        )
    }

    fn forget_offer(&self, server_id: &str, nick: &str, kind: DccKind, token: Option<u64>) {
        self.incoming_offers
            .lock()
            .unwrap()
            .retain(|(sid, who, offer)| {
                !(sid == server_id
                    && who.eq_ignore_ascii_case(nick)
                    && offer.kind == kind
                    && offer.token == token)
            });
    }

    fn recv_offer(
        &self,
        app: AppHandle,
        server_id: String,
        nick: String,
        offer: DccOffer,
        resume: bool,
        resume_path: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        let dir = crate::storage::dcc_dir(&app)?;
        let base = safe_basename(&offer.filename);
        let exact = dir.join(&base);
        let (path, position) = if resume || resume_path.is_some() {
            let path = resume_path.unwrap_or(exact);
            let n = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if n == 0 {
                return Err(format!("there is no partial \"{base}\" to resume"));
            }
            if offer.size > 0 && n >= offer.size {
                return Err(format!("\"{base}\" is already complete"));
            }
            (path, n)
        } else {
            (reserve_destination(&dir, &base)?, 0)
        };
        let xid = format!("recv-{}", NEXT_XFER.fetch_add(1, Ordering::Relaxed));
        self.register_transfer(
            &app,
            DccTransferSnapshot {
                server_id: server_id.clone(),
                id: xid.clone(),
                kind: "recv".into(),
                nick: nick.clone(),
                filename: base.clone(),
                path: path.to_string_lossy().into_owned(),
                ip: offer.ip.to_string(),
                status: "waiting".into(),
                transferred: position,
                size: offer.size,
                resume: position,
                last_ack: 0,
                opened: Instant::now(),
            },
            RetrySpec::Recv {
                server_id: server_id.clone(),
                nick: nick.clone(),
                offer: offer.clone(),
                path: path.clone(),
            },
        );
        let pending = PendingResume {
            xid: xid.clone(),
            server_id: server_id.clone(),
            nick: nick.clone(),
            offer: offer.clone(),
            base: base.clone(),
            path,
            position,
        };
        if position == 0 {
            self.spawn_receive_transport(app, pending);
        } else {
            let key = DccKey::new(&server_id, &nick, DccKind::Send, offer.port, offer.token);
            self.pending_resumes
                .lock()
                .unwrap()
                .insert(key.clone(), pending);
            if let Err(error) = send_ctcp(
                &app,
                &server_id,
                &nick,
                &format_resume("RESUME", &base, offer.port, position, offer.token),
            ) {
                self.pending_resumes.lock().unwrap().remove(&key);
                fail_transfer(&app, &xid, &error);
                return Err(error);
            }
            let app2 = app.clone();
            let xid2 = xid.clone();
            let task = tauri::async_runtime::spawn(async move {
                tokio::time::sleep(OFFER_TIMEOUT).await;
                let still_pending = app2
                    .try_state::<DccManager>()
                    .and_then(|m| m.pending_resumes.lock().unwrap().remove(&key))
                    .is_some();
                if still_pending {
                    fail_transfer(&app2, &xid2, "DCC resume negotiation timed out");
                }
            });
            self.set_transfer_task(&xid, task);
        }
        Ok(())
    }

    fn spawn_receive_transport(&self, app: AppHandle, pending: PendingResume) {
        self.abort_transfer_task(&pending.xid);
        let cfg = self.config.lock().unwrap().clone();
        let xid = pending.xid.clone();
        let task = if pending.offer.port == 0 {
            let Some(token) = pending.offer.token else {
                return fail_transfer(&app, &xid, "passive DCC SEND is missing its token");
            };
            let advertised = match resolve_dcc_ip(&cfg.ip) {
                Ok(ip) => ip,
                Err(e) => return fail_transfer(&app, &xid, &e),
            };
            let Some((listener, port)) = bind_in_range(advertised, cfg.port_from, cfg.port_to)
            else {
                return fail_transfer(&app, &xid, "no free DCC port in range");
            };
            if let Err(error) = send_ctcp(
                &app,
                &pending.server_id,
                &pending.nick,
                &format!(
                    "{} {token}",
                    format_send_offer(&pending.base, advertised, port, pending.offer.size)
                ),
            ) {
                return fail_transfer(&app, &xid, &error);
            }
            tauri::async_runtime::spawn(run_receive(app.clone(), pending, async move {
                let listener = TcpListener::from_std(listener)?;
                timeout(OFFER_TIMEOUT, listener.accept())
                    .await
                    .map_err(|_| timeout_error("DCC peer did not connect"))?
                    .map(|(stream, _)| stream)
            }))
        } else {
            let ip = pending.offer.ip;
            let port = pending.offer.port;
            tauri::async_runtime::spawn(run_receive(app.clone(), pending, async move {
                timeout(IO_TIMEOUT, TcpStream::connect((ip, port)))
                    .await
                    .map_err(|_| timeout_error("DCC connection timed out"))?
            }))
        };
        self.set_transfer_task(&xid, task);
    }

    /// `/dcc send <nick> <file>` — offer a file, listen, and stream it on connect.
    pub fn send_file(
        &self,
        app: AppHandle,
        server_id: String,
        nick: String,
        path: std::path::PathBuf,
    ) -> Result<(), String> {
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("not a file".to_string());
        }
        let size = meta.len();
        let base = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .ok_or("bad filename")?;
        if let Some((ip, port)) = parse_server_target(&nick) {
            return self.send_file_to_server(app, server_id, nick, ip, port, path, base, size);
        }
        let cfg = self.config.lock().unwrap().clone();
        let ip = resolve_dcc_ip(&cfg.ip)?;
        let xid = format!("send-{}", NEXT_XFER.fetch_add(1, Ordering::Relaxed));
        let wire_name = base.replace(' ', "_");
        let offset = Arc::new(AtomicU64::new(0));
        let (port, token, task, start) = if cfg.passive {
            let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
            let (endpoint_tx, endpoint_rx) = oneshot::channel();
            let endpoint_key = DccKey::new(&server_id, &nick, DccKind::Send, 0, Some(token));
            self.passive_endpoints.lock().unwrap().insert(
                endpoint_key.clone(),
                PassiveEndpoint {
                    owner_id: xid.clone(),
                    tx: endpoint_tx,
                    filename: wire_name.clone(),
                    size,
                },
            );
            if let Err(error) = send_ctcp(
                &app,
                &server_id,
                &nick,
                &format_send_passive(&wire_name, ip, size, token),
            ) {
                self.passive_endpoints.lock().unwrap().remove(&endpoint_key);
                return Err(error);
            }
            let app2 = app.clone();
            let sid = server_id.clone();
            let nick2 = nick.clone();
            let base2 = base.clone();
            let path2 = path.clone();
            let xid2 = xid.clone();
            let offset2 = offset.clone();
            let (start_tx, start_rx) = oneshot::channel();
            let task = tauri::async_runtime::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let result = async {
                    let (peer_ip, peer_port) = timeout(OFFER_TIMEOUT, endpoint_rx)
                        .await
                        .map_err(|_| timeout_error("DCC offer timed out"))?
                        .map_err(|_| timeout_error("DCC offer was cancelled"))?;
                    let stream = timeout(IO_TIMEOUT, TcpStream::connect((peer_ip, peer_port)))
                        .await
                        .map_err(|_| timeout_error("DCC connection timed out"))??;
                    let start = offset2.load(Ordering::Acquire);
                    mark_transfer_active(&app2, &xid2, start);
                    send_into(
                        &app2, &sid, &xid2, &nick2, &base2, &path2, stream, size, start,
                    )
                    .await
                }
                .await;
                finish_transfer(&app2, &xid2, result);
                cleanup_outgoing(&app2, &sid, &nick2, 0, Some(token));
            });
            (0, Some(token), task, start_tx)
        } else {
            let (listener, port) =
                bind_in_range(ip, cfg.port_from, cfg.port_to).ok_or("no free DCC port in range")?;
            send_ctcp(
                &app,
                &server_id,
                &nick,
                &format_send_offer(&wire_name, ip, port, size),
            )?;
            let app2 = app.clone();
            let sid = server_id.clone();
            let nick2 = nick.clone();
            let base2 = base.clone();
            let path2 = path.clone();
            let xid2 = xid.clone();
            let offset2 = offset.clone();
            let (start_tx, start_rx) = oneshot::channel();
            let task = tauri::async_runtime::spawn(async move {
                if start_rx.await.is_err() {
                    return;
                }
                let result = async {
                    let listener = TcpListener::from_std(listener)?;
                    let (stream, _) = timeout(OFFER_TIMEOUT, listener.accept())
                        .await
                        .map_err(|_| timeout_error("DCC offer timed out"))??;
                    let start = offset2.load(Ordering::Acquire);
                    mark_transfer_active(&app2, &xid2, start);
                    send_into(
                        &app2, &sid, &xid2, &nick2, &base2, &path2, stream, size, start,
                    )
                    .await
                }
                .await;
                finish_transfer(&app2, &xid2, result);
                cleanup_outgoing(&app2, &sid, &nick2, port, None);
            });
            (port, None, task, start_tx)
        };

        self.outgoing_sends.lock().unwrap().insert(
            DccKey::new(&server_id, &nick, DccKind::Send, port, token),
            OutgoingSend {
                xid: xid.clone(),
                filename: wire_name,
                size,
                offset,
            },
        );
        self.register_transfer(
            &app,
            DccTransferSnapshot {
                server_id: server_id.clone(),
                id: xid.clone(),
                kind: "send".into(),
                nick: nick.clone(),
                filename: base,
                path: path.to_string_lossy().into_owned(),
                ip: ip.to_string(),
                status: "waiting".into(),
                transferred: 0,
                size,
                resume: 0,
                last_ack: 0,
                opened: Instant::now(),
            },
            RetrySpec::Send {
                server_id,
                nick,
                path,
            },
        );
        self.set_transfer_task(&xid, task);
        let _ = start.send(());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn send_file_to_server(
        &self,
        app: AppHandle,
        server_id: String,
        target: String,
        ip: IpAddr,
        port: u16,
        path: std::path::PathBuf,
        base: String,
        size: u64,
    ) -> Result<(), String> {
        let xid = format!("send-{}", NEXT_XFER.fetch_add(1, Ordering::Relaxed));
        let app2 = app.clone();
        let sid = server_id.clone();
        let who = target.clone();
        let path2 = path.clone();
        let base2 = base.clone();
        let xid2 = xid.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let result = async {
                let mut stream = timeout(IO_TIMEOUT, TcpStream::connect((ip, port)))
                    .await
                    .map_err(|_| timeout_error("DCC Server connection timed out"))??;
                let nick = server_nick(&app2, &sid);
                server_line(
                    &mut stream,
                    &format!("120 {nick} {size} {}", base2.replace(['\r', '\n'], "_")),
                )
                .await?;
                let reply = read_server_request(&mut stream).await?;
                let mut fields = reply.split_whitespace();
                if fields.next() != Some("121") {
                    return Err(std::io::Error::other(format!(
                        "DCC Server rejected send: {reply}"
                    )));
                }
                let _remote_nick = fields.next();
                let offset = fields
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|offset| *offset <= size)
                    .ok_or_else(|| std::io::Error::other("invalid DCC Server resume position"))?;
                mark_transfer_active(&app2, &xid2, offset);
                send_into(
                    &app2, &sid, &xid2, &who, &base2, &path2, stream, size, offset,
                )
                .await
            }
            .await;
            finish_transfer(&app2, &xid2, result);
        });
        self.register_transfer(
            &app,
            DccTransferSnapshot {
                server_id: server_id.clone(),
                id: xid.clone(),
                kind: "send".into(),
                nick: target.clone(),
                filename: base,
                path: path.to_string_lossy().into_owned(),
                ip: ip.to_string(),
                status: "waiting".into(),
                transferred: 0,
                size,
                resume: 0,
                last_ack: 0,
                opened: Instant::now(),
            },
            RetrySpec::Send {
                server_id,
                nick: target,
                path,
            },
        );
        self.set_transfer_task(&xid, task);
        let _ = start_tx.send(());
        Ok(())
    }

    fn register_transfer(&self, app: &AppHandle, snapshot: DccTransferSnapshot, retry: RetrySpec) {
        let xid = snapshot.id.clone();
        let mut records = self.transfers.lock().unwrap();
        while records.len() >= MAX_TRANSFER_HISTORY {
            let oldest = records
                .iter()
                .filter(|(_, record)| {
                    !matches!(record.snapshot.status.as_str(), "active" | "waiting")
                })
                .min_by_key(|(_, record)| record.snapshot.opened)
                .map(|(id, _)| id.clone());
            let Some(oldest) = oldest else { break };
            records.remove(&oldest);
        }
        records.insert(
            xid.clone(),
            TransferRecord {
                snapshot: snapshot.clone(),
                task: None,
                retry,
            },
        );
        drop(records);
        emit_transfer_snapshot(app, &snapshot);
    }

    fn set_transfer_task(&self, id: &str, task: tauri::async_runtime::JoinHandle<()>) {
        let mut records = self.transfers.lock().unwrap();
        if let Some(record) = records
            .get_mut(id)
            .filter(|record| matches!(record.snapshot.status.as_str(), "active" | "waiting"))
        {
            record.task = Some(task);
        } else {
            task.abort();
        }
    }

    fn abort_transfer_task(&self, id: &str) {
        if let Some(task) = self
            .transfers
            .lock()
            .unwrap()
            .get_mut(id)
            .and_then(|record| record.task.take())
        {
            task.abort();
        }
    }

    pub fn cancel_transfer(&self, app: &AppHandle, id: &str) -> Result<(), String> {
        let mut records = self.transfers.lock().unwrap();
        let record = records.get_mut(id).ok_or("no such DCC transfer")?;
        if !matches!(record.snapshot.status.as_str(), "active" | "waiting") {
            return Ok(());
        }
        if let Some(task) = record.task.take() {
            task.abort();
        }
        record.snapshot.status = "cancelled".into();
        let snapshot = record.snapshot.clone();
        drop(records);
        self.pending_resumes
            .lock()
            .unwrap()
            .retain(|_, pending| pending.xid != id);
        self.outgoing_sends
            .lock()
            .unwrap()
            .retain(|_, send| send.xid != id);
        self.passive_endpoints
            .lock()
            .unwrap()
            .retain(|_, endpoint| endpoint.owner_id != id);
        emit_transfer_snapshot(app, &snapshot);
        fire_file_event(app, &snapshot, false);
        Ok(())
    }

    pub fn retry_transfer(&self, app: AppHandle, id: &str) -> Result<(), String> {
        let records = self.transfers.lock().unwrap();
        let record = records.get(id).ok_or("no such DCC transfer")?;
        if !matches!(record.snapshot.status.as_str(), "error" | "cancelled") {
            return Err("only failed or cancelled DCC transfers can be retried".into());
        }
        let retry = record.retry.clone();
        drop(records);
        match retry {
            RetrySpec::Send {
                server_id,
                nick,
                path,
            } => self.send_file(app, server_id, nick, path),
            RetrySpec::Recv {
                server_id,
                nick,
                offer,
                path,
            } => {
                if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) == 0 {
                    let _ = std::fs::remove_file(path);
                    self.recv_offer(app, server_id, nick, offer, false, None)
                } else {
                    self.recv_offer(app, server_id, nick, offer, true, Some(path))
                }
            }
            RetrySpec::ServerDirect => {
                Err("DCC Server receives must be retried by the peer".into())
            }
        }
    }

    pub fn accept_pending(
        &self,
        app: AppHandle,
        server_id: &str,
        nick: Option<&str>,
        resume: bool,
    ) -> Result<(), String> {
        let mut offers = self.incoming_offers.lock().unwrap();
        let pos = offers
            .iter()
            .rposition(|(sid, who, _)| {
                sid == server_id && nick.is_none_or(|wanted| who.eq_ignore_ascii_case(wanted))
            })
            .ok_or("no matching DCC offer")?;
        let (_, who, offer) = offers.remove(pos);
        drop(offers);
        match offer.kind {
            DccKind::Chat => self.accept(
                app,
                server_id.to_string(),
                who,
                offer.ip,
                offer.port,
                offer.token,
            ),
            DccKind::Send => self.recv_offer(app, server_id.to_string(), who, offer, resume, None),
        }
    }

    pub fn run_script_command(
        &self,
        app: AppHandle,
        server_id: &str,
        args: &str,
        data_dir: &std::path::Path,
    ) -> Result<(), String> {
        let command = parse_dcc_command(args).ok_or("invalid /dcc command")?;
        match command {
            DccCommand::Chat { nick } => self.chat(app, server_id.to_string(), nick),
            DccCommand::Fserve { target } => {
                let (ip, port) =
                    parse_server_target(&target).ok_or("DCC fserve requires an IP address")?;
                self.chat_server(app, server_id.to_string(), target, ip, port, 110)
            }
            DccCommand::Send { nick, file } => {
                // Script file access follows jIRC's intentional sandbox: a
                // remote script may send a file from scriptdata, never an
                // arbitrary host path supplied by untrusted IRC input.
                let path = data_dir.join(safe_basename(&file));
                self.send_file(app, server_id.to_string(), nick, path)
            }
            DccCommand::Get { nick } => self.accept_pending(app, server_id, nick.as_deref(), false),
            DccCommand::Resume { nick } => {
                self.accept_pending(app, server_id, nick.as_deref(), true)
            }
            DccCommand::Passive { enabled } => {
                if let Some(enabled) = enabled {
                    self.set_passive(enabled);
                }
                dcc_notice(
                    &app,
                    server_id,
                    &format!(
                        "DCC passive is {}",
                        if self.passive() { "on" } else { "off" }
                    ),
                );
                Ok(())
            }
            DccCommand::Close { kind, nick } => {
                if kind != Some(DccKind::Send) {
                    if let Some(nick) = nick.as_deref() {
                        self.close(&app, server_id, &format!("={nick}"));
                    }
                }
                if kind != Some(DccKind::Chat) {
                    let ids: Vec<String> = self
                        .transfers
                        .lock()
                        .unwrap()
                        .values()
                        .filter(|record| {
                            record.snapshot.server_id == server_id
                                && nick.as_ref().map_or(true, |nick| {
                                    record.snapshot.nick.eq_ignore_ascii_case(nick)
                                })
                                && matches!(record.snapshot.status.as_str(), "active" | "waiting")
                        })
                        .map(|record| record.snapshot.id.clone())
                        .collect();
                    for id in ids {
                        let _ = self.cancel_transfer(&app, &id);
                    }
                }
                Ok(())
            }
        }
    }

    pub fn run_server_command(
        &self,
        app: AppHandle,
        server_id: &str,
        args: &str,
    ) -> Result<(), String> {
        let current = self.server_snapshot();
        let mut chat = current.as_ref().map_or(true, |value| value.2);
        let mut send = current.as_ref().map_or(true, |value| value.3);
        let mut fserve = current.as_ref().map_or(true, |value| value.4);
        let mut enabled = current.is_some();
        let mut port = current.as_ref().map_or(59, |value| value.1);
        let words = args.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            dcc_notice(
                &app,
                server_id,
                &current.map_or_else(
                    || "DCC server is off".to_string(),
                    |(_, port, chat, send, fserve)| {
                        format!(
                            "DCC server is on at port {port} ({})",
                            server_services(chat, send, fserve)
                        )
                    },
                ),
            );
            return Ok(());
        }
        for word in words {
            let lower = word.to_ascii_lowercase();
            if lower == "on" {
                enabled = true;
            } else if lower == "off" {
                enabled = false;
            } else if let Ok(value) = lower.parse::<u16>() {
                if value == 0 {
                    return Err("port must be between 1 and 65535".into());
                }
                port = value;
            } else if lower.starts_with('+') || lower.starts_with('-') {
                let value = lower.starts_with('+');
                for switch in lower[1..].chars() {
                    match switch {
                        'c' => chat = value,
                        's' => send = value,
                        'f' => fserve = value,
                        _ => return Err(format!("unknown /dccserver switch {switch}")),
                    }
                }
            } else {
                return Err("usage: /dccserver [+|-scf] [on|off] [port]".into());
            }
        }
        self.configure_server(
            app,
            server_id.to_string(),
            enabled,
            port,
            chat,
            send,
            fserve,
        )
    }

    pub fn transfer_snapshots(
        &self,
        server_id: &str,
        kind: Option<&str>,
    ) -> Vec<DccTransferSnapshot> {
        self.transfers
            .lock()
            .unwrap()
            .values()
            .filter(|record| {
                (server_id.is_empty() || record.snapshot.server_id == server_id)
                    && kind.is_none_or(|kind| record.snapshot.kind == kind)
            })
            .map(|record| record.snapshot.clone())
            .collect()
    }

    pub fn chat_snapshots(&self, server_id: &str) -> Vec<DccChatSnapshot> {
        self.chats
            .lock()
            .unwrap()
            .values()
            .filter(|chat| server_id.is_empty() || chat.server_id == server_id)
            .map(|chat| DccChatSnapshot {
                nick: chat.nick.clone(),
                ip: chat.ip.clone(),
                status: if chat.connected.load(Ordering::Acquire) {
                    "active".into()
                } else {
                    "waiting".into()
                },
                opened: chat.opened,
            })
            .collect()
    }
}

/// Script-engine bridge for mIRC's live `$chat`/`$send`/`$get` identifiers.
pub struct EngineDcc {
    app: AppHandle,
}

impl EngineDcc {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl crate::script::eval::ScriptDcc for EngineDcc {
    fn snapshot(&self, server_id: &str) -> Vec<crate::script::eval::DccInfo> {
        let Some(manager) = self.app.try_state::<DccManager>() else {
            return Vec::new();
        };
        let mut items: Vec<(Instant, crate::script::eval::DccInfo)> = manager
            .transfer_snapshots(server_id, None)
            .into_iter()
            // mIRC removes completed/failed transfers from `$send`/`$get`;
            // jIRC keeps them separately only so the transfer UI can retry.
            .filter(|transfer| matches!(transfer.status.as_str(), "active" | "waiting"))
            .map(|transfer| {
                (
                    transfer.opened,
                    crate::script::eval::DccInfo {
                        kind: transfer.kind,
                        nick: transfer.nick,
                        filename: transfer.filename,
                        path: transfer.path,
                        ip: transfer.ip,
                        status: transfer.status,
                        transferred: transfer.transferred,
                        size: transfer.size,
                        resume: transfer.resume,
                        last_ack: transfer.last_ack,
                        secs: transfer.opened.elapsed().as_secs(),
                    },
                )
            })
            .collect();
        items.extend(manager.chat_snapshots(server_id).into_iter().map(|chat| {
            (
                chat.opened,
                crate::script::eval::DccInfo {
                    kind: "chat".into(),
                    nick: chat.nick,
                    ip: chat.ip,
                    status: chat.status,
                    secs: chat.opened.elapsed().as_secs(),
                    ..Default::default()
                },
            )
        }));
        items.sort_by_key(|(opened, _)| *opened);
        items.into_iter().map(|(_, info)| info).collect()
    }

    fn server_port(&self) -> Option<u16> {
        self.app
            .try_state::<DccManager>()
            .and_then(|manager| manager.server_snapshot().map(|snapshot| snapshot.1))
    }
}

fn server_services(chat: bool, send: bool, fserve: bool) -> String {
    let mut services = Vec::new();
    if chat {
        services.push("chat");
    }
    if send {
        services.push("send");
    }
    if fserve {
        services.push("fserve");
    }
    if services.is_empty() {
        "no services".into()
    } else {
        services.join(", ")
    }
}

fn server_nick(app: &AppHandle, server_id: &str) -> String {
    app.try_state::<crate::irc::state::StateStore>()
        .map(|store| store.get(server_id).nick.clone())
        .filter(|nick| !nick.is_empty())
        .unwrap_or_else(|| "jIRC".into())
}

async fn server_line(stream: &mut TcpStream, line: &str) -> std::io::Result<()> {
    timeout(
        Duration::from_secs(15),
        stream.write_all(format!("{line}\r\n").as_bytes()),
    )
    .await
    .map_err(|_| timeout_error("DCC Server response timed out"))?
}

async fn read_server_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut line = Vec::new();
    timeout(Duration::from_secs(15), async {
        loop {
            let mut byte = [0u8; 1];
            if stream.read(&mut byte).await? == 0 {
                break;
            }
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
            if line.len() > 4096 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "DCC Server request is too long",
                ));
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| timeout_error("DCC Server request timed out"))??;
    if line.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "DCC Server connection closed before a request",
        ));
    }
    Ok(String::from_utf8_lossy(&line)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

async fn handle_server_connection(
    app: AppHandle,
    server_id: String,
    mut stream: TcpStream,
    peer_ip: IpAddr,
    allow_chat: bool,
    allow_send: bool,
    allow_fserve: bool,
) {
    let request = match read_server_request(&mut stream).await {
        Ok(request) => request,
        Err(_) => return,
    };
    let parsed = match parse_server_request(&request) {
        Some(parsed) => parsed,
        None => {
            let _ = server_line(&mut stream, "150 unavailable").await;
            return;
        }
    };
    let (service, nick, filename, filesize) = match parsed {
        DccServerRequest::Chat { nick } => ("chat", nick, String::new(), 0),
        DccServerRequest::Fserve { nick } => ("fserve", nick, String::new(), 0),
        DccServerRequest::Send {
            nick,
            size,
            filename,
        } => ("send", nick, filename, size),
        DccServerRequest::Get { nick, filename } => ("get", nick, filename, 0),
    };
    let allowed = match service {
        "chat" => allow_chat,
        "send" => allow_send,
        "fserve" => allow_fserve,
        "get" => allow_fserve,
        _ => false,
    };
    if !allowed {
        let _ = server_line(&mut stream, "150 unavailable").await;
        return;
    }
    if service != "get"
        && fire_dcc_server_event(&app, &server_id, service, &nick, peer_ip, &filename)
    {
        let _ = server_line(&mut stream, "151 rejected").await;
        return;
    }
    let my_nick = server_nick(&app, &server_id);
    match service {
        "chat" => {
            if server_line(&mut stream, &format!("101 {my_nick}"))
                .await
                .is_err()
            {
                return;
            }
            if let Some(manager) = app.try_state::<DccManager>() {
                if let Err(error) = manager.accept_server_chat(
                    app.clone(),
                    server_id.clone(),
                    nick,
                    peer_ip,
                    stream,
                ) {
                    dcc_notice(&app, &server_id, &format!("DCC Server chat: {error}"));
                }
            }
        }
        "fserve" => {
            if server_line(&mut stream, &format!("111 {my_nick}"))
                .await
                .is_err()
            {
                return;
            }
            let root = crate::script::script_data_dir(&app);
            let result = run_fserve(
                app.clone(),
                server_id.clone(),
                nick.clone(),
                stream,
                root,
                3,
                None,
                Some(peer_ip),
            )
            .await;
            if let Err(error) = result {
                dcc_notice(
                    &app,
                    &server_id,
                    &format!("DCC Server fserve for {nick}: {error}"),
                );
            }
        }
        "send" => {
            if server_line(&mut stream, &format!("121 {my_nick} 0"))
                .await
                .is_err()
            {
                return;
            }
            receive_server_file(app, server_id, nick, peer_ip, filename, filesize, stream).await;
        }
        "get" => {
            send_server_get(app, server_id, nick, peer_ip, filename, stream).await;
        }
        _ => {}
    }
}

async fn receive_server_file(
    app: AppHandle,
    server_id: String,
    nick: String,
    peer_ip: IpAddr,
    filename: String,
    size: u64,
    stream: TcpStream,
) {
    let Some(manager) = app.try_state::<DccManager>() else {
        return;
    };
    let dir = match crate::storage::dcc_dir(&app) {
        Ok(dir) => dir,
        Err(error) => {
            dcc_notice(&app, &server_id, &format!("DCC Server receive: {error}"));
            return;
        }
    };
    let base = safe_basename(&filename);
    let path = match reserve_destination(&dir, &base) {
        Ok(path) => path,
        Err(error) => {
            dcc_notice(&app, &server_id, &format!("DCC Server receive: {error}"));
            return;
        }
    };
    let xid = format!("recv-{}", NEXT_XFER.fetch_add(1, Ordering::Relaxed));
    manager.register_transfer(
        &app,
        DccTransferSnapshot {
            server_id: server_id.clone(),
            id: xid.clone(),
            kind: "recv".into(),
            nick: nick.clone(),
            filename: base.clone(),
            path: path.to_string_lossy().into_owned(),
            ip: peer_ip.to_string(),
            status: "active".into(),
            transferred: 0,
            size,
            resume: 0,
            last_ack: 0,
            opened: Instant::now(),
        },
        RetrySpec::ServerDirect,
    );
    let result = recv_into(&app, &server_id, &xid, &nick, &base, &path, stream, size, 0).await;
    finish_transfer(&app, &xid, result);
}

async fn send_server_get(
    app: AppHandle,
    server_id: String,
    nick: String,
    peer_ip: IpAddr,
    filename: String,
    mut stream: TcpStream,
) {
    let Some(manager) = app.try_state::<DccManager>() else {
        return;
    };
    let key = (peer_ip, safe_basename(&filename).to_ascii_lowercase());
    let pending = manager.server_gets.lock().unwrap().remove(&key);
    let Some((session_nick, path, opened)) = pending else {
        let _ = server_line(&mut stream, "151 rejected").await;
        return;
    };
    if opened.elapsed() > OFFER_TIMEOUT || !session_nick.eq_ignore_ascii_case(&nick) {
        let _ = server_line(&mut stream, "151 rejected").await;
        return;
    }
    let size = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => {
            let _ = server_line(&mut stream, "151 rejected").await;
            return;
        }
    };
    let my_nick = server_nick(&app, &server_id);
    if server_line(&mut stream, &format!("131 {my_nick} {size}"))
        .await
        .is_err()
    {
        return;
    }
    let reply = match read_server_request(&mut stream).await {
        Ok(reply) => reply,
        Err(_) => return,
    };
    let mut fields = reply.split_whitespace();
    if fields.next() != Some("132") {
        return;
    }
    let _client_nick = fields.next();
    let Some(offset) = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|offset| *offset <= size)
    else {
        return;
    };
    let base = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| safe_basename(&filename));
    let xid = format!("send-{}", NEXT_XFER.fetch_add(1, Ordering::Relaxed));
    manager.register_transfer(
        &app,
        DccTransferSnapshot {
            server_id: server_id.clone(),
            id: xid.clone(),
            kind: "send".into(),
            nick: nick.clone(),
            filename: base.clone(),
            path: path.to_string_lossy().into_owned(),
            ip: peer_ip.to_string(),
            status: "active".into(),
            transferred: offset,
            size,
            resume: offset,
            last_ack: offset,
            opened: Instant::now(),
        },
        RetrySpec::ServerDirect,
    );
    let result = send_into(
        &app, &server_id, &xid, &nick, &base, &path, stream, size, offset,
    )
    .await;
    finish_transfer(&app, &xid, result);
}

async fn run_chat(
    app: AppHandle,
    server_id: String,
    id: String,
    nick: String,
    stream: TcpStream,
    mut rx: UnboundedReceiver<String>,
    connected: Arc<AtomicBool>,
) {
    connected.store(true, Ordering::Release);
    fire_chat_event(&app, &server_id, "OPEN", &nick, "");
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(line) => {
                    if timeout(IO_TIMEOUT, write_half.write_all(format!("{line}\r\n").as_bytes()))
                        .await
                        .map_err(|_| ())
                        .and_then(|result| result.map_err(|_| ()))
                        .is_err()
                    {
                        break;
                    }
                }
                None => break,
            },
            res = reader.read_until(b'\n', &mut buf) => match res {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\r', '\n'])
                        .to_string();
                    buf.clear();
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::DccChatLine {
                            server_id: server_id.clone(),
                            id: id.clone(),
                            from: nick.clone(),
                            text: text.clone(),
                        },
                    );
                    fire_chat_event(&app, &server_id, "CHAT", &nick, &text);
                }
                Err(_) => break,
            },
            _ = tokio::time::sleep(IO_TIMEOUT) => break,
        }
    }
    emit_closed_and_script(&app, &server_id, &id, &nick);
}

async fn run_fserve(
    app: AppHandle,
    server_id: String,
    nick: String,
    stream: TcpStream,
    root: std::path::PathBuf,
    max_gets: usize,
    welcome: Option<String>,
    direct_peer: Option<IpAddr>,
) -> Result<(), String> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    write_fserve_line(&mut write_half, "jIRC DCC file server").await?;
    if let Some(welcome) = welcome {
        for line in welcome.lines().take(100) {
            write_fserve_line(&mut write_half, line).await?;
        }
    }
    write_fserve_line(
        &mut write_half,
        "Commands: dir, cd <dir>, get <file>, pwd, help, quit",
    )
    .await?;
    let mut cwd = root.clone();
    let mut buf = Vec::new();
    loop {
        write_fserve_line(&mut write_half, "fserve>").await?;
        let read = timeout(IO_TIMEOUT, reader.read_until(b'\n', &mut buf))
            .await
            .map_err(|_| "session timed out".to_string())?
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        let input = String::from_utf8_lossy(&buf).trim().to_string();
        buf.clear();
        let (command, argument) = input
            .split_once(char::is_whitespace)
            .map(|(a, b)| (a, b.trim()))
            .unwrap_or((input.as_str(), ""));
        match command.to_ascii_lowercase().as_str() {
            "dir" | "ls" => {
                let mut entries = std::fs::read_dir(&cwd)
                    .map_err(|error| error.to_string())?
                    .filter_map(Result::ok)
                    .collect::<Vec<_>>();
                entries
                    .sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());
                for entry in entries.into_iter().take(500) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let line = if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        format!("[{name}]")
                    } else {
                        format!("{} {name}", entry.metadata().map(|m| m.len()).unwrap_or(0))
                    };
                    write_fserve_line(&mut write_half, &line).await?;
                }
            }
            "cd" => match resolve_fserve_input(&root, &cwd, argument, true) {
                Ok(path) => cwd = path,
                Err(error) => {
                    write_fserve_line(&mut write_half, &format!("Error: {error}")).await?
                }
            },
            "pwd" => {
                let relative = cwd
                    .strip_prefix(&root)
                    .unwrap_or_else(|_| std::path::Path::new(""));
                write_fserve_line(&mut write_half, &format!("/{}", relative.display())).await?;
            }
            "get" => {
                let active = app
                    .try_state::<DccManager>()
                    .map(|manager| {
                        manager
                            .transfer_snapshots(&server_id, Some("send"))
                            .into_iter()
                            .filter(|item| {
                                item.nick.eq_ignore_ascii_case(&nick)
                                    && matches!(item.status.as_str(), "active" | "waiting")
                            })
                            .count()
                    })
                    .unwrap_or(0);
                if active >= max_gets {
                    write_fserve_line(&mut write_half, "All send slots are in use.").await?;
                    continue;
                }
                match resolve_fserve_input(&root, &cwd, argument, false) {
                    Ok(path) => {
                        if let Some(peer_ip) = direct_peer {
                            let name = path
                                .file_name()
                                .map(|value| value.to_string_lossy().to_string())
                                .ok_or("invalid fileserver filename")?;
                            app.try_state::<DccManager>()
                                .ok_or("DCC manager is unavailable".to_string())?
                                .server_gets
                                .lock()
                                .unwrap()
                                .insert(
                                    (peer_ip, name.to_ascii_lowercase()),
                                    (nick.clone(), path, Instant::now()),
                                );
                            write_fserve_line(&mut write_half, "DCC Server GET is ready.").await?;
                            continue;
                        }
                        let result = app
                            .try_state::<DccManager>()
                            .ok_or("DCC manager is unavailable".to_string())?
                            .send_file(app.clone(), server_id.clone(), nick.clone(), path);
                        match result {
                            Ok(()) => {
                                write_fserve_line(&mut write_half, "DCC SEND offered.").await?
                            }
                            Err(error) => {
                                write_fserve_line(&mut write_half, &format!("Error: {error}"))
                                    .await?
                            }
                        }
                    }
                    Err(error) => {
                        write_fserve_line(&mut write_half, &format!("Error: {error}")).await?
                    }
                }
            }
            "help" => {
                write_fserve_line(
                    &mut write_half,
                    "Commands: dir, cd <dir>, get <file>, pwd, help, quit",
                )
                .await?
            }
            "quit" | "exit" => break,
            "" => {}
            _ => write_fserve_line(&mut write_half, "Unknown command. Type help.").await?,
        }
    }
    Ok(())
}

async fn write_fserve_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    line: &str,
) -> Result<(), String> {
    timeout(
        IO_TIMEOUT,
        writer.write_all(format!("{line}\r\n").as_bytes()),
    )
    .await
    .map_err(|_| "write timed out".to_string())?
    .map_err(|error| error.to_string())
}

fn canonical_fserve_root(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let root = path.canonicalize().map_err(|error| error.to_string())?;
    if !root.is_dir() {
        return Err("fserve home must be a directory".into());
    }
    Ok(root)
}

fn canonical_fserve_path(
    root: &std::path::Path,
    path: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if !path.starts_with(root) {
        return Err("path is outside the fserve home".into());
    }
    Ok(path)
}

fn resolve_fserve_input(
    root: &std::path::Path,
    cwd: &std::path::Path,
    input: &str,
    directory: bool,
) -> Result<std::path::PathBuf, String> {
    if input.is_empty() {
        return Err("missing path".into());
    }
    let relative = input.trim_start_matches(['/', '\\']);
    let candidate = if input.starts_with(['/', '\\']) {
        root.join(relative)
    } else {
        cwd.join(relative)
    };
    let path = canonical_fserve_path(root, &candidate)?;
    if directory && !path.is_dir() {
        return Err("not a directory".into());
    }
    if !directory && !path.is_file() {
        return Err("not a file".into());
    }
    Ok(path)
}

fn emit_closed(app: &AppHandle, server_id: &str, id: &str) {
    let _ = app.emit(
        IRC_EVENT,
        UiEvent::DccChatClosed {
            server_id: server_id.to_string(),
            id: id.to_string(),
        },
    );
}

fn chat_key(server_id: &str, id: &str) -> String {
    format!("{server_id}\0{id}")
}

fn emit_closed_and_script(app: &AppHandle, server_id: &str, id: &str, nick: &str) {
    if let Some(manager) = app.try_state::<DccManager>() {
        if manager
            .chats
            .lock()
            .unwrap()
            .remove(&chat_key(server_id, id))
            .is_none()
        {
            return;
        }
    }
    emit_closed(app, server_id, id);
    fire_chat_event(app, server_id, "CLOSE", nick, "");
}

/// Runs a receive once the standard connect or passive accept transport is ready.
async fn run_receive<F>(app: AppHandle, pending: PendingResume, transport: F)
where
    F: std::future::Future<Output = std::io::Result<TcpStream>>,
{
    let result = async {
        let stream = transport.await?;
        mark_transfer_active(&app, &pending.xid, pending.position);
        recv_into(
            &app,
            &pending.server_id,
            &pending.xid,
            &pending.nick,
            &pending.base,
            &pending.path,
            stream,
            pending.offer.size,
            pending.position,
        )
        .await
    }
    .await;
    finish_transfer(&app, &pending.xid, result);
}

/// Connects to a DCC SEND peer, streams the file to `path`, and acknowledges
/// received bytes (the 4-byte big-endian running total DCC expects). Returns the
/// number of bytes received.
#[allow(clippy::too_many_arguments)]
async fn recv_into(
    app: &AppHandle,
    _server_id: &str,
    xid: &str,
    _nick: &str,
    _base: &str,
    path: &std::path::Path,
    mut stream: TcpStream,
    size: u64,
    offset: u64,
) -> std::io::Result<u64> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(path)
        .await?;
    if offset > 0 {
        let actual = file.metadata().await?.len();
        if actual != offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "partial file changed during DCC resume",
            ));
        }
        file.seek(SeekFrom::Start(offset)).await?;
    }
    let mut received = offset;
    let mut last_emit = offset;
    let mut buf = [0u8; 8192];
    loop {
        if size > 0 && received >= size {
            break;
        }
        let limit = if size > 0 {
            let remaining = size - received;
            if remaining < buf.len() as u64 {
                remaining as usize
            } else {
                buf.len()
            }
        } else {
            buf.len()
        };
        let n = timeout(IO_TIMEOUT, stream.read(&mut buf[..limit]))
            .await
            .map_err(|_| timeout_error("DCC receive timed out"))??;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await?;
        received += n as u64;
        // Acknowledge the running total (4 bytes, big-endian; u32 per the DCC spec).
        timeout(
            IO_TIMEOUT,
            stream.write_all(&(received as u32).to_be_bytes()),
        )
        .await
        .map_err(|_| timeout_error("DCC acknowledgement timed out"))??;
        // Throttle progress updates to ~64 KB steps.
        if received - last_emit >= 65536 {
            update_transfer(app, xid, received, None);
            last_emit = received;
        }
    }
    file.flush().await?;
    if size > 0 && received != size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("transfer ended at {received} of {size} bytes"),
        ));
    }
    Ok(received)
}

#[allow(clippy::too_many_arguments)]
async fn send_into(
    app: &AppHandle,
    _server_id: &str,
    xid: &str,
    _nick: &str,
    _base: &str,
    path: &std::path::Path,
    stream: TcpStream,
    size: u64,
    offset: u64,
) -> std::io::Result<u64> {
    let (mut rd, mut wr) = stream.into_split();
    let sent_total = Arc::new(AtomicU64::new(offset));
    let acked_total = Arc::new(AtomicU64::new(offset));
    let sent_for_acks = sent_total.clone();
    let acked_for_acks = acked_total.clone();
    // Validate four-byte cumulative ACKs. Values wrap at 2^32 for large files;
    // unwrap them relative to the last acknowledged absolute offset.
    let acks = tauri::async_runtime::spawn(async move {
        let mut last = offset;
        while last < size {
            let mut b = [0u8; 4];
            timeout(IO_TIMEOUT, rd.read_exact(&mut b))
                .await
                .map_err(|_| timeout_error("DCC acknowledgement timed out"))??;
            let next = unwrap_ack(u32::from_be_bytes(b), last);
            let sent = sent_for_acks.load(Ordering::Acquire);
            if next <= last || next > sent {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid DCC acknowledgement {next} (sent {sent})"),
                ));
            }
            last = next;
            acked_for_acks.store(last, Ordering::Release);
        }
        Ok::<u64, std::io::Error>(last)
    });
    let write_result = async {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(SeekFrom::Start(offset)).await?;
        let mut sent = offset;
        let mut last_emit = offset;
        let mut buf = [0u8; 8192];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let next = sent + n as u64;
            // Publish the write's upper bound before yielding: a fast peer may
            // ACK immediately after `write_all` makes the bytes visible.
            sent_total.store(next, Ordering::Release);
            if let Err(error) = timeout(IO_TIMEOUT, wr.write_all(&buf[..n]))
                .await
                .map_err(|_| timeout_error("DCC send timed out"))
                .and_then(|result| result)
            {
                sent_total.store(sent, Ordering::Release);
                return Err(error);
            }
            sent = next;
            if sent - last_emit >= 65536 {
                update_transfer(app, xid, sent, Some(acked_total.load(Ordering::Acquire)));
                last_emit = sent;
            }
        }
        timeout(IO_TIMEOUT, wr.flush())
            .await
            .map_err(|_| timeout_error("DCC send timed out"))??;
        Ok::<u64, std::io::Error>(sent)
    }
    .await;
    let sent = match write_result {
        Ok(sent) => sent,
        Err(error) => {
            acks.abort();
            return Err(error);
        }
    };
    if sent != size {
        acks.abort();
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("source file changed: sent {sent} of {size} bytes"),
        ));
    }
    let final_ack = timeout(IO_TIMEOUT, acks)
        .await
        .map_err(|_| timeout_error("final DCC acknowledgement timed out"))?
        .map_err(|e| std::io::Error::other(e.to_string()))??;
    update_transfer(app, xid, sent, Some(final_ack));
    drop(wr);
    Ok(sent)
}

fn unwrap_ack(raw: u32, previous: u64) -> u64 {
    let wrap = 1u64 << 32;
    let mut candidate = (previous & !(wrap - 1)) | raw as u64;
    if candidate < previous && previous - candidate > wrap / 2 {
        candidate += wrap;
    }
    candidate
}

fn timeout_error(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, message)
}

/// Emits a `[DCC]` status notice to the status window.
fn dcc_notice(app: &AppHandle, server_id: &str, text: &str) {
    let _ = app.emit(
        IRC_EVENT,
        UiEvent::Echo {
            server_id: server_id.to_string(),
            target: "(status)".to_string(),
            text: text.to_string(),
        },
    );
}

/// Monotonic id source for transfers, so the UI can track each independently.
static NEXT_XFER: AtomicU64 = AtomicU64::new(1);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn emit_transfer_snapshot(app: &AppHandle, snapshot: &DccTransferSnapshot) {
    let _ = app.emit(
        IRC_EVENT,
        UiEvent::DccTransfer {
            server_id: snapshot.server_id.clone(),
            id: snapshot.id.clone(),
            kind: snapshot.kind.clone(),
            nick: snapshot.nick.clone(),
            filename: snapshot.filename.clone(),
            transferred: snapshot.transferred,
            size: snapshot.size,
            status: snapshot.status.clone(),
        },
    );
}

fn mark_transfer_active(app: &AppHandle, id: &str, position: u64) {
    if let Some(manager) = app.try_state::<DccManager>() {
        let mut records = manager.transfers.lock().unwrap();
        if let Some(record) = records.get_mut(id) {
            if record.snapshot.status != "waiting" {
                return;
            }
            record.snapshot.status = "active".into();
            record.snapshot.transferred = position;
            record.snapshot.resume = position;
            let snapshot = record.snapshot.clone();
            drop(records);
            emit_transfer_snapshot(app, &snapshot);
        }
    }
}

fn update_transfer(app: &AppHandle, id: &str, transferred: u64, last_ack: Option<u64>) {
    if let Some(manager) = app.try_state::<DccManager>() {
        let mut records = manager.transfers.lock().unwrap();
        if let Some(record) = records.get_mut(id) {
            if record.snapshot.status != "active" {
                return;
            }
            record.snapshot.transferred = transferred;
            if let Some(last_ack) = last_ack {
                record.snapshot.last_ack = last_ack;
            }
            let snapshot = record.snapshot.clone();
            drop(records);
            emit_transfer_snapshot(app, &snapshot);
        }
    }
}

fn finish_transfer(app: &AppHandle, id: &str, result: std::io::Result<u64>) {
    let Some(manager) = app.try_state::<DccManager>() else {
        return;
    };
    let mut records = manager.transfers.lock().unwrap();
    let Some(record) = records.get_mut(id) else {
        return;
    };
    if !matches!(record.snapshot.status.as_str(), "active" | "waiting") {
        return;
    }
    match &result {
        Ok(n) => {
            record.snapshot.transferred = *n;
            record.snapshot.status = "done".into();
        }
        Err(_) => record.snapshot.status = "error".into(),
    }
    record.task = None;
    let snapshot = record.snapshot.clone();
    drop(records);
    emit_transfer_snapshot(app, &snapshot);
    match result {
        Ok(n) => dcc_notice(
            app,
            &snapshot.server_id,
            &format!(
                "DCC: {} \"{}\" ({n} bytes) {} {}",
                if snapshot.kind == "send" {
                    "sent"
                } else {
                    "received"
                },
                snapshot.filename,
                if snapshot.kind == "send" {
                    "to"
                } else {
                    "from"
                },
                snapshot.nick,
            ),
        ),
        Err(error) => dcc_notice(
            app,
            &snapshot.server_id,
            &format!(
                "DCC: failed {} \"{}\" {} {}: {error}",
                if snapshot.kind == "send" {
                    "sending"
                } else {
                    "receiving"
                },
                snapshot.filename,
                if snapshot.kind == "send" {
                    "to"
                } else {
                    "from"
                },
                snapshot.nick,
            ),
        ),
    }
    fire_file_event(app, &snapshot, snapshot.status == "done");
}

fn fail_transfer(app: &AppHandle, id: &str, reason: &str) {
    finish_transfer(
        app,
        id,
        Err(std::io::Error::new(std::io::ErrorKind::Other, reason)),
    );
}

fn cleanup_outgoing(app: &AppHandle, server_id: &str, nick: &str, port: u16, token: Option<u64>) {
    if let Some(manager) = app.try_state::<DccManager>() {
        let key = DccKey::new(server_id, nick, DccKind::Send, port, token);
        manager.outgoing_sends.lock().unwrap().remove(&key);
        if port == 0 {
            manager.passive_endpoints.lock().unwrap().remove(&key);
        }
    }
}

fn send_ctcp(app: &AppHandle, server_id: &str, nick: &str, payload: &str) -> Result<(), String> {
    let manager = app
        .try_state::<ConnectionManager>()
        .ok_or("IRC connection manager is unavailable")?;
    manager.send(server_id, format!("PRIVMSG {nick} :\u{1}{payload}\u{1}"))
}

fn safe_basename(filename: &str) -> String {
    let filename = filename.replace('\\', "/");
    filename
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .unwrap_or("received.bin")
        .to_string()
}

fn parse_server_target(target: &str) -> Option<(IpAddr, u16)> {
    if let Ok(address) = target.parse::<SocketAddr>() {
        return Some((address.ip(), address.port()));
    }
    target.parse::<IpAddr>().ok().map(|ip| (ip, 59))
}

/// Never silently overwrites an existing download. `file.ext`, `file (1).ext`, …
fn unused_destination(dir: &std::path::Path, filename: &str) -> std::path::PathBuf {
    let direct = dir.join(filename);
    if !direct.exists() {
        return direct;
    }
    let path = std::path::Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("received");
    let ext = path.extension().and_then(|s| s.to_str());
    for n in 1u64.. {
        let name = match ext {
            Some(ext) if !ext.is_empty() => format!("{stem} ({n}).{ext}"),
            _ => format!("{stem} ({n})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn reserve_destination(
    dir: &std::path::Path,
    filename: &str,
) -> Result<std::path::PathBuf, String> {
    loop {
        let path = unused_destination(dir, filename);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn fire_chat_event(app: &AppHandle, server_id: &str, kind: &str, nick: &str, text: &str) {
    let target = format!("={nick}");
    fire_dcc_script(
        app,
        server_id,
        kind,
        crate::script::eval::EventVars {
            nick: nick.to_string(),
            chan: String::new(),
            target,
            text: text.to_string(),
            params: text.split_whitespace().map(String::from).collect(),
            ..Default::default()
        },
    );
}

fn fire_file_event(app: &AppHandle, snapshot: &DccTransferSnapshot, success: bool) {
    let kind = match (snapshot.kind.as_str(), success) {
        ("send", true) => "FILESENT",
        ("recv", true) => "FILERCVD",
        ("send", false) => "SENDFAIL",
        _ => "GETFAIL",
    };
    fire_dcc_script(
        app,
        &snapshot.server_id,
        kind,
        crate::script::eval::EventVars {
            nick: snapshot.nick.clone(),
            target: snapshot.nick.clone(),
            text: snapshot.filename.clone(),
            params: vec![snapshot.path.clone()],
            filename: snapshot.path.clone(),
            dcc_id: snapshot.id.clone(),
            ..Default::default()
        },
    );
}

fn fire_dcc_script(
    app: &AppHandle,
    server_id: &str,
    kind: &str,
    vars: crate::script::eval::EventVars,
) {
    let (Some(engine), Some(store)) = (
        app.try_state::<crate::script::ScriptEngine>(),
        app.try_state::<crate::irc::state::StateStore>(),
    ) else {
        return;
    };
    let state = store.get(server_id);
    let my_nick = state.nick.clone();
    let (network, server) = engine.connection_context(server_id).unwrap_or_default();
    let ctx = crate::script::RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: &server,
        data_dir: crate::script::script_data_dir(app),
        state,
    };
    let actions = engine.dispatch_event(&ctx, kind, vars);
    crate::script::apply_actions(app, server_id, &my_nick, &network, &server, actions);
}

fn fire_dcc_server_event(
    app: &AppHandle,
    server_id: &str,
    service: &str,
    nick: &str,
    address: IpAddr,
    filename: &str,
) -> bool {
    let (Some(engine), Some(store)) = (
        app.try_state::<crate::script::ScriptEngine>(),
        app.try_state::<crate::irc::state::StateStore>(),
    ) else {
        return false;
    };
    let state = store.get(server_id);
    let my_nick = state.nick.clone();
    let (network, server) = engine.connection_context(server_id).unwrap_or_default();
    let ctx = crate::script::RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: &server,
        data_dir: crate::script::script_data_dir(app),
        state,
    };
    let (actions, halted) = engine.dispatch_event_halt(
        &ctx,
        "DCCSERVER",
        crate::script::eval::EventVars {
            nick: nick.to_string(),
            target: nick.to_string(),
            text: service.to_string(),
            params: if filename.is_empty() {
                Vec::new()
            } else {
                vec![filename.to_string()]
            },
            filename: filename.to_string(),
            peer_address: address.to_string(),
            ..Default::default()
        },
    );
    crate::script::apply_actions(app, server_id, &my_nick, &network, &server, actions);
    halted
}

/// The machine's primary local IPv4, found from the local address a UDP socket
/// would use to reach a public host (no packets are actually sent).
fn local_ipv4() -> Option<Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        _ => None,
    }
}

/// The machine's routable global IPv6, if any (skips link-local `fe80::`). This
/// is what makes DCC work through CGNAT, where IPv4 has no reachable port.
fn local_ipv6() -> Option<Ipv6Addr> {
    let sock = std::net::UdpSocket::bind("[::]:0").ok()?;
    sock.connect("[2001:4860:4860::8888]:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V6(ip) if !ip.is_loopback() && (ip.segments()[0] & 0xffc0) != 0xfe80 => Some(ip),
        _ => None,
    }
}

/// The IP to auto-fill in the DCC settings: a routable global IPv6 (which works
/// through CGNAT) if the machine has one, else empty so the UI falls back to the
/// USERHOST-detected IPv4.
pub fn detect_local_ip() -> String {
    local_ipv6().map(|v6| v6.to_string()).unwrap_or_default()
}

/// Binds a listener (on the advertised IP's family) in the configured port range,
/// or an ephemeral port when `from == 0`. Returns the listener and chosen port.
fn bind_in_range(ip: IpAddr, from: u16, to: u16) -> Option<(std::net::TcpListener, u16)> {
    let any: IpAddr = if ip.is_ipv6() {
        Ipv6Addr::UNSPECIFIED.into()
    } else {
        Ipv4Addr::UNSPECIFIED.into()
    };
    if from == 0 {
        let l = std::net::TcpListener::bind((any, 0)).ok()?;
        l.set_nonblocking(true).ok()?;
        let p = l.local_addr().ok()?.port();
        return Some((l, p));
    }
    (from..=to.max(from)).find_map(|p| {
        let listener = std::net::TcpListener::bind((any, p)).ok()?;
        listener.set_nonblocking(true).ok()?;
        Some((listener, p))
    })
}

/// The IP to advertise in offers: the configured one (IPv4 or IPv6) if set, else
/// the local IPv4 (LAN default).
fn resolve_dcc_ip(configured: &str) -> Result<IpAddr, String> {
    let c = configured.trim();
    if !c.is_empty() {
        return c
            .parse::<IpAddr>()
            .map_err(|_| format!("invalid DCC IP: {c}"));
    }
    local_ipv4()
        .map(IpAddr::V4)
        .ok_or_else(|| "could not determine the local IP address".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_round_trips_as_dcc_integer() {
        let ip = Ipv4Addr::new(192, 168, 0, 1);
        assert_eq!(ip_to_dcc(ip), 3232235521);
        assert_eq!(dcc_to_ip(3232235521), ip);
    }

    #[test]
    fn parses_dcc_chat() {
        let o = parse_dcc("DCC CHAT chat 3232235521 1024").unwrap();
        assert_eq!(o.kind, DccKind::Chat);
        assert_eq!(o.ip, Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(o.port, 1024);
    }

    #[test]
    fn parses_dcc_send_with_and_without_size_and_quotes() {
        let o = parse_dcc("DCC SEND readme.txt 3232235521 5000 12345").unwrap();
        assert_eq!(o.kind, DccKind::Send);
        assert_eq!(o.filename, "readme.txt");
        assert_eq!(o.port, 5000);
        assert_eq!(o.size, 12345);

        // Quoted filename with spaces.
        let o = parse_dcc("DCC SEND \"my long file.bin\" 3232235521 5000 99").unwrap();
        assert_eq!(o.filename, "my long file.bin");
        assert_eq!(o.size, 99);

        // Legacy, size-less.
        let o = parse_dcc("DCC SEND a.txt 16909060 6000").unwrap();
        assert_eq!(o.ip, Ipv4Addr::new(1, 2, 3, 4));
        assert_eq!(o.port, 6000);
        assert_eq!(o.size, 0);
    }

    #[test]
    fn rejects_non_dcc_and_garbage() {
        assert!(parse_dcc("VERSION").is_none());
        assert!(parse_dcc("DCC WAT something").is_none());
        assert!(parse_dcc("DCC CHAT chat notanip 1024").is_none());
        assert!(parse_dcc("DCC CHAT not-chat 3232235521 1024").is_none());
        assert!(parse_dcc("DCC CHAT chat 3232235521 1024 bad-token").is_none());
        assert!(parse_dcc("DCC SEND file.bin 3232235521 1024 bad-size").is_none());
        assert!(parse_dcc("DCC SEND file.bin 3232235521 1024 1 bad-token").is_none());
        assert!(parse_dcc_message("DCC RESUME file.bin 1024 1 bad-token").is_none());
    }

    #[test]
    fn formats_offers() {
        let ip: IpAddr = Ipv4Addr::new(192, 168, 0, 1).into();
        assert_eq!(format_chat_offer(ip, 1024), "DCC CHAT chat 3232235521 1024");
        assert_eq!(
            format_send_offer("file.txt", ip, 1024, 50),
            "DCC SEND file.txt 3232235521 1024 50"
        );
        assert_eq!(
            format_send_offer("a b.txt", ip, 1024, 50),
            "DCC SEND \"a b.txt\" 3232235521 1024 50"
        );
    }

    #[test]
    fn handles_ipv6_offers() {
        let o = parse_dcc("DCC CHAT chat 2001:db8::1 1024").unwrap();
        assert_eq!(o.ip, "2001:db8::1".parse::<IpAddr>().unwrap());
        assert_eq!(o.port, 1024);
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(
            format_chat_offer(ip, 1024),
            "DCC CHAT chat 2001:db8::1 1024"
        );
        let o = parse_dcc("DCC SEND f.bin 2001:db8::2 5000 100").unwrap();
        assert_eq!(o.ip, "2001:db8::2".parse::<IpAddr>().unwrap());
        assert_eq!(o.size, 100);
    }

    #[test]
    fn parses_mirc_passive_chat_and_send_tokens() {
        let chat = parse_dcc("DCC CHAT chat 3232235521 0 44").unwrap();
        assert_eq!(chat.kind, DccKind::Chat);
        assert_eq!(chat.port, 0);
        assert_eq!(chat.token, Some(44));

        let send = parse_dcc("DCC SEND \"long file.bin\" 3232235521 0 99 45").unwrap();
        assert_eq!(send.kind, DccKind::Send);
        assert_eq!(send.filename, "long file.bin");
        assert_eq!(send.size, 99);
        assert_eq!(send.token, Some(45));

        assert!(parse_dcc("DCC SEND file.bin 3232235521 0 99").is_none());
        assert_eq!(
            format_send_passive("long file.bin", "192.168.0.1".parse().unwrap(), 99, 45),
            "DCC SEND \"long file.bin\" 3232235521 0 99 45"
        );
    }

    #[test]
    fn parses_standard_and_passive_resume_accept() {
        assert_eq!(
            parse_dcc_message("DCC RESUME file.bin 5000 123"),
            Some(DccMessage::Resume {
                filename: "file.bin".into(),
                port: 5000,
                position: 123,
                token: None,
            })
        );
        assert_eq!(
            parse_dcc_message("DCC ACCEPT \"long file.bin\" 0 123 77"),
            Some(DccMessage::Accept {
                filename: "long file.bin".into(),
                port: 0,
                position: 123,
                token: Some(77),
            })
        );
        assert_eq!(
            format_resume("RESUME", "long file.bin", 0, 123, Some(77)),
            "DCC RESUME \"long file.bin\" 0 123 77"
        );
    }

    #[test]
    fn ack_unwrap_handles_four_gibibyte_boundary() {
        assert_eq!(
            unwrap_ack(u32::MAX - 4, u32::MAX as u64 - 10),
            u32::MAX as u64 - 4
        );
        assert_eq!(unwrap_ack(25, (1u64 << 32) - 10), (1u64 << 32) + 25);
    }

    #[test]
    fn receive_names_are_traversal_safe_and_collision_free() {
        assert_eq!(safe_basename("../../secret.txt"), "secret.txt");
        assert_eq!(safe_basename("..\\..\\secret.txt"), "secret.txt");
        let dir = std::env::temp_dir().join(format!(
            "jirc-dcc-test-{}",
            NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), b"old").unwrap();
        assert_eq!(
            unused_destination(&dir, "file.txt").file_name().unwrap(),
            "file (1).txt"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fserve_paths_cannot_escape_the_served_root() {
        let dir = std::env::temp_dir().join(format!(
            "jirc-fserve-test-{}",
            NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
        ));
        let root = dir.join("public");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("file.txt"), b"safe").unwrap();
        std::fs::write(dir.join("secret.txt"), b"secret").unwrap();
        let root = canonical_fserve_root(&root).unwrap();
        assert_eq!(
            resolve_fserve_input(&root, &root, "file.txt", false)
                .unwrap()
                .file_name()
                .unwrap(),
            "file.txt"
        );
        assert!(resolve_fserve_input(&root, &root, "../secret.txt", false).is_err());
        assert!(resolve_fserve_input(&root, &root, "file.txt", true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn listeners_are_nonblocking_before_tokio_adopts_them() {
        let (listener, port) = bind_in_range(Ipv4Addr::LOCALHOST.into(), 0, 0).unwrap();
        assert_ne!(port, 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn parses_dcc_commands() {
        assert_eq!(
            parse_dcc_command("chat bob"),
            Some(DccCommand::Chat { nick: "bob".into() })
        );
        assert_eq!(
            parse_dcc_command("send bob my file.txt"),
            Some(DccCommand::Send {
                nick: "bob".into(),
                file: "my file.txt".into()
            })
        );
        assert_eq!(
            parse_dcc_command("send -clmn bob file.txt"),
            Some(DccCommand::Send {
                nick: "bob".into(),
                file: "file.txt".into()
            })
        );
        assert_eq!(
            parse_dcc_command("get"),
            Some(DccCommand::Get { nick: None })
        );
        assert_eq!(
            parse_dcc_command("resume bob"),
            Some(DccCommand::Resume {
                nick: Some("bob".into())
            })
        );
        assert_eq!(
            parse_dcc_command("passive on"),
            Some(DccCommand::Passive {
                enabled: Some(true)
            })
        );
        assert_eq!(
            parse_dcc_command("close chat bob"),
            Some(DccCommand::Close {
                kind: Some(DccKind::Chat),
                nick: Some("bob".into())
            })
        );
        assert_eq!(parse_dcc_command("chat"), None); // missing nick
        assert_eq!(parse_dcc_command("wat"), None);
    }

    #[test]
    fn parses_dcc_server_protocol_requests_without_losing_spaces() {
        assert_eq!(
            parse_server_request("100 visitor\r\n"),
            Some(DccServerRequest::Chat {
                nick: "visitor".into()
            })
        );
        assert_eq!(
            parse_server_request("110 visitor"),
            Some(DccServerRequest::Fserve {
                nick: "visitor".into()
            })
        );
        assert_eq!(
            parse_server_request("120 visitor 42 file name.txt"),
            Some(DccServerRequest::Send {
                nick: "visitor".into(),
                size: 42,
                filename: "file name.txt".into(),
            })
        );
        assert_eq!(
            parse_server_request("130 visitor wanted file.txt"),
            Some(DccServerRequest::Get {
                nick: "visitor".into(),
                filename: "wanted file.txt".into(),
            })
        );
        assert_eq!(parse_server_request("120 visitor nope file.txt"), None);
        assert_eq!(parse_server_request("999 visitor"), None);
    }

    #[test]
    fn parses_direct_server_targets() {
        assert_eq!(
            parse_server_target("192.0.2.1"),
            Some(("192.0.2.1".parse().unwrap(), 59))
        );
        assert_eq!(
            parse_server_target("192.0.2.1:50059"),
            Some(("192.0.2.1".parse().unwrap(), 50059))
        );
        assert_eq!(
            parse_server_target("[2001:db8::1]:50059"),
            Some(("2001:db8::1".parse().unwrap(), 50059))
        );
        assert_eq!(parse_server_target("nickname"), None);
    }
}
