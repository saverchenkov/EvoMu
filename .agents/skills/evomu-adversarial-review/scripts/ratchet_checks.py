#!/usr/bin/python3
"""Zero-cargo (< 1s) source ratchet and commit issue-closure checks for `preflight.py`.

Mirrors the exact Rust ratchet implementations in:
  - `crates/eve-proto/src/pb.rs` (`crates/eve-proto/ratchets/raw_encode_field_ratchet.json`)
  - `crates/eve-testkit/src/notify_ratchet.rs` (`crates/eve-testkit/ratchets/raw_notify_ratchet.json`)
  - `crates/eve-testkit/src/client_harness.rs` (`crates/eve-testkit/ratchets/raw_dispatch_ratchet.json`)
  - `crates/eve-db/src/lock_rank.rs` (`crates/eve-db/lock_rank_ratchet.txt`)
  - Conditional (#1189): `crates/eve-testkit/ratchets/client_compat_ratchet.json`
  - Conditional (#1197): `crates/eve-testkit/ratchets/arch_debt_ratchet.json`

Also enforces the review-branch commit-message guard blocking GitHub auto-closing keywords
(`Fixes #N`, `Closes #N`, `Resolves #N`) against open RFC/architecture umbrella issues or non-zero
ratchet `owning_issue`s, and capping closing defect references at <= 5 distinct issues per commit.
"""
import glob
import hashlib
import json
import os
import re
import subprocess

import review_lib as rl

RATCHET_CACHE_FILENAME = "ratchet_scan_cache_v3.json"
RATCHET_STAT_FILENAME = "ratchet_stat_index_v3.json"

# Protected open RFC/architecture umbrella trackers (plus #933 until all C-9 ratchets reach 0).
DEFAULT_UMBRELLA_ISSUES = frozenset({898, 899, 900, 901, 916, 921, 922, 923, 931, 932, 933})
MAX_FIXES_PER_COMMIT = 5
BATCH_FIXES_ALLOW_RE = re.compile(r"^\s*batch-fixes:\s*allow\b(?:\s+\S.*)?$", re.I | re.M)

# Matches GitHub closing clauses including optional `owner/repo#N` and comma/`and` issue lists
# (`Fixes #101, #102 and Closes #103`), stopping before non-closing prefixes such as `Refs #933`.
_CLOSE_KW = r"(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)"
_ISSUE_REF = r"(?:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)?#(\d+)"
CLOSING_CLAUSE_RE = re.compile(
    r"\b" + _CLOSE_KW + r"\b\s*:?\s*(?:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)?#\d+"
    r"(?:\s*(?:,\s*(?:and\s+)?|\band\b\s*)(?:" + _CLOSE_KW + r"\b\s*:?\s*)?(?:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)?#\d+)*",
    re.I,
)
ISSUE_NUM_RE = re.compile(_ISSUE_REF)

LEX_START_RE = re.compile(
    r"//[^\n]*"
    r"|/\*"
    r"|(?<![A-Za-z0-9_])b?r(#*)\""
    r"|b?\""
    r"|b?\x27(?:\\[^\n\x27]{1,8}|[^\n\x27\\])\x27"
)
STR_BODY_RE = re.compile(r"(?:\\[\s\S]|[^\"\\])*\"")
NL_SUB_RE = re.compile(r"[^\n]+")
CFG_TEST_RE = re.compile(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]")

ENC_FIELD_RE = re.compile(
    r"\b(?:encode_varint_field|encode_bytes_field|encode_string_field|encode_fixed32_field|encode_fixed64_field)\s*\("
)
NOTIFY_PLUMBING_FILES = frozenset({
    "router/cache_notifications.rs",
    "router/notify_registry.rs",
    "router/session.rs",
    "router/dispatch.rs",
    "macho_listener.rs",
})
NOTIFY_METH_RE = re.compile(r"\.(?:notify|push_notification|notify_remote_message)\s*\(")
NOTIFY_FN_RE = re.compile(r"(?<![A-Za-z0-9_.])(build_macho_notification_packet|notify_char)\s*\(")
FN_KW_BEFORE_RE = re.compile(r"\bfn\s+$")

DISPATCH_RPC_RE = re.compile(r"\.dispatch_rpc\s*\(")
COMMA_PAREN_RE = re.compile(r"[(),]")

DIRECT_LAYER_FNS = (
    "normalize_macho_rpc_reply_at",
    "normalize_macho_rpc_reply",
    "normalize_macho_notification_with_sde",
    "normalize_macho_notification",
    "should_suppress_macho_notification",
)
DIRECT_LAYER_RE = re.compile(
    r"(?<![A-Za-z0-9_.])(?:" + "|".join(DIRECT_LAYER_FNS) + r")\s*\("
)

LOCK_METHODS = frozenset({
    "read", "write", "lock", "try_read", "try_write", "try_lock",
    "read_unranked", "try_write_unranked",
    "with_snapshot", "try_with_snapshot", "with_mut_snapshot", "try_with_mut_snapshot",
    "tx", "read_state", "try_read_state", "assert_no_locks_held", "assert_locks_below",
})
RUST_TOK_RE = re.compile(
    r"//[^\n]*"
    r"|/\*"
    r"|(?<![A-Za-z0-9_])b?r(#*)\""
    r"|r#([A-Za-z_][A-Za-z0-9_]*)"
    r"|b?\"(?:\\[\s\S]|[^\"\\])*\""
    r"|b\x27(?:\\[\s\S]|[^\x27\\])*\x27"
    r"|\x27(?:\\[^\n\x27]*\x27|[^\n\x27\\]\x27|[A-Za-z_][A-Za-z0-9_]*)"
    r"|([A-Za-z_][A-Za-z0-9_]*)"
    r"|(\S)"
)

ARCH_DEBT_PATTERNS = {
    "master_wallet": re.compile(r"\bmaster_wallet\b"),
    "direct_items_map_mut": re.compile(r"\bitems\s*\.\s*(?:remove|get_mut|insert)\s*\("),
    "hardcoded_fallback_entity_ids": re.compile(
        r"\.unwrap_or\s*\(\s*(?:90_?000_?001|30_?000_?142|60_?003_?760)\b"
    ),
    "service_state_blobs_refs": re.compile(r"\bservice_state_blobs\b"),
    "direct_member_roles_get": re.compile(r"\bmember_roles\s*\.\s*get\s*\("),
    "direct_location_or_ship_assign": re.compile(
        r"\.\s*(?:solar_system_id|station_id|structure_id|active_ship_id)\s*=(?!=)"
    ),
    "positional_rpc_args_index": re.compile(r"\bargs\s*\.\s*(?:first\s*\(\s*\)|get\s*\(\s*\d+\s*\))"),
    "stringly_bridge_rpc_error_code": re.compile(r"\bBridgeRpcError\s*\{\s*code\s*:\s*\""),
}


def _mask_non_newlines(s):
    return NL_SUB_RE.sub(lambda m: " " * len(m.group(0)), s)


def strip_rust_comments_and_literals(src):
    """Replace Rust line/nested-block comments, raw strings, byte/normal strings, and char literals
    with spaces while preserving exact newline layout and byte/char offsets outside literals."""
    out = []
    pos = 0
    n = len(src)
    while pos < n:
        m = LEX_START_RE.search(src, pos)
        if not m:
            out.append(src[pos:])
            break
        s = m.start()
        if s > pos:
            out.append(src[pos:s])
        tok = m.group(0)
        if tok.startswith("//") or tok.endswith("'"):
            out.append(_mask_non_newlines(tok))
            pos = m.end()
        elif tok == "/*":
            depth = 1
            cur = m.end()
            while cur < n and depth > 0:
                n_open = src.find("/*", cur)
                n_close = src.find("*/", cur)
                if n_close < 0:
                    cur = n
                    break
                if 0 <= n_open < n_close:
                    depth += 1
                    cur = n_open + 2
                else:
                    depth -= 1
                    cur = n_close + 2
            out.append(_mask_non_newlines(src[s:cur]))
            pos = cur
        elif m.group(1) is not None:
            hashes = m.group(1)
            closing = '"' + hashes
            end_q = src.find(closing, m.end())
            cur = n if end_q < 0 else end_q + len(closing)
            out.append(_mask_non_newlines(src[s:cur]))
            pos = cur
        else:
            sm = STR_BODY_RE.match(src, m.end())
            cur = sm.end() if sm else n
            out.append(_mask_non_newlines(src[s:cur]))
            pos = cur
    return "".join(out)


def strip_cfg_test_items(clean):
    """Mask `#[cfg(test)]` items (modules, functions, `#[path = ...] mod ...;`) in already
    comment/literal-stripped Rust source, matching `eve-testkit/src/notify_ratchet.rs`."""
    if "cfg" not in clean:
        return clean
    chars = list(clean)
    n = len(chars)
    pos = 0
    while True:
        m = CFG_TEST_RE.search(clean, pos)
        if not m:
            break
        start = m.start()
        cur = m.end()
        while True:
            while cur < n and chars[cur].isspace():
                cur += 1
            if cur + 1 < n and chars[cur] == "#" and chars[cur + 1] == "[":
                cur += 2
                bd = 1
                while cur < n and bd > 0:
                    if chars[cur] == "[":
                        bd += 1
                    elif chars[cur] == "]":
                        bd -= 1
                    cur += 1
                continue
            break
        end = n
        while cur < n:
            if chars[cur] == ";":
                end = cur + 1
                break
            if chars[cur] == "{":
                depth = 1
                cur += 1
                while cur < n and depth > 0:
                    if chars[cur] == "{":
                        depth += 1
                    elif chars[cur] == "}":
                        depth -= 1
                    cur += 1
                end = cur
                break
            cur += 1
        for k in range(start, end):
            if chars[k] != "\n":
                chars[k] = " "
        pos = end
    return "".join(chars)


def is_test_only_rel_path(rel_unix):
    fname = rel_unix.rsplit("/", 1)[-1]
    return (
        rel_unix.startswith("tests/")
        or "/tests/" in rel_unix
        or fname == "tests.rs"
        or fname.endswith("_tests.rs")
        or fname.endswith("_test.rs")
    )


def _read_bytes(path):
    with open(path, "rb") as fh:
        return fh.read()


def _read_text(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def _read_json(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def _file_sig(path):
    st = os.stat(path)
    return "%d:%d" % (st.st_mtime_ns, st.st_size)


def _load_cache(cache_path):
    if cache_path and os.path.isfile(cache_path):
        try:
            data = _read_json(cache_path)
            if isinstance(data, dict):
                return data
        except (OSError, ValueError):
            pass
    return {"src_files": {}, "it_files": {}, "lock_files": {}}


def _save_cache(cache_path, cache):
    if not cache_path:
        return
    try:
        os.makedirs(os.path.dirname(cache_path), exist_ok=True)
        tmp = cache_path + ".tmp.%d" % os.getpid()
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(cache, fh)
        os.replace(tmp, cache_path)
    except OSError:
        pass


def count_raw_notify_in_prod(prod):
    c = len(NOTIFY_METH_RE.findall(prod))
    for m in NOTIFY_FN_RE.finditer(prod):
        if not FN_KW_BEFORE_RE.search(prod[max(0, m.start() - 16):m.start()]):
            c += 1
    return c


def count_direct_layer_calls(clean_src):
    c = 0
    for m in DIRECT_LAYER_RE.finditer(clean_src):
        if not FN_KW_BEFORE_RE.search(clean_src[max(0, m.start() - 16):m.start()]):
            c += 1
    return c


def _is_ident_b(c):
    return (48 <= c <= 57) or (65 <= c <= 90) or (97 <= c <= 122) or c == 95


def _is_word_at(b, pos, word_b):
    end = pos + len(word_b)
    return (
        end <= len(b)
        and b[pos:end] == word_b
        and (pos == 0 or not _is_ident_b(b[pos - 1]))
        and (end == len(b) or not _is_ident_b(b[end]))
    )


def _skip_ws_back(b, pos):
    while pos > 0 and chr(b[pos - 1]).isspace():
        pos -= 1
    return pos


def _is_prec_matches(cb, vp):
    bv = _skip_ws_back(cb, vp)
    if bv == 0 or cb[bv - 1] != 40:
        return False
    bp = _skip_ws_back(cb, bv - 1)
    mt = b"matches!"
    return (
        bp >= len(mt)
        and cb[bp - len(mt):bp] == mt
        and (bp == len(mt) or not _is_ident_b(cb[bp - len(mt) - 1]))
    )


def count_caller_trace_comparisons(prod_clean):
    if "caller_trace" not in prod_clean:
        return 0
    b = prod_clean.encode("latin1", errors="replace")
    needle = b"caller_trace"
    count = 0
    idx = 0
    while True:
        p = b.find(needle, idx)
        if p < 0:
            break
        if _is_word_at(b, p, needle):
            after = p + len(needle)
            while after < len(b) and chr(b[after]).isspace():
                after += 1
            fwd = (
                after + 2 <= len(b)
                and b[after:after + 2] in (b"==", b"!=")
                and (after + 2 == len(b) or b[after + 2] != 61)
            ) or (_is_prec_matches(b, p) and after < len(b) and b[after] == 44)
            before = _skip_ws_back(b, p)
            bwd = (
                before >= 2
                and b[before - 2:before] in (b"==", b"!=")
                and (before == 2 or b[before - 3] not in (61, 33))
            )
            if fwd or bwd:
                count += 1
        idx = p + len(needle)
    return count


def count_caller_trace_params(prod_clean):
    if "caller_trace" not in prod_clean:
        return 0
    b = prod_clean.encode("latin1", errors="replace")
    needle = b"caller_trace"
    count = 0
    idx = 0
    while True:
        p = b.find(needle, idx)
        if p < 0:
            break
        if _is_word_at(b, p, needle):
            after = p + len(needle)
            while after < len(b) and chr(b[after]).isspace():
                after += 1
            if after < len(b) and b[after] == 58 and (after + 1 == len(b) or b[after + 1] != 58):
                count += 1
        idx = p + len(needle)
    return count


def _scan_server_src_file(path, rel_in_src, src_cache, stat_src=None):
    sig = _file_sig(path)
    if stat_src is not None:
        st_hit = stat_src.get(rel_in_src)
        if st_hit and st_hit.get("sig") == sig and isinstance(st_hit.get("rec"), dict):
            return st_hit["rec"], False, False
    raw_bytes = _read_bytes(path)
    ckey = "%s:%s" % (rel_in_src, hashlib.sha1(raw_bytes).hexdigest())
    cached = src_cache.get(ckey)
    if isinstance(cached, dict):
        if stat_src is not None:
            stat_src[rel_in_src] = {"sig": sig, "rec": cached}
        return cached, False, stat_src is not None
    raw = raw_bytes.decode("utf-8", errors="replace")
    needs_clean = any(
        s in raw
        for s in (
            "encode_",
            ".notify",
            ".push_notification",
            "build_macho_notification_packet",
            "notify_char",
            "caller_trace",
            "normalize_macho_",
            "should_suppress_macho_notification",
        )
    )
    is_test_rel = is_test_only_rel_path(rel_in_src)
    is_plumbing = rel_in_src in NOTIFY_PLUMBING_FILES

    enc_calls = 0
    notify_calls = 0
    ct_cmp = 0
    ct_params = 0
    direct_layer_test = 0

    if needs_clean:
        clean = strip_rust_comments_and_literals(raw)
        if "encode_" in raw:
            enc_calls = len(ENC_FIELD_RE.findall(clean))
        if is_test_rel:
            if "normalize_macho_" in raw or "should_suppress_macho_notification" in raw:
                direct_layer_test = count_direct_layer_calls(clean)
        else:
            prod = strip_cfg_test_items(clean)
            if not is_plumbing and any(
                s in raw for s in (".notify", ".push_notification", "build_macho_notification_packet", "notify_char")
            ):
                notify_calls = count_raw_notify_in_prod(prod)
            if "caller_trace" in raw:
                ct_cmp = count_caller_trace_comparisons(prod)
                if rel_in_src.startswith("services/"):
                    ct_params = count_caller_trace_params(prod)
            if "normalize_macho_" in raw or "should_suppress_macho_notification" in raw:
                total_dl = count_direct_layer_calls(clean)
                prod_dl = count_direct_layer_calls(prod)
                direct_layer_test = max(0, total_dl - prod_dl)

    rec = {
        "enc": enc_calls,
        "notify": notify_calls,
        "ct_cmp": ct_cmp,
        "ct_params": ct_params,
        "dl_test": direct_layer_test,
    }
    src_cache[ckey] = rec
    if stat_src is not None:
        stat_src[rel_in_src] = {"sig": sig, "rec": rec}
    return rec, True, stat_src is not None


def _scan_it_file(path, rel_in_it, it_cache, stat_it=None):
    sig = _file_sig(path)
    if stat_it is not None:
        st_hit = stat_it.get(rel_in_it)
        if st_hit and st_hit.get("sig") == sig and isinstance(st_hit.get("rec"), dict):
            return st_hit["rec"], False, False
    raw_bytes = _read_bytes(path)
    ckey = "%s:%s" % (rel_in_it, hashlib.sha1(raw_bytes).hexdigest())
    cached = it_cache.get(ckey)
    if isinstance(cached, dict):
        if stat_it is not None:
            stat_it[rel_in_it] = {"sig": sig, "rec": cached}
        return cached, False, stat_it is not None
    raw = raw_bytes.decode("utf-8", errors="replace")
    has_disp = False
    raw_none = 0
    dl_test = 0
    if ".dispatch_rpc" in raw or "normalize_macho_" in raw or "should_suppress_macho_notification" in raw:
        clean = strip_rust_comments_and_literals(raw)
        if ".dispatch_rpc" in raw:
            idx = 0
            while True:
                m = DISPATCH_RPC_RE.search(clean, idx)
                if not m:
                    break
                has_disp = True
                open_p = m.end() - 1
                depth = 1
                last_comma = prev_comma = open_p + 1
                pos = len(clean)
                for pm in COMMA_PAREN_RE.finditer(clean, open_p + 1):
                    ch = pm.group(0)
                    p_idx = pm.start()
                    if ch == "(":
                        depth += 1
                    elif ch == ")":
                        depth -= 1
                        if depth == 0:
                            pos = p_idx
                            break
                    elif ch == "," and depth == 1:
                        prev_comma = last_comma
                        last_comma = p_idx + 1
                tail = clean[last_comma:pos].strip()
                last_arg = clean[prev_comma:last_comma - 1].strip() if (not tail and last_comma > open_p + 1) else tail
                if last_arg == "None":
                    raw_none += 1
                idx = pos + 1
        if "normalize_macho_" in raw or "should_suppress_macho_notification" in raw:
            dl_test = count_direct_layer_calls(clean)
    rec = {"has_disp": has_disp, "raw_none": raw_none, "dl_test": dl_test}
    it_cache[ckey] = rec
    if stat_it is not None:
        stat_it[rel_in_it] = {"sig": sig, "rec": rec}
    return rec, True, stat_it is not None


def tokenize_rust_for_lock_rank(src):
    toks = []
    pos = 0
    n = len(src)
    while pos < n:
        m = RUST_TOK_RE.search(src, pos)
        if not m:
            break
        raw_t = m.group(0)
        if raw_t.startswith("//"):
            pos = m.end()
        elif raw_t == "/*":
            depth = 1
            cur = m.end()
            while cur < n and depth > 0:
                no = src.find("/*", cur)
                nc = src.find("*/", cur)
                if nc < 0:
                    cur = n
                    break
                if 0 <= no < nc:
                    depth += 1
                    cur = no + 2
                else:
                    depth -= 1
                    cur = nc + 2
            pos = cur
        elif m.group(1) is not None:
            hashes = m.group(1)
            end_q = src.find('"' + hashes, m.end())
            pos = n if end_q < 0 else end_q + 1 + len(hashes)
            toks.append(("Other", ""))
        elif m.group(2) is not None:
            toks.append(("RawIdent", raw_t))
            pos = m.end()
        elif m.group(3) is not None:
            toks.append(("Ident", m.group(3)))
            pos = m.end()
        elif m.group(4) is not None:
            toks.append(("Punct", m.group(4)))
            pos = m.end()
        else:
            toks.append(("Other", ""))
            pos = m.end()
    return toks


def scan_lock_rank_sites(src):
    tokens = tokenize_rust_for_lock_rank(src)
    brace_depth = 0
    fn_stack = []
    pending_fn = None
    pending_paren = 0
    sites = set()
    nt = len(tokens)
    for idx, (kind, text) in enumerate(tokens):
        if pending_fn is not None and kind == "Punct":
            if text == "(":
                pending_paren += 1
            elif text == ")" and pending_paren > 0:
                pending_paren -= 1
            elif text == ";" and pending_paren == 0:
                pending_fn = None
            elif text == "{" and pending_paren == 0:
                brace_depth += 1
                fn_stack.append((pending_fn, brace_depth))
                pending_fn = None
                continue
        if kind == "Punct":
            if text == "{":
                brace_depth += 1
            elif text == "}":
                brace_depth = max(0, brace_depth - 1)
                while fn_stack and fn_stack[-1][1] > brace_depth:
                    fn_stack.pop()
        elif kind == "Ident" and text == "fn" and pending_fn is None and idx + 1 < nt and tokens[idx + 1][0] == "Ident":
            pending_fn = tokens[idx + 1][1]
            pending_paren = 0

        if kind == "Ident" and text in LOCK_METHODS:
            is_call = False
            if idx + 1 < nt and tokens[idx + 1][0] == "Punct":
                if tokens[idx + 1][1] == "(":
                    is_call = True
                elif (
                    tokens[idx + 1][1] == ":"
                    and idx + 3 < nt
                    and tokens[idx + 2] == ("Punct", ":")
                    and tokens[idx + 3] == ("Punct", "<")
                ):
                    is_call = True
            if not is_call:
                continue
            enclosing_fn = fn_stack[-1][0] if fn_stack else "<module>"
            field_name = text
            if idx >= 1 and tokens[idx - 1] == ("Punct", ".") and idx >= 2:
                pk, pt = tokens[idx - 2]
                if pk in ("Ident", "RawIdent"):
                    field_name = text if pt == "self" else pt
                elif (pk, pt) == ("Punct", ")"):
                    pdepth = 1
                    k = max(0, idx - 3)
                    while True:
                        if tokens[k][0] == "Punct":
                            if tokens[k][1] == ")":
                                pdepth += 1
                            elif tokens[k][1] == "(":
                                pdepth -= 1
                                if pdepth == 0:
                                    break
                        if k == 0:
                            break
                        k -= 1
                    if k > 0 and pdepth == 0 and tokens[k - 1][0] in ("Ident", "RawIdent"):
                        field_name = tokens[k - 1][1]
            sites.add([enclosing_fn, field_name] if False else (enclosing_fn, field_name))
    return sites


def _normalize_rel_file(raw_file):
    f = raw_file.replace("\\", "/")
    idx = f.find("/crates/")
    if idx >= 0:
        return f[idx + 1:]
    if f.startswith("./"):
        return f[2:]
    return f


def check_lock_rank_ratchet(root, lock_cache=None):
    """Validate `crates/eve-db/lock_rank_ratchet.txt` identically to `eve_db::lock_rank::validate_ratchet_against_workspace`."""
    rel_ratchet = "crates/eve-db/lock_rank_ratchet.txt"
    full_ratchet = os.path.join(root, rel_ratchet)
    if not os.path.isfile(full_ratchet):
        return [], False
    lock_cache = lock_cache if lock_cache is not None else {}
    dirty = False
    violations = []
    seen = set()
    entries = []
    raw_lines = _read_text(full_ratchet).splitlines()
    for lineno, raw_line in enumerate(raw_lines, 1):
        trimmed = raw_line.strip()
        if not trimmed or trimmed.startswith("#"):
            continue
        parts = trimmed.split("\t")
        if len(parts) != 3:
            violations.append((
                "RATCHET_LOCK_RANK",
                "line %d: expected 3 tab-separated columns `<held>\\t<acq>\\t<file>::<fn>::<field>`" % lineno,
                rel_ratchet,
                lineno,
            ))
            continue
        try:
            held_rank = int(parts[0])
            acq_rank = int(parts[1])
            if not (0 <= held_rank <= 65535 and 0 <= acq_rank <= 65535):
                raise ValueError("out of u16 range")
        except ValueError:
            violations.append((
                "RATCHET_LOCK_RANK",
                "line %d: invalid rank integer in `%s`" % (lineno, trimmed),
                rel_ratchet,
                lineno,
            ))
            continue
        kparts = [p.strip() for p in parts[2].split("::")]
        if len(kparts) != 3 or any((not x or ":" in x) for x in kparts):
            violations.append((
                "RATCHET_LOCK_RANK",
                "line %d: ratchet key must use structured `<file>::<fn>::<field>` format (legacy `file:line:col` rejected): %s"
                % (lineno, parts[2]),
                rel_ratchet,
                lineno,
            ))
            continue
        rel_f = _normalize_rel_file(kparts[0])
        enc_fn, fld = kparts[1], kparts[2]
        tup = (held_rank, acq_rank, rel_f, enc_fn, fld)
        if tup in seen:
            violations.append((
                "RATCHET_LOCK_RANK",
                "line %d: duplicate ratchet entry `%d\\t%d\\t%s::%s::%s`" % (lineno, held_rank, acq_rank, rel_f, enc_fn, fld),
                rel_ratchet,
                lineno,
            ))
            continue
        seen.add(tup)
        entries.append((lineno, held_rank, acq_rank, rel_f, enc_fn, fld))

    if violations:
        return violations, dirty

    mem_sites = {}
    for lineno, held_rank, acq_rank, rel_f, enc_fn, fld in entries:
        if held_rank == 20 and acq_rank == 20:
            violations.append((
                "RATCHET_LOCK_RANK",
                "forbidden 20 -> 20 same-rank Ballpark ratchet entry: %s::%s::%s" % (rel_f, enc_fn, fld),
                rel_ratchet,
                lineno,
            ))
        if held_rank in (41, 42) or acq_rank in (41, 42):
            violations.append((
                "RATCHET_LOCK_RANK",
                "stale RANK_SQLITE_CONN/FP (41/42) entry: %d -> %d %s::%s::%s"
                % (held_rank, acq_rank, rel_f, enc_fn, fld),
                rel_ratchet,
                lineno,
            ))
        if held_rank in (60, 40) and acq_rank in (20, 21):
            violations.append((
                "RATCHET_LOCK_RANK",
                "forbidden %d -> %d ratchet entry: %s::%s::%s" % (held_rank, acq_rank, rel_f, enc_fn, fld),
                rel_ratchet,
                lineno,
            ))
        if rel_f not in mem_sites:
            full_src = os.path.join(root, rel_f)
            if not os.path.isfile(full_src):
                violations.append((
                    "RATCHET_LOCK_RANK",
                    "ratchet entry references missing file %s" % rel_f,
                    rel_ratchet,
                    lineno,
                ))
                mem_sites[rel_f] = None
                continue
            sig = _file_sig(full_src)
            raw_b = _read_bytes(full_src)
            lkey = "%s:%s" % (rel_f, hashlib.sha1(raw_b).hexdigest())
            cached = lock_cache.get(lkey)
            if isinstance(cached, dict) and isinstance(cached.get("sites"), list):
                mem_sites[rel_f] = {tuple(x) for x in cached.get("sites", ())}
            else:
                src_txt = raw_b.decode("utf-8", errors="replace")
                s_set = scan_lock_rank_sites(src_txt)
                mem_sites[rel_f] = s_set
                lock_cache[lkey] = {"sig": sig, "sites": sorted(list(x) for x in s_set)}
                dirty = True
        if mem_sites[rel_f] is not None and (enc_fn, fld) not in mem_sites[rel_f]:
            violations.append((
                "RATCHET_LOCK_RANK",
                "stale ratchet entry matches 0 live lock sites: %d -> %d %s::%s::%s"
                % (held_rank, acq_rank, rel_f, enc_fn, fld),
                rel_ratchet,
                lineno,
            ))
    return violations, dirty


# ---------------------------------------------------------------------------
# Conditional #1189 `client_compat_ratchet.json` scanner (exact Rust parity)
# ---------------------------------------------------------------------------

def _is_fn_def_header_b(b, start):
    pos = start
    while pos > 0 and chr(b[pos - 1]).isspace():
        pos -= 1
    return pos >= 2 and b[pos - 2:pos] == b"fn" and (pos == 2 or not _is_ident_b(b[pos - 3]))


def _find_fn_body_span(clean_b, fn_name):
    nb = fn_name.encode("ascii")
    idx = 0
    fn_start = None
    while True:
        p = clean_b.find(nb, idx)
        if p < 0:
            break
        if _is_word_at(clean_b, p, nb) and _is_fn_def_header_b(clean_b, p):
            fn_start = p
            break
        idx = p + len(nb)
    if fn_start is None:
        raise ValueError("missing function `%s`" % fn_name)
    pos = fn_start + len(nb)
    pd = brd = 0
    open_b = None
    while pos < len(clean_b):
        c = clean_b[pos]
        if c == 40:
            pd += 1
        elif c == 41:
            pd = max(0, pd - 1)
        elif c == 91:
            brd += 1
        elif c == 93:
            brd = max(0, brd - 1)
        elif c == 123 and pd == 0 and brd == 0:
            open_b = pos
            break
        pos += 1
    if open_b is None:
        raise ValueError("unopened body for `%s`" % fn_name)
    bd = 1
    cur = open_b + 1
    while cur < len(clean_b) and bd > 0:
        if clean_b[cur] == 123:
            bd += 1
        elif clean_b[cur] == 125:
            bd -= 1
            if bd == 0:
                return open_b, cur
        cur += 1
    raise ValueError("unclosed body for `%s`" % fn_name)


def _find_if_header_end(clean_b, if_pos, limit):
    k = if_pos + 2
    kp = kbr = 0
    while k < limit:
        c = clean_b[k]
        if c == 40:
            kp += 1
        elif c == 41:
            kp = max(0, kp - 1)
        elif c == 91:
            kbr += 1
        elif c == 93:
            kbr = max(0, kbr - 1)
        elif c == 123 and kp == 0 and kbr == 0:
            return k
        k += 1
    raise ValueError("unterminated `if` header at byte offset %d" % if_pos)


def _collect_top_if(clean_b, fn_name):
    bo, bc = _find_fn_body_span(clean_b, fn_name)
    bd, pd, brd = 1, 0, 0
    hdrs = []
    i = bo + 1
    while i < bc:
        if bd == 1 and pd == 0 and brd == 0 and _is_word_at(clean_b, i, b"if"):
            hdrs.append((i, _find_if_header_end(clean_b, i, bc)))
        c = clean_b[i]
        if c == 123:
            bd += 1
        elif c == 125:
            bd = max(0, bd - 1)
        elif c == 40:
            pd += 1
        elif c == 41:
            pd = max(0, pd - 1)
        elif c == 91:
            brd += 1
        elif c == 93:
            brd = max(0, brd - 1)
        i += 1
    return hdrs


def _parse_quoted_at(b, oq):
    if oq >= len(b) or b[oq] != 34:
        return None
    out = bytearray()
    i = oq + 1
    while i < len(b):
        ch = b[i]
        if ch == 92 and i + 1 < len(b):
            esc = b[i + 1]
            out.append({34: 34, 92: 92, 110: 10, 114: 13, 116: 9}.get(esc, esc))
            i += 2
            continue
        if ch == 34:
            return out.decode("utf-8", errors="replace"), i + 1
        out.append(ch)
        i += 1
    return None


def _skip_ws_comments_b(b, i):
    n = len(b)
    while i < n:
        if chr(b[i]).isspace():
            i += 1
        elif i + 1 < n and b[i:i + 2] == b"//":
            while i < n and b[i] != 10:
                i += 1
        elif i + 1 < n and b[i:i + 2] == b"/*":
            d = 1
            i += 2
            while i < n and d > 0:
                if b[i:i + 2] == b"/*":
                    d += 1
                    i += 2
                elif b[i:i + 2] == b"*/":
                    d -= 1
                    i += 2
                else:
                    i += 1
        else:
            break
    return i


def _extract_str_lits(rb):
    n = len(rb)
    out = []
    i = 0
    while i < n:
        if i + 1 < n and rb[i] == 47 and rb[i + 1] in (47, 42):
            i = _skip_ws_comments_b(rb, i)
        elif rb[i] == 34:
            pq = _parse_quoted_at(rb, i)
            if pq:
                out.append(pq[0])
                i = pq[1]
            else:
                i += 1
        elif rb[i] == 39:
            if i + 2 < n and rb[i + 1] == 92:
                k = i + 2
                lim = min(i + 10, n)
                while k < lim and rb[k] not in (39, 10):
                    k += 1
                if k < lim and rb[k] == 39:
                    i = k + 1
                    continue
            elif i + 2 < n and rb[i + 2] == 39 and rb[i + 1] != 10:
                i += 3
                continue
            i += 1
        else:
            i += 1
    return out


def _extract_var_matches(cb, rb, var_name):
    vb = var_name.encode("ascii")
    out = []
    i = 0
    while i < len(cb):
        if _is_word_at(cb, i, vb):
            after = i + len(vb)
            while after < len(cb) and chr(cb[after]).isspace():
                after += 1
            if _is_prec_matches(cb, i) and after < len(cb) and cb[after] == 44:
                ps = after + 1
                k = ps
                kp = 1
                kbr = 0
                pe = len(cb)
                while k < len(cb) and kp > 0:
                    if kp == 1 and kbr == 0 and _is_word_at(cb, k, b"if"):
                        pe = k
                        break
                    c = cb[k]
                    if c == 40:
                        kp += 1
                    elif c == 41:
                        kp -= 1
                        if kp == 0:
                            pe = k
                            break
                    elif c == 91:
                        kbr += 1
                    elif c == 93:
                        kbr = max(0, kbr - 1)
                    k += 1
                out.extend(_extract_str_lits(rb[ps:pe]))
                i = pe
                continue
            elif (
                after + 2 <= len(cb)
                and cb[after:after + 2] == b"=="
                and (after + 2 == len(cb) or cb[after + 2] != 61)
            ):
                lp = _skip_ws_comments_b(rb, after + 2)
                pq = _parse_quoted_at(rb, lp)
                if pq:
                    out.append(pq[0])
                    i = pq[1]
                    continue
        i += 1
    return out


def _scan_client_compat_rs(compat_path):
    raw_b = _read_bytes(compat_path)
    clean_s = strip_rust_comments_and_literals(raw_b.decode("latin1"))
    clean_b = clean_s.encode("latin1")

    lines = clean_s.splitlines()
    prod_lines = len(lines)
    for idx, ln in enumerate(lines):
        if "".join(ln.split()).startswith("#[cfg(test)]"):
            prod_lines = idx
            break

    known = set()
    rpc_hdrs = _collect_top_if(clean_b, "normalize_macho_rpc_reply_at")
    for s, e in rpc_hdrs:
        svcs = _extract_var_matches(clean_b[s:e], raw_b[s:e], "service")
        meths = _extract_var_matches(clean_b[s:e], raw_b[s:e], "method")
        if not svcs or not meths:
            raise ValueError("could not extract (service, method) literals from RPC compat header")
        for svc in svcs:
            for meth in meths:
                known.add("rpc:%s.%s" % (svc, meth))

    sup_hdrs = _collect_top_if(clean_b, "should_suppress_macho_notification")
    for s, e in sup_hdrs:
        evs = _extract_var_matches(clean_b[s:e], raw_b[s:e], "event")
        if not evs:
            raise ValueError("could not extract `event` literals from suppress header")
        for ev in evs:
            known.add("suppress:%s" % ev)

    bo, bc = _find_fn_body_span(clean_b, "normalize_macho_notification_with_sde")
    m_match = re.search(rb"\bmatch\s+event\s*\{", clean_b[bo + 1:bc])
    if not m_match:
        raise ValueError("missing `match event {` in `normalize_macho_notification_with_sde`")
    mo = bo + 1 + m_match.end() - 1
    bd = pd = brd = 0
    ps = mo + 1
    notify_arms = 0
    i = mo + 1
    while i < bc:
        if bd == 0 and pd == 0 and brd == 0:
            if clean_b[i] == 125:
                break
            if i + 1 < bc and clean_b[i:i + 2] == b"=>":
                evs = _extract_str_lits(raw_b[ps:i])
                if evs:
                    notify_arms += 1
                    for ev in evs:
                        known.add("notify:%s" % ev)
                ps = None
                i += 2
                continue
            if clean_b[i] == 44:
                ps = i + 1
                i += 1
                continue
        c = clean_b[i]
        if c == 123:
            bd += 1
        elif c == 125:
            bd = max(0, bd - 1)
            if bd == 0 and pd == 0 and brd == 0:
                ps = i + 1
        elif c == 40:
            pd += 1
        elif c == 41:
            pd = max(0, pd - 1)
        elif c == 91:
            brd += 1
        elif c == 93:
            brd = max(0, brd - 1)
        i += 1

    rewrites = 0
    uo, uc = _find_fn_body_span(clean_b, "normalize_destiny_update_arg")
    pd = brd = 0
    i = uo + 1
    while i < uc:
        if pd == 0 and brd == 0 and _is_word_at(clean_b, i, b"if"):
            he = _find_if_header_end(clean_b, i, uc)
            ns = _extract_var_matches(clean_b[i:he], raw_b[i:he], "name")
            if ns:
                rewrites += 1
                for n in ns:
                    known.add("destiny:update:%s" % n)
        c = clean_b[i]
        if c == 40:
            pd += 1
        elif c == 41:
            pd = max(0, pd - 1)
        elif c == 91:
            brd += 1
        elif c == 93:
            brd = max(0, brd - 1)
        i += 1

    po, pc = _find_fn_body_span(clean_b, "normalize_destiny_payload_arg")
    fm = re.search(rb"\.filter\s*\(", clean_b[po + 1:pc])
    if not fm:
        raise ValueError("missing `.filter(...)` in `normalize_destiny_payload_arg`")
    cur = po + 1 + fm.end()
    cp = 1
    cs = ce = None
    while cur < pc and cp > 0:
        if cp == 1 and clean_b[cur] == 123:
            cs = cur + 1
            cb2 = 1
            ce = cs
            while ce < pc and cb2 > 0:
                if clean_b[ce] == 123:
                    cb2 += 1
                elif clean_b[ce] == 125:
                    cb2 -= 1
                    if cb2 == 0:
                        break
                ce += 1
            break
        if clean_b[cur] == 40:
            cp += 1
        elif clean_b[cur] == 41:
            cp -= 1
        cur += 1
    if cs is None or ce is None:
        raise ValueError("missing filter closure body in `normalize_destiny_payload_arg`")

    last_semi = None
    lb = lp = lbr = 0
    for k in range(cs, ce):
        c = clean_b[k]
        if c == 123:
            lb += 1
        elif c == 125:
            lb = max(0, lb - 1)
        elif c == 40:
            lp += 1
        elif c == 41:
            lp = max(0, lp - 1)
        elif c == 91:
            lbr += 1
        elif c == 93:
            lbr = max(0, lbr - 1)
        elif c == 59 and lb == 0 and lp == 0 and lbr == 0:
            last_semi = k
    es = cs if last_semi is None else last_semi + 1
    conj_s = es
    cp = cb2 = cbr = 0
    conjs = []
    k = es
    while k < ce:
        if cb2 == 0 and cp == 0 and cbr == 0 and k + 1 < ce and clean_b[k:k + 2] == b"&&":
            conjs.append((conj_s, k))
            conj_s = k + 2
            k += 2
            continue
        c = clean_b[k]
        if c == 123:
            cb2 += 1
        elif c == 125:
            cb2 = max(0, cb2 - 1)
        elif c == 40:
            cp += 1
        elif c == 41:
            cp = max(0, cp - 1)
        elif c == 91:
            cbr += 1
        elif c == 93:
            cbr = max(0, cbr - 1)
        k += 1
    conjs.append((conj_s, ce))
    for a_i, b_i in conjs:
        if re.search(rb"\bfname\b", clean_b[a_i:b_i]):
            rewrites += 1
            for lit in _extract_str_lits(raw_b[a_i:b_i]):
                known.add("destiny:payload:%s" % lit)

    tm = re.search(rb"PyRep::Tuple\s*\([^)]*\)\s*=>\s*\{", clean_b[po:pc])
    if not tm:
        raise ValueError("missing `PyRep::Tuple(...) => {` arm in `normalize_destiny_payload_arg`")
    ao = po + tm.end() - 1
    ab = 1
    ac = ao + 1
    while ac < pc and ab > 0:
        if clean_b[ac] == 123:
            ab += 1
        elif clean_b[ac] == 125:
            ab -= 1
            if ab == 0:
                break
        ac += 1
    tp = tbr = 0
    ti = ao + 1
    while ti < ac:
        if tp == 0 and tbr == 0 and _is_word_at(clean_b, ti, b"if"):
            he = _find_if_header_end(clean_b, ti, ac)
            if _extract_var_matches(clean_b[ti:he], raw_b[ti:he], "name"):
                rewrites += 1
            for lit in _extract_str_lits(raw_b[ti:he]):
                known.add("destiny:payload:%s" % lit)
        c = clean_b[ti]
        if c == 40:
            tp += 1
        elif c == 41:
            tp = max(0, tp - 1)
        elif c == 91:
            tbr += 1
        elif c == 93:
            tbr = max(0, tbr - 1)
        ti += 1

    return {
        "rpc_rules": len(rpc_hdrs),
        "suppress_rules": len(sup_hdrs),
        "notify_arms": notify_arms,
        "destiny_rewrites": rewrites,
        "production_lines": prod_lines,
        "known_rules": known,
    }


def _iter_arch_debt_entries(doc):
    """Normalize supported `arch_debt_ratchet.json` layouts into `(name, spec_dict)` pairs."""
    if not isinstance(doc, dict):
        return []
    if isinstance(doc.get("patterns"), dict):
        return [(str(k), v) for k, v in doc["patterns"].items() if isinstance(v, dict)]
    if isinstance(doc.get("entries"), list):
        out = []
        for item in doc["entries"]:
            if isinstance(item, dict) and item.get("name"):
                out.append((str(item["name"]), item))
        return out
    return [
        (str(k), v)
        for k, v in doc.items()
        if k not in ("version", "notes", "description", "schema_version") and isinstance(v, dict)
    ]


def check_arch_debt_ratchet(root, rel_path="crates/eve-testkit/ratchets/arch_debt_ratchet.json"):
    """Conditionally validate `arch_debt_ratchet.json` (#1197) when present."""
    full_path = os.path.join(root, rel_path)
    if not os.path.isfile(full_path):
        return []
    try:
        doc = _read_json(full_path)
    except (OSError, ValueError) as e:
        return [("RATCHET_ARCH_DEBT", "Failed to parse %s: %s" % (rel_path, e), rel_path, 1)]

    entries = _iter_arch_debt_entries(doc)
    compiled = []
    violations = []
    for name, spec in entries:
        max_occ = spec.get("max_occurrences", spec.get("max_calls", spec.get("max_count")))
        max_files = spec.get("max_files")
        if max_occ is not None and (not isinstance(max_occ, int) or isinstance(max_occ, bool) or max_occ < 0):
            violations.append(("RATCHET_ARCH_DEBT", "%s has invalid max_occurrences=%r" % (name, max_occ), rel_path, 1))
            continue
        if max_files is not None and (not isinstance(max_files, int) or isinstance(max_files, bool) or max_files < 0):
            violations.append(("RATCHET_ARCH_DEBT", "%s has invalid max_files=%r" % (name, max_files), rel_path, 1))
            continue
        if spec.get("zero_required") is True and ((max_occ or 0) > 0 or (max_files or 0) > 0):
            violations.append((
                "RATCHET_ARCH_DEBT",
                "%s marks zero_required=true while max ceiling is non-zero (max_occurrences=%r, max_files=%r)"
                % (name, max_occ, max_files),
                rel_path,
                1,
            ))
            continue
        rx = None
        if isinstance(spec.get("regex"), str) and spec["regex"]:
            try:
                rx = re.compile(spec["regex"])
            except re.error as e:
                violations.append(("RATCHET_ARCH_DEBT", "%s has invalid regex: %s" % (name, e), rel_path, 1))
                continue
        elif isinstance(spec.get("literals"), list) and spec["literals"]:
            rx = re.compile("|".join(re.escape(str(x)) for x in spec["literals"] if x))
        elif name in ARCH_DEBT_PATTERNS:
            rx = ARCH_DEBT_PATTERNS[name]
        if rx is not None and (max_occ is not None or max_files is not None):
            compiled.append((name, spec, rx, max_occ, max_files))

    if not compiled:
        return violations

    src_dir = os.path.join(root, "crates", "eve-server", "src")
    if not os.path.isdir(src_dir):
        return violations

    obs = {name: [0, 0] for name, *_ in compiled}
    for p in sorted(glob.glob(os.path.join(src_dir, "**", "*.rs"), recursive=True)):
        rel = os.path.relpath(p, src_dir).replace("\\", "/")
        if is_test_only_rel_path(rel):
            continue
        raw = _read_text(p)
        prod = strip_cfg_test_items(strip_rust_comments_and_literals(raw))
        for name, _, rx, _, _ in compiled:
            c = len(rx.findall(prod))
            if c > 0:
                obs[name][0] += c
                obs[name][1] += 1

    for name, spec, _, max_occ, max_files in compiled:
        got_occ, got_files = obs[name]
        if max_occ is not None and got_occ > max_occ:
            violations.append((
                "RATCHET_ARCH_DEBT",
                "Architecture debt pattern '%s' regressed above ratchet: found %d occurrences (max %d)"
                % (name, got_occ, max_occ),
                rel_path,
                1,
            ))
        if max_files is not None and got_files > max_files:
            violations.append((
                "RATCHET_ARCH_DEBT",
                "Architecture debt pattern '%s' file count regressed above ratchet: found %d files (max %d)"
                % (name, got_files, max_files),
                rel_path,
                1,
            ))
        if spec.get("exact") is True:
            if max_occ is not None and got_occ < max_occ:
                violations.append((
                    "RATCHET_ARCH_DEBT",
                    "Architecture debt pattern '%s' dropped below ratchet (%d < %d); lower %s in the same commit"
                    % (name, got_occ, max_occ, rel_path),
                    rel_path,
                    1,
                ))
    return violations


def check_all_ratchets(root, cache_path=None):
    """Run all workspace source ratchets in < 1s without spawning `cargo`."""
    stat_path = None
    if cache_path is None:
        try:
            cache_path = os.path.join(rl.main_state_dir(root), RATCHET_CACHE_FILENAME)
            stat_path = os.path.join(rl.state_dir(root), RATCHET_STAT_FILENAME)
        except BaseException:  # noqa: BLE001
            cache_path = None
            stat_path = None

    cache = _load_cache(cache_path)
    stat_idx = _load_cache(stat_path) if stat_path else None
    src_cache = cache.setdefault("src_files", {})
    it_cache = cache.setdefault("it_files", {})
    lock_cache = cache.setdefault("lock_files", {})
    stat_src = stat_idx.setdefault("src_files", {}) if stat_idx is not None else None
    stat_it = stat_idx.setdefault("it_files", {}) if stat_idx is not None else None
    dirty = False
    stat_dirty = False
    violations = []

    enc_ratchet_rel = "crates/eve-proto/ratchets/raw_encode_field_ratchet.json"
    notify_ratchet_rel = "crates/eve-testkit/ratchets/raw_notify_ratchet.json"
    dispatch_ratchet_rel = "crates/eve-testkit/ratchets/raw_dispatch_ratchet.json"
    compat_ratchet_rel = "crates/eve-testkit/ratchets/client_compat_ratchet.json"

    has_enc = os.path.isfile(os.path.join(root, enc_ratchet_rel))
    has_notify = os.path.isfile(os.path.join(root, notify_ratchet_rel))
    has_dispatch = os.path.isfile(os.path.join(root, dispatch_ratchet_rel))
    has_compat = os.path.isfile(os.path.join(root, compat_ratchet_rel))

    src_dir = os.path.join(root, "crates", "eve-server", "src")
    it_dir = os.path.join(root, "crates", "eve-server", "tests", "it")

    enc_calls = enc_files = 0
    notify_calls = notify_files = 0
    transport_conditionals = 0
    service_ct_cmp = 0
    service_ct_params = 0
    service_ct_files = set()
    direct_layer_test_calls = 0

    if (has_enc or has_notify or has_compat) and os.path.isdir(src_dir):
        for p in sorted(glob.glob(os.path.join(src_dir, "**", "*.rs"), recursive=True)):
            rel_in_src = os.path.relpath(p, src_dir).replace("\\", "/")
            rec, chg, st_chg = _scan_server_src_file(p, rel_in_src, src_cache, stat_src=stat_src)
            dirty = dirty or chg
            stat_dirty = stat_dirty or st_chg
            if rec["enc"] > 0:
                enc_calls += rec["enc"]
                enc_files += 1
            if rec["notify"] > 0:
                notify_calls += rec["notify"]
                notify_files += 1
            transport_conditionals += rec["ct_cmp"]
            direct_layer_test_calls += rec["dl_test"]
            if rel_in_src.startswith("services/") and (rec["ct_cmp"] > 0 or rec["ct_params"] > 0):
                service_ct_cmp += rec["ct_cmp"]
                service_ct_params += rec["ct_params"]
                service_ct_files.add("crates/eve-server/src/" + rel_in_src)

    disp_none_calls = disp_none_files = 0
    it_dispatch_files = set()
    if (has_dispatch or has_compat) and os.path.isdir(it_dir):
        for p in sorted(glob.glob(os.path.join(it_dir, "**", "*.rs"), recursive=True)):
            rel_in_it = os.path.relpath(p, it_dir).replace("\\", "/")
            rec, chg, st_chg = _scan_it_file(p, rel_in_it, it_cache, stat_it=stat_it)
            dirty = dirty or chg
            stat_dirty = stat_dirty or st_chg
            if rec["raw_none"] > 0:
                disp_none_calls += rec["raw_none"]
                disp_none_files += 1
            if rec["has_disp"]:
                it_dispatch_files.add(rel_in_it)
            direct_layer_test_calls += rec["dl_test"]

    # Also check any non-`it` integration tests under `crates/eve-server/tests/*.rs` for `client_compat`
    if has_compat:
        extra_tests_dir = os.path.join(root, "crates", "eve-server", "tests")
        if os.path.isdir(extra_tests_dir):
            for p in sorted(glob.glob(os.path.join(extra_tests_dir, "*.rs"))):
                raw = _read_text(p)
                if "normalize_macho_" in raw or "should_suppress_macho_notification" in raw:
                    direct_layer_test_calls += count_direct_layer_calls(strip_rust_comments_and_literals(raw))

    if has_enc:
        try:
            er = _read_json(os.path.join(root, enc_ratchet_rel))
            max_c = int(er["max_raw_encode_field_calls"])
            max_f = int(er["max_files_with_raw_encode_field"])
            if enc_calls > max_c:
                violations.append((
                    "RATCHET_RAW_ENCODE_FIELD",
                    "Raw encode_*_field(...) count regressed above falling ratchet: found %d, max_raw_encode_field_calls=%d. Use eve_proto::pb::message(type_name) instead."
                    % (enc_calls, max_c),
                    enc_ratchet_rel,
                    1,
                ))
            if enc_files > max_f:
                violations.append((
                    "RATCHET_RAW_ENCODE_FIELD",
                    "Files containing encode_*_field(...) regressed above ratchet: found %d, max_files_with_raw_encode_field=%d."
                    % (enc_files, max_f),
                    enc_ratchet_rel,
                    1,
                ))
        except (OSError, ValueError, KeyError, TypeError) as e:
            violations.append(("RATCHET_RAW_ENCODE_FIELD", "Invalid %s: %s" % (enc_ratchet_rel, e), enc_ratchet_rel, 1))

    if has_notify:
        try:
            nr = _read_json(os.path.join(root, notify_ratchet_rel))
            max_c = int(nr["max_raw_notify_calls"])
            max_f = int(nr["max_files_with_raw_notify"])
            if notify_calls > max_c:
                violations.append((
                    "RATCHET_RAW_NOTIFY",
                    "Raw untyped notify call count regressed above falling ratchet: found %d calls across %d files, max_raw_notify_calls=%d, max_files_with_raw_notify=%d. Route new/modified notifications through router::notify_registry instead."
                    % (notify_calls, notify_files, max_c, max_f),
                    notify_ratchet_rel,
                    1,
                ))
            if notify_files > max_f:
                violations.append((
                    "RATCHET_RAW_NOTIFY",
                    "Files containing raw untyped notify calls regressed above ratchet: found %d files (%d calls), max_files_with_raw_notify=%d."
                    % (notify_files, notify_calls, max_f),
                    notify_ratchet_rel,
                    1,
                ))
        except (OSError, ValueError, KeyError, TypeError) as e:
            violations.append(("RATCHET_RAW_NOTIFY", "Invalid %s: %s" % (notify_ratchet_rel, e), notify_ratchet_rel, 1))

    if has_dispatch:
        try:
            dr = _read_json(os.path.join(root, dispatch_ratchet_rel))
            max_c = int(dr["max_raw_dispatch_calls"])
            max_f = int(dr["max_files_with_raw_dispatch"])
            if disp_none_calls > max_c:
                violations.append((
                    "RATCHET_RAW_DISPATCH",
                    "Raw ctx.dispatch_rpc(..., None) count regressed above falling ratchet: found %d, max_raw_dispatch_calls=%d. Route new/modified calls through ClientHarness instead of None."
                    % (disp_none_calls, max_c),
                    dispatch_ratchet_rel,
                    1,
                ))
            if disp_none_files > max_f:
                violations.append((
                    "RATCHET_RAW_DISPATCH",
                    "Files containing ctx.dispatch_rpc(..., None) regressed above ratchet: found %d, max_files_with_raw_dispatch=%d."
                    % (disp_none_files, max_f),
                    dispatch_ratchet_rel,
                    1,
                ))
        except (OSError, ValueError, KeyError, TypeError) as e:
            violations.append(("RATCHET_RAW_DISPATCH", "Invalid %s: %s" % (dispatch_ratchet_rel, e), dispatch_ratchet_rel, 1))

    lr_viol, lr_dirty = check_lock_rank_ratchet(root, lock_cache=lock_cache)
    violations.extend(lr_viol)
    dirty = dirty or lr_dirty

    if has_compat:
        compat_rs = os.path.join(root, "crates", "eve-server", "src", "router", "client_compat.rs")
        try:
            cr = _read_json(os.path.join(root, compat_ratchet_rel))
            compat_rep = _scan_client_compat_rs(compat_rs)
            for field in ("known_rules", "allowed_service_caller_trace_files", "allowed_it_dispatch_rpc_files"):
                arr = cr.get(field, [])
                if not isinstance(arr, list) or any(arr[i] >= arr[i + 1] for i in range(len(arr) - 1)):
                    violations.append((
                        "RATCHET_CLIENT_COMPAT",
                        "%s in %s must be strictly sorted and deduplicated" % (field, compat_ratchet_rel),
                        compat_ratchet_rel,
                        1,
                    ))
            exact_pairs = (
                ("max_rpc_rules", compat_rep["rpc_rules"]),
                ("max_suppress_rules", compat_rep["suppress_rules"]),
                ("max_notify_arms", compat_rep["notify_arms"]),
                ("max_destiny_rewrites", compat_rep["destiny_rewrites"]),
                ("max_production_lines", compat_rep["production_lines"]),
                ("max_transport_conditionals", transport_conditionals),
                ("max_direct_layer_test_calls", direct_layer_test_calls),
                ("max_service_caller_trace_comparisons", service_ct_cmp),
                ("max_service_caller_trace_params", service_ct_params),
            )
            for fname, obs_val in exact_pairs:
                exp_val = int(cr[fname])
                if obs_val > exp_val:
                    violations.append((
                        "RATCHET_CLIENT_COMPAT",
                        "%s regressed above falling ratchet: observed %d, %s=%d"
                        % (fname, obs_val, fname, exp_val),
                        compat_ratchet_rel,
                        1,
                    ))
                elif obs_val < exp_val:
                    violations.append((
                        "RATCHET_CLIENT_COMPAT",
                        "%s dropped below falling ratchet (%d < %d): lower %s to %d in %s in the same commit"
                        % (fname, obs_val, exp_val, fname, obs_val, compat_ratchet_rel),
                        compat_ratchet_rel,
                        1,
                    ))
            exp_rules = set(cr.get("known_rules", ()))
            added_rules = sorted(compat_rep["known_rules"] - exp_rules)
            removed_rules = sorted(exp_rules - compat_rep["known_rules"])
            if added_rules:
                violations.append((
                    "RATCHET_CLIENT_COMPAT",
                    "Unlisted client_compat rules added without updating %s: %s" % (compat_ratchet_rel, added_rules[:10]),
                    compat_ratchet_rel,
                    1,
                ))
            if removed_rules:
                violations.append((
                    "RATCHET_CLIENT_COMPAT",
                    "client_compat rules deleted without removing from %s in same commit: %s"
                    % (compat_ratchet_rel, removed_rules[:10]),
                    compat_ratchet_rel,
                    1,
                ))
            exp_svc = set(cr.get("allowed_service_caller_trace_files", ()))
            if service_ct_files != exp_svc:
                violations.append((
                    "RATCHET_CLIENT_COMPAT",
                    "services/ caller_trace file set %s != allowed_service_caller_trace_files %s"
                    % (sorted(service_ct_files), sorted(exp_svc)),
                    compat_ratchet_rel,
                    1,
                ))
            allowed_it = set(cr.get("allowed_it_dispatch_rpc_files", ()))
            disallowed_it = sorted(it_dispatch_files - allowed_it)
            if disallowed_it:
                violations.append((
                    "RATCHET_CLIENT_COMPAT",
                    "Integration test files outside allowed_it_dispatch_rpc_files contain .dispatch_rpc(: %s"
                    % disallowed_it[:10],
                    compat_ratchet_rel,
                    1,
                ))
        except (OSError, ValueError, KeyError, TypeError) as e:
            violations.append(("RATCHET_CLIENT_COMPAT", "Invalid client_compat ratchet state: %s" % e, compat_ratchet_rel, 1))

    violations.extend(check_arch_debt_ratchet(root))

    if dirty:
        _save_cache(cache_path, cache)
    if stat_dirty and stat_path:
        _save_cache(stat_path, stat_idx)
    return violations


# ---------------------------------------------------------------------------
# Umbrella / RFC `Fixes #<N>` Auto-Close Guard & `<= 5` Batch Cap
# ---------------------------------------------------------------------------

def _parse_issue_numbers(val):
    nums = set()
    if isinstance(val, int) and not isinstance(val, bool) and val > 0:
        nums.add(val)
    elif isinstance(val, str):
        for m in re.findall(r"\d+", val):
            nums.add(int(m))
    elif isinstance(val, list):
        for item in val:
            nums |= _parse_issue_numbers(item)
    return nums


def protected_umbrella_issues(root):
    """Return `{issue_num: reason_str}` of umbrella RFC/architecture issues and non-zero ratchet
    `owning_issue`s that must NOT be auto-closed via `Fixes #N` / `Closes #N` / `Resolves #N`."""
    protected = {
        n: "open RFC/architecture umbrella tracker"
        for n in DEFAULT_UMBRELLA_ISSUES
    }

    # Check if #932 or #933 ratchets have reached complete zero across all owning ratchets.
    notify_path = os.path.join(root, "crates/eve-testkit/ratchets/raw_notify_ratchet.json")
    if os.path.isfile(notify_path):
        try:
            nr = _read_json(notify_path)
            if int(nr.get("max_raw_notify_calls", 1)) == 0:
                protected.pop(932, None)
        except (OSError, ValueError, TypeError):
            pass

    compat_path = os.path.join(root, "crates/eve-testkit/ratchets/client_compat_ratchet.json")
    enc_path = os.path.join(root, "crates/eve-proto/ratchets/raw_encode_field_ratchet.json")
    if os.path.isfile(compat_path) and os.path.isfile(enc_path):
        try:
            cr = _read_json(compat_path)
            er = _read_json(enc_path)
            compat_counters = [
                int(cr.get(k, 1) or 0)
                for k in (
                    "max_rpc_rules",
                    "max_suppress_rules",
                    "max_notify_arms",
                    "max_destiny_rewrites",
                    "max_transport_conditionals",
                    "max_service_caller_trace_comparisons",
                    "max_service_caller_trace_params",
                )
            ]
            if int(er.get("max_raw_encode_field_calls", 1)) == 0 and sum(compat_counters) == 0:
                protected.pop(933, None)
            else:
                for iss in _parse_issue_numbers(cr.get("owning_issue")):
                    protected[iss] = "non-zero client_compat_ratchet.json"
        except (OSError, ValueError, TypeError):
            pass

    arch_path = os.path.join(root, "crates/eve-testkit/ratchets/arch_debt_ratchet.json")
    if os.path.isfile(arch_path):
        try:
            ad = _read_json(arch_path)
            by_issue = {}
            for name, spec in _iter_arch_debt_entries(ad):
                cnt = spec.get("max_occurrences", spec.get("max_calls", spec.get("max_count", 0)))
                cnt_i = int(cnt) if isinstance(cnt, int) and not isinstance(cnt, bool) else 0
                for iss in _parse_issue_numbers(spec.get("owning_issue", spec.get("owning_issues"))):
                    by_issue.setdefault(iss, []).append((name, cnt_i))
            for iss, items in by_issue.items():
                nonzero = [nm for nm, c in items if c > 0]
                if nonzero:
                    protected[iss] = "non-zero arch_debt_ratchet.json (%s)" % ", ".join(nonzero[:3])
                elif iss not in DEFAULT_UMBRELLA_ISSUES:
                    protected.pop(iss, None)
        except (OSError, ValueError, TypeError):
            pass

    return protected


def extract_closing_issues(commit_message):
    """Extract sorted distinct GitHub issue numbers targeted by auto-closing keywords in `commit_message`."""
    closed = set()
    for m in CLOSING_CLAUSE_RE.finditer(commit_message):
        for num_str in ISSUE_NUM_RE.findall(m.group(0)):
            closed.add(int(num_str))
    return sorted(closed)


def lint_commit_messages_for_issue_closures(commits, protected_issues, claims_allow_batch=False, max_fixes=MAX_FIXES_PER_COMMIT):
    """Validate `(sha, message)` pairs from a review branch (`base..HEAD`)."""
    violations = []
    for sha, msg in commits:
        short = sha[:9]
        closing = extract_closing_issues(msg)
        if not closing:
            continue
        for iss in closing:
            if iss in protected_issues:
                violations.append((
                    "UMBRELLA_ISSUE_AUTOCLOSE",
                    "Commit %s uses a GitHub closing keyword on #%d (%s). Use `Refs #%d` instead until its ratchet reaches 0."
                    % (short, iss, protected_issues[iss], iss),
                    None,
                    None,
                ))
        non_umbrella = [iss for iss in closing if iss not in protected_issues]
        allow_batch = claims_allow_batch or bool(BATCH_FIXES_ALLOW_RE.search(msg))
        if len(closing) > max_fixes and not allow_batch:
            sample = ", ".join("#%d" % i for i in (non_umbrella or closing)[:8])
            violations.append((
                "TOO_MANY_FIXES_PER_COMMIT",
                "Commit %s cites %d distinct issues with Fixes/Closes/Resolves (%s; max %d per commit). "
                "Keep tracks single-focused or add a `batch-fixes: allow <reason>` trailer when genuinely required."
                % (short, len(closing), sample, max_fixes),
                None,
                None,
            ))
    return violations


def lint_branch_commit_issue_closures(root, claims_path=None):
    """Inspect unmerged review-branch commits (`HEAD` not in `origin/main` or `main`).
    Passes cleanly on `origin/main` / `main` itself (`0` unmerged commits)."""
    trunk_refs = []
    for ref in ("origin/main", "main"):
        if subprocess.run(["git", "rev-parse", "--verify", "-q", ref], cwd=root, capture_output=True).returncode == 0:
            trunk_refs.append(ref)
    if not trunk_refs:
        return []
    proc = subprocess.run(
        ["git", "log", "--format=%H%x00%B%x1e", "HEAD", "--not"] + trunk_refs,
        cwd=root,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0 or not proc.stdout.strip():
        return []
    commits = []
    for rec in proc.stdout.split("\x1e"):
        rec = rec.strip()
        if not rec or "\x00" not in rec:
            continue
        sha, body = rec.split("\x00", 1)
        commits.append((sha.strip(), body))
    if not commits:
        return []
    claims_allow = False
    if claims_path and os.path.isfile(claims_path):
        try:
            claims_allow = bool(BATCH_FIXES_ALLOW_RE.search(_read_text(claims_path)))
        except OSError:
            pass
    protected = protected_umbrella_issues(root)
    return lint_commit_messages_for_issue_closures(commits, protected, claims_allow_batch=claims_allow)
