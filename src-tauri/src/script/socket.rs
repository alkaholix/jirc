//! Script-controlled TCP sockets for mSL (`/sockopen`, `/socklisten`, `on
//! SOCKREAD`, …).
//!
//! Each connected socket runs as an async task that reads newline-delimited
//! lines (firing `on SOCKREAD` per line), forwards outgoing writes (firing `on
//! SOCKWRITE` when the send buffer drains), and tracks per-socket stats for
//! `$sock(name).property`. A listening socket (`/socklisten`) is bound
//! synchronously — so its port is immediately readable via `$sock(name).port` —
//! and its accept loop is started separately ([`start_listener`]) with the
//! owning connection's context, so an incoming connection fires `on SOCKLISTEN`;
//! the handler's `/sockaccept <name>` then turns the pending connection into a
//! named connected socket.
//!
//! Stored as Tauri managed state, mirroring [`crate::irc::ConnectionManager`].
//! Sockets belong to the connection that created them. Plain TCP (TLS for `-e`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::eval::{wildcard_match, EventVars};
use super::{apply_actions, script_data_dir, RunCtx, ScriptEngine};
use crate::irc::stream::NetStream;

struct SockHandle {
    /// Outgoing channel — `None` for a pure listening socket.
    outgoing: Option<UnboundedSender<Vec<u8>>>,
    task: tauri::async_runtime::JoinHandle<()>,
    port: u16,
    mark: String,
    listening: bool,
    tls: bool,
    /// Named address passed to `/sockopen` (`$sock().addr`).
    addr: String,
    /// Peer IP (`$sock().ip`).
    ip: String,
    sent: u64,
    rcvd: u64,
    opened: Instant,
    last_sent: Instant,
    last_rcvd: Instant,
    paused: bool,
    /// Last error message (`$sock().wsmsg`).
    wsmsg: String,
}

impl SockHandle {
    fn new(outgoing: Option<UnboundedSender<Vec<u8>>>, task: tauri::async_runtime::JoinHandle<()>, port: u16, listening: bool) -> Self {
        let now = Instant::now();
        SockHandle {
            outgoing,
            task,
            port,
            mark: String::new(),
            listening,
            tls: false,
            addr: String::new(),
            ip: String::new(),
            sent: 0,
            rcvd: 0,
            opened: now,
            last_sent: now,
            last_rcvd: now,
            paused: false,
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
            "status" => if self.listening { "listening" } else { "active" }.to_string(),
            "type" => "tcp".to_string(),
            "ssl" => bool_id(self.tls),
            "pause" => bool_id(self.paused),
            "sent" => self.sent.to_string(),
            "rcvd" => self.rcvd.to_string(),
            "sq" | "rq" => "0".to_string(),
            "ls" => secs(self.last_sent),
            "lr" => secs(self.last_rcvd),
            "to" => secs(self.opened),
            "wserr" => "0".to_string(),
            "wsmsg" => self.wsmsg.clone(),
            "saddr" | "sport" => String::new(), // UDP only
            _ => String::new(),
        }
    }
}

fn bool_id(b: bool) -> String {
    if b { "$true" } else { "$false" }.to_string()
}

#[derive(Default)]
pub struct SocketManager {
    socks: Mutex<HashMap<String, SockHandle>>,
    /// Listeners bound by `/socklisten`, awaiting their accept loop to start.
    bound: Mutex<HashMap<String, (std::net::TcpListener, u16)>>,
    /// listener name -> the name a `/sockaccept` assigned in `on SOCKLISTEN`.
    accept_names: Mutex<HashMap<String, String>>,
}

impl SocketManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a TCP socket named `name` to `host:port`, replacing any existing one.
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
    ) {
        if let Some(old) = self.socks.lock().unwrap().remove(&name) {
            old.task.abort();
        }
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let key = name.clone();
        let host_for_task = host.clone();
        let task = tauri::async_runtime::spawn(async move {
            let tcp = match TcpStream::connect((host_for_task.as_str(), port)).await {
                Ok(s) => s,
                Err(e) => {
                    set_wsmsg(&app, &name, &e.to_string());
                    fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, &e.to_string());
                    forget(&app, &name);
                    return;
                }
            };
            let peer = tcp.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            set_ip(&app, &name, &peer);
            let stream = if tls {
                match crate::irc::stream::tls_client(&host_for_task, tcp).await {
                    Ok(s) => s,
                    Err(e) => {
                        set_wsmsg(&app, &name, &e.to_string());
                        fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, &e.to_string());
                        forget(&app, &name);
                        return;
                    }
                }
            } else {
                NetStream::Plain(tcp)
            };
            fire(&app, &server_id, &network, &nick, "SOCKOPEN", &name, "");
            run_connected(app, server_id, network, nick, name, stream, rx).await;
        });
        let mut h = SockHandle::new(Some(tx), task, port, false);
        h.addr = host;
        h.tls = tls;
        self.socks.lock().unwrap().insert(key, h);
    }

    /// Binds a listening socket synchronously (so `$sock(name).port` is readable
    /// on the same line, like mIRC). `port == 0` lets the OS assign one.
    pub fn listen(&self, name: &str, port: u16) -> Option<u16> {
        let listener = std::net::TcpListener::bind(("0.0.0.0", port)).ok()?;
        let bound_port = listener.local_addr().ok()?.port();
        listener.set_nonblocking(true).ok()?;
        self.bound.lock().unwrap().insert(name.to_string(), (listener, bound_port));
        Some(bound_port)
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
    ) {
        let Some((std_listener, port)) = self.bound.lock().unwrap().remove(&name) else {
            return;
        };
        if let Some(old) = self.socks.lock().unwrap().remove(&name) {
            old.task.abort();
        }
        let key = name.clone();
        let task = tauri::async_runtime::spawn(async move {
            let listener = match TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, &e.to_string());
                    forget(&app, &name);
                    return;
                }
            };
            loop {
                let (stream, _addr) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                if let Some(m) = app.try_state::<SocketManager>() {
                    m.accept_names.lock().unwrap().remove(&name);
                }
                fire(&app, &server_id, &network, &nick, "SOCKLISTEN", &name, "");
                let accepted = app
                    .try_state::<SocketManager>()
                    .and_then(|m| m.accept_names.lock().unwrap().remove(&name));
                if let Some(newname) = accepted {
                    let peer = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
                    spawn_connected(
                        app.clone(),
                        server_id.clone(),
                        network.clone(),
                        nick.clone(),
                        newname,
                        NetStream::Plain(stream),
                        peer,
                    );
                }
            }
            fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, "");
            forget(&app, &name);
        });
        self.socks.lock().unwrap().insert(key, SockHandle::new(None, task, port, true));
    }

    /// Records the name a `/sockaccept` assigned to a listener's pending connection.
    pub fn accept(&self, listener: &str, name: &str) {
        self.accept_names.lock().unwrap().insert(listener.to_string(), name.to_string());
    }

    /// Writes `data` to `name`, or every socket matching it when `name` is a wildcard.
    pub fn write(&self, name: &str, data: Vec<u8>) {
        let socks = self.socks.lock().unwrap();
        if let Some(h) = socks.get(name) {
            if let Some(tx) = &h.outgoing {
                let _ = tx.send(data);
            }
            return;
        }
        if name.contains(['*', '?']) {
            for (k, h) in socks.iter() {
                if wildcard_match(name, k) {
                    if let Some(tx) = &h.outgoing {
                        let _ = tx.send(data.clone());
                    }
                }
            }
        }
    }

    /// Closes sockets whose name matches `pattern` (plain name or wildcard).
    pub fn close(&self, pattern: &str) {
        self.bound.lock().unwrap().retain(|k, _| !wildcard_match(pattern, k));
        let mut socks = self.socks.lock().unwrap();
        let matched: Vec<String> =
            socks.keys().filter(|k| wildcard_match(pattern, k)).cloned().collect();
        for name in matched {
            if let Some(h) = socks.remove(&name) {
                h.task.abort();
            }
        }
    }

    /// `/sockrename <name> <newname>`.
    pub fn rename(&self, name: &str, newname: &str) {
        // Bind the removed value to a `let` first so the lock guard is dropped
        // before we re-lock to insert (re-locking the same Mutex would deadlock).
        let moved = self.socks.lock().unwrap().remove(name);
        if let Some(h) = moved {
            self.socks.lock().unwrap().insert(newname.to_string(), h);
            return;
        }
        let moved = self.bound.lock().unwrap().remove(name);
        if let Some(b) = moved {
            self.bound.lock().unwrap().insert(newname.to_string(), b);
        }
    }

    /// `/sockpause [-r] <name>` — pause or (with `resume`) restart reading.
    pub fn pause(&self, name: &str, resume: bool) {
        if let Some(h) = self.socks.lock().unwrap().get_mut(name) {
            h.paused = !resume;
        }
    }

    pub fn set_mark(&self, name: &str, mark: &str) {
        if let Some(h) = self.socks.lock().unwrap().get_mut(name) {
            h.mark = mark.to_string();
        }
    }

    /// `$sock(name).property` value (empty for unknown name/property).
    pub fn prop(&self, name: &str, property: &str) -> String {
        if let Some(h) = self.socks.lock().unwrap().get(name) {
            return h.prop(name, property);
        }
        // A bound-but-not-yet-started listener.
        if let Some((_, port)) = self.bound.lock().unwrap().get(name) {
            return match property.to_ascii_lowercase().as_str() {
                "name" => name.to_string(),
                "port" => port.to_string(),
                "status" => "listening".to_string(),
                "type" => "tcp".to_string(),
                _ => String::new(),
            };
        }
        String::new()
    }

    /// Names of sockets for `/socklist` — `filter` may carry `-l` (listening only)
    /// and/or a trailing name/wildcard.
    pub fn list(&self, filter: &str) -> Vec<String> {
        let listening_only = filter.split_whitespace().any(|t| t == "-l" || t.starts_with("-l"));
        let pat = filter
            .split_whitespace()
            .find(|t| !t.starts_with('-'))
            .unwrap_or("*");
        let mut out: Vec<String> = Vec::new();
        for (name, h) in self.socks.lock().unwrap().iter() {
            if listening_only && !h.listening {
                continue;
            }
            if wildcard_match(pat, name) {
                let status = if h.listening { "listening" } else { "active" };
                out.push(format!("{name}  {status}  port {}", h.port));
            }
        }
        for (name, (_, port)) in self.bound.lock().unwrap().iter() {
            if wildcard_match(pat, name) {
                out.push(format!("{name}  listening  port {port}"));
            }
        }
        out.sort();
        out
    }

    pub fn exists(&self, name: &str) -> bool {
        if self.socks.lock().unwrap().keys().any(|k| wildcard_match(name, k)) {
            return true;
        }
        self.bound.lock().unwrap().keys().any(|k| wildcard_match(name, k))
    }

    /// All open socket names (incl. bound listeners), sorted — for the frontend
    /// socket list (`script_sockets`).
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.socks.lock().unwrap().keys().cloned().collect();
        names.extend(self.bound.lock().unwrap().keys().cloned());
        names.sort();
        names.dedup();
        names
    }
}

/// Updates a socket's I/O stats (called by the read/write loop).
fn bump(app: &AppHandle, name: &str, sent: u64, rcvd: u64) {
    if let Some(m) = app.try_state::<SocketManager>() {
        if let Some(h) = m.socks.lock().unwrap().get_mut(name) {
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

fn set_ip(app: &AppHandle, name: &str, ip: &str) {
    if let Some(m) = app.try_state::<SocketManager>() {
        if let Some(h) = m.socks.lock().unwrap().get_mut(name) {
            h.ip = ip.to_string();
        }
    }
}

fn set_wsmsg(app: &AppHandle, name: &str, msg: &str) {
    if let Some(m) = app.try_state::<SocketManager>() {
        if let Some(h) = m.socks.lock().unwrap().get_mut(name) {
            h.wsmsg = msg.to_string();
        }
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
) {
    if let Some(m) = app.try_state::<SocketManager>() {
        if let Some(old) = m.socks.lock().unwrap().remove(&name) {
            old.task.abort();
        }
    }
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let app2 = app.clone();
    let key = name.clone();
    let task = tauri::async_runtime::spawn(run_connected(app, server_id, network, nick, name, stream, rx));
    if let Some(m) = app2.try_state::<SocketManager>() {
        let mut h = SockHandle::new(Some(tx), task, 0, false);
        h.ip = peer_ip;
        m.socks.lock().unwrap().insert(key, h);
    }
}

/// The read/write loop shared by connect and accepted sockets.
async fn run_connected(
    app: AppHandle,
    server_id: String,
    network: String,
    nick: String,
    name: String,
    stream: NetStream,
    mut rx: UnboundedReceiver<Vec<u8>>,
) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut buf: Vec<u8> = Vec::new();
    'outer: loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(mut data) => {
                    // Drain and send everything currently queued, then fire
                    // on SOCKWRITE once (mIRC: "finished sending all queued data").
                    loop {
                        if write_half.write_all(&data).await.is_err() {
                            break 'outer;
                        }
                        bump(&app, &name, data.len() as u64, 0);
                        match rx.try_recv() {
                            Ok(more) => data = more,
                            Err(_) => break,
                        }
                    }
                    fire(&app, &server_id, &network, &nick, "SOCKWRITE", &name, "");
                }
                None => break,
            },
            res = reader.read_until(b'\n', &mut buf) => match res {
                Ok(0) => break, // EOF
                Ok(n) => {
                    bump(&app, &name, 0, n as u64);
                    // Raw line bytes minus the trailing CR/LF (mIRC strips the
                    // terminator): a decoded text view for `%var` reads, and the
                    // exact bytes for `&binvar` reads (binary protocols).
                    let mut end = buf.len();
                    while end > 0 && matches!(buf[end - 1], b'\n' | b'\r') {
                        end -= 1;
                    }
                    let text = decode_line(&buf[..end]);
                    fire_read(&app, &server_id, &network, &nick, &name, &text, &buf[..end]);
                    buf.clear();
                }
                Err(_) => break,
            },
        }
    }
    fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, "");
    forget(&app, &name);
}

fn forget(app: &AppHandle, name: &str) {
    if let Some(m) = app.try_state::<SocketManager>() {
        m.socks.lock().unwrap().remove(name);
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
    let actions = engine.dispatch_event(&ctx, "SOCKREAD", vars);
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
}

impl super::eval::ScriptSockets for EngineSockets {
    fn listen(&self, name: &str, port: u16) -> Option<u16> {
        self.mgr()?.listen(name, port)
    }
    fn accept(&self, name: &str, listener: &str) -> bool {
        match self.mgr() {
            Some(m) => {
                m.accept(listener, name);
                true
            }
            None => false,
        }
    }
    fn set_mark(&self, name: &str, mark: &str) {
        if let Some(m) = self.mgr() {
            m.set_mark(name, mark);
        }
    }
    fn rename(&self, name: &str, newname: &str) {
        if let Some(m) = self.mgr() {
            m.rename(name, newname);
        }
    }
    fn pause(&self, name: &str, resume: bool) {
        if let Some(m) = self.mgr() {
            m.pause(name, resume);
        }
    }
    fn exists(&self, name: &str) -> bool {
        self.mgr().map(|m| m.exists(name)).unwrap_or(false)
    }
    fn prop(&self, name: &str, property: &str) -> String {
        self.mgr().map(|m| m.prop(name, property)).unwrap_or_default()
    }
    fn list(&self, filter: &str) -> Vec<String> {
        self.mgr().map(|m| m.list(filter)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_bind_props_rename_close() {
        let m = SocketManager::new();
        let port = m.listen("relay", 0).expect("bind a local listener");
        assert!(port > 0);
        assert_eq!(m.prop("relay", "port"), port.to_string());
        assert_eq!(m.prop("relay", "status"), "listening");
        assert_eq!(m.prop("relay", "name"), "relay");
        assert_eq!(m.prop("relay", "type"), "tcp");
        assert!(m.exists("rel*"));
        assert!(m.list("*").iter().any(|l| l.contains("relay")));
        m.rename("relay", "rl2");
        assert!(!m.exists("relay"));
        assert_eq!(m.prop("rl2", "port"), port.to_string());
        m.close("rl2");
        assert!(!m.exists("rl2"));
    }
}
