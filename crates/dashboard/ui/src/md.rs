//! Discord markdown as preview HTML.

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Turns `mk`text`mk` pairs into `<tag>`; an unmatched marker stays as text.
fn pairs(s: &str, mk: &str, tag: &str) -> String {
    let (mut out, mut rest) = (String::new(), s);
    while let Some(a) = rest.find(mk) {
        let after = &rest[a + mk.len()..];
        let Some(b) = after.find(mk).filter(|&b| b > 0) else {
            break;
        };
        out += &format!(
            "{}<{tag}>{}</{}>",
            &rest[..a],
            &after[..b],
            tag.split(' ').next().unwrap_or(tag)
        );
        rest = &after[b + mk.len()..];
    }
    out + rest
}

// shortcut: no nesting rules (`***x***`, `snake_case` italics); swap in a real parser if previews look wrong.
fn emph(s: &str) -> String {
    [
        ("**", "b"),
        ("__", "u"),
        ("~~", "s"),
        ("||", "span class=\"spoiler\""),
        ("*", "i"),
        ("_", "i"),
    ]
    .iter()
    .fold(s.to_string(), |s, (mk, tag)| pairs(&s, mk, tag))
}

/// Discord's inline markdown as HTML; `s` must already be escaped.
pub fn inline(s: &str) -> String {
    // U+0001 marks a stashed part: typed by the author it would let a later swap write a link's
    // HTML into an earlier part's attribute (an emoji's `alt`), so it is dropped first.
    let s = s.replace('\u{1}', "");
    // code spans and links are stashed first so the emphasis passes leave their insides (and URLs) alone
    let (mut out, mut kept, mut rest) = (String::new(), Vec::new(), s.as_str());
    while let Some(i) = rest.find(['`', '[', '&', '@']) {
        let t = &rest[i..];
        let hit = if let Some(typed) = t.strip_prefix('@') {
            // A typed @name (an example such as @alex) draws as the mention it stands for; an email's @ does not.
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let before = rest[..i]
                .chars()
                .next_back()
                .or_else(|| out.chars().next_back());
            let name = typed
                .split(|c: char| !(word(c) || c == '.' || c == '-'))
                .next()
                .unwrap_or("")
                .trim_end_matches(['.', '-']);
            (!name.is_empty() && !before.is_some_and(word)).then(|| {
                (
                    name.len() + 1,
                    format!("<span class=\"mention\">@{name}</span>"),
                )
            })
        } else if t.starts_with('&') {
            t.strip_prefix("&lt;").and_then(|r| {
                r.find("&gt;")
                    .filter(|&e| e < 80)
                    .and_then(|e| token(&r[..e]).map(|h| (e + 8, h)))
            })
        } else if let Some(c) = t.strip_prefix('`') {
            c.find('`')
                .filter(|&n| n > 0)
                .map(|n| (n + 2, format!("<code>{}</code>", &c[..n])))
        } else {
            t.find("](")
                .and_then(|m| t[m + 2..].find(')').map(|e| (m, &t[m + 2..m + 2 + e])))
                .filter(|(_, u)| u.starts_with("https://") || u.starts_with("http://"))
                .map(|(m, u)| {
                    // Encoded, so `md`'s icon pass cannot put markup inside the attribute.
                    let href = u.replace('{', "%7B").replace('}', "%7D");
                    (
                        m + 3 + u.len(),
                        format!(
                            "<a href=\"{href}\" target=\"_blank\" rel=\"noopener\">{}</a>",
                            emph(&t[1..m])
                        ),
                    )
                })
        };
        out += &rest[..i];
        match hit {
            Some((n, html)) => {
                out += &format!("\u{1}{}\u{1}", kept.len());
                kept.push(html);
                rest = &t[n..];
            }
            None => {
                out += &t[..1];
                rest = &t[1..];
            }
        }
    }
    let mut out = emph(&(out + rest));
    for (k, h) in kept.iter().enumerate() {
        out = out.replace(&format!("\u{1}{k}\u{1}"), h);
    }
    out
}

/// The markdown Discord renders in embed descriptions and field values, as escaped preview HTML.
pub fn md(s: &str) -> String {
    // shortcut: also fills a tag inside a code span; split by part if a server needs that.
    crate::icons::fill_tags(&md_only(s))
}

fn md_only(s: &str) -> String {
    let e = esc(s);
    let parts: Vec<&str> = e.split("```").collect();
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 && i + 1 < parts.len() {
                // a first line without spaces is the language tag
                let body = part
                    .split_once('\n')
                    .filter(|(l, _)| !l.contains(' '))
                    .map_or(*part, |(_, b)| b);
                return format!("<pre>{}</pre>", body.trim_end_matches('\n'));
            }
            let part = if i % 2 == 1 {
                format!("```{part}")
            } else {
                part.strip_prefix('\n')
                    .filter(|_| i > 0)
                    .unwrap_or(part)
                    .to_string()
            };
            if part.is_empty() {
                return part;
            }
            part.split('\n')
                .map(|l| {
                    let (cls, l) = [
                        ("# ", "h1"),
                        ("## ", "h2"),
                        ("### ", "h3"),
                        ("-# ", "sub"),
                        ("&gt; ", "q"),
                        ("- ", "li"),
                        ("* ", "li"),
                    ]
                    .iter()
                    .find_map(|(p, c)| l.strip_prefix(p).map(|r| (*c, r)))
                    .unwrap_or(("", l));
                    format!(
                        "<div class=\"{cls}\">{}</div>",
                        if l.is_empty() {
                            "<br>".into()
                        } else {
                            inline(l)
                        }
                    )
                })
                .collect::<String>()
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn now_secs() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

#[cfg(not(target_arch = "wasm32"))]
fn now_secs() -> i64 {
    0
}

/// Discord's `<@id>`, `<#id>`, `<@&id>`, `<:emoji:id>` and `<t:unix:style>` (inside, still escaped) as HTML.
fn token(t: &str) -> Option<String> {
    let id = |s: &str| s.parse::<u64>().ok().map(|n| n.to_string());
    let pill = |text: String| Some(format!("<span class=\"mention\">{}</span>", esc(&text)));
    if let Some(r) = t.strip_prefix("@&amp;") {
        let r = id(r)?;
        return pill(format!(
            "@{}",
            crate::discord::name("roles", &r)
                .unwrap_or(r.clone())
                .trim_start_matches('@')
        ));
    }
    if let Some(r) = t.strip_prefix('@') {
        let r = id(r.trim_start_matches('!'))?;
        return pill(format!(
            "@{}",
            crate::discord::name("users", &r).unwrap_or(r.clone())
        ));
    }
    if let Some(r) = t.strip_prefix('#') {
        let r = id(r)?;
        return pill(format!(
            "#{}",
            crate::discord::name("channels", &r).unwrap_or(r.clone())
        ));
    }
    if let Some(r) = t.strip_prefix("t:") {
        let (n, style) = r.split_once(':').unwrap_or((r, "f"));
        // `<t:+3600:R>` counts from sending; the preview counts from now.
        let secs = match n.strip_prefix('+') {
            Some(after) => now_secs().checked_add(after.parse::<i64>().ok()?)?,
            None => n.parse().ok()?,
        };
        return Some(format!(
            "<span class=\"ts\">{}</span>",
            crate::discord::time(secs, style)
        ));
    }
    let (name, eid) = t
        .strip_prefix("a:")
        .unwrap_or(t)
        .strip_prefix(':')?
        .split_once(':')?;
    let eid = id(eid)?;
    Some(format!(
        "<img class=\"emo\" alt=\":{name}:\" src=\"https://cdn.discordapp.com/emojis/{eid}.webp\">"
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_typed_marker_cannot_move_a_link_into_an_attribute() {
        let h = super::inline(&super::esc(
            "<:\u{1}1\u{1}:42> [y](https://a src=x onerror=alert`1` b)",
        ));
        assert!(!h.contains('\u{1}'));
        assert!(!h.contains("alt=\":<a"), "{h}");
        // The emoji's alt attribute holds no quote, so nothing after it becomes an attribute.
        let alt = h
            .split("alt=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .unwrap_or("");
        assert!(!alt.contains('<'), "{h}");
    }

    use super::*;

    #[test]
    fn md_renders_discord_markdown() {
        assert_eq!(
            inline("**b** *i* __u__ ~~s~~ ||x||"),
            "<b>b</b> <i>i</i> <u>u</u> <s>s</s> <span class=\"spoiler\">x</span>"
        );
        assert_eq!(inline("`**a**` [x_y](https://e.com/a_b_c)"), "<code>**a**</code> <a href=\"https://e.com/a_b_c\" target=\"_blank\" rel=\"noopener\">x_y</a>");
        assert_eq!(
            inline("[x](javascript:alert(1))"),
            "[x](javascript:alert(1))"
        );
        assert_eq!(
            md("<b>\n> q"),
            "<div class=\"\">&lt;b&gt;</div><div class=\"q\">q</div>"
        );
        assert_eq!(md("```rs\nlet *a*;\n```"), "<pre>let *a*;</pre>");
        assert_eq!(md("## H"), "<div class=\"h2\">H</div>");
        // mentions and emojis keep their underscores away from the italic pass
        assert_eq!(inline(&esc("<@1001> <#2001> <@&9> <:right_sort:42> _a_ <x> a & b")),
            "<span class=\"mention\">@1001</span> <span class=\"mention\">#2001</span> <span class=\"mention\">@9</span> <img class=\"emo\" alt=\":right_sort:\" src=\"https://cdn.discordapp.com/emojis/42.webp\"> <i>a</i> &lt;x&gt; a &amp; b");
    }

    #[test]
    fn a_typed_at_name_is_a_mention_but_an_email_is_not() {
        let m = |n: &str| format!("<span class=\"mention\">@{n}</span>");
        assert_eq!(
            inline("Needs @Members."),
            format!("Needs {}.", m("Members"))
        );
        assert_eq!(
            inline("@alex, @sam"),
            format!("{}, {}", m("alex"), m("sam"))
        );
        assert_eq!(inline("a@b.com"), "a@b.com");
        assert_eq!(inline("@"), "@");
    }
}
