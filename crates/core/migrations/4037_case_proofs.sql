-- Several proofs per case, and the history of every case edit.

-- A proof staff added (a link, a short text, or `image`: the link of the mod log message that holds
-- the file), or `verified`: a copy of the punished member's own message that the bot stored when it
-- acted (text, where and when it was sent, the names of its files; never the files themselves).
-- Both tables go with their case (staff delete, the server's cases limit, a person's data deletion,
-- the server purge). A verified copy also has its own limit (`retention.proofs_days`).
CREATE TABLE case_proofs (
    id         BIGSERIAL PRIMARY KEY,
    guild_id   BIGINT  NOT NULL,
    case_no    INT     NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('link', 'image', 'text')),
    body       TEXT    NOT NULL,
    verified   BOOLEAN NOT NULL DEFAULT false,
    added_by   BIGINT  NOT NULL,
    ai_client  TEXT,
    added_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Verified only: the message it copies.
    channel_id BIGINT,
    message_id BIGINT,
    sent_at    TIMESTAMPTZ,
    files      TEXT[] NOT NULL DEFAULT '{}',
    FOREIGN KEY (guild_id, case_no) REFERENCES cases (guild_id, case_no) ON DELETE CASCADE
);
CREATE INDEX case_proofs_case ON case_proofs (guild_id, case_no);

-- `field`: reason (old and new text), active (open or closed), or proof (a removed proof: only what
-- kind it was, never its content, so removing a proof really removes it).
CREATE TABLE case_edits (
    id        BIGSERIAL PRIMARY KEY,
    guild_id  BIGINT NOT NULL,
    case_no   INT    NOT NULL,
    field     TEXT   NOT NULL CHECK (field IN ('reason', 'active', 'proof')),
    old_value TEXT,
    new_value TEXT,
    editor_id BIGINT NOT NULL,
    ai_client TEXT,
    edited_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (guild_id, case_no) REFERENCES cases (guild_id, case_no) ON DELETE CASCADE
);
CREATE INDEX case_edits_case ON case_edits (guild_id, case_no);

-- The one proof text a case had becomes its first proof.
INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by, added_at)
SELECT guild_id, case_no,
       CASE WHEN proof LIKE 'https://discord.com/channels/%' THEN 'image'
            WHEN proof ~* '^https?://\S+$' THEN 'link' ELSE 'text' END,
       proof, mod_id, created_at
FROM cases WHERE btrim(coalesce(proof, '')) <> '';

-- `cases.proof` stays for one release and nothing reads it any more. An image from before this
-- migration (after a rollback) still writes it: that proof is copied here, so it is not lost when
-- the newer image runs again (PITFALLS: a column that replaces another goes stale after a rollback).
-- A proof the older image replaced or cleared loses its copy here too, so it does not come back.
CREATE FUNCTION case_proof_from_old() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND btrim(coalesce(OLD.proof, '')) <> '' THEN
        DELETE FROM case_proofs WHERE id = (
            SELECT min(id) FROM case_proofs
            WHERE guild_id = OLD.guild_id AND case_no = OLD.case_no AND body = OLD.proof AND NOT verified);
    END IF;
    IF btrim(coalesce(NEW.proof, '')) <> '' THEN
        INSERT INTO case_proofs (guild_id, case_no, kind, body, added_by)
        VALUES (NEW.guild_id, NEW.case_no,
                CASE WHEN NEW.proof LIKE 'https://discord.com/channels/%' THEN 'image'
                     WHEN NEW.proof ~* '^https?://\S+$' THEN 'link' ELSE 'text' END,
                NEW.proof, NEW.mod_id);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER case_proof_from_old_insert AFTER INSERT ON cases FOR EACH ROW
    WHEN (btrim(coalesce(NEW.proof, '')) <> '') EXECUTE FUNCTION case_proof_from_old();
CREATE TRIGGER case_proof_from_old_update AFTER UPDATE OF proof ON cases FOR EACH ROW
    WHEN (NEW.proof IS DISTINCT FROM OLD.proof) EXECUTE FUNCTION case_proof_from_old();

-- A person's data deletion keeps a case without the person (`persondata::ANONYMIZE`: the newest
-- case, a running timed action). Its proofs (their message copies) and history (old reasons) go
-- then. The rule is here, not in the code, so it also holds when an older image runs the deletion.
CREATE FUNCTION case_forget_person() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM case_proofs WHERE guild_id = NEW.guild_id AND case_no = NEW.case_no;
    DELETE FROM case_edits WHERE guild_id = NEW.guild_id AND case_no = NEW.case_no;
    RETURN NULL;
END $$;
CREATE TRIGGER case_forget_person AFTER UPDATE OF user_id ON cases FOR EACH ROW
    WHEN (NEW.user_id = 0 AND OLD.user_id <> 0) EXECUTE FUNCTION case_forget_person();

-- What the read-only MCP role sees as a case's proof: what staff added, as before. The copies of
-- members' messages are left out (the role has no grant on the tables themselves).
CREATE OR REPLACE VIEW mcp_cases AS
    SELECT guild_id, case_no, kind, user_id, mod_id, reason,
           (SELECT string_agg(p.body, E'\n' ORDER BY p.id) FROM case_proofs p
             WHERE p.guild_id = cases.guild_id AND p.case_no = cases.case_no AND NOT p.verified) AS proof,
           points, duration_s, expires_at, active, created_at
    FROM cases;
