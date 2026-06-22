//! Script-controlled TCP sockets for mSL (`/sockopen`, `on SOCKREAD`, …).
//!
//! Each socket runs as an async task that connects, reads newline-delimited
//! lines (firing `on SOCKREAD` per line), and accepts outgoing writes. Stored as
//! Tauri managed state, mirroring [`crate::irc::ConnectionManager`]. Sockets are
//! line-oriented and plain TCP (no TLS yet).
//!
//! Sockets belong to the connection that opened them: their script events are
//! applied with that server's id, so `/msg #chan` from a socket handler routes
//! to the right network.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedSender};

use super::eval::{wildcard_match, EventVars};
use super::{apply_actions, script_data_dir, RunCtx, ScriptEngine};
use crate::irc::stream::NetStream;

struct SockHandle {
    outgoing: UnboundedSender<Vec<u8>>,
    task: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Default)]
pub struct SocketManager {
    socks: Mutex<HashMap<String, SockHandle>>,
}

impl SocketManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a TCP socket named `name` to `host:port`, replacing any existing
    /// socket with the same name. `server_id`/`network`/`nick` give the socket's
    /// script events a connection context.
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
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
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
            // Wrap in TLS for `/sockopen -e`; otherwise stay plain.
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
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            fire(&app, &server_id, &network, &nick, "SOCKOPEN", &name, "");

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
        });
        self.socks.lock().unwrap().insert(key, SockHandle { outgoing: tx, task });
    }

    /// Writes `data` to the socket named `name`, or to every socket matching it
    /// when `name` is a wildcard (e.g. `bot.*`).
    pub fn write(&self, name: &str, data: Vec<u8>) {
        let socks = self.socks.lock().unwrap();
        if let Some(h) = socks.get(name) {
            let _ = h.outgoing.send(data);
            return;
        }
        if name.contains(['*', '?']) {
            for (k, h) in socks.iter() {
                if wildcard_match(name, k) {
                    let _ = h.outgoing.send(data.clone());
                }
            }
        }
    }

    /// Closes sockets whose name matches `pattern` (a plain name or wildcard
    /// like `*`).
    pub fn close(&self, pattern: &str) {
        let mut socks = self.socks.lock().unwrap();
        let matched: Vec<String> = socks
            .keys()
            .filter(|k| wildcard_match(pattern, k))
            .cloned()
            .collect();
        for name in matched {
            if let Some(h) = socks.remove(&name) {
                h.task.abort();
            }
        }
    }

    /// Names of all open sockets, sorted.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.socks.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }
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
