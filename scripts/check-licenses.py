#!/usr/bin/env python3
"""Fail if a crate in the given workspace's dependency graph is copyleft-only.

The app ships under MIT, and GPUI lives next to Zed's GPL-3.0 crates (`ui`,
`theme`, ...) in the same git repository, so one wrong `use` would pull GPL code
in (ADR-120002). A license expression passes when it has a non-GPL alternative
("MIT OR GPL-3.0" is fine) or no GPL at all.

Usage: check-licenses.py <path to Cargo.toml>
"""
import json
import subprocess
import sys


def copyleft_only(expr: str) -> bool:
    alternatives = [a.strip() for a in expr.replace("/", " OR ").split(" OR ")]
    return all("GPL" in a for a in alternatives)


def main() -> int:
    manifest = sys.argv[1]
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--manifest-path", manifest],
        check=True, capture_output=True, text=True,
    ).stdout
    meta = json.loads(out)
    used = {n["id"] for n in meta["resolve"]["nodes"]}
    workspace = set(meta["workspace_members"])
    bad = [
        f'{p["name"]} {p["version"]}: {p.get("license") or "no license field"}'
        for p in meta["packages"]
        if p["id"] in used and p["id"] not in workspace
        and (not p.get("license") or copyleft_only(p["license"]))
    ]
    for line in sorted(bad):
        print(line)
    return 1 if bad else 0


if __name__ == "__main__":
    assert not copyleft_only("MIT OR GPL-3.0-or-later")
    assert copyleft_only("GPL-3.0-or-later")
    assert not copyleft_only("Apache-2.0 WITH LLVM-exception")
    sys.exit(main())
