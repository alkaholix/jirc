//! Per-connection session state tracked by the read loop.
//!
//! This is the backend's authoritative view of the connection (current nick,
//! joined channels, members, topics) plus the server's ISUPPORT (005) info so
//! non-standard prefixes and channel types are handled correctly.

use std::collections::{BTreeMap, HashMap};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use crate::irc::event::Member;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// IRC casemapping advertised by `RPL_ISUPPORT CASEMAPPING`. IRC identifiers
/// are not compared with Unicode or plain ASCII lowercase: RFC1459 also treats
/// `[]\\` as equivalent to `{}|`, and (in the non-strict form) `^` as `~`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMapping {
    Ascii,
    Rfc1459,
    StrictRfc1459,
}

impl Default for CaseMapping {
    fn default() -> Self {
        Self::Rfc1459
    }
}

impl CaseMapping {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ascii => "ascii",
            Self::Rfc1459 => "rfc1459",
            Self::StrictRfc1459 => "strict-rfc1459",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("ascii") {
            Some(Self::Ascii)
        } else if value.eq_ignore_ascii_case("rfc1459") {
            Some(Self::Rfc1459)
        } else if value.eq_ignore_ascii_case("strict-rfc1459") {
            Some(Self::StrictRfc1459)
        } else {
            None
        }
    }

    /// Returns the canonical key used to compare IRC nicknames and channels.
    pub fn fold(self, value: &str) -> String {
        value
            .chars()
            .map(|c| match c {
                'A'..='Z' => c.to_ascii_lowercase(),
                '[' if self != Self::Ascii => '{',
                ']' if self != Self::Ascii => '}',
                '\\' if self != Self::Ascii => '|',
                '^' if self == Self::Rfc1459 => '~',
                _ => c,
            })
            .collect()
    }

    pub fn eq(self, left: &str, right: &str) -> bool {
        self.fold(left) == self.fold(right)
    }
}

fn wildcard_ascii_case(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let text: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (mut p, mut t, mut star, mut retry) = (0usize, 0usize, None, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            retry += 1;
            t = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Server capabilities learned from RPL_ISUPPORT (005).
#[derive(Debug, Clone)]
pub struct Isupport {
    /// Human-readable live network name advertised by `NETWORK=`.
    pub network: Option<String>,
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
    /// Max mode params per `/mode` line (ISUPPORT `MODES`), for `$modespl`.
    pub modes: u32,
    /// Server advertises the valueless WHOX token (extended WHO replies).
    pub whox: bool,
    /// Nick/channel comparison rules. RFC1459 is the protocol default when the
    /// server does not advertise CASEMAPPING.
    pub case_mapping: CaseMapping,
    /// Prefixes which may address only channel members of a given status, e.g.
    /// `@#channel` when `STATUSMSG=@+`.
    pub status_msg: String,
}

impl Default for Isupport {
    fn default() -> Self {
        Isupport {
            network: None,
            prefix_modes: vec![('q', '~'), ('a', '&'), ('o', '@'), ('h', '%'), ('v', '+')],
            chan_types: "#&!+".to_string(),
            chanmodes_a: "beI".to_string(),
            chanmodes_b: "k".to_string(),
            chanmodes_c: "l".to_string(),
            chanmodes_d: "imnpstrS".to_string(),
            modes: 3,
            whox: false,
            case_mapping: CaseMapping::default(),
            status_msg: String::new(),
        }
    }
}

impl Isupport {
    pub fn casefold(&self, value: &str) -> String {
        self.case_mapping.fold(value)
    }

    pub fn names_equal(&self, left: &str, right: &str) -> bool {
        self.case_mapping.eq(left, right)
    }

    pub fn prefix_for_mode(&self, mode: char) -> Option<char> {
        self.prefix_modes
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, p)| *p)
    }

    pub fn mode_for_prefix(&self, prefix: char) -> Option<char> {
        self.prefix_modes
            .iter()
            .find(|(_, p)| *p == prefix)
            .map(|(m, _)| *m)
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
        // Driven entirely by the server's advertised CHANTYPES. IRCX servers list
        // their '%#'/'%&' prefixes here (e.g. CHANTYPES=%#), so no client-side
        // special-casing is needed.
        name.chars()
            .next()
            .is_some_and(|c| self.chan_types.contains(c))
    }

    /// Returns the bare channel represented by a normal channel target or a
    /// STATUSMSG target. A leading status character is stripped only when the
    /// remainder is itself a valid channel, so a real `+channel` remains valid.
    pub fn channel_target<'a>(&self, target: &'a str) -> Option<&'a str> {
        // IRCX uses composite `%#name` / `%&name` channel names. Scripts can run
        // during registration (before the server's CHANTYPES=%# token arrives),
        // so retain this unambiguous IRCX form as a compatibility fallback.
        if target.starts_with("%#") || target.starts_with("%&") {
            return Some(target);
        }
        let mut bare = target;
        while let Some(first) = bare.chars().next() {
            if self.status_msg.contains(first) {
                bare = &bare[first.len_utf8()..];
            } else {
                break;
            }
        }
        if bare != target && self.is_channel(bare) {
            Some(bare)
        } else if self.is_channel(target) {
            Some(target)
        } else {
            None
        }
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
        if let Some(v) = token.strip_prefix("NETWORK=") {
            if !v.trim().is_empty() {
                self.network = Some(v.trim().to_string());
            }
        } else if let Some(v) = token.strip_prefix("PREFIX=") {
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
        } else if token == "WHOX" || token.starts_with("WHOX=") {
            self.whox = true;
        } else if let Some(v) = token.strip_prefix("CASEMAPPING=") {
            if let Some(mapping) = CaseMapping::parse(v) {
                self.case_mapping = mapping;
            }
        } else if let Some(v) = token.strip_prefix("STATUSMSG=") {
            self.status_msg = v.to_string();
        } else if token == "-CASEMAPPING" {
            self.case_mapping = CaseMapping::default();
        } else if token == "-STATUSMSG" {
            self.status_msg.clear();
        } else if let Some(v) = token.strip_prefix("MODES=") {
            if let Ok(n) = v.parse::<u32>() {
                self.modes = n;
            }
        }
    }
}

/// Rich fields associated with one Internal Address List entry. The address
/// remains in `SessionState::ial` for compatibility with existing lookup code;
/// these fields are populated by WHOX and IRCv3 notifications when available.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IalInfo {
    pub account: String,
    pub away: Option<bool>,
    pub gecos: String,
    pub id: String,
    pub marks: BTreeMap<String, String>,
}

/// Read-only rich IAL entry exposed to the script snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IalView {
    pub nick: String,
    pub address: String,
    pub account: String,
    pub away: Option<bool>,
    pub gecos: String,
    pub id: String,
    pub marks: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct ChannelState {
    pub topic: Option<String>,
    /// Active non-list channel modes. The value is empty for flag modes and
    /// contains the current argument for modes such as `+k` and `+l`.
    pub modes: BTreeMap<char, String>,
    /// nick (case-sensitive as seen) -> prefix string, e.g. "@+".
    pub members: BTreeMap<String, String>,
    /// Last observed channel activity per member as a Unix timestamp. This is
    /// kept separate from `members` to preserve the existing roster shape.
    pub member_activity: BTreeMap<String, u64>,
    /// Active `+b` ban masks (from live MODE and RPL_BANLIST), for `isban`.
    pub bans: std::collections::BTreeSet<String>,
    /// True while a `+b` listing is in flight (RPL_BANLIST until its ENDOF),
    /// for `$chan().banlist` / `$inmode`.
    pub in_mode: bool,
    /// True while a `/who` reply is in flight, for `$chan().inwho` / `$inwho`.
    pub in_who: bool,
    pub ban_entries: BTreeMap<String, ListEntry>,
    pub except_entries: BTreeMap<String, ListEntry>,
    pub invite_entries: BTreeMap<String, ListEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub mask: String,
    pub by: String,
    pub ctime: u64,
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

    /// mIRC's `$chan(#).mode` form: active mode letters followed by any mode
    /// arguments in the same order. Prefix/list modes are tracked elsewhere
    /// and are deliberately absent from this string.
    pub fn mode_string(&self) -> String {
        if self.modes.is_empty() {
            return String::new();
        }
        let letters: String = self.modes.keys().collect();
        let args = self
            .modes
            .values()
            .filter(|value| !value.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if args.is_empty() {
            format!("+{letters}")
        } else {
            format!("+{letters} {}", args.join(" "))
        }
    }
}

#[derive(Debug, Default)]
pub struct SessionState {
    pub nick: String,
    /// This connection's id (the StateStore key), surfaced in the snapshot for `$cid`.
    pub server_id: String,
    pub channels: BTreeMap<String, ChannelState>,
    pub isupport: Isupport,
    /// Connection facts (seeded from the profile) for $port/$ssl/$anick/$fullname.
    pub server_port: u16,
    pub tls: bool,
    pub server_ip: String,
    pub server_target: String,
    pub tls_version: String,
    pub tls_peer_certificate: Vec<u8>,
    pub tls_cert_valid: bool,
    /// Path to this connection's client certificate, for `$sslcertsha1` /
    /// `$sslcertsha256`. Empty when no client certificate is configured.
    pub tls_client_cert_path: String,
    pub alt_nick: String,
    /// Our configured main (primary) nick, for `$mnick`.
    pub main_nick: String,
    pub realname: String,
    /// Our own user modes (e.g. "iwx"), tracked from MODE messages on our nick.
    pub user_mode: String,
    /// Whether we are marked away (from RPL_UNAWAY/RPL_NOWAWAY).
    pub away: bool,
    /// Unix time we connected (RPL_WELCOME) / went away, for $online / $awaytime.
    pub connect_time: u64,
    pub away_time: u64,
    /// Our own away message (captured from the outgoing AWAY command), for `$awaymsg`.
    pub away_msg: String,
    /// Set once RPL_WELCOME is received.
    pub registered: bool,
    /// How many alternative nicks we've tried during registration.
    pub nick_attempts: u32,
    /// Internal address list: lowercase nick -> full `nick!user@host`, learned
    /// from message prefixes and `userhost-in-names` NAMES replies.
    pub ial: BTreeMap<String, String>,
    /// WHOX/notification fields and `/ialmark` data for entries in `ial`.
    pub ial_info: BTreeMap<String, IalInfo>,
    /// `/ial off` is per-session and defaults to false (IAL enabled).
    pub ial_disabled: bool,
    pub links: Vec<LinkView>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkView {
    pub addr: String,
    pub ip: String,
    pub level: u32,
    pub info: String,
}

impl SessionState {
    fn matching_channel_key(&self, channel: &str) -> Option<String> {
        self.channels
            .keys()
            .find(|known| self.isupport.names_equal(known, channel))
            .cloned()
    }

    pub fn channel_name<'a>(&'a self, channel: &str) -> Option<&'a str> {
        self.channels
            .keys()
            .find(|known| self.isupport.names_equal(known, channel))
            .map(String::as_str)
    }

    pub fn channel(&self, channel: &str) -> Option<&ChannelState> {
        let key = self
            .channels
            .keys()
            .find(|known| self.isupport.names_equal(known, channel))?;
        self.channels.get(key)
    }

    pub fn channel_mut(&mut self, channel: &str) -> Option<&mut ChannelState> {
        let key = self.matching_channel_key(channel)?;
        self.channels.get_mut(&key)
    }

    pub fn remove_channel(&mut self, channel: &str) -> Option<ChannelState> {
        let key = self.matching_channel_key(channel)?;
        self.channels.remove(&key)
    }

    fn matching_member_key<V>(
        mapping: CaseMapping,
        members: &BTreeMap<String, V>,
        nick: &str,
    ) -> Option<String> {
        members
            .keys()
            .find(|known| mapping.eq(known, nick))
            .cloned()
    }

    pub fn has_member(&self, channel: &str, nick: &str) -> bool {
        self.channel(channel).is_some_and(|ch| {
            ch.members
                .keys()
                .any(|known| self.isupport.names_equal(known, nick))
        })
    }

    pub fn upsert_member(&mut self, channel: &str, nick: &str, prefixes: String) {
        let channel_key = self
            .matching_channel_key(channel)
            .unwrap_or_else(|| channel.to_string());
        let mapping = self.isupport.case_mapping;
        let members = &mut self
            .channels
            .entry(channel_key.clone())
            .or_default()
            .members;
        if let Some(old) = Self::matching_member_key(mapping, members, nick) {
            members.remove(&old);
        }
        members.insert(nick.to_string(), prefixes);
        let ch = self.channels.get_mut(&channel_key).unwrap();
        let old_activity = Self::matching_member_key(mapping, &ch.member_activity, nick)
            .and_then(|old| ch.member_activity.remove(&old));
        ch.member_activity
            .insert(nick.to_string(), old_activity.unwrap_or_else(unix_now));
    }

    pub fn remove_member(&mut self, channel: &str, nick: &str) {
        let mapping = self.isupport.case_mapping;
        if let Some(ch) = self.channel_mut(channel) {
            if let Some(key) = Self::matching_member_key(mapping, &ch.members, nick) {
                ch.members.remove(&key);
            }
            if let Some(key) = Self::matching_member_key(mapping, &ch.member_activity, nick) {
                ch.member_activity.remove(&key);
            }
        }
    }

    /// Removes a nick from every channel, returning the channels they were in.
    pub fn remove_member_everywhere(&mut self, nick: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mapping = self.isupport.case_mapping;
        for (name, ch) in self.channels.iter_mut() {
            if let Some(key) = Self::matching_member_key(mapping, &ch.members, nick) {
                if ch.members.remove(&key).is_some() {
                    found.push(name.clone());
                }
            }
            if let Some(key) = Self::matching_member_key(mapping, &ch.member_activity, nick) {
                ch.member_activity.remove(&key);
            }
        }
        found
    }

    /// Renames a nick across all channels (preserving prefixes).
    pub fn rename_member(&mut self, old: &str, new: &str) {
        let mapping = self.isupport.case_mapping;
        for ch in self.channels.values_mut() {
            if let Some(key) = Self::matching_member_key(mapping, &ch.members, old) {
                if let Some(prefix) = ch.members.remove(&key) {
                    ch.members.insert(new.to_string(), prefix);
                }
            }
            if let Some(key) = Self::matching_member_key(mapping, &ch.member_activity, old) {
                if let Some(last) = ch.member_activity.remove(&key) {
                    ch.member_activity.insert(new.to_string(), last);
                }
            }
        }
    }

    /// Records a message/action from `nick` in `channel` for `$nick().idle`.
    pub fn touch_member(&mut self, channel: &str, nick: &str) {
        let mapping = self.isupport.case_mapping;
        if let Some(ch) = self.channel_mut(channel) {
            let Some(member) = Self::matching_member_key(mapping, &ch.members, nick) else {
                return;
            };
            if let Some(key) = Self::matching_member_key(mapping, &ch.member_activity, nick) {
                ch.member_activity.remove(&key);
            }
            ch.member_activity.insert(member, unix_now());
        }
    }

    pub fn prune_member_activity(&mut self, channel: &str) {
        let mapping = self.isupport.case_mapping;
        if let Some(ch) = self.channel_mut(channel) {
            let members = ch.members.keys().cloned().collect::<Vec<_>>();
            ch.member_activity
                .retain(|nick, _| members.iter().any(|member| mapping.eq(member, nick)));
        }
    }

    /// Records a `nick!user@host` address in the internal address list.
    pub fn record_address(&mut self, nick: &str, address: String) -> bool {
        if self.ial_disabled {
            return false;
        }
        let key = self.isupport.casefold(nick);
        let changed = self.ial.get(&key) != Some(&address);
        self.ial.insert(key.clone(), address);
        self.ial_info.entry(key).or_default();
        changed
    }

    pub fn set_ial_enabled(&mut self, enabled: bool) {
        self.ial_disabled = !enabled;
        if !enabled {
            self.ial.clear();
            self.ial_info.clear();
        }
    }

    pub fn clear_ial(&mut self, nick: Option<&str>) {
        if let Some(nick) = nick.filter(|nick| !nick.is_empty()) {
            let key = self.isupport.casefold(nick);
            self.ial.remove(&key);
            self.ial_info.remove(&key);
        } else {
            self.ial.clear();
            self.ial_info.clear();
        }
    }

    /// Removes a user's IAL entry once they no longer share any channel with us.
    pub fn prune_ial_nick(&mut self, nick: &str) {
        let present = self.channels.values().any(|channel| {
            channel
                .members
                .keys()
                .any(|member| self.isupport.names_equal(member, nick))
        });
        if !present {
            self.clear_ial(Some(nick));
        }
    }

    /// Drops every stale entry after we ourselves leave a channel.
    pub fn prune_ial(&mut self) {
        let present: std::collections::BTreeSet<String> = self
            .channels
            .values()
            .flat_map(|channel| {
                channel
                    .members
                    .keys()
                    .map(|nick| self.isupport.casefold(nick))
            })
            .collect();
        self.ial.retain(|nick, _| present.contains(nick));
        self.ial_info.retain(|nick, _| present.contains(nick));
    }

    pub fn rename_ial(&mut self, old: &str, new: &str) {
        let old_key = self.isupport.casefold(old);
        let new_key = self.isupport.casefold(new);
        if let Some(address) = self.ial.remove(&old_key) {
            let suffix = address
                .split_once('!')
                .map(|(_, suffix)| suffix)
                .unwrap_or("");
            let address = if suffix.is_empty() {
                new.to_string()
            } else {
                format!("{new}!{suffix}")
            };
            self.ial.insert(new_key.clone(), address);
        }
        if let Some(info) = self.ial_info.remove(&old_key) {
            self.ial_info.insert(new_key, info);
        }
    }

    /// Rebuilds canonical IAL keys after a server changes/announces its
    /// CASEMAPPING. The display nick in each stored address lets us avoid
    /// carrying keys folded under the previous mapping.
    pub fn reindex_ial(&mut self) {
        let old_ial = std::mem::take(&mut self.ial);
        let mut old_info = std::mem::take(&mut self.ial_info);
        for (old_key, address) in old_ial {
            let nick = address
                .split_once('!')
                .map(|(nick, _)| nick)
                .unwrap_or(&old_key);
            let new_key = self.isupport.casefold(nick);
            self.ial.insert(new_key.clone(), address);
            if let Some(info) = old_info.remove(&old_key) {
                self.ial_info.insert(new_key, info);
            }
        }
    }

    pub fn update_ial_account(&mut self, nick: &str, account: &str) {
        let key = self.isupport.casefold(nick);
        if self.ial_disabled || !self.ial.contains_key(&key) {
            return;
        }
        self.ial_info.entry(key).or_default().account = if account == "0" || account == "*" {
            String::new()
        } else {
            account.to_string()
        };
    }

    pub fn update_ial_away(&mut self, nick: &str, away: bool) {
        let key = self.isupport.casefold(nick);
        if self.ial_disabled || !self.ial.contains_key(&key) {
            return;
        }
        self.ial_info.entry(key).or_default().away = Some(away);
    }

    pub fn update_ial_gecos(&mut self, nick: &str, gecos: &str) {
        let key = self.isupport.casefold(nick);
        if self.ial_disabled || !self.ial.contains_key(&key) {
            return;
        }
        self.ial_info.entry(key).or_default().gecos = gecos.to_string();
    }

    pub fn update_ial_chghost(&mut self, nick: &str, user: &str, host: &str) {
        if self.ial_disabled || !self.ial.contains_key(&self.isupport.casefold(nick)) {
            return;
        }
        self.record_address(nick, format!("{nick}!{user}@{host}"));
    }

    pub fn update_ial_whox(
        &mut self,
        nick: &str,
        user: &str,
        host: &str,
        account: &str,
        away: bool,
        gecos: &str,
    ) {
        self.record_address(nick, format!("{nick}!{user}@{host}"));
        if self.ial_disabled {
            return;
        }
        let info = self
            .ial_info
            .entry(self.isupport.casefold(nick))
            .or_default();
        info.account = if account == "0" || account == "*" {
            String::new()
        } else {
            account.to_string()
        };
        info.away = Some(away);
        info.gecos = gecos.to_string();
    }

    /// Adds/removes one named `/ialmark`. A wildcard remove applies to every
    /// matching mark name; an empty name means mIRC's `default` mark.
    pub fn update_ial_mark(
        &mut self,
        nick: &str,
        name: &str,
        text: &str,
        remove: bool,
        wildcard: bool,
    ) {
        let key = self.isupport.casefold(nick);
        let Some(info) = self.ial_info.get_mut(&key) else {
            return;
        };
        let name = if name.is_empty() { "default" } else { name };
        if remove {
            if wildcard {
                info.marks
                    .retain(|mark, _| !wildcard_ascii_case(name, mark));
            } else {
                if let Some(existing) = info
                    .marks
                    .keys()
                    .find(|mark| mark.eq_ignore_ascii_case(name))
                    .cloned()
                {
                    info.marks.remove(&existing);
                }
            }
        } else {
            if let Some(existing) = info
                .marks
                .keys()
                .find(|mark| mark.eq_ignore_ascii_case(name))
                .cloned()
            {
                info.marks.remove(&existing);
            }
            info.marks.insert(name.to_string(), text.to_string());
        }
    }

    /// Adds (`adding`) or removes a `+b` ban mask for a channel.
    pub fn set_channel_list(
        &mut self,
        channel: &str,
        mode: char,
        mask: &str,
        by: &str,
        ctime: u64,
        adding: bool,
    ) {
        let channel_key = self
            .matching_channel_key(channel)
            .unwrap_or_else(|| channel.to_string());
        let ch = self.channels.entry(channel_key).or_default();
        let entries = match mode {
            'b' => {
                if adding {
                    ch.bans.insert(mask.to_string());
                } else {
                    ch.bans.remove(mask);
                }
                &mut ch.ban_entries
            }
            'e' => &mut ch.except_entries,
            'I' => &mut ch.invite_entries,
            _ => return,
        };
        if adding {
            entries.insert(
                mask.to_string(),
                ListEntry {
                    mask: mask.to_string(),
                    by: by.to_string(),
                    ctime,
                },
            );
        } else {
            entries.remove(mask);
        }
    }

    /// Adds or removes one non-list channel mode. Member prefix modes and list
    /// modes are handled by their dedicated state stores.
    pub fn set_channel_mode(
        &mut self,
        channel: &str,
        mode: char,
        argument: Option<&str>,
        adding: bool,
    ) {
        let Some(channel_key) = self.matching_channel_key(channel) else {
            return;
        };
        let Some(ch) = self.channels.get_mut(&channel_key) else {
            return;
        };
        if adding {
            ch.modes
                .insert(mode, argument.unwrap_or_default().to_string());
        } else {
            ch.modes.remove(&mode);
        }
    }

    /// Applies a privilege mode change to a member's prefixes.
    pub fn apply_prefix_mode(&mut self, channel: &str, nick: &str, mode: char, adding: bool) {
        let Some(prefix_char) = self.isupport.prefix_for_mode(mode) else {
            return;
        };
        let order = self.isupport.clone();
        let mapping = self.isupport.case_mapping;
        if let Some(ch) = self.channel_mut(channel) {
            if let Some(member_key) = Self::matching_member_key(mapping, &ch.members, nick) {
                let Some(prefixes) = ch.members.get_mut(&member_key) else {
                    return;
                };
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
    pub topic: String,
    pub mode: String,
    pub key: String,
    pub limit: String,
    /// Member nicks (without prefixes), in roster order.
    pub nicks: Vec<String>,
    /// (nick, prefix chars) per member, e.g. `("bob", "@")`. Powers the
    /// `isop`/`ishop`/`isvoice`/`ison`/`isreg`/... condition operators.
    pub members: Vec<(String, String)>,
    /// (nick, last activity Unix timestamp), for `$nick(#,N).idle`.
    pub member_activity: Vec<(String, u64)>,
    /// Active `+b` ban masks, for the `isban` operator.
    pub bans: Vec<String>,
    /// Listing-in-flight flags, for `$chan().banlist` / `$chan().inwho`.
    pub in_mode: bool,
    pub in_who: bool,
    pub ban_entries: Vec<ListEntry>,
    pub except_entries: Vec<ListEntry>,
    pub invite_entries: Vec<ListEntry>,
}

/// A snapshot of a connection's channel/member state, shared with the script
/// engine so identifiers like `$chan(N)` and `$nick(#,N)` can resolve.
#[derive(Debug, Clone)]
pub struct StateSnapshot {
    pub nick: String,
    /// The connection's id (StateStore key), so `$cid` can map it to its number.
    pub server_id: String,
    pub channels: Vec<ChannelView>,
    /// (lowercase nick, full `nick!user@host`) pairs for `$address`/`$ial`.
    pub ial: Vec<(String, String)>,
    pub ial_enabled: bool,
    pub ial_info: Vec<IalView>,
    /// ISUPPORT tokens for `$prefix` / `$chanmodes` / `$chantypes`.
    pub isupport: Isupport,
    /// Connection facts for `$port` / `$ssl` / `$anick` / `$fullname`.
    pub server_port: u16,
    pub tls: bool,
    pub server_ip: String,
    pub server_target: String,
    pub tls_version: String,
    pub tls_peer_certificate: Vec<u8>,
    pub tls_cert_valid: bool,
    /// Path to this connection's client certificate, for `$sslcertsha1` /
    /// `$sslcertsha256`. Empty when no client certificate is configured.
    pub tls_client_cert_path: String,
    pub alt_nick: String,
    /// Our configured main (primary) nick, for `$mnick`.
    pub main_nick: String,
    pub realname: String,
    /// Our own user modes (e.g. "iwx") for `$usermode`.
    pub user_mode: String,
    /// Whether we are marked away, for `$away`.
    pub away: bool,
    /// Unix times for `$online` (connect) and `$awaytime`.
    pub connect_time: u64,
    pub away_time: u64,
    /// Our own away message, for `$awaymsg`.
    pub away_msg: String,
    /// During KICK/PART/QUIT handlers mIRC intentionally exposes the old
    /// nicklist/IAL until `/updatenl` is called. The updated snapshot and one
    /// shared activation flag preserve that behaviour across handlers.
    pub pending_nicklist_update: Option<Arc<PendingNicklistUpdate>>,
    pub links: Vec<LinkView>,
}

#[derive(Debug)]
pub struct PendingNicklistUpdate {
    pub updated: Arc<StateSnapshot>,
    activated: AtomicBool,
}

impl PendingNicklistUpdate {
    pub fn activate(&self) {
        self.activated.store(true, Ordering::Release);
    }

    pub fn is_active(&self) -> bool {
        self.activated.load(Ordering::Acquire)
    }
}

impl StateSnapshot {
    pub fn with_pending_nicklist_update(mut self, updated: StateSnapshot) -> Self {
        self.pending_nicklist_update = Some(Arc::new(PendingNicklistUpdate {
            updated: Arc::new(updated),
            activated: AtomicBool::new(false),
        }));
        self
    }
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            nick: String::new(),
            server_id: String::new(),
            channels: Vec::new(),
            ial: Vec::new(),
            ial_enabled: true,
            ial_info: Vec::new(),
            isupport: Isupport::default(),
            server_port: 0,
            tls: false,
            server_ip: String::new(),
            server_target: String::new(),
            tls_version: String::new(),
            tls_client_cert_path: String::new(),
            tls_peer_certificate: Vec::new(),
            tls_cert_valid: false,
            alt_nick: String::new(),
            main_nick: String::new(),
            realname: String::new(),
            user_mode: String::new(),
            away: false,
            connect_time: 0,
            away_time: 0,
            away_msg: String::new(),
            pending_nicklist_update: None,
            links: Vec::new(),
        }
    }
}

impl SessionState {
    /// Builds a snapshot for the script engine.
    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            nick: self.nick.clone(),
            server_id: self.server_id.clone(),
            channels: self
                .channels
                .iter()
                .map(|(name, ch)| ChannelView {
                    name: name.clone(),
                    topic: ch.topic.clone().unwrap_or_default(),
                    mode: ch.mode_string(),
                    key: ch.modes.get(&'k').cloned().unwrap_or_default(),
                    limit: ch.modes.get(&'l').cloned().unwrap_or_default(),
                    nicks: ch.members.keys().cloned().collect(),
                    members: ch
                        .members
                        .iter()
                        .map(|(n, p)| (n.clone(), p.clone()))
                        .collect(),
                    member_activity: ch
                        .member_activity
                        .iter()
                        .map(|(nick, last)| (nick.clone(), *last))
                        .collect(),
                    bans: ch.bans.iter().cloned().collect(),
                    in_mode: ch.in_mode,
                    in_who: ch.in_who,
                    ban_entries: ch.ban_entries.values().cloned().collect(),
                    except_entries: ch.except_entries.values().cloned().collect(),
                    invite_entries: ch.invite_entries.values().cloned().collect(),
                })
                .collect(),
            ial: self
                .ial
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ial_enabled: !self.ial_disabled,
            ial_info: self
                .ial
                .iter()
                .map(|(nick, address)| {
                    let info = self.ial_info.get(nick).cloned().unwrap_or_default();
                    IalView {
                        nick: nick.clone(),
                        address: address.clone(),
                        account: info.account,
                        away: info.away,
                        gecos: info.gecos,
                        id: info.id,
                        marks: info.marks.into_iter().collect(),
                    }
                })
                .collect(),
            isupport: self.isupport.clone(),
            server_port: self.server_port,
            tls: self.tls,
            server_ip: self.server_ip.clone(),
            server_target: self.server_target.clone(),
            tls_version: self.tls_version.clone(),
            tls_peer_certificate: self.tls_peer_certificate.clone(),
            tls_cert_valid: self.tls_cert_valid,
            tls_client_cert_path: self.tls_client_cert_path.clone(),
            alt_nick: self.alt_nick.clone(),
            main_nick: self.main_nick.clone(),
            realname: self.realname.clone(),
            user_mode: self.user_mode.clone(),
            away: self.away,
            connect_time: self.connect_time,
            away_time: self.away_time,
            away_msg: self.away_msg.clone(),
            pending_nicklist_update: None,
            links: self.links.clone(),
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
        self.map
            .lock()
            .unwrap()
            .insert(server_id.to_string(), Arc::new(snap));
    }

    pub fn get(&self, server_id: &str) -> Arc<StateSnapshot> {
        self.map
            .lock()
            .unwrap()
            .get(server_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn remove(&self, server_id: &str) {
        self.map.lock().unwrap().remove(server_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_network_as_the_live_display_name() {
        let mut support = Isupport::default();
        support.parse_token("NETWORK=IRC7");
        assert_eq!(support.network.as_deref(), Some("IRC7"));
        support.parse_token("NETWORK=");
        assert_eq!(support.network.as_deref(), Some("IRC7"));
    }

    #[test]
    fn splits_known_prefixes() {
        let s = Isupport::default();
        assert_eq!(
            s.split_prefixes("@+bob"),
            ("@+".to_string(), "bob".to_string())
        );
        assert_eq!(
            s.split_prefixes("alice"),
            (String::new(), "alice".to_string())
        );
    }

    #[test]
    fn orders_prefixes_by_rank() {
        let s = Isupport::default();
        assert_eq!(
            s.split_prefixes("+@carol"),
            ("@+".to_string(), "carol".to_string())
        );
    }

    #[test]
    fn parses_nonstandard_prefix_and_chantypes() {
        let mut s = Isupport::default();
        s.parse_token("PREFIX=(qov).@+");
        s.parse_token("CHANTYPES=%#");
        s.parse_token("WHOX");
        // Owner is now '.', and '%' starts a channel.
        assert_eq!(s.prefix_for_mode('q'), Some('.'));
        assert!(s.is_channel("%room"));
        assert!(s.is_channel("#room"));
        assert!(!s.is_channel("nick"));
        assert_eq!(
            s.split_prefixes(".@dave"),
            (".@".to_string(), "dave".to_string())
        );
        assert!(s.whox);
    }

    #[test]
    fn parses_and_applies_server_casemapping() {
        let mut s = Isupport::default();
        assert!(s.names_equal("Nick[\\^", "nick{|~"));

        s.parse_token("CASEMAPPING=strict-rfc1459");
        assert!(s.names_equal("Nick[\\", "nick{|"));
        assert!(!s.names_equal("nick^", "nick~"));

        s.parse_token("CASEMAPPING=ascii");
        assert!(s.names_equal("Nick", "nick"));
        assert!(!s.names_equal("nick[", "nick{"));
    }

    #[test]
    fn resolves_statusmsg_without_misclassifying_real_plus_channels() {
        let mut s = Isupport::default();
        s.parse_token("CHANTYPES=#&+");
        s.parse_token("STATUSMSG=@+");
        assert_eq!(s.channel_target("@#room"), Some("#room"));
        assert_eq!(s.channel_target("+#room"), Some("#room"));
        assert_eq!(s.channel_target("+local"), Some("+local"));
        assert_eq!(s.channel_target("nick"), None);

        // IRCX composite channel names must also work before the server has
        // delivered its CHANTYPES=%# token (aliases may run during registration).
        let initial = Isupport::default();
        assert_eq!(initial.channel_target("%#Lobby"), Some("%#Lobby"));
        assert_eq!(initial.channel_target("%&Staff"), Some("%&Staff"));
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
    fn snapshots_channel_topic_modes_key_and_limit() {
        let mut s = SessionState::default();
        s.upsert_member("#Room[", "Alice", "@".into());
        s.channel_mut("#room{").unwrap().topic = Some("Welcome".into());
        s.set_channel_mode("#room{", 'n', None, true);
        s.set_channel_mode("#room{", 'k', Some("secret"), true);
        s.set_channel_mode("#room{", 'l', Some("25"), true);

        let snap = s.snapshot();
        let channel = &snap.channels[0];
        assert_eq!(channel.topic, "Welcome");
        assert_eq!(channel.mode, "+kln secret 25");
        assert_eq!(channel.key, "secret");
        assert_eq!(channel.limit, "25");

        s.set_channel_mode("#ROOM[", 'k', None, false);
        assert_eq!(s.snapshot().channels[0].mode, "+ln 25");
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

    #[test]
    fn membership_and_ial_use_rfc1459_keys() {
        let mut s = SessionState::default();
        s.upsert_member("#Room[", "User^", "@".into());
        s.upsert_member("#room{", "user~", "+".into());
        assert_eq!(s.channels.len(), 1);
        assert_eq!(s.channel("#ROOM{").unwrap().members.len(), 1);
        assert!(s.has_member("#room[", "USER^"));

        s.record_address("User[", "User[!u@h".into());
        assert!(s.ial.contains_key("user{"));
        s.update_ial_account("user{", "account");
        assert_eq!(s.ial_info["user{"].account, "account");

        s.remove_member("#ROOM{", "USER^");
        assert!(!s.has_member("#room[", "user~"));
    }

    #[test]
    fn ial_toggle_metadata_marks_and_snapshot() {
        assert!(StateSnapshot::default().ial_enabled);
        let mut s = SessionState::default();
        s.upsert_member("#room", "Alice", String::new());
        s.update_ial_whox("Alice", "user", "host.test", "account", true, "Alice Real");
        s.update_ial_mark("Alice", "note", "trusted", false, false);
        s.update_ial_mark("Alice", "NOTE", "trusted again", false, false);

        let snap = s.snapshot();
        assert!(snap.ial_enabled);
        assert_eq!(
            snap.ial,
            vec![("alice".into(), "Alice!user@host.test".into())]
        );
        assert_eq!(snap.ial_info[0].account, "account");
        assert_eq!(snap.ial_info[0].away, Some(true));
        assert_eq!(snap.ial_info[0].gecos, "Alice Real");
        assert_eq!(
            snap.ial_info[0].marks,
            vec![("NOTE".into(), "trusted again".into())]
        );

        s.update_ial_mark("Alice", "n*", "", true, true);
        assert!(s.ial_info["alice"].marks.is_empty());
        s.set_ial_enabled(false);
        s.record_address("Bob", "Bob!u@h".into());
        assert!(s.ial.is_empty());
        assert!(!s.snapshot().ial_enabled);
        s.set_ial_enabled(true);
        s.record_address("Bob", "Bob!u@h".into());
        assert!(s.ial.contains_key("bob"));
    }

    #[test]
    fn ial_rename_and_membership_pruning_preserve_integrity() {
        let mut s = SessionState::default();
        s.upsert_member("#a", "Alice", String::new());
        s.upsert_member("#b", "Alice", String::new());
        s.record_address("Alice", "Alice!u@host".into());
        s.update_ial_mark("Alice", "default", "kept", false, false);
        s.rename_member("Alice", "Alicia");
        s.rename_ial("Alice", "Alicia");
        assert_eq!(s.ial["alicia"], "Alicia!u@host");
        assert_eq!(s.ial_info["alicia"].marks["default"], "kept");

        s.remove_member("#a", "Alicia");
        s.prune_ial_nick("Alicia");
        assert!(s.ial.contains_key("alicia"));
        s.remove_member("#b", "Alicia");
        s.prune_ial_nick("Alicia");
        assert!(!s.ial.contains_key("alicia"));
    }
}
