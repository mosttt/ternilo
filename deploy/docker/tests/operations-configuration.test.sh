#!/bin/sh
set -eu

tests_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
docker_dir=$(CDPATH= cd -- "$tests_dir/.." && pwd)
repo_root=$(CDPATH= cd -- "$docker_dir/../.." && pwd)
temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/ternilo-operations-configuration.XXXXXX")
trap 'rm -rf -- "$temporary_root"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

expect_status() {
    expected=$1
    check_label=$2
    shift 2
    set +e
    "$@" > "$temporary_root/$check_label.stdout" 2> "$temporary_root/$check_label.stderr"
    status=$?
    set -e
    if [ "$status" -ne "$expected" ]; then
        echo "$check_label returned $status instead of $expected" >&2
        cat "$temporary_root/$check_label.stderr" >&2
        exit 1
    fi
}

for script in "$docker_dir/ternilo-deploy" "$docker_dir/cloud-rotate-credentials.sh" \
    "$docker_dir/release-gate.sh" "$repo_root/scripts/package-release.sh"
do
    name=$(basename -- "$script")
    expect_status 0 "$name-help" "$script" --help
    grep -q 'TERNILO_' "$temporary_root/$name-help.stdout"
    expect_status 2 "$name-unknown" "$script" --not-a-real-option
    expect_status 2 "$name-positional" "$script" unexpected-positional
done
grep -q 'verify' "$temporary_root/ternilo-deploy-help.stdout"
grep -q 'TERNILO_PG_RESTORE' "$temporary_root/ternilo-deploy-help.stdout"

env_deploy="$temporary_root/environment-server"
cli_deploy="$temporary_root/cli-server"
for directory in "$env_deploy" "$cli_deploy"
do
    mkdir "$directory"
    printf 'server\n' > "$directory/.ternilo-deployment"
    cp "$docker_dir/compose.server.yml" "$directory/compose.server.yml"
    cp "$docker_dir/.env.server.example" "$directory/.env"
done

expect_status 0 deploy-config-cli env TERNILO_DEPLOY_DIR="$env_deploy" \
    TERNILO_IMAGE=ternilo:latest TERNILO_SERVER_PUBLIC_URL=invalid \
    "$docker_dir/ternilo-deploy" check --offline --directory "$cli_deploy" \
    --image ternilo-server:cli-release --public-url https://cli.example.invalid
expect_status 0 deploy-config-env env TERNILO_DEPLOY_DIR="$env_deploy" \
    TERNILO_IMAGE=ternilo-server:environment-release TERNILO_SERVER_PUBLIC_URL=https://env.example.invalid \
    TERNILO_DEPLOY_OFFLINE=true "$docker_dir/ternilo-deploy" check

test_log="$temporary_root/docker.log"
env_docker="$temporary_root/environment-docker"
cli_docker="$temporary_root/cli-docker"
for executable in "$env_docker" "$cli_docker"
do
    cat > "$executable" <<'SH'
#!/bin/sh
printf '%s %s\n' "$0" "$*" >> "$TERNILO_OPERATIONS_TEST_LOG"
printf '[{"Service":"server","State":"running","Health":"healthy"}]\n'
SH
    chmod 700 "$executable"
done

expect_status 0 deploy-docker-cli env TERNILO_DOCKER="$env_docker" TERNILO_DEPLOY_DIR="$env_deploy" \
    TERNILO_IMAGE=ternilo-server:test-release TERNILO_OPERATIONS_TEST_LOG="$test_log" \
    "$docker_dir/ternilo-deploy" status --directory "$cli_deploy" --docker "$cli_docker"
grep -Fq "$cli_docker compose --env-file $cli_deploy/.env -f $cli_deploy/compose.server.yml" "$test_log"
if grep -Fq "$env_docker " "$test_log"; then
    echo 'Explicit Docker selection was overridden by its environment default' >&2
    exit 1
fi
: > "$test_log"
expect_status 0 deploy-docker-env env TERNILO_DOCKER="$env_docker" TERNILO_DEPLOY_DIR="$env_deploy" \
    TERNILO_IMAGE=ternilo-server:test-release TERNILO_OPERATIONS_TEST_LOG="$test_log" \
    "$docker_dir/ternilo-deploy" status
grep -Fq "$env_docker compose --env-file $env_deploy/.env -f $env_deploy/compose.server.yml" "$test_log"

expect_status 1 rotation-directory-cli env TERNILO_DEPLOY_DIR="$env_deploy" \
    "$docker_dir/cloud-rotate-credentials.sh" model-key-status --provider-id fixture \
    --directory "$temporary_root/cli-missing"
grep -Fq "$temporary_root/cli-missing/.ternilo-deployment" "$temporary_root/rotation-directory-cli.stderr"
expect_status 1 rotation-directory-env env TERNILO_DEPLOY_DIR="$temporary_root/env-missing" \
    "$docker_dir/cloud-rotate-credentials.sh" model-key-status --provider-id fixture
grep -Fq "$temporary_root/env-missing/.ternilo-deployment" "$temporary_root/rotation-directory-env.stderr"

env_linorun="$temporary_root/env-linorun"
cli_linorun="$temporary_root/cli-linorun"
expect_status 1 release-source-cli env TERNILO_LINORUN_SOURCE="$env_linorun" \
    "$docker_dir/release-gate.sh" --linorun-source "$cli_linorun"
grep -Fq "$cli_linorun" "$temporary_root/release-source-cli.stderr"
expect_status 1 release-source-env env TERNILO_LINORUN_SOURCE="$env_linorun" \
    "$docker_dir/release-gate.sh"
grep -Fq "$env_linorun" "$temporary_root/release-source-env.stderr"

printf '%s\n' 'Modern deployment operations CLI/environment checks passed'
