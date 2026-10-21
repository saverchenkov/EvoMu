#!/usr/bin/python3
"""EvoMu pre-push gate: refuse to push a `main` whose code was never reviewed.

Installed as <git common dir>/hooks/pre-push (shared by every worktree). For each ref being pushed to
refs/heads/main, the pushed commit's tree must have the same *code tree* (tree minus the supervisor-owned
`.agents/` and `AGENTS.md`) as a tree that got a non-FAIL claims review recorded in any EvoMu worktree's
.evomu-reviews/state.json. Gate-only commits on top of reviewed code therefore pass; unreviewed code does not.
Bypassing this hook (`--no-verify`, deleting it, pushing via another remote alias) is gate tampering (AGENTS.md).
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import review_lib as rl  # noqa: E402

ZERO = "0" * 40


def main():
    root = rl.repo_root(os.getcwd())
    bad = []
    for line in sys.stdin.read().splitlines():
        parts = line.split()
        if len(parts) != 4:
            continue
        local_ref, local_sha, remote_ref, _remote_sha = parts
        if remote_ref != "refs/heads/main" or local_sha == ZERO:
            continue
        tree = subprocess.run(["git", "rev-parse", local_sha + "^{tree}"], cwd=root, capture_output=True,
                              text=True).stdout.strip()
        r = rl.review_covering_tree(root, tree)
        if r:
            print("[pre-push gate] main @ %s: code covered by review %s (%s, %s)" % (
                local_sha[:9], r.get("timestamp"), r.get("verdict"), r.get("model")))
        else:
            bad.append(local_sha)
    if bad:
        print("[pre-push gate] REFUSED: main @ %s contains code with no non-FAIL claims review of the same code tree "
              "in any EvoMu worktree. Run preflight + run_review.py on that exact tree first (AGENTS.md: never push or "
              "merge an unreviewed tree). Do not bypass this hook." % ", ".join(b[:9] for b in bad), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
