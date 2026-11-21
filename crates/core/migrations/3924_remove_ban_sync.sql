-- Ban Sync is removed (owner decision 2026-11-13 22:50; Raider List stays). Copies already made stay as
-- normal bans. Only its queued jobs go now: ban_sync_links, the mcp_ban_sync view and
-- owner_handovers.ban_sync_was_on stay one release, because the previous image reads them and an
-- image rollback must keep the owner-change protection working (owner decision 23:29). Drop them in a
-- later migration once that image is gone.
DELETE FROM scheduled_jobs WHERE kind LIKE 'ban\_sync\_%';
