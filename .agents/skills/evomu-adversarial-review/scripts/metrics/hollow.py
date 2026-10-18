"""Better stub detector: a handler arm is 'hollow' if its body never touches server state
(ctx.db / ctx.sde / ctx.systems / ctx.living_world / ctx.ship_fittings / helper fns taking ctx)
and never assigns to session fields. Echoing session_id / method / args.len() does not count."""
import re, glob, collections
import os, subprocess
ROOT = os.environ.get("EVOMU_ROOT") or subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, cwd=os.path.dirname(os.path.abspath(__file__))).stdout.strip()

STATE = re.compile(r'ctx\s*\.\s*(db|sde|systems|living_world|ship_fittings|bind_object|release_session|get_or_rebuild|cataloged)|'
                   r'session\.[a-z_]+\s*=[^=]|\b(handle_[a-z_]+|[a-z_]+_transactional)\(\s*ctx')
total_arms = hollow_arms = 0
hollow_pairs = []
per_file = collections.Counter()
for f in sorted(glob.glob(ROOT + "/crates/eve-server/src/services/*.rs")):
    src = open(f).read()
    # find match arms: pattern block of ("svc","m") | ... => { body } ; use brace matching
    for m in re.finditer(r'((?:\s*\|?\s*\("[A-Za-z_0-9]+",\s*"[A-Za-z_0-9]+"\))+)\s*=>\s*', src):
        pats = re.findall(r'\("([A-Za-z_0-9]+)",\s*"([A-Za-z_0-9]+)"\)', m.group(1))
        i = m.end()
        if src[i] == '{':
            depth, j = 0, i
            while j < len(src):
                if src[j] == '{': depth += 1
                elif src[j] == '}':
                    depth -= 1
                    if depth == 0: break
                j += 1
            body = src[i:j+1]
        else:
            body = src[i:src.find('\n', i)]
        total_arms += 1
        if not STATE.search(body):
            hollow_arms += 1
            per_file[f.split('/')[-1]] += len(pats)
            hollow_pairs += [f"{s}.{mm}" for s, mm in pats]
print(f"arms: {total_arms}, hollow arms (no state read/write): {hollow_arms}, hollow (svc,method) pairs: {len(hollow_pairs)}")
for k, v in per_file.most_common():
    print(f"  {k:28s} {v}")
print("hollow pairs:", hollow_pairs)
