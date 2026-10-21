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


GATE_PATHS = (".agents", "AGENTS.md")  # supervisor-owned; excluded from the code fingerprint


def code_tree_of(root, tree, _cache={}):
    """Tree hash with the supervisor-owned gate paths removed. Two working trees with the same code tree differ
    only in .agents/ and AGENTS.md, so a review of one covers the other once the gate files match `main`."""
    if not tree:
        return ""
    if tree in _cache:
        return _cache[tree]
    cache_file = os.path.join(main_state_dir(root), "code_tree_cache.json")
    disk = {}
    if os.path.exists(cache_file):
        try:
            disk = json.load(open(cache_file))
        except Exception:  # noqa: BLE001
            disk = {}
    if tree in disk:
        _cache[tree] = disk[tree]
        return disk[tree]
    with tempfile.TemporaryDirectory() as d:
        env = dict(os.environ, GIT_INDEX_FILE=os.path.join(d, "index"))
        if subprocess.run(["git", "read-tree", tree], cwd=root, env=env, capture_output=True).returncode:
            return ""
        subprocess.run(["git", "rm", "-r", "-q", "--cached", "--ignore-unmatch", "--"] + list(GATE_PATHS),
                       cwd=root, env=env, capture_output=True)
        out = subprocess.run(["git", "write-tree"], cwd=root, env=env, capture_output=True, text=True).stdout.strip()
    _cache[tree] = out
    disk[tree] = out
    try:
        tmp = cache_file + ".tmp"
        json.dump(disk, open(tmp, "w"))
        os.replace(tmp, cache_file)
    except OSError:
        pass
    return out


def main_state_dir(root):
    common = subprocess.run(["git", "rev-parse", "--git-common-dir"], cwd=root, capture_output=True,
                            text=True).stdout.strip()
    main_root = os.path.dirname(os.path.abspath(os.path.join(root, common))) if common else root
    p = os.path.join(main_root, STATE_DIRNAME)
    os.makedirs(p, exist_ok=True)
    return p


def worktree_roots(root):
    out = subprocess.run(["git", "worktree", "list", "--porcelain"], cwd=root, capture_output=True, text=True).stdout
    return [ln[len("worktree "):] for ln in out.splitlines() if ln.startswith("worktree ")]


def gate_matches_main(root):
    if subprocess.run(["git", "rev-parse", "--verify", "-q", "main"], cwd=root, capture_output=True).returncode:
        return False
    return subprocess.run(["git", "diff", "--quiet", "main", "--"] + list(GATE_PATHS) +
                          [":(exclude)%s" % AUDITOR_ROWS_REL], cwd=root, capture_output=True).returncode == 0


def reviewed_by_code(root, verdicts=("PASS", "PASS_WITH_ISSUES"), mode="claims", models=None, tree=None):
    """Latest review (in ANY worktree of this repo) whose reviewed tree has the same code tree as this working
    tree, provided this tree's gate files equal `main`. Returns the review dict (plus 'worktree') or None."""
    if not gate_matches_main(root):
        return None
    mine = code_tree_of(root, tree or worktree_fingerprint(root))
    if not mine:
        return None
    best = None
    for wt in worktree_roots(root):
        p = os.path.join(wt, STATE_DIRNAME, "state.json")
        if not os.path.exists(p):
            continue
        try:
            st = json.load(open(p))
        except Exception:  # noqa: BLE001
            continue
        for r in st.get("reviews", []):
            if r.get("mode", "claims") != mode or r.get("verdict") not in verdicts:
                continue
            if models and r.get("model") not in models:
                continue
            if code_tree_of(root, r.get("tree")) == mine and (best is None or r.get("timestamp", "") > best.get("timestamp", "")):
                best = dict(r, worktree=wt)
    return best


def stale_gate(root):
    """True (with a message) when this worktree's .agents/ differs from the `main` branch's copy, i.e. a track
    worktree is running an outdated review gate. auditor_rows.json is exempt (written by the orchestrator)."""
    if subprocess.run(["git", "rev-parse", "--verify", "-q", "main"], cwd=root, capture_output=True).returncode:
        return None
    r = subprocess.run(["git", "diff", "--quiet", "main", "--", ".agents",
                        ":(exclude).agents/skills/evomu-adversarial-review/references/parity/auditor_rows.json"],
                       cwd=root, capture_output=True)
    if r.returncode == 0:
        return None
    return ("STALE REVIEW GATE: this worktree's .agents/ differs from the `main` branch. Rebase or merge `main` "
            "into this branch (git stash; git rebase main; git stash pop) so you run the current pre-flight and "
            "review rules, then retry. Never edit .agents/ yourself.")


if __name__ == "__main__":
    r = repo_root(os.getcwd())
    print(worktree_fingerprint(r))
