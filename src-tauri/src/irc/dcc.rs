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
use std::net::Ipv4Addr;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::event::{UiEvent, IRC_EVENT};
use super::ConnectionManager;

/// The kind of a DCC handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub ip: Ipv4Addr,
    pub port: u16,
    /// The file size in bytes for `SEND` (`0` when absent or for `CHAT`).
    pub size: u64,
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

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s)
        .to_string()
}

/// Parses a CTCP DCC payload (the text between the `\x01` markers, already
/// stripped), e.g. `DCC CHAT chat 3232235521 1024` or
/// `DCC SEND "my file.txt" 3232235521 1024 5000`. Returns `None` if it isn't a
/// DCC offer we understand.
pub fn parse_dcc(payload: &str) -> Option<DccOffer> {
    let mut head = payload.split_whitespace();
    if !head.next()?.eq_ignore_ascii_case("DCC") {
        return None;
    }
    let kind = head.next()?;

    if kind.eq_ignore_ascii_case("CHAT") {
        let _proto = head.next()?; // the literal "chat"
        let ip = dcc_to_ip(head.next()?.parse::<u32>().ok()?);
        let port = head.next()?.parse::<u16>().ok()?;
        return Some(DccOffer {
            kind: DccKind::Chat,
            filename: String::new(),
            ip,
            port,
            size: 0,
        });
    }

    if kind.eq_ignore_ascii_case("SEND") {
        // After "DCC SEND ": `<filename> <ip> <port> [size]`. The filename may
        // contain spaces (then it's quoted), so split the trailing fields off
        // from the right. Try the modern 4-field form first, then the legacy
        // size-less 3-field form.
        let rest = payload.splitn(3, char::is_whitespace).nth(2)?.trim();
        let w: Vec<&str> = rest.rsplitn(4, char::is_whitespace).collect();
        if w.len() == 4 {
            if let (Ok(size), Ok(port), Ok(ipn)) = (
                w[0].parse::<u64>(),
                w[1].parse::<u16>(),
                w[2].parse::<u32>(),
            ) {
                return Some(DccOffer {
                    kind: DccKind::Send,
                    filename: unquote(w[3]),
                    ip: dcc_to_ip(ipn),
                    port,
                    size,
                });
            }
        }
        let w: Vec<&str> = rest.rsplitn(3, char::is_whitespace).collect();
        if w.len() == 3 {
            if let (Ok(port), Ok(ipn)) = (w[0].parse::<u16>(), w[1].parse::<u32>()) {
                return Some(DccOffer {
                    kind: DccKind::Send,
                    filename: unquote(w[2]),
                    ip: dcc_to_ip(ipn),
                    port,
                    size: 0,
                });
            }
        }
        return None;
    }

    None
}

/// Builds the CTCP payload for an outgoing DCC CHAT offer (caller wraps it in
/// `\x01` and a `PRIVMSG`).
#[allow(dead_code)] // wired in the DCC connect/send phase
pub fn format_chat_offer(ip: Ipv4Addr, port: u16) -> String {
    format!("DCC CHAT chat {} {}", ip_to_dcc(ip), port)
}

/// Builds the CTCP payload for an outgoing DCC SEND offer. Filenames containing
/// spaces are quoted.
#[allow(dead_code)] // wired in the DCC connect/send phase
pub fn format_send_offer(filename: &str, ip: Ipv4Addr, port: u16, size: u64) -> String {
    let name = if filename.contains(' ') {
        format!("\"{filename}\"")
    } else {
        filename.to_string()
    };
    format!("DCC SEND {} {} {} {}", name, ip_to_dcc(ip), port, size)
}

/// A parsed `/dcc` subcommand (the part after `/dcc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DccCommand {
    /// `/dcc chat <nick>` — offer/open a direct chat.
    Chat { nick: String },
    /// `/dcc send <nick> <file>` — offer a file.
    Send { nick: String, file: String },
    /// `/dcc get [nick]` — accept a pending incoming offer.
    Get { nick: Option<String> },
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
        "chat" => Some(DccCommand::Chat {
            nick: t.next()?.to_string(),
        }),
        "send" => {
            let nick = t.next()?.to_string();
            let file = t.collect::<Vec<_>>().join(" ");
            (!file.is_empty()).then_some(DccCommand::Send { nick, file })
        }
        "get" | "accept" => Some(DccCommand::Get {
            nick: t.next().map(String::from),
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
}

/// Manages active DCC chat sessions, keyed by their `=nick` buffer id.
#[derive(Default)]
pub struct DccManager {
    chats: Mutex<HashMap<String, DccChat>>,
}

impl DccManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// `/dcc chat <nick>` — listen on an ephemeral port, send the peer a CHAT
    /// offer over IRC, and accept their connection.
    pub fn chat(&self, app: AppHandle, server_id: String, nick: String) -> Result<(), String> {
        let listener = std::net::TcpListener::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let ip = local_ipv4().ok_or("could not determine the local IP address")?;
        let offer = format_chat_offer(ip, port);
        if let Some(m) = app.try_state::<ConnectionManager>() {
            let _ = m.send(&server_id, format!("PRIVMSG {nick} :\u{1}{offer}\u{1}"));
        }
        let id = format!("={nick}");
        let _ = app.emit(
            IRC_EVENT,
            UiEvent::DccChatOpen {
                server_id: server_id.clone(),
                id: id.clone(),
                nick: nick.clone(),
                outgoing: true,
            },
        );
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let id2 = id.clone();
        let task = tauri::async_runtime::spawn(async move {
            let listener = match TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(_) => return emit_closed(&app, &server_id, &id),
            };
            match listener.accept().await {
                Ok((stream, _)) => run_chat(app, server_id, id, nick, stream, rx).await,
                Err(_) => emit_closed(&app, &server_id, &id),
            }
        });
        self.chats.lock().unwrap().insert(id2, DccChat { tx, task });
        Ok(())
    }

    /// Accept an incoming offer by connecting to `ip:port`.
    pub fn accept(&self, app: AppHandle, server_id: String, nick: String, ip: Ipv4Addr, port: u16) {
        let id = format!("={nick}");
        let _ = app.emit(
            IRC_EVENT,
            UiEvent::DccChatOpen {
                server_id: server_id.clone(),
                id: id.clone(),
                nick: nick.clone(),
                outgoing: false,
            },
        );
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let id2 = id.clone();
        let task = tauri::async_runtime::spawn(async move {
            match TcpStream::connect((ip, port)).await {
                Ok(stream) => run_chat(app, server_id, id, nick, stream, rx).await,
                Err(_) => emit_closed(&app, &server_id, &id),
            }
        });
        self.chats.lock().unwrap().insert(id2, DccChat { tx, task });
    }

    /// Send a typed line to a DCC chat peer.
    pub fn send(&self, id: &str, text: String) {
        if let Some(c) = self.chats.lock().unwrap().get(id) {
            let _ = c.tx.send(text);
        }
    }

    /// Close a DCC chat session.
    pub fn close(&self, id: &str) {
        if let Some(c) = self.chats.lock().unwrap().remove(id) {
            c.task.abort();
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
        ip: Ipv4Addr,
        port: u16,
        size: u64,
    ) {
        tauri::async_runtime::spawn(async move {
            let dir = match crate::storage::dcc_dir(&app) {
                Ok(d) => d,
                Err(e) => {
                    return dcc_notice(&app, &server_id, &format!("DCC: can't open the dcc folder: {e}"));
                }
            };
            // Use only the file's base name, to avoid path traversal.
            let base = std::path::Path::new(&filename)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "received.bin".to_string());
            let path = dir.join(&base);
            dcc_notice(
                &app,
                &server_id,
                &format!("DCC: receiving \"{base}\" ({size} bytes) from {nick}…"),
            );
            match recv_into(&path, ip, port, size).await {
                Ok(n) => dcc_notice(
                    &app,
                    &server_id,
                    &format!("DCC: received \"{base}\" ({n} bytes) → {}", path.display()),
                ),
                Err(e) => {
                    dcc_notice(&app, &server_id, &format!("DCC: failed to receive \"{base}\": {e}"))
                }
            }
        });
    }
}

async fn run_chat(
    app: AppHandle,
    server_id: String,
    id: String,
    nick: String,
    stream: TcpStream,
    mut rx: UnboundedReceiver<String>,
) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(line) => {
                    if write_half.write_all(format!("{line}\n").as_bytes()).await.is_err() {
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
                            text,
                        },
                    );
                }
                Err(_) => break,
            },
        }
    }
    emit_closed(&app, &server_id, &id);
    if let Some(m) = app.try_state::<DccManager>() {
        m.chats.lock().unwrap().remove(&id);
    }
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

/// Connects to a DCC SEND peer, streams the file to `path`, and acknowledges
/// received bytes (the 4-byte big-endian running total DCC expects). Returns the
/// number of bytes received.
async fn recv_into(
    path: &std::path::Path,
    ip: Ipv4Addr,
    port: u16,
    size: u64,
) -> std::io::Result<u64> {
    let mut stream = TcpStream::connect((ip, port)).await?;
    let mut file = tokio::fs::File::create(path).await?;
    let mut received: u64 = 0;
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await?;
        received += n as u64;
        // Acknowledge the running total (4 bytes, big-endian; u32 per the DCC spec).
        let _ = stream.write_all(&(received as u32).to_be_bytes()).await;
        if size > 0 && received >= size {
            break;
        }
    }
    file.flush().await?;
    Ok(received)
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
    }

    #[test]
    fn formats_offers() {
        let ip = Ipv4Addr::new(192, 168, 0, 1);
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
        assert_eq!(parse_dcc_command("get"), Some(DccCommand::Get { nick: None }));
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
}
