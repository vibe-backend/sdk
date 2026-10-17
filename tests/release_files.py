#!/usr/bin/env python3
"""Negative tests for release workflow file-list drift."""
import copy
import importlib.util
from pathlib import Path
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('release_files', ROOT / 'scripts/check-release-files.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class ReleaseFiles(unittest.TestCase):
    def setUp(self):
        self.doc = yaml.safe_load((ROOT / '.github/workflows/release.yml').read_text())

    def test_real_workflow(self):
        self.assertEqual(policy.check(self.doc), [])

    def test_each_duplicated_list(self):
        for name in ('checksums', 'attest', 'release', 'tag-path-rehearsal'):
            with self.subTest(name=name):
                doc = copy.deepcopy(self.doc)
                key = 'RELEASE_ASSETS' if name == 'checksums' else 'RELEASE_FILES'
                doc['jobs'][name]['env'][key] += ' extra.zip'
                self.assertTrue(policy.check(doc))

    def test_missing_target(self):
        self.doc['jobs']['build']['strategy']['matrix']['include'].pop()
        self.assertTrue(policy.check(self.doc))

    def test_wrong_asset_for_target(self):
        self.doc['jobs']['build']['strategy']['matrix']['include'][0]['target'] = 'other'
        self.assertTrue(policy.check(self.doc))

    def test_duplicate_target(self):
        rows = self.doc['jobs']['build']['strategy']['matrix']['include']
        rows.append(copy.deepcopy(rows[0]))
        self.assertTrue(policy.check(self.doc))

    def test_download_and_subject_drift(self):
        for action, key in [('download-artifact', 'name'), ('attest-build-provenance', 'subject-path')]:
            with self.subTest(action=action):
                doc = copy.deepcopy(self.doc)
                step = next(s for s in doc['jobs']['attest']['steps']
                            if s.get('uses', '').startswith('actions/' + action + '@'))
                step['with'][key] = 'unexpected'
                self.assertTrue(policy.check(doc))


if __name__ == '__main__':
    unittest.main()
