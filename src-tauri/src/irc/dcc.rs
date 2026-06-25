//! DCC (Direct Client-to-Client) — peer-to-peer **chat** and **file transfer**,
//! negotiated over CTCP exactly like mIRC. This module is the protocol core:
//! parsing and formatting the CTCP DCC handshake, and encoding the IP the way
//! DCC does (a 32-bit integer). The TCP I/O, transfer state, and UI build on top
//! of this (later phases).
//!
//! Handshake (carried in a `PRIVMSG` to the peer, wrapped in `\x01`):
//! - `DCC CHAT chat <ip> <port>` — open a direct chat.
//! - `DCC SEND <filename> <ip> <port> <size>` — offer a file.
//!
//! `<ip>` is the IPv4 address as a big-endian `u32` written in decimal, and the
//! **offerer listens** on `<port>` while the **receiver connects** to it.

use std::net::Ipv4Addr;

/// The kind of a DCC handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DccKind {
    Chat,
    Send,
}

/// A parsed incoming DCC offer (the `DCC …` text inside a CTCP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DccOffer {
    pub kind: DccKind,
    /// The offered filename for `SEND` (empty for `CHAT`).
    pub filename: String,
    pub ip: Ipv4Addr,
    pub port: u16,
    /// The file size in bytes for `SEND` (`0` when absent or for `CHAT`).
    pub size: u64,
}

/// Encodes an IPv4 address as the decimal 32-bit integer DCC uses.
// Part of the outgoing-offer API, wired in the DCC connect/send phase.
#[allow(dead_code)]
pub fn ip_to_dcc(ip: Ipv4Addr) -> u32 {
    u32::from(ip)
}

/// Decodes DCC's decimal 32-bit integer IP back into an address.
pub fn dcc_to_ip(n: u32) -> Ipv4Addr {
    Ipv4Addr::from(n)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s)
        .to_string()
}

/// Parses a CTCP DCC payload (the text between the `\x01` markers, already
/// stripped), e.g. `DCC CHAT chat 3232235521 1024` or
/// `DCC SEND "my file.txt" 3232235521 1024 5000`. Returns `None` if it isn't a
/// DCC offer we understand.
pub fn parse_dcc(payload: &str) -> Option<DccOffer> {
    let mut head = payload.split_whitespace();
    if !head.next()?.eq_ignore_ascii_case("DCC") {
        return None;
    }
    let kind = head.next()?;

    if kind.eq_ignore_ascii_case("CHAT") {
        let _proto = head.next()?; // the literal "chat"
        let ip = dcc_to_ip(head.next()?.parse::<u32>().ok()?);
        let port = head.next()?.parse::<u16>().ok()?;
        return Some(DccOffer {
            kind: DccKind::Chat,
            filename: String::new(),
            ip,
            port,
            size: 0,
        });
    }

    if kind.eq_ignore_ascii_case("SEND") {
        // After "DCC SEND ": `<filename> <ip> <port> [size]`. The filename may
        // contain spaces (then it's quoted), so split the trailing fields off
        // from the right. Try the modern 4-field form first, then the legacy
        // size-less 3-field form.
        let rest = payload.splitn(3, char::is_whitespace).nth(2)?.trim();
        let w: Vec<&str> = rest.rsplitn(4, char::is_whitespace).collect();
        if w.len() == 4 {
            if let (Ok(size), Ok(port), Ok(ipn)) = (
                w[0].parse::<u64>(),
                w[1].parse::<u16>(),
                w[2].parse::<u32>(),
            ) {
                return Some(DccOffer {
                    kind: DccKind::Send,
                    filename: unquote(w[3]),
                    ip: dcc_to_ip(ipn),
                    port,
                    size,
                });
            }
        }
        let w: Vec<&str> = rest.rsplitn(3, char::is_whitespace).collect();
        if w.len() == 3 {
            if let (Ok(port), Ok(ipn)) = (w[0].parse::<u16>(), w[1].parse::<u32>()) {
                return Some(DccOffer {
                    kind: DccKind::Send,
                    filename: unquote(w[2]),
                    ip: dcc_to_ip(ipn),
                    port,
                    size: 0,
                });
            }
        }
        return None;
    }

    None
}

/// Builds the CTCP payload for an outgoing DCC CHAT offer (caller wraps it in
/// `\x01` and a `PRIVMSG`).
#[allow(dead_code)] // wired in the DCC connect/send phase
pub fn format_chat_offer(ip: Ipv4Addr, port: u16) -> String {
    format!("DCC CHAT chat {} {}", ip_to_dcc(ip), port)
}

/// Builds the CTCP payload for an outgoing DCC SEND offer. Filenames containing
/// spaces are quoted.
#[allow(dead_code)] // wired in the DCC connect/send phase
pub fn format_send_offer(filename: &str, ip: Ipv4Addr, port: u16, size: u64) -> String {
    let name = if filename.contains(' ') {
        format!("\"{filename}\"")
    } else {
        filename.to_string()
    };
    format!("DCC SEND {} {} {} {}", name, ip_to_dcc(ip), port, size)
}

/// A parsed `/dcc` subcommand (the part after `/dcc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DccCommand {
    /// `/dcc chat <nick>` — offer/open a direct chat.
    Chat { nick: String },
    /// `/dcc send <nick> <file>` — offer a file.
    Send { nick: String, file: String },
    /// `/dcc get [nick]` — accept a pending incoming offer.
    Get { nick: Option<String> },
    /// `/dcc close [chat|send] [nick]` — close matching DCC session(s).
    Close {
        kind: Option<DccKind>,
        nick: Option<String>,
    },
}

/// Parses the arguments to `/dcc` (everything after the command word). Returns
/// `None` for an unknown/incomplete subcommand.
#[allow(dead_code)] // wired into the /dcc command + DCC manager next
pub fn parse_dcc_command(args: &str) -> Option<DccCommand> {
    let mut t = args.split_whitespace();
    match t.next()?.to_ascii_lowercase().as_str() {
        "chat" => Some(DccCommand::Chat {
            nick: t.next()?.to_string(),
        }),
        "send" => {
            let nick = t.next()?.to_string();
            let file = t.collect::<Vec<_>>().join(" ");
            (!file.is_empty()).then_some(DccCommand::Send { nick, file })
        }
        "get" | "accept" => Some(DccCommand::Get {
            nick: t.next().map(String::from),
        }),
        "close" => {
            let mut kind = None;
            let mut nick = None;
            for w in t {
                match w.to_ascii_lowercase().as_str() {
                    "chat" => kind = Some(DccKind::Chat),
                    "send" => kind = Some(DccKind::Send),
                    other => nick = Some(other.to_string()),
                }
            }
            Some(DccCommand::Close { kind, nick })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_round_trips_as_dcc_integer() {
        let ip = Ipv4Addr::new(192, 168, 0, 1);
        assert_eq!(ip_to_dcc(ip), 3232235521);
        assert_eq!(dcc_to_ip(3232235521), ip);
    }

    #[test]
    fn parses_dcc_chat() {
        let o = parse_dcc("DCC CHAT chat 3232235521 1024").unwrap();
        assert_eq!(o.kind, DccKind::Chat);
        assert_eq!(o.ip, Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(o.port, 1024);
    }

    #[test]
    fn parses_dcc_send_with_and_without_size_and_quotes() {
        let o = parse_dcc("DCC SEND readme.txt 3232235521 5000 12345").unwrap();
        assert_eq!(o.kind, DccKind::Send);
        assert_eq!(o.filename, "readme.txt");
        assert_eq!(o.port, 5000);
        assert_eq!(o.size, 12345);

        // Quoted filename with spaces.
        let o = parse_dcc("DCC SEND \"my long file.bin\" 3232235521 5000 99").unwrap();
        assert_eq!(o.filename, "my long file.bin");
        assert_eq!(o.size, 99);

        // Legacy, size-less.
        let o = parse_dcc("DCC SEND a.txt 16909060 6000").unwrap();
        assert_eq!(o.ip, Ipv4Addr::new(1, 2, 3, 4));
        assert_eq!(o.port, 6000);
        assert_eq!(o.size, 0);
    }

    #[test]
    fn rejects_non_dcc_and_garbage() {
        assert!(parse_dcc("VERSION").is_none());
        assert!(parse_dcc("DCC WAT something").is_none());
        assert!(parse_dcc("DCC CHAT chat notanip 1024").is_none());
    }

    #[test]
    fn formats_offers() {
        let ip = Ipv4Addr::new(192, 168, 0, 1);
        assert_eq!(format_chat_offer(ip, 1024), "DCC CHAT chat 3232235521 1024");
        assert_eq!(
            format_send_offer("file.txt", ip, 1024, 50),
            "DCC SEND file.txt 3232235521 1024 50"
        );
        assert_eq!(
            format_send_offer("a b.txt", ip, 1024, 50),
            "DCC SEND \"a b.txt\" 3232235521 1024 50"
        );
    }

    #[test]
    fn parses_dcc_commands() {
        assert_eq!(
            parse_dcc_command("chat bob"),
            Some(DccCommand::Chat { nick: "bob".into() })
        );
        assert_eq!(
            parse_dcc_command("send bob my file.txt"),
            Some(DccCommand::Send {
                nick: "bob".into(),
                file: "my file.txt".into()
            })
        );
        assert_eq!(parse_dcc_command("get"), Some(DccCommand::Get { nick: None }));
        assert_eq!(
            parse_dcc_command("close chat bob"),
            Some(DccCommand::Close {
                kind: Some(DccKind::Chat),
                nick: Some("bob".into())
            })
        );
        assert_eq!(parse_dcc_command("chat"), None); // missing nick
        assert_eq!(parse_dcc_command("wat"), None);
    }
}
