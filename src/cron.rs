//! Five-field cron with DST handling.
//!
//! Fires are found by walking UTC minutes and reading each one's wall-clock
//! time in the automation's zone. A fixed-time entry (minute and hour fields
//! both not starting with `*`) fires once on the night the clocks go back (the
//! repeated wall-clock hour is skipped) and, on the night they go forward, a
//! time that never happens fires at the first minute after the jump. Entries
//! with a wildcard minute or hour follow real minutes, as Vixie cron does.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone)]
pub struct Cron {
    minute: u64,
    hour: u32,
    dom: u32,
    month: u16,
    dow: u8,
    dom_star: bool,
    dow_star: bool,
    fixed: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn parse_value(s: &str, names: &[&str], offset: u32) -> Result<u32> {
    let lower = s.to_ascii_lowercase();
    if let Some(i) = names.iter().position(|n| *n == lower) {
        return Ok(i as u32 + offset);
    }
    s.parse::<u32>()
        .with_context(|| format!("`{s}` is not a number"))
}

/// Parses one field into a bitmask over `min..=max`.
fn parse_field(field: &str, min: u32, max: u32, names: &[&str], name_offset: u32) -> Result<u64> {
    if field.is_empty() {
        bail!("empty field");
    }
    let mut mask = 0u64;
    for item in field.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s.parse().with_context(|| format!("bad step `{s}`"))?;
                if step == 0 {
                    bail!("step 0 in `{item}`");
                }
                (r, step)
            }
            None => (item, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (parse_value(a, names, name_offset)?, parse_value(b, names, name_offset)?)
        } else {
            let v = parse_value(range, names, name_offset)?;
            // `5/15` means 5 through the end, every 15.
            if item.contains('/') { (v, max) } else { (v, v) }
        };
        if lo < min || hi > max || lo > hi {
            bail!("`{item}` is outside {min}-{max}");
        }
        let mut v = lo;
        while v <= hi {
            mask |= 1 << v;
            v += step;
        }
    }
    Ok(mask)
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron> {
        let expanded = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            other => other,
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        if fields.len() != 5 {
            bail!("cron needs 5 fields (minute hour day month weekday), got {}", fields.len());
        }
        let minute = parse_field(fields[0], 0, 59, &[], 0).context("minute")?;
        let hour = parse_field(fields[1], 0, 23, &[], 0).context("hour")? as u32;
        let dom = parse_field(fields[2], 1, 31, &[], 0).context("day of month")? as u32;
        let month = parse_field(fields[3], 1, 12, &MONTHS, 1).context("month")? as u16;
        let mut dow = parse_field(fields[4], 0, 7, &DAYS, 0).context("weekday")?;
        if dow & (1 << 7) != 0 {
            dow = (dow | 1) & !(1 << 7); // 7 is Sunday too
        }
        Ok(Cron {
            minute,
            hour,
            dom,
            month,
            dow: dow as u8,
            dom_star: fields[2].starts_with('*'),
            dow_star: fields[4].starts_with('*'),
            fixed: !fields[0].starts_with('*') && !fields[1].starts_with('*'),
        })
    }

    fn day_matches(&self, t: &NaiveDateTime) -> bool {
        if self.month & (1 << t.month()) == 0 {
            return false;
        }
        let dom = self.dom & (1 << t.day()) != 0;
        let dow = self.dow & (1 << t.weekday().num_days_from_sunday()) != 0;
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    pub fn matches(&self, t: &NaiveDateTime) -> bool {
        self.minute & (1 << t.minute()) != 0 && self.hour & (1 << t.hour()) != 0 && self.day_matches(t)
    }

    /// Scheduled fires in `(after, until]`, as UTC minutes, oldest first, at most `limit`.
    pub fn fires_between(&self, tz: &Tz, after: DateTime<Utc>, until: DateTime<Utc>, limit: usize) -> Vec<DateTime<Utc>> {
        let mut out = Vec::new();
        let mut m = floor_minute(after) + Duration::minutes(1);
        let until = floor_minute(until);
        while m <= until && out.len() < limit {
            let local = m.with_timezone(tz).naive_local();
            let prev = (m - Duration::minutes(1)).with_timezone(tz).naive_local();

            if !self.day_matches(&local) && !self.day_matches(&prev) {
                // Jump to the next local midnight.
                let next_day = local.date().succ_opt().map(|d| d.and_hms_opt(0, 0, 0).unwrap());
                let jump = next_day.and_then(|nd| match tz.from_local_datetime(&nd) {
                    LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => Some(t.with_timezone(&Utc)),
                    LocalResult::None => None,
                });
                match jump {
                    Some(j) if j > m => {
                        m = j;
                        continue;
                    }
                    _ => {}
                }
            }

            let mut fire = false;
            if self.matches(&local) {
                // Clocks went back: this wall time already happened an hour ago.
                let repeat = self.fixed
                    && matches!(tz.from_local_datetime(&local), LocalResult::Ambiguous(_, later) if later.with_timezone(&Utc) == m);
                fire = !repeat;
            }
            if !fire && self.fixed && local - prev > Duration::minutes(1) {
                // Clocks went forward: wall times between prev and local never happened.
                let mut w = prev + Duration::minutes(1);
                while w < local {
                    if self.matches(&w) {
                        fire = true;
                        break;
                    }
                    w += Duration::minutes(1);
                }
            }
            if fire {
                out.push(m);
            }
            m += Duration::minutes(1);
        }
        out
    }

    pub fn next_after(&self, tz: &Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.fires_between(tz, after, after + Duration::days(366 * 4 + 1), 1).into_iter().next()
    }
}

pub fn floor_minute(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_second(0).unwrap().with_nanosecond(0).unwrap()
}

/// The system zone, from `/etc/localtime`, else `$TZ`, else UTC.
pub fn system_tz() -> Tz {
    if let Ok(tz) = std::env::var("TZ") {
        if let Ok(z) = tz.trim_start_matches(':').parse::<Tz>() {
            return z;
        }
    }
    if let Ok(link) = std::fs::read_link("/etc/localtime") {
        let s = link.to_string_lossy();
        if let Some(idx) = s.find("zoneinfo/") {
            if let Ok(z) = s[idx + "zoneinfo/".len()..].parse::<Tz>() {
                return z;
            }
        }
    }
    Tz::UTC
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn local_strings(c: &Cron, tz: &Tz, from: &str, to: &str) -> Vec<String> {
        c.fires_between(tz, utc(from), utc(to), 1000)
            .into_iter()
            .map(|t| t.with_timezone(tz).format("%Y-%m-%d %H:%M %Z").to_string())
            .collect()
    }

    #[test]
    fn parses_ranges_lists_steps_names() {
        let c = Cron::parse("30 8-15 * * 1-5").unwrap();
        let t = NaiveDateTime::parse_from_str("2026-11-03 08:30", "%Y-%m-%d %H:%M").unwrap();
        assert!(c.matches(&t));
        let sat = NaiveDateTime::parse_from_str("2026-11-07 08:30", "%Y-%m-%d %H:%M").unwrap();
        assert!(!c.matches(&sat));
        assert!(Cron::parse("*/2 * * * *").is_ok());
        assert!(Cron::parse("0 9 * jan-mar mon,fri").is_ok());
        assert!(Cron::parse("0 9 * * 7").is_ok());
        assert!(Cron::parse("61 * * * *").is_err());
        assert!(Cron::parse("* * * *").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
    }

    #[test]
    fn dom_or_dow_when_both_restricted() {
        let c = Cron::parse("0 0 1 * 1").unwrap();
        let first = NaiveDateTime::parse_from_str("2026-10-01 00:00", "%Y-%m-%d %H:%M").unwrap(); // Thu
        let monday = NaiveDateTime::parse_from_str("2026-10-05 00:00", "%Y-%m-%d %H:%M").unwrap();
        let tue = NaiveDateTime::parse_from_str("2026-10-06 00:00", "%Y-%m-%d %H:%M").unwrap();
        assert!(c.matches(&first) && c.matches(&monday) && !c.matches(&tue));
    }

    #[test]
    fn weekday_hour_range() {
        let tz: Tz = "America/Toronto".parse().unwrap();
        let c = Cron::parse("30 7,13 * * 1-5").unwrap();
        // Fri 2026-10-30 through Mon 2026-11-02 (Toronto)
        let got = local_strings(&c, &tz, "2026-10-30T12:00:00Z", "2026-11-02T19:00:00Z");
        assert_eq!(got, vec!["2026-10-30 13:30 EDT", "2026-11-02 07:30 EST", "2026-11-02 13:30 EST"]);
    }

    #[test]
    fn fall_back_fixed_time_fires_once() {
        let tz: Tz = "America/Toronto".parse().unwrap();
        // 2026-11-01: 02:00 EDT -> 01:00 EST
        let c = Cron::parse("30 1 * * *").unwrap();
        let got = local_strings(&c, &tz, "2026-11-01T00:00:00Z", "2026-11-01T12:00:00Z");
        assert_eq!(got, vec!["2026-11-01 01:30 EDT"]);
    }

    #[test]
    fn fall_back_wildcard_hour_fires_each_real_hour() {
        let tz: Tz = "America/Toronto".parse().unwrap();
        let c = Cron::parse("30 * * * *").unwrap();
        let got = local_strings(&c, &tz, "2026-11-01T04:00:00Z", "2026-11-01T07:00:00Z");
        assert_eq!(got, vec!["2026-11-01 00:30 EDT", "2026-11-01 01:30 EDT", "2026-11-01 01:30 EST"]);
    }

    #[test]
    fn spring_forward_fixed_time_not_lost() {
        let tz: Tz = "America/Toronto".parse().unwrap();
        // 2026-03-08: 02:00 EST -> 03:00 EDT
        let c = Cron::parse("30 2 * * *").unwrap();
        let got = local_strings(&c, &tz, "2026-03-08T05:00:00Z", "2026-03-08T12:00:00Z");
        assert_eq!(got, vec!["2026-03-08 03:00 EDT"]);
        // and a normal day still fires at 02:30
        let got = local_strings(&c, &tz, "2026-03-09T05:00:00Z", "2026-03-09T12:00:00Z");
        assert_eq!(got, vec!["2026-03-09 02:30 EDT"]);
    }

    #[test]
    fn next_after_skips_days() {
        let tz: Tz = "America/Toronto".parse().unwrap();
        let c = Cron::parse("0 9 29 2 *").unwrap();
        let n = c.next_after(&tz, utc("2026-11-03T00:00:00Z")).unwrap();
        assert_eq!(n.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string(), "2028-02-29 09:00");
    }
}
