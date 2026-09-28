import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("windows_sidecar", ROOT / "scripts/prepare-windows-sidecar.py")
SIDECAR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIDECAR)


class WindowsSidecarTests(unittest.TestCase):
    def test_stages_reported_artifact_with_custom_cargo_target_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "custom build directory" / "runner.exe"
            binary.parent.mkdir()
            binary.write_bytes(b"test sandbox artifact")
            report = json.dumps({"reason": "compiler-artifact", "target": {"name": "ternilo-sandbox-windows"}, "executable": str(binary)})
            output = root / "sidecars"
            with patch.object(SIDECAR.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, report)) as cargo:
                staged = SIDECAR.prepare("aarch64-pc-windows-msvc", output)
            self.assertEqual(staged.name, "ternilo-sandbox-windows-aarch64-pc-windows-msvc.exe")
            self.assertEqual(staged.read_bytes(), binary.read_bytes())
            args = cargo.call_args.args[0]
            self.assertIn("--locked", args)
            self.assertIn("--release", args)
            self.assertEqual(args[args.index("--target") + 1], "aarch64-pc-windows-msvc")

    def test_failed_build_keeps_previous_sidecar(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            previous = output / "ternilo-sandbox-windows-x86_64-pc-windows-msvc.exe"
            previous.write_bytes(b"previous build")
            with patch.object(SIDECAR.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "cargo")):
                with self.assertRaises(subprocess.CalledProcessError):
                    SIDECAR.prepare("x86_64-pc-windows-msvc", output)
            self.assertEqual(previous.read_bytes(), b"previous build")

    def test_missing_or_wrong_artifact_cannot_stage_a_stale_executable(self):
        reports = [
            {"reason": "build-finished", "success": True},
            {"reason": "compiler-artifact", "target": {"name": "other-binary"}, "executable": "other.exe"},
            {"reason": "compiler-artifact", "target": {"name": "ternilo-sandbox-windows"}, "executable": "missing.exe"},
        ]
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "sidecars"
            for report in reports:
                with self.subTest(report=report), patch.object(SIDECAR.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(report))):
                    with self.assertRaises(ValueError):
                        SIDECAR.prepare("x86_64-pc-windows-msvc", output)
                    self.assertFalse(output.exists())

    def test_rejects_non_windows_and_path_targets_before_building(self):
        with patch.object(SIDECAR.subprocess, "run") as cargo:
            for target in ["x86_64-unknown-linux-gnu", "../runner", "x86_64-pc-windows-msvc/other"]:
                with self.subTest(target=target), self.assertRaises(ValueError):
                    SIDECAR.prepare(target, Path("unused"))
            cargo.assert_not_called()

    def test_uses_compiler_host_on_windows(self):
        with patch.object(SIDECAR.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "rustc 1.98\nhost: x86_64-pc-windows-msvc\n")):
            self.assertEqual(SIDECAR.host_target(), "x86_64-pc-windows-msvc")


if __name__ == "__main__":
    unittest.main()
