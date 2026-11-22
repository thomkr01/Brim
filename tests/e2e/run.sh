#!/usr/bin/env bash
# One-command stack check for a cloud/CI box with no Discord credentials: local Postgres, workspace
# tests against it, dashboard + mock Discord, headless-browser sweep of every page.
# Needs: postgresql-16, cargo, trunk (+ wasm32 target), node with playwright. Run from the repo root.
set -euo pipefail
cd "$(dirname "$0")/../.."
# This box's own Postgres when DATABASE_URL is unset. A set one must name a scratch database: the
# tests and the e2e suite delete rows (same rule as `db::test_url` in the Rust tests).
if [ -z "${DATABASE_URL:-}" ]; then
  export DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres BRIM_TEST_DB=1
fi
db_name=${DATABASE_URL%%\?*}; db_name=${db_name##*/}
if [[ $db_name != *_test && ${BRIM_TEST_DB:-} != 1 ]]; then
  echo "DATABASE_URL names the database '$db_name': run only on a database whose name ends in _test, or set BRIM_TEST_DB=1" >&2
  exit 1
fi
export DISCORD_API_BASE=http://127.0.0.1:9100 DASHBOARD_URL=http://127.0.0.1:8080
export DISCORD_CLIENT_ID=1 DISCORD_CLIENT_SECRET=x DISCORD_TOKEN=mock BIND=127.0.0.1:8080
export DASHBOARD_DIST=crates/dashboard/ui/dist

# Skipped when Postgres is already reachable (CI service container).
if ! pg_isready -q -h localhost; then
  pg_lsclusters | grep -q online || { pg_ctlcluster 16 main start; sleep 3; }
  su postgres -c "psql -qc \"alter user postgres password 'postgres'\""
fi

[ -n "${SKIP_UNIT:-}" ] || cargo test --workspace
[ -f $DASHBOARD_DIST/index.html ] || (cd crates/dashboard/ui && trunk build --release)

# The permission matrix runs at the same time as dashboard.js, on its own database, dashboard and
# mock (each counts what its mock was sent), so neither sees the other's rows. DEEP=1 runs all of
# deep.js after dashboard.js on one database, as before.
matrix_db=${DATABASE_URL%/*}/matrix_$db_name
python3 tests/e2e/mock_discord.py & mock=$!
python3 tests/e2e/mock_discord.py 9101 & mock2=$!
cargo build -q --release -p brim-dashboard
target/release/brim-dashboard & dash=$!
dash2=
trap 'kill $mock $mock2 $dash $dash2 2>/dev/null' EXIT
if [ -z "${DEEP:-}" ]; then
  psql "$DATABASE_URL" -q -c "drop database if exists matrix_$db_name" -c "create database matrix_$db_name"
  DATABASE_URL=$matrix_db DISCORD_API_BASE=http://127.0.0.1:9101 DASHBOARD_URL=http://127.0.0.1:8081 BIND=127.0.0.1:8081 \
    target/release/brim-dashboard & dash2=$!
fi
up() { for _ in $(seq 600); do curl -s "$1/api/me" -o /dev/null -w "%{http_code}" | grep -q 401 && return; sleep 1; done; }
up $DASHBOARD_URL
[ -n "$dash2" ] && up http://127.0.0.1:8081

# dashboard.js counts rows of these tables, so a second run on the same database starts clean.
psql "$DATABASE_URL" -q -c "delete from message_sends; delete from message_rules; delete from sticky_msgs; delete from scheduled_jobs where kind = 'case_log'"
if [ -n "${DEEP:-}" ]; then
  # The mock's owner is the guild owner, so the dashboard grants owner level without any seed data.
  node tests/e2e/dashboard.js
  # Every page as every level and every control (~1 h).
  psql "$DATABASE_URL" -q -f tests/e2e/levels.sql
  node tests/e2e/deep.js
  exit
fi
# Every write route for every access level always runs (the fast part of deep.js).
psql "$matrix_db" -q -f tests/e2e/levels.sql
DATABASE_URL=$matrix_db DISCORD_API_BASE=http://127.0.0.1:9101 DASHBOARD_URL=http://127.0.0.1:8081 E2E_OUT=${E2E_OUT:-/tmp/e2e}/matrix \
  DEEP_ONLY=matrix,proofs node tests/e2e/deep.js > /tmp/e2e-matrix.log 2>&1 & matrix=$!
sweep=0; node tests/e2e/dashboard.js || sweep=$?
deep=0; wait $matrix || deep=$?
cat /tmp/e2e-matrix.log
echo "dashboard.js: exit $sweep. Matrix and proofs: exit $deep."
[ "$sweep" -eq 0 ] && [ "$deep" -eq 0 ]
