-- One person's rows across all servers: their data request, and the hourly sweep that finishes
-- a delete request (a warn it kept until it ended, a report, appeal or ticket it kept while open).
CREATE INDEX cases_person ON cases (user_id);
CREATE INDEX tickets_person ON tickets (user_id);
CREATE INDEX reports_target ON reports (target_id);
CREATE INDEX reports_reporter ON reports (reporter_id);
CREATE INDEX appeals_person ON appeals (user_id);
