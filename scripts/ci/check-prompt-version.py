#!/usr/bin/env python3
"""Require a minor/major release for compile prompt changes, from 1.0.0 on."""
import argparse
from pathlib import Path
import re
import subprocess

# compile.rs is mostly compiler code. Its system, translation and repair prompt
# strings are covered by the fingerprint test, forcing compile_prompt.sha256 edits.
# interactive.rs has an authoring prompt, not a compile prompt; shared repair text
# lives in prompt_guide.rs.
PROMPT_FILES = ["src/compile_prompt.sha256", "src/prompt_guide.rs"]


def version_pair(version):
    major, minor, *_ = version.split("-", 1)[0].split(".")
    return int(major), int(minor)


def decide(changed_files, base_version, head_version):
    changed = sorted(set(changed_files).intersection(PROMPT_FILES))
    if not changed:
        return True, "No compile prompt files changed"
    if "-" in head_version:
        return True, "pre-release: prompt check skipped"
    if version_pair(head_version)[0] == 0:
        return True, "Before 1.0.0: prompt check skipped (rule applies from 1.0.0 on)"
    if version_pair(head_version) > version_pair(base_version):
        return True, "Compile prompt change has a minor or major version bump"
    return False, (f"Changed compile prompt files: {', '.join(changed)}; prompts may change "
                   "only in a minor (or major) release, never a patch: bump the minor "
                   "version in Cargo.toml")


def package_version(text):
    package = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", text)
    if package is None:
        raise ValueError("Cargo.toml has no [package] section")
    match = re.search(r'^version\s*=\s*"([^"]+)"', package.group(1), re.MULTILINE)
    if match is None:
        raise ValueError("Cargo.toml has no package version")
    return match.group(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    args = parser.parse_args()
    def git(*args):
        return subprocess.check_output(["git", *args], text=True)
    try:
        changed = git("diff", "--name-only", f"{args.base}...HEAD").splitlines()
        base = package_version(git("show", f"{args.base}:Cargo.toml"))
        head = package_version(Path("Cargo.toml").read_text())
        ok, message = decide(changed, base, head)
    except (subprocess.CalledProcessError, ValueError, OSError) as error:
        ok, message = False, str(error)
    print(message if ok else f"::error::{message}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
