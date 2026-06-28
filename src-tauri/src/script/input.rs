//! `$input` — a modal text prompt the script engine shows *during* a run.
//!
//! mIRC's `$input` blocks until the user answers. jIRC's engine is synchronous,
//! so the production backend emits a `script-prompt` event (the frontend shows
//! the in-app prompt dialog) and **blocks** the script's thread on a channel
//! until a `script_prompt_reply` command delivers the answer. The script-run
//! commands that can reach `$input` run on a blocking thread, so this never
//! freezes the UI (the WebView and its dialog live on the main thread).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use super::eval::ScriptInput;

/// How long a `$input` waits for a reply before giving up (returns cancelled).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(600);

/// Pending `$input` prompts awaiting a UI reply, shared (Tauri managed state)
/// between the engine — which blocks on a prompt — and the reply command, which
/// fulfils it. Cloning shares the same map.
#[derive(Default, Clone)]
pub struct PromptRegistry {
    pending: Arc<Mutex<HashMap<u64, Sender<Option<String>>>>>,
    next: Arc<AtomicU64>,
}

impl PromptRegistry {
    /// Registers a new pending prompt, returning its id and the receiver the
    /// caller blocks on.
    fn register(&self) -> (u64, std::sync::mpsc::Receiver<Option<String>>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        (id, rx)
    }

    /// Delivers a UI reply (text, or `None` for cancel) to the prompt `id`.
    pub fn reply(&self, id: u64, value: Option<String>) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(value);
        }
    }
}

#[derive(Serialize, Clone)]
struct PromptReq {
    id: u64,
    message: String,
    title: String,
    default: String,
}

/// Production `$input` backend: emit the request, block for the reply.
pub struct EngineInput {
    app: AppHandle,
    registry: PromptRegistry,
}

impl EngineInput {
    pub fn new(app: AppHandle, registry: PromptRegistry) -> Self {
        Self { app, registry }
    }
}

impl ScriptInput for EngineInput {
    fn prompt(&self, message: &str, title: &str, default: &str) -> Option<String> {
        let (id, rx) = self.registry.register();
        let _ = self.app.emit(
            "script-prompt",
            PromptReq {
                id,
                message: message.to_string(),
                title: if title.is_empty() { "Input".to_string() } else { title.to_string() },
                default: default.to_string(),
            },
        );
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok(v) => v,
            Err(_) => {
                // Timed out / dialog never answered — clean up and cancel.
                self.registry.reply(id, None);
                None
            }
        }
    }
}

/// Frontend → backend: the user's answer to a `$input` prompt (`value` absent =
/// cancelled).
#[tauri::command]
pub fn script_prompt_reply(registry: State<'_, PromptRegistry>, id: u64, value: Option<String>) {
    registry.reply(id, value);
}
