#!/usr/bin/python3
"""Antigravity `Stop` hook: forces the coding agent to run the evomu-adversarial-review skill
before it finishes a turn in which the EvoMu working tree changed.

Contract (see agy-customizations hooks docs): JSON on stdin, JSON on stdout.
  {"decision": "continue", "reason": "..."}  -> blocks the stop and re-enters the loop
  {"decision": "allow"}                      -> lets the agent stop

Rules:
- Background work still running (fullyIdle == false) -> allow (the agent will be woken later).
- No baseline yet -> record current tree as baseline and allow (first install is not a change).
- Current working-tree fingerprint already reviewed (any verdict) or equals baseline -> allow.
  (The agent must still report the verdict honestly; AGENTS.md requires that.)
- Otherwise -> continue with instructions to run the skill. Loop guard: after 3 forced
  continues for the same tree, allow and log, so the agent can never be trapped.
Disable with "enabled": false in .agents/hooks.json or EVOMU_REVIEW_HOOK=off in the agent env.
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import review_lib as rl  # noqa: E402

MAX_FORCED = 3


def emit(obj):
    sys.stdout.write(json.dumps(obj))
    sys.stdout.flush()
    sys.exit(0)


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:  # noqa: BLE001
        payload = {}
    if os.environ.get("EVOMU_REVIEW_HOOK", "").lower() == "off":
        emit({"decision": "allow"})
    if payload.get("fullyIdle") is False or payload.get("terminationReason") == "error":
        emit({"decision": "allow"})

    try:
        root = rl.repo_root(HERE)
        tree = rl.worktree_fingerprint(root)
    except BaseException:  # noqa: BLE001 - never wedge the agent on hook failure
        emit({"decision": "allow"})

    sdir = rl.state_dir(root)
    hook_state_path = os.path.join(sdir, "stop_hook.json")
    hs = {}
    if os.path.exists(hook_state_path):
        try:
            hs = json.load(open(hook_state_path))
        except Exception:  # noqa: BLE001
            hs = {}

    if not hs.get("baseline_tree"):
        # Baseline = last commit, so uncommitted work present at install time is still reviewed.
        hs["baseline_tree"] = rl.head_tree(root) or tree
        json.dump(hs, open(hook_state_path, "w"), indent=2)
        if hs["baseline_tree"] == tree:
            emit({"decision": "allow"})

    state = rl.load_state(root)
    if tree == hs.get("baseline_tree") or tree in rl.reviewed_trees(state):
        hs.pop("forced", None)
        json.dump(hs, open(hook_state_path, "w"), indent=2)
        emit({"decision": "allow"})

    forced = hs.get("forced", {})
    n = forced.get(tree, 0)
    if n >= MAX_FORCED:
        hs.setdefault("bypassed_trees", []).append(tree)
        json.dump(hs, open(hook_state_path, "w"), indent=2)
        emit({"decision": "allow"})
    forced = {tree: n + 1}
    hs["forced"] = forced
    json.dump(hs, open(hook_state_path, "w"), indent=2)

    skill = os.path.join(root, ".agents", "skills", "evomu-adversarial-review", "SKILL.md")
    emit({
        "decision": "continue",
        "reason": (
            "EvoMu review gate: the working tree changed (fingerprint %s) and has not been "
            "independently reviewed. Before finishing, activate the `evomu-adversarial-review` skill "
            "(%s): write your claims to a temp file, run "
            "`/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py --claims-file <file>`, "
            "wait for the verdict, and report it verbatim to the user together with the report path. "
            "Do not claim anything is verified unless the verdict says so. (forced %d/%d)"
            % (tree[:12], skill, n + 1, MAX_FORCED)
        ),
    })


if __name__ == "__main__":
    main()
