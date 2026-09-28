#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
linorun_root=${TERNILO_LINORUN_SOURCE:-}
server_image=${TERNILO_RELEASE_SERVER_IMAGE:-}
worker_image=${TERNILO_RELEASE_WORKER_IMAGE:-}
test_image=${TERNILO_ACCEPTANCE_TEST_IMAGE:-}
artifact_dir=${TERNILO_RELEASE_ARTIFACT_DIR:-}

usage() {
    printf '%s\n' \
        'usage: release-gate.sh [OPTIONS]' \
        '' \
        'Options override the same fixed environment variables:' \
        '  --linorun-source PATH  TERNILO_LINORUN_SOURCE' \
        '  --server-image TAG     TERNILO_RELEASE_SERVER_IMAGE' \
        '  --worker-image TAG     TERNILO_RELEASE_WORKER_IMAGE' \
        '  --test-image TAG       TERNILO_ACCEPTANCE_TEST_IMAGE (acceptance tools only)' \
        '  --artifact-dir PATH    TERNILO_RELEASE_ARTIFACT_DIR'
}

while [ "$#" -gt 0 ]
do
    case "$1" in
        --linorun-source)
            linorun_root=${2:?--linorun-source requires a value}
            shift 2
            ;;
        --server-image)
            server_image=${2:?--server-image requires a value}
            shift 2
            ;;
        --worker-image)
            worker_image=${2:?--worker-image requires a value}
            shift 2
            ;;
        --test-image)
            test_image=${2:?--test-image requires a value}
            shift 2
            ;;
        --artifact-dir)
            artifact_dir=${2:?--artifact-dir requires a value}
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        --*)
            echo "unknown release gate option: $1" >&2
            exit 2
            ;;
        *)
            echo "unexpected release gate argument: $1" >&2
            exit 2
            ;;
    esac
done

linorun_root=${linorun_root:-$repo_root/../linorun}
server_image=${server_image:-ternilo-server:release-candidate}
worker_image=${worker_image:-ternilo-worker:release-candidate}
test_image=${test_image:-ternilo-acceptance:release-candidate}
artifact_dir=${artifact_dir:-$repo_root/release-artifacts}
if [ "$server_image" = "$worker_image" ] || [ "$test_image" = "$server_image" ] || [ "$test_image" = "$worker_image" ]; then
    echo 'Server, Worker and acceptance tools require different image tags.' >&2
    exit 2
fi

for command in cargo docker jq node npm python3 sha256sum; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "required release dependency is missing: $command" >&2
        exit 1
    }
done
test -f "$linorun_root/Cargo.toml" || {
    echo "Linorun source is missing: $linorun_root" >&2
    exit 1
}
mkdir -p "$artifact_dir"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
log_dir="$artifact_dir/$stamp"
mkdir -p "$log_dir"
passed_gates="$log_dir/gates.txt"
: > "$passed_gates"

run() {
    name=$1
    shift
    echo "==> $name"
    if "$@" > "$log_dir/$name.log" 2>&1; then
        printf '%s\n' "$name" >> "$passed_gates"
        echo "PASS $name"
    else
        cat "$log_dir/$name.log" >&2
        echo "FAIL $name" >&2
        exit 1
    fi
}

cd "$repo_root"
run rust-fmt cargo fmt --all -- --check
run rust-test cargo test --locked --workspace --all-targets --all-features
run rust-clippy cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
run web-clean-install npm --prefix web ci
run formal-docs npm --prefix web run verify:docs
run web-test npm --prefix web test
run web-browser-host env TERNILO_SERVER_BROWSER_IMAGE= npm --prefix web run test:web-browser-all
run deployment-tools python3 "$script_dir/tests/deployment-tools.test.py"
run release-gate-contract python3 "$script_dir/tests/release-gate.test.py"
run server-compose env TERNILO_IMAGE="$server_image" docker compose \
    --env-file "$script_dir/.env.server.example" -f "$script_dir/compose.server.yml" config --quiet
run worker-compose env TERNILO_IMAGE="$worker_image" docker compose \
    --env-file "$script_dir/.env.worker.example" -f "$script_dir/compose.worker.yml" config --quiet
run server-image docker build --target server \
    --build-context "linorun=$linorun_root" \
    -f "$script_dir/Dockerfile" -t "$server_image" "$repo_root"
run worker-image docker build --target worker \
    --build-context "linorun=$linorun_root" \
    -f "$script_dir/Dockerfile" -t "$worker_image" "$repo_root"
run acceptance-tools-image docker build --target acceptance \
    --build-context "linorun=$linorun_root" \
    -f "$script_dir/Dockerfile" -t "$test_image" "$repo_root"
run server-browser-container env TERNILO_SERVER_BROWSER_IMAGE="$server_image" \
    node --test web/tests/relay-browser-e2e.test.mjs
run cloud-browser-container env TERNILO_CLOUD_E2E_CONTAINER=1 \
    TERNILO_CLOUD_E2E_SERVER_IMAGE="$server_image" TERNILO_CLOUD_E2E_WORKER_IMAGE="$worker_image" \
    node --test web/tests/cloud-browser-e2e.test.mjs
run cloud-fresh-restore env TERNILO_ACCEPTANCE_SERVER_IMAGE="$server_image" \
    TERNILO_ACCEPTANCE_TEST_IMAGE="$test_image" \
    TERNILO_ACCEPTANCE_WORKER_IMAGE="$worker_image" TERNILO_ACCEPTANCE_DATABASE=all \
    TERNILO_ACCEPTANCE_SKIP_BUILD=1 sh "$script_dir/tests/cloud-backup-restore.acceptance.sh"
run cloud-credential-rotation-sqlite env TERNILO_ACCEPTANCE_SERVER_IMAGE="$server_image" \
    TERNILO_ACCEPTANCE_TEST_IMAGE="$test_image" \
    TERNILO_ACCEPTANCE_WORKER_IMAGE="$worker_image" TERNILO_ROTATION_DATABASE=sqlite \
    TERNILO_ACCEPTANCE_SKIP_BUILD=1 sh "$script_dir/tests/cloud-credential-rotation.acceptance.sh"
run cloud-credential-rotation-postgres env TERNILO_ACCEPTANCE_SERVER_IMAGE="$server_image" \
    TERNILO_ACCEPTANCE_TEST_IMAGE="$test_image" \
    TERNILO_ACCEPTANCE_WORKER_IMAGE="$worker_image" TERNILO_ROTATION_DATABASE=postgres \
    TERNILO_ACCEPTANCE_SKIP_BUILD=1 sh "$script_dir/tests/cloud-credential-rotation.acceptance.sh"

server_image_id=$(docker image inspect "$server_image" --format '{{.Id}}')
worker_image_id=$(docker image inspect "$worker_image" --format '{{.Id}}')
jq -n \
    --arg completed_at "$stamp" \
    --arg server_image "$server_image" \
    --arg server_image_id "$server_image_id" \
    --arg worker_image "$worker_image" \
    --arg worker_image_id "$worker_image_id" \
    --arg rust "$(rustc --version)" \
    --arg node "$(node --version)" \
    --rawfile gates "$passed_gates" \
    '{completed_at: $completed_at,
      images: {server: {image: $server_image, image_id: $server_image_id},
               worker: {image: $worker_image, image_id: $worker_image_id}},
      rust: $rust, node: $node,
      gates: ($gates | split("\n") | map(select(length > 0)))}' > "$log_dir/release.json"
(
    cd "$log_dir"
    sha256sum ./*.log gates.txt release.json > SHA256SUMS
)
echo "Release gate PASS: $log_dir/release.json"
