#!/usr/bin/env python3
"""Keep every release file list bound to the build matrix targets."""
import sys
from collections import Counter

import yaml


def check(doc):
    jobs = doc['jobs']
    rows = jobs['build']['strategy']['matrix']['include']
    assets = [row['asset'] for row in rows]
    problems = []
    for row in rows:
        extension = '.zip' if 'windows' in row['target'] else '.tar.gz'
        if row['asset'] != f"vibe-{row['target']}{extension}":
            problems.append(f"matrix asset does not match target: {row}")
    if len(set(assets)) != len(assets) or len({r['target'] for r in rows}) != len(rows):
        problems.append('duplicate matrix target or asset')

    def compare(label, actual, expected):
        if Counter(actual) != Counter(expected):
            problems.append(f'{label} differs from build matrix: {actual}')

    for name in ('checksums', 'attest', 'release', 'tag-path-rehearsal'):
        job = jobs[name]
        expected = assets if name == 'checksums' else assets + ['SHA256SUMS']
        key = 'RELEASE_ASSETS' if name == 'checksums' else 'RELEASE_FILES'
        compare(f'{name}.{key}', job.get('env', {}).get(key, '').split(), expected)
        downloads = [s.get('with', {}).get('name') for s in job['steps']
                     if s.get('uses', '').startswith('actions/download-artifact@')]
        compare(f'{name} downloads', downloads, expected)
        for step in job['steps']:
            if step.get('uses', '').startswith('actions/attest-build-provenance@'):
                compare(f'{name} subjects', step.get('with', {}).get('subject-path', '').split(),
                        ['dist/' + asset for asset in expected])
    return problems


if __name__ == '__main__':
    with open(sys.argv[1] if len(sys.argv) > 1 else '.github/workflows/release.yml', encoding='utf-8') as f:
        errors = check(yaml.safe_load(f))
    print('\n'.join(errors) if errors else 'Release file lists match the build matrix.')
    sys.exit(bool(errors))
