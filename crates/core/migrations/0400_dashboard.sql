-- Dashboard login sessions and TOTP step-up.
CREATE TABLE dash_session (
    id            TEXT PRIMARY KEY,
    user_id       BIGINT NOT NULL,
    username      TEXT   NOT NULL,
    access_token  TEXT   NOT NULL,
    csrf          TEXT   NOT NULL,
    step_up_until TIMESTAMPTZ,
    expires_at    TIMESTAMPTZ NOT NULL
);

CREATE TABLE dash_totp (
    user_id   BIGINT PRIMARY KEY,
    secret    TEXT NOT NULL,
    confirmed BOOLEAN NOT NULL DEFAULT false
);
