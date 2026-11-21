#!/usr/bin/env bash
# Restores a dump into the live database: scripts/restore.sh <file>. An encrypted copy (*.age, every
# daily copy and every off-site copy) needs the private key: AGE_KEY=<key file> scripts/restore.sh <file>. Deletions made after the
# dump (purged servers, removed Extra Owners, Trusted Admins and permit holders, replaced rescue keys,
# reset 2FA, revoked MCP tokens, carried-out data deletion requests) are applied again, and every
# dashboard login ends (security review). Read any WARNING it prints (DEPLOY.md section 8).
set -euo pipefail
cd "$(dirname "$0")/.."
f=${1:?usage: scripts/restore.sh <dump file>}
umask 077
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
case $f in
  *.age)
    command -v age > /dev/null || { echo "$f is encrypted and age is not installed (sudo apt install age): nothing was restored" >&2; exit 1; }
    [ -r "${AGE_KEY:-}" ] || { echo "$f is encrypted: put the private key (AGE-SECRET-KEY-1...) in a file and run AGE_KEY=<that file> scripts/restore.sh $f (DEPLOY.md section 8). Nothing was restored." >&2; exit 1; }
    age -d -i "$AGE_KEY" -o "$tmp/dump.sql.gz" "$f" || { echo "Could not decrypt $f with the key in $AGE_KEY (another key, or a damaged file): nothing was restored" >&2; exit 1; }
    f=$tmp/dump.sql.gz ;;
esac
# A cut-off file would commit the part before the cut.
gunzip -t "$f"
psql() { docker compose exec -T postgres psql -U brim brim -q -v ON_ERROR_STOP=1 "$@"; }
# Nothing writes or caches during the restore; the log is read after they stopped, so no deletion is missed.
docker compose stop bot dashboard
: > "$tmp/log"
# A new, empty Postgres has no log: then nothing is applied again.
has=$(psql -tA -c "SELECT to_regclass('public.erasure_log') IS NOT NULL") \
  && { [ "$has" = f ] || psql -c '\copy erasure_log TO stdout' > "$tmp/log"; } || {
  echo "Could not read the live database's deletion log; nothing was restored." >&2
  docker compose start bot dashboard
  exit 1
}
# The MCP and dashboard roles: a dump has no roles, and its grants to them stop the restore without them
# (tommybot_mcp: migration 2700 grants to it in every database).
psql -c "DO \$\$ DECLARE r TEXT; BEGIN FOREACH r IN ARRAY ARRAY['brim_mcp', 'brim_dashboard', 'tommybot_mcp'] LOOP IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN EXECUTE format('CREATE ROLE %I NOLOGIN', r); END IF; END LOOP; END \$\$"
# One transaction that stops at the first error, committed only by the last line restore-sql.sh
# prints: a damaged dump or a cut stream changes nothing. The schema is dropped first because a dump
# drops only the tables it holds (newer migrations would then fail to run again).
{ echo 'BEGIN; DROP SCHEMA public CASCADE; CREATE SCHEMA public;'; gunzip -c "$f"; scripts/restore-sql.sh "$tmp/log"; } \
  | psql
docker compose up -d
echo "Restored $1"
