-- Access levels for the mock users in mock_discord.py (run after the dashboard has migrated the DB).
delete from extra_owners where guild_id = 100000000000000001;
delete from trusted_admins where guild_id = 100000000000000001;
delete from permits where guild_id = 100000000000000001 and name like 'e2e %';
-- No unique key on evidence: deep.js runs this file again after a rollback.
delete from raider_evidence where user_id in (200000000000000042, 200000000000000043);
insert into extra_owners values (100000000000000001, 200000000000000002);
insert into trusted_admins values (100000000000000001, 200000000000000003);
-- Both mock servers belong to the owner x.
insert into guild_owners (guild_id, owner_id) values (100000000000000001, 200000000000000001),
  (100000000000000002, 200000000000000001) on conflict (guild_id) do update set owner_id = excluded.owner_id;
-- Raider List: one listed user with an open appeal and a match here, one pending report in the queue.
insert into raiders (user_id, status, category, verified, public_note) values
  (200000000000000042, 'confirmed', 'raid', false, 'e2e raider'),
  (200000000000000043, 'pending', 'spam', false, '') on conflict (user_id) do nothing;
insert into raider_evidence (user_id, source_guild, source_owner, kind, ref, detail, reporter_id) values
  (200000000000000042, 100000000000000002, 200000000000000001, 'auto_raid', '1', '{"joins": 12}', null),
  (200000000000000043, 100000000000000001, 200000000000000001, 'report', '1', '{"case_no": 1, "case_kind": "ban"}', 200000000000000001)
  on conflict do nothing;
insert into raider_appeals (user_id, text) values (200000000000000042, 'e2e appeal') on conflict do nothing;
insert into raider_actions (guild_id, user_id, action) values (100000000000000001, 200000000000000042, 'alert') on conflict do nothing;
with p as (insert into permits (guild_id, name, rank, perms) values
  (100000000000000001, 'e2e editor', 3, '{dashboard_view,dashboard_edit}'),
  (100000000000000001, 'e2e viewer', 2, '{dashboard_view}'),
  (100000000000000001, 'e2e cases', 1, '{dashboard_view,cases,notes_view}') returning id, name)
insert into permit_roles select id, 100000000000000001,
  case name when 'e2e editor' then 400000000000000001 when 'e2e viewer' then 400000000000000002 else 400000000000000003 end from p;
-- A permit belongs to user ids: the mock users hold these roles, and are listed too.
insert into permit_members (permit_id, guild_id, user_id)
  select id, guild_id, case name when 'e2e editor' then 200000000000000004::bigint
    when 'e2e viewer' then 200000000000000005 else 200000000000000006 end
  from permits where guild_id = 100000000000000001 and name in ('e2e editor', 'e2e viewer', 'e2e cases');
