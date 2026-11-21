#!/usr/bin/python3
"""Auto-tighten per-file v2 ratchet baselines and dump canonical scanner counts (#1538).

Usage:
  tighten_ratchets.py [--root DIR] --check              # exit 0 if tight, 1 if slack exists, 2 on regression
  tighten_ratchets.py [--root DIR] --write              # strictly lower v2 baselines in place (never raises!)
  tighten_ratchets.py [--root DIR] --dump-counts-json   # print exact per-file scanner counts for Rust parity test
  tighten_ratchets.py [--root DIR] --migrate-v2 [--only arch_debt,client_compat,untyped_user_error,stringly_error_bypass,test_fixture]
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import ratchet_checks as rc  # noqa: E402

ARCH_DEBT_REL = "crates/eve-testkit/ratchets/arch_debt_ratchet.json"
CLIENT_COMPAT_REL = "crates/eve-testkit/ratchets/client_compat_ratchet.json"
UNTYPED_UE_REL = rc.UNTYPED_USER_ERROR_REL
STRINGLY_BYPASS_REL = rc.STRINGLY_ERR_BYPASS_REL
TEST_FIXTURE_REL = rc.TEST_FIXTURE_REL

ALL_TARGETS = ("arch_debt", "client_compat", "untyped_user_error", "stringly_error_bypass", "test_fixture")


def _dump_json(obj):
    return json.dumps(obj, indent=2) + "\n"


def _write_if_changed(path, obj):
    new_text = _dump_json(obj)
    old_text = None
    if os.path.isfile(path):
        with open(path, encoding="utf-8") as fh:
            old_text = fh.read()
    if old_text == new_text:
        return False
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(new_text)
    return True


def _tighten_file_map(label, base_map, obs_map, zero_required=False):
    """Return `(tightened_map, slack_count, regressions)` without ever raising a baseline entry."""
    regressions = []
    if zero_required:
        for rel_f, obs_c in sorted(obs_map.items()):
            if obs_c > 0:
                regressions.append("%s: zero_required rule regressed in %s (observed %d > 0)" % (label, rel_f, obs_c))
        slack = len(base_map)
        return {}, slack, regressions

    for rel_f, obs_c in sorted(obs_map.items()):
        if obs_c <= 0:
            continue
        base_c = int(base_map.get(rel_f, 0) or 0)
        if obs_c > base_c:
            regressions.append("%s: regressed in %s (observed %d > baseline %d)" % (label, rel_f, obs_c, base_c))

    tight = {}
    slack = 0
    for rel_f in sorted(base_map):
        base_c = int(base_map[rel_f] or 0)
        obs_c = int(obs_map.get(rel_f, 0) or 0)
        if obs_c > base_c:
            tight[rel_f] = base_c
        elif obs_c > 0:
            tight[rel_f] = obs_c
            if obs_c < base_c:
                slack += 1
        else:
            slack += 1
    return tight, slack, regressions


def _tighten_subset_list(label, allowed_list, observed_set):
    """Return `(tightened_sorted_list, pruned_count, regressions)` without ever adding unlisted entries."""
    allowed_set = set(allowed_list)
    added = sorted(observed_set - allowed_set)
    regressions = []
    if added:
        regressions.append("%s: unlisted entries added (%s)" % (label, ", ".join(added[:10])))
    kept = sorted(allowed_set & observed_set)
    pruned = len(allowed_set) - len(kept)
    return kept, pruned, regressions


def collect_all_counts(root):
    """Scan the workspace once and return canonical per-file counts for all 5 ratchet families."""
    arch_path = os.path.join(root, ARCH_DEBT_REL)
    arch_counts = {}
    if os.path.isfile(arch_path):
        arch_doc = rc._read_json(arch_path)
        pf, _, viols, _, _ = rc.scan_arch_debt_per_file(root, arch_doc, rel_path=ARCH_DEBT_REL)
        if viols:
            raise ValueError("arch_debt scan errors: %s" % (viols,))
        arch_counts = {k: dict(sorted(v.items())) for k, v in sorted(pf.items())}

    shared_it_cache = {}
    shared_stat_it = {}
    compat_rep = rc.scan_client_compat_per_file(root, it_cache=shared_it_cache, stat_it=shared_stat_it)
    client_compat_counts = {
        "counters": compat_rep["counters"],
        "known_rules": compat_rep["known_rules"],
        "service_caller_trace_files": compat_rep["service_caller_trace_files"],
        "it_dispatch_rpc_files": compat_rep["it_dispatch_rpc_files"],
    }
    untyped_ue_counts = dict(sorted(rc.scan_untyped_user_error_per_file(root).items()))
    bypass_pf, _, _ = rc.scan_stringly_error_bypass_per_file(root)
    bypass_counts = {k: dict(sorted(v.items())) for k, v in sorted(bypass_pf.items())}
    test_fixture_pf = rc.scan_test_fixture_per_file(root, it_cache=shared_it_cache, stat_it=shared_stat_it)
    test_fixture_counts = {k: dict(sorted(test_fixture_pf.get(k, {}).items())) for k in rc.TEST_FIXTURE_PATTERNS}

    return {
        "arch_debt": arch_counts,
        "client_compat": client_compat_counts,
        "untyped_user_error": untyped_ue_counts,
        "stringly_error_bypass": bypass_counts,
        "test_fixture": test_fixture_counts,
    }


def _build_arch_debt_v2(old_doc, obs_by_rule):
    new_patterns = {}
    for name, spec in rc._iter_arch_debt_entries(old_doc):
        zero_req = bool(spec.get("zero_required") is True)
        entry = {"zero_required": zero_req}
        for opt_k in ("owning_issue", "regex", "strip_literals", "path_include", "path_exclude_substring", "description"):
            if opt_k in spec and spec[opt_k] is not None:
                entry[opt_k] = spec[opt_k]
        obs_pf = obs_by_rule.get(name, {})
        if zero_req and obs_pf:
            raise ValueError("Cannot migrate zero_required arch_debt rule '%s' with non-zero occurrences: %s" % (name, obs_pf))
        entry["files"] = {} if zero_req else dict(sorted(obs_pf.items()))
        new_patterns[name] = entry
    return {
        "version": 2,
        "description": (
            "Per-file falling architecture-debt ratchet across non-test crates/eve-server/src (#1197, #1538). "
            "Non-zero rules store positive per-file counts (missing file = 0) tightened mechanically after landing; "
            "zero_required rules stay strictly pinned at 0."
        ),
        "patterns": new_patterns,
    }


def _build_client_compat_v2(old_doc, compat_counts):
    out = {
        "version": 2,
        "counters": {k: dict(sorted(compat_counts["counters"].get(k, {}).items())) for k in rc.CLIENT_COMPAT_COUNTER_KEYS},
    }
    if old_doc.get("max_transport_differing_replies") is not None:
        out["max_transport_differing_replies"] = int(old_doc["max_transport_differing_replies"])
    out["known_rules"] = list(compat_counts["known_rules"])
    out["allowed_service_caller_trace_files"] = list(compat_counts["service_caller_trace_files"])
    out["allowed_it_dispatch_rpc_files"] = list(compat_counts["it_dispatch_rpc_files"])
    return out


def _build_untyped_ue_v2(ue_counts):
    return {
        "version": 2,
        "owning_issue": 929,
        "description": (
            "Per-file falling baseline for non-test BridgeRpcError::user_error( sites under crates/eve-server/src "
            "(#929, #1538). Missing file = 0; tightened mechanically after landing."
        ),
        "files": dict(sorted(ue_counts.items())),
    }


def _build_stringly_bypass_v2(old_doc, bypass_counts):
    old_rules = old_doc.get("rules") if isinstance(old_doc, dict) else {}
    new_rules = {}
    for k in rc.STRINGLY_ERR_BYPASS_KEYS:
        spec = old_rules.get(k, {}) if isinstance(old_rules, dict) else {}
        entry = {}
        if isinstance(spec, dict) and spec.get("description"):
            entry["description"] = spec["description"]
        entry["files"] = dict(sorted(bypass_counts.get(k, {}).items()))
        new_rules[k] = entry
    return {
        "version": 2,
        "owning_issue": int((old_doc or {}).get("owning_issue", 1195) or 1195),
        "description": (
            "Per-file companion bypass ratchet for #1195 stringly BridgeRpcError codes (TT-11 / #1538). "
            "Missing file = 0; tightened mechanically after landing."
        ),
        "rules": new_rules,
    }


def _build_test_fixture_v2(old_doc, tf_counts):
    old_patterns = old_doc.get("patterns") if isinstance(old_doc, dict) else {}
    default_rules = {
        "hand_built_sessions": (
            "No new hand-built SessionState { ... } struct literals in crates/eve-server/tests/it/; "
            "use ScenarioBuilder or login flows"
        ),
        "discarded_rpc_results": (
            "No new let _ = / let _name = RPC calls discarding results in crates/eve-server/tests/it/; "
            "handle or assert RPC outcomes explicitly"
        ),
    }
    new_patterns = {}
    for pat in rc.TEST_FIXTURE_PATTERNS:
        spec = old_patterns.get(pat, {}) if isinstance(old_patterns, dict) else {}
        zero_req = bool(isinstance(spec, dict) and spec.get("zero_required") is True)
        obs_pf = tf_counts.get(pat, {})
        if zero_req and obs_pf:
            raise ValueError(
                "Cannot migrate zero_required test_fixture pattern '%s' with non-zero occurrences: %s" % (pat, obs_pf)
            )
        new_patterns[pat] = {
            "rule": spec.get("rule", default_rules[pat]) if isinstance(spec, dict) else default_rules[pat],
            "zero_required": zero_req,
            "files": {} if zero_req else dict(sorted(obs_pf.items())),
        }
    return {
        "version": 2,
        "owning_issue": (old_doc or {}).get("owning_issue", "#1540") if isinstance(old_doc, dict) else "#1540",
        "description": (
            (old_doc or {}).get("description")
            if isinstance(old_doc, dict) and (old_doc or {}).get("description")
            else (
                "Per-file integration test hygiene baselines (#1540): block new hand-built "
                "SessionState { ... } structs and discarded let _ = RPC results in crates/eve-server/tests/it/."
            )
        ),
        "patterns": new_patterns,
    }


def tighten_ratchets(root, write=False, targets=ALL_TARGETS):
    """Inspect (and optionally rewrite) v2 per-file ratchet baselines strictly downward.

    Returns `(changed_files, slack_total, regressions)`. Never raises any baseline entry."""
    counts = collect_all_counts(root)
    changed_files = []
    slack_total = 0
    regressions = []

    if "arch_debt" in targets:
        p = os.path.join(root, ARCH_DEBT_REL)
        if os.path.isfile(p):
            doc = rc._read_json(p)
            if isinstance(doc, dict) and int(doc.get("version", 1) or 1) >= 2 and isinstance(doc.get("patterns"), dict):
                new_doc = dict(doc)
                new_patterns = {}
                file_slack = 0
                for name, spec in doc["patterns"].items():
                    if not isinstance(spec, dict) or not isinstance(spec.get("files"), dict):
                        new_patterns[name] = spec
                        continue
                    tight_pf, sl, regs = _tighten_file_map(
                        "arch_debt.%s" % name,
                        spec["files"],
                        counts["arch_debt"].get(name, {}),
                        zero_required=bool(spec.get("zero_required") is True),
                    )
                    file_slack += sl
                    regressions.extend(regs)
                    nspec = dict(spec)
                    nspec["files"] = tight_pf
                    new_patterns[name] = nspec
                new_doc["patterns"] = new_patterns
                slack_total += file_slack
                if file_slack > 0:
                    if write and not regressions:
                        if _write_if_changed(p, new_doc):
                            changed_files.append(ARCH_DEBT_REL)
                    elif not write:
                        changed_files.append(ARCH_DEBT_REL)

    if "client_compat" in targets:
        p = os.path.join(root, CLIENT_COMPAT_REL)
        if os.path.isfile(p):
            doc = rc._read_json(p)
            if isinstance(doc, dict) and int(doc.get("version", 1) or 1) >= 2 and isinstance(doc.get("counters"), dict):
                new_doc = dict(doc)
                new_counters = {}
                file_slack = 0
                obs_c = counts["client_compat"]["counters"]
                for cname in rc.CLIENT_COMPAT_COUNTER_KEYS:
                    bmap = doc["counters"].get(cname, {})
                    tight_pf, sl, regs = _tighten_file_map(
                        "client_compat.%s" % cname,
                        bmap if isinstance(bmap, dict) else {},
                        obs_c.get(cname, {}),
                        zero_required=(cname == "service_caller_trace_comparisons"),
                    )
                    file_slack += sl
                    regressions.extend(regs)
                    new_counters[cname] = tight_pf
                new_doc["counters"] = new_counters
                for field, obs_key in (
                    ("known_rules", "known_rules"),
                    ("allowed_service_caller_trace_files", "service_caller_trace_files"),
                    ("allowed_it_dispatch_rpc_files", "it_dispatch_rpc_files"),
                ):
                    if isinstance(doc.get(field), list):
                        kept, pr, regs = _tighten_subset_list(
                            "client_compat.%s" % field,
                            doc[field],
                            set(counts["client_compat"][obs_key]),
                        )
                        file_slack += pr
                        regressions.extend(regs)
                        new_doc[field] = kept
                slack_total += file_slack
                if file_slack > 0:
                    if write and not regressions:
                        if _write_if_changed(p, new_doc):
                            changed_files.append(CLIENT_COMPAT_REL)
                    elif not write:
                        changed_files.append(CLIENT_COMPAT_REL)

    if "untyped_user_error" in targets:
        p = os.path.join(root, UNTYPED_UE_REL)
        if os.path.isfile(p):
            doc = rc._read_json(p)
            if isinstance(doc, dict) and isinstance(doc.get("files"), dict):
                tight_pf, sl, regs = _tighten_file_map(
                    "untyped_user_error",
                    doc["files"],
                    counts["untyped_user_error"],
                    zero_required=False,
                )
                slack_total += sl
                regressions.extend(regs)
                if sl > 0:
                    new_doc = dict(doc)
                    new_doc["files"] = tight_pf
                    if write and not regressions:
                        if _write_if_changed(p, new_doc):
                            changed_files.append(UNTYPED_UE_REL)
                    elif not write:
                        changed_files.append(UNTYPED_UE_REL)

    if "stringly_error_bypass" in targets:
        p = os.path.join(root, STRINGLY_BYPASS_REL)
        if os.path.isfile(p):
            doc = rc._read_json(p)
            if isinstance(doc, dict) and int(doc.get("version", 1) or 1) >= 2 and isinstance(doc.get("rules"), dict):
                new_doc = dict(doc)
                new_rules = {}
                file_slack = 0
                for k, spec in doc["rules"].items():
                    if not isinstance(spec, dict) or not isinstance(spec.get("files"), dict):
                        new_rules[k] = spec
                        continue
                    tight_pf, sl, regs = _tighten_file_map(
                        "stringly_error_bypass.%s" % k,
                        spec["files"],
                        counts["stringly_error_bypass"].get(k, {}),
                        zero_required=bool(spec.get("zero_required") is True),
                    )
                    file_slack += sl
                    regressions.extend(regs)
                    nspec = dict(spec)
                    nspec["files"] = tight_pf
                    new_rules[k] = nspec
                new_doc["rules"] = new_rules
                slack_total += file_slack
                if file_slack > 0:
                    if write and not regressions:
                        if _write_if_changed(p, new_doc):
                            changed_files.append(STRINGLY_BYPASS_REL)
                    elif not write:
                        changed_files.append(STRINGLY_BYPASS_REL)

    if "test_fixture" in targets:
        p = os.path.join(root, TEST_FIXTURE_REL)
        if os.path.isfile(p):
            doc = rc._read_json(p)
            if isinstance(doc, dict) and int(doc.get("version", 0) or 0) == 2 and isinstance(doc.get("patterns"), dict):
                new_doc = dict(doc)
                new_patterns = {}
                file_slack = 0
                for name, spec in doc["patterns"].items():
                    if (
                        name not in rc.TEST_FIXTURE_PATTERNS
                        or not isinstance(spec, dict)
                        or not isinstance(spec.get("files"), dict)
                    ):
                        new_patterns[name] = spec
                        continue
                    tight_pf, sl, regs = _tighten_file_map(
                        "test_fixture.%s" % name,
                        spec["files"],
                        counts["test_fixture"].get(name, {}),
                        zero_required=bool(spec.get("zero_required") is True),
                    )
                    file_slack += sl
                    regressions.extend(regs)
                    nspec = dict(spec)
                    nspec["files"] = tight_pf
                    new_patterns[name] = nspec
                for name in rc.TEST_FIXTURE_PATTERNS:
                    if name not in doc["patterns"]:
                        regressions.append("test_fixture: missing required pattern `%s`" % name)
                new_doc["patterns"] = new_patterns
                slack_total += file_slack
                if file_slack > 0:
                    if write and not regressions:
                        if _write_if_changed(p, new_doc):
                            changed_files.append(TEST_FIXTURE_REL)
                    elif not write:
                        changed_files.append(TEST_FIXTURE_REL)

    return changed_files, slack_total, regressions


def migrate_v2(root, targets=ALL_TARGETS):
    """Generate/convert selected ratchet baselines to exact v2 per-file format from live tree counts."""
    counts = collect_all_counts(root)
    written = []
    if "arch_debt" in targets:
        p = os.path.join(root, ARCH_DEBT_REL)
        if os.path.isfile(p):
            doc = _build_arch_debt_v2(rc._read_json(p), counts["arch_debt"])
            if _write_if_changed(p, doc):
                written.append(ARCH_DEBT_REL)
    if "client_compat" in targets:
        p = os.path.join(root, CLIENT_COMPAT_REL)
        if os.path.isfile(p):
            doc = _build_client_compat_v2(rc._read_json(p), counts["client_compat"])
            if _write_if_changed(p, doc):
                written.append(CLIENT_COMPAT_REL)
    if "untyped_user_error" in targets:
        p = os.path.join(root, UNTYPED_UE_REL)
        doc = _build_untyped_ue_v2(counts["untyped_user_error"])
        if _write_if_changed(p, doc):
            written.append(UNTYPED_UE_REL)
    if "stringly_error_bypass" in targets:
        p = os.path.join(root, STRINGLY_BYPASS_REL)
        old_doc = rc._read_json(p) if os.path.isfile(p) else {}
        doc = _build_stringly_bypass_v2(old_doc, counts["stringly_error_bypass"])
        if _write_if_changed(p, doc):
            written.append(STRINGLY_BYPASS_REL)
    if "test_fixture" in targets:
        p = os.path.join(root, TEST_FIXTURE_REL)
        if os.path.isfile(p):
            doc = _build_test_fixture_v2(rc._read_json(p), counts["test_fixture"])
            if _write_if_changed(p, doc):
                written.append(TEST_FIXTURE_REL)
    return written


def main():
    ap = argparse.ArgumentParser(description="Per-file v2 ratchet auto-tightener and count dumper (#1538)")
    ap.add_argument("--root", default=os.getcwd(), help="Repository root (default: cwd)")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="Check whether any v2 ratchet baseline has slack")
    mode.add_argument("--write", action="store_true", help="Strictly tighten v2 ratchet baselines in place")
    mode.add_argument("--migrate-v2", action="store_true", help="Convert selected ratchet baselines to v2 per-file format")
    mode.add_argument("--dump-counts-json", action="store_true", help="Print canonical per-file scanner counts as JSON")
    ap.add_argument(
        "--only",
        default="",
        help="Comma-separated subset of targets: %s" % ",".join(ALL_TARGETS),
    )
    args = ap.parse_args()
    root = os.path.abspath(args.root)
    if args.only:
        targets = tuple(t.strip() for t in args.only.split(",") if t.strip())
        bad = [t for t in targets if t not in ALL_TARGETS]
        if bad:
            sys.stderr.write("Unknown --only target(s): %s\n" % ", ".join(bad))
            sys.exit(2)
    else:
        targets = ALL_TARGETS

    if args.dump_counts_json:
        print(json.dumps(collect_all_counts(root), indent=2, sort_keys=True))
        return

    if args.migrate_v2:
        written = migrate_v2(root, targets=targets)
        print("migrate_v2: updated %d file(s): %s" % (len(written), ", ".join(written) or "(none)"))
        return

    write_mode = bool(args.write)
    changed, slack, regressions = tighten_ratchets(root, write=write_mode, targets=targets)
    if regressions:
        for r in regressions:
            sys.stderr.write("[REGRESSION] %s\n" % r)
        sys.exit(2)
    if write_mode:
        print(
            "tighten_ratchets: tightened %d slack entry/entries across %d file(s): %s"
            % (slack, len(changed), ", ".join(changed) or "(already tight)")
        )
        return
    if slack > 0:
        print(
            "tighten_ratchets: %d slack entry/entries pending tightening in %s"
            % (slack, ", ".join(changed))
        )
        sys.exit(1)
    print("tighten_ratchets: TIGHT (0 slack entries, 0 regressions)")


if __name__ == "__main__":
    main()
