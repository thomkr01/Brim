#!/usr/bin/env bash
# Prints the SQL that follows a dump in a restore (scripts/restore.sh, scripts/restore-test.sh): the
# deletion log's migration when the dump is from before it, the live database's deletion log from
# file $1 (COPY text format), restore_erasures, and the COMMIT. The caller sends BEGIN before the
# dump, so a stream cut anywhere ends without COMMIT and the server rolls everything back.
set -euo pipefail
cd "$(dirname "$0")/.."
migration=crates/core/migrations/4035_erasure_log.sql
[ -r "$migration" ] && [ -r "$1" ]
# A dump empties search_path.
echo 'SET search_path = public;'
# Only for a dump without the log: running the file again would undo a later migration's changes.
echo "SELECT to_regclass('public.erasure_log') IS NULL AS no_log \\gset"
echo '\if :no_log'
cat "$migration"
echo '\endif'
echo 'CREATE TEMP TABLE saved (LIKE erasure_log); COPY saved FROM stdin;'
cat "$1"
echo '\.'
echo 'SELECT restore_erasures() AS log_rows_applied_again;'
echo 'COMMIT;'
