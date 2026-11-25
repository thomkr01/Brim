# Pitfalls: Discord API

Index and rules: [PITFALLS.md](../PITFALLS.md).

**Three lists said which Discord permissions are dangerous, and they disagreed.**
Anti-nuke, verification, quarantine, backup restore and the security score read `DANGEROUS_PERMS` (9 permissions); the role lock had its own list of 15; `info` had its own names. A role with View Audit Log, Manage Nicknames, Manage Expressions, Manage Events, Manage Threads and Posts or Mention @everyone was taken by a lock but not guarded by anti-nuke. *Avoid:* `brim_antinuke::DANGEROUS_PERMS` is the one list and `PERM_NAMES` its names; the role lock's `DANGEROUS` is that constant; `/tag dangerous-permissions` lists it. A new feature that judges a permission dangerous reads it, never its own list. One exception: Mention @everyone on a staff permit role (Auto Setup gives it); use `STAFF_DANGEROUS` for those roles (security score, giving a role).

**Lookups ran once per item.**
`unban a b c` read the whole ban list (up to 50 pages) for each name, and a purge with `role`/`norole` read one member at a time, also for webhook authors. *Avoid:* read a list once per command (`targets::ban_names`) and read members a few at a time (`buffer_unordered(5)`), once each. An author who left counts as having no role, so `norole` matches them; a failed read is unknown and matches no role filter (`purge::known_roles`).

**Every prefixed message fetched its channel from Discord.**
`commands::ignored` asked for the channel kind over HTTP on each message, and a bad body skipped automod. *Avoid:* `GuildInfo.voice` holds the cached voice and stage channels; HTTP only for a channel the cache does not know (not a known thread), and any failure counts as not voice.

**A click or a DM paid REST calls per server or per member page with no cache.**
The DM server menu asked Discord about every ticket server on each open and each "Other server" press, and a message menu's `role.member_count` paged up to 10 000 members on every click. *Avoid:* cache such answers per user or per server for a minute (`dm::shared`, `msgmenu::role_counts`), give every button that starts the work the same cooldown (`tk:other`), and never cache an answer built while a lookup failed. Do not cap the lookups (a cap of 50 servers locked out members of every newer server).

**A bot's managed role cannot be removed.**
Quarantining a nuking bot failed, because Discord does not let anyone take a bot's own managed role away, and that role kept Administrator. The same hole was in `quarantine_actor` (verification channel delete, command rate limit), the mod `/quarantine` command and the Raider List's Quarantine action, which skipped the shared path.
*Avoid:* every place that contains an actor goes through `antinuke::contain`. It sets the managed role's permissions to 0, then quarantines, and kicks only as a fallback ("Kicked" when the fallback ran). Never write a second quarantine path. Verification's join and time-limit quarantines call `quarantine_as` directly because `verify::on_join` skips bots.

**A role list without the member's managed roles is refused.**
Discord refuses a role list that leaves out a managed role (Server Booster, an integration's role), and refuses to remove one. The quarantine hold set a held member to `[quarantine]` and verification replaced roles with the verification roles, so both failed for a booster; because the hold used `?`, the actor who touched the member was then never quarantined. "Enforce permits" tried to remove a dangerous managed role and stopped at the error, so no log was sent.
*Avoid:* build every full role list with `antinuke::quarantine_roles` (it keeps the managed roles) or add the member's managed roles yourself, and never remove a managed role on purpose: skip `GuildInfo::managed`. Covered: `hold` (now `back_to_quarantine`), `verify::grant`, `enforce_permits`. `Inverse::RestoreRoles` and Unquarantine restore saved lists, which held the managed roles when they were saved.

**Role order decides everything.**
A live nuke test got 403 because Brim's role was tied with the attacker's. A role between the bot and the quarantine role also breaks quarantine.
*Avoid:* the bot role must sit above every role it acts on, and the quarantine role right below the bot. The security score checks the second one (Move role fix). When a live test gets 403, check the role order first.

**Tied role positions were compared as equal.**
On a server whose roles were never reordered, every role sits at position 1. Discord breaks the tie by id, with the older role ranking higher, but Brim compared only the numbers. So `/kick` and `/ban` refused any target with a role, anti-nuke would not strip Admins from a nuker, lockdown and `tshoot lockdown` skipped every role, the security score read them all as level with the bot, and Auto Setup never moved the quarantine role. `security.rs` already ranked ties by id, but other code called it with raw positions.
*Avoid:* rank roles once where they are read from Discord: `hierarchy::ranked` in the bot, `ops::roles_of` and `recovery::live_image` in the dashboard, all built on `brim_core::roles::ranks`. Every role list that is compared or written back goes through one of them. Never compare `Role::position` straight from Discord.

**Member edits share one rate-limit bucket.**
Nick changes, role adds, timeouts and kicks all use Discord's member bucket, so a burst of automatic renames or role restores slows the punishments of a raid.
*Avoid:* bulk automatic member edits are capped per guild (`dehoist::rename`, 10 a minute through `RateLimiter`) and auto-dehoist is off during a join raid or lockdown.

**Not every member lookup error means "not a member".**
All `guild_member` errors (5xx, timeouts) were read as "left the server". Staff then lost their permit level for that event, so automod or anti-nuke could act on them.
*Avoid:* use `http_err::member`. Only a 404 means not a member. Other errors propagate.

**Swallowed errors hide missing permissions.**
`.await.ok` dropped real failures, so a punishment Discord refused left no trace.
*Avoid:* use `http_err::best_effort`. It stays quiet only for 404 and closed DMs (50007) and logs everything else. Failed automod actions go to the log with `action_failed`.

**Bulk delete takes at most 100 messages.**
More than 100 gave a 400, nothing was deleted, and the error was swallowed.
*Avoid:* split with `reply::batches` (100 per call, 14-day cutoff, plain delete for one message).

**Outgoing HTTP needs a timeout.**
The raw reqwest client had none, so one hung call could hold a lockdown or rule sync forever. The dashboard's and the MCP server's Discord clients had none either. Ticket attachment copies used a bare `reqwest::get`, so one hung download held the ticket reply.
*Avoid:* every client gets a timeout (15 s in `automod/src/discord_api.rs`, the dashboard's and the MCP's Discord client); ticket attachment copies use the bot's shared client (`Api::client`). Twilight already handles 429. Never call `reqwest::get` or `Client::new` outside tests.

**twilight 0.18 has no soundboard audit types.**
Types 130-132 arrive as `AuditLogEventType::Unknown(n)`. Matching `Unknown(132)` would silently stop matching once a later twilight names the variant.
*Avoid:* match a type twilight does not name by its number (`t if u16::from(t) == 132`), as `antinuke::watched` does.

**Voice disconnects and moves are merged into one audit entry.**
Discord merges `MEMBER_DISCONNECT` (27) and `MEMBER_MOVE` (26) by the same actor in quick succession into the first entry and raises its `options.count`, and those updates fire no gateway event. Counting gateway entries sees 1 for a mass disconnect of 20.
*Avoid:* count the growth of `count`, not entries. `voice_nuke` watches a guild for 10 minutes after the first entry and rereads the audit log on voice updates (one fetch at a time, at most one per 3 s, plus a trailing fetch so the end of a burst counts). The last seen count per entry is read and stored under one lock, entries older than the bot are seeded at their count (a restart does not recount them), `count` is a string parsed to u32 and capped at 500, and 26/27 are counted only there, never also through `watched`. Message deletes merge the same way (see "Discord's own rendering quirks").
A fetch can be the first sight of an old entry, so "first sight" is not "new": growth from while the guild was unwatched (a moderator moving 12 members over minutes) landed as one lump and quarantined them. A fetch seeds an unseen entry made before the current watch began (minus 5 s slack, `State::seed_before`); a gateway entry (`note`) is new by definition and counts from its count.

**A server icon is downloaded from a stored hash.**
The icon revert fetches the old icon by the hash in `action_log.inverse`, which anyone with database or revert access could shape into another path.
*Avoid:* build the URL only with `brim_antinuke::icon_url` (`a_` + 32 lowercase hex, fixed CDN host) and fetch through `discord_api::icon_data_uri`: client with a timeout, no token, refuse a redirect off the CDN, `image/*` only, 10 MB cap while reading. Bot and dashboard share it.

**The old server icon is gone from the CDN.**
The icon revert never worked live: Discord's CDN answers 404 for an icon hash the moment the server icon changes, so the report said "Could not undo: Put the server icon back". Root cause: the design assumed the CDN keeps old hashes. *Avoid:* Brim keeps its own copy (`guild_icons`, migration 3929): `welcome::save_icon` saves the current icon at GuildCreate and on every GuildUpdate, and a backup saves the icon it names. The anti-nuke revert (bot and dashboard) and the backup restore read the copy first through `discord_api::stored_icon` and fall back to the CDN only for the live icon. Kept per server: the newest 3, every icon a backup names and every icon an anti-nuke revert row (`action_log`) names, so an attacker's many changes cannot push out the icon from before the attack; the purge removes the rest with the guild. The save runs in the background so a slow CDN does not hold up GuildCreate, and a restore whose icon copy is gone says so. A backup without an icon never removes the live one. The bot revert, the dashboard revert center and the backup restore are the only places that set a server icon.

**A bot cannot delete another app's integration.**
`DELETE /guilds/{id}/integrations/{id}` with a bot token answers 403 Missing Access (50001), even with Administrator. The anti-nuke undo "Remove the new app" never worked.
*Avoid:* `brim_antinuke::app_removal` looks the integration up and kicks the app's bot user, which removes the app. An app already gone counts as undone. An app with no bot user can only be removed by a person; the report says where (Server Settings > Integrations). Only `antinuke::remove_app` and the dashboard `ops::run_inverse` remove an app.

**A REST call before an interaction's answer can miss the 3 seconds.**
Discord drops an interaction answer after 3 s. The verify Start button asked Discord for the spammer flag on every click through `Api::send`, which waits out a 429 up to 30 s and retries (only the 15 s client timeout limited it): "This interaction failed", and one request per click on one bucket. `verify::guild_name` had no limit either.
*Avoid:* do the work that needs no REST first; call only when the answer changes the result (`flagged_step`); cache the answer (`App::spam_flags`, 10 minutes, bounded); bound the call: one try with no 429 wait (`Api::send_tries(.., 1)`) inside `tokio::time::timeout(1.5 s)`. Decide what a timeout means (here: a captcha, fail closed). Anything slower defers the interaction first.

**A DM can be slow or refused.**
A DM before a punishment must not hold the punishment back; a DM to a bot is refused.
*Avoid:* send DMs through the shared helper with the `DM_WAIT` limit (3 s). The punishment runs anyway. A DM that times out is logged.

**Discord's own rendering quirks.**
A mention of the @everyone role renders as `@@everyone`: write plain `@everyone`. Discord lists each mentioned user once per message, so mention spam counts one ping per user per message. Discord merges audit log entries, so the bot's deletion of someone else's message can show no Responsible.

**A saved id can point at something deleted in Discord.**
Config keeps ids after the role or channel is gone (deleted by hand, backup restore). Auto Setup answered "internal error": the saved Quarantine role was deleted, the PATCH got 404, the whole setup stopped. With the role deleted, each quarantine PATCHed a list holding the dead id, Discord refused it, and no human could be quarantined.
*Avoid:* treat a 404 on a saved id as "missing" and make it again (`sections::create` checks an existing id first); never let a 404 on reuse fail a whole setup. `quarantine_as` reads the live roles, leaves a missing quarantine role out (the member is stripped to their managed roles) and posts "Quarantine role missing" in the anti-nuke log. The hold and the live containment check use the same `quarantine_parts`.
Other half: a revert, panic restore or backup load makes the object again with a new id while the config names the old one, and a deleted log channel was already dropped from `logging` (`forget_log_channel`), so logs, catch, tickets and the quarantine role stay broken.
*Avoid:* every restore passes its old-to-new id map to `relink::relink` (`backup::recreate`, `backup::restore`). It swaps the id everywhere in the config and in `permit_roles` as one config version, and puts back the log routes of the version just before the deletion (the version before the bot's `forget_log_channel` version, never an older one, so a route staff removed on purpose does not come back; a slot that version had empty stays empty). The lookup is per deleted channel (`forget_log_channel` writes one version per channel). `relink` takes the image and looks up only ids that were channels in it (`relink::channels`); the `permit_roles` swap covers every id.

**A saved role id must be one the bot can use.**
The quarantine role and main roles took any role id: @everyone, a managed role, one above the bot. Auto Setup then set the "quarantine role" to no permissions, which for @everyone strips every member, and `t!setup` denied it in every channel.
*Avoid:* a role the bot edits or hands out passes `brim_core::roles::unfit` (not @everyone, not managed, below the bot's top role) wherever it is saved: dashboard `put_section` (`check_held_roles`, only ids the save adds), the bot's settings panel for main roles (`GuildInfo::unfit`), Auto Setup and `t!setup` (make a new Quarantine role instead of using an unfit saved one). Not covered: config import and History rollback write the stored JSON as is.

**A Discord outage is not a refusal.**
`commands::apply_dm` turned every failed Discord call, a 502 too, into a refusal reply. A job that bans through it (Ban Sync copy) marked the row skipped and never retried. The error arm did not tell "Discord said no" (4xx) from "Discord did not answer" (5xx, timeout).
*Avoid:* only a 4xx is a refusal (`http_err::refused`); anything else returns `Err`, so jobs retry and commands show "Something went wrong". Other `http_err::why` uses are fine: `confirm`, `reports`, `appeals`, `slash`, `tickets::dm` answer a person who can press again; `verify::deleted` sends the failure to the owner; `joingate::punish` returns the error after its log, so the job retries.
Join and voice raid punishments only logged the error in a loop, so a 5xx left the raider in. They go through `joingate::punish_or_retry`, which queues a `raid_punish` job for anything not a refusal. A retry is safe only when the error means the action did not happen: `joingate::finish` returned its Join Gate log's error, so a ban that worked plus a failed log was punished again. `finish` only warns now; Warn (case written first) is never retried; the job skips whoever is immune or allowed by then; `raiders::allow` (every unban, ban appeal, Allow) drops a pending retry.

**Raw requests need a real User-Agent.**
Without `User-Agent: DiscordBot (...)` Cloudflare answers 403 / error 1010.
*Avoid:* every reqwest client that talks to Discord (bot, dashboard, MCP) sets `DiscordBot (<repo url>, <version>)`.

**Twilight drops permission and user flag bits it does not know.**
Twilight 0.18 deserialized `Permissions` with `from_bits_truncate`, so every role and overwrite the bot read lost unknown bits and every read-modify-write sent them back without them. Live, one role lock and unlock took 1<<47 and 1<<48 (Set Voice Channel Status) off @everyone, Member, Admins and Moderators, with nothing logged. The snapshot was truncated at the read, so `from_bits_retain` at the write alone would not help. Hit: panic's lock, channel locks, backups restore, anti-nuke `Inverse::RolePerms` and bot-role record, permission undo, `general` role tweaks, panic's category exempt.
*Avoid:* the workspace patches twilight-model (`vendor/twilight-model`, `[patch.crates-io]` in `Cargo.toml`) so `Permissions` deserializes with `from_bits_retain`; re-apply that one line on every twilight upgrade. `exclude = ["vendor"]` keeps the vendored tests out of the workspace run. A `[patch]` left at an old version is ignored with only a warning, so the guard is `lockdown::tests::twilight_keeps_unknown_permission_bits` (no DB). Turn a stored u64 back into `Permissions` with `from_bits_retain` before writing it to Discord; `from_bits_truncate` only for reading names (`render::perm_names`), checking known bits (`setup`, `events::public`) and constants. Exact u64 shape checks (`verification_shape`) now see unknown bits too, which is right. The dashboard uses raw strings, not affected. Test: `a_lock_and_unlock_keep_unknown_permission_bits`.
User flags too: `UserFlags` is read with `from_bits_truncate`, so `public_flags` never holds Discord's undocumented spammer bit (`1 << 20`).
*Avoid:* read such a bit from the raw gateway JSON before twilight parses it, like `spam_flag` in `crates/bot/src/main.rs` (the loop reads raw messages instead of `next_event`).

**Discord answers 200 and keeps the old image.**
`PATCH /guilds/{g}/members/@me` with avatar data Discord cannot use (`data:image/png;base64,AAAA`) answers 200 with the old avatar.
*Avoid:* after setting an image, check that the answer carries an image hash; no hash is "rejected" (`profile::apply`). The member object has no `bio` (only the PATCH answer has it), so the dashboard shows the bio from its own last accepted change, unless the bot rejoined since or the last change got no answer.

**Member role adds share Discord's rate-limit bucket with punishments.**
A mass rejoin giving every member's roles back would fill the per-guild member bucket that timeouts, kicks and bans need right then.
*Avoid:* bulk role or nickname work on members gets a per-guild cap (`sticky::take`, 20 adds a minute) and queues the rest as a job instead of dropping it.

**A gateway event's value can be older than the bot's own write.**
Events run in parallel tasks and Discord sends a channel update for every edit (a rename too), so a ChannelUpdate handled after the bot's own PATCH can still carry the old slowmode. Auto slowmode read that as "staff changed it" and put it back, undoing its own slowmode.
*Avoid:* don't decide from an event field when the bot itself writes that field. Read the live value (`GET channel`) and compare with both the old and the bot's value (`slowmode::staff_value`), or skip the event when a later compare-and-set already handles it (the off job reverts only its own value). `enforce_quarantine` and `catch::keep_top` act on event values too, but their writes are idempotent, so a stale event only repeats them.

**Renaming the bot would have made a second Discord AutoMod rule.**
The bot finds its own keyword rule in Discord by its name ("TommyBot keywords"). With only the name changed, the next sync finds no rule, makes a new one in every server, and the old rule keeps blocking the old word list. *Avoid:* anything the bot finds by a name it gave (Discord AutoMod rules, channels, roles, webhooks) also matches the old name after a rename (`discord_api::is_keyword_rule`), and the sync writes the new name. Prefer a stored id over a name. Old exported files keep their internal ids (message kit `tommybot-messages`) so they still import.

**Id 0 parsed as a target.**
The prefix parser and `info` accepted `0` and `<@0>`, which reached Discord as a request for a user that cannot exist. *Avoid:* both parsers drop id 0. A test covers users, roles and channels.

**Byte slicing panics on non-ASCII.**
Duration parsing sliced off the last character by byte and panicked on `5å`; converting large values to milliseconds overflowed. *Avoid:* split on `char` boundaries (`chars`, `char_indices`), use checked or saturating maths. Never `&s[..n]` on user input.

**`Id::new` panics on 0.**
Join Raid read its Warned Roles (strings in the config) with `r.parse.ok.map(Id::new)`, so a stored `"0"` (import or hand edit) panicked the join task before the raid log and punishments. A parsed number is not a valid snowflake. *Avoid:* turn parsed or stored numbers into ids with `Id::new_checked` and skip `None`. Both raid triggers use `joingate::warned_roles`; `verify::guild_of` (guild from a button's custom id) also uses `new_checked`. Other `Id::new` calls take ids Discord or the database already gave.

- **A filter lived in one caller, not in the shared lookup.** Roles Discord hands out were dropped only in `App::level`, so `permits::resolve` and `top_permit` called directly (enforce permits, Raider List, dehoist, mod commands, voice raid, the dashboard's permit lookup) still gave their permit. *Avoid:* put a rule about which roles count into `permits::top_permit` (here the `unpermittable_roles` table), which every reader goes through.

- **A webhook token in a logged URL.** A webhook path holds its token, and reqwest errors print the URL. *Avoid:* map request errors with `without_url` for any path that holds a secret.

- **A 429 retry waited too short and too long at once.** `Discord::send` clamped each Retry-After to 10 s and tried 3 times, so a longer limit failed after 30 s anyway, and the Overview reload after a Fix hung, then showed a 500 with the old list. *Avoid:* a retry waits the time Discord asks for, within a total budget (15 s); a longer wait returns the 429 at once, so the page says "try again in a minute". A best effort step (role order, catch channel) puts its failure in the step result, not only in the log, and keeps Discord's own reason for statuses that do not say it (a 400).

**The dashboard showed "Discord is limiting requests" on normal page loads and after "Fix all".** One page load starts several handlers at once (`/score`, `/score/fix`, `/discord`, `/lockdown/check`), and each fetched `GET /guilds/{id}/channels` and `/roles` fresh, so the same requests went out side by side. On a 429 they all waited the same Retry-After and retried together, hit the limit again and ran out of tries. Discord's `X-RateLimit-Remaining: 0` was ignored, and the 30 s memo (`memo_get`) and a few raw calls skipped the 429 handling. *Avoid:* every bot request goes through `Discord::send`, which waits when Discord said a path's bucket is used up or sent a 429 (shared per "METHOD /path" and for a global limit, 15 s in all). GETs through `call`, `get_json` and `memo_get` share one request when asked at once (`shared_get`) and reuse an answer for 2 s. Every write clears all reused answers and stops a GET under way from storing its answer, so data read after a fix is fresh. Tests: `discord::tests::parallel_gets_share_one_request_and_a_write_clears_them`, `a_used_up_bucket_makes_the_next_request_wait`. Not live tested.
