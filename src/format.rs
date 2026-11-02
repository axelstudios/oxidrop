//! Human-readable formatting for sizes, percentages, and durations.

use std::time::Duration;

const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];

/// Formats a byte count with binary units and three significant digits, e.g. `48.2 KiB`.
#[must_use]
pub fn bytes(count: u64) -> String {
    if count < 1024 {
        return format!("{} B", grouped(count));
    }
    let mut value = count as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // Three significant digits: 2 decimals below 10, 1 below 100, none above.
    let decimals = usize::from(value < 10.0) + usize::from(value < 100.0);
    format!("{} {}", decimal(value, decimals), UNITS[unit])
}

/// Signed size difference, e.g. `−14.1 KiB`, `+128 B`, or `no change`.
#[must_use]
pub fn bytes_change(before: u64, after: u64) -> String {
    match after.cmp(&before) {
        std::cmp::Ordering::Less => format!("\u{2212}{}", bytes(before - after)),
        std::cmp::Ordering::Greater => format!("+{}", bytes(after - before)),
        std::cmp::Ordering::Equal => "no change".to_owned(),
    }
}

/// Formats an integer with thousands separators, e.g. `49,357`.
#[must_use]
pub fn grouped(count: u64) -> String {
    group_digits(&count.to_string())
}

/// Formats a number with `decimals` decimal places and thousands separators, e.g.
/// `1,100.9`.
#[must_use]
pub fn decimal(value: f64, decimals: usize) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let text = format!("{:.decimals$}", value.abs());
    match text.split_once('.') {
        Some((whole, fraction)) => format!("{sign}{}.{fraction}", group_digits(whole)),
        None => format!("{sign}{}", group_digits(&text)),
    }
}

/// Puts a comma between every three digits, counting from the right.
fn group_digits(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A count with the right noun form, e.g. `1 file` or `3 files`.
#[must_use]
pub fn count(n: usize, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {plural}", grouped(n as u64))
    }
}

/// Size change as a signed percentage of the original, e.g. `−35.7%`.
#[must_use]
pub fn savings(original: u64, optimized: u64) -> String {
    if original == 0 {
        return "—".to_owned();
    }
    let change = (optimized as f64 - original as f64) / original as f64 * 100.0;
    if change.abs() < 0.05 {
        "0.0%".to_owned()
    } else if change < 0.0 {
        // U+2212 MINUS SIGN lines up with digits better than a hyphen.
        format!("\u{2212}{}%", decimal(-change, 1))
    } else {
        format!("+{}%", decimal(change, 1))
    }
}

/// Short duration for table cells: `0.42 s`, `12.4 s`, `3:05`.
#[must_use]
pub fn duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.2} s")
    } else if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        clock(elapsed)
    }
}

/// Wall-clock style duration: `0:14`, `12:03`, `1:02:03`.
#[must_use]
pub fn clock(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_with_three_significant_digits() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1009), "1,009 B");
        assert_eq!(bytes(1024), "1.00 KiB");
        assert_eq!(bytes(49_357), "48.2 KiB");
        assert_eq!(bytes(250_000), "244 KiB");
        assert_eq!(bytes(2_422_210), "2.31 MiB");
        assert_eq!(bytes(1_023_000), "999 KiB");
        assert_eq!(bytes(1_024_000), "0.98 MiB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.00 GiB");
    }

    #[test]
    fn formats_signed_byte_changes() {
        assert_eq!(bytes_change(44_032, 29_594), "\u{2212}14.1 KiB");
        assert_eq!(bytes_change(100, 228), "+128 B");
        assert_eq!(bytes_change(2018, 1009), "\u{2212}1,009 B");
        assert_eq!(bytes_change(7, 7), "no change");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(grouped(7), "7");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(49_357), "49,357");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(decimal(1100.94, 1), "1,100.9");
        assert_eq!(decimal(999.96, 1), "1,000.0");
        assert_eq!(decimal(12_345.0, 0), "12,345");
        assert_eq!(decimal(-1234.5, 1), "-1,234.5");
    }

    #[test]
    fn pluralizes_counts() {
        assert_eq!(count(0, "file", "files"), "0 files");
        assert_eq!(count(1, "file", "files"), "1 file");
        assert_eq!(count(12, "folder", "folders"), "12 folders");
        assert_eq!(count(1_200, "frame", "frames"), "1,200 frames");
    }

    #[test]
    fn formats_savings() {
        assert_eq!(savings(49_357, 31_744), "\u{2212}35.7%");
        assert_eq!(savings(1000, 1000), "0.0%");
        assert_eq!(savings(1000, 1021), "+2.1%");
        assert_eq!(savings(100, 1_200), "+1,100.0%");
        assert_eq!(savings(0, 10), "—");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_millis(420)), "0.42 s");
        assert_eq!(duration(Duration::from_millis(12_400)), "12.4 s");
        assert_eq!(duration(Duration::from_secs(185)), "3:05");
        assert_eq!(clock(Duration::from_secs(14)), "0:14");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }
}
