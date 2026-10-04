"""Point latest at the exact multi-platform image of the published stable release."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def run(*arguments):
    return subprocess.check_output(arguments, text=True)


def main():
    repository = os.environ["GH_REPO"]
    image = f"ghcr.io/{repository.split('/')[0].lower()}/ternilo-server"
    release = json.loads(run("gh", "api", f"repos/{repository}/releases/latest"))
    tag = release["tag_name"]
    if release["draft"] or release["prerelease"] or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError("latest requires a published stable release")
    with tempfile.TemporaryDirectory() as temporary:
        run("gh", "release", "download", tag, "--dir", temporary, "--pattern", "server-image.txt", "--pattern", "server-image-index.json", "--pattern", "SHA256SUMS")
        directory = Path(temporary)
        checksums = dict(line.split(maxsplit=1)[::-1] for line in (directory / "SHA256SUMS").read_text().splitlines())
        checksums = {name.strip(): digest for name, digest in checksums.items()}
        for name in ("server-image.txt", "server-image-index.json"):
            assert hashlib.sha256((directory / name).read_bytes()).hexdigest() == checksums[name], name
        metadata = (directory / "server-image.txt").read_text()
        manifest = json.loads(metadata[metadata.index("{"):])
        digest = manifest["digest"]
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
            raise ValueError("Invalid released image digest")
        released = json.loads((directory / "server-image-index.json").read_text())
        platforms = {(entry["platform"]["os"], entry["platform"]["architecture"]) for entry in released["manifests"]}
        assert platforms == {("linux", "amd64"), ("linux", "arm64")}, platforms
        reference = f"{image}@{digest}"
        actual = json.loads(run("docker", "buildx", "imagetools", "inspect", reference, "--raw"))
        assert actual == released, "Registry image differs from the published release"
        version = json.loads(run("docker", "buildx", "imagetools", "inspect", f"{image}:{tag[1:]}", "--format", "{{json .Manifest}}"))
        assert version["digest"] == digest, "Version tag differs from the published release"
        current = json.loads(run("gh", "api", f"repos/{repository}/releases/latest"))
        if current["tag_name"] != tag:
            raise ValueError("A newer release was published during promotion; rerun this workflow")
        run("docker", "buildx", "imagetools", "create", "--tag", f"{image}:latest", reference)
        promoted = json.loads(run("docker", "buildx", "imagetools", "inspect", f"{image}:latest", "--format", "{{json .Manifest}}"))
        assert promoted["digest"] == digest, "latest must preserve the released index digest"
        print(f"{image}:latest -> {tag} ({digest}; linux/amd64, linux/arm64)")


if __name__ == "__main__":
    main()
