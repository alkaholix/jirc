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

/// The whole user list, plus the auto-owner / auto-op / auto-voice / protect lists.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserList {
    entries: Vec<UserEntry>,
    aop: AutoList,
    avoice: AutoList,
    protect: AutoList,
    /// `#[serde(default)]` is load-bearing: `load_from` falls back to
    /// `Default` on any parse error, so a field an older `users.json` lacks
    /// would not merely be empty — it would discard the whole saved file.
    #[serde(default)]
    aowner: AutoList,
    /// Set on any change; the engine saves + clears it after a run.
    #[serde(skip)]
    dirty: bool,
}

/// Which auto-list a command/identifier operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoKind {
    Aop,
    Avoice,
    Protect,
    /// jIRC extension: mIRC has no owner list. Kept separate from `Aop` because
    /// owner (`+q`) is a distinct mode that only some networks support.
    Aowner,
}

/// One of the auto-lists (`/aowner`, `/aop`, `/avoice`, `/protect`): an on/off
/// flag plus entries.
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
    pub fn formatted_entries(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                let base = format!("{}:{}", entry.levels.join(","), entry.address);
                if entry.info.is_empty() {
                    base
                } else {
                    format!("{base} {}", entry.info)
                }
            })
            .collect()
    }

    /// `/rlevel <levels>` removes those levels from every user-list entry.
    pub fn remove_levels(&mut self, levels: &str) {
        let remove = split_levels(levels);
        if remove.is_empty() {
            return;
        }
        self.dirty = true;
        for entry in &mut self.entries {
            entry.levels.retain(|level| {
                !remove
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(level))
            });
        }
        self.entries.retain(|entry| !entry.levels.is_empty());
    }

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
                    if !self.entries[i]
                        .levels
                        .iter()
                        .any(|x| x.eq_ignore_ascii_case(&l))
                    {
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
            self.entries
                .retain(|e| !e.address.to_lowercase().starts_with(&p));
            return;
        }
        let rm = split_levels(levels);
        if rm.is_empty() {
            self.entries
                .retain(|e| !e.address.eq_ignore_ascii_case(address));
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
                        e.levels
                            .iter()
                            .any(|l| l.trim_start_matches('=').eq_ignore_ascii_case(want))
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
            for (i, level) in e.levels.iter().enumerate() {
                // In mIRC only the first numeric level on a user-list entry is
                // general (it grants that level and every lower one). All later
                // levels are specific, as is an explicitly `=N` first level.
                let normalized = if i == 0 || level.starts_with('=') {
                    level.clone()
                } else {
                    format!("={level}")
                };
                if !out.iter().any(|x| x.eq_ignore_ascii_case(&normalized)) {
                    out.push(normalized);
                }
            }
        }
        // mIRC's default access level for an unlisted user is 1. Without this,
        // the canonical `on 1:TEXT:...` form never fires in a fresh jIRC setup.
        if out.is_empty() {
            out.push("1".into());
        }
        out
    }

    /// The user-list mask that supplied the access level selected for an
    /// event. This is mIRC's `$maddress` (the matching list address), which is
    /// distinct from the triggering user's concrete `$fulladdress`.
    pub fn matched_address_for(&self, nick: &str, address: &str, level: &str) -> Option<&str> {
        let wanted = level.trim_start_matches('=');
        let mut entries = Vec::new();
        if !nick.is_empty() {
            entries.extend(self.matching(nick, None));
        }
        if !address.is_empty() {
            entries.extend(self.matching(address, None));
        }
        entries
            .into_iter()
            .find(|entry| {
                entry.levels.iter().any(|entry_level| {
                    entry_level
                        .trim_start_matches('=')
                        .eq_ignore_ascii_case(wanted)
                })
            })
            .map(|entry| entry.address.as_str())
    }

    // ---- auto-op / auto-voice / protect lists ----

    fn auto(&self, kind: AutoKind) -> &AutoList {
        match kind {
            AutoKind::Aop => &self.aop,
            AutoKind::Avoice => &self.avoice,
            AutoKind::Protect => &self.protect,
            AutoKind::Aowner => &self.aowner,
        }
    }

    fn auto_mut(&mut self, kind: AutoKind) -> &mut AutoList {
        match kind {
            AutoKind::Aop => &mut self.aop,
            AutoKind::Avoice => &mut self.avoice,
            AutoKind::Protect => &mut self.protect,
            AutoKind::Aowner => &mut self.aowner,
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
    pub fn auto_add(
        &mut self,
        kind: AutoKind,
        address: &str,
        channels: Vec<String>,
        network: String,
    ) {
        self.dirty = true;
        let list = self.auto_mut(kind);
        if let Some(e) = list
            .entries
            .iter_mut()
            .find(|e| e.address.eq_ignore_ascii_case(address))
        {
            for c in channels {
                if !e.channels.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                    e.channels.push(c);
                }
            }
            if !network.is_empty() {
                e.network = network;
            }
        } else {
            list.entries.push(AutoEntry {
                address: address.to_string(),
                channels,
                network,
            });
        }
    }

    pub fn auto_remove(&mut self, kind: AutoKind, address: &str) {
        self.dirty = true;
        self.auto_mut(kind)
            .entries
            .retain(|e| !e.address.eq_ignore_ascii_case(address));
    }

    /// `$aop(addr/N)[.prop]`: Nth entry's field (N=0 -> count) or an address match.
    pub fn auto_lookup(&self, kind: AutoKind, arg: &str, prop: &str) -> String {
        let list = self.auto(kind);
        if let Ok(n) = arg.trim().parse::<usize>() {
            if n == 0 {
                return list.entries.len().to_string();
            }
            return list
                .entries
                .get(n - 1)
                .map(|e| auto_prop(e, prop))
                .unwrap_or_default();
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

    /// Whether `value` (a nick or address) matches an entry in the list, for the
    /// `isaop`/`isavoice`/`isprotect`/`isaowner` operators. Membership only — the
    /// list's on/off flag gates the *automatic* behaviour, not whether someone is
    /// on it, so `/aop off` must not make `isaop` start lying.
    pub fn auto_contains(&self, kind: AutoKind, value: &str) -> bool {
        let q = complete_mask(value);
        self.auto(kind).entries.iter().any(|e| {
            let m = complete_mask(&e.address);
            wildcard_match(&q, &m) || wildcard_match(&m, &q)
        })
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
            let chan_ok =
                e.channels.is_empty() || e.channels.iter().any(|c| c.eq_ignore_ascii_case(channel));
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
pub fn level_matches(
    event_level: &str,
    user_levels: &[String],
    _status: &str,
) -> Option<(String, String)> {
    let lvl = event_level.trim();
    // `*` or empty: fires for anyone.
    if lvl.is_empty() || lvl == "*" {
        return Some(("*".into(), highest_level(user_levels)));
    }
    // `=N`: the user must hold exactly that level.
    if let Some(n) = lvl.strip_prefix('=') {
        return user_levels
            .iter()
            .any(|l| l.trim_start_matches('=') == n)
            .then(|| (lvl.into(), n.into()));
    }
    // `+N` is an exact event level in mIRC: a general level 10 user must not
    // trigger `+5`, while an explicit/specific level 5 user does.
    if let Some(exact) = lvl.strip_prefix('+') {
        return user_levels
            .iter()
            .any(|l| l.trim_start_matches('=').eq_ignore_ascii_case(exact))
            .then(|| (lvl.into(), exact.into()));
    }
    // Plain `N`: a general level >= N matches; an `=M` specific level only
    // matches when M == N.
    if let Ok(want) = lvl.parse::<i64>() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_users_have_mirc_default_level_one() {
        let users = UserList::default();
        let levels = users.levels_of("someone", "someone!u@example.test");
        assert_eq!(levels, vec!["1"]);
        assert!(level_matches("1", &levels, "").is_some());
        assert!(level_matches("2", &levels, "").is_none());
    }

    #[test]
    fn only_first_user_level_is_general_and_plus_is_exact() {
        let mut users = UserList::default();
        users.add("3,5,6", "nick!*@*", "", false);
        let levels = users.levels_of("nick", "nick!u@example.test");
        assert_eq!(levels, vec!["3", "=5", "=6"]);

        assert!(level_matches("2", &levels, "").is_some());
        assert!(level_matches("4", &levels, "").is_none());
        assert!(level_matches("5", &levels, "").is_some());
        assert!(level_matches("+5", &levels, "").is_some());
        assert!(level_matches("+3", &levels, "").is_some());
        assert!(level_matches("+2", &levels, "").is_none());
    }

    #[test]
    fn explicit_first_level_is_not_general() {
        let mut users = UserList::default();
        users.add("=3,5", "nick!*@*", "", false);
        let levels = users.levels_of("nick", "nick!u@example.test");
        assert_eq!(levels, vec!["=3", "=5"]);
        assert!(level_matches("2", &levels, "").is_none());
        assert!(level_matches("3", &levels, "").is_some());
    }

    #[test]
    fn maddress_is_the_user_list_mask_that_supplied_the_level() {
        let mut users = UserList::default();
        users.add("3", "nick!*@*", "", false);
        users.add("8", "*!*@example.test", "", false);
        assert_eq!(
            users.matched_address_for("nick", "nick!u@example.test", "8"),
            Some("*!*@example.test")
        );
        assert_eq!(
            users.matched_address_for("nick", "nick!u@example.test", "3"),
            Some("nick!*@*")
        );
    }
}
