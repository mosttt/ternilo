#!/usr/bin/env bash
set -euo pipefail

container=$(docker run -d --rm \
  -e POSTGRES_PASSWORD=temporary-auth-check \
  -e POSTGRES_DB=ternilo_control_test \
  -p 127.0.0.1::5432 "${TERNILO_E2E_POSTGRES_IMAGE:-postgres:17}")
trap 'docker stop "$container" >/dev/null' EXIT
port=$(docker port "$container" 5432/tcp | cut -d: -f2)
for attempt in $(seq 1 60); do
  if docker exec "$container" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; then break; fi
  if [ "$attempt" -eq 60 ]; then echo 'Disposable PostgreSQL did not become ready' >&2; exit 1; fi
  sleep 1
done
export TERNILO_TEST_DATABASE_URL="postgres://postgres:temporary-auth-check@127.0.0.1:$port/ternilo_control_test"
cargo test --locked -p ternilo-control --test authentication_settings --test identity_sessions --test oidc_sessions --test edge_workbench_postgres --test native_recovery -- --ignored --test-threads=1

cargo test --locked -p ternilo-control --lib model_store:: -- --ignored --test-threads=1

cargo test --locked -p ternilo-control --lib project_sharing::tests -- --ignored --test-threads=1

cargo test --locked -p ternilo-server --bin ternilo-server postgres_project_files_search -- --ignored --test-threads=1

cargo test --locked -p ternilo-server --bin ternilo-server postgres_account_delivery -- --ignored --test-threads=1

docker exec "$container" createdb -U postgres ternilo_cloud_test
export TERNILO_CLOUD_TEST_DATABASE_URL="postgres://postgres:temporary-auth-check@127.0.0.1:$port/ternilo_cloud_test"
cargo test --locked -p ternilo-cloud --test account_cleanup --test batch_authors -- --ignored --test-threads=1
