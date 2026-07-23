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

    /// mIRC destroys all binary variables when the outer script run finishes.
    pub fn clear(&mut self) {
        self.vars.clear();
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

    /// `$bvar(&v[,N,M]).text` — the bytes as text. Valid UTF-8 is preserved;
    /// otherwise each byte maps one-to-one to the corresponding character.
    pub fn text(&self, name: &str, n: i64, m: Option<i64>) -> String {
        let Some(v) = self.get(name) else {
            return String::new();
        };
        let (start, count) = if n == 0 {
            (0usize, v.len())
        } else {
            (
                (n.max(1) as usize) - 1,
                m.map(|c| c.max(0) as usize).unwrap_or(v.len()),
            )
        };
        let mut slice: Vec<u8> = v.iter().skip(start).take(count).copied().collect();
        // `.text` stops at the first NUL; bytes after it remain available through
        // numeric `$bvar()` reads.
        if let Some(end) = slice.iter().position(|byte| *byte == 0) {
            slice.truncate(end);
        }
        match std::str::from_utf8(&slice) {
            Ok(text) => text.to_string(),
            // mIRC binary variables are byte-oriented. Preserve every non-UTF-8
            // byte one-to-one instead of replacing it with U+FFFD when a socket
            // script converts a binary line back to text for command parsing.
            Err(_) => slice.into_iter().map(|byte| byte as char).collect(),
        }
    }

    /// Reads a 16-bit value beginning at the 1-based position `n`. mIRC's
    /// `.word` property uses host (little-endian on every platform mIRC runs
    /// on) byte order, while `.nword` uses network/big-endian order.
    pub fn word(&self, name: &str, n: i64, network: bool) -> String {
        let Some(bytes) = self.slice_at(name, n, 2) else {
            return String::new();
        };
        let pair = [bytes[0], bytes[1]];
        if network {
            u16::from_be_bytes(pair).to_string()
        } else {
            u16::from_le_bytes(pair).to_string()
        }
    }

    /// Reads a 32-bit value beginning at the 1-based position `n`. `.long`
    /// uses little-endian host order and `.nlong` network/big-endian order.
    pub fn long(&self, name: &str, n: i64, network: bool) -> String {
        let Some(bytes) = self.slice_at(name, n, 4) else {
            return String::new();
        };
        let quad = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if network {
            u32::from_be_bytes(quad).to_string()
        } else {
            u32::from_le_bytes(quad).to_string()
        }
    }

    fn slice_at(&self, name: &str, n: i64, len: usize) -> Option<&[u8]> {
        if n <= 0 {
            return None;
        }
        let start = n as usize - 1;
        self.get(name)?.get(start..start.checked_add(len)?)
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
        self.bfind_text_case(name, n, needle, true)
    }

    /// Text searches in mIRC are case-insensitive by default. The `.textcs`
    /// property opts into a byte-for-byte, case-sensitive search.
    pub fn bfind_text_case(
        &self,
        name: &str,
        n: i64,
        needle: &[u8],
        case_sensitive: bool,
    ) -> usize {
        let Some(v) = self.get(name) else {
            return 0;
        };
        if needle.is_empty() || needle.len() > v.len() {
            return 0;
        }
        let start = (n.max(1) as usize) - 1;
        (start..=v.len() - needle.len())
            .find(|&i| {
                let candidate = &v[i..i + needle.len()];
                if case_sensitive {
                    candidate == needle
                } else {
                    candidate.eq_ignore_ascii_case(needle)
                }
            })
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
        b.set("v", 1, b"wavMIRC32WAV", true);
        assert_eq!(b.bfind_text_case("v", 1, b"WAV", false), 1);
        assert_eq!(b.bfind_text_case("v", 1, b"WAV", true), 10);
        b.unset("v");
        assert_eq!(b.bvar("v", 0, None), "");
    }

    #[test]
    fn word_and_long_properties_use_mirc_byte_orders() {
        let mut b = BinStore::default();
        b.set("&v", 1, &[0x0c, 0x22, 0x38, 0x4e], false);

        assert_eq!(b.word("&v", 1, false), "8716");
        assert_eq!(b.word("&v", 1, true), "3106");
        assert_eq!(b.long("&v", 1, false), "1312301580");
        assert_eq!(b.long("&v", 1, true), "203569230");
        assert_eq!(b.long("&v", 2, false), "");
    }

    #[test]
    fn text_preserves_non_utf8_bytes_one_to_one() {
        let mut b = BinStore::default();
        b.set("&raw", 1, &[0x47, 0xff, 0x80, 0x20, 0x41], false);

        assert_eq!(
            b.text("&raw", 0, None),
            ['G', '\u{00ff}', '\u{0080}', ' ', 'A']
                .into_iter()
                .collect::<String>()
        );
    }
}
