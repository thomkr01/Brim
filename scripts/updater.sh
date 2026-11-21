#!/bin/sh
# Runs in the updater container. Waits for an update request from the dashboard (a queued row in
# update_runs), pulls exactly the release the bot owner approved there (by image digest) for bot,
# dashboard and the MCP server when it runs, recreates them, checks they stay up, and rolls back to the
# previous images if anything fails. Progress goes to the same row for the dashboard.
set -u
DC="docker compose --project-directory /project -f /project/docker-compose.yml"
OLD=/tmp/old-images

q() { psql "$DATABASE_URL" -Atq -v ON_ERROR_STOP=1 "$@"; }
# The role and database from DATABASE_URL (postgres://user:pass@host:port/db), not fixed names.
DB_USER=${DATABASE_URL#*://}; DB_USER=${DB_USER%%[:@]*}
DB_NAME=${DATABASE_URL##*/}; DB_NAME=${DB_NAME%%\?*}

# Starts step $1, recording how long the previous one took.
step() {
  q -v id="$RUN" -v n="$1" <<'SQL'
update update_runs set step = :n, step_started_at = now(),
  step_times = case when :n = 0 then '{}' else step_times || extract(epoch from now() - step_started_at)::int end
where id = :id;
SQL
}

# Ends the run with status $1 and message $2 (empty for none).
finish() {
  q -v id="$RUN" -v st="$1" -v err="$2" <<'SQL'
update update_runs set status = :'st', error = nullif(:'err', ''), finished_at = now(),
  step_times = case when :'st' = 'done' then step_times || extract(epoch from now() - step_started_at)::int else step_times end
where id = :id;
SQL
}

# Puts the images from before the update back under their tags and restarts on them. The old version
# is checked like the new one: it can crash too, for example on a migration it does not know.
rollback() {
  while read -r svc ref digest; do docker tag "${ref%:*}:rollback" "$ref"; done < "$OLD"
  if ! $DC up -d --no-deps --no-build --force-recreate $SVCS > /dev/null 2>&1; then
    finish failed "$1 Rolling back failed too: run 'docker compose up -d' on the server."
  elif settled; then
    finish rolled_back "$1 The previous version is running again."
  else
    finish failed "$1 The previous version does not start either: check 'docker compose logs $SVCS' on the server."
  fi
}

# Whether every updated service is running and has not restarted, and the dashboard answers.
healthy() {
  for svc in $SVCS; do
    c=$($DC ps -q "$svc")
    [ "$(docker inspect -f '{{.State.Running}} {{.RestartCount}}' "$c" 2> /dev/null)" = "true 0" ] || return 1
  done
  wget -q -T 5 -O /dev/null http://dashboard:8080/
}

# Up and answering within a minute, then still up 15 seconds later (catches crash loops).
# SETTLE_TRIES and SETTLE_SECS are set only by the updater test (tests/e2e/updater.sh), to wait less.
settled() {
  i=0
  until healthy; do
    i=$((i + 1))
    [ "$i" -lt "${SETTLE_TRIES:-30}" ] || return 1
    sleep 2
  done
  sleep "${SETTLE_SECS:-15}"
  healthy
}

# Whether GitHub holds the signed build record (attestation) that the Images workflow on main of
# the Brim repository built digest $2 as image $1: the updater checks it itself, so a queued row
# alone (the dashboard's database role can write one) can never install other bytes. The dashboard
# checks the same before it queues (crates/dashboard/src/update.rs `attests`). Prints the reason when
# it is not.
# shortcut: trusts GitHub's list without checking the Sigstore signature, as the dashboard does.
ATTEST=${ATTEST_URL:-https://api.github.com/repos/thomkr01/Brim/attestations}
attested() {
  rm -f /tmp/attest.json
  wget -q -T 20 -O /tmp/attest.json -U Brim-updater "$ATTEST/$2" 2> /dev/null || { echo "GitHub did not answer or holds no signed build record for it"; return 1; }
  # psql reads the answer from the file (`\set` with backticks), so its size has no command-line limit.
  ok=$(q -v name="$1" -v hex="${2#sha256:}" <<'SQL' 2> /dev/null
\set j `cat /tmp/attest.json`
select exists (
  select 1 from jsonb_array_elements(:'j'::jsonb -> 'attestations') a,
    lateral (select convert_from(decode(a #>> '{bundle,dsseEnvelope,payload}', 'base64'), 'UTF8')::jsonb s) st
  where st.s -> 'subject' @> jsonb_build_array(jsonb_build_object('name', :'name', 'digest', jsonb_build_object('sha256', :'hex')))
    and st.s #>> '{predicate,buildDefinition,externalParameters,workflow,repository}' = 'https://github.com/thomkr01/Brim'
    and st.s #>> '{predicate,buildDefinition,externalParameters,workflow,path}' = '.github/workflows/images.yml'
    and st.s #>> '{predicate,buildDefinition,externalParameters,workflow,ref}' = 'refs/heads/main');
SQL
)
  [ "$ok" = t ] || { echo "GitHub holds no signed build record from the Images workflow on main for it"; return 1; }
}

rev() { docker image inspect -f '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$1" 2> /dev/null; }

run() {
  step 0
  # The build the owner approved: its commit, and the digest of each image (resolved and checked
  # against GitHub's signed build record by the dashboard). Nothing else is ever pulled.
  target=$(q -v id="$RUN" <<'SQL'
select coalesce(target_sha, '') from update_runs where id = :id and target_sha ~ '^[0-9a-f]{40}$';
SQL
)
  [ -n "$target" ] || { finish failed "This update names no approved build (it was started from an older dashboard). Nothing was changed. Reload the Update page and start it again."; return; }
  SVCS="bot dashboard"
  # The MCP server is optional (compose profile mcp): updated with the rest only while it runs, so a
  # stopped one is never started (and never fails the health check).
  [ -n "$($DC ps -q --status running mcp 2> /dev/null)" ] && SVCS="$SVCS mcp"
  : > "$OLD"
  for svc in $SVCS; do
    c=$($DC ps -aq "$svc")
    [ -n "$c" ] || { finish failed "The $svc container was not found. Start Brim with docker compose first."; return; }
    ref=$(docker inspect -f '{{.Config.Image}}' "$c")
    # A pinned tag names one build; moving it to another would make it lie.
    [ "${ref##*:}" = latest ] || { finish failed "BRIM_TAG (or TOMMYBOT_TAG) is set in .env on the server, so $svc runs a pinned version. Remove that line and run docker compose up -d, then update again. Nothing was changed."; return; }
    digest=$(q -v id="$RUN" -v svc="$svc" <<'SQL'
select coalesce(digests ->> :'svc', '') from update_runs where id = :id and digests ->> :'svc' ~ '^sha256:[0-9a-f]{64}$';
SQL
)
    [ -n "$digest" ] || { finish failed "The approved build has no image digest for $svc. Nothing was changed. Reload the Update page and start it again."; return; }
    why=$(attested "${ref%:*}" "$digest") || { finish failed "The $svc image of the approved build is not installed: $why. Nothing was changed."; return; }
    # A named copy of the running image, since the new one takes its tag (and the old may be deleted).
    docker tag "$(docker inspect -f '{{.Image}}' "$c")" "${ref%:*}:rollback" || { finish failed "Could not save the $svc image for a rollback."; return; }
    echo "$svc $ref $digest" >> "$OLD"
  done
  dref=$(awk '$1 == "dashboard" { print $2 }' "$OLD")
  if [ "$target" = "$(rev "${dref%:*}:rollback")" ]; then
    finish failed "The approved build ($(echo "$target" | cut -c1-7)) is the version already running. Nothing was changed."
    return
  fi

  step 1
  # By digest: the registry cannot hand over anything but the approved bytes. The tags compose runs
  # move only after the backup, so a run that stops before it leaves every tag as it was.
  while read -r svc ref digest; do
    img="${ref%:*}@$digest"
    if ! out=$(timeout 900 docker pull -q "$img" 2>&1); then
      finish failed "Downloading the $svc image of the approved build failed, nothing was changed: $(echo "$out" | tail -c 300)"
      return
    fi
    got=$(rev "$img")
    [ "$got" = "$target" ] || { finish failed "The $svc image of the approved build says it is version ${got:-unknown}, not $(echo "$target" | cut -c1-7). Nothing was changed."; return; }
    # Only releases are installed: the Images workflow sets this label only for a release tag on main,
    # and the label is part of the signed digest. A build of main from before releases has none.
    ver=$(docker image inspect -f '{{index .Config.Labels "org.opencontainers.image.version"}}' "$img" 2> /dev/null)
    printf '%s\n' "$ver" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+' || { finish failed "The $svc image of build $(echo "$target" | cut -c1-7) is not a release. Only releases are installed. Nothing was changed."; return; }
    # A dashboard from before approved builds queues runs this updater refuses, and one from before
    # the rename to Brim (no org.brim.update) checks only the old repository: installing either would
    # leave the Update button with no way forward.
    if [ "$svc" = dashboard ] && [ "$(docker image inspect -f '{{index .Config.Labels "org.brim.update"}}' "$img")" != digest ]; then
      finish failed "Build $(echo "$target" | cut -c1-7) is older than installing approved builds, so its Update page could not update again. Pick a newer build. Nothing was changed."
      return
    fi
  done < "$OLD"
  # psql only fills :'vars' in stdin, never in -c.
  q -v id="$RUN" -v sha="$target" <<'SQL'
update update_runs set to_sha = :'sha' where id = :id;
SQL

  step 2
  # A rollback brings back only the old images, not the data a new migration changed or dropped.
  # Made here, after the checks that can end the run, so a run that changes nothing never rotates
  # out the dump from before a migration. The server's own pg_dump (a newer one writes settings an
  # older server refuses); -Z gzips the whole plain dump. timeout runs inside the container, so it
  # stops pg_dump itself, not only the exec client.
  grep -q ' /backups ' /proc/mounts || { finish failed "No backup folder is mounted: run git pull && docker compose up -d on the server, then update again."; return; }
  free=$(df -Pk /backups | awk 'NR == 2 { print $4 }')
  size=$(q -c "select pg_database_size(current_database()) / 1024")
  if [ -z "$free" ] || [ -z "$size" ] || [ "$free" -lt $((size * 2)) ]; then
    finish failed "Not enough free disk space for the database backup before the update (free: $((${free:-0} / 1024)) MB, needed: twice the database, $((${size:-0} * 2 / 1024)) MB). Nothing was changed."
    return
  fi
  dump=/backups/pre-update-$RUN.sql.gz
  if ! err=$( (umask 077; timeout 960 $DC exec -T postgres timeout 900 pg_dump --lock-wait-timeout=60s --clean --if-exists -Z 6 -U "$DB_USER" "$DB_NAME" > "$dump.tmp") 2>&1) || ! gunzip -t "$dump.tmp" 2> /dev/null; then
    rm -f "$dump.tmp"
    finish failed "The database backup before the update failed, nothing was changed: $(echo "${err:-the dump did not finish within 15 minutes or is not a complete gzip file}" | tail -c 300)"
    return
  fi
  mv "$dump.tmp" "$dump"
  # Keep the newest three.
  ls -1t /backups/pre-update-*.sql.gz | tail -n +4 | xargs -r rm -f
  while read -r svc ref digest; do
    docker tag "${ref%:*}@$digest" "$ref" || { rollback "Could not tag the new $svc image."; return; }
  done < "$OLD"
  # --force-recreate: with a pinned tag the pull brings the same image, and the old container's
  # restart count would fail the health check.
  out=$($DC up -d --no-deps --no-build --force-recreate bot 2>&1) || { rollback "Restarting the bot failed: $(echo "$out" | tail -c 300)"; return; }
  step 3
  out=$($DC up -d --no-deps --no-build --force-recreate dashboard 2>&1) || { rollback "Restarting the dashboard failed: $(echo "$out" | tail -c 300)"; return; }
  case " $SVCS " in *" mcp "*)
    out=$($DC up -d --no-deps --no-build --force-recreate mcp 2>&1) || { rollback "Restarting the MCP server failed: $(echo "$out" | tail -c 300)"; return; } ;;
  esac

  step 4
  settled || { rollback "The new version did not start properly."; return; }
  finish done ""
}

# The bot runs the migrations and starts at the same time as this container: wait until the tables
# and columns this script uses exist, so its first writes do not fail on an older schema.
until q -c "select target_sha, digests from update_runs limit 0" > /dev/null 2>&1 \
  && q -c "select sha, sha_at from updater_seen limit 0" > /dev/null 2>&1; do
  sleep 3
done
# A run cut off by a restart of this container cannot be resumed safely.
q -c "update update_runs set status = 'failed', error = 'The updater restarted during the update. Check the server and try again.', finished_at = now() where status = 'running'"
n=0
while :; do
  # The heartbeat carries this updater's commit: the dashboard warns when it lags behind.
  [ $((n % 10)) -eq 0 ] && q -v sha="${BRIM_SHA:-}" <<'SQL'
insert into updater_seen (id, at, sha, sha_at) values (true, now(), nullif(nullif(:'sha', ''), 'dev'), now())
on conflict (id) do update set at = now(), sha = excluded.sha, sha_at = now();
SQL
  # Runs are done one at a time inside this loop, so a row still 'running' here lost its last
  # write (the database was briefly unreachable); without this the dashboard would wait forever.
  [ $((n % 10)) -eq 0 ] && q -c "update update_runs set status = 'failed', error = 'The updater could not record the end of the update because the database did not answer. Check the server and try again.', finished_at = now() where status = 'running'"
  n=$((n + 1))
  RUN=$(q -c "update update_runs set status = 'running' where id = (select min(id) from update_runs where status = 'queued') returning id" 2> /dev/null)
  [ -n "$RUN" ] && run
  sleep 3
done
