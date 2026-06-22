//! Minimal INI file support for mSL `/writeini`, `/remini`, `$readini`, `$ini`.
//!
//! Format: `[section]` headers followed by `item=value` lines. Section and item
//! lookups are case-insensitive (like mIRC); order is preserved.

type Sections = Vec<(String, Vec<(String, String)>)>;

fn parse(text: &str) -> Sections {
    let mut out: Sections = Vec::new();
    let mut cur: Option<usize> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            out.push((name.trim().to_string(), Vec::new()));
            cur = Some(out.len() - 1);
        } else if let Some((k, v)) = t.split_once('=') {
            if let Some(i) = cur {
                out[i].1.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

fn serialize(data: &Sections) -> String {
    let mut out = String::new();
    for (sec, items) in data {
        out.push_str(&format!("[{sec}]\n"));
        for (k, v) in items {
            out.push_str(&format!("{k}={v}\n"));
        }
        out.push('\n');
    }
    out
}

/// `$readini(file, section, item)` — the item's value (None if absent).
pub fn read(text: &str, section: &str, item: &str) -> Option<String> {
    parse(text)
        .into_iter()
        .find(|(s, _)| s.eq_ignore_ascii_case(section))
        .and_then(|(_, items)| {
            items.into_iter().find(|(k, _)| k.eq_ignore_ascii_case(item)).map(|(_, v)| v)
        })
}

/// `/writeini` — set section/item=value, returning the new file text.
pub fn set(text: &str, section: &str, item: &str, value: &str) -> String {
    let mut data = parse(text);
    let si = match data.iter().position(|(s, _)| s.eq_ignore_ascii_case(section)) {
        Some(i) => i,
        None => {
            data.push((section.to_string(), Vec::new()));
            data.len() - 1
        }
    };
    if let Some(kv) = data[si].1.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(item)) {
        kv.1 = value.to_string();
    } else {
        data[si].1.push((item.to_string(), value.to_string()));
    }
    serialize(&data)
}

/// `/remini` — remove an item, or the whole section if `item` is None.
pub fn remove(text: &str, section: &str, item: Option<&str>) -> String {
    let mut data = parse(text);
    match item {
        Some(item) => {
            if let Some((_, items)) = data.iter_mut().find(|(s, _)| s.eq_ignore_ascii_case(section)) {
                items.retain(|(k, _)| !k.eq_ignore_ascii_case(item));
            }
        }
        None => data.retain(|(s, _)| !s.eq_ignore_ascii_case(section)),
    }
    serialize(&data)
}

/// Section names, in order (for `$ini(file, N)`).
pub fn sections(text: &str) -> Vec<String> {
    parse(text).into_iter().map(|(s, _)| s).collect()
}

/// Item names in a section, in order (for `$ini(file, section, N)`).
pub fn items(text: &str, section: &str) -> Vec<String> {
    parse(text)
        .into_iter()
        .find(|(s, _)| s.eq_ignore_ascii_case(section))
        .map(|(_, items)| items.into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_round_trip() {
        let mut t = set("", "User", "nick", "bob");
        t = set(&t, "User", "host", "x.example");
        t = set(&t, "Opts", "color", "4");
        assert_eq!(read(&t, "User", "nick").as_deref(), Some("bob"));
        assert_eq!(read(&t, "user", "HOST").as_deref(), Some("x.example")); // case-insensitive
        assert_eq!(read(&t, "Opts", "color").as_deref(), Some("4"));
        assert_eq!(read(&t, "User", "missing"), None);
        // overwrite
        t = set(&t, "User", "nick", "alice");
        assert_eq!(read(&t, "User", "nick").as_deref(), Some("alice"));
        // enumerate
        assert_eq!(sections(&t), vec!["User".to_string(), "Opts".to_string()]);
        assert_eq!(items(&t, "User"), vec!["nick".to_string(), "host".to_string()]);
        // remove item, then section
        t = remove(&t, "User", Some("host"));
        assert_eq!(read(&t, "User", "host"), None);
        assert_eq!(read(&t, "User", "nick").as_deref(), Some("alice"));
        t = remove(&t, "Opts", None);
        assert_eq!(sections(&t), vec!["User".to_string()]);
    }
}
