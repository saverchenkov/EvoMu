# -*- coding: utf-8 -*-
"""Shared host-side GM slash command rewriting and sim-time extraction helpers."""


def looks_like_char_id_token(tok, target_char_id):
    t = str(tok or "").strip()
    if not t or not t.isdigit():
        return False
    n = int(t)
    return n == int(target_char_id or 0) or (90000000 <= n < 1000000000)


def looks_like_entity_or_location_token(tok, target_char_id):
    t = str(tok or "").strip()
    if not t or not t.isdigit():
        return False
    n = int(t)
    return (
        n == int(target_char_id or 0)
        or (60000000 <= n < 70000000)
        or (90000000 <= n < 1000000000)
        or n >= 1000000000
    )


def rewrite_gm_slash_for_target(cmd, target_char_id):
    raw = str(cmd or "").strip()
    tgt_id = int(target_char_id or 0)
    if not raw.startswith("/") or tgt_id <= 0:
        return raw
    parts = raw.split()
    verb = parts[0].lower()
    rest = parts[1:]
    tgt_s = str(tgt_id)
    me_tokens = ("me", "myship", "self")

    if verb == "/giveisk":
        if len(rest) == 1:
            return "/giveisk %s %s" % (tgt_s, rest[0])
        if len(rest) >= 2 and rest[0].lower() in ("me", "self"):
            return " ".join(["/giveisk", tgt_s] + rest[1:])
    elif verb in ("/giveskill", "/setskill"):
        if len(rest) == 2:
            return "%s %s %s %s" % (parts[0], tgt_s, rest[0], rest[1])
        if len(rest) >= 3 and rest[0].lower() in ("me", "self"):
            return " ".join([parts[0], tgt_s] + rest[1:])
    elif verb == "/heal":
        if not rest or rest[0].lower() in ("me", "self"):
            return "/heal %s" % tgt_s
    elif verb in ("/teleport", "/tr", "/translocate", "/moveme"):
        noblock_tokens = [t for t in rest if t.lower() == "noblock"]
        non_noblock = [t for t in rest if t.lower() != "noblock"]
        if len(non_noblock) == 1:
            return " ".join([parts[0], tgt_s, non_noblock[0]] + noblock_tokens)
        if len(non_noblock) == 2:
            if non_noblock[0].lower() in me_tokens:
                return " ".join([parts[0], tgt_s, non_noblock[1]] + noblock_tokens)
            if non_noblock[1].lower() in me_tokens:
                return " ".join([parts[0], tgt_s, non_noblock[0]] + noblock_tokens)
    elif verb in ("/giveitem", "/createitem", "/spawn", "/spawnnpc", "/spawnwreck", "/spawnanomaly"):
        if not rest:
            return "%s %s" % (parts[0], tgt_s)
        if rest[0].lower() in me_tokens:
            return " ".join([parts[0], tgt_s] + rest[1:])
        if not looks_like_char_id_token(rest[0], tgt_id):
            return " ".join([parts[0], tgt_s] + rest)
    elif verb in ("/load", "/fit"):
        if len(rest) >= 2 and rest[0].lower() in me_tokens:
            return " ".join([parts[0], tgt_s] + rest[1:])
        if rest and not looks_like_entity_or_location_token(rest[0], tgt_id):
            return " ".join([parts[0], tgt_s] + rest)
    elif verb == "/setattr":
        if len(rest) >= 3 and rest[0].lower() in me_tokens:
            return " ".join([parts[0], tgt_s] + rest[1:])
        if len(rest) == 2 and not looks_like_entity_or_location_token(rest[0], tgt_id):
            return " ".join([parts[0], tgt_s] + rest)
    elif verb == "/getattr":
        if len(rest) >= 2 and rest[0].lower() in me_tokens:
            return " ".join([parts[0], tgt_s] + rest[1:])
        if len(rest) == 1 and not looks_like_entity_or_location_token(rest[0], tgt_id):
            return " ".join([parts[0], tgt_s] + rest)
    elif verb in ("/energy", "/secstatus"):
        if len(rest) == 1:
            return "%s %s %s" % (parts[0], tgt_s, rest[0])
        if len(rest) >= 2 and rest[0].lower() in ("me", "self"):
            return " ".join([parts[0], tgt_s] + rest[1:])
    elif verb == "/cleartimers":
        if not rest or rest[0].lower() in ("me", "self"):
            return "%s %s" % (parts[0], tgt_s)
    elif verb in ("/completeskills", "/trainskills", "/finishskills"):
        if not rest:
            return "%s %s all" % (parts[0], tgt_s)
        if rest[0].lower() in ("me", "self"):
            return " ".join([parts[0], tgt_s] + (rest[1:] or ["all"]))
        if not looks_like_char_id_token(rest[0], tgt_id):
            return " ".join([parts[0], tgt_s] + rest)
    elif verb in (
        "/completeindustry",
        "/finishjobs",
        "/industrytimer",
        "/upwelltimer",
        "/structuretimer",
        "/finishstructure",
    ):
        if not rest:
            return "%s %s" % (parts[0], tgt_s)
        if rest[0].lower() in ("me", "self"):
            return " ".join([parts[0], tgt_s] + rest[1:])
        if not looks_like_char_id_token(rest[0], tgt_id):
            return " ".join([parts[0], tgt_s] + rest)
    elif verb in ("/advancetime", "/fastforward", "/ff"):
        if not rest:
            return "%s %s" % (parts[0], tgt_s)
        if rest[0].lower() in ("me", "self"):
            return " ".join([parts[0], tgt_s] + rest[1:])
        if len(rest) >= 2 and rest[-1].lower() in ("me", "self"):
            return " ".join([parts[0], tgt_s] + rest[:-1])
        if not any(looks_like_char_id_token(t, tgt_id) for t in rest):
            return " ".join([parts[0], tgt_s] + rest)
    return raw


def extract_slash_sim_advance_seconds(cmd, gm_res=None, target_char_id=0):
    """Return simulated seconds advanced by /advancetime, /fastforward, or /ff, else None."""
    parts = str(cmd or "").strip().split()
    if not parts:
        return None
    verb = parts[0].lower()
    if verb not in ("/advancetime", "/fastforward", "/ff"):
        return None
    adv_raw = None
    if isinstance(gm_res, dict) and gm_res.get("advancedSeconds") is not None:
        adv_raw = gm_res.get("advancedSeconds")
    elif gm_res is not None and getattr(gm_res, "advancedSeconds", None) is not None:
        adv_raw = getattr(gm_res, "advancedSeconds")
    if adv_raw is not None:
        try:
            return max(0.0, float(adv_raw))
        except Exception:
            pass
    tgt_i = int(target_char_id or 0)
    for tok in parts[1:]:
        low = tok.lower()
        if low in ("me", "self"):
            continue
        if looks_like_char_id_token(tok, tgt_i):
            continue
        try:
            return max(0.0, float(int(tok)))
        except Exception:
            try:
                return max(0.0, float(tok))
            except Exception:
                continue
    return 3600.0
