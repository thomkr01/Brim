//! Laying a server's edited version of an entry over a log the code built. Shared by the bot and
//! the dashboard (the appeal log is posted by the dashboard).
use crate::botmsg;
use serde_json::Value;
use twilight_model::channel::message::Embed;

/// The content and embeds of a rendered message.
pub fn parts(m: &Value) -> (String, Vec<Embed>) {
    // The builder's embeds have no `type`, which twilight's Embed requires.
    let mut e = m["embeds"].clone();
    e.as_array_mut()
        .into_iter()
        .flatten()
        .filter(|x| x.is_object())
        .for_each(|x| x["type"] = "rich".into());
    let embeds = serde_json::from_value(e).unwrap_or_default();
    (m["content"].as_str().unwrap_or_default().into(), embeds)
}

/// The first embed of the built-in message `key` (the server's copy `stored`, else the default),
/// for senders that post exactly one embed. A copy that renders no embed falls back to the default.
// shortcut: only the first embed of a saved copy is sent; send all once such a sender takes a list.
pub fn bot_embed(key: &str, stored: Option<&Value>, vars: &[(&str, &str)]) -> Embed {
    let first = |s| parts(&botmsg::render(key, s, vars)).1.into_iter().next();
    first(stored)
        .or_else(|| first(None))
        .expect("every default has an embed")
}

/// Lays a saved copy of entry `key` over `embed`: title and text from the copy (filled with the
/// log's own and with `extra`, the log's values), colour, footer, author and pictures when the
/// copy sets them. For an entry of facts (`whole`) the copy's fields replace the log's own, else
/// they are added. Over Discord's limits (4096 text, 6000 in all, 25 fields) the log is sent as
/// built, so an edit never loses a log.
pub fn over(
    key: &str,
    stored: &Value,
    embed: &Embed,
    extra: &[(String, String)],
    whole: bool,
) -> Embed {
    let mut vars = vec![
        ("title", embed.title.as_deref().unwrap_or("")),
        ("text", embed.description.as_deref().unwrap_or("")),
    ];
    vars.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    // A placeholder the log did not set is empty, never sent as raw `{name}`.
    if let Some(d) = botmsg::find(key) {
        for v in &d.vars {
            if !vars.iter().any(|(k, _)| k == v) {
                vars.push((v, ""));
            }
        }
    }
    let o = bot_embed(key, Some(stored), &vars);
    // The frame's own lines must not push the log's lines past Discord's 4096: send it as built.
    let text = vars[1].1;
    let clipped = !text.is_empty()
        && stored.to_string().contains("{text}")
        && !o.description.as_deref().is_some_and(|d| d.contains(text));
    if clipped {
        return embed.clone();
    }
    let mut e = embed.clone();
    e.title = o.title;
    e.description = o.description;
    e.color = o.color.or(e.color);
    e.footer = o.footer.or(e.footer);
    // A copy's author line keeps the log's icon (the member's avatar): links are never filled.
    let icon = e.author.as_ref().and_then(|a| a.icon_url.clone());
    if let Some(mut a) = o.author {
        a.icon_url = a.icon_url.or(icon);
        e.author = Some(a);
    }
    e.image = o.image.or(e.image);
    e.thumbnail = o.thumbnail.or(e.thumbnail);
    if whole {
        e.fields = o.fields;
    } else {
        e.fields.extend(o.fields);
    }
    let len = |s: Option<&str>| s.map_or(0, |s| s.chars().count());
    let total = len(e.title.as_deref())
        + len(e.description.as_deref())
        + len(e.footer.as_ref().map(|f| f.text.as_str()))
        + len(e.author.as_ref().map(|a| a.name.as_str()))
        + e.fields
            .iter()
            .map(|f| len(Some(&f.name)) + len(Some(&f.value)))
            .sum::<usize>();
    if total > 6000 || e.fields.len() > 25 {
        return embed.clone();
    }
    e
}

/// The security log `key` (one of `botmsg::SECURITY_LOGS`) as its default reads, filled with
/// `vars`, so the log the code sends and the default on the Bot messages page never differ. `vars`
/// holds every placeholder of the entry but `{title}` and `{when}`: a missing one is empty.
pub fn security_log(title: &str, key: &str, vars: &[(&str, &str)]) -> crate::Log {
    let Some(d) = botmsg::find(key) else {
        // Never in a release: a test checks every key the bot names. The facts still go out.
        debug_assert!(false, "{key} is no security log entry");
        let text: Vec<&str> = vars.iter().map(|(_, v)| *v).collect();
        return crate::Log::new(title, crate::embed::ALERT).text(&text.join("\n"));
    };
    debug_assert!(
        d.required
            .iter()
            .all(|r| matches!(*r, "title" | "when") || vars.iter().any(|(k, _)| k == r)),
        "{key} is missing a value"
    );
    let mut all = vars.to_vec();
    for v in &d.vars {
        if !all.iter().any(|(k, _)| k == v) {
            all.push((v, ""));
        }
    }
    let m = botmsg::render(key, None, &all);
    let e = &m["embeds"][0];
    let color = e["color"].as_u64().unwrap_or(0) as u32;
    let mut log = crate::Log::new(title, color).entry(key);
    if let Some(t) = e["description"].as_str() {
        log = log.text(t);
    }
    for f in e["fields"].as_array().into_iter().flatten() {
        let (n, v) = (
            f["name"].as_str().unwrap_or(""),
            f["value"].as_str().unwrap_or(""),
        );
        log = log.raw_field(n, v, f["inline"].as_bool().unwrap_or(false));
    }
    for (k, v) in vars {
        log = log.var(k, v);
    }
    log
}
