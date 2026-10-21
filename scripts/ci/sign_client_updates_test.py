import base64
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("sign_client_updates", Path(__file__).with_name("sign-client-updates.py"))
SIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIGN)


class SignedClientReleaseTests(unittest.TestCase):
    def test_signs_all_platforms_and_rejects_an_unmatched_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            targets = json.loads((SIGN.ROOT / ".github/package-targets.json").read_text())
            for entry in targets:
                target = entry["target"]
                suffix = "zip" if target.endswith("windows-msvc") else "tar.gz"
                (root / f"ternilo-0.2.20-{target}.{suffix}").write_bytes(b"fixture archive")
            key = subprocess.run(["openssl", "genpkey", "-algorithm", "ED25519"], capture_output=True, check=True).stdout
            public = subprocess.run(["openssl", "pkey", "-pubout", "-outform", "DER"], input=key, capture_output=True, check=True).stdout
            with self.assertRaises(ValueError):
                SIGN.sign(root, "0.2.20", key, bytes(32))
            self.assertFalse((root / "client-updates.json").exists())
            SIGN.sign(root, "0.2.20", key, public[12:])
            document = json.loads((root / "client-updates.json").read_text())
            self.assertEqual(len(document["artifacts"]), len(targets))
            self.assertTrue(all(item["bytes"] == 15 for item in document["artifacts"]))
            (root / "public.der").write_bytes(public)
            (root / "signature.bin").write_bytes(base64.b64decode((root / "client-updates.json.sig").read_text()))
            command = ["openssl", "pkeyutl", "-verify", "-rawin", "-pubin", "-keyform", "DER", "-inkey", str(root / "public.der"), "-in", str(root / "client-updates.json"), "-sigfile", str(root / "signature.bin")]
            self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
            (root / "client-updates.json").write_text("{}")
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertFalse(any(path.suffix == ".pem" for path in root.rglob("*")))

    def test_missing_platform_and_prerelease_are_not_published(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                SIGN.manifest(Path(directory), "0.2.20")
            with self.assertRaises(ValueError):
                SIGN.manifest(Path(directory), "0.2.20-rc.1")


if __name__ == "__main__":
    unittest.main()
