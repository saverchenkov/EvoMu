#!/usr/bin/env python3
"""Coverage of ledger rpc:/notify: rows by client_expectations.json (deterministic, prints JSON stats)."""
import collections
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
LEDGER = '/Users/sergeya/ai/EvoMu-supervisor/.agents/skills/evomu-adversarial-review/references/parity/ledger.json'


def main():
    exp = json.load(open(os.path.join(HERE, 'client_expectations.json')))
    rows = json.load(open(LEDGER))['rows']
    rpc_rows = sorted(r['id'][4:] for r in rows if r['category'] == 'rpc')
    ntf_rows = sorted(r['id'][7:] for r in rows if r['category'] == 'notify')
    rpc = exp['rpc']
    ntf = exp['notify']
    # case-insensitive service match fallback (ledger service casing can differ)
    lower = collections.defaultdict(list)
    for k in rpc:
        lower[k.lower()].append(k)
    st = {}
    cov = [r for r in rpc_rows if r in rpc]
    cov_ci = [r for r in rpc_rows if r not in rpc and r.lower() in lower]
    shaped = [r for r in cov if any(p != '$' or set(u) - {'discard'} for p, u in rpc[r]['reply_shape'].items())]
    discard_only = [r for r in cov if r not in shaped]
    nh = [r for r in ntf_rows if r in ntf and ntf[r]['handlers']]
    nlisted = [r for r in ntf_rows if r in ntf]
    nreads = [r for r in nh if ntf[r]['reads']]
    unc_rpc = [r for r in rpc_rows if r not in rpc and r.lower() not in lower]
    by_svc = collections.Counter(r.split('.')[0] for r in unc_rpc)
    st['rpc_rows'] = len(rpc_rows)
    st['rpc_covered_exact'] = len(cov)
    st['rpc_covered_case_insensitive_extra'] = len(cov_ci)
    st['rpc_with_reply_use'] = len(shaped)
    st['rpc_discard_only'] = len(discard_only)
    st['rpc_uncovered_top_services'] = by_svc.most_common(25)
    st['notify_rows'] = len(ntf_rows)
    st['notify_in_notifyevents'] = len(nlisted)
    st['notify_with_handler'] = len(nh)
    st['notify_with_param_reads'] = len(nreads)
    st['extracted_rpc_not_in_ledger'] = sorted(k for k in rpc if k not in set(rpc_rows))
    st['extracted_notify_not_in_ledger_count'] = len([k for k in ntf if k not in set(ntf_rows)])
    st['uncovered_notify_sample'] = [r for r in ntf_rows if r not in ntf][:40]
    json.dump(st, sys.stdout, indent=1, sort_keys=True)
    print()


if __name__ == '__main__':
    main()
