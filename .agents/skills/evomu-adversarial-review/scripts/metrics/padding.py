"""Counts (service, method) pairs that share a single handler body with many other methods.
Large shared arms are where name-padding (adding unrelated method names to inflate coverage) hides."""
import os, re, glob, subprocess, collections
ROOT = os.environ.get("EVOMU_ROOT") or subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, cwd=os.path.dirname(os.path.abspath(__file__))).stdout.strip()
dist = collections.Counter(); big = []
for f in sorted(glob.glob(ROOT + "/crates/eve-server/src/services/*.rs")):
    src = open(f).read()
    for m in re.finditer(r'((?:\s*\|?\s*\("[A-Za-z_0-9]+",\s*"[A-Za-z_0-9]+"\))+)\s*=>', src):
        pats = re.findall(r'\("([A-Za-z_0-9]+)",\s*"([A-Za-z_0-9]+)"\)', m.group(1))
        dist[len(pats)] += 1
        if len(pats) >= 4:
            line = src[:m.start()].count("\n") + 2
            big.append((os.path.basename(f), line, len(pats), ["%s.%s" % p for p in pats]))
total = sum(k * v for k, v in dist.items())
print("handled (svc,method) pairs:", total, "| arms by #methods:", sorted(dist.items()))
print("pairs in arms with >=6 methods:", sum(n for _, _, n, _ in big if n >= 6))
print("arms with >=4 methods (inspect each for per-method semantics):")
for fn, line, n, pats in sorted(big, key=lambda x: -x[2]):
    print("  %s:%d  [%d] %s" % (fn, line, n, ", ".join(pats)))
