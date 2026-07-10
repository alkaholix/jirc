//! Named, stoppable script timers (`/timer`, `/timers`).
//!
//! Each timer runs as an async task that fires its command `reps` times. The
//! manager keeps a handle per name so timers can be listed (`/timers`) and
//! stopped (`/timer name off`, `/timers off`). Stored as Tauri managed state.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use super::{apply_actions, script_data_dir, RunCtx, ScriptEngine};
use crate::irc::ConnectionManager;

#[derive(Default)]
pub struct TimerManager {
    timers: Mutex<HashMap<String, TimerEntry>>,
    counter: Mutex<u64>,
}

/// A running timer: its task handle plus the metadata `$timer` reports.
struct TimerEntry {
    handle: tauri::async_runtime::JoinHandle<()>,
    command: String,
    reps: u32,
    /// Delay between fires, in seconds.
    delay: u64,
}

impl TimerManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn auto_name(&self) -> String {
        let mut c = self.counter.lock().unwrap();
        *c += 1;
        format!("_t{}", *c)
    }

    /// Starts (or replaces) a named timer. An empty `name` is auto-assigned.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        app: AppHandle,
        server_id: String,
        my_nick: String,
        network: String,
        server: String,
        name: String,
        reps: u32,
        interval_ms: u64,
        command: String,
        target: String,
    ) {
        let name = if name.is_empty() { self.auto_name() } else { name };
        if let Some(old) = self.timers.lock().unwrap().remove(&name) {
            old.handle.abort();
        }
        let task_name = name.clone();
        let entry_command = command.clone();
        let task = tauri::async_runtime::spawn(async move {
            for _ in 0..reps {
                tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
                // Stop if the connection that owns this timer is gone.
                match app.try_state::<ConnectionManager>() {
                    Some(m) if m.list().iter().any(|id| id == &server_id) => {}
                    _ => break,
                }
                let Some(engine) = app.try_state::<ScriptEngine>() else {
                    break;
                };
                let ctx = RunCtx {
                    my_nick: &my_nick,
                    network: &network,
                    server: &server,
                    data_dir: script_data_dir(&app),
                    state: app
                        .try_state::<crate::irc::state::StateStore>()
                        .map(|s| s.get(&server_id))
                        .unwrap_or_default(),
                };
                let actions = engine.run_command(&ctx, &target, &command, &[]);
                apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
            }
            // Self-cleanup once finished.
            if let Some(m) = app.try_state::<TimerManager>() {
                m.timers.lock().unwrap().remove(&task_name);
            }
        });
        self.timers.lock().unwrap().insert(
            name,
            TimerEntry { handle: task, command: entry_command, reps, delay: interval_ms / 1000 },
        );
    }

    /// Stops a timer by name, or all timers when `name` is "*".
    pub fn stop(&self, name: &str) {
        let mut timers = self.timers.lock().unwrap();
        if name == "*" {
            for (_, e) in timers.drain() {
                e.handle.abort();
            }
        } else if let Some(e) = timers.remove(name) {
            e.handle.abort();
        }
    }

    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.timers.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    /// A snapshot of every active timer (sorted by name), for `$timer`.
    pub fn snapshot(&self) -> Vec<super::eval::TimerInfo> {
        let mut out: Vec<super::eval::TimerInfo> = self
            .timers
            .lock()
            .unwrap()
            .iter()
            .map(|(name, e)| super::eval::TimerInfo {
                name: name.clone(),
                command: e.command.clone(),
                reps: e.reps,
                delay: e.delay,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Bridges the engine's `$timer` reads to the Tauri-managed [`TimerManager`].
pub struct EngineTimers {
    app: AppHandle,
}

impl EngineTimers {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl super::eval::ScriptTimers for EngineTimers {
    fn snapshot(&self) -> Vec<super::eval::TimerInfo> {
        self.app.try_state::<TimerManager>().map(|m| m.snapshot()).unwrap_or_default()
    }
}
