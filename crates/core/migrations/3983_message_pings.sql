-- Message Builder pings: who a send may ping, and the level of whoever saved the message last.
-- A send pings only when that level is Trusted Admin or higher (2 = Dashboard Edit, 3 = TA, 4 = Owner/EO).
ALTER TABLE embeds
    ADD COLUMN pings TEXT NOT NULL DEFAULT 'none' CHECK (pings IN ('none', 'named', 'all')),
    ADD COLUMN saved_level SMALLINT NOT NULL DEFAULT 0;

-- Set with `updated_at` by every save of this image; an older image's save moves only `updated_at`,
-- which switches the pings off until someone with the right level saves again.
ALTER TABLE embeds ADD COLUMN saved_at TIMESTAMPTZ;
