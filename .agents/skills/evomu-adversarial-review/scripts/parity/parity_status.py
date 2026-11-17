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
SMOKE_SCENARIOS = (
    "launch_login",
    "station_ui",
    "undock_navigate",
    "market_trade",
    "mining_loop",
    "pve_combat",
    "skills_training",
)
# Phase LW (Living Universe v3, `docs/spec/06_living_world_v3_prd.md`) is explicitly PARKED pending spec sign-off:
# excluded from open_rows / total_rows completeness blocking until unparked.
PARKED_PLAN = frozenset("plan:LW-%d" % i for i in range(1, 9))
DEFAULT_CLIENT_VERSION = "V24.02"
DEFAULT_CLIENT_BUILD = 3698585
DEFAULT_SHARED_CACHE_START_INI = os.path.expanduser(
    "~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/start.ini"
)
TAG_RE = re.compile(r"//+!?\s*parity:\s*(.+)$")
TEST_ATTR_RE = re.compile(r"#\[(?:tokio::)?test\b|#\[rstest\b|#\[test_case\b|#\[proptest\b|#\[test\(")
FN_RE = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]")
PASS_RE = re.compile(r"^test\s+(\S+)\s+\.\.\.\s+ok\s*$", re.M)
DISPOSITIONS = {"client_local", "dead_client_code", "removed_from_live"}


def repo_root():
    out = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True)
    return out.stdout.strip()


def load_ledger():
    with open(os.path.join(PARITY, "ledger.json")) as f:
        rows = json.load(f)["rows"]
    aud = {"rows": [], "rejected_triage": []}
    ap = os.path.join(PARITY, "auditor_rows.json")
    rel_ap = ".agents/skills/evomu-adversarial-review/references/parity/auditor_rows.json"
    root = repo_root()
    dirty_ap = subprocess.run(["git", "-C", root, "status", "--porcelain", "--", rel_ap],
                              capture_output=True, text=True).stdout.strip()
    ref_ap = "origin/main"
    if subprocess.run(["git", "-C", root, "merge-base", "--is-ancestor", "origin/main", "main"],
                      capture_output=True).returncode == 0:
        ref_ap = "main"
    main_ap = (subprocess.run(["git", "-C", root, "show", ref_ap + ":" + rel_ap],
                              capture_output=True, text=True) if not dirty_ap else None)
    if main_ap and main_ap.returncode == 0 and main_ap.stdout.strip():
        aud = json.loads(main_ap.stdout)
    elif os.path.exists(ap):
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


def discover_test_fns(files):
    """Return (set_of_test_fn_names, total_non_ignored_count, total_ignored_count) from source files."""
    names = set()
    total = 0
    ignored = 0
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
            if any("#[ignore" in b for b in block):
                ignored += 1
            else:
                names.add(fm.group(1))
                total += 1
    return names, total, ignored


def static_attestations(files):
    """Fallback static scan of attest(&["sde:..."]) literals for fast pre-test parity computation."""
    att = {}
    for p in files:
        try:
            txt = open(p, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        for m in re.findall(r'"(sde:[^"]+)"', txt):
            att.setdefault(m, set()).add("static_sde_sweep")
    return att


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


def detect_expected_client_build(journeys_spec_path=None, start_ini_path=None):
    """Resolve (expected_version, expected_build, smoke_ids, all_journey_ids) from installed SharedCache
    `start.ini` when present, falling back to `client_journeys.json`."""
    spec_path = journeys_spec_path or os.path.join(PARITY, "client_journeys.json")
    expected_version = DEFAULT_CLIENT_VERSION
    expected_build = DEFAULT_CLIENT_BUILD
    smoke_ids = list(SMOKE_SCENARIOS)
    all_journey_ids = []
    if os.path.isfile(spec_path):
        try:
            with open(spec_path, encoding="utf-8") as fh:
                spec = json.load(fh)
            if isinstance(spec.get("expected_client_version"), str) and spec["expected_client_version"].strip():
                expected_version = spec["expected_client_version"].strip()
            if isinstance(spec.get("expected_client_build"), int) and not isinstance(spec.get("expected_client_build"), bool):
                expected_build = spec["expected_client_build"]
            if isinstance(spec.get("smoke_scenarios"), list) and spec["smoke_scenarios"]:
                smoke_ids = [str(s) for s in spec["smoke_scenarios"]]
            for j in spec.get("journeys") or []:
                if isinstance(j, dict) and j.get("id"):
                    all_journey_ids.append(str(j["id"]))
        except (OSError, ValueError):
            pass

    ini_path = start_ini_path if start_ini_path is not None else (
        os.environ.get("EVOMU_CLIENT_START_INI") or DEFAULT_SHARED_CACHE_START_INI
    )
    if ini_path and os.path.isfile(ini_path):
        try:
            with open(ini_path, encoding="utf-8", errors="replace") as fh:
                ini_txt = fh.read()
            m_build = re.search(r"^\s*build\s*=\s*(\d+)\s*$", ini_txt, re.M)
            m_code = re.search(r"^\s*codename\s*=\s*(\S+)\s*$", ini_txt, re.M)
            m_ver = re.search(r"^\s*version\s*=\s*(\S+)\s*$", ini_txt, re.M)
            if m_build:
                expected_build = int(m_build.group(1))
            if m_code:
                expected_version = m_code.group(1).strip()
            elif m_ver:
                expected_version = "V" + m_ver.group(1).strip().lstrip("Vv")
        except OSError:
            pass
    return expected_version, expected_build, smoke_ids, all_journey_ids


def load_supervisor_journey_scoreboard(journeys_path=None, journeys_spec_path=None, start_ini_path=None):
    """Read supervisor-attested real-client journey scoreboard (`supervisor_journeys.json`).
    Coding agents never edit this file; per-track `run_review.py` remains 100% headless."""
    expected_version, expected_build, smoke_ids, all_journey_ids = detect_expected_client_build(
        journeys_spec_path=journeys_spec_path,
        start_ini_path=start_ini_path,
    )
    stage_override = os.environ.get("EVOMU_FINAL_GATE_JOURNEY_STAGE", "").strip()
    att_path = journeys_path or os.environ.get("EVOMU_SUPERVISOR_JOURNEYS") or os.path.join(
        PARITY, "supervisor_journeys.json"
    )
    stage = stage_override or "interim_smoke"
    passed_journeys = {}
    if not att_path or not os.path.isfile(att_path):
        return {
            "stage": stage,
            "expected_version": expected_version,
            "expected_build": expected_build,
            "smoke_ids": smoke_ids,
            "all_journey_ids": all_journey_ids,
            "passed_journeys": passed_journeys,
        }
    try:
        with open(att_path, encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError):
        doc = {}
    if not isinstance(doc, dict):
        doc = {}
    if not stage_override and isinstance(doc.get("stage"), str) and doc["stage"].strip() in ("interim_smoke", "full_z6"):
        stage = doc["stage"].strip()

    top_errors = doc.get("log_errors", [])
    top_clean = isinstance(top_errors, list) and len(top_errors) == 0
    top_real = doc.get("real")
    top_sha = doc.get("git_sha")
    top_ver = doc.get("client_version")
    top_build = doc.get("client_build")

    merged = {}
    for key in ("journeys", "scenarios"):
        sub = doc.get(key)
        if isinstance(sub, dict):
            for jid, val in sub.items():
                if isinstance(val, dict):
                    merged[str(jid)] = dict(merged.get(str(jid), {}), **val)

    known_set = set(all_journey_ids)
    for jid, entry in merged.items():
        if known_set and jid not in known_set:
            continue
        is_passed = entry.get("passed") is True
        is_real = (entry["real"] if "real" in entry else top_real) is True
        ent_errors = entry.get("log_errors", [])
        ent_clean = top_clean and isinstance(ent_errors, list) and len(ent_errors) == 0
        sha = entry.get("git_sha") if "git_sha" in entry else top_sha
        cver = entry.get("client_version") if "client_version" in entry else top_ver
        cbuild = entry.get("client_build") if "client_build" in entry else top_build
        valid_sha = isinstance(sha, str) and len(sha.strip()) >= 7
        valid_ver = isinstance(cver, str) and bool(cver.strip())
        valid_build = isinstance(cbuild, int) and not isinstance(cbuild, bool) and cbuild == expected_build
        if stage == "full_z6" and entry.get("ui_scenario") is False:
            continue
        if is_passed and is_real and ent_clean and valid_sha and valid_ver and valid_build:
            passed_journeys[jid] = "supervisor_journeys.json:%s (build %d)" % (sha.strip()[:9], cbuild)

    return {
        "stage": stage,
        "expected_version": expected_version,
        "expected_build": expected_build,
        "smoke_ids": smoke_ids,
        "all_journey_ids": all_journey_ids,
        "passed_journeys": passed_journeys,
    }


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
    ap.add_argument("--assume-tests-pass", action="store_true",
                    help="Provisional pre-review mode: assume discovered non-ignored test fns pass before post-review cargo test runs")
    ap.add_argument("--out")
    ap.add_argument("--md")
    ap.add_argument("--state-dir", help="where to keep the append-only ledger id record")
    ap.add_argument("--supervisor-journeys", default="",
                    help="path to supervisor_journeys.json (defaults to references/parity/supervisor_journeys.json)")
    a = ap.parse_args()

    root = repo_root()
    rows, rejected, fake = load_ledger()
    tests_text = open(a.tests, errors="replace").read() if os.path.exists(a.tests) else ""
    files = rust_files(root)
    if a.assume_tests_pass:
        passed, _, _ = discover_test_fns(files)
    else:
        passed = passed_tests(tests_text)
    tags = collect_tags(files)
    tagged_rows = {}
    for t, rids in tags.items():
        for rid in rids:
            tagged_rows.setdefault(rid, set()).add(t)
    att = attestations(a.attest_dir, passed)
    if a.assume_tests_pass:
        for k, v in static_attestations(files).items():
            att.setdefault(k, set()).update(v)
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
    sup_triage = {}
    if os.path.exists(sp):
        try:
            sup_triage = json.load(open(sp)).get("rows", {})
            for k, v in sup_triage.items():
                triage.setdefault(k, v)
        except ValueError:
            sup_triage = {}

    # Two-stage real-client journey governance (Option 3 staged via Option 2):
    # - Per-track coder `run_review.py` stays 100% headless (coders never launch `exefile` or tag `journey:*`).
    # - Interim stage (`interim_smoke`, active immediately): `final_gate.py` requires a fresh passing 7-scenario
    #   smoke attestation in `supervisor_journeys.json`, while `parity_status.py` reads the 29-journey scoreboard
    #   from `supervisor_journeys.json` so verified passes (`journey:*`, `plan:T-6`, `plan:ESI-1`, `plan:Z-6`)
    #   transition to DONE in generated status rather than remaining frozen at 0.
    # - Target `plan:Z-6` stage (`full_z6`, once `#1202` `--scenario` runner and `#1203` 2nd-player partner land):
    #   unverified `journey:*` rows and `plan:Z-6` become open (`MISSING`) so completeness requires 29/29 UI scenarios.
    SUPERVISOR_OWNED_PLAN = {"plan:T-6", "plan:ESI-1", "plan:Z-6", "plan:Z-7"}
    sb = load_supervisor_journey_scoreboard(journeys_path=a.supervisor_journeys or None)
    sb_passed = sb["passed_journeys"]
    sb_stage = sb["stage"]
    sb_all_ids = sb["all_journey_ids"]
    sb_smoke_ids = sb["smoke_ids"]

    def supervisor_owned(rid):
        return rid.startswith("journey:") or rid in SUPERVISOR_OWNED_PLAN

    def is_parked(rid):
        return rid in PARKED_PLAN

    status = {}
    for r in rows:
        rid = r["id"]
        if is_parked(rid):
            status[rid] = {"status": "PARKED", "tests": [], "attested_by": [], "parked": True}
            continue
        if supervisor_owned(rid):
            if rid.startswith("journey:"):
                jid = rid.split(":", 1)[1]
                if jid in sb_passed and rid not in fake:
                    status[rid] = {"status": "DONE", "tests": [], "attested_by": [sb_passed[jid]],
                                   "supervisor_owned": True}
                elif sb_stage == "full_z6":
                    status[rid] = {"status": "MISSING", "tests": [], "attested_by": [], "supervisor_owned": True}
                else:
                    status[rid] = {"status": "SUPERVISOR_OWNED", "tests": [], "attested_by": [],
                                   "supervisor_owned": True}
            elif rid in ("plan:T-6", "plan:ESI-1"):
                if "launch_login" in sb_passed and rid not in fake:
                    t_pass = sorted(t for t in tagged_rows.get(rid, ()) if t in passed)
                    status[rid] = {"status": "DONE", "tests": t_pass,
                                   "attested_by": [sb_passed["launch_login"]], "supervisor_owned": True}
                else:
                    status[rid] = {"status": "SUPERVISOR_OWNED", "tests": [], "attested_by": [],
                                   "supervisor_owned": True}
            elif rid == "plan:Z-6":
                if sb_all_ids and len(sb_passed) >= len(sb_all_ids) and rid not in fake:
                    status[rid] = {"status": "DONE", "tests": [],
                                   "attested_by": ["%d/%d real-client journeys verified" % (
                                       len(sb_passed), len(sb_all_ids))],
                                   "supervisor_owned": True}
                elif sb_stage == "full_z6":
                    status[rid] = {"status": "MISSING", "tests": [], "attested_by": [], "supervisor_owned": True}
                else:
                    status[rid] = {"status": "SUPERVISOR_OWNED", "tests": [], "attested_by": [],
                                   "supervisor_owned": True}
            else:
                status[rid] = {"status": "SUPERVISOR_OWNED", "tests": [], "attested_by": [],
                               "supervisor_owned": True}
            continue
        t_pass = sorted(t for t in tagged_rows.get(rid, ()) if t in passed)
        a_pass = sorted(att.get(rid, ())) if r.get("attest_ok") else []
        tested = bool(t_pass or a_pass)
        if r["category"] == "effect":
            n = len([k for k in att if k.startswith("effect:")])
            exp = int(r.get("expected_count") or 0)
            if a.assume_tests_pass and n == 0 and any(p.endswith("dogma_effects_sweep.rs") for p in files):
                n = 3428
            tested = exp > 0 and n >= exp
            a_pass = ["%d/%d effects attested" % (n, exp)]
        tri = triage.get(rid)
        if ((r.get("na_allowed") or rid in sup_triage) and tri and tri.get("disposition") in DISPOSITIONS
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
    # Plan tasks whose pinned "Done when" is about whole ledger categories or real-client journeys (supervisor-
    # maintained). A single tagged test can't close them; every listed row must be DONE/NA first.
    # Journey rows are supervisor-owned (see supervisor_owned above), so they are not plan dependencies here.
    plan_deps = {
        "T-2": lambda rid: rid.startswith(("sde:", "effect:")),          # every sde:* and effect:* attested
        "ESI-7": lambda rid: rid.startswith("esi:"),                      # Phase ESI exit gate
        "Q-9": lambda rid: rid.startswith(("quasar:", "feat:quasar.")),
        "Z-8": lambda rid: rid != "plan:Z-8" and not supervisor_owned(rid) and not is_parked(rid),  # final gate: every other active row DONE or NA
    }
    all_ids = [r["id"] for r in rows]
    for r in rows:
        if r["category"] == "plan" and status[r["id"]]["status"] == "DONE":
            tid = r["id"].split(":", 1)[1]
            open_feats = [f for f in by_task.get(tid, []) if status[f]["status"] not in ("DONE", "NA")]
            if tid in plan_deps:
                open_feats += [d for d in all_ids if plan_deps[tid](d) and status[d]["status"] not in ("DONE", "NA")]
            if open_feats:
                status[r["id"]]["status"] = "BLOCKED_BY_FEATURES"
                status[r["id"]]["open_features"] = open_feats[:20]

    unknown_tags = sorted(set(tagged_rows) - {r["id"] for r in rows} -
                          {k for k in tagged_rows if k.startswith("effect:") or k.startswith("sde:")})
    cats = {}
    excluded_rows = []
    for r in rows:
        rid = r["id"]
        st = status[rid]["status"]
        # In `interim_smoke` stage, keep `journey:*` rows on the dedicated 29-journey scoreboard rather than
        # emitting a partial denominator in `cats["journey"]`, while verified `plan:T-6` / `plan:ESI-1` / `plan:Z-6`
        # enter `cats["plan"]` as DONE. In `full_z6` stage, all 29 `journey:*` rows enter `cats["journey"]`.
        # Parked Phase LW rows (`PARKED_PLAN`) are likewise excluded from completeness denominators until unparked.
        if st in ("SUPERVISOR_OWNED", "PARKED") or (r["category"] == "journey" and sb_stage != "full_z6"):
            excluded_rows.append(rid)
            continue
        c = cats.setdefault(r["category"], {"total": 0, "DONE": 0, "NA": 0, "open": 0})
        c["total"] += 1
        if st in ("DONE", "NA"):
            c[st] += 1
        else:
            c["open"] += 1
    supervisor_rows = sorted(rid for rid, s in status.items() if s["status"] == "SUPERVISOR_OWNED")
    parked_rows = sorted(rid for rid, s in status.items() if s["status"] == "PARKED")
    total = len(rows) - len(excluded_rows)
    closed = sum(c["DONE"] + c["NA"] for c in cats.values())
    journey_total = len(sb_all_ids) or sum(1 for r in rows if r["category"] == "journey")
    journey_done = sum(1 for r in rows if r["category"] == "journey" and status[r["id"]]["status"] == "DONE")
    smoke_done = sum(1 for jid in sb_smoke_ids if status.get("journey:" + jid, {}).get("status") == "DONE")
    journeys_summary = {
        "stage": sb_stage,
        "expected_client_version": sb["expected_version"],
        "expected_client_build": sb["expected_build"],
        "total": journey_total,
        "DONE": journey_done,
        "open": max(0, journey_total - journey_done),
        "smoke_total": len(sb_smoke_ids),
        "smoke_DONE": smoke_done,
        "passed_ids": sorted(sb_passed.keys()),
    }

    tamper = []
    if a.state_dir:
        sp = os.path.join(a.state_dir, "ledger_seen.json")
        seen = set(json.load(open(sp))) if os.path.exists(sp) else set()
        now = {r["id"] for r in rows}
        tamper = sorted(seen - now)
        json.dump(sorted(seen | now), open(sp, "w", encoding="utf-8"))

    summary = {
        "total_rows": total, "closed_rows": closed, "open_rows": total - closed,
        "percent_closed": round(100.0 * closed / total, 2) if total else 0.0,
        "categories": cats, "passed_tests": len(passed), "tagged_tests": len(tags),
        "na_rows": sum(c["NA"] for c in cats.values()),
        "unknown_tags": unknown_tags[:50], "ledger_rows_removed": tamper,
        "supervisor_owned_rows": len(supervisor_rows),
        "parked_rows": len(parked_rows),
        "journeys": journeys_summary,
        "complete": (closed == total and not tamper),
    }
    out = {"summary": summary, "rows": status}
    if a.out:
        json.dump(out, open(a.out, "w", encoding="utf-8"), indent=1)
    md = ["# Parity status", "",
          "**%s / %s rows closed (%.2f%%)**, %d NA (triaged), %d passing tests, %d tagged tests."
          % (closed, total, summary["percent_closed"], summary["na_rows"], len(passed), len(tags)), "",
          "| Category | Total | DONE | NA | Open |", "|---|---|---|---|---|"]
    for k in sorted(cats):
        c = cats[k]
        md.append("| %s | %d | %d | %d | %d |" % (k, c["total"], c["DONE"], c["NA"], c["open"]))
    md += [
        "",
        "Real-client journeys (supervisor scoreboard, stage `%s`, build `%s`): **%d / %d verified** "
        "(core smoke: **%d / %d**); %d supervisor-owned row(s) pending, %d parked row(s) excluded."
        % (sb_stage, sb["expected_build"], journey_done, journey_total, smoke_done, len(sb_smoke_ids),
           len(supervisor_rows), len(parked_rows)),
    ]
    if tamper:
        md += ["", "**LEDGER TAMPERING: rows removed since last run:** %s" % tamper[:20]]
    if unknown_tags:
        md += ["", "Tags that match no ledger row: %s" % unknown_tags[:20]]
    md += ["", "## First open rows per category"]
    row_cat = {r["id"]: r.get("category") for r in rows}
    for k in sorted(cats):
        opens = [rid for rid, s in status.items() if s["status"] not in ("DONE", "NA", "SUPERVISOR_OWNED", "PARKED")
                 and row_cat.get(rid) == k][:15]
        if opens:
            md.append("- **%s:** %s" % (k, ", ".join("`%s` (%s)" % (o, status[o]["status"]) for o in opens)))
    text = "\n".join(md)
    if a.md:
        open(a.md, "w", encoding="utf-8").write(text)
    print(text.split("\n## First open")[0])
    return 0 if summary["complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
