//! Deferred SIGNAL turns. The interpreter handles `-n` synchronously; these
//! signals are submitted only after the caller's entire action list is applied.

use std::collections::VecDeque;

use tauri::{AppHandle, Manager};

use super::{apply_actions, script_data_dir, Action, EventVars, RunCtx, ScriptEngine};

const BATCH_SIZE: usize = 64;

#[derive(Debug)]
pub(super) struct PendingSignal {
    pub server_id: String,
    pub my_nick: String,
    pub network: String,
    pub server: String,
    pub name: String,
    pub params: Vec<String>,
}

#[derive(Default)]
pub(super) struct SignalQueue {
    pending: VecDeque<PendingSignal>,
    running: bool,
}

impl SignalQueue {
    /// The queue and worker flag are changed under the same engine mutex, so a
    /// submission racing worker shutdown cannot lose its wakeup.
    fn enqueue(&mut self, signals: Vec<PendingSignal>) -> bool {
        self.pending.extend(signals);
        if self.running || self.pending.is_empty() {
            return false;
        }
        self.running = true;
        true
    }

    fn take_batch(&mut self) -> Option<Vec<PendingSignal>> {
        if self.pending.is_empty() {
            self.running = false;
            return None;
        }
        let count = self.pending.len().min(BATCH_SIZE);
        Some(self.pending.drain(..count).collect())
    }
}

/// Retains the emitting connection even if a different window becomes active
/// before dispatch. The state snapshot and profile context are refreshed here.
fn run_signal(
    engine: &ScriptEngine,
    signal: &PendingSignal,
    data_dir: std::path::PathBuf,
    state: std::sync::Arc<crate::irc::state::StateSnapshot>,
    apply: impl FnOnce(&RunCtx<'_>, Vec<Action>),
) {
    let (network, server) = engine
        .connection_context(&signal.server_id)
        .unwrap_or_else(|| (signal.network.clone(), signal.server.clone()));
    let my_nick = if state.nick.is_empty() {
        &signal.my_nick
    } else {
        &state.nick
    };
    let ctx = RunCtx {
        server_id: &signal.server_id,
        my_nick,
        network: &network,
        server: &server,
        data_dir,
        state: state.clone(),
    };
    let event = EventVars {
        nick: my_nick.to_string(),
        chan: signal.name.clone(),
        target: signal.name.clone(),
        params: signal.params.clone(),
        ..Default::default()
    };
    let actions = engine.dispatch_event(&ctx, "SIGNAL", event);
    apply(&ctx, actions);
}

pub(super) fn enqueue(app: &AppHandle, signals: Vec<PendingSignal>) {
    if signals.is_empty() {
        return;
    }
    let start_worker = app
        .try_state::<ScriptEngine>()
        .is_some_and(|engine| engine.signals.lock().unwrap().enqueue(signals));
    if !start_worker {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let batch = app
                .try_state::<ScriptEngine>()
                .and_then(|engine| engine.signals.lock().unwrap().take_batch());
            let Some(batch) = batch else { break };
            // Detach this turn's entries. Signals emitted by their handlers go
            // onto the next turn, after already waiting entries, without Rust
            // recursion or a lock held while executing a script.
            for signal in batch {
                if let Some(engine) = app.try_state::<ScriptEngine>() {
                    let state = app
                        .try_state::<crate::irc::state::StateStore>()
                        .map(|store| store.get(&signal.server_id))
                        .unwrap_or_default();
                    run_signal(
                        &engine,
                        &signal,
                        script_data_dir(&app),
                        state,
                        |ctx, actions| {
                            apply_actions(
                                &app,
                                &signal.server_id,
                                ctx.my_nick,
                                ctx.network,
                                ctx.server,
                                actions,
                            );
                        },
                    );
                }
            }
            tokio::task::yield_now().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(name: &str) -> PendingSignal {
        PendingSignal {
            server_id: "one".into(),
            my_nick: "me".into(),
            network: "old network".into(),
            server: "old.server".into(),
            name: name.into(),
            params: vec!["payload".into()],
        }
    }

    #[test]
    fn deferred_signal_queue_is_fifo_and_coalesces_worker_wakeups() {
        let mut queue = SignalQueue::default();
        assert!(!queue.enqueue(vec![]));
        assert!(queue.enqueue(vec![pending("first"), pending("second")]));
        assert!(!queue.enqueue(vec![pending("third")]));
        let batch = queue.take_batch().unwrap();
        assert_eq!(
            batch
                .iter()
                .map(|signal| signal.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        // A handler can queue more work while the detached batch is running.
        assert!(!queue.enqueue(vec![pending("nested")]));
        assert_eq!(queue.take_batch().unwrap()[0].name, "nested");
        assert!(queue.take_batch().is_none());
        // A submission after shutdown starts a fresh worker.
        assert!(queue.enqueue(vec![pending("later")]));
    }

    #[test]
    fn deferred_signal_turns_have_a_bounded_batch() {
        let mut queue = SignalQueue::default();
        queue.enqueue(
            (0..BATCH_SIZE + 1)
                .map(|i| pending(&i.to_string()))
                .collect(),
        );
        let batch = queue.take_batch().unwrap();
        assert_eq!(batch.len(), BATCH_SIZE);
        assert!(!queue.enqueue(vec![pending("nested")]));
        let next = queue.take_batch().unwrap();
        assert_eq!(next[0].name, BATCH_SIZE.to_string());
        assert_eq!(next[1].name, "nested");
    }

    #[test]
    fn deferred_signal_refreshes_its_own_connection_context() {
        let engine = ScriptEngine::new();
        engine.assign_cid("one");
        engine.assign_cid("two");
        engine.set_connection_context("one", "first network", "first.server");
        engine.set_connection_context("two", "other network", "other.server");
        engine.set_active_conn("two");
        engine.load("on *:SIGNAL:refresh:{ echo -a $network $server $me $cid $signal $1 }");
        run_signal(
            &engine,
            &pending("refresh"),
            std::env::temp_dir(),
            std::sync::Arc::new(crate::irc::state::StateSnapshot {
                server_id: "one".into(),
                nick: "newnick".into(),
                ..Default::default()
            }),
            |ctx, actions| {
                assert_eq!(ctx.network, "first network");
                assert_eq!(
                    actions,
                    vec![Action::Echo {
                        target: "(active)".into(),
                        text: "first network first.server newnick 1 refresh payload".into(),
                    }]
                );
            },
        );
    }
}
