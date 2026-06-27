//! CAP negotiation and SASL PLAIN authentication.
//!
//! Flow: the connection sends `CAP LS 302` at registration. On the LS reply we
//! either request `sasl` (if wanted and offered) or end negotiation. On ACK we
//! begin `AUTHENTICATE PLAIN`; the server replies `AUTHENTICATE +`, we send the
//! base64 credential, and a 903 (success) / 904-907 (failure) numeric concludes
//! it. Either way we finish with `CAP END`.

use base64::{engine::general_purpose::STANDARD, Engine};
use irc_proto::CapSubCommand;

use crate::config::ServerProfile;

#[derive(Debug, Default)]
pub struct AuthState {
    pub sasl_attempted: bool,
    pub sasl_succeeded: bool,
    pub cap_ended: bool,
}

/// Whether SASL should be attempted for this profile.
pub fn sasl_wanted(p: &ServerProfile) -> bool {
    p.sasl && p.account_password.as_deref().is_some_and(|s| !s.is_empty())
}

fn end_cap(state: &mut AuthState) -> Vec<String> {
    if state.cap_ended {
        return vec![];
    }
    state.cap_ended = true;
    vec!["CAP END".to_string()]
}

/// Capabilities jIRC understands and will request if the server offers them.
/// These are passive (the server just sends richer info); we read what we use.
const SUPPORTED_CAPS: &[&str] = &[
    "away-notify",
    "server-time",
    "multi-prefix",
    "extended-join",
    "account-notify",
    "chghost",
    "userhost-in-names",
    "message-tags",
];

/// Handles a CAP reply. `caps` is the space-separated capability list.
pub fn on_cap(p: &ServerProfile, state: &mut AuthState, sub: &CapSubCommand, caps: &str) -> Vec<String> {
    let has = |name: &str| caps.split_whitespace().any(|c| c.eq_ignore_ascii_case(name));
    match sub {
        CapSubCommand::LS | CapSubCommand::NEW => {
            let mut req: Vec<&str> = Vec::new();
            if sasl_wanted(p) && has("sasl") {
                req.push("sasl");
            }
            for cap in SUPPORTED_CAPS {
                if has(cap) {
                    req.push(cap);
                }
            }
            if req.is_empty() {
                end_cap(state)
            } else {
                vec![format!("CAP REQ :{}", req.join(" "))]
            }
        }
        CapSubCommand::ACK => {
            if has("sasl") {
                state.sasl_attempted = true;
                vec!["AUTHENTICATE PLAIN".to_string()]
            } else {
                // away-notify (and friends) need no follow-up; finish negotiation.
                end_cap(state)
            }
        }
        CapSubCommand::NAK => end_cap(state),
        _ => vec![],
    }
}

/// Handles an `AUTHENTICATE` challenge from the server.
pub fn on_authenticate(p: &ServerProfile, data: &str) -> Vec<String> {
    if data.trim() == "+" {
        let account = p.account();
        let password = p.account_password.clone().unwrap_or_default();
        // SASL PLAIN: authzid \0 authcid \0 passwd (empty authzid).
        let payload = format!("\u{0}{account}\u{0}{password}");
        vec![format!("AUTHENTICATE {}", STANDARD.encode(payload))]
    } else {
        vec![]
    }
}

/// Concludes SASL after a result numeric. `success` reflects 903 vs 904-907.
pub fn on_sasl_result(state: &mut AuthState, success: bool) -> Vec<String> {
    state.sasl_succeeded = success;
    end_cap(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(sasl: bool, pw: Option<&str>) -> ServerProfile {
        ServerProfile {
            id: None,
            name: "n".into(),
            host: "h".into(),
            port: 6667,
            tls: false,
            tls_insecure: false,
            ircx: false,
            sasl,
            account: Some("acct".into()),
            account_password: pw.map(String::from),
            nickserv: false,
            auto_reconnect: false,
            proxy: None,
            nick: "nick".into(),
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

    #[test]
    fn requests_sasl_and_supported_caps_when_offered() {
        let p = profile(true, Some("secret"));
        let mut st = AuthState::default();
        // sasl first, then any offered caps we support (here: multi-prefix).
        assert_eq!(
            on_cap(&p, &mut st, &CapSubCommand::LS, "sasl multi-prefix foobar"),
            vec!["CAP REQ :sasl multi-prefix"]
        );
    }

    #[test]
    fn requests_only_sasl_when_no_other_supported_caps() {
        let p = profile(true, Some("secret"));
        let mut st = AuthState::default();
        assert_eq!(
            on_cap(&p, &mut st, &CapSubCommand::LS, "sasl foobar"),
            vec!["CAP REQ :sasl"]
        );
    }

    #[test]
    fn ends_cap_when_sasl_not_wanted() {
        let p = profile(false, None);
        let mut st = AuthState::default();
        assert_eq!(on_cap(&p, &mut st, &CapSubCommand::LS, "sasl"), vec!["CAP END"]);
    }

    #[test]
    fn authenticate_plain_payload() {
        let p = profile(true, Some("secret"));
        let out = on_authenticate(&p, "+");
        // base64 of "\0acct\0secret"
        let expected = STANDARD.encode("\u{0}acct\u{0}secret");
        assert_eq!(out, vec![format!("AUTHENTICATE {expected}")]);
    }

    #[test]
    fn cap_end_sent_once() {
        let mut st = AuthState::default();
        assert_eq!(on_sasl_result(&mut st, true), vec!["CAP END"]);
        assert!(on_sasl_result(&mut st, true).is_empty());
    }
}
