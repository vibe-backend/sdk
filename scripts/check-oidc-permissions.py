#!/usr/bin/env python3
"""Fail if a workflow job can mint an OIDC token or write attestations
outside the `release` environment.

A job may have `id-token: write` or `attestations: write` only when it runs
with `environment: release`. Workflow-level grants of either are refused.
Checks .github/workflows/*.yml and docs/ci/*.yml (or the paths given).
"""
import glob
import sys

import yaml

GUARDED = ("id-token", "attestations")


def grants(perms):
    if perms in ("write-all",):
        return list(GUARDED)
    if isinstance(perms, dict):
        return [k for k in GUARDED if perms.get(k) == "write"]
    return []


def env_name(env):
    if isinstance(env, dict):
        return env.get("name")
    return env


def check(path):
    with open(path, encoding="utf-8") as f:
        doc = yaml.safe_load(f) or {}
    problems = []
    for key in grants(doc.get("permissions")):
        problems.append(f"{path}: workflow-level permissions grant {key}: write")
    for name, job in (doc.get("jobs") or {}).items():
        job = job or {}
        granted = grants(job.get("permissions"))
        if granted and env_name(job.get("environment")) != "release":
            for key in granted:
                problems.append(
                    f"{path}: job `{name}` has {key}: write without environment: release"
                )
    return problems


def main(argv):
    paths = argv or sorted(
        glob.glob(".github/workflows/*.yml")
        + glob.glob(".github/workflows/*.yaml")
        + glob.glob("docs/ci/*.yml")
    )
    problems = [p for path in paths for p in check(path)]
    for p in problems:
        print(f"::error::{p}")
    print(f"{len(paths)} workflow files checked, {len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
