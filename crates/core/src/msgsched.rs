//! Message Builder schedules: the "when" lines of a schedule rule and their next runs, on the real
//! calendar in the rule's own time zone (month lengths, leap years, summer time).
//!
//! A rule's `spec` is `{"tz": "Europe/Oslo", "start": <unix secs>, "lines": [..]}`, each line one of:
//! `{"kind": "once", "date": "2026-11-20", "time": "18:00"}`, `{"kind": "every", "minutes": 30}`,
//! `{"kind": "daily", "time": "09:00"}`, `{"kind": "weekly", "days": [1, 5], "time": "18:00"}`
//! (1 = Monday), `{"kind": "monthly", "days": [1, 31], "time": "09:00"}`, `{"kind": "messages", "n": 5}`.
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};

/// Shortest repeat for "every N minutes or hours" (owner decision Q-D).
pub const MIN_MINUTES: i64 = 15;
/// Longest repeat: 4 weeks.
pub const MAX_MINUTES: i64 = 40_320;
/// Fewest member messages between sends for "every N messages" (owner decision Q-D).
pub const MIN_MESSAGES: i64 = 5;
pub const MAX_MESSAGES: i64 = 10_000;
/// "When" lines per rule.
pub const MAX_LINES: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Once(NaiveDateTime),
    Every(i64),
    Daily(NaiveTime),
    Weekly(Vec<u32>, NaiveTime),
    Monthly(Vec<u32>, NaiveTime),
    Messages(i64),
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub tz: Tz,
    /// Anchor for "every N minutes": runs fall on `start + k * N`.
    pub start: DateTime<Utc>,
    pub lines: Vec<Line>,
}

fn time(v: &Value) -> Result<NaiveTime, String> {
    v.as_str()
        .and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok())
        .ok_or_else(|| "a time is HH:MM, for example 09:00".to_string())
}

fn days(v: &Value, max: u32, what: &str) -> Result<Vec<u32>, String> {
    let mut d: Vec<u32> = v
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            d.as_u64()
                .filter(|d| (1..=u64::from(max)).contains(d))
                .map(|d| d as u32)
        })
        .collect::<Option<_>>()
        .ok_or_else(|| format!("pick {what} from the list"))?;
    d.sort_unstable();
    d.dedup();
    if d.is_empty() {
        return Err(format!("pick at least one {what}"));
    }
    Ok(d)
}

/// Checks a stored or posted spec. `start` fills in a missing anchor (a new rule: now).
pub fn parse(v: &Value, start: DateTime<Utc>) -> Result<Spec, String> {
    let tz: Tz = v["tz"]
        .as_str()
        .and_then(|z| z.parse().ok())
        .ok_or_else(|| "pick a time zone from the list".to_string())?;
    // Years the date box takes: a far year would overflow the time math.
    let years = start.year() - 10..=start.year() + 5;
    let start = v["start"]
        .as_i64()
        .and_then(|s| DateTime::from_timestamp(s, 0))
        .unwrap_or(start);
    let raw = v["lines"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if raw.is_empty() {
        return Err("add at least one time".into());
    }
    if raw.len() > MAX_LINES {
        return Err(format!("a schedule has at most {MAX_LINES} times"));
    }
    let lines = raw
        .iter()
        .map(|l| {
            Ok(match l["kind"].as_str() {
                Some("once") => {
                    let d = l["date"]
                        .as_str()
                        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                        .ok_or("pick a date")?;
                    if !years.contains(&d.year()) {
                        return Err("pick a date within the next 5 years".into());
                    }
                    Line::Once(d.and_time(time(&l["time"])?))
                }
                Some("every") => {
                    let m = l["minutes"].as_i64().unwrap_or(0);
                    if !(MIN_MINUTES..=MAX_MINUTES).contains(&m) {
                        return Err(format!(
                            "a repeat is every {MIN_MINUTES} minutes at the shortest and every 4 weeks at the longest"
                        ));
                    }
                    Line::Every(m)
                }
                Some("daily") => Line::Daily(time(&l["time"])?),
                Some("weekly") => Line::Weekly(days(&l["days"], 7, "weekday")?, time(&l["time"])?),
                Some("monthly") => Line::Monthly(days(&l["days"], 31, "day")?, time(&l["time"])?),
                Some("messages") => {
                    let n = l["n"].as_i64().unwrap_or(0);
                    if !(MIN_MESSAGES..=MAX_MESSAGES).contains(&n) {
                        return Err(format!(
                            "\"every N messages\" is at least {MIN_MESSAGES} and at most {MAX_MESSAGES}"
                        ));
                    }
                    Line::Messages(n)
                }
                _ => return Err("unknown kind of time".to_string()),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Spec { tz, start, lines })
}

/// The spec as stored: cleaned lines, the zone and the anchor.
pub fn to_json(s: &Spec) -> Value {
    let hm = |t: &NaiveTime| t.format("%H:%M").to_string();
    let lines: Vec<Value> = s
        .lines
        .iter()
        .map(|l| match l {
            Line::Once(at) => json!({"kind": "once", "date": at.date().format("%Y-%m-%d").to_string(), "time": hm(&at.time())}),
            Line::Every(m) => json!({"kind": "every", "minutes": m}),
            Line::Daily(t) => json!({"kind": "daily", "time": hm(t)}),
            Line::Weekly(d, t) => json!({"kind": "weekly", "days": d, "time": hm(t)}),
            Line::Monthly(d, t) => json!({"kind": "monthly", "days": d, "time": hm(t)}),
            Line::Messages(n) => json!({"kind": "messages", "n": n}),
        })
        .collect();
    json!({"tz": s.tz.name(), "start": s.start.timestamp(), "lines": lines})
}

/// A local wall time in `tz` as UTC: a time that occurs twice (summer time ends) is the first one;
/// a time that does not exist (summer time starts) is the first valid minute after it.
pub fn resolve(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    for m in 0..=180 {
        let Some(t) = local.checked_add_signed(Duration::minutes(m)) else {
            break;
        };
        if let Some(d) = tz.from_local_datetime(&t).earliest() {
            return d.with_timezone(&Utc);
        }
    }
    // No zone skips three hours; this is only a fallback.
    Utc.from_utc_datetime(&local)
}

/// The first run of one line strictly after `after`; `None` when it never runs again (or it runs
/// on member messages, not on the clock).
pub fn line_next(s: &Spec, line: &Line, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let on_days = |pick: &dyn Fn(NaiveDate) -> bool, t: &NaiveTime| {
        let first = after.with_timezone(&s.tz).date_naive();
        // Four years and a day covers a 29 February, the rarest day a line can pick.
        (0..=1462)
            .filter_map(|n| first.checked_add_signed(Duration::days(n)))
            .filter(|d| pick(*d))
            .map(|d| resolve(s.tz, d.and_time(*t)))
            .find(|r| *r > after)
    };
    match line {
        Line::Once(at) => Some(resolve(s.tz, *at)).filter(|r| *r > after),
        Line::Every(m) => {
            let step = m * 60;
            let since = after.timestamp() - s.start.timestamp();
            let k = if since < 0 { 0 } else { since / step + 1 };
            DateTime::from_timestamp(s.start.timestamp() + k * step, 0)
        }
        Line::Daily(t) => on_days(&|_| true, t),
        Line::Weekly(days, t) => on_days(&|d| days.contains(&d.weekday().number_from_monday()), t),
        // A month without the picked day has no run that month, as in a calendar app (owner decision Q-C).
        Line::Monthly(days, t) => on_days(&|d| days.contains(&d.day()), t),
        Line::Messages(_) => None,
    }
}

/// The next clock run of the whole rule after `after`.
pub fn next(s: &Spec, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    s.lines.iter().filter_map(|l| line_next(s, l, after)).min()
}

/// The next `n` runs of one line, for the list under it.
/// True when two clock runs of the schedule (from any of its lines) come less than
/// [`MIN_MINUTES`] apart within the next 400 runs: the shortest repeat holds for the whole rule.
pub fn too_close(s: &Spec, after: DateTime<Utc>) -> bool {
    let mut t = after;
    let mut last: Option<DateTime<Utc>> = None;
    for _ in 0..400 {
        let Some(n) = next(s, t) else { return false };
        if last.is_some_and(|l| (n - l).num_minutes() < MIN_MINUTES) {
            return true;
        }
        last = Some(n);
        t = n;
    }
    false
}

/// The next clock run of a stored spec from now, as Unix seconds (for the bot, which has no
/// chrono of its own).
pub fn next_of(spec: &Value) -> Option<i64> {
    let now = Utc::now();
    next(&parse(spec, now).ok()?, now).map(|t| t.timestamp())
}

pub fn upcoming(s: &Spec, line: &Line, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
    let mut out = Vec::with_capacity(n);
    let mut at = after;
    while out.len() < n {
        match line_next(s, line, at) {
            Some(t) => {
                out.push(t);
                at = t;
            }
            None => break,
        }
    }
    out
}

/// "Every N messages" counts of a rule (the smallest wins).
pub fn messages(s: &Spec) -> Option<i64> {
    s.lines
        .iter()
        .filter_map(|l| match l {
            Line::Messages(n) => Some(*n),
            _ => None,
        })
        .min()
}

/// "Every N messages" of a stored spec, or None (no such line, or a spec that does not parse).
pub fn messages_of(spec: &Value) -> Option<i64> {
    messages(&parse(spec, Utc::now()).ok()?)
}

/// One line in words, for the Sent log and the rule's card.
pub fn describe(line: &Line) -> String {
    const WD: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let hm = |t: &NaiveTime| t.format("%H:%M").to_string();
    match line {
        Line::Once(at) => format!(
            "once on {} at {}",
            at.date().format("%-d %b %Y"),
            hm(&at.time())
        ),
        Line::Every(m) if m % 60 == 0 => match m / 60 {
            1 => "every hour".into(),
            h => format!("every {h} hours"),
        },
        Line::Every(m) => format!("every {m} minutes"),
        Line::Daily(t) => format!("every day at {}", hm(t)),
        Line::Weekly(d, t) => {
            let names: Vec<&str> = d.iter().map(|d| WD[(*d as usize - 1) % 7]).collect();
            format!("{} at {}", names.join(", "), hm(t))
        }
        Line::Monthly(d, t) => {
            let names: Vec<String> = d.iter().map(u32::to_string).collect();
            format!("day {} of each month at {}", names.join(", "), hm(t))
        }
        Line::Messages(n) => format!("every {n} messages"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tz: &str, lines: Value) -> Spec {
        parse(&json!({"tz": tz, "lines": lines, "start": 0}), Utc::now()).unwrap()
    }
    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn checks_lines() {
        let bad = |l: Value| parse(&json!({"tz": "UTC", "lines": [l]}), Utc::now()).is_err();
        assert!(bad(json!({"kind": "every", "minutes": 14})));
        assert!(!bad(json!({"kind": "every", "minutes": 15})));
        assert!(bad(json!({"kind": "messages", "n": 4})));
        assert!(bad(
            json!({"kind": "monthly", "days": [32], "time": "09:00"})
        ));
        assert!(bad(json!({"kind": "weekly", "days": [], "time": "09:00"})));
        assert!(bad(json!({"kind": "daily", "time": "25:00"})));
        assert!(parse(
            &json!({"tz": "Mars/Base", "lines": [{"kind": "daily", "time": "09:00"}]}),
            Utc::now()
        )
        .is_err());
        assert!(parse(&json!({"tz": "UTC", "lines": []}), Utc::now()).is_err());
    }

    #[test]
    fn a_month_without_the_day_is_skipped_and_29_february_waits_for_a_leap_year() {
        let s = spec(
            "UTC",
            json!([{"kind": "monthly", "days": [31], "time": "09:00"}]),
        );
        let l = &s.lines[0];
        let got = upcoming(&s, l, at("2026-01-31T10:00:00Z"), 3);
        assert_eq!(
            got,
            [
                at("2026-03-31T09:00:00Z"),
                at("2026-05-31T09:00:00Z"),
                at("2026-07-31T09:00:00Z")
            ]
        );
        let s = spec(
            "UTC",
            json!([{"kind": "monthly", "days": [29], "time": "00:00"}]),
        );
        let got = upcoming(&s, &s.lines[0], at("2027-01-30T00:00:00Z"), 2);
        assert_eq!(
            got,
            [at("2027-03-29T00:00:00Z"), at("2027-04-29T00:00:00Z")]
        );
        let got = upcoming(&s, &s.lines[0], at("2028-01-30T00:00:00Z"), 1);
        assert_eq!(got, [at("2028-02-29T00:00:00Z")]);
    }

    #[test]
    fn nine_stays_nine_across_summer_time() {
        // Oslo: summer time starts 29 March 2026 (02:00 -> 03:00), ends 25 October (03:00 -> 02:00).
        let s = spec("Europe/Oslo", json!([{"kind": "daily", "time": "09:00"}]));
        assert_eq!(
            line_next(&s, &s.lines[0], at("2026-03-28T09:00:00Z")),
            Some(at("2026-03-29T07:00:00Z"))
        );
        assert_eq!(
            line_next(&s, &s.lines[0], at("2026-10-24T08:00:00Z")),
            Some(at("2026-10-25T08:00:00Z"))
        );
        // 02:30 does not exist on 29 March: the first valid minute after it, 03:00 summer time.
        let s = spec("Europe/Oslo", json!([{"kind": "daily", "time": "02:30"}]));
        assert_eq!(
            line_next(&s, &s.lines[0], at("2026-03-28T12:00:00Z")),
            Some(at("2026-03-29T01:00:00Z"))
        );
        // 02:30 happens twice on 25 October: once, at the first.
        let got = upcoming(&s, &s.lines[0], at("2026-10-24T12:00:00Z"), 2);
        assert_eq!(
            got,
            [at("2026-10-25T00:30:00Z"), at("2026-10-26T01:30:00Z")]
        );
    }

    #[test]
    fn weekly_every_once_and_the_soonest_line() {
        // 20 Nov 2026 is a Friday.
        let s = spec(
            "UTC",
            json!([{"kind": "weekly", "days": [5], "time": "18:00"}, {"kind": "once", "date": "2026-11-19", "time": "08:00"}]),
        );
        assert_eq!(
            next(&s, at("2026-11-18T00:00:00Z")),
            Some(at("2026-11-19T08:00:00Z"))
        );
        assert_eq!(
            next(&s, at("2026-11-19T08:00:00Z")),
            Some(at("2026-11-20T18:00:00Z"))
        );
        assert_eq!(line_next(&s, &s.lines[1], at("2026-11-19T08:00:00Z")), None);
        let mut s = spec("UTC", json!([{"kind": "every", "minutes": 30}]));
        s.start = at("2026-11-18T00:10:00Z");
        assert_eq!(
            next(&s, at("2026-11-18T01:00:00Z")),
            Some(at("2026-11-18T01:10:00Z"))
        );
        assert_eq!(
            next(&s, at("2026-11-18T01:10:00Z")),
            Some(at("2026-11-18T01:40:00Z"))
        );
        assert_eq!(
            next(&s, at("2026-11-17T00:00:00Z")),
            Some(at("2026-11-18T00:10:00Z"))
        );
        let s = spec("UTC", json!([{"kind": "messages", "n": 5}]));
        assert_eq!(next(&s, Utc::now()), None);
        assert_eq!(messages(&s), Some(5));
        assert_eq!(
            describe(&Line::Weekly(
                vec![1, 5],
                NaiveTime::from_hms_opt(18, 0, 0).unwrap()
            )),
            "Mon, Fri at 18:00"
        );
    }

    #[test]
    fn a_far_date_is_refused_and_times_too_close_are_found() {
        let now = at("2026-11-17T00:00:00Z");
        let far = json!({"tz": "America/New_York", "lines": [{"kind": "once", "date": "+262142-12-31", "time": "23:59"}]});
        assert!(parse(&far, now).is_err());
        let far = json!({"tz": "America/New_York", "lines": [{"kind": "once", "date": "2040-01-01", "time": "10:00"}]});
        assert!(parse(&far, now).is_err());
        let near = spec(
            "UTC",
            json!([{"kind": "daily", "time": "09:00"}, {"kind": "daily", "time": "09:10"}]),
        );
        assert!(too_close(&near, now));
        let mixed = spec(
            "UTC",
            json!([{"kind": "every", "minutes": 15}, {"kind": "every", "minutes": 16}]),
        );
        assert!(too_close(&mixed, now));
        let ok = spec(
            "UTC",
            json!([{"kind": "daily", "time": "09:00"}, {"kind": "weekly", "days": [1], "time": "18:00"}]),
        );
        assert!(!too_close(&ok, now));
    }

    #[test]
    fn stored_spec_reads_back_the_same() {
        let s = spec(
            "Europe/Oslo",
            json!([{"kind": "monthly", "days": [31, 1, 1], "time": "09:00"}]),
        );
        let v = to_json(&s);
        assert_eq!(v["lines"][0]["days"], json!([1, 31]));
        let back = parse(&v, Utc::now()).unwrap();
        assert_eq!(back.lines, s.lines);
        assert_eq!(back.start, s.start);
    }
}
