//! `commit at <time>` (docs/CLI-DESIGN.md): the time a person types, as the broker takes it — an
//! RFC 3339 time in UTC, in the future, at most `MAX_DAYS` away. The forms, as on a network device:
//! `HH:MM` (today, or tomorrow if that has passed), `"YYYY-MM-DD HH:MM"` (local time), or RFC 3339
//! with its offset (`2026-11-08T10:00:00+01:00`, `…Z`).

use anyhow::{bail, Context, Result};

/// The furthest ahead (the broker's limit too: broker/src/at.rs).
pub const MAX_DAYS: i64 = 30;
/// How late the broker still runs one (broker/src/at.rs `LATE_MINUTES`): after that it refuses it.
pub const LATE_SECONDS: i64 = 15 * 60;

/// How a local wall time ("YYYY-MM-DD HH:MM") is a moment (Unix seconds), on this machine: its time
/// zone, daylight saving included. None: no such local time (a date that does not exist, the hour a
/// clock skips).
pub type Local<'a> = &'a dyn Fn(&str) -> Option<i64>;

/// Whether `w` has one of the forms (not yet whether it is in the future: that is when it is used).
pub fn form(w: &str) -> Result<()> {
    match shape(w) {
        Some(_) => Ok(()),
        None => bail!("'{w}' is not a time: HH:MM, \"YYYY-MM-DD HH:MM\", or RFC 3339 (2026-11-08T10:00:00+01:00)"),
    }
}

enum Shape {
    /// Hours and minutes, today or tomorrow.
    Clock(u32, u32),
    /// A local date and time.
    Local(String),
    /// A moment: Unix seconds.
    Moment(i64),
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// `HH:MM` (or `H:MM`).
fn clock(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let h = digits(h, 2).or_else(|| digits(h, 1))?;
    let m = digits(m, 2)?;
    (h < 24 && m < 60).then_some((h, m))
}

/// `YYYY-MM-DD`, a date that exists.
fn date(s: &str) -> Option<(u32, u32, u32)> {
    let mut p = s.split('-');
    let (y, m, d) = (digits(p.next()?, 4)?, digits(p.next()?, 2)?, digits(p.next()?, 2)?);
    if p.next().is_some() || !(1..=12).contains(&m) || d == 0 {
        return None;
    }
    // It exists if it is itself again after a round through the calendar (2026-02-30 is not).
    let t = crate::history::parse_ts(&format!("{s}T00:00:00Z"))?;
    (t >= 0 && crate::pkg::ymd(t as u64) == s).then_some((y, m, d))
}

fn shape(w: &str) -> Option<Shape> {
    let w = w.trim();
    if let Some((h, m)) = clock(w) {
        return Some(Shape::Clock(h, m));
    }
    let (d, rest) = w.split_once([' ', 'T', 't'])?;
    date(d)?;
    // A local date and time, with no offset.
    if let Some((h, m)) = clock(rest) {
        return Some(Shape::Local(format!("{d} {h:02}:{m:02}")));
    }
    // RFC 3339: HH:MM:SS, perhaps a fraction (dropped), then Z or ±HH:MM.
    let (time, offset) = match rest.strip_suffix(['Z', 'z']) {
        Some(t) => (t, 0),
        None => {
            let at = rest.rfind(['+', '-'])?;
            let (t, z) = rest.split_at(at);
            let (oh, om) = z[1..].split_once(':')?;
            let (oh, om) = (digits(oh, 2)?, digits(om, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            (t, (if z.starts_with('-') { -1 } else { 1 }) * i64::from(oh * 3600 + om * 60))
        }
    };
    let time = time.split_once('.').map_or(time, |(t, frac)| if !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()) { t } else { "" });
    let (hm, s) = time.rsplit_once(':')?;
    let (h, m) = clock(hm).filter(|_| hm.len() == 5)?;
    if digits(s, 2)? > 59 {
        return None;
    }
    let t = crate::history::parse_ts(&format!("{d}T{h:02}:{m:02}:{s}Z"))?;
    Some(Shape::Moment(t - offset))
}

/// `w` as a moment (Unix seconds): `now` is the time, `today` the local date (`YYYY-MM-DD`), `local`
/// the machine's zone. Refused: in the past, or more than `MAX_DAYS` away.
pub fn when(w: &str, now: i64, today: &str, local: Local) -> Result<i64> {
    let unknown = |s: &str| anyhow::anyhow!("{s} is not a time here (a date that does not exist, or the hour a clock change skips)");
    let t = match shape(w).with_context(|| form(w).unwrap_err())? {
        Shape::Moment(t) => t,
        Shape::Local(s) => local(&s).ok_or_else(|| unknown(&s))?,
        Shape::Clock(h, m) => {
            let at = |day: &str| {
                let s = format!("{day} {h:02}:{m:02}");
                local(&s).ok_or_else(|| unknown(&s))
            };
            let t = at(today)?;
            if t > now {
                t
            } else {
                let next = crate::history::parse_ts(&format!("{today}T00:00:00Z")).context("today's date")? + 86_400;
                at(&crate::pkg::ymd(next as u64))?
            }
        }
    };
    if t <= now {
        bail!("{w} has passed: `commit at` takes a time to come");
    }
    if t > now + MAX_DAYS * 86_400 {
        bail!("{w} is more than {MAX_DAYS} days away");
    }
    Ok(t)
}

/// Unix seconds as the broker takes a time: `2026-11-08T09:00:00Z`.
pub fn rfc3339(t: i64) -> String {
    let t = t.max(0);
    format!("{}T{:02}:{:02}:{:02}Z", crate::pkg::ymd(t as u64), t / 3600 % 24, t / 60 % 60, t % 60)
}

/// This machine's zone, as `date` knows it (no time-zone database is read here).
pub fn system_local(s: &str) -> Option<i64> {
    let out = std::process::Command::new("date").env("LC_ALL", "C").args(["-d", s, "+%s"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// This machine's offset from UTC at moment `t`, in seconds (daylight saving may differ from now's).
pub fn system_local_offset(t: i64) -> Option<i64> {
    let out = std::process::Command::new("date").env("LC_ALL", "C").args(["-d", &format!("@{t}"), "+%z"]).output().ok()?;
    crate::history::parse_offset(String::from_utf8_lossy(&out.stdout).trim())
}

/// Today's local date, as `date` says.
pub fn system_today() -> Result<String> {
    let out = std::process::Command::new("date").env("LC_ALL", "C").arg("+%F").output().context("running date")?;
    let d = String::from_utf8_lossy(&out.stdout).trim().to_string();
    date(&d).with_context(|| format!("`date +%F` said {d:?}"))?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One hour east of UTC, all year: "YYYY-MM-DD HH:MM" local is an hour before in UTC.
    fn plus_one(s: &str) -> Option<i64> {
        crate::history::parse_ts(&format!("{}:00Z", s.replace(' ', "T"))).map(|t| t - 3600)
    }
    /// 2026-11-07 12:30 local (11:30 UTC).
    fn now() -> i64 {
        crate::history::parse_ts("2026-11-07T11:30:00Z").unwrap()
    }

    #[test]
    fn the_clock_is_today_or_tomorrow() {
        let at = |w: &str| when(w, now(), "2026-11-07", &plus_one).map(rfc3339);
        assert_eq!(at("14:00").unwrap(), "2026-11-07T13:00:00Z", "later today");
        assert_eq!(at("9:05").unwrap(), "2026-11-08T08:05:00Z", "passed today: tomorrow");
        assert_eq!(at("12:30").unwrap(), "2026-11-08T11:30:00Z", "now itself has passed");
        // Across a month's and a year's end.
        assert_eq!(when("00:10", crate::history::parse_ts("2026-12-31T22:00:00Z").unwrap(), "2026-12-31", &plus_one).map(rfc3339).unwrap(), "2026-12-31T23:10:00Z");
        assert_eq!(when("08:00", crate::history::parse_ts("2026-12-31T22:00:00Z").unwrap(), "2026-12-31", &plus_one).map(rfc3339).unwrap(), "2027-01-01T07:00:00Z");
    }

    #[test]
    fn a_date_is_local_and_rfc_3339_carries_its_offset() {
        let at = |w: &str| when(w, now(), "2026-11-07", &plus_one).map(rfc3339);
        assert_eq!(at("2026-11-08 10:00").unwrap(), "2026-11-08T09:00:00Z");
        assert_eq!(at("2026-11-08T10:00").unwrap(), "2026-11-08T09:00:00Z");
        assert_eq!(at("2026-11-08T10:00:00Z").unwrap(), "2026-11-08T10:00:00Z");
        assert_eq!(at("2026-11-08T10:00:00+02:00").unwrap(), "2026-11-08T08:00:00Z");
        assert_eq!(at("2026-11-08T10:00:00.5-05:30").unwrap(), "2026-11-08T15:30:00Z");
    }

    #[test]
    fn the_past_and_the_far_future_are_refused() {
        let at = |w: &str| format!("{:#}", when(w, now(), "2026-11-07", &plus_one).unwrap_err());
        assert!(at("2026-11-07 12:00").contains("has passed"));
        assert!(at("2026-11-07T11:30:00Z").contains("has passed"));
        assert!(at("2026-12-08 10:00").contains("more than 30 days"));
        assert!(when("2026-12-07 12:29", now(), "2026-11-07", &plus_one).is_ok(), "just within");
        // A local time the zone does not have.
        assert!(format!("{:#}", when("2026-11-08 02:30", now(), "2026-11-07", &|_| None).unwrap_err()).contains("not a time here"));
    }

    #[test]
    fn only_the_forms_are_times() {
        for good in ["10:00", "0:00", "23:59", "2026-11-08 10:00", "2026-11-08T10:00:00Z", "2026-11-08t10:00:00z", "2028-02-29 10:00"] {
            assert!(form(good).is_ok(), "{good}");
        }
        for bad in ["", "10", "24:00", "10:60", "10:5", "1000", "tomorrow", "now", "2026-11-08", "2026-13-01 10:00", "2026-02-30 10:00", "2027-02-29 10:00", "2026-11-08 10:00:00", "2026-11-08T10:00:00+1:00", "2026-11-08T10:00:00+24:00", "2026-11-08T10:00:61Z", "2026-11-08T10:00:00.Z", "26-11-08 10:00", "+10:00"] {
            assert!(form(bad).is_err(), "{bad}");
        }
    }
}
