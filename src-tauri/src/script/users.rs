//! mIRC-style user access list (the script editor's "Users" tab). Each entry is
//! `<levels>:<address> [info]`, e.g. `10,=5:*!*@example.com Cool people`. Managed
//! by `/auser`/`/guser`/`/ruser`/`/iuser`, queried by `$ulist`/`$level`, and used
//! to gate level-prefixed `on` events (`on 5:TEXT:…`). Stored in the engine's
//! global state so it persists across script runs within a session (like hash
//! tables and variables).

use super::eval::wildcard_match;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One user-list entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserEntry {
    /// Access levels in order — numeric (`5`, `=5`) or named (`friend`).
    pub levels: Vec<String>,
    /// The nick or (wildcard) address mask.
    pub address: String,
    /// Optional info string.
    pub info: String,
}

/// The whole user list, plus the auto-op / auto-voice / protect lists.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserList {
    entries: Vec<UserEntry>,
    aop: AutoList,
    avoice: AutoList,
    protect: AutoList,
    /// Set on any change; the engine saves + clears it after a run.
    #[serde(skip)]
    dirty: bool,
}

/// Which auto-list a command/identifier operates on.
#[derive(Debug, Clone, Copy)]
pub enum AutoKind {
    Aop,
    Avoice,
    Protect,
}

/// One of the auto-lists (`/aop`, `/avoice`, `/protect`): an on/off flag plus
/// entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AutoList {
    enabled: bool,
    entries: Vec<AutoEntry>,
}

/// A single auto-list entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AutoEntry {
    address: String,
    /// Channels it applies to (empty = all channels).
    channels: Vec<String>,
    /// Network it applies to (empty = all networks, from `-w`).
    network: String,
}

/// `.type` returns the channel list; `.network` the network; else the address.
fn auto_prop(e: &AutoEntry, prop: &str) -> String {
    match prop.to_ascii_lowercase().as_str() {
        "type" => e.channels.join(","),
        "network" => e.network.clone(),
        _ => e.address.clone(),
    }
}

/// Completes a partial address to a full `nick!user@host` mask by filling the
/// missing parts with `*` (mIRC does this for a bare nick or `*@host` etc.).
fn complete_mask(addr: &str) -> String {
    let addr = addr.trim();
    if addr.is_empty() {
        return "*!*@*".into();
    }
    let (nick, rest) = match addr.split_once('!') {
        Some((n, r)) => (n.to_string(), r.to_string()),
        None => (addr.to_string(), String::new()),
    };
    if rest.is_empty() {
        // No '!': a bare nick (or user@host with no nick).
        if let Some((user, host)) = addr.split_once('@') {
            return format!("*!{user}@{host}");
        }
        return format!("{nick}!*@*");
    }
    match rest.split_once('@') {
        Some((user, host)) => format!("{nick}!{user}@{host}"),
        None => format!("{nick}!{rest}@*"),
    }
}

/// Splits a comma-separated level list into trimmed, non-empty parts.
fn split_levels(levels: &str) -> Vec<String> {
    levels
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl UserList {
    /// Load the user list (and auto-lists) from `dir/users.json`; empty if the
    /// file is absent or unreadable.
    pub fn load_from(dir: &Path) -> UserList {
        std::fs::read_to_string(dir.join("users.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save the user list (and auto-lists) to `dir/users.json`.
    pub fn save_to(&self, dir: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("users.json"), json);
        }
    }

    /// Whether the list changed since the last check (clears the flag).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn position(&self, address: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.address.eq_ignore_ascii_case(address))
    }

    /// `/auser [-a]` / `/guser`: create or replace the entry for `address`, or —
    /// with `add` — merge the new levels into an existing entry (deduped).
    pub fn add(&mut self, levels: &str, address: &str, info: &str, add: bool) {
        self.dirty = true;
        let new_levels = split_levels(levels);
        if let Some(i) = self.position(address) {
            if add {
                for l in new_levels {
                    if !self.entries[i].levels.iter().any(|x| x.eq_ignore_ascii_case(&l)) {
                        self.entries[i].levels.push(l);
                    }
                }
            } else {
                self.entries[i].levels = new_levels;
            }
            if !info.is_empty() {
                self.entries[i].info = info.to_string();
            }
        } else {
            self.entries.push(UserEntry {
                levels: new_levels,
                address: address.to_string(),
                info: info.to_string(),
            });
        }
    }

    /// `/ruser [levels] <nick|address>`: remove the whole entry, or just the
    /// listed levels (removing the entry if none remain). A trailing `!` on the
    /// address removes every entry whose address begins with it.
    pub fn remove(&mut self, levels: &str, address: &str) {
        self.dirty = true;
        if let Some(prefix) = address.strip_suffix('!') {
            let p = prefix.to_lowercase();
            self.entries.retain(|e| !e.address.to_lowercase().starts_with(&p));
            return;
        }
        let rm = split_levels(levels);
        if rm.is_empty() {
            self.entries.retain(|e| !e.address.eq_ignore_ascii_case(address));
        } else if let Some(i) = self.position(address) {
            self.entries[i]
                .levels
                .retain(|l| !rm.iter().any(|r| r.eq_ignore_ascii_case(l)));
            if self.entries[i].levels.is_empty() {
                self.entries.remove(i);
            }
        }
    }

    /// `/iuser <nick|address> [info]`: set (or clear) an entry's info.
    pub fn set_info(&mut self, address: &str, info: &str) {
        self.dirty = true;
        if let Some(i) = self.position(address) {
            self.entries[i].info = info.to_string();
        }
    }

    /// Entries matching `addr` (bidirectional wildcard so a wildcard query matches
    /// specific entries and a real address matches wildcard entries), optionally
    /// filtered to those carrying `level` (compared ignoring a leading `=`).
    pub fn matching(&self, addr: &str, level: Option<&str>) -> Vec<&UserEntry> {
        let q = complete_mask(addr);
        self.entries
            .iter()
            .filter(|e| {
                let m = complete_mask(&e.address);
                (wildcard_match(&q, &m) || wildcard_match(&m, &q))
                    && level.map_or(true, |want| {
                        e.levels.iter().any(|l| l.trim_start_matches('=').eq_ignore_ascii_case(want))
                    })
            })
            .collect()
    }

    /// `$level(addr)`: the comma-joined levels of the first matching entry.
    pub fn levels_for(&self, addr: &str) -> String {
        self.matching(addr, None)
            .first()
            .map(|e| e.levels.join(","))
            .unwrap_or_default()
    }

    /// All access levels a user holds — the union of the levels on every entry
    /// matching their `nick` or resolved `address` (deduped). Used to gate events.
    pub fn levels_of(&self, nick: &str, address: &str) -> Vec<String> {
        let mut entries: Vec<&UserEntry> = Vec::new();
        if !nick.is_empty() {
            entries.extend(self.matching(nick, None));
        }
        if !address.is_empty() {
            entries.extend(self.matching(address, None));
        }
        let mut out: Vec<String> = Vec::new();
        for e in entries {
            for l in &e.levels {
                if !out.iter().any(|x| x.eq_ignore_ascii_case(l)) {
                    out.push(l.clone());
                }
            }
        }
        out
    }

    // ---- auto-op / auto-voice / protect lists ----

    fn auto(&self, kind: AutoKind) -> &AutoList {
        match kind {
            AutoKind::Aop => &self.aop,
            AutoKind::Avoice => &self.avoice,
            AutoKind::Protect => &self.protect,
        }
    }

    fn auto_mut(&mut self, kind: AutoKind) -> &mut AutoList {
        match kind {
            AutoKind::Aop => &mut self.aop,
            AutoKind::Avoice => &mut self.avoice,
            AutoKind::Protect => &mut self.protect,
        }
    }

    pub fn auto_toggle(&mut self, kind: AutoKind, on: bool) {
        self.dirty = true;
        self.auto_mut(kind).enabled = on;
    }

    pub fn auto_enabled(&self, kind: AutoKind) -> bool {
        self.auto(kind).enabled
    }

    /// Add or merge an auto-list entry (merging channels on an existing address).
    pub fn auto_add(&mut self, kind: AutoKind, address: &str, channels: Vec<String>, network: String) {
        self.dirty = true;
        let list = self.auto_mut(kind);
        if let Some(e) = list.entries.iter_mut().find(|e| e.address.eq_ignore_ascii_case(address)) {
            for c in channels {
                if !e.channels.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                    e.channels.push(c);
                }
            }
            if !network.is_empty() {
                e.network = network;
            }
        } else {
            list.entries.push(AutoEntry { address: address.to_string(), channels, network });
        }
    }

    pub fn auto_remove(&mut self, kind: AutoKind, address: &str) {
        self.dirty = true;
        self.auto_mut(kind).entries.retain(|e| !e.address.eq_ignore_ascii_case(address));
    }

    /// `$aop(addr/N)[.prop]`: Nth entry's field (N=0 -> count) or an address match.
    pub fn auto_lookup(&self, kind: AutoKind, arg: &str, prop: &str) -> String {
        let list = self.auto(kind);
        if let Ok(n) = arg.trim().parse::<usize>() {
            if n == 0 {
                return list.entries.len().to_string();
            }
            return list.entries.get(n - 1).map(|e| auto_prop(e, prop)).unwrap_or_default();
        }
        let q = complete_mask(arg);
        for e in &list.entries {
            let m = complete_mask(&e.address);
            if wildcard_match(&q, &m) || wildcard_match(&m, &q) {
                return auto_prop(e, prop);
            }
        }
        String::new()
    }

    /// Whether the auto behaviour applies to a joining user: the list is enabled
    /// and an entry matches their address/nick, channel, and network.
    pub fn auto_should_apply(
        &self,
        kind: AutoKind,
        address: &str,
        nick: &str,
        channel: &str,
        network: &str,
    ) -> bool {
        let list = self.auto(kind);
        if !list.enabled {
            return false;
        }
        list.entries.iter().any(|e| {
            let m = complete_mask(&e.address);
            let hit = |v: &str| {
                let x = complete_mask(v);
                wildcard_match(&m, &x) || wildcard_match(&x, &m)
            };
            let addr_ok = (!address.is_empty() && hit(address)) || hit(nick);
            let chan_ok = e.channels.is_empty() || e.channels.iter().any(|c| c.eq_ignore_ascii_case(channel));
            let net_ok = e.network.is_empty() || e.network.eq_ignore_ascii_case(network);
            addr_ok && chan_ok && net_ok
        })
    }
}

/// The highest numeric level in a user's level list (ignoring a leading `=`), as
/// a string, or empty if none are numeric. Used for `$ulevel` on `*` events.
fn highest_level(user_levels: &[String]) -> String {
    user_levels
        .iter()
        .filter_map(|l| l.trim_start_matches('=').parse::<i64>().ok())
        .max()
        .map(|n| n.to_string())
        .unwrap_or_default()
}

/// Decides whether an event with access-level prefix `event_level` fires for a
/// user holding `user_levels`, whose channel-status prefixes are `status`
/// (e.g. `"@+"`). Returns `Some((clevel, ulevel))` — the event level and the
/// user's matched level — when it fires, else `None`.
pub fn level_matches(event_level: &str, user_levels: &[String], status: &str) -> Option<(String, String)> {
    let lvl = event_level.trim();
    // `*` or empty: fires for anyone.
    if lvl.is_empty() || lvl == "*" {
        return Some(("*".into(), highest_level(user_levels)));
    }
    // Channel-status prefix (~ owner, & admin, @ op, % halfop, + voice).
    if lvl.chars().count() == 1 {
        if let Some(c) = lvl.chars().next().filter(|c| "~&@%+".contains(*c)) {
            return status.contains(c).then(|| (lvl.into(), lvl.into()));
        }
    }
    // `=N`: the user must hold exactly that level.
    if let Some(n) = lvl.strip_prefix('=') {
        return user_levels
            .iter()
            .any(|l| l.trim_start_matches('=') == n)
            .then(|| (lvl.into(), n.into()));
    }
    // `N` or `+N`: the user needs a level >= N (an `=M` level only matches M == N).
    let num = lvl.strip_prefix('+').unwrap_or(lvl);
    if let Ok(want) = num.parse::<i64>() {
        let mut best: Option<i64> = None;
        for l in user_levels {
            let matched = match l.strip_prefix('=') {
                Some(exact) => exact.parse::<i64>().ok().filter(|&e| e == want),
                None => l.parse::<i64>().ok().filter(|&m| m >= want),
            };
            if let Some(m) = matched {
                best = Some(best.map_or(m, |b| b.max(m)));
            }
        }
        return best.map(|b| (lvl.into(), b.to_string()));
    }
    // Named level: the user must hold that name.
    user_levels
        .iter()
        .any(|l| l.trim_start_matches('=').eq_ignore_ascii_case(lvl))
        .then(|| (lvl.into(), lvl.into()))
}
