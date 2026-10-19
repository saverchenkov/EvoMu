#!/usr/bin/python3
"""Mechanical check that RPC service/method names and notification names used in EvoMu exist in the REAL
V24.01 client. Invented RPC names were the main cause of fake ledger rows and review FAILs.

Oracles:
  1. Client identifier set (HARD): every identifier string in the client's own bytecode (code.ccp, parsed as
     Python 2.7 marshal strings), built locally from your client install and cached in
     .evomu-reviews/client_identifiers.json (gitignored, never committed: client-derived).
     A name the client code never contains cannot be a client RPC/notification.  -> NOT_IN_CLIENT (blocking)
  2. Pinned RPC catalog (SOFT): references/parity/sources/rpc_service_catalog.json. It misses methods called on
     bound/moniker objects (beyonce, dogmaIM, skill handler, ...), so a miss is informational. -> NOT_IN_CATALOG

Detection (crates/**/*.rs, tests included): a service-like literal ("charMgr", "invbroker", or any lowerCamel
ending in Mgr/Svc/Manager/Broker/Service/Proxy/Registry, or any catalog service) followed within ~250 chars by
method literals ("M" or "A" | "B"): matches `("svc", "M") =>` arms, alias arms, and
`dispatch_rpc(&mut s, "svc", None, "M", ...)`. Plus "On<Name>" literals in non-test code (notifications).
Escape hatch, visible to the reviewer and requiring a reason: `rpc-lint: allow <reason>` on the same line or
the line above.

Usage: rpc_names.py [--files f ...] [--json out.json] [--code-ccp PATH]
Exit 1 if any NOT_IN_CLIENT violation in the scanned files, else 0.
"""
import argparse
import glob
import json
import os
import re
import struct
import subprocess
import sys
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.environ.get("EVOMU_PARITY_SOURCES") or os.path.join(os.path.dirname(os.path.dirname(HERE)),
                                                             "references", "parity", "sources")
DEFAULT_CCP = os.path.expanduser("~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/"
                                 "Contents/Resources/build/code.ccp")
LIT = re.compile(r'"((?:[^"\\\n]|\\.)*)"')
SERVICE_LIKE = re.compile(r"^[a-z][A-Za-z0-9]*(?:Mgr|Svc|Manager|Broker|broker|Service|Proxy|Registry)$")
METHOD_LIKE = re.compile(r"^[A-Z][A-Za-z0-9_]*$")
NOTIFY_LIKE = re.compile(r"^On[A-Z][A-Za-z0-9]+$")


def repo_root():
    return subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True,
                          cwd=os.getcwd()).stdout.strip()


def client_identifiers(root, ccp):
    cache = os.path.join(root, ".evomu-reviews", "client_identifiers.json")
    try:
        if os.path.exists(cache) and os.path.exists(ccp) and os.path.getmtime(cache) >= os.path.getmtime(ccp):
            return set(json.load(open(cache)))
    except (OSError, ValueError):
        pass
    if not os.path.exists(ccp):
        return None
    ident = re.compile(rb"[A-Za-z_][A-Za-z0-9_]*\Z")
    ids = set()
    z = zipfile.ZipFile(ccp)
    for n in z.namelist():
        b = z.read(n)
        try:
            b = zlib.decompress(b)
        except zlib.error:
            pass
        L = len(b)
        for m in re.finditer(rb"[stu]", b):
            k = m.start()
            if k + 5 > L:
                break
            ln = struct.unpack_from("<I", b, k + 1)[0]
            if 3 <= ln <= 120 and k + 5 + ln <= L:
                s = b[k + 5:k + 5 + ln]
                if ident.match(s):
                    ids.add(s.decode())
    os.makedirs(os.path.dirname(cache), exist_ok=True)
    json.dump(sorted(ids), open(cache, "w"))
    return ids


def allowed(lines, lineno):
    return any(0 <= ln < len(lines) and "rpc-lint: allow" in lines[ln] for ln in (lineno - 1, lineno - 2))


def scan_file(path, methods, notes, ids):
    try:
        text = open(path, encoding="utf-8", errors="replace").read()
    except OSError:
        return []
    lines = text.splitlines()
    is_test = "/tests/" in path or "/benches/" in path
    cut = len(text) if is_test else (text.find("#[cfg(test)]") if "#[cfg(test)]" in text else len(text))
    lits = [(m.start(), m.end(), m.group(1)) for m in LIT.finditer(text)]
    out = []

    def known(name):
        return ids is None or name in ids

    for i, (s, e, val) in enumerate(lits):
        lineno = text.count("\n", 0, s) + 1
        if val in methods or SERVICE_LIKE.match(val):
            meths, last_end = [], e
            for j in range(i + 1, min(i + 5, len(lits))):
                js, je, jv = lits[j]
                if js - e > 250 or jv in methods or SERVICE_LIKE.match(jv):
                    break
                if METHOD_LIKE.match(jv):
                    if meths and not re.fullmatch(r"\s*\|\s*", text[last_end:js]):
                        break
                    meths.append((jv, text.count("\n", 0, js) + 1))
                    last_end = je
                elif meths:
                    break
            if not meths or allowed(lines, lineno):
                continue
            if not known(val):
                out.append({"kind": "NOT_IN_CLIENT", "file": path, "line": lineno, "name": val, "what": "service",
                            "methods": [m for m, _ in meths]})
                continue
            for m, ml in meths:
                if allowed(lines, ml):
                    continue
                if not known(m):
                    out.append({"kind": "NOT_IN_CLIENT", "file": path, "line": ml, "name": "%s.%s" % (val, m),
                                "what": "method"})
                elif m not in methods.get(val, ()):
                    out.append({"kind": "NOT_IN_CATALOG", "file": path, "line": ml, "name": "%s.%s" % (val, m),
                                "what": "method"})
        elif s < cut and NOTIFY_LIKE.match(val) and val not in notes and not allowed(lines, lineno):
            out.append({"kind": "NOT_IN_CLIENT" if not known(val) else "NOT_IN_CATALOG", "file": path,
                        "line": lineno, "name": val, "what": "notification"})
    seen, uniq = set(), []
    for v in out:
        k = (v["kind"], v["file"], v["line"], v["name"])
        if k not in seen:
            seen.add(k)
            uniq.append(v)
    return uniq


def run(files=None, code_ccp=None):
    root = repo_root()
    rpc = json.load(open(os.path.join(SRC, "rpc_service_catalog.json")))
    methods = {s: set(v.get("v24_client_candidate_methods") or []) for s, v in rpc.items()}
    notes = set(json.load(open(os.path.join(SRC, "client_notifications.json"))))
    ids = client_identifiers(root, code_ccp or os.environ.get("EVOMU_CODE_CCP") or DEFAULT_CCP)
    if files is None:
        files = [p for p in glob.glob(os.path.join(root, "crates", "**", "*.rs"), recursive=True) if "/target/" not in p]
    viol = []
    for p in files:
        p = p if os.path.isabs(p) else os.path.join(root, p)
        if p.endswith(".rs") and os.path.exists(p):
            viol += scan_file(p, methods, notes, ids)
    for v in viol:
        v["file"] = os.path.relpath(v["file"], root)
    return viol, ids is not None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--files", nargs="*")
    ap.add_argument("--json")
    ap.add_argument("--code-ccp")
    a = ap.parse_args()
    viol, have_client = run(a.files, a.code_ccp)
    if a.json:
        json.dump(viol, open(a.json, "w"), indent=1)
    hard = [v for v in viol if v["kind"] == "NOT_IN_CLIENT"]
    soft = [v for v in viol if v["kind"] == "NOT_IN_CATALOG"]
    print("RPC-name check: %d NOT_IN_CLIENT (blocking), %d NOT_IN_CATALOG (informational)%s" % (
        len(hard), len(soft), "" if have_client else "  [WARNING: client code.ccp not found; only catalog checks ran]"))
    for v in hard[:300]:
        print("  NOT_IN_CLIENT  %s:%d %s %s" % (v["file"], v["line"], v["what"], v["name"]))
    return 1 if hard else 0


if __name__ == "__main__":
    sys.exit(main())
