#!/usr/bin/env python3
"""Run `cargo test` only for the workspace crates a PR actually affects.

A PR that touches one crate should not pay for testing all 16 first-party
crates. This maps the PR's changed files to their owning workspace member(s),
adds every member that (transitively) depends on them — so API/re-export
breakage in a dependent is still caught — and runs `cargo test -p ...` for just
that set.

Falls back to the full workspace test when a cross-cutting file changes
(root Cargo.toml / Cargo.lock / rust-toolchain / the CI workflow / this script),
since those can affect every crate. Skips testing entirely when nothing
testable changed (e.g. docs only).

Env:
  BASE_SHA, HEAD_SHA   the PR's base and head commits (CI passes these);
                       defaults to origin/master..HEAD for local runs.
  CI_TEST_DRY_RUN=1    print the decision + command, don't run cargo.
"""
import json
import os
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CROSS_CUTTING_EXACT = {"Cargo.toml", "Cargo.lock"}
CROSS_CUTTING_PREFIX = (".github/", "scripts/")
CROSS_CUTTING_GLOB = ("rust-toolchain",)  # rust-toolchain / rust-toolchain.toml


def run(cmd, **kw):
    return subprocess.run(cmd, cwd=REPO, text=True, capture_output=True, **kw)


def changed_files(base, head):
    # three-dot: changes the PR introduces relative to the merge-base.
    res = run(["git", "diff", "--name-only", f"{base}...{head}"])
    if res.returncode != 0:
        # fall back to two-dot if merge-base is unavailable
        res = run(["git", "diff", "--name-only", f"{base}", f"{head}"])
    if res.returncode != 0:
        sys.exit(f"git diff failed:\n{res.stderr}")
    return [f for f in res.stdout.splitlines() if f.strip()]


def workspace_members():
    """name -> relative crate dir, plus the intra-workspace dependency graph."""
    res = run(["cargo", "metadata", "--no-deps", "--format-version", "1"])
    if res.returncode != 0:
        sys.exit(f"cargo metadata failed:\n{res.stderr}")
    meta = json.loads(res.stdout)
    names = {p["name"] for p in meta["packages"]}
    dirs = {}
    deps = defaultdict(set)  # crate -> set(workspace deps)
    for p in meta["packages"]:
        rel = str(Path(p["manifest_path"]).resolve().parent.relative_to(REPO))
        dirs[p["name"]] = rel
        for d in p.get("dependencies", []):
            if d["name"] in names:
                deps[p["name"]].add(d["name"])
    return names, dirs, deps


def is_cross_cutting(f):
    if f in CROSS_CUTTING_EXACT:
        return True
    if any(f.startswith(p) for p in CROSS_CUTTING_PREFIX):
        return True
    if any(Path(f).name.startswith(g) for g in CROSS_CUTTING_GLOB):
        return True
    return False


def owning_crate(f, dirs):
    # longest matching crate dir prefix wins (handles nested paths)
    best = None
    for name, d in dirs.items():
        if f == d or f.startswith(d + "/"):
            if best is None or len(dirs[name]) > len(dirs[best]):
                best = name
    return best


def dependents_closure(seeds, deps):
    # invert dep graph: dep -> set(crates that depend on it)
    rev = defaultdict(set)
    for crate, ds in deps.items():
        for d in ds:
            rev[d].add(crate)
    affected = set(seeds)
    stack = list(seeds)
    while stack:
        cur = stack.pop()
        for dependent in rev.get(cur, ()):
            if dependent not in affected:
                affected.add(dependent)
                stack.append(dependent)
    return affected


def main():
    base = os.environ.get("BASE_SHA", "origin/master")
    head = os.environ.get("HEAD_SHA", "HEAD")
    files = changed_files(base, head)
    if not files:
        print("No changed files detected; skipping tests.")
        return

    names, dirs, deps = workspace_members()

    cross = [f for f in files if is_cross_cutting(f)]
    if cross:
        print(f"Cross-cutting change(s) detected ({', '.join(sorted(set(cross))[:5])}"
              f"{' ...' if len(set(cross)) > 5 else ''}); testing the full workspace.")
        cmd = ["cargo", "test", "--workspace", "--locked", "--jobs", "8"]
        print("+", " ".join(cmd))
        if os.environ.get("CI_TEST_DRY_RUN"):
            return
        sys.exit(subprocess.run(cmd, cwd=REPO).returncode)

    seeds = set()
    for f in files:
        c = owning_crate(f, dirs)
        if c:
            seeds.add(c)
    if not seeds:
        print("No first-party crate affected (docs/vendored only); skipping tests.")
        return

    affected = sorted(dependents_closure(seeds, deps))
    print(f"Directly changed crates: {sorted(seeds)}")
    print(f"+ dependents -> testing:  {affected}")
    cmd = ["cargo", "test", "--locked", "--jobs", "8"]
    for c in affected:
        cmd += ["-p", c]
    print("+", " ".join(cmd))
    if os.environ.get("CI_TEST_DRY_RUN"):
        return
    sys.exit(subprocess.run(cmd, cwd=REPO).returncode)


if __name__ == "__main__":
    main()
