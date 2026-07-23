//! mIRC-compatible `/play` queue.
//!
//! Script files remain inside jIRC's `scriptdata/` sandbox. The manager owns a
//! single application-wide queue, like mIRC's Play Central, and a lightweight
//! worker advances it one line at a time.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use tokio::time::{Duration, Instant};

use super::eval::{Action, PlayInfo};
use super::{apply_actions, RunCtx, ScriptEngine};

const DEFAULT_DELAY_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlayMode {
    Message,
    Notice,
    Commands,
    Alias(String),
}

impl PlayMode {
    fn name(&self) -> String {
        match self {
            Self::Message => "message".into(),
            Self::Notice => "notice".into(),
            Self::Commands => "command".into(),
            Self::Alias(_) => "alias".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PlayOptions {
    alias: bool,
    commands: bool,
    echo: bool,
    offline: bool,
    priority: bool,
    clipboard: bool,
    notice: bool,
    literal_first: bool,
    queue_limit: Option<usize>,
    target_limit: Option<usize>,
    random: bool,
    line: Option<usize>,
    from: Option<usize>,
    topic: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedPlay {
    Stop,
    Queue(PlaySpec),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlaySpec {
    target: String,
    filename: String,
    delay_ms: u64,
    mode: PlayMode,
    echo: bool,
    offline: bool,
    priority: bool,
    clipboard: bool,
    queue_limit: Option<usize>,
    target_limit: Option<usize>,
    random: bool,
    line: Option<usize>,
    from: Option<usize>,
    literal_first: bool,
    topic: String,
}

/// Context captured when the script creates the queue item. Deferred playback
/// must retain both its connection and script-file origin.
#[derive(Debug, Clone)]
pub struct PlayInvocation {
    pub args: String,
    pub current_target: String,
    pub remote: bool,
    pub source: String,
}

#[derive(Clone)]
struct PlayRun {
    server_id: String,
    my_nick: String,
    network: String,
    server: String,
    target: String,
    source: String,
    mode: PlayMode,
    echo: bool,
    offline: bool,
}

struct PlayEntry {
    id: u64,
    run: PlayRun,
    filename: String,
    topic: String,
    lines: Vec<String>,
    pos: usize,
    delay_ms: u64,
    status: &'static str,
    /// Delay remaining when a priority item preempts this one.
    resume_delay_ms: Option<u64>,
    waiting_until: Option<Instant>,
    /// An empty final line is itself a delay; remove the item after that wait.
    finish_after_delay: bool,
}

#[derive(Default)]
struct PlayState {
    queue: VecDeque<PlayEntry>,
    worker_running: bool,
}

#[derive(Default)]
pub struct PlayManager {
    state: Mutex<PlayState>,
    wake: Arc<tokio::sync::Notify>,
}

impl PlayManager {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn command(
        &self,
        app: AppHandle,
        server_id: String,
        my_nick: String,
        network: String,
        server: String,
        invocation: PlayInvocation,
    ) -> Result<(), String> {
        let parsed = parse_play(&invocation.args, &invocation.current_target)?;
        let ParsedPlay::Queue(mut spec) = parsed else {
            self.stop();
            return Ok(());
        };
        if spec.clipboard {
            return Err(
                "clipboard playback (-b) is unavailable: jIRC has no safe clipboard backend".into(),
            );
        }

        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|store| store.get(&server_id))
            .unwrap_or_default();
        let connected = state.server_id == server_id && state.connect_time != 0;
        if !spec.offline && !connected {
            return Err(
                "/play requires a registered IRC connection (or -s for offline commands)".into(),
            );
        }
        if spec.target.is_empty() || spec.target == "(status)" {
            if spec.offline {
                spec.target = "(status)".into();
            } else {
                return Err("/play needs a channel or query target".into());
            }
        }

        let data_dir = super::script_data_dir(&app);
        let path = super::eval::sandbox_path(&data_dir, &spec.filename);
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", spec.filename))?;
        let text = String::from_utf8_lossy(&bytes);
        let all_lines: Vec<String> = text.lines().map(str::to_string).collect();
        let lines = select_lines(&all_lines, &spec, None)?;

        let entry = PlayEntry {
            id: NEXT_PLAY_ID.fetch_add(1, Ordering::Relaxed),
            run: PlayRun {
                server_id,
                my_nick,
                network,
                server,
                target: spec.target.clone(),
                source: invocation.source,
                mode: spec.mode,
                echo: spec.echo,
                offline: spec.offline,
            },
            filename: path.to_string_lossy().into_owned(),
            topic: spec.topic,
            lines,
            pos: 0,
            delay_ms: spec.delay_ms,
            status: "queued",
            resume_delay_ms: None,
            waiting_until: None,
            finish_after_delay: false,
        };

        let mut state = self.state.lock().unwrap();
        // mIRC applies these limits only to requests created by remote events.
        if invocation.remote {
            if spec
                .queue_limit
                .is_some_and(|limit| state.queue.len() >= limit)
            {
                return Ok(());
            }
            if spec.target_limit.is_some_and(|limit| {
                state
                    .queue
                    .iter()
                    .filter(|item| item.run.target.eq_ignore_ascii_case(&entry.run.target))
                    .count()
                    >= limit
            }) {
                return Ok(());
            }
        }

        let was_idle = state.queue.is_empty();
        if spec.priority && !was_idle {
            if let Some(current) = state.queue.front_mut() {
                current.status = "paused";
                if let Some(deadline) = current.waiting_until.take() {
                    current.resume_delay_ms = Some(remaining_millis(deadline));
                }
            }
            state.queue.push_front(entry);
        } else {
            state.queue.push_back(entry);
        }
        let start_worker = !state.worker_running;
        if start_worker {
            state.worker_running = true;
        }
        drop(state);

        if start_worker {
            let wake = self.wake.clone();
            tauri::async_runtime::spawn(run_queue(app, wake));
        } else if spec.priority || was_idle {
            self.wake.notify_one();
        }
        Ok(())
    }

    pub fn stop(&self) {
        self.state.lock().unwrap().queue.clear();
        self.wake.notify_one();
    }

    pub fn snapshot(&self) -> Vec<PlayInfo> {
        self.state
            .lock()
            .unwrap()
            .queue
            .iter()
            .map(|entry| PlayInfo {
                target: entry.run.target.clone(),
                play_type: entry.run.mode.name(),
                filename: entry.filename.clone(),
                topic: entry.topic.clone(),
                pos: entry.pos,
                lines: entry.lines.len(),
                delay: entry.delay_ms,
                status: entry.status.to_string(),
            })
            .collect()
    }
}

enum QueueStep {
    Execute {
        id: u64,
        run: PlayRun,
        line: String,
        final_line: bool,
    },
    Wait {
        id: u64,
        deadline: Instant,
    },
    Continue,
}

async fn run_queue(app: AppHandle, wake: Arc<tokio::sync::Notify>) {
    loop {
        let step = {
            let Some(manager) = app.try_state::<PlayManager>() else {
                return;
            };
            let mut state = manager.state.lock().unwrap();
            let Some(current) = state.queue.front_mut() else {
                state.worker_running = false;
                return;
            };
            current.status = "playing";
            if let Some(delay) = current.resume_delay_ms.take() {
                let deadline = Instant::now() + Duration::from_millis(delay);
                current.waiting_until = Some(deadline);
                QueueStep::Wait {
                    id: current.id,
                    deadline,
                }
            } else if current.finish_after_delay || current.pos >= current.lines.len() {
                state.queue.pop_front();
                QueueStep::Continue
            } else {
                let line = current.lines[current.pos].clone();
                current.pos += 1;
                QueueStep::Execute {
                    id: current.id,
                    run: current.run.clone(),
                    line,
                    final_line: current.pos == current.lines.len(),
                }
            }
        };

        match step {
            QueueStep::Continue => continue,
            QueueStep::Wait { id, deadline } => {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {
                        if let Some(manager) = app.try_state::<PlayManager>() {
                            let mut state = manager.state.lock().unwrap();
                            if let Some(index) = state.queue.iter().position(|item| item.id == id) {
                                let finished = state.queue[index].finish_after_delay;
                                state.queue[index].waiting_until = None;
                                if finished {
                                    state.queue.remove(index);
                                }
                            }
                        }
                    }
                    _ = wake.notified() => {}
                }
            }
            QueueStep::Execute {
                id,
                run,
                line,
                final_line,
            } => {
                let keep = line.is_empty() || execute_line(&app, &run, &line);
                let Some(manager) = app.try_state::<PlayManager>() else {
                    return;
                };
                let mut state = manager.state.lock().unwrap();
                let Some(index) = state.queue.iter().position(|item| item.id == id) else {
                    continue;
                };
                if !keep || (final_line && !line.is_empty()) {
                    state.queue.remove(index);
                    continue;
                }
                let item = &mut state.queue[index];
                item.resume_delay_ms = Some(item.delay_ms);
                item.finish_after_delay = final_line;
            }
        }
    }
}

fn execute_line(app: &AppHandle, run: &PlayRun, line: &str) -> bool {
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|store| store.get(&run.server_id))
        .unwrap_or_default();
    if !run.offline && (state.server_id != run.server_id || state.connect_time == 0) {
        return false;
    }
    let Some(engine) = app.try_state::<ScriptEngine>() else {
        return false;
    };
    let my_nick = if state.nick.is_empty() {
        run.my_nick.clone()
    } else {
        state.nick.clone()
    };
    let (network, server) = engine
        .connection_context(&run.server_id)
        .unwrap_or_else(|| (run.network.clone(), run.server.clone()));
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: &server,
        data_dir: super::script_data_dir(app),
        state,
    };
    let actions = match &run.mode {
        PlayMode::Message | PlayMode::Notice => vec![Action::PlayLine {
            target: run.target.clone(),
            text: line.to_string(),
            notice: run.mode == PlayMode::Notice,
            echo: run.echo,
        }],
        PlayMode::Commands => {
            engine.run_play_command(&ctx, &run.target, line, &run.source, &run.target)
        }
        PlayMode::Alias(alias) => {
            engine.run_play_alias(&ctx, &run.target, alias, line, &run.source, &run.target)
        }
    };
    apply_actions(app, &run.server_id, &my_nick, &network, &server, actions);
    true
}

fn remaining_millis(deadline: Instant) -> u64 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = remaining.as_millis().min(u64::MAX as u128) as u64;
    millis + u64::from(remaining.subsec_nanos() % 1_000_000 != 0)
}

fn parse_play(raw: &str, current_target: &str) -> Result<ParsedPlay, String> {
    let mut tokens = command_tokens(raw)?;
    if tokens.is_empty() {
        return Err("the Play Central dialog is unavailable; specify a scriptdata filename".into());
    }
    if tokens[0].eq_ignore_ascii_case("stop") {
        return Ok(ParsedPlay::Stop);
    }

    let mut options = PlayOptions::default();
    while tokens
        .first()
        .is_some_and(|token| token.starts_with('-') && token.len() > 1)
    {
        let token = tokens.remove(0);
        parse_switch_token(&token[1..], &mut options)?;
    }
    let alias = if options.alias {
        if tokens.is_empty() {
            return Err("/play -a requires an alias name".into());
        }
        Some(tokens.remove(0))
    } else {
        None
    };
    if tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("stop"))
    {
        return Ok(ParsedPlay::Stop);
    }
    if tokens.is_empty() {
        return Err("/play requires a filename".into());
    }

    let mut delay_ms = DEFAULT_DELAY_MS;
    if tokens.len() >= 2 {
        if let Some(delay) = tokens.last().and_then(|token| token.parse::<u64>().ok()) {
            delay_ms = delay;
            tokens.pop();
        }
    }
    let (target, filename) = match tokens.as_slice() {
        [filename] => (current_target.to_string(), filename.clone()),
        [target, filename] => (target.clone(), filename.clone()),
        _ => return Err("quote filenames that contain spaces in /play".into()),
    };
    if filename.is_empty() {
        return Err("/play requires a filename".into());
    }
    let mode = if let Some(alias) = alias {
        PlayMode::Alias(alias)
    } else if options.commands {
        PlayMode::Commands
    } else if options.notice {
        PlayMode::Notice
    } else {
        PlayMode::Message
    };
    Ok(ParsedPlay::Queue(PlaySpec {
        target,
        filename,
        delay_ms,
        mode,
        echo: options.echo,
        offline: options.offline,
        priority: options.priority,
        clipboard: options.clipboard,
        queue_limit: options.queue_limit,
        target_limit: options.target_limit,
        random: options.random,
        line: options.line,
        from: options.from,
        literal_first: options.literal_first,
        topic: options.topic,
    }))
}

fn parse_switch_token(token: &str, options: &mut PlayOptions) -> Result<(), String> {
    let chars: Vec<char> = token.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i].to_ascii_lowercase();
        i += 1;
        match ch {
            'a' => options.alias = true,
            'e' => options.echo = true,
            's' => options.offline = true,
            'c' => options.commands = true,
            'p' => options.priority = true,
            'b' => options.clipboard = true,
            'n' => options.notice = true,
            'x' => options.literal_first = true,
            'r' => options.random = true,
            'q' | 'm' | 'f' | 'l' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if start == i {
                    return Err(format!("/play -{ch} requires a number"));
                }
                let value: usize = chars[start..i]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .map_err(|_| format!("invalid /play -{ch} value"))?;
                match ch {
                    'q' => options.queue_limit = Some(value),
                    'm' => options.target_limit = Some(value),
                    'f' => options.from = Some(value.max(1)),
                    'l' => options.line = Some(value.max(1)),
                    _ => unreachable!(),
                }
            }
            't' => {
                let topic: String = chars[i..].iter().collect();
                if topic.is_empty() {
                    return Err("/play -t requires an attached topic".into());
                }
                options.topic = topic;
                return Ok(());
            }
            _ => return Err(format!("unsupported /play switch -{ch}")),
        }
    }
    Ok(())
}

fn command_tokens(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for ch in input.trim().chars() {
        match ch {
            '"' => quoted = !quoted,
            ch if ch.is_whitespace() && !quoted => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            ch => token.push(ch),
        }
    }
    if quoted {
        return Err("unterminated quote in /play".into());
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

fn select_lines(
    all_lines: &[String],
    spec: &PlaySpec,
    random_index: Option<usize>,
) -> Result<Vec<String>, String> {
    let mut lines = if !spec.topic.is_empty() {
        let wanted = format!("[{}]", spec.topic);
        let Some(start) = all_lines
            .iter()
            .position(|line| line.trim().eq_ignore_ascii_case(&wanted))
        else {
            return Err(format!("/play topic [{}] was not found", spec.topic));
        };
        all_lines[start + 1..]
            .iter()
            .take_while(|line| !is_topic_header(line))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        all_lines.to_vec()
    };

    let numbered_selection = spec.random || spec.line.is_some() || spec.from.is_some();
    if spec.topic.is_empty()
        && numbered_selection
        && !spec.literal_first
        && lines
            .first()
            .is_some_and(|line| line.trim().parse::<usize>().is_ok())
    {
        lines.remove(0);
    }

    if let Some(line) = spec.line {
        lines = lines
            .get(line.saturating_sub(1))
            .cloned()
            .into_iter()
            .collect();
    } else if let Some(from) = spec.from {
        lines = lines.into_iter().skip(from.saturating_sub(1)).collect();
    } else if spec.random {
        if !lines.is_empty() {
            let index =
                random_index.unwrap_or_else(|| random_line_index(lines.len())) % lines.len();
            lines = vec![lines[index].clone()];
        }
    }

    if lines.is_empty() {
        return Err("/play selected no lines".into());
    }
    Ok(lines)
}

fn is_topic_header(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 2 && line.starts_with('[') && line.ends_with(']')
}

fn random_line_index(len: usize) -> usize {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let mut value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
        ^ COUNTER.fetch_add(1, Ordering::Relaxed);
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    value as usize % len.max(1)
}

/// Bridge used by `$play(...)` during script evaluation.
pub struct EnginePlay {
    app: AppHandle,
}

impl EnginePlay {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl super::eval::ScriptPlay for EnginePlay {
    fn snapshot(&self) -> Vec<PlayInfo> {
        self.app
            .try_state::<PlayManager>()
            .map(|manager| manager.snapshot())
            .unwrap_or_default()
    }
}

static NEXT_PLAY_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(options: &str) -> PlaySpec {
        match parse_play(options, "#current").unwrap() {
            ParsedPlay::Queue(spec) => spec,
            ParsedPlay::Stop => panic!("unexpected stop"),
        }
    }

    #[test]
    fn play_parser_handles_targets_delays_aliases_and_compound_switches() {
        let basic = spec("poem.txt");
        assert_eq!(basic.target, "#current");
        assert_eq!(basic.filename, "poem.txt");
        assert_eq!(basic.delay_ms, 1000);
        assert_eq!(basic.mode, PlayMode::Message);

        let advanced = spec("-aepq5m1f9 helper #room \"long file.txt\" 250");
        assert_eq!(advanced.target, "#room");
        assert_eq!(advanced.filename, "long file.txt");
        assert_eq!(advanced.delay_ms, 250);
        assert_eq!(advanced.mode, PlayMode::Alias("helper".into()));
        assert!(advanced.echo && advanced.priority);
        assert_eq!(advanced.queue_limit, Some(5));
        assert_eq!(advanced.target_limit, Some(1));
        assert_eq!(advanced.from, Some(9));
        assert_eq!(parse_play("stop", "#c"), Ok(ParsedPlay::Stop));
    }

    #[test]
    fn play_line_selection_matches_numeric_header_topic_and_x_rules() {
        let lines = vec!["3", "one", "two", "three"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(
            select_lines(&lines, &spec("-l2 file.txt"), None).unwrap(),
            ["two"]
        );
        assert_eq!(
            select_lines(&lines, &spec("-xl2 file.txt"), None).unwrap(),
            ["one"]
        );
        assert_eq!(
            select_lines(&lines, &spec("-f2 file.txt"), None).unwrap(),
            ["two", "three"]
        );
        assert_eq!(
            select_lines(&lines, &spec("-r file.txt"), Some(2)).unwrap(),
            ["three"]
        );

        let topics = vec!["[one]", "a", "", "b", "[two]", "c"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(
            select_lines(&topics, &spec("-tone file.txt"), None).unwrap(),
            ["a", "", "b"]
        );
    }

    #[test]
    fn play_priority_preserves_paused_item_and_remaining_delay() {
        let manager = PlayManager::new();
        let run = PlayRun {
            server_id: "s".into(),
            my_nick: "me".into(),
            network: String::new(),
            server: String::new(),
            target: "bob".into(),
            source: String::new(),
            mode: PlayMode::Message,
            echo: false,
            offline: false,
        };
        let make = |id, target: &str| PlayEntry {
            id,
            run: PlayRun {
                target: target.into(),
                ..run.clone()
            },
            filename: "file.txt".into(),
            topic: String::new(),
            lines: vec!["line".into()],
            pos: 0,
            delay_ms: 1000,
            status: "queued",
            resume_delay_ms: None,
            waiting_until: None,
            finish_after_delay: false,
        };
        let mut state = manager.state.lock().unwrap();
        let mut current = make(1, "bob");
        current.status = "playing";
        current.waiting_until = Some(Instant::now() + Duration::from_millis(500));
        state.queue.push_back(current);
        let mut priority = make(2, "alice");
        if let Some(current) = state.queue.front_mut() {
            current.status = "paused";
            current.resume_delay_ms = current.waiting_until.take().map(remaining_millis);
        }
        priority.status = "queued";
        state.queue.push_front(priority);
        assert_eq!(state.queue[0].run.target, "alice");
        assert_eq!(state.queue[1].status, "paused");
        assert!(state.queue[1].resume_delay_ms.is_some_and(|ms| ms <= 500));
    }
}
