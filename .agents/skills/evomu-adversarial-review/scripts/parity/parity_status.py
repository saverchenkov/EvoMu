#!/usr/bin/python3
"""Compute EvoMu parity status against the pinned ledger. Mechanical; no LLM.

A row is DONE when ALL hold:
  1. At least one test tagged with the row id PASSED in the given cargo test output.
     Tag syntax (in the doc/comment block right above the test fn, or in its first lines):
         // parity: rpc:miningScanMgr.perform_scan, feat:mining.belt_respawn
     Wildcards are not accepted. For rows with attest_ok (sde:*, effect:*), a passing test may instead
     attest ids at runtime by appending JSON lines {"test": "<fn name>", "rows": [...]} to files in
     $EVOMU_PARITY_ATTEST_DIR.
  2. The category's static check passes on non-test source:
       rpc     -> the method name literal and service name literal appear in the same non-test .rs file
                  under crates/, and that file does not answer the method with UnimplementedRPC only.
       notify  -> the event name literal appears in non-test server code.
       destiny -> the action name appears in non-test code under crates/.
       esi     -> the route path (params normalized) appears in non-test code.
  3. plan:<id> rows additionally require every feat row mapped to that task to be DONE.
  4. effect:* requires attested effect:<id> count >= expected_count.

A row is NA when the category allows it and docs/parity/triage.json has an entry
  {"<row id>": {"disposition": "client_local|dead_client_code|removed_from_live", "evidence": "<path:line ...>"}}
that the gap auditor has not rejected (auditor_rows.json "rejected_triage").

Usage: parity_status.py --tests <tests.txt> [--attest-dir DIR] [--out parity.json] [--md parity.md]
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SKILL = os.path.dirname(os.path.dirname(HERE))
PARITY = os.path.join(SKILL, "references", "parity")
TAG_RE = re.compile(r"//+!?\s*parity:\s*(.+)$")
TEST_ATTR_RE = re.compile(r"#\[(?:tokio::)?test\b|#\[rstest\b|#\[test_case\b|#\[proptest\b|#\[test\(")
FN_RE = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]")
PASS_RE = re.compile(r"^test\s+(\S+)\s+\.\.\.\s+ok\s*$", re.M)
DISPOSITIONS = {"client_local", "dead_client_code", "removed_from_live"}


def repo_root():
    out = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=HERE, capture_output=True, text=True)
    return out.stdout.strip()


def load_ledger():
    with open(os.path.join(PARITY, "ledger.json")) as f:
        rows = json.load(f)["rows"]
    aud = {"rows": [], "rejected_triage": []}
    ap = os.path.join(PARITY, "auditor_rows.json")
    if os.path.exists(ap):
        with open(ap) as f:
            aud = json.load(f)
    ids = {r["id"] for r in rows}
    for r in aud.get("rows", []):
        if r["id"] not in ids:
            r.setdefault("category", "feat")
            r.setdefault("na_allowed", False)
            r.setdefault("attest_ok", False)
            rows.append(r)
            ids.add(r["id"])
    return rows, set(aud.get("rejected_triage", [])), {f["id"] for f in aud.get("fake_done", [])}


def rust_files(root):
    return [p for p in glob.glob(os.path.join(root, "crates", "**", "*.rs"), recursive=True)
            if "/target/" not in p]


def split_test_code(path, text):
    """Return (non_test_text, is_test_file). Test code = files under tests/ or benches/, and
    everything from the first `#[cfg(test)]` onward (EvoMu convention: test module at file end)."""
    rel = path.replace("\\", "/")
    if "/tests/" in rel or "/benches/" in rel:
        return "", True
    i = text.find("#[cfg(test)]")
    return (text if i < 0 else text[:i]), False


def _tags_in(line):
    m = TAG_RE.search(line)
    return {t.strip() for t in m.group(1).split(",") if t.strip()} if m else set()


def collect_tags(files):
    """Map test fn name -> set(row ids). A test fn is a `fn` whose contiguous attribute/comment block
    (walking upward over lines starting with `#[`, `//`, `///`) contains a test attribute. Tags are taken
    from that block and from `// parity:` lines in the first 12 lines of the body."""
    tags = {}
    for p in files:
        try:
            lines = open(p, encoding="utf-8", errors="replace").read().splitlines()
        except OSError:
            continue
        for i, ln in enumerate(lines):
            fm = FN_RE.search(ln)
            if not fm:
                continue
            block, j = [], i - 1
            while j >= 0:
                s = lines[j].strip()
                if s.startswith("#[") or s.startswith("//") or (s.endswith(")]") and block):
                    block.append(lines[j])
                    j -= 1
                    continue
                break
            if not any(TEST_ATTR_RE.search(b) for b in block):
                continue
            got = set()
            for b in block:
                got |= _tags_in(b)
            depth = ln.count("{") - ln.count("}")
            opened = "{" in ln
            k = i + 1
            while k < min(i + 13, len(lines)) and not (opened and depth <= 0):
                got |= _tags_in(lines[k])
                depth += lines[k].count("{") - lines[k].count("}")
                opened = opened or "{" in lines[k]
                k += 1
            got = {t for t in got if "*" not in t}
            if got:
                tags.setdefault(fm.group(1), set()).update(got)
    return tags


def passed_tests(tests_text):
    names = set()
    for full in PASS_RE.findall(tests_text):
        names.add(full.split("::")[-1])
    return names


def attestations(attest_dir, passed):
    att = {}
    if not attest_dir or not os.path.isdir(attest_dir):
        return att
    for p in glob.glob(os.path.join(attest_dir, "*.jsonl")):
        for ln in open(p, errors="replace"):
            try:
                o = json.loads(ln)
            except ValueError:
                continue
            t = str(o.get("test", "")).split("::")[-1]
            if t in passed:
                for rid in o.get("rows") or []:
                    att.setdefault(rid, set()).add(t)
    return att


def norm_route(p):
    return re.sub(r"\{[^}]+\}|:[A-Za-z_]+", "{}", p).rstrip("/")


def static_checks(root, rows):
    src_by_file = {}
    for p in rust_files(root):
        txt = open(p, encoding="utf-8", errors="replace").read()
        nt, is_test = split_test_code(p, txt)
        if nt:
            src_by_file[p] = nt
    blob = "\n".join(src_by_file.values())
    routes = {norm_route(m) for m in re.findall(r"\"(/[A-Za-z0-9_/{}:\-]+)\"", blob)}
    ok = {}
    for r in rows:
        c, rid = r["category"], r["id"]
        if c == "rpc":
            lit_s, lit_m = '"%s"' % r["service"], '"%s"' % r["method"]
            good = False
            for txt in src_by_file.values():
                if lit_s in txt and lit_m in txt:
                    # reject if every occurrence of the method literal is followed by UnimplementedRPC
                    occ = [m.start() for m in re.finditer(re.escape(lit_m), txt)]
                    if any("UnimplementedRPC" not in txt[o:o + 400] for o in occ):
                        good = True
                        break
            ok[rid] = good
        elif c == "notify":
            ok[rid] = ('"%s"' % r["title"]) in blob
        elif c == "destiny":
            ok[rid] = r["title"] in blob
        elif c == "esi":
            ok[rid] = norm_route(r["title"].split(" ", 1)[1]) in routes
        else:
            ok[rid] = True
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tests", required=True)
    ap.add_argument("--attest-dir", default=os.environ.get("EVOMU_PARITY_ATTEST_DIR", ""))
    ap.add_argument("--out")
    ap.add_argument("--md")
    ap.add_argument("--state-dir", help="where to keep the append-only ledger id record")
    a = ap.parse_args()

    root = repo_root()
    rows, rejected, fake = load_ledger()
    tests_text = open(a.tests, errors="replace").read() if os.path.exists(a.tests) else ""
    passed = passed_tests(tests_text)
    files = rust_files(root)
    tags = collect_tags(files)
    tagged_rows = {}
    for t, rids in tags.items():
        for rid in rids:
            tagged_rows.setdefault(rid, set()).add(t)
    att = attestations(a.attest_dir, passed)
    sc = static_checks(root, rows)

    triage = {}
    tp = os.path.join(root, "docs", "parity", "triage.json")
    if os.path.exists(tp):
        try:
            triage = json.load(open(tp))
        except ValueError:
            triage = {}
    # Supervisor-owned triage (evidence-backed, audited like docs/parity/triage.json). Agent entries win on conflict.
    sp = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "..", "references", "parity",
                      "supervisor_triage.json")
    if os.path.exists(sp):
        try:
            for k, v in json.load(open(sp)).get("rows", {}).items():
                triage.setdefault(k, v)
        except ValueError:
            pass

    status = {}
    for r in rows:
        rid = r["id"]
        t_pass = sorted(t for t in tagged_rows.get(rid, ()) if t in passed)
        a_pass = sorted(att.get(rid, ())) if r.get("attest_ok") else []
        tested = bool(t_pass or a_pass)
        if r["category"] == "effect":
            n = len([k for k in att if k.startswith("effect:")])
            exp = int(r.get("expected_count") or 0)
            tested = exp > 0 and n >= exp
            a_pass = ["%d/%d effects attested" % (n, exp)]
        tri = triage.get(rid)
        if (r.get("na_allowed") and tri and tri.get("disposition") in DISPOSITIONS
                and len(str(tri.get("evidence", ""))) >= 12 and rid not in rejected):
            st = "NA"
        elif rid in fake:
            st = "FLAGGED_FAKE"
        elif tested and sc.get(rid, True):
            st = "DONE"
        elif tested:
            st = "TESTED_NO_IMPL"
        elif rid in tagged_rows:
            st = "TEST_NOT_PASSING"
        else:
            st = "MISSING"
        status[rid] = {"status": st, "tests": t_pass, "attested_by": a_pass}

    # plan rows require mapped feature rows DONE
    by_task = {}
    for r in rows:
        if r["category"] == "feat" and r.get("plan_task"):
            by_task.setdefault(r["plan_task"], []).append(r["id"])
    for r in rows:
        if r["category"] == "plan" and status[r["id"]]["status"] == "DONE":
            tid = r["id"].split(":", 1)[1]
            open_feats = [f for f in by_task.get(tid, []) if status[f]["status"] not in ("DONE", "NA")]
            if open_feats:
                status[r["id"]]["status"] = "BLOCKED_BY_FEATURES"
                status[r["id"]]["open_features"] = open_feats[:20]

    unknown_tags = sorted(set(tagged_rows) - {r["id"] for r in rows} -
                          {k for k in tagged_rows if k.startswith("effect:") or k.startswith("sde:")})
    cats = {}
    for r in rows:
        c = cats.setdefault(r["category"], {"total": 0, "DONE": 0, "NA": 0, "open": 0})
        c["total"] += 1
        s = status[r["id"]]["status"]
        if s in ("DONE", "NA"):
            c[s] += 1
        else:
            c["open"] += 1
    total = len(rows)
    closed = sum(c["DONE"] + c["NA"] for c in cats.values())

    tamper = []
    if a.state_dir:
        sp = os.path.join(a.state_dir, "ledger_seen.json")
        seen = set(json.load(open(sp))) if os.path.exists(sp) else set()
        now = {r["id"] for r in rows}
        tamper = sorted(seen - now)
        json.dump(sorted(seen | now), open(sp, "w"))

    summary = {
        "total_rows": total, "closed_rows": closed, "open_rows": total - closed,
        "percent_closed": round(100.0 * closed / total, 2) if total else 0.0,
        "categories": cats, "passed_tests": len(passed), "tagged_tests": len(tags),
        "na_rows": sum(c["NA"] for c in cats.values()),
        "unknown_tags": unknown_tags[:50], "ledger_rows_removed": tamper,
        "complete": (closed == total and not tamper),
    }
    out = {"summary": summary, "rows": status}
    if a.out:
        json.dump(out, open(a.out, "w"), indent=1)
    md = ["# Parity status", "",
          "**%s / %s rows closed (%.2f%%)**, %d NA (triaged), %d passing tests, %d tagged tests."
          % (closed, total, summary["percent_closed"], summary["na_rows"], len(passed), len(tags)), "",
          "| Category | Total | DONE | NA | Open |", "|---|---|---|---|---|"]
    for k in sorted(cats):
        c = cats[k]
        md.append("| %s | %d | %d | %d | %d |" % (k, c["total"], c["DONE"], c["NA"], c["open"]))
    if tamper:
        md += ["", "**LEDGER TAMPERING: rows removed since last run:** %s" % tamper[:20]]
    if unknown_tags:
        md += ["", "Tags that match no ledger row: %s" % unknown_tags[:20]]
    md += ["", "## First open rows per category"]
    for k in sorted(cats):
        opens = [rid for rid, s in status.items() if s["status"] not in ("DONE", "NA")
                 and next((r for r in rows if r["id"] == rid), {}).get("category") == k][:15]
        if opens:
            md.append("- **%s:** %s" % (k, ", ".join("`%s` (%s)" % (o, status[o]["status"]) for o in opens)))
    text = "\n".join(md)
    if a.md:
        open(a.md, "w").write(text)
    print(text.split("\n## First open")[0])
    return 0 if summary["complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
