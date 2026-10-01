"""Exercise the packaged desktop service without a controlling console or WebView."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request


def request(connection, resource, body=None):
    payload = None if body is None else json.dumps(body).encode()
    call = urllib.request.Request(
        f"http://{connection['info']['address']}/api/v1{resource}",
        data=payload,
        headers={
            "Authorization": f"Bearer {connection['api_token']}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(call, timeout=10) as response:
        data = response.read()
        return json.loads(data) if data else None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--desktop", required=True, type=Path)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    executable = args.desktop.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="ternilo desktop service ") as directory:
        root = Path(directory)
        state = root / "state"
        workspace = root / "workspace"
        workspace.mkdir()
        expected_session = None
        environment = {key: value for key, value in os.environ.items() if not key.startswith("TERNILO_")}
        for attempt in range(2):
            with (root / f"service-{attempt}.log").open("w+b") as log:
                process = subprocess.Popen(
                    [str(executable), "--service", "--data-dir", str(state)]
                    + (["--listen", "127.0.0.1:0"] if attempt == 0 else []),
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, env=environment,
                    creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
                )
                try:
                    deadline = time.monotonic() + 60
                    connection = None
                    while time.monotonic() < deadline:
                        if process.poll() is not None:
                            log.seek(0)
                            raise AssertionError(f"Desktop service exited: {log.read().decode(errors='replace')}")
                        discovery = state / "runtime" / "service.json"
                        if discovery.is_file():
                            connection = json.loads(discovery.read_text())
                            break
                        time.sleep(0.1)
                    assert connection is not None, "Desktop service did not register"
                    assert json.loads((state / "config.json").read_text())["listen"] == "127.0.0.1:0"
                    assert request(connection, "/service")["version"] == args.expected_version
                    if attempt == 0:
                        saved = request(connection, "/workspaces", {"path": str(workspace)})
                        session = request(connection, "/sessions", {"workspace_id": saved["workspace_id"]})
                        expected_session = session["identity"]["session_id"]
                    else:
                        snapshot = request(connection, "/state")
                        assert any(session["identity"]["session_id"] == expected_session for session in snapshot["sessions"])
                    request(connection, "/service/stop", {})
                    assert process.wait(timeout=30) == 0, "Desktop service did not shut down cleanly"
                    assert not (state / "runtime" / "service.json").exists(), "Service discovery survived shutdown"
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=10)
        print("Desktop background service starts without a console, persists state, restarts and stops cleanly.")


if __name__ == "__main__":
    main()
