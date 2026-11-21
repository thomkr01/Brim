-- A server went through setup (Auto Setup or the wizard's last step), so Overview stops asking
-- for the wizard. purge_guild removes the row with the rest of the server's data.
CREATE TABLE guild_setup_done (
    guild_id BIGINT PRIMARY KEY,
    done_by  BIGINT NOT NULL,
    at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
