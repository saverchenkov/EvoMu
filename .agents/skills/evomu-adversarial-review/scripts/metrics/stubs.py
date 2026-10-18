import re, glob
import os, subprocess
ROOT = os.environ.get("EVOMU_ROOT") or subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, cwd=os.path.dirname(os.path.abspath(__file__))).stdout.strip()

stub_ret = re.compile(r'Some\(Ok\(PyRep::(List\(vec!\[\]\)|Bool\(true\)|None|Dict\(vec!\[\]\))\)\)')
total = 0
for f in sorted(glob.glob(ROOT + "/crates/eve-server/src/services/*.rs")):
    src = open(f).read()
    # split into arms at lines that start a pattern block
    arms = re.split(r'\n(?=\s{8}(?:\("|\|\s*\(")[^\n]*)', src)
    # merge consecutive pattern-only chunks
    blocks, cur = [], ""
    for a in arms:
        cur += "\n" + a
        if "=>" in a:
            blocks.append(cur); cur = ""
    for b in blocks:
        head, _, body = b.partition("=>")
        pats = re.findall(r'\("([A-Za-z_0-9]+)",\s*"([A-Za-z_0-9]+)"\)', head)
        body_lines = [l for l in body.splitlines() if l.strip()]
        # body ends at next arm; consider stub if it's short and returns constant stub
        if pats and stub_ret.search(body) and len(body_lines) <= 12:
            total += len(pats)
            print(f.split('/')[-1], len(pats), [f"{s}.{m}" for s, m in pats][:12])
print("TOTAL stubbed (service,method) pairs:", total)
