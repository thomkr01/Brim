-- When an appeal was decided. The bot gives a member a little more automod heat for 7 days after
-- an accepted appeal (owner decision 2026-11-22) and reads this to know who.
ALTER TABLE appeals ADD COLUMN decided_at TIMESTAMPTZ;
