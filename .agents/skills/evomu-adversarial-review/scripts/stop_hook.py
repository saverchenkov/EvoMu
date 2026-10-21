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
    if os.environ.get("EVOMU_REVIEW_HOOK", "").lower() == "off" or os.environ.get("EVOMU_REVIEWER") == "1":
        emit({"decision": "allow"})
    if payload.get("fullyIdle") is False or payload.get("terminationReason") == "error":
        emit({"decision": "allow"})

    try:
        root = rl.repo_root(HERE)
        tree = rl.worktree_fingerprint(root)
    except BaseException:  # noqa: BLE001 - never wedge the agent on hook failure
        emit({"decision": "allow"})

    sdir = rl.state_dir(root)
    # A review is running (this Stop most likely comes from the headless reviewer session, which
    # loads the same workspace hooks). Allow it and don't count toward the loop guard.
    lock_path = os.path.join(sdir, "review_in_progress.json")
    if os.path.exists(lock_path):
        try:
            pid = int(json.load(open(lock_path)).get("pid", 0))
            if pid <= 0:
                raise ValueError("bad pid")
            try:
                os.kill(pid, 0)
            except PermissionError:
                pass  # process exists but is owned by another user: still a live review
            emit({"decision": "allow"})
        except (OSError, ValueError, TypeError):
            pass  # stale lock: orchestrator died
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
        json.dump(hs, open(hook_state_path, "w", encoding="utf-8"), indent=2)

    state = rl.load_state(root)
    skill = os.path.join(root, ".agents", "skills", "evomu-adversarial-review", "SKILL.md")
    reviewed = tree == hs.get("baseline_tree") or tree in rl.reviewed_trees(state) or \
        rl.reviewed_by_code(root, tree=tree) is not None

    if reviewed:
        # Review gate satisfied. Now the goal gate: the project is only done when final_gate.py passed
        # on this exact tree. Until then, push the agent to keep closing ledger rows.
        gate = {}
        gp = os.path.join(sdir, "final_gate.json")
        if os.path.exists(gp):
            try:
                gate = json.load(open(gp))
            except Exception:  # noqa: BLE001
                gate = {}
        if gate.get("passed") and gate.get("tree") == tree:
            hs.pop("forced", None)
            json.dump(hs, open(hook_state_path, "w", encoding="utf-8"), indent=2)
            emit({"decision": "allow"})
        key = "goal:" + tree
        forced = hs.get("forced", {})
        n = forced.get(key, 0)
        if n >= MAX_FORCED:
            emit({"decision": "allow"})
        hs["forced"] = {key: n + 1}
        json.dump(hs, open(hook_state_path, "w", encoding="utf-8"), indent=2)
        pct = None
        for r in reversed(state.get("reviews", [])):
            if r.get("parity_percent") is not None:
                pct = r.get("parity_percent")
                break
        emit({
            "decision": "continue",
            "reason": (
                "EvoMu goal gate: the project is NOT complete. Last measured parity: %s%% of ledger rows "
                "closed. The user will only review when `final_gate.py` passes (100%% ledger, PASS review and "
                "CLEAN gap audit on the same tree, coverage and mutation thresholds, real-client journeys). "
                "Do not stop and do not ask the user for interim review. Pick the next open ledger rows from "
                "`.evomu-reviews/latest.md` (Parity section) in canonical plan order, implement them fully with "
                "tagged tests (`// parity: <row id>`), run the review skill (%s), commit, and continue. When you "
                "believe everything is done, run `/usr/bin/python3 "
                ".agents/skills/evomu-adversarial-review/scripts/final_gate.py` and fix whatever it reports. "
                "(nudge %d/%d for this tree)" % (pct, skill, n + 1, MAX_FORCED)
            ),
        })

    forced = hs.get("forced", {})
    n = forced.get(tree, 0)
    if n >= MAX_FORCED:
        hs.setdefault("bypassed_trees", []).append(tree)
        json.dump(hs, open(hook_state_path, "w", encoding="utf-8"), indent=2)
        emit({"decision": "allow"})
    forced = {tree: n + 1}
    hs["forced"] = forced
    json.dump(hs, open(hook_state_path, "w", encoding="utf-8"), indent=2)

    emit({
        "decision": "continue",
        "reason": (
            "EvoMu review gate: the working tree changed (fingerprint %s) and has not been "
            "independently reviewed. Before finishing, activate the `evomu-adversarial-review` skill "
            "(%s): write your claims to a temp file, run "
            "`/usr/bin/python3 .agents/skills/evomu-adversarial-review/scripts/run_review.py --claims-file <file>`, "
            "wait for the verdict, and act on it. "
            "Do not claim anything is verified unless the verdict says so. (forced %d/%d)"
            % (tree[:12], skill, n + 1, MAX_FORCED)
        ),
    })


if __name__ == "__main__":
    main()
