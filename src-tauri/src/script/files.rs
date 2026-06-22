//! File-handle I/O for mSL: `/fopen`, `/fwrite`, `/fread`, `/fgetc`, `/fseek`,
//! `/fclose` and the `$fopen` / `$feof` / `$ferr` identifiers.
//!
//! Handles persist across script runs (you `/fopen` in one event and `/fwrite`
//! or `$fread` in another), so the [`FileStore`] lives in the engine's global
//! state. We keep a lightweight `{path, pos}` per handle and re-open the file
//! for each operation (seeking to the saved position) rather than holding an OS
//! file object inside the global mutex — which keeps the state cheap to clone
//! and avoids leaking OS handles if a script forgets to `/fclose`.

use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use super::eval::wildcard_match;

#[derive(Clone, Default)]
pub struct FileHandle {
    pub path: PathBuf,
    pub pos: u64,
    pub eof: bool,
    pub err: bool,
}

/// How `/fseek` should move the pointer.
pub enum SeekMode {
    /// `/fseek name N` — absolute byte position.
    Byte(u64),
    /// `/fseek -l name N` — start of line N (1-based).
    Line(u64),
    /// `/fseek -n name` — start of the next line.
    Next,
    /// `/fseek -p name` — start of the current line (or the previous line if the
    /// pointer is already at a line start).
    Prev,
    /// `/fseek -w name wildcard` — start of the next line matching a wildcard.
    Wild(String),
    /// `/fseek -r name regex` — start of the next line matching a regex.
    Regex(String),
}

#[derive(Default)]
pub struct FileStore {
    handles: HashMap<String, FileHandle>,
    /// `$feof` / `$ferr` — the result of the last file access in any script.
    pub feof: bool,
    pub ferr: bool,
}

impl FileStore {
    /// `/fopen [-no]` — returns true on success. `create_new` = `-n` (create if
    /// absent, fail if present); `overwrite` = `-o` (create/truncate).
    pub fn open(&mut self, name: &str, path: PathBuf, create_new: bool, overwrite: bool) -> bool {
        use std::fs::OpenOptions;
        let ok = if overwrite {
            OpenOptions::new().write(true).create(true).truncate(true).open(&path).is_ok()
        } else if create_new {
            OpenOptions::new().write(true).create_new(true).open(&path).is_ok()
        } else {
            path.is_file()
        };
        self.feof = false;
        self.ferr = !ok;
        if ok {
            self.handles
                .insert(name.to_string(), FileHandle { path, pos: 0, eof: false, err: false });
        }
        ok
    }

    /// `/fclose <name | wildcard>`.
    pub fn close(&mut self, name_or_wild: &str) {
        if name_or_wild.contains(['*', '?']) {
            let hit: Vec<String> = self
                .handles
                .keys()
                .filter(|k| wildcard_match(name_or_wild, k))
                .cloned()
                .collect();
            for k in hit {
                self.handles.remove(&k);
            }
        } else {
            self.handles.remove(name_or_wild);
        }
    }

    /// `/fwrite [-n]` — write at the current pointer; `-n` appends a `$crlf`.
    pub fn write(&mut self, name: &str, text: &[u8], newline: bool) {
        let Self { handles, feof, ferr } = self;
        *feof = false;
        let Some(h) = handles.get_mut(name) else {
            *ferr = true;
            return;
        };
        let mut data = text.to_vec();
        if newline {
            data.extend_from_slice(b"\r\n");
        }
        match write_at(&h.path, h.pos, &data) {
            Ok(()) => {
                h.pos += data.len() as u64;
                h.err = false;
                *ferr = false;
            }
            Err(_) => {
                h.err = true;
                *ferr = true;
            }
        }
    }

    /// `$fread(name)` — the next `$crlf`-delimited line (without the line break).
    pub fn read_line(&mut self, name: &str) -> String {
        let Self { handles, feof, ferr } = self;
        *feof = false;
        let Some(h) = handles.get_mut(name) else {
            *ferr = true;
            return String::new();
        };
        match read_line_at(&h.path, h.pos) {
            Ok(buf) => {
                let n = buf.len() as u64;
                h.pos += n;
                h.eof = n == 0;
                h.err = false;
                *feof = h.eof;
                *ferr = false;
                strip_crlf(buf)
            }
            Err(_) => {
                h.err = true;
                *ferr = true;
                String::new()
            }
        }
    }

    /// `$fgetc(name)` — the next character (byte).
    pub fn read_char(&mut self, name: &str) -> String {
        let Self { handles, feof, ferr } = self;
        *feof = false;
        let Some(h) = handles.get_mut(name) else {
            *ferr = true;
            return String::new();
        };
        match read_byte_at(&h.path, h.pos) {
            Ok(Some(c)) => {
                h.pos += 1;
                h.eof = false;
                h.err = false;
                *ferr = false;
                (c as char).to_string()
            }
            Ok(None) => {
                h.eof = true;
                *feof = true;
                h.err = false;
                *ferr = false;
                String::new()
            }
            Err(_) => {
                h.err = true;
                *ferr = true;
                String::new()
            }
        }
    }

    /// `/fseek` — move the pointer per [`SeekMode`].
    pub fn seek(&mut self, name: &str, mode: SeekMode) {
        let Self { handles, feof, ferr } = self;
        *feof = false;
        let Some(h) = handles.get_mut(name) else {
            *ferr = true;
            return;
        };
        let data = match std::fs::read(&h.path) {
            Ok(d) => d,
            Err(_) => {
                h.err = true;
                *ferr = true;
                return;
            }
        };
        let len = data.len() as u64;
        let new_pos = match mode {
            SeekMode::Byte(n) => n.min(len),
            SeekMode::Line(n) => line_start(&data, n).unwrap_or(len),
            SeekMode::Next => next_line(&data, h.pos),
            SeekMode::Prev => prev_line(&data, h.pos),
            SeekMode::Wild(p) => {
                find_line(&data, h.pos, |line| wildcard_match(&p, line)).unwrap_or(h.pos)
            }
            SeekMode::Regex(p) => match build_regex(&p) {
                Some(re) => find_line(&data, h.pos, |line| re.is_match(line)).unwrap_or(h.pos),
                None => h.pos,
            },
        };
        h.pos = new_pos;
        h.eof = new_pos >= len;
        h.err = false;
        *feof = h.eof;
        *ferr = false;
    }

    /// `$fopen(name).prop` — the handle, if open.
    pub fn handle(&self, name: &str) -> Option<&FileHandle> {
        self.handles.get(name)
    }

    /// Open handle names, sorted (for `$fopen(N)`).
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.handles.keys().cloned().collect();
        v.sort();
        v
    }

    /// `$fopen(0)` — number of open handles.
    pub fn count(&self) -> usize {
        self.handles.len()
    }
}

fn write_at(path: &PathBuf, pos: u64, data: &[u8]) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    f.seek(SeekFrom::Start(pos))?;
    f.write_all(data)?;
    Ok(())
}

fn read_line_at(path: &PathBuf, pos: u64) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(pos))?;
    let mut reader = std::io::BufReader::new(f);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf)?;
    Ok(buf)
}

fn read_byte_at(path: &PathBuf, pos: u64) -> std::io::Result<Option<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(pos))?;
    let mut buf = [0u8; 1];
    match f.read(&mut buf)? {
        0 => Ok(None),
        _ => Ok(Some(buf[0])),
    }
}

fn strip_crlf(mut b: Vec<u8>) -> String {
    if b.last() == Some(&b'\n') {
        b.pop();
        if b.last() == Some(&b'\r') {
            b.pop();
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

/// Byte offset of the start of each line (index 0 is always 0).
fn line_offsets(data: &[u8]) -> Vec<u64> {
    let mut offs = vec![0u64];
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            offs.push((i + 1) as u64);
        }
    }
    offs
}

fn line_start(data: &[u8], n: u64) -> Option<u64> {
    if n == 0 {
        return Some(0);
    }
    line_offsets(data).get((n - 1) as usize).copied()
}

fn next_line(data: &[u8], pos: u64) -> u64 {
    line_offsets(data)
        .into_iter()
        .find(|&o| o > pos)
        .unwrap_or(data.len() as u64)
}

fn prev_line(data: &[u8], pos: u64) -> u64 {
    let offs = line_offsets(data);
    let cur = offs.iter().copied().filter(|&o| o <= pos).max().unwrap_or(0);
    if cur == pos {
        offs.iter().copied().filter(|&o| o < pos).max().unwrap_or(0)
    } else {
        cur
    }
}

/// Start offset of the first line at/after `pos` for which `pred` is true.
fn find_line<F: Fn(&str) -> bool>(data: &[u8], pos: u64, pred: F) -> Option<u64> {
    let offs = line_offsets(data);
    for (i, &start) in offs.iter().enumerate() {
        if start < pos {
            continue;
        }
        let end = offs.get(i + 1).copied().unwrap_or(data.len() as u64);
        let line = strip_crlf(data[start as usize..end as usize].to_vec());
        if pred(&line) {
            return Some(start);
        }
    }
    None
}

/// Parse a mIRC `/pattern/flags` (or bare) regex for `/fseek -r`.
fn build_regex(pat: &str) -> Option<regex::Regex> {
    let body = pat.trim();
    let re = if body.starts_with('/') && body.len() > 1 {
        match body.rfind('/') {
            Some(end) if end > 0 => {
                let inner = &body[1..end];
                let flags = &body[end + 1..];
                let prefix: String = ['i', 's', 'm']
                    .into_iter()
                    .filter(|c| flags.contains(*c))
                    .collect();
                if prefix.is_empty() {
                    inner.to_string()
                } else {
                    format!("(?{prefix}){inner}")
                }
            }
            _ => body.to_string(),
        }
    } else {
        body.to_string()
    };
    regex::Regex::new(&re).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("jirc-files-{}-{name}", std::process::id()))
    }

    #[test]
    fn write_read_seek_round_trip() {
        let p = tmp("rt.txt");
        let _ = std::fs::remove_file(&p);
        let mut fs = FileStore::default();

        // -o creates/overwrites; write three lines.
        assert!(fs.open("h", p.clone(), false, true));
        fs.write("h", b"alpha", true);
        fs.write("h", b"beta", true);
        fs.write("h", b"gamma", true);
        assert_eq!(fs.handle("h").unwrap().pos, 5 + 4 + 5 + 6); // +2 crlf each = 7+6+7
        fs.close("h");

        // plain open requires existence; read the lines back.
        assert!(fs.open("h", p.clone(), false, false));
        assert!(!fs.feof);
        assert_eq!(fs.read_line("h"), "alpha");
        assert_eq!(fs.read_line("h"), "beta");
        // seek by line (1-based) then read.
        fs.seek("h", SeekMode::Line(1));
        assert_eq!(fs.read_line("h"), "alpha");
        // wildcard seek.
        fs.seek("h", SeekMode::Byte(0));
        fs.seek("h", SeekMode::Wild("gam*".into()));
        assert_eq!(fs.read_line("h"), "gamma");
        // reading past the end sets $feof.
        let _ = fs.read_line("h");
        assert!(fs.feof);
        fs.close("h");

        // plain open of a missing file fails and sets $ferr.
        let missing = tmp("nope.txt");
        let _ = std::fs::remove_file(&missing);
        assert!(!fs.open("x", missing, false, false));
        assert!(fs.ferr);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn fgetc_and_props() {
        let p = tmp("gc.txt");
        let mut fs = FileStore::default();
        assert!(fs.open("g", p.clone(), false, true));
        fs.write("g", b"Hi", false);
        fs.close("g");
        assert!(fs.open("g", p.clone(), false, false));
        assert_eq!(fs.read_char("g"), "H");
        assert_eq!(fs.read_char("g"), "i");
        assert_eq!(fs.handle("g").unwrap().pos, 2);
        assert_eq!(fs.read_char("g"), ""); // eof
        assert!(fs.feof);
        assert_eq!(fs.names(), vec!["g".to_string()]);
        assert_eq!(fs.count(), 1);
        fs.close("g");
        assert_eq!(fs.count(), 0);
        let _ = std::fs::remove_file(&p);
    }
}
