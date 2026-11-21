//! Confirm / Cancel prompts: a mod command parked in `pending_confirms` until its buttons are clicked.
use std::time::Duration;

use anyhow::{Context, Result};
use brim_core::jobs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use twilight_model::{
    application::interaction::{Interaction, InteractionData},
    channel::message::{
        component::{ActionRow, Button, ButtonStyle, Component},
        AllowedMentions, Embed, MessageFlags,
    },
    gateway::payload::incoming::MessageCreate,
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::{
        marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
        Id,
    },
};

use crate::{
    app::App,
    commands::{apply_dm, ModKind},
    modcmds::{self, ModCmd},
    prefix::Flags,
    reply::{self, Applied},
};

/// A message in this server's channel that a prefix command replied to.
pub type ReplyTo = (Id<ChannelMarker>, Id<MessageMarker>);

/// The message `msg` replies to, when it is a plain reply in the same channel.
pub(crate) fn reply_of(msg: &twilight_model::channel::Message) -> Option<ReplyTo> {
    let r = msg.reference.as_ref()?;
    let here = r.channel_id.is_none_or(|c| c == msg.channel_id);
    (here && r.kind == twilight_model::channel::message::MessageReferenceType::Default)
        .then_some((msg.channel_id, r.message_id?))
}

/// What a [`Pending`] does once confirmed.
#[derive(Serialize, Deserialize)]
pub enum Action {
    Mod {
        kind: ModKind,
        targets: Vec<Id<UserMarker>>,
        reason: Option<String>,
        duration_s: Option<i64>,
        flags: Flags,
        #[serde(default)]
        proof: Option<String>,
        /// A target typed as a raw id or a name, not a mention: worth a Yes/No first.
        #[serde(default)]
        unsure: bool,
        /// The message the command replied to: verified proof when the punished member wrote it.
        #[serde(default)]
        reply: Option<ReplyTo>,
    },
    Ext {
        cmd: ModCmd,
        channel: Id<ChannelMarker>,
        reason: Option<String>,
        #[serde(default)]
        proof: Option<String>,
        /// Quarantine, unquarantine and unban run once per member here; empty runs `cmd` as is.
        #[serde(default)]
        users: Vec<u64>,
        #[serde(default)]
        unsure: bool,
        /// The `?dm` flag.
        #[serde(default)]
        dm: bool,
        /// As in `Mod`; used by quarantine only.
        #[serde(default)]
        reply: Option<ReplyTo>,
    },
    /// `closeall` / `acloseall`: every open ticket of the server.
    CloseTickets {
        reason: String,
        anon: bool,
        /// The channel the question is in: left open if it is a ticket.
        #[serde(default)]
        here: Option<Id<ChannelMarker>>,
    },
}

/// A command waiting for its Confirm click, stored in `pending_confirms` so it survives a restart.
pub struct Pending {
    gid: Id<GuildMarker>,
    actor: Id<UserMarker>,
    action: Action,
}

pub const CONFIRM_TTL: Duration = Duration::from_secs(10);

impl Pending {
    pub fn new(gid: Id<GuildMarker>, actor: Id<UserMarker>, action: Action) -> Self {
        Self { gid, actor, action }
    }

    /// Mod commands ask when a target was typed as an id or a name (a mention is sure), and
    /// hackbans always; destructive extras ask as `ModCmd::asks_confirm` says.
    /// `enabled` is the server's `misc.confirm`.
    pub fn needs_confirm(&self, enabled: bool) -> bool {
        match &self.action {
            Action::Mod {
                targets,
                unsure,
                flags,
                ..
            } => enabled && !targets.is_empty() && (*unsure || flags.hack),
            Action::Ext { cmd, unsure, .. } => {
                cmd.asks_confirm(enabled) || (enabled && *unsure && cmd.asks_when_unsure())
            }
            Action::CloseTickets { .. } => true,
        }
    }

    /// The entry shown in place of the question while a confirmed command runs.
    fn working(&self) -> &'static str {
        match &self.action {
            Action::Ext {
                cmd: ModCmd::Purge { .. },
                ..
            } => "reply.confirm.purging",
            _ => "reply.confirm.working",
        }
    }

    /// The prompt, from the server's edited version `stored` or the default.
    fn question(&self, stored: Option<&Value>) -> Embed {
        let target = match &self.action {
            Action::Mod { targets, .. } => {
                let who: Vec<String> = targets.iter().map(|t| format!("<@{t}> [`{t}`]")).collect();
                who.join("\n")
            }
            Action::Ext { users, .. } if !users.is_empty() => {
                let who: Vec<String> = users.iter().map(|t| format!("<@{t}> [`{t}`]")).collect();
                who.join("\n")
            }
            Action::Ext { cmd, .. } => cmd.describe(),
            Action::CloseTickets { .. } => "Every open ticket of this server".into(),
        };
        let seconds = CONFIRM_TTL.as_secs().to_string();
        let vars = [("text", target.as_str()), ("seconds", seconds.as_str())];
        reply::bot_embed(QUESTION, stored, &vars)
    }

    /// One outcome per target. A punishment typed as a reply keeps the replied message as the
    /// verified proof of each case it made.
    pub(crate) async fn run(
        &self,
        app: &App,
        owner: Id<UserMarker>,
        points: i32,
    ) -> Result<Vec<Applied>> {
        let reply = match &self.action {
            Action::Mod { reply, .. } => *reply,
            Action::Ext {
                cmd: ModCmd::Quarantine { .. },
                reply,
                ..
            } => *reply,
            _ => None,
        };
        // Read first: a ban deletes the member's last messages.
        let replied = match reply {
            Some(at) => crate::proof::replied(app, at).await,
            None => None,
        };
        let out = self.run_action(app, owner, points).await?;
        if let Some(m) = &replied {
            for a in &out {
                let (Some(case_no), Some(t)) = (a.case, a.target) else {
                    continue;
                };
                if let Some(copy) = crate::proof::own_copy(m, Id::new(t)) {
                    crate::proof::verified(app, self.gid, case_no, &copy).await;
                }
            }
        }
        Ok(out)
    }

    async fn run_action(
        &self,
        app: &App,
        owner: Id<UserMarker>,
        points: i32,
    ) -> Result<Vec<Applied>> {
        let (kind, targets, reason, duration_s, flags, proof) = match &self.action {
            Action::Mod {
                kind,
                targets,
                reason,
                duration_s,
                flags,
                proof,
                ..
            } => (*kind, targets, reason, *duration_s, *flags, proof),
            Action::Ext {
                cmd,
                channel,
                reason,
                proof,
                users,
                dm,
                ..
            } => {
                let cmds: Vec<ModCmd> = users.iter().filter_map(|u| cmd.with_user(*u)).collect();
                let cmds = if cmds.is_empty() {
                    vec![cmd.clone()]
                } else {
                    cmds
                };
                let mut out = vec![];
                for cmd in cmds {
                    // Fresh level each time: a rate-limit quarantine mid-list must stop the rest.
                    let actor = app.level(self.gid, owner, self.actor).await?;
                    let ran = modcmds::run(
                        app,
                        self.gid,
                        owner,
                        (self.actor, &actor),
                        *channel,
                        cmd,
                        (reason.as_deref(), proof.as_deref()),
                        *dm,
                    )
                    .await;
                    let done = match ran {
                        Ok(done) => done,
                        Err(e) => {
                            tracing::warn!(?e, "mod command failed");
                            let why = crate::http_err::why(&e);
                            let key = "reply.confirm.command_failed";
                            reply::say_line(app, self.gid, key, &[("error", &why)])
                                .await
                                .into()
                        }
                    };
                    out.push(done);
                }
                return Ok(out);
            }
            Action::CloseTickets { reason, anon, here } => {
                // Checked again at the click: the permit may be gone since the question.
                let level = app.level(self.gid, owner, self.actor).await?;
                if !level.has("tickets") {
                    let key = "reply.confirm.no_ticket_permit";
                    return Ok(vec![reply::say_line(app, self.gid, key, &[]).await.into()]);
                }
                let staff = app.http.user(self.actor).await?.model().await?;
                let text =
                    crate::tickets::close_all(app, self.gid, &staff, reason, *anon, *here).await?;
                return Ok(vec![text.into()]);
            }
        };
        let mut out = vec![];
        for &t in targets {
            // Fresh level each time: a rate-limit quarantine mid-list must stop the rest.
            let actor = app.level(self.gid, owner, self.actor).await?;
            let done = match apply_dm(
                app,
                self.gid,
                owner,
                (self.actor, &actor),
                kind,
                t,
                reason.as_deref(),
                duration_s,
                points,
                flags,
                proof.as_deref(),
            )
            .await
            {
                Ok(done) => done,
                Err(e) => {
                    tracing::warn!(?e, "mod command failed");
                    let (why, user) = (crate::http_err::why(&e), format!("<@{t}>"));
                    let vars = [("action", kind.name()), ("user", &user), ("error", &why)];
                    let key = "reply.confirm.target_failed";
                    let text = reply::say_line(app, self.gid, key, &vars).await;
                    Applied {
                        target: Some(t.get()),
                        ..text.into()
                    }
                }
            };
            out.push(done);
        }
        Ok(out)
    }
}

fn confirm_button(label: &str, id: String, style: ButtonStyle) -> Component {
    Component::Button(Button {
        id: None,
        custom_id: Some(id),
        disabled: false,
        emoji: None,
        label: Some(label.into()),
        style,
        url: None,
        sku_id: None,
    })
}

/// The question for a warn on `target`, for the dashboard preview.
pub fn sample_question(target: u64) -> (Embed, Component) {
    let action = Action::Mod {
        kind: ModKind::Warn,
        targets: vec![Id::new(target)],
        reason: None,
        duration_s: None,
        flags: Flags::default(),
        proof: None,
        unsure: true,
        reply: None,
    };
    let job = Pending::new(Id::new(1), Id::new(1), action);
    (job.question(None), job.row(1))
}

impl Pending {
    fn row(&self, nonce: u64) -> Component {
        Component::ActionRow(ActionRow {
            id: None,
            components: vec![
                confirm_button(
                    "Yes",
                    format!("{CONFIRM}{nonce}:{}:y", self.actor),
                    ButtonStyle::Success,
                ),
                confirm_button(
                    "No",
                    format!("{CONFIRM}{nonce}:{}:n", self.actor),
                    ButtonStyle::Danger,
                ),
            ],
        })
    }
}

/// Stores `job` for its buttons and returns the question with the Yes / No row.
/// `nonce` is the command message or interaction id.
pub async fn stash_confirm(app: &App, nonce: u64, job: Pending) -> Result<(Embed, Component)> {
    let row = job.row(nonce);
    // Expired rows are dropped on the way in, so the table stays small without a sweeper.
    sqlx::query(
        "WITH gone AS (DELETE FROM pending_confirms WHERE created_at < now() - make_interval(secs => $4))
         INSERT INTO pending_confirms (nonce, guild_id, action) VALUES ($1, $2, $3)",
    )
    .bind(nonce as i64)
    .bind(job.gid.get() as i64)
    .bind(serde_json::to_value(&job.action)?)
    // A minute past expiry, so `run_expire` still finds the row and closes the question.
    .bind(CONFIRM_TTL.as_secs_f64() + 60.0)
    .execute(&app.pool)
    .await?;
    let stored = reply::stored_msg(app, job.gid, QUESTION).await;
    Ok((job.question(stored.as_ref()), row))
}

pub async fn ask_confirm(app: &App, msg: &MessageCreate, job: Pending) -> Result<()> {
    let nonce = msg.id.get();
    let (embed, row) = stash_confirm(app, nonce, job).await?;
    let (embeds, rows) = ([embed], [row]);
    let mut post = app.http.create_message(msg.channel_id).embeds(&embeds);
    // The question is not deleted on the command's own timer: its result is written into it.
    if let Some(to) = crate::cmdcfg::reply_to(msg.channel_id) {
        post = post.reply(to).fail_if_not_exists(false);
    }
    let sent = post
        .components(&rows)
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await?
        .model()
        .await?;
    let gid = msg.guild_id.context("not in a server")?;
    expire_later(app, gid, nonce, Asked::Message(msg.channel_id, sent.id)).await
}

/// Where a question was posted, so it can be closed when nobody answers.
pub enum Asked {
    Message(Id<ChannelMarker>, Id<MessageMarker>),
    /// A slash command's interaction token (valid 15 minutes).
    Interaction(String),
}

const QUESTION: &str = "reply.confirm.question";
const TIMED_OUT: &str = "reply.confirm.timed_out";

/// The confirm entry `key`, with the seconds to answer filled in where it names them.
async fn said(app: &App, gid: Id<GuildMarker>, key: &str, vars: &[(&str, &str)]) -> reply::Said {
    let seconds = CONFIRM_TTL.as_secs().to_string();
    let vars: Vec<_> = vars
        .iter()
        .copied()
        .chain([("seconds", seconds.as_str())])
        .collect();
    reply::say(app, gid, key, &vars).await
}

/// Queues the `confirm_expire` job that closes the question after [`CONFIRM_TTL`] (the
/// scheduler ticks every 5 s, so it closes 10 to 15 s after asking).
pub async fn expire_later(app: &App, gid: Id<GuildMarker>, nonce: u64, at: Asked) -> Result<()> {
    let job = match at {
        Asked::Message(c, m) => json!({"nonce": nonce, "channel": c.get(), "message": m.get()}),
        Asked::Interaction(token) => json!({"nonce": nonce, "token": token}),
    };
    let when = crate::app::now_secs() + CONFIRM_TTL.as_secs() as i64;
    jobs::enqueue(&app.pool, gid.get() as i64, "confirm_expire", job, when).await?;
    Ok(())
}

/// Scheduler arm for `confirm_expire`: swaps an unanswered question for [`TIMED_OUT`]. The row is
/// taken first, so a click at the same moment either runs the command or sees it expired.
pub async fn run_expire(app: &App, gid: Id<GuildMarker>, p: &Value) -> Result<()> {
    let nonce = p["nonce"].as_i64().context("no nonce")?;
    let gone = sqlx::query("DELETE FROM pending_confirms WHERE nonce = $1 AND guild_id = $2")
        .bind(nonce)
        .bind(gid.get() as i64)
        .execute(&app.pool)
        .await?;
    if gone.rows_affected() == 0 {
        return Ok(());
    }
    let told = said(app, gid, TIMED_OUT, &[]).await;
    let content = Some(told.text.as_str()).filter(|t| !t.is_empty());
    let r = match (
        p["token"].as_str(),
        p["channel"].as_u64(),
        p["message"].as_u64(),
    ) {
        (Some(token), ..) => app
            .http
            .interaction(Id::new(app.me.get()))
            .update_response(token)
            .content(content)
            .embeds(Some(&told.embeds))
            .components(Some(&[]))
            .await
            .map(drop),
        (None, Some(c), Some(m)) => app
            .http
            .update_message(Id::new(c), Id::new(m))
            .content(content)
            .embeds(Some(&told.embeds))
            .components(Some(&[]))
            .await
            .map(drop),
        _ => return Ok(()),
    };
    // The question may already be deleted; nothing else to do then.
    crate::http_err::best_effort(r, "close timed out question");
    Ok(())
}

const CONFIRM: &str = "confirm:";

/// `confirm:<nonce>:<actor>:<y|n>` -> (nonce, actor, yes).
fn parse_answer(custom_id: &str) -> Option<(i64, u64, bool)> {
    let mut it = custom_id.strip_prefix(CONFIRM)?.splitn(3, ':');
    let (n, actor, a) = (it.next()?, it.next()?, it.next()?);
    Some((n.parse().ok()?, actor.parse().ok()?, a == "y"))
}

/// Only the member who typed the command may answer, whatever their level.
fn may_answer(presser: Option<u64>, actor: u64) -> bool {
    presser == Some(actor)
}

/// Handles the Confirm / Cancel buttons. Returns false for other interactions.
pub async fn handle_confirm(app: &App, i: &Interaction) -> Result<bool> {
    let Some(InteractionData::MessageComponent(c)) = &i.data else {
        return Ok(false);
    };
    let Some((nonce, actor, yes)) = parse_answer(&c.custom_id) else {
        return Ok(false);
    };
    let client = app.http.interaction(Id::new(app.me.get()));
    let row = if !may_answer(i.author_id().map(Id::get), actor) {
        Err("reply.confirm.not_yours")
    } else {
        // A late press leaves the row, so the expiry job still closes the question.
        let row: Option<(i64, serde_json::Value)> = sqlx::query_as(
            "DELETE FROM pending_confirms
             WHERE nonce = $1 AND created_at >= now() - make_interval(secs => $2)
             RETURNING guild_id, action",
        )
        .bind(nonce)
        .bind(CONFIRM_TTL.as_secs_f64())
        .fetch_optional(&app.pool)
        .await?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        row.ok_or_else(|| gone_text(nonce as u64, now_ms))
    };
    // Every answer is sent before any slow work: Discord fails a click left unanswered for 3 s.
    let (job, first) = match row {
        Ok((gid, action)) => {
            let action: Action = serde_json::from_value(action)?;
            let job = Pending::new(Id::new(gid as u64), Id::new(actor), action);
            let first = if yes {
                job.working()
            } else {
                "reply.confirm.cancelled"
            };
            (job, first)
        }
        // Someone else, a repeat or a late press: only the presser sees why, the question stays
        // (the expiry job closes a timed out one). A question is only ever asked in a server.
        Err(key) => {
            let Some(gid) = i.guild_id else {
                return Ok(true);
            };
            let told = said(app, gid, key, &[]).await;
            let r = InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    content: Some(told.text).filter(|t| !t.is_empty()),
                    embeds: Some(told.embeds),
                    flags: Some(MessageFlags::EPHEMERAL),
                    ..Default::default()
                }),
            };
            client.create_response(i.id, &i.token, &r).await?;
            return Ok(true);
        }
    };
    // The buttons go at once, so the first press shows and a second one cannot run it twice.
    let first = said(app, job.gid, first, &[]).await;
    client
        .create_response(i.id, &i.token, &closed(first))
        .await?;
    if !yes {
        return Ok(true);
    }
    // Any failure replaces the "Working on it..." line: the question never stays stuck.
    let ran = async {
        let owner = app.guild_info(job.gid).await?.owner;
        let cfg = app.cfg(job.gid.get()).await?;
        let done = job.run(app, owner, cfg.misc.warn_base_points).await?;
        anyhow::Ok((done, cfg))
    }
    .await;
    let (text, embeds) = match &ran {
        // A purge posted its own note in the channel; the question would only repeat it. It goes
        // only now, after the note, not in the purge's bulk delete.
        Ok((done, _)) if !done.is_empty() && done.iter().all(|d| d.posted) => {
            let r = client.delete_response(&i.token).await;
            crate::http_err::best_effort(r, "remove answered question");
            return Ok(true);
        }
        Ok((done, cfg)) if !done.is_empty() => {
            let t = crate::reply::ModTexts::load(app, job.gid).await;
            job.reply(&t, done, cfg.misc.compact_replies, cfg.misc.timeout_secs())
        }
        Ok(_) => not_finished(app, job.gid, "it returned no result").await,
        Err(e) => {
            tracing::warn!(?e, "confirmed command failed");
            not_finished(app, job.gid, &crate::http_err::why(e)).await
        }
    };
    // The question can be gone by now (its channel deleted, or a run past the 15-minute token).
    let r = client
        .update_response(&i.token)
        .content(Some(&text))
        .embeds(Some(&embeds))
        .components(Some(&[]))
        .allowed_mentions(Some(&AllowedMentions::default()))
        .await;
    crate::http_err::best_effort(r, "confirm result");
    Ok(true)
}

async fn not_finished(app: &App, gid: Id<GuildMarker>, why: &str) -> (String, Vec<Embed>) {
    let told = said(app, gid, "reply.confirm.not_finished", &[("error", why)]).await;
    (told.text, told.embeds)
}

/// The question replaced by `told`, without its buttons.
fn closed(told: reply::Said) -> InteractionResponse {
    InteractionResponse {
        kind: InteractionResponseType::UpdateMessage,
        data: Some(InteractionResponseData {
            content: Some(told.text),
            embeds: Some(told.embeds),
            components: Some(vec![]),
            allowed_mentions: Some(AllowedMentions::default()),
            ..Default::default()
        }),
    }
}

const ANSWERED: &str = "reply.confirm.answered";

/// The entry that says why a press found no question row. The nonce is the snowflake of the command, so its age
/// tells an answered question (gone within the time) from one that timed out.
fn gone_text(nonce: u64, now_ms: u64) -> &'static str {
    let asked_ms = (nonce >> 22) + 1_420_070_400_000;
    if now_ms.saturating_sub(asked_ms) < CONFIRM_TTL.as_millis() as u64 {
        ANSWERED
    } else {
        TIMED_OUT
    }
}

impl Pending {
    /// The reply once the command ran: result embeds, or text when compact.
    pub(crate) fn reply(
        &self,
        t: &reply::ModTexts,
        done: &[Applied],
        compact: bool,
        default_timeout_s: u64,
    ) -> (String, Vec<Embed>) {
        let actor = self.actor.get();
        match &self.action {
            Action::Mod {
                kind,
                reason,
                duration_s,
                flags,
                ..
            } => {
                let name = result_kind(*kind, *flags);
                let compact = compact || flags.compact;
                if compact {
                    return reply::summary_with(t, name, true, done, reason.as_deref(), actor);
                }
                let dur = match kind {
                    ModKind::Timeout => duration_s.or(Some(default_timeout_s as i64)),
                    _ => *duration_s,
                };
                let e = reply::result_embeds_with(
                    t,
                    name,
                    done,
                    (*flags, dur),
                    reason.as_deref(),
                    actor,
                );
                (String::new(), e)
            }
            Action::CloseTickets { .. } => (
                done.first().map(|d| d.text.clone()).unwrap_or_default(),
                vec![],
            ),
            Action::Ext { cmd, reason, .. } => modcmds::reply_parts(
                t,
                cmd.result_kind(),
                compact,
                done,
                (cmd.flags(), None),
                reason.as_deref(),
                actor,
            ),
        }
    }
}

/// The result embed key: a ban with `?s` is a softban.
pub(crate) fn result_kind(kind: ModKind, flags: Flags) -> &'static str {
    match kind {
        ModKind::Ban if flags.soft => "softban",
        k => k.name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brim_logging::embed::{ARROW, BLUE};

    fn job(kind: ModKind, unsure: bool, hack: bool) -> Pending {
        let flags = Flags {
            hack,
            ..Default::default()
        };
        let action = Action::Mod {
            kind,
            targets: vec![Id::new(5)],
            reason: None,
            duration_s: None,
            flags,
            proof: None,
            unsure,
            reply: None,
        };
        Pending::new(Id::new(1), Id::new(9), action)
    }

    #[test]
    fn asks_only_for_ids_and_names() {
        for kind in [ModKind::Ban, ModKind::Kick, ModKind::Timeout, ModKind::Warn] {
            assert!(
                !job(kind, false, false).needs_confirm(true),
                "mention {kind:?}"
            );
            assert!(
                job(kind, true, false).needs_confirm(true),
                "id or name {kind:?}"
            );
        }
        // A hackban always asks, a mention or not.
        assert!(job(ModKind::Ban, false, true).needs_confirm(true));
        // The settings switch off: nothing asks, not even a hackban by id.
        assert!(!job(ModKind::Ban, true, true).needs_confirm(false));
        assert!(!job(ModKind::Warn, true, false).needs_confirm(false));
    }

    fn ext(cmd: ModCmd, unsure: bool) -> Pending {
        let action = Action::Ext {
            cmd,
            channel: Id::new(2),
            reason: None,
            proof: None,
            users: vec![5],
            unsure,
            dm: false,
            reply: None,
        };
        Pending::new(Id::new(1), Id::new(9), action)
    }

    #[test]
    fn quarantine_and_unban_ask_for_ids_too() {
        for cmd in [
            ModCmd::Quarantine { user: 5 },
            ModCmd::Unquarantine { user: 5 },
            ModCmd::Unban { user: 5 },
        ] {
            assert!(!ext(cmd.clone(), false).needs_confirm(true), "{cmd:?}");
            assert!(ext(cmd, true).needs_confirm(true));
        }
        assert!(!ext(ModCmd::Untimeout { user: 5 }, true).needs_confirm(true));
        assert!(ext(ModCmd::Hackban { user: 5 }, false).needs_confirm(true));
    }

    #[test]
    fn ten_second_question() {
        assert_eq!(CONFIRM_TTL, Duration::from_secs(10));
        let q = job(ModKind::Warn, true, false).question(None);
        assert_eq!(q.title.as_deref(), Some("Do you want to proceed?"));
        assert_eq!(q.color, Some(BLUE));
        // Byte-equal to the prompt the code built before it became an entry.
        assert_eq!(
            q.description.as_deref(),
            Some(
                format!("{ARROW} **Target:**\n<@5> [`5`]\nPress Yes or No. You have 10 seconds.")
                    .as_str()
            )
        );
    }

    #[test]
    fn only_the_moderator_who_ran_it_answers() {
        let (nonce, actor, yes) = parse_answer("confirm:77:9:y").unwrap();
        assert_eq!((nonce, actor, yes), (77, 9, true));
        assert!(may_answer(Some(9), actor));
        // Anyone else, owner or trusted admin included, is refused before the row is touched.
        assert!(!may_answer(Some(1), actor));
        assert!(!may_answer(None, actor));
        assert_eq!(parse_answer("confirm:77:9:n").map(|a| a.2), Some(false));
        assert!(parse_answer("other:77:9:y").is_none());
    }

    #[test]
    fn a_repeat_press_is_answered_not_timed_out() {
        let asked_ms = 1_700_000_000_000u64;
        let nonce = (asked_ms - 1_420_070_400_000) << 22;
        assert_eq!(gone_text(nonce, asked_ms + 2_000), ANSWERED);
        assert_eq!(gone_text(nonce, asked_ms + 11_000), TIMED_OUT);
        let text = brim_logging::botmsg::render(TIMED_OUT, None, &[("seconds", "10")]);
        assert_eq!(
            text["content"],
            "This confirmation timed out after 10 seconds, so nothing was done. Run the command again."
        );
    }
}
