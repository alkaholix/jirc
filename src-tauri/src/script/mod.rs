//! The mIRC scripting (mSL) engine: compiles scripts and runs aliases and
//! event handlers, producing [`Action`]s (lines to send / text to echo).
//!
//! This is a substantial, working subset of mSL — aliases, events, control
//! flow, variables, hash tables, and a library of identifiers and commands —
//! not a 100% mIRC-compatible implementation.

pub mod ast;
pub mod eval;
pub mod ident;
pub mod parser;
pub mod socket;
pub mod timer;

use std::collections::HashMap;
use std::sync::Mutex;

use ast::{PopupItem, Script};
use eval::{wildcard_match, Action, EventVars, Runtime};

/// Connection context supplied by the caller for each run.
pub struct RunCtx<'a> {
    pub my_nick: &'a str,
    pub network: &'a str,
    pub server: &'a str,
    /// Sandbox directory for script file I/O (`$read`/`/write`).
    pub data_dir: std::path::PathBuf,
    /// Live channel/member snapshot for state-aware identifiers.
    pub state: std::sync::Arc<crate::irc::state::StateSnapshot>,
}

struct Inner {
    script: Script,
    vars: HashMap<String, String>,
    hashes: HashMap<String, HashMap<String, String>>,
}

impl Inner {
    fn empty() -> Self {
        Inner {
            script: Script::default(),
            vars: HashMap::new(),
            hashes: HashMap::new(),
        }
    }
}

/// The script engine, stored as Tauri managed state.
pub struct ScriptEngine {
    inner: Mutex<Inner>,
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptEngine {
    pub fn new() -> Self {
        ScriptEngine {
            inner: Mutex::new(Inner::empty()),
        }
    }

    /// Compiles the combined source of all loaded script files.
    pub fn load(&self, source: &str) {
        let mut g = self.inner.lock().unwrap();
        g.script = parser::parse(source);
    }

    pub fn has_alias(&self, name: &str) -> bool {
        // Local (`-l`) aliases aren't user-callable as `/commands`.
        self.inner
            .lock()
            .unwrap()
            .script
            .find_alias(name)
            .is_some_and(|a| !a.local)
    }

    /// Returns the user-defined popup items for a context (nicklist, channel, …).
    pub fn popups(&self, context: &str) -> Vec<PopupItem> {
        self.inner.lock().unwrap().script.popup_items(context)
    }

    /// Like [`popups`], but evaluates each item's dynamic label ($iif/$sock/…) in a
    /// run context (the right-clicked nick + channel), dropping items whose label
    /// renders empty — mIRC's display behaviour. The `command` is left unexpanded
    /// (it's expanded when the item runs via [`run_command`]).
    pub fn popups_evaluated(&self, ctx: &RunCtx, context: &str, nick: &str, chan: &str) -> Vec<PopupItem> {
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let raw = script.popup_items(context);
        let event = EventVars {
            nick: nick.to_string(),
            chan: chan.to_string(),
            target: if chan.is_empty() { nick.to_string() } else { chan.to_string() },
            params: if nick.is_empty() { Vec::new() } else { vec![nick.to_string()] },
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            hashes: &mut g.hashes,
            event,
            actions: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
        };
        eval_popup_labels(&mut rt, &raw)
    }

    /// Runs a user-invoked alias. Returns the resulting actions.
    pub fn run_alias(&self, ctx: &RunCtx, target: &str, name: &str, args: &str) -> Vec<Action> {
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let Some(alias) = script.find_alias(name).filter(|a| !a.local) else {
            return Vec::new();
        };
        let chan = if is_channel(target) { target.to_string() } else { String::new() };
        let event = EventVars {
            nick: ctx.my_nick.to_string(),
            chan,
            target: target.to_string(),
            text: args.to_string(),
            params: args.split_whitespace().map(String::from).collect(),
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            hashes: &mut g.hashes,
            event,
            actions: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
        };
        rt.run(&alias.body);
        rt.actions
    }

    /// Runs a single command line (used by timers and popups when they fire).
    /// `params` populate `$1..` (and `$nick` from `$1`, e.g. a popup's selected
    /// nick); pass an empty slice for none.
    pub fn run_command(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        params: &[String],
    ) -> Vec<Action> {
        let body = parser::parse_body(command);
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let chan = if is_channel(target) { target.to_string() } else { String::new() };
        let event = EventVars {
            nick: params
                .first()
                .cloned()
                .unwrap_or_else(|| ctx.my_nick.to_string()),
            chan,
            target: target.to_string(),
            params: params.to_vec(),
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            hashes: &mut g.hashes,
            event,
            actions: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
        };
        rt.run(&body);
        rt.actions
    }

    /// Dispatches an event to all matching handlers. Returns the actions.
    pub fn dispatch_event(&self, ctx: &RunCtx, kind: &str, event: EventVars) -> Vec<Action> {
        self.dispatch_event_halt(ctx, kind, event).0
    }

    /// Like [`dispatch_event`], but also reports whether any handler called
    /// `/halt` (used by `on INPUT` to suppress the typed line).
    pub fn dispatch_event_halt(
        &self,
        ctx: &RunCtx,
        kind: &str,
        event: EventVars,
    ) -> (Vec<Action>, bool) {
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let g = &mut *g;
        let vars = &mut g.vars;
        let hashes = &mut g.hashes;
        let mut actions = Vec::new();
        let mut halted = false;
        for ev in script.events_of(kind) {
            if !matches(&event, &ev.pattern, &ev.target) {
                continue;
            }
            let mut rt = Runtime {
                script: &script,
                my_nick: ctx.my_nick,
                network: ctx.network,
                server: ctx.server,
                vars: &mut *vars,
                hashes: &mut *hashes,
                event: event.clone(),
                actions: Vec::new(),
                halted: false,
                steps: 0,
                depth: 0,
                ret: None,
                goto: None,
                data_dir: ctx.data_dir.clone(),
                state: ctx.state.clone(),
            };
            rt.run(&ev.body);
            halted |= rt.halted;
            actions.extend(rt.actions);
        }
        (actions, halted)
    }
}

/// Recursively evaluates popup item labels with `rt`, dropping items whose label
/// renders empty (mIRC hides those). Separators pass through unchanged.
fn eval_popup_labels(rt: &mut Runtime, items: &[PopupItem]) -> Vec<PopupItem> {
    let mut out = Vec::new();
    for item in items {
        if item.separator {
            out.push(item.clone());
            continue;
        }
        let label = rt.expand(&item.label).trim().to_string();
        if label.is_empty() {
            continue;
        }
        out.push(PopupItem {
            label,
            command: item.command.clone(),
            separator: false,
            children: eval_popup_labels(rt, &item.children),
        });
    }
    out
}

fn is_channel(name: &str) -> bool {
    // Includes IRCX's '%' channel prefix so `$chan` resolves on IRCX servers.
    name.starts_with(['#', '&', '!', '+', '%'])
}

/// Splits a phrase into whitespace-separated `$1..` parameters.
fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

/// Maps a prefix/ban mode letter + direction to its specific event name.
fn mode_event_name(letter: char, adding: bool) -> Option<&'static str> {
    Some(match (letter, adding) {
        ('o', true) => "OP",
        ('o', false) => "DEOP",
        ('v', true) => "VOICE",
        ('v', false) => "DEVOICE",
        ('h', true) => "HELP",
        ('h', false) => "DEHELP",
        ('q', true) => "OWNER",
        ('q', false) => "DEOWNER",
        ('a', true) => "ADMIN",
        ('a', false) => "DEADMIN",
        ('b', true) => "BAN",
        ('b', false) => "UNBAN",
        _ => return None,
    })
}

/// Parses a rendered mode string ("+o bob -v alice +b m!*@*") into the specific
/// (event-name, affected-target) pairs to fire alongside the generic `on MODE`.
fn split_mode_events(modes: &str) -> Vec<(&'static str, String)> {
    let toks: Vec<&str> = modes.split_whitespace().collect();
    let is_spec = |t: &str| t.len() == 2 && (t.starts_with('+') || t.starts_with('-'));
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        if is_spec(t) {
            let adding = t.starts_with('+');
            let letter = t.chars().nth(1).unwrap();
            let arg = toks.get(i + 1).filter(|n| !is_spec(n)).copied();
            if let (Some(kind), Some(a)) = (mode_event_name(letter, adding), arg) {
                out.push((kind, a.to_string()));
            }
            i += if arg.is_some() { 2 } else { 1 };
        } else {
            i += 1;
        }
    }
    out
}

/// Tests whether an event matches a handler's pattern and target spec.
fn matches(ev: &EventVars, pattern: &str, target_spec: &str) -> bool {
    let pat_ok = pattern.is_empty() || pattern == "*" || wildcard_match(pattern, &ev.text);
    if !pat_ok {
        return false;
    }
    match target_spec {
        "" | "*" => true,
        "#" => is_channel(&ev.chan),
        "?" => ev.chan.is_empty(),
        spec if is_channel(spec) => ev.chan.eq_ignore_ascii_case(spec),
        // A named target (e.g. a socket name in `on *:SOCKREAD:bot:`) is matched
        // as a wildcard against the event's name/channel.
        spec => wildcard_match(spec, &ev.chan),
    }
}

// ---- Tauri commands ----

use tauri::{AppHandle, Emitter, Manager, State};

use crate::irc::event::{MessageKind, UiEvent, IRC_EVENT};
use crate::irc::ConnectionManager;

// ---- Multi-file script storage (<config>/scripts/*.mrc) ----

fn scripts_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("scripts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// The sandbox directory for script file I/O (`$read`/`/write`). Created on
/// demand; falls back to the system temp dir if the config dir is unavailable.
pub fn script_data_dir(app: &AppHandle) -> std::path::PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .map(|c| c.join("scriptdata"))
        .unwrap_or_else(|_| std::env::temp_dir().join("jirc-scriptdata"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Sanitizes a script name into a safe file stem.
fn script_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() { "script".to_string() } else { trimmed }
}

/// Starter example scripts seeded on first run and via "Add examples".
const EXAMPLE_SCRIPTS: &[(&str, &str)] = &[
    (
        "aliases",
        "; Aliases — type these as /commands in a channel.\n\
         alias hello { /msg $chan Hello from a script, $me $+ ! }\n\
         alias slap { /me slaps $1 around a bit with a large trout }\n\
         alias shrug { /msg $chan \u{00af}\\_(\u{30c4})_/\u{00af} }\n",
    ),
    (
        "events",
        "; Events — automatic responses.\n\
         on *:TEXT:!ping*:#:{ /msg $chan pong $nick }\n\
         on *:JOIN:#:{ /msg $chan welcome $nick }\n",
    ),
    (
        "popups",
        "; Right-click menus. The nicklist menu uses $1 = the selected nick.\n\
         ; Leading dots make submenus; a line with just - is a separator.\n\
         menu nicklist {\n\
         \x20 Whois:/whois $1\n\
         \x20 -\n\
         \x20 Control\n\
         \x20 .Op:/mode $chan +o $1\n\
         \x20 .Deop:/mode $chan -o $1\n\
         \x20 .Voice:/mode $chan +v $1\n\
         \x20 .Kick:/kick $chan $1\n\
         \x20 -\n\
         \x20 Slap:/me slaps $1 around a bit\n\
         }\n",
    ),
    (
        "dialog",
        "; A custom dialog. Type /qsay in a channel to open it.\n\
         dialog quicksay {\n\
         \x20 title \"Quick say\"\n\
         \x20 text   info  \"Type a message:\"\n\
         \x20 edit   msg\n\
         \x20 combo  where \"#test\"\n\
         \x20 check  act   \"Send as an action\"\n\
         \x20 button send  \"Send\" :default\n\
         \x20 button cancel \"Cancel\" :cancel\n\
         }\n\
         alias qsay { /dialog quicksay }\n\
         on *:DIALOG:quicksay:{\n\
         \x20 if ($1 == send) {\n\
         \x20   if ($did(quicksay, act) == 1) { /describe $did(quicksay, where) $did(quicksay, msg) }\n\
         \x20   else { /msg $did(quicksay, where) $did(quicksay, msg) }\n\
         \x20   /dialog -c quicksay\n\
         \x20 }\n\
         }\n",
    ),
];

/// Writes the example scripts that don't already exist. Returns how many added.
fn write_examples(dir: &std::path::Path) -> usize {
    let mut added = 0;
    for (name, body) in EXAMPLE_SCRIPTS {
        let path = dir.join(format!("{name}.mrc"));
        if !path.exists() && std::fs::write(&path, body).is_ok() {
            added += 1;
        }
    }
    added
}

/// Reads and compiles every `.mrc` file into the engine.
fn recompile(app: &AppHandle, engine: &ScriptEngine) {
    let Ok(dir) = scripts_dir(app) else { return };
    let mut combined = String::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mrc"))
            .collect();
        files.sort();
        for path in files {
            if let Ok(src) = std::fs::read_to_string(&path) {
                combined.push_str(&src);
                combined.push('\n');
            }
        }
    }
    engine.load(&combined);
}

/// Loads persisted scripts at startup, migrating a legacy single script.mrc and
/// seeding example scripts on first run.
pub fn load_persisted(app: &AppHandle, engine: &ScriptEngine) {
    // First run = the scripts dir does not exist yet.
    let first_run = app
        .path()
        .app_config_dir()
        .map(|c| !c.join("scripts").exists())
        .unwrap_or(false);

    if let Ok(config) = app.path().app_config_dir() {
        let legacy = config.join("script.mrc");
        if legacy.exists() {
            if let Ok(dir) = scripts_dir(app) {
                let dest = dir.join("main.mrc");
                if !dest.exists() {
                    let _ = std::fs::rename(&legacy, &dest);
                }
            }
        }
    }

    if first_run {
        if let Ok(dir) = scripts_dir(app) {
            // Only seed if nothing was migrated in.
            let empty = std::fs::read_dir(&dir)
                .map(|mut it| it.next().is_none())
                .unwrap_or(true);
            if empty {
                write_examples(&dir);
            }
        }
    }

    recompile(app, engine);
}

/// Writes the bundled example scripts (skipping any that already exist) and
/// recompiles. Returns the number of scripts added.
#[tauri::command]
pub fn script_add_examples(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
) -> Result<usize, String> {
    let dir = scripts_dir(&app)?;
    let added = write_examples(&dir);
    recompile(&app, &engine);
    Ok(added)
}

/// Lists script names (file stems), sorted.
#[tauri::command]
pub fn scripts_list(app: AppHandle) -> Result<Vec<String>, String> {
    let dir = scripts_dir(&app)?;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "mrc") {
                p.file_stem().map(|s| s.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    Ok(names)
}

/// Reads one script file's source.
#[tauri::command]
pub fn script_read(app: AppHandle, name: String) -> Result<String, String> {
    let path = scripts_dir(&app)?.join(format!("{}.mrc", script_stem(&name)));
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// Writes one script file and recompiles all scripts.
#[tauri::command]
pub fn script_write(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    name: String,
    source: String,
) -> Result<(), String> {
    let path = scripts_dir(&app)?.join(format!("{}.mrc", script_stem(&name)));
    std::fs::write(&path, source).map_err(|e| e.to_string())?;
    recompile(&app, &engine);
    Ok(())
}

/// Deletes one script file and recompiles all scripts.
#[tauri::command]
pub fn script_delete(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    name: String,
) -> Result<(), String> {
    let path = scripts_dir(&app)?.join(format!("{}.mrc", script_stem(&name)));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    recompile(&app, &engine);
    Ok(())
}

/// If `line` is a `PRIVMSG`/`NOTICE`, builds a local echo so the user sees their
/// own scripted message in the right buffer (`from` = self). Returns `None` for
/// any other raw line.
fn self_echo(server_id: &str, my_nick: &str, line: &str) -> Option<UiEvent> {
    let (kind, rest) = if let Some(r) = line.strip_prefix("PRIVMSG ") {
        (MessageKind::Privmsg, r)
    } else if let Some(r) = line.strip_prefix("NOTICE ") {
        (MessageKind::Notice, r)
    } else {
        return None;
    };
    let (target, text) = rest.split_once(" :")?;
    Some(UiEvent::Message {
        server_id: server_id.to_string(),
        kind,
        from: Some(my_nick.to_string()),
        target: target.trim().to_string(),
        text: text.to_string(),
        time: None,
    })
}

/// Applies script actions: sends lines via the manager, emits echoes, and
/// schedules timers. `my_nick`/`network`/`server` give timer commands context.
pub fn apply_actions(
    app: &AppHandle,
    server_id: &str,
    my_nick: &str,
    network: &str,
    server: &str,
    actions: Vec<Action>,
) {
    let manager = app.try_state::<ConnectionManager>();
    for action in actions {
        match action {
            Action::Send(line) => {
                // Echo scripted chat messages locally so the sender sees their
                // own output (like mIRC). Raw commands (MODE/JOIN/…) are skipped
                // — those become visible through the server's own reply.
                if let Some(ev) = self_echo(server_id, my_nick, &line) {
                    let _ = app.emit(IRC_EVENT, ev);
                }
                if let Some(m) = &manager {
                    let _ = m.send(server_id, line);
                }
            }
            Action::Echo { target, text } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::Echo {
                        server_id: server_id.to_string(),
                        target,
                        text,
                    },
                );
            }
            Action::Timer {
                name,
                reps,
                interval_ms,
                command,
                target,
            } => {
                if let Some(m) = app.try_state::<timer::TimerManager>() {
                    m.start(
                        app.clone(),
                        server_id.to_string(),
                        my_nick.to_string(),
                        network.to_string(),
                        server.to_string(),
                        name,
                        reps,
                        interval_ms,
                        command,
                        target,
                    );
                }
            }
            Action::TimerStop { name } => {
                if let Some(m) = app.try_state::<timer::TimerManager>() {
                    m.stop(&name);
                }
            }
            Action::TimerList { target } => {
                let names = app
                    .try_state::<timer::TimerManager>()
                    .map(|m| m.list())
                    .unwrap_or_default();
                let text = if names.is_empty() {
                    "No active timers".to_string()
                } else {
                    format!("Active timers: {}", names.join(", "))
                };
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::Echo {
                        server_id: server_id.to_string(),
                        target,
                        text,
                    },
                );
            }
            Action::SockOpen { name, host, port, tls } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.open(
                        app.clone(),
                        server_id.to_string(),
                        network.to_string(),
                        my_nick.to_string(),
                        name,
                        host,
                        port,
                        tls,
                    );
                }
            }
            Action::SockWrite { name, data } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.write(&name, data);
                }
            }
            Action::SockClose { name } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.close(&name);
                }
            }
            Action::DialogOpen { name, title, controls } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::DialogOpen { server_id: server_id.to_string(), name, title, controls },
                );
            }
            Action::DialogClose { name } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::DialogClose { server_id: server_id.to_string(), name },
                );
            }
            Action::DialogSet { dialog, control, op, value } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::DialogSet { server_id: server_id.to_string(), dialog, control, op, value },
                );
            }
            Action::NickIcon { nick, icon } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::NickIcon { server_id: server_id.to_string(), nick, icon },
                );
            }
        }
    }
}

/// Runs a user-typed alias (invoked by the frontend for unknown `/commands`).
/// Returns true if an alias handled it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_run_alias(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    name: String,
    args: String,
) -> bool {
    if !engine.has_alias(&name) {
        return false;
    }
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default(),
    };
    let actions = engine.run_alias(&ctx, &target, &name, &args);
    apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    true
}

/// Returns the user-defined popup items for a context (nicklist / channel / status
/// / menubar), with dynamic labels ($iif/$sock/…) evaluated against the right-click
/// context and empty-label items dropped (mIRC behaviour).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_popups(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    context: String,
    nick: String,
) -> Vec<PopupItem> {
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default(),
    };
    let chan = if is_channel(&target) { target.as_str() } else { "" };
    engine.popups_evaluated(&ctx, &context, &nick, chan)
}

/// Fires an `on DIALOG` handler when the user interacts with a script dialog.
/// `control` is the control that triggered it (a button id, or `init`/`close`);
/// `values` is the current id->value of every control, exposed via `$did`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_run_dialog(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    server_id: String,
    my_nick: String,
    network: String,
    dialog: String,
    control: String,
    values: HashMap<String, String>,
) {
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default(),
    };
    let vars = EventVars {
        nick: control.clone(),
        chan: dialog.clone(),
        target: dialog,
        text: control.clone(),
        params: vec![control],
        did: values,
        ..Default::default()
    };
    let actions = engine.dispatch_event(&ctx, "DIALOG", vars);
    apply_actions(&app, &server_id, &my_nick, &network, "", actions);
}

/// Returns the names of all open script sockets (for `/socklist`).
#[tauri::command]
pub fn script_sockets(socks: State<'_, socket::SocketManager>) -> Vec<String> {
    socks.names()
}

/// Runs a typed command line through the engine (built-in script commands like
/// /sockopen, /timer, /hadd, a user alias, or — failing those — a raw IRC line).
/// Used for input the frontend's own `/command` handling doesn't cover.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_run_command(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    command: String,
    args: String,
) {
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default(),
    };
    let line = if args.is_empty() {
        command
    } else {
        format!("{command} {args}")
    };
    let actions = engine.run_command(&ctx, &target, &line, &[]);
    apply_actions(&app, &server_id, &my_nick, &network, "", actions);
}

/// Fires `on INPUT` handlers for a line the user typed (the line is still sent
/// normally by the caller; this just lets scripts react to it).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_run_input(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    text: String,
) -> bool {
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default(),
    };
    let chan = if is_channel(&target) { target.clone() } else { String::new() };
    let vars = EventVars {
        nick: my_nick.clone(),
        chan,
        target: target.clone(),
        text: text.clone(),
        params: text.split_whitespace().map(String::from).collect(),
        ..Default::default()
    };
    let (actions, halted) = engine.dispatch_event_halt(&ctx, "INPUT", vars);
    apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    // `/halt` in an on INPUT handler suppresses the default send.
    halted
}

/// Runs a popup item's command, with `params` populating `$1..` (e.g. the
/// selected nick) and `target` giving `$chan`/`$target`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_run_popup(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    command: String,
    params: Vec<String>,
) {
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state: app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default(),
    };
    let actions = engine.run_command(&ctx, &target, &command, &params);
    apply_actions(&app, &server_id, &my_nick, &network, "", actions);
}

/// Drives event handlers from a UI event produced by the connection. Returns
/// the resulting outgoing lines and echo events to apply.
pub fn drive_event(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    ev: &UiEvent,
) -> Vec<Action> {
    let (kind, vars) = match ev {
        UiEvent::Message { kind, from, target, text, .. } => {
            let from = from.clone().unwrap_or_default();
            if from == ctx.my_nick {
                return Vec::new();
            }
            let is_chan = is_channel(target);
            let action = text.strip_prefix('\u{1}').is_some() && text.contains("ACTION");
            let kind = match kind {
                crate::irc::event::MessageKind::Notice => "NOTICE",
                _ if action => "ACTION",
                _ => "TEXT",
            };
            let clean = text
                .trim_start_matches('\u{1}')
                .trim_end_matches('\u{1}')
                .strip_prefix("ACTION ")
                .unwrap_or(text)
                .to_string();
            let vars = EventVars {
                nick: from.clone(),
                chan: if is_chan { target.clone() } else { String::new() },
                target: if is_chan { target.clone() } else { from.clone() },
                params: words(&clean),
                text: clean,
                ..Default::default()
            };
            (kind, vars)
        }
        UiEvent::Join { channel, nick, .. } => {
            if nick == ctx.my_nick {
                return Vec::new();
            }
            (
                "JOIN",
                EventVars {
                    nick: nick.clone(),
                    chan: channel.clone(),
                    target: channel.clone(),
                    ..Default::default()
                },
            )
        }
        UiEvent::Part { channel, nick, reason, .. } => (
            "PART",
            EventVars {
                nick: nick.clone(),
                chan: channel.clone(),
                target: channel.clone(),
                params: words(reason.as_deref().unwrap_or("")),
                text: reason.clone().unwrap_or_default(),
                ..Default::default()
            },
        ),
        UiEvent::Quit { nick, reason, .. } => (
            "QUIT",
            EventVars {
                nick: nick.clone(),
                params: words(reason.as_deref().unwrap_or("")),
                text: reason.clone().unwrap_or_default(),
                ..Default::default()
            },
        ),
        UiEvent::NickChange { old, new, .. } => (
            "NICK",
            EventVars {
                // $nick = old nick, $1 / $newnick = the new nick.
                nick: old.clone(),
                knick: new.clone(),
                text: new.clone(),
                params: vec![new.clone()],
                ..Default::default()
            },
        ),
        UiEvent::Kick { channel, nick, by, reason, .. } => (
            "KICK",
            EventVars {
                // $nick = kicker, $knick = the kicked user (mIRC semantics).
                nick: by.clone().unwrap_or_default(),
                knick: nick.clone(),
                chan: channel.clone(),
                target: channel.clone(),
                params: words(reason.as_deref().unwrap_or("")),
                text: reason.clone().unwrap_or_default(),
                ..Default::default()
            },
        ),
        UiEvent::Topic { channel, topic, set_by, .. } => {
            // Only fire on a live change (set_by present), not the join-time
            // RPL_TOPIC snapshot.
            let Some(setter) = set_by else {
                return Vec::new();
            };
            (
                "TOPIC",
                EventVars {
                    nick: setter.clone(),
                    chan: channel.clone(),
                    target: channel.clone(),
                    params: words(topic.as_deref().unwrap_or("")),
                    text: topic.clone().unwrap_or_default(),
                    ..Default::default()
                },
            )
        }
        UiEvent::Invite { from, channel, .. } => (
            "INVITE",
            EventVars {
                nick: from.clone().unwrap_or_default(),
                chan: channel.clone(),
                target: channel.clone(),
                ..Default::default()
            },
        ),
        UiEvent::Mode { target, modes, by, .. } => {
            let setter = by.clone().unwrap_or_default();
            let chan = if is_channel(target) { target.clone() } else { String::new() };
            // Generic `on MODE` ($1- = the whole change).
            let generic = EventVars {
                nick: setter.clone(),
                chan: chan.clone(),
                target: target.clone(),
                params: words(modes),
                text: modes.clone(),
                ..Default::default()
            };
            let mut actions = engine.dispatch_event(ctx, "MODE", generic);
            // Plus a specific event per prefix/ban change (on OP/DEOP/BAN/…),
            // with the affected nick/mask as $1 and $knick/$opnick/$bnick/…
            for (kind, affected) in split_mode_events(modes) {
                let vars = EventVars {
                    nick: setter.clone(),
                    knick: affected.clone(),
                    chan: chan.clone(),
                    target: target.clone(),
                    params: vec![affected.clone()],
                    text: affected,
                    ..Default::default()
                };
                actions.extend(engine.dispatch_event(ctx, kind, vars));
            }
            return actions;
        }
        UiEvent::Disconnected { .. } => ("DISCONNECT", EventVars::default()),
        UiEvent::Registered { .. } => ("CONNECT", EventVars::default()),
        _ => return Vec::new(),
    };
    engine.dispatch_event(ctx, kind, vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>() -> RunCtx<'a> {
        RunCtx {
            my_nick: "me",
            network: "Net",
            server: "irc.example.org",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
        }
    }

    #[test]
    fn alias_sends_message() {
        let engine = ScriptEngine::new();
        engine.load("alias hi { /msg $chan hello $me }");
        let actions = engine.run_alias(&ctx(), "#test", "hi", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #test :hello me".into())]);
    }

    #[test]
    fn local_alias_callable_from_script_not_input() {
        // A `-l` local helper must be invokable from another alias, but not as a
        // user `/command` (which would otherwise be sent to the server as raw).
        let engine = ScriptEngine::new();
        engine.load("alias -l helper { /msg #c from-helper }\nalias go { helper }");
        // invoked from within `go`: resolves and runs the helper body
        let actions = engine.run_alias(&ctx(), "#c", "go", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :from-helper".into())]);
        // invoked directly as a user command: not exposed
        assert!(!engine.has_alias("helper"));
        assert!(engine.run_alias(&ctx(), "#c", "helper", "").is_empty());
        // a normal (global) alias is still user-callable
        assert!(engine.has_alias("go"));
    }

    #[test]
    fn text_event_responds() {
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:!ping*:#:{ /msg $chan pong $nick }");
        let vars = EventVars {
            nick: "bob".into(),
            chan: "#test".into(),
            target: "#test".into(),
            text: "!ping now".into(),
            params: vec!["!ping".into(), "now".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "TEXT", vars);
        assert_eq!(actions, vec![Action::Send("PRIVMSG #test :pong bob".into())]);
    }

    #[test]
    fn custom_identifier_alias_returns_value() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias double { /return $calc($1 * 2) }\nalias t { /msg #c result $double(5) }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :result 10".into())]);
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = std::env::temp_dir().join(format!("jirc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/write -c notes.txt first line", &[]);
        engine.run_command(&rctx, "#c", "/write notes.txt second line", &[]);
        engine.load("alias r { /msg #c $read(notes.txt, 2) [ $+ $lines(notes.txt) $+ ] }");
        let actions = engine.run_alias(&rctx, "#c", "r", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :second line [2]".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn socket_commands_produce_actions() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias go { /sockopen -e bot irc.example.org 6667 | /sockwrite -n bot NICK x | /sockclose bot }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "go", "");
        assert_eq!(
            actions,
            vec![
                Action::SockOpen { name: "bot".into(), host: "irc.example.org".into(), port: 6667, tls: true },
                Action::SockWrite { name: "bot".into(), data: b"NICK x\r\n".to_vec() },
                Action::SockClose { name: "bot".into() },
            ]
        );
    }

    #[test]
    fn sockread_consumes_line_and_sets_sockbr() {
        let engine = ScriptEngine::new();
        // First /sockread gets the line; the while loop then ends ($sockbr 0).
        engine.load(
            "on *:SOCKREAD:bot:{ /sockread %x | /msg #c got %x len $sockbr | /sockread %y | /msg #c again [ $+ %y $+ ] $sockbr }",
        );
        let vars = EventVars {
            chan: "bot".into(),
            target: "bot".into(),
            text: "PING 123".into(),
            params: vec!["PING".into(), "123".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "SOCKREAD", vars);
        assert_eq!(
            actions,
            vec![
                Action::Send("PRIVMSG #c :got PING 123 len 8".into()),
                Action::Send("PRIVMSG #c :again [] 0".into()),
            ]
        );
    }

    #[test]
    fn sockread_only_fires_for_matching_name() {
        let engine = ScriptEngine::new();
        engine.load("on *:SOCKREAD:bot:{ /msg #c hit }");
        let other = EventVars { chan: "other".into(), target: "other".into(), ..Default::default() };
        assert!(engine.dispatch_event(&ctx(), "SOCKREAD", other).is_empty());
    }

    #[test]
    fn state_aware_identifiers() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![
                ChannelView { name: "#a".into(), nicks: vec!["me".into(), "bob".into()], ..Default::default() },
                ChannelView { name: "#b".into(), nicks: vec!["me".into()], ..Default::default() },
            ],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "alias t { /echo chans=$chan(0) first=$chan(1) users=$nick(#a, 0) u2=$nick(#a, 2) com=$comchan(bob, 0) on=$onchan(#b) }",
        );
        let actions = engine.run_alias(&rctx, "#a", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "#a".into(),
                text: "chans=2 first=#a users=2 u2=bob com=1 on=$true".into(),
            }]
        );
    }

    #[test]
    fn dialog_open_produces_action() {
        let engine = ScriptEngine::new();
        engine.load("dialog g {\n title \"Hi\"\n edit name\n}\nalias o { /dialog g }");
        let actions = engine.run_alias(&ctx(), "#c", "o", "");
        match &actions[..] {
            [Action::DialogOpen { name, title, controls }] => {
                assert_eq!(name, "g");
                assert_eq!(title, "Hi");
                assert_eq!(controls.len(), 1);
                assert_eq!(controls[0].id, "name");
            }
            _ => panic!("expected DialogOpen, got {actions:?}"),
        }
    }

    #[test]
    fn dialog_event_reads_values_and_acts() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:DIALOG:g:{ if ($1 == send) { /msg #c hi $did(g, name) | /dialog -c g } }",
        );
        let mut vals = std::collections::HashMap::new();
        vals.insert("name".to_string(), "bob".to_string());
        let vars = EventVars {
            chan: "g".into(),
            target: "g".into(),
            text: "send".into(),
            params: vec!["send".into()],
            did: vals,
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "DIALOG", vars);
        assert_eq!(
            actions,
            vec![
                Action::Send("PRIVMSG #c :hi bob".into()),
                Action::DialogClose { name: "g".into() },
            ]
        );
    }

    #[test]
    fn ial_and_address_identifiers() {
        use crate::irc::state::StateSnapshot;
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![],
            ial: vec![
                ("bob".into(), "bob!~bob@host.example.com".into()),
                ("alice".into(), "alice!ali@other.net".into()),
            ],
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "alias t { /echo a=$address(bob) m2=$mask(bob!~bob@host.example.com, 2) m3=$address(bob, 3) c=$ial(*!*@*.example.com, 0) n=$ial(*!*@*.example.com, 1) }",
        );
        let actions = engine.run_alias(&rctx, "#c", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "#c".into(),
                text: "a=~bob@host.example.com m2=*!*bob@host.example.com m3=*!*@host.example.com c=1 n=bob".into(),
            }]
        );
    }

    #[test]
    fn event_address_and_whitespace_identifiers() {
        use crate::irc::state::StateSnapshot;
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![],
            ial: vec![("bob".into(), "bob!~bob@host.example.com".into())],
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        // Bare $address/$site/$fulladdress/$wildsite resolve the triggering user
        // from the IAL; the whitespace constants expand to real control chars.
        engine.load(
            "on *:TEXT:*:#:{ /echo a=$address s=$site f=$fulladdress w=$wildsite t=[$tab]c=[$cr]l=[$lf]nl=[$crlf] }",
        );
        let vars = EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "hi".into(),
            params: vec!["hi".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&rctx, "TEXT", vars);
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "#c".into(),
                text: "a=~bob@host.example.com s=host.example.com f=bob!~bob@host.example.com w=*!*@host.example.com t=[\t]c=[\r]l=[\n]nl=[\r\n]".into(),
            }]
        );
    }

    #[test]
    fn list_operators_use_channel_state() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![ChannelView {
                name: "#a".into(),
                nicks: vec!["op".into(), "voiced".into(), "plain".into()],
                members: vec![
                    ("op".into(), "@".into()),
                    ("voiced".into(), "+".into()),
                    ("plain".into(), String::new()),
                ],
            }],
            ial: vec![],
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "on *:TEXT:*:#:{
              if (op isop #a) { /echo op-is-op }
              if (!plain isop #a) { /echo plain-not-op }
              if (voiced isvoice #a) { /echo voiced-ok }
              if (plain isreg #a) { /echo plain-reg }
              if (op ison #a) { /echo ison-ok }
              if (#a ischan) { /echo ischan-ok }
              if (ghost isop #a) { /echo should-not-fire }
              if (6 & 2) { /echo bitand }
            }",
        );
        let vars = EventVars {
            nick: "op".into(),
            chan: "#a".into(),
            target: "#a".into(),
            text: "hi".into(),
            params: vec!["hi".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&rctx, "TEXT", vars);
        let echoed: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Echo { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            echoed,
            vec!["op-is-op", "plain-not-op", "voiced-ok", "plain-reg", "ison-ok", "ischan-ok", "bitand"]
        );
    }

    #[test]
    fn hash_save_load_and_find() {
        let dir = std::env::temp_dir().join(format!("jirc-htest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/hadd seen alice 10", &[]);
        engine.run_command(&rctx, "#c", "/hadd seen bob 20", &[]);
        engine.run_command(&rctx, "#c", "/hsave seen seen.txt", &[]);

        let engine2 = ScriptEngine::new();
        engine2.run_command(&rctx, "#c", "/hload seen seen.txt", &[]);
        engine2.load("alias r { /msg #c $hget(seen, bob) and $hfind(seen, a*, 1) }");
        let actions = engine2.run_alias(&rctx, "#c", "r", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :20 and alice".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_make_add_clear_free() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias addvar { hadd -m room $1 Lobby }\n\
             alias getval { /msg #c $hget(room,key.1) }\n\
             alias gettab { /msg #c $hget(room) }",
        );
        // /hmake creates an empty table -> $hget(table) is truthy (its name).
        engine.run_command(&ctx(), "#c", "/hmake room 10", &[]);
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "gettab", ""),
            vec![Action::Send("PRIVMSG #c :room".into())]
        );
        // A variable/identifier item key ($1) is expanded on insert so the read
        // back under the same expanded key matches (the bug that broke i7flood).
        engine.run_alias(&ctx(), "#c", "addvar", "key.1");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "getval", ""),
            vec![Action::Send("PRIVMSG #c :Lobby".into())]
        );
        // /hclear empties the items but keeps the table.
        engine.run_command(&ctx(), "#c", "/hclear room", &[]);
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "getval", ""),
            vec![Action::Send("PRIVMSG #c :".into())]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "gettab", ""),
            vec![Action::Send("PRIVMSG #c :room".into())]
        );
        // /hfree removes the table entirely.
        engine.run_command(&ctx(), "#c", "/hfree room", &[]);
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "gettab", ""),
            vec![Action::Send("PRIVMSG #c :".into())]
        );
    }

    #[test]
    fn var_assignment_tokenize_hinc() {
        let engine = ScriptEngine::new();
        // /var with `=` and comma-separated decls; /set space form; /unset wildcard
        engine.load(
            "alias t {\n\
               var %a = hello, %b = $calc(2 + 3), %c\n\
               set %d world\n\
               /msg #c a=$+(%a) b=$+(%b) c=[ $+ %c $+ ] d=$+(%d)\n\
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :a=hello b=5 c=[] d=world".into())]
        );

        // /tokenize rebinds $1.. from the given text
        engine.load("alias t { tokenize 32 $2- | /msg #c first=$1 last=$3 }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "cmd x y z"),
            vec![Action::Send("PRIVMSG #c :first=x last=z".into())]
        );

        // /hinc and /hdec on a numeric hash item
        engine.load("alias t { hinc c hits 5 | hinc c hits | hdec c hits 2 | /msg #c $hget(c,hits) }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :4".into())]
        );
    }

    #[test]
    fn braceless_if_executes_conditionally() {
        let engine = ScriptEngine::new();
        // body runs to the first `|`; the rest is unconditional
        engine.load("alias t { if ($1 == yes) /msg #c YES | /msg #c always }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "yes"),
            vec![
                Action::Send("PRIVMSG #c :YES".into()),
                Action::Send("PRIVMSG #c :always".into()),
            ]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "no"),
            vec![Action::Send("PRIVMSG #c :always".into())]
        );
        // brace-less elseif/else chain across lines
        engine.load(
            "alias t {\n  if ($1 == 1) /msg #c one\n  elseif ($1 == 2) /msg #c two\n  else /msg #c other\n}",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "2"),
            vec![Action::Send("PRIVMSG #c :two".into())]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "9"),
            vec![Action::Send("PRIVMSG #c :other".into())]
        );
    }

    #[test]
    fn dotted_variable_names() {
        // mIRC %vars can contain dots; %i7f.host must be one variable, not
        // %i7f followed by literal ".host".
        let engine = ScriptEngine::new();
        engine.load(
            "alias t { set %i7f.host irc.irc7.com | var %i7f.port = 6667 | /msg #c $+(%i7f.host) : $+(%i7f.port) }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :irc.irc7.com : 6667".into())]
        );
    }

    #[test]
    fn unset_wildcard_removes_matching() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               set %i7f.a 1\n\
               set %i7f.b 2\n\
               set %keep 3\n\
               unset %i7f.*\n\
               /msg #c a=[ $+ %i7f.a $+ ] keep=$+(%keep)\n\
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :a=[] keep=3".into())]
        );
    }

    #[test]
    fn amsg_and_ban_use_state() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![
                ChannelView { name: "#a".into(), nicks: vec!["me".into(), "bob".into()], ..Default::default() },
                ChannelView { name: "#b".into(), nicks: vec!["me".into()], ..Default::default() },
            ],
            ial: vec![("bob".into(), "bob!user@host.example.com".into())],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        engine.load("alias a { /amsg hi all }\nalias b { /ban #a bob }");
        // /amsg goes to every joined channel
        assert_eq!(
            engine.run_alias(&rctx, "#a", "a", ""),
            vec![
                Action::Send("PRIVMSG #a :hi all".into()),
                Action::Send("PRIVMSG #b :hi all".into()),
            ]
        );
        // /ban masks a known nick via the IAL (default type 2) and sets +b
        assert_eq!(
            engine.run_alias(&rctx, "#a", "b", ""),
            vec![Action::Send("MODE #a +b *!*user@host.example.com".into())]
        );
    }

    #[test]
    fn unknown_client_command_does_not_leak_to_server() {
        // /clear etc. are client-side: they must NOT become a raw IRC line.
        let engine = ScriptEngine::new();
        engine.load("alias t { clear | window @x | beep 1 100 | /msg #c done }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :done".into())]
        );
        // A genuine IRC command still falls through to raw.
        engine.load("alias t { whois someone }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("WHOIS someone".into())]
        );
    }

    #[test]
    fn input_halt_suppresses_line() {
        let engine = ScriptEngine::new();
        engine.load("on *:INPUT:*:{ if (spam isin $1-) { /halt } }");
        let spam = EventVars {
            text: "spam now".into(),
            params: vec!["spam".into(), "now".into()],
            ..Default::default()
        };
        assert!(engine.dispatch_event_halt(&ctx(), "INPUT", spam).1);
        let ok = EventVars {
            text: "hello".into(),
            params: vec!["hello".into()],
            ..Default::default()
        };
        assert!(!engine.dispatch_event_halt(&ctx(), "INPUT", ok).1);
    }

    #[test]
    fn input_event_fires_on_own_text() {
        let engine = ScriptEngine::new();
        engine.load("on *:INPUT:#:{ /msg $chan you said $1- }");
        let vars = EventVars {
            nick: "me".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "hi there".into(),
            params: vec!["hi".into(), "there".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "INPUT", vars);
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :you said hi there".into())]);
    }

    #[test]
    fn per_mode_events_fire() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:OP:#:{ /msg $chan $nick opped $opnick }\non *:BAN:#:{ /msg $chan $nick banned $bnick }",
        );
        let ev = UiEvent::Mode {
            server_id: "s".into(),
            target: "#c".into(),
            modes: "+o bob -v alice +b m!*@*".into(),
            by: Some("op".into()),
        };
        let actions = drive_event(&engine, &ctx(), &ev);
        assert_eq!(
            actions,
            vec![
                Action::Send("PRIVMSG #c :op opped bob".into()),
                Action::Send("PRIVMSG #c :op banned m!*@*".into()),
            ]
        );
    }

    #[test]
    fn mode_event_fires_with_setter() {
        let engine = ScriptEngine::new();
        engine.load("on *:MODE:#:{ /msg $chan $nick set $1- }");
        let ev = UiEvent::Mode {
            server_id: "s".into(),
            target: "#test".into(),
            modes: "+o bob".into(),
            by: Some("op".into()),
        };
        let actions = drive_event(&engine, &ctx(), &ev);
        assert_eq!(actions, vec![Action::Send("PRIVMSG #test :op set +o bob".into())]);
    }

    #[test]
    fn kick_event_exposes_kicker_and_kicked() {
        let engine = ScriptEngine::new();
        engine.load("on *:KICK:#:{ /msg $chan $knick was kicked by $nick ( $+ $1- $+ ) }");
        let ev = UiEvent::Kick {
            server_id: "s".into(),
            channel: "#test".into(),
            nick: "victim".into(),
            by: Some("op".into()),
            reason: Some("bye".into()),
            is_self: false,
        };
        let actions = drive_event(&engine, &ctx(), &ev);
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #test :victim was kicked by op (bye)".into())]
        );
    }

    #[test]
    fn event_pattern_must_match() {
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:!ping*:#:{ /msg $chan pong }");
        let vars = EventVars {
            nick: "bob".into(),
            chan: "#test".into(),
            target: "#test".into(),
            text: "hello".into(),
            ..Default::default()
        };
        assert!(engine.dispatch_event(&ctx(), "TEXT", vars).is_empty());
    }

    #[test]
    fn if_else_and_vars() {
        let engine = ScriptEngine::new();
        engine.load("alias t { /set %n 2 | if (%n == 2) { /echo two } else { /echo other } }");
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "#c".into(),
                text: "two".into()
            }]
        );
    }

    #[test]
    fn timer_produces_action() {
        let engine = ScriptEngine::new();
        engine.load("alias t { /timer 3 5 /msg #c tick }");
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert_eq!(
            actions,
            vec![Action::Timer {
                name: String::new(),
                reps: 3,
                interval_ms: 5000,
                command: "/msg #c tick".into(),
                target: "#c".into(),
            }]
        );
    }

    #[test]
    fn alias_resolves_chan_on_ircx_channel() {
        let engine = ScriptEngine::new();
        engine.load("alias hi { /msg $chan yo }");
        let actions = engine.run_alias(&ctx(), "%#lobby", "hi", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG %#lobby :yo".into())]);
    }

    #[test]
    fn scripted_privmsg_echoes_locally() {
        match self_echo("s1", "me", "PRIVMSG #c :hi there") {
            Some(UiEvent::Message { from, target, text, kind, .. }) => {
                assert_eq!(from.as_deref(), Some("me"));
                assert_eq!(target, "#c");
                assert_eq!(text, "hi there");
                assert!(matches!(kind, MessageKind::Privmsg));
            }
            _ => panic!("expected a local echo"),
        }
        // Raw commands aren't echoed (their effect shows via the server reply).
        assert!(self_echo("s1", "me", "MODE #c +o bob").is_none());
        assert!(self_echo("s1", "me", "WHOIS bob").is_none());
    }

    #[test]
    fn goto_loops_across_a_block() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t { /set %i 0 | :top | /inc %i | /echo %i | if (%i < 3) { /goto top } | /echo done }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        let echo = |t: &str| Action::Echo { target: "#c".into(), text: t.into() };
        assert_eq!(actions, vec![echo("1"), echo("2"), echo("3"), echo("done")]);
    }

    #[test]
    fn named_timer_start_and_stop() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias a { /timerfoo 2 1 /msg #c tick }\n\
             alias b { /timerfoo off }\n\
             alias c { /timers off }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "a", ""),
            vec![Action::Timer {
                name: "foo".into(),
                reps: 2,
                interval_ms: 1000,
                command: "/msg #c tick".into(),
                target: "#c".into(),
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "b", ""),
            vec![Action::TimerStop { name: "foo".into() }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "c", ""),
            vec![Action::TimerStop { name: "*".into() }]
        );
    }

    #[test]
    fn while_loop_counts() {
        let engine = ScriptEngine::new();
        engine.load("alias t { /set %i 0 | while (%i < 3) { /inc %i } | /echo done $+ %i }");
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "#c".into(),
                text: "done3".into()
            }]
        );
    }
}
