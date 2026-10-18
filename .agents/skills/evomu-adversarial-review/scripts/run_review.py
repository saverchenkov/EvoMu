#!/usr/bin/python3
"""EvoMu independent adversarial review orchestrator.

Collects evidence (git diff, honest test run, metrics, toolchain audit), then launches a
headless Jetski/Antigravity CLI reviewer (`agy -p ... --model opus-5.5-high --effort high`)
with a structured-output schema, validates that the reviewer did not touch the workspace,
and records the verdict in <repo>/.evomu-reviews/ (gitignored).

Exit codes: 0 PASS, 1 PASS_WITH_ISSUES, 2 FAIL, 3 INVALID / ERROR.
"""
import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import review_lib as rl  # noqa: E402

SKILL_DIR = os.path.dirname(HERE)
BRIEF = os.path.join(SKILL_DIR, "references", "reviewer_brief.md")
OPEN_FINDINGS = os.path.join(SKILL_DIR, "references", "open_findings.md")
HISTORY_DIR = os.path.join(SKILL_DIR, "references", "history")
SCHEMA = os.path.join(HERE, "verdict_schema.json")
WIKI = os.path.expanduser("~/ai/eve-wiki")
WRAPPER_ENV = ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTDOC", "CARGO_BUILD_RUSTDOC",
               "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR"]
EXIT = {"PASS": 0, "PASS_WITH_ISSUES": 1, "FAIL": 2}
CONTINUE_PROMPT = (
    "Continue the EvoMu review exactly where you left off. Do not restart and do not summarize progress. "
    "Keep using file-reading/search tools silently (no prose between tool calls) until every step of the "
    "reviewer brief is done, then return ONLY the final JSON object required by the schema."
)


def sh(cmd, cwd, env=None, timeout=None):
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True,
                           timeout=timeout)
        return p.returncode, p.stdout + (("\n[stderr]\n" + p.stderr) if p.stderr else "")
    except subprocess.TimeoutExpired as e:
        return 124, "TIMEOUT after %ss\n%s" % (timeout, (e.stdout or ""))
    except FileNotFoundError as e:
        return 127, "NOT FOUND: %s" % e


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


def pick_base(root, state, explicit):
    if explicit:
        return explicit
    for r in reversed(state.get("reviews", [])):
        head = r.get("head")
        if head and subprocess.run(["git", "merge-base", "--is-ancestor", head, "HEAD"],
                                   cwd=root).returncode == 0:
            return head
    return subprocess.run(["git", "rev-list", "--max-parents=0", "HEAD"], cwd=root,
                          capture_output=True, text=True).stdout.split()[0]


def env_audit(root):
    lines = []
    for p in [os.path.join(root, ".cargo", "config.toml"), os.path.join(root, ".cargo", "config"),
              os.path.expanduser("~/.cargo/config.toml"), os.path.expanduser("~/.cargo/config")]:
        if os.path.exists(p):
            lines.append("FOUND %s:\n%s" % (p, open(p).read()))
    for k in WRAPPER_ENV:
        if os.environ.get(k):
            lines.append("ENV %s=%s" % (k, os.environ[k]))
    rc, out = sh(["git", "grep", "-n", "-I", "-E",
                  r"ctypes\.CDLL|-Wl,-dylib|test result: ok\. 0 passed|rustc-wrapper|santa",
                  "--", ".", ":(exclude).agents"], root)
    lines.append("SUSPICIOUS PATTERN GREP (rc=%d):\n%s" % (rc, out.strip() or "(none)"))
    return "\n\n".join(lines) or "(no wrappers found)"


def run_tests(root, sdir, timeout):
    env = {k: v for k, v in os.environ.items() if k not in WRAPPER_ENV}
    env["CARGO_TARGET_DIR"] = os.path.join(sdir, "target")  # isolated from agent artifacts
    cargo = shutil.which("cargo", path=env.get("PATH")) or "/opt/homebrew/bin/cargo"
    rc, out = sh([cargo, "test", "--workspace", "--no-fail-fast"], root, env=env, timeout=timeout)
    santa = max(out.count("This application has been blocked from executing"),
                len(re.findall(r"signal: 9, SIGKILL", out)))  # Santa banner goes to the TTY
    passed = sum(int(m) for m in re.findall(r"test result: \w+\. (\d+) passed", out))
    failed = sum(int(m) for m in re.findall(r"(\d+) failed", out))
    header = ("ORCHESTRATOR SUMMARY: exit=%d santa_blocks=%d passed=%d failed=%d executed=%s\n"
              "command: CARGO_TARGET_DIR=%s %s test --workspace --no-fail-fast (wrappers scrubbed)\n\n"
              % (rc, santa, passed, failed, "yes" if (passed + failed) > 0 else "NO",
                 env["CARGO_TARGET_DIR"], cargo))
    return header + out, {"exit": rc, "santa_blocks": santa, "passed": passed, "failed": failed}


def py2_check(root):
    p = os.path.join(root, "client-bridge", "evomu_bridge.py")
    if not os.path.exists(p):
        return "(bridge missing)"
    if shutil.which("python2"):
        return sh(["python2", "-m", "py_compile", p], root)[1] or "python2 py_compile OK"
    raw = open(p, "rb").read()
    head = raw.split(b"\n", 2)[:2]
    cookie = any(re.search(rb"coding[:=]\s*([-\w.]+)", l) for l in head)
    non_ascii = [i + 1 for i, l in enumerate(raw.split(b"\n")) if any(b > 127 for b in l)]
    probs = []
    if non_ascii and not cookie:
        probs.append("non-ASCII bytes on lines %s without PEP 263 coding cookie -> SyntaxError on Py2"
                     % non_ascii[:10])
    src = raw.decode("utf-8", "replace")
    for pat, why in [(r'(?<![\w])f"', "f-string"), (r"(?<![\w])f'", "f-string"),
                     (r"\bnonlocal\b", "nonlocal"), (r"\byield from\b", "yield from"),
                     (r"daemon\s*=\s*True\s*\)", "Thread(daemon=) kwarg"),
                     (r"\bTimeoutError\b", "TimeoutError builtin")]:
        if re.search(pat, src):
            probs.append("Py3-only construct: " + why)
    return "python2 not installed; static check:\n" + ("\n".join(probs) or "no obvious Py3-only constructs")


def build_prompt(ctx_dir, has_claims):
    brief = open(BRIEF).read()
    return (
        brief
        + "\n\n---\n## This run\n"
        + "- Context directory (read these first): `%s`\n" % ctx_dir
        + "- Open findings to re-check: `%s`\n" % OPEN_FINDINGS
        + "- Previous review reports: `%s/`\n" % HISTORY_DIR
        + ("- Claims to verify: `%s/claims.md`\n" % ctx_dir if has_claims
           else "- No claims file was provided: review the diff and all open findings.\n")
        + "- Do NOT modify any file. Produce your final answer strictly as the JSON object required "
          "by the schema; put the full human-readable report in `report_markdown`.\n"
        + "- IMPORTANT (headless mode): any message that contains text without a tool call ends your session. "
          "Work silently: call tools back-to-back with no narration, and emit text only once, as the final JSON.\n"
    )


def render_report(meta, so):
    out = ["# EvoMu independent review: %s" % so.get("verdict", meta["verdict"]), "",
           "| Field | Value |", "|---|---|"]
    for k in ["verdict", "timestamp", "model", "effort", "head", "base", "tree", "tests_executed",
              "test_summary", "conversation_id"]:
        out.append("| %s | `%s` |" % (k, meta.get(k)))
    out += ["", "## Summary", so.get("summary", ""), "", "## Blocking findings",
            "| ID | Sev | Title | Evidence | Fix |", "|---|---|---|---|---|"]
    for f in so.get("blocking_findings", []):
        out.append("| %s | %s | %s | %s | %s |" % tuple(
            str(f.get(k, "")).replace("|", "\\|").replace("\n", " ")
            for k in ["id", "severity", "title", "evidence", "fix"]))
    out += ["", "## Claims assessment", "| Claim | Status | Evidence |", "|---|---|---|"]
    for c in so.get("claims_assessment", []):
        out.append("| %s | %s | %s |" % tuple(
            str(c.get(k, "")).replace("|", "\\|").replace("\n", " ")
            for k in ["claim", "status", "evidence"]))
    out += ["", "## Full report", "", so.get("report_markdown", "")]
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--claims-file", help="Markdown file with the coding agent's claims/summary")
    ap.add_argument("--base", help="Base ref for the diff (default: last reviewed HEAD, else root commit)")
    ap.add_argument("--model", default=os.environ.get("EVOMU_REVIEW_MODEL", "opus-5.5-high"))
    ap.add_argument("--effort", default=os.environ.get("EVOMU_REVIEW_EFFORT", "high"))
    ap.add_argument("--reviewer-timeout", default=os.environ.get("EVOMU_REVIEW_TIMEOUT", "0s"),
                    help="agy --print-timeout (0s = wait for completion)")
    ap.add_argument("--test-timeout", type=int, default=1800)
    ap.add_argument("--skip-tests", action="store_true", help="Do not run cargo test (recorded in report)")
    ap.add_argument("--launcher", choices=["agy", "jetski-chat"], default="agy")
    ap.add_argument("--dry-run", action="store_true", help="Collect context and print the reviewer command only")
    a = ap.parse_args()

    root = rl.repo_root(os.getcwd())
    sdir = rl.state_dir(root)
    state = rl.load_state(root)
    ts = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir = os.path.join(sdir, ts)
    ctx = os.path.join(run_dir, "context")
    os.makedirs(ctx, exist_ok=True)

    tree_before = rl.worktree_fingerprint(root)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True).stdout.strip()
    base = pick_base(root, state, a.base)
    print("[review] repo=%s head=%s base=%s tree=%s" % (root, head[:9], base[:9], tree_before[:12]))

    write(os.path.join(ctx, "git_log.txt"), sh(["git", "log", "--oneline", "-n", "30"], root)[1])
    write(os.path.join(ctx, "git_status.txt"), sh(["git", "status", "--short"], root)[1])
    write(os.path.join(ctx, "diffstat.txt"), sh(["git", "diff", "--stat", base, "--", ".", ":(exclude).agents"], root)[1])
    write(os.path.join(ctx, "diff.patch"), sh(["git", "diff", base, "--", ".", ":(exclude).agents", ":(exclude)docs/index/*.json"], root)[1])
    write(os.path.join(ctx, "env_audit.txt"), env_audit(root))

    test_meta = {"executed": False, "note": "skipped by --skip-tests"}
    if a.skip_tests:
        write(os.path.join(ctx, "tests.txt"), "ORCHESTRATOR SUMMARY: tests skipped by --skip-tests (NOT executed)\n")
    else:
        print("[review] running cargo test (wrappers scrubbed, isolated target dir)...")
        text, tm = run_tests(root, sdir, a.test_timeout)
        write(os.path.join(ctx, "tests.txt"), text)
        test_meta = dict(tm, executed=(tm["passed"] + tm["failed"]) > 0)
        print("[review] tests: %s" % test_meta)

    for name in ["coverage", "hollow", "stubs", "padding"]:
        write(os.path.join(ctx, "metrics_%s.txt" % name),
              sh([sys.executable, os.path.join(HERE, "metrics", name + ".py")], root)[1])
    vpm = os.path.join(root, "tools", "eve-indexer", "verify_plan_mapping.py")
    if os.path.exists(vpm):
        write(os.path.join(ctx, "verify_plan_mapping.txt"), sh([sys.executable, vpm, "--check"], root, timeout=300)[1])
    bridge = os.path.join(root, "client-bridge", "evomu_bridge.py")
    if os.path.exists(bridge):
        write(os.path.join(ctx, "bridge_selftest.txt"), sh([sys.executable, bridge, "--self-test"], root, timeout=120)[1])
    write(os.path.join(ctx, "py2_compile.txt"), py2_check(root))

    has_claims = bool(a.claims_file and os.path.exists(a.claims_file))
    if has_claims:
        shutil.copy(a.claims_file, os.path.join(ctx, "claims.md"))
    prompt = build_prompt(ctx, has_claims)
    write(os.path.join(run_dir, "prompt.md"), prompt)

    if a.launcher == "jetski-chat":
        cmd = ["jetski", "chat", "-m", "agent", "-a", BRIEF, "-a", OPEN_FINDINGS,
               "Act as the reviewer described in the attached brief. Context directory: %s. "
               "Select the model 'Claude Opus 5.5 (High)' before running. Do not modify files." % ctx]
        print("[review] jetski chat cannot pin a model or return structured output; launching interactive session:")
        print("  " + " ".join(cmd))
        if not a.dry_run:
            subprocess.Popen(cmd, cwd=root)
        print("[review] Interactive review launched; verdict will NOT be recorded automatically.")
        return 3

    cmd = ["agy", "-p", prompt, "--model", a.model, "--effort", a.effort,
           "--output-format", "json", "--json-schema", SCHEMA,
           "--print-timeout", a.reviewer_timeout, "--sandbox", "--add-dir", root]
    if os.path.isdir(WIKI):
        cmd += ["--add-dir", WIKI]
    if not os.path.abspath(SKILL_DIR).startswith(os.path.abspath(root) + os.sep):
        cmd += ["--add-dir", SKILL_DIR]  # reviewing a separate worktree: brief/history live elsewhere
    printable = [c if c != prompt else "<prompt: %s>" % os.path.join(run_dir, "prompt.md") for c in cmd]
    print("[review] reviewer command: " + " ".join(printable))
    if a.dry_run:
        print("[review] dry run: context in %s" % ctx)
        return 0

    print("[review] launching headless reviewer (this can take a while)...")
    max_rounds = int(os.environ.get("EVOMU_REVIEW_MAX_ROUNDS", "8"))
    base_flags = cmd[3:]  # everything after `agy -p <prompt>`
    verdict, so, conv, env, p = "ERROR", {}, "", {}, None
    for rnd in range(1, max_rounds + 1):
        if rnd == 1:
            run_cmd = cmd
        else:
            run_cmd = ["agy", "-p", CONTINUE_PROMPT, "--conversation", conv] + base_flags
            print("[review] round %d: resuming conversation %s (no final verdict yet)" % (rnd, conv))
        p = subprocess.run(run_cmd, cwd=root, capture_output=True, text=True)
        write(os.path.join(run_dir, "agy_stdout_r%d.json" % rnd), p.stdout)
        write(os.path.join(run_dir, "agy_stderr_r%d.txt" % rnd), p.stderr)
        try:
            env = json.loads(p.stdout.strip().splitlines()[-1]) if p.stdout.strip() else {}
        except Exception:  # noqa: BLE001
            env = {}
        conv = env.get("conversation_id") or conv
        if env.get("status") != "SUCCESS":
            so = {"summary": "Reviewer run failed: %s" % (env.get("error") or p.stderr[-2000:] or "no output"),
                  "report_markdown": "agy exit code %d. stderr tail:\n\n```\n%s\n```" % (p.returncode, p.stderr[-4000:])}
            verdict = "ERROR"
            break
        so = env.get("structured_output") or {}
        if not so.get("verdict") and env.get("response"):
            m = re.search(r"\{.*\}", env["response"], re.S)
            try:
                so = json.loads(m.group(0)) if m else {}
            except Exception:  # noqa: BLE001
                so = {}
        if so.get("verdict") in EXIT:
            verdict = so["verdict"]
            break
        if not conv:
            break
    if verdict == "ERROR" and not so.get("summary"):
        so = {"summary": "Reviewer did not return a verdict after %d rounds." % max_rounds,
              "report_markdown": (env.get("response") or "")[-4000:]}
    tree_after = rl.worktree_fingerprint(root)
    if tree_after != tree_before:
        verdict = "INVALID"
        so["summary"] = ("INVALID: the workspace changed during the review (tree %s -> %s). "
                         "Either the reviewer wrote files or someone edited concurrently. Re-run on a quiet tree. "
                         % (tree_before[:12], tree_after[:12])) + so.get("summary", "")

    meta = {"verdict": verdict, "timestamp": ts, "model": a.model, "effort": a.effort, "head": head,
            "base": base, "tree": tree_before, "tests_executed": test_meta.get("executed"),
            "test_summary": json.dumps(test_meta), "conversation_id": conv}
    report = render_report(meta, so)
    rpath = os.path.join(run_dir, "report.md")
    write(rpath, report)
    write(os.path.join(sdir, "latest.md"), report)
    write(os.path.join(run_dir, "result.json"), json.dumps({"meta": meta, "structured": so}, indent=2))
    state.setdefault("reviews", []).append(dict(meta, report=rpath))
    rl.save_state(root, state)

    print("[review] VERDICT: %s" % verdict)
    print("[review] report: %s" % rpath)
    if so.get("blocking_findings"):
        print("[review] blocking findings: %d" % len(so["blocking_findings"]))
    return EXIT.get(verdict, 3)


if __name__ == "__main__":
    sys.exit(main())
