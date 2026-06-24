//! Built-in mSL identifiers ($me, $nick, $rand, string functions, …).

use std::time::{SystemTime, UNIX_EPOCH};

use super::eval::{eval_bool_public, wildcard_match, Runtime, SOCK_BR_KEY};
use sha2::Digest; // brings the Digest trait into scope for Md5/Sha1/Sha2 too

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
        "ialchan" => {
            // $ialchan(mask, #, N) -> Nth nick on channel # whose address matches the
            // mask (N=0 -> count). Like $ial, restricted to that channel's members.
            let members: std::collections::HashSet<String> = rt
                .state
                .channels
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&a(1)))
                .map(|c| c.nicks.iter().map(|n| n.to_lowercase()).collect())
                .unwrap_or_default();
            let mut hits: Vec<&str> = rt
                .state
                .ial
                .iter()
                .filter(|(nick, full)| {
                    members.contains(&nick.to_lowercase()) && wildcard_match(&a(0), full)
                })
                .filter_map(|(_, full)| full.split('!').next())
                .collect();
            hits.sort_unstable();
            let n: usize = a(2).parse().unwrap_or(0);
            if n == 0 {
                hits.len().to_string()
            } else {
                hits.get(n - 1).map(|s| s.to_string()).unwrap_or_default()
            }
        }
        "halted" => bool_str(rt.halted),
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
        "time" => chrono::Local::now().format("%H:%M:%S").to_string(),
        "date" => chrono::Local::now().format("%d/%m/%Y").to_string(),
        "fulldate" => chrono::Local::now().format("%a %b %d %H:%M:%S %Y").to_string(),
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
        // $rands is the cryptographically-secure variant; the output (a random
        // value in range) is indistinguishable, so it shares $rand's logic.
        "rand" | "r" | "rands" => {
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
                return if name == "isbit" { "0".into() } else { v.to_string() };
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
                let parts: Vec<u32> = arg.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect();
                if parts.len() == 4 {
                    parts.iter().fold(0u32, |acc, &p| (acc << 8) | (p & 0xFF)).to_string()
                } else {
                    String::new()
                }
            } else {
                let n: u32 = arg.trim().parse().unwrap_or(0);
                format!("{}.{}.{}.{}", (n >> 24) & 0xFF, (n >> 16) & 0xFF, (n >> 8) & 0xFF, n & 0xFF)
            }
        }
        // $os — OS family. mIRC returns a Windows version; we are cross-platform.
        "os" => std::env::consts::OS.to_string(),
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
            base.join(format!("tmp{}_{}", std::process::id(), process_start().elapsed().as_nanos()))
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
            format!("{},{},{},{}", is.chanmodes_a, is.chanmodes_b, is.chanmodes_c, is.chanmodes_d)
        }
        "chantypes" => rt.state.isupport.chan_types.clone(),
        "modespl" => rt.state.isupport.modes.to_string(),
        // $isalias(name) — $true if a user alias by that name is defined.
        "isalias" => bool_str(rt.script.find_alias(&a(0)).is_some()),
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
        "ssl" => bool_str(rt.state.tls),
        "anick" => rt.state.alt_nick.clone(),
        "fullname" => rt.state.realname.clone(),
        "usermode" => rt.state.user_mode.clone(),
        "away" => bool_str(rt.state.away),
        "awaymsg" => rt.state.away_msg.clone(),
        // $online — seconds connected so far; $awaytime — unix time you went away.
        "online" => {
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
        // $bvar(&v,N[,M]) — ASCII byte values from 1-based N (N=0 = length);
        // the .text property returns the bytes as text.
        "bvar" => {
            let n: i64 = a(1).trim().parse().unwrap_or(0);
            let m: Option<i64> = a(2).trim().parse().ok();
            if prop == "text" {
                rt.bins.text(&a(0), n, m)
            } else {
                rt.bins.bvar(&a(0), n, m)
            }
        }
        // $bfind(&v,N,M) — 1-based position of byte value M (or text) at/after N.
        "bfind" => {
            let n: i64 = a(1).trim().parse().unwrap_or(1);
            match a(2).trim().parse::<u16>() {
                Ok(v) if v <= 255 => rt.bins.bfind(&a(0), n, v as u8).to_string(),
                _ => rt.bins.bfind_text(&a(0), n, a(2).as_bytes()).to_string(),
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
                "title" => rt.windows.get(&name).map(|w| w.title.clone()).unwrap_or_default(),
                "type" => rt.windows.get(&name).map(|w| w.kind.as_str().to_string()).unwrap_or_default(),
                _ => String::new(),
            }
        }
        // $line(@name, N) — the Nth line of a custom window (1-based).
        "line" => {
            let n: usize = a(1).trim().parse().unwrap_or(0);
            rt.windows.line(&a(0), n)
        }
        // $replacex (single-pass, non-recursive replace of from/to pairs).
        "replacex" => {
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
            replacex(&s, &pairs)
        }
        // $powmod(B,E,M) — modular exponentiation (modular inverse for negative E).
        "powmod" => powmod(
            a(0).trim().parse().unwrap_or(0),
            a(1).trim().parse().unwrap_or(0),
            a(2).trim().parse().unwrap_or(0),
        ),
        // Our strings are already UTF-8, so $utfencode/$utfdecode are identity.
        "utfencode" | "utfdecode" => a(0),
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
                Ok(re) => {
                    rt.vars.remove(REGERR_KEY);
                    rt.vars.retain(|k, _| !k.starts_with(REGML_PREFIX));
                    // Store every match's capture groups for $regmlex (keyed
                    // `<prefix>m<M>.<N>`), and the first match's groups flat
                    // (`<prefix><N>`) for $regml.
                    let mut count = 0usize;
                    for (m, caps) in re.captures_iter(&text).enumerate() {
                        count += 1;
                        for (g, grp) in caps.iter().enumerate() {
                            let v = grp.map(|x| x.as_str().to_string()).unwrap_or_default();
                            rt.vars.insert(format!("{REGML_PREFIX}m{}.{}", m + 1, g), v.clone());
                            if m == 0 {
                                rt.vars.insert(format!("{REGML_PREFIX}{g}"), v);
                            }
                        }
                    }
                    count.to_string()
                }
                Err(e) => {
                    rt.vars.insert(REGERR_KEY.to_string(), e);
                    "0".to_string()
                }
            }
        }
        "regml" => {
            // $regml([name,] N) -> Nth capture group from the last $regex.
            let n = if args.len() >= 2 { a(1) } else { a(0) };
            let n: usize = n.parse().unwrap_or(1);
            rt.vars.get(&format!("{REGML_PREFIX}{n}")).cloned().unwrap_or_default()
        }
        "regmlex" => {
            // $regmlex([name,] M, N) -> Nth capture group of the Mth match (N
            // defaults to 1). Skips an optional leading (non-numeric) name.
            let i = usize::from(args.first().map_or(false, |s| s.trim().parse::<usize>().is_err()));
            let m: usize = a(i).trim().parse().unwrap_or(1);
            let n: usize = a(i + 1).trim().parse().unwrap_or(1);
            rt.vars.get(&format!("{REGML_PREFIX}m{m}.{n}")).cloned().unwrap_or_default()
        }
        "regsub" => {
            // $regsub(text, pattern, replacement) -> replaced text. mIRC \1
            // backreferences are translated to the regex crate's ${1} form, and
            // /pattern/flags is honoured. (mIRC's [name,]…,%var form isn't
            // supported — args are pre-expanded, so the %var name is lost.)
            let (text, pat, rep) = (a(0), a(1), a(2));
            match mirc_regex(&pat) {
                Ok(re) => {
                    rt.vars.remove(REGERR_KEY);
                    re.replace_all(&text, translate_backrefs(&rep).as_str()).into_owned()
                }
                Err(e) => {
                    rt.vars.insert(REGERR_KEY.to_string(), e);
                    text
                }
            }
        }
        "regerrstr" => rt.vars.get(REGERR_KEY).cloned().unwrap_or_default(),
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
                "pos" => rt.files.handle(&name).map(|h| h.pos.to_string()).unwrap_or_default(),
                "eof" => bool_str(rt.files.handle(&name).map(|h| h.eof).unwrap_or(false)),
                "err" => bool_str(rt.files.handle(&name).map(|h| h.err).unwrap_or(false)),
                _ => String::new(),
            }
        }
        "readini" => {
            // $readini(file, [n], section, item) -> an item's value. The optional
            // `n` switch (no evaluation) is accepted but a no-op for us.
            let off = if args.len() >= 4 && a(1).eq_ignore_ascii_case("n") { 1 } else { 0 };
            let path = super::eval::sandbox_path(&rt.data_dir, &a(0));
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            super::ini::read(&text, &a(1 + off), &a(2 + off)).unwrap_or_default()
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
/// `$mkfn`/`$mknickfn` — replace characters that are invalid in a filename
/// (`\ / : * ? " < > |` and control chars) with `_`, so the result is safe on disk.
fn mkfn(name: &str) -> String {
    name.chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 0x20 {
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
const REGML_PREFIX: &str = "\u{0}re";

/// Reserved var key where the last regex compile error is stashed, for `$regerrstr`.
const REGERR_KEY: &str = "\u{0}regerr";

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
fn mirc_regex(pat: &str) -> Result<regex::Regex, String> {
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
    regex::Regex::new(&full).map_err(|e| e.to_string())
}

/// `$regsubex([name,] text, pattern, subtext)` — replace each match of `pattern`
/// in `text` with the *evaluated* `subtext`. `subtext` arrives RAW (the engine
/// bypasses pre-expansion for `$regsubex`) and is evaluated once per match, after
/// its markers are substituted: `\t`=whole match, `\1`..`\9`=capture group,
/// `\0`=number of groups, `\n`=match number, `\a`/`\A`=all groups (spaced/joined).
pub fn eval_regsubex(rt: &mut Runtime, raw: &[String]) -> String {
    let off = usize::from(raw.len() >= 4); // skip an optional leading [name]
    let text = rt.expand(raw.get(off).map_or("", |s| s));
    let pat = rt.expand(raw.get(off + 1).map_or("", |s| s));
    let subtext = raw.get(off + 2).cloned().unwrap_or_default();
    let re = match mirc_regex(&pat) {
        Ok(r) => r,
        Err(e) => {
            rt.vars.insert(REGERR_KEY.to_string(), e);
            return text;
        }
    };
    rt.vars.remove(REGERR_KEY);
    let group_count = re.captures_len().saturating_sub(1);
    // Collect match spans + groups first (immutable borrow of `text`), then
    // evaluate each replacement (which needs a mutable borrow of `rt`).
    let matches: Vec<(usize, usize, Vec<String>)> = re
        .captures_iter(&text)
        .map(|caps| {
            let m = caps.get(0).unwrap();
            let groups = caps
                .iter()
                .map(|g| g.map_or(String::new(), |x| x.as_str().to_string()))
                .collect();
            (m.start(), m.end(), groups)
        })
        .collect();
    let mut out = String::new();
    let mut last = 0;
    for (n, (start, end, groups)) in matches.iter().enumerate() {
        out.push_str(&text[last..*start]);
        out.push_str(&rt.expand(&regsubex_fill(&subtext, groups, n + 1, group_count)));
        last = *end;
    }
    out.push_str(&text[last..]);
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
                't' => out.push_str(groups.first().map_or("", |s| s)),
                'n' => out.push_str(&match_num.to_string()),
                'a' => out.push_str(&groups.iter().skip(1).cloned().collect::<Vec<_>>().join(" ")),
                'A' => out.push_str(&groups.iter().skip(1).cloned().collect::<String>()),
                '0'..='9' => {
                    let idx = c as usize - '0' as usize;
                    if idx == 0 {
                        out.push_str(&group_count.to_string());
                    } else {
                        out.push_str(groups.get(idx).map_or("", |s| s));
                    }
                }
                '\\' => out.push('\\'),
                other => out.push(other),
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
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

fn replacex(s: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for (from, to) in pairs {
            let fl = from.len();
            if fl > 0
                && rest.len() >= fl
                && rest.is_char_boundary(fl)
                && rest.as_bytes()[..fl].eq_ignore_ascii_case(from.as_bytes())
            {
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

fn fmt_num(n: f64) -> String {
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
    format!("{:0width$}", bin % 10u32.pow(digits), width = digits as usize)
}

/// HOTP/TOTP digit count: 3-10, default 6.
fn otp_digits(s: &str) -> u32 {
    s.trim().parse().ok().filter(|d| (3..=10).contains(d)).unwrap_or(6)
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
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut rt = Runtime {
            script: &script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars: &mut vars,
            hashes: &mut hashes,
            files: &mut files,
            bins: &mut bins,
            windows: &mut windows,
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
        assert!(id("envvar", &["0"]).parse::<usize>().map(|c| c > 0).unwrap_or(false));
        // local time/date — format checks (the values are timezone-dependent).
        let d = id("date", &[]);
        assert!(d.len() == 10 && &d[2..3] == "/" && &d[5..6] == "/", "date={d}");
        let t = id("time", &[]);
        assert!(t.len() == 8 && &t[2..3] == ":" && &t[5..6] == ":", "time={t}");
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
        assert_eq!(id("sha1", &["abc"]), "a9993e364706816aba3e25717850c26c9cd0d89d");
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
            id("hmac", &["The quick brown fox jumps over the lazy dog", "key", "sha256"]),
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
    fn mirc_format_translation() {
        assert_eq!(mirc_to_chrono("yyyy-mm-dd"), "%Y-%m-%d");
        assert_eq!(mirc_to_chrono("dd/mm/yyyy HH:nn:ss"), "%d/%m/%Y %H:%M:%S");
        assert_eq!(mirc_to_chrono("ddd mmm dd"), "%a %b %d");
        assert_eq!(mirc_to_chrono("h:nn tt"), "%-I:%M %p");
        assert_eq!(mirc_to_chrono("yy"), "%y");
    }

    fn rt_for<'a>(
        script: &'a crate::script::ast::Script,
        vars: &'a mut std::collections::HashMap<String, String>,
        hashes: &'a mut std::collections::HashMap<String, std::collections::HashMap<String, String>>,
        files: &'a mut crate::script::files::FileStore,
        bins: &'a mut crate::script::binvar::BinStore,
        windows: &'a mut crate::script::window::WindowStore,
    ) -> Runtime<'a> {
        use crate::script::eval::EventVars;
        Runtime {
            script,
            my_nick: "me",
            network: "n",
            server: "s",
            vars,
            hashes,
            files,
            bins,
            windows,
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
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut rt = rt_for(&script, &mut vars, &mut hashes, &mut files, &mut bins, &mut windows);
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
        let mut files = crate::script::files::FileStore::default();
        let mut bins = crate::script::binvar::BinStore::default();
        let mut windows = crate::script::window::WindowStore::default();
        let mut rt = rt_for(&script, &mut vars, &mut hashes, &mut files, &mut bins, &mut windows);
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
