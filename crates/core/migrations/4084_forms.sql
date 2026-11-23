-- Forms: applications (staff accept or deny) and plain forms (answers are only collected). One
-- engine for every way to answer; `brim_core::forms::submit` is the only writer of responses.
CREATE TABLE forms (
    id          BIGSERIAL PRIMARY KEY,
    guild_id    BIGINT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('application', 'form')),
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- "Accepting responses". The open and close jobs set it at `opens_at` and `closes_at`.
    open        BOOLEAN NOT NULL DEFAULT true,
    -- The questions and the settings of the tabs, typed and checked in `brim_core::forms::Body`.
    body        JSONB NOT NULL,
    opens_at    TIMESTAMPTZ,
    closes_at   TIMESTAMPTZ,
    next_number INT NOT NULL DEFAULT 1,
    created_by  BIGINT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX forms_guild ON forms (guild_id);

CREATE TABLE form_responses (
    id           BIGSERIAL PRIMARY KEY,
    form_id      BIGINT NOT NULL REFERENCES forms (id) ON DELETE CASCADE,
    guild_id     BIGINT NOT NULL,
    number       INT NOT NULL,
    -- NULL on an anonymous form: `user_key` stands for the member there. Its key is FORMS_KEY in
    -- .env, never a column: no dump or database role can tie it back to a member.
    user_id      BIGINT,
    user_key     TEXT,
    status       TEXT NOT NULL CHECK (status IN ('open', 'accepted', 'denied', 'collected', 'withdrawn')),
    answers      JSONB NOT NULL,
    via          TEXT NOT NULL CHECK (via IN ('popup', 'dm', 'web')),
    started_at   TIMESTAMPTZ,
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_by   BIGINT,
    decided_at   TIMESTAMPTZ,
    reason       TEXT,
    review_msg   BIGINT,
    UNIQUE (form_id, number)
);
-- The user first: a data request asks by user only, a send asks by form and user.
CREATE INDEX form_responses_user ON form_responses (user_id, form_id, submitted_at) WHERE user_id IS NOT NULL;
CREATE INDEX form_responses_key ON form_responses (form_id, user_key) WHERE user_key IS NOT NULL;
-- Read from a button press on the review post.
CREATE INDEX form_responses_review ON form_responses (review_msg) WHERE review_msg IS NOT NULL;

-- A pop-up chain or a DM form half way. Never a response; it goes 30 minutes after the last step.
-- No `guild_id`: the server purge reaches it through the form.
CREATE TABLE form_drafts (
    form_id    BIGINT NOT NULL REFERENCES forms (id) ON DELETE CASCADE,
    user_id    BIGINT NOT NULL,
    via        TEXT NOT NULL CHECK (via IN ('popup', 'dm', 'web')),
    answers    JSONB NOT NULL,
    step       SMALLINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, form_id)
);
CREATE INDEX form_drafts_expires ON form_drafts (expires_at);

CREATE TABLE form_templates (
    id         BIGSERIAL PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    name       TEXT NOT NULL,
    body       JSONB NOT NULL,
    created_by BIGINT NOT NULL
);
CREATE INDEX form_templates_guild ON form_templates (guild_id);

CREATE TABLE form_panels (
    id         BIGSERIAL PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    channel_id BIGINT,
    message_id BIGINT,
    forms      BIGINT[] NOT NULL,
    message    JSONB NOT NULL
);
CREATE INDEX form_panels_guild ON form_panels (guild_id);
CREATE INDEX form_panels_msg ON form_panels (message_id) WHERE message_id IS NOT NULL;
