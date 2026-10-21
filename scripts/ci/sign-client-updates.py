"""Publish an Ed25519-signed manifest for the exact tested client archives."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def manifest(directory, version):
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError("Remote updates require a stable version")
    policy = json.loads((ROOT / ".github/client-update-policy.json").read_text())
    targets = json.loads((ROOT / ".github/package-targets.json").read_text())
    artifacts = []
    for entry in targets:
        target = entry["target"]
        extension = "zip" if target.endswith("windows-msvc") else "tar.gz"
        filename = f"ternilo-{version}-{target}.{extension}"
        artifact = directory / filename
        with artifact.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        artifacts.append({"target": target, "filename": filename,
                          "bytes": artifact.stat().st_size, "sha256": digest})
    return {**policy, "version": version, "artifacts": artifacts}


def sign(directory, version, key, public_key):
    encoded = (json.dumps(manifest(directory, version), ensure_ascii=True, separators=(",", ":")) + "\n").encode()
    with tempfile.TemporaryDirectory(prefix="ternilo-release-signing-") as temporary:
        root = Path(temporary)
        private = root / "signing.pem"
        with private.open("xb") as stream:
            os.chmod(private, 0o600)
            stream.write(key)
        public = subprocess.run(["openssl", "pkey", "-in", str(private), "-pubout", "-outform", "DER"],
                                capture_output=True, check=True).stdout
        if len(public) != 44 or public[:12] != bytes.fromhex("302a300506032b6570032100") or public[12:] != public_key:
            raise ValueError("Signing secret does not match the built-in Ed25519 public key")
        document = root / "manifest.json"
        document.write_bytes(encoded)
        signature = subprocess.run(["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", str(private), "-in", str(document)],
                                   capture_output=True, check=True).stdout
        if len(signature) != 64:
            raise ValueError("Unexpected Ed25519 signature size")
    (directory / "client-updates.json").write_bytes(encoded)
    (directory / "client-updates.json.sig").write_text(base64.b64encode(signature).decode() + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    key = os.environ.get("TERNILO_UPDATE_SIGNING_KEY", "").encode()
    if not key:
        raise ValueError("TERNILO_UPDATE_SIGNING_KEY must be set by the protected signing environment")
    public = base64.b64decode((ROOT / "crates/ternilo-update/release-public-key.txt").read_text().strip(), validate=True)
    sign(args.artifacts, args.version, key, public)
    print(f"Signed client update manifest for {args.version}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        # The key is never included in commands, stdout, diagnostics or release artifacts.
        raise SystemExit(f"Client update signing failed: {type(error).__name__}") from None
