//! mIRC-compatible text persistence for hash tables.
//!
//! mIRC's default format stores the item and its value on separate lines.
//! `-n` stores values only (and recreates items named `1`, `2`, ...), while
//! `-i` stores `item=value` pairs under an INI section. The binary `-b`/`-B`
//! layouts use the compatible little-endian, length-prefixed records emitted by
//! mIRC-style clients (16-bit lengths for `-b`, 32-bit lengths for `-B`).

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine};

/// Hash values remain strings in the shared script store for backwards
/// compatibility. Binary items use a NUL-prefixed representation that cannot
/// collide with text produced by mSL, and are decoded at every public boundary.
const BINARY_VALUE_PREFIX: &str = "\0jirc-hash-binary:";
const META_TABLE: &str = "\0jirc-hash-table-slots";

pub fn table_names(tables: &HashMap<String, HashMap<String, String>>) -> Vec<String> {
    let mut names = tables
        .keys()
        .filter(|name| name.as_str() != META_TABLE)
        .cloned()
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}

pub fn table_key(
    tables: &HashMap<String, HashMap<String, String>>,
    wanted: &str,
) -> Option<String> {
    table_names(tables)
        .into_iter()
        .find(|name| name.eq_ignore_ascii_case(wanted))
}

pub fn item_key(table: &HashMap<String, String>, wanted: &str) -> Option<String> {
    table
        .keys()
        .find(|item| item.eq_ignore_ascii_case(wanted))
        .cloned()
}

pub fn set_slots(tables: &mut HashMap<String, HashMap<String, String>>, table: &str, slots: usize) {
    tables
        .entry(META_TABLE.to_string())
        .or_default()
        .insert(table.to_string(), slots.max(1).to_string());
}

pub fn slots(tables: &HashMap<String, HashMap<String, String>>, table: &str) -> usize {
    tables
        .get(META_TABLE)
        .and_then(|metadata| metadata.get(table))
        .and_then(|value| value.parse().ok())
        .unwrap_or(100)
}

pub fn remove_slots(tables: &mut HashMap<String, HashMap<String, String>>, table: &str) {
    if let Some(metadata) = tables.get_mut(META_TABLE) {
        metadata.remove(table);
        if metadata.is_empty() {
            tables.remove(META_TABLE);
        }
    }
}

pub fn binary_value(bytes: &[u8]) -> String {
    format!("{BINARY_VALUE_PREFIX}{}", STANDARD.encode(bytes))
}

pub fn value_bytes(value: &str) -> Vec<u8> {
    value
        .strip_prefix(BINARY_VALUE_PREFIX)
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .unwrap_or_else(|| value.as_bytes().to_vec())
}

pub fn value_text(value: &str) -> String {
    let Some(encoded) = value.strip_prefix(BINARY_VALUE_PREFIX) else {
        return value.to_string();
    };
    let mut bytes = STANDARD.decode(encoded).unwrap_or_default();
    if let Some(end) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(end);
    }
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => error
            .into_bytes()
            .into_iter()
            .map(|byte| byte as char)
            .collect(),
    }
}

pub fn is_binary_value(value: &str) -> bool {
    value.starts_with(BINARY_VALUE_PREFIX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFormat {
    ItemsAndData,
    DataOnly,
    Ini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryFormat {
    U16,
    U32,
}

fn write_binary_length(out: &mut Vec<u8>, format: BinaryFormat, length: usize) -> bool {
    match format {
        BinaryFormat::U16 => match u16::try_from(length) {
            Ok(length) => out.extend_from_slice(&length.to_le_bytes()),
            Err(_) => return false,
        },
        BinaryFormat::U32 => match u32::try_from(length) {
            Ok(length) => out.extend_from_slice(&length.to_le_bytes()),
            Err(_) => return false,
        },
    }
    true
}

fn read_binary_length(bytes: &[u8], cursor: &mut usize, format: BinaryFormat) -> Option<usize> {
    match format {
        BinaryFormat::U16 => {
            let end = cursor.checked_add(2)?;
            let value = u16::from_le_bytes(bytes.get(*cursor..end)?.try_into().ok()?);
            *cursor = end;
            Some(value as usize)
        }
        BinaryFormat::U32 => {
            let end = cursor.checked_add(4)?;
            let value = u32::from_le_bytes(bytes.get(*cursor..end)?.try_into().ok()?);
            *cursor = end;
            usize::try_from(value).ok()
        }
    }
}

/// Serializes mIRC's `-b`/`-B` hash files. Each item and value is preceded by
/// a little-endian 16- or 32-bit byte length. `data_only` omits item names.
pub fn save_binary(
    entries: &[(String, String)],
    format: BinaryFormat,
    data_only: bool,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (item, value) in entries {
        let value = value_bytes(value);
        if !data_only {
            let item = item.as_bytes();
            if !write_binary_length(&mut out, format, item.len()) {
                return None;
            }
            out.extend_from_slice(item);
        }
        if !write_binary_length(&mut out, format, value.len()) {
            return None;
        }
        out.extend_from_slice(&value);
    }
    Some(out)
}

/// Parses mIRC `-b`/`-B` files. Some compatible clients pad empty values with
/// zero length words; zero-length item records are skipped so those files load
/// without losing the following real record.
pub fn load_binary(bytes: &[u8], format: BinaryFormat, data_only: bool) -> Vec<(String, String)> {
    let mut cursor = 0;
    let mut numeric_item = 1usize;
    let mut entries = Vec::new();
    while cursor < bytes.len() {
        let item = if data_only {
            let item = numeric_item.to_string();
            numeric_item += 1;
            item
        } else {
            let Some(item_len) = read_binary_length(bytes, &mut cursor, format) else {
                break;
            };
            if item_len == 0 {
                continue;
            }
            let Some(end) = cursor.checked_add(item_len) else {
                break;
            };
            let Some(item) = bytes.get(cursor..end) else {
                break;
            };
            cursor = end;
            String::from_utf8_lossy(item).into_owned()
        };
        let Some(value_len) = read_binary_length(bytes, &mut cursor, format) else {
            break;
        };
        let Some(end) = cursor.checked_add(value_len) else {
            break;
        };
        let Some(value) = bytes.get(cursor..end) else {
            break;
        };
        cursor = end;
        entries.push((item, binary_value(value)));
    }
    entries
}

fn one_line(value: &str) -> String {
    value.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

/// Serializes a table using one of mIRC's documented text formats.
pub fn save(entries: &[(String, String)], format: TextFormat, section: &str) -> Vec<u8> {
    let mut out = String::new();
    if format == TextFormat::Ini {
        out.push('[');
        out.push_str(section);
        out.push_str("]\r\n");
    }
    for (item, value) in entries {
        let value = value_text(value);
        match format {
            TextFormat::ItemsAndData => {
                out.push_str(&one_line(item));
                out.push_str("\r\n");
                out.push_str(&one_line(&value));
                out.push_str("\r\n");
            }
            TextFormat::DataOnly => {
                out.push_str(&one_line(&value));
                out.push_str("\r\n");
            }
            TextFormat::Ini => {
                out.push_str(&one_line(item));
                out.push('=');
                out.push_str(&one_line(&value));
                out.push_str("\r\n");
            }
        }
    }
    out.into_bytes()
}

/// Parses one of mIRC's documented text formats into item/value pairs.
pub fn load(bytes: &[u8], format: TextFormat, section: &str) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    match format {
        TextFormat::ItemsAndData => {
            let mut lines = text.lines();
            let mut out = Vec::new();
            while let Some(item) = lines.next() {
                let value = lines.next().unwrap_or_default();
                if !item.is_empty() {
                    out.push((item.to_string(), value.to_string()));
                }
            }
            out
        }
        TextFormat::DataOnly => text
            .lines()
            .enumerate()
            .map(|(i, value)| ((i + 1).to_string(), value.to_string()))
            .collect(),
        TextFormat::Ini => load_ini_section(&text, section),
    }
}

fn load_ini_section(text: &str, wanted: &str) -> Vec<(String, String)> {
    let mut in_section = false;
    let mut values = HashMap::<String, (String, String)>::new();
    let mut order = Vec::<String>::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_section = section.trim().eq_ignore_ascii_case(wanted);
            continue;
        }
        if !in_section || line.is_empty() || line.starts_with(';') {
            continue;
        }
        let Some((item, value)) = line.split_once('=') else {
            continue;
        };
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let key = item.to_ascii_lowercase();
        if !values.contains_key(&key) {
            order.push(key.clone());
        }
        values.insert(key, (item.to_string(), value.trim().to_string()));
    }
    order
        .into_iter()
        .filter_map(|key| values.remove(&key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<(String, String)> {
        vec![
            ("alpha".into(), "one".into()),
            ("beta".into(), "two words\r\ncontinued".into()),
        ]
    }

    #[test]
    fn default_format_uses_alternating_lines_and_strips_newlines() {
        let bytes = save(&entries(), TextFormat::ItemsAndData, "ignored");
        assert_eq!(bytes, b"alpha\r\none\r\nbeta\r\ntwo wordscontinued\r\n");
        assert_eq!(
            load(&bytes, TextFormat::ItemsAndData, "ignored"),
            vec![
                ("alpha".into(), "one".into()),
                ("beta".into(), "two wordscontinued".into())
            ]
        );
    }

    #[test]
    fn data_only_load_assigns_numeric_items() {
        let bytes = save(&entries(), TextFormat::DataOnly, "ignored");
        assert_eq!(bytes, b"one\r\ntwo wordscontinued\r\n");
        assert_eq!(
            load(&bytes, TextFormat::DataOnly, "ignored"),
            vec![
                ("1".into(), "one".into()),
                ("2".into(), "two wordscontinued".into())
            ]
        );
    }

    #[test]
    fn ini_load_is_case_insensitive_and_uses_only_the_requested_section() {
        let text = b"[Other]\r\nx=no\r\n[Wanted]\r\nAlpha=one\r\nalpha=last\r\n";
        assert_eq!(
            load(text, TextFormat::Ini, "wanted"),
            vec![("alpha".into(), "last".into())]
        );
        assert_eq!(
            save(&entries(), TextFormat::Ini, "Saved"),
            b"[Saved]\r\nalpha=one\r\nbeta=two wordscontinued\r\n"
        );
    }

    #[test]
    fn binary_formats_use_mirc_length_prefixed_records() {
        let entries = vec![
            ("alpha".to_string(), "one".to_string()),
            (
                "binary".to_string(),
                binary_value(&[65, 0, 66, 13, 10, 255]),
            ),
        ];
        let small = save_binary(&entries, BinaryFormat::U16, false).unwrap();
        assert_eq!(small, b"\x05\0alpha\x03\0one\x06\0binary\x06\0A\0B\r\n\xff");
        let large = save_binary(&entries, BinaryFormat::U32, false).unwrap();
        assert_eq!(
            large,
            b"\x05\0\0\0alpha\x03\0\0\0one\x06\0\0\0binary\x06\0\0\0A\0B\r\n\xff"
        );
        for loaded in [
            load_binary(&small, BinaryFormat::U16, false),
            load_binary(&large, BinaryFormat::U32, false),
        ] {
            assert_eq!(loaded.len(), entries.len());
            for ((loaded_item, loaded_value), (item, value)) in loaded.iter().zip(&entries) {
                assert_eq!(loaded_item, item);
                assert_eq!(value_bytes(loaded_value), value_bytes(value));
            }
        }
    }

    #[test]
    fn binary_loader_accepts_empty_value_padding_and_data_only_files() {
        let compatible = b"\x05\0empty\0\0\0\0\0\0\0\0\x06\0binary\x03\0A\0B";
        let loaded = load_binary(compatible, BinaryFormat::U16, false);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].0, "empty");
        assert!(value_bytes(&loaded[0].1).is_empty());
        assert_eq!(loaded[1].0, "binary");
        assert_eq!(value_bytes(&loaded[1].1), [65, 0, 66]);

        let data = b"\x05\0alpha\x04\0beta";
        let loaded = load_binary(data, BinaryFormat::U16, true);
        assert_eq!(loaded[0].0, "1");
        assert_eq!(value_bytes(&loaded[0].1), b"alpha");
        assert_eq!(loaded[1].0, "2");
        assert_eq!(value_bytes(&loaded[1].1), b"beta");
    }
}
