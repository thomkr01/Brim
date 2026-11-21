-- The server a dashboard 2FA step-up was done for (security review): it counts there only, in the
-- dashboard and for the 2FA-gated chat actions. NULL: done on a page without a server.
ALTER TABLE dash_session ADD COLUMN step_up_guild BIGINT;
