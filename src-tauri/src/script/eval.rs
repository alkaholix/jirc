//! The mSL evaluator: identifier/variable expansion, condition evaluation,
//! control flow, and the built-in command library.

use std::collections::HashMap;

use super::ast::{group_var_key, Script, Stmt};
use super::ident;

/// A side effect produced by running a script.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A raw line to send to the server.
    Send(String),
    /// Text to display locally in `target` (channel/query/status).
    Echo { target: String, text: String },
    /// Schedule a command to run later, `reps` times every `interval_ms`.
    /// An empty `name` means auto-assign one.
    Timer {
        name: String,
        reps: u32,
        interval_ms: u64,
        command: String,
        target: String,
    },
    /// Stop a named timer (`name` = "*" stops all).
    TimerStop { name: String },
    /// Echo the list of active timers into `target`.
    TimerList { target: String },
    /// Open a TCP socket (`/sockopen`); `tls` for `-e` (encrypted).
    SockOpen { name: String, host: String, port: u16, tls: bool },
    /// Write bytes to a socket (`/sockwrite`).
    SockWrite { name: String, data: Vec<u8> },
    /// Close a socket (`/sockclose`).
    SockClose { name: String },
    /// Start the accept loop for a listener bound by `/socklisten` (carries the
    /// owning connection's context to apply-time so events route correctly).
    SockListen { name: String },
    /// Open a custom dialog (`/dialog`).
    DialogOpen {
        name: String,
        title: String,
        controls: Vec<super::ast::DialogControl>,
    },
    /// Close a dialog (`/dialog -c`).
    DialogClose { name: String },
    /// Mutate a dialog control (`/did`): `op` is set/add/clear.
    DialogSet {
        dialog: String,
        control: String,
        op: String,
        value: String,
    },
    /// Set (or clear, if empty) a nick-list icon for a nick (`/nickicon`).
    NickIcon { nick: String, icon: String },
    /// Open a custom `@window` (`/window`).
    WindowOpen { name: String, kind: String, title: String },
    /// Close a custom `@window` (`/window -c`).
    WindowClose { name: String },
    /// A line op on a custom `@window`: `op` = add/insert/replace/delete/clear.
    WindowLine { name: String, op: String, n: u32, text: String },
    /// Set a stored identity field (`/anick`/`/mnick`/`/fullname`). `field` is
    /// `anick`/`mnick`/`fullname`; updates the live session state so the matching
    /// `$anick`/`$mnick`/`$fullname` reflects it.
    SetIdentity { field: String, value: String },
    /// Recompile every script file from disk (`/reload`).
    ReloadScripts,
    /// Define/replace (`command` = Some) or remove (`command` = None) a runtime
    /// alias (`/alias <name> [command]`). Persisted to a `_runtime.mrc` file.
    DefineAlias { name: String, command: Option<String> },
    /// Fire `on SIGNAL` handlers matching `name` (`/signal`); `params` become `$1-`.
    Signal { name: String, params: Vec<String> },
    /// Control the connect-time autojoin from within `on CONNECT` (`/autojoin`):
    /// `skip` cancels it, `delay_secs` > 0 postpones it that many seconds.
    Autojoin { skip: bool, delay_secs: u32 },
}

/// Reserved `%var` key holding the byte count of the last `/sockread` (read by
/// `$sockbr`); the NUL char can't appear in a real variable name.
pub const SOCK_BR_KEY: &str = "\u{0}sockbr";

/// Sentinel that `$style(N)` returns; consumed while building a popup menu (a
/// Private-Use char, so it can't collide with a real label). The digit that
/// follows is mIRC's style: 1 = checked, 2 = disabled, 3 = both.
pub const STYLE_MARK: char = '\u{E000}';

/// Per-invocation variables ($nick, $chan, params, …).
#[derive(Debug, Clone, Default)]
pub struct EventVars {
    pub nick: String,
    pub chan: String,
    pub target: String,
    pub text: String,
    pub params: Vec<String>,
    /// Selected nicknames for a nicklist popup run, exposed as `$snick`/`$snicks`.
    /// Empty for every other run (timers, typed commands, events).
    pub snicks: Vec<String>,
    /// Secondary nick for events that involve two people (e.g. `on KICK`'s
    /// kicked user, exposed as `$knick`).
    pub knick: String,
    /// Dialog control values (id -> value) for `on DIALOG`, read by `$did`.
    pub did: std::collections::HashMap<String, String>,
    /// The event type name, e.g. "text"/"raw"/"op" — exposed as `$event`.
    pub event: String,
    /// The numeric of a raw server line (`on RAW`) — exposed as `$numeric`.
    pub numeric: String,
}

const STEP_LIMIT: u32 = 100_000;

/// Sentinel `goto` targets for `/break` and `/continue` — the NUL prefix keeps
/// them from colliding with any real `:label`. Consumed by `Stmt::While`.
const LOOP_BREAK: &str = "\u{0}break";
const LOOP_CONTINUE: &str = "\u{0}continue";
const STATUS: &str = "(status)";

/// Synchronous socket operations the engine can call *during* a run, so
/// `/socklisten` binds immediately and `$sock(name).port` is readable on the
/// same line (like mIRC). The production backend is the SocketManager; tests use
/// [`NoSockets`] or a fake. Names may be wildcards for the query methods.
pub trait ScriptSockets: Send + Sync {
    /// Binds a listening socket; returns the bound port (`port == 0` → OS-assigned).
    fn listen(&self, name: &str, port: u16) -> Option<u16>;
    /// Accepts the pending incoming connection of listener `listener` into a
    /// socket named `name`.
    fn accept(&self, name: &str, listener: &str) -> bool;
    fn set_mark(&self, name: &str, mark: &str);
    /// `/sockrename <name> <newname>`.
    fn rename(&self, name: &str, newname: &str);
    /// `/sockpause [-r]` — pause (or, with `resume`, restart) reading.
    fn pause(&self, name: &str, resume: bool);
    /// Whether a socket matching `name` (possibly a wildcard) exists.
    fn exists(&self, name: &str) -> bool;
    /// `$sock(name).property` value (empty for unknown name/property).
    fn prop(&self, name: &str, property: &str) -> String;
    /// `/socklist` — formatted lines for sockets matching `filter`.
    fn list(&self, filter: &str) -> Vec<String>;
}

/// A no-op socket backend (used in tests and before a real one is installed).
pub struct NoSockets;
impl ScriptSockets for NoSockets {
    fn listen(&self, _: &str, _: u16) -> Option<u16> {
        None
    }
    fn accept(&self, _: &str, _: &str) -> bool {
        false
    }
    fn set_mark(&self, _: &str, _: &str) {}
    fn rename(&self, _: &str, _: &str) {}
    fn pause(&self, _: &str, _: bool) {}
    fn exists(&self, _: &str) -> bool {
        false
    }
    fn prop(&self, _: &str, _: &str) -> String {
        String::new()
    }
    fn list(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A text prompt the engine shows *during* a run for `$input`, blocking until
/// the user answers (like mIRC's modal prompt). The production backend drives
/// the UI dialog; tests use [`NoInput`].
pub trait ScriptInput: Send + Sync {
    /// Shows a prompt pre-filled with `default`; returns the entered text, or
    /// `None` if cancelled.
    fn prompt(&self, message: &str, title: &str, default: &str) -> Option<String>;
}

/// A no-op input backend (tests / before a real one is installed): returns the
/// default so a non-interactive run proceeds without a UI.
pub struct NoInput;
impl ScriptInput for NoInput {
    fn prompt(&self, _: &str, _: &str, default: &str) -> Option<String> {
        Some(default.to_string())
    }
}

/// The execution context for a single alias/event run.
pub struct Runtime<'a> {
    pub script: &'a Script,
    pub my_nick: &'a str,
    pub network: &'a str,
    pub server: &'a str,
    pub vars: &'a mut HashMap<String, String>,
    pub hashes: &'a mut HashMap<String, HashMap<String, String>>,
    pub event: EventVars,
    pub actions: Vec<Action>,
    pub halted: bool,
    pub steps: u32,
    pub depth: u32,
    /// Value set by `/return`, consumed when an alias is used as `$identifier`.
    pub ret: Option<String>,
    /// Pending `/goto` target, bubbled up until a body containing the label
    /// resolves it.
    pub goto: Option<String>,
    /// Sandbox directory for `$read`/`/write` file I/O.
    pub data_dir: std::path::PathBuf,
    /// Live channel/member snapshot for state-aware identifiers.
    pub state: std::sync::Arc<crate::irc::state::StateSnapshot>,
    /// Synchronous socket backend for `/socklisten`/`/sockaccept`/`$sock(...)`.
    pub sockets: std::sync::Arc<dyn ScriptSockets>,
    /// Backend for `$input` prompts.
    pub input: std::sync::Arc<dyn ScriptInput>,
    /// Open file handles for `/fopen`/`/fwrite`/`$fread`/`$fopen(...)`.
    pub files: &'a mut crate::script::files::FileStore,
    /// Binary variables for `/bset`/`/bunset`/`$bvar`/`$bfind`/`&binvar`.
    pub bins: &'a mut crate::script::binvar::BinStore,
    /// Custom `@windows` for `/window`/`/aline`/`/rline`/`$window`/`$line`.
    pub windows: &'a mut crate::script::window::WindowStore,
    /// What invoked the current alias frame ("command"/"event"/"menu"/"identifier"),
    /// for `$caller`/`$isid`. Saved + restored around nested alias calls.
    pub caller: &'static str,
}

impl<'a> Runtime<'a> {
    pub fn run(&mut self, body: &[Stmt]) {
        self.depth += 1;
        if self.depth > 64 {
            self.halted = true;
        }
        let mut i = 0;
        while i < body.len() {
            if self.halted || self.steps > STEP_LIMIT {
                break;
            }
            // Resolve a pending `/goto` — jump if its label is in this body,
            // otherwise bubble up so an enclosing block can resolve it. The step
            // cap guards against runaway loops.
            if let Some(label) = self.goto.clone() {
                match find_label(body, &label) {
                    Some(idx) => {
                        self.goto = None;
                        i = idx + 1;
                        continue;
                    }
                    None => break,
                }
            }
            self.steps += 1;
            match &body[i] {
                Stmt::Command { name, args } if name.eq_ignore_ascii_case("goto") => {
                    self.goto = Some(self.expand(args));
                    // The loop top resolves it (jump here or bubble up).
                }
                Stmt::Command { name, .. } if name.eq_ignore_ascii_case("break") => {
                    // Exit the innermost while loop (sentinel consumed by Stmt::While).
                    self.goto = Some(LOOP_BREAK.to_string());
                }
                Stmt::Command { name, .. } if name.eq_ignore_ascii_case("continue") => {
                    // Skip to the next iteration of the innermost while loop.
                    self.goto = Some(LOOP_CONTINUE.to_string());
                }
                stmt => {
                    let stmt = stmt.clone();
                    self.exec(&stmt);
                    if self.goto.is_none() {
                        i += 1;
                    }
                }
            }
        }
        self.depth -= 1;
    }

    fn exec(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Command { name, args } => self.dispatch(name, args),
            Stmt::If {
                branches,
                else_body,
            } => {
                for (cond, body) in branches {
                    if self.eval_cond(cond) {
                        let body = body.clone();
                        self.run(&body);
                        return;
                    }
                }
                if let Some(body) = else_body {
                    let body = body.clone();
                    self.run(&body);
                }
            }
            Stmt::While { cond, body } => {
                while !self.halted
                    && self.goto.is_none()
                    && self.eval_cond(cond)
                    && self.steps <= STEP_LIMIT
                {
                    self.steps += 1;
                    let body = body.clone();
                    self.run(&body);
                    match self.goto.as_deref() {
                        Some(LOOP_CONTINUE) => self.goto = None, // re-check the condition
                        Some(LOOP_BREAK) => {
                            self.goto = None;
                            break;
                        }
                        Some(_) => break, // a real goto out of the loop bubbles up
                        None => {}
                    }
                }
            }
            Stmt::Label(_) => {} // a jump target; no-op when reached normally
        }
    }

    // ---- command dispatch ----

    fn dispatch(&mut self, name: &str, raw_args: &str) {
        let lname = name.to_ascii_lowercase();
        // mIRC's silent prefix: `.command` runs the command but suppresses its
        // output. We don't echo command output anyway, so just drop a leading
        // dot — otherwise `.timer`, `.msg`, `.notice`, … fail to match and get
        // mis-sent to the server as a raw line.
        let lname = lname.strip_prefix('.').unwrap_or(lname.as_str());
        match lname {
            "echo" => self.cmd_echo(raw_args),
            "say" => {
                let text = self.expand(raw_args);
                let target = self.reply_target();
                if !target.is_empty() {
                    self.send_privmsg(&target, &text);
                }
            }
            "msg" | "m" => {
                let (target, text) = self.split_target(raw_args);
                if !target.is_empty() {
                    self.send_privmsg(&target, &text);
                }
            }
            "notice" => {
                let (target, text) = self.split_target(raw_args);
                if !target.is_empty() {
                    self.actions.push(Action::Send(format!("NOTICE {target} :{text}")));
                }
            }
            "me" => {
                let text = self.expand(raw_args);
                let target = self.reply_target();
                if !target.is_empty() {
                    self.actions
                        .push(Action::Send(format!("PRIVMSG {target} :\u{1}ACTION {text}\u{1}")));
                }
            }
            "describe" => {
                let (target, text) = self.split_target(raw_args);
                if !target.is_empty() {
                    self.actions
                        .push(Action::Send(format!("PRIVMSG {target} :\u{1}ACTION {text}\u{1}")));
                }
            }
            "join" | "j" => {
                let ch = self.expand(raw_args);
                if !ch.is_empty() {
                    self.actions.push(Action::Send(format!("JOIN {ch}")));
                }
            }
            "part" => {
                let ch = self.expand(raw_args);
                let ch = if ch.is_empty() { self.event.chan.clone() } else { ch };
                if !ch.is_empty() {
                    self.actions.push(Action::Send(format!("PART {ch}")));
                }
            }
            "nick" => {
                let n = self.expand(raw_args);
                if !n.is_empty() {
                    self.actions.push(Action::Send(format!("NICK {n}")));
                }
            }
            "mode" => {
                let m = self.expand(raw_args);
                self.actions.push(Action::Send(format!("MODE {m}")));
            }
            "topic" => {
                let (target, text) = self.split_target(raw_args);
                self.actions.push(Action::Send(format!("TOPIC {target} :{text}")));
            }
            "kick" => {
                // /kick <#channel> <nick> [reason]
                let s = self.expand(raw_args);
                let mut it = s.splitn(3, char::is_whitespace);
                if let (Some(chan), Some(nick)) = (it.next(), it.next()) {
                    let line = match it.next().filter(|r| !r.is_empty()) {
                        Some(reason) => format!("KICK {chan} {nick} :{reason}"),
                        None => format!("KICK {chan} {nick}"),
                    };
                    self.actions.push(Action::Send(line));
                }
            }
            "invite" => {
                // /invite <nick> <#channel>
                let s = self.expand(raw_args);
                let mut it = s.split_whitespace();
                if let (Some(nick), Some(chan)) = (it.next(), it.next()) {
                    self.actions.push(Action::Send(format!("INVITE {nick} {chan}")));
                }
            }
            "hop" => {
                // /hop [#channel] — cycle the channel (part then rejoin).
                let ch = self.expand(raw_args);
                let ch = if ch.is_empty() { self.event.chan.clone() } else { ch };
                if !ch.is_empty() {
                    self.actions.push(Action::Send(format!("PART {ch}")));
                    self.actions.push(Action::Send(format!("JOIN {ch}")));
                }
            }
            "knock" => {
                let (chan, msg) = self.split_target(raw_args);
                if !chan.is_empty() {
                    let line = if msg.is_empty() {
                        format!("KNOCK {chan}")
                    } else {
                        format!("KNOCK {chan} :{msg}")
                    };
                    self.actions.push(Action::Send(line));
                }
            }
            "away" => {
                // /away [message] — an empty message clears away status.
                let msg = self.expand(raw_args);
                let line = if msg.is_empty() {
                    "AWAY".to_string()
                } else {
                    format!("AWAY :{msg}")
                };
                self.actions.push(Action::Send(line));
            }
            "omsg" => {
                // /omsg <#channel> <message> — message to channel ops (@#chan).
                let (chan, text) = self.split_target(raw_args);
                if chan.starts_with('#') && !text.is_empty() {
                    self.actions.push(Action::Send(format!("PRIVMSG @{chan} :{text}")));
                }
            }
            "onotice" => {
                let (chan, text) = self.split_target(raw_args);
                if chan.starts_with('#') && !text.is_empty() {
                    self.actions.push(Action::Send(format!("NOTICE @{chan} :{text}")));
                }
            }
            "ctcp" => {
                // /ctcp <target> <ctcp> [params] — send a CTCP request (PRIVMSG)
                // and echo it locally as `-> [target] CTCP`, like mIRC. PING with
                // no explicit param carries a millisecond timestamp so the reply
                // yields a round-trip latency (kept out of the local echo).
                let s = self.expand(raw_args);
                let mut it = s.splitn(3, char::is_whitespace);
                if let (Some(target), Some(ctcp)) = (it.next(), it.next()) {
                    let cmd = ctcp.to_ascii_uppercase();
                    let extra = it.next().filter(|t| !t.is_empty());
                    let shown = match extra {
                        Some(t) => format!("{cmd} {t}"),
                        None => cmd.clone(),
                    };
                    let body = match (extra, cmd.as_str()) {
                        (Some(t), _) => format!("{cmd} {t}"),
                        (None, "PING") => {
                            let ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis())
                                .unwrap_or(0);
                            format!("PING {ms}")
                        }
                        (None, _) => cmd.clone(),
                    };
                    self.actions
                        .push(Action::Send(format!("PRIVMSG {target} :\u{1}{body}\u{1}")));
                    let rt = self.reply_target();
                    self.actions.push(Action::Echo {
                        target: rt,
                        text: format!("-> [{target}] {shown}"),
                    });
                }
            }
            "ctcpreply" => {
                // /ctcpreply <nick> <ctcp> [text] — a CTCP reply (NOTICE).
                let s = self.expand(raw_args);
                let mut it = s.splitn(3, char::is_whitespace);
                if let (Some(nick), Some(ctcp)) = (it.next(), it.next()) {
                    let body = match it.next().filter(|t| !t.is_empty()) {
                        Some(t) => format!("{} {}", ctcp.to_ascii_uppercase(), t),
                        None => ctcp.to_ascii_uppercase(),
                    };
                    self.actions.push(Action::Send(format!("NOTICE {nick} :\u{1}{body}\u{1}")));
                }
            }
            "nickserv" | "ns" => self.send_service("NickServ", raw_args),
            "chanserv" | "cs" => self.send_service("ChanServ", raw_args),
            "memoserv" | "ms" => self.send_service("MemoServ", raw_args),
            "quit" => {
                let msg = self.expand(raw_args);
                self.actions.push(Action::Send(format!("QUIT :{msg}")));
            }
            "raw" | "quote" => {
                let line = self.expand(raw_args);
                if !line.is_empty() {
                    self.actions.push(Action::Send(line));
                }
            }
            "set" => self.cmd_set(raw_args, false),
            "var" => self.cmd_set(raw_args, true),
            "unset" => self.cmd_unset(raw_args),
            "enable" => self.cmd_set_group(raw_args, true),
            "disable" => self.cmd_set_group(raw_args, false),
            "groups" => self.cmd_groups(raw_args),
            "unsetall" => {
                // Remove all user %variables; engine-internal reserved keys (group
                // state, etc.) are NUL-prefixed and kept.
                self.vars.retain(|k, _| k.starts_with('\u{0}'));
            }
            "anick" => self.set_identity("anick", raw_args),
            "mnick" => self.set_identity("mnick", raw_args),
            "fullname" => self.set_identity("fullname", raw_args),
            "flushini" | "saveini" => {
                // No-op: jIRC writes INI/JSON to disk immediately (no cache).
            }
            "reload" => self.actions.push(Action::ReloadScripts),
            "signal" => {
                // `/signal [-n] [-d] <name> [parameters]` fires `on *:SIGNAL:<name>`
                // handlers ($signal = name, $1- = params). Switches are accepted
                // but the signal is always dispatched after the current run (mIRC's
                // default, non-`-n`, behaviour).
                let mut rest = self.expand(raw_args).trim().to_string();
                while rest.starts_with('-') {
                    match rest.split_once(char::is_whitespace) {
                        Some((_, after)) => rest = after.trim().to_string(),
                        None => {
                            rest.clear();
                            break;
                        }
                    }
                }
                let (name, params) = match rest.split_once(char::is_whitespace) {
                    Some((n, p)) => (
                        n.to_string(),
                        p.split_whitespace().map(String::from).collect(),
                    ),
                    None => (rest.clone(), Vec::new()),
                };
                if !name.is_empty() {
                    self.actions.push(Action::Signal { name, params });
                }
            }
            "autojoin" => {
                // `/autojoin [-n|-s|-dN]` controls the connect-time autojoin (used
                // in `on CONNECT`): `-n` join now (default), `-s` skip, `-dN` delay
                // N seconds.
                let mut skip = false;
                let mut delay_secs = 0u32;
                for tok in self.expand(raw_args).split_whitespace() {
                    if tok == "-s" {
                        skip = true;
                    } else if tok == "-n" {
                        skip = false;
                        delay_secs = 0;
                    } else if let Some(n) = tok.strip_prefix("-d") {
                        delay_secs = n.parse().unwrap_or(0);
                    }
                }
                self.actions.push(Action::Autojoin { skip, delay_secs });
            }
            "alias" => {
                // `/alias <name> <command>` adds/replaces; `/alias <name>` (no
                // command) removes. Single-line only. The command is stored
                // unexpanded (identifiers resolve when the alias runs). A leading
                // [filename] arg (mIRC) isn't supported — jIRC has one script set.
                let raw = raw_args.trim();
                let (name, command) = match raw.split_once(char::is_whitespace) {
                    Some((n, c)) => (n.to_string(), Some(c.trim().to_string())),
                    None => (raw.to_string(), None),
                };
                if !name.is_empty() {
                    self.actions.push(Action::DefineAlias { name, command });
                }
            }
            "inc" => self.cmd_incdec(raw_args, 1),
            "dec" => self.cmd_incdec(raw_args, -1),
            "write" => self.cmd_write(raw_args),
            "writeini" => self.cmd_writeini(raw_args),
            "remini" => self.cmd_remini(raw_args),
            "fopen" => self.cmd_fopen(raw_args),
            "fwrite" => self.cmd_fwrite(raw_args),
            "fclose" => self.cmd_fclose(raw_args),
            "fseek" => self.cmd_fseek(raw_args),
            "bset" => self.cmd_bset(raw_args),
            "bunset" => self.cmd_bunset(raw_args),
            "bcopy" => self.cmd_bcopy(raw_args),
            "breplace" => self.cmd_breplace(raw_args),
            "btrunc" => self.cmd_btrunc(raw_args),
            "bread" => self.cmd_bread(raw_args),
            "bwrite" => self.cmd_bwrite(raw_args),
            "window" => self.cmd_window(raw_args),
            "aline" => self.cmd_window_line(raw_args, "add"),
            "rline" => self.cmd_window_line(raw_args, "replace"),
            "iline" => self.cmd_window_line(raw_args, "insert"),
            "dline" => self.cmd_window_line(raw_args, "delete"),
            "clear" => self.cmd_window_clear(raw_args),
            "mkdir" => {
                let dir = self.expand(raw_args);
                if !dir.trim().is_empty() {
                    let _ = std::fs::create_dir_all(sandbox_path(&self.data_dir, dir.trim()));
                }
            }
            "rmdir" => {
                let dir = self.expand(raw_args);
                if !dir.trim().is_empty() {
                    let _ = std::fs::remove_dir(sandbox_path(&self.data_dir, dir.trim()));
                }
            }
            "remove" => {
                let f = self.expand(raw_args);
                if !f.trim().is_empty() {
                    let _ = std::fs::remove_file(sandbox_path(&self.data_dir, f.trim()));
                }
            }
            "rename" => {
                let s = self.expand(raw_args);
                if let Some((old, new)) = s.trim().split_once(char::is_whitespace) {
                    let _ = std::fs::rename(
                        sandbox_path(&self.data_dir, old.trim()),
                        sandbox_path(&self.data_dir, new.trim()),
                    );
                }
            }
            "copy" => {
                // /copy [-switches] <source> <target>
                let s = self.expand(raw_args);
                let mut rest = s.trim();
                while rest.starts_with('-') {
                    rest = rest.split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or("").trim();
                }
                if let Some((src, dst)) = rest.split_once(char::is_whitespace) {
                    let _ = std::fs::copy(
                        sandbox_path(&self.data_dir, src.trim()),
                        sandbox_path(&self.data_dir, dst.trim()),
                    );
                }
            }
            "sockopen" => self.cmd_sockopen(raw_args),
            "sockwrite" => self.cmd_sockwrite(raw_args),
            "sockclose" => {
                let name = self.expand(raw_args.trim());
                if !name.is_empty() {
                    self.actions.push(Action::SockClose { name });
                }
            }
            "socklisten" => self.cmd_socklisten(raw_args),
            "sockaccept" => {
                let name = self.expand(raw_args.trim());
                if !name.is_empty() {
                    // $sockname (the listener) identifies whose pending connection.
                    let listener = self.event.chan.clone();
                    self.sockets.accept(&name, &listener);
                }
            }
            "sockmark" => self.cmd_sockmark(raw_args),
            "socklist" => self.cmd_socklist(raw_args),
            "sockrename" => {
                let expanded = self.expand(raw_args);
                let mut toks = expanded.split_whitespace();
                if let (Some(name), Some(newname)) = (toks.next(), toks.next()) {
                    self.sockets.rename(name, newname);
                }
            }
            "sockpause" => {
                let expanded = self.expand(raw_args);
                let resume = expanded
                    .split_whitespace()
                    .take_while(|t| t.starts_with('-'))
                    .any(|t| t.contains('r'));
                if let Some(name) = expanded.split_whitespace().find(|t| !t.starts_with('-')) {
                    self.sockets.pause(name, resume);
                }
            }
            "sockread" => self.cmd_sockread(raw_args),
            "dialog" => self.cmd_dialog(raw_args),
            "did" => self.cmd_did(raw_args),
            "nickicon" => {
                // /nickicon <nick> [icon]  — empty icon clears it.
                let expanded = self.expand(raw_args.trim());
                let mut it = expanded.splitn(2, char::is_whitespace);
                let nick = it.next().unwrap_or("").to_string();
                let icon = it.next().unwrap_or("").trim().to_string();
                if !nick.is_empty() {
                    self.actions.push(Action::NickIcon { nick, icon });
                }
            }
            "hadd" => self.cmd_hadd(raw_args),
            "hdel" => self.cmd_hdel(raw_args),
            "hmake" => self.cmd_hmake(raw_args),
            "hfree" => self.cmd_hfree(raw_args),
            "hclear" => self.cmd_hclear(raw_args),
            "hinc" => self.cmd_hincdec(raw_args, 1),
            "hdec" => self.cmd_hincdec(raw_args, -1),
            "hsave" => self.cmd_hsave(raw_args),
            "hload" => self.cmd_hload(raw_args),
            "tokenize" => self.cmd_tokenize(raw_args),
            // /noop evaluates its parameters (for identifier side effects) and
            // does nothing else.
            "noop" => {
                let _ = self.expand(raw_args);
            }
            "amsg" => self.cmd_amsg(raw_args, false),
            "ame" => self.cmd_amsg(raw_args, true),
            "ban" => self.cmd_ban(raw_args, true),
            "unban" => self.cmd_ban(raw_args, false),
            "query" => self.cmd_query(raw_args),
            "timers" => self.cmd_timers(raw_args),
            s if s.starts_with("timer") => {
                let name = s.strip_prefix("timer").unwrap_or("").to_string();
                self.cmd_timer(&name, raw_args);
            }
            "halt" | "haltdef" => {
                self.halted = true;
            }
            "return" => {
                self.ret = Some(self.expand(raw_args));
                self.halted = true;
            }
            // Client-side commands we don't (yet) implement but which must NOT be
            // sent to the server as raw IRC (that produces "421 Unknown command").
            "ialfill" => {
                // /ialfill [network] <#channel> — populate the IAL by WHOing the
                // channel; each WHO reply records that member's address.
                let s = self.expand(raw_args);
                if let Some(chan) = s.split_whitespace().rev().find(|t| t.starts_with('#')) {
                    self.actions.push(Action::Send(format!("WHO {chan}")));
                }
            }
            // We evaluate any parameters (for identifier side effects) and stop.
            // `/run` is deliberately a no-op — jIRC never launches programs.
            // `/ial`/`/ialclear`/`/ialmark` are recognised here so they aren't sent
            // to the server as raw commands; mutating the live IAL needs a
            // connection-control channel that isn't built yet.
            "clearall" | "close" | "sline" | "cline" | "fline" | "renwin"
            | "titlebar" | "editbox" | "linesep"
            | "background" | "color" | "font" | "flash" | "beep" | "ebeeps" | "speak" | "splay"
            | "play" | "sound" | "run" | "url" | "dns" | "debug" | "log" | "logview"
            | "timestamp" | "donotdisturb" | "toolbar" | "menubar" | "switchbar" | "treebar"
            | "mdi" | "save" | "loadbuf"
            | "savebuf" | "filter" | "showmirc" | "maximize" | "minimize"
            | "ial" | "ialclear" | "ialmark"
            | "creq" | "sreq" | "clipboard" | "resetidle" => {
                let _ = self.expand(raw_args);
            }
            _ => {
                // A user-defined alias? (skipped when its `#group` is disabled)
                if let Some(alias) = self.script.find_active_alias(&lname, self.vars) {
                    let body = alias.body.clone();
                    let params = split_params(&self.expand(raw_args));
                    // Invoked via the command syntax (/alias) — flag for $caller.
                    let saved = self.caller;
                    self.caller = "command";
                    self.call_alias(&body, params);
                    self.caller = saved;
                } else {
                    // Fall back to a raw IRC command.
                    let args = self.expand(raw_args);
                    let line = if args.is_empty() {
                        name.to_ascii_uppercase()
                    } else {
                        format!("{} {}", name.to_ascii_uppercase(), args)
                    };
                    self.actions.push(Action::Send(line));
                }
            }
        }
    }

    fn send_privmsg(&mut self, target: &str, text: &str) {
        self.actions.push(Action::Send(format!("PRIVMSG {target} :{text}")));
    }

    /// `/nickserv`, `/chanserv`, `/memoserv` (and `/ns`, `/cs`, `/ms`) — send a
    /// PRIVMSG to the named service.
    fn send_service(&mut self, service: &str, raw: &str) {
        let msg = self.expand(raw);
        if !msg.is_empty() {
            self.actions.push(Action::Send(format!("PRIVMSG {service} :{msg}")));
        }
    }

    /// Runs an alias body with `params` as `$1..`, isolating `$1..`, the halt
    /// flag, and the return value from the caller. Returns the `/return` value
    /// (empty if none). A bare `/halt` still propagates to stop the caller.
    pub fn call_alias(&mut self, body: &[Stmt], params: Vec<String>) -> String {
        let saved_params = std::mem::replace(&mut self.event.params, params);
        let saved_halted = std::mem::replace(&mut self.halted, false);
        let saved_ret = self.ret.take();
        let saved_goto = self.goto.take(); // goto is routine-local
        self.run(body);
        self.goto = saved_goto;
        let returned = self.ret.is_some();
        let result = self.ret.take().unwrap_or_default();
        let halted_in_alias = self.halted;
        self.event.params = saved_params;
        self.ret = saved_ret;
        // Restore the caller's halt state, but let a non-return /halt bubble up.
        self.halted = saved_halted || (halted_in_alias && !returned);
        result
    }

    fn reply_target(&self) -> String {
        if !self.event.target.is_empty() {
            self.event.target.clone()
        } else {
            self.event.chan.clone()
        }
    }

    /// Splits `raw_args` into (expanded target, expanded remaining text).
    fn split_target(&mut self, raw_args: &str) -> (String, String) {
        let raw = raw_args.trim();
        match raw.split_once(char::is_whitespace) {
            Some((t, rest)) => (self.expand(t), self.expand(rest.trim())),
            None => (self.expand(raw), String::new()),
        }
    }

    fn first_token<'b>(&self, raw: &'b str) -> &'b str {
        raw.trim().split_whitespace().next().unwrap_or("")
    }

    fn cmd_echo(&mut self, raw: &str) {
        let raw = raw.trim();
        let mut rest = raw;
        let mut target = self.reply_target();
        // Skip a leading switch like -a / -s / -ti.
        if rest.starts_with('-') {
            if let Some((_, after)) = rest.split_once(char::is_whitespace) {
                rest = after.trim();
            } else {
                rest = "";
            }
            target = STATUS.to_string();
        }
        // An explicit channel/nick target.
        if let Some((maybe_target, after)) = rest.split_once(char::is_whitespace) {
            if maybe_target.starts_with('#') {
                target = maybe_target.to_string();
                rest = after.trim();
            }
        }
        if target.is_empty() {
            target = STATUS.to_string();
        }
        let text = self.expand(rest);
        self.actions.push(Action::Echo { target, text });
    }

    /// `/enable <#group ...>` / `/disable <#group ...>` — toggle one or more
    /// script groups on/off. Names may be wildcards (`#help*`, or `#*` for all);
    /// a leading `#` is optional. The state is stored under a reserved `%var`.
    fn cmd_set_group(&mut self, raw: &str, on: bool) {
        let expanded = self.expand(raw);
        let patterns: Vec<String> = expanded.split_whitespace().map(String::from).collect();
        if patterns.is_empty() {
            return;
        }
        // Resolve matching group names first so we don't hold a `self.script`
        // borrow across the `self.vars` mutation.
        let names: Vec<String> = self
            .script
            .groups
            .iter()
            .filter(|(name, _)| {
                patterns
                    .iter()
                    .any(|p| wildcard_match(p.trim_start_matches('#'), name))
            })
            .map(|(name, _)| name.clone())
            .collect();
        let val = if on { "1" } else { "0" };
        for name in names {
            self.vars.insert(group_var_key(&name), val.to_string());
        }
    }

    /// `/groups [-e|-d]` — list script groups (all, or only enabled `-e` /
    /// disabled `-d`) in the active window.
    fn cmd_groups(&mut self, raw: &str) {
        let flag = raw.split_whitespace().next().unwrap_or("");
        let only_enabled = flag.eq_ignore_ascii_case("-e");
        let only_disabled = flag.eq_ignore_ascii_case("-d");
        let target = self.reply_target();
        let names: Vec<String> = self.script.groups.iter().map(|(n, _)| n.clone()).collect();
        for name in names {
            let on = self.script.group_enabled(self.vars, &Some(name.clone()));
            if (only_enabled && !on) || (only_disabled && on) {
                continue;
            }
            let text = format!("#{} ({})", name, if on { "on" } else { "off" });
            self.actions.push(Action::Echo {
                target: target.clone(),
                text,
            });
        }
    }

    /// `/anick` / `/mnick` / `/fullname` — update a stored identity field. The
    /// value is expanded (identifiers/variables resolve); empty values are ignored.
    fn set_identity(&mut self, field: &str, raw_args: &str) {
        let value = self.expand(raw_args).trim().to_string();
        if !value.is_empty() {
            self.actions.push(Action::SetIdentity {
                field: field.to_string(),
                value,
            });
        }
    }

    /// `/set [-switches] %var value` and `/var [-switches] %var = value`.
    /// `is_var` selects mIRC's `/var` form: `=` assignment and comma-separated
    /// declarations (`/var %a = 1, %b, %c = $me`). `/set` takes the rest of the
    /// line as the value verbatim (no `=`, no comma splitting). Timing switches
    /// like `-u30`/`-z` are accepted but not timed; the value is still set.
    fn cmd_set(&mut self, raw: &str, is_var: bool) {
        let (_flags, rest) = split_switches(raw);
        if is_var {
            for decl in split_top_commas(rest) {
                let decl = decl.trim();
                if decl.is_empty() {
                    continue;
                }
                let (name, value) = match decl.split_once('=') {
                    Some((n, v)) => (n.trim(), self.expand(v.trim())),
                    None => (decl, String::new()),
                };
                let key = name.trim_start_matches('%').trim().to_string();
                if !key.is_empty() {
                    self.vars.insert(key, value);
                }
            }
        } else if let Some((name, value)) = rest.split_once(char::is_whitespace) {
            let key = name.trim_start_matches('%').to_string();
            let value = self.expand(value.trim());
            self.vars.insert(key, value);
        } else if !rest.is_empty() {
            self.vars.insert(rest.trim_start_matches('%').to_string(), String::new());
        }
    }

    /// `/unset [-sgl] <%var> [%var2 ...]` — remove one or more variables;
    /// names may be wildcards (`/unset %prefix.*`).
    fn cmd_unset(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        for tok in rest.split_whitespace() {
            // /unset takes literal variable names/patterns — don't value-expand
            // them (so `%i7f.*` stays a wildcard rather than becoming its value).
            let pat = tok.trim_start_matches('%');
            if pat.is_empty() {
                continue;
            }
            if pat.contains('*') || pat.contains('?') {
                let keys: Vec<String> =
                    self.vars.keys().filter(|k| wildcard_match(pat, k)).cloned().collect();
                for k in keys {
                    self.vars.remove(&k);
                }
            } else {
                self.vars.remove(pat);
            }
        }
    }

    fn cmd_incdec(&mut self, raw: &str, sign: i64) {
        let (_flags, rest) = split_switches(raw);
        let mut it = rest.split_whitespace();
        let Some(name) = it.next() else { return };
        let key = name.trim_start_matches('%').to_string();
        let by: i64 = it
            .next()
            .map(|s| self.expand(s))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let cur: i64 = self.vars.get(&key).and_then(|v| v.parse().ok()).unwrap_or(0);
        self.vars.insert(key, (cur + sign * by).to_string());
    }

    /// `/write [-c] <file> [text]` — appends `text` as a new line in `file`
    /// (sandboxed to the data dir). `-c` clears/creates the file first.
    fn cmd_write(&mut self, raw: &str) {
        let mut rest = raw.trim();
        let mut clear = false;
        while rest.starts_with('-') {
            let (sw, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if sw.contains('c') {
                clear = true;
            }
            rest = more.trim();
        }
        let (file, text) = match rest.split_once(char::is_whitespace) {
            Some((f, t)) => (self.expand(f), self.expand(t.trim())),
            None => (self.expand(rest), String::new()),
        };
        if file.is_empty() {
            return;
        }
        let path = sandbox_path(&self.data_dir, &file);
        if clear {
            let _ = std::fs::write(&path, "");
        }
        if !text.is_empty() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{text}");
            }
        }
    }

    /// `/writeini [-n] <file> <section> <item> <value>` — set an INI item.
    fn cmd_writeini(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        while rest.starts_with('-') {
            rest = rest.split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or("").trim();
        }
        let mut parts = rest.splitn(4, char::is_whitespace);
        if let (Some(file), Some(section), Some(item), Some(value)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        {
            let path = sandbox_path(&self.data_dir, file);
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::write(&path, super::ini::set(&text, section, item, value));
        }
    }

    /// `/remini <file> <section> [item]` — remove an INI item, or a whole section.
    fn cmd_remini(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut parts = expanded.split_whitespace();
        if let (Some(file), Some(section)) = (parts.next(), parts.next()) {
            let item = parts.next();
            let path = sandbox_path(&self.data_dir, file);
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::write(&path, super::ini::remove(&text, section, item));
        }
    }

    /// `/fopen [-nox] <name> <filename>` — open a file with a named handle.
    fn cmd_fopen(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let (mut create_new, mut overwrite) = (false, false);
        while let Some(stripped) = rest.strip_prefix('-') {
            let (sw, more) = stripped.split_once(char::is_whitespace).unwrap_or((stripped, ""));
            if sw.contains('n') {
                create_new = true;
            }
            if sw.contains('o') {
                overwrite = true;
            }
            // -x (exclusive) is accepted but a no-op: we re-open per operation.
            rest = more.trim();
        }
        let mut parts = rest.splitn(2, char::is_whitespace);
        if let (Some(name), Some(file)) = (parts.next(), parts.next()) {
            let path = sandbox_path(&self.data_dir, file.trim());
            self.files.open(name, path, create_new, overwrite);
        }
    }

    /// `/fwrite [-bn] <name> <text>` — write at the pointer; `-n` appends a `$crlf`.
    fn cmd_fwrite(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let mut newline = false;
        while let Some(stripped) = rest.strip_prefix('-') {
            let (sw, more) = stripped.split_once(char::is_whitespace).unwrap_or((stripped, ""));
            if sw.contains('n') {
                newline = true;
            }
            // -b (binary variable) is accepted but treated as text.
            rest = more.trim();
        }
        let mut parts = rest.splitn(2, char::is_whitespace);
        if let Some(name) = parts.next() {
            let text = parts.next().unwrap_or("");
            self.files.write(name, text.as_bytes(), newline);
        }
    }

    /// `/fclose <name | wildcard>` — close one or more file handles.
    fn cmd_fclose(&mut self, raw: &str) {
        let name = self.expand(raw);
        let name = name.trim();
        if !name.is_empty() {
            self.files.close(name);
        }
    }

    /// `/fseek [-lnpwr] <name> [position]` — move the file pointer.
    fn cmd_fseek(&mut self, raw: &str) {
        use super::files::SeekMode;
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let mut sw = "";
        if let Some(stripped) = rest.strip_prefix('-') {
            let (flags, more) = stripped.split_once(char::is_whitespace).unwrap_or((stripped, ""));
            sw = flags;
            rest = more.trim();
        }
        let mut parts = rest.splitn(2, char::is_whitespace);
        let Some(name) = parts.next() else {
            return;
        };
        let arg = parts.next().unwrap_or("").trim();
        let mode = if sw.contains('l') {
            SeekMode::Line(arg.parse().unwrap_or(0))
        } else if sw.contains('n') {
            SeekMode::Next
        } else if sw.contains('p') {
            SeekMode::Prev
        } else if sw.contains('w') {
            SeekMode::Wild(arg.to_string())
        } else if sw.contains('r') {
            SeekMode::Regex(arg.to_string())
        } else {
            SeekMode::Byte(arg.parse().unwrap_or(0))
        };
        self.files.seek(name, mode);
    }

    /// `/bset [-tacz] <&binvar> <N> <value…>` — write bytes at 1-based position N
    /// (`-t` = the values are plain text, `-z` = empty the var first).
    fn cmd_bset(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let (mut text, mut zero) = (false, false);
        while let Some(stripped) = rest.strip_prefix('-') {
            let (sw, more) = stripped.split_once(char::is_whitespace).unwrap_or((stripped, ""));
            if sw.contains('t') {
                text = true;
            }
            if sw.contains('z') {
                zero = true;
            }
            // -a (no UTF-8) / -c (chop) accepted but not specially handled.
            rest = more.trim();
        }
        let mut parts = rest.splitn(3, char::is_whitespace);
        let (Some(name), Some(npart)) = (parts.next(), parts.next()) else {
            return;
        };
        let pos: i64 = npart.trim().parse().unwrap_or(1);
        let valstr = parts.next().unwrap_or("");
        let bytes: Vec<u8> = if text {
            valstr.as_bytes().to_vec()
        } else {
            valstr
                .split_whitespace()
                .filter_map(|t| t.parse::<u16>().ok())
                .map(|n| n as u8)
                .collect()
        };
        self.bins.set(name, pos, &bytes, zero);
    }

    /// `/bunset <&binvar> [&binvar…]` — unset binary variables.
    fn cmd_bunset(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        for name in expanded.split_whitespace() {
            self.bins.unset(name);
        }
    }

    /// `/bcopy <&dest> <N> <&source> <S> <M>` — copy M bytes from &source position S
    /// to &dest position N (1-based positions).
    fn cmd_bcopy(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let p: Vec<&str> = expanded.split_whitespace().collect();
        if p.len() < 5 {
            return;
        }
        let (dest, src) = (p[0], p[2]);
        let n: i64 = p[1].trim().parse().unwrap_or(1);
        let s: usize = p[3].trim().parse().unwrap_or(1);
        let m: usize = p[4].trim().parse().unwrap_or(0);
        let slice: Vec<u8> = self
            .bins
            .get(src)
            .map(|b| b.iter().skip(s.saturating_sub(1)).take(m).copied().collect())
            .unwrap_or_default();
        self.bins.set(dest, n, &slice, false);
    }

    /// `/breplace <&binvar> <old> <new> [<old> <new>…]` — replace matching byte
    /// values throughout &binvar.
    fn cmd_breplace(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut parts = expanded.split_whitespace();
        let Some(name) = parts.next() else { return };
        let nums: Vec<u8> = parts.filter_map(|t| t.parse::<u16>().ok()).map(|n| n as u8).collect();
        let pairs: Vec<(u8, u8)> =
            nums.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0], c[1])).collect();
        if pairs.is_empty() {
            return;
        }
        let Some(mut bytes) = self.bins.get(name).cloned() else { return };
        for b in bytes.iter_mut() {
            for (old, new) in &pairs {
                if *b == *old {
                    *b = *new;
                    break;
                }
            }
        }
        self.bins.set(name, 1, &bytes, false);
    }

    /// `/btrunc <file> <bytes>` — truncate or zero-extend a file to `bytes` long.
    fn cmd_btrunc(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut parts = expanded.splitn(2, char::is_whitespace);
        let (Some(file), Some(len)) = (parts.next(), parts.next()) else { return };
        let path = sandbox_path(&self.data_dir, file.trim());
        let len: u64 = len.trim().parse().unwrap_or(0);
        if let Ok(f) = std::fs::OpenOptions::new().write(true).create(true).open(&path) {
            let _ = f.set_len(len);
        }
    }

    /// `/bread <file> <S> <N> <&binvar>` — read N bytes from `file` at position S
    /// (1-based) into &binvar, replacing it.
    fn cmd_bread(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let p: Vec<&str> = expanded.split_whitespace().collect();
        if p.len() < 4 {
            return;
        }
        let path = sandbox_path(&self.data_dir, p[0]);
        let s: usize = p[1].trim().parse().unwrap_or(1);
        let n: usize = p[2].trim().parse().unwrap_or(0);
        let name = p[3];
        if let Ok(data) = std::fs::read(&path) {
            let slice: Vec<u8> = data.iter().skip(s.saturating_sub(1)).take(n).copied().collect();
            self.bins.unset(name);
            self.bins.set(name, 1, &slice, false);
        }
    }

    /// `/bwrite <file> <S> <N> <text|%var|&binvar>` — write N bytes (N<0 = all) of
    /// the data to `file` at position S (1-based), extending the file if needed.
    fn cmd_bwrite(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let p: Vec<&str> = expanded.splitn(4, char::is_whitespace).collect();
        if p.len() < 4 {
            return;
        }
        let path = sandbox_path(&self.data_dir, p[0]);
        let s: usize = p[1].trim().parse().unwrap_or(1);
        let n: i64 = p[2].trim().parse().unwrap_or(-1);
        let data_arg = p[3];
        // A known &binvar contributes its bytes; otherwise the literal text.
        let data: Vec<u8> = if data_arg.starts_with('&') && self.bins.get(data_arg).is_some() {
            self.bins.get(data_arg).cloned().unwrap_or_default()
        } else {
            data_arg.as_bytes().to_vec()
        };
        let to_write: Vec<u8> =
            if n < 0 { data } else { data.into_iter().take(n as usize).collect() };
        let mut content = std::fs::read(&path).unwrap_or_default();
        let start = s.saturating_sub(1);
        if content.len() < start {
            content.resize(start, 0);
        }
        for (i, b) in to_write.iter().enumerate() {
            let idx = start + i;
            if idx < content.len() {
                content[idx] = *b;
            } else {
                content.push(*b);
            }
        }
        let _ = std::fs::write(&path, &content);
    }

    /// `/window [-celp] @name [...]` — create a custom `@window` (`-c` closes,
    /// `-e` editbox, `-p` picture; default listbox).
    fn cmd_window(&mut self, raw: &str) {
        use super::window::WindowKind;
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let mut close = false;
        let mut kind = WindowKind::Listbox;
        while let Some(stripped) = rest.strip_prefix('-') {
            let (sw, more) = stripped.split_once(char::is_whitespace).unwrap_or((stripped, ""));
            if sw.contains('c') {
                close = true;
            }
            if sw.contains('e') {
                kind = WindowKind::Editbox;
            } else if sw.contains('p') {
                kind = WindowKind::Picture;
            }
            rest = more.trim();
        }
        let Some(name) = rest.split_whitespace().next() else {
            return;
        };
        if !name.starts_with('@') {
            return;
        }
        if close {
            self.windows.close(name);
            self.actions.push(Action::WindowClose { name: name.to_string() });
        } else {
            self.windows.open(name, kind, name);
            self.actions.push(Action::WindowOpen {
                name: name.to_string(),
                kind: kind.as_str().to_string(),
                title: name.to_string(),
            });
        }
    }

    /// `/aline @w text`, `/rline @w N text`, `/iline @w N text`, `/dline @w N`.
    fn cmd_window_line(&mut self, raw: &str, op: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        // Skip a leading switch (e.g. `/aline -p @w text` colour switch).
        if rest.starts_with('-') {
            rest = rest.split_once(char::is_whitespace).map(|(_, r)| r.trim()).unwrap_or("");
        }
        let mut parts = rest.splitn(2, char::is_whitespace);
        let Some(name) = parts.next() else {
            return;
        };
        if !name.starts_with('@') || !self.windows.exists(name) {
            return;
        }
        let arg = parts.next().unwrap_or("");
        let (n, text) = match op {
            "add" => {
                self.windows.aline(name, arg);
                (0u32, arg.to_string())
            }
            "delete" => {
                let n: u32 = arg.trim().parse().unwrap_or(0);
                self.windows.dline(name, n as usize);
                (n, String::new())
            }
            _ => {
                // replace / insert: <N> <text>
                let mut p2 = arg.splitn(2, char::is_whitespace);
                let n: u32 = p2.next().unwrap_or("").trim().parse().unwrap_or(0);
                let text = p2.next().unwrap_or("");
                if op == "replace" {
                    self.windows.rline(name, n as usize, text);
                } else {
                    self.windows.iline(name, n as usize, text);
                }
                (n, text.to_string())
            }
        };
        self.actions.push(Action::WindowLine {
            name: name.to_string(),
            op: op.to_string(),
            n,
            text,
        });
    }

    /// `/clear @window` — clear a custom window's lines (channel-buffer clear is
    /// a frontend concern, deferred).
    fn cmd_window_clear(&mut self, raw: &str) {
        let name = self.expand(raw);
        let name = name.trim();
        if name.starts_with('@') && self.windows.exists(name) {
            self.windows.clear(name);
            self.actions.push(Action::WindowLine {
                name: name.to_string(),
                op: "clear".to_string(),
                n: 0,
                text: String::new(),
            });
        }
    }

    /// `/sockopen [-e] <name> <host> <port>` — open a TCP socket; `-e` uses TLS.
    fn cmd_sockopen(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let tls = expanded
            .split_whitespace()
            .take_while(|t| t.starts_with('-'))
            .any(|t| t.contains('e'));
        let mut toks = expanded.split_whitespace().filter(|t| !t.starts_with('-'));
        if let (Some(name), Some(host), Some(port)) = (toks.next(), toks.next(), toks.next()) {
            if let Ok(port) = port.parse::<u16>() {
                self.actions.push(Action::SockOpen {
                    name: name.to_string(),
                    host: host.to_string(),
                    port,
                    tls,
                });
            }
        }
    }

    /// `/sockwrite [-n] <name> <text>` — send to a socket; `-n` appends CRLF.
    fn cmd_sockwrite(&mut self, raw: &str) {
        let mut rest = raw.trim();
        let mut newline = false;
        while rest.starts_with('-') {
            let (sw, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if sw.contains('n') {
                newline = true;
            }
            rest = more.trim();
        }
        let (name_tok, data_tok) = match rest.split_once(char::is_whitespace) {
            Some((n, t)) => (n, t.trim()),
            None => (rest, ""),
        };
        let name = self.expand(name_tok);
        if name.is_empty() {
            return;
        }
        // `/sockwrite name &binvar` sends the binary variable's bytes verbatim —
        // binary protocols build their packet in a &binvar (e.g. a crypto auth
        // response). Anything else is text, expanded as usual.
        let mut data = match data_tok.strip_prefix('&') {
            Some(bin) if !bin.is_empty() && !bin.contains(char::is_whitespace) => {
                self.bins.get(data_tok).cloned().unwrap_or_default()
            }
            _ => self.expand(data_tok).into_bytes(),
        };
        if newline {
            data.extend_from_slice(b"\r\n");
        }
        self.actions.push(Action::SockWrite { name, data });
    }

    /// `/socklisten [-options] <name> [port]` — bind a listening socket. With no
    /// (or 0) port the OS assigns one, readable via `$sock(name).port`.
    fn cmd_socklisten(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut toks = expanded.split_whitespace().filter(|t| !t.starts_with('-'));
        if let Some(name) = toks.next() {
            let name = name.to_string();
            let port = toks.next().and_then(|p| p.parse::<u16>().ok()).unwrap_or(0);
            // Bind now (so $sock(name).port is readable inline); the accept loop
            // is started at apply-time with the owning connection's context.
            self.sockets.listen(&name, port);
            self.actions.push(Action::SockListen { name });
        }
    }

    /// `/sockmark <name> [text]` — set (or clear) a socket's mark, read back via
    /// `$sock(name).mark`.
    fn cmd_sockmark(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let trimmed = expanded.trim();
        let (name, mark) = trimmed.split_once(char::is_whitespace).unwrap_or((trimmed, ""));
        if !name.is_empty() {
            self.sockets.set_mark(name, mark.trim());
        }
    }

    /// `/socklist [-tul] [name]` — echoes the list of open sockets.
    fn cmd_socklist(&mut self, raw: &str) {
        let filter = self.expand(raw);
        let target = self.reply_target();
        let lines = self.sockets.list(filter.trim());
        self.actions.push(Action::Echo {
            target: target.clone(),
            text: format!("Sock List - {} socket(s)", lines.len()),
        });
        for line in lines {
            self.actions.push(Action::Echo { target: target.clone(), text: line });
        }
    }

    /// `/sockread <%var>` — inside `on SOCKREAD`, copies the current line into
    /// `%var` and sets `$sockbr`. A second call in the same event reads empty
    /// (so `while ($sockbr)` loops terminate).
    fn cmd_sockread(&mut self, raw: &str) {
        let var = self.first_token(raw).trim_start_matches('%').to_string();
        if var.is_empty() {
            return;
        }
        let line = std::mem::take(&mut self.event.text);
        let br = line.len();
        self.vars.insert(var, line);
        self.vars.insert(SOCK_BR_KEY.to_string(), br.to_string());
    }

    /// `/dialog [-c] <name>` — open (or, with `-c`, close) a custom dialog.
    fn cmd_dialog(&mut self, raw: &str) {
        let toks: Vec<&str> = raw.split_whitespace().collect();
        let close = toks.iter().any(|t| *t == "-c");
        let Some(name) = toks.iter().find(|t| !t.starts_with('-')) else {
            return;
        };
        if close {
            self.actions.push(Action::DialogClose { name: name.to_string() });
        } else if let Some(d) = self.script.find_dialog(name) {
            self.actions.push(Action::DialogOpen {
                name: d.name.clone(),
                title: d.title.clone(),
                controls: d.controls.clone(),
            });
        }
    }

    /// `/did [-a|-r] <dialog> <control> [text]` — mutate a dialog control:
    /// `-a` add a list/combo item, `-r` clear it, default set its value.
    fn cmd_did(&mut self, raw: &str) {
        let mut rest = raw.trim();
        let mut op = "set";
        if rest.starts_with('-') {
            let (sw, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            op = match sw {
                "-a" => "add",
                "-r" | "-c" => "clear",
                _ => "set",
            };
            rest = more.trim();
        }
        let mut it = rest.splitn(3, char::is_whitespace);
        let (dialog, control) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if dialog.is_empty() || control.is_empty() {
            return;
        }
        let value = self.expand(it.next().unwrap_or("").trim());
        self.actions.push(Action::DialogSet {
            dialog: dialog.to_string(),
            control: control.to_string(),
            op: op.to_string(),
            value,
        });
    }

    /// `/hsave <table> <file>` — write a hash table to a sandboxed file.
    fn cmd_hsave(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut toks = expanded.split_whitespace();
        if let (Some(table), Some(file)) = (toks.next(), toks.next()) {
            let mut out = String::new();
            if let Some(h) = self.hashes.get(table) {
                for (item, value) in h {
                    out.push_str(&format!("{item} {value}\n"));
                }
            }
            let _ = std::fs::write(sandbox_path(&self.data_dir, file), out);
        }
    }

    /// `/hload <table> <file>` — load a hash table from a sandboxed file.
    fn cmd_hload(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut toks = expanded.split_whitespace();
        if let (Some(table), Some(file)) = (toks.next(), toks.next()) {
            if let Ok(content) = std::fs::read_to_string(sandbox_path(&self.data_dir, file)) {
                let h = self.hashes.entry(table.to_string()).or_default();
                for line in content.lines() {
                    if let Some((item, value)) = line.split_once(' ') {
                        h.insert(item.to_string(), value.to_string());
                    }
                }
            }
        }
    }

    /// `/hmake [-s] <name> [slots]` — create an (empty) hash table. Slots are a
    /// sizing hint in mIRC; ignored here.
    fn cmd_hmake(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        if let Some(table) = rest.split_whitespace().next() {
            let table = self.expand(table);
            self.hashes.entry(table).or_default();
        }
    }

    /// `/hfree [-w] <name>` — delete a hash table (`-w`: name is a wildcard).
    fn cmd_hfree(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let wild = flags.contains('w');
        if let Some(table) = rest.split_whitespace().next() {
            let table = self.expand(table);
            if wild {
                let keys: Vec<String> = self
                    .hashes
                    .keys()
                    .filter(|k| wildcard_match(&table, k))
                    .cloned()
                    .collect();
                for k in keys {
                    self.hashes.remove(&k);
                }
            } else {
                self.hashes.remove(&table);
            }
        }
    }

    /// `/hclear <name>` — remove every item but keep the (now empty) table.
    fn cmd_hclear(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        if let Some(table) = rest.split_whitespace().next() {
            let table = self.expand(table);
            if let Some(h) = self.hashes.get_mut(&table) {
                h.clear();
            }
        }
    }

    /// `/hadd [-m] <table> <item> [value]` — set an item (`-m` makes the table
    /// if it doesn't exist; we always create-on-demand). Table and item names
    /// are expanded so variable keys match what `$hget` reads back.
    fn cmd_hadd(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        let mut it = rest.splitn(3, char::is_whitespace);
        let (table, item, value) = (it.next(), it.next(), it.next());
        if let (Some(table), Some(item)) = (table, item) {
            let table = self.expand(table.trim());
            let item = self.expand(item.trim());
            let value = self.expand(value.unwrap_or("").trim());
            self.hashes.entry(table).or_default().insert(item, value);
        }
    }

    /// `/hdel [-w] <table> <item>` — remove an item (`-w`: item is a wildcard).
    fn cmd_hdel(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let wild = flags.contains('w');
        let mut it = rest.split_whitespace();
        if let (Some(table), Some(item)) = (it.next(), it.next()) {
            let table = self.expand(table);
            let item = self.expand(item);
            if let Some(h) = self.hashes.get_mut(&table) {
                if wild {
                    let keys: Vec<String> =
                        h.keys().filter(|k| wildcard_match(&item, k)).cloned().collect();
                    for k in keys {
                        h.remove(&k);
                    }
                } else {
                    h.remove(&item);
                }
            }
        }
    }

    /// `/hinc|/hdec [-switches] <table> <item> [n]` — add/subtract `n` (default
    /// 1) to a numeric hash item, creating the table/item if needed.
    fn cmd_hincdec(&mut self, raw: &str, sign: i64) {
        let (_flags, rest) = split_switches(raw);
        let mut it = rest.splitn(3, char::is_whitespace);
        if let (Some(table), Some(item)) = (it.next(), it.next()) {
            let table = self.expand(table.trim());
            let item = self.expand(item.trim());
            let by: i64 = it
                .next()
                .map(|s| self.expand(s.trim()))
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            let h = self.hashes.entry(table).or_default();
            let cur: i64 = h.get(&item).and_then(|v| v.parse().ok()).unwrap_or(0);
            h.insert(item, (cur + sign * by).to_string());
        }
    }

    /// `/tokenize <c> <text>` — split `text` by character code `c` into `$1, $2…`
    /// for the rest of the current routine. `c` of 32 (space) collapses runs.
    fn cmd_tokenize(&mut self, raw: &str) {
        let raw = raw.trim();
        let Some((c, rest)) = raw.split_once(char::is_whitespace) else {
            return;
        };
        let sep = self.expand(c).trim().parse::<u32>().ok().and_then(char::from_u32).unwrap_or(' ');
        let text = self.expand(rest.trim());
        self.event.params = if sep == ' ' {
            text.split_whitespace().map(String::from).collect()
        } else {
            text.split(sep).map(String::from).collect()
        };
    }

    /// `/amsg <text>` / `/ame <action>` — send to every channel you're on.
    fn cmd_amsg(&mut self, raw: &str, action: bool) {
        let text = self.expand(raw);
        if text.is_empty() {
            return;
        }
        let channels: Vec<String> = self.state.channels.iter().map(|c| c.name.clone()).collect();
        for chan in channels {
            let line = if action {
                format!("PRIVMSG {chan} :\u{1}ACTION {text}\u{1}")
            } else {
                format!("PRIVMSG {chan} :{text}")
            };
            self.actions.push(Action::Send(line));
        }
    }

    /// `/query <nick> [message]` — open a query; if a message is given, send it
    /// (which opens the query window on the echo). Without a message this is a
    /// no-op rather than a stray QUERY line to the server.
    fn cmd_query(&mut self, raw: &str) {
        let (target, text) = self.split_target(raw);
        if !target.is_empty() && !text.is_empty() {
            self.send_privmsg(&target, &text);
        }
    }

    /// `/ban [-switches] [#channel] <nick|address> [type]` — set (or, when
    /// `add` is false for `/unban`, remove) a channel ban. A bare nick known in
    /// the IAL is converted to a masked address of the given `type` (default 2).
    fn cmd_ban(&mut self, raw: &str, add: bool) {
        let (_flags, rest) = split_switches(raw);
        let toks: Vec<String> = rest.split_whitespace().map(|t| self.expand(t)).collect();
        if toks.is_empty() {
            return;
        }
        // Optional leading channel; otherwise the current event channel.
        let (chan, idx) = if super::is_channel(&toks[0]) {
            (toks[0].clone(), 1)
        } else {
            (self.event.chan.clone(), 0)
        };
        let Some(target) = toks.get(idx) else { return };
        if chan.is_empty() {
            return;
        }
        // Resolve a bare nick to a hostmask via the IAL when possible.
        let mask = if target.contains('!') || target.contains('@') || target.contains('*') {
            target.clone()
        } else {
            let kind: u32 = toks.get(idx + 1).and_then(|s| s.parse().ok()).unwrap_or(2);
            let who = target.to_lowercase();
            match self.state.ial.iter().find(|(n, _)| *n == who) {
                Some((_, full)) => ident::mask_address(full, kind),
                None => format!("{target}!*@*"),
            }
        };
        let sign = if add { '+' } else { '-' };
        self.actions.push(Action::Send(format!("MODE {chan} {sign}b {mask}")));
    }

    /// `/timer[name] <reps> <interval-secs> <command>` — schedules a command.
    /// `reps` of 0 means repeat (capped); the command is evaluated when it fires.
    /// `/timer[name] off` stops the timer (empty name stops all).
    fn cmd_timer(&mut self, name: &str, raw: &str) {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("off") {
            self.actions.push(Action::TimerStop {
                name: if name.is_empty() { "*".to_string() } else { name.to_string() },
            });
            return;
        }
        let mut parts = raw.splitn(3, char::is_whitespace);
        let reps_tok = parts.next().unwrap_or("");
        let interval_tok = parts.next().unwrap_or("");
        let command = parts.next().unwrap_or("").trim().to_string();
        if command.is_empty() {
            return;
        }
        let reps: u32 = self.expand(reps_tok).trim().parse().unwrap_or(1);
        let reps = if reps == 0 { 1000 } else { reps.min(100_000) };
        let secs: f64 = self.expand(interval_tok).trim().parse().unwrap_or(0.0);
        let interval_ms = (secs * 1000.0).max(0.0) as u64;
        self.actions.push(Action::Timer {
            name: name.to_string(),
            reps,
            interval_ms,
            command,
            target: self.reply_target(),
        });
    }

    /// `/timers` lists active timers; `/timers off` stops them all.
    fn cmd_timers(&mut self, raw: &str) {
        if raw.trim().eq_ignore_ascii_case("off") {
            self.actions.push(Action::TimerStop { name: "*".to_string() });
        } else {
            self.actions.push(Action::TimerList { target: self.reply_target() });
        }
    }

    // ---- expansion ----

    /// Expands `%vars`, `$identifiers`, params, and the `$+` join operator.
    pub fn expand(&mut self, text: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut join_next = false;
        // Split on spaces, but keep `$ident(a b c)` whole (spaces inside the
        // parentheses are part of the identifier's arguments).
        for tok in split_top_level(text) {
            if tok == "$+" {
                join_next = true;
                continue;
            }
            let v = self.eval_token(&tok);
            if join_next {
                if let Some(last) = parts.last_mut() {
                    last.push_str(&v);
                } else {
                    parts.push(v);
                }
                join_next = false;
            } else {
                parts.push(v);
            }
        }
        parts.join(" ")
    }

    /// Expands identifiers/vars within a single (space-free) token.
    fn eval_token(&mut self, tok: &str) -> String {
        // A lone `#` is the current channel (mIRC); `#name` stays a literal channel.
        if tok == "#" {
            return self.event.chan.clone();
        }
        let chars: Vec<char> = tok.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '%' => {
                    i += 1;
                    let name = read_var_name(&chars, &mut i);
                    if !name.is_empty() {
                        out.push_str(self.vars.get(&name).map(|s| s.as_str()).unwrap_or(""));
                    } else {
                        out.push('%');
                    }
                }
                '$' => {
                    i += 1;
                    out.push_str(&self.eval_dollar(&chars, &mut i));
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }

    fn eval_dollar(&mut self, chars: &[char], i: &mut usize) -> String {
        // $+(a,b,c) — concatenate args with no separator. This is the function
        // form, distinct from the `$+` join operator (handled in `expand`).
        if chars.get(*i) == Some(&'+') {
            *i += 1;
            if chars.get(*i) == Some(&'(') {
                let inner = read_balanced(chars, i);
                return split_args(&inner).iter().map(|a| self.expand(a)).collect();
            }
            return "+".to_string();
        }
        // `$$N` — a require prefix: like `$N`, but the script halts when the
        // parameter is empty. Only when a digit follows (a literal `$$`
        // elsewhere is left untouched).
        let require = chars.get(*i) == Some(&'$')
            && matches!(chars.get(*i + 1), Some(c) if c.is_ascii_digit());
        if require {
            *i += 1;
        }
        // Numeric param: $1 (single), $2- (to end), $2-4 (range), $0 (count).
        if matches!(chars.get(*i), Some(c) if c.is_ascii_digit()) {
            let start = read_number(chars, i);
            let end = if chars.get(*i) == Some(&'-') {
                *i += 1;
                if matches!(chars.get(*i), Some(c) if c.is_ascii_digit()) {
                    Some(read_number(chars, i)) // $N-M
                } else {
                    None // $N- (to end)
                }
            } else {
                Some(start) // $N (single)
            };
            let val = self.params_range(start, end);
            if require && val.is_empty() {
                self.halted = true;
            }
            return val;
        }
        // Identifier name.
        let name = read_name(chars, i);
        if name.is_empty() {
            return "$".to_string();
        }
        // $regsubex evaluates its subtext once per match, so its args must NOT be
        // pre-expanded here — hand the raw args to a dedicated handler.
        if name.eq_ignore_ascii_case("regsubex") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            return ident::eval_regsubex(self, &split_args(&inner));
        }
        // Optional (args).
        let (args, had_parens) = if chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            (
                split_args(&inner).into_iter().map(|a| self.expand(&a)).collect::<Vec<_>>(),
                true,
            )
        } else {
            (Vec::new(), false)
        };
        // Optional `.property` suffix — only after `(args)`, matching mIRC's
        // `$sock(x).port` / `$hget(t,N).item`. Restricting it to the
        // parenthesised form avoids swallowing a literal `.word` after a bare
        // identifier (e.g. `$nick.example`).
        let prop = if had_parens && chars.get(*i) == Some(&'.') {
            let mut j = *i + 1;
            let p = read_name(chars, &mut j);
            if p.is_empty() {
                String::new()
            } else {
                *i = j;
                p
            }
        } else {
            String::new()
        };
        ident::eval_ident(self, &name, &args, &prop)
    }

    /// Resolves a parameter spec: `$N` (`end = Some(N)`), `$N-` (`end = None`,
    /// to the last param) or `$N-M` (`end = Some(M)`, inclusive). `$0` returns
    /// the parameter count. Indices are 1-based; out-of-range yields "".
    fn params_range(&self, start: usize, end: Option<usize>) -> String {
        if start == 0 {
            return self.event.params.len().to_string();
        }
        let params = &self.event.params;
        let lo = start - 1;
        if lo >= params.len() {
            return String::new();
        }
        let hi = match end {
            None => params.len(),
            Some(e) => e.min(params.len()),
        };
        if hi <= lo {
            return String::new();
        }
        params[lo..hi].join(" ")
    }

    // ---- conditions ----

    fn eval_cond(&mut self, cond: &str) -> bool {
        let expanded = self.expand(cond);
        // Clone the Arc (cheap) so the leaf resolver can read channel state
        // without borrowing `self` across the evaluation.
        let state = self.state.clone();
        eval_bool_with(&expanded, &|term| state_op(&state, term))
    }
}

/// Reads consecutive ASCII digits as a number (0 if none / on overflow).
fn read_number(chars: &[char], i: &mut usize) -> usize {
    let mut num = String::new();
    while matches!(chars.get(*i), Some(c) if c.is_ascii_digit()) {
        num.push(chars[*i]);
        *i += 1;
    }
    num.parse().unwrap_or(0)
}

fn read_name(chars: &[char], i: &mut usize) -> String {
    let mut name = String::new();
    while let Some(&c) = chars.get(*i) {
        if c.is_alphanumeric() || c == '_' {
            name.push(c);
            *i += 1;
        } else {
            break;
        }
    }
    name
}

/// Reads a `%variable` name. Unlike identifier names, mIRC variable names may
/// contain dots (e.g. `%i7f.chan`, `%a.b.c`), so `.` is part of the name — but
/// a trailing dot is treated as punctuation (e.g. "joined %chan.").
fn read_var_name(chars: &[char], i: &mut usize) -> String {
    let mut name = String::new();
    while let Some(&c) = chars.get(*i) {
        if c.is_alphanumeric() || c == '_' || c == '.' {
            name.push(c);
            *i += 1;
        } else {
            break;
        }
    }
    while name.ends_with('.') {
        name.pop();
        *i -= 1;
    }
    name
}

/// Reads a balanced `(...)`; cursor must be on `(`. Returns inner text.
fn read_balanced(chars: &[char], i: &mut usize) -> String {
    let mut depth = 0;
    let mut out = String::new();
    while let Some(&c) = chars.get(*i) {
        *i += 1;
        match c {
            '(' => {
                depth += 1;
                if depth > 1 {
                    out.push(c);
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Splits identifier arguments on top-level commas. Each arg is trimmed (mIRC
/// tolerates spaces around commas, and much of the engine relies on it), EXCEPT
/// a whitespace-only arg is kept intact so a deliberate single space survives:
/// `$asc(" ")` is 32, which byte-list builders like
/// `$regsubex(text,/(.)/g,$asc(\1) $+ $chr(32))` depend on. Empty input = no args.
fn split_args(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut raw: Vec<String> = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => raw.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    raw.push(cur);
    raw.into_iter()
        .map(|seg| {
            let t = seg.trim();
            if t.is_empty() && !seg.is_empty() {
                seg // whitespace-only: keep so `$asc(" ")` stays a space
            } else {
                t.to_string()
            }
        })
        .collect()
}

fn split_params(s: &str) -> Vec<String> {
    s.split_whitespace().map(|x| x.to_string()).collect()
}

/// Splits on top-level commas (depth 0), keeping `$id(a,b)` argument commas
/// intact. Used by `/var %a = 1, %b = $iif(x,y,z)`.
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Finds the index of a `:label` within a body (case-insensitive).
fn find_label(body: &[Stmt], name: &str) -> Option<usize> {
    body.iter()
        .position(|s| matches!(s, Stmt::Label(l) if l.eq_ignore_ascii_case(name)))
}

/// Resolves a script-supplied filename to a path inside the sandbox `dir`,
/// using only the final filename component so scripts can't escape the dir.
pub fn sandbox_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let file = std::path::Path::new(name)
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("script.dat"));
    dir.join(file)
}

/// Splits text on spaces at parenthesis depth 0, so `$ident(a b c)` (whose
/// arguments may contain spaces) stays a single token for expansion.
fn split_top_level(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            // Only a `$id(`/`id(`/`$+(` paren groups arguments; a bare `(` is
            // literal, so `$+` and spaces around plain parens still work.
            '(' if depth > 0
                || cur.chars().last().is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '+') =>
            {
                depth += 1;
                cur.push(c);
            }
            ')' if depth > 0 => {
                depth -= 1;
                cur.push(c);
            }
            ' ' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// ---- boolean / comparison evaluation ----

/// Public wrapper so identifiers like `$iif` can evaluate conditions.
pub fn eval_bool_public(s: &str) -> bool {
    eval_bool(s)
}

fn eval_bool(s: &str) -> bool {
    eval_bool_with(s, &|_| None)
}

/// Boolean evaluator with an optional stateful leaf resolver. Each leaf term
/// (after `||`/`&&` splitting, paren and `!` stripping) is offered to `leaf`
/// first; `Some(b)` overrides the built-in comparison. This is how the
/// state-aware operators (`isop`, `ison`, `ischan`, …) — which the pure
/// comparator can't evaluate — are resolved against the channel snapshot.
fn eval_bool_with(s: &str, leaf: &dyn Fn(&str) -> Option<bool>) -> bool {
    let s = s.trim();
    if let Some(idx) = find_top(s, "||") {
        return eval_bool_with(&s[..idx], leaf) || eval_bool_with(&s[idx + 2..], leaf);
    }
    if let Some(idx) = find_top(s, "&&") {
        return eval_bool_with(&s[..idx], leaf) && eval_bool_with(&s[idx + 2..], leaf);
    }
    eval_term_with(s, leaf)
}

/// Finds a top-level (paren-depth 0) occurrence of `op`.
fn find_top(s: &str, op: &str) -> Option<usize> {
    let bytes: Vec<char> = s.chars().collect();
    let opc: Vec<char> = op.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && bytes[i..].starts_with(opc.as_slice()) {
            return Some(s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(i));
        }
        i += 1;
    }
    None
}

fn eval_term_with(s: &str, leaf: &dyn Fn(&str) -> Option<bool>) -> bool {
    let s = s.trim();
    // Leading `!` negation: if (!%x), if (!$ident), if (!(a == b)).
    if let Some(rest) = s.strip_prefix('!') {
        // Don't mistake the `!=` operator for a negation prefix.
        if !rest.starts_with('=') {
            let rest = rest.trim();
            // `!(expr)` or a multi-token expression negates the evaluated boolean.
            // A bare `!operand` negates the operand's *truthiness* — mIRC negates
            // the value, it does not re-parse a bare value as a comparison. So
            // `if (!$2)` where $2 is data containing `<`/`=`/`>` stays an emptiness
            // test instead of being misread as `a < b` (which would pick the wrong
            // branch). Mirrors the multi-word `!= $null` handling below.
            return if rest.starts_with('(') || rest.contains(char::is_whitespace) {
                !eval_term_with(rest, leaf)
            } else {
                !truthy(rest)
            };
        }
    }
    // A fully-parenthesised term: re-evaluate its contents so nested grouping
    // and `!`/`&&`/`||` keep working (e.g. `(a||b) && c`, `(!nick isop #)`).
    if is_fully_parenthesised(s) {
        return eval_bool_with(&s[1..s.len() - 1], leaf);
    }
    // State-aware operators (isop/ison/ischan/...) get first crack at the term.
    if let Some(b) = leaf(s) {
        return b;
    }
    let toks: Vec<&str> = s.split_whitespace().collect();
    match toks.len() {
        0 => false,
        // A lone comparison operator means both operands expanded to empty
        // (`$null != $null`); compare empty-to-empty rather than reading the bare
        // operator as a truthy string.
        1 if is_cmp_op(toks[0]) => compare("", toks[0], ""),
        // A lone token may be a spaceless comparison (`5==X`); else it's truthy.
        1 => match split_spaceless_op(toks[0]) {
            Some((a, op, b)) => compare(a, op, b),
            None => truthy(toks[0]),
        },
        // Two tokens are normally a unary test (`%x isnum`). But a comparison
        // whose other operand expanded to empty — the ubiquitous `%x == $null`,
        // where `$null` -> "" — also collapses to two tokens, because
        // split_whitespace drops the empty side. Route a bare comparison
        // operator to `compare` with that empty operand instead of mistaking the
        // whole thing for a (truthy) unary expression.
        2 if is_cmp_op(toks[1]) => compare(toks[0], toks[1], ""),
        2 if is_cmp_op(toks[0]) => compare("", toks[0], toks[1]),
        2 => unary_op(toks[0], toks[1]),
        // 3+ tokens are normally `a OP rest`. But when an operand expands to a
        // multi-word value, an equality test against `$null` (which becomes "")
        // leaves the operator as the LAST token — `if (%line == $null)` with a
        // space-containing %line expands to `word1 word2 … ==`. Detect a trailing
        // `==`/`===`/`!=` as that emptiness test. (`<`/`>` stay positional — they
        // also occur as literal characters, e.g. a `>guest` nick prefix.)
        len if is_eq_op(toks[len - 1]) => compare(&toks[..len - 1].join(" "), toks[len - 1], ""),
        _ => compare(toks[0], toks[1], &toks[2..].join(" ")),
    }
}

/// Resolves the state-aware list operators (those needing channel/member
/// state). Operand order matches mSL: `<value> <op> <target>`. Returns `None`
/// for any other term so the caller falls back to the pure comparison logic.
/// Prefix chars assume the standard PREFIX set (~ owner, & admin, @ op,
/// % halfop, + voice).
fn state_op(state: &crate::irc::state::StateSnapshot, term: &str) -> Option<bool> {
    let toks: Vec<&str> = term.split_whitespace().collect();
    if toks.len() < 2 {
        return None;
    }
    let a = toks[0];
    let op = toks[1].to_ascii_lowercase();
    let target = toks.get(2..).map(|r| r.join(" ")).unwrap_or_default();
    // Is `nick` a member of `chan` holding `prefix` (None = any membership)?
    let member_has = |chan: &str, nick: &str, prefix: Option<char>| -> bool {
        match state.channels.iter().find(|c| c.name.eq_ignore_ascii_case(chan)) {
            Some(c) => c.members.iter().any(|(n, pre)| {
                n.eq_ignore_ascii_case(nick)
                    && match prefix {
                        Some(p) => pre.contains(p),
                        None => true,
                    }
            }),
            None => false,
        }
    };
    match op.as_str() {
        "ison" => Some(member_has(&target, a, None)),
        "isop" => Some(member_has(&target, a, Some('@'))),
        "ishop" => Some(member_has(&target, a, Some('%'))),
        "isvoice" => Some(member_has(&target, a, Some('+'))),
        "isowner" => Some(member_has(&target, a, Some('~'))),
        "isadmin" => Some(member_has(&target, a, Some('&'))),
        // `$nick isreg #chan` -> a member of the channel holding no prefix.
        "isreg" => Some(
            match state.channels.iter().find(|c| c.name.eq_ignore_ascii_case(&target)) {
                Some(c) => c.members.iter().any(|(n, pre)| n.eq_ignore_ascii_case(a) && pre.is_empty()),
                None => false,
            },
        ),
        // `<mask> isban #chan` -> the value is covered by a +b ban there.
        "isban" => Some(
            match state.channels.iter().find(|c| c.name.eq_ignore_ascii_case(&target)) {
                Some(c) => c.bans.iter().any(|b| b.eq_ignore_ascii_case(a) || wildcard_match(b, a)),
                None => false,
            },
        ),
        // `#chan ischan` -> are we on that channel?
        "ischan" => Some(state.channels.iter().any(|c| c.name.eq_ignore_ascii_case(a))),
        _ => None,
    }
}

fn truthy(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s != "0" && !s.eq_ignore_ascii_case("$false") && !s.eq_ignore_ascii_case("false")
}

/// A `v1 op` test where `op` takes no right-hand operand: `isnum`, `isletter`,
/// `isalnum`, `isalpha`, `islower`, `isupper`.
fn unary_op(a: &str, op: &str) -> bool {
    match op.to_ascii_lowercase().as_str() {
        "isnum" => !a.is_empty() && a.parse::<f64>().is_ok(),
        "isletter" | "isalpha" => !a.is_empty() && a.chars().all(|c| c.is_alphabetic()),
        "isalnum" => !a.is_empty() && a.chars().all(|c| c.is_alphanumeric()),
        "islower" => !a.is_empty() && a.chars().any(|c| c.is_alphabetic()) && a.chars().all(|c| !c.is_uppercase()),
        "isupper" => !a.is_empty() && a.chars().any(|c| c.is_alphabetic()) && a.chars().all(|c| !c.is_lowercase()),
        // A bare two-token expression with an unknown operator: treat as truthy
        // of the whole (mIRC would generally see this as a non-empty string).
        _ => truthy(&format!("{a} {op}")),
    }
}

fn compare(a: &str, op: &str, b: &str) -> bool {
    match op.to_ascii_lowercase().as_str() {
        "==" => a.eq_ignore_ascii_case(b),
        "===" => a == b,
        "!=" => !a.eq_ignore_ascii_case(b),
        "isin" => b.to_lowercase().contains(&a.to_lowercase()),
        "isincs" => b.contains(a),
        "iswm" => wildcard_match(a, b),
        "iswmcs" => wildcard_match_cs(a, b),
        // `v1 isnum n1-n2` — numeric and within the inclusive range.
        "isnum" => match a.parse::<f64>() {
            Ok(x) => match b.split_once('-') {
                Some((lo, hi)) => {
                    let lo = lo.trim().parse::<f64>().unwrap_or(f64::MIN);
                    let hi = hi.trim().parse::<f64>().unwrap_or(f64::MAX);
                    x >= lo && x <= hi
                }
                None => true,
            },
            Err(_) => false,
        },
        // `v1 isletter list` — every char of v1 is alphabetic and in `list`.
        "isletter" => {
            !a.is_empty()
                && a.chars().all(|c| c.is_alphabetic())
                && (b.is_empty() || a.chars().all(|c| b.contains(c)))
        }
        // `v1 // v2` -> v2 is a multiple of v1; `v1 \\ v2` -> it is not.
        "//" | "\\\\" => match (a.parse::<i64>(), b.parse::<i64>()) {
            (Ok(x), Ok(y)) if x != 0 => {
                let multiple = y % x == 0;
                if op == "//" {
                    multiple
                } else {
                    !multiple
                }
            }
            _ => false,
        },
        // `v1 & v2` -> their bitwise AND is non-zero (mIRC's `&` test).
        "&" => match (a.parse::<i64>(), b.parse::<i64>()) {
            (Ok(x), Ok(y)) => (x & y) != 0,
            _ => false,
        },
        "<" | ">" | "<=" | ">=" => match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(x), Ok(y)) => match op {
                "<" => x < y,
                ">" => x > y,
                "<=" => x <= y,
                _ => x >= y,
            },
            _ => false,
        },
        _ => false,
    }
}

/// True for the exclusively-binary comparison operators (the same set
/// `split_spaceless_op` recognises). Lets a collapsed comparison with an empty
/// operand (`%x == $null`) be told apart from a unary `is*` test.
fn is_cmp_op(op: &str) -> bool {
    matches!(op, "===" | "==" | "!=" | "<=" | ">=" | "<" | ">")
}

/// The equality operators only. Safe to locate positionally even when an operand
/// expanded to a multi-word value — unlike `<`/`>`, which also occur as literal
/// characters and so can't be assumed to be operators.
fn is_eq_op(op: &str) -> bool {
    matches!(op, "==" | "===" | "!=")
}

/// Splits a spaceless `a<op>b` comparison (e.g. `5==X`, `%n>=3`) into its parts,
/// so mSL's no-space conditions — `if ($2==X)` — compare correctly. Longer
/// operators are tried first so `===`/`<=`/`>=` aren't mis-split.
fn split_spaceless_op(s: &str) -> Option<(&str, &'static str, &str)> {
    for op in ["===", "==", "!=", "<=", ">=", "<", ">"] {
        if let Some(idx) = s.find(op) {
            if idx > 0 {
                return Some((&s[..idx], op, &s[idx + op.len()..]));
            }
        }
    }
    None
}

/// True if `s` is one balanced `(...)` group wrapping the whole string — so its
/// contents can be safely re-evaluated. False for `(a)==(b)` (the first group
/// closes before the end). Parens are ASCII, so byte indexing is fine.
fn is_fully_parenthesised(s: &str) -> bool {
    let b = s.as_bytes();
    if b.first() != Some(&b'(') || b.last() != Some(&b')') {
        return false;
    }
    let mut depth = 0u32;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && i + 1 < b.len() {
            return false;
        }
    }
    depth == 0
}

/// Splits leading `-switches` off command args, e.g. `"-m tbl item"` ->
/// `("m", "tbl item")`. Returns `("", trimmed)` when there are no switches.
/// Only a leading `-token` is treated as switches; later `-` args (e.g. a
/// negative value) are left in place.
fn split_switches(raw: &str) -> (&str, &str) {
    let t = raw.trim_start();
    match t.strip_prefix('-') {
        Some(body) => {
            let end = body.find(char::is_whitespace).unwrap_or(body.len());
            (&body[..end], body[end..].trim_start())
        }
        None => ("", t),
    }
}

/// Case-insensitive wildcard match supporting `*` and `?`.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    wm(&p, &t)
}

/// Case-sensitive wildcard match (for `iswmcs`).
pub fn wildcard_match_cs(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    wm(&p, &t)
}

fn wm(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' => {
            // Match zero or more characters.
            wm(&p[1..], t) || (!t.is_empty() && wm(p, &t[1..]))
        }
        '?' => !t.is_empty() && wm(&p[1..], &t[1..]),
        c => !t.is_empty() && t[0] == c && wm(&p[1..], &t[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard() {
        assert!(wildcard_match("!ping*", "!ping hello"));
        assert!(wildcard_match("*", "anything"));
        assert!(!wildcard_match("!ping*", "hello"));
        assert!(wildcard_match("h?llo", "hello"));
    }

    #[test]
    fn bool_eval() {
        assert!(eval_bool("5 == 5"));
        assert!(eval_bool("3 < 5"));
        assert!(!eval_bool("3 > 5"));
        assert!(eval_bool("abc isin xabcy"));
        assert!(eval_bool("1 == 1 && 2 == 2"));
        assert!(eval_bool("1 == 2 || 3 == 3"));
        assert!(eval_bool("nonempty"));
        assert!(!eval_bool("0"));
    }

    #[test]
    fn bool_operators() {
        // isnum, with and without a range
        assert!(eval_bool("5 isnum"));
        assert!(!eval_bool("abc isnum"));
        assert!(eval_bool("5 isnum 1-10"));
        assert!(!eval_bool("50 isnum 1-10"));
        // letter / alnum classes
        assert!(eval_bool("abc isletter"));
        assert!(!eval_bool("ab2 isletter"));
        assert!(eval_bool("b isletter abc"));
        assert!(!eval_bool("z isletter abc"));
        assert!(eval_bool("abc123 isalnum"));
        assert!(eval_bool("abc isalpha"));
        assert!(eval_bool("abc islower"));
        assert!(eval_bool("ABC isupper"));
        assert!(!eval_bool("Abc islower"));
        // case sensitivity
        assert!(eval_bool("ABC isincs xABCy"));
        assert!(!eval_bool("abc isincs xABCy"));
        assert!(eval_bool("AB* iswmcs ABCD"));
        assert!(!eval_bool("ab* iswmcs ABCD"));
        // multiple-of
        assert!(eval_bool("3 // 9"));
        assert!(!eval_bool("3 // 10"));
        assert!(eval_bool("3 \\\\ 10"));
        // negation
        assert!(eval_bool("!0"));
        assert!(!eval_bool("!5"));
        assert!(eval_bool("!")); // empty operand -> negation of false
        assert!(!eval_bool("!(5 == 5)"));
    }

    #[test]
    fn empty_operand_comparisons() {
        // `%x == $null` is the canonical mSL emptiness test. After expansion
        // `$null` is "", so the term becomes `value ==` (whitespace splitting
        // drops the empty side). It must compare against empty, not read as a
        // truthy unary expression.
        assert!(!eval_bool("abc =="), "nonempty == $null must be false");
        assert!(eval_bool("abc !="), "nonempty != $null must be true");
        assert!(!eval_bool("abc <"), "nonempty < $null (empty !numeric) is false");
        // The operand that expanded to empty may be on the left, too.
        assert!(!eval_bool("== abc"));
        assert!(eval_bool("!= abc"));
        // Both operands empty (`$null == $null`) -> just the operator.
        assert!(eval_bool("=="));
        assert!(!eval_bool("!="));
        // Genuine unary tests must not be mistaken for collapsed comparisons.
        assert!(eval_bool("5 isnum"));
        assert!(eval_bool("abc isletter"));

        // A multi-word value tested against $null: the value's spaces make the
        // operator land last (`if (%line != $null)` with a spacey %line). This is
        // the canonical socket-read guard.
        assert!(eval_bool("AUTH GateKeeper S :GKSSP x !="));
        assert!(!eval_bool("AUTH GateKeeper S :GKSSP x =="));
        // A literal `>` / `<` as a real right-hand operand stays a comparison, not
        // a mistaken emptiness test (e.g. `if ($left(%nick,1) == >)`).
        assert!(eval_bool("> == >"));
        assert!(!eval_bool("a == >"));

        // `!operand` negates the operand's truthiness even when the value holds
        // comparison characters — `if (!$2)` is an emptiness test, not `a < b`.
        assert!(!eval_bool("!abc"));
        assert!(!eval_bool("!a<b"));
        assert!(!eval_bool("!x>y=z"));
        assert!(eval_bool("!")); // empty value -> !false
        assert!(eval_bool("!0")); // 0 is falsy -> !false
        assert!(!eval_bool("!(5 == 5)"));
    }

    #[test]
    fn split_args_keeps_whitespace_only() {
        // mIRC keeps a deliberate space — `$asc(" ")` is 32 — but still trims
        // ordinary args (much of the engine relies on it).
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args(" "), vec![" ".to_string()]);
        assert_eq!(split_args("a, b"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(split_args("a,"), vec!["a".to_string(), String::new()]);
    }

    #[test]
    fn top_level_commas() {
        assert_eq!(split_top_commas("a, b, c"), vec!["a", " b", " c"]);
        // commas inside an identifier's args are not split points
        assert_eq!(split_top_commas("%x = $iif(a,b,c)"), vec!["%x = $iif(a,b,c)"]);
    }
}
