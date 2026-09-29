#!/bin/sh
set -eu

source_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=${TERNILO_RELEASE_VERSION:-}
component=${TERNILO_RELEASE_COMPONENT:-all}
output_dir=${TERNILO_RELEASE_OUTPUT_DIR:-$source_dir/dist}
bin_dir=${TERNILO_RELEASE_BIN_DIR:-$source_dir/target/release}
target_name=${TERNILO_RELEASE_TARGET:-$(uname -s)-$(uname -m)}
build=${TERNILO_RELEASE_BUILD:-true}
binary_suffix=${TERNILO_RELEASE_BINARY_SUFFIX:-}

usage() {
    printf '%s\n' \
        'Usage: package-release.sh --version VERSION [OPTIONS]' \
        'Creates a local binary + deployment archive; does not publish or upload.' \
        'CLI overrides the fixed environment variable shown below:' \
        '  --version VALUE      TERNILO_RELEASE_VERSION (required)' \
        '  --component VALUE    TERNILO_RELEASE_COMPONENT (local|server|worker|all; default: all)' \
        '  --output-dir PATH    TERNILO_RELEASE_OUTPUT_DIR (default: dist)' \
        '  --bin-dir PATH       TERNILO_RELEASE_BIN_DIR (default: target/release)' \
        '  --target-name VALUE  TERNILO_RELEASE_TARGET (archive label only)' \
        '  --binary-suffix .exe TERNILO_RELEASE_BINARY_SUFFIX (Windows executables and sandbox runner)' \
        '  --build / --no-build TERNILO_RELEASE_BUILD (default: true)'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) version=${2:?--version requires a value}; shift 2 ;;
        --component) component=${2:?--component requires a value}; shift 2 ;;
        --output-dir) output_dir=${2:?--output-dir requires a value}; shift 2 ;;
        --bin-dir) bin_dir=${2:?--bin-dir requires a value}; shift 2 ;;
        --target-name) target_name=${2:?--target-name requires a value}; shift 2 ;;
        --binary-suffix) binary_suffix=${2:?--binary-suffix requires a value}; shift 2 ;;
        --build) build=true; shift ;;
        --no-build) build=false; shift ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

case "$version" in ''|*[!A-Za-z0-9._-]*) echo 'A version containing only letters, digits, dots, underscores and hyphens is required.' >&2; exit 2 ;; esac
case "$target_name" in ''|*[!A-Za-z0-9._-]*) echo 'Invalid target name.' >&2; exit 2 ;; esac
case "$build" in true|false) ;; *) echo 'TERNILO_RELEASE_BUILD must be true or false.' >&2; exit 2 ;; esac
case "$binary_suffix" in ''|.exe) ;; *) echo 'Binary suffix must be empty or .exe.' >&2; exit 2 ;; esac

case "$component" in
    local) binary_names='ternilo ternilo-plugin'; set -- -p ternilo -p ternilo-plugin-cli ;;
    server) binary_names='ternilo-server'; set -- -p ternilo-server ;;
    worker) binary_names='ternilo-worker'; set -- -p ternilo-worker ;;
    all) binary_names='ternilo ternilo-server ternilo-worker ternilo-plugin'; set -- -p ternilo -p ternilo-server -p ternilo-worker -p ternilo-plugin-cli ;;
    *) echo 'Component must be local, server, worker, or all.' >&2; exit 2 ;;
esac

if [ "$binary_suffix" = .exe ]; then
    case "$component" in
        local|all) binary_names="$binary_names ternilo-sandbox-windows"; set -- "$@" -p ternilo-sandbox-windows ;;
    esac
    suffixed_names=''
    for binary in $binary_names; do suffixed_names="$suffixed_names $binary.exe"; done
    binary_names=${suffixed_names# }
fi

if [ "$build" = true ]; then
    cd "$source_dir"
    if [ "$component" != worker ]; then
        npm --prefix web ci
        npm --prefix web run build
    fi
    cargo build --locked --release "$@"
fi

for binary in $binary_names; do
    if [ ! -x "$bin_dir/$binary" ]; then
        echo "Required executable is missing: $bin_dir/$binary" >&2
        exit 1
    fi
done

mkdir -p "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
prefix=ternilo
if [ "$component" != local ]; then
    prefix="ternilo-$component"
fi
extension=tar.gz
if [ "$binary_suffix" = .exe ]; then extension=zip; fi
name="$prefix-$version-$target_name"
archive="$output_dir/$name.$extension"
if [ -e "$archive" ] || [ -e "$archive.sha256" ]; then
    echo "Release output already exists: $archive" >&2
    exit 1
fi
staging=$(mktemp -d "${TMPDIR:-/tmp}/ternilo-package.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$staging/$name/bin" "$staging/$name/deploy/docker" "$staging/$name/docs"
for binary in $binary_names; do
    install -m 755 "$bin_dir/$binary" "$staging/$name/bin/$binary"
done

# Explicit deployment inputs exclude local secrets, backups and environment files.
for file in \
    compose.server.yml .env.server.example compose.worker.yml .env.worker.example \
    compose.server.build.yml compose.worker.build.yml \
    ternilo-deploy cloud-rotate-credentials.sh rotate-credentials.py \
    entrypoint.sh worker-policy.json; do
    cp "$source_dir/deploy/docker/$file" "$staging/$name/deploy/docker/$file"
done
# Include the small documented examples and SDK sources, never their local outputs.
for file in \
    examples/execution-envelope.json examples/searxng-profile.json examples/rust-analyzer-profile.json \
    examples/openai-compatible-profile.json examples/worker-policy.json examples/run-spec.json \
    examples/rhai-echo-extension/README.md examples/rhai-echo-extension/manifest.json \
    examples/rhai-echo-extension/extension.rhai examples/wasm-echo-plugin/README.md \
    examples/wasm-echo-plugin/manifest.json examples/wasm-echo-plugin/Cargo.toml \
    examples/wasm-echo-plugin/Cargo.lock examples/wasm-echo-plugin/src/lib.rs \
    crates/ternilo-extension/wit/plugin.wit \
    sdk/python/pyproject.toml sdk/python/src/ternilo/__init__.py sdk/python/src/ternilo/client.py \
    sdk/python/src/ternilo/server.py sdk/python/tests/test_smoke.py sdk/python/tests/test_server.py \
    sdk/python/tests/server_smoke.py sdk/typescript/package.json sdk/typescript/package-lock.json \
    sdk/typescript/tsconfig.json sdk/typescript/src/client.ts sdk/typescript/src/server.ts \
    sdk/typescript/test/smoke.ts sdk/typescript/test/server.test.ts sdk/typescript/test/server-smoke.ts; do
    mkdir -p "$staging/$name/$(dirname -- "$file")"
    cp "$source_dir/$file" "$staging/$name/$file"
done
cp -R "$source_dir/docs/." "$staging/$name/docs/"
rm -rf "$staging/$name/docs/development"
cp "$source_dir/README.md" "$source_dir/README.zh-CN.md" "$source_dir/LICENSE" "$source_dir/THIRD_PARTY_NOTICES.md" "$staging/$name/"
cp -R "$source_dir/licenses" "$staging/$name/"
sed 's|(../en/binaries.md)|(START-HERE.en.md)|g' "$source_dir/docs/zh-CN/binaries.md" > "$staging/$name/START-HERE.md"
sed 's|(../zh-CN/binaries.md)|(START-HERE.md)|g' "$source_dir/docs/en/binaries.md" > "$staging/$name/START-HERE.en.md"
printf '%s\n' "version=$version" "target=$target_name" "component=$component" "binaries=$binary_names" > "$staging/$name/RELEASE"
if [ "$extension" = zip ]; then
    python3 - "$staging" "$name" "$archive" <<'PYZIP'
from pathlib import Path
import sys
import zipfile
root = Path(sys.argv[1])
with zipfile.ZipFile(sys.argv[3], "x", compression=zipfile.ZIP_DEFLATED) as archive:
    for entry in sorted((root / sys.argv[2]).rglob("*")):
        archive.write(entry, entry.relative_to(root).as_posix())
PYZIP
else
    tar -C "$staging" -czf "$archive" "$name"
fi
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$output_dir" && sha256sum "$name.$extension") > "$archive.sha256"
else
    (cd "$output_dir" && shasum -a 256 "$name.$extension") > "$archive.sha256"
fi
printf 'Created %s\nChecksum %s\n' "$archive" "$archive.sha256"
printf 'Component %s; binaries: %s\n' "$component" "$binary_names"
printf '%s\n' 'Shared documentation may describe other components. Install those binaries separately; Server Docker deployment pulls the published image.'
