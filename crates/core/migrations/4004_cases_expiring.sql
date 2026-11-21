-- The scheduler ends expired cases (`jobs::expire_cases`) every 5 seconds, and it scanned all of
-- `cases`, every server's history, each time. This index holds only the active cases that can expire.
CREATE INDEX cases_expiring ON cases (expires_at) WHERE active AND expires_at IS NOT NULL;
