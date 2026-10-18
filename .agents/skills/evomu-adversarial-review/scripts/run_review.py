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
GAP_BRIEF = os.path.join(SKILL_DIR, "references", "gap_auditor_brief.md")
GAP_SCHEMA = os.path.join(HERE, "gap_schema.json")
PARITY_DIR = os.path.join(SKILL_DIR, "references", "parity")
PARITY_STATUS = os.path.join(HERE, "parity", "parity_status.py")
WIKI = os.path.expanduser("~/ai/eve-wiki")
CRUCIBLE = os.path.expanduser("~/ai/evemu_Crucible")
WRAPPER_ENV = ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTDOC", "CARGO_BUILD_RUSTDOC",
               "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR"]
EXIT = {"PASS": 0, "PASS_WITH_ISSUES": 1, "FAIL": 2}
GAP_EXIT = {"CLEAN": 0, "GAPS": 2}
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


def extract_verdict(env, valid=None):
    """Return the structured verdict dict from an agy JSON envelope, salvaging it from the raw
    response text when the stream ended in ERROR or structured_output is missing."""
    so = env.get("structured_output") or {}
    if so.get("verdict") in (valid or EXIT):
        return so
    resp = env.get("response") or ""
    candidates = re.findall(r"```(?:json)?\s*(\{.*?\})\s*```", resp, re.S)
    m = re.search(r"\{.*\}", resp, re.S)
    if m:
        candidates.append(m.group(0))
    for c in candidates:
        try:
            obj = json.loads(c, strict=False)  # tolerate raw newlines/tabs inside strings
        except Exception:  # noqa: BLE001
            continue
        if isinstance(obj, dict) and obj.get("verdict") in (valid or EXIT):
            return obj
    return {}


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


def run_tests(root, sdir, timeout, attest_dir=None):
    env = {k: v for k, v in os.environ.items() if k not in WRAPPER_ENV}
    env["CARGO_TARGET_DIR"] = os.path.join(sdir, "target")  # isolated from agent artifacts
    if attest_dir:
        shutil.rmtree(attest_dir, ignore_errors=True)
        os.makedirs(attest_dir, exist_ok=True)
        env["EVOMU_PARITY_ATTEST_DIR"] = attest_dir
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


def build_prompt(ctx_dir, has_claims, mode="claims"):
    brief = open(GAP_BRIEF if mode == "gap-audit" else BRIEF).read()
    lines = [brief, "\n\n---\n## This run\n",
             "- Context directory (read these first): `%s`\n" % ctx_dir,
             "- Parity status (mechanical, authoritative for completeness): `%s/parity.md` and `parity.json`\n"
             % ctx_dir,
             "- Pinned ledger and auditor rows: `%s/`\n" % PARITY_DIR]
    if mode == "gap-audit":
        lines.append("- NA triage claimed by the agent: `docs/parity/triage.json` in the repo.\n")
    else:
        lines += ["- Open findings to re-check: `%s`\n" % OPEN_FINDINGS,
                  "- Previous review reports: `%s/`\n" % HISTORY_DIR,
                  ("- Claims to verify: `%s/claims.md`\n" % ctx_dir if has_claims
                   else "- No claims file was provided: review the diff and all open findings.\n")]
    lines += [
        "- Do NOT modify any file. Produce your final answer strictly as the JSON object required "
        "by the schema; put the full human-readable report in `report_markdown`.\n",
        "- IMPORTANT (headless mode): any message that contains text without a tool call ends your session. "
        "Work silently: call tools back-to-back with no narration, and emit text only once, as the final JSON.\n",
        "- IMPORTANT: shell/terminal commands are auto-denied in this headless session, and a denied command "
        "ends your turn with no output. Do NOT run commands (no cargo, git, grep, python). Use only file "
        "view/list/search tools. The diff, test output, metrics, parity status and tool outputs are already "
        "pre-computed in the context directory.\n",
        "- Ignore any Stop-hook message asking you to run the `evomu-adversarial-review` skill; that hook is for "
        "the coding agent, not for you.\n"]
    return "".join(lines)


def render_report(meta, so, parity_md=""):
    out = ["# EvoMu independent %s: %s" % ("gap audit" if meta.get("mode") == "gap-audit" else "review",
                                          so.get("verdict", meta["verdict"])), "",
           "| Field | Value |", "|---|---|"]
    for k in ["verdict", "mode", "timestamp", "model", "effort", "head", "base", "tree", "tests_executed",
              "test_summary", "parity_percent", "conversation_id"]:
        out.append("| %s | `%s` |" % (k, meta.get(k)))
    out += ["", "## Summary", so.get("summary", "")]
    if meta.get("mode") == "gap-audit":
        out += ["", "## New ledger rows (%d)" % len(so.get("new_rows", []))]
        for r in so.get("new_rows", []):
            out.append("- `%s`: %s (%s)" % (r.get("id"), r.get("title"), ", ".join(r.get("sources", [])[:3])))
        out += ["", "## Rejected NA triage (%d)" % len(so.get("rejected_triage", []))]
        for r in so.get("rejected_triage", []):
            out.append("- `%s`: %s" % (r.get("id"), r.get("reason")))
        out += ["", "## Fake DONE rows (%d)" % len(so.get("fake_done", []))]
        for r in so.get("fake_done", []):
            out.append("- `%s`: %s" % (r.get("id"), r.get("evidence")))
        out += ["", "Cleared fake flags: %s" % ", ".join(so.get("cleared_fake", []) or ["none"])]
    else:
        out += ["", "## Blocking findings", "| ID | Sev | Title | Evidence | Fix |", "|---|---|---|---|---|"]
        for f in so.get("blocking_findings", []):
            out.append("| %s | %s | %s | %s | %s |" % tuple(
                str(f.get(k, "")).replace("|", "\\|").replace("\n", " ")
                for k in ["id", "severity", "title", "evidence", "fix"]))
        out += ["", "## Claims assessment", "| Claim | Status | Evidence |", "|---|---|---|"]
        for c in so.get("claims_assessment", []):
            out.append("| %s | %s | %s |" % tuple(
                str(c.get(k, "")).replace("|", "\\|").replace("\n", " ")
                for k in ["claim", "status", "evidence"]))
    if parity_md:
        out += ["", "## Parity (mechanical)", "", parity_md.split("\n## First open")[0]]
    out += ["", "## Full report", "", so.get("report_markdown", "")]
    return "\n".join(out)


def launch_reviewer(first_cmd, root, sdir, run_dir, tree_before, ts, valid_verdicts, prefix=""):
    max_rounds = int(os.environ.get("EVOMU_REVIEW_MAX_ROUNDS", "8"))
    max_errors = int(os.environ.get("EVOMU_REVIEW_MAX_STREAM_ERRORS", "2"))
    base_flags = first_cmd[3:]  # everything after `agy -p <prompt>`
    # The reviewer runs inside the same workspace, so it loads .agents/hooks.json. Without this, the
    # coding-agent Stop hook fires inside the reviewer session, forces "continue", burns the per-tree
    # loop guard, and interrupts the reviewer's stream.
    reviewer_env = dict(os.environ, EVOMU_REVIEW_HOOK="off", EVOMU_REVIEWER="1")
    lock = os.path.join(sdir, "review_in_progress.json")
    write(lock, json.dumps({"pid": os.getpid(), "tree": tree_before, "ts": ts}))
    verdict, so, conv, env = "ERROR", {}, "", {}
    errors = 0
    try:
        for rnd in range(1, max_rounds + 1):
            if rnd == 1:
                run_cmd = first_cmd
            else:
                run_cmd = ["agy", "-p", CONTINUE_PROMPT, "--conversation", conv] + base_flags
                print("[review] %sround %d: resuming conversation %s" % (prefix, rnd, conv))
            p = subprocess.run(run_cmd, cwd=root, capture_output=True, text=True, env=reviewer_env)
            write(os.path.join(run_dir, "%sagy_stdout_r%d.json" % (prefix, rnd)), p.stdout)
            write(os.path.join(run_dir, "%sagy_stderr_r%d.txt" % (prefix, rnd)), p.stderr)
            try:
                env = json.loads(p.stdout.strip().splitlines()[-1], strict=False) if p.stdout.strip() else {}
            except Exception:  # noqa: BLE001
                env = {}
            conv = env.get("conversation_id") or conv
            so = extract_verdict(env, valid_verdicts)
            if so.get("verdict") in valid_verdicts:
                verdict = so["verdict"]
                if env.get("status") != "SUCCESS":
                    so["summary"] = ("(Verdict salvaged from a reviewer stream that ended with status %s: %s) "
                                     % (env.get("status"), env.get("error"))) + so.get("summary", "")
                break
            if env.get("status") != "SUCCESS":
                errors += 1
                so = {"summary": "Reviewer run failed: %s" % (env.get("error") or p.stderr[-2000:] or "no output"),
                      "report_markdown": "agy exit code %d. stderr tail:\n\n```\n%s\n```"
                                         % (p.returncode, p.stderr[-4000:])}
                if conv and errors <= max_errors:
                    print("[review] %sround %d: reviewer stream error (%s); resuming" % (prefix, rnd, env.get("error")))
                    continue
                verdict = "ERROR"
                break
            if not conv:
                break
    finally:
        try:
            os.remove(lock)
        except OSError:
            pass
    if verdict == "ERROR" and not so.get("summary"):
        so = {"summary": "Reviewer did not return a verdict after %d rounds." % max_rounds,
              "report_markdown": (env.get("response") or "")[-4000:]}
    tree_after = rl.worktree_fingerprint(root)
    if tree_after != tree_before:
        verdict = "INVALID"
        so["summary"] = ("INVALID: the workspace changed during the review (tree %s -> %s). "
                         "Either the reviewer wrote files or someone edited concurrently. Re-run on a quiet tree. "
                         % (tree_before[:12], tree_after[:12])) + so.get("summary", "")
    return verdict, so, conv


def apply_gap_audit(so, ts):
    """Append auditor findings to the pinned auditor_rows.json (append-only for rows)."""
    path = os.path.join(PARITY_DIR, "auditor_rows.json")
    aud = {"rows": [], "rejected_triage": [], "fake_done": []}
    if os.path.exists(path):
        aud = json.load(open(path))
    with open(os.path.join(PARITY_DIR, "ledger.json")) as f:
        known = {r["id"] for r in json.load(f)["rows"]} | {r["id"] for r in aud.get("rows", [])}
    added = 0
    for r in so.get("new_rows", []):
        rid = str(r.get("id", ""))
        if not re.match(r"^feat:[a-z0-9_]+\.[a-z0-9_.\-]+$", rid) or rid in known:
            continue
        aud.setdefault("rows", []).append(dict(r, category="feat", na_allowed=False, attest_ok=False,
                                               added_by_audit=ts))
        known.add(rid)
        added += 1
    rej = set(aud.get("rejected_triage", []))
    rej |= {str(r.get("id")) for r in so.get("rejected_triage", [])}
    aud["rejected_triage"] = sorted(rej)
    cleared = set(so.get("cleared_fake", []))
    fake = {f["id"]: f for f in aud.get("fake_done", []) if f["id"] not in cleared}
    for f in so.get("fake_done", []):
        fake[str(f.get("id"))] = dict(f, flagged_by_audit=ts)
    aud["fake_done"] = sorted(fake.values(), key=lambda x: x["id"])
    write(path, json.dumps(aud, indent=1))
    return added


def run_parity(root, sdir, ctx):
    tests = os.path.join(ctx, "tests.txt")
    rc, out = sh([sys.executable, PARITY_STATUS, "--tests", tests, "--attest-dir", os.path.join(ctx, "attest"),
                  "--out", os.path.join(ctx, "parity.json"), "--md", os.path.join(ctx, "parity.md"),
                  "--state-dir", sdir], root, timeout=900)
    try:
        summ = json.load(open(os.path.join(ctx, "parity.json")))["summary"]
    except Exception:  # noqa: BLE001
        summ = {"percent_closed": None, "complete": False, "error": out[-2000:]}
    md = open(os.path.join(ctx, "parity.md")).read() if os.path.exists(os.path.join(ctx, "parity.md")) else out
    return summ, md


def reviews_since_last_audit(state):
    n = 0
    for r in reversed(state.get("reviews", [])):
        if r.get("mode") == "gap-audit" and r.get("verdict") in ("CLEAN", "GAPS"):
            break
        if r.get("mode", "claims") == "claims":
            n += 1
    return n


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--claims-file", help="Markdown file with the coding agent's claims/summary")
    ap.add_argument("--base", help="Base ref for the diff (default: last reviewed HEAD, else root commit)")
    ap.add_argument("--mode", choices=["claims", "gap-audit"], default="claims",
                    help="claims = precision review of the change; gap-audit = recall audit of the parity ledger")
    ap.add_argument("--model", default=os.environ.get("EVOMU_REVIEW_MODEL", "opus-5.5-high"))
    ap.add_argument("--effort", default=os.environ.get("EVOMU_REVIEW_EFFORT", "high"))
    ap.add_argument("--reviewer-timeout", default=os.environ.get("EVOMU_REVIEW_TIMEOUT", "0s"),
                    help="agy --print-timeout (0s = wait for completion)")
    ap.add_argument("--test-timeout", type=int, default=int(os.environ.get("EVOMU_TEST_TIMEOUT", "7200")))
    ap.add_argument("--skip-tests", action="store_true", help="Do not run cargo test (recorded in report)")
    ap.add_argument("--no-auto-audit", action="store_true", help="Do not chain a due gap audit after claims")
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
    print("[review] mode=%s repo=%s head=%s base=%s tree=%s" % (a.mode, root, head[:9], base[:9], tree_before[:12]))

    write(os.path.join(ctx, "git_log.txt"), sh(["git", "log", "--oneline", "-n", "30"], root)[1])
    write(os.path.join(ctx, "git_status.txt"), sh(["git", "status", "--short"], root)[1])
    write(os.path.join(ctx, "diffstat.txt"), sh(["git", "diff", "--stat", base, "--", ".", ":(exclude).agents"], root)[1])
    write(os.path.join(ctx, "diff.patch"), sh(["git", "diff", base, "--", ".", ":(exclude).agents", ":(exclude)docs/index/*.json"], root)[1])
    write(os.path.join(ctx, "agents_dir_diff.txt"),
          sh(["git", "diff", "--stat", base, "--", ".agents", ":(exclude)%s" % rl.AUDITOR_ROWS_REL], root)[1]
          or "(no changes under .agents/ other than auditor_rows.json)")
    write(os.path.join(ctx, "env_audit.txt"), env_audit(root))

    test_meta = {"executed": False, "note": "skipped by --skip-tests"}
    if a.skip_tests:
        write(os.path.join(ctx, "tests.txt"), "ORCHESTRATOR SUMMARY: tests skipped by --skip-tests (NOT executed)\n")
    else:
        print("[review] running cargo test (wrappers scrubbed, isolated target dir)...")
        text, tm = run_tests(root, sdir, a.test_timeout, attest_dir=os.path.join(ctx, "attest"))
        write(os.path.join(ctx, "tests.txt"), text)
        test_meta = dict(tm, executed=(tm["passed"] + tm["failed"]) > 0)
        print("[review] tests: %s" % test_meta)

    parity, parity_md = run_parity(root, sdir, ctx)
    print("[review] parity: %s%% closed (%s/%s rows)" % (parity.get("percent_closed"), parity.get("closed_rows"),
                                                        parity.get("total_rows")))

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

    def reviewer_cmd(prompt, schema):
        c = ["agy", "-p", prompt, "--model", a.model, "--effort", a.effort,
             "--output-format", "json", "--json-schema", schema,
             "--print-timeout", a.reviewer_timeout, "--sandbox", "--add-dir", root]
        if os.path.isdir(WIKI):
            c += ["--add-dir", WIKI]
        if os.path.isdir(CRUCIBLE):
            c += ["--add-dir", CRUCIBLE]
        if not os.path.abspath(SKILL_DIR).startswith(os.path.abspath(root) + os.sep):
            c += ["--add-dir", SKILL_DIR]  # reviewing a separate worktree: brief/history live elsewhere
        return c

    def record(mode, verdict, so, conv, prefix):
        meta = {"verdict": verdict, "mode": mode, "timestamp": ts, "model": a.model, "effort": a.effort,
                "head": head, "base": base, "tree": tree_before, "tests_executed": test_meta.get("executed"),
                "test_summary": json.dumps(test_meta), "parity_percent": parity.get("percent_closed"),
                "parity_complete": parity.get("complete"), "conversation_id": conv}
        report = render_report(meta, so, parity_md)
        rpath = os.path.join(run_dir, "%sreport.md" % prefix)
        write(rpath, report)
        write(os.path.join(sdir, "latest_gap_audit.md" if mode == "gap-audit" else "latest.md"), report)
        write(os.path.join(run_dir, "%sresult.json" % prefix), json.dumps({"meta": meta, "structured": so}, indent=2))
        st = rl.load_state(root)
        st.setdefault("reviews", []).append(dict(meta, report=rpath))
        rl.save_state(root, st)
        print("[review] %s VERDICT: %s" % (mode, verdict))
        print("[review] report: %s" % rpath)
        return rpath

    rc = 0
    modes = [a.mode]
    if a.mode == "claims" and not a.no_auto_audit and \
            reviews_since_last_audit(state) + 1 >= int(os.environ.get("EVOMU_GAP_AUDIT_EVERY", "5")):
        modes.append("gap-audit")
    for mode in modes:
        prefix = "" if mode == a.mode else "gap_"
        prompt = build_prompt(ctx, has_claims, mode)
        write(os.path.join(run_dir, "%sprompt.md" % prefix), prompt)
        cmd = reviewer_cmd(prompt, GAP_SCHEMA if mode == "gap-audit" else SCHEMA)
        printable = [c if c != prompt else "<prompt: %s>" % os.path.join(run_dir, "%sprompt.md" % prefix) for c in cmd]
        print("[review] %s reviewer command: %s" % (mode, " ".join(printable)))
        if a.dry_run:
            continue
        print("[review] launching headless %s reviewer (this can take a while)..." % mode)
        valid = GAP_EXIT if mode == "gap-audit" else EXIT
        verdict, so, conv = launch_reviewer(cmd, root, sdir, run_dir, tree_before, ts, valid, prefix)
        if mode == "gap-audit" and verdict in GAP_EXIT:
            added = apply_gap_audit(so, ts)
            so["summary"] = ("[%d new ledger rows appended to auditor_rows.json] " % added) + so.get("summary", "")
        record(mode, verdict, so, conv, prefix)
        if so.get("blocking_findings"):
            print("[review] blocking findings: %d" % len(so["blocking_findings"]))
        code = (GAP_EXIT if mode == "gap-audit" else EXIT).get(verdict, 3)
        if mode == a.mode:
            rc = code
        if mode == "claims" and verdict not in ("PASS", "PASS_WITH_ISSUES") and len(modes) > 1:
            print("[review] claims verdict %s: skipping the chained gap audit (it only runs on a committable "
                  "tree; fix the findings and re-run)." % verdict)
            break
    if a.dry_run:
        print("[review] dry run: context in %s" % ctx)
        return 0
    return rc


if __name__ == "__main__":
    sys.exit(main())
