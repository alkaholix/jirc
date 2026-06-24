//! Tauri commands exposed to the frontend (the `invoke` surface).

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::config::ServerProfile;
use crate::irc::ConnectionManager;

/// Returns a human-readable version string for the backend core.
#[tauri::command]
pub fn core_version() -> String {
    format!("jIRC core {}", env!("CARGO_PKG_VERSION"))
}

/// The bundled help/scripting guide, embedded at build time.
const HELP_HTML: &str = include_str!("../../public/help.html");

/// Writes the help guide to disk and opens it in the user's default browser.
#[tauri::command]
pub fn open_help(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("help.html");
    std::fs::write(&path, HELP_HTML).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(path.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Opens a URL in the user's default browser (the `/url` command).
#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// ---- Detachable windows (pop-out / dock-back) ----
// Spawning/closing is done in Rust so the JS side doesn't need window-create or
// window-close permissions; the detached window stays live by listening to the
// app-wide `irc-event` broadcast.

/// Opens (or focuses, if it already exists) a detached OS window showing one
/// buffer. `label` is a unique window id; `route` is the in-app hash route
/// (e.g. `win=<serverId>|<bufferKey>`) the frontend reads to render single-window mode.
#[tauri::command]
pub fn open_detached_window(
    app: AppHandle,
    label: String,
    route: String,
    title: String,
) -> Result<(), String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }
    let url = WebviewUrl::App(format!("index.html#{route}").into());
    WebviewWindowBuilder::new(&app, &label, url)
        .title(title)
        .inner_size(640.0, 420.0)
        .min_inner_size(280.0, 160.0)
        .build()
        .map_err(|e| e.to_string())?;
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
    let target = if host.contains(':') { host } else { format!("{host}:0") };
    let addrs = tokio::net::lookup_host(target).await.map_err(|e| e.to_string())?;
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
    manager: State<'_, ConnectionManager>,
    server_id: String,
    quit_message: Option<String>,
) -> Result<(), String> {
    manager.disconnect(&server_id, quit_message)
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
