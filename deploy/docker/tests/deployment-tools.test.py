"""Exercise deployment initialization and delivery without starting Docker."""

import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tarfile
import zipfile
import tempfile
import threading
import unittest
from unittest import mock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


ROOT = Path(__file__).resolve().parents[3]
DEPLOY = ROOT / "deploy/docker"
IMAGE_CONFIG = b'{"architecture":"amd64","rootfs":{"type":"layers","diff_ids":[]}}'
IMAGE_ID = "sha256:" + hashlib.sha256(IMAGE_CONFIG).hexdigest()


class DeploymentToolsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ternilo-deployment-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.environment = {key: value for key, value in os.environ.items() if not key.startswith("TERNILO_")}
        self.docker = self.directory / "fake-docker"
        self.docker.write_text("""#!/usr/bin/env python3
import hashlib, json, os, pathlib, sys, tarfile, io
args=sys.argv[1:]
service='worker' if any(value.endswith('compose.worker.yml') for value in args) else 'server'
state=pathlib.Path(os.environ['DEPLOY_TEST_STATE'])
with open(os.environ['DEPLOY_TEST_CALLS'],'a') as log:
    log.write(json.dumps({'args':args, 'database':os.getenv('TERNILO_DATABASE_URL'), 'worker_token_set':bool(os.getenv('TERNILO_WORKER_TOKEN'))})+'\\n')
image_config=b'{"architecture":"amd64","rootfs":{"type":"layers","diff_ids":[]}}'
config_id='sha256:'+hashlib.sha256(image_config).hexdigest()
descriptor=json.dumps({'schemaVersion':2,'config':{'digest':config_id},'layers':[]}).encode()
oci=bool(os.getenv('DEPLOY_TEST_IMAGE_OCI'))
image_id='sha256:'+hashlib.sha256(descriptor).hexdigest() if oci else config_id
if args[:2]==['image','inspect']:
    previous=[json.loads(line)['args'] for line in pathlib.Path(os.environ['DEPLOY_TEST_CALLS']).read_text().splitlines()]
    changed=os.getenv('DEPLOY_TEST_IMAGE_CHANGED') and sum(call[:2]==['image','inspect'] for call in previous)>1
    print('sha256:'+'f'*64 if changed else os.getenv('DEPLOY_TEST_IMAGE_ID',image_id)); sys.exit(0)
if args[:2]==['image','save']:
    config_name='blobs/sha256/'+config_id.removeprefix('sha256:') if oci else config_id.removeprefix('sha256:')+'.json'
    manifest=json.dumps([{'Config':config_name,'RepoTags':[args[2]],'Layers':[]}]).encode()
    files=[('manifest.json',manifest),(config_name,image_config)]
    if oci:
        index=json.dumps({'schemaVersion':2,'manifests':[{'digest':image_id}]}).encode()
        files.extend([('index.json',index),('blobs/sha256/'+image_id.removeprefix('sha256:'),descriptor)])
    with tarfile.open(args[args.index('-o')+1],'w') as bundle:
        for name,content in files:
            item=tarfile.TarInfo(name); item.size=len(content); bundle.addfile(item,io.BytesIO(content))
    sys.exit(0)
if '--entrypoint' in args and 'test' in args:
    if args[-1].endswith('server.next.json'): sys.exit(0 if os.getenv('DEPLOY_TEST_PENDING') else 1)
    sys.exit(0 if state.exists() else 1)
if args[0]=='ps':
    print(os.getenv('DEPLOY_TEST_CONSUMER','')); sys.exit(0)
if 'config' in args and '--format' in args:
    print(json.dumps({'services':{service:{'volumes':[{'type':'volume','source':service+'-data','target':'/var/lib/ternilo'}]}},'volumes':{service+'-data':{'name':'test-'+service+'-data'}}}))
if 'init' in args: state.write_text('initialized')
if 'ps' in args:
    if '--status' in args: print('running-id' if os.getenv('DEPLOY_TEST_RUNNING') else '')
    else: print(os.getenv('DEPLOY_TEST_STATUS','[{"Service":"server","State":"running","Health":"healthy"}]'))
config={'database_url':os.getenv('DEPLOY_TEST_DATABASE','sqlite:///var/lib/ternilo/server.sqlite3?mode=rwc'), 'secret_master_key':'private-fixture-key'}
if service=='worker': config={'server_url':'https://server.invalid','token':'private-worker-fixture-token','workspace_root':'/var/lib/ternilo/workspaces'}
if '--entrypoint' in args and 'cat' in args: print(json.dumps(config))
if '--entrypoint' in args and 'tar' in args:
    if '-czf' in args:
        with tarfile.open(fileobj=sys.stdout.buffer,mode='w|gz') as bundle:
            files=[('server.json',json.dumps(config).encode()),('server.sqlite3',b'fixture database')] if service=='server' else [('worker.json',json.dumps(config).encode()),('workspaces/.ternilo-storage.json',b'original marker'),('workspaces/tenant/project/.ternilo/objects/digest',b'original attachment')]
            for name,value in files:
                info=tarfile.TarInfo(name); info.size=len(value); info.mode=0o600
                if name.startswith('workspaces/tenant'): info.uid=info.gid=10001
                bundle.addfile(info,io.BytesIO(value))
            if service=='worker':
                link=tarfile.TarInfo('workspaces/tenant/project/link'); link.type=tarfile.SYMTYPE; link.linkname='.ternilo/objects/digest'
                bundle.addfile(link)
    else: pathlib.Path(os.environ['DEPLOY_TEST_RESTORED']).write_bytes(sys.stdin.buffer.read())
if '--entrypoint' in args and 'sh' in args:
    if os.getenv('DEPLOY_TEST_NONEMPTY'): sys.exit(1)
    if 'cat >' in args[-1]: pathlib.Path(os.environ['DEPLOY_TEST_RESTORED']).write_bytes(sys.stdin.buffer.read())
if 'rotate-secret-master-key' in args and os.getenv('DEPLOY_TEST_ROTATION_FAIL'): sys.exit(1)
""")
        self.docker.chmod(0o700)
        self.pg_restore = self.directory / "fixture-pg-restore"
        self.pg_restore.write_text("""#!/usr/bin/env python3
import os, pathlib, sys
with open(os.environ['DEPLOY_TEST_PG_CALLS'],'a') as calls:
    calls.write(' '.join(sys.argv[1:])+'\\n')
sys.exit(0 if sys.argv[1]=='--list' and pathlib.Path(sys.argv[2]).read_bytes()==b'PGDMP fixture' else 1)
""")
        self.pg_restore.chmod(0o700)
        self.environment.update(TERNILO_DOCKER=str(self.docker), DEPLOY_TEST_STATE=str(self.directory / "volume-state"),
                                TERNILO_PG_RESTORE=str(self.pg_restore), DEPLOY_TEST_PG_CALLS=str(self.directory / "pg-calls"),
                                DEPLOY_TEST_CALLS=str(self.directory / "docker-calls.jsonl"),
                                DEPLOY_TEST_RESTORED=str(self.directory / "restored.tar.gz"))

    def run_tool(self, *arguments, success=True, environment=None):
        result = subprocess.run([str(DEPLOY / "ternilo-deploy"), *map(str, arguments)],
                                env=environment or self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def init(self, name="server"):
        target = self.directory / name
        self.run_tool("init", "--directory", target, "--image", "ternilo:test-release-1")
        return target

    def rewrite_backup(self, archive, name, change):
        with tarfile.open(archive) as bundle:
            files = {member.name: bundle.extractfile(member).read() for member in bundle.getmembers()}
            modes = {member.name: member.mode for member in bundle.getmembers()}
        change(files)
        target = self.directory / name
        with tarfile.open(target, "w:gz") as bundle:
            for path, content in files.items():
                member = tarfile.TarInfo(path)
                member.size, member.mode = len(content), modes.get(path, 0o600)
                bundle.addfile(member, io.BytesIO(content))
        Path(str(target) + ".sha256").write_text(hashlib.sha256(target.read_bytes()).hexdigest() + "  " + target.name + "\n")
        return target

    def test_server_init_uses_real_initializer_and_preserves_existing_data(self):
        target = self.init()
        original = (target / ".env").read_bytes()
        calls = [json.loads(line) for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        initializer = next(call["args"] for call in calls if "init" in call["args"])
        self.assertIn("--non-interactive", initializer)
        self.assertIn("/var/lib/ternilo/server.json", initializer)
        self.assertFalse((target / "secrets").exists())
        self.assertFalse((target / "compose.cloud.yml").exists())
        self.assertEqual((target / ".env").stat().st_mode & 0o777, 0o600)
        for name in ("cloud-rotate-credentials.sh", "rotate-credentials.py"):
            self.assertEqual((target / name).read_bytes(), (DEPLOY / name).read_bytes())
            self.assertTrue(os.access(target / name, os.X_OK))
        self.run_tool("check", "--directory", target, "--offline")
        self.run_tool("init", "--directory", target, "--image", "ternilo:test-release-2")
        self.assertEqual((target / ".env").read_bytes(), original)
        calls = [json.loads(line) for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        self.assertEqual(sum("init" in call["args"] for call in calls), 1)

    def test_init_refuses_unversioned_image_and_nonempty_directory(self):
        for image in ("ternilo", "ternilo:latest", "ternilo:local", "registry:5000/ternilo"):
            self.run_tool("init", "--directory", self.directory / "unsafe", "--image", image, success=False)
        (self.directory / "user-data").write_text("keep me")
        self.run_tool("init", "--directory", self.directory, "--image", "ternilo:1", success=False)
        self.assertEqual((self.directory / "user-data").read_text(), "keep me")

    def test_postgres_is_optional_and_passed_privately_to_server_init(self):
        target = self.directory / "postgres"
        environment = dict(self.environment, TERNILO_DATABASE_URL="postgres://user:private@database.invalid/ternilo")
        self.run_tool("init", "--directory", target, "--image", "ternilo:release-test", environment=environment)
        self.run_tool("check", "--directory", target, "--offline")
        calls = [json.loads(line) for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        initializer = next(call for call in calls if "init" in call["args"])
        self.assertEqual(initializer["database"], environment["TERNILO_DATABASE_URL"])
        self.assertIn("TERNILO_DATABASE_URL", initializer["args"])
        self.assertNotIn(environment["TERNILO_DATABASE_URL"], initializer["args"])
        self.assertNotIn("private", (target / ".env").read_text())
        self.assertFalse((target / "worker-policy.json").exists())

    def test_worker_init_keeps_only_a_scoped_token_in_private_volume(self):
        target = self.directory / "worker"
        environment = dict(self.environment, TERNILO_WORKER_TOKEN="private-worker-fixture-token",
                           TERNILO_WORKER_MAX_ACTIVE_RUNS="3", TERNILO_WORKER_MAX_RESIDENT_RUNS="12")
        self.run_tool("init", "--component", "worker", "--directory", target, "--image", "ternilo-worker:test-1", "--server-url", "https://server.invalid", environment=environment)
        self.assertEqual((target / ".ternilo-deployment").read_text(), "worker\n")
        self.assertNotIn("token", (target / ".env").read_text().lower())
        compose = (target / "compose.worker.yml").read_text()
        for forbidden in ("database", "master_key", "model_api_key", "policy.json", "ports:"):
            self.assertNotIn(forbidden, compose)
        calls = [json.loads(line) for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        initializer = next(call for call in calls if "init" in call["args"])
        self.assertTrue(initializer["worker_token_set"])
        self.assertIn("TERNILO_WORKER_TOKEN", initializer["args"])
        self.assertNotIn(environment["TERNILO_WORKER_TOKEN"], initializer["args"])
        self.assertIn("container", initializer["args"])
        self.assertEqual(initializer["args"][initializer["args"].index("--max-active-runs") + 1], "3")
        self.assertEqual(initializer["args"][initializer["args"].index("--max-resident-runs") + 1], "12")
        self.run_tool("check", "--directory", target, "--offline")
        self.run_tool("init", "--directory", target, "--image", "ternilo-worker:test-2")
        self.run_tool("rotate-key", "--directory", target, success=False)
        self.run_tool("init", "--component", "server", "--directory", target, success=False)

    def test_worker_backup_restores_hidden_objects_marker_links_and_original_owner(self):
        target = self.directory / "worker"
        self.run_tool("init", "--component", "worker", "--directory", target, "--image", "ternilo-worker:test-1", "--server-url", "https://server.invalid",
                      environment=dict(self.environment, TERNILO_WORKER_TOKEN="private-worker-fixture-token"))
        output = self.directory / "worker-backups"
        self.run_tool("backup", "--directory", target, "--output-dir", output)
        archive = next(output.glob("*.tar.gz"))
        calls_before = Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes()
        self.run_tool("verify", "--archive", archive, "--docker", self.directory / "no-docker")
        self.assertEqual(Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes(), calls_before)
        restored = self.directory / "worker-restored"
        self.run_tool("restore", "--directory", restored, "--archive", archive, "--server-url", "https://restored.invalid")
        self.assertEqual((restored / ".ternilo-deployment").read_text(), "worker\n")
        with tarfile.open(self.environment["DEPLOY_TEST_RESTORED"]) as data:
            self.assertEqual(json.load(data.extractfile("worker.json"))["server_url"], "https://restored.invalid")
            self.assertEqual(data.extractfile("workspaces/.ternilo-storage.json").read(), b"original marker")
            item = data.getmember("workspaces/tenant/project/.ternilo/objects/digest")
            self.assertEqual(item.uid, 10001)
            self.assertEqual(data.extractfile(item).read(), b"original attachment")
            self.assertEqual(data.getmember("workspaces/tenant/project/link").linkname, ".ternilo/objects/digest")
        self.run_tool("backup", "--directory", target, "--output-dir", output, success=False,
                      environment=dict(self.environment, DEPLOY_TEST_RUNNING="1"))

    def test_managed_execution_can_be_enabled_without_editing_server_json(self):
        target = self.directory / "managed"
        self.run_tool("init", "--directory", target, "--image", "ternilo-server:test-1", "--managed-execution-enabled")
        self.assertIn("TERNILO_SERVER_MANAGED_EXECUTION_ENABLED=true", (target / ".env").read_text())
        self.run_tool("configure", "--directory", target, "--no-managed-execution-enabled")
        self.assertIn("TERNILO_SERVER_MANAGED_EXECUTION_ENABLED=false", (target / ".env").read_text())
        self.assertIn("TERNILO_SERVER_MANAGED_EXECUTION_ENABLED", (target / "compose.server.yml").read_text())

    def test_execution_policy_is_private_and_restart_recreates_the_server(self):
        target = self.init()
        policy = self.directory / "execution-policy.json"
        policy.write_text(json.dumps({"catalog_revision": "ternilo-cloud-v2", "denied_tools": []}))
        self.run_tool("configure", "--directory", target, "--worker-policy", policy)
        calls = [json.loads(line)["args"] for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        writes = [call for call in calls if '--entrypoint' in call and 'sh' in call and 'cat >' in call[-1]]
        self.assertEqual(len(writes), 2)
        self.assertTrue(all("server" in call and "worker" not in call for call in writes))
        self.assertTrue(all("sync -f" in call[-1] and ".pending" in call[-1] for call in writes))
        self.run_tool("restart", "--directory", target)
        calls = [json.loads(line)["args"] for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        self.assertIn("--force-recreate", calls[-1])
        self.run_tool("configure", "--directory", target, "--model-api-key", "", success=False)

    def test_model_provider_configuration_uses_authenticated_api_without_a_plaintext_key_file(self):
        target = self.init()
        calls = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_PUT(self):
                calls.append((self.path, self.headers.get("Authorization"), json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"has_api_key":true}')

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        profile = {"id": "test-provider", "defaults": {"context_window": 10000, "max_output_tokens": 1000}}
        profile_path = self.directory / "provider.json"
        profile_path.write_text(json.dumps(profile))
        secret = "private-platform-model-key"
        before = Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes()
        result = self.run_tool("configure-model", "--directory", target, "--server-url", f"http://127.0.0.1:{server.server_port}", "--provider-profile", profile_path,
                               environment=dict(self.environment, TERNILO_SERVER_ACCESS_TOKEN="admin-session", TERNILO_MODEL_API_KEY=secret))
        self.assertEqual(calls, [("/api/v1/admin/models/providers/test-provider", "Bearer admin-session", {"profile": profile, "enabled": True, "api_key": secret})])
        self.assertEqual(before, Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes())
        self.assertNotIn(secret, (target / ".env").read_text() + result.stdout + result.stderr)
        self.assertFalse(Path(self.environment["DEPLOY_TEST_RESTORED"]).exists())

    def test_model_key_canary_uses_server_rollback_and_recovers_an_interrupted_rotation(self):
        target = self.init()
        state = {"claims_paused": False, "active_runs": 0, "active_commands": 0, "key": "old-key", "rotation": None, "previous": None}
        calls = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def result(self, value=None):
                self.send_response(204 if value is None else 200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                if value is not None:
                    self.wfile.write(json.dumps(value).encode())

            def do_GET(self):
                calls.append(("GET", self.path))
                self.result({"rotation": state["rotation"]})

            def do_PATCH(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                state["claims_paused"] = body["claims_paused"]
                calls.append(("PATCH", self.path, state["claims_paused"]))
                self.result({key: state[key] for key in ("claims_paused", "active_runs", "active_commands")})

            def do_POST(self):
                calls.append(("POST", self.path))
                if self.path.endswith("/auth/login"):
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    state["login_inputs"] = body
                    self.result({"access_token": "temporary-interactive-session"})
                    return
                if self.path.endswith("/auth/logout"):
                    self.result()
                    return
                if self.path.endswith("/key-rotation"):
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    state["previous"], state["key"] = state["key"], body["api_key"]
                    state["rotation"] = {"rotation_id": "rotation-one", "created_at_ms": 1}
                    self.result(state["rotation"])
                else:
                    if self.path.endswith("/rollback"):
                        state["key"] = state["previous"]
                    state["rotation"] = state["previous"] = None
                    self.result()

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        environment = dict(self.environment, TERNILO_SERVER_ACCESS_TOKEN="admin-session", TERNILO_MODEL_PROVIDER_ID="provider-one", TERNILO_MODEL_API_KEY_NEXT="candidate-key")
        base = [str(target / "cloud-rotate-credentials.sh"), "--directory", str(target), "--server-url", f"http://127.0.0.1:{server.server_port}"]
        not_executable = self.directory / "not-executable-canary"
        not_executable.write_text("#!/bin/sh\nexit 0\n")
        not_executable.chmod(0o600)
        failures = {
            "nonzero exit": ["python3", "-c", "raise SystemExit(1)"],
            "missing command": [str(self.directory / "missing-canary")],
            "not executable": [str(not_executable)],
        }
        for reason, canary in failures.items():
            with self.subTest(reason=reason):
                offset = len(calls)
                result = subprocess.run([*base, "model-key", "--", *canary], capture_output=True, text=True, env=environment)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(state["key"], "old-key")
                self.assertIsNone(state["rotation"])
                self.assertFalse(state["claims_paused"])
                attempt = calls[offset:]
                self.assertEqual([call[2] for call in attempt if call[0] == "PATCH"], [True, False, True, False])
                self.assertEqual([call[1] for call in attempt if call[0] == "POST"], [
                    "/api/v1/admin/models/providers/provider-one/key-rotation",
                    "/api/v1/admin/models/providers/provider-one/key-rotation/rotation-one/rollback",
                ])
                self.assertNotIn("candidate-key", result.stdout + result.stderr)

        result = subprocess.run([*base, "model-key", "--", "python3", "-c", "raise SystemExit(0)"], capture_output=True, text=True, env=environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(state["key"], "candidate-key")
        self.assertIsNone(state["rotation"])
        self.assertIn(("POST", "/api/v1/admin/models/providers/provider-one/key-rotation/rotation-one/commit"), calls)
        docker_calls = [json.loads(line)["args"] for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        self.assertFalse(any("stop" in call or "--force-recreate" in call for call in docker_calls))

        output = self.directory / "rotation-backup"
        self.run_tool("backup", "--directory", target, "--output-dir", output)
        restored = self.directory / "rotation-restored"
        self.run_tool("restore", "--directory", restored, "--archive", next(output.glob("*.tar.gz")))
        base = [str(restored / "cloud-rotate-credentials.sh"), "--directory", str(restored), "--server-url", f"http://127.0.0.1:{server.server_port}"]
        state.update(key="interrupted-key", previous="candidate-key", rotation={"rotation_id": "interrupted-rotation", "created_at_ms": 2})
        status = subprocess.run([*base, "model-key-status"], capture_output=True, text=True, env=environment)
        self.assertEqual(json.loads(status.stdout)["rotation"]["rotation_id"], "interrupted-rotation")
        refusal = subprocess.run([*base, "model-key", "--", "python3", "-c", "raise SystemExit(0)"], capture_output=True, text=True, env=environment)
        self.assertNotEqual(refusal.returncode, 0)
        self.assertEqual(state["key"], "interrupted-key")
        recovery = subprocess.run([*base, "model-key-rollback", "--rotation-id", "interrupted-rotation"], capture_output=True, text=True, env=environment)
        self.assertEqual(recovery.returncode, 0, recovery.stderr)
        self.assertEqual(state["key"], "candidate-key")
        self.assertIsNone(state["rotation"])
        self.assertFalse(state["claims_paused"])

        rotation = runpy.run_path(str(restored / "rotate-credentials.py"))
        argv = [str(restored / "rotate-credentials.py"), *base[1:], "model-key", "--", "python3", "-c", "raise SystemExit(0)"]
        interactive = {key: value for key, value in environment.items() if key != "TERNILO_SERVER_ACCESS_TOKEN"}
        for blocked in (False, True):
            with self.subTest(interactive=True, blocked=blocked):
                state["active_runs"] = int(blocked)
                offset = len(calls)
                with mock.patch.dict(os.environ, dict(interactive, TERNILO_BACKUP_DRAIN_SECONDS="0"), clear=True), \
                     mock.patch.object(sys, "argv", argv), mock.patch.object(sys.stdin, "isatty", return_value=True), \
                     mock.patch("builtins.input", return_value="interactive-owner") as username, \
                     mock.patch("getpass.getpass", return_value="interactive-password") as password:
                    if blocked:
                        with self.assertRaisesRegex(ValueError, "Execution remains paused"):
                            rotation["main"]()
                    else:
                        self.assertEqual(rotation["main"](), 0)
                    username.assert_called_once()
                    password.assert_called_once()
                requests = calls[offset:]
                self.assertEqual(sum(call == ("POST", "/api/v1/auth/login") for call in requests), 1)
                self.assertEqual(sum(call == ("POST", "/api/v1/auth/logout") for call in requests), 1)
                self.assertEqual(requests[-1], ("POST", "/api/v1/auth/logout"))
                self.assertEqual(state["login_inputs"], {"username": "interactive-owner", "password": "interactive-password"})
                self.assertEqual(state["claims_paused"], blocked)
                self.assertIsNone(state["rotation"])
                if blocked:
                    self.assertNotIn(("POST", "/api/v1/admin/models/providers/provider-one/key-rotation"), requests)

    def test_execution_pause_waits_for_canonical_zero_and_revokes_its_temporary_login(self):
        state = {"claims_paused": False, "active_runs": 1, "active_commands": 1}
        calls = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def result(self, value=None):
                self.send_response(204 if value is None else 200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                if value is not None:
                    self.wfile.write(json.dumps(value).encode())

            def do_POST(self):
                calls.append(self.path)
                if self.path.endswith("/login"):
                    self.result({"access_token": "temporary-owner-session"})
                else:
                    self.result()

            def do_PATCH(self):
                calls.append((self.path, self.headers.get("Authorization")))
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                state["claims_paused"] = body["claims_paused"]
                self.result(dict(state))

            def do_GET(self):
                state.update(active_runs=0, active_commands=0)
                self.result(dict(state))

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        origin = f"http://127.0.0.1:{server.server_port}"
        environment = dict(self.environment, TERNILO_SERVER_OWNER_USERNAME="owner", TERNILO_SERVER_OWNER_PASSWORD="private-owner-password")
        result = self.run_tool("pause-execution", "--server-url", origin, "--drain-seconds", "3", environment=environment)
        self.assertTrue(state["claims_paused"])
        self.assertIn("active runs and commands are zero", result.stdout)
        self.assertEqual(calls[-1], "/api/v1/auth/logout")
        self.assertIn(("/api/v1/admin/execution", "Bearer temporary-owner-session"), calls)
        self.assertNotIn("temporary-owner-session", result.stdout + result.stderr)
        self.run_tool("resume-execution", "--server-url", origin, environment=environment)
        self.assertFalse(state["claims_paused"])
        state["active_runs"] = 1
        result = self.run_tool("pause-execution", "--server-url", origin, "--drain-seconds", "0", environment=environment, success=False)
        self.assertTrue(state["claims_paused"])
        self.assertIn("Do not back up yet", result.stderr)
        self.assertEqual(calls[-1], "/api/v1/auth/logout")

    def test_cli_precedence_and_status_diagnostics(self):
        target = self.directory / "cli-target"
        environment = dict(self.environment, TERNILO_DEPLOY_DIR=str(self.directory / "env-target"),
                           TERNILO_IMAGE="ternilo:env", TERNILO_SERVER_PUBLIC_URL="https://env.invalid")
        self.run_tool("init", "--directory", target, "--image", "ternilo:cli", "--public-url", "https://cli.invalid", environment=environment)
        self.assertFalse((self.directory / "env-target").exists())
        self.assertIn("ternilo:cli", (target / ".env").read_text())
        self.assertIn("https://cli.invalid", (target / ".env").read_text())
        calls = [json.loads(line) for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        initializer = next(call["args"] for call in calls if "init" in call["args"])
        self.assertEqual(initializer[initializer.index("--public-url") + 1], "https://cli.invalid")
        self.run_tool("status", "--directory", target)
        self.run_tool("up", "--directory", target)
        self.run_tool("logs", "--directory", target)
        self.run_tool("down", "--directory", target)
        environment = dict(self.environment, DEPLOY_TEST_STATUS="[]")
        result = self.run_tool("status", "--directory", target, success=False, environment=environment)
        self.assertIn("server: not created", result.stderr)

    def test_backup_and_restore_keep_configuration_and_use_a_new_volume(self):
        target = self.init()
        output = self.directory / "backups"
        self.run_tool("backup", "--directory", target, "--output-dir", output)
        archive = next(output.glob("*.tar.gz"))
        self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
        self.assertTrue(Path(str(archive) + ".sha256").exists())
        restored = self.directory / "restored"
        self.run_tool("restore", "--directory", restored, "--archive", archive)
        for name in ("cloud-rotate-credentials.sh", "rotate-credentials.py"):
            self.assertEqual((restored / name).read_bytes(), (target / name).read_bytes())
            self.assertTrue(os.access(restored / name, os.X_OK))
        source_project = next(line for line in (target / ".env").read_text().splitlines() if line.startswith("TERNILO_DEPLOY_PROJECT="))
        self.assertNotIn(source_project, (restored / ".env").read_text())
        self.assertIn("TERNILO_IMAGE=" + IMAGE_ID, (restored / ".env").read_text())
        with tarfile.open(self.environment["DEPLOY_TEST_RESTORED"]) as data:
            config = json.load(data.extractfile("./server.json"))
            self.assertEqual(config["secret_master_key"], "private-fixture-key")
            self.assertEqual(data.getmember("./server.json").uid, 10001)
        self.run_tool("restore", "--directory", restored, "--archive", archive, success=False)
        self.run_tool("backup", "--directory", target, "--output-dir", output, success=False,
                      environment=dict(self.environment, DEPLOY_TEST_RUNNING="1"))
        archive.write_bytes(archive.read_bytes() + b"corruption")
        self.run_tool("restore", "--directory", self.directory / "bad-checksum", "--archive", archive, success=False)
        self.assertFalse((self.directory / "bad-checksum").exists())

    def test_verify_is_offline_and_rejects_incomplete_backups_and_wrong_saved_images(self):
        target = self.init()
        output = self.directory / "offline-backups"
        self.run_tool("backup", "--directory", target, "--output-dir", output, "--include-image")
        archive = next(output.glob("*.tar.gz"))
        sentinel = target / "do-not-change"
        sentinel.write_text("original deployment")
        calls_before = Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes()
        environment = dict(self.environment, TERNILO_DEPLOY_BACKUP_ARCHIVE=str(self.directory / "missing-env-backup"))
        result = self.run_tool("verify", "--archive", archive, "--directory", target,
                               "--docker", self.directory / "no-docker", environment=environment)
        self.assertIn(IMAGE_ID, result.stdout)
        self.assertNotIn("private-fixture-key", result.stdout + result.stderr)
        self.assertEqual(sentinel.read_text(), "original deployment")
        incomplete = self.rewrite_backup(archive, "incomplete.tar.gz", lambda files: files.pop("server-data.tar.gz"))
        self.run_tool("verify", "--archive", incomplete, success=False)
        self.run_tool("restore", "--archive", incomplete, "--directory", self.directory / "incomplete-target", success=False)
        self.assertFalse((self.directory / "incomplete-target").exists())

        def wrong_image(files):
            with tarfile.open(fileobj=io.BytesIO(files["image.tar"])) as image:
                entries = {member.name: image.extractfile(member).read() for member in image.getmembers()}
            config = json.loads(entries["manifest.json"])[0]["Config"]
            entries[config] = b'{"different":"image"}'
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w") as image:
                for path, content in entries.items():
                    member = tarfile.TarInfo(path)
                    member.size = len(content)
                    image.addfile(member, io.BytesIO(content))
            files["image.tar"] = output.getvalue()

        changed = self.rewrite_backup(archive, "wrong-image.tar.gz", wrong_image)
        result = self.run_tool("verify", "--archive", changed, success=False)
        self.assertIn("image identity", result.stderr)
        self.assertEqual(Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes(), calls_before)

    def test_backup_rejects_image_drift_and_restore_requires_the_recorded_image_before_mutation(self):
        target = self.init()
        output = self.directory / "drifting-backups"
        result = self.run_tool("backup", "--directory", target, "--output-dir", output, success=False,
                               environment=dict(self.environment, DEPLOY_TEST_IMAGE_CHANGED="1"))
        self.assertIn("image changed", result.stderr)
        self.assertEqual(list(output.iterdir()), [])
        output = self.directory / "stable-backups"
        self.run_tool("backup", "--directory", target, "--output-dir", output)
        archive = next(output.glob("*.tar.gz"))
        restored = self.directory / "wrong-image-target"
        calls_before = len(Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines())
        result = self.run_tool("restore", "--archive", archive, "--directory", restored, "--image", "ternilo:other-release", success=False,
                               environment=dict(self.environment, DEPLOY_TEST_IMAGE_ID="sha256:" + "a" * 64))
        self.assertIn("image identity", result.stderr)
        self.assertFalse(restored.exists())
        calls = [json.loads(line)["args"] for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()][calls_before:]
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][:2], ["image", "inspect"])

    def test_oci_descriptor_image_identity_survives_offline_verification_and_restore(self):
        target = self.init()
        output = self.directory / "oci-backups"
        environment = dict(self.environment, DEPLOY_TEST_IMAGE_OCI="1")
        self.run_tool("backup", "--directory", target, "--output-dir", output, "--include-image", environment=environment)
        archive = next(output.glob("*.tar.gz"))
        with tarfile.open(archive) as bundle:
            metadata = json.load(bundle.extractfile("backup.json"))
            self.assertNotEqual(metadata["image_id"], IMAGE_ID)
        self.run_tool("verify", "--archive", archive, "--docker", self.directory / "no-docker")
        restored = self.directory / "oci-restored"
        self.run_tool("restore", "--directory", restored, "--archive", archive, environment=environment)
        self.assertIn("TERNILO_IMAGE=" + metadata["image_id"], (restored / ".env").read_text())

    def test_postgres_dump_verification_is_mandatory_and_read_only(self):
        target = self.init()
        environment = dict(self.environment, DEPLOY_TEST_DATABASE="postgres://user:secret@source.invalid/ternilo")
        output = self.directory / "checked-pg-backups"
        dump = self.directory / "database.dump"
        dump.write_bytes(b"plain SQL is not a custom dump")
        result = self.run_tool("backup", "--directory", target, "--output-dir", output, "--database-dump", dump,
                               success=False, environment=environment)
        self.assertIn("custom-format", result.stderr)
        self.assertFalse(output.exists())
        dump.write_bytes(b"PGDMP fixture")
        result = self.run_tool("backup", "--directory", target, "--output-dir", output, "--database-dump", dump,
                               "--pg-restore", self.directory / "missing-pg-restore", success=False, environment=environment)
        self.assertIn("matching pg_restore", result.stderr)
        self.assertFalse(output.exists())
        self.run_tool("backup", "--directory", target, "--output-dir", output, "--database-dump", dump, environment=environment)
        archive = next(output.glob("*.tar.gz"))
        calls_before = Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes()
        self.run_tool("verify", "--archive", archive, "--docker", self.directory / "no-docker", "--pg-restore", self.pg_restore,
                      environment=dict(self.environment, TERNILO_PG_RESTORE=str(self.directory / "bad-env-pg-restore")))
        broken = self.rewrite_backup(archive, "unreadable-pg.tar.gz", lambda files: files.update({"database.dump": b"PGDMP broken"}))
        result = self.run_tool("verify", "--archive", broken, success=False)
        self.assertIn("not readable", result.stderr)
        self.assertEqual(Path(self.environment["DEPLOY_TEST_CALLS"]).read_bytes(), calls_before)
        self.assertTrue(all(line.startswith("--list ") for line in Path(self.environment["DEPLOY_TEST_PG_CALLS"]).read_text().splitlines()))

    def test_postgres_backup_requires_dump_and_restore_requires_explicit_new_database(self):
        target = self.init()
        environment = dict(self.environment, DEPLOY_TEST_DATABASE="postgres://user:secret@source.invalid/ternilo")
        output = self.directory / "pg-backups"
        self.run_tool("backup", "--directory", target, "--output-dir", output, success=False, environment=environment)
        dump = self.directory / "database.dump"
        dump.write_bytes(b"PGDMP fixture")
        self.run_tool("backup", "--directory", target, "--output-dir", output, "--database-dump", dump, environment=environment)
        archive = next(output.glob("*.tar.gz"))
        restored = self.directory / "pg-restored"
        self.run_tool("restore", "--directory", restored, "--archive", archive, success=False)
        self.assertFalse(restored.exists())
        self.run_tool("restore", "--directory", restored, "--archive", archive,
                      "--database-url", "postgres://user:new@new.invalid/ternilo")
        with tarfile.open(self.environment["DEPLOY_TEST_RESTORED"]) as data:
            config = json.load(data.extractfile("./server.json"))
            self.assertEqual(config["database_url"], "postgres://user:new@new.invalid/ternilo")
            self.assertIsNone(config["migration_database_url"])

    def test_key_rotation_prepares_private_recovery_config_before_database_and_never_prints_keys(self):
        target = self.init()
        next_key = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="
        result = self.run_tool("rotate-key", "--directory", target, "--next-key", next_key)
        self.assertNotIn(next_key, result.stdout + result.stderr)
        prepared = json.loads(Path(self.environment["DEPLOY_TEST_RESTORED"]).read_text())
        self.assertEqual(prepared["secret_master_key"], next_key)
        calls = [json.loads(line)["args"] for line in Path(self.environment["DEPLOY_TEST_CALLS"]).read_text().splitlines()]
        rotation = next(index for index, args in enumerate(calls) if "rotate-secret-master-key" in args)
        self.assertIn("server.next.json", calls[rotation - 1][-1])
        self.assertNotIn(next_key, calls[rotation])
        self.assertIn("mv /var/lib/ternilo/server.next.json", calls[rotation + 1][-1])
        self.assertIn("sync -f", calls[rotation - 1][-1])
        self.assertIn("sync -f", calls[rotation + 1][-1])
        result = self.run_tool("rotate-key", "--directory", target, "--next-key", next_key, success=False,
                               environment=dict(self.environment, DEPLOY_TEST_ROTATION_FAIL="1"))
        self.assertIn("preserve server.json and server.next.json", result.stderr)
        self.run_tool("rotate-key", "--directory", target, "--next-key", "invalid", success=False)

    def test_unfinished_rotation_blocks_start_and_backup_and_other_volume_consumers_block_mutation(self):
        target = self.init()
        for action in ("up", "backup"):
            result = self.run_tool(action, "--directory", target, "--output-dir", self.directory / "blocked",
                                   success=False, environment=dict(self.environment, DEPLOY_TEST_PENDING="1"))
            self.assertIn("Unfinished key rotation", result.stderr)
        self.assertFalse((self.directory / "blocked").exists())
        for action in ("backup", "rotate-key"):
            result = self.run_tool(action, "--directory", target, "--output-dir", self.directory / "blocked",
                                   success=False, environment=dict(self.environment, DEPLOY_TEST_CONSUMER="other-project-container"))
            self.assertIn("Another running container", result.stderr)

    def test_release_archive_has_executables_and_no_development_or_secrets(self):
        binaries = self.directory / "bin"
        binaries.mkdir()
        for name in ("ternilo", "ternilo-server", "ternilo-worker", "ternilo-plugin"):
            path = binaries / name
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o700)
        command = [str(ROOT / "scripts/package-release.sh"), "--no-build", "--version", "test-1", "--target-name", "test-host",
                   "--bin-dir", str(binaries), "--output-dir", str(self.directory / "release")]
        result = subprocess.run(command, env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        archive = self.directory / "release/ternilo-all-test-1-test-host.tar.gz"
        with tarfile.open(archive) as bundle:
            members = bundle.getmembers()
            self.assertTrue(any(member.name.endswith("/bin/ternilo-server") and member.mode & 0o111 for member in members))
            self.assertFalse(any("/secrets/" in member.name or "/docs/development" in member.name or member.name.endswith("/.env") for member in members))
            self.assertTrue(any(member.name.endswith("/.env.server.example") for member in members))
            for name in ("README.md", "README.zh-CN.md", "docs/en/README.md", "docs/zh-CN/README.md",
                         "THIRD_PARTY_NOTICES.md", "licenses/web-interface.MIT"):
                self.assertEqual(bundle.extractfile(f"ternilo-all-test-1-test-host/{name}").read(), (ROOT / name).read_bytes())
            for name in ("cloud-rotate-credentials.sh", "rotate-credentials.py"):
                tool = bundle.getmember(f"ternilo-all-test-1-test-host/deploy/docker/{name}")
                self.assertTrue(tool.mode & 0o111)
                self.assertEqual(bundle.extractfile(tool).read(), (DEPLOY / name).read_bytes())
            for path in ("examples/openai-compatible-profile.json", "examples/rhai-echo-extension/extension.rhai",
                         "examples/wasm-echo-plugin/src/lib.rs", "crates/ternilo-extension/wit/plugin.wit",
                         "sdk/python/src/ternilo/client.py", "sdk/typescript/src/client.ts",
                         "sdk/python/src/ternilo/server.py", "sdk/typescript/src/server.ts"):
                self.assertTrue(any(member.name.endswith("/" + path) for member in members), path)
        self.assertTrue(Path(str(archive) + ".sha256").is_file())
        result = subprocess.run(command, env=self.environment, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)


    def test_release_components_require_and_include_only_selected_binaries(self):
        expected = {
            "local": {"ternilo", "ternilo-plugin"},
            "server": {"ternilo-server"},
            "worker": {"ternilo-worker"},
        }
        for component, names in expected.items():
            with self.subTest(component=component):
                binaries = self.directory / component
                binaries.mkdir()
                for name in names:
                    path = binaries / name
                    path.write_text("#!/bin/sh\nexit 0\n")
                    path.chmod(0o700)
                command = [str(ROOT / "scripts/package-release.sh"), "--no-build", "--version", "scope-test",
                           "--target-name", "test-host", "--bin-dir", str(binaries),
                           "--output-dir", str(self.directory / "release")]
                environment = dict(self.environment, TERNILO_RELEASE_COMPONENT=component)
                if component == "local":
                    environment["TERNILO_RELEASE_COMPONENT"] = "worker"
                    command += ["--component", "local"]
                result = subprocess.run(command, env=environment, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                prefix = f"{'ternilo' if component == 'local' else 'ternilo-' + component}-scope-test-test-host"
                with tarfile.open(self.directory / f"release/{prefix}.tar.gz") as bundle:
                    members = bundle.getmembers()
                    actual = {Path(member.name).name for member in members
                              if member.isfile() and Path(member.name).parent.as_posix() == f"{prefix}/bin"}
                    self.assertEqual(actual, names)
                    metadata = bundle.extractfile(f"{prefix}/RELEASE").read().decode()
                    self.assertIn(f"component={component}\n", metadata)
                    self.assertEqual(set(metadata.split("binaries=", 1)[1].strip().split()), names)
                    self.assertFalse(any("/secrets/" in member.name or "/docs/development" in member.name
                                         or member.name.endswith("/.env") for member in members))

    def test_windows_release_requires_the_matching_sandbox_runner(self):
        binaries = self.directory / "windows-binaries"
        binaries.mkdir()
        for name in ("ternilo.exe", "ternilo-plugin.exe", "ternilo-server.exe"):
            (binaries / name).write_bytes(b"windows-fixture")
            (binaries / name).chmod(0o700)
        command = [str(ROOT / "scripts/package-release.sh"), "--no-build", "--version", "windows-test",
                   "--component", "local", "--target-name", "x86_64-pc-windows-msvc", "--binary-suffix", ".exe",
                   "--bin-dir", str(binaries), "--output-dir", str(self.directory / "release")]
        missing = subprocess.run(command, env=self.environment, capture_output=True, text=True)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("ternilo-sandbox-windows.exe", missing.stderr)
        (binaries / "ternilo-sandbox-windows.exe").write_bytes(b"sandbox-fixture")
        (binaries / "ternilo-sandbox-windows.exe").chmod(0o700)
        result = subprocess.run(command, env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        archive = self.directory / "release/ternilo-windows-test-x86_64-pc-windows-msvc.zip"
        with zipfile.ZipFile(archive) as bundle:
            self.assertIsNone(bundle.testzip())
            binaries = {Path(member.filename).name for member in bundle.infolist() if not member.is_dir() and "/bin/" in member.filename}
            self.assertEqual(binaries, {"ternilo.exe", "ternilo-plugin.exe", "ternilo-sandbox-windows.exe"})
            prefix = archive.stem
            self.assertEqual(bundle.read(prefix + "/bin/ternilo.exe"), b"windows-fixture")
            self.assertEqual(bundle.read(prefix + "/LICENSE"), (ROOT / "LICENSE").read_bytes())
            self.assertIn(b"(START-HERE.en.md)", bundle.read(prefix + "/START-HERE.md"))
            self.assertIn(b"(START-HERE.md)", bundle.read(prefix + "/START-HERE.en.md"))
            for language in ("zh-CN", "en"):
                self.assertIn(prefix + "/docs/" + language + "/getting-started.md", bundle.namelist())
            self.assertFalse(any("/docs/development/" in name or name.endswith("/.env") for name in bundle.namelist()))
        self.assertEqual(Path(str(archive) + ".sha256").read_text().split()[0], hashlib.sha256(archive.read_bytes()).hexdigest())
        repeated = subprocess.run(command, env=self.environment, capture_output=True, text=True)
        self.assertNotEqual(repeated.returncode, 0)

    def test_release_component_help_validation_and_missing_local_plugin(self):
        script = str(ROOT / "scripts/package-release.sh")
        help_result = subprocess.run([script, "--help"], env=self.environment, capture_output=True, text=True)
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("--component", help_result.stdout)
        self.assertIn("TERNILO_RELEASE_COMPONENT", help_result.stdout)
        result = subprocess.run([script, "--no-build", "--version", "test", "--component", "unknown"],
                                env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        binaries = self.directory / "bin"
        binaries.mkdir()
        (binaries / "ternilo").write_text("#!/bin/sh\nexit 0\n")
        (binaries / "ternilo").chmod(0o700)
        result = subprocess.run([script, "--no-build", "--version", "test", "--component", "local",
                                 "--bin-dir", str(binaries), "--output-dir", str(self.directory / "release")],
                                env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("ternilo-plugin", result.stderr)
        self.assertFalse((self.directory / "release").exists())


if __name__ == "__main__":
    unittest.main()
