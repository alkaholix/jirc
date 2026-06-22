//! The mSL evaluator: identifier/variable expansion, condition evaluation,
//! control flow, and the built-in command library.

use std::collections::HashMap;

use super::ast::{Script, Stmt};
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
}

/// Reserved `%var` key holding the byte count of the last `/sockread` (read by
/// `$sockbr`); the NUL char can't appear in a real variable name.
pub const SOCK_BR_KEY: &str = "\u{0}sockbr";

/// Per-invocation variables ($nick, $chan, params, …).
#[derive(Debug, Clone, Default)]
pub struct EventVars {
    pub nick: String,
    pub chan: String,
    pub target: String,
    pub text: String,
    pub params: Vec<String>,
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
                    if self.goto.is_some() {
                        break; // a goto out of the loop body bubbles up
                    }
                }
            }
            Stmt::Label(_) => {} // a jump target; no-op when reached normally
        }
    }

    // ---- command dispatch ----

    fn dispatch(&mut self, name: &str, raw_args: &str) {
        let lname = name.to_ascii_lowercase();
        match lname.as_str() {
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
            "inc" => self.cmd_incdec(raw_args, 1),
            "dec" => self.cmd_incdec(raw_args, -1),
            "write" => self.cmd_write(raw_args),
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
            // We evaluate any parameters (for identifier side effects) and stop.
            // `/run` is deliberately a no-op — jIRC never launches programs.
            "clear" | "clearall" | "close" | "window" | "aline" | "rline" | "sline" | "dline"
            | "cline" | "iline" | "fline" | "renwin" | "titlebar" | "editbox" | "linesep"
            | "background" | "color" | "font" | "flash" | "beep" | "ebeeps" | "speak" | "splay"
            | "play" | "sound" | "run" | "url" | "dns" | "debug" | "log" | "logview"
            | "timestamp" | "donotdisturb" | "toolbar" | "menubar" | "switchbar" | "treebar"
            | "mdi" | "save" | "saveini" | "flushini" | "writeini" | "remini" | "loadbuf"
            | "savebuf" | "filter" | "showmirc" | "maximize" | "minimize" | "ial"
            | "creq" | "sreq" | "fullname" | "clipboard" | "resetidle" => {
                let _ = self.expand(raw_args);
            }
            _ => {
                // A user-defined alias?
                if let Some(alias) = self.script.find_alias(&lname) {
                    let body = alias.body.clone();
                    let params = split_params(&self.expand(raw_args));
                    self.call_alias(&body, params);
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
        let (name, text) = match rest.split_once(char::is_whitespace) {
            Some((n, t)) => (self.expand(n), self.expand(t)),
            None => (self.expand(rest), String::new()),
        };
        if name.is_empty() {
            return;
        }
        let mut data = text.into_bytes();
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

/// Splits identifier arguments on top-level commas.
fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
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
            ',' if depth == 0 => {
                args.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() || !args.is_empty() {
        args.push(cur.trim().to_string());
    }
    args
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
            return !eval_term_with(rest.trim(), leaf);
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
        // A lone token may be a spaceless comparison (`5==X`); else it's truthy.
        1 => match split_spaceless_op(toks[0]) {
            Some((a, op, b)) => compare(a, op, b),
            None => truthy(toks[0]),
        },
        2 => unary_op(toks[0], toks[1]),
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
    fn top_level_commas() {
        assert_eq!(split_top_commas("a, b, c"), vec!["a", " b", " c"]);
        // commas inside an identifier's args are not split points
        assert_eq!(split_top_commas("%x = $iif(a,b,c)"), vec!["%x = $iif(a,b,c)"]);
    }
}
