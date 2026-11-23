use crate::{
    cache::CachedMessage,
    embed::{self, Log, GREEN, NO, ORANGE, RED, YES},
    Entry, LogEvent, LogKind,
};
use serde_json::Value;
use twilight_model::{
    channel::{Channel, Message},
    guild::{
        audit_log::{AuditLogEntry, AuditLogEventType as A, AuditLogOptionalEntryInfo},
        Permissions,
    },
    id::{
        marker::{ChannelMarker, MessageMarker, UserMarker},
        Id,
    },
    poll::{Poll, PollAnswer},
    user::User,
};

/// Discord snowflakes count milliseconds from this epoch.
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// The labels of the link button under a log; the dashboard preview draws the same ones.
pub const GO_MESSAGE: &str = "Go to Message";
pub const GO_POLL: &str = "Go to Poll";
pub const GO_CHANNEL: &str = "Go to Channel";

/// A link button under a log: label and address.
pub type Link = (&'static str, String);

fn message_link(label: &'static str, guild: u64, channel: u64, id: u64) -> Link {
    (
        label,
        format!("https://discord.com/channels/{guild}/{channel}/{id}"),
    )
}

/// What the logger keeps of a poll: its question for the delete log, its answers for the votes.
#[derive(Clone, Debug)]
pub(crate) struct PollInfo {
    pub question: String,
    pub author: Id<UserMarker>,
    pub answers: Vec<(u8, String)>,
}

impl PollInfo {
    pub fn of(p: &Poll, author: Id<UserMarker>) -> Self {
        Self {
            author,
            question: p.question.text.clone().unwrap_or_default(),
            answers: p
                .answers
                .iter()
                .map(|a| (a.answer_id, answer_text(a)))
                .collect(),
        }
    }

    fn answer(&self, id: u8) -> String {
        self.answers
            .iter()
            .find(|a| a.0 == id)
            .map_or(format!("Answer {id}"), |a| a.1.clone())
    }
}

fn answer_text(a: &PollAnswer) -> String {
    let m = &a.poll_media;
    let emoji = m.emoji.as_ref().and_then(|e| e.name.as_deref());
    match (emoji, m.text.as_deref()) {
        (Some(e), Some(t)) => format!("{e} {t}"),
        (None, Some(t)) => t.into(),
        (Some(e), None) => e.into(),
        (None, None) => format!("Answer {}", a.answer_id),
    }
}

fn poll_head(title: &str, color: u32, msg: &Message, poll: &Poll) -> Log {
    Log::new(title, color)
        .fact("Author", "member", &embed::user(msg.author.id.get()))
        .fact("Channel", "channel", &embed::channel(msg.channel_id.get()))
        .fact(
            "Question",
            "question",
            &embed::clip(poll.question.text.as_deref().unwrap_or("(no text)"), 300),
        )
}

/// `log` with `lines` below a bold heading, the lines also as one value `{var}`.
fn block(mut log: Log, heading: &str, var: &str, lines: &[String]) -> Log {
    log = log.section(heading);
    for l in lines {
        log = log.text(l);
    }
    log.var(var, &lines.join("\n"))
}

pub fn poll_create(msg: &Message, poll: &Poll) -> (Log, Option<Link>) {
    let ends = poll
        .expiry
        .map(|end| format!("<t:{0}:F> (<t:{0}:R>)", end.as_secs()));
    let log = poll_head("Poll Created", GREEN, msg, poll)
        .fact(
            "Multiple Answers",
            "multiple",
            if poll.allow_multiselect { "yes" } else { "no" },
        )
        .fact("Ends", "ends", ends.as_deref().unwrap_or(""))
        .time();
    let answers: Vec<String> = poll
        .answers
        .iter()
        .map(|a| format!("{}. {}", a.answer_id, embed::clip(&answer_text(a), 100)))
        .collect();
    (block(log, "Answers", "answers", &answers), poll_link(msg))
}

/// The results of a poll that has ended: each answer with its votes.
pub fn poll_end(msg: &Message, poll: &Poll) -> (Log, Option<Link>) {
    let log = poll_head("Poll Ended", ORANGE, msg, poll).time();
    let results: Vec<String> = poll
        .answers
        .iter()
        .map(|a| {
            let votes = poll.results.as_ref().map_or(0, |r| {
                r.answer_counts
                    .iter()
                    .find(|c| c.id == a.answer_id)
                    .map_or(0, |c| c.count)
            });
            format!(
                "{}. {}: **{votes}**",
                a.answer_id,
                embed::clip(&answer_text(a), 100)
            )
        })
        .collect();
    (block(log, "Results", "results", &results), poll_link(msg))
}

fn poll_link(msg: &Message) -> Option<Link> {
    msg.guild_id
        .map(|g| message_link(GO_POLL, g.get(), msg.channel_id.get(), msg.id.get()))
}

/// A vote added or taken back. `poll` names the question and answer when the logger saw the poll.
pub fn poll_vote(
    add: bool,
    v: (Id<UserMarker>, Id<ChannelMarker>, Id<MessageMarker>, u8),
    guild: u64,
    poll: Option<&PollInfo>,
) -> (Log, Option<Link>) {
    let (user, channel, message, answer) = v;
    let log = Log::new(
        if add {
            "Poll Vote Added"
        } else {
            "Poll Vote Removed"
        },
        if add { GREEN } else { RED },
    )
    .fact("Target", "member", &embed::user(user.get()))
    .fact("Channel", "channel", &embed::channel(channel.get()))
    .fact(
        "Poll",
        "poll",
        &poll.map_or(format!("`{message}`"), |p| embed::clip(&p.question, 200)),
    )
    .fact(
        "Answer",
        "answer",
        &poll.map_or(format!("Answer {answer}"), |p| {
            embed::clip(&p.answer(answer), 100)
        }),
    )
    .time();
    (
        log,
        Some(message_link(GO_POLL, guild, channel.get(), message.get())),
    )
}

/// An announcement the server published to the channels that follow it.
pub fn message_publish(msg: &Message) -> (Log, Option<Link>) {
    let log = Log::new("Message Published", GREEN)
        .fact("Author", "member", &embed::user(msg.author.id.get()))
        .fact("Channel", "channel", &embed::channel(msg.channel_id.get()))
        .time();
    let link = msg
        .guild_id
        .map(|g| message_link(GO_MESSAGE, g.get(), msg.channel_id.get(), msg.id.get()));
    (log, link)
}

/// Someone turned the notification of a scheduled event on or off.
pub fn event_subscribe(add: bool, user: Id<UserMarker>, event: u64, name: Option<&str>) -> Log {
    Log::new(
        if add {
            "Event Subscribed"
        } else {
            "Event Unsubscribed"
        },
        if add { GREEN } else { RED },
    )
    .fact("Target", "member", &embed::user(user.get()))
    .fact(
        "Event",
        "event",
        &name.map_or(format!("`{event}`"), |n| embed::clip(n, 100)),
    )
    .time()
}

pub fn boost_level(old: u8, new: u8, boosts: Option<u64>) -> Log {
    Log::new("Boost Level Changed", if new > old { GREEN } else { RED })
        .line("Level", &format!("{old} → {new}"))
        .var("before", &old.to_string())
        .var("after", &new.to_string())
        .fact(
            "Boosts",
            "count",
            &boosts.map(|n| n.to_string()).unwrap_or_default(),
        )
        .time()
}

/// A thread Discord archived after its time without messages ran out.
pub fn thread_auto_archive(thread: &Channel) -> (Log, Option<Link>) {
    let name = thread.name.as_deref().map(|n| embed::clip(n, 100));
    let inactive = thread.thread_metadata.as_ref().map(|m| {
        let mins = i64::from(m.auto_archive_duration.number());
        embed::duration(mins * 60)
    });
    let log = Log::new("Thread Archived Automatically", ORANGE)
        .fact("Thread", "channel", &embed::channel(thread.id.get()))
        .fact("Name", "thread_name", name.as_deref().unwrap_or(""))
        .fact(
            "Inactive For",
            "inactive",
            inactive.as_deref().unwrap_or(""),
        );
    let link = thread.guild_id.map(|g| {
        (
            GO_CHANNEL,
            format!("https://discord.com/channels/{g}/{}", thread.id),
        )
    });
    (log.time(), link)
}

pub fn message_edit(old: Option<&str>, msg: &Message) -> (Log, Option<Link>) {
    let log = Log::new("Message Edited", ORANGE)
        .fact("Author", "member", &embed::user(msg.author.id.get()))
        .fact("Channel", "channel", &embed::channel(msg.channel_id.get()))
        .time();
    let head = log.lines_text().len();
    let log = log
        .section("Changes")
        .text(&embed::label_head("Content"))
        .diff(
            Some(old.unwrap_or("(previous content not cached)")),
            Some(&crate::cache::shown(&msg.content, msg.embeds.len())),
        );
    let changes = log.lines_text()[head..]
        .trim_start_matches('\n')
        .to_string();
    let embed = log.var("changes", &changes);
    let link = msg.guild_id.map(|g| {
        let url = format!(
            "https://discord.com/channels/{g}/{}/{}",
            msg.channel_id, msg.id
        );
        (GO_MESSAGE, url)
    });
    (embed, link)
}

/// `poll`: the question and author, when the deleted message was a poll the logger had seen.
pub fn message_delete(
    id: Id<MessageMarker>,
    channel: Id<ChannelMarker>,
    cached: Option<&CachedMessage>,
    deleter: Option<Id<UserMarker>>,
    poll: Option<(&str, Id<UserMarker>)>,
) -> Log {
    let sent = ((id.get() >> 22) + DISCORD_EPOCH_MS) / 1000;
    let mut log = Log::new(
        if poll.is_some() {
            "Poll Deleted"
        } else {
            "Message Deleted"
        },
        RED,
    );
    // No deleter means the author deleted it (or nobody knows); either way there is no name to show.
    let user = |id: Option<Id<UserMarker>>| id.map(|i| embed::user(i.get())).unwrap_or_default();
    log = log
        .fact("Responsible", "by", &user(deleter))
        .fact(
            "Author",
            "member",
            &user(cached.map(|c| c.author).or(poll.map(|p| p.1))),
        )
        .fact("Channel", "channel", &embed::channel(channel.get()))
        .fact("Sent", "sent", &format!("<t:{sent}:F>"))
        .time();
    if let Some((q, _)) = poll {
        return log.fact("Question", "question", &embed::clip(q, 300));
    }
    log = log.text(&embed::label_head("Content"));
    let head = log.lines_text().len();
    log = log.diff(
        Some(&cached.map_or("(content not cached)".into(), CachedMessage::text)),
        None,
    );
    let content = log.lines_text()[head..]
        .trim_start_matches('\n')
        .to_string();
    let files = cached.map(|c| c.attachments.join(", ")).unwrap_or_default();
    log.var("content", &content)
        .fact("Attachments", "attachments", &files)
}

/// The name of a bulk delete's file, from its oldest message id, so the embed and file match.
pub fn bulk_file(oldest: Id<MessageMarker>) -> String {
    let (mut n, mut code) = (oldest.get(), Vec::new());
    while n > 0 {
        code.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
        n /= 36;
    }
    code.reverse();
    format!("bulk-{}.txt", String::from_utf8(code).expect("ascii"))
}

/// `saved` is how many of the deleted messages are in the file named `file`.
pub fn bulk_delete(channel: Id<ChannelMarker>, count: usize, saved: usize, file: &str) -> Log {
    let content = match saved {
        0 => "not cached".into(),
        n => format!("`{n}/{count}` saved in `{file}` below"),
    };
    Log::new("Message Delete Bulk", RED)
        .fact("Channel", "channel", &embed::channel(channel.get()))
        .time()
        .fact("Count", "count", &count.to_string())
        .fact("Content", "saved", &content)
}

pub fn member_add(user: &User) -> Log {
    member_log("Member Join", GREEN, user)
}

fn member_log(title: &str, color: u32, user: &User) -> Log {
    let created = ((user.id.get() >> 22) + DISCORD_EPOCH_MS) / 1000;
    Log::new(title, color)
        .thumbnail(user)
        .fact("Target", "member", &embed::user(user.id.get()))
        .time()
        .fact(
            "Creation",
            "created",
            &format!("<t:{created}:F> (<t:{created}:R>)"),
        )
}

/// A voice join, leave or move; None when the member stayed in the same channel (mute, stream...).
pub fn voice(
    user: Id<UserMarker>,
    old: Option<Id<ChannelMarker>>,
    new: Option<Id<ChannelMarker>>,
) -> Option<Log> {
    let ch = |c: Id<ChannelMarker>| embed::channel(c.get());
    let log = match (old, new) {
        (None, Some(n)) => Log::new("Voice Join", GREEN).fact("Channel", "channel", &ch(n)),
        (Some(o), None) => Log::new("Voice Leave", RED).fact("Channel", "channel", &ch(o)),
        (Some(o), Some(n)) if o != n => Log::new("Voice Move", ORANGE)
            .fact("From", "from", &ch(o))
            .fact("To", "to", &ch(n)),
        _ => return None,
    };
    Some(
        log.fact("Target", "member", &embed::user(user.get()))
            .time(),
    )
}

pub fn member_remove(user: &User) -> Log {
    member_log("Member Leave", RED, user)
}

/// Turns a guild audit log entry into a log, or `None` for types we do not log.
pub fn audit(e: &AuditLogEntry) -> Option<Entry> {
    let kind = match e.action_type {
        A::GuildUpdate | A::ApplicationCommandPermissionUpdate => LogKind::Server,
        A::AutoModerationRuleCreate | A::AutoModerationRuleUpdate | A::AutoModerationRuleDelete => {
            LogKind::AutoModRule
        }
        A::StageInstanceCreate
        | A::StageInstanceUpdate
        | A::StageInstanceDelete
        | A::ChannelCreate
        | A::ChannelUpdate
        | A::ChannelDelete
        | A::ChannelOverwriteCreate
        | A::ChannelOverwriteUpdate
        | A::ChannelOverwriteDelete => LogKind::Channel,
        A::RoleCreate | A::RoleUpdate | A::RoleDelete => LogKind::Role,
        A::MemberKick
        | A::MemberPrune
        | A::MemberBanAdd
        | A::MemberBanRemove
        | A::MemberUpdate
        | A::MemberRoleUpdate
        | A::MemberMove
        | A::MemberDisconnect
        | A::BotAdd
        | A::IntegrationDelete => LogKind::Member,
        A::InviteCreate | A::InviteUpdate | A::InviteDelete => LogKind::Invite,
        A::WebhookCreate | A::WebhookUpdate | A::WebhookDelete => LogKind::Webhook,
        A::EmojiCreate | A::EmojiUpdate | A::EmojiDelete => LogKind::Emoji,
        A::StickerCreate | A::StickerUpdate | A::StickerDelete => LogKind::Sticker,
        A::GuildScheduledEventCreate
        | A::GuildScheduledEventUpdate
        | A::GuildScheduledEventDelete => LogKind::ScheduledEvent,
        A::ThreadCreate | A::ThreadUpdate | A::ThreadDelete => LogKind::Thread,
        A::MessagePin | A::MessageUnpin => LogKind::Message,
        // Message deletes are logged from the gateway event, which has the cached content.
        _ => return None,
    };

    let a = e.action_type;
    let name = format!("{a:?}");
    let created = name.ends_with("Create") || matches!(a, A::BotAdd | A::MessagePin);
    let deleted = name.ends_with("Delete");
    let opts = e.options.as_ref();
    let overwrite = matches!(
        a,
        A::ChannelOverwriteCreate | A::ChannelOverwriteUpdate | A::ChannelOverwriteDelete
    );

    let title = match a {
        A::AutoModerationRuleCreate => "Discord AutoMod Rule Created".into(),
        A::AutoModerationRuleUpdate => "Discord AutoMod Rule Updated".into(),
        A::AutoModerationRuleDelete => "Discord AutoMod Rule Deleted".into(),
        A::IntegrationDelete => "App Remove".into(),
        A::ApplicationCommandPermissionUpdate => "App Command Permission Update".into(),
        A::StageInstanceCreate => "Stage Start".into(),
        A::StageInstanceUpdate => "Stage Update".into(),
        A::StageInstanceDelete => "Stage End".into(),
        _ => words(&name),
    };
    let by = e.user_id.map_or("Unknown".into(), |u| embed::user(u.get()));
    let mut log = Log::new(&title, color(a))
        .line("Responsible", &by)
        .var("by", &by);
    // Overwrite entries target the channel; The log shows the role or member as the target.
    let channel = if overwrite {
        e.target_id.map(Id::get)
    } else {
        opts.and_then(|o| o.channel_id).map(Id::get)
    };
    let channel_ref = channel.map(embed::channel).unwrap_or_default();
    if channel.is_some() {
        log = log.line("Channel", &channel_ref);
    }
    // A stage's target is the stage instance, not a channel; the Channel line above names the stage.
    let stage = matches!(
        a,
        A::StageInstanceCreate | A::StageInstanceUpdate | A::StageInstanceDelete
    );
    let target = if stage {
        None
    } else if overwrite {
        opts.and_then(|o| o.id).map(|id| {
            if opts.and_then(|o| o.kind.as_deref()) == Some("1") {
                embed::user(id.get())
            } else {
                embed::role(id.get())
            }
        })
    } else {
        e.target_id.map(|t| match (kind, change_name(e)) {
            // A webhook id alone says nothing; its name is in the changes.
            (LogKind::Webhook, Some(n)) => format!("{n} `{t}`"),
            _ => target_ref(kind, t.get()),
        })
    };
    // Discord renders a mention of the @everyone role (its id is the guild id) as "@@everyone".
    let everyone = e.guild_id.map(|g| format!("<@&{g}>"));
    let target = target.map(|t| match &everyone {
        Some(m) if t.starts_with(m) => t.replacen(m, "@everyone", 1),
        _ => t,
    });
    if let Some(t) = &target {
        log = log.line("Target", t);
    }
    log = log.time();
    let reason = e.reason.as_deref().filter(|r| !r.trim().is_empty());
    if let Some(reason) = reason {
        log = log.line("Reason", reason);
    }
    // Every placeholder of the entry gets a value, also an empty one (an unset one stays `{name}`).
    log = log
        .var("channel", &channel_ref)
        .var("target", target.as_deref().unwrap_or(""))
        .var("reason", reason.unwrap_or(""));
    let head = log.lines_text().len();

    log = changes(log, e, kind, overwrite, created, deleted);
    // twilight reads only the change keys it knows; rule and app permission settings are not among them.
    let hidden = |e: &AuditLogEntry| {
        e.changes
            .iter()
            .any(|c| serde_json::to_value(c).is_ok_and(|v| v["key"] == "other"))
    };
    if matches!(
        a,
        A::AutoModerationRuleCreate
            | A::AutoModerationRuleUpdate
            | A::AutoModerationRuleDelete
            | A::ApplicationCommandPermissionUpdate
    ) && hidden(e)
    {
        log = log.line(
            "Not Shown",
            "Some changed settings: Discord does not send them in a form Brim can read.",
        );
    }
    log = additional_info(log, opts);
    // The changes, the "Not Shown" line and the extra information: one block an edit places whole.
    let rest = log.lines_text()[head..]
        .trim_start_matches('\n')
        .to_string();
    log = log.var("changes", &rest);

    // Logs link the channel of channel and thread logs, except when it is gone.
    let link = match (kind, e.guild_id, channel.or(e.target_id.map(Id::get))) {
        (LogKind::Channel | LogKind::Thread, Some(g), Some(c)) if !deleted => {
            Some((GO_CHANNEL, format!("https://discord.com/channels/{g}/{c}")))
        }
        _ => None,
    };

    Some(Entry {
        kind,
        event: LogEvent::from_action(a),
        channel: None,
        embed: log,
        link,
        actor: e.user_id,
    })
}

/// "Settings" (created), "Previous Settings" (deleted) or "Changes" (updated) section.
fn changes(
    mut log: Log,
    e: &AuditLogEntry,
    kind: LogKind,
    overwrite: bool,
    created: bool,
    deleted: bool,
) -> Log {
    if !e.changes.is_empty() {
        log = log.section(if created {
            "Settings"
        } else if deleted {
            "Previous Settings"
        } else {
            "Changes"
        });
    }
    for change in &e.changes {
        // The serde shape is {"key", "old_value", "new_value"} for every change variant.
        let Ok(v) = serde_json::to_value(change) else {
            continue;
        };
        let (old, new) = (v.get("old_value"), v.get("new_value"));
        log = match v["key"].as_str().unwrap_or("?") {
            // Changes twilight cannot decode, and the id and type the target line already shows.
            "other" | "id" => log,
            "type" if overwrite => log,
            // The target line names a new or removed webhook already.
            "name" if kind == LogKind::Webhook && (created || deleted) => log,
            "$add" => log.line("Roles Added", &roles(new)),
            "$remove" => log.line("Roles Removed", &roles(new)),
            key @ ("permissions" | "allow" | "deny") => {
                let label = match key {
                    "allow" => format!("{YES} **Allowed:**"),
                    "deny" => format!("{NO} **Denied:**"),
                    _ => embed::label_head("Permissions"),
                };
                let (added, removed) = perm_diff(old, new);
                if added.is_empty() && removed.is_empty() {
                    continue;
                }
                let mut l = log.text(&label);
                if !added.is_empty() {
                    l = l.text(&format!("{} {added}", embed::label_head("Additions")));
                }
                if !removed.is_empty() {
                    l = l.text(&format!("{} {removed}", embed::label_head("Removals")));
                }
                l
            }
            // A new or removed thing's unset values (off, 0, empty) carry no information.
            key if (created || deleted)
                && key != "type"
                && unset(if created { new } else { old }) =>
            {
                log
            }
            key if created => log.line(&label(key), &value(key, kind, new)),
            key if deleted => log.line(&label(key), &value(key, kind, old)),
            key => log
                .text(&embed::label_head(&label(key)))
                .diff(Some(&show(key, kind, old)), Some(&show(key, kind, new))),
        };
    }

    log
}

/// The audit entry's options, as the "Additional Information" section. An overwrite's id, type
/// and role name are left out: the target line already shows them.
fn additional_info(mut log: Log, opts: Option<&AuditLogOptionalEntryInfo>) -> Log {
    let info: Vec<(&str, String)> = opts
        .map(|o| {
            [
                ("Count", o.count.clone()),
                ("Delete Member Days", o.delete_member_days.clone()),
                ("Members Removed", o.members_removed.clone()),
                ("Message Id", o.message_id.map(|i| i.to_string())),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect()
        })
        .unwrap_or_default();
    if !info.is_empty() {
        log = log.section("Additional Information");
        for (k, v) in info {
            log = log.line(k, &format!("`{v}`"));
        }
    }

    log
}

/// Green for things added, red for things removed, orange for changes.
fn color(a: A) -> u32 {
    let name = format!("{a:?}");
    if name.ends_with("Create") || matches!(a, A::BotAdd | A::MemberBanRemove | A::MessagePin) {
        GREEN
    } else if name.ends_with("Delete")
        || matches!(
            a,
            A::MemberKick
                | A::MemberPrune
                | A::MemberBanAdd
                | A::MemberDisconnect
                | A::MessageUnpin
        )
    {
        RED
    } else {
        ORANGE
    }
}

/// The `$add` and `$remove` changes list partial roles: `[{"id", "name"}]`.
fn roles(v: Option<&Value>) -> String {
    let list: Vec<String> = v
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| r["id"].as_str().map(|id| format!("<@&{id}>")))
        .collect();
    if list.is_empty() {
        "none".into()
    } else {
        list.join(" ")
    }
}

/// Permission bitfields (strings) as "Create Instant Invite, View Channel" for the added and
/// removed bits.
fn perm_diff(old: Option<&Value>, new: Option<&Value>) -> (String, String) {
    let bits = |v: Option<&Value>| -> u64 {
        v.and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    let (old, new) = (bits(old), bits(new));
    (perm_names(new & !old), perm_names(old & !new))
}

/// Permission bits as "Create Instant Invite, View Channel"; bits twilight does not know yet
/// show as "Bit 48" so a change to only those is not empty.
pub fn perm_names(bits: u64) -> String {
    let known = Permissions::from_bits_truncate(bits);
    let unknown = (0..64)
        .filter(|i| (bits & !known.bits()) & (1 << i) != 0)
        .map(|i| format!("Bit {i}"));
    known
        .iter_names()
        .map(|(n, _)| {
            n.split('_')
                .map(|w| {
                    let mut c = w.chars();
                    c.next().map_or(String::new(), |f| {
                        f.to_string() + &c.as_str().to_lowercase()
                    })
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .chain(unknown)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `name` change's new value, else its old one.
fn change_name(e: &AuditLogEntry) -> Option<String> {
    e.changes.iter().find_map(|c| {
        let v = serde_json::to_value(c).ok()?;
        (v["key"] == "name").then(|| {
            v["new_value"]
                .as_str()
                .or(v["old_value"].as_str())
                .map(str::to_owned)
        })?
    })
}

fn target_ref(kind: LogKind, id: u64) -> String {
    match kind {
        LogKind::Channel | LogKind::Thread => embed::channel(id),
        LogKind::Role => embed::role(id),
        // A pin's target is the message author.
        LogKind::Member | LogKind::Message => embed::user(id),
        _ => format!("`{id}`"),
    }
}

/// A settings line value: like [`show`], with mentions for ids (they would not render in a diff).
fn value(key: &str, kind: LogKind, v: Option<&Value>) -> String {
    let id = v.and_then(Value::as_str).and_then(|s| s.parse().ok());
    match (key, id) {
        ("channel_id", Some(id)) => embed::channel(id),
        ("inviter_id" | "application_id", Some(id)) => embed::user(id),
        _ => show(key, kind, v),
    }
}

/// A change value as the log shows it: names for channel types, hex colours.
/// No value, `false`, `0`, `""` or `[]`.
fn unset(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => !b,
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::String(s)) => s.is_empty() || s == "0",
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
    }
}

fn show(key: &str, kind: LogKind, v: Option<&Value>) -> String {
    match (key, v) {
        (_, None | Some(Value::Null)) => "none".into(),
        ("color", Some(Value::Number(n))) => format!("#{:06X}", n.as_u64().unwrap_or(0)),
        ("permission_overwrites", Some(Value::Array(a))) => a.len().to_string(),
        ("type", Some(Value::Number(n))) if matches!(kind, LogKind::Channel | LogKind::Thread) => {
            channel_type(n.as_u64().unwrap_or(u64::MAX)).into()
        }
        ("type", Some(Value::Number(n))) if kind == LogKind::Webhook => match n.as_u64() {
            Some(1) => "Incoming",
            Some(2) => "Channel Follower",
            Some(3) => "Application",
            _ => "Unknown",
        }
        .into(),
        (_, Some(Value::String(s))) => s.clone(),
        (_, Some(v)) => v.to_string(),
    }
}

fn channel_type(t: u64) -> &'static str {
    match t {
        0 => "Text",
        2 => "Voice",
        4 => "Category",
        5 => "Announcement",
        10 => "Announcement Thread",
        11 => "Public Thread",
        12 => "Private Thread",
        13 => "Stage",
        15 => "Forum",
        16 => "Media",
        _ => "Other",
    }
}

/// `CamelCase` to `Camel Case`, `snake_case` to `Snake case`.
/// A change key as a label: `channel_id` reads "Channel", since the value is the channel itself.
fn label(key: &str) -> String {
    words(key.strip_suffix("_id").unwrap_or(key))
}

pub(crate) fn words(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        match ch {
            '_' => out.push(' '),
            c if i == 0 => out.extend(c.to_uppercase()),
            c if c.is_uppercase() => {
                out.push(' ');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use twilight_model::channel::message::Embed;

    #[test]
    fn splits_camel_case() {
        assert_eq!(words("ChannelOverwriteCreate"), "Channel Overwrite Create");
        assert_eq!(words("rate_limit_per_user"), "Rate limit per user");
    }

    #[test]
    fn colours_follow_the_action() {
        assert_eq!(color(A::RoleCreate), GREEN);
        assert_eq!(color(A::MemberBanAdd), RED);
        assert_eq!(color(A::RoleUpdate), ORANGE);
    }

    #[test]
    fn permission_bits_become_names() {
        let (added, removed) = perm_diff(
            Some(&Value::String("1024".into())),
            Some(&Value::String("3072".into())),
        );
        assert_eq!(added, "Send Messages");
        assert_eq!(removed, "");
    }

    #[test]
    fn unknown_permission_bits_are_named() {
        let (added, _) = perm_diff(
            Some(&Value::String("0".into())),
            Some(&Value::String((1u64 << 63).to_string())),
        );
        assert_eq!(added, "Bit 63");
    }

    #[test]
    fn overwrite_shows_channel_and_role_target_with_denied_names() {
        let e: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": 13, "id": "1", "guild_id": "9", "user_id": "10", "target_id": "30",
            "options": { "id": "40", "type": "0", "role_name": "Quarantine" },
            "changes": [
                { "key": "id", "new_value": "40" },
                { "key": "type", "new_value": 0 },
                { "key": "deny", "new_value": "2048" }
            ]
        }))
        .unwrap();
        let entry = audit(&e).unwrap();
        let d = entry.embed.clone().build().description.unwrap();
        assert!(d.contains("▸ **Channel:** <#30> *`30`*\n▸ **Target:** <@&40> *`40`*"));
        assert!(d.ends_with("**Settings**\n❌ **Denied:**\n▸ **Additions:** Send Messages"));
        assert!(!d.contains("Additional Information"));
        assert_eq!(entry.link.unwrap().1, "https://discord.com/channels/9/30");
    }

    #[test]
    fn created_things_skip_unset_values_and_name_webhook_types() {
        let e: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": 50, "id": "1", "guild_id": "9", "user_id": "10", "target_id": "60",
            "changes": [
                { "key": "channel_id", "new_value": "30" },
                { "key": "name", "new_value": "hook" },
                { "key": "type", "new_value": 1 },
                { "key": "avatar_hash", "new_value": null },
                { "key": "nsfw", "new_value": false }
            ]
        }))
        .unwrap();
        let d = audit(&e).unwrap().embed.build().description.unwrap();
        assert!(
            d.ends_with("**Settings**\n▸ **Channel:** <#30> *`30`*\n▸ **Type:** Incoming"),
            "{d}"
        );
    }

    #[test]
    fn everyone_overwrite_target_has_one_at_sign() {
        let e: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": 15, "id": "1", "guild_id": "9", "user_id": "10", "target_id": "30",
            "options": { "id": "9", "type": "0", "role_name": "@everyone" },
            "changes": [{ "key": "deny", "old_value": "2048" }]
        }))
        .unwrap();
        let d = audit(&e).unwrap().embed.build().description.unwrap();
        assert!(d.contains("▸ **Target:** @everyone *`9`*"));
    }

    #[test]
    fn change_values_read_well() {
        let e: AuditLogEntry = serde_json::from_value(serde_json::json!({
            "action_type": 10, "id": "1", "guild_id": "9", "user_id": "10", "target_id": "30",
            "changes": [
                { "key": "type", "new_value": 0 },
                { "key": "permission_overwrites", "new_value": [] },
                { "key": "id", "new_value": "30" }
            ]
        }))
        .unwrap();
        let d = audit(&e).unwrap().embed.build().description.unwrap();
        assert!(d.ends_with("**Settings**\n▸ **Type:** Text"), "{d}");
        let v = serde_json::json!("5");
        assert_eq!(value("channel_id", LogKind::Invite, Some(&v)), "<#5> *`5`*");
        assert_eq!(show("channel_id", LogKind::Invite, Some(&v)), "5");
        assert_eq!(
            show("color", LogKind::Role, Some(&serde_json::json!(3447003))),
            "#3498DB"
        );
    }

    fn entry(v: serde_json::Value) -> Entry {
        audit(&serde_json::from_value(v).unwrap()).unwrap()
    }

    /// The default entry of a log of group `kind`, filled with its values, as
    /// `events::framed_with` fills it; and the log as the code builds it.
    fn filled(kind: LogKind, log: &Log, stored: Option<&Value>) -> (String, Value, Embed) {
        let (built, own) = log.clone().parts();
        let kind = serde_json::to_value(kind).unwrap();
        let title = built.title.clone().unwrap();
        let key = crate::botmsg::log_type_key(&format!("log.{}", kind.as_str().unwrap()), &title)
            .unwrap();
        let mut vars = vec![
            ("title", title.as_str()),
            ("text", built.description.as_deref().unwrap_or("")),
        ];
        vars.extend(own.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let m = crate::botmsg::render(&key, stored, &vars);
        (key, m["embeds"][0].clone(), built)
    }

    fn rendered(e: &Entry, stored: Option<&Value>) -> (String, Value) {
        let (k, m, _) = filled(e.kind, &e.embed, stored);
        (k, m)
    }

    /// The default reads exactly as the built log, and the entry is one of facts.
    fn same(kind: LogKind, log: &Log) {
        let (key, m, built) = filled(kind, log, None);
        assert!(crate::botmsg::find(&key).unwrap().details(), "{key}");
        assert_eq!(m["title"].as_str(), built.title.as_deref(), "{key}");
        assert_eq!(
            m["description"].as_str(),
            built.description.as_deref(),
            "{key}"
        );
    }

    fn samples() -> Vec<Entry> {
        vec![
            // Overwrite: channel and role target, a permission block.
            entry(serde_json::json!({
                "action_type": 13, "id": "1", "guild_id": "9", "user_id": "10", "target_id": "30",
                "options": { "id": "40", "type": "0", "role_name": "Quarantine" },
                "changes": [{ "key": "deny", "new_value": "2048" }]
            })),
            // Update with a reason and a diff, no executor.
            entry(serde_json::json!({
                "action_type": 31, "id": "1", "guild_id": "9", "target_id": "40",
                "reason": "tidy up",
                "changes": [{ "key": "name", "old_value": "a", "new_value": "b" }]
            })),
            // Webhook created: named target, settings.
            entry(serde_json::json!({
                "action_type": 50, "id": "1", "guild_id": "9", "user_id": "10", "target_id": "60",
                "changes": [
                    { "key": "channel_id", "new_value": "30" },
                    { "key": "name", "new_value": "hook" },
                    { "key": "type", "new_value": 1 }
                ]
            })),
            // Prune: no target, no changes, extra information.
            entry(serde_json::json!({
                "action_type": 21, "id": "1", "guild_id": "9", "user_id": "10",
                "options": { "delete_member_days": "7", "members_removed": "3" }
            })),
        ]
    }

    #[test]
    fn every_audit_log_default_reads_as_the_code_builds_it() {
        for e in samples() {
            same(e.kind, &e.embed);
        }
    }

    #[test]
    fn an_edited_audit_log_places_each_fact() {
        let e = &samples()[1];
        let copy = serde_json::json!({"content": "", "embeds": [{
            "title": "Role renamed",
            "description": "{target} by {by} in {channel} ({reason})\n{changes}"
        }]});
        let (key, m) = rendered(e, Some(&copy));
        assert!(crate::botmsg::missing_text(&key, &copy).is_none());
        let d = m["description"].as_str().unwrap();
        assert!(
            d.starts_with("<@&40> *`40`* by Unknown in  (tidy up)\n**Changes**"),
            "{d}"
        );
        assert_eq!(m["title"], "Role renamed");
        // An edit that drops who did it is refused, and the default is sent for a stored one.
        let lost = serde_json::json!({"content": "", "embeds": [{
            "description": "{target} {channel} {reason} {changes}"
        }]});
        assert!(crate::botmsg::missing_text(&key, &lost).is_some());
        // A frame made before, with `{text}` in its footer only, keeps working.
        let old = serde_json::json!({"content": "", "embeds": [{
            "title": "{title}", "description": "Logged", "footer": {"text": "{text}"}
        }]});
        assert!(crate::botmsg::missing_text(&key, &old).is_none());
        let f = rendered(e, Some(&old)).1;
        assert_eq!(
            f["footer"]["text"].as_str(),
            e.embed.clone().build().description.as_deref()
        );
        assert_eq!(
            rendered(e, Some(&lost)).1["description"],
            rendered(e, None).1["description"]
        );
    }

    #[test]
    fn every_audit_log_type_of_the_event_groups_is_an_entry_of_facts() {
        for (kind, titles) in crate::botmsg::AUDIT_LOGS {
            let all = crate::botmsg::LOG_TYPES
                .iter()
                .find(|t| t.0 == *kind)
                .unwrap()
                .1;
            for t in *titles {
                assert!(all.contains(t), "{kind} {t}");
            }
        }
        for (kind, t, _) in crate::botmsg::EVENT_LOGS {
            let all = crate::botmsg::LOG_TYPES
                .iter()
                .find(|x| x.0 == *kind)
                .unwrap()
                .1;
            assert!(all.contains(t), "{kind} {t}");
        }
    }

    #[test]
    fn role_changes_show_mentions() {
        let v = serde_json::json!([{ "id": "5", "name": "a" }, { "id": "6", "name": "b" }]);
        assert_eq!(roles(Some(&v)), "<@&5> <@&6>");
        assert_eq!(roles(None), "none");
    }
}
