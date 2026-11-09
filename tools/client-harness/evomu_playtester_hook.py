# -*- coding: utf-8 -*-
"""In-client Stackless Python 2.7 interactive hook for the EvoMu Autonomous Playtester.

Loaded inside the real V24.01 `exefile` process when `EVOMU_PLAYTESTER_MODE=1`.
Provides a localhost TCP JSON-RPC server that:
  1. Ensures the client logs in and enters the initial hangar/space view.
  2. Records an in-band flight recorder of client MachoNet & Quasar requests and UI modals.
  3. Inspects the live `uicore` scene graph (open windows, tabs, clickable buttons, modals, context menus).
  4. Queries live session, ship, Dogma, Destiny ballpark, warp FX (`effects.Warping`), and Quasar channel telemetry.
  5. Executes UI-first actions (widget clicks, drags, key/command shortcuts, window toggles, real-time warp,
     skill training/plan UI flows, PLEX/NES store flows, and slash command fuzzing).
"""

import __builtin__
import json
import math
import os
import socket
import sys
import threading
import time
import traceback

import evomu_inclient_helpers as H

sys.dont_write_bytecode = True

_STARTED = False
_FLIGHT_RECORDER = []
_FLIGHT_SEQ = 0
_LAST_OBSERVED_SEQ = 0
_RECENT_MODALS = []
_RECENT_SCATTERS = []
_SCATTER_SEQ = 0
_HOOKED_REMOTESVC = False

_WARP_TELEMETRY = {
    "hooked": False,
    "in_warp": False,
    "started_count": 0,
    "finished_count": 0,
    "last_warp_saw_mode3": False,
    "last_warp_saw_fx_warping": False,
    "last_warp_saw_signal": False,
    "last_warp_peak_speed_mps": 0.0,
    "last_warp_samples": [],
}


def _record_flight(kind, service, method, args_summary, status, duration_ms, error=""):
    global _FLIGHT_SEQ
    _FLIGHT_SEQ += 1
    entry = {
        "seq": _FLIGHT_SEQ,
        "ts": round(time.time(), 3),
        "kind": str(kind),
        "service": str(service),
        "method": str(method),
        "args": str(args_summary)[:160],
        "status": str(status),
        "duration_ms": round(float(duration_ms), 2),
        "error": str(error)[:240] if error else "",
    }
    _FLIGHT_RECORDER.append(entry)
    if len(_FLIGHT_RECORDER) > 150:
        del _FLIGHT_RECORDER[:-120]


def _get_uicore():
    uc = getattr(__builtin__, "uicore", None)
    if uc is not None:
        return uc
    try:
        from carbonui.uicore import uicore as _uc
        return _uc
    except Exception:
        return None


def _wrap_message_owner(owner):
    if owner is None or not hasattr(owner, "Message"):
        return
    orig_msg = getattr(owner, "Message", None)
    if orig_msg is None or getattr(orig_msg, "_evomu_pt_wrapped", False):
        return

    def _wrapped_msg(*args, **kwargs):
        try:
            _RECENT_MODALS.append({
                "ts": round(time.time(), 3),
                "msg_key": str(args[0]) if args else "",
                "params": repr(args[1])[:240] if len(args) > 1 else "",
            })
            if len(_RECENT_MODALS) > 40:
                del _RECENT_MODALS[:-30]
        except Exception:
            pass
        return orig_msg(*args, **kwargs)

    _wrapped_msg._evomu_pt_wrapped = True
    try:
        setattr(owner, "Message", _wrapped_msg)
    except Exception:
        pass


def _install_flight_recorder_hooks(sm):
    global _HOOKED_REMOTESVC
    try:
        _wrap_message_owner(_get_uicore())
        _wrap_message_owner(getattr(__builtin__, "eve", None))
    except Exception:
        pass

    if _HOOKED_REMOTESVC or sm is None:
        return
    _HOOKED_REMOTESVC = True

    try:
        orig_remote = getattr(sm, "RemoteSvc", None)
        if orig_remote is not None and not getattr(orig_remote, "_evomu_pt_wrapped", False):
            class _RemoteMethodProxy(object):
                def __init__(self, svc_name, method_name, fn):
                    self._svc = svc_name
                    self._meth = method_name
                    self._fn = fn

                def __call__(self, *args, **kwargs):
                    t0 = time.time()
                    try:
                        res = self._fn(*args, **kwargs)
                        dur_ms = (time.time() - t0) * 1000.0
                        _record_flight("macho", self._svc, self._meth, repr(args)[:140], "OK", dur_ms)
                        return res
                    except Exception as exc:
                        dur_ms = (time.time() - t0) * 1000.0
                        _record_flight("macho", self._svc, self._meth, repr(args)[:140], "ERR", dur_ms, repr(exc))
                        raise

            class _RemoteSvcProxy(object):
                def __init__(self, svc_name, inner):
                    self._svc = svc_name
                    self._inner = inner

                def __getattr__(self, name):
                    attr = getattr(self._inner, name)
                    if callable(attr) and not str(name).startswith("_"):
                        return _RemoteMethodProxy(self._svc, str(name), attr)
                    return attr

            def _wrapped_remote_svc(svc_name, *args, **kwargs):
                inner = orig_remote(svc_name, *args, **kwargs)
                if inner is None:
                    return None
                return _RemoteSvcProxy(str(svc_name), inner)

            _wrapped_remote_svc._evomu_pt_wrapped = True
            sm.RemoteSvc = _wrapped_remote_svc
    except Exception:
        pass

    try:
        orig_scatter = getattr(sm, "ScatterEvent", None)
        if orig_scatter is not None and not getattr(orig_scatter, "_evomu_pt_wrapped", False):
            def _wrapped_scatter(event_name, *args, **kwargs):
                global _SCATTER_SEQ
                try:
                    ename = str(event_name)
                    if ename.startswith("On"):
                        _SCATTER_SEQ += 1
                        _RECENT_SCATTERS.append({
                            "seq": _SCATTER_SEQ,
                            "ts": round(time.time(), 3),
                            "event": ename,
                            "args": repr(args)[:140],
                        })
                        if len(_RECENT_SCATTERS) > 120:
                            del _RECENT_SCATTERS[:-90]
                except Exception:
                    pass
                return orig_scatter(event_name, *args, **kwargs)

            _wrapped_scatter._evomu_pt_wrapped = True
            sm.ScatterEvent = _wrapped_scatter
    except Exception:
        pass


def _on_pt_warp_started(*_args, **_kwargs):
    _WARP_TELEMETRY["in_warp"] = True
    _WARP_TELEMETRY["started_count"] += 1
    _WARP_TELEMETRY["last_warp_saw_signal"] = True


def _on_pt_warp_finished(*_args, **_kwargs):
    _WARP_TELEMETRY["in_warp"] = False
    _WARP_TELEMETRY["finished_count"] += 1


def _install_warp_telemetry(sm):
    if _WARP_TELEMETRY["hooked"]:
        return
    try:
        from eve.client.script.environment.effects import warp_signals
        sig_start = getattr(warp_signals, "on_warp_activated", None) or getattr(warp_signals, "on_warp_started", None)
        if sig_start is not None:
            sig_start.connect(_on_pt_warp_started)
        warp_signals.on_warp_finished.connect(_on_pt_warp_finished)
        _WARP_TELEMETRY["hooked"] = True
    except Exception:
        pass


def _safe_str(val, max_len=120):
    if val is None:
        return ""
    try:
        if isinstance(val, unicode):
            s = val.encode("utf-8", "replace")
        else:
            s = str(val)
        s = " ".join(s.split())
        return s[:max_len]
    except Exception:
        return ""


def _widget_abs_rect(node):
    try:
        if hasattr(node, "GetAbsolute"):
            res = node.GetAbsolute()
            if isinstance(res, (tuple, list)) and len(res) == 4:
                return int(res[0]), int(res[1]), int(res[2]), int(res[3])
        l = int(getattr(node, "displayX", getattr(node, "left", 0)) or 0)
        t = int(getattr(node, "displayY", getattr(node, "top", 0)) or 0)
        w = int(getattr(node, "displayWidth", getattr(node, "width", 0)) or 0)
        h = int(getattr(node, "displayHeight", getattr(node, "height", 0)) or 0)
        return l, t, w, h
    except Exception:
        return 0, 0, 0, 0


def _is_visible(node):
    if node is None or getattr(node, "destroyed", False):
        return False
    if not getattr(node, "display", True):
        return False
    state = getattr(node, "state", None)
    # In CarbonUI (carbonui.uiconst): UI_NORMAL=0, UI_DISABLED=1, UI_HIDDEN=2, UI_PICKCHILDREN=3
    if state == 2:
        return False
    return True


def _is_pickable(node):
    if not _is_visible(node):
        return False
    state = getattr(node, "state", None)
    # When a container or widget has state == 1 (UI_DISABLED / TR2_SPS_OFF) or isDisabled,
    # Trinity skips picking the node and its entire subtree.
    if state in (1, 2):
        return False
    if getattr(node, "disabled", False) or getattr(node, "isDisabled", False):
        return False
    return True


def _strip_tags(text):
    if not text:
        return ""
    out = []
    in_tag = 0
    for ch in text:
        if ch == "<":
            in_tag += 1
        elif ch == ">" and in_tag > 0:
            in_tag -= 1
        elif in_tag == 0:
            out.append(ch)
    return " ".join("".join(out).split())


def _extract_widget_label(node):
    if node is None:
        return ""
    for attr in ("text", "caption", "label", "_label_text", "rawText", "hint"):
        val = getattr(node, attr, None)
        if val is None or callable(val):
            continue
        if hasattr(val, "text") and not isinstance(val, (str, unicode)):
            val = getattr(val, "text", None)
        s = _strip_tags(_safe_str(val, 120))
        if s and not s.startswith("<"):
            return s[:80]
    for child_obj_attr in ("_label", "label", "caption", "title", "_message_label"):
        sub_obj = getattr(node, child_obj_attr, None)
        if sub_obj is not None and not isinstance(sub_obj, (str, unicode)):
            s = _strip_tags(_safe_str(getattr(sub_obj, "text", None), 120))
            if s:
                return s[:80]
    sr = getattr(node, "sr", None)
    if sr is not None:
        for sub in ("label", "caption", "text"):
            sub_node = getattr(sr, sub, None)
            if sub_node is not None:
                s = _strip_tags(_safe_str(getattr(sub_node, "text", sub_node), 120))
                if s and not s.startswith("<"):
                    return s[:80]
        sr_node = getattr(sr, "node", None)
        if sr_node is not None:
            for k in ("label", "name", "text", "caption"):
                v = sr_node.get(k) if isinstance(sr_node, dict) else getattr(sr_node, k, None)
                s = _strip_tags(_safe_str(v, 120))
                if s and not s.startswith("<"):
                    return s[:80]
    children = getattr(node, "children", None)
    if children:
        parts = []
        try:
            for ch in list(children)[:8]:
                if getattr(ch, "destroyed", False) or not getattr(ch, "display", True):
                    continue
                ch_txt = getattr(ch, "text", None)
                if ch_txt and not callable(ch_txt):
                    s = _strip_tags(_safe_str(ch_txt, 120))
                    if s and not s.startswith("<") and s not in parts:
                        parts.append(s)
                for gch in list(getattr(ch, "children", []) or [])[:6]:
                    if getattr(gch, "destroyed", False) or not getattr(gch, "display", True):
                        continue
                    g_txt = getattr(gch, "text", None)
                    if g_txt and not callable(g_txt):
                        s = _strip_tags(_safe_str(g_txt, 120))
                        if s and not s.startswith("<") and s not in parts:
                            parts.append(s)
        except Exception:
            pass
        if parts:
            return " | ".join(parts[:3])[:80]
    nm = _safe_str(getattr(node, "name", ""), 80)
    if nm and nm not in ("main", "cont", "underlay", "wrapper", "content"):
        return nm
    return ""


_NON_INTERACTIVE_CONTAINER_CLASSES = (
    "Container",
    "ContainerAutoSize",
    "Fill",
    "Frame",
    "Sprite",
    "Transform",
    "Line",
    "GridContainer",
    "FlowContainer",
    "ScrollContainer",
    "StretchSpriteHorizontal",
    "StretchSpriteVertical",
    "WindowUnderlay",
    "ButtonUnderlay",
    "CheckboxUnderlay",
    "WindowControlButton",
    "DefaultWindowHeader",
    "WindowStackHeader",
    "TabGroup",
    "ToggleButtonGroup",
    "StationButtonGrid",
)


def _has_real_click_handler(node):
    if node is None:
        return False
    cls_name = node.__class__.__name__
    if cls_name in _NON_INTERACTIVE_CONTAINER_CLASSES:
        return False
    if cls_name.endswith("Wnd") or cls_name.endswith("Window") or cls_name.endswith("WindowStack") or hasattr(node, "windowID"):
        return False
    try:
        if hasattr(node, "HasEventHandler"):
            if node.HasEventHandler("OnClick") or node.HasEventHandler("OnDblClick"):
                return True
    except Exception:
        pass
    if hasattr(node, "Click") or getattr(node, "func", None) is not None:
        return True
    if any(
        k in cls_name
        for k in (
            "Button",
            "Tab",
            "Entry",
            "MenuEntry",
            "Checkbox",
            "Combo",
            "Slot",
            "Card",
            "Btn",
            "Header",
            "Link",
            "RadioButton",
        )
    ):
        return True
    return False


_LAYER_SCAN_ORDER = (
    "menu",
    "modal",
    "systemmenu",
    "utilmenu",
    "abovemain",
    "main",
    "sidePanels",
    "sidepanels",
    "neocom",
    "alwaysvisible",
    "hangar",
    "station",
    "inflight",
    "viewstate",
    "charsel",
)


def _iter_visible_layers(uicore):
    seen = set()
    layers = getattr(uicore, "layer", None)
    if layers is not None:
        for lname in _LAYER_SCAN_ORDER:
            lobj = getattr(layers, lname, None)
            if lobj is not None and id(lobj) not in seen and _is_visible(lobj):
                seen.add(id(lobj))
                yield lname, lobj
    desktop = getattr(uicore, "desktop", None)
    if desktop is not None:
        for ch in list(getattr(desktop, "children", []) or []):
            if ch is None or id(ch) in seen or not _is_visible(ch):
                continue
            cname = _safe_str(getattr(ch, "name", ""), 48)
            if cname in ("l_hint", "l_dragging", "l_loading", "l_loadingFill", "l_mloading", "l_videoOverlay"):
                continue
            seen.add(id(ch))
            yield (cname[2:] if cname.startswith("l_") else cname), ch


def _inspect_uicore_tree():
    uicore = _get_uicore()
    if uicore is None:
        return {"open_windows": [], "modals": [], "context_menu": [], "clickable_widgets": [], "recent_modal_messages": []}

    open_windows = []
    modals = []
    context_menu = []
    clickables = []

    try:
        reg = getattr(uicore, "registry", None)
        wins = list(reg.GetWindows() or []) if (reg is not None and hasattr(reg, "GetWindows")) else []
        layers_root = getattr(uicore, "layer", None)
        if layers_root is not None:
            for lname in ("modal", "abovemain", "main"):
                lobj = getattr(layers_root, lname, None)
                for ch in list(getattr(lobj, "children", []) or []):
                    cls_n = ch.__class__.__name__
                    if ch not in wins and (
                        hasattr(ch, "windowID")
                        or "Window" in cls_n
                        or "Wnd" in cls_n
                        or "Dialog" in cls_n
                        or "MessageBox" in cls_n
                        or "Panel" in cls_n
                    ):
                        wins.append(ch)
        seen_ids = set()
        for w in (wins or []):
            if getattr(w, "destroyed", False) or not _is_visible(w):
                continue
            x, y, wd, ht = _widget_abs_rect(w)
            wname = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", None), 64)
            if isinstance(wname, tuple):
                wname = _safe_str(wname[0], 64)
            wcap = _strip_tags(_safe_str(getattr(w, "caption", None) or _extract_widget_label(w), 100))
            cls_name = w.__class__.__name__
            dedup_key = "%s:%s:%d:%d" % (cls_name, wname, x, y)
            if dedup_key in seen_ids:
                continue
            seen_ids.add(dedup_key)
            is_modal = bool(
                getattr(w, "isModal", False)
                or "MessageBox" in cls_name
                or "Popup" in cls_name
                or "HybridWindow" in cls_name
            )
            try:
                is_min = bool(getattr(w, "minimized", False) or (hasattr(w, "IsMinimized") and w.IsMinimized()))
            except Exception:
                is_min = False
            info = {
                "window_id": str(wname),
                "name": _safe_str(getattr(w, "name", wname), 64),
                "cls": cls_name,
                "caption": wcap,
                "x": x,
                "y": y,
                "w": wd,
                "h": ht,
                "minimized": is_min,
            }
            msg_lbl = getattr(w, "_message_label", None)
            if msg_lbl is not None and getattr(msg_lbl, "text", None):
                info["message_text"] = _strip_tags(_safe_str(getattr(msg_lbl, "text", ""), 200))
            if getattr(w, "msgKey", None):
                info["msg_key"] = _safe_str(getattr(w, "msgKey", ""), 80)
            open_windows.append(info)
            if is_modal:
                modals.append(info)
    except Exception:
        pass

    visited_nodes = set()

    def _walk(node, path_prefix, depth, bucket):
        if depth > 16 or len(bucket) >= 160 or id(node) in visited_nodes or not _is_pickable(node):
            return
        visited_nodes.add(id(node))
        name = _safe_str(getattr(node, "name", ""), 48) or node.__class__.__name__
        cur_path = "%s/%s" % (path_prefix, name) if path_prefix else name
        cls_name = node.__class__.__name__
        is_interactive = _has_real_click_handler(node)
        if is_interactive:
            x, y, w, h = _widget_abs_rect(node)
            if w >= 8 and h >= 8 and x >= 0 and y >= 0:
                lbl = _extract_widget_label(node)
                item = {
                    "path": cur_path[-120:],
                    "name": name,
                    "cls": cls_name,
                    "label": lbl,
                    "hint": _strip_tags(_safe_str(getattr(node, "hint", ""), 80)),
                    "x": int(x + w // 2),
                    "y": int(y + h // 2),
                    "w": w,
                    "h": h,
                }
                if "menu" in path_prefix.lower() or "MenuEntry" in cls_name:
                    context_menu.append(item)
                else:
                    bucket.append(item)
        children = getattr(node, "children", None)
        if children:
            try:
                for ch in list(children):
                    _walk(ch, cur_path, depth + 1, bucket)
            except Exception:
                pass

    for lname, lobj in _iter_visible_layers(uicore):
        _walk(lobj, lname, 0, clickables)

    return {
        "open_windows": open_windows[:40],
        "modals": modals[:10],
        "context_menu": context_menu[:30],
        "clickable_widgets": clickables[:140],
        "recent_modal_messages": list(_RECENT_MODALS[-10:]),
    }


def _find_widget(target, layers_root, hint_x=None, hint_y=None):
    uicore = _get_uicore()
    if uicore is None or not target:
        return None
    t_low = str(target).lower()
    best = [None, -1]
    fallback = [None, -1]

    def _search(node, cur_path, depth):
        if depth > 17 or not _is_pickable(node):
            return
        name = _safe_str(getattr(node, "name", ""), 64)
        path = "%s/%s" % (cur_path, name) if cur_path else name
        p_low = path.lower()
        n_low = name.lower()
        lbl = _extract_widget_label(node)
        l_low = lbl.lower() if lbl else ""
        base_score = 0
        if p_low == t_low or ( "/" in t_low and p_low.endswith(t_low)):
            base_score = 100
        elif n_low == t_low or p_low.endswith("/" + t_low):
            base_score = 90
        elif l_low and l_low == t_low:
            base_score = 80
        elif len(t_low) >= 4 and l_low and t_low in l_low:
            base_score = 60

        if base_score > 0:
            score = base_score + depth
            if hint_x is not None and hint_y is not None and (hint_x > 0 or hint_y > 0):
                try:
                    wx, wy, ww, wh = _widget_abs_rect(node)
                    if ww >= 4 and wh >= 4 and wx <= hint_x <= (wx + ww) and wy <= hint_y <= (wy + wh):
                        score += 50
                except Exception:
                    pass
            if _has_real_click_handler(node):
                if score >= best[1]:
                    best[0] = node
                    best[1] = score
            elif hasattr(node, "OnClick") or hasattr(node, "Click") or hasattr(node, "OnMouseDown"):
                if score >= fallback[1]:
                    fallback[0] = node
                    fallback[1] = score
        children = getattr(node, "children", None)
        if children:
            try:
                for ch in list(children):
                    _search(ch, path, depth + 1)
            except Exception:
                pass

    for lname, lobj in _iter_visible_layers(uicore):
        _search(lobj, lname, 0)
        if best[0] is not None and best[1] >= 140:
            return best[0]
    if best[0] is not None:
        return best[0]
    return fallback[0]


def _find_widget_at_xy(px, py, layers_root):
    uicore = _get_uicore()
    if uicore is None:
        return None
    best = [None, 10**12]

    def _hit(node, depth):
        if depth > 17 or not _is_pickable(node):
            return
        x, y, w, h = _widget_abs_rect(node)
        if w >= 6 and h >= 6 and x <= px <= (x + w) and y <= py <= (y + h):
            if _has_real_click_handler(node) or (hasattr(node, "HasEventHandler") and node.HasEventHandler("GetMenu")):
                area = w * h
                if area <= best[1]:
                    best[0] = node
                    best[1] = area
        children = getattr(node, "children", None)
        if children:
            try:
                for ch in list(children):
                    _hit(ch, depth + 1)
            except Exception:
                pass

    for _, lobj in _iter_visible_layers(uicore):
        _hit(lobj, 0)
        if best[0] is not None:
            return best[0]
    return None


def _resolve_editable_widget(node):
    cur = node
    for _ in range(5):
        if cur is None:
            break
        if hasattr(cur, "SetValue") or hasattr(cur, "SetText"):
            return cur
        cur = getattr(cur, "parent", None)
    if node is not None:
        for ch in list(getattr(node, "children", []) or []):
            if hasattr(ch, "SetValue") or hasattr(ch, "SetText"):
                return ch
    return None


def _find_any_visible_editable(layers):
    if layers is None:
        return None
    found = []

    def _walk(n, depth):
        if n is None or depth > 18 or getattr(n, "destroyed", False) or not _is_visible(n):
            return
        if hasattr(n, "SetValue"):
            cls_name = n.__class__.__name__
            prio = 2 if ("Edit" in cls_name or "Input" in cls_name) else 1
            found.append((prio, n))
        for ch in list(getattr(n, "children", []) or []):
            _walk(ch, depth + 1)

    for lname in ("modal", "abovemain", "main"):
        _walk(getattr(layers, lname, None), 0)
    if not found:
        return None
    found.sort(key=lambda x: x[0], reverse=True)
    return found[0][1]


def _invoke_widget_click(node, right_click=False, double_click=False):
    edit_w = _resolve_editable_widget(node)
    if edit_w is not None:
        try:
            uic = _get_uicore()
            reg = getattr(uic, "registry", None) if uic is not None else None
            if reg is not None and hasattr(reg, "SetFocus"):
                reg.SetFocus(edit_w)
        except Exception:
            pass
    if right_click or (not _has_real_click_handler(node) and hasattr(node, "HasEventHandler") and node.HasEventHandler("GetMenu")):
        cur = node
        for _ in range(5):
            if cur is None:
                break
            has_menu = hasattr(cur, "GetMenu") and (
                not hasattr(cur, "HasEventHandler") or cur.HasEventHandler("GetMenu") or right_click
            )
            if has_menu:
                try:
                    from carbonui.control.contextMenu.contextMenu import ShowMenu
                    ShowMenu(cur)
                    return "ShowMenu(%s)" % (_safe_str(getattr(cur, "name", ""), 48),)
                except Exception:
                    try:
                        menu = cur.GetMenu()
                        return "GetMenu(%d)" % (len(menu or []),)
                    except Exception:
                        pass
            cur = getattr(cur, "parent", None)
    if double_click and hasattr(node, "OnDblClick"):
        node.OnDblClick()
        return "OnDblClick"
    if hasattr(node, "Click"):
        node.Click()
        return "Click"
    if hasattr(node, "OnClick"):
        node.OnClick()
        return "OnClick"
    if hasattr(node, "OnMouseDown"):
        node.OnMouseDown()
        return "OnMouseDown"
    return "noop"


def _sample_ego_warp_state(sm):
    sess = getattr(__builtin__, "session", None)
    sid = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
    michelle = getattr(sm, "services", {}).get("michelle")
    fx_svc = getattr(sm, "services", {}).get("fxSequencer")
    space_svc = getattr(sm, "services", {}).get("space")
    bp = michelle.GetBallpark() if (michelle is not None and hasattr(michelle, "GetBallpark")) else None
    ball = bp.GetBall(sid) if (bp is not None and sid and hasattr(bp, "GetBall")) else None
    mode = int(getattr(ball, "mode", -1)) if ball is not None else -1
    vx = float(getattr(ball, "vx", 0.0) or 0.0) if ball is not None else 0.0
    vy = float(getattr(ball, "vy", 0.0) or 0.0) if ball is not None else 0.0
    vz = float(getattr(ball, "vz", 0.0) or 0.0) if ball is not None else 0.0
    speed = math.sqrt(vx * vx + vy * vy + vz * vz)
    fx_names = []
    if fx_svc is not None and sid and hasattr(fx_svc, "GetAllBallActivationNames"):
        try:
            fx_names = list(fx_svc.GetAllBallActivationNames(sid) or [])
        except Exception:
            fx_names = []
    warp_cache = getattr(space_svc, "warpDestinationCache", None) if space_svc is not None else None
    has_warp_cache = bool(
        any(x is not None for x in warp_cache)
        if isinstance(warp_cache, (list, tuple))
        else warp_cache is not None
    )
    return {
        "mode": mode,
        "speed_mps": round(speed, 2),
        "fx_activations": [str(x) for x in fx_names],
        "has_warp_dest_cache": has_warp_cache,
        "signal_in_warp": bool(_WARP_TELEMETRY["in_warp"]),
    }


def _get_state_snapshot(sm):
    global _LAST_OBSERVED_SEQ
    _install_flight_recorder_hooks(sm)
    _install_warp_telemetry(sm)
    sess = getattr(__builtin__, "session", None)
    sess_dict = {}
    if sess is not None:
        for k in ("userid", "charid", "corpid", "allianceid", "solarsystemid", "solarsystemid2", "constellationid", "regionid", "stationid", "structureid", "shipid", "mutating"):
            v = getattr(sess, k, None)
            if v is not None:
                try:
                    sess_dict[k] = int(v)
                except Exception:
                    sess_dict[k] = str(v)

    vs = getattr(sm, "services", {}).get("viewState")
    cur_view = None
    if vs is not None and hasattr(vs, "GetCurrentViewInfo"):
        try:
            vi = vs.GetCurrentViewInfo()
            cur_view = getattr(vi, "name", None)
        except Exception:
            cur_view = None

    loading_svc = getattr(sm, "services", {}).get("loading")
    is_loading = bool(loading_svc.IsLoading()) if (loading_svc is not None and hasattr(loading_svc, "IsLoading")) else False

    quasar_state = "UNKNOWN"
    try:
        pgw = getattr(sm, "services", {}).get("publicGatewaySvc")
        gw = getattr(pgw, "publicGateway", None) if pgw is not None else None
        if gw is not None and hasattr(gw, "get_request_broker_details"):
            bd = gw.get_request_broker_details()
            quasar_state = str(bd.get("channel_state", "") if isinstance(bd, dict) else getattr(bd, "channel_state", ""))
    except Exception:
        pass

    space_info = {"in_space": False}
    if sess_dict.get("solarsystemid") and not sess_dict.get("stationid") and not sess_dict.get("structureid"):
        space_info["in_space"] = True
        space_info["ego"] = _sample_ego_warp_state(sm)
        nearby = []
        try:
            michelle = getattr(sm, "services", {}).get("michelle")
            bp = michelle.GetBallpark() if michelle is not None else None
            sid = sess_dict.get("shipid", 0)
            if bp is not None:
                slims = getattr(bp, "slimItems", {}) or {}
                for bid, slim in list(slims.items()):
                    if int(bid) == int(sid):
                        continue
                    b = bp.GetBall(bid)
                    dist = float(getattr(b, "surfaceDist", getattr(b, "maxVelocity", 0.0)) or 0.0) if b is not None else 0.0
                    nearby.append({
                        "item_id": int(bid),
                        "type_id": int(getattr(slim, "typeID", 0) or 0),
                        "group_id": int(getattr(slim, "groupID", 0) or 0),
                        "category_id": int(getattr(slim, "categoryID", 0) or 0),
                        "name": _safe_str(getattr(slim, "name", None) or getattr(slim, "itemName", ""), 48),
                        "distance_m": round(dist, 1),
                        "mode": int(getattr(b, "mode", -1)) if b is not None else -1,
                    })
                nearby.sort(key=lambda x: x["distance_m"])
        except Exception:
            pass
        space_info["nearby_balls"] = nearby[:30]

    new_flights = [e for e in _FLIGHT_RECORDER if e["seq"] > _LAST_OBSERVED_SEQ]
    _LAST_OBSERVED_SEQ = _FLIGHT_SEQ

    # Snapshot skills & queue state for UI-state expectation verification
    skills_state = {
        "queue_len": 0,
        "queue_items": [],
        "total_trained_skills": 0,
        "total_sp": 0.0,
    }
    has_char = bool(sess_dict.get("charid"))
    try:
        sq_svc = (
            getattr(sm, "services", {}).get("skillqueue")
            or getattr(sm, "services", {}).get("skillQueue")
            or (sm.GetService("skillqueue") if has_char else None)
        )
        if sq_svc is not None and hasattr(sq_svc, "GetQueue"):
            q = list(sq_svc.GetQueue() or [])
            skills_state["queue_len"] = len(q)
            for qitem in q[:10]:
                tid = getattr(qitem, "trainingTypeID", None)
                lvl = getattr(qitem, "trainingToLevel", None)
                if tid is not None:
                    skills_state["queue_items"].append({"type_id": int(tid), "level": int(lvl or 0)})
    except Exception:
        pass
    try:
        sk_svc = sm.GetService("skills") if has_char else getattr(sm, "services", {}).get("skills")
        if sk_svc is not None and hasattr(sk_svc, "GetSkills"):
            smap = sk_svc.GetSkills() or {}
            skills_state["total_trained_skills"] = len(smap)
            tot_sp = 0.0
            for srec in smap.values():
                sp = getattr(srec, "trainedSkillPoints", None) or getattr(srec, "skillPoints", 0.0) or 0.0
                tot_sp += float(sp)
            skills_state["total_sp"] = round(tot_sp, 1)
    except Exception:
        pass

    wallet_state = {"isk_balance": None}
    try:
        w_svc = sm.GetService("wallet") if has_char else getattr(sm, "services", {}).get("wallet")
        if w_svc is not None and hasattr(w_svc, "GetWealth"):
            wallet_state["isk_balance"] = round(float(w_svc.GetWealth() or 0.0), 2)
    except Exception:
        pass

    locked_targets = []
    try:
        t_svc = getattr(sm, "services", {}).get("target")
        if t_svc is not None and hasattr(t_svc, "GetTargets"):
            locked_targets = [int(x) for x in list(t_svc.GetTargets() or [])[:20]]
    except Exception:
        pass

    ui_tree = _inspect_uicore_tree()
    desktop = getattr(_get_uicore(), "desktop", None)
    desktop_info = {
        "width": int(getattr(desktop, "width", 1280) or 1280),
        "height": int(getattr(desktop, "height", 800) or 800),
    }

    return {
        "ts": round(time.time(), 3),
        "session": sess_dict,
        "active_view": cur_view,
        "current_view": cur_view,
        "is_loading": is_loading,
        "quasar_channel_state": quasar_state,
        "desktop": desktop_info,
        "skills_state": skills_state,
        "wallet_state": wallet_state,
        "locked_targets": locked_targets,
        "space": space_info,
        "warp_telemetry": dict(_WARP_TELEMETRY),
        "ui": ui_tree,
        "open_windows": ui_tree.get("open_windows", []),
        "modals": ui_tree.get("modals", []),
        "context_menu": ui_tree.get("context_menu", []),
        "flight_recorder_delta": new_flights[-40:],
    }


def _compute_state_diff(pre, post, scatters):
    pre_wins = set(w.get("window_id") or w.get("name") for w in (pre.get("open_windows") or []))
    post_wins = set(w.get("window_id") or w.get("name") for w in (post.get("open_windows") or []))
    wins_opened = sorted(list(post_wins - pre_wins))
    wins_closed = sorted(list(pre_wins - post_wins))

    pre_modals = set(m.get("title") or m.get("name") for m in (pre.get("modals") or []))
    post_modals = set(m.get("title") or m.get("name") for m in (post.get("modals") or []))
    modals_appeared = sorted(list(post_modals - pre_modals))

    view_changed = None
    if pre.get("active_view") != post.get("active_view"):
        view_changed = {"from": pre.get("active_view"), "to": post.get("active_view")}

    pre_sk = pre.get("skills_state") or {}
    post_sk = post.get("skills_state") or {}
    skill_delta = {
        "queue_len_before": pre_sk.get("queue_len", 0),
        "queue_len_after": post_sk.get("queue_len", 0),
        "total_sp_before": pre_sk.get("total_sp", 0.0),
        "total_sp_after": post_sk.get("total_sp", 0.0),
        "trained_skills_before": pre_sk.get("total_trained_skills", 0),
        "trained_skills_after": post_sk.get("total_trained_skills", 0),
        "changed": (
            pre_sk.get("queue_len") != post_sk.get("queue_len")
            or pre_sk.get("total_sp") != post_sk.get("total_sp")
            or pre_sk.get("total_trained_skills") != post_sk.get("total_trained_skills")
            or pre_sk.get("queue_items") != post_sk.get("queue_items")
        ),
    }

    pre_isk = (pre.get("wallet_state") or {}).get("isk_balance")
    post_isk = (post.get("wallet_state") or {}).get("isk_balance")
    isk_delta = None
    if pre_isk is not None and post_isk is not None:
        isk_delta = round(float(post_isk) - float(pre_isk), 2)

    pre_sp = pre.get("space") or {}
    post_sp = post.get("space") or {}
    pre_ego = pre_sp.get("ego") or {}
    post_ego = post_sp.get("ego") or {}
    ship_delta = {
        "in_space_before": bool(pre_sp.get("in_space")),
        "in_space_after": bool(post_sp.get("in_space")),
        "mode_before": pre_ego.get("mode"),
        "mode_after": post_ego.get("mode"),
        "speed_before": pre_ego.get("speed_mps"),
        "speed_after": post_ego.get("speed_mps"),
        "locked_targets_before": len(pre.get("locked_targets") or []),
        "locked_targets_after": len(post.get("locked_targets") or []),
    }

    pre_click_count = len((pre.get("ui") or {}).get("clickable_widgets") or [])
    post_click_count = len((post.get("ui") or {}).get("clickable_widgets") or [])
    pre_menu_count = len(pre.get("context_menu") or [])
    post_menu_count = len(post.get("context_menu") or [])

    scatter_names = [s.get("event") for s in scatters]
    ui_changed = bool(
        wins_opened
        or wins_closed
        or modals_appeared
        or view_changed
        or skill_delta["changed"]
        or (isk_delta is not None and abs(isk_delta) > 0.001)
        or ship_delta["in_space_before"] != ship_delta["in_space_after"]
        or ship_delta["mode_before"] != ship_delta["mode_after"]
        or ship_delta["locked_targets_before"] != ship_delta["locked_targets_after"]
        or pre_click_count != post_click_count
        or pre_menu_count != post_menu_count
        or len(scatter_names) > 0
    )

    return {
        "ui_changed": ui_changed,
        "windows_opened": wins_opened,
        "windows_closed": wins_closed,
        "modals_appeared": modals_appeared,
        "view_changed": view_changed,
        "skill_queue_delta": skill_delta,
        "wallet_isk_delta": isk_delta,
        "ship_delta": ship_delta,
        "clickable_widget_count_delta": post_click_count - pre_click_count,
        "context_menu_count_delta": post_menu_count - pre_menu_count,
        "scatters_observed": scatter_names,
    }


def _open_client_window(sm, target):
    uicore = _get_uicore()
    cmd_svc = getattr(uicore, "cmd", None) if uicore is not None else None
    if cmd_svc is None:
        try:
            cmd_svc = sm.GetService("cmd")
        except Exception:
            cmd_svc = None

    raw_target = str(target or "").strip()
    t = raw_target.lower().replace("_", "").replace("-", "").replace(" ", "")

    if t in ("skills", "skillqueue", "skillswindow", "skillplan", "skillplans", "skillplanwindow"):
        try:
            sp_svc = sm.GetService("skillPlanSvc")
            if hasattr(sp_svc, "GetAllPersonal"):
                sp_svc.GetAllPersonal()
            from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
            SkillPlanDockablePanel.Open()
            H._sleep_ms(500)
            return {"ok": True, "opened_via": "SkillPlanDockablePanel.Open"}
        except Exception:
            pass

    cmd_map = {
        "inventory": "OpenInventory",
        "cargohold": "OpenCargoHoldOfActiveShip",
        "shiphangar": "OpenShipHangar",
        "itemhangar": "OpenHangarFloor",
        "market": "OpenMarket",
        "marketorders": "OpenMarketOrders",
        "multibuy": "OpenMultibuy",
        "charsheet": "OpenCharactersheet",
        "charactersheet": "OpenCharactersheet",
        "character": "OpenCharactersheet",
        "skills": "OpenSkillsWindow",
        "skillqueue": "OpenSkillsWindow",
        "skillswindow": "OpenSkillsWindow",
        "skilltree": "OpenSkillTreeWindow",
        "skillplan": "OpenSkillsWindow",
        "skillplans": "OpenSkillsWindow",
        "nes": "ToggleAurumStore",
        "newedenstore": "ToggleAurumStore",
        "plexstore": "ToggleAurumStore",
        "aurumstore": "ToggleAurumStore",
        "store": "ToggleAurumStore",
        "plexvault": "OpenPlexVault",
        "fitting": "OpenFitting",
        "fittingmgmt": "OpenFittingMgmt",
        "contracts": "OpenContracts",
        "industry": "OpenIndustry",
        "planets": "OpenPlanets",
        "pi": "OpenPlanets",
        "corp": "OpenCorporationPanel",
        "corporation": "OpenCorporationPanel",
        "fleet": "OpenFleet",
        "agency": "OpenAgencyNew",
        "jobboard": "OpenJobBoardWindow",
        "opportunities": "OpenJobBoardWindow",
        "assets": "OpenAssets",
        "wallet": "OpenWallet",
        "mail": "OpenMail",
        "map": "CmdToggleMap",
        "solarsystemmap": "CmdToggleSolarSystemMap",
        "dscan": "OpenDirectionalScanner",
        "directionalscanner": "OpenDirectionalScanner",
        "probe": "OpenProbeScanner",
        "probescanner": "OpenProbeScanner",
        "scanner": "OpenProbeScanner",
        "notepad": "OpenNotepad",
        "calculator": "OpenCalculator",
        "calendar": "OpenCalendar",
        "journal": "OpenJournal",
        "agentfinder": "OpenAgentFinder",
        "lpstore": "OpenLPStore",
        "repair": "OpenRepairshop",
        "repairshop": "OpenRepairshop",
        "reprocessing": "OpenReprocessingPlant",
        "insurance": "OpenInsurance",
        "bountyoffice": "OpenBountyOffice",
        "securityoffice": "OpenSecurityOffice",
        "militia": "OpenMilitia",
        "factionalwarfare": "OpenFwWindow",
        "fw": "OpenFwWindow",
        "insurgency": "OpenInsurgencyDashboard",
        "skinr": "OpenShipSKINRWindow",
        "shipskinr": "OpenShipSKINRWindow",
        "structurebrowser": "OpenStructureBrowser",
        "shiptree": "CmdToggleShipTree",
        "isis": "CmdToggleShipTree",
        "aircareerprogram": "CmdToggleAirCareerProgram",
        "acp": "CmdToggleAirCareerProgram",
        "peopleandplaces": "OpenPeopleAndPlaces",
        "locations": "OpenLocationsWindow",
        "bookmarks": "OpenLocationsWindow",
        "compare": "OpenCompare",
        "channels": "OpenChannels",
        "chat": "OpenChannels",
        "help": "OpenHelp",
        "log": "OpenLog",
    }
    mname = cmd_map.get(t, raw_target)
    if cmd_svc is not None and hasattr(cmd_svc, mname):
        fn = getattr(cmd_svc, mname)
        fn()
        H._sleep_ms(500)
        return {"ok": True, "opened_via": "uicore.cmd.%s" % mname}

    if t in ("nes", "newedenstore", "plexstore", "aurumstore", "store"):
        try:
            vs = sm.GetService("viewState")
            vs.ToggleSecondaryView("aurumstore")
            H._sleep_ms(600)
            return {"ok": True, "opened_via": "viewState.ToggleSecondaryView(aurumstore)"}
        except Exception as exc:
            return {"ok": False, "error": "aurumstore open failed: %r" % (exc,)}

    return {"ok": False, "error": "unknown window target %r (normalized=%r)" % (target, t)}


def _run_gm_slash(sm, scmd):
    slash_svc = sm.GetService("slash")
    if scmd == "/" and hasattr(slash_svc, "FetchRemoteCommandList"):
        slash_svc.FetchRemoteCommandList()
        return "FetchRemoteCommandList OK"
    # Pre-seed remoteCommandList if absent so svc_slash._SlashCmd never blocks on the
    # "Hmmm: Unable to acquire server slash command list" modal dialog during automated playtesting.
    if not hasattr(slash_svc, "remoteCommandList") or getattr(slash_svc, "remoteCommandList", None) is None:
        try:
            setattr(
                slash_svc,
                "remoteCommandList",
                [
                    "advancetime",
                    "cleartimers",
                    "completeindustry",
                    "completejob",
                    "completeskills",
                    "createitem",
                    "dogma",
                    "ffstructure",
                    "finishskills",
                    "fit",
                    "giveisk",
                    "giveskill",
                    "heal",
                    "kill",
                    "moveme",
                    "repair",
                    "secstatus",
                    "spawn",
                    "standings",
                    "tr",
                    "unspawn",
                    "upwelltimer",
                ],
            )
        except Exception:
            pass
    return sm.RemoteSvc("slash").SlashCmd(scmd)


def _execute_action(sm, action):
    _install_flight_recorder_hooks(sm)
    _install_warp_telemetry(sm)
    pre_state = _get_state_snapshot(sm)
    pre_flight_seq = _FLIGHT_SEQ
    pre_scatter_seq = _SCATTER_SEQ
    pre_modal_cnt = len(_RECENT_MODALS)
    try:
        import evomu_site as _es
        pre_exc_seq = int(getattr(_es, "_EXC_SEQ", 0) or 0)
    except Exception:
        _es = None
        pre_exc_seq = 0

    raw_atype = str(action.get("type", "") or "").strip()
    raw_target = str(action.get("target") or "").strip()
    raw_cmd = str(action.get("command") or action.get("slash_cmd") or "").strip()
    wait_ms = int(action.get("wait_ms") or action.get("wait_after_ms") or 800)

    # Normalize schema action aliases to internal handlers
    atype = raw_atype
    target = raw_target
    if raw_atype == "open_neocom":
        atype = "open_window"
    elif raw_atype == "close_window":
        atype = "close_modal_or_window"
    elif raw_atype == "slash_command":
        atype = "run_slash"
        if not target and raw_cmd:
            target = raw_cmd
    elif raw_atype == "complete_skills":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/completeskills %s" % (raw_target or "all")).strip()
    elif raw_atype == "advance_upwell_timer":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/upwelltimer %s" % raw_target).strip()
    elif raw_atype == "complete_industry_jobs":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/completeindustry %s" % raw_target).strip()
    elif raw_atype == "advance_server_time":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/advancetime %s" % (raw_target or "3600")).strip()
    elif raw_atype == "clear_timers":
        atype = "run_slash"
        target = "/cleartimers"
    elif raw_atype == "undock":
        atype = "station_command"
        target = "undock"
    elif raw_atype == "open_plex_store":
        atype = "station_command"
        target = "open_plex_store"
    elif raw_atype == "train_skill":
        atype = "station_command"
        target = "inject_and_train_skill"
        if raw_target.isdigit():
            action = dict(action)
            action["type_id"] = int(raw_target)
    elif raw_atype in ("warp_to", "dock", "approach", "orbit", "keep_at_range", "lock_target"):
        atype = "space_command"
        target = raw_atype
        if raw_target.isdigit():
            action = dict(action)
            action["item_id"] = int(raw_target)

    uicore = _get_uicore()
    layers = getattr(uicore, "layer", None) if uicore is not None else None
    result = {"ok": True, "action_ok": True, "action_type": raw_atype, "target": raw_target}

    def _do_action():
        try:
            if atype == "open_window":
                result.update(_open_client_window(sm, target))

            elif atype == "open_structure_cosmetics":
                try:
                    sess = getattr(__builtin__, "session", None)
                    cur_corpid = int(getattr(sess, "corpid", 0) or 0) if sess is not None else 0
                    is_docked = bool(
                        sess is not None
                        and (getattr(sess, "stationid", None) is not None or getattr(sess, "structureid", None) is not None)
                    )
                    if cur_corpid < 2000000 and is_docked:
                        try:
                            _run_gm_slash(sm, "/cleartimers")
                            _run_gm_slash(sm, "/giveskill 3363 1")
                            stamp = int(time.time()) % 100000
                            sm.RemoteSvc("corpRegistry").AddCorporation(
                                "EvoMu Designers %d" % stamp,
                                "EV%03d" % (stamp % 1000),
                            )
                            H._sleep_ms(400)
                            H._sync_quasar_gateway(sm, int(getattr(sess, "charid", 0) or 0))
                        except Exception:
                            pass
                    cur_corpid = int(getattr(sess, "corpid", 0) or 0) if sess is not None else 0
                    prev_corprole = int(getattr(sess, "corprole", 0) or 0) if sess is not None else 0
                    spoofed_npc_role = False
                    if (cur_corpid < 2000000 or prev_corprole == 0) and sess is not None and hasattr(sess, "__dict__"):
                        try:
                            lp_svc = sm.GetService("loyaltyPointsWalletSvc")
                            if lp_svc is not None:
                                setattr(lp_svc, "_corpLPBalanceByIssuerCorpID", {})
                        except Exception:
                            pass
                        sess.__dict__["corprole"] = prev_corprole | 0x7FFFFFFFFFFFFFFF
                        spoofed_npc_role = True
                    try:
                        struct_id = int(raw_target) if raw_target.isdigit() else 1000000000001
                        sys_id = int(getattr(sess, "solarsystemid2", 0) or getattr(sess, "solarsystemid", 0) or 30000142)
                        cos_svc = sm.GetService("cosmeticsSvc") if sm is not None else None
                        ctrl = getattr(cos_svc, "_structure_cosmetic_states_controller", None)
                        if ctrl is None:
                            from cosmetics.client.structureCosmeticStatesController import (
                                StructureCosmeticStatesController,
                            )
                            pg = sm.GetService("publicGatewaySvc")
                            ctrl = StructureCosmeticStatesController(pg)
                        if hasattr(ctrl, "get_cosmetic_state"):
                            state = ctrl.get_cosmetic_state(struct_id, sys_id, force_refresh=True)
                            result["cosmetic_state"] = _safe_str(repr(state), 180)
                        lic_svc = sm.GetService("cosmeticsLicenseSvc") if sm is not None else None
                        if lic_svc is not None and hasattr(lic_svc, "get_structure_licenses_for_corporation"):
                            lics = lic_svc.get_structure_licenses_for_corporation()
                            result["corp_structure_licenses"] = _safe_str(repr(lics), 180)
                        try:
                            from eve.client.script.ui.cosmetics.structure.paintToolWnd import PaintToolWnd
                            PaintToolWnd.Open()
                            result["opened_window"] = "PaintToolWnd"
                        except Exception as wnd_exc:
                            result["window_note"] = _safe_str(repr(wnd_exc), 140)
                    finally:
                        if spoofed_npc_role and sess is not None and hasattr(sess, "__dict__"):
                            sess.__dict__["corprole"] = prev_corprole
                except Exception as exc:
                    result["ok"] = False
                    result["error"] = "open_structure_cosmetics failed: %r" % (exc,)

            elif atype == "activate_module":
                slot_name = raw_target or "medSlot0"
                node = _find_widget(slot_name, layers)
                if node is not None and hasattr(node, "OnClick"):
                    node.OnClick()
                    result["activated_slot"] = slot_name
                else:
                    result["ok"] = False
                    result["error"] = "module slot %r not found" % (slot_name,)

            elif atype == "close_modal_or_window":
                closed = []
                reg = getattr(uicore, "registry", None) if uicore is not None else None
                wins = list(reg.GetWindows() or []) if (reg is not None and hasattr(reg, "GetWindows")) else []
                if layers is not None:
                    for lname in ("modal", "abovemain", "main"):
                        lobj = getattr(layers, lname, None)
                        for ch in list(getattr(lobj, "children", []) or []):
                            if ch not in wins and not getattr(ch, "destroyed", False) and _is_visible(ch):
                                wins.append(ch)
                t_low = target.lower() if target else ""
                for w in wins:
                    if getattr(w, "destroyed", False) or not _is_visible(w):
                        continue
                    wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
                    if isinstance(wid, tuple):
                        wid = _safe_str(wid[0], 64)
                    wname = _safe_str(getattr(w, "name", ""), 64)
                    wcap = _strip_tags(_safe_str(getattr(w, "caption", None) or _extract_widget_label(w), 100))
                    wcls = w.__class__.__name__
                    is_mod = bool(getattr(w, "isModal", False) or "MessageBox" in wcls or "Popup" in wcls)
                    match = (
                        not t_low
                        or t_low in wid.lower()
                        or t_low in wname.lower()
                        or t_low in wcap.lower()
                        or t_low in wcls.lower()
                        or (t_low in ("modal", "popup", "dialog", "messagebox") and is_mod)
                    )
                    if match:
                        try:
                            if is_mod and hasattr(w, "SetModalResult"):
                                try:
                                    w.SetModalResult(1)
                                except Exception:
                                    pass
                            try:
                                if hasattr(w, "CloseByUser"):
                                    w.CloseByUser()
                                elif hasattr(w, "Close"):
                                    w.Close()
                            except Exception as close_exc:
                                result["close_warning"] = _safe_str(repr(close_exc), 160)
                            if not getattr(w, "destroyed", False):
                                try:
                                    from carbonui.control.window import Window as _BaseWindow
                                    _BaseWindow.Close(w)
                                except Exception:
                                    try:
                                        w.display = False
                                    except Exception:
                                        pass
                            closed.append(wid or wcap or wcls)
                        except Exception:
                            pass
                result["closed"] = closed

            elif atype == "click_widget":
                hx = int(action["x"]) if action.get("x") is not None else None
                hy = int(action["y"]) if action.get("y") is not None else None
                node = _find_widget(target, layers, hint_x=hx, hint_y=hy)
                if node is None and hx is not None and hy is not None and (hx > 0 or hy > 0):
                    node = _find_widget_at_xy(hx, hy, layers)
                if node is None:
                    result["ok"] = False
                    result["error"] = "widget %r not found in visible uicore layers" % (target,)
                else:
                    result["clicked_widget"] = _invoke_widget_click(
                        node,
                        right_click=bool(action.get("right_click")),
                        double_click=bool(action.get("double_click")),
                    )

            elif atype == "click_xy":
                x_val = int(action.get("x", 0) or 0)
                y_val = int(action.get("y", 0) or 0)
                node = _find_widget_at_xy(x_val, y_val, layers)
                if node is None:
                    result["ok"] = False
                    result["error"] = "no clickable widget found at (%d, %d)" % (x_val, y_val)
                else:
                    result["clicked_widget"] = _invoke_widget_click(
                        node,
                        right_click=bool(action.get("right_click")),
                        double_click=bool(action.get("double_click")),
                    )
                    result["clicked_xy"] = [x_val, y_val]

            elif atype == "drag_widget":
                node = _find_widget(target, layers)
                if node is None and action.get("x") is not None and action.get("y") is not None:
                    node = _find_widget_at_xy(int(action["x"]), int(action["y"]), layers)
                if node is None:
                    result["ok"] = False
                    result["error"] = "source widget %r not found for drag" % (target,)
                else:
                    result["dragged_widget"] = _safe_str(getattr(node, "name", ""), 64)

            elif atype == "wait":
                result["waited_ms"] = int(wait_ms or 500)

            elif atype == "press_key":
                key_name = str(action.get("key", "") or target)
                cmd_svc = getattr(uicore, "cmd", None) if uicore is not None else None
                if cmd_svc is not None and hasattr(cmd_svc, key_name):
                    getattr(cmd_svc, key_name)()
                    result["executed_cmd"] = key_name
                elif key_name.upper() == "ESCAPE" and cmd_svc is not None and hasattr(cmd_svc, "OnEsc"):
                    cmd_svc.OnEsc()
                    result["executed_cmd"] = "OnEsc"
                else:
                    result["ok"] = False
                    result["error"] = "key/cmd %r not found on uicore.cmd" % (key_name,)

            elif atype == "type_text":
                txt = str(action.get("text", "") or raw_cmd or raw_target)
                reg = getattr(uicore, "registry", None) if uicore is not None else None
                focus = reg.GetFocus() if (reg is not None and hasattr(reg, "GetFocus")) else None
                edit_node = _resolve_editable_widget(focus)
                if edit_node is None and raw_target:
                    cand = _find_widget(raw_target, layers)
                    edit_node = _resolve_editable_widget(cand)
                if edit_node is None and action.get("x") is not None and action.get("y") is not None:
                    cand = _find_widget_at_xy(int(action["x"]), int(action["y"]), layers)
                    edit_node = _resolve_editable_widget(cand)
                if edit_node is None:
                    edit_node = _find_any_visible_editable(layers)
                if edit_node is not None and hasattr(edit_node, "SetValue"):
                    try:
                        if reg is not None and hasattr(reg, "SetFocus"):
                            reg.SetFocus(edit_node)
                    except Exception:
                        pass
                    edit_node.SetValue(txt)
                    if hasattr(edit_node, "OnChange"):
                        try:
                            edit_node.OnChange(txt)
                        except Exception:
                            pass
                    result["typed_into"] = _safe_str(getattr(edit_node, "name", ""), 64)
                else:
                    result["ok"] = False
                    result["error"] = "no focused input widget with SetValue"

            elif atype == "run_slash":
                scmd = str(action.get("slash_cmd", "") or raw_cmd or target or "/whoami")
                try:
                    res = _run_gm_slash(sm, scmd)
                    result["slash_cmd"] = scmd
                    result["slash_result"] = _safe_str(repr(res), 240)
                except Exception as exc:
                    result["ok"] = False
                    result["error"] = "slash %r raised: %r" % (scmd, exc)

            elif atype == "station_command":
                sub = target.lower()
                if sub == "undock":
                    H._undock_ship(sm)
                    result["undocked"] = True
                elif sub == "open_plex_store":
                    result.update(_open_client_window(sm, "nes"))
                elif sub == "create_skill_plan":
                    sp_svc = sm.GetService("skillPlanSvc")
                    plans = sp_svc.GetAllPersonal()
                    result["personal_plans_count"] = len(plans) if plans is not None else 0
                elif sub == "inject_and_train_skill":
                    skill_tid = int(action.get("type_id", 3300) or 3300)
                    purchased = sm.RemoteSvc("skillMgr2").PurchaseSkills([skill_tid])
                    try:
                        sq_svc = sm.GetService("skillqueue")
                    except Exception:
                        sq_svc = getattr(sm, "services", {}).get("skillqueue") or getattr(sm, "services", {}).get("skillQueue")
                    cur_lvl = 0
                    try:
                        sk_svc = sm.GetService("skills")
                        sk_obj = sk_svc.GetSkill(skill_tid) if (sk_svc is not None and hasattr(sk_svc, "GetSkill")) else None
                        if sk_obj is not None:
                            cur_lvl = int(
                                getattr(sk_obj, "trainedSkillLevel", None)
                                or getattr(sk_obj, "skillLevel", None)
                                or getattr(sk_obj, "effectiveSkillLevel", 0)
                                or 0
                            )
                        if sq_svc is not None and hasattr(sq_svc, "GetQueue"):
                            for q_entry in list(sq_svc.GetQueue() or []):
                                q_tid = int(getattr(q_entry, "trainingTypeID", getattr(q_entry, "typeID", 0)) or 0)
                                q_lvl = int(getattr(q_entry, "trainingToLevel", getattr(q_entry, "level", 0)) or 0)
                                if q_tid == skill_tid and q_lvl > cur_lvl:
                                    cur_lvl = q_lvl
                    except Exception:
                        pass
                    target_lvl = min(5, max(1, cur_lvl + 1))
                    if cur_lvl >= 5:
                        result["already_max_level"] = 5
                    elif sq_svc is not None and hasattr(sq_svc, "BeginTransaction"):
                        try:
                            if not sq_svc.IsTransactionOpen():
                                sq_svc.BeginTransaction()
                            sq_svc.AddSkillToQueue(skill_tid, target_lvl)
                            sq_svc.CommitTransaction(activate=True)
                        except Exception:
                            sm.RemoteSvc("skillMgr2").SaveSkillQueue([(skill_tid, target_lvl)], True)
                    else:
                        sm.RemoteSvc("skillMgr2").SaveSkillQueue([(skill_tid, target_lvl)], True)
                    result["trained_skill_type_id"] = skill_tid
                    result["trained_to_level"] = target_lvl
                    result["purchased"] = repr(purchased)[:120]
                elif sub == "buy_free_plex_pack":
                    fc = sm.RemoteSvc("FastCheckoutService")
                    offers_res = fc.GetOffersForUser()
                    offers = offers_res.get("offers", []) if isinstance(offers_res, dict) else getattr(offers_res, "offers", [])
                    free_offer = next((o for o in (offers or []) if float(o.get("price", 1.0)) == 0.0), None)
                    if free_offer:
                        buy_res = fc.BuyOffer(int(free_offer["offerId"]), 1)
                        result["buy_offer_result"] = repr(buy_res)[:160]
                    else:
                        result["ok"] = False
                        result["error"] = "no $0.00 FastCheckout PLEX offer found"
                elif sub == "buy_nes_offer":
                    st = sm.RemoteSvc("storeManager")
                    offers = st.get_offers() or []
                    if offers:
                        oid = int(getattr(offers[0], "offerID", offers[0].get("offerID", 1) if isinstance(offers[0], dict) else 1))
                        res = st.buy_offer(oid, "PLX", 1)
                        result["nes_buy_result"] = repr(res)[:160]
                elif sub == "repair_all":
                    sess = getattr(__builtin__, "session", None)
                    ship_id = int(getattr(sess, "shipid", 0) or 0)
                    if ship_id:
                        rep = sm.RemoteSvc("repairSvc").GetRepairQuote([ship_id])
                        result["repair_quote"] = repr(rep)[:120]
                else:
                    result["ok"] = False
                    result["error"] = "unknown station_command %r" % (sub,)

            elif atype == "space_command":
                sub = target.lower()
                sess = getattr(__builtin__, "session", None)
                sys_id = int(getattr(sess, "solarsystemid", 0) or 0) if sess is not None else 0
                item_id = int(action.get("item_id", 0) or 0)
                dist = float(action.get("distance", 0.0) or 0.0)
                if not sys_id or getattr(sess, "stationid", None) or getattr(sess, "structureid", None):
                    result["ok"] = False
                    result["error"] = "cannot run space_command while docked"
                else:
                    beyonce = H._get_beyonce(sm, sys_id)
                    michelle = sm.GetService("michelle")
                    bp = michelle.GetBallpark() if michelle is not None else None
                    if not item_id and bp is not None:
                        sid = int(getattr(sess, "shipid", 0) or 0)
                        for bid, slim in (getattr(bp, "slimItems", {}) or {}).items():
                            if int(bid) == sid:
                                continue
                            b = bp.GetBall(bid)
                            bdist = float(getattr(b, "surfaceDist", 0.0) or 0.0) if b is not None else 0.0
                            if sub == "warp_to" and bdist > 250000.0:
                                item_id = int(bid)
                                break
                            elif sub == "dock" and int(getattr(slim, "categoryID", 0) or 0) in (3, 65):
                                item_id = int(bid)
                                break
                            elif sub != "warp_to" and bdist < 200000.0:
                                item_id = int(bid)
                                break
                    if sub == "warp_to":
                        _WARP_TELEMETRY["last_warp_saw_mode3"] = False
                        _WARP_TELEMETRY["last_warp_saw_fx_warping"] = False
                        _WARP_TELEMETRY["last_warp_saw_signal"] = False
                        _WARP_TELEMETRY["last_warp_peak_speed_mps"] = 0.0
                        _WARP_TELEMETRY["last_warp_samples"] = []
                        beyonce.CmdWarpToStuff("item", int(item_id), float(dist))
                        # Sample real-time warp tunnel & ball mode over 3.2 seconds WITHOUT instant 240s clock skip!
                        for _ in range(8):
                            H._sleep_sim_ms(400)
                            smp = _sample_ego_warp_state(sm)
                            _WARP_TELEMETRY["last_warp_samples"].append(smp)
                            if smp["mode"] == 3:
                                _WARP_TELEMETRY["last_warp_saw_mode3"] = True
                            if "effects.Warping" in smp["fx_activations"] or smp["has_warp_dest_cache"]:
                                _WARP_TELEMETRY["last_warp_saw_fx_warping"] = True
                            if smp["speed_mps"] > _WARP_TELEMETRY["last_warp_peak_speed_mps"]:
                                _WARP_TELEMETRY["last_warp_peak_speed_mps"] = smp["speed_mps"]
                        result["warp_target_id"] = int(item_id)
                        result["warp_realtime_telemetry"] = {
                            "saw_mode3": _WARP_TELEMETRY["last_warp_saw_mode3"],
                            "saw_fx_warping": _WARP_TELEMETRY["last_warp_saw_fx_warping"],
                            "saw_signal": _WARP_TELEMETRY["last_warp_saw_signal"],
                            "warp_signal_count": 1 if _WARP_TELEMETRY["last_warp_saw_signal"] else 0,
                            "peak_speed_mps": _WARP_TELEMETRY["last_warp_peak_speed_mps"],
                        }
                        result["warp_telemetry"] = result["warp_realtime_telemetry"]
                    elif sub == "align_to":
                        beyonce.CmdAlignTo(int(item_id))
                    elif sub == "approach":
                        beyonce.CmdFollowBall(int(item_id), 50.0)
                    elif sub == "orbit":
                        beyonce.CmdOrbit(int(item_id), float(dist or 5000.0))
                    elif sub == "keep_at_range":
                        beyonce.CmdFollowBall(int(item_id), float(dist or 10000.0))
                    elif sub == "stop":
                        beyonce.CmdStop()
                    elif sub == "dock":
                        H._dock_station(sm, int(item_id or 60003760))
                    elif sub == "lock_target":
                        tgt_svc = sm.GetService("target")
                        tgt_svc.TryLockTarget(int(item_id))
                    elif sub == "unlock_target":
                        tgt_svc = sm.GetService("target")
                        tgt_svc.UnlockTarget(int(item_id))
        except Exception as exc:
            result["ok"] = False
            result["error"] = "%s: %s" % (exc.__class__.__name__, exc)
            result["traceback"] = traceback.format_exc()[-1200:]

    done_flag = [False]
    try:
        import uthread

        def _worker():
            try:
                _do_action()
            finally:
                done_flag[0] = True

        uthread.new(_worker)
        max_wait_s = 25.0 if raw_atype in ("warp_to", "undock", "dock") else 12.0
        t_start = time.time()
        while not done_flag[0] and (time.time() - t_start) < max_wait_s:
            H._sleep_ms(50)
            # If a modal dialog popped up and blocked the worker tasklet inside MessageBox.Execute(),
            # break after 1.0s so serve_forever_on_stackless never deadlocks!
            if (time.time() - t_start) > 1.0 and len(_RECENT_MODALS) > pre_modal_cnt:
                result["blocked_by_modal"] = True
                break
        if not done_flag[0] and not result.get("blocked_by_modal"):
            result["ok"] = False
            result["error"] = "action tasklet timed out after %.1fs" % (time.time() - t_start,)
    except Exception:
        _do_action()

    if wait_ms > 0:
        H._sleep_ms(min(wait_ms, 5000))

    result["action_ok"] = bool(result.get("ok", True))
    result["action_error"] = result.get("error")

    post_state = _get_state_snapshot(sm)
    new_flights = [e for e in _FLIGHT_RECORDER if e["seq"] > pre_flight_seq]
    new_scatters = [s for s in _RECENT_SCATTERS if s["seq"] > pre_scatter_seq]
    new_modals = list(_RECENT_MODALS[pre_modal_cnt:])
    new_log_excs = []
    if _es is not None:
        try:
            new_log_excs = [e for e in getattr(_es, "_RECENT_EXCEPTIONS", []) if e.get("seq", 0) > pre_exc_seq]
        except Exception:
            new_log_excs = []

    macho_excs = [e for e in new_flights if e.get("status") == "ERR" and e.get("kind") == "macho"]
    quasar_errs = [e for e in new_flights if e.get("status") == "ERR" and e.get("kind") == "quasar"]
    result["state_diff"] = _compute_state_diff(pre_state, post_state, new_scatters)
    result["flight_recorder"] = {
        "macho_calls": new_flights[-30:],
        "macho_exceptions": macho_excs,
        "quasar_errors": quasar_errs,
        "log_exceptions": new_log_excs[-10:],
        "scatters_observed": [s.get("event") for s in new_scatters],
        "modals_opened": new_modals,
    }
    return result


class PlaytesterRpcServer(object):
    def __init__(self, cfg, sm):
        self.cfg = cfg
        self.sm = sm
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        bind_port = int(
            os.environ.get("EVOMU_PLAYTESTER_PORT", 0)
            or cfg.get("playtester_port", 0)
            or cfg.get("hook_port", 0)
            or 26099
        )
        self.sock.bind(("127.0.0.1", bind_port))
        self.sock.listen(8)
        self.sock.setblocking(0)
        self.port = self.sock.getsockname()[1]
        self.running = True

    def serve_forever_on_stackless(self):
        out_dir = self.cfg.get("run_dir") or self.cfg.get("out_dir") or "/tmp"
        ready_path = os.path.join(out_dir, "playtester_hook_ready.json")
        H._atomic_write_json(ready_path, {
            "pid": os.getpid(),
            "hook_port": self.port,
            "ts": time.time(),
        })
        while self.running:
            try:
                conn, _ = self.sock.accept()
            except socket.error:
                H._sleep_ms(40)
                continue
            except Exception:
                H._sleep_ms(80)
                continue
            try:
                conn.setblocking(1)
                conn.settimeout(45.0)
                rfile = conn.makefile("r")
                line = rfile.readline()
                if line:
                    req = json.loads(line)
                    resp = self._dispatch(req)
                    conn.sendall(json.dumps(resp) + "\n")
                rfile.close()
            except Exception as exc:
                try:
                    conn.sendall(json.dumps({"ok": False, "error": repr(exc), "traceback": traceback.format_exc()[-800:]}) + "\n")
                except Exception:
                    pass
            finally:
                try:
                    conn.close()
                except Exception:
                    pass
            H._sleep_ms(20)

    def _dispatch(self, req):
        cmd = req.get("method") or req.get("cmd") or ""
        params = req.get("params") or req
        if cmd == "ping":
            res = {"pong": True, "pid": os.getpid(), "ts": time.time()}
            return {"ok": True, "result": res, "pid": os.getpid(), "ts": time.time()}
        if cmd in ("bootstrap_session", "ensure_logged_in"):
            sess = getattr(__builtin__, "session", None)
            cid = getattr(sess, "charid", None) if sess is not None else None
            if not cid:
                timeout_s = float(params.get("timeout_ms", 90000)) / 1000.0 if "timeout_ms" in params else float(params.get("timeout_sec", 90.0))
                cid = H._wait_for_session(
                    self.sm,
                    self.cfg.get("user", "demo"),
                    self.cfg.get("password", "demo-secret"),
                    self.cfg.get("char_id", 90000001),
                    timeout_sec=timeout_s,
                )
            _ensure_windowed_mode(
                self.sm,
                int(self.cfg.get("window_width", 1280) or 1280),
                int(self.cfg.get("window_height", 800) or 800),
            )
            _install_flight_recorder_hooks(self.sm)
            _install_warp_telemetry(self.sm)
            st = _get_state_snapshot(self.sm)
            return {"ok": True, "result": st, "char_id": int(cid or 0), "state": st}
        if cmd in ("get_state", "observe"):
            st = _get_state_snapshot(self.sm)
            return {"ok": True, "result": st, "state": st}
        if cmd in ("capture_screenshot", "screenshot"):
            cap_res = _capture_trinity_screenshot(params.get("path", ""))
            return {"ok": bool(cap_res.get("ok")), "result": cap_res}
        if cmd in ("execute_action", "act"):
            act_res = _execute_action(self.sm, params.get("action", {}))
            return {"ok": True, "result": act_res, "action_result": act_res}
        if cmd == "shutdown":
            self.running = False
            return {"ok": True, "result": {"shutting_down": True}, "shutting_down": True}
        return {"ok": False, "error": "unknown method/cmd %r" % (cmd,)}


def _capture_trinity_screenshot(path):
    """Capture the live EVE.app 3D + CarbonUI backbuffer via Trinity (works even on an off-screen Mission Control Space)."""
    if not path:
        return {"ok": False, "error": "empty path"}
    p = str(path)
    try:
        parent = os.path.dirname(p)
        if parent and not os.path.exists(parent):
            os.makedirs(parent)
    except Exception:
        pass
    try:
        import trinity
        if hasattr(trinity, "SaveRenderTarget"):
            trinity.SaveRenderTarget(p)
            if os.path.exists(p) and os.path.getsize(p) > 0:
                return {"ok": True, "mode": "trinity_save_render_target", "path": p}
        dev = getattr(trinity, "device", None)
        if dev is not None and hasattr(dev, "GetRenderContext"):
            rt = dev.GetRenderContext().GetDefaultBackBuffer()
            if rt is not None:
                if not getattr(rt, "isReadable", False):
                    readable = trinity.Tr2RenderTarget(rt.width, rt.height, 1, rt.format)
                    rt.Resolve(readable)
                    bmp = trinity.Tr2HostBitmap(readable)
                else:
                    bmp = trinity.Tr2HostBitmap(rt)
                bmp.Save(p)
                if os.path.exists(p) and os.path.getsize(p) > 0:
                    return {"ok": True, "mode": "trinity_host_bitmap", "path": p}
    except Exception as exc:
        return {"ok": False, "error": str(exc)}
    return {"ok": False, "error": "trinity backbuffer save did not create file"}


def _ensure_windowed_mode(sm, width=1280, height=800):
    """Switch EVE.app into compact windowed mode (default 1280x800, 1.0 UI scale)
    via DeviceMgr.SetWindowState (bypassing ForceSize/_SetDevice's 15s confirmation modal)."""
    try:
        import trinity
        mw = getattr(trinity, "mainWindow", None)
        if mw is None or not hasattr(mw, "GetWindowState"):
            return
        win_mode = getattr(getattr(trinity, "Tr2WindowMode", None), "WINDOWED", 1)
        ws = mw.GetWindowState()
        cur_mode = getattr(ws, "windowMode", None)
        cur_w = int(getattr(ws, "width", 0) or 0)
        cur_h = int(getattr(ws, "height", 0) or 0)
        need_resize = not (cur_mode == win_mode and 800 <= cur_w <= int(width) and 600 <= cur_h <= int(height))
        dev_svc = sm.GetService("device") if sm is not None and hasattr(sm, "GetService") else None
        if need_resize:
            st = ws.CopyTo() if hasattr(ws, "CopyTo") else trinity.Tr2MainWindowState()
            st.windowMode = win_mode
            st.width = int(width)
            st.height = int(height)
            st.left = 40
            st.top = 40
            if dev_svc is not None and hasattr(dev_svc, "SetWindowState"):
                dev_svc.SetWindowState(st)
            elif hasattr(mw, "SetWindowState"):
                mw.SetWindowState(st)
            H._sleep_ms(300)
        if dev_svc is not None and hasattr(dev_svc, "SetUIScaleValue"):
            try:
                dev_svc.SetUIScaleValue(1.0)
            except Exception:
                pass
    except Exception:
        pass


def run_playtester_hook():
    cfg_path = os.environ.get("EVOMU_PLAYTESTER_CFG", "") or os.environ.get("EVOMU_INCLIENT_CFG", "")
    if not cfg_path or not os.path.exists(cfg_path):
        return
    cfg = json.load(open(cfg_path, "r"))
    for _ in range(160):
        sm = getattr(__builtin__, "sm", None)
        if sm is not None and hasattr(sm, "IsServiceRunning") and sm.IsServiceRunning("machoNet"):
            break
        H._sleep_ms(250)
    sm = getattr(__builtin__, "sm", None)
    try:
        import evomu_site
        evomu_site._install_logmodule_hook(propagate=True)
    except Exception:
        pass
    try:
        H._wait_for_session(
            sm,
            cfg.get("user", "demo"),
            cfg.get("password", "demo-secret"),
            int(cfg.get("char_id", 90000001)),
            timeout_sec=120.0,
        )
    except Exception:
        pass
    _ensure_windowed_mode(
        sm,
        int(cfg.get("window_width", 1280) or 1280),
        int(cfg.get("window_height", 800) or 800),
    )
    _install_flight_recorder_hooks(sm)
    _install_warp_telemetry(sm)
    srv = PlaytesterRpcServer(cfg, sm)
    srv.serve_forever_on_stackless()


def schedule_startup():
    global _STARTED
    if _STARTED:
        return
    _STARTED = True
    try:
        import uthread
        uthread.new(run_playtester_hook)
    except Exception:
        t = threading.Thread(target=run_playtester_hook)
        t.setDaemon(True)
        t.start()
