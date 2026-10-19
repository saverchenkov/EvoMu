#!/usr/bin/python3
"""EvoMu pre-flight: fast, mechanical self-check to run BEFORE run_review.py.

It does what the reviewer would otherwise catch a full review cycle later:
  1. Runs `cargo test --workspace` exactly like the orchestrator does (wrappers scrubbed; shares the
     orchestrator's target dir, so the following review builds incrementally), with a fresh parity
     attestation dir.
  2. Computes parity with scripts/parity/parity_status.py and diffs it against the last review:
     rows newly DONE (with their acceptance text, so you can check each clause is really tested), rows
     that regressed, tagged tests that don't pass, and tags matching no ledger row.
  3. Prints AUTHORITATIVE NUMBERS (tests, parity, per-category, SDE manifest counts, diffstat) and writes
     them as a claims skeleton to .evomu-reviews/preflight/claims_template.md. Paste numbers from here;
     never type them.
  4. With --claims FILE, lints your draft claims:
       - every number must appear in the evidence (test output, parity, manifest, diffstat, or the
         repo text the claim points at), otherwise it's flagged UNTRACEABLE;
       - every `path:line` reference must exist (file present, line number within the file);
       - every double-quoted string of 12+ characters presented as a quotation must occur verbatim
         somewhere in the repo, otherwise it's flagged NOT FOUND;
       - every ledger row id mentioned as closed must be DONE in this pre-flight.
Exit code: 0 when tests pass and the claims lint is clean (or no --claims), 1 otherwise.
This script never replaces the independent review; it only makes the review cheaper to pass honestly.
"""
import argparse
import glob
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import review_lib as rl  # noqa: E402

PARITY_STATUS = os.path.join(HERE, "parity", "parity_status.py")
sys.path.insert(0, os.path.join(HERE, "metrics"))
import rpc_names  # noqa: E402
LEDGER = os.path.join(os.path.dirname(HERE), "references", "parity", "ledger.json")
AUDITOR = os.path.join(os.path.dirname(HERE), "references", "parity", "auditor_rows.json")
WRAPPER_ENV = ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTDOC", "CARGO_BUILD_RUSTDOC",
               "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR"]
ROW_RE = re.compile(r"\b(?:rpc|notify|destiny|ballmode|quasar|esi|sde|effect|feat|plan|journey):[^\s`'\"|,;)]+")


def sh(cmd, cwd, env=None, timeout=None):
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout + (("\n[stderr]\n" + p.stderr) if p.stderr else "")
    except subprocess.TimeoutExpired as e:
        return 124, "TIMEOUT after %ss\n%s" % (timeout, e.stdout or "")


def last_review_parity(sdir):
    st = rl.load_state(os.path.dirname(sdir))
    for r in reversed(st.get("reviews", [])):
        rep = r.get("report")
        if rep:
            pj = os.path.join(os.path.dirname(rep), "context", "parity.json")
            if os.path.exists(pj):
                return pj, r.get("timestamp")
    return None, None


def ledger_rows():
    rows = {r["id"]: r for r in json.load(open(LEDGER))["rows"]}
    if os.path.exists(AUDITOR):
        for r in json.load(open(AUDITOR)).get("rows", []):
            rows.setdefault(r["id"], r)
    return rows


def numbers_in(text):
    out = set()
    for m in re.findall(r"(?<![\w.])\d[\d,]*(?:\.\d+)?", text):
        n = m.replace(",", "").rstrip(".")
        if n:
            out.add(n)
            if "." in n:
                out.add(n.rstrip("0").rstrip("."))
    return out


def lint_claims(root, claims_text, evidence_text, parity_rows):
    problems = []
    ev_nums = numbers_in(evidence_text)
    # path:line references
    refs = re.findall(r"([A-Za-z0-9_./\-]+\.(?:rs|py|md|toml|json|sql|c|sh|txt)):(\d+)(?:[-–](\d+))?", claims_text)
    cited_text = ""
    for path, a, b in refs:
        cands = [path] if os.path.exists(os.path.join(root, path)) else \
            [p for p in glob.glob(os.path.join(root, "**", os.path.basename(path)), recursive=True)
             if "/target/" not in p and "/.evomu-reviews/" not in p]
        full = os.path.join(root, path) if os.path.exists(os.path.join(root, path)) else (cands[0] if len(cands) == 1 else None)
        if not full:
            problems.append("PATH NOT FOUND: %s:%s" % (path, a))
            continue
        try:
            lines = open(full, errors="replace").read().splitlines()
        except OSError:
            continue
        hi = int(b or a)
        if int(a) < 1 or hi > len(lines):
            problems.append("LINE OUT OF RANGE: %s:%s%s (file has %d lines)" % (path, a, "-" + b if b else "", len(lines)))
        else:
            cited_text += "\n".join(lines[int(a) - 1:hi]) + "\n"
    ev_nums |= numbers_in(cited_text)
    # numbers that occur in the full text of files the claims mention (IDs, constants) are traceable
    for path in set(re.findall(r"([A-Za-z0-9_./\-]+\.(?:rs|py|md|toml|json|sql|c|sh|txt))", claims_text)):
        fp = os.path.join(root, path)
        if os.path.isfile(fp) and os.path.getsize(fp) < 5_000_000:
            ev_nums |= numbers_in(open(fp, errors="replace").read())
    # numbers
    body = re.sub(r"([A-Za-z0-9_./\-]+\.(?:rs|py|md|toml|json|sql|c|sh|txt)):\d+(?:[-–]\d+)?", " ", claims_text)
    body = re.sub(r"\b[0-9a-f]{7,40}\b", " ", body)              # commit hashes
    body = re.sub(r"\b(?:[A-Z]{1,4}\d*-P?\d+(?:-\d+)?|OF-\d+)\b", " ", body)  # task / finding ids
    body = ROW_RE.sub(" ", body)
    body = re.sub(r"\b\d{8}-\d{6}\b", " ", body)                  # review timestamps
    body = re.sub(r"(?i)\b(?:lines?|L)\s*\d[\d,\s\-–]*", " ", body)  # bare line references
    for n in sorted(numbers_in(body), key=lambda x: (len(x), x)):
        if len(n.replace(".", "")) <= 1 or (n.startswith("0") and not n.startswith("0.")):
            continue  # single digits are too ambiguous; leading-zero fragments come from ids/hashes
        if n not in ev_nums:
            problems.append("UNTRACEABLE NUMBER: %s (not in test output, parity, manifest, diffstat or cited lines)" % n)
    # quoted strings
    for q in re.findall(r"[\"“]([^\"”\n]{12,200})[\"”]", claims_text):
        if ROW_RE.search(q):
            continue
        rc, out = sh(["git", "grep", "-F", "-q", "--untracked", "-e", q, "--", ".", ":(exclude).evomu-reviews"], root)
        if rc != 0:
            problems.append("QUOTE NOT FOUND IN REPO: \"%s\"" % q[:120])
    # row ids claimed
    for rid in sorted(set(ROW_RE.findall(claims_text))):
        st = (parity_rows.get(rid) or {}).get("status")
        if st is None:
            problems.append("UNKNOWN LEDGER ROW: %s" % rid)
        elif st not in ("DONE", "NA"):
            problems.append("ROW MENTIONED BUT NOT CLOSED: %s is %s (fine only if your claim says it is open)" % (rid, st))
    return problems


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--claims", help="draft claims file to lint")
    ap.add_argument("--no-tests", action="store_true", help="reuse the last pre-flight test output")
    ap.add_argument("--no-status", action="store_true", help="don't regenerate docs/ACTION_PLAN_STATUS.md")
    ap.add_argument("--test-timeout", type=int, default=7200)
    a = ap.parse_args()

    root = rl.repo_root(os.getcwd())
    msg = rl.stale_gate(root)
    if msg:
        print("[preflight] " + msg)
        return 1
    sdir = rl.state_dir(root)
    pdir = os.path.join(sdir, "preflight")
    os.makedirs(pdir, exist_ok=True)
    tests_txt = os.path.join(pdir, "tests.txt")
    attest = os.path.join(pdir, "attest")

    if not a.no_tests:
        shutil.rmtree(attest, ignore_errors=True)
        os.makedirs(attest, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if k not in WRAPPER_ENV}
        env["CARGO_TARGET_DIR"] = os.path.join(sdir, "target")
        env["EVOMU_PARITY_ATTEST_DIR"] = attest
        cargo = shutil.which("cargo", path=env.get("PATH")) or "/opt/homebrew/bin/cargo"
        print("[preflight] cargo test --workspace --no-fail-fast ...", flush=True)
        rc, out = sh([cargo, "test", "--workspace", "--no-fail-fast"], root, env=env, timeout=a.test_timeout)
        open(tests_txt, "w").write("exit=%d\n%s" % (rc, out))
    out = open(tests_txt, errors="replace").read() if os.path.exists(tests_txt) else ""
    passed = sum(int(m) for m in re.findall(r"test result: \w+\. (\d+) passed", out))
    failed = sum(int(m) for m in re.findall(r"test result: \w+\. \d+ passed; (\d+) failed", out))
    ignored = sum(int(m) for m in re.findall(r"test result: \w+\. \d+ passed; \d+ failed; (\d+) ignored", out))
    failing = re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M)
    compile_err = "error[E" in out or "could not compile" in out

    sh([sys.executable, PARITY_STATUS, "--tests", tests_txt, "--attest-dir", attest,
        "--out", os.path.join(pdir, "parity.json"), "--md", os.path.join(pdir, "parity.md")], root, timeout=900)
    par = json.load(open(os.path.join(pdir, "parity.json")))
    if not a.no_status:
        _, st_out = sh([sys.executable, os.path.join(HERE, "parity", "status_table.py"), "--parity",
                        os.path.join(pdir, "parity.json")], root, timeout=600)
        print(st_out.strip())
    summ, prow = par["summary"], par["rows"]
    rows = ledger_rows()

    prev_path, prev_ts = last_review_parity(sdir)
    prev = json.load(open(prev_path))["rows"] if prev_path else {}
    newly = sorted(r for r, v in prow.items() if v["status"] in ("DONE", "NA")
                   and (prev.get(r) or {}).get("status") not in ("DONE", "NA"))
    regressed = sorted(r for r, v in prev.items() if v.get("status") in ("DONE", "NA")
                       and (prow.get(r) or {}).get("status") not in ("DONE", "NA"))
    not_passing = sorted(r for r, v in prow.items() if v["status"] in ("TEST_NOT_PASSING", "TESTED_NO_IMPL"))

    _, diffstat = sh(["git", "diff", "--stat", "HEAD"], root)
    _, untracked = sh(["git", "ls-files", "--others", "--exclude-standard"], root)
    manifest = os.path.join(root, "data", "sde_import_manifest.json")
    man_txt = open(manifest).read() if os.path.exists(manifest) else ""
    man_counts = {}
    try:
        man_counts = json.loads(man_txt).get("table_counts", {}) if man_txt else {}
    except ValueError:
        pass

    L = ["# Pre-flight numbers (authoritative; paste, don't type)", "",
         "- Tests: **%d passed, %d failed, %d ignored**%s" % (passed, failed, ignored,
                                                              " (COMPILE ERROR)" if compile_err else ""),
         "- Parity: **%d / %d rows closed (%.2f%%)**, %d NA, %d tagged tests" % (
             summ["closed_rows"], summ["total_rows"], summ["percent_closed"], summ["na_rows"], summ["tagged_tests"]),
         "- Baseline for the diff below: last review %s" % (prev_ts or "(none)"), "",
         "| Category | Total | DONE | NA | Open |", "|---|---|---|---|---|"]
    for k, c in sorted(summ["categories"].items()):
        L.append("| %s | %d | %d | %d | %d |" % (k, c["total"], c["DONE"], c["NA"], c["open"]))
    L += ["", "## Rows newly closed since last review (%d). Check EVERY clause of each acceptance text is tested" % len(newly)]
    for r in newly:
        L.append("- `%s` (tests: %s)\n  acceptance: %s" % (r, ", ".join(prow[r]["tests"] or prow[r]["attested_by"])[:200],
                                                          (rows.get(r) or {}).get("acceptance", "")[:400]))
    if regressed:
        L += ["", "## Rows that REGRESSED since last review (%d)" % len(regressed)] + ["- `%s`" % r for r in regressed]
    if not_passing:
        L += ["", "## Tagged but not closed (%d)" % len(not_passing)] + \
             ["- `%s`: %s" % (r, prow[r]["status"]) for r in not_passing[:60]]
    if summ.get("unknown_tags"):
        L += ["", "## Tags matching no ledger row (fix the id)"] + ["- `%s`" % t for t in summ["unknown_tags"]]
    if failing:
        L += ["", "## Failing tests"] + ["- %s" % t for t in failing]
    if man_counts:
        L += ["", "## SDE manifest table_counts (data/sde_import_manifest.json)"] + \
             ["- %s: %s" % (k, v) for k, v in sorted(man_counts.items())]
    L += ["", "## Diffstat vs HEAD", "```", diffstat.strip() or "(clean)", "```"]
    if untracked.strip():
        L += ["", "## Untracked files", "```", untracked.strip()[:4000], "```"]
    # RPC-name check: names must exist in the real client (blocking for files changed since the last review)
    st = rl.load_state(root)
    base = None
    for r in reversed(st.get("reviews", [])):
        h = r.get("head")
        if h and subprocess.run(["git", "merge-base", "--is-ancestor", h, "HEAD"], cwd=root).returncode == 0:
            base = h
            break
    added = {}  # file -> set of added/modified line numbers (new code must be clean; legacy lines are reported)

    def collect(diff_args):
        cur = None
        for ln in sh(["git", "diff", "-U0"] + diff_args, root)[1].splitlines():
            if ln.startswith("+++ "):
                cur = ln[6:] if ln.startswith("+++ b/") else None
            elif ln.startswith("@@") and cur:
                m = re.search(r"\+(\d+)(?:,(\d+))?", ln)
                if m:
                    a0, n = int(m.group(1)), int(m.group(2) or 1)
                    added.setdefault(cur, set()).update(range(a0, a0 + n))
    if base:
        collect([base])
    collect(["HEAD"])
    for f in sh(["git", "ls-files", "--others", "--exclude-standard"], root)[1].split():
        if f.endswith(".rs") and os.path.exists(os.path.join(root, f)):
            added[f] = None  # whole new file
    all_viol, have_client = rpc_names.run()
    hard_all = [v for v in all_viol if v["kind"] == "NOT_IN_CLIENT"]
    rpc_block = [v for v in hard_all if v["file"] in added and (added[v["file"]] is None or v["line"] in added[v["file"]])]
    L += ["", "## RPC-name check (names must exist in the real V24 client)",
          "- Repo-wide: **%d NOT_IN_CLIENT**, %d NOT_IN_CATALOG (informational)%s" % (
              len(hard_all), len(all_viol) - len(hard_all),
              "" if have_client else " [WARNING: client code.ccp not found; catalog-only]"),
          "- On lines you added or changed since the last review (blocking): **%d**" % len(rpc_block)]
    for v in rpc_block[:80]:
        L.append("  - `%s:%d` %s `%s`" % (v["file"], v["line"], v["what"], v["name"]))
    report = "\n".join(L)
    open(os.path.join(pdir, "claims_template.md"), "w").write(report)
    print(report)

    lint_ok = True
    if a.claims:
        claims = open(a.claims, errors="replace").read()
        evidence = "\n".join([out, json.dumps(summ), report, man_txt, diffstat])
        probs = lint_claims(root, claims, evidence, prow)
        print("\n# Claims lint: %s" % ("CLEAN" if not probs else "%d problem(s)" % len(probs)))
        for p in probs:
            print("- " + p)
        lint_ok = not probs
    print("\n[preflight] template: %s" % os.path.join(pdir, "claims_template.md"))
    if rpc_block:
        print("\n# RPC-name check: %d name(s) on lines you added/changed don't exist anywhere in the V24 client. Remove them, "
              "use the client's real method, or justify with `rpc-lint: allow <reason>`." % len(rpc_block))
    ok = failed == 0 and not compile_err and passed > 0 and lint_ok and not rpc_block
    print("[preflight] %s" % ("OK: run the review now." if ok else "NOT READY: fix the items above before spending a review."))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
