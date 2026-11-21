//! Default bot messages as Message Builder templates. Each built-in message has a key, a default
//! template shipped here (content + embeds, the builder's format) and the variables it may use. A
//! server may store its own copy (`bot_messages`); [`render`] takes that or the default and fills the
//! `{variables}`. Locked messages (security texts) never take an override.
use crate::embed::{label_head, ALERT, BLUE, DONE, GREEN, ORANGE, RED};
use serde_json::{json, Value};
use std::sync::OnceLock;

pub struct Def {
    pub key: String,
    pub name: String,
    pub group: &'static str,
    /// Viewable, never editable, and [`render`] ignores any stored copy.
    pub locked: bool,
    /// Security texts (plan section 6): only Owner and Extra Owners change them, each change is
    /// audited with the old and new version, a required placeholder only counts where it shows as
    /// plain text, and [`render`] sends the default when the edited version loses a fact.
    pub security: bool,
    /// Variables the template may use. Anything else in a saved copy is refused.
    pub vars: Vec<&'static str>,
    /// Variables whose removal from an edited copy gets a red warning (not a block).
    pub important: Vec<&'static str>,
    /// Placeholders the entry must keep in its first embed (the save refuses a copy without them;
    /// a stored copy without them sends the default). `{text}` wherever the template has it.
    pub required: Vec<&'static str>,
    /// What Brim puts in `{text}`, for the editor's notice ("the details of the log").
    pub what: &'static str,
    pub template: Value,
}

impl Def {
    /// A text whose facts must arrive as plain text (see [`Def::security`]): the security texts and
    /// the two bot-wide DMs that state what happened to the recipient's data or standing.
    pub fn strict(&self) -> bool {
        self.security
            || matches!(self.key.as_str(), "dm.data_request" | "dm.raider_notify")
            || self.details()
    }

    /// A moderation log or answer whose edited version is the whole text and names its facts
    /// (`{case}`, `{reason}` ...): see [`FACTS`]. A copy made before, a frame around `{text}`,
    /// still works because `{text}` then carries every fact.
    pub fn details(&self) -> bool {
        is_facts(&self.key)
    }
}

fn embed(title: &str, color: u32, lines: &[String]) -> Value {
    let mut e = json!({"color": color, "description": lines.join("\n")});
    if !title.is_empty() {
        e["title"] = title.into();
    }
    json!({"content": "", "embeds": [e]})
}

fn def(
    key: &str,
    name: &str,
    group: &'static str,
    locked: bool,
    vars: &[&'static str],
    template: Value,
) -> Def {
    Def {
        key: key.into(),
        name: name.into(),
        group,
        locked,
        security: false,
        vars: vars.to_vec(),
        important: Vec::new(),
        required: Vec::new(),
        what: "",
        template,
    }
}

/// A message whose text the code builds (several parts, many cases): the template is a frame
/// around `{text}`.
fn frame(key: &str, name: &str, group: &'static str, locked: bool, title: &str, color: u32) -> Def {
    def(
        key,
        name,
        group,
        locked,
        &["text"],
        embed(title, color, &["{text}".into()]),
    )
}

fn plain(
    key: &str,
    name: &str,
    group: &'static str,
    vars: &[&'static str],
    title: &str,
    color: u32,
    text: &str,
) -> Def {
    def(
        key,
        name,
        group,
        false,
        vars,
        embed(title, color, &[text.into()]),
    )
}

/// A short reply (one sentence, sent as the message text): one entry per reply, code id
/// `reply.<area>.<name>`. Values are named variables; the sentence itself is the default.
fn say(key: &str, name: &str, group: &'static str, vars: &[&'static str], text: &str) -> Def {
    def(
        key,
        name,
        group,
        false,
        vars,
        json!({"content": text, "embeds": []}),
    )
}

/// Ticket side replies (one entry each): key, name, vars, red (an error) or blue, default text.
const TICKET_REPLIES: &[(&str, &str, &[&str], bool, &str)] = &[
    ("panel_unused", "panel no longer in use", &[], true, "This ticket panel is no longer in use."),
    ("form_changed", "form was changed", &[], true, "This form was changed meanwhile. Please press the button again."),
    ("blacklisted_panel", "blacklisted (panel button)", &[], true, "You are blacklisted from opening tickets in this server."),
    ("paused_panel", "tickets turned off (panel button)", &[], true, "New tickets are turned off in this server right now."),
    ("lockdown_panel", "lockdown (panel button)", &[], true, "This server is in lockdown and does not take new tickets right now. Please try again later."),
    ("quarantined_panel", "quarantined (panel button)", &[], true, "You are quarantined in this server and were given the reason, so you cannot open a ticket."),
    ("answers_filtered", "form answers not allowed", &[], true, "Your answers hold words or links this server does not allow, so no ticket was opened."),
    ("not_taking", "server takes no tickets (panel button)", &[], true, "This server does not take tickets right now."),
    ("panel_cap", "too many open tickets of this kind", &[], true, "You already have as many open tickets of this kind as you may. Find them in the server's ticket channels."),
    ("channel_failed_panel", "ticket channel could not be made (panel button)", &[], true, "Brim could not make your ticket channel. Please contact an admin of the server."),
    ("open_failed_panel", "ticket could not be opened (panel button)", &[], true, "Your ticket could not be opened. Please try again."),
    ("error_generic", "something went wrong opening a ticket", &[], true, "Something went wrong, and no ticket was opened. Please try again later."),
    ("opened", "your ticket is open", &["channel"], false, "Your ticket is open: {channel}"),
    ("closed", "this ticket is closed", &[], true, "This ticket is closed."),
    ("staff_only", "only staff can do this", &[], true, "Only staff can do this."),
    ("no_types", "no ticket types", &[], true, "This server has no ticket types. Add them on the Tickets tab."),
    ("cap_user", "too many tickets in an hour", &[], true, "You opened too many tickets in that server in the last hour. Please try again later."),
    ("cap_server", "server gets too many tickets", &[], true, "That server gets too many new tickets right now. Please try again later."),
    ("being_opened", "ticket is being opened", &[], true, "Your ticket is being opened, please send your message again in a moment."),
    ("staff_blacklisted", "staff reply to a blacklisted member", &[], true, "That user is blacklisted from sending a message here. You need to whitelist them before you can send them a message."),
    ("user_not_found", "ticket member not found", &["prefix"], true, "The user was not found. Use `{prefix}close [reason]` to close this channel."),
    ("dm_closed", "member has DMs closed", &[], true, "The message could not be sent. The user might have disabled Direct Messages."),
    ("already_closing", "ticket is already being closed", &[], true, "This ticket is already being closed."),
    ("closing", "closing the ticket", &[], false, "Closing ticket..."),
    ("log_not_saved", "ticket log could not be saved", &["error"], true, "Could not save the ticket log, so the ticket stays open: {error}."),
    ("no_delete_perm", "no permission to delete the channel", &[], true, "Missing permissions to delete this channel."),
    ("close_unfinished", "closing did not finish", &[], true, "Closing this ticket did not finish, so it is still open. Please close it again."),
    ("not_ticket", "channel is not a ticket", &[], true, "This channel is not a ticket channel."),
    ("dm_server_missing", "server not found (DM)", &[], true, "The server was not found."),
    ("dm_not_member", "member is not in the server (DM)", &[], true, "You are not in that server, and the message is not sent."),
    ("dm_not_taking", "server takes no tickets (DM)", &[], true, "That server does not take tickets right now."),
    ("dm_category_failed", "ticket category could not be made (DM)", &[], true, "A ticket category is not found and Brim could not make one. Please contact an admin on the server."),
    ("dm_blacklisted", "blacklisted (DM)", &[], true, "That server has blacklisted you from sending a message there."),
    ("dm_filtered", "message not allowed (DM)", &[], true, "Your message was not sent: it holds words or links that server does not allow."),
    ("dm_lockdown", "lockdown (DM)", &[], true, "That server is in lockdown and does not take new tickets right now. Please try again later."),
    ("dm_quarantined", "quarantined (DM)", &[], true, "You are quarantined in that server and were given the reason, so you cannot open a ticket there."),
    ("dm_earlier_closed", "earlier ticket was closed (DM)", &[], true, "Your earlier ticket there was closed. Please send your message again to open a new one."),
    ("bot_missing_perms", "bot is missing permissions", &[], true, "The bot is missing permissions. Please contact an admin on the server."),
    ("max_active", "too many active tickets", &[], true, "The server has reached the maximum number of active tickets. Please try again later."),
    ("dm_error", "error opening a ticket (DM)", &["error"], true, "An error occurred. Please contact an admin on the server with the following information: {error}."),
    ("dm_open_failed", "ticket could not be opened (DM)", &[], true, "Your ticket could not be opened. Please send your message again."),
    ("ask_type", "ask the ticket type", &[], false, "What is your ticket about? Pick one so staff can help you faster."),
    // Ticket command results (P6b): staff work in a ticket and the ticket settings commands.
    ("claimed", "you claimed the ticket", &[], false, "You claimed this ticket."),
    ("claimed_self", "you already claimed it", &[], true, "You have already claimed this ticket."),
    ("claimed_other", "someone else claimed it", &["user"], true, "{user} has already claimed this ticket."),
    ("claim_failed", "ticket cannot be claimed now", &[], true, "This ticket cannot be claimed now."),
    ("not_claimed", "ticket is not claimed", &[], true, "This ticket is not claimed."),
    ("claim_not_yours", "claim belongs to someone else", &["user"], true, "{user} claimed this ticket; only they or an admin can unclaim it."),
    ("claim_changed", "claim changed meanwhile", &[], true, "The claim changed meanwhile. Please try again."),
    ("reassign_no_permit", "new handler has no Manage Tickets", &[], true, "That member does not have Manage Tickets."),
    ("reassign_failed", "ticket cannot be reassigned now", &[], true, "This ticket cannot be reassigned now."),
    ("shown_claimed", "notice: ticket claimed", &["user"], false, "{user} claimed this ticket."),
    ("shown_unclaimed", "notice: claim removed", &[], false, "This ticket is no longer claimed."),
    ("shown_reassigned", "notice: ticket reassigned", &["user"], false, "This ticket is now handled by {user}."),
    ("shown_type_changed", "notice: ticket type changed", &["user", "old_type", "type"], false, "{user} changed the ticket type from **{old_type}** to **{type}**."),
    ("shown_type_set", "notice: ticket type set", &["user", "type"], false, "{user} set the ticket type to **{type}**."),
    ("choices", "value is not one of the choices", &["choices"], true, "Use one of: {choices}."),
    ("priority_set", "priority set", &["priority"], false, "Priority set to {priority}."),
    ("not_open", "ticket is not open", &[], true, "This ticket is not open."),
    ("type_gone", "ticket type no longer offered", &[], true, "That ticket type is no longer offered."),
    ("type_staff_only", "only staff change the type now", &[], true, "Only staff can change the ticket type now."),
    ("type_set", "ticket type set", &["type"], false, "Ticket type set to {type}."),
    ("note_empty", "note command without text", &[], true, "Write the note after the command."),
    ("note_form_empty", "note form left empty", &[], true, "Write the note in the form."),
    ("note_no_log", "no ticket log for the note", &[], true, "Notes in a panel ticket go to the ticket log, and none is set."),
    ("note_no_log_deleted", "no ticket log, command deleted", &[], true, "Notes in a panel ticket go to the ticket log, and none is set. Your message was deleted so the member cannot read it."),
    ("note_no_log_kept", "no ticket log, command not deleted", &[], true, "Notes in a panel ticket go to the ticket log, and none is set. Your message could not be deleted, so the member can read it. Delete it by hand."),
    ("note_not_deleted", "note saved, command not deleted", &[], true, "The note is saved, but your message could not be deleted, so the member can read it. Delete it by hand."),
    ("note_saved", "note saved", &[], false, "The note is saved."),
    ("users_bad", "role or channel given for a user", &["typed"], true, "{typed} is a role or channel. Give user mentions or ids, not roles or channels."),
    ("users_not_found", "users not found", &[], true, "The user(s) are not found. Please try again."),
    ("users_missing", "no users given outside a ticket", &[], true, "You must provide users or run this command in a ticket."),
    ("blacklisted", "users blacklisted", &[], false, "The user(s) are blacklisted successfully."),
    ("unblacklisted", "users taken off the blacklist", &[], false, "The user(s) are whitelisted successfully."),
    ("blacklist_cleared", "blacklist cleared", &[], false, "The blacklist is cleared successfully."),
    ("blacklist_empty", "nobody is blacklisted", &[], false, "No one is blacklisted."),
    ("snippet_not_found", "snippet not found (reply)", &[], true, "The snippet was not found."),
    ("snippet_missing", "snippet not found (settings)", &[], true, "A snippet with that name was not found."),
    ("snippet_name_long", "snippet name too long", &[], true, "The snippet name cannot exceed 100 characters."),
    ("snippet_content_long", "snippet content too long", &[], true, "The snippet content cannot exceed 1000 characters."),
    ("snippet_exists", "snippet already exists", &[], true, "A snippet with that name already exists."),
    ("snippet_added", "snippet added", &[], false, "The snippet was added successfully."),
    ("snippet_removed", "snippet removed", &[], false, "The snippet was removed successfully."),
    ("snippets_cleared", "all snippets removed", &[], false, "All snippets were removed successfully."),
    ("snippets_empty", "no snippets yet", &[], false, "No snippet has been added yet."),
    ("roles_not_found", "ping roles not found", &[], true, "The role(s) are not found. Please try again."),
    ("roles_too_many", "too many ping roles", &[], true, "There can at most be 10 roles. Try using the command again but specify fewer roles."),
    ("roles_updated", "ping roles updated", &[], false, "The role(s) are updated successfully."),
    ("log_channel_not_found", "log channel not found", &[], true, "The channel is not found. Please try again."),
    ("logging_off", "ticket logging turned off", &[], false, "Ticket logging is disabled. Closed tickets are now deleted with no copy of the conversation. You may delete the log channel."),
    ("logging_on", "ticket logging turned on", &[], false, "Ticket logging is enabled."),
    ("command_only_on", "command only mode on", &[], false, "Command only mode is enabled."),
    ("command_only_off", "command only mode off", &[], false, "Command only mode is disabled."),
    ("anonymous_on", "anonymous messaging on", &[], false, "Anonymous messaging is enabled."),
    ("anonymous_off", "anonymous messaging off", &[], false, "Anonymous messaging is disabled."),
    ("full_log_on", "advanced logging on", &[], false, "Advanced logging is enabled."),
    ("full_log_off", "advanced logging off", &[], false, "Advanced logging is disabled."),
    ("greeting_set", "greeting message set", &[], false, "The greeting message is set successfully."),
    ("closing_set", "closing message set", &[], false, "The closing message is set successfully."),
    ("creation_on", "ticket creation turned on", &[], false, "Ticket creation is enabled."),
    ("creation_off", "ticket creation turned off", &[], false, "Ticket creation is disabled."),
    ("creation_already_on", "ticket creation already on", &[], false, "Ticket creation is already enabled."),
    ("creation_already_off", "ticket creation already off", &[], false, "Ticket creation is already disabled."),
    ("category_name_long", "category name too long", &[], true, "The category name cannot be longer than 100 characters"),
    ("category_exists", "ticket category already exists", &[], true, "A ticket category already exists. Please delete that category and try again."),
    ("already_set_up", "tickets already set up", &[], true, "The bot has already been set up."),
    ("category_made", "category made", &[], false, "Successfully created the category."),
    ("set_up", "tickets set up", &["prefix"], false, "Everything has been set up! Next up, you can give your staff access to tickets with a permit that has Manage Tickets (the server owner, Extra Owners and Trusted Admins always have full access). You can also test things out by direct messaging me. Check out more information and configurations with `{prefix}help`."),
    // Permit refusals, usage lines and caps of the ticket commands.
    ("no_reply_access", "no access to reply to tickets", &[], true, "You do not have access to reply to tickets, so your message was not sent."),
    ("admin_only", "command is for Owner, EO and TA", &["command"], true, "Only the server owner, Extra Owners and Trusted Admins can use `{command}`."),
    ("staff_only_command", "command needs Manage Tickets", &["command"], true, "You need a permit with Manage Tickets to use `{command}`."),
    ("reassign_admin_only", "reassign is for Owner, EO and TA", &[], true, "Only the server owner, Extra Owners and Trusted Admins can reassign tickets."),
    ("not_set_up", "tickets not set up yet", &["prefix"], true, "Your server has not been set up yet. Use `{prefix}ticketsetup` first."),
    ("panel_write_here", "reply command in a panel ticket", &[], true, "The member reads this panel ticket, so just write your answer here."),
    ("usage", "usage line", &["text"], true, "Usage: {text}"),
    ("snippets_full", "too many snippets", &["count", "cap"], true, "This server has {count} snippets, the most allowed is {cap}. Remove one first."),
    ("blacklist_full", "blacklist is full", &["count", "cap"], true, "This server has {count} blacklisted users, the most allowed is {cap}. Remove one first."),
];

const OUTSIDE: &str = "Outside a server (bot owner)";

/// The DM commands of a member with no server picked yet (`tickets/dm.rs`): bot-wide entries
/// (see [`BOT_WIDE`]), `reply.dm.*`. `{prefix}` is the DM command prefix.
const DM_REPLIES: &[(&str, &str, &[&str], bool, &str)] = &[
    ("usage_new", "usage line of the new command", &["prefix"], true, "Usage: `{prefix}new <message>`"),
    ("usage_send", "usage line of the send command", &["prefix"], true, "Usage: `{prefix}send <server ID> <message>`"),
    ("confirmation_off", "confirmation messages turned off", &["prefix"], false, "Confirmation messages are disabled. When you share more than one ticket server with Brim, you still pick the server for each message, or use `{prefix}send <server ID> <message>`."),
    ("confirmation_on", "confirmation messages turned on", &[], false, "Confirmation messages are enabled."),
    ("no_server", "no server to send to", &[], true, "Oops, something strange happened. No server found."),
    ("expired", "server prompt expired", &[], true, "This request has expired. Send your message again."),
    ("cancelled", "request cancelled", &[], true, "Request cancelled successfully."),
    ("wait", "wait before pressing again", &[], true, "Please wait a few seconds, then press again."),
    ("message_deleted", "your message was deleted", &[], true, "Your message was deleted, so nothing was sent."),
    ("type_closed", "ticket closed before the type pick", &[], true, "This ticket is closed."),
];

/// The server menu of a DM: its embed, and the texts inside the menu itself (message text only).
fn outside_replies(out: &mut Vec<Def>) {
    use crate::emoji::TICKET;
    for (k, name, vars, red, text) in DM_REPLIES {
        out.push(def(
            &format!("reply.dm.{k}"),
            &format!("DM: {name}"),
            OUTSIDE,
            false,
            vars,
            embed("", if *red { RED } else { BLUE }, &[(*text).into()]),
        ));
    }
    out.push(def(
        "reply.dm.menu",
        "DM: pick the server",
        OUTSIDE,
        false,
        &[],
        embed(
            &format!("{TICKET} Select Server"),
            BLUE,
            &["Please select the server you want to send this message to.".into()],
        ),
    ));
    for (k, name, text) in DM_MENU_TEXTS {
        out.push(say(
            &format!("reply.dm.{k}"),
            &format!("DM: {name}"),
            OUTSIDE,
            &[],
            text,
        ));
    }
}

/// Texts inside the server menu: they are message text only (an embed in them is not used).
const DM_MENU_TEXTS: &[(&str, &str, &str)] = &[
    (
        "option_existing",
        "menu line of a server with an open ticket",
        "Existing ticket.",
    ),
    (
        "option_new",
        "menu line of a server with no open ticket",
        "Create a new ticket.",
    ),
    ("placeholder", "menu placeholder", "Select a server"),
];

/// The look (plan section 9): a short title, a side bar by kind, a status emoji and one short
/// sentence first, then label lines with values in code style.
fn build() -> Vec<Def> {
    let mut out = Vec::new();
    let reason = format!("{} {{reason}}", label_head("Reason"));
    let moderator = format!("{} {{moderator_code}}", label_head("Moderator"));
    let duration = format!("{} {{duration}}", label_head("Duration"));
    let vars = &[
        "server",
        "reason",
        "moderator",
        "moderator_code",
        "duration",
        "staff_line",
        "appeal",
    ];
    for (kind, name, title, color, first) in [
        (
            "warn",
            "Warn",
            "You have been warned in {server}!",
            ORANGE,
            "⚠️ You got a warning in `{server}`.",
        ),
        (
            "timeout",
            "Timeout",
            "You have been timed out in {server}!",
            RED,
            "⏳ You cannot talk in `{server}` for a while.",
        ),
        (
            "quarantine",
            "Quarantine",
            "You have been quarantined in {server}!",
            RED,
            "🔒 You were put in quarantine in `{server}`.",
        ),
        (
            "kick",
            "Kick",
            "You have been kicked in {server}!",
            RED,
            "👢 You were removed from `{server}`.",
        ),
        (
            "softban",
            "Softban",
            "You have been softbanned in {server}!",
            RED,
            "🧹 You were removed from `{server}` and your recent messages were deleted.",
        ),
        (
            "tempban",
            "Temporary ban",
            "You have been temporarily banned in {server}!",
            RED,
            "⛔ You are banned from `{server}` for a set time.",
        ),
        (
            "unquarantine",
            "Unquarantine",
            "You have been unquarantined in {server}!",
            GREEN,
            "✅ Your quarantine in `{server}` was lifted.",
        ),
        (
            "untimeout",
            "Timeout removed",
            "Your timeout was removed in {server}!",
            GREEN,
            "✅ You can talk in `{server}` again.",
        ),
        (
            "unban",
            "Unban",
            "You have been unbanned in {server}!",
            GREEN,
            "✅ You can join `{server}` again.",
        ),
        (
            "ban",
            "Ban",
            "You have been banned in {server}!",
            RED,
            "⛔ You can no longer join `{server}`.",
        ),
    ] {
        let mut lines = vec![
            first.to_string(),
            String::new(),
            reason.clone(),
            moderator.clone(),
        ];
        if matches!(kind, "timeout" | "tempban") {
            lines.push(duration.clone());
        }
        if kind == "quarantine" {
            lines.push("{staff_line}".into());
        }
        if matches!(kind, "ban" | "timeout" | "tempban") {
            lines.push("{appeal}".into());
        }
        let mut d = def(
            &format!("dm.punish.{kind}"),
            name,
            "Punishment DMs",
            false,
            vars,
            embed(title, color, &lines),
        );
        d.important = vec!["reason"];
        out.push(d);
    }
    let g = "Member DMs";
    out.push(plain(
        "dm.quarantine_notice",
        "Quarantine notice (no case)",
        g,
        &["server"],
        "You have been quarantined",
        RED,
        "🔒 You have been quarantined in `{server}`. You can DM Brim to reach this server's staff.",
    ));
    out.push(plain("dm.invite_link", "Your invite link", g, &["code", "expiry", "uses"], "Your invite link", BLUE, "🔗 Your invite link: https://discord.gg/{code}\nIt {expiry}, {uses}. Joins through it count for you."));
    out.push(plain(
        "dm.automod_timeout",
        "Automod timeout",
        g,
        &["why", "note"],
        "You were timed out",
        RED,
        "⏳ You were timed out by automod{why}{note}",
    ));
    out.push(plain("dm.verify_reminder", "Verification reminder", g, &["server", "time"], "Verification needed", ORANGE, "⚠️ You have not verified in `{server}`, so you cannot see its channels. Verify in its verification channel within {time}, or you will be removed from the server."));
    for (key, name, color) in [
        ("dm.appeal_approved", "Appeal approved", GREEN),
        ("dm.appeal_denied", "Appeal denied", RED),
    ] {
        let text = "📨 Your {kind} appeal (#{id}) was {verdict}.\n💬 Reply from the staff: {reply}";
        out.push(plain(
            key,
            name,
            g,
            &["kind", "id", "verdict", "reply"],
            "Appeal answer",
            color,
            text,
        ));
    }
    out.push(frame(
        "dm.join_gate",
        "Join Gate punishment",
        g,
        false,
        "Join Gate",
        RED,
    ));
    out.push(frame(
        "dm.raider_notify",
        "Raider List notice",
        OUTSIDE,
        false,
        "Raider List",
        BLUE,
    ));
    out.push(frame(
        "dm.data_request",
        "Your data request",
        OUTSIDE,
        false,
        "Your data",
        BLUE,
    ));
    outside_replies(&mut out);
    tickets_and_verify(&mut out);
    logs(&mut out);
    commands(&mut out);
    let o = "Owner DMs";
    out.push(frame(
        "dm.owner_alert",
        "Owner alert",
        o,
        false,
        "Owner alert",
        ORANGE,
    ));
    out.push(frame(
        "dm.owner_handover",
        "Ownership change summary",
        o,
        false,
        "Ownership change",
        BLUE,
    ));
    let s = SECURITY;
    out.push(frame(
        "dm.security",
        "Anti-nuke and panic alerts to the owner",
        s,
        false,
        "Security alert",
        RED,
    ));
    // The owner approves an MCP change only by what this DM shows.
    out.push(frame(
        "dm.mcp_decision",
        "Bot owner decision notice",
        s,
        false,
        "MCP change to approve",
        ORANGE,
    ));
    out.push(frame(
        "dm.invite_back",
        "Invite back after an unlawful removal",
        s,
        false,
        "You were set aside",
        ORANGE,
    ));
    // Locked (owner decision 2026-11-21): no rescue key text is editable.
    out.push(def("dm.rescue_expired", "Rescue key expired", s, true, &["name", "page"], embed("Rescue key expired", ORANGE, &["⚠️ The rescue key of **{name}** stopped working: it was made before the server changed owner 7 days ago. Make a new one at {page}. It needs your dashboard 2FA.".into()])));
    out.push(def("dm.dashboard_2fa", "Dashboard 2FA setup code", s, false, &["code"], embed("Your setup code", BLUE, &["🔐 Your Brim authenticator setup code is **{code}**. If you didn't start this, don't share it: someone may have your dashboard session.".into()])));
    feeds(&mut out);
    mod_entries(&mut out);
    d3_entries(&mut out);
    d5_entries(&mut out);
    lock_answers(&mut out);
    d7_entries(&mut out);
    d4_entries(&mut out);
    audit_entries(&mut out);
    event_entries(&mut out);
    answer_entries(&mut out);
    security_answers(&mut out);
    settings_errors(&mut out);
    for d in &mut out {
        if shown(&d.template).iter().any(|v| v == "text") {
            d.required = vec!["text"];
            d.important.retain(|v| *v != "text");
            d.what = what(&d.key);
        }
        if d.group == SECURITY {
            d.security = true;
            match d.key.as_str() {
                // The title says what kind of event it is: an edit cannot make a panic log look
                // like a normal one.
                k if k.starts_with("log.") => d.required.insert(0, "title"),
                "dm.rescue_expired" => {
                    d.required = vec!["name", "page"];
                    d.what = "the server's name and the page to make a new key";
                }
                "dm.dashboard_2fa" => {
                    d.required = vec!["code"];
                    d.what = "the setup code";
                }
                "reply.whitelist.not_role" => {
                    d.required = vec!["id"];
                    d.what = "the ID that was typed";
                }
                _ => {}
            }
        }
    }
    for d in out.iter_mut().filter(|d| HIDDEN.contains(&d.key.as_str())) {
        d.locked = true;
    }
    out
}

/// Texts that warn the real owner or let them get their access back: nobody may change them, not
/// even the Owner, so a hijacked account cannot take facts out of them (owner decision,
/// 2026-11-21). They are locked and left out of the Bot messages page.
pub const HIDDEN: &[&str] = &[
    "dm.rescue_expired",
    "dm.dashboard_2fa",
    "dm.owner_handover",
    "dm.invite_back",
    "dm.mcp_decision",
];

/// The moderation logs and answers whose edited version names their facts (plan D1): the key is
/// the whole entry, and an edit must keep every required placeholder as plain text.
pub const FACTS: &[&str] = &[
    "log.mod_command.case",
    "log.mod_command.case_updated",
    "log.mod_command.case_proof_added",
    "log.mod_command.case_proof_removed",
    "log.mod_command.case_deleted",
    "log.mod_command.old_cases_removed",
    "log.mod_command.note_added",
    "log.mod_command.note_modified",
    "log.mod_command.note_removed",
    "log.mod_command.notes_swept",
    "reply.mod.result",
    "reply.mod.total",
    "reply.mod.case",
    "log.auto_mod.filter_delete",
    "log.auto_mod.filter_warn",
    "log.auto_mod.filter_timeout",
    "log.auto_mod.filter_kick",
    "log.auto_mod.filter_ban",
    "log.auto_mod.heat_timeout",
    "log.auto_mod.heat_strike",
    "log.auto_mod.discord_automod_exemptions_cut",
    "log.auto_mod.discord_automod_sync_failed",
    "log.auto_mod.warn_action",
    "log.auto_mod.automod_action_failed",
    "log.auto_mod.discord_automod_rule_approved",
    "log.auto_mod.discord_automod_rule_not_approved",
    "log.auto_mod.rule_not_read",
    "log.auto_mod.discord_automod_rule_changed_without_a_permit",
    "log.auto_mod.discord_automod_report",
    "log.member.invite_used",
    "log.join_gate.join_gate",
    "log.verification.verification_channel_not_saved",
    "log.verification.possible_alt_account",
    "log.verification.verification_blocked",
    "log.verification.passed",
    "log.verification.not_verified",
    "log.verification.role_taken",
    "log.verification.bot_role_taken",
    "log.verification.offline_role_taken",
    "log.verification.channel_permission_taken",
    "log.verification.catch_up_cap",
    "log.verification.take_back_failed",
    "log.mod_command.server_locked",
    "log.mod_command.server_unlocked",
    "log.server.unlock_incomplete",
    "log.server.startup_check_failed",
    "log.server.scheduled_action_failed",
    "log.server.scheduled_action_dropped",
    "log.server.backup_created",
    "log.server.backup_loaded",
    "log.server.backup_not_loaded",
    "log.auto_mod.auto_slowmode_on",
    "log.auto_mod.auto_slowmode_off",
    "log.auto_mod.auto_slowmode_failed",
    "log.auto_mod.auto_slowmode_off_failed",
    "reply.lock.locking",
    "reply.lock.unlocking",
    "reply.lock.locked",
    "reply.lock.unlocked",
    "reply.lock.channel_locked",
    "reply.lock.channel_unlocked",
    "reply.lock.lock_result",
    "reply.lock.unlock_result",
    "reply.lock.none_active",
    "reply.lock.active",
    "log.ticket.new_ticket",
    "log.ticket.ticket_closed",
    "log.ticket.claimed",
    "log.ticket.unclaimed",
    "log.ticket.reassigned",
    "log.ticket.type_changed",
    "log.ticket.type_set",
    "log.report.message_reported",
    "log.report.discord_automod_report",
    "log.appeal.appeal_sent",
    "log.appeal.appeal_not_accepted",
    "log.auto_mod.catch_channel_test_mode",
    "log.auto_mod.catch_channel_action_failed",
];

/// The server event logs Brim builds from Discord's audit log (plan D2): (log group, titles). Each
/// is an entry of facts like [`FACTS`], all with one template ([`audit_entries`]).
pub const AUDIT_LOGS: &[(&str, &[&str])] = &[
    ("message", &["Message Pin", "Message Unpin"]),
    (
        "member",
        &[
            "Member Kick",
            "Member Prune",
            "Member Ban Add",
            "Member Ban Remove",
            "Member Update",
            "Member Role Update",
            "Member Move",
            "Member Disconnect",
            "Bot Add",
            "App Remove",
        ],
    ),
    ("role", &["Role Create", "Role Update", "Role Delete"]),
    (
        "channel",
        &[
            "Channel Create",
            "Channel Update",
            "Channel Delete",
            "Channel Overwrite Create",
            "Channel Overwrite Update",
            "Channel Overwrite Delete",
            "Stage Start",
            "Stage Update",
            "Stage End",
        ],
    ),
    ("server", &["Guild Update", "App Command Permission Update"]),
    (
        "invite",
        &["Invite Create", "Invite Update", "Invite Delete"],
    ),
    (
        "webhook",
        &["Webhook Create", "Webhook Update", "Webhook Delete"],
    ),
    ("emoji", &["Emoji Create", "Emoji Update", "Emoji Delete"]),
    (
        "sticker",
        &["Sticker Create", "Sticker Update", "Sticker Delete"],
    ),
    (
        "scheduled_event",
        &[
            "Guild Scheduled Event Create",
            "Guild Scheduled Event Update",
            "Guild Scheduled Event Delete",
        ],
    ),
    (
        "thread",
        &["Thread Create", "Thread Update", "Thread Delete"],
    ),
];

/// The other server event logs (plan D2 part 2): (log group, title, lines). A line is
/// `Label|value` (a labelled line, `value` with placeholders) or a line as written. Required are
/// the placeholders a line shows outside a link target, except `{when}`.
pub const EVENT_LOGS: &[(&str, &str, &[&str])] = &[
    (
        "message",
        "Message Edited",
        &[
            "Author|{member}",
            "Channel|{channel}",
            "{when}",
            "{changes}",
        ],
    ),
    (
        "message",
        "Message Deleted",
        &[
            "Responsible|{by}",
            "Author|{member}",
            "Channel|{channel}",
            "Sent|{sent}",
            "{when}",
            "Content|",
            "{content}",
            "Attachments|{attachments}",
        ],
    ),
    (
        "message",
        "Poll Deleted",
        &[
            "Responsible|{by}",
            "Author|{member}",
            "Channel|{channel}",
            "Sent|{sent}",
            "{when}",
            "Question|{question}",
        ],
    ),
    (
        "message",
        "Message Delete Bulk",
        &[
            "Channel|{channel}",
            "{when}",
            "Count|{count}",
            "Content|{saved}",
        ],
    ),
    (
        "message",
        "Message Published",
        &["Author|{member}", "Channel|{channel}", "{when}"],
    ),
    (
        "member",
        "Member Join",
        &["Target|{member}", "{when}", "Creation|{created}"],
    ),
    (
        "member",
        "Member Leave",
        &["Target|{member}", "{when}", "Creation|{created}"],
    ),
    (
        "member",
        "Member Renamed",
        &[
            "Member|{member}",
            "Name|`{before}` → `{after}`",
            "Reason|{reason}",
            "{when}",
        ],
    ),
    (
        "member",
        "Timeout Ended",
        &[
            "Member|{member}",
            "Timed Out|{started}",
            "Ended|{ended}",
            "Case|`{case}`",
            "Logged Late|{late}",
            "{when}",
        ],
    ),
    (
        "server",
        "Boost Level Changed",
        &["Level|{before} → {after}", "Boosts|{count}", "{when}"],
    ),
    (
        "scheduled_event",
        "Event Subscribed",
        &["Target|{member}", "Event|{event}", "{when}"],
    ),
    (
        "scheduled_event",
        "Event Unsubscribed",
        &["Target|{member}", "Event|{event}", "{when}"],
    ),
    (
        "thread",
        "Thread Archived Automatically",
        &[
            "Thread|{channel}",
            "Name|{thread_name}",
            "Inactive For|{inactive}",
            "{when}",
        ],
    ),
    (
        "voice",
        "Voice Join",
        &["Channel|{channel}", "Target|{member}", "{when}"],
    ),
    (
        "voice",
        "Voice Leave",
        &["Channel|{channel}", "Target|{member}", "{when}"],
    ),
    (
        "voice",
        "Voice Move",
        &["From|{from}", "To|{to}", "Target|{member}", "{when}"],
    ),
    (
        "poll",
        "Poll Created",
        &[
            "Author|{member}",
            "Channel|{channel}",
            "Question|{question}",
            "Multiple Answers|{multiple}",
            "Ends|{ends}",
            "{when}",
            "**Answers**",
            "{answers}",
        ],
    ),
    (
        "poll",
        "Poll Ended",
        &[
            "Author|{member}",
            "Channel|{channel}",
            "Question|{question}",
            "{when}",
            "**Results**",
            "{results}",
        ],
    ),
    (
        "poll",
        "Poll Vote Added",
        &[
            "Target|{member}",
            "Channel|{channel}",
            "Poll|{poll}",
            "Answer|{answer}",
            "{when}",
        ],
    ),
    (
        "poll",
        "Poll Vote Removed",
        &[
            "Target|{member}",
            "Channel|{channel}",
            "Poll|{poll}",
            "Answer|{answer}",
            "{when}",
        ],
    ),
    (
        "user",
        "User Avatar Changed",
        &[
            "Member|{member}",
            "Old Avatar|[small picture]({before})",
            "New Avatar|[large picture]({after})",
            "{when}",
        ],
    ),
    (
        "user",
        "User Name Changed",
        &["Member|{member}", "{changes}", "{when}"],
    ),
];

/// Whether `key` is an entry of facts: [`FACTS`], an [`AUDIT_LOGS`] or an [`EVENT_LOGS`] type.
fn is_facts(key: &str) -> bool {
    FACTS.contains(&key)
        || ANSWERS.contains(&key)
        || WHITELIST_ANSWERS.contains(&key)
        || CHANGED.contains(&key)
        || settings_error(key)
        || panel(key).is_some()
        || SECURITY_LOGS.iter().any(|l| l.0 == key)
        || key
            .strip_prefix("log.")
            .and_then(|k| k.split_once('.'))
            .is_some_and(|(g, t)| {
                AUDIT_LOGS
                    .iter()
                    .any(|(group, titles)| *group == g && titles.iter().any(|x| slug(x) == t))
                    || EVENT_LOGS
                        .iter()
                        .any(|(group, title, _)| *group == g && slug(title) == t)
            })
}

/// The variables of the [`FACTS`] entries: (name, editor name, example).
const FACT_VARS: &[(&str, &str, &str)] = &[
    ("row", "Number of the setting, as its panel shows it", "[7B]"),
    ("setting", "Name of the setting", "Regular Strike Duration"),
    ("value", "New value of the setting", "1 hour"),
    ("number", "Number of the setting, as typed", "7"),
    ("typed", "What was typed", "abc"),
    ("min", "Lowest allowed value", "1"),
    ("max", "Highest allowed value", "100"),
    ("options", "The values the setting takes", "kick, ban, quarantine"),
    ("dashboard", "Link to the dashboard", "[Dashboard](https://example.com/#/1)"),
    ("case", "Case number", "12"),
    ("kind", "Kind of case or command", "ban"),
    (
        "member",
        "Member (mention with ID)",
        "@alex *`123456789012345678`*",
    ),
    (
        "moderator",
        "Moderator (mention with ID)",
        "@sam *`123456789012345679`*",
    ),
    (
        "duration",
        "Duration (code style, empty without one)",
        "`1 hour`",
    ),
    ("reason", "Reason", "Spamming in #general"),
    (
        "written_by",
        "Who wrote it, when an AI did (empty otherwise)",
        "Written by an AI assistant",
    ),
    (
        "state",
        "New state of the case (empty if unchanged)",
        "Closed",
    ),
    (
        "proof",
        "The proof that was added",
        "https://example.com/proof.png",
    ),
    ("removed", "Kind of proof that was removed", "image"),
    (
        "changes",
        "What changed, as Brim lists it (empty without changes)",
        "**Reason**\n```diff\n- old reason\n+ new reason\n```",
    ),
    ("question", "The poll's question", "Pizza or pasta?"),
    (
        "multiple",
        "Whether more than one answer may be picked",
        "no",
    ),
    (
        "ends",
        "When the poll ends (empty without an end)",
        "<t:1700000000:F>",
    ),
    (
        "answers",
        "The poll's answers, one per line",
        "1. Pizza\n2. Pasta",
    ),
    (
        "results",
        "Each answer with its votes",
        "1. Pizza: **4**\n2. Pasta: **2**",
    ),
    (
        "poll",
        "The poll's question (its ID when unknown)",
        "Pizza or pasta?",
    ),
    ("answer", "The answer voted for", "Pizza"),
    ("sent", "When the message was sent", "<t:1700000000:F>"),
    (
        "attachments",
        "File names of the deleted message (empty without files)",
        "photo.png",
    ),
    (
        "saved",
        "How many deleted messages are in the file below",
        "`12/12` saved in `bulk-abc.txt` below",
    ),
    (
        "created",
        "When the account was made",
        "<t:1600000000:F> (<t:1600000000:R>)",
    ),
    ("from", "Voice channel left", "#lounge"),
    ("to", "Voice channel joined", "#gaming"),
    ("event", "The scheduled event", "Movie night"),
    ("thread_name", "The thread's name", "help-123"),
    ("inactive", "How long the thread had no messages", "`1 day`"),
    ("before", "The old value", "old-name"),
    ("after", "The new value", "new-name"),
    ("started", "When the timeout started", "<t:1700000000:F>"),
    ("ended", "When the timeout ended", "<t:1700003600:F>"),
    (
        "late",
        "Why the log is late (empty when on time)",
        "Brim was not running at the end of the timeout",
    ),
    (
        "by",
        "Who did it, a person or a bot (mention with ID)",
        "@sam *`123456789012345679`*",
    ),
    (
        "target",
        "What was acted on: a member, role, channel or a name (empty without one)",
        "@Helpers *`123456789012345681`*",
    ),
    (
        "author",
        "Member who wrote the note (mention with ID)",
        "@alex *`123456789012345678`*",
    ),
    (
        "deleted_by",
        "Staff who deleted the case (mention with ID)",
        "@sam *`123456789012345679`*",
    ),
    ("scope", "What a sweep covered", "Whole server"),
    (
        "content",
        "The text of the message or note, cut to fit",
        "Keeps pinging staff.",
    ),
    ("id", "Note ID (code style)", "`q413p9`"),
    (
        "count",
        "The number the log is about (cases, notes, messages, boosts)",
        "5",
    ),
    ("days", "Days", "30"),
    ("when", "The time the log was made", "▸ <t:1700000000:F>"),
    (
        "channel",
        "Channel (mention with ID)",
        "#general *`123456789012345680`*",
    ),
    (
        "deleted",
        "Messages a purge deleted (code style, empty without a purge)",
        "`12`",
    ),
    (
        "too_old",
        "Messages a purge could not delete (code style, empty without a purge)",
        "`2` (older than 14 days)",
    ),
    (
        "filter",
        "What a purge matched (empty without a purge)",
        "all messages",
    ),
    (
        "text",
        "Everything the default shows (for copies made before this entry had placeholders)",
        "The text Brim writes here.",
    ),
    ("title", "Title", "Ban result:"),
    (
        "details",
        "The Details block, heading and lines (empty when the command has none)",
        "🗒️ **Details:**\n▸ DM-Members: ✅",
    ),
    (
        "successful",
        "The Successful block: heading and members",
        "✅ **Successful bans**\n▸ <@1> [`1`]",
    ),
    (
        "unsuccessful",
        "The Unsuccessful block: heading and what failed",
        "❎ **Unsuccessful bans**\nAll members were banned!",
    ),
    ("ok", "How many worked", "3"),
    ("failed", "How many failed", "1"),
    (
        "reason_line",
        "The Reason line (empty without a reason)",
        "📄 Reason: spam",
    ),
    (
        "failures",
        "One line for each failure (empty when none)",
        "▸ Could not ban <@1>",
    ),
    (
        "rest",
        "More lines the bot adds under the case line (warn threshold, hint to add a reason)",
        "Add Reason: `/cases modify case:12 reason:`",
    ),
    ("member.username", "Member's username", "alex"),
    (
        "message.link",
        "Link to the message the log is about",
        "https://discord.com/channels/1/2/3",
    ),
    ("ticket", "Ticket number", "12"),
    ("ticket_type", "Ticket type (empty while unset)", "Support"),
    (
        "panel",
        "The ticket panel (empty for a ticket made by DM)",
        "Support panel",
    ),
    ("staff", "Staff member who acted (name)", "sam"),
    ("reporter", "Username of the member who reported", "sam"),
    ("appeal", "Appeal number", "42"),
    ("member.id", "Member's ID", "123456789012345678"),
    (
        "member.created_at",
        "When the member's account was made (Discord time stamp)",
        "<t:1700000000:F>",
    ),
    // D6: the general command answers.
    (
        "member.joined_at",
        "When the member joined the server (Discord time stamp)",
        "<t:1700000000:R>",
    ),
    ("in_server", "Whether the user is in the server", "❌"),
    ("left_timed_out", "Whether they left while timed out", "❌"),
    ("left_quarantined", "Whether they left while quarantined", "❌"),
    ("banned", "Whether they are banned here", "✅"),
    ("is_bot", "Whether the account is a bot", "❌"),
    ("color", "The colour of their highest coloured role", "#3498db"),
    ("badges", "Their Discord badges", "Hypesquad Online House 1"),
    (
        "suspicious",
        "Signs of a suspicious account (no avatar, young account)",
        "✅ No Avatar\n❌ Account Younger Than 7d",
    ),
    (
        "whitelist",
        "Which Brim whitelists hold the member or role",
        "✅ Spam\n❌ Ping\n❌ Advertising\n❌ @everyone",
    ),
    ("warn_points", "Their warn points (code style)", "`3`"),
    ("strikes", "Their active strikes (code style)", "`1`"),
    ("heat", "Their current heat (code style)", "`40%`"),
    (
        "dangerous",
        "The member's or role's dangerous Discord permissions",
        "Administrator, Ban Members",
    ),
    ("permits", "Their Brim permit", "Trusted Admin"),
    (
        "accessories",
        "Their account accessories (decoration, banner, Nitro, server avatar)",
        "✅ Avatar Decoration\n❌ Banner\n✅ Nitro\n❌ Server Avatar",
    ),
    ("member_roles", "The member's roles (mentions)", "@Helpers, @Trial"),
    ("webhooks", "Names of the webhooks they made", "Alerts"),
    ("role", "The role (mention with ID)", "@Helpers *`123456789012345678`*"),
    ("role.name", "The role's name", "Helpers"),
    (
        "role.created_at",
        "When the role was made (Discord time stamp)",
        "<t:1700000000:R>",
    ),
    ("position", "The role's position", "4"),
    (
        "role_details",
        "The role's settings (managed, mentionable, hoisted, main role)",
        "❌ Managed\n✅ Mentionable\n✅ Hoisted\n❌ Main Role",
    ),
    ("server.name", "Server name", "My Server"),
    ("owner", "The server owner (mention with ID)", "@sam *`123456789012345679`*"),
    (
        "server.created_at",
        "When the server was made (Discord time stamp)",
        "<t:1600000000:R>",
    ),
    ("members", "How many members the server has", "120"),
    ("role_count", "How many roles the server has", "14"),
    ("channel_count", "How many channels the server has", "30"),
    ("boosts", "How many boosts the server has", "3"),
    ("boost_tier", "The server's boost tier", "1"),
    ("role_list", "The roles on this page, top down (mentions)", "▸ @Admins\n▸ @Helpers"),
    ("page_no", "The page shown", "2"),
    ("pages", "How many pages the answer has", "3"),
    ("version", "Brim's version", "0.1.0"),
    ("uptime", "How long Brim has run", "3d"),
    ("command_count", "How many commands Brim has", "90"),
    ("latency", "The gateway latency", "42ms"),
    ("api", "How long a Discord API call took", "120ms"),
    ("ram", "The memory Brim uses", "35 MB"),
    ("cpu", "The CPU Brim uses", "0.40%"),
    ("servers", "How many servers have Brim", "12"),
    ("invited", "How many members they invited", "5"),
    ("still_here", "How many of them are still here", "3"),
    ("left", "How many of them left", "2"),
    ("since", "Since when joins are tracked", "<t:1700000000:d>"),
    (
        "notes",
        "Why a count may be low (tracking off or failing, joins not tied to an invite); empty otherwise",
        "Invite tracking is off on this server, so new joins are not counted.",
    ),
    (
        "ranking",
        "The leaderboard lines (or that none are counted yet)",
        "`1.` <@123456789012345679>: `5` (3 still here)",
    ),
    ("first_seen", "When the member was first seen joining", "<t:1700000000:f>"),
    (
        "method",
        "How the invite was found",
        "One invite grew by one at this join (observed, not confirmed by Discord)",
    ),
    ("present", "Whether the member is still here", "Yes"),
    ("joins", "How many joins of the member were seen", "1"),
    ("tracking", "Whether invite tracking is on", "On"),
    ("status", "How invite tracking is doing", "Working since <t:1700000000:R>"),
    ("unknown", "Joins in the last 7 days not tied to an invite", "0"),
    (
        "factor",
        "The filter that matched",
        "Use of blacklisted words",
    ),
    ("strike", "Strike number", "2"),
    (
        "rule",
        "The Discord AutoMod rule (its name when known, ID in code style)",
        "Bad words (`123456789012345678`)",
    ),
    ("job", "The scheduled action or check (its name)", "unban"),
    ("lock_type", "The lock type", "Channels"),
    ("took", "How long it took, in ms", "390"),
    (
        "took_exact",
        "How long it took, in ms with two decimals",
        "390.11",
    ),
    ("locks", "The active locks, one per line", "🔒 Channels"),
    (
        "lists",
        "Each whitelist with ✅ (on it) or ❌ (not on it), one per line",
        "✅ Spam Whitelist\n❌ Ping Whitelist",
    ),
    ("prefix", "The server's command prefix", "t!"),
    ("tries", "How many tries or changes failed", "5"),
    ("backup", "Backup ID", "a1b2c3"),
    (
        "slowmode",
        "The slowmode (code style), or what happened to it",
        "`10s`",
    ),
    (
        "heat_pct",
        "Channel heat that started it, in percent of the maximum",
        "80",
    ),
    ("error", "What went wrong", "Missing Permissions"),
    (
        "result",
        "What the action did (warn action, backup load)",
        "Timed out for 1 hour",
    ),
    ("roles", "The roles (mentions)", "@Helpers, @Trial"),
    ("code", "Invite code", "abc123"),
    (
        "inviter",
        "Who made the invite (mention with ID)",
        "@sam *`123456789012345679`*",
    ),
    (
        "group",
        "The group the join matched (empty when the member joined alone)",
        "Matched together with 4 joins",
    ),
    (
        "filters",
        "The filters that matched (empty when none)",
        "Account age, Avatar",
    ),
    (
        "bot",
        "Who added the bot (empty when a person joined)",
        "Yes, added by @sam",
    ),
    (
        "refused",
        "Why Discord refused the action (empty when it worked)",
        "Missing Permissions",
    ),
    (
        "same_ip",
        "Members who verified from the same IP",
        "@alex, @sam",
    ),
    ("outcome", "What happened to the member", "Kick"),
    (
        "dm_note",
        "A note when their DMs are closed (empty otherwise)",
        " (their DMs are closed, so the \"Timed out\" message did not arrive)",
    ),
    (
        "lookup_note",
        "A note when their permits could not be read (empty otherwise)",
        "Their permits could not be read (timeout), so the role was taken back to be safe.",
    ),
    (
        "alert",
        "What happened, the bold line of a security log",
        "**Gave a member a role that carries a permit**",
    ),
    (
        "action",
        "What Brim did about it",
        "taken back: Brim gives a permit's roles only to its listed members",
    ),
    (
        "repeats",
        "How often it happened before, not logged one by one (empty otherwise)",
        "taken back 6 times since <t:1700000000:R>, not logged one by one",
    ),
    ("permit", "The permit", "Moderators"),
    ("effect", "What it means for the server", "It will not come back on release."),
    ("reverted", "Changes reverted, of all", "`5/5`"),
    ("reinvite", "Members kicked by the nuker, to invite back", "@alex"),
    ("applied", "Whether the action was applied (✅ or ❌)", "✅"),
    (
        "bot_role",
        "Whether the bot role was cleared (empty for a person)",
        "✅",
    ),
    ("recreated", "Channels and roles made again", "`2`"),
    (
        "not_undone",
        "The changes that could not be undone (empty when none)",
        "\n- Delete the new channel #spam",
    ),
    (
        "not_recreated",
        "Deleted channels or roles no backup holds (empty when none)",
        "`1` deleted channels or roles did not come back: no backup holds them.",
    ),
    (
        "older_backup",
        "Things made again from an older backup (empty when none)",
        "`1` recreated roles or channels came from an older backup.",
    ),
    (
        "not_saved",
        "A warning when the case or nuke record was not saved (empty otherwise)",
        "the case or the nuke record could not be saved; see the bot's logs",
    ),
    ("taken", "Who lost which roles", "@alex: @Moderators"),
    ("listed", "Who was listed in which permit", "@alex: Moderators"),
    (
        "gone",
        "Who left and what was removed from them",
        "@alex left the server: removed Moderators",
    ),
    ("accounts", "The accounts, as mentions", "@SomeBot"),
    ("channel_name", "The channel's name", "verify"),
    ("channel_kind", "What kind of channel it was", "log channel"),
    ("made", "What Brim did about the channel", "Brim made a new one: #verify."),
    (
        "quarantined",
        "The panic case and who was quarantined (empty without a case)",
        "▸ **CASE:** `a1b2`\n🧍 **Members Quarantined:** 1 ✅ | 0 ❌",
    ),
    (
        "backup_result",
        "What the backup step did",
        "✅ **Backup System loaded the recent backup successfully!**",
    ),
    (
        "failed_steps",
        "The backup steps that failed (empty when none)",
        "roles",
    ),
    ("backup_roles", "Roles the backup changed", "Deleted: `0`"),
    ("backup_channels", "Channels the backup changed", "Deleted: `0`"),
    ("backup_categories", "Categories the backup changed", "Deleted: `0`"),
    ("backup_webhooks", "Webhooks the backup deleted", "Deleted: `0`"),
    ("backup_settings", "Settings the backup set", "▸ Server: ✅"),
    ("pattern", "The kind of raid pattern", "join"),
    (
        "next",
        "What happens to flagged accounts next",
        "Flagged accounts joining for the next `10 minutes` will be **banned**.",
    ),
    (
        "caught_link",
        "The link to every caught account (empty without a page)",
        "▸ [Every caught account](https://example.com)",
    ),
    ("flags", "The first flagged accounts", "`1234 | alex`"),
    ("raid_code", "The raid's code", "a1b2c3"),
    ("raid", "The kind of raid", "join raid"),
    ("code_line", "The raid's code line (empty without one)", "▸ `a1b2c3`"),
    ("score", "The NSFW score", "`91%`"),
    ("scan", "The scan's number", "`12`"),
];

/// One field of a default template, named as the log builder names it.
fn fact_field(name: &str, value: &str, inline: bool) -> Value {
    json!({"name": crate::embed::field_name(name), "value": value, "inline": inline})
}

/// A labelled description line of a default template.
fn fact_line(label: &str, value: &str) -> String {
    format!("{} {value}", label_head(label))
}

fn set(
    out: &mut Vec<Def>,
    (key, name, group): (&str, &str, &'static str),
    template: Value,
    (vars, required, what): (&[&'static str], &[&'static str], &'static str),
) {
    let mut d = def(key, name, group, false, vars, template);
    d.required = required.to_vec();
    d.what = what;
    match out.iter().position(|x| x.key == key) {
        Some(i) => out[i] = d,
        None => out.push(d),
    }
}

/// The D1 entries: moderation logs (the case log, case changes, notes, deleted cases, the retention
/// sweep) and the answers of the moderation commands. Each default reads exactly as the code
/// builds it (a bot test compares them); the code builds the log itself until a server edits one.
fn mod_entries(out: &mut Vec<Def>) {
    use crate::emoji::{MODERATOR, NO, REASON, YES};
    let group = "Moderation logs";
    let case_log = json!({"content": "", "embeds": [{
    "color": ORANGE,
    "description": "{when}",
    "fields": [
        fact_field("Case:", "`{case}`", true),
        fact_field("Type:", "`{kind}`", true),
        fact_field("Moderator:", "{moderator}", true),
        fact_field("Target:", &format!("{} {{member}}", crate::embed::ARROW), false),
        fact_field("Duration:", "{duration}", false),
        fact_field("Reason:", "{reason}", false),
        fact_field("Channel:", "{channel}", true),
        fact_field("Deleted:", "{deleted}", true),
        fact_field("Not deleted:", "{too_old}", true),
        fact_field("Filter:", "{filter}", false),
    ]}]});
    set(
        out,
        (
            "log.mod_command.case",
            "Case: ban, kick, warn and the other punishments",
            group,
        ),
        case_log,
        (
            &[
                "case",
                "kind",
                "member",
                "moderator",
                "duration",
                "reason",
                "channel",
                "deleted",
                "too_old",
                "filter",
                "when",
            ],
            &["case", "kind", "member", "moderator", "duration", "reason"],
            "the case number, kind, member, moderator, duration and reason",
        ),
    );
    let lines = |ls: &[String]| ls.join("\n");
    let changed = lines(&[
        fact_line("Case", "`{case}`"),
        fact_line("Moderator", "{moderator}"),
        fact_line("Written By", "{written_by}"),
        fact_line("State", "{state}"),
        fact_line("Proof", "{proof}"),
        fact_line("Removed", "{removed}"),
        "{changes}".into(),
        "{when}".into(),
    ]);
    let changed_vars: &[&'static str] = &[
        "title",
        "text",
        "case",
        "moderator",
        "written_by",
        "state",
        "proof",
        "removed",
        "changes",
        "when",
    ];
    let changed_req: &[&'static str] = &[
        "case",
        "moderator",
        "written_by",
        "state",
        "proof",
        "removed",
        "changes",
    ];
    for (title, color) in [
        ("Case Updated", ORANGE),
        ("Case Proof Added", GREEN),
        ("Case Proof Removed", RED),
    ] {
        let key = format!("log.mod_command.{}", slug(title));
        set(
            out,
            (&key, title, group),
            json!({"content": "", "embeds": [{"title": "{title}", "color": color, "description": changed}]}),
            (
                changed_vars,
                changed_req,
                "the case number, moderator, state, proof and the change of the reason",
            ),
        );
    }
    let note = lines(&[
        fact_line("Note", "{id}"),
        fact_line("Member", "{member}"),
        fact_line("Written By", "{author}"),
        fact_line("Moderator", "{moderator}"),
        fact_line("Written By", "{written_by}"),
        fact_line("Scope", "{scope}"),
        fact_line("Notes Removed", "{count}"),
        "{changes}".into(),
        fact_line("Content", "{content}"),
        "{when}".into(),
    ]);
    let note_vars: &[&'static str] = &[
        "title",
        "text",
        "id",
        "member",
        "author",
        "moderator",
        "written_by",
        "scope",
        "count",
        "changes",
        "content",
        "when",
    ];
    let note_req: &[&'static str] = &[
        "id",
        "member",
        "author",
        "moderator",
        "written_by",
        "scope",
        "count",
        "changes",
        "content",
    ];
    for (title, color) in [
        ("Note Added", GREEN),
        ("Note Modified", ORANGE),
        ("Note Removed", RED),
        ("Notes Swept", RED),
    ] {
        let key = format!("log.mod_command.{}", slug(title));
        set(
            out,
            (&key, title, group),
            json!({"content": "", "embeds": [{"title": "{title}", "color": color, "description": note}]}),
            (
                note_vars,
                note_req,
                "the note, member, moderator and what changed",
            ),
        );
    }
    let deleted = lines(&[
        fact_line("Case", "`{case}`"),
        fact_line("Kind", "{kind}"),
        fact_line("Member", "{member}"),
        fact_line("Moderator", "{moderator}"),
        fact_line("Deleted By", "{deleted_by}"),
        fact_line("Reason", "{reason}"),
        "{when}".into(),
    ]);
    set(
        out,
        ("log.mod_command.case_deleted", "Case Deleted", group),
        json!({"content": "", "embeds": [{"title": "{title}", "color": RED, "description": deleted}]}),
        (
            &[
                "title",
                "text",
                "case",
                "kind",
                "member",
                "moderator",
                "deleted_by",
                "reason",
                "when",
            ],
            &[
                "case",
                "kind",
                "member",
                "moderator",
                "deleted_by",
                "reason",
            ],
            "the case that was deleted and who deleted it",
        ),
    );
    let swept = lines(&[
        fact_line("Cases Removed", "{count}"),
        fact_line("Older Than", "{days} days"),
        fact_line("Reason", "The server's retention limit for cases"),
        "{when}".into(),
    ]);
    set(
        out,
        (
            "log.mod_command.old_cases_removed",
            "Old Cases Removed",
            group,
        ),
        json!({"content": "", "embeds": [{"title": "{title}", "color": RED, "description": swept}]}),
        (
            &["title", "text", "count", "days", "when"],
            &["count", "days"],
            "how many cases were removed and how old",
        ),
    );
    let answers = "Moderation commands";
    let result = format!(
        "{REASON} **Reason:** {{reason}}\n{MODERATOR} **Moderator:** {{moderator}}\n{{details}}\n{{successful}}\n{{unsuccessful}}"
    );
    set(
        out,
        (
            "reply.mod.result",
            "Answer: result of a moderation command",
            answers,
        ),
        json!({"content": "", "embeds": [{"title": "{title}", "color": BLUE, "description": result}]}),
        (
            &[
                "title",
                "reason",
                "moderator",
                "details",
                "successful",
                "unsuccessful",
            ],
            &[
                "title",
                "reason",
                "moderator",
                "details",
                "successful",
                "unsuccessful",
            ],
            "the reason, the moderator, the options used and who it worked or failed for",
        ),
    );
    let total = format!(
        "{YES} Successful {{kind}}s: `{{ok}}`\n{NO} Unsuccessful {{kind}}s: `{{failed}}`\n{{reason_line}}\n{MODERATOR} Moderator: {{moderator}}\n{{failures}}"
    );
    set(
        out,
        (
            "reply.mod.total",
            "Answer: short result for several members",
            answers,
        ),
        json!({"content": total, "embeds": []}),
        (
            &[
                "kind",
                "ok",
                "failed",
                "reason_line",
                "moderator",
                "failures",
            ],
            &[
                "kind",
                "ok",
                "failed",
                "reason_line",
                "moderator",
                "failures",
            ],
            "how many worked and failed, the reason, the moderator and each failure",
        ),
    );
    set(
        out,
        ("reply.mod.case", "Answer: short line of one case", answers),
        json!({"content": "Case #{case}: {kind} {member}\n{rest}", "embeds": []}),
        (
            &["case", "kind", "member", "rest"],
            &["case", "kind", "member", "rest"],
            "the case number, kind, member and what the bot adds below",
        ),
    );
}

/// One D3 log entry: title, colour, description lines (each ends with `{when}`) and fields.
struct D3 {
    key: String,
    name: &'static str,
    group: &'static str,
    title: String,
    color: u32,
    lines: Vec<String>,
    fields: Vec<Value>,
    vars: &'static [&'static str],
    required: &'static [&'static str],
    what: &'static str,
}

fn d3(out: &mut Vec<Def>, e: D3) {
    let mut lines = e.lines;
    lines.push("{when}".into());
    let mut embed = json!({"title": e.title, "color": e.color, "description": lines.join("\n")});
    if !e.fields.is_empty() {
        embed["fields"] = Value::Array(e.fields);
    }
    // `title` and `text` keep a frame made before this entry had placeholders working.
    let mut vars = vec!["title", "text", "when"];
    vars.extend(e.vars);
    set(
        out,
        (&e.key, e.name, e.group),
        json!({"content": "", "embeds": [embed]}),
        (&vars, e.required, e.what),
    );
}

/// The D3 entries: automod, Discord AutoMod rule, Join Gate and verification logs. Each default
/// reads exactly as the code builds it (`crates/bot/src/d3_texts_tests.rs` compares them).
fn d3_entries(out: &mut Vec<Def>) {
    use crate::emoji::{NO, YES};
    let l = fact_line;
    let am = "Automod logs";
    let ver = "Verification logs";
    let log = |key: &str| format!("log.{key}");
    for (slug, done, name) in [
        (
            "delete",
            "'s message has been deleted!",
            "Automod: message deleted",
        ),
        ("warn", " has been warned!", "Automod: member warned"),
        (
            "timeout",
            " has been timed out!",
            "Automod: member timed out by a filter",
        ),
        ("kick", " has been kicked!", "Automod: member kicked"),
        ("ban", " has been banned!", "Automod: member banned"),
    ] {
        let color = if matches!(slug, "kick" | "ban") {
            RED
        } else {
            ORANGE
        };
        d3(
            out,
            D3 {
                key: log(&format!("auto_mod.filter_{slug}")),
                name,
                group: am,
                title: format!("*{{member.username}}{done}*"),
                color,
                lines: vec![
                    l("Factor", "*{factor}*"),
                    l("Channel", "{channel}"),
                    l("Message", "||{content}||"),
                    l("Member", "{member}"),
                ],
                fields: vec![],
                vars: &["member.username", "factor", "channel", "content", "member"],
                required: &["member", "factor", "channel"],
                what: "the member, the filter that matched and the channel",
            },
        );
    }
    d3(
        out,
        D3 {
            key: log("auto_mod.heat_timeout"),
            name: "Automod: member timed out for spam",
            group: am,
            title: "*{member.username} has been timed out!*".into(),
            color: ORANGE,
            lines: vec![
                l("Reason", "*{reason}*"),
                l("Member", "{member}"),
                l("Channel", "{channel}"),
                l("Duration", "{duration}"),
                l("Strike", "`{strike}`"),
            ],
            fields: vec![],
            vars: &[
                "member.username",
                "reason",
                "member",
                "channel",
                "duration",
                "strike",
            ],
            required: &["member", "reason", "duration", "strike"],
            what: "the member, the reason, the timeout and the strike number",
        },
    );
    d3(
        out,
        D3 {
            key: log("auto_mod.heat_strike"),
            name: "Automod: member got a strike",
            group: am,
            title: "*{member.username} got a strike!*".into(),
            color: ORANGE,
            lines: vec![
                l("Reason", "*{reason}*"),
                l("Member", "{member}"),
                l("Strike", "`{strike}`"),
                l("Channel", "{channel}"),
            ],
            fields: vec![],
            vars: &["member.username", "reason", "member", "strike", "channel"],
            required: &["member", "reason", "strike"],
            what: "the member, the reason and the strike number",
        },
    );
    d3(out, D3 {
        key: log("auto_mod.discord_automod_exemptions_cut"),
        name: "Discord AutoMod Exemptions Cut",
        group: am,
        title: "Discord AutoMod Exemptions Cut".into(),
        color: ORANGE,
        lines: vec![
            "Discord exempts at most 20 roles from a rule, so the keyword rule leaves out the lowest-ranked permit roles exempt from the word filter. Brim's own word filter still exempts them.".into(),
            l("Left out", "{roles}"),
        ],
        fields: vec![],
        vars: &["roles"],
        required: &["roles"],
        what: "the roles left out of the rule",
    });
    d3(
        out,
        D3 {
            key: log("auto_mod.discord_automod_sync_failed"),
            name: "Discord AutoMod Sync Failed",
            group: am,
            title: "Discord AutoMod Sync Failed".into(),
            color: RED,
            lines: vec![
                l("Error", "{error}"),
                "Brim's own word filter still checks every blacklisted word.".into(),
            ],
            fields: vec![],
            vars: &["error"],
            required: &["error"],
            what: "what went wrong",
        },
    );
    d3(
        out,
        D3 {
            key: log("auto_mod.warn_action"),
            name: "Warn Action",
            group: am,
            title: "Warn Action".into(),
            color: ORANGE,
            lines: vec![l("Member", "{member}"), l("Result", "{result}")],
            fields: vec![],
            vars: &["member", "result"],
            required: &["member", "result"],
            what: "the member and what the warn action did",
        },
    );
    d3(
        out,
        D3 {
            key: log("auto_mod.automod_action_failed"),
            name: "Automod Action Failed",
            group: am,
            title: "Automod Action Failed".into(),
            color: RED,
            lines: vec![
                l("Member", "{member}"),
                l("Action", "`{kind}`"),
                l("Error", "{error}"),
            ],
            fields: vec![],
            vars: &["member", "kind", "error"],
            required: &["member", "kind", "error"],
            what: "the member, the action and what went wrong",
        },
    );
    d3(out, D3 {
        key: log("auto_mod.discord_automod_rule_approved"),
        name: "Discord AutoMod Rule Approved",
        group: am,
        title: "Discord AutoMod Rule Approved".into(),
        color: ORANGE,
        lines: vec![
            l("Rule", "{rule}"),
            l("Why", "Approved at the update to permit-checked rules. Brim acts on its hits while it stays as it is now"),
        ],
        fields: vec![],
        vars: &["rule"],
        required: &["rule"],
        what: "the rule",
    });
    d3(out, D3 {
        key: log("auto_mod.discord_automod_rule_not_approved"),
        name: "Discord AutoMod Rule Not Approved",
        group: am,
        title: "Discord AutoMod Rule Not Approved".into(),
        color: ORANGE,
        lines: vec![
            l("Rule", "{rule}"),
            l("Action", "Brim takes no action on this rule's hits: it is not as a person with a permit approved it. An Owner, Extra Owner, Trusted Admin or a permit with dashboard edit must save it in the dashboard or change it in Discord"),
        ],
        fields: vec![],
        vars: &["rule"],
        required: &["rule"],
        what: "the rule",
    });
    d3(out, D3 {
        key: log("auto_mod.rule_not_read"),
        name: "Discord AutoMod Rule Not Approved: could not read it",
        group: am,
        title: "Discord AutoMod Rule Not Approved".into(),
        color: ORANGE,
        lines: vec![
            l("Changed by", "{by}"),
            l("Rule", "{rule}"),
            l("Action", "Brim could not read the rule from Discord, so it is not approved and its hits get no Brim action. Save it in the dashboard or change it in Discord again"),
        ],
        fields: vec![],
        vars: &["by", "rule"],
        required: &["by", "rule"],
        what: "who changed the rule and the rule",
    });
    d3(out, D3 {
        key: log("auto_mod.discord_automod_rule_changed_without_a_permit"),
        name: "Discord AutoMod Rule Changed Without A Permit",
        group: am,
        title: "Discord AutoMod Rule Changed Without A Permit".into(),
        color: ORANGE,
        lines: vec![
            l("Responsible", "{by}"),
            l("Rule", "{rule}"),
            l("Action", "Brim takes no action on this rule's hits until an Owner, Extra Owner, Trusted Admin or a permit with dashboard edit changes it"),
        ],
        fields: vec![],
        vars: &["by", "rule"],
        required: &["by", "rule"],
        what: "who changed the rule and the rule",
    });
    d3(
        out,
        D3 {
            key: log("auto_mod.discord_automod_report"),
            name: "Discord AutoMod Report",
            group: am,
            title: "Discord AutoMod Report".into(),
            color: ORANGE,
            lines: vec![
                l("Member", "{member}"),
                l("Rule", "{rule}"),
                label_head("Content"),
                "{content}".into(),
            ],
            fields: vec![],
            vars: &["member", "rule", "content"],
            required: &["member", "rule", "content"],
            what: "the member, the rule and the message",
        },
    );
    d3(
        out,
        D3 {
            key: log("member.invite_used"),
            name: "Invite Used",
            group: "Member logs",
            title: "Invite Used".into(),
            color: crate::embed::DONE,
            lines: vec![
                l("Member", "{member}"),
                l("Invite", "`{code}`"),
                l("Inviter", "{inviter}"),
                l("Group", "{group}"),
            ],
            fields: vec![],
            vars: &["member", "code", "inviter", "group"],
            required: &["member", "code", "inviter"],
            what: "the member, the invite and who made it",
        },
    );
    d3(
        out,
        D3 {
            key: log("join_gate.join_gate"),
            name: "Join Gate",
            group: "Join Gate logs",
            title: "Join Gate".into(),
            color: ORANGE,
            lines: vec![
                l("Member", "{member}"),
                l("Action", "`{kind}`"),
                l("Reason", "{reason}"),
                l("Filters", "{filters}"),
                l("Bot", "{bot}"),
                format!("{NO} **Failed:** {{refused}}"),
            ],
            fields: vec![],
            vars: &["member", "kind", "reason", "filters", "bot", "refused"],
            required: &["member", "kind", "reason", "filters", "bot", "refused"],
            what: "the member, the action, the reason, the filters, who added a bot and a refusal",
        },
    );
    d3(out, D3 {
        key: log("verification.verification_channel_not_saved"),
        name: "Verification Channel Not Saved",
        group: ver,
        title: "Verification Channel Not Saved".into(),
        color: crate::embed::ALERT,
        lines: vec!["Brim made a verification channel for a new member but could not save it, so it deleted it again and tries again in 10 minutes: {error}. Fix the verification settings on the dashboard.".into()],
        fields: vec![],
        vars: &["error"],
        required: &["error"],
        what: "what went wrong",
    });
    d3(
        out,
        D3 {
            key: log("verification.possible_alt_account"),
            name: "Possible Alt Account",
            group: ver,
            title: "Possible Alt Account".into(),
            color: crate::embed::ALERT,
            lines: vec![l("Member", "{member}"), l("Same IP as", "{same_ip}")],
            fields: vec![],
            vars: &["member", "same_ip"],
            required: &["member", "same_ip"],
            what: "the member and the members with the same IP",
        },
    );
    d3(
        out,
        D3 {
            key: log("verification.verification_blocked"),
            name: "Verification Blocked",
            group: ver,
            title: "Verification Blocked".into(),
            color: crate::embed::ALERT,
            lines: vec![l("Member", "{member}"), l("Reason", "{reason}")],
            fields: vec![],
            vars: &["member", "reason"],
            required: &["member", "reason"],
            what: "the member and the reason",
        },
    );
    let result_lines = || {
        vec![
            l("Member", "<@{member.id}> *`{member.id}`*"),
            l("Creation", "{member.created_at}"),
        ]
    };
    d3(
        out,
        D3 {
            key: log("verification.passed"),
            name: "Verification result: passed",
            group: ver,
            title: "{member.username}'s Verification Result:".into(),
            color: crate::embed::DONE,
            lines: result_lines(),
            fields: vec![fact_field(
                "Status:",
                &format!("{YES} <@{{member.id}}> has passed verification successfully."),
                false,
            )],
            vars: &["member.username", "member.id", "member.created_at"],
            required: &["member.username", "member.id"],
            what: "the member",
        },
    );
    d3(
        out,
        D3 {
            key: log("verification.not_verified"),
            name: "Verification result: did not verify in time",
            group: ver,
            title: "{member.username}'s Verification Result:".into(),
            color: crate::embed::ALERT,
            lines: result_lines(),
            fields: vec![fact_field(
                "Status:",
                &format!(
                    "{NO} <@{{member.id}}> did not verify in time: **{{outcome}}**{{dm_note}}"
                ),
                false,
            )],
            vars: &[
                "member.username",
                "member.id",
                "member.created_at",
                "outcome",
                "dm_note",
            ],
            required: &["member.username", "member.id", "outcome"],
            what: "the member and what happened to them",
        },
    );
    d3(out, D3 {
        key: log("verification.role_taken"),
        name: "Verification: role given without a permit taken back",
        group: ver,
        title: "Verification".into(),
        color: ORANGE,
        lines: vec![
            l("Member", "{member}"),
            l("Given by", "{by}"),
            format!("{NO} **A role that skips verification, given without a verify permit, was taken back**"),
            l("Why", "Only Brim's verification, a verify or quarantine permit, or an owner or trusted admin can verify a member."),
            l("Note", "{lookup_note}"),
        ],
        fields: vec![],
        vars: &["member", "by", "lookup_note"],
        required: &["member", "by"],
        what: "the member and who gave the role",
    });
    for (slug, name, what) in [
        (
            "bot_role_taken",
            "Verification: role given by another bot taken back",
            "A role that skips verification, given by another bot, was taken back",
        ),
        (
            "offline_role_taken",
            "Verification: role given while Brim was offline taken back",
            "A verification role given while Brim was offline, by another bot or without a verify permit, was taken back",
        ),
        (
            "channel_permission_taken",
            "Verification: channel permission given by another bot taken back",
            "A channel permission given by another bot was taken back",
        ),
    ] {
        d3(out, D3 {
            key: log(&format!("verification.{slug}")),
            name,
            group: ver,
            title: "Verification".into(),
            color: ORANGE,
            lines: vec![
                l("Member", "{member}"),
                l("Bot", "{by}"),
                format!("{NO} **{what}**"),
                l("Why", "Brim's verification is on, so members verify through it. Turn verification off to let another bot verify."),
            ],
            fields: vec![],
            vars: &["member", "by"],
            required: &["member", "by"],
            what: "the member and who gave the role",
        });
    }
    d3(out, D3 {
        key: log("verification.catch_up_cap"),
        name: "Verification: offline check read only the newest changes",
        group: ver,
        title: "Verification".into(),
        color: ORANGE,
        lines: vec!["Checked only the newest 500 role changes for verification roles given while Brim was offline; older grants from that time were not checked.".into()],
        fields: vec![],
        vars: &[],
        required: &[],
        what: "",
    });
    d3(
        out,
        D3 {
            key: log("verification.take_back_failed"),
            name: "Verification: could not take back a role",
            group: ver,
            title: "Verification".into(),
            color: ORANGE,
            lines: vec![
                l("Member", "{member}"),
                l("Bot", "{by}"),
                format!("{NO} **Could not take back a verification role given by another bot**"),
                l("Error", "{error}"),
                l(
                    "Effect",
                    "The member may skip verification. Check their roles.",
                ),
            ],
            fields: vec![],
            vars: &["member", "by", "error"],
            required: &["member", "by", "error"],
            what: "the member, the bot and what went wrong",
        },
    );
}

/// The D5 log entries: lockdown, auto slowmode, backup and scheduled action logs. Each default
/// reads exactly as the code builds it (`crates/bot/src/d5_texts_tests.rs` compares them).
fn d5_entries(out: &mut Vec<Def>) {
    use crate::embed::{ALERT, DONE};
    use crate::emoji::{NO, YES};
    let l = fact_line;
    let (srv, am) = ("Server logs", "Automod logs");
    let plain = |out: &mut Vec<Def>,
                 key: &str,
                 name: &'static str,
                 group: &'static str,
                 title: &str,
                 color: u32,
                 lines: Vec<String>,
                 vars: &'static [&'static str],
                 required: &'static [&'static str],
                 what: &'static str| {
        d3(
            out,
            D3 {
                key: format!("log.{key}"),
                name,
                group,
                title: title.into(),
                color,
                lines,
                fields: vec![],
                vars,
                required,
                what,
            },
        );
    };
    for (slug, title, color, line) in [
        (
            "server_locked",
            "Server Locked",
            ALERT,
            "The server has been locked down.",
        ),
        (
            "server_unlocked",
            "Server Unlocked",
            DONE,
            "The server has been unlocked.",
        ),
    ] {
        plain(
            out,
            &format!("mod_command.{slug}"),
            title,
            "Moderation logs",
            title,
            color,
            vec![format!("{YES} **{line}**")],
            &[],
            &[],
            "",
        );
    }
    plain(out, "server.unlock_incomplete", "Unlock Incomplete", srv, "Unlock Incomplete", ALERT,
        vec!["`{tries}` permission changes could not be undone. Check the bot's role and permissions, then fix them by hand.".into()],
        &["tries"], &["tries"], "how many permission changes could not be undone");
    plain(out, "server.startup_check_failed", "Scheduled Action Failed: a startup check gave up", srv, "Scheduled Action Failed", RED,
        vec![l("Action", "`{job}`"), "Gave up after {tries} failed tries. It runs again when Brim reconnects; check the bot log.".into()],
        &["job", "tries"], &["job", "tries"], "the check and how many tries failed");
    plain(
        out,
        "server.scheduled_action_failed",
        "Scheduled Action Failed",
        srv,
        "Scheduled Action Failed",
        RED,
        vec![
            l("Action", "`{job}`"),
            l("Error", "{error}"),
            "Gave up after {tries} tries, do it by hand.".into(),
        ],
        &["job", "error", "tries"],
        &["job", "error", "tries"],
        "the action, what went wrong and how many tries failed",
    );
    plain(
        out,
        "server.scheduled_action_dropped",
        "Scheduled Action Dropped",
        srv,
        "Scheduled Action Dropped",
        RED,
        vec![
            l("Action", "`{job}`"),
            l("Asked by", "{by}"),
            "{reason}".into(),
        ],
        &["job", "by", "reason"],
        &["job", "by", "reason"],
        "the action, who asked for it and why it was dropped",
    );
    plain(
        out,
        "server.backup_created",
        "Backup Created",
        srv,
        "Backup Created",
        DONE,
        vec![l("Id", "`{backup}`")],
        &["backup"],
        &["backup"],
        "the backup ID",
    );
    for (slug, title, color, mark) in [
        ("backup_loaded", "Backup Loaded", DONE, YES),
        ("backup_not_loaded", "Backup Not Loaded", RED, NO),
    ] {
        plain(
            out,
            &format!("server.{slug}"),
            title,
            srv,
            title,
            color,
            vec![l("Id", "`{backup}`"), format!("{mark} **{{result}}**")],
            &["backup", "result"],
            &["backup", "result"],
            "the backup ID and what the load did",
        );
    }
    plain(
        out,
        "auto_mod.auto_slowmode_on",
        "Auto Slowmode On",
        am,
        "Auto Slowmode On",
        ORANGE,
        vec![
            l("Channel", "{channel}"),
            l("Slowmode", "{slowmode}"),
            l("Factor", "*{heat_pct}% of max heat*"),
        ],
        &["channel", "slowmode", "heat_pct"],
        &["channel", "slowmode"],
        "the channel and the slowmode",
    );
    plain(
        out,
        "auto_mod.auto_slowmode_off",
        "Auto Slowmode Off",
        am,
        "Auto Slowmode Off",
        DONE,
        vec![l("Channel", "{channel}"), l("Slowmode", "{slowmode}")],
        &["channel", "slowmode"],
        &["channel", "slowmode"],
        "the channel and the slowmode now",
    );
    for (slug, title) in [
        ("auto_slowmode_failed", "Auto Slowmode Failed"),
        ("auto_slowmode_off_failed", "Auto Slowmode Off Failed"),
    ] {
        plain(
            out,
            &format!("auto_mod.{slug}"),
            title,
            am,
            title,
            RED,
            vec![l("Channel", "{channel}"), l("Error", "{error}")],
            &["channel", "error"],
            &["channel", "error"],
            "the channel and what went wrong",
        );
    }
}

/// The D7 entries: ticket, report, appeal and catch channel logs. Each default reads exactly as
/// the code builds it (`crates/bot/src/d7_texts_tests.rs` compares them). Footers, thumbnails and
/// the ticket closer's author line stay as the code builds them unless an edit sets its own.
fn d7_entries(out: &mut Vec<Def>) {
    use crate::embed::ARROW;
    use crate::emoji::{AGE, MEMBER};
    let l = fact_line;
    let log =
        |out: &mut Vec<Def>,
         (key, name, group): (&str, &str, &'static str),
         title: &str,
         color: u32,
         lines: &[String],
         fields: Vec<Value>,
         (vars, required, what): (&[&'static str], &[&'static str], &'static str)| {
            let mut e = json!({"color": color, "description": lines.join("\n")});
            if !title.is_empty() {
                e["title"] = json!(title);
            }
            if !fields.is_empty() {
                e["fields"] = Value::Array(fields);
            }
            // `title` and `text` keep a frame made before this entry had placeholders working.
            let mut all = vec!["title", "text", "when"];
            all.extend(vars);
            set(
                out,
                (&format!("log.{key}"), name, group),
                json!({"content": "", "embeds": [e]}),
                (&all, required, what),
            );
        };
    let (tk, rp, ap) = ("Ticket logs", "Report logs", "Appeal logs");
    log(
        out,
        ("ticket.new_ticket", "New Ticket", tk),
        "New Ticket",
        GREEN,
        &[l("User", "{member}"), "{when}".into()],
        vec![],
        (
            &["member", "member.username", "member.id", "panel"],
            &["member"],
            "the member who opened the ticket",
        ),
    );
    log(
        out,
        ("ticket.ticket_closed", "Ticket Closed", tk),
        "Ticket Closed",
        RED,
        &[
            "{reason}".into(),
            l("User", "{member}"),
            l("Type", "{ticket_type}"),
            "{when}".into(),
        ],
        vec![fact_field("Changes", "{changes}", false)],
        (
            &["reason", "member", "ticket_type", "changes", "staff"],
            &["reason", "member"],
            "the close reason and the member",
        ),
    );
    for (slug, name, line, vars, required) in [
        (
            "claimed",
            "Ticket Updated: claimed",
            "{by} claimed this ticket.",
            &["ticket", "by"][..],
            &["ticket", "by"][..],
        ),
        (
            "unclaimed",
            "Ticket Updated: claim removed",
            "{by} removed the claim of {member}.",
            &["ticket", "by", "member"],
            &["ticket", "by", "member"],
        ),
        (
            "reassigned",
            "Ticket Updated: given to another staff member",
            "{by} gave this ticket to {member}.",
            &["ticket", "by", "member"],
            &["ticket", "by", "member"],
        ),
        (
            "type_changed",
            "Ticket Updated: type changed",
            "{by} changed the ticket type from **{before}** to **{after}**.",
            &["ticket", "by", "before", "after"],
            &["ticket", "by", "before", "after"],
        ),
        (
            "type_set",
            "Ticket Updated: type set",
            "{by} set the ticket type to **{after}**.",
            &["ticket", "by", "after"],
            &["ticket", "by", "after"],
        ),
    ] {
        log(
            out,
            (&format!("ticket.{slug}"), name, tk),
            "Ticket Updated",
            BLUE,
            &[format!("Ticket #{{ticket}}: {line}"), "{when}".into()],
            vec![],
            (vars, required, "the ticket number and what changed"),
        );
    }
    log(
        out,
        ("report.message_reported", "Message reported", rp),
        "",
        ORANGE,
        &[
            "{member}'s [message]({message.link}) has been reported by {by}".into(),
            format!("{MEMBER} **Member:** {{member.username}} [{{member.id}}]"),
            format!("{AGE} **Creation:** {{member.created_at}}"),
            "{when}".into(),
        ],
        vec![
            fact_field("Reported Message:", "{content}", false),
            fact_field("Reason:", "{reason}", false),
        ],
        (
            &[
                "member",
                "member.username",
                "member.id",
                "member.created_at",
                "message.link",
                "by",
                "reporter",
                "content",
                "reason",
            ],
            &["member", "by", "reason"],
            "the member, who reported the message and why",
        ),
    );
    // The headline sits in the author line next to the member's avatar.
    if let Some(d) = out
        .iter_mut()
        .find(|d| d.key == "log.report.message_reported")
    {
        // The avatar beside it stays the code's (`overlay::over`): links are never filled.
        d.template["embeds"][0]["author"] =
            json!({"name": "A message by {member.username} has been reported!"});
    }
    log(
        out,
        (
            "report.discord_automod_report",
            "Discord AutoMod Report",
            rp,
        ),
        "Discord AutoMod Report",
        ORANGE,
        &[
            "{member}'s message was blocked by Discord AutoMod rule {rule}".into(),
            "{when}".into(),
        ],
        vec![fact_field("Blocked Message:", "{content}", false)],
        (
            &["member", "member.id", "rule", "content"],
            &["member", "rule"],
            "the member and the rule",
        ),
    );
    log(
        out,
        ("appeal.appeal_sent", "Appeal sent", ap),
        "A {kind} appeal by {member.username} has been sent!",
        ORANGE,
        &[
            l("Member", "<@{member.id}> *`{member.id}`*"),
            l("Appeal", "`#{appeal}`"),
            "{when}".into(),
            format!("{ARROW} Review it on the dashboard."),
        ],
        vec![],
        (
            &["kind", "member.username", "member.id", "appeal"],
            &["kind", "member.username", "appeal"],
            "the kind of appeal, the member and the appeal number (the answers follow as fields)",
        ),
    );
    log(
        out,
        ("appeal.appeal_not_accepted", "Appeal Not Accepted", ap),
        "Appeal Not Accepted",
        RED,
        &[
            l("Appeal", "#{appeal}"),
            l("Staff", "{by}"),
            "{error}".into(),
            "{when}".into(),
        ],
        vec![],
        (
            &["appeal", "by", "error"],
            &["appeal", "by", "error"],
            "the appeal, the staff member and why it was not accepted",
        ),
    );
    log(
        out,
        (
            "auto_mod.catch_channel_test_mode",
            "Catch channel (test mode)",
            "Automod logs",
        ),
        "Catch channel (test mode)",
        RED,
        &[
            l("Member", "{member}"),
            l("Action", "None, test mode only logs"),
            "{when}".into(),
        ],
        vec![],
        (&["member"], &["member"], "the member who posted"),
    );
    log(
        out,
        (
            "auto_mod.catch_channel_action_failed",
            "Catch channel action failed",
            "Automod logs",
        ),
        "Catch channel action failed",
        RED,
        &[
            l("Member", "{member}"),
            l("Error", "{error}"),
            "{when}".into(),
        ],
        vec![],
        (
            &["member", "error"],
            &["member", "error"],
            "the member who posted and what went wrong",
        ),
    );
}

/// The D4 security logs (anti-nuke, panic, join raid, and the NSFW and webhook spam logs): key,
/// name, colour, description lines and fields (name, value, inline). A `Label|value` line is a
/// labelled line; `[yes]` and `[arrow]` are those emojis. The title is always `{title}` (the kind of
/// event) and every placeholder shown is required, so an edit cannot drop who did what, to whom,
/// the counts, or that the alarm fired. `crates/bot/src/d4_texts_tests.rs` lays each default
/// beside the code's log.
#[allow(clippy::type_complexity)]
pub const SECURITY_LOGS: &[(&str, &str, u32, &[&str], &[(&str, &str, bool)])] = &[
    ("log.anti_nuke.settings_unread", "Anti-nuke: settings could not be read", ALERT, &["{alert}", "Effect|Dangerous actions are saved for `/revert` but not counted, so nobody is quarantined until the settings can be read again."], &[]),
    ("log.anti_nuke.permit_role_taken_back", "Anti-nuke: role with a permit taken back", ALERT, &["Responsible|{by}", "Member|{member}", "{alert}", "Roles|{roles}", "Action|{action}", "Repeats|{repeats}"], &[]),
    ("log.anti_nuke.permit_role_given_back", "Anti-nuke: permit role given back", ALERT, &["Responsible|{by}", "Member|{member}", "{alert}", "Permit|{permit}", "Roles|{roles}", "Action|{action}", "Repeats|{repeats}"], &[]),
    ("log.anti_nuke.permit_roles_slowed", "Anti-nuke: setting back permit roles slowed down", ALERT, &["Responsible|{by}", "Member|{member}", "{alert}", "Action|{action}"], &[]),
    ("log.anti_nuke.locked_target_changed", "Anti-nuke: locked target changed in a lockdown", ALERT, &["Responsible|{by}", "{alert}", "Target|{target}", "Change|{result}"], &[]),
    ("log.anti_nuke.managed_role_dangerous", "Anti-nuke: managed role with dangerous permissions", ALERT, &["{alert}", "Role|{role}", "Effect|Enforce permits cannot take a bot or integration role off. Remove the permissions from the role instead."], &[]),
    ("log.anti_nuke.dangerous_roles_removed", "Anti-nuke: dangerous roles removed", ALERT, &["Member|{member}", "{alert}", "Roles|{roles}", "Result|{result}", "Reason|holds dangerous permissions without a permit"], &[]),
    ("log.anti_nuke.quarantine_role_missing", "Anti-nuke: quarantine role missing", ALERT, &["{alert}", "Effect|Quarantined members lose their roles but get no Quarantine role. Pick or make a new one in the anti-nuke settings."], &[]),
    ("log.anti_nuke.quarantined", "Anti-nuke: member quarantined", ALERT, &["CASE|{case}", "Responsible|{by}", "{alert}", "Reason|{reason}"], &[]),
    ("log.anti_nuke.rate_limit_reverted", "Anti-nuke: command rate limit reverted", ALERT, &["Responsible|{by}", "{alert}", "Reason|command rate limit exceeded", "Reverted|{reverted}", "Kicked, re-invite|{reinvite}"], &[]),
    ("log.anti_nuke.quarantined_rejoined", "Anti-nuke: quarantined member rejoined", ALERT, &["Member|{member}", "{alert}", "Reason|Rejoined the server while quarantined"], &[]),
    ("log.anti_nuke.channel_access_not_taken", "Anti-nuke: channel access not taken from a quarantined member", ALERT, &["Member|{member}", "{alert}", "Error|{error}", "Effect|Their own or a managed role's overwrite may still let them in there. Check it."], &[]),
    ("log.anti_nuke.overwrite_not_saved", "Anti-nuke: quarantined member's overwrite not saved", ALERT, &["Member|{member}", "{alert}", "Channel|{channel}", "Effect|{effect}"], &[]),
    ("log.anti_nuke.immune_permits_unread", "Anti-nuke: limit reached while permits could not be read", ALERT, &["Responsible|{by}", "{alert}", "Action|they are immune, so nothing was undone and nobody was punished"], &[]),
    ("log.anti_nuke.contained_by_retry", "Anti-nuke: contained by a retry", ALERT, &["CASE|{case}", "Responsible|{by}", "{alert}", "Next|contained by a retry; the earlier report said they were not"], &[]),
    ("log.anti_nuke.revert_failed", "Anti-nuke: revert failed", ALERT, &["Responsible|{by}", "{alert}", "Reason|{reason}", "Next|It is tried again by itself; `/revert` also works"], &[]),
    ("log.anti_nuke.settings_unread_in_nuke", "Anti-nuke: settings could not be read during a nuke", ALERT, &["Responsible|{by}", "{alert}", "Reason|{error}", "Effect|The nuker was contained and reverted with the last settings loaded, or the defaults. Check the revert; `/revert` can run it again."], &[]),
    ("log.anti_nuke.report", "Anti-nuke: nuke report", ALERT, &["CASE|{case}", "Responsible|{by}", "{alert}", "Reason|{reason}", "**More Details:**", "Action Applied|{applied}", "Bot Role Cleansing|{bot_role}", "**Revert Report:**", "Reverted|{reverted}", "Recreated|{recreated}", "Kicked, re-invite|{reinvite}", "Could not undo|{not_undone}", "Not recreated|{not_recreated}", "Older backup|{older_backup}", "Not saved|{not_saved}"], &[]),
    ("log.anti_nuke.unverified_roles_taken", "Anti-nuke: roles given to an unverified member", ALERT, &["Responsible|{by}", "Member|{member}", "{alert}", "Roles|{roles}", "Action|taken back: the member is held until they verify. Anti-nuke is off, so nobody was punished"], &[]),
    ("log.anti_nuke.overwrite_not_taken_back", "Anti-nuke: channel access of a quarantined member not taken back", ALERT, &["Responsible|{by}", "Member|{member}", "{alert}", "Channel|{channel}", "Error|{error}", "Effect|The member may have access to this channel. Check its overwrites."], &[]),
    ("log.anti_nuke.change_undone", "Anti-nuke: guarded change undone", ALERT, &["Responsible|{by}", "Member|{member}", "{alert}", "Action|{action}"], &[]),
    ("log.anti_nuke.roles_not_taken_back", "Anti-nuke: roles of a quarantined member not taken back", ALERT, &["Member|{member}", "{alert}", "Error|{error}", "Effect|The member may hold roles again. Check them and quarantine again if needed."], &[]),
    ("log.anti_nuke.mod_stats_role_removed", "Anti-nuke: View Mod Stats role removed", ALERT, &["Member|{member}", "Given by|{by}", "{alert}", "Roles|{roles}", "Reason|only owners can give roles that see moderator stats"], &[]),
    ("log.anti_nuke.permit_on_handed_out_role", "Anti-nuke: permit on a role Discord hands out", ALERT, &["{alert}", "Roles|{roles}", "Effect|the permit is ignored for these roles: Discord gives them through Onboarding, a linked role or a server subscription, so anyone could get the permit. It counts again once the role is no longer handed out that way; else put the permit on a staff role that only staff hand out."], &[]),
    ("log.anti_nuke.audit_gap", "Anti-nuke: audit log not all checked", ALERT, &["**{reason}.** Actions made while it was away were not all checked by anti-nuke. Check the audit log."], &[]),
    ("log.anti_nuke.permit_roles_taken", "Permits: roles taken from members not listed", ALERT, &["{alert}", "Taken|{taken}", "Action|a role alone gives no permit, so nobody was punished. To give someone a permit, add them to it on the dashboard"], &[]),
    ("log.anti_nuke.permits_moved", "Permits: moved from roles to members", ORANGE, &["{alert}", "Each holder of a permit role was listed in the permit (one permit each). From now on Brim gives a permit's roles to its listed members and takes them from everyone else; change who is listed on the dashboard.", "Listed|{listed}"], &[]),
    ("log.anti_nuke.permit_holders_left", "Permits: holders who left", ORANGE, &["{gone}"], &[]),
    ("log.anti_nuke.bot_check_failed", "Anti-nuke: bot check failed", ORANGE, &["Could not check if {accounts} are bots, so they stay listed. Only users can be Extra Owner, Trusted Admin or on the quarantine whitelist; the check runs again at the next start."], &[]),
    ("log.anti_nuke.bots_removed", "Anti-nuke: bots removed from user-only lists", ORANGE, &["Bot accounts were removed: {accounts}. Only users can be Extra Owner, Trusted Admin or on the quarantine whitelist. A bot permit gives a bot exactly the whitelist flags you pick: add the bot as a member of a permit on the dashboard's Permits page."], &[]),
    ("log.anti_nuke.verification_channel_not_remade", "Anti-nuke: verification channel deleted, not made again", ALERT, &["The verification channel #{channel_name} was deleted and Brim could not make a new one: {error}"], &[]),
    ("log.anti_nuke.verification_channel_deleted_again", "Anti-nuke: verification channel deleted again", ALERT, &["The verification channel #{channel_name} was deleted again after {count} new ones in 10 minutes. Brim stopped making new ones. Check who is deleting it."], &[]),
    ("log.anti_nuke.verification_channel_not_set_up", "Anti-nuke: verification channel made again, not set up", ALERT, &["Brim made the verification channel {channel} again, but could not set it up: {error}. Press Setup Channel on the dashboard."], &[]),
    ("log.anti_nuke.verification_channel_level_unread", "Anti-nuke: verification channel deleted, level not read", ALERT, &["The verification channel #{channel_name} was deleted by {by}. {made} Brim could not read the permit level of {by}, so nobody was punished. Check who this is."], &[]),
    ("log.anti_nuke.verification_channel_deleted_by_owner", "Anti-nuke: verification channel deleted by the owner", ALERT, &["The verification channel #{channel_name} was deleted by the server owner. {made}"], &[]),
    ("log.anti_nuke.verification_channel_deleted_by_trusted", "Anti-nuke: verification channel deleted by a trusted member", ALERT, &["The verification channel #{channel_name} was deleted by {by}. {made} {by} is trusted, so only this message was sent. If they delete it again, they will be quarantined."], &[]),
    ("log.anti_nuke.verification_channel_not_quarantined", "Anti-nuke: verification channel deleted, deleter not quarantined", ALERT, &["The verification channel #{channel_name} was deleted by {by}. {made} Brim could not quarantine them: {error}"], &[]),
    ("log.anti_nuke.protected_channel_deleted", "Anti-nuke: log channel or other kept channel deleted", ALERT, &["The {channel_kind} #{channel_name} was deleted by {by}. {made}"], &[]),
    ("log.anti_nuke.log_channel_locked", "Anti-nuke: log channel locked", ALERT, &["Brim locked the log channel #{channel_name}: only Brim may delete messages or the channel there now."], &[]),
    ("log.anti_nuke.log_channel_lock_changed", "Anti-nuke: log channel lock changed", ALERT, &["{by} changed the permissions on the log channel #{channel_name} so others could delete messages or the channel. Brim put the lock back."], &[]),
    ("log.anti_nuke.log_messages_deleted", "Anti-nuke: messages deleted in a log channel", ALERT, &["{by} deleted {count} message(s) in the log channel {channel}. Deleted messages cannot be brought back."], &[]),
    ("log.panic.heat_panic", "Panic: heat panic mode triggered", ALERT, &["[arrow] {count} raiders will be timed out on every message for {duration}."], &[]),
    ("log.panic.triggered", "Panic: panic mode triggered", ALERT, &["{alert}", "[arrow] Brim will lockdown the server and run a scan."], &[]),
    ("log.panic.lockdown", "Panic: server locked down", ALERT, &["{alert}"], &[]),
    ("log.panic.report", "Panic: panic mode report", DONE, &["{quarantined}", "**Backup System Report:**", "{backup_result}", "Error|{error}", "Failed|{failed_steps}"], &[("Roles", "{backup_roles}", true), ("Channels", "{backup_channels}", true), ("Categories", "{backup_categories}", true), ("Webhooks", "{backup_webhooks}", true), ("Settings", "{backup_settings}", true)]),
    ("log.panic.ended", "Panic: panic mode ended", DONE, &["{alert}", "Ended by|{by}"], &[]),
    ("log.panic.server_locked", "Panic: server locked by automod", ALERT, &["{alert}", "Member|{member}", "Reason|{reason}"], &[]),
    ("log.join_raid.raid_started", "Join raid: raid started", ALERT, &["[yes] Brim has identified a weird {pattern} pattern.", "[arrow] {next}", "[arrow] A full report will be posted once the duration ends.", "{caught_link}"], &[("Initial Flags:", "{flags}", false), ("Code:", "`{raid_code}`", false)]),
    ("log.join_raid.raid_report", "Join raid: raid report", DONE, &["[yes] `{count}` users have been caught by the {raid}.", "{code_line}"], &[]),
    ("log.auto_mod.nsfw_image_found", "Automod: NSFW image found", ALERT, &["Member|{member}", "Channel|{channel}", "Score|{score}", "Scan|{scan}"], &[]),
    ("log.auto_mod.webhooks_deleted", "Automod: webhooks deleted", RED, &["Count|{count}", "Reason|Webhook spam"], &[]),
];

fn d4_entries(out: &mut Vec<Def>) {
    use crate::emoji::{ARROW, YES};
    for (key, name, color, lines, fields) in SECURITY_LOGS {
        let line = |l: &str| {
            let l = l
                .replace("[yes]", &YES.to_string())
                .replace("[arrow]", &ARROW.to_string());
            match l.split_once('|') {
                Some((label, value)) => fact_line(label, value),
                None => l,
            }
        };
        let mut lines: Vec<String> = lines.iter().map(|l| line(l)).collect();
        lines.push("{when}".into());
        let mut e = json!({"title": "{title}", "color": color, "description": lines.join("\n")});
        if !fields.is_empty() {
            e["fields"] = fields
                .iter()
                .map(|(n, v, i)| fact_field(n, v, *i))
                .collect();
        }
        let template = json!({"content": "", "embeds": [e]});
        let mut required: Vec<&'static str> = Vec::new();
        for v in shown(&template) {
            let n = FACT_VARS
                .iter()
                .find(|(n, _, _)| *n == v)
                .unwrap_or_else(|| panic!("{key}: {{{v}}} is not in FACT_VARS"))
                .0;
            if !matches!(n, "when" | "title") && !required.contains(&n) {
                required.push(n);
            }
        }
        // No `text`: a frame around it would skip every fact check (`is_frame`), and no copy of
        // these entries predates their facts.
        let mut vars = vec!["title", "when"];
        vars.extend(required.iter().copied());
        // `build` puts `{title}` first: the title says what kind of event it is.
        set(
            out,
            (key, name, SECURITY),
            template,
            (&vars, &required, "who did what, to whom and where"),
        );
    }
}

/// One D5 answer: an embed with an optional title and the given lines (no `{when}`).
#[allow(clippy::too_many_arguments)]
fn answer(
    out: &mut Vec<Def>,
    (key, name): (&str, &'static str),
    title: &str,
    color: u32,
    lines: &[String],
    vars: &'static [&'static str],
    required: &'static [&'static str],
    what: &'static str,
) {
    let mut e = json!({"color": color, "description": lines.join("\n")});
    if !title.is_empty() {
        e["title"] = json!(title);
    }
    set(
        out,
        (key, name, "Lockdown"),
        json!({"content": "", "embeds": [e]}),
        (vars, required, what),
    );
}

/// The D5 answers of the lock and unlock commands. Each default reads exactly as the code builds
/// it (`d5_texts_tests`); the code builds the answer itself until a server edits the entry.
fn lock_answers(out: &mut Vec<Def>) {
    use crate::emoji::{ANNOUNCE, ARROW, LOCK, MODERATOR, NO, RUNNING, UNLOCK, YES};
    for (key, name, title, verb) in [
        (
            "reply.lock.locking",
            "Answer: lock is running",
            "Brim Lockdown:",
            "Enabling",
        ),
        (
            "reply.lock.unlocking",
            "Answer: unlock is running",
            "Brim Unlocking:",
            "Disabling",
        ),
    ] {
        answer(
            out,
            (key, name),
            title,
            BLUE,
            &[format!("{RUNNING} {verb} `{{lock_type}}` lockdown!")],
            &["lock_type"],
            &["lock_type"],
            "the lock type",
        );
    }
    for (key, name, title, icon, line, ok, verb) in [
        (
            "reply.lock.locked",
            "Answer: server locked",
            "Brim Lockdown:",
            LOCK,
            "has been successfully enforced",
            "Lockdown",
            "locking down",
        ),
        (
            "reply.lock.unlocked",
            "Answer: server unlocked",
            "Brim Unlocking:",
            UNLOCK,
            "has been lifted",
            "Unlocking",
            "unlocking",
        ),
    ] {
        let lines = [
            format!("{icon} `{{lock_type}}` lockdown mode {line} in `{{took}} ms`."),
            String::new(),
            format!("{YES} **{ok} successful!**"),
            format!("{ARROW} *Brim has finished {verb} in `{{took_exact}} ms`.*"),
            format!("{MODERATOR} **Staff:** {{moderator}}"),
            fact_line("Type", "{lock_type}"),
        ];
        answer(
            out,
            (key, name),
            title,
            GREEN,
            &lines,
            &["lock_type", "took", "took_exact", "moderator"],
            &["lock_type", "moderator"],
            "the lock type and the staff member",
        );
    }
    for (key, name, color, icon, done, body) in [
        (
            "reply.lock.channel_locked",
            "Answer: channel locked",
            RED,
            LOCK,
            "locked",
            "You will gain access again once the lockdown is lifted.",
        ),
        (
            "reply.lock.channel_unlocked",
            "Answer: channel unlocked",
            GREEN,
            UNLOCK,
            "unlocked",
            "Everyone now has access to this channel.",
        ),
    ] {
        answer(
            out,
            (key, name),
            "",
            color,
            &[
                format!("{icon} **{{channel}} has been {done}!**"),
                body.into(),
            ],
            &["channel"],
            &["channel"],
            "the channel",
        );
    }
    for (key, name, title) in [
        (
            "reply.lock.lock_result",
            "Answer: lock not done",
            "Lock Result:",
        ),
        (
            "reply.lock.unlock_result",
            "Answer: unlock not done",
            "Unlock Result:",
        ),
    ] {
        answer(
            out,
            (key, name),
            title,
            BLUE,
            &[format!("{ARROW} {{result}}")],
            &["result"],
            &["result"],
            "why it was not done",
        );
    }
    answer(
        out,
        ("reply.lock.none_active", "Answer: no active locks"),
        "Active lockdown modes:",
        RED,
        &[
            format!("{NO} There are no active lockdown modes currently!"),
            String::new(),
            format!("{ANNOUNCE} **Add a lockdown:**"),
            "You can add a lockdown by doing `{prefix}lock ?t [TYPE]`".into(),
        ],
        &["prefix"],
        &[],
        "",
    );
    answer(
        out,
        ("reply.lock.active", "Answer: active locks"),
        "Active lockdown modes:",
        RED,
        &["{locks}".into()],
        &["locks"],
        &["locks"],
        "the active locks",
    );
}

/// The D2 entries: every server event log built from Discord's audit log. One template for all,
/// as `render::audit` builds them (a bot test compares them); the changes stay one block.
fn audit_entries(out: &mut Vec<Def>) {
    let text = [
        fact_line("Responsible", "{by}"),
        fact_line("Channel", "{channel}"),
        fact_line("Target", "{target}"),
        "{when}".into(),
        fact_line("Reason", "{reason}"),
        "{changes}".into(),
    ]
    .join("\n");
    for (kind, titles) in AUDIT_LOGS {
        let group = LOGS.iter().find(|l| l.0 == *kind).map_or("", |l| l.1);
        for title in *titles {
            set(
                out,
                (&format!("log.{kind}.{}", slug(title)), title, group),
                json!({"content": "", "embeds": [{"title": "{title}", "description": text}]}),
                (
                    &[
                        "title", "text", "by", "channel", "target", "reason", "changes", "when",
                    ],
                    &["by", "channel", "target", "reason", "changes"],
                    "who did it, what was acted on, the reason and what changed",
                ),
            );
        }
    }
}

/// The D2 part 2 entries: the gateway and bot-made server event logs ([`EVENT_LOGS`]), each as
/// its builder lays it out (tests compare them).
fn event_entries(out: &mut Vec<Def>) {
    for (kind, title, spec) in EVENT_LOGS {
        let lines: Vec<String> = spec
            .iter()
            .map(|l| match l.split_once('|') {
                Some((label, "")) => label_head(label),
                Some((label, value)) => fact_line(label, value),
                None => (*l).to_string(),
            })
            .collect();
        let mut vars = Vec::new();
        collect(&Value::String(lines.join("\n")), &mut vars);
        let mut required = Vec::new();
        collect(&Value::String(visible(&lines.join("\n"))), &mut required);
        let known = |n: &String| -> &'static str {
            FACT_VARS
                .iter()
                .find(|v| v.0 == n)
                .map(|v| v.0)
                .unwrap_or_else(|| panic!("{{{n}}} is not in FACT_VARS"))
        };
        let mut vars: Vec<&'static str> = vars.iter().map(known).collect();
        let mut required: Vec<&'static str> = required
            .iter()
            .map(known)
            .filter(|v| *v != "when")
            .collect();
        vars.extend(["title", "text"]);
        vars.sort_unstable();
        vars.dedup();
        required.sort_unstable();
        required.dedup();
        let group = LOGS.iter().find(|l| l.0 == *kind).map_or("", |l| l.1);
        set(
            out,
            (&format!("log.{kind}.{}", slug(title)), title, group),
            json!({"content": "", "embeds": [{"title": "{title}", "description": lines.join("\n")}]}),
            (&vars, &required, "the details of the log"),
        );
    }
}

/// The general command answers (plan D6): entries of facts like [`FACTS`], see [`answer_entries`].
pub const ANSWERS: &[&str] = &[
    "reply.info.user",
    "reply.info.role",
    "reply.info.server",
    "reply.info.server_roles",
    "reply.about.pitch",
    "reply.about.protects",
    "reply.about.stats",
    "reply.ping.status",
    "reply.invite.stats",
    "reply.invite.top",
    "reply.invite.who",
    "reply.invite.who_unseen",
    "reply.invite.status",
];

/// A [`FACT_VARS`] name; a name that is not there is a mistake in a template.
fn fact_var(n: &str) -> &'static str {
    FACT_VARS
        .iter()
        .find(|v| v.0 == n)
        .map(|v| v.0)
        .unwrap_or_else(|| panic!("{{{n}}} is not in FACT_VARS"))
}

/// One D6 answer: one embed with a title, description lines, inline fields and a footer.
struct D6 {
    key: &'static str,
    name: &'static str,
    title: String,
    color: u32,
    lines: Vec<String>,
    fields: Vec<(String, String)>,
    footer: String,
    required: &'static [&'static str],
    what: &'static str,
}

fn d6(out: &mut Vec<Def>, e: D6) {
    let mut embed = json!({"title": e.title, "color": e.color});
    if !e.lines.is_empty() {
        embed["description"] = e.lines.join("\n").into();
    }
    if !e.fields.is_empty() {
        embed["fields"] = e
            .fields
            .iter()
            .map(|(n, v)| fact_field(n, v, true))
            .collect();
    }
    if !e.footer.is_empty() {
        embed["footer"] = json!({ "text": e.footer });
    }
    let template = json!({"content": "", "embeds": [embed]});
    let mut found = Vec::new();
    collect(&template, &mut found);
    let mut vars: Vec<&'static str> = found.iter().map(|n| fact_var(n)).collect();
    vars.sort_unstable();
    vars.dedup();
    assert!(e.required.iter().all(|r| vars.contains(r)), "{}", e.key);
    set(
        out,
        (e.key, e.name, "General commands"),
        template,
        (&vars, e.required, e.what),
    );
}

/// A row of a security settings panel: (code, name, value placeholder, how the value shows).
/// The placeholder is empty for a card heading (its lettered rows follow). How: `""` in code
/// style, `"raw"` as given (mentions, the dashboard link), `"toggle"` in code style with the on or
/// off sign after the name. The row's number is its own placeholder, named as the code (`{3}`
/// shows `[3]`, `{4a}` shows `[A]`): people type it to change the setting, so it is required.
pub type PanelRow = (&'static str, &'static str, &'static str, &'static str);

/// The security command panels (`t!antinuke`, `t!ancaps`, `t!anpanic`, `t!antinukerestore`,
/// `t!joinraid`): key, command, title, footer and rows, in the bot's order (a bot test keeps them
/// equal to `settings::maps` and each default equal to the panel the code built).
#[allow(clippy::type_complexity)]
pub const PANELS: &[(&str, &str, &str, &str, &[PanelRow])] = &[
    (
        "reply.panel.antinuke",
        "antinuke",
        "Anti Nuke Panel:",
        "Panic Mode: t!anp · Restore Filters: t!anr",
        &[
            ("1", "Status", "status", "toggle"),
            ("2", "Limits", "limits", "raw"),
            ("3", "Quarantine On Limit", "quarantine_on_limit", ""),
            ("4", "Prune", "", ""),
            ("4a", "Prune Detection", "prune", ""),
            ("5", "Quarantine Hold", "", ""),
            ("5a", "Quarantine Hold", "quarantine_hold", ""),
            ("5c", "Monitor Public Roles", "monitor_public_roles", ""),
            ("5d", "Vanity Protection", "vanity_protection", ""),
            ("6", "Ticket Categories", "ticket_categories", "raw"),
        ],
    ),
    (
        "reply.panel.ancaps",
        "ancaps",
        "Anti-Nuke Caps:",
        "Change one: `1 ?off a` or `6 ?set 1b`. More caps on the dashboard.",
        &[
            ("1", "Kicks & Bans", "", ""),
            ("1a", "Status", "bans.status", ""),
            ("1b", "Minute Cap", "bans.minute_cap", ""),
            ("1c", "Hour Cap", "bans.hour_cap", ""),
            ("1d", "Heat %", "bans.heat", ""),
            ("2", "Role Creations", "", ""),
            ("2a", "Status", "role_create.status", ""),
            ("2b", "Minute Cap", "role_create.minute_cap", ""),
            ("2c", "Hour Cap", "role_create.hour_cap", ""),
            ("2d", "Heat %", "role_create.heat", ""),
            ("3", "Role Deletions", "", ""),
            ("3a", "Status", "role_delete.status", ""),
            ("3b", "Minute Cap", "role_delete.minute_cap", ""),
            ("3c", "Hour Cap", "role_delete.hour_cap", ""),
            ("3d", "Heat %", "role_delete.heat", ""),
            ("4", "Channel Creations", "", ""),
            ("4a", "Status", "channel_create.status", ""),
            ("4b", "Minute Cap", "channel_create.minute_cap", ""),
            ("4c", "Hour Cap", "channel_create.hour_cap", ""),
            ("4d", "Heat %", "channel_create.heat", ""),
            ("5", "Channel Deletions", "", ""),
            ("5a", "Status", "channel_delete.status", ""),
            ("5b", "Minute Cap", "channel_delete.minute_cap", ""),
            ("5c", "Hour Cap", "channel_delete.hour_cap", ""),
            ("5d", "Heat %", "channel_delete.heat", ""),
            ("6", "Webhook Creations", "", ""),
            ("6a", "Status", "webhook_create.status", ""),
            ("6b", "Minute Cap", "webhook_create.minute_cap", ""),
            ("6c", "Hour Cap", "webhook_create.hour_cap", ""),
            ("6d", "Heat %", "webhook_create.heat", ""),
            ("7", "Webhook Deletions", "", ""),
            ("7a", "Status", "webhook_delete.status", ""),
            ("7b", "Minute Cap", "webhook_delete.minute_cap", ""),
            ("7c", "Hour Cap", "webhook_delete.hour_cap", ""),
            ("7d", "Heat %", "webhook_delete.heat", ""),
        ],
    ),
    (
        "reply.panel.anpanic",
        "anpanic",
        "Panic Mode Panel:",
        "",
        &[
            ("1", "Status", "status", ""),
            ("2", "Heat Algorithm", "heat_algorithm", ""),
            ("3", "Lockdown Server On Trigger", "lock_on_trigger", ""),
            ("4", "Unlock Server When Ending", "unlock_when_ending", ""),
            ("5", "Warned Roles", "warned_roles", "raw"),
            (
                "6",
                "Whitelisted Categories",
                "whitelisted_categories",
                "raw",
            ),
            ("7", "Lock Mod Cmds On Trigger", "lock_mod_commands", ""),
            ("8", "Trigger", "trigger", ""),
            ("9", "Panic Duration", "duration", ""),
        ],
    ),
    (
        "reply.panel.antinukerestore",
        "antinukerestore",
        "Restore System Filters:",
        "",
        &[
            ("1", "Status", "status", "toggle"),
            ("2", "Restoring Deleted Roles", "restore_roles", ""),
            ("3", "Deleting Created Roles", "delete_roles", ""),
            ("4", "Restoring Deleted Channels", "restore_channels", ""),
            ("5", "Deleting Created Channels", "delete_channels", ""),
            ("6", "Deleting Created Webhooks", "delete_webhooks", ""),
        ],
    ),
    (
        "reply.panel.joinraid",
        "joinraid",
        "Join Raid",
        "",
        &[
            ("1", "Status", "status", "toggle"),
            ("2", "Action", "action", ""),
            ("3", "Warned Roles", "warned_roles", "raw"),
            ("4", "Static Flags", "", ""),
            ("4a", "Minimum Trigger", "min_trigger", ""),
            ("4b", "Join History", "join_history", ""),
            ("4c", "Trigger Duration", "trigger_duration", ""),
            ("4d", "Accounts Type", "accounts_type", ""),
            ("5", "String Flag", "string_flag", "raw"),
            ("6", "Age Flag", "", ""),
            ("6a", "Status", "age_flag.status", ""),
            ("6b", "Minimum Age", "age_flag.min_age", ""),
            ("7", "NoPfP Flag", "no_pfp_flag", ""),
            ("8", "ID Flag", "", ""),
            ("8a", "Status", "id_flag.status", ""),
            ("8b", "Granularity", "id_flag.granularity", ""),
            ("8c", "Margin", "id_flag.margin", ""),
            ("8d", "Minimum Matches", "id_flag.min_matches", ""),
            ("9", "Spammer Flag", "spammer_flag", ""),
            ("10", "Voice Raid", "", ""),
            ("10a", "Status", "voice.status", ""),
            ("10b", "Minimum Trigger", "voice.min_trigger", ""),
            ("10c", "Join History", "voice.join_history", ""),
            ("10d", "Trigger Duration", "voice.trigger_duration", ""),
            ("10e", "Action", "voice.action", ""),
        ],
    ),
];

/// The other settings panels (`t!statics`, `t!heat`, `t!hfilters`, `t!joingate`,
/// `t!verification`, `t!misc`, `t!raiderlist`), as [`PANELS`] but in group Settings panels: a
/// Trusted Admin with Dashboard Edit may edit them. `t!hfilters` keeps its pages: the bot cuts the
/// filled entry into pages of 4 cards.
#[allow(clippy::type_complexity)]
pub const SETTINGS_PANELS: &[(&str, &str, &str, &str, &[PanelRow])] = &[
    (
        "reply.panel.statics",
        "statics",
        "Statics Panel:",
        "",
        &[
            ("2", "Quarantine Role", "quarantine_role", "raw"),
            ("5", "Main Roles", "main_roles", "raw"),
            ("6", "Logging Channel", "logging_channel", "raw"),
            ("7", "Mod-Logging Channel", "mod_logging_channel", "raw"),
            ("8", "Partnering Channels", "partnering_channels", "raw"),
            ("10", "Trusted Admins", "trusted_admins", "raw"),
            ("11", "Extra Owners", "extra_owners", "raw"),
            ("12", "Verification Channel", "verification_channel", "raw"),
        ],
    ),
    (
        "reply.panel.heat",
        "heat",
        "Heat System Panel:",
        "Advanced Settings: Heat System Filters: t!hfilters",
        &[
            ("1", "Status", "status", "toggle"),
            ("2", "Spam Filters", "spam_filters", ""),
            ("3", "Max Heat Percentage", "max_heat_percentage", ""),
            ("4", "Heat Degradation", "heat_degradation", ""),
            ("5", "Strikes CAP", "strikes_cap", ""),
            ("6", "Webhook Coverage", "webhook_coverage", ""),
            ("7", "Auto Timeouts", "", ""),
            ("7a", "Status", "auto_timeouts.status", ""),
            (
                "7b",
                "Regular Strike Duration",
                "auto_timeouts.regular_strike_duration",
                "",
            ),
            (
                "7c",
                "CAP Strike Duration",
                "auto_timeouts.cap_strike_duration",
                "",
            ),
            (
                "7d",
                "Multiplier Status",
                "auto_timeouts.multiplier_status",
                "",
            ),
            (
                "7e",
                "Multiplier Percentage",
                "auto_timeouts.multiplier_percentage",
                "",
            ),
            ("8", "Reset Heat On Timeout", "reset_heat_on_timeout", ""),
            ("9", "Heat Panic Mode", "", ""),
            ("9a", "Status", "heat_panic_mode.status", ""),
            ("9b", "Trigger", "heat_panic_mode.trigger", ""),
            ("9c", "Panic Duration", "heat_panic_mode.panic_duration", ""),
            ("10", "Auto Server Lockdown", "", ""),
            ("10a", "Status", "auto_server_lockdown.status", ""),
            ("10b", "Mentions", "auto_server_lockdown.mentions", ""),
            ("10c", "Under", "auto_server_lockdown.under", ""),
            ("11", "Auto Slowmode", "", ""),
            ("11a", "Status", "auto_slowmode.status", ""),
            ("11b", "Trigger", "auto_slowmode.trigger", ""),
            ("11c", "Slowmode", "auto_slowmode.slowmode", ""),
            (
                "11d",
                "Minimum Time On",
                "auto_slowmode.minimum_time_on",
                "",
            ),
            ("11e", "Calm Time", "auto_slowmode.calm_time", ""),
        ],
    ),
    (
        "reply.panel.hfilters",
        "hfilters",
        "Heat System Filters Panel:",
        "",
        &[
            ("1", "Normal Message Heat", "", ""),
            ("1a", "Status", "normal_message_heat.status", ""),
            ("1b", "Action", "normal_message_heat.action", ""),
            ("1c", "Heat", "normal_message_heat.heat", ""),
            ("2", "Similar Message Repetition Heat", "", ""),
            ("2a", "Status", "similar_message_repetition_heat.status", ""),
            ("2b", "Action", "similar_message_repetition_heat.action", ""),
            ("2c", "Heat", "similar_message_repetition_heat.heat", ""),
            ("3", "Mentions Heat", "", ""),
            ("3a", "Status", "mentions_heat.status", ""),
            ("3b", "Action", "mentions_heat.action", ""),
            ("3c", "Heat", "mentions_heat.heat", ""),
            ("4", "Attachments Heat", "", ""),
            ("4a", "Status", "attachments_heat.status", ""),
            ("4b", "Action", "attachments_heat.action", ""),
            ("4c", "Heat", "attachments_heat.heat", ""),
            ("5", "Advertisement Heat", "", ""),
            ("5a", "Status", "advertisement_heat.status", ""),
            ("5b", "Action", "advertisement_heat.action", ""),
            ("5c", "Heat", "advertisement_heat.heat", ""),
            ("6", "Malicious Websites Heat", "", ""),
            ("6a", "Status", "malicious_websites_heat.status", ""),
            ("6b", "Action", "malicious_websites_heat.action", ""),
            ("6c", "Heat", "malicious_websites_heat.heat", ""),
            ("6d", "Anti-Scam", "malicious_websites_heat.anti_scam", ""),
            ("7", "Emojis Heat", "", ""),
            ("7a", "Status", "emojis_heat.status", ""),
            ("7b", "Action", "emojis_heat.action", ""),
            ("7c", "Heat", "emojis_heat.heat", ""),
            ("8", "Message Characters Heat", "", ""),
            ("8a", "Status", "message_characters_heat.status", ""),
            ("8b", "Action", "message_characters_heat.action", ""),
            ("8c", "LC Heat", "message_characters_heat.lc_heat", ""),
            ("8d", "UC Heat", "message_characters_heat.uc_heat", ""),
            ("9", "New Line Heat", "", ""),
            ("9a", "Status", "new_line_heat.status", ""),
            ("9b", "Action", "new_line_heat.action", ""),
            ("9c", "Heat", "new_line_heat.heat", ""),
            ("10", "Lowercase Characters Heat", "", ""),
            ("10a", "Status", "lowercase_characters_heat.status", ""),
            ("10b", "Action", "lowercase_characters_heat.action", ""),
            ("10c", "Heat", "lowercase_characters_heat.heat", ""),
            ("11", "Inactive Channel Heat", "", ""),
            ("11a", "Status", "inactive_channel_heat.status", ""),
            ("11b", "Action", "inactive_channel_heat.action", ""),
            ("11c", "Heat", "inactive_channel_heat.heat", ""),
            ("12", "NSFW Websites Heat", "", ""),
            ("12a", "Status", "nsfw_websites_heat.status", ""),
            ("12b", "Action", "nsfw_websites_heat.action", ""),
            ("12c", "Heat", "nsfw_websites_heat.heat", ""),
            ("12d", "NSFW Images", "nsfw_websites_heat.nsfw_images", ""),
            (
                "12e",
                "NSFW Images Action",
                "nsfw_websites_heat.nsfw_images_action",
                "",
            ),
            (
                "12f",
                "NSFW Image Score",
                "nsfw_websites_heat.nsfw_image_score",
                "",
            ),
            ("13", "Words Blacklist Heat", "", ""),
            ("13a", "Status", "words_blacklist_heat.status", ""),
            ("13b", "Action", "words_blacklist_heat.action", ""),
            ("13c", "Heat", "words_blacklist_heat.heat", ""),
            ("14", "Suspicious Account Heat", "", ""),
            ("14a", "Status", "suspicious_account_heat.status", ""),
            ("14b", "Action", "suspicious_account_heat.action", ""),
            ("14c", "Heat", "suspicious_account_heat.heat", ""),
        ],
    ),
    (
        "reply.panel.joingate",
        "joingate",
        "JoinGate Panel:",
        "Username words: Dashboard",
        &[
            ("1", "General", "", ""),
            ("1a", "Status", "general.status", ""),
            ("1b", "DM Members", "general.dm_members", ""),
            ("1c", "Exclude Owner Bots", "general.exclude_owner_bots", ""),
            ("2", "No PfP Filter", "", ""),
            ("2a", "Status", "no_pfp_filter.status", ""),
            ("2b", "Action", "no_pfp_filter.action", ""),
            ("3", "Account Age Filter", "", ""),
            ("3a", "Status", "account_age_filter.status", ""),
            ("3b", "Action", "account_age_filter.action", ""),
            ("3c", "Minimum Age", "account_age_filter.minimum_age", ""),
            (
                "3d",
                "Expose Minimum Age",
                "account_age_filter.expose_minimum_age",
                "",
            ),
            ("4", "Bot Addition Filter", "", ""),
            ("4a", "Status", "bot_addition_filter.status", ""),
            ("4b", "Action", "bot_addition_filter.action", ""),
            ("5", "Advertising Names", "", ""),
            ("5a", "Status", "advertising_names.status", ""),
            ("5b", "Action", "advertising_names.action", ""),
            ("6", "Unverified Bots", "", ""),
            ("6a", "Status", "unverified_bots.status", ""),
            ("6b", "Action", "unverified_bots.action", ""),
            ("7", "Suspicious Accounts", "", ""),
            ("7a", "Status", "suspicious_accounts.status", ""),
            ("7b", "Action", "suspicious_accounts.action", ""),
            ("8", "Username Filter", "", ""),
            ("8a", "Status", "username_filter.status", ""),
            ("8b", "Action", "username_filter.action", ""),
            ("9", "Spam Flag Filter", "", ""),
            ("9a", "Status", "spam_flag_filter.status", ""),
            ("9b", "Action", "spam_flag_filter.action", ""),
        ],
    ),
    (
        "reply.panel.verification",
        "verification",
        "Verification Panel:",
        "Captcha Customization: Dashboard",
        &[
            ("1", "Status", "status", "toggle"),
            ("2", "Action", "action", ""),
            ("3", "Target", "target", ""),
            ("4", "Verified Roles", "verified_roles", "raw"),
            ("5", "Duration", "duration", ""),
            ("6", "Mode", "mode", ""),
            ("12", "Regeneration", "regeneration", ""),
            ("12a", "Regeneration Status", ".regeneration_status", ""),
            ("13", "Notes", "notes", ""),
            ("14", "Audit Log Reason", "audit_log_reason", ""),
            ("15", "Block VPN", "block_vpn", ""),
            ("16", "Block Hosting Networks", "block_hosting_networks", ""),
        ],
    ),
    (
        "reply.panel.misc",
        "misc",
        "Miscellaneous Panel:",
        "",
        &[
            ("1", "Prefix", "prefix", ""),
            ("2", "Moderation", "", ""),
            ("2a", "Confirm Actions", "moderation.confirm_actions", ""),
            (
                "2b",
                "Days Purged On Bans",
                "moderation.days_purged_on_bans",
                "",
            ),
            (
                "2c",
                "Default Timeout Duration",
                "moderation.default_timeout_duration",
                "",
            ),
            ("2d", "Anonymous Mod", "moderation.anonymous_mod", ""),
            ("2e", "DM Targets", "moderation.dm_targets", ""),
            (
                "2f",
                "Display Reason On Auto Timeout",
                "moderation.display_reason_on_auto_timeout",
                "",
            ),
            (
                "2g",
                "Purge On Auto Timeout",
                "moderation.purge_on_auto_timeout",
                "",
            ),
            (
                "2h",
                "Auto Delete Mod Cmd",
                "moderation.auto_delete_mod_cmd",
                "",
            ),
            (
                "2i",
                "Remove Main Roles On TO",
                "moderation.remove_main_roles_on_to",
                "",
            ),
            ("3", "Warn Settings", "", ""),
            ("3a", "Status", "warn_settings.status", ""),
            ("3b", "Warn Action", "warn_settings.warn_action", ""),
            ("3c", "Max Warns", "warn_settings.max_warns", ""),
            ("4", "Compact Outputs", "compact_outputs", ""),
            ("5", "Informing Messages", "informing_messages", ""),
            (
                "6",
                "Enforce Quarantine Perms",
                "enforce_quarantine_perms",
                "",
            ),
            (
                "7",
                "Ignore Permit 1 Commands",
                "ignore_permit_1_commands",
                "",
            ),
            ("8", "Enforce Permits", "enforce_permits", ""),
            ("9", "Ban Appeal Message", "ban_appeal_message", ""),
            ("10", "Sticky Roles", "sticky_roles", ""),
            ("11", "Never Sticky Roles", "never_sticky_roles", "raw"),
            ("12", "Auto Dehoist", "auto_dehoist", ""),
        ],
    ),
    (
        "reply.panel.raiderlist",
        "raiderlist",
        "Raider List Panel:",
        "Change the Raider List on the dashboard · Look someone up: t!raider check @user",
        &[
            ("1", "Status", "status", "toggle"),
            ("2", "Action", "action", ""),
            ("3", "Categories", "categories", "raw"),
            (
                "4",
                "Alert For Members Already Here",
                "alert_for_members_already_here",
                "",
            ),
            (
                "5",
                "Lift When Removed From The List",
                "lift_when_removed_from_the_list",
                "",
            ),
            ("6", "Do Not Send Detections", "do_not_send_detections", ""),
        ],
    ),
];

/// The group of the settings panels that are not security texts.
const SETTINGS: &str = "Settings panels";

/// The whitelist command's own answers (the menu's target, the saved result), entries of facts.
pub const WHITELIST_ANSWERS: &[&str] = &["reply.whitelist.prompt", "reply.whitelist.saved"];

/// The panel's colour (Discord's dark embed grey).
const GREY: u32 = 0x2B2D31;

fn panel(key: &str) -> Option<&'static [PanelRow]> {
    PANELS
        .iter()
        .chain(SETTINGS_PANELS)
        .find(|p| p.0 == key)
        .map(|p| p.4)
}

/// The settings command answers: each panel, the whitelist answers and the reply after a setting
/// changed are entries of facts, every value and row number required, and no `{text}` (a frame
/// around it would skip every fact rule). The security ones are in group Security (Owner and
/// Extra Owners, 2FA, audited). Each default reads exactly as the code built it
/// (`crates/bot/src/panel_texts_tests.rs`).
fn security_answers(out: &mut Vec<Def>) {
    use crate::embed::ARROW;
    use crate::emoji::{MODERATOR, TARGET};
    for (p, group) in PANELS
        .iter()
        .map(|p| (p, SECURITY))
        .chain(SETTINGS_PANELS.iter().map(|p| (p, SETTINGS)))
    {
        let (key, word, title, footer, rows) = p;
        let mut fields: Vec<(String, String)> = Vec::new();
        let (mut vars, mut required): (Vec<&'static str>, Vec<&'static str>) = (vec![], vec![]);
        for (code, name, var, how) in rows.iter() {
            vars.push(code);
            required.push(code);
            if !var.is_empty() {
                vars.push(var);
                required.push(var);
            }
            let val = match *how {
                "raw" => format!("{{{var}}}"),
                _ => format!("*`{{{var}}}`*"),
            };
            if code
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .is_empty()
            {
                let lines = if var.is_empty() {
                    String::new()
                } else {
                    format!("{ARROW} {val}")
                };
                let toggle = if *how == "toggle" { "{toggle}" } else { "" };
                fields.push((format!("{{{code}}} {name}:{toggle}"), lines));
            } else if let Some((_, lines)) = fields.last_mut() {
                if !lines.is_empty() {
                    lines.push('\n');
                }
                lines.push_str(&format!("{ARROW} {{{code}}} {name}: {val}"));
            }
        }
        // The on or off sign states the status too: an edit must not leave a fixed one.
        if rows.iter().any(|r| r.3 == "toggle") {
            vars.push("toggle");
            required.push("toggle");
        }
        let mut e = json!({"title": title, "color": GREY, "fields": fields
            .iter()
            .map(|(n, v)| json!({"name": n, "value": v, "inline": true}))
            .collect::<Vec<_>>()});
        // The footer names commands with the default prefix; the bot puts the server's own in
        // after filling (`settings::with_prefix`). No `{prefix}`: a server's prefix such as `||`
        // could wrap the facts in a spoiler the save check never sees.
        if !footer.is_empty() {
            e["footer"] = json!({ "text": footer });
        }
        set(
            out,
            (key, &format!("Answer: {word} panel"), group),
            json!({"content": "", "embeds": [e]}),
            (
                &vars,
                &required,
                "the settings and the numbers people type to change them",
            ),
        );
    }
    let wl = "📋 Interactive Whitelist";
    set(
        out,
        ("reply.whitelist.prompt", "Answer: whitelist menu", SECURITY),
        json!({"content": "", "embeds": [{"title": wl, "color": ORANGE, "description": "Target: {target}"}]}),
        (&["target"], &["target"], "what is being whitelisted"),
    );
    set(
        out,
        ("reply.whitelist.saved", "Answer: whitelist saved", SECURITY),
        json!({"content": "", "embeds": [{"title": wl, "color": GREEN, "description": "{lists}", "fields": [
            {"name": format!("{MODERATOR} Executor"), "value": "{moderator}", "inline": true},
            {"name": format!("{TARGET} Target"), "value": "{target}", "inline": true},
        ]}]}),
        (
            &["lists", "moderator", "target"],
            &["lists", "moderator", "target"],
            "which whitelists the target is on, who changed it and the target",
        ),
    );
    // The reply after a setting changed. A security panel's setting (join raid) and the Trusted
    // Admins list answer in the Security copy, so only the Owner and Extra Owners can reword it.
    for (key, group) in CHANGED.iter().zip([SETTINGS, SECURITY]) {
        set(
            out,
            (key, "Answer: setting changed", group),
            json!({"content": format!("{} {{row}} {{setting}}: {{value}}", crate::emoji::YES), "embeds": []}),
            (
                &["row", "setting", "value"],
                &["row", "value"],
                "the number of the setting that changed and its new value",
            ),
        );
    }
}

/// The reply after a setting changed: the settings panels' copy, then the security one.
pub const CHANGED: &[&str] = &["reply.settings.changed", "reply.settings.changed_security"];

/// The settings commands' error replies: (entry, editor name, variables, key variables,
/// default). Each has a Security copy (`<entry>_security`) for a setting of a security panel or
/// one only the Owner and Extra Owners may change, as [`CHANGED`]. What went wrong (the row, the
/// refused value, the limits) is a key variable, so an edit always says it. Each default reads
/// exactly as the code wrote it (`crates/bot/src/panel_texts_tests.rs`).
#[allow(clippy::type_complexity)]
pub const SETTINGS_ERRORS: &[(&str, &str, &[&str], &[&str], &str)] = &[
    (
        "reply.settings.dashboard_only",
        "only on the dashboard",
        &["row", "setting"],
        &["row"],
        "{row} {setting} can only be changed on the dashboard",
    ),
    (
        "reply.settings.sub_settings",
        "has sub-settings",
        &["row", "setting", "number"],
        &["row", "number"],
        "{row} {setting} has sub-settings, use {number}a, {number}b...",
    ),
    (
        "reply.settings.is_switch",
        "is a switch",
        &["row", "setting", "number"],
        &["row", "number"],
        "{row} {setting} is a switch: use `{number} ?on` or `{number} ?off`",
    ),
    (
        "reply.settings.not_switch",
        "is not a switch",
        &["row", "setting", "number"],
        &["row", "number"],
        "{row} {setting} is not a switch: use `VALUE ?set {number}`",
    ),
    (
        "reply.settings.not_list",
        "is not a list",
        &["row", "setting", "number"],
        &["row", "number"],
        "{row} {setting} is not a list: use `VALUE ?set {number}`",
    ),
    (
        "reply.settings.bad_value",
        "not a valid value",
        &["typed", "row", "setting"],
        &["typed", "row"],
        "`{typed}` is not a valid value for {row} {setting}",
    ),
    (
        "reply.settings.bad_whole",
        "not a whole number",
        &["typed", "row", "setting"],
        &["typed", "row"],
        "`{typed}` is not a valid value for {row} {setting}. Give a whole number.",
    ),
    (
        "reply.settings.bad_duration",
        "not a duration",
        &["typed", "row", "setting"],
        &["typed", "row"],
        "`{typed}` is not a valid value for {row} {setting}. Give a number with a unit, like `15d`, `12h` or `30m`.",
    ),
    (
        "reply.settings.bad_age",
        "not an age",
        &["typed", "row", "setting"],
        &["typed", "row"],
        "`{typed}` is not a valid value for {row} {setting}. Give days, or a number with a unit, like `12h` or `30m`.",
    ),
    (
        "reply.settings.out_of_range",
        "out of range",
        &["row", "setting", "min", "max"],
        &["row", "min", "max"],
        "{row} {setting} must be between {min} and {max}",
    ),
    (
        "reply.settings.too_long",
        "duration too long",
        &["row", "setting"],
        &["row"],
        "{row} {setting}: A duration can be at most 365 days (52w).",
    ),
    (
        "reply.settings.age_range",
        "age out of range",
        &["row", "setting"],
        &["row"],
        "{row} {setting} ranges from 5 seconds to 100 days",
    ),
    (
        "reply.settings.prefix_rule",
        "prefix not allowed",
        &["row", "setting"],
        &["row"],
        "{row} {setting} must be 1-32 characters with no spaces",
    ),
    (
        "reply.settings.one_of",
        "takes one of",
        &["row", "setting", "options"],
        &["row", "options"],
        "{row} {setting} takes one of: {options}",
    ),
    (
        "reply.settings.exactly_one",
        "takes exactly one",
        &["row", "setting"],
        &["row"],
        "{row} {setting} takes exactly one",
    ),
    (
        "reply.settings.not_a_role",
        "not a role of this server",
        &["typed"],
        &["typed"],
        "`{typed}` is not a role of this server",
    ),
    (
        "reply.settings.not_a_channel",
        "not a channel of this server",
        &["typed"],
        &["typed"],
        "`{typed}` is not a channel of this server",
    ),
    (
        "reply.settings.nothing_found",
        "nothing found",
        &["typed"],
        &["typed"],
        "Nothing found for `{typed}`",
    ),
    (
        "reply.settings.ambiguous",
        "more than one match",
        &["typed"],
        &["typed"],
        "`{typed}` is ambiguous, use a mention or id",
    ),
    (
        "reply.settings.no_such_row",
        "no such setting",
        &["typed", "max"],
        &["typed", "max"],
        "`{typed}` is not an option. Options available are [1 to {max}].",
    ),
    (
        "reply.settings.card_options",
        "no such sub-setting",
        &["number", "min", "max"],
        &["number", "min", "max"],
        "Card {number} has options {min} to {max}",
    ),
    (
        "reply.settings.not_saved",
        "not saved",
        &["reason"],
        &["reason"],
        "Not saved: {reason}",
    ),
];

/// The settings refusals that are always security texts: the 2FA replies, the main roles
/// and the Trusted Admins refusals. Same columns as [`SETTINGS_ERRORS`].
#[allow(clippy::type_complexity)]
pub const SECURITY_ERRORS: &[(&str, &str, &[&str], &[&str], &str)] = &[
    (
        "reply.settings.needs_2fa_dashboard",
        "needs 2FA, change it on the dashboard",
        &["dashboard"],
        &["dashboard"],
        "This setting needs 2FA, so change it on the {dashboard}",
    ),
    (
        "reply.settings.needs_2fa",
        "needs 2FA, confirm it first",
        &["dashboard"],
        &["dashboard"],
        "This change needs 2FA for this server. Open this server on the {dashboard}, choose Confirm 2FA in the account menu, then try again within 10 minutes.",
    ),
    (
        "reply.settings.main_roles_owners",
        "main roles are for the owners",
        &[],
        &[],
        "Anti-nuke monitors the main roles, so only the owner and Extra Owners can change them",
    ),
    (
        "reply.settings.not_main_role",
        "cannot be a main role",
        &["target", "reason"],
        &["target", "reason"],
        "{target} cannot be a main role: {reason}",
    ),
    (
        "reply.settings.ta_usage",
        "Trusted Admins usage",
        &[],
        &[],
        "[10] Trusted Admins: use `USER ?add 10` or `USER ?remove 10`",
    ),
    (
        "reply.settings.ta_owners",
        "Trusted Admins are for the owners",
        &[],
        &[],
        "Only the owner and Extra Owners can change Trusted Admins",
    ),
    (
        "reply.settings.ta_no_user",
        "Trusted Admins: no user given",
        &[],
        &[],
        "Give at least one user mention or ID",
    ),
    (
        "reply.settings.bot_trusted_admin",
        "a bot cannot be a Trusted Admin",
        &["target"],
        &["target"],
        "{target} is a bot, and only users can be a Trusted Admin. Add the bot as a member of a permit instead: it gets exactly the whitelist flags you pick.",
    ),
    (
        "reply.whitelist.bot_quarantine",
        "a bot cannot be on the quarantine whitelist",
        &["target"],
        &["target"],
        "{target} is a bot, and only users can be on the quarantine whitelist. Add the bot as a member of a permit instead: it gets exactly the whitelist flags you pick.",
    ),
    (
        "reply.settings.bot_check_failed",
        "could not check for a bot",
        &["target"],
        &["target"],
        "Could not check that {target} is a user and not a bot, so nothing was changed. Try again.",
    ),
    (
        "reply.backups.owners_load",
        "backups: load and delete are for the owners",
        &[],
        &[],
        "Only the owner and Extra Owners can load or delete backups",
    ),
    (
        "reply.backups.owners_create",
        "backups: create is for the owners",
        &[],
        &[],
        "Only the owner and Extra Owners can create backups",
    ),
    (
        "reply.backups.locked",
        "backups: not while locked or in panic",
        &[],
        &[],
        "Cannot delete backups while the server is locked or in panic mode. End it first.",
    ),
    (
        "reply.backups.locked_load",
        "backups: no load while locked or in panic",
        &[],
        &[],
        "Cannot load a backup while the server is locked or in panic mode. End it first.",
    ),
    (
        "reply.revert.owners_only",
        "revert: owners' actions are for the owners",
        &["target"],
        &["target"],
        "Cannot revert {target}: only an Owner or Extra Owner can undo what an Owner, Extra Owner, Trusted Admin or Brim did",
    ),
    (
        "reply.revert.include_owners",
        "revert: target is an owner or Brim",
        &["target"],
        &["target"],
        "{target} is an Owner, Extra Owner, Trusted Admin or Brim. Nothing was reverted: run it again with `include_owners: True` to undo their actions too.",
    ),
];

/// The Security copy of a [`SETTINGS_ERRORS`] entry.
pub fn security_copy(key: &str) -> String {
    format!("{key}_security")
}

/// Whether `key` is one of the settings error replies or its Security copy.
fn settings_error(key: &str) -> bool {
    let base = key.strip_suffix("_security").unwrap_or(key);
    SETTINGS_ERRORS.iter().any(|e| e.0 == base) || SECURITY_ERRORS.iter().any(|e| e.0 == key)
}

/// The settings error replies, entries of facts with no `{text}`.
fn settings_errors(out: &mut Vec<Def>) {
    let copies = SETTINGS_ERRORS
        .iter()
        .flat_map(|e| {
            [
                (e.0.to_string(), e, SETTINGS),
                (security_copy(e.0), e, SECURITY),
            ]
        })
        .chain(
            SECURITY_ERRORS
                .iter()
                .map(|e| (e.0.to_string(), e, SECURITY)),
        );
    for (key, (_, name, vars, required, text), group) in copies {
        set(
            out,
            (&key, &format!("Reply: {name}"), group),
            json!({"content": text, "embeds": []}),
            (
                vars,
                required,
                if required.is_empty() {
                    ""
                } else {
                    "what went wrong"
                },
            ),
        );
    }
}

/// The D6 entries: the answers of `info`, `roleinfo`, `serverinfo`, `about`, `ping` and
/// `invites`. A line whose placeholders are all empty goes, so one entry holds every variant of
/// its answer (a user not in the server, a member, staff or not). Each default reads exactly as
/// the code built it before (`crates/bot/src/answer_texts_tests.rs` compares them).
fn answer_entries(out: &mut Vec<Def>) {
    use crate::emoji::{
        ANTI_NUKE, ARROW, CPU, HEAT, JOIN_GATE, JOIN_RAID, LATENCY, ONLINE, QUARANTINED, RAM,
        RESOURCES, SERVERS, SIZE, UPTIME, VERIFICATION,
    };
    let l = |label: &str, value: &str| fact_line(label, value);
    let f = |name: &str, value: &str| (name.to_string(), value.to_string());
    let none = Vec::new;
    d6(
        out,
        D6 {
            key: "reply.info.user",
            name: "Answer: member info",
            title: "{member.username}'s Info:".into(),
            color: BLUE,
            lines: vec![
                l("User", "{member}"),
                l("Created", "{member.created_at}"),
                l("In Server", "{in_server}"),
                l("Left While Timed Out", "{left_timed_out}"),
                l("Left While Quarantined", "{left_quarantined}"),
                l("Banned", "{banned}"),
                l("Bot", "{is_bot}"),
                l("Joined", "{member.joined_at}"),
                l("Color", "`{color}`"),
                l("Badges", "{badges}"),
            ],
            fields: vec![
                f("Suspicious Status:", "{suspicious}"),
                f("Whitelist:", "{whitelist}"),
                f("Warn Points:", "{warn_points}"),
                f("Active Strikes:", "{strikes}"),
                f("Current Heat:", "{heat}"),
                f("Dangerous Permissions:", "{dangerous}"),
                f("Brim Permissions:", "{permits}"),
                f("Account Accessories:", "{accessories}"),
                f("Roles:", "{member_roles}"),
                f("Webhooks:", "{webhooks}"),
            ],
            footer: String::new(),
            required: &["member"],
            what: "the user and what Brim knows of them",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.info.role",
            name: "Answer: role info",
            title: "{role.name}'s Info:".into(),
            color: BLUE,
            lines: vec![
                l("Role", "{role}"),
                l("Created", "{role.created_at}"),
                l("Position", "`{position}`"),
            ],
            fields: vec![
                f("Details:", "{role_details}"),
                f("Whitelist:", "{whitelist}"),
                f("Dangerous Permissions:", "{dangerous}"),
            ],
            footer: String::new(),
            required: &["role"],
            what: "the role and its settings",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.info.server",
            name: "Answer: server info",
            title: "{server.name}'s Info:".into(),
            color: BLUE,
            lines: vec![
                l("Owner", "{owner}"),
                l("Created", "{server.created_at}"),
                l("Members", "`{members}`"),
                l("Roles", "`{role_count}`"),
                l("Channels", "`{channel_count}`"),
                l("Boosts", "`{boosts}` (tier {boost_tier})"),
            ],
            fields: none(),
            footer: "Page: 1/{pages}".into(),
            required: &["server.name"],
            what: "the server's details",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.info.server_roles",
            name: "Answer: server roles",
            title: "{server.name}'s Roles:".into(),
            color: BLUE,
            lines: vec!["{role_list}".into()],
            fields: none(),
            footer: "Page: {page_no}/{pages}".into(),
            required: &["role_list"],
            what: "the server's roles on this page",
        },
    );
    d6(out, D6 {
        key: "reply.about.pitch",
        name: "Answer: about Brim",
        title: "About Brim:".into(),
        color: BLUE,
        lines: vec![
            format!("{ARROW} Brim is a Discord security and moderation bot, written in Rust to be fast and light. It stops nukes, raids and spam on its own, undoes the damage, and gives your staff moderation commands that are quick to use."),
            format!("{ARROW} Every feature is free. There is no premium tier."),
        ],
        fields: none(),
        footer: "Page: 1/3".into(),
        required: &[],
        what: "",
    });
    d6(out, D6 {
        key: "reply.about.protects",
        name: "Answer: what Brim protects",
        title: "About Brim [What it protects]:".into(),
        color: BLUE,
        lines: vec![
            format!("{ANTI_NUKE} **Anti-nuke:** quarantines staff who mass ban, kick or delete, then reverts what they did"),
            format!("{HEAT} **Heat automod:** spam, links, scams, mentions, words and more add heat until the member is punished"),
            format!("{JOIN_GATE} **Join gate:** filters bots, young accounts and suspicious names the moment they join"),
            format!("{JOIN_RAID} **Join raid:** spots join waves and kicks or bans the raiders"),
            format!("{VERIFICATION} **Verification:** captcha, web or button checks before members get in"),
            format!("{QUARANTINED} **Lockdown, tickets, appeals, backups and logs:** all in the dashboard"),
        ],
        fields: none(),
        footer: "Page: 2/3".into(),
        required: &[],
        what: "",
    });
    d6(
        out,
        D6 {
            key: "reply.about.stats",
            name: "Answer: Brim's stats",
            title: "About Brim [Nerdy Stats]:".into(),
            color: BLUE,
            lines: vec![
                l("Version", "`{version}`"),
                l("Uptime", "`{uptime}`"),
                l("Language", "`Rust`"),
                l("Commands", "`{command_count}`"),
                l("Price", "Everything is free"),
            ],
            fields: none(),
            footer: "Page: 3/3".into(),
            required: &["version"],
            what: "Brim's version and numbers",
        },
    );
    d6(out, D6 {
        key: "reply.ping.status",
        name: "Answer: ping",
        title: "Brim Status:".into(),
        color: GREEN,
        lines: vec![],
        fields: vec![f(
            &format!("{ONLINE} Shard [0 -Current-]:"),
            &format!("{LATENCY} **Latency:** `{{latency}}` (API `{{api}}`)\n{UPTIME} **Uptime:** `{{uptime}}`\n{RESOURCES} **Resources:**\n\u{2003}{RAM} RAM: `{{ram}}`\n\u{2003}{CPU} CPU: `{{cpu}}`\n{SIZE} **Size:**\n\u{2003}{SERVERS} Servers: `{{servers}}`"),
        )],
        footer: String::new(),
        required: &["latency"],
        what: "the latency, uptime and resources",
    });
    let tracking = [l("Tracking since", "{since}"), "{notes}".into()];
    d6(
        out,
        D6 {
            key: "reply.invite.stats",
            name: "Answer: a member's invites",
            title: "Invites".into(),
            color: BLUE,
            lines: [
                l("Member", "{member}"),
                l("Invited", "`{invited}`"),
                l("Still here", "`{still_here}`"),
                l("Left", "`{left}`"),
            ]
            .into_iter()
            .chain(tracking.clone())
            .collect(),
            fields: none(),
            footer: String::new(),
            required: &["member"],
            what: "the member and their invite counts",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.invite.top",
            name: "Answer: invite leaderboard",
            title: "Invite Leaderboard".into(),
            color: BLUE,
            lines: ["{ranking}".to_string()]
                .into_iter()
                .chain(tracking.clone())
                .collect(),
            fields: none(),
            footer: "Page: {page_no}/{pages}".into(),
            required: &["ranking"],
            what: "the leaderboard lines",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.invite.who",
            name: "Answer: who invited a member",
            title: "Who Invited".into(),
            color: BLUE,
            lines: vec![
                l("Member", "{member}"),
                l("Invited by", "{inviter}"),
                l("First seen", "{first_seen}"),
                l("Method", "{method}"),
                l("Still here", "{present}"),
                l("Joins seen", "`{joins}`"),
                l("Invite code", "`{code}`"),
                "{notes}".into(),
            ],
            fields: none(),
            footer: String::new(),
            required: &["member", "inviter"],
            what: "the member and who invited them",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.invite.who_unseen",
            name: "Answer: who invited a member (no join seen)",
            title: "Who Invited".into(),
            color: BLUE,
            lines: [
                l("Member", "{member}"),
                "No join of this member was seen since tracking started".into(),
            ]
            .into_iter()
            .chain(tracking)
            .collect(),
            fields: none(),
            footer: String::new(),
            required: &["member"],
            what: "the member",
        },
    );
    d6(
        out,
        D6 {
            key: "reply.invite.status",
            name: "Answer: invite tracking status",
            title: "Invite Tracking".into(),
            color: BLUE,
            lines: vec![
                l("Tracking", "{tracking}"),
                l("Status", "{status}"),
                l("Tracking since", "{since}"),
                l("Unknown joins (7 days)", "`{unknown}`"),
                "Brim needs the Manage Server permission to read the server's invites.".into(),
            ],
            fields: none(),
            footer: String::new(),
            required: &["status"],
            what: "whether invite tracking works",
        },
    );
}

/// The group of the security texts (plan section 6).
const SECURITY: &str = "Security";

/// Feed posts (feeds plan 4b): by default as if the source posted it, the plain link so Discord
/// shows the site's own preview, no Brim branding. `{link}` is required: it is what a feed
/// post is for. Each feed may also keep its own edited version (`feeds.message`).
fn feeds(out: &mut Vec<Def>) {
    for (key, name) in [
        ("feed.rss", "RSS feed post"),
        ("feed.youtube", "YouTube video post"),
    ] {
        let mut d = def(
            key,
            name,
            "Feeds",
            false,
            &[
                "title", "link", "author", "summary", "image", "feed", "ping",
            ],
            json!({"content": "{ping}\n{link}", "embeds": []}),
        );
        d.required = vec!["link"];
        d.what = "the link to the new post";
        out.push(d);
    }
    // GitHub and GitLab events (feeds plan 9a): by default the embed Discord's own GitHub webhook
    // posts, recorded from GitHub's sample events on 2026-11-20 (tommybot-data
    // feeds/github-look-2026-11-20.md). `{headline}` is that embed's title, built per event by
    // the dashboard (`feedhooks.rs`); the bot sets the event's own colour while the default is used.
    for (event, name, color) in GITHUB_EVENTS {
        let mut d = def(
            &format!("feed.github.{event}"),
            name,
            "Feeds",
            false,
            GITHUB_VARS,
            json!({"content": "{ping}", "embeds": [{
                "title": "{headline}", "url": "{link}", "description": "{summary}", "color": color,
                "author": {"name": "{author}", "url": "{author_link}", "icon_url": "{author_icon}"},
            }]}),
        );
        d.required = vec!["link"];
        d.what = "the link to the event";
        out.push(d);
    }
}

/// Each GitHub event with an entry (GitLab events use the same ones), its name and the colour of
/// its usual look (the bot uses the event's own colour while the default is sent).
pub const GITHUB_EVENTS: &[(&str, &str, u32)] = &[
    ("push", "GitHub/GitLab: commits pushed", 0x7289DA),
    ("pull_request", "GitHub/GitLab: pull request", 0x009800),
    ("issues", "GitHub/GitLab: issue", 0xEB6420),
    ("release", "GitHub/GitLab: release published", 0),
    ("create", "GitHub/GitLab: branch or tag made", 0),
    ("delete", "GitHub/GitLab: branch or tag deleted", 0),
    (
        "issue_comment",
        "GitHub/GitLab: comment on an issue or pull request",
        0xE68D60,
    ),
    ("commit_comment", "GitHub/GitLab: comment on a commit", 0),
    (
        "pull_request_review",
        "GitHub: pull request review",
        0x009800,
    ),
    (
        "pull_request_review_comment",
        "GitHub: pull request review comment",
        0xBFE5BF,
    ),
    ("watch", "GitHub: star added", 0),
    ("fork", "GitHub: fork made", 0),
    ("check_suite", "GitHub/GitLab: checks failed", 0xFC2929),
    ("discussion", "GitHub: discussion", 0xE68D60),
    ("discussion_comment", "GitHub: discussion comment", 0xE68D60),
    ("member", "GitHub: collaborator added", 0),
    ("public", "GitHub: repository made public", 0),
];

const GITHUB_VARS: &[&str] = &[
    "headline",
    "link",
    "summary",
    "author",
    "author_link",
    "author_icon",
    "repo",
    "repo_name",
    "branch",
    "action",
    "number",
    "title",
    "feed",
    "ping",
];

/// Errors, usage lines and permit refusals of commands. `{text}` in the usage line is the
/// command's own usage; in the wrong use reply it is what was wrong with the command as typed (the
/// parsers' many cases: a missing value, an unknown case type, a message the menu cannot find).
fn error_replies(out: &mut Vec<Def>) {
    let g = "Errors and usage";
    let rows: &[(&str, &str, &[&'static str], &str)] = &[
        (
            "reply.error.usage",
            "usage line",
            &["text"],
            "Usage: {text}",
        ),
        (
            "reply.error.wrong_use",
            "command typed wrong",
            &["text"],
            "{text}",
        ),
        (
            "reply.error.unknown_command",
            "unknown command",
            &[],
            "Unknown command",
        ),
        (
            "reply.error.no_permit",
            "missing permit",
            &["permit"],
            "You lack the {permit} permit.",
        ),
        (
            "reply.error.no_permit_command",
            "missing permit for the command",
            &[],
            "You lack the permit for this command",
        ),
        (
            "reply.error.slow_down",
            "slow down",
            &[],
            "Slow down a little, then try again",
        ),
        // Key kept from the tags package, so an edited version made then still applies.
        (
            "reply.tag.slash_off",
            "slash commands are off",
            &[],
            "Slash commands are disabled on this server",
        ),
        (
            "reply.settings.no_access",
            "settings need Dashboard Access",
            &[],
            "You need a permit with the Dashboard Access perm to see settings",
        ),
        (
            "reply.settings.not_your_panel",
            "panel button of someone else",
            &[],
            "Only the person who opened this panel can use it.",
        ),
        (
            "reply.settings.may_not_change",
            "may not change this setting",
            &[],
            "You may not change this setting",
        ),
        (
            "reply.invite.no_permit",
            "needs the View Invites permit",
            &[],
            "You need the View Invites permit for this",
        ),
        (
            "reply.appeal.no_permit",
            "needs the Review Appeals permit",
            &[],
            "You need the Review Appeals permit (and the ban or timeout permit to approve)",
        ),
        (
            "reply.nsfw.no_permit",
            "needs the purge permit",
            &[],
            "You need the purge permit (or timeout and up) in this server.",
        ),
        (
            "reply.raider.no_permit_allow",
            "Raider List: needs a ban, kick or quarantine permit",
            &[],
            "You need the ban, kick or quarantine permit in this server to do this.",
        ),
        (
            "reply.raider.no_permit",
            "Raider List: needs the permit of the button",
            &["permit"],
            "You need the {permit} permit in this server to do this.",
        ),
        (
            "reply.raider.owner_only",
            "Raider List: report is for Owner and EO",
            &[],
            "Only the owner or an Extra Owner can report to the Raider List.",
        ),
        (
            "reply.raider.usage",
            "Raider List: usage",
            &[],
            "Use `raider check @user` or `raider report @user <nuke|raid|scam|spam> <reason>`.",
        ),
    ];
    for (key, name, vars, text) in rows {
        let mut d = say(key, &format!("Reply: {name}"), g, vars, text);
        // Message text, so the first embed check of `build` does not see it (like a feed post).
        if vars.contains(&"text") {
            d.required = vec!["text"];
            d.what = what(key);
        }
        out.push(d);
    }
}

/// Lockdown notices and short replies (`reply.lock.*`). `{text}` in the two notices is the list
/// of lock updates the bot builds (none when nobody sent one).
fn lock_replies(out: &mut Vec<Def>) {
    use crate::emoji::{ANNOUNCE, ARROW, LOCKDOWN};
    let g = "Lockdown";
    let n = def(
        "reply.lock.updates_notice",
        "Reply: lockdown updates notice",
        g,
        false,
        &["text"],
        embed("Lockdown Updates", crate::embed::ALERT, &["{text}".into()]),
    );
    out.push(n);
    let n = def(
        "reply.lock.locked_notice",
        "Reply: server is locked notice",
        g,
        false,
        &["text"],
        embed(
            &format!("{LOCKDOWN} The server is locked"),
            RED,
            &[
                format!("{ANNOUNCE} Staff have locked this server down."),
                format!("{ARROW} You cannot see or talk in the channels until the lockdown ends."),
                format!("{ARROW} **Hang tight while staff sort everything out.**"),
                "{text}".into(),
            ],
        ),
    );
    out.push(n);
    for (k, name, vars, text) in [
        (
            "update_no_permit",
            "no permit to post lock updates",
            &[][..],
            "Cannot post lock updates: you lack the lock updates permit",
        ),
        (
            "nothing_locked",
            "nothing is locked",
            &[],
            "Nothing is locked",
        ),
        (
            "update_added",
            "update added to the lock notice",
            &[],
            "Lock update added to the lock notice",
        ),
        ("update_posted", "update posted", &[], "Lock update posted"),
        (
            "no_permit_lock",
            "no permit to lock",
            &[],
            "Cannot lock: you lack the lockdown permit",
        ),
        (
            "no_permit_unlock",
            "no permit to unlock",
            &[],
            "Cannot unlock: you lack the lockdown permit",
        ),
        (
            "unlock_needs_permit",
            "unlock needs another permit",
            &["permit"],
            "Cannot unlock: you lack the {permit} permit",
        ),
        (
            "lock_needs_permit",
            "lock needs another permit",
            &["lock", "permit"],
            "Cannot lock {lock}: you lack the {permit} permit",
        ),
        (
            "already_covered",
            "an active lock covers it",
            &["lock"],
            "Cannot lock {lock}: an active lock already covers it",
        ),
        (
            "nothing_to_unlock",
            "nothing to unlock",
            &[],
            "Nothing to unlock",
        ),
    ] {
        out.push(say(
            &format!("reply.lock.{k}"),
            &format!("Reply: {name}"),
            g,
            vars,
            text,
        ));
    }
}

/// The confirm prompt and its answers (`reply.confirm.*`), and the Undo button and menu answers
/// (`reply.undo.*`, `reply.menu.*`). `{text}` in the prompt is who or what the command acts on.
fn confirm_and_button_replies(out: &mut Vec<Def>) {
    use crate::emoji::ARROW;
    let g = "Confirm prompt";
    out.push(def(
        "reply.confirm.question",
        "Reply: confirm prompt",
        g,
        false,
        &["text", "seconds"],
        embed(
            "Do you want to proceed?",
            crate::embed::BLUE,
            &[
                format!("{ARROW} **Target:**"),
                "{text}".into(),
                "Press Yes or No. You have {seconds} seconds.".into(),
            ],
        ),
    ));
    let confirm: &[(&str, &str, &[&'static str], &str)] = &[
        ("working", "working on it", &[], "Working on it..."),
        ("purging", "purging messages", &[], "Purging messages..."),
        ("cancelled", "cancelled", &[], "Cancelled."),
        ("timed_out", "timed out", &["seconds"], "This confirmation timed out after {seconds} seconds, so nothing was done. Run the command again."),
        ("answered", "already answered", &[], "This confirmation was already answered, so the command runs only once."),
        ("not_yours", "someone else pressed", &[], "Only the moderator who ran this command can answer."),
        ("not_finished", "command did not finish", &["error"], "The command did not finish: {error}"),
        ("command_failed", "command failed", &["error"], "Command failed: {error}"),
        ("target_failed", "failed on one member", &["action", "user", "error"], "Failed to {action} {user}: {error}"),
        ("no_ticket_permit", "ticket permit gone", &[], "You no longer have a permit with Manage Tickets."),
    ];
    let g2 = "Buttons and menus";
    let buttons: &[(&str, &str, &[&'static str], &str)] = &[
        (
            "undo.wrong_case",
            "Undo: button does not fit the case",
            &[],
            "This button does not fit the case.",
        ),
        (
            "undo.not_current",
            "Undo: case no longer holds the member",
            &["case", "user", "cause"],
            "Case #{case} is not what holds {user} now: {cause}. Nothing was lifted.",
        ),
        (
            "undo.no_permit",
            "Undo: presser has no permit",
            &[],
            "You need a permit to use this button.",
        ),
        (
            "undo.busy",
            "Undo: another moderator is acting",
            &[],
            "Another moderator is acting on this case right now.",
        ),
        (
            "undo.error",
            "Undo: something went wrong",
            &["error"],
            "Something went wrong: {error}",
        ),
        (
            "menu.slow_down",
            "Menu: picked too fast",
            &[],
            "Slow down a little, then pick again.",
        ),
        (
            "menu.unused",
            "Menu: no longer in use",
            &[],
            "This menu is no longer in use.",
        ),
    ];
    for (k, name, vars, text) in confirm {
        out.push(say(
            &format!("reply.confirm.{k}"),
            &format!("Reply: {name}"),
            g,
            vars,
            text,
        ));
    }
    for (k, name, vars, text) in buttons {
        out.push(say(
            &format!("reply.{k}"),
            &format!("Reply: {name}"),
            g2,
            vars,
            text,
        ));
    }
}

/// Report, case, note, whitelist and catch channel replies (`reply.report.*`, `reply.case.*`,
/// `reply.note.*`, `reply.whitelist.*`, `reply.catch.*`). The whitelist command is a security
/// command, so its replies are security texts like `cmd.whitelist`.
fn report_case_replies(out: &mut Vec<Def>) {
    use crate::emoji::YES;
    out.push(def(
        "reply.report.sent",
        "Reply: report sent",
        "Reports",
        false,
        &[],
        embed(
            "",
            GREEN,
            &[format!(
                "{YES} This message has been flagged and sent to the staff team for a review!"
            )],
        ),
    ));
    out.push(def(
        "reply.catch.warning",
        "Catch channel warning",
        "Catch channel",
        false,
        &["punishment"],
        embed("Catch channel", RED, &["This channel is to catch bots which automatically spam each channel with scams before they are able to post to the rest of the server.\n\n**Sending a message in this channel will {punishment}.**".into()]),
    ));
    type Reply = (
        &'static str,
        &'static str,
        &'static [&'static str],
        &'static str,
    );
    let groups: [(&str, &[Reply]); 3] = [
        (
            "Reports",
            &[
                (
                    "report.not_set_up",
                    "reports are not set up",
                    &[],
                    "Reports are not set up on this server",
                ),
                (
                    "report.bot_message",
                    "bot messages cannot be reported",
                    &[],
                    "Messages from bots and webhooks cannot be reported",
                ),
                (
                    "report.own_message",
                    "own message",
                    &[],
                    "You cannot report your own message",
                ),
                (
                    "report.blacklisted",
                    "reporter is blacklisted",
                    &[],
                    "You can no longer report messages in this server",
                ),
                (
                    "report.protected",
                    "member cannot be reported",
                    &[],
                    "This member cannot be reported",
                ),
                (
                    "report.too_many",
                    "too many reports this hour",
                    &[],
                    "You reported a lot of messages this hour, try again later",
                ),
                (
                    "report.already",
                    "already reported",
                    &[],
                    "You already reported this message",
                ),
                (
                    "report.not_found",
                    "reported message not found",
                    &[],
                    "Message not found",
                ),
                (
                    "report.no_permit",
                    "no Manage Reports permit",
                    &[],
                    "You need the Manage Reports permit",
                ),
                ("report.gone", "report not found", &[], "Report not found"),
                (
                    "report.dismissed",
                    "report dismissed",
                    &[],
                    "Report dismissed",
                ),
                (
                    "report.log_channel",
                    "message is in a log channel",
                    &[],
                    "This message is in a log channel; it cannot be deleted",
                ),
                (
                    "report.deleted",
                    "reported message deleted",
                    &[],
                    "Message deleted",
                ),
                (
                    "report.already_gone",
                    "message is already gone",
                    &[],
                    "Could not delete the message: it is already gone",
                ),
                (
                    "report.delete_failed",
                    "message could not be deleted",
                    &["error"],
                    "Could not delete the message: {error}",
                ),
                (
                    "report.unknown_action",
                    "unknown report button",
                    &[],
                    "Unknown action",
                ),
                (
                    "report.already_banned",
                    "member is already banned",
                    &[],
                    "This user is already banned",
                ),
                (
                    "report.busy",
                    "another moderator is acting",
                    &[],
                    "Another moderator is acting on this report right now.",
                ),
                (
                    "report.error",
                    "something went wrong",
                    &["error"],
                    "Something went wrong: {error}",
                ),
            ],
        ),
        (
            "Cases and notes",
            &[
                (
                    "case.no_case",
                    "case not found",
                    &["case"],
                    "No case #{case}",
                ),
                (
                    "case.no_warn_case",
                    "no active warn case",
                    &["case"],
                    "No active warn case #{case}",
                ),
                (
                    "case.none_found",
                    "no matching cases",
                    &[],
                    "No matching cases",
                ),
                (
                    "case.reason_too_long",
                    "reason too long",
                    &[],
                    "Keep the reason under 1,000 characters",
                ),
                (
                    "case.reason_updated",
                    "case reason updated",
                    &["case"],
                    "Case #{case} reason updated",
                ),
                (
                    "case.active_again",
                    "case active again",
                    &["case"],
                    "Case #{case} is active again",
                ),
                (
                    "case.closed",
                    "case closed",
                    &["case"],
                    "Case #{case} closed",
                ),
                (
                    "case.no_proof",
                    "case has no proof",
                    &["case"],
                    "Case #{case} has no proof. Add one with `proof {case} <link or text>`",
                ),
                (
                    "case.proofs_full",
                    "case has the most proofs",
                    &["case", "count"],
                    "Case #{case} already has {count} proofs",
                ),
                (
                    "case.proof_added",
                    "proof added",
                    &["case"],
                    "Proof added to case #{case}",
                ),
                (
                    "case.proof_too_long",
                    "proof too long",
                    &[],
                    "Keep the proof under 1,000 characters",
                ),
                (
                    "note.length",
                    "note too short or too long",
                    &[],
                    "A note must be 5 to 500 characters long",
                ),
                (
                    "note.not_found",
                    "note not found",
                    &["id"],
                    "No note `{id}`",
                ),
                (
                    "note.none_found",
                    "no matching notes",
                    &[],
                    "No matching notes",
                ),
                (
                    "note.unchanged",
                    "note already says that",
                    &["id"],
                    "Note `{id}` already says that",
                ),
                (
                    "note.updated",
                    "note updated",
                    &["id"],
                    "Note `{id}` updated",
                ),
                (
                    "note.removed",
                    "note removed",
                    &["id"],
                    "Removed note `{id}`",
                ),
                (
                    "note.swept",
                    "notes removed",
                    &["count"],
                    "Removed {count} notes",
                ),
                ("note.dm_sent", "notes sent by DM", &[], "Sent to your DMs."),
                (
                    "note.saved_no_dm",
                    "note saved, DM failed",
                    &["id"],
                    "Note `{id}` saved; could not DM you the card (open your DMs).",
                ),
                (
                    "note.no_dm",
                    "notes could not be sent by DM",
                    &[],
                    "Could not DM you: open your DMs to read notes.",
                ),
            ],
        ),
        (
            SECURITY,
            &[
                (
                    "whitelist.no_permit",
                    "may not change the whitelist",
                    &[],
                    "You may not change the whitelist",
                ),
                (
                    "whitelist.not_role",
                    "whitelist target is not a role here",
                    &["id"],
                    "`{id}` is not a role of this server",
                ),
                (
                    "whitelist.not_yours",
                    "whitelist menu of someone else",
                    &[],
                    "Only the staff member who typed the command can use this.",
                ),
                (
                    "whitelist.cancelled",
                    "whitelist menu cancelled",
                    &[],
                    "Cancelled.",
                ),
            ],
        ),
    ];
    for (g, list) in groups {
        for (k, name, vars, text) in list {
            out.push(say(
                &format!("reply.{k}"),
                &format!("Reply: {name}"),
                g,
                vars,
                text,
            ));
        }
    }
}

/// Member-facing ticket, verification and welcome messages (phase 3). The code adds what is not
/// text: buttons, the author (staff avatar), the time stamp, and a server footer when the copy has
/// none.
fn tickets_and_verify(out: &mut Vec<Def>) {
    use crate::emoji::{ARROW, TICKET, TICKET_BLOCK, TICKET_CLOSE, TICKET_REPLY, YES};
    let g = "Tickets";
    let mut c = def(
        "ticket.confirm",
        "Confirm the server",
        g,
        false,
        &["server", "server_id"],
        embed(
            &format!("{TICKET} Confirmation"),
            BLUE,
            &["📨 You are sending this message to `{server}` (ID: `{server_id}`).".into(), "Press **Send** to confirm, **Other server** to pick another server, or **Cancel** to stop.".into()],
        ),
    );
    c.template["embeds"][0]["footer"] = json!({"text": "Tip: you can turn confirmation messages off with the t!confirmation command."});
    out.push(c);
    out.push(frame(
        "ticket.sent",
        "Your message was sent",
        g,
        false,
        &format!("{TICKET_REPLY} Message Sent"),
        GREEN,
    ));
    out.push(frame(
        "ticket.reply",
        "Staff reply",
        g,
        false,
        &format!("{TICKET_REPLY} Message Received"),
        BLUE,
    ));
    out.push(frame(
        "ticket.greeting",
        "Greeting message frame",
        g,
        false,
        &format!("{TICKET} Welcome to Your Ticket"),
        GREEN,
    ));
    // The panel message in #open-a-ticket and in channels staff publish it to; the buttons
    // come from the panels on the Tickets tab.
    // `{e:name}` tags: the panel message fills them, like the panel descriptions it lists.
    let mut p = def(
        "ticket.panel",
        "Ticket panel message",
        g,
        false,
        &["text"],
        embed(
            "{e:ticket} Open a Ticket",
            BLUE,
            &[
                "Need a hand from the staff? Press the button below that fits. Brim opens a private channel that only you and the staff can see.".into(),
                String::new(),
                "{e:note} **Have ready:** names, message links or screenshots.".into(),
                "{e:clock} **One ticket per issue.** Staff answer as soon as they can.".into(),
                String::new(),
                "{text}".into(),
            ],
        ),
    );
    p.template["embeds"][0]["footer"] = json!({"text": "Brim Tickets • Private and secure"});
    out.push(p);
    out.push(frame(
        "ticket.closing",
        "Closing message frame",
        g,
        false,
        &format!("{TICKET_CLOSE} Closing Message"),
        BLUE,
    ));
    let mut d = def(
        "ticket.closed",
        "Ticket closed",
        g,
        false,
        &["server", "reason"],
        embed(
            &format!("{TICKET_CLOSE} Ticket Closed"),
            BLUE,
            &[
                "📪 Your ticket in `{server}` was closed.".into(),
                format!("{} {{reason}}", label_head("Reason")),
            ],
        ),
    );
    d.important = vec!["reason"];
    out.push(d);
    let mut d = plain(
        "ticket.paused",
        "Ticket creation turned off (DM)",
        g,
        &["reason"],
        &format!("{TICKET_BLOCK} Ticket Creation Disabled"),
        RED,
        "{reason}",
    );
    d.important = vec!["reason"];
    out.push(d);
    let closed: &[(&str, &str, &[&'static str], &str)] = &[
        ("done", "all tickets closed", &["total"], "All {total} open tickets are closed."),
        ("done_anon", "all tickets closed anonymously", &["total"], "All {total} open tickets are closed anonymously."),
        ("partly", "some tickets not closed", &["closed", "total", "left"], "Closed {closed} of {total} open tickets. The other {left} could not be closed; see the bot log, then try again."),
        ("partly_anon", "some tickets not closed (anonymous)", &["closed", "total", "left"], "Closed {closed} of {total} open tickets anonymously. The other {left} could not be closed; see the bot log, then try again."),
        ("kept_open", "this ticket stays open", &[], "This ticket stays open, since this answer is in it; close it with `close`."),
    ];
    for (k, name, vars, text) in closed {
        out.push(say(
            &format!("reply.closeall.{k}"),
            &format!("Reply: close all, {name}"),
            g,
            vars,
            text,
        ));
    }

    let g = "Verification";
    out.push(plain(
        "verify.passed",
        "Verification passed",
        g,
        &["server"],
        "You have been verified!",
        GREEN,
        &format!("{YES} You passed the verification. You can now access `{{server}}`!"),
    ));
    out.push(plain(
        "verify.wrong",
        "Wrong captcha answer",
        g,
        &[],
        "❌ Unsuccessful Operation!",
        RED,
        &format!("{ARROW} Wrong answer, try again before it's too late."),
    ));
    out.push(plain(
        "verify.spent",
        "Too many wrong answers",
        g,
        &[],
        "❌ Unsuccessful Operation!",
        RED,
        &format!("{ARROW} Too many wrong answers. Press **Verify** again for a new captcha."),
    ));
    out.push(def(
        "verify.help",
        "Verification help",
        g,
        false,
        &["steps"],
        embed("🧩 Verification Help", BLUE, &["{steps}".into()]),
    ));
    out.push(def("verify.kicked", "Removed for not verifying", g, false, &["server", "again"], embed("Timed out. Try again", RED, &["⏳ You did not finish the verification of `{server}` in time, so you were removed from the server.".into(), format!("{ARROW} {{again}}")])));
    out.push(say(
        "reply.verify.not_set_up",
        "Reply: verification is not set up",
        g,
        &[],
        "Verification is not set up on this server.",
    ));
    out.push(say(
        "reply.verify.already_verified",
        "Reply: already verified",
        g,
        &[],
        "You are already verified.",
    ));
    out.push(say(
        "reply.verify.press_verify",
        "Reply: verify first",
        g,
        &[],
        "This server asks you to verify first. Press Verify in the verification channel.",
    ));
    out.push(say(
        "reply.verify.set_channel",
        "Reply: set a verification channel",
        g,
        &[],
        "Set a verification channel on the dashboard first.",
    ));
    out.push(say("reply.verify.wrong_channel", "Reply: channel is in another server", g, &[], "The verification channel is not in this server. Pick a channel of this server on the dashboard."));
    out.push(say(
        "reply.verify.posted_button",
        "Reply: verify button posted",
        g,
        &["channel"],
        "Posted the verify button in {channel}",
    ));
    for (k, name, vars, red, text) in TICKET_REPLIES {
        out.push(def(
            &format!("reply.ticket.{k}"),
            &format!("Reply: {name}"),
            "Tickets",
            false,
            vars,
            embed("", if *red { RED } else { BLUE }, &[(*text).into()]),
        ));
    }
    lock_replies(out);
    error_replies(out);
    confirm_and_button_replies(out);
    report_case_replies(out);
    let g = "Tags";
    out.push(say(
        "reply.tag.no_permit",
        "Reply: no tags permit",
        g,
        &[],
        "You lack the tags permit.",
    ));
    out.push(say(
        "reply.tag.module_off",
        "Reply: Messages module is off",
        g,
        &[],
        "Not posted: the Messages module is off on this server.",
    ));
    out.push(say(
        "reply.tag.use_in_channel",
        "Reply: used outside a channel",
        g,
        &[],
        "Use /tag in a channel.",
    ));
    out.push(say(
        "reply.tag.cannot_send",
        "Reply: cannot write in this channel",
        g,
        &[],
        "Not posted: you cannot send messages in this channel, so /tag cannot post here.",
    ));
    out.push(say(
        "reply.tag.not_found",
        "Reply: no message has the tag",
        g,
        &["tag"],
        "No message has the tag “{tag}”.",
    ));
    out.push(say("reply.tag.slow_down", "Reply: slow down", g, &[], "Slow down: one tag every 10 seconds per member, the same tag in a channel once every 30 seconds, and 10 tags a minute in this server."));
    out.push(say(
        "reply.tag.no_answer",
        "Reply: dashboard did not answer",
        g,
        &[],
        "Not posted: Brim's dashboard did not answer. Try again in a minute.",
    ));
    out.push(say(
        "reply.tag.queued",
        "Reply: queued",
        g,
        &["tag"],
        "“{tag}” is queued: Brim posts it when Discord answers.",
    ));
    // Built-in tag: `/tag dangerous-permissions` in every server. `{permissions}` is Brim's own
    // list of dangerous Discord permissions, one per line, so the tag and the protection agree.
    let mut d = plain(
        "tag.dangerous_permissions",
        "Built-in tag: dangerous-permissions",
        g,
        &["permissions"],
        "Dangerous Permissions",
        ORANGE,
        "{permissions}",
    );
    d.required = vec!["permissions"];
    out.push(d);
    let g = "Verification";
    out.push(plain("verify.banned", "Banned for not verifying", g, &["server"], "Timed out. Try again", RED, "⛔ You did not finish the verification of `{server}` in time 3 times in 24 hours, so you were banned for 7 days.\nWhen the ban ends, Brim sends you a new invite here, valid for 24 hours."));
    out.push(plain(
        "verify.ban_ended",
        "Verification ban ended",
        g,
        &["server"],
        "Your ban has ended",
        GREEN,
        "✅ You can join `{server}` again and verify. The invite below works once, for 24 hours.",
    ));

    out.push(def(
        "welcome.added",
        "Thank you for adding Brim",
        "Welcome",
        false,
        &["server", "setup"],
        embed(
            "Thank you for choosing Brim!",
            GREEN,
            &[
                format!("{YES} `Brim` has been added to `{{server}}`."),
                String::new(),
                "{setup} Every feature is free.".into(),
            ],
        ),
    ));
}

/// Log groups (`LogKind` in its serde name) and the ticket logs. Each group has one entry per log
/// type ([`LOG_TYPES`], key `log.<kind>.<type>`) and one for every other log of the group (key
/// `log.<kind>`, also what a server's edit from before the per-type entries still sits in). Each is a
/// frame laid over the log the code builds: `{title}` and `{text}` are its title and lines; colour,
/// footer and pictures of a saved copy replace the code's, fields are added. No colour by default,
/// so each log keeps its own.
pub const LOGS: &[(&str, &str)] = &[
    ("message", "Message logs"),
    ("member", "Member logs"),
    ("role", "Role logs"),
    ("channel", "Channel logs"),
    ("server", "Server logs"),
    ("invite", "Invite logs"),
    ("webhook", "Webhook logs"),
    ("emoji", "Emoji logs"),
    ("sticker", "Sticker logs"),
    ("scheduled_event", "Event logs"),
    ("thread", "Thread logs"),
    ("report", "Report logs"),
    ("auto_mod", "Automod logs"),
    ("auto_mod_rule", "Discord AutoMod rule logs"),
    ("verification", "Verification logs"),
    ("join_gate", "Join Gate logs"),
    ("mod_command", "Moderation logs"),
    ("appeal", "Appeal logs"),
    ("voice", "Voice logs"),
    ("poll", "Poll logs"),
    ("user", "User logs"),
    ("ticket", "Ticket logs"),
    ("anti_nuke", "Anti-nuke logs"),
    ("panic", "Panic mode logs"),
    ("join_raid", "Join raid logs"),
];

/// The log types with an entry of their own: (log group, the title the log is sent with). The key is
/// `log.<group>.<title as a slug>`. A log whose title is not here (its title holds a name or a
/// number) is edited in its group's `log.<group>` entry. The security groups have no types: one
/// security entry each. A test keeps every title equal to one the code sends.
pub const LOG_TYPES: &[(&str, &[&str])] = &[
    (
        "message",
        &[
            "Message Edited",
            "Message Deleted",
            "Message Delete Bulk",
            "Message Pin",
            "Message Unpin",
            "Poll Deleted",
            "Message Published",
        ],
    ),
    (
        "member",
        &[
            "Member Join",
            "Member Leave",
            "Member Kick",
            "Member Prune",
            "Member Ban Add",
            "Member Ban Remove",
            "Member Update",
            "Member Role Update",
            "Member Move",
            "Member Disconnect",
            "Bot Add",
            "Member Renamed",
            "Invite Used",
            "App Remove",
            "Timeout Ended",
        ],
    ),
    ("role", &["Role Create", "Role Update", "Role Delete"]),
    (
        "channel",
        &[
            "Channel Create",
            "Channel Update",
            "Channel Delete",
            "Channel Overwrite Create",
            "Channel Overwrite Update",
            "Channel Overwrite Delete",
            "Stage Start",
            "Stage Update",
            "Stage End",
        ],
    ),
    (
        "server",
        &[
            "Guild Update",
            "Log Channel Deleted",
            "Backup Created",
            "Scheduled Action Failed",
            "Scheduled Action Dropped",
            "Unlock Incomplete",
            "App Command Permission Update",
            "Boost Level Changed",
        ],
    ),
    (
        "invite",
        &["Invite Create", "Invite Update", "Invite Delete"],
    ),
    (
        "webhook",
        &[
            "Webhook Create",
            "Webhook Update",
            "Webhook Delete",
            "Message Builder Webhook Deleted",
            "Feed Webhook Deleted",
            "Feed Webhook Moved",
            "Feed Broken",
            "Feed Items Skipped",
            "Feed Connected",
        ],
    ),
    ("emoji", &["Emoji Create", "Emoji Update", "Emoji Delete"]),
    (
        "sticker",
        &["Sticker Create", "Sticker Update", "Sticker Delete"],
    ),
    (
        "scheduled_event",
        &[
            "Guild Scheduled Event Create",
            "Guild Scheduled Event Update",
            "Guild Scheduled Event Delete",
            "Event Subscribed",
            "Event Unsubscribed",
        ],
    ),
    (
        "thread",
        &[
            "Thread Create",
            "Thread Update",
            "Thread Delete",
            "Thread Archived Automatically",
        ],
    ),
    ("report", &["Discord AutoMod Report"]),
    (
        "auto_mod",
        &[
            "Discord AutoMod Report",
            "Discord AutoMod Rule Approved",
            "Discord AutoMod Rule Not Approved",
            "Discord AutoMod Rule Changed Without A Permit",
            "Discord AutoMod Exemptions Cut",
            "Discord AutoMod Sync Failed",
            "Warn Action",
            "Automod Action Failed",
            "NSFW Image Found",
            "Auto Slowmode On",
            "Auto Slowmode Off",
            "Auto Slowmode Failed",
            "Auto Slowmode Off Failed",
            "Webhooks Deleted",
            "Feed Post Blocked",
            "Catch channel (test mode)",
            "Catch channel action failed",
        ],
    ),
    (
        "verification",
        &[
            "Verification Channel Not Saved",
            "Possible Alt Account",
            "Verification Blocked",
            "Verification",
        ],
    ),
    ("join_gate", &["Join Gate"]),
    (
        "auto_mod_rule",
        &[
            "Discord AutoMod Rule Created",
            "Discord AutoMod Rule Updated",
            "Discord AutoMod Rule Deleted",
        ],
    ),
    (
        "mod_command",
        &[
            "Raider List match",
            "Raider List removal failed",
            "Case Deleted",
            "Old Cases Removed",
            "Case Updated",
            "Case Proof Added",
            "Case Proof Removed",
            "Note Added",
            "Note Modified",
            "Note Removed",
            "Notes Swept",
        ],
    ),
    ("appeal", &["Appeal Not Accepted"]),
    ("voice", &["Voice Join", "Voice Leave", "Voice Move"]),
    (
        "poll",
        &[
            "Poll Created",
            "Poll Ended",
            "Poll Vote Added",
            "Poll Vote Removed",
        ],
    ),
    ("user", &["User Avatar Changed", "User Name Changed"]),
    ("ticket", &["New Ticket", "Ticket Closed", "Ticket Updated"]),
];

fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_end_matches('_').into()
}

/// The entry of one log type: `kind_key` is the group's key (`log.<group>`), `title` the title the
/// log is sent with. `None` when that type has no entry of its own.
pub fn log_type_key(kind_key: &str, title: &str) -> Option<String> {
    let kind = kind_key.strip_prefix("log.")?;
    LOG_TYPES
        .iter()
        .find(|(k, t)| *k == kind && t.contains(&title))
        .map(|_| format!("log.{kind}.{}", slug(title)))
}

fn logs(out: &mut Vec<Def>) {
    for (kind, name) in LOGS {
        let security = matches!(*kind, "anti_nuke" | "panic" | "join_raid");
        let group = if security { SECURITY } else { *name };
        let types = LOG_TYPES
            .iter()
            .find(|(k, _)| k == kind)
            .map_or(&[][..], |t| t.1);
        let mut entries: Vec<(String, String)> = types
            .iter()
            .map(|t| (format!("log.{kind}.{}", slug(t)), (*t).to_string()))
            .collect();
        let rest = if types.is_empty() {
            name.to_string()
        } else {
            format!("Other {}", name.to_lowercase())
        };
        entries.push((format!("log.{kind}"), rest));
        for (key, label) in entries {
            let d = def(
                &key,
                &label,
                group,
                false,
                &["title", "text"],
                json!({"content": "", "embeds": [{"title": "{title}", "description": "{text}"}]}),
            );
            out.push(d);
        }
    }
}

/// Every command (the bot's `CMDS` by category; a bot test keeps the two equal): each reply of it
/// is a frame like a log, key `cmd.<name>`. Anti-nuke, panic, join-raid and whitelist commands are
/// security texts.
pub const COMMANDS: &[(&str, &[&str])] = &[
    (
        "General commands",
        &[
            "about",
            "commands",
            "dashboard",
            "help",
            "invite",
            "invites",
            "ping",
            "info",
            "roleinfo",
            "serverinfo",
            "security",
        ],
    ),
    (
        "Settings commands",
        &[
            "config",
            "statics",
            "heat",
            "hfilters",
            "antinuke",
            "ancaps",
            "anpanic",
            "antinukerestore",
            "joingate",
            "joinraid",
            "verification",
            "raiderlist",
            "misc",
            "whitelist",
            "tshoot",
        ],
    ),
    (
        "Moderation commands",
        &[
            "ban",
            "hackban",
            "unban",
            "kick",
            "timeout",
            "untimeout",
            "warn",
            "unwarn",
            "warns",
            "clearwarns",
            "quarantine",
            "unquarantine",
            "purge",
            "slowmode",
            "lock",
            "unlock",
            "sanitize",
            "verify",
            "cases",
            "modcases",
            "editc",
            "proof",
            "closecase",
            "raider",
            "ncases",
            "notes",
            "backups",
        ],
    ),
    (
        "Ticket commands",
        &[
            "reply",
            "close",
            "closeall",
            "blacklist",
            "unblock",
            "blacklistclear",
            "viewblacklist",
            "snippet",
            "snippetadd",
            "snippetremove",
            "snippetclear",
            "viewsnippet",
            "claim",
            "unclaim",
            "reassign",
            "priority",
            "tickettype",
            "ticketnote",
            "ticketinfo",
            "ticketsetup",
            "category",
            "pingrole",
            "logging",
            "loggingplus",
            "commandonly",
            "anonymous",
            "greetingmessage",
            "closingmessage",
            "toggle",
            "viewconfig",
        ],
    ),
];

fn commands(out: &mut Vec<Def>) {
    for (group, names) in COMMANDS {
        for name in *names {
            let security = matches!(
                *name,
                "antinuke" | "ancaps" | "anpanic" | "antinukerestore" | "joinraid" | "whitelist"
            );
            let d = def(
                &format!("cmd.{name}"),
                &format!("{name} command"),
                if security { SECURITY } else { group },
                false,
                &["title", "text"],
                json!({"content": "", "embeds": [{"title": "{title}", "description": "{text}"}]}),
            );
            out.push(d);
        }
    }
}

/// Entries sent outside any server (no server's edited version can apply): one copy for the whole
/// bot, stored under guild 0, edited by the bot owner only.
pub const BOT_WIDE: &[&str] = &[
    "dm.raider_notify",
    "dm.data_request",
    "reply.dm.usage_new",
    "reply.dm.usage_send",
    "reply.dm.confirmation_off",
    "reply.dm.confirmation_on",
    "reply.dm.no_server",
    "reply.dm.expired",
    "reply.dm.cancelled",
    "reply.dm.wait",
    "reply.dm.message_deleted",
    "reply.dm.type_closed",
    "reply.dm.menu",
    "reply.dm.option_existing",
    "reply.dm.option_new",
    "reply.dm.placeholder",
];

/// One version of a security text for its dashboard log line: the default, or the copy as JSON.
fn version(m: Option<&Value>) -> String {
    m.map_or("the default".into(), |m| {
        format!("`{}`", m.to_string().replace('`', "'"))
    })
}

/// The dashboard log line of a change to entry `d` from `old` to `new` (`None` = the default): a
/// security text names both versions, so the log shows what it said before.
pub fn change_line(d: &Def, did: &str, old: Option<&Value>, new: Option<&Value>) -> String {
    if d.strict() {
        format!(
            "{did} the {} “{}”: was {}, now {}",
            if d.security {
                "security text"
            } else {
                "bot message"
            },
            d.name,
            version(old),
            version(new)
        )
    } else {
        format!("{did} the bot message “{}”", d.name)
    }
}

/// The security texts' keys.
pub fn security_keys() -> Vec<String> {
    defs()
        .iter()
        .filter(|d| d.security)
        .map(|d| d.key.clone())
        .collect()
}

/// The `bot_messages` guild a copy of `key` lives under: 0 for a bot-wide entry, else `guild`.
pub fn copy_guild(key: &str, guild: i64) -> i64 {
    if BOT_WIDE.contains(&key) {
        0
    } else {
        guild
    }
}

/// What Brim fills `{text}` with, by entry.
fn what(key: &str) -> &'static str {
    match key {
        k if k.starts_with("log.") => "the details of the log (who, what and where)",
        k if k.starts_with("cmd.") => "the answer of the command",
        "ticket.reply" => "the staff member's reply",
        "ticket.sent" => "the message the member sent",
        "ticket.greeting" => {
            "the greeting text from the Tickets tab (the panel's own, else the server's)"
        }
        "ticket.panel" => "a line per ticket panel that has a description (Tickets tab)",
        "ticket.closing" => "the closing text of the ticket type",
        "reply.lock.updates_notice" => "the last five lock updates, newest last",
        "reply.lock.locked_notice" => "the numbered lock updates (empty until staff send one)",
        "reply.confirm.question" => "who or what the command acts on",
        "reply.error.usage" | "reply.ticket.usage" => "the usage line of the command",
        "reply.error.wrong_use" => "what was wrong with the command as typed",
        _ => "the text it writes for this message",
    }
}

pub fn defs() -> &'static [Def] {
    static D: OnceLock<Vec<Def>> = OnceLock::new();
    D.get_or_init(build)
}

pub fn find(key: &str) -> Option<&'static Def> {
    defs().iter().find(|d| d.key == key)
}

/// The main areas of the Bot messages page, in page order.
pub const AREAS: &[&str] = &[
    "Commands",
    "Logging",
    "Moderation",
    "Tickets",
    "Members and server",
    "Security",
];

/// The main area and sub-group of an entry on the Bot messages page (owner decision, 2026-11-21).
/// `None` for a group not placed yet; a test keeps every group placed.
pub fn area(d: &Def) -> Option<(&'static str, &'static str)> {
    let g = d.group;
    Some(match g {
        "General commands"
        | "Moderation commands"
        | "Settings commands"
        | "Settings panels"
        | "Tags"
        | "Errors and usage"
        | "Confirm prompt"
        | "Buttons and menus" => ("Commands", g),
        "Message logs" => ("Logging", "Messages"),
        "Member logs" | "User logs" | "Voice logs" => ("Logging", "Members and voice"),
        "Role logs" | "Channel logs" | "Server logs" | "Invite logs" | "Webhook logs"
        | "Emoji logs" | "Sticker logs" | "Event logs" | "Thread logs" | "Poll logs" => {
            ("Logging", "Server")
        }
        "Moderation logs" | "Appeal logs" | "Report logs" => {
            ("Logging", "Moderation, appeals and reports")
        }
        "Automod logs" | "Discord AutoMod rule logs" => ("Logging", "Automod"),
        "Verification logs" | "Join Gate logs" => ("Logging", "Verification and Join Gate"),
        "Ticket logs" => ("Logging", "Tickets"),
        "Punishment DMs" | "Member DMs" | "Cases and notes" | "Reports" | "Lockdown"
        | "Catch channel" => ("Moderation", g),
        "Tickets" if d.name.starts_with("Reply:") => ("Tickets", "Ticket replies"),
        "Tickets" => ("Tickets", "Ticket messages"),
        "Ticket commands" => ("Tickets", g),
        "Verification" | "Welcome" | "Feeds" => ("Members and server", g),
        OUTSIDE | "Owner DMs" => ("Members and server", "Bot owner"),
        "Security" => {
            let n = d.name.to_lowercase();
            let sub = if n.starts_with("reply:") || n.starts_with("answer:") {
                "Command replies"
            } else if n.contains("anti-nuke")
                || n.starts_with("antinuke")
                || n.starts_with("ancaps")
            {
                "Anti-nuke"
            } else if n.contains("panic") || n.contains("join raid") || n.starts_with("joinraid") {
                "Panic and join raid"
            } else {
                "Other alerts"
            };
            ("Security", sub)
        }
        _ => return None,
    })
}

/// (variable, name in the editor, example for the preview and the test DM).
const VAR_INFO: &[(&str, &str, &str)] = &[
    ("server", "Server name", "My Server"),
    (
        "permissions",
        "Brim's dangerous Discord permissions, one per line",
        "Administrator",
    ),
    ("reason", "Reason", "Spamming in #general"),
    ("moderator", "Moderator name", "alex"),
    (
        "moderator_code",
        "Moderator (code style, empty when staff is hidden)",
        "`alex`",
    ),
    ("duration", "Duration (code style)", "`1 hour`"),
    (
        "staff_line",
        "How to reach staff (quarantine without a reason)",
        "You can DM Brim to reach this server's staff.",
    ),
    (
        "appeal",
        "Appeal line with link",
        "You can appeal here: https://example.com/appeal",
    ),
    ("code", "Invite code", "AbCd123"),
    ("expiry", "Invite expiry", "expires in 7 days"),
    ("uses", "Invite uses", "for 5 uses"),
    (
        "why",
        "Automod reason (with \": \", empty when hidden)",
        ": Message spam",
    ),
    (
        "note",
        "Appeal note (new line first)",
        "\nYou can appeal here: https://example.com/appeal",
    ),
    ("time", "Time left to verify", "10 minutes"),
    ("kind", "Appeal kind", "ban"),
    ("id", "ID of the item (appeal number, note id ...)", "42"),
    ("verdict", "Verdict", "approved"),
    (
        "reply",
        "Staff reply",
        "Welcome back, please read the rules.",
    ),
    (
        "text",
        "Text Brim writes",
        "What happened, written by Brim.",
    ),
    ("name", "Server name", "My Server"),
    ("channel", "Channel", "#verify"),
    ("prefix", "Command prefix", "t!"),
    ("permit", "Permit name", "lock updates"),
    ("lock", "Lock type", "channels"),
    ("error", "What went wrong", "Missing Permissions"),
    ("tag", "Tag the member typed", "rules"),
    ("title", "Log title", "Message Deleted"),
    ("server_id", "Server ID", "123456789012345678"),
    (
        "steps",
        "Steps for this server's verification",
        "1. Press the **Verify** button.\n2. You get access right away.",
    ),
    (
        "again",
        "How to try again",
        "Use the button below to join again and verify.",
    ),
    (
        "setup",
        "Where to set the bot up",
        "You can set the bot up in the dashboard.",
    ),
    ("page", "Link to the page", "https://example.com/rescue"),
    ("seconds", "Seconds left to answer", "10"),
    ("count", "Number", "3"),
    ("cap", "The most allowed", "100"),
    ("command", "The command with its prefix", "t!claim"),
    (
        "punishment",
        "What writing in the catch channel does",
        "get you automatically banned",
    ),
    ("case", "Case number", "12"),
    ("type", "Ticket type", "Billing"),
    ("old_type", "Earlier ticket type", "General"),
    ("priority", "Ticket priority", "high"),
    (
        "choices",
        "The values you can pick",
        "low, normal, high, urgent",
    ),
    ("typed", "What the member typed", "<@&5>"),
    ("closed", "Tickets closed", "3"),
    ("total", "Open tickets", "4"),
    ("left", "Tickets not closed", "1"),
    ("user", "Member (mention)", "@alex"),
    ("action", "What the command does", "ban"),
    (
        "cause",
        "Why the case no longer holds the member",
        "case #14 came after it",
    ),
    ("text", "Bot text (required)", "The text Brim writes here."),
];

/// The variables of the feed entries, which reuse common names (`{title}`) with their own meaning.
const FEED_VARS: &[(&str, &str, &str)] = &[
    ("title", "Post title", "A new video"),
    (
        "link",
        "Link to the post (required)",
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    ),
    ("author", "Post author", "Some Creator"),
    (
        "summary",
        "Start of the post text",
        "The first lines of the post.",
    ),
    (
        "image",
        "Post picture link",
        "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
    ),
    ("feed", "Feed name", "Some Creator"),
    ("ping", "Ping role", "@News"),
    (
        "headline",
        "Event title, as GitHub's own webhook writes it",
        "[octo/hello] Pull request opened: #2 Update the README",
    ),
    (
        "author_link",
        "Link to the author's profile",
        "https://github.com/octocat",
    ),
    (
        "author_icon",
        "Author's picture link",
        "https://avatars.githubusercontent.com/u/583231",
    ),
    ("repo", "Repository with its owner", "octo/hello"),
    ("repo_name", "Repository name", "hello"),
    ("branch", "Branch", "main"),
    ("action", "What happened", "opened"),
    ("number", "Issue or pull request number", "2"),
];

/// The editor name and example of a template variable of entry `key`.
pub fn var_info(key: &str, name: &str) -> (&'static str, &'static str) {
    if let Some(rows) = panel(key) {
        if let Some(r) = rows.iter().find(|r| r.0 == name) {
            // As the panel shows it: `[3]`, or `[A]` for a lettered row.
            const SHOWN: [&str; 25] = [
                "[1]", "[2]", "[3]", "[4]", "[5]", "[6]", "[7]", "[8]", "[9]", "[10]", "[11]",
                "[12]", "[13]", "[14]", "[15]", "[16]", "[A]", "[B]", "[C]", "[D]", "[E]", "[F]",
                "[G]", "[H]", "[I]",
            ];
            let tail = r.0.trim_start_matches(|c: char| c.is_ascii_digit());
            let want = if tail.is_empty() { r.0 } else { tail }.to_uppercase();
            let shown = SHOWN.iter().find(|s| s[1..s.len() - 1] == want);
            return (
                "Row number people type to change it",
                shown.copied().unwrap_or(r.0),
            );
        }
        if let Some(r) = rows.iter().find(|r| r.2 == name) {
            return (
                r.1,
                if r.3 == "raw" {
                    "<#123456789012345678>"
                } else {
                    "Enabled"
                },
            );
        }
        if name == "toggle" {
            return ("On or off sign after Status", " 🟢");
        }
    }
    let list = if key.starts_with("feed.") {
        FEED_VARS
    } else {
        VAR_INFO
    };
    let own: &[_] = if is_facts(key) { FACT_VARS } else { &[] };
    own.iter()
        .chain(list)
        .find(|v| v.0 == name)
        .map_or(("", ""), |v| (v.1, v.2))
}

/// `{name}` for each variable in `vars`, in one left-to-right pass (a value is never searched for
/// more variables). A line whose variables are all empty and that is left as a bare label
/// (`... **Moderator:**`) or nothing is dropped, so an optional line vanishes without its label.
fn fill_text(s: &str, vars: &[(&str, &str)]) -> String {
    s.split('\n')
        .filter_map(|line| {
            let (mut out, mut rest, mut used, mut any) = (String::new(), line, false, false);
            while let Some(i) = rest.find('{') {
                out.push_str(&rest[..i]);
                rest = &rest[i..];
                let hit = rest.find('}').and_then(|j| {
                    let n = &rest[1..j];
                    vars.iter().find(|(k, _)| *k == n).map(|(_, v)| (v, j))
                });
                match hit {
                    Some((v, j)) => {
                        used = true;
                        any |= !v.is_empty();
                        out.push_str(v);
                        rest = &rest[j + 1..];
                    }
                    None => {
                        out.push('{');
                        rest = &rest[1..];
                    }
                }
            }
            out.push_str(rest);
            // An empty code span (`` `{case}` `` with no case) leaves the label bare too.
            let t = out.trim_end().trim_end_matches('`').trim_end();
            let bare = t.is_empty() || t.ends_with(":**");
            (!used || any || !bare).then_some(out)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fill_value(v: &mut Value, vars: &[(&str, &str)]) {
    match v {
        Value::String(s) => *s = fill_text(s, vars),
        Value::Array(a) => a.iter_mut().for_each(|x| fill_value(x, vars)),
        Value::Object(o) => o
            .iter_mut()
            .filter(|(k, _)| !matches!(k.as_str(), "color" | "url" | "icon_url" | "proxy_icon_url"))
            .for_each(|(_, x)| fill_value(x, vars)),
        _ => {}
    }
}

/// Feed entries: the link fields (embed, author and footer links, pictures) get the feed's link
/// variables, which the sender checked are web links. Other text never becomes a link, and a field
/// left without a web link is emptied (then dropped): Discord refuses the whole message for one.
fn fill_links(v: &mut Value, vars: &[(&str, &str)]) {
    const LINKS: &[&str] = &["link", "image", "author_link", "author_icon"];
    match v {
        Value::Array(a) => a.iter_mut().for_each(|x| fill_links(x, vars)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(s) if matches!(k.as_str(), "url" | "icon_url") => {
                        let links: Vec<(&str, &str)> = vars
                            .iter()
                            .copied()
                            .filter(|(n, _)| LINKS.contains(n))
                            .collect();
                        let f = fill_text(s, &links);
                        *s = if f.starts_with("https://") || f.starts_with("http://") {
                            f
                        } else {
                            String::new()
                        };
                    }
                    _ => fill_links(x, vars),
                }
            }
        }
        _ => {}
    }
}

/// Whether [`render`] sends the edited version `stored` of `key` (not locked, a message, with
/// every required placeholder) and not the default.
pub fn uses_stored(key: &str, stored: &Value) -> bool {
    find(key)
        .is_some_and(|d| !d.locked && stored.is_object() && missing_text(key, stored).is_none())
}

/// The message to send: the stored copy (unless locked, not a message, or without a required
/// placeholder: the bot's own text must never be dropped) else the default, filled.
pub fn render(key: &str, stored: Option<&Value>, vars: &[(&str, &str)]) -> Value {
    let d = find(key).expect("known bot message key");
    let used = stored.filter(|s| uses_stored(key, s));
    let mut m = used.unwrap_or(&d.template).clone();
    // A copy made before the facts had placeholders: `{text}` holds them all.
    let framed = used.is_some_and(|u| is_frame(d, u));
    fill_value(&mut m, vars);
    if key.starts_with("feed.") {
        fill_links(&mut m, vars);
    }
    let whole = m.clone();
    limit(&mut m);
    // A security text's facts must arrive whole: an edited version whose own words pushed one past
    // Discord's limits sends the default. A text of facts also sends it when anything was cut
    // (a short fact like a case number is found elsewhere by `keeps_facts`).
    let cut = d.details() && !framed && m != whole;
    drop_blank_fields(&mut m);
    if d.strict() && (cut || !keeps_facts(d, &m, vars, framed)) {
        let mut def = d.template.clone();
        fill_value(&mut def, vars);
        limit(&mut def);
        drop_blank_fields(&mut def);
        // A value too long for Discord is cut in the default too: then the edit stays.
        if keeps_facts(d, &def, vars, false) {
            m = def;
        }
    }
    // An embed whose lines were all empty variables has only a colour: Discord refuses it.
    if let Some(e) = m["embeds"].as_array_mut() {
        e.retain(has_text);
        // A frame filled with an empty `{text}` (a ticket message with only a file): Discord
        // refuses an empty description.
        for x in e.iter_mut().filter_map(Value::as_object_mut) {
            if x.get("description").is_some_and(|d| d == "") {
                x.remove("description");
            }
            // An empty link (a variable with no value, like a GitHub event without a page):
            // Discord refuses the whole message.
            let blank = |o: &serde_json::Map<String, Value>, k: &str| {
                o.get(k)
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.trim().is_empty())
            };
            if blank(x, "url") {
                x.remove("url");
            }
            for part in ["image", "thumbnail"] {
                if x.get(part)
                    .and_then(Value::as_object)
                    .is_some_and(|o| blank(o, "url"))
                {
                    x.remove(part);
                }
            }
            for (part, k) in [
                ("author", "url"),
                ("author", "icon_url"),
                ("footer", "icon_url"),
            ] {
                if let Some(o) = x.get_mut(part).and_then(Value::as_object_mut) {
                    if blank(o, k) {
                        o.remove(k);
                    }
                }
            }
            // Same for a footer or author with no text (an edited copy may leave one).
            for (part, text) in [("footer", "text"), ("author", "name")] {
                let empty = x.get(part).is_some_and(|p| {
                    p.get(text)
                        .and_then(Value::as_str)
                        .is_none_or(|t| t.trim().is_empty())
                });
                if empty {
                    x.remove(part);
                }
            }
        }
    }
    // A saved copy that sends nothing falls back to the default.
    if m["embeds"].as_array().is_none_or(Vec::is_empty)
        && m["content"].as_str().is_none_or(|c| c.trim().is_empty())
    {
        m = d.template.clone();
        fill_value(&mut m, vars);
        limit(&mut m);
    }
    m
}

/// A field whose name or value is empty (a case without a duration): Discord refuses the whole
/// message for one, so it goes.
fn drop_blank_fields(m: &mut Value) {
    let blank = |v: &Value| v.as_str().is_none_or(|v| v.trim().is_empty());
    for f in m["embeds"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter_map(|e| e.get_mut("fields").and_then(Value::as_array_mut))
    {
        f.retain(|x| !blank(&x["value"]) && !blank(&x["name"]));
    }
}

/// Whether `copy` is a frame made before `d` had placeholders for its facts: it shows `{text}`.
fn is_frame(d: &Def, copy: &Value) -> bool {
    d.details() && d.vars.contains(&"text") && shown(copy).iter().any(|v| v == "text")
}

/// Whether every non-empty value of `d`'s required placeholders is whole in `m`'s title or
/// description (or text).
fn keeps_facts(d: &Def, m: &Value, vars: &[(&str, &str)], framed: bool) -> bool {
    // Only where the save check put them: a copy of a fact in a footer spoiler does not count.
    // A copy made before (a frame) was saved under the old rules: `{text}` may be anywhere shown.
    let texts = if framed {
        let mut all = String::new();
        strings(m, &mut all);
        all
    } else {
        fact_strings(d, m).join("\n")
    };
    let own = ["text"];
    let required: &[&str] = if framed { &own } else { &d.required };
    required.iter().all(|r| {
        vars.iter()
            .find(|(k, _)| k == r)
            .is_none_or(|(_, v)| v.trim().is_empty() || texts.contains(v.trim()))
    })
}

fn strings(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// Whether Discord shows something of the embed: a text, a field or a picture.
fn has_text(e: &Value) -> bool {
    let set = |p: &str| {
        e.pointer(p)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    [
        "/title",
        "/description",
        "/author/name",
        "/footer/text",
        "/image/url",
        "/thumbnail/url",
    ]
    .iter()
    .any(|p| set(p))
        || e["fields"].as_array().is_some_and(|f| !f.is_empty())
}

/// Cuts a filled message to Discord's limits (a long reason must not make Discord refuse the DM):
/// title 256, description 4096, footer 2048, field name 256 and value 1024, content 2000, and
/// 6000 for all embed text together (the description gives way first).
fn limit(m: &mut Value) {
    // Only existing strings are touched: indexing a missing key would add a null one.
    fn cut(v: Option<&mut Value>, max: usize) -> usize {
        let Some(v) = v else { return 0 };
        if let Some(s) = v.as_str() {
            *v = crate::embed::clip(s, max).into();
        }
        v.as_str().map_or(0, |s| s.chars().count())
    }
    cut(m.get_mut("content"), 2000);
    let Some(embeds) = m["embeds"].as_array_mut() else {
        return;
    };
    embeds.retain(Value::is_object);
    embeds.truncate(10);
    let mut total = 0;
    for e in embeds.iter_mut() {
        total += cut(e.get_mut("title"), 256) + cut(e.get_mut("description"), 4096);
        total += cut(e.get_mut("footer").and_then(|f| f.get_mut("text")), 2048);
        total += cut(e.get_mut("author").and_then(|a| a.get_mut("name")), 256);
        if let Some(f) = e.get_mut("fields").and_then(Value::as_array_mut) {
            f.truncate(25);
            for x in f {
                total += cut(x.get_mut("name"), 256) + cut(x.get_mut("value"), 1024);
            }
        }
    }
    if total > 6000 {
        let cut_by = total - 6000;
        if let Some(d) = embeds
            .iter_mut()
            .find_map(|e| e["description"].as_str().map(str::to_string))
        {
            let keep = d.chars().count().saturating_sub(cut_by).max(1);
            if let Some(e) = embeds.iter_mut().find(|e| e["description"].is_string()) {
                e["description"] = crate::embed::clip(&d, keep).into();
            }
        }
    }
    // Still over (no description, or too short to give enough): the last fields go, so Discord
    // never refuses the whole message.
    let len = |v: &Value| v.as_str().map_or(0, |s| s.chars().count());
    let size = |e: &Value| {
        len(&e["title"])
            + len(&e["description"])
            + len(&e["footer"]["text"])
            + len(&e["author"]["name"])
            + e["fields"].as_array().map_or(0, |f| {
                f.iter().map(|x| len(&x["name"]) + len(&x["value"])).sum()
            })
    };
    while embeds.iter().map(size).sum::<usize>() > 6000 {
        let Some(f) = embeds.iter_mut().rev().find_map(|e| {
            e.get_mut("fields")
                .and_then(Value::as_array_mut)
                .filter(|f| !f.is_empty())
        }) else {
            break;
        };
        f.pop();
    }
}

/// Variables in `msg` that `key` does not offer (the save refuses these).
pub fn unknown_vars(key: &str, msg: &Value) -> Vec<String> {
    let Some(d) = find(key) else { return vec![] };
    let mut found = Vec::new();
    collect(msg, &mut found);
    // The panel message fills Brim emoji tags itself.
    found.retain(|n| {
        !d.vars.contains(&n.as_str()) && !(key == "ticket.panel" && n.starts_with("e:"))
    });
    found.sort();
    found.dedup();
    found
}

/// Placeholders where every sender shows them: the first embed's title, description, field names
/// and values, footer and author text. Senders of one embed use only the first; content, links
/// and pictures do not count (a frame laid over a log ignores content).
fn shown(msg: &Value) -> Vec<String> {
    let e = &msg["embeds"][0];
    let mut out = Vec::new();
    for p in ["/title", "/description", "/footer/text", "/author/name"] {
        if let Some(v) = e.pointer(p) {
            collect(v, &mut out);
        }
    }
    for f in e["fields"].as_array().into_iter().flatten() {
        collect(&f["name"], &mut out);
        collect(&f["value"], &mut out);
    }
    out
}

/// Why an edited copy of `key` cannot be saved or sent: it lacks a required placeholder in its
/// first embed. `None` when it has all of them (or `key` is unknown).
pub fn missing_text(key: &str, msg: &Value) -> Option<String> {
    let d = find(key)?;
    let frame = is_frame(d, msg);
    if d.strict() && !frame {
        if let Some(why) = hides_facts(d, msg) {
            return Some(why);
        }
    }
    let mut found = if d.strict() {
        fact_places(d, msg)
    } else {
        shown(msg)
    };
    // An entry whose default is message text (a feed's plain `{link}`) may keep it in the text.
    let in_text = in_text(d);
    if in_text && !d.strict() {
        collect(&msg["content"], &mut found);
    }
    // A feed's link may be what the first embed's title opens (the GitHub look).
    if key.starts_with("feed.") {
        collect(&msg["embeds"][0]["url"], &mut found);
    }
    // A copy made before the facts had placeholders is a frame around `{text}`, which holds them.
    // Anywhere a frame could hold it before (footer, author, field), as the old rules allowed.
    let framed = frame;
    let gone: Vec<String> = d
        .required
        .iter()
        .filter(|v| !framed && !found.iter().any(|f| f == *v))
        .map(|v| format!("{{{v}}}"))
        .collect();
    (!gone.is_empty()).then(|| {
        let (place, insert) = if in_text {
            ("its text or first embed", "Put it back")
        } else {
            ("its first embed", "Press Insert Bot text")
        };
        let plain = if d.strict() {
            ", as plain text (not in a spoiler, a link or <...>)"
        } else {
            ""
        };
        let key = if gone.len() == 1 { "the key variable" } else { "the key variables" };
        format!(
            "this message must keep {key} {} in {place}{plain}: Brim puts {} there. {insert}, or use Revert to default",
            gone.join(", "),
            d.what
        )
    })
}

/// Where a security text's facts must be: the first embed's title and description (the text, for a
/// text-only entry), not a footer, author or field Discord shows small or cuts.
fn fact_strings(d: &Def, msg: &Value) -> Vec<String> {
    if in_text(d) {
        return vec![msg["content"].as_str().unwrap_or("").to_string()];
    }
    let mut out: Vec<String> = ["title", "description"]
        .iter()
        .map(|k| msg["embeds"][0][k].as_str().unwrap_or("").to_string())
        .collect();
    // A log whose default keeps its facts in fields (the case log) may keep them there.
    if d.template["embeds"][0]["fields"].is_array() {
        for f in msg["embeds"][0]["fields"].as_array().into_iter().flatten() {
            out.extend(["name", "value"].map(|k| f[k].as_str().unwrap_or("").to_string()));
        }
    }
    out
}

/// The placeholders a security text shows as plain text in [`fact_strings`], within the first
/// [`FACT_LINES`] lines, or as many as the default has (a wall of empty lines must not push a fact
/// out of sight).
fn fact_places(d: &Def, msg: &Value) -> Vec<String> {
    let mut own = d.template["embeds"][0]["description"]
        .as_str()
        .map_or(0, |t| t.split('\n').count());
    // A panel's rows may all go in one list: as many lines as its fields have.
    if panel(&d.key).is_some() {
        own = fact_strings(d, &d.template)
            .iter()
            .map(|s| s.split('\n').count())
            .sum();
    }
    let mut found = Vec::new();
    for s in fact_strings(d, msg) {
        let top: Vec<&str> = s.split('\n').take(FACT_LINES.max(own)).collect();
        collect(&Value::String(visible(&top.join("\n"))), &mut found);
    }
    found
}

const FACT_LINES: usize = 10;

/// Why an edited security text could misstate its facts: characters that hide or reverse text
/// (control, zero-width and direction marks).
fn hides_facts(d: &Def, msg: &Value) -> Option<String> {
    let mut all = String::new();
    strings(msg, &mut all);
    let bad = |c: char| {
        (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
    };
    all.chars().any(bad).then(|| {
        format!(
            "“{}” holds key variables the recipient needs: it cannot hold invisible, control or text direction characters",
            d.name
        )
    })
}

/// `s` without what hides a placeholder: spoilers (`||...||`, and all after an unclosed `||`),
/// link targets (`](...)`), anything in `<...>` (mentions, emoji, times, comment tricks) and a code
/// block's language tag. An unclosed `](` or `<` hides the rest.
fn visible(s: &str) -> String {
    let open: String = s.split("||").step_by(2).collect::<Vec<_>>().join("");
    let open: String = open
        .split("```")
        .enumerate()
        .map(|(i, p)| match p.split_once('\n') {
            // Inside a code block, its first line is the language tag when it has no spaces.
            Some((tag, rest)) if i % 2 == 1 && !tag.contains(' ') => rest,
            _ => p,
        })
        .collect::<Vec<_>>()
        .join("```");
    let (mut out, mut rest) = (String::new(), open.as_str());
    loop {
        let link = rest.find("](").map(|i| (i, 2, ')'));
        let angle = rest.find('<').map(|i| (i, 1, '>'));
        let Some((i, n, close)) = [link, angle].into_iter().flatten().min_by_key(|x| x.0) else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i + n - 1]);
        rest = &rest[i + n..];
        match rest.find(close) {
            Some(j) => rest = &rest[j + 1..],
            None => return out,
        }
    }
}

/// Whether `d`'s default is message text only, so its required placeholder may be in the text.
pub fn in_text(d: &Def) -> bool {
    d.template["embeds"].as_array().is_none_or(Vec::is_empty) && !d.required.is_empty()
}

/// Important variables an edited copy dropped (red warning, not a block).
pub fn dropped(key: &str, msg: &Value) -> Vec<&'static str> {
    let Some(d) = find(key) else { return vec![] };
    let mut found = Vec::new();
    collect(msg, &mut found);
    d.important
        .iter()
        .copied()
        .filter(|v| !found.iter().any(|f| f == v))
        .collect()
}

fn collect(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(i) = rest.find('{') {
                rest = &rest[i + 1..];
                if let Some(j) = rest.find('}') {
                    let n = &rest[..j];
                    // Message Builder variables (`{server.name}`, `{role:1}`) count too: a template
                    // does not fill them, so a saved copy must not hold them.
                    let ok =
                        |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:".contains(&b);
                    if !n.is_empty() && n.bytes().all(ok) {
                        out.push(n.into());
                    }
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_texts_are_locked() {
        for k in HIDDEN {
            let d = find(k).unwrap_or_else(|| panic!("{k} is not an entry"));
            assert!(d.locked, "{k}");
            // A stored copy never reaches the member.
            let m = render(k, Some(&json!({"content": "hijacked"})), &[]);
            assert_ne!(m["content"], "hijacked", "{k}");
        }
    }

    #[test]
    fn every_entry_has_a_main_area() {
        for d in defs() {
            let (a, _) = area(d).unwrap_or_else(|| panic!("group {:?} has no main area", d.group));
            assert!(AREAS.contains(&a));
            // A security text never lands outside the Security area, where it is marked as such.
            assert_eq!(d.security, a == "Security", "{}", d.key);
        }
    }

    #[test]
    fn default_fills_and_drops_empty_lines() {
        let m = render(
            "dm.punish.timeout",
            None,
            &[
                ("server", "S"),
                ("reason", "spam"),
                ("moderator", ""),
                ("moderator_code", ""),
                ("duration", "`5m`"),
                ("staff_line", ""),
            ],
        );
        assert_eq!(m["embeds"][0]["title"], "You have been timed out in S!");
        let d = m["embeds"][0]["description"].as_str().unwrap();
        assert!(d.contains("spam") && d.contains("`5m`") && !d.contains("Moderator"));
    }

    /// The data request and Raider List DMs state facts about the recipient's data or standing:
    /// an edit keeps them as plain text in the title or description, or the default is sent.
    #[test]
    fn outside_server_facts_arrive_as_plain_text() {
        for k in ["dm.data_request", "dm.raider_notify"] {
            let d = find(k).unwrap();
            assert!(!d.locked && d.strict() && !d.security, "{k}");
            let vars = [("text", "FACT")];
            let ok =
                json!({"content": "", "embeds": [{"title": "Mine", "description": "Hi\n{text}"}]});
            assert_eq!(render(k, Some(&ok), &vars)["embeds"][0]["title"], "Mine");
            assert!(missing_text(k, &ok).is_none());
            for bad in [
                json!({"embeds": [{"description": "||{text}||"}]}),
                json!({"embeds": [{"description": "x", "footer": {"text": "{text}"}}]}),
                json!({"embeds": [{"description": "[a]({text})"}]}),
                json!({"embeds": [{"description": "{text}\u{200b}"}]}),
            ] {
                assert!(missing_text(k, &bad).is_some(), "{k} {bad}");
                let m = render(k, Some(&bad), &vars);
                assert_eq!(m, render(k, None, &vars), "{k} {bad}");
            }
        }
    }

    /// Every entry of the outside group is bot-wide, and the DM command defaults read as before.
    #[test]
    fn dm_replies_are_bot_wide_and_keep_their_words() {
        for d in defs() {
            assert_eq!(
                d.group == OUTSIDE,
                BOT_WIDE.contains(&d.key.as_str()),
                "{}",
                d.key
            );
        }
        let desc = |k: &str, v: &[(&str, &str)]| {
            render(k, None, v)["embeds"][0]["description"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            desc("reply.dm.usage_new", &[("prefix", "t!")]),
            "Usage: `t!new <message>`"
        );
        assert_eq!(
            desc("reply.dm.usage_send", &[("prefix", "t!")]),
            "Usage: `t!send <server ID> <message>`"
        );
        assert_eq!(
            desc("reply.dm.confirmation_off", &[("prefix", "t!")]),
            "Confirmation messages are disabled. When you share more than one ticket server with Brim, you still pick the server for each message, or use `t!send <server ID> <message>`."
        );
        let menu = render("reply.dm.menu", None, &[]);
        assert!(menu["embeds"][0]["title"]
            .as_str()
            .unwrap()
            .ends_with("Select Server"));
        assert_eq!(
            render("reply.dm.placeholder", None, &[])["content"],
            "Select a server"
        );
        assert_eq!(
            render("reply.dm.option_new", None, &[])["content"],
            "Create a new ticket."
        );
        assert_eq!(
            render("reply.dm.wait", None, &[])["embeds"][0]["color"],
            RED
        );
        assert_eq!(
            render("reply.dm.confirmation_on", None, &[])["embeds"][0]["color"],
            BLUE
        );
    }

    /// Security texts (plan section 6): editable, but every required placeholder must show as
    /// plain text, and a fact that does not arrive whole sends the default.
    #[test]
    fn security_texts_keep_their_facts() {
        let sec: Vec<&str> = defs()
            .iter()
            .filter(|d| d.security)
            .map(|d| d.key.as_str())
            .collect();
        for k in [
            "dm.security",
            "log.anti_nuke",
            "log.panic",
            "log.join_raid",
            "cmd.antinuke",
            "cmd.whitelist",
            "reply.whitelist.cancelled",
        ] {
            assert!(sec.contains(&k), "{k}");
            assert!(!find(k).unwrap().locked, "{k}");
        }
        // The recovery texts stay security texts, but nobody changes them (owner decision 2026-11-21).
        for k in ["dm.mcp_decision", "dm.invite_back", "dm.dashboard_2fa"] {
            assert!(sec.contains(&k) && find(k).unwrap().locked, "{k}");
        }
        assert_eq!(find("log.panic").unwrap().required, ["title", "text"]);
        // No rescue key text is editable: a saved copy never applies.
        let rescue = find("dm.rescue_expired").unwrap();
        assert!(rescue.locked && rescue.security);
        let copy =
            json!({"content": "", "embeds": [{"title": "Hi", "description": "{name} {page}"}]});
        let m = render(
            "dm.rescue_expired",
            Some(&copy),
            &[("name", "S"), ("page", "https://x")],
        );
        assert_eq!(m["embeds"][0]["title"], "Rescue key expired");
        assert_eq!(find("dm.dashboard_2fa").unwrap().required, ["code"]);
        let e =
            |d: &str| json!({"content": "", "embeds": [{"title": "{title}", "description": d}]});
        for hidden in [
            "||{text}||",
            "a || b {text}",
            "[look](https://x/{text})",
            "<!-- {text} -->",
            "<{text}>",
            "```{text}\ncode```",
        ] {
            let why = missing_text("log.panic", &e(hidden));
            assert!(why.is_some_and(|w| w.contains("plain text")), "{hidden}");
            // Other entries keep the old rule.
            assert!(
                missing_text("log.message", &e(hidden)).is_none(),
                "{hidden}"
            );
        }
        for shown in [
            "Alert: {text}",
            "[{text}](https://x)",
            "||spoiler|| {text}",
            "`{text}`",
            "```\n{text}```",
        ] {
            assert!(missing_text("log.panic", &e(shown)).is_none(), "{shown}");
        }
        let no_title =
            json!({"content": "", "embeds": [{"title": "Fine", "description": "{text}"}]});
        assert!(missing_text("log.panic", &no_title).is_some_and(|w| w.contains("{title}")));
        // An edited version is used, with the facts in it.
        let vars = [("text", "<@1> deleted 5 channels")];
        let ok =
            json!({"content": "", "embeds": [{"title": "Look", "description": "Alert: {text}"}]});
        let m = render("dm.security", Some(&ok), &vars);
        assert_eq!(
            m["embeds"][0]["description"],
            "Alert: <@1> deleted 5 channels"
        );
        // Its own words push the facts past Discord's 4096: the default goes out.
        let long = json!({"content": "", "embeds": [{"description": format!("{}{{text}}", "x".repeat(4095))}]});
        let m = render("dm.security", Some(&long), &vars);
        assert_eq!(m["embeds"][0]["title"], "Security alert");
        assert!(m.to_string().contains("deleted 5 channels"));
        // A fact in the author name is cut to Discord's 256 too, so the guard sees it.
        let author = json!({"content": "", "embeds": [{"author": {"name": format!("{}{{code}}", "y".repeat(254))}}]});
        let m = render("dm.dashboard_2fa", Some(&author), &[("code", "123456")]);
        assert!(m.to_string().contains("**123456**"), "{m}");
        // Facts go in the title or description, near the top, with no hiding characters.
        for bad in [
            json!({"embeds": [{"title": "Calm", "description": "{text}", "footer": {"text": "{title}"}}]}),
            json!({"embeds": [{"title": "Calm", "author": {"name": "{title}"}, "description": "{text}"}]}),
            json!({"embeds": [{"title": "{title}", "description": format!("{}{{text}}", "\u{200b}\n".repeat(10))}]}),
            json!({"embeds": [{"title": "{title}", "description": "\u{202e}{text}"}]}),
            json!({"embeds": [{"title": "{title}", "description": "```ansi\n\u{1b}[30m{text}```"}]}),
        ] {
            assert!(missing_text("log.panic", &bad).is_some(), "{bad}");
        }
        // A copy of the fact in a footer spoiler does not count when the description cut it.
        let spoil = json!({"embeds": [{"description": format!("{}{{text}}", "x".repeat(4090)), "footer": {"text": "||{text}||"}}]});
        let m = render("dm.security", Some(&spoil), &vars);
        assert_eq!(m["embeds"][0]["title"], "Security alert");
        // A value too long for Discord anyway keeps the edit (the default would cut it too).
        let huge = "z".repeat(5000);
        let m = render("dm.security", Some(&ok), &[("text", &huge)]);
        assert_eq!(m["embeds"][0]["title"], "Look");
    }

    /// A feed post: the plain link (no ping line without a ping role); an edited version must keep
    /// `{link}` in its text or first embed, else the default is sent.
    #[test]
    fn feed_posts_need_their_link() {
        let vars = [
            ("link", "https://a.example/1"),
            ("ping", ""),
            ("title", "T"),
        ];
        assert_eq!(
            render("feed.rss", None, &vars)["content"],
            "https://a.example/1"
        );
        let pinged = [("link", "https://a.example/1"), ("ping", "<@&5>")];
        assert_eq!(
            render("feed.rss", None, &pinged)["content"],
            "<@&5>\nhttps://a.example/1"
        );
        let own = json!({"content": "New: {title} {link}", "embeds": []});
        assert!(missing_text("feed.youtube", &own).is_none());
        assert_eq!(
            render("feed.rss", Some(&own), &vars)["content"],
            "New: T https://a.example/1"
        );
        let in_embed = json!({"content": "", "embeds": [{"description": "{link}"}]});
        assert!(missing_text("feed.rss", &in_embed).is_none());
        let lost = json!({"content": "New: {title}", "embeds": []});
        assert!(missing_text("feed.rss", &lost).unwrap().contains("{link}"));
        assert_eq!(
            render("feed.rss", Some(&lost), &vars)["content"],
            "https://a.example/1"
        );
        // Elsewhere content does not count: a log frame ignores it.
        assert!(missing_text(
            "ticket.reply",
            &json!({"content": "{text}", "embeds": [{"description": "x"}]})
        )
        .is_some());
    }

    /// The GitHub look: title, link and author line as GitHub's own webhook; parts without a
    /// value (no link, no text, no author) leave the embed instead of making Discord refuse it.
    #[test]
    fn github_entries_render_the_recorded_look() {
        let vars = [
            ("headline", "[o/r] Issue opened: #1 Bug"),
            ("link", "https://github.com/o/r/issues/1"),
            ("summary", "Text"),
            ("author", "octo"),
            ("author_link", "https://github.com/octo"),
            ("author_icon", "https://avatars.githubusercontent.com/u/1"),
            ("ping", ""),
        ];
        let m = render("feed.github.issues", None, &vars);
        let e = &m["embeds"][0];
        assert_eq!(e["title"], "[o/r] Issue opened: #1 Bug");
        assert_eq!(e["url"], "https://github.com/o/r/issues/1");
        assert_eq!(e["author"]["name"], "octo");
        assert_eq!(e["color"], 0xEB6420);
        assert_eq!(m["content"], "");
        let bare = [
            ("headline", "[o/r] New tag created: v1"),
            ("link", ""),
            ("summary", ""),
            ("author", ""),
            ("author_link", ""),
            ("author_icon", ""),
        ];
        let e = render("feed.github.create", None, &bare)["embeds"][0].clone();
        assert_eq!(e["title"], "[o/r] New tag created: v1");
        for gone in ["url", "description", "author"] {
            assert!(e.get(gone).is_none(), "{gone}: {e}");
        }
        // An author with a name but no links keeps the name only.
        let half = [
            ("headline", "h"),
            ("author", "octo"),
            ("author_link", ""),
            ("author_icon", ""),
        ];
        let e = render("feed.github.public", None, &half)["embeds"][0].clone();
        assert_eq!(e["author"], json!({"name": "octo"}));
        // An edited version may keep {link} as the title's link.
        let own = json!({"content": "", "embeds": [{"title": "New: {title}", "url": "{link}"}]});
        assert!(missing_text("feed.github.push", &own).is_none());
        let lost = json!({"content": "", "embeds": [{"title": "New: {title}"}]});
        assert!(missing_text("feed.github.push", &lost).is_some());
        assert_eq!(GITHUB_EVENTS.len(), 17);
    }

    #[test]
    fn text_is_required_in_the_first_embed() {
        for d in defs() {
            if d.key.starts_with("feed.") {
                assert_eq!(d.required, ["link"], "{}", d.key);
                continue;
            }
            let has = d.template.to_string().contains("{text}");
            assert!(missing_text(&d.key, &d.template).is_none(), "{}", d.key);
            if d.strict() && !d.required.contains(&"text") {
                // Security texts and moderation logs with their own facts (`{code}`, `{name}` and `{page}`, `{id}`).
                assert_eq!(d.required.is_empty(), d.what.is_empty(), "{}", d.key);
                continue;
            }
            let text_only = d
                .required
                .iter()
                .filter(|r| **r != "title")
                .collect::<Vec<_>>();
            assert_eq!(text_only == [&"text"], has, "{}", d.key);
            assert!(!d.important.contains(&"text"), "{}", d.key);
            assert_eq!(d.what.is_empty(), !has, "{}", d.key);
            assert!(missing_text(&d.key, &d.template).is_none(), "{}", d.key);
        }
        let ok = json!({"embeds": [{"fields": [{"name": "n", "value": "Got: {text}"}]}]});
        assert!(missing_text("ticket.reply", &ok).is_none());
        for bad in [
            json!({"content": "{text}", "embeds": [{"description": "x"}]}),
            json!({"embeds": [{"description": "x"}, {"description": "{text}"}]}),
            json!({"embeds": [{"description": "x", "url": "https://a/{text}"}]}),
        ] {
            let why = missing_text("ticket.reply", &bad).expect("refused");
            assert!(
                why.contains("{text}") && why.contains("staff member's reply"),
                "{why}"
            );
            // A stored copy without it sends the default: the reply is never lost.
            let m = render("ticket.reply", Some(&bad), &[("text", "hello")]);
            assert_eq!(m["embeds"][0]["description"], "hello");
        }
        assert!(
            missing_text("dm.punish.ban", &json!({"embeds": [{"description": "x"}]})).is_none()
        );
        assert!(missing_text("no.such", &json!({})).is_none());
    }

    #[test]
    fn stored_copy_wins_and_checks_work() {
        let stored = json!({"content": "", "embeds": [{"description": "Hi {nope}"}]});
        assert_eq!(unknown_vars("dm.punish.ban", &stored), vec!["nope"]);
        assert_eq!(dropped("dm.punish.ban", &stored), vec!["reason"]);
        let m = render("dm.punish.ban", Some(&stored), &[("reason", "r")]);
        assert_eq!(m["embeds"][0]["description"], "Hi {nope}");
    }

    #[test]
    fn long_text_is_cut_and_a_bare_label_line_drops() {
        let long = "x".repeat(6000);
        let m = render(
            "dm.punish.ban",
            None,
            &[
                ("server", "S"),
                ("reason", &long),
                ("moderator", ""),
                ("moderator_code", ""),
                ("duration", ""),
                ("staff_line", ""),
            ],
        );
        assert!(
            m["embeds"][0]["description"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= 4096
        );
        let m = render("dm.automod_timeout", None, &[("why", ""), ("note", "")]);
        assert_eq!(
            m["embeds"][0]["description"],
            "⏳ You were timed out by automod"
        );
        let m = render(
            "dm.punish.ban",
            None,
            &[
                ("server", "{moderator}"),
                ("reason", "{staff_line}"),
                ("moderator", "m"),
                ("moderator_code", "`m`"),
                ("duration", ""),
                ("staff_line", ""),
            ],
        );
        assert!(m["embeds"][0]["description"]
            .as_str()
            .unwrap()
            .contains("{staff_line}"));
        let bad = json!({"content": "", "embeds": [1, "a"]});
        assert!(
            render("dm.punish.ban", Some(&bad), &[("server", "S")])["embeds"][0]["title"]
                .is_string()
        );
    }

    /// A key typed in code that has no default would panic at send time.
    #[test]
    fn every_key_used_in_code_exists() {
        fn walk(dir: &std::path::Path, keys: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, keys);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("botmsg.rs") {
                    let s = std::fs::read_to_string(&p).unwrap();
                    for pre in ["dm.", "ticket.", "verify.", "welcome.", "reply."] {
                        for part in s.split(&format!("\"{pre}")).skip(1) {
                            if let Some((k, _)) = part.split_once('"') {
                                if k.bytes()
                                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.')
                                {
                                    keys.push(format!("{pre}{k}"));
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut keys = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."),
            &mut keys,
        );
        assert!(keys.len() > 15, "scan found {}", keys.len());
        for k in keys {
            assert!(find(&k).is_some(), "no default for {k}");
        }
    }

    /// Short replies: the default is today's sentence as message text, values are variables, and
    /// every `reply.*` entry is sent by some code (a dead entry would sit on the page for nothing).
    #[test]
    fn short_replies_are_sent_by_code_and_keep_their_words() {
        fn walk(dir: &std::path::Path, src: &mut String) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, src);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("botmsg.rs") {
                    src.push_str(&std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        let mut src = String::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."),
            &mut src,
        );
        let replies: Vec<_> = defs()
            .iter()
            .filter(|d| d.key.starts_with("reply."))
            .collect();
        assert!(replies.len() >= 14);
        for d in &replies {
            // A settings error's Security copy is sent by the code that sends its entry.
            let sent = match d.key.strip_suffix("_security") {
                Some(base) if SETTINGS_ERRORS.iter().any(|e| e.0 == base) => base,
                _ => d.key.as_str(),
            };
            assert!(src.contains(&format!("\"{sent}\"")), "dead entry {}", d.key);
            // A ticket reply or a lock notice is one embed, the rest is text. Only the notices
            // that carry the bot's list in `{text}` require it.
            let notice = d.key.ends_with("_notice") || d.key == "reply.confirm.question";
            // The usage line and the wrong use reply carry the command's own text.
            let line = [
                "reply.error.usage",
                "reply.error.wrong_use",
                "reply.ticket.usage",
            ]
            .contains(&d.key.as_str());
            let menu_text = DM_MENU_TEXTS
                .iter()
                .any(|t| d.key == format!("reply.dm.{}", t.0));
            let ticket = d.key.starts_with("reply.ticket.")
                || (d.key.starts_with("reply.dm.") && !menu_text);
            let one_embed = [
                "reply.report.sent",
                "reply.catch.warning",
                "reply.mod.result",
            ]
            .contains(&d.key.as_str())
                // The lock command answers  are one embed each.
                || (d.key.starts_with("reply.lock.") && d.details())
                || ANSWERS.contains(&d.key.as_str())
                || WHITELIST_ANSWERS.contains(&d.key.as_str())
                || panel(&d.key).is_some();
            assert_eq!(d.required == ["text"], notice || line, "{}", d.key);
            // The whitelist command is a security command, so its replies are security texts, as
            // the security panels and their setting changed reply.
            let security_panel = PANELS.iter().any(|p| p.0 == d.key);
            assert_eq!(
                d.security,
                d.key.starts_with("reply.whitelist.")
                    || security_panel
                    || d.key == "reply.settings.changed_security"
                    || sent != d.key
                    || SECURITY_ERRORS.iter().any(|e| e.0 == d.key),
                "{}",
                d.key
            );
            assert!(!d.locked, "{}", d.key);
            assert_eq!(
                d.template["embeds"] != json!([]),
                ticket || notice || one_embed,
                "{}",
                d.key
            );
        }
        let say =
            |k, v: &[(&str, &str)]| render(k, None, v)["content"].as_str().unwrap().to_string();
        assert_eq!(
            say("reply.verify.already_verified", &[]),
            "You are already verified."
        );
        assert_eq!(
            say("reply.verify.posted_button", &[("channel", "<#5>")]),
            "Posted the verify button in <#5>"
        );
        let ticket = render("reply.ticket.user_not_found", None, &[("prefix", "t!")]);
        assert_eq!(
            ticket["embeds"][0]["description"],
            "The user was not found. Use `t!close [reason]` to close this channel."
        );
        assert_eq!(ticket["embeds"][0]["color"], RED);
        assert_eq!(
            render("reply.ticket.opened", None, &[("channel", "<#5>")])["embeds"][0]["color"],
            BLUE
        );
        // P6b: ticket command results keep the old words, with the values in place.
        let desc = |k, v: &[(&str, &str)]| render(k, None, v)["embeds"][0]["description"].clone();
        assert_eq!(
            desc("reply.ticket.claimed_other", &[("user", "<@5>")]),
            "<@5> has already claimed this ticket."
        );
        assert_eq!(
            desc(
                "reply.ticket.shown_type_changed",
                &[("user", "<@5>"), ("old_type", "A"), ("type", "B")]
            ),
            "<@5> changed the ticket type from **A** to **B**."
        );
        assert!(desc("reply.ticket.set_up", &[("prefix", "t!")])
            .as_str()
            .unwrap()
            .ends_with("configurations with `t!help`."));
        assert_eq!(
            say("reply.closeall.partly_anon", &[("closed", "1"), ("total", "3"), ("left", "2")]),
            "Closed 1 of 3 open tickets anonymously. The other 2 could not be closed; see the bot log, then try again."
        );
        let n = [("closed", "1"), ("total", "3"), ("left", "2")];
        assert_eq!(
            say("reply.closeall.done", &n),
            "All 3 open tickets are closed."
        );
        assert_eq!(
            say("reply.closeall.done_anon", &n),
            "All 3 open tickets are closed anonymously."
        );
        assert!(say("reply.closeall.partly", &n)
            .starts_with("Closed 1 of 3 open tickets. The other 2 could"));
        // P7: errors, usage lines and permit refusals keep the old words.
        assert_eq!(
            say("reply.error.usage", &[("text", "`backups [ID]`")]),
            "Usage: `backups [ID]`"
        );
        assert_eq!(
            say("reply.error.wrong_use", &[("text", "missing user")]),
            "missing user"
        );
        assert_eq!(
            say("reply.error.no_permit", &[("permit", "ban")]),
            "You lack the ban permit."
        );
        assert_eq!(
            desc("reply.ticket.admin_only", &[("command", "t!logging")]),
            "Only the server owner, Extra Owners and Trusted Admins can use `t!logging`."
        );
        assert_eq!(
            desc(
                "reply.ticket.snippets_full",
                &[("count", "100"), ("cap", "100")]
            ),
            "This server has 100 snippets, the most allowed is 100. Remove one first."
        );
        assert_eq!(
            desc("reply.ticket.usage", &[("text", "`t!reassign <member>`")]),
            "Usage: `t!reassign <member>`"
        );
        // An edited usage line without `{text}` sends the default, never a reply without the line.
        let lost = json!({"content": "Usage:", "embeds": []});
        assert_eq!(
            render("reply.error.usage", Some(&lost), &[("text", "`x`")])["content"],
            "Usage: `x`"
        );
        let paused = render("ticket.paused", None, &[("reason", "Busy")]);
        assert_eq!(paused["embeds"][0]["title"], "⛔ Ticket Creation Disabled");
        assert_eq!(paused["embeds"][0]["description"], "Busy");
        assert_eq!(
            say("reply.tag.not_found", &[("tag", "rules")]),
            "No message has the tag “rules”."
        );
        assert_eq!(
            say("reply.case.proofs_full", &[("case", "4"), ("count", "10")]),
            "Case #4 already has 10 proofs"
        );
        assert_eq!(
            say("reply.note.not_found", &[("id", "q413p9")]),
            "No note `q413p9`"
        );
        assert_eq!(
            say("reply.case.no_proof", &[("case", "4")]),
            "Case #4 has no proof. Add one with `proof 4 <link or text>`"
        );
        assert_eq!(
            render(
                "reply.whitelist.no_permit",
                Some(&json!({"content": "x"})),
                &[]
            )["content"],
            "x"
        );
        assert_eq!(
            say("reply.tag.queued", &[("tag", "a{b}")]),
            "“a{b}” is queued: Brim posts it when Discord answers."
        );
    }

    #[test]
    fn every_default_uses_only_its_variables() {
        for d in defs() {
            assert!(unknown_vars(&d.key, &d.template).is_empty(), "{}", d.key);
        }
    }

    /// A default never fails its own save check (the editor showed "{link} is missing" on the
    /// untouched GitHub default).
    #[test]
    fn every_default_passes_its_own_check() {
        for d in defs() {
            assert_eq!(missing_text(&d.key, &d.template), None, "{}", d.key);
        }
    }

    /// The editor shows a name and an example for each variable.
    #[test]
    fn every_variable_has_a_name_and_example() {
        for d in defs() {
            for v in &d.vars {
                assert!(!var_info(&d.key, v).0.is_empty(), "{} {v}", d.key);
            }
        }
        let m = json!({"embeds": [{"description": "{server.name} {role:12}"}]});
        assert_eq!(
            unknown_vars("dm.punish.ban", &m),
            ["role:12", "server.name"]
        );
    }

    /// A copy that is only an empty variable must not send a bare embed: it falls back.
    #[test]
    fn an_embed_left_empty_falls_back_to_the_default() {
        let stored =
            json!({"content": "", "embeds": [{"color": 1, "description": "{moderator_code}"}]});
        let m = render(
            "dm.punish.ban",
            Some(&stored),
            &[("server", "S"), ("moderator_code", "")],
        );
        assert_eq!(m["embeds"][0]["title"], "You have been banned in S!");
    }

    /// Every log type has its own entry, and each title is one the code really sends: an audit
    /// title (from the event name) or a literal in the source. A typo would leave a dead entry.
    #[test]
    fn every_log_type_title_is_sent_by_code() {
        use crate::LogEvent;
        fn walk(dir: &std::path::Path, src: &mut String) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, src);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("botmsg.rs") {
                    src.push_str(&std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        let mut src = String::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."),
            &mut src,
        );
        let audit: Vec<String> = LogEvent::ALL
            .iter()
            // The gateway ones have their own titles; so do the audit ones listed here.
            .filter(|e| {
                !matches!(
                    e,
                    LogEvent::MessageDelete
                        | LogEvent::MessageDeleteBulk
                        | LogEvent::MessageUpdate
                        // Their titles are literals in `render::audit`; the timeout end is a job.
                        | LogEvent::AutoModRuleCreate
                        | LogEvent::AutoModRuleUpdate
                        | LogEvent::AutoModRuleDelete
                        | LogEvent::AppRemove
                        | LogEvent::AppCommandPermissionUpdate
                        | LogEvent::MemberTimeoutEnd
                        // Gateway logs with their own titles, and the stage titles in `render::audit`.
                        | LogEvent::PollCreate
                        | LogEvent::PollEnd
                        | LogEvent::PollVoteAdd
                        | LogEvent::PollVoteRemove
                        | LogEvent::MessagePublish
                        | LogEvent::GuildScheduledEventSubscribe
                        | LogEvent::GuildScheduledEventUnsubscribe
                        | LogEvent::StageStart
                        | LogEvent::StageUpdate
                        | LogEvent::StageEnd
                        | LogEvent::BoostLevelChange
                        | LogEvent::ThreadAutoArchive
                        // Built by the bot from the stored last seen name and avatar.
                        | LogEvent::UserAvatarUpdate
                        | LogEvent::UserNameUpdate
                )
            })
            .map(|e| crate::render::words(&format!("{e:?}")))
            .collect();
        let all: Vec<&str> = LOG_TYPES.iter().flat_map(|t| t.1.iter().copied()).collect();
        for a in &audit {
            assert!(all.contains(&a.as_str()), "audit log {a} has no entry");
        }
        for (kind, titles) in LOG_TYPES {
            assert!(LOGS.iter().any(|l| l.0 == *kind), "{kind}");
            assert!(!matches!(*kind, "anti_nuke" | "panic" | "join_raid"));
            for t in *titles {
                assert!(
                    audit.contains(&t.to_string()) || src.contains(&format!("\"{t}\"")),
                    "no log sends the title {t}"
                );
                let key = log_type_key(&format!("log.{kind}"), t).unwrap();
                assert!(find(&key).is_some_and(|d| !d.locked), "{key}");
            }
        }
        let mut keys: Vec<&str> = defs().iter().map(|d| d.key.as_str()).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "a log key is used twice");
        assert!(log_type_key("log.message", "Message Deleted").is_some());
        assert_eq!(
            log_type_key("log.message", "Message Deleted").unwrap(),
            "log.message.message_deleted"
        );
        assert!(log_type_key("log.message", "Alex's Info:").is_none());
        assert!(log_type_key("log.anti_nuke", "Anti Nuke").is_none());
    }
}

#[cfg(test)]
mod bot_wide_tests {
    use super::*;

    #[test]
    fn outside_server_entries_keep_one_copy_under_guild_zero() {
        assert_eq!(copy_guild("dm.raider_notify", 42), 0);
        assert_eq!(copy_guild("dm.data_request", 42), 0);
        assert_eq!(copy_guild("dm.invite_link", 42), 42);
    }

    #[test]
    fn every_outside_server_entry_is_a_default() {
        for k in BOT_WIDE {
            assert!(find(k).is_some(), "{k}");
        }
    }
}
