import json, re, glob, collections
import os, subprocess
ROOT = os.environ.get("EVOMU_ROOT") or subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, cwd=os.path.dirname(os.path.abspath(__file__))).stdout.strip()
cat = json.load(open(f"{ROOT}/docs/index/rpc_service_catalog.json"))
src = "\n".join(open(f).read() for f in glob.glob(f"{ROOT}/crates/eve-server/src/services/*.rs"))
explicit = collections.defaultdict(set)
wild = set()
for svc, meth in re.findall(r'\("([A-Za-z_0-9]+)",\s*(?:"([A-Za-z_0-9]+)"|_)\)', src):
    if meth:
        explicit[svc].add(meth)
    else:
        wild.add(svc)
# check wildcard arms really are wildcard (pattern with _)
wild = set(re.findall(r'\("([A-Za-z_0-9]+)",\s*_\)', src))
handled_names = set(explicit) | wild
real, wild_only, fallback = [], [], []
for s in sorted(cat):
    if s in explicit:
        real.append(s)
    elif s in wild:
        wild_only.append(s)
    else:
        fallback.append(s)
print("catalog services:", len(cat))
print("with >=1 explicit method arm:", len(real))
print("wildcard-only (any method -> True):", len(wild_only))
print("generic fallback KeyVal{ok:True}:", len(fallback))
# client method coverage for real ones
tot_client = cov = 0
rows = []
for s in real:
    cm = set(cat[s].get("v24_client_candidate_methods") or [])
    hit = cm & explicit[s]
    tot_client += len(cm); cov += len(hit)
    rows.append((s, len(hit), len(cm), sorted(explicit[s] - cm)[:6]))
print(f"client candidate methods across explicit services: {cov}/{tot_client} handled by name")
for r in rows:
    print(f"  {r[0]:24s} {r[1]}/{r[2]}  non-client methods handled: {r[3]}")
allc = sum(len(set(e.get('v24_client_candidate_methods') or [])) for e in cat.values())
allhit = sum(len(set(cat[s].get('v24_client_candidate_methods') or []) & explicit.get(s,set())) for s in cat)
print(f"ALL catalog client methods handled explicitly: {allhit}/{allc}")
inv = sorted(set(explicit) - set(cat))
print(f"explicit services NOT in catalog ({len(inv)}):", inv)
winv = sorted(wild - set(cat))
print(f"wildcard services NOT in catalog ({len(winv)}):", winv)
