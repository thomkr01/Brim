/// The longest duration any command or setting takes: a year, the same cap as a filter's action
/// duration (`limits::FILTER_DURATION`). Longer values made `now + d` overflow or ran past any use.
pub const MAX_SECS: i64 = 365 * 86_400;

/// The answer for a duration over [`MAX_SECS`].
pub const TOO_LONG: &str = "A duration can be at most 365 days (52w).";

/// Parses `30s`, `10m`, `2h`, `5d`, `1w` into seconds, up to [`MAX_SECS`].
pub fn parse(s: &str) -> Option<i64> {
    raw(s)?.filter(|d| *d <= MAX_SECS)
}

/// True for a duration written right but longer than [`MAX_SECS`], so the caller can say so.
pub fn too_long(s: &str) -> bool {
    raw(s).is_some_and(|d| d.is_none_or(|d| d > MAX_SECS))
}

/// `None`: not a duration. `Some(None)`: a duration too big for an i64.
fn raw(s: &str) -> Option<Option<i64>> {
    let (n, unit) = s.split_at_checked(s.len().checked_sub(1)?)?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Only digits: a parse error is an overflow.
    let Ok(n) = n.parse::<i64>() else {
        return Some(None);
    };
    (n > 0).then(|| n.checked_mul(mult))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse("5d"), Some(432_000));
        assert_eq!(parse("10m"), Some(600));
        assert_eq!(parse("0m"), None);
        assert_eq!(parse("x"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("1é"), None);
        assert_eq!(parse("-5d"), None);
    }

    #[test]
    fn caps_at_a_year() {
        assert_eq!(parse("365d"), Some(MAX_SECS));
        assert_eq!(parse("366d"), None);
        assert!(too_long("366d"));
        assert!(too_long("106751991167300d"));
        assert!(too_long("99999999999999999999999s"));
        assert!(!too_long("365d"));
        assert!(!too_long("hello"));
        assert!(!too_long("0d"));
    }
}
