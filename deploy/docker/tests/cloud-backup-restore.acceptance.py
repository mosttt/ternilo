"""Verify matched Server/database and independent Worker volume restoration."""
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tarfile
import tempfile
import time
from urllib.request import Request, urlopen
from urllib.error import HTTPError

ROOT = Path(__file__).resolve().parents[3]
HELPER = ROOT / "deploy/docker/ternilo-deploy"
DOCKER = os.getenv("TERNILO_DOCKER", "docker")
SERVER_IMAGE = os.getenv("TERNILO_ACCEPTANCE_SERVER_IMAGE", "ternilo-server:release-candidate")
WORKER_IMAGE = os.getenv("TERNILO_ACCEPTANCE_WORKER_IMAGE", "ternilo-worker:release-candidate")
TEST_IMAGE = os.getenv("TERNILO_ACCEPTANCE_TEST_IMAGE", "ternilo-acceptance:release-candidate")
CLEAN_ENV = {key: value for key, value in os.environ.items() if not key.startswith("TERNILO_")}


def command(args, *, env=None, content=None):
    result = subprocess.run([str(value) for value in args], input=content, capture_output=True,
                            env=dict(CLEAN_ENV, **(env or {})))
    if result.returncode:
        raise RuntimeError(f"Command failed: {args[0]}\n{result.stderr.decode(errors='replace')}")
    return result.stdout


def unused_port():
    with socket.socket() as listener:
        listener.bind(("0.0.0.0", 0))
        return listener.getsockname()[1]


def compose(directory, *arguments, content=None):
    component = (directory / ".ternilo-deployment").read_text().strip()
    return command([DOCKER, "compose", "--env-file", directory / ".env", "-f",
                    directory / f"compose.{component}.yml", *arguments], content=content)


def helper(directory, action, *arguments, env=None):
    environment = dict(env or {})
    if os.getenv("TERNILO_PG_RESTORE"):
        environment["TERNILO_PG_RESTORE"] = os.environ["TERNILO_PG_RESTORE"]
    return command([HELPER, action, "--directory", directory, *arguments], env=environment)


def volume(directory, component):
    configuration = json.loads(compose(directory, "config", "--format", "json"))
    return configuration["volumes"][f"{component}-data"]["name"]


def volume_read(directory, component, path):
    return compose(directory, "run", "--rm", "--no-deps", "--entrypoint", "cat", component, path)


def database_container(name, bridge, password, app_password):
    port = unused_port()
    command([DOCKER, "run", "-d", "--name", name, "-e", "POSTGRES_PASSWORD", "-e", "POSTGRES_DB=ternilo",
             "-p", f"{bridge}:{port}:5432", "postgres:17-alpine"], env={"POSTGRES_PASSWORD": password})
    for _ in range(120):
        # The entrypoint's temporary bootstrap server accepts Unix sockets before creating the requested database.
        result = subprocess.run([DOCKER, "exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "postgres", "-d", "ternilo"], capture_output=True)
        if result.returncode == 0:
            break
        time.sleep(0.25)
    else:
        raise RuntimeError("PostgreSQL fixture did not finish initialization")
    sql = (f"CREATE ROLE ternilo_runtime NOLOGIN; CREATE ROLE ternilo_app LOGIN PASSWORD '{app_password}'; "
           "GRANT ternilo_runtime TO ternilo_app; REVOKE CREATE ON SCHEMA public FROM PUBLIC; "
           "GRANT USAGE ON SCHEMA public TO ternilo_runtime;")
    command([DOCKER, "exec", "-i", name, "psql", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "ternilo"], content=sql.encode())
    return f"postgres://ternilo_app:{app_password}@{bridge}:{port}/ternilo", f"postgres://postgres:{password}@{bridge}:{port}/ternilo"


def acceptance(backend):
    directory = Path(tempfile.mkdtemp(prefix=f"ternilo-matched-restore-{backend}-"))
    os.chmod(directory, 0o700)
    identifier = secrets.token_hex(6)
    server_image = f"ternilo-restore-server:{identifier}"
    worker_image = f"ternilo-restore-worker:{identifier}"
    command([DOCKER, "tag", SERVER_IMAGE, server_image])
    command([DOCKER, "tag", WORKER_IMAGE, worker_image])
    postgres = f"ternilo-restore-postgres-{identifier}"
    source_server, source_worker = directory / "server", directory / "worker"
    restored_server, restored_worker = directory / "restored-server", directory / "restored-worker"
    deployments = [source_server, source_worker, restored_server, restored_worker]
    bridge = command([DOCKER, "network", "inspect", "bridge", "--format", "{{(index .IPAM.Config 0).Gateway}}"]).decode().strip()
    if not bridge:
        raise RuntimeError("Docker bridge has no host gateway for the isolated acceptance fixture")
    port = unused_port()
    origin = f"http://{bridge}:{port}"
    password = secrets.token_hex(24)
    environment = {"TERNILO_SERVER_OWNER_USERNAME": "owner", "TERNILO_SERVER_OWNER_EMAIL": "owner@example.test", "TERNILO_SERVER_OWNER_PASSWORD": password,
                   "TERNILO_SERVER_HTTP_BIND_ADDRESS": bridge, "TERNILO_SERVER_HTTP_PORT": str(port)}
    runtime = migration = None
    token = tenant = None
    model = model_log = None
    model_key = secrets.token_hex(24)
    expected_key = directory / "expected-model-key"
    expected_key.write_text(model_key)
    os.chmod(expected_key, 0o600)
    policy_file = directory / "worker-policy.json"
    policy = json.loads((ROOT / "deploy/docker/worker-policy.json").read_text())
    policy["minimum_workspace_free_bytes"] = 0
    policy["allowed_plugin_kinds"].append("ternilo.model.rule")
    policy_file.write_text(json.dumps(policy))
    os.chmod(policy_file, 0o600)

    def api(path, body=None, method=None):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        if tenant:
            headers["x-ternilo-tenant"] = tenant
        request = Request(origin + "/api/v1" + path, method=method or ("POST" if body is not None else "GET"),
                          headers=headers, data=json.dumps(body).encode() if body is not None else None)
        try:
            with urlopen(request, timeout=30) as response:
                return json.load(response) if response.status != 204 else None
        except HTTPError as error:
            raise RuntimeError(f"Server API {path} returned {error.code}: {error.read().decode()}") from None

    try:
        model_log = (directory / "model.stderr").open("wb")
        model = subprocess.Popen(
            ["node", str(ROOT / "deploy/docker/tests/rotation-model-fixture.mjs"), str(expected_key)],
            stdout=subprocess.PIPE, stderr=model_log,
            env=dict(CLEAN_ENV, TERNILO_ACCEPTANCE_MODEL_BIND=bridge),
        )
        model_origin = f"http://{bridge}:{int(model.stdout.readline().decode().strip())}"
        if backend == "postgres":
            runtime, migration = database_container(postgres, bridge, secrets.token_hex(24), secrets.token_hex(24))
            environment.update(TERNILO_DATABASE_URL=runtime, TERNILO_MIGRATION_DATABASE_URL=migration)
        helper(source_server, "init", "--image", server_image, "--public-url", "https://restore.example.invalid",
               "--managed-execution-enabled", "--worker-policy", policy_file, env=environment)
        helper(source_server, "up")
        login = api("/auth/login", {"username": "owner", "password": password})
        token = login["access_token"]
        tenant = login["personal_tenant_id"]
        owner_id = login["user"]["user_id"]
        grant = api("/admin/workers", {"worker_id": "worker-" + identifier})
        helper(source_worker, "init", "--component", "worker", "--image", worker_image, "--server-url", origin,
               env={"TERNILO_WORKER_TOKEN": grant["token"]})
        helper(source_worker, "up")
        worker_container = compose(source_worker, "ps", "-q", "worker").decode().strip()
        variables = json.loads(command([DOCKER, "inspect", worker_container, "--format", "{{json .Config.Env}}"]))
        assert not any(any(name in value for name in ("DATABASE_URL", "SECRET_MASTER_KEY", "MODEL_API_KEY")) for value in variables)
        project = login["personal_project_id"]
        workspace = api("/workspaces", {"project_id": project, "name": "Restored files", "placement": "cloud"})["workspace"]
        model_profile = {
            "id": "restore-upstream", "display_name": "Restored provider", "base_url": model_origin + "/v1",
            "protocol": "openai-chat-completions", "api_key_ref": None,
            "defaults": {"context_window": 128000, "max_output_tokens": 256},
            "models": [{"id": "restore-model", "display_name": "Restored model", "settings": {"mode": "inherit"}}],
            "timeout_ms": 5000, "max_attempts": 1, "retry_base_delay_ms": 10,
        }
        api("/admin/models/providers", {"profile": model_profile, "enabled": True, "api_key": model_key})
        api("/admin/models/publications", {"model_id": "restore-model", "display_name": "Restored model",
            "provider_id": "restore-upstream", "upstream_model": "restore-model", "enabled": True})
        model_grant = api("/admin/models/grants", {
            "name": "Restored allowance", "subject": {"kind": "user", "id": owner_id},
            "model_ids": ["restore-model"], "monthly_tokens": 1_000_000,
            "max_concurrent_requests": 4, "allow_resource_sharing": False,
        })
        api("/providers", dict(model_profile, id="restore-private", api_key_ref="RESTORE_PRIVATE_KEY"))
        api("/credentials", {"name": "RESTORE_PRIVATE_KEY", "value": model_key})
        model_session = api("/sessions", {"workspace_id": workspace["workspace_id"]})["identity"]["session_id"]
        platform_model = {"provider": "platform_model", "grant_id": model_grant["grant_id"], "model_id": "restore-model"}
        private_model = {"provider": "named_provider", "provider_id": "restore-private", "model": "restore-model"}

        def model_canary(selection, source):
            api(f"/sessions/{model_session}", {"model": selection}, "PATCH")
            receipt = api(f"/sessions/{model_session}/queue", {
                "delivery": "queue", "content": {"kind": "prompt", "input": "Confirm the restored model connection works."},
            })
            for _ in range(300):
                current = api(f"/tenants/{tenant}/runs/{receipt['run_id']}")["run"]
                if current["state"] in ("succeeded", "failed", "cancelled", "indeterminate"):
                    assert current["state"] == "succeeded", current.get("error")
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError("Restored model canary did not finish")
            events = api(f"/sessions/{model_session}/events")
            assert "credential rotation canary ready" in json.dumps(events)
            requests = [record for record in api("/admin/models/requests?limit=100")["requests"]
                        if (record.get("workload") or {}).get("run_id") == receipt["run_id"]]
            assert requests and all(record["source"] == source and record["actor_user_id"] == owner_id
                                    and record["resource_owner_user_id"] == owner_id for record in requests)
            assert any(attempt["usage"] is not None and attempt["usage"]["input_tokens"] == 12
                       and attempt["usage"]["output_tokens"] == 4
                       and attempt["usage"]["cached_input_tokens"] == 2 and attempt["accounted_tokens"] == 16
                       for record in requests for attempt in record["attempts"])

        model_canary(platform_model, "platform_grant")
        model_canary(private_model, "user_provider")
        pending_rotation = api("/admin/models/providers/restore-upstream/key-rotation", {"api_key": secrets.token_hex(24)})
        session = "restore-session-" + identifier
        profile = json.loads((ROOT / "examples/execution-envelope.json").read_text())["spec"]["profile"]
        profile["plugins"] = [entry for entry in profile["plugins"] if entry["id"] != "echo-tool"]
        for row, kind in [("outer", "ternilo.sandbox.cloud_outer"), ("files", "ternilo.files.local"), ("file-tools", "ternilo.tools.files")]:
            profile["plugins"].append({"id": row, "kind": kind, "config": {}})

        def run(input_text):
            record = api(f"/tenants/{tenant}/runs", {"project_id": project, "workspace_id": workspace["workspace_id"],
                "agent_id": "restore-agent", "session_id": session, "profile": profile, "input": input_text,
                "limits": {"max_steps": 4, "max_tool_calls": 4}, "permissions": "workspace_write", "mode": "execute",
                "reserved_model_tokens": 100, "references": [], "reference_contexts": [], "attachments": []})["run"]
            for _ in range(300):
                current = api(f"/tenants/{tenant}/runs/{record['run_id']}")["run"]
                if current["state"] in ("succeeded", "failed", "cancelled", "indeterminate"):
                    assert current["state"] == "succeeded", current.get("error")
                    return current
                time.sleep(0.2)
            raise RuntimeError("Container Worker did not finish its restore proof")

        run("/write preserved.txt file-from-before-backup")
        events = api(f"/sessions/{session}/events")
        attachment = next(event["attachment"] for event in events if event["type"] == "deliverable_produced")
        original_attachment = api(f"/sessions/{session}/attachments/resolve", {"attachment": attachment})
        assert original_attachment["content"] == "file-from-before-backup"
        digest = lambda value: hashlib.sha256(value.encode()).hexdigest()
        physical = f"/var/lib/ternilo/workspaces/{digest(tenant)}/{digest(workspace['workspace_id'])}"
        original_marker = volume_read(source_worker, "worker", "/var/lib/ternilo/workspaces/.ternilo-storage.json")
        assert volume_read(source_worker, "worker", physical + "/preserved.txt") == b"file-from-before-backup"
        print(f"PASS {backend}: production Worker executed in its own private volume", flush=True)

        maintenance = {"TERNILO_SERVER_OWNER_USERNAME": "owner", "TERNILO_SERVER_OWNER_PASSWORD": password}
        helper(source_server, "pause-execution", "--server-url", origin, env=maintenance)
        state = api("/admin/execution")
        assert state == {"claims_paused": True, "active_runs": 0, "active_commands": 0}, state
        helper(source_worker, "down")
        helper(source_server, "down")
        worker_backups, server_backups = directory / "worker-backups", directory / "server-backups"
        helper(source_worker, "backup", "--output-dir", worker_backups)
        backup_arguments = ["--output-dir", server_backups]
        if backend == "postgres":
            dump = command([DOCKER, "exec", postgres, "pg_dump", "-U", "postgres", "-d", "ternilo", "--format=custom"])
            dump_path = directory / "database.dump"
            dump_path.write_bytes(dump)
            os.chmod(dump_path, 0o600)
            backup_arguments += ["--database-dump", dump_path]
        if backend == "sqlite":
            backup_arguments += ["--include-image"]
        helper(source_server, "backup", *backup_arguments)
        server_backup, worker_backup = next(server_backups.glob("*.tar.gz")), next(worker_backups.glob("*.tar.gz"))
        for archive in (server_backup, worker_backup):
            assert hashlib.sha256(archive.read_bytes()).hexdigest() == Path(str(archive) + ".sha256").read_text().split()[0]
            helper(source_server, "verify", "--archive", archive)
        if backend == "sqlite":
            image_id = command([DOCKER, "image", "inspect", server_image, "--format", "{{.Id}}"]).strip()
            with tarfile.open(server_backup) as archive:
                image = directory / "image.tar"
                with archive.extractfile("image.tar") as source, image.open("wb") as target:
                    while chunk := source.read(1024 * 1024):
                        target.write(chunk)
            command([DOCKER, "image", "load", "--input", image])
            assert command([DOCKER, "image", "inspect", server_image, "--format", "{{.Id}}"]).strip() == image_id
        restore_arguments = []
        if backend == "postgres":
            command([DOCKER, "exec", postgres, "createdb", "-U", "postgres", "ternilo_restored"])
            command([DOCKER, "exec", "-i", postgres, "pg_restore", "--exit-on-error", "-U", "postgres", "-d", "ternilo_restored"], content=dump)
            runtime, migration = runtime.rsplit("/", 1)[0] + "/ternilo_restored", migration.rsplit("/", 1)[0] + "/ternilo_restored"
            restore_arguments = ["--database-url", runtime, "--migration-database-url", migration]
        helper(restored_server, "restore", "--archive", server_backup, *restore_arguments)
        helper(restored_worker, "restore", "--archive", worker_backup)
        assert volume(source_server, "server") != volume(restored_server, "server")
        assert volume(source_worker, "worker") != volume(restored_worker, "worker")
        assert volume_read(restored_worker, "worker", "/var/lib/ternilo/workspaces/.ternilo-storage.json") == original_marker
        assert volume_read(restored_worker, "worker", physical + "/preserved.txt") == b"file-from-before-backup"
        helper(restored_server, "up")
        login = api("/auth/login", {"username": "owner", "password": password})
        token = login["access_token"]
        assert login["user"]["user_id"] == owner_id
        assert login["personal_tenant_id"] == tenant and login["personal_project_id"] == project
        assert api("/admin/execution")["claims_paused"] is True
        assert api("/admin/models/providers/restore-upstream/key-rotation")["rotation"]["rotation_id"] == pending_rotation["rotation_id"]
        api(f"/admin/models/providers/restore-upstream/key-rotation/{pending_rotation['rotation_id']}/rollback", method="POST")
        assert api("/admin/models/providers/restore-upstream/key-rotation")["rotation"] is None
        helper(restored_worker, "up")
        assert api("/admin/workers")[0]["worker_id"] == grant["worker_id"]
        helper(restored_server, "resume-execution", "--server-url", origin, env=maintenance)
        model_canary(platform_model, "platform_grant")
        model_canary(private_model, "user_provider")
        print(f"PASS {backend}: restored platform grant, encrypted BYOK and pending rotation rollback completed real Worker model calls", flush=True)
        run("/read preserved.txt")
        restored_events = api(f"/sessions/{session}/events")
        assert "file-from-before-backup" in json.dumps(restored_events)
        assert attachment in [event.get("attachment") for event in restored_events]
        assert api(f"/sessions/{session}/attachments/resolve", {"attachment": attachment}) == original_attachment
        print(f"PASS {backend}: original account, credential, marker, files and retained event references survived matched restoration", flush=True)

        server_config = json.loads(volume_read(restored_server, "server", "/var/lib/ternilo/server.json"))
        contract_environment = {"TERNILO_RESTORE_RUNTIME_DATABASE_URL": server_config["database_url"],
                                "TERNILO_RESTORE_MIGRATION_DATABASE_URL": server_config.get("migration_database_url") or server_config["database_url"],
                                "TERNILO_RESTORE_WORKER_POLICY": "/var/lib/ternilo/worker-policy.json"}
        arguments = [DOCKER, "run", "--rm", "--user", "10001:10001", "--volume", f"{volume(restored_server, 'server')}:/var/lib/ternilo"]
        for name in contract_environment:
            arguments += ["-e", name]
        command(arguments + [TEST_IMAGE, "restore-acceptance", "--ignored", "--nocapture"], env=contract_environment)
        print(f"PASS {backend}: original restored-stack Rust execution/quota contract", flush=True)
    finally:
        for deployment in reversed(deployments):
            if (deployment / ".ternilo-deployment").exists():
                try:
                    compose(deployment, "down", "--volumes", "--remove-orphans")
                except RuntimeError:
                    pass
        if backend == "postgres":
            subprocess.run([DOCKER, "rm", "-f", "-v", postgres], capture_output=True)
        subprocess.run([DOCKER, "image", "rm", server_image, worker_image], capture_output=True)
        if model is not None:
            model.terminate()
            try:
                model.wait(timeout=5)
            except subprocess.TimeoutExpired:
                model.kill()
                model.wait()
        if model_log is not None:
            model_log.close()
        print(f"Acceptance artifacts: {directory}", flush=True)


def main():
    selected = os.getenv("TERNILO_ACCEPTANCE_DATABASE", "all")
    if selected not in ("sqlite", "postgres", "all"):
        raise ValueError("TERNILO_ACCEPTANCE_DATABASE must be sqlite, postgres or all")
    for backend in (["sqlite", "postgres"] if selected == "all" else [selected]):
        acceptance(backend)


if __name__ == "__main__":
    main()
