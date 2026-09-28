import json
import os
import tempfile
import unittest
from pathlib import Path

from ternilo import HarnessClient


class HarnessClientSmokeTest(unittest.TestCase):
    def test_real_runtime(self) -> None:
        binary = os.environ.get("TERNILO_BIN")
        if binary is None:
            self.skipTest("TERNILO_BIN is not set")
        with tempfile.TemporaryDirectory(prefix="ternilo-python-sdk-") as root:
            workspace = Path(root, "workspace")
            workspace.mkdir()
            command = [binary, "rpc", "--data-dir", str(Path(root, "data"))]
            with HarnessClient(command) as client:
                written = client.run("/write proof.txt python sdk proof", workspace_path=workspace)
                self.assertEqual(written.status, "idle")
                result = client.run("/read proof.txt", workspace_path=workspace)
                unconfigured = client.run("Needs a model", workspace_path=workspace)
            self.assertEqual(result.status, "idle")
            self.assertEqual(json.loads(result.answer)["content"], "python sdk proof")
            self.assertEqual(Path(workspace, "proof.txt").read_text(), "python sdk proof")
            self.assertTrue(any(event.get("type") == "tool_call_finished" for event in result.events))
            self.assertEqual(unconfigured.status, "failed")
            self.assertTrue(any(
                "Configure a Provider" in notification.get("params", {}).get("error", {}).get("message", "")
                for notification in unconfigured.notifications
            ))


if __name__ == "__main__":
    unittest.main()
