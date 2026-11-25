# Pitfalls

Technical mistakes that already happened in Brim and were fixed. Each entry says what went wrong, why, and how to avoid it. The entries live in area files in `pitfalls/`. UI (Leptos) pitfalls are in [crates/dashboard/ui/PITFALLS.md](crates/dashboard/ui/PITFALLS.md).

## How to use

Read this index. Then read every area file that your change touches. When unsure, read the area. A new pitfall goes into the matching area file with its root cause and how to avoid it.

## Always read

- **When you fix a bug, search the whole codebase for the same mistake and for similar ones, and fix every place, not only the one that was reported.** Name in the entry where else it was found.
- A bot path is never weaker than its dashboard twin. An action never needs less than the permit perm of the effect it has.
- `brim_antinuke::DANGEROUS_PERMS` is the one list of dangerous Discord permissions. A feature that judges a permission dangerous reads it, never a list of its own.
- Never edit an old migration, not even a comment. Write a new one.
- Do not rely on the order of two gateway events.
- A message Brim posts never has a typed Unicode emoji. Use a constant from `crates/logging/src/emoji.rs`.

## Area files

| File | Read it when your change touches |
|---|---|
| [pitfalls/anti-nuke-guards.md](pitfalls/anti-nuke-guards.md) | limits and counting, guards on roles, bots and dangerous grants, trust and exemptions in anti-nuke |
| [pitfalls/anti-nuke-panic.md](pitfalls/anti-nuke-panic.md) | panic mode and lockdown, the mod command lock, steps that must not skip each other |
| [pitfalls/anti-nuke-restore.md](pitfalls/anti-nuke-restore.md) | reverting a nuke, backups, restoring roles, channels and overwrites, unique names on load |
| [pitfalls/automod-raids-filters.md](pitfalls/automod-raids-filters.md) | word, link and media filters, strikes, Discord AutoMod rules, heat and crowd checks, bot-role checks |
| [pitfalls/automod-raids-join.md](pitfalls/automod-raids-join.md) | Join Gate, verification, captcha, quarantine, raid detection, the Raider List |
| [pitfalls/moderation.md](pitfalls/moderation.md) | cases, warns, mod commands, appeals, notes, reports, undo, user info |
| [pitfalls/tickets.md](pitfalls/tickets.md) | the ticket system: commands, panels, type buttons, transcripts, DMs |
| [pitfalls/messages-emoji-safety.md](pitfalls/messages-emoji-safety.md) | log and message size limits, edits in place, who may send or delete, buttons on saved messages |
| [pitfalls/messages-emoji-literals.md](pitfalls/messages-emoji-literals.md) | texts and facts that code built as literals and must be editable defaults |
| [pitfalls/messages-emoji-builder.md](pitfalls/messages-emoji-builder.md) | Message Builder, default messages, previews, test send, variables, Brim emoji |
| [pitfalls/modules-other.md](pitfalls/modules-other.md) | giveaways, feeds, sticky messages, schedules, webhooks, module switches |
| [pitfalls/permits-access-levels.md](pitfalls/permits-access-levels.md) | permits, permit sync, Owner, Extra Owner, Trusted Admin levels, whitelists, the Staff role |
| [pitfalls/permits-access-sessions-mcp.md](pitfalls/permits-access-sessions-mcp.md) | dashboard login, OAuth, sessions, 2FA, rescue key, MCP tokens and MCP texts |
| [pitfalls/jobs-concurrency.md](pitfalls/jobs-concurrency.md) | the job scheduler, restarts, locks, caches, event order, background tasks, memory |
| [pitfalls/discord-api.md](pitfalls/discord-api.md) | rate limits, gateway events, permission bits, ids, HTTP timeouts, parsing of Discord data |
| [pitfalls/database.md](pitfalls/database.md) | SQL, migrations, schema, retention, privacy tables, backups of data |
| [pitfalls/dashboard-server-channels.md](pitfalls/dashboard-server-channels.md) | Channel Setup, Auto Setup, role and channel checks, security score, Overview Fix |
| [pitfalls/dashboard-server-config.md](pitfalls/dashboard-server-config.md) | config saves and range checks, request safety, data requests, the Update page, MCP replies |
| [pitfalls/dashboard-ui.md](pitfalls/dashboard-ui.md) | a few web-app notes that stand beside the web-app file `crates/dashboard/ui/PITFALLS.md` |
| [pitfalls/ci-release.md](pitfalls/ci-release.md) | Docker, CI, images, updater, deploy and release steps, dependencies |
| [pitfalls/tests.md](pitfalls/tests.md) | unit, e2e and live tests, fixtures, mocks, test servers |
| [crates/dashboard/ui/PITFALLS.md](crates/dashboard/ui/PITFALLS.md) | the web app (Leptos, CSS, browser behaviour) |
