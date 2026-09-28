import argparse
import json
import os
import secrets
import subprocess
import time
import urllib.error
import urllib.request
import uuid


def docker(*arguments, environment=None):
    result = subprocess.run(["docker", *arguments], env=environment, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"Docker {arguments[0]} failed: {result.stderr[-2000:]}")
    return result.stdout.strip()


def request(origin, path, token=None, body=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    data = json.dumps(body).encode() if body is not None else None
    with urllib.request.urlopen(urllib.request.Request(origin + path, data=data, headers=headers), timeout=5) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def ready(origin):
    for _ in range(90):
        try:
            request(origin, "/readyz")
            return
        except (OSError, ValueError):
            time.sleep(1)
    raise RuntimeError("Server image did not become ready")


def main():
    parser = argparse.ArgumentParser(description="Validate a built Server image with isolated Docker resources")
    parser.add_argument("--image", required=True)
    args = parser.parse_args()
    name = f"ternilo-image-check-{uuid.uuid4().hex[:12]}"
    volume = name + "-data"
    password = secrets.token_urlsafe(24)
    environment = {**os.environ, "TERNILO_SERVER_OWNER_USERNAME": "image-owner", "TERNILO_SERVER_OWNER_EMAIL": "image-owner@example.test", "TERNILO_SERVER_OWNER_PASSWORD": password}
    isolation = ["--user", "0:0", "--read-only", "--tmpfs", "/tmp:size=64m,mode=1777", "--cap-drop=ALL", "--security-opt=no-new-privileges:true"]
    for capability in ["CHOWN", "DAC_READ_SEARCH", "KILL", "SETUID", "SETGID", "SETPCAP"]:
        isolation.extend(["--cap-add", capability])
    docker("volume", "create", volume)
    try:
        docker("run", "--rm", *isolation, "--mount", f"source={volume},target=/var/lib/ternilo",
               "--env", "TERNILO_SERVER_OWNER_USERNAME", "--env", "TERNILO_SERVER_OWNER_EMAIL", "--env", "TERNILO_SERVER_OWNER_PASSWORD",
               args.image, "server", "init", "--non-interactive", "--config", "/var/lib/ternilo/server.json", "--listen", "0.0.0.0:4321", environment=environment)
        docker("run", "-d", "--name", name, *isolation, "--mount", f"source={volume},target=/var/lib/ternilo", "-p", "127.0.0.1::4321", args.image)
        origin = "http://" + docker("port", name, "4321/tcp").splitlines()[0]
        ready(origin)
        login = request(origin, "/api/v1/auth/login", body={"username": "image-owner", "password": password})
        token = login["access_token"]
        identity = request(origin, "/api/v1/auth/session", token=token)
        assert identity["is_instance_owner"]
        assert request(origin, "/auth/config")["native_enabled"]
        assert request(origin, "/api/v1/admin/instance/authentication", token=token)["revision"] == 0
        with urllib.request.urlopen(origin, timeout=5) as page:
            assert b"/assets/app.js" in page.read()
        processes = docker("top", name, "-eo", "pid,uid,comm").splitlines()
        assert any(line.split()[1:] == ["10001", "ternilo-server"] for line in processes), "Server must run as UID 10001"
        docker("restart", name)
        origin = "http://" + docker("port", name, "4321/tcp").splitlines()[0]
        ready(origin)
        assert request(origin, "/api/v1/auth/session", token=token)["user"]["user_id"] == identity["user"]["user_id"]
        print("Server image passed: non-root process, read-only root, initialization, login, settings, embedded Web and persistent restart")
    finally:
        subprocess.run(["docker", "rm", "-f", name], capture_output=True)
        subprocess.run(["docker", "volume", "rm", volume], capture_output=True)


if __name__ == "__main__":
    main()
