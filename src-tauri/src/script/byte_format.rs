//! The unit and presentation options used by `$bytes()`.

const SUFFIXES: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Format a byte count using explicit units or automatic 1024-based scaling.
/// `3` selects automatic units and limits the displayed fraction so the whole
/// and fractional parts together contain at most three digits. At PB, large
/// integer parts remain intact rather than inventing another unit.
pub(super) fn format_bytes(value: &str, options: &str, suffix: bool) -> String {
    let number = value
        .trim()
        .replace(['d', 'D'], "e")
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .unwrap_or(0.0);
    let negative = number < 0.0;
    let mut scaled = number.abs();
    let three_digits = options.contains('3');
    let decimals = options.contains('d');
    let forced = if three_digits {
        None
    } else {
        // Unit precedence is independent of the order of the option letters.
        "bkmgtp"
            .chars()
            .enumerate()
            .filter(|(_, flag)| options.contains(*flag))
            .map(|(unit, _)| unit)
            .last()
    };
    let mut unit = 0;
    if let Some(selected) = forced {
        for _ in 0..selected {
            scaled /= 1024.0;
        }
        unit = selected;
    } else {
        while scaled >= 1024.0 && unit < 5 {
            scaled /= 1024.0;
            unit += 1;
        }
    }

    // Plain bytes retain the existing integer behaviour. Scaled quantities
    // normally show at most two decimal places; d preserves fractional values
    // at the six-place precision used by the script engine's math output.
    let mut text = if decimals {
        trim_fraction(format!("{scaled:.6}"))
    } else if unit == 0 {
        format!("{:.0}", scaled.trunc())
    } else {
        trim_fraction(format!("{scaled:.2}"))
    };
    if three_digits {
        if let Some(dot) = text.find('.') {
            let fraction = 3usize.saturating_sub(dot);
            if fraction == 0 {
                text.truncate(dot);
            } else {
                text.truncate((dot + 1 + fraction).min(text.len()));
                text = trim_fraction(text);
            }
        }
    }
    let mut result = comma_group(&text);
    if negative {
        result.insert(0, '-');
    }
    if suffix {
        result.push_str(SUFFIXES[unit]);
    }
    result
}

fn trim_fraction(mut text: String) -> String {
    if text.contains('.') {
        let length = text.trim_end_matches('0').trim_end_matches('.').len();
        text.truncate(length);
    }
    text
}

fn comma_group(text: &str) -> String {
    let (integer, fraction) = text.split_once('.').unwrap_or((text, ""));
    let mut result = String::with_capacity(text.len() + integer.len() / 3);
    for (index, digit) in integer.bytes().enumerate() {
        if index != 0 && (integer.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(digit as char);
    }
    if !fraction.is_empty() {
        result.push('.');
        result.push_str(fraction);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn explicit_units_fix_the_audited_ignored_option_cases() {
        for (value, options, expected) in [
            ("1024", "k", "1"),
            ("1048576", "m", "1"),
            ("1073741824", "g", "1"),
            ("1099511627776", "t", "1"),
            ("1125899906842624", "p", "1"),
            ("1048576", "k", "1,024"),
            ("1024", "b", "1,024"),
            ("317889213", "b", "317,889,213"),
        ] {
            assert_eq!(format_bytes(value, options, false), expected);
        }
    }

    #[test]
    fn automatic_units_preserve_signs_and_supported_suffixes() {
        for (value, expected) in [
            ("0", "0B"),
            ("1023", "1,023B"),
            ("1024", "1KB"),
            ("1536", "1.5KB"),
            ("1048576", "1MB"),
            ("1073741824", "1GB"),
            ("1099511627776", "1TB"),
            ("1125899906842624", "1PB"),
            ("1152921504606846976", "1,024PB"),
            ("-1536", "-1.5KB"),
        ] {
            assert_eq!(format_bytes(value, "", true), expected);
        }
        assert_eq!(format_bytes("-1048576", "m", false), "-1");
        assert_eq!(format_bytes("1024", "pkm", true), "0PB");
    }

    #[test]
    fn decimal_and_three_digit_options_keep_fractional_values() {
        assert_eq!(format_bytes("1536", "kd", false), "1.5");
        assert_eq!(format_bytes("11.35", "d", false), "11.35");
        assert_eq!(format_bytes("11.35", "3d", false), "11.3");
        assert_eq!(format_bytes("1536", "b3", true), "1.5KB");
        assert_eq!(format_bytes("513018", "3", false), "500");
        assert_eq!(format_bytes("126464", "3", false), "123");
        assert_eq!(format_bytes("0.123456", "d", false), "0.123456");
        assert_eq!(format_bytes("0.123456", "3d", false), "0.12");
        assert_eq!(format_bytes("1234.9", "b", false), "1,234");
    }

    #[test]
    fn exponent_values_and_invalid_inputs_are_bounded() {
        assert_eq!(format_bytes(" 1e4 ", "b", false), "10,000");
        assert_eq!(format_bytes("1D4", "b", false), "10,000");
        assert_eq!(format_bytes("-1d4", "b", false), "-10,000");
        for invalid in ["", "NaN", "inf", "-inf", "1e999", "not-a-number"] {
            assert_eq!(format_bytes(invalid, "", false), "0");
        }
        let largest = format_bytes("1.7976931348623157e308", "bd", true);
        assert!(largest.len() < 420);
        assert!(largest.ends_with('B'));
        assert!(!largest.contains(['e', 'E']));
    }
}
