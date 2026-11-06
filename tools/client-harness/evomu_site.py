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
import traceback

sys.dont_write_bytecode = True

_HOOKED_LOGMODULE = False
_HOOKED_XMPP_PREFS = False
_DRIVER_SCHEDULED = False


def _log_exc_line(msg):
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


def _install_logmodule_hook():
    global _HOOKED_LOGMODULE
    lm = sys.modules.get("logmodule")
    log_mod = sys.modules.get("log")
    svc_mod = sys.modules.get("service")
    blue_mod = sys.modules.get("blue")
    if lm is None and log_mod is None:
        return False
    _wrap_log_target(lm, "logmodule")
    _wrap_log_target(log_mod, "log")
    if svc_mod is not None and hasattr(svc_mod, "CoreMixIn"):
        _wrap_log_target(svc_mod.CoreMixIn, "service.CoreMixIn")
    if blue_mod is not None and hasattr(blue_mod, "pyos"):
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
    _propagate_log_wrappers(_ORIG_LOG_FUNCS)

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

    slu = sys.modules.get("stdlogutils")
    if slu is not None and hasattr(slu, "EveExceptionsFormatter"):
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
    sys.meta_path.insert(0, _EvomuMetaHook())
