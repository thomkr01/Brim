-- Wrong 2FA codes and rescue claim or Secure my account tries, counted in the database so a
-- dashboard restart does not reset the lockouts. `kind` names the counter: totp, totp-mcp, claim
-- or press. Rows older than an hour are deleted after each try.
CREATE TABLE dash_fails (
    kind TEXT NOT NULL,
    user_id BIGINT NOT NULL,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX dash_fails_kind_user_at ON dash_fails (kind, user_id, at);
CREATE INDEX dash_fails_at ON dash_fails (at);

-- The newest authenticator time step used per user: a code is accepted once, also after a restart.
ALTER TABLE dash_totp ADD COLUMN last_step BIGINT;
