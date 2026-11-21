-- Secure my account (owner decision 2026-11-20): the page checks a Rescue Key and its authenticator code
-- without using the key up, so the last code step used is kept: a press or a claim refuses a code
-- at or below it (no replay within the code's 30 s).
ALTER TABLE rescue_key ADD COLUMN IF NOT EXISTS last_step BIGINT;
