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
import time
import datetime

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
ROW_RE = re.compile(r"\besi:(?:GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)\s+/[^\s`'\"|,;)]+"  # esi ids: "esi:GET /path"
                    r"|\b(?:rpc|notify|destiny|ballmode|quasar|esi|sde|effect|feat|plan|journey):[^\s`'\"|,;)]+")


def sh(cmd, cwd, env=None, timeout=None):
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout + (("\n[stderr]\n" + p.stderr) if p.stderr else "")
    except subprocess.TimeoutExpired as e:
        return 124, "TIMEOUT after %ss\n%s" % (timeout, e.stdout or "")


def last_review_parity(sdir):
    """Baseline for "newly closed": the most recent review that is an ancestor of this branch: either a local review
    whose head is in HEAD's history, or (fresh worktrees, branches rebased onto a newer main) the review that covers
    `main`'s current code tree in any worktree."""
    root = os.path.dirname(sdir)
    cands = []
    for r in rl.load_state(root).get("reviews", []):
        h = r.get("head")
        if r.get("report") and h and subprocess.run(["git", "merge-base", "--is-ancestor", h, "HEAD"], cwd=root,
                                                     capture_output=True).returncode == 0:
            cands.append(r)
    try:
        mtree = subprocess.run(["git", "rev-parse", "main^{tree}"], cwd=root, capture_output=True, text=True).stdout.strip()
        mr = rl.review_covering_tree(root, mtree) if mtree else None
        if mr and mr.get("report"):
            cands.append(mr)
    except Exception:  # noqa: BLE001
        pass
    for r in sorted(cands, key=lambda x: x.get("timestamp", ""), reverse=True):
        pj = os.path.join(os.path.dirname(r["report"]), "context", "parity.json")
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
    # A comma is a thousands separator only before exactly three digits ("2,023"); "147,294" in "rs:89-147,294" is
    # a list of line numbers, not 147294.
    for m in re.findall(r"(?<![\w.])(?:\d{1,3}(?:,\d{3})+(?![\d])|\d+)(?:\.\d+)?", text):
        n = m.replace(",", "").rstrip(".")
        if n:
            out.add(n)
            if "." in n:
                out.add(n.rstrip("0").rstrip("."))
    return out


CITE_RE = re.compile(r"([A-Za-z0-9_./\-]+\.(?:rs|py|md|toml|json|sql|c|sh|txt)):"
                     r"(\d+(?:[-–]\d+)?(?:,\s?\d+(?:[-–]\d+)?)*)")
ROW_WILDCARD_RE = re.compile(r"^[a-z]+:\*$")  # category shorthand in prose, e.g. "sde:*"


def cite_ranges(spec):
    """'79,89-147,294' -> [(79, 79), (89, 147), (294, 294)]"""
    out = []
    for part in re.split(r",\s?", spec):
        a, _, b = part.replace("–", "-").partition("-")
        out.append((int(a), int(b or a)))
    return out


def lint_claims(root, claims_text, evidence_text, parity_rows):
    problems = []
    ev_nums = numbers_in(evidence_text)
    # path:line references
    refs = CITE_RE.findall(claims_text)
    cited_text = ""
    for path, spec in refs:
        cands = [path] if os.path.exists(os.path.join(root, path)) else \
            [p for p in glob.glob(os.path.join(root, "**", os.path.basename(path)), recursive=True)
             if "/target/" not in p and "/.evomu-reviews/" not in p]
        full = os.path.join(root, path) if os.path.exists(os.path.join(root, path)) else (cands[0] if len(cands) == 1 else None)
        if not full:
            problems.append("PATH NOT FOUND: %s:%s" % (path, spec))
            continue
        try:
            lines = open(full, errors="replace").read().splitlines()
        except OSError:
            continue
        for a, hi in cite_ranges(spec):
            if a < 1 or hi > len(lines) or hi < a:
                problems.append("LINE OUT OF RANGE: %s:%d%s (file has %d lines)" % (
                    path, a, "-%d" % hi if hi != a else "", len(lines)))
            else:
                cited_text += "\n".join(lines[a - 1:hi]) + "\n"
    ev_nums |= numbers_in(cited_text)
    # numbers that occur in the full text of files the claims mention (IDs, constants) are traceable
    for path in set(re.findall(r"([A-Za-z0-9_./\-]+\.(?:rs|py|md|toml|json|sql|c|sh|txt))", claims_text)):
        fp = os.path.join(root, path)
        if os.path.isfile(fp) and os.path.getsize(fp) < 5_000_000:
            ev_nums |= numbers_in(open(fp, errors="replace").read())
    # numbers
    body = CITE_RE.sub(" ", claims_text)
    body = re.sub(r"\b\d{8}-\d{6}\b", " ", body)                  # review timestamps (before hashes: 20261020 is hex)
    body = re.sub(r"\b[0-9a-f]{7,40}\b", " ", body)              # commit hashes
    body = re.sub(r"\b(?:[A-Z]{1,4}\d*-P?\d+(?:-\d+)?|OF-\d+)\b", " ", body)  # task / finding ids
    body = ROW_RE.sub(" ", body)
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
    # row ids claimed (category shorthand like "sde:*" in prose is not a row id)
    for rid in sorted(set(ROW_RE.findall(claims_text))):
        if ROW_WILDCARD_RE.match(rid):
            continue
        st = (parity_rows.get(rid) or {}).get("status")
        if st is None:
            problems.append("UNKNOWN LEDGER ROW: %s" % rid)
        elif st not in ("DONE", "NA"):
            problems.append("ROW MENTIONED BUT NOT CLOSED: %s is %s (fine only if your claim says it is open)" % (rid, st))
    return problems


def find_base(root):
    st = rl.load_state(root)
    for r in reversed(st.get("reviews", [])):
        h = r.get("head")
        if h and subprocess.run(["git", "merge-base", "--is-ancestor", h, "HEAD"], cwd=root, capture_output=True).returncode == 0:
            return h
    if subprocess.run(["git", "rev-parse", "--verify", "-q", "main"], cwd=root, capture_output=True).returncode == 0:
        mb = subprocess.run(["git", "merge-base", "main", "HEAD"], cwd=root, capture_output=True, text=True).stdout.strip()
        if mb:
            return mb
    return None


def collect_added_lines(root, base):
    added = {}  # file -> set of line numbers or None for whole file
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
            added[f] = None
    return added


TEST_ATTR_RE = re.compile(r"^\s*#\[(?:tokio::)?test\b")
FN_RE = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)")
ASSERT_RE = re.compile(r"\b(assert_eq|assert_ne|assert|prop_assert_eq|prop_assert_ne|prop_assert)!\s*\(")
WEAK_ASSERT_RE = re.compile(
    r"matches!\s*\(|\.is_(?:ok|err|some|none|empty)\(\)|status_code|STATUS_OK|check_handled|"
    r"\.len\(\)\s*(?:>|>=|!=)\s*[01]\b")
REAL_ASSERT_RE = re.compile(r"==|!=|<=|>=|\s<\s|\s>\s|\.contains(?:_key)?\(|\.starts_with\(|\.ends_with\(")
DATA_ROW_PREFIXES = ("sde:", "effect:")  # data-driven sweeps may close many rows per test (AGENTS.md T-1/T-2)
ARM_RE = re.compile(r'^\s*\|?\s*\(\s*"([A-Za-z_]\w*)"\s*,\s*"([A-Za-z_]\w*)"\s*\)')
CONST_RET_RE = re.compile(
    r"PyRep::None\b|PyRep::Bool\((?:true|false)\)|PyRep::List\((?:Vec::new\(\)|vec!\[\])\)|"
    r"PyRep::Dict\((?:Default::default\(\)|\w+::new\(\))\)|PyRep::Tuple\((?:Vec::new\(\)|vec!\[\])\)")
STATE_RE = re.compile(
    r"ctx\.(?!now_unix)|\bctx\s*[,)]|\bsnap\b|snapshot|\.insert\(|\.remove\(|\.push\(|\.retain\(|"
    r"notify|send_|emit|\bself\.")


def _balanced(text, start):
    depth, i = 0, start
    while i < len(text):
        c = text[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return text[start:i + 1]
        i += 1
    return text[start:]


def lint_tagged_tests(root, added):
    """Tagged tests touched by the diff must not close many rows with few substantive assertions."""
    max_rows = int(os.environ.get("EVOMU_MAX_ROWS_PER_TEST", "15"))
    rows_per_assert = float(os.environ.get("EVOMU_ROWS_PER_ASSERT", "3"))
    out = []
    for fpath, lset in added.items():
        if not fpath.endswith(".rs") or not ("/tests/" in fpath or fpath.endswith("tests.rs")):
            continue
        full = os.path.join(root, fpath)
        if not os.path.exists(full):
            continue
        try:
            full_text = open(full, errors="replace").read()
            lines = full_text.splitlines()
        except OSError:
            continue
        attrs = [i for i, l in enumerate(lines) if TEST_ATTR_RE.match(l)]
        for k, a in enumerate(attrs):
            end = attrs[k + 1] if k + 1 < len(attrs) else len(lines)
            start = a
            while start > 0 and lines[start - 1].lstrip().startswith(("//", "#[")):
                start -= 1
            fn_line, name = None, "?"
            for j in range(a, min(a + 6, end)):
                m = FN_RE.match(lines[j])
                if m:
                    fn_line, name = j, m.group(1)
                    break
            if fn_line is None:
                continue
            if lset is not None and not any((i + 1) in lset for i in range(start, end)):
                continue
            block = lines[start:end]
            if any("parity-lint: allow" in l for l in block):
                continue
            rows = set()
            for l in block:
                if "// parity:" in l:
                    rows.update(ROW_RE.findall(l))
            counted = {r for r in rows if not r.startswith(DATA_ROW_PREFIXES)}
            if not counted:
                continue
            feat_counted = {r for r in counted if not r.startswith(("rpc:", "notify:", "destiny:"))}
            body = "\n".join(lines[fn_line:end])
            strong = weak = 0
            for m in ASSERT_RE.finditer(body):
                txt = _balanced(body, m.end() - 1)
                if WEAK_ASSERT_RE.search(txt):
                    weak += 1
                elif m.group(1) in ("assert", "prop_assert") and not REAL_ASSERT_RE.search(txt):
                    weak += 1
                else:
                    strong += 1
            loc = (fpath, fn_line + 1)
            for r in sorted(counted):
                if r.startswith("rpc:") and "." in r[4:]:
                    _, meth = r[4:].split(".", 1)
                    if meth not in full_text:
                        out.append(("UNTESTED_RPC_TAG",
                                    "%s tags %s but file never references method '%s'" % (name, r, meth)) + loc)
            n = len(counted)
            nf = len(feat_counted)
            if nf > max_rows:
                out.append(("TOO_MANY_ROWS_PER_TEST",
                            "%s tags %d feature/endpoint rows (max %d per test; split it so each row's acceptance is "
                            "exercised and asserted)" % (name, nf, max_rows)) + loc)
            if strong == 0:
                out.append(("SHAPE_ONLY_TEST",
                            "%s tags %d row(s) but has no value assertions (%d shape/status-only asserts such as "
                            "matches!/is_ok/status_code)" % (name, n, weak)) + loc)
            elif (nf > 0 and strong < -(-nf // rows_per_assert)) or strong < -(-n // (rows_per_assert * 2)):
                out.append(("THIN_ASSERTIONS",
                            "%s tags %d row(s) (%d feat/endpoint) with only %d value assertion(s) (need >= 1 per %g feat rows)"
                            % (name, n, nf, strong, rows_per_assert)) + loc)
    return out


def lint_constant_arms(root, added):
    """Production RPC handler arms added in the diff that return a constant without touching state."""
    max_const = int(os.environ.get("EVOMU_MAX_CONST_ARMS", "5"))
    found = []
    for fpath, lset in added.items():
        if not fpath.endswith(".rs") or "/src/" not in fpath or "/tests/" in fpath or fpath.endswith("tests.rs"):
            continue
        full = os.path.join(root, fpath)
        if not os.path.exists(full):
            continue
        try:
            lines = open(full, errors="replace").read().splitlines()
        except OSError:
            continue
        starts = [i for i, l in enumerate(lines) if ARM_RE.match(l)
                  and not (i > 0 and ARM_RE.match(lines[i - 1]) and "=>" not in lines[i - 1])]
        for k, s in enumerate(starts):
            if lset is not None and (s + 1) not in lset:
                continue
            nxt = starts[k + 1] if k + 1 < len(starts) else min(len(lines), s + 40)
            arm = lines[s:min(nxt, s + 40)]
            methods = []
            j = 0
            while j < len(arm):
                methods += ["%s.%s" % m.groups() for m in [ARM_RE.match(arm[j])] if m]
                if "=>" in arm[j]:
                    break
                j += 1
            body = "\n".join(arm[j:])
            body_wo_auth = body.replace("ServerContext::require_character(session)", "")
            if "const-ok:" in "\n".join(arm) or not CONST_RET_RE.search(body):
                continue
            if STATE_RE.search(body_wo_auth):
                continue
            if len([l for l in arm[j:] if l.strip()]) > 14:
                continue
            found.append((fpath, s + 1, methods))
    total = sum(len(m) for _, _, m in found)
    if total < max_const:
        return []
    return [("CONSTANT_RPC_HANDLER",
             "%s return a constant without reading or writing state (%d such methods in this diff; max %d). "
             "Implement the semantics, return UnimplementedRPC, or justify with `// const-ok: <reason>`"
             % (", ".join(m[:4]) + (" +%d" % (len(m) - 4) if len(m) > 4 else ""), total, max_const), f, ln)
            for f, ln, m in found]


def timing_line(root):
    try:
        recs = [json.loads(l) for l in open(history_path(root)) if l.strip()]
    except (OSError, ValueError):
        return "(no timing recorded)"
    mine = [r for r in recs if r.get("worktree") == os.path.basename(root)]
    if not mine:
        return "(no timing recorded)"
    r = mine[-1]
    return "cargo test wall %ss (build %ss, test run sum %ss over %d binaries) on %s@%s" % (
        r["wall_s"], r["build_s"], r["run_sum_s"], r["binaries"], r["branch"], r["head"])


def history_path(root):
    """One preflight timing history shared by every worktree: <main checkout>/.evomu-reviews/preflight_history.jsonl."""
    common = subprocess.run(["git", "rev-parse", "--git-common-dir"], cwd=root, capture_output=True, text=True).stdout.strip()
    main_root = os.path.dirname(os.path.abspath(os.path.join(root, common))) if common else root
    d = os.path.join(main_root, rl.STATE_DIRNAME)
    os.makedirs(d, exist_ok=True)
    return os.path.join(d, "preflight_history.jsonl")


def record_timing(root, wall, out):
    m = re.search(r"Finished `test` profile[^\n]* in (?:(\d+)m )?([\d.]+)s", out)
    build = (int(m.group(1) or 0) * 60 + float(m.group(2))) if m else None
    res = re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed;.*?finished in ([\d.]+)s", out)
    branch = subprocess.run(["git", "branch", "--show-current"], cwd=root, capture_output=True, text=True).stdout.strip()
    head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=root, capture_output=True, text=True).stdout.strip()
    rec = {"ts": datetime.datetime.now().isoformat(timespec="seconds"), "worktree": os.path.basename(root),
           "branch": branch, "head": head, "wall_s": round(wall, 1), "build_s": build,
           "run_sum_s": round(sum(float(c) for *_, c in res), 1), "binaries": len(res),
           "passed": sum(int(a) for a, _, _ in res), "failed": sum(int(b) for _, b, _ in res)}
    try:
        with open(history_path(root), "a") as fh:
            fh.write(json.dumps(rec) + "\n")
    except OSError:
        pass
    return rec


def lint_new_test_files(root, base):
    """Once eve-server has a single integration-test binary (tests/it/main.rs), new standalone files under
    crates/eve-server/tests/*.rs would each relink the whole server again. Block them."""
    if not os.path.exists(os.path.join(root, "crates/eve-server/tests/it/main.rs")):
        return []
    new = set()
    if base:
        new |= set(sh(["git", "diff", "--name-only", "--diff-filter=A", base], root)[1].split())
    new |= set(sh(["git", "ls-files", "--others", "--exclude-standard"], root)[1].split())
    pat = re.compile(r"^crates/eve-server/tests/[^/]+\.rs$")
    return [("NEW_STANDALONE_TEST_BINARY",
             "%s adds a separate test binary that relinks eve_server; put it in crates/eve-server/tests/it/ and "
             "add `mod` to tests/it/main.rs" % f, f, 1) for f in sorted(new) if pat.match(f)]


def lint_merge_conflicts_and_docs(root, added):
    """Block unresolved git merge conflict markers in tracked files and self-contradictory S-10 counts in PLAN.md/README.md."""
    out = []
    check_files = set(added.keys()) | {"PLAN.md", "README.md"}
    conflict_re = re.compile(r"^(?:<{7}\s|={7}\s*$|>{7}\s)")
    for fpath in sorted(check_files):
        full = os.path.join(root, fpath)
        if not os.path.isfile(full):
            continue
        try:
            lines = open(full, errors="replace").read().splitlines()
        except OSError:
            continue
        for idx, ln in enumerate(lines, 1):
            if conflict_re.match(ln):
                out.append(("MERGE_CONFLICT_MARKER",
                            "Unresolved git merge conflict marker '%s' in %s:%d" % (ln[:24], fpath, idx),
                            fpath, idx))
    # Check PLAN.md and README.md for contradictory explicit-service or match-arm counts across lines
    for doc in ("PLAN.md", "README.md"):
        full = os.path.join(root, doc)
        if not os.path.isfile(full):
            continue
        try:
            lines = open(full, errors="replace").read().splitlines()
        except OSError:
            continue
        arms_seen = {}
        for idx, ln in enumerate(lines, 1):
            for m in re.finditer(r"\b(\d+)\s+total\s+`?\(service,\s*method\)`?\s+handlers\s+across\s+`?\*?\*?(\d+)\*?\*?`?\s+match\s+arms", ln):
                arms_seen.setdefault((m.group(1), m.group(2)), []).append(idx)
        if len(arms_seen) > 1:
            desc = ", ".join("%s handlers/%s arms at L%s" % (k[0], k[1], v[0]) for k, v in sorted(arms_seen.items()))
            first_ln = min(v[0] for v in arms_seen.values())
            out.append(("S10_DOC_COUNT_MISMATCH",
                        "%s states contradictory (service, method) handler / match-arm counts across lines: %s" % (doc, desc),
                        doc, first_ln))
    return out


def run_early_gate(root, base, added, claims_path):
    violations = []

    # 0. Merge conflict markers and contradictory S-10 doc counts
    violations += lint_merge_conflicts_and_docs(root, added)

    # 1. RPC-name check (names must exist in the real client)
    all_viol, have_client = rpc_names.run()
    hard_all = [v for v in all_viol if v["kind"] == "NOT_IN_CLIENT"]
    rpc_block = [v for v in hard_all if v["file"] in added and (added[v["file"]] is None or v["line"] in added[v["file"]])]
    for v in rpc_block:
        violations.append(("NOT_IN_CLIENT_RPC", "Unknown/invented RPC name '%s' in %s" % (v["name"], v["what"]),
                           v["file"], v["line"]))

    # 2. Hollow Assertion & Metric Gaming anti-patterns
    hollow_patterns = [
        (re.compile(r"fn\s+check_handled\b|!e\.code\S*contains\(\"unimplemented\"\)|\bcheck_handled\s*\(&"),
         "Hollow error filter accepts arbitrary runtime errors as passes"),
        (re.compile(r"status_code\s*==\s*(?:200|201)\s*\|\|.*(?:400|401|403|404|500)"),
         "Permissive status code assertion accepts client/server error codes as success"),
    ]

    for fpath, lset in added.items():
        if not fpath.endswith(".rs") or "/tests/" not in fpath:
            continue
        full_path = os.path.join(root, fpath)
        if not os.path.exists(full_path):
            continue
        try:
            flines = open(full_path, errors="replace").read().splitlines()
        except OSError:
            continue
        is_descriptor_test = "descriptor" in fpath.lower()
        for idx, line in enumerate(flines, 1):
            if lset is not None and idx not in lset:
                continue
            for pat, desc in hollow_patterns:
                if pat.search(line):
                    violations.append(("HOLLOW_ASSERTION", desc, fpath, idx))
            if is_descriptor_test and ("// parity: quasar:" in line or ("attest" in line and "quasar:" in line)):
                violations.append(("QUASAR_SWEEP_GATE",
                                   "quasar:* rows require live public gateway backend handlers; descriptor sweeps cannot close them",
                                   fpath, idx))

    # 2b. Tagged tests: too many rows per test, or too few substantive assertions per row
    violations += lint_tagged_tests(root, added)

    # 2c. Constant-return RPC handler arms added in production code
    violations += lint_constant_arms(root, added)

    # 2d. New standalone eve-server test binaries after the single-binary refactor
    violations += lint_new_test_files(root, base)

    # 3. Claims citations & quotes check (if draft claims provided)
    if claims_path and os.path.exists(claims_path):
        claims_text = open(claims_path, errors="replace").read()
        refs = CITE_RE.findall(claims_text)
        for path, spec in refs:
            full = os.path.join(root, path)
            if not os.path.exists(full):
                violations.append(("CLAIMS_CITED_PATH_MISSING", "Cited path does not exist: %s" % path, None, None))
            else:
                try:
                    num_lines = len(open(full, errors="replace").read().splitlines())
                    for a, hi in cite_ranges(spec):
                        if a < 1 or hi > num_lines or hi < a:
                            violations.append(("CLAIMS_LINE_OUT_OF_RANGE",
                                               "Line range %d-%d out of range in %s (has %d lines)" % (a, hi, path, num_lines),
                                               path, a))
                except OSError:
                    pass
        for q in re.findall(r"[\"“]([^\"”\n]{12,200})[\"”]", claims_text):
            if ROW_RE.search(q):
                continue
            rc, _ = sh(["git", "grep", "-F", "-q", "--untracked", "-e", q, "--", ".", ":(exclude).evomu-reviews"], root)
            if rc != 0:
                violations.append(("CLAIMS_FABRICATED_QUOTE", "Quote not found verbatim anywhere in repo: \"%s\"" % q[:80],
                                   None, None))

    return rpc_block, hard_all, all_viol, have_client, violations


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--claims", help="draft claims file to lint")
    ap.add_argument("--no-tests", action="store_true", help="reuse the last pre-flight test output")
    ap.add_argument("--no-status", action="store_true", help="don't regenerate docs/ACTION_PLAN_STATUS.md")
    ap.add_argument("--skip-early-fail", action="store_true",
                    help="Warn but do not exit early on P0 static lint failures (also EVOMU_PREFLIGHT_SKIP_EARLY_FAIL=1)")
    ap.add_argument("--early-only", action="store_true",
                    help="Run only the early static lint checks in <1s and exit immediately")
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

    # Early Sanity Gate: detect P0 violations in < 1 second BEFORE running 30+ minutes of cargo test
    base = find_base(root)
    added = collect_added_lines(root, base)
    rpc_block, hard_all, all_viol, have_client, early_violations = run_early_gate(root, base, added, a.claims)
    skip_early = a.skip_early_fail or bool(os.environ.get("EVOMU_PREFLIGHT_SKIP_EARLY_FAIL"))

    if early_violations:
        print("\n" + "=" * 76)
        print("[preflight] %d BLOCKING P0 ISSUE(S) DETECTED IN DIFF (BEFORE RUNNING TESTS):" % len(early_violations))
        by_cat = {}
        for v in early_violations:
            by_cat[v[0]] = by_cat.get(v[0], 0) + 1
        print("  by category: " + ", ".join("%s=%d" % kv for kv in sorted(by_cat.items())))
        full_list = os.path.join(pdir, "early_gate.txt")
        with open(full_list, "w", encoding="utf-8") as fh:
            for cat, vmsg, fpath, lno in early_violations:
                fh.write("[%s] %s%s\n" % (cat, vmsg, (" (%s:%s)" % (fpath, lno)) if fpath else ""))
        for cat, vmsg, fpath, lno in early_violations[:60]:
            loc = (" (%s:%s)" % (fpath, lno)) if fpath else ""
            print("  - [%s] %s%s" % (cat, vmsg, loc))
        if len(early_violations) > 60:
            print("  ... %d more; full list: %s" % (len(early_violations) - 60, full_list))
        print("=" * 76)
        if not skip_early:
            print("\n[preflight] EARLY FAIL: Aborting test execution to save 30-60 minutes.")
            print("            Fix the issues above, or rerun with --skip-early-fail")
            print("            (or EVOMU_PREFLIGHT_SKIP_EARLY_FAIL=1) to run the full test suite anyway.\n")
            return 1
        else:
            print("\n[preflight] WARNING: Early fail overridden by --skip-early-fail. Proceeding to tests...\n")
    elif a.early_only:
        print("[preflight] Early static checks: CLEAN (0 P0 issues found in diff).")
        return 0

    if not a.no_tests:
        shutil.rmtree(attest, ignore_errors=True)
        os.makedirs(attest, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if k not in WRAPPER_ENV}
        env["CARGO_TARGET_DIR"] = os.path.join(sdir, "target")
        env["EVOMU_PARITY_ATTEST_DIR"] = attest
        cargo = shutil.which("cargo", path=env.get("PATH")) or "/opt/homebrew/bin/cargo"
        print("[preflight] cargo test --workspace --no-fail-fast ...", flush=True)
        t0 = time.time()
        rc, out = sh([cargo, "test", "--workspace", "--no-fail-fast"], root, env=env, timeout=a.test_timeout)
        wall = time.time() - t0
        open(tests_txt, "w", encoding="utf-8").write("exit=%d\n%s" % (rc, out))
        record_timing(root, wall, out)
    out = open(tests_txt, errors="replace").read() if os.path.exists(tests_txt) else ""
    passed = sum(int(m) for m in re.findall(r"test result: \w+\. (\d+) passed", out))
    failed = sum(int(m) for m in re.findall(r"test result: \w+\. \d+ passed; (\d+) failed", out))
    ignored = sum(int(m) for m in re.findall(r"test result: \w+\. \d+ passed; \d+ failed; (\d+) ignored", out))
    failing = re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M)
    compile_err = "error[E" in out or "could not compile" in out
    m_exit = re.match(r"exit=(-?\d+)", out)
    test_exit = int(m_exit.group(1)) if m_exit else 0
    aborted = re.findall(r"(?:process didn't exit successfully|stack overflow|SIGABRT|SIGSEGV|signal: \d+)[^\n]*", out)

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
         "- Tests: **%d passed, %d failed, %d ignored**, cargo exit %d%s%s" % (
             passed, failed, ignored, test_exit, " (COMPILE ERROR)" if compile_err else "",
             " (A TEST BINARY ABORTED: some tests never ran: %s)" % aborted[0][:160] if aborted else ""),
         "- Parity: **%d / %d rows closed (%.2f%%)**, %d NA, %d tagged tests" % (
             summ["closed_rows"], summ["total_rows"], summ["percent_closed"], summ["na_rows"], summ["tagged_tests"]),
         "- Test loop: %s" % timing_line(root),
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
    L += ["", "## RPC-name check (names must exist in the real V24 client)",
          "- Repo-wide: **%d NOT_IN_CLIENT**, %d NOT_IN_CATALOG (informational)%s" % (
              len(hard_all), len(all_viol) - len(hard_all),
              "" if have_client else " [WARNING: client code.ccp not found; catalog-only]"),
          "- On lines you added or changed since the last review (blocking): **%d**" % len(rpc_block)]
    for v in rpc_block[:80]:
        L.append("  - `%s:%d` %s `%s`" % (v["file"], v["line"], v["what"], v["name"]))
    report = "\n".join(L)
    open(os.path.join(pdir, "claims_template.md"), "w", encoding="utf-8").write(report)
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
    if test_exit != 0 or aborted:
        print("\n# cargo test exited %d%s. A non-zero exit means tests failed or a test binary aborted, so some tests "
              "never ran. Don't claim tests pass." % (test_exit, " (%s)" % aborted[0][:160] if aborted else ""))
    ok = (failed == 0 and test_exit == 0 and not aborted and not compile_err and passed > 0 and lint_ok
          and not rpc_block)
    print("[preflight] %s" % ("OK: run the review now." if ok else "NOT READY: fix the items above before spending a review."))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
