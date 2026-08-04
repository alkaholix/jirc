//! IRCv3 CAP negotiation and SASL authentication.
//!
//! CAP LS can span multiple replies.  We accumulate the complete offer before
//! sending one request, then keep the request pending until every capability is
//! ACKed or NAKed. SASL supports PLAIN, EXTERNAL, SCRAM-SHA-256, and
//! OAUTHBEARER; all
//! AUTHENTICATE payloads use the protocol's 400-byte chunking rules.

use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use irc_proto::CapSubCommand;
use pbkdf2::pbkdf2_hmac;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::{SaslMechanism, ServerProfile};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Default, PartialEq, Eq)]
enum SaslPhase {
    #[default]
    Idle,
    AwaitingInitial,
    AwaitingServerFirst,
    AwaitingServerFinal,
    AwaitingResult,
    Verified,
    Complete,
    Failed,
}

#[derive(Debug, Default)]
pub struct AuthState {
    pub sasl_attempted: bool,
    pub sasl_succeeded: bool,
    pub cap_ended: bool,
    ls_in_progress: bool,
    offered_caps: HashMap<String, Option<String>>,
    pending_caps: HashSet<String>,
    acknowledged_caps: HashSet<String>,
    mechanism: Option<SaslMechanism>,
    phase: SaslPhase,
    incoming_authenticate: String,
    scram_nonce: String,
    scram_client_first_bare: String,
    scram_expected_server_signature: Vec<u8>,
}

impl AuthState {
    /// Whether the server acknowledged an IRCv3 capability for this session.
    pub fn cap_enabled(&self, name: &str) -> bool {
        self.acknowledged_caps.contains(&name.to_ascii_lowercase())
    }
}

/// Whether SASL should be attempted for this profile.
pub fn sasl_wanted(p: &ServerProfile) -> bool {
    p.sasl
        && (p.sasl_mechanism == SaslMechanism::External
            || p.account_password.as_deref().is_some_and(|s| !s.is_empty()))
}

fn end_cap(state: &mut AuthState) -> Vec<String> {
    if state.cap_ended {
        return vec![];
    }
    state.cap_ended = true;
    vec!["CAP END".to_string()]
}

/// Capabilities jIRC understands and will request if the server offers them.
const SUPPORTED_CAPS: &[&str] = &[
    "away-notify",
    "server-time",
    "multi-prefix",
    "extended-join",
    "account-notify",
    "chghost",
    "userhost-in-names",
    "message-tags",
    "echo-message",
    "account-tag",
    "invite-notify",
    "setname",
    "batch",
    "labeled-response",
    "draft/chathistory",
];

fn parse_cap_token(token: &str) -> Option<(String, Option<String>, bool)> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let disabled = token.starts_with('-');
    let bare = token.trim_start_matches(|c: char| matches!(c, '-' | '~' | '='));
    let (name, value) = bare.split_once('=').map_or((bare, None), |(name, value)| {
        (name, Some(value.to_string()))
    });
    if name.is_empty() {
        return None;
    }
    Some((name.to_ascii_lowercase(), value, disabled))
}

fn add_offers(state: &mut AuthState, caps: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    for token in caps.split_whitespace() {
        if let Some((name, value, disabled)) = parse_cap_token(token) {
            if disabled {
                state.offered_caps.remove(&name);
            } else {
                names.insert(name.clone());
                state.offered_caps.insert(name, value);
            }
        }
    }
    names
}

fn sasl_mechanism_offered(p: &ServerProfile, state: &AuthState) -> bool {
    let Some(value) = state.offered_caps.get("sasl") else {
        return false;
    };
    let Some(value) = value.as_deref().filter(|value| !value.is_empty()) else {
        // A value-less `sasl` offer means the mechanism list is unspecified.
        return true;
    };
    value
        .split(',')
        .any(|mechanism| mechanism.eq_ignore_ascii_case(p.sasl_mechanism.as_str()))
}

fn desired_caps(
    p: &ServerProfile,
    state: &AuthState,
    only: Option<&HashSet<String>>,
) -> Vec<String> {
    let eligible = |name: &str| {
        state.offered_caps.contains_key(name)
            && only.is_none_or(|names| names.contains(name))
            && !state.pending_caps.contains(name)
            && !state.acknowledged_caps.contains(name)
    };
    let mut desired = Vec::new();
    if sasl_wanted(p) && eligible("sasl") && sasl_mechanism_offered(p, state) {
        desired.push("sasl".to_string());
    }
    desired.extend(
        SUPPORTED_CAPS
            .iter()
            .copied()
            .filter(|cap| {
                eligible(cap)
                    && (*cap != "labeled-response" || state.offered_caps.contains_key("batch"))
                    && (*cap != "draft/chathistory" || state.offered_caps.contains_key("batch"))
            })
            .map(str::to_string),
    );
    desired
}

fn request_caps(
    p: &ServerProfile,
    state: &mut AuthState,
    only: Option<&HashSet<String>>,
    finish_if_empty: bool,
) -> Vec<String> {
    let desired = desired_caps(p, state, only);
    if desired.is_empty() {
        return if finish_if_empty {
            end_cap(state)
        } else {
            Vec::new()
        };
    }
    state.pending_caps.extend(desired.iter().cloned());
    vec![format!("CAP REQ :{}", desired.join(" "))]
}

fn sasl_in_progress(state: &AuthState) -> bool {
    matches!(
        state.phase,
        SaslPhase::AwaitingInitial
            | SaslPhase::AwaitingServerFirst
            | SaslPhase::AwaitingServerFinal
            | SaslPhase::AwaitingResult
            | SaslPhase::Verified
    )
}

/// Handles a CAP reply. `caps` is the space-separated capability list and
/// `continuation` is true for the `CAP * LS * :...` continuation form.
pub fn on_cap(
    p: &ServerProfile,
    state: &mut AuthState,
    sub: &CapSubCommand,
    caps: &str,
    continuation: bool,
) -> Vec<String> {
    match sub {
        CapSubCommand::LS => {
            if !state.ls_in_progress {
                state.offered_caps.clear();
            }
            add_offers(state, caps);
            state.ls_in_progress = continuation;
            if continuation {
                Vec::new()
            } else {
                request_caps(p, state, None, true)
            }
        }
        CapSubCommand::NEW => {
            let names = add_offers(state, caps);
            request_caps(p, state, Some(&names), false)
        }
        CapSubCommand::ACK => {
            let mut sasl_acked = false;
            for token in caps.split_whitespace() {
                if let Some((name, _, disabled)) = parse_cap_token(token) {
                    state.pending_caps.remove(&name);
                    if disabled {
                        state.acknowledged_caps.remove(&name);
                    } else {
                        sasl_acked |= name == "sasl";
                        state.acknowledged_caps.insert(name);
                    }
                }
            }

            let mut outgoing = Vec::new();
            if sasl_acked && !state.sasl_attempted && sasl_wanted(p) {
                state.sasl_attempted = true;
                state.mechanism = Some(p.sasl_mechanism);
                state.phase = SaslPhase::AwaitingInitial;
                outgoing.push(format!("AUTHENTICATE {}", p.sasl_mechanism.as_str()));
            }
            if state.pending_caps.is_empty() && !sasl_in_progress(state) {
                outgoing.extend(end_cap(state));
            }
            outgoing
        }
        CapSubCommand::NAK => {
            let rejected: Vec<String> = caps
                .split_whitespace()
                .filter_map(parse_cap_token)
                .map(|(name, _, _)| name)
                .collect();
            if rejected.is_empty() {
                state.pending_caps.clear();
            } else {
                for name in rejected {
                    state.pending_caps.remove(&name);
                }
            }
            if state.pending_caps.is_empty() && !sasl_in_progress(state) {
                end_cap(state)
            } else {
                Vec::new()
            }
        }
        CapSubCommand::DEL => {
            for token in caps.split_whitespace() {
                if let Some((name, _, _)) = parse_cap_token(token) {
                    state.acknowledged_caps.remove(&name);
                    state.offered_caps.remove(&name);
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn authenticate_chunks(encoded: &str) -> Vec<String> {
    if encoded.is_empty() {
        return vec!["AUTHENTICATE +".to_string()];
    }
    let mut outgoing: Vec<String> = encoded
        .as_bytes()
        .chunks(400)
        .map(|chunk| {
            format!(
                "AUTHENTICATE {}",
                std::str::from_utf8(chunk).unwrap_or_default()
            )
        })
        .collect();
    if encoded.len() % 400 == 0 {
        outgoing.push("AUTHENTICATE +".to_string());
    }
    outgoing
}

fn encode_authenticate(payload: &[u8]) -> Vec<String> {
    authenticate_chunks(&STANDARD.encode(payload))
}

fn fail_exchange(state: &mut AuthState) -> Vec<String> {
    state.phase = SaslPhase::Failed;
    state.incoming_authenticate.clear();
    vec!["AUTHENTICATE *".to_string()]
}

fn scram_escape_username(username: &str) -> String {
    username.replace('=', "=3D").replace(',', "=2C")
}

fn scram_attr<'a>(message: &'a str, name: &str) -> Option<&'a str> {
    message.split(',').find_map(|part| {
        part.split_once('=')
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| value)
    })
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn handle_complete_challenge(
    p: &ServerProfile,
    state: &mut AuthState,
    challenge: &str,
) -> Vec<String> {
    match (state.mechanism, &state.phase) {
        (Some(SaslMechanism::Plain), SaslPhase::AwaitingInitial) => {
            let account = p.account();
            let password = p.account_password.as_deref().unwrap_or_default();
            let payload = format!("\0{account}\0{password}");
            state.phase = SaslPhase::AwaitingResult;
            encode_authenticate(payload.as_bytes())
        }
        (Some(SaslMechanism::External), SaslPhase::AwaitingInitial) => {
            state.phase = SaslPhase::AwaitingResult;
            // EXTERNAL's response is an optional authorization identity.  An
            // omitted account must stay empty (AUTHENTICATE +), rather than
            // inheriting the nick as PLAIN/SCRAM do.
            encode_authenticate(p.account.as_deref().unwrap_or_default().as_bytes())
        }
        (Some(SaslMechanism::OAuthBearer), SaslPhase::AwaitingInitial) => {
            if !challenge.is_empty() {
                return fail_exchange(state);
            }
            let token = p.account_password.as_deref().unwrap_or_default();
            if token.is_empty() {
                return fail_exchange(state);
            }
            let authzid = p
                .account
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(|value| format!("a={}", value.replace('=', "=3D").replace(',', "=2C")))
                .unwrap_or_default();
            let payload = format!("n,{authzid},\u{1}auth=Bearer {token}\u{1}\u{1}");
            state.phase = SaslPhase::AwaitingResult;
            encode_authenticate(payload.as_bytes())
        }
        (Some(SaslMechanism::OAuthBearer), SaslPhase::AwaitingResult) => {
            // RFC 7628 sends a base64 JSON error challenge before the failure
            // numeric. An empty response acknowledges it and lets the server
            // finish the exchange.
            vec!["AUTHENTICATE +".into()]
        }
        (Some(SaslMechanism::ScramSha256), SaslPhase::AwaitingInitial) => {
            if !challenge.is_empty() {
                return fail_exchange(state);
            }
            state.scram_nonce = Uuid::new_v4().simple().to_string();
            state.scram_client_first_bare = format!(
                "n={},r={}",
                scram_escape_username(p.account()),
                state.scram_nonce
            );
            let first = format!("n,,{}", state.scram_client_first_bare);
            state.phase = SaslPhase::AwaitingServerFirst;
            encode_authenticate(first.as_bytes())
        }
        (Some(SaslMechanism::ScramSha256), SaslPhase::AwaitingServerFirst) => {
            let Ok(decoded) = STANDARD.decode(challenge) else {
                return fail_exchange(state);
            };
            let Ok(server_first) = String::from_utf8(decoded) else {
                return fail_exchange(state);
            };
            if scram_attr(&server_first, "m").is_some() {
                return fail_exchange(state);
            }
            let (Some(nonce), Some(salt), Some(iterations)) = (
                scram_attr(&server_first, "r"),
                scram_attr(&server_first, "s"),
                scram_attr(&server_first, "i"),
            ) else {
                return fail_exchange(state);
            };
            if !nonce.starts_with(&state.scram_nonce) || nonce.len() <= state.scram_nonce.len() {
                return fail_exchange(state);
            }
            let Ok(salt) = STANDARD.decode(salt) else {
                return fail_exchange(state);
            };
            let Ok(iterations) = iterations.parse::<u32>() else {
                return fail_exchange(state);
            };
            if iterations == 0 {
                return fail_exchange(state);
            }

            let mut salted_password = [0u8; 32];
            pbkdf2_hmac::<Sha256>(
                p.account_password.as_deref().unwrap_or_default().as_bytes(),
                &salt,
                iterations,
                &mut salted_password,
            );
            let client_key = hmac_sha256(&salted_password, b"Client Key");
            let stored_key = Sha256::digest(&client_key);
            let final_without_proof = format!("c=biws,r={nonce}");
            let auth_message = format!(
                "{},{},{}",
                state.scram_client_first_bare, server_first, final_without_proof
            );
            let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
            let proof: Vec<u8> = client_key
                .iter()
                .zip(client_signature.iter())
                .map(|(key, signature)| key ^ signature)
                .collect();
            let server_key = hmac_sha256(&salted_password, b"Server Key");
            state.scram_expected_server_signature =
                hmac_sha256(&server_key, auth_message.as_bytes());
            state.phase = SaslPhase::AwaitingServerFinal;
            encode_authenticate(
                format!("{final_without_proof},p={}", STANDARD.encode(proof)).as_bytes(),
            )
        }
        (Some(SaslMechanism::ScramSha256), SaslPhase::AwaitingServerFinal) => {
            let Ok(decoded) = STANDARD.decode(challenge) else {
                return fail_exchange(state);
            };
            let Ok(server_final) = String::from_utf8(decoded) else {
                return fail_exchange(state);
            };
            if scram_attr(&server_final, "e").is_some() {
                return fail_exchange(state);
            }
            let Some(signature) = scram_attr(&server_final, "v") else {
                return fail_exchange(state);
            };
            let Ok(signature) = STANDARD.decode(signature) else {
                return fail_exchange(state);
            };
            if signature != state.scram_expected_server_signature {
                return fail_exchange(state);
            }
            state.phase = SaslPhase::Verified;
            Vec::new()
        }
        _ => fail_exchange(state),
    }
}

/// Handles an AUTHENTICATE challenge from the server, including challenge
/// chunks. A 400-byte chunk is continued; a shorter chunk (or `+` terminator)
/// completes the challenge.
pub fn on_authenticate(p: &ServerProfile, state: &mut AuthState, data: &str) -> Vec<String> {
    if !sasl_in_progress(state) || data == "*" {
        if data == "*" {
            state.phase = SaslPhase::Failed;
        }
        return Vec::new();
    }
    if data == "+" {
        let challenge = std::mem::take(&mut state.incoming_authenticate);
        return handle_complete_challenge(p, state, &challenge);
    }
    if data.len() > 400 || !data.is_ascii() {
        return fail_exchange(state);
    }
    state.incoming_authenticate.push_str(data);
    if data.len() == 400 {
        Vec::new()
    } else {
        let challenge = std::mem::take(&mut state.incoming_authenticate);
        handle_complete_challenge(p, state, &challenge)
    }
}

/// Concludes SASL after a result numeric. `success` reflects 903 vs 904-907.
pub fn on_sasl_result(state: &mut AuthState, success: bool) -> Vec<String> {
    let mechanism_verified =
        state.mechanism != Some(SaslMechanism::ScramSha256) || state.phase == SaslPhase::Verified;
    state.sasl_succeeded = success && mechanism_verified;
    state.phase = if state.sasl_succeeded {
        SaslPhase::Complete
    } else {
        SaslPhase::Failed
    };
    if state.pending_caps.is_empty() {
        end_cap(state)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(sasl: bool, pw: Option<&str>, mechanism: SaslMechanism) -> ServerProfile {
        ServerProfile {
            local_address: None,
            id: None,
            name: "n".into(),
            host: "h".into(),
            port: 6667,
            tls: false,
            tls_insecure: false,
            tls_client_cert_path: None,
            tls_client_key_path: None,
            ircx: false,
            sasl,
            sasl_mechanism: mechanism,
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
            ircx_auth_package: None,
            ntlm_domain: None,
            ntlm_user: None,
            ntlm_password: None,
            autojoin: vec![],
            perform: vec![],
        }
    }

    fn begin_sasl(p: &ServerProfile, offered: &str) -> AuthState {
        let mut state = AuthState::default();
        assert_eq!(
            on_cap(p, &mut state, &CapSubCommand::LS, offered, false),
            vec!["CAP REQ :sasl"]
        );
        assert_eq!(
            on_cap(p, &mut state, &CapSubCommand::ACK, "sasl", false),
            vec![format!("AUTHENTICATE {}", p.sasl_mechanism.as_str())]
        );
        state
    }

    #[test]
    fn accumulates_multiline_ls_before_requesting() {
        let p = profile(true, Some("secret"), SaslMechanism::Plain);
        let mut state = AuthState::default();
        assert!(on_cap(&p, &mut state, &CapSubCommand::LS, "multi-prefix", true).is_empty());
        assert_eq!(
            on_cap(
                &p,
                &mut state,
                &CapSubCommand::LS,
                "sasl=PLAIN,EXTERNAL server-time",
                false
            ),
            vec!["CAP REQ :sasl server-time multi-prefix"]
        );
    }

    #[test]
    fn negotiates_and_tracks_echo_message() {
        let p = profile(false, None, SaslMechanism::Plain);
        let mut state = AuthState::default();
        assert_eq!(
            on_cap(&p, &mut state, &CapSubCommand::LS, "echo-message", false),
            vec!["CAP REQ :echo-message"]
        );
        assert!(!state.cap_enabled("echo-message"));
        assert_eq!(
            on_cap(&p, &mut state, &CapSubCommand::ACK, "echo-message", false),
            vec!["CAP END"]
        );
        assert!(state.cap_enabled("ECHO-MESSAGE"));
    }

    #[test]
    fn negotiates_batch_dependent_capabilities_together() {
        let p = profile(false, None, SaslMechanism::Plain);
        let mut state = AuthState::default();
        assert_eq!(
            on_cap(
                &p,
                &mut state,
                &CapSubCommand::LS,
                "account-tag labeled-response draft/chathistory batch",
                false,
            ),
            vec!["CAP REQ :account-tag batch labeled-response draft/chathistory"]
        );

        let mut without_batch = AuthState::default();
        assert_eq!(
            on_cap(
                &p,
                &mut without_batch,
                &CapSubCommand::LS,
                "labeled-response draft/chathistory",
                false,
            ),
            vec!["CAP END"]
        );
    }

    #[test]
    fn capability_values_select_the_configured_mechanism() {
        let p = profile(true, Some("secret"), SaslMechanism::ScramSha256);
        let mut state = AuthState::default();
        assert_eq!(
            on_cap(
                &p,
                &mut state,
                &CapSubCommand::LS,
                "sasl=PLAIN,SCRAM-SHA-256",
                false
            ),
            vec!["CAP REQ :sasl"]
        );

        let mut state = AuthState::default();
        assert_eq!(
            on_cap(
                &p,
                &mut state,
                &CapSubCommand::LS,
                "sasl=PLAIN,EXTERNAL",
                false
            ),
            vec!["CAP END"]
        );
    }

    #[test]
    fn waits_for_all_cap_acknowledgements_and_sasl_result() {
        let p = profile(true, Some("secret"), SaslMechanism::Plain);
        let mut state = AuthState::default();
        assert_eq!(
            on_cap(
                &p,
                &mut state,
                &CapSubCommand::LS,
                "sasl multi-prefix",
                false
            ),
            vec!["CAP REQ :sasl multi-prefix"]
        );
        assert!(on_cap(&p, &mut state, &CapSubCommand::ACK, "multi-prefix", false).is_empty());
        assert_eq!(
            on_cap(&p, &mut state, &CapSubCommand::ACK, "sasl", false),
            vec!["AUTHENTICATE PLAIN"]
        );
        assert_eq!(on_sasl_result(&mut state, true), vec!["CAP END"]);
        assert!(state.sasl_succeeded);
    }

    #[test]
    fn plain_payload_is_chunked_and_exact_400_gets_a_terminator() {
        let password = "x".repeat(294);
        let p = profile(true, Some(&password), SaslMechanism::Plain);
        let mut state = begin_sasl(&p, "sasl=PLAIN");
        let outgoing = on_authenticate(&p, &mut state, "+");
        assert_eq!(outgoing.len(), 2);
        assert_eq!(
            outgoing[0].strip_prefix("AUTHENTICATE ").unwrap().len(),
            400
        );
        assert_eq!(outgoing[1], "AUTHENTICATE +");
    }

    #[test]
    fn external_does_not_require_a_password() {
        let p = profile(true, None, SaslMechanism::External);
        let mut state = begin_sasl(&p, "sasl=EXTERNAL");
        assert_eq!(
            on_authenticate(&p, &mut state, "+"),
            vec![format!("AUTHENTICATE {}", STANDARD.encode("acct"))]
        );
    }

    #[test]
    fn oauthbearer_sends_rfc_7628_initial_response_and_acknowledges_errors() {
        let p = profile(true, Some("access-token"), SaslMechanism::OAuthBearer);
        let mut state = begin_sasl(&p, "sasl=OAUTHBEARER");
        let outgoing = on_authenticate(&p, &mut state, "+");
        let payload = outgoing[0].strip_prefix("AUTHENTICATE ").unwrap();
        assert_eq!(
            String::from_utf8(STANDARD.decode(payload).unwrap()).unwrap(),
            "n,a=acct,\u{1}auth=Bearer access-token\u{1}\u{1}"
        );
        assert_eq!(
            on_authenticate(
                &p,
                &mut state,
                &STANDARD.encode(r#"{"status":"invalid_token"}"#),
            ),
            vec!["AUTHENTICATE +"]
        );
    }

    #[test]
    fn scram_sha_256_verifies_the_server_signature() {
        let p = profile(true, Some("pencil"), SaslMechanism::ScramSha256);
        let mut state = begin_sasl(&p, "sasl=SCRAM-SHA-256");
        let first = on_authenticate(&p, &mut state, "+");
        let first = first[0].strip_prefix("AUTHENTICATE ").unwrap();
        let decoded = String::from_utf8(STANDARD.decode(first).unwrap()).unwrap();
        assert!(decoded.starts_with("n,,n=acct,r="));

        let server_first = format!("r={}server,s=QSXCR+Q6sek8bf92,i=4096", state.scram_nonce);
        let final_message = on_authenticate(&p, &mut state, &STANDARD.encode(server_first));
        let decoded_final = String::from_utf8(
            STANDARD
                .decode(final_message[0].strip_prefix("AUTHENTICATE ").unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(decoded_final.starts_with("c=biws,r="));
        assert!(decoded_final.contains(",p="));

        let server_final = format!(
            "v={}",
            STANDARD.encode(&state.scram_expected_server_signature)
        );
        assert!(on_authenticate(&p, &mut state, &STANDARD.encode(server_final)).is_empty());
        assert_eq!(state.phase, SaslPhase::Verified);
        assert_eq!(on_sasl_result(&mut state, true), vec!["CAP END"]);
        assert!(state.sasl_succeeded);
    }

    #[test]
    fn rejects_a_bad_scram_server_signature() {
        let p = profile(true, Some("pencil"), SaslMechanism::ScramSha256);
        let mut state = begin_sasl(&p, "sasl");
        on_authenticate(&p, &mut state, "+");
        state.phase = SaslPhase::AwaitingServerFinal;
        state.scram_expected_server_signature = vec![1, 2, 3];
        let final_message = STANDARD.encode(format!("v={}", STANDARD.encode([4, 5, 6])));
        assert_eq!(
            on_authenticate(&p, &mut state, &final_message),
            vec!["AUTHENTICATE *"]
        );
        assert_eq!(state.phase, SaslPhase::Failed);
    }

    #[test]
    fn cap_end_is_sent_once() {
        let mut state = AuthState::default();
        assert_eq!(on_sasl_result(&mut state, true), vec!["CAP END"]);
        assert!(on_sasl_result(&mut state, true).is_empty());
    }
}
