# Pitfalls: Tests

Index and rules: [PITFALLS.md](../PITFALLS.md).

- **Every bug from the deep test of 2026-11-10** is listed with its cause in `tests/e2e/dashboard-deep-test-2026-11-10.md` of the archived old repository thomkr01/TommyBot. The UI ones (hover contrast, double page load, copy button, null number fields, `.row label` on the Import button) are in the UI file, sections 3 to 7.

- **`tests/e2e/deep.js`** checks every write route for seven access levels, every page as every level, the owner's number fields, toggles, save and cancel, the server switcher, 2FA, Auto Setup, toasts, theme, mobile and hover contrast.

**A new migration was missing from the test binary.**
`sqlx::migrate!` embeds `crates/core/migrations` at compile time, and core had no build script, so after adding a migration an incremental build kept the old list and the tests ran without it. *Avoid:* `crates/core/build.rs` reruns on a change in `migrations`.

**Two tests waited 30 seconds on a database that never answers.**
`owner_gets_a_token_others_do_not` (MCP) and `icon_revert_clears_a_new_icon_and_refuses_a_bad_hash` (dashboard) use `connect_lazy("postgres://x@127.0.0.1:1/x")`. A path that reads the database waits for the sqlx default acquire timeout (30 s) before it fails. *Avoid:* a pool that must never connect gets `PgPoolOptions::new().acquire_timeout(200 ms)` (all four such pools: dashboard `main.rs`, `ops.rs`, `sections.rs`, MCP `tests/oauth.rs`).

**A role setup that never committed passed every local run.**
`set_mcp_password` wrote the password outside its transaction, with no commit, so the role was rolled back. Locally the role still existed from earlier runs, because roles belong to the whole Postgres server, not to the dropped test database. CI (new server each run) failed. *Avoid:* test role setup on a fresh Postgres server (`docker run postgres:16`), not only a fresh database. Run every statement of a transaction through `&mut *tx` up to `commit`.

**A DB test overflowed the 2 MB test thread stack.**
`verify_lifts_only_a_join_quarantine_and_admin_roles_need_staff` awaited `modcmds::run` many times inline. In a debug build its async frames took about 1.7 MB, and a small growth in the called code tipped it over; this aborts the whole test binary. It is not a recursion, and not the live bot (spawned tasks keep futures on the heap). *Avoid:* a test that awaits a big command future (`modcmds::run`, `on_audit`) in a helper or loop boxes it: `Box::pin(..).await`. Check with `RUST_MIN_STACK=1048576` when a test grows.

- **The e2e sweep read the sidebar links before they rendered.** `dashboard.js` waited for more than 10 `#/` links of any kind; the header's links met that before the sidebar's module links existed, so `links[0]` was undefined. *Avoid:* wait for the exact element the next line reads (here a `#/<id>/module/` link), never a count of something broader or a fixed sleep.

- **Two DB tests shared a guild id and one purged the other's rows.** A new test in `cases.rs` took `5_100_000_000 + pid`, the base `db.rs` uses for a `purge_guild` test, so its cases vanished mid-test about one run in four. The same for a shared helper offset: in `core/src/giveaways.rs` two tests passed `db(7)` and one deleted the other's giveaway. *Avoid:* before a DB test picks a guild id base, search the crate for that number; a test that passes alone and fails in the full run has a neighbour writing its rows. List offsets in use (`grep -o 'db([0-9]*)'`) before you take one.

- **Dashboard tests shared a guild id base and failed with a duplicate key.** `g3_handler_tests::setup(base)` uses `base + pid`; bases (`9_884`, `9_893`, `9_894`) were used by two or three tests of the same process, so parallel tests wrote the same `guild_config` row (`guild_config_pkey` violation). *Avoid:* a new `setup(base)` takes a base no other call uses; `grep -rhoE 'setup\(\s*[0-9_]{10,}' crates/dashboard/src | sort | uniq -d` must print nothing.

- **Two DB tests used the same guild id.** Both took `9_300_000_000 + pid`, ran at once in one process and saw each other's config versions, so `history_keeps_newest_200_versions` failed now and then (`6_100_000_000` was also used twice in the bot crate). *Avoid:* give each DB test its own base (`grep -rn '_000_000_000 +' crates/` before picking one) and delete its rows at the end.

- **Two test files gave `T::new` the same numbers.** `lifecycle_tests` and `quarantine_tests` both used 70-81; the number picks the guild ids, so parallel tests shared a guild and one `cleanup` could delete the other's config. *Avoid:* `T::new(n)` numbers are unique across all test files (h2 tests use 150-199; the base is `pid * 10_000 + n * 10`, so `n` stays below 1000); grep `T::new(` before picking one.

- **A changed MCP tool description reached the AI model unseen.** AI clients take new tool text without asking the user, and nothing in CI compared it. *Avoid:* `crates/mcp/tests/definitions.rs` pins the instructions and every tool definition in `definitions.json`; a change fails CI until the reviewed file is updated with `UPDATE_DEFINITIONS=1`.

- **A mock let a timeout succeed while member edits failed.** The timeout arm came before the `fail_member_edit` arm, so a test of a failed timeout passed against a mock kinder than Discord. *Avoid:* a failure flag arm comes before every arm for the same route.

**The DB tests skipped green and ran on any database.**
Without `DATABASE_URL` about 500 tests passed without running; with a production URL they deleted rows and changed the MCP role's password. *Avoid:* every DB test gets its URL from `db::test_url`: it panics in CI when the URL is missing, and refuses a database whose name does not end in `_test` unless `BRIM_TEST_DB=1` (CI sets it). `tests/e2e/run.sh` has the same check.

**Tests that could not fail.**
A native AutoMod test never approved its rule, so every hit stopped before the whitelist it tested; two tests asserted only "nothing happened"; a core test checked a drifted copy of antinuke SQL; the OAuth test allowed a refresh to move `exp`; two tests shared `T::new(601)`; one ordered two handlers with a 100 ms sleep. *Avoid:* an "it did not act" assert has a control in the same test that does act; never test a copy of production SQL; assert the exact value of a security limit; each `T::new` number is unique; wait for the event (a mock call), not a fixed time.

**The dashboard access matrix ran only by hand, and a wrong refusal only made a note.**
`deep.js`'s matrix ran with `DEEP=1` only, and a refused user who got 400/404/500 was a NOTE, so a route that validates before it checks access passed. *Avoid:* `run.sh` always runs `DEEP_ONLY=matrix`; a refused user must get 401 or 403, and a 5xx for an allowed user fails.

- **Defaults are off on new servers.** Heat filters and anti-nuke start off, so a live scenario must turn on what it tests.

- **Bots are exempt from a lot.** Webhooks owned by Brim or a verified bot are exempt, bot-authored commands are ignored and bots cannot press buttons. Coordinated spam, confirm buttons, appeals, reports, Join Gate and DM tickets need a second human account.

- **The live test must not use the hosted bot's token or server.** The cloud env had the VPS bot's `DISCORD_TOKEN` and its only guild as `TEST_GUILD_ID`, so `live.sh` and the VPS bot both react to every attack. *Avoid:* a separate test bot (own Discord application) on a separate test server, or stop the VPS bot (`docker compose stop bot`) for the run.

- **The live script shares the bot's token** and its rate limits. Cache channel and role lookups and wait longer instead of polling hard.

- **Make scenarios clean up after themselves** so they can run again (the old nuke scenario kicked the attacker bot every run).

- **An e2e step waited for any row, not the one it made.** The notes check waited for "a note is listed", which old notes already met; the reload then swapped the card under the fill, so Save sent the old text. *Avoid:* wait for the exact value the step created.

- **An e2e step waited for a dropdown, not its options.** The Logging Cancel check waited for the channel select, which shows only "none" until the channel list loads. *Avoid:* wait for the option the step will pick (`option:nth-child(2)`).

- **A mock was kinder than Discord, and a test passed only because of it.** The Ban Sync mock's ban PUT replaced the reason, but Discord keeps the first ban's reason (and writes no audit entry) when a banned user is banned again, so `a_synced_ban_replaces_a_shorter_local_temp_ban` passed. *Avoid:* when a live run shows Discord behaving differently, fix the mock first (the mock PUT, in `test_support.rs`, keeps an existing reason) and re-run the tests that touch it. The code fix is under "A second ban keeps the first ban's reason" (section 2).

- **A body-less DELETE was refused and a soft call hid it.** `live.py`'s `call` sends `Content-Type: application/json` on every request; `DELETE /guilds/{g}/bans/{u}` with no body got 400 (50109), and `soft=True` turned that into None, so cleanup left the user banned. *Avoid:* `call` sends `{}` on a DELETE without a body; a soft call is for an expected 404, not every 4xx.

- **`tests/e2e/run.sh` needs root Postgres, trunk and Playwright**; the gateway is not covered by the Discord mock.

- **A global cleanup in one test deleted another test's rows halfway through its setup.** `raiders::expire` deletes every old rejected or pending entry in the database. Test 55 created old entries first and their evidence after, so test 54's parallel `expire` could delete an entry before its evidence existed (count 2 instead of 1). *Avoid:* in a test that calls a job working on the whole table, create the rows such a parallel run could act on last, so every state it can see is complete.

- **Two tests ran the same global sweep at once and deadlocked.** `persondata::sweep_kept` finishes every done delete request in one transaction (cases, then reports, appeals and tickets, then cases again). The two datareq tests (978, 979) each called it and each held rows the other wanted (40P01). Production runs one sweep an hour, so only tests collided. *Avoid:* tests that call a job working on the whole table and holding locks across statements take turns on one static `tokio::sync::Mutex` (`datareq::tests::SWEEP`).

- **A test row's level ignored what the call changes.** `deep.js` expected a Trusted Admin to roll back to History v1. v1 comes from `dashboard.js`, before `levels.sql` adds the TA and EO, so the rollback also changes both lists, and `api::rollback` needs an owner for the TA list and the Discord owner for Extra Owners (same rule as `put_permits`). The owner's rollback also wiped the TA, EO and permits for later phases. *Avoid:* a matrix row's level is the strictest rule the call touches with the fixture's data; a row that changes fixture data undoes it (the rollback row runs `levels.sql` again).

- **Two tests in one binary used the same test server id.** Test ids are `<base> + std::process::id`, so two tests with the same base get the same id, and in a parallel run both insert `guild_config` version 1 (`duplicate key`, a random red check). Nine bases were used twice (dashboard `api.rs` 9_884_000_000, `mcp.rs`/`api.rs`, `ops.rs`/`api.rs`, `embeds.rs`/`update.rs`, `mcp.rs`/`channels.rs`; bot `lockdown.rs`/`tickets`, `antinuke.rs`/`slowmode.rs`, `antinuke.rs`/`joingate.rs`; core `sections.rs`/`tests/db.rs`). *Avoid:* grep a new base first (`grep -rn "<base>" crates`) and use it once per crate.

- **A handler that awaited a whole mod command overflowed the test stack.** `undo::handle` awaited `modcmds::run` inline; that future is large, a test thread has 2 MB, and the test died with "stack overflow" instead of a failed assert. The mock also answered 404 to every interaction reply, so a handler that returns its error stopped before it acted. *Avoid:* a handler in the interaction chain boxes a large command future (`Box::pin(modcmds::run(..))`), which also keeps the gateway event future small. The mock in `test_support` takes interaction answers, follow-ups and edits.

- **An e2e step that fills a form right after `goto` can lose the fill.** The page rebuilds its inputs when its data arrives, so the user ID was empty when Add note was pressed and `POST /notes` answered 400 ("user ID must be a number"); it failed on some PRs only. *Avoid:* wait for `networkidle` after `goto` before filling, as for the dropdown pick above.

- **A test with many calls of one large async function overflowed the stack.** `modcmds::run` holds every command's state in one future; two new arms made its debug frame large enough that an old test overflowed, and a new test with 11 call sites held 11 copies. *Avoid:* a new arm of a large `match` in an async fn goes into its own async fn called with `Box::pin`; a test that calls such a function many times calls it through one closure that returns `Box::pin(async move { .. })`.

- **The browser test ran into the dashboard's own request limit.** A session gets 40 API requests at once and 20 more each second (`auth.rs` BURST, RATE). `tests/e2e/dashboard.js` opens pages back to back; the case page steps added about 25 requests in under a second (each page load and change does `/config`, `/access`, the case and a member lookup), so on a fast run the next page (Logging) got 429 for `/config` and `/schema`, showed no channel dropdown and timed out. The page itself was not over the limit for a person, but it asked again for the name of a member who left, after every change. *Avoid:* the test counts its own API requests and `page.goto` waits exactly until the limit has room for a page load (no fixed sleep; the limit stays); new steps need nothing extra. A page that reloads data after a change does not repeat lookups that gave no answer (`asked` in `CasePage`).

## A load test of the plan runner shared the database with other runners

**`many_due_plans_all_start_once` failed with 0 or a few started plans.** Two causes outside the plan code. (1) The test asked `due` for the oldest 200 plans of all servers and then kept its own; plans left by another test run filled the 200, so a lane saw none and stopped. (2) A dashboard server on the same database took the due plans with its own runner and moved them a minute on ("Brim could not run this plan"), because it cannot reach Discord. *Avoid:* a test of a runner picks its plans in the query, by its own server range, never by filtering after a limit. Never run a dashboard server on the database the tests use: give the server its own database (`createdb`), and start it only with that one.

## One dropped request ended the whole access matrix and named nothing
In CI the browser sweep failed with `FAIL matrix: crashed: page.evaluate: TypeError: Failed to fetch`. Page tests passed, the server log held no error, and the same code passed on the next commit. One thrown error ended the whole matrix loop and only the crash text was kept, so the dropped call among hundreds could not be named.
*Avoid:* a test loop that makes many calls catches the error of each call and fails with the call's own name (user, method, path), then goes on. A crash that names nothing is a finding nobody can follow up. The matrix in `tests/e2e/deep.js` does this. If the same route drops again, it is a real fault in that route, not the runner.

- **New migration not in the e2e build.** `sqlx::migrate!` embeds the folder at compile time and core has no build script, so an incremental build after adding a migration ran without it ("relation does not exist"). *Avoid:* touch `crates/core/src/db.rs` after adding a migration locally; CI builds clean.

- **The restore test left a plain copy of the database behind, and leaked rows into its log.** `restore-test.sh` removed its Postgres container with `docker rm -f`, but the image's data folder is an anonymous volume, so every run left the restored database (2FA secrets, session tokens, deleted data) unencrypted under `/var/lib/docker/volumes`. Its psql errors went to the log with DETAIL and CONTEXT lines, which quote rows. A typo in `BACKUP_AGE_RECIPIENT` made `age` fail inside the dump pipe, so no copy at all was made. *Avoid:* a throwaway container that holds data is removed with `docker rm -fv`; a log gets only psql's ERROR line; a key is checked (`age -r KEY < /dev/null`) before the dump, and a bad key falls back to the plain copy with a message.
