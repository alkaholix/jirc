//! Tauri commands exposed to the frontend (the `invoke` surface).

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::config::ServerProfile;
use crate::irc::ConnectionManager;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlPreview {
    url: String,
    final_url: String,
    domain: String,
    title: String,
    description: String,
    image: String,
}

const PREVIEW_HTML_LIMIT: usize = 512 * 1024;
const PREVIEW_IMAGE_LIMIT: usize = 3 * 1024 * 1024;

/// Fetches public HTTP(S) metadata without cookies, scripts, local-network
/// access, unrestricted redirects, or unbounded downloads.
#[tauri::command]
pub async fn url_preview(url: String, include_image: bool) -> Result<UrlPreview, String> {
    let original = reqwest::Url::parse(url.trim()).map_err(|_| "invalid URL".to_string())?;
    let (final_url, response) = preview_request(original.clone(), "text/html,*/*;q=0.1").await?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let direct_image_mime = preview_image_mime(&content_type);
    if let Some(mime) = direct_image_mime {
        let title = final_url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|value| !value.is_empty())
            .unwrap_or("Image")
            .to_string();
        let image = if include_image {
            preview_image_data(response, mime).await?
        } else {
            String::new()
        };
        return Ok(UrlPreview {
            url: original.to_string(),
            final_url: final_url.to_string(),
            domain: final_url.host_str().unwrap_or_default().to_string(),
            title,
            description: "Image".into(),
            image,
        });
    }
    if !content_type.contains("text/html") && !content_type.contains("application/xhtml") {
        return Err("URL is not an HTML page".into());
    }
    let html =
        String::from_utf8_lossy(&read_limited(response, PREVIEW_HTML_LIMIT).await?).into_owned();
    let title = meta_value(&html, &["og:title", "twitter:title"])
        .or_else(|| html_title(&html))
        .unwrap_or_else(|| final_url.host_str().unwrap_or("Link").to_string());
    let description = meta_value(
        &html,
        &["og:description", "twitter:description", "description"],
    )
    .unwrap_or_default();
    let image_url = meta_value(&html, &["og:image:secure_url", "og:image", "twitter:image"])
        .and_then(|value| final_url.join(&value).ok());
    let image = if include_image {
        match image_url {
            Some(image_url) => fetch_preview_image(image_url).await.unwrap_or_default(),
            None => String::new(),
        }
    } else {
        String::new()
    };
    Ok(UrlPreview {
        url: original.to_string(),
        final_url: final_url.to_string(),
        domain: final_url.host_str().unwrap_or_default().to_string(),
        title: truncate_preview_text(&decode_html(&title), 240),
        description: truncate_preview_text(&decode_html(&description), 500),
        image,
    })
}

async fn preview_request(
    mut url: reqwest::Url,
    accept: &str,
) -> Result<(reqwest::Url, reqwest::Response), String> {
    for _ in 0..=3 {
        let (host, address) = public_preview_address(&url).await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(8))
            .connect_timeout(std::time::Duration::from_secs(4))
            .resolve(&host, address)
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, accept)
            .header(reqwest::header::ACCEPT_LANGUAGE, "en,*;q=0.5")
            .header(reqwest::header::USER_AGENT, "jIRC-LinkPreview/1.0")
            .send()
            .await
            .map_err(|error| format!("preview request failed: {error}"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("redirect has no location")?;
            url = url.join(location).map_err(|_| "invalid redirect URL")?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("preview returned HTTP {}", response.status()));
        }
        return Ok((url, response));
    }
    Err("too many preview redirects".into())
}

async fn public_preview_address(
    url: &reqwest::Url,
) -> Result<(String, std::net::SocketAddr), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only HTTP and HTTPS URLs can be previewed".into());
    }
    let host = url
        .host_str()
        .ok_or("URL has no host")?
        .trim_end_matches('.')
        .to_string();
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("local addresses cannot be previewed".into());
    }
    let port = url.port_or_known_default().ok_or("URL has no port")?;
    let address = {
        let mut addresses = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| "could not resolve preview host")?;
        addresses
            .find(|address| is_public_ip(address.ip()))
            .ok_or("preview host does not resolve to a public address")?
    };
    Ok((host, address))
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0)
                || (a == 192 && b == 2)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51)
                || (a == 203 && b == 0)
                || a >= 224)
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(ipv4) = ip.to_ipv4_mapped() {
                return is_public_ip(std::net::IpAddr::V4(ipv4));
            }
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first & 0xffc0) == 0xfec0
                || (first & 0xff00) == 0xff00)
                && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8)
        }
    }
}

async fn read_limited(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("preview response is too large".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        if bytes.len() + chunk.len() > limit {
            return Err("preview response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn fetch_preview_image(url: reqwest::Url) -> Result<String, String> {
    let (_, response) = preview_request(url, "image/*").await?;
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mime = preview_image_mime(&mime).ok_or("preview image format is unsupported")?;
    preview_image_data(response, mime).await
}

fn preview_image_mime(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim();
    Some(match mime {
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => return None,
    })
}

async fn preview_image_data(response: reqwest::Response, mime: &str) -> Result<String, String> {
    use base64::Engine as _;
    let bytes = read_limited(response, PREVIEW_IMAGE_LIMIT).await?;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn meta_value(html: &str, names: &[&str]) -> Option<String> {
    static META: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?is)<meta\s+[^>]*>").unwrap());
    static ATTR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?is)([a-z_:.-]+)\s*=\s*(?:\"([^\"]*)\"|'([^']*)'|([^\s>]+))"#)
            .unwrap()
    });
    for tag in META.find_iter(html) {
        let mut key = String::new();
        let mut content = String::new();
        for capture in ATTR.captures_iter(tag.as_str()) {
            let name = capture.get(1).map_or("", |value| value.as_str());
            let value = capture
                .get(2)
                .or_else(|| capture.get(3))
                .or_else(|| capture.get(4))
                .map_or("", |value| value.as_str());
            if name.eq_ignore_ascii_case("property") || name.eq_ignore_ascii_case("name") {
                key = value.to_ascii_lowercase();
            } else if name.eq_ignore_ascii_case("content") {
                content = value.to_string();
            }
        }
        if names.iter().any(|name| key.eq_ignore_ascii_case(name)) && !content.trim().is_empty() {
            return Some(content.trim().to_string());
        }
    }
    None
}

fn html_title(html: &str) -> Option<String> {
    static TITLE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap());
    TITLE
        .captures(html)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str().trim().to_string())
}

fn decode_html(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_preview_text(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    format!(
        "{}…",
        value
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
    )
}

#[cfg(test)]
mod url_preview_tests {
    use super::*;

    #[test]
    fn preview_network_guard_rejects_private_and_special_addresses() {
        for address in [
            "127.0.0.1",
            "10.2.3.4",
            "172.20.1.1",
            "192.168.1.2",
            "169.254.2.3",
            "100.64.0.1",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "192.0.2.1",
            "198.51.100.2",
            "203.0.113.3",
            "2001:db8::1",
        ] {
            assert!(
                !is_public_ip(address.parse().unwrap()),
                "accepted {address}"
            );
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn open_graph_parser_handles_attribute_order_quotes_and_fallback_title() {
        let html = r#"
          <html><head>
          <meta content="A &amp; B" property="og:title">
          <meta name='description' content='A useful page'>
          <meta property=og:image content=/card.png>
          <title>Fallback</title>
          </head></html>
        "#;
        assert_eq!(meta_value(html, &["og:title"]), Some("A &amp; B".into()));
        assert_eq!(
            meta_value(html, &["description"]),
            Some("A useful page".into())
        );
        assert_eq!(meta_value(html, &["og:image"]), Some("/card.png".into()));
        assert_eq!(
            html_title("<TITLE> Plain title </TITLE>"),
            Some("Plain title".into())
        );
        assert_eq!(decode_html(" A &amp;  B "), "A & B");
    }

    #[test]
    fn preview_text_limits_are_unicode_safe() {
        assert_eq!(truncate_preview_text("hello", 10), "hello");
        assert_eq!(truncate_preview_text("😀😀😀😀", 3), "😀😀…");
    }

    #[tokio::test]
    #[ignore = "live internet preview smoke test"]
    async fn live_example_dot_com_preview() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let preview = url_preview("https://example.com/".into(), false)
            .await
            .expect("example.com preview");
        assert_eq!(preview.domain, "example.com");
        assert!(preview.title.to_ascii_lowercase().contains("example"));
        assert!(preview.image.is_empty());
    }
}

/// Returns a human-readable version string for the backend core.
#[tauri::command]
pub fn core_version() -> String {
    format!("jIRC core {}", env!("CARGO_PKG_VERSION"))
}

/// Lists installed font families through fontdb's native Windows, macOS, Linux,
/// and BSD system-font discovery.
#[tauri::command]
pub async fn system_fonts() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let families: Vec<String> = database
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        normalize_font_families(families)
    })
    .await
    .map_err(|error| error.to_string())
}

fn normalize_font_families(mut families: Vec<String>) -> Vec<String> {
    families.sort_by(|left, right| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right))
    });
    families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    families
}

#[cfg(test)]
mod font_tests {
    use super::normalize_font_families;

    #[test]
    fn font_families_are_sorted_and_deduplicated_case_insensitively() {
        assert_eq!(
            normalize_font_families(vec![
                "Verdana".into(),
                "arial".into(),
                "Arial".into(),
                "Consolas".into(),
            ]),
            vec!["Arial", "Consolas", "Verdana"]
        );
    }
}

/// The bundled help/scripting guide, embedded at build time.
const HELP_HTML: &str = include_str!("../../public/help.html");

/// Writes the help guide to disk and opens it in the user's default browser.
#[tauri::command]
pub fn open_help(app: AppHandle, keyword: Option<String>) -> Result<(), String> {
    let dir = crate::storage::config_dir(&app)?;
    let path = dir.join("help.html");
    std::fs::write(&path, HELP_HTML).map_err(|e| e.to_string())?;
    let mut url = tauri::Url::from_file_path(&path).map_err(|_| "invalid help path".to_string())?;
    if let Some(keyword) = keyword.filter(|value| !value.trim().is_empty()) {
        let anchor = keyword
            .trim()
            .trim_start_matches(['/', '$'])
            .to_ascii_lowercase()
            .replace(' ', "-");
        url.set_fragment(Some(&anchor));
    }
    app.opener()
        .open_url(url.to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Opens a URL in the user's default browser (the `/url` command).
#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

// ---- Detachable windows (pop-out / dock-back) ----
// Spawning/closing is done in Rust so the JS side doesn't need window-create or
// window-close permissions; the detached window stays live by listening to the
// app-wide `irc-event` broadcast.

/// Opens (or focuses, if it already exists) a detached OS window showing one
/// buffer. `label` is a unique window id; the frontend identifies which buffer to
/// render from this same label (mapped to a buffer key in shared localStorage).
///
/// The URL deliberately carries **no `#route` fragment**: in a release build a
/// fragment in `WebviewUrl::App` is treated as part of the asset path and 404s to
/// a blank ("white box") window. So we load `index.html` cleanly and route by label.
///
/// This command is **async on purpose.** A *synchronous* Tauri command runs on the
/// main (event-loop) thread, and calling `WebviewWindowBuilder::build()` there
/// blocks the loop that WebView2 needs to finish initializing — the native frame
/// appears but the webview never loads its page (a blank, unresponsive window).
/// Running async moves the blocking build off the main thread so the webview can
/// initialize.
#[tauri::command]
pub async fn open_detached_window(
    app: AppHandle,
    label: String,
    title: String,
) -> Result<(), String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(640.0, 420.0)
        .min_inner_size(280.0, 160.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Opens or focuses the dedicated, freely resizable script editor window.
#[tauri::command]
pub async fn open_script_editor(app: AppHandle) -> Result<(), String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
    const LABEL: &str = "script-editor";
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::App("index.html".into()))
        .title("jIRC Script Editor")
        .inner_size(1050.0, 760.0)
        .min_inner_size(600.0, 400.0)
        .resizable(true)
        .build()
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Focuses an existing detached window (clicking its popped-out switchbar entry).
#[tauri::command]
pub fn focus_window(app: AppHandle, label: String) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
    }
}

/// Docks a detached window back into jIRC: broadcasts `win-dock` (the main window
/// re-shows the buffer) and closes the detached OS window.
#[tauri::command]
pub fn dock_window(app: AppHandle, label: String, buffer_key: String) {
    use tauri::{Emitter, Manager};
    let _ = app.emit("win-dock", buffer_key);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.close();
    }
}

/// Closes a detached window *and* its buffer (the native ✕ behaviour, distinct from
/// dock-back): broadcasts `win-close-buffer` (the main window closes the buffer) and
/// closes the detached OS window.
#[tauri::command]
pub fn close_detached(app: AppHandle, label: String, buffer_key: String) {
    use tauri::{Emitter, Manager};
    let _ = app.emit("win-close-buffer", buffer_key);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.close();
    }
}

/// Quits the application (the `/exit` command).
#[tauri::command]
pub fn exit_app(app: AppHandle) {
    app.exit(0);
}

/// Resolves a hostname to its IP address(es) — forward DNS only (host -> IPs; an
/// IP passed in resolves to itself). Used by the `/dns` command.
#[tauri::command]
pub async fn dns_lookup(host: String) -> Result<Vec<String>, String> {
    resolve_host(&host).await
}

pub(crate) async fn resolve_host(host: &str) -> Result<Vec<String>, String> {
    let target = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:0")
    };
    let addrs = tokio::net::lookup_host(target)
        .await
        .map_err(|e| e.to_string())?;
    let mut ips: Vec<String> = Vec::new();
    for addr in addrs {
        let ip = addr.ip().to_string();
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    Ok(ips)
}

/// Opens a connection. Returns the server id used for subsequent calls/events.
#[tauri::command]
pub fn irc_connect(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    profile: ServerProfile,
) -> Result<String, String> {
    manager.connect(app, profile)
}

/// Closes a connection, optionally with a quit message.
#[tauri::command]
pub fn irc_disconnect(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    server_id: String,
    quit_message: Option<String>,
) -> Result<(), String> {
    manager.disconnect(&server_id, quit_message)?;
    if let Some(store) = app.try_state::<crate::irc::state::StateStore>() {
        store.remove(&server_id);
    }
    if let Some(engine) = app.try_state::<crate::script::ScriptEngine>() {
        engine.forget_cid(&server_id);
    }
    if let Some(timers) = app.try_state::<crate::script::timer::TimerManager>() {
        timers.session_dropped(&app, &server_id);
    }
    Ok(())
}

/// Sends a raw protocol line on a connection.
#[tauri::command]
pub fn irc_send_raw(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    line: String,
) -> Result<(), String> {
    manager.send(&server_id, line)
}

/// Sends an IRCv3 `+typing` notification for `target`.
///
/// A no-op unless the server negotiated `message-tags`: without it a TAGMSG
/// carrying a client tag is not merely ignored, it is a protocol error, and
/// some servers disconnect for it. The check lives here rather than in the UI
/// so the frontend never has to reason about capability state.
#[tauri::command]
pub fn irc_send_typing(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    server_id: String,
    target: String,
    state: String,
) -> Result<(), String> {
    let Some(store) = app.try_state::<crate::irc::state::StateStore>() else {
        return Ok(());
    };
    if !store
        .get(&server_id)
        .caps
        .iter()
        .any(|c| c == "message-tags")
    {
        return Ok(());
    }
    let state = match state.as_str() {
        "active" | "paused" | "done" => state,
        _ => return Err(format!("invalid typing state: {state}")),
    };
    manager.send(&server_id, format!("@+typing={state} TAGMSG {target}"))
}

/// Sends a PRIVMSG to a target (channel or nick).
#[tauri::command]
pub fn irc_send_message(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    target: String,
    text: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("PRIVMSG {target} :{text}"))
}

/// Joins a channel.
#[tauri::command]
pub fn irc_join(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    channel: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("JOIN {channel}"))
}

/// Parts a channel, optionally with a reason.
#[tauri::command]
pub fn irc_part(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    channel: String,
    reason: Option<String>,
) -> Result<(), String> {
    let line = match reason {
        Some(r) if !r.is_empty() => format!("PART {channel} :{r}"),
        _ => format!("PART {channel}"),
    };
    manager.send(&server_id, line)
}

/// Requests WHOIS information for a nick.
#[tauri::command]
pub fn irc_whois(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    nick: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("WHOIS {nick}"))
}

/// Changes the current nick.
#[tauri::command]
pub fn irc_set_nick(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    nick: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("NICK {nick}"))
}

/// Lists currently active connection ids.
#[tauri::command]
pub fn irc_list_connections(manager: State<'_, ConnectionManager>) -> Vec<String> {
    manager.list()
}

// ---- IRCX (Phase 1b) ----

/// Enables IRCX mode on the connection (`IRCX`), or queries it (`ISIRCX`).
#[tauri::command]
pub fn ircx_enable(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    query_only: bool,
) -> Result<(), String> {
    let cmd = if query_only { "ISIRCX" } else { "IRCX" };
    manager.send(&server_id, cmd.to_string())
}

/// Sends a whisper (channel-scoped private message) to one or more targets.
#[tauri::command]
pub fn ircx_whisper(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    channel: String,
    targets: String,
    text: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("WHISPER {channel} {targets} :{text}"))
}

/// Manages an object's access list, e.g. action="ADD"/"DELETE"/"LIST"/"CLEAR".
#[tauri::command]
pub fn ircx_access(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    object: String,
    action: String,
    level: Option<String>,
    mask: Option<String>,
) -> Result<(), String> {
    let mut line = format!("ACCESS {object} {action}");
    if let Some(l) = level {
        line.push(' ');
        line.push_str(&l);
    }
    if let Some(m) = mask {
        line.push(' ');
        line.push_str(&m);
    }
    manager.send(&server_id, line)
}

/// Reads object properties. `property` defaults to `*` (all properties).
#[tauri::command]
pub fn ircx_prop_get(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    object: String,
    property: Option<String>,
) -> Result<(), String> {
    let property = property.unwrap_or_else(|| "*".to_string());
    manager.send(&server_id, format!("PROP {object} {property}"))
}

/// Sets an object property.
#[tauri::command]
pub fn ircx_prop_set(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    object: String,
    property: String,
    value: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("PROP {object} {property} :{value}"))
}

/// Creates a channel (optionally with inline mode/key arguments).
#[tauri::command]
pub fn ircx_create(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    channel: String,
    args: Option<String>,
) -> Result<(), String> {
    let line = match args {
        Some(a) if !a.is_empty() => format!("CREATE {channel} {a}"),
        _ => format!("CREATE {channel}"),
    };
    manager.send(&server_id, line)
}

/// Extended channel listing with an optional filter mask.
#[tauri::command]
pub fn ircx_listx(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    mask: Option<String>,
) -> Result<(), String> {
    let line = match mask {
        Some(m) if !m.is_empty() => format!("LISTX {m}"),
        _ => "LISTX".to_string(),
    };
    manager.send(&server_id, line)
}

/// Requests entry to a channel (KNOCK).
#[tauri::command]
pub fn ircx_knock(
    manager: State<'_, ConnectionManager>,
    server_id: String,
    channel: String,
) -> Result<(), String> {
    manager.send(&server_id, format!("KNOCK {channel}"))
}

// ---- DCC ----

/// `/dcc chat <nick>` — offer a direct chat to `nick` (we listen for them).
#[tauri::command]
pub fn dcc_chat(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    nick: String,
) -> Result<(), String> {
    dcc.chat(app.clone(), server_id, nick)
}

/// Accepts an incoming DCC chat offer by connecting to its `ip:port`.
#[tauri::command]
pub fn dcc_accept(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    nick: String,
    ip: String,
    port: u16,
    token: Option<u64>,
) -> Result<(), String> {
    let addr: std::net::IpAddr = ip.parse().map_err(|_| "invalid DCC IP".to_string())?;
    dcc.accept(app.clone(), server_id, nick, addr, port, token)
}

/// Sends a typed line to a DCC chat peer.
#[tauri::command]
pub fn dcc_send(
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    id: String,
    text: String,
) -> Result<(), String> {
    dcc.send(&server_id, &id, text)
}

/// Closes a DCC chat session.
#[tauri::command]
pub fn dcc_close(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    id: String,
) {
    dcc.close(&app, &server_id, &id);
}

/// Accepts an incoming DCC SEND offer and downloads the file into the `dcc/` folder.
#[tauri::command]
pub fn dcc_recv(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    nick: String,
    filename: String,
    ip: String,
    port: u16,
    size: u64,
    token: Option<u64>,
    resume: Option<bool>,
) -> Result<(), String> {
    let addr: std::net::IpAddr = ip.parse().map_err(|_| "invalid DCC IP".to_string())?;
    dcc.recv_file(
        app.clone(),
        server_id,
        nick,
        filename,
        addr,
        port,
        size,
        token,
        resume.unwrap_or(false),
    )
}

/// `/dcc send <nick> <file>` — offer and stream a local file to `nick`.
#[tauri::command]
pub fn dcc_send_file(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    nick: String,
    path: String,
) -> Result<(), String> {
    dcc.send_file(app.clone(), server_id, nick, std::path::PathBuf::from(path))
}

/// Sets the DCC IP to advertise and the listen-port range (for transfers across NAT).
#[tauri::command]
pub fn dcc_configure(
    dcc: State<'_, crate::irc::dcc::DccManager>,
    ip: String,
    bind_ip: String,
    port_from: u16,
    port_to: u16,
    passive: bool,
) {
    dcc.configure(ip, bind_ip, port_from, port_to, passive);
}

/// Starts/stops the direct DCC Server protocol listener.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn dcc_server_configure(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    server_id: String,
    enabled: bool,
    port: u16,
    chat: bool,
    send: bool,
    fserve: bool,
) -> Result<(), String> {
    if server_id.is_empty() && enabled {
        return Err("connect to an IRC server before enabling DCC Server".into());
    }
    dcc.configure_server(app, server_id, enabled, port, chat, send, fserve)
}

/// Cancels an active/waiting DCC transfer.
#[tauri::command]
pub fn dcc_cancel_transfer(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    id: String,
) -> Result<(), String> {
    dcc.cancel_transfer(&app, &id)
}

/// Re-attempts a failed/cancelled transfer (receives negotiate a resume).
#[tauri::command]
pub fn dcc_retry_transfer(
    app: AppHandle,
    dcc: State<'_, crate::irc::dcc::DccManager>,
    id: String,
) -> Result<(), String> {
    dcc.retry_transfer(app, &id)
}

/// A routable local IP for DCC (a global IPv6 if available), for the "Detect"
/// button. Empty when there's no global IPv6.
#[tauri::command]
pub fn dcc_local_ip() -> String {
    crate::irc::dcc::detect_local_ip()
}

/// Updates outbound user/script line throttling for all connections.
#[tauri::command]
pub fn irc_configure_flood(
    manager: State<'_, ConnectionManager>,
    enabled: bool,
    messages: usize,
    seconds: u64,
) {
    manager.configure_flood(enabled, messages, seconds);
}
