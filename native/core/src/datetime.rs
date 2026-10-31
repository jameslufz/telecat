//! Calendar arithmetic for the Date Picker (Set Value ▸ Date Picker…), without time zones: the
//! picked value is plain text the server reads in its session's zone, like a typed value.
use crate::columns::ValueKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}
impl Stamp {
    pub fn date(year: i32, month: u32, day: u32) -> Self {
        Self {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }
    /// The same moment with the day moved into `year`/`month`, clamped to that month's length.
    pub fn with_month(self, year: i32, month: u32) -> Self {
        Self {
            year,
            month,
            day: self.day.min(days_in_month(year, month)),
            ..self
        }
    }
    /// Months away from this one (`delta` may be negative), the day clamped as needed.
    pub fn add_months(self, delta: i32) -> Self {
        let index = self.year * 12 + self.month as i32 - 1 + delta;
        self.with_month(index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
    }
}
pub fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}
/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
/// The civil date of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}
/// 0 = Sunday … 6 = Saturday.
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    (days_from_civil(year, month, day) + 4).rem_euclid(7) as u32
}
/// The month as the picker lays it out: six weeks starting on Sunday, `None` outside it.
pub fn month_grid(year: i32, month: u32) -> Vec<Option<u32>> {
    let lead = weekday(year, month, 1) as usize;
    let days = days_in_month(year, month) as usize;
    (0..42)
        .map(|i| (i >= lead && i < lead + days).then(|| (i - lead + 1) as u32))
        .collect()
}
fn number(s: &str, len: usize) -> Option<u32> {
    (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}
/// Reads `YYYY-MM-DD`, `YYYY-MM-DD HH:MM:SS` (a `T` separator, fraction and zone are ignored)
/// or `HH:MM:SS`. `None` for anything else or an impossible date.
pub fn parse(text: &str) -> Option<Stamp> {
    let text = text.trim();
    let time = |t: &str| -> Option<(u32, u32, u32)> {
        let t = t.split(['.', '+', 'Z', 'z']).next()?;
        let t = t.split('-').next()?;
        let mut parts = t.split(':');
        let h = number(parts.next()?, 2)?;
        let m = number(parts.next()?, 2)?;
        let s = parts.next().map_or(Some(0), |s| number(s, 2))?;
        (h < 24 && m < 60 && s < 60).then_some((h, m, s))
    };
    if text.len() >= 5 && text.as_bytes()[2] == b':' {
        let (hour, minute, second) = time(text)?;
        return Some(Stamp {
            hour,
            minute,
            second,
            ..Stamp::date(1970, 1, 1)
        });
    }
    let (date, rest) = match text.find([' ', 'T']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year = number(parts.next()?, 4)? as i32;
    let month = number(parts.next()?, 2)?;
    let day = number(parts.next()?, 2)?;
    if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    if day > days_in_month(year, month) {
        return None;
    }
    let (hour, minute, second) = match rest {
        Some(t) => time(t)?,
        None => (0, 0, 0),
    };
    Some(Stamp {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}
/// The text a column of `kind` stores for `stamp`.
pub fn format(stamp: Stamp, kind: ValueKind) -> String {
    let date = format!("{:04}-{:02}-{:02}", stamp.year, stamp.month, stamp.day);
    let time = format!("{:02}:{:02}:{:02}", stamp.hour, stamp.minute, stamp.second);
    match kind {
        ValueKind::Date => date,
        ValueKind::Time => time,
        _ => format!("{date} {time}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_arithmetic() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(weekday(2026, 10, 31), 6);
        assert_eq!(weekday(1970, 1, 1), 4);
        for days in [-800_000, -1, 0, 1, 19_000, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        let grid = month_grid(2026, 2);
        assert_eq!(grid.len(), 42);
        assert_eq!(grid[0], Some(1));
        assert_eq!(grid.iter().flatten().count(), 28);
        let oct = month_grid(2026, 10);
        assert_eq!(oct[4], Some(1));
        assert_eq!(oct[3], None);
    }
    #[test]
    fn months_move_and_clamp_the_day() {
        let jan31 = Stamp::date(2024, 1, 31);
        assert_eq!(jan31.add_months(1), Stamp::date(2024, 2, 29));
        assert_eq!(jan31.add_months(-1), Stamp::date(2023, 12, 31));
        assert_eq!(jan31.add_months(-13), Stamp::date(2022, 12, 31));
        assert_eq!(
            Stamp::date(2024, 12, 5).add_months(1),
            Stamp::date(2025, 1, 5)
        );
    }
    #[test]
    fn parse_and_format_round_trip() {
        let s = parse("2026-10-30 07:36:48.822006+00").unwrap();
        assert_eq!(
            (s.year, s.month, s.day, s.hour, s.minute, s.second),
            (2026, 10, 30, 7, 36, 48)
        );
        assert_eq!(format(s, ValueKind::DateTime), "2026-10-30 07:36:48");
        assert_eq!(format(s, ValueKind::Date), "2026-10-30");
        assert_eq!(format(s, ValueKind::Time), "07:36:48");
        assert_eq!(parse("2026-10-30T01:02:03Z").unwrap().hour, 1);
        assert_eq!(parse("13:14:15.5").unwrap().minute, 14);
        assert_eq!(parse("10:20:30-07").unwrap().second, 30);
        for bad in [
            "",
            "2026-02-30",
            "2026-13-01",
            "26-01-01",
            "25:00:00",
            "2026-01-01 1:2:3",
            "abc",
            "2026-01-01-01",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
