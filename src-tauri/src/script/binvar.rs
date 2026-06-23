//! Binary variables (`&binvar`) for mSL: `/bset`, `/bunset`, `$bvar`, `$bfind`.
//!
//! Stored in the engine's global state so they persist across script runs (like
//! hash tables). Byte positions are **1-based** (mIRC convention); `$bvar(&v,0)`
//! returns the length. The store keys on the name with any leading `&` stripped.

use std::collections::HashMap;

#[derive(Default)]
pub struct BinStore {
    vars: HashMap<String, Vec<u8>>,
}

fn key(name: &str) -> &str {
    name.trim().trim_start_matches('&')
}

impl BinStore {
    pub fn get(&self, name: &str) -> Option<&Vec<u8>> {
        self.vars.get(key(name))
    }

    /// `/bset [-z] &v N val…` — write bytes starting at 1-based position `pos`
    /// (`pos < 0` appends). `zero` (`-z`) empties the var first. Positions past
    /// the current end are zero-filled.
    pub fn set(&mut self, name: &str, pos: i64, bytes: &[u8], zero: bool) {
        let v = self.vars.entry(key(name).to_string()).or_default();
        if zero {
            v.clear();
        }
        let start = if pos < 0 {
            v.len()
        } else {
            (pos.max(1) as usize) - 1
        };
        if start > v.len() {
            v.resize(start, 0);
        }
        for (i, &b) in bytes.iter().enumerate() {
            let idx = start + i;
            if idx < v.len() {
                v[idx] = b;
            } else {
                v.push(b);
            }
        }
    }

    pub fn unset(&mut self, name: &str) {
        self.vars.remove(key(name));
    }

    /// `$bvar(&v,N[,M])` — M ASCII byte values from 1-based position N, space
    /// separated. `N == 0` returns the length; no M returns the single byte at N.
    pub fn bvar(&self, name: &str, n: i64, m: Option<i64>) -> String {
        let Some(v) = self.get(name) else {
            return String::new();
        };
        if n == 0 {
            return v.len().to_string();
        }
        let start = (n.max(1) as usize) - 1;
        let count = m.map(|c| c.max(0) as usize).unwrap_or(1);
        v.iter()
            .skip(start)
            .take(count)
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `$bvar(&v[,N,M]).text` — the bytes as a (lossy UTF-8) string.
    pub fn text(&self, name: &str, n: i64, m: Option<i64>) -> String {
        let Some(v) = self.get(name) else {
            return String::new();
        };
        let (start, count) = if n == 0 {
            (0usize, v.len())
        } else {
            ((n.max(1) as usize) - 1, m.map(|c| c.max(0) as usize).unwrap_or(v.len()))
        };
        let slice: Vec<u8> = v.iter().skip(start).take(count).copied().collect();
        String::from_utf8_lossy(&slice).into_owned()
    }

    /// `$bfind(&v,N,M)` — 1-based position of byte value M at/after position N
    /// (0 if not found).
    pub fn bfind(&self, name: &str, n: i64, m: u8) -> usize {
        let Some(v) = self.get(name) else {
            return 0;
        };
        let start = (n.max(1) as usize) - 1;
        v.iter()
            .enumerate()
            .skip(start)
            .find(|(_, &b)| b == m)
            .map(|(i, _)| i + 1)
            .unwrap_or(0)
    }

    /// `$bfind(&v,N,text)` — 1-based position of a byte subsequence (0 if none).
    pub fn bfind_text(&self, name: &str, n: i64, needle: &[u8]) -> usize {
        let Some(v) = self.get(name) else {
            return 0;
        };
        if needle.is_empty() || needle.len() > v.len() {
            return 0;
        }
        let start = (n.max(1) as usize) - 1;
        (start..=v.len() - needle.len())
            .find(|&i| &v[i..i + needle.len()] == needle)
            .map(|i| i + 1)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bset_bvar_roundtrip() {
        let mut b = BinStore::default();
        // /bset &v 1 72 105  -> "Hi"
        b.set("&v", 1, &[72, 105], false);
        assert_eq!(b.bvar("&v", 0, None), "2"); // length
        assert_eq!(b.bvar("&v", 1, None), "72"); // first byte
        assert_eq!(b.bvar("&v", 1, Some(2)), "72 105"); // both
        assert_eq!(b.text("&v", 0, None), "Hi");
        // append at -1
        b.set("v", -1, &[33], false); // '!'
        assert_eq!(b.text("v", 0, None), "Hi!");
        // overwrite at position 2
        b.set("v", 2, &[97], false); // 'a'
        assert_eq!(b.text("v", 0, None), "Ha!");
        // zero with -z
        b.set("v", 1, &[88], true); // clears then writes 'X'
        assert_eq!(b.text("v", 0, None), "X");
        // $bfind
        b.set("v", 1, &[1, 2, 3, 2, 1], true);
        assert_eq!(b.bfind("v", 1, 2), 2);
        assert_eq!(b.bfind("v", 3, 2), 4);
        assert_eq!(b.bfind("v", 1, 9), 0);
        b.unset("v");
        assert_eq!(b.bvar("v", 0, None), "");
    }
}
