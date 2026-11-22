# -*- coding: utf-8 -*-
"""Helper utilities for the EvoMu in-client journey driver (Python 2.7)."""

try:
    import __builtin__
except ImportError:
    import builtins as __builtin__
import json
import os
import socket
import time

try:
    _INT_TYPES = (int, long)
except NameError:
    _INT_TYPES = (int,)

try:
    _STR_TYPES = (basestring,)
except NameError:
    _STR_TYPES = (str, bytes)


def _to_long(val):
    try:
        return long(val)
    except NameError:
        return int(val)


def _sleep_ms(ms):
    sec = max(0.005, float(ms) / 1000.0)
    try:
        import blue
        blue.pyos.synchro.SleepWallclock(max(1, int(ms)))
        return
    except Exception:
        pass
    try:
        import uthread2
        uthread2.sleep(sec)
        return
    except Exception:
        pass
    time.sleep(sec)


def _trace(cfg, msg):
    try:
        out_dir = (cfg or {}).get("out_dir")
        if out_dir:
            with open(os.path.join(out_dir, "driver_trace.log"), "a") as tf:
                tf.write("[%.3f] %s\n" % (time.time(), msg))
                tf.flush()
    except Exception:
        pass


def _atomic_write_json(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.flush()
    if os.path.exists(path):
        try:
            os.remove(path)
        except Exception:
            pass
    os.rename(tmp, path)


class IpcClient(object):
    def __init__(self, port):
        self.sock = socket.create_connection(("127.0.0.1", int(port)), 15.0) if port else None
        if self.sock is not None:
            self.sock.settimeout(30.0)
        self.rfile = self.sock.makefile("rb") if self.sock is not None else None

    def request(self, payload):
        if self.sock is None:
            return {"ok": False, "error": "no ipc socket"}
        wire = (json.dumps(payload) + "\n").encode("utf-8")
        self.sock.sendall(wire)
        resp = self.rfile.readline()
        if not resp:
            return {"ok": False, "error": "ipc eof"}
        if isinstance(resp, bytes):
            resp = resp.decode("utf-8", "replace")
        return json.loads(resp)

    def notify(self, payload):
        try:
            return self.request(payload)
        except Exception:
            return {"ok": False}

    def close(self):
        for h in (self.rfile, self.sock):
            try:
                if h is not None:
                    h.close()
            except Exception:
                pass


_HOST_GM_STATE = {
    "port": 0,
    "client": None,
    "target_char_id": 0,
    "sim_seq": 0,
    "sim_events": [],
}


def _resolve_target_char_id(target_char_id=0):
    if target_char_id:
        return int(target_char_id)
    sess = getattr(__builtin__, "session", None)
    if sess is not None:
        live = int(getattr(sess, "charid", 0) or 0)
        if live:
            return live
    return int(_HOST_GM_STATE.get("target_char_id") or 0)


def _resolve_target_solar_system_id(target_solar_system_id=0):
    if target_solar_system_id:
        return int(target_solar_system_id)
    sess = getattr(__builtin__, "session", None)
    if sess is not None:
        live = int(getattr(sess, "solarsystemid", 0) or getattr(sess, "solarsystemid2", 0) or 0)
        if live:
            return live
    return 0


def _configure_host_gm_bridge(cfg=None, ipc=None, target_char_id=0, port=0):
    cfg_dict = cfg if isinstance(cfg, dict) else {}
    cfg_mode = str(cfg_dict.get("mode") or "").strip().lower()
    if port:
        resolved_port = int(port)
    elif ipc is not None:
        resolved_port = 0
    elif isinstance(cfg, dict) and "host_gm_port" in cfg:
        resolved_port = int(cfg.get("host_gm_port") or 0)
    else:
        resolved_port = int(os.environ.get("EVOMU_HOST_GM_PORT") or 0)

    resolved_char = int(
        target_char_id
        or cfg_dict.get("char_id")
        or 0
    )
    if not resolved_char:
        sess = getattr(__builtin__, "session", None)
        if sess is not None:
            resolved_char = int(getattr(sess, "charid", 0) or 0)

    old_port = int(_HOST_GM_STATE.get("port") or 0)
    old_client = _HOST_GM_STATE.get("client")
    if resolved_port != old_port or (resolved_port == 0 and ipc is None and old_client is not None):
        if old_client is not None and old_client is not ipc and hasattr(old_client, "close"):
            try:
                old_client.close()
            except Exception:
                pass
        _HOST_GM_STATE["client"] = ipc
    elif ipc is not None:
        _HOST_GM_STATE["client"] = ipc

    _HOST_GM_STATE["port"] = resolved_port
    _HOST_GM_STATE["configured_with_ipc"] = bool(ipc is not None)
    _HOST_GM_STATE["mode"] = ("journeys" if (ipc is not None and not cfg_mode) else cfg_mode)
    if resolved_char:
        _HOST_GM_STATE["target_char_id"] = resolved_char
    return {
        "port": int(_HOST_GM_STATE.get("port") or 0),
        "target_char_id": int(_HOST_GM_STATE.get("target_char_id") or 0),
        "has_client": _HOST_GM_STATE.get("client") is not None,
    }


def _reset_sim_time_telemetry():
    _HOST_GM_STATE["sim_seq"] = 0
    _HOST_GM_STATE["sim_events"] = []


def snapshot_sim_time_cursor():
    return int(_HOST_GM_STATE.get("sim_seq") or 0)


def _record_sim_time_injection(
    seconds,
    target_char_id=0,
    via="gm_bridge",
    injection_id=None,
    source="inclient",
    method="AdvanceClock",
):
    seq = int(_HOST_GM_STATE.get("sim_seq") or 0) + 1
    _HOST_GM_STATE["sim_seq"] = seq
    inj_id = injection_id if injection_id else ("inclient-sim-%d" % seq)
    evt = {
        "seq": seq,
        "injection_id": str(inj_id),
        "seconds": float(seconds),
        "target_char_id": int(target_char_id or 0),
        "via": str(via or "gm_bridge"),
        "source": str(source or "inclient"),
        "method": str(method or "AdvanceClock"),
        "ts": time.time(),
    }
    events = _HOST_GM_STATE.get("sim_events")
    if not isinstance(events, list):
        events = []
        _HOST_GM_STATE["sim_events"] = events
    events.append(evt)
    if len(events) > 2000:
        del events[:-1000]
    return evt


def collect_sim_time_telemetry(since_seq=0):
    events = [
        dict(evt)
        for evt in (_HOST_GM_STATE.get("sim_events") or [])
        if int(evt.get("seq", 0) or 0) > int(since_seq or 0)
    ]
    total_sec = 0.0
    for evt in events:
        try:
            total_sec += float(evt.get("seconds", 0.0) or 0.0)
        except Exception:
            pass
    return {
        "sim_time_injected_sec": round(total_sec, 6),
        "sim_time_injection_count": len(events),
        "sim_time_injections": events,
        "sim_seq_after": snapshot_sim_time_cursor(),
    }


def _gm_ipc_request(payload, ipc=None):
    client = ipc if ipc is not None else _HOST_GM_STATE.get("client")
    if ipc is not None or _HOST_GM_STATE.get("configured_with_ipc"):
        port = 0
    elif _HOST_GM_STATE.get("mode"):
        port = int(_HOST_GM_STATE.get("port") or 0)
    else:
        port = int(_HOST_GM_STATE.get("port") or os.environ.get("EVOMU_HOST_GM_PORT") or 0)
    if client is None and port > 0:
        try:
            client = IpcClient(port)
            _HOST_GM_STATE["port"] = port
            _HOST_GM_STATE["client"] = client
        except Exception as exc:
            return {"ok": False, "error": str(exc)}
    if client is None:
        return None
    try:
        res = client.request(payload)
        if (
            ipc is None
            and port > 0
            and isinstance(res, dict)
            and res.get("error") == "ipc eof"
        ):
            try:
                client.close()
            except Exception:
                pass
            _HOST_GM_STATE["client"] = None
            client = IpcClient(port)
            _HOST_GM_STATE["client"] = client
            res = client.request(payload)
        return res
    except Exception as exc:
        if ipc is None and port > 0:
            try:
                client.close()
            except Exception:
                pass
            _HOST_GM_STATE["client"] = None
            try:
                client = IpcClient(port)
                _HOST_GM_STATE["client"] = client
                return client.request(payload)
            except Exception as retry_exc:
                return {"ok": False, "error": str(retry_exc)}
        return {"ok": False, "error": str(exc)}


def _clear_session_timer(sm, ipc=None, target_char_id=0, allow_time_control=True, raise_on_error=False):
    if not allow_time_control:
        return False
    tgt = _resolve_target_char_id(target_char_id)
    res = _gm_ipc_request(
        {"cmd": "clear_session_timer", "target_char_id": tgt},
        ipc=ipc,
    )
    if isinstance(res, dict) and res.get("ok"):
        return True
    if raise_on_error:
        if res is None:
            raise RuntimeError("Host GM bridge unavailable for ClearSessionTimer(target_char_id=%d)" % tgt)
        raise RuntimeError(
            "Host GM ClearSessionTimer(target_char_id=%d) failed: %s"
            % (tgt, (res or {}).get("error") or (res or {}).get("reason") or res)
        )
    return False


def _get_field(obj, name, default=None):
    if obj is None:
        return default
    if isinstance(obj, dict):
        return obj.get(name, default)
    if hasattr(obj, name):
        return getattr(obj, name)
    try:
        return obj[name]
    except Exception:
        return default


def _extract_id(res, keys=("itemID", "contractID", "fleetID", "jobID", "mailID", "eventID", "labelID", "listID", "corporationID", "allianceID", "structureID", "orderID")):
    if isinstance(res, _INT_TYPES):
        return int(res)
    for k in keys:
        v = res.get(k) if isinstance(res, dict) else getattr(res, k, None)
        if v:
            return int(v)
    if isinstance(res, (tuple, list)) and len(res) > 0:
        first = res[0]
        if isinstance(first, _INT_TYPES):
            return int(first)
        if hasattr(first, "GetMachoObjectBoundId"):
            return first
    return res


def _get_uicore():
    uc = getattr(__builtin__, "uicore", None)
    if uc is not None:
        return uc
    try:
        from carbonui.uicore import uicore as _uc
        return _uc
    except Exception:
        return None


def _ensure_view(sm, target_view):
    uicore = _get_uicore()
    if uicore is not None and getattr(uicore, "layer", None) is not None:
        charsel = getattr(uicore.layer, "charsel", None)
        if charsel is not None and getattr(charsel, "isopen", False):
            try:
                charsel.CloseView()
            except Exception:
                pass
    vs = None
    try:
        vs = sm.GetService("viewState")
    except Exception:
        vs = None
    if vs is not None:
        for _ in range(100):
            cur = vs.GetCurrentViewInfo()
            cname = getattr(cur, "name", None) if cur is not None else None
            opening = getattr(vs, "isOpeningView", None)
            if not opening:
                if target_view == "hangar" and (
                    vs.IsViewActive("hangar")
                    or vs.IsViewActive("station")
                    or cname in ("hangar", "station")
                ):
                    break
                if vs.IsViewActive(target_view) or cname == target_view:
                    break
            _sleep_ms(100)
        else:
            if not getattr(vs, "isOpeningView", None):
                cur = vs.GetCurrentViewInfo()
                cname = getattr(cur, "name", None) if cur is not None else None
                already = (
                    target_view == "hangar" and (
                        vs.IsViewActive("hangar") or vs.IsViewActive("station") or cname in ("hangar", "station")
                    )
                ) or vs.IsViewActive(target_view) or cname == target_view
                if not already:
                    if hasattr(vs, "ChangePrimaryView"):
                        vs.ChangePrimaryView(target_view)
                    else:
                        vs.ActivateView(target_view)
                    _sleep_ms(300)
    loading = None
    try:
        loading = sm.GetService("loading")
    except Exception:
        loading = None
    if loading is not None and hasattr(loading, "IsLoading"):
        for _ in range(40):
            if not loading.IsLoading():
                break
            _sleep_ms(100)
    _wait_view_settled(sm)


def _wait_view_settled(sm, timeout_sec=10.0):
    vs = None
    try:
        vs = sm.GetService("viewState")
    except Exception:
        vs = None
    services_map = getattr(sm, "services", None) or {}
    loading = services_map.get("loading")
    start = time.time()
    deadline = start + timeout_sec
    while time.time() < deadline:
        now = time.time()
        opening = getattr(vs, "isOpeningView", None) if vs is not None else None
        is_loading = bool(loading.IsLoading()) if loading is not None and hasattr(loading, "IsLoading") else False
        space_svc = services_map.get("space")
        lazy_cnt = getattr(space_svc, "lazyLoadQueueCount", 0) if space_svc is not None else 0
        sess = getattr(__builtin__, "session", None)
        sess_busy = bool(sess is not None and (getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None))
        sys_id = int(getattr(sess, "solarsystemid", 0) or 0) if sess is not None else 0
        in_space = bool(
            sess is not None and 30000000 <= sys_id < 34000000
            and not getattr(sess, "stationid", None) and not getattr(sess, "structureid", None)
        )
        bp_ok = True
        if in_space and not opening and vs is not None and vs.IsViewActive("inflight"):
            michelle = services_map.get("michelle")
            bp = getattr(michelle, "_Michelle__bp", None) if michelle is not None else None
            if (bp is None or int(getattr(bp, "solarsystemID", 0) or 0) != sys_id) and now > start + 2.0 and michelle is not None:
                try:
                    michelle.AddBallpark(_to_long(sys_id))
                    sm.RemoteSvc("beyonce").MachoBindObject(int(sys_id), None)
                    bp = getattr(michelle, "_Michelle__bp", None)
                except Exception:
                    pass
            bp_ok = bool(
                bp is not None and getattr(michelle, "bpReady", False)
                and int(getattr(bp, "solarsystemID", 0) or 0) == sys_id
                and getattr(bp, "validState", 0)
            )
        ip_svc = services_map.get("infoPanel")
        ip_busy = False
        if ip_svc is not None and now <= start + 1.5:
            ip_busy = any(
                getattr(ip_svc, attr, None) is not None
                for attr in (
                    "operationInfoPanelUpdater",
                    "recommendationsInfoPanelUpdater",
                    "missionInfoPanelUpdater",
                    "airNpeInfoPanelUpdater",
                )
            )
        if not opening and not is_loading and not lazy_cnt and not sess_busy and not ip_busy and bp_ok:
            break
        _sleep_ms(50)
    _sleep_ms(100)


def _sync_quasar_gateway(sm, char_id=None, raise_on_error=False):
    try:
        pgw = sm.GetService("publicGatewaySvc")
        sess = getattr(__builtin__, "session", None)
        uid_cur = int(getattr(sess, "userid", 1) or 1)
        gw = getattr(pgw, "publicGateway", None)
        if gw is not None:
            if hasattr(gw, "set_authenticated_user_id"):
                gw.set_authenticated_user_id(uid_cur)
            if char_id and hasattr(gw, "set_active_character_id"):
                gw.set_active_character_id(int(char_id))
            broker_details = gw.get_request_broker_details() if hasattr(gw, "get_request_broker_details") else {}
            if isinstance(broker_details, dict):
                ch_state = str(broker_details.get("channel_state", "")).upper()
            else:
                ch_state = str(getattr(broker_details, "channel_state", "")).upper()
            need_setup = (
                getattr(gw, "_evomu_dev_uid", None) != uid_cur
                or getattr(gw, "_evomu_dev_cid", None) != char_id
            )
            if not need_setup and ch_state not in ("READY", "CONNECTED", "CONNECTING"):
                need_setup = True
            if need_setup:
                tok = None
                try:
                    tok = sm.RemoteSvc("userSvc").GetUserToken()
                except Exception:
                    if raise_on_error:
                        raise
                    tok = None
                if tok and hasattr(pgw, "set_auth_token"):
                    pgw.set_auth_token(tok)
                    if ch_state not in ("READY", "CONNECTED", "CONNECTING") and hasattr(pgw, "connect"):
                        pgw.connect()
                elif tok and hasattr(gw, "set_auth_token"):
                    gw.set_auth_token(tok)
                    if ch_state not in ("READY", "CONNECTED", "CONNECTING") and hasattr(gw, "connect"):
                        gw.connect()
                elif hasattr(pgw, "setup_local_dev_token"):
                    pgw.setup_local_dev_token(uid_cur)
                if hasattr(gw, "get_request_broker_details"):
                    for _ in range(15):
                        bd = gw.get_request_broker_details()
                        st = str(bd.get("channel_state", "") if isinstance(bd, dict) else getattr(bd, "channel_state", "")).upper()
                        if st in ("READY", "CONNECTED"):
                            break
                        _sleep_ms(50)
                gw._evomu_dev_uid = uid_cur
                gw._evomu_dev_cid = char_id
        return pgw
    except Exception:
        if raise_on_error:
            raise
        return None


def _assert_ui_rendered(sm, expected_view):
    vs = sm.GetService("viewState")
    cur = vs.GetCurrentViewInfo()
    cname = getattr(cur, "name", None) if cur is not None else None
    if expected_view == "hangar":
        ok_view = bool(vs.IsViewActive("hangar") or vs.IsViewActive("station") or cname in ("hangar", "station"))
    else:
        ok_view = bool(vs.IsViewActive(expected_view) or cname == expected_view)
    if not ok_view:
        raise RuntimeError("UI view mismatch: expected %r, got current=%r" % (expected_view, cname))
    loading = sm.GetService("loading")
    if loading.IsLoading():
        raise RuntimeError("UI loading curtain still active (IsLoading() == True)")
    uicore = _get_uicore()
    if uicore is not None and getattr(uicore, "layer", None) is not None:
        llayer = getattr(uicore.layer, "loading", None)
        if llayer is not None and getattr(llayer, "display", False) and getattr(llayer, "state", 2) not in (1, 2):
            raise RuntimeError("UI loading layer still visible: state=%r" % (getattr(llayer, "state", None),))
    scene_mgr = sm.GetService("sceneManager")
    scene = scene_mgr.GetActiveScene()
    for _ in range(30):
        if scene is not None:
            break
        _sleep_ms(100)
        scene = scene_mgr.GetActiveScene()
    if scene is None:
        raise RuntimeError("sceneManager.GetActiveScene() is None for %r" % (expected_view,))
    godma = sm.GetService("godma")
    deadline_godma = time.time() + 5.0
    while getattr(godma, "state", None) == 2 and time.time() < deadline_godma:
        _sleep_ms(100)
    if not sm.IsServiceRunning("godma"):
        failed_svcs = [(k, getattr(v, "state", None)) for k, v in getattr(sm, "services", {}).items() if getattr(v, "state", None) != 4]
        raise RuntimeError("godma service not running: state=%r non_running=%r" % (getattr(godma, "state", None), failed_svcs))
    if expected_view == "inflight":
        michelle = sm.GetService("michelle")
        bp = getattr(michelle, "_Michelle__bp", None)
        for _ in range(30):
            if bp is not None:
                break
            _sleep_ms(100)
            bp = getattr(michelle, "_Michelle__bp", None)
        if bp is None:
            raise RuntimeError("michelle.GetBallpark() is None in inflight view")


_CLUSTER_SHUTDOWN_MODAL_MARKERS = (
    "clustershuttingdown",
    "cluster shutdown in progress",
    "daily cluster downtime",
    "non-gm logins are rejected",
    "cluster is currently shutting down",
    "server is shutting down",
)


def _is_widget_visible(obj):
    if obj is None or getattr(obj, "destroyed", False):
        return False
    if not getattr(obj, "display", True):
        return False
    if getattr(obj, "state", None) == 2:
        return False
    return True


def _is_modal_dialog_root(obj):
    if obj is None:
        return False
    if getattr(obj, "isModal", False):
        return True
    cls_name = str(getattr(getattr(obj, "__class__", None), "__name__", "") or "")
    return any(tok in cls_name for tok in ("MessageBox", "Dialog", "Popup", "HybridWindow", "Modal"))


def _iter_visible_modal_roots(uic):
    if uic is None:
        return []
    roots = []
    seen = set()

    def _add(node):
        if node is None or id(node) in seen or not _is_widget_visible(node):
            return
        seen.add(id(node))
        roots.append(node)

    reg = getattr(uic, "registry", None)
    if reg is not None:
        get_modal = getattr(reg, "GetModalWindow", None)
        if callable(get_modal):
            try:
                _add(get_modal())
            except Exception:
                pass
        for mw in list(getattr(reg, "modals", None) or []):
            _add(mw)
    lyr = getattr(uic, "layer", None)
    if lyr is not None:
        modal_lyr = getattr(lyr, "modal", None)
        if modal_lyr is not None and _is_widget_visible(modal_lyr):
            for ch in list(getattr(modal_lyr, "children", None) or []):
                if ch is None:
                    continue
                cname = str(getattr(ch, "name", "") or "")
                if cname.startswith("l_modal_"):
                    if _is_widget_visible(ch):
                        for gch in list(getattr(ch, "children", None) or []):
                            _add(gch)
                else:
                    _add(ch)
        above_lyr = getattr(lyr, "abovemain", None)
        if above_lyr is not None and _is_widget_visible(above_lyr):
            for ch in list(getattr(above_lyr, "children", None) or []):
                if _is_widget_visible(ch) and _is_modal_dialog_root(ch):
                    _add(ch)
    return roots


def _collect_widget_text_fragments(root, max_nodes=40):
    frags = []
    if not _is_widget_visible(root):
        return frags
    queue = [root]
    visited = set()
    while queue and len(visited) < max_nodes:
        obj = queue.pop(0)
        if obj is None or id(obj) in visited or not _is_widget_visible(obj):
            continue
        visited.add(id(obj))
        for attr in ("msgKey", "caption", "text"):
            val = getattr(obj, attr, None)
            if isinstance(val, _STR_TYPES):
                try:
                    if isinstance(val, bytes) and str is not bytes:
                        s = val.decode("utf-8", "replace")
                    elif isinstance(val, str):
                        s = val
                    else:
                        s = val.encode("utf-8", "replace")
                except Exception:
                    try:
                        s = str(val)
                    except Exception:
                        s = ""
                s = s.strip()
                if s and s not in frags:
                    frags.append(s)
        msg_lbl = getattr(obj, "_message_label", None)
        if msg_lbl is not None and id(msg_lbl) not in visited:
            queue.append(msg_lbl)
        children = getattr(obj, "children", None)
        if children:
            try:
                for ch in list(children)[:16]:
                    if ch is not None and id(ch) not in visited:
                        queue.append(ch)
            except Exception:
                pass
    return frags


def _detect_cluster_shutdown_modal_message():
    uic = _get_uicore()
    if uic is None:
        return ""
    for root in _iter_visible_modal_roots(uic):
        frags = _collect_widget_text_fragments(root)
        if not frags:
            continue
        joined = " | ".join(frags)
        low = joined.lower()
        if any(marker in low for marker in _CLUSTER_SHUTDOWN_MODAL_MARKERS):
            return joined[:200]
    return ""


def _wait_for_session(sm, user, password, target_char_id, timeout_sec=120.0, defer_char_select=False):
    deadline = time.time() + timeout_sec
    tried_manual_login, tried_manual_select = False, False
    chosen_char_id, avail_ids = int(target_char_id or 0), []
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        userid = getattr(sess, "userid", None) if sess is not None else None
        charid = getattr(sess, "charid", None) if sess is not None else None
        if userid:
            _sync_quasar_gateway(sm, int(charid) if charid else None)
        if defer_char_select and userid and not charid:
            try:
                vs = sm.GetService("viewState") if sm is not None else None
            except Exception:
                vs = None
            cur = vs.GetCurrentViewInfo() if (vs is not None and hasattr(vs, "GetCurrentViewInfo")) else None
            cname = getattr(cur, "name", None) if cur is not None else None
            if cname == "charsel" and not getattr(vs, "isOpeningView", None):
                uc = _get_uicore()
                cs_layer = getattr(getattr(uc, "layer", None), "charsel", None) if uc is not None else None
                cs_data = getattr(cs_layer, "characterSelectionData", None) if cs_layer is not None else None
                cs_slots = getattr(cs_layer, "slots", None) if cs_layer is not None else None
                if cs_layer is None or cs_data is not None or cs_slots:
                    _sleep_ms(200)
                    return 0
        sess_busy = bool(
            sess is not None
            and (getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None)
        )
        has_loc = bool(
            sess is not None
            and (
                getattr(sess, "stationid", None)
                or getattr(sess, "structureid", None)
                or getattr(sess, "solarsystemid", None)
            )
        )
        if charid and not sess_busy and has_loc and (
            not target_char_id
            or int(charid) == int(chosen_char_id)
            or (avail_ids and int(target_char_id) not in avail_ids and int(charid) in avail_ids)
        ):
            vs = sm.GetService("viewState")
            init_view = (
                "inflight"
                if (getattr(sess, "solarsystemid", None) and not getattr(sess, "stationid", None) and not getattr(sess, "structureid", None))
                else "hangar"
            )
            cur = vs.GetCurrentViewInfo()
            cname = getattr(cur, "name", None) if cur is not None else None
            if cname in (None, "login", "charsel") or getattr(vs, "isOpeningView", None):
                _sleep_ms(150)
                continue
            _sleep_ms(200)
            _sync_quasar_gateway(sm, int(charid))
            _ensure_view(sm, init_view)
            try:
                sm.GetService("shipTree")
            except Exception:
                pass
            return int(charid)
        conn_svc = sm.services.get("connection") if hasattr(sm, "services") else None
        macho_svc = sm.services.get("machoNet") if hasattr(sm, "services") else None
        conn_busy = bool(
            (
                conn_svc is not None
                and (
                    getattr(conn_svc, "connecting", False)
                    or getattr(conn_svc, "isConnecting", False)
                )
            )
            or (
                macho_svc is not None
                and (
                    getattr(macho_svc, "authenticating", 0)
                    or getattr(macho_svc, "gettingServerStatus", 0)
                    or getattr(macho_svc, "transportsByID", None)
                    or (hasattr(macho_svc, "IsConnected") and macho_svc.IsConnected())
                )
            )
        )
        if not userid and not tried_manual_login and not conn_busy and time.time() > deadline - 5.0:
            tried_manual_login = True
            try:
                if sm.IsServiceRunning("connection"):
                    sm.GetService("connection").Login(user, password, None, None)
                else:
                    sm.RemoteSvc("authentication").Login(user, password)
            except Exception:
                try:
                    sm.RemoteSvc("authentication").Login(user, password)
                except Exception:
                    pass
        if userid and not charid and not tried_manual_select and not defer_char_select:
            vs = sm.GetService("viewState")
            cur = vs.GetCurrentViewInfo()
            cname = getattr(cur, "name", None) if cur is not None else None
            view_ready = (cname == "charsel" and not getattr(vs, "isOpeningView", None))
            if view_ready or time.time() > deadline - (timeout_sec - 12.0):
                tried_manual_select = True
                chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
                avail_ids = [int(_get_field(c, "characterID", 0)) for c in (chars or []) if _get_field(c, "characterID", None)]
                if target_char_id and int(target_char_id) in avail_ids:
                    chosen_char_id = int(target_char_id)
                elif avail_ids:
                    chosen_char_id = avail_ids[0]
                if sess is not None:
                    sess.nextSessionChange = None
                sm.GetService("sessionMgr").PerformSessionChange("charsel", sm.RemoteSvc("charUnboundMgr").SelectCharacterID, int(chosen_char_id), None, True)
        cur_sess = getattr(__builtin__, "session", None)
        if not getattr(cur_sess, "charid", None):
            shutdown_msg = _detect_cluster_shutdown_modal_message()
            if shutdown_msg:
                raise RuntimeError("ClusterShuttingDown: %s" % shutdown_msg)
        _sleep_ms(250)
    sess = getattr(__builtin__, "session", None)
    if getattr(sess, "charid", None):
        return int(sess.charid)
    raise RuntimeError("Timed out waiting for EVE session character selection")


def _live_session_attr(attr, default=None):
    sess = getattr(__builtin__, "session", None)
    val = getattr(sess, attr, default) if sess is not None else default
    return default if val is None else val


_MODULE_SLOT_FLAGS = set(range(11, 35))
_RIG_SLOT_FLAGS = set(range(92, 100))
_SUBSYSTEM_SLOT_FLAGS = set(range(125, 133))
_FITTED_MODULE_FLAGS = _MODULE_SLOT_FLAGS | _RIG_SLOT_FLAGS | _SUBSYSTEM_SLOT_FLAGS


def _snapshot_ship_fitted_items(sm, ship_id):
    """Return dict {itemID: {'flag_id': int, 'category_id': int|None}} fitted on ship_id."""
    if not ship_id:
        return {}
    _, rows = sm.RemoteSvc("invbroker").MachoBindObject(int(ship_id), ("List", (), {}))
    rows = rows or []
    fitted = {}
    for row in rows:
        flag = _get_field(row, "flagID", None)
        if flag is None:
            flag = _get_field(row, "flag", None)
        if flag is None or int(flag) not in _FITTED_MODULE_FLAGS:
            continue
        loc = _get_field(row, "locationID", None)
        if loc is not None and int(loc) != int(ship_id):
            continue
        iid = _extract_id(row, ("itemID", "itemId"))
        if iid:
            cat_id = _get_field(row, "categoryID", None)
            fitted[int(iid)] = {
                "flag_id": int(flag),
                "category_id": int(cat_id) if cat_id is not None else None,
            }
    return fitted


def _snapshot_ship_fitted_item_ids(sm, ship_id):
    """Return the set of itemIDs currently fitted in module/rig/subsystem slots on ship_id."""
    return set(_snapshot_ship_fitted_items(sm, ship_id).keys())


def _fitted_strip_sort_key(entry):
    iid, meta = entry
    cat_id = meta.get("category_id")
    flag_id = meta.get("flag_id", 0)
    if cat_id == 8:
        tier = 0
    elif flag_id in _MODULE_SLOT_FLAGS:
        tier = 1
    elif flag_id in _RIG_SLOT_FLAGS:
        tier = 2
    else:
        tier = 3
    return (tier, int(iid))


def _strip_extra_ship_fittings(sm, home_station, home_ship, baseline_fitted):
    """Strip non-baseline fitted charges, modules, rigs, and subsystems from home_ship."""
    cur_ship_id = int(_live_session_attr("shipid", 0) or home_ship or 0)
    if cur_ship_id != home_ship:
        raise RuntimeError(
            "Cannot strip modules: expected home_ship=%d, got %d" % (home_ship, cur_ship_id)
        )
    baseline_set = set(int(x) for x in (baseline_fitted or ()))
    extra_items = dict(
        (iid, meta)
        for iid, meta in _snapshot_ship_fitted_items(sm, home_ship).items()
        if iid not in baseline_set
    )
    if not extra_items:
        return
    inv_hangar, _ = sm.RemoteSvc("invbroker").MachoBindObject(home_station, None)
    dogma_ship = None
    ordered_items = sorted(extra_items.items(), key=_fitted_strip_sort_key)
    for iid, _ in ordered_items:
        # Re-snapshot before each unfit: moving a module auto-unfits loaded charges
        # to home_station, so a stale inv_hangar.Add(charge, home_ship, flag=4) would
        # re-inject the charge onto home_ship with flag=4 via resolve_inv_target_location.
        current_fitted = _snapshot_ship_fitted_items(sm, home_ship)
        if iid not in current_fitted:
            continue
        flag_id = current_fitted[iid]["flag_id"]
        if flag_id in _RIG_SLOT_FLAGS:
            if dogma_ship is None:
                dogma_ship, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(home_ship, None)
            dogma_ship.DestroyFitting(iid)
            continue
        try:
            inv_hangar.Add(iid, home_ship, flag=4)
        except Exception:
            if dogma_ship is None:
                dogma_ship, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(home_ship, None)
            dogma_ship.TakeModuleOffline(home_ship, iid)
            inv_hangar.Add(iid, home_ship, flag=4)
    remaining_extra = sorted(_snapshot_ship_fitted_item_ids(sm, home_ship) - baseline_set)
    if remaining_extra:
        raise RuntimeError(
            "Extra fitted items remain on home_ship %d after strip: %s"
            % (home_ship, remaining_extra)
        )


def _wait_session_attr(attr, expected, timeout_sec=5.0, raise_on_timeout=True):
    deadline = time.time() + float(timeout_sec)
    cur = None
    busy = False
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        cur = getattr(sess, attr, None) if sess is not None else None
        busy = bool(
            sess is not None
            and (getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None)
        )
        if cur == expected and not busy:
            _sleep_ms(200)
            return cur
        _sleep_ms(50)
    sess = getattr(__builtin__, "session", None)
    if sess is not None:
        cur = getattr(sess, attr, None)
        busy = bool(getattr(sess, "mutating", 0) or getattr(sess, "changing", None) is not None)
        if cur == expected and not busy:
            _sleep_ms(200)
            return cur
    if raise_on_timeout:
        raise RuntimeError(
            "Timed out waiting for session.%s == %r (got %r, busy=%r)"
            % (attr, expected, cur, busy)
        )
    return cur


def _try_slash(sm, cmd, ipc=None, target_char_id=0, raise_on_error=False, target_solar_system_id=0):
    tgt = _resolve_target_char_id(target_char_id)
    tgt_sys = _resolve_target_solar_system_id(target_solar_system_id)
    payload = {"cmd": "slash_cmd", "command": cmd, "target_char_id": tgt}
    if tgt_sys:
        payload["target_solar_system_id"] = tgt_sys
    res = _gm_ipc_request(
        payload,
        ipc=ipc,
    )
    if isinstance(res, dict) and (
        res.get("handled")
        or res.get("via") == "gm_bridge"
        or "reason" in res
        or res.get("ok") is False
        or "error" in res
    ):
        if res.get("ok"):
            try:
                sim_sec = float(res.get("sim_time_injected_sec") or 0.0)
            except Exception:
                sim_sec = 0.0
            if sim_sec > 0.0:
                stripped = str(cmd or "").strip()
                verb = stripped.split()[0].lower() if stripped.startswith("/") else "slash"
                _record_sim_time_injection(
                    sim_sec,
                    target_char_id=tgt,
                    via=res.get("via", "gm_bridge"),
                    injection_id=res.get("injection_id"),
                    source="inclient_slash",
                    method="SlashCmd:%s" % verb,
                )
            if cmd.startswith("/giveskill ") or cmd.startswith("/setskill "):
                _sleep_ms(150)
            return res.get("result")
        if raise_on_error:
            raise RuntimeError(
                "Host GM slash_cmd(%r) failed: %s"
                % (cmd, res.get("error") or res.get("reason") or res)
            )
        return None
    if res is None and _HOST_GM_STATE.get("mode") in ("playtester", "scenario", "journeys"):
        if raise_on_error:
            raise RuntimeError("Host GM bridge unavailable for slash_cmd(%r)" % (cmd,))
        return None
    try:
        res = sm.RemoteSvc("slash").SlashCmd(cmd)
        if cmd.startswith("/giveskill ") or cmd.startswith("/setskill "):
            _sleep_ms(150)
        return res
    except Exception:
        if raise_on_error:
            raise
        return None


def _get_beyonce(sm, sys_id):
    try:
        michelle = sm.GetService("michelle")
        rp = michelle.GetRemotePark()
        bp = getattr(michelle, "_Michelle__bp", None)
        if (
            rp is not None
            and getattr(rp, "boundObject", None) is not None
            and bp is not None
            and int(getattr(bp, "solarsystemID", 0) or 0) == int(sys_id)
        ):
            sink = getattr(sm, "_evomu_rpc_sink", None)
            if sink is not None and not isinstance(rp, _RecordedBoundProxy):
                return _RecordedBoundProxy("beyonce.bound", rp, sink)
            return rp
    except Exception:
        pass
    b_obj, _ = sm.RemoteSvc("beyonce").MachoBindObject(int(sys_id), None)
    return b_obj


def _sleep_sim_ms(ms):
    try:
        import blue
        blue.synchro.SleepSim(max(1, int(ms)))
        return
    except Exception:
        pass
    try:
        import blue
        blue.pyos.synchro.SleepSim(max(1, int(ms)))
        return
    except Exception:
        pass
    _sleep_ms(ms)


_WARP_SIGNAL_STATE = {"hooked": False, "in_warp": False, "started": 0, "finished": 0}


def _on_warp_signal_started(*_args, **_kwargs):
    _WARP_SIGNAL_STATE["in_warp"] = True
    _WARP_SIGNAL_STATE["started"] += 1


def _on_warp_signal_finished(*_args, **_kwargs):
    _WARP_SIGNAL_STATE["in_warp"] = False
    _WARP_SIGNAL_STATE["finished"] += 1


def _ensure_warp_signal_tracker(sm):
    if _WARP_SIGNAL_STATE["hooked"]:
        return
    try:
        from eve.client.script.environment.effects import warp_signals

        sig_start = getattr(warp_signals, "on_warp_activated", None) or getattr(
            warp_signals, "on_warp_started", None
        )
        if sig_start is not None:
            sig_start.connect(_on_warp_signal_started)
        warp_signals.on_warp_finished.connect(_on_warp_signal_finished)
        _WARP_SIGNAL_STATE["hooked"] = True
        return
    except Exception:
        pass
    try:
        space_svc = sm.GetService("space")
        for attr in ("warp_signals", "warp_signal", "warpSignals"):
            ws = getattr(space_svc, attr, None)
            if ws is not None and hasattr(ws, "on_warp_finished"):
                if hasattr(ws, "on_warp_activated"):
                    ws.on_warp_activated.connect(_on_warp_signal_started)
                elif hasattr(ws, "on_warp_started"):
                    ws.on_warp_started.connect(_on_warp_signal_started)
                ws.on_warp_finished.connect(_on_warp_signal_finished)
                _WARP_SIGNAL_STATE["hooked"] = True
                break
    except Exception:
        pass


def _wait_session_timer_clear(max_wait_sec=12.0):
    deadline = time.time() + float(max_wait_sec)
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if sess is None:
            return
        nsc = getattr(sess, "nextSessionChange", None)
        mut = getattr(sess, "mutating", 0)
        chg = getattr(sess, "changing", None)
        now_sim = None
        try:
            import blue

            now_sim = blue.os.GetSimTime()
        except Exception:
            now_sim = None
        timer_active = (
            bool(mut)
            or chg is not None
            or (nsc is not None and now_sim is not None and int(nsc) > int(now_sim))
        )
        if not timer_active:
            return
        _sleep_ms(200)


def _wait_warp_fx_settled(sm, timeout_sec=4.0, min_finished=0, allow_time_control=True, ipc=None, post_settle_sec=None, cancel_check=None):
    _ensure_warp_signal_tracker(sm)
    try:
        sess = getattr(__builtin__, "session", None)
        sid = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
        michelle = sm.services.get("michelle") if hasattr(sm, "services") else None
        space_svc = sm.services.get("space") if hasattr(sm, "services") else None
        fx_svc = (
            (sm.services.get("FxSequencer") or sm.services.get("fxSequencer"))
            if hasattr(sm, "services")
            else None
        )
        def _is_warp_settled():
            cur_sess = getattr(__builtin__, "session", None)
            if getattr(cur_sess, "stationid", None) is not None or getattr(cur_sess, "structureid", None) is not None:
                _WARP_SIGNAL_STATE["in_warp"] = False
                return True
            bp = michelle.GetBallpark() if (sid and michelle is not None) else None
            ball = bp.GetBall(sid) if (bp is not None and sid) else None
            warping_ball = ball is not None and int(getattr(ball, "mode", 0) or 0) == 3
            warp_cache = (
                getattr(space_svc, "warpDestinationCache", None)
                if space_svc is not None
                else None
            )
            warping_fx = (
                any(x is not None for x in warp_cache)
                if isinstance(warp_cache, (list, tuple))
                else warp_cache is not None
            )
            if not warping_fx and fx_svc is not None and sid:
                try:
                    if hasattr(fx_svc, "GetAllBallActivationNames") and "effects.Warping" in (
                        fx_svc.GetAllBallActivationNames(sid) or ()
                    ):
                        warping_fx = True
                    if not warping_fx and hasattr(fx_svc, "balls"):
                        for eff in (fx_svc.balls.get(sid) or {}).values():
                            sball = getattr(eff, "shipBall", None)
                            if sball is not None and int(getattr(sball, "mode", 0) or 0) == 3:
                                warping_fx = True
                                break
                except Exception:
                    pass
            signals_settled = not _WARP_SIGNAL_STATE["hooked"] or (
                not _WARP_SIGNAL_STATE["in_warp"]
                and _WARP_SIGNAL_STATE["finished"] >= int(min_finished)
            )
            return not warping_ball and not warping_fx and signals_settled

        deadline = time.time() + float(timeout_sec)
        settled = False
        saw_warping = False
        poll_iters = 0
        adv_enabled = bool(allow_time_control)
        while time.time() < deadline:
            if callable(cancel_check) and cancel_check():
                return
            if _is_warp_settled():
                settled = True
                break
            saw_warping = True
            poll_iters += 1
            if adv_enabled and poll_iters >= 3 and (poll_iters % 2 == 1):
                adv_step_ok = bool(_advance_clock(sm, ipc, 4, raise_on_error=False))
                if not adv_step_ok:
                    adv_enabled = False
                _sleep_sim_ms(200 if adv_step_ok else 100)
            else:
                _sleep_sim_ms(100)
        if callable(cancel_check) and cancel_check():
            return
        sess = getattr(__builtin__, "session", None)
        if getattr(sess, "stationid", None) is None and getattr(sess, "structureid", None) is None:
            if not (settled and post_settle_sec is not None):
                adv_ok = False
                if adv_enabled:
                    adv_ok = bool(_advance_clock(sm, ipc, 2, raise_on_error=False))
                    _sleep_sim_ms(1500 if adv_ok else 600)
                else:
                    _sleep_sim_ms(600)
                _sleep_ms(300)
                if not settled:
                    post_budget = (
                        float(post_settle_sec)
                        if post_settle_sec is not None
                        else (4.0 if (allow_time_control and adv_ok) else 12.0)
                    )
                    post_deadline = time.time() + post_budget
                    while time.time() < post_deadline:
                        if callable(cancel_check) and cancel_check():
                            return
                        if _is_warp_settled():
                            break
                        _sleep_sim_ms(100)
        if callable(cancel_check) and cancel_check():
            return
        if not _WARP_SIGNAL_STATE["hooked"] or saw_warping:
            # CCP's WarpLoop sleeps blue.synchro.SleepSim(500) per iteration and emits
            # OnSpecialFX(..., "effects.Warping", 0) only after waking from that sleep.
            # Wait >500ms sim time after warp clears so a follow-up CmdWarpToStuff cannot
            # have its new Warping activation stopped mid-SleepSim (Warp.py:597).
            _sleep_sim_ms(600)
        if _WARP_SIGNAL_STATE["in_warp"]:
            _WARP_SIGNAL_STATE["in_warp"] = False
    except Exception:
        pass
    _sleep_ms(150)


def _warp_full(sm, b_obj, tid, stuff_type="item", distance=0.0, ipc=None):
    _ensure_warp_signal_tracker(sm)
    for _ in range(4):
        _try_slash(sm, "/heal", ipc=ipc)
        try:
            b_obj.CmdWarpToStuff(stuff_type, tid, float(distance))
        except Exception as exc:
            msg = str(exc)
            if "WarpDistanceTooShort" in msg or "WarpTooShort" in msg:
                break
            if "ShipInWarp" in msg or "ShipAlreadyInWarp" in msg:
                adv_ok = bool(_advance_clock(sm, ipc, 120, raise_on_error=False))
                _sleep_sim_ms(400)
                _wait_warp_fx_settled(sm, timeout_sec=(4.0 if adv_ok else 35.0), allow_time_control=bool(adv_ok), ipc=ipc)
                continue
            raise
        _sleep_ms(30)
        adv_ok = bool(_advance_clock(sm, ipc, 240, raise_on_error=False))
        _sleep_sim_ms(400)
        _wait_warp_fx_settled(sm, timeout_sec=(4.0 if adv_ok else 35.0), allow_time_control=bool(adv_ok), ipc=ipc)
        break
    _wait_view_settled(sm)


def _warp_animated(sm, b_obj, tid, stuff_type="item", distance=0.0, max_ticks=45, ipc=None):
    """Execute one stepped warp leg and assert client-side warp physics & tunnel FX activate."""
    _ensure_warp_signal_tracker(sm)
    start_before = int(_WARP_SIGNAL_STATE["started"])
    finish_before = int(_WARP_SIGNAL_STATE["finished"])
    sess = getattr(__builtin__, "session", None)
    sid = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
    michelle = sm.services.get("michelle") if hasattr(sm, "services") else None
    fx_svc = (
        (sm.services.get("FxSequencer") or sm.services.get("fxSequencer"))
        if hasattr(sm, "services")
        else None
    )

    for _ in range(4):
        _try_slash(sm, "/heal", ipc=ipc)
        try:
            b_obj.CmdWarpToStuff(stuff_type, tid, float(distance))
            break
        except Exception as exc:
            msg = str(exc)
            if "WarpDistanceTooShort" in msg or "WarpTooShort" in msg:
                return {"skipped": True}
            if "ShipInWarp" in msg or "ShipAlreadyInWarp" in msg:
                adv_ok = bool(_advance_clock(sm, ipc, 120, raise_on_error=False))
                _sleep_sim_ms(400)
                _wait_warp_fx_settled(sm, timeout_sec=(4.0 if adv_ok else 35.0), allow_time_control=bool(adv_ok), ipc=ipc)
                start_before = int(_WARP_SIGNAL_STATE["started"])
                finish_before = int(_WARP_SIGNAL_STATE["finished"])
                continue
            raise

    # 0.005 * 3.0 * AU_METERS (~2.24e9 m/s): threshold in client Warp.WarpLoop for warp tunnel/explosion.
    tunnel_speed_threshold = 0.005 * 3.0 * 149597870700.0
    saw_warp_mode = False
    saw_active_stamp = False
    saw_tunnel_gfx = False
    saw_explosion = False
    max_speed = 0.0

    for _ in range(int(max_ticks)):
        _sleep_sim_ms(1000)
        _advance_clock(sm, ipc, 1, raise_on_error=False)
        bp = michelle.GetBallpark() if (sid and michelle is not None) else None
        ball = bp.GetBall(sid) if (bp is not None and sid) else None
        if ball is not None:
            mode = int(getattr(ball, "mode", 0) or 0)
            est = int(getattr(ball, "effectStamp", -1) or -1)
            spd = (
                float(getattr(ball, "vx", 0.0) or 0.0) ** 2
                + float(getattr(ball, "vy", 0.0) or 0.0) ** 2
                + float(getattr(ball, "vz", 0.0) or 0.0) ** 2
            ) ** 0.5
            max_speed = max(max_speed, spd)
            if mode == 3:
                saw_warp_mode = True
                if est >= 0:
                    saw_active_stamp = True
            if fx_svc is not None and sid:
                fx_effects = []
                if hasattr(fx_svc, "GetAllBallActivations"):
                    for act in (fx_svc.GetAllBallActivations(sid) or ()):
                        eff = getattr(act, "effect", None)
                        fx_effects.append(eff if eff is not None else act)
                elif hasattr(fx_svc, "balls"):
                    fx_effects.extend((fx_svc.balls.get(sid) or {}).values())
                for eff in fx_effects:
                    if getattr(eff, "hasExploded", False):
                        saw_explosion = True
                    gfx = getattr(eff, "gfx", None)
                    if gfx is not None and getattr(gfx, "display", False):
                        saw_tunnel_gfx = True
            if saw_active_stamp and max_speed >= tunnel_speed_threshold and (saw_tunnel_gfx or saw_explosion or mode != 3):
                break
            if saw_active_stamp and mode != 3:
                break

    adv_finish_ok = bool(_advance_clock(sm, ipc, 120, raise_on_error=False))
    _sleep_sim_ms(400)
    _wait_warp_fx_settled(sm, timeout_sec=(8.0 if adv_finish_ok else 35.0), min_finished=finish_before + 1, allow_time_control=bool(adv_finish_ok), ipc=ipc)
    _wait_view_settled(sm)

    started_delta = int(_WARP_SIGNAL_STATE["started"]) - start_before
    finished_delta = int(_WARP_SIGNAL_STATE["finished"]) - finish_before
    if not (
        saw_warp_mode
        and saw_active_stamp
        and max_speed >= tunnel_speed_threshold
        and (saw_tunnel_gfx or saw_explosion)
        and (not _WARP_SIGNAL_STATE["hooked"] or (started_delta >= 1 and finished_delta >= 1))
    ):
        raise AssertionError(
            "Client warp animation/physics check failed (#531): "
            "warp_mode=%r active_stamp=%r max_speed=%.3e threshold=%.3e "
            "tunnel_gfx=%r exploded=%r signals=(+%d/+%d)"
            % (
                saw_warp_mode,
                saw_active_stamp,
                max_speed,
                tunnel_speed_threshold,
                saw_tunnel_gfx,
                saw_explosion,
                started_delta,
                finished_delta,
            )
        )
    return {
        "max_speed": max_speed,
        "saw_tunnel_gfx": saw_tunnel_gfx,
        "saw_explosion": saw_explosion,
        "started_delta": started_delta,
        "finished_delta": finished_delta,
    }


def _undock_ship(sm, allow_time_control=True, ipc=None):
    _wait_view_settled(sm)
    sess = getattr(__builtin__, "session", None)
    if (
        getattr(sess, "solarsystemid", None)
        and not getattr(sess, "stationid", None)
        and not getattr(sess, "structureid", None)
    ):
        _ensure_view(sm, "inflight")
        _ensure_warp_signal_tracker(sm)
        return
    clear_ok = bool(
        _clear_session_timer(
            sm,
            ipc=ipc,
            allow_time_control=allow_time_control,
            raise_on_error=False,
        )
    )
    if not clear_ok:
        _wait_session_timer_clear(max_wait_sec=12.0)
    _sleep_ms(600)
    sm.RemoteSvc("ship").Undock()
    undocked_ok = False
    deadline = time.time() + (10.0 if (allow_time_control and clear_ok) else 16.0)
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if (
            getattr(sess, "solarsystemid", None)
            and not getattr(sess, "stationid", None)
            and not getattr(sess, "structureid", None)
            and not getattr(sess, "mutating", 0)
            and getattr(sess, "changing", None) is None
        ):
            undocked_ok = True
            break
        _sleep_ms(100)
    if not undocked_ok:
        sess = getattr(__builtin__, "session", None)
        raise RuntimeError(
            "Timed out waiting for undock (solarsystemid=%r, stationid=%r, structureid=%r)"
            % (
                getattr(sess, "solarsystemid", None) if sess is not None else None,
                getattr(sess, "stationid", None) if sess is not None else None,
                getattr(sess, "structureid", None) if sess is not None else None,
            )
        )
    if allow_time_control:
        _clear_session_timer(sm, ipc=ipc, raise_on_error=False)
    _ensure_view(sm, "inflight")
    _ensure_warp_signal_tracker(sm)


def _dock_station(sm, station_id=60003760, allow_time_control=True, ipc=None):
    sess = getattr(__builtin__, "session", None)
    sid = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
    michelle = sm.services.get("michelle") if hasattr(sm, "services") else None
    bp = michelle.GetBallpark() if (sid and michelle is not None) else None
    ball = bp.GetBall(sid) if (bp is not None and sid) else None
    if ball is not None and int(getattr(ball, "mode", 0) or 0) == 3:
        warp_adv_ok = bool(
            _advance_clock(sm, ipc, 120, allow_time_control=allow_time_control, raise_on_error=False)
        )
        _wait_warp_fx_settled(
            sm,
            timeout_sec=(6.0 if warp_adv_ok else 35.0),
            allow_time_control=bool(warp_adv_ok),
            ipc=ipc,
        )
    _wait_view_settled(sm)
    clear_ok = bool(
        _clear_session_timer(
            sm,
            ipc=ipc,
            allow_time_control=allow_time_control,
            raise_on_error=False,
        )
    )
    if not clear_ok:
        _wait_session_timer_clear(max_wait_sec=12.0)
    sm.RemoteSvc("stationSvc").Dock(station_id)
    docked_ok = False
    deadline = time.time() + (10.0 if (allow_time_control and clear_ok) else 16.0)
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if (
            (getattr(sess, "stationid", None) == station_id or getattr(sess, "structureid", None) == station_id)
            and not getattr(sess, "mutating", 0)
            and getattr(sess, "changing", None) is None
        ):
            docked_ok = True
            break
        _sleep_ms(100)
    if not docked_ok:
        sess = getattr(__builtin__, "session", None)
        raise RuntimeError(
            "Timed out waiting for dock at %r (stationid=%r, structureid=%r)"
            % (
                station_id,
                getattr(sess, "stationid", None) if sess is not None else None,
                getattr(sess, "structureid", None) if sess is not None else None,
            )
        )
    if allow_time_control:
        _clear_session_timer(sm, ipc=ipc, raise_on_error=False)
    _ensure_view(sm, "hangar")


def _advance_clock(sm, ipc, secs, target_char_id=0, allow_time_control=True, raise_on_error=True, source="inclient"):
    """Advances server simulation clock via the host-side GM bridge IPC (never played-client timeControl)."""
    if not allow_time_control:
        return False
    secs_i = int(secs)
    tgt = _resolve_target_char_id(target_char_id)
    res = _gm_ipc_request(
        {"cmd": "advance_clock", "secs": secs_i, "target_char_id": tgt},
        ipc=ipc,
    )
    if isinstance(res, dict) and res.get("ok"):
        _record_sim_time_injection(
            secs_i,
            target_char_id=tgt,
            via=res.get("via", "gm_bridge"),
            injection_id=res.get("injection_id"),
            source=source,
            method="AdvanceClock",
        )
        tval = res.get("time")
        return True if (tval is None or (isinstance(tval, (int, float)) and tval == 0)) else tval
    if raise_on_error:
        if res is None:
            raise RuntimeError("Host GM bridge unavailable for AdvanceClock(%d, target_char_id=%d)" % (secs_i, tgt))
        raise RuntimeError(
            "Host GM AdvanceClock(%d, target_char_id=%d) failed: %s"
            % (secs_i, tgt, (res or {}).get("error") or (res or {}).get("reason") or res)
        )
    return False


def _ensure_docked_jita(sm, ipc, station_id=60003760, system_id=30000142):
    target_station = int(station_id)
    target_system = int(system_id)
    _wait_view_settled(sm)
    sess = getattr(__builtin__, "session", None)
    cur_station = getattr(sess, "stationid", None)
    if cur_station == target_station:
        _ensure_view(sm, "hangar")
        return
    if cur_station is not None and cur_station != target_station:
        _undock_ship(sm, ipc=ipc)
    if getattr(sess, "structureid", None):
        if not _clear_session_timer(sm, ipc=ipc, raise_on_error=False):
            _wait_session_timer_clear(max_wait_sec=12.0)
        sm.RemoteSvc("structureDocking").Undock(int(getattr(sess, "structureid")))
        _wait_session_attr("structureid", None, timeout_sec=3.0, raise_on_timeout=True)
    sess = getattr(__builtin__, "session", None)
    if getattr(sess, "solarsystemid", None) and not getattr(sess, "stationid", None):
        _sleep_sim_ms(1200)
    _wait_warp_fx_settled(sm, timeout_sec=35.0, allow_time_control=False, ipc=ipc)
    sess = getattr(__builtin__, "session", None)
    cur_stop_sys = int(getattr(sess, "solarsystemid", 0) or getattr(sess, "solarsystemid2", 0) or 0) if sess is not None else 0
    if cur_stop_sys and not getattr(sess, "stationid", None) and not getattr(sess, "structureid", None):
        beyonce, _ = sm.RemoteSvc("beyonce").MachoBindObject(cur_stop_sys, None)
        beyonce.CmdStop()
    adv_ok = bool(_advance_clock(sm, ipc, 12, raise_on_error=False))
    _sleep_sim_ms(1200)
    _sleep_ms(1200 if adv_ok else 2000)
    _wait_warp_fx_settled(sm, timeout_sec=(5.0 if adv_ok else 35.0), allow_time_control=bool(adv_ok), ipc=ipc)
    try:
        sm.RemoteSvc("stationSvc").Dock(target_station)
    except Exception:
        _try_slash(sm, "/tr %d" % target_station, ipc=ipc, raise_on_error=True)
        _wait_session_attr("solarsystemid2", target_system, timeout_sec=3.0, raise_on_timeout=True)
        _wait_view_settled(sm)
        adv_ok = bool(_advance_clock(sm, ipc, 12, raise_on_error=False))
        sm.RemoteSvc("stationSvc").Dock(target_station)
    docked_ok = False
    deadline = time.time() + (10.0 if adv_ok else 16.0)
    while time.time() < deadline:
        sess = getattr(__builtin__, "session", None)
        if getattr(sess, "stationid", None) == target_station and getattr(sess, "changing", None) is None:
            docked_ok = True
            break
        _sleep_ms(100)
    if not docked_ok:
        sess = getattr(__builtin__, "session", None)
        raise RuntimeError(
            "Timed out waiting to dock at baseline station %d (got stationid=%r)"
            % (target_station, getattr(sess, "stationid", None) if sess is not None else None)
        )
    _clear_session_timer(sm, ipc=ipc, raise_on_error=False)
    _ensure_view(sm, "hangar")


def _leave_active_fleet(sm):
    fid = _live_session_attr("fleetid", None)
    if fid:
        sm.RemoteSvc("fleetMgr").LeaveFleet()
        _wait_session_attr("fleetid", None, timeout_sec=3.0, raise_on_timeout=True)
    fleet_svc = sm.GetService("fleet") if hasattr(sm, "GetService") else None
    if fleet_svc is not None and hasattr(fleet_svc, "fleet"):
        fleet_svc.fleet = None


def _cleanup_corp_structures(sm, ipc=None, structure_ids=None):
    """Unanchor and clean up active structures owned by the current player corp."""
    cur_corp = int(_live_session_attr("corpid", 0) or 0)
    if cur_corp < 98000000:
        return
    ids = []
    errors = []
    for sid in (structure_ids or ()):
        sid_i = int(sid) if sid else 0
        if sid_i and sid_i not in ids:
            ids.append(sid_i)
    try:
        corp_structs = sm.RemoteSvc("structureDirectory").GetMyCorporationStructures()
        if isinstance(corp_structs, dict):
            for k in corp_structs.keys():
                if int(k) not in ids:
                    ids.append(int(k))
        elif isinstance(corp_structs, (list, tuple)):
            for row in corp_structs:
                sid = int(_get_field(row, "structureID", _get_field(row, "itemID", 0)) or 0)
                if sid and sid not in ids:
                    ids.append(sid)
    except Exception as exc:
        errors.append("GetMyCorporationStructures: %s" % exc)
    for sid in ids:
        try:
            if _live_session_attr("structureid", None) == sid:
                if not _clear_session_timer(sm, ipc=ipc, raise_on_error=False):
                    _wait_session_timer_clear(max_wait_sec=12.0)
                sm.RemoteSvc("structureDocking").Undock(sid)
                _wait_session_attr("structureid", None, timeout_sec=3.0, raise_on_timeout=True)
            try:
                sm.RemoteSvc("structureDeployment").Unanchor(sid)
            except Exception:
                _try_slash(sm, "/structure state %d 110" % sid, ipc=ipc, raise_on_error=True)
                sm.RemoteSvc("structureDeployment").Unanchor(sid)
            _try_slash(sm, "/upwelltimer %d" % sid, ipc=ipc, raise_on_error=True)
            _advance_clock(sm, ipc, 5, raise_on_error=False)
        except Exception as exc:
            errors.append("structure %d cleanup: %s" % (sid, exc))
    if ids:
        try:
            rem_structs = sm.RemoteSvc("structureDirectory").GetMyCorporationStructures()
            rem_ids = []
            if isinstance(rem_structs, dict):
                rem_ids = [int(k) for k in rem_structs.keys()]
            elif isinstance(rem_structs, (list, tuple)):
                rem_ids = [
                    int(_get_field(row, "structureID", _get_field(row, "itemID", 0)) or 0)
                    for row in rem_structs
                ]
                rem_ids = [x for x in rem_ids if x]
            if rem_ids:
                errors.append("remaining active corp structures after cleanup: %r" % (rem_ids,))
        except Exception as exc:
            errors.append("GetMyCorporationStructures verify: %s" % exc)
    if errors:
        raise RuntimeError("Corp structure cleanup failed: %s" % "; ".join(errors))


def _resign_player_corp(sm, baseline_corp_id=0, ipc=None, structure_ids=None):
    cur_corp = int(_live_session_attr("corpid", 0) or 0)
    if cur_corp >= 98000000 and (not baseline_corp_id or cur_corp != int(baseline_corp_id)):
        _cleanup_corp_structures(sm, ipc=ipc, structure_ids=structure_ids)
        if not _clear_session_timer(sm, ipc=ipc, raise_on_error=False):
            _wait_session_timer_clear(max_wait_sec=12.0)
        _advance_clock(sm, ipc, 12, raise_on_error=False)
        sm.RemoteSvc("corpRegistry").ResignFromCorp()
        resigned = False
        now_corp = cur_corp
        deadline = time.time() + 3.0
        while time.time() < deadline:
            now_corp = int(_live_session_attr("corpid", 0) or 0)
            if 0 < now_corp < 98000000:
                resigned = True
                break
            _sleep_ms(50)
        if not resigned:
            raise RuntimeError(
                "Timed out waiting for corp resignation (corpid=%r)" % (now_corp,)
            )


def _ensure_home_ship(sm, target_ship_id, ipc=None):
    if not target_ship_id:
        return int(_live_session_attr("shipid", 0) or 0)
    target_sid = int(target_ship_id)
    cur_sid = int(_live_session_attr("shipid", 0) or 0)
    if cur_sid == target_sid:
        return target_sid
    if not _clear_session_timer(sm, ipc=ipc, raise_on_error=False):
        _wait_session_timer_clear(max_wait_sec=12.0)
    try:
        sm.RemoteSvc("ship").Board(target_sid)
    except Exception:
        _advance_clock(sm, ipc, 12, raise_on_error=False)
        sm.RemoteSvc("ship").AssembleShip([target_sid])
        _clear_session_timer(sm, ipc=ipc, raise_on_error=False)
        sm.RemoteSvc("ship").Board(target_sid)
    _wait_session_attr("shipid", target_sid, timeout_sec=3.0, raise_on_timeout=True)
    _sleep_ms(100)
    _wait_view_settled(sm)
    _clear_session_timer(sm, ipc=ipc, raise_on_error=False)
    return int(_live_session_attr("shipid", target_sid) or target_sid)


def _reset_baseline_state(sm, ipc, baseline, char_id=None):
    """Restore baseline character, station, ship fit, NPC corp, FW enlistment, targets, and fleet."""
    target_char = int(char_id or baseline.get("char_id", 0) or 0)
    home_station = int(baseline.get("station_id", 60003760) or 60003760)
    home_system = int(baseline.get("system_id", 30000142) or 30000142)
    home_ship = int(baseline.get("ship_id", 0) or 0)
    home_corp = int(baseline.get("corp_id", 0) or 0)
    baseline_wf = baseline.get("warfaction_id", None)
    errors = []

    def _run_step(label, fn):
        try:
            fn()
        except Exception as exc:
            errors.append("%s: %s" % (label, exc))

    if target_char and int(_live_session_attr("charid", 0) or 0) != target_char:
        def _switch_char():
            sm.GetService("sessionMgr").PerformSessionChange(
                "charsel", sm.RemoteSvc("charUnboundMgr").SelectCharacterID, target_char
            )
            _wait_session_attr("charid", target_char, timeout_sec=3.0, raise_on_timeout=True)
            _ensure_view(sm, "hangar")

        _run_step("char_switch", _switch_char)

    def _reset_fleet():
        if _live_session_attr("fleetid", None):
            _leave_active_fleet(sm)
        else:
            fleet_svc = sm.GetService("fleet") if hasattr(sm, "GetService") else None
            if fleet_svc is not None and getattr(fleet_svc, "fleet", None) is not None:
                fleet_svc.fleet = None

    _run_step("leave_fleet", _reset_fleet)

    def _clear_targets():
        in_space = bool(_live_session_attr("solarsystemid", None) and not _live_session_attr("stationid", None))
        has_targets = False
        target_svc = None
        if hasattr(sm, "GetService"):
            target_svc = sm.GetService("target")
            if target_svc is not None and hasattr(target_svc, "GetTargets"):
                raw_targets = target_svc.GetTargets()
                if isinstance(raw_targets, (dict, list, tuple, set)) and raw_targets:
                    has_targets = True
        if in_space or has_targets:
            sm.RemoteSvc("dogmaIM").ClearTargets()
            if target_svc is not None and hasattr(target_svc, "ClearTargets"):
                target_svc.ClearTargets()

    _run_step("clear_targets", _clear_targets)

    cur_corp = int(_live_session_attr("corpid", 0) or 0)
    if cur_corp >= 98000000 and (not home_corp or cur_corp != home_corp):
        _run_step("cleanup_corp_structures", lambda: _cleanup_corp_structures(sm, ipc=ipc))

    cur_station = _live_session_attr("stationid", None)
    cur_struct = _live_session_attr("structureid", None)
    cur_sys = int(_live_session_attr("solarsystemid2", home_system) or home_system)
    if cur_station != home_station or cur_struct is not None or cur_sys != home_system:
        _run_step(
            "ensure_docked_jita",
            lambda: _ensure_docked_jita(sm, ipc, station_id=home_station, system_id=home_system),
        )

    if home_ship and int(_live_session_attr("shipid", 0) or 0) != home_ship:
        _run_step("ensure_home_ship", lambda: _ensure_home_ship(sm, home_ship, ipc=ipc))

    if home_ship and baseline.get("fitted_item_ids") is not None:
        _run_step(
            "strip_non_baseline_fitting",
            lambda: _strip_extra_ship_fittings(
                sm, home_station, home_ship, baseline.get("fitted_item_ids")
            ),
        )

    cur_corp = int(_live_session_attr("corpid", 0) or 0)
    if cur_corp >= 98000000 and (not home_corp or cur_corp != home_corp):
        _run_step("resign_player_corp", lambda: _resign_player_corp(sm, home_corp, ipc=ipc))

    cur_wf = _live_session_attr("warfactionid", None)
    if cur_wf and cur_wf != baseline_wf:
        def _leave_fw():
            sm.RemoteSvc("fwCharacterEnlistmentMgr").RemoveMyDirectEnlistment()
            _wait_session_attr("warfactionid", baseline_wf, timeout_sec=3.0, raise_on_timeout=True)

        _run_step("leave_faction_warfare", _leave_fw)

    if target_char:
        _run_step("sync_quasar_gateway", lambda: _sync_quasar_gateway(sm, target_char, raise_on_error=True))

    if errors:
        raise RuntimeError("Baseline state reset failed: %s" % "; ".join(errors))


class _RecordedBoundProxy(object):
    def __init__(self, svc_name, target, active_sink):
        self.__dict__["_svc_name"] = svc_name
        self.__dict__["_target"] = target
        self.__dict__["_active_sink"] = active_sink

    def __setattr__(self, name, value):
        return setattr(self._target, name, value)

    def __getattr__(self, method_name):
        attr = getattr(self._target, method_name)
        if str(method_name).startswith("_") or not callable(attr):
            return attr
        svc_name = self._svc_name
        active_sink = self._active_sink

        def _wrapped(*args, **kwargs):
            status = "ok"
            try:
                res = attr(*args, **kwargs)
                if (
                    method_name == "MachoBindObject"
                    and isinstance(res, tuple)
                    and len(res) >= 1
                    and res[0] is not None
                    and not isinstance(res[0], (_INT_TYPES, _STR_TYPES, float, bool, list, dict, tuple))
                ):
                    bound = _RecordedBoundProxy("%s.bound" % svc_name, res[0], active_sink)
                    res = (bound,) + res[1:]
                return res
            except Exception:
                status = "error"
                raise
            finally:
                if (
                    isinstance(active_sink, list)
                    and len(active_sink) == 1
                    and (active_sink[0] is None or isinstance(active_sink[0], list))
                ):
                    sink = active_sink[0]
                else:
                    sink = active_sink
                if sink is not None:
                    try:
                        sink.append({"service": svc_name, "method": method_name, "status": status})
                    except Exception:
                        pass

        return _wrapped


def _install_rpc_recorder(sm):
    if getattr(sm, "_evomu_rpc_recorder_installed", False):
        return getattr(sm, "_evomu_rpc_sink", [None])
    orig_remote = getattr(sm, "RemoteSvc", None)
    if not callable(orig_remote):
        return [None]
    active_sink = [None]

    def _wrapped_remote_svc(service_name, *args, **kwargs):
        svc = orig_remote(service_name, *args, **kwargs)
        if svc is None:
            return None
        return _RecordedBoundProxy(str(service_name), svc, active_sink)

    sm.RemoteSvc = _wrapped_remote_svc
    sm._evomu_rpc_recorder_installed = True
    sm._evomu_rpc_sink = active_sink
    return active_sink


def _capture_inclient_screenshot(out_dir, jid, ipc=None):
    if not out_dir:
        return None
    shots_dir = os.path.join(out_dir, "screenshots")
    try:
        if not os.path.isdir(shots_dir):
            os.makedirs(shots_dir)
    except Exception:
        return None
    shot_path = os.path.join(shots_dir, "%s.png" % jid)
    try:
        import trinity
        dev = getattr(trinity, "device", None)
        if dev is not None and hasattr(dev, "GetRenderTargets") and hasattr(trinity, "Tr2HostBitmap"):
            rts = dev.GetRenderTargets()
            rt = rts[0] if rts else None
            if rt is not None:
                bmp = trinity.Tr2HostBitmap()
                bmp.CreateFromRenderTarget(rt)
                bmp.Save(shot_path)
                if os.path.exists(shot_path) and os.path.getsize(shot_path) > 0:
                    return shot_path
    except Exception:
        pass
    if ipc is not None:
        try:
            resp = ipc.request({"cmd": "capture_screenshot", "path": shot_path, "journey_id": jid})
            if isinstance(resp, dict) and resp.get("ok") and resp.get("path"):
                return resp.get("path")
        except Exception:
            pass
    return None
