//! The mIRC scripting (mSL) engine: compiles scripts and runs aliases and
//! event handlers, producing [`Action`]s (lines to send / text to echo).
//!
//! This is a substantial, working subset of mSL — aliases, events, control
//! flow, variables, hash tables, and a library of identifiers and commands —
//! not a 100% mIRC-compatible implementation.

pub mod ast;
pub mod binvar;
pub mod eval;
pub mod files;
pub mod ident;
pub mod ini;
pub mod input;
pub mod parser;
pub mod socket;
pub mod timer;
pub mod window;

use std::collections::HashMap;
use std::sync::Mutex;

use ast::{PopupItem, Script};
use eval::{wildcard_match, Action, EventVars, NoInput, NoSockets, Runtime, ScriptInput, ScriptSockets};

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
    files: files::FileStore,
    bins: binvar::BinStore,
    windows: window::WindowStore,
    sockets: std::sync::Arc<dyn ScriptSockets>,
    input: std::sync::Arc<dyn ScriptInput>,
    /// The frontend's currently-focused window/buffer name, for `$active`.
    active: String,
    /// Numeric connection-id registry for `$cid`/`$scon`/`$activecid`.
    conns: ConnReg,
    /// Numeric window-id registry for `$wid`/`$activewid`.
    wins: WinReg,
}

impl Inner {
    fn empty() -> Self {
        Inner {
            script: Script::default(),
            vars: HashMap::new(),
            hashes: HashMap::new(),
            files: files::FileStore::default(),
            bins: binvar::BinStore::default(),
            windows: window::WindowStore::default(),
            sockets: std::sync::Arc::new(NoSockets),
            input: std::sync::Arc::new(NoInput),
            active: String::new(),
            conns: ConnReg::default(),
            wins: WinReg::default(),
        }
    }
}

/// Assigns each connection a small, stable number (`$cid`) in connect order and
/// tracks which one owns the active window (`$activecid`).
#[derive(Default)]
struct ConnReg {
    next: u32,
    /// `(cid, server_id)` in ascending cid order.
    entries: Vec<(u32, String)>,
    /// The active window's server id.
    active: String,
}

impl ConnReg {
    /// Assigns a cid for a server id (idempotent — a reconnect keeps its number).
    fn assign(&mut self, server_id: &str) -> u32 {
        if let Some((c, _)) = self.entries.iter().find(|(_, id)| id == server_id) {
            return *c;
        }
        self.next += 1;
        self.entries.push((self.next, server_id.to_string()));
        self.next
    }

    fn forget(&mut self, server_id: &str) {
        self.entries.retain(|(_, id)| id != server_id);
    }

    fn view(&self) -> crate::script::eval::ConnsView {
        let active_cid = self
            .entries
            .iter()
            .find(|(_, id)| *id == self.active)
            .map(|(c, _)| *c)
            .unwrap_or(0);
        crate::script::eval::ConnsView { entries: self.entries.clone(), active_cid }
    }
}

/// Assigns each open window a small, stable number (`$wid`) as the frontend
/// opens it, and tracks which one is active (`$activewid`). Keyed by
/// `(server_id, window name)` — the same name the UI reports for `$active`.
#[derive(Default)]
struct WinReg {
    next: u32,
    /// `(wid, server_id, name)` for every open window.
    entries: Vec<(u32, String, String)>,
    active_wid: u32,
}

impl WinReg {
    fn open(&mut self, server_id: &str, name: &str) -> u32 {
        if let Some((w, _, _)) = self
            .entries
            .iter()
            .find(|(_, s, n)| s == server_id && n.eq_ignore_ascii_case(name))
        {
            return *w;
        }
        self.next += 1;
        self.entries.push((self.next, server_id.to_string(), name.to_string()));
        self.next
    }

    fn close(&mut self, server_id: &str, name: &str) {
        self.entries
            .retain(|(_, s, n)| !(s == server_id && n.eq_ignore_ascii_case(name)));
    }

    fn set_active(&mut self, server_id: &str, name: &str) {
        self.active_wid = self
            .entries
            .iter()
            .find(|(_, s, n)| s == server_id && n.eq_ignore_ascii_case(name))
            .map(|(w, _, _)| *w)
            .unwrap_or(0);
    }

    fn view(&self) -> crate::script::eval::WinView {
        crate::script::eval::WinView { entries: self.entries.clone(), active_wid: self.active_wid }
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

    /// Installs the (production) socket backend; called once at startup so the
    /// engine can run `/socklisten`/`/sockaccept`/`$sock(...)` against real sockets.
    pub fn set_sockets(&self, sockets: std::sync::Arc<dyn ScriptSockets>) {
        self.inner.lock().unwrap().sockets = sockets;
    }

    /// Installs the (production) `$input` prompt backend; called once at startup.
    pub fn set_input(&self, input: std::sync::Arc<dyn ScriptInput>) {
        self.inner.lock().unwrap().input = input;
    }

    /// Records the frontend's currently-focused window/buffer name (for `$active`).
    pub fn set_active(&self, name: &str) {
        self.inner.lock().unwrap().active = name.to_string();
    }

    /// Assigns (idempotently) the numeric `$cid` for a connection; returns it.
    pub fn assign_cid(&self, server_id: &str) -> u32 {
        self.inner.lock().unwrap().conns.assign(server_id)
    }

    /// Drops a connection's `$cid` entry (on disconnect).
    pub fn forget_cid(&self, server_id: &str) {
        self.inner.lock().unwrap().conns.forget(server_id);
    }

    /// Records which connection owns the active window (for `$activecid`).
    pub fn set_active_conn(&self, server_id: &str) {
        self.inner.lock().unwrap().conns.active = server_id.to_string();
    }

    /// Assigns (idempotently) the `$wid` for a window as the UI opens it.
    pub fn window_open(&self, server_id: &str, name: &str) -> u32 {
        self.inner.lock().unwrap().wins.open(server_id, name)
    }

    /// Drops a window's `$wid` when the UI closes it.
    pub fn window_close(&self, server_id: &str, name: &str) {
        self.inner.lock().unwrap().wins.close(server_id, name);
    }

    /// Records which window is active (for `$activewid`).
    pub fn set_active_win(&self, server_id: &str, name: &str) {
        self.inner.lock().unwrap().wins.set_active(server_id, name);
    }

    /// Compiles the combined source of all loaded script files.
    pub fn load(&self, source: &str) {
        let mut g = self.inner.lock().unwrap();
        g.script = parser::parse(source);
    }

    pub fn has_alias(&self, name: &str) -> bool {
        // Local (`-l`) aliases aren't user-callable as `/commands`, and a disabled
        // `#group` makes its aliases uncallable too.
        let g = self.inner.lock().unwrap();
        g.script
            .find_alias(name)
            .is_some_and(|a| !a.local && g.script.group_enabled(&g.vars, &a.group))
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
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            event,
            actions: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            input: g.input.clone(),
            caller: "menu",
            show: true,
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
        // A disabled `#group` makes its aliases uncallable.
        if !script.group_enabled(&g.vars, &alias.group) {
            return Vec::new();
        }
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
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            event,
            actions: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            input: g.input.clone(),
            caller: "command",
            show: true,
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
        self.run_command_snicks(ctx, target, command, params, &[])
    }

    /// Like [`run_command`], but also supplies the selected nicknames for a
    /// nicklist popup run (`$snick`/`$snicks`). `params` still drive `$1..`
    /// ($1 = the right-clicked nick); `snicks` is the full listbox selection.
    pub fn run_command_snicks(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        params: &[String],
        snicks: &[String],
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
            snicks: snicks.to_vec(),
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
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            event,
            actions: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            input: g.input.clone(),
            caller: "command",
            show: true,
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
        // $event reflects the dispatch kind for every handler (text, raw, op, …).
        let mut event = event;
        event.event = kind.to_ascii_lowercase();
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let g = &mut *g;
        let vars = &mut g.vars;
        let hashes = &mut g.hashes;
        let files = &mut g.files;
        let bins = &mut g.bins;
        let windows = &mut g.windows;
        let mut actions = Vec::new();
        let mut halted = false;
        for ev in script.events_of(kind) {
            if !matches(&event, &ev.pattern, &ev.target, kind) {
                continue;
            }
            // A disabled `#group` suppresses its event handlers.
            if !script.group_enabled(vars, &ev.group) {
                continue;
            }
            let mut rt = Runtime {
                script: &script,
                my_nick: ctx.my_nick,
                network: ctx.network,
                server: ctx.server,
                vars: &mut *vars,
                hashes: &mut *hashes,
                files: &mut *files,
                bins: &mut *bins,
                windows: &mut *windows,
                event: event.clone(),
                actions: Vec::new(),
                halted: false,
                steps: 0,
                depth: 0,
                ret: None,
                goto: None,
                data_dir: ctx.data_dir.clone(),
                state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
                sockets: g.sockets.clone(),
            input: g.input.clone(),
                caller: "event",
                show: true,
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
        // $submenu($id($1)) dynamically generates a flat list of items in place.
        if let Some(arg) = parse_submenu_arg(&item.label) {
            out.extend(expand_submenu(rt, &arg));
            continue;
        }
        // A leading $style(N) sentinel (mIRC requires it be the first word) sets
        // the item's check/disabled state and is stripped from the visible label.
        let expanded = rt.expand(&item.label);
        let (checked, disabled, rest) = split_style_marker(&expanded);
        let label = rest.trim().to_string();
        if label.is_empty() {
            continue;
        }
        out.push(PopupItem {
            label,
            command: item.command.clone(),
            separator: false,
            checked,
            disabled,
            children: eval_popup_labels(rt, &item.children),
        });
    }
    out
}

/// Splits a leading `$style(N)` sentinel off an expanded popup label, returning
/// `(checked, disabled, remaining-label)`. Leading whitespace before the marker
/// (e.g. from an `$iif(...)` that produced nothing) is tolerated.
fn split_style_marker(s: &str) -> (bool, bool, &str) {
    let trimmed = s.trim_start();
    if let Some(rest) = trimmed.strip_prefix(crate::script::eval::STYLE_MARK) {
        match rest.chars().next().and_then(|c| c.to_digit(10)) {
            Some(n) => (n == 1 || n == 3, n == 2 || n == 3, &rest[1..]),
            // A bare marker with no digit: drop it, no style.
            None => (false, false, rest),
        }
    } else {
        (false, false, s)
    }
}

/// If `label` is a `$submenu($id($1))` item, returns the inner argument
/// (e.g. `$animal($1)`); otherwise `None`. The match is case-insensitive and
/// balances parentheses so a nested `(...)` in the argument is kept.
fn parse_submenu_arg(label: &str) -> Option<String> {
    let t = label.trim();
    if !t.to_ascii_lowercase().starts_with("$submenu(") {
        return None;
    }
    let rest = &t["$submenu(".len()..]; // "$submenu(" is 9 ASCII bytes
    let mut depth = 1;
    for (i, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[..i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Expands a `$submenu` argument into a flat list of items, mIRC-style: call the
/// argument with `$1` = `begin`, then `1, 2, …` until it returns empty, then
/// `end`. `begin`/`end` let a script wrap the list in separators. A safety cap
/// bounds a script that never returns empty; nested submenus aren't supported.
fn expand_submenu(rt: &mut Runtime, arg: &str) -> Vec<PopupItem> {
    const CAP: usize = 1000;
    let saved = rt.event.params.clone();
    let mut out = Vec::new();

    rt.event.params = vec!["begin".to_string()];
    if let Some(it) = make_generated_item(&rt.expand(arg)) {
        out.push(it);
    }
    for i in 1..=CAP {
        rt.event.params = vec![i.to_string()];
        let r = rt.expand(arg);
        if r.trim().is_empty() {
            break;
        }
        if let Some(it) = make_generated_item(&r) {
            out.push(it);
        }
    }
    rt.event.params = vec!["end".to_string()];
    if let Some(it) = make_generated_item(&rt.expand(arg)) {
        out.push(it);
    }

    rt.event.params = saved;
    out
}

/// Parses one generated `$submenu` line (`-` separator, or `label:command`, with
/// an optional leading `$style` marker) into a flat popup item.
fn make_generated_item(text: &str) -> Option<PopupItem> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t == "-" {
        return Some(PopupItem {
            label: String::new(),
            command: String::new(),
            separator: true,
            checked: false,
            disabled: false,
            children: Vec::new(),
        });
    }
    let (checked, disabled, rest) = split_style_marker(t);
    let (label, command) = match rest.split_once(':') {
        Some((l, c)) => (l.trim().to_string(), c.trim().to_string()),
        None => (rest.trim().to_string(), String::new()),
    };
    if label.is_empty() {
        return None;
    }
    Some(PopupItem {
        label,
        command,
        separator: false,
        checked,
        disabled,
        children: Vec::new(),
    })
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
fn matches(ev: &EventVars, pattern: &str, target_spec: &str, kind: &str) -> bool {
    let pat_ok = pattern.is_empty()
        || pattern == "*"
        || wildcard_match(pattern, &ev.text)
        // A CTCP matchtext also matches just the command word, so
        // `on CTCP:PING:` catches "PING <timestamp>" (likewise `on CTCPREPLY`).
        || ((kind == "CTCP" || kind == "CTCPREPLY")
            && wildcard_match(pattern, ev.text.split_whitespace().next().unwrap_or("")));
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
    let dir = crate::storage::config_dir(app)?.join("scripts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// The sandbox directory for script file I/O (`$read`/`/write`). Created on
/// demand; falls back to the system temp dir if the config dir is unavailable.
pub fn script_data_dir(app: &AppHandle) -> std::path::PathBuf {
    let dir = crate::storage::config_dir(app)
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

/// Guards `on UNLOAD` firing against an `on UNLOAD` handler that calls `/reload`
/// (which would recompile → fire UNLOAD → … forever).
static FIRING_UNLOAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Fires a global script-lifecycle event (`on START`/`UNLOAD`/`EXIT`) with no
/// connection context and applies the resulting actions. Lifecycle events have
/// no server/window — their commands (timers, hash tables, file I/O) don't need one.
pub fn fire_lifecycle(app: &AppHandle, engine: &ScriptEngine, kind: &str) {
    let ctx = RunCtx {
        my_nick: "",
        network: "",
        server: "",
        data_dir: script_data_dir(app),
        state: std::sync::Arc::new(Default::default()),
    };
    let actions = engine.dispatch_event(&ctx, kind, EventVars::default());
    apply_actions(app, "", "", "", "", actions);
}

/// Reads and compiles every `.mrc` file into the engine.
fn recompile(app: &AppHandle, engine: &ScriptEngine) {
    use std::sync::atomic::Ordering;
    // Fire `on UNLOAD` on the outgoing scripts before replacing them (a no-op on
    // the first, empty load). The guard breaks a /reload-inside-on-UNLOAD loop.
    if !FIRING_UNLOAD.swap(true, Ordering::SeqCst) {
        fire_lifecycle(app, engine, "UNLOAD");
        FIRING_UNLOAD.store(false, Ordering::SeqCst);
    }
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

/// Whether a script line defines an alias named `name` (case-insensitive).
fn alias_line_defines(line: &str, name: &str) -> bool {
    line.trim_start()
        .strip_prefix("alias ")
        .map(|rest| {
            rest.trim_start()
                .split([' ', '\t', '{'])
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(name)
        })
        .unwrap_or(false)
}

/// Adds/replaces (`command` = Some) or removes (`command` = None) a single-line
/// runtime alias (`/alias`) in `_runtime.mrc`, then recompiles so it takes effect.
/// That file sorts first, so a runtime alias overrides a same-named one elsewhere.
fn update_runtime_alias(app: &AppHandle, name: &str, command: Option<&str>) {
    let Ok(dir) = scripts_dir(app) else { return };
    let path = dir.join("_runtime.mrc");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !alias_line_defines(l, name))
        .map(String::from)
        .collect();
    if let Some(cmd) = command {
        lines.push(format!("alias {name} {{ {cmd} }}"));
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = std::fs::write(&path, out);
    if let Some(engine) = app.try_state::<ScriptEngine>() {
        recompile(app, &engine);
    }
}

/// Loads persisted scripts at startup, migrating a legacy single script.mrc and
/// seeding example scripts on first run.
pub fn load_persisted(app: &AppHandle, engine: &ScriptEngine) {
    // First run = the scripts dir does not exist yet.
    let first_run = crate::storage::config_dir(app)
        .map(|c| !c.join("scripts").exists())
        .unwrap_or(false);

    if let Ok(config) = crate::storage::config_dir(app) {
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
    apply_actions_depth(app, server_id, my_nick, network, server, actions, 0);
}

/// `apply_actions` with a recursion `depth`, so `/signal` (which dispatches more
/// handlers, possibly emitting more signals) can be capped like mIRC's 24-deep limit.
fn apply_actions_depth(
    app: &AppHandle,
    server_id: &str,
    my_nick: &str,
    network: &str,
    server: &str,
    actions: Vec<Action>,
    depth: u32,
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
            Action::SetIdentity { field, value } => {
                // Routed to the connection task as an internal control line: it
                // updates the live session state (so $anick/$mnick/$fullname
                // reflect it) and is not forwarded to the server.
                if let Some(m) = &manager {
                    let _ = m.send(server_id, format!("\u{0}SETID {field} {value}"));
                }
            }
            Action::ReloadScripts => {
                // Recompile all script files from disk. Safe here: apply_actions
                // runs after the engine lock (run_command/dispatch) is released.
                if let Some(engine) = app.try_state::<ScriptEngine>() {
                    recompile(app, &engine);
                }
            }
            Action::DefineAlias { name, command } => {
                update_runtime_alias(app, &name, command.as_deref());
            }
            Action::Autojoin { .. } => {
                // Only meaningful at connect time, where the connection task
                // extracts it from the `on CONNECT` actions; a no-op elsewhere.
            }
            Action::Signal { name, params } => {
                // Dispatch `on SIGNAL` handlers after the current run (so it's safe
                // re-entrancy-wise). Capped to mIRC's 24-deep signal recursion.
                if depth < 24 {
                    if let Some(engine) = app.try_state::<ScriptEngine>() {
                        let ctx = RunCtx {
                            my_nick,
                            network,
                            server,
                            data_dir: script_data_dir(app),
                            state: app
                                .try_state::<crate::irc::state::StateStore>()
                                .map(|s| s.get(server_id))
                                .unwrap_or_default(),
                        };
                        let event = EventVars {
                            nick: my_nick.to_string(),
                            chan: name.clone(),
                            target: name,
                            params,
                            ..Default::default()
                        };
                        let more = engine.dispatch_event(&ctx, "SIGNAL", event);
                        apply_actions_depth(
                            app, server_id, my_nick, network, server, more, depth + 1,
                        );
                    }
                }
            }
            Action::RunOn { server_id: target, command } => {
                // /scon /scid: run the command in the target connection's context
                // and route its output there. Depth-capped like /signal.
                if depth < 24 {
                    if let (Some(engine), Some(store)) = (
                        app.try_state::<ScriptEngine>(),
                        app.try_state::<crate::irc::state::StateStore>(),
                    ) {
                        let state = store.get(&target);
                        let t_nick = state.nick.clone();
                        let ctx = RunCtx {
                            my_nick: &t_nick,
                            network: "",
                            server: "",
                            data_dir: script_data_dir(app),
                            state,
                        };
                        let more = engine.run_command(&ctx, "", &command, &[]);
                        apply_actions_depth(app, &target, &t_nick, "", "", more, depth + 1);
                    }
                }
            }
            Action::WindowOpen { name, kind, title } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::WindowOpen {
                        server_id: server_id.to_string(),
                        name,
                        kind,
                        title,
                    },
                );
            }
            Action::WindowClose { name } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::WindowClose { server_id: server_id.to_string(), name },
                );
            }
            Action::WindowLine { name, op, n, text } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::WindowLine {
                        server_id: server_id.to_string(),
                        name,
                        op,
                        n,
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
            Action::SockListen { name } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.start_listener(
                        app.clone(),
                        server_id.to_string(),
                        network.to_string(),
                        my_nick.to_string(),
                        name,
                    );
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

/// A notify-list nick came online (`on NOTIFY`) or went offline (`on UNOTIFY`).
/// The frontend calls this from its ISON diff; `$nick` is the affected nick.
#[tauri::command]
pub fn script_notify(app: AppHandle, server_id: String, network: String, nick: String, online: bool) {
    let kind = if online { "NOTIFY" } else { "UNOTIFY" };
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|s| s.get(&server_id))
        .unwrap_or_default();
    let my_nick = state.nick.clone();
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: "",
        data_dir: script_data_dir(&app),
        state,
    };
    let vars = EventVars { nick: nick.clone(), target: nick, ..Default::default() };
    let actions = app.state::<ScriptEngine>().dispatch_event(&ctx, kind, vars);
    apply_actions(&app, &server_id, &my_nick, &network, "", actions);
}

/// Returns the names of all open script sockets (for `/socklist`).
#[tauri::command]
pub fn script_sockets(socks: State<'_, socket::SocketManager>) -> Vec<String> {
    socks.names()
}

/// Records the currently-focused window (`$active`) and its connection
/// (`$activecid`). The frontend calls this whenever the active buffer changes.
#[tauri::command]
pub fn script_set_active(engine: State<'_, ScriptEngine>, name: String, server_id: String) {
    engine.set_active(&name);
    engine.set_active_conn(&server_id);
    engine.set_active_win(&server_id, &name);
}

/// The UI opened a window/buffer — assign its `$wid` and fire `on OPEN`.
#[tauri::command]
pub fn script_window_open(app: AppHandle, server_id: String, name: String) {
    app.state::<ScriptEngine>().window_open(&server_id, &name);
    fire_window_event(&app, &server_id, &name, "OPEN");
}

/// The UI closed a window/buffer — release its `$wid` and fire `on CLOSE`.
#[tauri::command]
pub fn script_window_close(app: AppHandle, server_id: String, name: String) {
    app.state::<ScriptEngine>().window_close(&server_id, &name);
    fire_window_event(&app, &server_id, &name, "CLOSE");
}

/// Dispatches `on OPEN`/`on CLOSE` for a window. A plain nick is a query window
/// (empty `$chan` so the `?` target matches, `$nick` = the other party); a
/// channel / `@window` keeps its name as `$chan` for `#` / `@name` targets. The
/// status window is always present, so mIRC fires neither for it.
fn fire_window_event(app: &AppHandle, server_id: &str, name: &str, kind: &str) {
    if name.eq_ignore_ascii_case("Status Window") {
        return;
    }
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|s| s.get(server_id))
        .unwrap_or_default();
    let my_nick = state.nick.clone();
    let is_query = !is_channel(name) && !name.starts_with('@') && !name.starts_with('=');
    let vars = EventVars {
        nick: if is_query { name.to_string() } else { String::new() },
        chan: if is_query { String::new() } else { name.to_string() },
        target: name.to_string(),
        ..Default::default()
    };
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: "",
        server: "",
        data_dir: script_data_dir(app),
        state,
    };
    let actions = app.state::<ScriptEngine>().dispatch_event(&ctx, kind, vars);
    apply_actions(app, server_id, &my_nick, "", "", actions);
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
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    command: String,
    params: Vec<String>,
    snicks: Option<Vec<String>>,
) {
    // A popup command may call `$input`, which blocks the run waiting for the UI
    // dialog. Run it on a blocking thread so the main thread (and WebView2) stay
    // responsive — a sync command blocking the main thread deadlocks the webview.
    tauri::async_runtime::spawn_blocking(move || {
        let engine = app.state::<ScriptEngine>();
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
        // A nicklist popup carries the listbox selection ($snick/$snicks); other
        // contexts (channel/menubar) send none, so fall back to the item params.
        let snicks = snicks.unwrap_or_else(|| params.clone());
        let actions = engine.run_command_snicks(&ctx, &target, &command, &params, &snicks);
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    });
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
            let chan = if is_chan { target.clone() } else { String::new() };
            let reply = if is_chan { target.clone() } else { from.clone() };
            // CTCP framing: \x01COMMAND args\x01. ACTION surfaces as `on ACTION`;
            // any other CTCP (PING, VERSION, DCC, ...) as `on CTCP`, with
            // $1 = the command word.
            if let Some(ctcp) = text.strip_prefix('\u{1}') {
                let ctcp = ctcp.trim_end_matches('\u{1}');
                let (ckind, body) = if matches!(kind, crate::irc::event::MessageKind::Notice) {
                    // A CTCP reply (NOTICE \x01..\x01) → `on CTCPREPLY`.
                    ("CTCPREPLY", ctcp)
                } else {
                    // A CTCP request (PRIVMSG \x01..\x01): ACTION → `on ACTION`,
                    // anything else → `on CTCP`.
                    match ctcp.strip_prefix("ACTION ") {
                        Some(act) => ("ACTION", act),
                        None => ("CTCP", ctcp),
                    }
                };
                let vars = EventVars {
                    nick: from,
                    chan,
                    target: reply,
                    params: words(body),
                    text: body.to_string(),
                    ..Default::default()
                };
                return engine.dispatch_event(ctx, ckind, vars);
            }
            let kind = match kind {
                // A NOTICE with no nick prefix is a server notice → `on SNOTICE`.
                crate::irc::event::MessageKind::Notice if from.is_empty() => "SNOTICE",
                crate::irc::event::MessageKind::Notice => "NOTICE",
                _ => "TEXT",
            };
            let vars = EventVars {
                nick: from,
                chan,
                target: reply,
                params: words(text),
                text: text.clone(),
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
            if !is_channel(target) {
                // A user-mode change (only ever your own) fires `on USERMODE`.
                let vars = EventVars {
                    nick: setter,
                    target: target.clone(),
                    params: words(modes),
                    text: modes.clone(),
                    ..Default::default()
                };
                return engine.dispatch_event(ctx, "USERMODE", vars);
            }
            let chan = target.clone();
            // Generic `on MODE` and raw `on RAWMODE` ($1- = the whole change).
            let generic = EventVars {
                nick: setter.clone(),
                chan: chan.clone(),
                target: target.clone(),
                params: words(modes),
                text: modes.clone(),
                ..Default::default()
            };
            let mut actions = engine.dispatch_event(ctx, "MODE", generic.clone());
            actions.extend(engine.dispatch_event(ctx, "RAWMODE", generic));
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

/// Dispatches `on RAW` for one inbound server line. `command` is the
/// numeric/command word, `params` the line's parameters. `$numeric` is set when
/// the command is a numeric; `$1-` are the params; the matchtext matches the
/// command/numeric.
pub fn dispatch_raw(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    command: &str,
    params: Vec<String>,
) -> Vec<Action> {
    let numeric = if !command.is_empty() && command.bytes().all(|b| b.is_ascii_digit()) {
        command.to_string()
    } else {
        String::new()
    };
    let vars = EventVars {
        text: command.to_string(),
        params,
        numeric,
        ..Default::default()
    };
    engine.dispatch_event(ctx, "RAW", vars)
}

/// Dispatches a named protocol event fired straight off an inbound command —
/// `on WALLOPS` / `ERROR` / `PING` / `PONG` / `CONNECTFAIL`. `$nick` is the
/// source (empty for server-only commands), `$1-` the message text. WALLOPS is
/// a matchtext event (matches the text); the rest are plain.
pub fn dispatch_named(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    kind: &str,
    nick: &str,
    text: &str,
) -> Vec<Action> {
    let vars = EventVars {
        nick: nick.to_string(),
        params: words(text),
        text: text.to_string(),
        ..Default::default()
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
    fn regsubex_subtext_handles_structural_chars() {
        // A captured mSL-structural char ( ( ) [ ] $ % , & … ) must not corrupt
        // `$asc(\1)` — byte builders depend on it. "a(b]c" -> the asc of each
        // char, separators intact (a captured "(" used to make `$asc(()` and
        // drop/merge bytes, which corrupted GKSSP HMAC/gkid responses).
        let engine = ScriptEngine::new();
        engine.load(r#"alias t { var %x = a(b]c | /echo -a [ $+ $regsubex(%x,/(.)/g,$asc(\1) $+ $chr(32)) $+ ] }"#);
        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert_eq!(actions, vec![Action::Echo { target: "(status)".into(), text: "[97 40 98 93 99 ]".into() }]);
    }

    #[test]
    fn regsubex_keeps_unknown_escape_backslash() {
        // mIRC keeps an unrecognised escape literal: `\*` stays "\*" (used as a
        // wildcard to tell an escape sequence from a plain char). Input "a\0b" ->
        // 'a' and 'b' are plain (asc 97/98), only "\0" matches the "\*" wildcard.
        let engine = ScriptEngine::new();
        engine.load(r#"alias t { /echo -a [ $+ $regsubex(a\0b,/(\\?.)/g,$iif(\* iswm \1,ESC,$asc(\1)) $+ $chr(32)) $+ ] }"#);
        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert_eq!(actions, vec![Action::Echo { target: "(status)".into(), text: "[97 ESC 98 ]".into() }]);
    }

    #[test]
    fn input_returns_default_without_ui() {
        // With no UI backend installed (NoInput), $input returns its default (4th
        // arg) so a non-interactive/test run proceeds. The production backend
        // shows a dialog and blocks for the answer.
        let engine = ScriptEngine::new();
        engine.load("alias t { /echo -a [ $+ $input(msg,e,title,thedefault) $+ ] }");
        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert_eq!(actions, vec![Action::Echo { target: "(status)".into(), text: "[thedefault]".into() }]);
    }

    #[test]
    fn alias_sends_message() {
        let engine = ScriptEngine::new();
        engine.load("alias hi { /msg $chan hello $me }");
        let actions = engine.run_alias(&ctx(), "#test", "hi", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #test :hello me".into())]);
    }

    #[test]
    fn alias_param_ranges_and_require() {
        let engine = ScriptEngine::new();
        engine.load("alias t { /msg #d [$2-4] [$2-] [$3] [$1-] [$0] [$$2] }");
        let actions = engine.run_alias(&ctx(), "#here", "t", "a b c d e");
        assert_eq!(
            actions,
            vec![Action::Send(
                "PRIVMSG #d :[b c d] [b c d e] [c] [a b c d e] [5] [b]".into()
            )]
        );
    }

    #[test]
    fn bare_hash_resolves_to_current_channel() {
        let engine = ScriptEngine::new();
        engine.load("alias t { /msg # hello }");
        let actions = engine.run_alias(&ctx(), "#here", "t", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #here :hello".into())]);
    }

    #[test]
    fn require_param_halts_rest_when_missing() {
        let engine = ScriptEngine::new();
        // $$2 is empty -> the run halts before the second command. The first
        // still emits (the current command isn't suppressed mid-flight).
        engine.load("alias t { /msg #d got=$$2 | /msg #d after }");
        let actions = engine.run_alias(&ctx(), "#here", "t", "only");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #d :got=".into())]);
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
    fn braceless_one_liner_on_events() {
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:!ping:#:/msg $chan pong $nick\non *:TEXT:!hi:#:/msg $chan yo");
        let mk = |t: &str| EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: t.into(),
            params: words(t),
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", mk("!ping")),
            vec![Action::Send("PRIVMSG #c :pong bob".into())]
        );
        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", mk("!hi")),
            vec![Action::Send("PRIVMSG #c :yo".into())]
        );
    }

    #[test]
    fn script_groups_toggle_aliases() {
        let engine = ScriptEngine::new();
        engine.load(
            "#g off\nalias gg { msg #c hi }\n#g end\n\
             alias en { enable #g }\nalias dis { disable #g }",
        );
        // Declared `#g off` → the grouped alias is silent.
        assert_eq!(engine.run_alias(&ctx(), "#c", "gg", ""), vec![]);
        // /enable #g activates it.
        engine.run_alias(&ctx(), "#c", "en", "");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "gg", ""),
            vec![Action::Send("PRIVMSG #c :hi".into())]
        );
        // /disable #g silences it again.
        engine.run_alias(&ctx(), "#c", "dis", "");
        assert_eq!(engine.run_alias(&ctx(), "#c", "gg", ""), vec![]);
    }

    #[test]
    fn script_groups_suppress_events() {
        let engine = ScriptEngine::new();
        engine.load(
            "#g off\non *:TEXT:*:#:{ msg #c got }\n#g end\nalias en { enable #g }",
        );
        let ev = EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "hello".into(),
            params: vec!["hello".into()],
            ..Default::default()
        };
        // Group off → the handler is suppressed.
        assert_eq!(engine.dispatch_event(&ctx(), "TEXT", ev.clone()), vec![]);
        // Enable the group → it fires.
        engine.run_alias(&ctx(), "#c", "en", "");
        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", ev),
            vec![Action::Send("PRIVMSG #c :got".into())]
        );
    }

    #[test]
    fn group_identifier_reports_count_name_and_status() {
        let engine = ScriptEngine::new();
        engine.load(
            "#a on\nalias x { echo a }\n#a end\n#b off\nalias y { echo b }\n#b end\n\
             alias info { echo $group(0) $group(1) $group(#b).status }",
        );
        // $group(0) = 2 groups; $group(1) = #a; $group(#b).status = off.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "info", ""),
            vec![Action::Echo {
                target: "#c".into(),
                text: "2 #a off".into(),
            }]
        );
    }

    #[test]
    fn unsetall_clears_user_vars_but_keeps_group_state() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               set %a 1\n\
               set %b 2\n\
               unsetall\n\
               /msg #c a=[ $+ %a $+ ] b=[ $+ %b $+ ]\n\
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :a=[] b=[]".into())]
        );
        // A group override (a reserved NUL-prefixed key) survives /unsetall.
        let engine2 = ScriptEngine::new();
        engine2.load(
            "#g off\nalias gg { msg #c hi }\n#g end\n\
             alias en { enable #g }\nalias clr { unsetall }",
        );
        engine2.run_alias(&ctx(), "#c", "en", "");
        engine2.run_alias(&ctx(), "#c", "clr", "");
        assert_eq!(
            engine2.run_alias(&ctx(), "#c", "gg", ""),
            vec![Action::Send("PRIVMSG #c :hi".into())]
        );
    }

    #[test]
    fn identity_commands_emit_set_identity() {
        let engine = ScriptEngine::new();
        engine.load("alias setid { anick Backup | mnick Primary | fullname Real Name }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "setid", ""),
            vec![
                Action::SetIdentity {
                    field: "anick".into(),
                    value: "Backup".into(),
                },
                Action::SetIdentity {
                    field: "mnick".into(),
                    value: "Primary".into(),
                },
                Action::SetIdentity {
                    field: "fullname".into(),
                    value: "Real Name".into(),
                },
            ]
        );
    }

    #[test]
    fn alias_command_emits_define_then_remove() {
        let engine = ScriptEngine::new();
        // `/alias <name> <cmd>` defines (command stored unexpanded); `/alias <name>`
        // alone removes.
        engine.load("alias mk { alias greet /msg # hi $nick | alias greet }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "mk", ""),
            vec![
                Action::DefineAlias {
                    name: "greet".into(),
                    command: Some("/msg # hi $nick".into()),
                },
                Action::DefineAlias {
                    name: "greet".into(),
                    command: None,
                },
            ]
        );
    }

    #[test]
    fn signal_command_and_on_signal_event() {
        // /signal emits a Signal action (leading switches skipped, params -> $1-).
        let engine = ScriptEngine::new();
        engine.load("alias s { signal -n myevt hello world }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "s", ""),
            vec![Action::Signal {
                name: "myevt".into(),
                params: vec!["hello".into(), "world".into()],
            }]
        );
        // on SIGNAL matches the name (wildcard); $signal = name, $1- = params.
        let engine2 = ScriptEngine::new();
        engine2.load("on *:SIGNAL:my*:{ msg #c got $1 via $signal }");
        let ev = EventVars {
            chan: "myevt".into(),
            params: vec!["hi".into()],
            ..Default::default()
        };
        assert_eq!(
            engine2.dispatch_event(&ctx(), "SIGNAL", ev),
            vec![Action::Send("PRIVMSG #c :got hi via myevt".into())]
        );
    }

    #[test]
    fn autojoin_command_emits_control() {
        let engine = ScriptEngine::new();
        engine.load("alias a1 { autojoin -s }\nalias a2 { autojoin -d5 }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "a1", ""),
            vec![Action::Autojoin {
                skip: true,
                delay_secs: 0,
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "a2", ""),
            vec![Action::Autojoin {
                skip: false,
                delay_secs: 5,
            }]
        );
    }

    #[test]
    fn ctcp_event_fires_and_matches_command_or_full() {
        let engine = ScriptEngine::new();
        // PING matchtext must catch "PING <timestamp>"; VERSION is whole-text.
        engine.load("on *:CTCP:PING:?:/msg $nick pong\non *:CTCP:VERSION:?:/msg $nick jirc");
        let msg = |text: &str| UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "me".into(),
            text: text.into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &msg("\u{1}PING 99\u{1}")),
            vec![Action::Send("PRIVMSG bob :pong".into())]
        );
        assert_eq!(
            drive_event(&engine, &ctx(), &msg("\u{1}VERSION\u{1}")),
            vec![Action::Send("PRIVMSG bob :jirc".into())]
        );
        // A plain message must NOT fire the CTCP handlers.
        assert!(drive_event(&engine, &ctx(), &msg("hello PING")).is_empty());
    }

    #[test]
    fn ctcpreply_event_fires_on_notice_only() {
        let engine = ScriptEngine::new();
        // A NOTICE-wrapped CTCP fires `on CTCPREPLY`, never `on CTCP`.
        engine.load("on *:CTCPREPLY:PING*:?:/echo $nick replied $1-\non *:CTCP:PING:?:/echo req");
        let notice = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Notice,
            from: Some("bob".into()),
            target: "me".into(),
            text: "\u{1}PING 99\u{1}".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &notice),
            vec![Action::Echo {
                target: "bob".into(),
                text: "bob replied PING 99".into(),
            }]
        );
    }

    #[test]
    fn ctcp_command_sends_and_echoes() {
        let engine = ScriptEngine::new();
        // A script /ctcp sends the request and echoes `-> [nick] CMD` locally.
        engine.load("on *:TEXT:ping:#:/ctcp $nick version");
        let ev = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "#chan".into(),
            text: "ping".into(),
            time: None,
        };
        let actions = drive_event(&engine, &ctx(), &ev);
        assert!(actions.contains(&Action::Send("PRIVMSG bob :\u{1}VERSION\u{1}".into())));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Echo { text, .. } if text == "-> [bob] VERSION")));
    }

    #[test]
    fn protocol_named_events_fire() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:WALLOPS:*flood*:/echo w $nick $1-\non *:ERROR:*:/echo e $1-\non *:PING:/echo p\non *:CONNECTFAIL:/echo cf $1-",
        );
        // WALLOPS is a matchtext event — matches the text; $nick = sender.
        assert_eq!(
            dispatch_named(&engine, &ctx(), "WALLOPS", "oper", "net flood detected"),
            vec![Action::Echo { target: "(status)".into(), text: "w oper net flood detected".into() }]
        );
        // ERROR / PING / CONNECTFAIL are plain — they fire regardless; $1- = text.
        assert_eq!(
            dispatch_named(&engine, &ctx(), "ERROR", "", "Closing Link: spam"),
            vec![Action::Echo { target: "(status)".into(), text: "e Closing Link: spam".into() }]
        );
        assert_eq!(
            dispatch_named(&engine, &ctx(), "PING", "", "12345"),
            vec![Action::Echo { target: "(status)".into(), text: "p".into() }]
        );
        assert_eq!(
            dispatch_named(&engine, &ctx(), "CONNECTFAIL", "", "connection refused"),
            vec![Action::Echo { target: "(status)".into(), text: "cf connection refused".into() }]
        );
    }

    #[test]
    fn server_notice_fires_snotice_not_notice() {
        let engine = ScriptEngine::new();
        engine.load("on *:SNOTICE:*:/echo s $1-\non *:NOTICE:*:*:/echo n $1-");
        // A NOTICE with no nick prefix (server source) → on SNOTICE, not NOTICE.
        let ev = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Notice,
            from: None,
            target: "me".into(),
            text: "*** Looking up your hostname".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &ev),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "s *** Looking up your hostname".into()
            }]
        );
    }

    #[test]
    fn ialfill_sends_who_for_the_channel() {
        let engine = ScriptEngine::new();
        engine.load("alias f { /ialfill $1- }");
        // Bare channel, and with a leading network token — both WHO the channel.
        assert_eq!(engine.run_alias(&ctx(), "#x", "f", "#chan"), vec![Action::Send("WHO #chan".into())]);
        assert_eq!(
            engine.run_alias(&ctx(), "#x", "f", "libera #chan"),
            vec![Action::Send("WHO #chan".into())]
        );
    }

    #[test]
    fn raw_event_matches_and_exposes_numeric_event() {
        let engine = ScriptEngine::new();
        engine.load("on *:RAW:001:/echo got $numeric ev $event p1 $1-\non *:RAW:PING:/echo gotping");
        let welcome = dispatch_raw(&engine, &ctx(), "001", vec!["me".into(), "Welcome here".into()]);
        assert_eq!(
            welcome,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "got 001 ev raw p1 me Welcome here".into(),
            }]
        );
        let ping = dispatch_raw(&engine, &ctx(), "PING", vec!["12345".into()]);
        assert_eq!(
            ping,
            vec![Action::Echo { target: "(status)".into(), text: "gotping".into() }]
        );
        // A numeric matching neither handler fires nothing.
        assert!(dispatch_raw(&engine, &ctx(), "999", vec![]).is_empty());
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
    fn read_search_switches() {
        let dir = std::env::temp_dir().join(format!("jirc-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/write -c data.txt apple red", &[]);
        engine.run_command(&rctx, "#c", "/write data.txt banana yellow", &[]);
        engine.run_command(&rctx, "#c", "/write data.txt cherry red", &[]);
        engine.run_command(&rctx, "#c", "/write data.txt yesterday news", &[]);
        engine.run_command(&rctx, "#c", "/write data.txt yes sir", &[]);
        // w: first line matching a wildcard -> the whole line; $readn = line number.
        engine.load("alias t { /msg #c $read(data.txt, w, *yellow*) @ $readn }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :banana yellow @ 2".into())]
        );
        // s: line beginning with the text -> the remainder after it.
        engine.load("alias t2 { /msg #c $read(data.txt, s, cherry) @ $readn }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t2", ""),
            vec![Action::Send("PRIVMSG #c :red @ 3".into())]
        );
        // no match -> $readn is 0.
        engine.load("alias t3 { var %x $read(data.txt, w, *grape*) | /msg #c found=$readn }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t3", ""),
            vec![Action::Send("PRIVMSG #c :found=0".into())]
        );
        // s matches a whole token: `yes` skips "yesterday news" and hits "yes sir".
        engine.load("alias t4 { /msg #c $read(data.txt, s, yes) @ $readn }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t4", ""),
            vec![Action::Send("PRIVMSG #c :sir @ 5".into())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writeini_readini_roundtrips() {
        let dir = std::env::temp_dir().join(format!("jirc-ini-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/writeini cfg.ini User nick bob", &[]);
        engine.run_command(&rctx, "#c", "/writeini cfg.ini User host x.example", &[]);
        engine.load("alias r { /msg #c $readini(cfg.ini, User, nick) [ $+ $ini(cfg.ini, User, 0) $+ ] }");
        let actions = engine.run_alias(&rctx, "#c", "r", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :bob [2]".into())]);
        // /remini removes a single item; $readini of it is then empty.
        engine.run_command(&rctx, "#c", "/remini cfg.ini User host", &[]);
        engine.load("alias r2 { /msg #c [ $+ $readini(cfg.ini, User, host) $+ ] }");
        let actions = engine.run_alias(&rctx, "#c", "r2", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :[]".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_handle_io_round_trip() {
        let dir = std::env::temp_dir().join(format!("jirc-fio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        // Write two lines through a handle; the handle persists across the
        // separate run_command calls (it lives in the engine's global state).
        engine.run_command(&rctx, "#c", "/fopen -o w notes.txt", &[]);
        engine.run_command(&rctx, "#c", "/fwrite -n w alpha", &[]);
        engine.run_command(&rctx, "#c", "/fwrite -n w beta", &[]);
        engine.run_command(&rctx, "#c", "/fclose w", &[]);
        // Read them back via a fresh handle; $fread advances the pointer.
        engine.run_command(&rctx, "#c", "/fopen r notes.txt", &[]);
        engine.load("alias r { /msg #c $fread(r) $+ - $+ $fread(r) }");
        let actions = engine.run_alias(&rctx, "#c", "r", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :alpha-beta".into())]);
        engine.run_command(&rctx, "#c", "/fclose r", &[]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_commands_mkdir_copy_rename_remove() {
        let dir = std::env::temp_dir().join(format!("jirc-fc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/write a.txt hello", &[]);
        engine.run_command(&rctx, "#c", "/copy a.txt b.txt", &[]);
        engine.run_command(&rctx, "#c", "/rename b.txt c.txt", &[]);
        engine.run_command(&rctx, "#c", "/remove a.txt", &[]);
        assert!(!dir.join("a.txt").exists());
        assert!(!dir.join("b.txt").exists()); // renamed away
        assert!(dir.join("c.txt").is_file());
        engine.run_command(&rctx, "#c", "/mkdir sub", &[]);
        assert!(dir.join("sub").is_dir());
        engine.run_command(&rctx, "#c", "/rmdir sub", &[]);
        assert!(!dir.join("sub").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn findfile_counts_matches() {
        let dir = std::env::temp_dir().join(format!("jirc-ff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data/sub")).unwrap();
        std::fs::write(dir.join("data/a.txt"), "x").unwrap();
        std::fs::write(dir.join("data/b.txt"), "y").unwrap();
        std::fs::write(dir.join("data/sub/c.txt"), "z").unwrap();
        std::fs::write(dir.join("data/note.log"), "n").unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        // recursive: *.txt under data/ = a,b,sub/c = 3; dirs = sub = 1.
        engine.load("alias n { /msg #c files= $+ $findfile(data, *.txt, 0) dirs= $+ $finddir(data, *, 0) }");
        let actions = engine.run_alias(&rctx, "#c", "n", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :files=3 dirs=1".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binvar_bset_bvar_and_hash() {
        let engine = ScriptEngine::new();
        // Build "abc" (97 98 99) in &v, read it back, and hash the binvar (N=1).
        engine.load(
            "alias n { /bset &v 1 97 98 99 | /msg #c $bvar(&v,0) $+ / $+ $bvar(&v,1,3) $+ / $+ $bvar(&v).text $+ / $+ $sha256(&v,1) }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "n", "");
        assert_eq!(
            actions,
            vec![Action::Send(
                "PRIVMSG #c :3/97 98 99/abc/ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()
            )]
        );
    }

    #[test]
    fn awaytime_and_online() {
        use crate::irc::state::StateSnapshot;
        let engine = ScriptEngine::new();
        // Not connected / not away -> both empty.
        let r0 = RunCtx {
            my_nick: "me",
            network: "N",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(StateSnapshot::default()),
        };
        engine.load("alias t { /msg #c [ $+ $awaytime $+ ][ $+ $online $+ ] }");
        assert_eq!(
            engine.run_alias(&r0, "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :[][]".into())]
        );
        // away_time set -> $awaytime returns it verbatim.
        let r1 = RunCtx {
            my_nick: "me",
            network: "N",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(StateSnapshot { away_time: 1_700_000_500, ..Default::default() }),
        };
        engine.load("alias t { /msg #c $awaytime }");
        assert_eq!(
            engine.run_alias(&r1, "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :1700000500".into())]
        );
    }

    #[test]
    fn custom_window_lines() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias n { /window @list | /aline @list one | /aline @list two | /rline @list 1 ONE | /msg #c $window(@list).lines $+ / $+ $line(@list,1) $+ / $+ $line(@list,2) }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "n", "");
        // The window ops also emit WindowOpen/WindowLine actions; check the /msg.
        let sends: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(sends, vec!["PRIVMSG #c :2/ONE/two"]);
    }

    #[test]
    fn connection_identifiers_from_snapshot() {
        use crate::irc::state::StateSnapshot;
        let snap = StateSnapshot {
            nick: "me".into(),
            server_port: 6697,
            tls: true,
            alt_nick: "me_".into(),
            realname: "Real Name".into(),
            user_mode: "ix".into(),
            away: true,
            away_msg: "Gone fishing".into(),
            main_nick: "MainNick".into(),
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "irc.x",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "alias n { /msg #c $port $+ / $+ $ssl $+ / $+ $anick $+ / $+ $fullname $+ / $+ $usermode $+ / $+ $away $+ / $+ $awaymsg $+ / $+ $mnick }",
        );
        let actions = engine.run_alias(&rctx, "#c", "n", "");
        assert_eq!(
            actions,
            vec![Action::Send(
                "PRIVMSG #c :6697/$true/me_/Real Name/ix/$true/Gone fishing/MainNick".into()
            )]
        );
    }

    #[test]
    fn ialchan_filters_ial_by_channel() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            ial: vec![
                ("alice".into(), "alice!a@host1.com".into()),
                ("bob".into(), "bob!b@host2.com".into()),
                ("carol".into(), "carol!c@host1.com".into()), // host1, but not on #chan
            ],
            channels: vec![ChannelView {
                name: "#chan".into(),
                nicks: vec!["alice".into(), "bob".into()],
                members: vec![],
                bans: vec![],
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "irc.x",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        // host1 members on #chan = {alice}; all #chan members = {alice, bob}.
        engine.load("alias n { /msg #c $ialchan(*!*@host1.com,#chan,0) $+ / $+ $ialchan(*!*@*,#chan,0) }");
        let actions = engine.run_alias(&rctx, "#c", "n", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :1/2".into())]);
    }

    #[test]
    fn regsubex_evaluates_subtext_per_match() {
        // \2\1 swaps each match's two groups (markers only, no eval needed).
        let engine = ScriptEngine::new();
        engine.load("alias swap { /msg #c $regsubex(a1 b2,/(\\w)(\\d)/g,\\2\\1) }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "swap", ""),
            vec![Action::Send("PRIVMSG #c :1a 2b".into())]
        );
        // The subtext is also evaluated per match: $upper(\t) upper-cases each.
        let engine2 = ScriptEngine::new();
        engine2.load("alias up { /msg #c $regsubex(ab,/(\\w)/g,$upper(\\t)) }");
        assert_eq!(
            engine2.run_alias(&ctx(), "#c", "up", ""),
            vec![Action::Send("PRIVMSG #c :AB".into())]
        );
    }

    #[test]
    fn caller_and_isid() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias c { return $caller }\nalias i { return $isid }\nalias top { /msg #c $caller/$c | /msg #c $isid/$i }",
        );
        // `top` runs as a command; `$c`/`$i` are invoked as identifiers.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "top", ""),
            vec![
                Action::Send("PRIVMSG #c :command/identifier".into()),
                Action::Send("PRIVMSG #c :$false/$true".into()),
            ]
        );
    }

    #[test]
    fn numeric_connection_ids() {
        let engine = ScriptEngine::new();
        assert_eq!((engine.assign_cid("s1"), engine.assign_cid("s2")), (1, 2));
        assert_eq!(engine.assign_cid("s1"), 1); // idempotent — a reconnect keeps its number
        engine.set_active_conn("s2");

        // $scon(0) = count, $scon(N) = Nth cid, $activecid = the active connection.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c n=$scon(0) first=$scon(1) act=$activecid", &[]),
            vec![Action::Send("PRIVMSG #c :n=2 first=1 act=2".into())]
        );

        // $cid is the *run's own* connection, read from the state snapshot.
        let snap = crate::irc::state::StateSnapshot { server_id: "s2".into(), ..Default::default() };
        let ctx2 = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        assert_eq!(
            engine.run_command(&ctx2, "#c", "/msg #c cid=$cid", &[]),
            vec![Action::Send("PRIVMSG #c :cid=2".into())]
        );

        // Forgetting a connection drops it from $scon.
        engine.forget_cid("s1");
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c n=$scon(0)", &[]),
            vec![Action::Send("PRIVMSG #c :n=1".into())]
        );
    }

    #[test]
    fn scid_identifier() {
        let engine = ScriptEngine::new();
        engine.assign_cid("s1");
        engine.assign_cid("s2");
        engine.set_active_conn("s2");
        // $scid(0) = count, $scid(-1) = active cid, $scid(cid) = echo if it exists.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c c=$scid(0) a=$scid(-1) v=$scid(2) x=$scid(9)", &[]),
            vec![Action::Send("PRIVMSG #c :c=2 a=2 v=2 x=".into())]
        );
    }

    #[test]
    fn window_ids() {
        let engine = ScriptEngine::new();
        // The UI opens windows; each gets a stable wid. Same (server,name) is idempotent.
        assert_eq!(engine.window_open("s1", "#a"), 1);
        assert_eq!(engine.window_open("s1", "#b"), 2);
        assert_eq!(engine.window_open("s1", "#a"), 1);
        engine.set_active_win("s1", "#b");

        // $activewid = the active window; $wid (in an event for #a) = that window.
        engine.load("on *:TEXT:*:#:{ /msg $chan wid=$wid active=$activewid }");
        let snap = crate::irc::state::StateSnapshot { server_id: "s1".into(), ..Default::default() };
        let ctx2 = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let ev = UiEvent::Message {
            server_id: "s1".into(),
            kind: crate::irc::event::MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "#a".into(),
            text: "hi".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx2, &ev),
            vec![Action::Send("PRIVMSG #a :wid=1 active=2".into())]
        );

        // Closing #a drops its wid.
        engine.window_close("s1", "#a");
        assert_eq!(engine.window_open("s1", "#c"), 3); // new window, not reusing 1
    }

    #[test]
    fn scon_scid_dispatch() {
        let engine = ScriptEngine::new();
        engine.assign_cid("s1");
        engine.assign_cid("s2");
        // /scon N targets the Nth connection; the subcommand is carried raw to it.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/scon 2 /msg #c hi", &[]),
            vec![Action::RunOn { server_id: "s2".into(), command: "/msg #c hi".into() }]
        );
        // /scid targets by cid.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/scid 1 /msg #c yo", &[]),
            vec![Action::RunOn { server_id: "s1".into(), command: "/msg #c yo".into() }]
        );
        // An out-of-range selector produces nothing.
        assert_eq!(engine.run_command(&ctx(), "#c", "/scon 9 /msg #c x", &[]), vec![]);
    }

    #[test]
    fn lifecycle_events_dispatch() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:START:{ /echo -s started }\n\
             on *:UNLOAD:{ /echo -s unloading }\n\
             on *:EXIT:{ /echo -s exiting }",
        );
        let echoed = |acts: Vec<Action>, want: &str| {
            acts.iter().any(|a| matches!(a, Action::Echo { text, .. } if text == want))
        };
        assert!(echoed(engine.dispatch_event(&ctx(), "START", EventVars::default()), "started"));
        assert!(echoed(engine.dispatch_event(&ctx(), "UNLOAD", EventVars::default()), "unloading"));
        assert!(echoed(engine.dispatch_event(&ctx(), "EXIT", EventVars::default()), "exiting"));
        // A script with no lifecycle handlers dispatches to nothing.
        let bare = ScriptEngine::new();
        bare.load("alias x { /echo hi }");
        assert!(bare.dispatch_event(&ctx(), "START", EventVars::default()).is_empty());
    }

    #[test]
    fn open_close_window_events() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:OPEN:?:*:{ /echo -s opened query $target }\n\
             on *:CLOSE:?:{ /echo -s closed query $target }\n\
             on *:OPEN:#:*:{ /echo -s opened chan $chan }",
        );
        let echoed = |acts: Vec<Action>, want: &str| {
            acts.iter().any(|a| matches!(a, Action::Echo { text, .. } if text == want))
        };
        // A query window (empty $chan so `?` matches; $target = the other party).
        let q = EventVars { nick: "bob".into(), target: "bob".into(), ..Default::default() };
        assert!(echoed(engine.dispatch_event(&ctx(), "OPEN", q.clone()), "opened query bob"));
        // A channel window: `#` matches, `?` does not.
        let c = EventVars { chan: "#c".into(), target: "#c".into(), ..Default::default() };
        let ca = engine.dispatch_event(&ctx(), "OPEN", c);
        assert!(echoed(ca.clone(), "opened chan #c"));
        assert!(!echoed(ca, "opened query #c"));
        // on CLOSE:? fires when a query closes.
        assert!(echoed(engine.dispatch_event(&ctx(), "CLOSE", q), "closed query bob"));
    }

    #[test]
    fn notify_events() {
        let engine = ScriptEngine::new();
        // Plain events (no target/matchtext); $nick is the friend who changed state.
        engine.load(
            "on *:NOTIFY:/msg #f $nick is online\n\
             on *:UNOTIFY:/msg #f $nick left",
        );
        let vars = EventVars { nick: "alice".into(), target: "alice".into(), ..Default::default() };
        assert_eq!(
            engine.dispatch_event(&ctx(), "NOTIFY", vars.clone()),
            vec![Action::Send("PRIVMSG #f :alice is online".into())]
        );
        assert_eq!(
            engine.dispatch_event(&ctx(), "UNOTIFY", vars),
            vec![Action::Send("PRIVMSG #f :alice left".into())]
        );
    }

    #[test]
    fn iif_supports_state_operators() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["alice".into(), "bob".into()],
                members: vec![("bob".into(), "@".into()), ("alice".into(), "".into())],
                bans: vec![],
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "irc.x",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        // $iif's condition is now evaluated like `if`, so isop/ison/… work.
        engine.load("alias t { /msg #c $iif(bob isop #c,op,notop) $iif(alice isop #c,op,notop) }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :op notop".into())]
        );
    }

    #[test]
    fn var_set_math() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               var %a 1 + 2\n\
               var %b 2 ^ 16\n\
               var %c 7 % 3\n\
               var %d 1 + 1 + 1\n\
               var -n %e 9 - 4\n\
               var %f a + b\n\
               set %g 3 * 4\n\
               /msg #c %a/%b/%c/%d/%e/%f/%g\n\
             }",
        );
        // +, ^, % compute; `1 + 1 + 1` (not 3 tokens), `-n`, and non-numeric stay
        // literal; /set does math too. Also exercises the no-`=` /var form.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :3/65536/1/1 + 1 + 1/9 - 4/a + b/12".into())]
        );
    }

    #[test]
    fn returnex_is_return_synonym() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias val { returnex hello world }\n\
             alias t { /msg #c $val }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :hello world".into())]
        );
    }

    #[test]
    fn show_and_result() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias inner { return payload }\n\
             alias verbose { /msg #c show=$show }\n\
             alias t {\n\
               verbose\n\
               .verbose\n\
               inner\n\
               /msg #c result=$result\n\
             }",
        );
        // `verbose` (no dot) -> $show true; `.verbose` -> $show false; after the
        // `inner` command, $result holds its /return value.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![
                Action::Send("PRIVMSG #c :show=$true".into()),
                Action::Send("PRIVMSG #c :show=$false".into()),
                Action::Send("PRIVMSG #c :result=payload".into()),
            ]
        );
    }

    #[test]
    fn v1_v2_and_lazy_iif() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t1 { /msg #c $iif(hello,$v1,none) }\n\
             alias t2 { /msg #c $iif($null,$v1,none) }\n\
             alias t3 { /msg #c $iif(3 == 3,$v1-$v2,no) }\n\
             alias t4 { if (foo isin foobar) { /msg #c $v1 in $v2 } }",
        );
        // The classic idiom: $iif(value, $v1, default) yields the value when truthy…
        assert_eq!(engine.run_alias(&ctx(), "#c", "t1", ""), vec![Action::Send("PRIVMSG #c :hello".into())]);
        // …and the default when the value is empty.
        assert_eq!(engine.run_alias(&ctx(), "#c", "t2", ""), vec![Action::Send("PRIVMSG #c :none".into())]);
        // A comparison sets both operands.
        assert_eq!(engine.run_alias(&ctx(), "#c", "t3", ""), vec![Action::Send("PRIVMSG #c :3-3".into())]);
        // $v1/$v2 also come from an `if` comparison (here a binary word operator).
        assert_eq!(engine.run_alias(&ctx(), "#c", "t4", ""), vec![Action::Send("PRIVMSG #c :foo in foobar".into())]);
    }

    #[test]
    fn active_window_identifier() {
        // $active reflects the focused window the UI last reported.
        let engine = ScriptEngine::new();
        engine.set_active("#focused");
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c here=$active", &[]),
            vec![Action::Send("PRIVMSG #c :here=#focused".into())]
        );
        // Also visible inside an event handler, not just typed commands.
        engine.load("on *:TEXT:*:#:{ /msg $chan active=$active }");
        engine.set_active("#lobby");
        let ev = UiEvent::Message {
            server_id: "s".into(),
            kind: crate::irc::event::MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "#c".into(),
            text: "hi".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &ev),
            vec![Action::Send("PRIVMSG #c :active=#lobby".into())]
        );
        // $null (empty) until the UI reports one.
        let fresh = ScriptEngine::new();
        assert_eq!(
            fresh.run_command(&ctx(), "#c", "/msg #c here=$active", &[]),
            vec![Action::Send("PRIVMSG #c :here=".into())]
        );
    }

    #[test]
    fn snick_snicks_threaded_through_popup_run() {
        let engine = ScriptEngine::new();
        let sel = ["alice".to_string(), "bob".to_string(), "carol".to_string()];
        // $snicks -> comma-separated selection.
        assert_eq!(
            engine.run_command_snicks(&ctx(), "#c", "/msg #c $snicks", &["alice".into()], &sel),
            vec![Action::Send("PRIVMSG #c :alice,bob,carol".into())]
        );
        // $snick(#,0) -> count; $snick(#,N) -> Nth selected.
        assert_eq!(
            engine.run_command_snicks(
                &ctx(),
                "#c",
                "/msg #c $snick(#c,0) $snick(#c,2)",
                &["alice".into()],
                &sel
            ),
            vec![Action::Send("PRIVMSG #c :3 bob".into())]
        );
        // A plain run (no popup selection, e.g. a timer) leaves the selection empty.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c count=$snick(#c,0)", &[]),
            vec![Action::Send("PRIVMSG #c :count=0".into())]
        );
    }

    #[test]
    fn popup_style_marks_checked_and_disabled() {
        let engine = ScriptEngine::new();
        engine.load(
            "menu nicklist {\n\
             $style(2) Disabled:noop\n\
             $iif(1 == 1,$style(1)) Checked:noop\n\
             $iif(1 == 2,$style(2)) Normal:noop\n\
             }",
        );
        let items = engine.popups_evaluated(&ctx(), "nicklist", "bob", "#c");
        assert_eq!(items.len(), 3);
        // $style(2): greyed, label stripped of the marker.
        assert_eq!(items[0].label, "Disabled");
        assert!(items[0].disabled && !items[0].checked);
        // $iif(...,$style(1)): the true branch checks the item.
        assert_eq!(items[1].label, "Checked");
        assert!(items[1].checked && !items[1].disabled);
        // $iif false branch yields no marker -> a plain item.
        assert_eq!(items[2].label, "Normal");
        assert!(!items[2].checked && !items[2].disabled);
    }

    #[test]
    fn popup_submenu_expands_dynamic_items() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias animal {\n\
               if ($1 == begin) return -\n\
               if ($1 == 1) return Cow:echo Cow\n\
               if ($1 == 2) return Llama:echo Llama\n\
               if ($1 == end) return -\n\
             }\n\
             menu nicklist {\n\
               Animal\n\
               .$submenu($animal($1))\n\
             }",
        );
        let items = engine.popups_evaluated(&ctx(), "nicklist", "bob", "#c");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Animal");
        // begin '-' → sep, then Cow, Llama (iteration stops at the empty $animal(3)),
        // then end '-' → sep.
        let kids = &items[0].children;
        assert_eq!(kids.len(), 4);
        assert!(kids[0].separator);
        assert_eq!(kids[1].label, "Cow");
        assert_eq!(kids[1].command, "echo Cow");
        assert_eq!(kids[2].label, "Llama");
        assert_eq!(kids[2].command, "echo Llama");
        assert!(kids[3].separator);
    }

    #[test]
    fn submenu_arg_parse() {
        use super::parse_submenu_arg;
        assert_eq!(parse_submenu_arg("$submenu($animal($1))").as_deref(), Some("$animal($1)"));
        assert_eq!(parse_submenu_arg("  $SubMenu($x($1)) ").as_deref(), Some("$x($1)"));
        assert_eq!(parse_submenu_arg("Plain:cmd"), None);
    }

    #[test]
    fn style_marker_split() {
        use super::split_style_marker;
        let m = crate::script::eval::STYLE_MARK;
        assert_eq!(split_style_marker(&format!("{m}3 Both")), (true, true, " Both"));
        assert_eq!(split_style_marker(&format!("  {m}2 Off")), (false, true, " Off"));
        assert_eq!(split_style_marker("Plain"), (false, false, "Plain"));
        // A bare marker (no digit) is dropped, no style applied.
        assert_eq!(split_style_marker(&format!("{m} x")), (false, false, " x"));
    }

    #[test]
    fn break_and_continue() {
        // /break exits the loop: msgs 1, 2 then breaks at 3.
        let engine = ScriptEngine::new();
        engine.load("alias b { set %i 0 | while (%i < 5) { inc %i | if (%i == 3) break | msg #c %i } }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "b", ""),
            vec![
                Action::Send("PRIVMSG #c :1".into()),
                Action::Send("PRIVMSG #c :2".into()),
            ]
        );
        // /continue skips the first two iterations: msgs 3, 4, 5.
        let engine2 = ScriptEngine::new();
        engine2.load("alias c { set %i 0 | while (%i < 5) { inc %i | if (%i < 3) continue | msg #c %i } }");
        assert_eq!(
            engine2.run_alias(&ctx(), "#c", "c", ""),
            vec![
                Action::Send("PRIVMSG #c :3".into()),
                Action::Send("PRIVMSG #c :4".into()),
                Action::Send("PRIVMSG #c :5".into()),
            ]
        );
    }

    #[test]
    fn binary_var_commands() {
        // /breplace replaces matching byte values (2 -> 9).
        let engine = ScriptEngine::new();
        engine.load("alias t { bset &v 1 1 2 3 2 1 | breplace &v 2 9 | msg #c $bvar(&v,1,5) }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :1 9 3 9 1".into())]
        );
        // /bcopy copies M bytes from one binvar to another.
        let engine2 = ScriptEngine::new();
        engine2.load("alias t { bset &v 1 10 20 30 | bcopy &w 1 &v 2 2 | msg #c $bvar(&w,1,2) }");
        assert_eq!(
            engine2.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :20 30".into())]
        );
        // /bwrite + /bread roundtrip through the sandbox.
        let engine3 = ScriptEngine::new();
        engine3.load("alias t { bset &v 1 65 66 67 | bwrite jirc_bin_rt.bin 1 -1 &v | bread jirc_bin_rt.bin 1 3 &w | msg #c $bvar(&w,1,3) }");
        assert_eq!(
            engine3.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :65 66 67".into())]
        );
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
    fn protocol_commands_emit_raw_lines() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias go { /kick #c bob being rude | /away gone fishing | /hop #c | /nickserv identify pw | /omsg #c ops only | /ctcpreply bob ping 123 }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "go", "");
        assert_eq!(
            actions,
            vec![
                Action::Send("KICK #c bob :being rude".into()),
                Action::Send("AWAY :gone fishing".into()),
                Action::Send("PART #c".into()),
                Action::Send("JOIN #c".into()),
                Action::Send("PRIVMSG NickServ :identify pw".into()),
                Action::Send("PRIVMSG @#c :ops only".into()),
                Action::Send("NOTICE bob :\u{1}PING 123\u{1}".into()),
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
    fn sockwrite_sends_binvar_bytes() {
        let engine = ScriptEngine::new();
        // `/sockwrite name &v` must emit the binary variable's raw bytes, not the
        // literal text "&v" (binary protocols build their packet in a &binvar).
        engine.load("alias t { bset &v 1 72 105 33 | sockwrite sk &v }");
        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert_eq!(
            actions,
            vec![Action::SockWrite { name: "sk".into(), data: vec![72, 105, 33] }]
        );
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
            isupport: Default::default(),
            channels: vec![],
            ial: vec![
                ("bob".into(), "bob!~bob@host.example.com".into()),
                ("alice".into(), "alice!ali@other.net".into()),
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
            isupport: Default::default(),
            channels: vec![],
            ial: vec![("bob".into(), "bob!~bob@host.example.com".into())],
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
            isupport: Default::default(),
            channels: vec![ChannelView {
                name: "#a".into(),
                nicks: vec!["op".into(), "voiced".into(), "plain".into()],
                members: vec![
                    ("op".into(), "@".into()),
                    ("voiced".into(), "+".into()),
                    ("plain".into(), String::new()),
                ],
                ..Default::default()
            }],
            ial: vec![],
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
    fn isban_checks_channel_ban_list() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            isupport: Default::default(),
            channels: vec![ChannelView {
                name: "#a".into(),
                bans: vec!["*!*@evil.example".into(), "baddie!*@*".into()],
                ..Default::default()
            }],
            ial: vec![],
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
            "on *:TEXT:*:#:{
              if (nick!user@evil.example isban #a) { /echo masked }
              if (baddie!x@y isban #a) { /echo baddie }
              if (good!user@host isban #a) { /echo should-not }
            }",
        );
        let vars = EventVars {
            nick: "x".into(),
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
        assert_eq!(echoed, vec!["masked", "baddie"]);
    }

    #[test]
    fn no_space_and_mixed_if_conditions() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:TEXT:*:#:{
              if ($1==hi) { /echo eq }
              if ($1!=bye) { /echo ne }
              if ($2==5) && $1==hi { /echo mixed }
              if ($2>3) { /echo gt }
              if ($1==nope) { /echo should-not }
            }",
        );
        let vars = EventVars {
            nick: "b".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "hi 5".into(),
            params: vec!["hi".into(), "5".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "TEXT", vars);
        let echoed: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Echo { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(echoed, vec!["eq", "ne", "mixed", "gt"]);
    }

    #[test]
    fn hget_property_iteration() {
        let dir = std::env::temp_dir().join(format!("jirc-hprop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir,
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        // .item / .data iterate the table in sorted-key order; $hget(h,0).item is
        // the count. Exercises the `.property` suffix parser end-to-end.
        engine.load(
            "alias t { hmake h | hadd h apple red | hadd h banana yellow | /echo n=$hget(h,0).item i1=$hget(h,1).item d1=$hget(h,1).data i2=$hget(h,2).item }",
        );
        let actions = engine.run_alias(&rctx, "#c", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "#c".into(),
                text: "n=2 i1=apple d1=red i2=banana".into(),
            }]
        );
    }

    #[derive(Default)]
    struct FakeSockets {
        listened: std::sync::Mutex<Vec<(String, u16)>>,
        accepted: std::sync::Mutex<Vec<String>>,
        marks: std::sync::Mutex<HashMap<String, String>>,
        ports: std::sync::Mutex<HashMap<String, u16>>,
    }
    impl ScriptSockets for FakeSockets {
        fn listen(&self, name: &str, port: u16) -> Option<u16> {
            let p = if port == 0 { 54321 } else { port };
            self.ports.lock().unwrap().insert(name.into(), p);
            self.listened.lock().unwrap().push((name.into(), port));
            Some(p)
        }
        fn accept(&self, name: &str, _listener: &str) -> bool {
            self.accepted.lock().unwrap().push(name.into());
            true
        }
        fn set_mark(&self, name: &str, mark: &str) {
            self.marks.lock().unwrap().insert(name.into(), mark.into());
        }
        fn rename(&self, _: &str, _: &str) {}
        fn pause(&self, _: &str, _: bool) {}
        fn exists(&self, name: &str) -> bool {
            self.ports.lock().unwrap().contains_key(name)
                || self.marks.lock().unwrap().contains_key(name)
        }
        fn prop(&self, name: &str, property: &str) -> String {
            match property {
                "port" => {
                    self.ports.lock().unwrap().get(name).map(|p| p.to_string()).unwrap_or_default()
                }
                "mark" => self.marks.lock().unwrap().get(name).cloned().unwrap_or_default(),
                "status" => {
                    if self.ports.lock().unwrap().contains_key(name) {
                        "listening".into()
                    } else {
                        String::new()
                    }
                }
                _ => String::new(),
            }
        }
        fn list(&self, _: &str) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn socklisten_and_sock_properties() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake.clone());
        // /socklisten binds (port readable inline, like mIRC); /sockmark stores a
        // mark; $sock(name) is the existence check.
        engine.load(
            "alias t { socklisten relay | sockmark relay hi there | sockaccept conn | /echo port=$sock(relay).port mark=$sock(relay).mark st=$sock(relay).status ex=$sock(relay) no=$sock(nope) }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        let echoed: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Echo { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(echoed, vec!["port=54321 mark=hi there st=listening ex=relay no="]);
        // /socklisten binds (recorded) and queues the accept-loop start.
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::SockListen { name } if name == "relay")));
        assert_eq!(*fake.listened.lock().unwrap(), vec![("relay".to_string(), 0u16)]);
        assert_eq!(*fake.accepted.lock().unwrap(), vec!["conn".to_string()]);
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
        // (`/window` is now a real command — covered by custom_window_lines.)
        let engine = ScriptEngine::new();
        engine.load("alias t { clear | beep 1 100 | /msg #c done }");
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
            "on *:OP:#:{ /msg $chan $nick opped $opnick }\n\
             on *:BAN:#:{ /msg $chan banned bnick=$bnick mask=$banmask }",
        );
        let ev = UiEvent::Mode {
            server_id: "s".into(),
            target: "#c".into(),
            modes: "+o bob +b m!*@* +b *!*@evil.host".into(),
            by: Some("op".into()),
        };
        let actions = drive_event(&engine, &ctx(), &ev);
        assert_eq!(
            actions,
            vec![
                Action::Send("PRIVMSG #c :op opped bob".into()),
                // $bnick = the mask's nick part; $banmask = the whole mask.
                Action::Send("PRIVMSG #c :banned bnick=m mask=m!*@*".into()),
                // A nickless mask (*!*@host): $bnick is $null (empty), mask intact.
                Action::Send("PRIVMSG #c :banned bnick= mask=*!*@evil.host".into()),
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
    fn rawmode_and_usermode_events() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:RAWMODE:#:{ /msg $chan raw $1- }\non *:USERMODE:{ /msg me umode $1- }",
        );
        // A channel mode fires on RAWMODE (and on MODE, no handler here).
        let ch = UiEvent::Mode {
            server_id: "s".into(),
            target: "#c".into(),
            modes: "+nt".into(),
            by: Some("op".into()),
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &ch),
            vec![Action::Send("PRIVMSG #c :raw +nt".into())]
        );
        // A user mode (non-channel target) fires on USERMODE.
        let um = UiEvent::Mode {
            server_id: "s".into(),
            target: "me".into(),
            modes: "+ix".into(),
            by: Some("me".into()),
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &um),
            vec![Action::Send("PRIVMSG me :umode +ix".into())]
        );
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
