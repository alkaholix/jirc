//! Owns all live IRC connections and routes outgoing lines to them.
//!
//! Stored as Tauri managed state. Each connection runs as an async task; the
//! manager keeps the outgoing channel and a handle to abort it. When a task
//! ends on its own (server closed the link) it removes itself from the map.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::{self, UnboundedSender};
use uuid::Uuid;

use crate::config::ServerProfile;
use crate::irc::connection;

struct ConnHandle {
    outgoing: UnboundedSender<String>,
    task: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Default)]
pub struct ConnectionManager {
    conns: Mutex<HashMap<String, ConnHandle>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a connection for `profile`, returning its server id. If a
    /// connection with the same id already exists it is replaced.
    pub fn connect(&self, app: AppHandle, mut profile: ServerProfile) -> Result<String, String> {
        let server_id = profile
            .id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        profile.id = Some(server_id.clone());

        // Replace any prior handle for this id.
        if let Some(old) = self.conns.lock().unwrap().remove(&server_id) {
            old.task.abort();
        }

        let (tx, rx) = mpsc::unbounded_channel::<String>();

        let app_run = app.clone();
        let id_run = server_id.clone();
        let id_cleanup = server_id.clone();
        let task = tauri::async_runtime::spawn(async move {
            connection::supervise(app_run.clone(), id_run, profile, rx).await;
            // Self-cleanup once the connection ends naturally.
            if let Some(mgr) = app_run.try_state::<ConnectionManager>() {
                mgr.forget(&id_cleanup);
            }
        });

        self.conns
            .lock()
            .unwrap()
            .insert(server_id.clone(), ConnHandle { outgoing: tx, task });
        Ok(server_id)
    }

    /// Queues a raw line to be sent on the given connection.
    pub fn send(&self, server_id: &str, line: String) -> Result<(), String> {
        let conns = self.conns.lock().unwrap();
        let handle = conns
            .get(server_id)
            .ok_or_else(|| format!("no such connection: {server_id}"))?;
        handle
            .outgoing
            .send(line)
            .map_err(|_| "connection is closed".to_string())
    }

    /// Sends QUIT and tears down the connection.
    pub fn disconnect(&self, server_id: &str, quit_msg: Option<String>) -> Result<(), String> {
        let handle = self.conns.lock().unwrap().remove(server_id);
        let handle = handle.ok_or_else(|| format!("no such connection: {server_id}"))?;
        let msg = quit_msg.unwrap_or_else(|| "Leaving".to_string());
        let _ = handle.outgoing.send(format!("QUIT :{msg}"));
        handle.task.abort();
        Ok(())
    }

    /// Removes a connection entry without aborting (used for self-cleanup).
    fn forget(&self, server_id: &str) {
        self.conns.lock().unwrap().remove(server_id);
    }

    pub fn list(&self) -> Vec<String> {
        self.conns.lock().unwrap().keys().cloned().collect()
    }
}
