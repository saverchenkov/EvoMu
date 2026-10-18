#!/usr/bin/python3
"""Shared helpers for the EvoMu adversarial review skill.

- Working-tree fingerprint = git tree hash of the full working tree (tracked + untracked,
  respecting .gitignore), computed through a temporary index so the real index is untouched.
  After `git add -A && git commit`, HEAD^{tree} equals this fingerprint.
- Review state lives in <repo>/.evomu-reviews/ (gitignored, so it never changes the fingerprint).
"""
import json
import os
import subprocess
import tempfile

STATE_DIRNAME = ".evomu-reviews"
# Written by the orchestrator after gap audits; excluded from the fingerprint so an audit does not
# itself invalidate the tree it just reviewed.
AUDITOR_ROWS_REL = ".agents/skills/evomu-adversarial-review/references/parity/auditor_rows.json"


def repo_root(start=None):
    start = start or os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=start,
                         capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit("not inside a git repository: %s" % start)
    return out.stdout.strip()


def worktree_fingerprint(root):
    with tempfile.TemporaryDirectory() as d:
        env = dict(os.environ, GIT_INDEX_FILE=os.path.join(d, "index"))
        if subprocess.run(["git", "rev-parse", "--verify", "-q", "HEAD"], cwd=root,
                          capture_output=True).returncode == 0:
            subprocess.run(["git", "read-tree", "HEAD"], cwd=root, env=env, check=True,
                           capture_output=True)
        subprocess.run(["git", "add", "-A", "--", ".", ":(exclude)%s" % STATE_DIRNAME, ":(exclude)%s" % AUDITOR_ROWS_REL], cwd=root,
                       env=env, check=True, capture_output=True)
        return subprocess.run(["git", "write-tree"], cwd=root, env=env, check=True,
                              capture_output=True, text=True).stdout.strip()


def head_tree(root):
    out = subprocess.run(["git", "rev-parse", "HEAD^{tree}"], cwd=root,
                         capture_output=True, text=True)
    return out.stdout.strip() if out.returncode == 0 else ""


def state_dir(root):
    p = os.path.join(root, STATE_DIRNAME)
    os.makedirs(p, exist_ok=True)
    return p


def load_state(root):
    p = os.path.join(state_dir(root), "state.json")
    if os.path.exists(p):
        with open(p) as f:
            return json.load(f)
    return {"reviews": []}


def save_state(root, state):
    p = os.path.join(state_dir(root), "state.json")
    tmp = p + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f, indent=2)
    os.replace(tmp, p)


def reviewed_trees(state, verdicts=None):
    return {r["tree"] for r in state.get("reviews", [])
            if verdicts is None or r.get("verdict") in verdicts}


if __name__ == "__main__":
    r = repo_root(os.getcwd())
    print(worktree_fingerprint(r))
