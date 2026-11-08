# -*- coding: utf-8 -*-
"""Early Python 2.7 startup hook for real V24.01 `exefile` inside EvoMu harness.

Loaded via `sitecustomize.py` before `autoexec` starts. Hooks `logmodule`
exception reporting (recording every exception/traceback without suppressing it),
disables XMPP TLS on localhost (`chatserver_disable_tls`), and schedules
`evomu_inclient_driver.schedule_startup()` on the main Stackless thread as soon
as `__builtin__.sm` and `machoNet` are running.
"""

import __builtin__
import os
import sys
import time
import traceback

sys.dont_write_bytecode = True

_HOOKED_LOGMODULE = False
_HOOKED_TARGETS = set()
_HOOKED_XMPP_PREFS = False
_DRIVER_SCHEDULED = False
_META_HOOK_INSTALLED = False
_RECENT_EXCEPTIONS = []
_EXC_SEQ = 0


def _log_exc_line(msg):
    global _EXC_SEQ
    _EXC_SEQ += 1
    try:
        lines = [ln.strip() for ln in str(msg).splitlines() if ln.strip()]
        headline = lines[-1] if lines else "Client Python exception"
        for ln in reversed(lines):
            if "Error:" in ln or "Exception:" in ln:
                headline = ln
                break
        _RECENT_EXCEPTIONS.append({
            "seq": _EXC_SEQ,
            "ts": round(time.time(), 3),
            "headline": headline[:200],
            "trace": str(msg)[:1200],
        })
        if len(_RECENT_EXCEPTIONS) > 40:
            del _RECENT_EXCEPTIONS[:-30]
    except Exception:
        pass
    cfg_path = os.environ.get("EVOMU_INCLIENT_CFG", "")
    if cfg_path:
        try:
            out_dir = os.path.dirname(cfg_path)
            with open(os.path.join(out_dir, "client_tracebacks.log"), "a") as tf:
                tf.write("%s\n" % (msg,))
        except Exception:
            pass
    try:
        sys.stderr.write("[evomu_site] %s\n" % (msg,))
        sys.stderr.flush()
    except Exception:
        pass


_ORIG_LOG_FUNCS = []


def _propagate_log_wrappers(replacements):
    for mod in list(sys.modules.values()):
        if mod is None:
            continue
        try:
            d = getattr(mod, "__dict__", None)
            if not isinstance(d, dict):
                continue
            for k, v in list(d.items()):
                for orig_fn, wrap_fn in replacements:
                    if v is orig_fn:
                        d[k] = wrap_fn
        except Exception:
            pass


def _wrap_log_target(target, label):
    if target is None:
        return
    for attr in ("LogException", "_LogException", "LogTraceback"):
        orig_fn = getattr(target, attr, None)
        if orig_fn is None or getattr(orig_fn, "_evomu_wrapped", False):
            continue
        def _make_wrapper(orig, attr_name):
            def _wrapped(*args, **kwargs):
                if attr_name == "LogTraceback" and kwargs.get("severity") in (1, 2) and sys.exc_info()[0] is None:
                    return orig(*args, **kwargs)
                tb = traceback.format_exc()
                inner_tb = ""
                try:
                    if args and isinstance(args[0], tuple) and len(args[0]) >= 3 and args[0][2] is not None:
                        inner_tb = "\nINNER TRACEBACK:\n" + "".join(traceback.format_tb(args[0][2]))
                except Exception:
                    pass
                stack = "".join(traceback.format_stack()[:-1])
                _log_exc_line("%s.%s args=%r kwargs=%r%s\n%s\nCALLSTACK:\n%s" % (label, attr_name, args, kwargs, inner_tb, tb, stack))
                return orig(*args, **kwargs)
            _wrapped._evomu_wrapped = True
            return _wrapped
        wrap_fn = _make_wrapper(orig_fn, attr)
        setattr(target, attr, wrap_fn)
        _ORIG_LOG_FUNCS.append((orig_fn, wrap_fn))


def _install_logmodule_hook(propagate=False):
    global _HOOKED_LOGMODULE
    if _HOOKED_LOGMODULE and not propagate:
        return True
    lm = sys.modules.get("logmodule")
    log_mod = sys.modules.get("log")
    svc_mod = sys.modules.get("service")
    blue_mod = sys.modules.get("blue")
    slu = sys.modules.get("stdlogutils")
    if lm is None and log_mod is None:
        return False
    if lm is not None and "logmodule" not in _HOOKED_TARGETS:
        _wrap_log_target(lm, "logmodule")
        _HOOKED_TARGETS.add("logmodule")
    if log_mod is not None and "log" not in _HOOKED_TARGETS:
        _wrap_log_target(log_mod, "log")
        _HOOKED_TARGETS.add("log")
    if svc_mod is not None and hasattr(svc_mod, "CoreMixIn") and "service" not in _HOOKED_TARGETS:
        _wrap_log_target(svc_mod.CoreMixIn, "service.CoreMixIn")
        _HOOKED_TARGETS.add("service")
    if blue_mod is not None and hasattr(blue_mod, "pyos") and "blue" not in _HOOKED_TARGETS:
        orig_eh = getattr(blue_mod.pyos, "exceptionHandler", None)
        if orig_eh is not None and not getattr(orig_eh, "_evomu_wrapped", False):
            def _wrapped_eh(etype, eval_, etb):
                try:
                    _log_exc_line(
                        "blue.pyos.exceptionHandler: %s"
                        % ("".join(traceback.format_exception(etype, eval_, etb)),)
                    )
                except Exception:
                    pass
                return orig_eh(etype, eval_, etb)
            _wrapped_eh._evomu_wrapped = True
            blue_mod.pyos.exceptionHandler = _wrapped_eh
        _HOOKED_TARGETS.add("blue")

    orig_excepthook = getattr(sys, "excepthook", None)
    if orig_excepthook is not None and not getattr(orig_excepthook, "_evomu_hooked", False):
        def _wrapped_excepthook(etype, eval_, etb):
            try:
                _log_exc_line(
                    "sys.excepthook: %s"
                    % ("".join(traceback.format_exception(etype, eval_, etb)),)
                )
            except Exception:
                pass
            return orig_excepthook(etype, eval_, etb)
        _wrapped_excepthook._evomu_hooked = True
        sys.excepthook = _wrapped_excepthook

    if slu is not None and hasattr(slu, "EveExceptionsFormatter") and "stdlogutils" not in _HOOKED_TARGETS:
        fmt_cls = slu.EveExceptionsFormatter
        orig_fmt = getattr(fmt_cls, "format", None)
        if orig_fmt is not None and not getattr(orig_fmt, "_evomu_wrapped", False):
            def _wrapped_fmt(self, record):
                res = orig_fmt(self, record)
                try:
                    if getattr(record, "exc_info", None) and int(getattr(record, "levelno", 40) or 40) >= 40:
                        _log_exc_line(
                            "stdlogutils.EveExceptionsFormatter logger=%s:\n%s"
                            % (getattr(record, "name", "?"), res)
                        )
                except Exception:
                    pass
                return res
            _wrapped_fmt._evomu_wrapped = True
            fmt_cls.format = _wrapped_fmt
        _HOOKED_TARGETS.add("stdlogutils")

    if propagate:
        _propagate_log_wrappers(_ORIG_LOG_FUNCS)

    if len(_HOOKED_TARGETS) >= 5:
        _HOOKED_LOGMODULE = True
    return True


def _install_xmpp_plaintext_guard():
    global _HOOKED_XMPP_PREFS
    if _HOOKED_XMPP_PREFS:
        return
    ep = sys.modules.get("eveprefs")
    p = getattr(ep, "prefs", None) if ep is not None else getattr(__builtin__, "prefs", None)
    if p is not None and hasattr(p, "SetValue"):
        try:
            p.SetValue("chatserver_disable_tls", 1)
            _HOOKED_XMPP_PREFS = True
        except Exception:
            pass


def _maybe_schedule_driver_on_main_thread():
    global _DRIVER_SCHEDULED
    if _DRIVER_SCHEDULED:
        return
    if "uthread" not in sys.modules:
        return
    sm = getattr(__builtin__, "sm", None)
    if sm is None or not hasattr(sm, "IsServiceRunning"):
        return
    try:
        if not sm.IsServiceRunning("machoNet"):
            return
    except Exception:
        return
    _DRIVER_SCHEDULED = True
    try:
        if os.environ.get("EVOMU_PLAYTESTER_PORT") or os.environ.get("EVOMU_PLAYTESTER_CFG"):
            import evomu_playtester_hook
            evomu_playtester_hook.schedule_startup()
        else:
            import evomu_inclient_driver
            evomu_inclient_driver.schedule_startup()
    except Exception:
        _log_exc_line("schedule_startup failed:\n%s" % traceback.format_exc())


class _EvomuMetaHook(object):
    def find_module(self, fullname, path=None):
        _install_logmodule_hook()
        _install_xmpp_plaintext_guard()
        _maybe_schedule_driver_on_main_thread()
        return None


def install():
    global _META_HOOK_INSTALLED
    if _META_HOOK_INSTALLED:
        return
    _META_HOOK_INSTALLED = True
    sys.meta_path.insert(0, _EvomuMetaHook())
