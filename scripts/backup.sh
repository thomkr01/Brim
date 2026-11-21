#!/usr/bin/env bash
# Daily dump, age-encrypted: keep 7 local copies, upload them to RCLONE_REMOTE (DEPLOY.md section 8).
set -euo pipefail
cd "$(dirname "$0")/.."
# The dump holds 2FA secrets and session tokens: only the owner may read it.
umask 077
# Only these values, read as text: sourcing .env as shell breaks on (or runs) values with
# spaces, `&`, `;` or `$(`.
env_val() { [ -f .env ] && grep -E "^$1=" .env | tail -n 1 | cut -d= -f2- | sed -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/" || true; }
BACKUP_DIR=${BACKUP_DIR:-$(env_val BACKUP_DIR)}
RCLONE_REMOTE=${RCLONE_REMOTE:-$(env_val RCLONE_REMOTE)}
RCLONE_CONFIG_DIR=${RCLONE_CONFIG_DIR:-$(env_val RCLONE_CONFIG_DIR)}
BACKUP_AGE_RECIPIENT=${BACKUP_AGE_RECIPIENT:-$(env_val BACKUP_AGE_RECIPIENT)}
dir=${BACKUP_DIR:-$HOME/brim-backups}
mkdir -p "$dir"
file=$dir/$(date +%F).sql.gz
# A failed dump or encryption leaves no half file behind.
trap 'rm -f -- "$dir"/*.tmp' EXIT
dump() { docker compose exec -T postgres pg_dump --clean --if-exists -U brim brim | gzip; }
# security review and owner decision 2026-11-19: once BACKUP_AGE_RECIPIENT is set, every copy, the local
# ones too, is age-encrypted to a public key whose private key the owner keeps off the server, so a
# copy of the backup folder (with data deleted since), the provider account or a stolen rclone
# config cannot read it. The dump is encrypted in the pipe; only on the day of the
# monthly restore test is a plain file written first (mode 600), and it is removed once encrypted.
plain=
# A key age refuses (a typo) also gives a plain copy, not none.
[ -n "${BACKUP_AGE_RECIPIENT:-}" ] && command -v age > /dev/null \
  && age -r "$BACKUP_AGE_RECIPIENT" < /dev/null > /dev/null 2>&1 || plain=1
# The monthly restore test runs on the dump before it is encrypted, so the server needs no private key.
monthly=
[ "$(date +%d)" != 01 ] || monthly=1
failed=
if [ -z "$plain$monthly" ]; then
  file=$file.age
  dump | age -r "$BACKUP_AGE_RECIPIENT" > "$file.tmp"
else
  dump > "$file.tmp"
  [ -z "$monthly" ] || scripts/restore-test.sh "$file.tmp" >> "$dir/restore-test.log" 2>&1 \
    || failed="the monthly restore test failed (see $dir/restore-test.log)"
  if [ -z "$plain" ]; then
    age -r "$BACKUP_AGE_RECIPIENT" -o "$file.age.tmp" "$file.tmp"
    rm -f -- "$file.tmp"
    file=$file.age
  fi
fi
mv "$file.tmp" "$file"
find "$dir" \( -name '*.sql.gz' -o -name '*.sql.gz.age' \) -mtime +7 -delete
if [ -n "$plain" ]; then
  # Without the key a plain copy is still made (backups never stop), and cron reports it every day.
  why="BACKUP_AGE_RECIPIENT is not set"
  if [ -n "${BACKUP_AGE_RECIPIENT:-}" ]; then
    why="age is not installed (sudo apt install age)"
    ! command -v age > /dev/null || why="BACKUP_AGE_RECIPIENT is not a valid age public key (age1...)"
  fi
  echo "$why: $file was saved UNENCRYPTED and nothing was uploaded (DEPLOY.md section 8)" >&2
  [ -z "$failed" ] || echo "Also, $failed" >&2
  exit 1
fi
# The remote is optional: without RCLONE_REMOTE only the local copies are kept.
if [ -n "${RCLONE_REMOTE:-}" ]; then
  # The image is pinned by digest in docker-compose.yml (it reads the dumps and holds the remote's
  # credential). No delete here: the remote's own lifecycle rule removes copies after 30 days,
  # and its object lock stops anyone, this key included, from deleting them earlier. rclone skips
  # copies the remote already has, so a copy a failed day could not send goes with the next one.
  docker compose run --rm -T -v "${RCLONE_CONFIG_DIR:-$HOME/.config/rclone}:/config/rclone" -v "$dir:/data:ro" \
    rclone copy /data "$RCLONE_REMOTE" --include '/*.sql.gz.age'
fi
[ -z "$failed" ] || { echo "$file was saved, but $failed" >&2; exit 1; }
