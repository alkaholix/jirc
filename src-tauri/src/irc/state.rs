//! Per-connection session state tracked by the read loop.
//!
//! This is the backend's authoritative view of the connection (current nick,
//! joined channels, members, topics) plus the server's ISUPPORT (005) info so
//! non-standard prefixes and channel types are handled correctly.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::irc::event::Member;

/// Server capabilities learned from RPL_ISUPPORT (005).
#[derive(Debug, Clone)]
pub struct Isupport {
    /// (mode letter, prefix char) in descending rank order.
    pub prefix_modes: Vec<(char, char)>,
    /// Leading characters that denote a channel name.
    pub chan_types: String,
    /// CHANMODES type A (lists — always take an argument).
    pub chanmodes_a: String,
    /// CHANMODES type B (always take an argument).
    pub chanmodes_b: String,
    /// CHANMODES type C (take an argument only when set, i.e. `+`).
    pub chanmodes_c: String,
    /// CHANMODES type D (never take an argument).
    pub chanmodes_d: String,
}

impl Default for Isupport {
    fn default() -> Self {
        Isupport {
            prefix_modes: vec![
                ('q', '~'),
                ('a', '&'),
                ('o', '@'),
                ('h', '%'),
                ('v', '+'),
            ],
            chan_types: "#&!+".to_string(),
            chanmodes_a: "beI".to_string(),
            chanmodes_b: "k".to_string(),
            chanmodes_c: "l".to_string(),
            chanmodes_d: "imnpstrS".to_string(),
        }
    }
}

impl Isupport {
    pub fn prefix_for_mode(&self, mode: char) -> Option<char> {
        self.prefix_modes
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, p)| *p)
    }

    /// All prefix chars, highest rank first (e.g. "~&@%+" or ".@+").
    pub fn prefix_chars(&self) -> String {
        self.prefix_modes.iter().map(|(_, p)| *p).collect()
    }

    /// Sorts prefix chars by rank (highest first), dropping unknowns.
    pub fn order_prefixes(&self, prefixes: &mut String) {
        let ranked: String = self
            .prefix_modes
            .iter()
            .map(|(_, p)| *p)
            .filter(|p| prefixes.contains(*p))
            .collect();
        *prefixes = ranked;
    }

    #[allow(dead_code)]
    pub fn is_channel(&self, name: &str) -> bool {
        name.chars().next().is_some_and(|c| self.chan_types.contains(c))
    }

    /// Splits a NAMES entry like "@+nick" into (prefixes, nick).
    pub fn split_prefixes(&self, entry: &str) -> (String, String) {
        let known = self.prefix_chars();
        let mut prefixes = String::new();
        let mut rest = entry;
        while let Some(c) = rest.chars().next() {
            if known.contains(c) {
                prefixes.push(c);
                rest = &rest[c.len_utf8()..];
            } else {
                break;
            }
        }
        self.order_prefixes(&mut prefixes);
        (prefixes, rest.to_string())
    }

    /// True if mode `letter` carries an argument for this `adding` direction.
    pub fn mode_takes_arg(&self, letter: char, adding: bool) -> bool {
        if self.prefix_modes.iter().any(|(m, _)| *m == letter) {
            return true;
        }
        if self.chanmodes_a.contains(letter) || self.chanmodes_b.contains(letter) {
            return true;
        }
        if self.chanmodes_c.contains(letter) {
            return adding;
        }
        false
    }

    /// Parses a single ISUPPORT token, e.g. `PREFIX=(qov).@+`, `CHANTYPES=%#`,
    /// or `CHANMODES=A,B,C,D`.
    pub fn parse_token(&mut self, token: &str) {
        if let Some(v) = token.strip_prefix("PREFIX=") {
            if let Some((modes, prefixes)) = v.strip_prefix('(').and_then(|s| s.split_once(')')) {
                let pairs: Vec<(char, char)> = modes.chars().zip(prefixes.chars()).collect();
                if !pairs.is_empty() {
                    self.prefix_modes = pairs;
                }
            }
        } else if let Some(v) = token.strip_prefix("CHANTYPES=") {
            if !v.is_empty() {
                self.chan_types = v.to_string();
            }
        } else if let Some(v) = token.strip_prefix("CHANMODES=") {
            let parts: Vec<&str> = v.split(',').collect();
            if parts.len() >= 4 {
                self.chanmodes_a = parts[0].to_string();
                self.chanmodes_b = parts[1].to_string();
                self.chanmodes_c = parts[2].to_string();
                self.chanmodes_d = parts[3].to_string();
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ChannelState {
    pub topic: Option<String>,
    /// nick (case-sensitive as seen) -> prefix string, e.g. "@+".
    pub members: BTreeMap<String, String>,
    /// Active `+b` ban masks (from live MODE and RPL_BANLIST), for `isban`.
    pub bans: std::collections::BTreeSet<String>,
}

impl ChannelState {
    pub fn member_list(&self) -> Vec<Member> {
        self.members
            .iter()
            .map(|(nick, prefix)| Member {
                nick: nick.clone(),
                prefix: prefix.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub struct SessionState {
    pub nick: String,
    pub channels: BTreeMap<String, ChannelState>,
    pub isupport: Isupport,
    /// Set once RPL_WELCOME is received.
    pub registered: bool,
    /// How many alternative nicks we've tried during registration.
    pub nick_attempts: u32,
    /// Internal address list: lowercase nick -> full `nick!user@host`, learned
    /// from message prefixes and `userhost-in-names` NAMES replies.
    pub ial: BTreeMap<String, String>,
}

impl SessionState {
    pub fn upsert_member(&mut self, channel: &str, nick: &str, prefixes: String) {
        self.channels
            .entry(channel.to_string())
            .or_default()
            .members
            .insert(nick.to_string(), prefixes);
    }

    pub fn remove_member(&mut self, channel: &str, nick: &str) {
        if let Some(ch) = self.channels.get_mut(channel) {
            ch.members.remove(nick);
        }
    }

    /// Removes a nick from every channel, returning the channels they were in.
    pub fn remove_member_everywhere(&mut self, nick: &str) -> Vec<String> {
        let mut found = Vec::new();
        for (name, ch) in self.channels.iter_mut() {
            if ch.members.remove(nick).is_some() {
                found.push(name.clone());
            }
        }
        found
    }

    /// Renames a nick across all channels (preserving prefixes).
    pub fn rename_member(&mut self, old: &str, new: &str) {
        for ch in self.channels.values_mut() {
            if let Some(prefix) = ch.members.remove(old) {
                ch.members.insert(new.to_string(), prefix);
            }
        }
    }

    /// Records a `nick!user@host` address in the internal address list.
    pub fn record_address(&mut self, nick: &str, address: String) {
        self.ial.insert(nick.to_lowercase(), address);
    }

    /// Adds (`adding`) or removes a `+b` ban mask for a channel.
    pub fn set_ban(&mut self, channel: &str, mask: &str, adding: bool) {
        let ch = self.channels.entry(channel.to_string()).or_default();
        if adding {
            ch.bans.insert(mask.to_string());
        } else {
            ch.bans.remove(mask);
        }
    }

    /// Applies a privilege mode change to a member's prefixes.
    pub fn apply_prefix_mode(&mut self, channel: &str, nick: &str, mode: char, adding: bool) {
        let Some(prefix_char) = self.isupport.prefix_for_mode(mode) else {
            return;
        };
        let order = &self.isupport;
        if let Some(ch) = self.channels.get_mut(channel) {
            if let Some(prefixes) = ch.members.get_mut(nick) {
                if adding {
                    if !prefixes.contains(prefix_char) {
                        prefixes.push(prefix_char);
                    }
                } else {
                    prefixes.retain(|c| c != prefix_char);
                }
                order.order_prefixes(prefixes);
            }
        }
    }
}

/// A read-only view of one channel for script identifiers.
#[derive(Debug, Default, Clone)]
pub struct ChannelView {
    pub name: String,
    /// Member nicks (without prefixes), in roster order.
    pub nicks: Vec<String>,
    /// (nick, prefix chars) per member, e.g. `("bob", "@")`. Powers the
    /// `isop`/`ishop`/`isvoice`/`ison`/`isreg`/... condition operators.
    pub members: Vec<(String, String)>,
    /// Active `+b` ban masks, for the `isban` operator.
    pub bans: Vec<String>,
}

/// A snapshot of a connection's channel/member state, shared with the script
/// engine so identifiers like `$chan(N)` and `$nick(#,N)` can resolve.
#[derive(Debug, Default, Clone)]
pub struct StateSnapshot {
    pub nick: String,
    pub channels: Vec<ChannelView>,
    /// (lowercase nick, full `nick!user@host`) pairs for `$address`/`$ial`.
    pub ial: Vec<(String, String)>,
}

impl SessionState {
    /// Builds a snapshot for the script engine.
    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            nick: self.nick.clone(),
            channels: self
                .channels
                .iter()
                .map(|(name, ch)| ChannelView {
                    name: name.clone(),
                    nicks: ch.members.keys().cloned().collect(),
                    members: ch.members.iter().map(|(n, p)| (n.clone(), p.clone())).collect(),
                    bans: ch.bans.iter().cloned().collect(),
                })
                .collect(),
            ial: self.ial.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }
}

/// Per-connection state snapshots, keyed by server id. Managed Tauri state so
/// script commands/timers/sockets can read channel/member info off the engine's
/// own (non-connection) threads.
#[derive(Default)]
pub struct StateStore {
    map: Mutex<HashMap<String, Arc<StateSnapshot>>>,
}

impl StateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, server_id: &str, snap: StateSnapshot) {
        self.map.lock().unwrap().insert(server_id.to_string(), Arc::new(snap));
    }

    pub fn get(&self, server_id: &str) -> Arc<StateSnapshot> {
        self.map.lock().unwrap().get(server_id).cloned().unwrap_or_default()
    }

    pub fn remove(&self, server_id: &str) {
        self.map.lock().unwrap().remove(server_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_known_prefixes() {
        let s = Isupport::default();
        assert_eq!(s.split_prefixes("@+bob"), ("@+".to_string(), "bob".to_string()));
        assert_eq!(s.split_prefixes("alice"), (String::new(), "alice".to_string()));
    }

    #[test]
    fn orders_prefixes_by_rank() {
        let s = Isupport::default();
        assert_eq!(s.split_prefixes("+@carol"), ("@+".to_string(), "carol".to_string()));
    }

    #[test]
    fn parses_nonstandard_prefix_and_chantypes() {
        let mut s = Isupport::default();
        s.parse_token("PREFIX=(qov).@+");
        s.parse_token("CHANTYPES=%#");
        // Owner is now '.', and '%' starts a channel.
        assert_eq!(s.prefix_for_mode('q'), Some('.'));
        assert!(s.is_channel("%room"));
        assert!(s.is_channel("#room"));
        assert!(!s.is_channel("nick"));
        assert_eq!(s.split_prefixes(".@dave"), (".@".to_string(), "dave".to_string()));
    }

    #[test]
    fn applies_and_removes_prefix_modes() {
        let mut s = SessionState::default();
        s.upsert_member("#test", "dave", String::new());
        s.apply_prefix_mode("#test", "dave", 'o', true);
        assert_eq!(s.channels["#test"].members["dave"], "@");
        s.apply_prefix_mode("#test", "dave", 'v', true);
        assert_eq!(s.channels["#test"].members["dave"], "@+");
        s.apply_prefix_mode("#test", "dave", 'o', false);
        assert_eq!(s.channels["#test"].members["dave"], "+");
    }

    #[test]
    fn rename_and_remove_everywhere() {
        let mut s = SessionState::default();
        s.upsert_member("#a", "eve", "@".to_string());
        s.upsert_member("#b", "eve", String::new());
        s.rename_member("eve", "eve2");
        assert!(s.channels["#a"].members.contains_key("eve2"));
        let chans = s.remove_member_everywhere("eve2");
        assert_eq!(chans.len(), 2);
    }
}
