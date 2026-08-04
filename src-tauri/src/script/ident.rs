//! Built-in mSL identifiers ($me, $nick, $rand, string functions, …).

use std::time::{SystemTime, UNIX_EPOCH};

use super::eval::{eval_bool_public, wildcard_match, wildcard_match_cs, Runtime, SOCK_BR_KEY};
use sha2::Digest; // brings the Digest trait into scope for Md5/Sha1/Sha2 too

/// Evaluates `$name(args...)` with an optional `.property` suffix (empty when
/// none). Args are already expanded.
pub fn eval_ident(rt: &mut Runtime, name: &str, args: &[String], prop: &str) -> String {
    rt.purge_expired();
    let a = |i: usize| args.get(i).cloned().unwrap_or_default();
    match name.to_ascii_lowercase().as_str() {
        // Full local path in on FILESENT/FILERCVD/GETFAIL/SENDFAIL.
        "filename" => rt.event.filename.clone(),
        "dccid" => rt.event.dcc_id.clone(),
        "raddress" => rt.event.dns_query.clone(),
        "dns" => {
            let n = a(0).parse::<usize>().unwrap_or(0);
            if n == 0 {
                rt.event.dns_ips.len().to_string()
            } else if n <= rt.event.dns_ips.len() {
                match prop.to_ascii_lowercase().as_str() {
                    "ip" => rt.event.dns_ips[n - 1].clone(),
                    "addr" | "" => rt.event.dns_query.clone(),
                    "nick" => String::new(),
                    _ => String::new(),
                }
            } else {
                String::new()
            }
        }
        "dccport" => rt
            .dcc
            .server_port()
            .map(|port| port.to_string())
            .unwrap_or_default(),
        "bindip" => rt.dcc.bind_ip(),
        "passivedcc" => if rt.dcc.passive() { "on" } else { "off" }.to_string(),
        "me" => rt.my_nick.to_string(),
        "pnick" => rt.event.pnick.clone(),
        "mnick" => rt.state.main_nick.clone(),
        "nick" => {
            // $nick = event nick; $nick(#chan,N/nick[,include[,exclude]])
            // reads the live nickname list using the server's CASEMAPPING.
            if args.len() >= 2 {
                let Some(channel) = find_channel(&rt.state, &a(0)) else {
                    return String::new();
                };
                let include = a(2);
                let exclude = a(3);
                let member_rows = if channel.members.is_empty() {
                    channel
                        .nicks
                        .iter()
                        .map(|nick| (nick.as_str(), ""))
                        .collect::<Vec<_>>()
                } else {
                    channel
                        .members
                        .iter()
                        .map(|(nick, prefixes)| (nick.as_str(), prefixes.as_str()))
                        .collect::<Vec<_>>()
                };
                let members = member_rows
                    .into_iter()
                    .filter(|(_, prefixes)| {
                        nick_filter_matches(&rt.state.isupport, prefixes, &include)
                            && (exclude.is_empty()
                                || !nick_filter_matches(&rt.state.isupport, prefixes, &exclude))
                    })
                    .collect::<Vec<_>>();
                let selector = a(1);
                let member = match selector.parse::<usize>() {
                    Ok(0) => return members.len().to_string(),
                    Ok(n) => members.get(n - 1).copied(),
                    Err(_) => members
                        .into_iter()
                        .find(|(nick, _)| rt.state.isupport.names_equal(nick, &selector)),
                };
                member.map_or_else(String::new, |(nick, prefixes)| {
                    let last_activity = channel
                        .member_activity
                        .iter()
                        .find(|(known, _)| rt.state.isupport.names_equal(known, nick))
                        .map(|(_, last)| *last);
                    nick_value(&rt.state, nick, prefixes, last_activity, prop)
                })
            } else {
                rt.event.nick.clone()
            }
        }
        // The secondary nick/target: kicked user (on KICK), new nick (on NICK),
        // or the affected nick/mask in per-mode events (on OP/BAN/VOICE/…).
        "knick" | "newnick" | "opnick" | "vnick" | "hnick" => rt.event.knick.clone(),
        // In on BAN/UNBAN the affected value is a mask. $banmask is the whole mask;
        // $bnick is just its nick part — and, like mIRC, $null when the mask carries
        // no real nick (e.g. *!*@host).
        "banmask" => rt.event.knick.clone(),
        "bnick" => match rt.event.knick.split_once('!') {
            Some((nick, _)) if !nick.is_empty() && nick != "*" => nick.to_string(),
            _ => String::new(),
        },
        "chan" => {
            // $chan = event channel; $chan(N/#) selects a joined channel.
            if args.is_empty() {
                rt.event.chan.clone()
            } else {
                let selector = a(0);
                let channel = match selector.parse::<usize>() {
                    Ok(0) => return rt.state.channels.len().to_string(),
                    Ok(n) => rt.state.channels.get(n - 1),
                    Err(_) => find_channel(&rt.state, &selector),
                };
                channel.map_or_else(String::new, |channel| {
                    match prop.to_ascii_lowercase().as_str() {
                        "topic" => channel.topic.clone(),
                        "mode" => channel.mode.clone(),
                        "key" => channel.key.clone(),
                        "limit" => channel.limit.clone(),
                        "status" => "joined".to_string(),
                        "ial" => {
                            let nicks = if channel.members.is_empty() {
                                channel.nicks.iter().map(String::as_str).collect::<Vec<_>>()
                            } else {
                                channel
                                    .members
                                    .iter()
                                    .map(|(nick, _)| nick.as_str())
                                    .collect::<Vec<_>>()
                            };
                            bool_str(nicks.iter().all(|nick| {
                                rt.state
                                    .ial
                                    .iter()
                                    .any(|(known, _)| rt.state.isupport.names_equal(known, nick))
                            }))
                        }
                        _ => channel.name.clone(),
                    }
                })
            }
        }
        // $active -> the name of the frontend's currently-focused window (the
        // channel/query/status buffer). Empty ($null) if none reported yet, as in
        // mIRC. Set by the UI via `script_set_active` on every buffer switch.
        "active" => rt.active.clone(),
        // $v1 / $v2 -> the operands of the most recent comparison (or the value
        // whose truthiness was tested). Set by `if`/`while` conditions and lazy
        // `$iif`; the classic `$iif(getvalue, $v1, default)` idiom reads $v1 here.
        "v1" => rt
            .vars
            .get(super::eval::V1_KEY)
            .cloned()
            .unwrap_or_default(),
        "v2" => rt
            .vars
            .get(super::eval::V2_KEY)
            .cloned()
            .unwrap_or_default(),
        // Numeric connection ids. $cid = this run's connection; $activecid = the
        // connection owning the focused window; both $null when unknown.
        "cid" => match rt.conns.cid_of(&rt.state.server_id) {
            0 => String::new(),
            c => c.to_string(),
        },
        "activecid" => match rt.conns.active_cid {
            0 => String::new(),
            c => c.to_string(),
        },
        // $scon(0) = number of connections; $scon(N) = the Nth connection's cid.
        "scon" => {
            let n: usize = a(0).parse().unwrap_or(0);
            if n == 0 {
                rt.conns.entries.len().to_string()
            } else {
                rt.conns
                    .entries
                    .get(n - 1)
                    .map(|(c, _)| c.to_string())
                    .unwrap_or_default()
            }
        }
        // $scid(N): index by cid value. 0 = connection count; -1 = active cid;
        // otherwise echo the cid if a connection with it exists ($null if not).
        "scid" => {
            let n: i64 = a(0).trim().parse().unwrap_or(0);
            if n == 0 {
                rt.conns.entries.len().to_string()
            } else if n == -1 {
                match rt.conns.active_cid {
                    0 => String::new(),
                    c => c.to_string(),
                }
            } else if n > 0 && rt.conns.entries.iter().any(|(c, _)| *c == n as u32) {
                n.to_string()
            } else {
                String::new()
            }
        }
        // $wid = the window the current run relates to (its channel/query, else the
        // active window); $activewid = the active window. Both $null when unknown.
        "wid" => {
            let name = if !rt.event.chan.is_empty() {
                &rt.event.chan
            } else {
                &rt.event.target
            };
            let w = if name.is_empty() {
                0
            } else {
                rt.wins.wid_of(&rt.state.server_id, name)
            };
            match if w == 0 { rt.wins.active_wid } else { w } {
                0 => String::new(),
                w => w.to_string(),
            }
        }
        "activewid" => match rt.wins.active_wid {
            0 => String::new(),
            w => w.to_string(),
        },
        "lactivewid" => match rt.wins.last_active_wid {
            0 => String::new(),
            w => w.to_string(),
        },
        "lactive" => rt
            .wins
            .entry_for_wid(rt.wins.last_active_wid)
            .map(|(_, _, window)| window.clone())
            .unwrap_or_default(),
        "lactivecid" => rt
            .wins
            .entry_for_wid(rt.wins.last_active_wid)
            .map(|(_, server_id, _)| rt.conns.cid_of(server_id))
            .filter(|cid| *cid != 0)
            .map(|cid| cid.to_string())
            .unwrap_or_default(),
        "query" => {
            let queries: Vec<&(u32, String, String)> = rt
                .wins
                .entries
                .iter()
                .filter(|(_, server_id, window)| {
                    server_id == &rt.state.server_id
                        && !window.eq_ignore_ascii_case("Status Window")
                        && !window.eq_ignore_ascii_case("(status)")
                        && !window.starts_with('@')
                        && !window.starts_with('=')
                        && !window
                            .chars()
                            .next()
                            .is_some_and(|prefix| rt.state.isupport.chan_types.contains(prefix))
                })
                .collect();
            let selector = a(0);
            if selector == "0" {
                queries.len().to_string()
            } else {
                let entry = selector
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|index| queries.get(index).copied())
                    .or_else(|| {
                        queries
                            .iter()
                            .copied()
                            .find(|(_, _, window)| rt.state.isupport.names_equal(window, &selector))
                    });
                entry.map_or_else(String::new, |(wid, server_id, window)| {
                    match prop.to_ascii_lowercase().as_str() {
                        "" => window.clone(),
                        "wid" => wid.to_string(),
                        "cid" => match rt.conns.cid_of(server_id) {
                            0 => String::new(),
                            cid => cid.to_string(),
                        },
                        "addr" => rt
                            .state
                            .ial
                            .iter()
                            .find(|(nick, _)| rt.state.isupport.names_equal(nick, window))
                            .map(|(_, address)| address.clone())
                            .unwrap_or_default(),
                        "idle" => rt
                            .state
                            .channels
                            .iter()
                            .filter_map(|channel| {
                                channel
                                    .member_activity
                                    .iter()
                                    .find(|(nick, _)| rt.state.isupport.names_equal(nick, window))
                                    .map(|(_, activity)| *activity)
                            })
                            .max()
                            .map(|activity| now_secs().saturating_sub(activity).to_string())
                            .unwrap_or_default(),
                        "logfile" => {
                            let clean = |value: &str| {
                                value
                                    .chars()
                                    .map(|ch| {
                                        if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                                            ch
                                        } else {
                                            '_'
                                        }
                                    })
                                    .collect::<String>()
                            };
                            rt.data_dir
                                .parent()
                                .unwrap_or(&rt.data_dir)
                                .join("logs")
                                .join(clean(rt.network))
                                .join(format!("{}.log", clean(window)))
                                .to_string_lossy()
                                .into_owned()
                        }
                        "stamp" => "$true".to_string(),
                        // Native HWNDs are intentionally unavailable on a
                        // cross-platform WebView client.
                        "hwnd" => String::new(),
                        _ => String::new(),
                    }
                })
            }
        }
        "chat" | "send" | "get" => eval_dcc_ident(rt, name, args, prop),
        "onchan" => {
            // $onchan(#chan) -> are you in that channel?
            if rt
                .state
                .channels
                .iter()
                .any(|c| channel_names_equal(&rt.state, &c.name, &a(0)))
            {
                "$true".to_string()
            } else {
                "$false".to_string()
            }
        }
        // $style(N) marks the popup item it prefixes: 1 checked, 2 disabled, 3
        // both. Returns a sentinel consumed when the menu is built (harmless and
        // inert anywhere else, matching mIRC — it's only meaningful in a menu).
        "style" => {
            let n = a(0);
            format!(
                "{}{}",
                super::eval::STYLE_MARK,
                if n.is_empty() { "0" } else { &n }
            )
        }
        // Selected nicknames in a nicklist popup (empty in any other context).
        // $snicks -> comma-separated list; matches mIRC's nick1,nick2,...
        "snicks" => rt.event.snicks.join(","),
        // $snick(#,N) -> Nth selected nick (1-based); N=0 -> count. With no N,
        // the whole selection space-separated. The channel arg is accepted for
        // mIRC compatibility but not filtered on — a popup run carries the one
        // listbox selection it was invoked from.
        "snick" => {
            let sel = &rt.event.snicks;
            if args.len() >= 2 {
                let n: usize = a(1).parse().unwrap_or(0);
                if n == 0 {
                    sel.len().to_string()
                } else {
                    sel.get(n - 1).cloned().unwrap_or_default()
                }
            } else {
                sel.join(" ")
            }
        }
        "did" => {
            if args.is_empty() {
                return rt.event.dialog_control.clone();
            }
            let (control, line) = if args.len() >= 2 {
                (a(1), a(2).parse::<usize>().unwrap_or(0))
            } else {
                (a(0), a(1).parse::<usize>().unwrap_or(0))
            };
            let value = rt.event.did.get(&control).cloned().unwrap_or_default();
            let lines: Vec<&str> = value.split('\n').collect();
            let options = rt
                .event
                .did
                .get(&format!("\u{0}options\u{0}{control}"))
                .map(|value| value.split('\n').collect::<Vec<_>>())
                .unwrap_or_default();
            match prop {
                "len" => {
                    let text = if line > 0 {
                        lines.get(line - 1).copied().unwrap_or("")
                    } else {
                        &value
                    };
                    text.chars().count().to_string()
                }
                "lines" => if options.is_empty() {
                    lines.len()
                } else {
                    options.len()
                }
                .to_string(),
                "state" => {
                    if value == "1" || value == "2" {
                        value
                    } else {
                        "0".to_string()
                    }
                }
                "seltext" => value.split('\n').next().unwrap_or("").to_string(),
                "sel" => {
                    let selected: Vec<usize> = value
                        .split('\n')
                        .filter_map(|selected| {
                            options.iter().position(|option| *option == selected)
                        })
                        .map(|index| index + 1)
                        .collect();
                    if line == 0 {
                        selected.len().to_string()
                    } else {
                        selected.get(line - 1).copied().unwrap_or(0).to_string()
                    }
                }
                "visible" | "enabled" => rt
                    .event
                    .did
                    .get(&format!("\u{0}{prop}\u{0}{control}"))
                    .map(|value| if value == "true" { "$true" } else { "$false" })
                    .unwrap_or("$false")
                    .to_string(),
                "isid" => bool_str(rt.event.did.contains_key(&control)),
                "edited" => rt
                    .event
                    .did
                    .get(&format!("\u{0}edited\u{0}{control}"))
                    .map(|value| if value == "true" { "$true" } else { "$false" })
                    .unwrap_or("$false")
                    .to_string(),
                "next" | "prev" => rt
                    .event
                    .did
                    .get(&format!("\u{0}{prop}\u{0}{control}"))
                    .cloned()
                    .unwrap_or_default(),
                _ if line > 0 => lines.get(line - 1).copied().unwrap_or("").to_string(),
                _ => value,
            }
        }
        "dname" => rt.event.dialog_name.clone(),
        "devent" => rt.event.dialog_event.clone(),
        "dialog" => {
            let prefix = "\u{0}dialog\u{0}";
            let mut names: Vec<String> = rt
                .vars
                .keys()
                .filter_map(|key| {
                    key.strip_prefix(prefix)
                        .and_then(|rest| rest.split_once('\u{0}'))
                        .map(|(name, _)| name.to_string())
                })
                .collect();
            names.sort();
            names.dedup();
            let selector = a(0);
            let dialog = match selector.parse::<usize>() {
                Ok(0) => return names.len().to_string(),
                Ok(index) => names.get(index - 1).cloned().unwrap_or_default(),
                Err(_) => selector.to_ascii_lowercase(),
            };
            if !names.iter().any(|name| name.eq_ignore_ascii_case(&dialog)) {
                return String::new();
            }
            match prop {
                "" => dialog,
                "title" | "table" | "width" | "height" | "w" | "h" => {
                    let property = match prop {
                        "w" => "width",
                        "h" => "height",
                        value => value,
                    };
                    rt.vars
                        .get(&super::eval::dialog_state_key(&dialog, property))
                        .cloned()
                        .unwrap_or_default()
                }
                "modal" => "$false".to_string(),
                "active" => "$true".to_string(),
                "focus" => rt.event.dialog_control.clone(),
                _ => String::new(),
            }
        }
        "didtok" => {
            let control = a(1);
            let delimiter = a(2)
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .unwrap_or(',');
            rt.event
                .did
                .get(&format!("\u{0}options\u{0}{control}"))
                .map(|value| {
                    value
                        .split('\n')
                        .collect::<Vec<_>>()
                        .join(&delimiter.to_string())
                })
                .unwrap_or_default()
        }
        "didwm" | "didreg" => {
            let control = a(1);
            let needle = a(2);
            let start = a(3).parse::<usize>().unwrap_or(1).max(1);
            let options = rt
                .event
                .did
                .get(&format!("\u{0}options\u{0}{control}"))
                .cloned()
                .unwrap_or_default();
            options
                .split('\n')
                .enumerate()
                .skip(start - 1)
                .find(|(_, value)| {
                    if name.eq_ignore_ascii_case("didreg") {
                        mirc_regex_is_match(value, &needle)
                    } else {
                        wildcard_match(&needle, value)
                    }
                })
                .map_or_else(String::new, |(index, _)| (index + 1).to_string())
        }
        "address" => {
            // Bare $address -> the triggering user's user@host; $address(nick) ->
            // that nick's user@host; $address(nick, type) -> masked address.
            let who = if args.is_empty() {
                rt.state.isupport.casefold(&rt.event.nick)
            } else {
                rt.state.isupport.casefold(&a(0))
            };
            if args.is_empty() && !rt.event.peer_address.is_empty() {
                return rt.event.peer_address.clone();
            }
            match rt
                .state
                .ial
                .iter()
                .find(|(n, _)| rt.state.isupport.names_equal(n, &who))
            {
                Some((_, full)) => {
                    if args.len() >= 2 {
                        mask_address(full, a(1).parse().unwrap_or(0))
                    } else {
                        full.split_once('!')
                            .map(|(_, h)| h.to_string())
                            .unwrap_or_default()
                    }
                }
                None => String::new(),
            }
        }
        // The triggering user's address pieces, looked up from the IAL:
        // $fulladdress = nick!user@host, $site = host, $wildsite = *!*@host.
        "fulladdress" => {
            let who = rt.state.isupport.casefold(&rt.event.nick);
            rt.state
                .ial
                .iter()
                .find(|(n, _)| rt.state.isupport.names_equal(n, &who))
                .map(|(_, f)| f.clone())
                .unwrap_or_default()
        }
        "site" => {
            let who = rt.state.isupport.casefold(&rt.event.nick);
            rt.state
                .ial
                .iter()
                .find(|(n, _)| rt.state.isupport.names_equal(n, &who))
                .and_then(|(_, f)| f.split_once('@').map(|(_, h)| h.to_string()))
                .unwrap_or_default()
        }
        "wildsite" => {
            let who = rt.state.isupport.casefold(&rt.event.nick);
            rt.state
                .ial
                .iter()
                .find(|(n, _)| rt.state.isupport.names_equal(n, &who))
                .and_then(|(_, f)| f.split_once('@').map(|(_, h)| format!("*!*@{h}")))
                .unwrap_or_default()
        }
        "mask" => {
            // $mask(nick!user@host, type) -> wildcard mask of that type.
            mask_address(&a(0), a(1).parse().unwrap_or(0))
        }
        "ial" => {
            // Bare `$ial` reports whether the IAL is enabled for this session.
            if args.is_empty() {
                return bool_str(rt.state.ial_enabled);
            }
            // `$ial(nick/mask,N)` returns the Nth full nick!user@host entry;
            // N defaults to 1 and zero returns the count.
            let query = a(0);
            let mut hits: Vec<(&str, &str)> = rt
                .state
                .ial
                .iter()
                .filter(|(nick, full)| ial_query_matches(&rt.state, &query, nick, full))
                .map(|(nick, full)| (nick.as_str(), full.as_str()))
                .collect();
            hits.sort_unstable_by(|a, b| a.1.cmp(b.1));
            let n: usize = a(1).parse().unwrap_or(1);
            if n == 0 {
                hits.len().to_string()
            } else {
                hits.get(n - 1).map_or_else(String::new, |(nick, full)| {
                    let info = find_ial_info(&rt.state, nick, full);
                    ial_value(full, prop, "", info)
                })
            }
        }
        "ialchan" => {
            // `$ialchan(nick/mask,#,N)` is `$ial()` restricted to members of #.
            // The `.pnick` property includes that member's channel prefix.
            let query = a(0);
            let channel = rt
                .state
                .channels
                .iter()
                .find(|c| channel_names_equal(&rt.state, &c.name, &a(1)));
            let mut hits: Vec<(&str, &str)> = rt
                .state
                .ial
                .iter()
                .filter(|(nick, full)| {
                    channel.is_some_and(|c| {
                        c.nicks
                            .iter()
                            .any(|member| rt.state.isupport.names_equal(member, nick))
                    }) && ial_query_matches(&rt.state, &query, nick, full)
                })
                .map(|(nick, full)| (nick.as_str(), full.as_str()))
                .collect();
            hits.sort_unstable_by(|a, b| a.1.cmp(b.1));
            let n: usize = a(2).parse().unwrap_or(1);
            if n == 0 {
                hits.len().to_string()
            } else {
                hits.get(n - 1)
                    .map(|(nick, full)| {
                        let prefixes = channel
                            .and_then(|c| {
                                c.members
                                    .iter()
                                    .find(|(member, _)| rt.state.isupport.names_equal(member, nick))
                            })
                            .map(|(_, prefixes)| prefixes.as_str())
                            .unwrap_or("");
                        let info = find_ial_info(&rt.state, nick, full);
                        ial_value(full, prop, prefixes, info)
                    })
                    .unwrap_or_default()
            }
        }
        "ialmark" => {
            let query = a(0);
            let mut entries: Vec<_> = rt
                .state
                .ial_info
                .iter()
                .filter(|info| ial_query_matches(&rt.state, &query, &info.nick, &info.address))
                .collect();
            entries.sort_unstable_by(|a, b| a.address.cmp(&b.address));
            let Some(info) = entries.first() else {
                return String::new();
            };
            let selector = a(1);
            if let Ok(n) = selector.parse::<usize>() {
                if n == 0 {
                    return info.marks.len().to_string();
                }
                return info
                    .marks
                    .get(n - 1)
                    .map_or_else(String::new, |(name, text)| {
                        if prop.eq_ignore_ascii_case("name") {
                            name.clone()
                        } else {
                            text.clone()
                        }
                    });
            }
            info.marks
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&selector))
                .map_or_else(String::new, |(name, text)| {
                    if prop.eq_ignore_ascii_case("name") {
                        name.clone()
                    } else {
                        text.clone()
                    }
                })
        }
        "halted" => bool_str(rt.event.default_halted),
        "caller" => rt.caller.to_string(),
        "ctimer" => rt.event.timer.clone(),
        "ltimer" => rt
            .vars
            .get(super::eval::LTIMER_KEY)
            .cloned()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| rt.timers.last()),
        "isid" => bool_str(rt.caller == "identifier"),
        // $show -> $false inside an alias invoked as a silent `.command`, else $true.
        "show" => bool_str(rt.show),
        // $result -> the value returned by the most recently called alias.
        "result" => rt
            .vars
            .get(super::eval::RESULT_KEY)
            .cloned()
            .unwrap_or_default(),
        // $prop -> the `.property` the current custom identifier was called with.
        "prop" => rt.vars.get(PROP_KEY).cloned().unwrap_or_default(),
        // $unsafe(text): mIRC delays one evaluation level to survive double-eval
        // contexts (timers etc.); jIRC evaluates once, so it's a passthrough.
        "unsafe" => a(0),
        // $stripped -> control codes stripped from the incoming message by the
        // strip-incoming setting. jIRC doesn't strip incoming messages, so 0.
        "stripped" => "0".to_string(),
        // $ulist(addr[, L], N)[.info] -> Nth user-list entry matching addr (and, if
        // given, level L); N=0 -> count. Returns the entry address (or its .info).
        "ulist" => {
            let addr = a(0);
            let level_s = a(1);
            let level = if level_s.trim().is_empty() {
                None
            } else {
                Some(level_s.trim())
            };
            let n: usize = a(2).trim().parse().unwrap_or(1);
            let m = rt.users.matching(&addr, level);
            if n == 0 {
                m.len().to_string()
            } else if let Some(e) = m.get(n - 1) {
                if prop.eq_ignore_ascii_case("info") {
                    e.info.clone()
                } else {
                    e.address.clone()
                }
            } else {
                String::new()
            }
        }
        // $level(addr) -> the comma-joined levels of the first matching entry.
        "level" => rt.users.levels_for(&a(0)),
        // $ulevel / $clevel -> the user's / the event's matched access level,
        // set by the dispatcher's level gate.
        "ulevel" => rt.event.ulevel.clone(),
        "clevel" => rt.event.clevel.clone(),
        // $aop / $avoice / $protect -> $true/$false enabled (bare); with an arg,
        // $aop(addr/N)[.type|.network] looks up an auto-list entry.
        "aop" | "avoice" | "protect" => {
            use crate::script::users::AutoKind;
            let kind = if name.eq_ignore_ascii_case("aop") {
                AutoKind::Aop
            } else if name.eq_ignore_ascii_case("avoice") {
                AutoKind::Avoice
            } else {
                AutoKind::Protect
            };
            if args.is_empty() {
                bool_str(rt.users.auto_enabled(kind))
            } else {
                rt.users.auto_lookup(kind, &a(0), prop)
            }
        }
        "comchan" => {
            // $comchan(nick, N) -> Nth channel you share with nick (N=0 → count).
            let who = a(0);
            let common: Vec<&crate::irc::state::ChannelView> = rt
                .state
                .channels
                .iter()
                .filter(|channel| {
                    channel
                        .nicks
                        .iter()
                        .any(|member| rt.state.isupport.names_equal(member, &who))
                })
                .collect();
            let n: usize = a(1).parse().unwrap_or(0);
            if n == 0 {
                common.len().to_string()
            } else {
                common.get(n - 1).map_or_else(String::new, |channel| {
                    let own_prefixes = channel
                        .members
                        .iter()
                        .find(|(member, _)| rt.state.isupport.names_equal(member, rt.my_nick))
                        .map(|(_, prefixes)| prefixes.as_str())
                        .unwrap_or("");
                    let mode = match prop.to_ascii_lowercase().as_str() {
                        "op" => Some('o'),
                        "help" => Some('h'),
                        "voice" => Some('v'),
                        _ => None,
                    };
                    match mode {
                        Some(mode) => bool_str(
                            rt.state
                                .isupport
                                .prefix_for_mode(mode)
                                .is_some_and(|prefix| own_prefixes.contains(prefix)),
                        ),
                        None => channel.name.clone(),
                    }
                })
            }
        }
        "target" => {
            if rt.event.target.is_empty() {
                rt.event.chan.clone()
            } else {
                rt.event.target.clone()
            }
        }
        "event" => rt.event.event.clone(),
        "numeric" => rt.event.numeric.clone(),
        "parseline" => rt.event.parse_line.clone(),
        "parsetype" => rt.event.parse_type.clone(),
        "parseutf" => {
            if rt.event.parse_type.is_empty() {
                String::new()
            } else {
                bool_str(rt.event.parse_utf)
            }
        }
        "parseem" => {
            if rt.event.parse_type.is_empty() {
                String::new()
            } else {
                bool_str(rt.event.parse_em)
            }
        }
        "rawmsg" => rt.event.raw_msg.clone(),
        "rawbytes" => rt
            .event
            .raw_bytes
            .iter()
            .map(|byte| *byte as char)
            .collect(),
        "msgstamp" => rt.event.msg_stamp.clone(),
        "msgtags" => {
            if args.is_empty() {
                rt.event.msg_tags_raw.clone()
            } else {
                let selector = a(0);
                if selector == "0" {
                    rt.event.msg_tags.len().to_string()
                } else {
                    let entry = selector
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|n| rt.event.msg_tags.get(n))
                        .or_else(|| {
                            rt.event
                                .msg_tags
                                .iter()
                                .find(|(tag, _, _)| tag == &selector)
                        });
                    match entry {
                        Some((tag, _, _)) if prop.eq_ignore_ascii_case("tag") => tag.clone(),
                        Some((_, key, _)) if prop.eq_ignore_ascii_case("key") => key.clone(),
                        Some((tag, key, has_key)) if *has_key => format!("{tag}={key}"),
                        Some((tag, _, _)) => tag.clone(),
                        None => String::new(),
                    }
                }
            }
        }
        "script" => {
            if args.is_empty() {
                rt.event.script_source.clone()
            } else {
                let sources = rt.script.source_files();
                let selector = a(0);
                if selector == "0" {
                    sources.len().to_string()
                } else if let Ok(n) = selector.parse::<usize>() {
                    n.checked_sub(1)
                        .and_then(|index| sources.get(index))
                        .copied()
                        .unwrap_or("")
                        .to_string()
                } else {
                    sources
                        .iter()
                        .find(|source| source.eq_ignore_ascii_case(&selector))
                        .copied()
                        .unwrap_or("")
                        .to_string()
                }
            }
        }
        // $alias(N/filename) lists loaded files containing aliases.
        "alias" => {
            let sources = rt.script.alias_source_files();
            let selector = a(0);
            if selector == "0" {
                sources.len().to_string()
            } else if let Ok(n) = selector.parse::<usize>() {
                n.checked_sub(1)
                    .and_then(|index| sources.get(index))
                    .copied()
                    .unwrap_or("")
                    .to_string()
            } else {
                sources
                    .iter()
                    .find(|source| source.eq_ignore_ascii_case(&selector))
                    .copied()
                    .unwrap_or("")
                    .to_string()
            }
        }
        "scriptline" => match rt.event.script_line {
            0 => String::new(),
            line => line.to_string(),
        },
        "matchkey" => rt.event.match_key.clone(),
        "maddress" => rt.event.matched_address.clone(),
        "network" => rt.network.to_string(),
        "appactive" => rt
            .vars
            .get(super::eval::CLIENT_APP_ACTIVE_KEY)
            .cloned()
            .unwrap_or_else(|| "$false".into()),
        "appstate" => rt
            .vars
            .get(super::eval::CLIENT_APP_STATE_KEY)
            .cloned()
            .unwrap_or_else(|| "normal".into()),
        "fullscreen" => bool_str(
            rt.vars
                .get(super::eval::CLIENT_APP_STATE_KEY)
                .is_some_and(|state| state == "full"),
        )
        .to_string(),
        "darkmode" => rt
            .vars
            .get(super::eval::CLIENT_DARK_MODE_KEY)
            .cloned()
            .unwrap_or_else(|| "$false".into()),
        "toolbar" => client_on_off(rt, super::eval::CLIENT_TOOLBAR_KEY),
        "treebar" => client_on_off(rt, super::eval::CLIENT_TREEBAR_KEY),
        "switchbar" => client_on_off(rt, super::eval::CLIENT_SWITCHBAR_KEY),
        "keychar" => rt.event.key_char.clone(),
        "keyval" => rt
            .event
            .key_val
            .map(|value| value.to_string())
            .unwrap_or_default(),
        "keyrpt" => if rt.event.key_repeat {
            "$true"
        } else {
            "$false"
        }
        .into(),
        "notify" => {
            let list = rt
                .vars
                .get(super::eval::CLIENT_NOTIFY_LIST_KEY)
                .map(|value| {
                    value
                        .split('\u{1f}')
                        .filter(|nick| !nick.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let online = rt
                .vars
                .get(super::eval::CLIENT_NOTIFY_ONLINE_KEY)
                .map(|value| value.split('\u{1f}').collect::<Vec<_>>())
                .unwrap_or_default();

            if args.is_empty() {
                bool_str(!list.is_empty()).to_string()
            } else {
                let selector = a(0);
                let property = prop.to_ascii_lowercase();
                if selector == "0" {
                    list.len().to_string()
                } else {
                    let numeric = selector.parse::<usize>().ok();
                    let index = numeric
                        .filter(|number| *number > 0 && *number <= list.len())
                        .map(|number| number - 1)
                        .or_else(|| {
                            list.iter()
                                .position(|nick| nick.eq_ignore_ascii_case(&selector))
                        });

                    if property.is_empty() {
                        if numeric.is_some() {
                            index
                                .and_then(|position| list.get(position).copied())
                                .unwrap_or_default()
                                .to_string()
                        } else {
                            index.map(|position| position + 1).unwrap_or(0).to_string()
                        }
                    } else if let Some(position) = index {
                        let nick = list[position];
                        match property.as_str() {
                            "ison" => bool_str(
                                online
                                    .iter()
                                    .any(|online_nick| online_nick.eq_ignore_ascii_case(nick)),
                            )
                            .to_string(),
                            "addr" => rt
                                .state
                                .ial
                                .iter()
                                .find(|(known_nick, _)| known_nick.eq_ignore_ascii_case(nick))
                                .map(|(_, address)| address.clone())
                                .unwrap_or_default(),
                            _ => String::new(),
                        }
                    } else {
                        String::new()
                    }
                }
            }
        }
        "ignore" => eval_client_list_ident(
            rt,
            args,
            prop,
            super::eval::CLIENT_IGNORE_LIST_KEY,
            "ignore",
        ),
        "highlight" => eval_client_list_ident(
            rt,
            args,
            prop,
            super::eval::CLIENT_HIGHLIGHT_LIST_KEY,
            "highlight",
        ),
        "font" => eval_client_list_ident(rt, args, prop, super::eval::CLIENT_FONT_LIST_KEY, "font"),
        "editbox" => {
            let target = if a(0).is_empty() {
                rt.active.clone()
            } else {
                a(0)
            };
            let packed = rt.vars.get(&format!(
                "{}{}",
                super::eval::CLIENT_EDITBOX_PREFIX,
                target.to_lowercase()
            ));
            let mut fields = packed.map(|value| value.splitn(3, '\u{1f}'));
            let start = fields
                .as_mut()
                .and_then(|parts| parts.next())
                .unwrap_or("0");
            let end = fields
                .as_mut()
                .and_then(|parts| parts.next())
                .unwrap_or("0");
            let text = fields.as_mut().and_then(|parts| parts.next()).unwrap_or("");
            match prop.to_ascii_lowercase().as_str() {
                "selstart" => start.to_string(),
                "selend" => end.to_string(),
                _ => text.to_string(),
            }
        }
        "server" => rt.server.to_string(),
        "cmdline" => process_command_line(std::env::args_os().skip(1)),
        "portable" => std::env::current_exe()
            .ok()
            .is_some_and(|exe| portable_from_executable(&exe))
            .then_some("$true")
            .unwrap_or("$false")
            .to_string(),
        "true" => "$true".to_string(),
        "false" => "$false".to_string(),
        "null" => String::new(),
        "remote" => rt
            .vars
            .get(super::eval::REMOTE_FLAGS_KEY)
            .cloned()
            .unwrap_or_else(|| "7".into()),
        "parms" => rt.event.text.clone(),
        // These are steady-state process flags. Startup scripts run after the
        // engine has loaded, and scripts cannot execute during process teardown.
        "starting" | "exiting" => "0".to_string(),
        // Whitespace constants (used heavily by socket scripts).
        "crlf" => "\r\n".to_string(),
        "cr" => "\r".to_string(),
        "lf" => "\n".to_string(),
        "tab" => "\t".to_string(),
        "ctime" => now_secs().to_string(),
        // $gmt -> current GMT time as unixtime (absolute, == $ctime here).
        "gmt" => now_secs().to_string(),
        // $ticks -> milliseconds since this process started (deltas are what
        // scripts use; the absolute base differs from mIRC's OS-boot base).
        "ticks" => ticks().to_string(),
        "timestamp" | "logstamp" => chrono::Local::now().format("[%H:%M]").to_string(),
        "timestampfmt" | "logstampfmt" => "[HH:nn]".to_string(),
        "uptime" => {
            let item = a(0).to_ascii_lowercase();
            let milliseconds = match item.as_str() {
                "mirc" => Some(ticks()),
                "server" if rt.state.connect_time != 0 => {
                    Some(now_secs().saturating_sub(rt.state.connect_time) * 1000)
                }
                // A portable system-boot clock is not available in std. Returning
                // $null is safer than reporting the jIRC process as OS uptime.
                "system" => None,
                _ => None,
            };
            milliseconds.map_or_else(String::new, |milliseconds| match a(1).as_str() {
                "1" => format_duration((milliseconds / 1000) as i64),
                "2" => format_duration_without_seconds(milliseconds / 1000),
                "3" => (milliseconds / 1000).to_string(),
                _ => milliseconds.to_string(),
            })
        }
        "time" => chrono::Local::now().format("%H:%M:%S").to_string(),
        "date" => chrono::Local::now().format("%d/%m/%Y").to_string(),
        "fulldate" => chrono::Local::now()
            .format("%a %b %d %H:%M:%S %Y")
            .to_string(),
        "asctime" => {
            // $asctime([N,] format) -> the ctime N (or now) in local time.
            let (ts, fmt) = match a(0).parse::<i64>() {
                Ok(n) => (n, a(1)),
                Err(_) => (now_secs() as i64, a(0)),
            };
            let fmt = if fmt.is_empty() {
                "ddd mmm dd HH:nn:ss yyyy".to_string()
            } else {
                fmt
            };
            asctime(ts, &fmt)
        }
        // mIRC: seconds your local time is behind GMT (positive west of GMT).
        "timezone" => (-chrono::Local::now().offset().local_minus_utc()).to_string(),
        "daylight" => "0".to_string(),
        "len" => a(0).chars().count().to_string(),
        "upper" => a(0).to_uppercase(),
        "lower" => a(0).to_lowercase(),
        "left" => {
            let n: i64 = a(1).parse().unwrap_or(0);
            take_left(&a(0), n)
        }
        "right" => {
            let n: i64 = a(1).parse().unwrap_or(0);
            take_right(&a(0), n)
        }
        "mid" => {
            // $mid(text, S [, N]). S is 1-based; negative = from the end; 0 = 1.
            // N absent = to the end; N=0 = the numeric length of the remainder;
            // N<0 = the remainder except the last |N| chars (all mIRC-exact).
            let text: Vec<char> = a(0).chars().collect();
            let len = text.len() as i64;
            let s: i64 = a(1).trim().parse().unwrap_or(1);
            let start = (if s < 0 {
                (len + s).max(0)
            } else {
                s.max(1) - 1
            } as usize)
                .min(text.len());
            let n_arg = a(2);
            if n_arg.is_empty() {
                text[start..].iter().collect()
            } else {
                let n: i64 = n_arg.trim().parse().unwrap_or(0);
                if n == 0 {
                    (text.len() - start).to_string()
                } else if n < 0 {
                    let end = ((len + n).max(start as i64) as usize).min(text.len());
                    text[start..end].iter().collect()
                } else {
                    text[start..].iter().take(n as usize).collect()
                }
            }
        }
        "chr" => a(0)
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default(),
        "asc" => a(0)
            .chars()
            .next()
            .map(|c| (c as u32).to_string())
            .unwrap_or_default(),
        // $input(message, type, title, default, …) — a modal text prompt. We
        // drive the edit form; returns the entered text, or empty if cancelled.
        "input" => {
            let v = rt.input.prompt(&a(0), &a(2), &a(3)).unwrap_or_default();
            rt.vars
                .insert(super::eval::LASTINPUT_KEY.to_string(), v.clone());
            v
        }
        "str" => {
            let n: usize = a(1).parse().unwrap_or(0);
            a(0).repeat(n)
        }
        // $rands is the cryptographically-secure variant; the output (a random
        // value in range) is indistinguishable, so it shares $rand's logic.
        "rand" | "r" | "rands" => {
            let (lo, hi) = (a(0), a(1));
            match (lo.parse::<i64>(), hi.parse::<i64>()) {
                (Ok(x), Ok(y)) => rand_range(x, y).to_string(),
                _ => {
                    // Letter range: $rand(a,z) / $r(A,Z).
                    match (lo.chars().next(), hi.chars().next()) {
                        (Some(l), Some(h))
                            if l.is_ascii_alphabetic() && h.is_ascii_alphabetic() =>
                        {
                            char::from_u32(rand_range(l as i64, h as i64) as u32)
                                .map(String::from)
                                .unwrap_or_default()
                        }
                        _ => String::new(),
                    }
                }
            }
        }
        "base" => {
            let inb: u32 = a(1).parse().unwrap_or(10);
            let outb: u32 = a(2).parse().unwrap_or(10);
            let zeropad: usize = a(3).parse().unwrap_or(0);
            base_convert(&a(0), inb, outb, zeropad)
        }
        "round" => match a(0).parse::<f64>() {
            Ok(x) => {
                let d: usize = a(1).parse().unwrap_or(0);
                if d == 0 {
                    (x.round() as i64).to_string()
                } else {
                    format!("{x:.d$}")
                }
            }
            Err(_) => String::new(),
        },
        "duration" => format_duration(a(0).parse::<i64>().unwrap_or(0)),
        "isfile" => bool_str(super::eval::sandbox_path(&rt.data_dir, &a(0)).is_file()),
        "isdir" => bool_str(super::eval::sandbox_path(&rt.data_dir, &a(0)).is_dir()),
        "exists" => bool_str(super::eval::sandbox_path(&rt.data_dir, &a(0)).exists()),
        // $nopath(filename) -> the file name without its path.
        "nopath" => a(0).rsplit(['\\', '/']).next().unwrap_or("").to_string(),
        // $nofile(filename) -> the path (incl. trailing separator), no file name.
        "nofile" => {
            let p = a(0);
            match p.rfind(['\\', '/']) {
                Some(idx) => p[..=idx].to_string(),
                None => String::new(),
            }
        }
        // $longfn/$shortfn -> long / 8.3-short filename; we pass through (modern
        // filesystems use the long form).
        "longfn" | "shortfn" => a(0),
        "samepath" => bool_str(same_sandbox_path(&rt.data_dir, &a(0), &a(1))),
        // $file(name).prop -> file info. Sandboxed to the script-data dir like
        // $isfile/$read (sandbox_path keeps only the leaf name). Times are unix
        // seconds ($ctime-style). attr is best-effort/cross-platform; the
        // Windows-only sig/version return empty. Bare $file(name) -> the resolved
        // path if it exists, else $null.
        "file" => {
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            let md = std::fs::metadata(&path).ok();
            let secs = |t: Option<SystemTime>| {
                t.and_then(|st| st.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs().to_string())
                    .unwrap_or_default()
            };
            let leaf = || a(0).rsplit(['\\', '/']).next().unwrap_or("").to_string();
            match prop.to_ascii_lowercase().as_str() {
                "" | "longfn" | "shortfn" => md
                    .as_ref()
                    .map(|_| path.display().to_string())
                    .unwrap_or_default(),
                "size" => md.as_ref().map(|m| m.len().to_string()).unwrap_or_default(),
                "mtime" => secs(md.as_ref().and_then(|m| m.modified().ok())),
                "ctime" => secs(md.as_ref().and_then(|m| m.created().ok())),
                "atime" => secs(md.as_ref().and_then(|m| m.accessed().ok())),
                "name" => leaf(),
                "ext" => leaf()
                    .rsplit_once('.')
                    .map(|(_, e)| e.to_string())
                    .unwrap_or_default(),
                "path" => {
                    let p = a(0);
                    p.rfind(['\\', '/'])
                        .map(|i| p[..=i].to_string())
                        .unwrap_or_default()
                }
                "attr" => md
                    .as_ref()
                    .map(|m| {
                        let mut s = String::new();
                        if m.is_dir() {
                            s.push('d');
                        }
                        if m.permissions().readonly() {
                            s.push('r');
                        }
                        s
                    })
                    .unwrap_or_default(),
                // Windows PE signature/version — not meaningful cross-platform.
                _ => String::new(),
            }
        }
        "scriptdir" => std::path::Path::new(&rt.event.script_source)
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| format!("{}{}", path.display(), std::path::MAIN_SEPARATOR))
            .unwrap_or_else(|| format!("{}{}", rt.data_dir.display(), std::path::MAIN_SEPARATOR)),
        "mircdir" => {
            format!("{}{}", rt.data_dir.display(), std::path::MAIN_SEPARATOR)
        }
        "iif" => {
            if eval_bool_public(&a(0)) {
                a(1)
            } else {
                a(2)
            }
        }
        "calc" => calc(&a(0)).map(fmt_num).unwrap_or_default(),
        // Roots / powers / logs (6-decimal default like mIRC).
        "sqrt" => fmt_round6(num(&a(0)).sqrt()),
        "cbrt" => fmt_round6(num(&a(0)).cbrt()),
        "hypot" => fmt_round6(num(&a(0)).hypot(num(&a(1)))),
        "log" => fmt_round6(num(&a(0)).ln()),
        "log2" => fmt_round6(num(&a(0)).log2()),
        "log10" => fmt_round6(num(&a(0)).log10()),
        // mIRC returns pi to 20 decimal places.
        "pi" => "3.14159265358979323846".to_string(),
        // Trig — angles are radians by default; the `.deg` property uses degrees
        // for the angle (forward functions) or the result (inverse functions).
        "sin" | "cos" | "tan" => {
            let mut n = num(&a(0));
            if prop == "deg" {
                n = n.to_radians();
            }
            fmt_round6(match name {
                "sin" => n.sin(),
                "cos" => n.cos(),
                _ => n.tan(),
            })
        }
        "sinh" | "cosh" | "tanh" => {
            let n = num(&a(0));
            fmt_round6(match name {
                "sinh" => n.sinh(),
                "cosh" => n.cosh(),
                _ => n.tanh(),
            })
        }
        "asin" | "acos" | "atan" => {
            let mut v = match name {
                "asin" => num(&a(0)).asin(),
                "acos" => num(&a(0)).acos(),
                _ => num(&a(0)).atan(),
            };
            if prop == "deg" {
                v = v.to_degrees();
            }
            fmt_round6(v)
        }
        "atan2" => {
            let mut v = num(&a(0)).atan2(num(&a(1)));
            if prop == "deg" {
                v = v.to_degrees();
            }
            fmt_round6(v)
        }
        // Hashing — $md5(value,[N]): N = 0 plain text (default), 2 filename. N=1
        // (&binvar) is treated as text since the engine has no binary variables.
        "md5" | "sha1" | "sha256" | "sha384" | "sha512" => {
            let data = hash_input(rt, &a(0), &a(1));
            match name {
                "md5" => hex_of(md5::Md5::digest(&data)),
                "sha1" => hex_of(sha1::Sha1::digest(&data)),
                "sha256" => hex_of(sha2::Sha256::digest(&data)),
                "sha384" => hex_of(sha2::Sha384::digest(&data)),
                _ => hex_of(sha2::Sha512::digest(&data)),
            }
        }
        // mIRC renders CRC in uppercase hex (confirmed via $crc64("abc",0)).
        "crc" => format!("{:08X}", crc32fast::hash(&hash_input(rt, &a(0), &a(1)))),
        // $crc64 is CRC-64/XZ, 16 uppercase hex chars.
        "crc64" => {
            use std::sync::OnceLock;
            static CRC64: OnceLock<crc::Crc<u64>> = OnceLock::new();
            let crc = CRC64.get_or_init(|| crc::Crc::<u64>::new(&crc::CRC_64_XZ));
            format!("{:016X}", crc.checksum(&hash_input(rt, &a(0), &a(1))))
        }
        // $hmac(text, key, hash, N) — keyed hash; hash default sha1, N text/binvar/file.
        "hmac" => {
            let data = hash_input(rt, &a(0), &a(3));
            hex_of(hmac_raw(&a(2), a(1).as_bytes(), &data))
        }
        // $hotp(key, count, hash, digits) — RFC 4226. Key auto-detected hex/base32/plain.
        "hotp" => {
            let key = decode_otp_key(&a(0));
            let count: u64 = a(1).trim().parse().unwrap_or(0);
            hotp(&a(2), &key, count, otp_digits(&a(3)))
        }
        // $totp(key, time, hash, digits, timestep) — RFC 6238 (time default now, step 30).
        "totp" => {
            let key = decode_otp_key(&a(0));
            let time: u64 = if a(1).trim().is_empty() {
                now_secs()
            } else {
                a(1).trim().parse().unwrap_or_else(|_| now_secs())
            };
            let step: u64 = a(4).trim().parse().ok().filter(|&s| s >= 1).unwrap_or(30);
            hotp(&a(2), &key, time / step, otp_digits(&a(3)))
        }
        // $pbkdf2(text, salt, hash, length, iterations) — RFC 8018, hex output.
        "pbkdf2" => {
            let length: usize = a(3).trim().parse().unwrap_or(0);
            let iters: u32 = a(4).trim().parse().unwrap_or(1).max(1);
            pbkdf2_hex(&a(2), a(0).as_bytes(), a(1).as_bytes(), iters, length)
        }
        // Bitwise (binary) operators on integers.
        "and" => (uint(&a(0)) & uint(&a(1))).to_string(),
        "or" => (uint(&a(0)) | uint(&a(1))).to_string(),
        "xor" => (uint(&a(0)) ^ uint(&a(1))).to_string(),
        // $not is a 32-bit complement, matching classic mIRC.
        "not" => (!(uint(&a(0)) as u32) as u64).to_string(),
        // Bit test/set — bit positions are 1-based (bit 1 = least significant).
        "biton" | "bitoff" | "isbit" => {
            let v = uint(&a(0));
            let b = uint(&a(1));
            if !(1..=64).contains(&b) {
                return if name == "isbit" {
                    "0".into()
                } else {
                    v.to_string()
                };
            }
            let mask = 1u64 << (b - 1);
            match name {
                "biton" => (v | mask).to_string(),
                "bitoff" => (v & !mask).to_string(),
                _ => {
                    if v & mask != 0 {
                        "1".into()
                    } else {
                        "0".into()
                    }
                }
            }
        }
        // $gcd/$lcm are variadic.
        "gcd" => fold_ints(args, gcd2).to_string(),
        "lcm" => fold_ints(args, |a, b| {
            let g = gcd2(a, b);
            if g == 0 {
                0
            } else {
                (a / g * b).abs()
            }
        })
        .to_string(),
        // $day -> current weekday name; $ord -> English ordinal (2 -> 2nd).
        "day" => chrono::Local::now().format("%A").to_string(),
        "ord" => {
            let n = a(0).trim().parse::<i64>().unwrap_or(0);
            let m = n.unsigned_abs() % 100;
            let suffix = if (11..=13).contains(&m) {
                "th"
            } else {
                match m % 10 {
                    1 => "st",
                    2 => "nd",
                    3 => "rd",
                    _ => "th",
                }
            };
            format!("{n}{suffix}")
        }
        // $longip — IP string <-> 32-bit number (direction follows the input).
        "longip" => {
            let arg = a(0);
            if arg.contains('.') {
                let parts: Vec<u32> = arg
                    .split('.')
                    .map(|p| p.trim().parse().unwrap_or(0))
                    .collect();
                if parts.len() == 4 {
                    parts
                        .iter()
                        .fold(0u32, |acc, &p| (acc << 8) | (p & 0xFF))
                        .to_string()
                } else {
                    String::new()
                }
            } else {
                let n: u32 = arg.trim().parse().unwrap_or(0);
                format!(
                    "{}.{}.{}.{}",
                    (n >> 24) & 0xFF,
                    (n >> 16) & 0xFF,
                    (n >> 8) & 0xFF,
                    n & 0xFF
                )
            }
        }
        // $os — OS family. mIRC returns a Windows version; we are cross-platform.
        "os" => std::env::consts::OS.to_string(),
        // $version -> the jIRC client version (its own CalVer, not an mIRC number).
        "version" => env!("CARGO_PKG_VERSION").to_string(),
        // Safe script string-length limits (mIRC's current values).
        "maxlenl" => "10240".to_string(),
        "maxlenm" => "2048".to_string(),
        "maxlens" => "512".to_string(),
        // $bits -> the app's bit width (64 for jIRC). $numbits(N) -> the number of
        // bits in N's base-2 representation (= length of its binary string).
        "bits" => (std::mem::size_of::<usize>() * 8).to_string(),
        "numbits" => a(0)
            .trim()
            .parse::<u64>()
            .map(|n| format!("{n:b}").len().to_string())
            .unwrap_or_default(),
        // $rgb(R,G,B) -> mIRC's RGB number (R + G*256 + B*65536); $rgb(N) -> "R,G,B".
        // System-colour names are platform-specific and not supported.
        "rgb" => {
            if args.len() >= 3 {
                let v = |k: usize| a(k).trim().parse::<u64>().unwrap_or(0) & 255;
                (v(0) + v(1) * 256 + v(2) * 65536).to_string()
            } else if let Ok(n) = a(0).trim().parse::<u64>() {
                format!("{},{},{}", n & 255, (n >> 8) & 255, (n >> 16) & 255)
            } else {
                String::new()
            }
        }
        // $ansi2mirc(text) -> ANSI SGR escape sequences converted to mIRC codes.
        "ansi2mirc" => ansi_to_mirc(&a(0)),
        // $timer(name/N)[.com|.reps|.delay] -> info about a `/timer` (N=0 -> count).
        "timer" => {
            let list = rt.timers.snapshot();
            let arg = a(0);
            if prop.eq_ignore_ascii_case("name") {
                return list
                    .iter()
                    .position(|t| t.name.eq_ignore_ascii_case(arg.trim()))
                    .map(|n| (n + 1).to_string())
                    .unwrap_or_default();
            }
            match arg.trim().parse::<usize>() {
                Ok(0) => list.len().to_string(),
                Ok(k) => timer_prop(list.get(k - 1), prop),
                Err(_) => timer_prop(
                    list.iter()
                        .find(|t| t.name.eq_ignore_ascii_case(arg.trim())),
                    prop,
                ),
            }
        }
        // $play(0) is the queue size; $play(N) selects the Nth item.
        // $play(target,0/N) counts/selects entries for one destination.
        "play" => {
            let list = rt.play.snapshot();
            let selector = a(0);
            if args.len() >= 2 {
                let matching = list
                    .iter()
                    .filter(|item| item.target.eq_ignore_ascii_case(selector.trim()))
                    .collect::<Vec<_>>();
                match a(1).trim().parse::<usize>() {
                    Ok(0) => matching.len().to_string(),
                    Ok(n) => play_prop(matching.get(n - 1).copied(), prop),
                    Err(_) => String::new(),
                }
            } else {
                match selector.trim().parse::<usize>() {
                    Ok(0) => list.len().to_string(),
                    Ok(n) => play_prop(list.get(n - 1), prop),
                    Err(_) => play_prop(
                        list.iter()
                            .find(|item| item.target.eq_ignore_ascii_case(selector.trim())),
                        prop,
                    ),
                }
            }
        }
        // $mircexe — full path to the jIRC executable.
        "mircexe" => std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        // $tempfn[(path)] — a unique temp filename (in the script data dir by default).
        "tempfn" => {
            let base = if a(0).trim().is_empty() {
                rt.data_dir.clone()
            } else {
                super::eval::sandbox_path(&rt.data_dir, a(0).trim())
            };
            base.join(format!(
                "tmp{}_{}",
                std::process::id(),
                process_start().elapsed().as_nanos()
            ))
            .to_string_lossy()
            .into_owned()
        }
        // $findfile/$finddir(dir, wildcard, N[, depth]) — the Nth matching file/dir
        // (N=0 returns the count). Recurses fully by default; an optional depth
        // limits how deep. The N=0 command-callback form is not supported.
        "findfile" | "finddir" => {
            let base = super::eval::sandbox_path(&rt.data_dir, &a(0));
            let wild = a(1);
            let n: usize = a(2).trim().parse().unwrap_or(0);
            let depth: Option<usize> = a(3).trim().parse().ok().filter(|&d| d > 0);
            let mut out = Vec::new();
            find_entries(&base, &wild, name == "finddir", depth, 1, &mut out);
            out.sort();
            if n == 0 {
                out.len().to_string()
            } else {
                out.get(n - 1).cloned().unwrap_or_default()
            }
        }
        // ISUPPORT-derived: $prefix "(modes)chars", $chanmodes "A,B,C,D".
        "prefix" => {
            let is = &rt.state.isupport;
            let modes: String = is.prefix_modes.iter().map(|&(m, _)| m).collect();
            let chars: String = is.prefix_modes.iter().map(|&(_, p)| p).collect();
            format!("({modes}){chars}")
        }
        "chanmodes" => {
            let is = &rt.state.isupport;
            format!(
                "{},{},{},{}",
                is.chanmodes_a, is.chanmodes_b, is.chanmodes_c, is.chanmodes_d
            )
        }
        "chantypes" => rt.state.isupport.chan_types.clone(),
        "modespl" => rt.state.isupport.modes.to_string(),
        // $isalias(name) — $true if a user alias by that name is defined.
        "isalias" => bool_str(
            rt.script
                .find_active_alias_from(&a(0), rt.vars, &rt.event.script_source)
                .is_some(),
        ),
        // $signal = the name of the signal currently being handled (on SIGNAL).
        "signal" => {
            if rt.event.event == "signal" {
                rt.event.chan.clone()
            } else {
                String::new()
            }
        }
        // $group(N|#name)[.status] — script groups. $group(0) = count;
        // $group(N) = the Nth group's name (#-prefixed); $group(#name) = that
        // name if it exists; the `.status` property is `on` or `off`.
        "group" => {
            let sel = a(0);
            if sel == "0" {
                rt.script.groups.len().to_string()
            } else {
                let found = if let Ok(n) = sel.parse::<usize>() {
                    rt.script
                        .groups
                        .get(n.wrapping_sub(1))
                        .map(|(name, _)| name.clone())
                } else {
                    let want = sel.trim_start_matches('#');
                    rt.script
                        .groups
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(want))
                        .map(|(name, _)| name.clone())
                };
                match found {
                    None => String::new(),
                    Some(name) if prop.eq_ignore_ascii_case("status") => {
                        if rt.script.group_enabled(rt.vars, &Some(name)) {
                            "on".into()
                        } else {
                            "off".into()
                        }
                    }
                    Some(name) => format!("#{name}"),
                }
            }
        }
        // $modinv(a, m) — modular multiplicative inverse (empty if none exists).
        "modinv" => {
            let m: i128 = a(1).trim().parse().unwrap_or(0);
            if m <= 0 {
                String::new()
            } else {
                modinv(a(0).trim().parse().unwrap_or(0), m)
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            }
        }
        // $mircpid — the client process id.
        "mircpid" => std::process::id().to_string(),
        // Connection facts (seeded from the profile, via the snapshot).
        "port" => {
            let p = rt.state.server_port;
            if p == 0 {
                String::new()
            } else {
                p.to_string()
            }
        }
        "portfree" => bool_str(port_is_free(&a(0), &a(1))),
        "status" => {
            if rt.state.connect_time != 0 {
                "connected".to_string()
            } else if !rt.state.server_id.is_empty() {
                "connecting".to_string()
            } else {
                "disconnected".to_string()
            }
        }
        "ssl" => bool_str(rt.state.tls),
        "serverip" => rt.state.server_ip.clone(),
        "servertarget" => rt.state.server_target.clone(),
        "sslversion" => rt.state.tls_version.clone(),
        "sslhash" => {
            let certificate = &rt.state.tls_peer_certificate;
            if certificate.is_empty() || (!a(1).is_empty() && !a(1).eq_ignore_ascii_case("s")) {
                String::new()
            } else {
                let digest = match a(0).to_ascii_lowercase().as_str() {
                    "md5" => format!("{:x}", md5::Md5::digest(certificate)),
                    "sha1" => format!("{:x}", sha1::Sha1::digest(certificate)),
                    "sha512" => format!("{:x}", sha2::Sha512::digest(certificate)),
                    "sha256" | "" => format!("{:x}", sha2::Sha256::digest(certificate)),
                    _ => return String::new(),
                };
                if prop.eq_ignore_ascii_case("colons") {
                    digest
                        .as_bytes()
                        .chunks(2)
                        .map(|pair| std::str::from_utf8(pair).unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join(":")
                } else {
                    digest
                }
            }
        }
        "sslcertvalid" => bool_str(rt.state.tls && rt.state.tls_cert_valid),
        "anick" => rt.state.alt_nick.clone(),
        "fullname" => rt.state.realname.clone(),
        "usermode" => rt.state.user_mode.clone(),
        "away" => bool_str(rt.state.away),
        "awaymsg" => rt.state.away_msg.clone(),
        // $online — seconds connected so far; $awaytime — unix time you went away.
        "online" | "onlineserver" | "onlinetotal" => {
            let c = rt.state.connect_time;
            if c == 0 {
                String::new()
            } else {
                now_secs().saturating_sub(c).to_string()
            }
        }
        "awaytime" => {
            let t = rt.state.away_time;
            if t == 0 {
                String::new()
            } else {
                t.to_string()
            }
        }
        // $bvar(&v,N[,M]) — ASCII byte values from 1-based N (N=0 = length).
        // `.word`/`.long` use mIRC's little-endian host order; their `n*`
        // counterparts use network/big-endian order.
        "bvar" => {
            let n: i64 = a(1).trim().parse().unwrap_or(0);
            let m: Option<i64> = a(2).trim().parse().ok();
            match prop {
                "text" | "ansi" => rt.bins.text(&a(0), n, m),
                "word" if m.is_none() => rt.bins.word(&a(0), n, false),
                "nword" if m.is_none() => rt.bins.word(&a(0), n, true),
                "long" if m.is_none() => rt.bins.long(&a(0), n, false),
                "nlong" if m.is_none() => rt.bins.long(&a(0), n, true),
                "" => rt.bins.bvar(&a(0), n, m),
                _ => String::new(),
            }
        }
        // $bfind(&v,N,M) — 1-based position of byte value M (or text) at/after
        // N. Text is caseless by default; `.textcs` is byte-case-sensitive and
        // `.regex` returns the number of regex matches while filling $regml().
        "bfind" => {
            let n: i64 = a(1).trim().parse().unwrap_or(1);
            let needle = a(2);
            if prop == "regex" {
                let Some(bytes) = rt.bins.get(&a(0)).cloned() else {
                    return "0".to_string();
                };
                let start = (n.max(1) as usize).saturating_sub(1).min(bytes.len());
                let subject = String::from_utf8_lossy(&bytes[start..]).into_owned();
                let result_name = a(3);
                clear_regex_results(rt, &result_name);
                return match mirc_regex(&needle) {
                    Ok(spec) => match store_regex_results(rt, &result_name, &subject, &spec) {
                        Ok(count) => {
                            rt.vars.remove(REGERR_KEY);
                            count.to_string()
                        }
                        Err(error) => {
                            clear_regex_results(rt, &result_name);
                            rt.vars.insert(REGERR_KEY.to_string(), error);
                            "0".to_string()
                        }
                    },
                    Err(error) => {
                        rt.vars.insert(REGERR_KEY.to_string(), error);
                        "0".to_string()
                    }
                };
            }
            let numeric: Option<Vec<u8>> = {
                let values: Vec<&str> = needle.split_whitespace().collect();
                (prop != "text"
                    && prop != "textcs"
                    && !values.is_empty()
                    && values
                        .iter()
                        .all(|value| value.parse::<u16>().is_ok_and(|n| n <= 255)))
                .then(|| {
                    values
                        .iter()
                        .map(|value| value.parse::<u8>().unwrap())
                        .collect()
                })
            };
            match numeric.as_deref() {
                Some([byte]) => rt.bins.bfind(&a(0), n, *byte).to_string(),
                Some(bytes) => rt.bins.bfind_text(&a(0), n, bytes).to_string(),
                None => rt
                    .bins
                    .bfind_text_case(&a(0), n, needle.as_bytes(), prop == "textcs")
                    .to_string(),
            }
        }
        // $window(@name|N) — info about a custom window (N=0 = count); properties
        // .lines / .title / .type.
        "window" => {
            let key = a(0);
            let name = match key.parse::<usize>() {
                Ok(0) => return rt.windows.names().len().to_string(),
                Ok(n) => rt.windows.names().get(n - 1).cloned().unwrap_or_default(),
                Err(_) => key,
            };
            match prop {
                "" => {
                    if rt.windows.exists(&name) {
                        name
                    } else {
                        String::new()
                    }
                }
                "lines" => rt.windows.count(&name).to_string(),
                "title" => rt
                    .windows
                    .get(&name)
                    .map(|w| w.title.clone())
                    .unwrap_or_default(),
                "type" => rt
                    .windows
                    .get(&name)
                    .map(|w| w.kind.as_str().to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            }
        }
        // `$webview(name)` — a jIRC native browser window. The base value is
        // its script name while it exists; properties expose cached state only,
        // so identifier evaluation never blocks on WebView2.
        "webview" => {
            let entries = rt.webviews.snapshot(&rt.state.server_id);
            let key = a(0);
            let entry = match key.parse::<usize>() {
                Ok(0) => return entries.len().to_string(),
                Ok(n) => entries.get(n.saturating_sub(1)),
                Err(_) => entries
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(&key)),
            };
            entry.map_or_else(String::new, |entry| {
                match prop.to_ascii_lowercase().as_str() {
                    "" | "name" => entry.name.clone(),
                    "profile" => entry.profile.clone(),
                    "status" => entry.status.clone(),
                    "url" => entry.url.clone(),
                    _ => String::new(),
                }
            })
        }
        // $line(@name, N) — the Nth line of a custom window (1-based).
        "line" => {
            let n: usize = a(1).trim().parse().unwrap_or(0);
            if n == 0 {
                rt.windows.count(&a(0)).to_string()
            } else if prop == "state" {
                if rt.windows.is_selected(&a(0), n) {
                    "1"
                } else {
                    "0"
                }
                .to_string()
            } else {
                rt.windows.line(&a(0), n)
            }
        }
        // `$sline(@name,N)` — selected text/count; `.ln` is its source line.
        "sline" => {
            let name = a(0);
            let n: usize = a(1).trim().parse().unwrap_or(0);
            if n == 0 {
                rt.windows.selected_count(&name).to_string()
            } else if prop == "ln" {
                rt.windows.selected_line(&name, n).unwrap_or(0).to_string()
            } else {
                rt.windows
                    .selected_line(&name, n)
                    .map(|line| rt.windows.line(&name, line))
                    .unwrap_or_default()
            }
        }
        // `$mouse` state while a custom-window menu event is running.
        "mouse" => match prop {
            "x" => rt.event.mouse_x.to_string(),
            "y" => rt.event.mouse_y.to_string(),
            "mx" | "cx" | "dx" => rt.event.mouse_x.to_string(),
            "my" | "cy" | "dy" => rt.event.mouse_y.to_string(),
            "win" => rt.event.mouse_win.clone(),
            "lb" => rt.event.mouse_lb.clone(),
            "key" => rt.event.mouse_key.to_string(),
            _ => format!(
                "{} {} {}",
                rt.event.mouse_x, rt.event.mouse_y, rt.event.mouse_key
            ),
        },
        // `$click(@window,N)` — retained click coordinates, oldest first.
        "click" => {
            let window = a(0);
            let n = a(1).parse::<usize>().unwrap_or(1);
            rt.windows
                .get(&window)
                .and_then(|window| window.clicks.get(n.saturating_sub(1)))
                .map_or_else(String::new, |(x, y)| match prop {
                    "x" => x.to_string(),
                    "y" => y.to_string(),
                    _ => format!("{x} {y}"),
                })
        }
        "getdot" => {
            let x = a(1).parse::<u32>().unwrap_or(u32::MAX);
            let y = a(2).parse::<u32>().unwrap_or(u32::MAX);
            rt.windows
                .dot(&a(0), x, y)
                .map_or_else(String::new, |value| value.to_string())
        }
        "inrect" => {
            let values: Vec<f64> = (0..6)
                .map(|index| a(index).parse().unwrap_or(0.0))
                .collect();
            bool_str(
                values[0] >= values[2]
                    && values[1] >= values[3]
                    && values[0] <= values[2] + values[4]
                    && values[1] <= values[3] + values[5],
            )
        }
        "inellipse" => {
            let values: Vec<f64> = (0..6)
                .map(|index| a(index).parse().unwrap_or(0.0))
                .collect();
            let rx = values[4] / 2.0;
            let ry = values[5] / 2.0;
            let dx = values[0] - (values[2] + rx);
            let dy = values[1] - (values[3] + ry);
            bool_str(rx > 0.0 && ry > 0.0 && dx * dx / (rx * rx) + dy * dy / (ry * ry) <= 1.0)
        }
        "inroundrect" => {
            let values: Vec<f64> = (0..8)
                .map(|index| a(index).parse().unwrap_or(0.0))
                .collect();
            let (px, py, x, y, w, h) = (
                values[0], values[1], values[2], values[3], values[4], values[5],
            );
            let rx = (values[6].abs() / 2.0).min(w.abs() / 2.0);
            let ry = (values[7].abs() / 2.0).min(h.abs() / 2.0);
            let middle = px >= x + rx && px <= x + w - rx && py >= y && py <= y + h;
            let cross = px >= x && px <= x + w && py >= y + ry && py <= y + h - ry;
            let cx = if px < x + rx { x + rx } else { x + w - rx };
            let cy = if py < y + ry { y + ry } else { y + h - ry };
            bool_str(
                middle
                    || cross
                    || (rx > 0.0
                        && ry > 0.0
                        && (px - cx).powi(2) / rx.powi(2) + (py - cy).powi(2) / ry.powi(2) <= 1.0),
            )
        }
        "inpoly" => {
            let px = a(0).parse::<f64>().unwrap_or(0.0);
            let py = a(1).parse::<f64>().unwrap_or(0.0);
            let points: Vec<(f64, f64)> = args[2..]
                .chunks(2)
                .filter(|pair| pair.len() == 2)
                .map(|pair| {
                    (
                        pair[0].parse().unwrap_or(0.0),
                        pair[1].parse().unwrap_or(0.0),
                    )
                })
                .collect();
            bool_str(point_in_polygon((px, py), &points))
        }
        "intersect" => {
            let values: Vec<f64> = (0..8)
                .map(|index| a(index).parse().unwrap_or(0.0))
                .collect();
            let method = a(8).to_ascii_lowercase();
            let method = if method.len() == 2 {
                method
            } else {
                "ll".into()
            };
            line_intersection(
                (values[0], values[1]),
                (values[2], values[3]),
                (values[4], values[5]),
                (values[6], values[7]),
                method.as_bytes()[0] as char,
                method.as_bytes()[1] as char,
            )
            .map_or_else(String::new, |(x, y)| {
                format!("{} {}", fmt_round6(x), fmt_round6(y))
            })
        }
        "onpoly" => {
            let first_count = a(0).parse::<usize>().unwrap_or(0);
            let second_count = a(1).parse::<usize>().unwrap_or(0);
            let coordinates: Vec<f64> = args[2..]
                .iter()
                .map(|value| value.parse().unwrap_or(0.0))
                .collect();
            let first_end = first_count.saturating_mul(2).min(coordinates.len());
            let first = coordinate_pairs(&coordinates[..first_end]);
            let second_end = first_end
                .saturating_add(second_count.saturating_mul(2))
                .min(coordinates.len());
            let second = coordinate_pairs(&coordinates[first_end..second_end]);
            bool_str(polygons_overlap(&first, &second))
        }
        "height" => {
            let size = a(2).parse::<f64>().unwrap_or(14.0).abs();
            (size * 1.2).ceil().to_string()
        }
        "width" => {
            let size = a(2).parse::<f64>().unwrap_or(14.0).abs();
            (a(0).chars().count() as f64 * size * 0.6)
                .ceil()
                .to_string()
        }
        // $replacex (single-pass, non-recursive replace of from/to pairs).
        "replacex" | "replacexcs" => {
            let s = a(0);
            let pairs: Vec<(String, String)> = if args.len() > 1 {
                args[1..]
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| (c[0].clone(), c[1].clone()))
                    .collect()
            } else {
                Vec::new()
            };
            replacex(&s, &pairs, name.eq_ignore_ascii_case("replacexcs"))
        }
        // $powmod(B,E,M) — modular exponentiation (modular inverse for negative E).
        "powmod" => powmod(
            a(0).trim().parse().unwrap_or(0),
            a(1).trim().parse().unwrap_or(0),
            a(2).trim().parse().unwrap_or(0),
        ),
        // Our strings are already UTF-8, so $utfencode/$utfdecode are identity.
        // mIRC represents encoded bytes as one character per byte. This lets a
        // PARSELINE handler explicitly decode an incoming byte string, mutate
        // it, and use `-u0`, or pre-encode an outgoing line the same way.
        "utfencode" => a(0).as_bytes().iter().map(|byte| *byte as char).collect(),
        "utfdecode" => {
            let text = a(0);
            let bytes = super::eval::byte_string_bytes(&text);
            String::from_utf8(bytes).unwrap_or(text)
        }
        // $ticksqpc — high-resolution counter (process-relative nanoseconds).
        "ticksqpc" => process_start().elapsed().as_nanos().to_string(),
        // $encode/$decode — m = base64 (MIME), x = percent-encode (RFC3986). The
        // other switches (a/u/v/y = base32/uucode/z85/puny, b = &binvar) aren't
        // supported yet, so the text passes through unchanged.
        "encode" | "decode" => {
            let text = a(0);
            let sw = a(1);
            let is_enc = name == "encode";
            if sw.contains('m') {
                use base64::{engine::general_purpose::STANDARD, Engine};
                if is_enc {
                    STANDARD.encode(text.as_bytes())
                } else {
                    STANDARD
                        .decode(text.as_bytes())
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default()
                }
            } else if sw.contains('x') {
                if is_enc {
                    percent_encode(&text)
                } else {
                    percent_decode(&text)
                }
            } else {
                text
            }
        }
        "gettok" => {
            let sep = sep_code(&a(2));
            let text = a(0);
            // mIRC token identifiers ignore null tokens. Consecutive, leading,
            // and trailing delimiters therefore do not change token positions.
            let toks: Vec<&str> = text.split(sep).filter(|tok| !tok.is_empty()).collect();
            gettok_range(&toks, &a(1), sep)
        }
        "numtok" => {
            let sep = a(1)
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .unwrap_or(' ');
            a(0).split(sep)
                .filter(|tok| !tok.is_empty())
                .count()
                .to_string()
        }
        "hget" => {
            // $hget(table) -> table name if it exists; $hget(table, item) -> value;
            // $hget(table, N).item / .data -> Nth key name / value in sorted order
            // (N=0 -> the item count), for iterating a table.
            let Some(table) = resolve_hash_table(rt, &a(0)) else {
                return if args.len() < 2 && a(0) == "0" {
                    super::hash::table_names(rt.hashes).len().to_string()
                } else {
                    String::new()
                };
            };
            if args.len() < 2 {
                if prop.eq_ignore_ascii_case("size") {
                    super::hash::slots(rt.hashes, &table).to_string()
                } else {
                    table
                }
            } else if prop.eq_ignore_ascii_case("unset") {
                let item = resolve_hash_item(rt, &table, &a(1));
                if let Some(item) = item {
                    rt.hash_expiry
                        .get(&(table, item))
                        .map(|expiry| {
                            expiry
                                .seconds_remaining(std::time::Instant::now())
                                .to_string()
                        })
                        .unwrap_or_else(|| "0".to_string())
                } else {
                    String::new()
                }
            } else if prop.eq_ignore_ascii_case("item") || prop.eq_ignore_ascii_case("data") {
                match rt.hashes.get(&table) {
                    Some(h) => {
                        let mut keys: Vec<&String> = h.keys().collect();
                        keys.sort_by_key(|key| key.to_ascii_lowercase());
                        let n: usize = a(1).parse().unwrap_or(0);
                        if n == 0 {
                            keys.len().to_string()
                        } else if let Some(k) = keys.get(n - 1) {
                            if prop.eq_ignore_ascii_case("item") {
                                (*k).clone()
                            } else {
                                h.get(*k)
                                    .map(|value| super::hash::value_text(value))
                                    .unwrap_or_default()
                            }
                        } else {
                            String::new()
                        }
                    }
                    None => String::new(),
                }
            } else {
                let item = resolve_hash_item(rt, &table, &a(1));
                let value = item
                    .as_ref()
                    .and_then(|item| rt.hashes.get(&table)?.get(item));
                if args.len() >= 3 && a(2).trim_start().starts_with('&') {
                    let output = a(2);
                    let bytes = value
                        .map(|value| super::hash::value_bytes(value))
                        .unwrap_or_default();
                    rt.bins.unset(&output);
                    rt.bins.set(&output, 1, &bytes, false);
                    bytes.len().to_string()
                } else {
                    value
                        .map(|value| super::hash::value_text(value))
                        .unwrap_or_default()
                }
            }
        }
        "hfind" => eval_hfind_expanded(rt, args, prop, None),
        // Socket identifiers (used inside on SOCKOPEN/SOCKREAD/SOCKCLOSE).
        "sock" => {
            // $sock(name) -> the name if a matching socket exists (else empty),
            // so `if ($sock(x))` works. `$sock(pattern,0)` is the match count and
            // `$sock(pattern,N)` is the Nth matching name; a property on the Nth
            // form reads that resolved socket. `$sock(name).property` reads any
            // property (.port/.ip/.addr/.status/.mark/.sent/.rcvd/.ls/.lr/.to/...).
            let name = a(0);
            let n: usize = if args.len() > 1 {
                a(1).parse().unwrap_or(1)
            } else {
                1
            };
            let names = rt.sockets.matching_names(&name);
            if n == 0 {
                names.len().to_string()
            } else if let Some(name) = names.get(n - 1) {
                if prop.is_empty() {
                    name.clone()
                } else {
                    rt.sockets.prop(name, prop)
                }
            } else {
                String::new()
            }
        }
        "sockname" => rt.event.chan.clone(),
        "sockbr" => rt
            .vars
            .get(SOCK_BR_KEY)
            .cloned()
            .unwrap_or_else(|| "0".to_string()),
        "sockerr" => rt.event.sock_error.to_string(),
        "replace" => {
            // $replace(text, from1, to1, from2, to2, ...) -> sequential replaces.
            // Case-INSENSITIVE in mIRC ($replacecs is the case-sensitive form);
            // hex byte-escapers rely on this (matching lowercase `$hmac` output
            // against uppercase `0A`/`5C`/… literals).
            let mut text = a(0);
            let mut i = 1;
            while i + 1 < args.len() {
                if !args[i].is_empty() {
                    text = replace_ci(&text, &args[i], &args[i + 1]);
                }
                i += 2;
            }
            text
        }
        "remove" => {
            // $remove(text, substr1, substr2, ...) -> remove all of each.
            // Case-insensitive in mIRC (matches lowercase/uppercase alike).
            let mut text = a(0);
            for s in args.iter().skip(1).filter(|s| !s.is_empty()) {
                text = replace_ci(&text, s, "");
            }
            text
        }
        "instok" => {
            // $instok(text, token, N, C) -> insert token at the Nth position.
            // Negative N counts from the end (-1 = before the last element).
            let sep = sep_code(&a(3));
            let mut toks: Vec<String> = if a(0).is_empty() {
                Vec::new()
            } else {
                a(0).split(sep).map(String::from).collect()
            };
            let len = toks.len() as i64;
            let raw: i64 = a(2).trim().parse().unwrap_or(1);
            let idx = (if raw < 0 {
                (len + raw).max(0)
            } else {
                raw.max(1) - 1
            } as usize)
                .min(toks.len());
            toks.insert(idx, a(1));
            toks.join(&sep.to_string())
        }
        "reptok" => {
            // $reptok(text, token, new, N, C) -> replace the Nth matching token
            // (N=0 -> all) with `new`.
            let sep = sep_code(&a(4));
            let token = a(1);
            let new = a(2);
            let n: usize = a(3).parse().unwrap_or(1);
            let mut count = 0usize;
            let out: Vec<String> = a(0)
                .split(sep)
                .map(|t| {
                    if t.eq_ignore_ascii_case(&token) {
                        count += 1;
                        if n == 0 || count == n {
                            return new.clone();
                        }
                    }
                    t.to_string()
                })
                .collect();
            out.join(&sep.to_string())
        }
        "lastpos" => {
            // $lastpos(text, string, N) -> position of the Nth-from-last
            // occurrence (default last); 0 if not found.
            let needle = a(1);
            let hay = a(0);
            if needle.is_empty() {
                "0".to_string()
            } else {
                let n = a(2).parse::<usize>().unwrap_or(1).max(1);
                let positions = ci_match_indices(&hay, &needle);
                if positions.len() >= n {
                    let byte_idx = positions[positions.len() - n];
                    (hay[..byte_idx].chars().count() + 1).to_string()
                } else {
                    "0".to_string()
                }
            }
        }
        "pos" => {
            // $pos(text, string, N) -> 1-based position of the Nth occurrence
            // (default 1st); N=0 returns the number of matches.
            let needle = a(1);
            let hay = a(0);
            let n = a(2).parse::<usize>().unwrap_or(1);
            let positions = ci_match_indices(&hay, &needle);
            if n == 0 {
                return positions.len().to_string();
            }
            match positions.get(n - 1) {
                Some(&byte_idx) => (hay[..byte_idx].chars().count() + 1).to_string(),
                None => String::new(),
            }
        }
        "count" => {
            // $count(text, substr1, substr2, ...) -> total occurrences of all.
            let hay = a(0);
            let total: usize = args
                .iter()
                .skip(1)
                .filter(|s| !s.is_empty())
                .map(|s| ci_match_indices(&hay, s).len())
                .sum();
            total.to_string()
        }
        // Case-sensitive variants (mIRC appends `cs`). The base identifiers are
        // case-insensitive; these use exact matching.
        "poscs" => {
            let (hay, needle) = (a(0), a(1));
            let n = a(2).parse::<usize>().unwrap_or(1);
            if n == 0 {
                return hay.matches(needle.as_str()).count().to_string();
            }
            match hay.match_indices(needle.as_str()).nth(n - 1) {
                Some((b, _)) => (hay[..b].chars().count() + 1).to_string(),
                None => String::new(),
            }
        }
        "countcs" => {
            let hay = a(0);
            args.iter()
                .skip(1)
                .filter(|s| !s.is_empty())
                .map(|s| hay.matches(s.as_str()).count())
                .sum::<usize>()
                .to_string()
        }
        "replacecs" => {
            let mut text = a(0);
            let mut i = 1;
            while i + 1 < args.len() {
                if !args[i].is_empty() {
                    text = text.replace(args[i].as_str(), args[i + 1].as_str());
                }
                i += 2;
            }
            text
        }
        "removecs" => {
            let mut text = a(0);
            for s in args.iter().skip(1).filter(|s| !s.is_empty()) {
                text = text.replace(s.as_str(), "");
            }
            text
        }
        "istokcs" => {
            let sep = sep_code(&a(2));
            let needle = a(1);
            bool_str(!needle.is_empty() && a(0).split(sep).any(|t| t == needle.as_str()))
        }
        "findtokcs" => {
            let sep = sep_code(&a(3));
            let needle = a(1);
            let n = a(2).parse::<usize>().unwrap_or(1);
            let mut seen = 0;
            let mut result = 0;
            for (i, t) in a(0).split(sep).enumerate() {
                if t == needle.as_str() {
                    seen += 1;
                    if seen == n {
                        result = i + 1;
                        break;
                    }
                }
            }
            if n == 0 {
                seen.to_string()
            } else {
                result.to_string()
            }
        }
        "addtokcs" => {
            let sep = sep_code(&a(2));
            let (list, tok) = (a(0), a(1));
            if tok.is_empty() || list.split(sep).any(|t| t == tok.as_str()) {
                list
            } else if list.is_empty() {
                tok
            } else {
                format!("{list}{sep}{tok}")
            }
        }
        "remtokcs" => {
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let (list, token) = (a(0), a(1));
            let mut seen = 0;
            list.split(sep)
                .filter(|t| {
                    if *t == token.as_str() {
                        seen += 1;
                        n != 0 && seen != n
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>()
                .join(&sep.to_string())
        }
        "reptokcs" => {
            let sep = sep_code(&a(4));
            let (token, new) = (a(1), a(2));
            let n: usize = a(3).parse().unwrap_or(1);
            let mut count = 0usize;
            a(0).split(sep)
                .map(|t| {
                    if t == token.as_str() {
                        count += 1;
                        if n == 0 || count == n {
                            return new.clone();
                        }
                    }
                    t.to_string()
                })
                .collect::<Vec<_>>()
                .join(&sep.to_string())
        }
        "matchtokcs" => {
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let (list, needle) = (a(0), a(1));
            let m: Vec<&str> = list
                .split(sep)
                .filter(|t| t.contains(needle.as_str()))
                .collect();
            if n == 0 {
                m.len().to_string()
            } else {
                m.get(n - 1).copied().unwrap_or("").to_string()
            }
        }
        "wildtokcs" => {
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let (list, wild) = (a(0), a(1));
            let m: Vec<&str> = list
                .split(sep)
                .filter(|t| wildcard_match_cs(&wild, t))
                .collect();
            if n == 0 {
                m.len().to_string()
            } else {
                m.get(n - 1).copied().unwrap_or("").to_string()
            }
        }
        "reverse" => a(0).chars().rev().collect(),
        "abs" => a(0)
            .parse::<f64>()
            .map(|n| fmt_num(n.abs()))
            .unwrap_or_default(),
        "int" => a(0)
            .parse::<f64>()
            .map(|n| (n.trunc() as i64).to_string())
            .unwrap_or_default(),
        "ceil" => a(0)
            .parse::<f64>()
            .map(|n| (n.ceil() as i64).to_string())
            .unwrap_or_default(),
        "floor" => a(0)
            .parse::<f64>()
            .map(|n| (n.floor() as i64).to_string())
            .unwrap_or_default(),
        "min" => num2(&a(0), &a(1), f64::min),
        "max" => num2(&a(0), &a(1), f64::max),
        "addtok" => {
            // $addtok(list, token, sepcode)
            let sep = sep_code(&a(2));
            let exists = a(0).split(sep).any(|t| t.eq_ignore_ascii_case(&a(1)));
            if exists || a(1).is_empty() {
                a(0)
            } else if a(0).is_empty() {
                a(1)
            } else {
                format!("{}{}{}", a(0), sep, a(1))
            }
        }
        "istok" => {
            // $istok(list, token, sepcode) -> $true/$false
            let sep = sep_code(&a(2));
            if !a(1).is_empty() && a(0).split(sep).any(|t| t.eq_ignore_ascii_case(&a(1))) {
                "$true".to_string()
            } else {
                "$false".to_string()
            }
        }
        "findtok" => {
            // $findtok(list, token, N, sepcode) -> position of the Nth match (else 0)
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let mut seen = 0;
            let mut result = 0;
            for (i, t) in a(0).split(sep).enumerate() {
                if t.eq_ignore_ascii_case(&a(1)) {
                    seen += 1;
                    if seen == n {
                        result = i + 1;
                        break;
                    }
                }
            }
            if n == 0 {
                seen.to_string()
            } else {
                result.to_string()
            }
        }
        "deltok" => {
            // $deltok(list, N[-N2], sepcode) -> list with token(s) removed
            let sep = sep_code(&a(2));
            let list = a(0);
            let toks: Vec<&str> = list.split(sep).collect();
            let (lo, hi) = parse_range(&a(1), toks.len());
            toks.iter()
                .enumerate()
                .filter(|(i, _)| {
                    let p = i + 1;
                    p < lo || p > hi
                })
                .map(|(_, t)| *t)
                .collect::<Vec<_>>()
                .join(&sep.to_string())
        }
        "remtok" => {
            // $remtok(list, token, N, sepcode) -> remove the Nth occurrence of token
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let (list, token) = (a(0), a(1));
            let mut seen = 0;
            list.split(sep)
                .filter(|t| {
                    if t.eq_ignore_ascii_case(&token) {
                        seen += 1;
                        n != 0 && seen != n
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>()
                .join(&sep.to_string())
        }
        "puttok" => {
            // $puttok(list, token, N, sepcode) -> replace the Nth token.
            // Negative N counts from the end (-1 = last token).
            let sep = sep_code(&a(3));
            let mut toks: Vec<String> = a(0).split(sep).map(String::from).collect();
            let len = toks.len() as i64;
            let raw: i64 = a(2).trim().parse().unwrap_or(0);
            let n = if raw < 0 { len + raw + 1 } else { raw };
            if n >= 1 && n <= len {
                toks[(n - 1) as usize] = a(1);
            }
            toks.join(&sep.to_string())
        }
        "sorttok" | "sorttokcs" => {
            // $sorttok(list, sepcode, [options]) -> sorted.
            // opts: a=alpha (default), n=numeric, c=channel-prefix, r=reverse.
            // The `cs` form sorts alphabetically case-sensitively.
            let sep = sep_code(&a(1));
            let opts = a(2).to_lowercase();
            let cs = name.eq_ignore_ascii_case("sorttokcs");
            let mut toks: Vec<String> = a(0).split(sep).map(String::from).collect();
            if opts.contains('c') {
                // Channel prefix order ~ & @ % + then none; stable within a rank.
                let rank = |t: &str| match t.chars().next() {
                    Some('~') => 0,
                    Some('&') => 1,
                    Some('@') => 2,
                    Some('%') => 3,
                    Some('+') => 4,
                    _ => 5,
                };
                toks.sort_by_key(|t| rank(t));
            } else if opts.contains('n') {
                toks.sort_by(|x, y| {
                    let (x, y) = (
                        x.parse::<f64>().unwrap_or(0.0),
                        y.parse::<f64>().unwrap_or(0.0),
                    );
                    x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                });
            } else if cs {
                toks.sort();
            } else {
                toks.sort_by(|x, y| x.to_lowercase().cmp(&y.to_lowercase()));
            }
            if opts.contains('r') {
                toks.reverse();
            }
            toks.join(&sep.to_string())
        }
        "wildtok" => {
            // $wildtok(list, wildcard, N, sepcode) -> Nth matching token (N=0 -> count)
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let (list, wild) = (a(0), a(1));
            let m: Vec<&str> = list
                .split(sep)
                .filter(|t| wildcard_match(&wild, t))
                .collect();
            if n == 0 {
                m.len().to_string()
            } else {
                m.get(n - 1).copied().unwrap_or("").to_string()
            }
        }
        "matchtok" => {
            // $matchtok(list, substring, N, sepcode) -> Nth token containing substring
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1);
            let needle = a(1).to_lowercase();
            let list = a(0);
            let m: Vec<&str> = list
                .split(sep)
                .filter(|t| t.to_lowercase().contains(&needle))
                .collect();
            if n == 0 {
                m.len().to_string()
            } else {
                m.get(n - 1).copied().unwrap_or("").to_string()
            }
        }
        "qt" => {
            let s = a(0);
            if s.contains(' ') && !(s.starts_with('"') && s.ends_with('"')) {
                format!("\"{s}\"")
            } else {
                s
            }
        }
        "noqt" => {
            // $noqt(text) -> remove outer enclosing quotes.
            let s = a(0);
            if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
                s[1..s.len() - 1].to_string()
            } else {
                s
            }
        }
        "envvar" => {
            // $envvar(name) -> env var value; $envvar(0) -> count; $envvar(N) -> Nth name.
            let arg = a(0);
            match arg.parse::<usize>() {
                Ok(0) => std::env::vars().count().to_string(),
                Ok(n) => std::env::vars()
                    .nth(n - 1)
                    .map(|(k, _)| k)
                    .unwrap_or_default(),
                Err(_) => std::env::var(&arg).unwrap_or_default(),
            }
        }
        "bytes" => {
            // $bytes(N) -> comma-formatted; $bytes(N).suf -> human-readable suffix.
            let n: f64 = a(0).parse().unwrap_or(0.0);
            if prop.eq_ignore_ascii_case("suf") {
                let units = ["", "K", "M", "G", "T"];
                let mut v = n.abs();
                let mut i = 0;
                while v >= 1024.0 && i < units.len() - 1 {
                    v /= 1024.0;
                    i += 1;
                }
                if i == 0 {
                    (n as i64).to_string()
                } else {
                    format!("{:.2}{}", v, units[i])
                }
            } else {
                comma_format(n as i64)
            }
        }
        "strip" => strip_codes_opts(&a(0), &a(1)),
        // $notags(line) -> the line with a leading IRCv3 message-tag block
        // (`@key=val;... `) removed. Tags only ever prefix a line, so drop up to
        // the first space; a tags-only string becomes empty.
        "notags" => {
            let t = a(0);
            match t.strip_prefix('@') {
                Some(rest) => rest
                    .split_once(' ')
                    .map_or(String::new(), |(_, r)| r.to_string()),
                None => t,
            }
        }
        "regex" => {
            // $regex([name,] text, pattern) -> full-match count. Each name owns
            // an independent $regml/$regmlex result set; an unnamed call uses
            // mIRC's default result set.
            let (result_name, text, pat) = if args.len() >= 3 {
                (a(0), a(1), a(2))
            } else {
                (String::new(), a(0), a(1))
            };
            clear_regex_results(rt, &result_name);
            match mirc_regex(&pat) {
                Ok(spec) => {
                    let matched_text = spec.prepare_text(&text);
                    match store_regex_results(rt, &result_name, &matched_text, &spec) {
                        Ok(count) => {
                            rt.vars.remove(REGERR_KEY);
                            count.to_string()
                        }
                        Err(e) => {
                            clear_regex_results(rt, &result_name);
                            rt.vars.insert(REGERR_KEY.to_string(), e);
                            "0".to_string()
                        }
                    }
                }
                Err(e) => {
                    rt.vars.insert(REGERR_KEY.to_string(), e);
                    "0".to_string()
                }
            }
        }
        "regml" => {
            // $regml([name,] N, [&binvar]); N=0 returns the number of capture
            // strings, and the properties expose the captured span metadata.
            let (result_name, n_index) = if args.len() >= 2 && !is_integer_arg(&a(0)) {
                (a(0), 1)
            } else {
                (String::new(), 0)
            };
            let n: i64 = a(n_index).trim().parse().unwrap_or(1);
            let value = regex_result(rt, &result_name, &format!("flat.{n}"), prop);
            let bin = args.get(n_index + 1).map(|s| s.trim()).unwrap_or("");
            save_regex_binvar(rt, bin, value)
        }
        "regmlex" => {
            // $regmlex([name,] M, N) -> Nth capture group of the Mth match (N
            // defaults to 1). N=-1 returns the full match, matching mIRC.
            let named = args.first().is_some_and(|s| !is_integer_arg(s));
            let i = usize::from(named);
            let result_name = if named { a(0) } else { String::new() };
            let m: i64 = a(i).trim().parse().unwrap_or(1);
            let has_n = args.get(i + 1).is_some_and(|value| is_integer_arg(value));
            let n: i64 = if has_n {
                a(i + 1).trim().parse().unwrap_or(1)
            } else {
                1
            };
            let suffix = if n == -1 {
                format!("match.{m}.full")
            } else if n == 0 {
                format!("match.{m}.count")
            } else {
                format!("match.{m}.{n}")
            };
            let value = regex_result(rt, &result_name, &suffix, prop);
            let bin_index = i + if has_n { 2 } else { 1 };
            let bin = args.get(bin_index).map(|s| s.trim()).unwrap_or("");
            save_regex_binvar(rt, bin, value)
        }
        "regsub" => {
            // Backward-compatible expression form. The standard output-variable
            // form is intercepted before generic argument expansion and handled
            // by `eval_regsub()` below.
            regsub_replace(rt, "", &a(0), &a(1), &a(2)).0
        }
        "regerrstr" => rt.vars.get(REGERR_KEY).cloned().unwrap_or_default(),
        "read" => eval_read(rt, args),
        // $readn -> the line number matched by the last $read (0 if none).
        "readn" => rt
            .vars
            .get(READN_KEY)
            .cloned()
            .unwrap_or_else(|| "0".into()),
        // Number of lines selected by the most recent `/filter`.
        "filtered" => rt
            .vars
            .get(super::eval::FILTERED_KEY)
            .cloned()
            .unwrap_or_else(|| "0".into()),
        "lines" => {
            // $lines(file) -> number of lines in the file.
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            std::fs::read_to_string(&path)
                .map(|c| c.lines().count())
                .unwrap_or(0)
                .to_string()
        }
        "feof" => bool_str(rt.files.feof),
        "ferr" => bool_str(rt.files.ferr),
        "fread" => rt.files.read_line(&a(0)),
        "fgetc" => rt.files.read_char(&a(0)),
        "fopen" => {
            // $fopen(N) -> Nth open handle (0 = count); $fopen(name) -> the name if
            // open; properties .fname/.pos/.eof/.err.
            let key = a(0);
            let name = match key.parse::<usize>() {
                Ok(0) => return rt.files.count().to_string(),
                Ok(n) => rt.files.names().get(n - 1).cloned().unwrap_or_default(),
                Err(_) => key,
            };
            match prop {
                "" => {
                    if rt.files.handle(&name).is_some() {
                        name
                    } else {
                        String::new()
                    }
                }
                "fname" => rt
                    .files
                    .handle(&name)
                    .map(|h| h.path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                "pos" => rt
                    .files
                    .handle(&name)
                    .map(|h| h.pos.to_string())
                    .unwrap_or_default(),
                "eof" => bool_str(rt.files.handle(&name).map(|h| h.eof).unwrap_or(false)),
                "err" => bool_str(rt.files.handle(&name).map(|h| h.err).unwrap_or(false)),
                _ => String::new(),
            }
        }
        "readini" => {
            // `$readini(file,[np],section,item)`: mIRC evaluates the stored value
            // once by default; `n` returns it as plain text. `p` is accepted as
            // the command-pipe option and does not itself disable evaluation.
            let switches = a(1).to_ascii_lowercase();
            let has_switches = args.len() >= 4
                && !switches.is_empty()
                && switches.chars().all(|c| matches!(c, 'n' | 'p'));
            let off = if has_switches { 1 } else { 0 };
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let value = super::ini::read(&text, &a(1 + off), &a(2 + off)).unwrap_or_default();
            finish_file_read(
                rt,
                value,
                has_switches && switches.contains('n'),
                has_switches && switches.contains('p'),
            )
        }
        "ini" => {
            // $ini(file, N) -> Nth section (N=0 -> count); $ini(file, section) -> its
            // 1-based index. $ini(file, section, N) -> Nth item; (.., item) -> index.
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if args.len() >= 3 {
                let items = super::ini::items(&text, &a(1));
                match a(2).parse::<usize>() {
                    Ok(0) => items.len().to_string(),
                    Ok(n) => items.get(n - 1).cloned().unwrap_or_default(),
                    Err(_) => items
                        .iter()
                        .position(|k| k.eq_ignore_ascii_case(&a(2)))
                        .map(|i| (i + 1).to_string())
                        .unwrap_or_else(|| "0".to_string()),
                }
            } else {
                let secs = super::ini::sections(&text);
                match a(1).parse::<usize>() {
                    Ok(0) => secs.len().to_string(),
                    Ok(n) => secs.get(n - 1).cloned().unwrap_or_default(),
                    Err(_) => secs
                        .iter()
                        .position(|s| s.eq_ignore_ascii_case(&a(1)))
                        .map(|i| (i + 1).to_string())
                        .unwrap_or_else(|| "0".to_string()),
                }
            }
        }
        // ---- File-name & misc utility identifiers ----
        "comchar" => "/".to_string(),
        "mkfn" | "mknickfn" => mkfn(&a(0)),
        "iptype" => {
            // mIRC: "ipv4" / "ipv6" for a valid address, else $null (empty).
            let s = a(0);
            if s.parse::<std::net::Ipv4Addr>().is_ok() {
                "ipv4".to_string()
            } else if s.parse::<std::net::Ipv6Addr>().is_ok() {
                "ipv6".to_string()
            } else {
                String::new()
            }
        }
        "eval" => {
            // mIRC `$eval(text,N)` evaluates text N times (default 1; N=0 → not
            // evaluated). Args arrive already expanded once, so N≤1 returns it as-is
            // and N≥2 expands the remaining N-1 times.
            let n: i64 = a(1).trim().parse().unwrap_or(1);
            let mut s = a(0);
            for _ in 1..n.max(1) {
                s = rt.expand(&s);
            }
            s
        }
        // A user-defined alias used as an identifier ($myalias): run it and use
        // its `/return` value.
        other => {
            if let Some((body, source, source_line)) = rt
                .script
                .find_active_alias_from(other, rt.vars, &rt.event.script_source)
                .map(|alias| (alias.body.clone(), alias.source.clone(), alias.source_line))
            {
                // This alias is being used as an identifier — flag it for
                // $caller/$isid, keep $show true (identifiers aren't silenced),
                // expose the `.property` as $prop, and record its return for $result.
                let saved = rt.caller;
                let saved_show = rt.show;
                let saved_prop = rt.vars.insert(PROP_KEY.to_string(), prop.to_string());
                rt.caller = "identifier";
                rt.show = true;
                let result = rt.call_named_alias_in_source(
                    other,
                    &body,
                    args.to_vec(),
                    &source,
                    source_line,
                );
                rt.vars
                    .insert(super::eval::RESULT_KEY.to_string(), result.clone());
                rt.caller = saved;
                rt.show = saved_show;
                match saved_prop {
                    Some(v) => {
                        rt.vars.insert(PROP_KEY.to_string(), v);
                    }
                    None => {
                        rt.vars.remove(PROP_KEY);
                    }
                }
                return result;
            }
            // mIRC identifiers that cannot be evaluated return `$null`. Keeping
            // the source text visible makes an unknown identifier truthy and can
            // take the opposite branch from the same script in mIRC.
            String::new()
        }
    }
}

fn eval_client_list_ident(
    rt: &Runtime<'_>,
    args: &[String],
    prop: &str,
    key: &str,
    kind: &str,
) -> String {
    let list = rt
        .vars
        .get(key)
        .map(|value| {
            value
                .split('\u{1f}')
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if args.is_empty() {
        return bool_str(!list.is_empty()).to_string();
    }
    let selector = &args[0];
    if selector == "0" {
        return list.len().to_string();
    }
    let numeric = selector.parse::<usize>().ok();
    let index = numeric
        .filter(|number| *number > 0 && *number <= list.len())
        .map(|number| number - 1)
        .or_else(|| {
            list.iter()
                .position(|item| item.eq_ignore_ascii_case(selector))
        });
    let Some(position) = index else {
        return if numeric.is_some() {
            String::new()
        } else {
            "0".into()
        };
    };
    if prop.is_empty() {
        return if numeric.is_some() {
            list[position].to_string()
        } else {
            (position + 1).to_string()
        };
    }
    match (kind, prop.to_ascii_lowercase().as_str()) {
        ("ignore", "type") => "pcntikd".into(),
        ("ignore", "secs") => "0".into(),
        ("ignore", "network") => String::new(),
        ("highlight", "text") => list[position].to_string(),
        ("highlight", "flash" | "regex" | "cs") => "$false".into(),
        ("highlight", "message" | "nicks") => "$true".into(),
        ("highlight", "color" | "sound" | "chans") => String::new(),
        ("font", "size" | "pitch" | "type") => String::new(),
        _ => String::new(),
    }
}

fn client_on_off(rt: &Runtime<'_>, key: &str) -> String {
    rt.vars.get(key).cloned().unwrap_or_else(|| "off".into())
}

fn process_command_line<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn portable_from_executable(executable: &std::path::Path) -> bool {
    executable
        .parent()
        .is_some_and(|directory| directory.join("portable.txt").is_file())
}

fn same_sandbox_path(sandbox: &std::path::Path, left: &str, right: &str) -> bool {
    fn resolved(sandbox: &std::path::Path, value: &str) -> std::path::PathBuf {
        let path = super::eval::sandbox_path(sandbox, value);
        path.canonicalize().unwrap_or_else(|_| {
            let root = sandbox
                .canonicalize()
                .unwrap_or_else(|_| sandbox.to_path_buf());
            root.join(path.file_name().unwrap_or_default())
        })
    }

    let left = resolved(sandbox, left);
    let right = resolved(sandbox, right);
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn channel_names_equal(state: &crate::irc::state::StateSnapshot, known: &str, query: &str) -> bool {
    let query = state.isupport.channel_target(query).unwrap_or(query);
    state.isupport.names_equal(known, query)
}

fn find_channel<'a>(
    state: &'a crate::irc::state::StateSnapshot,
    query: &str,
) -> Option<&'a crate::irc::state::ChannelView> {
    state
        .channels
        .iter()
        .find(|channel| channel_names_equal(state, &channel.name, query))
}

fn irc_wildcard(state: &crate::irc::state::StateSnapshot, pattern: &str, text: &str) -> bool {
    wildcard_match_cs(
        &state.isupport.casefold(pattern),
        &state.isupport.casefold(text),
    )
}

fn ial_query_matches(
    state: &crate::irc::state::StateSnapshot,
    query: &str,
    nick: &str,
    full: &str,
) -> bool {
    irc_wildcard(state, query, nick) || wildcard_match(query, full)
}

/// Applies mIRC's optional `aohvr` nickname-list filter. `a` means every
/// member; `o`/`h`/`v` use the server's PREFIX mapping and `r` means no prefix.
fn nick_filter_matches(
    isupport: &crate::irc::state::Isupport,
    prefixes: &str,
    filter: &str,
) -> bool {
    if filter.is_empty() || filter.chars().any(|kind| kind.to_ascii_lowercase() == 'a') {
        return true;
    }
    filter.chars().any(|kind| match kind.to_ascii_lowercase() {
        'o' => isupport
            .prefix_for_mode('o')
            .is_some_and(|prefix| prefixes.contains(prefix)),
        'h' => isupport
            .prefix_for_mode('h')
            .is_some_and(|prefix| prefixes.contains(prefix)),
        'v' => isupport
            .prefix_for_mode('v')
            .is_some_and(|prefix| prefixes.contains(prefix)),
        'r' => prefixes.is_empty(),
        _ => false,
    })
}

fn nick_value(
    state: &crate::irc::state::StateSnapshot,
    nick: &str,
    prefixes: &str,
    last_activity: Option<u64>,
    prop: &str,
) -> String {
    match prop.to_ascii_lowercase().as_str() {
        "pnick" => format!("{prefixes}{nick}"),
        "prefix" => prefixes.to_string(),
        "mode" => prefixes
            .chars()
            .filter_map(|prefix| state.isupport.mode_for_prefix(prefix))
            .collect(),
        "account" => find_ial_info(state, nick, "")
            .map(|info| info.account.clone())
            .unwrap_or_default(),
        "away" => find_ial_info(state, nick, "")
            .and_then(|info| info.away)
            .map(bool_str)
            .unwrap_or_default(),
        "idle" => last_activity
            .map(|last| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs().saturating_sub(last))
                    .unwrap_or(0)
                    .to_string()
            })
            .unwrap_or_default(),
        _ => nick.to_string(),
    }
}

/// Formats the default value and currently representable properties of an IAL
/// entry. Rich WHOX-only fields (account/away/gecos/id) remain empty until the
/// connection state stores them.
fn find_ial_info<'a>(
    state: &'a crate::irc::state::StateSnapshot,
    nick: &str,
    full: &str,
) -> Option<&'a crate::irc::state::IalView> {
    state.ial_info.iter().find(|info| {
        state.isupport.names_equal(&info.nick, nick)
            || (!full.is_empty() && info.address.eq_ignore_ascii_case(full))
    })
}

fn ial_value(
    full: &str,
    prop: &str,
    prefixes: &str,
    info: Option<&crate::irc::state::IalView>,
) -> String {
    let (nick, address) = full.split_once('!').unwrap_or((full, ""));
    let (user, host) = address.split_once('@').unwrap_or((address, ""));
    match prop.to_ascii_lowercase().as_str() {
        "nick" => nick.to_string(),
        "user" => user.to_string(),
        "host" => host.to_string(),
        "addr" => address.to_string(),
        "pnick" => format!("{prefixes}{nick}"),
        "account" => info.map(|v| v.account.clone()).unwrap_or_default(),
        "away" => info.and_then(|v| v.away).map(bool_str).unwrap_or_default(),
        "gecos" => info.map(|v| v.gecos.clone()).unwrap_or_default(),
        "id" => info.map(|v| v.id.clone()).unwrap_or_default(),
        "mark" => info
            .and_then(|v| {
                v.marks
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("default"))
            })
            .map(|(_, text)| text.clone())
            .unwrap_or_default(),
        _ => full.to_string(),
    }
}

/// Builds a wildcard hostmask from a `nick!user@host` address, following mIRC's
/// `$mkfn`/`$mknickfn` — replace characters that are invalid in a filename
/// (`\ / : * ? " < > |` and control chars) with `_`, so the result is safe on disk.
fn mkfn(name: &str) -> String {
    name.chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                || (c as u32) < 0x20
            {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// `$mask`/`$address` type table (1–10; anything else → `*!*@host`).
pub(super) fn mask_address(addr: &str, kind: u32) -> String {
    let (nick, rest) = addr.split_once('!').unwrap_or(("*", addr));
    let (user, host) = rest.split_once('@').unwrap_or((rest, "*"));
    // "*user": drop a leading ident marker (~^=+-) and prepend '*'.
    let star_user = format!("*{}", user.trim_start_matches(['~', '^', '=', '+', '-']));
    // "*.host": replace the first host segment with '*' (else just '*').
    let dot_host = match host.split_once('.') {
        Some((_, tail)) => format!("*.{tail}"),
        None => "*".to_string(),
    };
    match kind {
        1 => format!("*!{user}@{host}"),
        2 => format!("*!{star_user}@{host}"),
        3 => format!("*!*@{host}"),
        4 => format!("*!{star_user}@{dot_host}"),
        5 => format!("*!*@{dot_host}"),
        6 => format!("{nick}!{user}@{host}"),
        7 => format!("{nick}!{star_user}@{host}"),
        8 => format!("{nick}!*@{host}"),
        9 => format!("{nick}!{star_user}@{dot_host}"),
        10 => format!("{nick}!*@{dot_host}"),
        _ => format!("*!*@{host}"),
    }
}

/// Reserved variable-key prefix where `$regex` stashes capture groups for
/// `$regml` (the NUL char can't appear in a user `%var` name).
const REGML_PREFIX: &str = "\u{0}re\u{0}";

/// Reserved var key where the last regex compile error is stashed, for `$regerrstr`.
const REGERR_KEY: &str = "\u{0}regerr";

fn regex_namespace(name: &str) -> String {
    format!("{REGML_PREFIX}{}\u{0}", name.trim().to_ascii_lowercase())
}

fn regex_key(name: &str, suffix: &str) -> String {
    format!("{}{suffix}", regex_namespace(name))
}

fn clear_regex_results(rt: &mut Runtime, name: &str) {
    let namespace = regex_namespace(name);
    rt.vars.retain(|key, _| !key.starts_with(&namespace));
}

fn is_integer_arg(value: &str) -> bool {
    value.trim().parse::<i64>().is_ok()
}

/// Returns a stored capture value/property. The `value` suffix is implicit;
/// metadata uses the same base with `.pos`, `.bytepos`, `.group`, and `.match`.
fn regex_result(rt: &Runtime, name: &str, suffix: &str, prop: &str) -> String {
    let property = match prop.to_ascii_lowercase().as_str() {
        "" => "value",
        "pos" => "pos",
        "bytepos" => "bytepos",
        "group" => "group",
        "match" => "match",
        _ => return String::new(),
    };
    rt.vars
        .get(&regex_key(name, &format!("{suffix}.{property}")))
        .cloned()
        .unwrap_or_default()
}

fn save_regex_binvar(rt: &mut Runtime, name: &str, value: String) -> String {
    if name.starts_with('&') {
        let bytes = value.as_bytes();
        rt.bins.set(name, 1, bytes, true);
        bytes.len().to_string()
    } else {
        value
    }
}

fn store_regex_capture(
    rt: &mut Runtime,
    name: &str,
    suffix: &str,
    value: &str,
    start: usize,
    text: &str,
    group: usize,
    match_number: usize,
) {
    // PCRE's `\C` can report a byte offset in the middle of a UTF-8 codepoint.
    // Do not slice the Rust string at that boundary; mIRC exposes both a
    // character position and a byte position, so count the valid/lossy prefix.
    let start = start.min(text.len());
    let char_pos = String::from_utf8_lossy(&text.as_bytes()[..start])
        .chars()
        .count()
        + 1;
    for (property, value) in [
        ("value", value.to_string()),
        ("pos", char_pos.to_string()),
        ("bytepos", (start + 1).to_string()),
        ("group", group.to_string()),
        ("match", match_number.to_string()),
    ] {
        rt.vars
            .insert(regex_key(name, &format!("{suffix}.{property}")), value);
    }
}

/// Saves capture strings in both mIRC views: `$regml()` is a flat list across
/// matches and `$regmlex(M,N)` addresses one global match. The full match is
/// available as `$regmlex(M,-1)` but is not included in `$regml(0)`.
fn store_regex_results(
    rt: &mut Runtime,
    name: &str,
    text: &str,
    spec: &MircRegex,
) -> Result<usize, String> {
    let mut full_match_count = 0usize;
    let mut flat_count = 0usize;
    for caps in spec.regex.captures_iter(text.as_bytes()) {
        let caps = caps.map_err(|e| e.to_string())?;
        full_match_count += 1;
        let full = caps.get(0).expect("captures always contain a full match");
        let full_value = String::from_utf8_lossy(full.as_bytes());
        store_regex_capture(
            rt,
            name,
            &format!("match.{full_match_count}.full"),
            &full_value,
            full.start(),
            text,
            0,
            full_match_count,
        );

        let mut per_match_count = 0usize;
        for group in 1..caps.len() {
            let capture = caps.get(group);
            let include =
                spec.fixed_groups || capture.is_some_and(|matched| !matched.as_bytes().is_empty());
            if !include {
                continue;
            }
            per_match_count += 1;
            flat_count += 1;
            let (value, start) = capture.map_or_else(
                || (String::new(), full.start()),
                |matched| {
                    (
                        String::from_utf8_lossy(matched.as_bytes()).into_owned(),
                        matched.start(),
                    )
                },
            );
            store_regex_capture(
                rt,
                name,
                &format!("match.{full_match_count}.{per_match_count}"),
                &value,
                start,
                text,
                group,
                full_match_count,
            );
            store_regex_capture(
                rt,
                name,
                &format!("flat.{flat_count}"),
                &value,
                start,
                text,
                group,
                full_match_count,
            );
        }
        rt.vars.insert(
            regex_key(name, &format!("match.{full_match_count}.count.value")),
            per_match_count.to_string(),
        );
        if !spec.global {
            break;
        }
    }
    rt.vars
        .insert(regex_key(name, "flat.0.value"), flat_count.to_string());
    rt.vars.insert(
        regex_key(name, "matches.value"),
        full_match_count.to_string(),
    );
    Ok(full_match_count)
}

/// Expands mIRC `$regsub` replacement backreferences. The PCRE2 Rust wrapper
/// intentionally has no replacement API, so render capture references while
/// rebuilding the output from match spans. Consecutive digits allow the
/// unlimited capture numbers supported by current mIRC.
fn render_regsub_replacement(s: &str, caps: &pcre2::bytes::Captures<'_>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && matches!(chars.get(i + 1), Some(c) if c.is_ascii_digit()) {
            let mut end = i + 1;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            let group = chars[i + 1..end]
                .iter()
                .collect::<String>()
                .parse::<usize>()
                .unwrap_or(0);
            if let Some(value) = caps.get(group) {
                out.push_str(&String::from_utf8_lossy(value.as_bytes()));
            }
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Turns a token "sepcode" argument (an ASCII code) into its character; spaces
/// are the mIRC default when absent or invalid.
pub(super) struct MircRegex {
    regex: pcre2::bytes::Regex,
    global: bool,
    fixed_groups: bool,
    strip_codes: bool,
}

impl MircRegex {
    fn prepare_text(&self, text: &str) -> String {
        if self.strip_codes {
            strip_codes_opts(text, "")
        } else {
            text.to_string()
        }
    }

    pub(super) fn is_match(&self, text: &str) -> Result<bool, String> {
        let text = self.prepare_text(text);
        self.regex
            .is_match(text.as_bytes())
            .map_err(|e| e.to_string())
    }
}

/// Compiles mIRC's `/pattern/modifiers` form with PCRE2. `g`, `F`, and `S` are
/// wrapper behaviours; PCRE handles the pattern language and the remaining
/// matching modifiers. mIRC still embeds PCRE1, so a few obscure engine-version
/// differences remain, but using PCRE2 preserves the advanced constructs that
/// mIRC scripts commonly rely on (look-around, backreferences, recursion,
/// conditionals, branch-reset groups, and named captures).
pub(super) fn mirc_regex(pat: &str) -> Result<MircRegex, String> {
    let p = pat.trim();
    let (body, flags) = match (p.strip_prefix('/'), p.rfind('/')) {
        (Some(_), Some(close)) if close > 0 => (&p[1..close], &p[close + 1..]),
        _ => (p, ""),
    };

    // PCRE1 spells this start option `(*UTF8)` while PCRE2 spells it `(*UTF)`.
    // The builder already enables UTF for Rust strings, so consume both forms.
    // Consume `(*UCP)` too and apply it through the builder. Do not discard
    // arbitrary leading `(*...)` verbs: LIMIT/NO_JIT/SKIP/FAIL and friends are
    // meaningful PCRE constructs.
    let mut body = body.trim_start();
    let mut pattern_ucp = false;
    loop {
        if let Some(rest) = body.strip_prefix("(*UTF8)") {
            body = rest.trim_start();
        } else if let Some(rest) = body.strip_prefix("(*UTF)") {
            body = rest.trim_start();
        } else if let Some(rest) = body.strip_prefix("(*UCP)") {
            pattern_ucp = true;
            body = rest.trim_start();
        } else {
            break;
        }
    }

    let mut full = body.to_string();
    if flags.contains('D') || flags.contains('E') {
        full = dollar_endonly_pattern(&full);
    }
    if flags.contains('U') {
        full = format!("(?U){full}");
    }
    if flags.contains('A') {
        full = format!(r"\A(?:{full})");
    }

    let mut builder = pcre2::bytes::RegexBuilder::new();
    builder
        .caseless(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dotall(flags.contains('s'))
        .extended(flags.contains('x'))
        // Runtime strings are valid UTF-8. Keeping UTF enabled also preserves
        // jIRC's existing Unicode capture/position behaviour when `/u` is not
        // present; `/u` additionally makes \w/\d/\s Unicode-property aware.
        .utf(true)
        .ucp(flags.contains('u') || pattern_ucp);
    let regex = builder.build(&full).map_err(|e| e.to_string())?;
    Ok(MircRegex {
        regex,
        global: flags.contains('g'),
        fixed_groups: flags.contains('F'),
        strip_codes: flags.contains('S'),
    })
}

/// Shared event-match helper for mIRC's `$` remote-event matchtext prefix.
pub(super) fn mirc_regex_is_match(text: &str, pattern: &str) -> bool {
    mirc_regex(pattern)
        .and_then(|spec| spec.is_match(text))
        .unwrap_or(false)
}

/// PCRE's D/E modifiers make `$` match only the absolute end of the subject.
/// The safe Rust wrapper does not expose `PCRE2_DOLLAR_ENDONLY`, so translate
/// unescaped dollar assertions to PCRE's equivalent `\z`. Dollars inside a
/// character class or `\Q...\E` quote remain literal.
fn dollar_endonly_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    let mut quoted = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(c);
            if let Some(next) = chars.next() {
                out.push(next);
                if !in_class {
                    if next == 'Q' {
                        quoted = true;
                    } else if next == 'E' {
                        quoted = false;
                    }
                }
            }
            continue;
        }
        if !quoted {
            if c == '[' && !in_class {
                in_class = true;
            } else if c == ']' && in_class {
                in_class = false;
            }
        }
        if c == '$' && !in_class && !quoted {
            out.push_str(r"\z");
        } else {
            out.push(c);
        }
    }
    out
}

/// Performs a `$regsub` replacement and keeps the same `$regml()` capture
/// side effects as `$regex`/`$regsubex`. The caller decides whether the
/// resulting text is returned directly or assigned to an output variable.
fn regsub_replace(
    rt: &mut Runtime,
    result_name: &str,
    text: &str,
    pattern: &str,
    replacement: &str,
) -> (String, usize) {
    clear_regex_results(rt, result_name);
    let spec = match mirc_regex(pattern) {
        Ok(regex) => regex,
        Err(error) => {
            rt.vars.insert(REGERR_KEY.to_string(), error);
            return (text.to_string(), 0);
        }
    };
    rt.vars.remove(REGERR_KEY);
    let text = spec.prepare_text(text);
    if let Err(error) = store_regex_results(rt, result_name, &text, &spec) {
        clear_regex_results(rt, result_name);
        rt.vars.insert(REGERR_KEY.to_string(), error);
        return (text, 0);
    }

    let source = text.as_bytes();
    let mut output = Vec::with_capacity(source.len());
    let mut last = 0;
    let mut count = 0;
    for result in spec
        .regex
        .captures_iter(source)
        .take(if spec.global { usize::MAX } else { 1 })
    {
        let captures = match result {
            Ok(captures) => captures,
            Err(error) => {
                rt.vars.insert(REGERR_KEY.to_string(), error.to_string());
                return (text, 0);
            }
        };
        let Some(matched) = captures.get(0) else {
            continue;
        };
        output.extend_from_slice(&source[last..matched.start()]);
        output.extend_from_slice(render_regsub_replacement(replacement, &captures).as_bytes());
        last = matched.end();
        count += 1;
    }
    output.extend_from_slice(&source[last..]);
    (String::from_utf8_lossy(&output).into_owned(), count)
}

fn assign_regex_output(rt: &mut Runtime, target: &str, value: &str) {
    let target = target.trim();
    if target.starts_with('&') {
        rt.bins.unset(target);
        rt.bins.set(target, 1, value.as_bytes(), false);
    } else if target.starts_with('%') {
        rt.set_visible_var(
            target.trim_start_matches('%').to_string(),
            value.to_string(),
        );
    }
}

fn finish_regex_output(
    rt: &mut Runtime,
    raw: &[String],
    output_index: Option<usize>,
    value: String,
    count: usize,
) -> String {
    if let Some(index) = output_index {
        let target = raw.get(index).map_or("", |argument| argument).trim();
        assign_regex_output(rt, target, &value);
        count.to_string()
    } else {
        value
    }
}

/// `$regsub([name,] text, pattern, subtext, %var|&binvar)` assigns the
/// replaced text and returns the substitution count. The historical jIRC
/// three-argument result form remains supported for backwards compatibility.
pub fn eval_regsub(rt: &mut Runtime, raw: &[String]) -> String {
    let output_index = raw
        .last()
        .filter(|value| {
            let value = value.trim_start();
            value.starts_with('%') || value.starts_with('&')
        })
        .map(|_| raw.len() - 1);
    let core_len = output_index.unwrap_or(raw.len());
    let offset = usize::from(core_len >= 4);
    let result_name = if offset == 1 {
        rt.expand(raw.first().map_or("", |value| value))
    } else {
        String::new()
    };
    let text = rt.expand(raw.get(offset).map_or("", |value| value));
    let pattern = rt.expand(raw.get(offset + 1).map_or("", |value| value));
    let replacement = rt.expand(raw.get(offset + 2).map_or("", |value| value));
    let (result, count) = regsub_replace(rt, &result_name, &text, &pattern, &replacement);
    finish_regex_output(rt, raw, output_index, result, count)
}

/// `$regsubex([name,] text, pattern, subtext)` — replace each match of `pattern`
/// in `text` with the *evaluated* `subtext`. `subtext` arrives RAW (the engine
/// bypasses pre-expansion for `$regsubex`) and is evaluated once per match, after
/// its markers are substituted: `\t`=whole match, `\1`..`\9`=capture group,
/// `\0`=number of groups, `\n`=match number, `\a`/`\A`=all groups (spaced/joined).
pub fn eval_regsubex(rt: &mut Runtime, raw: &[String]) -> String {
    let output_index = raw
        .last()
        .filter(|_| raw.len() >= 5)
        .filter(|value| {
            let value = value.trim_start();
            value.starts_with('%') || value.starts_with('&')
        })
        .map(|_| raw.len() - 1);
    let core_len = output_index.unwrap_or(raw.len());
    let off = usize::from(core_len >= 4); // skip an optional leading [name]
    let result_name = if off == 1 {
        rt.expand(raw.first().map_or("", |s| s))
    } else {
        String::new()
    };
    let text = rt.expand(raw.get(off).map_or("", |s| s));
    let pat = rt.expand(raw.get(off + 1).map_or("", |s| s));
    let subtext = raw.get(off + 2).cloned().unwrap_or_default();
    clear_regex_results(rt, &result_name);
    let spec = match mirc_regex(&pat) {
        Ok(r) => r,
        Err(e) => {
            rt.vars.insert(REGERR_KEY.to_string(), e);
            return finish_regex_output(rt, raw, output_index, text, 0);
        }
    };
    rt.vars.remove(REGERR_KEY);
    let text = spec.prepare_text(&text);
    let group_count = spec.regex.captures_len().saturating_sub(1);
    if let Err(e) = store_regex_results(rt, &result_name, &text, &spec) {
        clear_regex_results(rt, &result_name);
        rt.vars.insert(REGERR_KEY.to_string(), e);
        return finish_regex_output(rt, raw, output_index, text, 0);
    }
    // Collect match spans + groups first (immutable borrow of `text`), then
    // evaluate each replacement (which needs a mutable borrow of `rt`).
    let mut matches: Vec<(usize, usize, Vec<String>)> = Vec::new();
    for result in spec
        .regex
        .captures_iter(text.as_bytes())
        .take(if spec.global { usize::MAX } else { 1 })
    {
        let caps = match result {
            Ok(caps) => caps,
            Err(e) => {
                rt.vars.insert(REGERR_KEY.to_string(), e.to_string());
                return finish_regex_output(rt, raw, output_index, text, 0);
            }
        };
        let Some(matched) = caps.get(0) else {
            continue;
        };
        let groups = (0..caps.len())
            .map(|group| {
                caps.get(group).map_or_else(String::new, |value| {
                    String::from_utf8_lossy(value.as_bytes()).into_owned()
                })
            })
            .collect();
        matches.push((matched.start(), matched.end(), groups));
    }
    let source = text.as_bytes();
    let mut out = Vec::with_capacity(source.len());
    let mut last = 0;
    for (n, (start, end, groups)) in matches.iter().enumerate() {
        out.extend_from_slice(&source[last..*start]);
        out.extend_from_slice(
            rt.expand(&regsubex_fill(&subtext, groups, n + 1, group_count))
                .as_bytes(),
        );
        last = *end;
    }
    out.extend_from_slice(&source[last..]);
    let result = String::from_utf8_lossy(&out).into_owned();
    finish_regex_output(rt, raw, output_index, result, matches.len())
}

/// Encodes a captured group value so it survives being substituted into the
/// regsubex subtext and re-evaluated. mSL-structural characters — `( ) [ ] { }
/// $ % , &` — would otherwise re-parse (e.g. a captured `(` makes `$asc(\1)`
/// become `$asc(()`, which mis-matches parens and corrupts/drops the byte).
/// Each is replaced by the `$chr(N)` that evaluates back to it, so the value is
/// preserved exactly while the surrounding subtext still parses. mIRC binds `\1`
/// as a value rather than re-tokenizing it, so this matches its behaviour.
fn subtext_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' | ')' | '[' | ']' | '{' | '}' | '$' | '%' | ',' | '&' => {
                out.push_str(&format!("$chr({})", c as u32));
            }
            _ => out.push(c),
        }
    }
    out
}

/// Substitutes the `$regsubex` subtext markers for one match.
fn regsubex_fill(subtext: &str, groups: &[String], match_num: usize, group_count: usize) -> String {
    let chars: Vec<char> = subtext.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let c = chars[i + 1];
            i += 2;
            match c {
                't' => out.push_str(&subtext_literal(groups.first().map_or("", |s| s))),
                'n' => out.push_str(&match_num.to_string()),
                'a' => out.push_str(&subtext_literal(
                    &groups.iter().skip(1).cloned().collect::<Vec<_>>().join(" "),
                )),
                'A' => out.push_str(&subtext_literal(
                    &groups.iter().skip(1).cloned().collect::<String>(),
                )),
                '0'..='9' => {
                    let idx = c as usize - '0' as usize;
                    if idx == 0 {
                        out.push_str(&group_count.to_string());
                    } else {
                        out.push_str(&subtext_literal(groups.get(idx).map_or("", |s| s)));
                    }
                }
                '\\' => out.push('\\'),
                // An unrecognised escape keeps its backslash — mIRC leaves `\*`
                // etc. literal, which scripts rely on (e.g. `\* iswm \1` uses the
                // wildcard "\*" to tell an escape sequence from a plain char when
                // converting text to bytes for HMAC).
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn sep_code(s: &str) -> char {
    s.trim()
        .parse::<u32>()
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(' ')
}

fn bool_str(b: bool) -> String {
    if b { "$true" } else { "$false" }.to_string()
}

fn coordinate_pairs(values: &[f64]) -> Vec<(f64, f64)> {
    values
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

fn cross((ax, ay): (f64, f64), (bx, by): (f64, f64)) -> f64 {
    ax * by - ay * bx
}

fn line_intersection(
    p: (f64, f64),
    p2: (f64, f64),
    q: (f64, f64),
    q2: (f64, f64),
    first_kind: char,
    second_kind: char,
) -> Option<(f64, f64)> {
    let r = (p2.0 - p.0, p2.1 - p.1);
    let s = (q2.0 - q.0, q2.1 - q.1);
    let denominator = cross(r, s);
    if denominator.abs() <= f64::EPSILON {
        return None;
    }
    let qp = (q.0 - p.0, q.1 - p.1);
    let t = cross(qp, s) / denominator;
    let u = cross(qp, r) / denominator;
    let accepts = |kind: char, value: f64| match kind {
        'r' => value >= 0.0,
        's' => (0.0..=1.0).contains(&value),
        _ => true,
    };
    (accepts(first_kind, t) && accepts(second_kind, u)).then_some((p.0 + t * r.0, p.1 + t * r.1))
}

fn point_on_segment(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> bool {
    let segment = (end.0 - start.0, end.1 - start.1);
    let relative = (point.0 - start.0, point.1 - start.1);
    cross(segment, relative).abs() <= 1e-9
        && point.0 >= start.0.min(end.0) - 1e-9
        && point.0 <= start.0.max(end.0) + 1e-9
        && point.1 >= start.1.min(end.1) - 1e-9
        && point.1 <= start.1.max(end.1) + 1e-9
}

fn point_in_polygon(point: (f64, f64), points: &[(f64, f64)]) -> bool {
    if points.len() < 3 {
        return false;
    }
    let mut inside = false;
    for index in 0..points.len() {
        let current = points[index];
        let previous = points[(index + points.len() - 1) % points.len()];
        if point_on_segment(point, previous, current) {
            return true;
        }
        if (current.1 > point.1) != (previous.1 > point.1)
            && point.0
                < (previous.0 - current.0) * (point.1 - current.1) / (previous.1 - current.1)
                    + current.0
        {
            inside = !inside;
        }
    }
    inside
}

fn polygons_overlap(first: &[(f64, f64)], second: &[(f64, f64)]) -> bool {
    if first.len() < 3 || second.len() < 3 {
        return false;
    }
    for first_index in 0..first.len() {
        let a = first[first_index];
        let b = first[(first_index + 1) % first.len()];
        for second_index in 0..second.len() {
            let c = second[second_index];
            let d = second[(second_index + 1) % second.len()];
            if line_intersection(a, b, c, d, 's', 's').is_some()
                || point_on_segment(a, c, d)
                || point_on_segment(c, a, b)
            {
                return true;
            }
        }
    }
    point_in_polygon(first[0], second) || point_in_polygon(second[0], first)
}

/// `$gettok` index/range resolver: `N`, `N-`, `N1-N2`, and negative indices
/// (`-1` = last token). Returns the joined slice, or empty if out of range.
fn gettok_range(toks: &[&str], spec: &str, sep: char) -> String {
    let len = toks.len() as i64;
    let norm = |n: i64| if n < 0 { len + n + 1 } else { n };
    let spec = spec.trim();
    // A '-' after position 0 marks a range; a leading '-' is a negative index.
    let range_dash = spec
        .char_indices()
        .find(|&(i, c)| c == '-' && i > 0)
        .map(|(i, _)| i);
    let (lo, hi) = match range_dash {
        Some(p) => {
            let l = spec[..p].trim();
            let r = spec[p + 1..].trim();
            let l = if l.is_empty() {
                1
            } else {
                norm(l.parse().unwrap_or(1))
            };
            let r = if r.is_empty() {
                len
            } else {
                norm(r.parse().unwrap_or(len))
            };
            (l, r)
        }
        None => {
            let n = norm(spec.parse().unwrap_or(0));
            (n, n)
        }
    };
    if lo < 1 || lo > len {
        return String::new();
    }
    let lo = lo as usize;
    let hi = hi.clamp(lo as i64, len) as usize;
    toks[lo - 1..hi].join(&sep.to_string())
}

/// Formats a number of seconds as mIRC's `$duration` (e.g. `1day2hrs3mins`).
fn format_duration(mut s: i64) -> String {
    if s <= 0 {
        return "0secs".to_string();
    }
    let units = [
        ("wk", 604800),
        ("day", 86400),
        ("hr", 3600),
        ("min", 60),
        ("sec", 1),
    ];
    let mut out = String::new();
    for (name, size) in units {
        let n = s / size;
        if n > 0 {
            out.push_str(&format!("{n}{name}{}", if n == 1 { "" } else { "s" }));
            s -= n * size;
        }
    }
    out
}

fn format_duration_without_seconds(seconds: u64) -> String {
    if seconds < 60 {
        "0mins".to_string()
    } else {
        format_duration((seconds - seconds % 60) as i64)
    }
}

fn port_is_free(port: &str, bind_ip: &str) -> bool {
    let Ok(port) = port.trim().parse::<u16>() else {
        return false;
    };
    if port == 0 {
        return false;
    }

    let ip = if bind_ip.trim().is_empty() {
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    } else {
        let Ok(ip) = bind_ip.trim().parse::<std::net::IpAddr>() else {
            return false;
        };
        ip
    };
    std::net::TcpListener::bind((ip, port)).is_ok()
}

/// `$base(N, frombase, tobase, [zeropad])` — integer base conversion, 2..=36.
/// The fractional part (if any) is dropped; output digits A–Z are uppercase.
fn base_convert(n: &str, inb: u32, outb: u32, zeropad: usize) -> String {
    if !(2..=36).contains(&inb) || !(2..=36).contains(&outb) {
        return String::new();
    }
    let intpart = n.trim().split('.').next().unwrap_or("").trim();
    let Ok(val) = i64::from_str_radix(intpart, inb) else {
        return String::new();
    };
    let mut out = to_radix(val.unsigned_abs(), outb);
    while out.len() < zeropad {
        out.insert(0, '0');
    }
    if val < 0 {
        out.insert(0, '-');
    }
    out
}

fn to_radix(mut v: u64, base: u32) -> String {
    if v == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let base = base as u64;
    let mut bytes = Vec::new();
    while v > 0 {
        bytes.push(DIGITS[(v % base) as usize]);
        v /= base;
    }
    bytes.reverse();
    String::from_utf8(bytes).unwrap_or_default()
}

/// Parses a token index spec (`N`, `N-M`, or `N-`) into an inclusive 1-based
/// range, clamped against `len`.
fn parse_range(spec: &str, len: usize) -> (usize, usize) {
    let spec = spec.trim();
    if let Some((lo, hi)) = spec.split_once('-') {
        let lo = lo.trim().parse().unwrap_or(1);
        let hi = if hi.trim().is_empty() {
            len
        } else {
            hi.trim().parse().unwrap_or(lo)
        };
        (lo, hi)
    } else {
        let n = spec.parse().unwrap_or(0);
        (n, n)
    }
}

/// Removes mIRC formatting control codes (bold, colour, underline, …).
/// `$strip(text[, options])` — remove control codes. Options select which:
/// b=bold u=underline r=reverse c=colour i=italics e=strikethrough. With no
/// options everything is stripped (reset/monospace too); with options, only the
/// chosen codes (reset/monospace left in place).
pub(super) fn strip_codes_opts(s: &str, options: &str) -> String {
    let opts = options.to_lowercase();
    let all = opts.trim().is_empty();
    let has = |c: char| all || opts.contains(c);
    let (bold, uline, rev, color, ital, strike) =
        (has('b'), has('u'), has('r'), has('c'), has('i'), has('e'));
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\u{2}' if bold => i += 1,
            '\u{1f}' if uline => i += 1,
            '\u{16}' if rev => i += 1,
            '\u{1d}' if ital => i += 1,
            '\u{1e}' if strike => i += 1,
            // Reset and monospace have no option letter — strip only in the
            // default "remove everything" mode.
            '\u{f}' | '\u{11}' if all => i += 1,
            '\u{3}' if color => {
                i += 1;
                let mut d = 0;
                while d < 2 && matches!(chars.get(i), Some(c) if c.is_ascii_digit()) {
                    i += 1;
                    d += 1;
                }
                if chars.get(i) == Some(&',')
                    && matches!(chars.get(i + 1), Some(c) if c.is_ascii_digit())
                {
                    i += 1;
                    let mut d2 = 0;
                    while d2 < 2 && matches!(chars.get(i), Some(c) if c.is_ascii_digit()) {
                        i += 1;
                        d2 += 1;
                    }
                }
            }
            '\u{4}' if color => {
                i += 1;
                let mut d = 0;
                while d < 6 && matches!(chars.get(i), Some(c) if c.is_ascii_hexdigit()) {
                    i += 1;
                    d += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats an integer with thousands separators (for `$bytes`).
fn comma_format(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Monotonic clock origin shared by `$ticks` and `$ticksqpc`.
fn process_start() -> std::time::Instant {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

/// Milliseconds since this process started (for `$ticks` — scripts use deltas).
fn ticks() -> u64 {
    process_start().elapsed().as_millis() as u64
}

/// Recursively collect matching file or directory paths under `base` for
/// $findfile/$finddir. `depth` starts at 1 (base level); `max_depth` (if set)
/// caps how many levels deep to search.
fn find_entries(
    base: &std::path::Path,
    wild: &str,
    want_dirs: bool,
    max_depth: Option<usize>,
    depth: usize,
    out: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = path.is_dir();
        let fname = entry.file_name().to_string_lossy().into_owned();
        if is_dir == want_dirs && wildcard_match(wild, &fname) {
            out.push(path.to_string_lossy().into_owned());
        }
        if is_dir && max_depth.map_or(true, |d| depth < d) {
            find_entries(&path, wild, want_dirs, max_depth, depth + 1, out);
        }
    }
}

/// Byte offsets of every non-overlapping, case-insensitive match of `needle` in
/// `hay`. ASCII case-folding keeps offsets aligned with the original string.
fn ci_match_indices(hay: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    hay.to_ascii_lowercase()
        .match_indices(&needle.to_ascii_lowercase())
        .map(|(i, _)| i)
        .collect()
}

/// Case-insensitive substring replace (mIRC's `$replace`). ASCII-case-folding
/// keeps byte offsets aligned, so non-ASCII text is preserved correctly.
fn replace_ci(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_string();
    }
    let hay = text.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::new();
    let mut last = 0;
    while let Some(rel) = hay[last..].find(&needle) {
        let at = last + rel;
        out.push_str(&text[last..at]);
        out.push_str(to);
        last = at + from.len();
    }
    out.push_str(&text[last..]);
    out
}

fn replacex(s: &str, pairs: &[(String, String)], cs: bool) -> String {
    if pairs.is_empty() {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for (from, to) in pairs {
            let fl = from.len();
            if fl == 0 || rest.len() < fl || !rest.is_char_boundary(fl) {
                continue;
            }
            let hit = if cs {
                &rest[..fl] == from.as_str()
            } else {
                rest.as_bytes()[..fl].eq_ignore_ascii_case(from.as_bytes())
            };
            if hit {
                out.push_str(to);
                rest = &rest[fl..];
                continue 'outer;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

fn modpow(mut base: u128, mut exp: u128, m: u128) -> u128 {
    if m <= 1 {
        return 0;
    }
    let mut result = 1u128;
    base %= m;
    while exp > 0 {
        if exp & 1 == 1 {
            result = result * base % m;
        }
        exp >>= 1;
        base = base * base % m;
    }
    result
}

fn modinv(a: i128, m: i128) -> Option<i128> {
    let (mut old_r, mut r) = (a.rem_euclid(m), m);
    let (mut old_s, mut s) = (1i128, 0i128);
    while r != 0 {
        let q = old_r / r;
        let nr = old_r - q * r;
        old_r = r;
        r = nr;
        let ns = old_s - q * s;
        old_s = s;
        s = ns;
    }
    if old_r == 1 {
        Some(old_s.rem_euclid(m))
    } else {
        None
    }
}

/// $powmod(B,E,M) = B^E mod M; for negative E, the modular inverse is used.
/// Inputs are i64 so the u128 products in modpow cannot overflow.
fn powmod(b: i64, e: i64, m: i64) -> String {
    if m <= 0 {
        return String::new();
    }
    let (m128, b128) = (m as i128, b as i128);
    if e >= 0 {
        modpow(b128.rem_euclid(m128) as u128, e as u128, m as u128).to_string()
    } else {
        match modinv(b128, m128) {
            Some(inv) => modpow(inv as u128, (-(e as i128)) as u128, m as u128).to_string(),
            None => String::new(),
        }
    }
}

/// Formats a unixtime `ts` in local time using a mIRC format string ($asctime).
fn asctime(ts: i64, mirc_fmt: &str) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format(&mirc_to_chrono(mirc_fmt)).to_string(),
        None => String::new(),
    }
}

/// Translates a mIRC date/time format into a chrono format string. Letter runs
/// map to fields (y=year m=month d=day h=12h H=24h n=minutes s=seconds t=AM/PM
/// z=timezone); other characters pass through literally — like mIRC, a literal
/// letter that's also a code (e.g. the `y` in "Day") is interpreted as the code.
fn mirc_to_chrono(fmt: &str) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if "ymdhHnstz".contains(c) {
            let mut j = i;
            while j < chars.len() && chars[j] == c {
                j += 1;
            }
            out.push_str(mirc_token(c, j - i));
            i = j;
        } else {
            if c == '%' {
                out.push('%'); // escape literal % for chrono
            }
            out.push(c);
            i += 1;
        }
    }
    out
}

fn mirc_token(c: char, n: usize) -> &'static str {
    match (c, n) {
        ('y', 2) => "%y",
        ('y', _) => "%Y",
        ('m', 1) => "%-m",
        ('m', 2) => "%m",
        ('m', 3) => "%b",
        ('m', _) => "%B",
        ('d', 1) => "%-d",
        ('d', 2) => "%d",
        ('d', 3) => "%a",
        ('d', _) => "%A",
        ('h', 1) => "%-I",
        ('h', _) => "%I",
        ('H', 1) => "%-H",
        ('H', _) => "%H",
        ('n', 1) => "%-M",
        ('n', _) => "%M",
        ('s', 1) => "%-S",
        ('s', _) => "%S",
        ('t', _) => "%p",
        ('z', _) => "%Z",
        _ => "",
    }
}

fn take_left(s: &str, n: i64) -> String {
    if n >= 0 {
        s.chars().take(n as usize).collect()
    } else {
        let len = s.chars().count() as i64;
        s.chars().take((len + n).max(0) as usize).collect()
    }
}

fn take_right(s: &str, n: i64) -> String {
    let len = s.chars().count() as i64;
    if n >= 0 {
        s.chars().skip((len - n).max(0) as usize).collect()
    } else {
        s.chars().skip((-n) as usize).collect()
    }
}

/// A small, time-seeded xorshift PRNG (no external dependency).
fn rand_range(lo: i64, hi: i64) -> i64 {
    if lo >= hi {
        return lo;
    }
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15)
        | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let span = (hi - lo + 1) as u64;
    lo + (x % span) as i64
}

fn num2(a: &str, b: &str, f: fn(f64, f64) -> f64) -> String {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => fmt_num(f(x, y)),
        _ => String::new(),
    }
}

/// Key under which `$read` records the matched line number for `$readn`.
const READN_KEY: &str = "\u{0}readn";

/// Key holding the `.property` of the custom identifier being evaluated, for `$prop`.
const PROP_KEY: &str = "\u{0}prop";

/// `$read(file [, ntswrp] [, matchtext] [, N])`. Without switches it returns the
/// Nth line (1-based) or a random line. The `w`/`s`/`r` switches search from line
/// N (default 1): `w` = wildcard match (returns the whole line), `s` = line
/// starting with matchtext (returns the remainder), `r` = regex. `$readn` is set
/// to the matched line number (0 if no match). Returned text is evaluated once
/// unless `n` is present, matching mIRC; `t` and `p` retain their parsing roles.
fn eval_read(rt: &mut Runtime, args: &[String]) -> String {
    let a = |i: usize| args.get(i).cloned().unwrap_or_default();
    let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    // mIRC stops text reads at the first NUL and accepts CR, LF, or CRLF line
    // endings. Normalize only for line selection; returned line text is intact.
    let content = content.split('\0').next().unwrap_or("");
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let arg1 = a(1);
    let arg1 = arg1.trim();
    // arg1 is a switch string when it's present and not a plain number.
    let has_switches = !arg1.is_empty() && arg1.parse::<i64>().is_err();
    let switches = if has_switches {
        arg1.to_ascii_lowercase()
    } else {
        String::new()
    };
    let no_eval = switches.contains('n');
    let command_pipes = switches.contains('p');
    // Unless `t` is specified, a numeric first line is mIRC's line-count
    // header and is not part of the readable data. `$read(file,0)` returns it.
    let header = (!switches.contains('t'))
        .then(|| lines.first()?.trim().parse::<usize>().ok())
        .flatten();
    let data_lines = if header.is_some() && !lines.is_empty() {
        &lines[1..]
    } else {
        lines.as_slice()
    };

    if has_switches {
        let sw = &switches;
        let matchtext = a(2);
        let from = a(3).trim().parse::<usize>().unwrap_or(1).max(1) - 1;
        if sw.contains('w') {
            for (i, line) in data_lines.iter().enumerate().skip(from) {
                if wildcard_match(&matchtext, line) {
                    rt.vars.insert(READN_KEY.into(), (i + 1).to_string());
                    return finish_file_read(rt, line.to_string(), no_eval, command_pipes);
                }
            }
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        }
        if sw.contains('s') {
            let ml = matchtext.to_lowercase();
            let ml_sp = format!("{ml} ");
            for (i, line) in data_lines.iter().enumerate().skip(from) {
                let ll = line.to_lowercase();
                // Whole-token match: the line equals matchtext or begins with
                // "matchtext " — so `s,yes` matches "yes ..." but not "yesterday".
                if ll == ml || ll.starts_with(&ml_sp) {
                    rt.vars.insert(READN_KEY.into(), (i + 1).to_string());
                    let char_count = matchtext.chars().count();
                    let rest = line
                        .char_indices()
                        .nth(char_count)
                        .map_or("", |(byte, _)| &line[byte..]);
                    return finish_file_read(
                        rt,
                        rest.trim_start_matches(' ').to_string(),
                        no_eval,
                        command_pipes,
                    );
                }
            }
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        }
        if sw.contains('r') {
            if let Ok(re) = mirc_regex(&matchtext) {
                for (i, line) in data_lines.iter().enumerate().skip(from) {
                    if re.is_match(line).unwrap_or(false) {
                        rt.vars.insert(READN_KEY.into(), (i + 1).to_string());
                        return finish_file_read(rt, line.to_string(), no_eval, command_pipes);
                    }
                }
            }
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        }
        // Control switches only (n/t/p): a line number, if any, is in arg 2.
        let number = args
            .get(2)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        if let Some(number) = number {
            let n = number.parse::<usize>().unwrap_or(0);
            if n == 0 {
                if let Some(count) = header {
                    rt.vars.insert(READN_KEY.into(), "0".into());
                    return finish_file_read(rt, count.to_string(), no_eval, command_pipes);
                }
            }
            if n >= 1 {
                let Some(line) = data_lines.get(n - 1) else {
                    rt.vars.insert(READN_KEY.into(), "0".into());
                    return String::new();
                };
                rt.vars.insert(READN_KEY.into(), n.to_string());
                return finish_file_read(rt, (*line).to_string(), no_eval, command_pipes);
            }
        }
        // fall through to a random read
    }

    if !has_switches && !arg1.is_empty() {
        let n: usize = arg1.parse().unwrap_or(0);
        if n == 0 {
            rt.vars.insert(READN_KEY.into(), "0".into());
            return header.map(|count| count.to_string()).unwrap_or_default();
        }
        if data_lines.is_empty() {
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        }
        let Some(line) = data_lines.get(n.saturating_sub(1)) else {
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        };
        rt.vars.insert(READN_KEY.into(), n.to_string());
        finish_file_read(rt, (*line).to_string(), false, false)
    } else {
        if data_lines.is_empty() {
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        }
        let available = header.unwrap_or(data_lines.len()).min(data_lines.len());
        if available == 0 {
            rt.vars.insert(READN_KEY.into(), "0".into());
            return String::new();
        }
        let idx = rand_range(0, available as i64 - 1) as usize;
        rt.vars.insert(READN_KEY.into(), (idx + 1).to_string());
        finish_file_read(
            rt,
            data_lines.get(idx).copied().unwrap_or("").to_string(),
            no_eval,
            command_pipes,
        )
    }
}

fn finish_file_read(rt: &mut Runtime, value: String, no_eval: bool, command_pipes: bool) -> String {
    let value = if no_eval { value } else { rt.expand(&value) };
    if command_pipes {
        super::eval::encode_command_pipes(&value)
    } else {
        value
    }
}

/// `$var(name, N)` -> the Nth matching variable name (with `%`; N=0 -> count),
/// or its `.value`. The name is taken *literally* (not dereferenced, like `/set`)
/// and may be a wildcard; internal NUL-prefixed keys are excluded. Sorted for a
/// stable order.
pub(crate) fn eval_var(rt: &mut Runtime, args: &[String], prop: &str) -> String {
    rt.purge_expired();
    let raw0 = args.first().map(String::as_str).unwrap_or("");
    let pat = raw0.trim().trim_start_matches('%');
    let n: usize = rt
        .expand(args.get(1).map(String::as_str).unwrap_or(""))
        .trim()
        .parse()
        .unwrap_or(0);
    let mut entries: Vec<(String, String, bool)> = rt
        .visible_vars()
        .into_iter()
        .filter(|(name, _, _)| wildcard_match(pat, name))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if n == 0 {
        entries.len().to_string()
    } else if let Some((name, value, local)) = entries.get(n - 1) {
        match prop {
            "value" => value.clone(),
            "local" => bool_str(*local),
            p if p.eq_ignore_ascii_case("secs") => {
                if *local {
                    "0".to_string()
                } else {
                    rt.var_expiry
                        .get(name)
                        .map(|expiry| {
                            expiry
                                .seconds_remaining(std::time::Instant::now())
                                .to_string()
                        })
                        .unwrap_or_else(|| "0".to_string())
                }
            }
            _ => format!("%{name}"),
        }
    } else {
        String::new()
    }
}

/// Formats a `$timer(...)` result using mIRC's documented timer properties.
fn timer_prop(e: Option<&crate::script::eval::TimerInfo>, prop: &str) -> String {
    match e {
        Some(t) => match prop.to_ascii_lowercase().as_str() {
            "com" => t.command.clone(),
            "time" => t.time.clone(),
            "reps" => t.reps.to_string(),
            "delay" => t.delay.to_string(),
            "type" => t.timer_type.clone(),
            "secs" => t.secs.to_string(),
            "mmt" => if t.mmt { "$true" } else { "$false" }.to_string(),
            "anysc" => if t.anysc { "$true" } else { "$false" }.to_string(),
            "cid" => t.cid.to_string(),
            // jIRC has no native mIRC window handles for timers.
            "wid" | "hwnd" => "0".to_string(),
            "pause" => t.pause.to_string(),
            "name" => t.name.clone(),
            _ => t.name.clone(),
        },
        None => String::new(),
    }
}

fn play_prop(e: Option<&crate::script::eval::PlayInfo>, prop: &str) -> String {
    match e {
        Some(item) => match prop.to_ascii_lowercase().as_str() {
            "type" => item.play_type.clone(),
            "fname" | "filename" => item.filename.clone(),
            "topic" => item.topic.clone(),
            "pos" => item.pos.to_string(),
            "lines" => item.lines.to_string(),
            "delay" => item.delay.to_string(),
            "status" => item.status.clone(),
            _ => item.target.clone(),
        },
        None => String::new(),
    }
}

/// Converts ANSI SGR escape sequences (`ESC[…m`) to mIRC control codes: reset,
/// bold, underline, reverse, and the 8 standard foreground (30-37) / background
/// (40-47) colours, mapped to the same-named mIRC colour. Other text passes
/// through unchanged.
fn ansi_to_mirc(s: &str) -> String {
    const MAP: [u8; 8] = [1, 4, 3, 8, 2, 6, 10, 15]; // ANSI 0-7 -> mIRC colour
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            i += 2;
            let start = i;
            while i < chars.len() && chars[i] != 'm' {
                i += 1;
            }
            let codes: String = chars[start..i].iter().collect();
            if i < chars.len() {
                i += 1; // skip the terminating 'm'
            }
            for code in codes.split(';') {
                match code.trim().parse::<u8>() {
                    Ok(0) => out.push('\u{f}'),  // reset
                    Ok(1) => out.push('\u{2}'),  // bold
                    Ok(4) => out.push('\u{1f}'), // underline
                    Ok(7) => out.push('\u{16}'), // reverse
                    Ok(c @ 30..=37) => {
                        out.push('\u{3}');
                        out.push_str(&MAP[(c - 30) as usize].to_string());
                    }
                    Ok(c @ 40..=47) => {
                        out.push('\u{3}');
                        out.push_str(&format!(",{}", MAP[(c - 40) as usize]));
                    }
                    _ => {}
                }
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

pub(crate) fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Parse a number for the math identifiers (non-numeric -> 0).
fn num(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

/// Format a math result to mIRC's default 6 decimal places, trimming trailing
/// zeros (and a trailing dot). Non-finite results (NaN/inf) render as empty.
fn fmt_round6(n: f64) -> String {
    if !n.is_finite() {
        return String::new();
    }
    let s = format!("{n:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Input bytes for $md5/$sha*/$crc: N=2 reads file contents (sandboxed); any
/// other N treats `value` as plain text.
/// Raw `$hfind` entry point used to preserve the callback command until each
/// matching item has replaced `$1-`.
pub fn eval_hfind(rt: &mut Runtime, raw: &[String], prop: &str) -> String {
    let mut args = raw
        .iter()
        .take(4)
        .map(|argument| rt.expand(argument))
        .collect::<Vec<_>>();
    while args.len() < 4 {
        args.push(String::new());
    }
    let output = (raw.len() > 4).then(|| raw[4..].join(","));
    eval_hfind_expanded(rt, &args, prop, output.as_deref())
}

fn eval_hfind_expanded(
    rt: &mut Runtime,
    args: &[String],
    prop: &str,
    raw_output: Option<&str>,
) -> String {
    let a = |index: usize| args.get(index).cloned().unwrap_or_default();
    let n = a(2).parse::<usize>().unwrap_or(1);
    let Some(table) = resolve_hash_table(rt, &a(0)) else {
        return String::new();
    };
    let needle = a(1);
    let mode = a(3).chars().next().unwrap_or('n');
    let search_data = prop.eq_ignore_ascii_case("data");
    let mut keys = rt
        .hashes
        .get(&table)
        .map(|hash| {
            hash.iter()
                .filter(|(item, value)| {
                    let haystack = if search_data {
                        super::hash::value_text(value)
                    } else {
                        (*item).clone()
                    };
                    hash_find_matches(&needle, &haystack, mode)
                })
                .map(|(item, _)| item.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    keys.sort_by_key(|key| key.to_ascii_lowercase());

    let mut processed = keys.len();
    if let Some(output) = raw_output.filter(|output| !output.trim().is_empty()) {
        let output_trimmed = output.trim_start();
        let window = if output_trimmed.starts_with('@') {
            Some(rt.expand(output))
        } else if output_trimmed.starts_with('%') {
            let expanded = rt.expand(output);
            expanded.starts_with('@').then_some(expanded)
        } else {
            None
        };
        if let Some(window) = window {
            for item in &keys {
                rt.actions.push(super::eval::Action::WindowLine {
                    name: window.clone(),
                    op: "add".to_string(),
                    n: 0,
                    text: item.clone(),
                });
            }
        } else {
            processed = 0;
            for item in &keys {
                processed += 1;
                if rt.run_hfind_callback(output, item) {
                    break;
                }
            }
        }
    }
    if n == 0 {
        processed.to_string()
    } else {
        keys.get(n - 1).cloned().unwrap_or_default()
    }
}

fn resolve_hash_table(rt: &Runtime, selector: &str) -> Option<String> {
    if let Ok(index) = selector.parse::<usize>() {
        return index
            .checked_sub(1)
            .and_then(|index| super::hash::table_names(rt.hashes).get(index).cloned());
    }
    super::hash::table_key(rt.hashes, selector)
}

fn resolve_hash_item(rt: &Runtime, table: &str, selector: &str) -> Option<String> {
    super::hash::item_key(rt.hashes.get(table)?, selector)
}

fn hash_find_matches(needle: &str, haystack: &str, mode: char) -> bool {
    match mode {
        'w' => wildcard_match(needle, haystack),
        'W' => wildcard_match(haystack, needle),
        'r' => mirc_regex(needle)
            .and_then(|regex| regex.is_match(haystack))
            .unwrap_or(false),
        'R' => mirc_regex(haystack)
            .and_then(|regex| regex.is_match(needle))
            .unwrap_or(false),
        _ => needle.eq_ignore_ascii_case(haystack),
    }
}

fn hash_input(rt: &Runtime, value: &str, n: &str) -> Vec<u8> {
    match n {
        "2" => std::fs::read(super::eval::sandbox_path(&rt.data_dir, value)).unwrap_or_default(),
        "1" => rt.bins.get(value).cloned().unwrap_or_default(),
        _ => value.as_bytes().to_vec(),
    }
}

/// Lowercase hex of a digest/byte slice.
fn hex_of(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Raw HMAC bytes for the given algorithm (sha1 is mIRC's default).
fn hmac_raw(algo: &str, key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    macro_rules! go {
        ($t:ty) => {{
            let mut m = <Hmac<$t>>::new_from_slice(key).expect("HMAC accepts any key length");
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }};
    }
    match algo.to_ascii_lowercase().as_str() {
        "md5" => go!(md5::Md5),
        "sha256" => go!(sha2::Sha256),
        "sha384" => go!(sha2::Sha384),
        "sha512" => go!(sha2::Sha512),
        _ => go!(sha1::Sha1),
    }
}

/// One HOTP/TOTP code (RFC 4226 dynamic truncation).
fn hotp(algo: &str, key: &[u8], counter: u64, digits: u32) -> String {
    let mac = hmac_raw(algo, key, &counter.to_be_bytes());
    let offset = ((mac[mac.len() - 1] & 0x0f) as usize).min(mac.len() - 4);
    let bin = ((mac[offset] as u32 & 0x7f) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    format!(
        "{:0width$}",
        bin % 10u32.pow(digits),
        width = digits as usize
    )
}

/// HOTP/TOTP digit count: 3-10, default 6.
fn otp_digits(s: &str) -> u32 {
    s.trim()
        .parse()
        .ok()
        .filter(|d| (3..=10).contains(d))
        .unwrap_or(6)
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut bits, mut nbits) = (0u32, 0u32);
    let mut out = Vec::new();
    for c in s.bytes() {
        let c = c.to_ascii_uppercase();
        if c == b'=' {
            break;
        }
        let val = ALPHABET.iter().position(|&x| x == c)? as u32;
        bits = (bits << 5) | val;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

/// A TOTP/HOTP key: hex (40/64/128/256 chars), base32 (16/26/32), else plain text.
fn decode_otp_key(s: &str) -> Vec<u8> {
    let t: String = s.split_whitespace().collect();
    let len = t.len();
    if matches!(len, 40 | 64 | 128 | 256) && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return hex_decode(&t);
    }
    if matches!(len, 16 | 26 | 32) && t.bytes().all(|b| b.is_ascii_alphanumeric()) {
        if let Some(d) = base32_decode(&t) {
            return d;
        }
    }
    t.into_bytes()
}

/// PBKDF2-HMAC derived key as hex (sha1 default, like mIRC's hash family).
fn pbkdf2_hex(algo: &str, pass: &[u8], salt: &[u8], iters: u32, length: usize) -> String {
    use pbkdf2::pbkdf2_hmac;
    let mut out = vec![0u8; length];
    match algo.to_ascii_lowercase().as_str() {
        "md5" => pbkdf2_hmac::<md5::Md5>(pass, salt, iters, &mut out),
        "sha256" => pbkdf2_hmac::<sha2::Sha256>(pass, salt, iters, &mut out),
        "sha384" => pbkdf2_hmac::<sha2::Sha384>(pass, salt, iters, &mut out),
        "sha512" => pbkdf2_hmac::<sha2::Sha512>(pass, salt, iters, &mut out),
        _ => pbkdf2_hmac::<sha1::Sha1>(pass, salt, iters, &mut out),
    }
    hex_of(&out)
}

/// Percent-encode per RFC 3986 (keep unreserved A-Za-z0-9 - . _ ~).
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse an integer for the bitwise identifiers (non-numeric -> 0).
fn uint(s: &str) -> u64 {
    s.trim().parse::<i64>().map(|n| n as u64).unwrap_or(0)
}

fn gcd2(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Reduce all args (parsed as integers) with `f`, for $gcd / $lcm.
fn fold_ints(args: &[String], f: impl Fn(i64, i64) -> i64) -> i64 {
    args.iter()
        .map(|s| s.trim().parse::<i64>().unwrap_or(0))
        .reduce(f)
        .unwrap_or(0)
}

/// Evaluates a simple arithmetic expression (+ - * / %, parens).
fn calc(expr: &str) -> Option<f64> {
    let toks: Vec<char> = expr.chars().filter(|c| !c.is_whitespace()).collect();
    let mut p = CalcParser { toks, pos: 0 };
    let v = p.expr()?;
    if p.pos == p.toks.len() {
        Some(v)
    } else {
        None
    }
}

struct CalcParser {
    toks: Vec<char>,
    pos: usize,
}

impl CalcParser {
    fn peek(&self) -> Option<char> {
        self.toks.get(self.pos).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(op) = self.peek() {
            if op == '+' || op == '-' {
                self.pos += 1;
                let rhs = self.term()?;
                v = if op == '+' { v + rhs } else { v - rhs };
            } else {
                break;
            }
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.factor()?;
        while let Some(op) = self.peek() {
            if op == '*' || op == '/' || op == '%' {
                self.pos += 1;
                let rhs = self.factor()?;
                v = match op {
                    '*' => v * rhs,
                    '/' => v / rhs,
                    _ => v % rhs,
                };
            } else {
                break;
            }
        }
        Some(v)
    }

    fn factor(&mut self) -> Option<f64> {
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let v = self.expr()?;
                if self.peek() == Some(')') {
                    self.pos += 1;
                }
                Some(v)
            }
            Some('-') => {
                self.pos += 1;
                Some(-self.factor()?)
            }
            _ => {
                let mut num = String::new();
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        num.push(c);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                num.parse().ok()
            }
        }
    }
}

fn eval_dcc_ident(rt: &Runtime, name: &str, args: &[String], prop: &str) -> String {
    let kind = match name.to_ascii_lowercase().as_str() {
        "chat" => "chat",
        "send" => "send",
        _ => "recv",
    };
    let items: Vec<_> = rt
        .dcc
        .snapshot(&rt.state.server_id)
        .into_iter()
        .filter(|item| item.kind == kind)
        .collect();
    let Some(query) = args.first() else {
        return String::new();
    };
    let selected = if let Ok(n) = query.trim().parse::<usize>() {
        if n == 0 {
            return items.len().to_string();
        }
        items.get(n - 1)
    } else {
        let matching: Vec<_> = items
            .iter()
            .filter(|item| item.nick.eq_ignore_ascii_case(query))
            .collect();
        let n = args
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1);
        if n == 0 {
            return matching.len().to_string();
        }
        matching.get(n - 1).copied()
    };
    let Some(item) = selected else {
        return String::new();
    };
    match prop.to_ascii_lowercase().as_str() {
        "" => item.nick.clone(),
        "ip" => item.ip.clone(),
        "status" => item.status.clone(),
        "file" => item.filename.clone(),
        "path" => std::path::Path::new(&item.path)
            .parent()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "size" => item.size.to_string(),
        "pos" | "sent" | "rcvd" => item.transferred.to_string(),
        "lra" => item.last_ack.to_string(),
        "pc" => {
            if item.size == 0 {
                "0".into()
            } else {
                ((item.transferred.saturating_mul(100) / item.size).min(100)).to_string()
            }
        }
        "secs" | "idle" => item.secs.to_string(),
        "done" => bool_str(item.status == "done"),
        "resume" => item.resume.to_string(),
        "cid" => match rt.conns.cid_of(&rt.state.server_id) {
            0 => String::new(),
            cid => cid.to_string(),
        },
        "cps" => {
            let elapsed = item.secs.max(1);
            item.transferred
                .saturating_sub(item.resume)
                .checked_div(elapsed)
                .unwrap_or(0)
                .to_string()
        }
        // Native window handles do not exist in the web frontend.
        "wid" | "hwnd" | "logfile" | "stamp" => String::new(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_basics() {
        assert_eq!(calc("2 + 3 * 4"), Some(14.0));
        assert_eq!(calc("(2 + 3) * 4"), Some(20.0));
        assert_eq!(calc("10 / 4"), Some(2.5));
    }

    #[test]
    fn left_right_mid() {
        assert_eq!(take_left("hello", 3), "hel");
        assert_eq!(take_right("hello", 2), "lo");
    }

    #[test]
    fn regex_accepts_mirc_unicode_options_and_preserves_pcre_verbs() {
        // PCRE1's (*UTF8) spelling is common in mIRC scripts. Other PCRE verbs
        // must remain part of the pattern rather than being discarded.
        assert!(mirc_regex("/(*UTF8)(.)/g").is_ok());
        assert!(mirc_regex("/(*UTF8)(*UCP)\\w+/").is_ok());
        let re = mirc_regex("/(*UTF8)(.)/").unwrap();
        assert!(re.is_match("A").unwrap());
        assert!(!mirc_regex_is_match("foo", "/foo(*FAIL)|bar/"));
        assert!(mirc_regex("/(*LIMIT_MATCH=1000)(a+)+$/").is_ok());
    }

    #[test]
    fn regex_supports_pcre_advanced_and_named_capture_syntax() {
        let named = mirc_regex(r"/^(?<word>[a-z]+)-\k<word>$/").unwrap();
        let captures = named.regex.captures(b"mirror-mirror").unwrap().unwrap();
        assert_eq!(captures.name("word").unwrap().as_bytes(), b"mirror");
        assert!(named.is_match("mirror-mirror").unwrap());
        assert!(!named.is_match("mirror-other").unwrap());

        // All three PCRE named-group spellings, lookbehind, conditionals,
        // recursion/subroutines, atomic groups, and branch-reset groups are
        // used by real mIRC scripts and are unsupported by Rust's regex crate.
        assert!(mirc_regex_is_match(
            "abca",
            r"/^(?<angle>a)(?'quote'b)(?P<python>c)\k<angle>$/"
        ));
        assert!(mirc_regex_is_match("foobar", r"/(?<=foo)bar/"));
        assert!(mirc_regex_is_match("ab", r"/^(a)?(?(1)b|c)$/"));
        assert!(mirc_regex_is_match("c", r"/^(a)?(?(1)b|c)$/"));
        assert!(mirc_regex_is_match(
            "(a(b)c)",
            r"/^(?<paren>\((?:[^()]++|(?&paren))*\))$/"
        ));
        assert!(!mirc_regex_is_match("aa", r"/^(?>a+)a$/"));

        let branch_reset = mirc_regex(r"/(?|(a)|(b))/").unwrap();
        let captures = branch_reset.regex.captures(b"b").unwrap().unwrap();
        assert_eq!(captures.len(), 2);
        assert_eq!(captures.get(1).unwrap().as_bytes(), b"b");
    }

    #[test]
    fn string_helpers_via_eval() {
        use crate::script::ast::Script;
        use crate::script::eval::{EventVars, Runtime};
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = Runtime {
            script: &script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars: &mut vars,
            local_scopes: Vec::new(),
            hashes: &mut hashes,
            var_expiry: &mut var_expiry,
            hash_expiry: &mut hash_expiry,
            files: &mut files,
            bins: &mut bins,
            windows: &mut windows,
            users: &mut users,
            event: EventVars::default(),
            actions: vec![],
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: Vec::new(),
            ret: None,
            goto: None,
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
            active: String::new(),
            conns: Default::default(),
            wins: Default::default(),
            sockets: std::sync::Arc::new(crate::script::eval::NoSockets),
            timers: std::sync::Arc::new(crate::script::eval::NoTimers),
            play: std::sync::Arc::new(crate::script::eval::NoPlay),
            dcc: std::sync::Arc::new(crate::script::eval::NoDcc),
            webviews: std::sync::Arc::new(crate::script::eval::NoWebviews),
            input: std::sync::Arc::new(crate::script::eval::NoInput),
            caller: "command",
            show: true,
        };
        assert_eq!(
            eval_ident(
                &mut rt,
                "replace",
                &["abcabc".into(), "b".into(), "X".into()],
                ""
            ),
            "aXcaXc"
        );
        // $replace is case-INSENSITIVE in mIRC (a lowercase pattern matches
        // uppercase text); hex byte-escapers depend on it.
        assert_eq!(
            eval_ident(
                &mut rt,
                "replace",
                &["0A".into(), "0a".into(), "n".into()],
                ""
            ),
            "n"
        );
        assert_eq!(
            eval_ident(
                &mut rt,
                "replace",
                &["AbAb".into(), "ab".into(), "X".into()],
                ""
            ),
            "XX"
        );
        assert_eq!(
            eval_ident(&mut rt, "remove", &["abcabc".into(), "a".into()], ""),
            "bcbc"
        );
        assert_eq!(
            eval_ident(&mut rt, "pos", &["hello".into(), "l".into()], ""),
            "3"
        );
        assert_eq!(
            eval_ident(&mut rt, "count", &["banana".into(), "a".into()], ""),
            "3"
        );
        assert_eq!(eval_ident(&mut rt, "reverse", &["abc".into()], ""), "cba");
        assert_eq!(
            eval_ident(&mut rt, "max", &["3".into(), "7".into()], ""),
            "7"
        );
        // mIRC-compat: Nth-occurrence $pos/$lastpos, N=0 $mid, multiple args.
        let mut id = |n: &str, a: &[&str]| {
            eval_ident(
                &mut rt,
                n,
                &a.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "",
            )
        };
        assert_eq!(id("pos", &["hello", "l", "2"]), "4");
        assert_eq!(id("pos", &["hello", "l", "0"]), "2");
        assert_eq!(id("pos", &["hello", "l", "3"]), "");
        assert_eq!(id("lastpos", &["hello", "l"]), "4");
        assert_eq!(id("lastpos", &["hello", "l", "2"]), "3");
        // $mid mIRC-exact: N=0 -> length of the remainder; negatives supported.
        assert_eq!(id("mid", &["hello", "2", "0"]), "4"); // len of "ello"
        assert_eq!(id("mid", &["hello", "2", "3"]), "ell");
        assert_eq!(id("mid", &["abcdefghij", "-6", "2"]), "ef"); // 6th from end, 2 chars
        assert_eq!(id("mid", &["abcdefghij", "-6"]), "efghij"); // 6th from end, to end
        assert_eq!(id("mid", &["abcdefghij", "3", "-2"]), "cdefgh"); // from 3, drop last 2
        assert_eq!(id("maxlenl", &[]), "10240");
        assert_eq!(id("maxlenm", &[]), "2048");
        assert_eq!(id("maxlens", &[]), "512");
        assert_eq!(
            id("bits", &[]),
            (std::mem::size_of::<usize>() * 8).to_string()
        );
        assert_eq!(id("numbits", &["255"]), "8");
        assert_eq!(id("numbits", &["0"]), "1");
        assert_eq!(id("numbits", &["256"]), "9");
        assert_eq!(id("rgb", &["252", "127", "0"]), "32764"); // R,G,B -> number
        assert_eq!(id("rgb", &["32764"]), "252,127,0"); // number -> R,G,B
        assert_eq!(id("ansi2mirc", &["\u{1b}[32mgreen"]), "\u{3}3green");
        assert_eq!(id("count", &["banana", "a", "n"]), "5");
        assert_eq!(id("replace", &["abcabc", "a", "X", "c", "Y"]), "XbYXbY");
        assert_eq!(id("remove", &["abcabc", "a", "c"]), "bb");
        assert_eq!(id("instok", &["a.b.c", "X", "2", "46"]), "a.X.b.c");
        // negative N: -1 inserts before the last element.
        assert_eq!(id("instok", &["a.b.c", "X", "-1", "46"]), "a.b.X.c");
        assert_eq!(id("reptok", &["a.b.a.c", "a", "X", "2", "46"]), "a.b.X.c");
        assert_eq!(id("reptok", &["a.b.a", "a", "X", "0", "46"]), "X.b.X");
        // Case-INSENSITIVITY (mIRC default): token/substring ops match across case.
        assert_eq!(id("istok", &["Foo.Bar", "foo", "46"]), "$true");
        assert_eq!(id("addtok", &["Foo.Bar", "FOO", "46"]), "Foo.Bar"); // CI dedup
        assert_eq!(id("findtok", &["a.B.c", "b", "1", "46"]), "2");
        assert_eq!(id("remtok", &["a.B.c", "b", "1", "46"]), "a.c");
        assert_eq!(id("reptok", &["a.B.c", "b", "X", "1", "46"]), "a.X.c");
        assert_eq!(id("pos", &["Hello", "L"]), "3");
        assert_eq!(id("count", &["BaNaNa", "a"]), "3");
        assert_eq!(id("remove", &["aAbB", "a", "b"]), "");
        // mIRC /pattern/flags regex: i = case-insensitive; bare = case-sensitive.
        assert_eq!(id("regex", &["Hello", "/hello/i"]), "1");
        assert_eq!(id("regex", &["Hello", "hello"]), "0");
        assert_eq!(id("regsub", &["Hello World", "/o/g", "0"]), "Hell0 W0rld");
        // $regerrstr — set on a bad pattern, cleared on a good one.
        assert_eq!(id("regex", &["x", "("]), "0");
        assert!(!id("regerrstr", &[]).is_empty());
        assert_eq!(id("regex", &["x", "x"]), "1");
        assert_eq!(id("regerrstr", &[]), "");
        // $regmlex — per-match capture groups for a global pattern.
        assert_eq!(id("regex", &["a1 b2 c3", "/(\\w)(\\d)/g"]), "3");
        assert_eq!(id("regmlex", &["2", "1"]), "b");
        assert_eq!(id("regmlex", &["2", "2"]), "2");
        assert_eq!(id("regmlex", &["3", "1"]), "c");
        assert_eq!(id("regml", &["1"]), "a"); // first match still flat for $regml
                                              // $notags — strip a leading IRCv3 message-tag block
        assert_eq!(
            id("notags", &["@time=x;id=5 :nick!u@h PRIVMSG #c :hi"]),
            ":nick!u@h PRIVMSG #c :hi"
        );
        assert_eq!(
            id("notags", &[":nick PRIVMSG #c :no tags"]),
            ":nick PRIVMSG #c :no tags"
        );
        assert_eq!(id("notags", &["@only=tags"]), ""); // tags-only -> empty
                                                       // file-name identifiers
        assert_eq!(id("nopath", &["C:\\folder\\file.txt"]), "file.txt");
        assert_eq!(id("nopath", &["/usr/bin/foo"]), "foo");
        assert_eq!(id("nofile", &["C:\\folder\\file.txt"]), "C:\\folder\\");
        assert_eq!(id("nofile", &["bare.txt"]), "");
        assert_eq!(id("longfn", &["foo.txt"]), "foo.txt");
        // $comchar / $mkfn / $mknickfn / $eval
        assert_eq!(id("comchar", &[]), "/");
        assert_eq!(id("mkfn", &["a/b:c*d?.txt"]), "a_b_c_d_.txt");
        assert_eq!(id("mknickfn", &["ni|ck"]), "ni_ck");
        assert_eq!(id("iptype", &["192.168.0.1"]), "ipv4");
        assert_eq!(id("iptype", &["2001:db8::1"]), "ipv6");
        assert_eq!(id("iptype", &["example.com"]), "");
        assert_eq!(id("halted", &[]), "$false");
        assert_eq!(id("eval", &["hello", "1"]), "hello");
        assert_eq!(id("eval", &["$len(hi)", "2"]), "2"); // N≥2 expands the arg again
        assert!(id("ticks", &[]).parse::<u64>().is_ok());
        assert!(id("gmt", &[]).parse::<u64>().is_ok());
        assert_eq!(id("noqt", &["\"hello world\""]), "hello world");
        assert_eq!(id("noqt", &["plain"]), "plain");
        assert_eq!(id("bytes", &["1234567"]), "1,234,567");
        assert!(id("envvar", &["0"])
            .parse::<usize>()
            .map(|c| c > 0)
            .unwrap_or(false));
        // local time/date — format checks (the values are timezone-dependent).
        let d = id("date", &[]);
        assert!(
            d.len() == 10 && &d[2..3] == "/" && &d[5..6] == "/",
            "date={d}"
        );
        let t = id("time", &[]);
        assert!(
            t.len() == 8 && &t[2..3] == ":" && &t[5..6] == ":",
            "time={t}"
        );
        assert!(!id("asctime", &["0", "yyyy"]).is_empty());
        // math / trig — 6-decimal default, radians unless `.deg`.
        assert_eq!(id("sqrt", &["16"]), "4");
        assert_eq!(id("sqrt", &["2"]), "1.414214");
        assert_eq!(id("cbrt", &["27"]), "3");
        assert_eq!(id("hypot", &["3", "4"]), "5");
        assert_eq!(id("log10", &["1000"]), "3");
        assert_eq!(id("pi", &[]), "3.14159265358979323846");
        assert_eq!(id("cos", &["0"]), "1");
        // hashing (known test vectors)
        assert_eq!(id("md5", &["abc"]), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            id("sha1", &["abc"]),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            id("sha256", &["abc"]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(id("crc", &["123456789"]), "CBF43926");
        assert_eq!(id("crc64", &["abc", "0"]), "2CD8094A1A277627");
        // bitwise / integer math
        assert_eq!(id("and", &["12", "10"]), "8");
        assert_eq!(id("or", &["12", "10"]), "14");
        assert_eq!(id("xor", &["12", "10"]), "6");
        assert_eq!(id("not", &["0"]), "4294967295");
        assert_eq!(id("biton", &["0", "3"]), "4");
        assert_eq!(id("bitoff", &["7", "1"]), "6");
        assert_eq!(id("isbit", &["5", "3"]), "1");
        assert_eq!(id("isbit", &["5", "2"]), "0");
        assert_eq!(id("gcd", &["12", "18", "24"]), "6");
        assert_eq!(id("lcm", &["4", "6", "8"]), "24");
        // misc: ordinal, longip (both directions), day/os non-empty
        assert_eq!(id("ord", &["1"]), "1st");
        assert_eq!(id("ord", &["2"]), "2nd");
        assert_eq!(id("ord", &["11"]), "11th");
        assert_eq!(id("ord", &["22"]), "22nd");
        assert_eq!(id("longip", &["192.168.0.1"]), "3232235521");
        assert_eq!(id("longip", &["3232235521"]), "192.168.0.1");
        assert!(!id("day", &[]).is_empty());
        assert!(!id("os", &[]).is_empty());
        // ISUPPORT-derived (default Isupport values)
        assert_eq!(id("prefix", &[]), "(qaohv)~&@%+");
        assert_eq!(id("chantypes", &[]), "#&!+");
        assert_eq!(id("chanmodes", &[]), "beI,k,l,imnpstrS");
        assert_eq!(id("modespl", &[]), "3");
        // $replacex single-pass (a->b is NOT then matched by b->c), $powmod, $utf
        assert_eq!(id("replacex", &["hello", "l", "L"]), "heLLo");
        assert_eq!(id("replacex", &["abc", "a", "b", "b", "c"]), "bcc");
        assert_eq!(id("powmod", &["4", "13", "497"]), "445");
        assert_eq!(id("utfencode", &["hi"]), "hi");
        assert!(id("ticksqpc", &[]).parse::<u64>().is_ok());
        // $encode/$decode — base64 (m) and percent-encode (x)
        assert_eq!(id("encode", &["Man", "m"]), "TWFu");
        assert_eq!(id("decode", &["TWFu", "m"]), "Man");
        assert_eq!(id("encode", &["a b&c", "x"]), "a%20b%26c");
        assert_eq!(id("decode", &["a%20b%26c", "x"]), "a b&c");
        // $mircexe non-empty; $tempfn contains a "tmp" component
        assert!(!id("mircexe", &[]).is_empty());
        assert!(id("version", &[]).contains('.')); // jIRC CalVer, e.g. 26.7.x
        assert!(id("tempfn", &[]).contains("tmp"));
        // $rands in range; $isalias false with no aliases loaded
        let rv: i64 = id("rands", &["1", "3"]).parse().unwrap();
        assert!((1..=3).contains(&rv));
        assert_eq!(id("isalias", &["nope"]), "$false");
        // $modinv (3*4 = 12 ≡ 1 mod 11); $mircpid numeric
        assert_eq!(id("modinv", &["3", "11"]), "4");
        assert!(id("mircpid", &[]).parse::<u32>().is_ok());
        // HMAC / HOTP / TOTP — canonical RFC 2104 / 4226 / 6238 vectors
        assert_eq!(
            id(
                "hmac",
                &[
                    "The quick brown fox jumps over the lazy dog",
                    "key",
                    "sha256"
                ]
            ),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        assert_eq!(id("hotp", &["12345678901234567890", "0"]), "755224");
        assert_eq!(id("hotp", &["12345678901234567890", "1"]), "287082");
        // TOTP at t=59 with step 30 -> counter 1 -> same as hotp(...,1)
        assert_eq!(id("totp", &["12345678901234567890", "59"]), "287082");
        // PBKDF2-HMAC-SHA1 — RFC 6070 vectors
        assert_eq!(
            id("pbkdf2", &["password", "salt", "sha1", "20", "1"]),
            "0c60c80f961f0e71f3a9b524af6012062fe037a6"
        );
        assert_eq!(
            id("pbkdf2", &["password", "salt", "sha1", "20", "4096"]),
            "4b007901b765489abead49d926f721d065a429c1"
        );
        // `.deg` needs the property, so call eval_ident directly — this is after
        // the `id` closure's final use, so its borrow of `rt` has ended.
        assert_eq!(eval_ident(&mut rt, "sin", &["90".into()], "deg"), "1");
        assert_eq!(eval_ident(&mut rt, "atan", &["1".into()], "deg"), "45");
    }

    #[test]
    fn file_identifier() {
        use crate::script::ast::Script;
        use crate::script::eval::{EventVars, Runtime};
        use std::collections::HashMap;

        // A private sandbox dir so the test is hermetic and parallel-safe.
        let dir = std::env::temp_dir().join(format!(
            "jirc_file_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("probe.dat"), b"hello").unwrap(); // 5 bytes

        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = Runtime {
            script: &script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars: &mut vars,
            local_scopes: Vec::new(),
            hashes: &mut hashes,
            var_expiry: &mut var_expiry,
            hash_expiry: &mut hash_expiry,
            files: &mut files,
            bins: &mut bins,
            windows: &mut windows,
            users: &mut users,
            event: EventVars::default(),
            actions: vec![],
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: Vec::new(),
            ret: None,
            goto: None,
            data_dir: dir.clone(),
            state: std::sync::Arc::new(Default::default()),
            active: String::new(),
            conns: Default::default(),
            wins: Default::default(),
            sockets: std::sync::Arc::new(crate::script::eval::NoSockets),
            timers: std::sync::Arc::new(crate::script::eval::NoTimers),
            play: std::sync::Arc::new(crate::script::eval::NoPlay),
            dcc: std::sync::Arc::new(crate::script::eval::NoDcc),
            webviews: std::sync::Arc::new(crate::script::eval::NoWebviews),
            input: std::sync::Arc::new(crate::script::eval::NoInput),
            caller: "command",
            show: true,
        };
        let f = |rt: &mut Runtime, prop: &str| eval_ident(rt, "file", &["probe.dat".into()], prop);

        assert_eq!(f(&mut rt, "size"), "5");
        assert_eq!(f(&mut rt, "name"), "probe.dat");
        assert_eq!(f(&mut rt, "ext"), "dat");
        assert!(f(&mut rt, "mtime")
            .parse::<u64>()
            .map(|t| t > 0)
            .unwrap_or(false));
        assert!(!f(&mut rt, "").is_empty()); // bare -> resolved path (exists)
                                             // The path is sandboxed to the data dir like $isfile/$read: a leading
                                             // path is stripped to the leaf, so i7.mrc's $file($scriptdir\x) resolves.
        assert_eq!(
            eval_ident(&mut rt, "file", &["C:\\anywhere\\probe.dat".into()], "size"),
            "5"
        );
        // A missing file -> empty for every property (and bare).
        assert_eq!(
            eval_ident(&mut rt, "file", &["nope.dat".into()], "size"),
            ""
        );
        assert_eq!(eval_ident(&mut rt, "file", &["nope.dat".into()], ""), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snick_identifiers() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = rt_for(
            &script,
            &mut vars,
            &mut hashes,
            &mut var_expiry,
            &mut hash_expiry,
            &mut files,
            &mut bins,
            &mut windows,
            &mut users,
        );
        rt.event.snicks = vec!["alice".into(), "bob".into(), "carol".into()];
        assert_eq!(eval_ident(&mut rt, "snicks", &[], ""), "alice,bob,carol");
        assert_eq!(
            eval_ident(&mut rt, "snick", &["#c".into(), "0".into()], ""),
            "3"
        );
        assert_eq!(
            eval_ident(&mut rt, "snick", &["#c".into(), "2".into()], ""),
            "bob"
        );
        assert_eq!(
            eval_ident(&mut rt, "snick", &["#c".into(), "9".into()], ""),
            ""
        ); // out of range
        assert_eq!(
            eval_ident(&mut rt, "snick", &["#c".into()], ""),
            "alice bob carol"
        ); // no N
           // No selection (a timer / typed command) -> empty list, count 0.
        rt.event.snicks.clear();
        assert_eq!(eval_ident(&mut rt, "snicks", &[], ""), "");
        assert_eq!(
            eval_ident(&mut rt, "snick", &["#c".into(), "0".into()], ""),
            "0"
        );
    }

    #[test]
    fn mirc_format_translation() {
        assert_eq!(mirc_to_chrono("yyyy-mm-dd"), "%Y-%m-%d");
        assert_eq!(mirc_to_chrono("dd/mm/yyyy HH:nn:ss"), "%d/%m/%Y %H:%M:%S");
        assert_eq!(mirc_to_chrono("ddd mmm dd"), "%a %b %d");
        assert_eq!(mirc_to_chrono("h:nn tt"), "%-I:%M %p");
        assert_eq!(mirc_to_chrono("yy"), "%y");
    }

    #[test]
    fn geometry_intersection_and_polygon_identifiers() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = rt_for(
            &script,
            &mut vars,
            &mut hashes,
            &mut var_expiry,
            &mut hash_expiry,
            &mut files,
            &mut bins,
            &mut windows,
            &mut users,
        );
        let mut evaluate = |name: &str, values: &[&str]| {
            eval_ident(
                &mut rt,
                name,
                &values
                    .iter()
                    .map(|value| (*value).into())
                    .collect::<Vec<_>>(),
                "",
            )
        };

        assert_eq!(
            evaluate("intersect", &["0", "0", "10", "10", "0", "10", "10", "0"]),
            "5 5"
        );
        assert_eq!(
            evaluate(
                "intersect",
                &["0", "0", "1", "0", "2", "-1", "2", "1", "ss"]
            ),
            ""
        );
        assert_eq!(
            evaluate(
                "onpoly",
                &[
                    "4", "4", "0", "0", "10", "0", "10", "10", "0", "10", "5", "5", "15", "5",
                    "15", "15", "5", "15",
                ],
            ),
            "$true"
        );
        assert_eq!(
            evaluate(
                "onpoly",
                &[
                    "4", "4", "0", "0", "20", "0", "20", "20", "0", "20", "5", "5", "10", "5",
                    "10", "10", "5", "10",
                ],
            ),
            "$true"
        );
        assert_eq!(
            evaluate(
                "onpoly",
                &[
                    "4", "4", "0", "0", "2", "0", "2", "2", "0", "2", "5", "5", "7", "5", "7", "7",
                    "5", "7",
                ],
            ),
            "$false"
        );
    }

    fn rt_for<'a>(
        script: &'a crate::script::ast::Script,
        vars: &'a mut std::collections::HashMap<String, String>,
        hashes: &'a mut std::collections::HashMap<
            String,
            std::collections::HashMap<String, String>,
        >,
        var_expiry: &'a mut std::collections::HashMap<String, crate::script::eval::TimedExpiry>,
        hash_expiry: &'a mut std::collections::HashMap<
            (String, String),
            crate::script::eval::TimedExpiry,
        >,
        files: &'a mut crate::script::files::FileStore,
        bins: &'a mut crate::script::binvar::BinStore,
        windows: &'a mut crate::script::window::WindowStore,
        users: &'a mut crate::script::users::UserList,
    ) -> Runtime<'a> {
        use crate::script::eval::EventVars;
        Runtime {
            script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars,
            local_scopes: Vec::new(),
            hashes,
            var_expiry,
            hash_expiry,
            files,
            bins,
            windows,
            users,
            event: EventVars::default(),
            actions: vec![],
            pending_pipe_commands: Vec::new(),
            halted: false,
            steps: 0,
            depth: 0,
            alias_stack: Vec::new(),
            ret: None,
            goto: None,
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
            active: String::new(),
            conns: Default::default(),
            wins: Default::default(),
            sockets: std::sync::Arc::new(crate::script::eval::NoSockets),
            timers: std::sync::Arc::new(crate::script::eval::NoTimers),
            play: std::sync::Arc::new(crate::script::eval::NoPlay),
            dcc: std::sync::Arc::new(crate::script::eval::NoDcc),
            webviews: std::sync::Arc::new(crate::script::eval::NoWebviews),
            input: std::sync::Arc::new(crate::script::eval::NoInput),
            caller: "command",
            show: true,
        }
    }

    #[test]
    fn token_identifiers() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = rt_for(
            &script,
            &mut vars,
            &mut hashes,
            &mut var_expiry,
            &mut hash_expiry,
            &mut files,
            &mut bins,
            &mut windows,
            &mut users,
        );
        let mut e = |n: &str, args: &[&str]| {
            eval_ident(
                &mut rt,
                n,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "",
            )
        };
        assert_eq!(e("istok", &["a b c", "b", "32"]), "$true");
        assert_eq!(e("istok", &["a b c", "z", "32"]), "$false");
        assert_eq!(e("numtok", &["", "32"]), "0");
        assert_eq!(e("numtok", &["..a..b...c.", "46"]), "3");
        assert_eq!(e("numtok", &["....", "46"]), "0");
        assert_eq!(e("gettok", &["..a..b...c.", "2", "46"]), "b");
        assert_eq!(e("gettok", &["..a..b...c.", "2-", "46"]), "b.c");
        assert_eq!(e("gettok", &["..a..b...c.", "-1", "46"]), "c");
        assert_eq!(e("findtok", &["a b c b", "b", "2", "32"]), "4");
        assert_eq!(e("findtok", &["a b c b", "b", "0", "32"]), "2");
        assert_eq!(e("deltok", &["a b c d", "2", "32"]), "a c d");
        assert_eq!(e("deltok", &["a b c d", "2-3", "32"]), "a d");
        assert_eq!(e("remtok", &["a b a c", "a", "2", "32"]), "a b c");
        assert_eq!(e("remtok", &["a b a c", "a", "0", "32"]), "b c");
        assert_eq!(e("puttok", &["a b c", "X", "2", "32"]), "a X c");
        // negative N: -1 replaces the last token.
        assert_eq!(e("puttok", &["a b c d", "X", "-1", "32"]), "a b c X");
        assert_eq!(e("sorttok", &["c a b", "32"]), "a b c");
        assert_eq!(e("sorttok", &["3 1 2", "32", "n"]), "1 2 3");
        assert_eq!(e("sorttok", &["a b c", "32", "r"]), "c b a");
        // channel-prefix order ~ & @ % + then none (stable within a rank).
        assert_eq!(
            e("sorttok", &["+aa @bb +cc dd @ee", "32", "c"]),
            "@bb @ee +aa +cc dd"
        );
        // case-sensitive variants
        assert_eq!(e("istokcs", &["a B c", "B", "32"]), "$true");
        assert_eq!(e("istokcs", &["a B c", "b", "32"]), "$false");
        assert_eq!(e("replacecs", &["Hello", "l", "L"]), "HeLLo");
        assert_eq!(e("replacecs", &["Hello", "L", "x"]), "Hello");
        assert_eq!(e("poscs", &["aAa", "A", "1"]), "2");
        assert_eq!(e("countcs", &["aAa", "a"]), "2");
        assert_eq!(e("findtokcs", &["a A a", "A", "1", "32"]), "2");
        assert_eq!(e("matchtokcs", &["Apple apple", "A", "1", "32"]), "Apple");
        assert_eq!(e("wildtokcs", &["Apple apple", "A*", "1", "32"]), "Apple");
        assert_eq!(e("addtokcs", &["a B", "b", "32"]), "a B b");
        assert_eq!(e("remtokcs", &["a A a", "A", "1", "32"]), "a a");
        assert_eq!(e("reptokcs", &["a A a", "A", "X", "1", "32"]), "a X a");
        assert_eq!(e("sorttokcs", &["b A a B", "32"]), "A B a b");
        assert_eq!(e("replacexcs", &["aAa", "a", "X"]), "XAX");
        assert_eq!(e("wildtok", &["cat car dog", "ca*", "2", "32"]), "car");
        assert_eq!(e("wildtok", &["cat car dog", "ca*", "0", "32"]), "2");
        assert_eq!(e("matchtok", &["cat car dog", "ar", "1", "32"]), "car");
        assert_eq!(e("strip", &["\u{2}bold\u{f} \u{3}4red"]), "bold red");
        // $strip options: strip only the requested code (colour), keep the rest.
        assert_eq!(e("strip", &["\u{2}b\u{3}4c", "c"]), "\u{2}bc"); // only colour removed
        assert_eq!(e("strip", &["\u{2}b\u{3}4c", "b"]), "b\u{3}4c"); // only bold removed
        assert_eq!(e("qt", &["a b"]), "\"a b\"");
        // Regex: $regex sets up captures that $regml reads back.
        assert_eq!(e("regex", &["abc123", "([a-z]+)(\\d+)"]), "1");
        assert_eq!(e("regml", &["1"]), "abc");
        assert_eq!(e("regml", &["2"]), "123");
        assert_eq!(e("regsub", &["hello world", "o", "0"]), "hell0 world");
    }

    #[test]
    fn regex_names_global_results_and_metadata_follow_mirc() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = rt_for(
            &script,
            &mut vars,
            &mut hashes,
            &mut var_expiry,
            &mut hash_expiry,
            &mut files,
            &mut bins,
            &mut windows,
            &mut users,
        );
        let mut e = |name: &str, args: &[&str], prop: &str| {
            eval_ident(
                &mut rt,
                name,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                prop,
            )
        };

        assert_eq!(
            e(
                "regex",
                &["alpha", "é1 z2", "/(?P<letter>\\p{L})(\\d)/g"],
                ""
            ),
            "2"
        );
        assert_eq!(e("regml", &["alpha", "0"], ""), "4");
        assert_eq!(e("regml", &["alpha", "1"], ""), "é");
        assert_eq!(e("regml", &["alpha", "1"], "pos"), "1");
        assert_eq!(e("regml", &["alpha", "1"], "bytepos"), "1");
        assert_eq!(e("regml", &["alpha", "1"], "group"), "1");
        assert_eq!(e("regml", &["alpha", "3"], ""), "z");
        assert_eq!(e("regml", &["alpha", "3"], "pos"), "4");
        assert_eq!(e("regml", &["alpha", "3"], "bytepos"), "5");
        assert_eq!(e("regml", &["alpha", "3"], "match"), "2");
        assert_eq!(e("regmlex", &["alpha", "2", "2"], ""), "2");
        assert_eq!(e("regmlex", &["alpha", "2", "-1"], ""), "z2");

        // The unnamed namespace is independent and a missing /g stops after the
        // first full match instead of treating captures_iter as implicitly global.
        assert_eq!(e("regex", &["a1 b2", "(\\w)(\\d)"], ""), "1");
        assert_eq!(e("regml", &["0"], ""), "2");
        assert_eq!(e("regml", &["1"], ""), "a");
        assert_eq!(e("regml", &["alpha", "3"], ""), "z");

        // Without F empty captures are omitted; F keeps fixed group indexing.
        assert_eq!(e("regex", &["a", "/^(a)?(b)?$/"], ""), "1");
        assert_eq!(e("regml", &["0"], ""), "1");
        assert_eq!(e("regex", &["a", "/^(a)?(b)?$/F"], ""), "1");
        assert_eq!(e("regml", &["0"], ""), "2");
        assert_eq!(e("regmlex", &["1", "2"], ""), "");
        assert_eq!(e("regmlex", &["1", "2"], "group"), "2");

        // Saving a result into a binary variable returns its byte length.
        assert_eq!(e("regml", &["1", "&capture"], ""), "1");
        assert_eq!(e("regmlex", &["alpha", "2", "&global"], ""), "1");
        assert_eq!(rt.bins.text("&capture", 0, None), "a");
        assert_eq!(rt.bins.text("&global", 0, None), "z");
    }

    #[test]
    fn regex_modifiers_and_substitution_scope_follow_mirc() {
        assert!(!mirc_regex_is_match("xfoo", "/foo/A"));
        assert!(mirc_regex_is_match("foo", "/foo/A"));
        assert!(mirc_regex_is_match("\u{2}hello\u{2}", "/^hello$/S"));
        assert!(mirc_regex_is_match("foo\n", "/foo$/"));
        assert!(!mirc_regex_is_match("foo\n", "/foo$/D"));
        assert!(!mirc_regex_is_match("foo\n", "/foo$/E"));

        let ungreedy = mirc_regex("/(a+)/U").unwrap();
        let captures = ungreedy.regex.captures(b"aaa").unwrap().unwrap();
        assert_eq!(captures.get(1).unwrap().as_bytes(), b"a");
    }

    #[test]
    fn base_and_number_identifiers() {
        assert_eq!(base_convert("255", 10, 16, 0), "FF");
        assert_eq!(base_convert("5", 10, 16, 2), "05");
        assert_eq!(base_convert("FF", 16, 10, 0), "255");
        assert_eq!(base_convert("1010", 2, 10, 0), "10");
        assert_eq!(base_convert("-15", 10, 16, 0), "-F");
        assert_eq!(format_duration(0), "0secs");
        assert_eq!(format_duration(1), "1sec");
        assert_eq!(format_duration(90), "1min30secs");
        assert_eq!(format_duration(90061), "1day1hr1min1sec");
        assert_eq!(format_duration_without_seconds(30), "0mins");
        assert_eq!(format_duration_without_seconds(3691), "1hr1min");
    }

    #[test]
    fn gettok_ranges() {
        let toks = ["a", "b", "c", "d", "e"];
        assert_eq!(gettok_range(&toks, "3", '.'), "c");
        assert_eq!(gettok_range(&toks, "2-4", '.'), "b.c.d");
        assert_eq!(gettok_range(&toks, "2-", '.'), "b.c.d.e");
        assert_eq!(gettok_range(&toks, "-1", '.'), "e");
        assert_eq!(gettok_range(&toks, "9", '.'), "");
    }

    #[test]
    fn runtime_and_network_environment_identifiers() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        assert!(!port_is_free(&port, "127.0.0.1"));
        drop(listener);
        assert!(port_is_free(&port, "127.0.0.1"));
        assert!(!port_is_free("0", ""));
        assert!(!port_is_free("not-a-port", ""));
        assert!(!port_is_free(&port, "not-an-ip"));
    }

    #[test]
    fn ident_round_base_concat() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut var_expiry = HashMap::new();
        let mut hash_expiry = HashMap::new();
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut users = crate::script::users::UserList::default();
        let mut rt = rt_for(
            &script,
            &mut vars,
            &mut hashes,
            &mut var_expiry,
            &mut hash_expiry,
            &mut files,
            &mut bins,
            &mut windows,
            &mut users,
        );
        let mut e = |n: &str, args: &[&str]| {
            eval_ident(
                &mut rt,
                n,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "",
            )
        };
        assert_eq!(e("base", &["255", "10", "16"]), "FF");
        assert_eq!(e("round", &["3.14159", "2"]), "3.14");
        assert_eq!(e("round", &["3.6", "0"]), "4");
        assert_eq!(e("duration", &["3661"]), "1hr1min1sec");
        assert_eq!(e("timestampfmt", &[]), "[HH:nn]");
        assert_eq!(e("logstampfmt", &[]), "[HH:nn]");
        assert!(e("timestamp", &[]).starts_with('['));
        assert!(e("ticksqpc", &[]).parse::<u64>().is_ok());
        assert!(e("uptime", &["mirc", "3"]).parse::<u64>().is_ok());
        assert_eq!(e("uptime", &["system"]), "");
        assert_eq!(e("remote", &[]), "7");
        assert_eq!(e("starting", &[]), "0");
        assert_eq!(e("exiting", &[]), "0");
        assert_eq!(e("status", &[]), "disconnected");
        assert_eq!(e("gettok", &["a.b.c.d", "2-3", "46"]), "b.c");
        // $r letter range stays within bounds
        let r = e("r", &["a", "a"]);
        assert_eq!(r, "a");
    }

    #[test]
    fn process_command_line_preserves_argument_order() {
        assert_eq!(
            process_command_line(["--profile", "Work IRC", "--portable"]),
            "--profile Work IRC --portable"
        );
        assert_eq!(process_command_line(std::iter::empty::<&str>()), "");
    }

    #[test]
    fn portable_marker_is_checked_beside_the_executable() {
        let root = std::env::temp_dir().join(format!("jirc-portable-ident-{}", std::process::id()));
        let executable = root.join("bin").join("jirc-test");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        assert!(!portable_from_executable(&executable));
        std::fs::write(executable.parent().unwrap().join("portable.txt"), "").unwrap();
        assert!(portable_from_executable(&executable));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn samepath_stays_in_the_sandbox_and_matches_platform_case_rules() {
        let root = std::env::temp_dir().join(format!("jirc-samepath-ident-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "test").unwrap();

        assert!(same_sandbox_path(&root, "file.txt", "nested/file.txt"));
        assert!(same_sandbox_path(&root, "../file.txt", "file.txt"));
        assert!(!same_sandbox_path(&root, "file.txt", "other.txt"));
        assert_eq!(
            same_sandbox_path(&root, "FILE.TXT", "file.txt"),
            cfg!(windows)
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
