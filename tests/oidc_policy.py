#!/usr/bin/env python3
"""Tests for scripts/check-oidc-permissions.py: stricter policy checks for
workflow jobs that can mint OIDC tokens or write attestations.

Each fixture in tests/fixtures/oidc-policy is named pass_*.yml (no
problems) or fail_*.yml (at least one problem). The repository's own
workflows must pass.
"""
import glob
import importlib.util
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(ROOT, "tests", "fixtures", "oidc-policy")

spec = importlib.util.spec_from_file_location(
    "check_oidc_permissions", os.path.join(ROOT, "scripts", "check-oidc-permissions.py")
)
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


def problems(name):
    return policy.check(os.path.join(FIXTURES, name))


class Fixtures(unittest.TestCase):
    def assert_passes(self, name):
        self.assertEqual(problems(name), [], name)

    def assert_rejected(self, name, needle):
        found = problems(name)
        self.assertTrue(any(needle in p for p in found), f"{name}: {found}")

    def test_policy_accepts_attest_job(self):
        self.assert_passes("pass_attest_job.yml")

    def test_policy_accepts_id_token_only_job(self):
        self.assert_passes("pass_id_token_only_job.yml")

    def test_policy_accepts_attestations_only_job(self):
        self.assert_passes("pass_attestations_only_job.yml")

    def test_policy_rejects_checkout_in_attest_job(self):
        self.assert_rejected("fail_checkout_in_attest_job.yml", "actions/checkout")

    def test_policy_rejects_checkout_in_id_token_job(self):
        self.assert_rejected("fail_checkout_in_id_token_job.yml", "actions/checkout")

    def test_policy_rejects_project_code_in_attest_job(self):
        self.assert_rejected("fail_project_code_in_attest_job.yml", "`make`")

    def test_policy_rejects_download_from_other_run(self):
        self.assert_rejected("fail_download_other_run.yml", "input `run-id`")

    def test_policy_rejects_download_with_token(self):
        self.assert_rejected("fail_download_with_token.yml", "input `github-token`")

    def test_policy_rejects_download_by_pattern(self):
        self.assert_rejected("fail_download_by_pattern.yml", "input `pattern`")
        self.assert_rejected("fail_download_by_pattern.yml", "input `merge-multiple`")

    def test_policy_rejects_download_without_exact_name(self):
        self.assert_rejected("fail_download_wildcard_name.yml", "exact name")
        self.assert_rejected("fail_download_without_name.yml", "exact name")

    def test_policy_rejects_unlisted_run_commands(self):
        for name, cmd in [
            ("fail_run_unlisted_command_1.yml", "`curl`"),
            ("fail_run_unlisted_command_2.yml", "`wget`"),
            ("fail_run_unlisted_command_3.yml", "`gh`"),
            ("fail_run_unlisted_command_4.yml", "`gh`"),
            ("fail_run_unlisted_command_5.yml", "`curl`"),
        ]:
            with self.subTest(name):
                self.assert_rejected(name, cmd)

    def test_policy_rejects_unlisted_command_argument(self):
        self.assert_rejected("fail_run_command_argument.yml", "`-exec`")

    def test_policy_rejects_expression_not_allowed_in_run(self):
        self.assert_rejected("fail_run_expression_not_allowed.yml", "github.event")

    def test_policy_requires_exact_action_match(self):
        self.assert_rejected("fail_action_prefix_match.yml", "not pinned to a full commit SHA")
        self.assert_rejected("fail_action_tag_ref.yml", "not pinned to a full commit SHA")
        self.assert_rejected("fail_action_not_allowlisted.yml", "not an allowlisted action")

    def test_policy_requires_exact_subject_paths(self):
        self.assert_rejected("fail_attest_wildcard_subject.yml", "wildcard")
        self.assert_rejected("fail_attest_subject_digest.yml", "input `subject-digest`")

    def test_policy_requires_release_environment(self):
        self.assert_rejected("fail_no_release_environment.yml", "environment: release")

    def test_policy_rejects_container_in_attest_job(self):
        self.assert_rejected("fail_container_in_attest_job.yml", "`container`")

    def test_policy_rejects_non_bash_shell(self):
        self.assert_rejected("fail_non_bash_shell.yml", "shell `python`")

    def test_policy_rejects_workflow_level_grant(self):
        self.assert_rejected("fail_workflow_level_grant.yml", "workflow-level")

    def test_every_fail_fixture_is_rejected(self):
        for path in sorted(glob.glob(os.path.join(FIXTURES, "fail_*.yml"))):
            with self.subTest(os.path.basename(path)):
                self.assertTrue(policy.check(path))

    def test_repository_workflows_pass(self):
        paths = sorted(
            glob.glob(os.path.join(ROOT, ".github", "workflows", "*.yml"))
            + glob.glob(os.path.join(ROOT, "docs", "ci", "*.yml"))
        )
        self.assertTrue(any(p.endswith("release.yml") for p in paths))
        for path in paths:
            with self.subTest(os.path.basename(path)):
                self.assertEqual(policy.check(path), [])


if __name__ == "__main__":
    unittest.main()
