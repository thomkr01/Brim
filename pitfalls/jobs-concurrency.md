# Pitfalls: Jobs, restarts and races

Index and rules: [PITFALLS.md](../PITFALLS.md).

**Auto backups reached only the first 200 servers.**
`current_user_guilds` returns at most 200 rows, and a parse error read as "no servers". *Avoid:* `backup::all_guilds` pages with `after(last id)` until a page has fewer than 200 rows, and a failed page is logged. Any other job over every server must page the same way.

**Write state after the Discord call, or roll it back.**
The quarantine row was written before the role change. When the change failed, the bot believed a member was quarantined and refused a correct save later.
*Avoid:* write the row after the call succeeds, or delete the row you created when the call fails.

**Never mark a row done before the Discord call.**
The first Ban Sync copy set its row to `banned` before the ban; a crash in between left "banned" with no ban and the retry stopped. The claim that makes a step run once was used as the record that it happened.
*Avoid:* keep the row `pending` while the call runs. On retry, recognise your own earlier result in Discord (a ban whose reason starts with `Ban Sync from`), then move the row on with `UPDATE … WHERE state = 'pending' RETURNING`; write the case only if the row has none. Mark a row skipped only for a refusal, never while the action exists. The Ban here button does the same and gives the row back to `alerted` when the ban does not happen. Same pattern: `reports::report` deletes its row when the log message fails, `joingate::punish` records after the action, lockdown `locking` → `locked`, quarantine row written after the role change.

**A REST call on the join path had no time limit.**
`invites::used_invite` ran before raid detection on every join and waited out Discord's rate limit, so in a raid each joiner's detection queued behind the invites bucket.
*Avoid:* a lookup on a protection path gets `tokio::time::timeout` (1.5 s); a timeout means "unknown". Raid detection runs before every REST call of the join (invite lookup, bot adder, verification quarantine); the invite is added to the raid record and `raid_log` afterwards (`note_invite`). The join path makes no invite call: the per-guild tracker (`invites.rs`) fetches in its own task and Join Gate reads its answer in a spawned task. The answer can come after the raid triggered (and got a new code), so `note_invite` always notes the invite in the detector and updates rows under the join-time and the current code. A raider the raid removed gets no verification quarantine and no second Join Gate punishment.

**A one-time pass at bot start must not repeat, and must not stop for good on a failed read.**
The pass that shows staff roles separately runs at GuildCreate. On every start it would undo an owner who turned the grouping off; marked done before its reads, one failed read skips the server forever.
*Avoid:* read what the pass needs first, then claim the server's row (`INSERT ... ON CONFLICT DO NOTHING RETURNING`), then edit. A server with no staff roles yet is not claimed; the row is released when every edit was refused, so the next start retries.

- **One server's big job delayed another server's panic start.** `panic_start` and `nuke_revert` shared the one-job-at-a-time heavy worker with `backup_load` and `raider_scan`; a scan of a 30,000-member server held another server's panic start 3.25 s (minutes for bigger ones). *Avoid:* kinds that must start at once go in `jobs::URGENT`, with their own worker (`scheduler::run_urgent`, each job in its own task); any kind that can run for minutes goes in `HEAVY` (test: `a_big_job_in_one_server_does_not_delay_another_servers_panic_start`). A panic could then start in the middle of a backup load, whose later steps wrote the locked permissions back: a full load checks for a lock, panic or running nuke revert before every step (a revert and a load would recreate the same channels), and a stopped load writes no server settings or role order (`backup::halt`, test: `a_full_load_stops_when_panic_or_a_nuke_revert_starts`). Heat panic could do the same.

- **Two dashboard-queued job kinds were left out of the run-time re-check.** #312 re-checked the asker of lock, unlock, lock update, panic start and backup jobs, but not the early `panic_end` (moved to now with `ended_by`) or the Raider List decisions (`raider_check`, `raider_lift` at server 0, no asker). Root cause: the list came from jobs that carried `by`, not from every place the dashboard queues or moves a job. *Avoid:* when a check covers "jobs a person queues", grep the dashboard (and MCP) for every `jobs::enqueue`, `insert into scheduled_jobs` and `update scheduled_jobs`, and decide per kind. An early `panic_end` is re-checked as panic start (Owner, Extra Owner, Trusted Admin); a refused one must not delete panic's only end, so `panic::keep_timed_end` puts the job back at `ends_at` (where the dashboard moved it from) without the asker. `panic::end_payload` keeps `ends_at` with `ended_by`, and the put-back adds no second end on a retry. A Raider List decision carries `by` and fans out only while that person is the bot owner; a dropped one DMs the bot owner. Left as is: `appeal_accept` (`decide` checks permits at run time), the timed `unban`/`timeout_renew` of a mod action, the member's own `verify_*` jobs, `permit_sync`, `verify_sweep`, `note_log`, and `raider_notify` DMs (a security alert must never be dropped).

**Every event posted its logs before raid detection, anti-nuke and automod ran.**
`events::run` awaited each log post, which waits on the log channel's rate limit (about 5 per 5 s), so the 300th joiner's detection ran minutes late; `Logger::handle` also returns the due held logs of every server, so one server's delete flood slowed all others. A failed config read there (`?`) ended the whole event. *Avoid:* the event path never awaits a log post: `events::run` sends logs in a spawned task with the event's span and an `InFlight` drop guard (a panic must not keep shutdown waiting); a failed config read skips only the logs. One task per event, not one sender for all servers.

**A guild_info refill put back what an invalidation removed.**
A cache miss made four REST reads and inserted with no check, so an owner transfer or role edit that invalidated during the reads was lost for 5 minutes. *Avoid:* generation rule as `lockdown::LOCKS`, per guild: invalidations bump that guild's entry in `hierarchy::GENERATION` under the `guilds` lock, and a fill is kept only when it did not change. A global counter made one server's events discard the fills of all others.

**Background sweeps stuck and kept state for good.**
The message webhook sweep took 50 rows with no order, kept every row Discord refused (the same 50 came back each hour), and ran inside the scheduler tick. `MENTION_HITS` kept one entry per member who ever hit a scoped rule. *Avoid:* a sweep orders by age, moves a refused row forward (4xx; an outage is retried), and runs in its own task; a map keyed by member drops entries past the longest window (an hour for scopes) once it grows.

**Two presses of one button both acted.**
Two moderators pressing Ban on one report both banned and a later press overwrote `handled_by`; two quick presses of a ticket DM's Send relayed twice. *Avoid:* claim before acting: an in-memory claim per report (`reports::Acting`, freed on drop) and `WHERE status='open'` on the close; the ticket prompt's delete is the claim (10008 means a press already took it). Build a drop guard only for a won claim (`then(|| ..)`, never `then_some(..)`): an eager guard for a lost claim drops at once, under the lock it re-takes (a deadlock), or removes the winner's entry. `t!enable`/`t!disable` flipped the state, so a repeated `disable` turned tickets on: a command named after a state sets that state.

**A panicking event handler left the in-flight count up.**
`events.rs` decremented `IN_FLIGHT` at the end of the handler, so a panic skipped it and shutdown waited forever. *Avoid:* a drop guard decrements it on every exit.

**The message cache counted messages, not bytes.**
`MessageCache` kept 50k messages per server, so 4,000-character messages held about 200 MB per server. *Avoid:* a per-server byte budget (`MAX_BYTES`, 16 MB) also evicts the oldest; the byte count is kept on insert, edit, remove and prune, and a test checks it against the map.

**Two in-memory maps grew until restart.**
Unanswered captchas (`app.captchas`) and emptied anti-nuke windows (`AntiNuke::windows`) were never removed. *Avoid:* a map keyed by member or actor drops stale entries once it passes a size cap, as `cool`, `remember_flag`, `remember_code` and `on_actions_at` do. `App::nuke_heat` (per actor) had the same gap and now drops decayed entries.

**Events run concurrently.** (#100)
`main.rs` spawns a task per event, so messages in flight after a trigger each added a strike; one burst could hit the strike cap and send several DMs, cases and logs.
*Avoid:* claim the punishment (`HeatState::claim`, one per 10 s). Anything that must happen once needs an atomic claim: `DELETE … RETURNING`, `WHERE status='open'`, `SKIP LOCKED`.

**A claim freed only on the normal return stayed taken after a panic.**
`voice_nuke` set `running = true` and cleared it only when the fetch loop returned, so a panic in a fetch left the guild claimed for good. `nuke_runs` had the same hole: a panicking run kept the actor claimed, so later events only logged rows and the guard job skipped the "running" run. A guard alone kept a second bug: `rerun` frees the claim when nothing new came in, then the run awaits `jobs::finish`, and its final remove could delete a claim a newer run took meanwhile (a third run next to the second). *Avoid:* free an in-memory claim in a `Drop` guard (`voice_nuke::Claim`, `antinuke::RunClaim`) taken right after the claim succeeds, and when anything else can free the claim first, let the guard remove only its own (per-claim token, `antinuke::release`).

**Replacing a job dropped a report that was due but not yet claimed.**
A raid trigger called `jobs::replace`, which deletes every pending job of that kind in the guild, so a raid starting right after the last one deleted its report job before the scheduler took it. Join Raid's `raid_end` had the same pattern. An extended voice raid's end was also only in memory, so `restore` after a restart ended it early. *Avoid:* `replace` only where one job per guild is the meaning (`panic_end`, which cannot start again until its end job ran). A job per event uses `enqueue`; a job that re-queues itself enqueues too (finished after it returns). Write a moved deadline into its job (`voice_raid::extend`), and read `jobs::pending` in `run_at` order so a restore keeps the latest per guild.

**A run marked rows it never read.**
`revert_actor` read the actor's rows, worked for seconds, then marked every row since `since` as reverted; what the nuker did meanwhile was marked too and never undone, not even by `/revert`.
*Avoid:* mark only the ids you read (`WHERE id = ANY($1)`). Never close work with a range condition when rows can arrive while it runs. Marking every id read is still too much: a failed inverse, an op the restore filters dropped and a deleted channel the backup did not recreate would be marked reverted and never retried. Mark a row only when its op ran (or its target was already gone): `antinuke::undone` for logged inverses, and `backup::recreate` returns the ids that exist again. The dashboard revert already marks per success.

**A gateway event and its audit entry run in their own tasks.**
The ticket-category exemption read the category that the `ChannelCreate` task stores. When the audit task ran first, the lookup missed and a ticket bot's channels counted towards its limit.
*Avoid:* do not rely on the order of two events. `note_parent` stores every channel (also "no category"), so a miss means "not seen yet", and `parent_of` waits up to 0.5 s for it.

**Join-time steps race each other.**
A `MemberUpdate` (verification, another bot, the bot's own role adds) can run before or alongside the `MemberAdd` task, and a late update can come from the membership before. Saving roles "per user id" would let it overwrite the saved roles before the rejoin reads them.
*Avoid:* key per-member state by membership: `member_roles` rows carry `joined_at`, an update writes only when its `joined_at` matches the row, and the rejoin claims the older row in one statement (`sticky::claim`), so a replayed `MemberAdd` gets nothing.

**Long work under a global lock.** (#100)
Similarity compared whole 4000-character messages (quadratic) while holding the global heat lock.
*Avoid:* bound the input (first 500 characters) and keep work under shared locks short.
The same holds for a claim: `voice_nuke` held the guild's fetch claim while counting, and counting can start a nuke run of tens of seconds, so no other voice update of the guild was fetched meanwhile. The claim covers only the fetch (`run` takes `fetch` and `apply`); the one-run-per-actor claim of the nuke run is separate and unchanged.

**Listeners must survive a reconnect.** (#100)
The config listener died after one DB hiccup, so dashboard changes never applied until a bot restart.
*Avoid:* long-lived listeners restart in a loop and reload their caches after reconnecting.

**A claim row without its job blocked the channel for good.**
An `auto_slowmode` row is claimed with `ON CONFLICT DO NOTHING`. When its `slowmode_off` job gave up after its retries (Discord down, a missing permission retried to the last attempt), the row stayed and every later start found "already owned". Also: a failed start deleted row and job even when the PATCH might have landed, the not-calm re-queue made a job for a row staff had removed, and a 403 was retried until the job gave up.
*Avoid:* a claim takes over a row with no pending job (`DELETE … WHERE NOT EXISTS (job)` in the claim's transaction, so a parallel claim conflicts instead of doubling it). Treat 50001/50013 as final (`http_err::forbidden_any`): end the claim and log it. Re-queue only while the row exists, under `FOR UPDATE`. Scope a job's rows to its own guild (`AND guild_id = $2`). A start that stops early because the channel already shows the auto value also gives a stuck row a new off job (`requeue_stuck`), since the claim's takeover never runs then.

**State built from gateway events starts empty after a restart.**
The logger tells voice joins from moves by the channel it last saw each member in, and that map started empty, so after every restart or new gateway session the first move of each member already in voice was a "join". It was only a wrong log line (marked `shortcut:`) until the voice raid counted joins from it: a restart during a busy event could start a false raid. Root cause: the old value came only from earlier events, which Discord does not replay.
*Avoid:* seed such state from `GuildCreate`, which carries the whole snapshot (`welcome::on_guild_create` seeds `Logger::seed_voice` from `voice_states`, replacing what the guild had), and read the old value under the same lock right before the event overwrites it (`events.rs`). Both must happen before the task's first await: a seed after `app.cfg.await` and a DB write wipes voice updates handled meanwhile, and an old channel read after it lets a member's second update take the lock first so both read the same old channel. `events::run` seeds a `GuildCreate` and records a voice update (`Logger::voice_update`) first, and logs it afterwards with `handle_voice`. `thread_parents` is filled from `ThreadListSync` and thread events, the ticket-category lookup (`parent_of`) only pairs an event with its own audit entry, and Join Raid and heat start empty on purpose (a restart counts from zero, it never counts twice).

**A restart reset every limit window.**
Anti-nuke's per-minute/hour windows, the permit command limits and the heat panic list lived only in memory, and a restart can be forced: a nuker at 4/5 bans who crashed the bot started again at 0. Root cause: the counted actions were in `action_log`, but nothing read them back.
*Avoid:* a limit that protects against an attacker is rebuilt at startup before the gateway (`antinuke::seed_windows`): anti-nuke from `action_log` rows of the last hour (`counted` holds how many actions a row adds: a merged voice entry its growth, a second inverse row 0), command limits from `command_uses`, heat panic from `heat_panic`. Both new tables are pruned hourly in `welcome::purge_left`. Personal heat (above) and Join Raid stay in memory on purpose.

**A resumable run read its intent from the state it was changing.**
Channel Setup Push decided "this channel follows its category" from live Discord. Once the category's overwrites were pushed and a child's step failed, the child no longer looked synced, read as an exception on the next push, and stayed public.
*Avoid:* a run resumed by recomputing a diff stores every decision derived from the old state before its first step (Push writes `reset` into the draft for each follower), and stops at the first failed step instead of running the rest (deletes included) against a half-changed server.

**A per-process flag does not stop a second writer.**
Push guarded itself with a static `HashSet` of guilds, so a second dashboard process could push at once, and a draft PUT or DELETE during a push was overwritten by the push's write-back. A stale PUT after a push brought back `new-*` keys already created; the next push created them twice.
*Avoid:* hold a Postgres advisory lock for the whole run (`pg_try_advisory_xact_lock` in a transaction the run owns, so it is freed when the task ends or the process dies) and refuse other writers with 409 while held. Record keys a run has used up (`next`) and refuse them when they come back. Run the steps in `tokio::spawn` and await the handle, so a dropped request does not stop a run half way; log what will happen before the first step and the result after; record a failed save instead of returning with `?`.

**No row lock across Discord calls.**
The Raider List lift held `SELECT … FOR UPDATE` on its action row while it unbanned, so a slow Discord held the lock for the HTTP timeout and a second run waited. The lock was only there to make the case and notice happen once, which a claim does without waiting.
*Avoid:* read without a lock, make the Discord call (it must be safe to repeat: the unban checks the ban is still the list's), then claim with `DELETE … ` and act on `rows_affected == 1`; only the claimer writes the case and log. A lift that gives up after its last try posts "Raider List removal failed" to the mod log (`raiders::lift_gave_up`). Left as is: `appeals` decide (a person's click) and `backup::track_created` (needs the lock; short GET).

**A button read its state, acted, then marked it done.**
The Raider List alert's Ban/Kick/Quarantine read `action = 'alert'`, ran the punishment and only then set the row. Two staff pressing at once both passed: two cases, DMs and mod logs, or a ban and a kick.
*Avoid:* claim first with `UPDATE ... WHERE action = 'alert'` and act only on `rows_affected == 1`; put the row back when nothing was done. Left as is: Ban Sync's Ban already claims first; appeals decide claims after the unban (safe to repeat; case and log come after the claim).

**"It is gone, so an earlier try of mine removed it" needs proof.**
The lift took a missing ban to mean an earlier try had unbanned, and wrote an unban case in the bot's name even when staff had unbanned by a path the list never saw.
*Avoid:* record the intent (`raider_actions.unbanning = true`) just before the Discord call; only a retry that finds that mark writes the missed case.

**An idle timer fed only by gateway events counts the bot's downtime as quiet.**
`last_activity_at` is set when the bot sees a message; Discord does not replay messages sent while the bot was offline. The first sweep after a restart would close a ticket the member wrote in an hour ago.
*Avoid:* before an idle action, read the channel's `last_message_id` from Discord (a snowflake holds its time) and skip the action when it is newer than the cutoff.

**A retried job counted its own attempt again.**
The verification deadline counted a time out before the kick or ban ran. When the removal failed, the job ran again and counted again, so one real time out could reach the 3 that bans, and each failed ban left an unban job behind. A wave of joiners that never verify could also fill the invite cap with retry invites.
*Avoid:* count an attempt only after the action it counts has worked; remove a job queued for an action that then fails; cap anything a joiner can make the bot create (retry invites: 20 per server per hour, none during a raid or lock, none with verification off). Keep such counts in the database (`verify_timeouts`), not in memory: the bot restarts several times a day. A delayed job about a member must check the member is still in the same stay: the verification deadline skips a member who joined less than the time limit ago (a quick rejoin). A timed unban must lift only its own ban: the verification time-out unban checks the ban reason first, so a ban staff put on top stays.

**A done marker written before the action.**
The NSFW scan row was saved as a hit before automod acted. A failed action left the row, and the unique key then made every later pass (preview update, edit, sweep) skip the message for good. *Avoid:* a marker that makes later passes skip work stands only once the work succeeded. Mark the errors that come after the punishment landed (`automod::Punished`; a saved strike counts as landed) and free it on every other error, so a brief DB error before the punishment is retried, while a failed case, log, lockdown or strike timeout after it never punishes twice. Free the whole unit a pass checks: the sweep skips a message with any row, so every row of the message goes, and its attachment ids are forgotten.

**The message cache grew with the number of servers.**
No total cap, no count of map and queue memory, and text kept for servers with no message log (300 servers: about 5 GB). *Avoid:* an in-memory store per server also has a total cap (`cache::MAX_TOTAL_BYTES`, 512 MB; over it the biggest server loses its oldest first), counts per-entry overhead (`OVERHEAD`), and holds data only where a feature reads it (`Logger::handle` caches only while a message edit or delete log is set; an edit while it is off drops the cached copy, which would be stale later).

**A full process-wide map was cleared whole.**
Heat, ping windows, reaction-spam windows, scam-hit counts, live captchas, the spammer-flag cache, trigger cooldowns, anti-nuke heat, fight and take-back counts, and the "already acted on this message" sets were cleared whole past their cap, so a flood in one server wiped every other server's detection state (heat restarted, live captchas went invalid, a message could be punished twice). *Avoid:* past the cap, after dropping idle entries, `caps::drop_biggest` drops only the server (or rule) holding the most; maps keyed by message id drop their older half (`caps::newer_half`; by an id that is a snowflake, never a 0 placeholder). A restart counts the size this hour's log file already has. Caches that only cost a lookup (levels, thread parents) may still clear whole.

**A per-member path must hold for 100,000 members.**
A giveaway can get 100,000 entries, and a button press can come from 10,000 members in a minute. *Avoid:* a press is one small indexed write (unique on giveaway + user, so a double press is a no-op) and a private answer, never a message edit; the shown count is refreshed by a job at most once a minute (`giveaways::queue_refresh` also skips the database for 60 s per server). Never load all rows to pick from them: the database draws (`ORDER BY -ln(1 - random()) / weight LIMIT n`, a weighted sample without repeats), and only the picked candidates are checked with Discord, with a cap on the checks. A change of rules does not re-check every entry; the draw re-checks the winners. Every query on a per-press path needs an index on its `WHERE` (migration 4061 adds `raid_log (guild_id, user_id)`). The load test `a_giveaway_with_100000_entries` measures it.

**A saved draw was never announced after a crash.**
The end job saved the winners, then posted them and queued the DMs. A restart between the two left winners nobody was told about, and the retry saw the draw as done. *Avoid:* the save of a result queues its follow-up jobs (announcement, one DM per winner) in the same transaction (`giveaways::save_draw`). A DM job checks that the member is still a winner, and DM jobs run spawned (`scheduler::SPAWNED`), so 100 DMs do not hold back unbans. A cancel drops the end job only when it changed the row, so it never takes the retry from a running draw.

**A schedule's claim moved `next_at` before the send, so an error after it lost a one-time schedule silently.**
*Avoid:* an unread saver level after the claim sets the rule's `issue` with the reason. Nothing is paused.

- **A "sent once" set shared the size-cap clear with the counts.** The owner DM count map was cleared at 10 000 entries, and with it the record of who was DMed, so a busy server could DM the owner again. The key also held the role set, so one actor giving other roles counted again. *Avoid:* keep "already done" apart from a cache that gets cleared, and key it by what the message is about (guild, actor).

- **A batched counter lost counts.** Entries were marked written before the write, and the size cap cleared counts that were never stored. *Avoid:* mark a batch as written only after the write succeeds and only if the value did not change meanwhile; on overflow drop only stored entries, and read a missing count back from the database before counting on.

- **A move of a job that the scheduler holds is lost.** The scheduler leases a job, runs it, then deletes it by id. An update of `run_at` on the leased row (a shift of the end time, the end of a pause) changes a row that is deleted a moment later, so the giveaway never ended. *Avoid:* a move updates only unleased rows (`locked_until IS NULL OR locked_until < now()`) and queues a new job when it moved none. The job checks under the row lock that it is still due (`giveaways::end_due`): when the end moved later or the giveaway is paused, it waits and makes sure one other pending job exists.

- **A draw or an entry during an attack.** `paused_at` existed but nothing set it, so a raid, lockdown, panic mode or nuke did not stop entries or the draw. *Avoid:* the Enter press and the end job read the live state (`giveaways::attacked`: raid detector, panic run, open anti-nuke case, server locks; a lock of one channel does not count) and set the pause at once; a round every 15 s pauses and resumes. A restored or remade role or channel moves into running giveaways (`giveaways::relink`), so a remade blocked role still keeps its members out.

- **A winner could claim while the place was being drawn again.** The claim job read the expired place, then spent seconds on member checks; a Claim press in that time still saw an open deadline, said "You claimed", and the redraw then removed the place. *Avoid:* the job closes the claim in the same statement that reads it (`claim_due`, `claims_due`: `deadline = LEAST(deadline, now())`). Places that run out together are drawn again in one redraw (one post, not one per place). A new place number skips every place in use, also those handed out in the same save.

- **A repeat that starts a giveaway must claim its run and make the giveaway in one transaction.** The message schedule stores the next run first and sends after, so a crash between the two loses a run. *Avoid:* for a plan, the plan row lock (`FOR UPDATE SKIP LOCKED`), the new giveaway (`giveaways::create_in`, under the per-server lock that counts the limit) and the next run are one transaction: two runners, a restart or a late tick start it once, and a crash starts it never or once, not twice. A run more than an hour late is skipped with the reason on the plan (the last run of a plan has 6 hours); a plan over the limit of running giveaways skips the run, it does not queue. "Start after the last giveaway ends" keeps `held` and looks again every minute: one indexed read per waiting plan.

- **A runner that looks at few plans per pass, and marks a plan late by the clock, loses runs under load.** A pass looked at 8 plans, one at a time, and a run was skipped when over an hour late, so many plans due in the same minute were skipped only because the runner was busy. *Avoid:* a pass takes 200 plans in 8 lanes, and a run is late only by the time Brim was down (runner start minus run time). 10,000 due plans start once in about 8 s.

- **A plan must not start a giveaway in a server under attack.** The attack state is in the bot's memory, the plan runner is in the dashboard. *Avoid:* the bot's pause round (every 15 s) moves the plans due soon of an attacked server a minute on, with the reason on the plan; it starts once when the attack is over. Tests that call the pause round side by side use `pause_round`, not `sync_pauses`, whose one-round guard makes a second call do nothing.

**A failed look-up replaced a good cached value with an empty one.** The sponsor picture is cached in memory for hours. *Avoid:* never store the result of a failed fetch over a good cache entry. On a Discord error (5xx, 429, timeout) keep the old picture and ask again in a few minutes. Only a first look-up that fails stays empty.
