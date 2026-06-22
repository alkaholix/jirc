//! IRCX dialect handling (the Microsoft chat extension protocol).
//!
//! IRCX numerics (800–999) and extension commands are unknown to `irc-proto`,
//! so they arrive as [`irc_proto::Command::Raw`]. This module translates those
//! raw frames into typed [`UiEvent`]s. Outgoing IRCX commands are formatted by
//! the Tauri command layer as raw lines.
//!
//! Numerics per the IRCX draft (`draft-pfenning-irc-extensions-04`):
//! - 800 IRCRPL_IRCX — capabilities: `<state> <version> <packages> <maxmsg> <options>`
//! - 801 ACCESSADD / 804 ACCESSLIST — `<object> <level> <mask> <timeout> <user> <reason>`
//! - 802 ACCESSDELETE, 803 ACCESSSTART, 805 ACCESSEND
//! - 811 LISTXSTART / 812 LISTXLIST / 816 LISTXTRUNC / 817 LISTXEND
//! - 818 PROPLIST — `<object> <property> <value>` / 819 PROPEND
//! - 900–999 — error replies.

use crate::irc::event::UiEvent;

/// Error-reply numeric range.
pub const IRCX_ERROR_MIN: u16 = 900;
pub const IRCX_ERROR_MAX: u16 = 999;

/// Handles an `irc-proto` raw frame. Returns a typed event when recognized.
///
/// `args` includes the leading target nick for numerics (per IRC), which we
/// skip when extracting fields.
pub fn raw_event(
    server_id: &str,
    source: Option<String>,
    cmd: &str,
    args: &[String],
) -> Option<UiEvent> {
    if let Ok(code) = cmd.parse::<u16>() {
        return numeric_event(server_id, code, args);
    }
    match cmd.to_ascii_uppercase().as_str() {
        "WHISPER" => {
            // WHISPER <channel> <targets> :<text>
            let channel = args.first()?.clone();
            let text = args.last()?.clone();
            Some(UiEvent::Whisper {
                server_id: server_id.to_string(),
                from: source,
                channel,
                text,
            })
        }
        _ => None,
    }
}

/// Translates a numeric reply (any code) into a typed event. IRCX-specific
/// codes get rich events; everything else falls back to a generic `Numeric`.
pub fn numeric_event(server_id: &str, code: u16, args: &[String]) -> Option<UiEvent> {
    // Field accessor that skips the leading target nick (args[0]).
    let f = |i: usize| args.get(i + 1).cloned();
    let sid = || server_id.to_string();

    let event = match code {
        800 => UiEvent::IrcxState {
            server_id: sid(),
            // args after nick: state, version, packages, maxmsg, options
            version: f(1),
            packages: f(2),
            max_message_length: f(3),
            options: f(4),
        },
        801 | 804 => UiEvent::IrcxAccess {
            server_id: sid(),
            object: f(0).unwrap_or_default(),
            level: f(1),
            mask: f(2),
            timeout: f(3),
            set_by: f(4),
            reason: f(5),
        },
        805 => UiEvent::IrcxAccessEnd {
            server_id: sid(),
            object: f(0).unwrap_or_default(),
        },
        // LISTX channel entry — feed the same channel-list window as LIST.
        // Field order varies by server, so parse defensively: channel is first,
        // users is the first numeric field, topic is the last non-numeric field.
        812 => {
            let channel = f(0).unwrap_or_default();
            let users = (1..6)
                .find_map(|i| f(i).and_then(|s| s.parse::<u32>().ok()))
                .unwrap_or(0);
            let topic = args
                .last()
                .filter(|t| **t != channel && t.parse::<u32>().is_err())
                .cloned()
                .unwrap_or_default();
            UiEvent::ListEntry {
                server_id: sid(),
                channel,
                users,
                topic,
            }
        }
        // LISTX truncated / end.
        816 | 817 => UiEvent::ListEnd { server_id: sid() },
        818 => UiEvent::IrcxProp {
            server_id: sid(),
            object: f(0).unwrap_or_default(),
            name: f(1).unwrap_or_default(),
            value: f(2).unwrap_or_default(),
        },
        819 => UiEvent::IrcxPropEnd {
            server_id: sid(),
            object: f(0).unwrap_or_default(),
        },
        IRCX_ERROR_MIN..=IRCX_ERROR_MAX => UiEvent::Error {
            server_id: sid(),
            message: format!("IRCX error {code}: {}", args.join(" ")),
        },
        _ => UiEvent::Numeric {
            server_id: sid(),
            code,
            args: args.to_vec(),
        },
    };
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ircx_state() {
        let args: Vec<String> = ["me", "1", "5.0", "ANON,NTLM", "512", "*"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ev = numeric_event("s", 800, &args).unwrap();
        match ev {
            UiEvent::IrcxState {
                version, packages, ..
            } => {
                assert_eq!(version.as_deref(), Some("5.0"));
                assert_eq!(packages.as_deref(), Some("ANON,NTLM"));
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parses_prop() {
        let args: Vec<String> = ["me", "#chan", "TOPIC", "hello world"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match numeric_event("s", 818, &args).unwrap() {
            UiEvent::IrcxProp {
                object, name, value, ..
            } => {
                assert_eq!(object, "#chan");
                assert_eq!(name, "TOPIC");
                assert_eq!(value, "hello world");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn error_codes_become_error_events() {
        let args: Vec<String> = ["me", "no access"].iter().map(|s| s.to_string()).collect();
        assert!(matches!(
            numeric_event("s", 913, &args),
            Some(UiEvent::Error { .. })
        ));
    }

    #[test]
    fn whisper_parsed() {
        let args: Vec<String> = ["#chan", "me", "secret"].iter().map(|s| s.to_string()).collect();
        match raw_event("s", Some("bob".into()), "WHISPER", &args).unwrap() {
            UiEvent::Whisper {
                from, channel, text, ..
            } => {
                assert_eq!(from.as_deref(), Some("bob"));
                assert_eq!(channel, "#chan");
                assert_eq!(text, "secret");
            }
            _ => panic!("wrong event"),
        }
    }
}
