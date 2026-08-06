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
pub mod hash;
pub mod ident;
pub mod ini;
pub mod input;
pub mod parser;
pub mod play;
pub mod socket;
pub mod timer;
pub mod users;
pub mod webview;
pub mod window;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use ast::{PopupItem, Script};
use eval::{
    wildcard_match, Action, EventVars, NoDcc, NoInput, NoPlay, NoSockets, NoTimers, NoWebviews,
    Runtime, ScriptDcc, ScriptInput, ScriptPlay, ScriptSockets, ScriptTimers, ScriptWebviews,
};

static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);

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

/// Raw server-line metadata shared by every script event derived from one IRC
/// message. IRCv3 tags are removed from `raw_msg` but retained in `msg_tags`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawEventContext {
    pub raw_msg: String,
    pub raw_bytes: Vec<u8>,
    pub msg_tags: Vec<(String, String, bool)>,
    pub msg_tags_raw: String,
    pub msg_stamp: String,
}

/// Result of the pre-protocol `on PARSELINE` pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseLineOutcome {
    /// Last non-queued replacement for the current direction, if supplied.
    pub current: Option<Vec<u8>>,
    /// Current replacement requested `-n`; outgoing processing should ensure a
    /// CRLF even when the queued line being replaced did not carry one.
    pub force_crlf: bool,
    /// `/parseline -q` actions, applied after the handler exits.
    pub queued: Vec<Action>,
    /// Ordinary actions emitted by the handler.
    pub actions: Vec<Action>,
}

struct Inner {
    script: Script,
    vars: HashMap<String, String>,
    hashes: HashMap<String, HashMap<String, String>>,
    var_expiry: HashMap<String, eval::TimedExpiry>,
    hash_expiry: HashMap<(String, String), eval::TimedExpiry>,
    files: files::FileStore,
    bins: binvar::BinStore,
    windows: window::WindowStore,
    users: users::UserList,
    sockets: std::sync::Arc<dyn ScriptSockets>,
    timers: std::sync::Arc<dyn ScriptTimers>,
    play: std::sync::Arc<dyn ScriptPlay>,
    dcc: std::sync::Arc<dyn ScriptDcc>,
    webviews: std::sync::Arc<dyn ScriptWebviews>,
    input: std::sync::Arc<dyn ScriptInput>,
    /// The frontend's currently-focused window/buffer name, for `$active`.
    active: String,
    /// Numeric connection-id registry for `$cid`/`$scon`/`$activecid`.
    conns: ConnReg,
    /// Numeric window-id registry for `$wid`/`$activewid`.
    wins: WinReg,
    /// Native windows that currently report focus, for `$appactive`.
    focused_windows: std::collections::HashSet<String>,
}

impl Inner {
    fn empty() -> Self {
        Inner {
            script: Script::default(),
            vars: HashMap::new(),
            hashes: HashMap::new(),
            var_expiry: HashMap::new(),
            hash_expiry: HashMap::new(),
            files: files::FileStore::default(),
            bins: binvar::BinStore::default(),
            windows: window::WindowStore::default(),
            users: users::UserList::default(),
            sockets: std::sync::Arc::new(NoSockets),
            timers: std::sync::Arc::new(NoTimers),
            play: std::sync::Arc::new(NoPlay),
            dcc: std::sync::Arc::new(NoDcc),
            webviews: std::sync::Arc::new(NoWebviews),
            input: std::sync::Arc::new(NoInput),
            active: String::new(),
            conns: ConnReg::default(),
            wins: WinReg::default(),
            focused_windows: std::collections::HashSet::new(),
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
    /// Profile context used when a dynamic timer follows another connection.
    contexts: HashMap<String, (String, String)>,
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
        self.contexts.remove(server_id);
        if self.active == server_id {
            self.active.clear();
        }
    }

    fn cid_for(&self, server_id: &str) -> u32 {
        self.entries
            .iter()
            .find(|(_, id)| id == server_id)
            .map(|(cid, _)| *cid)
            .unwrap_or(0)
    }

    fn set_context(&mut self, server_id: &str, network: &str, server: &str) {
        self.contexts.insert(
            server_id.to_string(),
            (network.to_string(), server.to_string()),
        );
    }

    fn view(&self) -> crate::script::eval::ConnsView {
        let active_cid = self
            .entries
            .iter()
            .find(|(_, id)| *id == self.active)
            .map(|(c, _)| *c)
            .unwrap_or(0);
        crate::script::eval::ConnsView {
            entries: self.entries.clone(),
            active_cid,
        }
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
    last_active_wid: u32,
    last_closed: Option<(u32, String, String)>,
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
        self.entries
            .push((self.next, server_id.to_string(), name.to_string()));
        self.next
    }

    fn close(&mut self, server_id: &str, name: &str) {
        let removed = self
            .entries
            .iter()
            .find(|(_, s, n)| s == server_id && n.eq_ignore_ascii_case(name))
            .cloned();
        let removed_wid = removed.as_ref().map(|(wid, _, _)| *wid);
        if removed.is_some() {
            self.last_closed = removed;
        }
        self.entries
            .retain(|(_, s, n)| !(s == server_id && n.eq_ignore_ascii_case(name)));
        if removed_wid == Some(self.active_wid) {
            self.active_wid = 0;
        }
        if removed_wid == Some(self.last_active_wid) {
            self.last_active_wid = 0;
        }
    }

    fn set_active(&mut self, server_id: &str, name: &str) {
        let next = self
            .entries
            .iter()
            .find(|(_, s, n)| s == server_id && n.eq_ignore_ascii_case(name))
            .map(|(w, _, _)| *w)
            .unwrap_or(0);
        if next != self.active_wid {
            if self.active_wid != 0 {
                self.last_active_wid = self.active_wid;
            }
            self.active_wid = next;
        }
    }

    fn view(&self) -> crate::script::eval::WinView {
        crate::script::eval::WinView {
            entries: self.entries.clone(),
            active_wid: self.active_wid,
            last_active_wid: self.last_active_wid,
            last_closed: self.last_closed.clone(),
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
    /// Records editbox activity for `$idle` and `/resetidle`.
    pub fn reset_idle(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        self.inner
            .lock()
            .unwrap()
            .vars
            .insert(eval::CLIENT_IDLE_SINCE_KEY.into(), now.to_string());
    }

    pub fn new() -> Self {
        ScriptEngine {
            inner: Mutex::new(Inner::empty()),
        }
    }

    /// Installs the (production) socket backend; called once at startup so the
    /// engine can run `/socklisten`/`/sockaccept`/`$sock(...)` against real sockets.
    pub fn set_timers(&self, timers: std::sync::Arc<dyn ScriptTimers>) {
        self.inner.lock().unwrap().timers = timers;
    }

    pub fn set_play(&self, play: std::sync::Arc<dyn ScriptPlay>) {
        self.inner.lock().unwrap().play = play;
    }

    pub fn set_dcc(&self, dcc: std::sync::Arc<dyn ScriptDcc>) {
        self.inner.lock().unwrap().dcc = dcc;
    }

    pub fn set_webviews(&self, webviews: std::sync::Arc<dyn ScriptWebviews>) {
        self.inner.lock().unwrap().webviews = webviews;
    }

    pub fn set_sockets(&self, sockets: std::sync::Arc<dyn ScriptSockets>) {
        self.inner.lock().unwrap().sockets = sockets;
    }

    /// Installs the (production) `$input` prompt backend; called once at startup.
    pub fn set_input(&self, input: std::sync::Arc<dyn ScriptInput>) {
        self.inner.lock().unwrap().input = input;
    }

    /// Records the frontend's currently-focused window/buffer name (for `$active`).
    pub fn set_active(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.active.eq_ignore_ascii_case(name) {
            return false;
        }
        inner.active = name.to_string();
        true
    }

    /// Assigns (idempotently) the numeric `$cid` for a connection; returns it.
    pub fn assign_cid(&self, server_id: &str) -> u32 {
        self.inner.lock().unwrap().conns.assign(server_id)
    }

    /// Drops a connection's `$cid` entry (on disconnect).
    pub fn forget_cid(&self, server_id: &str) {
        self.inner.lock().unwrap().conns.forget(server_id);
    }

    /// Returns the numeric mIRC-style connection id for a server id.
    pub fn cid_for(&self, server_id: &str) -> u32 {
        self.inner.lock().unwrap().conns.cid_for(server_id)
    }

    /// Records the profile context timers need when `-i` changes connection.
    pub fn set_connection_context(&self, server_id: &str, network: &str, server: &str) {
        self.inner
            .lock()
            .unwrap()
            .conns
            .set_context(server_id, network, server);
    }

    /// Returns `(network, server)` for a live connection profile.
    pub fn connection_context(&self, server_id: &str) -> Option<(String, String)> {
        self.inner
            .lock()
            .unwrap()
            .conns
            .contexts
            .get(server_id)
            .cloned()
    }

    /// The connection owning the active frontend window, if one is selected.
    pub fn active_connection(&self) -> Option<String> {
        let active = self.inner.lock().unwrap().conns.active.clone();
        (!active.is_empty()).then_some(active)
    }

    /// Server ids in stable mIRC `$cid` order (never HashMap iteration order).
    pub fn connections_in_cid_order(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .conns
            .entries
            .iter()
            .map(|(_, server_id)| server_id.clone())
            .collect()
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

    pub fn set_client_window_state(&self, label: &str, focused: bool, app_state: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let was_active = !inner.focused_windows.is_empty();
        if focused {
            inner.focused_windows.insert(label.to_string());
        } else {
            inner.focused_windows.remove(label);
        }
        let app_active = !inner.focused_windows.is_empty();
        inner.vars.insert(
            eval::CLIENT_APP_ACTIVE_KEY.into(),
            if app_active { "$true" } else { "$false" }.into(),
        );
        if label == "main" {
            inner
                .vars
                .insert(eval::CLIENT_APP_STATE_KEY.into(), app_state.to_string());
        }
        was_active != app_active
    }

    pub fn set_client_preferences(
        &self,
        dark_mode: bool,
        notify_list: Vec<String>,
        notify_online: Vec<String>,
        ignore_list: Vec<String>,
        highlight_list: Vec<String>,
        font_list: Vec<String>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.vars.insert(
            eval::CLIENT_DARK_MODE_KEY.into(),
            if dark_mode { "$true" } else { "$false" }.into(),
        );
        inner.vars.insert(
            eval::CLIENT_NOTIFY_LIST_KEY.into(),
            notify_list.join("\u{1f}"),
        );
        inner.vars.insert(
            eval::CLIENT_NOTIFY_ONLINE_KEY.into(),
            notify_online.join("\u{1f}"),
        );
        inner.vars.insert(
            eval::CLIENT_IGNORE_LIST_KEY.into(),
            ignore_list.join("\u{1f}"),
        );
        inner.vars.insert(
            eval::CLIENT_HIGHLIGHT_LIST_KEY.into(),
            highlight_list.join("\u{1f}"),
        );
        inner
            .vars
            .insert(eval::CLIENT_FONT_LIST_KEY.into(), font_list.join("\u{1f}"));
    }

    pub fn set_client_editbox(&self, target: &str, text: &str, start: usize, end: usize) {
        self.inner.lock().unwrap().vars.insert(
            format!("{}{}", eval::CLIENT_EDITBOX_PREFIX, target.to_lowercase()),
            format!("{start}\u{1f}{end}\u{1f}{text}"),
        );
    }

    pub fn set_client_unread_windows(&self, windows: Vec<String>) {
        self.inner.lock().unwrap().vars.insert(
            eval::CLIENT_UNREAD_WINDOWS_KEY.into(),
            windows.join("\u{1f}"),
        );
    }

    pub fn set_client_ui_state(
        &self,
        toolbar: bool,
        treebar: bool,
        switchbar: bool,
        menubar: bool,
        tips: bool,
    ) {
        let mut inner = self.inner.lock().unwrap();
        for (key, enabled) in [
            (eval::CLIENT_TOOLBAR_KEY, toolbar),
            (eval::CLIENT_TREEBAR_KEY, treebar),
            (eval::CLIENT_SWITCHBAR_KEY, switchbar),
            (eval::CLIENT_MENUBAR_KEY, menubar),
            (eval::CLIENT_TIPS_KEY, tips),
        ] {
            inner
                .vars
                .insert(key.into(), if enabled { "on" } else { "off" }.into());
        }
    }

    pub fn set_client_compat_state(
        &self,
        desktop_width: u32,
        desktop_height: u32,
        sound_enabled: bool,
        sound_volume: f64,
        do_not_disturb: bool,
        self_color: &str,
    ) {
        let mut vars = self.inner.lock().unwrap();
        for (key, value) in [
            (eval::CLIENT_DESKTOP_WIDTH_KEY, desktop_width.to_string()),
            (eval::CLIENT_DESKTOP_HEIGHT_KEY, desktop_height.to_string()),
            (
                eval::CLIENT_SOUND_ENABLED_KEY,
                if sound_enabled {
                    "$true".into()
                } else {
                    "$false".into()
                },
            ),
            (
                eval::CLIENT_SOUND_VOLUME_KEY,
                (sound_volume.clamp(0.0, 1.0) * 100.0).round().to_string(),
            ),
            (
                eval::CLIENT_DND_KEY,
                if do_not_disturb {
                    "$true".into()
                } else {
                    "$false".into()
                },
            ),
            (eval::CLIENT_SELF_COLOR_KEY, self_color.to_string()),
        ] {
            vars.vars.insert(key.into(), value);
        }
    }

    /// Compiles the combined source of all loaded script files.
    pub fn load(&self, source: &str) {
        self.load_sources(&[("<memory>".to_string(), source.to_string())]);
    }

    /// Compiles independently-loaded script files, preserving their order and
    /// source identity for mIRC's per-file event and local-alias semantics.
    pub fn load_sources(&self, sources: &[(String, String)]) {
        let mut combined = Script::default();
        for (name, source) in sources {
            let mut parsed = parser::parse(source);
            parsed.set_source(name);
            combined.append(parsed);
        }
        let mut g = self.inner.lock().unwrap();
        g.script = combined;
    }

    /// Loads the persisted user list (and auto-lists) from `dir/users.json`.
    pub fn load_users(&self, dir: &std::path::Path) {
        self.inner.lock().unwrap().users = users::UserList::load_from(dir);
    }

    /// A JSON snapshot of the user list + auto-lists (for the settings UI).
    pub fn users_json(&self) -> String {
        serde_json::to_string(&self.inner.lock().unwrap().users).unwrap_or_else(|_| "{}".into())
    }

    /// Mutates the user list under lock and persists it to `dir`.
    pub fn edit_users(&self, dir: &std::path::Path, f: impl FnOnce(&mut users::UserList)) {
        let mut g = self.inner.lock().unwrap();
        f(&mut g.users);
        g.users.save_to(dir);
    }

    pub fn has_alias(&self, name: &str) -> bool {
        // Local (`-l`) aliases aren't user-callable as `/commands`, and a disabled
        // `#group` makes its aliases uncallable too.
        let g = self.inner.lock().unwrap();
        g.script
            .find_public_alias(name)
            .is_some_and(|a| g.script.group_enabled(&g.vars, &a.group))
    }

    fn close_dialog_state(&self, name: &str) {
        eval::clear_dialog_state(&mut self.inner.lock().unwrap().vars, name);
    }

    /// Returns the user-defined popup items for a context (nicklist, channel, …),
    /// evaluating each item's dynamic label ($iif/$sock/…) in a run context (the
    /// right-clicked nick + channel) and dropping items whose label renders empty
    /// — mIRC's display behaviour. The `command` is left unexpanded (it's expanded
    /// when the item runs via [`run_command`]).
    pub fn popups_evaluated(
        &self,
        ctx: &RunCtx,
        context: &str,
        nick: &str,
        chan: &str,
    ) -> Vec<PopupItem> {
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let raw = script.popup_items(context);
        let event = EventVars {
            nick: nick.to_string(),
            chan: chan.to_string(),
            target: if chan.is_empty() {
                nick.to_string()
            } else {
                chan.to_string()
            },
            params: if nick.is_empty() {
                Vec::new()
            } else {
                vec![nick.to_string()]
            },
            // mIRC exposes the current listbox selection while dynamic popup
            // labels are evaluated, not only after an item is clicked.
            snicks: if nick.is_empty() {
                Vec::new()
            } else {
                vec![nick.to_string()]
            },
            menu: context.to_string(),
            menu_context: "window".into(),
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            local_scopes: Vec::new(),
            hashes: &mut g.hashes,
            var_expiry: &mut g.var_expiry,
            hash_expiry: &mut g.hash_expiry,
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            users: &mut g.users,
            event,
            actions: Vec::new(),
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: Vec::new(),
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            timers: g.timers.clone(),
            play: g.play.clone(),
            dcc: g.dcc.clone(),
            webviews: g.webviews.clone(),
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
        let Some(alias) = script.find_public_alias(name) else {
            return Vec::new();
        };
        // A disabled `#group` makes its aliases uncallable.
        if !script.group_enabled(&g.vars, &alias.group) {
            return Vec::new();
        }
        let chan = context_channel(ctx, target);
        let event = EventVars {
            nick: ctx.my_nick.to_string(),
            chan,
            target: target.to_string(),
            text: args.to_string(),
            params: args.split_whitespace().map(String::from).collect(),
            script_source: alias.source.clone(),
            script_line: alias.source_line,
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            local_scopes: Vec::new(),
            hashes: &mut g.hashes,
            var_expiry: &mut g.var_expiry,
            hash_expiry: &mut g.hash_expiry,
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            users: &mut g.users,
            event,
            actions: Vec::new(),
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: vec![alias.name.clone()],
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            timers: g.timers.clone(),
            play: g.play.clone(),
            dcc: g.dcc.clone(),
            webviews: g.webviews.clone(),
            input: g.input.clone(),
            caller: "command",
            show: true,
        };
        rt.run(&alias.body);
        let actions = std::mem::take(&mut rt.actions);
        drop(rt);
        if g.users.take_dirty() {
            g.users.save_to(&ctx.data_dir);
        }
        actions
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
        self.run_command_from_source(ctx, target, command, params, "")
    }

    /// Runs a deferred command while retaining the remote script file that
    /// created it, so `alias -l` resolution matches mIRC when a timer fires.
    pub fn run_command_from_source(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        params: &[String],
        source: &str,
    ) -> Vec<Action> {
        self.run_command_snicks_from_source(
            ctx,
            target,
            command,
            params,
            &[],
            source,
            "command",
            false,
            "",
            "",
            "",
            "",
        )
    }

    /// Runs a timer callback with mIRC's `$caller`/`$ctimer` context.
    pub fn run_timer_command(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        source: &str,
        timer_name: &str,
    ) -> Vec<Action> {
        self.run_command_snicks_from_source(
            ctx,
            target,
            command,
            &[],
            &[],
            source,
            "timer",
            false,
            timer_name,
            "",
            "",
            "",
        )
    }

    /// Runs a command line dequeued by `/play -c`, retaining both the script
    /// file that created it and mIRC's `$pnick` destination.
    pub fn run_play_command(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        source: &str,
        play_target: &str,
    ) -> Vec<Action> {
        self.run_command_snicks_from_source(
            ctx,
            target,
            command,
            &[],
            &[],
            source,
            "play",
            false,
            "",
            play_target,
            "",
            "",
        )
    }

    /// Runs one `/play -a` line as the selected alias' parameters. File-local
    /// aliases remain visible because the deferred invocation retains `source`.
    pub fn run_play_alias(
        &self,
        ctx: &RunCtx,
        target: &str,
        alias: &str,
        line: &str,
        source: &str,
        play_target: &str,
    ) -> Vec<Action> {
        let command = if line.is_empty() {
            alias.to_string()
        } else {
            format!("{alias} {line}")
        };
        self.run_command_snicks_from_source(
            ctx,
            target,
            &command,
            &[],
            &[],
            source,
            "play",
            false,
            "",
            play_target,
            "",
            "",
        )
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
        self.run_command_snicks_from_source(
            ctx, target, command, params, snicks, "", "command", false, "", "", "", "",
        )
    }

    pub fn run_editbox_command(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        params: &[String],
    ) -> Vec<Action> {
        self.run_command_snicks_from_source(
            ctx,
            target,
            command,
            params,
            &[],
            "",
            "command",
            true,
            "",
            "",
            "",
            "",
        )
    }

    pub fn run_popup_command(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        params: &[String],
        snicks: &[String],
        source: &str,
        menu: &str,
        menu_context: &str,
    ) -> Vec<Action> {
        self.run_command_snicks_from_source(
            ctx,
            target,
            command,
            params,
            snicks,
            source,
            "menu",
            false,
            "",
            "",
            menu,
            menu_context,
        )
    }

    fn run_command_snicks_from_source(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        params: &[String],
        snicks: &[String],
        source: &str,
        caller: &'static str,
        from_editbox: bool,
        timer_name: &str,
        play_target: &str,
        menu: &str,
        menu_context: &str,
    ) -> Vec<Action> {
        let body = parser::parse_body(command);
        let mut g = self.inner.lock().unwrap();
        let script = g.script.clone();
        let chan = context_channel(ctx, target);
        let event = EventVars {
            nick: params
                .first()
                .cloned()
                .unwrap_or_else(|| ctx.my_nick.to_string()),
            chan,
            target: target.to_string(),
            params: params.to_vec(),
            snicks: snicks.to_vec(),
            from_editbox,
            menu: menu.to_string(),
            menu_context: menu_context.to_string(),
            script_source: source.to_string(),
            timer: timer_name.to_string(),
            pnick: play_target.to_string(),
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            local_scopes: Vec::new(),
            hashes: &mut g.hashes,
            var_expiry: &mut g.var_expiry,
            hash_expiry: &mut g.hash_expiry,
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            users: &mut g.users,
            event,
            actions: Vec::new(),
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: Vec::new(),
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            timers: g.timers.clone(),
            play: g.play.clone(),
            dcc: g.dcc.clone(),
            webviews: g.webviews.clone(),
            input: g.input.clone(),
            caller,
            show: true,
        };
        rt.run(&body);
        let actions = std::mem::take(&mut rt.actions);
        drop(rt);
        if g.users.take_dirty() {
            g.users.save_to(&ctx.data_dir);
        }
        actions
    }

    fn run_window_mouse_command(
        &self,
        ctx: &RunCtx,
        target: &str,
        command: &str,
        source: &str,
        x: i32,
        y: i32,
        list_line: u32,
        key: u32,
    ) -> Vec<Action> {
        let body = parser::parse_body(command);
        let mut g = self.inner.lock().unwrap();
        if list_line == 0 {
            g.windows.record_click(target, x, y);
        }
        let script = g.script.clone();
        let event = EventVars {
            nick: ctx.my_nick.to_string(),
            chan: context_channel(ctx, target),
            target: target.to_string(),
            params: if list_line == 0 {
                vec![x.to_string(), y.to_string()]
            } else {
                vec![list_line.to_string()]
            },
            script_source: source.to_string(),
            mouse_x: x,
            mouse_y: y,
            mouse_win: target.to_string(),
            mouse_lb: (list_line != 0).to_string(),
            mouse_key: key,
            ..Default::default()
        };
        let g = &mut *g;
        let mut rt = Runtime {
            script: &script,
            my_nick: ctx.my_nick,
            network: ctx.network,
            server: ctx.server,
            vars: &mut g.vars,
            local_scopes: Vec::new(),
            hashes: &mut g.hashes,
            var_expiry: &mut g.var_expiry,
            hash_expiry: &mut g.hash_expiry,
            files: &mut g.files,
            bins: &mut g.bins,
            windows: &mut g.windows,
            users: &mut g.users,
            event,
            actions: Vec::new(),
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: Vec::new(),
            ret: None,
            goto: None,
            data_dir: ctx.data_dir.clone(),
            state: ctx.state.clone(),
            active: g.active.clone(),
            conns: g.conns.view(),
            wins: g.wins.view(),
            sockets: g.sockets.clone(),
            timers: g.timers.clone(),
            play: g.play.clone(),
            dcc: g.dcc.clone(),
            webviews: g.webviews.clone(),
            input: g.input.clone(),
            caller: "menu",
            show: true,
        };
        rt.run(&body);
        std::mem::take(&mut rt.actions)
    }

    /// Dispatches an event to all matching handlers. Returns the actions.
    pub fn dispatch_event(&self, ctx: &RunCtx, kind: &str, event: EventVars) -> Vec<Action> {
        self.dispatch_event_status(ctx, kind, event, None, None).0
    }

    /// Like [`dispatch_event`], but also reports whether any handler called
    /// `/halt` (used by `on INPUT` to suppress the typed line).
    pub fn dispatch_event_halt(
        &self,
        ctx: &RunCtx,
        kind: &str,
        event: EventVars,
    ) -> (Vec<Action>, bool) {
        let (actions, halted, _) = self.dispatch_event_status(ctx, kind, event, None, None);
        (actions, halted)
    }

    fn dispatch_event_default_halt_raw(
        &self,
        ctx: &RunCtx,
        kind: &str,
        event: EventVars,
        raw: Option<&RawEventContext>,
    ) -> (Vec<Action>, bool) {
        let (actions, _, default_halted) = self.dispatch_event_status(ctx, kind, event, raw, None);
        (actions, default_halted)
    }

    fn dispatch_event_status(
        &self,
        ctx: &RunCtx,
        kind: &str,
        event: EventVars,
        raw: Option<&RawEventContext>,
        phase: Option<bool>,
    ) -> (Vec<Action>, bool, bool) {
        // $event reflects the dispatch kind for every handler (text, raw, op, …).
        let mut event = event;
        event.event = kind.to_ascii_lowercase();
        if event.event_id.is_empty() {
            event.event_id = NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed).to_string();
        }
        if let Some(raw) = raw {
            event.raw_msg = raw.raw_msg.clone();
            event.raw_bytes = raw.raw_bytes.clone();
            event.msg_tags = raw.msg_tags.clone();
            event.msg_tags_raw = raw.msg_tags_raw.clone();
            event.msg_stamp = raw.msg_stamp.clone();
        }
        let mut g = self.inner.lock().unwrap();
        let remote_flags = g
            .vars
            .get(eval::REMOTE_FLAGS_KEY)
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(7);
        let required_flag = if kind.eq_ignore_ascii_case("RAW") {
            4
        } else if kind.eq_ignore_ascii_case("CTCP") || kind.eq_ignore_ascii_case("CTCPREPLY") {
            1
        } else {
            2
        };
        if remote_flags & required_flag == 0 {
            return (Vec::new(), false, false);
        }
        let script = g.script.clone();
        let g = &mut *g;
        let vars = &mut g.vars;
        let hashes = &mut g.hashes;
        let var_expiry = &mut g.var_expiry;
        let hash_expiry = &mut g.hash_expiry;
        let files = &mut g.files;
        let bins = &mut g.bins;
        let windows = &mut g.windows;
        let users = &mut g.users;
        let mut actions = Vec::new();
        let mut halted = false;
        let mut default_halted = false;
        let mut event_sources = Vec::new();
        for ev in script.events_of(kind) {
            if !event.event_source_filter.is_empty()
                && !ev.source.eq_ignore_ascii_case(&event.event_source_filter)
            {
                continue;
            }
            if !event_sources
                .iter()
                .any(|source: &String| source.eq_ignore_ascii_case(&ev.source))
            {
                event_sources.push(ev.source.clone());
            }
        }
        // `^` handlers are the early/default-text pass. mIRC processes them
        // independently from normal handlers, in script-file load order.
        for early_pass in [true, false] {
            if phase.is_some_and(|wanted| wanted != early_pass) {
                continue;
            }
            for source in &event_sources {
                let mut candidates = Vec::new();
                let mut highest_numeric: Option<i64> = None;
                for ev in script
                    .events_of(kind)
                    .filter(|ev| ev.source.eq_ignore_ascii_case(source))
                {
                    // A disabled `#group` suppresses its event handlers.
                    if !script.group_enabled(vars, &ev.group) {
                        continue;
                    }
                    let access = event_access(&ev.level);
                    if access.early != early_pass || (access.skip_if_halted && default_halted) {
                        continue;
                    }
                    let pattern = expand_event_vars(&ev.pattern, vars);
                    let selector = expand_event_vars(&ev.selector, vars);
                    let target = expand_event_vars(&ev.target, vars);
                    if !matches(
                        &event,
                        &pattern,
                        &selector,
                        &target,
                        kind,
                        access.regex_match,
                        &ctx.state.isupport,
                    ) {
                        continue;
                    }
                    let is_self = !event.nick.is_empty()
                        && ctx.state.isupport.names_equal(&event.nick, ctx.my_nick);
                    if (access.self_only && !is_self) || (access.exclude_self && is_self) {
                        continue;
                    }
                    if access.require_own_op {
                        let op_prefix = ctx.state.isupport.prefix_for_mode('o').unwrap_or('@');
                        let own_is_op = ctx
                            .state
                            .channels
                            .iter()
                            .find(|c| {
                                let channel = ctx
                                    .state
                                    .isupport
                                    .channel_target(&event.chan)
                                    .unwrap_or(&event.chan);
                                ctx.state.isupport.names_equal(&c.name, channel)
                            })
                            .and_then(|c| {
                                c.members.iter().find(|(nick, _)| {
                                    ctx.state.isupport.names_equal(nick, ctx.my_nick)
                                })
                            })
                            .is_some_and(|(_, prefixes)| prefixes.contains(op_prefix));
                        if !own_is_op {
                            continue;
                        }
                    }
                    // Access-level gate: the triggering user must satisfy the
                    // remaining level. $clevel/$ulevel come from this match.
                    let mut ev_event = event.clone();
                    let addr = ctx
                        .state
                        .ial
                        .iter()
                        .find(|(n, _)| ctx.state.isupport.names_equal(n, &event.nick))
                        .map(|(_, a)| a.as_str())
                        .unwrap_or("");
                    ev_event.match_key = pattern.clone();
                    let status = ctx
                        .state
                        .channels
                        .iter()
                        .find(|c| {
                            let channel = ctx
                                .state
                                .isupport
                                .channel_target(&event.chan)
                                .unwrap_or(&event.chan);
                            ctx.state.isupport.names_equal(&c.name, channel)
                        })
                        .and_then(|c| {
                            c.members
                                .iter()
                                .find(|(n, _)| ctx.state.isupport.names_equal(n, &event.nick))
                        })
                        .map(|(_, p)| p.as_str())
                        .unwrap_or("");
                    let ulevels = users.levels_of(&event.nick, addr);
                    match users::level_matches(&access.level, &ulevels, status) {
                        Some((clevel, ulevel)) => {
                            ev_event.clevel = clevel;
                            ev_event.matched_address = users
                                .matched_address_for(&event.nick, addr, &ulevel)
                                .unwrap_or(addr)
                                .to_string();
                            ev_event.ulevel = ulevel;
                        }
                        None => continue,
                    }
                    ev_event.script_source = ev.source.clone();
                    ev_event.script_line = ev.source_line;
                    ev_event.default_halted = default_halted;
                    let rank = event_level_rank(&access.level);
                    if let Some(rank) = rank {
                        highest_numeric = Some(highest_numeric.map_or(rank, |old| old.max(rank)));
                    }
                    candidates.push((ev, access, ev_event, rank));
                }

                for (ev, access, ev_event, rank) in candidates {
                    // Only the highest numeric level matching this event in this
                    // script file fires. Named levels are exact but unordered.
                    if rank.is_some() && rank != highest_numeric {
                        continue;
                    }
                    let mut rt = Runtime {
                        script: &script,
                        my_nick: ctx.my_nick,
                        network: ctx.network,
                        server: ctx.server,
                        vars: &mut *vars,
                        local_scopes: Vec::new(),
                        hashes: &mut *hashes,
                        var_expiry: &mut *var_expiry,
                        hash_expiry: &mut *hash_expiry,
                        files: &mut *files,
                        bins: &mut *bins,
                        windows: &mut *windows,
                        users: &mut *users,
                        event: ev_event,
                        actions: Vec::new(),
                        pending_pipe_commands: Vec::new(),
                        halted: false,
                        steps: 0,
                        depth: 0,
                        alias_stack: Vec::new(),
                        ret: None,
                        goto: None,
                        data_dir: ctx.data_dir.clone(),
                        state: ctx.state.clone(),
                        active: g.active.clone(),
                        conns: g.conns.view(),
                        wins: g.wins.view(),
                        sockets: g.sockets.clone(),
                        timers: g.timers.clone(),
                        play: g.play.clone(),
                        dcc: g.dcc.clone(),
                        webviews: g.webviews.clone(),
                        input: g.input.clone(),
                        caller: "event",
                        show: true,
                    };
                    rt.run(&ev.body);
                    // `/return` ends the handler but is not `/halt`.
                    let handler_halted = rt.halted && rt.ret.is_none();
                    // `/haltdef` always suppresses the event's default display;
                    // a plain `/halt` only does so in an early (`^`) handler.
                    let handler_default_halted =
                        rt.event.default_halted || (access.early && handler_halted);
                    default_halted |= handler_default_halted;
                    halted |= handler_halted || handler_default_halted;
                    actions.extend(rt.actions);
                }
            }
        }
        // Auto-op / auto-voice: when someone else joins a channel where I hold
        // op (or higher) and they match an enabled list, queue the mode change.
        if kind == "JOIN" && !ctx.state.isupport.names_equal(&event.nick, ctx.my_nick) {
            let addr = ctx
                .state
                .ial
                .iter()
                .find(|(n, _)| ctx.state.isupport.names_equal(n, &event.nick))
                .map(|(_, a)| a.as_str())
                .unwrap_or("");
            let am_op = ctx
                .state
                .channels
                .iter()
                .find(|c| {
                    let channel = ctx
                        .state
                        .isupport
                        .channel_target(&event.chan)
                        .unwrap_or(&event.chan);
                    ctx.state.isupport.names_equal(&c.name, channel)
                })
                .and_then(|c| {
                    c.members
                        .iter()
                        .find(|(n, _)| ctx.state.isupport.names_equal(n, ctx.my_nick))
                })
                .map(|(_, p)| p.contains('@') || p.contains('&') || p.contains('~'))
                .unwrap_or(false);
            if am_op {
                use users::AutoKind;
                if users.auto_should_apply(
                    AutoKind::Aop,
                    addr,
                    &event.nick,
                    &event.chan,
                    ctx.network,
                ) {
                    actions.push(Action::Send(format!(
                        "MODE {} +o {}",
                        event.chan, event.nick
                    )));
                } else if users.auto_should_apply(
                    AutoKind::Avoice,
                    addr,
                    &event.nick,
                    &event.chan,
                    ctx.network,
                ) {
                    actions.push(Action::Send(format!(
                        "MODE {} +v {}",
                        event.chan, event.nick
                    )));
                }
            }
        }
        // Protect: re-op a protected user who is deopped ($knick) in a channel
        // where I hold op.
        if kind == "DEOP"
            && !event.knick.is_empty()
            && !ctx.state.isupport.names_equal(&event.knick, ctx.my_nick)
        {
            let addr = ctx
                .state
                .ial
                .iter()
                .find(|(n, _)| ctx.state.isupport.names_equal(n, &event.knick))
                .map(|(_, a)| a.as_str())
                .unwrap_or("");
            let am_op = ctx
                .state
                .channels
                .iter()
                .find(|c| {
                    let channel = ctx
                        .state
                        .isupport
                        .channel_target(&event.chan)
                        .unwrap_or(&event.chan);
                    ctx.state.isupport.names_equal(&c.name, channel)
                })
                .and_then(|c| {
                    c.members
                        .iter()
                        .find(|(n, _)| ctx.state.isupport.names_equal(n, ctx.my_nick))
                })
                .map(|(_, p)| p.contains('@') || p.contains('&') || p.contains('~'))
                .unwrap_or(false);
            // MODE batches can remove and restore +o in the same server line.
            // The DEOP event still fires, but no repair is needed when the
            // protected nick is already op in the final channel snapshot.
            let protected_is_op = ctx
                .state
                .channels
                .iter()
                .find(|channel| ctx.state.isupport.names_equal(&channel.name, &event.chan))
                .and_then(|channel| {
                    channel
                        .members
                        .iter()
                        .find(|(nick, _)| ctx.state.isupport.names_equal(nick, &event.knick))
                })
                .map(|(_, prefixes)| {
                    prefixes.contains('@') || prefixes.contains('&') || prefixes.contains('~')
                })
                .unwrap_or(false);
            if am_op
                && !protected_is_op
                && users.auto_should_apply(
                    users::AutoKind::Protect,
                    addr,
                    &event.knick,
                    &event.chan,
                    ctx.network,
                )
            {
                actions.push(Action::Send(format!(
                    "MODE {} +o {}",
                    event.chan, event.knick
                )));
            }
        }
        if users.take_dirty() {
            users.save_to(&ctx.data_dir);
        }
        (actions, halted, default_halted)
    }
}

/// Recursively evaluates popup item labels with `rt`, dropping items whose label
/// renders empty (mIRC hides those). Separators pass through unchanged.
fn eval_popup_labels(rt: &mut Runtime, items: &[PopupItem]) -> Vec<PopupItem> {
    let mut out = Vec::new();
    for item in items {
        let saved_source = std::mem::replace(&mut rt.event.script_source, item.source.clone());
        if item.separator {
            out.push(item.clone());
            rt.event.script_source = saved_source;
            continue;
        }
        // $submenu($id($1)) dynamically generates a flat list of items in place.
        if let Some(arg) = parse_submenu_arg(&item.label) {
            out.extend(expand_submenu(rt, &arg));
            rt.event.script_source = saved_source;
            continue;
        }
        // A leading $style(N) sentinel (mIRC requires it be the first word) sets
        // the item's check/disabled state and is stripped from the visible label.
        let expanded = rt.expand(&item.label);
        let (checked, disabled, rest) = split_style_marker(&expanded);
        let label = rest.trim().to_string();
        if label.is_empty() {
            rt.event.script_source = saved_source;
            continue;
        }
        out.push(PopupItem {
            label,
            command: item.command.clone(),
            separator: false,
            checked,
            disabled,
            source: item.source.clone(),
            children: eval_popup_labels(rt, &item.children),
        });
        rt.event.script_source = saved_source;
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
    let source = rt.event.script_source.clone();
    let mut out = Vec::new();

    rt.event.params = vec!["begin".to_string()];
    if let Some(it) = make_generated_item(&rt.expand(arg), &source) {
        out.push(it);
    }
    for i in 1..=CAP {
        rt.event.params = vec![i.to_string()];
        let r = rt.expand(arg);
        if r.trim().is_empty() {
            break;
        }
        if let Some(it) = make_generated_item(&r, &source) {
            out.push(it);
        }
    }
    rt.event.params = vec!["end".to_string()];
    if let Some(it) = make_generated_item(&rt.expand(arg), &source) {
        out.push(it);
    }

    rt.event.params = saved;
    out
}

/// Parses one generated `$submenu` line (`-` separator, or `label:command`, with
/// an optional leading `$style` marker) into a flat popup item.
fn make_generated_item(text: &str, source: &str) -> Option<PopupItem> {
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
            source: source.to_string(),
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
        source: source.to_string(),
        children: Vec::new(),
    })
}

fn is_channel(name: &str) -> bool {
    // Includes IRCX's '%' channel prefix so `$chan` resolves on IRCX servers.
    name.starts_with(['#', '&', '!', '+', '%'])
}

fn context_channel(ctx: &RunCtx<'_>, target: &str) -> String {
    let Some(bare) = ctx.state.isupport.channel_target(target) else {
        return String::new();
    };
    ctx.state
        .channels
        .iter()
        .find(|channel| ctx.state.isupport.names_equal(&channel.name, bare))
        .map(|channel| channel.name.clone())
        .unwrap_or_else(|| bare.to_string())
}

#[derive(Clone)]
struct EventAccess {
    level: String,
    self_only: bool,
    exclude_self: bool,
    require_own_op: bool,
    early: bool,
    skip_if_halted: bool,
    regex_match: bool,
}

/// Separates the mIRC event gates that can be combined before an access level,
/// e.g. `on @!*:JOIN:#:`. `+N` deliberately remains part of the level because
/// it means an exact user-list level, not a gate character.
fn event_access(raw: &str) -> EventAccess {
    let mut value = raw.trim();
    let mut self_only = false;
    if value
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("me:"))
    {
        self_only = true;
        value = &value[3..];
    }
    let mut exclude_self = false;
    let mut require_own_op = false;
    let mut early = false;
    let mut skip_if_halted = false;
    let mut regex_match = false;
    loop {
        match value.chars().next() {
            Some('!') => {
                exclude_self = true;
                value = &value[1..];
            }
            Some('@') => {
                require_own_op = true;
                value = &value[1..];
            }
            Some('^') => {
                early = true;
                value = &value[1..];
            }
            Some('&') => {
                skip_if_halted = true;
                value = &value[1..];
            }
            Some('$') => {
                regex_match = true;
                value = &value[1..];
            }
            _ => break,
        }
    }
    EventAccess {
        level: if value.is_empty() {
            "*".into()
        } else {
            value.into()
        },
        self_only,
        exclude_self,
        require_own_op,
        early,
        skip_if_halted,
        regex_match,
    }
}

/// Numeric ordering key used by mIRC's "highest matching event level" rule.
/// Named levels are specific but unordered, so they are kept independently.
fn event_level_rank(level: &str) -> Option<i64> {
    let level = level.trim();
    if level.is_empty() || level == "*" {
        // `*` explicitly bypasses access levels and remains independent from
        // the numeric highest-level selection.
        return None;
    }
    level
        .strip_prefix('+')
        .or_else(|| level.strip_prefix('='))
        .unwrap_or(level)
        .parse::<i64>()
        .ok()
}

/// Splits a phrase into whitespace-separated `$1..` parameters.
fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn raw_source_is_server(raw: Option<&RawEventContext>) -> bool {
    raw.and_then(|context| context.raw_msg.strip_prefix(':'))
        .and_then(|line| line.split_whitespace().next())
        .is_some_and(|prefix| !prefix.contains('!') && !prefix.contains('@'))
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

/// Parses a rendered mode string ("+ov bob alice", "+o bob -v alice") into the
/// specific (event-name, affected-target) pairs to fire alongside `on MODE`.
/// Argument consumption follows the server's ISUPPORT PREFIX/CHANMODES rules so
/// parameter modes such as `+k`/`+l` do not shift the nick attached to a later
/// batched `+o`/`+v` change.
fn split_mode_events(
    modes: &str,
    isupport: &crate::irc::state::Isupport,
) -> Vec<(&'static str, String)> {
    let toks: Vec<&str> = modes.split_whitespace().collect();
    let is_mode_token = |t: &str| {
        t.len() > 1
            && t.starts_with(['+', '-'])
            && t.chars()
                .all(|c| c == '+' || c == '-' || c.is_ascii_alphabetic())
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if !is_mode_token(toks[i]) {
            i += 1;
            continue;
        }

        let mode_token = toks[i];
        i += 1;
        let mut adding = true;
        for letter in mode_token.chars() {
            match letter {
                '+' => adding = true,
                '-' => adding = false,
                _ => {
                    let arg = if isupport.mode_takes_arg(letter, adding)
                        && toks.get(i).is_some_and(|t| !is_mode_token(t))
                    {
                        let arg = toks[i];
                        i += 1;
                        Some(arg)
                    } else {
                        None
                    };
                    if let (Some(kind), Some(affected)) = (mode_event_name(letter, adding), arg) {
                        out.push((kind, affected.to_string()));
                    }
                }
            }
        }
    }
    out
}

/// Tests whether an event matches a handler's pattern and target spec.
fn matches(
    ev: &EventVars,
    pattern: &str,
    selector: &str,
    target_spec: &str,
    kind: &str,
    regex_match: bool,
    isupport: &crate::irc::state::Isupport,
) -> bool {
    if kind == "PARSELINE" {
        let direction_ok = target_spec.is_empty()
            || target_spec == "*"
            || target_spec.eq_ignore_ascii_case(&ev.parse_type);
        let pattern_ok = pattern.is_empty()
            || pattern == "*"
            || if regex_match {
                ident::mirc_regex_is_match(&ev.parse_line, pattern)
            } else {
                wildcard_match(pattern, &ev.parse_line)
            };
        return direction_ok && pattern_ok;
    }
    if kind == "DIALOG" {
        let name_ok = target_spec.is_empty()
            || target_spec == "*"
            || wildcard_match(target_spec, &ev.dialog_name);
        let event_ok = selector.is_empty()
            || selector == "*"
            || selector.eq_ignore_ascii_case(&ev.dialog_event);
        let id_ok =
            pattern.is_empty() || pattern == "*" || dialog_id_matches(pattern, &ev.dialog_control);
        return name_ok && event_ok && id_ok;
    }
    if kind == "DCCSERVER" {
        return selector.is_empty() || selector == "*" || selector.eq_ignore_ascii_case(&ev.text);
    }
    if kind == "CHAR" {
        let target_ok =
            target_spec.is_empty() || target_spec == "@" || wildcard_match(target_spec, &ev.target);
        let key = ev
            .key_val
            .map(|value| value.to_string())
            .unwrap_or_default();
        let key_ok = selector.is_empty()
            || selector == "*"
            || selector.split(',').any(|value| value.trim() == key);
        return target_ok && key_ok;
    }
    if kind == "RAW" {
        let selector_ok =
            selector.is_empty() || selector == "*" || wildcard_match(selector, &ev.text);
        let raw_text = ev.params.join(" ");
        let pattern_ok = pattern.is_empty()
            || pattern == "*"
            || if regex_match {
                ident::mirc_regex_is_match(&raw_text, pattern)
            } else {
                wildcard_match(pattern, &raw_text)
            };
        return selector_ok && pattern_ok;
    }
    let pat_ok = pattern.is_empty()
        || pattern == "*"
        || if regex_match {
            ident::mirc_regex_is_match(&ev.text, pattern)
        } else {
            wildcard_match(pattern, &ev.text)
        }
        // A CTCP matchtext also matches just the command word, so
        // `on CTCP:PING:` catches "PING <timestamp>" (likewise `on CTCPREPLY`).
        || (!regex_match
            && (kind == "CTCP" || kind == "CTCPREPLY")
            && wildcard_match(pattern, ev.text.split_whitespace().next().unwrap_or("")));
    if !pat_ok {
        return false;
    }
    target_spec.split(',').any(|spec| match spec.trim() {
        "" | "*" => true,
        "#" => isupport.channel_target(&ev.chan).is_some(),
        "?" => {
            ev.chan.is_empty()
                && !ev.target.starts_with('=')
                && !ev.target.starts_with('!')
                && !ev.target.starts_with('@')
        }
        "=" => ev.target.starts_with('='),
        "!" => ev.target.starts_with('!'),
        "@" => ev.target.starts_with('@'),
        spec if isupport.channel_target(spec).is_some() => {
            let wanted = isupport.channel_target(spec).unwrap_or(spec);
            let actual = isupport.channel_target(&ev.chan).unwrap_or(&ev.chan);
            isupport.names_equal(actual, wanted)
        }
        // A named target (e.g. a socket name in `on *:SOCKREAD:bot:`) is matched
        // as a wildcard against the event's name/channel.
        spec => wildcard_match(spec, &ev.chan),
    })
}

fn dialog_id_matches(spec: &str, id: &str) -> bool {
    spec.split(',').any(|part| {
        let part = part.trim();
        if let Some((start, end)) = part.split_once('-') {
            let id = id.parse::<i64>().ok();
            let start = start.trim().parse::<i64>().ok();
            let end = end.trim().parse::<i64>().ok();
            id.is_some()
                && start
                    .zip(end)
                    .is_some_and(|(start, end)| (start..=end).contains(&id.unwrap()))
        } else {
            part.eq_ignore_ascii_case(id)
        }
    })
}

/// Event matchtext/target fields are evaluated when the event fires. This
/// covers the common mIRC `%match` / `%channel` forms using persistent globals;
/// local variables cannot exist between event runs.
fn expand_event_vars(field: &str, vars: &HashMap<String, String>) -> String {
    let chars: Vec<char> = field.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < chars.len()
            && (chars[end].is_alphanumeric() || chars[end] == '_' || chars[end] == '.')
        {
            end += 1;
        }
        if end == start {
            out.push('%');
            i += 1;
        } else {
            let name: String = chars[start..end].iter().collect();
            out.push_str(vars.get(&name).map(String::as_str).unwrap_or(""));
            i = end;
        }
    }
    out
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

/// Reads an image from the script-data sandbox for `/drawpic`.
#[tauri::command]
pub fn script_picture_read(app: AppHandle, filename: String) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let path = eval::sandbox_path(&script_data_dir(&app), &filename);
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let mime = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        _ => "image/png",
    };
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

/// Saves `/drawsave` output inside the script-data sandbox.
#[tauri::command]
pub fn script_picture_save(app: AppHandle, filename: String, data: String) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let encoded = data
        .split_once(',')
        .map(|(_, encoded)| encoded)
        .unwrap_or(data.as_str());
    let bytes = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
    let path = eval::sandbox_path(&script_data_dir(&app), &filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

/// Refreshes the engine-side bitmap queried synchronously by `$getdot`.
#[tauri::command]
pub fn script_picture_snapshot(
    engine: State<'_, ScriptEngine>,
    name: String,
    width: u32,
    height: u32,
    rgba: String,
) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let bytes = STANDARD.decode(rgba).map_err(|e| e.to_string())?;
    engine
        .inner
        .lock()
        .unwrap()
        .windows
        .set_bitmap(&name, width, height, bytes);
    Ok(())
}

/// Stores `/drawsave -v` output in an mSL binary variable.
#[tauri::command]
pub fn script_picture_binvar(
    engine: State<'_, ScriptEngine>,
    name: String,
    data: String,
) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let encoded = data
        .split_once(',')
        .map(|(_, encoded)| encoded)
        .unwrap_or(data.as_str());
    let bytes = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
    engine
        .inner
        .lock()
        .unwrap()
        .bins
        .set(&name, 1, &bytes, true);
    Ok(())
}

/// Sanitizes a script name into a safe file stem.
fn script_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "script".to_string()
    } else {
        trimmed
    }
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
    // Popup menus are seeded per context, matching the tabs in the script
    // editor's Popups section — the editor edits one file per context, so
    // seeding a single combined file would show up as duplicate menus.
    // `popups.mrc` itself stays an empty combined file for imported menus.
    //
    // Kept as real .msl files rather than escaped string literals so the
    // shipped defaults stay readable and can be tested as ordinary mSL. These
    // must match the templates in `src/components/ScriptDialog.tsx`, which the
    // editor falls back to when a file is missing.
    ("popups-status", include_str!("examples/popups-status.msl")),
    ("popups-channel", include_str!("examples/popups-channel.msl")),
    ("popups-nicklist", include_str!("examples/popups-nicklist.msl")),
    ("popups-query", include_str!("examples/popups-query.msl")),
    (
        "popups",
        "; Optional combined popup file for imported or existing menu blocks.\n\
         ; Dedicated context files are shown above entries from Remote scripts.\n",
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

fn fire_lifecycle_source(app: &AppHandle, engine: &ScriptEngine, kind: &str, source: &str) {
    let ctx = RunCtx {
        my_nick: "",
        network: "",
        server: "",
        data_dir: script_data_dir(app),
        state: std::sync::Arc::new(Default::default()),
    };
    let actions = engine.dispatch_event(
        &ctx,
        kind,
        EventVars {
            event_source_filter: source.to_string(),
            ..Default::default()
        },
    );
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
    let disabled_path = dir.join("_disabled.json");
    let disabled: Vec<String> = std::fs::read_to_string(&disabled_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mrc"))
            .filter(|p| {
                let name = p.file_name().and_then(|name| name.to_str()).unwrap_or("");
                !disabled.iter().any(|item| item.eq_ignore_ascii_case(name))
            })
            .collect();
        files.sort();
        for path in files {
            if let Ok(src) = std::fs::read_to_string(&path) {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<script>")
                    .to_string();
                sources.push((name, src));
            }
        }
    }
    engine.load_sources(&sources);
    engine.load_users(&script_data_dir(app));
}

fn set_script_loaded(
    app: &AppHandle,
    engine: &ScriptEngine,
    name: &str,
    load: bool,
    suppress_event: bool,
) {
    let Ok(dir) = scripts_dir(app) else { return };
    let filename = format!("{}.mrc", script_stem(name.trim_end_matches(".mrc")));
    if load && !dir.join(&filename).exists() {
        return;
    }
    let disabled_path = dir.join("_disabled.json");
    let mut disabled: Vec<String> = std::fs::read_to_string(&disabled_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    disabled.retain(|item| !item.eq_ignore_ascii_case(&filename));
    if !load {
        if !suppress_event {
            use std::sync::atomic::Ordering;
            if !FIRING_UNLOAD.swap(true, Ordering::SeqCst) {
                fire_lifecycle_source(app, engine, "UNLOAD", &filename);
                FIRING_UNLOAD.store(false, Ordering::SeqCst);
            }
        }
        disabled.push(filename.clone());
    }
    let _ = std::fs::write(
        &disabled_path,
        serde_json::to_string_pretty(&disabled).unwrap_or_else(|_| "[]".into()),
    );
    // Avoid the global UNLOAD performed by recompile: the selected file's
    // lifecycle event was handled above and /load must not unload existing files.
    let Ok(dir) = scripts_dir(app) else { return };
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "mrc"))
            .filter(|path| {
                let file = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                !disabled.iter().any(|item| item.eq_ignore_ascii_case(file))
            })
            .collect();
        files.sort();
        for path in files {
            if let Ok(source) = std::fs::read_to_string(&path) {
                sources.push((
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    source,
                ));
            }
        }
    }
    engine.load_sources(&sources);
    if load {
        fire_lifecycle_source(app, engine, "LOAD", &filename);
    }
}

/// Whether a script line defines an alias named `name` (case-insensitive).
fn alias_line_defines(line: &str, name: &str) -> bool {
    line.trim_start()
        .strip_prefix("alias ")
        .map(|rest| {
            let mut words = rest.trim_start().split_whitespace();
            let first = words.next().unwrap_or("");
            let candidate = if first.eq_ignore_ascii_case("-l") {
                words.next().unwrap_or("")
            } else {
                first
            };
            candidate
                .trim_start_matches('/')
                .split([' ', '\t', '{'])
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(name)
        })
        .unwrap_or(false)
}

/// Adds/replaces (`command` = Some) or removes (`command` = None) a single-line
/// runtime alias (`/alias`) in `_runtime.mrc` or the requested script file, then
/// recompiles so it takes effect.
fn update_runtime_alias(
    app: &AppHandle,
    name: &str,
    command: Option<&str>,
    file: Option<&str>,
    local: bool,
) {
    let Ok(dir) = scripts_dir(app) else { return };
    let stem = file.map(script_stem).unwrap_or_else(|| "_runtime".into());
    let path = dir.join(format!("{stem}.mrc"));
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !alias_line_defines(l, name))
        .map(String::from)
        .collect();
    if let Some(cmd) = command {
        lines.push(format!(
            "alias {}{name} {{ {cmd} }}",
            if local { "-l " } else { "" }
        ));
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

/// Returns whether a script file is currently loaded (not present in
/// `_disabled.json`). Missing files are treated as enabled so a new popup
/// section starts visible when first saved.
#[tauri::command]
pub fn script_is_loaded(app: AppHandle, name: String) -> Result<bool, String> {
    let filename = format!("{}.mrc", script_stem(name.trim_end_matches(".mrc")));
    let disabled_path = scripts_dir(&app)?.join("_disabled.json");
    let disabled: Vec<String> = std::fs::read_to_string(disabled_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    Ok(!disabled
        .iter()
        .any(|item| item.eq_ignore_ascii_case(&filename)))
}

/// Enables or disables one script file without deleting its contents.
#[tauri::command]
pub fn script_set_loaded(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    name: String,
    loaded: bool,
) -> Result<(), String> {
    set_script_loaded(&app, &engine, &name, loaded, true);
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

/// Fires `on LOGON` after the client has sent its registration lines but before
/// it receives the welcome numeric (`on CONNECT`).
pub fn fire_logon(
    app: &AppHandle,
    server_id: &str,
    my_nick: &str,
    network: &str,
    server: &str,
    state: std::sync::Arc<crate::irc::state::StateSnapshot>,
    early: bool,
) -> bool {
    let Some(engine) = app.try_state::<ScriptEngine>() else {
        return false;
    };
    let ctx = RunCtx {
        my_nick,
        network,
        server,
        data_dir: script_data_dir(app),
        state,
    };
    let match_name = if network.is_empty() { server } else { network };
    let vars = EventVars {
        chan: match_name.to_string(),
        target: match_name.to_string(),
        text: match_name.to_string(),
        params: vec![match_name.to_string()],
        ..Default::default()
    };
    let (actions, _, default_halted) =
        engine.dispatch_event_status(&ctx, "LOGON", vars, None, Some(early));
    apply_actions(app, server_id, my_nick, network, server, actions);
    default_halted
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
                // mIRC addresses DCC chat buffers as `=nick`. Scripted
                // `/msg =$nick ...` must use the peer socket, never leak a
                // non-standard `PRIVMSG =nick` to the IRC server.
                if let Some(rest) = line.strip_prefix("PRIVMSG =") {
                    if let Some((nick, text)) = rest.split_once(" :") {
                        let id = format!("={nick}");
                        if let Some(dcc) = app.try_state::<crate::irc::dcc::DccManager>() {
                            match dcc.send(server_id, &id, text.to_string()) {
                                Ok(()) => {
                                    let _ = app.emit(
                                        IRC_EVENT,
                                        UiEvent::DccChatLine {
                                            server_id: server_id.to_string(),
                                            id,
                                            from: my_nick.to_string(),
                                            text: text.to_string(),
                                        },
                                    );
                                }
                                Err(error) => {
                                    let _ = app.emit(
                                        IRC_EVENT,
                                        UiEvent::Echo {
                                            server_id: server_id.to_string(),
                                            target: "(status)".to_string(),
                                            text: format!("DCC: {error}"),
                                        },
                                    );
                                }
                            }
                        }
                        continue;
                    }
                }
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
            Action::DccServer { args } => {
                if let Some(dcc) = app.try_state::<crate::irc::dcc::DccManager>() {
                    if let Err(error) = dcc.run_server_command(app.clone(), server_id, &args) {
                        let _ = app.emit(
                            IRC_EVENT,
                            UiEvent::Echo {
                                server_id: server_id.to_string(),
                                target: "(status)".to_string(),
                                text: format!("DCC server: {error}"),
                            },
                        );
                    }
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
            Action::Play {
                args,
                current_target,
                remote,
                source,
            } => {
                let invocation = play::PlayInvocation {
                    args,
                    current_target,
                    remote,
                    source,
                };
                let result = app
                    .try_state::<play::PlayManager>()
                    .ok_or_else(|| "play manager is unavailable".to_string())
                    .and_then(|play| {
                        play.command(
                            app.clone(),
                            server_id.to_string(),
                            my_nick.to_string(),
                            network.to_string(),
                            server.to_string(),
                            invocation,
                        )
                    });
                if let Err(error) = result {
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::Echo {
                            server_id: server_id.to_string(),
                            target: "(status)".to_string(),
                            text: format!("Play: {error}"),
                        },
                    );
                }
            }
            Action::PlayLine {
                target,
                text,
                notice,
                echo,
            } => {
                if let Some(nick) = target.strip_prefix('=').filter(|_| !notice) {
                    let id = format!("={nick}");
                    if let Some(dcc) = app.try_state::<crate::irc::dcc::DccManager>() {
                        if dcc.send(server_id, &id, text.clone()).is_ok() && echo {
                            let _ = app.emit(
                                IRC_EVENT,
                                UiEvent::DccChatLine {
                                    server_id: server_id.to_string(),
                                    id,
                                    from: my_nick.to_string(),
                                    text,
                                },
                            );
                        }
                    }
                    continue;
                }
                let command = if notice { "NOTICE" } else { "PRIVMSG" };
                let line = format!("{command} {target} :{text}");
                if echo {
                    if let Some(event) = self_echo(server_id, my_nick, &line) {
                        let _ = app.emit(IRC_EVENT, event);
                    }
                }
                if let Some(manager) = &manager {
                    let _ = manager.send(server_id, line);
                }
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
            Action::Dcc { args } => {
                if let Some(dcc) = app.try_state::<crate::irc::dcc::DccManager>() {
                    if let Err(error) =
                        dcc.run_script_command(app.clone(), server_id, &args, &script_data_dir(app))
                    {
                        let _ = app.emit(
                            IRC_EVENT,
                            UiEvent::Echo {
                                server_id: server_id.to_string(),
                                target: "(status)".to_string(),
                                text: format!("DCC: {error}"),
                            },
                        );
                    }
                }
            }
            Action::Fserve {
                nick,
                max_gets,
                home,
                welcome,
            } => {
                if let Some(dcc) = app.try_state::<crate::irc::dcc::DccManager>() {
                    let data_dir = script_data_dir(app);
                    let home = data_dir.join(home);
                    let welcome = welcome.map(|path| data_dir.join(path));
                    if let Err(error) = dcc.fserve(
                        app.clone(),
                        server_id.to_string(),
                        nick,
                        max_gets,
                        data_dir,
                        home,
                        welcome,
                    ) {
                        let _ = app.emit(
                            IRC_EVENT,
                            UiEvent::Echo {
                                server_id: server_id.to_string(),
                                target: "(status)".to_string(),
                                text: format!("DCC fserve: {error}"),
                            },
                        );
                    }
                }
            }
            Action::DefineAlias {
                name,
                command,
                file,
                local,
            } => {
                update_runtime_alias(app, &name, command.as_deref(), file.as_deref(), local);
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
                            app,
                            server_id,
                            my_nick,
                            network,
                            server,
                            more,
                            depth + 1,
                        );
                    }
                }
            }
            Action::RunOn {
                server_id: target,
                command,
            } => {
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
            Action::Toolbar {
                op,
                name,
                tooltip,
                icon,
                command,
                source,
            } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::Toolbar {
                        server_id: server_id.to_string(),
                        op,
                        name,
                        tooltip,
                        icon,
                        command,
                        source,
                    },
                );
            }
            Action::Panel {
                op,
                panel,
                id,
                label,
                value,
                command,
                source,
            } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::Panel {
                        server_id: server_id.to_string(),
                        op,
                        panel,
                        id,
                        label,
                        value,
                        command,
                        source,
                    },
                );
            }
            Action::Audio {
                operation,
                path,
                end_event,
            } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::Audio {
                        server_id: server_id.to_string(),
                        operation,
                        path,
                        end_event,
                    },
                );
            }
            Action::ClientCommand {
                command,
                args,
                current_target,
            } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::ClientCommand {
                        server_id: server_id.to_string(),
                        command,
                        args,
                        current_target,
                    },
                );
            }
            Action::ScriptLoad {
                name,
                load,
                suppress_event,
            } => {
                if let Some(engine) = app.try_state::<ScriptEngine>() {
                    set_script_loaded(app, &engine, &name, load, suppress_event);
                }
            }
            Action::DnsLookup { host } => {
                let app = app.clone();
                let server_id = server_id.to_string();
                tauri::async_runtime::spawn(async move {
                    let ips = crate::commands::resolve_host(&host)
                        .await
                        .unwrap_or_default();
                    let Some(engine) = app.try_state::<ScriptEngine>() else {
                        return;
                    };
                    let state = app
                        .try_state::<crate::irc::state::StateStore>()
                        .map(|store| store.get(&server_id))
                        .unwrap_or_default();
                    let my_nick = state.nick.clone();
                    let (network, server) =
                        engine.connection_context(&server_id).unwrap_or_default();
                    let ctx = RunCtx {
                        my_nick: &my_nick,
                        network: &network,
                        server: &server,
                        data_dir: script_data_dir(&app),
                        state,
                    };
                    let vars = EventVars {
                        dns_query: host.clone(),
                        dns_ips: ips,
                        text: host.clone(),
                        params: vec![host],
                        ..Default::default()
                    };
                    let actions = engine.dispatch_event(&ctx, "DNS", vars);
                    apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
                });
            }
            Action::WindowClose { name } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::WindowClose {
                        server_id: server_id.to_string(),
                        name,
                    },
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
            Action::WindowTitle { name, title } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::WindowTitle {
                        server_id: server_id.to_string(),
                        name,
                        title,
                    },
                );
            }
            Action::WindowDraw { name, op, args } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::WindowDraw {
                        server_id: server_id.to_string(),
                        name,
                        op,
                        args,
                    },
                );
            }
            Action::WebviewOpen {
                name,
                profile,
                width,
                height,
                url,
                title,
            } => {
                let result = match app.try_state::<webview::WebviewManager>() {
                    Some(manager) => manager.open(
                        app.clone(),
                        server_id,
                        my_nick,
                        network,
                        server,
                        name,
                        profile,
                        width,
                        height,
                        url,
                        title,
                    ),
                    None => Err("native browser manager is unavailable".to_string()),
                };
                if let Err(error) = result {
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::Echo {
                            server_id: server_id.to_string(),
                            target: "(status)".to_string(),
                            text: format!("Webview: {error}"),
                        },
                    );
                }
            }
            Action::WebviewNavigate { name, url } => {
                let result = match app.try_state::<webview::WebviewManager>() {
                    Some(manager) => manager.navigate(app.clone(), server_id, &name, url),
                    None => Err("native browser manager is unavailable".to_string()),
                };
                if let Err(error) = result {
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::Echo {
                            server_id: server_id.to_string(),
                            target: "(status)".to_string(),
                            text: format!("Webview: {error}"),
                        },
                    );
                }
            }
            Action::WebviewCookies { name, url } => {
                let result = match app.try_state::<webview::WebviewManager>() {
                    Some(manager) => manager.cookies(app.clone(), server_id, &name, url),
                    None => Err("native browser manager is unavailable".to_string()),
                };
                if let Err(error) = result {
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::Echo {
                            server_id: server_id.to_string(),
                            target: "(status)".to_string(),
                            text: format!("Webview: {error}"),
                        },
                    );
                }
            }
            Action::WebviewFocus { name } => {
                let result = match app.try_state::<webview::WebviewManager>() {
                    Some(manager) => manager.focus(app.clone(), server_id, &name),
                    None => Err("native browser manager is unavailable".to_string()),
                };
                if let Err(error) = result {
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::Echo {
                            server_id: server_id.to_string(),
                            target: "(status)".to_string(),
                            text: format!("Webview: {error}"),
                        },
                    );
                }
            }
            Action::WebviewClose { name } => {
                let result = match app.try_state::<webview::WebviewManager>() {
                    Some(manager) => manager.close(app.clone(), server_id, &name),
                    None => Err("native browser manager is unavailable".to_string()),
                };
                if let Err(error) = result {
                    let _ = app.emit(
                        IRC_EVENT,
                        UiEvent::Echo {
                            server_id: server_id.to_string(),
                            target: "(status)".to_string(),
                            text: format!("Webview: {error}"),
                        },
                    );
                }
            }
            Action::Timer {
                name,
                reps,
                interval_ms,
                start_at,
                command,
                target,
                offline,
                catch_up,
                ordered,
                milliseconds,
                high_resolution,
                dynamic,
                source,
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
                        start_at,
                        command,
                        target,
                        offline,
                        catch_up,
                        ordered,
                        milliseconds,
                        high_resolution,
                        dynamic,
                        source,
                    );
                }
            }
            Action::TimerStop { name } => {
                if let Some(m) = app.try_state::<timer::TimerManager>() {
                    m.stop(&name);
                }
            }
            Action::TimerExecute { name } => {
                if let Some(m) = app.try_state::<timer::TimerManager>() {
                    m.execute(&name);
                }
            }
            Action::TimerPause { name, countdown } => {
                if let Some(m) = app.try_state::<timer::TimerManager>() {
                    m.pause(&name, countdown);
                }
            }
            Action::TimerResume { name } => {
                if let Some(m) = app.try_state::<timer::TimerManager>() {
                    m.resume(&name);
                }
            }
            Action::TimerList { target, name } => {
                let timers = app
                    .try_state::<timer::TimerManager>()
                    .map(|m| m.snapshot_matching(&name))
                    .unwrap_or_default();
                let text = if timers.is_empty() {
                    "No active timers".to_string()
                } else if name == "*" {
                    format!(
                        "Active timers: {}",
                        timers
                            .iter()
                            .map(|timer| timer.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    timers
                        .iter()
                        .map(|timer| {
                            format!(
                                "Timer {}: {} {} {}",
                                timer.name, timer.reps, timer.delay, timer.command
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
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
            Action::SockOpen {
                name,
                host,
                port,
                tls,
                accept_invalid,
                bind_ip,
                nodelay,
                ip_version,
                reservation_id,
            } => {
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
                        accept_invalid,
                        bind_ip,
                        nodelay,
                        ip_version,
                        reservation_id,
                    );
                }
            }
            Action::SockUdp {
                name,
                bind_ip,
                local_port,
                dest_ip,
                dest_port,
                data,
                keep,
                dual_stack,
                reservation_id,
            } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.udp(
                        app.clone(),
                        server_id.to_string(),
                        network.to_string(),
                        my_nick.to_string(),
                        name,
                        bind_ip,
                        local_port,
                        dest_ip,
                        dest_port,
                        data,
                        keep,
                        dual_stack,
                        reservation_id,
                    );
                }
            }
            Action::SockWrite { name, data } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.write(&name, data);
                }
            }
            Action::SockError { kind, name, error } => {
                socket::fire_error(app, server_id, network, my_nick, &kind, &name, "", error);
            }
            Action::SockClose { name } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.close(&name);
                }
            }
            Action::SockMark { name, mark } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.set_mark(&name, &mark);
                }
            }
            Action::SockRename { name, newname } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.rename(&name, &newname);
                }
            }
            Action::SockPause { name, resume } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.pause(&name, resume);
                }
            }
            Action::SockListen { name, listener_id } => {
                if let Some(m) = app.try_state::<socket::SocketManager>() {
                    m.start_listener(
                        app.clone(),
                        server_id.to_string(),
                        network.to_string(),
                        my_nick.to_string(),
                        name,
                        listener_id,
                    );
                }
            }
            Action::Server {
                host,
                port,
                pass,
                new_window,
            } => {
                // The frontend opens a server window and starts the native
                // connection (a script `/server`, as used by local bridges).
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::ScriptServer {
                        server_id: server_id.to_string(),
                        host,
                        port,
                        pass,
                        new_window,
                    },
                );
            }
            Action::DialogOpen {
                name,
                title,
                controls,
                width,
                height,
            } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::DialogOpen {
                        server_id: server_id.to_string(),
                        name,
                        title,
                        controls,
                        width,
                        height,
                    },
                );
            }
            Action::DialogClose { name } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::DialogClose {
                        server_id: server_id.to_string(),
                        name,
                    },
                );
            }
            Action::DialogSet {
                dialog,
                control,
                op,
                value,
            } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::DialogSet {
                        server_id: server_id.to_string(),
                        dialog,
                        control,
                        op,
                        value,
                    },
                );
            }
            Action::NickIcon { nick, icon } => {
                let _ = app.emit(
                    IRC_EVENT,
                    UiEvent::NickIcon {
                        server_id: server_id.to_string(),
                        nick,
                        icon,
                    },
                );
            }
            queued @ Action::ParseLine { queue: true, .. } => {
                if let (Some(m), Some(control)) = (&manager, encode_parseline_control(&queued)) {
                    let _ = m.send(server_id, control);
                }
            }
            // A non-queued replacement is meaningful only while the connection
            // task is extracting PARSELINE actions from the current event.
            Action::ParseLine { .. } => {}
        }
    }
}

/// Encodes a queued PARSELINE action as an internal connection-manager line.
/// Base64 keeps arbitrary binary variables and embedded whitespace byte-exact.
pub fn encode_parseline_control(action: &Action) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
    let Action::ParseLine {
        direction,
        bytes,
        queue: true,
        trigger,
        append_crlf,
        utf8,
    } = action
    else {
        return None;
    };
    Some(format!(
        "\u{0}PARSELINE {direction} {}{}{} {}",
        u8::from(*trigger),
        u8::from(*append_crlf),
        u8::from(*utf8),
        STANDARD_NO_PAD.encode(bytes)
    ))
}

/// Decodes an internal queued PARSELINE control line.
pub fn decode_parseline_control(line: &str) -> Option<Action> {
    use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
    let rest = line.strip_prefix("\u{0}PARSELINE ")?;
    let mut fields = rest.splitn(3, ' ');
    let direction = fields.next()?.to_string();
    if direction != "in" && direction != "out" {
        return None;
    }
    let flags = fields.next()?.as_bytes();
    if flags.len() != 3 {
        return None;
    }
    let bytes = STANDARD_NO_PAD.decode(fields.next()?).ok()?;
    Some(Action::ParseLine {
        direction,
        bytes,
        queue: true,
        trigger: flags[0] == b'1',
        append_crlf: flags[1] == b'1',
        utf8: flags[2] == b'1',
    })
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
    // The alias may call `$input`/`$?`, which blocks the run waiting for the UI
    // dialog. Run it on a blocking thread so the main thread (and WebView2) stay
    // responsive — a sync command blocking the main thread freezes the dialog.
    // (The `has_alias` check above is cheap and stays synchronous for the return.)
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
        let actions = engine.run_alias(&ctx, &target, &name, &args);
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    });
    true
}

/// Returns the user-defined popup items for a context (nicklist / channel / status
/// / menubar), with dynamic labels ($iif/$sock/…) evaluated against the right-click
/// context and empty-label items dropped (mIRC behaviour).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn script_popups(
    app: AppHandle,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    context: String,
    nick: String,
) -> Vec<PopupItem> {
    // A popup label is evaluated ($iif/$sock/…) to build the menu; in the unlikely
    // event a label reaches `$input`, do it off the main thread (async command +
    // blocking thread) so building the menu can't freeze WebView2.
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
        let chan = if is_channel(&target) {
            target.as_str()
        } else {
            ""
        };
        engine.popups_evaluated(&ctx, &context, &nick, chan)
    })
    .await
    .unwrap_or_default()
}

/// Fires an `on DIALOG` handler when the user interacts with a script dialog.
/// `control` is the control that triggered it (a button id, or `init`/`close`);
/// `values` is the current id->value of every control, exposed via `$did`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn script_run_dialog(
    app: AppHandle,
    server_id: String,
    my_nick: String,
    network: String,
    dialog: String,
    event: String,
    control: String,
    values: HashMap<String, String>,
) -> bool {
    // An `on DIALOG` handler may call `$input`/`$?`, which blocks the run waiting
    // for the prompt reply. Run it on a blocking thread so the main thread (and
    // WebView2) stay responsive — a sync command blocking the main thread freezes
    // the whole UI, including the dialog itself.
    tauri::async_runtime::spawn_blocking(move || {
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
            target: dialog.clone(),
            text: control.clone(),
            params: vec![control.clone()],
            did: values,
            dialog_name: dialog.clone(),
            dialog_event: event.clone(),
            dialog_control: control,
            ..Default::default()
        };
        let engine = app.state::<ScriptEngine>();
        let (actions, halted) = engine.dispatch_event_halt(&ctx, "DIALOG", vars);
        if event.eq_ignore_ascii_case("close") {
            engine.close_dialog_state(&dialog);
        }
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
        halted
    })
    .await
    .unwrap_or(false)
}

/// A notify-list nick came online (`on NOTIFY`) or went offline (`on UNOTIFY`).
/// The frontend calls this from its ISON diff; `$nick` is the affected nick.
#[tauri::command]
pub fn script_notify(
    app: AppHandle,
    server_id: String,
    network: String,
    nick: String,
    online: bool,
) {
    // `on NOTIFY`/`on UNOTIFY` may call `$input`; run off the main thread so a
    // blocking prompt can't freeze WebView2.
    tauri::async_runtime::spawn_blocking(move || {
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
        let vars = EventVars {
            nick: nick.clone(),
            target: nick,
            ..Default::default()
        };
        let actions = app.state::<ScriptEngine>().dispatch_event(&ctx, kind, vars);
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    });
}

/// Maps a settings string to the auto-list it targets.
fn auto_kind(s: &str) -> Option<users::AutoKind> {
    match s.to_ascii_lowercase().as_str() {
        "aop" => Some(users::AutoKind::Aop),
        "avoice" => Some(users::AutoKind::Avoice),
        "protect" => Some(users::AutoKind::Protect),
        _ => None,
    }
}

/// A JSON snapshot of the user + auto-op/voice/protect lists (settings UI).
#[tauri::command]
pub fn users_snapshot(app: AppHandle) -> String {
    app.state::<ScriptEngine>().users_json()
}

/// Add/replace a user-list entry (`/auser`).
#[tauri::command]
pub fn users_set(app: AppHandle, levels: String, address: String, info: String) {
    let dir = script_data_dir(&app);
    app.state::<ScriptEngine>()
        .edit_users(&dir, |u| u.add(&levels, &address, &info, false));
}

/// Remove a user-list entry (`/ruser`).
#[tauri::command]
pub fn users_remove(app: AppHandle, address: String) {
    let dir = script_data_dir(&app);
    app.state::<ScriptEngine>()
        .edit_users(&dir, |u| u.remove("", &address));
}

/// Toggle an auto-list on/off.
#[tauri::command]
pub fn users_auto_toggle(app: AppHandle, kind: String, on: bool) {
    if let Some(k) = auto_kind(&kind) {
        let dir = script_data_dir(&app);
        app.state::<ScriptEngine>()
            .edit_users(&dir, |u| u.auto_toggle(k, on));
    }
}

/// Add an auto-list entry.
#[tauri::command]
pub fn users_auto_add(
    app: AppHandle,
    kind: String,
    address: String,
    channels: Vec<String>,
    network: String,
) {
    if let Some(k) = auto_kind(&kind) {
        let dir = script_data_dir(&app);
        app.state::<ScriptEngine>()
            .edit_users(&dir, |u| u.auto_add(k, &address, channels, network));
    }
}

/// Remove an auto-list entry.
#[tauri::command]
pub fn users_auto_remove(app: AppHandle, kind: String, address: String) {
    if let Some(k) = auto_kind(&kind) {
        let dir = script_data_dir(&app);
        app.state::<ScriptEngine>()
            .edit_users(&dir, |u| u.auto_remove(k, &address));
    }
}

/// Returns the names of all open script sockets (for `/socklist`).
#[tauri::command]
pub fn script_sockets(socks: State<'_, socket::SocketManager>) -> Vec<String> {
    socks.names()
}

/// Records the currently-focused window (`$active`) and its connection
/// (`$activecid`). The frontend calls this whenever the active buffer changes.
#[tauri::command]
pub fn script_set_active(
    app: AppHandle,
    engine: State<'_, ScriptEngine>,
    name: String,
    server_id: String,
) {
    let changed = engine.set_active(&name);
    engine.set_active_conn(&server_id);
    engine.set_active_win(&server_id, &name);
    if changed {
        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|store| store.get(&server_id))
            .unwrap_or_default();
        let my_nick = state.nick.clone();
        let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
        let ctx = RunCtx {
            my_nick: &my_nick,
            network: &network,
            server: &server,
            data_dir: script_data_dir(&app),
            state,
        };
        let vars = EventVars {
            target: name.clone(),
            chan: is_channel(&name).then_some(name).unwrap_or_default(),
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx, "ACTIVE", vars);
        apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
    }
}

/// Fires `on TABCOMP` before the frontend performs its normal nickname
/// completion. A halted handler suppresses that default completion.
#[tauri::command]
pub async fn script_run_tabcomp(
    app: AppHandle,
    server_id: String,
    target: String,
    text: String,
) -> bool {
    tauri::async_runtime::spawn_blocking(move || {
        let engine = app.state::<ScriptEngine>();
        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|store| store.get(&server_id))
            .unwrap_or_default();
        let my_nick = state.nick.clone();
        let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
        let ctx = RunCtx {
            my_nick: &my_nick,
            network: &network,
            server: &server,
            data_dir: script_data_dir(&app),
            state,
        };
        let vars = EventVars {
            chan: is_channel(&target)
                .then_some(target.clone())
                .unwrap_or_default(),
            target: target.clone(),
            text: text.clone(),
            params: text.split_whitespace().map(String::from).collect(),
            ..Default::default()
        };
        let (actions, halted) = engine.dispatch_event_halt(&ctx, "TABCOMP", vars);
        apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
        halted
    })
    .await
    .unwrap_or(false)
}

#[tauri::command]
pub async fn script_run_key(
    app: AppHandle,
    server_id: String,
    target: String,
    kind: String,
    key: String,
    key_val: u32,
    key_repeat: bool,
    modifiers: String,
    text: String,
) -> bool {
    tauri::async_runtime::spawn_blocking(move || {
        if !matches!(
            kind.to_ascii_uppercase().as_str(),
            "KEYDOWN" | "KEYUP" | "CHAR"
        ) {
            return false;
        }
        let engine = app.state::<ScriptEngine>();
        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|store| store.get(&server_id))
            .unwrap_or_default();
        let my_nick = state.nick.clone();
        let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
        let ctx = RunCtx {
            my_nick: &my_nick,
            network: &network,
            server: &server,
            data_dir: script_data_dir(&app),
            state,
        };
        let vars = EventVars {
            target: target.clone(),
            chan: is_channel(&target).then_some(target).unwrap_or_default(),
            text: text.clone(),
            params: vec![key.clone(), modifiers, text],
            key_char: key
                .chars()
                .count()
                .eq(&1)
                .then_some(key)
                .unwrap_or_default(),
            key_val: Some(key_val),
            key_repeat,
            ..Default::default()
        };
        let (actions, halted) = engine.dispatch_event_halt(&ctx, &kind, vars);
        apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
        halted
    })
    .await
    .unwrap_or(false)
}

/// Dispatches `on HOTLINK` for a word under the pointer in a rendered buffer.
#[tauri::command]
pub async fn script_run_hotlink(
    app: AppHandle,
    server_id: String,
    target: String,
    word: String,
    line_text: String,
    action: String,
    line: usize,
    position: usize,
) -> bool {
    tauri::async_runtime::spawn_blocking(move || {
        if word.is_empty()
            || !matches!(
                action.as_str(),
                "mouse" | "sclick" | "dclick" | "rclick" | "uclick"
            )
        {
            return false;
        }
        let engine = app.state::<ScriptEngine>();
        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|store| store.get(&server_id))
            .unwrap_or_default();
        let my_nick = state.nick.clone();
        let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
        let ctx = RunCtx {
            my_nick: &my_nick,
            network: &network,
            server: &server,
            data_dir: script_data_dir(&app),
            state,
        };
        let vars = EventVars {
            chan: is_channel(&target)
                .then_some(target.clone())
                .unwrap_or_default(),
            target: target.clone(),
            text: word.clone(),
            params: vec![word],
            hotlink_event: action,
            hotlink_line_text: line_text,
            hotlink_line: line,
            hotlink_pos: position,
            ..Default::default()
        };
        let (actions, halted) = engine.dispatch_event_halt(&ctx, "HOTLINK", vars);
        apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
        halted
    })
    .await
    .unwrap_or(false)
}

/// The events a finished audio file fires, in order. `/splay` raises a
/// sound-type event chosen by extension, and mIRC pairs each of those with the
/// generic `on SONGEND`; `on PLAYEND` comes from `/play` (a text file) and has
/// no SONGEND counterpart. An unrecognised kind fires nothing.
fn audio_end_event_chain(kind: &str) -> &'static [&'static str] {
    match kind.to_ascii_uppercase().as_str() {
        "WAVEEND" => &["WAVEEND", "SONGEND"],
        "MIDIEND" => &["MIDIEND", "SONGEND"],
        "MP3END" => &["MP3END", "SONGEND"],
        "SONGEND" => &["SONGEND"],
        "PLAYEND" => &["PLAYEND"],
        _ => &[],
    }
}

#[tauri::command]
pub fn script_dispatch_audio_end(app: AppHandle, server_id: String, kind: String, path: String) {
    let kinds = audio_end_event_chain(&kind);
    if kinds.is_empty() {
        return;
    }
    let engine = app.state::<ScriptEngine>();
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|store| store.get(&server_id))
        .unwrap_or_default();
    let my_nick = state.nick.clone();
    let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: &server,
        data_dir: script_data_dir(&app),
        state,
    };
    let filename = std::path::Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&path)
        .to_string();
    for kind in kinds {
        let actions = engine.dispatch_event(
            &ctx,
            kind,
            EventVars {
                filename: filename.clone(),
                text: filename.clone(),
                params: vec![filename.clone()],
                ..Default::default()
            },
        );
        apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
    }
}

#[tauri::command]
pub fn script_dispatch_dns(app: AppHandle, server_id: String, host: String, ips: Vec<String>) {
    let engine = app.state::<ScriptEngine>();
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|store| store.get(&server_id))
        .unwrap_or_default();
    let my_nick = state.nick.clone();
    let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
    let ctx = RunCtx {
        my_nick: &my_nick,
        network: &network,
        server: &server,
        data_dir: script_data_dir(&app),
        state,
    };
    let vars = EventVars {
        dns_query: host.clone(),
        dns_ips: ips,
        text: host.clone(),
        params: vec![host],
        ..Default::default()
    };
    let actions = engine.dispatch_event(&ctx, "DNS", vars);
    apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
}

/// Records native-window and UI preference state used by client identifiers.
#[tauri::command]
pub async fn script_set_client_window_state(
    app: AppHandle,
    label: String,
    focused: bool,
    app_state: String,
) {
    let app_state = match app_state.as_str() {
        "minimized" | "maximized" | "full" | "normal" | "hidden" | "tray" => app_state,
        _ => "normal".into(),
    };
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let engine = app.state::<ScriptEngine>();
        if !engine.set_client_window_state(&label, focused, &app_state) {
            return;
        }
        let server_id = engine.active_connection().unwrap_or_default();
        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|store| store.get(&server_id))
            .unwrap_or_default();
        let my_nick = state.nick.clone();
        let (network, server) = engine.connection_context(&server_id).unwrap_or_default();
        let ctx = RunCtx {
            my_nick: &my_nick,
            network: &network,
            server: &server,
            data_dir: script_data_dir(&app),
            state,
        };
        let actions = engine.dispatch_event(&ctx, "APPACTIVE", EventVars::default());
        apply_actions(&app, &server_id, &my_nick, &network, &server, actions);
    })
    .await;
}

#[tauri::command]
pub fn script_set_client_preferences(
    engine: State<'_, ScriptEngine>,
    dark_mode: bool,
    notify_list: Vec<String>,
    notify_online: Vec<String>,
    ignore_list: Vec<String>,
    highlight_list: Vec<String>,
    font_list: Vec<String>,
) {
    engine.set_client_preferences(
        dark_mode,
        notify_list,
        notify_online,
        ignore_list,
        highlight_list,
        font_list,
    );
}

#[tauri::command]
pub fn script_set_client_editbox(
    engine: State<'_, ScriptEngine>,
    target: String,
    text: String,
    start: usize,
    end: usize,
) {
    engine.set_client_editbox(&target, &text, start, end);
}

#[tauri::command]
pub fn script_set_client_unread_windows(engine: State<'_, ScriptEngine>, windows: Vec<String>) {
    engine.set_client_unread_windows(windows);
}

#[tauri::command]
pub fn script_set_client_ui_state(
    engine: State<'_, ScriptEngine>,
    toolbar: bool,
    treebar: bool,
    switchbar: bool,
    menubar: bool,
    tips: bool,
) {
    engine.set_client_ui_state(toolbar, treebar, switchbar, menubar, tips);
}

#[tauri::command]
pub fn script_set_client_compat_state(
    engine: State<'_, ScriptEngine>,
    desktop_width: u32,
    desktop_height: u32,
    sound_enabled: bool,
    sound_volume: f64,
    do_not_disturb: bool,
    self_color: String,
) {
    engine.set_client_compat_state(
        desktop_width,
        desktop_height,
        sound_enabled,
        sound_volume,
        do_not_disturb,
        &self_color,
    );
}

/// The UI opened a window/buffer — assign its `$wid` and fire `on OPEN`.
#[tauri::command]
pub fn script_window_open(app: AppHandle, server_id: String, name: String) {
    app.state::<ScriptEngine>().window_open(&server_id, &name);
    fire_window_event(app, server_id, name, "OPEN");
}

/// The UI closed a window/buffer — release its `$wid` and fire `on CLOSE`.
#[tauri::command]
pub fn script_window_close(app: AppHandle, server_id: String, name: String) {
    app.state::<ScriptEngine>().window_close(&server_id, &name);
    fire_window_event(app, server_id, name, "CLOSE");
}

/// Dispatches `on OPEN`/`on CLOSE` for a window. A plain nick is a query window
/// (empty `$chan` so the `?` target matches, `$nick` = the other party); a
/// channel / `@window` keeps its name as `$chan` for `#` / `@name` targets. The
/// status window is always present, so mIRC fires neither for it.
fn fire_window_event(app: AppHandle, server_id: String, name: String, kind: &'static str) {
    if name.eq_ignore_ascii_case("Status Window") {
        return;
    }
    // An `on OPEN`/`on CLOSE` handler may call `$input`; run off the main thread so
    // a blocking prompt can't freeze WebView2.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app
            .try_state::<crate::irc::state::StateStore>()
            .map(|s| s.get(&server_id))
            .unwrap_or_default();
        let my_nick = state.nick.clone();
        let is_query = !is_channel(&name) && !name.starts_with('@') && !name.starts_with('=');
        let vars = EventVars {
            nick: if is_query {
                name.clone()
            } else {
                String::new()
            },
            chan: if is_query {
                String::new()
            } else {
                name.clone()
            },
            target: name.clone(),
            ..Default::default()
        };
        let ctx = RunCtx {
            my_nick: &my_nick,
            network: "",
            server: "",
            data_dir: script_data_dir(&app),
            state,
        };
        let actions = app.state::<ScriptEngine>().dispatch_event(&ctx, kind, vars);
        apply_actions(&app, &server_id, &my_nick, "", "", actions);
    });
}

/// Runs a typed command line through the engine (built-in script commands like
/// /sockopen, /timer, /hadd, a user alias, or — failing those — a raw IRC line).
/// Used for input the frontend's own `/command` handling doesn't cover.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_run_command(
    app: AppHandle,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    command: String,
    args: String,
    from_editbox: Option<bool>,
) {
    // The line may resolve to an alias that calls `$input`/`$?`, which blocks the
    // run waiting for the UI dialog. Run it on a blocking thread so the main
    // thread (and WebView2) stay responsive — a sync command blocking the main
    // thread deadlocks the webview and freezes the dialog.
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
        let line = if args.is_empty() {
            command
        } else {
            format!("{command} {args}")
        };
        let actions = if from_editbox.unwrap_or(true) {
            engine.run_editbox_command(&ctx, &target, &line, &[])
        } else {
            engine.run_command(&ctx, &target, &line, &[])
        };
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    });
}

/// Fires `on INPUT` handlers for a line the user typed (the line is still sent
/// normally by the caller; this just lets scripts react to it).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn script_run_input(
    app: AppHandle,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    text: String,
) -> bool {
    // An `on INPUT` handler may call `$input`/`$?`, which blocks the run. This
    // command is async (so it runs off the main thread) and does the work on a
    // blocking thread, so a prompt can't freeze WebView2 — while still returning
    // `halted` to the caller, which awaits it before sending the line.
    tauri::async_runtime::spawn_blocking(move || {
        let engine = app.state::<ScriptEngine>();
        engine.reset_idle();
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
        let chan = if is_channel(&target) {
            target.clone()
        } else {
            String::new()
        };
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
    })
    .await
    .unwrap_or(false)
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
    source: Option<String>,
    context: String,
    menu_context: Option<String>,
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
        let actions = engine.run_popup_command(
            &ctx,
            &target,
            &command,
            &params,
            &snicks,
            source.as_deref().unwrap_or(""),
            &context,
            menu_context.as_deref().unwrap_or("window"),
        );
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    });
}

/// Runs a command attached to a custom-window mouse menu entry.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn script_window_mouse(
    app: AppHandle,
    server_id: String,
    target: String,
    my_nick: String,
    network: String,
    command: String,
    source: String,
    x: i32,
    y: i32,
    list_line: u32,
    key: u32,
) {
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
        let actions =
            engine.run_window_mouse_command(&ctx, &target, &command, &source, x, y, list_line, key);
        apply_actions(&app, &server_id, &my_nick, &network, "", actions);
    });
}

/// Drives event handlers from a UI event produced by the connection. Returns
/// the resulting outgoing lines and echo events to apply.
pub fn drive_event(engine: &ScriptEngine, ctx: &RunCtx, ev: &UiEvent) -> Vec<Action> {
    drive_event_halt(engine, ctx, ev).0
}

/// Like [`drive_event`], also reports whether a matching handler suppressed
/// mIRC's default display for this event (`^` + `/halt`, `/haltdef`, or `/halt`).
pub fn drive_event_halt(engine: &ScriptEngine, ctx: &RunCtx, ev: &UiEvent) -> (Vec<Action>, bool) {
    drive_event_halt_raw(engine, ctx, ev, None)
}

/// Event dispatch retaining the exact server line that produced `ev` for
/// `$rawmsg`, `$rawbytes`, `$msgtags`, and `$msgstamp`.
pub fn drive_event_halt_raw(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    ev: &UiEvent,
    raw: Option<&RawEventContext>,
) -> (Vec<Action>, bool) {
    let (kind, vars) = match ev {
        UiEvent::Message {
            kind,
            from,
            target,
            text,
            ..
        } => {
            let from = from.clone().unwrap_or_default();
            if ctx.state.isupport.names_equal(&from, ctx.my_nick) {
                return (Vec::new(), false);
            }
            let channel_target = ctx.state.isupport.channel_target(target);
            let chan = channel_target
                .map(|bare| {
                    ctx.state
                        .channels
                        .iter()
                        .find(|channel| ctx.state.isupport.names_equal(&channel.name, bare))
                        .map(|channel| channel.name.clone())
                        .unwrap_or_else(|| bare.to_string())
                })
                .unwrap_or_default();
            let is_chan = !chan.is_empty();
            let reply = if is_chan { chan.clone() } else { from.clone() };
            // `$nonstdmsg`: a server normally addresses a PRIVMSG/NOTICE either
            // to a channel we are on or to our own nick. Anything else is the
            // non-standard combination mIRC flags.
            let nonstdmsg = !is_chan && !ctx.state.isupport.names_equal(target, ctx.my_nick);
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
                    nonstdmsg,
                    ..Default::default()
                };
                let sound = (ckind == "CTCP")
                    .then(|| body.strip_prefix("SOUND "))
                    .flatten()
                    .and_then(|value| value.split_whitespace().next())
                    .map(str::to_string);
                let sound_context = vars.clone();
                let (mut actions, mut halted) =
                    engine.dispatch_event_default_halt_raw(ctx, ckind, vars, raw);
                if let Some(filename) =
                    sound.filter(|filename| !eval::sandbox_path(&ctx.data_dir, filename).is_file())
                {
                    let vars = EventVars {
                        nick: sound_context.nick,
                        chan: sound_context.chan,
                        target: sound_context.target,
                        text: filename.clone(),
                        params: vec![filename.clone()],
                        filename,
                        ..Default::default()
                    };
                    let (more, event_halted) =
                        engine.dispatch_event_default_halt_raw(ctx, "NOSOUND", vars, raw);
                    actions.extend(more);
                    halted |= event_halted;
                }
                return (actions, halted);
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
                nonstdmsg,
                ..Default::default()
            };
            (kind, vars)
        }
        UiEvent::Join { channel, nick, .. } => (
            "JOIN",
            EventVars {
                nick: nick.clone(),
                chan: channel.clone(),
                target: channel.clone(),
                ..Default::default()
            },
        ),
        UiEvent::Part {
            channel,
            nick,
            reason,
            ..
        } => (
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
        UiEvent::Kick {
            channel,
            nick,
            by,
            reason,
            ..
        } => (
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
        UiEvent::Topic {
            channel,
            topic,
            set_by,
            ..
        } => {
            // Only fire on a live change (set_by present), not the join-time
            // RPL_TOPIC snapshot.
            let Some(setter) = set_by else {
                return (Vec::new(), false);
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
        UiEvent::Mode {
            target, modes, by, ..
        } => {
            let setter = by.clone().unwrap_or_default();
            let server_source = raw_source_is_server(raw);
            let Some(bare_target) = ctx.state.isupport.channel_target(target) else {
                // A user-mode change (only ever your own) fires `on USERMODE`.
                let vars = EventVars {
                    nick: setter,
                    target: target.clone(),
                    params: words(modes),
                    text: modes.clone(),
                    ..Default::default()
                };
                return engine.dispatch_event_default_halt_raw(ctx, "USERMODE", vars, raw);
            };
            let chan = ctx
                .state
                .channels
                .iter()
                .find(|channel| ctx.state.isupport.names_equal(&channel.name, bare_target))
                .map(|channel| channel.name.clone())
                .unwrap_or_else(|| bare_target.to_string());
            // Generic `on MODE` and raw `on RAWMODE` ($1- = the whole change).
            let generic = EventVars {
                nick: setter.clone(),
                chan: chan.clone(),
                target: chan.clone(),
                params: words(modes),
                text: modes.clone(),
                ..Default::default()
            };
            let (mut actions, mut halted) =
                engine.dispatch_event_default_halt_raw(ctx, "MODE", generic.clone(), raw);
            let (more, raw_halted) =
                engine.dispatch_event_default_halt_raw(ctx, "RAWMODE", generic, raw);
            actions.extend(more);
            halted |= raw_halted;
            if server_source {
                let vars = EventVars {
                    nick: setter.clone(),
                    chan: chan.clone(),
                    target: chan.clone(),
                    params: words(modes),
                    text: modes.clone(),
                    ..Default::default()
                };
                let (more, event_halted) =
                    engine.dispatch_event_default_halt_raw(ctx, "SERVERMODE", vars, raw);
                actions.extend(more);
                halted |= event_halted;
            }
            // Plus a specific event per prefix/ban change (on OP/DEOP/BAN/…),
            // with the affected nick/mask as $1 and $knick/$opnick/$bnick/…
            let mode_events = split_mode_events(modes, &ctx.state.isupport);
            let mode_count = mode_events.len();
            for (mode_offset, (kind, affected)) in mode_events.into_iter().enumerate() {
                let vars = EventVars {
                    nick: setter.clone(),
                    knick: affected.clone(),
                    chan: chan.clone(),
                    target: chan.clone(),
                    params: vec![affected.clone()],
                    text: affected.clone(),
                    mode_index: mode_offset + 1,
                    mode_count,
                    ..Default::default()
                };
                let (more, event_halted) =
                    engine.dispatch_event_default_halt_raw(ctx, kind, vars, raw);
                actions.extend(more);
                halted |= event_halted;
                if server_source && kind == "OP" {
                    let vars = EventVars {
                        nick: setter.clone(),
                        knick: affected.clone(),
                        chan: chan.clone(),
                        target: chan.clone(),
                        params: vec![affected.clone()],
                        text: affected,
                        mode_index: mode_offset + 1,
                        mode_count,
                        ..Default::default()
                    };
                    let (more, event_halted) =
                        engine.dispatch_event_default_halt_raw(ctx, "SERVEROP", vars, raw);
                    actions.extend(more);
                    halted |= event_halted;
                }
            }
            return (actions, halted);
        }
        UiEvent::Disconnected { .. } => ("DISCONNECT", EventVars::default()),
        UiEvent::Registered { .. } => ("CONNECT", EventVars::default()),
        _ => return (Vec::new(), false),
    };
    engine.dispatch_event_default_halt_raw(ctx, kind, vars, raw)
}

/// Builds the raw-event context for one decoded IRC line and its exact bytes.
pub fn raw_event_context(line: &str, bytes: &[u8]) -> RawEventContext {
    let (msg_tags_raw, raw_msg) = match line.strip_prefix('@').and_then(|s| s.split_once(' ')) {
        Some((tags, rest)) => (tags.to_string(), rest.to_string()),
        None => (String::new(), line.to_string()),
    };
    let msg_tags = msg_tags_raw
        .split(';')
        .filter(|tag| !tag.is_empty())
        .map(|tag| match tag.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string(), true),
            None => (tag.to_string(), String::new(), false),
        })
        .collect::<Vec<_>>();
    let msg_stamp = msg_tags
        .iter()
        .find(|(key, _, _)| {
            key == "time" || key == "znc.in/server-time-iso" || key == "server-time"
        })
        .map(|(_, value, _)| value.clone())
        .unwrap_or_default();
    RawEventContext {
        raw_msg,
        raw_bytes: bytes.to_vec(),
        msg_tags,
        msg_tags_raw,
        msg_stamp,
    }
}

/// Runs `on PARSELINE` for an incoming or outgoing line and separates its
/// replacement/queue controls from ordinary script side effects.
pub fn dispatch_parseline(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    direction: &str,
    display_line: &str,
    bytes: &[u8],
    parse_utf: bool,
    parse_em: bool,
) -> ParseLineOutcome {
    let parse_line = if direction.eq_ignore_ascii_case("in") && parse_utf {
        bytes.iter().map(|byte| *byte as char).collect()
    } else {
        display_line.to_string()
    };
    let event = EventVars {
        text: parse_line.clone(),
        parse_line,
        parse_type: direction.to_ascii_lowercase(),
        parse_utf,
        parse_em,
        ..Default::default()
    };
    let (emitted, _, _) = engine.dispatch_event_status(ctx, "PARSELINE", event, None, None);
    let mut outcome = ParseLineOutcome::default();
    for action in emitted {
        match action {
            queued @ Action::ParseLine { queue: true, .. } => outcome.queued.push(queued),
            Action::ParseLine {
                direction: ref action_direction,
                ref bytes,
                queue: false,
                append_crlf,
                ..
            } if action_direction.eq_ignore_ascii_case(direction) => {
                outcome.current = Some(bytes.clone());
                outcome.force_crlf |= append_crlf;
            }
            Action::ParseLine { .. } => {}
            other => outcome.actions.push(other),
        }
    }
    outcome
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
    dispatch_raw_with_context(engine, ctx, command, params, None).0
}

/// Raw dispatch with full line metadata. The boolean reports any `/halt` or
/// `/haltdef`, allowing the caller to suppress the UI events derived from this
/// same server line while protocol state continues to update.
pub fn dispatch_raw_with_context(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    command: &str,
    params: Vec<String>,
    raw: Option<&RawEventContext>,
) -> (Vec<Action>, bool) {
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
    let (actions, halted, default_halted) =
        engine.dispatch_event_status(ctx, "RAW", vars, raw, None);
    (actions, halted || default_halted)
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
    dispatch_named_with_context(engine, ctx, kind, nick, text, None)
}

pub fn dispatch_named_with_context(
    engine: &ScriptEngine,
    ctx: &RunCtx,
    kind: &str,
    nick: &str,
    text: &str,
    raw: Option<&RawEventContext>,
) -> Vec<Action> {
    let vars = EventVars {
        nick: nick.to_string(),
        params: words(text),
        text: text.to_string(),
        ..Default::default()
    };
    engine.dispatch_event_status(ctx, kind, vars, raw, None).0
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
    fn client_state_and_notify_identifiers_follow_frontend_state() {
        let engine = ScriptEngine::new();
        assert!(engine.set_client_window_state("main", true, "maximized"));
        engine.set_client_preferences(
            true,
            vec!["Alice".into(), "Bob".into()],
            vec!["bob".into()],
            vec!["Bad!*@*".into()],
            vec!["jIRC".into()],
            vec!["Consolas".into(), "Verdana".into()],
        );
        engine.set_client_ui_state(true, true, false, true, true);
        engine.set_active_win("server", "#jirc");
        engine.set_client_editbox("#jirc", "hello world", 2, 7);
        engine.set_client_unread_windows(vec!["\u{1e}#busy".into()]);
        engine.load(
            "alias inspect { echo -a $appactive $appstate $fullscreen $darkmode $toolbar $treebar $switchbar $menubar $tips $markasread(#jirc) $markasread(#busy) $notify $notify(0) $notify(1) $notify(bob) $notify(Alice).ison $notify(Bob).ison $ignore(0) $ignore(1) $ignore(Bad!*@*) $highlight(0) $highlight(1).text $font(0) $font(2) $editbox(#jirc) $editbox(#jirc).selstart $editbox(#jirc).selend }",
        );

        assert_eq!(
            engine.run_alias(&ctx(), "", "inspect", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "$true maximized $false $true on on off on on $true $false $true 2 Alice 2 $false $true 1 Bad!*@* 1 1 jIRC 2 Verdana hello world 2 7".into(),
            }]
        );

        assert!(!engine.set_client_window_state("detached-one", true, "normal"));
        assert!(!engine.set_client_window_state("main", false, "normal"));
        engine.load("alias active { echo -a $appactive $appstate }");
        assert_eq!(
            engine.run_alias(&ctx(), "", "active", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "$true normal".into(),
            }]
        );
        assert!(engine.set_client_window_state("detached-one", false, "hidden"));
        assert_eq!(
            engine.run_alias(&ctx(), "", "active", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "$false normal".into(),
            }]
        );
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
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "[97 40 98 93 99 ]".into()
            }]
        );
    }

    #[test]
    fn regsubex_keeps_unknown_escape_backslash() {
        // mIRC keeps an unrecognised escape literal: `\*` stays "\*" (used as a
        // wildcard to tell an escape sequence from a plain char). Input "a\0b" ->
        // 'a' and 'b' are plain (asc 97/98), only "\0" matches the "\*" wildcard.
        let engine = ScriptEngine::new();
        engine.load(r#"alias t { /echo -a [ $+ $regsubex(a\0b,/(\\?.)/g,$iif(\* iswm \1,ESC,$asc(\1)) $+ $chr(32)) $+ ] }"#);
        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "[97 ESC 98 ]".into()
            }]
        );
    }

    #[test]
    fn input_returns_default_without_ui() {
        // With no UI backend installed (NoInput), $input returns its default (4th
        // arg) so a non-interactive/test run proceeds. The production backend
        // shows a dialog and blocks for the answer.
        let engine = ScriptEngine::new();
        engine.load("alias t { /echo -a [ $+ $input(msg,e,title,thedefault) $+ ] }");
        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "[thedefault]".into()
            }]
        );
    }

    #[test]
    fn alias_sends_message() {
        let engine = ScriptEngine::new();
        engine.load("alias hi { /msg $chan hello $me }");
        let actions = engine.run_alias(&ctx(), "#test", "hi", "");
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #test :hello me".into())]
        );
    }

    #[test]
    fn aliases_override_builtins_and_bang_bypasses_them() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias join { /msg #audit custom $1- }
             alias frob { /msg #audit alias $1- }
             alias t {
               join #room
               !join #room
               frob one
               !frob two
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#audit", "t", ""),
            vec![
                Action::Send("PRIVMSG #audit :custom #room".into()),
                Action::Send("JOIN #room".into()),
                Action::Send("PRIVMSG #audit :alias one".into()),
                Action::Send("FROB two".into()),
            ]
        );
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
    fn nested_alias_missing_parameter_does_not_leak_from_caller() {
        let engine = ScriptEngine::new();
        engine.load(
            r#"
            alias outer { rekey i7.room %#room - }
            alias -l rekey {
              var %sock = $1, %chan = $2, %flags = $3, %kicknick = $4
              /msg #d count=$0 fourth=[$4] local=[%kicknick]
            }
            "#,
        );

        let actions = engine.run_alias(&ctx(), "#here", "outer", "one two three Snue");
        assert_eq!(
            actions,
            vec![Action::Send(
                "PRIVMSG #d :count=3 fourth=[] local=[]".into()
            )]
        );
    }

    #[test]
    fn aliases_cannot_recurse_directly_or_indirectly() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias direct { msg #c before | direct | msg #c after }\n\
             alias first { msg #c first | second }\n\
             alias second { msg #c second | first }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "direct", ""),
            vec![
                Action::Send("PRIVMSG #c :before".into()),
                Action::Send("PRIVMSG #c :after".into()),
            ]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "first", ""),
            vec![
                Action::Send("PRIVMSG #c :first".into()),
                Action::Send("PRIVMSG #c :second".into()),
            ]
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
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #c :from-helper".into())]
        );
        // invoked directly as a user command: not exposed
        assert!(!engine.has_alias("helper"));
        assert!(engine.run_alias(&ctx(), "#c", "helper", "").is_empty());
        // a normal (global) alias is still user-callable
        assert!(engine.has_alias("go"));
    }

    #[test]
    fn local_aliases_are_isolated_to_their_defining_script_file() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "alias -l helper { msg #c local-one }\n\
                 alias -l value { return local-value }\n\
                 on *:TEXT:one:#:{ helper | msg #c $value }"
                    .into(),
            ),
            (
                "two.mrc".into(),
                "alias helper { msg #c global-two }\n\
                 alias value { return global-value }\n\
                 on *:TEXT:two:#:{ helper | msg #c $value }"
                    .into(),
            ),
        ]);
        let text = |body: &str| EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: body.into(),
            params: vec![body.into()],
            ..Default::default()
        };

        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", text("one")),
            vec![
                Action::Send("PRIVMSG #c :local-one".into()),
                Action::Send("PRIVMSG #c :local-value".into()),
            ]
        );
        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", text("two")),
            vec![
                Action::Send("PRIVMSG #c :global-two".into()),
                Action::Send("PRIVMSG #c :global-value".into()),
            ]
        );
        assert!(engine.has_alias("helper"));
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "helper", ""),
            vec![Action::Send("PRIVMSG #c :global-two".into())]
        );
    }

    #[test]
    fn popup_commands_keep_their_defining_script_source() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "alias -l helper { msg #c from-one }\nmenu channel { One:helper }".into(),
            ),
            (
                "two.mrc".into(),
                "alias -l helper { msg #c from-two }\nmenu channel { Two:helper }".into(),
            ),
        ]);
        let items = engine.popups_evaluated(&ctx(), "channel", "", "#c");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].source, "one.mrc");
        assert_eq!(items[1].source, "two.mrc");
        assert_eq!(
            engine.run_popup_command(
                &ctx(),
                "#c",
                &items[1].command,
                &[],
                &[],
                &items[1].source,
                "channel",
                "window",
            ),
            vec![Action::Send("PRIVMSG #c :from-two".into())]
        );
    }

    #[test]
    fn popup_identifiers_expose_menu_type_and_window_context() {
        let engine = ScriptEngine::new();
        engine.load(
            "menu channel { $+($menu,|,$menutype,|,$menucontext):noop }\n\
             menu @tools { $+($menu,|,$menutype,|,$menucontext):noop }",
        );
        let channel = engine.popups_evaluated(&ctx(), "channel", "", "#c");
        assert_eq!(channel[0].label, "channel|channel|window");
        let custom = engine.popups_evaluated(&ctx(), "@tools", "", "@tools");
        assert_eq!(custom[0].label, "@tools|custom|window");

        let command = engine.run_popup_command(
            &ctx(),
            "#c",
            "echo -a $menu $menutype $menucontext",
            &[],
            &[],
            "",
            "channel",
            "window",
        );
        assert!(
            matches!(command.as_slice(), [Action::Echo { text, .. }] if text == "channel channel window")
        );
    }

    #[test]
    fn scripted_tips_create_query_update_and_close() {
        let engine = ScriptEngine::new();
        engine.set_client_ui_state(true, true, false, true, true);
        engine.load(
            "alias create_tip { echo -a $tip(counter,Count Down,10 seconds,5,,,clicked,0) }\n\
             alias inspect_tip { echo -a $tip(0) $tip(counter) $tip(counter).title $tip(counter).text $tip(counter).delay $tip(counter).alias $tip(counter).wid $tip(counter).cid }",
        );
        let created = engine.run_alias(&ctx(), "#c", "create_tip", "");
        assert!(
            matches!(created.as_slice(), [Action::ClientCommand { command, .. }, Action::Echo { text, .. }] if command == "tip-create" && text == "1")
        );
        let inspected = engine.run_alias(&ctx(), "#c", "inspect_tip", "");
        assert!(
            matches!(inspected.as_slice(), [Action::Echo { text, .. }] if text.starts_with("1 1 Count Down 10 seconds ") && text.ends_with(" clicked 0 0"))
        );

        let updated = engine.run_command(&ctx(), "#c", "/tip -t counter 9 seconds", &[]);
        assert!(
            matches!(updated.as_slice(), [Action::ClientCommand { command, args, .. }] if command == "tip-update" && args.ends_with("9 seconds"))
        );
        let closed = engine.run_command(&ctx(), "#c", "/tip -c counter", &[]);
        assert!(
            matches!(closed.as_slice(), [Action::ClientCommand { command, args, .. }] if command == "tip-close" && args == "counter")
        );
    }

    #[test]
    fn menubar_and_tips_commands_report_current_state() {
        let engine = ScriptEngine::new();
        engine.set_client_ui_state(true, true, true, true, false);
        assert!(matches!(
            engine.run_command(&ctx(), "", "/menubar", &[]).as_slice(),
            [Action::Echo { text, .. }] if text == "Menubar is on"
        ));
        assert!(matches!(
            engine.run_command(&ctx(), "", "/tips", &[]).as_slice(),
            [Action::Echo { text, .. }] if text == "Tips are off"
        ));
    }

    #[test]
    fn fromeditbox_survives_into_aliases_called_from_user_input() {
        let engine = ScriptEngine::new();
        engine.load("alias origin { echo -a $fromeditbox }");
        assert!(matches!(
            engine.run_editbox_command(&ctx(), "#c", "origin", &[]).as_slice(),
            [Action::Echo { text, .. }] if text == "$true"
        ));
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "origin", &[]).as_slice(),
            [Action::Echo { text, .. }] if text == "$false"
        ));
    }

    #[test]
    fn dedicated_popup_files_are_ordered_before_remote_script_menus() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "aaa-remote.mrc".into(),
                "menu channel { Remote:/echo remote }".into(),
            ),
            (
                "popups-channel.mrc".into(),
                "menu channel { Mine:/echo mine }".into(),
            ),
            (
                "zzz-remote.mrc".into(),
                "menu channel { Last:/echo last }".into(),
            ),
        ]);
        let items = engine.popups_evaluated(&ctx(), "channel", "", "#c");
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Mine", "Remote", "Last"]
        );
        assert_eq!(items[0].source, "popups-channel.mrc");
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
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #test :pong bob".into())]
        );
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
        engine.load("#g off\non *:TEXT:*:#:{ msg #c got }\n#g end\nalias en { enable #g }");
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
        // `/alias <name> <cmd>` defines (evaluating once); `/alias <name>` removes.
        engine.load("alias mk { alias greet /msg # hi $nick | alias greet }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "mk", ""),
            vec![
                Action::DefineAlias {
                    name: "greet".into(),
                    command: Some("/msg #c hi me".into()),
                    file: None,
                    local: false,
                },
                Action::DefineAlias {
                    name: "greet".into(),
                    command: None,
                    file: None,
                    local: false,
                },
            ]
        );
    }

    #[test]
    fn alias_command_accepts_local_filename_and_slash_forms() {
        let engine = ScriptEngine::new();
        engine.load("alias mk { alias -l helpers.mrc /local /echo $!1 | alias aliases.mrc /gone }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "mk", ""),
            vec![
                Action::DefineAlias {
                    name: "local".into(),
                    command: Some("/echo $1".into()),
                    file: Some("helpers.mrc".into()),
                    local: true,
                },
                Action::DefineAlias {
                    name: "gone".into(),
                    command: None,
                    file: Some("aliases.mrc".into()),
                    local: false,
                },
            ]
        );
    }

    #[test]
    fn sound_and_splay_produce_safe_local_audio_actions() {
        let engine = ScriptEngine::new();
        let actions = engine.run_command(&ctx(), "#c", "/splay alert.wav", &[]);
        assert!(matches!(
            &actions[..],
            [Action::Audio { operation, path, .. }]
                if operation == "play" && path.ends_with("alert.wav")
        ));

        let actions = engine.run_command(&ctx(), "#c", "/sound #c alert.wav hello", &[]);
        assert!(
            matches!(
                &actions[..],
                [
                    Action::Send(line),
                    Action::Audio { operation, path, .. }
                ] if line == "PRIVMSG #c :\u{1}SOUND alert.wav hello\u{1}"
                    && operation == "play"
                    && path.ends_with("alert.wav")
            ),
            "unexpected /sound actions: {actions:?}"
        );
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/splay -p", &[]),
            vec![Action::Audio {
                operation: "pause".into(),
                path: String::new(),
                end_event: "WAVEEND".into(),
            }]
        );
    }

    #[test]
    fn ui_commands_produce_client_actions_with_context() {
        let engine = ScriptEngine::new();
        for (command, args) in [
            ("editbox", "-af hello"),
            ("timestamp", "divider"),
            ("switchbar", "on"),
            ("treebar", "off"),
            ("font", "14 Cascadia Code"),
            ("clearall", "-nq"),
            ("close", "-m nick"),
        ] {
            assert_eq!(
                engine.run_command(&ctx(), "#c", &format!("/{command} {args}"), &[]),
                vec![Action::ClientCommand {
                    command: command.into(),
                    args: args.into(),
                    current_target: "#c".into(),
                }]
            );
        }
    }

    #[test]
    fn toolbar_add_update_delete_and_clear_produce_ui_actions() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias tools { \
               toolbar -a Cow \"Moo moo!\" 🐄 \"/echo -a clicked $!1\" | \
               toolbar -t Cow \"New tip\" | \
               toolbar -l Cow \"/echo -a changed\" | \
               toolbar -d Cow | toolbar -c \
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "tools", ""),
            vec![
                Action::Toolbar {
                    op: "upsert".into(),
                    name: "Cow".into(),
                    tooltip: "Moo moo!".into(),
                    icon: "🐄".into(),
                    command: "/echo -a clicked $1".into(),
                    source: "<memory>".into(),
                },
                Action::Toolbar {
                    op: "tooltip".into(),
                    name: "Cow".into(),
                    tooltip: "New tip".into(),
                    icon: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
                Action::Toolbar {
                    op: "command".into(),
                    name: "Cow".into(),
                    tooltip: String::new(),
                    icon: String::new(),
                    command: "/echo -a changed".into(),
                    source: "<memory>".into(),
                },
                Action::Toolbar {
                    op: "delete".into(),
                    name: "Cow".into(),
                    tooltip: String::new(),
                    icon: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
                Action::Toolbar {
                    op: "clear".into(),
                    name: String::new(),
                    tooltip: String::new(),
                    icon: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
            ]
        );
    }

    #[test]
    fn panel_commands_produce_safe_ui_actions() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias ui { \
               panel -a stats \"Channel stats\" | \
               panel -t stats users \"42 users\" | \
               panel -b stats refresh \"Refresh\" \"/echo -a refresh $!1\" | \
               panel -d stats users | panel -d stats | panel -c \
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "ui", ""),
            vec![
                Action::Panel {
                    op: "upsert".into(),
                    panel: "stats".into(),
                    id: String::new(),
                    label: "Channel stats".into(),
                    value: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
                Action::Panel {
                    op: "text".into(),
                    panel: "stats".into(),
                    id: "users".into(),
                    label: String::new(),
                    value: "42 users".into(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
                Action::Panel {
                    op: "button".into(),
                    panel: "stats".into(),
                    id: "refresh".into(),
                    label: "Refresh".into(),
                    value: String::new(),
                    command: "/echo -a refresh $1".into(),
                    source: "<memory>".into(),
                },
                Action::Panel {
                    op: "deleteItem".into(),
                    panel: "stats".into(),
                    id: "users".into(),
                    label: String::new(),
                    value: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
                Action::Panel {
                    op: "deletePanel".into(),
                    panel: "stats".into(),
                    id: String::new(),
                    label: String::new(),
                    value: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
                Action::Panel {
                    op: "clear".into(),
                    panel: String::new(),
                    id: String::new(),
                    label: String::new(),
                    value: String::new(),
                    command: String::new(),
                    source: "<memory>".into(),
                },
            ]
        );
    }

    #[test]
    fn richer_panel_toolbar_and_ui_completion_events() {
        let engine = ScriptEngine::new();
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "/panel -i stats query \"Search\" \"hello\" \"/echo -a $!2\"", &[]).as_slice(),
            [Action::Panel { op, id, label, value, .. }]
                if op == "input" && id == "query" && label == "Search" && value == "hello"
        ));
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "/panel -k stats enabled \"Enabled\" 1 \"/echo -a $!2\"", &[]).as_slice(),
            [Action::Panel { op, id, value, .. }]
                if op == "checkbox" && id == "enabled" && value == "1"
        ));
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "/toolbar -n Cow", &[]).as_slice(),
            [Action::Toolbar { op, command, .. }] if op == "enabled" && command == "0"
        ));
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "/toolbar off", &[]).as_slice(),
            [Action::ClientCommand { command, args, .. }] if command == "toolbar" && args == "off"
        ));

        engine.load(
            "on *:KEYDOWN:*:/echo -a key $1 $2 $keychar $keyval $keyrpt\n\
             on *:WAVEEND:*:/echo -a wave $filename\n\
             on *:PLAYEND:*:/echo -a play $filename",
        );
        let key = engine.dispatch_event(
            &ctx(),
            "KEYDOWN",
            EventVars {
                params: vec!["A".into(), "ctrl".into()],
                key_char: "A".into(),
                key_val: Some(65),
                key_repeat: true,
                ..Default::default()
            },
        );
        assert_eq!(
            key,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "key A ctrl A 65 $true".into()
            }]
        );
        for (kind, expected) in [("WAVEEND", "wave alert.wav"), ("PLAYEND", "play lines.txt")] {
            let filename = if kind == "WAVEEND" {
                "alert.wav"
            } else {
                "lines.txt"
            };
            assert_eq!(
                engine.dispatch_event(
                    &ctx(),
                    kind,
                    EventVars {
                        filename: filename.into(),
                        ..Default::default()
                    }
                ),
                vec![Action::Echo {
                    target: "(status)".into(),
                    text: expected.into()
                }]
            );
        }
    }

    #[test]
    fn splay_sound_end_events_follow_the_file_type() {
        use crate::script::eval::sound_end_event;
        // /splay picks the end event from the file extension.
        assert_eq!(sound_end_event("tune.mid"), "MIDIEND");
        assert_eq!(sound_end_event("tune.MIDI"), "MIDIEND");
        assert_eq!(sound_end_event("tune.rmi"), "MIDIEND");
        assert_eq!(sound_end_event("song.Mp3"), "MP3END");
        assert_eq!(sound_end_event("alert.wav"), "WAVEEND");
        // A control operation (`/splay -p`) has no file and no end event.
        assert_eq!(sound_end_event(""), "WAVEEND");

        // Each sound event is paired with the generic `on SONGEND`; PLAYEND
        // (from /play, a text file) is not, and unknown kinds fire nothing.
        assert_eq!(audio_end_event_chain("MP3END"), ["MP3END", "SONGEND"]);
        assert_eq!(audio_end_event_chain("midiend"), ["MIDIEND", "SONGEND"]);
        assert_eq!(audio_end_event_chain("WAVEEND"), ["WAVEEND", "SONGEND"]);
        assert_eq!(audio_end_event_chain("SONGEND"), ["SONGEND"]);
        assert_eq!(audio_end_event_chain("PLAYEND"), ["PLAYEND"]);
        assert!(audio_end_event_chain("NOPE").is_empty());

        // /splay routes a real file to the matching event.
        let engine = ScriptEngine::new();
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "/splay song.mp3", &[]).as_slice(),
            [Action::Audio { operation, end_event, .. }]
                if operation == "play" && end_event == "MP3END"
        ));
        assert!(matches!(
            engine.run_command(&ctx(), "#c", "/splay tune.midi", &[]).as_slice(),
            [Action::Audio { end_event, .. }] if end_event == "MIDIEND"
        ));

        // The handlers themselves fire and fill $filename.
        engine.load(
            "on *:MIDIEND:/echo -a midi $filename\n\
             on *:MP3END:/echo -a mp3 $filename\n\
             on *:SONGEND:/echo -a song $nopath($filename)",
        );
        for (kind, filename, expected) in [
            ("MIDIEND", "tune.mid", "midi tune.mid"),
            ("MP3END", "song.mp3", "mp3 song.mp3"),
            ("SONGEND", "a/b/song.mp3", "song song.mp3"),
        ] {
            assert_eq!(
                engine.dispatch_event(
                    &ctx(),
                    kind,
                    EventVars {
                        filename: filename.into(),
                        ..Default::default()
                    }
                ),
                vec![Action::Echo {
                    target: "(status)".into(),
                    text: expected.into()
                }]
            );
        }
    }

    #[test]
    fn legacy_udpwrite_handler_does_not_disturb_sockwrite() {
        // mIRC removed `on UDPWRITE` in 7.33; UDP write completion — success or
        // error — reports through `on SOCKWRITE` instead, which is what
        // `socket.rs` fires for both socket kinds. Nothing in jIRC dispatches
        // UDPWRITE, so all a legacy handler has to do is load harmlessly and
        // leave the handlers around it intact.
        let engine = ScriptEngine::new();
        engine.load(
            "on *:UDPWRITE:sock:/echo -a udpwrite fired\n\
             on *:SOCKWRITE:sock:/echo -a sockwrite $sockname",
        );
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "SOCKWRITE",
                EventVars {
                    chan: "sock".into(),
                    target: "sock".into(),
                    ..Default::default()
                }
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "sockwrite sock".into()
            }]
        );
    }

    #[test]
    fn portable_client_commands_route_with_mirc_arguments() {
        let engine = ScriptEngine::new();
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/tnick Temporary", &[]),
            vec![Action::Send("NICK Temporary".into())]
        );
        for (line, expected, args) in [
            ("/abook -w Alice", "abook", "-w Alice"),
            ("/markasread", "markasread", ""),
            ("/channel #other", "channel", "#other"),
            ("/strip +bur-c", "strip", "+bur-c"),
            ("/pop 2 #c Bob", "pop", "2 #c Bob"),
            ("/pvoice 0 Alice", "pvoice", "0 Alice"),
            ("/qmsg hello all", "qmsg", "hello all"),
            ("/qme waves", "qme", "waves"),
        ] {
            assert!(matches!(
                engine.run_command(&ctx(), "#c", line, &[]).as_slice(),
                [Action::ClientCommand { command, args: actual, current_target }]
                    if command == expected && actual == args && current_target == "#c"
            ));
        }
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/links -nx", &[]),
            vec![Action::Send("LINKS".into())]
        );
    }

    #[test]
    fn fserve_command_preserves_sandbox_relative_arguments() {
        let engine = ScriptEngine::new();
        engine.load("alias serve { fserve $1 3 public welcome.txt }");
        assert_eq!(
            engine.run_alias(&ctx(), "", "serve", "bob"),
            vec![Action::Fserve {
                nick: "bob".into(),
                max_gets: 3,
                home: "public".into(),
                welcome: Some("welcome.txt".into()),
            }]
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
    fn standard_top_level_ctcp_definition_dispatches() {
        let engine = ScriptEngine::new();
        engine.load("ctcp *:PING:?:{ /msg $nick official $1- }");
        let request = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "me".into(),
            text: "\u{1}PING 99\u{1}".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &request),
            vec![Action::Send("PRIVMSG bob :official PING 99".into())]
        );
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
    fn standard_targetless_ctcpreply_definition_dispatches() {
        let engine = ScriptEngine::new();
        engine.load("on *:CTCPREPLY:VERSION*:/msg #audit reply $1-");
        let reply = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Notice,
            from: Some("bob".into()),
            target: "me".into(),
            text: "\u{1}VERSION mIRC v7\u{1}".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &reply),
            vec![Action::Send("PRIVMSG #audit :reply VERSION mIRC v7".into())]
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
            vec![Action::Echo {
                target: "(status)".into(),
                text: "w oper net flood detected".into()
            }]
        );
        // ERROR / PING / CONNECTFAIL are plain — they fire regardless; $1- = text.
        assert_eq!(
            dispatch_named(&engine, &ctx(), "ERROR", "", "Closing Link: spam"),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "e Closing Link: spam".into()
            }]
        );
        assert_eq!(
            dispatch_named(&engine, &ctx(), "PING", "", "12345"),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "p".into()
            }]
        );
        assert_eq!(
            dispatch_named(&engine, &ctx(), "CONNECTFAIL", "", "connection refused"),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "cf connection refused".into()
            }]
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
        assert_eq!(
            engine.run_alias(&ctx(), "#x", "f", "#chan"),
            vec![Action::Send("WHO #chan".into())]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#x", "f", "libera #chan"),
            vec![Action::Send("WHO #chan".into())]
        );

        let mut state = crate::irc::state::StateSnapshot::default();
        state.isupport.whox = true;
        state.channels.push(crate::irc::state::ChannelView {
            name: "#chan".into(),
            nicks: vec!["alice".into(), "bob".into()],
            ..Default::default()
        });
        state.ial = vec![
            ("alice".into(), "alice!u@a".into()),
            ("bob".into(), "bob!u@b".into()),
        ];
        let rich = RunCtx {
            state: std::sync::Arc::new(state),
            ..ctx()
        };
        // A complete IAL suppresses redundant WHO traffic; -f forces mIRC's
        // fixed WHOX query when the server advertises support.
        assert!(engine.run_alias(&rich, "#chan", "f", "#chan").is_empty());
        assert_eq!(
            engine.run_alias(&rich, "#chan", "f", "-f #chan"),
            vec![Action::Send("WHO #chan %acdfhlnrstu,995".into())]
        );
    }

    #[test]
    fn ial_commands_emit_connection_local_controls() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias c { /ial off | /ial on | /ialclear Bob | /ialclear | /ialmark Bob trusted | /ialmark -n Bob role admin | /ialmark -rnw Bob r* }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#chan", "c", ""),
            vec![
                Action::Send("\u{0}IAL OFF".into()),
                Action::Send("\u{0}IAL ON".into()),
                Action::Send("\u{0}IAL CLEAR Bob".into()),
                Action::Send("\u{0}IAL CLEAR".into()),
                Action::Send("\u{0}IAL MARK\t0\t0\tBob\tdefault\ttrusted".into()),
                Action::Send("\u{0}IAL MARK\t0\t0\tBob\trole\tadmin".into()),
                Action::Send("\u{0}IAL MARK\t1\t1\tBob\tr*\t".into()),
            ]
        );
    }

    #[test]
    fn raw_event_matches_and_exposes_numeric_event() {
        let engine = ScriptEngine::new();
        engine
            .load("on *:RAW:001:/echo got $numeric ev $event p1 $1-\non *:RAW:PING:/echo gotping");
        let welcome = dispatch_raw(
            &engine,
            &ctx(),
            "001",
            vec!["me".into(), "Welcome here".into()],
        );
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
            vec![Action::Echo {
                target: "(status)".into(),
                text: "gotping".into()
            }]
        );
        // A numeric matching neither handler fires nothing.
        assert!(dispatch_raw(&engine, &ctx(), "999", vec![]).is_empty());
    }

    #[test]
    fn standard_top_level_raw_definition_matches_command_and_reply_text() {
        let engine = ScriptEngine::new();
        engine.load(
            "raw 322:*mirc*:{ /msg #audit list $numeric $1- }\n\
             raw PROP:*owner*:/msg #audit prop $1-",
        );
        assert_eq!(
            dispatch_raw(
                &engine,
                &ctx(),
                "322",
                vec![
                    "me".into(),
                    "#mirc".into(),
                    "42".into(),
                    "mirc users".into()
                ],
            ),
            vec![Action::Send(
                "PRIVMSG #audit :list 322 me #mirc 42 mirc users".into()
            )]
        );
        assert!(dispatch_raw(
            &engine,
            &ctx(),
            "322",
            vec!["me".into(), "#other".into(), "1".into(), "unrelated".into()],
        )
        .is_empty());
        assert_eq!(
            dispatch_raw(
                &engine,
                &ctx(),
                "PROP",
                vec!["%#room".into(), "OWNERKEY".into(), "owner value".into()],
            ),
            vec![Action::Send(
                "PRIVMSG #audit :prop %#room OWNERKEY owner value".into()
            )]
        );
    }

    #[test]
    fn parseline_matches_direction_and_returns_replacement_and_queue() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:PARSELINE:in:*PRIVMSG*:{ echo -a $parsetype $parseutf $parseem $parseline | parseline -itu0 :srv NOTICE me :changed | parseline -oqpt PING :later }\n\
             on *:PARSELINE:out:*:{ parseline -ot PRIVMSG #c :outbound }",
        );
        let incoming = dispatch_parseline(
            &engine,
            &ctx(),
            "in",
            ":nick PRIVMSG #c :café",
            b":nick PRIVMSG #c :caf\xc3\xa9",
            true,
            false,
        );
        assert_eq!(incoming.current, Some(b":srv NOTICE me :changed".to_vec()));
        assert_eq!(
            incoming.actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "in $true $false :nick PRIVMSG #c :cafÃ©".into(),
            }]
        );
        let echoed = dispatch_parseline(
            &engine,
            &ctx(),
            "in",
            ":me PRIVMSG #c :echo",
            b":me PRIVMSG #c :echo",
            true,
            true,
        );
        assert!(matches!(
            echoed.actions.as_slice(),
            [Action::Echo { text, .. }] if text.starts_with("in $true $true ")
        ));
        assert!(matches!(
            incoming.queued.as_slice(),
            [Action::ParseLine {
                direction,
                bytes,
                queue: true,
                trigger: true,
                ..
            }] if direction == "out" && bytes == b"PING :later"
        ));
        let encoded = encode_parseline_control(&incoming.queued[0]).unwrap();
        assert_eq!(
            decode_parseline_control(&encoded),
            Some(incoming.queued[0].clone())
        );

        let outgoing = dispatch_parseline(
            &engine,
            &ctx(),
            "out",
            "PRIVMSG #c :original",
            b"PRIVMSG #c :original",
            true,
            false,
        );
        assert_eq!(outgoing.current, Some(b"PRIVMSG #c :outbound".to_vec()));

        let utf = ScriptEngine::new();
        utf.load("on *:PARSELINE:in:*:{ parseline -itu0 $upper($utfdecode($parseline)) }");
        let decoded = dispatch_parseline(&utf, &ctx(), "in", "café", b"caf\xc3\xa9", true, false);
        assert_eq!(decoded.current, Some("CAFÉ".as_bytes().to_vec()));

        let binary = ScriptEngine::new();
        binary.load("on *:PARSELINE:in:*:{ bset -z &wire 1 0 255 13 10 | parseline -iqb &wire }");
        let binary_outcome =
            dispatch_parseline(&binary, &ctx(), "in", "PING", b"PING", true, false);
        assert!(matches!(
            binary_outcome.queued.as_slice(),
            [Action::ParseLine { bytes, .. }] if bytes == &[0, 255, 13, 10]
        ));
    }

    #[test]
    fn raw_context_exposes_tags_stamp_and_halt_suppression() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:TEXT:*:#:{ echo -a raw=$rawmsg bytes=$rawbytes tags=$msgtags count=$msgtags(0) tag=$msgtags(2).tag key=$msgtags(2).key full=$msgtags(label) stamp=$msgstamp }\n\
             on *:RAW:PRIVMSG:{ halt }",
        );
        let line = "@time=2026-07-15T01:02:03.000Z;label=hello\\sworld :bob!u@h PRIVMSG #c :hi";
        let raw = raw_event_context(line, line.as_bytes());
        let event = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "#c".into(),
            text: "hi".into(),
            time: None,
        };
        let actions = drive_event_halt_raw(&engine, &ctx(), &event, Some(&raw)).0;
        assert_eq!(actions.len(), 1);
        let Action::Echo { text, .. } = &actions[0] else {
            panic!("expected echo");
        };
        assert!(text.contains("raw=:bob!u@h PRIVMSG #c :hi"));
        assert!(text.contains("count=2 tag=label key=hello\\sworld full=label=hello\\sworld"));
        assert!(text.contains("stamp=2026-07-15T01:02:03.000Z"));
        let (_, halted) = dispatch_raw_with_context(
            &engine,
            &ctx(),
            "PRIVMSG",
            vec!["#c".into(), "hi".into()],
            Some(&raw),
        );
        assert!(halted);

        let haltdef = ScriptEngine::new();
        haltdef.load("on *:RAW:PING:{ haltdef }");
        assert!(
            dispatch_raw_with_context(&haltdef, &ctx(), "PING", vec!["token".into()], Some(&raw),)
                .1
        );
    }

    #[test]
    fn script_and_scriptline_follow_loaded_source_and_nested_alias() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "\n\nalias show {\n echo -a $script $scriptline $script(0) $script(2)\n}".into(),
            ),
            ("two.mrc".into(), "alias helper return ok".into()),
        ]);
        assert_eq!(
            engine.run_alias(&ctx(), "", "show", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "one.mrc 4 2 two.mrc".into(),
            }]
        );
    }

    #[test]
    fn alias_identifier_lists_only_alias_source_files() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "alias show echo -a $alias(0) $alias(1) $alias(2) $alias(two.MRC)".into(),
            ),
            ("events.mrc".into(), "on *:CONNECT:/echo connected".into()),
            ("Two.mrc".into(), "alias -l helper return ok".into()),
        ]);
        assert_eq!(
            engine.run_alias(&ctx(), "", "show", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "2 one.mrc Two.mrc Two.mrc".into(),
            }]
        );
    }

    #[test]
    fn custom_identifier_alias_returns_value() {
        let engine = ScriptEngine::new();
        engine
            .load("alias double { /return $calc($1 * 2) }\nalias t { /msg #c result $double(5) }");
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert_eq!(actions, vec![Action::Send("PRIVMSG #c :result 10".into())]);
    }

    #[test]
    fn unknown_identifiers_evaluate_to_mirc_null() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {
                /msg #c before[ $+ $definitely_missing $+ ][ $+ $also_missing(x) $+ ]after
                if ($definitely_missing) /msg #c should-not-run
                if ($definitely_missing == $null) /msg #c null-ok
            }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![
                Action::Send("PRIVMSG #c :before[][]after".into()),
                Action::Send("PRIVMSG #c :null-ok".into()),
            ]
        );
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
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #c :second line [2]".into())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_line_insert_replace_delete_and_search_switches() {
        let dir = std::env::temp_dir().join(format!("jirc-write-ops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/write -c ops.txt one", &[]);
        engine.run_command(&rctx, "#c", "/write ops.txt three", &[]);
        engine.run_command(&rctx, "#c", "/write -il2 ops.txt two", &[]);
        engine.run_command(&rctx, "#c", "/write -l1 ops.txt ONE", &[]);
        engine.run_command(&rctx, "#c", "/write -al3 ops.txt !", &[]);
        engine.run_command(&rctx, "#c", "/write -dsONE ops.txt", &[]);
        assert_eq!(
            std::fs::read_to_string(dir.join("ops.txt")).unwrap(),
            "two\r\nthree!\r\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filter_handles_files_windows_regex_ranges_and_filtered_count() {
        let dir = std::env::temp_dir().join(format!("jirc-filter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("in.txt"),
            "apple red\r\nbanana yellow\r\ncherry red\r\ndate brown\r\n",
        )
        .unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.run_command(&rctx, "#c", "/filter -ffc in.txt out.txt *red*", &[]);
        engine.load("alias count { msg #c $filtered $lines(out.txt) }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "count", ""),
            vec![Action::Send("PRIVMSG #c :2 2".into())]
        );

        engine.run_command(&rctx, "#c", "/window @input", &[]);
        engine.run_command(&rctx, "#c", "/aline @input Alpha", &[]);
        engine.run_command(&rctx, "#c", "/aline @input beta", &[]);
        engine.run_command(&rctx, "#c", "/aline @input ALPINE", &[]);
        engine.run_command(&rctx, "#c", "/filter -wwcg @input @output /^alp/i", &[]);
        engine.load("alias rows { msg #c $window(@output).lines $line(@output,2) $filtered }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "rows", ""),
            vec![Action::Send("PRIVMSG #c :2 ALPINE 2".into())]
        );
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
        engine.load(
            "alias r { /msg #c $readini(cfg.ini, User, nick) [ $+ $ini(cfg.ini, User, 0) $+ ] }",
        );
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
    fn read_and_readini_pipe_switches_match_mirc_evaluation() {
        let dir = std::env::temp_dir().join(format!("jirc-read-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pipe.txt"), "$me | /msg #c read-tail $me\n").unwrap();
        std::fs::write(
            dir.join("pipe.ini"),
            "[Data]\nvalue=$me | /msg #c ini-tail $me\n",
        )
        .unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "alias read_plain { /msg #c $read(pipe.txt,n,1) }\n\
             alias read_p { /msg #c $read(pipe.txt,p,1) }\n\
             alias read_np { /msg #c $read(pipe.txt,np,1) }\n\
             alias ini_plain { /msg #c $readini(pipe.ini,n,Data,value) }\n\
             alias ini_p { /msg #c $readini(pipe.ini,p,Data,value) }\n\
             alias ini_np { /msg #c $readini(pipe.ini,np,Data,value) }",
        );

        // Without `p`, a pipe returned by either identifier is ordinary data.
        assert_eq!(
            engine.run_alias(&rctx, "#c", "read_plain", ""),
            vec![Action::Send(
                "PRIVMSG #c :$me | /msg #c read-tail $me".into()
            )]
        );
        assert_eq!(
            engine.run_alias(&rctx, "#c", "ini_plain", ""),
            vec![Action::Send(
                "PRIVMSG #c :$me | /msg #c ini-tail $me".into()
            )]
        );

        // `p` evaluates the returned value and makes its pipe structural.
        assert_eq!(
            engine.run_alias(&rctx, "#c", "read_p", ""),
            vec![
                Action::Send("PRIVMSG #c :me".into()),
                Action::Send("PRIVMSG #c :read-tail me".into()),
            ]
        );
        assert_eq!(
            engine.run_alias(&rctx, "#c", "ini_p", ""),
            vec![
                Action::Send("PRIVMSG #c :me".into()),
                Action::Send("PRIVMSG #c :ini-tail me".into()),
            ]
        );

        // `n` keeps the value before the separator literal; `p` still executes
        // the command after it, which then performs its own normal evaluation.
        assert_eq!(
            engine.run_alias(&rctx, "#c", "read_np", ""),
            vec![
                Action::Send("PRIVMSG #c :$me".into()),
                Action::Send("PRIVMSG #c :read-tail me".into()),
            ]
        );
        assert_eq!(
            engine.run_alias(&rctx, "#c", "ini_np", ""),
            vec![
                Action::Send("PRIVMSG #c :$me".into()),
                Action::Send("PRIVMSG #c :ini-tail me".into()),
            ]
        );
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
        engine.load(
            "alias n { /msg #c files= $+ $findfile(data, *.txt, 0) dirs= $+ $finddir(data, *, 0) }",
        );
        let actions = engine.run_alias(&rctx, "#c", "n", "");
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #c :files=3 dirs=1".into())]
        );
        engine.load("alias n { /noop $findfile(data, *.txt, 0, 0, /msg #c $findfilen $1-) }");
        let callback_actions = engine.run_alias(&rctx, "#c", "n", "");
        assert_eq!(callback_actions.len(), 3);
        for (index, action) in callback_actions.iter().enumerate() {
            let Action::Send(line) = action else {
                panic!("findfile callback must send each match");
            };
            assert!(line.starts_with(&format!("PRIVMSG #c :{} ", index + 1)));
            assert!(line.ends_with(".txt"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completed_compatibility_commands_update_runtime_and_user_state() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias n { /bigfloat on | /dlevel 7 | /debug 1 | /dccignore on | /emailaddr user@example.test | /creq auto | /sreq ignore | /localinfo workstation 192.0.2.4 | /ebeeps on | /msg #c $bigfloat $dlevel $debug $dccignore $emailaddr $creq $sreq $host $ip $ebeeps }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "n", "");
        assert!(actions.iter().any(|action| matches!(action, Action::Send(line) if line == "PRIVMSG #c :$true 7 1 on user@example.test auto ignore workstation 192.0.2.4 $true")));
        assert!(actions.iter().any(|action| matches!(action, Action::ClientCommand { command, args, .. } if command == "debug" && args == "1")));
        assert!(actions.iter().any(|action| matches!(action, Action::ClientCommand { command, args, .. } if command == "dccignore" && args == "on")));

        engine.load("alias n { /auser 5,7 *!*@example.test note | /rlevel 5 | /ulist } ");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "n", ""),
            vec![Action::Echo {
                target: "#c".into(),
                text: "7:*!*@example.test note".into(),
            }]
        );
    }

    #[test]
    fn audited_compatibility_commands_are_all_client_local() {
        let commands = [
            "ajinvite",
            "background",
            "beep",
            "bigfloat",
            "bindip",
            "cline",
            "clipboard",
            "cnick",
            "color",
            "creq",
            "debug",
            "dlevel",
            "donotdisturb",
            "dqwindow",
            "ebeeps",
            "emailaddr",
            "firewall",
            "flash",
            "flist",
            "flood",
            "flush",
            "ghide",
            "gload",
            "gmove",
            "gopts",
            "gplay",
            "gpoint",
            "gqreq",
            "gshow",
            "gsize",
            "gstop",
            "gtalk",
            "gunload",
            "identd",
            "localinfo",
            "mdi",
            "perform",
            "playctrl",
            "proxy",
            "reseterror",
            "resetidle",
            "rlevel",
            "save",
            "setlayer",
            "showmirc",
            "speak",
            "sreq",
            "tray",
            "ulist",
            "vcadd",
            "vcmd",
            "vcrem",
            "vol",
            "winhelp",
        ];
        for command in commands {
            let engine = ScriptEngine::new();
            let actions = engine.run_command(&ctx(), "#c", &format!("/{command}"), &[]);
            assert!(
                !actions.iter().any(|action| matches!(action, Action::Send(line) if line.split_whitespace().next().is_some_and(|word| word.eq_ignore_ascii_case(command)))),
                "/{command} leaked to the IRC server: {actions:?}"
            );
        }
        assert_eq!(
            ScriptEngine::new().run_command(&ctx(), "#c", "/links -n", &[]),
            vec![Action::Send("LINKS".into())]
        );
        assert_eq!(
            ScriptEngine::new().run_command(&ctx(), "#c", "/uwho Alice", &[]),
            vec![Action::Send("WHOIS Alice".into())]
        );
    }

    #[test]
    fn cline_recolors_an_existing_custom_window_line() {
        let engine = ScriptEngine::new();
        engine.load("alias n { /window -l @list | /aline @list hello | /cline @list 1 4 | /msg #c $line(@list,1) }");
        let actions = engine.run_alias(&ctx(), "#c", "n", "");
        assert!(actions.iter().any(
            |action| matches!(action, Action::Send(line) if line == "PRIVMSG #c :\u{3}04hello")
        ));
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
            state: std::sync::Arc::new(StateSnapshot {
                away_time: 1_700_000_500,
                ..Default::default()
            }),
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
            "alias n { /window @list | /aline @list one | /aline @list two | /rline @list 1 ONE | /sline @list 2 | /msg #c $window(@list).lines $+ / $+ $line(@list,1) $+ / $+ $line(@list,2) $+ / $+ $line(@list,2).state $+ / $+ $sline(@list,1) $+ / $+ $sline(@list,1).ln }",
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
        assert_eq!(sends, vec!["PRIVMSG #c :2/ONE/two/1/two/2"]);

        let echo = engine.run_command(&ctx(), "#c", "/echo -t @list debug output", &[]);
        assert_eq!(
            echo,
            vec![Action::WindowLine {
                name: "@list".into(),
                op: "add".into(),
                n: 0,
                text: "debug output".into(),
            }]
        );
        let inspect = engine.run_command(&ctx(), "#c", "/echo -a $line(@list,3)", &[]);
        assert_eq!(
            inspect,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "debug output".into(),
            }]
        );

        let edit = engine.run_command(&ctx(), "#c", "/window -e @input", &[]);
        assert!(edit.iter().any(|action| matches!(
            action,
            Action::WindowOpen { name, kind, .. }
                if name == "@input" && kind == "editbox"
        )));

        engine.run_command(&ctx(), "#c", "/window -p @graph", &[]);
        let picture = engine.run_command(&ctx(), "#c", "/drawline @graph 4 2 10 20 30 40", &[]);
        assert!(picture.iter().any(|action| matches!(
            action,
            Action::WindowDraw { name, op, args }
                if name == "@graph" && op == "drawline"
                    && args == &["", "4", "2", "10", "20", "30", "40"]
        )));

        let fill = engine.run_command(&ctx(), "#c", "/drawfill @graph 3 1 12 24", &[]);
        assert!(fill.iter().any(|action| matches!(
            action,
            Action::WindowDraw { name, op, args }
                if name == "@graph" && op == "drawfill"
                    && args == &["", "3", "1", "12", "24"]
        )));

        engine.run_command(&ctx(), "#c", "/window -p @copy", &[]);
        for (command, expected_name, expected_op) in [
            (
                "/drawcopy -tm @graph 16711935 0 0 20 20 @copy 5 6 40 40",
                "@copy",
                "drawcopy",
            ),
            (
                "/drawpic -sm @graph 10 20 64 32 \"images/test image.png\"",
                "@graph",
                "drawpic",
            ),
            ("/drawrot -bfc @graph 1 45 0 0 100 80", "@graph", "drawrot"),
            (
                "/drawscroll -n @graph 2 -3 0 0 100 80",
                "@graph",
                "drawscroll",
            ),
            (
                "/drawsave -aq90 @graph 0 0 100 80 \"shots/test image.jpg\"",
                "@graph",
                "drawsave",
            ),
        ] {
            let actions = engine.run_command(&ctx(), "#c", command, &[]);
            assert!(actions.iter().any(|action| matches!(
                action,
                Action::WindowDraw { name, op, .. }
                    if name == expected_name && op == expected_op
            )));
        }

        let mouse = engine.run_window_mouse_command(
            &ctx(),
            "@graph",
            "/msg #c $mouse.win $mouse.x $mouse.y $mouse.lb $mouse.key $click(@graph,1).x $inrect(12,24,0,0,20,30)",
            "",
            12,
            24,
            0,
            5,
        );
        assert_eq!(
            mouse,
            vec![Action::Send(
                "PRIVMSG #c :@graph 12 24 false 5 12 $true".into()
            )]
        );
        assert_eq!(
            engine.run_window_mouse_command(
                &ctx(),
                "@list",
                "/msg #c $mouse.lb $1",
                "",
                0,
                0,
                2,
                0,
            ),
            vec![Action::Send("PRIVMSG #c :true 2".into())]
        );
    }

    #[test]
    fn custom_window_title_and_buffer_files() {
        let data_dir = std::env::temp_dir().join(format!(
            "jirc-window-buffer-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let run_ctx = RunCtx {
            data_dir: data_dir.clone(),
            ..ctx()
        };
        let engine = ScriptEngine::new();
        engine.run_command(&run_ctx, "#c", "/window @notes", &[]);
        engine.run_command(&run_ctx, "#c", "/aline @notes first", &[]);
        engine.run_command(&run_ctx, "#c", "/aline @notes second", &[]);

        let title = engine.run_command(&run_ctx, "#c", "/titlebar @notes Saved notes", &[]);
        assert_eq!(
            title,
            vec![Action::WindowTitle {
                name: "@notes".into(),
                title: "Saved notes".into(),
            }]
        );

        engine.run_command(&run_ctx, "#c", "/savebuf @notes notes.txt", &[]);
        assert_eq!(
            std::fs::read_to_string(data_dir.join("notes.txt")).unwrap(),
            "first\nsecond\n"
        );

        engine.run_command(&run_ctx, "#c", "/clear @notes", &[]);
        engine.run_command(&run_ctx, "#c", "/aline @notes existing", &[]);
        let loaded = engine.run_command(&run_ctx, "#c", "/loadbuf @notes notes.txt", &[]);
        assert_eq!(
            loaded,
            vec![
                Action::WindowLine {
                    name: "@notes".into(),
                    op: "add".into(),
                    n: 0,
                    text: "first".into(),
                },
                Action::WindowLine {
                    name: "@notes".into(),
                    op: "add".into(),
                    n: 0,
                    text: "second".into(),
                },
            ]
        );
        let count = engine.run_command(&run_ctx, "#c", "/msg #c $window(@notes).lines", &[]);
        assert_eq!(count, vec![Action::Send("PRIVMSG #c :3".into())]);

        engine.run_command(&run_ctx, "#c", "/loadbuf 1 -r @notes notes.txt", &[]);
        let last = engine.run_command(&run_ctx, "#c", "/msg #c $line(@notes,1)", &[]);
        assert_eq!(last, vec![Action::Send("PRIVMSG #c :second".into())]);
        engine.run_command(&run_ctx, "#c", "/savebuf 1 @notes last.txt", &[]);
        assert_eq!(
            std::fs::read_to_string(data_dir.join("last.txt")).unwrap(),
            "second\n"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
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
    fn extended_connection_and_alias_parameter_identifiers() {
        use sha2::Digest;
        let certificate = b"test certificate".to_vec();
        let snap = crate::irc::state::StateSnapshot {
            server_ip: "203.0.113.4".into(),
            server_target: "irc.example.test".into(),
            tls: true,
            tls_version: "TLSv1.3".into(),
            tls_peer_certificate: certificate.clone(),
            tls_cert_valid: true,
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let engine = ScriptEngine::new();
        engine.load("alias facts { /msg #c $serverip $servertarget $sslversion $sslcertvalid $sslhash(sha256,s) $parms }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "facts", "one two three"),
            vec![Action::Send(format!(
                "PRIVMSG #c :203.0.113.4 irc.example.test TLSv1.3 $true {:x} one two three",
                sha2::Sha256::digest(&certificate)
            ))]
        );
    }

    #[test]
    fn remote_command_controls_event_dispatch_bits() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:SIGNAL:test:/echo -a fired\nalias off { /remote off }\nalias on { /remote on }",
        );
        let signal = || EventVars {
            chan: "test".into(),
            ..Default::default()
        };
        assert_eq!(engine.dispatch_event(&ctx(), "SIGNAL", signal()).len(), 1);
        engine.run_alias(&ctx(), "#c", "off", "");
        assert!(engine.dispatch_event(&ctx(), "SIGNAL", signal()).is_empty());
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c $remote", &[]),
            vec![Action::Send("PRIVMSG #c :0".into())]
        );
        engine.run_alias(&ctx(), "#c", "on", "");
        assert_eq!(engine.dispatch_event(&ctx(), "SIGNAL", signal()).len(), 1);
        engine.run_command(&ctx(), "#c", "/events off", &[]);
        assert!(engine.dispatch_event(&ctx(), "SIGNAL", signal()).is_empty());
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/msg #c $remote", &[]),
            vec![Action::Send("PRIVMSG #c :5".into())]
        );
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/events", &[]),
            vec![Action::Echo {
                target: "#c".into(),
                text: "* Events are off".into(),
            }]
        );
        engine.run_command(&ctx(), "#c", "/events on", &[]);
        assert_eq!(engine.dispatch_event(&ctx(), "SIGNAL", signal()).len(), 1);
    }

    #[test]
    fn dns_event_identifiers_and_command_are_script_visible() {
        let engine = ScriptEngine::new();
        engine.load("on *:DNS:{ /echo -a $raddress $dns(0) $dns(1).ip }");
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/dns example.test", &[]),
            vec![Action::DnsLookup {
                host: "example.test".into()
            }]
        );
        let actions = engine.dispatch_event(
            &ctx(),
            "DNS",
            EventVars {
                dns_query: "example.test".into(),
                dns_ips: vec!["192.0.2.1".into()],
                params: vec!["example.test".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "example.test 1 192.0.2.1".into()
            }]
        );
    }

    #[test]
    fn script_lifecycle_commands_preserve_switch_intent() {
        let engine = ScriptEngine::new();
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/load -rs tools.mrc", &[]),
            vec![Action::ScriptLoad {
                name: "tools.mrc".into(),
                load: true,
                suppress_event: false
            }]
        );
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/unload -nrs tools.mrc", &[]),
            vec![Action::ScriptLoad {
                name: "tools.mrc".into(),
                load: false,
                suppress_event: true
            }]
        );
    }

    #[test]
    fn active_and_tabcomp_events_match_targets_and_can_halt() {
        let engine = ScriptEngine::new();
        engine.load("on *:ACTIVE:#chan:{ /echo -a active $target }\non *:TABCOMP:#chan:{ /echo -a tab $1- | /halt }");
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "ACTIVE",
                EventVars {
                    target: "#chan".into(),
                    chan: "#chan".into(),
                    ..Default::default()
                }
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "active #chan".into()
            }]
        );
        let (actions, halted) = engine.dispatch_event_halt(
            &ctx(),
            "TABCOMP",
            EventVars {
                target: "#chan".into(),
                chan: "#chan".into(),
                text: "hello al".into(),
                params: vec!["hello".into(), "al".into()],
                ..Default::default()
            },
        );
        assert!(halted);
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "tab hello al".into()
            }]
        );
    }

    #[test]
    fn real_world_msl_compatibility_corpus() {
        use crate::irc::state::{ChannelView, StateSnapshot};

        let daily = ScriptEngine::new();
        daily.load(include_str!(
            "../../tests/fixtures/msl-compat/daily-aliases.msl"
        ));
        assert_eq!(
            daily.run_alias(&ctx(), "#compat", "compat_daily", "10 20 12"),
            vec![Action::Send(
                "PRIVMSG #compat :total=42 first=10 parms=10 20 12".into()
            )]
        );

        let bot = ScriptEngine::new();
        bot.load(include_str!(
            "../../tests/fixtures/msl-compat/event-bot.msl"
        ));
        let message = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("alice".into()),
            target: "#compat".into(),
            text: "!hello world".into(),
            time: None,
        };
        assert_eq!(
            drive_event(&bot, &ctx(), &message),
            vec![Action::Send(
                "PRIVMSG #compat :Hello world, requested by alice on Net".into()
            )]
        );

        let stateful = ScriptEngine::new();
        stateful.load(include_str!(
            "../../tests/fixtures/msl-compat/channel-state.msl"
        ));
        let snapshot = StateSnapshot {
            nick: "me".into(),
            channels: vec![ChannelView {
                name: "#compat".into(),
                nicks: vec!["me".into(), "alice".into()],
                members: vec![("me".into(), "@".into()), ("alice".into(), "".into())],
                ..Default::default()
            }],
            ial: vec![("alice".into(), "alice!u@example.test".into())],
            ..Default::default()
        };
        let state_ctx = RunCtx {
            state: std::sync::Arc::new(snapshot),
            ..ctx()
        };
        assert_eq!(
            stateful.run_alias(&state_ctx, "#compat", "compat_state", "alice"),
            vec![
                Action::Send("MODE #compat +v alice".into()),
                // $address(nick,5) is mIRC's nick!user@host mask type.
                Action::Send("PRIVMSG #compat :voiced alice from alice!u@example.test".into()),
            ]
        );

        let ui = ScriptEngine::new();
        ui.load(include_str!(
            "../../tests/fixtures/msl-compat/script-ui.msl"
        ));
        assert!(matches!(
            ui.run_alias(&ctx(), "#compat", "compat_ui", "").as_slice(),
            [Action::DialogOpen { name, .. }] if name == "compatbox"
        ));
    }

    #[test]
    fn process_identifiers_expand_without_platform_specific_state() {
        let engine = ScriptEngine::new();
        let actions =
            engine.run_command(&ctx(), "#c", "/msg #c portable=$portable cmd=$cmdline", &[]);
        let [Action::Send(line)] = actions.as_slice() else {
            panic!("expected one outgoing message");
        };
        assert!(
            line.starts_with("PRIVMSG #c :portable=$true cmd=")
                || line.starts_with("PRIVMSG #c :portable=$false cmd=")
        );
    }

    #[test]
    fn samepath_identifier_compares_sandboxed_paths() {
        let engine = ScriptEngine::new();
        assert_eq!(
            engine.run_command(
                &ctx(),
                "#c",
                "/msg #c $samepath(file.txt,nested/file.txt) $samepath(file.txt,other.txt)",
                &[]
            ),
            vec![Action::Send("PRIVMSG #c :$true $false".into())]
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
                ..Default::default()
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
        engine.load(
            "alias n { /msg #c $ialchan(*!*@host1.com,#chan,0) $+ / $+ $ialchan(*!*@*,#chan,0) }",
        );
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
    fn regsub_output_variables_receive_text_and_return_match_count() {
        let engine = ScriptEngine::new();
        engine.load(
            r#"alias replace {
              var %out = old
              var %n = $regsub(a1 b2,/(\w)(\d)/g,\2\1,%out)
              var %m = $regsubex(rx,ab,/(\w)/g,$upper(\t),&binary)
              /msg #c %n $+ / $+ %out $+ / $+ %m $+ / $+ $bvar(&binary).text
            }"#,
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "replace", ""),
            vec![Action::Send("PRIVMSG #c :2/1a 2b/2/AB".into())]
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
        assert_eq!(engine.cid_for("s1"), 1);
        assert_eq!(engine.cid_for("missing"), 0);
        assert_eq!(engine.connections_in_cid_order(), vec!["s1", "s2"]);
        engine.set_connection_context("s2", "Network Two", "irc.two.test");
        assert_eq!(
            engine.connection_context("s2"),
            Some(("Network Two".into(), "irc.two.test".into()))
        );
        engine.set_active_conn("s2");
        assert_eq!(engine.active_connection().as_deref(), Some("s2"));

        // $scon(0) = count, $scon(N) = Nth cid, $activecid = the active connection.
        assert_eq!(
            engine.run_command(
                &ctx(),
                "#c",
                "/msg #c n=$scon(0) first=$scon(1) act=$activecid",
                &[]
            ),
            vec![Action::Send("PRIVMSG #c :n=2 first=1 act=2".into())]
        );

        // $cid is the *run's own* connection, read from the state snapshot.
        let snap = crate::irc::state::StateSnapshot {
            server_id: "s2".into(),
            ..Default::default()
        };
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
        assert_eq!(engine.cid_for("s1"), 0);
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
            engine.run_command(
                &ctx(),
                "#c",
                "/msg #c c=$scid(0) a=$scid(-1) v=$scid(2) x=$scid(9)",
                &[]
            ),
            vec![Action::Send("PRIVMSG #c :c=2 a=2 v=2 x=".into())]
        );
    }

    #[test]
    fn window_ids() {
        let engine = ScriptEngine::new();
        engine.assign_cid("s1");
        // The UI opens windows; each gets a stable wid. Same (server,name) is idempotent.
        assert_eq!(engine.window_open("s1", "#a"), 1);
        assert_eq!(engine.window_open("s1", "#b"), 2);
        assert_eq!(engine.window_open("s1", "#a"), 1);
        engine.set_active_win("s1", "#a");
        engine.set_active_win("s1", "#b");

        // $activewid = the active window; $wid (in an event for #a) = that window.
        engine.load(
            "on *:TEXT:*:#:{ /msg $chan wid=$wid active=$activewid last=$lactive/$lactivewid/$lactivecid }",
        );
        let snap = crate::irc::state::StateSnapshot {
            server_id: "s1".into(),
            ..Default::default()
        };
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
            vec![Action::Send(
                "PRIVMSG #a :wid=1 active=2 last=#a/1/1".into()
            )]
        );

        // Closing #a clears previous-active state and retains the closed-window
        // identity independently for `$leftwin*`.
        engine.window_close("s1", "#a");
        assert_eq!(
            engine.run_command(
                &ctx2,
                "#c",
                "/msg #c last=$lactive/$lactivewid/$lactivecid",
                &[]
            ),
            vec![Action::Send("PRIVMSG #c :last=//".into())]
        );
        assert_eq!(
            engine.run_command(
                &ctx2,
                "#c",
                "/msg #c left=$leftwin/$leftwinwid/$leftwincid",
                &[]
            ),
            vec![Action::Send("PRIVMSG #c :left=#a/1/1".into())]
        );
        assert_eq!(engine.window_open("s1", "#c"), 3); // new window, not reusing 1
    }

    #[test]
    fn query_identifier_uses_open_windows_and_live_state() {
        let engine = ScriptEngine::new();
        engine.assign_cid("s1");
        engine.assign_cid("s2");
        engine.window_open("s1", "Status Window");
        engine.window_open("s1", "#channel");
        engine.window_open("s1", "@custom");
        let bob_wid = engine.window_open("s1", "Bob");
        engine.window_open("s2", "Alice");

        let activity = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(4);
        let snap = crate::irc::state::StateSnapshot {
            server_id: "s1".into(),
            ial: vec![("Bob".into(), "Bob!user@example.test".into())],
            channels: vec![crate::irc::state::ChannelView {
                name: "#channel".into(),
                member_activity: vec![("bOB".into(), activity)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let query_ctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        engine.load(
            "alias q { echo -a $query(0) $query(1) $query(bob).wid $query(BOB).cid $query(bob).addr $query(bob).idle }",
        );
        let actions = engine.run_alias(&query_ctx, "", "q", "");
        let Action::Echo { text, .. } = &actions[0] else {
            panic!("expected echo");
        };
        let fields: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(fields[0], "1");
        assert_eq!(fields[1], "Bob");
        assert_eq!(fields[2], bob_wid.to_string());
        assert_eq!(fields[3], "1");
        assert_eq!(fields[4], "Bob!user@example.test");
        assert!(matches!(fields[5].parse::<u64>(), Ok(4..=6)));
    }

    #[test]
    fn scon_scid_dispatch() {
        let engine = ScriptEngine::new();
        engine.assign_cid("s1");
        engine.assign_cid("s2");
        // /scon N targets the Nth connection; the subcommand is carried raw to it.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/scon 2 /msg #c hi", &[]),
            vec![Action::RunOn {
                server_id: "s2".into(),
                command: "/msg #c hi".into()
            }]
        );
        // /scid targets by cid.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/scid 1 /msg #c yo", &[]),
            vec![Action::RunOn {
                server_id: "s1".into(),
                command: "/msg #c yo".into()
            }]
        );
        // An out-of-range selector produces nothing.
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/scon 9 /msg #c x", &[]),
            vec![]
        );
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
            acts.iter()
                .any(|a| matches!(a, Action::Echo { text, .. } if text == want))
        };
        assert!(echoed(
            engine.dispatch_event(&ctx(), "START", EventVars::default()),
            "started"
        ));
        assert!(echoed(
            engine.dispatch_event(&ctx(), "UNLOAD", EventVars::default()),
            "unloading"
        ));
        assert!(echoed(
            engine.dispatch_event(&ctx(), "EXIT", EventVars::default()),
            "exiting"
        ));
        // A script with no lifecycle handlers dispatches to nothing.
        let bare = ScriptEngine::new();
        bare.load("alias x { /echo hi }");
        assert!(bare
            .dispatch_event(&ctx(), "START", EventVars::default())
            .is_empty());
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
            acts.iter()
                .any(|a| matches!(a, Action::Echo { text, .. } if text == want))
        };
        // A query window (empty $chan so `?` matches; $target = the other party).
        let q = EventVars {
            nick: "bob".into(),
            target: "bob".into(),
            ..Default::default()
        };
        assert!(echoed(
            engine.dispatch_event(&ctx(), "OPEN", q.clone()),
            "opened query bob"
        ));
        // A channel window: `#` matches, `?` does not.
        let c = EventVars {
            chan: "#c".into(),
            target: "#c".into(),
            ..Default::default()
        };
        let ca = engine.dispatch_event(&ctx(), "OPEN", c);
        assert!(echoed(ca.clone(), "opened chan #c"));
        assert!(!echoed(ca, "opened query #c"));
        // on CLOSE:? fires when a query closes.
        assert!(echoed(
            engine.dispatch_event(&ctx(), "CLOSE", q),
            "closed query bob"
        ));
    }

    #[test]
    fn notify_events() {
        let engine = ScriptEngine::new();
        // Plain events (no target/matchtext); $nick is the friend who changed state.
        engine.load(
            "on *:NOTIFY:/msg #f $nick is online\n\
             on *:UNOTIFY:/msg #f $nick left",
        );
        let vars = EventVars {
            nick: "alice".into(),
            target: "alice".into(),
            ..Default::default()
        };
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
                ..Default::default()
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
        // $iif's condition is evaluated like `if`, including mIRC's infix
        // negated operator spelling (`!isop`).
        engine.load("alias t { /msg #c $iif(bob isop #c,op,notop) $iif(alice isop #c,op,notop) $iif(bob !isop #c,notop,op) $iif(alice !isop #c,notop,op) $iif($null !isop #c,notop,op) }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :op notop op notop notop".into())]
        );
    }

    #[test]
    fn i7_nickname_validation_supports_negated_isnum_range() {
        let engine = ScriptEngine::new();
        engine.load(
            r#"
            alias t {
              var %n = $remove($1,$chr(62),$chr(32),$cr,$lf)
              if (($len(%n) !isnum 2-20) || (!$regex(%n,/^[A-Za-z][A-Za-z0-9_-]+$/))) { /msg #c invalid | return }
              /msg #c valid %n
            }
            "#,
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "Nick"),
            vec![Action::Send("PRIVMSG #c :valid Nick".into())]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "A"),
            vec![Action::Send("PRIVMSG #c :invalid".into())]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "1bad"),
            vec![Action::Send("PRIVMSG #c :invalid".into())]
        );

        // i7 also uses `item !isin %list` where the list may be unset. The
        // missing RHS remains an empty binary operand rather than becoming a
        // malformed unary expression.
        engine.load("alias t { var %listed | /msg #c $iif(. !isin %listed,not-listed,listed) $iif($null !isnum 2-20,out-of-range,in-range) }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :not-listed out-of-range".into())]
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
            vec![Action::Send(
                "PRIVMSG #c :3/65536/1/1 + 1 + 1/9 - 4/a + b/12".into()
            )]
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
    fn eval_short_form() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               set %y hello\n\
               set %x % $+ y\n\
               /msg #c $(%x,2) and $(%x,1)\n\
             }",
        );
        // $(text,N) == $eval(text,N): $(%x,2) evaluates %x's value ("%y") once more
        // -> "hello"; $(%x,1) leaves it "%y".
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :hello and %y".into())]
        );
    }

    #[test]
    fn eval_zero_keeps_the_first_argument_literal() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t { set %value expanded | msg #c [ $+ $eval(%value,0) $+ ] [ $+ $eval(%value,1) $+ ] }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :[%value] [expanded]".into())]
        );
    }

    #[test]
    fn read_and_readini_evaluate_by_default_and_n_returns_plain_text() {
        let name = format!("jirc-read-eval-{}.txt", std::process::id());
        let ini_name = format!("jirc-read-eval-{}.ini", std::process::id());
        let base = std::env::temp_dir();
        std::fs::write(base.join(&name), "2\n%value\n$upper(done)\n").unwrap();
        std::fs::write(
            base.join(&ini_name),
            "[section]\nevaluated=%value\nplain=$upper(done)\n",
        )
        .unwrap();
        let engine = ScriptEngine::new();
        engine.load(&format!(
            "alias t {{ set %value expanded | msg #c $read({name},1) [ $+ $read({name},n,1) $+ ] $read({name},2) $readini({ini_name},section,evaluated) [ $+ $readini({ini_name},n,section,plain) $+ ] }}"
        ));
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send(
                "PRIVMSG #c :expanded [%value] DONE expanded [$upper(done)]".into()
            )]
        );
        let _ = std::fs::remove_file(base.join(name));
        let _ = std::fs::remove_file(base.join(ini_name));
    }

    #[test]
    fn read_switch_headers_regex_unicode_and_line_endings_match_mirc() {
        let id = std::process::id();
        let header = format!("jirc-read-header-{id}.txt");
        let regex = format!("jirc-read-regex-{id}.txt");
        let unicode = format!("jirc-read-unicode-{id}.txt");
        let endings = format!("jirc-read-endings-{id}.txt");
        let base = std::env::temp_dir();
        std::fs::write(base.join(&header), "1\nonly\n").unwrap();
        std::fs::write(base.join(&regex), "Alpha\nBeta\n").unwrap();
        std::fs::write(base.join(&unicode), "K rest\n").unwrap();
        std::fs::write(base.join(&endings), "first\rsecond\0\rthird").unwrap();
        let engine = ScriptEngine::new();
        engine.load(&format!(
            "alias t {{ msg #c random=$read({header},n) miss=[ $+ $read({header},n,99) $+ ] readn=$readn regex=$read({regex},rt,/^beta$/i) unicode=$read({unicode},st,k) cr=$read({endings},t,2) nul=[ $+ $read({endings},t,3) $+ ] }}"
        ));
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send(
                "PRIVMSG #c :random=only miss=[] readn=0 regex=Beta unicode=rest cr=second nul=[]"
                    .into()
            )]
        );
        for name in [header, regex, unicode, endings] {
            let _ = std::fs::remove_file(base.join(name));
        }
    }

    #[test]
    fn prop_and_unsafe() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias conv {\n\
               if ($prop == double) return $calc($1 * 2)\n\
               if ($prop == triple) return $calc($1 * 3)\n\
               return $1\n\
             }\n\
             alias t { /msg #c $conv(5).double $conv(5).triple $conv(5) $unsafe(hi) }",
        );
        // $prop is the `.property`; an ordinary safe `$unsafe` value displays
        // unchanged after its one-level protection is consumed.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :10 15 5 hi".into())]
        );
    }

    #[test]
    fn unsafe_blocks_deferred_timer_injection_and_preserves_private_use_text() {
        let engine = ScriptEngine::new();
        let old_sentinels = "\u{E101}\u{E102}\u{E103}\u{E104}\u{E105}\u{E106}";
        engine.load(&format!(
            "alias literal {{ /msg #c {old_sentinels} $unsafe({old_sentinels}) }}\n\
             alias safe {{ /timerwork 1 1 /msg #c $unsafe($1-) }}\n\
             alias exposed {{ /timerwork 1 1 /msg #c $1- }}"
        ));
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "literal", ""),
            vec![Action::Send(format!(
                "PRIVMSG #c :{old_sentinels} {old_sentinels}"
            ))]
        );

        let payload = "$me | /msg #c injected";
        let safe = engine.run_alias(&ctx(), "#c", "safe", payload).remove(0);
        let safe_command = match safe {
            Action::Timer { command, .. } => command,
            other => panic!("expected safe timer action, got {other:?}"),
        };
        // `$unsafe` keeps both `$me` and the pipe literal through the timer's
        // deferred parse, so attacker-controlled text remains one message.
        assert_eq!(
            engine.run_command(&ctx(), "#c", &safe_command, &[]),
            vec![Action::Send("PRIVMSG #c :$me | /msg #c injected".into())]
        );

        // The control demonstrates the deferred-evaluation threat `$unsafe`
        // prevents: without it, the same payload evaluates and splits in two.
        let exposed = engine.run_alias(&ctx(), "#c", "exposed", payload).remove(0);
        let exposed_command = match exposed {
            Action::Timer { command, .. } => command,
            other => panic!("expected exposed timer action, got {other:?}"),
        };
        assert_eq!(
            engine.run_command(&ctx(), "#c", &exposed_command, &[]),
            vec![
                Action::Send("PRIVMSG #c :me".into()),
                Action::Send("PRIVMSG #c :injected".into()),
            ]
        );
    }

    #[test]
    fn user_list_commands_and_idents() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               auser 5 *!*@example.com Cool people\n\
               auser 10 SomeNick\n\
               auser -a 20 *!*@example.com\n\
               /msg #c $ulist(*,,0) [ $+ $level(*!*@example.com) $+ ] $ulist(nick!u@example.com,,1).info\n\
             }",
        );
        // 2 entries; example.com has levels 5,20 (merged via -a); info preserved.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :2 [5,20] Cool people".into())]
        );
    }

    #[test]
    fn user_list_ruser_removes() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               auser 5,10 bob\n\
               ruser 5 bob\n\
               auser 3 alice\n\
               ruser alice\n\
               /msg #c $level(bob) / $ulist(*,,0)\n\
             }",
        );
        // bob keeps level 10 (5 removed); alice removed entirely -> 1 entry left.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :10 / 1".into())]
        );
    }

    #[test]
    fn event_level_gating() {
        use crate::irc::state::StateSnapshot;
        let engine = ScriptEngine::new();
        engine.load(
            "on *:TEXT:!setup:#:{ auser 10 *!*@trusted.com }\n\
             on 5:TEXT:!g*:#:{ /msg #c ok5 $matchkey $maddress }\n\
             on 50:TEXT:!go:#:{ /msg #c ok50 }",
        );
        let snap = StateSnapshot {
            ial: vec![("bob".into(), "bob!u@trusted.com".into())],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let ev = |t: &str| EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: t.into(),
            params: vec![t.into()],
            ..Default::default()
        };
        // `*` event adds bob's mask at level 10.
        engine.dispatch_event(&rctx, "TEXT", ev("!setup"));
        // bob (level 10) triggers the `on 5:` handler but not `on 50:`.
        assert_eq!(
            engine.dispatch_event(&rctx, "TEXT", ev("!go")),
            vec![Action::Send("PRIVMSG #c :ok5 !g* *!*@trusted.com".into())]
        );
    }

    #[test]
    fn highest_matching_event_level_is_selected_per_script_file() {
        use crate::irc::state::StateSnapshot;

        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "on *:TEXT:setup:#:auser 10 bob!*@*\n\
                 on 1:TEXT:go:#:msg #c one-1\n\
                 on 5:TEXT:go:#:msg #c one-5\n\
                 on 9:TEXT:go:#:msg #c one-9"
                    .into(),
            ),
            (
                "two.mrc".into(),
                "on 2:TEXT:go:#:msg #c two-2\n\
                 on 7:TEXT:go:#:msg #c two-7"
                    .into(),
            ),
        ]);
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(StateSnapshot {
                ial: vec![("bob".into(), "bob!u@example.test".into())],
                ..Default::default()
            }),
        };
        let text = |body: &str| EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: body.into(),
            params: vec![body.into()],
            ..Default::default()
        };

        engine.dispatch_event(&rctx, "TEXT", text("setup"));
        assert_eq!(
            engine.dispatch_event(&rctx, "TEXT", text("go")),
            vec![
                Action::Send("PRIVMSG #c :one-9".into()),
                Action::Send("PRIVMSG #c :two-7".into()),
            ]
        );
    }

    #[test]
    fn caret_and_ampersand_event_passes_follow_default_halt_state() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "on 1:TEXT:*:#:msg #c normal-one-$halted\n\
                 on ^1:TEXT:*:#:{ msg #c before-$halted | haltdef | msg #c after-$halted }"
                    .into(),
            ),
            (
                "two.mrc".into(),
                "on &1:TEXT:*:#:msg #c must-not-run\n\
                 on 1:TEXT:*:#:msg #c normal-two"
                    .into(),
            ),
        ]);
        let vars = EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "hello".into(),
            params: vec!["hello".into()],
            ..Default::default()
        };

        let (actions, halted) = engine.dispatch_event_halt(&ctx(), "TEXT", vars);
        assert!(halted);
        assert_eq!(
            actions,
            vec![
                Action::Send("PRIVMSG #c :before-$false".into()),
                Action::Send("PRIVMSG #c :after-$true".into()),
                Action::Send("PRIVMSG #c :normal-one-$true".into()),
                Action::Send("PRIVMSG #c :normal-two".into()),
            ]
        );
    }

    #[test]
    fn dollar_event_prefix_uses_regex_matchtext_with_colons() {
        let engine = ScriptEngine::new();
        engine.load("on $*:TEXT:/^foo:[0-9]+$/i:#:msg #c regex");
        let vars = EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "FOO:42".into(),
            params: vec!["FOO:42".into()],
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", vars),
            vec![Action::Send("PRIVMSG #c :regex".into())]
        );
    }

    #[test]
    fn mirc_me_bang_and_own_op_event_gates() {
        use crate::irc::state::{ChannelView, StateSnapshot};

        let engine = ScriptEngine::new();
        engine.load(
            "on 1:JOIN:#:{ /msg #c any-$nick }
             on !1:JOIN:#:{ /msg #c other-$nick }
             on me:*:JOIN:#:{ /msg #c self-$nick }
             on @1:JOIN:#:{ /msg #c ownop-$nick }",
        );
        let snap = StateSnapshot {
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["me".into(), "bob".into()],
                members: vec![("me".into(), "@".into()), ("bob".into(), String::new())],
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let join = |nick: &str| EventVars {
            nick: nick.into(),
            chan: "#c".into(),
            target: "#c".into(),
            ..Default::default()
        };

        assert_eq!(
            engine.dispatch_event(&rctx, "JOIN", join("bob")),
            vec![
                Action::Send("PRIVMSG #c :any-bob".into()),
                Action::Send("PRIVMSG #c :other-bob".into()),
                Action::Send("PRIVMSG #c :ownop-bob".into()),
            ]
        );
        assert_eq!(
            engine.dispatch_event(&rctx, "JOIN", join("me")),
            vec![
                Action::Send("PRIVMSG #c :any-me".into()),
                Action::Send("PRIVMSG #c :self-me".into()),
                Action::Send("PRIVMSG #c :ownop-me".into()),
            ]
        );

        let not_op = RunCtx {
            state: std::sync::Arc::new(StateSnapshot {
                channels: vec![ChannelView {
                    name: "#c".into(),
                    members: vec![("me".into(), String::new())],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..rctx
        };
        assert_eq!(
            engine.dispatch_event(&not_op, "JOIN", join("bob")),
            vec![
                Action::Send("PRIVMSG #c :any-bob".into()),
                Action::Send("PRIVMSG #c :other-bob".into()),
            ]
        );
    }

    #[test]
    fn event_matchtext_variables_and_channel_lists_are_evaluated() {
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:%match:%where,#two:{ /msg #audit matched-$chan }");
        engine.run_command(&ctx(), "", "/set %match *help*", &[]);
        engine.run_command(&ctx(), "", "/set %where #one", &[]);
        let text = |chan: &str, body: &str| EventVars {
            nick: "bob".into(),
            chan: chan.into(),
            target: chan.into(),
            text: body.into(),
            params: body.split_whitespace().map(String::from).collect(),
            ..Default::default()
        };

        assert_eq!(
            engine.dispatch_event(&ctx(), "TEXT", text("#two", "please help me")),
            vec![Action::Send("PRIVMSG #audit :matched-#two".into())]
        );
        assert!(engine
            .dispatch_event(&ctx(), "TEXT", text("#three", "please help me"))
            .is_empty());
        assert!(engine
            .dispatch_event(&ctx(), "TEXT", text("#one", "unrelated"))
            .is_empty());
    }

    #[test]
    fn timer_identifier() {
        use crate::script::eval::{ScriptTimers, TimerInfo};
        struct Fake;
        impl ScriptTimers for Fake {
            fn snapshot(&self) -> Vec<TimerInfo> {
                vec![
                    TimerInfo {
                        name: "greet".into(),
                        command: "/msg #c hi".into(),
                        reps: 3,
                        delay: 5,
                        ..Default::default()
                    },
                    TimerInfo {
                        name: "poll".into(),
                        command: "/who".into(),
                        reps: 0,
                        delay: 60,
                        ..Default::default()
                    },
                ]
            }
        }
        let engine = ScriptEngine::new();
        engine.set_timers(std::sync::Arc::new(Fake));
        engine.load("alias t { /msg #c $timer(0) $timer(greet).com $timer(greet).reps $timer(2) }");
        // 2 timers; greet's command + reps; the 2nd timer's name.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :2 /msg #c hi 3 poll".into())]
        );
    }

    #[test]
    fn if_multi_paren_and_or() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               if ($1 == a) && ($2 == b) /msg #c both\n\
               if ($1 == a) && ($2 == x) /msg #c nope\n\
               if ($1 == z) || ($2 == b) /msg #c either\n\
             }",
        );
        // `(a) && (b)` and `(a) || (b)` bracketed conditions both work.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "a b"),
            vec![
                Action::Send("PRIVMSG #c :both".into()),
                Action::Send("PRIVMSG #c :either".into()),
            ]
        );
    }

    #[test]
    fn question_input_identifier() {
        use crate::script::eval::ScriptInput;
        struct Fake(String);
        impl ScriptInput for Fake {
            fn prompt(&self, _: &str, _: &str, _: &str) -> Option<String> {
                Some(self.0.clone())
            }
        }
        // $? returns the answer + fills $!; $!name is delayed ($name literal);
        // $?! maps a non-empty answer to $true.
        let e = ScriptEngine::new();
        e.set_input(std::sync::Arc::new(Fake("banana".into())));
        e.load("alias t { /msg #c $?=\"fruit\" | /msg #c got $! and $!me yn $?!\"ok\" }");
        assert_eq!(
            e.run_alias(&ctx(), "#c", "t", ""),
            vec![
                Action::Send("PRIVMSG #c :banana".into()),
                Action::Send("PRIVMSG #c :got banana and $me yn $true".into()),
            ]
        );
        // A multi-word quoted prompt message stays intact (no trailing tokens).
        e.load("alias p { /msg #c pass=$?=\"Enter Password\" }");
        assert_eq!(
            e.run_alias(&ctx(), "#c", "p", ""),
            vec![Action::Send("PRIVMSG #c :pass=banana".into())]
        );
        // $$? halts the run when the answer is empty.
        let e2 = ScriptEngine::new();
        e2.set_input(std::sync::Arc::new(Fake(String::new())));
        e2.load("alias t { /msg #c one | var %x $$?\"q\" | /msg #c two }");
        assert_eq!(
            e2.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :one".into())]
        );
    }

    #[test]
    fn user_list_persists() {
        use crate::script::users::{AutoKind, UserList};
        let dir = std::env::temp_dir().join(format!("jirc-users-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let engine = ScriptEngine::new();
        engine.load("alias t { auser 10 *!*@save.com | aop on }");
        engine.run_alias(&rctx, "#c", "t", "");
        // A fresh load from disk sees the entry and the aop toggle.
        let loaded = UserList::load_from(&dir);
        assert_eq!(loaded.levels_for("nick!u@save.com"), "10");
        assert!(loaded.auto_enabled(AutoKind::Aop));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn autolist_commands_and_idents() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               aop on\n\
               aop *!*@friend.com #chan\n\
               avoice regular\n\
               /msg #c $aop $aop(0) $aop(*!*@friend.com).type $avoice(0)\n\
             }",
        );
        // aop enabled; 1 aop entry (channels #chan); 1 avoice entry.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :$true 1 #chan 1".into())]
        );
    }

    #[test]
    fn auto_op_on_join() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:!setup:#:{ aop on | aop *!*@trusted.com }");
        let snap = StateSnapshot {
            ial: vec![
                ("bob".into(), "bob!u@trusted.com".into()),
                ("eve".into(), "eve!u@evil.com".into()),
            ],
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["me".into(), "bob".into()],
                members: vec![("me".into(), "@".into())],
                bans: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let setup = EventVars {
            nick: "x".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "!setup".into(),
            params: vec!["!setup".into()],
            ..Default::default()
        };
        engine.dispatch_event(&rctx, "TEXT", setup);
        let join = |n: &str| EventVars {
            nick: n.into(),
            chan: "#c".into(),
            target: "#c".into(),
            ..Default::default()
        };
        // I'm @op and bob matches the aop list -> auto-op him.
        assert_eq!(
            engine.dispatch_event(&rctx, "JOIN", join("bob")),
            vec![Action::Send("MODE #c +o bob".into())]
        );
        // eve doesn't match -> nothing queued.
        assert_eq!(engine.dispatch_event(&rctx, "JOIN", join("eve")), vec![]);
    }

    #[test]
    fn protect_reop_on_deop() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:!setup:#:{ protect on | protect vip }");
        let snap = StateSnapshot {
            ial: vec![("vip".into(), "vip!u@vip.com".into())],
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["me".into(), "vip".into()],
                members: vec![("me".into(), "@".into())],
                bans: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let setup = EventVars {
            nick: "x".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "!setup".into(),
            params: vec!["!setup".into()],
            ..Default::default()
        };
        engine.dispatch_event(&rctx, "TEXT", setup);
        let deop = |who: &str| EventVars {
            nick: "baddie".into(),
            knick: who.into(),
            chan: "#c".into(),
            target: "#c".into(),
            ..Default::default()
        };
        // vip is protected -> re-op; rando isn't -> nothing.
        assert_eq!(
            engine.dispatch_event(&rctx, "DEOP", deop("vip")),
            vec![Action::Send("MODE #c +o vip".into())]
        );
        assert_eq!(engine.dispatch_event(&rctx, "DEOP", deop("rando")), vec![]);

        let already_restored = StateSnapshot {
            ial: vec![("vip".into(), "vip!u@vip.com".into())],
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["me".into(), "vip".into()],
                members: vec![("me".into(), "@".into()), ("vip".into(), "@".into())],
                ..Default::default()
            }],
            ..Default::default()
        };
        let restored_ctx = RunCtx {
            state: std::sync::Arc::new(already_restored),
            ..rctx
        };
        assert_eq!(
            engine.dispatch_event(&restored_ctx, "DEOP", deop("vip")),
            vec![]
        );
    }

    #[test]
    fn var_introspection() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               set %fruit.apple red\n\
               set %fruit.grape purple\n\
               set %other 1\n\
               /msg #c $var(%fruit*,0) $var(%fruit*,1) $var(%fruit*,1).value\n\
             }",
        );
        // 2 match; sorted, %fruit.apple is 1st; its value is red.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :2 %fruit.apple red".into())]
        );
    }

    #[test]
    fn dynamic_variable_brackets() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               set %color.John blue\n\
               var %who John\n\
               /msg #c %color. [ $+ [ %who ] ] and %color. [ $+ [ $1 ] ]\n\
             }",
        );
        // `%color. [ $+ [ x ] ]` reads the variable %color.<value of x>.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", "John"),
            vec![Action::Send("PRIVMSG #c :blue and blue".into())]
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
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t1", ""),
            vec![Action::Send("PRIVMSG #c :hello".into())]
        );
        // …and the default when the value is empty.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t2", ""),
            vec![Action::Send("PRIVMSG #c :none".into())]
        );
        // A comparison sets both operands.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t3", ""),
            vec![Action::Send("PRIVMSG #c :3-3".into())]
        );
        // $v1/$v2 also come from an `if` comparison (here a binary word operator).
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t4", ""),
            vec![Action::Send("PRIVMSG #c :foo in foobar".into())]
        );
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

    /// The popup files jIRC seeds on first run, concatenated as the engine
    /// compiles them. Testing the real shipped bytes means the defaults users
    /// actually get are the ones verified here.
    const EXAMPLE_POPUPS: &str = concat!(
        include_str!("examples/popups-status.msl"),
        "
",
        include_str!("examples/popups-channel.msl"),
        "
",
        include_str!("examples/popups-nicklist.msl"),
        "
",
        include_str!("examples/popups-query.msl"),
    );

    #[test]
    fn shipped_popup_examples_build_correctly() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        // Two joined channels, and we hold ops on the active one.
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![
                ChannelView {
                    name: "#one".into(),
                    nicks: vec!["me".into(), "bob".into()],
                    members: vec![("me".into(), "@".into()), ("bob".into(), String::new())],
                    mode: "+nt".into(),
                    bans: vec!["*!*@bad.example".into(), "spam!*@*".into()],
                    ..Default::default()
                },
                ChannelView {
                    name: "#two".into(),
                    ..Default::default()
                },
            ],
            // $address / $mask read the internal address list.
            ial: vec![("bob".into(), "bob!~user@host.example.com".into())],
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let engine = ScriptEngine::new();
        engine.load(EXAMPLE_POPUPS);

        let items = engine.popups_evaluated(&rctx, "channel", "bob", "#one");
        let by = |l: &str| {
            items
                .iter()
                .find(|i| i.label == l)
                .unwrap_or_else(|| panic!("missing {l}: {items:#?}"))
        };
        // The greyed information line reads live channel state.
        let info = by("#one has 2 users");
        assert!(info.disabled && info.command.is_empty());

        // Mode items tick from the channel's actual mode string (+nt here) and
        // toggle the opposite way when already set.
        let modes = by("Modes");
        assert!(!modes.disabled, "we hold ops, so this must not be greyed");
        let mode_item = |l: &str| {
            modes
                .children
                .iter()
                .find(|i| i.label == l)
                .unwrap_or_else(|| panic!("missing mode {l}: {:#?}", modes.children))
        };
        assert!(mode_item("No external messages").checked, "+n is set");
        assert!(mode_item("Topic ops only").checked, "+t is set");
        assert!(!mode_item("Moderated").checked, "+m is not set");
        // Commands stay unexpanded so the toggle re-evaluates on each click.
        assert!(mode_item("Moderated").command.contains("$iif("));
        // Running it resolves against live state: +n is set, so this removes it.
        assert_eq!(
            engine.run_popup_command(
                &rctx,
                "#one",
                &mode_item("No external messages").command.clone(),
                &[],
                &[],
                "",
                "channel",
                "#one",
            ),
            vec![Action::Send("MODE #one -n".into())]
        );
        // And +m is not set, so this one adds it.
        assert_eq!(
            engine.run_popup_command(
                &rctx, "#one", &mode_item("Moderated").command.clone(),
                &[], &[], "", "channel", "#one",
            ),
            vec![Action::Send("MODE #one +m".into())]
        );

        // The ban list is built from live state by $submenu.
        let bans = by("Bans (2)");
        let ban_labels: Vec<&str> = bans
            .children
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(
            ban_labels,
            ["Unban *!*@bad.example", "Unban spam!*@*"],
            "got {:#?}",
            bans.children
        );
        // Unlike static items, $submenu-generated commands are already
        // expanded — they come from running the alias that produced them.
        assert_eq!(bans.children[1].command, "mode #one -b *!*@bad.example");

        // Channel list carries each channel's user count.
        let jump = by("Jump to");
        let jump_labels: Vec<&str> = jump
            .children
            .iter()
            .filter(|i| !i.separator)
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(jump_labels, ["#one (2)", "#two (0)"]);

        // Running an item expands it against the live context.
        assert_eq!(
            engine.run_popup_command(
                &rctx, "#one", "who $chan", &["bob".into()], &["bob".into()],
                "", "channel", "#one",
            ),
            vec![Action::Send("WHO #one".into())]
        );

        // Nick list: bob holds no prefix, so the toggle offers to GIVE ops.
        let nl = engine.popups_evaluated(&rctx, "nicklist", "bob", "#one");
        assert!(nl.iter().any(|i| i.label == "Whois bob"), "got {nl:#?}");
        assert!(
            nl.iter().any(|i| i.label == "Give ops"),
            "expected the give-ops form for an unprefixed nick: {nl:#?}"
        );
        // The mask submenu previews every $mask type against the real address.
        let masks = nl
            .iter()
            .find(|i| i.label == "Ban with mask")
            .unwrap_or_else(|| panic!("mask submenu missing: {nl:#?}"));
        let first = masks
            .children
            .iter()
            .find(|i| !i.separator)
            .expect("at least one mask");
        assert!(
            first.label.starts_with("0 ") && first.command.starts_with("mode #one +b "),
            "got {first:#?}"
        );
        // Multi-select group greys out with a single nick chosen.
        let sel = nl
            .iter()
            .find(|i| i.label.starts_with("Selected "))
            .unwrap_or_else(|| panic!("selected group missing; got {nl:#?}"));
        assert!(sel.disabled, "single selection should grey the group");
    }

    #[test]
    fn popup_engine_handles_depth_styles_and_separators() {
        let engine = ScriptEngine::new();
        engine.load(
            "menu channel {\n\
             Top:echo top\n\
             -\n\
             Parent\n\
             .Child:echo child\n\
             .Deeper\n\
             ..Deepest:echo deepest\n\
             $style(1) Checked:echo checked\n\
             $style(2) Greyed:echo greyed\n\
             $style(3) Both:echo both\n\
             Dyn $+ amic:echo dynamic\n\
             }",
        );
        let items = engine.popups_evaluated(&ctx(), "channel", "bob", "#a");
        let by = |l: &str| {
            items
                .iter()
                .find(|i| i.label == l)
                .unwrap_or_else(|| panic!("missing {l}: {items:?}"))
        };
        // A separator survives as its own item.
        assert!(items.iter().any(|i| i.separator), "separator kept");
        // Three levels of nesting via leading dots.
        let parent = by("Parent");
        assert_eq!(parent.children.len(), 2);
        let deeper = parent
            .children
            .iter()
            .find(|i| i.label == "Deeper")
            .expect("Deeper present");
        assert_eq!(deeper.children.len(), 1);
        assert_eq!(deeper.children[0].label, "Deepest");
        assert_eq!(deeper.children[0].command, "echo deepest");
        // $style marks are applied and stripped from the visible label.
        assert!(by("Checked").checked && !by("Checked").disabled);
        assert!(by("Greyed").disabled && !by("Greyed").checked);
        assert!(by("Both").checked && by("Both").disabled);
        // Labels are expanded, so $+ concatenation works.
        assert_eq!(by("Dynamic").command, "echo dynamic");
    }

    #[test]
    fn submenu_expands_dynamically_like_mirc() {
        // The KB's own example: the identifier is called with `begin`, then an
        // incrementing integer, then `end`, and each numbered call returns a
        // `label:command` popup line.
        let engine = ScriptEngine::new();
        engine.load(
            "menu status {\n\
             Animal\n\
             .$submenu($animal($1))\n\
             }\n\
             alias animal {\n\
               if ($1 == begin) return -\n\
               if ($1 == 1) return Cow:echo Cow\n\
               if ($1 == 2) return Llama:echo Llama\n\
               if ($1 == 3) return Emu:echo Emu\n\
               if ($1 == end) return -\n\
             }",
        );
        let items = engine.popups_evaluated(&ctx(), "status", "", "");
        let animal = items
            .iter()
            .find(|i| i.label == "Animal")
            .expect("Animal parent present");
        let kids = &animal.children;
        // begin -> separator, three animals, end -> separator.
        assert_eq!(kids.len(), 5, "got {kids:?}");
        assert!(kids[0].separator);
        assert!(kids[4].separator);
        let labels: Vec<&str> = kids[1..4].iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Cow", "Llama", "Emu"]);
        let commands: Vec<&str> = kids[1..4].iter().map(|i| i.command.as_str()).collect();
        assert_eq!(commands, ["echo Cow", "echo Llama", "echo Emu"]);
        // Iteration stops as soon as a numbered call returns nothing, so an
        // alias with no terminating case cannot run away.
        let bounded = ScriptEngine::new();
        bounded.load(
            "menu status {\n\
             Few\n\
             .$submenu($two($1))\n\
             }\n\
             alias two {\n\
               if ($1 == 1) return One:echo 1\n\
               if ($1 == 2) return Two:echo 2\n\
             }",
        );
        let few = bounded.popups_evaluated(&ctx(), "status", "", "");
        let kids = &few
            .iter()
            .find(|i| i.label == "Few")
            .expect("Few present")
            .children;
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].label, "One");
        assert_eq!(kids[1].label, "Two");
    }

    #[test]
    fn submenu_arg_parse() {
        use super::parse_submenu_arg;
        assert_eq!(
            parse_submenu_arg("$submenu($animal($1))").as_deref(),
            Some("$animal($1)")
        );
        assert_eq!(
            parse_submenu_arg("  $SubMenu($x($1)) ").as_deref(),
            Some("$x($1)")
        );
        assert_eq!(parse_submenu_arg("Plain:cmd"), None);
    }

    #[test]
    fn style_marker_split() {
        use super::split_style_marker;
        let m = crate::script::eval::STYLE_MARK;
        assert_eq!(
            split_style_marker(&format!("{m}3 Both")),
            (true, true, " Both")
        );
        assert_eq!(
            split_style_marker(&format!("  {m}2 Off")),
            (false, true, " Off")
        );
        assert_eq!(split_style_marker("Plain"), (false, false, "Plain"));
        // A bare marker (no digit) is dropped, no style applied.
        assert_eq!(split_style_marker(&format!("{m} x")), (false, false, " x"));
    }

    #[test]
    fn break_and_continue() {
        // /break exits the loop: msgs 1, 2 then breaks at 3.
        let engine = ScriptEngine::new();
        engine.load(
            "alias b { set %i 0 | while (%i < 5) { inc %i | if (%i == 3) break | msg #c %i } }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "b", ""),
            vec![
                Action::Send("PRIVMSG #c :1".into()),
                Action::Send("PRIVMSG #c :2".into()),
            ]
        );
        // /continue skips the first two iterations: msgs 3, 4, 5.
        let engine2 = ScriptEngine::new();
        engine2.load(
            "alias c { set %i 0 | while (%i < 5) { inc %i | if (%i < 3) continue | msg #c %i } }",
        );
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
        engine3.load("alias t { bset &v 1 65 66 67 | bwrite -c jirc_bin_rt.bin 0 -1 &v | bread jirc_bin_rt.bin 0 3 &w | msg #c $bvar(&w,1,3) }");
        assert_eq!(
            engine3.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :65 66 67".into())]
        );

        // Numeric views honour mIRC's host/network byte orders, and text
        // searches are caseless unless `.textcs` is requested.
        let engine4 = ScriptEngine::new();
        engine4.load(
            "alias t { bset &v 1 12 34 56 78 119 97 118 87 65 86 | msg #c $bvar(&v,1).word $bvar(&v,1).nword $bvar(&v,1).long $bvar(&v,1).nlong $bfind(&v,5,WAV).text $bfind(&v,5,WAV).textcs $bfind(&v,5,/wav/ig,rx).regex }",
        );
        assert_eq!(
            engine4.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send(
                "PRIVMSG #c :8716 3106 1312301580 203569230 5 8 2".into()
            )]
        );
    }

    #[test]
    fn binary_files_and_variables_follow_mirc_offsets_and_lifetime() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {
                bwrite -tc mirc_offsets.bin 0 -1 ABC
                bread mirc_offsets.bin 1 1 &at1
                bwrite mirc_offsets.bin 1 1 Z
                bread mirc_offsets.bin 0 3 &whole
                bset &tail 1 0 255
                bwrite mirc_offsets.bin -1 -1 &tail
                fopen -o fh mirc_fwrite.bin
                fwrite -b fh &tail
                fclose fh
                bread mirc_fwrite.bin 0 2 &fhread
                bset &find 1 88 87 65 86 89
                bset &nul 1 65 0 66
                msg #c at1=$bvar(&at1,1) whole=$bvar(&whole,1,3).text fh=$bvar(&fhread,1,2) find=$bfind(&find,1,87 65 86) nul=[ $+ $bvar(&nul,1,3).text $+ ]
            }
            alias after { msg #c [ $+ $bvar(&whole,0) $+ ] }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send(
                "PRIVMSG #c :at1=66 whole=AZC fh=0 255 find=2 nul=[A]".into()
            )]
        );
        // &binvars are destroyed when the outer routine finishes.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "after", ""),
            vec![Action::Send("PRIVMSG #c :[]".into())]
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
                Action::SockOpen {
                    name: "bot".into(),
                    host: "irc.example.org".into(),
                    port: 6667,
                    tls: true,
                    accept_invalid: false,
                    bind_ip: String::new(),
                    nodelay: false,
                    ip_version: 0,
                    reservation_id: 0,
                },
                Action::SockWrite {
                    name: "bot".into(),
                    data: b"NICK x\r\n".to_vec()
                },
                Action::SockClose { name: "bot".into() },
            ]
        );
    }

    #[test]
    fn socklisten_dash_d_bindip_registers_under_the_name() {
        // mIRC's `/socklisten -d <bindip> <name>` — the bind IP must NOT be taken
        // as the socket name, or `$sock(name).port` reads blank (breaking a local
        // bridge that then does `/server 127.0.0.1 $sock(name).port`).
        let engine = ScriptEngine::new();
        engine.load("alias go { /socklisten -d 127.0.0.1 lsn }");
        let actions = engine.run_alias(&ctx(), "#c", "go", "");
        assert_eq!(
            actions,
            vec![Action::SockListen {
                name: "lsn".into(),
                listener_id: 0,
            }]
        );
    }

    #[test]
    fn server_command_emits_connect_action() {
        // `/server [-m] <host> <port> [pass]` — a script connecting the native
        // client (e.g. a local bridge). `-m` requests a new server window.
        let engine = ScriptEngine::new();
        engine.load(
            "alias new { /server -m 127.0.0.1 50641 mykey }\n\
             alias reuse { /server 127.0.0.1 50642 otherkey }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "new", ""),
            vec![Action::Server {
                host: "127.0.0.1".into(),
                port: 50641,
                pass: "mykey".into(),
                new_window: true,
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "reuse", ""),
            vec![Action::Server {
                host: "127.0.0.1".into(),
                port: 50642,
                pass: "otherkey".into(),
                new_window: false,
            }]
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
    fn i7n_decode_preserves_escaped_and_high_bytes() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias i7n_decode {\n\
             var %input = $1-, %out, %i = 1\n\
             while (%i <= $len(%input)) {\n\
             var %char = $mid(%input,%i,1), %byte\n\
             if (%char == $chr(92)) {\n\
             inc %i\n\
             if (%i > $len(%input)) return\n\
             var %esc = $mid(%input,%i,1)\n\
             if (%esc == 0) var %byte = 0\n\
             elseif (%esc == t) var %byte = 9\n\
             elseif (%esc == $chr(92)) var %byte = 92\n\
             else return\n\
             }\n\
             else var %byte = $asc(%char)\n\
             var %out = $+(%out,$iif($len(%out),$chr(32)),%byte)\n\
             inc %i\n\
             }\n\
             return %out\n\
             }\n\
             alias t {\n\
             var %bs = $chr(92)\n\
             var %w = $+(GKSSP,%bs,0,%bs,0,%bs,0,$chr(3),%bs,0,%bs,0,%bs,0,$chr(2),%bs,0,%bs,0,%bs,0,$chr(200),$chr(150),$chr(77),$chr(88),$chr(99),$chr(111),$chr(222),$chr(133))\n\
             /msg #c len= $len(%w) dec= $i7n_decode(%w)\n\
             }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert_eq!(
            actions,
            vec![Action::Send(
                "PRIVMSG #c :len= 33 dec= 71 75 83 83 80 0 0 0 3 0 0 0 2 0 0 0 200 150 77 88 99 111 222 133"
                    .into()
            )]
        );
    }

    #[test]
    fn sockread_binvar_preserves_exact_bytes() {
        // `sockread &binvar` delivers the line's exact bytes — including a null and
        // high bytes (0xC3, 0xFF) that a UTF-8 round-trip would corrupt. This is
        // what a binary crypto handshake needs; a text `sockread %var` can't.
        let engine = ScriptEngine::new();
        engine.load(
            "on *:SOCKREAD:bot:{ sockread &c | /msg #c len $bvar(&c,0) a $bvar(&c,1) b $bvar(&c,2) d $bvar(&c,3) br $sockbr }",
        );
        let vars = EventVars {
            chan: "bot".into(),
            target: "bot".into(),
            sock_bytes: vec![0u8, 195u8, 255u8],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "SOCKREAD", vars);
        assert_eq!(
            actions,
            vec![Action::Send(
                "PRIVMSG #c :len 3 a 0 b 195 d 255 br 3".into()
            )]
        );
    }

    #[test]
    fn captured_irc7_gkssp_challenge_produces_auth_reply() {
        let engine = ScriptEngine::new();
        engine.load(
            r#"
            alias -l i7n_authreply {
              var %gkid = $upper($1), %raw = $2, %mark = $bfind(%raw,1,$+($chr(32),S,$chr(32),:))
              if (!%mark) return
              var %bytes = $i7n_decode(%raw,$calc(%mark + 4))
              if ($numtok(%bytes,32) != 24) return
              if ($gettok(%bytes,1-8,32) != 71 75 83 83 80 0 0 0) return
              if ($gettok(%bytes,10-12,32) != 0 0 0) return
              if ($gettok(%bytes,13-16,32) != 2 0 0 0) return
              var %version = $gettok(%bytes,9,32)
              if (!$istok(3 4,%version,32)) return
              var %hmac = $i7n_hmac($gettok(%bytes,17-24,32)), %guid = $i7n_guidwire(%gkid)
              if ((!$regex(%hmac,/^[0-9A-F]{32}$/)) || (!$regex(%guid,/^[0-9A-F]{32}$/))) return
              return $i7n_escape(71 75 83 83 80 0 0 0 %version 0 0 0 3 0 0 0 $i7n_hexbytes(%hmac) $i7n_hexbytes(%guid))
            }
            alias -l i7n_decode {
              var %input = $1, %i = $2, %end = $bvar(%input,0), %out
              while (%i <= %end) {
                var %byte = $bvar(%input,%i)
                if (%byte == 92) {
                  inc %i
                  if (%i > %end) return
                  var %esc = $bvar(%input,%i)
                  if (%esc == 48) var %byte = 0
                  elseif (%esc == 116) var %byte = 9
                  elseif (%esc == 110) var %byte = 10
                  elseif (%esc == 114) var %byte = 13
                  elseif (%esc == 98) var %byte = 32
                  elseif (%esc == 99) var %byte = 44
                  elseif (%esc == 92) var %byte = 92
                  else return
                }
                var %out = $+(%out,$iif($len(%out),$chr(32)),%byte)
                inc %i
              }
              return %out
            }
            alias -l i7n_hmac {
              bunset &hmac
              bset &hmac 1 $1-
              bset &hmac 9 $i7n_serialize(irc.irc7.com)
              var %hex = $hmac(&hmac,SRFMKSJANDRESKKC,md5,1)
              bunset &hmac
              return $upper(%hex)
            }
            alias -l i7n_guidwire {
              var %hex = $upper($1)
              return $+($mid(%hex,7,2),$mid(%hex,5,2),$mid(%hex,3,2),$mid(%hex,1,2),$mid(%hex,11,2),$mid(%hex,9,2),$mid(%hex,15,2),$mid(%hex,13,2),$mid(%hex,17))
            }
            alias -l i7n_hexbytes {
              var %hex = $upper($1), %out, %i = 1
              while (%i <= $len(%hex)) { var %out = $+(%out,$iif($len(%out),$chr(32)),$base($mid(%hex,%i,2),16,10)) | inc %i 2 }
              return %out
            }
            alias -l i7n_escape {
              var %out, %i = 1
              while (%i <= $numtok($1-,32)) { var %out = %out $+ $i7n_byteesc($gettok($1-,%i,32)) | inc %i }
              return %out
            }
            alias -l i7n_byteesc {
              if ($1 == 0) return \0
              if ($1 == 9) return \t
              if ($1 == 10) return \n
              if ($1 == 13) return \r
              if ($1 == 32) return \b
              if ($1 == 44) return \c
              if ($1 == 92) return $str($chr(92),2)
              return $chr($1)
            }
            alias -l i7n_serialize { return $regsubex($1-,/(*UTF8)(.)/g,$asc(\1) $+ $chr(32)) }
            on *:SOCKREAD:bot:{
              bunset &line
              sockread -n &line
              var %reply = $i7n_authreply(01110001111010101100000110111000,&line)
              bunset &out
              bset &out 1 $i7n_serialize(AUTH,GateKeeper,S,$+(:,%reply))
              bset &out $calc($bvar(&out,0) + 1) 13 10
              sockwrite bot &out
            }
            "#,
        );
        // Fresh challenge captured from irc.irc7.com. The final eight bytes are
        // deliberately invalid UTF-8 and must survive the socket/binvar path.
        let line =
            b"AUTH GateKeeper S :GKSSP\\0\\0\\0\x03\\0\\0\\0\x02\\0\\0\\0f\x8b\xb0\xd5\xfa!Fk";
        let vars = EventVars {
            chan: "bot".into(),
            target: "bot".into(),
            sock_bytes: line.to_vec(),
            ..Default::default()
        };
        let expected_hex = "4155544820476174654b65657065722053203a474b5353505c305c305c30035c305c305c30035c305c305c3080b07828e658d4292ba4705fbd055c6e0b015c30110110111010115c305c30011011105c300d0a";
        let expected = (0..expected_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&expected_hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(
            engine.dispatch_event(&ctx(), "SOCKREAD", vars),
            vec![Action::SockWrite {
                name: "bot".into(),
                data: expected
            }]
        );
    }

    #[test]
    fn sockread_text_then_binary_does_not_duplicate_the_same_data() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:SOCKREAD:bot:{ sockread %line | sockread &raw | /msg #c text %line binary $bvar(&raw,0) br $sockbr }",
        );
        let vars = EventVars {
            chan: "bot".into(),
            target: "bot".into(),
            text: "PING".into(),
            sock_bytes: b"PING".to_vec(),
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "SOCKREAD", vars),
            vec![Action::Send("PRIVMSG #c :text PING binary 0 br 0".into())]
        );
    }

    #[test]
    fn sockerr_reflects_the_current_socket_event() {
        let engine = ScriptEngine::new();
        engine.load("on *:SOCKOPEN:bot:{ /msg #c error $sockerr }");
        let vars = EventVars {
            chan: "bot".into(),
            target: "bot".into(),
            sock_error: 10061,
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "SOCKOPEN", vars),
            vec![Action::Send("PRIVMSG #c :error 10061".into())]
        );
    }

    #[test]
    fn sockread_only_fires_for_matching_name() {
        let engine = ScriptEngine::new();
        engine.load("on *:SOCKREAD:bot:{ /msg #c hit }");
        let other = EventVars {
            chan: "other".into(),
            target: "other".into(),
            ..Default::default()
        };
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
            vec![Action::SockWrite {
                name: "sk".into(),
                data: vec![72, 105, 33]
            }]
        );
    }

    #[test]
    fn sockwrite_honours_mirc_binary_count_and_newline_rules() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {
                bset &v 1 72 105 33
                sockwrite -n sk &v
                sockwrite -b sk 2 &v
                sockwrite -n sk $+(PING,$crlf)
                sockwrite -nt sk &v
            }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![
                Action::SockWrite {
                    name: "sk".into(),
                    data: b"Hi!".to_vec()
                },
                Action::SockWrite {
                    name: "sk".into(),
                    data: b"Hi".to_vec()
                },
                Action::SockWrite {
                    name: "sk".into(),
                    data: b"PING\r\n".to_vec()
                },
                Action::SockWrite {
                    name: "sk".into(),
                    data: b"&v\r\n".to_vec()
                },
            ]
        );
    }

    #[test]
    fn sockopen_dash_d_consumes_the_bind_address() {
        let engine = ScriptEngine::new();
        engine.load("alias t { sockopen -d 127.0.0.1 bot example.org 80 }");
        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![Action::SockOpen {
                name: "bot".into(),
                host: "example.org".into(),
                port: 80,
                tls: false,
                accept_invalid: false,
                bind_ip: "127.0.0.1".into(),
                nodelay: false,
                ip_version: 0,
                reservation_id: 0,
            }]
        );
    }

    #[test]
    fn webview_commands_produce_managed_browser_actions() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t {\n\
               webview -o @auth passport-a 980 720 about:blank IRC7 Passport Login |\n\
               webview -n @auth https://api.irc7.com/api/auth/login |\n\
               webview -k @auth https://www.irc7.com/ |\n\
               webview -f @auth |\n\
               webview -c @auth\n\
             }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![
                Action::WebviewOpen {
                    name: "@auth".into(),
                    profile: "passport-a".into(),
                    width: 980,
                    height: 720,
                    url: "about:blank".into(),
                    title: "IRC7 Passport Login".into(),
                },
                Action::WebviewNavigate {
                    name: "@auth".into(),
                    url: "https://api.irc7.com/api/auth/login".into(),
                },
                Action::WebviewCookies {
                    name: "@auth".into(),
                    url: "https://www.irc7.com/".into(),
                },
                Action::WebviewFocus {
                    name: "@auth".into(),
                },
                Action::WebviewClose {
                    name: "@auth".into(),
                },
            ]
        );
    }

    #[test]
    fn webview_identifier_reads_cached_manager_snapshot() {
        struct FakeWebviews;
        impl crate::script::eval::ScriptWebviews for FakeWebviews {
            fn snapshot(&self, _: &str) -> Vec<crate::script::eval::WebviewInfo> {
                vec![crate::script::eval::WebviewInfo {
                    name: "@auth".into(),
                    profile: "passport-a".into(),
                    status: "ready".into(),
                    url: "https://www.irc7.com/".into(),
                }]
            }
        }

        let engine = ScriptEngine::new();
        engine.set_webviews(std::sync::Arc::new(FakeWebviews));
        engine.load(
            "alias t { echo -s count=$webview(0) name=$webview(@AUTH) status=$webview(@auth).status profile=$webview(@auth).profile url=$webview(@auth).url }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text:
                    "count=1 name=@auth status=ready profile=passport-a url=https://www.irc7.com/"
                        .into(),
            }]
        );
    }

    #[test]
    fn webview_event_matches_name_and_preserves_cookie_value() {
        let engine = ScriptEngine::new();
        engine.load("on *:WEBVIEW:@auth:{ echo -s event=$1 name=$2 value=$3- target=$target }");
        let vars = EventVars {
            chan: "@auth".into(),
            target: "@auth".into(),
            text: "cookie ticket abc=def==".into(),
            params: vec!["cookie".into(), "ticket".into(), "abc=def==".into()],
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "WEBVIEW", vars),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "event=cookie name=ticket value=abc=def== target=@auth".into(),
            }]
        );
    }

    #[test]
    fn native_i7_updater_commits_and_selects_complete_credentials() {
        fn updater_ctx(data_dir: std::path::PathBuf) -> RunCtx<'static> {
            RunCtx {
                my_nick: "me",
                network: "Net",
                server: "irc.example.org",
                data_dir,
                state: std::sync::Arc::new(Default::default()),
            }
        }

        fn webview_event(params: &[&str]) -> EventVars {
            EventVars {
                chan: "i7update".into(),
                target: "i7update".into(),
                text: params.join(" "),
                params: params.iter().map(|value| (*value).to_string()).collect(),
                ..Default::default()
            }
        }

        fn finish_credentials(
            engine: &ScriptEngine,
            rctx: &RunCtx<'_>,
            ticket: &str,
            profile: &str,
        ) -> Vec<Action> {
            engine.dispatch_event(
                rctx,
                "WEBVIEW",
                webview_event(&["cookie", "ticket", ticket]),
            );
            engine.dispatch_event(
                rctx,
                "WEBVIEW",
                webview_event(&["cookie", "profile", profile]),
            );
            engine.dispatch_event(rctx, "WEBVIEW", webview_event(&["cookies_done"]))
        }

        let data_dir =
            std::env::temp_dir().join(format!("jirc-i7updater-explicit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();
        let rctx = updater_ctx(data_dir.clone());
        let engine = ScriptEngine::new();
        engine.load(include_str!("../../tests/fixtures/msl-compat/i7updater.msl"));
        assert!(engine.has_alias("i7update"));

        let actions = engine.run_alias(&rctx, "", "i7update", "PassportOne pick client");
        assert!(actions.iter().any(|action| matches!(
            action,
            Action::WebviewOpen {
                name,
                profile,
                url,
                ..
            } if name == "i7update" && profile == "passportone" && url == "about:blank"
        )));

        let opened = EventVars {
            chan: "i7update".into(),
            target: "i7update".into(),
            text: "opened".into(),
            params: vec!["opened".into()],
            ..Default::default()
        };
        let actions = engine.dispatch_event(&rctx, "WEBVIEW", opened);
        assert!(actions.iter().any(|action| matches!(
            action,
            Action::WebviewNavigate { name, url }
                if name == "i7update" && url == "https://login.live.com/logout.srf"
        )));

        finish_credentials(&engine, &rctx, "ticket-one", "profile-one");
        let settings = std::fs::read_to_string(data_dir.join("settings.ini")).unwrap();
        assert_eq!(
            ini::read(&settings, "settings", "curpp").as_deref(),
            Some("PassportOne")
        );
        assert_eq!(
            ini::read(&settings, "pp.PassportOne", "ticket").as_deref(),
            Some("ticket-one")
        );
        assert_eq!(
            ini::read(&settings, "pp.PassportOne", "profile").as_deref(),
            Some("profile-one")
        );
        let _ = std::fs::remove_dir_all(&data_dir);

        // With no explicit target, infer client only when it is the sole
        // registered role missing a selection; preserve the existing bot.
        let inferred_dir =
            std::env::temp_dir().join(format!("jirc-i7updater-inferred-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&inferred_dir).unwrap();
        std::fs::write(
            inferred_dir.join("settings.ini"),
            "[settings]\nmode=registered\nbotmode=registered\nbotpp=ExistingBot\n",
        )
        .unwrap();
        let inferred_ctx = updater_ctx(inferred_dir.clone());
        let inferred = ScriptEngine::new();
        inferred.load(include_str!("../../tests/fixtures/msl-compat/i7updater.msl"));
        inferred.run_alias(&inferred_ctx, "", "i7update", "PassportTwo");
        finish_credentials(&inferred, &inferred_ctx, "ticket-two", "profile-two");

        let settings = std::fs::read_to_string(inferred_dir.join("settings.ini")).unwrap();
        assert_eq!(
            ini::read(&settings, "settings", "curpp").as_deref(),
            Some("PassportTwo")
        );
        assert_eq!(
            ini::read(&settings, "settings", "botpp").as_deref(),
            Some("ExistingBot")
        );
        let _ = std::fs::remove_dir_all(&inferred_dir);
    }

    #[test]
    fn sockudp_parses_bind_port_binary_count_and_keep_switch() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias t { bset &packet 1 1 2 3 | sockudp -kbnd 127.0.0.1 datagram 4567 127.0.0.2 9000 2 &packet }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![Action::SockUdp {
                name: "datagram".into(),
                bind_ip: "127.0.0.1".into(),
                local_port: 4567,
                dest_ip: "127.0.0.2".into(),
                dest_port: 9000,
                data: vec![1, 2],
                keep: true,
                dual_stack: false,
                reservation_id: 0,
            }]
        );
    }

    #[test]
    fn state_aware_identifiers() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![
                ChannelView {
                    name: "#a".into(),
                    nicks: vec!["me".into(), "bob".into()],
                    ..Default::default()
                },
                ChannelView {
                    name: "#b".into(),
                    nicks: vec!["me".into()],
                    ..Default::default()
                },
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
        engine.load(
            "dialog g {\n title \"Hi\"\n size -1 -1 360 240\n edit name\n}\n\
             alias o { /dialog g }\n\
             alias info { /echo $dialog(0) $dialog(g).title $dialog(g).table $dialog(g).w $dialog(g).h }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "o", "");
        match &actions[..] {
            [Action::DialogOpen {
                name,
                title,
                controls,
                width,
                height,
            }] => {
                assert_eq!(name, "g");
                assert_eq!(title, "Hi");
                assert_eq!(controls.len(), 1);
                assert_eq!(controls[0].id, "name");
                assert_eq!((*width, *height), (360, 240));
            }
            _ => panic!("expected DialogOpen, got {actions:?}"),
        }
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "info", ""),
            vec![Action::Echo {
                target: "#c".into(),
                text: "1 Hi g 360 240".into(),
            }]
        );
        let did = engine.run_command(
            &ctx(),
            "#c",
            "/did -a g name one | /did -i g name 1 zero | /did -b g name",
            &[],
        );
        assert!(did.iter().any(|action| matches!(
            action,
            Action::DialogSet { op, value, .. } if op == "insert" && value == "1 zero"
        )));
        assert!(did.iter().any(|action| matches!(
            action,
            Action::DialogSet { op, .. } if op == "disable"
        )));
    }

    #[test]
    fn dialog_event_reads_values_and_acts() {
        let engine = ScriptEngine::new();
        engine
            .load("on *:DIALOG:g:sclick:send:{ /msg #c $dname $devent $did hi $did(g, name) | /dialog -c g }");
        let mut vals = std::collections::HashMap::new();
        vals.insert("name".to_string(), "bob".to_string());
        let vars = EventVars {
            chan: "g".into(),
            target: "g".into(),
            text: "send".into(),
            params: vec!["send".into()],
            did: vals,
            dialog_name: "g".into(),
            dialog_event: "sclick".into(),
            dialog_control: "send".into(),
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx(), "DIALOG", vars);
        assert_eq!(
            actions,
            vec![
                Action::Send("PRIVMSG #c :g sclick send hi bob".into()),
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
                text: "a=~bob@host.example.com m2=*!*@host.example.com m3=*!*bob@*.example.com c=1 n=bob!~bob@host.example.com".into(),
            }]
        );
    }

    #[test]
    fn ial_rich_properties_marks_and_enabled_state() {
        use crate::irc::state::{IalView, StateSnapshot};
        let snap = StateSnapshot {
            ial_enabled: false,
            ial: vec![("bob".into(), "Bob!user@host.example".into())],
            ial_info: vec![IalView {
                nick: "Bob".into(),
                address: "Bob!user@host.example".into(),
                account: "bob-account".into(),
                away: Some(true),
                gecos: "Bob Example".into(),
                id: "42".into(),
                marks: vec![
                    ("default".into(), "trusted".into()),
                    ("note".into(), "friend".into()),
                ],
            }],
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
        engine.load("alias t { echo enabled=$ial account=$ial(Bob).account away=$ial(Bob).away mark=$ial(Bob).mark count=$ialmark(Bob,0) name=$ialmark(Bob,2).name note=$ialmark(Bob,note) }");
        assert_eq!(
            engine.run_alias(&rctx, "#c", "t", ""),
            vec![Action::Echo {
                target: "#c".into(),
                text: "enabled=$false account=bob-account away=$true mark=trusted count=2 name=note note=friend".into(),
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
        let mut isupport = crate::irc::state::Isupport::default();
        isupport.parse_token("PREFIX=(qov).@+");
        let snap = StateSnapshot {
            nick: "me".into(),
            isupport,
            channels: vec![ChannelView {
                name: "#a".into(),
                nicks: vec!["owner".into(), "op".into(), "voiced".into(), "plain".into()],
                members: vec![
                    ("owner".into(), ".".into()),
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
              if (owner isowner #a) { /echo owner-is-owner }
              if (owner !isowner #a) { /echo should-not-fire-owner }
              if (!plain isop #a) { /echo plain-not-op }
              if (plain !isop #a) { /echo plain-infix-not-op }
              if (op !isop #a) { /echo should-not-fire-negated }
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
            vec![
                "op-is-op",
                "owner-is-owner",
                "plain-not-op",
                "plain-infix-not-op",
                "voiced-ok",
                "plain-reg",
                "ison-ok",
                "ischan-ok",
                "bitand"
            ]
        );
    }

    #[test]
    fn state_identifiers_use_casemapping_statusmsg_and_rich_member_data() {
        use crate::irc::state::{ChannelView, IalView, StateSnapshot};
        let mut isupport = crate::irc::state::Isupport::default();
        isupport.parse_token("PREFIX=(qov).@+");
        isupport.parse_token("CHANTYPES=#");
        isupport.parse_token("STATUSMSG=@+");
        let last_activity = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(12);
        let snap = StateSnapshot {
            nick: "Me".into(),
            isupport,
            channels: vec![ChannelView {
                name: "#Room^".into(),
                topic: "Welcome".into(),
                mode: "+kln secret 25".into(),
                key: "secret".into(),
                limit: "25".into(),
                nicks: vec!["User^".into(), "Plain".into()],
                members: vec![("User^".into(), "@".into()), ("Plain".into(), "".into())],
                member_activity: vec![("User^".into(), last_activity)],
                bans: vec![],
                ..Default::default()
            }],
            ial: vec![("user~".into(), "User^!ident@host.test".into())],
            ial_info: vec![IalView {
                nick: "user~".into(),
                address: "User^!ident@host.test".into(),
                account: "account-name".into(),
                away: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            my_nick: "Me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(snap),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "alias inspect {
               echo chan=$chan(@#ROOM~) topic=$chan(@#ROOM~).topic mode=$chan(@#ROOM~).mode key=$chan(@#ROOM~).key limit=$chan(@#ROOM~).limit status=$chan(@#ROOM~).status
               echo nick=$nick(@#ROOM~,USER~) pnick=$nick(@#ROOM~,USER~).pnick prefix=$nick(@#ROOM~,USER~).prefix nmode=$nick(@#ROOM~,USER~).mode away=$nick(@#ROOM~,USER~).away account=$nick(@#ROOM~,USER~).account ops=$nick(@#ROOM~,0,O) regs=$nick(@#ROOM~,0,r)
               echo addr=$address(USER~) ial=$ial(USER~) ialchan=$ialchan(USER~,@#ROOM~,1).pnick
               if (USER~ isop @#ROOM~) echo state-op
               if ($nick(@#ROOM~,USER~).idle isnum 10-20) echo idle-ok
             }
             on *:TEXT:*:@#ROOM~:{ echo target-match }",
        );
        assert_eq!(
            engine.run_alias(&rctx, "#Room^", "inspect", ""),
            vec![
                Action::Echo {
                    target: "#Room^".into(),
                    text: "chan=#Room^ topic=Welcome mode=+kln secret 25 key=secret limit=25 status=joined".into(),
                },
                Action::Echo {
                    target: "#Room^".into(),
                    text: "nick=User^ pnick=@User^ prefix=@ nmode=o away=$true account=account-name ops=1 regs=1".into(),
                },
                Action::Echo {
                    target: "#Room^".into(),
                    text: "addr=ident@host.test ial=User^!ident@host.test ialchan=@User^".into(),
                },
                Action::Echo {
                    target: "#Room^".into(),
                    text: "state-op".into(),
                },
                Action::Echo {
                    target: "#Room^".into(),
                    text: "idle-ok".into(),
                },
            ]
        );

        let event = EventVars {
            nick: "User^".into(),
            chan: "#room~".into(),
            target: "#room~".into(),
            text: "hello".into(),
            params: vec!["hello".into()],
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&rctx, "TEXT", event),
            vec![Action::Echo {
                target: "#room~".into(),
                text: "target-match".into(),
            }]
        );
    }

    #[test]
    fn updatenl_switches_departure_handlers_from_old_to_updated_state() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let updated = StateSnapshot {
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["me".into()],
                members: vec![("me".into(), String::new())],
                ..Default::default()
            }],
            ..Default::default()
        };
        let old = StateSnapshot {
            channels: vec![ChannelView {
                name: "#c".into(),
                nicks: vec!["me".into(), "Bob".into()],
                members: vec![("me".into(), String::new()), ("Bob".into(), "@".into())],
                ..Default::default()
            }],
            ial: vec![("bob".into(), "Bob!u@host".into())],
            ..Default::default()
        }
        .with_pending_nicklist_update(updated);
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(old),
        };
        let engine = ScriptEngine::new();
        engine.load(
            "on *:PART:#:{
               echo before=$nick(#c,0):$ial(Bob)
               updatenl
               echo after=$nick(#c,0):$ial(Bob)
             }",
        );
        let event = EventVars {
            nick: "Bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&rctx, "PART", event),
            vec![
                Action::Echo {
                    target: "#c".into(),
                    text: "before=2:Bob!u@host".into(),
                },
                Action::Echo {
                    target: "#c".into(),
                    text: "after=1:".into(),
                },
            ]
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

    #[test]
    fn hash_commands_join_dynamic_item_before_splitting_arguments() {
        let engine = ScriptEngine::new();
        engine.load(
            r"alias t {
                var %suffix = %#The\bLobby
                hadd -m h state. $+ %suffix ready
                hinc h count. $+ %suffix 2
                echo before=$hget(h,state. $+ %suffix) count=$hget(h,count. $+ %suffix)
                hdel h state. $+ %suffix
                echo after=$hget(h,state. $+ %suffix)
            }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![
                Action::Echo {
                    target: "#c".into(),
                    text: r"before=ready count=2".into(),
                },
                Action::Echo {
                    target: "#c".into(),
                    text: "after=".into()
                },
            ]
        );
    }

    #[derive(Default)]
    struct FakeSockets {
        listened: std::sync::Mutex<Vec<(String, u16)>>,
        listen_options: std::sync::Mutex<Vec<(bool, bool)>>,
        accepted: std::sync::Mutex<Vec<String>>,
        accept_options: std::sync::Mutex<Vec<bool>>,
        marks: std::sync::Mutex<HashMap<String, String>>,
        ports: std::sync::Mutex<HashMap<String, u16>>,
        read_options: std::sync::Mutex<Vec<crate::script::eval::SocketReadOptions>>,
        read_result: std::sync::Mutex<Option<crate::script::eval::SocketReadResult>>,
        read_error: std::sync::Mutex<Option<i32>>,
        write_result: std::sync::Mutex<Option<crate::script::eval::SocketWriteResult>>,
        writes: std::sync::Mutex<Vec<Vec<u8>>>,
        send_queued: std::sync::Mutex<usize>,
        starttls: std::sync::Mutex<Vec<String>>,
    }
    impl ScriptSockets for FakeSockets {
        fn reserve_open(
            &self,
            name: &str,
            _host: &str,
            port: u16,
            _tls: bool,
            _bind_ip: &str,
        ) -> Option<Result<u64, i32>> {
            self.ports.lock().unwrap().insert(name.into(), port);
            Some(Ok(77))
        }
        fn reserve_udp(
            &self,
            name: &str,
            _bind_ip: &str,
            local_port: u16,
            _dest_ip: &str,
            _dest_port: u16,
        ) -> Option<Result<u64, i32>> {
            let mut ports = self.ports.lock().unwrap();
            if ports.contains_key(name) {
                Some(Ok(0))
            } else {
                ports.insert(name.into(), local_port);
                Some(Ok(78))
            }
        }
        fn listen(
            &self,
            _bind_ip: &str,
            name: &str,
            port: u16,
            _nodelay: bool,
            _dual_stack: bool,
        ) -> Option<Result<u16, i32>> {
            let p = if port == 0 { 54321 } else { port };
            self.ports.lock().unwrap().insert(name.into(), p);
            self.listened.lock().unwrap().push((name.into(), port));
            self.listen_options
                .lock()
                .unwrap()
                .push((_nodelay, _dual_stack));
            Some(Ok(p))
        }
        fn accept(&self, name: &str, _listener: &str, _nodelay: bool) -> Option<i32> {
            self.accepted.lock().unwrap().push(name.into());
            self.accept_options.lock().unwrap().push(_nodelay);
            Some(0)
        }
        fn close(&self, _pattern: &str) -> Option<i32> {
            Some(0)
        }
        fn set_mark(&self, name: &str, mark: &str) -> Option<i32> {
            self.marks.lock().unwrap().insert(name.into(), mark.into());
            Some(0)
        }
        fn rename(&self, _: &str, _: &str) -> Option<i32> {
            Some(0)
        }
        fn pause(&self, _: &str, _: bool) -> Option<i32> {
            Some(0)
        }
        fn write(&self, name: &str, data: &[u8]) -> Option<crate::script::eval::SocketWriteResult> {
            let result = self.write_result.lock().unwrap().clone();
            if result.as_ref().is_some_and(|result| result.error == 0) {
                // A successful synchronous write implies the fake socket exists,
                // matching the production manager's `$sock(name)` enumeration.
                self.ports
                    .lock()
                    .unwrap()
                    .entry(name.to_string())
                    .or_insert(0);
                *self.send_queued.lock().unwrap() += data.len();
                self.writes.lock().unwrap().push(data.to_vec());
            }
            result
        }
        fn starttls(&self, name: &str) -> Option<i32> {
            self.starttls.lock().unwrap().push(name.to_string());
            Some(0)
        }
        fn read(
            &self,
            _: &str,
            options: crate::script::eval::SocketReadOptions,
        ) -> Option<Result<crate::script::eval::SocketReadResult, i32>> {
            self.read_options.lock().unwrap().push(options);
            if let Some(error) = self.read_error.lock().unwrap().take() {
                Some(Err(error))
            } else {
                self.read_result.lock().unwrap().take().map(Ok)
            }
        }
        fn exists(&self, name: &str) -> bool {
            self.ports.lock().unwrap().contains_key(name)
                || self.marks.lock().unwrap().contains_key(name)
        }
        fn matching_names(&self, pattern: &str) -> Vec<String> {
            let mut names: Vec<String> = self.ports.lock().unwrap().keys().cloned().collect();
            names.extend(self.marks.lock().unwrap().keys().cloned());
            names.sort();
            names.dedup();
            names.retain(|name| wildcard_match(pattern, name));
            names
        }
        fn prop(&self, name: &str, property: &str) -> String {
            match property {
                "port" => self
                    .ports
                    .lock()
                    .unwrap()
                    .get(name)
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
                "mark" => self
                    .marks
                    .lock()
                    .unwrap()
                    .get(name)
                    .cloned()
                    .unwrap_or_default(),
                "sq" => self.send_queued.lock().unwrap().to_string(),
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
        assert_eq!(
            echoed,
            vec!["port=54321 mark=hi there st=listening ex=relay no="]
        );
        // /socklisten binds (recorded) and queues the accept-loop start.
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::SockListen { name, .. } if name == "relay")));
        assert_eq!(
            *fake.listened.lock().unwrap(),
            vec![("relay".to_string(), 0u16)]
        );
        assert_eq!(*fake.accepted.lock().unwrap(), vec!["conn".to_string()]);
    }

    #[test]
    fn socket_nodelay_and_dual_stack_switches_reach_the_backend() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake.clone());
        engine.load(
            "alias t { var %listen_opts = -nu | var %accept_opts = -n | socklisten %listen_opts relay | sockaccept %accept_opts conn }",
        );

        let actions = engine.run_alias(&ctx(), "", "t", "");
        assert!(actions
            .iter()
            .any(|action| matches!(action, Action::SockListen { name, .. } if name == "relay")));
        assert_eq!(*fake.listen_options.lock().unwrap(), vec![(true, true)]);
        assert_eq!(*fake.accept_options.lock().unwrap(), vec![true]);
    }

    #[test]
    fn sockopen_is_visible_to_state_commands_in_the_same_handler() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake);
        engine.load(
            "alias t { sockopen pending example.test 80 | sockmark pending hello | /echo -a ex=$sock(pending) mark=$sock(pending).mark }",
        );

        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![
                Action::SockOpen {
                    name: "pending".into(),
                    host: "example.test".into(),
                    port: 80,
                    tls: false,
                    accept_invalid: false,
                    bind_ip: String::new(),
                    nodelay: false,
                    ip_version: 0,
                    reservation_id: 77,
                },
                Action::Echo {
                    target: "(status)".into(),
                    text: "ex=pending mark=hello".into(),
                },
            ]
        );
    }

    #[test]
    fn sockopen_tls_invalid_certificate_switches_are_preserved() {
        let engine = ScriptEngine::new();
        engine.load("alias t { sockopen -es secure example.test 6697 }");

        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![Action::SockOpen {
                name: "secure".into(),
                host: "example.test".into(),
                port: 6697,
                tls: true,
                accept_invalid: true,
                bind_ip: String::new(),
                nodelay: false,
                ip_version: 0,
                reservation_id: 0,
            }]
        );
    }

    #[test]
    fn sockopen_dash_t_upgrades_the_existing_socket_without_reopening_it() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake.clone());
        engine.load("alias t { sockopen -t mail }");

        assert!(engine.run_alias(&ctx(), "", "t", "").is_empty());
        assert_eq!(*fake.starttls.lock().unwrap(), vec!["mail".to_string()]);
    }

    #[test]
    fn sock_wildcard_count_and_nth_name() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake);
        engine.load(
            "alias t { socklisten i7.zeta | socklisten b7.other | socklisten i7.alpha | /echo count=$sock(I7.*,0) default=$sock(i7.*) first=$sock(i7.*,1) second=$sock(i7.*,2) missing=[$sock(i7.*,3)] status=$sock(i7.*).status }",
        );
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        let echoed: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Echo { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            echoed,
            vec!["count=2 default=i7.alpha first=i7.alpha second=i7.zeta missing=[] status=listening"]
        );
    }

    #[test]
    fn socket_commands_validate_switches_and_reset_sockerr_on_success() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake);
        engine.load(
            "alias t { sockwrite -z relay nope | /echo -a first=$sockerr | sockmark relay ok | /echo -a second=$sockerr }",
        );

        assert_eq!(
            engine.run_alias(&ctx(), "", "t", ""),
            vec![
                Action::Echo {
                    target: "(status)".into(),
                    text: "first=10022".into(),
                },
                Action::Echo {
                    target: "(status)".into(),
                    text: "second=0".into(),
                },
            ]
        );
    }

    #[test]
    fn sockudp_expands_switch_variables_and_preserves_dual_stack() {
        let engine = ScriptEngine::new();
        engine.load("alias send { var %opts = -uk | sockudp %opts packet 127.0.0.1 9000 hi }");

        assert_eq!(
            engine.run_alias(&ctx(), "", "send", ""),
            vec![Action::SockUdp {
                name: "packet".into(),
                bind_ip: String::new(),
                local_port: 0,
                dest_ip: "127.0.0.1".into(),
                dest_port: 9000,
                data: b"hi".to_vec(),
                keep: true,
                dual_stack: true,
                reservation_id: 0,
            }]
        );
    }

    #[test]
    fn sockwrite_treats_an_unset_binvar_as_empty_binary_data() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        *fake.write_result.lock().unwrap() = Some(crate::script::eval::SocketWriteResult {
            error: 0,
            failures: Vec::new(),
        });
        engine.set_sockets(fake.clone());
        engine.load("alias send { sockwrite relay &missing }");

        assert!(engine.run_alias(&ctx(), "", "send", "").is_empty());
        assert_eq!(*fake.writes.lock().unwrap(), vec![Vec::<u8>::new()]);
    }

    #[test]
    fn sockwrite_binary_count_expands_variables() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        *fake.write_result.lock().unwrap() = Some(crate::script::eval::SocketWriteResult {
            error: 0,
            failures: Vec::new(),
        });
        engine.set_sockets(fake);
        engine.load(
            "alias send { var %opts = -b | var %bytes = 3 | sockwrite %opts relay %bytes abcdef | /echo -a sq=$sock(relay).sq err=$sockerr }",
        );

        assert_eq!(
            engine.run_alias(&ctx(), "", "send", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "sq=3 err=0".into(),
            }]
        );
    }

    #[test]
    fn sockwrite_updates_sq_and_sockerr_before_the_next_script_line() {
        const MISSING_SOCKET: i32 = 10_038;
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        *fake.write_result.lock().unwrap() = Some(crate::script::eval::SocketWriteResult {
            error: 0,
            failures: Vec::new(),
        });
        engine.set_sockets(fake.clone());
        engine
            .load("alias send { sockwrite relay abc | /echo -a sq=$sock(relay).sq err=$sockerr }");
        assert_eq!(
            engine.run_alias(&ctx(), "", "send", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "sq=3 err=0".into(),
            }]
        );

        *fake.write_result.lock().unwrap() = Some(crate::script::eval::SocketWriteResult {
            error: MISSING_SOCKET,
            failures: Vec::new(),
        });
        assert_eq!(
            engine.run_alias(&ctx(), "", "send", ""),
            vec![
                Action::SockError {
                    kind: "SOCKWRITE".into(),
                    name: "relay".into(),
                    error: MISSING_SOCKET,
                },
                Action::Echo {
                    target: "(status)".into(),
                    text: format!("sq=3 err={MISSING_SOCKET}"),
                },
            ]
        );
    }

    #[test]
    fn sockwrite_failure_queues_one_event_per_concrete_socket() {
        const MISSING_SOCKET: i32 = 10_038;
        const CONNECTION_RESET: i32 = 10_054;
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        *fake.write_result.lock().unwrap() = Some(crate::script::eval::SocketWriteResult {
            error: MISSING_SOCKET,
            failures: vec![
                ("relay.one".into(), MISSING_SOCKET),
                ("relay.two".into(), CONNECTION_RESET),
            ],
        });
        engine.set_sockets(fake);
        engine.load("alias send { sockwrite relay.* abc | /echo -a err=$sockerr }");

        assert_eq!(
            engine.run_alias(&ctx(), "", "send", ""),
            vec![
                Action::SockError {
                    kind: "SOCKWRITE".into(),
                    name: "relay.one".into(),
                    error: MISSING_SOCKET,
                },
                Action::SockError {
                    kind: "SOCKWRITE".into(),
                    name: "relay.two".into(),
                    error: CONNECTION_RESET,
                },
                Action::Echo {
                    target: "(status)".into(),
                    text: format!("err={MISSING_SOCKET}"),
                },
            ]
        );
    }

    #[test]
    fn sockread_error_preserves_the_destination_and_resets_sockbr() {
        const CONNECTION_RESET: i32 = 10_054;
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        *fake.read_error.lock().unwrap() = Some(CONNECTION_RESET);
        engine.set_sockets(fake);
        engine.load(
            "alias read { set %dest keep | sockread %dest | /echo -a dest=%dest br=$sockbr err=$sockerr }",
        );

        assert_eq!(
            engine.run_alias(&ctx(), "", "read", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: format!("dest=keep br=0 err={CONNECTION_RESET}"),
            }]
        );
    }

    #[test]
    fn sockread_expands_binary_byte_count_but_not_destination() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        *fake.read_result.lock().unwrap() = Some(crate::script::eval::SocketReadResult {
            data: vec![1, 2, 3],
            bytes_read: 3,
        });
        engine.set_sockets(fake.clone());
        engine.load(
            "on *:SOCKREAD:bot:{ sockread %bytes &raw | /msg #c len $bvar(&raw,0) br $sockbr }",
        );
        engine.run_command(&ctx(), "", "/set %bytes 3", &[]);
        let vars = EventVars {
            chan: "bot".into(),
            target: "bot".into(),
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "SOCKREAD", vars),
            vec![Action::Send("PRIVMSG #c :len 3 br 3".into())]
        );
        assert_eq!(
            *fake.read_options.lock().unwrap(),
            vec![crate::script::eval::SocketReadOptions {
                binary: true,
                force: false,
                line: false,
                max_bytes: 3,
            }]
        );
    }

    #[test]
    fn i7_names_relay_preserves_full_active_353_and_followup_chunk() {
        let engine = ScriptEngine::new();
        let fake = std::sync::Arc::new(FakeSockets::default());
        engine.set_sockets(fake);
        engine.load(
            r#"
            alias -l i7n_chan {
              if ($left($1,3) == i7.) return $mid($1,4)
              if ($left($1,2) == $+($chr(37),$chr(35))) return $1
            }
            alias -l i7n_trail {
              var %p = $pos($1-,$+($chr(32),:),1)
              if (%p) return $mid($1-,$calc(%p + 2))
            }
            alias -l i7n_nameitem {
              var %item = $1
              if ($left(%item,1) == :) var %item = $mid(%item,2)
              if ($chr(44) isin %item) return $gettok(%item,-1,44)
              return %item
            }
            alias -l i7n_name {
              var %n = $i7n_nameitem($1)
              while ($istok(. @ +,$left(%n,1),32)) var %n = $mid(%n,2)
              return %n
            }
            on *:SOCKREAD:i7.*:{
              bunset &line
              sockread -n &line
              if ($sockbr == 0) return
              var %line = $bvar(&line).text
              var %pfx = $iif($left(%line,1) == :,2,1), %cmd = $upper($gettok(%line,%pfx,32))
              if (%cmd == 353) {
                var %chan = $i7n_chan($sockname), %names = $i7n_trail(%line), %i = 1
                var %out
                while (%i <= $numtok(%names,32)) {
                  var %item = $i7n_nameitem($gettok(%names,%i,32)), %name = $i7n_name(%item), %flag = $left(%item,1)
                  if (%item != $null) var %out = $iif(%out,%out %item,%item)
                  inc %i
                }
                if ($sock(mIRC.local)) sockwrite -n mIRC.local $gettok(%line,1-5,32) : $+ %out
              }
            }
            "#,
        );
        engine.run_command(&ctx(), "", "/sockmark mIRC.local live", &[]);

        let line = r":TK2CHATCHATA01 353 >guest = %#The\bLobby :+Sky +xpulse .Admin_Sky Sockbot4820 Skyxo @>QuirkyOtter88 .Sysop_Liam >User9711-rs Snue >HappyWombat61 SnueJr @>guest";
        let fire = |line: &str| {
            let vars = EventVars {
                chan: r"i7.%#The\bLobby".into(),
                target: r"i7.%#The\bLobby".into(),
                params: line.split_whitespace().map(String::from).collect(),
                text: line.into(),
                sock_bytes: line.as_bytes().to_vec(),
                ..Default::default()
            };
            engine.dispatch_event(&ctx(), "SOCKREAD", vars)
        };
        assert_eq!(
            fire(line),
            vec![Action::SockWrite {
                name: "mIRC.local".into(),
                data: format!("{line}\r\n").into_bytes(),
            }]
        );

        let chunk = r":TK2CHATCHATA01 353 >guest = %#The\bLobby :1,x,+Alpha 2,x,Beta 3,x,.Owner 4,x,@Host 5,x,Regular";
        let expected =
            r":TK2CHATCHATA01 353 >guest = %#The\bLobby :+Alpha Beta .Owner @Host Regular";
        assert_eq!(
            fire(chunk),
            vec![Action::SockWrite {
                name: "mIRC.local".into(),
                data: format!("{expected}\r\n").into_bytes(),
            }]
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
        engine2.run_command(&rctx, "#c", "/hload -m seen seen.txt", &[]);
        engine2.load("alias r { /msg #c $hget(seen, bob) and $hfind(seen, a*, 1, w) }");
        let actions = engine2.run_alias(&rctx, "#c", "r", "");
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #c :20 and alice".into())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_binary_roundtrip_slots_case_and_find_modes() {
        let dir = std::env::temp_dir().join(format!("jirc-hbin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rctx = RunCtx {
            my_nick: "me",
            network: "Net",
            server: "s",
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
        };
        let writer = ScriptEngine::new();
        writer.load(
            "alias save { hmake Binary 17 | bset &source 1 65 0 66 13 10 255 | hadd -b binary Payload &source | hsave -b BINARY values.dat }",
        );
        writer.run_alias(&rctx, "#c", "save", "");

        let reader = ScriptEngine::new();
        reader.load(
            "alias remember { hadd -m found $1 yes | if ($1 == Beta) halt }
             alias load { hload -bm17 binary values.dat | var %n = $hget(BINARY,pAyLoAd,&out) | hadd binary Alpha one | hadd binary Beta two | var %matches = $hfind(binary,*,0,w,remember $1-) | msg #c $hget(binary).size $+ / $+ %n $+ / $+ $bvar(&out,1,6) $+ / $+ $hfind(binary,a*,1,w) $+ / $+ $hfind(binary,two,1,n).data $+ / $+ %matches $+ / $+ $hget(found,alpha) $+ / $+ $hget(found,payload) }",
        );
        assert_eq!(
            reader.run_alias(&rctx, "#c", "load", ""),
            vec![Action::Send(
                "PRIVMSG #c :17/6/65 0 66 13 10 255/Alpha/Beta/2/yes/".into()
            )]
        );
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
        engine.load(
            "alias t { hinc c hits 5 | hinc c hits | hdec c hits 2 | /msg #c $hget(c,hits) }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :4".into())]
        );
    }

    #[test]
    fn routine_local_vars_shadow_nested_aliases_and_do_not_leak() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias inner { var %value = inner | /msg #c inner=%value local=$var(%value,1).local }\n\
             alias outer { set %value global | var %value = outer | /msg #c before=%value local=$var(%value,1).local | inner | /msg #c after=%value }\n\
             alias leak { var %only = temporary | inc %only | /msg #c %only }\n\
             alias unset_local { set %unset.global global | var %unset.global = local | var %unset.a = a | var %unset.b = b | unset %unset.global | unset %unset.* | /msg #c exact=%unset.global count=$var(%unset.*,0) }\n\
             alias clear_local { set %clear.global global | var %clear.local = local | unsetall | /msg #c globals=[ $+ %clear.global $+ ] locals=$var(%clear.local,0) }",
        );

        assert_eq!(
            engine.run_alias(&ctx(), "#c", "outer", ""),
            vec![
                Action::Send("PRIVMSG #c :before=outer local=$true".into()),
                Action::Send("PRIVMSG #c :inner=inner local=$true".into()),
                Action::Send("PRIVMSG #c :after=outer".into()),
            ]
        );
        // The nested frame restored the caller's local, and the outer frame was
        // then discarded to reveal the persistent `/set` value underneath it.
        assert_eq!(
            engine.run_command(
                &ctx(),
                "#c",
                "/echo -a value=%value local=$var(%value,1).local",
                &[],
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "value=global local=$false".into(),
            }]
        );

        assert_eq!(
            engine.run_alias(&ctx(), "#c", "leak", ""),
            vec![Action::Send("PRIVMSG #c :1".into())]
        );
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/echo -a only=[ $+ %only $+ ]", &[]),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "only=[]".into()
            }]
        );
        assert!(!engine.inner.lock().unwrap().vars.contains_key("only"));

        // Ordinary /unset removes the nearest local declaration first. A
        // wildcard likewise clears the nearest matching frame without deleting
        // the global value that becomes visible underneath it.
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "unset_local", ""),
            vec![Action::Send("PRIVMSG #c :exact=global count=1".into())]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "clear_local", ""),
            vec![Action::Send("PRIVMSG #c :globals=[] locals=0".into())]
        );
        let g = engine.inner.lock().unwrap();
        assert!(!g.vars.contains_key("clear.global"));
        assert!(!g.var_expiry.contains_key("clear.global"));
    }

    #[test]
    fn timed_variables_expire_and_overwrites_manage_lifetimes() {
        let engine = ScriptEngine::new();

        // /inc and /dec can replace/preserve the lifetime of a global value.
        engine.run_command(&ctx(), "#c", "/set -u30 %temp one", &[]);
        let original = {
            let g = engine.inner.lock().unwrap();
            assert_eq!(g.vars.get("temp").map(String::as_str), Some("one"));
            *g.var_expiry.get("temp").expect("/set -uN lifetime")
        };
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/echo -a $var(%temp,1).secs", &[]),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "30".into()
            }]
        );

        engine.run_command(&ctx(), "#c", "/inc -k %temp 1", &[]);
        {
            let g = engine.inner.lock().unwrap();
            assert_eq!(g.var_expiry.get("temp"), Some(&original));
        }
        // Setting the variable again without -k or -uN cancels its old timer.
        engine.run_command(&ctx(), "#c", "/set %temp replacement", &[]);
        assert!(!engine.inner.lock().unwrap().var_expiry.contains_key("temp"));

        engine.run_command(&ctx(), "#c", "/set -u30 %temp expired", &[]);
        engine.run_command(&ctx(), "#c", "/set -u30 %wild.a a", &[]);
        engine.run_command(&ctx(), "#c", "/set -u30 %wild.b b", &[]);
        engine.run_command(&ctx(), "#c", "/unset %wild.*", &[]);
        {
            let g = engine.inner.lock().unwrap();
            assert!(!g.var_expiry.keys().any(|name| name.starts_with("wild.")));
            assert!(!g.vars.keys().any(|name| name.starts_with("wild.")));
        }

        // Expiry is lazy: the next execution/identifier boundary removes an
        // elapsed value. Exercise a real one-second monotonic lifetime.
        engine.run_command(&ctx(), "#c", "/set -u1 %temp expired", &[]);
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/echo -a [ $+ %temp $+ ]", &[]),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "[]".into()
            }]
        );
        assert!(!engine.inner.lock().unwrap().vars.contains_key("temp"));

        // Other assignment commands follow the same overwrite rule. A socket
        // read must not leave an older timer attached to the newly-read value.
        engine.run_command(&ctx(), "#c", "/set -u30 %line old", &[]);
        engine.load("on *:SOCKREAD:reader:{ sockread %line }");
        engine.dispatch_event(
            &ctx(),
            "SOCKREAD",
            EventVars {
                chan: "reader".into(),
                target: "reader".into(),
                text: "fresh".into(),
                sock_bytes: b"fresh".to_vec(),
                ..Default::default()
            },
        );
        {
            let g = engine.inner.lock().unwrap();
            assert_eq!(g.vars.get("line").map(String::as_str), Some("fresh"));
            assert!(!g.var_expiry.contains_key("line"));
        }

        // -u0 remains visible inside this invocation, then disappears as the
        // outer alias finishes.
        engine.load("alias zero { set -u0 %once visible | /msg #c %once }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "zero", ""),
            vec![Action::Send("PRIVMSG #c :visible".into())]
        );
        assert!(!engine.inner.lock().unwrap().vars.contains_key("once"));
    }

    #[test]
    fn timed_hash_items_expire_and_deletes_clear_metadata() {
        let engine = ScriptEngine::new();
        engine.run_command(&ctx(), "#c", "/hadd -mu30 cache item 4", &[]);
        let original = {
            let g = engine.inner.lock().unwrap();
            *g.hash_expiry
                .get(&("cache".into(), "item".into()))
                .expect("/hadd -uN lifetime")
        };
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/echo -a $hget(cache,item).unset", &[]),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "30".into()
            }]
        );

        engine.run_command(&ctx(), "#c", "/hinc -k cache item 2", &[]);
        {
            let g = engine.inner.lock().unwrap();
            assert_eq!(
                g.hash_expiry.get(&("cache".into(), "item".into())),
                Some(&original)
            );
            assert_eq!(g.hashes["cache"]["item"], "6");
        }
        // An ordinary overwrite cancels the existing lifetime.
        engine.run_command(&ctx(), "#c", "/hadd cache item permanent", &[]);
        assert!(!engine
            .inner
            .lock()
            .unwrap()
            .hash_expiry
            .contains_key(&("cache".into(), "item".into())));

        engine.run_command(&ctx(), "#c", "/hadd -u30 cache wild.a a", &[]);
        engine.run_command(&ctx(), "#c", "/hadd -u30 cache wild.b b", &[]);
        engine.run_command(&ctx(), "#c", "/hdel -w cache wild.*", &[]);
        {
            let g = engine.inner.lock().unwrap();
            assert!(!g
                .hash_expiry
                .keys()
                .any(|(table, item)| table == "cache" && item.starts_with("wild.")));
        }

        engine.run_command(&ctx(), "#c", "/hinc -u1 cache count 1", &[]);
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        assert_eq!(
            engine.run_command(&ctx(), "#c", "/echo -a [ $+ $hget(cache,count) $+ ]", &[]),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "[]".into()
            }]
        );

        engine.load("alias zero { hadd -u0 cache once value | /msg #c $hget(cache,once) }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "zero", ""),
            vec![Action::Send("PRIVMSG #c :value".into())]
        );
        assert!(!engine.inner.lock().unwrap().hashes["cache"].contains_key("once"));

        // Table-level deletion must not leave orphaned expiry records.
        engine.run_command(&ctx(), "#c", "/hadd -u30 cache other value", &[]);
        engine.run_command(&ctx(), "#c", "/hfree -w ca*", &[]);
        assert!(!engine
            .inner
            .lock()
            .unwrap()
            .hash_expiry
            .keys()
            .any(|(table, _)| table == "cache"));
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
    fn listing_sentinels_and_media_identifiers_match_mirc() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        // #busy is mid-listing on both; #idle is not.
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![
                ChannelView {
                    name: "#busy".into(),
                    in_mode: true,
                    in_who: true,
                    ..Default::default()
                },
                ChannelView {
                    name: "#idle".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let run = |script: &str| {
            let e = ScriptEngine::new();
            e.load(&format!("alias t {{ echo -a {script} }}"));
            match e.run_alias(&rctx, "#busy", "t", "").as_slice() {
                [Action::Echo { text, .. }] => text.clone(),
                other => panic!("unexpected: {other:?}"),
            }
        };
        // mIRC's idiom is comparing the property against the sentinel.
        assert_eq!(run("$chan(#busy).banlist"), run("$inmode"));
        assert_eq!(run("$chan(#busy).inwho"), run("$inwho"));
        assert_eq!(run("$chan(#idle).banlist"), "$false");
        assert_eq!(run("$chan(#idle).inwho"), "$false");
        assert_ne!(run("$inmode"), run("$inwho"));
        // Sound directories all resolve to the same folder as $mididir.
        assert_eq!(run("$wavedir"), run("$mididir"));
        assert_eq!(run("$mp3dir"), run("$mididir"));
        // Per-format playback tests stay inactive, like $insong/$inwave.
        assert_eq!(run("$inmp3"), "$false");
        assert_eq!(run("$inmp3"), run("$insong"));
        // $fserv is the mIRC spelling of the existing $fserve list.
        assert_eq!(run("$fserv(0)"), run("$fserve(0)"));
    }

    #[test]
    fn fsend_and_fupdate_round_trip_their_settings() {
        let engine = ScriptEngine::new();
        let show = |e: &ScriptEngine, cmd: &str| match e
            .run_command(&ctx(), "#a", cmd, &[])
            .as_slice()
        {
            [Action::Echo { text, .. }] => text.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        // Bare form reports the current value.
        assert_eq!(show(&engine, "/fsend"), "* Fast send is on");
        assert_eq!(show(&engine, "/fupdate"), "* Update delay is 0");
        // Setting a value is silent and sticks.
        assert!(engine.run_command(&ctx(), "#a", "/fsend off", &[]).is_empty());
        assert_eq!(show(&engine, "/fsend"), "* Fast send is off");
        assert!(engine.run_command(&ctx(), "#a", "/fupdate 40", &[]).is_empty());
        assert_eq!(show(&engine, "/fupdate"), "* Update delay is 40");
        // $fupdate reads the same setting back, clamped to mIRC's 0-100.
        engine.load("alias t { echo -a $fupdate }");
        assert!(matches!(
            engine.run_alias(&ctx(), "#a", "t", "").as_slice(),
            [Action::Echo { text, .. }] if text == "40"
        ));
        engine.run_command(&ctx(), "#a", "/fupdate 500", &[]);
        assert_eq!(show(&engine, "/fupdate"), "* Update delay is 100");
    }

    #[test]
    fn nonstdmsg_flags_unusual_privmsg_targets() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![ChannelView {
                name: "#a".into(),
                nicks: vec!["me".into(), "bob".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let engine = ScriptEngine::new();
        engine.load("on *:TEXT:*:*:/echo -a nonstd=$nonstdmsg");
        let fire = |target: &str| {
            let event = UiEvent::Message {
                server_id: "s".into(),
                kind: MessageKind::Privmsg,
                from: Some("bob".into()),
                target: target.into(),
                text: "hi".into(),
                time: None,
            };
            match drive_event(&engine, &rctx, &event).as_slice() {
                [Action::Echo { text, .. }] => text.clone(),
                other => panic!("unexpected for {target}: {other:?}"),
            }
        };
        // A channel we are on, and a message to us, are both standard.
        assert_eq!(fire("#a"), "nonstd=$false");
        assert_eq!(fire("me"), "nonstd=$false");
        // A target that is neither is the combination mIRC flags.
        assert_eq!(fire("someoneelse"), "nonstd=$true");
    }

    #[test]
    fn nick_variants_and_list_identifiers_read_live_state() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        // op / halfop / voiced / plain, so each filter picks a different set.
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![ChannelView {
                name: "#a".into(),
                nicks: vec!["op".into(), "hop".into(), "vox".into(), "plain".into()],
                members: vec![
                    ("op".into(), "@".into()),
                    ("hop".into(), "%".into()),
                    ("vox".into(), "+".into()),
                    ("plain".into(), String::new()),
                ],
                bans: vec!["*!*@bad.example".into(), "spam!*@*".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let engine = ScriptEngine::new();
        let run = |script: &str| {
            let e = ScriptEngine::new();
            e.load(&format!("alias t {{ echo -a {script} }}"));
            match e.run_alias(&rctx, "#a", "t", "").as_slice() {
                [Action::Echo { text, .. }] => text.clone(),
                other => panic!("unexpected: {other:?}"),
            }
        };
        let _ = &engine;
        // $rnick / $nvnick — only members with no status prefix at all.
        assert_eq!(run("$nvnick(#a,0)"), "1");
        assert_eq!(run("$nvnick(#a,1)"), "plain");
        assert_eq!(run("$rnick(#a,1)"), "plain");
        // $nopnick — everyone who is not an operator.
        assert_eq!(run("$nopnick(#a,0)"), "3");
        assert_eq!(run("$nopnick(#a,1)"), "hop");
        // $nhnick — everyone who is not a halfop.
        assert_eq!(run("$nhnick(#a,0)"), "3");
        assert_eq!(run("$nhnick(#a,1)"), "op");
        // $banlist(#chan,N); N=0 is the count.
        assert_eq!(run("$banlist(#a,0)"), "2");
        assert_eq!(run("$banlist(#a,1)"), "*!*@bad.example");
        // The quiet list is not tracked, so $iql is consistently empty.
        assert_eq!(run("$iql(#a,0)"), "0");
    }

    #[test]
    fn no_field_events_accept_mircs_documented_braceless_form() {
        // `ON <level>:EVENT:<commands>` has no matchtext and no target. If the
        // parser treats the command as a target it finds an empty command and
        // discards the handler with no error, so cover the whole class.
        for kind in [
            "CONNECT", "DISCONNECT", "DNS", "START", "LOAD", "UNLOAD", "EXIT", "QUIT", "NICK",
            "USERMODE", "PING", "PONG", "NOTIFY", "UNOTIFY", "APPACTIVE", "NOSOUND", "AGENT",
            "WAVEEND", "MIDIEND", "MP3END", "SONGEND", "PLAYEND",
        ] {
            let engine = ScriptEngine::new();
            engine.load(&format!("on *:{kind}:/echo -a fired {kind}"));
            assert_eq!(
                engine.dispatch_event(&ctx(), kind, EventVars::default()),
                vec![Action::Echo {
                    target: "(status)".into(),
                    text: format!("fired {kind}")
                }],
                "braceless `on *:{kind}:<command>` was dropped"
            );
            // The historical jIRC form with a redundant `*` still works too.
            let legacy = ScriptEngine::new();
            legacy.load(&format!("on *:{kind}:*:/echo -a fired {kind}"));
            assert_eq!(
                legacy.dispatch_event(&ctx(), kind, EventVars::default()),
                vec![Action::Echo {
                    target: "(status)".into(),
                    text: format!("fired {kind}")
                }],
                "legacy `on *:{kind}:*:<command>` regressed"
            );
        }
    }

    #[test]
    fn dns_event_exposes_name_and_resolved_address() {
        let engine = ScriptEngine::new();
        engine.load("on *:DNS:/echo -a $naddress $iaddress $raddress");
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "DNS",
                EventVars {
                    dns_query: "example.org".into(),
                    dns_ips: vec!["198.51.100.7".into()],
                    ..Default::default()
                }
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "example.org 198.51.100.7 example.org".into()
            }]
        );
    }

    #[test]
    fn client_side_commands_do_not_leak_to_the_server() {
        use crate::irc::state::{ChannelView, Isupport, StateSnapshot};
        // Two channels, and a server that advertises STATUSMSG=@+.
        let snap = StateSnapshot {
            nick: "me".into(),
            isupport: Isupport {
                status_msg: "@+".into(),
                ..Default::default()
            },
            channels: vec![
                ChannelView {
                    name: "#a".into(),
                    nicks: vec!["me".into(), "bob".into()],
                    members: vec![("me".into(), "@".into()), ("bob".into(), "+".into())],
                    ..Default::default()
                },
                ChannelView {
                    name: "#b".into(),
                    nicks: vec!["me".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let engine = ScriptEngine::new();

        // `/leave` and `/action` are mIRC synonyms, not raw IRC verbs.
        assert_eq!(
            engine.run_command(&rctx, "#a", "/leave #a", &[]),
            vec![Action::Send("PART #a".into())]
        );
        assert_eq!(
            engine.run_command(&rctx, "#a", "/action waves", &[]),
            vec![Action::Send("PRIVMSG #a :\u{1}ACTION waves\u{1}".into())]
        );
        // `/partall` leaves every joined channel.
        assert_eq!(
            engine.run_command(&rctx, "#a", "/partall", &[]),
            vec![
                Action::Send("PART #a".into()),
                Action::Send("PART #b".into())
            ]
        );
        // STATUSMSG is advertised, so these use the prefixed target.
        assert_eq!(
            engine.run_command(&rctx, "#a", "/vmsg hello", &[]),
            vec![Action::Send("PRIVMSG +#a :hello".into())]
        );
        assert_eq!(
            engine.run_command(&rctx, "#a", "/wallchops #a listen up", &[]),
            vec![Action::Send("NOTICE @#a :listen up".into())]
        );
        assert_eq!(
            engine.run_command(&rctx, "#a", "/wallvoices #a hi voices", &[]),
            vec![Action::Send("NOTICE +#a :hi voices".into())]
        );
        // `/exit` and `/disconnect` are client actions, never server lines.
        assert!(matches!(
            engine.run_command(&rctx, "#a", "/exit", &[]).as_slice(),
            [Action::ClientCommand { command, .. }] if command == "exit"
        ));
        assert!(matches!(
            engine.run_command(&rctx, "#a", "/disconnect bye", &[]).as_slice(),
            [Action::ClientCommand { command, args, .. }]
                if command == "disconnect" && args == "bye"
        ));
        // `/closemsg` maps onto the existing close router.
        assert!(matches!(
            engine.run_command(&rctx, "#a", "/closemsg", &[]).as_slice(),
            [Action::ClientCommand { command, args, .. }]
                if command == "close" && args == "-m"
        ));
        // `/colour` normalises to `color` so the frontend knows one name.
        assert!(matches!(
            engine.run_command(&rctx, "#a", "/colour", &[]).as_slice(),
            [Action::ClientCommand { command, .. }] if command == "color"
        ));
        // `/username` is inert but must not reach the server.
        assert!(engine.run_command(&rctx, "#a", "/username bob", &[]).is_empty());
        // Real IRC verbs still pass through untouched.
        assert_eq!(
            engine.run_command(&rctx, "#a", "/whois bob", &[]),
            vec![Action::Send("WHOIS bob".into())]
        );
    }

    #[test]
    fn status_message_commands_fall_back_without_statusmsg() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        // No STATUSMSG token: address the prefixed members individually.
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![ChannelView {
                name: "#a".into(),
                nicks: vec!["me".into(), "opguy".into(), "voiced".into()],
                members: vec![
                    ("me".into(), String::new()),
                    ("opguy".into(), "@".into()),
                    ("voiced".into(), "+".into()),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let rctx = RunCtx {
            state: std::sync::Arc::new(snap),
            ..ctx()
        };
        let engine = ScriptEngine::new();
        assert_eq!(
            engine.run_command(&rctx, "#a", "/wallchops #a ping", &[]),
            vec![Action::Send("NOTICE opguy :ping".into())]
        );
        assert_eq!(
            engine.run_command(&rctx, "#a", "/vmsg hey", &[]),
            vec![Action::Send("PRIVMSG voiced :hey".into())]
        );
    }

    #[test]
    fn ctcps_gates_ctcp_events_like_events_gates_the_rest() {
        let engine = ScriptEngine::new();
        engine.load("on *:CTCP:PING*:*:/echo -a ctcp $1-\non *:TEXT:*:#:/echo -a text $1-");
        let vars = || EventVars {
            chan: "#a".into(),
            target: "#a".into(),
            text: "PING 123".into(),
            params: vec!["PING".into(), "123".into()],
            ..Default::default()
        };
        // On by default.
        assert!(!engine.dispatch_event(&ctx(), "CTCP", vars()).is_empty());
        // `/ctcps off` clears the CTCP bit without disturbing normal events.
        engine.run_command(&ctx(), "#a", "/ctcps off", &[]);
        assert!(engine.dispatch_event(&ctx(), "CTCP", vars()).is_empty());
        assert!(!engine
            .dispatch_event(
                &ctx(),
                "TEXT",
                EventVars {
                    chan: "#a".into(),
                    target: "#a".into(),
                    text: "hi".into(),
                    ..Default::default()
                }
            )
            .is_empty());
        // And back on again.
        engine.run_command(&ctx(), "#a", "/ctcps on", &[]);
        assert!(!engine.dispatch_event(&ctx(), "CTCP", vars()).is_empty());
    }

    #[test]
    fn amsg_and_ban_use_state() {
        use crate::irc::state::{ChannelView, StateSnapshot};
        let snap = StateSnapshot {
            nick: "me".into(),
            channels: vec![
                ChannelView {
                    name: "#a".into(),
                    nicks: vec!["me".into(), "bob".into()],
                    ..Default::default()
                },
                ChannelView {
                    name: "#b".into(),
                    nicks: vec!["me".into()],
                    ..Default::default()
                },
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
        // /ban masks a known nick via the IAL (default type 2 = *!*@host) and sets +b
        assert_eq!(
            engine.run_alias(&rctx, "#a", "b", ""),
            vec![Action::Send("MODE #a +b *!*@host.example.com".into())]
        );
    }

    #[test]
    fn client_commands_do_not_leak_to_server() {
        // /clear etc. are client-side: they must NOT become a raw IRC line.
        // (`/window` is now a real command — covered by custom_window_lines.)
        let engine = ScriptEngine::new();
        engine.load("alias t { clear | beep 1 100 | /msg #c done }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "t", ""),
            vec![
                Action::ClientCommand {
                    command: "beep".into(),
                    args: "1 100".into(),
                    current_target: "#c".into(),
                },
                Action::Send("PRIVMSG #c :done".into()),
            ]
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
    fn haltdef_suppresses_the_default_wherever_it_appears() {
        // /haltdef sets a flag rather than stopping the routine, so it works
        // first or last in the handler. mIRC's own examples put it first;
        // scripts in the wild often put it last. Both must behave the same.
        use crate::irc::event::{MessageKind, UiEvent};
        let event = || UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "#c".into(),
            text: "hello".into(),
            time: None,
        };
        for script in [
            "on ^*:TEXT:*:#:{ haltdef | echo -a themed $1- }",
            "on ^*:TEXT:*:#:{ echo -a themed $1- | haltdef }",
        ] {
            let engine = ScriptEngine::new();
            engine.load(script);
            let (actions, halted) = drive_event_halt(&engine, &ctx(), &event());
            assert!(halted, "default not suppressed for: {script}");
            assert!(
                matches!(actions.as_slice(), [Action::Echo { text, .. }] if text == "themed hello"),
                "handler did not run to completion for {script}: {actions:?}"
            );
        }
        // jIRC also honours /haltdef from a handler with no `^` prefix. mIRC
        // documents it as taking effect only inside a `^` handler, so this is
        // deliberately more permissive; pinned here so the difference is a
        // decision rather than a surprise.
        let plain = ScriptEngine::new();
        plain.load("on *:TEXT:*:#:{ haltdef | echo -a themed $1- }");
        let (actions, halted) = drive_event_halt(&plain, &ctx(), &event());
        assert!(!actions.is_empty(), "handler should still run");
        assert!(halted, "jIRC suppresses without `^` too");
    }

    #[test]
    fn haltdef_does_not_halt_the_running_routine() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias def { echo -a before | haltdef | echo -a after }
             alias hard { echo -a before | halt | echo -a after }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "def", ""),
            vec![
                Action::Echo {
                    target: "(status)".into(),
                    text: "before".into()
                },
                Action::Echo {
                    target: "(status)".into(),
                    text: "after".into()
                },
            ]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "hard", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "before".into()
            }]
        );
    }

    #[test]
    fn drive_event_reports_default_display_suppression() {
        let engine = ScriptEngine::new();
        engine.load("on ^*:TEXT:*:#:{ echo before | haltdef | echo after }");
        let event = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("bob".into()),
            target: "#c".into(),
            text: "hidden".into(),
            time: None,
        };
        let (actions, suppressed) = drive_event_halt(&engine, &ctx(), &event);
        assert!(suppressed);
        assert_eq!(
            actions,
            vec![
                Action::Echo {
                    target: "#c".into(),
                    text: "before".into()
                },
                Action::Echo {
                    target: "#c".into(),
                    text: "after".into()
                },
            ]
        );

        engine.load("on *:TEXT:*:#:{ halt }");
        assert!(!drive_event_halt(&engine, &ctx(), &event).1);
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
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #c :you said hi there".into())]
        );
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
    fn batched_per_mode_events_fire_in_argument_order() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:OP:#:{ /msg $chan op $opnick $modefirst $modelast }\n\
             on *:VOICE:#:{ /msg $chan voice $vnick $modefirst $modelast }\n\
             on *:DEHELP:#:{ /msg $chan dehelp $hnick $modefirst $modelast }",
        );
        let ev = UiEvent::Mode {
            server_id: "s".into(),
            target: "#c".into(),
            modes: "+ov-h bob alice carol".into(),
            by: Some("setter".into()),
        };
        assert_eq!(
            drive_event(&engine, &ctx(), &ev),
            vec![
                Action::Send("PRIVMSG #c :op bob $true $false".into()),
                Action::Send("PRIVMSG #c :voice alice $false $false".into()),
                Action::Send("PRIVMSG #c :dehelp carol $false $true".into()),
            ]
        );
    }

    #[test]
    fn batched_mode_events_skip_other_parameter_modes_without_shifting_nicks() {
        let isupport = crate::irc::state::Isupport::default();
        assert_eq!(
            split_mode_events("+klov secret 50 bob alice", &isupport),
            vec![("OP", "bob".to_string()), ("VOICE", "alice".to_string()),]
        );
        assert_eq!(
            split_mode_events("+o bob -v alice +b *!*@evil.host", &isupport),
            vec![
                ("OP", "bob".to_string()),
                ("DEVOICE", "alice".to_string()),
                ("BAN", "*!*@evil.host".to_string()),
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
        assert_eq!(
            actions,
            vec![Action::Send("PRIVMSG #test :op set +o bob".into())]
        );
    }

    #[test]
    fn rawmode_and_usermode_events() {
        let engine = ScriptEngine::new();
        engine.load("on *:RAWMODE:#:{ /msg $chan raw $1- }\non *:USERMODE:{ /msg me umode $1- }");
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
            vec![Action::Send(
                "PRIVMSG #test :victim was kicked by op (bye)".into()
            )]
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
                name: "1".into(),
                reps: 3,
                interval_ms: 5000,
                start_at: None,
                command: "/msg #c tick".into(),
                target: "#c".into(),
                offline: false,
                catch_up: false,
                ordered: false,
                milliseconds: false,
                high_resolution: false,
                dynamic: false,
                source: "<memory>".into(),
            }]
        );
    }

    #[test]
    fn timer_auto_names_and_ltimer_are_visible_in_the_creating_routine() {
        let engine = ScriptEngine::new();
        engine.load("alias t { /timer 1 5 /noop | /timer 1 5 /noop | /msg #c $ltimer }");
        let actions = engine.run_alias(&ctx(), "#c", "t", "");
        assert!(matches!(&actions[0], Action::Timer { name, .. } if name == "1"));
        assert!(matches!(&actions[1], Action::Timer { name, .. } if name == "2"));
        assert_eq!(actions[2], Action::Send("PRIVMSG #c :2".into()));
    }

    #[test]
    fn timer_callback_exposes_caller_and_ctimer() {
        let engine = ScriptEngine::new();
        assert_eq!(
            engine.run_timer_command(
                &ctx(),
                "#c",
                "/msg #c $caller $ctimer",
                "timers.mrc",
                "work",
            ),
            vec![Action::Send("PRIVMSG #c :timer work".into())]
        );
    }

    #[test]
    fn named_timer_query_targets_only_that_timer() {
        let engine = ScriptEngine::new();
        engine.load("alias q { /timerwork }");
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "q", ""),
            vec![Action::TimerList {
                target: "#c".into(),
                name: "work".into(),
            }]
        );
    }

    #[test]
    fn timer_switches_wall_clock_and_deferred_capture_match_mirc() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[(
            "timers.mrc".into(),
            "alias make { var %captured = created | /timerwork -chodi 14:30 0 250 /msg #c %captured $!time }".into(),
        )]);
        assert_eq!(
            engine.run_alias(&ctx(), "#c", "make", ""),
            vec![Action::Timer {
                name: "work".into(),
                reps: 0,
                interval_ms: 250,
                start_at: Some("14:30".into()),
                command: "/msg #c created $time".into(),
                target: "#c".into(),
                offline: true,
                catch_up: true,
                ordered: true,
                milliseconds: true,
                high_resolution: true,
                dynamic: true,
                source: "timers.mrc".into(),
            }]
        );
    }

    #[test]
    fn timer_control_switches_and_wildcards_produce_manager_actions() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias stop { /timer3? off }\n\
             alias exec { /timershow* -e }\n\
             alias pause { /timerwork -p }\n\
             alias freeze { /timerwork -P }\n\
             alias resume { /timerwork -r }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "stop", ""),
            vec![Action::TimerStop { name: "3?".into() }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "exec", ""),
            vec![Action::TimerExecute {
                name: "show*".into()
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "pause", ""),
            vec![Action::TimerPause {
                name: "work".into(),
                countdown: false,
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "freeze", ""),
            vec![Action::TimerPause {
                name: "work".into(),
                countdown: true,
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "resume", ""),
            vec![Action::TimerResume {
                name: "work".into()
            }]
        );
    }

    #[test]
    fn timer_online_dialog_reset_switch_is_an_intentional_ui_noop() {
        let engine = ScriptEngine::new();
        // mIRC's -z0/-z1/-z2 reset counters in its Online Timer dialog. jIRC
        // has no equivalent native dialog or total-time counter, so this must
        // not be mistaken for a request to create or control a script timer.
        engine.load("alias reset { /timer -z0 | /timer -z1 | /timer -z2 }");
        assert!(engine.run_alias(&ctx(), "", "reset", "").is_empty());
    }

    #[test]
    fn deferred_timer_command_keeps_file_local_alias_context() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "alias -l localonly { /msg #c one }\nalias make { /timerwork 1 1 /localonly }"
                    .into(),
            ),
            (
                "two.mrc".into(),
                "alias -l localonly { /msg #c two }".into(),
            ),
        ]);
        let action = engine.run_alias(&ctx(), "#c", "make", "").remove(0);
        let (command, source) = match action {
            Action::Timer {
                command, source, ..
            } => (command, source),
            other => panic!("expected timer action, got {other:?}"),
        };
        assert_eq!(
            engine.run_command_from_source(&ctx(), "#c", &command, &[], &source),
            vec![Action::Send("PRIVMSG #c :one".into())]
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
            Some(UiEvent::Message {
                from,
                target,
                text,
                kind,
                ..
            }) => {
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
        let echo = |t: &str| Action::Echo {
            target: "#c".into(),
            text: t.into(),
        };
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
                start_at: None,
                command: "/msg #c tick".into(),
                target: "#c".into(),
                offline: false,
                catch_up: false,
                ordered: false,
                milliseconds: false,
                high_resolution: false,
                dynamic: false,
                source: "<memory>".into(),
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

    #[test]
    fn dcc_chat_and_file_events_expose_mirc_context() {
        let engine = ScriptEngine::new();
        engine.load(
            "on 1:CHAT:*help*:{ msg =$nick direct reply }\n\
             on 1:OPEN:=:*:{ echo -s opened $target }\n\
             on 1:FILERCVD:*.zip:{ echo -s got $filename from $nick }",
        );
        let chat = EventVars {
            nick: "bob".into(),
            target: "=bob".into(),
            text: "please help me".into(),
            params: vec!["please".into(), "help".into(), "me".into()],
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "CHAT", chat),
            vec![Action::Send("PRIVMSG =bob :direct reply".into())]
        );
        let open = EventVars {
            nick: "bob".into(),
            target: "=bob".into(),
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "OPEN", open),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "opened =bob".into(),
            }]
        );
        let file = EventVars {
            nick: "bob".into(),
            text: "archive.zip".into(),
            filename: "C:\\dcc\\archive.zip".into(),
            ..Default::default()
        };
        assert_eq!(
            engine.dispatch_event(&ctx(), "FILERCVD", file),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "got C:\\dcc\\archive.zip from bob".into(),
            }]
        );
    }

    #[test]
    fn dcc_command_is_client_local_not_a_raw_irc_line() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias p { dcc passive on }\n\
             alias legacy { pdcc off }\n\
             alias s { dccserver +scf on 50059 }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "p", ""),
            vec![Action::Dcc {
                args: "passive on".into(),
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "legacy", ""),
            vec![Action::Dcc {
                args: "passive off".into(),
            }]
        );
        assert_eq!(
            engine.run_alias(&ctx(), "", "s", ""),
            vec![Action::DccServer {
                args: "+scf on 50059".into(),
            }]
        );
    }

    struct ConfiguredDcc;

    impl ScriptDcc for ConfiguredDcc {
        fn snapshot(&self, _: &str) -> Vec<eval::DccInfo> {
            Vec::new()
        }

        fn server_port(&self) -> Option<u16> {
            None
        }

        fn bind_ip(&self) -> String {
            "192.0.2.10".into()
        }

        fn passive(&self) -> bool {
            true
        }
    }

    #[test]
    fn dcc_configuration_identifiers_read_the_current_backend() {
        let engine = ScriptEngine::new();
        engine.set_dcc(std::sync::Arc::new(ConfiguredDcc));
        engine.load("alias inspect { echo -s $bindip $passivedcc }");

        assert_eq!(
            engine.run_alias(&ctx(), "", "inspect", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "192.0.2.10 on".into(),
            }]
        );
    }

    #[test]
    fn dccserver_event_matches_service_and_can_reject() {
        let engine = ScriptEngine::new();
        engine.load(
            "on 1:DCCSERVER:Send:{ echo -s $nick $address $filename | halt }\n\
             on 1:DCCSERVER:Chat:{ echo -s chat }",
        );
        let event = EventVars {
            nick: "visitor".into(),
            text: "send".into(),
            filename: "payload.bin".into(),
            peer_address: "192.0.2.5".into(),
            ..Default::default()
        };
        let (actions, halted) = engine.dispatch_event_halt(&ctx(), "DCCSERVER", event);
        assert!(halted);
        assert_eq!(
            actions,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "visitor 192.0.2.5 payload.bin".into(),
            }]
        );
    }

    #[test]
    fn play_command_creates_a_local_queue_action_with_script_origin() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[(
            "one.mrc".into(),
            "alias go { play -e #room lines.txt 25 }".into(),
        )]);
        assert_eq!(
            engine.run_alias(&ctx(), "#current", "go", ""),
            vec![Action::Play {
                args: "-e #room lines.txt 25".into(),
                current_target: "#current".into(),
                remote: false,
                source: "one.mrc".into(),
            }]
        );
    }

    #[test]
    fn play_identifiers_report_queue_and_deferred_target() {
        struct FakePlay;
        impl crate::script::eval::ScriptPlay for FakePlay {
            fn snapshot(&self) -> Vec<crate::script::eval::PlayInfo> {
                vec![
                    crate::script::eval::PlayInfo {
                        target: "#a".into(),
                        status: "playing".into(),
                        filename: "a.txt".into(),
                        ..Default::default()
                    },
                    crate::script::eval::PlayInfo {
                        target: "#b".into(),
                        status: "queued".into(),
                        filename: "b.txt".into(),
                        ..Default::default()
                    },
                    crate::script::eval::PlayInfo {
                        target: "#a".into(),
                        status: "paused".into(),
                        filename: "c.txt".into(),
                        ..Default::default()
                    },
                ]
            }
        }

        let engine = ScriptEngine::new();
        engine.set_play(std::sync::Arc::new(FakePlay));
        engine.load(
            "alias inspect { echo -a $play(0) $play(1) $play(#a,0) $play(#a,2).status $play(2).fname }",
        );
        assert_eq!(
            engine.run_alias(&ctx(), "#current", "inspect", ""),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "3 #a 2 paused b.txt".into(),
            }]
        );
        assert_eq!(
            engine.run_play_command(&ctx(), "#current", "echo -a $pnick", "", "#dest"),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "#dest".into(),
            }]
        );
    }

    #[test]
    fn play_alias_retains_file_local_alias_resolution() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "one.mrc".into(),
                "alias -l item { msg #c one-$pnick-$1- }".into(),
            ),
            (
                "two.mrc".into(),
                "alias -l item { msg #c two-$pnick-$1- }".into(),
            ),
        ]);
        assert_eq!(
            engine.run_play_alias(
                &ctx(),
                "#current",
                "item",
                "hello world",
                "one.mrc",
                "#dest",
            ),
            vec![Action::Send("PRIVMSG #c :one-#dest-hello world".into())]
        );
    }

    #[test]
    fn custom_identifier_iif_v1_and_style_work_in_popup_labels() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[(
            "i7.mrc".into(),
            r#"
alias -l i7n_addrnick {
  var %selected = $snick($active,1)
  if ($1 == 0) { return %selected }
  if ($1 == 1) { return %selected }
  if ($1 == 3) { return Carol }
}
menu nicklist {
  $iif($i7n_addrnick(0) != $null,$style(2) $v1,$style(2) GateKeeper lookup pending):noop
  $style(2) $i7n_addrnick(1):noop
  $iif($i7n_addrnick(2) != $null,$style(2) $v1):noop
  $iif($i7n_addrnick(3) != $null,$style(2) $v1):noop
}
"#
            .into(),
        )]);

        let items = engine.popups_evaluated(&ctx(), "nicklist", "Guest", "#room");
        assert_eq!(
            items
                .iter()
                .map(|item| (item.label.as_str(), item.disabled, item.command.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Guest", true, "noop"),
                ("Guest", true, "noop"),
                ("Carol", true, "noop"),
            ]
        );
    }

    #[test]
    fn portable_event_tail_exposes_application_char_hotlink_logon_and_serv_context() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:APPACTIVE:echo -a app-$appactive\n\
             on *:CHAR:@keys:65:echo -a char-$keyval-$keychar-$keyrpt\n\
             on *:HOTLINK:*foo*:@:echo -a hot $1 $hotlink(event) $hotlink(word).pos $hotlink(line).pos\n\
             on *:LOGON:Net:echo -a logon-$network-$server\n\
             on *:SERV:dir*:echo -a serv-$nick-$cd-$1-",
        );

        assert_eq!(
            engine.dispatch_event(&ctx(), "APPACTIVE", EventVars::default()),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "app-$false".into()
            }]
        );
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "CHAR",
                EventVars {
                    target: "@keys".into(),
                    key_char: "A".into(),
                    key_val: Some(65),
                    key_repeat: true,
                    ..Default::default()
                },
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "char-65-A-$true".into()
            }]
        );
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "HOTLINK",
                EventVars {
                    target: "@links".into(),
                    text: "food".into(),
                    params: vec!["food".into()],
                    hotlink_event: "dclick".into(),
                    hotlink_line: 7,
                    hotlink_pos: 3,
                    hotlink_line_text: "one two food".into(),
                    ..Default::default()
                },
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "hot food dclick 3 7".into()
            }]
        );
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "LOGON",
                EventVars {
                    chan: "Net".into(),
                    target: "Net".into(),
                    text: "Net".into(),
                    ..Default::default()
                },
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "logon-Net-irc.example.org".into()
            }]
        );
        assert_eq!(
            engine.dispatch_event(
                &ctx(),
                "SERV",
                EventVars {
                    nick: "guest".into(),
                    target: "!guest".into(),
                    text: "dir files".into(),
                    params: vec!["dir".into(), "files".into()],
                    current_dir: "root/".into(),
                    ..Default::default()
                },
            ),
            vec![Action::Echo {
                target: "(status)".into(),
                text: "serv-guest-root/-dir files".into()
            }]
        );
    }

    #[test]
    fn logon_early_and_normal_handlers_run_in_their_registration_phases() {
        let engine = ScriptEngine::new();
        engine.load(
            "on ^*:LOGON:*:{ echo -a early | haltdef }\n\
             on *:LOGON:*:echo -a normal",
        );
        let vars = EventVars {
            chan: "Net".into(),
            target: "Net".into(),
            text: "Net".into(),
            ..Default::default()
        };
        let (early, _, halted) =
            engine.dispatch_event_status(&ctx(), "LOGON", vars.clone(), None, Some(true));
        assert_eq!(
            early,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "early".into()
            }]
        );
        assert!(halted);
        assert_eq!(
            engine
                .dispatch_event_status(&ctx(), "LOGON", vars, None, Some(false))
                .0,
            vec![Action::Echo {
                target: "(status)".into(),
                text: "normal".into()
            }]
        );
    }

    #[test]
    fn server_modes_and_missing_remote_sounds_fire_specific_events() {
        let engine = ScriptEngine::new();
        engine.load(
            "on *:SERVERMODE:#:echo -a servermode-$nick-$chan-$1-\n\
             on *:SERVEROP:#:echo -a serverop-$nick-$opnick\n\
             on *:NOSOUND:echo -a nosound-$nick-$filename",
        );
        let mode = UiEvent::Mode {
            server_id: "s".into(),
            target: "#c".into(),
            modes: "+o bob".into(),
            by: Some("irc.example.org".into()),
        };
        let raw = raw_event_context(
            ":irc.example.org MODE #c +o bob",
            b":irc.example.org MODE #c +o bob",
        );
        let actions = drive_event_halt_raw(&engine, &ctx(), &mode, Some(&raw)).0;
        assert!(actions.contains(&Action::Echo {
            target: "(status)".into(),
            text: "servermode-irc.example.org-#c-+o bob".into()
        }));
        assert!(actions.contains(&Action::Echo {
            target: "(status)".into(),
            text: "serverop-irc.example.org-bob".into()
        }));

        let sound = UiEvent::Message {
            server_id: "s".into(),
            kind: MessageKind::Privmsg,
            from: Some("alice".into()),
            target: "me".into(),
            text: "\u{1}SOUND missing-event-test.wav\u{1}".into(),
            time: None,
        };
        assert!(
            drive_event(&engine, &ctx(), &sound).contains(&Action::Echo {
                target: "(status)".into(),
                text: "nosound-alice-missing-event-test.wav".into(),
            })
        );
    }
}
