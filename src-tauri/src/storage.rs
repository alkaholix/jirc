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

use serde::Serialize;

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

/// The app's data-folder name on disk. We use a friendly `jIRC` rather than the
/// Tauri bundle identifier (`com.jirc.app`, which still drives bundling/OS
/// registration). [`migrate_legacy_app_dir`] moves the old folder once.
pub const APP_DIR_NAME: &str = "jIRC";

/// A user-chosen data location that overrides the default per-profile folder.
/// In priority: the `JIRC_DATA_DIR` env var, then — for a portable install — a
/// `data/` folder next to the executable when a `portable.txt` marker sits
/// beside it. `None` means "use the per-user OS directory".
fn custom_base(app: &AppHandle) -> Option<PathBuf> {
    let env = std::env::var("JIRC_DATA_DIR").ok();
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(|e| e.parent());
    if let Some(b) = resolve_custom_base(env.as_deref(), exe_dir) {
        return Some(b);
    }
    // A folder chosen in Settings is recorded as a `location.txt` redirect inside
    // the default (per-profile) jIRC dir, which we always look in.
    let default_base = app.path().config_dir().ok()?.join(APP_DIR_NAME);
    read_location_redirect(&default_base)
}

/// Reads a saved custom data path from `<default_base>/location.txt` (written by
/// the data-folder setting). Empty/whitespace means "no redirect".
fn read_location_redirect(default_base: &std::path::Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(default_base.join("location.txt")).ok()?;
    let p = content.trim();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

/// Pure resolver for [`custom_base`] (so it can be unit-tested).
fn resolve_custom_base(env_override: Option<&str>, exe_dir: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Some(p) = env_override {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Some(dir) = exe_dir {
        if dir.join("portable.txt").exists() {
            return Some(dir.join("data"));
        }
    }
    None
}

/// The base jIRC data folder — holds `profiles.json`, `scripts/`, `dcc/`,
/// `scriptdata/`. A custom location (env/portable) wins; otherwise it's
/// `<os config dir>/jIRC`, under the user's profile.
pub fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = match custom_base(app) {
        Some(b) => b,
        None => app
            .path()
            .config_dir()
            .map_err(|e| e.to_string())?
            .join(APP_DIR_NAME),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Where `logs/` lives: the same custom base when set, else `<os data dir>/jIRC`
/// (on Windows this is the same folder as [`config_dir`]).
pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = match custom_base(app) {
        Some(b) => b,
        None => app
            .path()
            .data_dir()
            .map_err(|e| e.to_string())?
            .join(APP_DIR_NAME),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// `<base>/dcc` — where received DCC files are saved. Created on demand.
pub fn dcc_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = config_dir(app)?.join("dcc");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// The data-folder state, for the Settings dialog.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataLocation {
    /// The folder data is currently stored in (resolved).
    pub current: String,
    /// The custom folder saved in Settings (empty = the default per-profile dir).
    pub custom: String,
    /// True when an env var (`JIRC_DATA_DIR`) or a portable install is forcing
    /// the location — the Settings field can't override that until it's removed.
    pub forced: bool,
}

/// Reports where data lives now, the saved custom path, and whether an
/// env/portable override is in force.
#[tauri::command]
pub fn data_location(app: AppHandle) -> Result<DataLocation, String> {
    let current = config_dir(&app)?.to_string_lossy().to_string();
    let default_base = app
        .path()
        .config_dir()
        .map_err(|e| e.to_string())?
        .join(APP_DIR_NAME);
    let custom = read_location_redirect(&default_base)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(|e| e.parent());
    let forced = resolve_custom_base(std::env::var("JIRC_DATA_DIR").ok().as_deref(), exe_dir).is_some();
    Ok(DataLocation {
        current,
        custom,
        forced,
    })
}

/// Sets (or, with `None`/empty, clears) the custom data folder. Takes effect on
/// the next launch; existing data is not moved.
#[tauri::command]
pub fn set_data_location(app: AppHandle, path: Option<String>) -> Result<(), String> {
    let default_base = app
        .path()
        .config_dir()
        .map_err(|e| e.to_string())?
        .join(APP_DIR_NAME);
    std::fs::create_dir_all(&default_base).map_err(|e| e.to_string())?;
    let redirect = default_base.join("location.txt");
    match path {
        Some(p) if !p.trim().is_empty() => {
            std::fs::write(&redirect, p.trim()).map_err(|e| e.to_string())
        }
        _ => {
            if redirect.exists() {
                std::fs::remove_file(&redirect).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
}

fn logs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_dir(app)?.join("logs"))
}

/// One-time rename of the old identifier-named folder (`com.jirc.app`) to `jIRC`,
/// in both the OS config and data base dirs. Runs at startup; a no-op once
/// migrated or on a fresh install.
pub fn migrate_legacy_app_dir(app: &AppHandle) {
    // A custom/portable data dir has no per-profile legacy folder to migrate.
    if custom_base(app).is_some() {
        return;
    }
    for base in [app.path().config_dir(), app.path().data_dir()] {
        if let Ok(base) = base {
            migrate_dir(&base);
        }
    }
}

/// Renames `<base>/com.jirc.app` to `<base>/jIRC` when the old exists and the new
/// doesn't (never clobbering). A same-volume rename, so it's atomic and instant.
fn migrate_dir(base: &std::path::Path) {
    let old = base.join("com.jirc.app");
    let new = base.join(APP_DIR_NAME);
    if old.exists() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
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
    use super::{
        keyring_available, load_secret, migrate_dir, read_location_redirect, resolve_custom_base,
        sanitize, store_secret, APP_DIR_NAME,
    };

    #[test]
    fn reads_location_redirect() {
        let tmp = std::env::temp_dir().join(format!("jirc-loc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(read_location_redirect(&tmp), None); // no file
        std::fs::write(tmp.join("location.txt"), "  \n").unwrap();
        assert_eq!(read_location_redirect(&tmp), None); // whitespace
        std::fs::write(tmp.join("location.txt"), "  D:/my data  \n").unwrap();
        assert_eq!(
            read_location_redirect(&tmp),
            Some(std::path::PathBuf::from("D:/my data"))
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn custom_base_resolution() {
        use std::path::PathBuf;
        // The env override wins and is taken verbatim.
        assert_eq!(
            resolve_custom_base(Some("D:/jirc-data"), None),
            Some(PathBuf::from("D:/jirc-data"))
        );
        // Blank env + no exe dir -> the default (None).
        assert_eq!(resolve_custom_base(Some("   "), None), None);
        assert_eq!(resolve_custom_base(None, None), None);

        // A portable.txt marker beside the exe -> <dir>/data; absent -> None.
        let tmp = std::env::temp_dir().join(format!("jirc-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(resolve_custom_base(None, Some(tmp.as_path())), None);
        std::fs::write(tmp.join("portable.txt"), "").unwrap();
        assert_eq!(
            resolve_custom_base(None, Some(tmp.as_path())),
            Some(tmp.join("data"))
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sanitizes_illegal_chars() {
        assert_eq!(sanitize("#chan"), "#chan");
        assert_eq!(sanitize("a/b:c*"), "a_b_c_");
        assert_eq!(sanitize("  "), "_");
        assert_eq!(sanitize("Libera.Chat"), "Libera.Chat");
    }

    #[test]
    fn migrates_legacy_app_dir() {
        let tmp = std::env::temp_dir().join(format!("jirc-mig-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let old = tmp.join("com.jirc.app");
        std::fs::create_dir_all(old.join("scripts")).unwrap();
        std::fs::write(old.join("profiles.json"), "[]").unwrap();

        migrate_dir(&tmp);

        let new = tmp.join(APP_DIR_NAME);
        assert!(new.join("profiles.json").exists(), "profiles migrated");
        assert!(new.join("scripts").exists(), "scripts migrated");
        assert!(!old.exists(), "old folder gone");

        // Idempotent and never clobbers: a second run is a no-op.
        migrate_dir(&tmp);
        assert!(new.exists());

        let _ = std::fs::remove_dir_all(&tmp);
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
