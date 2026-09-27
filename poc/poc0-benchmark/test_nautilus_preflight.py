import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import nautilus_preflight


class NautilusPreflightTests(unittest.TestCase):
    def test_insufficient_projected_space_stops_before_install_or_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            record_path = root / "preflight.json"
            observed_python_paths = []
            with (
                patch.object(
                    nautilus_preflight,
                    "_installed_version",
                    side_effect=lambda path: observed_python_paths.append(path) or None,
                ),
                patch.object(
                    nautilus_preflight.shutil,
                    "disk_usage",
                    return_value=SimpleNamespace(free=11 * 1024**3),
                ),
                patch.object(
                    nautilus_preflight,
                    "_guarded_pip_install",
                    side_effect=AssertionError("pip must not start"),
                ),
                patch.object(
                    nautilus_preflight.subprocess,
                    "run",
                    side_effect=AssertionError("build must not start"),
                ),
            ):
                exit_code = nautilus_preflight.main([
                    "--output", str(root / "report.json"),
                    "--preflight-record", str(record_path),
                    "--build-record", str(root / "build.json"),
                ])

            record = json.loads(record_path.read_text(encoding="utf-8"))
            self.assertEqual(exit_code, 3)
            self.assertEqual(record["status"], "unresolved")
            self.assertLess(record["projected_remaining_bytes"], 10 * 1024**3)
            self.assertFalse((root / "report.json").exists())
            self.assertEqual(
                observed_python_paths,
                [nautilus_preflight.ROOT / ".venv/bin/python"],
            )


if __name__ == "__main__":
    unittest.main()
