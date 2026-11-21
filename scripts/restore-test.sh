#!/usr/bin/env bash
# Restores a dump (default: the newest daily copy) into a throwaway Postgres and checks the key
# tables: scripts/restore-test.sh [file]. Exit 1 on failure. backup.sh runs it monthly on the dump
# before encrypting it.
set -euo pipefail
cd "$(dirname "$0")/.."
dir=${BACKUP_DIR:-$HOME/brim-backups}
file=${1:-$(ls -1 "$dir"/*.sql.gz "$dir"/*.sql.gz.age 2>/dev/null | sort | tail -n1 || true)}
[ -n "$file" ] || { echo "no dump in $dir" >&2; exit 1; }
echo "restoring $file"
name=brim-restore-test
umask 077
tmp=$(mktemp -d)
trap 'docker rm -fv $name > /dev/null 2>&1 || true; rm -rf -- "$tmp"' EXIT
# An encrypted copy needs the private key, which is kept off the server (DEPLOY.md section 8).
case $file in
  *.age)
    command -v age > /dev/null || { echo "FAIL: $file is encrypted and age is not installed (sudo apt install age)" >&2; exit 1; }
    [ -r "${AGE_KEY:-}" ] || { echo "FAIL: $file is encrypted: put the private key in a file and run AGE_KEY=<that file> scripts/restore-test.sh" >&2; exit 1; }
    age -d -i "$AGE_KEY" -o "$tmp/dump.sql.gz" "$file" || { echo "FAIL: could not decrypt $file with the key in $AGE_KEY (another key, or a damaged file)" >&2; exit 1; }
    file=$tmp/dump.sql.gz ;;
esac
# A cut-off file fails here with a clear message, not halfway through the restore.
gunzip -t "$file" || { echo "FAIL: $file is damaged (gunzip -t)" >&2; exit 1; }
# -v on every rm: the image's data folder is an anonymous volume, which would keep a plain copy of
# the restored database after the container is gone.
docker rm -fv $name > /dev/null 2>&1 || true
# The same image as the live database (docker-compose.yml, kept current by Dependabot).
docker run -d --name $name -e POSTGRES_USER=brim -e POSTGRES_PASSWORD=test -e POSTGRES_DB=brim "$(docker compose config --images postgres)" > /dev/null
# The image restarts postgres once after init, so wait for two queries in a row to succeed.
ok=0
for _ in $(seq 60); do
  if docker exec $name psql -U brim -qc 'select 1' > /dev/null 2>&1; then ok=$((ok + 1)); else ok=0; fi
  [ $ok -ge 2 ] && break
  sleep 1
done
[ $ok -ge 2 ] || { echo "postgres did not start" >&2; exit 1; }
# pg_dump has no roles, but the dump grants to these (tommybot_mcp: migration 2700, in every database).
docker exec $name psql -U brim -q -v ON_ERROR_STOP=1 brim -c 'CREATE ROLE brim_mcp NOLOGIN' -c 'CREATE ROLE brim_dashboard NOLOGIN' -c 'CREATE ROLE tommybot_mcp NOLOGIN' > /dev/null
# The same restore as scripts/restore.sh, with an empty deletion log: the whole schema replaced in one
# transaction, with the deletion log step on this dump.
{ echo 'BEGIN; DROP SCHEMA public CASCADE; CREATE SCHEMA public;'; gunzip -c "$file"; scripts/restore-sql.sh /dev/null; } \
  | docker exec -i $name psql -U brim -q -v ON_ERROR_STOP=1 brim > /dev/null 2> "$tmp/err" \
  || { echo "FAIL: the restore stopped: $(grep -m1 ERROR "$tmp/err")" >&2; exit 1; }
# Only the ERROR line is shown: psql's DETAIL and CONTEXT lines quote rows (2FA secrets, tokens).
ok=$(docker exec $name psql -U brim -tA -v ON_ERROR_STOP=1 brim -c "select has_table_privilege('brim_mcp', 'mcp_cases', 'select') or has_table_privilege('tommybot_mcp', 'mcp_cases', 'select')")
[ "$ok" = t ] || { echo "FAIL: the MCP role lost its grants" >&2; exit 1; }
# guild_config has a row for every server the bot ever saved; an empty one is a dump of the wrong
# (or an empty) database. The other tables only have to exist.
for t in guild_config guild_owners cases action_log backups scheduled_jobs; do
  n=$(docker exec $name psql -U brim -tA -v ON_ERROR_STOP=1 brim -c "select count(*) from $t") \
    || { echo "FAIL: table $t missing" >&2; exit 1; }
  echo "$t: $n rows"
  if [ "$t" = guild_config ] && [ "$n" -eq 0 ]; then echo "FAIL: guild_config is empty" >&2; exit 1; fi
done
echo OK
