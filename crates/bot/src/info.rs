//! Member, role and server info embeds behind `info`, `roleinfo` and `serverinfo`.
use anyhow::Result;
use brim_core::{cases, permits::Level};
use brim_logging::embed::{self, Log, ARROW, NO, YES};
use brim_logging::emoji::{CRITICAL, SHIELD, TIP, WARNING};
use twilight_model::{
    channel::message::embed::EmbedThumbnail,
    channel::message::{Component, Embed},
    id::{
        marker::{GuildMarker, UserMarker},
        Id,
    },
    user::{PremiumType, User},
};

use crate::{
    app::{now_secs, App},
    reply, settings,
};

fn created_secs(id: u64) -> i64 {
    (((id >> 22) + 1_420_070_400_000) / 1000) as i64
}

fn yes(b: bool) -> String {
    if b { YES } else { NO }.to_string()
}

/// Names of the dangerous Discord permissions in `perms`.
fn dangerous(perms: u64) -> String {
    let names = brim_antinuke::dangerous_names(perms);
    if names.is_empty() {
        NO.into()
    } else {
        names.join(", ")
    }
}

fn level_name(l: &Level) -> String {
    match l {
        Level::None => "None".into(),
        Level::Custom { rank, .. } => format!("Permit (rank {rank})"),
        Level::TrustedAdmin => "Trusted Admin".into(),
        Level::ExtraOwner => "Extra Owner".into(),
        Level::Owner => "Owner".into(),
    }
}

/// `full`: staff, or the member asking about themselves. Others see only account age and join
/// date: whitelists, points, heat and permit rank tell a raider who bypasses what.
pub(crate) async fn user_info(
    app: &App,
    gid: Id<GuildMarker>,
    uid: Id<UserMarker>,
    full: bool,
) -> Result<Embed> {
    let cfg = app.cfg(gid.get()).await?;
    let info = app.guild_info(gid).await?;
    let member = crate::http_err::member(&app.http, gid, uid).await?;
    let user = match &member {
        Some(m) => m.user.clone(),
        None => app.http.user(uid).await?.model().await?,
    };
    let created = created_secs(uid.get());
    let mut v = vec![
        ("member.username", user.name.clone()),
        ("member", embed::user(uid.get())),
        ("member.created_at", format!("<t:{created}:R>")),
    ];
    let Some(m) = member else {
        v.push(("in_server", NO.to_string()));
        if !full {
            return Ok(user_answer(app, gid, &user, &v).await);
        }
        let banned = app.http.ban(gid, uid).await.is_ok();
        let held: Option<(i64,)> =
            sqlx::query_as("SELECT 1::bigint FROM quarantines WHERE guild_id=$1 AND user_id=$2")
                .bind(gid.get() as i64)
                .bind(uid.get() as i64)
                .fetch_optional(&app.pool)
                .await?;
        let timed_out: Option<(i64,)> = sqlx::query_as(
            "SELECT 1::bigint FROM cases c WHERE guild_id=$1 AND user_id=$2 AND kind='timeout' AND active
             AND created_at + make_interval(secs => duration_s) > now()
             AND NOT EXISTS (SELECT 1 FROM cases n WHERE n.guild_id=c.guild_id AND n.user_id=c.user_id
                             AND n.kind='untimeout' AND n.case_no > c.case_no)",
        )
        .bind(gid.get() as i64)
        .bind(uid.get() as i64)
        .fetch_optional(&app.pool)
        .await?;
        v.extend([
            ("left_timed_out", yes(timed_out.is_some())),
            ("left_quarantined", yes(held.is_some())),
            ("banned", yes(banned)),
            ("is_bot", yes(user.bot)),
        ]);
        return Ok(user_answer(app, gid, &user, &v).await);
    };
    if let Some(j) = m.joined_at {
        v.push(("member.joined_at", format!("<t:{}:R>", j.as_secs())));
    }
    if !full {
        return Ok(user_answer(app, gid, &user, &v).await);
    }
    let young = now_secs() - created < cfg.joingate.min_age_ms() / 1000;
    let suspicious = format!(
        "{} No Avatar\n{} Account Younger Than {}",
        yes(user.avatar.is_none()),
        yes(young),
        brim_logging::embed::duration(cfg.joingate.min_age_ms() / 1000)
    );
    let points = cases::warn_points(&app.pool, gid.get() as i64, uid.get() as i64).await?;
    let wl = &cfg.whitelist;
    let on = |l: &brim_core::sections::WhitelistList| {
        l.users.contains(&uid) || m.roles.iter().any(|r| l.roles.contains(r))
    };
    let whitelists = format!(
        "{} Spam\n{} Ping\n{} Advertising\n{} @everyone\n{} Quarantine",
        yes(on(&wl.spam)),
        yes(on(&wl.mentions)),
        yes(on(&wl.invites)),
        yes(on(&wl.everyone)),
        yes(cfg.antinuke.quarantine_whitelist.contains(&uid))
    );
    let perms = m
        .roles
        .iter()
        .chain([&gid.cast()])
        .filter_map(|r| info.role_perms.get(r))
        .fold(0, |a, p| a | p);
    let level = app.level(gid, info.owner, uid).await?;
    let now = now_secs() * 1000;
    let st = brim_automod::store::load_strikes(&app.pool, gid.get(), uid.get()).await?;
    let strikes = brim_automod::strike::active(&st, &cfg.automod.strike, now);
    let heat = app
        .heat
        .lock()
        .unwrap()
        .get(&(gid.get(), uid.get()))
        .map_or(0.0, |(h, _)| h.decayed(now, &cfg.automod.heat));
    let roles = crate::hierarchy::ranked(gid, app.http.roles(gid).await?.models().await?);
    let color = roles
        .iter()
        .filter(|r| m.roles.contains(&r.id) && r.colors.primary_color != 0)
        .max_by_key(|r| r.position)
        .map_or(0, |r| r.colors.primary_color);
    let badges: Vec<String> = user
        .public_flags
        .map(|f| f.iter_names().map(|(n, _)| title_case(n)).collect())
        .unwrap_or_default();
    // Listing webhooks needs Manage Webhooks; without it the field just shows none.
    let webhooks: Vec<String> = match crate::http_err::best_effort(
        app.http.guild_webhooks(gid).await,
        "list webhooks for info",
    ) {
        Some(r) => r
            .models()
            .await?
            .into_iter()
            .filter(|w| w.user.as_ref().is_some_and(|u| u.id == uid))
            .filter_map(|w| w.name)
            .collect(),
        None => vec![],
    };
    let list = |v: Vec<String>| {
        if v.is_empty() {
            NO.to_string()
        } else {
            v.join(", ")
        }
    };
    let accessories = format!(
        "{} Avatar Decoration\n{} Banner\n{} Nitro\n{} Server Avatar",
        yes(user.avatar_decoration_data.is_some()),
        yes(user.banner.is_some()),
        yes(user.premium_type.is_some_and(|t| t != PremiumType::None)),
        yes(m.avatar.is_some())
    );
    v.extend([
        ("color", format!("#{color:06x}")),
        ("badges", list(badges)),
        ("suspicious", suspicious),
        ("whitelist", whitelists),
        ("warn_points", format!("`{points}`")),
        ("strikes", format!("`{strikes}`")),
        (
            "heat",
            format!("`{:.0}%`", heat / cfg.automod.heat.max.max(1.0) * 100.0),
        ),
        ("dangerous", dangerous(perms)),
        ("permits", level_name(&level)),
        ("accessories", accessories),
        (
            "member_roles",
            list(m.roles.iter().map(|r| format!("<@&{r}>")).collect()),
        ),
        ("webhooks", list(webhooks)),
    ]);
    Ok(user_answer(app, gid, &user, &v).await)
}

/// The `info` answer (`reply.info.user`) with the user's avatar, unless the edited version has a
/// picture of its own.
async fn user_answer(
    app: &App,
    gid: Id<GuildMarker>,
    user: &User,
    vars: &[(&str, String)],
) -> Embed {
    with_avatar(reply::answer(app, gid, "reply.info.user", vars).await, user)
}

pub(crate) fn with_avatar(mut e: Embed, user: &User) -> Embed {
    e.thumbnail.get_or_insert_with(|| EmbedThumbnail {
        url: embed::avatar(user),
        height: None,
        proxy_url: None,
        width: None,
    });
    e
}

/// `HYPESQUAD_ONLINE_HOUSE_1` -> `Hypesquad Online House 1`.
fn title_case(flag: &str) -> String {
    flag.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| {
                f.to_string() + c.as_str().to_lowercase().as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) async fn role_info(app: &App, gid: Id<GuildMarker>, rid: u64) -> Result<Option<Embed>> {
    let roles = crate::hierarchy::ranked(gid, app.http.roles(gid).await?.models().await?);
    let Some(r) = roles.into_iter().find(|r| r.id.get() == rid) else {
        return Ok(None);
    };
    let cfg = app.cfg(gid.get()).await?;
    let wl = &cfg.whitelist;
    let main = cfg.lockdown.main_roles.contains(&r.id);
    let whitelists = format!(
        "{} Spam\n{} Ping\n{} Advertising\n{} @everyone",
        yes(wl.spam.roles.contains(&r.id)),
        yes(wl.mentions.roles.contains(&r.id)),
        yes(wl.invites.roles.contains(&r.id)),
        yes(wl.everyone.roles.contains(&r.id))
    );
    let details = format!(
        "{} Managed\n{} Mentionable\n{} Hoisted\n{} Main Role",
        yes(r.managed),
        yes(r.mentionable),
        yes(r.hoist),
        yes(main)
    );
    let vars = [
        ("role.name", r.name.clone()),
        ("role", embed::role(rid)),
        ("role.created_at", format!("<t:{}:R>", created_secs(rid))),
        ("position", r.position.to_string()),
        ("role_details", details),
        ("whitelist", whitelists),
        ("dangerous", dangerous(r.permissions.bits())),
    ];
    Ok(Some(
        reply::answer(app, gid, "reply.info.role", &vars).await,
    ))
}

const ROLES_PER_PAGE: usize = 25;

/// Page 0 is the server's facts, the others list its roles top down. Returns the page count too.
pub async fn server_page(app: &App, gid: Id<GuildMarker>, page: usize) -> Result<(Embed, usize)> {
    let g = app.http.guild(gid).with_counts(true).await?.model().await?;
    let pages = 1 + g.roles.len().div_ceil(ROLES_PER_PAGE);
    let page = page.min(pages - 1);
    if page > 0 {
        let mut roles = crate::hierarchy::ranked(gid, g.roles.clone());
        roles.sort_by_key(|r| std::cmp::Reverse(r.position));
        let list: Vec<String> = roles
            .iter()
            .skip((page - 1) * ROLES_PER_PAGE)
            .take(ROLES_PER_PAGE)
            .map(|r| format!("{ARROW} {}", embed::role(r.id.get())))
            .collect();
        let vars = [
            ("server.name", g.name.clone()),
            ("role_list", list.join("\n")),
            ("page_no", (page + 1).to_string()),
            ("pages", pages.to_string()),
        ];
        let e = reply::answer(app, gid, "reply.info.server_roles", &vars).await;
        return Ok((e, pages));
    }
    let channels = app.http.guild_channels(gid).await?.models().await?.len();
    let vars = [
        ("server.name", g.name.clone()),
        ("owner", embed::user(g.owner_id.get())),
        (
            "server.created_at",
            format!("<t:{}:R>", created_secs(gid.get())),
        ),
        (
            "members",
            g.approximate_member_count.unwrap_or_default().to_string(),
        ),
        ("role_count", g.roles.len().to_string()),
        ("channel_count", channels.to_string()),
        (
            "boosts",
            g.premium_subscription_count.unwrap_or_default().to_string(),
        ),
        ("boost_tier", u8::from(g.premium_tier).to_string()),
        ("pages", pages.to_string()),
    ];
    let e = reply::answer(app, gid, "reply.info.server", &vars).await;
    Ok((e, pages))
}

pub(crate) async fn server_info(
    app: &App,
    gid: Id<GuildMarker>,
    invoker: Id<UserMarker>,
) -> Result<(String, Option<Embed>, Vec<Component>)> {
    let (e, pages) = server_page(app, gid, 0).await?;
    Ok((
        String::new(),
        Some(e),
        vec![settings::page_row(invoker.get(), "ginfo", 0, pages)],
    ))
}

/// `security`: the security score of the dashboard Overview; its fixes run there.
pub async fn security_page(app: &App, gid: Id<GuildMarker>) -> Result<Embed> {
    use brim_antinuke::security::{checks, score, GuildSafety, Setup, Severity};
    let g = app.http.guild(gid).await?.model().await?;
    let cfg = app.cfg(gid.get()).await?;
    let safety = GuildSafety {
        id: gid.get(),
        mfa: u8::from(g.mfa_level),
        verification: u8::from(g.verification_level),
        content_filter: u8::from(g.explicit_content_filter),
        notifications: u8::from(g.default_message_notifications),
    };
    let extra: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM extra_owners WHERE guild_id=$1")
        .bind(gid.get() as i64)
        .fetch_all(&app.pool)
        .await?;
    let me = app.http.guild_member(gid, app.me).await?.model().await?;
    let staff = brim_core::permits::all_staff_roles(&app.pool, gid.get() as i64).await?;
    let setup = Setup {
        quarantine_role: cfg.antinuke.quarantine_role.map(|r| r.get()),
        log_fallback: cfg.logging.fallback.is_some(),
        unwhitelisted_staff: brim_core::permits::unwhitelisted_staff(&app.pool, gid.get() as i64)
            .await?,
        bot_roles: me.roles.iter().map(|r| r.get()).collect(),
        mod_log: cfg
            .logging
            .routes
            .contains_key(&brim_logging::LogKind::ModCommand),
        main_roles: cfg.lockdown.main_roles.iter().map(|r| r.get()).collect(),
        mod_roles: brim_core::permits::mod_roles(&app.pool, gid.get() as i64)
            .await?
            .into_iter()
            .map(|r| r as u64)
            .collect(),
        modules: vec![
            ("Anti-nuke", cfg.antinuke.enabled),
            ("Join gate", cfg.joingate.enabled),
            (
                "Join raid",
                cfg.joinraid.as_ref().is_some_and(|r| r.enabled),
            ),
            ("Verification", cfg.verification.enabled),
        ],
        trusted: extra
            .into_iter()
            .map(|u| u as u64)
            .chain([g.owner_id.get(), app.me.get()])
            .collect(),
        log_channels: cfg.log_channels().iter().map(|c| c.get()).collect(),
        bot: app.me.get(),
        verification_channel: cfg.verification.channel.map(|c| c.get()),
        verification_roles: cfg.verification.roles.iter().map(|r| r.get()).collect(),
        staff_roles: staff.iter().map(|&r| r as u64).collect(),
    };
    let bot_top = app.guild_info(gid).await?.bot_top;
    let channels: Vec<_> = app
        .http
        .guild_channels(gid)
        .await?
        .models()
        .await?
        .into_iter()
        .map(crate::backup::channel_img)
        .collect();
    let roles: Vec<_> = crate::hierarchy::ranked(gid, g.roles)
        .into_iter()
        .map(crate::backup::role_img)
        .collect();
    let mut rows = checks(&safety, &roles, &channels, bot_top, &setup);
    rows.extend(brim_antinuke::security::staff_mention_note(&roles, &staff));
    let points = score(&rows);
    // Worst first; 12 rows with details stay under the embed's 4096 characters.
    rows.sort_by_key(|r| std::cmp::Reverse(r.severity));
    let hidden = rows.len().saturating_sub(12);
    rows.truncate(12);
    let color = match points {
        90.. => embed::GREEN,
        60..90 => embed::ORANGE,
        _ => embed::RED,
    };
    let mut e = Log::new(&format!("Security Score: {points}%"), color);
    for r in &rows {
        let icon = match r.severity {
            Severity::Ok => SHIELD,
            Severity::Warn => WARNING,
            Severity::Bad => CRITICAL,
        };
        e = e.text(&format!("{icon} **{}**", r.title));
        if r.severity != Severity::Ok {
            e = e.text(&format!("{ARROW} {}", r.detail));
        }
    }
    if hidden > 0 {
        e = e.text(&format!("{ARROW} +{hidden} more rows on the dashboard."));
    }
    if rows.iter().any(|r| r.fix.is_some()) {
        let link = crate::general::dashboard()
            .map_or("the dashboard".into(), |u| format!("[the dashboard]({u})"));
        e = e.text(&format!(
            "{TIP} Fix the marked rows with one click on the Overview page of {link}."
        ));
    }
    Ok(e.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_names_read_as_words() {
        assert_eq!(
            title_case("HYPESQUAD_ONLINE_HOUSE_1"),
            "Hypesquad Online House 1"
        );
        assert_eq!(title_case("VERIFIED_BOT"), "Verified Bot");
    }

    #[test]
    fn dangerous_names() {
        assert_eq!(dangerous(0), NO.to_string());
        assert_eq!(
            dangerous((1 << 3) | (1 << 2) | (1 << 10)),
            "Administrator, Ban Members"
        );
    }
}
