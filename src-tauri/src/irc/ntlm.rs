//! IRCX NTLM (SSPI) authentication — drives the `AUTH NTLM` handshake that IRC7
//! directory and chat servers require *before* registration (NICK/USER).
//!
//! The handshake is a stateful, multi-round exchange, so it runs as a dedicated
//! step before the main read loop (see [`crate::irc::connection`]), not through
//! the pure `process_message` path. Tokens are NTLMSSP messages carried as
//! MSN-backslash-escaped binary on the wire — byte-for-byte identical to the
//! server's `ToEscape`/`ToLiteral` (Irc.Helpers/StringExtensions.cs). All I/O
//! here is byte-level (Latin-1), never UTF-8, so high bytes in the Type 2
//! challenge survive the round trip intact.
//!
//! Wire exchange (see Irc/Commands/Auth.cs):
//! ```text
//! C -> S: AUTH NTLM I :<escaped Type 1>
//! S -> C: AUTH NTLM S :<escaped Type 2 challenge>
//! C -> S: AUTH NTLM S :<escaped Type 3 response>
//! S -> C: AUTH NTLM * <user>@<domain> 0      (success)   | 910/912 (failure)
//! ```

use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::ServerProfile;
use crate::irc::event::{Direction, UiEvent, IRC_EVENT};
use crate::irc::ircx_sspi::NtlmSession;

/// Runs the `AUTH NTLM` handshake to completion on an already-connected stream,
/// before NICK/USER. Returns `Ok` once the server confirms (`AUTH NTLM * …`),
/// or an error describing why it failed (which aborts the connection attempt).
///
/// `reader`/`writer` are the split halves of the live connection; the same
/// `reader` is reused by the main read loop afterwards (no buffered bytes lost).
pub async fn handshake<R, W>(
    reader: &mut R,
    writer: &mut W,
    profile: &ServerProfile,
    app: &AppHandle,
    server_id: &str,
) -> Result<(), String>
where
    R: AsyncBufReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut client = NtlmSession::new(
        profile.ntlm_domain(),
        profile.ntlm_user(),
        profile.ntlm_password.as_deref().unwrap_or(""),
    )?;

    // Type 1 (negotiate).
    let type1 = client.negotiate()?;
    echo(app, server_id, &format!("NTLM: sending negotiate (Type 1, {} bytes)…", type1.len()));
    send_token(writer, app, server_id, b"AUTH NTLM I :", &escape(&type1)).await?;

    // Await the Type 2 challenge (handling PING and any pre-auth notices meanwhile).
    let challenge = loop {
        let line = read_line(reader)
            .await
            .map_err(|e| format!("read during NTLM auth: {e}"))?
            .ok_or("connection closed during NTLM auth")?;
        emit_raw(app, server_id, Direction::In, &line);
        if handle_ping(writer, app, server_id, &line).await? {
            continue;
        }
        match parse_auth(&line) {
            Some((b'S', payload)) => break unescape(&payload),
            Some((b'*', _)) => {
                return Err("server reported success before the NTLM response was sent".into())
            }
            _ => {}
        }
        if is_numeric(&line, b"910") {
            return Err("NTLM authentication failed (910)".into());
        }
        if is_numeric(&line, b"912") {
            return Err("server rejected the NTLM package (912 unknown package)".into());
        }
    };

    // Type 3 (authenticate).
    let type3 = client.authenticate(&challenge)?;
    echo(app, server_id, &format!("NTLM: sending response (Type 3, {} bytes)…", type3.len()));
    send_token(writer, app, server_id, b"AUTH NTLM S :", &escape(&type3)).await?;

    // Await the success marker (`AUTH NTLM * …`).
    loop {
        let line = read_line(reader)
            .await
            .map_err(|e| format!("read during NTLM auth: {e}"))?
            .ok_or("connection closed during NTLM auth")?;
        emit_raw(app, server_id, Direction::In, &line);
        if handle_ping(writer, app, server_id, &line).await? {
            continue;
        }
        if let Some((b'*', _)) = parse_auth(&line) {
            echo(app, server_id, "NTLM authentication succeeded.");
            return Ok(());
        }
        if is_numeric(&line, b"910") {
            return Err(
                "NTLM authentication failed (910) — check the username, domain and password".into(),
            );
        }
    }
}

// --- wire I/O helpers (byte-level, Latin-1) ---

/// Reads one CRLF-terminated line as raw bytes (CRLF stripped), skipping blank
/// lines. Returns `None` at EOF.
async fn read_line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        let mut buf = Vec::new();
        if reader.read_until(b'\n', &mut buf).await? == 0 {
            return Ok(None);
        }
        while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
            buf.pop();
        }
        if !buf.is_empty() {
            return Ok(Some(buf));
        }
    }
}

/// Writes `prefix` + `body` + CRLF as raw bytes (never UTF-8 re-encoded, so the
/// escaped binary token survives) and mirrors the line to the raw console.
async fn send_token<W: AsyncWrite + Unpin>(
    writer: &mut W,
    app: &AppHandle,
    server_id: &str,
    prefix: &[u8],
    body: &[u8],
) -> Result<(), String> {
    let mut line = Vec::with_capacity(prefix.len() + body.len());
    line.extend_from_slice(prefix);
    line.extend_from_slice(body);
    let err = |e: std::io::Error| format!("write during NTLM auth: {e}");
    writer.write_all(&line).await.map_err(err)?;
    writer.write_all(b"\r\n").await.map_err(err)?;
    writer.flush().await.map_err(err)?;
    emit_raw(app, server_id, Direction::Out, &line);
    Ok(())
}

/// Answers a PING with the matching PONG. Returns true if the line was a PING.
async fn handle_ping<W: AsyncWrite + Unpin>(
    writer: &mut W,
    app: &AppHandle,
    server_id: &str,
    line: &[u8],
) -> Result<bool, String> {
    if !line.starts_with(b"PING") {
        return Ok(false);
    }
    let mut resp = b"PONG".to_vec();
    resp.extend_from_slice(&line[4..]);
    let err = |e: std::io::Error| format!("write during NTLM auth: {e}");
    writer.write_all(&resp).await.map_err(err)?;
    writer.write_all(b"\r\n").await.map_err(err)?;
    writer.flush().await.map_err(err)?;
    emit_raw(app, server_id, Direction::Out, &resp);
    Ok(true)
}

// --- event emission ---

fn emit_raw(app: &AppHandle, server_id: &str, direction: Direction, line: &[u8]) {
    let _ = app.emit(
        IRC_EVENT,
        UiEvent::Raw {
            server_id: server_id.to_string(),
            direction,
            line: decode(line),
        },
    );
}

fn echo(app: &AppHandle, server_id: &str, text: &str) {
    let _ = app.emit(
        IRC_EVENT,
        UiEvent::Echo {
            server_id: server_id.to_string(),
            target: "(status)".to_string(),
            text: text.to_string(),
        },
    );
}

/// Decodes a raw line for display: UTF-8 if valid, else Latin-1.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

// --- pure protocol helpers (MSN escaping + AUTH parsing) ---

/// IRCX/MSN backslash escaping — matches the server's `ToEscape`.
pub fn escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x00 => out.extend_from_slice(b"\\0"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x0A => out.extend_from_slice(b"\\n"),
            0x0D => out.extend_from_slice(b"\\r"),
            0x20 => out.extend_from_slice(b"\\b"),
            0x2C => out.extend_from_slice(b"\\c"),
            0x5C => out.extend_from_slice(b"\\\\"),
            _ => out.push(b),
        }
    }
    out
}

/// Inverse of [`escape`] — matches the server's `ToLiteral`.
pub fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let (byte, skip) = match bytes[i + 1] {
                b'0' => (0x00, 2),
                b't' => (0x09, 2),
                b'n' => (0x0A, 2),
                b'r' => (0x0D, 2),
                b'b' => (0x20, 2),
                b'c' => (0x2C, 2),
                b'\\' => (0x5C, 2),
                _ => (b'\\', 1), // unknown escape: keep the backslash, re-read next byte
            };
            out.push(byte);
            i += skip;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Parses `AUTH <pkg> <stage> [:payload]`, tolerating an optional `:prefix `.
/// Returns the stage byte (`I`/`S`/`*`) and the (still-escaped) payload bytes.
fn parse_auth(line: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut l = line;
    if l.first() == Some(&b':') {
        let sp = l.iter().position(|&b| b == b' ')?;
        l = &l[sp + 1..];
    }
    let rest = l.strip_prefix(b"AUTH ")?;
    let sp = rest.iter().position(|&b| b == b' ')?; // skip the package name
    let rest = &rest[sp + 1..];
    let stage = *rest.first()?;
    let payload = match rest.iter().position(|&b| b == b':') {
        Some(i) => rest[i + 1..].to_vec(),
        None => Vec::new(),
    };
    Some((stage, payload))
}

/// True if `line`'s command (after an optional `:prefix`) is the numeric `code`.
fn is_numeric(line: &[u8], code: &[u8]) -> bool {
    let mut it = line.split(|&b| b == b' ');
    let first = it.next();
    let token = match first {
        Some(t) if t.first() == Some(&b':') => it.next(),
        other => other,
    };
    token == Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip_matches_server_rules() {
        // The seven special bytes, plus high bytes that must pass through literally
        // (Latin-1) — the server's Encoding.Latin1 preserves these.
        let raw: Vec<u8> = vec![0x00, 0x09, 0x0A, 0x0D, 0x20, 0x2C, 0x5C, 0x41, 0xA2, 0xFF];
        let esc = escape(&raw);
        assert_eq!(esc, b"\\0\\t\\n\\r\\b\\c\\\\A\xA2\xFF");
        assert_eq!(unescape(&esc), raw);
    }

    #[test]
    fn unescape_handles_trailing_and_unknown_backslash() {
        assert_eq!(unescape(b"abc\\"), b"abc\\"); // dangling backslash kept
        assert_eq!(unescape(b"a\\zb"), b"a\\zb"); // unknown escape kept verbatim
    }

    #[test]
    fn parse_auth_challenge_and_success() {
        assert_eq!(
            parse_auth(b"AUTH NTLM S :tok\\0en"),
            Some((b'S', b"tok\\0en".to_vec()))
        );
        // The success marker carries no `:payload` (AUTH <pkg> * <addr> <oid>).
        assert_eq!(parse_auth(b"AUTH NTLM * user@cg 0"), Some((b'*', Vec::new())));
        // An optional leading :prefix is tolerated.
        assert_eq!(parse_auth(b":srv AUTH NTLM S :x"), Some((b'S', b"x".to_vec())));
        assert_eq!(parse_auth(b"PING :tok"), None);
    }

    #[test]
    fn numeric_detection() {
        assert!(is_numeric(b":server 910 nick NTLM :Authentication failed", b"910"));
        assert!(is_numeric(b"910 nick", b"910"));
        assert!(!is_numeric(b":server 001 nick :Welcome", b"910"));
    }
}
