//! Member verification: a button (or image captcha) in one channel hands out the main roles.
//! Web mode hands the member to the dashboard's /verify page, which queues a `verify_grant` job.

pub mod deleted;
pub mod given;

use anyhow::Result;
use brim_automod::join::{
    account_created_ms, effective_mode, must_verify, FailAction, VerifyCfg, VerifyMode,
};
use brim_core::jobs;
use brim_core::permits::{self, Level};
use brim_core::sections::{VERIFY_ALLOW, VERIFY_DENY};
use std::collections::HashMap;

use brim_logging::{
    embed::{ALERT, DONE, GREEN, NO, YES},
    Log, LogKind,
};
use twilight_http::request::AuditLogReason;
use twilight_model::{
    application::interaction::{modal::ModalInteractionComponent, Interaction, InteractionData},
    channel::message::{
        component::{ActionRow, Button, ButtonStyle, Component, TextInput, TextInputStyle},
        Embed, MessageFlags,
    },
    channel::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
    gateway::payload::incoming::MemberUpdate,
    guild::Permissions,
    http::{
        attachment::Attachment,
        interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    },
    id::{
        marker::{ChannelMarker, GuildMarker, RoleMarker, UserMarker},
        Id,
    },
    user::User,
};
use twilight_util::builder::embed::{EmbedBuilder, EmbedFooterBuilder, ImageSource};

use crate::{
    app::{now_secs, App},
    events::send_log,
};

pub use brim_core::sections::VerificationCfg;

use brim_logging::embed::{
    verify_panel, VERIFY_ACCEPT as ACCEPT, VERIFY_HELP as HELP, VERIFY_START as START,
};
// The enter and code ids end in the guild id: a DM interaction has no guild of its own.
pub(crate) const ENTER: &str = "verify:enter:";
const CODE: &str = "verify:code:";

#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Answers with the short reply entry of this key.
    Say(&'static str),
    Grant,
    Web,
    Captcha,
    Simple,
    Dm,
}

/// "I accept the rules" never starts a challenge and never passes one: when this member must solve
/// one (the captcha is on, a raid raised the mode, or the Join Gate finds them suspicious), it
/// sends them to the Verify button.
const ACCEPT_REFUSED: &str = "reply.verify.press_verify";

fn accept_step(step: Step) -> Step {
    match step {
        Step::Grant | Step::Say(_) => step,
        _ => Step::Say(ACCEPT_REFUSED),
    }
}

/// What a click on the verify button leads to.
fn decide(cfg: &VerificationCfg, raid_active: bool, suspicious: bool, has_role: bool) -> Step {
    if !cfg.enabled || cfg.roles.is_empty() {
        return Step::Say("reply.verify.not_set_up");
    }
    if has_role {
        return Step::Say("reply.verify.already_verified");
    }
    if !must_verify(&cfg.verify, suspicious, false) {
        return Step::Grant;
    }
    match effective_mode(&cfg.verify, raid_active) {
        VerifyMode::Button => Step::Grant,
        VerifyMode::Web => Step::Web,
        VerifyMode::SimpleCaptcha => Step::Simple,
        VerifyMode::Dm => Step::Dm,
        VerifyMode::Captcha => Step::Captcha,
    }
}

/// The step for a member Discord flags as a spammer, when that differs from a Grant; None when
/// the flag cannot change the step, so no lookup is needed.
fn flagged_step(
    cfg: &VerificationCfg,
    raid: bool,
    suspicious: bool,
    has_role: bool,
) -> Option<Step> {
    (!suspicious && decide(cfg, raid, false, has_role) == Step::Grant)
        .then(|| decide(cfg, raid, true, has_role))
        .filter(|s| *s != Step::Grant)
}

/// How long a spammer flag answer is reused, and how many are kept.
const FLAG_TTL_MS: i64 = 600_000;
const FLAG_KEEP: usize = 10_000;

/// Stores a challenge; past `FLAG_KEEP` entries, expired ones (never answered) are dropped first.
fn remember_code(
    codes: &mut HashMap<(u64, u64), crate::app::Pending>,
    key: (u64, u64),
    entry: crate::app::Pending,
    now: i64,
) {
    if codes.len() >= FLAG_KEEP {
        codes.retain(|_, e| e.1 > now);
        // All still live (a huge raid): the raided server's go, not every server's (a new Verify click).
        if codes.len() >= FLAG_KEEP {
            crate::caps::drop_biggest(codes, |k| k.0);
        }
    }
    codes.insert(key, entry);
}

/// The cached answer for `key` if it is still fresh.
fn cached_flag(map: &HashMap<(u64, u64), (bool, i64)>, key: (u64, u64), now: i64) -> Option<bool> {
    map.get(&key)
        .filter(|(_, at)| now - at < FLAG_TTL_MS)
        .map(|e| e.0)
}

fn remember_flag(
    map: &mut HashMap<(u64, u64), (bool, i64)>,
    key: (u64, u64),
    flagged: bool,
    now: i64,
) {
    if map.len() >= FLAG_KEEP {
        map.retain(|_, (_, at)| now - *at < FLAG_TTL_MS);
        if map.len() >= FLAG_KEEP {
            crate::caps::drop_biggest(map, |k| k.0);
        }
    }
    map.insert(key, (flagged, now));
}

/// Discord's spammer flag on `user`, cached 10 minutes so repeated clicks cost no request. The
/// interaction must be answered within 3 s: one try, 1.5 s at most, and a timeout, an error or a
/// 429 counts as flagged (a captcha, never a free pass).
async fn spammer(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> bool {
    let (key, now) = ((gid.get(), user.get()), now_secs() * 1000);
    if let Some(f) = cached_flag(&app.spam_flags.lock().unwrap(), key, now) {
        return f;
    }
    let read = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        app.api.public_flags(user.get()),
    )
    .await;
    let flagged = !matches!(read, Ok(Ok(f)) if f & brim_automod::join::SPAMMER_FLAG == 0);
    if !matches!(read, Ok(Ok(_))) {
        tracing::warn!(%user, ?read, "spammer flag lookup failed, the member gets a challenge");
    }
    remember_flag(&mut app.spam_flags.lock().unwrap(), key, flagged, now);
    flagged
}

pub(crate) fn button(label: &str, id: &str) -> Component {
    brim_logging::embed::button(label, id, ButtonStyle::Primary)
}

pub(crate) fn row(c: Component) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![c],
    })
}

/// Posts the verify button into the configured channel (staff command `verifysetup`).
pub async fn setup(app: &App, gid: Id<GuildMarker>) -> Result<crate::reply::Said> {
    let say = |key| crate::reply::say(app, gid, key, &[]);
    let cfg = app.cfg(gid.get()).await?;
    let Some(ch) = cfg.verification.channel else {
        return Ok(say("reply.verify.set_channel").await);
    };
    // A stored id from before the save-time checks may name another server's channel.
    if app.http.channel(ch).await?.model().await?.guild_id != Some(gid) {
        return Ok(say("reply.verify.wrong_channel").await);
    }
    let stored = crate::reply::stored_msg(app, gid, "verify.panel").await;
    let (embed, buttons) = verify_panel(&guild_name(app, gid).await, stored.as_ref());
    app.http
        .create_message(ch)
        .embeds(&[embed])
        .components(&[buttons])
        .await?;
    let channel = format!("<#{ch}>");
    Ok(crate::reply::say(
        app,
        gid,
        "reply.verify.posted_button",
        &[("channel", &channel)],
    )
    .await)
}

/// The server's name, or "the server". At most 1.5 s: buttons call it before their answer, and an
/// interaction must be answered within 3 s.
pub(crate) async fn guild_name(app: &App, gid: Id<GuildMarker>) -> String {
    let name = async { anyhow::Ok(app.http.guild(gid).await?.model().await?.name) };
    tokio::time::timeout(std::time::Duration::from_millis(1500), name)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(|| "the server".into())
}

/// "2 minutes" wording for the verification period.
fn period(ms: i64) -> String {
    let s = ms / 1000;
    match s {
        s if s % 60 == 0 && s >= 60 => {
            format!("{} minute{}", s / 60, if s == 60 { "" } else { "s" })
        }
        s => format!("{s} seconds"),
    }
}

/// What the Help button shows; also the dashboard preview.
pub(crate) fn help_embed(cfg: &VerificationCfg, stored: Option<&serde_json::Value>) -> Embed {
    crate::reply::bot_embed("verify.help", stored, &[("steps", &help_steps(cfg))])
}

/// Steps shown by the Help button, adapted to the mode, the roles a pass gives and the time limit.
fn help_steps(cfg: &VerificationCfg) -> String {
    let v = &cfg.verify;
    let mut steps = vec!["Press the **Verify** button.".to_string()];
    steps.extend(match v.mode {
        VerifyMode::Button => vec!["You get access right away.".into()],
        VerifyMode::Web => {
            vec!["Open the link and finish the check on the website.".into()]
        }
        VerifyMode::SimpleCaptcha => vec![
            "Solve the question you are shown.".into(),
            "Press **Answer** and type the result.".into(),
        ],
        _ => vec![
            "Read the captcha image you are shown.".into(),
            "Press **Answer** and type the characters.".into(),
        ],
    });
    if !cfg.roles.is_empty() {
        let roles: Vec<_> = cfg.roles.iter().map(|r| format!("<@&{r}>")).collect();
        steps.push(format!("After you pass, you get: {}", roles.join(", ")));
    }
    if v.on_fail != FailAction::Nothing {
        steps.push(format!("Finish within {}.", period(v.time_limit_ms)));
    }
    let steps = steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("**{}.** {s}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    // The picture captcha's tips (setting Notes).
    let picture = !matches!(
        v.mode,
        VerifyMode::Button | VerifyMode::Web | VerifyMode::SimpleCaptcha
    );
    match picture && v.show_notes {
        true => steps + NOTES,
        false => steps,
    }
}

/// Gives a fresh joiner until the time limit to verify; see [`deadline`]. True when they must verify.
pub async fn on_join(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    suspicious: bool,
    is_bot: bool,
    timeout_until: Option<i64>,
    immune: Option<bool>,
) -> bool {
    let Ok(cfg) = app.cfg(gid.get()).await else {
        return false;
    };
    let v = &cfg.verification;
    if is_bot || !v.enabled || !must_verify(&v.verify, suspicious, false) {
        return false;
    }
    // Owner, EO and TA bypass verification; unread (None) keeps it (fails closed). The caller
    // read it from the join's own roles: no member fetch per join in a raid.
    if immune == Some(true) {
        return false;
    }
    if v.channel.is_none() {
        if let Err(e) = ensure_channel(app, gid).await {
            tracing::warn!(?e, "creating the verification channel failed");
        }
    }
    if v.verify.quarantine_on_join {
        if let Err(e) =
            crate::antinuke::quarantine_as(app, gid, user, None, crate::antinuke::Kind::Join).await
        {
            tracing::warn!(?e, "quarantining an unverified joiner failed");
        }
    }
    if v.verify.on_fail == FailAction::Nothing {
        return true;
    }
    // A timeout survives a kick or ban, so a member can come back still timed out.
    let now = now_secs();
    let (at, paused) = paused_deadline(
        now,
        now + v.verify.time_limit_ms / 1000,
        timeout_until,
        None,
    );
    if let Err(e) = jobs::enqueue(
        &app.pool,
        gid.get() as i64,
        DEADLINE,
        deadline_job(user, paused),
        at,
    )
    .await
    {
        tracing::warn!(?e, "scheduling the verification deadline failed");
    }
    true
}

const DEADLINE: &str = "verify_deadline";

fn deadline_job(user: Id<UserMarker>, paused_until: Option<i64>) -> serde_json::Value {
    serde_json::json!({ "user": user.get(), "paused_until": paused_until })
}

/// The time limit stands still while a member is timed out (they cannot press Verify).
/// `deadline` is when the job runs now and `paused_until` the timeout end it was last moved past.
/// Returns the new run time and the timeout end it now waits behind, if any.
fn paused_deadline(
    now: i64,
    deadline: i64,
    timeout_until: Option<i64>,
    paused_until: Option<i64>,
) -> (i64, Option<i64>) {
    // Time left: counted from the timeout end while paused, from now otherwise.
    let left = (deadline - paused_until.map_or(now, |p| p.max(now))).max(0);
    let active = timeout_until.filter(|&t| t > now);
    (active.unwrap_or(now) + left, active)
}

/// Moves a waiting verification deadline when a timeout starts, changes or is lifted early.
pub async fn on_member_update(app: &App, m: &MemberUpdate) -> Result<()> {
    let cfg = app.cfg(m.guild_id.get()).await?;
    let v = &cfg.verification;
    if !v.enabled
        || v.verify.on_fail == FailAction::Nothing
        || m.roles.iter().any(|r| v.roles.contains(r))
    {
        return Ok(());
    }
    let g = m.guild_id.get() as i64;
    let Some((id, run_at, payload)) =
        jobs::of_user(&app.pool, g, DEADLINE, m.user.id.get()).await?
    else {
        return Ok(());
    };
    let old = payload["paused_until"].as_i64();
    let timeout = m.communication_disabled_until.map(|t| t.as_secs());
    let (at, paused) = paused_deadline(now_secs(), run_at, timeout, old);
    if (at, paused) != (run_at, old) {
        jobs::reschedule(&app.pool, id, at, deadline_job(m.user.id, paused)).await?;
    }
    Ok(())
}

/// Held while the bot makes and saves a verification channel.
pub(crate) static CHANNEL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The quarantine role's overwrite in a verification channel (`verification_shape`).
pub(super) fn quarantine_shape(q: Id<RoleMarker>) -> PermissionOverwrite {
    PermissionOverwrite {
        id: q.cast(),
        kind: PermissionOverwriteType::Role,
        allow: Permissions::from_bits_truncate(VERIFY_ALLOW),
        deny: Permissions::from_bits_truncate(VERIFY_DENY),
    }
}

/// First join after setup with no verification channel (`s 12`): create one that unverified
/// members can read but verified ones do not see, save it and post the button.
async fn ensure_channel(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let _one_at_a_time = CHANNEL_LOCK.lock().await;
    let cfg = app.cfg(gid.get()).await?;
    let now = now_secs();
    let refused = REFUSED.lock().unwrap().get(&gid.get()).copied();
    if cfg.verification.channel.is_some() || refused.is_some_and(|at| now - at < 600) {
        return Ok(());
    }
    let role = |id: Id<RoleMarker>, allow: Permissions, deny: Permissions| PermissionOverwrite {
        id: id.cast(),
        kind: PermissionOverwriteType::Role,
        allow,
        deny,
    };
    let none = Permissions::empty();
    let read = Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY;
    let mut overwrites = vec![role(gid.cast(), read, Permissions::SEND_MESSAGES)];
    overwrites.extend(cfg.antinuke.quarantine_role.map(quarantine_shape));
    overwrites.extend(
        cfg.verification
            .roles
            .iter()
            .map(|&r| role(r, none, Permissions::VIEW_CHANNEL)),
    );
    let ch = app
        .http
        .create_guild_channel(gid, "verification")
        .permission_overwrites(&overwrites)
        .await?
        .model()
        .await?;
    // Unsaved, the next joiner would make another one: delete it and wait 10 minutes.
    if let Err(e) = save_channel(app, gid, ch.id).await {
        REFUSED.lock().unwrap().insert(gid.get(), now);
        if let Err(d) = app.http.delete_channel(ch.id).await {
            tracing::warn!(?d, "deleting an unsaved verification channel failed");
        }
        let text = format!("Brim made a verification channel for a new member but could not save it, so it deleted it again and tries again in 10 minutes: {e}. Fix the verification settings on the dashboard.");
        let log = Log::new("Verification Channel Not Saved", ALERT)
            .text(&text)
            .var("error", &e.to_string());
        if let Err(l) = send_log(app, gid.get(), LogKind::Verification, log).await {
            tracing::warn!(?l, "verification channel log failed");
        }
        crate::reply::dm_owner(app, gid, "dm.owner_alert", &text).await;
        return Err(e);
    }
    setup(app, gid).await.map(drop)
}

/// When saving a new verification channel last failed, per guild ([`ensure_channel`]).
static REFUSED: std::sync::Mutex<std::collections::BTreeMap<u64, i64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Saves `ch` as the verification channel and posts the button in it.
pub(super) async fn adopt(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>) -> Result<()> {
    save_channel(app, gid, ch).await?;
    setup(app, gid).await.map(drop)
}

/// An error when the save failed or the settings refused it.
async fn save_channel(app: &App, gid: Id<GuildMarker>, ch: Id<ChannelMarker>) -> Result<()> {
    let saved = crate::settings::save(app, gid, app.me.get(), &["verification"], |doc| {
        doc["verification"]["channel"] = serde_json::json!(ch.to_string());
        Ok(())
    })
    .await?;
    saved
        .map(drop)
        .map_err(|e| anyhow::anyhow!("the verification settings refused the channel ({e})"))
}

/// The dashboard page passed Turnstile and sign-in: hand out the roles if the server still asks for web verification.
/// `same_ip`: members who verified from the same IP in the last day (alt accounts, or a shared network).
pub async fn web_grant(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    same_ip: &[u64],
) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?;
    let v = &cfg.verification;
    if !v.enabled || v.verify.mode != VerifyMode::Web {
        return Ok(());
    }
    // A Discord error is retried by the job; only a member who left ends it.
    let Some(member) = crate::http_err::member(&app.http, gid, user).await? else {
        return Ok(());
    };
    if member.roles.iter().any(|r| v.roles.contains(r)) {
        return Ok(());
    }
    if !same_ip.is_empty() {
        // Only logged: families, schools and mobile networks share addresses too.
        let others = same_ip
            .iter()
            .map(|u| format!("<@{u}>"))
            .collect::<Vec<_>>();
        let log = alt_log(user.get(), &others.join(", "));
        if let Err(e) = send_log(app, gid.get(), LogKind::Verification, log).await {
            tracing::warn!(?e, "alt log failed");
        }
    }
    self_grant(app, gid, user, v).await
}

pub(crate) fn alt_log(user: u64, same_ip: &str) -> Log {
    Log::new("Possible Alt Account", ALERT)
        .user("Member", user)
        .line("Same IP as", same_ip)
        .var("member", &brim_logging::embed::user(user))
        .var("same_ip", same_ip)
}

/// The web page refused a member on a VPN, proxy or hosting network (logged at most every 10 minutes per
/// member). The IP is never in the log.
pub async fn web_blocked(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    list: &str,
) -> Result<()> {
    let why = match list {
        "Tor" => "Tor",
        "hosting network" => "Hosting network",
        _ => "VPN or proxy",
    };
    let log = Log::new("Verification Blocked", ALERT)
        .user("Member", user.get())
        .line("Reason", why)
        .var("member", &brim_logging::embed::user(user.get()))
        .var("reason", why);
    send_log(app, gid.get(), LogKind::Verification, log).await
}

/// [`grant`] for a member verifying themselves, or a verify permit verifying them by hand.
/// Someone in a real quarantine (anti-nuke, a rate limit, staff, the Raider List) must not get out
/// by passing verification; only a join one lifts. Read again here, right before the grant.
pub(crate) async fn self_grant(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    v: &VerificationCfg,
) -> Result<()> {
    let on_join = crate::antinuke::quarantine_kind(app, gid, user).await?;
    anyhow::ensure!(on_join != Some(false), QUARANTINED);
    grant(app, gid, user, v).await
}

/// [`self_grant`] refuses a member held by a quarantine that is not verification's.
const QUARANTINED: &str =
    "You are in quarantine for another reason, so verifying cannot release you; staff must release you.";

/// What the member reads when [`self_grant`] fails.
fn grant_failed(e: &anyhow::Error) -> String {
    if e.to_string() == QUARANTINED {
        return QUARANTINED.into();
    }
    tracing::warn!(?e, "verification grant failed");
    match e.downcast_ref::<twilight_http::Error>() {
        Some(_) => format!(
            "Verification failed: Discord refused to give the roles ({}). Tell the staff.",
            crate::http_err::why(e)
        ),
        None => "Verification failed because of an error in Brim. Try again in a minute; staff can see it in the bot's logs.".into(),
    }
}

/// The audit reason of a verification timeout.
pub(crate) const AUDIT_REASON: &str = "Did not verify within the time limit";

/// The audit log reason at the time limit, when `audit_reason` is on.
fn audit_reason(v: &VerifyCfg) -> Option<&'static str> {
    v.audit_reason.then_some(AUDIT_REASON)
}

/// The first time verification is on in a server (owner decision 2026-11-17), every member already
/// there gets the roles it gives, so nobody loses the channels it locks: only later joiners must
/// verify. Bots and quarantined members are skipped. One run per server at a time
/// (`verify_started.claimed_at`, taken again after [`CLAIM_STALE`] if a run died); the server
/// counts as granted only when every member got the roles, so a failed step never lets
/// [`sweep`] remove members who should have had them.
pub async fn on_saved(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?;
    let v = &cfg.verification;
    if !v.enabled || v.roles.is_empty() {
        return Ok(());
    }
    let claimed: Option<i64> = sqlx::query_scalar(
        "INSERT INTO verify_started (guild_id, claimed_at) VALUES ($1, now())
         ON CONFLICT (guild_id) DO UPDATE SET claimed_at = now()
         WHERE NOT verify_started.granted
           AND (verify_started.claimed_at IS NULL OR verify_started.claimed_at < now() - $2::interval)
         RETURNING guild_id",
    )
    .bind(gid.get() as i64)
    .bind(CLAIM_STALE)
    .fetch_optional(&app.pool)
    .await?;
    if claimed.is_none() {
        return Ok(());
    }
    let done = grant_existing(app, gid, v).await;
    // Released either way: a failed run is tried again at the next save or sweep.
    sqlx::query("UPDATE verify_started SET claimed_at = NULL, granted = $2 WHERE guild_id = $1")
        .bind(gid.get() as i64)
        .bind(matches!(done, Ok(true)))
        .execute(&app.pool)
        .await?;
    match done {
        Ok(true) => Ok(()),
        Ok(false) => anyhow::bail!("some existing members did not get the verified roles"),
        Err(e) => Err(e),
    }
}

/// A claim older than this is from a run that died, and is taken again.
const CLAIM_STALE: &str = "2 hours";

/// Gives the safe verification roles to every human, unquarantined member without them. True
/// when every grant went through.
async fn grant_existing(app: &App, gid: Id<GuildMarker>, v: &VerificationCfg) -> Result<bool> {
    let (roles, _) = safe_roles(app, gid, v).await?;
    if roles.is_empty() {
        return Ok(false);
    }
    let members = crate::permit_sync::all_members(app, gid).await?;
    let held = quarantined(app, gid).await?;
    let mut all = true;
    for m in members.iter().filter(|m| {
        !m.user.bot
            && !held.contains(&(m.user.id.get() as i64))
            && !m.roles.iter().any(|r| v.roles.contains(r))
    }) {
        given::note_verified(gid, m.user.id);
        for &role in &roles {
            let add = app.http.add_guild_member_role(gid, m.user.id, role);
            if let Err(e) = add.reason(STARTED_REASON).await {
                tracing::warn!(?e, %gid, user = %m.user.id, "giving an existing member the verified role failed");
                all = false;
            }
        }
    }
    Ok(all)
}

async fn quarantined(app: &App, gid: Id<GuildMarker>) -> Result<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM quarantines WHERE guild_id = $1")
            .bind(gid.get() as i64)
            .fetch_all(&app.pool)
            .await?,
    )
}

const STARTED_REASON: &str = "Verification turned on: members already here count as verified";

/// Starts the time limit for members who still lack the verified roles where verification was
/// on already (owner decision 2026-11-17: they lose access, get a DM and the time limit). Queued by the
/// dashboard after it makes channels Member-only. Runs only once the first-time grant is done
/// ([`on_saved`]); while one runs it waits. Skips bots, quarantined members, anyone with a
/// permit or Owner, Extra Owner or Trusted Admin, members who have a limit running, and servers
/// where only suspicious joiners must verify.
pub async fn sweep(app: &App, gid: Id<GuildMarker>) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?;
    let v = &cfg.verification;
    if !v.enabled
        || v.roles.is_empty()
        || v.verify.on_fail == FailAction::Nothing
        || !must_verify(&v.verify, false, false)
    {
        return Ok(());
    }
    on_saved(app, gid).await?;
    let state: Option<(bool, bool)> = sqlx::query_as(
        "SELECT granted, claimed_at IS NOT NULL FROM verify_started WHERE guild_id = $1",
    )
    .bind(gid.get() as i64)
    .fetch_optional(&app.pool)
    .await?;
    match state {
        Some((true, _)) => {}
        Some((false, true)) => {
            // Another run is still giving the roles: look again later.
            jobs::enqueue_once(&app.pool, gid.get() as i64, SWEEP, now_secs() + 600).await?;
            return Ok(());
        }
        _ => return Ok(()),
    }
    let members = crate::permit_sync::all_members(app, gid).await?;
    let held = quarantined(app, gid).await?;
    let running: Vec<i64> = sqlx::query_scalar(
        "SELECT (payload->>'user')::bigint FROM scheduled_jobs WHERE guild_id = $1 AND kind = $2",
    )
    .bind(gid.get() as i64)
    .bind(DEADLINE)
    .fetch_all(&app.pool)
    .await?;
    let owner = app.guild_info(gid).await?.owner;
    let server = guild_name(app, gid).await;
    let now = now_secs();
    for m in &members {
        let id = m.user.id.get() as i64;
        if m.user.bot
            || held.contains(&id)
            || running.contains(&id)
            || m.roles.iter().any(|r| v.roles.contains(r))
        {
            continue;
        }
        let level =
            permits::resolve(&app.pool, gid.get() as i64, owner.get() as i64, id, false).await?;
        if level != Level::None {
            continue;
        }
        let timeout = m.communication_disabled_until.map(|t| t.as_secs());
        let (at, paused) = paused_deadline(now, now + v.verify.time_limit_ms / 1000, timeout, None);
        jobs::enqueue(
            &app.pool,
            gid.get() as i64,
            DEADLINE,
            deadline_job(m.user.id, paused),
            at,
        )
        .await?;
        let (server, time) = (server.to_string(), period(v.verify.time_limit_ms));
        let vars = [("server", server.as_str()), ("time", time.as_str())];
        crate::reply::dm_msg(app, Some(gid), m.user.id, "dm.verify_reminder", &vars, &[]).await;
    }
    Ok(())
}

const SWEEP: &str = "verify_sweep";

/// Applies the fail action to a member who never verified.
pub async fn deadline(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) -> Result<()> {
    let cfg = app.cfg(gid.get()).await?;
    let v = &cfg.verification;
    let Some(member) = crate::http_err::member(&app.http, gid, user).await? else {
        return Ok(());
    };
    if !v.enabled || member.roles.iter().any(|r| v.roles.contains(r)) {
        return Ok(());
    }
    // A failed read is an error, so the job tries again. Anyone with a permit is staff, and staff
    // are never removed for not verifying (permits, not Discord permissions, decide).
    let owner = app.guild_info(gid).await?.owner;
    if app.level(gid, owner, user).await? != Level::None {
        return Ok(());
    }
    let now = now_secs();
    // A job from an earlier join: the member left and came back, and the new join has its own
    // deadline. Acting here would count a time out that did not happen (security pass, 11-17).
    let joined = member.joined_at.map_or(0, |t| t.as_secs());
    if now - joined + 5 < v.verify.time_limit_ms / 1000 {
        return Ok(());
    }
    let timeout = member.communication_disabled_until.map(|t| t.as_secs());
    if let (at, Some(paused)) =
        paused_deadline(now, now + v.verify.time_limit_ms / 1000, timeout, None)
    {
        // The timeout update was missed (the bot was down): the full limit after the timeout.
        jobs::enqueue(
            &app.pool,
            gid.get() as i64,
            DEADLINE,
            deadline_job(user, Some(paused)),
            at,
        )
        .await?;
        return Ok(());
    }
    let reason = audit_reason(&v.verify);
    let (mut dm_failed, mut banned) = (false, false);
    match v.verify.on_fail {
        FailAction::Nothing => return Ok(()),
        FailAction::Kick => {
            // The third time out in a day bans for 7 days instead (owner decision 11-17).
            // Counted only once the removal worked, so a retried job does not count twice.
            let key = (gid.get(), user.get());
            let ban = recent(app, key, 86_400_000).await? + 1 >= BAN_AFTER;
            // A banned member gets the invite when the ban ends: Discord invites last 7 days at most.
            let link = if ban {
                None
            } else {
                retry_invite(app, gid, v).await
            };
            let server = guild_name(app, gid).await;
            let rows: Vec<Component> = link
                .into_iter()
                .map(|l| row(crate::welcome::link("Join again", l)))
                .collect();
            let (dm_key, vars) = timed_out_msg(&server, !rows.is_empty(), ban);
            let dm = crate::reply::dm_msg(app, Some(gid), user, dm_key, &vars, &rows);
            let act = async {
                if ban {
                    // Queued first, so a restart right after the ban cannot make it permanent.
                    let at = now_secs() + BAN_SECS;
                    let job = serde_json::json!({"user": user.get(), "verify_retry": true});
                    let job = jobs::enqueue(&app.pool, gid.get() as i64, "unban", job, at).await?;
                    if let Err(e) = app.http.create_ban(gid, user).reason(BAN_REASON).await {
                        jobs::finish(&app.pool, job).await?;
                        return Err(e.into());
                    }
                    crate::scheduler::forget_jobs(
                        app,
                        gid,
                        user.get(),
                        crate::scheduler::BANNED,
                        Some(job),
                    )
                    .await;
                } else {
                    let kick = app.http.remove_guild_member(gid, user);
                    match reason {
                        Some(r) => kick.reason(r).await?,
                        None => kick.await?,
                    };
                }
                anyhow::Ok(())
            };
            let (told, done) = crate::reply::dm_and_act(true, Some(dm), act).await;
            done?;
            // The removal is done: a failed write must not fail the job, which would remove again.
            if let Err(e) = record(app, key).await {
                tracing::warn!(?e, "recording a verification time out failed");
            }
            dm_failed = told == crate::reply::DmOutcome::Failed;
            banned = ban;
        }
        FailAction::Ban => {
            let ban = app.http.create_ban(gid, user);
            match reason {
                Some(r) => ban.reason(r).await?,
                None => ban.await?,
            };
            crate::scheduler::forget_jobs(app, gid, user.get(), crate::scheduler::BANNED, None)
                .await;
        }
        FailAction::Quarantine => {
            let kind = crate::antinuke::Kind::Deadline;
            crate::antinuke::quarantine_as(app, gid, user, reason, kind).await?
        }
    }
    let outcome = if banned {
        format!("Banned for 7 days ({BAN_REASON})")
    } else {
        format!("{:?}", v.verify.on_fail)
    };
    let dm_note = if dm_failed {
        " (their DMs are closed, so the \"Timed out\" message did not arrive)"
    } else {
        ""
    };
    let log = not_verified_log(&member.user, &outcome, dm_note);
    // The punishment is done: a failed log must not fail the job, which would punish again.
    if let Err(e) = send_log(app, gid.get(), LogKind::Verification, log).await {
        tracing::warn!(?e, "verification deadline log failed");
    }
    Ok(())
}

pub(crate) fn ephemeral(
    kind: InteractionResponseType,
    content: Option<&str>,
) -> InteractionResponse {
    InteractionResponse {
        kind,
        data: Some(InteractionResponseData {
            content: content.map(Into::into),
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    }
}

/// Time outs in one day that make the next removal a 7-day ban instead of a kick.
const BAN_AFTER: usize = 3;
const BAN_SECS: i64 = 7 * 86_400;
const BAN_REASON: &str = "Timed out of verification 3 times in 24 hours";

/// Retry invites one server creates per hour at most, so a wave of joiners that never verify
/// cannot fill the server's invite cap.
const INVITES_PER_HOUR: usize = 20;
/// The `verify_timeouts` user of a server's retry invites (user ids are never 0).
const INVITES: u64 = 0;

/// The `verify_timeouts` rows of `key` younger than `window` ms. Stored, so a restart keeps them.
async fn recent(app: &App, key: (u64, u64), window: i64) -> Result<usize> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM verify_timeouts
         WHERE guild_id = $1 AND user_id = $2 AND at > now() - make_interval(secs => $3)",
    )
    .bind(key.0 as i64)
    .bind(key.1 as i64)
    .bind(window as f64 / 1000.0)
    .fetch_one(&app.pool)
    .await?;
    Ok(n as usize)
}

/// Records one time out (or, for user [`INVITES`], one retry invite) and drops the server's rows
/// older than a day.
async fn record(app: &App, key: (u64, u64)) -> Result<()> {
    let g = key.0 as i64;
    sqlx::query(
        "DELETE FROM verify_timeouts WHERE guild_id = $1 AND at < now() - interval '1 day'",
    )
    .bind(g)
    .execute(&app.pool)
    .await?;
    sqlx::query("INSERT INTO verify_timeouts (guild_id, user_id) VALUES ($1, $2)")
        .bind(g)
        .bind(key.1 as i64)
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// A single-use invite to the verification channel, valid one day, for a member removed for not
/// verifying in time. None during a lock or a join raid, with no verification channel, or when
/// Discord refuses.
async fn retry_invite(app: &App, gid: Id<GuildMarker>, v: &VerificationCfg) -> Option<String> {
    // A failed read counts as a lock: no invite.
    let held = crate::lockdown::raid_or_lock(app, gid)
        .await
        .unwrap_or(true);
    let ch = v.channel.filter(|_| !held && v.enabled)?;
    // A stored id from before the save-time checks may name another server's channel.
    if !app.guild_info(gid).await.ok()?.channels.contains(&ch) {
        tracing::warn!(%gid, %ch, "verification channel is not in this server, no retry invite");
        return None;
    }
    // A failed read counts as full: no invite.
    if recent(app, (gid.get(), INVITES), 3_600_000)
        .await
        .unwrap_or(INVITES_PER_HOUR)
        >= INVITES_PER_HOUR
    {
        return None;
    }
    let made = app
        .http
        .create_invite(ch)
        .unique(true)
        .temporary(false)
        .max_age(86_400)
        .max_uses(1)
        .reason("Verification timed out: invite to try again")
        .await;
    let code = crate::http_err::best_effort(made, "verification retry invite")?
        .model()
        .await
        .ok()?
        .code;
    if let Err(e) = record(app, (gid.get(), INVITES)).await {
        tracing::warn!(?e, "recording a verification retry invite failed");
    }
    Some(format!("https://discord.gg/{code}"))
}

/// Whether the ban on `user` is still the verification time-out ban. A ban staff put on top in
/// Discord (another reason) must not be lifted by the time-out unban.
pub async fn still_timeout_ban(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Result<bool> {
    match app.http.ban(gid, user).await {
        Ok(b) => Ok(b.model().await?.reason.as_deref() == Some(BAN_REASON)),
        Err(e) if crate::http_err::gone(&e) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// The unban after a verification time-out ban: DMs a fresh invite, valid 24 hours from now.
pub async fn retry_after_ban(app: &App, gid: Id<GuildMarker>, user: Id<UserMarker>) {
    let Ok(cfg) = app.cfg(gid.get()).await else {
        return;
    };
    let Some(link) = retry_invite(app, gid, &cfg.verification).await else {
        return;
    };
    let server = guild_name(app, gid).await;
    let rows = [row(crate::welcome::link("Join again", link))];
    let vars = [("server", server.as_str())];
    crate::reply::dm_msg(app, Some(gid), user, "verify.ban_ended", &vars, &rows).await;
}

/// The key and variables of the DM a member gets before the removal for not verifying in time.
pub(crate) fn timed_out_msg(
    server: &str,
    link: bool,
    ban: bool,
) -> (&'static str, [(&str, &str); 2]) {
    let again = if link {
        "Use the button below to join again and verify. The invite works once, for 24 hours."
    } else {
        "Ask the server for a new invite to try again."
    };
    let key = if ban {
        "verify.banned"
    } else {
        "verify.kicked"
    };
    (key, [("server", server), ("again", again)])
}

/// The default pass message (dashboard preview).
pub(crate) fn passed_embed(server: &str) -> Embed {
    crate::reply::bot_embed("verify.passed", None, &[("server", server)])
}

/// An ephemeral answer with the text and embeds of a short reply.
fn said_reply(s: crate::reply::Said) -> InteractionResponse {
    let mut r = ephemeral(
        InteractionResponseType::ChannelMessageWithSource,
        Some(&s.text).filter(|t| !t.is_empty()).map(String::as_str),
    );
    if let Some(d) = r.data.as_mut() {
        d.embeds = Some(s.embeds);
    }
    r
}

pub(crate) fn embed_reply(embed: Embed) -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            embeds: Some(vec![embed]),
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    }
}

pub(crate) fn web_link(gid: Id<GuildMarker>) -> InteractionResponse {
    let base = std::env::var("DASHBOARD_URL").unwrap_or_default();
    let mut r = ephemeral(
        InteractionResponseType::ChannelMessageWithSource,
        Some("The dashboard URL is not configured, ask the server staff."),
    );
    if let (Some(d), false) = (r.data.as_mut(), base.is_empty()) {
        d.content = Some("Open the page to verify.".into());
        d.components = Some(vec![row(Component::Button(Button {
            id: None,
            custom_id: None,
            disabled: false,
            emoji: None,
            label: Some("Verify on the website".into()),
            style: ButtonStyle::Link,
            url: Some(format!("{}/verify/{gid}", base.trim_end_matches('/'))),
            sku_id: None,
        }))]);
    }
    r
}

/// The verification roles Brim may hand out. Verifying is open to anyone, so it never hands
/// out a permit role or one with kick/ban/admin-like rights, whoever put that role in the config.
async fn safe_roles(
    app: &App,
    gid: Id<GuildMarker>,
    v: &VerificationCfg,
) -> Result<(
    Vec<Id<RoleMarker>>,
    std::sync::Arc<crate::hierarchy::GuildInfo>,
)> {
    safe_roles_of(app, gid, &v.roles).await
}

/// The roles of `list` that Brim may hand out to anyone: roles of this server with no permit and
/// no dangerous Discord permissions. Also the giveaway winner role.
pub(crate) async fn safe_roles_of(
    app: &App,
    gid: Id<GuildMarker>,
    list: &[Id<RoleMarker>],
) -> Result<(
    Vec<Id<RoleMarker>>,
    std::sync::Arc<crate::hierarchy::GuildInfo>,
)> {
    let info = crate::lockdown::unlocked_info(app, gid).await?;
    let permit_roles = brim_core::permits::permit_roles(&app.pool, gid.get() as i64).await?;
    let roles = list
        .iter()
        .copied()
        .filter(|r| {
            let safe = !permit_roles.contains(&(r.get() as i64))
                // A role not in this server's list (another server's, or deleted) is not given.
                && info
                    .role_perms
                    .get(r)
                    .is_some_and(|p| p & brim_antinuke::DANGEROUS_PERMS == 0);
            if !safe {
                tracing::warn!(
                    guild = gid.get(),
                    role = r.get(),
                    "role holds a permit or dangerous permissions, or is not a role of this server, not granted"
                );
            }
            safe
        })
        .collect();
    Ok((roles, info))
}

/// Hands out the verification roles; a member quarantined on join leaves quarantine.
pub(crate) async fn grant(
    app: &App,
    gid: Id<GuildMarker>,
    user: Id<UserMarker>,
    v: &VerificationCfg,
) -> Result<()> {
    let (roles, info) = safe_roles(app, gid, v).await?;
    // Before the roles land: the other-bot check must not take back what Brim gives.
    given::note_verified(gid, user);
    // A join quarantine ends whatever "quarantine on join" says now: it may have been switched
    // off since they joined.
    let join = crate::antinuke::quarantine_kind(app, gid, user).await? == Some(true);
    if v.verify.replace_roles || join {
        // Discord refuses a role list that drops a managed (booster, integration) role.
        let member = app.http.guild_member(gid, user).await?.model().await?;
        let mut roles = roles;
        let managed: Vec<_> = member
            .roles
            .iter()
            .copied()
            .filter(|r| info.managed.contains(r) && !roles.contains(r))
            .collect();
        roles.extend(managed);
        app.http
            .update_guild_member(gid, user)
            .roles(&roles)
            .await?;
    } else {
        for r in &roles {
            app.http.add_guild_member_role(gid, user, *r).await?;
        }
    }
    if join {
        sqlx::query("DELETE FROM quarantines WHERE guild_id = $1 AND user_id = $2 AND on_join")
            .bind(gid.get() as i64)
            .bind(user.get() as i64)
            .execute(&app.pool)
            .await?;
    }
    // After the roles are set and the join quarantine is gone, so neither wipes the restored roles.
    if let Err(e) = crate::sticky::restore_pending(app, gid, user).await {
        tracing::warn!(?e, "giving back sticky roles after verification failed");
    }
    if let Ok(u) = async { anyhow::Ok(app.http.user(user).await?.model().await?) }.await {
        let log = passed_log(&u);
        if let Err(e) = send_log(app, gid.get(), LogKind::Verification, log).await {
            tracing::warn!(?e, "verification log failed");
        }
    }
    Ok(())
}

/// "name's Verification Result:" log.
fn result_log(u: &User, color: u32, status: &str) -> Log {
    let created = format!("<t:{}:F>", account_created_ms(u.id.get()) / 1000);
    Log::new(&format!("{}'s Verification Result:", u.name), color)
        .thumbnail(u)
        .user("Member", u.id.get())
        .line("Creation", &created)
        .field("Status:", status)
        .var("member.username", &u.name)
        .var("member.id", &u.id.to_string())
        .var("member.created_at", &created)
}

/// The result log of a member who passed.
pub(crate) fn passed_log(u: &User) -> Log {
    let status = format!("{YES} <@{}> has passed verification successfully.", u.id);
    result_log(u, DONE, &status).entry("log.verification.passed")
}

/// The result log of a member who did not verify in time.
pub(crate) fn not_verified_log(u: &User, outcome: &str, dm_note: &str) -> Log {
    let status = format!(
        "{NO} <@{}> did not verify in time: **{outcome}**{dm_note}",
        u.id
    );
    result_log(u, ALERT, &status)
        .entry("log.verification.not_verified")
        .var("outcome", outcome)
        .var("dm_note", dm_note)
}

pub(crate) use brim_automod::captcha::new_captcha;

/// Wrong answers one captcha takes before it is dropped.
const MAX_TRIES: u8 = 3;

/// A sum a member types; no image, so screen readers and slow connections cope. Returns (question, answer).
pub(crate) fn new_sum() -> (String, String) {
    use std::hash::{BuildHasher, Hasher};
    let r = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let (a, b) = (r % 9 + 1, (r >> 8) % 9 + 1);
    (format!("What is {a} + {b}?"), (a + b).to_string())
}

/// Which guild a verify interaction belongs to; DM ones carry it in the custom id.
fn guild_of(i: &Interaction, id: &str) -> Option<Id<GuildMarker>> {
    i.guild_id.or_else(|| {
        id.rsplit(':')
            .next()?
            .parse()
            .ok()
            .and_then(Id::new_checked)
    })
}

pub(crate) const PROMPT: &str = "Please type the captcha below to be able to access this server!";
/// Tips for reading the captcha picture: in the Help answer, so the challenge stays short.
const NOTES: &str = "\n\n**Additional Notes:**\n› Type out the traced colored characters from left to right.\n› Ignore the decoy characters spread-around.\n› You don't have to respect characters cases (upper/lower case)!";

pub(crate) fn challenge_embed(body: &str, png: bool, limit_ms: i64) -> Embed {
    let mut e = EmbedBuilder::new()
        .color(GREEN)
        .title(format!(
            "{} Hello! Are you human? Let's find out!",
            brim_logging::emoji::CAPTCHA
        ))
        .description(format!("```\n{body}\n```"))
        .footer(EmbedFooterBuilder::new(format!(
            "Verification Period: {}",
            period(limit_ms)
        )));
    if let (true, Ok(img)) = (png, ImageSource::attachment("captcha.png")) {
        e = e.image(img);
    }
    e.build()
}

/// Swaps the captcha image in the member's ephemeral message every `every` ms until it is
/// solved, expires or the interaction token runs out (15 minutes).
async fn regenerate(
    app: &App,
    client: &twilight_http::client::InteractionClient<'_>,
    token: &str,
    key: (u64, u64),
    expires: i64,
    every: i64,
    v: &VerifyCfg,
) {
    let stop = expires.min(now_secs() * 1000 + 14 * 60_000);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(every as u64)).await;
        let (code, png) = new_captcha(&v.captcha);
        let live = {
            let mut codes = app.captchas.lock().unwrap();
            // A newer Verify click replaces the entry (new expiry): this message is then stale.
            let live = now_secs() * 1000 < stop && codes.get(&key).is_some_and(|e| e.1 == expires);
            if live {
                codes.get_mut(&key).unwrap().0 = code;
            }
            live
        };
        let embeds = [challenge_embed(PROMPT, true, v.time_limit_ms)];
        let file = [Attachment::from_bytes("captcha.png".into(), png, 1)];
        let enter = [row(button("Answer", &format!("{ENTER}{}", key.0)))];
        if !live
            || client
                .update_response(token)
                .embeds(Some(&embeds))
                .attachments(&file)
                .components(Some(&enter))
                .await
                .is_err()
        {
            return;
        }
    }
}

/// The text a member typed, wherever the modal nests the input.
pub(crate) fn typed_text(c: &ModalInteractionComponent) -> Option<String> {
    match c {
        ModalInteractionComponent::TextInput(t) => Some(t.value.clone()),
        ModalInteractionComponent::ActionRow(r) => r.components.iter().find_map(typed_text),
        ModalInteractionComponent::Label(l) => typed_text(&l.component),
        _ => None,
    }
}

/// Handles the verify button, the "enter code" button and the code modal. Returns false for other interactions.
pub async fn handle(app: &App, i: &Interaction) -> Result<bool> {
    let id = match &i.data {
        Some(InteractionData::MessageComponent(c)) => c.custom_id.as_str(),
        Some(InteractionData::ModalSubmit(m)) => m.custom_id.as_str(),
        _ => return Ok(false),
    };
    if !id.starts_with("verify:") {
        return Ok(false);
    }
    let (Some(gid), Some(u)) = (guild_of(i, id), i.author()) else {
        return Ok(false);
    };
    let user = u.id;
    let client = app.http.interaction(Id::new(app.me.get()));
    let reply = |text: &'static str| {
        ephemeral(
            InteractionResponseType::ChannelMessageWithSource,
            Some(text),
        )
    };
    match &i.data {
        Some(InteractionData::MessageComponent(c)) if c.custom_id == HELP => {
            let cfg = app.cfg(gid.get()).await?;
            let stored = crate::reply::stored_msg(app, gid, "verify.help").await;
            let help = help_embed(&cfg.verification, stored.as_ref());
            client
                .create_response(i.id, &i.token, &embed_reply(help))
                .await?;
            Ok(true)
        }
        Some(InteractionData::MessageComponent(c))
            if c.custom_id == START || c.custom_id == ACCEPT =>
        {
            let cfg = app.cfg(gid.get()).await?;
            let member = i.member.as_ref();
            let has_role =
                member.is_some_and(|m| m.roles.iter().any(|r| cfg.verification.roles.contains(r)));
            // Same cured names as Join Gate, or a member it found suspicious skips the captcha here.
            let name = crate::joingate::names(&[Some(&u.name), u.global_name.as_ref()]);
            let joiner = brim_automod::join::Joiner {
                id: u.id.get(),
                name: &name,
                has_avatar: u.avatar.is_some(),
                ..Default::default()
            };
            let now = now_secs() * 1000;
            let min_age = cfg.joingate.min_age_ms();
            let suspicious =
                brim_automod::join::is_suspicious(&joiner, &cfg.joingate, min_age, now);
            let raid = app
                .raids
                .lock()
                .unwrap()
                .get(&gid.get())
                .is_some_and(|r| r.active(now));
            // Owners are immune and never need to verify.
            if matches!(cfg.verification.verify.mode, VerifyMode::Web) {
                let owner = app.guild_info(gid).await?.owner;
                if matches!(
                    app.level(gid, owner, user).await?,
                    brim_core::permits::Level::Owner | brim_core::permits::Level::ExtraOwner
                ) {
                    // Made EO while held in a join quarantine: release them.
                    if crate::antinuke::quarantine_kind(app, gid, user).await? == Some(true) {
                        let defer = InteractionResponseType::DeferredChannelMessageWithSource;
                        client
                            .create_response(i.id, &i.token, &ephemeral(defer, None))
                            .await?;
                        let t = match grant(app, gid, user, &cfg.verification).await {
                            Ok(()) => "The server owner and extra owners do not verify: Brim ended your join quarantine.".to_string(),
                            Err(e) => grant_failed(&e),
                        };
                        client.update_response(&i.token).content(Some(&t)).await?;
                        return Ok(true);
                    }
                    let t = "The server owner and extra owners do not verify; nothing is needed from you.";
                    client.create_response(i.id, &i.token, &reply(t)).await?;
                    return Ok(true);
                }
            }
            let mut step = decide(&cfg.verification, raid, suspicious, has_role);
            // The interaction user has no spammer bit (twilight drops it): ask Discord, only when
            // the answer would turn a Grant into a challenge.
            if let Some(flagged) = flagged_step(&cfg.verification, raid, suspicious, has_role) {
                if spammer(app, gid, user).await {
                    step = flagged;
                }
            }
            if c.custom_id == ACCEPT {
                step = accept_step(step);
            }
            match step {
                Step::Say(key) => {
                    let said = crate::reply::say(app, gid, key, &[]).await;
                    client
                        .create_response(i.id, &i.token, &said_reply(said))
                        .await?;
                }
                Step::Web => {
                    client
                        .create_response(i.id, &i.token, &web_link(gid))
                        .await?;
                }
                Step::Grant => {
                    // Deferred: the grant can take longer than Discord's 3 s to answer.
                    let defer = InteractionResponseType::DeferredChannelMessageWithSource;
                    client
                        .create_response(i.id, &i.token, &ephemeral(defer, None))
                        .await?;
                    let answer = client.update_response(&i.token);
                    match self_grant(app, gid, user, &cfg.verification).await {
                        Ok(()) => {
                            let server = guild_name(app, gid).await;
                            let vars = [("server", server.as_str())];
                            let done =
                                [
                                    crate::reply::bot_embed_for(app, gid, "verify.passed", &vars)
                                        .await,
                                ];
                            answer.embeds(Some(&done)).await?;
                        }
                        Err(e) => {
                            answer.content(Some(&grant_failed(&e))).await?;
                        }
                    }
                }
                step => {
                    let v = &cfg.verification.verify;
                    let (code, png, body) = match step {
                        Step::Simple => {
                            let (q, a) = new_sum();
                            (a, None, q)
                        }
                        _ => {
                            let (code, png) = new_captcha(&v.captcha);
                            (code, Some(png), PROMPT.to_string())
                        }
                    };
                    let png_shown = png.is_some();
                    let embed = challenge_embed(&body, png.is_some(), v.time_limit_ms);
                    let key = (gid.get(), user.get());
                    let expires = now_secs() * 1000 + v.time_limit_ms;
                    let now = now_secs() * 1000;
                    remember_code(
                        &mut app.captchas.lock().unwrap(),
                        key,
                        (code, expires, 0),
                        now,
                    );
                    let enter = [row(button("Answer", &format!("{ENTER}{gid}")))];
                    let embeds = [embed];
                    let file = png.map(|p| [Attachment::from_bytes("captcha.png".into(), p, 1)]);
                    if step == Step::Dm {
                        // Deferred: a DM has no time limit of its own.
                        let defer = InteractionResponseType::DeferredChannelMessageWithSource;
                        client
                            .create_response(i.id, &i.token, &ephemeral(defer, None))
                            .await?;
                        let send = async {
                            let ch = app.http.create_private_channel(user).await?.model().await?;
                            let mut m = app.http.create_message(ch.id).embeds(&embeds);
                            if let Some(f) = &file {
                                m = m.attachments(f);
                            }
                            m.components(&enter).await?;
                            anyhow::Ok(())
                        };
                        let sent = matches!(
                            tokio::time::timeout(crate::reply::DM_WAIT, send).await,
                            Ok(Ok(()))
                        );
                        let t = if sent {
                            "Check your DMs for the challenge."
                        } else {
                            "I could not DM you. Open your DMs for this server and press Verify again."
                        };
                        client.update_response(&i.token).content(Some(t)).await?;
                    } else {
                        let defer = InteractionResponseType::DeferredChannelMessageWithSource;
                        client
                            .create_response(i.id, &i.token, &ephemeral(defer, None))
                            .await?;
                        let mut u = client.update_response(&i.token).embeds(Some(&embeds));
                        if let Some(f) = &file {
                            u = u.attachments(f);
                        }
                        u.components(Some(&enter)).await?;
                        if let (Some(every), true) = (v.regen_ms, png_shown) {
                            regenerate(app, &client, &i.token, key, expires, every.max(5_000), v)
                                .await;
                        }
                    }
                }
            }
            Ok(true)
        }
        Some(InteractionData::MessageComponent(c)) if c.custom_id.starts_with(ENTER) => {
            #[allow(deprecated)]
            // Discord moved labels to the Label component; a plain row still works.
            let input = Component::TextInput(TextInput {
                id: None,
                custom_id: "code".into(),
                label: Some("Your answer".into()),
                max_length: Some(10),
                min_length: Some(1),
                placeholder: None,
                required: Some(true),
                style: TextInputStyle::Short,
                value: None,
            });
            client
                .create_response(
                    i.id,
                    &i.token,
                    &InteractionResponse {
                        kind: InteractionResponseType::Modal,
                        data: Some(InteractionResponseData {
                            custom_id: Some(format!("{CODE}{gid}")),
                            title: Some("Verify".into()),
                            components: Some(vec![row(input)]),
                            ..Default::default()
                        }),
                    },
                )
                .await?;
            Ok(true)
        }
        Some(InteractionData::ModalSubmit(m)) if m.custom_id.starts_with(CODE) => {
            let typed = m.components.iter().find_map(typed_text).unwrap_or_default();
            // (solved, the code is gone): a code dies after MAX_TRIES wrong answers, so a script
            // cannot guess on one image; Verify gives a new one.
            let (solved, spent) = {
                let mut codes = app.captchas.lock().unwrap();
                let key = (gid.get(), user.get());
                let live = codes.get_mut(&key).filter(|e| e.1 > now_secs() * 1000);
                let (ok, spent) = match live {
                    Some(e) if e.0.eq_ignore_ascii_case(typed.trim()) => (true, true),
                    Some(e) => {
                        e.2 += 1;
                        (false, e.2 >= MAX_TRIES)
                    }
                    None => (false, false),
                };
                if spent {
                    codes.remove(&key);
                }
                (ok, spent)
            };
            if solved {
                let cfg = app.cfg(gid.get()).await?;
                // Deferred: the grant can take longer than Discord's 3 s to answer.
                let edit = i.message.is_some();
                let kind = if edit {
                    InteractionResponseType::DeferredUpdateMessage
                } else {
                    InteractionResponseType::DeferredChannelMessageWithSource
                };
                client
                    .create_response(i.id, &i.token, &ephemeral(kind, None))
                    .await?;
                let granted = self_grant(app, gid, user, &cfg.verification).await;
                let text = match &granted {
                    Ok(()) => "Verification done.".to_string(),
                    Err(e) => grant_failed(e),
                };
                let mut answer = client.update_response(&i.token).content(Some(&text));
                if edit {
                    // The challenge is spent either way.
                    answer = answer
                        .embeds(Some(&[]))
                        .components(Some(&[]))
                        .keep_attachment_ids(&[]);
                }
                answer.await?;
                if granted.is_err() {
                    return Ok(true);
                }
                let server = guild_name(app, gid).await;
                let vars = [("server", server.as_str())];
                let done = crate::reply::bot_embed_for(app, gid, "verify.passed", &vars).await;
                client
                    .create_followup(&i.token)
                    .embeds(&[done])
                    .flags(MessageFlags::EPHEMERAL)
                    .await?;
            } else {
                let key = if spent {
                    "verify.spent"
                } else {
                    "verify.wrong"
                };
                let fail = crate::reply::bot_embed_for(app, gid, key, &[]).await;
                client
                    .create_response(i.id, &i.token, &embed_reply(fail))
                    .await?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brim_automod::join::{CaptchaCfg, VerifyTarget};

    fn cfg(mode: VerifyMode, target: VerifyTarget) -> VerificationCfg {
        VerificationCfg {
            enabled: true,
            roles: vec![Id::new(1)],
            verify: VerifyCfg {
                mode,
                target,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn help_names_the_roles_and_the_time_limit() {
        let mut c = cfg(VerifyMode::Captcha, VerifyTarget::Everyone);
        c.verify.on_fail = FailAction::Kick;
        let steps = help_steps(&c);
        assert!(
            steps.contains("**4.** After you pass, you get: <@&1>"),
            "{steps}"
        );
        assert!(
            steps.contains("**5.** Finish within 2 minutes.") && steps.ends_with(NOTES),
            "{steps}"
        );
        c.verify.show_notes = false;
        assert!(help_steps(&c).ends_with("**5.** Finish within 2 minutes."));
        c.verify.show_notes = true;
        c.roles = vec![Id::new(1), Id::new(2)];
        assert!(help_steps(&c).contains("you get: <@&1>, <@&2>"));
        // No roles: no role line; no fail action: no time limit line.
        c.roles.clear();
        c.verify.on_fail = FailAction::Nothing;
        let steps = help_steps(&c);
        assert!(
            !steps.contains("you get") && !steps.contains("Finish within"),
            "{steps}"
        );
        assert!(steps.contains("**3.** Press **Answer** and type the characters."));
    }

    #[test]
    fn audit_reason_is_on_unless_switched_off() {
        let saved: VerificationCfg = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(saved.verify.audit_reason);
        let off: VerificationCfg =
            serde_json::from_value(serde_json::json!({"audit_reason": false})).unwrap();
        assert!(!off.verify.audit_reason);
        assert_eq!(
            audit_reason(&saved.verify),
            Some("Did not verify within the time limit")
        );
        assert_eq!(audit_reason(&off.verify), None);
    }

    #[test]
    fn time_limit_pauses_while_timed_out() {
        let (now, deadline) = (1_000, 1_120);
        // No timeout: nothing moves.
        assert_eq!(paused_deadline(now, deadline, None, None), (deadline, None));
        assert_eq!(
            paused_deadline(now, deadline, Some(900), None),
            (deadline, None)
        );
        // Timed out until 1300 with 120 s left: the 120 s start at 1300.
        assert_eq!(
            paused_deadline(now, deadline, Some(1_300), None),
            (1_420, Some(1_300))
        );
        // Extended to 1500 while paused: still 120 s left, now after 1500.
        assert_eq!(
            paused_deadline(1_100, 1_420, Some(1_500), Some(1_300)),
            (1_620, Some(1_500))
        );
        // Lifted early at 1100: the 120 s run from now.
        assert_eq!(
            paused_deadline(1_100, 1_420, None, Some(1_300)),
            (1_220, None)
        );
        // Ran out by itself: the clock runs again, the run time stays.
        assert_eq!(
            paused_deadline(1_350, 1_420, None, Some(1_300)),
            (1_420, None)
        );
        // Already late: no time left, nothing negative.
        assert_eq!(
            paused_deadline(1_200, 1_100, Some(1_300), None),
            (1_300, Some(1_300))
        );
    }

    #[test]
    fn every_charset_draws_only_its_characters() {
        for name in brim_automod::join::Charset::NAMES {
            let charset = serde_json::from_value(serde_json::json!(name)).unwrap();
            let (code, png) = new_captcha(&CaptchaCfg {
                charset,
                ..Default::default()
            });
            assert!(!png.is_empty() && code.len() == 6, "{name}");
            assert!(code.chars().all(|c| charset.allows(c)), "{name}: {code}");
        }
    }

    #[test]
    fn accept_rules_never_skips_a_challenge() {
        use super::{accept_step, ACCEPT_REFUSED};
        for (mode, want) in [
            (VerifyMode::Button, Step::Grant),
            (VerifyMode::Captcha, Step::Say(ACCEPT_REFUSED)),
            (VerifyMode::SimpleCaptcha, Step::Say(ACCEPT_REFUSED)),
            (VerifyMode::Dm, Step::Say(ACCEPT_REFUSED)),
            (VerifyMode::Web, Step::Say(ACCEPT_REFUSED)),
        ] {
            let c = cfg(mode, VerifyTarget::Everyone);
            assert_eq!(
                accept_step(decide(&c, false, false, false)),
                want,
                "{mode:?}"
            );
        }
        // A raid raises Button to a challenge, and a suspicious member gets one too.
        let button = cfg(VerifyMode::Button, VerifyTarget::Everyone);
        assert_eq!(
            accept_step(decide(&button, true, false, false)),
            Step::Say(ACCEPT_REFUSED)
        );
        let captcha = cfg(VerifyMode::Captcha, VerifyTarget::SuspiciousOnly);
        assert_eq!(
            accept_step(decide(&captcha, false, true, false)),
            Step::Say(ACCEPT_REFUSED)
        );
        // Off, or already verified: the same answers as the Verify button.
        assert!(matches!(
            accept_step(decide(&captcha, false, false, true)),
            Step::Say(_)
        ));
    }

    #[test]
    fn click_outcomes() {
        let c = cfg(VerifyMode::Button, VerifyTarget::Everyone);
        assert_eq!(decide(&c, false, false, false), Step::Grant);
        assert_eq!(decide(&c, true, false, false), Step::Captcha);
        assert!(matches!(decide(&c, false, false, true), Step::Say(_)));
        let c = cfg(VerifyMode::Captcha, VerifyTarget::SuspiciousOnly);
        assert_eq!(decide(&c, false, false, false), Step::Grant);
        assert_eq!(decide(&c, false, true, false), Step::Captcha);
        for (mode, step) in [
            (VerifyMode::SimpleCaptcha, Step::Simple),
            (VerifyMode::Dm, Step::Dm),
            (VerifyMode::Captcha, Step::Captcha),
        ] {
            assert_eq!(
                decide(&cfg(mode, VerifyTarget::Everyone), false, false, false),
                step
            );
        }
        assert!(matches!(
            decide(&VerificationCfg::default(), false, true, false),
            Step::Say(_)
        ));
        assert_eq!(
            decide(
                &cfg(VerifyMode::Web, VerifyTarget::Everyone),
                false,
                true,
                false
            ),
            Step::Web
        );
    }

    #[test]
    fn spammer_flag_is_read_only_when_it_changes_the_step() {
        let c = cfg(VerifyMode::Captcha, VerifyTarget::SuspiciousOnly);
        assert_eq!(flagged_step(&c, false, false, false), Some(Step::Captcha));
        // Already suspicious or already verified: no lookup.
        assert_eq!(flagged_step(&c, false, true, false), None);
        assert_eq!(flagged_step(&c, false, false, true), None);
        let every = cfg(VerifyMode::Captcha, VerifyTarget::Everyone);
        assert_eq!(flagged_step(&every, false, false, false), None);
        // Button mode grants either way.
        let button = cfg(VerifyMode::Button, VerifyTarget::SuspiciousOnly);
        assert_eq!(flagged_step(&button, false, false, false), None);
        // ...except in a raid, where a suspicious member gets a captcha.
        assert_eq!(
            flagged_step(&button, true, false, false),
            Some(Step::Captcha)
        );
        assert_eq!(
            flagged_step(&VerificationCfg::default(), false, false, false),
            None
        );
    }

    #[test]
    fn unanswered_challenges_are_dropped_once_many_pile_up() {
        let mut m = HashMap::new();
        for u in 0..FLAG_KEEP as u64 {
            remember_code(&mut m, (9, u), ("a".into(), 10, 0), 0);
        }
        remember_code(&mut m, (1, 1), ("b".into(), 100, 0), 50);
        remember_code(&mut m, (1, 2), ("c".into(), 100, 0), 50);
        assert_eq!(m.len(), 2);
        for u in 0..FLAG_KEEP as u64 {
            remember_code(&mut m, (8, u), ("d".into(), 1_000, 0), 60);
        }
        // Full of live challenges: the flooding server's go; the other server's stay.
        assert!(m.len() < FLAG_KEEP);
        assert!(m.contains_key(&(1, 1)) && m.contains_key(&(1, 2)));
    }

    #[test]
    fn spammer_flag_cache_expires_and_stays_bounded() {
        let mut m = HashMap::new();
        remember_flag(&mut m, (1, 2), true, 1_000);
        assert_eq!(cached_flag(&m, (1, 2), 1_000 + FLAG_TTL_MS - 1), Some(true));
        assert_eq!(cached_flag(&m, (1, 2), 1_000 + FLAG_TTL_MS), None);
        assert_eq!(cached_flag(&m, (1, 3), 1_000), None);
        let mut m = HashMap::new();
        for u in 0..FLAG_KEEP as u64 {
            remember_flag(&mut m, (9, u), false, 0);
        }
        // Full of old answers: they are pruned before the next one goes in.
        remember_flag(&mut m, (1, 4), false, FLAG_TTL_MS);
        assert_eq!(m.len(), 1);
        assert_eq!(cached_flag(&m, (1, 4), FLAG_TTL_MS), Some(false));
    }

    #[test]
    fn sum_answer_matches_question() {
        for _ in 0..20 {
            let (q, a) = new_sum();
            let n: Vec<u32> = q
                .split(|c: char| !c.is_ascii_digit())
                .filter_map(|x| x.parse().ok())
                .collect();
            assert_eq!(n.iter().sum::<u32>().to_string(), a);
        }
    }

    #[test]
    fn config_accepts_flattened_verify_fields_and_defaults() {
        let c: VerificationCfg = serde_json::from_value(
            serde_json::json!({"enabled": true, "channel": "5", "roles": ["7"], "mode": "Web"}),
        )
        .unwrap();
        assert!(c.enabled && c.verify.mode == VerifyMode::Web && c.roles.len() == 1);
        assert!(!VerificationCfg::default().enabled);
    }
}
