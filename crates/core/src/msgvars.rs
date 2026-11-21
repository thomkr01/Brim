//! Message Builder variables (`{server.name}`, `{role:<id>}`, ...) and time tags counted from the
//! moment of sending (`<t:+3600:R>`), filled in when Brim sends a saved message or a menu answer.
use std::collections::HashMap;

/// Variables that need a member who did something (a menu pick, later a trigger word). A message
/// sent by Send, a schedule or as a sticky has no such member.
pub const MEMBER_ONLY: [&str; 10] = [
    "member",
    "member.display_name",
    "member.username",
    "member.id",
    "member.avatar",
    "member.top_role",
    "member.joined_at",
    "member.created_at",
    "message.link",
    "trigger",
];

/// Every variable that takes no argument; `role` and `role.member_count` take a role id.
pub const PLAIN: [&str; 26] = [
    "member",
    "member.display_name",
    "member.username",
    "member.id",
    "member.avatar",
    "member.top_role",
    "member.joined_at",
    "member.created_at",
    "server.name",
    "server.id",
    "server.member_count",
    "server.boost_count",
    "server.boost_level",
    "server.owner",
    "server.icon",
    "server.created_at",
    "channel",
    "channel.name",
    "channel.id",
    "channel.topic",
    "channel.category",
    "message.link",
    "trigger",
    "date",
    "time",
    "weekday",
];

/// Variables whose value is a link, the only ones allowed as a whole image link.
pub const LINKS: [&str; 2] = ["server.icon", "member.avatar"];

/// What a variable is filled with. Names and other text members choose are cut to 100 characters
/// and escaped, so they never format, link or ping; Discord syntax (a mention, a time tag) is kept.
#[derive(Default)]
pub struct Ctx {
    /// Seconds since 1970, the moment of sending.
    pub now: i64,
    /// Plain text values by variable name; escaped when filled in.
    pub text: HashMap<&'static str, String>,
    /// Discord syntax by variable name (`<@id>`, `<t:..:R>`, links); filled in as it is.
    pub raw: HashMap<&'static str, String>,
    /// Role id -> members with it, for `{role.member_count:<id>}`.
    pub role_counts: HashMap<u64, u64>,
}

/// Backslash before every character Discord reads as markdown, a link or a mention, so a member's
/// name like `**x**` or `@everyone` or `https://evil` shows as typed.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\*_~`|>#-[]()<:@/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A `{name}` or `{name:arg}` at the start of `s`: (name, arg, length).
pub fn token(s: &str) -> Option<(&str, Option<&str>, usize)> {
    let end = s.find('}')?;
    let inner = s.strip_prefix('{')?.get(..end - 1)?;
    let (name, arg) = match inner.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (inner, None),
    };
    let known = match arg {
        None => PLAIN.contains(&name),
        Some(a) => {
            matches!(name, "role" | "role.member_count")
                && !a.is_empty()
                && a.len() <= 20
                && a.bytes().all(|b| b.is_ascii_digit())
        }
    };
    known.then_some((name, arg, end + 1))
}

/// The known variables used in `s`.
pub fn used(s: &str) -> Vec<&str> {
    let mut out = vec![];
    for (i, _) in s.match_indices('{') {
        if let Some((n, ..)) = token(&s[i..]) {
            out.push(n);
        }
    }
    out
}

/// Fills variables and `<t:+seconds:style>` tags. A variable with no value is left as typed.
pub fn fill(s: &str, ctx: &Ctx) -> String {
    fill_with(s, ctx, true)
}

/// As `fill`, for text Discord shows without markdown (footer, author name, button label): member
/// text goes in as typed, since a backslash there would show.
pub fn fill_plain(s: &str, ctx: &Ctx) -> String {
    fill_with(s, ctx, false)
}

/// The longest a filled-in variable can be: 100 characters, each escaped.
pub const MAX_VALUE: usize = 200;

/// The furthest a `<t:+seconds>` tag may count, as the footer time: one year.
pub const MAX_AFTER: i64 = 365 * 86_400;

fn fill_with(s: &str, ctx: &Ctx, esc: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['{', '<']) {
        out.push_str(&rest[..i]);
        let t = &rest[i..];
        if let Some((name, arg, len)) = token(t) {
            let v = match (name, arg) {
                ("role", Some(id)) => Some(format!("<@&{id}>")),
                ("role.member_count", Some(id)) => id
                    .parse::<u64>()
                    .ok()
                    .and_then(|id| ctx.role_counts.get(&id))
                    .map(u64::to_string),
                _ => ctx.raw.get(name).cloned().or_else(|| {
                    ctx.text.get(name).map(|v| {
                        let v: String = v.chars().take(100).collect();
                        if esc {
                            escape(&v)
                        } else {
                            v
                        }
                    })
                }),
            };
            match v {
                Some(v) => out.push_str(&v),
                None => out.push_str(&t[..len]),
            }
            rest = &t[len..];
            continue;
        }
        // <t:+3600:R> -> <t:{now + 3600}:R>
        let after = t.strip_prefix("<t:+").and_then(|r| {
            let (n, r) = r.split_once(':')?;
            let (style, _) = r.split_once('>')?;
            let digits = n.len();
            if !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // A bigger number would run past the end of time (and of i64).
            let n: i64 = n.parse().ok().filter(|n| *n <= MAX_AFTER)?;
            (style.len() == 1 && "tTdDfFR".contains(style))
                .then(|| (format!("<t:{}:{style}>", ctx.now + n), 4 + digits + 3))
        });
        match after {
            Some((tag, len)) => {
                out.push_str(&tag);
                rest = &t[len..];
            }
            None => {
                out.push_str(&t[..1]);
                rest = &t[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

impl Ctx {
    /// A context for sending at `now` (seconds since 1970) with the date and time variables set.
    /// Date and time are Discord time tags, so each member sees them in their own time zone; the
    /// day of the week is in UTC.
    pub fn at(now: i64) -> Ctx {
        let mut c = Ctx {
            now,
            ..Default::default()
        };
        c.raw.insert("date", format!("<t:{now}:D>"));
        c.raw.insert("time", format!("<t:{now}:t>"));
        const DAYS: [&str; 7] = [
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
        ];
        c.text.insert(
            "weekday",
            DAYS[now.div_euclid(86_400).rem_euclid(7) as usize].into(),
        );
        c
    }

    pub fn now() -> Ctx {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Ctx::at(secs)
    }
}

/// Seconds since 1970 when a Discord id (snowflake) was made.
pub fn snowflake_secs(id: u64) -> i64 {
    ((id >> 22) / 1000 + 1_420_070_400) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_escapes() {
        let mut ctx = Ctx {
            now: 1000,
            ..Default::default()
        };
        ctx.text.insert("server.name", "**Big** @everyone".into());
        ctx.text.insert("channel.topic", "x".repeat(150));
        ctx.raw.insert("channel", "<#5>".into());
        ctx.role_counts.insert(7, 12);
        assert_eq!(
            fill("Hi {server.name} in {channel}", &ctx),
            "Hi \\*\\*Big\\*\\* \\@everyone in <#5>"
        );
        assert_eq!(
            fill("{role:7} has {role.member_count:7}, {role:x} {nope}", &ctx),
            "<@&7> has 12, {role:x} {nope}"
        );
        // Unfilled variables stay as typed; time tags count from now.
        assert_eq!(
            fill("{member} <t:+60:R> <t:5:R> <t:+x:R>", &ctx),
            "{member} <t:1060:R> <t:5:R> <t:+x:R>"
        );
        assert_eq!(fill("{channel.topic}", &ctx).len(), 100);
        assert_eq!(fill_plain("{server.name}", &ctx), "**Big** @everyone");
        assert_eq!(
            fill("<t:+99999999999999999999:R>", &ctx),
            "<t:+99999999999999999999:R>"
        );
        assert_eq!(used("{member.id} {x} {role:1}"), vec!["member.id", "role"]);
        assert_eq!(escape("https://a.b"), "https\\:\\/\\/a.b");
        assert_eq!(snowflake_secs(175928847299117063), 1_462_015_105);
        // 16 November 2026 is a Monday.
        assert_eq!(Ctx::at(1_794_830_400).text["weekday"], "Monday");
    }
}
