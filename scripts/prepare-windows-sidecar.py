import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
BINARY = "ternilo-sandbox-windows"


def windows_target(value):
    if not re.fullmatch(r"[A-Za-z0-9_]+-pc-windows-(msvc|gnu|gnullvm)", value):
        raise ValueError("A Windows target triple is required, for example x86_64-pc-windows-msvc")
    return value


def host_target():
    result = subprocess.run([os.environ.get("RUSTC", "rustc"), "-vV"], check=True, capture_output=True, text=True)
    for line in result.stdout.splitlines():
        if line.startswith("host: "):
            return windows_target(line.removeprefix("host: ").strip())
    raise ValueError("rustc did not report its host target; specify --target")


def prepare(target, output):
    target = windows_target(target)
    result = subprocess.run([
        "cargo", "build", "--locked", "--release", "--target", target,
        "-p", BINARY, "--bin", BINARY, "--message-format=json-render-diagnostics",
    ], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    executable = None
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == BINARY and message.get("executable"):
            executable = Path(message["executable"])
    if executable is None or executable.suffix.lower() != ".exe" or not executable.is_file():
        raise ValueError("Cargo did not produce the Windows sandbox executable; the existing sidecar is unchanged")
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"{BINARY}-{target}.exe"
    shutil.copy2(executable, destination)
    return destination


def main():
    parser = argparse.ArgumentParser(description="Build and stage the release sandbox sidecar before a Windows Tauri build")
    parser.add_argument("--target", default=os.environ.get("CARGO_BUILD_TARGET"), help="Windows Rust target; defaults to CARGO_BUILD_TARGET or the rustc host")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "apps/ternilo-desktop/binaries", help="Tauri external binary directory")
    args = parser.parse_args()
    try:
        destination = prepare(args.target or host_target(), args.output_dir)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Cannot prepare Windows sidecar: {error}", file=sys.stderr)
        return 1
    print(f"Prepared {destination}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
