//! UTC times without a date library: RFC 3339 output and HTTP-date input
//! (for `Retry-After`).

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Exactly `n` ASCII digits.
fn digits(s: &str, n: usize) -> Option<u64> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// An IMF-fixdate (`Wed, 21 Oct 2015 07:28:00 GMT`) as seconds since the
/// epoch; `None` for anything else: a 4-digit year, a real calendar date,
/// a valid time of day, and no arithmetic that could overflow.
pub fn parse_http_date(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(' ').collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }
    if !["Mon,", "Tue,", "Wed,", "Thu,", "Fri,", "Sat,", "Sun,"].contains(&parts[0]) {
        return None;
    }
    let d = digits(parts[1], 2)? as u32;
    let m = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|x| *x == parts[2])? as u32
        + 1;
    let y = digits(parts[3], 4)? as i64;
    let hms: Vec<&str> = parts[4].split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let (h, mi, sec) = (digits(hms[0], 2)?, digits(hms[1], 2)?, digits(hms[2], 2)?);
    if h > 23 || mi > 59 || sec > 60 || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    let days = u64::try_from(days_from_civil(y, m, d)).ok()?;
    days.checked_mul(86_400)?
        .checked_add(h * 3600)?
        .checked_add(mi * 60)?
        .checked_add(sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_445_412_480), "2015-10-21T07:28:00Z");
        assert_eq!(rfc3339(1_795_564_800), "2026-11-25T00:00:00Z");
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480)
        );
        assert_eq!(parse_http_date("21 Oct 2015"), None);
        assert_eq!(
            parse_http_date("Thu, 29 Feb 2024 00:00:00 GMT"),
            Some(1_709_164_800)
        );
        for bad in [
            "Mon, 31 Feb 2026 00:00:00 GMT",
            "Sun, 29 Feb 2025 00:00:00 GMT",
            "Mon, 31 Apr 2026 00:00:00 GMT",
            "Mon, 00 Jan 2026 00:00:00 GMT",
            "Mon, 01 Jan 20260 00:00:00 GMT",
            "Mon, 01 Jan 26 00:00:00 GMT",
            "Mon, 01 Jan 1969 00:00:00 GMT",
            "Mon, 01 Jan 2026 24:00:00 GMT",
            "Mon, 01 Jan 2026 23:60:00 GMT",
            "Mon, 1 Jan 2026 00:00:00 GMT",
            "Mon, 01 Jan +026 00:00:00 GMT",
            "Mon, 01 Jan 2026 00:00:00 UTC",
            "Xyz, 01 Jan 2026 00:00:00 GMT",
            "Mon, 01 Jan 2026 0:00:00 GMT",
            "Mon,  01 Jan 2026 00:00:00 GMT",
        ] {
            assert_eq!(parse_http_date(bad), None, "{bad}");
        }
        assert_eq!(parse_http_date("Wed, 21 Foo 2015 07:28:00 GMT"), None);
        for t in [951_782_400u64, 1_709_164_800, 4_102_444_799] {
            let s = rfc3339(t);
            let (date, time) = s.trim_end_matches('Z').split_once('T').unwrap();
            let ymd: Vec<&str> = date.split('-').collect();
            let months = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let http = format!(
                "Mon, {} {} {} {time} GMT",
                ymd[2],
                months[ymd[1].parse::<usize>().unwrap() - 1],
                ymd[0]
            );
            assert_eq!(parse_http_date(&http), Some(t), "{http}");
        }
    }

    /// Deterministic mutations of a valid date: never a panic, and anything
    /// accepted formats back to a real date.
    #[test]
    fn mutated_dates_never_panic() {
        let base = b"Wed, 21 Oct 2015 07:28:00 GMT".to_vec();
        let alphabet = b"0123456789: ,GMTWedOctFebX+-9";
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let mut s = base.clone();
            for _ in 0..(next() % 4 + 1) {
                let i = (next() as usize) % s.len();
                match next() % 3 {
                    0 => s[i] = alphabet[(next() as usize) % alphabet.len()],
                    1 => {
                        s.remove(i);
                    }
                    _ => s.insert(i, alphabet[(next() as usize) % alphabet.len()]),
                }
                if s.is_empty() {
                    s = base.clone();
                }
            }
            if let Ok(text) = std::str::from_utf8(&s)
                && let Some(t) = parse_http_date(text)
            {
                assert!(rfc3339(t).len() == 20, "{text}");
            }
        }
    }
}
