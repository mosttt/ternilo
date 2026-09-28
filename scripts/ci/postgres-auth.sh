#!/usr/bin/env bash
set -euo pipefail

container=$(docker run -d --rm \
  -e POSTGRES_PASSWORD=temporary-auth-check \
  -e POSTGRES_DB=ternilo_control_test \
  -p 127.0.0.1::5432 "${TERNILO_E2E_POSTGRES_IMAGE:-postgres:17}")
trap 'docker stop "$container" >/dev/null' EXIT
port=$(docker port "$container" 5432/tcp | cut -d: -f2)
for attempt in $(seq 1 60); do
  if docker exec "$container" pg_isready -U postgres >/dev/null 2>&1; then break; fi
  if [ "$attempt" -eq 60 ]; then echo 'Disposable PostgreSQL did not become ready' >&2; exit 1; fi
  sleep 1
done
export TERNILO_TEST_DATABASE_URL="postgres://postgres:temporary-auth-check@127.0.0.1:$port/ternilo_control_test"
cargo test --locked -p ternilo-control --test authentication_settings --test oidc_sessions --test edge_workbench_postgres -- --ignored --test-threads=1
