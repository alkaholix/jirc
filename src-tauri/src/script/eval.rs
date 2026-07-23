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
    Echo {
        target: String,
        text: String,
    },
    /// Add, stop, or otherwise control an application-wide `/play` queue.
    Play {
        args: String,
        current_target: String,
        remote: bool,
        /// Script file in which `/play` was invoked, for deferred local aliases.
        source: String,
    },
    /// One line emitted by the `/play` worker. `echo` is `/play -e`; ordinary
    /// playback deliberately does not use [`Action::Send`]'s local echo.
    PlayLine {
        target: String,
        text: String,
        notice: bool,
        echo: bool,
    },
    /// Schedule a command to run later, `reps` times every `interval_ms`.
    /// An empty `name` means auto-assign one.
    Timer {
        name: String,
        reps: u32,
        interval_ms: u64,
        /// Optional local wall-clock start (`HH:nn[:ss]`).
        start_at: Option<String>,
        command: String,
        target: String,
        /// `-o`: keep running after the originating connection closes.
        offline: bool,
        /// `-c`: retain the real-time schedule and catch up missed intervals.
        catch_up: bool,
        /// `-d`: serialize this timer with other ordered timers.
        ordered: bool,
        /// `-m`/`-h`: the interval is expressed in milliseconds.
        milliseconds: bool,
        /// `-h`: high-resolution millisecond timer (Tokio already uses its
        /// monotonic high-resolution clock; retained for `$timer().mmt`).
        high_resolution: bool,
        /// `-i`: move to another live connection if the original one closes.
        dynamic: bool,
        /// Script file in which the timer was created, for local aliases.
        source: String,
    },
    /// Stop timers matching a name or wildcard (`name` = "*" stops all).
    TimerStop {
        name: String,
    },
    /// Execute timers matching a name or wildcard immediately (`-e`).
    TimerExecute {
        name: String,
    },
    /// Pause timers matching a name or wildcard (`-p`/`-P`).
    TimerPause {
        name: String,
        countdown: bool,
    },
    /// Resume paused timers matching a name or wildcard (`-r`).
    TimerResume {
        name: String,
    },
    /// Echo the list of active timers into `target`.
    TimerList {
        target: String,
        name: String,
    },
    /// Add/update/delete script-defined application toolbar buttons.
    Toolbar {
        op: String,
        name: String,
        tooltip: String,
        icon: String,
        command: String,
        source: String,
    },
    /// Add/update/delete safe docked script panels, rows, and buttons.
    Panel {
        op: String,
        panel: String,
        id: String,
        label: String,
        value: String,
        command: String,
        source: String,
    },
    /// Open a TCP socket (`/sockopen`); `tls` for `-e` (encrypted).
    SockOpen {
        name: String,
        host: String,
        port: u16,
        tls: bool,
        /// `-a`: accept an invalid TLS certificate for this socket only.
        accept_invalid: bool,
        bind_ip: String,
        /// `-n`: disable Nagle's algorithm.
        nodelay: bool,
        /// `-4`/`-6`; zero lets the resolver choose either family.
        ip_version: u8,
        /// Stable synchronous reservation consumed when the deferred connect starts.
        reservation_id: u64,
    },
    /// Send a UDP datagram, optionally retaining the socket for `on UDPREAD`.
    SockUdp {
        name: String,
        bind_ip: String,
        local_port: u16,
        dest_ip: String,
        dest_port: u16,
        data: Vec<u8>,
        keep: bool,
        /// `-u`: create an IPv4/IPv6 dual-stack UDP socket.
        dual_stack: bool,
        /// Stable synchronous reservation, or zero when reusing an existing UDP socket.
        reservation_id: u64,
    },
    /// Write bytes to a socket (`/sockwrite`).
    SockWrite {
        name: String,
        data: Vec<u8>,
    },
    /// Dispatch a deferred socket error event after the current routine returns.
    SockError {
        kind: String,
        name: String,
        error: i32,
    },
    /// Close a socket (`/sockclose`).
    SockClose {
        name: String,
    },
    /// Deferred fallbacks used before a production socket backend is installed.
    SockMark {
        name: String,
        mark: String,
    },
    SockRename {
        name: String,
        newname: String,
    },
    SockPause {
        name: String,
        resume: bool,
    },
    /// Start the accept loop for a listener bound by `/socklisten` (carries the
    /// owning connection's context to apply-time so events route correctly).
    SockListen {
        name: String,
        /// Stable listener identity so a same-handler rename cannot stale the action.
        listener_id: u64,
    },
    /// `/server [-m] <host> <port> [password]` — connect the native client (also
    /// used by scripts that stand up a local bridge/proxy). The frontend opens or
    /// reuses the server window and starts the connection.
    Server {
        host: String,
        port: u16,
        pass: String,
        new_window: bool,
    },
    /// Open a custom dialog (`/dialog`).
    DialogOpen {
        name: String,
        title: String,
        controls: Vec<super::ast::DialogControl>,
    },
    /// Close a dialog (`/dialog -c`).
    DialogClose {
        name: String,
    },
    /// Mutate a dialog control (`/did`): `op` is set/add/clear.
    DialogSet {
        dialog: String,
        control: String,
        op: String,
        value: String,
    },
    /// Set (or clear, if empty) a nick-list icon for a nick (`/nickicon`).
    NickIcon {
        nick: String,
        icon: String,
    },
    /// Open a custom `@window` (`/window`).
    WindowOpen {
        name: String,
        kind: String,
        title: String,
    },
    /// Close a custom `@window` (`/window -c`).
    WindowClose {
        name: String,
    },
    /// A line op on a custom `@window`: `op` = add/insert/replace/delete/clear.
    WindowLine {
        name: String,
        op: String,
        n: u32,
        text: String,
    },
    /// Open a native browser window with its own persistent profile.
    WebviewOpen {
        name: String,
        profile: String,
        width: u32,
        height: u32,
        url: String,
        title: String,
    },
    /// Navigate a managed native browser window.
    WebviewNavigate {
        name: String,
        url: String,
    },
    /// Read cookies visible to `url` and emit `on WEBVIEW` cookie events.
    WebviewCookies {
        name: String,
        url: String,
    },
    /// Focus a managed native browser window.
    WebviewFocus {
        name: String,
    },
    /// Close a managed native browser window.
    WebviewClose {
        name: String,
    },
    /// Set a stored identity field (`/anick`/`/mnick`/`/fullname`). `field` is
    /// `anick`/`mnick`/`fullname`; updates the live session state so the matching
    /// `$anick`/`$mnick`/`$fullname` reflects it.
    SetIdentity {
        field: String,
        value: String,
    },
    /// Recompile every script file from disk (`/reload`).
    ReloadScripts,
    /// Execute a client-local mIRC `/dcc` subcommand.
    Dcc {
        args: String,
    },
    Fserve {
        nick: String,
        max_gets: usize,
        home: String,
        welcome: Option<String>,
    },
    /// Define/replace (`command` = Some) or remove (`command` = None) a runtime
    /// alias (`/alias <name> [command]`). Persisted to a `_runtime.mrc` file.
    DefineAlias {
        name: String,
        command: Option<String>,
        file: Option<String>,
        local: bool,
    },
    /// Fire `on SIGNAL` handlers matching `name` (`/signal`); `params` become `$1-`.
    Signal {
        name: String,
        params: Vec<String>,
    },
    /// Control the connect-time autojoin from within `on CONNECT` (`/autojoin`):
    /// `skip` cancels it, `delay_secs` > 0 postpones it that many seconds.
    Autojoin {
        skip: bool,
        delay_secs: u32,
    },
    /// Run `command` on another connection (`/scon`/`/scid`): re-dispatched in
    /// that connection's context so its output routes there.
    RunOn {
        server_id: String,
        command: String,
    },
    /// Replace the line currently being handled by `on PARSELINE`, or enqueue a
    /// synthetic incoming/outgoing line when `queue` is set (`/parseline -q`).
    ParseLine {
        direction: String,
        bytes: Vec<u8>,
        queue: bool,
        trigger: bool,
        append_crlf: bool,
        utf8: bool,
    },
}

/// Reserved `%var` key holding the byte count of the last `/sockread` (read by
/// `$sockbr`); the NUL char can't appear in a real variable name.
pub const SOCK_BR_KEY: &str = "\u{0}sockbr";

/// Special-var keys holding the operands of the most recent comparison, read
/// back by `$v1`/`$v2` (the NUL prefix keeps them out of the `%var` namespace).
pub const V1_KEY: &str = "\u{0}v1";
pub const V2_KEY: &str = "\u{0}v2";
/// Value returned by the most recently called alias, for `$result`.
pub const RESULT_KEY: &str = "\u{0}result";
/// The most recent `$?`/`$input` answer, for `$!`.
pub const LASTINPUT_KEY: &str = "\u{0}lastinput";
pub const LTIMER_KEY: &str = "\u{0}ltimer";
/// Number of lines selected by the most recent `/filter` command.
pub const FILTERED_KEY: &str = "\u{0}filtered";

/// Lifetime attached to a variable or hash item by mIRC's `-uN` switch.
/// `Instant` keeps expiry independent of wall-clock changes; `EndOfRun` is
/// mIRC's special `-u0` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimedExpiry {
    At(std::time::Instant),
    EndOfRun,
}

impl TimedExpiry {
    fn after(seconds: u64) -> Self {
        if seconds == 0 {
            Self::EndOfRun
        } else {
            Self::At(std::time::Instant::now() + std::time::Duration::from_secs(seconds))
        }
    }

    fn expired(self, now: std::time::Instant) -> bool {
        matches!(self, Self::At(deadline) if deadline <= now)
    }

    /// Whole seconds remaining, rounded up like mIRC's `.secs`/`.unset`
    /// properties so a newly-created `-u10` value reports 10 rather than 9.
    pub(crate) fn seconds_remaining(self, now: std::time::Instant) -> u64 {
        match self {
            Self::At(deadline) => {
                let remaining = deadline.saturating_duration_since(now);
                remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0)
            }
            Self::EndOfRun => 0,
        }
    }
}

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
    /// Name of the `/timer` currently invoking the command (`$ctimer`).
    pub timer: String,
    /// Destination of the active `/play` item (`$pnick`).
    pub pnick: String,
    /// Stable identity of the loaded script file whose alias/event is running.
    /// Used to enforce mIRC's file-local `alias -l` visibility.
    pub script_source: String,
    /// One-based source line for `$scriptline` (definition line for the current
    /// alias/event; nested aliases replace and restore it with their source).
    pub script_line: usize,
    /// Whether an earlier `^` handler suppressed mIRC's default event text.
    /// `/haltdef` updates this without stopping the current routine.
    pub default_halted: bool,
    /// The numeric of a raw server line (`on RAW`) — exposed as `$numeric`.
    pub numeric: String,
    /// Raw protocol context retained for server-driven events.
    pub raw_msg: String,
    pub raw_bytes: Vec<u8>,
    /// IRCv3 tags as `(key, decoded value)`, plus their original tag string.
    /// IRCv3 tags as `(tag, raw key, had '=')`. mIRC exposes the escaped wire
    /// value through `$msgtags(...).key`; it does not unescape it first.
    pub msg_tags: Vec<(String, String, bool)>,
    pub msg_tags_raw: String,
    pub msg_stamp: String,
    /// `on PARSELINE` context.
    pub parse_line: String,
    pub parse_type: String,
    pub parse_utf: bool,
    pub parse_em: bool,
    /// Wildcard/access data for `$matchkey` and `$maddress`.
    pub match_key: String,
    pub matched_address: String,
    /// The event's matched access level (`$clevel`) and the triggering user's
    /// matched level (`$ulevel`), set by the dispatcher's level gate.
    pub clevel: String,
    pub ulevel: String,
    /// The exact bytes of a `SOCKREAD` line (before UTF-8 decoding), so
    /// `sockread &binvar` can read binary protocols byte-for-byte. Empty for
    /// every other event.
    pub sock_bytes: Vec<u8>,
    /// Error code for the current socket event (`$sockerr`). Zero means that
    /// the event/command completed normally.
    pub sock_error: i32,
    /// Full local path for DCC file events (`$filename`).
    pub filename: String,
    /// Transfer/session id for DCC event-local identifiers.
    pub dcc_id: String,
}

const STEP_LIMIT: u32 = 100_000;

/// Sentinel `goto` targets for `/break` and `/continue` — the NUL prefix keeps
/// them from colliding with any real `:label`. Consumed by `Stmt::While`.
const LOOP_BREAK: &str = "\u{0}break";
const LOOP_CONTINUE: &str = "\u{0}continue";
const STATUS: &str = "(status)";
const WSA_INVALID_ARGUMENT: i32 = 10_022;
// Deferred values are wrapped and base64-encoded instead of replacing special
// characters with Private-Use code points. NUL framing cannot collide with
// mIRC file/script text (text reads stop at NUL), while allowing every visible
// Unicode scalar, including the old U+E101..U+E106 sentinels, to pass through.
const DELAY_PREFIX: &str = "\0jirc-unsafe:";
const PIPE_PREFIX: &str = "\0jirc-read-pipe:";
const ENVELOPE_END: char = '\0';

/// Synchronous socket operations the engine can call *during* a run, so
/// `/socklisten` binds immediately and `$sock(name).port` is readable on the
/// same line (like mIRC). The production backend is the SocketManager; tests use
/// [`NoSockets`] or a fake. Names may be wildcards for the query methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketReadOptions {
    /// Destination is a binary variable. This controls mIRC's default 4096-byte
    /// binary read and whether unread data causes another SOCKREAD event.
    pub binary: bool,
    /// `-f`: return an unterminated partial line instead of waiting for CRLF.
    pub force: bool,
    /// Text reads and `sockread -n &binvar` consume one CRLF-terminated line.
    pub line: bool,
    /// Maximum bytes for a raw binary read (4096 when omitted by the script).
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SocketReadResult {
    /// Bytes placed in the destination after any CRLF stripping.
    pub data: Vec<u8>,
    /// Total bytes consumed from the socket receive queue. For a line read this
    /// includes the stripped CRLF, allowing an empty line to keep `$sockbr > 0`.
    pub bytes_read: usize,
}

/// Result of synchronously queueing a `/sockwrite`. `failures` retains the
/// concrete socket names for wildcard writes so each failed socket can receive
/// its own `on SOCKWRITE` event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SocketWriteResult {
    pub error: i32,
    pub failures: Vec<(String, i32)>,
}

pub trait ScriptSockets: Send + Sync {
    /// Reserves a new TCP socket synchronously so `$sock()` and state commands
    /// observe it before the deferred network connect starts.
    fn reserve_open(
        &self,
        _name: &str,
        _host: &str,
        _port: u16,
        _tls: bool,
        _bind_ip: &str,
    ) -> Option<Result<u64, i32>> {
        None
    }
    /// Reserves a new UDP socket synchronously. An id of zero means an existing
    /// compatible UDP socket will be reused.
    fn reserve_udp(
        &self,
        _name: &str,
        _bind_ip: &str,
        _local_port: u16,
        _dest_ip: &str,
        _dest_port: u16,
    ) -> Option<Result<u64, i32>> {
        None
    }
    /// Binds a listening socket; `None` means no synchronous backend is installed.
    fn listen(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Option<Result<u16, i32>>;
    /// Binds a listener and returns its stable identity. Backends without stable
    /// reservations retain the original behavior with identity zero.
    fn listen_reserved(
        &self,
        bind_ip: &str,
        name: &str,
        port: u16,
        nodelay: bool,
        dual_stack: bool,
    ) -> Option<Result<(u16, u64), i32>> {
        self.listen(bind_ip, name, port, nodelay, dual_stack)
            .map(|result| result.map(|bound_port| (bound_port, 0)))
    }
    /// Accepts the pending incoming connection of listener `listener` into a
    /// socket named `name`.
    fn accept(&self, name: &str, listener: &str, nodelay: bool) -> Option<i32>;
    fn close(&self, pattern: &str) -> Option<i32>;
    fn set_mark(&self, name: &str, mark: &str) -> Option<i32>;
    /// `/sockrename <name> <newname>`.
    fn rename(&self, name: &str, newname: &str) -> Option<i32>;
    /// `/sockpause [-r]` — pause (or, with `resume`, restart) reading.
    fn pause(&self, name: &str, resume: bool) -> Option<i32>;
    /// Queues a TCP write synchronously so `$sock().sq` and `$sockerr` are
    /// observable on the next script line. `None` asks the evaluator to retain
    /// its deferred [`Action::SockWrite`] fallback; `Some(result)` handled it.
    fn write(&self, name: &str, data: &[u8]) -> Option<SocketWriteResult>;
    /// `/sockopen -t name` — upgrades an existing plain TCP socket in place.
    fn starttls(&self, name: &str) -> Option<i32>;
    /// Reads and consumes data queued for a connected socket. `None` means no
    /// synchronous backend is installed; `Some(Ok(default()))` means no data yet.
    fn read(&self, name: &str, options: SocketReadOptions)
        -> Option<Result<SocketReadResult, i32>>;
    /// Whether a socket matching `name` (possibly a wildcard) exists.
    fn exists(&self, name: &str) -> bool;
    /// Socket names matching `pattern`, in stable enumeration order. Used by
    /// mIRC's `$sock(pattern,0)` count and `$sock(pattern,N)` lookup forms.
    fn matching_names(&self, pattern: &str) -> Vec<String>;
    /// `$sock(name).property` value (empty for unknown name/property).
    fn prop(&self, name: &str, property: &str) -> String;
    /// `/socklist` — formatted lines for sockets matching `filter`.
    fn list(&self, filter: &str) -> Vec<String>;
}

/// A no-op socket backend (used in tests and before a real one is installed).
pub struct NoSockets;
impl ScriptSockets for NoSockets {
    fn listen(&self, _: &str, _: &str, _: u16, _: bool, _: bool) -> Option<Result<u16, i32>> {
        None
    }
    fn accept(&self, _: &str, _: &str, _: bool) -> Option<i32> {
        None
    }
    fn close(&self, _: &str) -> Option<i32> {
        None
    }
    fn set_mark(&self, _: &str, _: &str) -> Option<i32> {
        None
    }
    fn rename(&self, _: &str, _: &str) -> Option<i32> {
        None
    }
    fn pause(&self, _: &str, _: bool) -> Option<i32> {
        None
    }
    fn write(&self, _: &str, _: &[u8]) -> Option<SocketWriteResult> {
        None
    }
    fn starttls(&self, _: &str) -> Option<i32> {
        None
    }
    fn read(&self, _: &str, _: SocketReadOptions) -> Option<Result<SocketReadResult, i32>> {
        None
    }
    fn exists(&self, _: &str) -> bool {
        false
    }
    fn matching_names(&self, _: &str) -> Vec<String> {
        Vec::new()
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

/// A snapshot of one active `/timer`, for `$timer`.
#[derive(Clone, Default)]
pub struct TimerInfo {
    pub name: String,
    pub command: String,
    pub reps: u32,
    /// Delay between fires, in seconds.
    pub delay: u64,
    /// Wall-clock start, when one was supplied.
    pub time: String,
    /// `online` or `offline`.
    pub timer_type: String,
    /// Whole seconds until the next scheduled trigger.
    pub secs: u64,
    /// Whether `-m`/`-h` millisecond timing is in use.
    pub mmt: bool,
    /// Whether `-i` dynamic connection association is enabled.
    pub anysc: bool,
    /// mIRC-style numeric connection id assigned by the script engine.
    pub cid: u32,
    /// 0 = running, 1 = execution paused, 2 = countdown paused.
    pub pause: u8,
}

/// Read-only access to the active timers, for `$timer(...)`. Implemented by a
/// bridge that reads the Tauri-managed `TimerManager`.
pub trait ScriptTimers: Send + Sync {
    fn snapshot(&self) -> Vec<TimerInfo>;

    fn last(&self) -> String {
        String::new()
    }
}

/// A no-op timers backend (tests / before a real one is installed).
pub struct NoTimers;
impl ScriptTimers for NoTimers {
    fn snapshot(&self) -> Vec<TimerInfo> {
        Vec::new()
    }
}

/// A queued `/play` item exposed to `$play(...)`.
#[derive(Clone, Default)]
pub struct PlayInfo {
    pub target: String,
    pub play_type: String,
    pub filename: String,
    pub topic: String,
    pub pos: usize,
    pub lines: usize,
    pub delay: u64,
    pub status: String,
}

/// Read-only access to the application-wide play queue for `$play(...)`.
pub trait ScriptPlay: Send + Sync {
    fn snapshot(&self) -> Vec<PlayInfo>;
}

/// A no-op play backend used by tests and before application setup completes.
pub struct NoPlay;
impl ScriptPlay for NoPlay {
    fn snapshot(&self) -> Vec<PlayInfo> {
        Vec::new()
    }
}

/// One DCC chat/send/get item exposed to `$chat`/`$send`/`$get`.
#[derive(Clone, Default)]
pub struct DccInfo {
    pub kind: String,
    pub nick: String,
    pub filename: String,
    pub path: String,
    pub ip: String,
    pub status: String,
    pub transferred: u64,
    pub size: u64,
    pub resume: u64,
    pub last_ack: u64,
    pub secs: u64,
}

pub trait ScriptDcc: Send + Sync {
    fn snapshot(&self, server_id: &str) -> Vec<DccInfo>;
}

pub struct NoDcc;
impl ScriptDcc for NoDcc {
    fn snapshot(&self, _: &str) -> Vec<DccInfo> {
        Vec::new()
    }
}

/// One native script browser exposed to `$webview(...)`.
#[derive(Clone, Default)]
pub struct WebviewInfo {
    pub name: String,
    pub profile: String,
    pub status: String,
    pub url: String,
}

/// Read-only view of native script browser windows.
pub trait ScriptWebviews: Send + Sync {
    fn snapshot(&self, server_id: &str) -> Vec<WebviewInfo>;
}

/// No-op browser backend used by tests and before application setup completes.
pub struct NoWebviews;
impl ScriptWebviews for NoWebviews {
    fn snapshot(&self, _: &str) -> Vec<WebviewInfo> {
        Vec::new()
    }
}

/// A per-run view of the connection registry, for `$cid`/`$scon`/`$activecid`.
#[derive(Clone, Default)]
pub struct ConnsView {
    /// `(cid, server_id)` for every live connection, in ascending cid order.
    pub entries: Vec<(u32, String)>,
    /// The active window's connection cid (0 = none reported).
    pub active_cid: u32,
}

impl ConnsView {
    /// The cid for a server id (0 if unknown) — backs `$cid`.
    pub fn cid_of(&self, server_id: &str) -> u32 {
        self.entries
            .iter()
            .find(|(_, id)| id == server_id)
            .map(|(c, _)| *c)
            .unwrap_or(0)
    }
}

/// A per-run view of the window registry, for `$wid`/`$activewid`.
#[derive(Clone, Default)]
pub struct WinView {
    /// `(wid, server_id, name)` for every open window.
    pub entries: Vec<(u32, String, String)>,
    /// The active window's wid (0 = none reported).
    pub active_wid: u32,
}

impl WinView {
    /// The wid of a window (0 if unknown) — backs `$wid`.
    pub fn wid_of(&self, server_id: &str, name: &str) -> u32 {
        self.entries
            .iter()
            .find(|(_, sid, n)| sid == server_id && n.eq_ignore_ascii_case(name))
            .map(|(w, _, _)| *w)
            .unwrap_or(0)
    }
}

/// The execution context for a single alias/event run.
pub struct Runtime<'a> {
    pub script: &'a Script,
    pub my_nick: &'a str,
    pub network: &'a str,
    pub server: &'a str,
    /// The name of the frontend's currently-focused window/buffer, for `$active`
    /// (empty when unknown — mIRC's `$active` may be `$null` too).
    pub active: String,
    pub vars: &'a mut HashMap<String, String>,
    /// Routine-local `/var` frames. The last frame is the current alias/event
    /// routine; nested aliases push their own frame so locals shadow, rather
    /// than overwrite, the caller's locals and engine-global `/set` values.
    pub(crate) local_scopes: Vec<HashMap<String, String>>,
    pub hashes: &'a mut HashMap<String, HashMap<String, String>>,
    /// `-uN` metadata, kept separate so the existing variable/hash storage and
    /// all ordinary lookups remain lightweight and backwards-compatible.
    pub(crate) var_expiry: &'a mut HashMap<String, TimedExpiry>,
    pub(crate) hash_expiry: &'a mut HashMap<(String, String), TimedExpiry>,
    pub event: EventVars,
    pub actions: Vec<Action>,
    /// Commands created by a `$read(...,p)`/`$readini(...,p)` pipe. They run
    /// after the command containing the identifier, preserving left-to-right
    /// mIRC command-separator order.
    pub(crate) pending_pipe_commands: Vec<String>,
    pub halted: bool,
    pub steps: u32,
    pub depth: u32,
    /// Active alias names; mIRC aliases cannot recurse directly or indirectly.
    pub alias_stack: Vec<String>,
    /// Value set by `/return`, consumed when an alias is used as `$identifier`.
    pub ret: Option<String>,
    /// Pending `/goto` target, bubbled up until a body containing the label
    /// resolves it.
    pub goto: Option<String>,
    /// Sandbox directory for `$read`/`/write` file I/O.
    pub data_dir: std::path::PathBuf,
    /// Live channel/member snapshot for state-aware identifiers.
    pub state: std::sync::Arc<crate::irc::state::StateSnapshot>,
    /// Connection registry view for `$cid`/`$scon`/`$activecid`.
    pub conns: ConnsView,
    /// Window registry view for `$wid`/`$activewid`.
    pub wins: WinView,
    /// Synchronous socket backend for `/socklisten`/`/sockaccept`/`$sock(...)`.
    pub sockets: std::sync::Arc<dyn ScriptSockets>,
    /// Read-only view of active timers, for `$timer(...)`.
    pub timers: std::sync::Arc<dyn ScriptTimers>,
    /// Read-only view of the application-wide `/play` queue.
    pub play: std::sync::Arc<dyn ScriptPlay>,
    /// Read-only DCC manager view for `$chat`/`$send`/`$get`.
    pub dcc: std::sync::Arc<dyn ScriptDcc>,
    /// Read-only view of native script browser windows for `$webview(...)`.
    pub webviews: std::sync::Arc<dyn ScriptWebviews>,
    /// Backend for `$input` prompts.
    pub input: std::sync::Arc<dyn ScriptInput>,
    /// Open file handles for `/fopen`/`/fwrite`/`$fread`/`$fopen(...)`.
    pub files: &'a mut crate::script::files::FileStore,
    /// Binary variables for `/bset`/`/bunset`/`$bvar`/`$bfind`/`&binvar`.
    pub bins: &'a mut crate::script::binvar::BinStore,
    /// Custom `@windows` for `/window`/`/aline`/`/rline`/`$window`/`$line`.
    pub windows: &'a mut crate::script::window::WindowStore,
    /// User access list for `/auser`/`/ruser`/`$ulist`/`$level` + gated events.
    pub users: &'a mut crate::script::users::UserList,
    /// What invoked the current alias frame ("command"/"event"/"menu"/"identifier"),
    /// for `$caller`/`$isid`. Saved + restored around nested alias calls.
    pub caller: &'static str,
    /// Verbose flag for `$show`: `false` inside an alias invoked as a silent
    /// `.command`, else `true`. Saved + restored around nested alias calls.
    pub show: bool,
}

impl<'a> Runtime<'a> {
    pub fn run(&mut self, body: &[Stmt]) {
        self.purge_expired();
        // Top-level aliases/events enter through `run()` directly. Nested alias
        // calls install their own frame in `call_alias()` before coming here.
        let owns_local_scope = self.depth == 0 && self.local_scopes.is_empty();
        if owns_local_scope {
            self.local_scopes.push(HashMap::new());
        }
        self.depth += 1;
        if self.depth > 64 {
            self.halted = true;
        }
        let mut i = 0;
        while i < body.len() {
            self.refresh_nicklist_state();
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
            let stmt_line = body[i].source_line();
            if stmt_line != 0 {
                self.event.script_line = stmt_line;
            }
            match &body[i] {
                Stmt::Command { name, args, .. } if name.eq_ignore_ascii_case("goto") => {
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
        if owns_local_scope {
            self.local_scopes.pop();
        }
        if self.depth == 0 {
            self.finish_timed_values();
        }
    }

    /// Returns the visible value for a user variable: the innermost routine
    /// local wins, followed by an engine-global `/set` value.
    pub(crate) fn var_value(&self, name: &str) -> Option<&String> {
        self.local_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .or_else(|| self.vars.get(name))
    }

    fn set_local_var(&mut self, name: String, value: String) {
        // `/var` normally runs inside a top-level or nested routine. Keeping a
        // fallback frame makes direct evaluator use behave locally as well.
        if self.local_scopes.is_empty() {
            self.local_scopes.push(HashMap::new());
        }
        self.local_scopes.last_mut().unwrap().insert(name, value);
    }

    /// Assigns the nearest visible local, or the global variable when no local
    /// declaration exists. Used by mutating commands such as `/inc` and
    /// `/sockread`; `/set` deliberately continues to write the global map.
    pub(super) fn set_visible_var(&mut self, name: String, value: String) -> bool {
        if let Some(scope) = self
            .local_scopes
            .iter_mut()
            .rev()
            .find(|s| s.contains_key(&name))
        {
            scope.insert(name, value);
            true
        } else {
            self.vars.insert(name, value);
            false
        }
    }

    pub(crate) fn visible_vars(&self) -> Vec<(String, String, bool)> {
        let mut visible: HashMap<String, (String, bool)> = self
            .vars
            .iter()
            .filter(|(name, _)| !name.contains('\u{0}'))
            .map(|(name, value)| (name.clone(), (value.clone(), false)))
            .collect();
        for scope in &self.local_scopes {
            for (name, value) in scope {
                visible.insert(name.clone(), (value.clone(), true));
            }
        }
        visible
            .into_iter()
            .map(|(name, (value, local))| (name, value, local))
            .collect()
    }

    /// Lazily removes elapsed `-uN` values. Runs are synchronous, so checking
    /// at execution/expansion boundaries gives scripts mIRC-visible expiry
    /// without one background task per value.
    pub(crate) fn purge_expired(&mut self) {
        let now = std::time::Instant::now();
        let expired_vars: Vec<String> = self
            .var_expiry
            .iter()
            .filter(|(_, expiry)| expiry.expired(now))
            .map(|(name, _)| name.clone())
            .collect();
        for name in expired_vars {
            self.var_expiry.remove(&name);
            self.vars.remove(&name);
        }

        let expired_items: Vec<(String, String)> = self
            .hash_expiry
            .iter()
            .filter(|(_, expiry)| expiry.expired(now))
            .map(|(key, _)| key.clone())
            .collect();
        for (table, item) in expired_items {
            self.hash_expiry.remove(&(table.clone(), item.clone()));
            if let Some(hash) = self.hashes.get_mut(&table) {
                hash.remove(&item);
            }
        }
    }

    /// Switches a departure event from mIRC's intentionally delayed roster/IAL
    /// view to the post-event snapshot after any handler calls `/updatenl`.
    pub(crate) fn refresh_nicklist_state(&mut self) {
        let updated = self
            .state
            .pending_nicklist_update
            .as_ref()
            .filter(|pending| pending.is_active())
            .map(|pending| pending.updated.clone());
        if let Some(updated) = updated {
            self.state = updated;
        }
    }

    /// Runs one `$hfind(..., command)` callback with the matched item exposed as
    /// `$1-`. `/halt` stops the search without leaking a halted state into the
    /// surrounding alias/event.
    pub(super) fn run_hfind_callback(&mut self, command: &str, item: &str) -> bool {
        let saved_params = std::mem::replace(&mut self.event.params, vec![item.to_string()]);
        let saved_halted = self.halted;
        self.halted = false;
        let command = self.expand(command);
        let body = super::parser::parse_body(command.trim());
        self.run(&body);
        let stopped = self.halted;
        self.halted = saved_halted;
        self.event.params = saved_params;
        stopped
    }

    fn cmd_updatenl(&mut self) {
        if let Some(pending) = &self.state.pending_nicklist_update {
            pending.activate();
            let updated = pending.updated.clone();
            self.state = updated;
        }
    }

    fn finish_timed_values(&mut self) {
        let vars: Vec<String> = self
            .var_expiry
            .iter()
            .filter(|(_, expiry)| matches!(expiry, TimedExpiry::EndOfRun))
            .map(|(name, _)| name.clone())
            .collect();
        for name in vars {
            self.var_expiry.remove(&name);
            self.vars.remove(&name);
        }

        let items: Vec<(String, String)> = self
            .hash_expiry
            .iter()
            .filter(|(_, expiry)| matches!(expiry, TimedExpiry::EndOfRun))
            .map(|(key, _)| key.clone())
            .collect();
        for (table, item) in items {
            self.hash_expiry.remove(&(table.clone(), item.clone()));
            if let Some(hash) = self.hashes.get_mut(&table) {
                hash.remove(&item);
            }
        }
        // Binary variables are scoped to one outer script execution in mIRC.
        self.bins.clear();
    }

    fn exec(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Command { name, args, .. } => {
                self.dispatch(name, args);
                self.run_pending_pipe_commands();
            }
            Stmt::If {
                branches,
                else_body,
                ..
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
            Stmt::While { cond, body, .. } => {
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
            Stmt::Label { .. } => {} // a jump target; no-op when reached normally
        }
    }

    fn run_pending_pipe_commands(&mut self) {
        while !self.halted && !self.pending_pipe_commands.is_empty() {
            let pending = std::mem::take(&mut self.pending_pipe_commands);
            for command in pending {
                if self.halted {
                    break;
                }
                let body = super::parser::parse_body(command.trim());
                self.run(&body);
            }
        }
    }

    // ---- command dispatch ----

    fn dispatch(&mut self, name: &str, raw_args: &str) {
        // mIRC permits assignment as a statement (`%name = value`) without a
        // `/set` command. The parser deliberately leaves this as a command
        // whose name starts with `%`; only claim it when an equals sign is
        // actually present so malformed/unknown commands retain their normal
        // fallback behaviour. Like an unswitched `/set`, an existing local is
        // updated before falling back to a global variable.
        if name.starts_with('%') {
            // Include the command-name token so evaluation brackets can build
            // a dynamic target (`%base [ $+ suffix ] = value`) without
            // dereferencing the completed variable name before assignment.
            let assignment = self
                .expand_evaluation_brackets(split_top_level(&format!("{name} {raw_args}")))
                .join(" ");
            let (target, tail) = assignment
                .split_once(char::is_whitespace)
                .unwrap_or((assignment.as_str(), ""));
            let key = target.strip_prefix('%').unwrap_or("");
            if let Some(value) = tail
                .trim_start()
                .strip_prefix('=')
                .filter(|_| !key.is_empty())
            {
                let mut value = self.expand(value.trim_start());
                value = try_var_math(&value).unwrap_or(value);
                let is_local = self.set_visible_var(key.to_string(), value);
                if !is_local {
                    update_timed_expiry(self.var_expiry, key.to_string(), "");
                }
                return;
            }
        }
        let lname = name.to_ascii_lowercase();
        // mIRC's silent prefix: `.command` runs the command but suppresses its
        // output. We don't echo command output anyway, so just drop a leading
        // dot — otherwise `.timer`, `.msg`, `.notice`, … fail to match and get
        // mis-sent to the server as a raw line. The dot also sets `$show` to
        // `$false` inside a called alias.
        let silent = lname.starts_with('.');
        let lname = lname.strip_prefix('.').unwrap_or(lname.as_str());
        // User aliases override built-in commands in mIRC. A leading `!`
        // explicitly bypasses the alias and invokes the built-in/server command.
        // Resolve this before the built-in match so aliases named join/msg/mode
        // behave like aliases with otherwise-unknown names.
        let bypass_alias = lname.starts_with('!');
        let lname = lname.strip_prefix('!').unwrap_or(lname);
        if !bypass_alias {
            if let Some((body, source, source_line)) = self
                .script
                .find_active_alias_from(lname, self.vars, &self.event.script_source)
                .map(|alias| (alias.body.clone(), alias.source.clone(), alias.source_line))
            {
                let params = split_params(&self.expand(raw_args));
                let saved = self.caller;
                let saved_show = self.show;
                self.caller = "command";
                self.show = !silent;
                let ret =
                    self.call_named_alias_in_source(lname, &body, params, &source, source_line);
                self.vars.insert(RESULT_KEY.to_string(), ret);
                self.caller = saved;
                self.show = saved_show;
                return;
            }
        }
        match lname {
            "echo" => self.cmd_echo(raw_args),
            "toolbar" => self.cmd_toolbar(raw_args),
            "panel" => self.cmd_panel(raw_args),
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
                    self.actions
                        .push(Action::Send(format!("NOTICE {target} :{text}")));
                }
            }
            "me" => {
                let text = self.expand(raw_args);
                let target = self.reply_target();
                if !target.is_empty() {
                    self.actions.push(Action::Send(format!(
                        "PRIVMSG {target} :\u{1}ACTION {text}\u{1}"
                    )));
                }
            }
            "describe" => {
                let (target, text) = self.split_target(raw_args);
                if !target.is_empty() {
                    self.actions.push(Action::Send(format!(
                        "PRIVMSG {target} :\u{1}ACTION {text}\u{1}"
                    )));
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
                let ch = if ch.is_empty() {
                    self.event.chan.clone()
                } else {
                    ch
                };
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
                self.actions
                    .push(Action::Send(format!("TOPIC {target} :{text}")));
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
                    self.actions
                        .push(Action::Send(format!("INVITE {nick} {chan}")));
                }
            }
            "hop" => {
                // /hop [#channel] — cycle the channel (part then rejoin).
                let ch = self.expand(raw_args);
                let ch = if ch.is_empty() {
                    self.event.chan.clone()
                } else {
                    ch
                };
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
                    self.actions
                        .push(Action::Send(format!("PRIVMSG @{chan} :{text}")));
                }
            }
            "onotice" => {
                let (chan, text) = self.split_target(raw_args);
                if chan.starts_with('#') && !text.is_empty() {
                    self.actions
                        .push(Action::Send(format!("NOTICE @{chan} :{text}")));
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
                    self.actions
                        .push(Action::Send(format!("NOTICE {nick} :\u{1}{body}\u{1}")));
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
                for scope in &mut self.local_scopes {
                    scope.clear();
                }
                self.vars.retain(|k, _| k.starts_with('\u{0}'));
                self.var_expiry.retain(|k, _| k.starts_with('\u{0}'));
            }
            "anick" => self.set_identity("anick", raw_args),
            "mnick" => self.set_identity("mnick", raw_args),
            "fullname" => self.set_identity("fullname", raw_args),
            "flushini" | "saveini" => {
                // No-op: jIRC writes INI/JSON to disk immediately (no cache).
            }
            "reload" => self.actions.push(Action::ReloadScripts),
            "dcc" => {
                let args = self.expand(raw_args);
                self.actions.push(Action::Dcc { args });
            }
            "fserve" => {
                let args = split_params(&self.expand(raw_args));
                if args.len() >= 3 {
                    if let Ok(max_gets) = args[1].parse::<usize>() {
                        self.actions.push(Action::Fserve {
                            nick: args[0].clone(),
                            max_gets,
                            home: args[2].clone(),
                            welcome: args.get(3).cloned(),
                        });
                    }
                }
            }
            // /scon N command  — run `command` on the Nth connection.
            // /scid cid command — run `command` on the connection with that cid.
            // The number is evaluated now; the command runs in the target's context.
            "scon" | "scid" => {
                let raw = raw_args.trim();
                let (sel, rest) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
                let sel = self.expand(sel);
                let target = if lname == "scon" {
                    sel.trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|i| self.conns.entries.get(i))
                        .map(|(_, s)| s.clone())
                } else {
                    sel.trim()
                        .parse::<u32>()
                        .ok()
                        .and_then(|cid| self.conns.entries.iter().find(|(c, _)| *c == cid))
                        .map(|(_, s)| s.clone())
                };
                if let (Some(server_id), false) = (target, rest.is_empty()) {
                    self.actions.push(Action::RunOn {
                        server_id,
                        command: rest.to_string(),
                    });
                }
            }
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
            "parseline" => self.cmd_parseline(raw_args),
            "alias" => {
                // `/alias [-l] [filename] <name> [command]`. The definition is
                // evaluated once; `$!name` stores `$name` for invocation time.
                let mut rest = raw_args.trim();
                let mut local = false;
                if rest
                    .split_whitespace()
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("-l"))
                {
                    local = true;
                    rest = rest
                        .split_once(char::is_whitespace)
                        .map(|(_, tail)| tail.trim_start())
                        .unwrap_or("");
                }
                let (first, tail) = take_file_arg(rest).unwrap_or_default();
                let filename_form = first.to_ascii_lowercase().ends_with(".mrc")
                    || first.to_ascii_lowercase().ends_with(".txt");
                let (file, name, command_tail) = if filename_form {
                    let (name, command_tail) = take_file_arg(tail).unwrap_or_default();
                    (Some(first), name, command_tail)
                } else {
                    (None, first, tail)
                };
                let name = name.trim_start_matches('/').to_string();
                let command =
                    (!command_tail.trim().is_empty()).then(|| self.expand(command_tail.trim()));
                if !name.is_empty() {
                    self.actions.push(Action::DefineAlias {
                        name,
                        command,
                        file,
                        local,
                    });
                }
            }
            "inc" => self.cmd_incdec(raw_args, 1),
            "dec" => self.cmd_incdec(raw_args, -1),
            "write" => self.cmd_write(raw_args),
            "filter" => self.cmd_filter(raw_args),
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
            "webview" => self.cmd_webview(raw_args),
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
                    rest = rest
                        .split_once(char::is_whitespace)
                        .map(|(_, r)| r)
                        .unwrap_or("")
                        .trim();
                }
                if let Some((src, dst)) = rest.split_once(char::is_whitespace) {
                    let _ = std::fs::copy(
                        sandbox_path(&self.data_dir, src.trim()),
                        sandbox_path(&self.data_dir, dst.trim()),
                    );
                }
            }
            "server" => self.cmd_server(raw_args),
            "sockopen" => self.cmd_sockopen(raw_args),
            "sockudp" => self.cmd_sockudp(raw_args),
            "sockwrite" => self.cmd_sockwrite(raw_args),
            "sockclose" => {
                self.event.sock_error = 0;
                let name = self.expand(raw_args.trim());
                if name.is_empty() {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
                } else if let Some(error) = self.sockets.close(&name) {
                    self.event.sock_error = error;
                } else {
                    self.actions.push(Action::SockClose { name });
                }
            }
            "socklisten" => self.cmd_socklisten(raw_args),
            "sockaccept" => {
                self.event.sock_error = 0;
                let expanded = self.expand(raw_args);
                let mut toks = expanded.split_whitespace().peekable();
                let mut nodelay = false;
                while toks.peek().is_some_and(|token| token.starts_with('-')) {
                    let switches = toks.next().unwrap().trim_start_matches('-');
                    if switches.chars().any(|flag| flag != 'n') {
                        self.event.sock_error = WSA_INVALID_ARGUMENT;
                        return;
                    }
                    nodelay |= switches.contains('n');
                }
                if let Some(name) = toks.next() {
                    // $sockname (the listener) identifies whose pending connection.
                    let listener = self.event.chan.clone();
                    if let Some(error) = self.sockets.accept(name, &listener, nodelay) {
                        self.event.sock_error = error;
                    }
                } else {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
                }
            }
            "sockmark" => self.cmd_sockmark(raw_args),
            "socklist" => self.cmd_socklist(raw_args),
            "sockrename" => {
                self.event.sock_error = 0;
                let expanded = self.expand(raw_args);
                let mut toks = expanded.split_whitespace();
                if let (Some(name), Some(newname)) = (toks.next(), toks.next()) {
                    if let Some(error) = self.sockets.rename(name, newname) {
                        self.event.sock_error = error;
                    } else {
                        self.actions.push(Action::SockRename {
                            name: name.to_string(),
                            newname: newname.to_string(),
                        });
                    }
                } else {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
                }
            }
            "sockpause" => {
                self.event.sock_error = 0;
                let expanded = self.expand(raw_args);
                let resume = expanded
                    .split_whitespace()
                    .take_while(|t| t.starts_with('-'))
                    .any(|t| t.contains('r'));
                if let Some(name) = expanded.split_whitespace().find(|t| !t.starts_with('-')) {
                    if let Some(error) = self.sockets.pause(name, resume) {
                        self.event.sock_error = error;
                    } else {
                        self.actions.push(Action::SockPause {
                            name: name.to_string(),
                            resume,
                        });
                    }
                } else {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
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
            "auser" => self.cmd_auser(raw_args),
            "guser" => self.cmd_guser(raw_args),
            "ruser" => self.cmd_ruser(raw_args),
            "iuser" => self.cmd_iuser(raw_args),
            "aop" => self.cmd_autolist(crate::script::users::AutoKind::Aop, raw_args),
            "avoice" => self.cmd_autolist(crate::script::users::AutoKind::Avoice, raw_args),
            "protect" => self.cmd_autolist(crate::script::users::AutoKind::Protect, raw_args),
            "ban" => self.cmd_ban(raw_args, true),
            "unban" => self.cmd_ban(raw_args, false),
            "query" => self.cmd_query(raw_args),
            "play" => self.cmd_play(raw_args),
            "timers" => self.cmd_timers(raw_args),
            s if s.starts_with("timer") => {
                let name = s.strip_prefix("timer").unwrap_or("").to_string();
                self.cmd_timer(&name, raw_args);
            }
            "halt" => {
                self.halted = true;
            }
            // `/haltdef` suppresses mIRC's default event display without
            // stopping the remainder of this handler.
            "haltdef" => {
                self.event.default_halted = true;
            }
            "return" | "returnex" => {
                let value = self.expand(raw_args);
                // Ordinary /return passes through mIRC's command-token
                // whitespace normalization. /returnex is the intentionally
                // space-preserving variant used by custom identifiers and
                // $regsubex replacement aliases.
                self.ret = Some(if lname == "return" {
                    // mIRC normalizes the ordinary space byte (0x20), not
                    // arbitrary control characters. Binary-building aliases
                    // legitimately return values such as $chr(11).
                    value
                        .split(' ')
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    value
                });
                self.halted = true;
            }
            "ial" => self.cmd_ial(raw_args),
            "ialclear" => self.cmd_ialclear(raw_args),
            "ialfill" => self.cmd_ialfill(raw_args),
            "ialmark" => self.cmd_ialmark(raw_args),
            "updatenl" => self.cmd_updatenl(),
            // We evaluate any parameters (for identifier side effects) and stop.
            // `/run` is deliberately a no-op — jIRC never launches programs.
            "clearall" | "close" | "sline" | "cline" | "fline" | "renwin" | "titlebar"
            | "editbox" | "linesep" | "background" | "color" | "font" | "flash" | "beep"
            | "ebeeps" | "speak" | "splay" | "sound" | "run" | "url" | "dns" | "debug" | "log"
            | "logview" | "timestamp" | "donotdisturb" | "menubar" | "switchbar" | "treebar"
            | "mdi" | "save" | "loadbuf" | "savebuf" | "showmirc" | "maximize" | "minimize"
            | "creq" | "sreq" | "clipboard" | "resetidle" => {
                let _ = self.expand(raw_args);
            }
            _ => {
                // Unknown client command: pass it to the IRC server. When `!`
                // bypassed an alias, send the command without the prefix.
                let args = self.expand(raw_args);
                let line = if args.is_empty() {
                    lname.to_ascii_uppercase()
                } else {
                    format!("{} {}", lname.to_ascii_uppercase(), args)
                };
                self.actions.push(Action::Send(line));
            }
        }
    }

    /// `/ial [on|off]` — per-connection and reset to on by each new session.
    fn cmd_ial(&mut self, raw: &str) {
        match self.expand(raw).trim().to_ascii_lowercase().as_str() {
            "on" => self.actions.push(Action::Send("\u{0}IAL ON".into())),
            "off" => self.actions.push(Action::Send("\u{0}IAL OFF".into())),
            _ => {}
        }
    }

    /// `/ialclear [nick]` — clear all entries or one nickname locally.
    fn cmd_ialclear(&mut self, raw: &str) {
        let nick = self.expand(raw);
        let nick = nick.split_whitespace().next().unwrap_or("");
        let control = if nick.is_empty() {
            "\u{0}IAL CLEAR".to_string()
        } else {
            format!("\u{0}IAL CLEAR {nick}")
        };
        self.actions.push(Action::Send(control));
    }

    /// `/ialfill [-f] #channel` — avoid an unnecessary WHO when every roster
    /// nick already has an address, unless forced. WHOX supplies account/away/
    /// gecos fields when the server advertises its ISUPPORT token.
    fn cmd_ialfill(&mut self, raw: &str) {
        if !self.state.ial_enabled {
            return;
        }
        let expanded = self.expand(raw);
        let force = expanded
            .split_whitespace()
            .any(|token| token.starts_with('-') && token[1..].contains('f'));
        let channel = expanded
            .split_whitespace()
            .rev()
            .find_map(|token| {
                self.state
                    .isupport
                    .channel_target(token)
                    .map(str::to_string)
            })
            .or_else(|| {
                self.state
                    .isupport
                    .channel_target(&self.active)
                    .map(str::to_string)
            });
        let Some(channel) = channel else {
            return;
        };
        let complete = self
            .state
            .channels
            .iter()
            .find(|view| self.state.isupport.names_equal(&view.name, &channel))
            .is_some_and(|view| {
                !view.nicks.is_empty()
                    && view.nicks.iter().all(|nick| {
                        self.state
                            .ial
                            .iter()
                            .any(|(known, _)| self.state.isupport.names_equal(known, nick))
                    })
            });
        if complete && !force {
            return;
        }
        let line = if self.state.isupport.whox {
            format!("WHO {channel} %acdfhlnrstu,995")
        } else {
            format!("WHO {channel}")
        };
        self.actions.push(Action::Send(line));
    }

    /// `/ialmark -nrw <nick> [name] [text]`. Marks are applied by the live
    /// connection so all later scripts/timers see the updated snapshot.
    fn cmd_ialmark(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut tokens = expanded.split_whitespace();
        let first = tokens.next().unwrap_or("");
        let (flags, nick) = if first.starts_with('-') {
            (&first[1..], tokens.next().unwrap_or(""))
        } else {
            ("", first)
        };
        if nick.is_empty() {
            return;
        }
        let named = flags.contains('n');
        let name = if named {
            tokens.next().unwrap_or("default")
        } else {
            "default"
        };
        let text = tokens.collect::<Vec<_>>().join(" ");
        let remove = flags.contains('r');
        let wildcard = remove && named && flags.contains('w');
        self.actions.push(Action::Send(format!(
            "\u{0}IAL MARK\t{}\t{}\t{nick}\t{name}\t{text}",
            u8::from(remove),
            u8::from(wildcard)
        )));
    }

    fn send_privmsg(&mut self, target: &str, text: &str) {
        self.actions
            .push(Action::Send(format!("PRIVMSG {target} :{text}")));
    }

    /// `/parseline -iotbqpnuN <text|&binvar>` replacement/queue operation.
    /// Text is converted to wire bytes here because the conversion depends on
    /// direction: incoming UTF text represents undecoded bytes, while outgoing
    /// UTF text is encoded for the server. Binary variables are always exact.
    fn cmd_parseline(&mut self, raw: &str) {
        let raw = raw.trim();
        let (switches, value) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
        if !switches.starts_with('-') || value.trim().is_empty() {
            return;
        }

        let mut direction = "";
        let mut binary = false;
        let mut has_type = false;
        let mut queue = false;
        let mut trigger = false;
        let mut append_crlf = false;
        let mut utf8 = true;
        let chars: Vec<char> = switches.trim_start_matches('-').chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                'i' => direction = "in",
                'o' => direction = "out",
                't' => {
                    binary = false;
                    has_type = true;
                }
                'b' => {
                    binary = true;
                    has_type = true;
                }
                'q' => queue = true,
                'p' => trigger = true,
                'n' => append_crlf = true,
                'u' => {
                    if let Some(next) = chars.get(i + 1) {
                        if *next == '0' || *next == '1' {
                            utf8 = *next == '1';
                            i += 1;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if direction.is_empty() || !has_type || (trigger && !queue) {
            return;
        }

        let bytes = if binary {
            let name = self.expand(value.trim());
            let Some(bytes) = self.bins.get(&name).cloned() else {
                return;
            };
            bytes
        } else {
            let text = self.expand(value.trim());
            if (direction == "in" && utf8) || (direction == "out" && !utf8) {
                byte_string_bytes(&text)
            } else {
                text.into_bytes()
            }
        };
        self.actions.push(Action::ParseLine {
            direction: direction.to_string(),
            bytes,
            queue,
            trigger,
            append_crlf,
            utf8,
        });
    }

    /// `/nickserv`, `/chanserv`, `/memoserv` (and `/ns`, `/cs`, `/ms`) — send a
    /// PRIVMSG to the named service.
    fn send_service(&mut self, service: &str, raw: &str) {
        let msg = self.expand(raw);
        if !msg.is_empty() {
            self.actions
                .push(Action::Send(format!("PRIVMSG {service} :{msg}")));
        }
    }

    /// Runs an alias body with `params` as `$1..`, isolating `$1..`, the halt
    /// flag, and the return value from the caller. Returns the `/return` value
    /// (empty if none). A bare `/halt` still propagates to stop the caller.
    pub fn call_alias(&mut self, body: &[Stmt], params: Vec<String>) -> String {
        let source = self.event.script_source.clone();
        let source_line = self.event.script_line;
        self.call_alias_in_source(body, params, &source, source_line)
    }

    /// Calls an alias while switching the current script-file identity for the
    /// duration of its frame. This is what makes `alias -l` visible only to
    /// commands executing from the defining file.
    pub fn call_alias_in_source(
        &mut self,
        body: &[Stmt],
        params: Vec<String>,
        source: &str,
        source_line: usize,
    ) -> String {
        let saved_params = std::mem::replace(&mut self.event.params, params);
        let saved_source = std::mem::replace(&mut self.event.script_source, source.to_string());
        let saved_line = std::mem::replace(&mut self.event.script_line, source_line);
        let saved_halted = std::mem::replace(&mut self.halted, false);
        let saved_ret = self.ret.take();
        let saved_goto = self.goto.take(); // goto is routine-local
                                           // A pipe produced while expanding the alias invocation belongs after
                                           // the whole alias command, not after the alias body's first statement.
        let saved_pipe_commands = std::mem::take(&mut self.pending_pipe_commands);
        self.local_scopes.push(HashMap::new());
        self.run(body);
        self.local_scopes.pop();
        self.pending_pipe_commands.clear();
        self.pending_pipe_commands = saved_pipe_commands;
        self.goto = saved_goto;
        let returned = self.ret.is_some();
        let result = self.ret.take().unwrap_or_default();
        let halted_in_alias = self.halted;
        self.event.params = saved_params;
        self.event.script_source = saved_source;
        self.event.script_line = saved_line;
        self.ret = saved_ret;
        // Restore the caller's halt state, but let a non-return /halt bubble up.
        self.halted = saved_halted || (halted_in_alias && !returned);
        result
    }

    pub fn call_named_alias_in_source(
        &mut self,
        name: &str,
        body: &[Stmt],
        params: Vec<String>,
        source: &str,
        source_line: usize,
    ) -> String {
        if self
            .alias_stack
            .iter()
            .any(|active| active.eq_ignore_ascii_case(name))
        {
            return String::new();
        }
        self.alias_stack.push(name.to_string());
        let result = self.call_alias_in_source(body, params, source, source_line);
        self.alias_stack.pop();
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
    /// line as the value verbatim (no `=`, no comma splitting). `-uN` removes
    /// the value after N seconds (`-u0`: when the outer script run finishes),
    /// and `-k` preserves an existing lifetime.
    fn cmd_set(&mut self, raw: &str, is_var: bool) {
        let (flags, rest) = split_switches(raw);
        // Pre-evaluate square-bracket groups before separating the target name
        // from its value. This preserves the completed `%name` as a literal
        // assignment target instead of dereferencing it, and implements the
        // documented `/set %base [ $+ suffix ] value` form.
        let rest = self
            .expand_evaluation_brackets(split_top_level(rest))
            .join(" ");
        let rest = rest.as_str();
        let flags_lower = flags.to_ascii_lowercase();
        // mIRC applies one math operation by default (`var %a 1 + 2` -> 3);
        // -n and -p suppress it (keep the value literal).
        let no_math = flags_lower.contains('n') || flags_lower.contains('p');
        let force_global = flags_lower.contains('g');
        let force_local = !force_global && flags_lower.contains('l');
        if is_var {
            for decl in split_top_commas(rest) {
                let decl = decl.trim();
                if decl.is_empty() {
                    continue;
                }
                // The name runs to the first space or '='; then an optional '='
                // assignment (mIRC's `=` is optional and stripped, unlike /set).
                let name_end = decl
                    .find(|c: char| c.is_whitespace() || c == '=')
                    .unwrap_or(decl.len());
                let key = decl[..name_end].trim_start_matches('%').trim().to_string();
                if key.is_empty() {
                    continue;
                }
                let vraw = decl[name_end..].trim_start();
                let vraw = vraw.strip_prefix('=').map(str::trim_start).unwrap_or(vraw);
                let mut value = self.expand(vraw);
                if !no_math {
                    value = try_var_math(&value).unwrap_or(value);
                }
                if force_global {
                    update_timed_expiry(self.var_expiry, key.clone(), flags);
                    self.vars.insert(key, value);
                } else {
                    // `/var` is routine-local by default (and with `-l`). Its
                    // frame disappears when this alias returns independently
                    // of the persistent global store.
                    self.set_local_var(key, value);
                }
            }
        } else if let Some((name, value)) = rest.split_once(char::is_whitespace) {
            let key = name.trim_start_matches('%').to_string();
            let mut value = self.expand(value.trim());
            if !no_math {
                value = try_var_math(&value).unwrap_or(value);
            }
            let is_local = if force_global {
                self.vars.insert(key.clone(), value);
                false
            } else if force_local {
                self.set_local_var(key.clone(), value);
                true
            } else {
                // An existing routine-local variable takes precedence over a
                // same-named global unless `-g` explicitly overrides it.
                self.set_visible_var(key.clone(), value)
            };
            if !is_local {
                update_timed_expiry(self.var_expiry, key, flags);
            }
        } else if !rest.is_empty() {
            let key = rest.trim_start_matches('%').to_string();
            let is_local = if force_global {
                self.vars.insert(key.clone(), String::new());
                false
            } else if force_local {
                self.set_local_var(key.clone(), String::new());
                true
            } else {
                self.set_visible_var(key.clone(), String::new())
            };
            if !is_local {
                update_timed_expiry(self.var_expiry, key, flags);
            }
        }
    }

    /// `/unset [-sgl] <%var> [%var2 ...]` — remove one or more variables;
    /// names may be wildcards (`/unset %prefix.*`).
    /// `/auser [-a] <levels> <nick|address> [info]` — add/edit a user-list entry.
    fn cmd_auser(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let add = flags.contains('a');
        let mut it = rest.splitn(3, char::is_whitespace);
        let levels = self.expand(it.next().unwrap_or("").trim());
        let address = self.expand(it.next().unwrap_or("").trim());
        let info = self.expand(it.next().unwrap_or("").trim());
        if !levels.is_empty() && !address.is_empty() {
            self.users.add(&levels, &address, &info, add);
        }
    }

    /// `/guser [-a] <levels> <nick> [type] [info]` — like /auser, but looks the
    /// nick's address up in the IAL and stores it masked by [type] (default 6).
    fn cmd_guser(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let add = flags.contains('a');
        let mut it = rest.splitn(3, char::is_whitespace);
        let levels = self.expand(it.next().unwrap_or("").trim());
        let nick = self.expand(it.next().unwrap_or("").trim());
        let tail = it.next().unwrap_or("").trim();
        // The optional [type] is a leading numeric token; the rest is [info].
        let (typ, info) = match tail.split_once(char::is_whitespace) {
            Some((t, r)) if t.parse::<u32>().is_ok() => (t, r.trim()),
            _ if !tail.is_empty() && tail.parse::<u32>().is_ok() => (tail, ""),
            _ => ("", tail),
        };
        let info = self.expand(info);
        if levels.is_empty() || nick.is_empty() {
            return;
        }
        let who = self.state.isupport.casefold(&nick);
        let address = match self
            .state
            .ial
            .iter()
            .find(|(n, _)| self.state.isupport.names_equal(n, &who))
        {
            Some((_, full)) => {
                let t: u32 = if typ.is_empty() {
                    6
                } else {
                    typ.parse().unwrap_or(6)
                };
                crate::script::ident::mask_address(full, t)
            }
            None => nick.clone(),
        };
        self.users.add(&levels, &address, &info, add);
    }

    /// `/ruser [levels] <nick|address>` — remove a user, or just the listed
    /// (numeric) levels. The first token is treated as a levels list only when it
    /// looks numeric; otherwise it's the address and the whole entry is removed.
    fn cmd_ruser(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        let rest = self.expand(rest);
        let mut toks = rest.split_whitespace();
        let first = toks.next().unwrap_or("");
        let (levels, address) = match toks.next() {
            Some(second) if is_level_list(first) => (first, second),
            _ => ("", first),
        };
        if !address.is_empty() {
            self.users.remove(levels, address);
        }
    }

    /// `/iuser <nick|address> [info]` — set an existing entry's info string.
    fn cmd_iuser(&mut self, raw: &str) {
        let rest = self.expand(raw.trim());
        if let Some((address, info)) = rest.split_once(char::is_whitespace) {
            self.users.set_info(address.trim(), info.trim());
        } else if !rest.is_empty() {
            self.users.set_info(rest.trim(), "");
        }
    }

    /// `/aop`/`/avoice`/`/protect` `[-rwal] <on|off|nick|address> [#channels] [network]`.
    fn cmd_autolist(&mut self, kind: crate::script::users::AutoKind, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let rest = self.expand(rest);
        let mut toks = rest.split_whitespace();
        let Some(first) = toks.next() else { return };
        if first.eq_ignore_ascii_case("on") {
            self.users.auto_toggle(kind, true);
            return;
        }
        if first.eq_ignore_ascii_case("off") {
            self.users.auto_toggle(kind, false);
            return;
        }
        if flags.contains('r') {
            self.users.auto_remove(kind, first);
            return;
        }
        if flags.contains('l') {
            return; // listing is a display-only no-op
        }
        // Optional [#channels] (comma-separated) and an explicit [network]; `-w`
        // means all networks, otherwise it defaults to the current one.
        let extra: Vec<&str> = toks.collect();
        let channels: Vec<String> = extra
            .iter()
            .find(|t| t.starts_with('#'))
            .map(|t| t.split(',').map(String::from).collect())
            .unwrap_or_default();
        let network = if flags.contains('w') {
            String::new()
        } else {
            extra
                .iter()
                .find(|t| !t.starts_with('#') && t.parse::<u32>().is_err())
                .map(|t| t.to_string())
                .unwrap_or_else(|| self.network.to_string())
        };
        self.users.auto_add(kind, first, channels, network);
    }

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
                // Without an explicit -g/-l switch, mIRC resolves variables in
                // the same local-first order as reads. Remove matches from the
                // nearest local frame that has any; hidden globals stay intact.
                if let Some(scope) = self
                    .local_scopes
                    .iter_mut()
                    .rev()
                    .find(|scope| scope.keys().any(|k| wildcard_match(pat, k)))
                {
                    scope.retain(|k, _| !wildcard_match(pat, k));
                    continue;
                }
                let keys: Vec<String> = self
                    .vars
                    .keys()
                    .filter(|k| wildcard_match(pat, k))
                    .cloned()
                    .collect();
                for k in keys {
                    self.vars.remove(&k);
                    self.var_expiry.remove(&k);
                }
            } else {
                if let Some(scope) = self
                    .local_scopes
                    .iter_mut()
                    .rev()
                    .find(|scope| scope.contains_key(pat))
                {
                    scope.remove(pat);
                    continue;
                }
                self.vars.remove(pat);
                self.var_expiry.remove(pat);
            }
        }
    }

    fn cmd_incdec(&mut self, raw: &str, sign: i64) {
        let (flags, rest) = split_switches(raw);
        let mut it = rest.split_whitespace();
        let Some(name) = it.next() else { return };
        let key = name.trim_start_matches('%').to_string();
        let by: i64 = it
            .next()
            .map(|s| self.expand(s))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let cur: i64 = self
            .var_value(&key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let is_local = self.set_visible_var(key.clone(), (cur + sign * by).to_string());
        if !is_local {
            update_timed_expiry(self.var_expiry, key, flags);
        }
    }

    /// `/write [-cidnalNsNwNrNmN] <file> [text]` — mIRC-compatible line
    /// insert/replace/delete/search operations, sandboxed to the script data
    /// directory. Text files are written with CRLF separators like mIRC.
    fn cmd_write(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim_start();
        let mut switches = String::new();
        if let Some(body) = rest.strip_prefix('-') {
            let end = body.find(char::is_whitespace).unwrap_or(body.len());
            switches = body[..end].to_string();
            rest = body[end..].trim_start();
        }
        let control_switches = write_control_switches(&switches);
        let Some((file, text)) = take_file_arg(rest) else {
            return;
        };
        if file.is_empty() {
            return;
        }
        let path = sandbox_path(&self.data_dir, &file);
        let mut content = if control_switches.contains('c') {
            String::new()
        } else {
            std::fs::read_to_string(&path).unwrap_or_default()
        };
        let had_final_newline = content.ends_with('\n') || content.ends_with('\r');
        let mut lines: Vec<String> = content
            .lines()
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect();

        let mut line_number =
            write_numeric_switch(control_switches, 'l').map(|n| n.max(1) as usize);
        let search = write_search_switch(&switches);
        if line_number.is_none() {
            if let Some((mode, pattern)) = search.as_ref() {
                line_number = lines
                    .iter()
                    .position(|line| match mode {
                        's' => line
                            .get(..pattern.len())
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(pattern)),
                        'w' => wildcard_match(pattern, line),
                        'W' => wildcard_match(line, pattern),
                        'r' => ident::mirc_regex_is_match(line, pattern),
                        'R' => ident::mirc_regex_is_match(pattern, line),
                        _ => false,
                    })
                    .map(|index| index + 1);
            }
        }

        let writes_line = !control_switches.contains('d');
        if control_switches.contains('d') {
            let index = line_number.unwrap_or(lines.len());
            if index > 0 && index <= lines.len() {
                lines.remove(index - 1);
            }
        } else if control_switches.contains('i') {
            let index = line_number.unwrap_or(lines.len() + 1).max(1);
            lines.insert((index - 1).min(lines.len()), text.to_string());
        } else if control_switches.contains('a') && line_number.is_some() {
            let index = line_number.unwrap();
            if let Some(line) = index.checked_sub(1).and_then(|i| lines.get_mut(i)) {
                line.push_str(text);
            }
        } else if let Some(index) = line_number {
            if index <= lines.len() {
                lines[index - 1] = text.to_string();
            } else {
                lines.resize(index - 1, String::new());
                lines.push(text.to_string());
            }
        } else {
            lines.push(text.to_string());
        }

        content = lines.join("\r\n");
        let no_final_newline = control_switches.contains('n')
            || write_numeric_switch(control_switches, 'm') == Some(2);
        let force_separator = write_numeric_switch(control_switches, 'm') == Some(1);
        let add_final_newline = if writes_line {
            content.is_empty()
                || force_separator
                || had_final_newline
                || !control_switches.contains('a')
        } else {
            had_final_newline && !content.is_empty()
        };
        if !no_final_newline && add_final_newline {
            content.push_str("\r\n");
        }
        let _ = std::fs::write(&path, content);
    }

    /// `/filter` over sandboxed files and custom `@windows`. This implements
    /// the compatibility-critical file/window, wildcard/regex/exclude/range,
    /// line-number, clear, sort, and alias-output forms. Dialog/listbox-only
    /// switches are accepted but have no source in the reduced jIRC UI model.
    fn cmd_filter(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim_start();
        let mut switches = String::new();
        if let Some(body) = rest.strip_prefix('-') {
            let end = body.find(char::is_whitespace).unwrap_or(body.len());
            switches = body[..end].to_string();
            rest = body[end..].trim_start();
        }

        let mut range = None;
        if switches.contains('r') {
            if let Some((token, more)) = take_file_arg(rest) {
                if let Some((from, to)) = token.split_once('-') {
                    if let (Ok(from), Ok(to)) = (from.parse::<usize>(), to.parse::<usize>()) {
                        range = Some((from.max(1), to.max(from.max(1))));
                        rest = more;
                    }
                }
            }
        }

        // `-t` column sorting consumes `column separator` before the endpoints.
        let mut column_sort = None;
        if switches.contains('t') {
            if let Some((column, more)) = take_file_arg(rest) {
                if let Some((separator, tail)) = take_file_arg(more) {
                    column_sort = Some((
                        column.parse::<usize>().unwrap_or(1).max(1),
                        separator
                            .parse::<u32>()
                            .ok()
                            .and_then(char::from_u32)
                            .unwrap_or(' '),
                    ));
                    rest = tail;
                }
            }
        }

        let Some((input, more)) = take_file_arg(rest) else {
            return;
        };
        let Some((output, more)) = take_file_arg(more) else {
            return;
        };
        let (sort_alias, match_text) = if switches.contains('a') {
            let Some((alias, tail)) = take_file_arg(more) else {
                return;
            };
            (Some(alias), tail.to_string())
        } else {
            (None, more.to_string())
        };
        let match_text = match_text.trim();

        let type_marks: Vec<char> = switches
            .chars()
            .filter(|c| matches!(c, 'f' | 'w'))
            .collect();
        let input_is_window = type_marks
            .first()
            .map_or_else(|| input.starts_with('@'), |kind| *kind == 'w');
        let output_is_window = type_marks
            .get(1)
            .map_or_else(|| output.starts_with('@'), |kind| *kind == 'w');
        let input_lines: Vec<String> = if input_is_window {
            self.windows
                .get(&input)
                .map(|window| window.lines.clone())
                .unwrap_or_default()
        } else {
            std::fs::read_to_string(sandbox_path(&self.data_dir, &input))
                .unwrap_or_default()
                .lines()
                .map(|line| line.trim_end_matches('\r').to_string())
                .collect()
        };

        let (from, to) = range.unwrap_or((1, input_lines.len()));
        let regex = switches.contains('g');
        let exclude = switches.contains('x');
        let strip = switches.contains('b');
        let number = switches.contains('n');
        let mut selected = Vec::new();
        for (index, original) in input_lines.iter().enumerate() {
            let n = index + 1;
            if n < from || n > to {
                continue;
            }
            let candidate = if strip {
                ident::strip_codes_opts(original, "")
            } else {
                original.clone()
            };
            let matched = if regex {
                ident::mirc_regex_is_match(&candidate, match_text)
            } else {
                wildcard_match(match_text, &candidate)
            };
            if matched != exclude {
                selected.push(if number {
                    format!("{n} {original}")
                } else {
                    original.clone()
                });
            }
        }

        if column_sort.is_some() || switches.contains('u') || switches.contains('e') {
            let descending = switches.contains('e');
            let numeric = switches.contains('u');
            let (column, separator) = column_sort.unwrap_or((1, ' '));
            selected.sort_by(|left, right| {
                let key = |line: &str| -> String {
                    line.split(separator)
                        .filter(|value| !value.is_empty())
                        .nth(column - 1)
                        .unwrap_or("")
                        .to_string()
                };
                let order = if numeric {
                    key(left)
                        .trim()
                        .parse::<f64>()
                        .unwrap_or(0.0)
                        .partial_cmp(&key(right).trim().parse::<f64>().unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    key(left)
                        .to_ascii_lowercase()
                        .cmp(&key(right).to_ascii_lowercase())
                };
                if descending {
                    order.reverse()
                } else {
                    order
                }
            });
        }

        // Alias comparison sorting is inherently script-driven. Use stable
        // insertion sort so the alias sees deterministic `$1`/`$2` pairs.
        if let Some(alias) = sort_alias {
            let definition = self
                .script
                .find_active_alias_from(&alias, self.vars, &self.event.script_source)
                .cloned();
            let Some(definition) = definition else {
                self.vars.insert(FILTERED_KEY.to_string(), "0".to_string());
                return;
            };
            let mut sorted: Vec<String> = Vec::with_capacity(selected.len());
            for line in selected {
                let mut at = sorted.len();
                while at > 0 {
                    let cmp = self
                        .call_named_alias_in_source(
                            &alias,
                            &definition.body,
                            vec![line.clone(), sorted[at - 1].clone()],
                            &definition.source,
                            definition.source_line,
                        )
                        .parse::<i64>()
                        .unwrap_or(0);
                    if cmp >= 0 {
                        break;
                    }
                    at -= 1;
                }
                sorted.insert(at, line);
            }
            selected = sorted;
        }

        self.vars
            .insert(FILTERED_KEY.to_string(), selected.len().to_string());
        if switches.contains('k') {
            for line in selected {
                self.dispatch(&output, &line);
                if self.halted {
                    break;
                }
            }
            return;
        }
        if output_is_window {
            if !self.windows.exists(&output) {
                self.windows
                    .open(&output, super::window::WindowKind::Listbox, &output);
                self.actions.push(Action::WindowOpen {
                    name: output.clone(),
                    kind: "listbox".to_string(),
                    title: output.clone(),
                });
            }
            if switches.contains('c') {
                self.windows.clear(&output);
                self.actions.push(Action::WindowLine {
                    name: output.clone(),
                    op: "clear".to_string(),
                    n: 0,
                    text: String::new(),
                });
            }
            for line in selected {
                self.windows.aline(&output, &line);
                self.actions.push(Action::WindowLine {
                    name: output.clone(),
                    op: "add".to_string(),
                    n: 0,
                    text: line,
                });
            }
        } else {
            let path = sandbox_path(&self.data_dir, &output);
            let mut prior = if switches.contains('c') {
                String::new()
            } else {
                std::fs::read_to_string(&path).unwrap_or_default()
            };
            if !prior.is_empty()
                && !prior.ends_with('\r')
                && !prior.ends_with('\n')
                && !selected.is_empty()
            {
                prior.push_str("\r\n");
            }
            prior.push_str(&selected.join("\r\n"));
            if !selected.is_empty() {
                prior.push_str("\r\n");
            }
            let _ = std::fs::write(path, prior);
        }
    }

    /// `/writeini [-n] <file> <section> <item> <value>` — set an INI item.
    fn cmd_writeini(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        while rest.starts_with('-') {
            rest = rest
                .split_once(char::is_whitespace)
                .map(|(_, r)| r)
                .unwrap_or("")
                .trim();
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
            let (sw, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
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
        let (mut binary, mut newline) = (false, false);
        while let Some(stripped) = rest.strip_prefix('-') {
            let (sw, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
            if sw.contains('n') {
                newline = true;
            }
            if sw.contains('b') {
                binary = true;
            }
            rest = more.trim();
        }
        let mut parts = rest.splitn(2, char::is_whitespace);
        if let Some(name) = parts.next() {
            let value = parts.next().unwrap_or("").trim();
            let data = if binary {
                self.bins.get(value).cloned().unwrap_or_default()
            } else {
                value.as_bytes().to_vec()
            };
            self.files.write(name, &data, newline);
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
            let (flags, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
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
            let (sw, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
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
            .map(|b| {
                b.iter()
                    .skip(s.saturating_sub(1))
                    .take(m)
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        self.bins.set(dest, n, &slice, false);
    }

    /// `/breplace <&binvar> <old> <new> [<old> <new>…]` — replace matching byte
    /// values throughout &binvar.
    fn cmd_breplace(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut parts = expanded.split_whitespace();
        let Some(name) = parts.next() else { return };
        let nums: Vec<u8> = parts
            .filter_map(|t| t.parse::<u16>().ok())
            .map(|n| n as u8)
            .collect();
        let pairs: Vec<(u8, u8)> = nums
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0], c[1]))
            .collect();
        if pairs.is_empty() {
            return;
        }
        let Some(mut bytes) = self.bins.get(name).cloned() else {
            return;
        };
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
        let (Some(file), Some(len)) = (parts.next(), parts.next()) else {
            return;
        };
        let path = sandbox_path(&self.data_dir, file.trim());
        let len: u64 = len.trim().parse().unwrap_or(0);
        if let Ok(f) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(&path)
        {
            let _ = f.set_len(len);
        }
    }

    /// `/bread [-ta] <file> <S> <N> <&binvar>` — file offsets are zero-based.
    fn cmd_bread(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        while let Some(stripped) = rest.strip_prefix('-') {
            let (_, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
            rest = more.trim(); // -t/-a are accepted; byte-count behavior stays exact.
        }
        let p: Vec<&str> = rest.split_whitespace().collect();
        if p.len() < 4 {
            return;
        }
        let path = sandbox_path(&self.data_dir, p[0]);
        let s: usize = p[1].trim().parse().unwrap_or(0);
        let n: usize = p[2].trim().parse().unwrap_or(0);
        let name = p[3];
        if let Ok(data) = std::fs::read(&path) {
            let slice: Vec<u8> = data.iter().skip(s).take(n).copied().collect();
            self.bins.unset(name);
            self.bins.set(name, 1, &slice, false);
        }
    }

    /// `/bwrite [-tac] <file> <S> [N] <text|%var|&binvar>` — zero-based S;
    /// S=-1 appends, N omitted/-1 writes all, and -c truncates after the data.
    fn cmd_bwrite(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let (mut text, mut chop) = (false, false);
        while let Some(stripped) = rest.strip_prefix('-') {
            let (flags, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
            text |= flags.contains('t');
            chop |= flags.contains('c');
            rest = more.trim(); // -a is accepted; Rust strings are already UTF-8.
        }
        let mut head = rest.splitn(3, char::is_whitespace);
        let (Some(file), Some(offset), Some(tail)) = (head.next(), head.next(), head.next()) else {
            return;
        };
        let path = sandbox_path(&self.data_dir, file);
        let s: i64 = offset.trim().parse().unwrap_or(0);
        let (n, data_arg) = match tail.split_once(char::is_whitespace) {
            Some((candidate, data)) if candidate.parse::<i64>().is_ok() => {
                (candidate.parse::<i64>().unwrap(), data.trim())
            }
            _ => (-1, tail.trim()),
        };
        // A known &binvar contributes its bytes; otherwise the literal text.
        let data: Vec<u8> =
            if !text && data_arg.starts_with('&') && self.bins.get(data_arg).is_some() {
                self.bins.get(data_arg).cloned().unwrap_or_default()
            } else {
                data_arg.as_bytes().to_vec()
            };
        let to_write: Vec<u8> = if n < 0 {
            data
        } else {
            data.into_iter().take(n as usize).collect()
        };
        let mut content = std::fs::read(&path).unwrap_or_default();
        let start = if s < 0 { content.len() } else { s as usize };
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
        if chop {
            content.truncate(start + to_write.len());
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
            let (sw, more) = stripped
                .split_once(char::is_whitespace)
                .unwrap_or((stripped, ""));
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
            self.actions.push(Action::WindowClose {
                name: name.to_string(),
            });
        } else {
            self.windows.open(name, kind, name);
            self.actions.push(Action::WindowOpen {
                name: name.to_string(),
                kind: kind.as_str().to_string(),
                title: name.to_string(),
            });
        }
    }

    /// Native browser windows (jIRC extension):
    /// `/webview -o <name> <profile> <width> <height> <url> [title]`
    /// `/webview -n <name> <url>` (navigate), `-k <name> <url>` (cookies),
    /// `-f <name>` (focus), and `-c <name>` (close).
    fn cmd_webview(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut head = expanded.trim().splitn(2, char::is_whitespace);
        let switch = head.next().unwrap_or("").to_ascii_lowercase();
        let rest = head.next().unwrap_or("").trim();
        let clean = |value: &str| value.trim().trim_matches('"').to_string();
        match switch.as_str() {
            "-o" => {
                let mut parts = rest.splitn(6, char::is_whitespace);
                let (Some(name), Some(profile), Some(width), Some(height), Some(url)) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    return;
                };
                let width = width.parse::<u32>().unwrap_or(980);
                let height = height.parse::<u32>().unwrap_or(720);
                let name = clean(name);
                let profile = clean(profile);
                let url = clean(url);
                if name.is_empty() || profile.is_empty() || url.is_empty() {
                    return;
                }
                let title = parts
                    .next()
                    .map(clean)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| name.clone());
                self.actions.push(Action::WebviewOpen {
                    name,
                    profile,
                    width,
                    height,
                    url,
                    title,
                });
            }
            "-n" | "-k" => {
                let mut parts = rest.splitn(2, char::is_whitespace);
                let name = clean(parts.next().unwrap_or(""));
                let url = clean(parts.next().unwrap_or(""));
                if name.is_empty() || url.is_empty() {
                    return;
                }
                if switch == "-n" {
                    self.actions.push(Action::WebviewNavigate { name, url });
                } else {
                    self.actions.push(Action::WebviewCookies { name, url });
                }
            }
            "-f" | "-c" => {
                let name = clean(rest.split_whitespace().next().unwrap_or(""));
                if name.is_empty() {
                    return;
                }
                if switch == "-f" {
                    self.actions.push(Action::WebviewFocus { name });
                } else {
                    self.actions.push(Action::WebviewClose { name });
                }
            }
            _ => {}
        }
    }

    /// `/aline @w text`, `/rline @w N text`, `/iline @w N text`, `/dline @w N`.
    fn cmd_window_line(&mut self, raw: &str, op: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        // Skip a leading switch (e.g. `/aline -p @w text` colour switch).
        if rest.starts_with('-') {
            rest = rest
                .split_once(char::is_whitespace)
                .map(|(_, r)| r.trim())
                .unwrap_or("");
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

    /// `/sockopen [-deswap64nt] [bindip] <name> <host> <port>`.
    fn cmd_sockopen(&mut self, raw: &str) {
        self.event.sock_error = 0;
        let expanded = self.expand(raw);
        let mut toks = expanded.split_whitespace().peekable();
        let mut flags = String::new();
        while toks.peek().is_some_and(|token| token.starts_with('-')) {
            flags.push_str(toks.next().unwrap().trim_start_matches('-'));
        }
        if flags.chars().any(|flag| !"deswap64nt".contains(flag))
            || (flags.contains('4') && flags.contains('6'))
        {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let tls = flags.contains('e');
        let accept_invalid = flags.contains('a') || flags.contains('s');
        let certificate_flags = flags.chars().any(|flag| "swap".contains(flag));
        if (flags.contains('t') && flags.chars().any(|flag| flag != 't'))
            || (certificate_flags && !tls)
        {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let nodelay = flags.contains('n');
        let ip_version = if flags.contains('6') {
            6
        } else if flags.contains('4') {
            4
        } else {
            0
        };
        let bind_ip = if flags.contains('d') {
            let Some(bind_ip) = toks.next() else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            };
            bind_ip.to_string()
        } else {
            String::new()
        };
        if flags.contains('t') {
            if let Some(name) = toks.next() {
                if let Some(error) = self.sockets.starttls(name) {
                    self.event.sock_error = error;
                    if error != 0 {
                        self.actions.push(Action::SockError {
                            kind: "SOCKOPEN".to_string(),
                            name: name.to_string(),
                            error,
                        });
                    }
                }
            } else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
            }
            return;
        }
        if let (Some(name), Some(host), Some(port)) = (toks.next(), toks.next(), toks.next()) {
            if let Ok(port) = port.parse::<u16>() {
                let reservation_id =
                    match self.sockets.reserve_open(name, host, port, tls, &bind_ip) {
                        Some(Ok(id)) => id,
                        Some(Err(error)) => {
                            self.event.sock_error = error;
                            self.actions.push(Action::SockError {
                                kind: "SOCKOPEN".to_string(),
                                name: name.to_string(),
                                error,
                            });
                            return;
                        }
                        None => 0,
                    };
                self.actions.push(Action::SockOpen {
                    name: name.to_string(),
                    host: host.to_string(),
                    port,
                    tls,
                    accept_invalid,
                    bind_ip,
                    nodelay,
                    ip_version,
                    reservation_id,
                });
            } else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
            }
        } else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
        }
    }

    /// `/sockudp [-bntkdu] [bindip] <name> [local-port] <ip> <port>
    /// [numbytes] [data]`.
    fn cmd_sockudp(&mut self, raw: &str) {
        self.event.sock_error = 0;
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let mut flags = String::new();
        while rest.starts_with('-') {
            let (sw, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            flags.push_str(sw.trim_start_matches('-'));
            rest = more.trim();
        }
        if flags.chars().any(|flag| !"bntkdu".contains(flag)) {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let mut toks: Vec<&str> = rest.split_whitespace().collect();
        let bind_ip = if flags.contains('d') {
            if toks.is_empty() {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            }
            toks.remove(0).to_string()
        } else {
            String::new()
        };
        if toks.len() < 3 {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let name = toks.remove(0).to_string();
        if name.is_empty() {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let Some(ip_pos) = toks
            .iter()
            .position(|t| t.parse::<std::net::IpAddr>().is_ok())
        else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        };
        let local_port = if ip_pos == 1 {
            let Ok(port) = toks.remove(0).parse::<u16>() else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            };
            port
        } else if ip_pos == 0 {
            0
        } else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        };
        let dest_ip = toks.remove(0).to_string();
        let Some(dest_port) = toks.first().copied() else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        };
        let Ok(dest_port) = dest_port.parse::<u16>() else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        };
        toks.remove(0);
        let max_bytes = if flags.contains('b') {
            if toks.is_empty() {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            }
            let Ok(count) = toks.remove(0).parse::<usize>() else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            };
            Some(count)
        } else {
            None
        };
        let data_raw = toks.join(" ");
        let is_binvar = !flags.contains('t')
            && data_raw.starts_with('&')
            && !data_raw.contains(char::is_whitespace);
        if !flags.contains('t')
            && data_raw.starts_with('&')
            && data_raw.contains(char::is_whitespace)
        {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let mut data = if is_binvar {
            self.bins.get(&data_raw).cloned().unwrap_or_default()
        } else {
            data_raw.into_bytes()
        };
        if let Some(max) = max_bytes {
            data.truncate(max);
        }
        if flags.contains('n') && !is_binvar && !data.ends_with(b"\r\n") {
            data.extend_from_slice(b"\r\n");
        }
        let reservation_id = match self
            .sockets
            .reserve_udp(&name, &bind_ip, local_port, &dest_ip, dest_port)
        {
            Some(Ok(id)) => id,
            Some(Err(error)) => {
                self.event.sock_error = error;
                self.actions.push(Action::SockError {
                    kind: "SOCKWRITE".to_string(),
                    name,
                    error,
                });
                return;
            }
            None => 0,
        };
        self.actions.push(Action::SockUdp {
            name,
            bind_ip,
            local_port,
            dest_ip,
            dest_port,
            data,
            keep: flags.contains('k'),
            dual_stack: flags.contains('u'),
            reservation_id,
        });
    }

    /// `/sockwrite [-tnba] <name> [numbytes] <text|%var|&binvar>`.
    fn cmd_sockwrite(&mut self, raw: &str) {
        self.event.sock_error = 0;
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let mut flags = String::new();
        while rest.starts_with('-') {
            let (sw, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            flags.push_str(sw.trim_start_matches('-'));
            rest = more.trim();
        }
        if flags.chars().any(|flag| !"tnba".contains(flag)) {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let (name_tok, mut data_tok) = match rest.split_once(char::is_whitespace) {
            Some((n, t)) => (n, t.trim()),
            None => (rest, ""),
        };
        let name = name_tok.to_string();
        if name.is_empty() {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let max_bytes = if flags.contains('b') {
            match data_tok.split_once(char::is_whitespace) {
                Some((count, data)) => {
                    data_tok = data.trim();
                    let Ok(count) = count.parse::<usize>() else {
                        self.event.sock_error = WSA_INVALID_ARGUMENT;
                        return;
                    };
                    Some(count)
                }
                None => {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
                    return;
                }
            }
        } else {
            None
        };
        let is_binvar = !flags.contains('t')
            && data_tok.starts_with('&')
            && !data_tok.contains(char::is_whitespace);
        if !flags.contains('t')
            && data_tok.starts_with('&')
            && data_tok.contains(char::is_whitespace)
        {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let mut data = if is_binvar {
            self.bins.get(data_tok).cloned().unwrap_or_default()
        } else {
            let text = data_tok.to_string();
            if flags.contains('a') && text.chars().all(|c| (c as u32) <= 255) {
                text.chars().map(|c| c as u8).collect()
            } else {
                text.into_bytes()
            }
        };
        if let Some(max) = max_bytes {
            data.truncate(max);
        }
        if flags.contains('n') && !is_binvar && !data.ends_with(b"\r\n") {
            data.extend_from_slice(b"\r\n");
        }
        if let Some(result) = self.sockets.write(&name, &data) {
            self.event.sock_error = result.error;
            let mut failures = result.failures;
            if result.error != 0 && failures.is_empty() {
                failures.push((name.clone(), result.error));
            }
            for (failed_name, error) in failures {
                if error != 0 {
                    self.actions.push(Action::SockError {
                        kind: "SOCKWRITE".to_string(),
                        name: failed_name,
                        error,
                    });
                }
            }
        } else {
            self.actions.push(Action::SockWrite { name, data });
        }
    }

    /// `/socklisten [-options] <name> [port]` — bind a listening socket. With no
    /// (or 0) port the OS assigns one, readable via `$sock(name).port`.
    /// `/server [-m|-mn…] <host> <port> [password]` — connect the native IRC
    /// client to a server (or a local bridge). `-m` requests a new window; other
    /// mIRC switches are accepted and ignored.
    fn cmd_server(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let new_window = expanded
            .split_whitespace()
            .take_while(|t| t.starts_with('-'))
            .any(|t| t[1..].contains('m'));
        let mut toks = expanded.split_whitespace().filter(|t| !t.starts_with('-'));
        let Some(host) = toks.next().map(|s| s.to_string()) else {
            return;
        };
        let port = toks
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(6667);
        let pass = toks.collect::<Vec<_>>().join(" ");
        self.actions.push(Action::Server {
            host,
            port,
            pass,
            new_window,
        });
    }

    /// Cross-platform subset of mIRC `/toolbar`: add, delete, clear, and update
    /// tooltip/icon/command. Quoted fields preserve spaces.
    fn cmd_toolbar(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let (switches, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if !switches.starts_with('-') {
            return;
        }
        rest = more.trim_start();
        let flags = switches.trim_start_matches('-');
        if flags.contains('c') {
            self.actions.push(Action::Toolbar {
                op: "clear".into(),
                name: String::new(),
                tooltip: String::new(),
                icon: String::new(),
                command: String::new(),
                source: self.event.script_source.clone(),
            });
            return;
        }
        let Some((name, tail)) = take_file_arg(rest) else {
            return;
        };
        if flags.contains('d') {
            self.actions.push(Action::Toolbar {
                op: "delete".into(),
                name,
                tooltip: String::new(),
                icon: String::new(),
                command: String::new(),
                source: self.event.script_source.clone(),
            });
            return;
        }
        if flags.contains('a') || flags.contains('i') {
            let Some((tooltip, next)) = take_file_arg(tail) else {
                return;
            };
            let Some((icon, next)) = take_file_arg(next) else {
                return;
            };
            let Some((command, _)) = take_file_arg(next) else {
                return;
            };
            self.actions.push(Action::Toolbar {
                op: "upsert".into(),
                name,
                tooltip,
                icon,
                command,
                source: self.event.script_source.clone(),
            });
            return;
        }
        let (op, value) = if flags.contains('t') {
            ("tooltip", take_file_arg(tail).map(|v| v.0))
        } else if flags.contains('p') {
            ("icon", take_file_arg(tail).map(|v| v.0))
        } else if flags.contains('l') {
            ("command", take_file_arg(tail).map(|v| v.0))
        } else {
            return;
        };
        if let Some(value) = value {
            self.actions.push(Action::Toolbar {
                op: op.into(),
                name,
                tooltip: (op == "tooltip")
                    .then_some(value.clone())
                    .unwrap_or_default(),
                icon: (op == "icon").then_some(value.clone()).unwrap_or_default(),
                command: (op == "command").then_some(value).unwrap_or_default(),
                source: self.event.script_source.clone(),
            });
        }
    }

    /// jIRC's safe script-panel API:
    /// `/panel -a name "title"`, `-t name id "text"`,
    /// `-b name id "label" "/command $!1"`, `-d name [id]`, `-c`.
    fn cmd_panel(&mut self, raw: &str) {
        let expanded = self.expand(raw);
        let mut rest = expanded.trim();
        let (switches, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if !switches.starts_with('-') {
            return;
        }
        rest = more.trim_start();
        let flags = switches.trim_start_matches('-');
        if flags.contains('c') {
            self.actions.push(Action::Panel {
                op: "clear".into(),
                panel: String::new(),
                id: String::new(),
                label: String::new(),
                value: String::new(),
                command: String::new(),
                source: self.event.script_source.clone(),
            });
            return;
        }
        let Some((panel, tail)) = take_file_arg(rest) else {
            return;
        };
        if flags.contains('a') {
            let Some((title, _)) = take_file_arg(tail) else {
                return;
            };
            self.actions.push(Action::Panel {
                op: "upsert".into(),
                panel,
                id: String::new(),
                label: title,
                value: String::new(),
                command: String::new(),
                source: self.event.script_source.clone(),
            });
            return;
        }
        if flags.contains('d') {
            let id = take_file_arg(tail).map(|item| item.0).unwrap_or_default();
            self.actions.push(Action::Panel {
                op: if id.is_empty() {
                    "deletePanel".into()
                } else {
                    "deleteItem".into()
                },
                panel,
                id,
                label: String::new(),
                value: String::new(),
                command: String::new(),
                source: self.event.script_source.clone(),
            });
            return;
        }
        let Some((id, tail)) = take_file_arg(tail) else {
            return;
        };
        if flags.contains('t') {
            let Some((text, _)) = take_file_arg(tail) else {
                return;
            };
            self.actions.push(Action::Panel {
                op: "text".into(),
                panel,
                id,
                label: String::new(),
                value: text,
                command: String::new(),
                source: self.event.script_source.clone(),
            });
        } else if flags.contains('b') {
            let Some((label, tail)) = take_file_arg(tail) else {
                return;
            };
            let Some((command, _)) = take_file_arg(tail) else {
                return;
            };
            self.actions.push(Action::Panel {
                op: "button".into(),
                panel,
                id,
                label,
                value: String::new(),
                command,
                source: self.event.script_source.clone(),
            });
        }
    }

    fn cmd_socklisten(&mut self, raw: &str) {
        self.event.sock_error = 0;
        let expanded = self.expand(raw);
        // mIRC syntax: /socklisten [-dnpu] [bindip] <name> [port]. `-p` is
        // accepted for compatibility; jIRC does not create UPnP mappings.
        let mut toks = expanded.split_whitespace().peekable();
        let mut flags = String::new();
        while toks.peek().is_some_and(|token| token.starts_with('-')) {
            flags.push_str(toks.next().unwrap().trim_start_matches('-'));
        }
        if flags.chars().any(|flag| !"dnpu".contains(flag)) {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let bind_ip = if flags.contains('d') {
            let Some(bind_ip) = toks.next() else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            };
            bind_ip
        } else {
            ""
        };
        let Some(name) = toks.next() else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        };
        let port = match toks.next() {
            Some(port) => match port.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
                    return;
                }
            },
            None => 0,
        };
        let name = name.to_string();
        // Bind now (so $sock(name).port is readable inline); the accept loop is
        // started at apply-time with the owning connection's context.
        match self.sockets.listen_reserved(
            bind_ip,
            &name,
            port,
            flags.contains('n'),
            flags.contains('u'),
        ) {
            Some(Ok((_, listener_id))) => {
                self.actions.push(Action::SockListen { name, listener_id })
            }
            None => self.actions.push(Action::SockListen {
                name,
                listener_id: 0,
            }),
            Some(Err(error)) => self.event.sock_error = error,
        }
    }

    /// `/sockmark <name> [text]` — set (or clear) a socket's mark, read back via
    /// `$sock(name).mark`.
    fn cmd_sockmark(&mut self, raw: &str) {
        self.event.sock_error = 0;
        let expanded = self.expand(raw);
        let trimmed = expanded.trim();
        let (name, mark) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        if name.is_empty() {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
        } else if let Some(error) = self.sockets.set_mark(name, mark.trim()) {
            self.event.sock_error = error;
        } else {
            self.actions.push(Action::SockMark {
                name: name.to_string(),
                mark: mark.trim().to_string(),
            });
        }
    }

    /// `/socklist [-tul] [name]` — echoes the list of open sockets.
    fn cmd_socklist(&mut self, raw: &str) {
        self.event.sock_error = 0;
        let filter = self.expand(raw);
        let target = self.reply_target();
        let lines = self.sockets.list(filter.trim());
        self.actions.push(Action::Echo {
            target: target.clone(),
            text: format!("Sock List - {} socket(s)", lines.len()),
        });
        for line in lines {
            self.actions.push(Action::Echo {
                target: target.clone(),
                text: line,
            });
        }
    }

    /// `/sockread [-fn] [numbytes] <%var|&binvar>` — consumes the socket's
    /// receive queue and updates `$sockbr`.
    fn cmd_sockread(&mut self, raw: &str) {
        self.event.sock_error = 0;
        self.vars.insert(SOCK_BR_KEY.to_string(), "0".to_string());
        let mut force = false;
        let mut line_switch = false;
        let mut num_bytes = None;
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        let Some(target) = tokens.last().copied() else {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        };
        if !target.starts_with('%') && !target.starts_with('&') {
            self.event.sock_error = WSA_INVALID_ARGUMENT;
            return;
        }
        let option_tokens: Vec<String> = tokens[..tokens.len() - 1]
            .iter()
            .flat_map(|token| {
                self.expand(token)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        for tok in option_tokens {
            if let Some(switches) = tok.strip_prefix('-') {
                if switches.chars().any(|flag| !"fn".contains(flag)) {
                    self.event.sock_error = WSA_INVALID_ARGUMENT;
                    return;
                }
                force |= switches.contains('f');
                line_switch |= switches.contains('n');
                continue;
            }
            // Expand only the byte-count argument. Expanding the destination
            // would replace `%var` with its value instead of assigning to it.
            let Ok(n) = tok.parse::<usize>() else {
                self.event.sock_error = WSA_INVALID_ARGUMENT;
                return;
            };
            num_bytes = Some(n);
        }
        let binary = target.starts_with('&');
        let options = SocketReadOptions {
            binary,
            force,
            line: !binary || line_switch,
            max_bytes: num_bytes.unwrap_or(4096),
        };
        let result = match self.sockets.read(&self.event.chan, options) {
            Some(Ok(result)) => result,
            Some(Err(error)) => {
                self.event.sock_error = error;
                return;
            }
            None => {
                // Unit-test/no-backend events carry one legacy inline line. Treat it
                // as one read, and clear both representations so a following text
                // read cannot consume the same bytes after a binary read (or vice versa).
                let data = if self.event.sock_bytes.is_empty() {
                    self.event.text.as_bytes().to_vec()
                } else {
                    std::mem::take(&mut self.event.sock_bytes)
                };
                self.event.text.clear();
                self.event.sock_bytes.clear();
                SocketReadResult {
                    bytes_read: data.len(),
                    data,
                }
            }
        };
        if binary {
            self.bins.unset(target);
            self.bins.set(target, 1, &result.data, false);
        } else {
            let var = target.trim_start_matches('%').to_string();
            if var.is_empty() {
                return;
            }
            let line = match String::from_utf8(result.data) {
                Ok(text) => text,
                Err(e) => e.into_bytes().into_iter().map(|b| b as char).collect(),
            };
            let is_local = self.set_visible_var(var.clone(), line);
            if !is_local {
                self.var_expiry.remove(&var);
            }
        }
        self.vars
            .insert(SOCK_BR_KEY.to_string(), result.bytes_read.to_string());
    }

    /// `/dialog [-c] <name>` — open (or, with `-c`, close) a custom dialog.
    fn cmd_dialog(&mut self, raw: &str) {
        let toks: Vec<&str> = raw.split_whitespace().collect();
        let close = toks.iter().any(|t| *t == "-c");
        let Some(name) = toks.iter().find(|t| !t.starts_with('-')) else {
            return;
        };
        if close {
            self.actions.push(Action::DialogClose {
                name: name.to_string(),
            });
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

    /// `/hsave -s[bBniau] <table> <file> [section]` — save a hash table using
    /// mIRC's text, INI, 16-bit-index (`-b`), or 32-bit-index (`-B`) format.
    fn cmd_hsave(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let expanded = self.expand(rest);
        let Some((table, rest)) = take_file_arg(&expanded) else {
            return;
        };
        let Some(table) = super::hash::table_key(self.hashes, &table) else {
            return;
        };
        let Some((file, rest)) = take_file_arg(rest) else {
            return;
        };
        let Some(h) = self.hashes.get(&table) else {
            return;
        };
        let include_unset = flags.contains('u');
        let entries: Vec<(String, String)> = h
            .iter()
            .filter(|(item, _)| {
                include_unset
                    || !self
                        .hash_expiry
                        .contains_key(&(table.clone(), (*item).clone()))
            })
            .map(|(item, value)| (item.clone(), value.clone()))
            .collect();
        let format = hash_text_format(flags);
        let section = take_file_arg(rest)
            .map(|(section, _)| section)
            .filter(|section| !section.is_empty())
            .unwrap_or_else(|| table.clone());
        let binary_format = if flags.contains('i') {
            None
        } else if flags.contains('B') {
            Some(super::hash::BinaryFormat::U32)
        } else if flags.contains('b') {
            Some(super::hash::BinaryFormat::U16)
        } else {
            None
        };
        let Some(bytes) = binary_format
            .map(|binary| super::hash::save_binary(&entries, binary, flags.contains('n')))
            .unwrap_or_else(|| Some(super::hash::save(&entries, format, &section)))
        else {
            return;
        };
        let path = sandbox_path(&self.data_dir, &file);
        if flags.contains('a') {
            use std::io::Write;
            if let Ok(mut output) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = output.write_all(&bytes);
            }
        } else {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// `/hload -s[mN bBni] <table> <file> [section]` — load a table saved by
    /// `/hsave`. `-mN` creates a missing table and retains its slot count.
    fn cmd_hload(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let expanded = self.expand(rest);
        let Some((table, rest)) = take_file_arg(&expanded) else {
            return;
        };
        let Some((file, rest)) = take_file_arg(rest) else {
            return;
        };
        let table = super::hash::table_key(self.hashes, &table).unwrap_or(table);
        if !self.hashes.contains_key(&table) {
            if flags.contains('m') || flags.contains('M') {
                self.hashes.insert(table.clone(), HashMap::new());
                super::hash::set_slots(
                    self.hashes,
                    &table,
                    write_numeric_switch(flags, 'm').unwrap_or(100) as usize,
                );
            } else {
                return;
            }
        }
        let format = hash_text_format(flags);
        let section = take_file_arg(rest)
            .map(|(section, _)| section)
            .filter(|section| !section.is_empty())
            .unwrap_or_else(|| table.clone());
        if let Ok(content) = std::fs::read(sandbox_path(&self.data_dir, &file)) {
            let loaded = if flags.contains('i') {
                super::hash::load(&content, format, &section)
            } else if flags.contains('B') {
                super::hash::load_binary(
                    &content,
                    super::hash::BinaryFormat::U32,
                    flags.contains('n'),
                )
            } else if flags.contains('b') {
                super::hash::load_binary(
                    &content,
                    super::hash::BinaryFormat::U16,
                    flags.contains('n'),
                )
            } else {
                super::hash::load(&content, format, &section)
            };
            for (item, value) in loaded {
                let item = self
                    .hashes
                    .get(&table)
                    .and_then(|hash| super::hash::item_key(hash, &item))
                    .unwrap_or(item);
                self.hash_expiry.remove(&(table.clone(), item.clone()));
                self.hashes
                    .get_mut(&table)
                    .expect("table checked above")
                    .insert(item, value);
            }
        }
    }

    /// `/hmake [-s] <name> [slots]` — create an (empty) hash table. Slots are a
    /// sizing hint in mIRC; ignored here.
    fn cmd_hmake(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        let expanded = self.expand(rest);
        let mut args = expanded.split_whitespace();
        if let Some(table) = args.next() {
            let table = table.to_string();
            let slots = args.next().and_then(|n| n.parse().ok()).unwrap_or(100);
            if super::hash::table_key(self.hashes, &table).is_some() {
                return;
            }
            self.hashes.entry(table.clone()).or_default();
            super::hash::set_slots(self.hashes, &table, slots);
        }
    }

    /// `/hfree [-w] <name>` — delete a hash table (`-w`: name is a wildcard).
    fn cmd_hfree(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let wild = flags.contains('w');
        if let Some(table) = rest.split_whitespace().next() {
            let table = self.expand(table);
            if wild {
                let keys: Vec<String> = super::hash::table_names(self.hashes)
                    .into_iter()
                    .filter(|name| wildcard_match(&table, name))
                    .collect();
                for k in keys {
                    self.hashes.remove(&k);
                    super::hash::remove_slots(self.hashes, &k);
                    self.hash_expiry.retain(|(table, _), _| table != &k);
                }
            } else {
                let table = super::hash::table_key(self.hashes, &table).unwrap_or(table);
                self.hashes.remove(&table);
                super::hash::remove_slots(self.hashes, &table);
                self.hash_expiry.retain(|(name, _), _| name != &table);
            }
        }
    }

    /// `/hclear <name>` — remove every item but keep the (now empty) table.
    fn cmd_hclear(&mut self, raw: &str) {
        let (_flags, rest) = split_switches(raw);
        if let Some(table) = rest.split_whitespace().next() {
            let table = self.expand(table);
            let table = super::hash::table_key(self.hashes, &table).unwrap_or(table);
            if let Some(h) = self.hashes.get_mut(&table) {
                h.clear();
            }
            self.hash_expiry.retain(|(name, _), _| name != &table);
        }
    }

    /// `/hadd [-m] <table> <item> [value]` — set an item (`-m` makes the table
    /// if it doesn't exist; we always create-on-demand). Table and item names
    /// are expanded so variable keys match what `$hget` reads back.
    fn cmd_hadd(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        // Expand before splitting: in mIRC, the spaced `$+` operator can form an
        // argument (for example `hadd h state. $+ %sock ready`). Splitting the raw
        // text first incorrectly stores `state.` as the item and the joined item
        // suffix as part of the value.
        let expanded = self.expand(rest);
        let mut it = expanded.splitn(3, char::is_whitespace);
        let (table, item, value) = (it.next(), it.next(), it.next());
        if let (Some(table), Some(item)) = (table, item) {
            let requested_table = table.trim().to_string();
            let table =
                super::hash::table_key(self.hashes, &requested_table).unwrap_or(requested_table);
            let item = self
                .hashes
                .get(&table)
                .and_then(|hash| super::hash::item_key(hash, item.trim()))
                .unwrap_or_else(|| item.trim().to_string());
            update_timed_expiry(self.hash_expiry, (table.clone(), item.clone()), flags);
            let value = value.unwrap_or("").trim();
            let stored = if flags.contains('b') {
                let mut bytes = self.bins.get(value).cloned().unwrap_or_default();
                if flags.contains('c') {
                    if let Some(end) = bytes.iter().position(|byte| *byte == 0) {
                        bytes.truncate(end);
                    }
                    String::from_utf8(bytes).unwrap_or_else(|error| {
                        error
                            .into_bytes()
                            .into_iter()
                            .map(|byte| byte as char)
                            .collect()
                    })
                } else {
                    super::hash::binary_value(&bytes)
                }
            } else {
                value.to_string()
            };
            let is_new = !self.hashes.contains_key(&table);
            self.hashes
                .entry(table.clone())
                .or_default()
                .insert(item, stored);
            if is_new {
                super::hash::set_slots(
                    self.hashes,
                    &table,
                    write_numeric_switch(flags, 'm').unwrap_or(100) as usize,
                );
            }
        }
    }

    /// `/hdel [-w] <table> <item>` — remove an item (`-w`: item is a wildcard).
    fn cmd_hdel(&mut self, raw: &str) {
        let (flags, rest) = split_switches(raw);
        let wild = flags.contains('w');
        let expanded = self.expand(rest);
        let mut it = expanded.split_whitespace();
        if let (Some(table), Some(item)) = (it.next(), it.next()) {
            let Some(table) = super::hash::table_key(self.hashes, table) else {
                return;
            };
            if let Some(h) = self.hashes.get_mut(&table) {
                if wild {
                    let keys: Vec<String> = h
                        .keys()
                        .filter(|k| wildcard_match(item, k))
                        .cloned()
                        .collect();
                    for k in keys {
                        h.remove(&k);
                        self.hash_expiry.remove(&(table.clone(), k));
                    }
                } else {
                    if let Some(item) = super::hash::item_key(h, item) {
                        h.remove(&item);
                        self.hash_expiry.remove(&(table, item));
                    }
                }
            }
        }
    }

    /// `/hinc|/hdec [-switches] <table> <item> [n]` — add/subtract `n` (default
    /// 1) to a numeric hash item, creating the table/item if needed.
    fn cmd_hincdec(&mut self, raw: &str, sign: i64) {
        let (flags, rest) = split_switches(raw);
        let expanded = self.expand(rest);
        let mut it = expanded.splitn(3, char::is_whitespace);
        if let (Some(table), Some(item)) = (it.next(), it.next()) {
            let by: i64 = it
                .next()
                .map(str::trim)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            let requested_table = table.trim().to_string();
            let table =
                super::hash::table_key(self.hashes, &requested_table).unwrap_or(requested_table);
            let item = self
                .hashes
                .get(&table)
                .and_then(|hash| super::hash::item_key(hash, item.trim()))
                .unwrap_or_else(|| item.trim().to_string());
            update_timed_expiry(self.hash_expiry, (table.clone(), item.clone()), flags);
            let is_new = !self.hashes.contains_key(&table);
            let h = self.hashes.entry(table.clone()).or_default();
            let cur: i64 = h.get(&item).and_then(|v| v.parse().ok()).unwrap_or(0);
            h.insert(item, (cur + sign * by).to_string());
            if is_new {
                super::hash::set_slots(self.hashes, &table, 100);
            }
        }
    }

    /// `/tokenize <c> <text>` — split `text` by character code `c` into `$1, $2…`
    /// for the rest of the current routine. `c` of 32 (space) collapses runs.
    fn cmd_tokenize(&mut self, raw: &str) {
        let raw = raw.trim();
        let Some((c, rest)) = raw.split_once(char::is_whitespace) else {
            return;
        };
        let sep = self
            .expand(c)
            .trim()
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .unwrap_or(' ');
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
        let (chan, idx) = if let Some(bare) = self.state.isupport.channel_target(&toks[0]) {
            let display = self
                .state
                .channels
                .iter()
                .find(|channel| self.state.isupport.names_equal(&channel.name, bare))
                .map(|channel| channel.name.clone())
                .unwrap_or_else(|| bare.to_string());
            (display, 1)
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
            let who = self.state.isupport.casefold(target);
            match self
                .state
                .ial
                .iter()
                .find(|(n, _)| self.state.isupport.names_equal(n, &who))
            {
                Some((_, full)) => ident::mask_address(full, kind),
                None => format!("{target}!*@*"),
            }
        };
        let sign = if add { '+' } else { '-' };
        self.actions
            .push(Action::Send(format!("MODE {chan} {sign}b {mask}")));
    }

    /// mIRC `/timer[name] [-cdeomhipPrzN] [time] <reps> <interval> <command>`.
    /// The command is evaluated once now; `$!ident` and `$unsafe()` deliberately
    /// survive for the evaluation performed each time the timer fires.
    fn cmd_timer(&mut self, name: &str, raw: &str) {
        let raw = raw.trim();
        if raw.is_empty() {
            self.actions.push(Action::TimerList {
                target: self.reply_target(),
                name: if name.is_empty() { "*" } else { name }.to_string(),
            });
            return;
        }

        let mut tokens: Vec<&str> = raw.split_whitespace().collect();
        let mut idx = 0;
        let mut millis = false;
        let mut offline = false;
        let mut catch_up = false;
        let mut ordered = false;
        let mut high_resolution = false;
        let mut dynamic = false;
        let mut execute = false;
        let mut pause = None;
        let mut resume = false;
        while idx < tokens.len() {
            let switch = self.expand(tokens[idx]);
            if !switch.starts_with('-') || switch.len() == 1 {
                break;
            }
            for ch in switch[1..].chars() {
                match ch {
                    'c' => catch_up = true,
                    'd' => ordered = true,
                    'e' => execute = true,
                    'o' => offline = true,
                    'm' => millis = true,
                    'h' => {
                        millis = true;
                        high_resolution = true;
                    }
                    'i' => dynamic = true,
                    'p' => pause = Some(false),
                    'P' => pause = Some(true),
                    'r' => resume = true,
                    // -zN resets mIRC's Online Timer dialog, not a script timer.
                    'z' | '0' | '1' | '2' => {}
                    _ => {}
                }
            }
            idx += 1;
        }

        let pattern = if name.is_empty() { "*" } else { name };
        if execute {
            self.actions.push(Action::TimerExecute {
                name: pattern.to_string(),
            });
            return;
        }
        if let Some(countdown) = pause {
            self.actions.push(Action::TimerPause {
                name: pattern.to_string(),
                countdown,
            });
            return;
        }
        if resume {
            self.actions.push(Action::TimerResume {
                name: pattern.to_string(),
            });
            return;
        }

        if tokens
            .get(idx)
            .is_some_and(|v| v.eq_ignore_ascii_case("off"))
        {
            self.actions.push(Action::TimerStop {
                name: pattern.to_string(),
            });
            return;
        }

        let mut start_at = None;
        if let Some(candidate) = tokens.get(idx) {
            let candidate = self.expand(candidate);
            if super::timer::is_wall_clock_spec(&candidate) {
                start_at = Some(candidate);
                idx += 1;
            }
        }
        if tokens.len().saturating_sub(idx) < 3 {
            return;
        }
        let Ok(reps) = self.expand(tokens[idx]).trim().parse::<u32>() else {
            return;
        };
        let Ok(interval) = self.expand(tokens[idx + 1]).trim().parse::<f64>() else {
            return;
        };
        if !interval.is_finite() || interval < 0.0 {
            return;
        }
        let interval_ms = if millis {
            interval as u64
        } else {
            (interval * 1000.0) as u64
        };
        tokens.drain(..idx + 2);
        let command = self.expand_deferred(&tokens.join(" "));
        if command.trim().is_empty() {
            return;
        }
        let timer_name = if name.is_empty() {
            let mut used: Vec<String> = self
                .timers
                .snapshot()
                .into_iter()
                .map(|timer| timer.name)
                .collect();
            used.extend(self.actions.iter().filter_map(|action| match action {
                Action::Timer { name, .. } => Some(name.clone()),
                _ => None,
            }));
            (1_u64..)
                .map(|n| n.to_string())
                .find(|candidate| !used.iter().any(|n| n.eq_ignore_ascii_case(candidate)))
                .unwrap()
        } else {
            name.to_string()
        };
        self.vars.insert(LTIMER_KEY.to_string(), timer_name.clone());
        self.actions.push(Action::Timer {
            name: timer_name,
            reps,
            interval_ms,
            start_at,
            command,
            target: self.reply_target(),
            offline,
            catch_up,
            ordered,
            milliseconds: millis,
            high_resolution,
            dynamic,
            source: self.event.script_source.clone(),
        });
    }

    /// `/timers` lists active timers; `/timers off` stops them all.
    fn cmd_timers(&mut self, raw: &str) {
        if raw.trim().eq_ignore_ascii_case("off") {
            self.actions.push(Action::TimerStop {
                name: "*".to_string(),
            });
        } else {
            self.actions.push(Action::TimerList {
                target: self.reply_target(),
                name: "*".to_string(),
            });
        }
    }

    fn cmd_play(&mut self, raw: &str) {
        let args = self.expand(raw);
        // mIRC's -q/-m limits apply to requests made through remote events,
        // not commands typed by the local user or fired by a timer.
        let remote = !self.event.event.is_empty()
            && !self.event.nick.is_empty()
            && !self.event.nick.eq_ignore_ascii_case(self.my_nick);
        self.actions.push(Action::Play {
            args,
            current_target: self.reply_target(),
            remote,
            source: self.event.script_source.clone(),
        });
    }

    // ---- expansion ----

    /// Expands `%vars`, `$identifiers`, params, and the `$+` join operator.
    pub fn expand(&mut self, text: &str) -> String {
        let expanded = self.expand_inner(text);
        let mut segments = split_command_pipes(&expanded).into_iter();
        let first = decode_delayed(&segments.next().unwrap_or_default());
        self.pending_pipe_commands
            .extend(segments.map(|command| decode_delayed(&command)));
        first
    }

    /// Expands a command that will itself be evaluated later (notably a timer).
    /// `$unsafe()` data remains encoded through the deferred parser and is
    /// decoded by the later normal expansion.
    pub fn expand_deferred(&mut self, text: &str) -> String {
        self.expand_inner(text)
    }

    fn expand_inner(&mut self, text: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut join_next = false;
        // Split on spaces, but keep `$ident(a b c)` whole (spaces inside the
        // parentheses are part of the identifier's arguments).
        let tokens = self.expand_evaluation_brackets(split_top_level(text));
        let mut i = 0;
        while i < tokens.len() {
            let tok = &tokens[i];
            if tok == "$+" {
                join_next = true;
                i += 1;
                continue;
            }
            let v = self.eval_token(tok);
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
            i += 1;
        }
        parts.join(" ")
    }

    /// Pre-evaluates mIRC `[ ... ]` groups from the innermost group outward.
    /// A leading/trailing `$+` crosses the bracket boundary, which is how
    /// `% [ $+ [ $1 ] ]` constructs and then dereferences a dynamic variable.
    /// The symmetric `[ $+ value $+ ]` form is deliberately *not* an evaluation
    /// group: it is mIRC's common way to render literal square brackets, and is
    /// handled by the ordinary `$+` token pass below.
    fn expand_evaluation_brackets(&mut self, mut tokens: Vec<String>) -> Vec<String> {
        // Each replacement removes at least one evaluation-bracket pair. The
        // cap is defensive against values that themselves expand to bracket
        // syntax; real scripts cannot usefully nest anywhere near this depth.
        for _ in 0..64 {
            let Some(span) = evaluation_bracket_span(&tokens) else {
                break;
            };
            let mut inner = tokens[span.open + 1..span.close].to_vec();

            // Strip only the boundary-crossing join operators. Empty tokens
            // represent repeated source spaces, so locate the first/last
            // non-empty token rather than assuming adjacency.
            if span.join_left {
                if let Some(index) = inner.iter().position(|token| !token.is_empty()) {
                    inner.remove(index);
                }
            }
            if span.join_right {
                if let Some(index) = inner.iter().rposition(|token| !token.is_empty()) {
                    inner.remove(index);
                }
            }

            let mut value = self.expand_inner(&inner.join(" "));
            let mut replace_start = span.open;
            let mut replace_end = span.close;

            if span.join_left {
                if let Some(previous) = (0..span.open)
                    .rev()
                    .find(|index| !tokens[*index].is_empty())
                {
                    value.insert_str(0, &tokens[previous]);
                    replace_start = previous;
                }
            }
            if span.join_right {
                if let Some(next) =
                    (span.close + 1..tokens.len()).find(|index| !tokens[*index].is_empty())
                {
                    value.push_str(&tokens[next]);
                    replace_end = next;
                }
            }

            tokens.splice(replace_start..=replace_end, [value]);
        }
        tokens
    }

    /// Expands identifiers/vars within a single (space-free) token.
    fn eval_token(&mut self, tok: &str) -> String {
        // `$input` and other synchronous identifiers can keep a run open long
        // enough for timed values to expire between tokens.
        self.purge_expired();
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
                        out.push_str(self.var_value(&name).map(|s| s.as_str()).unwrap_or(""));
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
        // `$!name` — delayed evaluation: return the literal `$name` (evaluated zero
        // times; any `(args)`/`.prop` after it fall through as literal text). Bare
        // `$!` returns the last `$?`/`$input` answer.
        if chars.get(*i) == Some(&'!') {
            *i += 1;
            match chars.get(*i).copied() {
                Some('+') => {
                    *i += 1;
                    return "$+".to_string();
                }
                Some(c) if c.is_alphanumeric() || c == '_' => {
                    return format!("${}", read_name(chars, i));
                }
                _ => return self.vars.get(LASTINPUT_KEY).cloned().unwrap_or_default(),
            }
        }
        // `$$N` — a require prefix: like `$N`, but the script halts when the
        // parameter is empty. Only when a digit follows (a literal `$$`
        // elsewhere is left untouched).
        let require = chars.get(*i) == Some(&'$')
            && matches!(chars.get(*i + 1), Some(c) if c.is_ascii_digit() || *c == '?');
        if require {
            *i += 1;
        }
        // `$?` — the classic input identifier (`$?`, `$?="msg"`, `$?"msg"`, `$?*=`
        // password, `$?!=` yes/no, `$?N`, `$?#`). Deprecated in mIRC in favour of
        // `$input`, but still used. `$$?` requires a non-empty answer.
        if chars.get(*i) == Some(&'?') {
            *i += 1;
            return self.eval_question(chars, i, require);
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
        // Identifier name. An empty name immediately followed by `(` is the
        // `$(text[, N])` short form of `$eval` (mIRC's delayed-evaluation form).
        let mut name = read_name(chars, i);
        if name.is_empty() {
            if chars.get(*i) == Some(&'(') {
                name = "eval".to_string();
            } else {
                return "$".to_string();
            }
        }
        // `$eval(text,N)` must receive its first argument before the generic
        // argument pre-expansion below: N=0 returns the literal text, N=1
        // evaluates it once, and so on. `$(...)` reaches the same path.
        if name.eq_ignore_ascii_case("eval") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            let raw_args = split_args(&inner);
            let mut value = raw_args.first().cloned().unwrap_or_default();
            let count = self
                .expand(raw_args.get(1).map(String::as_str).unwrap_or("1"))
                .trim()
                .parse::<usize>()
                .unwrap_or(1);
            for _ in 0..count {
                value = self.expand(&value);
            }
            return value;
        }
        // `$unsafe(text)` evaluates its argument now but protects anything that
        // could be evaluated or parsed as a command for exactly one deferred
        // evaluation level. Normal (non-deferred) command expansion decodes the
        // markers immediately, so they never leak into displayed/sent text.
        if name.eq_ignore_ascii_case("unsafe") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            let raw = split_args(&inner).first().cloned().unwrap_or_default();
            return encode_delayed(&self.expand(&raw));
        }
        // `$regsub` must retain the literal output `%var`/`&binvar` name, and
        // `$regsubex` evaluates its subtext once per match. Neither can use the
        // generic eager argument-expansion path.
        if name.eq_ignore_ascii_case("regsub") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            return ident::eval_regsub(self, &split_args(&inner));
        }
        if name.eq_ignore_ascii_case("regsubex") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            return ident::eval_regsubex(self, &split_args(&inner));
        }
        // `$hfind(..., command)` exposes each match as `$1-` while executing
        // the command. Keep the final argument raw so the caller's current
        // parameters do not consume `$1-` before the search starts.
        if name.eq_ignore_ascii_case("hfind") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            let prop = if chars.get(*i) == Some(&'.') {
                let mut j = *i + 1;
                let property = read_name(chars, &mut j);
                if property.is_empty() {
                    String::new()
                } else {
                    *i = j;
                    property
                }
            } else {
                String::new()
            };
            return ident::eval_hfind(self, &split_args(&inner), &prop);
        }
        // $iif must evaluate lazily: expand the condition, set $v1/$v2, then expand
        // only the taken branch — so `$iif(x,$v1,y)` sees $v1 (and skips the other
        // branch, like mIRC). Pre-expanding the args would resolve $v1 too early.
        if name.eq_ignore_ascii_case("iif") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            return self.eval_iif(&split_args(&inner));
        }
        // $var's name argument is a literal pattern (like /set), not a value to
        // dereference — hand it the raw args, plus any `.property`, unexpanded.
        if name.eq_ignore_ascii_case("var") && chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            let prop = if chars.get(*i) == Some(&'.') {
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
            return ident::eval_var(self, &split_args(&inner), &prop);
        }
        // Optional (args).
        let (args, had_parens) = if chars.get(*i) == Some(&'(') {
            let inner = read_balanced(chars, i);
            (
                split_args(&inner)
                    .into_iter()
                    .map(|a| self.expand(&a))
                    .collect::<Vec<_>>(),
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
        self.record_v(&expanded);
        // Clone the Arc (cheap) so the leaf resolver can read channel state
        // without borrowing `self` across the evaluation.
        let state = self.state.clone();
        eval_bool_with(&expanded, &|term| state_op(&state, term))
    }

    /// Lazy `$iif(cond, iftrue, iffalse)`: expand the condition, publish `$v1`/`$v2`,
    /// then expand only the branch that's taken (so `$v1` inside it resolves, and
    /// the other branch isn't evaluated — mIRC's behaviour).
    fn eval_iif(&mut self, args: &[String]) -> String {
        // Evaluate the condition exactly like `if` — state-aware operators
        // (isop/ison/ischan/…) work, and $v1/$v2 are published — then expand only
        // the taken branch (the other isn't evaluated, matching mIRC).
        let is_true = self.eval_cond(args.first().map(String::as_str).unwrap_or(""));
        let taken = if is_true { args.get(1) } else { args.get(2) };
        taken.map(|a| self.expand(a)).unwrap_or_default()
    }

    /// Records `$v1`/`$v2` from an (already-expanded) condition: the two operands
    /// of a binary comparison (`a == b`, `a isin b`, …), else `$v1` = the whole
    /// value and `$v2` = empty (the truthiness form, `$iif(value, $v1, …)`).
    /// The classic `$?` input identifier. Optional modifier right after `?`
    /// (`*` password, `!` yes/no, a digit for `$N`, `#` for `$chan`), optional
    /// `=`, optional `"quoted message"`. `require` (from `$$?`) halts the run on
    /// an empty answer. Maps onto the same prompt backend as `$input`.
    fn eval_question(&mut self, chars: &[char], i: &mut usize, require: bool) -> String {
        let mut yes_no = false;
        let existing = match chars.get(*i).copied() {
            Some('*') => {
                *i += 1; // password field — jIRC prompts without masking
                None
            }
            Some('!') => {
                *i += 1;
                yes_no = true;
                None
            }
            Some('#') => {
                *i += 1;
                Some(self.event.chan.clone())
            }
            Some(c) if c.is_ascii_digit() => {
                let n = read_number(chars, i);
                Some(self.params_range(n, Some(n)))
            }
            _ => None,
        };
        let message = self.read_question_message(chars, i);
        // `$?N` / `$?#` return the existing value when it is already set.
        if let Some(v) = existing {
            if !v.is_empty() {
                return v;
            }
        }
        let msg = if message.is_empty() {
            "Enter reply:".to_string()
        } else {
            message
        };
        let answer = self.input.prompt(&msg, "", "").unwrap_or_default();
        self.vars.insert(LASTINPUT_KEY.to_string(), answer.clone());
        if yes_no {
            return if answer.is_empty() { "$false" } else { "$true" }.to_string();
        }
        if require && answer.is_empty() {
            self.halted = true;
        }
        answer
    }

    /// Reads `$?`'s optional `=` then `"quoted"` message, returning it expanded.
    fn read_question_message(&mut self, chars: &[char], i: &mut usize) -> String {
        if chars.get(*i) == Some(&'=') {
            *i += 1;
        }
        if chars.get(*i) == Some(&'"') {
            *i += 1;
            let mut m = String::new();
            while let Some(&c) = chars.get(*i) {
                *i += 1;
                if c == '"' {
                    break;
                }
                m.push(c);
            }
            return self.expand(&m);
        }
        String::new()
    }

    fn record_v(&mut self, cond: &str) {
        let (v1, v2) = split_v(cond);
        self.vars.insert(V1_KEY.to_string(), v1);
        self.vars.insert(V2_KEY.to_string(), v2);
    }
}

/// Splits an expanded condition into `($v1, $v2)`. See [`Runtime::record_v`].
fn split_v(cond: &str) -> (String, String) {
    let c = cond.trim();
    let toks: Vec<&str> = c.split_whitespace().collect();
    // Identifier/variable expansion can turn either operand into several words.
    // Equality operators remain unambiguous standalone tokens in the middle:
    // `$gettok(...) != 71 75 ...` must expose the full values as $v1/$v2.
    if let Some(i) = toks.iter().position(|tok| is_eq_op(tok)) {
        return (toks[..i].join(" "), toks[i + 1..].join(" "));
    }
    match toks.as_slice() {
        [] => (String::new(), String::new()),
        [one] => match split_spaceless_op(one) {
            Some((a, _, b)) => (a.to_string(), b.to_string()),
            None if is_supported_operator(one) => (String::new(), String::new()),
            None => (one.to_string(), String::new()),
        },
        // The left operand expanded to `$null`, so the operator is first.
        [op, rest @ ..] if is_supported_operator(op) => (String::new(), rest.join(" ")),
        // `a <binary-op> b…` — symbolic (==, <, …) or a binary word operator.
        [a, op, rest @ ..] if is_cmp_op(op) || is_binary_word_op(op) => {
            (a.to_string(), rest.join(" "))
        }
        // A required RHS expanded to `$null`.
        [a, op] if is_required_rhs_word_op(op) => (a.to_string(), String::new()),
        // Unary tests still expose the tested value as $v1, not the expression.
        [a, op] if is_unary_word_op(op) => (a.to_string(), String::new()),
        // A multi-word value: the whole value is $v1.
        _ => (c.to_string(), String::new()),
    }
}

/// The comparison operators that take a right-hand operand and aren't symbolic
/// (so aren't covered by [`is_cmp_op`]). Used only for `$v1`/`$v2` splitting.
fn is_binary_word_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(
        op.to_ascii_lowercase().as_str(),
        "isin"
            | "isincs"
            | "iswm"
            | "iswmcs"
            | "isnum"
            | "isletter"
            | "ison"
            | "isop"
            | "ishop"
            | "isvoice"
            | "isowner"
            | "isadmin"
            | "isreg"
            | "isban"
    )
}

/// Word tests that always take a right-hand string. If that value expands to
/// `$null`, whitespace tokenisation leaves `value operator`; it is still a
/// binary comparison against an empty string, not a unary test.
fn is_required_rhs_word_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(
        op.to_ascii_lowercase().as_str(),
        "isin" | "isincs" | "iswm" | "iswmcs"
    )
}

fn is_unary_word_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(
        op.to_ascii_lowercase().as_str(),
        "isnum" | "isletter" | "isalnum" | "isalpha" | "islower" | "isupper" | "ischan"
    )
}

/// mIRC allows any operator to be negated by prefixing it with `!`. Keep `!=`
/// as its normal not-equal operator (`=` alone is not a supported base), while
/// forms such as `!isnum`, `!isop`, `!==`, and `!<` invert their positive test.
fn split_operator_negation(op: &str) -> (bool, &str) {
    match op.strip_prefix('!') {
        Some(base) if is_supported_positive_operator(base) => (true, base),
        _ => (false, op),
    }
}

fn is_supported_operator(op: &str) -> bool {
    is_supported_positive_operator(op)
        || op
            .strip_prefix('!')
            .is_some_and(is_supported_positive_operator)
}

fn is_supported_positive_operator(op: &str) -> bool {
    matches!(
        op.to_ascii_lowercase().as_str(),
        "==="
            | "=="
            | "!="
            | "<="
            | ">="
            | "<"
            | ">"
            | "//"
            | "\\\\"
            | "&"
            | "isin"
            | "isincs"
            | "iswm"
            | "iswmcs"
            | "isnum"
            | "isletter"
            | "isalnum"
            | "isalpha"
            | "islower"
            | "isupper"
            | "ison"
            | "isop"
            | "ishop"
            | "isvoice"
            | "isowner"
            | "isadmin"
            | "isreg"
            | "ischan"
            | "isban"
    )
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

fn encode_delayed(value: &str) -> String {
    encode_envelope(DELAY_PREFIX, &value.replace(['\r', '\n'], " "))
}

pub(crate) fn decode_delayed(value: &str) -> String {
    decode_envelopes(value)
}

/// Marks the pipes in a file-identifier result as command separators without
/// making ordinary `|` characters structural. The envelope also keeps the
/// result intact while it travels through surrounding identifier expansion.
pub(super) fn encode_command_pipes(value: &str) -> String {
    if value.contains('|') {
        encode_envelope(PIPE_PREFIX, value)
    } else {
        value.to_string()
    }
}

fn encode_envelope(prefix: &str, value: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    format!(
        "{prefix}{}{ENVELOPE_END}",
        URL_SAFE_NO_PAD.encode(value.as_bytes())
    )
}

fn decode_payload(encoded: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

/// Decodes deferred `$unsafe` values for display/execution. Pipe envelopes are
/// decoded here too so `$timer(name).com` shows the original command; normal
/// execution extracts their separators first in [`split_command_pipes`].
fn decode_envelopes(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some((at, prefix)) = next_envelope(rest) {
        out.push_str(&rest[..at]);
        let after_prefix = &rest[at + prefix.len()..];
        let Some(end) = after_prefix.find(ENVELOPE_END) else {
            out.push_str(&rest[at..]);
            return out;
        };
        match decode_payload(&after_prefix[..end]) {
            Some(decoded) => out.push_str(&decoded),
            None => out.push_str(&rest[at..at + prefix.len() + end + ENVELOPE_END.len_utf8()]),
        }
        rest = &after_prefix[end + ENVELOPE_END.len_utf8()..];
    }
    out.push_str(rest);
    out
}

fn next_envelope(value: &str) -> Option<(usize, &'static str)> {
    [DELAY_PREFIX, PIPE_PREFIX]
        .into_iter()
        .filter_map(|prefix| value.find(prefix).map(|at| (at, prefix)))
        .min_by_key(|(at, _)| *at)
}

/// Expands only pipe envelopes into structural command segments. Literal pipes
/// (including those returned without `p`) and `$unsafe` envelopes stay opaque.
fn split_command_pipes(value: &str) -> Vec<String> {
    let mut segments = vec![String::new()];
    let mut rest = value;
    while let Some(at) = rest.find(PIPE_PREFIX) {
        segments.last_mut().unwrap().push_str(&rest[..at]);
        let after_prefix = &rest[at + PIPE_PREFIX.len()..];
        let Some(end) = after_prefix.find(ENVELOPE_END) else {
            segments.last_mut().unwrap().push_str(&rest[at..]);
            return segments;
        };
        let Some(decoded) = decode_payload(&after_prefix[..end]) else {
            segments
                .last_mut()
                .unwrap()
                .push_str(&rest[at..at + PIPE_PREFIX.len() + end + ENVELOPE_END.len_utf8()]);
            rest = &after_prefix[end + ENVELOPE_END.len_utf8()..];
            continue;
        };
        let mut pieces = decoded.split('|');
        segments
            .last_mut()
            .unwrap()
            .push_str(pieces.next().unwrap_or_default());
        for piece in pieces {
            let previous = segments.last_mut().unwrap();
            previous.truncate(previous.trim_end().len());
            segments.push(piece.trim_start().to_string());
        }
        rest = &after_prefix[end + ENVELOPE_END.len_utf8()..];
    }
    segments.last_mut().unwrap().push_str(rest);
    segments
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

/// Converts mIRC's byte-string representation (one character per byte) back
/// to bytes. Characters outside Latin-1 cannot be represented one-to-one, so
/// they retain their UTF-8 encoding instead of being truncated.
pub(crate) fn byte_string_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        if (ch as u32) <= 0xff {
            out.push(ch as u8);
        } else {
            let mut buf = [0; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
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
    body.iter().position(
        |s| matches!(s, Stmt::Label { name: label, .. } if label.eq_ignore_ascii_case(name)),
    )
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
#[derive(Clone, Copy)]
struct EvaluationBracketSpan {
    open: usize,
    close: usize,
    join_left: bool,
    join_right: bool,
    depth: usize,
}

/// Finds the leftmost deepest evaluation-bracket pair. A pair whose first and
/// last non-empty content tokens are both `$+` is a literal bracket wrapper,
/// not an evaluation group (`[ $+ value $+ ]`).
fn evaluation_bracket_span(tokens: &[String]) -> Option<EvaluationBracketSpan> {
    let mut stack = Vec::new();
    let mut spans = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.as_str() {
            "[" => stack.push(index),
            "]" => {
                let Some(open) = stack.pop() else { continue };
                let content = &tokens[open + 1..index];
                let first = content.iter().position(|token| !token.is_empty());
                let last = content.iter().rposition(|token| !token.is_empty());
                let join_left = first.is_some_and(|i| content[i] == "$+");
                let join_right = last.is_some_and(|i| content[i] == "$+");
                if !(join_left && join_right) {
                    spans.push(EvaluationBracketSpan {
                        open,
                        close: index,
                        join_left,
                        join_right,
                        depth: stack.len() + 1,
                    });
                }
            }
            _ => {}
        }
    }
    spans
        .into_iter()
        .max_by(|a, b| a.depth.cmp(&b.depth).then_with(|| b.open.cmp(&a.open)))
}

fn split_top_level(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    // Spaces inside a `"…"` don't split, so a quoted message stays one token
    // (`$?="Enter Password"`, `/timer name`). This is output-neutral for plain
    // text: tokens are re-joined with spaces, so the quotes render the same.
    let mut in_quote = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            // Only a `$id(`/`id(`/`$+(` paren groups arguments; a bare `(` is
            // literal, so `$+` and spaces around plain parens still work.
            '(' if !in_quote
                && (depth > 0
                    || cur
                        .chars()
                        .last()
                        .is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '+')) =>
            {
                depth += 1;
                cur.push(c);
            }
            ')' if !in_quote && depth > 0 => {
                depth -= 1;
                cur.push(c);
            }
            ' ' if depth == 0 && !in_quote => out.push(std::mem::take(&mut cur)),
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
        let first = s.split_whitespace().next().unwrap_or("");
        let starts_with_negated_operator = split_operator_negation(first).0
            || split_spaceless_op(first)
                .is_some_and(|(left, op, _)| left.is_empty() && split_operator_negation(op).0);
        // Don't mistake the `!=` operator for a negation prefix.
        // Also keep an infix operator whose left operand expanded to `$null`
        // (`!isnum 2-20`, `!isop #c`, `!<5`) out of this value-negation path.
        if !rest.starts_with('=') && !starts_with_negated_operator {
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
    // Expansion can produce multiword values on both sides of an equality
    // operator. Locate the standalone operator rather than assuming it remains
    // token two (the GKSSP challenge validator compares byte lists this way).
    if toks.len() >= 3 {
        if let Some(i) = toks.iter().position(|tok| is_eq_op(tok)) {
            return compare(&toks[..i].join(" "), toks[i], &toks[i + 1..].join(" "));
        }
    }
    match toks.len() {
        0 => false,
        // A lone comparison operator means both operands expanded to empty
        // (`$null != $null`); compare empty-to-empty rather than reading the bare
        // operator as a truthy string.
        1 if is_cmp_op(toks[0]) || is_binary_test_op(toks[0]) => compare("", toks[0], ""),
        1 if is_unary_word_op(toks[0]) => unary_op("", toks[0]),
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
        2 if is_cmp_op(toks[1]) || is_required_rhs_word_op(toks[1]) => {
            compare(toks[0], toks[1], "")
        }
        2 if is_cmp_op(toks[0]) || is_binary_test_op(toks[0]) => compare("", toks[0], toks[1]),
        2 => unary_op(toks[0], toks[1]),
        // 3+ tokens are normally `a OP rest`. But when an operand expands to a
        // multi-word value, an equality test against `$null` (which becomes "")
        // leaves the operator as the LAST token — `if (%line == $null)` with a
        // space-containing %line expands to `word1 word2 … ==`. Detect a trailing
        // `==`/`===`/`!=` as that emptiness test. (`<`/`>` stay positional — they
        // also occur as literal characters, e.g. a `>guest` nick prefix.)
        len if is_eq_op(toks[len - 1]) => compare(&toks[..len - 1].join(" "), toks[len - 1], ""),
        _ if is_binary_test_op(toks[1]) => compare(toks[0], toks[1], &toks[2..].join(" ")),
        // Expansion can turn a single operand into several words. Unless the
        // second token is an actual mSL test operator, this is still one
        // non-empty value (`if (%text)` / `$iif(%text,...)`), not a malformed
        // comparison. Socket scripts commonly use this while accumulating a
        // space-separated NAMES list.
        _ => truthy(s),
    }
}

/// Resolves the state-aware list operators (those needing channel/member
/// state). Operand order matches mSL: `<value> <op> <target>`. Returns `None`
/// for any other term so the caller falls back to the pure comparison logic.
/// Prefix chars assume the standard PREFIX set (~ owner, & admin, @ op,
/// % halfop, + voice).
fn state_op(state: &crate::irc::state::StateSnapshot, term: &str) -> Option<bool> {
    let toks: Vec<&str> = term.split_whitespace().collect();
    let (a, raw_op, target_from) = if toks.get(1).is_some_and(|op| is_state_word_op(op)) {
        (toks[0], toks[1], 2)
    } else if toks.first().is_some_and(|op| is_state_word_op(op)) {
        // The left operand expanded to `$null`, leaving the operator first.
        ("", toks[0], 1)
    } else {
        return None;
    };
    let raw_op = raw_op.to_ascii_lowercase();
    let (negated, op) = split_operator_negation(&raw_op);
    let target = toks
        .get(target_from..)
        .map(|r| r.join(" "))
        .unwrap_or_default();
    let find_channel = |name: &str| {
        let bare = state.isupport.channel_target(name).unwrap_or(name);
        state
            .channels
            .iter()
            .find(|channel| state.isupport.names_equal(&channel.name, bare))
    };
    // Is `nick` a member of `chan` holding the prefix for `mode`? Resolve the
    // actual character through ISUPPORT: IRC7 uses `.` for owner (`q`) rather
    // than the common `~`. `None` means any membership.
    let member_has = |chan: &str, nick: &str, mode: Option<char>| -> bool {
        let wanted_prefix = mode.and_then(|m| state.isupport.prefix_for_mode(m));
        if mode.is_some() && wanted_prefix.is_none() {
            return false;
        }
        match find_channel(chan) {
            Some(c) => c.members.iter().any(|(n, pre)| {
                state.isupport.names_equal(n, nick)
                    && match wanted_prefix {
                        Some(p) => pre.contains(p),
                        None => true,
                    }
            }),
            None => false,
        }
    };
    let result = match op {
        "ison" => Some(member_has(&target, a, None)),
        "isop" => Some(member_has(&target, a, Some('o'))),
        "ishop" => Some(member_has(&target, a, Some('h'))),
        "isvoice" => Some(member_has(&target, a, Some('v'))),
        "isowner" => Some(member_has(&target, a, Some('q'))),
        "isadmin" => Some(member_has(&target, a, Some('a'))),
        // `$nick isreg #chan` -> a member of the channel holding no prefix.
        "isreg" => Some(match find_channel(&target) {
            Some(c) => c
                .members
                .iter()
                .any(|(n, pre)| state.isupport.names_equal(n, a) && pre.is_empty()),
            None => false,
        }),
        // `<mask> isban #chan` -> the value is covered by a +b ban there.
        "isban" => Some(match find_channel(&target) {
            Some(c) => c.bans.iter().any(|b| {
                let pattern = fold_irc_mask(&state.isupport, b);
                let value = fold_irc_mask(&state.isupport, a);
                pattern == value || wildcard_match_cs(&pattern, &value)
            }),
            None => false,
        }),
        // `#chan ischan` -> are we on that channel?
        "ischan" => Some(find_channel(a).is_some()),
        _ => None,
    };
    result.map(|value| if negated { !value } else { value })
}

fn fold_irc_mask(isupport: &crate::irc::state::Isupport, value: &str) -> String {
    match value.split_once('!') {
        Some((nick, rest)) => format!("{}!{}", isupport.casefold(nick), rest.to_ascii_lowercase()),
        None => isupport.casefold(value),
    }
}

fn is_state_word_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(
        op.to_ascii_lowercase().as_str(),
        "ison"
            | "isop"
            | "ishop"
            | "isvoice"
            | "isowner"
            | "isadmin"
            | "isreg"
            | "ischan"
            | "isban"
    )
}

fn truthy(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s != "0"
        && !s.eq_ignore_ascii_case("$false")
        && !s.eq_ignore_ascii_case("false")
}

/// A `v1 op` test where `op` takes no right-hand operand: `isnum`, `isletter`,
/// `isalnum`, `isalpha`, `islower`, `isupper`.
fn unary_op(a: &str, op: &str) -> bool {
    let lower = op.to_ascii_lowercase();
    let (negated, op) = split_operator_negation(&lower);
    let result = match op {
        "isnum" => !a.is_empty() && a.parse::<f64>().is_ok(),
        "isletter" | "isalpha" => !a.is_empty() && a.chars().all(|c| c.is_alphabetic()),
        "isalnum" => !a.is_empty() && a.chars().all(|c| c.is_alphanumeric()),
        "islower" => {
            !a.is_empty()
                && a.chars().any(|c| c.is_alphabetic())
                && a.chars().all(|c| !c.is_uppercase())
        }
        "isupper" => {
            !a.is_empty()
                && a.chars().any(|c| c.is_alphabetic())
                && a.chars().all(|c| !c.is_lowercase())
        }
        // A bare two-token expression with an unknown operator: treat as truthy
        // of the whole (mIRC would generally see this as a non-empty string).
        _ => truthy(&format!("{a} {op}")),
    };
    if negated {
        !result
    } else {
        result
    }
}

fn compare(a: &str, op: &str, b: &str) -> bool {
    let lower = op.to_ascii_lowercase();
    let (negated, op) = split_operator_negation(&lower);
    let result = match op {
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
    };
    if negated {
        !result
    } else {
        result
    }
}

fn is_binary_test_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(
        op.to_ascii_lowercase().as_str(),
        "==" | "==="
            | "!="
            | "isin"
            | "isincs"
            | "iswm"
            | "iswmcs"
            | "isnum"
            | "isletter"
            | "//"
            | "\\\\"
            | "&"
            | "<"
            | ">"
            | "<="
            | ">="
    )
}

/// True for the exclusively-binary comparison operators (the same set
/// `split_spaceless_op` recognises). Lets a collapsed comparison with an empty
/// operand (`%x == $null`) be told apart from a unary `is*` test.
fn is_cmp_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(op, "===" | "==" | "!=" | "<=" | ">=" | "<" | ">")
}

/// The equality operators only. Safe to locate positionally even when an operand
/// expanded to a multi-word value — unlike `<`/`>`, which also occur as literal
/// characters and so can't be assumed to be operators.
fn is_eq_op(op: &str) -> bool {
    let (_, op) = split_operator_negation(op);
    matches!(op, "==" | "===" | "!=")
}

/// Splits a spaceless `a<op>b` comparison (e.g. `5==X`, `%n>=3`) into its parts,
/// so mSL's no-space conditions — `if ($2==X)` — compare correctly. Longer
/// operators are tried first so `===`/`<=`/`>=` aren't mis-split.
fn split_spaceless_op(s: &str) -> Option<(&str, &'static str, &str)> {
    for op in [
        "!===", "!==", "!!=", "!<=", "!>=", "!//", "!\\\\", "!&", "!<", "!>", "===", "==", "!=",
        "<=", ">=", "//", "\\\\", "&", "<", ">",
    ] {
        if let Some(idx) = s.find(op) {
            // `idx == 0` is meaningful when the left operand expanded to
            // `$null` (for example `$null!==x` -> `!==x`).
            return Some((&s[..idx], op, &s[idx + op.len()..]));
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
/// mIRC's single `/var`/`/set` math operation: exactly 3 space-separated tokens
/// where the 1st and 3rd parse as numbers and the 2nd is one of `+ - * / % ^ &`.
/// Returns the computed value (formatted like `$calc`), or `None` to keep the
/// text literal — so `1 + 2` → `3`, but `2^16`, `1 + 1 + 1`, and `a + b` stay as-is.
fn try_var_math(value: &str) -> Option<String> {
    let toks: Vec<&str> = value.split_whitespace().collect();
    if toks.len() != 3 {
        return None;
    }
    let a: f64 = toks[0].parse().ok()?;
    let b: f64 = toks[2].parse().ok()?;
    let r = match toks[1] {
        "+" => a + b,
        "-" => a - b,
        "*" => a * b,
        "/" => a / b,
        "%" => a % b,
        "^" => a.powf(b),
        "&" => ((a as i64) & (b as i64)) as f64,
        _ => return None,
    };
    r.is_finite().then(|| crate::script::ident::fmt_num(r))
}

/// Whether a token looks like a `/ruser` levels list: comma-separated numbers,
/// each optionally prefixed with `=`.
fn is_level_list(s: &str) -> bool {
    !s.is_empty()
        && s.split(',').all(|p| {
            let p = p.trim().trim_start_matches('=');
            !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())
        })
}

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

/// Numeric suffix attached to a `/write` switch (`-l5`, `-m2`).
fn write_numeric_switch(switches: &str, wanted: char) -> Option<u32> {
    let start = switches.find(wanted)? + wanted.len_utf8();
    let digits: String = switches[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

/// `/write` embeds the search text in the switch token (`-dstest`). Characters
/// after the first search-mode letter are data, not more switches.
fn write_control_switches(switches: &str) -> &str {
    let end = switches
        .char_indices()
        .find(|(_, ch)| matches!(ch, 's' | 'w' | 'W' | 'r' | 'R'))
        .map(|(offset, ch)| offset + ch.len_utf8())
        .unwrap_or(switches.len());
    &switches[..end]
}

/// Search mode and its attached pattern from `/write -sText`, `-w*mask*`,
/// `-r/regex/`, and their reverse-pattern uppercase variants.
fn write_search_switch(switches: &str) -> Option<(char, String)> {
    for (offset, ch) in switches.char_indices() {
        if matches!(ch, 's' | 'w' | 'W' | 'r' | 'R') {
            let pattern = switches[offset + ch.len_utf8()..].to_string();
            if !pattern.is_empty() {
                return Some((ch, pattern));
            }
        }
    }
    None
}

fn hash_text_format(flags: &str) -> super::hash::TextFormat {
    if flags.contains('i') {
        super::hash::TextFormat::Ini
    } else if flags.contains('n') {
        super::hash::TextFormat::DataOnly
    } else {
        super::hash::TextFormat::ItemsAndData
    }
}

/// Takes one command argument, accepting mIRC's common quoted-filename form.
fn take_file_arg(raw: &str) -> Option<(String, &str)> {
    let raw = raw.trim_start();
    if raw.is_empty() {
        return None;
    }
    if let Some(quoted) = raw.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some((quoted[..end].to_string(), quoted[end + 1..].trim_start()));
    }
    let end = raw.find(char::is_whitespace).unwrap_or(raw.len());
    Some((raw[..end].to_string(), raw[end..].trim_start()))
}

/// Extracts the decimal lifetime from a combined mIRC switch token such as
/// `-snu30`. A bare `-u` has no lifetime and is therefore ignored.
fn unset_seconds(flags: &str) -> Option<u64> {
    let mut chars = flags.chars();
    while let Some(ch) = chars.next() {
        if ch != 'u' && ch != 'U' {
            continue;
        }
        let digits: String = chars.by_ref().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse::<u32>().ok().map(u64::from);
    }
    None
}

/// Applies mIRC's overwrite rule for timed values: an ordinary assignment
/// cancels the old lifetime, `-uN` replaces it, and `-k` keeps an existing one.
fn update_timed_expiry<K>(expiries: &mut HashMap<K, TimedExpiry>, key: K, flags: &str)
where
    K: std::hash::Hash + Eq,
{
    let keep = flags.chars().any(|c| c == 'k' || c == 'K');
    if keep && expiries.contains_key(&key) {
        return;
    }
    match unset_seconds(flags) {
        Some(seconds) => {
            expiries.insert(key, TimedExpiry::after(seconds));
        }
        None if !keep => {
            expiries.remove(&key);
        }
        None => {}
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
    // The pattern starts at a word boundary (prev_space = true).
    wm_at(p, t, true)
}

/// `prev_space` = the previous pattern char was a space (or we're at the start).
/// mIRC's `&` is a whole-word wildcard only when it stands alone — space-bounded
/// on both sides; otherwise it's a literal `&`.
fn wm_at(p: &[char], t: &[char], prev_space: bool) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' => {
            // Match zero or more characters.
            wm_at(&p[1..], t, false) || (!t.is_empty() && wm_at(p, &t[1..], false))
        }
        '?' => !t.is_empty() && wm_at(&p[1..], &t[1..], false),
        // `&` alone matches one whole word (one or more non-space chars). Since a
        // word is space-bounded and `&` must be followed by space/end, the word is
        // the maximal non-space run — match it and continue.
        '&' if prev_space && (p.len() == 1 || p[1] == ' ') => {
            if t.is_empty() || t[0] == ' ' {
                return false; // needs at least one non-space character
            }
            let mut i = 1;
            while i < t.len() && t[i] != ' ' {
                i += 1;
            }
            wm_at(&p[1..], &t[i..], false)
        }
        c => !t.is_empty() && t[0] == c && wm_at(&p[1..], &t[1..], c == ' '),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_ctx<'a>() -> crate::script::RunCtx<'a> {
        crate::script::RunCtx {
            my_nick: "me",
            network: "Net",
            server: "irc.example.org",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
        }
    }

    #[test]
    fn direct_assignment_and_set_scope_match_mirc() {
        let engine = crate::script::ScriptEngine::new();
        engine.load(
            "alias mutate {\n\
               set %same original-global\n\
               var %same = original-local\n\
               %same = direct-local\n\
               set %same normal-local\n\
               set -g %same forced-global\n\
               set -l %only local-only\n\
               var -g %fromvar = global-from-var\n\
               %created = 2 + 3\n\
               set %stem [ $+ .leaf ] via-bracket\n\
               %stem [ $+ .other ] = 4 + 2\n\
               msg #c inside=%same only=%only fromvar=%fromvar created=%created leaf=%stem.leaf other=%stem.other\n\
             }\n\
             alias report msg #c same=%same only=[ $+ %only $+ ] fromvar=%fromvar created=%created leaf=%stem.leaf other=%stem.other",
        );

        assert_eq!(
            engine.run_alias(&run_ctx(), "#c", "mutate", ""),
            vec![Action::Send(
                "PRIVMSG #c :inside=normal-local only=local-only fromvar=global-from-var created=5 leaf=via-bracket other=6"
                    .into()
            )]
        );
        // Routine locals disappear; explicit/global assignments remain.
        assert_eq!(
            engine.run_alias(&run_ctx(), "#c", "report", ""),
            vec![Action::Send(
                "PRIVMSG #c :same=forced-global only=[] fromvar=global-from-var created=5 leaf=via-bracket other=6".into()
            )]
        );
    }

    #[test]
    fn nested_evaluation_brackets_and_literal_wrappers_coexist() {
        let engine = crate::script::ScriptEngine::new();
        engine.load(
            "alias t {\n\
               set %y hello\n\
               set -n %x % $+ y\n\
               set %fruit apple\n\
               var %key = fruit\n\
               msg #c [ [ %x ] ] % [ $+ [ %key ] ] [ $+ keep $+ ] pre [ $+ $upper(mid) ] [ $upper(end) $+ ] post\n\
             }",
        );
        assert_eq!(
            engine.run_alias(&run_ctx(), "#c", "t", ""),
            vec![Action::Send(
                "PRIVMSG #c :hello apple [keep] preMID ENDpost".into()
            )]
        );
    }

    #[test]
    fn return_normalizes_spaces_but_returnex_preserves_them() {
        let engine = crate::script::ScriptEngine::new();
        engine.load(
            "alias ordinary { return $+(a,$chr(32),$chr(32),b,$chr(32)) }\n\
             alias exact { returnex $+(a,$chr(32),$chr(32),b,$chr(32)) }\n\
             alias t msg #c $+(<,$ordinary,>) $+(<,$exact,>)",
        );
        assert_eq!(
            engine.run_alias(&run_ctx(), "#c", "t", ""),
            vec![Action::Send("PRIVMSG #c :<a b> <a  b >".into())]
        );
    }

    #[test]
    fn wildcard() {
        assert!(wildcard_match("!ping*", "!ping hello"));
        assert!(wildcard_match("*", "anything"));
        assert!(!wildcard_match("!ping*", "hello"));
        assert!(wildcard_match("h?llo", "hello"));
    }

    #[test]
    fn wildcard_ampersand_whole_word() {
        // `&` alone matches exactly one word.
        assert!(wildcard_match("!cmd &", "!cmd hello"));
        assert!(!wildcard_match("!cmd &", "!cmd hello world")); // extra word
        assert!(!wildcard_match("!cmd &", "!cmd ")); // needs a word
        assert!(wildcard_match("!reminder & *", "!reminder 5 buy milk"));
        // Not standalone -> literal `&`.
        assert!(wildcard_match("a&b", "a&b"));
        assert!(wildcard_match("test &his", "test &his"));
        assert!(!wildcard_match("test &his", "test this"));
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
        assert!(eval_bool("a multi word value"));
        assert!(eval_bool("+Sky +xpulse .Admin_Sky"));
        assert!(!eval_bool("0"));
    }

    #[test]
    fn bool_operators() {
        // isnum, with and without a range
        assert!(eval_bool("5 isnum"));
        assert!(!eval_bool("abc isnum"));
        assert!(!eval_bool("isalpha")); // empty LHS
        assert!(eval_bool("!isalpha"));
        assert!(eval_bool("5 isnum 1-10"));
        assert!(!eval_bool("50 isnum 1-10"));
        // mIRC permits `!` directly on any operator. This is distinct from a
        // leading `!value` and is used by imported scripts such as i7.mrc.
        assert!(!eval_bool("5 !isnum 2-20"));
        assert!(eval_bool("25 !isnum 2-20"));
        assert!(eval_bool("abc !isnum"));
        assert!(!eval_bool("x !isin xyz"));
        assert!(eval_bool("q !isin xyz"));
        assert!(!eval_bool(". isin")); // RHS expanded to $null
        assert!(eval_bool(". !isin"));
        assert!(eval_bool("!isnum 2-20")); // LHS expanded to $null
        assert!(!eval_bool("5 !< 10"));
        assert!(eval_bool("15 !< 10"));
        assert!(eval_bool("!<5"));
        assert!(!eval_bool("same !== same"));
        assert!(eval_bool("Same !=== same"));
        assert!(eval_bool("!==x"));
        assert!(eval_bool("!===x"));
        assert!(eval_bool("x !=")); // ordinary != remains unambiguous
        let challenge_header = "71 75 83 83 80 0 0 0";
        assert!(!eval_bool(&format!(
            "{challenge_header} != {challenge_header}"
        )));
        assert!(eval_bool(&format!(
            "{challenge_header} == {challenge_header}"
        )));
        assert!(eval_bool("71 75 83 83 80 0 0 0 != 71 75 83 83 80 0 0 1"));
        assert_eq!(
            split_v(&format!("{challenge_header} != {challenge_header}")),
            (challenge_header.into(), challenge_header.into())
        );
        // letter / alnum classes
        assert!(eval_bool("abc isletter"));
        assert!(!eval_bool("ab2 isletter"));
        assert!(eval_bool("ab2 !isalpha"));
        assert!(eval_bool("b isletter abc"));
        assert!(!eval_bool("z isletter abc"));
        assert!(eval_bool("abc123 isalnum"));
        assert!(eval_bool("abc isalpha"));
        assert!(eval_bool("abc islower"));
        assert!(eval_bool("ABC isupper"));
        assert!(!eval_bool("Abc islower"));
        assert_eq!(split_v("!isnum 2-20"), (String::new(), "2-20".into()));
        assert_eq!(split_v(". !isin"), (".".into(), String::new()));
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
        assert!(
            !eval_bool("abc <"),
            "nonempty < $null (empty !numeric) is false"
        );
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
        assert_eq!(
            split_top_commas("%x = $iif(a,b,c)"),
            vec!["%x = $iif(a,b,c)"]
        );
    }
}
