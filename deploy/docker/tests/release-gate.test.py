#!/usr/bin/env python3
"""Check release orchestration without building images or starting services."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPOSITORY = Path(__file__).resolve().parents[3]


class ReleaseGateTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ternilo release gate ")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.repository = self.directory / "ternilo"
        self.docker = self.repository / "deploy/docker"
        self.docker.joinpath("tests").mkdir(parents=True)
        self.script = self.docker / "release-gate.sh"
        shutil.copy2(REPOSITORY / "deploy/docker/release-gate.sh", self.script)
        self.linorun = self.directory / "linorun"
        self.linorun.mkdir()
        self.linorun.joinpath("Cargo.toml").write_text("[workspace]\n")
        self.artifacts = self.directory / "artifacts"
        self.calls = self.directory / "calls.jsonl"
        binaries = self.directory / "bin"
        binaries.mkdir()
        mock = binaries / "mock"
        mock.write_text(f"#!{sys.executable}\n" + '''import json, os, sys
from pathlib import Path
name = Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["GATE_TEST_CALLS"], "a") as output:
    output.write(json.dumps({"name": name, "args": args, "env": {
        key: value for key, value in os.environ.items() if key.startswith("TERNILO_")
    }}) + "\\n")
failure = os.environ.get("GATE_TEST_FAIL", "")
if failure == "worker-image" and name == "docker" and args[:3] == ["build", "--target", "worker"]:
    print("worker image build failed", file=sys.stderr)
    sys.exit(7)
if failure == "restore" and name == "node" and args == ["--release-fixture", "restore"]:
    print("restored file does not match", file=sys.stderr)
    sys.exit(8)
if name == "docker" and args[:2] == ["image", "inspect"]:
    print("sha256:" + args[2])
if name in ("node", "rustc") and args == ["--version"]:
    print(name + " fixture")
''')
        mock.chmod(0o755)
        for name in ("cargo", "docker", "node", "npm", "python3", "rustc"):
            binaries.joinpath(name).symlink_to(mock)
        for filename, fixture in (
            ("cloud-backup-restore.acceptance.sh", "restore"),
            ("cloud-credential-rotation.acceptance.sh", "rotation"),
        ):
            self.docker.joinpath("tests", filename).write_text(
                f"#!/bin/sh\nexec node --release-fixture {fixture}\n"
            )
        self.environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith("TERNILO_")
        }
        self.environment.update({
            "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
            "GATE_TEST_CALLS": str(self.calls),
            "TERNILO_RELEASE_ARTIFACT_DIR": str(self.artifacts),
        })

    def run_gate(self, *arguments, environment=None):
        return subprocess.run(
            ["sh", str(self.script), *arguments],
            env={**self.environment, **(environment or {})},
            capture_output=True, text=True, check=False,
        )

    def recorded_calls(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def test_builds_and_checks_both_components_and_records_exact_passed_gates(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.recorded_calls()
        builds = [call for call in calls if call["name"] == "docker" and call["args"][0] == "build"]
        self.assertEqual([call["args"][2] for call in builds], ["server", "worker", "acceptance"])
        for call, image in zip(builds, ("ternilo-server:release-candidate", "ternilo-worker:release-candidate", "ternilo-acceptance:release-candidate")):
            self.assertEqual(call["args"][call["args"].index("-t") + 1], image)
            source = next(argument.removeprefix("linorun=") for argument in call["args"] if argument.startswith("linorun="))
            self.assertEqual(Path(source).resolve(), self.linorun)
        self.assertFalse(any("compose.cloud.yml" in arg or "compose.relay.yml" in arg for call in calls for arg in call["args"]))
        container = next(call for call in calls if "web/tests/cloud-browser-e2e.test.mjs" in call["args"])
        self.assertEqual(container["env"]["TERNILO_CLOUD_E2E_CONTAINER"], "1")
        self.assertEqual(container["env"]["TERNILO_CLOUD_E2E_SERVER_IMAGE"], "ternilo-server:release-candidate")
        self.assertEqual(container["env"]["TERNILO_CLOUD_E2E_WORKER_IMAGE"], "ternilo-worker:release-candidate")
        fixtures = [call for call in calls if call["args"][:1] == ["--release-fixture"]]
        self.assertEqual([call["args"][1] for call in fixtures], ["restore", "rotation", "rotation"])
        self.assertEqual(fixtures[0]["env"]["TERNILO_ACCEPTANCE_DATABASE"], "all")
        self.assertEqual([call["env"]["TERNILO_ROTATION_DATABASE"] for call in fixtures[1:]], ["sqlite", "postgres"])
        for call in fixtures:
            self.assertEqual(call["env"]["TERNILO_ACCEPTANCE_SKIP_BUILD"], "1")
            self.assertEqual(call["env"]["TERNILO_ACCEPTANCE_SERVER_IMAGE"], "ternilo-server:release-candidate")
            self.assertEqual(call["env"]["TERNILO_ACCEPTANCE_WORKER_IMAGE"], "ternilo-worker:release-candidate")
            self.assertEqual(call["env"]["TERNILO_ACCEPTANCE_TEST_IMAGE"], "ternilo-acceptance:release-candidate")
        report_file = next(self.artifacts.glob("*/release.json"))
        report = json.loads(report_file.read_text())
        self.assertEqual(set(report["images"]), {"server", "worker"})
        self.assertEqual(report["images"]["server"]["image_id"], "sha256:ternilo-server:release-candidate")
        self.assertEqual(report["images"]["worker"]["image_id"], "sha256:ternilo-worker:release-candidate")
        self.assertEqual(report["gates"], report_file.with_name("gates.txt").read_text().splitlines())
        self.assertIn("server-browser-container", report["gates"])
        self.assertIn("cloud-credential-rotation-postgres", report["gates"])
        checksum = subprocess.run(["sha256sum", "--check", "SHA256SUMS"], cwd=report_file.parent, capture_output=True, check=False)
        self.assertEqual(checksum.returncode, 0, checksum.stderr)

    def test_cli_overrides_each_environment_value_including_paths_with_spaces(self):
        other_source = self.directory / "explicit Linorun"
        other_source.mkdir()
        other_source.joinpath("Cargo.toml").write_text("[workspace]\n")
        explicit_output = self.directory / "explicit artifacts"
        result = self.run_gate(
            "--server-image", "server:explicit", "--worker-image", "worker:explicit",
            "--test-image", "acceptance:explicit",
            "--linorun-source", str(other_source), "--artifact-dir", str(explicit_output),
            environment={"TERNILO_RELEASE_SERVER_IMAGE": "server:environment", "TERNILO_RELEASE_WORKER_IMAGE": "worker:environment", "TERNILO_ACCEPTANCE_TEST_IMAGE": "acceptance:environment", "TERNILO_LINORUN_SOURCE": str(self.directory / "missing")},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.artifacts.exists())
        report = json.loads(next(explicit_output.glob("*/release.json")).read_text())
        self.assertEqual(report["images"]["server"]["image"], "server:explicit")
        self.assertEqual(report["images"]["worker"]["image"], "worker:explicit")
        builds = [call for call in self.recorded_calls() if call["name"] == "docker" and call["args"][0] == "build"]
        self.assertTrue(all("linorun=" + str(other_source) in call["args"] for call in builds))
        self.assertEqual(builds[-1]["args"][builds[-1]["args"].index("-t") + 1], "acceptance:explicit")

    def test_failed_build_stops_before_container_acceptance_and_never_claims_success(self):
        result = self.run_gate(environment={"GATE_TEST_FAIL": "worker-image"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("FAIL worker-image", result.stderr)
        self.assertFalse(list(self.artifacts.glob("*/release.json")))
        self.assertFalse(any(call["args"][:1] == ["--release-fixture"] for call in self.recorded_calls()))

    def test_failed_restore_stops_before_rotation_and_keeps_diagnostics(self):
        result = self.run_gate(environment={"GATE_TEST_FAIL": "restore"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("restored file does not match", result.stderr)
        self.assertFalse(list(self.artifacts.glob("*/release.json")))
        self.assertFalse(any(call["args"] == ["--release-fixture", "rotation"] for call in self.recorded_calls()))
        self.assertIn("restored file does not match", next(self.artifacts.glob("*/cloud-fresh-restore.log")).read_text())

    def test_help_and_invalid_or_colliding_arguments_do_not_start_builds(self):
        help_result = self.run_gate("--help")
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("TERNILO_RELEASE_SERVER_IMAGE", help_result.stdout)
        self.assertIn("TERNILO_RELEASE_WORKER_IMAGE", help_result.stdout)
        self.assertIn("TERNILO_ACCEPTANCE_TEST_IMAGE", help_result.stdout)
        for arguments in (("--image", "old:single"), ("unexpected",), ("--server-image", "same:tag", "--worker-image", "same:tag"), ("--test-image", "ternilo-server:release-candidate")):
            with self.subTest(arguments=arguments):
                self.assertEqual(self.run_gate(*arguments).returncode, 2)
        self.assertFalse(self.calls.exists())


if __name__ == "__main__":
    unittest.main()
