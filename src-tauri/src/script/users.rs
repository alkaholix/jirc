//! mIRC-style user access list (the script editor's "Users" tab). Each entry is
//! `<levels>:<address> [info]`, e.g. `10,=5:*!*@example.com Cool people`. Managed
//! by `/auser`/`/guser`/`/ruser`/`/iuser`, queried by `$ulist`/`$level`, and used
//! to gate level-prefixed `on` events (`on 5:TEXT:…`). Stored in the engine's
//! global state so it persists across script runs within a session (like hash
//! tables and variables).

use super::eval::wildcard_match;

/// One user-list entry.
#[derive(Debug, Clone, Default)]
pub struct UserEntry {
    /// Access levels in order — numeric (`5`, `=5`) or named (`friend`).
    pub levels: Vec<String>,
    /// The nick or (wildcard) address mask.
    pub address: String,
    /// Optional info string.
    pub info: String,
}

/// The whole user list.
#[derive(Debug, Clone, Default)]
pub struct UserList {
    entries: Vec<UserEntry>,
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
    fn position(&self, address: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.address.eq_ignore_ascii_case(address))
    }

    /// `/auser [-a]` / `/guser`: create or replace the entry for `address`, or —
    /// with `add` — merge the new levels into an existing entry (deduped).
    pub fn add(&mut self, levels: &str, address: &str, info: &str, add: bool) {
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
}
