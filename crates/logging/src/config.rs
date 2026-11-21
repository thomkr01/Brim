use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use twilight_model::{
    guild::audit_log::AuditLogEventType as A,
    id::{
        marker::{ChannelMarker, RoleMarker, UserMarker},
        Id,
    },
};

/// Log types. Discord event types and our own log types share one routing table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    Message,
    Member,
    Role,
    Channel,
    Server,
    Invite,
    Webhook,
    Emoji,
    Sticker,
    ScheduledEvent,
    Thread,
    Report,
    AutoMod,
    /// Changes to Discord's own AutoMod rules (the audit log).
    AutoModRule,
    AntiNuke,
    Verification,
    JoinGate,
    JoinRaid,
    Panic,
    ModCommand,
    Appeal,
    /// Voice joins, leaves and moves; only sent once a channel is picked for them.
    Voice,
    /// Polls made and ended; the votes are events that need a channel of their own.
    Poll,
    /// A member's own avatar and name changes (the only logs that need stored member data).
    User,
}

/// A Discord event type that can get its own log channel. Names are the dashboard's option keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEvent {
    GuildUpdate,
    ChannelCreate,
    ChannelUpdate,
    ChannelDelete,
    ChannelOverwriteCreate,
    ChannelOverwriteUpdate,
    ChannelOverwriteDelete,
    MessagePin,
    MessageUnpin,
    MessageDelete,
    MessageDeleteBulk,
    MessageUpdate,
    MemberKick,
    MemberPrune,
    MemberBanAdd,
    MemberBanRemove,
    MemberUpdate,
    MemberRoleUpdate,
    MemberMove,
    MemberDisconnect,
    BotAdd,
    RoleCreate,
    RoleUpdate,
    RoleDelete,
    InviteCreate,
    InviteUpdate,
    InviteDelete,
    WebhookCreate,
    WebhookUpdate,
    WebhookDelete,
    EmojiCreate,
    EmojiUpdate,
    EmojiDelete,
    StickerCreate,
    StickerUpdate,
    StickerDelete,
    GuildScheduledEventCreate,
    GuildScheduledEventUpdate,
    GuildScheduledEventDelete,
    ThreadCreate,
    ThreadUpdate,
    ThreadDelete,
    AutoModRuleCreate,
    AutoModRuleUpdate,
    AutoModRuleDelete,
    AppRemove,
    AppCommandPermissionUpdate,
    /// A timeout that ran out by itself (a scheduled job; Discord sends no event).
    MemberTimeoutEnd,
    PollCreate,
    PollEnd,
    /// Votes name members and are many: only sent to a channel picked for this event ([`LogEvent::quiet`]).
    PollVoteAdd,
    PollVoteRemove,
    MessagePublish,
    /// Like the poll votes: only sent to a channel picked for the event.
    GuildScheduledEventSubscribe,
    GuildScheduledEventUnsubscribe,
    StageStart,
    StageUpdate,
    StageEnd,
    BoostLevelChange,
    /// A thread Discord archived after its inactivity time (no audit entry).
    ThreadAutoArchive,
    /// A member changed their own avatar, or their username or display name (member updates
    /// compared with the last seen values in `profile_snapshot`).
    UserAvatarUpdate,
    UserNameUpdate,
}

impl LogEvent {
    pub const ALL: [LogEvent; 62] = [
        LogEvent::GuildUpdate,
        LogEvent::ChannelCreate,
        LogEvent::ChannelUpdate,
        LogEvent::ChannelDelete,
        LogEvent::ChannelOverwriteCreate,
        LogEvent::ChannelOverwriteUpdate,
        LogEvent::ChannelOverwriteDelete,
        LogEvent::MessagePin,
        LogEvent::MessageUnpin,
        LogEvent::MessageDelete,
        LogEvent::MessageDeleteBulk,
        LogEvent::MessageUpdate,
        LogEvent::MemberKick,
        LogEvent::MemberPrune,
        LogEvent::MemberBanAdd,
        LogEvent::MemberBanRemove,
        LogEvent::MemberUpdate,
        LogEvent::MemberRoleUpdate,
        LogEvent::MemberMove,
        LogEvent::MemberDisconnect,
        LogEvent::BotAdd,
        LogEvent::RoleCreate,
        LogEvent::RoleUpdate,
        LogEvent::RoleDelete,
        LogEvent::InviteCreate,
        LogEvent::InviteUpdate,
        LogEvent::InviteDelete,
        LogEvent::WebhookCreate,
        LogEvent::WebhookUpdate,
        LogEvent::WebhookDelete,
        LogEvent::EmojiCreate,
        LogEvent::EmojiUpdate,
        LogEvent::EmojiDelete,
        LogEvent::StickerCreate,
        LogEvent::StickerUpdate,
        LogEvent::StickerDelete,
        LogEvent::GuildScheduledEventCreate,
        LogEvent::GuildScheduledEventUpdate,
        LogEvent::GuildScheduledEventDelete,
        LogEvent::ThreadCreate,
        LogEvent::ThreadUpdate,
        LogEvent::ThreadDelete,
        LogEvent::AutoModRuleCreate,
        LogEvent::AutoModRuleUpdate,
        LogEvent::AutoModRuleDelete,
        LogEvent::AppRemove,
        LogEvent::AppCommandPermissionUpdate,
        LogEvent::MemberTimeoutEnd,
        LogEvent::PollCreate,
        LogEvent::PollEnd,
        LogEvent::PollVoteAdd,
        LogEvent::PollVoteRemove,
        LogEvent::MessagePublish,
        LogEvent::GuildScheduledEventSubscribe,
        LogEvent::GuildScheduledEventUnsubscribe,
        LogEvent::StageStart,
        LogEvent::StageUpdate,
        LogEvent::StageEnd,
        LogEvent::BoostLevelChange,
        LogEvent::ThreadAutoArchive,
        LogEvent::UserAvatarUpdate,
        LogEvent::UserNameUpdate,
    ];

    /// Events that are off until a channel is picked for the event itself: no kind route, no
    /// general channel, and Auto Setup makes none. Votes and event sign-ups would flood a log.
    pub fn quiet(self) -> bool {
        matches!(
            self,
            Self::PollVoteAdd
                | Self::PollVoteRemove
                | Self::GuildScheduledEventSubscribe
                | Self::GuildScheduledEventUnsubscribe
        )
    }

    /// The event of an audit log entry. Message deletes come from the gateway instead.
    pub fn from_action(action: A) -> Option<Self> {
        Some(match action {
            A::GuildUpdate => Self::GuildUpdate,
            A::ChannelCreate => Self::ChannelCreate,
            A::ChannelUpdate => Self::ChannelUpdate,
            A::ChannelDelete => Self::ChannelDelete,
            A::ChannelOverwriteCreate => Self::ChannelOverwriteCreate,
            A::ChannelOverwriteUpdate => Self::ChannelOverwriteUpdate,
            A::ChannelOverwriteDelete => Self::ChannelOverwriteDelete,
            A::MessagePin => Self::MessagePin,
            A::MessageUnpin => Self::MessageUnpin,
            A::MemberKick => Self::MemberKick,
            A::MemberPrune => Self::MemberPrune,
            A::MemberBanAdd => Self::MemberBanAdd,
            A::MemberBanRemove => Self::MemberBanRemove,
            A::MemberUpdate => Self::MemberUpdate,
            A::MemberRoleUpdate => Self::MemberRoleUpdate,
            A::MemberMove => Self::MemberMove,
            A::MemberDisconnect => Self::MemberDisconnect,
            A::BotAdd => Self::BotAdd,
            A::RoleCreate => Self::RoleCreate,
            A::RoleUpdate => Self::RoleUpdate,
            A::RoleDelete => Self::RoleDelete,
            A::InviteCreate => Self::InviteCreate,
            A::InviteUpdate => Self::InviteUpdate,
            A::InviteDelete => Self::InviteDelete,
            A::WebhookCreate => Self::WebhookCreate,
            A::WebhookUpdate => Self::WebhookUpdate,
            A::WebhookDelete => Self::WebhookDelete,
            A::EmojiCreate => Self::EmojiCreate,
            A::EmojiUpdate => Self::EmojiUpdate,
            A::EmojiDelete => Self::EmojiDelete,
            A::StickerCreate => Self::StickerCreate,
            A::StickerUpdate => Self::StickerUpdate,
            A::StickerDelete => Self::StickerDelete,
            A::GuildScheduledEventCreate => Self::GuildScheduledEventCreate,
            A::GuildScheduledEventUpdate => Self::GuildScheduledEventUpdate,
            A::GuildScheduledEventDelete => Self::GuildScheduledEventDelete,
            A::ThreadCreate => Self::ThreadCreate,
            A::ThreadUpdate => Self::ThreadUpdate,
            A::ThreadDelete => Self::ThreadDelete,
            A::AutoModerationRuleCreate => Self::AutoModRuleCreate,
            A::AutoModerationRuleUpdate => Self::AutoModRuleUpdate,
            A::AutoModerationRuleDelete => Self::AutoModRuleDelete,
            A::IntegrationDelete => Self::AppRemove,
            A::ApplicationCommandPermissionUpdate => Self::AppCommandPermissionUpdate,
            A::StageInstanceCreate => Self::StageStart,
            A::StageInstanceUpdate => Self::StageUpdate,
            A::StageInstanceDelete => Self::StageEnd,
            _ => return None,
        })
    }
}

/// A custom logging type: every event in `events` goes to `channel`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomLog {
    #[serde(deserialize_with = "crate::lenient::list")]
    pub events: Vec<LogEvent>,
    pub channel: Id<ChannelMarker>,
}

/// Per-guild logging settings, stored as part of the guild config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// Receives every log type without its own route (`#brim-logs`).
    pub fallback: Option<Id<ChannelMarker>>,
    #[serde(deserialize_with = "crate::lenient::map")]
    pub routes: HashMap<LogKind, Id<ChannelMarker>>,
    /// One channel per Discord event type.
    #[serde(deserialize_with = "crate::lenient::map")]
    pub events: HashMap<LogEvent, Id<ChannelMarker>>,
    /// Custom logging types. The first one that lists an event wins.
    pub custom: Vec<CustomLog>,
    /// Message logs from these channels are dropped.
    pub ignored_channels: HashSet<Id<ChannelMarker>>,
    /// Logs whose author or executor holds one of these roles are dropped ([`LogKind::ignorable`]).
    pub ignored_roles: HashSet<Id<RoleMarker>>,
    /// Logs whose author or executor is one of these users are dropped ([`LogKind::ignorable`]).
    pub ignored_users: HashSet<Id<UserMarker>>,
    /// Edit and delete logs of messages with embeds are dropped.
    pub ignore_embeds: bool,
}

impl LogKind {
    /// Whether the ignore lists may hide this kind. Security, moderation and member-facing
    /// logs are never hidden: a role ignore must not hide a nuker who holds that role.
    pub fn ignorable(self) -> bool {
        use LogKind::*;
        match self {
            Message | Member | Voice | Thread | Channel | Role | Invite | Emoji | Sticker
            | Webhook | ScheduledEvent | Server | Poll | User => true,
            Report | AutoMod | AutoModRule | AntiNuke | Verification | JoinGate | JoinRaid
            | Panic | ModCommand | Appeal => false,
        }
    }
}

impl LogConfig {
    /// Whether the user ignore list hides a log of `kind` by `actor`.
    pub fn ignores_user(&self, kind: LogKind, actor: Option<Id<UserMarker>>) -> bool {
        kind.ignorable() && actor.is_some_and(|a| self.ignored_users.contains(&a))
    }

    /// The channel a log goes to, if any. A custom type beats an event route, which beats a kind route.
    pub fn target(&self, kind: LogKind, event: Option<LogEvent>) -> Option<Id<ChannelMarker>> {
        let quiet = event.is_some_and(LogEvent::quiet);
        let routed = event
            .and_then(|e| {
                self.custom
                    .iter()
                    .find(|c| c.events.contains(&e))
                    .map(|c| c.channel)
                    .or_else(|| self.events.get(&e).copied())
            })
            .or_else(|| self.routes.get(&kind).copied().filter(|_| !quiet));
        // Mod logs never go to the general log channel; voice logs are too noisy for it.
        if quiet || matches!(kind, LogKind::ModCommand | LogKind::Voice) {
            routed
        } else {
            routed.or(self.fallback)
        }
    }

    /// Every channel a log can go to.
    pub fn channels(&self) -> std::collections::BTreeSet<Id<ChannelMarker>> {
        self.fallback
            .into_iter()
            .chain(self.routes.values().copied())
            .chain(self.events.values().copied())
            .chain(self.custom.iter().map(|c| c.channel))
            .collect()
    }

    /// Drops a deleted channel from every route, so its logs fall back to the general channel.
    /// Returns whether anything pointed at it.
    pub fn forget(&mut self, channel: Id<ChannelMarker>) -> bool {
        let before = (self.routes.len(), self.events.len(), self.custom.len());
        self.routes.retain(|_, c| *c != channel);
        self.events.retain(|_, c| *c != channel);
        self.custom.retain(|c| c.channel != channel);
        let was_fallback = self.fallback == Some(channel);
        if was_fallback {
            self.fallback = None;
        }
        was_fallback || before != (self.routes.len(), self.events.len(), self.custom.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_beats_fallback() {
        let fallback = Id::new(1);
        let mut cfg = LogConfig {
            fallback: Some(fallback),
            ..Default::default()
        };
        assert_eq!(cfg.target(LogKind::Role, None), Some(fallback));
        cfg.routes.insert(LogKind::Role, Id::new(2));
        assert_eq!(cfg.target(LogKind::Role, None), Some(Id::new(2)));
        assert_eq!(cfg.target(LogKind::Member, None), Some(fallback));
    }

    #[test]
    fn event_and_custom_beat_kind_route() {
        let mut cfg = LogConfig::default();
        cfg.routes.insert(LogKind::Member, Id::new(2));
        cfg.events.insert(LogEvent::MemberKick, Id::new(3));
        cfg.custom.push(CustomLog {
            events: vec![LogEvent::MemberKick, LogEvent::MemberBanAdd],
            channel: Id::new(4),
        });
        let kick = Some(LogEvent::MemberKick);
        assert_eq!(cfg.target(LogKind::Member, kick), Some(Id::new(4)));
        assert_eq!(
            cfg.target(LogKind::Member, Some(LogEvent::MemberMove)),
            Some(Id::new(2))
        );
        cfg.custom.clear();
        assert_eq!(cfg.target(LogKind::Member, kick), Some(Id::new(3)));
    }

    #[test]
    fn quiet_events_need_a_channel_of_their_own() {
        let mut cfg = LogConfig {
            fallback: Some(Id::new(1)),
            routes: HashMap::from([(LogKind::Poll, Id::new(2))]),
            ..Default::default()
        };
        let vote = Some(LogEvent::PollVoteAdd);
        assert_eq!(cfg.target(LogKind::Poll, vote), None);
        assert_eq!(cfg.target(LogKind::Poll, None), Some(Id::new(2)));
        cfg.events.insert(LogEvent::PollVoteAdd, Id::new(3));
        assert_eq!(cfg.target(LogKind::Poll, vote), Some(Id::new(3)));
        assert_eq!(
            cfg.target(LogKind::Poll, Some(LogEvent::PollVoteRemove)),
            None
        );
        let sub = Some(LogEvent::GuildScheduledEventSubscribe);
        assert_eq!(cfg.target(LogKind::ScheduledEvent, sub), None);
    }

    #[test]
    fn mod_log_never_uses_fallback() {
        let cfg = LogConfig {
            fallback: Some(Id::new(1)),
            ..Default::default()
        };
        assert_eq!(cfg.target(LogKind::ModCommand, None), None);
        assert_eq!(cfg.target(LogKind::Voice, None), None);
    }

    #[test]
    fn forget_drops_deleted_channel_everywhere() {
        let (gone, kept) = (Id::new(9), Id::new(1));
        let mut cfg = LogConfig {
            fallback: Some(kept),
            routes: HashMap::from([(LogKind::Role, gone), (LogKind::Member, kept)]),
            events: HashMap::from([(LogEvent::MessageDelete, gone)]),
            custom: vec![CustomLog {
                events: vec![LogEvent::RoleDelete],
                channel: gone,
            }],
            ..Default::default()
        };
        assert_eq!(cfg.channels().into_iter().collect::<Vec<_>>(), [kept, gone]);
        assert!(cfg.forget(gone));
        assert_eq!(
            cfg.target(LogKind::Role, Some(LogEvent::RoleDelete)),
            Some(kept)
        );
        assert_eq!(
            cfg.target(LogKind::Message, Some(LogEvent::MessageDelete)),
            Some(kept)
        );
        assert_eq!(cfg.routes.len(), 1);
        assert!(!cfg.forget(Id::new(77)));
        assert!(cfg.forget(kept));
        assert_eq!(cfg.fallback, None);
    }

    #[test]
    fn no_route_and_no_fallback_is_none() {
        assert_eq!(LogConfig::default().target(LogKind::Message, None), None);
    }
}
