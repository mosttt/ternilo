#!/bin/sh
set -eu
tests_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$tests_dir/cloud-credential-rotation.acceptance.py" "$@"
