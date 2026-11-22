//! `/invites` and `t!invites` (Invite Tracking): invite stats, the leaderboard, who invited a member,
//! the tracking status, and temporary links made for a member and credited to them.
use std::collections::HashMap;

use anyhow::Result;
use brim_core::{duration, invites as store, limits, permits::Level};
use brim_logging::embed;
use twilight_http::request::AuditLogReason;
use twilight_model::{
    application::interaction::application_command::{CommandDataOption, CommandOptionValue},
    channel::{message::Embed, permission_overwrite::PermissionOverwrite, ChannelType},
    guild::Permissions,
    id::{
        marker::{ChannelMarker, GuildMarker, RoleMarker, UserMarker},
        Id,
    },
};
use twilight_util::permission_calculator::PermissionCalculator;

use crate::{app::App, automod::now_ms};

use crate::invites::{status, tracking_ok, Status};

const PER_PAGE: i64 = 10;
/// Policy, not Discord's limits: live links per member, and the gap between two new ones.
const MAX_LIVE: usize = 3;
const LINK_GAP_MS: i64 = 15 * 60 * 1000;
/// Links the whole server may make per minute, so many members cannot run up Discord calls.
const GUILD_PER_MINUTE: i64 = 5;
const NEED: Permissions = Permissions::VIEW_CHANNEL.union(Permissions::CREATE_INVITE);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Sub {
    /// A member as typed (mention, id or name); none is the caller.
    Show(Option<String>),
    /// 1-based.
    Top(i64),
    Who(String),
    Status,
    Create {
        channel: Option<u64>,
        secs: Option<i64>,
        uses: Option<i64>,
    },
}

/// `args` are the words after `invites`.
pub(crate) fn parse(args: &[&str]) -> Result<Sub, String> {
    let rest = || Some(args[1..].join(" ")).filter(|s| !s.is_empty());
    match args.first().map(|w| w.to_lowercase()).as_deref() {
        None => Ok(Sub::Show(None)),
        Some("show") => Ok(Sub::Show(rest())),
        Some("top") => match args.get(1) {
            None => Ok(Sub::Top(1)),
            Some(p) => p
                .parse()
                .ok()
                .filter(|p| *p >= 1)
                .map(Sub::Top)
                .ok_or_else(|| format!("`{p}` is not a page number")),
        },
        Some("who") => rest().map(Sub::Who).ok_or_else(|| "Name a member".into()),
        Some("status") => Ok(Sub::Status),
        Some("create") => {
            let (mut channel, mut secs, mut uses) = (None, None, None);
            for w in &args[1..] {
                let digits = w.trim_start_matches("<#").trim_end_matches('>');
                let num = digits.parse::<u64>().ok();
                match num {
                    // A channel mention, or a bare id (snowflakes have 17+ digits).
                    Some(id) if w.starts_with("<#") || digits.len() >= 17 => channel = Some(id),
                    Some(n) => uses = Some(n.min(i64::MAX as u64) as i64),
                    None if w.eq_ignore_ascii_case("never") => secs = Some(0),
                    None => match duration::parse(&w.to_lowercase()) {
                        Some(s) => secs = Some(s),
                        None if duration::too_long(&w.to_lowercase()) => {
                            return Err(duration::TOO_LONG.into())
                        }
                        None => {
                            return Err(format!(
                                "`{w}` is not a channel, a time or a number of uses"
                            ))
                        }
                    },
                }
            }
            Ok(Sub::Create {
                channel,
                secs,
                uses,
            })
        }
        // `t!invites @member`
        Some(_) => Ok(Sub::Show(Some(args.join(" ")))),
    }
}

/// `/invites <sub> ...` as the words of the prefix command.
pub(crate) fn slash_words(o: &[CommandDataOption]) -> Vec<String> {
    let mut w = vec!["invites".to_string()];
    let Some(CommandDataOption {
        name,
        value: CommandOptionValue::SubCommand(so),
    }) = o.first()
    else {
        return w;
    };
    w.push(name.clone());
    for o in so {
        w.push(match &o.value {
            CommandOptionValue::User(u) => format!("<@{u}>"),
            CommandOptionValue::Channel(c) => format!("<#{c}>"),
            CommandOptionValue::Integer(n) => n.to_string(),
            // A bare number in the duration box is seconds, never uses.
            // 0 means never, as in the settings.
            CommandOptionValue::String(s) if s == "0" => "never".into(),
            CommandOptionValue::String(s) if s.bytes().all(|b| b.is_ascii_digit()) => {
                format!("{s}s")
            }
            CommandOptionValue::String(s) => s.clone(),
            _ => continue,
        });
    }
    w
}

/// The reply: text and an embed. `here` is the channel the command was used in; `slash` replies
/// are ephemeral, prefix replies are public.
pub(crate) async fn run(
    app: &App,
    gid: Id<GuildMarker>,
    invoker: Id<UserMarker>,
    level: &Level,
    here: Option<Id<ChannelMarker>>,
    slash: bool,
    args: &[&str],
) -> Result<(String, Option<Embed>)> {
    let sub = match parse(args) {
        Ok(s) => s,
        Err(e) => return Ok((e, None)),
    };
    let cfg = app.cfg(gid.get()).await?;
    if cfg.misc.invite_tracking && matches!(sub, Sub::Status | Sub::Create { .. }) {
        crate::invites::reseed(app, gid.get());
    }
    let g = gid.get() as i64;
    let staff = level.has("invite_view");
    let st = status(app, gid.get());
    let (since, unknown) = store::summary(&app.pool, g).await?;
    let notes = notes(cfg.misc.invite_tracking, &st, unknown);
    let since_text = since.map_or("No joins seen yet".to_string(), |ms| {
        format!("<t:{}:d>", ms / 1000)
    });
    let notes = notes.join("\n");
    let answer = async |key: &str, vars: &[(&str, String)]| {
        let mut all = vars.to_vec();
        // Since when joins are tracked and the notes, where the answer shows them.
        let shows = |v| brim_logging::botmsg::find(key).is_some_and(|d| d.vars.contains(&v));
        all.extend(
            [("since", since_text.clone()), ("notes", notes.clone())]
                .into_iter()
                .filter(|(n, _)| shows(*n)),
        );
        Ok((
            String::new(),
            Some(crate::reply::answer(app, gid, key, &all).await),
        ))
    };
    let text = |t: &str| Ok((t.to_string(), None));
    let reply = async |(key, vars): Reply| {
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let said = crate::reply::say(app, gid, key, &vars).await;
        Ok((said.text, said.embeds.into_iter().next()))
    };
    let no_permit = async || {
        let said = crate::reply::say(app, gid, "reply.invite.no_permit", &[]).await;
        Ok((said.text, said.embeds.into_iter().next()))
    };
    match sub {
        Sub::Show(arg) => {
            let who = match arg {
                None => invoker,
                // Another member's counts are as public as the leaderboard.
                Some(_) if !cfg.misc.invites_public_top && *level == Level::None => {
                    return reply(("reply.invite.not_public_member", vec![])).await;
                }
                Some(a) => match member_arg(app, gid, &a).await? {
                    Ok(id) => id,
                    Err(e) => return text(&e),
                },
            };
            let s = store::stats(&app.pool, g, who.get() as i64).await?;
            let vars = [
                ("member", embed::user(who.get())),
                ("invited", s.total.to_string()),
                ("still_here", s.present.to_string()),
                ("left", s.left.to_string()),
            ];
            answer("reply.invite.stats", &vars).await
        }
        Sub::Top(page) => {
            if !cfg.misc.invites_public_top && *level == Level::None {
                return reply(("reply.invite.not_public_top", vec![])).await;
            }
            let pages = ((store::inviters(&app.pool, g).await? + PER_PAGE - 1) / PER_PAGE).max(1);
            let page = page.min(pages);
            let rows = store::top(&app.pool, g, PER_PAGE, (page - 1) * PER_PAGE).await?;
            let mut ranking: Vec<String> = rows
                .iter()
                .enumerate()
                .map(|(i, (id, s))| {
                    let rank = (page - 1) * PER_PAGE + i as i64 + 1;
                    format!(
                        "`{rank}.` <@{id}>: `{}` ({} still here)",
                        s.total, s.present
                    )
                })
                .collect();
            if ranking.is_empty() {
                ranking.push("No invites counted yet".into());
            }
            let vars = [
                ("ranking", ranking.join("\n")),
                ("page_no", page.to_string()),
                ("pages", pages.to_string()),
            ];
            answer("reply.invite.top", &vars).await
        }
        Sub::Who(arg) => {
            if !staff {
                return no_permit().await;
            }
            let user = match member_arg(app, gid, &arg).await? {
                Ok(id) => id,
                Err(e) => return text(&e),
            };
            let Some(m) = store::who(&app.pool, g, user.get() as i64).await? else {
                let vars = [("member", embed::user(user.get()))];
                return answer("reply.invite.who_unseen", &vars).await;
            };
            let by = match (m.inviter_id, m.creditable) {
                (Some(i), true) => embed::user(i as u64),
                (Some(i), false) => format!("{} (not counted)", embed::user(i as u64)),
                (None, _) => "Unknown".into(),
            };
            // A code is only for the asker: never in a public prefix reply.
            let code = m.code.filter(|_| slash).unwrap_or_default();
            let vars = [
                ("member", embed::user(user.get())),
                ("inviter", by),
                ("first_seen", format!("<t:{}:f>", m.first_joined_ms / 1000)),
                ("method", method(&m.how).to_string()),
                ("present", if m.present { "Yes" } else { "No" }.to_string()),
                ("joins", m.joins.to_string()),
                ("code", code),
            ];
            answer("reply.invite.who", &vars).await
        }
        Sub::Status => {
            if !staff {
                return no_permit().await;
            }
            let state = match st {
                Status::Waiting => "Starting: reading the server's invites".to_string(),
                Status::Healthy { since_ms } => format!("Working since <t:{}:R>", since_ms / 1000),
                Status::Failing { why, since_ms } => {
                    format!("Failing since <t:{}:R>: {why}", since_ms / 1000)
                }
            };
            let on = if cfg.misc.invite_tracking {
                "On"
            } else {
                "Off"
            };
            let vars = [
                ("tracking", on.to_string()),
                ("status", state),
                ("unknown", unknown.to_string()),
            ];
            answer("reply.invite.status", &vars).await
        }
        Sub::Create {
            channel,
            secs,
            uses,
        } => {
            if !cfg.misc.invite_tracking || !tracking_ok(app, gid.get()) {
                return reply(("reply.invite.tracking_off", vec![])).await;
            }
            // A join lock, any lock or an active raid: no new way in.
            if crate::lockdown::join_lock(app, gid).await?.is_some()
                || crate::lockdown::raid_or_lock(app, gid).await?
            {
                return reply(("reply.invite.locked", vec![])).await;
            }
            let Some(cid) = channel.and_then(Id::new_checked).or(here) else {
                return reply(("reply.invite.no_channel", vec![])).await;
            };
            // The settings are the server's maximums, and the default.
            let (max_secs, max_uses) = (
                cfg.misc.invite_max_age_secs.into(),
                cfg.misc.invite_max_uses.into(),
            );
            let secs = secs.unwrap_or(max_secs);
            let uses = uses.unwrap_or(max_uses);
            if let Some(why) = out_of_range(secs, uses, max_secs, max_uses) {
                return reply(why).await;
            }
            reply(make(app, gid, invoker, cid, secs, uses, slash).await?).await
        }
    }
}

/// A mention or id as given; a name is looked up.
async fn member_arg(
    app: &App,
    gid: Id<GuildMarker>,
    a: &str,
) -> Result<Result<Id<UserMarker>, String>> {
    let d = a.trim_start_matches("<@").trim_start_matches('!');
    match d
        .trim_end_matches('>')
        .parse()
        .ok()
        .and_then(Id::new_checked)
    {
        Some(id) => Ok(Ok(id)),
        None => crate::targets::resolve(app, gid, a).await,
    }
}

/// What a reply must add so a low or zero count is not read as the whole truth.
fn notes(on: bool, st: &Status, unknown: i64) -> Vec<String> {
    let mut v = vec![];
    match (on, st) {
        (false, _) => {
            v.push("Invite tracking is off on this server, so new joins are not counted.".into())
        }
        (true, Status::Waiting) => {
            v.push("Invite tracking is starting; new joins are not counted yet.".into())
        }
        (true, Status::Failing { why, .. }) => v.push(format!(
            "Invite tracking is not working ({why}), so new joins are not counted."
        )),
        (true, Status::Healthy { .. }) => {}
    }
    if unknown > 0 {
        v.push(format!(
            "{unknown} joins in the last 7 days could not be tied to an invite."
        ));
    }
    v
}

fn method(how: &str) -> &'static str {
    match how {
        "single" => "One invite grew by one at this join (observed, not confirmed by Discord)",
        "group" => "Matched with other joins at the same time (not confirmed by Discord)",
        "bot" => "Added as a bot, no invite",
        _ => "Unknown",
    }
}

/// Why a new link is refused for a member with `live` working links whose newest was made at `last_ms`.
fn capped(live: usize, last_ms: Option<i64>, now: i64) -> Option<Reply> {
    if live >= MAX_LIVE {
        return Some(("reply.invite.link_cap", vec![("max", MAX_LIVE.to_string())]));
    }
    let next = last_ms? + LINK_GAP_MS;
    (now < next).then(|| {
        let next = format!("<t:{}:R>", next / 1000);
        ("reply.invite.link_gap", vec![("next_link", next)])
    })
}

/// Why `secs` or `uses` is refused. A max of 0 allows "never" / "no limit"; any other max is a cap.
fn out_of_range(secs: i64, uses: i64, max_secs: i64, max_uses: i64) -> Option<Reply> {
    let max = |key, n: String| Some((key, vec![("max", n)]));
    let b = limits::INVITE_MAX_AGE.1;
    if max_secs == 0 && !(0..=b).contains(&secs) {
        return max("reply.invite.max_age", embed::duration(b));
    }
    if max_secs > 0 && !(1..=max_secs).contains(&secs) {
        return max("reply.invite.max_age_here", embed::duration(max_secs));
    }
    let b = limits::INVITE_MAX_USES.1;
    if max_uses == 0 && !(0..=b).contains(&uses) {
        return max("reply.invite.uses_range", b.to_string());
    }
    if max_uses > 0 && !(1..=max_uses).contains(&uses) {
        return max("reply.invite.max_uses_here", max_uses.to_string());
    }
    None
}

/// A reply entry and its values.
type Reply = (&'static str, Vec<(&'static str, String)>);

/// Discord's effective permissions in a channel include VIEW_CHANNEL and CREATE_INVITE.
fn can_invite(
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    owner: Id<UserMarker>,
    role_perms: &HashMap<Id<RoleMarker>, u64>,
    roles: &[Id<RoleMarker>],
    kind: ChannelType,
    overwrites: &[PermissionOverwrite],
) -> bool {
    let p = |r: &Id<RoleMarker>| {
        Permissions::from_bits_truncate(role_perms.get(r).copied().unwrap_or(0))
    };
    let mine: Vec<_> = roles.iter().map(|r| (*r, p(r))).collect();
    PermissionCalculator::new(gid, user, p(&gid.cast()), &mine)
        .owner_id(owner)
        .in_channel(kind, overwrites)
        .contains(NEED)
}

/// Frees the member's claim on every path out of [`make`].
struct Claim<'a>(&'a App, (u64, u64));

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.invite_claims.lock().unwrap().remove(&self.1);
    }
}

/// Makes a link in `cid` for `invoker` after the channel, permission and cap checks, saves it,
/// and gives it: in the reply for slash, by DM for prefix. Returns the reply text.
async fn make(
    app: &App,
    gid: Id<GuildMarker>,
    invoker: Id<UserMarker>,
    cid: Id<ChannelMarker>,
    secs: i64,
    uses: i64,
    slash: bool,
) -> Result<Reply> {
    let plain = |key| Ok((key, vec![]));
    // The caps only need the database, so they come before any Discord call.
    let key = (gid.get(), invoker.get());
    if !app.invite_claims.lock().unwrap().insert(key) {
        return plain("reply.invite.busy");
    }
    let _claim = Claim(app, key);
    let g = gid.get() as i64;
    let owner = invoker.get() as i64;
    let live = store::live_links(&app.pool, g, owner).await?.len();
    let last = store::last_link_ms(&app.pool, g, owner).await?;
    if let Some(why) = capped(live, last, now_ms()) {
        return Ok(why);
    }
    if store::recent_links(&app.pool, g).await? >= GUILD_PER_MINUTE {
        return plain("reply.invite.server_busy");
    }
    let ch = match app.http.channel(cid).await {
        Ok(r) => r.model().await?,
        Err(e) if crate::http_err::no_access(&e) => return plain("reply.invite.cannot_see"),
        Err(e) => return Err(e.into()),
    };
    let kinds = [
        ChannelType::GuildText,
        ChannelType::GuildAnnouncement,
        ChannelType::GuildVoice,
    ];
    if ch.guild_id != Some(gid) || !kinds.contains(&ch.kind) {
        return plain("reply.invite.wrong_channel");
    }
    let info = app.guild_info(gid).await?;
    // Read fresh, not from the cached GuildInfo: a role that just lost Create Invite must count.
    let role_perms: HashMap<Id<RoleMarker>, u64> = app
        .http
        .roles(gid)
        .await?
        .models()
        .await?
        .into_iter()
        .map(|r| (r.id, r.permissions.bits()))
        .collect();
    let ows = ch.permission_overwrites.as_deref().unwrap_or_default();
    let Some(m) = crate::http_err::member(&app.http, gid, invoker).await? else {
        return plain("reply.invite.not_member");
    };
    // A timed-out member keeps only View Channel and Read Message History.
    let timed_out = m
        .communication_disabled_until
        .is_some_and(|t| t.as_micros() / 1000 > now_ms());
    if timed_out
        || !can_invite(
            gid,
            invoker,
            info.owner,
            &role_perms,
            &m.roles,
            ch.kind,
            ows,
        )
    {
        let channel = format!("<#{cid}>");
        return Ok(("reply.invite.member_perms", vec![("channel", channel)]));
    }
    let me = crate::http_err::member(&app.http, gid, app.me).await?;
    let bot_ok = me
        .is_some_and(|b| can_invite(gid, app.me, info.owner, &role_perms, &b.roles, ch.kind, ows));
    if !bot_ok {
        let channel = format!("<#{cid}>");
        return Ok(("reply.invite.bot_perms", vec![("channel", channel)]));
    }
    // Only these four fields: no roles, targets or temporary membership.
    let made = app
        .http
        .create_invite(cid)
        .unique(true)
        .temporary(false)
        .max_age(secs as u32)
        .max_uses(uses as u16)
        .reason(&format!("/invites create by {invoker}"))
        .await;
    let code = match made {
        Ok(r) => r.model().await?.code,
        Err(e) => {
            let e = anyhow::Error::from(e);
            let error = crate::http_err::why(&e);
            return Ok(("reply.invite.refused", vec![("error", error)]));
        }
    };
    let expires = (secs > 0).then(|| now_ms() + secs * 1000);
    let saved = store::save_link(
        &app.pool,
        g,
        &code,
        owner,
        cid.get() as i64,
        uses as i32,
        expires,
    )
    .await;
    if let Err(e) = saved {
        tracing::warn!(?e, guild = gid.get(), "invite link not saved, removing it");
        revoke(app, &code).await;
        return plain("reply.invite.not_saved");
    }
    let when = expires.map_or("never expires".into(), |ms| {
        format!("expires <t:{}:R>", ms / 1000)
    });
    let uses_s = match uses {
        0 => "with no use limit".to_string(),
        n => format!("for {n} uses"),
    };
    if slash {
        let vars = vec![("code", code), ("expiry", when), ("uses", uses_s)];
        return Ok(("reply.invite.link", vars));
    }
    let vars = [
        ("code", code.as_str()),
        ("expiry", when.as_str()),
        ("uses", uses_s.as_str()),
    ];
    let rows = [crate::verify::row(crate::welcome::link(
        "Join",
        format!("https://discord.gg/{code}"),
    ))];
    if crate::reply::dm_msg(app, Some(gid), invoker, "dm.invite_link", &vars, &rows).await {
        return plain("reply.invite.dm_sent");
    }
    // Nobody got the link: remove it. The row stays as gone, so the 15 minute gap still holds.
    revoke(app, &code).await;
    if let Err(e) = store::links_gone(&app.pool, g, Some(&code), &[], now_ms()).await {
        tracing::warn!(?e, guild = gid.get(), "unsent invite link not marked gone");
    }
    plain("reply.invite.dm_failed")
}

/// Deletes the working `/invites create` links of a member who was banned or quarantined
/// (owner decision 11-15), so their links let nobody else in. Discord's own invites are left alone.
pub(crate) async fn revoke_links(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) {
    let g = gid.get() as i64;
    let live = match store::live_links(&app.pool, g, user.get() as i64).await {
        Ok(l) => l,
        Err(e) => return tracing::warn!(?e, "reading a member's invite links failed"),
    };
    for (code, _) in live {
        revoke(app, &code).await;
        if let Err(e) = store::links_gone(&app.pool, g, Some(&code), &[], 0).await {
            tracing::warn!(?e, "marking a revoked invite link failed");
        }
    }
}

/// [`revoke_links`] in its own task, so a protection step never waits for it.
pub(crate) fn revoke_links_later(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) {
    if let Some(app) = crate::invites::arc(app) {
        tokio::spawn(async move { revoke_links(&app, gid, user).await });
    }
}

async fn revoke(app: &App, code: &str) {
    crate::http_err::best_effort(app.http.delete_invite(code).await, "delete invite");
}

#[cfg(test)]
mod tests {
    use super::*;
    use twilight_model::channel::permission_overwrite::PermissionOverwriteType;

    #[test]
    fn parses_every_form() {
        assert_eq!(parse(&[]), Ok(Sub::Show(None)));
        assert_eq!(parse(&["<@5>"]), Ok(Sub::Show(Some("<@5>".into()))));
        assert_eq!(
            parse(&["show", "bob", "x"]),
            Ok(Sub::Show(Some("bob x".into())))
        );
        assert_eq!(parse(&["TOP"]), Ok(Sub::Top(1)));
        assert_eq!(parse(&["top", "3"]), Ok(Sub::Top(3)));
        assert!(parse(&["top", "0"]).is_err() && parse(&["top", "x"]).is_err());
        assert!(parse(&["who"]).is_err());
        assert_eq!(parse(&["who", "<@7>"]), Ok(Sub::Who("<@7>".into())));
        assert_eq!(parse(&["status"]), Ok(Sub::Status));
        assert_eq!(
            parse(&["create"]),
            Ok(Sub::Create {
                channel: None,
                secs: None,
                uses: None
            })
        );
        assert_eq!(
            parse(&["create", "10", "<#123>", "2H"]),
            Ok(Sub::Create {
                channel: Some(123),
                secs: Some(7200),
                uses: Some(10)
            })
        );
        assert_eq!(
            parse(&["create", "123456789012345678", "never"]),
            Ok(Sub::Create {
                channel: Some(123456789012345678),
                secs: Some(0),
                uses: None
            })
        );
        assert!(parse(&["create", "soon"]).is_err());
    }

    #[test]
    fn slash_options_become_words() {
        let o = |name: &str, value| CommandDataOption {
            name: name.into(),
            value,
        };
        let sub = |name: &str, opts| vec![o(name, CommandOptionValue::SubCommand(opts))];
        let w = slash_words(&sub(
            "create",
            vec![
                o("channel", CommandOptionValue::Channel(Id::new(9))),
                o("duration", CommandOptionValue::String("600".into())),
                o("uses", CommandOptionValue::Integer(4)),
            ],
        ));
        assert_eq!(w, ["invites", "create", "<#9>", "600s", "4"]);
        let words: Vec<&str> = w[1..].iter().map(String::as_str).collect();
        assert_eq!(
            parse(&words),
            Ok(Sub::Create {
                channel: Some(9),
                secs: Some(600),
                uses: Some(4)
            })
        );
        let w = slash_words(&sub(
            "who",
            vec![o("user", CommandOptionValue::User(Id::new(3)))],
        ));
        assert_eq!(w, ["invites", "who", "<@3>"]);
    }

    #[test]
    fn a_zero_never_hides_that_tracking_is_off_or_failing() {
        let ok = Status::Healthy { since_ms: 0 };
        assert!(notes(true, &ok, 0).is_empty());
        assert!(notes(false, &ok, 0)[0].contains("off"));
        assert!(notes(true, &Status::Waiting, 0)[0].contains("starting"));
        let f = Status::Failing {
            why: "missing Manage Server",
            since_ms: 0,
        };
        assert!(notes(true, &f, 0)[0].contains("missing Manage Server"));
        assert!(notes(true, &ok, 4)[0].starts_with("4 joins in the last 7 days"));
    }

    /// The default text of a reply.
    fn said((key, vars): Reply) -> String {
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::reply::said_from(key, None, &vars).text
    }

    #[test]
    fn caps_and_ranges() {
        let now = 10 * LINK_GAP_MS;
        assert!(capped(0, None, now).is_none());
        assert_eq!(
            said(capped(3, None, now).unwrap()),
            "You already have 3 working links here. Use one of them, or wait until one expires."
        );
        assert!(capped(2, Some(now - LINK_GAP_MS + 1), now).is_some());
        assert!(capped(2, Some(now - LINK_GAP_MS), now).is_none());
        // Max 0: never / no limit allowed, Discord's ranges hold.
        assert!(out_of_range(0, 0, 0, 0).is_none() && out_of_range(604_800, 100, 0, 0).is_none());
        assert!(out_of_range(604_801, 0, 0, 0).is_some() && out_of_range(60, 101, 0, 0).is_some());
        assert!(out_of_range(-1, 0, 0, 0).is_some() && out_of_range(60, -1, 0, 0).is_some());
        // Any other max is a cap, and never / no limit are past it.
        assert!(out_of_range(3_600, 5, 86_400, 10).is_none());
        assert!(
            out_of_range(0, 5, 86_400, 10).is_some()
                && out_of_range(90_000, 5, 86_400, 10).is_some()
        );
        assert!(
            out_of_range(60, 0, 86_400, 10).is_some() && out_of_range(60, 11, 86_400, 10).is_some()
        );
    }

    #[test]
    fn invite_permission_follows_roles_and_overwrites() {
        let (gid, user, owner) = (Id::new(1), Id::new(2), Id::new(3));
        let mod_role = Id::new(4);
        let need = NEED.bits();
        let perms = HashMap::from([
            (gid.cast(), Permissions::VIEW_CHANNEL.bits()),
            (mod_role, need),
        ]);
        let ow = |id: u64, kind, allow: Permissions, deny: Permissions| PermissionOverwrite {
            allow,
            deny,
            id: Id::new(id),
            kind,
        };
        let text = ChannelType::GuildText;
        let can = |roles: &[Id<RoleMarker>], ows: &[PermissionOverwrite]| {
            can_invite(gid, user, owner, &perms, roles, text, ows)
        };
        assert!(!can(&[], &[]));
        assert!(can(&[mod_role], &[]));
        // A channel deny on the member's role takes it away; a member allow gives it back.
        let deny = ow(
            4,
            PermissionOverwriteType::Role,
            Permissions::empty(),
            Permissions::CREATE_INVITE,
        );
        assert!(!can(&[mod_role], &[deny]));
        let allow = ow(
            2,
            PermissionOverwriteType::Member,
            Permissions::CREATE_INVITE,
            Permissions::empty(),
        );
        assert!(can(&[mod_role], &[deny, allow]));
        assert!(can(&[], &[allow]));
        // Hidden channel: no invite even with the bit.
        let hide = ow(
            1,
            PermissionOverwriteType::Role,
            Permissions::empty(),
            Permissions::VIEW_CHANNEL,
        );
        assert!(!can(&[mod_role], &[hide]));
        // The owner can always.
        assert!(can_invite(gid, owner, owner, &perms, &[], text, &[]));
    }

    use crate::test_support::{ME, T};
    use serde_json::json;

    fn desc(r: &(String, Option<Embed>)) -> String {
        let e =
            r.1.as_ref()
                .and_then(|e| e.description.clone())
                .unwrap_or_default();
        format!("{}{e}", r.0)
    }

    #[tokio::test]
    async fn a_banned_members_links_are_deleted() {
        let Some(t) = T::new(294).await else { return };
        let gid = t.g(0);
        let g = gid.get() as i64;
        let (banned, other) = (t.base * 10 + 1, t.base * 10 + 2);
        for (code, owner) in [("b1", banned), ("b2", banned), ("o1", other)] {
            store::save_link(&t.app.pool, g, code, owner as i64, 5, 0, None)
                .await
                .unwrap();
        }
        revoke_links(&t.app, gid, Id::new(banned)).await;
        assert_eq!(
            t.calls("DELETE /invites/b1") + t.calls("DELETE /invites/b2"),
            2
        );
        assert_eq!(t.calls("DELETE /invites/o1"), 0);
        assert!(store::live_links(&t.app.pool, g, banned as i64)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store::live_links(&t.app.pool, g, other as i64)
                .await
                .unwrap()
                .len(),
            1
        );
        t.cleanup().await;
    }

    #[tokio::test]
    async fn general_slash_follows_use_slash_and_the_cooldown() {
        let Some(t) = T::new(293).await else { return };
        let owner = t.owner();
        t.server_config(0, owner, json!({})).await;
        t.server_config(1, owner, json!({"misc": {"use_slash": false}}))
            .await;
        let member = Id::new(t.base * 10 + 1);
        let about = |i: u64, who: Id<UserMarker>| {
            crate::general::slash(&t.app, t.g(i), who, None, vec!["about".into()])
        };
        // Without a permit: the first call answers, a quick second one is cooled down.
        assert_ne!(about(0, member).await.unwrap().1.len(), 0);
        assert!(about(0, member).await.unwrap().0.contains("Slow down"));
        // Slash off: refused, but the owner still can.
        assert!(about(1, member).await.unwrap().0.contains("disabled"));
        assert_ne!(about(1, Id::new(owner)).await.unwrap().1.len(), 0);
        t.cleanup().await;
    }

    /// Stats for anyone, `who` and `status` only with View Invites, the code only in slash,
    /// and a private leaderboard only for permits.
    #[tokio::test]
    async fn replies_follow_the_gates() {
        let Some(t) = T::new(290).await else { return };
        let (gid, owner) = (t.g(0), t.owner());
        t.server_config(0, owner, json!({"misc": {"invites_public_top": false}}))
            .await;
        let (g, inviter, joiner) = (gid.get() as i64, t.base * 10 + 1, t.base * 10 + 2);
        let join = store::Join {
            guild_id: g,
            user_id: joiner as i64,
            joined_at_ms: 1_700_000_000_000,
            code: Some("secret1"),
            inviter_id: Some(inviter as i64),
            how: store::How::Single,
            reason: None,
        };
        store::record_joins(&t.app.pool, &[join]).await.unwrap();
        let run = |level: Level, slash: bool, args: &'static [&'static str]| {
            let app = &t.app;
            async move {
                run(app, gid, Id::new(inviter), &level, None, slash, args)
                    .await
                    .unwrap()
            }
        };
        let show = desc(&run(Level::None, false, &[]).await);
        assert!(
            show.contains("**Invited:** `1`") && show.contains("**Still here:** `1`"),
            "{show}"
        );
        assert!(desc(&run(Level::None, true, &["status"]).await).contains("View Invites"));
        assert!(
            desc(&run(Level::None, false, &["top"]).await).contains("Only members with a permit")
        );
        let top = desc(&run(Level::Owner, false, &["top"]).await);
        assert!(top.contains(&format!("`1.` <@{inviter}>: `1`")), "{top}");
        t.cleanup().await;
    }

    #[tokio::test]
    async fn who_shows_the_code_only_in_slash() {
        let Some(t) = T::new(291).await else { return };
        let (gid, owner) = (t.g(0), t.owner());
        t.server_config(0, owner, json!({})).await;
        let join = store::Join {
            guild_id: gid.get() as i64,
            user_id: 5_000,
            joined_at_ms: 1_700_000_000_000,
            code: Some("secret1"),
            inviter_id: Some(6_000),
            how: store::How::Group,
            reason: None,
        };
        store::record_joins(&t.app.pool, &[join]).await.unwrap();
        let who = |level: Level, slash: bool| {
            let app = &t.app;
            async move {
                desc(
                    &run(
                        app,
                        gid,
                        Id::new(7_000),
                        &level,
                        None,
                        slash,
                        &["who", "<@5000>"],
                    )
                    .await
                    .unwrap(),
                )
            }
        };
        assert!(who(Level::None, true).await.contains("View Invites"));
        let public = who(Level::TrustedAdmin, false).await;
        assert!(
            public.contains("<@6000>") && public.contains("Matched with other joins"),
            "{public}"
        );
        assert!(!public.contains("secret1"), "{public}");
        assert!(who(Level::TrustedAdmin, true).await.contains("`secret1`"));
        // Tracking not working: create refuses before anything else.
        let made = run(
            &t.app,
            gid,
            Id::new(5_000),
            &Level::Owner,
            None,
            true,
            &["create"],
        )
        .await
        .unwrap();
        assert!(made.0.contains("can't count"), "{}", made.0);
        assert_eq!(t.calls("/invites "), 0);
        t.cleanup().await;
    }

    /// The checks after tracking and the join lock: channel, member and bot permissions, the
    /// claim, the cooldown, saving, and the prefix DM that fails.
    #[tokio::test]
    async fn make_checks_saves_and_hands_out_the_link() {
        let Some(t) = T::new(292).await else { return };
        let (gid, owner) = (t.g(0), t.owner());
        t.server_config(0, owner, json!({})).await;
        let g = gid.get();
        let (staff, member, other) = (t.base * 10 + 1, t.base * 10 + 2, t.base * 10 + 3);
        let (here, elsewhere) = (t.base * 10 + 5, t.base * 10 + 6);
        t.set(|m| {
            // @everyone may view; role 50 may also create invites.
            m.roles
                .insert(g, vec![(g, false, 1024, 0), (50, false, 1025, 1)]);
            for (u, r) in [
                (staff, vec![50]),
                (member, vec![]),
                (other, vec![50]),
                (ME, vec![50]),
            ] {
                m.members.insert((g, u), r);
            }
            m.overwrites.insert(here, vec![]);
            m.channel_guild.insert(here, g);
            m.overwrites.insert(elsewhere, vec![]);
            m.channel_guild.insert(elsewhere, g + 1);
        });
        t.app.invalidate_guild(gid);
        let make = |u: u64, c: u64, slash: bool| {
            let app = &t.app;
            async move {
                said(
                    make(app, gid, Id::new(u), Id::new(c), 3600, 5, slash)
                        .await
                        .unwrap(),
                )
            }
        };
        assert!(make(member, here, true)
            .await
            .contains("You need View Channel"));
        assert!(make(staff, elsewhere, true)
            .await
            .contains("of this server"));
        // A claim held by a running create refuses a second one.
        t.app.invite_claims.lock().unwrap().insert((g, staff));
        assert!(make(staff, here, true).await.contains("still being made"));
        t.app.invite_claims.lock().unwrap().remove(&(g, staff));
        let link = make(staff, here, true).await;
        assert!(
            link.contains("https://discord.gg/c1") && link.contains("for 5 uses"),
            "{link}"
        );
        assert!(
            t.app.invite_claims.lock().unwrap().is_empty(),
            "claim freed"
        );
        let post = t
            .mock
            .lock()
            .unwrap()
            .log
            .iter()
            .find(|l| l.contains("/invites "))
            .cloned()
            .unwrap();
        assert!(
            post.contains(r#""max_age":3600"#) && post.contains(r#""max_uses":5"#),
            "{post}"
        );
        assert!(
            post.contains(r#""unique":true"#) && post.contains(r#""temporary":false"#),
            "{post}"
        );
        assert!(!post.contains("role") && !post.contains("target"), "{post}");
        let links = store::live_links(&t.app.pool, g as i64, staff as i64)
            .await
            .unwrap();
        assert_eq!(links.len(), 1);
        assert!(make(staff, here, true)
            .await
            .contains("You can make a new link"));
        // Saving fails (the code is taken): the invite is deleted and no link is given.
        store::save_link(&t.app.pool, g as i64, "c2", 1, here as i64, 0, None)
            .await
            .unwrap();
        let failed = make(other, here, true).await;
        assert!(
            failed.contains("Saving the link failed") && !failed.contains("c2"),
            "{failed}"
        );
        assert_eq!(t.calls("DELETE /invites/c2"), 1);
        // Prefix: a closed DM removes the link; it takes no slot, but the 15 minute gap holds.
        t.set(|m| m.fail_message = true);
        let dm = make(other, here, false).await;
        assert!(
            dm.contains("Use `/invites create`") && !dm.contains("discord.gg"),
            "{dm}"
        );
        assert_eq!(t.calls("DELETE /invites/c3"), 1);
        assert!(store::last_link_ms(&t.app.pool, g as i64, other as i64)
            .await
            .unwrap()
            .is_some());
        assert!(store::live_links(&t.app.pool, g as i64, other as i64)
            .await
            .unwrap()
            .is_empty());
        let back = "UPDATE invite_links SET created_at = created_at - interval '1 hour' WHERE guild_id = $1";
        sqlx::query(back)
            .bind(g as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        t.set(|m| m.fail_message = false);
        assert_eq!(make(other, here, false).await, "I sent you the link by DM");
        sqlx::query(back)
            .bind(g as i64)
            .execute(&t.app.pool)
            .await
            .unwrap();
        // The bot without Create Invite: refused.
        t.set(|m| drop(m.members.insert((g, ME), vec![])));
        t.app.invalidate_guild(gid);
        assert!(make(other, here, true).await.contains("Brim needs"));
        t.cleanup().await;
    }
}
