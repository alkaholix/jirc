//! Named, stoppable script timers (`/timer`, `/timers`).
//!
//! Each timer runs as an async task that fires its command `reps` times. The
//! manager keeps a handle per name so timers can be listed (`/timers`) and
//! stopped (`/timer name off`, `/timers off`). Stored as Tauri managed state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Local, NaiveDateTime, NaiveTime};
use tauri::{AppHandle, Manager};

use super::{apply_actions, script_data_dir, RunCtx, ScriptEngine};
use crate::irc::ConnectionManager;

/// Returns whether a timer name matches mIRC's case-insensitive `*`/`?`
/// wildcard syntax. `/timer3? off` and `/timerfoo* -e` both use this path.
fn name_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let value: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let (mut p, mut v, mut star, mut retry) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(s) = star {
            p = s + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Milliseconds from `now` until the next occurrence of an mIRC timer wall
/// clock (`HH:nn` or `HH:nn:ss`). A time that has already passed means the
/// following day, as it does for a scheduled `/timer`.
fn wall_clock_delay_ms_at(spec: &str, now: NaiveDateTime) -> Option<u64> {
    let time = parse_wall_clock(spec)?;
    let mut at = now.date().and_time(time);
    if at <= now {
        at = at.checked_add_days(chrono::Days::new(1))?;
    }
    Some((at - now).num_milliseconds().max(0) as u64)
}

fn parse_wall_clock(spec: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(spec, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(spec, "%H:%M"))
        .ok()
}

pub(crate) fn is_wall_clock_spec(spec: &str) -> bool {
    parse_wall_clock(spec).is_some()
}

fn wall_clock_delay_ms(spec: &str) -> Option<u64> {
    let now = Local::now();
    let local = NaiveDateTime::new(now.date_naive(), now.time());
    wall_clock_delay_ms_at(spec, local)
}

/// Chooses the connection a timer currently belongs to. Dynamic timers prefer
/// the active connection, then fall back in stable `$cid` order (mIRC's "next
/// available server window"), never HashMap iteration order.
fn choose_timer_connection(
    original_server_id: &str,
    dynamic: bool,
    offline: bool,
    active_server_id: Option<&str>,
    cid_order: &[String],
    mut is_registered: impl FnMut(&str) -> bool,
) -> Option<String> {
    if dynamic {
        if let Some(active) = active_server_id.filter(|id| is_registered(id)) {
            return Some(active.to_string());
        }
        if let Some(server_id) = cid_order.iter().find(|id| is_registered(id)) {
            return Some(server_id.clone());
        }
    } else if is_registered(original_server_id) {
        return Some(original_server_id.to_string());
    }
    offline.then(|| original_server_id.to_string())
}

/// A manager entry alone is not a live IRC session: it remains present while
/// `supervise` waits to reconnect. A session counts as registered only while
/// both its manager task and its post-001 state snapshot exist.
fn is_registered_session(app: &AppHandle, server_id: &str) -> bool {
    let managed = app
        .try_state::<ConnectionManager>()
        .is_some_and(|manager| manager.list().iter().any(|id| id == server_id));
    managed
        && app
            .try_state::<crate::irc::state::StateStore>()
            .is_some_and(|store| {
                let state = store.get(server_id);
                state.server_id == server_id && state.connect_time != 0
            })
}

fn select_timer_connection(
    app: &AppHandle,
    engine: &ScriptEngine,
    original_server_id: &str,
    dynamic: bool,
    offline: bool,
) -> Option<String> {
    let active = engine.active_connection();
    let cid_order = engine.connections_in_cid_order();
    choose_timer_connection(
        original_server_id,
        dynamic,
        offline,
        active.as_deref(),
        &cid_order,
        |server_id| is_registered_session(app, server_id),
    )
}

#[derive(Default)]
pub struct TimerManager {
    timers: Mutex<HashMap<String, TimerEntry>>,
    ordered: Arc<tokio::sync::Mutex<()>>,
    ordered_counter: Mutex<u64>,
    last: Mutex<String>,
}

/// A running timer: its task handle plus the metadata `$timer` reports.
struct TimerEntry {
    handle: tauri::async_runtime::JoinHandle<()>,
    command: String,
    interval_ms: u64,
    milliseconds: bool,
    offline: bool,
    high_resolution: bool,
    dynamic: bool,
    server_id: String,
    /// Current mIRC-style numeric connection id. Dynamic timers update this
    /// whenever they follow the active connection.
    cid: Arc<AtomicU32>,
    control: Arc<TimerControl>,
    next_fire: Arc<Mutex<Option<tokio::time::Instant>>>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum PauseMode {
    #[default]
    Running,
    /// `-p`: the schedule advances, but commands are not executed.
    Execution,
    /// `-P`: both execution and the countdown stop.
    Countdown,
}

struct TimerState {
    remaining: Option<u32>,
    pause: PauseMode,
    execute_requests: u32,
}

struct TimerControl {
    state: Mutex<TimerState>,
    notify: tokio::sync::Notify,
}

impl TimerControl {
    fn new(reps: u32) -> Self {
        Self {
            state: Mutex::new(TimerState {
                remaining: (reps != 0).then_some(reps),
                pause: PauseMode::Running,
                execute_requests: 0,
            }),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl TimerManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn auto_name(&self) -> String {
        let timers = self.timers.lock().unwrap();
        (1_u64..)
            .map(|n| n.to_string())
            .find(|n| !timers.keys().any(|k| k.eq_ignore_ascii_case(n)))
            .unwrap()
    }

    fn ordered_offset(&self, ordered: bool) -> std::time::Duration {
        if !ordered {
            return std::time::Duration::ZERO;
        }
        let mut counter = self.ordered_counter.lock().unwrap();
        *counter = counter.saturating_add(1);
        // A monotonic nanosecond tie-break preserves creation order without
        // materially changing a timer's documented interval.
        std::time::Duration::from_nanos(*counter)
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
        start_at: Option<String>,
        command: String,
        target: String,
        force_offline: bool,
        catch_up: bool,
        ordered: bool,
        milliseconds: bool,
        high_resolution: bool,
        dynamic: bool,
        source: String,
    ) {
        let name = if name.is_empty() {
            self.auto_name()
        } else {
            name
        };
        *self.last.lock().unwrap() = name.clone();
        let selected = app
            .try_state::<ScriptEngine>()
            .and_then(|engine| select_timer_connection(&app, &engine, &server_id, dynamic, false));
        let offline = force_offline || selected.is_none();
        let initial_server_id = selected.unwrap_or_else(|| server_id.clone());
        let cid = Arc::new(AtomicU32::new(
            app.try_state::<ScriptEngine>()
                .map(|engine| engine.cid_for(&initial_server_id))
                .unwrap_or(0),
        ));
        let control = Arc::new(TimerControl::new(reps));
        let initial_ms = start_at
            .as_deref()
            .and_then(wall_clock_delay_ms)
            .unwrap_or(interval_ms);
        let initial_delay = std::time::Duration::from_millis(initial_ms)
            .saturating_add(self.ordered_offset(ordered));
        let next_fire = Arc::new(Mutex::new(Some(
            tokio::time::Instant::now() + initial_delay,
        )));
        let gate = Arc::new(tokio::sync::Notify::new());

        let mut timers = self.timers.lock().unwrap();
        let old_names: Vec<String> = timers
            .keys()
            .filter(|key| key.eq_ignore_ascii_case(&name))
            .cloned()
            .collect();
        for old_name in old_names {
            if let Some(old) = timers.remove(&old_name) {
                old.handle.abort();
            }
        }
        let task_name = name.clone();
        let entry_command = command.clone();
        let task_control = control.clone();
        let task_next = next_fire.clone();
        let task_gate = gate.clone();
        let ordered_lock = self.ordered.clone();
        let task_server_id = server_id.clone();
        let task_cid = cid.clone();
        let task = tauri::async_runtime::spawn(async move {
            task_gate.notified().await;
            let mut deadline = tokio::time::Instant::now() + initial_delay;
            let mut frozen_remaining = None;
            loop {
                // Manual `-e` requests execute immediately, consume a
                // repetition, and leave the existing countdown unchanged.
                let execute_requests = {
                    let mut state = task_control.state.lock().unwrap();
                    let n = state.execute_requests;
                    state.execute_requests = 0;
                    n
                };
                let mut terminate = false;
                for _ in 0..execute_requests {
                    if !fire_timer(
                        &app,
                        &task_server_id,
                        &my_nick,
                        &network,
                        &server,
                        &target,
                        &command,
                        &source,
                        &task_name,
                        offline,
                        dynamic,
                        &task_cid,
                        ordered.then_some(&ordered_lock),
                    )
                    .await
                    {
                        terminate = true;
                        break;
                    }
                    if consume_repetition(&task_control) {
                        terminate = true;
                        break;
                    }
                }
                if terminate {
                    break;
                }

                let pause = task_control.state.lock().unwrap().pause;
                if pause == PauseMode::Countdown {
                    let remaining = *frozen_remaining.get_or_insert_with(|| {
                        deadline.saturating_duration_since(tokio::time::Instant::now())
                    });
                    task_control.notify.notified().await;
                    if task_control.state.lock().unwrap().pause != PauseMode::Countdown {
                        deadline = tokio::time::Instant::now() + remaining;
                        *task_next.lock().unwrap() = Some(deadline);
                        frozen_remaining = None;
                    }
                    continue;
                }
                frozen_remaining = None;

                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {
                        let pause = task_control.state.lock().unwrap().pause;
                        if pause == PauseMode::Running {
                            if !fire_timer(
                                &app, &task_server_id, &my_nick, &network, &server,
                                &target, &command, &source, &task_name, offline, dynamic,
                                &task_cid,
                                ordered.then_some(&ordered_lock),
                            ).await {
                                break;
                            }
                            if consume_repetition(&task_control) {
                                break;
                            }
                        }
                        if catch_up {
                            deadline += std::time::Duration::from_millis(interval_ms);
                        } else {
                            deadline = tokio::time::Instant::now()
                                + std::time::Duration::from_millis(interval_ms);
                        }
                        *task_next.lock().unwrap() = Some(deadline);
                        if interval_ms == 0 {
                            tokio::task::yield_now().await;
                        }
                    }
                    _ = task_control.notify.notified() => {}
                }
            }
            *task_next.lock().unwrap() = None;
            // Self-cleanup once finished.
            if let Some(m) = app.try_state::<TimerManager>() {
                let mut timers = m.timers.lock().unwrap();
                if timers
                    .get(&task_name)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.control, &task_control))
                {
                    timers.remove(&task_name);
                }
            }
        });
        timers.insert(
            name,
            TimerEntry {
                handle: task,
                command: entry_command,
                interval_ms,
                milliseconds,
                offline,
                high_resolution,
                dynamic,
                server_id,
                cid,
                control,
                next_fire,
            },
        );
        drop(timers);
        gate.notify_one();
    }

    /// Stops a timer by name, or all timers when `name` is "*".
    pub fn stop(&self, name: &str) {
        let mut timers = self.timers.lock().unwrap();
        let names: Vec<String> = timers
            .keys()
            .filter(|key| name_matches(name, key))
            .cloned()
            .collect();
        for timer_name in names {
            if let Some(entry) = timers.remove(&timer_name) {
                entry.handle.abort();
            }
        }
    }

    pub fn execute(&self, name: &str) {
        self.for_each_match(name, |control| {
            let mut state = control.state.lock().unwrap();
            state.execute_requests = state.execute_requests.saturating_add(1);
            drop(state);
            control.notify.notify_one();
        });
    }

    pub fn pause(&self, name: &str, countdown: bool) {
        self.for_each_match(name, |control| {
            control.state.lock().unwrap().pause = if countdown {
                PauseMode::Countdown
            } else {
                PauseMode::Execution
            };
            control.notify.notify_one();
        });
    }

    pub fn resume(&self, name: &str) {
        self.for_each_match(name, |control| {
            control.state.lock().unwrap().pause = PauseMode::Running;
            control.notify.notify_one();
        });
    }

    fn for_each_match(&self, name: &str, mut f: impl FnMut(&Arc<TimerControl>)) {
        let timers = self.timers.lock().unwrap();
        for (timer_name, entry) in timers.iter() {
            if name_matches(name, timer_name) {
                f(&entry.control);
            }
        }
    }

    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.timers.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    pub fn last(&self) -> String {
        self.last.lock().unwrap().clone()
    }

    /// Removes online timers as soon as their registered IRC session ends.
    /// Dynamic timers survive only when another registered connection is
    /// available for them to follow.
    pub fn session_dropped(&self, app: &AppHandle, server_id: &str) {
        let dynamic_target = app.try_state::<ScriptEngine>().and_then(|engine| {
            select_timer_connection(app, &engine, server_id, true, false)
                .map(|target| engine.cid_for(&target))
        });
        let mut timers = self.timers.lock().unwrap();
        let names: Vec<String> = timers
            .iter()
            .filter(|(_, entry)| {
                !entry.offline
                    && if entry.dynamic {
                        if let Some(cid) = dynamic_target {
                            entry.cid.store(cid, Ordering::Relaxed);
                            false
                        } else {
                            true
                        }
                    } else {
                        entry.server_id == server_id
                    }
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in names {
            if let Some(entry) = timers.remove(&name) {
                entry.handle.abort();
            }
        }
    }

    /// A snapshot of every active timer (sorted by name), for `$timer`.
    pub fn snapshot(&self) -> Vec<super::eval::TimerInfo> {
        let mut out: Vec<super::eval::TimerInfo> = self
            .timers
            .lock()
            .unwrap()
            .iter()
            .map(|(name, e)| {
                let state = e.control.state.lock().unwrap();
                let remaining_ms = e
                    .next_fire
                    .lock()
                    .unwrap()
                    .map(|at| {
                        at.saturating_duration_since(tokio::time::Instant::now())
                            .as_millis() as u64
                    })
                    .unwrap_or(0);
                super::eval::TimerInfo {
                    name: name.clone(),
                    command: super::eval::decode_delayed(&e.command),
                    reps: state.remaining.unwrap_or(0),
                    delay: if e.milliseconds {
                        e.interval_ms
                    } else {
                        e.interval_ms / 1000
                    },
                    time: (Local::now()
                        + chrono::Duration::milliseconds(remaining_ms.min(i64::MAX as u64) as i64))
                    .format("%H:%M:%S")
                    .to_string(),
                    timer_type: if e.offline { "offline" } else { "online" }.to_string(),
                    secs: remaining_ms / 1000,
                    mmt: e.high_resolution,
                    anysc: e.dynamic,
                    cid: e.cid.load(Ordering::Relaxed),
                    pause: match state.pause {
                        PauseMode::Running => 0,
                        PauseMode::Execution => 1,
                        PauseMode::Countdown => 2,
                    },
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn snapshot_matching(&self, name: &str) -> Vec<super::eval::TimerInfo> {
        self.snapshot()
            .into_iter()
            .filter(|timer| name_matches(name, &timer.name))
            .collect()
    }
}

fn consume_repetition(control: &TimerControl) -> bool {
    let mut state = control.state.lock().unwrap();
    match &mut state.remaining {
        Some(n) => {
            *n = n.saturating_sub(1);
            *n == 0
        }
        None => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn fire_timer(
    app: &AppHandle,
    original_server_id: &str,
    original_nick: &str,
    network: &str,
    server: &str,
    target: &str,
    command: &str,
    source: &str,
    timer_name: &str,
    offline: bool,
    dynamic: bool,
    associated_cid: &AtomicU32,
    ordered: Option<&Arc<tokio::sync::Mutex<()>>>,
) -> bool {
    let _ordered_guard = match ordered {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };
    let Some(engine) = app.try_state::<ScriptEngine>() else {
        return false;
    };
    let Some(server_id) =
        select_timer_connection(app, &engine, original_server_id, dynamic, offline)
    else {
        return false;
    };
    associated_cid.store(engine.cid_for(&server_id), Ordering::Relaxed);
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|s| s.get(&server_id))
        .unwrap_or_default();
    let nick = if state.nick.is_empty() {
        original_nick.to_string()
    } else {
        state.nick.clone()
    };
    let (network, server) = engine
        .connection_context(&server_id)
        .unwrap_or_else(|| (network.to_string(), server.to_string()));
    let ctx = RunCtx {
        my_nick: &nick,
        network: &network,
        server: &server,
        data_dir: script_data_dir(app),
        state,
    };
    let actions = engine.run_timer_command(&ctx, target, command, source, timer_name);
    apply_actions(app, &server_id, &nick, &network, &server, actions);
    true
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
        self.app
            .try_state::<TimerManager>()
            .map(|m| m.snapshot())
            .unwrap_or_default()
    }

    fn last(&self) -> String {
        self.app
            .try_state::<TimerManager>()
            .map(|m| m.last())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn timer_name_wildcards_are_case_insensitive() {
        assert!(name_matches("3?", "39"));
        assert!(name_matches("show*", "ShowGames"));
        assert!(!name_matches("3?", "300"));
        assert!(!name_matches("show?", "showgames"));
    }

    #[test]
    fn online_timer_requires_its_registered_session() {
        let order = vec!["one".to_string(), "two".to_string()];
        let registered = |id: &str| id == "two";
        assert_eq!(
            choose_timer_connection("one", false, false, Some("two"), &order, registered),
            None
        );
        assert_eq!(
            choose_timer_connection("one", false, true, Some("two"), &order, registered),
            Some("one".to_string())
        );
    }

    #[test]
    fn dynamic_timer_prefers_active_then_stable_cid_order() {
        let order = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let registered = |id: &str| id == "two" || id == "three";
        assert_eq!(
            choose_timer_connection("one", true, false, Some("three"), &order, registered),
            Some("three".to_string())
        );
        assert_eq!(
            choose_timer_connection("one", true, false, Some("one"), &order, registered),
            Some("two".to_string())
        );
    }

    #[test]
    fn wall_clock_timer_uses_today_or_the_next_day() {
        let now = NaiveDate::from_ymd_opt(2026, 7, 13)
            .unwrap()
            .and_hms_opt(14, 0, 0)
            .unwrap();
        assert_eq!(wall_clock_delay_ms_at("14:30", now), Some(30 * 60 * 1000));
        assert_eq!(wall_clock_delay_ms_at("14:00:01", now), Some(1000));
        assert_eq!(
            wall_clock_delay_ms_at("13:59", now),
            Some((23 * 60 + 59) * 60 * 1000)
        );
        assert_eq!(wall_clock_delay_ms_at("25:00", now), None);
    }

    #[test]
    fn manual_execution_consumes_finite_repetitions() {
        let finite = TimerControl::new(2);
        assert!(!consume_repetition(&finite));
        assert!(consume_repetition(&finite));

        let infinite = TimerControl::new(0);
        assert!(!consume_repetition(&infinite));
        assert!(!consume_repetition(&infinite));
    }

    #[test]
    fn ordered_timers_get_creation_order_tie_breaks() {
        let manager = TimerManager::new();
        assert_eq!(manager.ordered_offset(false), std::time::Duration::ZERO);
        let first = manager.ordered_offset(true);
        let second = manager.ordered_offset(true);
        assert!(first < second);
    }
}
