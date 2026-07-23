//! Native browser windows for scripts (`/webview`, `$webview`, `on WEBVIEW`).
//!
//! This is a safe replacement for the narrow browser work historically done by
//! helper DLLs: scripts can navigate normal web URLs and request cookies for one
//! URL, but cannot load native code or choose arbitrary profile directories.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use super::eval::{EventVars, ScriptWebviews, WebviewInfo};
use super::{apply_actions, script_data_dir, RunCtx, ScriptEngine};

#[derive(Clone)]
struct Owner {
    server_id: String,
    nick: String,
    network: String,
    server: String,
}

#[derive(Clone)]
struct Entry {
    name: String,
    label: String,
    profile: String,
    status: String,
    url: String,
    events: EventSink,
}

/// Serialises callbacks for one browser. Page-load callbacks can arrive on the
/// WebView thread and cookie reads run on a worker; one queue preserves their
/// observable order and keeps blocking mSL handlers off the WebView thread.
#[derive(Clone)]
struct EventSink {
    tx: std::sync::mpsc::Sender<(String, Vec<String>)>,
}

impl EventSink {
    fn new(app: AppHandle, owner: Owner, name: String) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel::<(String, Vec<String>)>();
        std::thread::Builder::new()
            .name("jirc-script-webview".to_string())
            .spawn(move || {
                while let Ok((event, args)) = rx.recv() {
                    fire(&app, &owner, &name, &event, args);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { tx })
    }

    fn emit(&self, event: &str, args: Vec<String>) {
        let _ = self.tx.send((event.to_string(), args));
    }
}

/// Application-wide registry of native browser windows opened by scripts.
pub struct WebviewManager {
    entries: Mutex<HashMap<String, Entry>>,
}

impl Default for WebviewManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WebviewManager {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        app: AppHandle,
        server_id: &str,
        nick: &str,
        network: &str,
        server: &str,
        name: String,
        profile: String,
        width: u32,
        height: u32,
        url: String,
        title: String,
    ) -> Result<(), String> {
        validate_name(&name)?;
        if profile.is_empty() || profile.len() > 128 || profile.chars().any(char::is_control) {
            return Err("profile must be 1-128 printable characters".to_string());
        }
        let parsed = parse_web_url(&url, true)?;
        let key = key(server_id, &name);
        {
            let entries = self.entries.lock().unwrap();
            if entries
                .get(&key)
                .is_some_and(|entry| entry.status != "closing")
            {
                return Err(format!("{name} is already open"));
            }
        }

        let data_root = crate::storage::config_dir(&app)?;
        let data_dir = data_root.join("webviews").join(profile_dir_name(&profile));
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        let label = format!("script-webview-{}", uuid::Uuid::new_v4().simple());
        let owner = Owner {
            server_id: server_id.to_string(),
            nick: nick.to_string(),
            network: network.to_string(),
            server: server.to_string(),
        };
        let events = EventSink::new(app.clone(), owner.clone(), name.clone())?;
        let mut entries = self.entries.lock().unwrap();
        if entries
            .get(&key)
            .is_some_and(|entry| entry.status != "closing")
        {
            return Err(format!("{name} is already open"));
        }
        entries.insert(
            key.clone(),
            Entry {
                name: name.clone(),
                label: label.clone(),
                profile: profile.clone(),
                status: "opening".to_string(),
                url: url.clone(),
                events: events.clone(),
            },
        );
        drop(entries);

        let open_app = app.clone();
        tauri::async_runtime::spawn(async move {
            let page_app = open_app.clone();
            let page_key = key.clone();
            let page_label = label.clone();
            let page_events = events.clone();
            let builder =
                WebviewWindowBuilder::new(&open_app, &label, WebviewUrl::External(parsed))
                    .title(if title.is_empty() { &name } else { &title })
                    .inner_size(
                        width.clamp(320, 3840) as f64,
                        height.clamp(240, 2160) as f64,
                    )
                    .min_inner_size(320.0, 240.0)
                    .data_directory(data_dir)
                    .data_store_identifier(profile_identifier(&profile))
                    .enable_clipboard_access()
                    .on_navigation(|url| {
                        matches!(url.scheme(), "http" | "https") || url.as_str() == "about:blank"
                    })
                    .on_page_load(move |_window, payload| {
                        let url = payload.url().to_string();
                        let (status, event) = match payload.event() {
                            PageLoadEvent::Started => ("navigating", "navigate_start"),
                            PageLoadEvent::Finished => ("ready", "navigate_complete"),
                        };
                        if let Some(manager) = page_app.try_state::<WebviewManager>() {
                            manager.update_if_label(&page_key, &page_label, status, Some(&url));
                        }
                        page_events.emit(event, vec![url]);
                    });

            match builder.build() {
                Ok(window) => {
                    let close_app = open_app.clone();
                    let close_key = key.clone();
                    let close_label = label.clone();
                    let close_events = events.clone();
                    window.on_window_event(move |event| {
                        if matches!(event, WindowEvent::Destroyed) {
                            let notify =
                                close_app
                                    .try_state::<WebviewManager>()
                                    .is_some_and(|manager| {
                                        manager.handle_destroyed(&close_key, &close_label)
                                    });
                            if notify {
                                close_events.emit("closed", Vec::new());
                            }
                        }
                    });
                    let current = open_app
                        .try_state::<WebviewManager>()
                        .is_some_and(|manager| manager.is_active_label(&key, &label));
                    if !current {
                        let _ = window.close();
                        return;
                    }
                    if let Some(manager) = open_app.try_state::<WebviewManager>() {
                        manager.update_if_label(&key, &label, "ready", None);
                    }
                    let _ = window.set_focus();
                    events.emit("opened", Vec::new());
                }
                Err(error) => {
                    if let Some(manager) = open_app.try_state::<WebviewManager>() {
                        manager.remove_if_label(&key, &label);
                    }
                    events.emit("error", vec![error.to_string()]);
                }
            }
        });
        Ok(())
    }

    pub fn navigate(
        &self,
        app: AppHandle,
        server_id: &str,
        name: &str,
        url: String,
    ) -> Result<(), String> {
        let parsed = parse_web_url(&url, true)?;
        let key = key(server_id, name);
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("{name} is not open"))?;
        self.update_if_label(&key, &entry.label, "navigating", None);
        tauri::async_runtime::spawn(async move {
            let result = app
                .get_webview_window(&entry.label)
                .ok_or_else(|| "native browser window is unavailable".to_string())
                .and_then(|window| window.navigate(parsed).map_err(|error| error.to_string()));
            if let Err(error) = result {
                if let Some(manager) = app.try_state::<WebviewManager>() {
                    manager.update_if_label(&key, &entry.label, "error", None);
                }
                entry.events.emit("error", vec![error]);
            }
        });
        Ok(())
    }

    pub fn cookies(
        &self,
        app: AppHandle,
        server_id: &str,
        name: &str,
        url: String,
    ) -> Result<(), String> {
        let parsed = parse_web_url(&url, false)?;
        let key = key(server_id, name);
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("{name} is not open"))?;
        self.update_if_label(&key, &entry.label, "cookies", None);

        // WebView2 cookie reads can deadlock in synchronous commands and event
        // handlers. Always perform the native call on a worker thread, then run
        // script callbacks sequentially there so `cookies_done` stays last.
        tauri::async_runtime::spawn_blocking(move || {
            let result = app
                .get_webview_window(&entry.label)
                .ok_or_else(|| "native browser window is unavailable".to_string())
                .and_then(|window| {
                    window
                        .cookies_for_url(parsed)
                        .map_err(|error| error.to_string())
                });
            match result {
                Ok(mut cookies) => {
                    cookies.sort_by(|a, b| a.name().cmp(b.name()));
                    for cookie in cookies {
                        entry.events.emit(
                            "cookie",
                            vec![cookie.name().to_string(), cookie.value().to_string()],
                        );
                    }
                    if let Some(manager) = app.try_state::<WebviewManager>() {
                        manager.update_if_label(&key, &entry.label, "ready", None);
                    }
                    entry.events.emit("cookies_done", Vec::new());
                }
                Err(error) => {
                    if let Some(manager) = app.try_state::<WebviewManager>() {
                        manager.update_if_label(&key, &entry.label, "error", None);
                    }
                    entry.events.emit("error", vec![error]);
                }
            }
        });
        Ok(())
    }

    pub fn focus(&self, app: AppHandle, server_id: &str, name: &str) -> Result<(), String> {
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&key(server_id, name))
            .cloned()
            .ok_or_else(|| format!("{name} is not open"))?;
        tauri::async_runtime::spawn(async move {
            if let Some(window) = app.get_webview_window(&entry.label) {
                let _ = window.show();
                let _ = window.set_focus();
            }
        });
        Ok(())
    }

    pub fn close(&self, app: AppHandle, server_id: &str, name: &str) -> Result<(), String> {
        let key = key(server_id, name);
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("{name} is not open"))?;
        self.update_if_label(&key, &entry.label, "closing", None);
        tauri::async_runtime::spawn(async move {
            match app.get_webview_window(&entry.label) {
                Some(window) => {
                    if let Err(error) = window.close() {
                        if let Some(manager) = app.try_state::<WebviewManager>() {
                            manager.update_if_label(&key, &entry.label, "error", None);
                        }
                        entry.events.emit("error", vec![error.to_string()]);
                    }
                }
                None => {
                    let removed = app
                        .try_state::<WebviewManager>()
                        .and_then(|manager| manager.remove_if_label(&key, &entry.label))
                        .is_some();
                    if removed {
                        entry.events.emit("closed", Vec::new());
                    }
                }
            }
        });
        Ok(())
    }

    fn snapshot(&self, _server_id: &str) -> Vec<WebviewInfo> {
        let mut entries = self
            .entries
            .lock()
            .unwrap()
            .values()
            .map(|entry| WebviewInfo {
                name: entry.name.clone(),
                profile: entry.profile.clone(),
                status: entry.status.clone(),
                url: entry.url.clone(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        entries
    }

    fn update_if_label(&self, key: &str, label: &str, status: &str, url: Option<&str>) {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(key) {
            if entry.label == label {
                entry.status = status.to_string();
                if let Some(url) = url {
                    entry.url = url.to_string();
                }
            }
        }
    }

    fn remove_if_label(&self, key: &str, label: &str) -> Option<Entry> {
        let mut entries = self.entries.lock().unwrap();
        if entries.get(key).is_some_and(|entry| entry.label == label) {
            entries.remove(key)
        } else {
            None
        }
    }

    fn is_active_label(&self, key: &str, label: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|entry| entry.label == label && entry.status != "closing")
    }

    /// Returns whether the destroyed window should emit `closed`. A closing
    /// window superseded by a same-name replacement is deliberately silent so
    /// its late native callback cannot tear down the replacement's script state.
    fn handle_destroyed(&self, key: &str, label: &str) -> bool {
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            Some(entry) if entry.label == label => {
                entries.remove(key);
                true
            }
            Some(_) | None => false,
        }
    }
}

/// Production `$webview(...)` backend, installed into the script engine at startup.
pub struct EngineWebviews {
    app: AppHandle,
}

impl EngineWebviews {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ScriptWebviews for EngineWebviews {
    fn snapshot(&self, server_id: &str) -> Vec<WebviewInfo> {
        self.app
            .try_state::<WebviewManager>()
            .map(|manager| manager.snapshot(server_id))
            .unwrap_or_default()
    }
}

fn fire(app: &AppHandle, owner: &Owner, name: &str, event: &str, args: Vec<String>) {
    let Some(engine) = app.try_state::<ScriptEngine>() else {
        return;
    };
    let state = app
        .try_state::<crate::irc::state::StateStore>()
        .map(|store| store.get(&owner.server_id))
        .unwrap_or_default();
    let nick = if state.nick.is_empty() {
        owner.nick.clone()
    } else {
        state.nick.clone()
    };
    let ctx = RunCtx {
        my_nick: &nick,
        network: &owner.network,
        server: &owner.server,
        data_dir: script_data_dir(app),
        state,
    };
    let mut params = Vec::with_capacity(args.len() + 1);
    params.push(event.to_string());
    params.extend(args);
    let vars = EventVars {
        chan: name.to_string(),
        target: name.to_string(),
        text: params.join(" "),
        params,
        ..Default::default()
    };
    let actions = engine.dispatch_event(&ctx, "WEBVIEW", vars);
    apply_actions(
        app,
        &owner.server_id,
        &nick,
        &owner.network,
        &owner.server,
        actions,
    );
}

fn key(_server_id: &str, name: &str) -> String {
    // mIRC custom windows are application-wide rather than per connection.
    // Keep native script browser names in the same global namespace while the
    // stored owner still routes their events back to the opening connection.
    name.trim().to_lowercase()
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        Err("name must be 1-128 printable characters".to_string())
    } else {
        Ok(())
    }
}

fn parse_web_url(value: &str, allow_about_blank: bool) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "invalid URL".to_string())?;
    let allowed = matches!(url.scheme(), "http" | "https")
        || (allow_about_blank && url.as_str() == "about:blank");
    if allowed {
        Ok(url)
    } else {
        Err("only http:// and https:// URLs are allowed".to_string())
    }
}

fn profile_hash(profile: &str) -> [u8; 32] {
    Sha256::digest(profile.as_bytes()).into()
}

fn profile_identifier(profile: &str) -> [u8; 16] {
    let digest = profile_hash(profile);
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

fn profile_dir_name(profile: &str) -> PathBuf {
    let mut readable = profile
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    readable = readable.trim_matches('.').chars().take(40).collect();
    if readable.is_empty() {
        readable.push_str("profile");
    }
    let digest = profile_hash(profile);
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    PathBuf::from(format!("{readable}-{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls_reject_local_and_script_schemes() {
        assert!(parse_web_url("https://www.irc7.com/", false).is_ok());
        assert!(parse_web_url("about:blank", true).is_ok());
        assert!(parse_web_url("about:blank", false).is_err());
        assert!(parse_web_url("file:///etc/passwd", true).is_err());
        assert!(parse_web_url("javascript:alert(1)", true).is_err());
        assert!(parse_web_url("data:text/html,x", true).is_err());
    }

    #[test]
    fn profile_directories_are_safe_deterministic_and_collision_resistant() {
        let one = profile_dir_name("Passport/A");
        assert_eq!(one, profile_dir_name("Passport/A"));
        assert_ne!(one, profile_dir_name("Passport:A"));
        let name = one.to_string_lossy();
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
        assert!(!name.starts_with('.'));
        assert_eq!(profile_identifier("x"), profile_identifier("x"));
        assert_ne!(profile_identifier("x"), profile_identifier("y"));
    }
}
