//! On-disk persistence: server profiles and per-buffer chat logs.
//!
//! Profiles live in the app config dir as `profiles.json`. Logs live under the
//! app data dir as `logs/<network>/<buffer>.log`. The frontend owns buffer
//! routing, so it calls [`log_append`] with already-formatted lines.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use keyring::Entry;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::config::ServerProfile;

const KEYRING_SERVICE: &str = "jirc";

/// Stores a secret in the OS keyring; returns true on success.
fn store_secret(id: &str, field: &str, value: Option<&str>) -> bool {
    let account = format!("{id}:{field}");
    let Ok(entry) = Entry::new(KEYRING_SERVICE, &account) else {
        return false;
    };
    match value {
        Some(v) if !v.is_empty() => entry.set_password(v).is_ok(),
        _ => {
            // Best-effort removal of any stale secret.
            let _ = entry.delete_credential();
            true
        }
    }
}

/// Probes whether the OS keyring is usable (write + read + delete a throwaway
/// entry). Used to tell the user where passwords will be stored.
#[tauri::command]
pub fn keyring_available() -> bool {
    let Ok(entry) = Entry::new(KEYRING_SERVICE, "__jirc_probe__") else {
        return false;
    };
    if entry.set_password("probe").is_err() {
        return false;
    }
    let ok = entry.get_password().map(|v| v == "probe").unwrap_or(false);
    let _ = entry.delete_credential();
    ok
}

/// Loads a secret from the OS keyring, if present.
fn load_secret(id: &str, field: &str) -> Option<String> {
    let account = format!("{id}:{field}");
    Entry::new(KEYRING_SERVICE, &account)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty())
}

/// Replaces characters that are illegal in file names on Windows/Unix.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches([' ', '.']).to_string();
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed
    }
}

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_config_dir().map_err(|e| e.to_string())
}

fn logs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("logs"))
}

/// Loads saved server profiles, rehydrating secrets from the OS keyring.
#[tauri::command]
pub fn profiles_load(app: AppHandle) -> Result<Vec<ServerProfile>, String> {
    let path = config_dir(&app)?.join("profiles.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut profiles: Vec<ServerProfile> = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    for p in &mut profiles {
        if let Some(id) = p.id.clone() {
            if p.account_password.is_none() {
                p.account_password = load_secret(&id, "account_password");
            }
            if p.password.is_none() {
                p.password = load_secret(&id, "password");
            }
            if let Some(proxy) = p.proxy.as_mut() {
                if proxy.password.is_none() {
                    proxy.password = load_secret(&id, "proxy_password");
                }
            }
        }
    }
    Ok(profiles)
}

/// Persists server profiles, moving secrets into the OS keyring when possible.
/// Secrets that cannot be stored in the keyring remain in the JSON as a
/// fallback so functionality is preserved.
#[tauri::command]
pub fn profiles_save(app: AppHandle, mut profiles: Vec<ServerProfile>) -> Result<(), String> {
    let dir = config_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    for p in &mut profiles {
        let id = p.id.get_or_insert_with(|| Uuid::new_v4().to_string()).clone();
        if store_secret(&id, "account_password", p.account_password.as_deref()) {
            p.account_password = None;
        }
        if store_secret(&id, "password", p.password.as_deref()) {
            p.password = None;
        }
        if let Some(proxy) = p.proxy.as_mut() {
            if store_secret(&id, "proxy_password", proxy.password.as_deref()) {
                proxy.password = None;
            }
        }
    }

    let json = serde_json::to_string_pretty(&profiles).map_err(|e| e.to_string())?;
    fs::write(dir.join("profiles.json"), json).map_err(|e| e.to_string())
}

/// Deletes a saved profile by id, including its keyring secrets.
#[tauri::command]
pub fn profiles_delete(app: AppHandle, id: String) -> Result<(), String> {
    let path = config_dir(&app)?.join("profiles.json");
    if path.exists() {
        let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut profiles: Vec<ServerProfile> =
            serde_json::from_str(&data).map_err(|e| e.to_string())?;
        profiles.retain(|p| p.id.as_deref() != Some(id.as_str()));
        let json = serde_json::to_string_pretty(&profiles).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())?;
    }
    // Remove any keyring secrets for this profile.
    store_secret(&id, "account_password", None);
    store_secret(&id, "password", None);
    store_secret(&id, "proxy_password", None);
    Ok(())
}

/// Appends a formatted line to a buffer's log file.
#[tauri::command]
pub fn log_append(
    app: AppHandle,
    network: String,
    buffer: String,
    line: String,
) -> Result<(), String> {
    let dir = logs_dir(&app)?.join(sanitize(&network));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.log", sanitize(&buffer)));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{line}").map_err(|e| e.to_string())
}

/// Reads back a buffer's log (empty string if none).
#[tauri::command]
pub fn log_read(app: AppHandle, network: String, buffer: String) -> Result<String, String> {
    let path = logs_dir(&app)?
        .join(sanitize(&network))
        .join(format!("{}.log", sanitize(&buffer)));
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{keyring_available, load_secret, sanitize, store_secret};

    #[test]
    fn sanitizes_illegal_chars() {
        assert_eq!(sanitize("#chan"), "#chan");
        assert_eq!(sanitize("a/b:c*"), "a_b_c_");
        assert_eq!(sanitize("  "), "_");
        assert_eq!(sanitize("Libera.Chat"), "Libera.Chat");
    }

    /// Round-trips a secret through the real OS keyring. Ignored by default
    /// (touches the OS credential store): run with `-- --ignored keyring`.
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        assert!(keyring_available(), "OS keyring not available on this platform");
        let id = "test-roundtrip";
        assert!(store_secret(id, "account_password", Some("s3cret")));
        assert_eq!(load_secret(id, "account_password").as_deref(), Some("s3cret"));
        // delete and confirm it's gone
        assert!(store_secret(id, "account_password", None));
        assert_eq!(load_secret(id, "account_password"), None);
    }
}
