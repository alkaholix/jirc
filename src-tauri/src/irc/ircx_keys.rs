//! Per-channel IRCX **owner/host keys**. When you become channel owner (`+q`),
//! the client provisions the channel: generates fresh OWNERKEY/HOSTKEY, sets
//! them, grants owner+host access to your username mask, and stores the keys.
//!
//! Storage is `ircx-keys.json` in the jIRC data folder — human-readable and
//! easy to open — mirrored in an in-memory map so the client can look a
//! channel's keys up instantly (e.g. to reclaim owner later).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use super::manager::ConnectionManager;
use super::state::StateStore;

/// A channel's generated owner + host keys.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelKeys {
    pub ownerkey: String,
    pub hostkey: String,
    /// Unix time (seconds) the keys were generated.
    pub updated: u64,
}

/// network -> channel -> keys.
type KeyMap = HashMap<String, HashMap<String, ChannelKeys>>;

/// Managed state: the key map (cached in memory) plus its on-disk path.
#[derive(Default)]
pub struct IrcxKeyStore {
    map: Mutex<KeyMap>,
    path: Mutex<Option<PathBuf>>,
}

impl IrcxKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads persisted keys from `path` (and remembers it as the save target).
    pub fn load(&self, path: PathBuf) {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(map) = serde_json::from_str::<KeyMap>(&data) {
                *self.map.lock().unwrap() = map;
            }
        }
        *self.path.lock().unwrap() = Some(path);
    }

    fn persist(&self) {
        let path = self.path.lock().unwrap().clone();
        if let Some(path) = path {
            if let Ok(json) = serde_json::to_string_pretty(&*self.map.lock().unwrap()) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    fn set(&self, network: &str, channel: &str, keys: ChannelKeys) {
        self.map
            .lock()
            .unwrap()
            .entry(network.to_string())
            .or_default()
            .insert(channel.to_string(), keys);
        self.persist();
    }

    fn get(&self, network: &str, channel: &str) -> Option<ChannelKeys> {
        self.map.lock().unwrap().get(network)?.get(channel).cloned()
    }
}

/// 16-char mixed-case alphanumeric keys, both drawn from one advancing xorshift
/// stream so OWNERKEY and HOSTKEY differ. (Channel keys, not crypto material.)
fn gen_keys() -> (String, String) {
    const CS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    let mut take = |n: usize| -> String {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                CS[(x % CS.len() as u64) as usize] as char
            })
            .collect()
    };
    (take(16), take(16))
}

/// The `username` from a `nick!username@host` mask (the IRC7 hex profile id).
fn username_of(mask: &str) -> Option<&str> {
    let after_bang = mask.split_once('!')?.1;
    Some(after_bang.split_once('@').map_or(after_bang, |(u, _)| u))
}

/// You got `+q` on `channel`: generate fresh owner/host keys, set them via PROP,
/// grant owner+host access to your username mask, and store the keys. Returns
/// the keys so the UI can show/save them.
#[tauri::command]
pub fn ircx_claim_owner(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    keys: State<'_, IrcxKeyStore>,
    server_id: String,
    network: String,
    channel: String,
) -> Result<ChannelKeys, String> {
    // Our own username (the stable hex profile id) from the IAL: nick!username@host.
    let snap = app.state::<StateStore>().get(&server_id);
    let mask = snap
        .ial
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&snap.nick))
        .and_then(|(_, addr)| username_of(addr))
        .map(|u| format!("*!{u}@*"))
        .ok_or("don't know our own username yet — try once the channel's names are in")?;

    let (ownerkey, hostkey) = gen_keys();

    manager.send(&server_id, format!("PROP {channel} OWNERKEY :{ownerkey}"))?;
    manager.send(&server_id, format!("PROP {channel} HOSTKEY :{hostkey}"))?;
    manager.send(&server_id, format!("ACCESS {channel} ADD OWNER {mask}"))?;
    manager.send(&server_id, format!("ACCESS {channel} ADD HOST {mask}"))?;

    let ck = ChannelKeys {
        ownerkey,
        hostkey,
        updated: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    keys.set(&network, &channel, ck.clone());
    Ok(ck)
}

/// Owner takeover protection: `offender` stripped our `+q`. Reclaim owner with
/// the stored OWNERKEY (`MODE <us> +h <key>`), wipe the owner access list, and
/// kick the offender. The server's `+q` echo for the reclaim then re-triggers
/// [`ircx_claim_owner`], which cuts fresh keys and re-adds our own owner/host
/// access — so the possibly-leaked keys are rotated as the final step.
#[tauri::command]
pub fn ircx_owner_protect(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    keys: State<'_, IrcxKeyStore>,
    server_id: String,
    network: String,
    channel: String,
    offender: String,
) -> Result<(), String> {
    let stored = keys
        .get(&network, &channel)
        .ok_or("no stored keys for this channel")?;
    let nick = app.state::<StateStore>().get(&server_id).nick.clone();
    if nick.is_empty() {
        return Err("own nick unknown".into());
    }
    manager.send(&server_id, format!("MODE {nick} +h {}", stored.ownerkey))?;
    manager.send(&server_id, format!("ACCESS {channel} CLEAR OWNER"))?;
    manager.send(&server_id, format!("KICK {channel} {offender} :owner protection"))?;
    Ok(())
}

/// Reads a channel's stored owner/host keys from the in-memory cache (fast).
#[tauri::command]
pub fn ircx_keys_get(
    keys: State<'_, IrcxKeyStore>,
    network: String,
    channel: String,
) -> Option<ChannelKeys> {
    keys.get(&network, &channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_and_key_generation() {
        // IRC7 mask is nick!username@domain; the username is the stable hex id.
        assert_eq!(username_of("Bob!a1b2c3d4@chat.irc7.com"), Some("a1b2c3d4"));
        assert_eq!(username_of("Bob!user@"), Some("user"));
        assert_eq!(username_of("no-separators"), None);

        let (owner, host) = gen_keys();
        assert_eq!(owner.len(), 16);
        assert_eq!(host.len(), 16);
        assert_ne!(owner, host);
        assert!(owner.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn store_round_trips() {
        let store = IrcxKeyStore::new();
        assert!(store.get("net", "%#c").is_none());
        store.set("net", "%#c", ChannelKeys { ownerkey: "o".into(), hostkey: "h".into(), updated: 1 });
        let got = store.get("net", "%#c").unwrap();
        assert_eq!((got.ownerkey.as_str(), got.hostkey.as_str()), ("o", "h"));
    }
}
