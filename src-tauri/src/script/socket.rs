//! Script-controlled TCP sockets for mSL (`/sockopen`, `/socklisten`, `on
//! SOCKREAD`, …).
//!
//! Each connected socket runs as an async task that reads newline-delimited
//! lines (firing `on SOCKREAD` per line) and accepts outgoing writes. A
//! listening socket (`/socklisten`) is bound synchronously — so its port is
//! immediately readable via `$sock(name).port` — and its accept loop is started
//! separately ([`start_listener`]) with the owning connection's context, so an
//! incoming connection fires `on SOCKLISTEN`; the handler's `/sockaccept <name>`
//! then turns the pending connection into a named connected socket.
//!
//! Stored as Tauri managed state, mirroring [`crate::irc::ConnectionManager`].
//! Sockets belong to the connection that created them: their script events are
//! applied with that server's id, so `/msg #chan` from a socket handler routes
//! to the right network. Plain TCP (TLS for `/sockopen -e`).

use std::collections::HashMap;
use std::sync::Mutex;

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
    /// Bound/remote port, for `$sock(name).port`.
    port: u16,
    /// `/sockmark` text, for `$sock(name).mark`.
    mark: String,
    listening: bool,
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

    /// Opens a TCP socket named `name` to `host:port`, replacing any existing
    /// socket with the same name.
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
        let task = tauri::async_runtime::spawn(async move {
            let tcp = match TcpStream::connect((host.as_str(), port)).await {
                Ok(s) => s,
                Err(e) => {
                    fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, &e.to_string());
                    forget(&app, &name);
                    return;
                }
            };
            let stream = if tls {
                match crate::irc::stream::tls_client(&host, tcp).await {
                    Ok(s) => s,
                    Err(e) => {
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
        self.socks.lock().unwrap().insert(
            key,
            SockHandle { outgoing: Some(tx), task, port, mark: String::new(), listening: false },
        );
    }

    /// Binds a listening socket synchronously (so `$sock(name).port` is readable
    /// on the same line, like mIRC). `port == 0` lets the OS assign one. The
    /// accept loop is started later via [`start_listener`].
    pub fn listen(&self, name: &str, port: u16) -> Option<u16> {
        let listener = std::net::TcpListener::bind(("0.0.0.0", port)).ok()?;
        let bound_port = listener.local_addr().ok()?.port();
        listener.set_nonblocking(true).ok()?;
        self.bound.lock().unwrap().insert(name.to_string(), (listener, bound_port));
        Some(bound_port)
    }

    /// Starts the accept loop for a listener bound by [`listen`], with the owning
    /// connection's context (so events route correctly). Called from apply-time.
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
                // Fire on SOCKLISTEN; the handler's /sockaccept records a name.
                if let Some(m) = app.try_state::<SocketManager>() {
                    m.accept_names.lock().unwrap().remove(&name);
                }
                fire(&app, &server_id, &network, &nick, "SOCKLISTEN", &name, "");
                let accepted = app
                    .try_state::<SocketManager>()
                    .and_then(|m| m.accept_names.lock().unwrap().remove(&name));
                if let Some(newname) = accepted {
                    spawn_connected(
                        app.clone(),
                        server_id.clone(),
                        network.clone(),
                        nick.clone(),
                        newname,
                        NetStream::Plain(stream),
                    );
                }
            }
            fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, "");
            forget(&app, &name);
        });
        self.socks.lock().unwrap().insert(
            key,
            SockHandle { outgoing: None, task, port, mark: String::new(), listening: true },
        );
    }

    /// Records the name a `/sockaccept` assigned to a listener's pending
    /// connection (read by the accept loop after `on SOCKLISTEN`).
    pub fn accept(&self, listener: &str, name: &str) {
        self.accept_names.lock().unwrap().insert(listener.to_string(), name.to_string());
    }

    /// Writes `data` to the socket named `name`, or to every socket matching it
    /// when `name` is a wildcard (e.g. `bot.*`).
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

    /// Closes sockets whose name matches `pattern` (a plain name or wildcard).
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

    /// Names of all open sockets (incl. bound listeners), sorted.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.socks.lock().unwrap().keys().cloned().collect();
        names.extend(self.bound.lock().unwrap().keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    pub fn set_mark(&self, name: &str, mark: &str) {
        if let Some(h) = self.socks.lock().unwrap().get_mut(name) {
            h.mark = mark.to_string();
        }
    }

    pub fn mark(&self, name: &str) -> String {
        self.socks.lock().unwrap().get(name).map(|h| h.mark.clone()).unwrap_or_default()
    }

    pub fn port(&self, name: &str) -> Option<u16> {
        if let Some(h) = self.socks.lock().unwrap().get(name) {
            return Some(h.port);
        }
        self.bound.lock().unwrap().get(name).map(|(_, p)| *p)
    }

    pub fn status(&self, name: &str) -> String {
        if let Some(h) = self.socks.lock().unwrap().get(name) {
            return if h.listening { "listening" } else { "active" }.to_string();
        }
        if self.bound.lock().unwrap().contains_key(name) {
            return "listening".to_string();
        }
        String::new()
    }

    pub fn exists(&self, name: &str) -> bool {
        if self.socks.lock().unwrap().keys().any(|k| wildcard_match(name, k)) {
            return true;
        }
        self.bound.lock().unwrap().keys().any(|k| wildcard_match(name, k))
    }
}

/// Spawns a connected-socket read/write task for an already-open stream (used by
/// `/sockaccept`) and registers it.
fn spawn_connected(
    app: AppHandle,
    server_id: String,
    network: String,
    nick: String,
    name: String,
    stream: NetStream,
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
        m.socks.lock().unwrap().insert(
            key,
            SockHandle { outgoing: Some(tx), task, port: 0, mark: String::new(), listening: false },
        );
    }
}

/// The read/write loop shared by connect and accepted sockets: fires `on
/// SOCKREAD` per inbound line, forwards outgoing writes, and `on SOCKCLOSE` at end.
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
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(data) => {
                    if write_half.write_all(&data).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            res = reader.read_until(b'\n', &mut buf) => match res {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let line = decode_line(&buf);
                    buf.clear();
                    let line = line.trim_end_matches(['\r', '\n']);
                    fire(&app, &server_id, &network, &nick, "SOCKREAD", &name, line);
                }
                Err(_) => break,
            },
        }
    }
    fire(&app, &server_id, &network, &nick, "SOCKCLOSE", &name, "");
    forget(&app, &name);
}

/// Removes a socket entry without aborting (used for self-cleanup when a task
/// ends on its own).
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

/// Production [`super::eval::ScriptSockets`] backend, backed by the
/// [`SocketManager`] (Tauri managed state). Installed on the engine at startup.
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
    fn mark(&self, name: &str) -> String {
        self.mgr().map(|m| m.mark(name)).unwrap_or_default()
    }
    fn port(&self, name: &str) -> Option<u16> {
        self.mgr()?.port(name)
    }
    fn status(&self, name: &str) -> String {
        self.mgr().map(|m| m.status(name)).unwrap_or_default()
    }
    fn exists(&self, name: &str) -> bool {
        self.mgr().map(|m| m.exists(name)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_binds_and_reports_port() {
        let m = SocketManager::new();
        // /socklisten with port 0 binds an OS-assigned port, readable at once.
        let port = m.listen("relay", 0).expect("bind a local listener");
        assert!(port > 0);
        assert_eq!(m.port("relay"), Some(port));
        assert!(m.exists("relay"));
        assert!(m.exists("rel*")); // wildcard existence
        assert_eq!(m.status("relay"), "listening");
        assert!(m.names().contains(&"relay".to_string()));
        // /sockclose drops the bound listener.
        m.close("relay");
        assert!(!m.exists("relay"));
        assert_eq!(m.port("relay"), None);
    }
}
