#!/usr/bin/env python3
"""Policy checks for workflow jobs that can mint OIDC tokens or write
attestations.

A job is guarded when it has `id-token: write` or `attestations: write`
(either one, or `write-all`). Workflow-level grants of either are refused.
Every guarded job must:

- run with `environment: release`, on a hosted runner, with no `container`,
  `services` or job-level reusable workflow `uses`;
- use only allowlisted actions, matched exactly as owner/repo@<40-char SHA>
  (no checkout, no other actions);
- download artifacts from the current run only, by exact `name` with an
  optional `path` and no other inputs;
- attest only exact `subject-path` entries (no wildcards);
- in `run` steps (bash only), match an exact reviewed verification program and call only allowlisted
  commands; anything else,
  including any other way to fetch files, is refused.

Checks .github/workflows/*.yml and docs/ci/*.yml (or the paths given).
"""
import hashlib
import glob
import re
import sys

import yaml

GUARDED = ("id-token", "attestations")

DOWNLOAD_ARTIFACT = "actions/download-artifact"
ATTEST = "actions/attest-build-provenance"
UPLOAD_ARTIFACT = "actions/upload-artifact"

# Exact action references allowed in guarded jobs.
ALLOWED_ACTIONS = {
    f"{DOWNLOAD_ARTIFACT}@9000827ccba6bdab643e8b6fd33ac0654aef8333",  # v8.0.2
    f"{ATTEST}@4d101475d8b20a2381f78447822ac1eab6504dd8",  # v4.2.2
    f"{UPLOAD_ARTIFACT}@cf430e030ddbb5b0abf93d22962f4752f3646cd9",  # v7.0.2
}

# Inputs each allowlisted action may take in a guarded job.
ALLOWED_INPUTS = {
    DOWNLOAD_ARTIFACT: {"name", "path"},
    ATTEST: {"subject-path"},
    UPLOAD_ARTIFACT: {"name", "path", "if-no-files-found"},
}

# Commands a `run` step in a guarded job may call: shell syntax plus local
# file inspection and checksum verification. Nothing that runs project code
# or reaches the network.
ALLOWED_COMMANDS = {
    "set", "if", "then", "elif", "else", "fi", "for", "in", "do", "done",
    "while", "case", "esac", "{", "}", "!", "[", "[[", "test", "true",
    "false", "exit", "echo", "printf", "cd", "mkdir", "mv", "cp", "find",
    "sort", "diff", "wc", "awk", "sha256sum", "cut", "cat",
}

# Arguments that make an allowlisted command run another command.
FORBIDDEN_WORDS = {"-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls"}
FORBIDDEN_TEXT = ("system(", "getline", "|&")
ALLOWED_SHELLS = {None, "bash"}
# Expressions a guarded `run` step may interpolate.
EXPR_OK = re.compile(r"^(steps|needs|env|runner)\.[A-Za-z0-9_.-]+$|^github\.(sha|ref|ref_name)$")
FORBIDDEN_ENV = {"BASH_ENV", "ENV", "LD_PRELOAD", "LD_LIBRARY_PATH", "PATH", "NODE_OPTIONS", "SHELLOPTS", "BASHOPTS", "CDPATH", "GLOBIGNORE", "LD_AUDIT", "PYTHONPATH"}

GLOB_CHARS = set("*?[]{}")
SHA_REF = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}$")


# Exact reviewed shell programs, hashed without normalization. New programs
# require policy review; command-name parsing alone is not a shell sandbox.
ALLOWED_RUN_SHA256 = {
    "45318d17a3b590330d1b6ffe6af1744648d6bce8ddd9788d29ca76e139578a52",  # Check the downloaded files against the expected list
    "d6d952a29a747d71d38c16dba3f7f503552467d19577b94017cec1d0bd88bf6a",  # Check assets against SHA256SUMS
    "cd605121c56b92ed6ebb2a7b3320693aa5cf34c53a0e32f6d74517d445f0b295",  # Same digest as the release build
    "a1dae5bedbae7993c29952c9cdd17087b9726097f9eed35b7add8a269353d83c",  # Keep the Sigstore bundle next to the manifest
    "ad4a545acdd2762662bf63b75ab47b4e32730b3e72440db47fdf48126900df94",  # pass_id_token_only_job.yml
}

def grants(perms):
    if perms == "write-all":
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
    problems = env_problems(doc.get("env"), "workflow")
    for key in grants(doc.get("permissions")):
        problems.append(f"{path}: workflow-level permissions grant {key}: write")
    for name, job in (doc.get("jobs") or {}).items():
        job = job or {}
        granted = grants(job.get("permissions"))
        if granted:
            defaults = (doc.get("defaults") or {}).get("run") or {}
            if defaults.get("shell") not in ALLOWED_SHELLS or "working-directory" in defaults:
                problems.append(f"{path}: workflow run defaults are not allowed for guarded jobs")
            problems += [f"{path}: job `{name}`: {p}" for p in guarded_job(job, granted)]
    return problems


def guarded_job(job, granted):
    problems = []
    if env_name(job.get("environment")) != "release":
        problems += [f"{key}: write without environment: release" for key in granted]
    for key in ("container", "services", "uses"):
        if key in job:
            problems.append(f"`{key}` is not allowed in a job that can mint OIDC tokens")
    runs_on = job.get("runs-on")
    labels = runs_on if isinstance(runs_on, list) else [runs_on]
    if len(labels) != 1 or labels[0] not in {"ubuntu-latest", "ubuntu-24.04", "ubuntu-22.04", "windows-2022", "macos-14"}:
        problems.append("must run on a GitHub-hosted runner")
    problems += env_problems(job.get("env"), "job")
    shell = ((job.get("defaults") or {}).get("run") or {}).get("shell")
    if shell not in ALLOWED_SHELLS:
        problems.append(f"default shell `{shell}` is not allowed")
    steps = job.get("steps") or []
    if not steps:
        problems.append("has no steps")
    for i, step in enumerate(steps):
        step = step or {}
        label = f"step {i + 1}" + (f" ({step['name']})" if step.get("name") else "")
        problems += [f"{label}: {p}" for p in step_problems(step)]
    return problems


def env_problems(env, where):
    if not isinstance(env, dict):
        return []
    return [f"{where} env sets {k}" for k in env if str(k).upper() in FORBIDDEN_ENV]


def step_problems(step):
    problems = env_problems(step.get("env"), "step")
    if "uses" in step and "run" in step:
        return problems + ["step has both `uses` and `run`"]
    if "uses" in step:
        return problems + uses_problems(str(step["uses"]), step.get("with") or {})
    if "run" in step:
        if step.get("shell") not in ALLOWED_SHELLS:
            problems.append(f"shell `{step.get('shell')}` is not allowed")
        if "working-directory" in step:
            problems.append("`working-directory` is not allowed")
        return problems + run_problems(str(step["run"]))
    return problems + ["step has neither `uses` nor `run`"]


def uses_problems(uses, args):
    if uses not in ALLOWED_ACTIONS:
        if not SHA_REF.match(uses):
            return [f"`{uses}` is not pinned to a full commit SHA"]
        return [f"`{uses}` is not an allowlisted action"]
    action = uses.split("@", 1)[0]
    problems = [
        f"`{action}` input `{k}` is not allowed"
        for k in args
        if k not in ALLOWED_INPUTS[action]
    ]
    if action == DOWNLOAD_ARTIFACT:
        artifact = str(args.get("name") or "")
        if not artifact or GLOB_CHARS & set(artifact) or "${{" in artifact:
            problems.append("downloads an artifact without an exact name")
    for key, value in args.items():
        if not isinstance(value, str) or "${{" in value:
            problems.append(f"`{action}` input `{key}` must be literal text")
    if action in (DOWNLOAD_ARTIFACT, UPLOAD_ARTIFACT):
        for key in ("name", "path"):
            value = str(args.get(key) or "")
            if key == "name" or action == UPLOAD_ARTIFACT or value:
                if not value or value.startswith(("/", "~", "-")) or ".." in value.split("/") or GLOB_CHARS & set(value) or "\\" in value:
                    problems.append(f"`{action}` input `{key}` must be an exact relative name/path")
    if action == UPLOAD_ARTIFACT and args.get("if-no-files-found") != "error":
        problems.append("upload must set if-no-files-found: error")
    if action == ATTEST:
        subjects = str(args.get("subject-path") or "")
        entries = [e.strip() for e in subjects.replace(",", "\n").splitlines() if e.strip()]
        if not entries or any(GLOB_CHARS & set(e) or "${{" in e or e.startswith(("/", "~", "-")) or ".." in e.split("/") or "\\" in e for e in entries):
            problems.append("attests subject-path with a wildcard or no path")
    return problems


def code_only(script):
    """Replace quoted literals with `_`, keeping command substitutions in "..."."""
    out = []
    i, n = 0, len(script)
    in_double = False
    depth_in_double = []  # paren depth stack for $( ) opened inside "..."
    depth = 0
    while i < n:
        c = script[i]
        if c == "\\" and i + 1 < n:
            out.append("  ")
            i += 2
            continue
        if not in_double:
            if c == "'":
                j = script.find("'", i + 1)
                j = n - 1 if j < 0 else j
                out.append("_" * (j - i + 1))
                i = j + 1
                continue
            if c == '"':
                in_double = True
                out.append("_")
                i += 1
                continue
            if c == "#" and (i == 0 or script[i - 1] in " \t\n;"):
                j = script.find("\n", i)
                j = n if j < 0 else j
                out.append(" " * (j - i))
                i = j
                continue
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth_in_double and depth_in_double[-1] == depth:
                    depth_in_double.pop()
                    in_double = True
                    out.append(")")
                    i += 1
                    continue
            out.append(c)
            i += 1
            continue
        # inside "..."
        if c == '"':
            in_double = False
            out.append("_")
        elif script.startswith("$(", i) and not script.startswith("$((", i):
            depth_in_double.append(depth)
            depth += 1
            in_double = False
            out.append("$(")
            i += 2
            continue
        elif c == "`":
            out.append(";")
        else:
            out.append("_")
        i += 1
    return "".join(out)


SEPARATORS = re.compile(r"\$\(|&&|\|\||[;|&\n()`]")
ASSIGN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")


def run_problems(script):
    problems = []
    if hashlib.sha256(script.encode()).hexdigest() not in ALLOWED_RUN_SHA256:
        problems.append("run script is not an explicitly allowlisted verification program")
    for expr in re.findall(r"\$\{\{(.*?)\}\}", script, re.S):
        if not EXPR_OK.match(expr.strip()):
            problems.append(f"run step interpolates `${{{{{expr.strip()}}}}}`")
    script = re.sub(r"\$\{\{.*?\}\}", "X", script, flags=re.S)
    for text in FORBIDDEN_TEXT:
        if text in script:
            problems.append(f"run step uses `{text}`")
    code = code_only(script)
    # Check each command substitution on its own, then treat it as a word.
    pieces = []
    inner = re.compile(r"(?:\$|[<>])\(([^()]*)\)")
    while True:
        m = inner.search(code)
        if not m:
            break
        pieces.append(m.group(1))
        code = code[: m.start()] + "X" + code[m.end():]
    pieces.append(code)
    for segment in (seg for piece in pieces for seg in SEPARATORS.split(piece)):
        words = segment.split()
        while words and (ASSIGN.match(words[0]) or words[0] in ("!", "{", "then", "do", "else", "elif", "if", "while")):
            if words[0] in ("if", "while", "!"):
                words = words[1:]
                break
            words = words[1:]
        if not words:
            continue
        cmd = words[0]
        if words and words[0] == "for":
            continue  # `for x in ...` lists words, not commands
        if cmd.startswith(">") or cmd.startswith("<") or cmd.startswith("2>"):
            continue
        if re.match(r"^[A-Za-z0-9_.-]+\)$", cmd) or cmd in (";;", "]", "]]"):
            continue
        if cmd not in ALLOWED_COMMANDS:
            problems.append(f"run step calls `{cmd}`, which is not an allowlisted command")
        for w in words:
            if w in FORBIDDEN_WORDS:
                problems.append(f"run step passes `{w}`")
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
