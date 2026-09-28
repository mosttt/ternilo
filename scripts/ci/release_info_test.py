import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("release_info", Path(__file__).with_name("release-info.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ReleaseInfoTests(unittest.TestCase):
    def test_publish_requires_an_exact_version_tag(self):
        MODULE.validate_ref("0.1.0", "refs/tags/v0.1.0", True)
        MODULE.validate_ref("0.1.0", "refs/heads/main", False)
        for ref in ["refs/heads/main", "refs/tags/v0.2.0", "refs/tags/v0.1.0-rc.1"]:
            with self.assertRaises(ValueError):
                MODULE.validate_ref("0.1.0", ref, True)

    def test_installers_exclude_private_build_inputs_and_reject_missing_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "bundle"
            source.mkdir()
            (source / "config.json").write_text("private")
            with self.assertRaises(ValueError):
                MODULE.collect(source, root / "release", "linux")
            (source / "Ternilo.deb").write_bytes(b"installer")
            MODULE.collect(source, root / "release", "linux")
            MODULE.checksums(root / "release")
            self.assertEqual({entry.name for entry in (root / "release").iterdir()}, {"linux-Ternilo.deb", "SHA256SUMS"})
            self.assertIn("  linux-Ternilo.deb", (root / "release/SHA256SUMS").read_text())


if __name__ == "__main__":
    unittest.main()
