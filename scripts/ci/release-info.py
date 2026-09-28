import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys

ROOT = Path(__file__).resolve().parents[2]


def version():
    source = (ROOT / "Cargo.toml").read_text()
    section = source.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    cargo = re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE).group(1)
    desktop = json.loads((ROOT / "apps/ternilo-desktop/tauri.conf.json").read_text())["version"]
    if cargo != desktop or not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", cargo):
        raise ValueError("Cargo and Tauri must have the same semantic version")
    return cargo


def validate_ref(release_version, ref, publish):
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{release_version}":
        raise ValueError("Release tag must exactly match the Cargo and Tauri version")
    if publish and ref != f"refs/tags/v{release_version}":
        raise ValueError("Publishing requires a matching version tag; branch runs only build artifacts")


def collect(source, output, target):
    output.mkdir(parents=True, exist_ok=True)
    artifacts = []
    for artifact in sorted(source.rglob("*")):
        if artifact.is_file() and artifact.suffix.lower() in {".deb", ".appimage", ".dmg", ".msi", ".exe"}:
            destination = output / f"{target}-{artifact.name}"
            if destination.exists():
                raise ValueError(f"Duplicate installer: {destination.name}")
            shutil.copy2(artifact, destination)
            artifacts.append(destination)
    if not artifacts:
        raise ValueError("Tauri did not produce any installers")


def checksums(directory):
    entries = []
    for artifact in sorted(directory.iterdir()):
        if artifact.is_file() and artifact.name != "SHA256SUMS":
            with artifact.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            entries.append(f"{digest}  {artifact.name}\n")
    if not entries:
        raise ValueError("No release files were produced")
    (directory / "SHA256SUMS").write_text("".join(entries))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--publish", action="store_true")
    parser.add_argument("--ref", default=os.environ.get("GITHUB_REF", ""))
    parser.add_argument("--collect", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--target")
    parser.add_argument("--checksums", type=Path)
    args = parser.parse_args()
    if args.checksums:
        checksums(args.checksums)
        return
    if args.collect:
        if not args.output or not args.target:
            parser.error("--collect requires --output and --target")
        collect(args.collect, args.output, args.target)
        return
    release_version = version()
    validate_ref(release_version, args.ref, args.publish)
    repository = os.environ.get("GITHUB_REPOSITORY", "local/ternilo").lower()
    values = {"version": release_version, "image": f"ghcr.io/{repository.split('/')[0]}/ternilo-server", "prerelease": str("-" in release_version).lower()}
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            for key, value in values.items():
                output.write(f"{key}={value}\n")
    print(json.dumps(values))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"Release metadata error: {error}", file=sys.stderr)
        sys.exit(1)
