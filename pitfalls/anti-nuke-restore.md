# Pitfalls: Anti-nuke undo and restore

Index and rules: [PITFALLS.md](../PITFALLS.md).

**A remade log channel took old backup overwrites without saying so.**
`copy_overwrites` uses the newest backup (a nuker may have added a grant first), which also undoes a later tightening. *Avoid:* the "Log Channel Deleted" alert names every role or member whose overwrite differs between the backup and the deleted channel (`protected::differing`).

**A lock set-back used the audit entry's old value.**
`guard_lock` set a locked role or overwrite back to the entry's `old` value. Two quick edits (L -> L+Send -> L+Send+Embed) set the second back to L+Send, so members could write during a lock. *Avoid:* `lock_target` takes the target from the lock snapshots only, with no live read: a role gets its saved value minus every holding lock's `takes`; an overwrite gets what `lockdown::exec` wrote (`prior` allow minus SILENCE, `prior` deny plus SILENCE). The role remap after a restore already strips `takes`. On purpose: under an All lock a non-main staff role is set back without SILENCE too, as the restore path does. A set-back never adds a `DANGEROUS` bit the entry's old value lacked (`& !(DANGEROUS & !old)`): a channel-only lock takes none, and its saved value can hold Administrator that staff removed since. The same holds for an overwrite's allow, against the entry's old allow (empty when the entry has none).

**Anti-nuke logs said "undone" or gave a wrong reason.**
The vanity guard only warned on a failed PATCH (and undid nothing when there was no earlier code), `hold_overwrite` only warned on a failed undo and then logged "undone where it can be", and every failed nuke quarantine said "check that the bot's role is on top", also when the nuker had left or only the `action_log` row failed (which also made `contain_bot` kick a quarantined bot). *Avoid:* a failed undo goes into `undo` or its own failure log, and no "undone" log follows it. `Applied::Failed` carries the reason (left the server, missing permissions, see logs). `record_quarantine` is best effort once the role change worked. The hold's `unsure` branch uses `undo_role_change` (retry and log), not `?`.

**A restore said "Restored" when Discord refused steps.**
`backup::execute` only traced each refused step (and the role order call). *Avoid:* every step goes through `Summary::count`, which counts the failure and keeps the first reason; the load reply and the panic report show "N steps failed, first: <reason>". Webhook deletes and both position calls use it too.

**A restore dropped every member overwrite.**
`map_overwrites` kept only targets in `id_map` (roles, channels, the guild), so a recreated or updated channel lost its member overwrites (ticket access, member mutes, Brim's own allow). *Avoid:* only role targets are remapped; a member overwrite keeps its user id. But an image can hold a grant a nuker gave himself, and a member overwrite beats the quarantine role's deny. So `execute` runs `guard_members`: a quarantined member's or a departed user's overwrite is dropped, an Update keeps a quarantined member's live overwrite (the quarantine deny), and every member who is not immune (or Brim) loses `DANGEROUS_PERMS` from the allow. `protected::copy_overwrites` drops member overwrites for the same reason.

**Restore kept every channel named "server-locked" and put recreated channels at the bottom.**
A nuker could name spam channels `server-locked` to keep them through a panic restore, and created channels got no position. *Avoid:* keep the notice channel by the id the lock saved (`Snapshot.locked_channel`), never by name. After the channel loop, one `update_guild_channel_positions` call puts created channels at their image positions, as roles already were.

**Restore could delete the catch channel, and an older image gave back dangerous overwrites.**
`KEEP_FIELDS` lacked `catch.channel` and `catch.category`, so a restore before the next backup deleted the honeypot and its Information category. `newest_with` stripped `DANGEROUS_PERMS` from roles of an older image but not from channel overwrites. *Avoid:* every channel or role Auto Setup makes goes into `KEEP_FIELDS` and its test. A channel from an older image loses dangerous overwrite allows and counts in `stripped`, as a role does.

**With anti-nuke off, the hold logged an undo that never happened.**
A kick, ban or member edit of a quarantined member cannot be undone, but with `punish` false `hold` sent "undone where it can be" for them. *Avoid:* with anti-nuke off, `hold` returns without a log for `MemberUpdate`, `MemberKick` and `MemberBanAdd`. A log line must say only what the code did.

**A join-quarantine undo was silent with anti-nuke off.**
Roles given to an unverified (join-quarantined) member were taken back with no log, so staff never learned who gave them. *Avoid:* the `on_join` branch of `hold` logs who gave which roles and that nobody was punished when `punish` is false. Who is exempt did not change (a verify permit exemption is asked separately).

**A take-back log could flood the anti-nuke channel.**
A reaction-role or level bot that gives a permit role on every click made one take-back log per click. *Avoid:* `guard_permit_roles` logs one message per (guild, actor, role set) per 10 minutes (`take_back_log_due`); the next log says how many were taken back since. The take-back itself always runs. For a staff (guarded) permit the owner also gets one DM at the fifth take-back of the same (guild, actor, role set) (`owner_dm_due`, in memory); weak permits never DM.

**A late undo set back a newer change.**
`guard_perms` restored the entry's old permissions on a role or channel overwrite without looking at the current value, so a replayed or slow entry could undo a later, correct change. *Avoid:* before the undo it reads the role (`http.roles`) or the overwrite (the channel) live; when the current value is no longer the entry's new value it skips the undo and logs that. Anti-nuke on or off. `guard_lock` sets back to the lock's snapshot, which is meant to win, so it has no such check.

**Undo guards ran only with anti-nuke on.**
`on_audit_as` returned early when anti-nuke was off, before `guard_permit_roles`, `hold`, `hold_overwrite`, `guard_lock` and `guard_perms`. So a Discord Manage Roles holder could hand out a permit, a quarantined member could be given roles back, a lock or strict mode was not enforced, and a weak permit role was only logged. *Avoid:* `on_audit_as` runs every guard with `punish = cfg.antinuke.enabled`; with `punish` false a guard undoes and logs "Anti-nuke is off, so nobody was punished" instead of `quarantine_and_revert`, and only the counting stops. Rules that come from "only permits act" must not hang on a module switch. `handled_audit` records entries with anti-nuke off too, so a catch-up does not undo an entry twice. `verify::given::hold_takes_it` dropped its anti-nuke check with it, or verification and the hold would both act on one grant. Still gated on anti-nuke, correctly: `voice_nuke::fetch` (counting only).

**A nuke case counted failed undo steps as undone.**
`revert_actor` returned every planned step as `done`, also those Discord refused, and `report` stored it in `nuke_incidents.data.reverted`, so the MCP `nuke_incidents` view and the case showed more undone than real.
*Avoid:* a result list holds only steps that succeeded; `revert_actor` drops failed ones from `done` (test: `a_failed_revert_step_stays_for_the_next_run`). The dashboard revert counts `done` only after a step succeeds.

**A re-invite list had no length cap.**
The anti-nuke breach log and `/revert` reply listed every kicked member; a large nuke passed Discord's embed or message limit and was refused.
*Avoid:* lists of users in a log or reply go through `antinuke::capped` (10, then "and N more"). The nuke report and owner DM already use it.

**Layout Guard took open ticket channels as part of the server layout.**
Each open ticket showed as "Made in Discord, not in the layout", and "Save again" wrote it into the layout, so the guard reported it deleted once the ticket closed. A layout image was the whole live channel list; Brim's temporary ticket channels were not told apart.
*Avoid:* `layout::temp_channels` reads Brim's ticket records and the held lock's `locked_channel` (the lockdown's `#server-locked`) by channel id (one query, never name or category, so nobody can hide a channel from the guard with a ticket-like name or by moving it into the ticket category). Open ones are left out of the live side of the compare, of every save, push and roll back; the saved side drops any channel a ticket record names, so an old layout holding a ticket channel shows nothing when it closes. A closed ticket whose channel still exists counts like any other channel. Channel Setup marks the row "Ticket" or "Lockdown": no drag, rename, delete or move, and `hold_fixed` drops those draft fields on the server. A new feature that makes temporary channels joins this list.

- **A push's layout version was read after the push.** `after_push` read the newest version once Discord was done, so an Owner's "Save again" became the base: the new channel was added twice and the "layout changed during the push" guard never fired. *Avoid:* read the base version at the start, inside the lock, and write with `HAVING max(version) = <that version>` (`layout::record`); a mismatch skips the write and says so. A push saves no version with a serious problem the saved layout did not have (`blocking` on both, compared by check id).

**One failed step stopped the rest of ChannelDelete.**
`tickets::on_channel_delete(..).await?` returned before the log channel and verification channel remakes. *Avoid:* in a handler with several independent steps, each step logs its error and the next one runs.

**A backup restore merged two same-named roles or channels.**
`backup::plan` matched image objects by id, else by name among live objects not yet used. With two "ticket" channels and one deleted, the deleted one name-matched the survivor before the survivor's id match ran, so nothing was recreated. *Avoid:* a name fallback skips every live object that some image object matches by id (`role_hits`, `chan_hits`). Test: `a_deleted_twin_is_recreated_not_merged_into_the_survivor`.

**Unlock deleted its snapshot first.** (#97, #111)
Unlock deleted the snapshot, then aborted on the first error (a deleted channel or role), leaving the server half locked with nothing to retry from. A restart in the middle did the same.
*Avoid:* lock state is a row `locking` → `locked` → `unlocking`, deleted only when the permissions are back. `lockdown::resume` finishes cut-off work at startup. A failed step is counted and the rest continue.

**Queue the undo before the action.** (#111)
A restart between a temp ban and its unban job made the ban permanent. Same for removing main roles on timeout and for timed locks.
*Avoid:* queue the undo job first, then act. A failed action leaves an undo that finds nothing and is dropped. Nuke reverts get a guard job that reruns after a crash.

**A revert applied same-second actions in any order.**
`revert_actor` read rows without `ORDER BY`, and `plan_revert` sorts by whole seconds, so two server renames in one second could be put back in the wrong order, leaving the attacker's first name. Root cause: a sort key coarser than the order of events, tie left to Postgres. Only name and icon reverts depend on order.
*Avoid:* read rows `ORDER BY id DESC` and keep the sort stable, so ties stay newest first. (The dashboard revert already orders by `created_at desc, id desc`.)

**A late rerun put back a name from a rename in between.**
O → X → Y → Z: the first run read two renames and restored O, then the row of Y → Z reached the rerun and set Y, the attacker's name. Compare-and-set on `from` alone was not enough: when the first run read before the last row was logged (live already Z), it skipped both rows and the rerun still set Y. Root cause: a "set to value" inverse applied per row, while each run sees a different subset and the right target is the oldest row of the incident. *Avoid:* the bot revert (`revert_actor`, `/revert`, the guard job) replaces all name and icon ops with one op each holding the value of the actor's oldest row since `since`, reverted or not (`settle_guild`), applies it only when the live value differs, and marks the newer rows reverted. The dashboard keeps compare-and-set on `from` (`still_set`, after a GET of the guild): staff pick rows already logged and it reverts them newest first; a row whose change is no longer live is listed under Skipped and left unreverted, so selecting the rest later still walks back to the original. Rows without `from` (written before) apply as before.

**Two locks at once saved a locked server as the snapshot.**
`lock_for` checked for an active lock, planned with several HTTP calls, then saved with `ON CONFLICT DO UPDATE`. Mention-spam lock calls it for every message past the limit, so a second call could plan after the first began applying and overwrite the first snapshot with locked permissions; unlock then restored the locked state.
*Avoid:* lock and unlock of a guild run one at a time (`lockdown::exclusive`). An in-process lock is enough only because the dashboard queues jobs and never writes `lock_snapshots`; a second writer process needs an advisory lock (see the Channel Setup entry below).

**An undo outlived what it undoes.**
Jobs queued to end something (a temp ban's `unban`, a long timeout's `timeout_renew`, a timed lock's `unlock`, panic's `panic_end` unlock) were not removed when it ended another way, so they hit what came later: a permanent ban after a temp ban was lifted by the old job, an untimeout or accepted appeal was timed out again by the renewal, a manual unlock and new lock was lifted by the first lock's timer, and panic's end lifted a role lock it did not make (its `lock` had returned false). Root cause: "queue the undo first" without the other half, "drop the undo when the thing is gone or replaced".
*Avoid:* every ban, unban, lift and new timeout drops the user's older pending job (`jobs::cancel_user`, `scheduler::forget_jobs` in the bot; the dashboard's appeal accept and revert call `cancel_user` in their own transaction), keeping only the job it just queued (`keep`). A lifted lock drops its own timer and clears panic's claim on the role lock (`lockdown::forget_timers`); `panic_end` carries `locked` (set by `panic::own_lock` only when panic's own lock call saved a snapshot, `lockdown::lock_saving`) and unlocks only then. Ban Sync does not skip a member who only has a local temp ban that ends before the copy would. The renewal is dropped only after the new timeout's PATCH worked (inside `timeout_until`, keeping its own job), so a failed new timeout keeps the old chain; a ban drops renewals too. A renewal that finds the member gone (10007) retries hourly until the timeout's end, since Discord keeps the timeout for a rejoin. Nuke reverts re-ban (`Inverse::Ban`) on purpose without dropping anything (it restores the earlier state).

**An undo kept less than it undid.**
An accepted appeal stored `{"op":"ban"}` and the visible timeout end, so a revert gave a temp ban back as permanent and a 90-day timeout as at most 28 days (the dashboard's PATCH past 28 days was refused). Root cause: the real end lived only in the job about to be dropped (`unban`, `timeout_renew`). *Avoid:* read the end from that job before dropping it and store it in the inverse (`Inverse::Ban.until`, `Inverse::Timeout.until` from `until_secs`); a revert queues the matching job again (bot `antinuke::run_inverse`, dashboard `ops::run_inverse`, 28-day chunk plus renewal) and skips a punishment that would have ended by now. A revert deletes the job it queued when the Discord call fails (both `run_inverse`; else a failed ban leaves the `unban` to lift a later permanent ban). The appeal's status, the action-log row with the end and the job drop are one transaction (else a crash between made the retry record a permanent ban as the undo, or answer "already handled" with no verdict DM and the log buttons left on), together with the Raider List allow of an accepted ban appeal (`raiders::allow` on the transaction; it once ran after the commit and was skipped on a resume). A retry by the same staff member that finds the appeal already accepted only sends the missed verdict DM (`appeals.verdict_sent_at`, migration 3922) and closes the log.

**A deleted object came back only from the newest backup.**
`backup::recreate` read only `latest`, so a backup that missed the object (taken mid-nuke, or after a delete not logged in time) made it unrecoverable, and the auto backup could run during a nuke or panic and save the damage as the newest image.
*Avoid:* recreate each id from the newest image that has it (`backup::newest_with`), and skip the auto backup while a nuke run or panic mode is on (`backup::under_attack`). A role from an older image may predate staff removing a permission, so it comes back without `DANGEROUS_PERMS` and the anti-nuke report says how many.

**Panic's backup load deleted what the bot made after the backup.**
A restore deletes every live role and channel missing from the image, protecting only the Quarantine role and log channels. Open ticket channels, a remade verification channel and Auto Setup's roles made after the last backup were deleted. *Avoid:* `sections::restore_keep` keeps a fixed list of config fields (`KEEP_FIELDS`: quarantine role, log channels, ticket category and log channel, anti-nuke's ticket categories, verification channel and roles), the permit roles and open tickets' channels; the bot's restore (panic, `/backup load`) and the dashboard's preview both use it. Do not keep every id in the config: most fields are Editor level, so an Editor could list a nuker's Administrator role in an automod list and panic would leave it. A keep or exempt list built from config names the fields it trusts.

**The bot's own rows were undone like anyone's.**
`/revert actor:@Brim` lifted every quarantine the bot made, gave a nuking bot its managed role's Administrator back, and unbanned automatic bans.
*Avoid:* the bot counts as an immune target (Owner or Extra Owner with `include_owners`), and a revert of the bot's rows never runs Unquarantine or RolePerms (`antinuke::bot_contain_op`; the dashboard's Revert Center skips both and lists the bot as immune too).

**An anti-nuke revert of a role removal gave back every removed role.**
`Inverse::AddRoles` gave even Administrator to a member quarantined since, and one deleted role stopped the rest. *Avoid:* the bot and the dashboard give back no role with dangerous permissions or a permit, and nothing to a quarantined member (left to staff, as the step text says). They try every role before they report the last error. The dashboard also reads the role lock snapshots, since a lock strips the bits that make a role dangerous.

**Dashboard fixes and Channel Setup pushes saved a locked state as the newest backup.**
The security-score fixes (`ops::backup`) and the Channel Setup push inserted a backup with no state check, while `backup::save`, backup create and delete refuse during a lock or panic mode. The newest backup is anti-nuke's restore source, so a later restore put the locked layout back. *Avoid:* these backups are skipped while `recovery::no_backup_now` is true (a lock, panic mode, or a nuke revert's guard job), and the log says "no backup" (`recovery::backup_named`).

- **The backup view sorted channels by category id, not position.** `(parent or own id, kind, position)` put categories (and loose channels) in snowflake order, so the tree did not match Discord, and matching children by category name broke when two categories share a name. *Avoid:* sort by `(voice, position, id)` and group children by `parent_id` (sent as a string: snowflakes lose precision as JS numbers).

- **A backup must not bring back a giveaway row.** Only the 90-day prune of ended giveaways deletes a row, and manual backups are kept for ever, so a load that restored rows would bring back a giveaway that already drew, and draw it again. *Avoid:* the rows stay in the database through a restore; only their channel and role ids move (`relink`). Panic and raids come back at start, but an open anti-nuke case is in memory only: the pause round also reads `nuke_incidents` before it resumes.

## Restoring names that must be unique

**A backup load could fail on category names that were swapped after the backup.** The load freed a clashing name by renaming it to "name (id)". That name could already exist (staff can type "B (5)"), and the unique check runs row by row, so the whole load stopped. *Avoid:* free every name first with a value the dashboard can never make (a leading space: names are trimmed), then set the names from the backup. Fixed in `crates/bot/src/msgbackup.rs`; test `swapped_category_names_still_load`.

- **A backup from before off gave the old Staff role its channel access back.** The role was no longer a level role, so nothing guarded it, and anyone with Manage Roles could hand that access out. *Avoid:* off, "Make it again" and a relink record the role and the time in `former_staff_roles`; every read of a backup image for a restore (`backup::latest`, `load`, `recreate`, the protected-channel copy) drops its overwrites from images taken before that time (`backup::Former`), while it is no level or permit role. Access given after off stays. A new read of backup images must go through `Former` too.
