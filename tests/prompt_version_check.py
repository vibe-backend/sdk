#!/usr/bin/env python3
"""Unit tests for the compile prompt release policy."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "prompt_version", Path(__file__).resolve().parents[1] / "scripts/ci/check-prompt-version.py")
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class PromptVersionTests(unittest.TestCase):
    def test_no_prompt_change(self):
        self.assertTrue(policy.decide(["src/compile.rs"], "1.0.0", "1.0.0")[0])

    def test_release_versions(self):
        for base, head, expected in [
            ("1.0.3", "1.1.0", True),
            ("1.0.0", "1.0.1", False),
            ("1.0.0", "1.0.0", False),
            ("1.4.2", "2.0.0", True),
            ("1.0.0-rc.2", "1.0.0-rc.3", True),
            ("0.1.0", "0.1.1", True),
            ("1.0.0-rc.3", "1.0.0", False),
        ]:
            for path in policy.PROMPT_FILES:
                with self.subTest(base=base, head=head, path=path):
                    ok, message = policy.decide([path], base, head)
                    self.assertEqual(ok, expected)
                    if not expected:
                        self.assertIn(path, message)
                        self.assertIn("never a patch", message)
                        self.assertIn("bump the minor version in Cargo.toml", message)
                    if "-" in head:
                        self.assertIn("pre-release: prompt check skipped", message)


if __name__ == "__main__":
    unittest.main()
