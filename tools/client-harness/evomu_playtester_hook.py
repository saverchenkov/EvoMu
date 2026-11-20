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
import re
import socket
import sys
import threading
import time
import traceback
import weakref

import evomu_inclient_helpers as H

sys.dont_write_bytecode = True

_STARTED = False
_FLIGHT_RECORDER = []
_FLIGHT_SEQ = 0
_CONSUMED_FLIGHT_ERR_SEQ = 0
_LAST_OBSERVED_SEQ = 0
_RECENT_MODALS = []
_RECENT_SCATTERS = []
_SCATTER_SEQ = 0
_HOOKED_REMOTESVC = False
_HOOK_RELOAD_GEN = 0
_MACHO_CALL_TASKLET_DEPTH = {}
_QUASAR_IN_FLIGHT = {}
_QUASAR_IN_FLIGHT_SEQ = 0
_PY_INT_TYPES = (int, type(1 << 65))
_LAST_SNAPSHOT = None
_LAST_SNAPSHOT_SM = None
_LAST_SNAPSHOT_TS = 0.0


def _unwrap_pt_wrapper(fn):
    cur = fn
    for _ in range(8):
        if cur is None or getattr(cur, "_evomu_pt_wrapped", False) is not True:
            break
        nxt = getattr(cur, "_evomu_pt_orig", None)
        if nxt is None:
            raw_fn = getattr(cur, "__func__", getattr(cur, "im_func", cur))
            code = getattr(raw_fn, "__code__", getattr(raw_fn, "func_code", None))
            clos = getattr(raw_fn, "__closure__", getattr(raw_fn, "func_closure", None))
            if code is not None and clos is not None:
                for name, cell in zip(getattr(code, "co_freevars", ()), clos):
                    if name.startswith("orig_") or name.startswith("raw_orig_"):
                        try:
                            nxt = cell.cell_contents
                            break
                        except Exception:
                            pass
        if nxt is None or nxt is cur:
            break
        cur = nxt
    return cur


def _needs_pt_wrap(fn):
    if fn is None:
        return False
    if getattr(fn, "_evomu_pt_wrapped", False) is not True:
        return True
    return getattr(fn, "_evomu_pt_gen", None) != _HOOK_RELOAD_GEN


def _get_macho_tasklet_id():
    sl = sys.modules.get("stackless")
    if sl is None:
        try:
            import stackless as sl
        except Exception:
            return 0
    try:
        return id(sl.getcurrent())
    except Exception:
        return 0

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
        "kind": _safe_str(kind, 32),
        "service": _safe_str(service, 64),
        "method": _safe_str(method, 64),
        "args": _utf8_trunc(args_summary, 160),
        "status": _safe_str(status, 32),
        "duration_ms": round(float(duration_ms), 2),
        "error": _utf8_trunc(error, 240) if error else "",
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


def _get_message_owner():
    eve_obj = getattr(__builtin__, "eve", None)
    if eve_obj is not None and hasattr(eve_obj, "Message"):
        _wrap_message_owner(eve_obj)
        return eve_obj
    uc = _get_uicore()
    if uc is not None and hasattr(uc, "Message"):
        _wrap_message_owner(uc)
        return uc
    return None


def _format_user_error(exc):
    msg_key = getattr(exc, "msg", "") or ""
    msg_dict = getattr(exc, "dict", None)
    if isinstance(msg_dict, dict) and msg_dict.get("info"):
        label = "%s: %s" % (msg_key or "CustomInfo", msg_dict.get("info"))
    elif msg_key:
        label = "%s" % (msg_key,)
    else:
        label = str(exc)
    return "UserError(%s)" % (_safe_str(label, 120),)


def _notify_user_error(exc, async_uthread=True):
    owner = _get_message_owner()
    if owner is None:
        return False
    msg_key = getattr(exc, "msg", "") or "CustomInfo"
    msg_dict = getattr(exc, "dict", None)
    if async_uthread:
        try:
            import uthread as _ut_msg
            _ut_msg.new(owner.Message, msg_key, msg_dict)
            return True
        except Exception:
            pass
    try:
        owner.Message(msg_key, msg_dict)
        return True
    except Exception:
        return False


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


def _record_scatter_event(event_name, args):
    global _SCATTER_SEQ
    try:
        ename = str(event_name)
        if ename.startswith("On"):
            now_ts = round(time.time(), 3)
            args_s = repr(args)[:140]
            if _RECENT_SCATTERS:
                last = _RECENT_SCATTERS[-1]
                if last.get("event") == ename and last.get("args") == args_s and abs(now_ts - float(last.get("ts", 0.0))) < 0.005:
                    return
            _SCATTER_SEQ += 1
            _RECENT_SCATTERS.append({
                "seq": _SCATTER_SEQ,
                "ts": now_ts,
                "event": ename,
                "args": args_s,
            })
            if len(_RECENT_SCATTERS) > 120:
                del _RECENT_SCATTERS[:-90]
    except Exception:
        pass


def _is_plain_int(val):
    return isinstance(val, _PY_INT_TYPES) and not isinstance(val, bool)


def _cleanup_quasar_in_flight(now=None):
    if not _QUASAR_IN_FLIGHT:
        return 0
    cur_t = time.time() if now is None else float(now)
    stale = []
    for qid, info in list(_QUASAR_IN_FLIGHT.items()):
        if not isinstance(info, dict):
            stale.append(qid)
            continue
        deadline = float(info.get("deadline", 0.0) or 0.0)
        if deadline > 0.0 and cur_t >= deadline:
            stale.append(qid)
            continue
        chan_ref = info.get("chan_ref")
        if chan_ref is not None:
            try:
                chan = chan_ref()
            except Exception:
                chan = None
            if chan is None or getattr(chan, "_open", True) is False:
                stale.append(qid)
    for qid in stale:
        _QUASAR_IN_FLIGHT.pop(qid, None)
    return len(_QUASAR_IN_FLIGHT)


def _has_in_flight_quasar(sm, now=None):
    if _cleanup_quasar_in_flight(now) > 0:
        return True
    try:
        sm_svcs = getattr(sm, "services", None) if sm is not None else None
        pgw = sm_svcs.get("publicGatewaySvc") if isinstance(sm_svcs, dict) else None
        gw = getattr(pgw, "publicGateway", None) if pgw is not None else None
        if gw is not None and hasattr(gw, "get_request_broker_details"):
            bd = gw.get_request_broker_details()
            if isinstance(bd, dict):
                pr = bd.get("pending_requests", 0)
                ps = bd.get("pending_responses", 0)
                if (_is_plain_int(pr) and pr > 0) or (_is_plain_int(ps) and ps > 0):
                    return True
    except Exception:
        pass
    return False


def _has_in_flight_rpc(sm):
    if sum(_MACHO_CALL_TASKLET_DEPTH.values()) > 0:
        return True
    return _has_in_flight_quasar(sm)


def _alloc_quasar_flight_token(req_name, start_ts=None, timeout_sec=15.0, resp_chan=None):
    global _QUASAR_IN_FLIGHT_SEQ
    _QUASAR_IN_FLIGHT_SEQ += 1
    qid = _QUASAR_IN_FLIGHT_SEQ
    t0 = time.time() if start_ts is None else float(start_ts)
    try:
        tmo = float(timeout_sec) if timeout_sec is not None else 15.0
    except Exception:
        tmo = 15.0
    deadline = t0 + min(max(tmo, 1.0), 15.0)
    chan_ref = None
    if resp_chan is not None:
        try:
            chan_ref = weakref.ref(resp_chan)
        except TypeError:
            chan_ref = None
    _QUASAR_IN_FLIGHT[qid] = {
        "ts": t0,
        "deadline": deadline,
        "chan_ref": chan_ref,
        "method": req_name,
    }
    return qid, t0


def _format_quasar_error(exc):
    resp_prim = getattr(exc, "response_primitive", None)
    status_code = getattr(resp_prim, "status_code", None)
    if _is_plain_int(status_code):
        return "status=%d %r" % (status_code, exc)
    return repr(exc)


def _wrap_quasar_response_channel(resp_chan, req_name, mname, start_ts, timeout_sec):
    orig_recv = getattr(resp_chan, "receive", None)
    if orig_recv is None or not callable(orig_recv):
        return False
    if getattr(orig_recv, "_evomu_pt_wrapped", False):
        return True
    qid, t0 = _alloc_quasar_flight_token(
        req_name,
        start_ts=start_ts,
        timeout_sec=timeout_sec,
        resp_chan=resp_chan,
    )

    def _wrapped_receive(*r_args, **r_kwargs):
        try:
            recv_res = orig_recv(*r_args, **r_kwargs)
            dur_ms = (time.time() - t0) * 1000.0
            _record_flight("quasar", "publicGateway", _safe_str(req_name, 64), mname, "OK", dur_ms)
            return recv_res
        except Exception as exc:
            dur_ms = (time.time() - t0) * 1000.0
            _record_flight(
                "quasar",
                "publicGateway",
                _safe_str(req_name, 64),
                mname,
                "ERR",
                dur_ms,
                _format_quasar_error(exc),
            )
            raise
        finally:
            _QUASAR_IN_FLIGHT.pop(qid, None)

    _wrapped_receive._evomu_pt_wrapped = True
    _wrapped_receive._evomu_pt_orig = orig_recv
    _wrapped_receive._evomu_pt_gen = _HOOK_RELOAD_GEN
    try:
        resp_chan.receive = _wrapped_receive
        return True
    except Exception:
        _QUASAR_IN_FLIGHT.pop(qid, None)
        return False


def _install_flight_recorder_hooks(sm):
    global _HOOKED_REMOTESVC
    _disable_background_focus_deferral()
    try:
        _wrap_message_owner(_get_uicore())
        _wrap_message_owner(getattr(__builtin__, "eve", None))
    except Exception:
        pass

    if sm is not None:
        try:
            orig_scatter_no_stars = getattr(sm, "ScatterEventWithoutTheStars", None)
            if orig_scatter_no_stars is not None and not getattr(orig_scatter_no_stars, "_evomu_pt_wrapped", False):
                def _wrapped_scatter_no_stars(event_name, args, keywords=None):
                    _record_scatter_event(event_name, args)
                    return orig_scatter_no_stars(event_name, args, keywords)

                _wrapped_scatter_no_stars._evomu_pt_wrapped = True
                sm.ScatterEventWithoutTheStars = _wrapped_scatter_no_stars
        except Exception:
            pass

    try:
        from eve.client.script.ui.inflight.bracketsAndTargets.bracket_attachment_mixin import (
            BracketAttachmentMixin as _bam_cls,
            ReverseScaleDpi as _bam_rev_dpi,
            ScaleDpi as _bam_scale_dpi,
        )
        if _bam_cls is not None and not getattr(_bam_cls, "_evomu_pt_offset_guarded_v2", False):
            def _get_x_offset(self_b):
                b = getattr(self_b, "_displayX_binding", None)
                if b is None or not getattr(b, "offset", None):
                    return 0
                return _bam_rev_dpi(b.offset[0])

            def _set_x_offset(self_b, val):
                b = getattr(self_b, "_displayX_binding", None)
                if b is not None:
                    b.offset = (_bam_scale_dpi(val), 0, 0, 0)

            def _get_y_offset(self_b):
                b = getattr(self_b, "_displayY_binding", None)
                if b is None or not getattr(b, "offset", None):
                    return 0
                return _bam_rev_dpi(b.offset[0])

            def _set_y_offset(self_b, val):
                b = getattr(self_b, "_displayY_binding", None)
                if b is not None:
                    b.offset = (_bam_scale_dpi(val), 0, 0, 0)

            _bam_cls.x_offset = property(_get_x_offset, _set_x_offset)
            _bam_cls.y_offset = property(_get_y_offset, _set_y_offset)
            _bam_cls._evomu_pt_offset_guarded_v2 = True
    except Exception:
        pass

    try:
        sm_svcs = getattr(sm, "services", None) if sm is not None else None
        pgw_svc = sm_svcs.get("publicGatewaySvc") if isinstance(sm_svcs, dict) else None
        if pgw_svc is not None:
            _pg_methods = (
                "send_request",
                "send_character_request",
                "send_user_request",
                "send_blocking_character_request_and_receive_response",
                "send_blocking_user_request_and_receive_response",
            )
            for pg_mname in _pg_methods:
                cur_pg_fn = getattr(pgw_svc, pg_mname, None)
                if cur_pg_fn is not None and _needs_pt_wrap(cur_pg_fn):
                    orig_pg_fn = _unwrap_pt_wrapper(cur_pg_fn)

                    def _make_pg_wrapper(orig_fn, mname):
                        is_blocking = mname.startswith("send_blocking_")

                        def _wrapped_pg(*args, **kwargs):
                            req_obj = (
                                args[0]
                                if args
                                else (kwargs.get("request") or kwargs.get("request_payload"))
                            )
                            r_name = type(req_obj).__name__ if req_obj is not None else mname
                            timeout_sec = kwargs.get("timeout_seconds")
                            if timeout_sec is None and len(args) >= 3:
                                timeout_sec = args[2]
                            t0 = time.time()
                            qid = None
                            if is_blocking:
                                qid, t0 = _alloc_quasar_flight_token(
                                    r_name,
                                    start_ts=t0,
                                    timeout_sec=timeout_sec,
                                )
                            try:
                                res = orig_fn(*args, **kwargs)
                                if (
                                    not is_blocking
                                    and isinstance(res, tuple)
                                    and len(res) >= 2
                                    and getattr(res[1], "receive", None) is not None
                                    and _wrap_quasar_response_channel(
                                        res[1], r_name, mname, t0, timeout_sec
                                    )
                                ):
                                    return res
                                dur_ms = (time.time() - t0) * 1000.0
                                _record_flight(
                                    "quasar",
                                    "publicGateway",
                                    _safe_str(r_name, 64),
                                    mname,
                                    "OK",
                                    dur_ms,
                                )
                                return res
                            except Exception as exc:
                                dur_ms = (time.time() - t0) * 1000.0
                                _record_flight(
                                    "quasar",
                                    "publicGateway",
                                    _safe_str(r_name, 64),
                                    mname,
                                    "ERR",
                                    dur_ms,
                                    _format_quasar_error(exc),
                                )
                                raise
                            finally:
                                if qid is not None:
                                    _QUASAR_IN_FLIGHT.pop(qid, None)

                        _wrapped_pg._evomu_pt_wrapped = True
                        _wrapped_pg._evomu_pt_orig = orig_fn
                        _wrapped_pg._evomu_pt_gen = _HOOK_RELOAD_GEN
                        return _wrapped_pg

                    setattr(pgw_svc, pg_mname, _make_pg_wrapper(orig_pg_fn, pg_mname))
    except Exception:
        pass

    if _HOOKED_REMOTESVC or sm is None:
        return
    _HOOKED_REMOTESVC = True

    try:
        cur_remote = getattr(sm, "RemoteSvc", None)
        if _needs_pt_wrap(cur_remote):
            orig_remote = _unwrap_pt_wrapper(cur_remote)

            class _RemoteMethodProxy(object):
                def __init__(self, svc_name, method_name, fn):
                    self._svc = svc_name
                    self._meth = method_name
                    self._fn = fn

                def __call__(self, *args, **kwargs):
                    tid = _get_macho_tasklet_id()
                    _MACHO_CALL_TASKLET_DEPTH[tid] = _MACHO_CALL_TASKLET_DEPTH.get(tid, 0) + 1
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
                    finally:
                        rem = _MACHO_CALL_TASKLET_DEPTH.get(tid, 1) - 1
                        if rem <= 0:
                            _MACHO_CALL_TASKLET_DEPTH.pop(tid, None)
                        else:
                            _MACHO_CALL_TASKLET_DEPTH[tid] = rem

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
            _wrapped_remote_svc._evomu_pt_orig = orig_remote
            _wrapped_remote_svc._evomu_pt_gen = _HOOK_RELOAD_GEN
            sm.RemoteSvc = _wrapped_remote_svc

        cur_proxy = getattr(sm, "ProxySvc", None)
        if _needs_pt_wrap(cur_proxy):
            orig_proxy = _unwrap_pt_wrapper(cur_proxy)

            def _wrapped_proxy_svc(svc_name, *args, **kwargs):
                inner = orig_proxy(svc_name, *args, **kwargs)
                if inner is None:
                    return None
                return _RemoteSvcProxy(str(svc_name), inner)

            _wrapped_proxy_svc._evomu_pt_wrapped = True
            _wrapped_proxy_svc._evomu_pt_orig = orig_proxy
            _wrapped_proxy_svc._evomu_pt_gen = _HOOK_RELOAD_GEN
            sm.ProxySvc = _wrapped_proxy_svc
    except Exception:
        pass

    try:
        from carbon.common.script.net import moniker as _moniker_mod
        mon_cls = getattr(_moniker_mod, "Moniker", None)
        cur_mcall = getattr(mon_cls, "MonikeredCall", None) if mon_cls is not None else None

        if _needs_pt_wrap(cur_mcall):
            orig_mcall = _unwrap_pt_wrapper(cur_mcall)
            raw_orig_mcall = getattr(orig_mcall, "__func__", getattr(orig_mcall, "im_func", orig_mcall))

            def _wrapped_monikered_call(self_mon, call, *args, **kwargs):
                svc_n = str(getattr(self_mon, "_Moniker__serviceName", None) or "moniker")
                meth_n = str(call[0]) if isinstance(call, (tuple, list)) and len(call) > 0 else str(call)
                call_args = call[1] if isinstance(call, (tuple, list)) and len(call) > 1 else args
                tid = _get_macho_tasklet_id()
                _MACHO_CALL_TASKLET_DEPTH[tid] = _MACHO_CALL_TASKLET_DEPTH.get(tid, 0) + 1
                t0 = time.time()
                try:
                    res = raw_orig_mcall(self_mon, call, *args, **kwargs)
                    dur_ms = (time.time() - t0) * 1000.0
                    _record_flight("macho", svc_n, meth_n, repr(call_args)[:140], "OK", dur_ms)
                    return res
                except Exception as exc:
                    dur_ms = (time.time() - t0) * 1000.0
                    _record_flight("macho", svc_n, meth_n, repr(call_args)[:140], "ERR", dur_ms, repr(exc))
                    raise
                finally:
                    rem = _MACHO_CALL_TASKLET_DEPTH.get(tid, 1) - 1
                    if rem <= 0:
                        _MACHO_CALL_TASKLET_DEPTH.pop(tid, None)
                    else:
                        _MACHO_CALL_TASKLET_DEPTH[tid] = rem

            _wrapped_monikered_call._evomu_pt_wrapped = True
            _wrapped_monikered_call._evomu_pt_orig = orig_mcall
            _wrapped_monikered_call._evomu_pt_gen = _HOOK_RELOAD_GEN
            mon_cls.MonikeredCall = _wrapped_monikered_call

        from carbon.common.script.net import ObjectCallGPCS as _oc_mod
        oc_wrap_cls = getattr(_oc_mod, "MachoObjectCallWrapper", None)
        cur_oc_call = getattr(oc_wrap_cls, "__call__", None) if oc_wrap_cls is not None else None
        if _needs_pt_wrap(cur_oc_call):
            orig_oc_call = _unwrap_pt_wrapper(cur_oc_call)
            raw_orig_oc_call = getattr(orig_oc_call, "__func__", getattr(orig_oc_call, "im_func", orig_oc_call))

            def _wrapped_bound_object_call(self_wrap, *args, **kwargs):
                tid = _get_macho_tasklet_id()
                if _MACHO_CALL_TASKLET_DEPTH.get(tid, 0) > 0:
                    return raw_orig_oc_call(self_wrap, *args, **kwargs)
                pid = getattr(self_wrap, "persistantObjectID", None)
                oid = getattr(self_wrap, "objectID", None)
                svc_n = _safe_str(pid if pid is not None else (oid if oid is not None else "bound_object"), 48)
                meth_n = _safe_str(getattr(self_wrap, "method", "") or "", 48)
                _MACHO_CALL_TASKLET_DEPTH[tid] = _MACHO_CALL_TASKLET_DEPTH.get(tid, 0) + 1
                t0 = time.time()
                try:
                    res = raw_orig_oc_call(self_wrap, *args, **kwargs)
                    dur_ms = (time.time() - t0) * 1000.0
                    _record_flight("macho", svc_n, meth_n, repr(args)[:140], "OK", dur_ms)
                    return res
                except Exception as exc:
                    dur_ms = (time.time() - t0) * 1000.0
                    _record_flight("macho", svc_n, meth_n, repr(args)[:140], "ERR", dur_ms, repr(exc))
                    raise
                finally:
                    rem = _MACHO_CALL_TASKLET_DEPTH.get(tid, 1) - 1
                    if rem <= 0:
                        _MACHO_CALL_TASKLET_DEPTH.pop(tid, None)
                    else:
                        _MACHO_CALL_TASKLET_DEPTH[tid] = rem

            _wrapped_bound_object_call._evomu_pt_wrapped = True
            _wrapped_bound_object_call._evomu_pt_orig = orig_oc_call
            _wrapped_bound_object_call._evomu_pt_gen = _HOOK_RELOAD_GEN
            oc_wrap_cls.__call__ = _wrapped_bound_object_call
    except Exception:
        pass

    # Hook CoreServiceCall.RemoteServiceCallWithoutTheStars so client services that
    # cached sm.ProxySvc(...) / sm.RemoteSvc(...) during boot (before our sm wrapper
    # was installed, e.g. ContractsSvc.contractProxySvc) still record macho_calls
    # without double-recording calls already wrapped by _RemoteSvcProxy or Moniker.
    try:
        from carbon.common.script.net import ServiceCallGPCS as _sc_mod
        core_sc_cls = getattr(_sc_mod, "CoreServiceCall", None)
        cur_rsc_no_stars = getattr(core_sc_cls, "RemoteServiceCallWithoutTheStars", None) if core_sc_cls is not None else None
        if _needs_pt_wrap(cur_rsc_no_stars):
            orig_rsc_no_stars = _unwrap_pt_wrapper(cur_rsc_no_stars)
            raw_orig_rsc = getattr(orig_rsc_no_stars, "__func__", getattr(orig_rsc_no_stars, "im_func", orig_rsc_no_stars))

            def _wrapped_remote_service_call(self_sc, sess, nodeID, service, method, *rest, **kw):
                tid = _get_macho_tasklet_id()
                if _MACHO_CALL_TASKLET_DEPTH.get(tid, 0) > 0:
                    return raw_orig_rsc(self_sc, sess, nodeID, service, method, *rest, **kw)
                call_args = rest[0] if len(rest) > 0 else kw.get("args", ())
                svc_n = _safe_str(service, 48) or (str(service)[:48] if service is not None else "")
                meth_n = _safe_str(method, 48) or (str(method)[:48] if method is not None else "")
                _MACHO_CALL_TASKLET_DEPTH[tid] = _MACHO_CALL_TASKLET_DEPTH.get(tid, 0) + 1
                t0 = time.time()
                try:
                    res = raw_orig_rsc(self_sc, sess, nodeID, service, method, *rest, **kw)
                    dur_ms = (time.time() - t0) * 1000.0
                    _record_flight("macho", svc_n, meth_n, repr(call_args)[:140], "OK", dur_ms)
                    return res
                except Exception as exc:
                    dur_ms = (time.time() - t0) * 1000.0
                    _record_flight("macho", svc_n, meth_n, repr(call_args)[:140], "ERR", dur_ms, repr(exc))
                    raise
                finally:
                    rem = _MACHO_CALL_TASKLET_DEPTH.get(tid, 1) - 1
                    if rem <= 0:
                        _MACHO_CALL_TASKLET_DEPTH.pop(tid, None)
                    else:
                        _MACHO_CALL_TASKLET_DEPTH[tid] = rem

            _wrapped_remote_service_call._evomu_pt_wrapped = True
            _wrapped_remote_service_call._evomu_pt_orig = orig_rsc_no_stars
            _wrapped_remote_service_call._evomu_pt_gen = _HOOK_RELOAD_GEN
            core_sc_cls.RemoteServiceCallWithoutTheStars = _wrapped_remote_service_call
    except Exception:
        pass

    try:
        orig_scatter = getattr(sm, "ScatterEvent", None)
        if orig_scatter is not None and not getattr(orig_scatter, "_evomu_pt_wrapped", False):
            def _wrapped_scatter(event_name, *args, **kwargs):
                _record_scatter_event(event_name, args)
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


def _utf8_trunc(val, max_len=120):
    if val is None:
        return ""
    try:
        if sys.version_info[0] < 3:
            if isinstance(val, unicode):
                u = val
            elif isinstance(val, str):
                u = val.decode("utf-8", "replace")
            else:
                try:
                    u = unicode(val)
                except Exception:
                    u = str(val).decode("utf-8", "replace")
            if max_len is not None and max_len >= 0:
                u = u[:max_len]
            return u.encode("utf-8", "replace")
        u = str(val)
        if max_len is not None and max_len >= 0:
            return u[:max_len]
        return u
    except Exception:
        return ""


def _safe_str(val, max_len=120):
    if val is None:
        return ""
    try:
        if sys.version_info[0] < 3:
            if isinstance(val, unicode):
                u = val
            elif isinstance(val, str):
                u = val.decode("utf-8", "replace")
            else:
                try:
                    u = unicode(val)
                except Exception:
                    u = str(val).decode("utf-8", "replace")
            u = u" ".join(u.split())
            if max_len is not None and max_len >= 0:
                u = u[:max_len]
            return u.encode("utf-8", "replace")
        u = " ".join(str(val).split())
        if max_len is not None and max_len >= 0:
            return u[:max_len]
        return u
    except Exception:
        return ""


def _json_safe(obj):
    if sys.version_info[0] >= 3:
        return obj
    if obj is None or isinstance(obj, (bool, float, unicode)):
        return obj
    if isinstance(obj, (int, long)):
        return obj
    if isinstance(obj, str):
        return obj.decode("utf-8", "replace")
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            if isinstance(k, str):
                sk = k.decode("utf-8", "replace")
            elif isinstance(k, unicode):
                sk = k
            else:
                try:
                    sk = unicode(k)
                except Exception:
                    sk = str(k).decode("utf-8", "replace")
            out[sk] = _json_safe(v)
        return out
    if isinstance(obj, (list, tuple)):
        return [_json_safe(x) for x in obj]
    return obj


def _to_utf8(val):
    if val is None:
        return ""
    if sys.version_info[0] < 3:
        try:
            if isinstance(val, unicode):
                return val.encode("utf-8", "replace")
            return str(val)
        except Exception:
            return ""
    try:
        return str(val)
    except Exception:
        return ""


def _to_unicode(val):
    if val is None:
        return u""
    if sys.version_info[0] < 3:
        try:
            if isinstance(val, unicode):
                return val
            if isinstance(val, str):
                return val.decode("utf-8", "replace")
            return unicode(val)
        except Exception:
            try:
                return _to_utf8(val).decode("utf-8", "replace")
            except Exception:
                return u""
    try:
        return str(val)
    except Exception:
        return ""


def _widget_abs_rect(node):
    try:
        cls_n = node.__class__.__name__ if node is not None else ""
        if "TreeViewEntry" in cls_n:
            hc = getattr(node, "topRightCont", None) or getattr(node, "headerCont", None)
            if hc is not None and not getattr(hc, "destroyed", False) and hasattr(hc, "GetAbsolute"):
                res_hc = hc.GetAbsolute()
                if isinstance(res_hc, (tuple, list)) and len(res_hc) == 4 and int(res_hc[2]) > 0 and int(res_hc[3]) > 0:
                    return int(res_hc[0]), int(res_hc[1]), int(res_hc[2]), int(res_hc[3])
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


def _widget_clipped_rect(node):
    rx, ry, rw, rh = _widget_abs_rect(node)
    if rw <= 0 or rh <= 0 or node is None:
        return rx, ry, rw, rh
    x0, y0, x1, y1 = rx, ry, rx + rw, ry + rh
    seen = set([id(node)])
    cur = getattr(node, "parent", None)
    for _ in range(24):
        if cur is None or id(cur) in seen:
            break
        seen.add(id(cur))
        cname = getattr(cur, "name", "")
        if cname == "desktop" or cname in _LAYER_SCAN_ORDER:
            break
        clip_flag = getattr(cur, "clipChildren", False)
        if isinstance(clip_flag, (bool, int)) and clip_flag:
            has_explicit_rect = (
                hasattr(cur, "GetAbsolute")
                or hasattr(cur, "width")
                or hasattr(cur, "displayWidth")
            )
            if has_explicit_rect:
                ax, ay, aw, ah = _widget_abs_rect(cur)
                if aw <= 0 or ah <= 0:
                    return rx, ry, 0, 0
                x0 = max(x0, ax)
                y0 = max(y0, ay)
                x1 = min(x1, ax + aw)
                y1 = min(y1, ay + ah)
                if x1 <= x0 or y1 <= y0:
                    return rx, ry, 0, 0
        cur = getattr(cur, "parent", None)
    return int(x0), int(y0), int(x1 - x0), int(y1 - y0)


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


def _is_under_widget(node, ancestor, max_depth=24):
    cur = node
    for _ in range(max_depth):
        if cur is None:
            return False
        if cur is ancestor:
            return True
        cur = getattr(cur, "parent", None)
    return False


_INERT_TOOLTIP_CLICK_BASE_CLASSES = (
    "ButtonIcon",
    "Container",
    "ContainerAutoSize",
    "LayoutGrid",
    "Base",
    "UIBase",
    "FrameWithPointer",
    "object",
)


def _is_tooltip_panel_trigger(node):
    if node is None:
        return False
    sr_node = getattr(getattr(node, "sr", None), "node", None)
    has_loader = any(
        callable(getattr(node, attr, None))
        for attr in ("LoadTooltipPanel", "ConstructTooltipPanel", "GetTooltipPanelClassInfo")
    ) or (sr_node is not None and callable(getattr(sr_node, "LoadTooltipPanel", None)))
    if not has_loader:
        return False
    if callable(getattr(node, "func", None)) or callable(getattr(node, "cmd", None)):
        return False
    inst_dict = getattr(node, "__dict__", None) or {}
    for hname in ("OnClick", "Click", "OnMouseDown"):
        hv = inst_dict.get(hname)
        if callable(hv) or (isinstance(hv, tuple) and len(hv) >= 1 and callable(hv[0])):
            return False
    for hname in ("Click", "OnClick", "OnMouseDown"):
        defining_cls = None
        for cls in getattr(type(node), "__mro__", ()):
            if hname in getattr(cls, "__dict__", {}):
                defining_cls = cls.__name__
                break
        if defining_cls is not None and defining_cls not in _INERT_TOOLTIP_CLICK_BASE_CLASSES:
            return False
    return True


def _is_tooltip_owner_alive(tp):
    owner = getattr(tp, "owner", None)
    if owner is None or getattr(owner, "destroyed", False):
        return False
    uic = _get_uicore()
    desktop = getattr(uic, "desktop", None) if uic is not None else None
    cur = owner
    root = None
    for _ in range(48):
        if cur is None:
            break
        if not _is_visible(cur):
            return False
        root = cur
        cur = getattr(cur, "parent", None)
    if desktop is not None and root is not desktop:
        return False
    return True


def _close_pinned_tooltip_panel(tp, th=None):
    if tp is None:
        return
    try:
        tp._evomu_pinned = False
    except Exception:
        pass
    try:
        if not getattr(tp, "destroyed", False) and callable(getattr(tp, "Close", None)):
            tp.Close()
    except Exception:
        pass
    if th is None:
        uic = _get_uicore()
        uilib = getattr(uic, "uilib", None) if uic is not None else None
        th = getattr(uilib, "tooltipHandler", None) if uilib is not None else None
    try:
        if th is not None and getattr(th, "tooltipPanel", None) is tp:
            th.tooltipPanel = None
    except Exception:
        pass


def _dismiss_pinned_tooltip_if_outside(node, close_if_owner=False):
    uic = _get_uicore()
    if uic is None:
        return False
    uilib = getattr(uic, "uilib", None)
    th = getattr(uilib, "tooltipHandler", None) if uilib is not None else None
    candidates = []
    seen_ids = set()
    if th is not None:
        tp = getattr(th, "tooltipPanel", None)
        if tp is not None:
            candidates.append(tp)
            seen_ids.add(id(tp))
    try:
        layer = getattr(uic, "layer", None)
        menu_layer = getattr(layer, "menu", None) if layer is not None else None
        for ch in list(getattr(menu_layer, "children", []) or []):
            if id(ch) in seen_ids:
                continue
            if getattr(ch, "_evomu_pinned", False) or "TooltipPanel" in getattr(getattr(ch, "__class__", None), "__name__", ""):
                candidates.append(ch)
                seen_ids.add(id(ch))
    except Exception:
        pass
    closed_owner = False
    for cand in candidates:
        if cand is None or getattr(cand, "destroyed", False):
            if th is not None and getattr(th, "tooltipPanel", None) is cand:
                try:
                    th.tooltipPanel = None
                except Exception:
                    pass
            continue
        is_pinned = bool(getattr(cand, "_evomu_pinned", False))
        owner_alive = _is_tooltip_owner_alive(cand)
        if not is_pinned and owner_alive:
            continue
        if node is not None and owner_alive and _is_under_widget(node, cand):
            continue
        is_same_owner = node is not None and owner_alive and getattr(cand, "owner", None) is node
        _close_pinned_tooltip_panel(cand, th=th)
        if close_if_owner and is_same_owner:
            closed_owner = True
    return bool(closed_owner)


def _clear_py2_exc():
    exc_clear = getattr(sys, "exc_clear", None)
    if callable(exc_clear):
        exc_clear()


def _set_uilib_mouseover(uilib, node):
    if uilib is None or node is None:
        return
    prop = getattr(type(uilib), "mouseOver", None)
    try:
        if isinstance(prop, property) and getattr(prop, "fset", None) is None:
            if hasattr(uilib, "_mouseOver"):
                try:
                    uilib._mouseOver = weakref.ref(node)
                except TypeError:
                    _clear_py2_exc()
        else:
            uilib.mouseOver = node
    except Exception:
        _clear_py2_exc()


def _prime_label_inline_link(node, uilib=None, click_x=None, click_y=None):
    """Ensure LabelCore inline <url=...> links populate _mouseOverUrl before OnClick/GetMenu."""
    if node is None:
        return
    has_inlines = getattr(node, "_inlineObjects", None)
    check_fn = getattr(node, "CheckInlines", None)
    if not has_inlines and not callable(check_fn):
        return
    if uilib is None:
        uic_p = _get_uicore()
        uilib = getattr(uic_p, "uilib", None) if uic_p is not None else None
    _set_uilib_mouseover(uilib, node)
    if callable(check_fn):
        try:
            check_fn()
        except Exception:
            _clear_py2_exc()
    if getattr(node, "_mouseOverUrl", None):
        return
    links = []
    for inl in list(has_inlines or []):
        if getattr(inl, "inlineType", None) == "link" and getattr(inl, "data", None):
            links.append(inl)
    if not links:
        return
    rx, ry, rw, _ = _widget_abs_rect(node)
    target_inl = None
    if len(links) == 1:
        target_inl = links[0]
    elif click_x is not None and click_y is not None:
        cx = int(click_x)
        cy = int(click_y)
        best_d2 = None
        for inl in links:
            ix0 = rx + float(getattr(inl, "inlineX", 0) or 0)
            ix1 = rx + float(getattr(inl, "inlineXEnd", 0) or 0)
            iy0 = ry + float(getattr(inl, "inlineY", 0) or 0)
            iy1 = iy0 + float(getattr(inl, "inlineHeight", 14) or 14)
            if ix0 <= cx <= ix1 and iy0 <= cy <= iy1:
                target_inl = inl
                break
            mx = 0.5 * (ix0 + ix1)
            my = 0.5 * (iy0 + iy1)
            d2 = (cx - mx) * (cx - mx) + (cy - my) * (cy - my)
            if best_d2 is None or d2 < best_d2:
                best_d2 = d2
                target_inl = inl
    else:
        target_inl = links[0]
    if target_inl is None:
        return
    ix0 = rx + float(getattr(target_inl, "inlineX", 0) or 0)
    ix1 = rx + float(getattr(target_inl, "inlineXEnd", 0) or 0)
    iy0 = ry + float(getattr(target_inl, "inlineY", 0) or 0)
    ih = float(getattr(target_inl, "inlineHeight", 14) or 14)
    if ix1 <= ix0 and rw > 0:
        ix1 = rx + min(rw, float(getattr(target_inl, "inlineX", 0) or 0) + 12.0)
    tx = int(0.5 * (ix0 + ix1))
    ty = int(iy0 + 0.5 * ih)
    if uilib is not None:
        try:
            uilib.x = tx
            uilib.y = ty
        except Exception:
            _clear_py2_exc()
        _set_uilib_mouseover(uilib, node)
    if callable(check_fn):
        try:
            check_fn()
        except Exception:
            _clear_py2_exc()
    if not getattr(node, "_mouseOverUrl", None):
        try:
            node._mouseOverUrl = target_inl.data
        except Exception:
            _clear_py2_exc()


def _trigger_tooltip_panel(node):
    uic = _get_uicore()
    if uic is None:
        return None
    uilib = getattr(uic, "uilib", None)
    th = getattr(uilib, "tooltipHandler", None) if uilib is not None else None
    if th is None:
        return None
    try:
        uilib.leftbtn = 0
    except Exception:
        _clear_py2_exc()
    _set_uilib_mouseover(uilib, node)
    try:
        th.lastMouseOver = None
    except Exception:
        _clear_py2_exc()
    prev_tp = getattr(th, "tooltipPanel", None)
    if prev_tp is not None and not getattr(prev_tp, "destroyed", False):
        try:
            prev_tp._evomu_pinned = False
            if callable(getattr(prev_tp, "Close", None)):
                prev_tp.Close()
            th.tooltipPanel = None
        except Exception:
            pass
    loaded = False
    if hasattr(th, "_LoadTooltipPanel"):
        th._LoadTooltipPanel(node, True)
        tp = getattr(th, "tooltipPanel", None)
        if tp is not None and not getattr(tp, "destroyed", False) and getattr(tp, "children", None):
            loaded = True
    elif hasattr(th, "LoadTooltipImmediate"):
        th.LoadTooltipImmediate()
        tp = getattr(th, "tooltipPanel", None)
        loaded = tp is not None and not getattr(tp, "destroyed", False)
    if not loaded:
        return None
    tp = getattr(th, "tooltipPanel", None)
    if tp is not None and not getattr(tp, "destroyed", False):
        try:
            from eve.client.script.ui.control.pointerPanel import RefreshPanelPosition
            RefreshPanelPosition(tp)
        except Exception:
            pass
        try:
            tp.opacity = 1.0
        except Exception:
            pass
        try:
            for _mname in ("IsMouseInside", "IsOwnerPickable"):
                _orig = getattr(tp, _mname, None)
                if callable(_orig) and not getattr(tp, "_evomu_wrapped_" + _mname, False):
                    def _make_pinned(_fn=_orig, _tp=tp, _th=th):
                        def _wrapped(*a, **kw):
                            if getattr(_tp, "_evomu_pinned", False) and not getattr(_tp, "destroyed", False):
                                if not _is_tooltip_owner_alive(_tp):
                                    _close_pinned_tooltip_panel(_tp, th=_th)
                                    return False
                                return True
                            return _fn(*a, **kw)
                        return _wrapped
                    setattr(tp, _mname, _make_pinned())
                    setattr(tp, "_evomu_wrapped_" + _mname, True)
            tp._evomu_pinned = True
        except Exception:
            pass
        return "OpenTooltipPanel(%s)" % (_safe_str(getattr(node, "name", ""), 48),)
    return None


def _strip_tags(text):
    if not text:
        return ""
    u = _to_unicode(text)
    try:
        u = u.replace(u"<t>", u" ").replace(u"<br>", u" ")
    except Exception:
        pass
    out = []
    in_tag = 0
    for ch in u:
        if ch == u"<":
            in_tag += 1
        elif ch == u">" and in_tag > 0:
            in_tag -= 1
        elif in_tag == 0:
            out.append(ch)
    res = u" ".join(u"".join(out).split())
    if sys.version_info[0] < 3 and isinstance(res, unicode):
        return res.encode("utf-8", "replace")
    return res


def _is_ancestor_chain_visible(sub_node, stop_at=None):
    cur = sub_node
    for _ in range(5):
        if cur is None or cur is stop_at:
            break
        if not _is_visible(cur):
            return False
        cur = getattr(cur, "parent", None)
    return True


def _extract_widget_label(node):
    if node is None:
        return ""
    col_lbls = getattr(node, "columnLabels", None)
    if isinstance(col_lbls, (list, tuple)) and col_lbls:
        col_parts = []
        try:
            for cl in col_lbls:
                if not _is_visible(cl):
                    continue
                ct = _strip_tags(_safe_str(getattr(cl, "text", None), 120))
                if ct and ct != "-" and not ct.startswith("<") and ct not in col_parts:
                    col_parts.append(ct)
        except Exception:
            pass
        if col_parts:
            return _safe_str(" | ".join(col_parts[:4]), 90)
    for attr in ("text", "caption", "label", "_label_text", "rawText", "hint"):
        val = getattr(node, attr, None)
        if val is None or callable(val):
            continue
        if hasattr(val, "text") and not isinstance(val, (str, unicode)):
            if not _is_ancestor_chain_visible(val, node):
                continue
            val = getattr(val, "text", None)
        s = _strip_tags(_safe_str(val, 120))
        if s and not s.startswith("<"):
            return _safe_str(s, 80)
    for child_obj_attr in ("_label", "label", "caption", "title", "_message_label"):
        sub_obj = getattr(node, child_obj_attr, None)
        if sub_obj is not None and not isinstance(sub_obj, (str, unicode)):
            if not _is_ancestor_chain_visible(sub_obj, node):
                continue
            s = _strip_tags(_safe_str(getattr(sub_obj, "text", None), 120))
            if s:
                return _safe_str(s, 80)
    sr = getattr(node, "sr", None)
    if sr is not None:
        for sub in ("label", "caption", "text"):
            sub_node = getattr(sr, sub, None)
            if sub_node is not None:
                if not isinstance(sub_node, (str, unicode)) and not _is_ancestor_chain_visible(sub_node, node):
                    continue
                s = _strip_tags(_safe_str(getattr(sub_node, "text", sub_node), 120))
                if s and not s.startswith("<"):
                    return _safe_str(s, 80)
        sr_node = getattr(sr, "node", None)
        if sr_node is not None:
            for k in ("label", "name", "text", "caption"):
                v = sr_node.get(k) if isinstance(sr_node, dict) else getattr(sr_node, k, None)
                s = _strip_tags(_safe_str(v, 120))
                if s and not s.startswith("<"):
                    return _safe_str(s, 80)
    children = getattr(node, "children", None)
    if children:
        parts = []
        try:
            for ch in list(children)[:8]:
                if not _is_visible(ch):
                    continue
                ch_txt = getattr(ch, "text", None)
                if ch_txt and not callable(ch_txt):
                    s = _strip_tags(_safe_str(ch_txt, 120))
                    if s and not s.startswith("<") and s not in parts:
                        parts.append(s)
                for gch in list(getattr(ch, "children", []) or [])[:6]:
                    if not _is_visible(gch):
                        continue
                    g_txt = getattr(gch, "text", None)
                    if g_txt and not callable(g_txt):
                        s = _strip_tags(_safe_str(g_txt, 120))
                        if s and not s.startswith("<") and s not in parts:
                            parts.append(s)
        except Exception:
            pass
        if parts:
            return _safe_str(" | ".join(parts[:3]), 80)
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
    "ButtonWrapper",
    "ButtonGroup",
)


def _has_real_click_handler(node):
    if node is None:
        return False
    if _is_editable_input_node(node):
        return True
    cls_name = node.__class__.__name__
    if cls_name in _NON_INTERACTIVE_CONTAINER_CLASSES or cls_name.endswith("Underlay"):
        return False
    if cls_name.endswith("Wnd") or cls_name.endswith("Window") or cls_name.endswith("WindowStack") or hasattr(node, "windowID"):
        return False
    try:
        if hasattr(node, "HasEventHandler"):
            if node.HasEventHandler("OnClick") or node.HasEventHandler("OnDblClick"):
                return True
    except Exception:
        pass
    if (
        hasattr(node, "Click")
        or getattr(node, "func", None) is not None
        or callable(getattr(node, "on_click", None))
        or callable(getattr(node, "on_double_click", None))
    ):
        return True
    ctrl_node = getattr(node, "controller", None)
    if ctrl_node is not None and (
        callable(getattr(ctrl_node, "on_click", None))
        or callable(getattr(ctrl_node, "on_double_click", None))
    ):
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
            "Slider",
            "TextField",
            "SearchInput",
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
    "viewstate",
    "hangar",
    "station",
    "inflight",
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


def _auto_position_structure_deployment(w):
    """Ensure StructurePlacementEntity is at a valid conflict-free 3D offset in headless mode."""
    try:
        sm_obj = getattr(__builtin__, "sm", None)
        sd_svc = getattr(sm_obj, "services", {}).get("structureDeployment") if sm_obj is not None else None
        if sd_svc is None and sm_obj is not None and hasattr(sm_obj, "GetService"):
            sd_svc = sm_obj.GetService("structureDeployment")
        entity = getattr(sd_svc, "entity", None) if sd_svc is not None else None
        if entity is not None and hasattr(entity, "FindDeploymentConflict"):
            if entity.FindDeploymentConflict() is not None:
                base_dist = float(getattr(entity, "radius", 10000.0) or 10000.0) + 15000.0
                for extra in (0.0, 20000.0, 50000.0, 100000.0, 200000.0, 400000.0):
                    dist = min(750000.0, base_dist + extra)
                    found_valid = False
                    for deg in (0, 45, 90, 135, 180, 225, 270, 315):
                        rad = deg * 0.017453292519943295
                        pos = (dist * math.cos(rad), 0.0, dist * math.sin(rad))
                        for m_attr in ("model_valid", "model_invalid", "model_refinary_invalid", "model"):
                            m_obj = getattr(entity, m_attr, None)
                            tc = getattr(m_obj, "translationCurve", None) if m_obj is not None else None
                            if tc is not None:
                                tc.value = pos
                        if entity.FindDeploymentConflict() is None:
                            found_valid = True
                            break
                    if found_valid:
                        if hasattr(entity, "UpdateModel"):
                            entity.UpdateModel()
                        if w is not None and hasattr(w, "TriggerUIForValidLocation"):
                            w.TriggerUIForValidLocation()
                        break
        sched = getattr(w, "scheduleCont", None) if w is not None else None
        if sched is not None and not getattr(sched, "destroyed", False) and _is_visible(sched):
            if hasattr(sched, "GetGivenName") and len(sched.GetGivenName() or "") < 3:
                name_edit = getattr(sched, "nameEdit", None)
                prefix = getattr(sched, "ssPrefix", "") or getattr(name_edit, "prefix", "") or ""
                if name_edit is not None and hasattr(name_edit, "SetValue"):
                    default_name = prefix + "Citadel"
                    name_edit.SetValue(default_name)
                    if hasattr(w, "OnNameChanged"):
                        w.OnNameChanged()
    except Exception:
        _clear_py2_exc()


def _inspect_window_content(w):
    """Inspect bounded visible tabs, rows, loading indicators, and sample text labels inside a window."""
    summary = {
        "has_loading": False,
        "has_error": False,
        "tab_count": 0,
        "row_count": 0,
        "text_count": 0,
        "populated_count": 0,
        "sample_texts": [],
    }
    if w is None or getattr(w, "destroyed", False):
        return summary
    if bool(getattr(w, "loading", False) or getattr(w, "isLoading", False)):
        summary["has_loading"] = True
    wcap = _strip_tags(_safe_str(getattr(w, "caption", "") or "", 100)).strip().lower()
    seen = set()
    samples = []
    tab_count = 0
    row_count = 0
    text_count = 0
    queue = [(w, 0)]
    visited = 0
    skip_chrome = ("DefaultWindowHeader", "WindowUnderlay", "WindowControlButton", "StretchSpriteHorizontal", "StretchSpriteVertical")
    while queue and visited < 110:
        node, depth = queue.pop(0)
        if node is None or id(node) in seen or getattr(node, "destroyed", False):
            continue
        seen.add(id(node))
        if node is not w and not _is_visible(node):
            continue
        cls_n = node.__class__.__name__
        if node is not w and cls_n in skip_chrome:
            continue
        visited += 1
        nname = _safe_str(getattr(node, "name", ""), 64).lower()
        cls_l = cls_n.lower()
        if "loadingwheel" in cls_l or nname in ("loading", "loadingwheel", "loadingindicator", "busyindicator"):
            summary["has_loading"] = True
        if hasattr(node, "GetNodes"):
            try:
                nodes = node.GetNodes() or []
                row_count += len(nodes)
            except Exception:
                pass
        if hasattr(node, "tabs") and isinstance(getattr(node, "tabs", None), (list, tuple)):
            try:
                tab_count += len(getattr(node, "tabs") or ())
            except Exception:
                pass
        if node is not w and ("tab" in cls_l and "tabgroup" not in cls_l):
            tab_count += 1
        elif node is not w and any(cls_l.endswith(suf) for suf in ("entry", "row", "order", "item", "notificationentry", "skillentry", "servicebtn")):
            row_count += 1
        raw_txt = getattr(node, "text", None)
        if raw_txt and not callable(raw_txt) and not isinstance(raw_txt, (bool, int, float)):
            clean_txt = _strip_tags(_safe_str(raw_txt, 100)).strip()
            low_txt = clean_txt.lower()
            if clean_txt:
                if low_txt in ("loading", "loading...", "please wait", "loading data..."):
                    summary["has_loading"] = True
                elif any(err_k in low_txt for err_k in ("error loading", "failed to load", "unable to load", "unexpected error")):
                    summary["has_error"] = True
                elif low_txt != wcap and low_txt not in ("x", "?", "-", "_"):
                    text_count += 1
                    if clean_txt not in samples and len(samples) < 10:
                        samples.append(clean_txt[:60])
        if depth < 8:
            for ch in list(getattr(node, "children", []) or [])[:25]:
                if id(ch) not in seen:
                    queue.append((ch, depth + 1))
    summary["tab_count"] = int(tab_count)
    summary["row_count"] = int(row_count)
    summary["text_count"] = int(text_count)
    summary["populated_count"] = int(tab_count + row_count + text_count)
    summary["sample_texts"] = samples
    return summary


def _inspect_charsel_state(sm):
    """Read visible character-selection UI slots/characters without issuing remote RPCs."""
    out = {"active": False, "characters": [], "names": [], "slot_count": 0}
    uc = _get_uicore()
    layers = getattr(uc, "layer", None) if uc is not None else None
    cs = getattr(layers, "charsel", None) if layers is not None else None
    is_open = bool(cs is not None and (getattr(cs, "isopen", False) or _is_visible(cs)))
    vs = getattr(sm, "services", {}).get("viewState") if sm is not None else None
    if not is_open and vs is not None and hasattr(vs, "GetCurrentViewInfo"):
        try:
            vi = vs.GetCurrentViewInfo()
            is_open = bool(getattr(vi, "name", None) == "charsel")
        except Exception:
            pass
    out["active"] = is_open
    if cs is None:
        return out
    seen_cids = set()
    seen_names = set()
    chars = []
    slots = list(getattr(cs, "slots", None) or [])
    out["slot_count"] = len(slots)
    cs_data = getattr(cs, "characterSelectionData", None)
    details_map = getattr(cs_data, "details", None) if cs_data is not None else None
    if isinstance(details_map, dict):
        for cid, det in details_map.items():
            try:
                cid_i = int(cid)
            except Exception:
                continue
            cname = _safe_str(
                getattr(det, "characterName", None)
                or (det.get("characterName") if isinstance(det, dict) else None)
                or getattr(det, "name", None)
                or "",
                64,
            ).strip()
            if not cname:
                try:
                    cinfo = getattr(__builtin__, "cfg", None)
                    eveowners = getattr(cinfo, "eveowners", None)
                    if eveowners is not None and hasattr(eveowners, "GetIfExists"):
                        owner = eveowners.GetIfExists(cid_i)
                        cname = _safe_str(getattr(owner, "name", "") or "", 64).strip()
                except Exception:
                    pass
            if cid_i and cid_i not in seen_cids:
                seen_cids.add(cid_i)
                chars.append({"character_id": cid_i, "name": cname})
                if cname and cname not in seen_names:
                    seen_names.add(cname)
    for sl in slots:
        cd = getattr(sl, "charDetails", None)
        cid_val = getattr(sl, "charID", None) or getattr(sl, "characterID", None)
        if cid_val is None and cd is not None:
            cid_val = getattr(cd, "charID", None) or getattr(cd, "characterID", None)
        lbl = _extract_widget_label(sl)
        try:
            cid_i = int(cid_val) if cid_val else 0
        except Exception:
            cid_i = 0
        cname = lbl.split("|")[0].strip() if lbl else ""
        if not cname:
            cname = _safe_str(
                getattr(sl, "characterName", None)
                or (getattr(cd, "characterName", None) if cd is not None else None)
                or (getattr(cd, "name", None) if cd is not None else None)
                or "",
                64,
            ).strip()
        if not cname and cid_i:
            try:
                cinfo_s = getattr(__builtin__, "cfg", None)
                eveowners_s = getattr(cinfo_s, "eveowners", None)
                if eveowners_s is not None and hasattr(eveowners_s, "GetIfExists"):
                    owner_s = eveowners_s.GetIfExists(cid_i)
                    cname = _safe_str(getattr(owner_s, "name", "") or "", 64).strip()
            except Exception:
                pass
        if cid_i and cid_i not in seen_cids:
            seen_cids.add(cid_i)
            chars.append({"character_id": cid_i, "name": cname})
            if cname and cname not in seen_names:
                seen_names.add(cname)
        elif cname and cname not in seen_names:
            seen_names.add(cname)
    # Also scan visible text labels under charsel layer for rendered character names
    if is_open:
        q = [cs]
        vset = set()
        while q and len(vset) < 120:
            n = q.pop(0)
            if n is None or id(n) in vset or not _is_visible(n):
                continue
            vset.add(id(n))
            t = _strip_tags(_safe_str(getattr(n, "text", "") or "", 64)).strip()
            if t and t in seen_names:
                pass
            for ch in list(getattr(n, "children", []) or [])[:16]:
                q.append(ch)
    out["characters"] = chars
    out["names"] = [c["name"] for c in chars if c.get("name")] or sorted(seen_names)
    return out


def _is_window_minimized(wnd):
    if wnd is None:
        return False
    try:
        return bool(getattr(wnd, "minimized", False) or (hasattr(wnd, "IsMinimized") and wnd.IsMinimized()))
    except Exception:
        return False


def _is_chain_visible(node, stop_at=None, max_depth=24, allow_minimized=False):
    cur = node
    for _ in range(max_depth):
        if cur is None:
            break
        cur_min = _is_window_minimized(cur)
        if cur_min and not allow_minimized:
            return False
        if cur is stop_at:
            return _is_visible(cur) or (allow_minimized and cur_min)
        if not _is_visible(cur) and not (allow_minimized and cur_min):
            return False
        cur = getattr(cur, "parent", None)
    return True


def _maximize_dscan_surface(ds_cont, ds_host):
    maximized = False
    targets = [ds_host] if (ds_cont is ds_host) else [ds_host, ds_cont]
    for target_node in targets:
        if target_node is None or not _is_window_minimized(target_node):
            continue
        if hasattr(target_node, "Maximize"):
            try:
                target_node.Maximize()
                maximized = True
            except Exception:
                pass
    if maximized:
        H._sleep_ms(300)


def _collect_dscan_candidate_roots(uicore):
    win_roots = []
    layer_roots = []
    seen = set()

    def _add(bucket, obj):
        if obj is not None and id(obj) not in seen and not getattr(obj, "destroyed", False):
            seen.add(id(obj))
            bucket.append(obj)

    if uicore is not None:
        reg = getattr(uicore, "registry", None)
        if reg is not None and hasattr(reg, "GetWindows"):
            try:
                for w in list(reg.GetWindows() or []):
                    _add(win_roots, w)
            except Exception:
                pass
    for mod_path, cls_name in (
        ("eve.client.script.ui.inflight.scannerFiles.directionalScannerWindow", "DirectionalScanner"),
        ("eve.client.script.ui.shared.mapView.solarSystemViewPanel", "SolarSystemViewPanel"),
    ):
        try:
            mod = __import__(mod_path, fromlist=[cls_name])
            cls_obj = getattr(mod, cls_name, None)
            if cls_obj is not None and hasattr(cls_obj, "GetIfOpen"):
                _add(win_roots, cls_obj.GetIfOpen())
        except Exception:
            pass
    if uicore is not None:
        for _, lobj in _iter_visible_layers(uicore):
            try:
                for ch in list(getattr(lobj, "children", []) or []):
                    _add(win_roots, ch)
            except Exception:
                pass
            _add(layer_roots, lobj)
    return win_roots + layer_roots


def _find_solar_system_map_panel(uicore=None, include_minimized=False):
    if uicore is None:
        uicore = _get_uicore()
    for w in _collect_dscan_candidate_roots(uicore):
        is_min = _is_window_minimized(w)
        if is_min and not include_minimized:
            continue
        if not _is_visible(w) and not (is_min and include_minimized):
            continue
        wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
        cls_n = w.__class__.__name__
        if wid == "solar_system_map_panel" or cls_n == "SolarSystemViewPanel" or hasattr(w, "mapView"):
            return w
    return None


def _find_directional_scanner_surface(uicore=None, include_minimized=False):
    """Locate visible standalone DirectionalScanner or embedded DirectionalScannerPalette.

    Returns (kind, container, host_window, is_minimized) or (None, None, None, False).
    """
    if uicore is None:
        uicore = _get_uicore()
    roots = _collect_dscan_candidate_roots(uicore)

    for w in roots:
        wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
        cls_n = w.__class__.__name__
        is_standalone = bool(
            wid == "directionalScannerWindow"
            or cls_n == "DirectionalScanner"
            or (cls_n.startswith("DirectionalScanner") and "Palette" not in cls_n)
        )
        if not is_standalone:
            continue
        is_min = _is_window_minimized(w)
        if is_min and not include_minimized:
            continue
        if _is_visible(w) or (is_min and include_minimized):
            return ("standalone", w, w, is_min)

    for host in roots:
        is_host_min = _is_window_minimized(host)
        if is_host_min and not include_minimized:
            continue
        if not _is_visible(host) and not (is_host_min and include_minimized):
            continue
        mv = getattr(host, "mapView", None)
        pal = getattr(mv, "directionalScannerPalette", None) if mv is not None else None
        if pal is None:
            pal = getattr(host, "directionalScannerPalette", None)
        if pal is None:
            q = [(host, 0)]
            q_seen = set()
            while q and len(q_seen) < 300:
                cur, depth = q.pop(0)
                if cur is None or id(cur) in q_seen or getattr(cur, "destroyed", False):
                    continue
                q_seen.add(id(cur))
                if cur is not host:
                    cur_min = _is_window_minimized(cur)
                    if cur_min and not include_minimized:
                        continue
                    if not _is_visible(cur) and not (cur_min and include_minimized):
                        continue
                cur_cls = cur.__class__.__name__
                cur_name = _safe_str(getattr(cur, "name", ""), 64)
                if cur_cls == "DirectionalScannerPalette" or cur_name == "DirectionalScannerPalette":
                    pal = cur
                    break
                if depth < 14 and "Marker" not in cur_cls and "Bracket" not in cur_cls:
                    for ch in list(getattr(cur, "children", []) or [])[:40]:
                        if id(ch) not in q_seen:
                            q.append((ch, depth + 1))
        if (
            pal is not None
            and not getattr(pal, "destroyed", False)
            and (_is_visible(pal) or (include_minimized and _is_window_minimized(pal)))
            and _is_chain_visible(pal, host, allow_minimized=include_minimized)
        ):
            eff_min = bool(is_host_min or _is_window_minimized(pal))
            if eff_min and not include_minimized:
                continue
            return ("embedded", pal, host, eff_min)

    return (None, None, None, False)


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
        desk = getattr(uicore, "desktop", None)
        desk_w = int(getattr(desk, "width", 1280) or 1280) if desk is not None else 1280
        desk_h = int(getattr(desk, "height", 800) or 800) if desk is not None else 800
        for w in (wins or []):
            if getattr(w, "destroyed", False) or not _is_visible(w):
                continue
            if w.__class__.__name__ == "CalendarWnd" and getattr(w, "calendarCore", "OK") is None:
                try:
                    w._OnResize = lambda *a, **kw: None
                    if hasattr(w, "Close"):
                        w.Close()
                except Exception:
                    pass
                continue
            if w.__class__.__name__ == "StructureDeploymentWnd":
                _auto_position_structure_deployment(w)
            try:
                is_min = bool(getattr(w, "minimized", False) or (hasattr(w, "IsMinimized") and w.IsMinimized()))
            except Exception:
                is_min = False
            x, y, wd, ht = _widget_abs_rect(w)
            if not is_min and wd > 40 and ht > 40 and wd < desk_w and ht < desk_h:
                clamped = False
                if y + ht > desk_h and y > 8:
                    try:
                        w.top = max(8, desk_h - ht - 8)
                        clamped = True
                    except Exception:
                        pass
                if x + wd > desk_w and x > 48:
                    try:
                        w.left = max(48, desk_w - wd - 8)
                        clamped = True
                    except Exception:
                        pass
                if clamped:
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
            w_content = _inspect_window_content(w)
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
                "has_loading": w_content["has_loading"],
                "has_error": w_content["has_error"],
                "tab_count": w_content["tab_count"],
                "row_count": w_content["row_count"],
                "populated_count": w_content["populated_count"],
                "sample_texts": w_content["sample_texts"],
            }
            msg_lbl = getattr(w, "_message_label", None)
            if msg_lbl is not None and getattr(msg_lbl, "text", None):
                info["message_text"] = _strip_tags(_safe_str(getattr(msg_lbl, "text", ""), 200))
            if getattr(w, "msgKey", None):
                info["msg_key"] = _safe_str(getattr(w, "msgKey", ""), 80)
            open_windows.append(info)
            if is_modal:
                modals.append(info)

        # Include NotificationCenter drawer and active secondary view (e.g. New Eden Store) when visible
        sm_obj = getattr(__builtin__, "sm", None)
        svcs = getattr(sm_obj, "services", {}) if sm_obj is not None else {}
        n_ui = svcs.get("notificationUIService")
        nc = getattr(n_ui, "notificationCenter", None) if n_ui is not None else None
        if nc is not None and not any(o.get("window_id") == "notifications" for o in open_windows):
            nc_cont = getattr(nc, "notificationContainer", None) or getattr(nc, "drawer", None) or nc
            is_showing = bool(getattr(nc, "isShowingDrawer", False))
            if is_showing or (nc_cont is not nc and _is_visible(nc_cont)):
                nc_c = _inspect_window_content(nc_cont)
                open_windows.append({
                    "window_id": "notifications",
                    "name": "NotificationCenter",
                    "cls": nc.__class__.__name__,
                    "caption": "Notifications",
                    "x": 0,
                    "y": 0,
                    "w": 320,
                    "h": 480,
                    "minimized": False,
                    "is_showing_drawer": is_showing,
                    "has_loading": nc_c["has_loading"],
                    "has_error": nc_c["has_error"],
                    "tab_count": nc_c["tab_count"],
                    "row_count": nc_c["row_count"],
                    "populated_count": nc_c["populated_count"],
                    "sample_texts": nc_c["sample_texts"],
                })
        vs = svcs.get("viewState")
        if vs is not None and hasattr(vs, "GetCurrentViewInfo"):
            try:
                vi = vs.GetCurrentViewInfo()
                vname = _safe_str(getattr(vi, "name", "") or "", 48)
                sec_vi = getattr(vs, "secondaryInfo", None)
                if sec_vi is not None and getattr(sec_vi, "name", None):
                    sec_name = _safe_str(getattr(sec_vi, "name", ""), 48)
                    if not any(sec_name.lower() in (o.get("window_id", "").lower(), o.get("cls", "").lower()) for o in open_windows):
                        sec_view_obj = getattr(vs, "activeView", None)
                        sec_layer = getattr(sec_view_obj, "layer", None) if sec_view_obj is not None else None
                        sec_c = _inspect_window_content(sec_layer)
                        open_windows.append({
                            "window_id": sec_name,
                            "name": sec_name,
                            "cls": sec_view_obj.__class__.__name__ if sec_view_obj is not None else sec_name,
                            "caption": "New Eden Store" if "store" in sec_name.lower() else sec_name,
                            "x": 0,
                            "y": 0,
                            "w": desk_w,
                            "h": desk_h,
                            "minimized": False,
                            "has_loading": sec_c["has_loading"],
                            "has_error": sec_c["has_error"],
                            "tab_count": sec_c["tab_count"],
                            "row_count": sec_c["row_count"],
                            "populated_count": sec_c["populated_count"],
                            "sample_texts": sec_c["sample_texts"],
                        })
            except Exception:
                pass
        ds_kind, ds_cont, ds_host, ds_min = _find_directional_scanner_surface(uicore, include_minimized=False)
        if ds_cont is not None and not ds_min and not any(
            "directionalscanner" in _normalize_window_tok(o.get("window_id") or o.get("cls") or "")
            for o in open_windows
        ):
            pal_x, pal_y, pal_w, pal_h = _widget_abs_rect(ds_cont)
            if (pal_w <= 0 or pal_h <= 0) and ds_host is not None:
                pal_x, pal_y, pal_w, pal_h = _widget_abs_rect(ds_host)
            pal_c = _inspect_window_content(ds_cont)
            ds_entry = {
                "window_id": "directionalScannerWindow",
                "name": _safe_str(getattr(ds_cont, "name", None) or ds_cont.__class__.__name__, 64),
                "cls": ds_cont.__class__.__name__,
                "caption": _strip_tags(_safe_str(getattr(ds_cont, "caption", None) or "Directional Scanner", 100)),
                "x": pal_x,
                "y": pal_y,
                "w": pal_w,
                "h": pal_h,
                "minimized": False,
                "has_loading": pal_c["has_loading"],
                "has_error": pal_c["has_error"],
                "tab_count": pal_c["tab_count"],
                "row_count": pal_c["row_count"],
                "populated_count": pal_c["populated_count"],
                "sample_texts": pal_c["sample_texts"],
            }
            if ds_kind == "embedded":
                ds_entry["embedded"] = True
                if ds_host is not None:
                    ds_entry["host_window_id"] = _safe_str(
                        getattr(ds_host, "windowID", None) or getattr(ds_host, "name", "") or ds_host.__class__.__name__,
                        64,
                    )
            open_windows.append(ds_entry)
    except Exception:
        pass

    visited_nodes = set()

    def _walk(node, path_prefix, depth, bucket, scroll_state=None):
        if depth > 18 or len(bucket) >= 320 or id(node) in visited_nodes or not _is_pickable(node):
            return
        if scroll_state is not None and scroll_state[0] >= 96:
            return
        visited_nodes.add(id(node))
        name = _safe_str(getattr(node, "name", ""), 64) or node.__class__.__name__
        cur_path = "%s/%s" % (path_prefix, name) if path_prefix else name
        cls_name = node.__class__.__name__
        is_editable = _is_editable_input_node(node)
        is_interactive = is_editable or _has_real_click_handler(node)
        if is_interactive:
            x, y, w, h = _widget_clipped_rect(node)
            if w >= 8 and h >= 8 and x >= 0 and y >= 0:
                lbl = _extract_widget_label(node)
                hint_str = _strip_tags(_safe_str(getattr(node, "hint", ""), 80))
                if cls_name == "SelectedItemButton":
                    dis_hint = _strip_tags(_safe_str(getattr(node, "disabledHint", ""), 80))
                    is_dis = bool(dis_hint) or (hasattr(node, "IsEnabled") and not node.IsEnabled())
                    if is_dis:
                        if not lbl.startswith("[DISABLED]"):
                            lbl = "[DISABLED] %s" % (lbl,)
                        if dis_hint and not hint_str:
                            hint_str = "DISABLED: %s" % (dis_hint,)
                if not hint_str and cls_name == "MarketOrder":
                    if "buyParent/buyscroll" in cur_path:
                        hint_str = "Sellers (Sell Order - double-click or right-click Buy This)"
                    elif "sellParent/sellscroll" in cur_path:
                        hint_str = "Buyers (Buy Order - right-click Sell This)"
                if is_editable and not hint_str:
                    hint_str = "Editable text field (type_text; end with \\n to submit)"
                item = {
                    "path": cur_path[-120:],
                    "name": name,
                    "cls": cls_name,
                    "label": lbl,
                    "hint": hint_str,
                    "x": int(x + w // 2),
                    "y": int(y + h // 2),
                    "w": w,
                    "h": h,
                }
                if is_editable:
                    item["editable"] = True
                    cur_val = _get_editable_value_preview(node)
                    if cur_val:
                        item["value"] = cur_val
                if (cur_path.startswith("menu/") and "TooltipPanel" not in cur_path and "Tooltip" not in cur_path) or "MenuEntry" in cls_name:
                    context_menu.append(item)
                else:
                    bucket.append(item)
                    if scroll_state is not None:
                        scroll_state[0] += 1
        clip_flag = getattr(node, "clipChildren", False)
        if isinstance(clip_flag, (bool, int)) and clip_flag:
            if hasattr(node, "GetAbsolute") or hasattr(node, "width") or hasattr(node, "displayWidth"):
                cx, cy, cw, ch = _widget_clipped_rect(node)
                if cw < 4 or ch < 4:
                    return
        is_scroll_bucket = (
            cls_name in ("ScrollContainer", "BasicDynamicScroll", "Scroll")
            or name in ("clipCont", "__clipper")
        )
        child_scroll_state = [0] if (scroll_state is None and is_scroll_bucket) else scroll_state
        children = getattr(node, "children", None)
        if children:
            try:
                for ch in list(children):
                    _walk(ch, cur_path, depth + 1, bucket, child_scroll_state)
                    if child_scroll_state is not None and child_scroll_state[0] >= 96:
                        break
            except Exception:
                pass

    for lname, lobj in _iter_visible_layers(uicore):
        _walk(lobj, lname, 0, clickables)

    if len(clickables) > 75:
        head = clickables[:55]
        tail = clickables[55:]

        def _is_priority_clickable(w):
            p = w.get("path", "")
            wcls = w.get("cls", "")
            wnm = w.get("name", "")
            if w.get("editable") or "/l_shipui/" in p or p.startswith(("utilmenu/", "abovemain/", "menu/")):
                return True
            if wcls in ("ScanButton", "DirectionalScannerPalette", "ScannerPaletteHeader", "ProbeScannerPalette"):
                return True
            if wnm in ("ScanButton", "auInput", "distanceSlider", "angleSlider", "alignWithCameraBtn", "showConeBtn"):
                return True
            if "/scroll/" not in p.lower() and (
                "/DirectionalScannerPalette/" in p
                or "/DirectionalScanner/" in p
                or "/directionalScannerWindow/" in p
                or "/ScannerPaletteHeader/" in p
            ):
                return True
            return False

        priority_tail = [w for w in tail if _is_priority_clickable(w)]
        if priority_tail:
            prio_ids = set(id(w) for w in priority_tail)
            other_tail = [w for w in tail if id(w) not in prio_ids]
            clickables = head + priority_tail + other_tail

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
    t_low = _to_utf8(target).lower()
    best = [None, -1]
    fallback = [None, -1]
    cur_modal = None
    try:
        reg = getattr(uicore, "registry", None)
        if reg is not None and hasattr(reg, "GetModalWindow"):
            cand_m = reg.GetModalWindow()
            if cand_m is not None and not getattr(cand_m, "destroyed", False):
                cur_modal = cand_m
    except Exception:
        cur_modal = None

    def _search(node, cur_path, depth):
        if depth > 19 or not _is_pickable(node):
            return
        name = _safe_str(getattr(node, "name", ""), 64) or node.__class__.__name__
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
            if cur_modal is not None and _is_under_widget(node, cur_modal):
                score += 200
            if hint_x is not None and hint_y is not None and (hint_x > 0 or hint_y > 0):
                try:
                    wx, wy, ww, wh = _widget_clipped_rect(node)
                    if ww >= 4 and wh >= 4 and wx <= hint_x <= (wx + ww) and wy <= hint_y <= (wy + wh):
                        score += 50
                except Exception:
                    pass
            if _has_real_click_handler(node):
                if score > best[1]:
                    best[0] = node
                    best[1] = score
            elif hasattr(node, "OnClick") or hasattr(node, "Click") or hasattr(node, "OnMouseDown"):
                if score > fallback[1]:
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


_MODAL_DISMISS_LABELS = {
    "maybe later": 100,
    "cancel": 95,
    "no": 90,
    "dismiss": 85,
    "close": 80,
}


def _find_modal_dismiss_button(wnd):
    """Find a visible non-confirm dismiss button (e.g. 'Maybe later', 'Cancel') inside a modal."""
    if wnd is None or getattr(wnd, "destroyed", False):
        return None
    best = [None, -1]

    def _walk(node, depth):
        if node is None or depth > 16 or not _is_visible(node):
            return
        if node is not wnd:
            lbl = _strip_tags(_extract_widget_label(node)).strip().lower()
            name = _safe_str(getattr(node, "name", ""), 64).strip().lower()
            prio = _MODAL_DISMISS_LABELS.get(lbl, _MODAL_DISMISS_LABELS.get(name, -1))
            if prio < 0 and ("cancel" in name or "dismiss" in name or "maybelater" in name.replace("_", "")):
                prio = 70
            if prio > best[1] and (
                _has_real_click_handler(node)
                or callable(getattr(node, "OnClick", None))
                or callable(getattr(node, "Click", None))
            ):
                best[0] = node
                best[1] = prio
        children = getattr(node, "children", None)
        if children:
            try:
                for ch in list(children):
                    _walk(ch, depth + 1)
            except Exception:
                pass

    _walk(wnd, 0)
    return best[0]


def _has_real_drop_handler(node):
    if node is None:
        return False
    d_cls = node.__class__.__name__
    d_name = _safe_str(getattr(node, "name", ""), 48)
    if (
        d_cls.endswith("Wnd")
        or d_cls.endswith("Window")
        or d_cls.endswith("WindowStack")
        or d_cls in ("Layer", "LayerCore", "Desktop", "InflightView")
        or d_cls.endswith("Layer")
        or d_name.startswith("l_")
        or hasattr(node, "windowID")
    ):
        return False
    if "TreeViewEntry" in d_cls:
        d_data = getattr(node, "data", None)
        has_ctrl = (
            getattr(node, "invController", None) is not None
            or getattr(d_data, "invController", None) is not None
            or getattr(node, "eventListener", None) is not None
        )
        if not has_ctrl:
            return False
    try:
        if hasattr(node, "HasEventHandler"):
            return bool(node.HasEventHandler("OnDropData"))
    except Exception:
        pass
    return False


def _find_widget_at_xy(px, py, layers_root, for_drop=False):
    uicore = _get_uicore()
    if uicore is None:
        return None
    best = [None, 10**12]

    def _hit(node, depth):
        if depth > 17 or not _is_pickable(node):
            return
        clip_flag = getattr(node, "clipChildren", False)
        if isinstance(clip_flag, (bool, int)) and clip_flag:
            if hasattr(node, "GetAbsolute") or hasattr(node, "width") or hasattr(node, "displayWidth"):
                cx, cy, cw, ch = _widget_clipped_rect(node)
                if cw <= 0 or ch <= 0 or not (cx <= px <= (cx + cw) and cy <= py <= (cy + ch)):
                    return
        x, y, w, h = _widget_clipped_rect(node)
        if w >= 6 and h >= 6 and x <= px <= (x + w) and y <= py <= (y + h):
            if for_drop:
                matched = _has_real_drop_handler(node)
            else:
                n_cls = node.__class__.__name__
                is_top_win = n_cls.endswith("Wnd") or n_cls.endswith("Window") or n_cls.endswith("WindowStack") or hasattr(node, "windowID")
                matched = _has_real_click_handler(node) or (not is_top_win and hasattr(node, "HasEventHandler") and node.HasEventHandler("GetMenu"))
            if matched:
                area = w * h
                cur_cls = best[0].__class__.__name__ if best[0] is not None else ""
                if area < best[1] or (cur_cls == "SE_BaseClassCore" and area < int(best[1] * 1.25)):
                    best[0] = node
                    best[1] = area
        children = getattr(node, "children", None)
        if children:
            try:
                for ch in list(children):
                    if depth == 0:
                        c_cls = ch.__class__.__name__
                        c_name = _safe_str(getattr(ch, "name", ""), 48)
                        is_win = getattr(ch, "windowID", None) is not None or c_cls.endswith("Window") or c_cls.endswith("Wnd") or c_cls == "WindowStack"
                        if is_win and _is_pickable(ch):
                            wx, wy, ww, wh = _widget_abs_rect(ch)
                            if ww >= 40 and wh >= 24 and wx <= px <= (wx + ww) and wy <= py <= (wy + wh):
                                _hit(ch, depth + 1)
                                break
                        elif c_name.startswith("l_modal_") and _is_pickable(ch):
                            hit_modal_win = False
                            for mch in list(getattr(ch, "children", []) or []):
                                mcls = mch.__class__.__name__
                                if (
                                    _is_pickable(mch)
                                    and (getattr(mch, "windowID", None) is not None or getattr(mch, "isModal", False) or "MessageBox" in mcls or mcls.endswith("Window") or mcls.endswith("Wnd"))
                                ):
                                    mx, my, mw, mh = _widget_abs_rect(mch)
                                    if mw >= 40 and mh >= 24 and mx <= px <= (mx + mw) and my <= py <= (my + mh):
                                        hit_modal_win = True
                                        break
                            if hit_modal_win:
                                _hit(ch, depth + 1)
                                break
                    _hit(ch, depth + 1)
            except Exception:
                pass

    for _, lobj in _iter_visible_layers(uicore):
        _hit(lobj, 0)
        if best[0] is not None and best[1] < 200000:
            return best[0]
    return best[0]


def _is_eveui_text_field(n):
    if n is None:
        return False
    ctrl = getattr(n, "controller", None)
    return bool(
        ctrl is not None
        and hasattr(ctrl, "replace_text")
        and hasattr(ctrl, "clear_text")
        and (hasattr(n, "insert_text") or hasattr(n, "on_char"))
    )


def _is_editable_input_node(n):
    if n is None:
        return False
    cls_n = n.__class__.__name__
    for non_edit in ("Label", "Button", "Combo", "Checkbox", "Slider", "Progress", "Scroll", "Menu"):
        if non_edit in cls_n:
            return False
    if _is_eveui_text_field(n):
        return True
    if hasattr(n, "SetValue") and ("Edit" in cls_n or "Input" in cls_n or hasattr(n, "OnChar")):
        return True
    if ("EditPlainText" in cls_n or "Edit" in cls_n or "TextField" in cls_n or "SearchInput" in cls_n) and (
        hasattr(n, "SetValue") or hasattr(n, "SetText") or hasattr(n, "insert_text")
    ):
        return True
    return False


def _get_editable_value_preview(n):
    if n is None:
        return ""
    try:
        ctrl = getattr(n, "controller", None)
        if ctrl is not None and hasattr(ctrl, "text") and ctrl.text is not None:
            return _strip_tags(_safe_str(ctrl.text, 40))
    except Exception:
        pass
    for attr in ("GetValue", "GetText"):
        fn = getattr(n, attr, None)
        if callable(fn):
            try:
                val = fn()
                if val is not None:
                    return _strip_tags(_safe_str(val, 40))
            except Exception:
                pass
    return _strip_tags(_safe_str(getattr(n, "text", ""), 40))


def _resolve_editable_widget(node):
    cur = node
    for _ in range(5):
        if cur is None:
            break
        if _is_editable_input_node(cur):
            return cur
        cur = getattr(cur, "parent", None)

    def _walk_down(n, depth):
        if n is None or depth > 3:
            return None
        for ch in list(getattr(n, "children", []) or []):
            if _is_editable_input_node(ch):
                return ch
            sub = _walk_down(ch, depth + 1)
            if sub is not None:
                return sub
        return None

    return _walk_down(node, 0)


def _find_any_visible_editable(layers):
    if layers is None:
        return None
    found = []

    def _walk(n, depth):
        if n is None or depth > 18 or getattr(n, "destroyed", False) or not _is_visible(n):
            return
        if _is_editable_input_node(n):
            cls_name = n.__class__.__name__
            prio = 2 if ("Edit" in cls_name or "Input" in cls_name or "Field" in cls_name) else 1
            found.append((prio, n))
        for ch in list(getattr(n, "children", []) or []):
            _walk(ch, depth + 1)

    for lname in ("modal", "abovemain", "main"):
        _walk(getattr(layers, lname, None), 0)
    if not found:
        return None
    found.sort(key=lambda x: x[0], reverse=True)
    return found[0][1]


def _set_slider_from_x(node, target_x=None):
    cur = node
    for _ in range(5):
        if cur is None:
            break
        cls_n = _safe_str(getattr(getattr(cur, "__class__", None), "__name__", ""), 48)
        if "Slider" in cls_n or (hasattr(cur, "SetValue") and hasattr(cur, "_OnEndUserAction")):
            try:
                bar = getattr(cur, "barCont", None) or cur
                sx, _sy, sw, _sh = _widget_abs_rect(bar)
                if sw <= 0:
                    sx, _sy, sw, _sh = _widget_abs_rect(cur)
                if target_x is not None and target_x > 0 and sw > 0:
                    frac = max(0.0, min(1.0, float(target_x - sx) / float(max(1, sw))))
                else:
                    frac = 0.5
                inc_vals = getattr(cur, "incrementValues", None)
                cur_val = getattr(cur, "value", None)
                if inc_vals and len(inc_vals) > 0:
                    idx = int(round(frac * float(len(inc_vals) - 1)))
                    idx = max(0, min(len(inc_vals) - 1, idx))
                    new_val = inc_vals[idx]
                    if new_val == cur_val and len(inc_vals) > 1:
                        idx = (idx + 1) % len(inc_vals)
                        new_val = inc_vals[idx]
                else:
                    min_v = float(getattr(cur, "minValue", 0.0) or 0.0)
                    max_v = float(getattr(cur, "maxValue", 1.0) or 1.0)
                    new_val = min_v + frac * (max_v - min_v)
                    if cur_val is not None and abs(float(cur_val) - new_val) < 1e-6:
                        new_val = min_v + (0.75 if frac < 0.6 else 0.25) * (max_v - min_v)
                cur.SetValue(new_val)
                if hasattr(cur, "_OnEndUserAction"):
                    cur._OnEndUserAction()
                return "Slider.SetValue(%s)" % (_safe_str(new_val, 32),)
            except Exception:
                pass
        cur = getattr(cur, "parent", None)
    return None


def _close_neocom_panels_if_outside(node):
    try:
        cur = node
        for _ in range(12):
            if cur is None:
                break
            cls_n = _safe_str(getattr(getattr(cur, "__class__", None), "__name__", ""), 48)
            nm = _safe_str(getattr(cur, "name", ""), 48)
            if "PanelEveMenu" in cls_n or "PanelBase" in cls_n or nm in ("NeocomMenuBtn", "eveMenuBtn", "charSheetBtn"):
                return
            cur = getattr(cur, "parent", None)
        sm = getattr(__builtin__, "sm", None)
        if sm is not None and hasattr(sm, "IsServiceRunning") and sm.IsServiceRunning("neocom"):
            neo = sm.GetService("neocom")
            if hasattr(neo, "CloseAllPanels"):
                neo.CloseAllPanels()
    except Exception:
        pass


def _close_utilmenu_if_outside(node):
    try:
        uic = _get_uicore()
        layer = getattr(uic, "layer", None) if uic is not None else None
        util_layer = getattr(layer, "utilmenu", None) if layer is not None else None
        if util_layer is None:
            return
        children = [ch for ch in list(getattr(util_layer, "children", []) or []) if not getattr(ch, "destroyed", False)]
        if not children:
            return
        if node is not None:
            mref = getattr(node, "_menu", None)
            try:
                node_menu = mref() if callable(mref) else mref
            except Exception:
                node_menu = None
            for ch in children:
                if _is_under_widget(node, ch) or getattr(ch, "controller", None) is node or (node_menu is not None and node_menu is ch):
                    return
        ordered = sorted(
            children,
            key=lambda c: 0 if (
                getattr(c, "controller", None) is not None
                or "ExpandedUtilMenu" in getattr(getattr(c, "__class__", None), "__name__", "")
            ) else 1,
        )
        for ch in ordered:
            if getattr(ch, "destroyed", False):
                continue
            try:
                if callable(getattr(ch, "Close", None)):
                    ch.Close()
            except Exception:
                pass
    except Exception:
        pass


def _is_top_level_window_node(node):
    """Return True for top-level windows/stacks, excluding child containers like FormWnd."""
    if node is None:
        return False
    if getattr(node, "isTopLevelWindow", False) or getattr(node, "windowID", None) is not None:
        return True
    wcls = _safe_str(getattr(getattr(node, "__class__", None), "__name__", ""), 64)
    if not wcls or wcls == "FormWnd":
        return False
    if not (wcls == "WindowStack" or wcls.endswith("Window") or wcls.endswith("Wnd")):
        return False
    anc = getattr(node, "parent", None)
    for _ in range(8):
        if anc is None:
            break
        if getattr(anc, "isTopLevelWindow", False) or getattr(anc, "windowID", None) is not None:
            return False
        anc = getattr(anc, "parent", None)
    return True


def _resolve_combo_dropdown(owner):
    try:
        dd_ref = getattr(owner, "_comboDropDown", None)
        return dd_ref() if callable(dd_ref) else dd_ref
    except Exception:
        return None


def _find_combo_owner_for_menu_container(ch):
    """Return the Combo widget that owns a dropdown Container in uicore.layer.menu."""
    if ch is None:
        return None
    candidates = []
    sr = getattr(ch, "sr", None)
    sr_scroll = getattr(sr, "scroll", None) if sr is not None else None
    if sr_scroll is not None:
        candidates.append(sr_scroll)
    for sub in list(getattr(ch, "children", []) or []):
        if sub is not None and sub is not sr_scroll:
            candidates.append(sub)
    for sc in candidates:
        for attr in ("OnKillFocus", "Confirm", "OnUp", "OnDown", "OnSelectionChange"):
            fn = getattr(sc, attr, None)
            if not callable(fn):
                continue
            owner = getattr(fn, "__self__", None)
            if owner is None:
                owner = getattr(fn, "im_self", None)
            if owner is None or getattr(owner, "destroyed", False) or not hasattr(owner, "_comboDropDown"):
                continue
            if _resolve_combo_dropdown(owner) is ch or getattr(owner, "optionMenu", None) is ch:
                return owner
    return None


def _cleanup_combo_dropdown(combo_owner, menu_container=None, preserve_pinned=False):
    """Reset Combo dropdown state cleanly so Combo._Expanded() cannot wedge True. # Fixes #1289"""
    if combo_owner is None:
        return
    ran_cleanup = False
    # Combo.Cleanup(False) calls ClearMenuLayer() (menu.Flush()), so skip it when a
    # pinned tooltip sibling on menu_layer must survive this outside click.
    if not preserve_pinned and callable(getattr(combo_owner, "Cleanup", None)):
        try:
            combo_owner.Cleanup(False)
            ran_cleanup = True
        except Exception:
            ran_cleanup = False
    if menu_container is not None and not getattr(menu_container, "destroyed", False):
        if callable(getattr(menu_container, "Close", None)):
            try:
                menu_container.Close()
            except Exception:
                pass
    dd_obj = _resolve_combo_dropdown(combo_owner)
    if not ran_cleanup or (dd_obj is not None and (dd_obj is menu_container or getattr(dd_obj, "destroyed", False))):
        try:
            combo_owner._comboDropDown = None
        except Exception:
            pass
        if callable(getattr(combo_owner, "CollapseBackground", None)):
            try:
                combo_owner.CollapseBackground()
            except Exception:
                pass


def _reset_stale_combo_dropdown(node):
    """Clear a destroyed Combo dropdown reference before OnClick checks _Expanded()."""
    if node is None or not hasattr(node, "_comboDropDown"):
        return
    dd = _resolve_combo_dropdown(node)
    if dd is not None and getattr(dd, "destroyed", False):
        # Preserve any unrelated live children in uicore.layer.menu.
        _cleanup_combo_dropdown(node, menu_container=dd, preserve_pinned=True)


def _close_context_menus_if_outside(node):
    try:
        uic = _get_uicore()
        layer = getattr(uic, "layer", None) if uic is not None else None
        menu_layer = getattr(layer, "menu", None) if layer is not None else None
        if menu_layer is None:
            return
        children = [ch for ch in list(getattr(menu_layer, "children", []) or []) if not getattr(ch, "destroyed", False)]
        if not children:
            return
        if node is not None:
            for ch in children:
                if _is_under_widget(node, ch):
                    return
        try:
            from carbonui.control.contextMenu.menuUtil import CloseContextMenus
            CloseContextMenus()
        except Exception:
            pass
        remaining = [
            ch for ch in list(getattr(menu_layer, "children", []) or [])
            if not getattr(ch, "destroyed", False)
        ]
        preserve_pinned = bool(
            node is not None and any(getattr(ch, "_evomu_pinned", False) for ch in remaining)
        )
        for ch in remaining:
            if getattr(ch, "destroyed", False):
                continue
            if node is not None and getattr(ch, "_evomu_pinned", False):
                continue
            try:
                if getattr(ch, "_evomu_pinned", False):
                    _close_pinned_tooltip_panel(ch)
                else:
                    combo_owner = _find_combo_owner_for_menu_container(ch)
                    if combo_owner is not None:
                        _cleanup_combo_dropdown(combo_owner, ch, preserve_pinned=preserve_pinned)
                    elif callable(getattr(ch, "Close", None)):
                        ch.Close()
            except Exception:
                pass
    except Exception:
        pass


def _stale_module_button_reason(node, sm=None):
    """Return a non-empty reason when a ModuleButton is stale (Capsule 670 active or item missing from godma invItems)."""
    if node is None:
        return ""
    cls_n = _safe_str(getattr(getattr(node, "__class__", None), "__name__", ""), 48)
    name_n = _safe_str(getattr(node, "name", ""), 64)
    mod_info = getattr(node, "moduleinfo", None)
    if cls_n != "ModuleButton" and not name_n.startswith("ModuleButton") and mod_info is None:
        return ""
    try:
        sm_obj = sm if sm is not None else getattr(__builtin__, "sm", None)
        godma = sm_obj.GetService("godma") if (sm_obj is not None and hasattr(sm_obj, "GetService")) else None
        if godma is None and sm_obj is not None:
            godma = getattr(sm_obj, "services", {}).get("godma")
        state_mgr = getattr(godma, "stateManager", None) if godma is not None else None
        inv_items = getattr(state_mgr, "invItems", None) if state_mgr is not None else None
        if inv_items is None:
            return ""
        sess = getattr(__builtin__, "session", None)
        ship_id = getattr(sess, "shipid", None)
        if ship_id is not None and hasattr(inv_items, "get"):
            ship_item = inv_items.get(ship_id)
            if getattr(ship_item, "typeID", None) == 670:
                return "StaleModuleButton(Capsule ship typeID=670)"
        mod_id = getattr(mod_info, "itemID", None) if mod_info is not None else None
        if mod_id is not None and hasattr(inv_items, "__contains__") and mod_id not in inv_items:
            return "StaleModuleButton(itemID=%s missing from godma.stateManager.invItems)" % (mod_id,)
    except Exception:
        pass
    return ""


def _coerce_optional_int(val):
    if val is None or isinstance(val, bool):
        return None
    if type(val).__name__ in ("int", "long"):
        return int(val)
    return None


def _invoke_dbl_callable(fn):
    try:
        fn()
        return True, None
    except Exception as exc:
        if exc.__class__.__name__ == "UserError":
            _notify_user_error(exc, async_uthread=True)
            return False, _format_user_error(exc)
        raise


def _capture_dbl_click_ui_state(sm_obj):
    wins = []
    modal_count = 0
    has_registry = False
    try:
        uic = _get_uicore()
        reg = getattr(uic, "registry", None) if uic is not None else None
        if reg is not None and hasattr(reg, "GetWindows"):
            has_registry = True
            for w in list(reg.GetWindows() or []):
                if w is None or getattr(w, "destroyed", False) or not _is_visible(w):
                    continue
                wins.append((
                    id(w),
                    _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64),
                    _safe_str(getattr(w, "caption", None) or "", 64),
                ))
        lyr = getattr(uic, "layer", None) if uic is not None else None
        modal_lyr = getattr(lyr, "modal", None) if lyr is not None else None
        for m in list(getattr(modal_lyr, "children", []) or []):
            if m is not None and not getattr(m, "destroyed", False) and _is_visible(m):
                modal_count += 1
    except Exception:
        pass
    sess = getattr(__builtin__, "session", None)
    ship_id = _coerce_optional_int(getattr(sess, "shipid", None)) if sess is not None else None
    view_sig = None
    is_deploying = False
    if sm_obj is not None:
        try:
            svcs = getattr(sm_obj, "services", None)
            vs = svcs.get("viewState") if isinstance(svcs, dict) else None
            if vs is not None:
                sec_vi = getattr(vs, "secondaryInfo", None)
                cur_vi = vs.GetCurrentViewInfo() if hasattr(vs, "GetCurrentViewInfo") else None
                view_sig = (
                    _safe_str(getattr(cur_vi, "name", None) or "", 48),
                    _safe_str(getattr(sec_vi, "name", None) or "", 48),
                )
            dep_svc = svcs.get("structureDeployment") if isinstance(svcs, dict) else None
            if dep_svc is not None:
                is_deploying = bool(getattr(dep_svc, "isDeploying", False) is True)
        except Exception:
            pass
    return {
        "has_registry": has_registry,
        "wins": tuple(wins),
        "modal_count": modal_count,
        "ship_id": ship_id,
        "view_sig": view_sig,
        "is_deploying": is_deploying,
        "flight_seq": _FLIGHT_SEQ,
        "scatter_seq": _SCATTER_SEQ,
    }


def _extract_inventory_entry_info(entry, sr_node=None):
    if entry is None:
        return None
    entry_cls = entry.__class__.__name__
    entry_name = _safe_str(getattr(entry, "name", ""), 64)
    rec = getattr(entry, "rec", None)
    if rec is None and sr_node is not None:
        rec = getattr(sr_node, "rec", None) or getattr(sr_node, "item", None)
    type_id = _coerce_optional_int(getattr(rec, "typeID", None)) if rec is not None else None
    if type_id is None and sr_node is not None:
        type_id = _coerce_optional_int(getattr(sr_node, "typeID", None))
    if (type_id is None or type_id <= 0) and entry_name.startswith("ItemEntry_"):
        suffix = entry_name[len("ItemEntry_"):]
        if suffix.isdigit():
            type_id = int(suffix)
    if type_id is None or type_id <= 0:
        return None
    if not (
        entry_name.startswith("ItemEntry_")
        or entry_cls in ("InvItem", "Item")
        or rec is not None
    ):
        return None
    item_id = _coerce_optional_int(getattr(rec, "itemID", None)) if rec is not None else None
    if item_id is None and sr_node is not None:
        item_id = _coerce_optional_int(getattr(sr_node, "itemID", None))
    if item_id is not None and item_id <= 0:
        item_id = None
    category_id = _coerce_optional_int(getattr(rec, "categoryID", None)) if rec is not None else None
    if category_id is None and sr_node is not None:
        category_id = _coerce_optional_int(getattr(sr_node, "categoryID", None))
    group_id = _coerce_optional_int(getattr(rec, "groupID", None)) if rec is not None else None
    if group_id is None and sr_node is not None:
        group_id = _coerce_optional_int(getattr(sr_node, "groupID", None))
    if category_id is None or group_id is None:
        try:
            import evetypes as _evetypes
            if category_id is None and hasattr(_evetypes, "GetCategoryID"):
                category_id = _coerce_optional_int(_evetypes.GetCategoryID(type_id))
            if group_id is None and hasattr(_evetypes, "GetGroupID"):
                group_id = _coerce_optional_int(_evetypes.GetGroupID(type_id))
        except Exception:
            pass
    is_container = bool(
        (sr_node is not None and getattr(sr_node, "isContainer", False) is True)
        or (getattr(entry, "isContainer", False) is True)
        or (group_id in (12, 340, 448, 649))
    )
    return {
        "type_id": type_id,
        "item_id": item_id,
        "category_id": category_id,
        "group_id": group_id,
        "is_container": is_container,
    }


def _maybe_showinfo_after_silent_item_dblclick(entry, sr_node, pre_state, sm_obj):
    info = _extract_inventory_entry_info(entry, sr_node=sr_node)
    if info is None:
        return None
    cat_id = info["category_id"]
    if cat_id in (6, 9) or info["is_container"]:
        return None
    sess = getattr(__builtin__, "session", None)
    in_space = bool(
        sess is not None
        and _coerce_optional_int(getattr(sess, "solarsystemid", None))
        and not _coerce_optional_int(getattr(sess, "stationid", None))
        and not _coerce_optional_int(getattr(sess, "structureid", None))
    )
    if cat_id == 65 and in_space:
        return None
    post_state = _capture_dbl_click_ui_state(sm_obj)
    state_unchanged = (
        post_state["wins"] == pre_state["wins"]
        and post_state["modal_count"] == pre_state["modal_count"]
        and post_state["ship_id"] == pre_state["ship_id"]
        and post_state["view_sig"] == pre_state["view_sig"]
        and post_state["is_deploying"] == pre_state["is_deploying"]
        and post_state["flight_seq"] == pre_state["flight_seq"]
        and post_state["scatter_seq"] == pre_state["scatter_seq"]
    )
    if not state_unchanged:
        return None
    # Require a window registry to prove no window opened unless this is a docked
    # structure whose InvItem.OnDblClick hits StructureDeployment.Deploy's early return (#1303).
    if not post_state["has_registry"] and not (cat_id == 65 and not in_space):
        return None
    if sm_obj is None:
        return None
    svcs = getattr(sm_obj, "services", None)
    info_svc = svcs.get("info") if isinstance(svcs, dict) else None
    if info_svc is None and hasattr(sm_obj, "GetService"):
        try:
            info_svc = sm_obj.GetService("info")
        except Exception:
            info_svc = None
    if info_svc is None or not hasattr(info_svc, "ShowInfo"):
        return None
    try:
        if info["item_id"] is not None:
            info_svc.ShowInfo(info["type_id"], itemID=info["item_id"])
        else:
            info_svc.ShowInfo(info["type_id"])
    except Exception as exc:
        if exc.__class__.__name__ == "UserError":
            _notify_user_error(exc, async_uthread=True)
            return _format_user_error(exc)
        raise
    after_state = _capture_dbl_click_ui_state(sm_obj)
    if after_state["has_registry"] and after_state["wins"] == post_state["wins"]:
        H._sleep_ms(150)
    return "ShowInfo"


def _run_double_click_action(entry, sr_node, dbl_attr, dbl_fn):
    sm_obj = getattr(__builtin__, "sm", None)
    pre_state = _capture_dbl_click_ui_state(sm_obj)
    ok, err_str = _invoke_dbl_callable(dbl_fn)
    if not ok:
        return err_str
    fallback = _maybe_showinfo_after_silent_item_dblclick(entry, sr_node, pre_state, sm_obj)
    if fallback:
        if fallback.startswith("UserError("):
            return fallback
        return "%s->%s" % (dbl_attr, fallback)
    return dbl_attr


def _invoke_widget_click(node, right_click=False, double_click=False, click_x=None, click_y=None):
    _close_neocom_panels_if_outside(node)
    _close_utilmenu_if_outside(node)
    _close_context_menus_if_outside(node)
    if node is not None:
        for ch in list(getattr(node, "children", []) or []):
            ch_cls = ch.__class__.__name__
            ch_name = _safe_str(getattr(ch, "name", ""), 48)
            if (
                ch_cls in ("ModuleButton", "ShipItemCard")
                or ch_name.startswith("ModuleButton")
                or (node.__class__.__name__ == "SE_BaseClassCore" and ("Card" in ch_cls or ch_name.startswith("ItemEntry_")))
            ):
                node = ch
                break
    stale_mod = _stale_module_button_reason(node)
    if stale_mod:
        return stale_mod
    try:
        uic_xy = _get_uicore()
        uilib = getattr(uic_xy, "uilib", None) if uic_xy is not None else None
        if uilib is not None:
            if click_x is not None and click_y is not None:
                uilib.x = int(click_x)
                uilib.y = int(click_y)
            elif node is not None:
                rx, ry, rw, rh = _widget_abs_rect(node)
                if rw > 0 and rh > 0:
                    uilib.x = int(rx + (rw // 2))
                    uilib.y = int(ry + (rh // 2))
            _set_uilib_mouseover(uilib, node)
            _prime_label_inline_link(node, uilib=uilib, click_x=click_x, click_y=click_y)
    except Exception:
        _clear_py2_exc()
    if _dismiss_pinned_tooltip_if_outside(node, close_if_owner=True):
        return "CloseTooltipPanel(%s)" % (_safe_str(getattr(node, "name", ""), 48),)
    edit_w = _resolve_editable_widget(node)
    if edit_w is not None:
        try:
            uic = _get_uicore()
            reg = getattr(uic, "registry", None) if uic is not None else None
            if reg is not None and hasattr(reg, "SetFocus"):
                reg.SetFocus(edit_w)
        except Exception:
            pass
    if (
        right_click
        or getattr(node, "expandOnLeft", False)
        or _safe_str(getattr(getattr(node, "__class__", None), "__name__", ""), 48) in ("ListSurroundingsBtn", "MenuButtonIcon", "MenuButton")
    ):
        cur = node
        for _ in range(5):
            if cur is None:
                break
            cur_cls = _safe_str(getattr(getattr(cur, "__class__", None), "__name__", ""), 48)
            has_menu = hasattr(cur, "GetMenu") and (
                not hasattr(cur, "HasEventHandler")
                or cur.HasEventHandler("GetMenu")
                or right_click
                or getattr(cur, "expandOnLeft", False)
                or cur_cls in ("ListSurroundingsBtn", "MenuButtonIcon", "MenuButton")
            )
            if has_menu:
                try:
                    if right_click:
                        try:
                            sr = getattr(cur, "sr", None)
                            sr_node = getattr(sr, "node", None) if sr is not None else None
                            scroll = getattr(sr_node, "scroll", None) if sr_node is not None else None
                            if sr_node is not None and scroll is not None and not getattr(sr_node, "selected", False):
                                if hasattr(scroll, "SelectNode"):
                                    scroll.SelectNode(sr_node)
                            elif hasattr(cur, "Select") and not getattr(cur, "isSelected", False):
                                cur.Select()
                        except Exception:
                            pass
                    try:
                        from carbonui.control.contextMenu.menuUtil import CloseContextMenus
                        CloseContextMenus()
                    except Exception:
                        pass
                    from carbonui.control.contextMenu.contextMenu import ShowMenu
                    ShowMenu(cur)
                    try:
                        wx, wy, ww, wh = _widget_abs_rect(cur)
                        uic = _get_uicore()
                        mlayer = getattr(getattr(uic, "layer", None), "menu", None) if uic is not None else None
                        desk = getattr(uic, "desktop", None) if uic is not None else None
                        dw = int(getattr(desk, "width", 1280) or 1280) if desk is not None else 1280
                        dh = int(getattr(desk, "height", 800) or 800) if desk is not None else 800
                        if mlayer is not None and (wx > 0 or wy > 0):
                            for mch in list(getattr(mlayer, "children", []) or []):
                                if not getattr(mch, "destroyed", False) and _safe_str(getattr(mch, "name", ""), 32) == "menuview":
                                    _, _, mw, mh = _widget_abs_rect(mch)
                                    tx = int(click_x) if (click_x is not None and click_x > 0) else int(wx + min(ww // 2, 40))
                                    ty = int(wy + min(wh // 2, 16))
                                    mch.left = max(4, min(max(4, dw - max(mw, 160) - 4), tx))
                                    mch.top = max(4, min(max(4, dh - max(mh, 120) - 4), ty))
                    except Exception:
                        pass
                    return "ShowMenu(%s)" % (_safe_str(getattr(cur, "name", "") or cur_cls, 48),)
                except Exception:
                    show_tb = traceback.format_exc()
                    try:
                        menu = cur.GetMenu()
                        try:
                            import evomu_site as _es_m
                            _es_m._log_exc_line("ShowMenu(%s) failed:\n%s" % (_safe_str(getattr(cur, "name", "") or cur_cls, 48), show_tb))
                        except Exception:
                            pass
                        return "GetMenu(%d)" % (len(menu or []),)
                    except Exception as menu_exc:
                        if menu_exc.__class__.__name__ == "UserError":
                            _notify_user_error(menu_exc, async_uthread=True)
                            return _format_user_error(menu_exc)
                        is_explicit_menu = (
                            right_click
                            or bool(getattr(cur, "expandOnLeft", False))
                            or cur_cls in ("ListSurroundingsBtn", "MenuButtonIcon", "MenuButton")
                        )
                        if is_explicit_menu:
                            raise
                        try:
                            import evomu_site as _es_m
                            _es_m._log_exc_line("ShowMenu(%s) failed:\n%s" % (_safe_str(getattr(cur, "name", "") or cur_cls, 48), show_tb))
                            _es_m._log_exc_line("GetMenu(%s) failed:\n%s" % (_safe_str(getattr(cur, "name", "") or cur_cls, 48), traceback.format_exc()))
                        except Exception:
                            pass
            cur = getattr(cur, "parent", None)
    if not right_click:
        cur_menu = node
        for _ in range(4):
            if cur_menu is None:
                break
            cls_menu = _safe_str(getattr(getattr(cur_menu, "__class__", None), "__name__", ""), 48)
            if "MenuEntry" in cls_menu:
                m_data = getattr(cur_menu, "menuEntryData", None) or getattr(cur_menu, "menu_entry_data", None)
                has_sub = False
                try:
                    if hasattr(cur_menu, "HasSubMenuData") and cur_menu.HasSubMenuData():
                        has_sub = True
                    elif m_data is not None and (
                        (hasattr(m_data, "HasSubMenuData") and m_data.HasSubMenuData())
                        or getattr(m_data, "_subMenuData", None) is not None
                        or getattr(m_data, "subMenuData", None) is not None
                    ):
                        has_sub = True
                except Exception:
                    pass
                if has_sub and hasattr(cur_menu, "ReconstructSubMenuView"):
                    try:
                        uilib_sub = getattr(_get_uicore(), "uilib", None)
                        _set_uilib_mouseover(uilib_sub, cur_menu)
                    except Exception:
                        _clear_py2_exc()
                    try:
                        if hasattr(cur_menu, "_CollapseSiblingSubMenus"):
                            cur_menu._CollapseSiblingSubMenus()
                    except Exception:
                        pass
                    cur_menu.ReconstructSubMenuView()
                    return "ExpandSubMenu(%s)" % (_safe_str(getattr(cur_menu, "name", ""), 48),)
                if m_data is not None and callable(getattr(m_data, "func", None)):
                    try:
                        try:
                            if hasattr(cur_menu, "ExecuteFunction"):
                                cur_menu.ExecuteFunction()
                            else:
                                m_data.ExecuteFunction()
                        except Exception as exc:
                            if exc.__class__.__name__ == "UserError":
                                _notify_user_error(exc, async_uthread=True)
                                return _format_user_error(exc)
                            raise
                    finally:
                        if hasattr(cur_menu, "_OnFunctionCalled"):
                            try:
                                cur_menu._OnFunctionCalled()
                            except Exception:
                                pass
                        try:
                            from carbonui.control.contextMenu.menuUtil import CloseContextMenus
                            CloseContextMenus()
                        except Exception:
                            pass
                    return "ExecuteFunction(%s)" % (_safe_str(getattr(cur_menu, "name", ""), 48),)
            cur_menu = getattr(cur_menu, "parent", None)

        cls_node = _safe_str(getattr(getattr(node, "__class__", None), "__name__", ""), 48)
        if cls_node == "SelectedItemButton":
            dis_hint = _strip_tags(_safe_str(getattr(node, "disabledHint", ""), 80))
            if bool(dis_hint) or (hasattr(node, "IsEnabled") and not node.IsEnabled()):
                return "disabled_selected_item_button(%s)" % (dis_hint or _safe_str(getattr(node, "name", ""), 48),)
            m_data = getattr(node, "menuEntryData", None)
            if m_data is not None and not callable(getattr(m_data, "func", None)):
                cmd_name = _safe_str(getattr(node, "cmdName", ""), 48)
                sel_item_id = getattr(node, "itemID", None)
                if not sel_item_id:
                    cur_p = getattr(node, "parent", None)
                    for _ in range(12):
                        if cur_p is None:
                            break
                        sel_item_id = getattr(cur_p, "itemID", None)
                        if sel_item_id:
                            break
                        cur_p = getattr(cur_p, "parent", None)
                if not sel_item_id:
                    try:
                        from eve.client.script.parklife import states as _park_states
                        st_svc = sm.GetService("stateSvc") if hasattr(sm, "GetService") else None
                        if st_svc is not None and hasattr(st_svc, "GetExclState"):
                            sel_item_id = st_svc.GetExclState(_park_states.selected)
                    except Exception:
                        pass
                if not sel_item_id:
                    try:
                        tgt_svc = sm.GetService("target") if hasattr(sm, "GetService") else None
                        if tgt_svc is not None and hasattr(tgt_svc, "GetActiveTargetID"):
                            sel_item_id = tgt_svc.GetActiveTargetID()
                    except Exception:
                        pass
                if sel_item_id:
                    try:
                        try:
                            sm.ScatterEvent("OnSelectedItemButtonClicked", getattr(node, "uniqueUiName", None) or getattr(node, "name", ""))
                        except Exception:
                            pass
                        from eve.client.script.ui.services.menuSvcExtras import movementFunctions as _mv_fn
                        menu_svc = sm.GetService("menu")
                        if cmd_name == "CmdKeepItemAtRange":
                            _mv_fn.KeepAtRange(sel_item_id)
                            return "movementFunctions.KeepAtRange(%s)" % (sel_item_id,)
                        if cmd_name == "CmdOrbitItem":
                            _mv_fn.Orbit(sel_item_id)
                            return "movementFunctions.Orbit(%s)" % (sel_item_id,)
                        if cmd_name == "CmdApproachItem":
                            _mv_fn.Approach(sel_item_id)
                            return "movementFunctions.Approach(%s)" % (sel_item_id,)
                        if cmd_name == "CmdWarpToItem":
                            _mv_fn.WarpToItem(sel_item_id)
                            return "movementFunctions.WarpToItem(%s)" % (sel_item_id,)
                        if cmd_name == "CmdDockOrJumpOrActivateGate":
                            _mv_fn.DockOrJumpOrActivateGate(sel_item_id)
                            return "movementFunctions.DockOrJumpOrActivateGate(%s)" % (sel_item_id,)
                        if cmd_name == "CmdAlignToItem" and hasattr(menu_svc, "AlignTo"):
                            menu_svc.AlignTo(sel_item_id)
                            return "menu.AlignTo(%s)" % (sel_item_id,)
                        if cmd_name == "CmdLockTargetItem":
                            sm.GetService("target").TryLockTarget(sel_item_id)
                            return "target.TryLockTarget(%s)" % (sel_item_id,)
                    except Exception:
                        pass
                return "disabled_selected_item_button"

        slider_res = _set_slider_from_x(node, click_x)
        if slider_res is not None:
            return slider_res

        try:
            from carbonui.control.contextMenu.menuUtil import CloseContextMenus
            CloseContextMenus()
        except Exception:
            pass

        cur_num_btn = node
        for _ in range(3):
            if cur_num_btn is None:
                break
            bname = _safe_str(getattr(cur_num_btn, "name", ""), 32)
            if bname in ("upButton", "downButton"):
                step_dir = 1 if bname == "upButton" else -1
                cur_num_edit = getattr(cur_num_btn, "parent", None)
                for _ in range(4):
                    if cur_num_edit is None:
                        break
                    if hasattr(cur_num_edit, "ChangeNumericValue"):
                        try:
                            cur_num_edit.ChangeNumericValue(step_dir)
                            if hasattr(cur_num_edit, "OnChange") and hasattr(cur_num_edit, "GetText"):
                                cur_num_edit.OnChange(cur_num_edit.GetText())
                            return "ChangeNumericValue(%d)" % (step_dir,)
                        except Exception:
                            pass
                    cur_num_edit = getattr(cur_num_edit, "parent", None)
                break
            cur_num_btn = getattr(cur_num_btn, "parent", None)

        cur_entry = node
        for _ in range(4):
            if cur_entry is None:
                break
            sr_obj = getattr(cur_entry, "sr", None)
            sr_node = getattr(sr_obj, "node", None) if sr_obj is not None else None
            if sr_node is not None:
                if hasattr(cur_entry, "OnMouseDown"):
                    try:
                        cur_entry.OnMouseDown()
                    except Exception:
                        pass
                item_id = getattr(sr_node, "itemID", None)
                if item_id is not None:
                    try:
                        sm_obj = getattr(__builtin__, "sm", None)
                        st_svc = sm_obj.GetService("stateSvc") if sm_obj is not None else None
                        if st_svc is not None and hasattr(st_svc, "SetState"):
                            from eve.client.script.parklife import states as _park_states
                            st_svc.SetState(item_id, _park_states.selected, 1)
                    except Exception:
                        pass
                if double_click:
                    try:
                        scroll = getattr(sr_node, "scroll", None)
                        if scroll is not None and not getattr(sr_node, "selected", False):
                            if hasattr(scroll, "SelectNode"):
                                scroll.SelectNode(sr_node)
                        elif hasattr(cur_entry, "Select") and not getattr(cur_entry, "isSelected", False):
                            cur_entry.Select()
                    except Exception:
                        pass
                    for dbl_attr in ("OnDblClick", "on_double_click", "DoubleClick"):
                        dbl_fn = getattr(cur_entry, dbl_attr, None)
                        if callable(dbl_fn):
                            return _run_double_click_action(cur_entry, sr_node, dbl_attr, dbl_fn)
                    ctrl_entry = getattr(cur_entry, "controller", None)
                    if ctrl_entry is not None and callable(getattr(ctrl_entry, "on_double_click", None)):
                        return _run_double_click_action(
                            cur_entry,
                            sr_node,
                            "controller.on_double_click",
                            ctrl_entry.on_double_click,
                        )
                break
            cur_entry = getattr(cur_entry, "parent", None)

        if double_click:
            cur_dbl = node
            for _ in range(4):
                if cur_dbl is None:
                    break
                for dbl_attr in ("OnDblClick", "on_double_click", "DoubleClick"):
                    dbl_fn = getattr(cur_dbl, dbl_attr, None)
                    if callable(dbl_fn):
                        return _run_double_click_action(cur_dbl, None, dbl_attr, dbl_fn)
                ctrl_dbl = getattr(cur_dbl, "controller", None)
                if ctrl_dbl is not None and callable(getattr(ctrl_dbl, "on_double_click", None)):
                    return _run_double_click_action(
                        cur_dbl,
                        None,
                        "controller.on_double_click",
                        ctrl_dbl.on_double_click,
                    )
                cur_dbl = getattr(cur_dbl, "parent", None)

        icon_btn = getattr(node, "iconBtn", None)
        if icon_btn is None:
            for ch in list(getattr(node, "children", []) or []):
                if getattr(ch, "name", "") == "iconBtn":
                    icon_btn = ch
                    break
                for gch in list(getattr(ch, "children", []) or []):
                    if getattr(gch, "name", "") == "iconBtn":
                        icon_btn = gch
                        break
        if icon_btn is not None and icon_btn is not node and hasattr(icon_btn, "OnClick"):
            try:
                icon_btn.OnClick()
                return "iconBtn.OnClick"
            except Exception:
                pass

    raised_wnd_name = None
    if not right_click:
        cur_w = node
        for _ in range(14):
            if cur_w is None:
                break
            wcls = _safe_str(getattr(getattr(cur_w, "__class__", None), "__name__", ""), 64)
            # Fixes #1289: skip child layout containers like FormWnd so SetOrder(0) does not
            # move TOALL containers ahead of TOTOP/TOBOTTOM siblings inside the parent Window.
            if _is_top_level_window_node(cur_w):
                try:
                    if hasattr(cur_w, "SetOrder"):
                        cur_w.SetOrder(0)
                    uic = _get_uicore()
                    reg = getattr(uic, "registry", None) if uic is not None else None
                    if reg is not None and hasattr(reg, "SetFocus"):
                        reg.SetFocus(cur_w)
                    raised_wnd_name = _safe_str(getattr(cur_w, "windowID", None) or getattr(cur_w, "name", "") or wcls, 64)
                except Exception:
                    pass
                break
            cur_w = getattr(cur_w, "parent", None)

    uic_pre = _get_uicore()
    cmd_pre = getattr(uic_pre, "cmd", None) if uic_pre is not None else None
    if cmd_pre is not None and getattr(cmd_pre, "combatFunctionLoaded", None) is not None and hasattr(cmd_pre, "UnloadCombatCommand"):
        try:
            cmd_pre.UnloadCombatCommand()
        except Exception:
            pass
    if hasattr(node, "selecting") and getattr(node, "selecting", 0):
        try:
            node.selecting = 0
        except Exception:
            pass
    if not right_click and not double_click and _is_tooltip_panel_trigger(node):
        tp_res = _trigger_tooltip_panel(node)
        if tp_res:
            return tp_res
    _clicked_pinned_tp = None
    if not right_click and not double_click and node is not None:
        try:
            _node_mro = [c.__name__ for c in getattr(getattr(node, "__class__", None), "__mro__", ())]
            if any("ButtonTooltipRow" in _cn for _cn in _node_mro):
                _u_tp = _get_uicore()
                _ul_tp = getattr(_u_tp, "uilib", None) if _u_tp is not None else None
                _th_tp = getattr(_ul_tp, "tooltipHandler", None) if _ul_tp is not None else None
                _tp_cand = getattr(_th_tp, "tooltipPanel", None) if _th_tp is not None else None
                if _tp_cand is not None and not getattr(_tp_cand, "destroyed", False) and getattr(_tp_cand, "_evomu_pinned", False) and _is_under_widget(node, _tp_cand):
                    _clicked_pinned_tp = _tp_cand
                else:
                    _lyr_tp = getattr(_u_tp, "layer", None) if _u_tp is not None else None
                    _ml_tp = getattr(_lyr_tp, "menu", None) if _lyr_tp is not None else None
                    for _ch_tp in list(getattr(_ml_tp, "children", []) or []):
                        if not getattr(_ch_tp, "destroyed", False) and getattr(_ch_tp, "_evomu_pinned", False) and _is_under_widget(node, _ch_tp):
                            _clicked_pinned_tp = _ch_tp
                            break
        except Exception:
            _clicked_pinned_tp = None
    if not right_click:
        _reset_stale_combo_dropdown(node)
    click_attrs = (
        ("OnDblClick", "on_double_click", "DoubleClick", "Click", "OnClick", "on_click", "OnMouseDown")
        if double_click
        else ("Click", "OnClick", "on_click", "OnMouseDown")
    )
    for attr_name in click_attrs:
        fn = getattr(node, attr_name, None)
        if callable(fn):
            try:
                fn()
            except Exception as exc:
                if _clicked_pinned_tp is not None:
                    _close_pinned_tooltip_panel(_clicked_pinned_tp)
                if exc.__class__.__name__ == "UserError":
                    _notify_user_error(exc, async_uthread=True)
                    return _format_user_error(exc)
                raise
            finally:
                if hasattr(node, "selecting") and getattr(node, "selecting", 0):
                    try:
                        node.selecting = 0
                    except Exception:
                        pass
            if _clicked_pinned_tp is not None:
                _close_pinned_tooltip_panel(_clicked_pinned_tp)
            if cmd_pre is not None and getattr(cmd_pre, "combatFunctionLoaded", None) is not None and hasattr(cmd_pre, "UnloadCombatCommand"):
                try:
                    cmd_pre.UnloadCombatCommand()
                except Exception:
                    pass
                return "NeedsLockedTarget(%s)" % (attr_name,)
            return attr_name
        elif isinstance(fn, tuple) and len(fn) >= 1 and callable(fn[0]):
            try:
                fn[0](*fn[1:])
            finally:
                if _clicked_pinned_tp is not None:
                    _close_pinned_tooltip_panel(_clicked_pinned_tp)
            return "%s(tuple)" % (attr_name,)
    if raised_wnd_name:
        return "RaiseWindow(%s)" % (raised_wnd_name,)
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
    has_warp_cache = bool(space_svc is not None and getattr(space_svc, "warpDestinationCache", None) is not None)
    ego_type_id = 0
    ego_group_id = 0
    ego_dmg = None
    if bp is not None and sid:
        try:
            ego_slim = (getattr(bp, "slimItems", {}) or {}).get(sid)
            if ego_slim is not None:
                ego_type_id = int(getattr(ego_slim, "typeID", 0) or 0)
                ego_group_id = int(getattr(ego_slim, "groupID", 0) or 0)
        except Exception:
            pass
        try:
            if hasattr(bp, "GetDamageState"):
                ds = bp.GetDamageState(sid)
                if isinstance(ds, (tuple, list)) and len(ds) >= 3 and ds[0] is not None:
                    ego_dmg = {
                        "shield": round(float(ds[0]), 3),
                        "armor": round(float(ds[1]), 3),
                        "hull": round(float(ds[2]), 3),
                    }
        except Exception:
            pass
    res = {
        "mode": mode,
        "speed_mps": round(speed, 2),
        "type_id": ego_type_id,
        "group_id": ego_group_id,
        "fx_activations": [str(x) for x in fx_names],
        "has_warp_dest_cache": has_warp_cache,
        "signal_in_warp": bool(_WARP_TELEMETRY["in_warp"]),
    }
    if ego_dmg is not None:
        res["damage_state"] = ego_dmg
    return res


def _resolve_ball_name(slim, bid):
    raw_bname = getattr(slim, "name", None) or getattr(slim, "itemName", "")
    try:
        cfg_g = getattr(__builtin__, "cfg", None)
        if not raw_bname and cfg_g is not None and hasattr(cfg_g, "evelocations"):
            loc_o = cfg_g.evelocations.GetIfExists(int(bid))
            if loc_o is not None:
                raw_bname = getattr(loc_o, "name", None) or getattr(loc_o, "locationName", "")
        if int(getattr(slim, "groupID", 0) or 0) == 10 and cfg_g is not None and hasattr(cfg_g, "evelocations"):
            jumps_l = getattr(slim, "jumps", None)
            if jumps_l and len(jumps_l) > 0:
                dst_loc = getattr(jumps_l[0], "locationID", None) or getattr(jumps_l[0], "toCelestialID", None)
                if dst_loc:
                    dloc_o = cfg_g.evelocations.GetIfExists(int(dst_loc))
                    dname = (getattr(dloc_o, "name", None) or getattr(dloc_o, "locationName", "")) if dloc_o is not None else ""
                    if dname and _to_utf8(dname).lower() not in _to_utf8(raw_bname or "").lower():
                        raw_bname = "Stargate (%s)" % (_to_utf8(dname),)
    except Exception:
        pass
    return _to_utf8(raw_bname or "")


def _resolve_ball_surface_dist(bp, sid, bid, ball=None, now_sim=None, ego_ball=None):
    b = ball if ball is not None else (bp.GetBall(bid) if hasattr(bp, "GetBall") else None)
    if b is None:
        return 0.0
    if hasattr(bp, "GetSurfaceDist") and sid:
        try:
            sd = bp.GetSurfaceDist(int(sid), int(bid))
            if sd is not None:
                return max(0.0, float(sd))
        except Exception:
            pass
    try:
        if now_sim is not None and hasattr(b, "GetVectorAt"):
            b.GetVectorAt(now_sim)
    except Exception:
        pass
    sd2 = getattr(b, "surfaceDist", None)
    if sd2 is not None and float(sd2) >= 0.0:
        return float(sd2)
    eb = ego_ball if ego_ball is not None else (bp.GetBall(sid) if (sid and hasattr(bp, "GetBall")) else None)
    if eb is not None:
        dx = float(getattr(b, "x", 0.0) or 0.0) - float(getattr(eb, "x", 0.0) or 0.0)
        dy = float(getattr(b, "y", 0.0) or 0.0) - float(getattr(eb, "y", 0.0) or 0.0)
        dz = float(getattr(b, "z", 0.0) or 0.0) - float(getattr(eb, "z", 0.0) or 0.0)
        r_sum = float(getattr(b, "radius", 0.0) or 0.0) + float(getattr(eb, "radius", 0.0) or 0.0)
        return max(0.0, ((dx * dx + dy * dy + dz * dz) ** 0.5) - r_sum)
    return 0.0


_STARGATE_SEC_SUFFIX_RE = re.compile(r"\s+-?[01]\.\d$")
_STARGATE_PAREN_RE = re.compile(r"\(([^()]+)\)")


def _extract_stargate_dest(slim, bname):
    """Extract lowercase destination name(s) and destination solarSystemID string for a stargate."""
    dest_names = []
    dst_sys_id = ""
    bname_s = _to_utf8(bname or "").strip()
    m = _STARGATE_PAREN_RE.search(bname_s)
    if m:
        inner = m.group(1).strip().lower()
        if inner and inner not in dest_names:
            dest_names.append(inner)
    try:
        cfg_g = getattr(__builtin__, "cfg", None)
        jumps_l = getattr(slim, "jumps", None) if slim is not None else None
        if jumps_l and len(jumps_l) > 0:
            dst_loc = getattr(jumps_l[0], "locationID", None) or getattr(jumps_l[0], "toCelestialID", None)
            if dst_loc:
                dst_sys_id = str(int(dst_loc))
                if cfg_g is not None and hasattr(cfg_g, "evelocations"):
                    dloc_o = cfg_g.evelocations.GetIfExists(int(dst_loc))
                    dname = (getattr(dloc_o, "name", None) or getattr(dloc_o, "locationName", "")) if dloc_o is not None else ""
                    dname_lc = _to_utf8(dname or "").strip().lower()
                    if dname_lc and dname_lc not in dest_names:
                        dest_names.append(dname_lc)
    except Exception:
        pass
    return dest_names, dst_sys_id


def _match_stargate_target_tier(slim, gname, q_lc, raw_lc=""):
    """Rank a stargate candidate against query text without matching substrings of the literal 'Stargate' prefix."""
    gname_lc = _to_utf8(gname or "").strip().lower()
    q_lc = _to_utf8(q_lc or "").strip().lower()
    raw_lc = _to_utf8(raw_lc or "").strip().lower()
    if not q_lc:
        return None
    if gname_lc and (gname_lc == q_lc or (raw_lc and gname_lc == raw_lc)):
        return 0

    dest_names, dst_sys_id = _extract_stargate_dest(slim, gname_lc)
    q_cands = [q_lc]
    q_no_sec = _STARGATE_SEC_SUFFIX_RE.sub("", q_lc).strip()
    if q_no_sec and q_no_sec not in q_cands:
        q_cands.append(q_no_sec)
    exact_only_dest = q_lc.startswith("stargate (") and q_lc.endswith(")")
    if q_lc.startswith("stargate ("):
        inner_q = q_lc[len("stargate ("):].rstrip(")").strip()
        if inner_q and inner_q not in q_cands:
            q_cands.append(inner_q)
        # An explicit 'stargate (<dest>...)' query should match <dest>, not the generic prefix or wrong destinations.
        q_cands = [c for c in q_cands if not c.startswith("stargate (")]

    for idx, qc in enumerate(q_cands):
        if dst_sys_id and qc == dst_sys_id:
            return 0 if idx == 0 else 1
        if qc and any(d == qc for d in dest_names):
            return 0 if idx == 0 else 1
    if exact_only_dest:
        return None
    for qc in q_cands:
        if qc and any(d.startswith(qc) for d in dest_names):
            return 2
    for qc in q_cands:
        if qc and any(qc in d for d in dest_names):
            return 3
    if not dest_names and gname_lc and (q_lc in gname_lc or (raw_lc and raw_lc in gname_lc)):
        return 4
    if ("stargate" in q_lc or "gate" in q_lc) and not q_lc.startswith("stargate (") and gname_lc and q_lc in gname_lc:
        return 4
    return None


def _resolve_space_command_item_id(bp, sid, sub, query_target):
    q_raw = _to_utf8(query_target or "").strip()
    slims = getattr(bp, "slimItems", {}) or {}
    if not q_raw:
        for bid, slim in list(slims.items()):
            if int(bid) == sid:
                continue
            bdist = _resolve_ball_surface_dist(bp, sid, bid)
            if sub == "warp_to" and bdist > 250000.0:
                return int(bid), None
            elif sub == "dock" and int(getattr(slim, "categoryID", 0) or 0) in (3, 65):
                return int(bid), None
            elif sub not in ("warp_to", "dock", "jump_gate") and bdist < 200000.0:
                return int(bid), None
        return 0, None

    q_name = q_raw
    if "|" in q_raw:
        parts = [p.strip() for p in q_raw.split("|") if p.strip()]
        if len(parts) >= 2:
            q_name = parts[1]
    q_type_str = None
    if q_name.startswith("scrollentry_") and q_name[12:].isdigit():
        q_type_str = q_name[12:]
    elif q_name.isdigit():
        q_type_str = q_name
    q_lc = q_name.lower()
    raw_lc = q_raw.lower()

    best_key = None
    best_bid = 0
    for bid, slim in list(slims.items()):
        if int(bid) == sid:
            continue
        bname = _resolve_ball_name(slim, bid)
        bname_lc = bname.lower()
        btype = str(int(getattr(slim, "typeID", 0) or 0))
        bgroup = int(getattr(slim, "groupID", 0) or 0)
        bcat = int(getattr(slim, "categoryID", 0) or 0)
        tier = None
        if bgroup == 10 or bname_lc.startswith("stargate ("):
            st_tier = _match_stargate_target_tier(slim, bname, q_lc, raw_lc)
            if st_tier is not None:
                tier = 0 if st_tier <= 1 else st_tier
            elif q_type_str and btype == q_type_str:
                tier = 1
        elif bname_lc and (bname_lc == q_lc or bname_lc == raw_lc):
            tier = 0
        elif q_type_str and btype == q_type_str:
            tier = 1
        elif bname_lc and (q_lc in bname_lc or bname_lc in raw_lc):
            tier = 2
        if tier is None:
            continue
        bdist = _resolve_ball_surface_dist(bp, sid, bid)
        if sub == "warp_to":
            suitability = 0 if bdist > 150000.0 else 1
        elif sub == "dock":
            suitability = 0 if bcat in (3, 65) else 1
        elif sub in ("lock_target", "approach", "orbit", "keep_at_range"):
            suitability = 0 if bdist < 200000.0 else 1
        else:
            suitability = 0
        cand_key = (tier, suitability, bdist, int(bid))
        if best_key is None or cand_key < best_key:
            best_key = cand_key
            best_bid = int(bid)

    if best_bid:
        return best_bid, None
    return 0, "No ballpark object matched %s target %r" % (sub, q_raw)


def _get_state_snapshot(sm):
    global _LAST_OBSERVED_SEQ
    _ensure_windowed_mode(sm)
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

    locked_targets = []
    try:
        t_svc = getattr(sm, "services", {}).get("target")
        if t_svc is not None and hasattr(t_svc, "GetTargets"):
            locked_targets = [int(x) for x in list(t_svc.GetTargets() or [])[:20]]
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
                now_sim = None
                try:
                    now_sim = blue.os.GetSimTime()
                except Exception:
                    now_sim = None
                ego_ball = bp.GetBall(sid) if sid else None
                if ego_ball is not None and now_sim is not None and hasattr(ego_ball, "GetVectorAt"):
                    try:
                        ego_ball.GetVectorAt(now_sim)
                    except Exception:
                        pass
                slims = getattr(bp, "slimItems", {}) or {}
                for bid, slim in list(slims.items()):
                    if int(bid) == int(sid):
                        continue
                    b = bp.GetBall(bid)
                    dist = _resolve_ball_surface_dist(bp, sid, bid, ball=b, now_sim=now_sim, ego_ball=ego_ball)
                    raw_bname = _resolve_ball_name(slim, bid)
                    ball_entry = {
                        "item_id": int(bid),
                        "type_id": int(getattr(slim, "typeID", 0) or 0),
                        "group_id": int(getattr(slim, "groupID", 0) or 0),
                        "category_id": int(getattr(slim, "categoryID", 0) or 0),
                        "name": _safe_str(raw_bname, 48),
                        "distance_m": round(dist, 1),
                        "mode": int(getattr(b, "mode", -1)) if b is not None else -1,
                    }
                    try:
                        if hasattr(bp, "GetDamageState"):
                            b_ds = bp.GetDamageState(int(bid))
                            if isinstance(b_ds, (tuple, list)) and len(b_ds) >= 3 and b_ds[0] is not None:
                                sh, ar, hu = round(float(b_ds[0]), 3), round(float(b_ds[1]), 3), round(float(b_ds[2]), 3)
                                if sh < 1.0 or ar < 1.0 or hu < 1.0 or int(bid) in locked_targets:
                                    ball_entry["damage_state"] = {"shield": sh, "armor": ar, "hull": hu}
                    except Exception:
                        pass
                    nearby.append(ball_entry)
                nearby.sort(key=lambda x: x["distance_m"])
                stargates = [b for b in nearby if int(b.get("group_id") or 0) == 10][:10]
                stations = [b for b in nearby if int(b.get("category_id") or 0) in (3, 65)][:8]
                keep_ids = set(b["item_id"] for b in stargates) | set(b["item_id"] for b in stations)
                others = [b for b in nearby if b["item_id"] not in keep_ids][:22]
                preserved = stargates + stations + others
                preserved.sort(key=lambda x: x["distance_m"])
                nearby = preserved
        except Exception:
            pass
        space_info["nearby_balls"] = nearby[:38]
        selected_id = None
        try:
            st_svc = getattr(sm, "services", {}).get("stateSvc")
            if st_svc is not None and hasattr(st_svc, "GetExclState"):
                from eve.client.script.parklife import states as _park_states
                selected_id = st_svc.GetExclState(_park_states.selected)
        except Exception:
            selected_id = None
        if selected_id is not None:
            try:
                sel_id_i = int(selected_id)
                sel_ball = next((b for b in nearby if int(b.get("item_id", 0)) == sel_id_i), None)
                space_info["selected_item"] = dict(sel_ball) if sel_ball is not None else {"item_id": sel_id_i}
            except Exception:
                pass
        overview_entries = []
        try:
            uc_ov = _get_uicore()
            reg_ov = getattr(uc_ov, "registry", None) if uc_ov is not None else None
            wins_ov = list(reg_ov.GetWindows() or []) if (reg_ov is not None and hasattr(reg_ov, "GetWindows")) else []
            for ov_w in wins_ov:
                if getattr(ov_w, "destroyed", False) or not _is_visible(ov_w):
                    continue
                if "Overview" not in ov_w.__class__.__name__ and "overview" not in str(getattr(ov_w, "windowID", "") or "").lower():
                    continue
                scroll_ov = getattr(ov_w, "scroll", None)
                if scroll_ov is not None and hasattr(scroll_ov, "GetNodes"):
                    for nd in list(scroll_ov.GetNodes() or [])[:40]:
                        bid_ov = getattr(nd, "itemID", None)
                        slim_ov = getattr(nd, "slimItem", None) or getattr(nd, "ball", None)
                        if bid_ov is None and slim_ov is not None:
                            bid_ov = getattr(slim_ov, "itemID", None)
                        gid_ov = int(getattr(slim_ov, "groupID", 0) or 0) if slim_ov is not None else 0
                        cid_ov = int(getattr(slim_ov, "categoryID", 0) or 0) if slim_ov is not None else 0
                        name_ov = _strip_tags(_safe_str(getattr(nd, "label", "") or getattr(nd, "name", "") or _resolve_ball_name(slim_ov, bid_ov), 64))
                        dist_ov = getattr(nd, "distance", None)
                        if bid_ov is not None or name_ov:
                            overview_entries.append({
                                "item_id": int(bid_ov) if bid_ov is not None else None,
                                "name": name_ov,
                                "group_id": gid_ov,
                                "category_id": cid_ov,
                                "distance_m": round(float(dist_ov), 1) if isinstance(dist_ov, (int, float)) else None,
                            })
        except Exception:
            pass
        space_info["overview_entries"] = overview_entries[:30]
        space_info["overview_count"] = len(overview_entries)
        space_info["celestials_in_overview"] = any(
            int(b.get("group_id") or 0) in (6, 7, 8, 9, 10, 15)
            for b in (overview_entries or nearby)
        )

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
            trained_levels = {}
            for stid, srec in smap.items():
                sp = getattr(srec, "trainedSkillPoints", None) or getattr(srec, "skillPoints", 0.0) or 0.0
                tot_sp += float(sp)
                lvl = getattr(srec, "trainedSkillLevel", None)
                if lvl is None:
                    lvl = getattr(srec, "skillLevel", None)
                if lvl is None:
                    lvl = getattr(srec, "effectiveSkillLevel", 0)
                try:
                    trained_levels[str(int(stid))] = int(lvl or 0)
                except Exception:
                    pass
            skills_state["total_sp"] = round(tot_sp, 1)
            skills_state["trained_skill_levels"] = trained_levels
    except Exception:
        pass

    wallet_state = {"isk_balance": None}
    try:
        w_svc = sm.GetService("wallet") if has_char else getattr(sm, "services", {}).get("wallet")
        if w_svc is not None and hasattr(w_svc, "GetWealth"):
            wallet_state["isk_balance"] = round(float(w_svc.GetWealth() or 0.0), 2)
    except Exception:
        pass

    inventory_state = {
        "hangar_item_count": None,
        "hangar_total_quantity": None,
        "hangar_non_ship_count": None,
        "hangar_non_ship_total_quantity": None,
        "cargo_item_count": None,
        "cargo_total_quantity": None,
        "ship_hangar_count": None,
        "ship_hangar_total_quantity": None,
        "hangar_containers_seen": None,
    }
    try:
        ic_svc = getattr(sm, "services", {}).get("invCache")
        invs = getattr(ic_svc, "inventories", None) if ic_svc is not None else None
        if isinstance(invs, dict):
            ship_id_i = sess_dict.get("shipid")
            st_locs = {x for x in (sess_dict.get("stationid"), sess_dict.get("structureid")) if x}

            def _item_category_id(rec):
                val = rec.get("categoryID") if isinstance(rec, dict) else getattr(rec, "categoryID", None)
                try:
                    return int(val) if val is not None else None
                except Exception:
                    return None

            def _item_flag_id(rec):
                val = rec.get("flagID") if isinstance(rec, dict) else getattr(rec, "flagID", None)
                if val is None:
                    val = rec.get("flag") if isinstance(rec, dict) else getattr(rec, "flag", None)
                try:
                    return int(val) if val is not None else None
                except Exception:
                    return None

            def _item_id(rec):
                val = rec.get("itemID") if isinstance(rec, dict) else getattr(rec, "itemID", None)
                try:
                    return int(val) if val is not None else None
                except Exception:
                    return None

            def _item_qty(rec):
                val = rec.get("stacksize") if isinstance(rec, dict) else getattr(rec, "stacksize", None)
                if val is None:
                    val = rec.get("quantity") if isinstance(rec, dict) else getattr(rec, "quantity", None)
                try:
                    return max(0, int(val)) if val is not None else 1
                except Exception:
                    return 1

            explicit_cargo_seen = False
            explicit_cargo_items = []
            unified_cargo_seen = False
            unified_cargo_items = []
            hangar_seen = False
            hangar_items = []
            hangar_containers = set()

            for ik, iv in invs.items():
                items_attr = getattr(iv, "items", None)
                cnt = len(items_attr) if isinstance(items_attr, (dict, list, tuple, set)) else None
                if cnt is None and hasattr(iv, "GetItems"):
                    continue
                if isinstance(ik, tuple) and len(ik) >= 1:
                    loc_k = ik[0]
                    flag_k = ik[1] if len(ik) > 1 else None
                elif hasattr(iv, "itemID"):
                    loc_k = getattr(iv, "itemID", None)
                    flag_k = getattr(iv, "flag", None)
                    if flag_k is None:
                        flag_k = getattr(iv, "locationFlag", None)
                else:
                    continue
                item_vals = list(items_attr.values()) if isinstance(items_attr, dict) else (
                    list(items_attr) if isinstance(items_attr, (list, tuple, set)) else []
                )
                if ship_id_i is not None and loc_k == ship_id_i:
                    if flag_k == 5:
                        explicit_cargo_seen = True
                        for it in item_vals:
                            if _item_flag_id(it) in (None, 5):
                                explicit_cargo_items.append(it)
                    elif flag_k is None:
                        unified_cargo_seen = True
                        for it in item_vals:
                            if _item_flag_id(it) == 5:
                                unified_cargo_items.append(it)
                elif (loc_k in st_locs or loc_k == 10004) and flag_k in (4, None):
                    hangar_seen = True
                    if loc_k == 10004:
                        hangar_containers.add("StationItems")
                    if loc_k in st_locs:
                        hangar_containers.add("StationHangar")
                    for it in item_vals:
                        if flag_k == 4 and _item_flag_id(it) in (None, 4):
                            hangar_items.append(it)
                        elif flag_k is None and _item_flag_id(it) == 4:
                            hangar_items.append(it)

            def _dedup_inv_items(raw_list):
                deduped = []
                seen_ids = set()
                for it in raw_list:
                    iid = _item_id(it)
                    if iid is not None:
                        if iid in seen_ids:
                            continue
                        seen_ids.add(iid)
                    deduped.append(it)
                return deduped

            if explicit_cargo_seen:
                cargo_list = _dedup_inv_items(explicit_cargo_items)
                inventory_state["cargo_item_count"] = len(cargo_list)
                inventory_state["cargo_total_quantity"] = sum(_item_qty(it) for it in cargo_list)
            elif unified_cargo_seen:
                cargo_list = _dedup_inv_items(unified_cargo_items)
                inventory_state["cargo_item_count"] = len(cargo_list)
                inventory_state["cargo_total_quantity"] = sum(_item_qty(it) for it in cargo_list)

            if hangar_seen:
                hangar_list = _dedup_inv_items(hangar_items)
                non_ship_list = [it for it in hangar_list if _item_category_id(it) != 6]
                ship_list = [it for it in hangar_list if _item_category_id(it) == 6]
                inventory_state["hangar_item_count"] = len(hangar_list)
                inventory_state["hangar_total_quantity"] = sum(_item_qty(it) for it in hangar_list)
                inventory_state["hangar_non_ship_count"] = len(non_ship_list)
                inventory_state["hangar_non_ship_total_quantity"] = sum(_item_qty(it) for it in non_ship_list)
                inventory_state["ship_hangar_count"] = len(ship_list)
                inventory_state["ship_hangar_total_quantity"] = sum(_item_qty(it) for it in ship_list)
                inventory_state["hangar_containers_seen"] = sorted(hangar_containers)
    except Exception:
        pass

    clone_state = {"home_station_id": None, "jump_clone_count": None}
    try:
        svcs_map = getattr(sm, "services", {}) if sm is not None else {}
        jc_svc = svcs_map.get("jumpCloneSvc") or svcs_map.get("clonejump")
        if jc_svc is not None:
            for attr in ("homeStationID", "_homeStationID", "home_station_id"):
                hv = getattr(jc_svc, attr, None)
                if hv:
                    clone_state["home_station_id"] = int(hv)
                    break
            c_state = getattr(jc_svc, "cloneState", None) or getattr(jc_svc, "_cloneState", None)
            if c_state is not None:
                if clone_state["home_station_id"] is None:
                    hv = (
                        c_state.get("homeStationID")
                        if isinstance(c_state, dict)
                        else getattr(c_state, "homeStationID", None)
                    )
                    if hv:
                        clone_state["home_station_id"] = int(hv)
                jc_list = (
                    c_state.get("clones")
                    if isinstance(c_state, dict)
                    else getattr(c_state, "clones", None)
                )
                if isinstance(jc_list, (list, tuple, dict, set)):
                    clone_state["jump_clone_count"] = len(jc_list)
            if clone_state["jump_clone_count"] is None:
                for cattr in ("jumpClones", "_jumpClones", "clones", "_clones"):
                    jc_val = getattr(jc_svc, cattr, None)
                    if isinstance(jc_val, (list, tuple, dict, set)):
                        clone_state["jump_clone_count"] = len(jc_val)
                        break
    except Exception:
        pass

    market_state = {"char_orders_count": None}
    try:
        mq_svc = getattr(sm, "services", {}).get("marketQuote")
        if mq_svc is not None:
            for attr in ("myOrders", "_myOrders", "orders"):
                val = getattr(mq_svc, attr, None)
                if isinstance(val, (list, tuple, dict, set)):
                    market_state["char_orders_count"] = len(val)
                    break
    except Exception:
        pass

    bookmarks_state = {"count": None, "names": [], "items": []}
    try:
        bm_svc = getattr(sm, "services", {}).get("bookmarkSvc")
        if bm_svc is not None:
            bmap = getattr(bm_svc, "bookmarkMap", None)
            if not isinstance(bmap, dict) and hasattr(bm_svc, "GetBookmarks"):
                bmap = bm_svc.GetBookmarks()
            if isinstance(bmap, dict):
                bm_items = []
                for bmid, bm in list(bmap.items())[:50]:
                    bm_memo = _safe_str(getattr(bm, "memo", None) or getattr(bm, "name", "") or "", 64)
                    bm_items.append({"bookmark_id": int(bmid) if str(bmid).isdigit() else str(bmid), "name": bm_memo})
                bookmarks_state["count"] = len(bmap)
                bookmarks_state["items"] = bm_items
                bookmarks_state["names"] = [b["name"] for b in bm_items if b.get("name")]
    except Exception:
        pass

    autopilot_state = {"active": False, "waypoints": [], "route": [], "route_len": 0}
    try:
        ap_svc = getattr(sm, "services", {}).get("autoPilot")
        if ap_svc is not None:
            if hasattr(ap_svc, "GetState"):
                autopilot_state["active"] = bool(ap_svc.GetState())
            elif hasattr(ap_svc, "InAutoPilot"):
                autopilot_state["active"] = bool(ap_svc.InAutoPilot())
        starmap_svc = getattr(sm, "services", {}).get("starmap")
        if starmap_svc is not None:
            if hasattr(starmap_svc, "GetWaypoints"):
                wps = list(starmap_svc.GetWaypoints() or [])
                autopilot_state["waypoints"] = [int(w) for w in wps if str(w).isdigit()]
            if hasattr(starmap_svc, "GetDestinationPath"):
                path_ids = list(starmap_svc.GetDestinationPath() or [])
                autopilot_state["route"] = [int(x) for x in path_ids if str(x).isdigit()]
                autopilot_state["route_len"] = len(autopilot_state["route"])
    except Exception:
        pass

    charsel_state = _inspect_charsel_state(sm)

    client_build = None
    client_version = None
    try:
        boot_obj = getattr(__builtin__, "boot", None)
        if boot_obj is not None:
            b_val = getattr(boot_obj, "build", None)
            v_val = getattr(boot_obj, "version", None)
            client_build = str(b_val) if b_val else None
            client_version = str(v_val) if v_val else None
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
        "client_build": client_build,
        "client_version": client_version,
        "session": sess_dict,
        "active_view": cur_view,
        "current_view": cur_view,
        "is_loading": is_loading,
        "quasar_channel_state": quasar_state,
        "desktop": desktop_info,
        "charsel_state": charsel_state,
        "skills_state": skills_state,
        "wallet_state": wallet_state,
        "inventory_state": inventory_state,
        "clone_state": clone_state,
        "market_state": market_state,
        "bookmarks_state": bookmarks_state,
        "autopilot_state": autopilot_state,
        "locked_targets": locked_targets,
        "space": space_info,
        "warp_telemetry": dict(_WARP_TELEMETRY),
        "ui": ui_tree,
        "open_windows": ui_tree.get("open_windows", []),
        "modals": ui_tree.get("modals", []),
        "context_menu": ui_tree.get("context_menu", []),
        "flight_recorder_delta": new_flights[-40:],
    }


def _compute_state_diff(pre, post, scatters, target_item_id=None):
    pre_wins = set(w.get("window_id") or w.get("name") for w in (pre.get("open_windows") or []))
    post_wins = set(w.get("window_id") or w.get("name") for w in (post.get("open_windows") or []))
    wins_opened = sorted(list(post_wins - pre_wins))
    wins_closed = sorted(list(pre_wins - post_wins))
    pre_win_rects = dict(
        ((w.get("window_id") or w.get("name")), (w.get("x"), w.get("y"), w.get("w"), w.get("h"), w.get("minimized")))
        for w in (pre.get("open_windows") or [])
    )
    post_win_rects = dict(
        ((w.get("window_id") or w.get("name")), (w.get("x"), w.get("y"), w.get("w"), w.get("h"), w.get("minimized")))
        for w in (post.get("open_windows") or [])
    )
    windows_moved = pre_win_rects != post_win_rects

    pre_modals = set(m.get("title") or m.get("name") for m in (pre.get("modals") or []))
    post_modals = set(m.get("title") or m.get("name") for m in (post.get("modals") or []))
    modals_appeared = sorted(list(post_modals - pre_modals))

    view_changed = None
    if pre.get("active_view") != post.get("active_view"):
        view_changed = {"from": pre.get("active_view"), "to": post.get("active_view")}

    pre_sess = pre.get("session") or {}
    post_sess = post.get("session") or {}
    pre_sys = pre_sess.get("solarsystemid2") or pre_sess.get("solarsystemid")
    post_sys = post_sess.get("solarsystemid2") or post_sess.get("solarsystemid")
    solar_system_changed = None
    if pre_sys is not None and post_sys is not None and pre_sys != post_sys:
        solar_system_changed = {"from": pre_sys, "to": post_sys}

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
    if target_item_id is not None:
        try:
            tid_int = int(target_item_id)
            for b in pre_sp.get("nearby_balls") or []:
                if int(b.get("item_id") or 0) == tid_int:
                    ship_delta["target_distance_m_before"] = b.get("distance_m")
                    break
            for b in post_sp.get("nearby_balls") or []:
                if int(b.get("item_id") or 0) == tid_int:
                    ship_delta["target_distance_m_after"] = b.get("distance_m")
                    break
        except Exception:
            pass

    pre_click_list = (pre.get("ui") or {}).get("clickable_widgets") or []
    post_click_list = (post.get("ui") or {}).get("clickable_widgets") or []
    pre_click_count = len(pre_click_list)
    post_click_count = len(post_click_list)
    pre_click_set = set(
        (w.get("path"), w.get("label"), w.get("x"), w.get("y"))
        for w in pre_click_list
    )
    post_click_set = set(
        (w.get("path"), w.get("label"), w.get("x"), w.get("y"))
        for w in post_click_list
    )
    widgets_changed = pre_click_set != post_click_set
    pre_menu_count = len(pre.get("context_menu") or [])
    post_menu_count = len(post.get("context_menu") or [])

    scatter_names = [s.get("event") for s in scatters]
    ui_changed = bool(
        wins_opened
        or wins_closed
        or windows_moved
        or modals_appeared
        or view_changed
        or bool(solar_system_changed)
        or skill_delta["changed"]
        or (isk_delta is not None and abs(isk_delta) > 0.001)
        or ship_delta["in_space_before"] != ship_delta["in_space_after"]
        or ship_delta["mode_before"] != ship_delta["mode_after"]
        or ship_delta["locked_targets_before"] != ship_delta["locked_targets_after"]
        or pre_click_count != post_click_count
        or widgets_changed
        or pre_menu_count != post_menu_count
        or len(scatter_names) > 0
    )

    return {
        "ui_changed": ui_changed,
        "windows_opened": wins_opened,
        "windows_closed": wins_closed,
        "modals_appeared": modals_appeared,
        "view_changed": view_changed,
        "solar_system_changed": solar_system_changed,
        "skill_queue_delta": skill_delta,
        "wallet_isk_delta": isk_delta,
        "ship_delta": ship_delta,
        "clickable_widget_count_delta": post_click_count - pre_click_count,
        "context_menu_count_delta": post_menu_count - pre_menu_count,
        "scatters_observed": scatter_names,
    }


_CORE_HUD_PRESERVE_TOKENS = (
    "lobby",
    "l_shipui",
    "shipui",
    "overview",
    "selecteditem",
    "droneview",
    "target",
    "neocom",
    "sidepanel",
    "infopanel",
    "activeitem",
    "stationservice",
)
_CHAT_WINDOW_TOKENS = ("chatchannel", "xmppchat", "lscchannel", "channelwindow")


def _is_preserved_chrome_window(w, keep_chat=True):
    if w is None:
        return False
    try:
        wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
        if isinstance(wid, tuple):
            wid = _safe_str(wid[0], 64)
        wname = _safe_str(getattr(w, "name", ""), 64)
        wcls = w.__class__.__name__
        combined = ("%s %s %s" % (wid, wname, wcls)).lower()
        if any(tok in combined for tok in _CORE_HUD_PRESERVE_TOKENS):
            return True
        if keep_chat and any(tok in combined for tok in _CHAT_WINDOW_TOKENS):
            return True
    except Exception:
        return False
    return False


def _cleanup_utility_windows(sm, keep_chat=True):
    """Deterministically close non-core utility windows and secondary views on persona rotation
    while preserving error modals (so BugOracle inspects them), station lobby, ship HUD, Overview,
    SelectedItem, DroneView, Neocom/SidePanel, and (optionally) chat channels."""
    uicore = _get_uicore()
    layers = getattr(uicore, "layer", None) if uicore is not None else None
    reg = getattr(uicore, "registry", None) if uicore is not None else None
    wins = list(reg.GetWindows() or []) if (reg is not None and hasattr(reg, "GetWindows")) else []
    if layers is not None:
        for lname in ("abovemain", "main"):
            lobj = getattr(layers, lname, None)
            for ch in list(getattr(lobj, "children", []) or []):
                if ch not in wins and not getattr(ch, "destroyed", False) and _is_visible(ch):
                    wins.append(ch)

    closed = []
    kept = []
    for w in wins:
        try:
            if getattr(w, "destroyed", False) or not _is_visible(w):
                continue
            wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
            if isinstance(wid, tuple):
                wid = _safe_str(wid[0], 64)
            wname = _safe_str(getattr(w, "name", ""), 64)
            wcls = w.__class__.__name__
            combined = ("%s %s %s" % (wid, wname, wcls)).lower()

            is_mod = bool(
                getattr(w, "isModal", False)
                or "messagebox" in combined
                or "popup" in combined
                or "hybridwindow" in combined
                or getattr(w, "_message_label", None) is not None
                or getattr(w, "msgKey", None) is not None
            )
            if is_mod:
                kept.append(wid or wcls)
                continue
            if _is_preserved_chrome_window(w, keep_chat=keep_chat):
                kept.append(wid or wcls)
                continue

            if hasattr(w, "Close"):
                w.Close()
            elif hasattr(w, "CloseByUser"):
                w.CloseByUser()
            if not getattr(w, "destroyed", False):
                try:
                    from carbonui.control.window import Window as _BaseWindow
                    _BaseWindow.Close(w)
                except Exception:
                    try:
                        w.display = False
                    except Exception:
                        pass
            closed.append(wid or wname or wcls)
        except Exception:
            pass

    closed_sec = None
    try:
        vs = sm.GetService("viewState") if sm is not None else None
        cur_vi = vs.GetCurrentViewInfo() if (vs is not None and hasattr(vs, "GetCurrentViewInfo")) else None
        cur_vname = _safe_str(getattr(cur_vi, "name", ""), 48)
        if cur_vname in (
            "paint_tool",
            "virtual_goods_store",
            "aurumstore",
            "starmap_new",
            "starmap",
            "systemmap",
            "systemmap_new",
            "planet",
            "shiptree",
            "career_portal",
            "skill_plan",
        ):
            if cur_vname == "skill_plan":
                try:
                    from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
                    SkillPlanDockablePanel.CloseIfOpen()
                except Exception:
                    pass
            try:
                vs.CloseSecondaryView(cur_vname)
            except Exception:
                pass
            cur_vi2 = vs.GetCurrentViewInfo() if hasattr(vs, "GetCurrentViewInfo") else None
            if getattr(cur_vi2, "name", "") == cur_vname and hasattr(vs, "primaryInfo") and getattr(vs, "primaryInfo", None) is not None:
                try:
                    vs.secondaryInfo = None
                except Exception:
                    pass
            closed_sec = cur_vname
    except Exception:
        pass

    try:
        _close_neocom_panels_if_outside(None)
        _close_utilmenu_if_outside(None)
        _close_context_menus_if_outside(None)
        _dismiss_pinned_tooltip_if_outside(None)
    except Exception:
        pass

    return {
        "ok": True,
        "closed_windows": closed,
        "closed_secondary_view": closed_sec,
        "kept_windows": kept,
    }


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
    "notifications": "OpenNotificationReadingWnd",
    "notification": "OpenNotificationReadingWnd",
    "notificationpanel": "OpenNotificationReadingWnd",
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
    "agentfinder": "OpenAgencyContentGroup",
    "lpstore": "OpenLpstore",
    "repair": "OpenRepairshop",
    "repairshop": "OpenRepairshop",
    "reprocessing": "OpenReprocessingPlant",
    "reprocessingplant": "OpenReprocessingPlant",
    "miningledger": "OpenLedger",
    "ledger": "OpenLedger",
    "mininghold": "OpenGeneralMiningHoldOfActiveShip",
    "orehold": "OpenGeneralMiningHoldOfActiveShip",
    "mineralhold": "OpenMineralHoldOfActiveShip",
    "moonmining": "OpenMoonMining",
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
    "aircareerprogram": "OpenAchievementsWindow",
    "acp": "OpenAchievementsWindow",
    "peopleandplaces": "OpenPeopleAndPlaces",
    "locations": "OpenLocations",
    "bookmarks": "OpenLocations",
    "compare": "OpenCompare",
    "channels": "OpenChannels",
    "chat": "OpenChannels",
    "help": "OpenHelp",
    "log": "OpenLog",
}

OPEN_WINDOW_GUARDS = {
    "moonmining": "structure_control_only",
    "dscan": "space_only",
    "directionalscanner": "space_only",
    "probe": "space_only",
    "probescanner": "space_only",
    "scanner": "space_only",
    "lpstore": "docked_only",
    "repair": "docked_only",
    "repairshop": "docked_only",
    "reprocessing": "docked_only",
    "reprocessingplant": "docked_only",
    "insurance": "docked_only",
}


OPEN_WINDOW_CMD_MAP = cmd_map

_WINDOW_TARGET_ALIASES = {
    "inventory": ("inventory", "inventoryprimary", "inventorywnd"),
    "cargohold": ("cargohold", "inventory", "activeshipcargo", "shipcargo"),
    "shiphangar": ("shiphangar", "inventory", "stationships"),
    "itemhangar": ("itemhangar", "inventory", "stationitems"),
    "market": ("market", "regionalmarket"),
    "marketorders": ("marketorders", "personalmarketorders"),
    "multibuy": ("multibuy", "multibuywnd"),
    "charsheet": ("charsheet", "charactersheet"),
    "charactersheet": ("charsheet", "charactersheet"),
    "character": ("charsheet", "charactersheet"),
    "skills": ("skills", "skillplan", "skillplandockablepanel", "skill_plan"),
    "skillqueue": ("skills", "skillqueue", "skillplan", "skillplandockablepanel", "skill_plan"),
    "skillswindow": ("skills", "skillplan", "skillplandockablepanel", "skill_plan"),
    "skillplan": ("skillplan", "skillplandockablepanel", "skills", "skill_plan"),
    "skillplans": ("skillplan", "skillplandockablepanel", "skills", "skill_plan"),
    "skillplanwindow": ("skillplan", "skillplandockablepanel", "skills", "skill_plan"),
    "skilltree": ("skilltree", "skill_tree", "skillplan", "skills"),
    "nes": ("aurumstore", "newedenstore", "virtualgoodsstore", "virtual_goods_store", "nes", "store"),
    "newedenstore": ("aurumstore", "newedenstore", "virtualgoodsstore", "virtual_goods_store", "nes"),
    "plexstore": ("aurumstore", "newedenstore", "virtualgoodsstore", "virtual_goods_store", "nes"),
    "aurumstore": ("aurumstore", "newedenstore", "virtualgoodsstore", "virtual_goods_store", "nes"),
    "store": ("aurumstore", "newedenstore", "virtualgoodsstore", "virtual_goods_store", "nes", "store"),
    "plexvault": ("plexvault",),
    "fitting": ("fitting", "fittingwindow"),
    "fittingmgmt": ("fittingmgmt", "fittingmanagement"),
    "contracts": ("contracts", "contractswindow"),
    "industry": ("industry", "industrywnd"),
    "planets": ("planets", "planet", "planetary"),
    "pi": ("planets", "planet", "planetary"),
    "corp": ("corp", "corporation"),
    "corporation": ("corp", "corporation"),
    "fleet": ("fleet", "fleetwindow"),
    "agency": ("agency", "agencywnd"),
    "jobboard": ("jobboard", "opportunities"),
    "opportunities": ("jobboard", "opportunities"),
    "assets": ("assets", "assetswindow"),
    "wallet": ("wallet", "walletwindow"),
    "mail": ("mail", "mailwindow"),
    "notifications": ("notification", "notifications", "notificationcenter", "notificationreading"),
    "notification": ("notification", "notifications", "notificationcenter", "notificationreading"),
    "notificationpanel": ("notification", "notifications", "notificationcenter", "notificationreading"),
    "notificationfeed": ("notification", "notifications", "notificationcenter", "notificationreading"),
    "notificationcenter": ("notification", "notifications", "notificationcenter", "notificationreading"),
    "notificationreading": ("notification", "notifications", "notificationcenter", "notificationreading"),
    "map": ("map", "starmap", "starmap_new", "mapbrowser"),
    "solarsystemmap": ("solarsystemmap", "systemmap", "systemmap_new"),
    "dscan": ("dscan", "directionalscanner"),
    "directionalscanner": ("dscan", "directionalscanner"),
    "probe": ("probe", "probescanner", "scanner"),
    "probescanner": ("probe", "probescanner", "scanner"),
    "scanner": ("probe", "probescanner", "scanner"),
    "notepad": ("notepad",),
    "calculator": ("calculator",),
    "calendar": ("calendar",),
    "journal": ("journal",),
    "agentfinder": ("agentfinder", "agency", "agencywnd"),
    "lpstore": ("lpstore", "loyaltypointstore"),
    "repair": ("repair", "repairshop"),
    "repairshop": ("repair", "repairshop"),
    "reprocessing": ("reprocessing", "reprocessingplant"),
    "reprocessingplant": ("reprocessing", "reprocessingplant"),
    "miningledger": ("miningledger", "ledger"),
    "ledger": ("miningledger", "ledger"),
    "mininghold": ("mininghold", "orehold", "inventory"),
    "orehold": ("mininghold", "orehold", "inventory"),
    "mineralhold": ("mineralhold", "inventory"),
    "insurance": ("insurance", "insurancewindow"),
    "bountyoffice": ("bountyoffice", "bounty"),
    "securityoffice": ("securityoffice", "security"),
    "militia": ("militia", "factionalwarfare", "fw"),
    "factionalwarfare": ("factionalwarfare", "factionwarfare", "fw", "militia"),
    "fw": ("factionalwarfare", "factionwarfare", "fw", "militia"),
    "insurgency": ("insurgency", "pirateinsurgency"),
    "skinr": ("skinr", "shipskinr", "ship_skinr", "paint_tool", "painttool"),
    "shipskinr": ("skinr", "shipskinr", "ship_skinr", "paint_tool", "painttool"),
    "structurebrowser": ("structurebrowser",),
    "shiptree": ("shiptree", "isis"),
    "isis": ("shiptree", "isis"),
    "aircareerprogram": ("aircareerprogram", "career_portal", "careerportal", "achievements", "achievement"),
    "acp": ("aircareerprogram", "career_portal", "careerportal", "achievements", "achievement"),
    "peopleandplaces": ("peopleandplaces", "addressbook"),
    "locations": ("locations", "bookmarks"),
    "bookmarks": ("locations", "bookmarks"),
    "compare": ("compare", "typecompare"),
    "channels": ("channels", "channel"),
    "chat": ("channels", "chat"),
    "help": ("help", "helpwindow"),
    "log": ("log", "logger", "logwindow"),
    "info": ("info", "infowindow"),
}


def _normalize_window_tok(val):
    return str(val or "").strip().lower().replace("_", "").replace("-", "").replace(" ", "")


def _window_target_tokens(t_norm):
    base = _WINDOW_TARGET_ALIASES.get(t_norm, (t_norm,) if t_norm else ())
    out = []
    for tok in base:
        nt = _normalize_window_tok(tok)
        if nt and nt not in out:
            out.append(nt)
    return tuple(out)


def _target_matches_window(t_norm, wid, wcap, wcls):
    tokens = _window_target_tokens(t_norm)
    if not tokens:
        return True
    wid_n = _normalize_window_tok(wid)
    wcap_n = _normalize_window_tok(wcap)
    wcls_n = _normalize_window_tok(wcls)
    return any((tok in wid_n or tok in wcap_n or tok in wcls_n) for tok in tokens)


def _target_matches_view(t_norm, view_name):
    if not t_norm or not view_name:
        return False
    vn = _normalize_window_tok(view_name)
    return any((tok in vn or vn in tok) for tok in _window_target_tokens(t_norm))


_PRIMARY_VIEW_NAMES = ("", "hangar", "inflight", "charsel", "station", "login", "intro")


def _current_secondary_view_name(sm):
    if sm is None:
        return ""
    try:
        svcs = getattr(sm, "services", None)
        vs_cur = svcs.get("viewState") if isinstance(svcs, dict) else None
        if vs_cur is None and hasattr(sm, "GetService"):
            vs_cur = sm.GetService("viewState")
        if vs_cur is None:
            return ""
        sec_cur = getattr(vs_cur, "secondaryInfo", None)
        if sec_cur is not None and getattr(sec_cur, "name", None):
            sec_name = _safe_str(getattr(sec_cur, "name", "") or "", 48)
            if sec_name.strip().lower() not in _PRIMARY_VIEW_NAMES:
                return sec_name
        if hasattr(vs_cur, "GetCurrentViewInfo"):
            vi_cur = vs_cur.GetCurrentViewInfo()
            vi_name = _safe_str(getattr(vi_cur, "name", "") or "", 48)
            if vi_name.strip().lower() not in _PRIMARY_VIEW_NAMES:
                return vi_name
    except Exception:
        pass
    return ""


def _find_matching_secondary_or_drawer_container(sm, t_norm):
    if not t_norm:
        return None, None, None
    tokens = _window_target_tokens(t_norm)
    svcs = getattr(sm, "services", {}) if sm is not None else {}
    if any(tok in ("notification", "notifications", "notificationcenter", "notificationreading") for tok in tokens):
        n_ui = svcs.get("notificationUIService") if isinstance(svcs, dict) else None
        if n_ui is None and sm is not None and hasattr(sm, "GetService"):
            try:
                n_ui = sm.GetService("notificationUIService")
            except Exception:
                n_ui = None
        nc = getattr(n_ui, "notificationCenter", None) if n_ui is not None else None
        if nc is not None:
            nc_cont = getattr(nc, "notificationContainer", None) or getattr(nc, "drawer", None) or nc
            if getattr(nc, "isShowingDrawer", False) or (nc_cont is not nc and _is_visible(nc_cont)):
                return nc_cont, "notifications", "drawer"
    if any(tok in ("skills", "skillplan", "skillplandockablepanel", "skillqueue") for tok in tokens):
        try:
            from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
            sp_wnd = SkillPlanDockablePanel.GetIfOpen()
            if sp_wnd is not None and not getattr(sp_wnd, "destroyed", False) and _is_visible(sp_wnd):
                return sp_wnd, "SkillPlanDockablePanel", "dockable_panel"
        except Exception:
            pass
    if any(tok in ("dscan", "directionalscanner", "directionalscannerwindow", "directionalscannerpalette") for tok in tokens):
        ds_kind, ds_cont, _, ds_min = _find_directional_scanner_surface(_get_uicore(), include_minimized=False)
        if ds_cont is not None and not ds_min:
            wid_ds = _safe_str(getattr(ds_cont, "windowID", None) or "directionalScannerWindow", 64)
            return ds_cont, wid_ds, ("embedded_panel" if ds_kind == "embedded" else "window")
    sec_name = _current_secondary_view_name(sm)
    if sec_name and _target_matches_view(t_norm, sec_name):
        vs = svcs.get("viewState") if isinstance(svcs, dict) else None
        if vs is None and sm is not None and hasattr(sm, "GetService"):
            try:
                vs = sm.GetService("viewState")
            except Exception:
                vs = None
        if vs is not None:
            try:
                sec_view = getattr(vs, "activeView", None)
                sec_layer = getattr(sec_view, "layer", None) if sec_view is not None else None
                return (sec_layer or sec_view), sec_name, "secondary_view"
            except Exception:
                pass
    return None, None, None


def get_open_window_cmd_map():
    return dict(cmd_map)


def _click_top_tabs_in_window(target="", max_tabs=6, require_match=False):
    """Click through visible top-level tabs inside the matching (or topmost) open window/panel."""
    uicore = _get_uicore()
    reg = getattr(uicore, "registry", None) if uicore is not None else None
    wins = list(reg.GetWindows() or []) if (reg is not None and hasattr(reg, "GetWindows")) else []
    t_norm = _normalize_window_tok(target)
    is_explicit_chrome = bool(t_norm and any(tok == t_norm or (len(t_norm) >= 5 and tok in t_norm) for tok in (_CORE_HUD_PRESERVE_TOKENS + _CHAT_WINDOW_TOKENS)))
    chosen = None
    surface_type = "window"
    chosen_id = ""
    for w in reversed(wins):
        if w is None or getattr(w, "destroyed", False) or not _is_visible(w):
            continue
        if not t_norm:
            if not _is_preserved_chrome_window(w, keep_chat=True):
                chosen = w
                break
            continue
        if not is_explicit_chrome and _is_preserved_chrome_window(w, keep_chat=True):
            continue
        wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
        wcap = _strip_tags(_safe_str(getattr(w, "caption", None) or "", 64))
        wcls = w.__class__.__name__
        if _target_matches_window(t_norm, wid, wcap, wcls):
            chosen = w
            break
    if chosen is None and t_norm:
        sm_obj = getattr(__builtin__, "sm", None)
        sec_cont, sec_id, sec_stype = _find_matching_secondary_or_drawer_container(sm_obj, t_norm)
        if sec_id:
            chosen = sec_cont
            chosen_id = sec_id
            surface_type = sec_stype or "secondary_view"
    if chosen is None and not chosen_id and wins and not (require_match and t_norm):
        for w in reversed(wins):
            if w is not None and not getattr(w, "destroyed", False) and _is_visible(w):
                if not is_explicit_chrome and _is_preserved_chrome_window(w, keep_chat=True):
                    continue
                chosen = w
                break
    if chosen is None and not chosen_id:
        return {
            "ok": False,
            "error": "no visible open window found for click_top_tabs (target=%r)" % (target,),
            "tabs_clicked": 0,
            "clicked_tabs": [],
            "window": None,
        }

    if not chosen_id:
        chosen_id = _safe_str(getattr(chosen, "windowID", None) or getattr(chosen, "name", "") or chosen.__class__.__name__, 64)
    seen = set()
    queue = [(chosen, 0)] if chosen is not None else []
    tab_group = None
    while queue and len(seen) < 160:
        node, depth = queue.pop(0)
        if node is None or id(node) in seen or getattr(node, "destroyed", False):
            continue
        seen.add(id(node))
        if node is not chosen and not _is_visible(node):
            continue
        cls_l = node.__class__.__name__.lower()
        if "tabgroup" in cls_l or (hasattr(node, "SelectByIdx") and isinstance(getattr(node, "tabs", None), (list, tuple))):
            tab_group = node
            break
        if depth < 8:
            for ch in list(getattr(node, "children", []) or [])[:30]:
                if id(ch) not in seen:
                    queue.append((ch, depth + 1))

    if tab_group is None:
        return {"ok": True, "tabs_clicked": 0, "clicked_tabs": [], "window": chosen_id, "surface_type": surface_type}

    tabs = list(getattr(tab_group, "tabs", None) or [])
    if not tabs:
        tabs = [
            ch for ch in list(getattr(tab_group, "children", []) or [])
            if ch is not None and not getattr(ch, "destroyed", False) and _is_visible(ch)
        ]
    clicked_tabs = []
    limit = max(1, int(max_tabs or 6))
    for idx, tab in enumerate(tabs[:limit]):
        if tab is None or getattr(tab, "destroyed", False):
            continue
        lbl = _strip_tags(
            _safe_str(
                getattr(tab, "text", None)
                or getattr(tab, "label", None)
                or _extract_widget_label(tab)
                or getattr(tab, "name", "")
                or ("tab_%d" % idx),
                64,
            )
        ).strip()
        try:
            if callable(getattr(tab, "OnClick", None)) or callable(getattr(tab, "on_click", None)):
                _invoke_widget_click(tab)
            elif hasattr(tab_group, "SelectByIdx"):
                tab_group.SelectByIdx(idx)
            elif callable(getattr(tab, "Select", None)):
                tab.Select()
            else:
                _invoke_widget_click(tab)
            H._sleep_ms(120)
            clicked_tabs.append(lbl or ("tab_%d" % idx))
        except Exception as exc:
            return {
                "ok": False,
                "error": "clicking top tab %r in %r raised: %r" % (lbl, chosen_id, exc),
                "tabs_clicked": len(clicked_tabs),
                "clicked_tabs": clicked_tabs,
                "window": chosen_id,
            }
    return {
        "ok": True,
        "tabs_clicked": len(clicked_tabs),
        "clicked_tabs": clicked_tabs,
        "window": chosen_id,
    }


def _open_client_window(sm, target, require_open=False):
    uicore = _get_uicore()
    layers = getattr(uicore, "layer", None) if uicore is not None else None
    menu_layer = getattr(layers, "menu", None) if layers is not None else None
    if menu_layer is not None:
        try:
            for mv in list(getattr(menu_layer, "children", []) or []):
                if hasattr(mv, "CloseMenu"):
                    mv.CloseMenu()
                elif hasattr(mv, "Close"):
                    mv.Close()
            if hasattr(menu_layer, "Flush"):
                menu_layer.Flush()
        except Exception:
            pass
    cmd_svc = getattr(uicore, "cmd", None) if uicore is not None else None
    if cmd_svc is None:
        try:
            cmd_svc = sm.GetService("cmd")
        except Exception:
            cmd_svc = None

    raw_target = str(target or "").strip()
    t = _normalize_window_tok(raw_target)
    reg = getattr(uicore, "registry", None) if uicore is not None else None

    def _current_sec_view():
        return _current_secondary_view_name(sm)

    pre_sec_view = _current_sec_view()

    def _verify_matching_surface_open(pre_sec_view="", opened_vis_ids=None):
        post_sec_view = _current_sec_view()
        if post_sec_view and (
            (t and _target_matches_view(t, post_sec_view))
            or post_sec_view != pre_sec_view
        ):
            return True
        if opened_vis_ids:
            return True
        sec_cont, sec_id, _ = _find_matching_secondary_or_drawer_container(sm, t)
        if sec_id and (sec_cont is None or _is_visible(sec_cont) or sec_id == "notifications"):
            return True
        if reg is not None and hasattr(reg, "GetWindows"):
            is_explicit_chrome = bool(t and any(tok == t or (len(t) >= 5 and tok in t) for tok in (_CORE_HUD_PRESERVE_TOKENS + _CHAT_WINDOW_TOKENS)))
            try:
                for rw in list(reg.GetWindows() or []):
                    if rw is None or getattr(rw, "destroyed", False) or not _is_visible(rw):
                        continue
                    if not is_explicit_chrome and _is_preserved_chrome_window(rw, keep_chat=True):
                        continue
                    wid_s = _safe_str(getattr(rw, "windowID", None) or getattr(rw, "name", ""), 64)
                    wcap_s = _strip_tags(_safe_str(getattr(rw, "caption", None) or "", 64))
                    wcls_s = rw.__class__.__name__
                    if t and _target_matches_window(t, wid_s, wcap_s, wcls_s):
                        return True
            except Exception:
                pass
        return False

    if t in ("skills", "skillqueue", "skillswindow", "skillplan", "skillplans", "skillplanwindow"):
        try:
            sp_svc = sm.GetService("skillPlanSvc")
            if hasattr(sp_svc, "GetAllPersonal"):
                sp_svc.GetAllPersonal()
            from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
            wnd = SkillPlanDockablePanel.Open()
            if wnd is not None and hasattr(wnd, "IsMinimized") and wnd.IsMinimized() and hasattr(wnd, "Maximize"):
                wnd.Maximize()
            H._sleep_ms(500)
            if wnd is None:
                wnd = SkillPlanDockablePanel.GetIfOpen()
            sp_visible = bool(wnd is not None and not getattr(wnd, "destroyed", False) and _is_visible(wnd))
            if require_open and not sp_visible and not _verify_matching_surface_open(pre_sec_view=pre_sec_view):
                return {
                    "ok": False,
                    "error": "open_window %r produced no visible SkillPlanDockablePanel" % (raw_target,),
                }
            return {"ok": True, "opened_via": "SkillPlanDockablePanel.Open"}
        except Exception:
            pass

    if t in ("notifications", "notification", "notificationpanel", "notificationfeed", "notificationcenter", "notificationreading"):
        svcs = getattr(sm, "services", {}) if sm is not None else {}
        n_ui = svcs.get("notificationUIService")
        if n_ui is None and sm is not None and hasattr(sm, "GetService"):
            try:
                n_ui = sm.GetService("notificationUIService")
            except Exception:
                n_ui = None
        if n_ui is not None:
            try:
                nc = getattr(n_ui, "notificationCenter", None)
                if nc is None and hasattr(n_ui, "_ConstructNotificationCenter"):
                    n_ui._ConstructNotificationCenter()
                    nc = getattr(n_ui, "notificationCenter", None)
                if nc is not None:
                    already_showing = bool(getattr(nc, "isShowingDrawer", False))
                    if not already_showing and hasattr(nc, "OpenDrawer"):
                        nc.OpenDrawer()
                        H._sleep_ms(500)
                    nc_cont = getattr(nc, "notificationContainer", None) or getattr(nc, "drawer", None)
                    drawer_open = bool(getattr(nc, "isShowingDrawer", False) or (nc_cont is not None and _is_visible(nc_cont)))
                    if drawer_open or not require_open:
                        via_n = (
                            "notificationUIService.notificationCenter (already open)"
                            if already_showing
                            else "notificationUIService.notificationCenter.OpenDrawer"
                        )
                        return {"ok": True, "opened_via": via_n}
            except Exception:
                pass
        n_svc = svcs.get("notificationSvc")
        if n_svc is None and sm is not None and hasattr(sm, "GetService"):
            try:
                n_svc = sm.GetService("notificationSvc")
            except Exception:
                n_svc = None
        if n_svc is not None:
            try:
                if hasattr(n_svc, "GetAllFormattedNotifications"):
                    n_svc.GetAllFormattedNotifications()
                if hasattr(n_svc, "OpenNotificationReadingWnd"):
                    n_svc.OpenNotificationReadingWnd()
                    H._sleep_ms(500)
                    if not require_open or _verify_matching_surface_open(pre_sec_view=pre_sec_view):
                        return {"ok": True, "opened_via": "notificationSvc.OpenNotificationReadingWnd"}
                    return {
                        "ok": False,
                        "error": "open_window %r produced no visible notification drawer or window" % (raw_target,),
                    }
            except Exception:
                pass

    mname = cmd_map.get(t, raw_target)
    if mname == "OpenMoonMining":
        sess = getattr(__builtin__, "session", None)
        struct_id = getattr(sess, "structureid", None) if sess is not None else None
        ship_id = getattr(sess, "shipid", None) if sess is not None else None
        if not struct_id or struct_id != ship_id:
            return {
                "ok": False,
                "error": "Moon Mining Scheduling requires controlling an Upwell Refinery with an active Standup Moon Drill; use 'miningledger' or 'structurebrowser' instead",
            }
    if mname in ("OpenDirectionalScanner", "OpenProbeScanner"):
        sess = getattr(__builtin__, "session", None)
        if sess is not None and (getattr(sess, "stationid", None) is not None or getattr(sess, "structureid", None) is not None):
            return {"ok": False, "error": "%s is only available while undocked in space" % (mname,)}
    if mname in ("OpenLpstore", "OpenLPStore", "OpenRepairshop", "OpenReprocessingPlant", "OpenInsurance"):
        sess = getattr(__builtin__, "session", None)
        if sess is None or (getattr(sess, "stationid", None) is None and getattr(sess, "structureid", None) is None):
            return {"ok": False, "error": "%s requires being docked at a station or structure; call dock first" % (mname,)}
    if cmd_svc is not None and hasattr(cmd_svc, mname):
        if mname == "OpenDirectionalScanner":
            pre_ds_kind, pre_ds_cont, pre_ds_host, pre_ds_min = _find_directional_scanner_surface(uicore, include_minimized=True)
            if pre_ds_cont is not None:
                if pre_ds_min:
                    _maximize_dscan_surface(pre_ds_cont, pre_ds_host)
                ds_kind, ds_cont, _, ds_min = _find_directional_scanner_surface(uicore, include_minimized=False)
                if ds_cont is not None and not ds_min:
                    return {
                        "ok": True,
                        "opened_via": "DirectionalScanner (%s, already open)" % (ds_kind,),
                        "opened_window_count": 0,
                    }

        def _reg_window_vis_map():
            snap = {}
            if reg is None or not hasattr(reg, "GetWindows"):
                return snap
            try:
                for rw in list(reg.GetWindows() or []):
                    if getattr(rw, "destroyed", False):
                        continue
                    try:
                        vis = bool(_is_visible(rw))
                    except Exception:
                        vis = True
                    snap[id(rw)] = vis
            except Exception:
                pass
            return snap

        pre_snap = _reg_window_vis_map()
        fn = getattr(cmd_svc, mname)
        cmd_args = ()
        cmd_kwargs = {}
        if mname == "OpenAgencyContentGroup" and t == "agentfinder":
            cg_id = 28
            try:
                from eve.client.script.ui.shared.agencyNew.contentGroups import contentGroupConst as _cgc
                cg_id = int(getattr(_cgc, "contentGroupAgentFinder", 28) or 28)
            except Exception:
                pass
            cmd_args = (cg_id,)
        elif mname == "OpenDirectionalScanner" and _find_solar_system_map_panel(uicore, include_minimized=True) is not None:
            # When SolarSystemViewPanel is already open (visible or minimized), V24
            # OpenDirectionalScanner(toggle=True) toggles D-Scan; pass toggle=False when
            # supported so open never acts as close.
            try:
                code_obj = getattr(fn, "__code__", None)
                varnames = getattr(code_obj, "co_varnames", ()) if code_obj is not None else ()
                if "toggle" in varnames:
                    cmd_kwargs["toggle"] = False
            except Exception:
                pass
        try:
            fn(*cmd_args, **cmd_kwargs)
        except Exception as exc:
            if "WindowBlockedFromOpening" in repr(exc) or getattr(exc, "msg", None) == "WindowBlockedFromOpening":
                H._sleep_ms(1200)
                fn(*cmd_args, **cmd_kwargs)
            else:
                raise
        H._sleep_ms(500)
        post_snap = _reg_window_vis_map()
        closed_ids = [wid for wid in pre_snap if wid not in post_snap]
        opened_vis_ids = [wid for wid, vis in post_snap.items() if wid not in pre_snap and vis]
        post_sec_view = _current_sec_view()
        sec_toggled_closed = bool(
            pre_sec_view
            and _target_matches_view(t, pre_sec_view)
            and post_sec_view != pre_sec_view
            and not _target_matches_view(t, post_sec_view)
        )
        retried = False
        if (closed_ids and not opened_vis_ids) or sec_toggled_closed:
            # ToggleOpenClose-style commands (e.g. OpenLedger / CmdToggleMap) close an already-open
            # window, secondary view, or zombie; re-invoke once so open_window guarantees opening.
            fn(*cmd_args, **cmd_kwargs)
            H._sleep_ms(500)
            retried = True
            post_snap = _reg_window_vis_map()
            opened_vis_ids = [wid for wid, vis in post_snap.items() if wid not in pre_snap and vis]

        if mname == "OpenDirectionalScanner":
            ds_kind, ds_cont, ds_host, ds_min = _find_directional_scanner_surface(uicore, include_minimized=True)
            if ds_cont is not None and ds_min:
                _maximize_dscan_surface(ds_cont, ds_host)
            ds_kind, ds_cont, ds_host, ds_min = _find_directional_scanner_surface(uicore, include_minimized=False)
            if ds_cont is None or ds_min:
                return {
                    "ok": False,
                    "error": "open_window %r produced no visible Directional Scanner window or panel (cmd=%s)" % (raw_target, mname),
                }
            via_msg = "uicore.cmd.%s (retried after toggle-close)" % mname if retried else "uicore.cmd.%s" % mname
            return {
                "ok": True,
                "opened_via": "%s (%s)" % (via_msg, ds_kind),
                "opened_window_count": len(opened_vis_ids) if opened_vis_ids else 1,
            }

        if require_open and not _verify_matching_surface_open(pre_sec_view=pre_sec_view, opened_vis_ids=opened_vis_ids):
            return {
                "ok": False,
                "error": "open_window %r produced no visible window or secondary view (cmd=%s)" % (raw_target, mname),
            }

        via_msg = "uicore.cmd.%s (retried after toggle-close)" % mname if retried else "uicore.cmd.%s" % mname
        return {"ok": True, "opened_via": via_msg, "opened_window_count": len(opened_vis_ids)}

    if t in ("nes", "newedenstore", "plexstore", "aurumstore", "store"):
        try:
            vs = sm.GetService("viewState")
            vs.ToggleSecondaryView("aurumstore")
            H._sleep_ms(600)
            post_sec = _current_sec_view()
            if require_open and pre_sec_view and _target_matches_view(t, pre_sec_view) and not _target_matches_view(t, post_sec):
                vs.ToggleSecondaryView("aurumstore")
                H._sleep_ms(600)
                post_sec = _current_sec_view()
            if require_open and not _verify_matching_surface_open(pre_sec_view=pre_sec_view):
                return {
                    "ok": False,
                    "error": "open_window %r did not open aurumstore secondary view (current=%r)" % (raw_target, post_sec),
                }
            return {"ok": True, "opened_via": "viewState.ToggleSecondaryView(aurumstore)"}
        except Exception as exc:
            return {"ok": False, "error": "aurumstore open failed: %r" % (exc,)}

    return {"ok": False, "error": "unknown window target %r (normalized=%r)" % (target, t)}


def _run_gm_slash(sm, scmd):
    slash_svc = sm.GetService("slash")
    if scmd == "/" and hasattr(slash_svc, "FetchRemoteCommandList"):
        try:
            if hasattr(slash_svc, "remoteCommandList"):
                delattr(slash_svc, "remoteCommandList")
        except Exception:
            pass
        slash_svc.FetchRemoteCommandList()
        cmds = getattr(slash_svc, "remoteCommandList", None)
        if cmds:
            return "Commands: [%s]" % (", ".join(repr(str(c)) for c in cmds))
        try:
            sm.RemoteSvc("slash").SlashCmd("/")
        except Exception as exc:
            args = getattr(exc, "args", None)
            if isinstance(args, tuple) and len(args) >= 2 and isinstance(args[1], dict) and args[1].get("reason"):
                return _safe_str(args[1]["reason"], 2000)
            return _safe_str(repr(exc), 2000)
        return "FetchRemoteCommandList OK (empty)"
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


def _invoke_text_submit_candidates(candidates, result):
    for obj_t, mname in candidates:
        fn = getattr(obj_t, mname, None) if obj_t is not None else None
        if not callable(fn):
            continue
        try:
            fn()
            result["submitted"] = mname
            return True
        except TypeError:
            _, _, tb_exc = sys.exc_info()
            has_inner = tb_exc is not None and tb_exc.tb_next is not None
            tb_exc = None
            if has_inner:
                raise
            continue
    return False


_ASYNC_SECONDARY_VIEWS = (
    "ship_skinr",
    "paint_tool",
    "virtual_goods_store",
    "aurumstore",
    "career_portal",
    "skill_plan",
    "skill_tree",
    "starmap_new",
    "systemmap_new",
    "planet",
    "shiptree",
)


def _settle_secondary_view_transition(
    sm,
    pre_state,
    inspect_ui_fn=None,
    sleep_fn=None,
    max_polls=12,
    poll_ms=250,
):
    """Wait briefly for async secondary view windows (e.g. ShipSKINRWindow) to populate child pages
    before post_state / screenshot capture, avoiding false-positive blank-window captures."""
    if sm is None:
        return
    try:
        vs = getattr(sm, "services", {}).get("viewState")
        if vs is None or not hasattr(vs, "GetCurrentViewInfo"):
            return
        vi = vs.GetCurrentViewInfo()
        cur_view = _safe_str(getattr(vi, "name", "") or "", 48)
    except Exception:
        return
    pre_view = _safe_str((pre_state or {}).get("active_view") or "", 48)
    if not cur_view or cur_view == pre_view or cur_view not in _ASYNC_SECONDARY_VIEWS:
        return

    inspect_fn = inspect_ui_fn or _inspect_uicore_tree
    sleeper = sleep_fn
    if sleeper is None:
        try:
            sleeper = sys.modules[__name__]._sleep_ms
        except Exception:
            return

    def _snapshot():
        try:
            tree = inspect_fn() or {}
        except Exception:
            return 0, False, False
        cw = tree.get("clickable_widgets") or []
        modals = tree.get("modals") or []
        has_page_content = True
        if cur_view == "ship_skinr":
            has_page_content = any(
                "page_container/" in str(w.get("path") or "")
                for w in cw
                if isinstance(w, dict)
            )
        return len(cw), bool(modals), has_page_content

    prev_cnt, has_modal, has_page_content = _snapshot()
    if has_modal:
        return
    stable_polls = 0
    for _ in range(int(max_polls)):
        sleeper(poll_ms)
        cnt, has_modal, has_page_content = _snapshot()
        if has_modal:
            return
        if has_page_content and cnt == prev_cnt and cnt > 0:
            stable_polls += 1
            if stable_polls >= 2:
                return
        else:
            stable_polls = 0
            prev_cnt = cnt


def _has_ancestor_named(node, fragment_lower):
    cur = node
    seen = set()
    while cur is not None and id(cur) not in seen:
        seen.add(id(cur))
        try:
            nm = _safe_str(getattr(cur, "name", ""), 64).lower()
            if fragment_lower in nm:
                return True
            cur = getattr(cur, "parent", None)
        except Exception:
            break
    return False


def _settle_after_action(sm, pre_flight_seq, pre_scatter_seq, min_settle_ms=0, max_ms=250):
    """Event-driven settle poll: waits briefly until in-flight RPCs drain and UI layer/scatters stabilize."""
    waited = 0
    try:
        sleeper = sys.modules[__name__]._sleep_ms if hasattr(sys.modules.get(__name__), "_sleep_ms") else H._sleep_ms
    except Exception:
        sleeper = H._sleep_ms

    if min_settle_ms and int(min_settle_ms) > 0:
        ms = min(int(min_settle_ms), 5000)
        sleeper(ms)
        waited += ms

    def _ui_sig():
        try:
            uc = _get_uicore()
            ly = getattr(uc, "layer", None) if uc is not None else None
            m_cnt = len(getattr(getattr(ly, "main", None), "children", None) or []) if ly is not None else 0
            modal_cnt = len(_active_modal_keys())
        except Exception:
            m_cnt, modal_cnt = 0, 0
        return (m_cnt, modal_cnt, _SCATTER_SEQ, _FLIGHT_SEQ)

    sleeper(25)
    waited += 25
    prev_sig = _ui_sig()
    in_flight = _has_in_flight_rpc(sm)
    if prev_sig[1] > 0 and not in_flight:
        return waited
    stable_ticks = 0
    cap = max(25, int(max_ms or 250))
    in_flight_cap = max(cap, min(max(cap * 4, 1500), 2500))
    had_in_flight = bool(in_flight)
    while waited < (in_flight_cap if had_in_flight else cap):
        in_flight = _has_in_flight_rpc(sm)
        if in_flight:
            had_in_flight = True
        cur_sig = _ui_sig()
        if not in_flight and cur_sig == prev_sig:
            stable_ticks += 1
            if stable_ticks >= 1:
                break
        else:
            stable_ticks = 0
            prev_sig = cur_sig
        limit = in_flight_cap if had_in_flight else cap
        step_ms = min(25, limit - waited)
        if step_ms <= 0:
            break
        sleeper(step_ms)
        waited += step_ms
    return waited


def _wait_session_settled(timeout_ms=5000, poll_ms=50):
    """Yield non-busily until session.mutating and session.changing clear before actions."""
    def _get_sess():
        s = getattr(__builtin__, "session", None)
        if s is None:
            eve_obj = getattr(__builtin__, "eve", None)
            s = getattr(eve_obj, "session", None) if eve_obj is not None else None
        return s

    def _is_settled(s):
        if s is None:
            return True
        return (not getattr(s, "mutating", 0)) and (getattr(s, "changing", None) is None)

    if _is_settled(_get_sess()):
        return True

    sleeper = H._sleep_ms
    try:
        mod = sys.modules.get(__name__)
        if mod is not None and hasattr(mod, "_sleep_ms"):
            sleeper = mod._sleep_ms
    except Exception:
        pass

    step_ms = max(10, int(poll_ms or 50))
    max_ms = max(0, int(timeout_ms or 0))
    max_polls = max(1, (max_ms + step_ms - 1) // step_ms) if max_ms > 0 else 0
    deadline = time.time() + (max_ms / 1000.0)
    for _ in range(max_polls):
        sleeper(step_ms)
        if _is_settled(_get_sess()):
            return True
        if time.time() >= deadline:
            break
    return _is_settled(_get_sess())


def _execute_action(sm, action):
    global _CONSUMED_FLIGHT_ERR_SEQ, _LAST_SNAPSHOT, _LAST_SNAPSHOT_SM, _LAST_SNAPSHOT_TS
    _install_flight_recorder_hooks(sm)
    _install_warp_telemetry(sm)
    if (
        action.get("reuse_pre_state", True)
        and _LAST_SNAPSHOT is not None
        and _LAST_SNAPSHOT_SM is sm
        and (time.time() - float(_LAST_SNAPSHOT_TS or 0.0)) <= 2.5
    ):
        pre_state = _LAST_SNAPSHOT
    else:
        pre_state = _get_state_snapshot(sm)
    pre_flight_seq = _FLIGHT_SEQ
    pre_scatter_seq = _SCATTER_SEQ
    pre_modal_cnt = len(_RECENT_MODALS)
    since_flight_err_seq = _CONSUMED_FLIGHT_ERR_SEQ
    if _FLIGHT_SEQ < since_flight_err_seq:
        since_flight_err_seq = 0
    try:
        import evomu_site as _es
        cur_exc_seq = int(getattr(_es, "_EXC_SEQ", 0) or 0)
        since_exc_seq = getattr(_es, "_PT_CONSUMED_EXC_SEQ", None)
        if since_exc_seq is None:
            since_exc_seq = 0
        else:
            since_exc_seq = int(since_exc_seq or 0)
            if cur_exc_seq < since_exc_seq:
                since_exc_seq = 0
    except Exception:
        _es = None
        since_exc_seq = 0

    raw_atype = _to_utf8(action.get("type", "") or "").strip()
    raw_target = _to_utf8(action.get("target") or "").strip()
    raw_cmd = _to_utf8(action.get("command") or action.get("slash_cmd") or "").strip()
    explicit_wait_raw = action.get("wait_ms")
    if explicit_wait_raw is None:
        explicit_wait_raw = action.get("wait_after_ms")
    wait_ms = int(explicit_wait_raw or 0)

    # Normalize schema action aliases to internal handlers
    atype = raw_atype
    target = raw_target
    _clicked_node_ref = [None]
    if raw_atype in ("open_neocom", "open_window"):
        atype = "open_window"
        if not target and raw_cmd:
            target = raw_cmd
    elif raw_atype == "close_window":
        atype = "close_modal_or_window"
    elif raw_atype == "slash_command":
        atype = "run_slash"
        if not target and raw_cmd:
            target = raw_cmd
    elif raw_atype == "complete_skills":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/completeskills %s" % (raw_target or raw_cmd or "all")).strip()
        raw_cmd = target
    elif raw_atype == "advance_upwell_timer":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/upwelltimer %s" % (raw_target or raw_cmd)).strip()
        raw_cmd = target
    elif raw_atype == "complete_industry_jobs":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/completeindustry %s" % (raw_target or raw_cmd)).strip()
        raw_cmd = target
    elif raw_atype == "advance_server_time":
        atype = "run_slash"
        target = raw_cmd if raw_cmd.startswith("/") else ("/advancetime %s" % (raw_target or raw_cmd or "3600")).strip()
        raw_cmd = target
    elif raw_atype == "clear_timers":
        atype = "run_slash"
        target = "/cleartimers"
        raw_cmd = target
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
    elif raw_atype in ("warp_to", "dock", "jump_gate", "approach", "orbit", "keep_at_range", "lock_target", "align", "align_to", "stop"):
        atype = "space_command"
        target = "align_to" if raw_atype in ("align", "align_to") else raw_atype
        if raw_target.isdigit():
            action = dict(action)
            if not action.get("item_id"):
                action["_inferred_item_id"] = True
            action["item_id"] = int(raw_target)
    elif raw_atype in ("cleanup_windows", "cleanup_utility_windows"):
        atype = "cleanup_utility_windows"
    elif raw_atype == "click":
        atype = "click_widget" if raw_target else "click_xy"
    elif raw_atype in ("drag", "drag_and_drop"):
        atype = "drag_widget"
    elif raw_atype in ("type", "input_text", "enter_text"):
        atype = "type_text"
    elif raw_atype in ("key_press", "hotkey", "press_hotkey", "shortcut"):
        atype = "press_key"

    if atype == "wait":
        for _cand in (raw_target, raw_cmd):
            _s = _to_utf8(_cand or "").strip().lower()
            if _s.startswith("wait "):
                _s = _s[5:].strip()
            if _s.endswith("ms"):
                _s = _s[:-2].strip()
            if _s.isdigit():
                _parsed_wait_ms = int(_s)
                if _parsed_wait_ms > 0 and (explicit_wait_raw is None or wait_ms <= 500) and _parsed_wait_ms > wait_ms:
                    wait_ms = _parsed_wait_ms
                break

    effective_wait_ms = (
        min(max(int(wait_ms or 500), 0), 30000)
        if atype == "wait"
        else min(max(int(wait_ms), 0), 5000)
    )

    allow_time_control = bool(action.get("time_control", not action.get("scenario_mode", False)))
    uicore = _get_uicore()
    layers = getattr(uicore, "layer", None) if uicore is not None else None
    result = {"ok": True, "action_ok": True, "action_type": raw_atype, "target": raw_target}

    def _do_action():
        try:
            if atype not in ("wait", "close_modal_or_window"):
                _wait_session_settled(timeout_ms=5000, poll_ms=50)
            if atype in (
                "open_window",
                "click_top_tabs",
                "show_info",
                "close_modal_or_window",
                "space_command",
                "station_command",
                "open_structure_cosmetics",
                "run_slash",
                "slash_command",
                "close_window",
            ):
                _close_neocom_panels_if_outside(None)
                _close_utilmenu_if_outside(None)
                _close_context_menus_if_outside(None)
                _dismiss_pinned_tooltip_if_outside(None)
            sess = getattr(__builtin__, "session", None)
            if atype == "open_window":
                req_open = bool(action.get("require_window_open"))
                open_res = _open_client_window(sm, target, require_open=req_open)
                result.update(open_res)
                if open_res.get("ok") and action.get("click_top_tabs"):
                    tab_res = _click_top_tabs_in_window(
                        target,
                        max_tabs=int(action.get("max_tabs", 6) or 6),
                        require_match=True,
                    )
                    result["tabs"] = tab_res
                    if not tab_res.get("ok"):
                        result["ok"] = False
                        result["error"] = tab_res.get("error") or "click_top_tabs failed"

            elif atype == "click_top_tabs":
                result.update(_click_top_tabs_in_window(target, max_tabs=int(action.get("max_tabs", 6) or 6)))

            elif atype == "show_info":
                raw_tid = action.get("type_id")
                if raw_tid in (None, "") and target and str(target).strip().isdigit():
                    raw_tid = str(target).strip()
                try:
                    tid_int = int(raw_tid or 0)
                except Exception:
                    tid_int = 0
                raw_iid = action.get("item_id")
                try:
                    iid_int = int(raw_iid) if raw_iid not in (None, "", 0) else None
                except Exception:
                    iid_int = None
                if tid_int <= 0:
                    result["ok"] = False
                    result["error"] = "show_info requires a positive integer type_id (got %r)" % (raw_tid,)
                else:
                    svcs_info = getattr(sm, "services", {}) if sm is not None else {}
                    info_svc = svcs_info.get("info") if isinstance(svcs_info, dict) else None
                    if info_svc is None and sm is not None and hasattr(sm, "GetService"):
                        info_svc = sm.GetService("info")
                    if info_svc is None or not hasattr(info_svc, "ShowInfo"):
                        result["ok"] = False
                        result["error"] = "info service does not expose ShowInfo"
                    else:
                        if iid_int is not None:
                            info_svc.ShowInfo(tid_int, itemID=iid_int)
                        else:
                            info_svc.ShowInfo(tid_int)
                        H._sleep_ms(450)
                        result["show_info_type_id"] = tid_int
                        result["category"] = _to_utf8(action.get("category") or "")
                        reg_si = getattr(uicore, "registry", None) if uicore is not None else None
                        if reg_si is not None and hasattr(reg_si, "GetWindows"):
                            found_info_wid = None
                            for rw in reversed(list(reg_si.GetWindows() or [])):
                                if rw is None or getattr(rw, "destroyed", False) or not _is_visible(rw):
                                    continue
                                wid_si = _safe_str(getattr(rw, "windowID", None) or getattr(rw, "name", ""), 64)
                                wcap_si = _strip_tags(_safe_str(getattr(rw, "caption", None) or "", 64))
                                wcls_si = rw.__class__.__name__
                                hay_si = (wid_si + " " + wcap_si + " " + wcls_si).lower()
                                if "info" in hay_si:
                                    found_info_wid = wid_si or wcls_si
                                    break
                            if found_info_wid is None:
                                result["ok"] = False
                                result["error"] = "show_info did not open a visible InfoWindow (type_id=%d)" % tid_int
                            else:
                                result["opened_window"] = found_info_wid
                        if result.get("ok", True) and action.get("click_top_tabs"):
                            tab_res = _click_top_tabs_in_window(
                                "info",
                                max_tabs=int(action.get("max_tabs", 6) or 6),
                                require_match=True,
                            )
                            result["tabs"] = tab_res
                            if not tab_res.get("ok"):
                                result["ok"] = False
                                result["error"] = tab_res.get("error") or "show_info click_top_tabs failed"

            elif atype == "cleanup_utility_windows":
                result.update(_cleanup_utility_windows(sm, keep_chat=bool(action.get("keep_chat", True))))

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
                try:
                    vs = sm.GetService("viewState") if sm is not None else None
                    sec_vi = getattr(vs, "secondaryInfo", None) if vs is not None else None
                    if sec_vi is not None:
                        sec_name = _safe_str(getattr(sec_vi, "name", ""), 48)
                        if sec_name:
                            try:
                                vs.CloseSecondaryView(sec_name)
                                H._sleep_ms(250)
                            except Exception:
                                pass
                except Exception:
                    pass
                explicit_tid = None
                if raw_target and ":" in raw_target:
                    _rhs = raw_target.split(":", 1)[1].strip().split()[0]
                    if _rhs.isdigit():
                        explicit_tid = int(_rhs)
                mod_target = raw_target.split(":")[0].split()[0].strip() if raw_target else ""
                if mod_target.isdigit() and int(mod_target) >= 50000000:
                    explicit_tid = int(mod_target)
                    mod_target = ""
                try:
                    from eve.client.script.parklife import states as _park_states
                    st_svc = sm.GetService("stateSvc") if hasattr(sm, "GetService") else None
                    tgt_svc = sm.GetService("target") if hasattr(sm, "GetService") else None
                    if explicit_tid and st_svc is not None:
                        st_svc.SetState(explicit_tid, _park_states.selected, 1)
                        st_svc.SetState(explicit_tid, _park_states.activeTarget, 1)
                    elif tgt_svc is not None and st_svc is not None:
                        t_map = getattr(tgt_svc, "targetsByID", None) or {}
                        if len(t_map) > 1:
                            mich_t = sm.services.get("michelle") if hasattr(sm, "services") else None
                            bp_t = mich_t.GetBallpark() if mich_t is not None else None
                            sid_t = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
                            best_tid = None
                            best_d = 1e18
                            for _tid in list(t_map.keys()):
                                try:
                                    _d = float(bp_t.GetSurfaceDist(sid_t, int(_tid))) if (bp_t is not None and sid_t) else 0.0
                                    if _d < best_d:
                                        best_d = _d
                                        best_tid = int(_tid)
                                except Exception:
                                    pass
                            if best_tid is not None:
                                st_svc.SetState(best_tid, _park_states.selected, 1)
                                st_svc.SetState(best_tid, _park_states.activeTarget, 1)
                except Exception:
                    pass
                slot_name = mod_target or "inFlightHighSlot1"
                candidates = [slot_name]
                if mod_target:
                    if mod_target.isdigit():
                        mb_cand = "ModuleButton_%s" % mod_target
                        if mb_cand not in candidates:
                            candidates.append(mb_cand)
                    _slot_prefixes = (
                        ("hiSlot", "inFlightHighSlot"),
                        ("medSlot", "inFlightMediumSlot"),
                        ("lowSlot", "inFlightLowSlot"),
                    )
                    for _short_p, _hud_p in _slot_prefixes:
                        if mod_target.startswith(_short_p) and mod_target[len(_short_p):].isdigit():
                            _hud_cand = "%s%d" % (_hud_p, int(mod_target[len(_short_p):]) + 1)
                            if _hud_cand not in candidates:
                                candidates.append(_hud_cand)
                        elif mod_target.startswith(_hud_p) and mod_target[len(_hud_p):].isdigit():
                            _idx = int(mod_target[len(_hud_p):])
                            if _idx >= 1:
                                _short_cand = "%s%d" % (_short_p, _idx - 1)
                                if _short_cand not in candidates:
                                    candidates.append(_short_cand)
                else:
                    for extra_cand in (
                        "ModuleButton_483",
                        "ModuleButton_3651",
                        "ModuleButton_482",
                        "inFlightHighSlot1",
                        "inFlightHighSlot2",
                        "inFlightHighSlot3",
                        "inFlightMediumSlot1",
                        "inFlightLowSlot1",
                        "hiSlot0",
                        "medSlot0",
                        "lowSlot0",
                    ):
                        if extra_cand not in candidates:
                            candidates.append(extra_cand)
                node = None
                fallback_offline_node = None
                hud_slot_buttons = []
                for slot_idx in range(1, 9):
                    for prefix in ("inFlightHighSlot", "inFlightMediumSlot", "inFlightLowSlot"):
                        s_key = "%s%d" % (prefix, slot_idx)
                        slot_w = _find_widget(s_key, layers)
                        if slot_w is None:
                            continue
                        for ch in list(getattr(slot_w, "children", []) or []):
                            if hasattr(ch, "moduleinfo") or _safe_str(getattr(ch, "name", ""), 64).startswith("ModuleButton_"):
                                hud_slot_buttons.append((s_key, ch))
                for cand_name in candidates:
                    matched_buttons = []
                    first_node = _find_widget(cand_name, layers)
                    if first_node is not None:
                        if hasattr(first_node, "moduleinfo") or _safe_str(getattr(first_node, "name", ""), 64).startswith("ModuleButton_"):
                            matched_buttons.append(first_node)
                        else:
                            for ch in list(getattr(first_node, "children", []) or []):
                                if hasattr(ch, "moduleinfo") or _safe_str(getattr(ch, "name", ""), 64).startswith("ModuleButton_"):
                                    matched_buttons.append(ch)
                    for s_key, ch in hud_slot_buttons:
                        ch_name = _safe_str(getattr(ch, "name", ""), 64)
                        minfo = getattr(ch, "moduleinfo", None)
                        m_type = str(getattr(minfo, "typeID", "") or "") if minfo is not None else ""
                        m_item = str(getattr(minfo, "itemID", "") or "") if minfo is not None else ""
                        if (
                            ch_name == cand_name
                            or s_key == cand_name
                            or (mod_target and (m_type == mod_target or m_item == mod_target))
                        ) and ch not in matched_buttons:
                            matched_buttons.append(ch)
                    for mb in matched_buttons:
                        if _stale_module_button_reason(mb, sm):
                            continue
                        if bool(getattr(mb, "online", False)):
                            node = mb
                            slot_name = _safe_str(getattr(mb, "name", "") or cand_name, 64)
                            break
                        elif fallback_offline_node is None:
                            fallback_offline_node = mb
                    if node is not None:
                        break
                if node is None and fallback_offline_node is not None:
                    node = fallback_offline_node
                    slot_name = _safe_str(getattr(node, "name", "") or slot_name, 64)
                    try:
                        minfo = getattr(node, "moduleinfo", None)
                        mod_item_id = getattr(minfo, "itemID", None) if minfo is not None else None
                        if mod_item_id and sm is not None and sess is not None and getattr(sess, "shipid", None):
                            dogma_lm = sm.GetService("godma").GetDogmaLM()
                            dogma_lm.SetModuleOnline(int(sess.shipid), int(mod_item_id))
                    except Exception:
                        pass
                    try:
                        if hasattr(node, "ShowOnline"):
                            node.ShowOnline()
                        node.online = True
                        H._sleep_ms(200)
                    except Exception:
                        pass
                if node is not None and (hasattr(node, "OnClick") or hasattr(node, "Click")):
                    click_res = _invoke_widget_click(node)
                    result["clicked_widget"] = click_res
                    result["activated_slot"] = slot_name
                    if str(click_res).startswith("NeedsLockedTarget"):
                        result["ok"] = False
                        result["error"] = "Module %s requires a locked target (loaded targeting cursor); call lock_target first" % (slot_name,)
                    elif str(click_res).startswith("UserError("):
                        result["ok"] = False
                        result["error"] = "Client action rejected with %s" % (click_res,)
                    elif str(click_res).startswith("StaleModuleButton("):
                        result["ok"] = False
                        result["error"] = "Module %s is stale (%s)" % (slot_name, click_res)
                else:
                    result["ok"] = False
                    result["activated_slot"] = "none_fitted"
                    result["error"] = (
                        "Requested module %r not found on ship HUD rack" % (raw_target,)
                        if raw_target
                        else "No fitted active module button found on ship HUD rack"
                    )

            elif atype == "close_modal_or_window":
                closed = []
                reg = getattr(uicore, "registry", None) if uicore is not None else None
                wins = []
                if reg is not None:
                    if hasattr(reg, "GetModalWindow"):
                        try:
                            top_m = reg.GetModalWindow()
                            if top_m is not None and top_m not in wins:
                                wins.append(top_m)
                        except Exception:
                            pass
                    for mdl in reversed(list(getattr(reg, "modals", []) or [])):
                        if mdl is not None and mdl not in wins:
                            wins.append(mdl)
                reg_wins = list(reg.GetWindows() or []) if (reg is not None and hasattr(reg, "GetWindows")) else []
                for rw in reg_wins:
                    if rw not in wins:
                        wins.append(rw)
                if layers is not None:
                    for lname in ("modal", "abovemain", "main"):
                        lobj = getattr(layers, lname, None)
                        for ch in list(getattr(lobj, "children", []) or []):
                            if getattr(ch, "destroyed", False) or not _is_visible(ch):
                                continue
                            cname = _safe_str(getattr(ch, "name", ""), 64)
                            if lname == "modal" and cname.startswith("l_modal_"):
                                for gch in list(getattr(ch, "children", []) or []):
                                    if gch not in wins and not getattr(gch, "destroyed", False) and _is_visible(gch):
                                        wins.append(gch)
                            elif ch not in wins:
                                wins.append(ch)
                t_low = target.lower() if target else ""
                t_norm = _normalize_window_tok(target)
                is_explicit_chrome = bool(t_norm and any(tok == t_norm or (len(t_norm) >= 5 and tok in t_norm) for tok in (_CORE_HUD_PRESERVE_TOKENS + _CHAT_WINDOW_TOKENS)))
                has_visible_modal = any(
                    not getattr(w, "destroyed", False)
                    and _is_visible(w)
                    and bool(
                        getattr(w, "isModal", False)
                        or "MessageBox" in w.__class__.__name__
                        or "Popup" in w.__class__.__name__
                        or "Dialog" in w.__class__.__name__
                    )
                    for w in wins
                )
                for w in wins:
                    if getattr(w, "destroyed", False) or not _is_visible(w):
                        continue
                    wid = _safe_str(getattr(w, "windowID", None) or getattr(w, "name", ""), 64)
                    if isinstance(wid, tuple):
                        wid = _safe_str(wid[0], 64)
                    wname = _safe_str(getattr(w, "name", ""), 64)
                    wcap = _strip_tags(_safe_str(getattr(w, "caption", None) or _extract_widget_label(w), 100))
                    wcls = w.__class__.__name__
                    is_mod = bool(
                        getattr(w, "isModal", False)
                        or "MessageBox" in wcls
                        or "Popup" in wcls
                        or "Dialog" in wcls
                    )
                    if not is_mod and not is_explicit_chrome and _is_preserved_chrome_window(w, keep_chat=True):
                        continue
                    if not t_low and has_visible_modal and not is_mod:
                        continue
                    match = (
                        not t_low
                        or _target_matches_window(t_norm, wid, wname + " " + wcap, wcls)
                        or t_low in wid.lower()
                        or t_low in wname.lower()
                        or t_low in wcap.lower()
                        or t_low in wcls.lower()
                        or (t_low in ("modal", "popup", "dialog", "messagebox") and is_mod)
                    )
                    if match:
                        try:
                            try:
                                _close_context_menus_if_outside(None)
                                _dismiss_pinned_tooltip_if_outside(None)
                            except Exception:
                                pass
                            dismissed_via_button = False
                            if is_mod:
                                dismiss_btn = _find_modal_dismiss_button(w)
                                if dismiss_btn is not None:
                                    try:
                                        _invoke_widget_click(dismiss_btn)
                                        for _ in range(25):
                                            if getattr(w, "destroyed", False) or not _is_visible(w):
                                                dismissed_via_button = True
                                                break
                                            H._sleep_ms(10)
                                    except Exception as btn_exc:
                                        result["close_warning"] = _safe_str(repr(btn_exc), 160)
                            if dismissed_via_button:
                                closed.append(wid or wcap or wcls)
                                continue
                            if is_mod and hasattr(w, "SetModalResult"):
                                try:
                                    cancel_id = 2
                                    try:
                                        import carbonui.uiconst as _uiconst
                                        cancel_id = getattr(_uiconst, "ID_CANCEL", 2)
                                    except Exception:
                                        pass
                                    w.SetModalResult(cancel_id)
                                except Exception:
                                    pass
                            if not getattr(w, "destroyed", False) and _is_visible(w):
                                close_delegated_to_modal = False
                                try:
                                    if hasattr(w, "CloseByUser"):
                                        w.CloseByUser()
                                        if result.get("blocked_by_modal"):
                                            close_delegated_to_modal = True
                                    elif hasattr(w, "Close"):
                                        w.Close()
                                except Exception as close_exc:
                                    result["close_warning"] = _safe_str(repr(close_exc), 160)
                                if close_delegated_to_modal or result.get("blocked_by_modal"):
                                    continue
                            if not getattr(w, "destroyed", False):
                                for _ in range(15):
                                    if getattr(w, "destroyed", False) or result.get("blocked_by_modal"):
                                        break
                                    H._sleep_ms(10)
                            if result.get("blocked_by_modal"):
                                continue
                            if not getattr(w, "destroyed", False):
                                try:
                                    if hasattr(w, "Close"):
                                        w.Close()
                                    if not getattr(w, "destroyed", False):
                                        from carbonui.control.window import Window as _BaseWindow
                                        _BaseWindow.Close(w)
                                except Exception:
                                    try:
                                        w.display = False
                                    except Exception:
                                        pass
                                if uicore is not None and hasattr(uicore, "dockablePanelManager") and hasattr(w, "panelID"):
                                    try:
                                        uicore.dockablePanelManager.UnregisterPanel(w)
                                    except Exception:
                                        _clear_py2_exc()
                            closed.append(wid or wcap or wcls)
                        except Exception:
                            pass
                if not (not t_low and has_visible_modal):
                    if not t_norm or _target_matches_window(t_norm, "SkillPlanDockablePanel", "skills", "SkillPlanDockablePanel"):
                        try:
                            from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
                            sp_open = SkillPlanDockablePanel.GetIfOpen()
                            if sp_open is not None and not getattr(sp_open, "destroyed", False) and _is_visible(sp_open):
                                SkillPlanDockablePanel.CloseIfOpen()
                                closed.append("SkillPlanDockablePanel")
                        except Exception:
                            pass
                    try:
                        vs = sm.GetService("viewState") if sm is not None else None
                        sec_vi = getattr(vs, "secondaryInfo", None) if vs is not None else None
                        cur_vi = sec_vi or (vs.GetCurrentViewInfo() if (vs is not None and hasattr(vs, "GetCurrentViewInfo")) else None)
                        cur_vname = _safe_str(getattr(cur_vi, "name", ""), 48)
                        if cur_vname in ("paint_tool", "ship_skinr", "virtual_goods_store", "aurumstore", "starmap_new", "starmap", "systemmap", "systemmap_new", "planet", "shiptree", "career_portal", "skill_plan", "skill_tree", "omega_upsell", "achievements") and (
                            not t_low
                            or _target_matches_view(t_norm, cur_vname)
                            or not closed
                            or any(_target_matches_view(_normalize_window_tok(c), cur_vname) for c in closed)
                        ):
                            if cur_vname == "skill_plan" or "skill" in t_low:
                                try:
                                    from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
                                    SkillPlanDockablePanel.CloseIfOpen()
                                except Exception:
                                    pass
                            try:
                                vs.CloseSecondaryView(cur_vname)
                            except Exception:
                                pass
                            cur_vi2 = vs.GetCurrentViewInfo() if hasattr(vs, "GetCurrentViewInfo") else None
                            if getattr(cur_vi2, "name", "") == cur_vname and hasattr(vs, "primaryInfo") and getattr(vs, "primaryInfo", None) is not None:
                                try:
                                    vs.secondaryInfo = None
                                except Exception:
                                    pass
                            if cur_vname not in closed:
                                closed.append(cur_vname)
                    except Exception:
                        pass
                try:
                    if not t_low or "notif" in t_low or _target_matches_window(t_norm, "notifications", "NotificationCenter", "NotificationCenter"):
                        svcs_c = getattr(sm, "services", {}) if sm is not None else {}
                        n_ui_c = svcs_c.get("notificationUIService")
                        nc_c = getattr(n_ui_c, "notificationCenter", None) if n_ui_c is not None else None
                        if nc_c is not None and getattr(nc_c, "isShowingDrawer", False):
                            if hasattr(nc_c, "CloseDrawer"):
                                nc_c.CloseDrawer()
                            elif hasattr(nc_c, "Hide"):
                                nc_c.Hide()
                            closed.append("notifications")
                except Exception:
                    pass
                try:
                    if t_norm and _target_matches_window(t_norm, "directionalScannerWindow", "Directional Scanner", "DirectionalScannerPalette"):
                        ds_kind_c, ds_cont_c, ds_host_c, _ = _find_directional_scanner_surface(uicore, include_minimized=True)
                        if ds_kind_c == "embedded" and ds_cont_c is not None:
                            mv_c = getattr(ds_host_c, "mapView", None) if ds_host_c is not None else None
                            if mv_c is not None and hasattr(mv_c, "CloseDirectionalScannerPalette"):
                                mv_c.CloseDirectionalScannerPalette()
                            elif mv_c is not None and hasattr(mv_c, "ToggleDirectionalScanner"):
                                mv_c.ToggleDirectionalScanner()
                            elif hasattr(ds_cont_c, "Close"):
                                ds_cont_c.Close()
                            else:
                                ds_cont_c.display = False
                            closed.append("directionalScannerWindow")
                        elif ds_kind_c == "standalone" and ds_cont_c is not None and not any("directionalscanner" in _normalize_window_tok(c) for c in closed):
                            if hasattr(ds_cont_c, "CloseByUser"):
                                ds_cont_c.CloseByUser()
                            elif hasattr(ds_cont_c, "Close"):
                                ds_cont_c.Close()
                            else:
                                ds_cont_c.display = False
                            closed.append("directionalScannerWindow")
                except Exception:
                    pass
                try:
                    _close_context_menus_if_outside(None)
                    _dismiss_pinned_tooltip_if_outside(None)
                except Exception:
                    pass
                result["closed"] = closed

            elif atype == "click_widget":
                hx = int(action["x"]) if action.get("x") is not None else None
                hy = int(action["y"]) if action.get("y") is not None else None
                node = _find_widget(target, layers, hint_x=hx, hint_y=hy)
                if node is None and target and "/" in target:
                    H._sleep_ms(200)
                    node = _find_widget(target, layers, hint_x=hx, hint_y=hy)
                if node is None and not (target and "/" in target) and hx is not None and hy is not None and (hx > 0 or hy > 0):
                    node = _find_widget_at_xy(hx, hy, layers)
                if node is None:
                    result["ok"] = False
                    result["error"] = "widget %r not found in visible uicore layers" % (target,)
                else:
                    _clicked_node_ref[0] = node
                    result["matched_widget"] = "%s(%s)" % (node.__class__.__name__, _safe_str(getattr(node, "name", ""), 48))
                    click_res = _invoke_widget_click(
                        node,
                        right_click=bool(action.get("right_click")),
                        double_click=bool(action.get("double_click")),
                        click_x=hx,
                        click_y=hy,
                    )
                    result["clicked_widget"] = click_res
                    if str(click_res).startswith("NeedsLockedTarget"):
                        result["ok"] = False
                        result["error"] = "Module requires a locked target (loaded targeting cursor); call lock_target first"
                    elif str(click_res).startswith("UserError("):
                        result["ok"] = False
                        result["error"] = "Client action rejected with %s" % (click_res,)
                    elif str(click_res).startswith("StaleModuleButton("):
                        result["ok"] = False
                        result["error"] = "Stale module button skipped (%s)" % (click_res,)
                    if getattr(node, "name", "") == "skillsBtn":
                        try:
                            from eve.client.script.ui.skillPlan.skillPlanDockablePanel import SkillPlanDockablePanel
                            sp_wnd = SkillPlanDockablePanel.GetIfOpen()
                            if sp_wnd is not None and hasattr(sp_wnd, "IsMinimized") and sp_wnd.IsMinimized() and hasattr(sp_wnd, "Maximize"):
                                sp_wnd.Maximize()
                                H._sleep_ms(300)
                        except Exception:
                            pass
                    elif getattr(node, "name", "") in ("StopButton", "stopButton", "MaxSpeedButton"):
                        H._sleep_ms(1200)

            elif atype == "click_xy":
                x_val = int(action.get("x", 0) or 0)
                y_val = int(action.get("y", 0) or 0)
                node = None
                if target:
                    cand = _find_widget(target, layers, hint_x=x_val, hint_y=y_val)
                    if cand is not None:
                        cx, cy, cw, ch = _widget_clipped_rect(cand)
                        if cw > 0 and ch > 0 and (cx - 16) <= x_val <= (cx + cw + 16) and (cy - 16) <= y_val <= (cy + ch + 16):
                            node = cand
                if node is None:
                    node = _find_widget_at_xy(x_val, y_val, layers)
                if node is None:
                    result["ok"] = False
                    result["error"] = "no clickable widget found at (%d, %d)" % (x_val, y_val)
                else:
                    _clicked_node_ref[0] = node
                    result["matched_widget"] = "%s(%s)" % (node.__class__.__name__, _safe_str(getattr(node, "name", ""), 48))
                    click_res = _invoke_widget_click(
                        node,
                        right_click=bool(action.get("right_click")),
                        double_click=bool(action.get("double_click")),
                        click_x=x_val,
                        click_y=y_val,
                    )
                    result["clicked_widget"] = click_res
                    if str(click_res).startswith("NeedsLockedTarget"):
                        result["ok"] = False
                        result["error"] = "Module requires a locked target (loaded targeting cursor); call lock_target first"
                    elif str(click_res).startswith("UserError("):
                        result["ok"] = False
                        result["error"] = "Client action rejected with %s" % (click_res,)
                    elif str(click_res).startswith("StaleModuleButton("):
                        result["ok"] = False
                        result["error"] = "Stale module button skipped (%s)" % (click_res,)
                    result["clicked_xy"] = [x_val, y_val]

            elif atype == "drag_widget":
                hx = int(action["x"]) if action.get("x") is not None else None
                hy = int(action["y"]) if action.get("y") is not None else None
                dx_to = int(action.get("dest_x", 0) or 0)
                dy_to = int(action.get("dest_y", 0) or 0)
                node = _find_widget(target, layers, hint_x=hx, hint_y=hy)
                if node is None and hx is not None and hy is not None and (hx > 0 or hy > 0):
                    node = _find_widget_at_xy(hx, hy, layers)
                if node is None:
                    result["ok"] = False
                    result["error"] = "source widget %r not found for drag" % (target,)
                else:
                    result["dragged_widget"] = _safe_str(getattr(node, "name", ""), 64)
                    dropped = False
                    slider_drag = _set_slider_from_x(node, dx_to if dx_to > 0 else hx)
                    if slider_drag is not None:
                        result["slider_updated"] = slider_drag
                        dropped = True
                    drag_src = None
                    drag_data = None
                    cur_s = node
                    for _ in range(4):
                        if cur_s is None:
                            break
                        if hasattr(cur_s, "GetDragData"):
                            try:
                                drag_data = cur_s.GetDragData()
                                if drag_data:
                                    drag_src = cur_s
                                    break
                            except Exception:
                                pass
                        cur_s = getattr(cur_s, "parent", None)
                    if drag_data and (dx_to > 0 or dy_to > 0):
                        dest_node = _find_widget_at_xy(dx_to, dy_to, layers, for_drop=True)
                        if dest_node is None:
                            dest_node = _find_widget_at_xy(dx_to, dy_to, layers, for_drop=False)
                        cur_d = dest_node
                        for _ in range(8):
                            if cur_d is None:
                                break
                            if _has_real_drop_handler(cur_d):
                                d_cls = cur_d.__class__.__name__
                                try:
                                    cur_d.OnDropData(drag_src, drag_data)
                                    H._sleep_ms(250)
                                    result["dropped_on"] = _safe_str(getattr(cur_d, "name", "") or d_cls, 64)
                                    dropped = True
                                    break
                                except Exception as drop_exc:
                                    if drop_exc.__class__.__name__ == "UserError":
                                        _notify_user_error(drop_exc, async_uthread=True)
                                        H._sleep_ms(150)
                                        result["dropped_on"] = _safe_str(getattr(cur_d, "name", "") or d_cls, 64)
                                        result["drop_user_error"] = _safe_str(getattr(drop_exc, "msg", str(drop_exc)), 80)
                                        result["ok"] = False
                                        result["action_ok"] = False
                                        result["error"] = "Client action rejected with %s" % (_format_user_error(drop_exc),)
                                        dropped = True
                                        break
                            cur_d = getattr(cur_d, "parent", None)
                    _tgt_l = (target or "").lower()
                    _node_name_l = _safe_str(getattr(node, "name", ""), 64).lower()
                    _allow_window_move = (
                        _node_name_l not in ("__content", "__clipper", "maincontainer", "scroll", "scrollcontainer")
                        and not any(k in _tgt_l for k in ("itementry_", "entry_", "blueprint", "modulebutton", "skill", "iconcontainer"))
                        and not any(k in _node_name_l for k in ("itementry_", "iconcontainer"))
                    )
                    if not dropped and not drag_data and _allow_window_move:
                        cur_w = node
                        for _ in range(12):
                            if cur_w is None:
                                break
                            if _is_top_level_window_node(cur_w):
                                try:
                                    wx, wy, ww, wh = _widget_abs_rect(cur_w)
                                    nx, ny, nw, nh = _widget_abs_rect(node)
                                    sx = hx if (hx is not None and hx > 0) else int(nx + nw // 2)
                                    sy = hy if (hy is not None and hy > 0) else int(ny + nh // 2)
                                    desk = getattr(uicore, "desktop", None)
                                    dw = int(getattr(desk, "width", 1280) or 1280) if desk is not None else 1280
                                    dh = int(getattr(desk, "height", 800) or 800) if desk is not None else 800
                                    if dx_to > 0 or dy_to > 0:
                                        new_left = max(0, min(max(0, dw - min(ww, dw) - 4), int(getattr(cur_w, "left", wx) or wx) + (dx_to - sx)))
                                        new_top = max(0, min(max(0, dh - min(wh, dh) - 4), int(getattr(cur_w, "top", wy) or wy) + (dy_to - sy)))
                                    else:
                                        new_left = max(48, min(max(48, dw - ww - 8), wx))
                                        new_top = max(8, min(max(8, dh - wh - 8), wy))
                                    cur_w.left = new_left
                                    cur_w.top = new_top
                                    result["moved_window"] = _safe_str(getattr(cur_w, "windowID", None) or getattr(cur_w, "name", ""), 64)
                                    result["new_pos"] = [new_left, new_top]
                                except Exception:
                                    pass
                                break
                            cur_w = getattr(cur_w, "parent", None)
                    if not dropped and "moved_window" not in result:
                        result["ok"] = False
                        result["action_ok"] = False
                        if drag_data:
                            result["error"] = (
                                "drag_widget on %r found GetDragData but no drop handler at (%s, %s)"
                                % (_safe_str(getattr(node, "name", "") or target, 64), dx_to, dy_to)
                            )
                        else:
                            result["error"] = (
                                "drag_widget on %r had no GetDragData drop target or movable Window ancestor"
                                % (_safe_str(getattr(node, "name", "") or target, 64),)
                            )

            elif atype == "wait":
                result["waited_ms"] = effective_wait_ms

            elif atype == "press_key":
                key_name = _to_utf8(action.get("key", "") or raw_cmd or target).strip()
                key_up = key_name.upper().replace(" ", "").replace("-", "+")
                key_cmd_map = {
                    "F1": "CmdActivateHighPowerSlot1",
                    "F2": "CmdActivateHighPowerSlot2",
                    "F3": "CmdActivateHighPowerSlot3",
                    "F4": "CmdActivateHighPowerSlot4",
                    "F5": "CmdActivateHighPowerSlot5",
                    "F6": "CmdActivateHighPowerSlot6",
                    "F7": "CmdActivateHighPowerSlot7",
                    "F8": "CmdActivateHighPowerSlot8",
                    "ALT+F1": "CmdActivateMediumPowerSlot1",
                    "ALT+F2": "CmdActivateMediumPowerSlot2",
                    "ALT+F3": "CmdActivateMediumPowerSlot3",
                    "ALT+F4": "CmdActivateMediumPowerSlot4",
                    "ALT+F5": "CmdActivateMediumPowerSlot5",
                    "ALT+F6": "CmdActivateMediumPowerSlot6",
                    "ALT+F7": "CmdActivateMediumPowerSlot7",
                    "ALT+F8": "CmdActivateMediumPowerSlot8",
                    "CTRL+F1": "CmdActivateLowPowerSlot1",
                    "CTRL+F2": "CmdActivateLowPowerSlot2",
                    "CTRL+F3": "CmdActivateLowPowerSlot3",
                    "CTRL+F4": "CmdActivateLowPowerSlot4",
                    "CTRL+F5": "CmdActivateLowPowerSlot5",
                    "CTRL+F6": "CmdActivateLowPowerSlot6",
                    "CTRL+F7": "CmdActivateLowPowerSlot7",
                    "CTRL+F8": "CmdActivateLowPowerSlot8",
                    "F9": "CmdToggleSolarSystemMap",
                    "F10": "CmdToggleMap",
                    "F11": "OpenMapBrowser",
                    "F12": "OpenHelp",
                    "ESCAPE": "OnEsc",
                    "ESC": "OnEsc",
                    "CTRL+SPACE": "CmdStopShip",
                    "ALT+C": "OpenCargoHoldOfActiveShip",
                    "ALT+F": "OpenFitting",
                    "ALT+G": "OpenCorporationPanel",
                    "ALT+E": "OpenPeopleAndPlaces",
                    "ALT+R": "OpenMarket",
                    "ALT+M": "OpenJournal",
                    "ALT+T": "OpenAssets",
                    "ALT+X": "OpenSkillsWindow",
                    "ALT+W": "OpenWallet",
                    "ALT+N": "OpenNotepad",
                    "ALT+I": "OpenIndustry",
                    "ALT+P": ("OpenProbeScanner",),
                    "ALT+D": ("OpenDirectionalScanner",),
                    "ALT+B": ("OpenFleet", "OpenContracts"),
                }
                cmd_svc = getattr(uicore, "cmd", None) if uicore is not None else None
                reg = getattr(uicore, "registry", None) if uicore is not None else None
                if key_up in ("RETURN", "ENTER", "KP_ENTER"):
                    focus = reg.GetFocus() if (reg is not None and hasattr(reg, "GetFocus")) else None
                    edit_f = _resolve_editable_widget(focus) if focus is not None else None
                    handled_ret = False
                    for cand_node in (edit_f, focus):
                        if cand_node is None:
                            continue
                        ctrl_f = getattr(cand_node, "controller", None)
                        for obj_t, mname in (
                            (ctrl_f, "on_return"),
                            (cand_node, "on_return"),
                            (cand_node, "OnReturn"),
                            (cand_node, "Confirm"),
                            (cand_node, "OnConfirm"),
                        ):
                            fn = getattr(obj_t, mname, None) if obj_t is not None else None
                            if callable(fn):
                                fn()
                                result["executed_cmd"] = "Return(%s.%s)" % (
                                    cand_node.__class__.__name__,
                                    mname,
                                )
                                handled_ret = True
                                break
                        if handled_ret:
                            break
                    if not handled_ret and focus is not None and callable(getattr(focus, "OnClick", None)):
                        focus.OnClick()
                        result["executed_cmd"] = "Return(OnClick:%s)" % (focus.__class__.__name__,)
                        handled_ret = True
                    if not handled_ret and cmd_svc is not None and hasattr(cmd_svc, "Confirm"):
                        try:
                            cmd_svc.Confirm()
                            result["executed_cmd"] = "cmd.Confirm"
                            handled_ret = True
                        except Exception:
                            pass
                    if not handled_ret:
                        result["executed_cmd"] = "Return(noop_no_focused_confirm)"
                elif key_up in ("TAB", "SHIFT+TAB"):
                    all_edits = []
                    def _collect_edits(n, d=0):
                        if n is None or d > 16 or not _is_pickable(n):
                            return
                        if _is_editable_input_node(n):
                            all_edits.append(n)
                        for ch in list(getattr(n, "children", []) or []):
                            _collect_edits(ch, d + 1)
                    if uicore is not None:
                        for _, lobj in _iter_visible_layers(uicore):
                            _collect_edits(lobj, 0)
                    if all_edits:
                        cur_focus = reg.GetFocus() if (reg is not None and hasattr(reg, "GetFocus")) else None
                        cur_edit = _resolve_editable_widget(cur_focus) if cur_focus is not None else None
                        if cur_edit in all_edits:
                            idx_cur = all_edits.index(cur_edit)
                            delta = -1 if key_up == "SHIFT+TAB" else 1
                            nxt_edit = all_edits[(idx_cur + delta) % len(all_edits)]
                        else:
                            nxt_edit = all_edits[-1] if key_up == "SHIFT+TAB" else all_edits[0]
                        if reg is not None and hasattr(reg, "SetFocus"):
                            reg.SetFocus(nxt_edit)
                        result["executed_cmd"] = "%s(focused:%s)" % (
                            key_up,
                            _safe_str(getattr(nxt_edit, "name", nxt_edit.__class__.__name__), 48),
                        )
                    else:
                        tab_cmd = "OnShiftTab" if key_up == "SHIFT+TAB" else "OnTab"
                        tab_fn = getattr(cmd_svc, tab_cmd, None) if cmd_svc is not None else None
                        if callable(tab_fn):
                            tab_fn()
                            result["executed_cmd"] = tab_cmd
                        else:
                            result["executed_cmd"] = "%s(noop_no_visible_editable)" % (key_up,)
                else:
                    raw_mapped = key_cmd_map.get(key_up, key_name)
                    if isinstance(raw_mapped, tuple):
                        resolved_cmd = next((c for c in raw_mapped if cmd_svc is not None and hasattr(cmd_svc, c)), raw_mapped[0])
                    else:
                        resolved_cmd = raw_mapped
                    if cmd_svc is not None and hasattr(cmd_svc, resolved_cmd):
                        if resolved_cmd.startswith("Open") or "Toggle" in resolved_cmd:
                            _close_context_menus_if_outside(None)
                            _dismiss_pinned_tooltip_if_outside(None)
                        elif resolved_cmd == "OnEsc":
                            _dismiss_pinned_tooltip_if_outside(None)
                        getattr(cmd_svc, resolved_cmd)()
                        if resolved_cmd == "OnEsc":
                            _dismiss_pinned_tooltip_if_outside(None)
                        if getattr(cmd_svc, "combatFunctionLoaded", None) is not None and hasattr(cmd_svc, "UnloadCombatCommand"):
                            H._sleep_ms(50)
                            try:
                                cmd_svc.UnloadCombatCommand()
                            except Exception:
                                pass
                            if resolved_cmd.startswith("CmdActivate"):
                                result["ok"] = False
                                result["error"] = "Command %s requires a locked target (loaded targeting cursor); call lock_target first" % (resolved_cmd,)
                        result["executed_cmd"] = resolved_cmd
                    else:
                        result["ok"] = False
                        result["error"] = "key/cmd %r (resolved=%r) not found on uicore.cmd" % (key_name, resolved_cmd)

            elif atype == "type_text":
                explicit_xy = bool(
                    action.get("x") is not None
                    and action.get("y") is not None
                    and (int(action["x"]) > 0 or int(action["y"]) > 0)
                )
                raw_txt_input = action.get("text")
                if raw_txt_input in (None, ""):
                    raw_txt_input = action.get("command") or action.get("slash_cmd")
                used_target_as_text = False
                if raw_txt_input in (None, ""):
                    if raw_target and (
                        "/" in raw_target
                        or "_" in raw_target
                        or (
                            not explicit_xy
                            and _resolve_editable_widget(_find_widget(raw_target, layers)) is not None
                        )
                    ):
                        result["ok"] = False
                        result["error"] = "type_text targeting widget %r requires non-empty 'command' or 'text'" % (raw_target,)
                        return
                    raw_target_unstripped = _to_utf8(action.get("target") or "")
                    raw_txt_input = raw_target_unstripped or raw_target
                    used_target_as_text = True
                u_txt = _to_unicode(raw_txt_input)
                should_submit = (
                    bool(action.get("submit"))
                    or u_txt.endswith(u"\n")
                    or u_txt.endswith(u"\r")
                    or u_txt.endswith(u"\\n")
                    or u_txt.endswith(u"\\r")
                )
                while u_txt.endswith((u"\n", u"\r", u"\\n", u"\\r")):
                    if u_txt.endswith(u"\\r\\n"):
                        u_txt = u_txt[:-4]
                    elif u_txt.endswith((u"\\n", u"\\r")):
                        u_txt = u_txt[:-2]
                    else:
                        u_txt = u_txt.rstrip(u"\r\n")
                reg = getattr(uicore, "registry", None) if uicore is not None else None
                edit_node = None
                explicit_target = bool(
                    not used_target_as_text
                    and raw_target
                    and (action.get("text") or raw_cmd or "/" in raw_target or "_" in raw_target)
                )
                if explicit_target:
                    cand = _find_widget(raw_target, layers)
                    edit_node = _resolve_editable_widget(cand)
                if edit_node is None and explicit_xy:
                    cand = _find_widget_at_xy(int(action["x"]), int(action["y"]), layers)
                    edit_node = _resolve_editable_widget(cand)
                if edit_node is None and not explicit_target and not explicit_xy:
                    focus = reg.GetFocus() if (reg is not None and hasattr(reg, "GetFocus")) else None
                    edit_node = _resolve_editable_widget(focus)
                if edit_node is None and raw_target and not explicit_target and not used_target_as_text:
                    cand = _find_widget(raw_target, layers)
                    edit_node = _resolve_editable_widget(cand)
                if edit_node is None and not explicit_target and not explicit_xy:
                    edit_node = _find_any_visible_editable(layers)
                if edit_node is not None and _is_eveui_text_field(edit_node):
                    try:
                        if reg is not None and hasattr(reg, "SetFocus"):
                            reg.SetFocus(edit_node)
                    except Exception:
                        pass
                    ctrl = edit_node.controller
                    try:
                        ctrl.clear_text()
                    except Exception:
                        pass
                    if u_txt:
                        if hasattr(edit_node, "insert_text"):
                            edit_node.insert_text(u_txt)
                        elif hasattr(ctrl, "replace_text"):
                            ctrl.replace_text(u_txt)
                        else:
                            ctrl.insert_text(u_txt)
                    result["typed_into"] = _safe_str(getattr(edit_node, "name", ""), 64)
                    if should_submit:
                        _invoke_text_submit_candidates(
                            ((ctrl, "on_return"), (edit_node, "on_return"), (edit_node, "OnReturn"), (edit_node, "Confirm")),
                            result,
                        )
                elif edit_node is not None and (hasattr(edit_node, "SetValue") or hasattr(edit_node, "SetText")):
                    try:
                        if reg is not None and hasattr(reg, "SetFocus"):
                            reg.SetFocus(edit_node)
                    except Exception:
                        pass
                    prefix_val = getattr(edit_node, "prefix", None)
                    if prefix_val and type(prefix_val).__name__ in ("str", "unicode") and not u_txt.startswith(prefix_val):
                        u_txt = prefix_val + u_txt
                    if hasattr(edit_node, "SetValue"):
                        try:
                            edit_node.SetValue(u_txt)
                        except (ValueError, TypeError) as val_exc:
                            result["ok"] = False
                            result["error"] = "numeric/value input %r rejected %r: %s" % (
                                _safe_str(getattr(edit_node, "name", edit_node.__class__.__name__), 48),
                                _safe_str(u_txt, 48),
                                val_exc,
                            )
                            return
                    else:
                        edit_node.SetText(u_txt)
                    if hasattr(edit_node, "OnChange"):
                        try:
                            edit_node.OnChange(u_txt)
                        except Exception:
                            pass
                    result["typed_into"] = _safe_str(getattr(edit_node, "name", ""), 64)
                    if should_submit:
                        _invoke_text_submit_candidates(
                            ((edit_node, "OnReturn"), (edit_node, "Confirm"), (edit_node, "OnConfirm")),
                            result,
                        )
                else:
                    result["ok"] = False
                    result["error"] = "no editable input widget found for target=%r" % (raw_target,)

            elif atype == "run_slash":
                scmd = _to_utf8(
                    action.get("slash_cmd", "")
                    or (target if target.startswith("/") else "")
                    or raw_cmd
                    or target
                    or "/whoami"
                )
                try:
                    res = _run_gm_slash(sm, scmd)
                    if scmd.strip().lower().split()[:1] == ["/cleartimers"]:
                        try:
                            if sm is not None:
                                sm.RemoteSvc("timeControl").ClearSessionTimer()
                        except Exception:
                            pass
                        if sess is not None and hasattr(sess, "nextSessionChange"):
                            try:
                                # Keep a Python 2 long(0) so both DoSimClockRebase (+= delta_long) and FmtDateEng (requires long) work.
                                sess.nextSessionChange = getattr(__builtin__, "long", int)(0)
                            except Exception:
                                pass
                    result["slash_cmd"] = scmd
                    result["slash_result"] = _safe_str(res if isinstance(res, str) else repr(res), 2000)
                    try:
                        _sid = None
                        if isinstance(res, dict):
                            _sid = res.get("spawnedID") or res.get("npcID") or res.get("itemID")
                        elif res is not None:
                            _sid = (
                                getattr(res, "spawnedID", None)
                                or getattr(res, "npcID", None)
                                or getattr(res, "itemID", None)
                            )
                        if _sid:
                            result["spawned_id"] = int(_sid)
                    except Exception:
                        pass
                except Exception as exc:
                    result["ok"] = False
                    result["error"] = "slash %r raised: %r" % (scmd, exc)

            elif atype == "station_command":
                sub = target.lower()
                if sub == "undock":
                    try:
                        vs = sm.GetService("viewState") if sm is not None else None
                        sec_vi = getattr(vs, "secondaryInfo", None) if vs is not None else None
                        if sec_vi is not None:
                            sec_name = _safe_str(getattr(sec_vi, "name", ""), 48)
                            if sec_name:
                                vs.CloseSecondaryView(sec_name)
                    except Exception:
                        pass
                    H._undock_ship(sm, allow_time_control=allow_time_control)
                    result["undocked"] = True
                elif sub == "open_plex_store":
                    result.update(_open_client_window(sm, "nes"))
                elif sub == "create_skill_plan":
                    sp_svc = sm.GetService("skillPlanSvc")
                    plans = sp_svc.GetAllPersonal()
                    result["personal_plans_count"] = len(plans) if plans is not None else 0
                elif sub == "inject_and_train_skill":
                    skill_tid = int(action.get("type_id", 3300) or 3300)
                    try:
                        sq_svc = sm.GetService("skillqueue")
                    except Exception:
                        sq_svc = getattr(sm, "services", {}).get("skillqueue") or getattr(sm, "services", {}).get("skillQueue")
                    cur_lvl = 0
                    sk_obj = None
                    existing_q = []
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
                                if q_tid > 0 and q_lvl > 0:
                                    existing_q.append((q_tid, q_lvl))
                                if q_tid == skill_tid and q_lvl > cur_lvl:
                                    cur_lvl = q_lvl
                    except Exception:
                        pass
                    if sk_obj is None:
                        purchased = sm.RemoteSvc("skillMgr2").PurchaseSkills([skill_tid])
                        for _ in range(10):
                            try:
                                if sk_svc is not None and hasattr(sk_svc, "GetSkill") and sk_svc.GetSkill(skill_tid) is not None:
                                    break
                            except Exception:
                                pass
                            try:
                                blue.pyos.synchro.SleepWallclock(50)
                            except Exception:
                                pass
                    else:
                        purchased = "already_injected"
                    target_lvl = min(5, max(1, cur_lvl + 1))
                    new_q = list(existing_q) + [(skill_tid, target_lvl)]
                    if cur_lvl >= 5:
                        result["already_max_level"] = 5
                    elif sq_svc is not None and hasattr(sq_svc, "BeginTransaction"):
                        try:
                            if not sq_svc.IsTransactionOpen():
                                sq_svc.BeginTransaction()
                            try:
                                sq_svc.AddSkillToQueue(skill_tid, target_lvl)
                            except Exception as _inner_sq_exc:
                                if getattr(_inner_sq_exc, "msg", None) == "QueueSkillNotUploaded" and sk_obj is None:
                                    try:
                                        blue.pyos.synchro.SleepWallclock(150)
                                    except Exception:
                                        pass
                                    try:
                                        if sq_svc.IsTransactionOpen() and hasattr(sq_svc, "RollbackTransaction"):
                                            sq_svc.RollbackTransaction()
                                        sq_svc.BeginTransaction()
                                    except Exception:
                                        pass
                                    sq_svc.AddSkillToQueue(skill_tid, target_lvl)
                                else:
                                    raise
                            sq_svc.CommitTransaction(activate=True)
                        except Exception as _sq_exc:
                            try:
                                if sq_svc.IsTransactionOpen() and hasattr(sq_svc, "RollbackTransaction"):
                                    sq_svc.RollbackTransaction()
                            except Exception:
                                pass
                            if _sq_exc.__class__.__name__ == "UserError":
                                raise
                            sm.RemoteSvc("skillMgr2").SaveSkillQueue(new_q, True)
                    else:
                        sm.RemoteSvc("skillMgr2").SaveSkillQueue(new_q, True)
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
                try:
                    vs = sm.GetService("viewState") if sm is not None else None
                    sec_vi = getattr(vs, "secondaryInfo", None) if vs is not None else None
                    if sec_vi is not None:
                        sec_name = _safe_str(getattr(sec_vi, "name", ""), 48)
                        if sec_name:
                            vs.CloseSecondaryView(sec_name)
                except Exception:
                    pass
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
                    if not item_id and sub not in ("jump_gate", "stop") and bp is not None:
                        sid = int(getattr(sess, "shipid", 0) or 0) if sess is not None else 0
                        item_id, resolve_err = _resolve_space_command_item_id(bp, sid, sub, raw_target)
                        if resolve_err:
                            result["ok"] = False
                            result["error"] = resolve_err
                    if not result.get("ok", True):
                        pass
                    elif sub == "warp_to":
                        _WARP_TELEMETRY["last_warp_saw_mode3"] = False
                        _WARP_TELEMETRY["last_warp_saw_fx_warping"] = False
                        _WARP_TELEMETRY["last_warp_saw_signal"] = False
                        _WARP_TELEMETRY["last_warp_peak_speed_mps"] = 0.0
                        _WARP_TELEMETRY["last_warp_samples"] = []
                        beyonce.CmdWarpToStuff("item", int(item_id), float(dist))
                        # Sample real-time warp tunnel & ball mode over 3.2 seconds WITHOUT instant 240s clock skip!
                        _req_bursts = list(action.get("burst_paths") or [])
                        _captured_bursts = []
                        _burst_ticks = (1, 3, 6)
                        for _w_idx in range(8):
                            H._sleep_sim_ms(400)
                            smp = _sample_ego_warp_state(sm)
                            _WARP_TELEMETRY["last_warp_samples"].append(smp)
                            if smp["mode"] == 3:
                                _WARP_TELEMETRY["last_warp_saw_mode3"] = True
                            if "effects.Warping" in smp["fx_activations"] or smp["has_warp_dest_cache"]:
                                _WARP_TELEMETRY["last_warp_saw_fx_warping"] = True
                            if smp["speed_mps"] > _WARP_TELEMETRY["last_warp_peak_speed_mps"]:
                                _WARP_TELEMETRY["last_warp_peak_speed_mps"] = smp["speed_mps"]
                            if _req_bursts and _w_idx in _burst_ticks:
                                _bp_out = _req_bursts.pop(0)
                                try:
                                    _bres = _capture_trinity_screenshot(_bp_out, skip_sleep=True)
                                    if isinstance(_bres, dict) and _bres.get("ok"):
                                        _captured_bursts.append(_bp_out)
                                except Exception:
                                    pass
                        if _captured_bursts:
                            result["captured_burst_paths"] = _captured_bursts
                        result["warp_target_id"] = int(item_id)
                        result["warp_realtime_telemetry"] = {
                            "saw_mode3": _WARP_TELEMETRY["last_warp_saw_mode3"],
                            "saw_fx_warping": _WARP_TELEMETRY["last_warp_saw_fx_warping"],
                            "saw_signal": _WARP_TELEMETRY["last_warp_saw_signal"],
                            "warp_signal_count": 1 if _WARP_TELEMETRY["last_warp_saw_signal"] else 0,
                            "started_count": int(_WARP_TELEMETRY.get("started_count", 0) or 0),
                            "finished_count": int(_WARP_TELEMETRY.get("finished_count", 0) or 0),
                            "in_warp": bool(_WARP_TELEMETRY.get("in_warp", False)),
                            "peak_speed_mps": _WARP_TELEMETRY["last_warp_peak_speed_mps"],
                        }
                        result["warp_telemetry"] = result["warp_realtime_telemetry"]
                    elif sub == "align_to":
                        beyonce.CmdAlignTo(int(item_id))
                    elif sub in ("approach", "orbit", "keep_at_range", "stop"):
                        if sub == "approach":
                            beyonce.CmdFollowBall(int(item_id), 50.0)
                            want_mode = 1
                        elif sub == "orbit":
                            beyonce.CmdOrbit(int(item_id), float(dist or 5000.0))
                            want_mode = 4
                        elif sub == "keep_at_range":
                            beyonce.CmdFollowBall(int(item_id), float(dist or 10000.0))
                            want_mode = 1
                        else:
                            beyonce.CmdStop()
                            want_mode = 2
                        try:
                            sess_m = getattr(__builtin__, "session", None)
                            sid_m = int(getattr(sess_m, "shipid", 0) or 0) if sess_m is not None else 0
                            mich_m = sm.services.get("michelle") if hasattr(sm, "services") else None
                            deadline_m = time.time() + 2.2
                            while sid_m and mich_m is not None and time.time() < deadline_m:
                                bp_m = mich_m.GetBallpark()
                                ball_m = bp_m.GetBall(sid_m) if bp_m is not None else None
                                if ball_m is not None and int(getattr(ball_m, "mode", -1) or -1) == want_mode:
                                    break
                                H._sleep_sim_ms(100)
                        except Exception:
                            pass
                    elif sub == "dock":
                        dock_id = int(item_id or 60003760)
                        sess_d = getattr(__builtin__, "session", None)
                        sid_d = int(getattr(sess_d, "shipid", 0) or 0) if sess_d is not None else 0
                        mich_d = sm.services.get("michelle") if hasattr(sm, "services") else None
                        bp_d = mich_d.GetBallpark() if (sid_d and mich_d is not None) else None
                        ball_d = bp_d.GetBall(sid_d) if (bp_d is not None and sid_d) else None
                        if (
                            (ball_d is not None and int(getattr(ball_d, "mode", 0) or 0) == 3)
                            or _WARP_TELEMETRY.get("in_warp")
                            or (getattr(H, "_WARP_SIGNAL_STATE", None) or {}).get("in_warp")
                        ):
                            H._wait_warp_fx_settled(
                                sm,
                                timeout_sec=(12.0 if not allow_time_control else 6.0),
                                allow_time_control=allow_time_control,
                            )
                        H._wait_view_settled(sm)
                        if allow_time_control:
                            try:
                                sm.RemoteSvc("timeControl").ClearSessionTimer()
                            except Exception:
                                pass
                        dock_started_at = time.time()
                        dock_loop_deadline = dock_started_at + (35.0 if not allow_time_control else 18.5)
                        def _dock_surf_dist():
                            cur_bp = mich_d.GetBallpark() if (sid_d and mich_d is not None) else bp_d
                            if cur_bp is None or not hasattr(cur_bp, "GetBall"):
                                return None
                            cur_tb = cur_bp.GetBall(dock_id)
                            if cur_tb is None:
                                return None
                            if hasattr(cur_bp, "GetSurfaceDist") and sid_d:
                                try:
                                    val = float(cur_bp.GetSurfaceDist(int(sid_d), dock_id))
                                    if val >= 0.0:
                                        return val
                                except Exception:
                                    pass
                            try:
                                raw_sd = getattr(cur_tb, "surfaceDist", None)
                                if raw_sd is not None:
                                    val = float(raw_sd)
                                    if val >= 0.0:
                                        return val
                            except Exception:
                                pass
                            return None
                        def _approach_dock_within(limit_m, max_iters):
                            beyonce.CmdFollowBall(dock_id, 0.0)
                            cur_sd = _dock_surf_dist()
                            steps = 0
                            iter_cap = max(max_iters, 50) if not allow_time_control else max_iters
                            for _ in range(iter_cap):
                                if time.time() >= dock_loop_deadline:
                                    break
                                if cur_sd is not None and cur_sd <= float(limit_m):
                                    break
                                if allow_time_control:
                                    adv_secs = 15 if (cur_sd is not None and cur_sd > 12000.0) else (10 if (cur_sd is not None and cur_sd > 4500.0) else 8)
                                    try:
                                        sm.RemoteSvc("timeControl").AdvanceClock(adv_secs)
                                    except Exception:
                                        pass
                                    H._sleep_sim_ms(200)
                                else:
                                    H._sleep_sim_ms(500)
                                steps += 1
                                cur_sd = _dock_surf_dist()
                            if allow_time_control:
                                try:
                                    sm.RemoteSvc("timeControl").ClearSessionTimer()
                                except Exception:
                                    pass
                            return steps
                        def _already_docked():
                            s_cur = getattr(__builtin__, "session", None)
                            if s_cur is None:
                                return False
                            return bool(
                                getattr(s_cur, "stationid", None)
                                or getattr(s_cur, "structureid", None)
                                or getattr(s_cur, "solarsystemid", None) is None
                            )

                        pending_dock_exc = None
                        if not _already_docked():
                            bp_d = mich_d.GetBallpark() if (sid_d and mich_d is not None) else None
                            target_ball_d = bp_d.GetBall(dock_id) if (bp_d is not None and hasattr(bp_d, "GetBall")) else None
                            surf_d = _dock_surf_dist()
                            warp_too_short = False
                            if (bp_d is not None and target_ball_d is None) or (surf_d is not None and surf_d >= 150000.0):
                                try:
                                    beyonce.CmdWarpToStuff("item", dock_id, 0.0)
                                except Exception as warp_exc:
                                    warp_msg = str(warp_exc)
                                    if "MonikerSessionCheckFailure" in warp_msg and _already_docked():
                                        warp_too_short = False
                                    elif "WarpDistanceTooShort" not in warp_msg and "WarpTooShort" not in warp_msg:
                                        raise
                                    else:
                                        warp_too_short = True
                                if not warp_too_short and not _already_docked():
                                    if allow_time_control:
                                        try:
                                            sm.RemoteSvc("timeControl").AdvanceClock(180)
                                        except Exception:
                                            pass
                                    H._sleep_sim_ms(400)
                                    H._wait_warp_fx_settled(
                                        sm,
                                        timeout_sec=(35.0 if not allow_time_control else 8.0),
                                        allow_time_control=allow_time_control,
                                    )
                                    surf_d = _dock_surf_dist()
                            if not _already_docked():
                                if warp_too_short or (surf_d is not None and surf_d > 2400.0):
                                    _approach_dock_within(2200.0, 12)
                                if not allow_time_control:
                                    H._wait_session_timer_clear(max_wait_sec=12.0)
                                try:
                                    sm.RemoteSvc("stationSvc").Dock(dock_id)
                                except Exception as dock_exc:
                                    if "OutsideDockingPerimeter" not in str(dock_exc) and "DockingApproach" not in str(dock_exc):
                                        raise
                                    pending_dock_exc = dock_exc
                                    advanced = _approach_dock_within(2000.0, 10)
                                    if advanced > 0:
                                        try:
                                            sm.RemoteSvc("stationSvc").Dock(dock_id)
                                            pending_dock_exc = None
                                        except Exception as retry_exc:
                                            if "OutsideDockingPerimeter" not in str(retry_exc) and "DockingApproach" not in str(retry_exc):
                                                raise
                                            pending_dock_exc = retry_exc
                                    if pending_dock_exc is not None:
                                        # GetSurfaceDist/surfaceDist can report stale <=2000m (e.g. 0.0 on
                                        # large stations) while server perimeter check still rejects Dock.
                                        # Advance simulation with CmdFollowBall active and retry Dock
                                        # within dock_loop_deadline instead of stopping at 0 iterations.
                                        for _ in range(16 if not allow_time_control else 8):
                                            if time.time() >= dock_loop_deadline:
                                                break
                                            if allow_time_control:
                                                try:
                                                    sm.RemoteSvc("timeControl").AdvanceClock(12)
                                                except Exception:
                                                    pass
                                                H._sleep_sim_ms(200)
                                                try:
                                                    sm.RemoteSvc("timeControl").ClearSessionTimer()
                                                except Exception:
                                                    pass
                                            else:
                                                H._sleep_sim_ms(600)
                                                H._wait_session_timer_clear(max_wait_sec=4.0)
                                            try:
                                                sm.RemoteSvc("stationSvc").Dock(dock_id)
                                                pending_dock_exc = None
                                                break
                                            except Exception as retry_exc:
                                                if "OutsideDockingPerimeter" not in str(retry_exc) and "DockingApproach" not in str(retry_exc):
                                                    raise
                                                pending_dock_exc = retry_exc
                                    if pending_dock_exc is not None:
                                        raise pending_dock_exc
                        dl_d = time.time() + (16.0 if not allow_time_control else 10.0)
                        while time.time() < dl_d:
                            sess_d = getattr(__builtin__, "session", None)
                            if (
                                (getattr(sess_d, "stationid", None) == dock_id or getattr(sess_d, "structureid", None) == dock_id)
                                and not getattr(sess_d, "mutating", 0)
                                and getattr(sess_d, "changing", None) is None
                            ):
                                break
                            H._sleep_ms(100)
                        if allow_time_control:
                            try:
                                sm.RemoteSvc("timeControl").ClearSessionTimer()
                            except Exception:
                                pass
                        H._ensure_view(sm, "hangar")
                    elif sub == "jump_gate":
                        sess_j = getattr(__builtin__, "session", None)
                        sid_j = int(getattr(sess_j, "shipid", 0) or 0) if sess_j is not None else 0
                        pre_sys = (
                            getattr(sess_j, "solarsystemid2", None)
                            or getattr(sess_j, "solarsystemid", None)
                        )
                        gate_id = int(item_id or 0)
                        slims_j = getattr(bp, "slimItems", {}) if bp is not None else {}
                        if (
                            gate_id
                            and action.get("_inferred_item_id")
                            and isinstance(slims_j, dict)
                            and slims_j
                            and gate_id not in slims_j
                        ):
                            gate_id = 0
                        if not gate_id and bp is not None:
                            q_low = _to_utf8(action.get("target") or "").strip().lower()
                            slims_j = slims_j or {}
                            is_generic_q = q_low in ("", "stargate", "gate", "nearest", "any")
                            cands = []
                            unmatched = []
                            for bid, slim in slims_j.items():
                                if int(bid) == sid_j or int(getattr(slim, "groupID", 0) or 0) != 10:
                                    continue
                                gname = _safe_str(_resolve_ball_name(slim, bid), 64)
                                b_j = bp.GetBall(bid)
                                bdist = _resolve_ball_surface_dist(bp, sid_j, bid, ball=b_j)
                                if is_generic_q:
                                    cands.append((0, bdist, int(bid), gname))
                                else:
                                    st_tier = _match_stargate_target_tier(slim, gname, q_low)
                                    if st_tier is not None:
                                        cands.append((st_tier, bdist, int(bid), gname))
                                    else:
                                        unmatched.append((bdist, gname))
                            if cands:
                                cands.sort(key=lambda t: (t[0], t[1], t[2]))
                                gate_id = cands[0][2]
                                result["resolved_gate_name"] = cands[0][3]
                            elif unmatched:
                                unmatched.sort(key=lambda t: t[0])
                                avail = ", ".join(t[1] for t in unmatched[:6])
                                result["ok"] = False
                                result["error"] = "no Stargate matching %r (available: %s)" % (action.get("target"), avail)
                        if not gate_id and not result.get("error"):
                            result["ok"] = False
                            result["error"] = "no Stargate (groupID=10) found in solar system ballpark"
                        elif gate_id:
                            mich_j = sm.GetService("michelle")
                            bp_j = mich_j.GetBallpark() if mich_j is not None else None
                            target_ball_j = bp_j.GetBall(gate_id) if (bp_j is not None and hasattr(bp_j, "GetBall")) else None
                            surf_j = None
                            if target_ball_j is not None:
                                try:
                                    if bp_j is not None and hasattr(bp_j, "GetSurfaceDist") and sid_j:
                                        surf_j = float(bp_j.GetSurfaceDist(int(sid_j), gate_id))
                                    else:
                                        surf_j = float(getattr(target_ball_j, "surfaceDist", 0.0) or 0.0)
                                except Exception:
                                    surf_j = None
                            warp_too_short_j = False
                            if (bp_j is not None and target_ball_j is None) or (surf_j is not None and surf_j >= 150000.0):
                                try:
                                    beyonce.CmdWarpToStuff("item", gate_id, 0.0)
                                except Exception as warp_exc_j:
                                    if "WarpDistanceTooShort" not in str(warp_exc_j) and "WarpTooShort" not in str(warp_exc_j):
                                        raise
                                    warp_too_short_j = True
                                if not warp_too_short_j:
                                    if allow_time_control:
                                        try:
                                            sm.RemoteSvc("timeControl").AdvanceClock(180)
                                        except Exception:
                                            pass
                                    H._sleep_sim_ms(400)
                                    H._wait_warp_fx_settled(
                                        sm,
                                        timeout_sec=(35.0 if not allow_time_control else 8.0),
                                        allow_time_control=allow_time_control,
                                    )
                                    bp_j = mich_j.GetBallpark() if mich_j is not None else None
                                    target_ball_j = bp_j.GetBall(gate_id) if (bp_j is not None and hasattr(bp_j, "GetBall")) else None
                                    surf_j = None
                                    if target_ball_j is not None:
                                        try:
                                            if bp_j is not None and hasattr(bp_j, "GetSurfaceDist") and sid_j:
                                                surf_j = float(bp_j.GetSurfaceDist(int(sid_j), gate_id))
                                            else:
                                                surf_j = float(getattr(target_ball_j, "surfaceDist", 0.0) or 0.0)
                                        except Exception:
                                            surf_j = None
                            if warp_too_short_j or surf_j is None or surf_j > 2400.0:
                                beyonce.CmdFollowBall(gate_id, 0.0)
                                gate_approach_dl = time.time() + (25.0 if not allow_time_control else 18.0)
                                gate_iters = 50 if not allow_time_control else 14
                                for _ in range(gate_iters):
                                    if time.time() >= gate_approach_dl:
                                        break
                                    if allow_time_control:
                                        try:
                                            sm.RemoteSvc("timeControl").AdvanceClock(8)
                                        except Exception:
                                            pass
                                        H._sleep_sim_ms(200)
                                    else:
                                        H._sleep_sim_ms(500)
                                    target_ball_j = bp_j.GetBall(gate_id) if (bp_j is not None and hasattr(bp_j, "GetBall")) else None
                                    if target_ball_j is not None:
                                        try:
                                            cur_sd_j = (
                                                float(bp_j.GetSurfaceDist(int(sid_j), gate_id))
                                                if (bp_j is not None and hasattr(bp_j, "GetSurfaceDist") and sid_j)
                                                else float(getattr(target_ball_j, "surfaceDist", 0.0) or 0.0)
                                            )
                                            if cur_sd_j <= 2200.0:
                                                break
                                        except Exception:
                                            pass
                            if allow_time_control:
                                try:
                                    sm.RemoteSvc("timeControl").ClearSessionTimer()
                                except Exception:
                                    pass
                            else:
                                H._wait_session_timer_clear(max_wait_sec=12.0)
                            beyonce.CmdStargateJump(gate_id)
                            dl_j = time.time() + (18.0 if not allow_time_control else 12.0)
                            post_sys = pre_sys
                            while time.time() < dl_j:
                                sess_now = getattr(__builtin__, "session", None)
                                cur_sys = (
                                    getattr(sess_now, "solarsystemid2", None)
                                    or getattr(sess_now, "solarsystemid", None)
                                )
                                if (
                                    cur_sys is not None
                                    and cur_sys != pre_sys
                                    and not getattr(sess_now, "mutating", 0)
                                    and getattr(sess_now, "changing", None) is None
                                ):
                                    post_sys = cur_sys
                                    break
                                H._sleep_sim_ms(200)
                            if allow_time_control:
                                try:
                                    sm.RemoteSvc("timeControl").ClearSessionTimer()
                                except Exception:
                                    pass
                            H._wait_view_settled(sm)
                            for _ in range(20):
                                bp_done = mich_j.GetBallpark() if mich_j is not None else None
                                slims_done = getattr(bp_done, "slimItems", None) if bp_done is not None else None
                                if (
                                    bp_done is not None
                                    and isinstance(slims_done, dict)
                                    and (
                                        any(int(getattr(s, "groupID", 0) or 0) == 10 for s in slims_done.values())
                                        or len(slims_done) > 1
                                    )
                                ):
                                    break
                                H._sleep_sim_ms(150)
                            sess_final = getattr(__builtin__, "session", None)
                            post_sys = (
                                getattr(sess_final, "solarsystemid2", None)
                                or getattr(sess_final, "solarsystemid", None)
                            )
                            result["jump_gate_id"] = gate_id
                            result["from_system_id"] = pre_sys
                            result["to_system_id"] = post_sys
                            if post_sys == pre_sys:
                                result["ok"] = False
                                result["error"] = "jump_gate did not change solar system (still %r)" % (pre_sys,)
                    elif sub == "lock_target":
                        tid = int(item_id)
                        tgt_svc = sm.GetService("target")
                        is_fully_locked = False
                        is_targeting = False
                        try:
                            is_fully_locked = tid in (getattr(tgt_svc, "targetsByID", None) or {})
                            is_targeting = tid in (tgt_svc.GetTargeting() or ())
                        except Exception:
                            is_fully_locked = False
                            is_targeting = False
                        try:
                            from eve.client.script.parklife import states as _park_states
                            st_svc = sm.GetService("stateSvc")
                            st_svc.SetState(tid, _park_states.selected, 1)
                            # Only mark activeTarget when already fully locked; setting activeTarget
                            # on an unlocked target pre-fires queued ModuleButtons via OnStateChange
                            # -> dogmaIM.Activate(..., TargetNotLocked) before TryLockTarget completes.
                            if is_fully_locked:
                                st_svc.SetState(tid, _park_states.activeTarget, 1)
                        except Exception:
                            pass
                        already_locked = is_fully_locked or is_targeting
                        if already_locked:
                            try:
                                sm.GetService("godma").GetDogmaLM().AddTarget(tid)
                            except Exception:
                                pass
                            result["note"] = "Target %d already locking/locked (selected as activeTarget)" % (tid,)
                        else:
                            cur_sess = getattr(__builtin__, "session", None)
                            if tid in (
                                getattr(cur_sess, "shipid", None),
                                getattr(cur_sess, "structureid", None),
                            ):
                                result["ok"] = False
                                result["error"] = "Cannot lock own ship/structure %d" % (tid,)
                            else:
                                slim = None
                                has_rec_fn = hasattr(tgt_svc, "GetBallparkRecord")
                                if has_rec_fn:
                                    try:
                                        slim = tgt_svc.GetBallparkRecord(tid)
                                    except Exception:
                                        slim = None
                                if has_rec_fn and slim is None:
                                    result["ok"] = False
                                    result["error"] = "Target %d not in ballpark (missing or destroyed)" % (tid,)
                                else:
                                    non_tgt = ()
                                    try:
                                        import const as _eve_const
                                        non_tgt = getattr(_eve_const, "nonTargetableGroups", ()) or ()
                                    except Exception:
                                        non_tgt = ()
                                    if slim is not None and getattr(slim, "groupID", None) in non_tgt:
                                        result["ok"] = False
                                        result["error"] = "Target %d (groupID=%s) is not targetable" % (
                                            tid,
                                            getattr(slim, "groupID", None),
                                        )
                                    else:
                                        tgt_svc.TryLockTarget(tid)
                    elif sub == "unlock_target":
                        tgt_svc = sm.GetService("target")
                        tgt_svc.UnlockTarget(int(item_id))
            elif atype == "select_character":
                req_cid = int(action.get("character_id", 0) or (raw_target if raw_target.isdigit() else 0) or 0)
                req_name = _to_utf8(action.get("target") or raw_cmd or "").strip().lower()
                cs_layer = getattr(getattr(uicore, "layer", None), "charsel", None) if uicore is not None else None
                cs_info = _inspect_charsel_state(sm)
                if not cs_info.get("active") or cs_layer is None:
                    result["ok"] = False
                    result["error"] = "character selection screen is not active"
                else:
                    matched_cid = req_cid
                    if not matched_cid and req_name:
                        for c_entry in cs_info.get("characters") or []:
                            if req_name in str(c_entry.get("name") or "").lower():
                                matched_cid = int(c_entry.get("character_id") or 0)
                                break
                    if not matched_cid and (cs_info.get("characters") or []):
                        matched_cid = int(cs_info["characters"][0]["character_id"])
                    if not matched_cid:
                        result["ok"] = False
                        result["error"] = "character %r not found on charsel screen (%r)" % (raw_target, cs_info.get("names"))
                    else:
                        slots = list(getattr(cs_layer, "slots", None) or [])
                        target_slot = None
                        for sl in slots:
                            scid = getattr(sl, "charID", None) or getattr(sl, "characterID", None)
                            if scid is None:
                                cd = getattr(sl, "charDetails", None)
                                scid = getattr(cd, "charID", None) or getattr(cd, "characterID", None) if cd is not None else None
                            if scid is not None and int(scid) == int(matched_cid):
                                target_slot = sl
                                break
                        if target_slot is not None and hasattr(target_slot, "OnClick"):
                            target_slot.OnClick()
                            H._sleep_ms(150)
                        entered = False
                        if target_slot is not None and hasattr(target_slot, "OnDblClick"):
                            try:
                                target_slot.OnDblClick()
                                entered = True
                            except Exception:
                                entered = False
                        if not entered:
                            for confirm_attr in ("_CharacterSelection__Confirm", "Confirm", "SelectCharacterID"):
                                cfn = getattr(cs_layer, confirm_attr, None)
                                if callable(cfn):
                                    try:
                                        if confirm_attr == "SelectCharacterID":
                                            cfn(matched_cid)
                                        else:
                                            cfn()
                                        entered = True
                                        break
                                    except TypeError:
                                        try:
                                            cfn()
                                            entered = True
                                            break
                                        except Exception:
                                            pass
                        if not entered:
                            sm.GetService("sessionMgr").PerformSessionChange(
                                "charsel",
                                sm.RemoteSvc("charUnboundMgr").SelectCharacterID,
                                int(matched_cid),
                                None,
                                True,
                            )
                        dl_cs = time.time() + 18.0
                        while time.time() < dl_cs:
                            s_now = getattr(__builtin__, "session", None)
                            if (
                                getattr(s_now, "charid", None) == matched_cid
                                and not getattr(s_now, "mutating", 0)
                                and getattr(s_now, "changing", None) is None
                            ):
                                break
                            H._sleep_ms(150)
                        s_now = getattr(__builtin__, "session", None)
                        if getattr(s_now, "stationid", None) or getattr(s_now, "structureid", None):
                            H._ensure_view(sm, "hangar")
                        elif getattr(s_now, "solarsystemid", None) or getattr(s_now, "solarsystemid2", None):
                            H._ensure_view(sm, "inflight")
                        H._sync_quasar_gateway(sm, int(matched_cid))
                        result["selected_character_id"] = int(matched_cid)
            elif atype == "select_overview":
                sess_ov = getattr(__builtin__, "session", None)
                sys_ov = int(getattr(sess_ov, "solarsystemid", 0) or 0) if sess_ov is not None else 0
                if not sys_ov or getattr(sess_ov, "stationid", None):
                    result["ok"] = False
                    result["error"] = "select_overview requires being undocked in space"
                else:
                    q_low = _to_utf8(action.get("target") or raw_cmd or "").strip().lower()
                    req_iid = int(action.get("item_id", 0) or (q_low if q_low.isdigit() else 0) or 0)
                    mich_ov = sm.GetService("michelle")
                    bp_ov = mich_ov.GetBallpark() if mich_ov is not None else None
                    sid_ov = int(getattr(sess_ov, "shipid", 0) or 0)
                    chosen_id = req_iid
                    chosen_name = ""
                    chosen_node = None
                    # Try OverviewWindow visible entries first
                    reg_ov = getattr(uicore, "registry", None) if uicore is not None else None
                    wins_ov = list(reg_ov.GetWindows() or []) if (reg_ov is not None and hasattr(reg_ov, "GetWindows")) else []
                    best_ov = None
                    for ov_w in wins_ov:
                        if getattr(ov_w, "destroyed", False) or not _is_visible(ov_w):
                            continue
                        if "Overview" not in ov_w.__class__.__name__ and "overview" not in str(getattr(ov_w, "windowID", "") or "").lower():
                            continue
                        sc_ov = getattr(ov_w, "scroll", None)
                        if sc_ov is None or not hasattr(sc_ov, "GetNodes"):
                            continue
                        for nd in list(sc_ov.GetNodes() or []):
                            bid_nd = getattr(nd, "itemID", None)
                            slim_nd = getattr(nd, "slimItem", None)
                            if bid_nd is None and slim_nd is not None:
                                bid_nd = getattr(slim_nd, "itemID", None)
                            if bid_nd is None or int(bid_nd) == sid_ov:
                                continue
                            nm_nd = _strip_tags(_safe_str(getattr(nd, "label", "") or getattr(nd, "name", "") or _resolve_ball_name(slim_nd, bid_nd), 64))
                            nm_nd_lc = nm_nd.lower()
                            gid_nd = int(getattr(slim_nd, "groupID", 0) or 0) if slim_nd is not None else 0
                            ov_tier = None
                            if req_iid and int(bid_nd) == req_iid:
                                ov_tier = 0
                            elif not q_low:
                                ov_tier = 1
                            elif (gid_nd == 10 or nm_nd_lc.startswith("stargate (")) and q_low not in ("stargate", "gate", "celestial"):
                                st_tier = _match_stargate_target_tier(slim_nd, nm_nd, q_low)
                                if st_tier is not None:
                                    ov_tier = st_tier
                            elif nm_nd_lc == q_low:
                                ov_tier = 0
                            elif (
                                (q_low in nm_nd_lc)
                                or (q_low == "planet" and gid_nd == 7)
                                or (q_low == "stargate" and gid_nd == 10)
                                or (q_low == "station" and gid_nd == 15)
                                or (q_low == "celestial" and gid_nd in (6, 7, 8, 10, 15))
                            ):
                                ov_tier = 2
                            if ov_tier is not None and (best_ov is None or ov_tier < best_ov[0]):
                                best_ov = (ov_tier, int(bid_nd), nm_nd, (sc_ov, nd))
                                if ov_tier == 0:
                                    break
                        if best_ov is not None and best_ov[0] == 0:
                            break
                    if best_ov is not None:
                        _, chosen_id, chosen_name, chosen_node = best_ov
                    if not chosen_id and bp_ov is not None:
                        chosen_id, r_err = _resolve_space_command_item_id(bp_ov, sid_ov, "approach", raw_target or "planet")
                        if r_err:
                            result["ok"] = False
                            result["error"] = r_err
                    if chosen_id:
                        if chosen_node is not None:
                            try:
                                sc_obj, nd_obj = chosen_node
                                panel_obj = getattr(nd_obj, "panel", None)
                                if panel_obj is not None and hasattr(panel_obj, "OnClick"):
                                    panel_obj.OnClick()
                                elif hasattr(sc_obj, "SelectNode"):
                                    sc_obj.SelectNode(nd_obj)
                            except Exception:
                                pass
                        try:
                            from eve.client.script.parklife import states as _park_states
                            sm.GetService("stateSvc").SetState(int(chosen_id), _park_states.selected, 1)
                        except Exception:
                            pass
                        result["selected_item_id"] = int(chosen_id)
                        if chosen_name:
                            result["selected_name"] = chosen_name
            elif atype == "save_location":
                bm_name = _to_utf8(action.get("target") or raw_cmd or "Scenario Bookmark").strip()
                cmd_svc_bm = getattr(uicore, "cmd", None) if uicore is not None else None
                bm_svc = sm.GetService("bookmarkSvc") if sm is not None else None
                created_bm = False
                if cmd_svc_bm is not None and hasattr(cmd_svc_bm, "CmdCreateBookmark"):
                    try:
                        cmd_svc_bm.CmdCreateBookmark()
                        H._sleep_ms(300)
                        reg_bm = getattr(uicore, "registry", None)
                        for bw in list(reg_bm.GetWindows() or []) if reg_bm is not None else []:
                            if "Bookmark" in bw.__class__.__name__ and _is_visible(bw):
                                for fld in ("locationName", "nameEdit", "name"):
                                    ed = getattr(bw, fld, None)
                                    if ed is not None and hasattr(ed, "SetValue"):
                                        ed.SetValue(bm_name)
                                        break
                                for cbtn in ("Confirm", "Submit", "OnConfirm"):
                                    fn_bm = getattr(bw, cbtn, None)
                                    if callable(fn_bm):
                                        fn_bm()
                                        created_bm = True
                                        break
                                break
                    except Exception:
                        created_bm = False
                if not created_bm and bm_svc is not None and hasattr(bm_svc, "BookmarkLocation"):
                    sess_bm = getattr(__builtin__, "session", None)
                    loc_id_bm = int(getattr(sess_bm, "shipid", 0) or getattr(sess_bm, "solarsystemid", 0) or 0)
                    bm_svc.BookmarkLocation(loc_id_bm, None, bm_name, "")
                    created_bm = True
                if not created_bm:
                    result["ok"] = False
                    result["error"] = "save_location could not trigger BookmarkLocationWindow or bookmarkSvc"
                else:
                    H._sleep_ms(350)
                    result["bookmark_name"] = bm_name
            elif atype in ("warp_to_location", "warp_to_bookmark"):
                bm_q = _to_utf8(action.get("target") or raw_cmd or "").strip().lower()
                bm_svc = sm.GetService("bookmarkSvc") if sm is not None else None
                found_bm = None
                if bm_svc is not None:
                    bmap = getattr(bm_svc, "bookmarkMap", None)
                    if not isinstance(bmap, dict) and hasattr(bm_svc, "GetBookmarks"):
                        bmap = bm_svc.GetBookmarks()
                    if isinstance(bmap, dict):
                        for bmid, bm_obj in bmap.items():
                            memo = _safe_str(getattr(bm_obj, "memo", None) or getattr(bm_obj, "name", "") or "", 64)
                            if not bm_q or bm_q in memo.lower() or str(bmid) == bm_q:
                                found_bm = (bmid, bm_obj, memo)
                                break
                if found_bm is None:
                    result["ok"] = False
                    result["error"] = "saved location %r not found in bookmarkSvc" % (action.get("target"),)
                else:
                    bmid, bm_obj, memo = found_bm
                    _WARP_TELEMETRY["last_warp_saw_mode3"] = False
                    _WARP_TELEMETRY["last_warp_saw_fx_warping"] = False
                    _WARP_TELEMETRY["last_warp_saw_signal"] = False
                    _WARP_TELEMETRY["last_warp_peak_speed_mps"] = 0.0
                    _WARP_TELEMETRY["last_warp_samples"] = []
                    dist_bm = float(action.get("distance", 0.0) or 0.0)
                    menu_svc = None
                    try:
                        menu_svc = sm.GetService("menu")
                    except Exception:
                        menu_svc = None
                    if menu_svc is not None and hasattr(menu_svc, "WarpToBookmark"):
                        menu_svc.WarpToBookmark(bm_obj, warpRange=dist_bm)
                    else:
                        sess_wb = getattr(__builtin__, "session", None)
                        sys_wb = int(getattr(sess_wb, "solarsystemid", 0) or 0)
                        H._get_beyonce(sm, sys_wb).CmdWarpToStuff("bookmark", int(bmid), dist_bm)
                    for _w_idx in range(8):
                        H._sleep_sim_ms(400)
                        smp = _sample_ego_warp_state(sm)
                        _WARP_TELEMETRY["last_warp_samples"].append(smp)
                        if smp["mode"] == 3:
                            _WARP_TELEMETRY["last_warp_saw_mode3"] = True
                        if "effects.Warping" in smp["fx_activations"] or smp["has_warp_dest_cache"]:
                            _WARP_TELEMETRY["last_warp_saw_fx_warping"] = True
                        if smp["speed_mps"] > _WARP_TELEMETRY["last_warp_peak_speed_mps"]:
                            _WARP_TELEMETRY["last_warp_peak_speed_mps"] = smp["speed_mps"]
                    result["bookmark_id"] = int(bmid) if str(bmid).isdigit() else str(bmid)
                    result["bookmark_name"] = memo
                    result["warp_telemetry"] = {
                        "saw_mode3": _WARP_TELEMETRY["last_warp_saw_mode3"],
                        "saw_fx_warping": _WARP_TELEMETRY["last_warp_saw_fx_warping"],
                        "saw_signal": _WARP_TELEMETRY["last_warp_saw_signal"],
                        "warp_signal_count": 1 if _WARP_TELEMETRY["last_warp_saw_signal"] else 0,
                        "started_count": int(_WARP_TELEMETRY.get("started_count", 0) or 0),
                        "finished_count": int(_WARP_TELEMETRY.get("finished_count", 0) or 0),
                        "in_warp": bool(_WARP_TELEMETRY.get("in_warp", False)),
                        "peak_speed_mps": _WARP_TELEMETRY["last_warp_peak_speed_mps"],
                    }
            elif atype == "set_destination":
                dest_raw = _to_utf8(action.get("target") or raw_cmd or "").strip()
                dest_id = int(action.get("system_id", 0) or (dest_raw if dest_raw.isdigit() else 0) or 0)
                if not dest_id and dest_raw:
                    try:
                        map_svc = sm.GetService("map")
                        if map_svc is not None and hasattr(map_svc, "GetItem"):
                            pass
                        c_cfg = getattr(__builtin__, "cfg", None)
                        locs = getattr(c_cfg, "evelocations", None)
                        if locs is not None and hasattr(locs, "data"):
                            for lid, lrec in locs.data.items():
                                if _to_utf8(getattr(lrec, "locationName", "") or "").lower() == dest_raw.lower():
                                    dest_id = int(lid)
                                    break
                    except Exception:
                        pass
                if not dest_id:
                    result["ok"] = False
                    result["error"] = "set_destination could not resolve solar system %r" % (dest_raw,)
                else:
                    starmap_svc = sm.GetService("starmap")
                    starmap_svc.SetWaypoint(int(dest_id), clearOtherWaypoints=bool(action.get("clear_other", True)))
                    H._sleep_ms(300)
                    path = list(starmap_svc.GetDestinationPath() or []) if hasattr(starmap_svc, "GetDestinationPath") else []
                    result["destination_id"] = int(dest_id)
                    result["route"] = [int(x) for x in path if str(x).isdigit()]
            elif atype in ("engage_autopilot", "toggle_autopilot"):
                ap_svc = sm.GetService("autoPilot")
                want_on = _to_utf8(action.get("target") or "on").strip().lower() not in ("off", "false", "0")
                cur_on = bool(ap_svc.GetState()) if (ap_svc is not None and hasattr(ap_svc, "GetState")) else False
                if cur_on != want_on:
                    cmd_svc_ap = getattr(uicore, "cmd", None) if uicore is not None else None
                    if cmd_svc_ap is not None and hasattr(cmd_svc_ap, "CmdToggleAutopilot"):
                        cmd_svc_ap.CmdToggleAutopilot()
                    elif ap_svc is not None and hasattr(ap_svc, "SetOn") and want_on:
                        ap_svc.SetOn()
                    elif ap_svc is not None and hasattr(ap_svc, "SetOff") and not want_on:
                        ap_svc.SetOff()
                    H._sleep_ms(300)
                result["autopilot_active"] = bool(ap_svc.GetState()) if (ap_svc is not None and hasattr(ap_svc, "GetState")) else want_on
            else:
                result["ok"] = False
                result["error"] = "unsupported action type %r" % (raw_atype,)
        except Exception as exc:
            tb_full = traceback.format_exc()
            result["ok"] = False
            if exc.__class__.__name__ == "UserError":
                result["error"] = "Client action rejected with %s" % (_format_user_error(exc),)
            else:
                result["error"] = "%s: %s" % (exc.__class__.__name__, exc)
            result["traceback"] = tb_full[-1200:]
            if (
                exc.__class__.__name__ == "UserError"
                and atype in ("click_widget", "click_xy", "drag_widget", "press_key", "type_text")
            ):
                _notify_user_error(exc, async_uthread=True)
            elif _es is not None and ("eve/client/script/" in tb_full or "packages/" in tb_full):
                try:
                    _es._log_exc_line("Action tasklet exception (%s):\n%s" % (raw_atype, tb_full))
                except Exception:
                    pass

    def _modal_key(obj, cls_n):
        wid = _safe_str(getattr(obj, "windowID", None) or getattr(obj, "name", None), 64)
        return (id(obj), wid, cls_n)

    def _active_modal_keys():
        seen_modals = set()
        reg = getattr(uicore, "registry", None) if uicore is not None else None
        if reg is not None and hasattr(reg, "GetWindows"):
            try:
                for w in list(reg.GetWindows() or []):
                    if not getattr(w, "destroyed", False) and _is_visible(w):
                        cls_n = w.__class__.__name__
                        if (
                            getattr(w, "isModal", False)
                            or "MessageBox" in cls_n
                            or "Popup" in cls_n
                            or "HybridWindow" in cls_n
                            or "Dialog" in cls_n
                        ):
                            seen_modals.add(_modal_key(w, cls_n))
            except Exception:
                pass
        modal_layer = getattr(layers, "modal", None) if layers is not None else None
        if modal_layer is not None:
            def _scan_modal_node(node, depth):
                if node is None or depth > 3 or getattr(node, "destroyed", False) or not _is_visible(node):
                    return
                cls_n = node.__class__.__name__
                if (
                    depth == 1
                    or getattr(node, "isModal", False)
                    or "MessageBox" in cls_n
                    or "Popup" in cls_n
                    or "HybridWindow" in cls_n
                    or "Dialog" in cls_n
                    or _is_top_level_window_node(node)
                ):
                    seen_modals.add(_modal_key(node, cls_n))
                for sub in list(getattr(node, "children", []) or []):
                    _scan_modal_node(sub, depth + 1)
            _scan_modal_node(modal_layer, 0)
        return seen_modals

    pre_modal_keys = _active_modal_keys()
    done_flag = [False]
    try:
        import uthread

        def _worker():
            try:
                _do_action()
            finally:
                done_flag[0] = True

        uthread.new(_worker)
        max_wait_s = (
            42.0
            if raw_atype == "jump_gate"
            else (
                38.0
                if raw_atype == "dock"
                else (
                    25.0
                    if raw_atype in ("warp_to", "warp_to_location", "warp_to_bookmark", "undock", "select_character")
                    or atype in ("select_character", "warp_to_location", "warp_to_bookmark")
                    else 12.0
                )
            )
        )
        t_start = time.time()
        while not done_flag[0] and (time.time() - t_start) < max_wait_s:
            H._sleep_ms(50)
            # If a modal dialog popped up and blocked the worker tasklet inside MessageBox.Execute(),
            # break after 0.8s so serve_forever_on_stackless never deadlocks or waits 12s!
            if (time.time() - t_start) > 0.8:
                if bool(_active_modal_keys() - pre_modal_keys):
                    result["blocked_by_modal"] = True
                    break
        if not done_flag[0] and not result.get("blocked_by_modal"):
            result["ok"] = False
            result["error"] = "action tasklet timed out after %.1fs" % (time.time() - t_start,)
    except Exception:
        _do_action()

    if atype == "wait":
        if action.get("strict_wait"):
            if effective_wait_ms > 0:
                H._sleep_ms(effective_wait_ms)
            result["waited_ms"] = effective_wait_ms
        else:
            pre_ego = (pre_state.get("space") or {}).get("ego") or {}
            pre_mode = pre_ego.get("mode")
            pre_in_warp = bool(pre_ego.get("signal_in_warp") or _WARP_TELEMETRY.get("in_warp"))
            pre_sys_w = (pre_state.get("session") or {}).get("solarsystemid2")
            pre_sta_w = (pre_state.get("session") or {}).get("stationid")
            pre_str_w = (pre_state.get("session") or {}).get("structureid")
            max_wait_budget = effective_wait_ms if (pre_mode == 3 or pre_in_warp) else min(effective_wait_ms, 3000)
            poll_step = 150 if (pre_mode == 3 or pre_in_warp) else 250
            waited_total = 0
            early_reason = ""
            while waited_total < max_wait_budget:
                chunk_ms = min(poll_step, max_wait_budget - waited_total)
                if chunk_ms <= 0:
                    break
                H._sleep_ms(chunk_ms)
                waited_total += chunk_ms
                if len(_RECENT_MODALS) > pre_modal_cnt or bool(_active_modal_keys() - pre_modal_keys):
                    early_reason = "modal_appeared"
                    break
                if _FLIGHT_SEQ > pre_flight_seq and any(
                    r.get("status") != "OK" for r in _FLIGHT_RECORDER if r.get("seq", 0) > pre_flight_seq
                ):
                    early_reason = "flight_error"
                    break
                try:
                    cur_sess_w = getattr(__builtin__, "session", None)
                    if cur_sess_w is not None:
                        cur_sys_w = getattr(cur_sess_w, "solarsystemid2", None)
                        cur_sta_w = getattr(cur_sess_w, "stationid", None)
                        cur_str_w = getattr(cur_sess_w, "structureid", None)
                        if (
                            (pre_sys_w is not None and cur_sys_w is not None and cur_sys_w != pre_sys_w)
                            or (cur_sta_w != pre_sta_w)
                            or (cur_str_w != pre_str_w)
                        ):
                            early_reason = "session_changed"
                            break
                except Exception:
                    pass
                if pre_mode == 3 or pre_in_warp:
                    try:
                        cur_ego = _sample_ego_warp_state(sm)
                    except Exception:
                        cur_ego = None
                    cur_mode = cur_ego.get("mode") if cur_ego else None
                    cur_in_warp = bool((cur_ego and cur_ego.get("signal_in_warp")) or _WARP_TELEMETRY.get("in_warp"))
                    if cur_mode is not None and cur_mode != 3 and not cur_in_warp:
                        if waited_total < max_wait_budget:
                            H._sleep_ms(150)
                            waited_total += 150
                        early_reason = "warp_finished"
                        break
            result["waited_ms"] = waited_total
            if early_reason:
                result["wait_early_exit"] = early_reason
    elif result.get("blocked_by_modal"):
        H._sleep_ms(25)
    elif explicit_wait_raw is not None and (int(explicit_wait_raw or 0) >= 5000 or action.get("settle_mode") == "fixed"):
        if effective_wait_ms > 0:
            H._sleep_ms(effective_wait_ms)
    else:
        _min_s = int(action.get("min_settle_ms") or 0)
        if atype == "type_text":
            _typed_lbl = (_safe_str(result.get("typed_into", ""), 64) + " " + target).lower()
            if "filter" in _typed_lbl or "search" in _typed_lbl:
                _min_s = max(_min_s, 350)
        elif atype in ("click_widget", "click_xy", "drag_widget"):
            _click_lbl = (_safe_str(result.get("matched_widget", ""), 64) + " " + target).lower()
            if any(k in _click_lbl for k in ("stackall", "splitstack", "reprocess", "board ship")):
                _min_s = max(_min_s, 300)
            if "xmppchatchannels" in _click_lbl or _has_ancestor_named(_clicked_node_ref[0], "xmppchatchannels"):
                # XMPP channel Join/Create runs over TCP outside MachoNet; settle past OnEveMessage for OnChannelJoined.
                _min_s = max(_min_s, 350)
        _max_s = min(max(int(wait_ms or 250), _min_s + 150, 150), 600)
        result["waited_ms"] = _settle_after_action(
            sm,
            pre_flight_seq,
            pre_scatter_seq,
            min_settle_ms=_min_s,
            max_ms=_max_s,
        )

    # Destiny Orbit/Follow/Approach transitions can lag ~0.5-1.5s behind the Macho ACK;
    # poll ego mode/speed briefly so post_state does not report a false-positive mode=2 stall.
    if result.get("ok", True) and (
        (atype == "space_command" and target in ("orbit", "approach", "keep_at_range"))
        or (
            atype in ("click_widget", "click_xy")
            and any(k in target.lower() for k in ("selecteditemorbit", "selecteditemapproach", "selecteditemkeepatrange"))
        )
    ):
        for _ in range(10):
            try:
                _smp = _sample_ego_warp_state(sm)
            except Exception:
                _smp = None
            if not _smp or _smp.get("mode") != 2 or float(_smp.get("speed_mps") or 0.0) >= 0.5:
                break
            H._sleep_ms(100)

    # /spawn and /spawnnpc publish DoBallsAdded/AddBalls2 on the next 1 Hz Destiny tick;
    # poll michelle.GetBallpark().balls briefly so post_state and the next step see the new ball.
    _spawned_id = result.get("spawned_id")
    if (
        result.get("ok", True)
        and atype == "run_slash"
        and _spawned_id
        and str(result.get("slash_cmd") or "").strip().lower().startswith(("/spawn ", "/spawnnpc "))
    ):
        for _ in range(16):
            try:
                _mic = sm.GetService("michelle") if sm is not None else None
                _bp = _mic.GetBallpark() if _mic is not None else None
                _balls = getattr(_bp, "balls", None) if _bp is not None else None
                if _bp is None or _balls is None or _spawned_id in _balls:
                    break
            except Exception:
                break
            H._sleep_ms(100)

    if result.get("ok", True):
        _settle_secondary_view_transition(
            sm,
            pre_state,
            inspect_ui_fn=_inspect_uicore_tree,
            sleep_fn=H._sleep_ms,
        )

    result["action_ok"] = bool(result.get("ok", True))
    result["action_error"] = result.get("error")

    post_state = _get_state_snapshot(sm)
    _LAST_SNAPSHOT = post_state
    _LAST_SNAPSHOT_SM = sm
    _LAST_SNAPSHOT_TS = time.time()
    result["post_state"] = post_state
    new_flights = [e for e in _FLIGHT_RECORDER if e["seq"] > pre_flight_seq]
    new_scatters = [s for s in _RECENT_SCATTERS if s["seq"] > pre_scatter_seq]
    new_modals = list(_RECENT_MODALS[pre_modal_cnt:])
    err_flights = [e for e in _FLIGHT_RECORDER if e["seq"] > since_flight_err_seq]
    _CONSUMED_FLIGHT_ERR_SEQ = _FLIGHT_SEQ
    new_log_excs = []
    suppressed_log_excs = []
    if _es is not None:
        try:
            seen_exc_sigs = getattr(_es, "_REPORTED_LOG_EXC_SIGS", None)
            if not isinstance(seen_exc_sigs, set):
                seen_exc_sigs = set()
                setattr(_es, "_REPORTED_LOG_EXC_SIGS", seen_exc_sigs)
            max_exc_seq = since_exc_seq
            for e in getattr(_es, "_RECENT_EXCEPTIONS", []):
                eseq = int(e.get("seq", 0) or 0)
                if eseq <= since_exc_seq:
                    continue
                if eseq > max_exc_seq:
                    max_exc_seq = eseq
                tr_txt = str(e.get("trace") or "")
                raw_frames = [
                    ln.strip()
                    for ln in tr_txt.splitlines()
                    if 'File "' in ln or re.search(r"\.py[cwj]?\(\d+\)\s+\S+", ln)
                ]
                game_frames = [
                    ln
                    for ln in raw_frames
                    if "evomu_site.py" not in ln
                    and "evomu_playtester_hook.py" not in ln
                    and "logmodule" not in ln
                    and "CallWrapper" not in ln
                    and "MollycoddledUthread" not in ln
                    and "ExceptionWrapperGPCS.py" not in ln
                    and "ExceptionMappingGPCS.py" not in ln
                    and "ObjectCallGPCS.py" not in ln
                    and "machobase.py" not in ln
                    and "moniker.py" not in ln
                ]
                chosen_frames = game_frames or raw_frames
                sig = "|".join(chosen_frames[-2:]) if chosen_frames else str(e.get("headline") or tr_txt)[:120]
                if sig in seen_exc_sigs:
                    suppressed_log_excs.append({
                        "seq": eseq,
                        "ts": e.get("ts"),
                        "headline": str(e.get("headline") or "")[:160],
                        "sig": sig[:160],
                    })
                    continue
                seen_exc_sigs.add(sig)
                new_log_excs.append(e)
            cur_exc_seq = int(getattr(_es, "_EXC_SEQ", 0) or 0)
            setattr(_es, "_PT_CONSUMED_EXC_SEQ", max(max_exc_seq, cur_exc_seq))
        except Exception:
            new_log_excs = []
            suppressed_log_excs = []

    macho_excs = [e for e in err_flights if e.get("status") == "ERR" and e.get("kind") == "macho"]
    quasar_errs = [e for e in err_flights if e.get("status") == "ERR" and e.get("kind") == "quasar"]
    _tgt_id = action.get("item_id") or (raw_target if raw_target.isdigit() else None)
    result["state_diff"] = _compute_state_diff(pre_state, post_state, new_scatters, target_item_id=_tgt_id)
    result["flight_recorder"] = {
        "macho_calls": new_flights[-80:],
        "macho_exceptions": macho_excs,
        "quasar_errors": quasar_errs,
        "log_exceptions": new_log_excs[-10:],
        "suppressed_log_exceptions": suppressed_log_excs[-5:],
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
        try:
            import evomu_site as _es
            if not hasattr(_es, "_PT_CONSUMED_EXC_SEQ"):
                setattr(_es, "_PT_CONSUMED_EXC_SEQ", int(getattr(_es, "_EXC_SEQ", 0) or 0))
        except Exception:
            pass

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
                    conn.sendall(json.dumps(_json_safe(resp)) + "\n")
                rfile.close()
            except Exception as exc:
                try:
                    conn.sendall(json.dumps(_json_safe({"ok": False, "error": _utf8_trunc(repr(exc), 400), "traceback": _utf8_trunc(traceback.format_exc(), 800)})) + "\n")
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
        if cmd == "reload_hook":
            import sys
            mod = sys.modules.get(__name__)
            if mod is not None:
                _preserve_keys = (
                    "_STARTED",
                    "_FLIGHT_RECORDER",
                    "_FLIGHT_SEQ",
                    "_CONSUMED_FLIGHT_ERR_SEQ",
                    "_LAST_OBSERVED_SEQ",
                    "_RECENT_MODALS",
                    "_RECENT_SCATTERS",
                    "_SCATTER_SEQ",
                    "_MACHO_CALL_TASKLET_DEPTH",
                    "_QUASAR_IN_FLIGHT",
                    "_QUASAR_IN_FLIGHT_SEQ",
                    "_WARP_TELEMETRY",
                    "_HOOK_RELOAD_GEN",
                )
                _saved = {k: globals()[k] for k in _preserve_keys if k in globals()}
                reload(mod)
                for k, v in _saved.items():
                    mod.__dict__[k] = v
                    globals()[k] = v
                _next_gen = int(_saved.get("_HOOK_RELOAD_GEN", 0) or 0) + 1
                mod.__dict__["_HOOK_RELOAD_GEN"] = _next_gen
                globals()["_HOOK_RELOAD_GEN"] = _next_gen
                mod.__dict__["_HOOKED_REMOTESVC"] = False
                globals()["_HOOKED_REMOTESVC"] = False
                for k, v in mod.__dict__.items():
                    if not k.startswith("__") and k not in _saved and k not in ("_HOOK_RELOAD_GEN", "_HOOKED_REMOTESVC"):
                        globals()[k] = v
                if hasattr(mod, "PlaytesterRpcServer"):
                    self.__class__ = mod.PlaytesterRpcServer
                _install_flight_recorder_hooks(self.sm)
            return {"ok": True, "reloaded": True, "ts": time.time()}
        if cmd in ("bootstrap_session", "ensure_logged_in"):
            _install_flight_recorder_hooks(self.sm)
            _install_warp_telemetry(self.sm)
            defer_sel = bool(params.get("defer_char_select", self.cfg.get("defer_char_select", False)))
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
                    defer_char_select=defer_sel,
                )
            _ensure_windowed_mode(
                self.sm,
                int(self.cfg.get("window_width") or 0) or None,
                int(self.cfg.get("window_height") or 0) or None,
                self.cfg.get("window_left"),
                self.cfg.get("window_top"),
                self.cfg.get("window_ui_scale"),
            )
            _install_flight_recorder_hooks(self.sm)
            _install_warp_telemetry(self.sm)
            st = _get_state_snapshot(self.sm)
            globals()["_LAST_SNAPSHOT"] = st
            globals()["_LAST_SNAPSHOT_SM"] = self.sm
            globals()["_LAST_SNAPSHOT_TS"] = time.time()
            return {
                "ok": True,
                "result": st,
                "char_id": int(cid or 0),
                "state": st,
                "flight_recorder": {
                    "macho_calls": list(_FLIGHT_RECORDER[-80:]),
                    "scatters_observed": [s.get("event") for s in _RECENT_SCATTERS[-60:]],
                },
            }
        if cmd == "get_flight_recorder":
            since_seq = int(params.get("since_seq", 0) or 0)
            fr_payload = {
                "macho_calls": [e for e in _FLIGHT_RECORDER if int(e.get("seq", 0) or 0) > since_seq][-120:],
                "scatters_observed": [s.get("event") for s in _RECENT_SCATTERS[-60:]],
            }
            return {
                "ok": True,
                "result": fr_payload,
                "macho_calls": fr_payload["macho_calls"],
                "scatters_observed": fr_payload["scatters_observed"],
            }
        if cmd in ("get_state", "observe"):
            st = _get_state_snapshot(self.sm)
            globals()["_LAST_SNAPSHOT"] = st
            globals()["_LAST_SNAPSHOT_SM"] = self.sm
            globals()["_LAST_SNAPSHOT_TS"] = time.time()
            return {"ok": True, "result": st, "state": st}
        if cmd in ("capture_screenshot", "screenshot"):
            cap_res = _capture_trinity_screenshot(params.get("path", ""))
            return {"ok": bool(cap_res.get("ok")), "result": cap_res}
        if cmd in ("cleanup_utility_windows", "cleanup_windows"):
            keep_chat = bool(params.get("keep_chat", True))
            try:
                import uthread as _ut_cln
                done_cln = [False]
                holder_cln = [None]
                err_cln = [None]

                def _cleanup_worker():
                    try:
                        holder_cln[0] = _cleanup_utility_windows(self.sm, keep_chat=keep_chat)
                    except Exception as _cexc:
                        err_cln[0] = repr(_cexc)
                    finally:
                        done_cln[0] = True

                _ut_cln.new(_cleanup_worker)
                t_start = time.time()
                while not done_cln[0] and (time.time() - t_start) < 4.0:
                    H._sleep_ms(50)
                if done_cln[0] and isinstance(holder_cln[0], dict):
                    cln_res = holder_cln[0]
                elif err_cln[0]:
                    cln_res = {"ok": False, "error": err_cln[0], "closed_windows": []}
                else:
                    cln_res = {"ok": False, "error": "cleanup_utility_windows tasklet timed out after 4.0s", "closed_windows": []}
            except Exception:
                cln_res = _cleanup_utility_windows(self.sm, keep_chat=keep_chat)
            return {"ok": bool(cln_res.get("ok")), "result": cln_res, "cleanup_result": cln_res}
        if cmd in ("execute_action", "act"):
            act_payload = dict(params.get("action") or {})
            if self.cfg.get("scenario_mode") and "scenario_mode" not in act_payload:
                act_payload["scenario_mode"] = True
            act_res = _execute_action(self.sm, act_payload)
            return {"ok": True, "result": act_res, "action_result": act_res}
        if cmd == "shutdown":
            self.running = False
            return {"ok": True, "result": {"shutting_down": True}, "shutting_down": True}
        return {"ok": False, "error": "unknown method/cmd %r" % (cmd,)}


def _disable_background_focus_deferral():
    """Keep @postponeUntilFocus UI callbacks (e.g. AurumStoreContainer.SetPlexBalance)
    active even while the EVE.app window is unfocused during background playtesting."""
    try:
        from eve.client.script.ui.util import focusUtil as _fu
        _fu.registeredToFocusEvent = True
        _fu.isFocused = True
        orig_handle = getattr(_fu, "HandleAppFocus", None)
        if orig_handle is not None and not getattr(orig_handle, "_evomu_pt_wrapped", False):
            def _always_focused_handle(wnd, msgID, vkey):
                try:
                    _fu.isFocused = True
                    _fu.CallPostponedFunctions()
                except Exception:
                    pass
                return 1

            _always_focused_handle._evomu_pt_wrapped = True
            _fu.HandleAppFocus = _always_focused_handle
        orig_reg = getattr(_fu, "RegisterToFocusEvent", None)
        if orig_reg is not None and not getattr(orig_reg, "_evomu_pt_wrapped", False):
            def _noop_reg():
                _fu.registeredToFocusEvent = True
                _fu.isFocused = True

            _noop_reg._evomu_pt_wrapped = True
            _fu.RegisterToFocusEvent = _noop_reg
        if getattr(_fu, "postponedFunctionCalls", None):
            _fu.CallPostponedFunctions()
    except Exception:
        pass


def _disable_trinity_throttling(trinity_mod):
    """Prevent Trinity from dropping to ~0.1 FPS when EVE.app is unfocused or off-Space."""
    _disable_background_focus_deferral()
    dev = getattr(trinity_mod, "device", None)
    if dev is None:
        return False, None
    was_throttled = False
    if hasattr(dev, "allowThrottling"):
        try:
            if getattr(dev, "allowThrottling", False):
                dev.allowThrottling = False
                was_throttled = True
        except Exception:
            pass
    return was_throttled, dev


def _capture_trinity_screenshot(path, skip_sleep=False):
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
        was_throttled, dev = _disable_trinity_throttling(trinity)
        if not skip_sleep:
            try:
                # Give the render loop at least 1-2 fresh unthrottled ticks so CarbonUI layout
                # changes (tab switches, newly opened windows, highlights) flush to the backbuffer.
                H._sleep_ms(120 if was_throttled else 50)
            except Exception:
                pass
        throttle_info = {}
        if dev is not None:
            for attr in ("allowThrottling", "throttlingState"):
                if hasattr(dev, attr):
                    try:
                        val = getattr(dev, attr)
                        if isinstance(val, (bool, int, str)):
                            throttle_info[attr] = val
                        elif val is not None:
                            throttle_info[attr] = _safe_str(val, 64)
                    except Exception:
                        pass
        if hasattr(trinity, "SaveRenderTarget"):
            trinity.SaveRenderTarget(p)
            if os.path.exists(p) and os.path.getsize(p) > 0:
                res = {"ok": True, "mode": "trinity_save_render_target", "path": p}
                res.update(throttle_info)
                return res
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
                    res = {"ok": True, "mode": "trinity_host_bitmap", "path": p}
                    res.update(throttle_info)
                    return res
    except Exception as exc:
        return {"ok": False, "error": _safe_str(exc, 240)}
    return {"ok": False, "error": "trinity backbuffer save did not create file"}


_CACHED_WINDOW_CFG = None


def _load_window_cfg():
    global _CACHED_WINDOW_CFG
    if _CACHED_WINDOW_CFG is not None:
        return _CACHED_WINDOW_CFG
    out = {
        "width": 1280,
        "height": 800,
        "left": 40,
        "top": 40,
        "ui_scale": 1.0,
    }
    try:
        cfg_path = os.environ.get("EVOMU_PLAYTESTER_CFG", "") or os.environ.get("EVOMU_INCLIENT_CFG", "")
        if cfg_path and os.path.exists(cfg_path):
            with open(cfg_path, "r") as fh:
                raw = json.load(fh)
            if isinstance(raw, dict):
                if raw.get("window_width"):
                    out["width"] = int(raw["window_width"])
                if raw.get("window_height"):
                    out["height"] = int(raw["window_height"])
                if raw.get("window_left") is not None:
                    out["left"] = int(raw["window_left"])
                if raw.get("window_top") is not None:
                    out["top"] = int(raw["window_top"])
                if raw.get("window_ui_scale") is not None:
                    out["ui_scale"] = float(raw["window_ui_scale"])
    except Exception:
        pass
    _CACHED_WINDOW_CFG = out
    return out


def _ensure_windowed_mode(sm, width=None, height=None, left=None, top=None, ui_scale=None):
    """Keep EVE.app in Windowed mode matching configured physical dimensions and UI scale
    via DeviceMgr.SetWindowState (bypassing ForceSize/_SetDevice's 15s confirmation modal)."""
    try:
        cfg_win = _load_window_cfg()
        target_w = int(width if width is not None else cfg_win.get("width", 1280))
        target_h = int(height if height is not None else cfg_win.get("height", 800))
        target_left = int(left if left is not None else cfg_win.get("left", 40))
        target_top = int(top if top is not None else cfg_win.get("top", 40))
        target_scale = float(ui_scale if ui_scale is not None else cfg_win.get("ui_scale", 1.0))

        import trinity
        _disable_trinity_throttling(trinity)
        mw = getattr(trinity, "mainWindow", None)
        if mw is None or not hasattr(mw, "GetWindowState"):
            return
        win_mode = getattr(getattr(trinity, "Tr2WindowMode", None), "WINDOWED", 1)
        ws = mw.GetWindowState()
        cur_mode = getattr(ws, "windowMode", None)
        cur_w = int(getattr(ws, "width", 0) or 0)
        cur_h = int(getattr(ws, "height", 0) or 0)
        need_resize = not (
            cur_mode == win_mode
            and abs(cur_w - target_w) <= 64
            and abs(cur_h - target_h) <= 64
        )
        dev_svc = sm.GetService("device") if sm is not None and hasattr(sm, "GetService") else None
        if need_resize:
            st = ws.CopyTo() if hasattr(ws, "CopyTo") else trinity.Tr2MainWindowState()
            st.windowMode = win_mode
            st.width = target_w
            st.height = target_h
            st.left = target_left
            st.top = target_top
            if dev_svc is not None and hasattr(dev_svc, "SetWindowState"):
                dev_svc.SetWindowState(st)
            elif hasattr(mw, "SetWindowState"):
                mw.SetWindowState(st)
            H._sleep_ms(300)
        if dev_svc is not None and hasattr(dev_svc, "SetUIScaleValue"):
            try:
                cur_scale = None
                if hasattr(dev_svc, "GetUIScaleValue"):
                    cur_scale = float(dev_svc.GetUIScaleValue() or 0.0)
                if cur_scale is None or abs(cur_scale - target_scale) > 0.01:
                    dev_svc.SetUIScaleValue(target_scale)
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
        evomu_site._install_fsd_typelist_preload_hook(preload_typelist=True)
    except Exception:
        pass
    _install_flight_recorder_hooks(sm)
    _install_warp_telemetry(sm)
    try:
        H._wait_for_session(
            sm,
            cfg.get("user", "demo"),
            cfg.get("password", "demo-secret"),
            int(cfg.get("char_id", 90000001)),
            timeout_sec=120.0,
            defer_char_select=bool(cfg.get("defer_char_select", False)),
        )
    except Exception:
        pass
    _ensure_windowed_mode(
        sm,
        int(cfg.get("window_width") or 0) or None,
        int(cfg.get("window_height") or 0) or None,
        cfg.get("window_left"),
        cfg.get("window_top"),
        cfg.get("window_ui_scale"),
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
