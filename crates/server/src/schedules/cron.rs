//! A standard five-field cron expression evaluated in an IANA time zone.
//!
//! Fields: `minute hour day-of-month month day-of-week`, with `*`, lists
//! (`1,15`), ranges (`1-5`), steps (`*/30`, `0-30/10`, `5/15`), month names
//! `jan`..`dec` and weekday names `sun`..`sat` (case-insensitive). Weekday
//! `0` and `7` are both Sunday. The `@hourly`, `@daily`/`@midnight`,
//! `@weekly`, `@monthly` and `@yearly`/`@annually` shorthands are accepted.
//! There is no seconds field.
//!
//! Day matching follows Vixie cron: when either the day-of-month or the
//! day-of-week field starts with `*`, a day must match both; when both are
//! restricted, a day matches if either does (`0 9 1 * mon` fires on the 1st
//! *and* on every Monday).
//!
//! # Time zones and daylight saving time
//!
//! The fields are local wall-clock times in the task's zone. Each matching
//! local time fires at most once:
//!
//! - A local time that does not exist because clocks jump forward (for
//!   example 02:30 on a spring-forward night in `America/New_York`) fires at
//!   the first valid instant after the gap (03:00 local), so a daily task is
//!   never silently skipped. Several skipped times that land on the same
//!   instant fire once.
//! - A local time that occurs twice because clocks fall back fires only at
//!   its first occurrence (the earlier, daylight-time instant). The repeated
//!   hour does not fire again.
//!
//! Zones without daylight saving time, such as `Asia/Manila`, are not
//! affected.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// How far ahead the next firing is searched. Long enough for
/// `0 0 29 2 *` combined with a weekday, which can be decades apart in
/// theory; such an expression is rejected as never firing.
const SEARCH_DAYS: i64 = 366 * 8 + 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronExpr {
    minutes: u64,
    hours: u32,
    /// Bits 1..=31.
    days: u32,
    /// Bits 1..=12.
    months: u16,
    /// Bits 0..=6, Sunday = 0.
    weekdays: u8,
    days_star: bool,
    weekdays_star: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl CronExpr {
    pub fn parse(expression: &str) -> Result<Self, String> {
        let expression = expression.trim();
        let expanded = match expression.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_owned(),
            "@daily" | "@midnight" => "0 0 * * *".to_owned(),
            "@weekly" => "0 0 * * 0".to_owned(),
            "@monthly" => "0 0 1 * *".to_owned(),
            "@yearly" | "@annually" => "0 0 1 1 *".to_owned(),
            s if s.starts_with('@') => {
                return Err(format!("unknown cron shorthand `{expression}`"));
            }
            _ => expression.to_owned(),
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(
                "cron expression must have 5 fields: minute hour day-of-month month day-of-week \
                 (no seconds field)"
                    .to_owned(),
            );
        }
        let minutes = field(fields[0], "minute", 0, 59, &[])?;
        let hours = field(fields[1], "hour", 0, 23, &[])?;
        let days = field(fields[2], "day-of-month", 1, 31, &[])?;
        let months = field(fields[3], "month", 1, 12, &MONTHS)?;
        let mut weekdays = field(fields[4], "day-of-week", 0, 7, &WEEKDAYS)?;
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays | 1) & !(1 << 7);
        }
        Ok(Self {
            minutes,
            hours: hours as u32,
            days: days as u32,
            months: months as u16,
            weekdays: weekdays as u8,
            days_star: fields[2].starts_with('*'),
            weekdays_star: fields[4].starts_with('*'),
        })
    }

    fn day_matches(&self, date: NaiveDate) -> bool {
        if self.months & (1 << date.month()) == 0 {
            return false;
        }
        let dom = self.days & (1 << date.day()) != 0;
        let dow = self.weekdays & (1 << date.weekday().num_days_from_sunday()) != 0;
        if self.days_star || self.weekdays_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// Minutes after local midnight at which this expression fires, ascending.
    pub fn minutes_of_day(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for h in 0..24u32 {
            if self.hours & (1 << h) == 0 {
                continue;
            }
            for m in 0..60u32 {
                if self.minutes & (1 << m) != 0 {
                    out.push(h * 60 + m);
                }
            }
        }
        out
    }

    /// The smallest gap, in minutes, between two local firing times, as if
    /// every day matched. A conservative bound for frequency limits.
    pub fn min_spacing_minutes(&self) -> u32 {
        let times = self.minutes_of_day();
        if times.len() < 2 {
            return 24 * 60;
        }
        let wrap = times[0] + 24 * 60 - times[times.len() - 1];
        times
            .windows(2)
            .map(|w| w[1] - w[0])
            .chain(std::iter::once(wrap))
            .min()
            .unwrap_or(24 * 60)
    }

    /// The first firing strictly after `after`, or `None` when there is
    /// none within the search horizon.
    pub fn next_after(&self, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        // Start a day early: with a daylight-time offset, a local time on the
        // previous local date can still be after `after`.
        let start = after.with_timezone(&tz).date_naive() - Duration::days(1);
        let times = self.minutes_of_day();
        for offset in 0..SEARCH_DAYS {
            let date = start + Duration::days(offset);
            if !self.day_matches(date) {
                continue;
            }
            for &t in &times {
                let naive = date.and_hms_opt(t / 60, t % 60, 0)?;
                if let Some(instant) = resolve(tz, naive)
                    && instant > after
                {
                    return Some(instant);
                }
            }
        }
        None
    }
}

/// The instant of a local wall-clock time: the earlier one when it occurs
/// twice, the end of the gap when it does not occur at all.
fn resolve(tz: Tz, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        LocalResult::Ambiguous(a, b) => Some(a.min(b).with_timezone(&Utc)),
        LocalResult::None => {
            // Gaps are at most a few hours; walk forward minute by minute.
            let mut probe = naive;
            for _ in 0..(24 * 60) {
                probe += Duration::minutes(1);
                match tz.from_local_datetime(&probe) {
                    LocalResult::Single(t) => return Some(t.with_timezone(&Utc)),
                    LocalResult::Ambiguous(a, b) => return Some(a.min(b).with_timezone(&Utc)),
                    LocalResult::None => {}
                }
            }
            None
        }
    }
}

fn value(text: &str, name: &str, min: u32, max: u32, names: &[&str]) -> Result<u32, String> {
    let lower = text.to_ascii_lowercase();
    if let Some(i) = names.iter().position(|n| *n == lower) {
        // Month names start at 1, weekday names at 0.
        return Ok(i as u32 + if names.len() == MONTHS.len() { 1 } else { 0 });
    }
    let v: u32 = text
        .parse()
        .map_err(|_| format!("invalid {name} value `{text}`"))?;
    if v < min || v > max {
        return Err(format!("{name} value {v} is outside {min}-{max}"));
    }
    Ok(v)
}

fn field(text: &str, name: &str, min: u32, max: u32, names: &[&str]) -> Result<u64, String> {
    let mut bits = 0u64;
    for item in text.split(',') {
        if item.is_empty() {
            return Err(format!("empty item in the {name} field"));
        }
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| format!("invalid step `{s}` in the {name} field"))?;
                if step == 0 || step > max {
                    return Err(format!("step {step} is out of range in the {name} field"));
                }
                (r, Some(step))
            }
            None => (item, None),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (
                value(a, name, min, max, names)?,
                value(b, name, min, max, names)?,
            );
            if a > b {
                return Err(format!("range {a}-{b} is reversed in the {name} field"));
            }
            (a, b)
        } else {
            let a = value(range, name, min, max, names)?;
            // `5/15` means from 5 to the end, every 15.
            (a, if step.is_some() { max } else { a })
        };
        let step = step.unwrap_or(1);
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn names_and_sunday_seven() {
        let a = CronExpr::parse("0 9 * JAN-mar sun").unwrap();
        let b = CronExpr::parse("0 9 * 1-3 7").unwrap();
        let c = CronExpr::parse("0 9 * 1,2,3 0").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a.weekdays, 1);
        assert_eq!(a.months, 0b1110);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "* * * *",
            "0 0 * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
            "1,,2 * * * *",
            "@reboot",
        ] {
            assert!(CronExpr::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn daily_manila() {
        let e = CronExpr::parse("0 9 * * *").unwrap();
        let tz: Tz = "Asia/Manila".parse().unwrap();
        // 08:59 Manila = 00:59Z.
        assert_eq!(
            e.next_after(tz, utc("2026-11-23T00:59:00Z")),
            Some(utc("2026-11-23T01:00:00Z"))
        );
        // Exactly at the firing: strictly after, so tomorrow.
        assert_eq!(
            e.next_after(tz, utc("2026-11-23T01:00:00Z")),
            Some(utc("2026-11-24T01:00:00Z"))
        );
    }

    #[test]
    fn vixie_day_or_rule() {
        let e = CronExpr::parse("0 0 1 * mon").unwrap();
        let tz = Tz::UTC;
        // 2026-12-01 is a Tuesday: the 1st matches. Then Monday the 7th.
        assert_eq!(
            e.next_after(tz, utc("2026-11-30T12:00:00Z")),
            Some(utc("2026-12-01T00:00:00Z"))
        );
        assert_eq!(
            e.next_after(tz, utc("2026-12-01T00:00:00Z")),
            Some(utc("2026-12-07T00:00:00Z"))
        );
        // Starred day-of-month: weekday alone decides.
        let e = CronExpr::parse("0 0 * * mon").unwrap();
        assert_eq!(
            e.next_after(tz, utc("2026-11-30T12:00:00Z")),
            Some(utc("2026-12-07T00:00:00Z"))
        );
    }

    #[test]
    fn never_fires() {
        let e = CronExpr::parse("0 0 31 2 *").unwrap();
        assert_eq!(e.next_after(Tz::UTC, utc("2026-01-01T00:00:00Z")), None);
    }

    #[test]
    fn leap_day() {
        let e = CronExpr::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            e.next_after(Tz::UTC, utc("2026-03-01T00:00:00Z")),
            Some(utc("2028-02-29T00:00:00Z"))
        );
    }

    #[test]
    fn spring_forward_gap_fires_after_gap() {
        // 2027-03-14: New York jumps 02:00 EST -> 03:00 EDT.
        let tz: Tz = "America/New_York".parse().unwrap();
        let e = CronExpr::parse("30 2 * * *").unwrap();
        let first = e.next_after(tz, utc("2027-03-13T12:00:00Z")).unwrap();
        // 03:00 EDT = 07:00Z.
        assert_eq!(first, utc("2027-03-14T07:00:00Z"));
        // Next day is normal again: 02:30 EDT = 06:30Z.
        assert_eq!(e.next_after(tz, first), Some(utc("2027-03-15T06:30:00Z")));
        // Several gap times collapse onto one instant and fire once.
        let e = CronExpr::parse("0,30 2,3 * * *").unwrap();
        let a = e.next_after(tz, utc("2027-03-14T06:00:00Z")).unwrap();
        assert_eq!(a, utc("2027-03-14T07:00:00Z"));
        let b = e.next_after(tz, a).unwrap();
        assert_eq!(b, utc("2027-03-14T07:30:00Z"));
    }

    #[test]
    fn fall_back_fires_once() {
        // 2026-11-01: New York repeats 01:00-02:00 (EDT then EST).
        let tz: Tz = "America/New_York".parse().unwrap();
        let e = CronExpr::parse("30 1 * * *").unwrap();
        let first = e.next_after(tz, utc("2026-10-31T12:00:00Z")).unwrap();
        // 01:30 EDT = 05:30Z; the 01:30 EST repeat (06:30Z) does not fire.
        assert_eq!(first, utc("2026-11-01T05:30:00Z"));
        assert_eq!(e.next_after(tz, first), Some(utc("2026-11-02T06:30:00Z")));
    }

    #[test]
    fn spacing() {
        assert_eq!(
            CronExpr::parse("*/15 * * * *")
                .unwrap()
                .min_spacing_minutes(),
            15
        );
        assert_eq!(
            CronExpr::parse("*/5 * * * *")
                .unwrap()
                .min_spacing_minutes(),
            5
        );
        assert_eq!(
            CronExpr::parse("0 9 * * *").unwrap().min_spacing_minutes(),
            1440
        );
        assert_eq!(
            CronExpr::parse("0 0,23 * * *")
                .unwrap()
                .min_spacing_minutes(),
            60
        );
        assert_eq!(
            CronExpr::parse("50 23 * * *")
                .unwrap()
                .min_spacing_minutes(),
            1440
        );
        assert_eq!(
            CronExpr::parse("55 23,0 * * *")
                .unwrap()
                .min_spacing_minutes(),
            60
        );
        assert_eq!(
            CronExpr::parse("0,10 9 * * *")
                .unwrap()
                .min_spacing_minutes(),
            10
        );
    }

    #[test]
    fn step_from_value() {
        let e = CronExpr::parse("5/20 * * * *").unwrap();
        assert_eq!(e.minutes_of_day()[..3], [5, 25, 45]);
    }
}
