#!/usr/bin/env bash
# Runs the real updater image against a throwaway compose project and a local registry: one good
# update, one whose dashboard crashes and must be rolled back, and one that also breaks the old version
# (like a migration the old binary does not know), which must be reported as failed. Needs Docker. Run from the repo root.
set -euo pipefail
reg=localhost:5555
dir=$(mktemp -d)
mkdir "$dir/m" "$dir/b" "$dir/att"
cat crates/core/migrations/1700_update_runs.sql crates/core/migrations/4014_update_target.sql > "$dir/schema.sql"
docker build -q --target updater -t brim-updater:test . > /dev/null
docker run -d --rm --name updtest-registry -p 5555:5000 registry:2 > /dev/null
dc() { docker compose --project-directory "$dir" "$@"; }
trap 'dc down -v -t 1 > /dev/null 2>&1; docker rm -f updtest-registry > /dev/null; rm -rf "$dir"' EXIT

# push <service> <sha> <cmd>: publishes a tiny image standing in for a Brim release ($VER, v1.0.0
# by default, is its version label) and keeps its
# registry digest in $dir/<service>.digest (what the dashboard approves). It is tagged under
# 127.0.0.1 so the local build does not take the tag of the image compose runs, as CI never would.
push() {
  # The service label keeps the bot and mcp images apart: with the same bytes they share one digest, and
  # so one record, on an engine that builds without BuildKit.
  printf 'FROM busybox:1.36\nLABEL org.opencontainers.image.revision=%s org.opencontainers.image.version=%s %s=%s test.service=%s\nCMD ["sh", "-c", "%s"]\n' "$2" "${VER:-v1.0.0}" "${LABEL:-org.brim.update}" "${4:-digest}" "$1" "$3" \
    | docker build -q -t "127.0.0.1:5555/$1:latest" - > /dev/null
  docker push -q "127.0.0.1:5555/$1:latest" > /dev/null
  docker inspect -f '{{index .RepoDigests 0}}' "127.0.0.1:5555/$1:latest" | sed 's/.*@//' > "$dir/$1.digest"
  attest "$1"
}
# attest <service>: GitHub's signed build record for the last pushed digest, as the stub GitHub serves it.
# $REPO: the repository the record names (Brim by default).
attest() {
  d=$(cat "$dir/$1.digest")
  st=$(printf '{"subject":[{"name":"%s/%s","digest":{"sha256":"%s"}}],"predicate":{"buildDefinition":{"externalParameters":{"workflow":{"repository":"%s","path":".github/workflows/images.yml","ref":"refs/heads/main"}}}}}' "$reg" "$1" "${d#sha256:}" "${REPO:-https://github.com/thomkr01/Brim}" | base64 -w0)
  printf '{"attestations":[{"bundle":{"dsseEnvelope":{"payload":"%s"}}}]}' "$st" > "$dir/att/$d"
}
# The marker in /m stands in for a newer migration: a version that finds it does not start.
serve='[ ! -e /m/new ] || exit 1; mkdir -p /w && echo ok > /w/index.html && exec httpd -f -p 8080 -h /w'
sha() { printf "$1%.0s" $(seq 40); }

cat > "$dir/docker-compose.yml" <<YML
name: updtest
services:
  postgres:
    image: postgres:16-alpine
    environment: { POSTGRES_PASSWORD: x }
    healthcheck: { test: ["CMD", "pg_isready", "-h", "127.0.0.1", "-U", "postgres"], interval: 1s, retries: 30 }
  bot: { image: "$reg/bot:latest", restart: unless-stopped }
  mcp: { image: "$reg/mcp:latest", restart: unless-stopped }
  dashboard: { image: "$reg/dashboard:latest", restart: unless-stopped, volumes: ["$dir/m:/m"] }
  # Stands in for GitHub's attestation API.
  attest: { image: "busybox:1.36", command: ["httpd", "-f", "-p", "8080", "-h", "/att"], volumes: ["$dir/att:/att:ro"] }
  updater:
    image: brim-updater:test
    depends_on: { postgres: { condition: service_healthy } }
    # Shorter health waits than on a server: these stand-in images start at once.
    environment: { DATABASE_URL: "postgres://postgres:x@postgres:5432/postgres", ATTEST_URL: "http://attest:8080", SETTLE_TRIES: 10, SETTLE_SECS: 5 }
    volumes: ["/var/run/docker.sock:/var/run/docker.sock", ".:/project:ro", "$dir/b:/backups"]
YML

sql() { dc exec -T postgres psql -U postgres -Atqc "$1"; }
label() { docker inspect -f '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$(dc ps -q "${1:-dashboard}")"; }
# update <sha> [bot digest]: queues a run for that build (the last pushed digests, as the dashboard
# would) and prints "status|to_sha|steps" once the updater has finished it.
update() {
  sql "insert into update_runs (requested_by, target_sha, digests) values (1, '$1',
    jsonb_build_object('bot', '${2:-$(cat "$dir/bot.digest")}', 'dashboard', '$(cat "$dir/dashboard.digest")', 'mcp', '$(cat "$dir/mcp.digest")'))"
  update_wait
}
update_wait() {
  for _ in $(seq 240); do
    r=$(sql "select status, to_sha, cardinality(step_times) from update_runs order by id desc limit 1")
    case $r in queued* | running*) sleep 1 ;; *) echo "$r"; return ;; esac
  done
  echo timeout
}
fail() { echo "FAIL: $*"; dc logs updater | tail -20; exit 1; }

push bot "$(sha a)" 'exec sleep 3600'
push mcp "$(sha a)" 'exec sleep 3600'
push dashboard "$(sha a)" "$serve"
# --pull always: a local tag left by an earlier run would start the wrong version.
dc up -d --wait --pull always postgres bot dashboard mcp attest > /dev/null 2>&1
# The updater starts before the migrations, as on a deploy where the bot migrates as it starts.
dc up -d updater > /dev/null 2>&1
sleep 5
dc exec -T postgres psql -U postgres -q < "$dir/schema.sql"
for _ in $(seq 30); do [ -n "$(sql 'select at from updater_seen')" ] && break; sleep 1; done
[ -n "$(sql 'select at from updater_seen')" ] || fail "no heartbeat"
dc logs updater 2>&1 | grep -q ERROR && fail "the updater wrote before the migrations ran"

# Refused, nothing pulled: a run with no approved build (an older dashboard), and a digest whose
# image is another version than the approved one.
sql "insert into update_runs (requested_by) values (1)"
r=$(update_wait)
case $r in failed*) ;; *) fail "run without an approved build: $r" ;; esac
a_bot=$(cat "$dir/bot.digest")
push bot "$(sha b)" 'exec sleep 3600'
push mcp "$(sha b)" 'exec sleep 3600'
push dashboard "$(sha b)" "$serve"
r=$(update "$(sha b)" "$a_bot")
case $r in failed*) ;; *) fail "digest of another version: $r" ;; esac
[ "$(label)" = "$(sha a)" ] || fail "dashboard changed by a refused run: $(label) $(sql "select error from update_runs order by id desc limit 1")"
# A digest with no signed build record (a row written straight into the database) is refused.
rm "$dir/att/$(cat "$dir/bot.digest")"
r=$(update "$(sha b)")
case $r in failed*) ;; *) fail "digest without a signed build record: $r" ;; esac
case $(sql "select error from update_runs order by id desc limit 1") in
  *"signed build record"*) ;; *) fail "missing build record not explained" ;;
esac
[ "$(label)" = "$(sha a)" ] || fail "dashboard changed by a run without a build record"

# A record from another repository (the old one too) is refused.
REPO=https://github.com/thomkr01/TommyBot attest bot
r=$(update "$(sha b)")
case $r in failed*) ;; *) fail "record from the old repository: $r" ;; esac
REPO=https://github.com/thomkr01/other attest bot
r=$(update "$(sha b)")
case $r in failed*) ;; *) fail "record from another repository: $r" ;; esac
case $(sql "select error from update_runs order by id desc limit 1") in
  *"signed build record"*) ;; *) fail "record from another repository not explained" ;;
esac
[ "$(label)" = "$(sha a)" ] || fail "dashboard changed by a record from another repository"
echo "ok: runs without the approved build refused"

# A dashboard build from before the rename to Brim (only the old label): refused, nothing changed.
attest bot
LABEL=org.tommybot.update push dashboard "$(sha b)" "$serve"
r=$(update "$(sha b)")
case $r in failed*) ;; *) fail "dashboard with only the old label: $r" ;; esac
[ "$(label)" = "$(sha a)" ] || fail "dashboard changed by a build with only the old label"
echo "ok: dashboard build from before the rename refused"

# A build of main from before releases (no release version label) is refused, nothing changed.
attest bot
VER=none push dashboard "$(sha b)" "$serve"
r=$(update "$(sha b)")
case $r in failed*) ;; *) fail "build that is not a release: $r" ;; esac
case $(sql "select error from update_runs order by id desc limit 1") in
  *"not a release"*) ;; *) fail "build that is not a release not explained" ;;
esac
[ "$(label)" = "$(sha a)" ] || fail "dashboard changed by a build that is not a release"
echo "ok: build that is not a release refused"

# The good update: records name Brim, the dashboard carries only the new label.
push dashboard "$(sha b)" "$serve"
attest bot
r=$(update "$(sha b)")
[ "$r" = "done|$(sha b)|5" ] || fail "good update: $r"
[ "$(label)" = "$(sha b)" ] || fail "dashboard not on the new image"
[ "$(label mcp)" = "$(sha b)" ] || fail "mcp not on the new image"
ls "$dir"/b/pre-update-*.sql.gz > /dev/null 2>&1 || fail "no database dump before the good update"
echo "ok: good update"

# A dashboard from before approved builds could never update again: refused.
push bot "$(sha e)" 'exec sleep 3600'
push mcp "$(sha e)" 'exec sleep 3600'
push dashboard "$(sha e)" "$serve" none
r=$(update "$(sha e)")
case $r in failed*) ;; *) fail "build without approved-build support: $r" ;; esac
[ "$(label)" = "$(sha b)" ] || fail "dashboard changed by a refused old build"
echo "ok: old dashboard build refused"

# A stopped MCP server stays stopped.
dc stop mcp > /dev/null 2>&1
push bot "$(sha f)" 'exec sleep 3600'
push mcp "$(sha f)" 'exec sleep 3600'
push dashboard "$(sha f)" "$serve"
r=$(update "$(sha f)")
[ "$r" = "done|$(sha f)|5" ] || fail "update with mcp stopped: $r"
[ -z "$(dc ps -q mcp)" ] || fail "a stopped mcp was started"
echo "ok: stopped mcp left alone"

push bot "$(sha c)" 'exec sleep 3600'
push mcp "$(sha c)" 'exec sleep 3600'
push dashboard "$(sha c)" 'exit 1'
r=$(update "$(sha c)")
case $r in rolled_back*) ;; *) fail "bad update: $r" ;; esac
[ "$(label)" = "$(sha f)" ] || fail "dashboard not rolled back"
[ "$(docker inspect -f '{{.State.Running}}' "$(dc ps -q dashboard)")" = true ] || fail "dashboard down after rollback"
echo "ok: crashing update rolled back"

push bot "$(sha d)" 'exec sleep 3600'
push mcp "$(sha d)" 'exec sleep 3600'
push dashboard "$(sha d)" 'touch /m/new; exit 1'
r=$(update "$(sha d)")
case $r in failed*) ;; *) fail "update that breaks the old version: $r" ;; esac
case $(sql "select error from update_runs order by id desc limit 1") in
  *"does not start either"*) ;; *) fail "failed rollback not explained" ;;
esac
echo "ok: rollback that does not start reported as failed"

sql "insert into update_runs (requested_by) values (1), (1)" 2> /dev/null && fail "two runs queued at once"
echo PASS
