#!/usr/bin/python3
"""Build the EvoMu parity ledger (the machine-checkable definition of "100% feature parity").

Run by the USER/reviewer tooling only; the output is pinned under
.agents/skills/evomu-adversarial-review/references/parity/ledger.json and is read-only for the
coding agent. Inputs are the pinned copies in references/parity/sources/ (NOT docs/index, which the
agent can edit).

Row categories (id prefix):
  rpc:<service>.<method>        every client-called machoNet RemoteSvc/moniker method (V24.01 code.ccp)
  notify:<EventName>            every __notifyevents__ handler in the client (server-emitted unless triaged)
  destiny:<Action>              every destiny update action the client consumes
  ballmode:<MODE>               every destiny ball mode
  quasar:<proto path>           every Quasar/public-gateway protobuf file the client ships
  esi:<METHOD> <path>           every ESI operation (pinned OpenAPI)
  sde:<res path>                every client static-data file (must be ingested + used + tested)
  effect:*                      dataset rule: every dogma effect in the client (count pinned)
  feat:<domain>.<slug>          curated game-mechanic rows (features.json)
  plan:<task id>                every canonical action-plan task
  journey:<slug>                real-client end-to-end journeys (client_journeys.json)
  + auditor rows appended by the gap auditor (auditor_rows.json), merged at status time.
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SKILL = os.path.dirname(os.path.dirname(HERE))
PARITY = os.path.join(SKILL, "references", "parity")
SRC = os.path.join(PARITY, "sources")


def load(name):
    with open(os.path.join(SRC, name)) as f:
        return json.load(f)


def row(rid, category, title, acceptance, na_allowed, attest_ok=False, **extra):
    r = {"id": rid, "category": category, "title": title, "acceptance": acceptance,
         "na_allowed": na_allowed, "attest_ok": attest_ok}
    r.update(extra)
    return r


def main():
    rows = []

    rpc = load("rpc_service_catalog.json")
    for svc in sorted(rpc):
        for m in sorted(set(rpc[svc].get("v24_client_candidate_methods") or [])):
            files = rpc[svc].get("v24_client_call_files") or []
            rows.append(row(
                "rpc:%s.%s" % (svc, m), "rpc", "%s.%s" % (svc, m),
                "Server implements %s.%s with V24 argument layout and live-server semantics; the exact "
                "return shape the client code (%s) consumes; persistent state effects; error paths raise "
                "the same UserError the live server raises." % (svc, m, ", ".join(files[:3]) or "client"),
                na_allowed=True, service=svc, method=m, client_files=files))

    notes = load("client_notifications.json")
    for name in sorted(notes):
        rows.append(row(
            "notify:%s" % name, "notify", name,
            "If server-emitted on the live server: emitted with the correct payload at the correct moment "
            "(scoped to char/corp/fleet/system/bubble/all). If client-local, triage with evidence.",
            na_allowed=True, client_files=notes[name]))

    dst = load("destiny_opcodes.json")
    for a in dst.get("v24_destiny_actions") or []:
        rows.append(row("destiny:%s" % a, "destiny", a,
                        "Binary destiny stream encodes %s exactly as the V24 client decodes it, emitted by "
                        "the simulation when the corresponding state change happens." % a, na_allowed=True))
    bm = dst.get("ball_modes") or {}
    for mode in (bm if isinstance(bm, dict) else {m: None for m in bm}):
        rows.append(row("ballmode:%s" % mode, "destiny", mode,
                        "Simulation supports ball mode %s with live-server movement physics." % mode,
                        na_allowed=False))

    q = load("quasar_protobuf_catalog.json")
    for pkg in sorted(q):
        for path in sorted(set(q[pkg])):
            rows.append(row(
                "quasar:%s" % path, "quasar", path,
                "Every request/response/notice message in this proto is served by the backend with live "
                "semantics over the public gateway (publicGatewaySvc/Quasar), incl. persistence and "
                "broadcast of notices; or triaged as client-internal with evidence.",
                na_allowed=True, package=pkg))

    esi = load("esi_openapi.json")
    for path in sorted(esi.get("paths", {})):
        for meth, op in sorted(esi["paths"][path].items()):
            if meth not in ("get", "post", "put", "delete", "patch"):
                continue
            tag = (op.get("tags") or ["?"])[0]
            scopes = []
            for sec in op.get("security") or []:
                for v in sec.values():
                    scopes += v
            rows.append(row(
                "esi:%s %s" % (meth.upper(), path), "esi", "%s %s" % (meth.upper(), path),
                "ESI HTTP endpoint served with the pinned OpenAPI request/response schema, SSO OAuth2 "
                "scope enforcement (%s), caching headers/ETag/expiry, pagination and error model, backed by "
                "the same game state the client sees." % (", ".join(sorted(set(scopes))) or "public"),
                na_allowed=False, tag=tag, operation_id=op.get("operationId")))

    for e in load("staticdata_catalog.json"):
        p = e["res_path"]
        rows.append(row(
            "sde:%s" % p, "sde", p,
            "Loaded from the real client resource (md5 %s, %s bytes) by the SDE importer (no hand-written "
            "data); every record available to server logic; a test asserts record count and spot values "
            "against the client file." % (e.get("md5"), e.get("uncompressed_bytes")),
            na_allowed=True, attest_ok=True, md5=e.get("md5")))

    eff = load("dogma_effects.json")
    rows.append(row(
        "effect:*", "effect", "All dogma effects",
        "Every dogma effect in res:/staticdata/effects.fsdbinary (%s effects) evaluates through the generic "
        "modifierInfo/expression engine with correct domain/func/operation, and has an attested "
        "data-driven test. Attest each as effect:<effectID>; the count must equal the client file."
        % eff.get("effects_count"), na_allowed=False, attest_ok=True,
        expected_count=eff.get("effects_count")))

    fpath = os.path.join(PARITY, "features.json")
    if os.path.exists(fpath):
        with open(fpath) as f:
            for ft in json.load(f)["features"]:
                rows.append(row(ft["id"], "feat", ft["title"], ft["acceptance"],
                                na_allowed=(ft.get("domain") == "removed"),
                                domain=ft.get("domain"), sources=ft.get("sources"),
                                plan_task=ft.get("plan_task")))

    with open(os.path.join(SKILL, "references", "action_plan.md")) as f:
        plan = f.read()
    ids = sorted(set(re.findall(r"\*\*`?((?:S|D|P|SP|DG|C|MN|CH|E|SO|T|Q|ESI|Z)-\d+)`?\*\*", plan)) |
                 set(re.findall(r"^\|\s*`?((?:S|D|P|SP|DG|C|MN|CH|E|SO|T|Q|ESI|Z)-\d+)`?\s*\|", plan, re.M)))
    for tid in ids:
        rows.append(row("plan:%s" % tid, "plan", tid,
                        "Canonical task %s meets every 'Done when' criterion in action_plan.md." % tid,
                        na_allowed=False))

    jpath = os.path.join(PARITY, "client_journeys.json")
    if os.path.exists(jpath):
        with open(jpath) as f:
            for j in json.load(f)["journeys"]:
                rows.append(row("journey:%s" % j["id"], "journey", j["title"], j["steps"], na_allowed=False))

    seen = set()
    dups = [r["id"] for r in rows if r["id"] in seen or seen.add(r["id"])]
    if dups:
        sys.exit("duplicate ids: %s" % dups[:20])
    out = {"version": 1, "row_count": len(rows), "rows": rows}
    with open(os.path.join(PARITY, "ledger.json"), "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, sort_keys=False)
    counts = {}
    for r in rows:
        counts[r["category"]] = counts.get(r["category"], 0) + 1
    print("ledger rows: %d %s" % (len(rows), counts))


if __name__ == "__main__":
    main()
