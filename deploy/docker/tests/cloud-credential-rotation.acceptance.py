"""Exercise credential rotation against isolated Server, Worker and acceptance images."""

import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
from urllib.parse import quote
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[3]
HELPER = ROOT / "deploy/docker/ternilo-deploy"
ROTATE = ROOT / "deploy/docker/cloud-rotate-credentials.sh"
DOCKER = os.getenv("TERNILO_DOCKER", "docker")
SERVER_IMAGE = os.getenv("TERNILO_ACCEPTANCE_SERVER_IMAGE", "ternilo-server:release-candidate")
WORKER_IMAGE = os.getenv("TERNILO_ACCEPTANCE_WORKER_IMAGE", "ternilo-worker:release-candidate")
TEST_IMAGE = os.getenv("TERNILO_ACCEPTANCE_TEST_IMAGE", "ternilo-acceptance:release-candidate")
CLEAN_ENV = {key: value for key, value in os.environ.items() if not key.startswith("TERNILO_")}


def run(arguments, *, env=None, content=None, success=True):
    result = subprocess.run([str(value) for value in arguments], input=content, capture_output=True,
                            env=dict(CLEAN_ENV, **(env or {})))
    if success and result.returncode:
        raise RuntimeError(f"Command failed ({arguments[0]}): {result.stderr.decode(errors='replace')[-4000:]}")
    return result


def unused_port():
    with socket.socket() as listener:
        listener.bind(("0.0.0.0", 0))
        return listener.getsockname()[1]


def acceptance(backend):
    temporary = Path(tempfile.mkdtemp(prefix=f"ternilo-rotation-{backend}-"))
    os.chmod(temporary, 0o700)
    server_dir, worker_dir = temporary / "server", temporary / "worker"
    identifier = secrets.token_hex(6)
    postgres = f"ternilo-rotation-postgres-{identifier}"
    bridge = run([DOCKER, "network", "inspect", "bridge", "--format", "{{(index .IPAM.Config 0).Gateway}}"]).stdout.decode().strip()
    if not bridge:
        raise RuntimeError("Docker bridge has no host gateway for this isolated acceptance fixture")
    origin = f"http://{bridge}:{unused_port()}"
    owner_password = secrets.token_hex(24)
    old_model, wrong_model, new_model = (secrets.token_hex(24) for _ in range(3))
    old_migrator, old_runtime = (secrets.token_hex(24) for _ in range(2))
    # Nontrivial passwords verify that updating a DSN preserves correct URL encoding.
    new_migrator = secrets.token_hex(16) + ":@ /?#%"
    new_runtime = secrets.token_hex(16) + ":@ /?#%"
    expected_key_file = temporary / "expected-model-key"
    expected_key_file.write_text(old_model)
    os.chmod(expected_key_file, 0o600)
    model_log = (temporary / "model.stderr").open("wb")
    model = subprocess.Popen(["node", str(ROOT / "deploy/docker/tests/rotation-model-fixture.mjs"), str(expected_key_file)],
                             stdout=subprocess.PIPE, stderr=model_log,
                             env=dict(CLEAN_ENV, TERNILO_ACCEPTANCE_MODEL_BIND=bridge))
    model_port = int(model.stdout.readline().decode().strip())
    model_origin = f"http://{bridge}:{model_port}"
    policy_path = temporary / "worker-policy.json"
    policy = json.loads((ROOT / "deploy/docker/worker-policy.json").read_text())
    policy.update(policy_revision="credential-rotation-v2", minimum_workspace_free_bytes=0)
    policy_path.write_text(json.dumps(policy))
    os.chmod(policy_path, 0o600)
    token = None
    model_grant_id = None
    database_port = None
    all_secrets = [owner_password, old_model, wrong_model, new_model, old_migrator, old_runtime, new_migrator, new_runtime]

    def compose(directory, *arguments, content=None, success=True):
        component = (directory / ".ternilo-deployment").read_text().strip()
        return run([DOCKER, "compose", "--env-file", directory / ".env", "-f", directory / f"compose.{component}.yml", *arguments], content=content, success=success)

    def helper(directory, action, *arguments, env=None, success=True):
        return run([HELPER, action, "--directory", directory, *arguments], env=env, success=success)

    def api(endpoint, body=None, method=None):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        request = Request(origin + "/api/v1" + endpoint, headers=headers,
                          method=method or ("POST" if body is not None else "GET"),
                          data=json.dumps(body).encode() if body is not None else None)
        with urlopen(request, timeout=30) as response:
            return json.load(response) if response.status != 204 else None

    def server_read(name):
        return compose(server_dir, "run", "--rm", "--no-deps", "--entrypoint", "cat", "server", "/var/lib/ternilo/" + name).stdout

    def private_server_command(command):
        return compose(server_dir, "run", "--rm", "--no-deps", "--user", "10001:10001", "--entrypoint", "sh", "server", "-c", command)

    def server_volume():
        return json.loads(compose(server_dir, "config", "--format", "json").stdout)["volumes"]["server-data"]["name"]

    def contract_command(function, key=None):
        config = json.loads(server_read("server.json"))
        environment = {
            "TERNILO_ROTATION_RUNTIME_DATABASE_URL": config["database_url"],
            "TERNILO_ROTATION_MIGRATION_DATABASE_URL": config.get("migration_database_url") or config["database_url"],
            "TERNILO_ROTATION_SECRET_MASTER_KEY": key or config["secret_master_key"],
            "TERNILO_ROTATION_WORKER_POLICY": "/var/lib/ternilo/worker-policy.json",
        }
        if model_grant_id:
            environment.update(TERNILO_ROTATION_MODEL_GRANT_ID=model_grant_id, TERNILO_ROTATION_PUBLIC_MODEL_ID="rotation-model")
        command = [DOCKER, "run", "--rm", "--user", "10001:10001", "--volume", f"{server_volume()}:/var/lib/ternilo"]
        for name in environment:
            command += ["-e", name]
        command += [TEST_IMAGE, "credential-rotation-acceptance", function, "--ignored", "--exact", "--nocapture"]
        return command, environment

    def contract(function, key=None):
        command, environment = contract_command(function, key)
        run(command, env=environment)

    def resume_after_known_rollback():
        helper(server_dir, "up")
        helper(server_dir, "resume-execution", "--server-url", origin, env={"TERNILO_SERVER_ACCESS_TOKEN": token})
        helper(worker_dir, "up")

    def rotate(operation, environment=None, canary=False, success=True):
        command = [ROTATE, "--directory", server_dir, "--server-url", origin, operation]
        env = dict(environment or {}, TERNILO_SERVER_ACCESS_TOKEN=token, TERNILO_MODEL_PROVIDER_ID="rotation")
        if canary:
            canary_command, canary_environment = contract_command("rotated_stack_executes_authoritative_model_canary")
            command += ["--", *canary_command]
            env.update(canary_environment)
        return run(command, env=env, success=success)

    def postgres_sql(sql):
        return run([DOCKER, "exec", "-i", postgres, "psql", "--username", "ternilo_migrator", "--dbname", "ternilo", "--set=ON_ERROR_STOP=1"], content=sql.encode())

    def assert_login(role, password, expected):
        result = run([DOCKER, "run", "--rm", "-e", "PGPASSWORD", "postgres:17-bookworm", "psql",
                      "--host", bridge, "--port", str(database_port), "--username", role, "--dbname", "ternilo",
                      "--tuples-only", "--no-align", "--command", "SELECT 1"], env={"PGPASSWORD": password}, success=False)
        assert (result.returncode == 0) is expected, f"unexpected login result for {role}"

    try:
        for image in (SERVER_IMAGE, WORKER_IMAGE, TEST_IMAGE):
            run([DOCKER, "image", "inspect", image])
        environment = {"TERNILO_SERVER_OWNER_USERNAME": "owner", "TERNILO_SERVER_OWNER_EMAIL": "owner@example.test", "TERNILO_SERVER_OWNER_PASSWORD": owner_password,
                       "TERNILO_SERVER_HTTP_BIND_ADDRESS": bridge, "TERNILO_SERVER_HTTP_PORT": origin.rsplit(":", 1)[1]}
        if backend == "postgres":
            database_port = unused_port()
            password_file = temporary / "postgres-password"
            password_file.write_text(old_migrator)
            os.chmod(password_file, 0o600)
            run([DOCKER, "run", "-d", "--name", postgres, "-e", "POSTGRES_DB=ternilo", "-e", "POSTGRES_USER=ternilo_migrator",
                 "-e", "POSTGRES_PASSWORD_FILE=/run/secrets/database-password", "--volume", f"{password_file}:/run/secrets/database-password:ro",
                 "-p", f"{bridge}:{database_port}:5432", "postgres:17-bookworm"])
            for _ in range(120):
                if run([DOCKER, "exec", postgres, "pg_isready", "-h", "127.0.0.1", "-U", "ternilo_migrator", "-d", "ternilo"], success=False).returncode == 0:
                    break
                time.sleep(0.25)
            else:
                raise RuntimeError("PostgreSQL did not initialize")
            postgres_sql(f"CREATE ROLE ternilo_runtime NOLOGIN; CREATE ROLE ternilo_app LOGIN PASSWORD '{old_runtime}'; GRANT ternilo_runtime TO ternilo_app; REVOKE CREATE ON SCHEMA public FROM PUBLIC; GRANT USAGE ON SCHEMA public TO ternilo_runtime;")
            environment.update(TERNILO_DATABASE_URL=f"postgres://ternilo_app:{old_runtime}@{bridge}:{database_port}/ternilo",
                               TERNILO_MIGRATION_DATABASE_URL=f"postgres://ternilo_migrator:{old_migrator}@{bridge}:{database_port}/ternilo")
        helper(server_dir, "init", "--image", SERVER_IMAGE, "--public-url", origin,
               "--managed-execution-enabled", "--worker-policy", policy_path, env=environment)
        helper(server_dir, "up")
        login = api("/auth/login", {"username": "owner", "password": owner_password})
        token = login["access_token"]
        api("/admin/instance", {"mode": "multi_user", "revision": login["instance"]["revision"]}, "PATCH")
        grant = api("/admin/workers", {"worker_id": "rotation-worker-" + identifier})
        all_secrets += [token, grant["token"]]
        helper(worker_dir, "init", "--component", "worker", "--image", WORKER_IMAGE, "--server-url", origin,
               env={"TERNILO_WORKER_TOKEN": grant["token"]})
        helper(worker_dir, "up")
        original_worker = compose(worker_dir, "run", "--rm", "--no-deps", "--entrypoint", "cat", "worker", "/var/lib/ternilo/worker.json").stdout
        assert all(value not in original_worker for value in (b"database_url", b"secret_master_key", b"model_api_key"))
        contract("seed_rotation_fixture_secret")
        profile_path = temporary / "model-provider.json"
        profile_path.write_text(json.dumps({
            "id": "rotation", "display_name": "Rotation provider", "base_url": model_origin + "/v1",
            "protocol": "openai-chat-completions", "api_key_ref": None,
            "defaults": {"context_window": 128000, "max_output_tokens": 256},
            "models": [{"id": "rotation-model", "display_name": "Rotation model", "settings": {"mode": "inherit"}}],
            "timeout_ms": 5000, "max_attempts": 1, "retry_base_delay_ms": 10,
        }))
        os.chmod(profile_path, 0o600)
        helper(server_dir, "configure-model", "--server-url", origin, "--provider-profile", profile_path,
               env={"TERNILO_SERVER_ACCESS_TOKEN": token, "TERNILO_MODEL_API_KEY": old_model})
        api("/admin/models/publications", {"model_id": "rotation-model", "display_name": "Rotation model", "provider_id": "rotation", "upstream_model": "rotation-model", "enabled": True})
        accounts = api("/admin/accounts?query=Credential%20rotation%20operator")["accounts"]
        assert len(accounts) == 1 and accounts[0]["display_name"] == "Credential rotation operator"
        model_grant_id = api("/admin/models/grants", {"name": "Rotation model budget", "subject": {"kind": "user", "id": accounts[0]["user_id"]}, "model_ids": ["rotation-model"], "monthly_tokens": 1_000_000, "max_concurrent_requests": 4, "allow_resource_sharing": False})["grant_id"]
        original_config = server_read("server.json")
        old_master = json.loads(original_config)["secret_master_key"]
        new_master = __import__("base64").b64encode(secrets.token_bytes(32)).decode()
        all_secrets += [old_master, new_master]
        failed = rotate("master-key", {"TERNILO_NEXT_SECRET_MASTER_KEY": "invalid-next-key"}, success=False)
        assert failed.returncode != 0
        assert server_read("server.json") == original_config
        resume_after_known_rollback()
        contract("rotation_fixture_secret_decrypts_with_configured_key", old_master)
        rotate("master-key", {"TERNILO_NEXT_SECRET_MASTER_KEY": new_master})
        contract("rotation_fixture_secret_rejects_configured_key", old_master)
        contract("rotation_fixture_secret_decrypts_with_configured_key", new_master)
        print(f"PASS {backend}: invalid master key left original data intact; new key decrypts and old key is rejected", flush=True)

        if backend == "postgres":
            database_config = server_read("server.json")
            postgres_sql("ALTER ROLE ternilo_app RENAME TO ternilo_app_rotation_missing")
            failed = rotate("database-passwords", {"TERNILO_RUNTIME_DB_PASSWORD_NEXT": new_runtime,
                            "TERNILO_MIGRATOR_DB_PASSWORD_NEXT": new_migrator}, success=False)
            assert failed.returncode != 0
            assert server_read("server.json") == database_config
            postgres_sql("ALTER ROLE ternilo_app_rotation_missing RENAME TO ternilo_app")
            assert_login("ternilo_migrator", old_migrator, True)
            assert_login("ternilo_migrator", new_migrator, False)
            assert_login("ternilo_app", old_runtime, True)
            assert_login("ternilo_app", new_runtime, False)
            # The deliberate SQL failure and successful old-password logins prove rollback before removing its candidate.
            private_server_command("rm /var/lib/ternilo/server.next.json")
            resume_after_known_rollback()
            rotate("database-passwords", {"TERNILO_RUNTIME_DB_PASSWORD_NEXT": new_runtime,
                   "TERNILO_MIGRATOR_DB_PASSWORD_NEXT": new_migrator})
            assert_login("ternilo_migrator", old_migrator, False)
            assert_login("ternilo_app", old_runtime, False)
            assert_login("ternilo_migrator", new_migrator, True)
            assert_login("ternilo_app", new_runtime, True)
            config = json.loads(server_read("server.json"))
            assert quote(new_runtime, safe="") in config["database_url"]
            assert quote(new_migrator, safe="") in config["migration_database_url"]
            print("PASS postgres: both role changes rolled back on SQL failure; new encoded passwords work after promotion", flush=True)
        contract("rotated_stack_executes_authoritative_model_canary")

        failed = rotate("model-key", {"TERNILO_MODEL_API_KEY_NEXT": wrong_model}, canary=True, success=False)
        assert failed.returncode != 0
        assert api("/admin/models/providers/rotation/key-rotation")["rotation"] is None
        private_server_command("test ! -e /var/lib/ternilo/operator-model-key && test ! -e /var/lib/ternilo/operator-model-key.rejected")
        contract("rotated_stack_executes_authoritative_model_canary")
        expected_key_file.write_text(new_model)
        rotate("model-key", {"TERNILO_MODEL_API_KEY_NEXT": new_model}, canary=True)
        assert api("/admin/models/providers/rotation/key-rotation")["rotation"] is None
        contract("rotated_stack_executes_authoritative_model_canary")
        assert compose(worker_dir, "run", "--rm", "--no-deps", "--entrypoint", "cat", "worker", "/var/lib/ternilo/worker.json").stdout == original_worker
        with urlopen(model_origin + "/stats") as response:
            stats = json.load(response)
        assert stats["accepted"] >= 3 and stats["rejected"] >= 1, stats
        print(f"PASS {backend}: rejected model candidate rolled back; valid candidate executed with authoritative 12/4/2 usage", flush=True)

        inspect = b""
        logs = b""
        for directory in (server_dir, worker_dir):
            ids = compose(directory, "ps", "--all", "-q").stdout.decode().split()
            logs += compose(directory, "logs", "--no-color").stdout
            if ids:
                inspect += run([DOCKER, "inspect", *ids]).stdout
        if backend == "postgres":
            inspect += run([DOCKER, "inspect", postgres]).stdout
        for secret in all_secrets:
            assert secret.encode() not in inspect + logs, "plaintext credential appeared in persistent container configuration or logs"
        print(f"PASS {backend}: Worker stayed credential-scoped; persistent container inspect and logs contain no keys", flush=True)
    finally:
        for directory in (worker_dir, server_dir):
            if (directory / ".ternilo-deployment").exists():
                compose(directory, "down", "--volumes", "--remove-orphans", success=False)
        if backend == "postgres":
            run([DOCKER, "rm", "-f", "-v", postgres], success=False)
        model.terminate()
        try:
            model.wait(timeout=5)
        except subprocess.TimeoutExpired:
            model.kill()
            model.wait()
        model_log.close()
        print(f"Rotation acceptance artifacts: {temporary}", flush=True)


if __name__ == "__main__":
    selected = os.getenv("TERNILO_ROTATION_DATABASE", "postgres")
    if selected not in ("sqlite", "postgres"):
        raise ValueError("TERNILO_ROTATION_DATABASE must be sqlite or postgres")
    acceptance(selected)
