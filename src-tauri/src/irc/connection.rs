//! A single IRC connection: TCP transport, registration, the read loop, and
//! protocol-to-UI-event translation for the **standard** dialect.
//!
//! The protocol logic ([`process_message`]) is pure: it takes a parsed message
//! and the session state and produces outgoing lines + UI events. The async
//! [`run`] loop wires that to a real socket and the Tauri event bus. IRCX
//! handling (Phase 1b) hangs off the `Command::Raw` arm.

use irc_proto::{Command, Message, Response};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;
use std::time::{Duration, Instant};

use crate::config::ServerProfile;
use crate::irc::auth::{self, AuthState};
use crate::irc::event::{Direction, MessageKind, UiEvent, IRC_EVENT};
use crate::irc::state::SessionState;
use crate::irc::stream;
use std::collections::HashMap;

fn emit(app: &AppHandle, ev: UiEvent) {
    if let Err(e) = app.emit(IRC_EVENT, ev) {
        tracing::warn!("failed to emit irc event: {e}");
    }
}

/// Side effects produced by handling one message: lines to send and UI events.
#[derive(Default)]
pub struct Effects {
    pub outgoing: Vec<String>,
    pub events: Vec<UiEvent>,
    /// Events surfaced to the script engine only (never emitted to the UI):
    /// CTCP requests/replies, which the UI renders as an `Echo` but scripts
    /// need as a `Message` so `on CTCP`/`on CTCPREPLY` fire live.
    pub script_events: Vec<UiEvent>,
    /// Set when a channel ban list changed without a state-event (RPL_BANLIST),
    /// so the script state snapshot is refreshed for `isban`.
    pub bans_changed: bool,
    /// Channels to auto-join once `on CONNECT` has run (populated at RPL_WELCOME).
    /// The connection task performs the JOINs, honoring `/autojoin` (`-s` skip,
    /// `-dN` delay) — so a script can control them from within `on CONNECT`.
    pub autojoin: Vec<String>,
}

/// Per-connection mutable context for the read loop / protocol logic.
pub struct Context<'a> {
    pub server_id: &'a str,
    pub profile: &'a ServerProfile,
    pub state: &'a mut SessionState,
    /// Accumulates NAMES replies until RPL_ENDOFNAMES.
    pub names_accum: &'a mut HashMap<String, Vec<String>>,
    /// Accumulates WHOIS reply lines until RPL_ENDOFWHOIS.
    pub whois_accum: &'a mut HashMap<String, Vec<String>>,
    pub auth: &'a mut AuthState,
}

/// Outcome of a single connection attempt.
enum Outcome {
    /// The connection dropped (network/server); the supervisor may reconnect.
    Dropped,
    /// The outgoing channel closed (the manager removed this connection); stop.
    Stop,
}

/// Supervises a connection: connects, runs it, and reconnects with backoff on
/// unexpected drops (unless disabled). Returns when stopped or non-reconnecting.
pub async fn supervise(
    app: AppHandle,
    server_id: String,
    profile: ServerProfile,
    mut outgoing_rx: UnboundedReceiver<String>,
) {
    let mut backoff = Duration::from_secs(2);
    loop {
        let started = Instant::now();
        let outcome = run_once(&app, &server_id, &profile, &mut outgoing_rx).await;
        match outcome {
            Outcome::Stop => break,
            Outcome::Dropped => {
                if !profile.auto_reconnect {
                    break;
                }
                // A long-lived connection resets the backoff.
                if started.elapsed() > Duration::from_secs(60) {
                    backoff = Duration::from_secs(2);
                }
                emit(
                    &app,
                    UiEvent::Echo {
                        server_id: server_id.clone(),
                        target: "(status)".to_string(),
                        text: format!("Reconnecting in {}s…", backoff.as_secs()),
                    },
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// Decodes a raw IRC line. UTF-8 when valid; otherwise a tolerant pass that
/// rebuilds **CESU-8** surrogate pairs — how .NET/Java IRCX servers (IRC7 /
/// MSN-Chat) encode emoji and other astral characters, as two 3-byte UTF-16
/// surrogates, which is illegal in plain UTF-8 — and maps any remaining stray
/// byte to its Latin-1 code point so a non-UTF-8 server never breaks the
/// connection.
fn decode_irc_line(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(ch) = cesu8_surrogate_pair(&bytes[i..]) {
            out.push(ch);
            i += 6;
        } else if let Some(len) = valid_utf8_char(&bytes[i..]) {
            out.push_str(std::str::from_utf8(&bytes[i..i + len]).unwrap());
            i += len;
        } else {
            out.push(bytes[i] as char); // Latin-1 fallback for a stray byte
            i += 1;
        }
    }
    out
}

/// If `b` starts with a CESU-8 UTF-16 surrogate pair (high surrogate as 3 bytes,
/// then low surrogate as 3 bytes), returns the astral char it encodes.
fn cesu8_surrogate_pair(b: &[u8]) -> Option<char> {
    if b.len() < 6 {
        return None;
    }
    let hi = surrogate_unit(b[0], b[1], b[2])?;
    let lo = surrogate_unit(b[3], b[4], b[5])?;
    if !(0xD800..=0xDBFF).contains(&hi) || !(0xDC00..=0xDFFF).contains(&lo) {
        return None;
    }
    let cp = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
    char::from_u32(cp)
}

/// Decodes a `0xED 0x80-0xBF 0x80-0xBF` group to its code point (the U+D000–DFFF
/// range, which includes the UTF-16 surrogates CESU-8 uses).
fn surrogate_unit(b0: u8, b1: u8, b2: u8) -> Option<u32> {
    if b0 != 0xED || b1 & 0xC0 != 0x80 || b2 & 0xC0 != 0x80 {
        return None;
    }
    Some(((b0 as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6) | (b2 as u32 & 0x3F))
}

/// Length (1–4) of the valid UTF-8 char at the start of `b`, if any.
fn valid_utf8_char(b: &[u8]) -> Option<usize> {
    let max = b.len().min(4);
    (1..=max).find(|&n| std::str::from_utf8(&b[..n]).is_ok())
}

/// Writes one line to the socket and mirrors it to the raw console.
async fn write_line<W: AsyncWrite + Unpin>(
    w: &mut W,
    app: &AppHandle,
    server_id: &str,
    line: &str,
) {
    let line = line.trim_end_matches(['\r', '\n']);
    if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\r\n").await.is_err() {
        return;
    }
    let _ = w.flush().await;
    if !line.starts_with("PASS ") && !line.starts_with("AUTHENTICATE ") {
        emit(
            app,
            UiEvent::Raw {
                server_id: server_id.to_string(),
                direction: Direction::Out,
                line: line.to_string(),
            },
        );
    }
}

/// Runs a single connection attempt to completion.
async fn run_once(
    app: &AppHandle,
    server_id: &str,
    profile: &ServerProfile,
    outgoing_rx: &mut UnboundedReceiver<String>,
) -> Outcome {
    tracing::info!(
        "connecting to {}:{} (tls={}) as {}",
        profile.host,
        profile.port,
        profile.tls,
        profile.nick
    );
    let stream = match stream::connect(profile).await {
        Ok(s) => s,
        Err(e) => {
            emit(
                app,
                UiEvent::Error {
                    server_id: server_id.to_string(),
                    message: format!("connection failed: {e}"),
                },
            );
            emit(
                app,
                UiEvent::Disconnected {
                    server_id: server_id.to_string(),
                    reason: e.to_string(),
                },
            );
            // `on CONNECTFAIL` for scripts ($1- = the failure reason).
            fire_connectfail(app, server_id, profile, &e.to_string());
            return Outcome::Dropped;
        }
    };

    emit(
        app,
        UiEvent::Connected {
            server_id: server_id.to_string(),
        },
    );

    let (read_half, mut write_half) = tokio::io::split(stream);
    // IRC is byte-oriented: read raw bytes and decode UTF-8 with a Latin-1
    // fallback so non-UTF-8 lines (common on IRCX/older nets) don't drop us.
    // `buf` persists across iterations so a select!-cancelled partial read isn't lost.
    // The reader is built before registration so an NTLM handshake can read its
    // challenge frames from the same buffered stream the read loop then reuses.
    let mut reader = BufReader::new(read_half);
    let mut buf: Vec<u8> = Vec::new();

    // Registration. IRC7 servers require an IRCX NTLM (SSPI) handshake *before*
    // NICK/USER; standard servers use CAP/SASL instead.
    if profile.ntlm {
        if let Err(e) =
            crate::irc::ntlm::handshake(&mut reader, &mut write_half, profile, app, server_id).await
        {
            emit(
                app,
                UiEvent::Error {
                    server_id: server_id.to_string(),
                    message: format!("NTLM authentication failed: {e}"),
                },
            );
            emit(
                app,
                UiEvent::Disconnected {
                    server_id: server_id.to_string(),
                    reason: format!("NTLM auth failed: {e}"),
                },
            );
            return Outcome::Dropped;
        }
        write_line(&mut write_half, app, server_id, &format!("NICK {}", profile.nick)).await;
        write_line(
            &mut write_half,
            app,
            server_id,
            &format!("USER {} 0 * :{}", profile.username(), profile.realname()),
        )
        .await;
    } else {
        // Begin CAP negotiation before NICK/USER so SASL can run.
        write_line(&mut write_half, app, server_id, "CAP LS 302").await;
        if let Some(pw) = profile.password.as_deref().filter(|p| !p.is_empty()) {
            write_line(&mut write_half, app, server_id, &format!("PASS {pw}")).await;
        }
        write_line(&mut write_half, app, server_id, &format!("NICK {}", profile.nick)).await;
        write_line(
            &mut write_half,
            app,
            server_id,
            &format!("USER {} 0 * :{}", profile.username(), profile.realname()),
        )
        .await;
    }

    let mut state = SessionState {
        nick: profile.nick.clone(),
        server_port: profile.port,
        tls: profile.tls,
        alt_nick: profile.alt_nick.clone().unwrap_or_default(),
        main_nick: profile.nick.clone(),
        realname: profile.realname.clone().unwrap_or_default(),
        ..Default::default()
    };
    let mut names_accum: HashMap<String, Vec<String>> = HashMap::new();
    let mut whois_accum: HashMap<String, Vec<String>> = HashMap::new();
    let mut auth = AuthState::default();

    let reason = loop {
        tokio::select! {
            read = reader.read_until(b'\n', &mut buf) => match read {
                Ok(0) => break ("connection closed by server".to_string(), Outcome::Dropped),
                Ok(_) => {
                    // Partial line (EOF mid-line) — keep buffering; processed on next Ok(0).
                    if buf.last() != Some(&b'\n') {
                        continue;
                    }
                    let decoded = decode_irc_line(&buf);
                    buf.clear();
                    let line = decoded.trim_end_matches(['\r', '\n']);
                    if line.is_empty() {
                        continue;
                    }
                    emit(app, UiEvent::Raw {
                        server_id: server_id.to_string(),
                        direction: Direction::In,
                        line: line.to_string(),
                    });
                    let Ok(msg) = line.parse::<Message>() else {
                        tracing::debug!("unparsed line: {line:?}");
                        continue;
                    };
                    let mut ctx = Context {
                        server_id,
                        profile,
                        state: &mut state,
                        names_accum: &mut names_accum,
                        whois_accum: &mut whois_accum,
                        auth: &mut auth,
                    };
                    let effects = process_message(&mut ctx, line, msg);
                    drop(ctx);

                    // Refresh the shared snapshot when membership/state changed,
                    // so script commands/timers/sockets see current channel info.
                    if effects.events.iter().any(is_state_event) || effects.bans_changed {
                        if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
                            store.set(server_id, state.snapshot());
                        }
                    }

                    let mut script_actions =
                        run_scripts(app, &state, profile, &effects.events, &effects.script_events, Some(line));

                    // `/autojoin` (used in `on CONNECT`) controls the deferred
                    // autojoin: pull its control out of the script actions before
                    // the rest are applied.
                    let mut autojoin_skip = false;
                    let mut autojoin_delay = 0u32;
                    script_actions.retain(|a| {
                        if let crate::script::eval::Action::Autojoin { skip, delay_secs } = a {
                            autojoin_skip |= *skip;
                            if *delay_secs > 0 {
                                autojoin_delay = *delay_secs;
                            }
                            false
                        } else {
                            true
                        }
                    });

                    for out in effects.outgoing {
                        write_line(&mut write_half, app, server_id, &out).await;
                    }
                    for ev in effects.events {
                        emit(app, ev);
                    }
                    if !script_actions.is_empty() {
                        crate::script::apply_actions(
                            app,
                            server_id,
                            &state.nick,
                            &profile.name,
                            &profile.host,
                            script_actions,
                        );
                    }

                    // Now that `on CONNECT` has run, perform the deferred autojoin
                    // (unless a script skipped it; a delay postpones the JOINs).
                    if !effects.autojoin.is_empty() && !autojoin_skip {
                        if autojoin_delay > 0 {
                            let app2 = app.clone();
                            let sid = server_id.to_string();
                            let channels = effects.autojoin.clone();
                            tauri::async_runtime::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_secs(
                                    autojoin_delay as u64,
                                ))
                                .await;
                                if let Some(m) = app2.try_state::<crate::irc::ConnectionManager>() {
                                    for ch in channels {
                                        let _ = m.send(&sid, format!("JOIN {ch}"));
                                    }
                                }
                            });
                        } else {
                            for ch in &effects.autojoin {
                                write_line(&mut write_half, app, server_id, &format!("JOIN {ch}"))
                                    .await;
                            }
                        }
                    }
                }
                Err(e) => break (format!("read error: {e}"), Outcome::Dropped),
            },
            cmd = outgoing_rx.recv() => match cmd {
                Some(line) => {
                    if let Some(rest) = line.strip_prefix("\u{0}SETID ") {
                        // Internal control line from /anick /mnick /fullname: update
                        // our identity in the session state so $anick/$mnick/$fullname
                        // reflect it, re-publish the snapshot, and don't send it on.
                        if let Some((field, value)) = rest.split_once(' ') {
                            match field {
                                "anick" => state.alt_nick = value.to_string(),
                                "mnick" => state.main_nick = value.to_string(),
                                "fullname" => state.realname = value.to_string(),
                                _ => {}
                            }
                            if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
                                store.set(server_id, state.snapshot());
                            }
                        }
                    } else {
                        // Capture our own away message for $awaymsg: "AWAY :msg" sets it,
                        // bare "AWAY" clears it. Propagate the snapshot to the engine.
                        if let Some(rest) = line.strip_prefix("AWAY") {
                            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with(':') {
                                let rest = rest.trim_start();
                                state.away_msg = rest.strip_prefix(':').unwrap_or(rest).to_string();
                                if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
                                    store.set(server_id, state.snapshot());
                                }
                            }
                        }
                        write_line(&mut write_half, app, server_id, &line).await;
                    }
                }
                None => break ("disconnected".to_string(), Outcome::Stop),
            },
        }
    };

    // Fire `on DISCONNECT` handlers (best-effort: the socket is already gone, so
    // outgoing sends won't reach the server, but /echo and state updates work).
    let disc = UiEvent::Disconnected {
        server_id: server_id.to_string(),
        reason: reason.0,
    };
    let actions = run_scripts(app, &state, profile, std::slice::from_ref(&disc), &[], None);
    if !actions.is_empty() {
        crate::script::apply_actions(
            app,
            server_id,
            &state.nick,
            &profile.name,
            &profile.host,
            actions,
        );
    }
    emit(app, disc);
    if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
        store.remove(server_id);
    }
    reason.1
}

/// True for events that change channel/membership state — when one occurs the
/// shared [`StateStore`](crate::irc::state::StateStore) snapshot is refreshed.
fn is_state_event(ev: &UiEvent) -> bool {
    matches!(
        ev,
        UiEvent::Names { .. }
            | UiEvent::Join { .. }
            | UiEvent::Part { .. }
            | UiEvent::Quit { .. }
            | UiEvent::Kick { .. }
            | UiEvent::NickChange { .. }
            | UiEvent::Mode { .. }
            | UiEvent::Registered { .. }
    )
}

/// The nick from a raw line's prefix (`:nick!user@host CMD …` → `nick`); the
/// bare prefix when there's no `!`/`@` (a server), or empty with no prefix.
fn source_nick(line: &str) -> String {
    line.strip_prefix(':')
        .and_then(|s| s.split(' ').next())
        .map(|p| p.split(['!', '@']).next().unwrap_or(p).to_string())
        .unwrap_or_default()
}

/// Maps an inbound IRC command to the named `on` event it fires, if any.
fn named_event_kind(command: &str) -> Option<&'static str> {
    match command.to_ascii_uppercase().as_str() {
        "WALLOPS" => Some("WALLOPS"),
        "ERROR" => Some("ERROR"),
        "PING" => Some("PING"),
        "PONG" => Some("PONG"),
        _ => None,
    }
}

/// Runs `on CONNECTFAIL` after a failed connection attempt. No live session
/// exists yet, so a minimal state (just our nick) backs the run context.
fn fire_connectfail(app: &AppHandle, server_id: &str, profile: &ServerProfile, reason: &str) {
    let Some(engine) = app.try_state::<crate::script::ScriptEngine>() else {
        return;
    };
    let state = SessionState {
        nick: profile.nick.clone(),
        ..Default::default()
    };
    let ctx = crate::script::RunCtx {
        my_nick: &state.nick,
        network: &profile.name,
        server: &profile.host,
        data_dir: crate::script::script_data_dir(app),
        state: std::sync::Arc::new(state.snapshot()),
    };
    let actions = crate::script::dispatch_named(&engine, &ctx, "CONNECTFAIL", "", reason);
    if !actions.is_empty() {
        crate::script::apply_actions(app, server_id, &state.nick, &profile.name, &profile.host, actions);
    }
}

/// Runs script event handlers for the events produced by one inbound message.
fn run_scripts(
    app: &AppHandle,
    state: &SessionState,
    profile: &ServerProfile,
    events: &[UiEvent],
    // Extra events for scripts only (CTCP requests/replies); see `Effects`.
    script_events: &[UiEvent],
    raw_line: Option<&str>,
) -> Vec<crate::script::eval::Action> {
    let Some(engine) = app.try_state::<crate::script::ScriptEngine>() else {
        return Vec::new();
    };
    let ctx = crate::script::RunCtx {
        my_nick: &state.nick,
        network: &profile.name,
        server: &profile.host,
        data_dir: crate::script::script_data_dir(app),
        state: std::sync::Arc::new(state.snapshot()),
    };
    let mut actions = Vec::new();
    // `on RAW` fires for every inbound server line; named protocol events
    // (`on WALLOPS`/`ERROR`/`PING`/`PONG`) fire off the same parsed command.
    if let Some(line) = raw_line {
        if let Some((command, params)) = raw_command_params(line) {
            if let Some(kind) = named_event_kind(&command) {
                let text = params.last().cloned().unwrap_or_default();
                actions.extend(crate::script::dispatch_named(
                    &engine,
                    &ctx,
                    kind,
                    &source_nick(line),
                    &text,
                ));
            }
            actions.extend(crate::script::dispatch_raw(&engine, &ctx, &command, params));
        }
    }
    for ev in events.iter().chain(script_events) {
        actions.extend(crate::script::drive_event(&engine, &ctx, ev));
    }
    actions
}

/// Splits a raw IRC line into its command/numeric and parameters. An optional
/// `:prefix` is dropped, middle params split on spaces, and a trailing `:param`
/// keeps its spaces.
fn raw_command_params(line: &str) -> Option<(String, Vec<String>)> {
    let rest = match line.strip_prefix(':') {
        Some(after) => after.split_once(' ').map(|(_, r)| r).unwrap_or(""),
        None => line,
    };
    let (command, argstr) = match rest.trim_start().split_once(' ') {
        Some((c, a)) => (c, a),
        None => (rest.trim_start(), ""),
    };
    if command.is_empty() {
        return None;
    }
    let mut params = Vec::new();
    let mut s = argstr.trim_start();
    while !s.is_empty() {
        if let Some(trailing) = s.strip_prefix(':') {
            params.push(trailing.to_string());
            break;
        }
        match s.split_once(' ') {
            Some((tok, more)) => {
                params.push(tok.to_string());
                s = more.trim_start();
            }
            None => {
                params.push(s.to_string());
                break;
            }
        }
    }
    Some((command.to_string(), params))
}

/// Pure protocol handler: updates session state and returns the side effects
/// (outgoing lines + UI events) for a single inbound message.
pub fn process_message(ctx: &mut Context, raw: &str, msg: Message) -> Effects {
    let mut fx = Effects::default();
    let server_id = ctx.server_id.to_string();
    let source = msg.source_nickname().map(|s| s.to_string());
    // Record the sender's nick!user@host in the internal address list ($ial).
    if let Some(irc_proto::Prefix::Nickname(nick, user, host)) = &msg.prefix {
        if !user.is_empty() && !host.is_empty() {
            ctx.state.record_address(nick, format!("{nick}!{user}@{host}"));
        }
    }
    // IRCv3 server-time (@time tag), used as the line timestamp when present.
    let server_time = msg.tags.as_ref().and_then(|tags| {
        tags.iter()
            .find(|t| t.0 == "time")
            .and_then(|t| t.1.clone())
    });

    match msg.command {
        Command::PING(ref s, ref t) => {
            let pong = match t {
                Some(token) => format!("PONG {s} :{token}"),
                None => format!("PONG :{s}"),
            };
            fx.outgoing.push(pong);
        }
        Command::CAP(_, ref sub, ref a, ref b) => {
            // The capability list is in the last present parameter.
            let caps = b.clone().or_else(|| a.clone()).unwrap_or_default();
            fx.outgoing
                .extend(auth::on_cap(ctx.profile, ctx.auth, sub, &caps));
        }
        Command::AUTHENTICATE(ref data) => {
            fx.outgoing.extend(auth::on_authenticate(ctx.profile, data));
        }
        Command::PRIVMSG(ref target, ref text) => {
            // IRCX/MSN-Chat clients prepend a font descriptor ("\x01S Tahoma;0 …")
            // that otherwise reads as a CTCP named "S". Detect it by its
            // distinctive shape and show the plain message before CTCP handling.
            // This is intentionally independent of the profile's IRCX flag: the
            // font tag is self-identifying (leading \x01 + `<effect> <font>;<n> `),
            // and some IRCX servers (e.g. Buzzen / MSN-Chat) aren't flagged as
            // IRCX at connect time, which left these showing as "[CTCP S]".
            if let Some(body) = strip_ircx_font(text) {
                fx.events.push(UiEvent::Message {
                    server_id,
                    kind: MessageKind::Privmsg,
                    from: source,
                    target: target.clone(),
                    text: body.to_string(),
                    time: server_time,
                });
                return fx;
            }
            // CTCP requests (\x01CMD args\x01), excluding ACTION, get auto-replies.
            if let Some(ctcp) = text.strip_prefix('\u{1}').map(|s| s.trim_end_matches('\u{1}')) {
                let (cmd, rest) = ctcp.split_once(' ').unwrap_or((ctcp, ""));
                // A DCC offer (CHAT/SEND) — surface it to the user. (Connecting to
                // accept it is a later phase; for now incoming offers are visible.)
                if cmd.eq_ignore_ascii_case("DCC") {
                    let who = source.as_deref().unwrap_or("?").to_string();
                    match crate::irc::dcc::parse_dcc(ctcp) {
                        // A CHAT offer is acceptable — surface it structurally so the
                        // UI can connect (`/dcc get <nick>`).
                        Some(o) if o.kind == crate::irc::dcc::DccKind::Chat => {
                            fx.events.push(UiEvent::DccChatOffer {
                                server_id: server_id.clone(),
                                nick: who.clone(),
                                ip: o.ip.to_string(),
                                port: o.port,
                            });
                            fx.events.push(UiEvent::Echo {
                                server_id,
                                target: "(status)".to_string(),
                                text: format!("[DCC] {who} offers a DCC CHAT — /dcc get {who} to accept"),
                            });
                        }
                        Some(o) => {
                            fx.events.push(UiEvent::DccFileOffer {
                                server_id: server_id.clone(),
                                nick: who.clone(),
                                filename: o.filename.clone(),
                                ip: o.ip.to_string(),
                                port: o.port,
                                size: o.size,
                            });
                            fx.events.push(UiEvent::Echo {
                                server_id,
                                target: "(status)".to_string(),
                                text: format!(
                                    "[DCC] {who} offers to send you \"{}\" ({} bytes) — /dcc get {who} to accept",
                                    o.filename, o.size
                                ),
                            });
                        }
                        None => fx.events.push(UiEvent::Echo {
                            server_id,
                            target: "(status)".to_string(),
                            text: format!("[DCC] {who} sent an unrecognised DCC request: {rest}"),
                        }),
                    }
                    return fx;
                }
                if !cmd.eq_ignore_ascii_case("ACTION") {
                    // Only auto-respond to direct CTCP (avoids channel storms).
                    if target == &ctx.state.nick {
                        if let (Some(nick), Some(reply)) =
                            (source.as_ref(), ctcp_reply(cmd, rest))
                        {
                            fx.outgoing
                                .push(format!("NOTICE {nick} :\u{1}{reply}\u{1}"));
                        }
                    }
                    // Surface the request to scripts as a Message so `on CTCP`
                    // fires; the UI shows the Echo below, not this.
                    fx.script_events.push(UiEvent::Message {
                        server_id: server_id.clone(),
                        kind: MessageKind::Privmsg,
                        from: source.clone(),
                        target: target.clone(),
                        text: text.clone(),
                        time: server_time.clone(),
                    });
                    fx.events.push(UiEvent::Echo {
                        server_id,
                        target: "(status)".to_string(),
                        text: format!(
                            "[CTCP {}] from {}",
                            cmd.to_uppercase(),
                            source.as_deref().unwrap_or("?")
                        ),
                    });
                    return fx;
                }
            }
            fx.events.push(UiEvent::Message {
                server_id,
                kind: MessageKind::Privmsg,
                from: source,
                target: target.clone(),
                text: text.clone(),
                time: server_time,
            });
        }
        Command::NOTICE(ref target, ref text) => {
            // A CTCP reply (\x01...\x01) — render it readably, and surface it to
            // scripts as a Message so `on CTCPREPLY` fires.
            if let Some(ctcp) = text.strip_prefix('\u{1}').map(|s| s.trim_end_matches('\u{1}')) {
                fx.script_events.push(UiEvent::Message {
                    server_id: server_id.clone(),
                    kind: MessageKind::Notice,
                    from: source.clone(),
                    target: target.clone(),
                    text: text.clone(),
                    time: server_time.clone(),
                });
                fx.events.push(UiEvent::Echo {
                    server_id,
                    target: "(status)".to_string(),
                    text: format!(
                        "[CTCP reply from {}] {}",
                        source.as_deref().unwrap_or("?"),
                        ctcp_reply_pretty(ctcp)
                    ),
                });
                return fx;
            }
            fx.events.push(UiEvent::Message {
                server_id,
                kind: MessageKind::Notice,
                from: source,
                target: target.clone(),
                text: text.clone(),
                time: server_time,
            });
        }
        Command::JOIN(ref channel, _, _) => {
            if let Some(nick) = &source {
                ctx.state.upsert_member(channel, nick, String::new());
                fx.events.push(UiEvent::Join {
                    server_id,
                    channel: channel.clone(),
                    nick: nick.clone(),
                });
            }
        }
        Command::PART(ref channel, ref reason) => {
            if let Some(nick) = &source {
                ctx.state.remove_member(channel, nick);
                if nick == &ctx.state.nick {
                    ctx.state.channels.remove(channel);
                }
                fx.events.push(UiEvent::Part {
                    server_id,
                    channel: channel.clone(),
                    nick: nick.clone(),
                    reason: reason.clone(),
                });
            }
        }
        Command::QUIT(ref reason) => {
            if let Some(nick) = &source {
                let channels = ctx.state.remove_member_everywhere(nick);
                fx.events.push(UiEvent::Quit {
                    server_id,
                    nick: nick.clone(),
                    reason: reason.clone(),
                    channels,
                });
            }
        }
        Command::KICK(ref channel, ref kicked, ref comment) => {
            let is_self = kicked == &ctx.state.nick;
            ctx.state.remove_member(channel, kicked);
            if is_self {
                ctx.state.channels.remove(channel);
            }
            fx.events.push(UiEvent::Kick {
                server_id,
                channel: channel.clone(),
                nick: kicked.clone(),
                by: source,
                reason: comment.clone(),
                is_self,
            });
        }
        Command::AWAY(ref message) => {
            // away-notify: another user's away state changed.
            if let Some(nick) = &source {
                let channels: Vec<String> = ctx
                    .state
                    .channels
                    .iter()
                    .filter(|(_, ch)| ch.members.contains_key(nick))
                    .map(|(name, _)| name.clone())
                    .collect();
                fx.events.push(UiEvent::AwayChange {
                    server_id,
                    nick: nick.clone(),
                    away: message.is_some(),
                    message: message.clone(),
                    channels,
                });
            }
        }
        Command::NICK(ref new_nick) => {
            if let Some(old) = &source {
                ctx.state.rename_member(old, new_nick);
                if old == &ctx.state.nick {
                    ctx.state.nick = new_nick.clone();
                }
                fx.events.push(UiEvent::NickChange {
                    server_id,
                    old: old.clone(),
                    new: new_nick.clone(),
                });
            }
        }
        Command::TOPIC(ref channel, ref topic) => {
            if let Some(ch) = ctx.state.channels.get_mut(channel) {
                ch.topic = topic.clone();
            }
            fx.events.push(UiEvent::Topic {
                server_id,
                channel: channel.clone(),
                topic: topic.clone(),
                set_by: source,
            });
        }
        // Parse MODE ourselves from the raw line: irc-proto ignores the
        // server's CHANTYPES/CHANMODES/PREFIX, so it mis-routes %#-channel
        // modes to UserMODE and drops prefix-mode arguments.
        Command::ChannelMODE(..) | Command::UserMODE(..) => {
            handle_mode(ctx, &mut fx, &server_id, raw, source.clone());
        }
        Command::INVITE(ref _invited, ref channel) => {
            fx.events.push(UiEvent::Invite {
                server_id,
                from: source,
                channel: channel.clone(),
            });
        }
        // account-notify: a user logged in/out of their account.
        Command::ACCOUNT(ref account) => {
            if let Some(nick) = &source {
                let text = if account == "*" || account == "0" {
                    format!("{nick} logged out")
                } else {
                    format!("{nick} is now logged in as {account}")
                };
                push_channel_notice(&mut fx, ctx, &server_id, nick, &text);
            }
        }
        // chghost: a user's user@host changed.
        Command::CHGHOST(ref user, ref host) => {
            if let Some(nick) = &source {
                let text = format!("{nick} is now {user}@{host}");
                push_channel_notice(&mut fx, ctx, &server_id, nick, &text);
            }
        }
        Command::WALLOPS(ref text) => {
            fx.events.push(UiEvent::Echo {
                server_id,
                target: "(status)".to_string(),
                text: format!("[WALLOPS{}] {text}", source.map(|s| format!(" from {s}")).unwrap_or_default()),
            });
        }
        Command::ERROR(ref message) => fx.events.push(UiEvent::Error {
            server_id,
            message: message.clone(),
        }),
        Command::Response(resp, ref args) => handle_numeric(ctx, &mut fx, resp, args),
        Command::Raw(ref cmd, ref args) => {
            // A numeric that irc-proto didn't recognise. If it belongs to an
            // in-progress WHOIS, fold it into that block (this is where most of
            // the extra WHOIS numerics — account/secure/host/etc. — arrive).
            if let Ok(code) = cmd.parse::<u16>() {
                if let Some(nick) = args.get(1) {
                    if ctx.whois_accum.contains_key(nick) {
                        let nick = nick.clone();
                        let line = whois_line(code, args);
                        if !line.trim().is_empty() {
                            ctx.whois_accum.entry(nick).or_default().push(line);
                        }
                        return fx;
                    }
                }
            }
            // IRCX numerics (800–999) and extension commands land here.
            match crate::irc::ircx::raw_event(&server_id, source, cmd, args) {
                Some(ev) => fx.events.push(ev),
                None => {
                    // An unrecognised numeric irc-proto routed to Raw: surface it
                    // as a Numeric so errors (≥400) still show and trace captures
                    // the rest — otherwise it would be dropped entirely.
                    if let Ok(code) = cmd.parse::<u16>() {
                        fx.events.push(UiEvent::Numeric {
                            server_id,
                            code,
                            args: args.to_vec(),
                        });
                    } else {
                        tracing::debug!("unhandled raw command {cmd} {args:?}");
                    }
                }
            }
        }
        _ => {}
    }

    fx
}

/// Picks the next nickname to try after a "nick in use" reply.
/// Attempt 1 uses the alternative nick (if set), then appends underscores,
/// then falls back to a numeric suffix.
fn next_nick(profile: &ServerProfile, attempt: u32) -> String {
    let base = &profile.nick;
    let alt = profile
        .alt_nick
        .as_deref()
        .filter(|a| !a.is_empty() && *a != base.as_str());
    if attempt == 1 {
        if let Some(a) = alt {
            return a.to_string();
        }
    }
    // If the alt consumed attempt 1, underscore depth starts one lower.
    let depth = (if alt.is_some() { attempt.saturating_sub(1) } else { attempt }).max(1);
    if depth <= 4 {
        format!("{base}{}", "_".repeat(depth as usize))
    } else {
        format!("{base}{attempt}")
    }
}

/// Emits an info line (via Echo) into every channel we share with `nick`.
fn push_channel_notice(fx: &mut Effects, ctx: &Context, server_id: &str, nick: &str, text: &str) {
    for (name, ch) in ctx.state.channels.iter() {
        if ch.members.contains_key(nick) {
            fx.events.push(UiEvent::Echo {
                server_id: server_id.to_string(),
                target: name.clone(),
                text: text.to_string(),
            });
        }
    }
}

/// Splits a raw IRC line into its command + parameters (handling `@tags`, the
/// `:prefix`, and a final `:trailing` parameter). `out[0]` is the command.
fn irc_params(raw: &str) -> Vec<String> {
    let mut s = raw.trim_start();
    if s.starts_with('@') {
        s = s.split_once(' ').map(|(_, r)| r).unwrap_or("");
    }
    s = s.trim_start();
    if s.starts_with(':') {
        s = s.split_once(' ').map(|(_, r)| r).unwrap_or("");
    }
    let mut params = Vec::new();
    let mut rest = s.trim_start();
    while !rest.is_empty() {
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing.to_string());
            break;
        }
        match rest.split_once(' ') {
            Some((tok, more)) => {
                params.push(tok.to_string());
                rest = more.trim_start();
            }
            None => {
                params.push(rest.to_string());
                break;
            }
        }
    }
    params
}

/// Parses and applies a MODE change using the server's ISUPPORT (CHANTYPES,
/// PREFIX, CHANMODES), then emits the display + an updated roster on prefix
/// changes. Works for `%`-style IRCX channels that irc-proto won't recognise.
fn handle_mode(
    ctx: &mut Context,
    fx: &mut Effects,
    server_id: &str,
    raw: &str,
    by: Option<String>,
) {
    let params = irc_params(raw);
    let Some(i) = params.iter().position(|p| p.eq_ignore_ascii_case("MODE")) else {
        return;
    };
    let (Some(target), Some(modestring)) = (params.get(i + 1).cloned(), params.get(i + 2).cloned())
    else {
        return;
    };
    let mut args = params.get(i + 3..).map(|s| s.to_vec()).unwrap_or_default().into_iter();

    if !ctx.state.isupport.is_channel(&target) {
        // User mode (only ever our own): track it for $usermode, then render.
        apply_user_modes(&mut ctx.state.user_mode, &modestring);
        fx.events.push(UiEvent::Mode {
            server_id: server_id.to_string(),
            target,
            modes: render_modestring(&modestring),
            by,
        });
        return;
    }

    let mut tokens: Vec<String> = Vec::new();
    let mut adding = true;
    let mut prefix_changed = false;
    let mut got_owner = false;
    let mut lost_owner = false;
    for letter in modestring.chars() {
        match letter {
            '+' => adding = true,
            '-' => adding = false,
            _ => {
                let arg = if ctx.state.isupport.mode_takes_arg(letter, adding) {
                    args.next()
                } else {
                    None
                };
                if ctx.state.isupport.prefix_for_mode(letter).is_some() {
                    if let Some(nick) = &arg {
                        ctx.state.apply_prefix_mode(&target, nick, letter, adding);
                        prefix_changed = true;
                        // Our own ownership changed (+q/-q on our own nick).
                        if letter == 'q' && nick.eq_ignore_ascii_case(&ctx.state.nick) {
                            if adding {
                                got_owner = true;
                            } else {
                                lost_owner = true;
                            }
                        }
                    }
                } else if letter == 'b' {
                    if let Some(mask) = &arg {
                        ctx.state.set_ban(&target, mask, adding);
                    }
                }
                let sign = if adding { '+' } else { '-' };
                match &arg {
                    Some(a) => tokens.push(format!("{sign}{letter} {a}")),
                    None => tokens.push(format!("{sign}{letter}")),
                }
            }
        }
    }

    // Someone else stripped our +q — capture the offender for takeover protection.
    let revoked_by = if lost_owner {
        by.clone().filter(|b| !b.eq_ignore_ascii_case(&ctx.state.nick))
    } else {
        None
    };
    fx.events.push(UiEvent::Mode {
        server_id: server_id.to_string(),
        target: target.clone(),
        modes: tokens.join(" "),
        by,
    });
    if got_owner {
        fx.events.push(UiEvent::OwnerGranted {
            server_id: server_id.to_string(),
            channel: target.clone(),
        });
    }
    if let Some(by) = revoked_by {
        fx.events.push(UiEvent::OwnerRevoked {
            server_id: server_id.to_string(),
            channel: target.clone(),
            by,
        });
    }
    if prefix_changed {
        if let Some(ch) = ctx.state.channels.get(&target) {
            fx.events.push(UiEvent::Names {
                server_id: server_id.to_string(),
                channel: target,
                members: ch.member_list(),
            });
        }
    }
}

/// Re-renders a modestring with an explicit sign on every letter (`+i+x`).
fn render_modestring(modestring: &str) -> String {
    let mut out = String::new();
    let mut adding = true;
    for ch in modestring.chars() {
        match ch {
            '+' => adding = true,
            '-' => adding = false,
            _ => {
                out.push(if adding { '+' } else { '-' });
                out.push(ch);
            }
        }
    }
    out
}

/// Current unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Apply a user-mode change string (e.g. "+i-w") to our tracked mode set.
fn apply_user_modes(current: &mut String, modes: &str) {
    let mut adding = true;
    for c in modes.chars() {
        match c {
            '+' => adding = true,
            '-' => adding = false,
            _ if c.is_ascii_alphanumeric() => {
                if adding {
                    if !current.contains(c) {
                        current.push(c);
                    }
                } else {
                    current.retain(|x| x != c);
                }
            }
            _ => {}
        }
    }
}

fn handle_numeric(ctx: &mut Context, fx: &mut Effects, resp: Response, args: &[String]) {
    let server_id = ctx.server_id.to_string();
    let code = resp as u16;
    match resp {
        // 302 USERHOST: "<nick>[*]=<+|-><user>@<host>". Pull our own host for the
        // DCC IP auto-detect (mIRC's "Server" lookup method).
        Response::RPL_USERHOST => {
            if let Some(reply) = args.last() {
                for tok in reply.split_whitespace() {
                    if let Some((who, rest)) = tok.split_once('=') {
                        if who.trim_end_matches('*').eq_ignore_ascii_case(&ctx.state.nick) {
                            if let Some((_, host)) = rest.split_once('@') {
                                fx.events.push(UiEvent::DccLocalHost {
                                    server_id: server_id.clone(),
                                    host: host.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
        Response::RPL_WELCOME => {
            ctx.state.registered = true;
            ctx.state.connect_time = now_secs();
            if let Some(nick) = args.first() {
                ctx.state.nick = nick.clone();
            }
            fx.events.push(UiEvent::Registered {
                server_id,
                nick: ctx.state.nick.clone(),
            });
            // Ask the server for our own host so DCC can auto-detect the IP to
            // advertise (mIRC's "Server" lookup). The 302 reply drives DccLocalHost.
            fx.outgoing.push(format!("USERHOST {}", ctx.state.nick));
            // NickServ identify (when not already authenticated via SASL).
            if ctx.profile.nickserv && !ctx.auth.sasl_succeeded {
                if let Some(pw) = ctx
                    .profile
                    .account_password
                    .as_deref()
                    .filter(|p| !p.is_empty())
                {
                    let acct = ctx.profile.account();
                    let line = if acct != ctx.profile.nick {
                        format!("PRIVMSG NickServ :IDENTIFY {acct} {pw}")
                    } else {
                        format!("PRIVMSG NickServ :IDENTIFY {pw}")
                    };
                    fx.outgoing.push(line);
                }
            }
            if ctx.profile.ircx {
                fx.outgoing.push("IRCX".to_string());
            }
            // Defer the autojoin until after `on CONNECT` runs, so a script can
            // skip/delay it with `/autojoin`. The connection task does the JOINs.
            fx.autojoin = ctx.profile.autojoin.clone();
        }
        Response::RPL_SASLSUCCESS => {
            fx.outgoing.extend(auth::on_sasl_result(ctx.auth, true));
        }
        Response::ERR_SASLFAIL
        | Response::ERR_SASLTOOLONG
        | Response::ERR_SASLABORT
        | Response::ERR_SASLALREADY
        | Response::ERR_NICKLOCKED => {
            fx.events.push(UiEvent::Error {
                server_id,
                message: format!("SASL: {}", args.last().cloned().unwrap_or_default()),
            });
            fx.outgoing.extend(auth::on_sasl_result(ctx.auth, false));
        }
        Response::RPL_BANLIST => {
            // [nick, channel, banmask, ...] — populate the channel ban list.
            if let (Some(channel), Some(mask)) = (args.get(1), args.get(2)) {
                ctx.state.set_ban(channel, mask, true);
                fx.bans_changed = true;
            }
        }
        Response::RPL_TOPIC => {
            // [nick, channel, topic]
            if let (Some(channel), Some(topic)) = (args.get(1), args.get(2)) {
                if let Some(ch) = ctx.state.channels.get_mut(channel) {
                    ch.topic = Some(topic.clone());
                }
                fx.events.push(UiEvent::Topic {
                    server_id,
                    channel: channel.clone(),
                    topic: Some(topic.clone()),
                    set_by: None,
                });
            }
        }
        Response::RPL_NAMREPLY => {
            // [nick, symbol, channel, "space separated names"]
            if let (Some(channel), Some(names)) = (args.get(2), args.last()) {
                let entry = ctx.names_accum.entry(channel.clone()).or_default();
                for name in names.split_whitespace() {
                    entry.push(name.to_string());
                }
            }
        }
        Response::RPL_ENDOFNAMES => {
            // [nick, channel, "End of /NAMES list"]
            if let Some(channel) = args.get(1) {
                if let Some(names) = ctx.names_accum.remove(channel) {
                    let parsed: Vec<(String, String)> = names
                        .iter()
                        .map(|e| ctx.state.isupport.split_prefixes(e))
                        .collect();
                    // With userhost-in-names, each entry is `nick!user@host`:
                    // split off the bare nick and record the address ($ial).
                    let mut members: Vec<(String, String)> = Vec::new();
                    for (prefixes, rest) in parsed {
                        let nick = match rest.split_once('!') {
                            Some((n, _)) => {
                                ctx.state.record_address(n, rest.clone());
                                n.to_string()
                            }
                            None => rest,
                        };
                        members.push((nick, prefixes));
                    }
                    let ch = ctx.state.channels.entry(channel.clone()).or_default();
                    ch.members.clear();
                    for (nick, prefixes) in members {
                        ch.members.insert(nick, prefixes);
                    }
                    fx.events.push(UiEvent::Names {
                        server_id,
                        channel: channel.clone(),
                        members: ch.member_list(),
                    });
                }
            }
        }
        Response::RPL_ISUPPORT => {
            // [nick, TOKEN=val, TOKEN=val, ..., ":are supported by this server"]
            for token in args.iter().skip(1) {
                if token.contains('=') {
                    ctx.state.isupport.parse_token(token);
                }
            }
            fx.events.push(UiEvent::Isupport {
                server_id,
                chan_types: ctx.state.isupport.chan_types.clone(),
                prefixes: ctx.state.isupport.prefix_chars(),
            });
        }
        Response::RPL_WHOISUSER
        | Response::RPL_WHOISSERVER
        | Response::RPL_WHOISOPERATOR
        | Response::RPL_WHOISIDLE
        | Response::RPL_WHOISCHANNELS
        | Response::RPL_WHOISCERTFP
        | Response::RPL_AWAY => {
            // Accumulate WHOIS detail lines keyed by the subject nick.
            if let Some(nick) = args.get(1).cloned() {
                let line = whois_line(code, args);
                ctx.whois_accum.entry(nick).or_default().push(line);
            }
        }
        Response::RPL_NOWAWAY => {
            ctx.state.away = true;
            ctx.state.away_time = now_secs();
            fx.events.push(UiEvent::SelfAway { server_id, away: true });
        }
        Response::RPL_UNAWAY => {
            ctx.state.away = false;
            ctx.state.away_time = 0;
            fx.events.push(UiEvent::SelfAway { server_id, away: false });
        }
        Response::RPL_ENDOFWHOIS => {
            if let Some(nick) = args.get(1).cloned() {
                let lines = ctx.whois_accum.remove(&nick).unwrap_or_default();
                fx.events.push(UiEvent::Whois {
                    server_id,
                    nick,
                    lines,
                });
            }
        }
        // Nick taken/unavailable: during registration, try an alternative.
        Response::ERR_NICKNAMEINUSE
        | Response::ERR_NICKCOLLISION
        | Response::ERR_UNAVAILRESOURCE => {
            if ctx.state.registered {
                fx.events.push(UiEvent::Error {
                    server_id,
                    message: format!("[{code}] {}", args.get(2).cloned().unwrap_or_default()),
                });
            } else {
                ctx.state.nick_attempts += 1;
                if ctx.state.nick_attempts > 8 {
                    fx.events.push(UiEvent::Error {
                        server_id,
                        message: "Could not find an available nickname.".to_string(),
                    });
                } else {
                    let candidate = next_nick(ctx.profile, ctx.state.nick_attempts);
                    ctx.state.nick = candidate.clone();
                    fx.outgoing.push(format!("NICK {candidate}"));
                    fx.events.push(UiEvent::Echo {
                        server_id,
                        target: "(status)".to_string(),
                        text: format!("Nickname in use — trying {candidate}…"),
                    });
                }
            }
        }
        // Channel list (LIST). [nick, channel, count, ":topic"]
        Response::RPL_LIST => {
            if let Some(channel) = args.get(1).cloned() {
                fx.events.push(UiEvent::ListEntry {
                    server_id,
                    channel,
                    users: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
                    topic: args.get(3).cloned().unwrap_or_default(),
                });
            }
        }
        Response::RPL_LISTEND => fx.events.push(UiEvent::ListEnd { server_id }),
        // Start-of-list is just a header; don't surface it as a numeric.
        Response::RPL_LISTSTART => {}
        // Quiet "CAP Unknown command" on servers without IRCv3 CAP.
        Response::ERR_UNKNOWNCOMMAND if args.get(1).map(|s| s.as_str()) == Some("CAP") => {}
        _ => {
            // Fold any other numeric that arrives during an in-progress WHOIS
            // into that block (covers account/secure/host/modes and any
            // server-specific whois numerics we don't format explicitly).
            let in_whois = args.get(1).is_some_and(|n| ctx.whois_accum.contains_key(n));
            if in_whois {
                let nick = args[1].clone();
                let line = whois_line(code, args);
                if !line.trim().is_empty() {
                    ctx.whois_accum.entry(nick).or_default().push(line);
                }
            } else {
                fx.events.push(UiEvent::Numeric {
                    server_id,
                    code,
                    args: args.to_vec(),
                });
            }
        }
    }
}

/// Builds a CTCP reply payload for a request, or None if unsupported.
fn ctcp_reply(cmd: &str, rest: &str) -> Option<String> {
    match cmd.to_ascii_uppercase().as_str() {
        "VERSION" => Some(format!(
            "VERSION jIRC {} - a modern open-source IRC client",
            env!("CARGO_PKG_VERSION")
        )),
        "PING" => Some(format!("PING {rest}")),
        "TIME" => Some(format!("TIME {}", ctcp_time())),
        "FINGER" => Some("FINGER jIRC user".to_string()),
        "USERINFO" => Some("USERINFO jIRC user".to_string()),
        "SOURCE" => Some("SOURCE https://github.com/alkaholix/jirc".to_string()),
        "CLIENTINFO" => Some(
            "CLIENTINFO ACTION CLIENTINFO FINGER PING SOURCE TIME USERINFO VERSION".to_string(),
        ),
        _ => None,
    }
}

/// Renders a CTCP reply for the status window. A PING reply echoes the
/// millisecond timestamp we sent with `/ctcp <nick> ping`, so turn it back into
/// a round-trip latency; anything else is shown as-is.
fn ctcp_reply_pretty(ctcp: &str) -> String {
    if let Some(ts) = ctcp
        .strip_prefix("PING ")
        .and_then(|s| s.trim().parse::<u128>().ok())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if now >= ts && now - ts <= 3_600_000 {
            return format!("PING reply: {:.3} seconds", (now - ts) as f64 / 1000.0);
        }
    }
    ctcp.to_string()
}

/// A local-time timestamp for CTCP TIME, e.g. `Thu 2026-06-25 14:32:10 +12:00`
/// (weekday + date + time + offset). Uses the OS timezone — NZST/NZDT in New
/// Zealand, the local zone elsewhere — matching mIRC, which replies with your
/// own clock (and handling DST for free).
fn ctcp_time() -> String {
    chrono::Local::now()
        .format("%a %Y-%m-%d %H:%M:%S %:z")
        .to_string()
}

/// Formats a single WHOIS numeric into a human-readable line.
/// Strips an IRCX/MSN-Chat font descriptor prefix from a message, e.g.
/// `"\x01S Tahoma;0 hello"` -> `Some("hello")`
/// (`\x01<effects> <fontname>;<color>[;…] <message>`).
///
/// The leading `\x01` is what marks a font-tagged message (it's what made these
/// read as a CTCP named "S"). We *require* it: plain typed text never starts
/// with `\x01`, so a normal line that merely contains `"word;digits "` (e.g.
/// "see you at 3;30 tomorrow") is left untouched. A genuine `ACTION` emote is
/// also left for CTCP handling. Returns `None` when the text isn't font-tagged.
fn strip_ircx_font(text: &str) -> Option<&str> {
    use std::sync::OnceLock;
    let rest = text.strip_prefix('\u{1}')?;
    // Don't swallow a real /me — let it fall through to CTCP/ACTION handling.
    if rest
        .split(' ')
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("ACTION"))
    {
        return None;
    }
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"^\S+ .+?;\d+ ").unwrap());
    let m = re.find(rest)?;
    Some(rest[m.end()..].trim_end_matches('\u{1}'))
}

fn whois_line(code: u16, args: &[String]) -> String {
    let rest = |from: usize| args.iter().skip(from).cloned().collect::<Vec<_>>().join(" ");
    match code {
        311 => format!("{} ({}@{}): {}", args.get(1).cloned().unwrap_or_default(),
            args.get(2).cloned().unwrap_or_default(),
            args.get(3).cloned().unwrap_or_default(),
            args.get(5).cloned().unwrap_or_default()),
        312 => format!("server: {} ({})", args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default()),
        313 => "is an IRC operator".to_string(),
        317 => format!("idle: {}s, signon: {}", args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default()),
        319 => format!("channels: {}", rest(2)),
        330 => format!("account: {}", args.get(2).cloned().unwrap_or_default()),
        338 => format!("actual: {}", rest(2)),
        378 => format!("host: {}", rest(2)),
        379 => format!("modes: {}", rest(2)),
        310 => "is available for help".to_string(),
        320 => rest(2),
        671 => "using a secure connection".to_string(),
        301 => format!("away: {}", rest(2)),
        _ => rest(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_modes_accumulate() {
        let mut m = String::new();
        apply_user_modes(&mut m, "+ix");
        assert_eq!(m, "ix");
        apply_user_modes(&mut m, "+w-i");
        assert_eq!(m, "xw");
        apply_user_modes(&mut m, "-xw");
        assert_eq!(m, "");
    }

    #[test]
    fn decodes_cesu8_emoji_and_falls_back() {
        // 🦊 (U+1F98A) as a .NET/IRCX CESU-8 surrogate pair: ED A0 BE ED B6 8A —
        // illegal in plain UTF-8, so this is the `>í ¾í¶…` mojibake case.
        let bytes = b"\x3e\xED\xA0\xBE\xED\xB6\x8A5833"; // ">🦊5833"
        assert_eq!(decode_irc_line(bytes), ">🦊5833");
        // Plain ASCII and ordinary (4-byte) UTF-8 still pass through unchanged.
        assert_eq!(decode_irc_line(b"JOIN #chan"), "JOIN #chan");
        assert_eq!(decode_irc_line("café 🚀".as_bytes()), "café 🚀");
        // A stray non-UTF-8 byte still maps to its Latin-1 code point.
        assert_eq!(decode_irc_line(&[0x68, 0x69, 0xC9]), "hiÉ");
    }

    fn profile() -> ServerProfile {
        ServerProfile {
            id: Some("s1".into()),
            name: "test".into(),
            host: "localhost".into(),
            port: 6667,
            tls: false,
            tls_insecure: false,
            ircx: false,
            sasl: false,
            account: None,
            account_password: None,
            nickserv: false,
            auto_reconnect: false,
            proxy: None,
            nick: "me".into(),
            alt_nick: None,
            username: None,
            realname: None,
            password: None,
            ntlm: false,
            ntlm_domain: None,
            ntlm_user: None,
            ntlm_password: None,
            autojoin: vec![],
        }
    }

    fn run_line(state: &mut SessionState, accum: &mut HashMap<String, Vec<String>>, line: &str) -> Effects {
        let p = profile();
        let mut auth = AuthState::default();
        let mut whois = HashMap::new();
        let mut ctx = Context {
            server_id: "s1",
            profile: &p,
            state,
            names_accum: accum,
            whois_accum: &mut whois,
            auth: &mut auth,
        };
        process_message(&mut ctx, line, line.parse::<Message>().unwrap())
    }

    #[test]
    fn decodes_non_utf8_as_latin1() {
        // valid UTF-8 is preserved
        assert_eq!(decode_irc_line("héllo".as_bytes()), "héllo");
        // a lone 0xe9 byte is invalid UTF-8 -> Latin-1 fallback maps it to 'é'
        let s = decode_irc_line(b"caf\xe9");
        assert_eq!(s, "café");
    }

    #[test]
    fn responds_to_ping() {
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, "PING :tok123");
        assert_eq!(fx.outgoing, vec!["PONG :tok123".to_string()]);
    }

    #[test]
    fn tracks_join_and_names() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        run_line(&mut s, &mut accum, ":me!u@h JOIN #test");
        run_line(&mut s, &mut accum, ":srv 353 me = #test :@alice +bob me");
        let fx = run_line(&mut s, &mut accum, ":srv 366 me #test :End of /NAMES list");
        assert!(matches!(fx.events.last(), Some(UiEvent::Names { .. })));
        let ch = &s.channels["#test"];
        assert_eq!(ch.members["alice"], "@");
        assert_eq!(ch.members["bob"], "+");
        assert!(ch.members.contains_key("me"));
    }

    #[test]
    fn parses_guest_prefixed_nick() {
        // IRC7/MSN nicks can start with a status char like '>'. Confirm we can
        // still get a message out of such a line (sanitised if irc-proto rejects it).
        let line = ":>HappyWombat61!CF86@GateKeeper PRIVMSG #c :hi there";
        let mut s = SessionState { nick: "me".into(), ..Default::default() };
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, line);
        let got = fx.events.iter().find_map(|e| match e {
            UiEvent::Message { from, text, .. } => Some((from.clone(), text.clone())),
            _ => None,
        });
        assert_eq!(got, Some((Some(">HappyWombat61".into()), "hi there".into())));
    }

    #[test]
    fn strips_ircx_font_descriptor() {
        // MSN-Chat font tags always carry the leading \x01.
        assert_eq!(strip_ircx_font("\u{1}S Tahoma;0 hkjhkh"), Some("hkjhkh"));
        assert_eq!(strip_ircx_font("\u{1}S Times New Roman;0 hi there"), Some("hi there"));
        assert_eq!(strip_ircx_font("\u{1}S Tahoma;0 (B)"), Some("(B)"));
        assert_eq!(strip_ircx_font("\u{1}S Tahoma;0 hi\u{1}"), Some("hi"));
        // Real MSN-Chat messages: emoji, punctuation, multiple words.
        assert_eq!(strip_ircx_font("\u{1}S Tahoma;0 🦋: Sup"), Some("🦋: Sup"));
        assert_eq!(
            strip_ircx_font("\u{1}S Tahoma;0 Nice work, you jerk."),
            Some("Nice work, you jerk.")
        );
        // Without the \x01 marker we don't touch the text, so a normal line that
        // happens to contain "word;digits " is never eaten.
        assert_eq!(strip_ircx_font("S Tahoma;0 hkjhkh"), None);
        assert_eq!(strip_ircx_font("see you at 3;30 tomorrow"), None);
        assert_eq!(strip_ircx_font("just a normal message"), None);
        assert_eq!(strip_ircx_font("hello world"), None);
        // A real ACTION emote is left for CTCP handling, even with a ;digits.
        assert_eq!(strip_ircx_font("\u{1}ACTION rolls a 6;5 dice\u{1}"), None);
    }

    #[test]
    fn ircx_font_message_not_treated_as_ctcp() {
        let mut p = profile();
        p.ircx = true;
        let mut state = SessionState { nick: "me".into(), ..Default::default() };
        let mut names: HashMap<String, Vec<String>> = HashMap::new();
        let mut whois: HashMap<String, Vec<String>> = HashMap::new();
        let mut auth = AuthState::default();
        // Leading \x01 + font descriptor — must surface as a channel message,
        // not a "[CTCP S]" echo.
        let line = ":>Bob!h@GateKeeper PRIVMSG #c :\u{1}S Tahoma;0 hello there";
        let mut ctx = Context {
            server_id: "s1",
            profile: &p,
            state: &mut state,
            names_accum: &mut names,
            whois_accum: &mut whois,
            auth: &mut auth,
        };
        let fx = process_message(&mut ctx, line, line.parse::<Message>().unwrap());
        let text = fx.events.iter().find_map(|e| match e {
            UiEvent::Message { text, .. } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(text.as_deref(), Some("hello there"));
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Echo { text, .. } if text.contains("CTCP"))));
    }

    #[test]
    fn privmsg_strips_font_on_ircx() {
        let mut p = profile();
        p.ircx = true;
        let mut state = SessionState { nick: "me".into(), ..Default::default() };
        let mut names: HashMap<String, Vec<String>> = HashMap::new();
        let mut whois: HashMap<String, Vec<String>> = HashMap::new();
        let mut auth = AuthState::default();
        let line = ":>Bob!h@GateKeeper PRIVMSG #c :\u{1}S Tahoma;0 hkjhkh";
        let mut ctx = Context {
            server_id: "s1",
            profile: &p,
            state: &mut state,
            names_accum: &mut names,
            whois_accum: &mut whois,
            auth: &mut auth,
        };
        let fx = process_message(&mut ctx, line, line.parse::<Message>().unwrap());
        let text = fx.events.iter().find_map(|e| match e {
            UiEvent::Message { text, .. } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(text.as_deref(), Some("hkjhkh"));
    }

    #[test]
    fn privmsg_strips_font_without_ircx_flag() {
        // Buzzen/MSN-Chat font tags must be stripped even when the profile is
        // NOT flagged IRCX (the font tag is self-identifying) — otherwise they
        // showed up as "[CTCP S]" in the status window instead of the channel.
        let p = profile(); // ircx defaults to false
        assert!(!p.ircx);
        let mut state = SessionState { nick: "me".into(), ..Default::default() };
        let mut names: HashMap<String, Vec<String>> = HashMap::new();
        let mut whois: HashMap<String, Vec<String>> = HashMap::new();
        let mut auth = AuthState::default();
        let line = ":JD!h@MicrosoftPassport PRIVMSG %#Lobby :\u{1}S Tahoma;0 🦋: Sup";
        let mut ctx = Context {
            server_id: "s1",
            profile: &p,
            state: &mut state,
            names_accum: &mut names,
            whois_accum: &mut whois,
            auth: &mut auth,
        };
        let fx = process_message(&mut ctx, line, line.parse::<Message>().unwrap());
        let text = fx.events.iter().find_map(|e| match e {
            UiEvent::Message { text, target, .. } => Some((text.clone(), target.clone())),
            _ => None,
        });
        assert_eq!(text, Some(("🦋: Sup".into(), "%#Lobby".into())));
        // ...and it must NOT be surfaced as a CTCP in the status window.
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Echo { text, .. } if text.contains("CTCP"))));
    }

    #[test]
    fn unknown_raw_numeric_is_surfaced() {
        // A numeric irc-proto doesn't know goes to Raw; it must still reach the
        // UI as a Numeric (so errors show / trace works) rather than vanish.
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":srv 1234 me :some server message");
        assert!(fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Numeric { code: 1234, .. })));
    }

    #[test]
    fn whois_folds_unknown_numerics() {
        let p = profile();
        let mut state = SessionState { nick: "me".into(), ..Default::default() };
        let mut names: HashMap<String, Vec<String>> = HashMap::new();
        let mut whois: HashMap<String, Vec<String>> = HashMap::new();
        let mut auth = AuthState::default();
        let mut run = |line: &str| {
            let mut ctx = Context {
                server_id: "s1",
                profile: &p,
                state: &mut state,
                names_accum: &mut names,
                whois_accum: &mut whois,
                auth: &mut auth,
            };
            process_message(&mut ctx, line, line.parse::<Message>().unwrap())
        };
        run(":srv 311 me bob bob host * :Real Name");
        // 330 (account) and 1234 (server-specific) aren't in the explicit arm.
        run(":srv 330 me bob coolacct :is logged in as");
        run(":srv 1234 me bob :some extra info");
        let fx = run(":srv 318 me bob :End of WHOIS");
        let lines = fx
            .events
            .iter()
            .find_map(|e| match e {
                UiEvent::Whois { lines, .. } => Some(lines.clone()),
                _ => None,
            })
            .expect("whois event");
        assert!(lines.iter().any(|l| l.contains("account: coolacct")));
        assert!(lines.iter().any(|l| l.contains("some extra info")));
    }

    #[test]
    fn ctcp_version_autoreply() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":bob!u@h PRIVMSG me :\u{1}VERSION\u{1}");
        assert!(fx
            .outgoing
            .iter()
            .any(|l| l.starts_with("NOTICE bob :\u{1}VERSION jIRC")));
        // The UI sees an Echo, not a raw Message...
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::Message { .. })));
        // ...but scripts get the request as a Message so `on CTCP` fires live.
        assert!(fx
            .script_events
            .iter()
            .any(|e| matches!(e, UiEvent::Message { text, .. } if text.contains("VERSION"))));
    }

    #[test]
    fn ctcp_finger_userinfo_source_autoreply() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        for (req, reply) in [
            ("FINGER", "FINGER jIRC"),
            ("USERINFO", "USERINFO jIRC"),
            ("SOURCE", "SOURCE https://github.com/alkaholix/jirc"),
        ] {
            let fx = run_line(&mut s, &mut accum, &format!(":bob!u@h PRIVMSG me :\u{1}{req}\u{1}"));
            assert!(
                fx.outgoing
                    .iter()
                    .any(|l| l.starts_with(&format!("NOTICE bob :\u{1}{reply}"))),
                "no auto-reply for {req}: {:?}",
                fx.outgoing
            );
        }
    }

    #[test]
    fn ctcp_reply_routes_to_scripts_not_ui_message() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        // A CTCP reply arrives as a NOTICE \x01..\x01.
        let fx = run_line(&mut s, &mut accum, ":bob!u@h NOTICE me :\u{1}VERSION jIRC 1.0\u{1}");
        // UI: a readable Echo, no raw Message, and a reply is never auto-replied to.
        assert!(fx.events.iter().any(
            |e| matches!(e, UiEvent::Echo { text, .. } if text.contains("CTCP reply from bob"))
        ));
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::Message { .. })));
        assert!(fx.outgoing.is_empty());
        // Scripts: a Notice Message so `on CTCPREPLY` fires.
        assert!(fx.script_events.iter().any(|e| matches!(
            e,
            UiEvent::Message { kind: MessageKind::Notice, text, .. } if text.contains("VERSION")
        )));
    }

    #[test]
    fn incoming_dcc_offer_is_surfaced() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(
            &mut s,
            &mut accum,
            ":bob!u@h PRIVMSG me :\u{1}DCC SEND readme.txt 3232235521 5000 12345\u{1}",
        );
        assert!(fx.events.iter().any(|e| matches!(
            e,
            UiEvent::Echo { text, .. }
                if text.contains("[DCC]") && text.contains("readme.txt") && text.contains("bob")
        )));
        // A DCC CTCP isn't echoed as a normal message and gets no auto-reply.
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::Message { .. })));
        assert!(fx.outgoing.is_empty());
    }

    #[test]
    fn ctcp_action_still_renders() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":bob!u@h PRIVMSG #c :\u{1}ACTION waves\u{1}");
        assert!(fx.events.iter().any(|e| matches!(e, UiEvent::Message { .. })));
        assert!(fx.outgoing.is_empty());
    }

    #[test]
    fn nick_in_use_tries_alternative_then_underscore() {
        let mut p = profile();
        p.nick = "bob".into();
        p.alt_nick = Some("bobby".into());
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let mut whois = HashMap::new();
        let mut auth = AuthState::default();
        let run = |s: &mut SessionState, accum: &mut HashMap<String, Vec<String>>, whois: &mut HashMap<String, Vec<String>>, auth: &mut AuthState| {
            let mut ctx = Context {
                server_id: "s1",
                profile: &p,
                state: s,
                names_accum: accum,
                whois_accum: whois,
                auth,
            };
            let raw = ":srv 433 * bob :Nickname is already in use";
            process_message(&mut ctx, raw, raw.parse().unwrap())
        };
        let fx1 = run(&mut s, &mut accum, &mut whois, &mut auth);
        assert_eq!(fx1.outgoing, vec!["NICK bobby".to_string()]);
        let fx2 = run(&mut s, &mut accum, &mut whois, &mut auth);
        assert_eq!(fx2.outgoing, vec!["NICK bob_".to_string()]);
    }

    #[test]
    fn kick_self_removes_channel() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.upsert_member("#c", "me", String::new());
        s.upsert_member("#c", "bob", String::new());
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":op!u@h KICK #c me :bye");
        assert!(fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Kick { is_self: true, .. })));
        assert!(!s.channels.contains_key("#c"));
    }

    #[test]
    fn server_time_tag_threads_into_message() {
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let fx = run_line(
            &mut s,
            &mut accum,
            "@time=2021-01-02T03:04:05.000Z :bob!u@h PRIVMSG #c :hi",
        );
        match fx.events.iter().find(|e| matches!(e, UiEvent::Message { .. })) {
            Some(UiEvent::Message { time, .. }) => {
                assert_eq!(time.as_deref(), Some("2021-01-02T03:04:05.000Z"));
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn invite_emits_event() {
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":bob!u@h INVITE me #cool");
        match fx.events.iter().find(|e| matches!(e, UiEvent::Invite { .. })) {
            Some(UiEvent::Invite { from, channel, .. }) => {
                assert_eq!(from.as_deref(), Some("bob"));
                assert_eq!(channel, "#cool");
            }
            _ => panic!("expected Invite"),
        }
    }

    #[test]
    fn user_mode_emits_mode() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":me!u@h MODE me +ix");
        match fx.events.iter().find(|e| matches!(e, UiEvent::Mode { .. })) {
            Some(UiEvent::Mode { target, modes, .. }) => {
                assert_eq!(target, "me");
                assert!(modes.contains('i') && modes.contains('x'), "{modes}");
            }
            _ => panic!("expected Mode"),
        }
    }

    #[test]
    fn away_change_lists_shared_channels() {
        let mut s = SessionState::default();
        s.upsert_member("#c", "bob", String::new());
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":bob!u@h AWAY :brb");
        match fx.events.iter().find(|e| matches!(e, UiEvent::AwayChange { .. })) {
            Some(UiEvent::AwayChange { away, channels, .. }) => {
                assert!(*away);
                assert_eq!(channels, &vec!["#c".to_string()]);
            }
            _ => panic!("expected AwayChange"),
        }
    }

    #[test]
    fn ircx_channel_mode_keeps_prefix_arg() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov).@+");
        s.upsert_member("%#chan", "owner", ".".to_string());
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":op!u@h MODE %#chan -q owner");
        match fx.events.iter().find(|e| matches!(e, UiEvent::Mode { .. })) {
            Some(UiEvent::Mode { target, modes, .. }) => {
                assert_eq!(target, "%#chan");
                assert_eq!(modes, "-q owner");
            }
            _ => panic!("expected Mode"),
        }
        // owner's '.' (founder) prefix was removed
        assert_eq!(s.channels["%#chan"].members["owner"], "");
    }

    #[test]
    fn owner_granted_only_when_we_get_plus_q() {
        let mut s = SessionState { nick: "me".into(), ..Default::default() };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov)~@+");
        let mut accum = HashMap::new();
        // +q on us -> OwnerGranted for the channel.
        let fx = run_line(&mut s, &mut accum, ":host!u@h MODE %#room +q me");
        assert!(fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerGranted { channel, .. } if channel == "%#room")));
        // +q on someone else, or -q on us -> no OwnerGranted.
        let fx = run_line(&mut s, &mut accum, ":host!u@h MODE %#room +q bob");
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::OwnerGranted { .. })));
        let fx = run_line(&mut s, &mut accum, ":host!u@h MODE %#room -q me");
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::OwnerGranted { .. })));
    }

    #[test]
    fn owner_revoked_only_when_someone_else_takes_our_q() {
        let mut s = SessionState { nick: "me".into(), ..Default::default() };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov)~@+");
        let mut accum = HashMap::new();
        // -q on us by someone else -> OwnerRevoked naming the offender.
        let fx = run_line(&mut s, &mut accum, ":taker!u@h MODE %#room -q me");
        assert!(fx.events.iter().any(|e| matches!(
            e,
            UiEvent::OwnerRevoked { channel, by, .. } if channel == "%#room" && by == "taker"
        )));
        // -q on someone else, or -q we set ourselves -> no OwnerRevoked.
        let fx = run_line(&mut s, &mut accum, ":taker!u@h MODE %#room -q bob");
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::OwnerRevoked { .. })));
        let fx = run_line(&mut s, &mut accum, ":me!u@h MODE %#room -q me");
        assert!(!fx.events.iter().any(|e| matches!(e, UiEvent::OwnerRevoked { .. })));
    }

    #[test]
    fn ircx_backspace_space_channel_mode() {
        // IRCX encodes a space in a name as 0x08; it must stay one token.
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov).@+");
        let chan = "%#The\u{08}Lobby";
        s.upsert_member(chan, "bob", String::new());
        let mut accum = HashMap::new();
        let raw = format!(":op!u@h MODE {chan} +o bob");
        let fx = run_line(&mut s, &mut accum, &raw);
        match fx.events.iter().find(|e| matches!(e, UiEvent::Mode { .. })) {
            Some(UiEvent::Mode { target, modes, .. }) => {
                assert_eq!(target, chan);
                assert_eq!(modes, "+o bob");
            }
            _ => panic!("expected Mode"),
        }
        assert_eq!(s.channels[chan].members["bob"], "@");
    }

    #[test]
    fn mode_change_updates_prefix() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        s.upsert_member("#test", "bob", String::new());
        run_line(&mut s, &mut accum, ":op!u@h MODE #test +o bob");
        assert_eq!(s.channels["#test"].members["bob"], "@");
    }

    /// Live smoke test against Libera.Chat. Ignored by default (hits the
    /// network); run with: `cargo test --manifest-path src-tauri/Cargo.toml
    /// -- --ignored --nocapture live_libera`.
    #[tokio::test]
    #[ignore]
    async fn live_libera() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpStream;
        use tokio::time::{timeout, Duration};

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 100000;
        let nick = format!("jircsm{suffix}");
        let channel = format!("##jirc-smoke{suffix}");

        let mut p = profile();
        p.host = "irc.libera.chat".into();
        p.port = 6667;
        p.nick = nick.clone();
        p.autojoin = vec![channel.clone()];

        let stream = TcpStream::connect((p.host.as_str(), p.port))
            .await
            .expect("connect");
        async fn send(w: &mut tokio::net::tcp::OwnedWriteHalf, line: &str) {
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.write_all(b"\r\n").await;
            let _ = w.flush().await;
        }

        let (read_half, mut write_half) = stream.into_split();
        send(&mut write_half, &format!("NICK {nick}")).await;
        send(&mut write_half, &format!("USER {nick} 0 * :jIRC smoke test")).await;

        let mut state = SessionState {
            nick: nick.clone(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let mut whois = HashMap::new();
        let mut auth = AuthState::default();
        let mut lines = BufReader::new(read_half).lines();

        let mut registered = false;
        let mut got_names = false;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        while tokio::time::Instant::now() < deadline && !got_names {
            let line = match timeout(Duration::from_secs(20), lines.next_line()).await {
                Ok(Ok(Some(l))) => l,
                _ => break,
            };
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            if line.is_empty() {
                continue;
            }
            eprintln!("<< {line}");
            let Ok(msg) = line.parse::<Message>() else {
                continue;
            };
            let mut ctx = Context {
                server_id: "live",
                profile: &p,
                state: &mut state,
                names_accum: &mut accum,
                whois_accum: &mut whois,
                auth: &mut auth,
            };
            let fx = process_message(&mut ctx, &line, msg);
            for out in &fx.outgoing {
                if !out.starts_with("PASS ") {
                    eprintln!(">> {out}");
                }
                send(&mut write_half, out).await;
            }
            for ev in &fx.events {
                match ev {
                    UiEvent::Registered { .. } => registered = true,
                    UiEvent::Names { channel: c, .. } if c == &channel => got_names = true,
                    _ => {}
                }
            }
        }

        send(&mut write_half, "QUIT :smoke test done").await;

        assert!(registered, "did not receive RPL_WELCOME");
        assert!(got_names, "did not receive NAMES for {channel}");
        assert!(
            state.channels[&channel].members.contains_key(&nick),
            "our nick missing from channel roster"
        );
    }

    /// Live TLS smoke test against Libera.Chat:6697. Ignored by default.
    /// Run with: `cargo test ... -- --ignored --nocapture live_libera_tls`.
    #[tokio::test]
    #[ignore]
    async fn live_libera_tls() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::time::{timeout, Duration};

        let _ = rustls::crypto::ring::default_provider().install_default();

        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() % 100000;
        let nick = format!("jirctls{suffix}");
        let channel = format!("##jirc-tls{suffix}");
        let mut p = profile();
        p.host = "irc.libera.chat".into();
        p.port = 6697;
        p.tls = true;
        p.nick = nick.clone();
        p.autojoin = vec![channel.clone()];

        let stream = stream::connect(&p).await.expect("tls connect");
        let (read_half, mut write_half) = tokio::io::split(stream);

        async fn send<W: AsyncWrite + Unpin>(w: &mut W, line: &str) {
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.write_all(b"\r\n").await;
            let _ = w.flush().await;
        }
        send(&mut write_half, "CAP LS 302").await;
        send(&mut write_half, &format!("NICK {nick}")).await;
        send(&mut write_half, &format!("USER {nick} 0 * :jIRC tls test")).await;

        let mut state = SessionState { nick: nick.clone(), ..Default::default() };
        let mut accum = HashMap::new();
        let mut whois = HashMap::new();
        let mut auth = AuthState::default();
        let mut lines = BufReader::new(read_half).lines();
        let mut registered = false;
        let mut got_names = false;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        while tokio::time::Instant::now() < deadline && !got_names {
            let line = match timeout(Duration::from_secs(20), lines.next_line()).await {
                Ok(Ok(Some(l))) => l,
                _ => break,
            };
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            if line.is_empty() {
                continue;
            }
            let Ok(msg) = line.parse::<Message>() else { continue };
            let mut ctx = Context {
                server_id: "tls",
                profile: &p,
                state: &mut state,
                names_accum: &mut accum,
                whois_accum: &mut whois,
                auth: &mut auth,
            };
            let fx = process_message(&mut ctx, &line, msg);
            for out in &fx.outgoing {
                send(&mut write_half, out).await;
            }
            for ev in &fx.events {
                match ev {
                    UiEvent::Registered { .. } => registered = true,
                    UiEvent::Names { channel: c, .. } if c == &channel => got_names = true,
                    _ => {}
                }
            }
        }
        send(&mut write_half, "QUIT :tls smoke done").await;
        assert!(registered, "did not register over TLS");
        assert!(got_names, "did not receive NAMES over TLS");
    }

    #[test]
    fn welcome_triggers_autojoin() {
        let mut p = profile();
        p.autojoin = vec!["#jirc".into()];
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let mut whois = HashMap::new();
        let mut auth = AuthState::default();
        let mut ctx = Context {
            server_id: "s1",
            profile: &p,
            state: &mut s,
            names_accum: &mut accum,
            whois_accum: &mut whois,
            auth: &mut auth,
        };
        let fx = process_message(&mut ctx, ":srv 001 me :Welcome", ":srv 001 me :Welcome".parse().unwrap());
        // The autojoin is deferred to the connection task (after `on CONNECT`),
        // so it's reported via `fx.autojoin`, not sent inline.
        assert_eq!(fx.autojoin, vec!["#jirc".to_string()]);
        assert!(!fx.outgoing.iter().any(|l| l.starts_with("JOIN")));
        assert_eq!(s.nick, "me");
    }
}
