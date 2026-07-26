//! A single IRC connection: TCP transport, registration, the read loop, and
//! protocol-to-UI-event translation for the **standard** dialect.
//!
//! The protocol logic ([`process_message`]) is pure: it takes a parsed message
//! and the session state and produces outgoing lines + UI events. The async
//! [`run`] loop wires that to a real socket and the Tauri event bus. IRCX
//! handling (Phase 1b) hangs off the `Command::Raw` arm.

use irc_proto::{CapSubCommand, Command, Message, Response};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::config::ServerProfile;
use crate::irc::auth::{self, AuthState};
use crate::irc::event::{Direction, MessageKind, UiEvent, IRC_EVENT};
use crate::irc::state::{SessionState, StateSnapshot};
use crate::irc::stream;
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
struct FloodLimiter {
    sent: VecDeque<Instant>,
}

impl FloodLimiter {
    fn delay(&mut self, config: &super::manager::FloodConfig, now: Instant) -> Duration {
        if !config.enabled {
            self.sent.clear();
            return Duration::ZERO;
        }
        let window = Duration::from_secs(config.seconds);
        while self
            .sent
            .front()
            .is_some_and(|sent| now.duration_since(*sent) >= window)
        {
            self.sent.pop_front();
        }
        let next_in_order = self.sent.back().copied().map_or(now, |last| last.max(now));
        let scheduled = if self.sent.len() < config.messages {
            next_in_order
        } else {
            (self.sent.front().copied().unwrap() + window).max(next_in_order)
        };
        while self
            .sent
            .front()
            .is_some_and(|sent| scheduled.duration_since(*sent) >= window)
        {
            self.sent.pop_front();
        }
        self.sent.push_back(scheduled);
        scheduled.saturating_duration_since(now)
    }
}

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
    /// Set when a numeric updates channel facts without emitting a normal
    /// state event (currently RPL_CHANNELMODEIS / 324).
    pub channel_state_changed: bool,
    /// Set when address/WHOX/mark state changes without a membership event.
    pub ial_changed: bool,
    /// Channels to auto-join once `on CONNECT` has run (populated at RPL_WELCOME).
    /// The connection task performs the JOINs, honoring `/autojoin` (`-s` skip,
    /// `-dN` delay) — so a script can control them from within `on CONNECT`.
    pub autojoin: Vec<String>,
    /// Profile commands to run after `on CONNECT` and before automatic joins.
    pub perform: Vec<String>,
    /// Parsed CTCP DCC negotiation, consumed by the stateful DCC manager in the
    /// async loop while keeping `process_message` itself pure and testable.
    pub dcc_message: Option<(String, crate::irc::dcc::DccMessage)>,
    /// Pre-departure script view for mIRC's delayed KICK/PART/QUIT nicklist and
    /// IAL update semantics. The post-departure snapshot is attached before
    /// scripts are dispatched and activated by `/updatenl`.
    pub script_state: Option<StateSnapshot>,
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

/// IRCv3 `echo-message` sends our own PRIVMSG/NOTICE/TAGMSG back from the
/// server. jIRC has already shown the local echo, so mIRC exposes these lines
/// to PARSELINE as `$parseem == $true` and does not display them again.
fn is_echoed_message(line: &str, state: &SessionState, auth: &AuthState) -> bool {
    if !auth.cap_enabled("echo-message") {
        return false;
    }
    let Ok(message) = line.parse::<Message>() else {
        return false;
    };
    let Some(nick) = message.source_nickname() else {
        return false;
    };
    if !state.isupport.names_equal(nick, &state.nick) {
        return false;
    }
    match message.command {
        Command::PRIVMSG(_, _) | Command::NOTICE(_, _) => true,
        Command::Raw(command, _) => command.eq_ignore_ascii_case("TAGMSG"),
        _ => false,
    }
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

/// Byte-exact form used by `/parseline -b` and `-u0`.
async fn write_bytes_line<W: AsyncWrite + Unpin>(
    w: &mut W,
    app: &AppHandle,
    server_id: &str,
    bytes: &[u8],
    append_crlf: bool,
) {
    let mut end = bytes.len();
    if append_crlf {
        while end > 0 && matches!(bytes[end - 1], b'\r' | b'\n') {
            end -= 1;
        }
    }
    let bytes = &bytes[..end];
    if w.write_all(bytes).await.is_err() || (append_crlf && w.write_all(b"\r\n").await.is_err()) {
        return;
    }
    let _ = w.flush().await;
    let line = decode_irc_line(bytes)
        .trim_end_matches(['\r', '\n'])
        .to_string();
    if !line.starts_with("PASS ") && !line.starts_with("AUTHENTICATE ") {
        emit(
            app,
            UiEvent::Raw {
                server_id: server_id.to_string(),
                direction: Direction::Out,
                line,
            },
        );
    }
}

/// Runs the outgoing PARSELINE pass, then writes the final bytes. Queued lines
/// are routed back through the connection manager and therefore run only after
/// this event/line has completed.
async fn write_parsed_line<W: AsyncWrite + Unpin>(
    w: &mut W,
    app: &AppHandle,
    server_id: &str,
    profile: &ServerProfile,
    state: &SessionState,
    bytes: &[u8],
    trigger_parseline: bool,
    parse_utf: bool,
    append_crlf: bool,
) {
    let mut final_bytes = bytes.to_vec();
    let mut final_append_crlf = append_crlf;
    if trigger_parseline {
        if let Some(engine) = app.try_state::<crate::script::ScriptEngine>() {
            let display = decode_irc_line(bytes);
            let ctx = crate::script::RunCtx {
                my_nick: &state.nick,
                network: &profile.name,
                server: &profile.host,
                data_dir: crate::script::script_data_dir(app),
                state: std::sync::Arc::new(state.snapshot()),
            };
            let mut outcome = crate::script::dispatch_parseline(
                &engine, &ctx, "out", &display, bytes, parse_utf, false,
            );
            if let Some(replacement) = outcome.current.take() {
                final_bytes = replacement;
            }
            final_append_crlf |= outcome.force_crlf;
            outcome.actions.extend(outcome.queued);
            if !outcome.actions.is_empty() {
                crate::script::apply_actions(
                    app,
                    server_id,
                    &state.nick,
                    &profile.name,
                    &profile.host,
                    outcome.actions,
                );
            }
        }
    }
    write_bytes_line(w, app, server_id, &final_bytes, final_append_crlf).await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_incoming_line<W: AsyncWrite + Unpin>(
    w: &mut W,
    app: &AppHandle,
    server_id: &str,
    profile: &ServerProfile,
    state: &mut SessionState,
    names_accum: &mut HashMap<String, Vec<String>>,
    whois_accum: &mut HashMap<String, Vec<String>>,
    auth: &mut AuthState,
    raw_bytes: &[u8],
    trigger_parseline: bool,
    parse_utf: bool,
    server_origin: bool,
) {
    let mut end = raw_bytes.len();
    while end > 0 && matches!(raw_bytes[end - 1], b'\r' | b'\n') {
        end -= 1;
    }
    let mut final_bytes = raw_bytes[..end].to_vec();
    let initial_display = decode_irc_line(&final_bytes);
    let parse_em = server_origin && is_echoed_message(&initial_display, state, auth);
    if trigger_parseline {
        if let Some(engine) = app.try_state::<crate::script::ScriptEngine>() {
            let ctx = crate::script::RunCtx {
                my_nick: &state.nick,
                network: &profile.name,
                server: &profile.host,
                data_dir: crate::script::script_data_dir(app),
                state: std::sync::Arc::new(state.snapshot()),
            };
            let mut outcome = crate::script::dispatch_parseline(
                &engine,
                &ctx,
                "in",
                &initial_display,
                &final_bytes,
                parse_utf,
                parse_em,
            );
            if let Some(replacement) = outcome.current.take() {
                final_bytes = replacement;
            }
            outcome.actions.extend(outcome.queued);
            if !outcome.actions.is_empty() {
                crate::script::apply_actions(
                    app,
                    server_id,
                    &state.nick,
                    &profile.name,
                    &profile.host,
                    outcome.actions,
                );
            }
        }
    }

    let decoded = decode_irc_line(&final_bytes);
    let line = decoded.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return;
    }
    emit(
        app,
        UiEvent::Raw {
            server_id: server_id.to_string(),
            direction: Direction::In,
            line: line.to_string(),
        },
    );
    let Ok(msg) = line.parse::<Message>() else {
        tracing::debug!("unparsed line: {line:?}");
        return;
    };
    let mut ctx = Context {
        server_id,
        profile,
        state,
        names_accum,
        whois_accum,
        auth,
    };
    let mut effects = process_message(&mut ctx, line, msg);
    drop(ctx);

    if let Some((nick, message)) = &effects.dcc_message {
        let consumed = app
            .try_state::<crate::irc::dcc::DccManager>()
            .is_some_and(|dcc| dcc.handle_protocol(app, server_id, nick, message));
        if consumed {
            effects.events.retain(|event| {
                !matches!(
                    event,
                    UiEvent::DccChatOffer { .. } | UiEvent::DccFileOffer { .. }
                ) && !matches!(event, UiEvent::Echo { text, .. } if text.starts_with("[DCC]"))
            });
        }
    }

    if effects.events.iter().any(is_state_event)
        || effects.bans_changed
        || effects.channel_state_changed
        || effects.ial_changed
    {
        if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
            store.set(server_id, state.snapshot());
        }
    }

    let (mut script_actions, suppressed_events) = run_scripts(
        app,
        state,
        effects.script_state.as_ref(),
        profile,
        &effects.events,
        &effects.script_events,
        Some(line),
        Some(&final_bytes),
    );
    let mut autojoin_skip = false;
    let mut autojoin_delay = 0u32;
    script_actions.retain(|action| {
        if let crate::script::eval::Action::Autojoin { skip, delay_secs } = action {
            autojoin_skip |= *skip;
            if *delay_secs > 0 {
                autojoin_delay = *delay_secs;
            }
            false
        } else {
            true
        }
    });
    if !effects.perform.is_empty() {
        if let Some(engine) = app.try_state::<crate::script::ScriptEngine>() {
            let snapshot = state.snapshot();
            let run_ctx = crate::script::RunCtx {
                my_nick: &state.nick,
                network: &profile.name,
                server: &profile.host,
                data_dir: crate::script::script_data_dir(app),
                state: std::sync::Arc::new(snapshot),
            };
            script_actions.extend(run_perform_commands(&engine, &run_ctx, &effects.perform));
        }
    }

    for out in effects.outgoing {
        write_parsed_line(
            w,
            app,
            server_id,
            profile,
            state,
            out.as_bytes(),
            true,
            true,
            true,
        )
        .await;
    }
    for (index, event) in effects.events.into_iter().enumerate() {
        if !parse_em && !suppressed_events.get(index).copied().unwrap_or(false) {
            emit(app, event);
        }
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

    if !effects.autojoin.is_empty() && !autojoin_skip {
        if autojoin_delay > 0 {
            let app2 = app.clone();
            let sid = server_id.to_string();
            let channels = effects.autojoin;
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(autojoin_delay as u64)).await;
                if let Some(manager) = app2.try_state::<crate::irc::ConnectionManager>() {
                    for channel in channels {
                        let _ = manager.send(&sid, format!("JOIN {channel}"));
                    }
                }
            });
        } else {
            if let Some(manager) = app.try_state::<crate::irc::ConnectionManager>() {
                for channel in effects.autojoin {
                    let _ = manager.send(server_id, format!("JOIN {channel}"));
                }
            }
        }
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
    let mut state = SessionState {
        nick: profile.nick.clone(),
        server_id: server_id.to_string(),
        server_port: profile.port,
        tls: profile.tls,
        alt_nick: profile.alt_nick.clone().unwrap_or_default(),
        main_nick: profile.nick.clone(),
        realname: profile.realname.clone().unwrap_or_default(),
        ..Default::default()
    };
    // The status connection exists before registration lines are sent, so
    // PARSELINE handlers on CAP/NICK/USER already see this connection's cid.
    if let Some(engine) = app.try_state::<crate::script::ScriptEngine>() {
        engine.assign_cid(server_id);
        engine.set_connection_context(server_id, &profile.name, &profile.host);
    }

    // Registration. IRCX AUTH packages run before NICK/USER; standard servers
    // use CAP/SASL instead.
    let ircx_anon = profile.ircx
        && profile
            .ircx_auth_package
            .as_deref()
            .is_some_and(|package| package.eq_ignore_ascii_case("ANON"));
    if profile.ntlm || ircx_anon {
        let auth_result = if profile.ntlm {
            crate::irc::ntlm::handshake(&mut reader, &mut write_half, profile, app, server_id).await
        } else {
            crate::irc::ntlm::anonymous_handshake(
                &mut reader,
                &mut write_half,
                profile,
                app,
                server_id,
            )
            .await
        };
        if let Err(e) = auth_result {
            let package = if profile.ntlm { "NTLM" } else { "ANON" };
            emit(
                app,
                UiEvent::Error {
                    server_id: server_id.to_string(),
                    message: format!("{package} authentication failed: {e}"),
                },
            );
            emit(
                app,
                UiEvent::Disconnected {
                    server_id: server_id.to_string(),
                    reason: format!("{package} auth failed: {e}"),
                },
            );
            return Outcome::Dropped;
        }
        write_parsed_line(
            &mut write_half,
            app,
            server_id,
            profile,
            &state,
            format!("NICK {}", profile.nick).as_bytes(),
            true,
            true,
            true,
        )
        .await;
        write_parsed_line(
            &mut write_half,
            app,
            server_id,
            profile,
            &state,
            format!("USER {} 0 * :{}", profile.username(), profile.realname()).as_bytes(),
            true,
            true,
            true,
        )
        .await;
    } else {
        // Begin CAP negotiation before NICK/USER so SASL can run.
        write_parsed_line(
            &mut write_half,
            app,
            server_id,
            profile,
            &state,
            b"CAP LS 302",
            true,
            true,
            true,
        )
        .await;
        if let Some(pw) = profile.password.as_deref().filter(|p| !p.is_empty()) {
            write_parsed_line(
                &mut write_half,
                app,
                server_id,
                profile,
                &state,
                format!("PASS {pw}").as_bytes(),
                true,
                true,
                true,
            )
            .await;
        }
        write_parsed_line(
            &mut write_half,
            app,
            server_id,
            profile,
            &state,
            format!("NICK {}", profile.nick).as_bytes(),
            true,
            true,
            true,
        )
        .await;
        write_parsed_line(
            &mut write_half,
            app,
            server_id,
            profile,
            &state,
            format!("USER {} 0 * :{}", profile.username(), profile.realname()).as_bytes(),
            true,
            true,
            true,
        )
        .await;
    }

    let mut names_accum: HashMap<String, Vec<String>> = HashMap::new();
    let mut whois_accum: HashMap<String, Vec<String>> = HashMap::new();
    let mut auth = AuthState::default();
    let mut flood = FloodLimiter::default();

    let reason = loop {
        tokio::select! {
            read = reader.read_until(b'\n', &mut buf) => match read {
                Ok(0) => break ("connection closed by server".to_string(), Outcome::Dropped),
                Ok(_) => {
                    // Partial line (EOF mid-line) — keep buffering; processed on next Ok(0).
                    if buf.last() != Some(&b'\n') {
                        continue;
                    }
                    let mut line_bytes = std::mem::take(&mut buf);
                    handle_incoming_line(
                        &mut write_half,
                        app,
                        server_id,
                        profile,
                        &mut state,
                        &mut names_accum,
                        &mut whois_accum,
                        &mut auth,
                        &line_bytes,
                        true,
                        true,
                        true,
                    )
                    .await;
                    line_bytes.clear();
                    buf = line_bytes;
                }
                Err(e) => break (format!("read error: {e}"), Outcome::Dropped),
            },
            cmd = outgoing_rx.recv() => match cmd {
                Some(line) => {
                    if let Some(crate::script::eval::Action::ParseLine {
                        direction,
                        bytes,
                        trigger,
                        append_crlf,
                        utf8,
                        ..
                    }) = crate::script::decode_parseline_control(&line)
                    {
                        if direction == "in" {
                            handle_incoming_line(
                                &mut write_half,
                                app,
                                server_id,
                                profile,
                                &mut state,
                                &mut names_accum,
                                &mut whois_accum,
                                &mut auth,
                                &bytes,
                                trigger,
                                utf8,
                                false,
                            )
                            .await;
                        } else {
                            write_parsed_line(
                                &mut write_half,
                                app,
                                server_id,
                                profile,
                                &state,
                                &bytes,
                                trigger,
                                utf8,
                                append_crlf,
                            )
                            .await;
                        }
                    } else if let Some(rest) = line.strip_prefix("\u{0}SETID ") {
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
                    } else if let Some(rest) = line.strip_prefix("\u{0}IAL ") {
                        apply_ial_control(&mut state, rest);
                        if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
                            store.set(server_id, state.snapshot());
                        }
                    } else {
                        let config = app
                            .try_state::<super::manager::ConnectionManager>()
                            .map(|manager| manager.flood_config())
                            .unwrap_or_default();
                        let delay = flood.delay(&config, Instant::now());
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
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
                        write_parsed_line(
                            &mut write_half,
                            app,
                            server_id,
                            profile,
                            &state,
                            line.as_bytes(),
                            true,
                            true,
                            true,
                        )
                        .await;
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
    let (actions, suppressed) = run_scripts(
        app,
        &state,
        None,
        profile,
        std::slice::from_ref(&disc),
        &[],
        None,
        None,
    );
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
    if !suppressed.first().copied().unwrap_or(false) {
        emit(app, disc);
    }
    if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
        store.remove(server_id);
    }
    if let Some(timers) = app.try_state::<crate::script::timer::TimerManager>() {
        timers.session_dropped(app, server_id);
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
    let line = strip_message_tags(line);
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
        crate::script::apply_actions(
            app,
            server_id,
            &state.nick,
            &profile.name,
            &profile.host,
            actions,
        );
    }
}

/// Runs script event handlers for the events produced by one inbound message.
fn run_scripts(
    app: &AppHandle,
    state: &SessionState,
    script_state: Option<&StateSnapshot>,
    profile: &ServerProfile,
    events: &[UiEvent],
    // Extra events for scripts only (CTCP requests/replies); see `Effects`.
    script_events: &[UiEvent],
    raw_line: Option<&str>,
    raw_bytes: Option<&[u8]>,
) -> (Vec<crate::script::eval::Action>, Vec<bool>) {
    let Some(engine) = app.try_state::<crate::script::ScriptEngine>() else {
        return (Vec::new(), vec![false; events.len()]);
    };
    let state_snapshot = script_state.cloned().unwrap_or_else(|| state.snapshot());
    let ctx = crate::script::RunCtx {
        my_nick: &state.nick,
        network: &profile.name,
        server: &profile.host,
        data_dir: crate::script::script_data_dir(app),
        state: std::sync::Arc::new(state_snapshot),
    };
    let mut actions = Vec::new();
    let raw_context = raw_line.map(|line| {
        crate::script::raw_event_context(line, raw_bytes.unwrap_or_else(|| line.as_bytes()))
    });
    let mut raw_halted = false;
    // `on RAW` fires for every inbound server line; named protocol events
    // (`on WALLOPS`/`ERROR`/`PING`/`PONG`) fire off the same parsed command.
    if let Some(line) = raw_line {
        if let Some((command, params)) = raw_command_params(line) {
            if let Some(kind) = named_event_kind(&command) {
                let text = params.last().cloned().unwrap_or_default();
                actions.extend(crate::script::dispatch_named_with_context(
                    &engine,
                    &ctx,
                    kind,
                    &source_nick(line),
                    &text,
                    raw_context.as_ref(),
                ));
            }
            let (more, halted) = crate::script::dispatch_raw_with_context(
                &engine,
                &ctx,
                &command,
                params,
                raw_context.as_ref(),
            );
            actions.extend(more);
            raw_halted |= halted;
        }
    }
    let mut suppressed = Vec::with_capacity(events.len());
    for ev in events {
        let (more, halted) =
            crate::script::drive_event_halt_raw(&engine, &ctx, ev, raw_context.as_ref());
        actions.extend(more);
        suppressed.push(halted || raw_halted);
    }
    for ev in script_events {
        actions
            .extend(crate::script::drive_event_halt_raw(&engine, &ctx, ev, raw_context.as_ref()).0);
    }
    (actions, suppressed)
}

fn run_perform_commands(
    engine: &crate::script::ScriptEngine,
    ctx: &crate::script::RunCtx<'_>,
    commands: &[String],
) -> Vec<crate::script::eval::Action> {
    commands
        .iter()
        .flat_map(|command| engine.run_command(ctx, "", command, &[]))
        .collect()
}

/// Splits a raw IRC line into its command/numeric and parameters. An optional
/// `:prefix` is dropped, middle params split on spaces, and a trailing `:param`
/// keeps its spaces.
fn raw_command_params(line: &str) -> Option<(String, Vec<String>)> {
    let line = strip_message_tags(line);
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

fn strip_message_tags(line: &str) -> &str {
    line.strip_prefix('@')
        .and_then(|rest| rest.split_once(' '))
        .map(|(_, line)| line)
        .unwrap_or(line)
}

/// Applies a client-local IAL mutation routed through the connection's outgoing
/// queue. These lines are never written to the IRC server.
fn apply_ial_control(state: &mut SessionState, control: &str) {
    match control.trim() {
        "ON" => state.set_ial_enabled(true),
        "OFF" => state.set_ial_enabled(false),
        "CLEAR" => state.clear_ial(None),
        _ => {
            if let Some(nick) = control.strip_prefix("CLEAR ") {
                state.clear_ial(Some(nick.trim()));
            } else if let Some(fields) = control.strip_prefix("MARK\t") {
                let mut fields = fields.splitn(5, '\t');
                let remove = fields.next() == Some("1");
                let wildcard = fields.next() == Some("1");
                let nick = fields.next().unwrap_or("");
                let name = fields.next().unwrap_or("default");
                let text = fields.next().unwrap_or("");
                state.update_ial_mark(nick, name, text, remove, wildcard);
            }
        }
    }
}

/// Normalises a PRIVMSG/NOTICE target for UI and script routing. STATUSMSG
/// prefixes address a subset of channel members but still belong in the bare
/// channel buffer; known channels retain their original display spelling.
fn routed_message_target(state: &SessionState, target: &str) -> String {
    let Some(channel) = state.isupport.channel_target(target) else {
        return target.to_string();
    };
    state.channel_name(channel).unwrap_or(channel).to_string()
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
            fx.ial_changed |= ctx
                .state
                .record_address(nick, format!("{nick}!{user}@{host}"));
        }
    }
    // IRCv3 server-time (@time tag), used as the line timestamp when present.
    let server_time = msg.tags.as_ref().and_then(|tags| {
        tags.iter()
            .find(|t| t.0 == "time")
            .and_then(|t| t.1.clone())
    });
    // IRCv3 account-tag applies to every command from a user, including direct
    // messages from users with whom we share no channel. Keep the IAL account
    // metadata current before routing the command or firing scripts.
    if let Some(nick) = source.as_deref() {
        if let Some(account) = msg.tags.as_ref().and_then(|tags| {
            tags.iter()
                .find(|tag| tag.0 == "account")
                .and_then(|tag| tag.1.as_deref())
        }) {
            ctx.state.update_ial_account(nick, account);
            fx.ial_changed = true;
        }
    }

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
            // In a server reply `CAP <nick> LS * :caps`, irc-proto stores the
            // continuation marker in `a` and the capability list in `b`.
            let continuation =
                matches!(sub, CapSubCommand::LS) && a.as_deref() == Some("*") && b.is_some();
            fx.outgoing.extend(auth::on_cap(
                ctx.profile,
                ctx.auth,
                sub,
                &caps,
                continuation,
            ));
        }
        Command::AUTHENTICATE(ref data) => {
            fx.outgoing
                .extend(auth::on_authenticate(ctx.profile, ctx.auth, data));
        }
        Command::PRIVMSG(ref target, ref text) => {
            let routed_target = routed_message_target(ctx.state, target);
            if let Some(nick) = source.as_deref() {
                if ctx.state.isupport.channel_target(&routed_target).is_some() {
                    ctx.state.touch_member(&routed_target, nick);
                    fx.channel_state_changed = true;
                }
            }
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
                    target: routed_target.clone(),
                    text: body.to_string(),
                    time: server_time,
                });
                return fx;
            }
            // CTCP requests (\x01CMD args\x01), excluding ACTION, get auto-replies.
            if let Some(ctcp) = text
                .strip_prefix('\u{1}')
                .map(|s| s.trim_end_matches('\u{1}'))
            {
                let (cmd, rest) = ctcp.split_once(' ').unwrap_or((ctcp, ""));
                // A DCC offer (CHAT/SEND) — surface it to the user. (Connecting to
                // accept it is a later phase; for now incoming offers are visible.)
                if cmd.eq_ignore_ascii_case("DCC") {
                    let who = source.as_deref().unwrap_or("?").to_string();
                    let parsed = crate::irc::dcc::parse_dcc_message(ctcp);
                    if let Some(message) = parsed.clone() {
                        fx.dcc_message = Some((who.clone(), message));
                    }
                    match parsed {
                        // A CHAT offer is acceptable — surface it structurally so the
                        // UI can connect (`/dcc get <nick>`).
                        Some(crate::irc::dcc::DccMessage::Offer(o))
                            if o.kind == crate::irc::dcc::DccKind::Chat =>
                        {
                            fx.events.push(UiEvent::DccChatOffer {
                                server_id: server_id.clone(),
                                nick: who.clone(),
                                ip: o.ip.to_string(),
                                port: o.port,
                                token: o.token,
                            });
                            fx.events.push(UiEvent::Echo {
                                server_id,
                                target: "(status)".to_string(),
                                text: format!(
                                    "[DCC] {who} offers a DCC CHAT — /dcc get {who} to accept"
                                ),
                            });
                        }
                        Some(crate::irc::dcc::DccMessage::Offer(o)) => {
                            fx.events.push(UiEvent::DccFileOffer {
                                server_id: server_id.clone(),
                                nick: who.clone(),
                                filename: o.filename.clone(),
                                ip: o.ip.to_string(),
                                port: o.port,
                                size: o.size,
                                token: o.token,
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
                        Some(crate::irc::dcc::DccMessage::Resume { .. })
                        | Some(crate::irc::dcc::DccMessage::Accept { .. }) => {}
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
                    if ctx.state.isupport.names_equal(target, &ctx.state.nick) {
                        if let (Some(nick), Some(reply)) = (source.as_ref(), ctcp_reply(cmd, rest))
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
                        target: routed_target.clone(),
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
                target: routed_target,
                text: text.clone(),
                time: server_time,
            });
        }
        Command::NOTICE(ref target, ref text) => {
            let routed_target = routed_message_target(ctx.state, target);
            if let Some(nick) = source.as_deref() {
                if ctx.state.isupport.channel_target(&routed_target).is_some() {
                    ctx.state.touch_member(&routed_target, nick);
                    fx.channel_state_changed = true;
                }
            }
            // A CTCP reply (\x01...\x01) — render it readably, and surface it to
            // scripts as a Message so `on CTCPREPLY` fires.
            if let Some(ctcp) = text
                .strip_prefix('\u{1}')
                .map(|s| s.trim_end_matches('\u{1}'))
            {
                fx.script_events.push(UiEvent::Message {
                    server_id: server_id.clone(),
                    kind: MessageKind::Notice,
                    from: source.clone(),
                    target: routed_target.clone(),
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
                target: routed_target,
                text: text.clone(),
                time: server_time,
            });
        }
        Command::JOIN(ref channel, ref account, ref realname) => {
            if let Some(nick) = &source {
                ctx.state.upsert_member(channel, nick, String::new());
                let display_channel = ctx
                    .state
                    .channel_name(channel)
                    .unwrap_or(channel)
                    .to_string();
                if let Some(account) = account {
                    ctx.state.update_ial_account(nick, account);
                }
                if let Some(realname) = realname {
                    ctx.state.update_ial_gecos(nick, realname);
                }
                fx.ial_changed = true;
                fx.events.push(UiEvent::Join {
                    server_id,
                    channel: display_channel,
                    nick: nick.clone(),
                });
            }
        }
        Command::PART(ref channel, ref reason) => {
            if let Some(nick) = &source {
                fx.script_state = Some(ctx.state.snapshot());
                let display_channel = ctx
                    .state
                    .channel_name(channel)
                    .unwrap_or(channel)
                    .to_string();
                ctx.state.remove_member(channel, nick);
                if ctx.state.isupport.names_equal(nick, &ctx.state.nick) {
                    ctx.state.remove_channel(channel);
                    ctx.state.prune_ial();
                } else {
                    ctx.state.prune_ial_nick(nick);
                }
                fx.ial_changed = true;
                fx.events.push(UiEvent::Part {
                    server_id,
                    channel: display_channel,
                    nick: nick.clone(),
                    reason: reason.clone(),
                });
            }
        }
        Command::QUIT(ref reason) => {
            if let Some(nick) = &source {
                fx.script_state = Some(ctx.state.snapshot());
                let channels = ctx.state.remove_member_everywhere(nick);
                ctx.state.clear_ial(Some(nick));
                fx.ial_changed = true;
                fx.events.push(UiEvent::Quit {
                    server_id,
                    nick: nick.clone(),
                    reason: reason.clone(),
                    channels,
                });
            }
        }
        Command::KICK(ref channel, ref kicked, ref comment) => {
            fx.script_state = Some(ctx.state.snapshot());
            let display_channel = ctx
                .state
                .channel_name(channel)
                .unwrap_or(channel)
                .to_string();
            let is_self = ctx.state.isupport.names_equal(kicked, &ctx.state.nick);
            ctx.state.remove_member(channel, kicked);
            if is_self {
                ctx.state.remove_channel(channel);
                ctx.state.prune_ial();
            } else {
                ctx.state.prune_ial_nick(kicked);
            }
            fx.ial_changed = true;
            fx.events.push(UiEvent::Kick {
                server_id,
                channel: display_channel,
                nick: kicked.clone(),
                by: source,
                reason: comment.clone(),
                is_self,
            });
        }
        Command::AWAY(ref message) => {
            // away-notify: another user's away state changed.
            if let Some(nick) = &source {
                ctx.state.update_ial_away(nick, message.is_some());
                fx.ial_changed = true;
                let channels: Vec<String> = ctx
                    .state
                    .channels
                    .iter()
                    .filter(|(name, _)| ctx.state.has_member(name, nick))
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
                ctx.state.rename_ial(old, new_nick);
                fx.ial_changed = true;
                if ctx.state.isupport.names_equal(old, &ctx.state.nick) {
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
            if let Some(ch) = ctx.state.channel_mut(channel) {
                ch.topic = topic.clone();
            }
            let display_channel = ctx
                .state
                .channel_name(channel)
                .unwrap_or(channel)
                .to_string();
            fx.events.push(UiEvent::Topic {
                server_id,
                channel: display_channel,
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
                ctx.state.update_ial_account(nick, account);
                fx.ial_changed = true;
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
                ctx.state.update_ial_chghost(nick, user, host);
                fx.ial_changed = true;
                let text = format!("{nick} is now {user}@{host}");
                push_channel_notice(&mut fx, ctx, &server_id, nick, &text);
            }
        }
        Command::WALLOPS(ref text) => {
            fx.events.push(UiEvent::Echo {
                server_id,
                target: "(status)".to_string(),
                text: format!(
                    "[WALLOPS{}] {text}",
                    source.map(|s| format!(" from {s}")).unwrap_or_default()
                ),
            });
        }
        Command::ERROR(ref message) => fx.events.push(UiEvent::Error {
            server_id,
            message: message.clone(),
        }),
        Command::Response(resp, ref args) => handle_numeric(ctx, &mut fx, resp, args),
        Command::Raw(ref cmd, ref args) => {
            // IRCv3 batch delimiters are structural. The contained messages are
            // still routed normally and retain their @batch tag.
            if cmd.eq_ignore_ascii_case("BATCH") {
                return fx;
            }
            // mIRC's `/ialfill` WHOX request uses token 995 and fields
            // %acdfhlnrstu. WHOX replies always use canonical field order:
            // token, channel, user, host, server, nick, flags, hops, idle,
            // account, realname (after our own nick at args[0]).
            if cmd == "354" && args.get(1).map(String::as_str) == Some("995") {
                if let (Some(user), Some(host), Some(nick), Some(flags), Some(account)) = (
                    args.get(3),
                    args.get(4),
                    args.get(6),
                    args.get(7),
                    args.get(10),
                ) {
                    ctx.state.update_ial_whox(
                        nick,
                        user,
                        host,
                        account,
                        flags.contains('G'),
                        args.get(11).map(String::as_str).unwrap_or(""),
                    );
                    fx.ial_changed = true;
                }
                return fx;
            }
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

    if let Some(before) = fx.script_state.take() {
        fx.script_state = Some(before.with_pending_nicklist_update(ctx.state.snapshot()));
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
    let depth = (if alt.is_some() {
        attempt.saturating_sub(1)
    } else {
        attempt
    })
    .max(1);
    if depth <= 4 {
        format!("{base}{}", "_".repeat(depth as usize))
    } else {
        format!("{base}{attempt}")
    }
}

/// Emits an info line (via Echo) into every channel we share with `nick`.
fn push_channel_notice(fx: &mut Effects, ctx: &Context, server_id: &str, nick: &str, text: &str) {
    for (name, ch) in ctx.state.channels.iter() {
        if ch
            .members
            .keys()
            .any(|known| ctx.state.isupport.names_equal(known, nick))
        {
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
    let mut args = params
        .get(i + 3..)
        .map(|s| s.to_vec())
        .unwrap_or_default()
        .into_iter();

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

    // Native IRCX key provisioning belongs only to connections that explicitly
    // enabled IRCX. Script-managed `/server` bridges must see the MODE normally
    // without jIRC competing with the script's own owner/key management.
    let owned_before = member_has_prefix_mode(ctx.state, &target, &ctx.state.nick, 'q');
    let mut tokens: Vec<String> = Vec::new();
    let mut adding = true;
    let mut prefix_changed = false;
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
                    }
                } else if ctx.state.isupport.chanmodes_a.contains(letter) {
                    if letter == 'b' {
                        if let Some(mask) = &arg {
                            ctx.state.set_ban(&target, mask, adding);
                        }
                    }
                } else {
                    ctx.state
                        .set_channel_mode(&target, letter, arg.as_deref(), adding);
                }
                let sign = if adding { '+' } else { '-' };
                match &arg {
                    Some(a) => tokens.push(format!("{sign}{letter} {a}")),
                    None => tokens.push(format!("{sign}{letter}")),
                }
            }
        }
    }

    let owned_after = member_has_prefix_mode(ctx.state, &target, &ctx.state.nick, 'q');
    let got_owner = ctx.profile.ircx && !owned_before && owned_after;
    let lost_owner = ctx.profile.ircx && owned_before && !owned_after;

    // Someone else stripped our +q — capture the offender for takeover protection.
    let revoked_by = if lost_owner {
        by.clone()
            .filter(|b| !ctx.state.isupport.names_equal(b, &ctx.state.nick))
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
        if let Some(ch) = ctx.state.channel(&target) {
            let channel = ctx
                .state
                .channel_name(&target)
                .unwrap_or(&target)
                .to_string();
            fx.events.push(UiEvent::Names {
                server_id: server_id.to_string(),
                channel,
                members: ch.member_list(),
            });
        }
    }
}

fn member_has_prefix_mode(state: &SessionState, channel: &str, nick: &str, mode: char) -> bool {
    let Some(prefix) = state.isupport.prefix_for_mode(mode) else {
        return false;
    };
    state.channel(channel).is_some_and(|channel| {
        channel.members.iter().any(|(known, prefixes)| {
            state.isupport.names_equal(known, nick) && prefixes.contains(prefix)
        })
    })
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
                        if ctx
                            .state
                            .isupport
                            .names_equal(who.trim_end_matches('*'), &ctx.state.nick)
                        {
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
        Response::RPL_CHANNELMODEIS => {
            // 324: <me> <channel> <modes> [mode arguments]. Seed the channel
            // mode cache used by `$chan().mode/.key/.limit`; later MODE lines
            // update it incrementally through `handle_mode`.
            if let (Some(channel), Some(modestring)) = (args.get(1), args.get(2)) {
                fx.channel_state_changed = true;
                let mut values = args.iter().skip(3);
                let mut adding = true;
                for letter in modestring.chars() {
                    match letter {
                        '+' => adding = true,
                        '-' => adding = false,
                        _ => {
                            let value = if ctx.state.isupport.mode_takes_arg(letter, adding) {
                                values.next().map(String::as_str)
                            } else {
                                None
                            };
                            if ctx.state.isupport.prefix_for_mode(letter).is_none()
                                && !ctx.state.isupport.chanmodes_a.contains(letter)
                            {
                                ctx.state.set_channel_mode(channel, letter, value, adding);
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
            fx.perform = ctx.profile.perform.clone();
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
        Response::RPL_WHOREPLY => {
            // 352: <me> <channel> <user> <host> <server> <nick> <flags>
            //      :<hopcount> <realname>. This is the fallback used when WHOX
            // isn't advertised; account and idle fields are unavailable.
            if let (Some(user), Some(host), Some(nick), Some(flags)) =
                (args.get(2), args.get(3), args.get(5), args.get(6))
            {
                let gecos = args
                    .get(7)
                    .and_then(|trailing| trailing.split_once(' ').map(|(_, real)| real))
                    .unwrap_or("");
                let account = ctx
                    .state
                    .ial_info
                    .get(&ctx.state.isupport.casefold(nick))
                    .map(|info| info.account.clone())
                    .unwrap_or_default();
                ctx.state
                    .update_ial_whox(nick, user, host, &account, flags.contains('G'), gecos);
                fx.ial_changed = true;
            }
        }
        Response::RPL_TOPIC => {
            // [nick, channel, topic]
            if let (Some(channel), Some(topic)) = (args.get(1), args.get(2)) {
                if let Some(ch) = ctx.state.channel_mut(channel) {
                    ch.topic = Some(topic.clone());
                }
                let display_channel = ctx
                    .state
                    .channel_name(channel)
                    .unwrap_or(channel)
                    .to_string();
                fx.events.push(UiEvent::Topic {
                    server_id,
                    channel: display_channel,
                    topic: Some(topic.clone()),
                    set_by: None,
                });
            }
        }
        Response::RPL_NAMREPLY => {
            // [nick, symbol, channel, "space separated names"]
            if let (Some(channel), Some(names)) = (args.get(2), args.last()) {
                // IRC channel names are case-insensitive. Keep the spelling from
                // the first 353 while appending subsequent replies whose server
                // happens to vary the channel's case.
                let accum_channel = ctx
                    .names_accum
                    .keys()
                    .find(|name| ctx.state.isupport.names_equal(name, channel))
                    .cloned()
                    .unwrap_or_else(|| channel.clone());
                let entry = ctx.names_accum.entry(accum_channel).or_default();
                for name in names.split_whitespace() {
                    entry.push(name.to_string());
                }
            }
        }
        Response::RPL_ENDOFNAMES => {
            // [nick, channel, "End of /NAMES list"]
            if let Some(channel) = args.get(1) {
                let accum_channel = ctx
                    .names_accum
                    .keys()
                    .find(|name| ctx.state.isupport.names_equal(name, channel))
                    .cloned();
                if let Some((accum_channel, names)) =
                    accum_channel.and_then(|name| ctx.names_accum.remove_entry(&name))
                {
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
                    // Prefer the channel spelling already established by JOIN.
                    // If there is no channel state yet, retain the spelling from
                    // the first 353 rather than changing it to the 366 spelling.
                    let display_channel = ctx
                        .state
                        .channels
                        .keys()
                        .find(|name| ctx.state.isupport.names_equal(name, channel))
                        .cloned()
                        .unwrap_or(accum_channel);
                    ctx.state
                        .channels
                        .entry(display_channel.clone())
                        .or_default()
                        .members
                        .clear();
                    for (nick, prefixes) in members {
                        ctx.state.upsert_member(&display_channel, &nick, prefixes);
                    }
                    ctx.state.prune_member_activity(&display_channel);
                    let member_list = ctx
                        .state
                        .channel(&display_channel)
                        .map(|ch| ch.member_list())
                        .unwrap_or_default();
                    fx.events.push(UiEvent::Names {
                        server_id,
                        channel: display_channel,
                        members: member_list,
                    });
                }
            }
        }
        Response::RPL_ISUPPORT => {
            // [nick, TOKEN=val, TOKEN=val, ..., ":are supported by this server"]
            let old_mapping = ctx.state.isupport.case_mapping;
            for token in args.iter().skip(1) {
                ctx.state.isupport.parse_token(token);
            }
            if ctx.state.isupport.case_mapping != old_mapping {
                ctx.state.reindex_ial();
                fx.ial_changed = true;
            }
            fx.events.push(UiEvent::Isupport {
                server_id,
                chan_types: ctx.state.isupport.chan_types.clone(),
                prefixes: ctx.state.isupport.prefix_chars(),
                prefix_modes: ctx
                    .state
                    .isupport
                    .prefix_modes
                    .iter()
                    .map(|(mode, _)| *mode)
                    .collect(),
                case_mapping: ctx.state.isupport.case_mapping.as_str().to_string(),
                status_msg: ctx.state.isupport.status_msg.clone(),
                chan_modes: format!(
                    "{},{},{},{}",
                    ctx.state.isupport.chanmodes_a,
                    ctx.state.isupport.chanmodes_b,
                    ctx.state.isupport.chanmodes_c,
                    ctx.state.isupport.chanmodes_d
                ),
                modes_per_line: ctx.state.isupport.modes,
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
            fx.events.push(UiEvent::SelfAway {
                server_id,
                away: true,
            });
        }
        Response::RPL_UNAWAY => {
            ctx.state.away = false;
            ctx.state.away_time = 0;
            fx.events.push(UiEvent::SelfAway {
                server_id,
                away: false,
            });
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
    let rest = |from: usize| {
        args.iter()
            .skip(from)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    };
    match code {
        311 => format!(
            "{} ({}@{}): {}",
            args.get(1).cloned().unwrap_or_default(),
            args.get(2).cloned().unwrap_or_default(),
            args.get(3).cloned().unwrap_or_default(),
            args.get(5).cloned().unwrap_or_default()
        ),
        312 => format!(
            "server: {} ({})",
            args.get(2).cloned().unwrap_or_default(),
            args.get(3).cloned().unwrap_or_default()
        ),
        313 => "is an IRC operator".to_string(),
        317 => format!(
            "idle: {}s, signon: {}",
            args.get(2).cloned().unwrap_or_default(),
            args.get(3).cloned().unwrap_or_default()
        ),
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
    fn outbound_flood_limiter_allows_burst_then_spaces_lines() {
        let config = super::super::manager::FloodConfig {
            enabled: true,
            messages: 2,
            seconds: 1,
        };
        let start = Instant::now();
        let mut limiter = FloodLimiter::default();
        assert_eq!(limiter.delay(&config, start), Duration::ZERO);
        assert_eq!(limiter.delay(&config, start), Duration::ZERO);
        assert_eq!(limiter.delay(&config, start), Duration::from_secs(1));
        assert_eq!(
            limiter.delay(&config, start + Duration::from_millis(500)),
            Duration::from_millis(500)
        );
        let disabled = super::super::manager::FloodConfig {
            enabled: false,
            ..config
        };
        assert_eq!(limiter.delay(&disabled, start), Duration::ZERO);
    }

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

    #[test]
    fn echo_message_cap_marks_only_our_echoed_message_lines() {
        let profile = profile();
        let state = SessionState {
            nick: "Me".into(),
            ..Default::default()
        };
        let mut auth = AuthState::default();
        assert!(!is_echoed_message(
            ":Me!u@h PRIVMSG #c :hello",
            &state,
            &auth
        ));
        assert_eq!(
            auth::on_cap(
                &profile,
                &mut auth,
                &CapSubCommand::LS,
                "echo-message",
                false
            ),
            vec!["CAP REQ :echo-message"]
        );
        assert_eq!(
            auth::on_cap(
                &profile,
                &mut auth,
                &CapSubCommand::ACK,
                "echo-message",
                false
            ),
            vec!["CAP END"]
        );
        assert!(is_echoed_message(
            "@time=2026-07-15T00:00:00Z :me!u@h PRIVMSG #c :hello",
            &state,
            &auth
        ));
        assert!(is_echoed_message(
            ":ME!u@h NOTICE bob :hello",
            &state,
            &auth
        ));
        assert!(is_echoed_message(
            "@label=x :me!u@h TAGMSG #c",
            &state,
            &auth
        ));
        assert!(!is_echoed_message(
            ":bob!u@h PRIVMSG #c :hello",
            &state,
            &auth
        ));
    }

    fn profile() -> ServerProfile {
        ServerProfile {
            id: Some("s1".into()),
            name: "test".into(),
            host: "localhost".into(),
            port: 6667,
            tls: false,
            tls_insecure: false,
            tls_client_cert_path: None,
            tls_client_key_path: None,
            ircx: false,
            sasl: false,
            sasl_mechanism: crate::config::SaslMechanism::Plain,
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
            ircx_auth_package: None,
            ntlm_domain: None,
            ntlm_user: None,
            ntlm_password: None,
            autojoin: vec![],
            perform: vec![],
        }
    }

    #[test]
    fn perform_commands_run_in_order_with_connection_context() {
        let engine = crate::script::ScriptEngine::new();
        let state = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let data_dir = std::env::temp_dir().join("jirc-perform-test");
        let ctx = crate::script::RunCtx {
            my_nick: "me",
            network: "TestNet",
            server: "irc.example.test",
            data_dir,
            state: std::sync::Arc::new(state.snapshot()),
        };
        let actions = run_perform_commands(
            &engine,
            &ctx,
            &[
                "mode $me +i".into(),
                "msg NickServ network=$network".into(),
            ],
        );
        assert_eq!(
            actions,
            vec![
                crate::script::eval::Action::Send("MODE me +i".into()),
                crate::script::eval::Action::Send(
                    "PRIVMSG NickServ :network=TestNet".into()
                ),
            ]
        );
    }

    fn run_line(
        state: &mut SessionState,
        accum: &mut HashMap<String, Vec<String>>,
        line: &str,
    ) -> Effects {
        let p = profile();
        run_line_with_profile(state, accum, line, &p)
    }

    fn run_line_with_profile(
        state: &mut SessionState,
        accum: &mut HashMap<String, Vec<String>>,
        line: &str,
        profile: &ServerProfile,
    ) -> Effects {
        let mut auth = AuthState::default();
        let mut whois = HashMap::new();
        let mut ctx = Context {
            server_id: "s1",
            profile,
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
    fn tracks_channel_mode_numeric_and_live_changes() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        run_line(&mut s, &mut accum, ":me!u@h JOIN #Room[");

        let fx = run_line(&mut s, &mut accum, ":srv 324 me #room{ +ntkl secret 25");
        assert!(fx.channel_state_changed);
        let channel = s.channel("#ROOM{").unwrap();
        assert_eq!(channel.mode_string(), "+klnt secret 25");
        assert_eq!(channel.modes.get(&'k').map(String::as_str), Some("secret"));
        assert_eq!(channel.modes.get(&'l').map(String::as_str), Some("25"));

        run_line(&mut s, &mut accum, ":op!u@h MODE #ROOM{ -k+l secret 40");
        let channel = s.channel("#room[").unwrap();
        assert!(!channel.modes.contains_key(&'k'));
        assert_eq!(channel.modes.get(&'l').map(String::as_str), Some("40"));
    }

    #[test]
    fn departure_effect_keeps_old_script_roster_until_updatenl() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.upsert_member("#c", "me", String::new());
        s.upsert_member("#c", "Bob", "@".into());
        s.record_address("Bob", "Bob!u@host".into());
        let mut accum = HashMap::new();

        let fx = run_line(&mut s, &mut accum, ":Bob!u@host PART #c :bye");
        assert!(!s.has_member("#c", "Bob"));
        let script = fx.script_state.expect("departure script snapshot");
        assert_eq!(script.channels[0].nicks.len(), 2);
        assert!(script
            .ial
            .iter()
            .any(|(_, address)| address.starts_with("Bob!")));
        let pending = script
            .pending_nicklist_update
            .as_ref()
            .expect("pending updated roster");
        assert!(!pending.is_active());
        assert_eq!(pending.updated.channels[0].nicks, vec!["me"]);
        assert!(pending.updated.ial.is_empty());
    }

    #[test]
    fn channel_messages_refresh_member_idle_state() {
        let mut s = SessionState::default();
        s.upsert_member("#c", "Alice", String::new());
        s.channels
            .get_mut("#c")
            .unwrap()
            .member_activity
            .insert("Alice".into(), 1);
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":Alice!u@host PRIVMSG #c :hello");
        assert!(fx.channel_state_changed);
        assert!(s.channels["#c"].member_activity["Alice"] > 1);
    }

    #[test]
    fn standard_who_and_whox_populate_rich_ial_fields() {
        let mut s = SessionState::default();
        let mut accum = HashMap::new();

        let fx = run_line(
            &mut s,
            &mut accum,
            ":srv 352 me #room user host.test server Alice G@ :0 Alice Real",
        );
        assert!(fx.ial_changed);
        assert_eq!(s.ial["alice"], "Alice!user@host.test");
        assert_eq!(s.ial_info["alice"].away, Some(true));
        assert_eq!(s.ial_info["alice"].gecos, "Alice Real");

        let fx = run_line(
            &mut s,
            &mut accum,
            ":srv 354 me 995 #room newuser new.host server Alice H 0 12 account :Alice Updated",
        );
        assert!(fx.ial_changed);
        assert_eq!(s.ial["alice"], "Alice!newuser@new.host");
        assert_eq!(s.ial_info["alice"].account, "account");
        assert_eq!(s.ial_info["alice"].away, Some(false));
        assert_eq!(s.ial_info["alice"].gecos, "Alice Updated");
    }

    #[test]
    fn ial_tracks_notifications_rename_and_last_common_channel() {
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        run_line(&mut s, &mut accum, ":Alice!u@old.host JOIN #room");
        run_line(&mut s, &mut accum, ":Alice!u@old.host ACCOUNT acct");
        run_line(&mut s, &mut accum, ":Alice!u@old.host AWAY :gone");
        run_line(
            &mut s,
            &mut accum,
            ":Alice!u@old.host CHGHOST ident new.host",
        );
        run_line(&mut s, &mut accum, ":Alice!ident@new.host NICK Alicia");

        assert_eq!(s.ial["alicia"], "Alicia!ident@new.host");
        assert_eq!(s.ial_info["alicia"].account, "acct");
        assert_eq!(s.ial_info["alicia"].away, Some(true));
        run_line(&mut s, &mut accum, ":Alicia!ident@new.host PART #room :bye");
        assert!(!s.ial.contains_key("alicia"));
    }

    #[test]
    fn ial_controls_never_require_server_round_trips() {
        let mut s = SessionState::default();
        s.record_address("Alice", "Alice!u@h".into());
        apply_ial_control(&mut s, "MARK\t0\t0\tAlice\tnote\ttrusted user");
        assert_eq!(s.ial_info["alice"].marks["note"], "trusted user");
        apply_ial_control(&mut s, "MARK\t1\t1\tAlice\tn*\t");
        assert!(s.ial_info["alice"].marks.is_empty());
        apply_ial_control(&mut s, "OFF");
        assert!(s.ial_disabled);
        assert!(s.ial.is_empty());
        apply_ial_control(&mut s, "ON");
        assert!(!s.ial_disabled);
    }

    #[test]
    fn names_channel_matching_is_case_insensitive_and_preserves_join_spelling() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        run_line(&mut s, &mut accum, ":me!u@h JOIN #MyChannel");
        run_line(&mut s, &mut accum, ":srv 353 me = #mychannel :@alice");
        run_line(&mut s, &mut accum, ":srv 353 me = #MYCHANNEL :+bob me");
        let fx = run_line(
            &mut s,
            &mut accum,
            ":srv 366 me #mYcHaNnEl :End of /NAMES list",
        );

        assert!(accum.is_empty());
        assert_eq!(s.channels.len(), 1);
        let ch = &s.channels["#MyChannel"];
        assert_eq!(ch.members["alice"], "@");
        assert_eq!(ch.members["bob"], "+");
        assert!(ch.members.contains_key("me"));
        assert!(matches!(
            fx.events.last(),
            Some(UiEvent::Names { channel, members, .. })
                if channel == "#MyChannel" && members.len() == 3
        ));
    }

    #[test]
    fn isupport_casemapping_and_statusmsg_drive_protocol_routing() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(
            &mut s,
            &mut accum,
            ":srv 005 me CASEMAPPING=strict-rfc1459 STATUSMSG=@+ CHANTYPES=# :are supported",
        );
        assert_eq!(
            s.isupport.case_mapping,
            crate::irc::state::CaseMapping::StrictRfc1459
        );
        assert_eq!(s.isupport.status_msg, "@+");
        assert!(matches!(
            fx.events.last(),
            Some(UiEvent::Isupport { case_mapping, status_msg, .. })
                if case_mapping == "strict-rfc1459" && status_msg == "@+"
        ));

        run_line(&mut s, &mut accum, ":me!u@h JOIN #Room");
        let fx = run_line(
            &mut s,
            &mut accum,
            ":operator!u@h PRIVMSG @#room :status-targeted",
        );
        assert!(matches!(
            fx.events.last(),
            Some(UiEvent::Message { target, text, .. })
                if target == "#Room" && text == "status-targeted"
        ));
    }

    #[test]
    fn rfc1459_equivalent_membership_events_update_one_roster() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        run_line(&mut s, &mut accum, ":me!u@h JOIN #Chan[");
        run_line(&mut s, &mut accum, ":User[!u@h JOIN #chan{");
        assert!(s.has_member("#CHAN{", "user{"));

        let fx = run_line(&mut s, &mut accum, ":user{!u@h PART #CHAN{ :bye");
        assert!(!s.has_member("#chan[", "User["));
        assert!(matches!(
            fx.events.last(),
            Some(UiEvent::Part { channel, nick, .. })
                if channel == "#Chan[" && nick == "user{"
        ));
    }

    #[test]
    fn parses_guest_prefixed_nick() {
        // IRC7/MSN nicks can start with a status char like '>'. Confirm we can
        // still get a message out of such a line (sanitised if irc-proto rejects it).
        let line = ":>HappyWombat61!CF86@GateKeeper PRIVMSG #c :hi there";
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, line);
        let got = fx.events.iter().find_map(|e| match e {
            UiEvent::Message { from, text, .. } => Some((from.clone(), text.clone())),
            _ => None,
        });
        assert_eq!(
            got,
            Some((Some(">HappyWombat61".into()), "hi there".into()))
        );
    }

    #[test]
    fn strips_ircx_font_descriptor() {
        // MSN-Chat font tags always carry the leading \x01.
        assert_eq!(strip_ircx_font("\u{1}S Tahoma;0 hkjhkh"), Some("hkjhkh"));
        assert_eq!(
            strip_ircx_font("\u{1}S Times New Roman;0 hi there"),
            Some("hi there")
        );
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
        let mut state = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
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
        let mut state = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
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
        let mut state = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
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
        let mut state = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
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
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Message { .. })));
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
            let fx = run_line(
                &mut s,
                &mut accum,
                &format!(":bob!u@h PRIVMSG me :\u{1}{req}\u{1}"),
            );
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
        let fx = run_line(
            &mut s,
            &mut accum,
            ":bob!u@h NOTICE me :\u{1}VERSION jIRC 1.0\u{1}",
        );
        // UI: a readable Echo, no raw Message, and a reply is never auto-replied to.
        assert!(fx.events.iter().any(
            |e| matches!(e, UiEvent::Echo { text, .. } if text.contains("CTCP reply from bob"))
        ));
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Message { .. })));
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
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Message { .. })));
        assert!(fx.outgoing.is_empty());
    }

    #[test]
    fn passive_dcc_offer_preserves_token_and_resume_is_internal() {
        let mut state = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let offer = run_line(
            &mut state,
            &mut accum,
            ":bob!u@h PRIVMSG me :\u{1}DCC SEND file.bin 3232235521 0 99 4242\u{1}",
        );
        assert!(matches!(
            offer.dcc_message.as_ref(),
            Some((nick, crate::irc::dcc::DccMessage::Offer(dcc)))
                if nick == "bob" && dcc.port == 0 && dcc.token == Some(4242)
        ));
        assert!(offer.events.iter().any(|event| matches!(
            event,
            UiEvent::DccFileOffer {
                token: Some(4242),
                ..
            }
        )));

        let resume = run_line(
            &mut state,
            &mut accum,
            ":bob!u@h PRIVMSG me :\u{1}DCC RESUME file.bin 0 20 4242\u{1}",
        );
        assert!(matches!(
            resume.dcc_message.as_ref(),
            Some((_, crate::irc::dcc::DccMessage::Resume { position: 20, .. }))
        ));
        assert!(resume.events.is_empty());
    }

    #[test]
    fn ctcp_action_still_renders() {
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        let mut accum = HashMap::new();
        let fx = run_line(
            &mut s,
            &mut accum,
            ":bob!u@h PRIVMSG #c :\u{1}ACTION waves\u{1}",
        );
        assert!(fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::Message { .. })));
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
        let run = |s: &mut SessionState,
                   accum: &mut HashMap<String, Vec<String>>,
                   whois: &mut HashMap<String, Vec<String>>,
                   auth: &mut AuthState| {
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
        match fx
            .events
            .iter()
            .find(|e| matches!(e, UiEvent::Message { .. }))
        {
            Some(UiEvent::Message { time, .. }) => {
                assert_eq!(time.as_deref(), Some("2021-01-02T03:04:05.000Z"));
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn account_tag_updates_ial_for_direct_messages() {
        let mut state = SessionState::default();
        let mut accum = HashMap::new();
        run_line(
            &mut state,
            &mut accum,
            "@account=bob_account :bob!u@h PRIVMSG me :hello",
        );
        assert_eq!(state.ial_info["bob"].account, "bob_account");
    }

    #[test]
    fn batch_delimiters_are_structural_and_silent() {
        let mut state = SessionState::default();
        let mut accum = HashMap::new();
        let open = run_line(
            &mut state,
            &mut accum,
            ":server BATCH +history chathistory #room",
        );
        let close = run_line(&mut state, &mut accum, ":server BATCH -history");
        assert!(open.events.is_empty());
        assert!(close.events.is_empty());
    }

    #[test]
    fn invite_emits_event() {
        let mut s = SessionState::default();
        let mut accum = HashMap::new();
        let fx = run_line(&mut s, &mut accum, ":bob!u@h INVITE me #cool");
        match fx
            .events
            .iter()
            .find(|e| matches!(e, UiEvent::Invite { .. }))
        {
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
        match fx
            .events
            .iter()
            .find(|e| matches!(e, UiEvent::AwayChange { .. }))
        {
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
        let mut p = profile();
        p.ircx = true;
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov)~@+");
        s.upsert_member("%#room", "me", String::new());
        s.upsert_member("%#room", "bob", String::new());
        let mut accum = HashMap::new();
        // +q on us -> OwnerGranted for the channel.
        let fx = run_line_with_profile(&mut s, &mut accum, ":host!u@h MODE %#room +q me", &p);
        assert!(fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerGranted { channel, .. } if channel == "%#room")));
        // A duplicate +q echo is not a second ownership transition.
        let fx = run_line_with_profile(&mut s, &mut accum, ":host!u@h MODE %#room +q me", &p);
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerGranted { .. })));
        // +q on someone else, or -q on us -> no OwnerGranted.
        let fx = run_line_with_profile(&mut s, &mut accum, ":host!u@h MODE %#room +q bob", &p);
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerGranted { .. })));
        let fx = run_line_with_profile(&mut s, &mut accum, ":host!u@h MODE %#room -q me", &p);
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerGranted { .. })));
    }

    #[test]
    fn script_managed_connection_does_not_start_native_owner_management() {
        let p = profile();
        assert!(!p.ircx);
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov)~@+");
        s.upsert_member("%#room", "me", String::new());
        let mut accum = HashMap::new();

        let fx = run_line_with_profile(&mut s, &mut accum, ":i7con MODE %#room +q me", &p);

        assert!(s.channels["%#room"].members["me"].contains('~'));
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerGranted { .. })));
    }

    #[test]
    fn owner_revoked_only_when_someone_else_takes_our_q() {
        let mut p = profile();
        p.ircx = true;
        let mut s = SessionState {
            nick: "me".into(),
            ..Default::default()
        };
        s.isupport.parse_token("CHANTYPES=%#");
        s.isupport.parse_token("PREFIX=(qov)~@+");
        s.upsert_member("%#room", "me", "~".into());
        s.upsert_member("%#room", "bob", String::new());
        let mut accum = HashMap::new();
        // -q on us by someone else -> OwnerRevoked naming the offender.
        let fx = run_line_with_profile(&mut s, &mut accum, ":taker!u@h MODE %#room -q me", &p);
        assert!(fx.events.iter().any(|e| matches!(
            e,
            UiEvent::OwnerRevoked { channel, by, .. } if channel == "%#room" && by == "taker"
        )));
        // -q on someone else, or -q we set ourselves -> no OwnerRevoked.
        let fx = run_line_with_profile(&mut s, &mut accum, ":taker!u@h MODE %#room -q bob", &p);
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerRevoked { .. })));
        let fx = run_line_with_profile(&mut s, &mut accum, ":me!u@h MODE %#room -q me", &p);
        assert!(!fx
            .events
            .iter()
            .any(|e| matches!(e, UiEvent::OwnerRevoked { .. })));
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
        send(
            &mut write_half,
            &format!("USER {nick} 0 * :jIRC smoke test"),
        )
        .await;

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

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 100000;
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
            let Ok(msg) = line.parse::<Message>() else {
                continue;
            };
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
        p.perform = vec!["mode $me +i".into(), "msg NickServ STATUS".into()];
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
        let fx = process_message(
            &mut ctx,
            ":srv 001 me :Welcome",
            ":srv 001 me :Welcome".parse().unwrap(),
        );
        // The autojoin is deferred to the connection task (after `on CONNECT`),
        // so it's reported via `fx.autojoin`, not sent inline.
        assert_eq!(fx.autojoin, vec!["#jirc".to_string()]);
        assert_eq!(
            fx.perform,
            vec!["mode $me +i".to_string(), "msg NickServ STATUS".to_string()]
        );
        assert!(!fx.outgoing.iter().any(|l| l.starts_with("JOIN")));
        assert_eq!(s.nick, "me");
    }

    #[test]
    fn raw_line_helpers_ignore_ircv3_tags() {
        let line = "@time=2026-07-15T00:00:00Z;label=x :bob!u@h PRIVMSG #c :hello world";
        assert_eq!(source_nick(line), "bob");
        assert_eq!(
            raw_command_params(line),
            Some(("PRIVMSG".into(), vec!["#c".into(), "hello world".into()]))
        );
    }
}
