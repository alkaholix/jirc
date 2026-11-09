//! File-handle I/O for mSL: `/fopen`, `/fwrite`, `/fread`, `/fgetc`, `/fseek`,
//! `/fclose` and the `$fopen` / `$feof` / `$ferr` identifiers.
//!
//! Handles persist across script runs (you `/fopen` in one event and `/fwrite`
//! or `$fread` in another), so the [`FileStore`] lives in the engine's global
//! state. Handles retain an OS file for their lifetime so `/fopen -x` can deny
//! competing writers while allowing readers. Ordinary handles still reopen
//! their path for operations; exclusive handles use their retained file.
//! Clones share ownership and the
//! final close/drop releases the OS handle and its lock.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::eval::wildcard_match;
use super::ident::mirc_regex;

/// One binary file read cannot allocate more than the HTTP body limit.
pub const MAX_BINARY_READ_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct FileHandle {
    pub path: PathBuf,
    pub pos: u64,
    pub eof: bool,
    pub err: bool,
    file: Option<Arc<Mutex<File>>>,
    exclusive: bool,
    writable: bool,
}

impl FileHandle {
    fn with_file<T>(
        &self,
        write: bool,
        action: impl FnOnce(&mut File) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        if write && !self.writable {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        if self.exclusive {
            let file = self.file.as_ref().ok_or(std::io::ErrorKind::NotFound)?;
            let mut file = file
                .lock()
                .map_err(|_| std::io::Error::other("file handle lock poisoned"))?;
            action(&mut file)
        } else {
            let mut file = OpenOptions::new()
                .read(true)
                .write(write)
                .open(&self.path)?;
            if write {
                lock_file(&file, false)?;
            }
            action(&mut file)
        }
    }

    fn read_all(&self) -> std::io::Result<Vec<u8>> {
        self.with_file(false, |file| {
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    }

    /// Encoding inspection must use the held file for an exclusive handle.
    /// It leaves the script's logical position and error/EOF flags untouched.
    pub fn bom(&self) -> Option<&'static str> {
        self.with_file(false, |file| {
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            file.take(3).read_to_end(&mut bytes)?;
            Ok(match bytes.as_slice() {
                [0xff, 0xfe, ..] => "utf16le",
                [0xfe, 0xff, ..] => "utf16be",
                [0xef, 0xbb, 0xbf, ..] => "utf8",
                _ => "ascii",
            })
        })
        .ok()
    }
}

fn lock_file(file: &File, exclusive: bool) -> std::io::Result<()> {
    // Windows sharing modes deny ordinary opens too. A byte-range shared lock
    // would unnecessarily prevent ordinary handles from writing concurrently.
    #[cfg(windows)]
    {
        let _ = (file, exclusive);
        Ok(())
    }
    // Unix locks are advisory: all jIRC file handles participate, and other
    // processes must also honour the filesystem's flock-compatible locks.
    #[cfg(not(windows))]
    {
        if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        }
        .map_err(std::io::Error::from)
    }
}

fn open_file(
    path: &PathBuf,
    create_new: bool,
    overwrite: bool,
    exclusive: bool,
) -> std::io::Result<(File, bool)> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if overwrite {
        // Acquire exclusivity before truncating, including advisory-lock hosts.
        options.create(true);
    } else if create_new {
        options.create_new(true);
    }
    #[cfg(windows)]
    if exclusive {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: readers may still open it.
    }
    let opened = options.open(path).and_then(|file| {
        lock_file(&file, exclusive)?;
        Ok(file)
    });
    let (file, writable) = match opened {
        Ok(file) => (file, true),
        // Native mIRC can open a reader while another handle holds -x. An
        // overwrite/create request must never silently become a reader.
        Err(error) if !create_new && !overwrite && allows_reader_fallback(&error) => {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            if exclusive {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(1);
            }
            let file = options.open(path)?;
            if exclusive {
                lock_file(&file, true)?;
            }
            (file, false)
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    if overwrite {
        file.set_len(0)?;
    }
    Ok((file, writable))
}

fn allows_reader_fallback(error: &std::io::Error) -> bool {
    // Rust currently classifies ERROR_SHARING_VIOLATION as Uncategorized.
    #[cfg(windows)]
    if error.raw_os_error() == Some(32) {
        return true;
    }
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
    )
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
    /// Resolve the same one-based, sorted handle enumeration as `$fopen(N)`.
    /// Zero is a count query, not an open handle.
    fn read_name(&self, selector: &str) -> Option<String> {
        match selector.parse::<usize>() {
            Ok(0) => None,
            Ok(n) => self.names().get(n - 1).cloned(),
            Err(_) => Some(selector.to_string()),
        }
    }

    /// Reject a read without consuming file bytes, including malformed binary
    /// destinations/counts detected by the identifier evaluator.
    pub fn read_failed(&mut self, selector: &str) {
        let name = self.read_name(selector);
        self.feof = false;
        self.ferr = true;
        if let Some(handle) = name.and_then(|name| self.handles.get_mut(&name)) {
            handle.eof = false;
            handle.err = true;
        }
    }

    /// `/fopen [-no]` — returns true on success. `create_new` = `-n` (create if
    /// absent, fail if present); `overwrite` = `-o` (create/truncate).
    pub fn open(&mut self, name: &str, path: PathBuf, create_new: bool, overwrite: bool) -> bool {
        self.open_exclusive(name, path, create_new, overwrite, false)
    }

    /// `-x` excludes competing writers until this handle's final close/drop.
    pub fn open_exclusive(
        &mut self,
        name: &str,
        path: PathBuf,
        create_new: bool,
        overwrite: bool,
        exclusive: bool,
    ) -> bool {
        let opened = open_file(&path, create_new, overwrite, exclusive);
        self.feof = false;
        self.ferr = opened.is_err();
        if let Ok((file, writable)) = opened {
            self.handles.insert(
                name.to_string(),
                FileHandle {
                    path,
                    pos: 0,
                    eof: false,
                    err: false,
                    file: Some(Arc::new(Mutex::new(file))),
                    exclusive,
                    writable,
                },
            );
            true
        } else {
            false
        }
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
        let Self {
            handles,
            feof,
            ferr,
        } = self;
        *feof = false;
        let Some(h) = handles.get_mut(name) else {
            *ferr = true;
            return;
        };
        let mut data = text.to_vec();
        if newline {
            data.extend_from_slice(b"\r\n");
        }
        match h.with_file(true, |file| write_at(file, h.pos, &data)) {
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

    /// `$fread(name | N)` — the next line without its trailing line break.
    pub fn read_line(&mut self, selector: &str) -> String {
        let name = self.read_name(selector);
        let Self {
            handles,
            feof,
            ferr,
        } = self;
        *feof = false;
        let Some(h) = name.and_then(|name| handles.get_mut(&name)) else {
            *ferr = true;
            return String::new();
        };
        match h.with_file(false, |file| read_line_at(file, h.pos)) {
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
                h.eof = false;
                h.err = true;
                *ferr = true;
                String::new()
            }
        }
    }

    /// `$freadex(name)` — everything from the pointer to end of file, leaving
    /// the pointer at EOF. Unlike `read_line` this keeps embedded line breaks.
    pub fn read_rest(&mut self, selector: &str) -> String {
        let name = self.read_name(selector);
        let Self {
            handles,
            feof,
            ferr,
        } = self;
        *feof = false;
        let Some(h) = name.and_then(|name| handles.get_mut(&name)) else {
            *ferr = true;
            return String::new();
        };
        match h.read_all() {
            Ok(bytes) => {
                let start = (h.pos as usize).min(bytes.len());
                let rest = &bytes[start..];
                h.pos = bytes.len() as u64;
                h.eof = true;
                h.err = false;
                *feof = true;
                *ferr = false;
                String::from_utf8_lossy(rest).into_owned()
            }
            Err(_) => {
                h.eof = false;
                h.err = true;
                *ferr = true;
                String::new()
            }
        }
    }

    /// `$fread(name | N, M, &binvar)` — at most M unchanged file bytes.
    /// `Some([])` is a successful end-of-file read; `None` is an error.
    pub fn read_bytes(&mut self, selector: &str, count: usize) -> Option<Vec<u8>> {
        if count == 0 || count > MAX_BINARY_READ_BYTES {
            self.read_failed(selector);
            return None;
        }
        let name = self.read_name(selector);
        let Self {
            handles,
            feof,
            ferr,
        } = self;
        *feof = false;
        let Some(h) = name.and_then(|name| handles.get_mut(&name)) else {
            *ferr = true;
            return None;
        };
        match h.with_file(false, |file| read_bytes_at(file, h.pos, count)) {
            Ok(bytes) => {
                h.pos += bytes.len() as u64;
                // Like text reads, a short read still returns data; EOF is
                // reported when the next read obtains no bytes.
                h.eof = bytes.is_empty();
                h.err = false;
                *feof = h.eof;
                *ferr = false;
                Some(bytes)
            }
            Err(_) => {
                h.eof = false;
                h.err = true;
                *ferr = true;
                None
            }
        }
    }

    /// `$fgetc(name | N)` — the next character (byte).
    pub fn read_char(&mut self, selector: &str) -> String {
        let name = self.read_name(selector);
        let Self {
            handles,
            feof,
            ferr,
        } = self;
        *feof = false;
        let Some(h) = name.and_then(|name| handles.get_mut(&name)) else {
            *ferr = true;
            return String::new();
        };
        match h.with_file(false, |file| read_byte_at(file, h.pos)) {
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
                h.eof = false;
                h.err = true;
                *ferr = true;
                String::new()
            }
        }
    }

    /// `/fseek` — move the pointer per [`SeekMode`].
    pub fn seek(&mut self, name: &str, mode: SeekMode) {
        let Self {
            handles,
            feof,
            ferr,
        } = self;
        *feof = false;
        let Some(h) = handles.get_mut(name) else {
            *ferr = true;
            return;
        };
        let data = match h.read_all() {
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
            SeekMode::Regex(p) => match mirc_regex(&p) {
                Ok(re) => find_line(&data, h.pos, |line| re.is_match(line).unwrap_or(false))
                    .unwrap_or(h.pos),
                Err(_) => h.pos,
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

fn write_at(f: &mut File, pos: u64, data: &[u8]) -> std::io::Result<()> {
    f.seek(SeekFrom::Start(pos))?;
    f.write_all(data)?;
    Ok(())
}

fn read_line_at(f: &mut File, pos: u64) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(pos))?;
    let mut reader = std::io::BufReader::new(f);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf)?;
    Ok(buf)
}

fn read_byte_at(f: &mut File, pos: u64) -> std::io::Result<Option<u8>> {
    f.seek(SeekFrom::Start(pos))?;
    let mut buf = [0u8; 1];
    match f.read(&mut buf)? {
        0 => Ok(None),
        _ => Ok(Some(buf[0])),
    }
}

fn read_bytes_at(file: &mut File, pos: u64, count: usize) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(pos))?;
    let mut bytes = Vec::new();
    file.take(count as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
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
    let cur = offs
        .iter()
        .copied()
        .filter(|&o| o <= pos)
        .max()
        .unwrap_or(0);
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

    #[test]
    fn binary_reads_preserve_bytes_and_report_eof_after_data_is_consumed() {
        let p = tmp("binary-read.bin");
        let bytes = [0, 255, 65, 254, 67];
        std::fs::write(&p, bytes).unwrap();
        let mut store = FileStore::default();
        assert!(store.open("raw", p.clone(), false, false));
        assert_eq!(store.read_bytes("raw", 3), Some(vec![0, 255, 65]));
        assert_eq!(store.handle("raw").unwrap().pos, 3);
        assert!(!store.feof && !store.ferr);
        assert_eq!(store.read_bytes("1", 9), Some(vec![254, 67]));
        assert_eq!(store.handle("raw").unwrap().pos, 5);
        assert!(!store.feof && !store.handle("raw").unwrap().eof);
        assert_eq!(store.read_bytes("raw", 1), Some(Vec::new()));
        assert!(store.feof && store.handle("raw").unwrap().eof);
        assert!(!store.ferr);
        assert_eq!(store.handle("raw").unwrap().pos, 5);
        store.seek("raw", SeekMode::Byte(0));
        assert_eq!(store.read_bytes("raw", 1), Some(vec![0]));
        assert!(!store.feof && !store.ferr);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn indexed_read_methods_share_fopen_handle_enumeration() {
        let a = tmp("indexed-a.txt");
        let z = tmp("indexed-z.txt");
        std::fs::write(&a, b"alpha\r\nrest").unwrap();
        std::fs::write(&z, b"Zed").unwrap();
        let mut store = FileStore::default();
        assert!(store.open("z", z.clone(), false, false));
        assert!(store.open("a", a.clone(), false, false));
        assert_eq!(store.names(), ["a", "z"]);
        assert_eq!(store.read_line("1"), "alpha");
        assert_eq!(store.read_char("2"), "Z");
        assert_eq!(store.read_rest("1"), "rest");
        assert_eq!(store.handle("a").unwrap().pos, 11);
        assert_eq!(store.handle("z").unwrap().pos, 1);
        assert_eq!(store.read_line("3"), "");
        assert!(store.ferr && !store.feof);
        assert_eq!(store.handle("a").unwrap().pos, 11);
        std::fs::remove_file(a).unwrap();
        std::fs::remove_file(z).unwrap();
    }

    #[test]
    fn invalid_binary_reads_are_bounded_and_do_not_consume_data() {
        let p = tmp("binary-errors.bin");
        std::fs::write(&p, b"abc").unwrap();
        let mut store = FileStore::default();
        assert!(store.open("h", p.clone(), false, false));
        for count in [0, MAX_BINARY_READ_BYTES + 1] {
            assert_eq!(store.read_bytes("h", count), None);
            let handle = store.handle("h").unwrap();
            assert_eq!(handle.pos, 0);
            assert!(handle.err && store.ferr);
            assert!(!handle.eof && !store.feof);
        }
        assert_eq!(store.read_bytes("9", 1), None);
        assert!(store.ferr);
        assert_eq!(store.read_bytes("h", 1), Some(vec![b'a']));
        assert!(!store.ferr && !store.handle("h").unwrap().err);
        store.read_failed("1");
        assert!(store.ferr && store.handle("h").unwrap().err);
        assert_eq!(store.handle("h").unwrap().pos, 1);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(store.read_bytes("h", 1), None);
        assert!(store.ferr && store.handle("h").unwrap().err);
        assert_eq!(store.handle("h").unwrap().pos, 1);
    }

    #[test]
    fn exclusive_owner_can_read_write_seek_and_inspect_bom() {
        let path = tmp("exclusive-owner.bin");
        let mut store = FileStore::default();
        assert!(store.open_exclusive("lock-owner", path.clone(), false, true, true));
        store.write("lock-owner", b"\xef\xbb\xbfabc\r\ndef", false);
        assert!(!store.ferr);
        assert_eq!(store.handle("lock-owner").unwrap().bom(), Some("utf8"));
        assert_eq!(store.handle("lock-owner").unwrap().pos, 11);
        store.seek("lock-owner", SeekMode::Byte(0));
        assert_eq!(store.read_bytes("lock-owner", 3), Some(vec![239, 187, 191]));
        assert_eq!(store.read_line("lock-owner"), "abc");
        assert_eq!(store.read_char("lock-owner"), "d");
        assert_eq!(store.read_rest("lock-owner"), "ef");
        store.seek("lock-owner", SeekMode::Line(2));
        assert_eq!(store.read_line("lock-owner"), "def");
        assert!(!store.ferr);
        store.close("lock-*");
        assert!(store.open("writer", path.clone(), false, true));
        store.write("writer", b"released", false);
        store.close("writer");
        assert_eq!(std::fs::read(&path).unwrap(), b"released");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn exclusive_writer_excludes_existing_writers_but_allows_new_readers() {
        let path = tmp("exclusive-read-sharing.bin");
        std::fs::write(&path, b"payload").unwrap();
        let mut first = FileStore::default();
        let mut second = FileStore::default();
        assert!(first.open("writer", path.clone(), false, false));
        assert!(!second.open_exclusive("lock", path.clone(), false, false, true));
        assert!(second.ferr);
        first.close("writer");
        assert!(second.open_exclusive("lock", path.clone(), false, false, true));
        assert!(first.open("reader", path.clone(), false, false));
        assert!(!first.ferr);
        assert_eq!(first.read_line("reader"), "payload");
        first.seek("reader", SeekMode::Byte(0));
        first.write("reader", b"corrupt", false);
        assert!(first.ferr && first.handle("reader").unwrap().err);
        assert_eq!(first.handle("reader").unwrap().pos, 0);
        second.seek("lock", SeekMode::Byte(0));
        assert_eq!(second.read_line("lock"), "payload");
        second.close("lock");
        first.close("reader");
        assert!(first.open("writer", path.clone(), false, false));
        first.write("writer", b"updated", false);
        first.close("writer");
        assert_eq!(std::fs::read(&path).unwrap(), b"updated");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_locked_overwrite_preserves_file_and_previous_named_handle() {
        let locked = tmp("exclusive-preserve.bin");
        let original = tmp("exclusive-previous.bin");
        std::fs::write(&locked, b"locked content").unwrap();
        std::fs::write(&original, b"previous").unwrap();
        let mut owner = FileStore::default();
        let mut other = FileStore::default();
        assert!(owner.open_exclusive("owner", locked.clone(), false, false, true));
        assert!(other.open("keep", original.clone(), false, false));
        assert_eq!(other.read_char("keep"), "p");
        for exclusive in [false, true] {
            assert!(!other.open_exclusive("keep", locked.clone(), false, true, exclusive));
            assert!(other.ferr);
            assert_eq!(other.handle("keep").unwrap().path, original);
            assert_eq!(other.handle("keep").unwrap().pos, 1);
        }
        assert!(!other.open_exclusive("new", locked.clone(), true, false, true));
        owner.seek("owner", SeekMode::Byte(0));
        assert_eq!(owner.read_rest("owner"), "locked content");
        owner.close("owner");
        assert!(other.open("keep", locked.clone(), false, true));
        assert_eq!(other.handle("keep").unwrap().pos, 0);
        other.close("keep");
        assert_eq!(std::fs::read(&locked).unwrap(), b"");
        std::fs::remove_file(locked).unwrap();
        std::fs::remove_file(original).unwrap();
    }

    #[test]
    fn exclusive_lock_follows_file_identity_and_last_owner() {
        let path = tmp("exclusive-identity.bin");
        let alias = tmp("exclusive-identity-link.bin");
        let replacement = tmp("exclusive-replacement.bin");
        let _ = std::fs::remove_file(&alias);
        std::fs::write(&path, b"identity").unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        let mut store = FileStore::default();
        let mut other = FileStore::default();
        assert!(store.open_exclusive("owner", path.clone(), false, false, true));
        assert!(!other.open_exclusive("alias", alias.clone(), false, true, false));
        assert!(!other.open_exclusive(
            "alias",
            path.parent()
                .unwrap()
                .join(".")
                .join(path.file_name().unwrap()),
            false,
            false,
            true
        ));
        let clone = store.handle("owner").unwrap().clone();
        // Replacing a named handle drops the store's ownership of its old file.
        assert!(store.open("owner", replacement.clone(), false, false));
        assert!(!other.open_exclusive("alias", alias.clone(), false, true, false));
        drop(clone);
        assert!(other.open("alias", alias.clone(), false, true));
        other.close("alias");
        assert!(store.open_exclusive("last", path.clone(), false, false, true));
        drop(store);
        assert!(other.open_exclusive("released", alias.clone(), false, false, true));
        other.close("released");
        std::fs::remove_file(alias).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(replacement).unwrap();
    }

    #[test]
    fn exclusive_writer_lock_is_visible_in_another_process() {
        const ENV_PATH: &str = "JIRC_FILE_LOCK_CHILD_PATH";
        const ENV_PHASE: &str = "JIRC_FILE_LOCK_CHILD_PHASE";
        if let Some(path) = std::env::var_os(ENV_PATH) {
            let path = PathBuf::from(path);
            let mut store = FileStore::default();
            if std::env::var(ENV_PHASE).unwrap() == "held" {
                assert!(store.open("reader", path.clone(), false, false));
                assert_eq!(store.read_line("reader"), "seed");
                store.write("reader", b"oops", false);
                assert!(store.ferr);
                assert!(!store.open_exclusive("writer", path.clone(), false, true, false));
                assert!(!store.open_exclusive("lock", path.clone(), false, false, true));
                #[cfg(windows)]
                {
                    assert_eq!(std::fs::read(&path).unwrap(), b"seed");
                    assert!(std::fs::write(&path, b"oops").is_err());
                }
            } else {
                assert!(store.open_exclusive("writer", path, false, false, true));
                store.write("writer", b"done", false);
                assert!(!store.ferr);
            }
            return;
        }
        let path = tmp("exclusive-process.bin");
        std::fs::write(&path, b"seed").unwrap();
        let mut owner = FileStore::default();
        assert!(owner.open_exclusive("owner", path.clone(), false, false, true));
        for phase in ["held", "released"] {
            if phase == "released" {
                owner.close("owner");
            }
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "script::files::tests::exclusive_writer_lock_is_visible_in_another_process",
                    "--nocapture",
                ])
                .env(ENV_PATH, &path)
                .env(ENV_PHASE, phase)
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{phase}: {}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            assert!(
                String::from_utf8_lossy(&child.stdout).contains("1 passed"),
                "child test did not execute"
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"done");
        std::fs::remove_file(path).unwrap();
    }
}
