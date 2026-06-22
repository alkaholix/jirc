//! Built-in mSL identifiers ($me, $nick, $rand, string functions, …).

use std::time::{SystemTime, UNIX_EPOCH};

use super::eval::{eval_bool_public, wildcard_match, Runtime, SOCK_BR_KEY};

/// Evaluates `$name(args...)` with an optional `.property` suffix (empty when
/// none). Args are already expanded.
pub fn eval_ident(rt: &mut Runtime, name: &str, args: &[String], prop: &str) -> String {
    let a = |i: usize| args.get(i).cloned().unwrap_or_default();
    match name.to_ascii_lowercase().as_str() {
        "me" => rt.my_nick.to_string(),
        "nick" => {
            // $nick = event nick; $nick(#chan, N) = Nth member (N=0 → count).
            if args.len() >= 2 {
                match rt.state.channels.iter().find(|c| c.name.eq_ignore_ascii_case(&a(0))) {
                    Some(v) => {
                        let n: usize = a(1).parse().unwrap_or(0);
                        if n == 0 {
                            v.nicks.len().to_string()
                        } else {
                            v.nicks.get(n - 1).cloned().unwrap_or_default()
                        }
                    }
                    None => String::new(),
                }
            } else {
                rt.event.nick.clone()
            }
        }
        // The secondary nick/target: kicked user (on KICK), new nick (on NICK),
        // or the affected nick/mask in per-mode events (on OP/BAN/VOICE/…).
        "knick" | "newnick" | "opnick" | "bnick" | "vnick" | "hnick" => rt.event.knick.clone(),
        "chan" => {
            // $chan = event channel; $chan(N) = Nth joined channel (N=0 → count).
            if args.is_empty() {
                rt.event.chan.clone()
            } else {
                let n: usize = a(0).parse().unwrap_or(0);
                if n == 0 {
                    rt.state.channels.len().to_string()
                } else {
                    rt.state.channels.get(n - 1).map(|c| c.name.clone()).unwrap_or_default()
                }
            }
        }
        "onchan" => {
            // $onchan(#chan) -> are you in that channel?
            if rt.state.channels.iter().any(|c| c.name.eq_ignore_ascii_case(&a(0))) {
                "$true".to_string()
            } else {
                "$false".to_string()
            }
        }
        "did" => {
            // $did(dialog, control) -> the control's current value (from the
            // event snapshot the UI sent).
            rt.event.did.get(&a(1)).cloned().unwrap_or_default()
        }
        "address" => {
            // Bare $address -> the triggering user's user@host; $address(nick) ->
            // that nick's user@host; $address(nick, type) -> masked address.
            let who = if args.is_empty() { rt.event.nick.to_lowercase() } else { a(0).to_lowercase() };
            match rt.state.ial.iter().find(|(n, _)| *n == who) {
                Some((_, full)) => {
                    if args.len() >= 2 {
                        mask_address(full, a(1).parse().unwrap_or(0))
                    } else {
                        full.split_once('!').map(|(_, h)| h.to_string()).unwrap_or_default()
                    }
                }
                None => String::new(),
            }
        }
        // The triggering user's address pieces, looked up from the IAL:
        // $fulladdress = nick!user@host, $site = host, $wildsite = *!*@host.
        "fulladdress" => {
            let who = rt.event.nick.to_lowercase();
            rt.state.ial.iter().find(|(n, _)| *n == who).map(|(_, f)| f.clone()).unwrap_or_default()
        }
        "site" => {
            let who = rt.event.nick.to_lowercase();
            rt.state
                .ial
                .iter()
                .find(|(n, _)| *n == who)
                .and_then(|(_, f)| f.split_once('@').map(|(_, h)| h.to_string()))
                .unwrap_or_default()
        }
        "wildsite" => {
            let who = rt.event.nick.to_lowercase();
            rt.state
                .ial
                .iter()
                .find(|(n, _)| *n == who)
                .and_then(|(_, f)| f.split_once('@').map(|(_, h)| format!("*!*@{h}")))
                .unwrap_or_default()
        }
        "mask" => {
            // $mask(nick!user@host, type) -> wildcard mask of that type.
            mask_address(&a(0), a(1).parse().unwrap_or(0))
        }
        "ial" => {
            // $ial(mask, N) -> Nth nick whose address matches the wildcard mask
            // (N=0 -> count).
            let mut hits: Vec<&str> = rt
                .state
                .ial
                .iter()
                .filter(|(_, full)| wildcard_match(&a(0), full))
                .filter_map(|(_, full)| full.split('!').next())
                .collect();
            hits.sort_unstable();
            let n: usize = a(1).parse().unwrap_or(0);
            if n == 0 {
                hits.len().to_string()
            } else {
                hits.get(n - 1).map(|s| s.to_string()).unwrap_or_default()
            }
        }
        "comchan" => {
            // $comchan(nick, N) -> Nth channel you share with nick (N=0 → count).
            let who = a(0).to_lowercase();
            let common: Vec<&String> = rt
                .state
                .channels
                .iter()
                .filter(|c| c.nicks.iter().any(|m| m.to_lowercase() == who))
                .map(|c| &c.name)
                .collect();
            let n: usize = a(1).parse().unwrap_or(0);
            if n == 0 {
                common.len().to_string()
            } else {
                common.get(n - 1).map(|s| s.to_string()).unwrap_or_default()
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
        "network" => rt.network.to_string(),
        "server" => rt.server.to_string(),
        "true" => "$true".to_string(),
        "false" => "$false".to_string(),
        "null" => String::new(),
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
        "time" => fmt_time(now_secs()),
        "date" => fmt_date(now_secs()),
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
            // $mid(text, S, N) -> N chars from position S; N=0 (or absent) = to end.
            let start: usize = a(1).parse().unwrap_or(1);
            let count = a(2).parse::<usize>().ok().filter(|&c| c > 0).unwrap_or(usize::MAX);
            a(0)
                .chars()
                .skip(start.saturating_sub(1))
                .take(count)
                .collect()
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
        "str" => {
            let n: usize = a(1).parse().unwrap_or(0);
            a(0).repeat(n)
        }
        "rand" | "r" => {
            let (lo, hi) = (a(0), a(1));
            match (lo.parse::<i64>(), hi.parse::<i64>()) {
                (Ok(x), Ok(y)) => rand_range(x, y).to_string(),
                _ => {
                    // Letter range: $rand(a,z) / $r(A,Z).
                    match (lo.chars().next(), hi.chars().next()) {
                        (Some(l), Some(h)) if l.is_ascii_alphabetic() && h.is_ascii_alphabetic() => {
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
        "scriptdir" | "mircdir" => {
            format!("{}{}", rt.data_dir.display(), std::path::MAIN_SEPARATOR)
        }
        "iif" => {
            if eval_bool_public(&a(0)) {
                a(1)
            } else {
                a(2)
            }
        }
        "calc" => calc(&a(0))
            .map(fmt_num)
            .unwrap_or_default(),
        "gettok" => {
            let sep = sep_code(&a(2));
            let text = a(0);
            let toks: Vec<&str> = text.split(sep).collect();
            gettok_range(&toks, &a(1), sep)
        }
        "numtok" => {
            let sep = a(1).parse::<u32>().ok().and_then(char::from_u32).unwrap_or(' ');
            a(0).split(sep).count().to_string()
        }
        "hget" => {
            // $hget(table) -> table name if it exists; $hget(table, item) -> value;
            // $hget(table, N).item / .data -> Nth key name / value in sorted order
            // (N=0 -> the item count), for iterating a table.
            if args.len() < 2 {
                if rt.hashes.contains_key(&a(0)) {
                    a(0)
                } else {
                    String::new()
                }
            } else if prop.eq_ignore_ascii_case("item") || prop.eq_ignore_ascii_case("data") {
                match rt.hashes.get(&a(0)) {
                    Some(h) => {
                        let mut keys: Vec<&String> = h.keys().collect();
                        keys.sort();
                        let n: usize = a(1).parse().unwrap_or(0);
                        if n == 0 {
                            keys.len().to_string()
                        } else if let Some(k) = keys.get(n - 1) {
                            if prop.eq_ignore_ascii_case("item") {
                                (*k).clone()
                            } else {
                                h.get(*k).cloned().unwrap_or_default()
                            }
                        } else {
                            String::new()
                        }
                    }
                    None => String::new(),
                }
            } else {
                rt.hashes
                    .get(&a(0))
                    .and_then(|h| h.get(&a(1)))
                    .cloned()
                    .unwrap_or_default()
            }
        }
        "hfind" => {
            // $hfind(table, wildcard, N) -> the Nth matching item name (keys are
            // sorted for a stable order). N=0 returns the match count.
            let n: usize = a(2).parse().unwrap_or(1);
            let mut keys: Vec<&String> = rt
                .hashes
                .get(&a(0))
                .map(|h| h.keys().filter(|k| wildcard_match(&a(1), k)).collect())
                .unwrap_or_default();
            keys.sort();
            if n == 0 {
                keys.len().to_string()
            } else {
                keys.get(n - 1).map(|s| s.to_string()).unwrap_or_default()
            }
        }
        // Socket identifiers (used inside on SOCKOPEN/SOCKREAD/SOCKCLOSE).
        "sock" => {
            // $sock(name) -> the name if a matching socket exists (else empty),
            // so `if ($sock(x))` works; $sock(name).property reads any property
            // (.port/.ip/.addr/.status/.mark/.sent/.rcvd/.ls/.lr/.to/.type/...).
            let name = a(0);
            if prop.is_empty() {
                if rt.sockets.exists(&name) {
                    name
                } else {
                    String::new()
                }
            } else {
                rt.sockets.prop(&name, prop)
            }
        }
        "sockname" => rt.event.chan.clone(),
        "sockbr" => rt.vars.get(SOCK_BR_KEY).cloned().unwrap_or_else(|| "0".to_string()),
        "sockerr" => "0".to_string(),
        "replace" => {
            // $replace(text, from1, to1, from2, to2, ...) -> sequential replaces.
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
        "remove" => {
            // $remove(text, substr1, substr2, ...) -> remove all of each.
            let mut text = a(0);
            for s in args.iter().skip(1).filter(|s| !s.is_empty()) {
                text = text.replace(s.as_str(), "");
            }
            text
        }
        "instok" => {
            // $instok(text, token, N, C) -> insert token at the Nth position.
            let sep = sep_code(&a(3));
            let mut toks: Vec<String> = if a(0).is_empty() {
                Vec::new()
            } else {
                a(0).split(sep).map(String::from).collect()
            };
            let n = a(2).parse::<usize>().unwrap_or(1).max(1);
            let idx = (n - 1).min(toks.len());
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
                    if t == token {
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
                let positions: Vec<usize> = hay.match_indices(&needle).map(|(i, _)| i).collect();
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
            // (default 1st); 0 if not found.
            let needle = a(1);
            let hay = a(0);
            let n = a(2).parse::<usize>().unwrap_or(1).max(1);
            if needle.is_empty() {
                "0".to_string()
            } else {
                let mut from = 0usize;
                let mut found = 0usize;
                let mut result = 0usize;
                while let Some(rel) = hay.get(from..).and_then(|h| h.find(&needle)) {
                    let byte_idx = from + rel;
                    found += 1;
                    if found == n {
                        result = hay[..byte_idx].chars().count() + 1;
                        break;
                    }
                    from = byte_idx + needle.len();
                }
                result.to_string()
            }
        }
        "count" => {
            // $count(text, substr1, substr2, ...) -> total occurrences of all.
            let hay = a(0);
            let total: usize = args
                .iter()
                .skip(1)
                .filter(|s| !s.is_empty())
                .map(|s| hay.matches(s.as_str()).count())
                .sum();
            total.to_string()
        }
        "reverse" => a(0).chars().rev().collect(),
        "abs" => a(0).parse::<f64>().map(|n| fmt_num(n.abs())).unwrap_or_default(),
        "int" => a(0).parse::<f64>().map(|n| (n.trunc() as i64).to_string()).unwrap_or_default(),
        "ceil" => a(0).parse::<f64>().map(|n| (n.ceil() as i64).to_string()).unwrap_or_default(),
        "floor" => a(0).parse::<f64>().map(|n| (n.floor() as i64).to_string()).unwrap_or_default(),
        "min" => num2(&a(0), &a(1), f64::min),
        "max" => num2(&a(0), &a(1), f64::max),
        "addtok" => {
            // $addtok(list, token, sepcode)
            let sep = sep_code(&a(2));
            let exists = a(0).split(sep).any(|t| t == a(1));
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
            if !a(1).is_empty() && a(0).split(sep).any(|t| t == a(1)) {
                "$true".to_string()
            } else {
                "$false".to_string()
            }
        }
        "findtok" => {
            // $findtok(list, token, N, sepcode) -> position of the Nth match (else 0)
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(1).max(1);
            let mut seen = 0;
            let mut result = 0;
            for (i, t) in a(0).split(sep).enumerate() {
                if t == a(1) {
                    seen += 1;
                    if seen == n {
                        result = i + 1;
                        break;
                    }
                }
            }
            result.to_string()
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
            let n = a(2).parse::<usize>().unwrap_or(1).max(1);
            let (list, token) = (a(0), a(1));
            let mut seen = 0;
            list.split(sep)
                .filter(|t| {
                    if *t == token {
                        seen += 1;
                        seen != n
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>()
                .join(&sep.to_string())
        }
        "puttok" => {
            // $puttok(list, token, N, sepcode) -> replace the Nth token
            let sep = sep_code(&a(3));
            let n = a(2).parse::<usize>().unwrap_or(0);
            let mut toks: Vec<String> = a(0).split(sep).map(String::from).collect();
            if n >= 1 && n <= toks.len() {
                toks[n - 1] = a(1);
            }
            toks.join(&sep.to_string())
        }
        "sorttok" => {
            // $sorttok(list, sepcode, [options]) -> sorted; opts: n=numeric, r=reverse
            let sep = sep_code(&a(1));
            let opts = a(2).to_lowercase();
            let mut toks: Vec<String> = a(0).split(sep).map(String::from).collect();
            if opts.contains('n') {
                toks.sort_by(|x, y| {
                    let (x, y) = (x.parse::<f64>().unwrap_or(0.0), y.parse::<f64>().unwrap_or(0.0));
                    x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                });
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
            let m: Vec<&str> = list.split(sep).filter(|t| wildcard_match(&wild, t)).collect();
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
                Ok(n) => std::env::vars().nth(n - 1).map(|(k, _)| k).unwrap_or_default(),
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
        "strip" => strip_codes(&a(0)),
        "regex" => {
            // $regex([name,] text, pattern) -> match count; stores the first
            // match's capture groups for $regml. The optional name is ignored.
            let (text, pat) = if args.len() >= 3 { (a(1), a(2)) } else { (a(0), a(1)) };
            match mirc_regex(&pat) {
                Some(re) => {
                    rt.vars.retain(|k, _| !k.starts_with(REGML_PREFIX));
                    let count = re.find_iter(&text).count();
                    if let Some(caps) = re.captures(&text) {
                        for (i, g) in caps.iter().enumerate() {
                            let v = g.map(|m| m.as_str().to_string()).unwrap_or_default();
                            rt.vars.insert(format!("{REGML_PREFIX}{i}"), v);
                        }
                    }
                    count.to_string()
                }
                None => "0".to_string(),
            }
        }
        "regml" => {
            // $regml([name,] N) -> Nth capture group from the last $regex.
            let n = if args.len() >= 2 { a(1) } else { a(0) };
            let n: usize = n.parse().unwrap_or(1);
            rt.vars.get(&format!("{REGML_PREFIX}{n}")).cloned().unwrap_or_default()
        }
        "regsub" => {
            // $regsub(text, pattern, replacement) -> replaced text. mIRC \1
            // backreferences are translated to the regex crate's ${1} form, and
            // /pattern/flags is honoured. (mIRC's [name,]…,%var form isn't
            // supported — args are pre-expanded, so the %var name is lost.)
            let (text, pat, rep) = (a(0), a(1), a(2));
            match mirc_regex(&pat) {
                Some(re) => re.replace_all(&text, translate_backrefs(&rep).as_str()).into_owned(),
                None => text,
            }
        }
        "read" => {
            // $read(file, [N]) -> the Nth line (1-based), or a random line.
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = content.lines().collect();
            if lines.is_empty() {
                String::new()
            } else if args.len() >= 2 {
                let n: usize = a(1).parse().unwrap_or(0);
                lines.get(n.saturating_sub(1)).copied().unwrap_or("").to_string()
            } else {
                let idx = rand_range(0, lines.len() as i64 - 1) as usize;
                lines.get(idx).copied().unwrap_or("").to_string()
            }
        }
        "lines" => {
            // $lines(file) -> number of lines in the file.
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            std::fs::read_to_string(&path).map(|c| c.lines().count()).unwrap_or(0).to_string()
        }
        // A user-defined alias used as an identifier ($myalias): run it and use
        // its `/return` value.
        other => {
            if let Some(alias) = rt.script.find_alias(other) {
                let body = alias.body.clone();
                return rt.call_alias(&body, args.to_vec());
            }
            // Unknown identifier: render literally so it is visible.
            if args.is_empty() {
                format!("${other}")
            } else {
                format!("${other}({})", args.join(","))
            }
        }
    }
}

/// Builds a wildcard hostmask from a `nick!user@host` address, following mIRC's
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
const REGML_PREFIX: &str = "\u{0}re";

/// Translates mIRC `\1`..`\9` replacement backreferences into the regex crate's
/// `${1}` form, escaping any literal `$` first.
fn translate_backrefs(s: &str) -> String {
    let escaped = s.replace('$', "$$");
    let chars: Vec<char> = escaped.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && matches!(chars.get(i + 1), Some(c) if c.is_ascii_digit()) {
            out.push_str(&format!("${{{}}}", chars[i + 1]));
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Turns a token "sepcode" argument (an ASCII code) into its character; spaces
/// are the mIRC default when absent or invalid.
/// Compiles a mIRC-style regex pattern. Handles the `/pattern/flags` form —
/// `i` (case-insensitive), `m` (multiline), `s` (dotall), `x` (extended); other
/// flags like `g` are ignored (global matching is handled by the caller). A bare
/// pattern (no surrounding slashes) is compiled as-is.
fn mirc_regex(pat: &str) -> Option<regex::Regex> {
    let p = pat.trim();
    let (body, flags) = match (p.strip_prefix('/'), p.rfind('/')) {
        (Some(_), Some(close)) if close > 0 => (&p[1..close], &p[close + 1..]),
        _ => (p, ""),
    };
    let inline: String = flags.chars().filter(|c| matches!(c, 'i' | 'm' | 's' | 'x')).collect();
    let full = if inline.is_empty() {
        body.to_string()
    } else {
        format!("(?{inline}){body}")
    };
    regex::Regex::new(&full).ok()
}

fn sep_code(s: &str) -> char {
    s.trim().parse::<u32>().ok().and_then(char::from_u32).unwrap_or(' ')
}

fn bool_str(b: bool) -> String {
    if b { "$true" } else { "$false" }.to_string()
}

/// `$gettok` index/range resolver: `N`, `N-`, `N1-N2`, and negative indices
/// (`-1` = last token). Returns the joined slice, or empty if out of range.
fn gettok_range(toks: &[&str], spec: &str, sep: char) -> String {
    let len = toks.len() as i64;
    let norm = |n: i64| if n < 0 { len + n + 1 } else { n };
    let spec = spec.trim();
    // A '-' after position 0 marks a range; a leading '-' is a negative index.
    let range_dash = spec.char_indices().find(|&(i, c)| c == '-' && i > 0).map(|(i, _)| i);
    let (lo, hi) = match range_dash {
        Some(p) => {
            let l = spec[..p].trim();
            let r = spec[p + 1..].trim();
            let l = if l.is_empty() { 1 } else { norm(l.parse().unwrap_or(1)) };
            let r = if r.is_empty() { len } else { norm(r.parse().unwrap_or(len)) };
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
    let units = [("wk", 604800), ("day", 86400), ("hr", 3600), ("min", 60), ("sec", 1)];
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
        let hi = if hi.trim().is_empty() { len } else { hi.trim().parse().unwrap_or(lo) };
        (lo, hi)
    } else {
        let n = spec.parse().unwrap_or(0);
        (n, n)
    }
}

/// Removes mIRC formatting control codes (bold, colour, underline, …).
fn strip_codes(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\u{2}' | '\u{f}' | '\u{16}' | '\u{1d}' | '\u{1e}' | '\u{1f}' | '\u{11}' => i += 1,
            '\u{3}' => {
                i += 1;
                let mut d = 0;
                while d < 2 && matches!(chars.get(i), Some(c) if c.is_ascii_digit()) {
                    i += 1;
                    d += 1;
                }
                if chars.get(i) == Some(&',') && matches!(chars.get(i + 1), Some(c) if c.is_ascii_digit()) {
                    i += 1;
                    let mut d2 = 0;
                    while d2 < 2 && matches!(chars.get(i), Some(c) if c.is_ascii_digit()) {
                        i += 1;
                        d2 += 1;
                    }
                }
            }
            '\u{4}' => {
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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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

/// Milliseconds since this process started (for `$ticks` — scripts use deltas).
fn ticks() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

fn fmt_time(secs: u64) -> String {
    let s = secs % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn fmt_date(secs: u64) -> String {
    // Days since epoch -> Y/M/D (civil calendar, UTC).
    let days = (secs / 86400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days->civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
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

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
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
    fn string_helpers_via_eval() {
        use crate::script::ast::Script;
        use crate::script::eval::{EventVars, Runtime};
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut rt = Runtime {
            script: &script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars: &mut vars,
            hashes: &mut hashes,
            event: EventVars::default(),
            actions: vec![],
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
            sockets: std::sync::Arc::new(crate::script::eval::NoSockets),
        };
        assert_eq!(eval_ident(&mut rt, "replace", &["abcabc".into(), "b".into(), "X".into()], ""), "aXcaXc");
        assert_eq!(eval_ident(&mut rt, "remove", &["abcabc".into(), "a".into()], ""), "bcbc");
        assert_eq!(eval_ident(&mut rt, "pos", &["hello".into(), "l".into()], ""), "3");
        assert_eq!(eval_ident(&mut rt, "count", &["banana".into(), "a".into()], ""), "3");
        assert_eq!(eval_ident(&mut rt, "reverse", &["abc".into()], ""), "cba");
        assert_eq!(eval_ident(&mut rt, "max", &["3".into(), "7".into()], ""), "7");
        // mIRC-compat: Nth-occurrence $pos/$lastpos, N=0 $mid, multiple args.
        let mut id = |n: &str, a: &[&str]| {
            eval_ident(&mut rt, n, &a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "")
        };
        assert_eq!(id("pos", &["hello", "l", "2"]), "4");
        assert_eq!(id("pos", &["hello", "l", "3"]), "0");
        assert_eq!(id("lastpos", &["hello", "l"]), "4");
        assert_eq!(id("lastpos", &["hello", "l", "2"]), "3");
        assert_eq!(id("mid", &["hello", "2", "0"]), "ello");
        assert_eq!(id("mid", &["hello", "2", "3"]), "ell");
        assert_eq!(id("count", &["banana", "a", "n"]), "5");
        assert_eq!(id("replace", &["abcabc", "a", "X", "c", "Y"]), "XbYXbY");
        assert_eq!(id("remove", &["abcabc", "a", "c"]), "bb");
        assert_eq!(id("instok", &["a.b.c", "X", "2", "46"]), "a.X.b.c");
        assert_eq!(id("reptok", &["a.b.a.c", "a", "X", "2", "46"]), "a.b.X.c");
        assert_eq!(id("reptok", &["a.b.a", "a", "X", "0", "46"]), "X.b.X");
        // mIRC /pattern/flags regex: i = case-insensitive; bare = case-sensitive.
        assert_eq!(id("regex", &["Hello", "/hello/i"]), "1");
        assert_eq!(id("regex", &["Hello", "hello"]), "0");
        assert_eq!(id("regsub", &["Hello World", "/o/g", "0"]), "Hell0 W0rld");
        // file-name identifiers
        assert_eq!(id("nopath", &["C:\\folder\\file.txt"]), "file.txt");
        assert_eq!(id("nopath", &["/usr/bin/foo"]), "foo");
        assert_eq!(id("nofile", &["C:\\folder\\file.txt"]), "C:\\folder\\");
        assert_eq!(id("nofile", &["bare.txt"]), "");
        assert_eq!(id("longfn", &["foo.txt"]), "foo.txt");
        assert!(id("ticks", &[]).parse::<u64>().is_ok());
        assert!(id("gmt", &[]).parse::<u64>().is_ok());
        assert_eq!(id("noqt", &["\"hello world\""]), "hello world");
        assert_eq!(id("noqt", &["plain"]), "plain");
        assert_eq!(id("bytes", &["1234567"]), "1,234,567");
        assert!(id("envvar", &["0"]).parse::<usize>().map(|c| c > 0).unwrap_or(false));
    }

    fn rt_for<'a>(
        script: &'a crate::script::ast::Script,
        vars: &'a mut std::collections::HashMap<String, String>,
        hashes: &'a mut std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    ) -> Runtime<'a> {
        use crate::script::eval::EventVars;
        Runtime {
            script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars,
            hashes,
            event: EventVars::default(),
            actions: vec![],
            halted: false,
            steps: 0,
            depth: 0,
            ret: None,
            goto: None,
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
            sockets: std::sync::Arc::new(crate::script::eval::NoSockets),
        }
    }

    #[test]
    fn token_identifiers() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut rt = rt_for(&script, &mut vars, &mut hashes);
        let mut e = |n: &str, args: &[&str]| {
            eval_ident(&mut rt, n, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "")
        };
        assert_eq!(e("istok", &["a b c", "b", "32"]), "$true");
        assert_eq!(e("istok", &["a b c", "z", "32"]), "$false");
        assert_eq!(e("findtok", &["a b c b", "b", "2", "32"]), "4");
        assert_eq!(e("deltok", &["a b c d", "2", "32"]), "a c d");
        assert_eq!(e("deltok", &["a b c d", "2-3", "32"]), "a d");
        assert_eq!(e("remtok", &["a b a c", "a", "2", "32"]), "a b c");
        assert_eq!(e("puttok", &["a b c", "X", "2", "32"]), "a X c");
        assert_eq!(e("sorttok", &["c a b", "32"]), "a b c");
        assert_eq!(e("sorttok", &["3 1 2", "32", "n"]), "1 2 3");
        assert_eq!(e("sorttok", &["a b c", "32", "r"]), "c b a");
        assert_eq!(e("wildtok", &["cat car dog", "ca*", "2", "32"]), "car");
        assert_eq!(e("wildtok", &["cat car dog", "ca*", "0", "32"]), "2");
        assert_eq!(e("matchtok", &["cat car dog", "ar", "1", "32"]), "car");
        assert_eq!(e("strip", &["\u{2}bold\u{f} \u{3}4red"]), "bold red");
        assert_eq!(e("qt", &["a b"]), "\"a b\"");
        // Regex: $regex sets up captures that $regml reads back.
        assert_eq!(e("regex", &["abc123", "([a-z]+)(\\d+)"]), "1");
        assert_eq!(e("regml", &["1"]), "abc");
        assert_eq!(e("regml", &["2"]), "123");
        assert_eq!(e("regsub", &["hello world", "o", "0"]), "hell0 w0rld");
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
    fn ident_round_base_concat() {
        use crate::script::ast::Script;
        use std::collections::HashMap;
        let script = Script::default();
        let mut vars = HashMap::new();
        let mut hashes = HashMap::new();
        let mut rt = rt_for(&script, &mut vars, &mut hashes);
        let mut e = |n: &str, args: &[&str]| {
            eval_ident(&mut rt, n, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "")
        };
        assert_eq!(e("base", &["255", "10", "16"]), "FF");
        assert_eq!(e("round", &["3.14159", "2"]), "3.14");
        assert_eq!(e("round", &["3.6", "0"]), "4");
        assert_eq!(e("duration", &["3661"]), "1hr1min1sec");
        assert_eq!(e("gettok", &["a.b.c.d", "2-3", "46"]), "b.c");
        // $r letter range stays within bounds
        let r = e("r", &["a", "a"]);
        assert_eq!(r, "a");
    }
}
