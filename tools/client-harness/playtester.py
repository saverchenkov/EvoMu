#!/usr/bin/env python3
"""
EvoMu Autonomous Live EVE.app Playtester & Continuous Regression Daemon.

Inspired by Anthropic's Project EVE (Claude Plays MMORPGs), adapted for EvoMu's
macOS V24.01 EVE.app (exefile) client + Rust eve-server:

1. Isolated Playtester Worktree & Staged Client:
   - Runs in `../EvoMu-playtester` (or repo root containing this script)
   - Stages EVE.app into `~/.cache/evomu/client_build_<build>_playtester` (derived dynamically
     from `start.ini`) so it never collides with `run_journeys` hourly regressions.
   - Injects `evomu_site.py` (with `_DRIVER_SCHEDULED = True` and `_install_logmodule_hook()`)
     plus `evomu_playtester_hook.py` (Stackless Python 2.7 localhost JSON-RPC bridge).

2. Multi-Modal Perception:
   - Live window screenshot (`screencapture -l <CGWindowID> -x`) + multi-frame burst
     screenshots during animations (`warp_to`, `undock`, `dock`).
   - Live `uicore` scene-graph inspection (Neocom buttons, windows, buttons, tabs,
     context menus, modals, Overview entries, Ship HUD, skill queue, PLEX balance).
   - Real-time MachoNet + Quasar + `logmodule.LogException` flight-recorder ring buffer.

3. Open-Ended `agy` Reasoning Loop with Configurable Model Fallback Chain:
   - Default model fallback chain:
     `hydrogen-l-tf-high` -> `gemini-flash-next` -> `gemini-4-carbon-medium` -> `opus-5.5-medium`
   - Configurable via `--model`, `--model-chain`, or `EVOMU_PLAYTESTER_MODEL`.
   - Enforces structured JSON output via `playtester_action_schema.json`.
   - Maintains persistent `memory.json` (working memory, long-term notes, coverage frontier,
     rotating persona objectives, and verified/buggy feature ledger).

4. Continuous Server Hot-Reload & Persistent World DB:
   - Preserves `harness_world.sqlite` across server restarts and hot-reloads so character
     progression, skills, fitting, wallet, and inventory persist across hours of play.
   - Creates a snapshot backup (`harness_world.sqlite.pre_reload.bak`) before each reload,
     and only resets the DB if `--reset-db` is passed or if a schema migration makes the
     old DB incompatible with the new `eve-server` binary.
   - Watches `origin/main` (every `--watch-origin-interval` seconds) and accepts instant
     external reload pings via:
       * `tools/client-harness/playtester.py --ping-reload`
       * `kill -USR1 $(cat /tmp/evomu_playtester.pid)`
       * `echo RELOAD > /tmp/evomu_playtester.control`
   - Uses `-j 4` + `sccache` for `eve-server` builds (playtester is exempt from coder cargo slot caps per `AGENTS.md`).

5. Zero-Tolerance Crash & Visual/Protocol Oracle:
   - Automatically detects Python tracebacks (`client_tracebacks.log`), modal error dialogs,
     Quasar/MachoNet timeouts, missing warp tunnel animations (`ball.mode == 3` / `effects.Warping`),
     `eve-server` ERROR/PANIC logs, and `agy`-diagnosed visual/functional bugs.
   - Emits deduplicated incident bundles to `.evomu-bugs/queue/` and syncs to GitHub via
     `tools/sync_bugs_to_github.py`.
"""

from __future__ import annotations

import argparse
import ast
import atexit
import base64
import hashlib
import hmac
import json
import math
import os
import queue
import re
import select
import shutil
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple


HARNESS_DIR = Path(__file__).resolve().parent
REPO_ROOT = HARNESS_DIR.parent.parent
DEFAULT_PLAYTESTER_WORKTREE = (
    Path(os.environ["EVOMU_PLAYTESTER_WORKTREE"])
    if os.environ.get("EVOMU_PLAYTESTER_WORKTREE")
    else (REPO_ROOT.parent / "EvoMu-playtester")
)

ACTION_SCHEMA_PATH = HARNESS_DIR / "playtester_action_schema.json"
PLANNER_SCHEMA_PATH = HARNESS_DIR / "playtester_planner_schema.json"
EXECUTOR_SCHEMA_PATH = HARNESS_DIR / "playtester_executor_schema.json"
PERSONAS_PATH = HARNESS_DIR / "playtester_personas.json"
HOOK_SCRIPT_PATH = HARNESS_DIR / "evomu_playtester_hook.py"
SITE_SCRIPT_PATH = HARNESS_DIR / "evomu_site.py"
HELPERS_SCRIPT_PATH = HARNESS_DIR / "evomu_inclient_helpers.py"
SCENARIOS_DIR = HARNESS_DIR / "scenarios"
TOOLS_DIR = HARNESS_DIR.parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))
import guard_registry  # noqa: E402
import gm_slash_utils  # noqa: E402
import sync_bugs_to_github  # noqa: E402
import sync_bugs_to_github  # noqa: E402


class _GuardHitTracker:
    """Deduplicating accumulator for client behaviour-guard hits across RPC payloads."""

    def __init__(self, retain_all: bool = True, max_recent: int = 100) -> None:
        self.retain_all = bool(retain_all)
        self.max_recent = int(max_recent)
        self.session_gen: int = 0
        self.seen_seqs: set = set()
        self.last_seq: int = 0
        self.total_count: int = 0
        self.by_guard: Dict[str, int] = {}
        self.client_only_count: int = 0
        self.client_only_by_guard: Dict[str, int] = {}
        self.client_guards_off: Optional[bool] = None
        self.hits: List[Dict[str, Any]] = []
        self.recent_hits: List[Dict[str, Any]] = []

    def reset_client_session(self) -> None:
        """Reset client-session sequence tracking across EVE.app restarts/hot-reloads."""
        self.session_gen += 1
        self.last_seq = 0
        self.seen_seqs.clear()

    def absorb_hits(self, raw_hits: Any) -> List[Dict[str, Any]]:
        if not isinstance(raw_hits, list):
            return []
        added: List[Dict[str, Any]] = []
        for item in raw_hits:
            if not isinstance(item, dict):
                continue
            seq_val = item.get("seq")
            if isinstance(seq_val, int):
                dedup_key: Any = (self.session_gen, "seq", seq_val)
                if seq_val > self.last_seq:
                    self.last_seq = seq_val
            else:
                dedup_key = (
                    self.session_gen,
                    "raw",
                    str(item.get("guard") or ""),
                    str(item.get("function") or ""),
                    str(item.get("reason") or ""),
                    item.get("time"),
                )
            if dedup_key in self.seen_seqs:
                continue
            self.seen_seqs.add(dedup_key)
            if len(self.seen_seqs) > 10000:
                self.seen_seqs = {
                    k
                    for k in self.seen_seqs
                    if isinstance(k, tuple)
                    and len(k) >= 3
                    and k[0] == self.session_gen
                    and k[1] == "seq"
                    and k[2] > self.last_seq - 2000
                }
            gid = str(item.get("guard") or "unknown")
            self.total_count += 1
            self.by_guard[gid] = self.by_guard.get(gid, 0) + 1
            if guard_registry.is_approved_client_only(gid):
                self.client_only_count += 1
                self.client_only_by_guard[gid] = self.client_only_by_guard.get(gid, 0) + 1
            entry = dict(item)
            added.append(entry)
            if self.retain_all:
                self.hits.append(entry)
            self.recent_hits.append(entry)
            if len(self.recent_hits) > self.max_recent:
                self.recent_hits = self.recent_hits[-self.max_recent:]
        return added

    def absorb_payload(self, payload: Any) -> List[Dict[str, Any]]:
        if not isinstance(payload, dict):
            return []
        fr = payload.get("flight_recorder")
        if isinstance(fr, dict):
            fr_sum = fr.get("guard_summary")
            if isinstance(fr_sum, dict) and isinstance(fr_sum.get("client_guards_off"), bool):
                self.client_guards_off = fr_sum["client_guards_off"]
        top_sum = payload.get("guard_summary")
        if isinstance(top_sum, dict) and isinstance(top_sum.get("client_guards_off"), bool):
            self.client_guards_off = top_sum["client_guards_off"]
        if isinstance(payload.get("guard_hits"), list):
            return self.absorb_hits(payload.get("guard_hits"))
        if isinstance(fr, dict) and isinstance(fr.get("guard_hits"), list):
            return self.absorb_hits(fr.get("guard_hits"))
        return []

    def poll_rpc(self, rpc_obj: Any, timeout: float = 2.0) -> List[Dict[str, Any]]:
        if rpc_obj is None or not callable(getattr(rpc_obj, "call", None)):
            return []
        try:
            resp = rpc_obj.call("get_guard_hits", {"since_seq": self.last_seq}, timeout=timeout)
        except Exception:
            return []
        if not isinstance(resp, dict):
            return []
        if isinstance(resp.get("result"), dict):
            resp = resp["result"]
        if not isinstance(resp.get("guard_hits"), list) and not isinstance(resp.get("guard_summary"), dict):
            return []
        return self.absorb_payload(resp)

    def summary(self, default_guards_off: bool = False) -> Dict[str, Any]:
        guards_off = default_guards_off if self.client_guards_off is None else bool(self.client_guards_off)
        ordered_by_guard = {gid: self.by_guard[gid] for gid in sorted(self.by_guard.keys())}
        ordered_co_by_guard = {
            gid: self.client_only_by_guard[gid]
            for gid in sorted(self.client_only_by_guard.keys())
        }
        return {
            "client_guards_off": guards_off,
            "guard_hits_count": self.total_count,
            "guard_hits_by_guard": ordered_by_guard,
            "client_only_guard_hits": self.client_only_count,
            "client_only_guard_hits_by_guard": ordered_co_by_guard,
            "server_caused_guard_hits": self.total_count - self.client_only_count,
        }


class _SimTimeTracker:
    """Deduplicating accumulator for host-side GM simulated-time injections."""

    def __init__(self, retain_events: bool = True, max_recent: int = 200) -> None:
        self.retain_events = bool(retain_events)
        self.max_recent = int(max_recent)
        self.seen_ids: set = set()
        self._seen_order: deque = deque()
        self.total_sec: float = 0.0
        self.total_count: int = 0
        self.events: List[Dict[str, Any]] = []
        self.recent_events: List[Dict[str, Any]] = []

    def absorb_events(self, raw_events: Any) -> List[Dict[str, Any]]:
        if not isinstance(raw_events, list):
            return []
        added: List[Dict[str, Any]] = []
        for idx, item in enumerate(raw_events):
            if not isinstance(item, dict):
                continue
            eid = item.get("injection_id")
            raw_seq = item.get("seq")
            if isinstance(eid, str) and eid.strip():
                key: Any = ("id", eid.strip())
            elif isinstance(raw_seq, int) and not isinstance(raw_seq, bool):
                key = (
                    "seq",
                    int(raw_seq),
                    str(item.get("method") or ""),
                    float(item.get("seconds") or 0.0),
                )
            else:
                key = (
                    "raw",
                    str(item.get("source") or ""),
                    str(item.get("method") or ""),
                    float(item.get("seconds") or 0.0),
                    int(item.get("target_char_id") or 0),
                    item.get("ts"),
                    idx,
                )
            if key in self.seen_ids:
                continue
            self.seen_ids.add(key)
            self._seen_order.append(key)
            while len(self.seen_ids) > 20000 and self._seen_order:
                oldest = self._seen_order.popleft()
                self.seen_ids.discard(oldest)
            try:
                sec = max(0.0, float(item.get("seconds") or 0.0))
            except Exception:
                sec = 0.0
            self.total_sec = round(self.total_sec + sec, 6)
            self.total_count += 1
            entry = dict(item)
            entry["seconds"] = sec
            added.append(entry)
            if self.retain_events:
                self.events.append(entry)
            self.recent_events.append(entry)
            if len(self.recent_events) > self.max_recent:
                self.recent_events = self.recent_events[-self.max_recent:]
        return added

    def absorb_payload(self, payload: Any) -> List[Dict[str, Any]]:
        if not isinstance(payload, dict):
            return []
        fr = payload.get("flight_recorder")
        if isinstance(payload.get("sim_time_injections"), list):
            return self.absorb_events(payload.get("sim_time_injections"))
        if isinstance(fr, dict) and isinstance(fr.get("sim_time_injections"), list):
            return self.absorb_events(fr.get("sim_time_injections"))
        return []

    def summary(self) -> Dict[str, Any]:
        return {
            "total_sim_time_injected_sec": round(self.total_sec, 6),
            "total_sim_time_injection_count": int(self.total_count),
        }


def _select_live_skill_state(
    primary: Optional[Dict[str, Any]],
    fallback: Optional[Dict[str, Any]] = None,
) -> Optional[Dict[str, Any]]:
    """Prefer the state snapshot carrying non-empty `skills_state.trained_skill_levels`."""
    for cand in (primary, fallback):
        if not isinstance(cand, dict):
            continue
        sk = cand.get("skills_state")
        if isinstance(sk, dict) and isinstance(sk.get("trained_skill_levels"), dict) and sk.get("trained_skill_levels"):
            return cand
        if isinstance(cand.get("trained_skill_levels"), dict) and cand.get("trained_skill_levels"):
            return cand
    if isinstance(primary, dict) and primary:
        return primary
    return fallback if isinstance(fallback, dict) else None


def summarize_user_error_exemptions(
    hits: Optional[List[Dict[str, Any]]] = None,
    *,
    tracker: Optional["_UserErrorExemptionTracker"] = None,
) -> Dict[str, Any]:
    """Aggregate exemption hit counts by UserError key, mechanism, and precondition verification."""
    if tracker is not None and hits is None:
        by_key = dict(tracker.by_key)
        by_mech = dict(tracker.by_mechanism)
        total = int(tracker.total_count)
        verified_total = int(tracker.verified_count)
    else:
        by_key = {}
        by_mech = {}
        total = 0
        verified_total = 0
        for item in hits or ():
            if not isinstance(item, dict):
                continue
            key_name = str(item.get("key") or "unknown").strip() or "unknown"
            mech = str(item.get("mechanism") or "unknown").strip() or "unknown"
            total += 1
            if item.get("verified"):
                verified_total += 1
            by_key[key_name] = by_key.get(key_name, 0) + 1
            by_mech[mech] = by_mech.get(mech, 0) + 1
    return {
        "user_error_exemptions_count": total,
        "user_error_exemptions_by_key": {k: by_key[k] for k in sorted(by_key.keys())},
        "user_error_exemptions_by_mechanism": {m: by_mech[m] for m in sorted(by_mech.keys())},
        "precondition_verified_user_error_exemptions": verified_total,
        "unverified_user_error_exemptions": total - verified_total,
    }


class _UserErrorExemptionTracker:
    """Deduplicating accumulator for expected UserError and tasklet-traceback exemptions (#1539)."""

    def __init__(self, retain_all: bool = True, max_recent: int = 100) -> None:
        self.retain_all = bool(retain_all)
        self.max_recent = int(max_recent)
        self.session_gen: int = 0
        self.seen_ids: set = set()
        self._seen_order: deque = deque()
        self.total_count: int = 0
        self.verified_count: int = 0
        self.by_key: Dict[str, int] = {}
        self.by_mechanism: Dict[str, int] = {}
        self.hits: List[Dict[str, Any]] = []
        self.recent_hits: List[Dict[str, Any]] = []

    def reset_client_session(self) -> None:
        """Advance session generation across client restarts so RPC `seq` IDs do not collide."""
        self.session_gen += 1

    def absorb_hits(
        self, raw_hits: Any, *, step_num: Optional[int] = None
    ) -> List[Dict[str, Any]]:
        if not isinstance(raw_hits, list):
            return []
        added: List[Dict[str, Any]] = []
        for idx, item in enumerate(raw_hits):
            if not isinstance(item, dict):
                continue
            key_name = str(item.get("key") or "unknown").strip() or "unknown"
            mechanism = str(item.get("mechanism") or "unknown").strip() or "unknown"
            channel = str(item.get("channel") or "unknown").strip() or "unknown"
            raw_step = item.get("step")
            if isinstance(raw_step, int) and not isinstance(raw_step, bool) and raw_step > 0:
                step_val = int(raw_step)
            elif step_num is not None:
                step_val = int(step_num)
            else:
                step_val = 0
            detail = str(item.get("detail") or item.get("reason") or "")[:240]
            dedup_id = item.get("dedup_id")
            seq_val = item.get("seq")
            if isinstance(dedup_id, str) and dedup_id.strip():
                dedup_key: Any = ("id", dedup_id.strip())
            elif isinstance(seq_val, int) and not isinstance(seq_val, bool):
                dedup_key = ("seq", self.session_gen, int(seq_val), key_name.lower())
            elif step_val > 0:
                occ = item.get("occ_idx")
                dedup_key = (
                    "step",
                    step_val,
                    channel,
                    key_name.lower(),
                    mechanism,
                    detail,
                    idx if occ is None else occ,
                )
            else:
                dedup_key = (
                    "raw",
                    channel,
                    key_name.lower(),
                    mechanism,
                    detail,
                    item.get("ts"),
                    idx,
                )
            if dedup_key in self.seen_ids:
                continue
            self.seen_ids.add(dedup_key)
            self._seen_order.append(dedup_key)
            while len(self.seen_ids) > 20000 and self._seen_order:
                oldest = self._seen_order.popleft()
                self.seen_ids.discard(oldest)

            self.total_count += 1
            verified = bool(item.get("verified", False))
            if verified:
                self.verified_count += 1
            self.by_key[key_name] = self.by_key.get(key_name, 0) + 1
            self.by_mechanism[mechanism] = self.by_mechanism.get(mechanism, 0) + 1

            entry = dict(item)
            entry["key"] = key_name
            entry["mechanism"] = mechanism
            entry["channel"] = channel
            entry["step"] = step_val
            entry["verified"] = verified
            if detail and "detail" not in entry:
                entry["detail"] = detail
            added.append(entry)
            if self.retain_all:
                self.hits.append(entry)
            self.recent_hits.append(entry)
            if len(self.recent_hits) > self.max_recent:
                self.recent_hits = self.recent_hits[-self.max_recent:]
        return added

    def absorb_payload(
        self, payload: Any, *, step_num: Optional[int] = None
    ) -> List[Dict[str, Any]]:
        if not isinstance(payload, dict):
            return []
        fr = payload.get("flight_recorder")
        if isinstance(payload.get("user_error_exemptions"), list):
            return self.absorb_hits(payload.get("user_error_exemptions"), step_num=step_num)
        if isinstance(fr, dict) and isinstance(fr.get("user_error_exemptions"), list):
            return self.absorb_hits(fr.get("user_error_exemptions"), step_num=step_num)
        return []

    def summary(self) -> Dict[str, Any]:
        return summarize_user_error_exemptions(
            self.hits if self.retain_all else None, tracker=None if self.retain_all else self
        )


class _HostGmBridgeRpcServer:
    """Loopback JSON-line TCP server routing in-client time-control requests to the host GM bridge."""

    def __init__(self, handler_fn: Any) -> None:
        self._handler_fn = handler_fn
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(16)
        self.port = int(self._sock.getsockname()[1])
        self._stop_evt = threading.Event()
        self._thread = threading.Thread(
            target=self._serve_loop,
            name=f"HostGmBridge-{self.port}",
            daemon=True,
        )
        self._thread.start()

    def _serve_loop(self) -> None:
        self._sock.settimeout(0.5)
        while not self._stop_evt.is_set():
            try:
                conn, _ = self._sock.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            t = threading.Thread(
                target=self._handle_conn,
                args=(conn,),
                daemon=True,
            )
            t.start()

    def _handle_conn(self, conn: socket.socket) -> None:
        try:
            with conn:
                conn.settimeout(30.0)
                buf = b""
                while not self._stop_evt.is_set():
                    try:
                        chunk = conn.recv(4096)
                    except socket.timeout:
                        continue
                    except OSError:
                        break
                    if not chunk:
                        break
                    buf += chunk
                    while b"\n" in buf:
                        line, buf = buf.split(b"\n", 1)
                        raw = line.decode("utf-8", errors="replace").strip()
                        if not raw:
                            continue
                        try:
                            req = json.loads(raw)
                        except Exception as exc:
                            resp: Dict[str, Any] = {"ok": False, "error": f"bad_json: {exc}"}
                        else:
                            try:
                                resp = self._handler_fn(req)
                                if not isinstance(resp, dict):
                                    resp = {"ok": False, "error": "invalid_handler_response"}
                            except Exception as exc:
                                resp = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
                        conn.sendall((json.dumps(resp) + "\n").encode("utf-8"))
        except Exception:
            pass

    def close(self) -> None:
        self._stop_evt.set()
        try:
            self._sock.close()
        except OSError:
            pass
        if self._thread.is_alive():
            self._thread.join(timeout=1.0)


CLIENT_BRIDGE_DIR = REPO_ROOT / "client-bridge"

DEFAULT_CACHE_ROOT = Path(
    os.environ.get("EVOMU_CLIENT_CACHE", os.path.expanduser("~/.cache/evomu"))
)
SYSTEM_EVE_BUILD_DIR = Path(
    os.path.expanduser(
        "~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build"
    )
)


def _find_cached_client_build(
    cache_root: Optional[Path] = None,
    prefer_build: Optional[str] = None,
) -> Optional[Path]:
    """Find a cached `client_build_<id>` directory with `bin64/exefile`."""
    croot = cache_root if cache_root is not None else DEFAULT_CACHE_ROOT
    if prefer_build and str(prefer_build).isdigit():
        exact = croot / f"client_build_{prefer_build}"
        if (exact / "bin64" / "exefile").exists():
            return exact
    if croot.exists():
        candidates = []
        for entry in croot.iterdir():
            m = re.match(r"^client_build_(\d+)$", entry.name)
            if m and (entry / "bin64" / "exefile").exists():
                candidates.append((int(m.group(1)), entry))
        if candidates:
            candidates.sort(key=lambda item: item[0], reverse=True)
            return candidates[0][1]
    return None


def _detect_system_eve_build(
    build_dir: Path = SYSTEM_EVE_BUILD_DIR,
    cache_root: Optional[Path] = None,
) -> str:
    """Read `build = <id>` from `start.ini`, falling back to cached build dirs or `current`."""
    start_ini = build_dir / "start.ini"
    try:
        for line in start_ini.read_text(encoding="utf-8", errors="ignore").splitlines():
            m = re.match(r"^\s*build\s*=\s*(\d+)\s*$", line)
            if m:
                return m.group(1)
    except Exception:
        pass
    cached = _find_cached_client_build(cache_root=cache_root)
    if cached is not None:
        m = re.match(r"^client_build_(\d+)$", cached.name)
        if m:
            return m.group(1)
    return "current"


def resolve_default_staged_app(
    build_dir: Optional[Path] = None,
    cache_root: Optional[Path] = None,
) -> Path:
    """Compute the isolated staged playtester dir for the active EVE build."""
    bdir = build_dir if build_dir is not None else SYSTEM_EVE_BUILD_DIR
    croot = cache_root if cache_root is not None else DEFAULT_CACHE_ROOT
    return croot / f"client_build_{_detect_system_eve_build(bdir, cache_root=croot)}_playtester"


SOURCE_STAGED_APP = _find_cached_client_build(DEFAULT_CACHE_ROOT)
DEFAULT_STAGED_APP = resolve_default_staged_app()
WINDOW_INFO_HELPER_BIN = DEFAULT_CACHE_ROOT / "evomu_window_info_v2"

DEFAULT_PID_FILE = Path("/tmp/evomu_playtester.pid")
DEFAULT_CONTROL_FILE = Path("/tmp/evomu_playtester.control")

DEFAULT_MODEL_CHAIN = [
    "hydrogen-l-tf-high",
    "gemini-4-carbon-high",
    "gemini-launch-candidate-high",
    "gemini-pro-next",
    "gemini-flash-next",
]

DEFAULT_EXECUTOR_MODEL_CHAIN = [
    "hydrogen-l-tf-medium",
    "gemini-4-carbon-low",
    "gemini-launch-candidate-low",
    "gemini-flash-next",
]

DEFAULT_ROUTINE_PLANNER_MODEL_CHAIN = list(DEFAULT_EXECUTOR_MODEL_CHAIN)

DEFAULT_Ports = {
    "macho": 26010,
    "http": 26011,
    "gateway": 26012,
    "bridge": 26013,
    "xmpp": 25232,
    "rpc": 26099,
}

HARNESS_JWT_SECRET = "evomu-playtester-jwt-secret-key-2026"


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


DEFAULT_DOWNTIME_UTC_SECS_OF_DAY = 39600  # 11:00:00 UTC (matches ServerConfig default)
DEFAULT_DOWNTIME_WINDOW_SECS = 900       # 15m window (11:00:00 - 11:15:00 UTC)
DEFAULT_DOWNTIME_RESUME_BUFFER_SECS = 5.0

_DOWNTIME_EXPLICIT_MARKERS = (
    "clustershuttingdown",
    "daily cluster downtime",
    "cluster shutdown in progress",
    "reason=clustershutdown",
    "\"reason\": \"clustershutdown\"",
)

_DOWNTIME_DISCONNECT_MARKERS = (
    "gpstransportclosed",
    "performsessioncheck",
    "connection reset",
    "connection refused",
    "connection closed",
    "transport closed",
    "rpc bridge unreachable",
    "bridge connection failed",
    "not connected",
    "timed out",
    "broken pipe",
    "remotingerror",
    "connectionlost",
    "connection lost",
    "socket was closed",
    "sessiondisconnected",
    "caller not in solsys nor station",
)

_DOWNTIME_MODAL_KEYS = ("modals", "modal", "active_modal", "recent_modal_messages")


def _utc_secs_of_day(now_unix: Optional[float] = None) -> float:
    ts = float(time.time() if now_unix is None else now_unix)
    return ts % 86400.0


def _is_cluster_downtime_window(
    now_unix: Optional[float] = None,
    downtime_utc_secs: int = DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
    window_secs: int = DEFAULT_DOWNTIME_WINDOW_SECS,
) -> bool:
    if window_secs <= 0:
        return False
    start = int(downtime_utc_secs) % 86400
    end = start + int(window_secs)
    sod = _utc_secs_of_day(now_unix)
    if end <= 86400:
        return start <= sod < end
    return sod >= start or sod < (end % 86400)


def _seconds_until_downtime_ends(
    now_unix: Optional[float] = None,
    downtime_utc_secs: int = DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
    window_secs: int = DEFAULT_DOWNTIME_WINDOW_SECS,
    buffer_secs: float = DEFAULT_DOWNTIME_RESUME_BUFFER_SECS,
    pre_window_secs: int = 0,
) -> float:
    buf = max(0.0, float(buffer_secs))
    start = int(downtime_utc_secs) % 86400
    sod = _utc_secs_of_day(now_unix)
    if _is_cluster_downtime_window(
        now_unix=now_unix,
        downtime_utc_secs=downtime_utc_secs,
        window_secs=window_secs,
    ):
        end = (start + int(window_secs)) % 86400
        rem = end - sod
        if rem <= 0.0:
            rem += 86400.0
        return max(0.0, rem + buf)
    if pre_window_secs > 0 and window_secs > 0:
        until_start = (start - sod) % 86400.0
        if until_start <= float(pre_window_secs):
            return max(0.0, until_start + float(window_secs) + buf)
    return 0.0


def _read_world_downtime_state(world_db_path: Any) -> tuple[int, Optional[int]]:
    """Read (clock_offset_secs, downtime_until_unix) read-only from harness_world.sqlite."""
    if not world_db_path:
        return (0, None)
    db_path = Path(world_db_path)
    if not db_path.exists():
        return (0, None)
    conn = None
    clock_offset_secs = 0
    downtime_until_unix: Optional[int] = None
    try:
        conn = sqlite3.connect(db_path.resolve().as_uri() + "?mode=ro", uri=True, timeout=1.0)
        try:
            row = conn.execute(
                "SELECT meta_value FROM world_meta WHERE meta_key = 'clock_offset_secs'"
            ).fetchone()
            if row and row[0] is not None:
                clock_offset_secs = int(row[0])
        except Exception:
            pass
        try:
            drow = conn.execute(
                "SELECT value FROM domain_rows WHERE domain = 'service_domains.cluster_and_runtime' AND key = 'cluster_downtime_state'"
            ).fetchone()
            if drow and drow[0]:
                payload = json.loads(str(drow[0]))
                if isinstance(payload, dict) and payload.get("downtime_until_unix") is not None:
                    downtime_until_unix = int(payload["downtime_until_unix"])
        except Exception:
            pass
    except Exception:
        pass
    finally:
        if conn is not None:
            try:
                conn.close()
            except Exception:
                pass
    return (clock_offset_secs, downtime_until_unix)


def _text_mentions_cluster_downtime(text: Any) -> bool:
    if text is None:
        return False
    raw = text if isinstance(text, str) else json.dumps(text, default=str)
    low = raw.lower()
    return any(marker in low for marker in _DOWNTIME_EXPLICIT_MARKERS)


def _text_mentions_disconnect(text: Any) -> bool:
    if text is None:
        return False
    raw = text if isinstance(text, str) else json.dumps(text, default=str)
    low = raw.lower()
    return any(marker in low for marker in _DOWNTIME_DISCONNECT_MARKERS)


def _serialize_failure_fragment(value: Any) -> str:
    if value is None or value == "" or value == [] or value == {}:
        return ""
    return value if isinstance(value, str) else json.dumps(value, default=str)


def _extract_downtime_modal_texts(ui_snapshot: Any) -> List[str]:
    if not isinstance(ui_snapshot, dict):
        frag = _serialize_failure_fragment(ui_snapshot)
        return [frag] if frag else []
    texts: List[str] = []
    containers: List[Dict[str, Any]] = [ui_snapshot]
    nested_ui = ui_snapshot.get("ui")
    if isinstance(nested_ui, dict):
        containers.append(nested_ui)
    for container in containers:
        for key in _DOWNTIME_MODAL_KEYS:
            frag = _serialize_failure_fragment(container.get(key))
            if frag:
                texts.append(frag)
    return texts


def _extract_downtime_scatter_texts(flight_recorder: Any) -> List[str]:
    if not isinstance(flight_recorder, dict):
        return []
    frag = _serialize_failure_fragment(flight_recorder.get("scatters_observed"))
    return [frag] if frag else []


def _extract_flight_recorder_failure_texts(
    flight_recorder: Any,
) -> tuple[List[str], List[str]]:
    """Return (both_texts, explicit_only_texts) from flight_recorder without healthy call chatter."""
    if not isinstance(flight_recorder, dict):
        return ([], [])
    both: List[str] = []
    for key in (
        "log_exceptions",
        "macho_exceptions",
        "quasar_errors",
        "modals_opened",
        "client_restarted_unexpectedly",
    ):
        frag = _serialize_failure_fragment(flight_recorder.get(key))
        if frag:
            both.append(frag)
    macho_calls = flight_recorder.get("macho_calls")
    if isinstance(macho_calls, list):
        for entry in macho_calls:
            if not isinstance(entry, dict):
                continue
            status = str(entry.get("status") or "OK").strip().upper()
            err = entry.get("error") or entry.get("exception")
            if status != "OK" or err:
                frag = _serialize_failure_fragment(
                    {"status": status, "error": err} if err else {"status": status}
                )
                if frag:
                    both.append(frag)
    return (both, _extract_downtime_scatter_texts(flight_recorder))


def _filter_downtime_server_log_lines(text: str) -> str:
    if not text:
        return ""
    kept: List[str] = []
    for line in text.splitlines():
        low = line.lower()
        if any(marker in low for marker in _DOWNTIME_EXPLICIT_MARKERS):
            kept.append(line)
            continue
        if "dispatching rpc call" in low:
            continue
        if any(
            lvl in low
            for lvl in (" warn", " error", " fatal", " critical", "panic", "disconnect", "shutting down")
        ) or low.startswith(("warn", "error", "fatal", "critical")):
            kept.append(line)
    return "\n".join(kept)


# ---------------------------------------------------------------------------
# ANSI Color Coding & Structured Trace Logging
# ---------------------------------------------------------------------------

USE_COLOR = os.environ.get("NO_COLOR", "") == ""

C_RESET = "\033[0m"
C_BOLD = "\033[1m"
C_DIM = "\033[2m"
C_CYAN = "\033[36m"
C_BCYAN = "\033[1;36m"
C_BLUE = "\033[34m"
C_BBLUE = "\033[1;34m"
C_MAGENTA = "\033[35m"
C_BMAGENTA = "\033[1;35m"
C_YELLOW = "\033[33m"
C_BYELLOW = "\033[1;33m"
C_GREEN = "\033[32m"
C_BGREEN = "\033[1;32m"
C_RED = "\033[31m"
C_BRED = "\033[1;31m"
C_WHITE = "\033[37m"


def _c(color: str, text: str) -> str:
    if not USE_COLOR:
        return text
    return f"{color}{text}{C_RESET}"


def log_info(msg: str) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    tag = _c(C_BCYAN, "[playtester]")
    print(f"{ts} {tag} {_c(C_CYAN, msg)}", flush=True)


def log_warn(msg: str) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    tag = _c(C_BYELLOW, "[playtester] WARN:")
    print(f"{ts} {tag} {_c(C_YELLOW, msg)}", file=sys.stderr, flush=True)


def log_err(msg: str) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    tag = _c(C_BRED, "[playtester] ERROR:")
    print(f"{ts} {tag} {_c(C_RED, msg)}", file=sys.stderr, flush=True)


def _extract_agy_transcript_traces(
    conversation_id: str,
    latest_turn_only: bool = True,
) -> Tuple[List[str], List[str]]:
    """
    Read internal thinking blocks and intermediate tool calls from agy's
    `~/.gemini/antigravity-cli/brain/<conversation_id>/.system_generated/logs/transcript_full.jsonl`.
    When `latest_turn_only=True`, slices from the last `USER_INPUT` step so resumed
    per-batch Executor threads only log the current turn's reasoning and tool calls.
    """
    if not conversation_id:
        return [], []
    logs_dir = (
        Path(os.path.expanduser("~/.gemini/antigravity-cli/brain"))
        / conversation_id
        / ".system_generated"
        / "logs"
    )
    transcript_path = logs_dir / "transcript_full.jsonl"
    if not transcript_path.exists():
        transcript_path = logs_dir / "transcript.jsonl"
    if not transcript_path.exists():
        return [], []

    thinking_blocks: List[str] = []
    tool_summaries: List[str] = []
    try:
        parsed_objs: List[Dict[str, Any]] = []
        for line in transcript_path.read_text(encoding="utf-8", errors="replace").splitlines():
            line = line.strip()
            if not line:
                continue
            parsed_objs.append(json.loads(line))
        if latest_turn_only and parsed_objs:
            last_user_idx = -1
            for idx, obj in enumerate(parsed_objs):
                if obj.get("type") == "USER_INPUT":
                    last_user_idx = idx
            if last_user_idx >= 0:
                parsed_objs = parsed_objs[last_user_idx + 1 :]
        for obj in parsed_objs:
            th = (obj.get("thinking") or "").strip()
            if th:
                # Collapse excessive blank lines while preserving paragraph breaks
                cleaned = "\n".join(ln.strip() for ln in th.splitlines() if ln.strip())
                if cleaned:
                    thinking_blocks.append(cleaned)
            for tc in obj.get("tool_calls") or []:
                if not isinstance(tc, dict):
                    continue
                tname = str(tc.get("name") or "")
                if tname and tname != "finish":
                    targs = tc.get("args") or {}
                    if isinstance(targs, dict) and "Path" in targs:
                        tpath_name = Path(str(targs["Path"]).strip('"')).name
                        tool_summaries.append(f"{tname}({tpath_name})")
                    else:
                        tool_summaries.append(tname)
    except Exception:
        pass
    return thinking_blocks, tool_summaries


def _extract_agy_latest_finish_decision(
    conversation_id: str,
    required_key: str = "action",
    expected_marker: str = "",
) -> Optional[Dict[str, Any]]:
    """
    Recover the latest-turn `finish` tool arguments from `agy`'s transcript log
    after the most recent `USER_INPUT`. Workaround for `agy --conversation --output-format json`
    sometimes returning an earlier turn's cached `structured_output`/`response` when resuming a batch.
    """
    if not conversation_id:
        return None
    logs_dir = (
        Path(os.path.expanduser("~/.gemini/antigravity-cli/brain"))
        / conversation_id
        / ".system_generated"
        / "logs"
    )
    transcript_path = logs_dir / "transcript_full.jsonl"
    if not transcript_path.exists():
        transcript_path = logs_dir / "transcript.jsonl"
    if not transcript_path.exists():
        return None

    try:
        parsed_objs: List[Dict[str, Any]] = []
        for line in transcript_path.read_text(encoding="utf-8", errors="replace").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                parsed_objs.append(json.loads(line))
            except Exception:
                continue
        last_user_idx = -1
        for idx, obj in enumerate(parsed_objs):
            if obj.get("type") == "USER_INPUT":
                last_user_idx = idx
        if last_user_idx < 0:
            return None
        if expected_marker:
            user_content = str(parsed_objs[last_user_idx].get("content") or "")
            if expected_marker not in user_content:
                return None

        latest_finish: Optional[Dict[str, Any]] = None
        for obj in parsed_objs[last_user_idx + 1 :]:
            for tc in obj.get("tool_calls") or []:
                if not isinstance(tc, dict) or tc.get("name") != "finish":
                    continue
                raw_args = tc.get("args")
                if raw_args is None:
                    raw_args = tc.get("arguments")
                if isinstance(raw_args, str):
                    try:
                        raw_args = json.loads(raw_args)
                    except Exception:
                        raw_args = None
                if isinstance(raw_args, dict) and isinstance(raw_args.get(required_key), dict):
                    latest_finish = dict(raw_args)
        return latest_finish
    except Exception:
        return None


def log_step_header(
    step_num: int,
    git_head: str,
    memory: "PlaytesterMemory",
    ui_snapshot: Dict[str, Any],
) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    mem = memory.data
    persona = mem.get("active_persona") or {}
    p_id = persona.get("id", "explorer")
    p_step = mem.get("active_persona_steps", 0)
    p_max = persona.get("rotate_after_steps", 25)
    view = (ui_snapshot or {}).get("active_view") or "unknown"
    sess = (ui_snapshot or {}).get("session") or {}
    ui_sub = (ui_snapshot or {}).get("ui") or {}
    wins = ui_sub.get("open_windows") or (ui_snapshot or {}).get("open_windows") or []
    widgets = ui_sub.get("clickable_widgets") or []
    space = (ui_snapshot or {}).get("space") or {}
    if space.get("in_space"):
        ego = space.get("ego") or {}
        loc_str = f"InSpace(sys={sess.get('solarsystemid2')}, mode={ego.get('mode')}, speed={ego.get('speed_mps', 0):.1f}m/s)"
    else:
        loc_str = f"Docked(station={sess.get('stationid') or sess.get('structureid')}, sys={sess.get('solarsystemid2')})"

    rule = _c(C_BCYAN, "═" * 92)
    header = (
        f"{ts} {_c(C_BCYAN, f'▶ STEP {step_num:04d}')} "
        f"{_c(C_DIM, f'(HEAD={git_head})')} | "
        f"{_c(C_BBLUE, f'Persona=[{p_id} {p_step}/{p_max}]')} | "
        f"{_c(C_CYAN, f'View={view} {loc_str} Wins={len(wins)} Widgets={len(widgets)}')}"
    )
    print(f"\n{rule}\n{header}", flush=True)


def log_agy_reasoning_trace(
    step_num: int,
    model_name: str,
    elapsed: float,
    decision: Dict[str, Any],
) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    conv_id = str(decision.get("_conversation_id") or "")
    turns = decision.get("_num_turns") or 1
    usage = decision.get("_usage") or {}
    tools_used = decision.get("_tool_calls") or []
    thinking_blocks = decision.get("_thinking_trace") or []

    usage_parts = []
    if usage.get("input_tokens") is not None:
        usage_parts.append(f"in={usage.get('input_tokens')}")
    if usage.get("output_tokens") is not None:
        usage_parts.append(f"out={usage.get('output_tokens')}")
    if usage.get("thinking_tokens") is not None:
        usage_parts.append(f"think={usage.get('thinking_tokens')}")
    if usage.get("cache_read_tokens"):
        usage_parts.append(f"cached={usage.get('cache_read_tokens')}")
    usage_str = f" tokens({', '.join(usage_parts)})" if usage_parts else ""
    tools_str = f" tools={tools_used}" if tools_used else ""
    conv_str = f" conv={conv_id[:8]}" if conv_id else ""

    print(
        f"{ts} {_c(C_BMAGENTA, '[agy:meta]   ')} "
        f"{_c(C_MAGENTA, f'model={model_name} latency={elapsed:.1f}s turns={turns}{conv_str}{usage_str}{tools_str}')}",
        flush=True,
    )

    for idx, block in enumerate(thinking_blocks, start=1):
        prefix = f"[agy:think#{idx}]" if len(thinking_blocks) > 1 else "[agy:think]  "
        for ln_idx, ln in enumerate(block.splitlines()):
            tag = _c(C_MAGENTA, f"{prefix:<13}") if ln_idx == 0 else " " * 13
            print(f"{ts} {tag} {_c(C_DIM + C_MAGENTA, ln)}", flush=True)

    obs = str(decision.get("observation") or "").strip()
    if obs:
        print(f"{ts} {_c(C_BCYAN, '[agy:observe]')} {_c(C_WHITE, obs)}", flush=True)

    pav = decision.get("previous_action_verification") or {}
    exp_met = pav.get("expectation_met", True)
    disc = str(pav.get("discrepancy_summary") or "").strip()
    if exp_met:
        print(
            f"{ts} {_c(C_BGREEN, '[agy:verify] ')} {_c(C_GREEN, '✓ Previous action expectation met')}",
            flush=True,
        )
    else:
        print(
            f"{ts} {_c(C_BRED, '[agy:verify] ')} {_c(C_RED, f'✗ Discrepancy: {disc}')}",
            flush=True,
        )

    goal = str(decision.get("current_goal") or "").strip()
    if goal:
        print(f"{ts} {_c(C_BBLUE, '[agy:goal]   ')} {_c(C_BLUE, goal)}", flush=True)

    reasoning = str(decision.get("reasoning") or "").strip()
    if reasoning:
        print(f"{ts} {_c(C_BMAGENTA, '[agy:reason] ')} {_c(C_BMAGENTA, reasoning)}", flush=True)

    act = decision.get("action") or {}
    act_type = act.get("type") or "wait"
    act_target = act.get("target") or act.get("command") or ""
    coords = ""
    if act.get("x") or act.get("y"):
        coords = f" @({act.get('x')},{act.get('y')})"
    flags = []
    if act.get("right_click"):
        flags.append("right_click")
    if act.get("double_click"):
        flags.append("double_click")
    flag_str = f" [{' '.join(flags)}]" if flags else ""
    expected = str(act.get("expected_ui_and_state_change") or "").strip()
    print(
        f"{ts} {_c(C_BYELLOW, '[agy:action] ')} "
        f"{_c(C_BYELLOW, f'{act_type}({act_target!r}){coords}{flag_str}')} "
        f"{_c(C_YELLOW, f'-> Expect: {expected}')}",
        flush=True,
    )

    br = decision.get("bug_report") or {}
    if isinstance(br, dict) and br.get("should_file") and br.get("title"):
        sev = str(br.get("severity") or "medium").upper()
        cat = str(br.get("category") or "bug")
        title = str(br.get("title") or "")
        print(
            f"{ts} {_c(C_BRED, '[agy:bug]    ')} "
            f"{_c(C_BRED, f'[{sev}/{cat}] {title}')}",
            flush=True,
        )


def log_planner_trace(
    step_num: int,
    model_name: str,
    elapsed: float,
    plan_decision: Dict[str, Any],
) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    conv_id = str(plan_decision.get("_conversation_id") or "")
    turns = plan_decision.get("_num_turns") or 1
    usage = plan_decision.get("_usage") or {}
    tools_used = plan_decision.get("_tool_calls") or []
    thinking_blocks = plan_decision.get("_thinking_trace") or []

    usage_parts = []
    if usage.get("input_tokens") is not None:
        usage_parts.append(f"in={usage.get('input_tokens')}")
    if usage.get("output_tokens") is not None:
        usage_parts.append(f"out={usage.get('output_tokens')}")
    if usage.get("thinking_tokens") is not None:
        usage_parts.append(f"think={usage.get('thinking_tokens')}")
    if usage.get("cache_read_tokens"):
        usage_parts.append(f"cached={usage.get('cache_read_tokens')}")
    usage_str = f" tokens({', '.join(usage_parts)})" if usage_parts else ""
    tools_str = f" tools={tools_used}" if tools_used else ""
    conv_str = f" conv={conv_id[:8]}" if conv_id else ""

    print(
        f"{ts} {_c(C_BBLUE, '[agy:planner]')} "
        f"{_c(C_BLUE, f'model={model_name} latency={elapsed:.1f}s turns={turns}{conv_str}{usage_str}{tools_str}')}",
        flush=True,
    )
    for idx, block in enumerate(thinking_blocks, start=1):
        prefix = f"[plan:thk#{idx}]" if len(thinking_blocks) > 1 else "[plan:think] "
        for ln_idx, ln in enumerate(block.splitlines()):
            tag = _c(C_BLUE, f"{prefix:<13}") if ln_idx == 0 else " " * 13
            print(f"{ts} {tag} {_c(C_DIM + C_BLUE, ln)}", flush=True)

    obs = str(plan_decision.get("observation") or "").strip()
    if obs:
        print(f"{ts} {_c(C_BCYAN, '[plan:obs]   ')} {_c(C_WHITE, obs)}", flush=True)

    pav = plan_decision.get("previous_action_verification") or {}
    exp_met = pav.get("expectation_met", True)
    disc = str(pav.get("discrepancy_summary") or "").strip()
    if exp_met:
        print(
            f"{ts} {_c(C_BGREEN, '[plan:verify]')} {_c(C_GREEN, '✓ Previous batch/action expectation met')}",
            flush=True,
        )
    else:
        print(
            f"{ts} {_c(C_BRED, '[plan:verify]')} {_c(C_RED, f'✗ Discrepancy: {disc}')}",
            flush=True,
        )

    reasoning = str(plan_decision.get("reasoning") or "").strip()
    if reasoning:
        print(f"{ts} {_c(C_BMAGENTA, '[plan:reason]')} {_c(C_BMAGENTA, reasoning)}", flush=True)

    dp = plan_decision.get("delegated_plan") or {}
    p_goal = str(dp.get("plan_goal") or "").strip()
    max_s = dp.get("max_batch_steps", 1)
    h_rat = str(dp.get("horizon_rationale") or "").strip()
    print(
        f"{ts} {_c(C_BCYAN, '[plan:batch] ')} "
        f"{_c(C_BCYAN, f'Goal={p_goal!r} (horizon={max_s} steps — {h_rat})')}",
        flush=True,
    )
    for st in dp.get("steps") or []:
        if isinstance(st, dict):
            s_no = st.get("step_number", "?")
            inst = st.get("instruction", "")
            at_h = st.get("action_type_hint", "")
            tg_h = st.get("target_or_command_hint", "")
            print(
                f"{ts}   {_c(C_CYAN, f'[{s_no}/{max_s}] {at_h}({tg_h!r}): {inst}')}",
                flush=True,
            )

    br = plan_decision.get("bug_report") or {}
    if isinstance(br, dict) and br.get("should_file") and br.get("title"):
        sev = str(br.get("severity") or "medium").upper()
        cat = str(br.get("category") or "bug")
        title = str(br.get("title") or "")
        print(
            f"{ts} {_c(C_BRED, '[plan:bug]   ')} "
            f"{_c(C_BRED, f'[{sev}/{cat}] {title}')}",
            flush=True,
        )


def log_executor_trace(
    step_num: int,
    batch_step_idx: int,
    max_batch_steps: int,
    model_name: str,
    elapsed: float,
    exec_decision: Dict[str, Any],
) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    conv_id = str(exec_decision.get("_conversation_id") or "")
    turns = exec_decision.get("_num_turns") or 1
    usage = exec_decision.get("_usage") or {}
    tools_used = exec_decision.get("_tool_calls") or []
    thinking_blocks = exec_decision.get("_thinking_trace") or []

    usage_parts = []
    if usage.get("input_tokens") is not None:
        usage_parts.append(f"in={usage.get('input_tokens')}")
    if usage.get("output_tokens") is not None:
        usage_parts.append(f"out={usage.get('output_tokens')}")
    if usage.get("thinking_tokens") is not None:
        usage_parts.append(f"think={usage.get('thinking_tokens')}")
    if usage.get("cache_read_tokens"):
        usage_parts.append(f"cached={usage.get('cache_read_tokens')}")
    usage_str = f" tokens({', '.join(usage_parts)})" if usage_parts else ""
    tools_str = f" tools={tools_used}" if tools_used else ""
    conv_str = f" batch_conv={conv_id[:8]}" if conv_id else ""

    print(
        f"{ts} {_c(C_BMAGENTA, f'[agy:exec {batch_step_idx}/{max_batch_steps}]')} "
        f"{_c(C_MAGENTA, f'model={model_name} latency={elapsed:.1f}s turns={turns}{conv_str}{usage_str}{tools_str}')}",
        flush=True,
    )
    for idx, block in enumerate(thinking_blocks, start=1):
        prefix = f"[exec:thk#{idx}]" if len(thinking_blocks) > 1 else "[exec:think] "
        for ln_idx, ln in enumerate(block.splitlines()):
            tag = _c(C_MAGENTA, f"{prefix:<13}") if ln_idx == 0 else " " * 13
            print(f"{ts} {tag} {_c(C_DIM + C_MAGENTA, ln)}", flush=True)

    obs = str(exec_decision.get("observation") or "").strip()
    if obs:
        print(f"{ts} {_c(C_BCYAN, '[exec:obs]   ')} {_c(C_WHITE, obs)}", flush=True)

    pav = exec_decision.get("previous_action_verification") or {}
    exp_met = pav.get("expectation_met", True)
    disc = str(pav.get("discrepancy_summary") or "").strip()
    if not exp_met:
        print(
            f"{ts} {_c(C_BRED, '[exec:verify]')} {_c(C_RED, f'✗ Discrepancy: {disc}')}",
            flush=True,
        )

    b_status = str(exec_decision.get("batch_status") or "continue")
    esc_reason = str(exec_decision.get("escalation_reason") or "").strip()
    reasoning = str(exec_decision.get("reasoning") or "").strip()
    if reasoning:
        print(
            f"{ts} {_c(C_BMAGENTA, '[exec:reason]')} {_c(C_BMAGENTA, f'[{b_status.upper()}] {reasoning}')}",
            flush=True,
        )

    if b_status == "escalate" or not exp_met:
        print(
            f"{ts} {_c(C_BRED, '[exec:escal] ')} "
            f"{_c(C_BRED, f'Escalating to Parent Planner: {esc_reason or disc}')}",
            flush=True,
        )
        return

    act = exec_decision.get("action") or {}
    act_type = act.get("type") or "wait"
    act_target = act.get("target") or act.get("command") or ""
    coords = ""
    if act.get("x") or act.get("y"):
        coords = f" @({act.get('x')},{act.get('y')})"
    flags = []
    if act.get("right_click"):
        flags.append("right_click")
    if act.get("double_click"):
        flags.append("double_click")
    flag_str = f" [{' '.join(flags)}]" if flags else ""
    expected = str(act.get("expected_ui_and_state_change") or "").strip()
    print(
        f"{ts} {_c(C_BYELLOW, '[agy:action] ')} "
        f"{_c(C_BYELLOW, f'{act_type}({act_target!r}){coords}{flag_str}')} "
        f"{_c(C_YELLOW, f'-> Expect: {expected}')}",
        flush=True,
    )


def log_step_execution_outcome(step_num: int, exec_res: Dict[str, Any], decision: Dict[str, Any]) -> None:
    ts = _c(C_DIM, f"[{datetime.now().strftime('%H:%M:%S')}]")
    ok = bool(exec_res.get("action_ok", exec_res.get("ok", False)))
    err = exec_res.get("action_error") or exec_res.get("error")
    matched = exec_res.get("matched_widget") or exec_res.get("clicked_widget") or ""
    sd = exec_res.get("state_diff") or {}
    fr = exec_res.get("flight_recorder") or {}
    calls = [
        f"{c.get('service')}.{c.get('method')}({c.get('status')},{c.get('duration_ms', 0):.0f}ms)"
        for c in (fr.get("macho_calls") or [])
        if isinstance(c, dict)
    ]
    scatters = sd.get("scatters_observed") or fr.get("scatters_observed") or []
    uniq_scatters = list(dict.fromkeys(scatters))[:8]
    log_excs = fr.get("log_exceptions") or []
    macho_excs = fr.get("macho_exceptions") or []
    quasar_errs = fr.get("quasar_errors") or []

    status_badge = _c(C_BGREEN, "✓ OK") if ok else _c(C_BRED, f"✗ FAIL ({err})")
    diff_summary = (
        f"ui_changed={sd.get('ui_changed')} "
        f"opened={sd.get('windows_opened', [])} "
        f"closed={sd.get('windows_closed', [])} "
        f"modals={sd.get('modals_appeared', [])} "
        f"widget_delta={sd.get('clickable_widget_count_delta', 0)}"
    )
    if sd.get("view_changed"):
        diff_summary += f" view_changed={sd.get('view_changed')}"
    if matched:
        diff_summary = f"matched={matched} | " + diff_summary

    print(
        f"{ts} {_c(C_BGREEN if ok else C_BRED, '[client:exec]')} {status_badge} {_c(C_GREEN if ok else C_RED, diff_summary)}",
        flush=True,
    )
    if calls or uniq_scatters:
        rpc_line = f"RPCs={calls[:6] if calls else '[]'} | Scatters={uniq_scatters}"
        print(f"{ts} {_c(C_CYAN, '[client:wire]')} {_c(C_DIM + C_CYAN, rpc_line)}", flush=True)
    if log_excs or macho_excs or quasar_errs:
        exc_heads = [str(e.get("headline") or e)[:100] for e in (log_excs + macho_excs + quasar_errs)[:3]]
        print(f"{ts} {_c(C_BRED, '[client:exc] ')} {_c(C_RED, ' | '.join(exc_heads))}", flush=True)

    mem_upd = decision.get("memory_update") or {}
    tested = mem_upd.get("mark_tested_features") or []
    buggy = mem_upd.get("mark_buggy_features") or []
    note = str(mem_upd.get("long_term_note_to_append") or "").strip()
    if tested or buggy or note:
        mem_parts = []
        if tested:
            mem_parts.append(f"verified+={tested}")
        if buggy:
            mem_parts.append(f"buggy+={buggy}")
        if note:
            mem_parts.append(f"note={note[:120]!r}")
        print(f"{ts} {_c(C_BBLUE, '[memory:upd] ')} {_c(C_BLUE, ' | '.join(mem_parts))}", flush=True)


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def mint_sso_jwt(
    account_id: int = 1,
    character_id: int = 90000001,
    secret: str = HARNESS_JWT_SECRET,
    ttl_secs: int = 86400 * 7,
) -> str:
    now = int(time.time())
    header = {"alg": "HS256", "typ": "JWT"}
    payload = {
        "sub": f"CHARACTER:EVE:{character_id}",
        "account_id": account_id,
        "character_id": character_id,
        "role": 0x7FFFFFFFFFFFFFFF,
        "iat": now - 60,
        "nbf": now - 60,
        "exp": now + ttl_secs,
        "iss": "evomu-harness",
        "name": "Demo Pilot",
    }
    signing_input = (
        _b64url(json.dumps(header, separators=(",", ":")).encode("utf-8"))
        + "."
        + _b64url(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
    ).encode("ascii")
    sig = hmac.new(secret.encode("utf-8"), signing_input, hashlib.sha256).digest()
    return signing_input.decode("ascii") + "." + _b64url(sig)


def mint_sso_jwt_via_server(
    esi_port: int,
    username: str = "demo",
    password_token: str = "demo-secret",
) -> str:
    """Mint a real OAuth2 PKCE access_token JWT against `eve-server`'s `/v2/oauth` endpoints."""
    import http.client
    import urllib.parse

    verifier = "evomu_harness_pkce_verifier_0123456789abcdef"
    challenge = (
        base64.urlsafe_b64encode(hashlib.sha256(verifier.encode("ascii")).digest())
        .decode("ascii")
        .rstrip("=")
    )
    redir = "eveauth-evomu://callback"
    hdrs = {"Content-Type": "application/x-www-form-urlencoded"}
    conn = http.client.HTTPConnection("127.0.0.1", int(esi_port), timeout=5.0)
    try:
        conn.request(
            "POST",
            "/v2/oauth/authorize",
            urllib.parse.urlencode(
                {
                    "response_type": "code",
                    "client_id": "eveLauncherTQ",
                    "redirect_uri": redir,
                    "code_challenge": challenge,
                    "code_challenge_method": "S256",
                    "scope": "publicData",
                    "username": username,
                    "password": password_token,
                }
            ),
            hdrs,
        )
        r1 = conn.getresponse()
        b1 = r1.read().decode("utf-8", "replace")
        code = (json.loads(b1) if b1.strip().startswith("{") else {}).get("code", "")
        if not code:
            loc = r1.getheader("Location") or ""
            code = urllib.parse.parse_qs(urllib.parse.urlparse(loc).query).get("code", [""])[0]
        conn.request(
            "POST",
            "/v2/oauth/token",
            urllib.parse.urlencode(
                {
                    "grant_type": "authorization_code",
                    "client_id": "eveLauncherTQ",
                    "redirect_uri": redir,
                    "code": code,
                    "code_verifier": verifier,
                }
            ),
            hdrs,
        )
        return str(json.loads(conn.getresponse().read().decode("utf-8"))["access_token"])
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# macOS Window Discovery (All Mission Control Spaces) & Screenshot Capture
# ---------------------------------------------------------------------------

_SWIFT_WINDOW_INFO_SRC = """
import Cocoa
import CoreGraphics
import Foundation

if CommandLine.arguments.count > 1 && CommandLine.arguments[1] == "--screen" {
    if let scr = NSScreen.main ?? NSScreen.screens.first {
        let vf = scr.visibleFrame
        let f = scr.frame
        let info: [String: Any] = [
            "frameWidth": Int(f.width),
            "frameHeight": Int(f.height),
            "visibleMinX": Int(vf.minX),
            "visibleMinY": Int(vf.minY),
            "visibleWidth": Int(vf.width),
            "visibleHeight": Int(vf.height),
            "backingScaleFactor": Double(scr.backingScaleFactor),
        ]
        if let data = try? JSONSerialization.data(withJSONObject: info),
           let s = String(data: data, encoding: .utf8) {
            print(s)
        } else {
            print("null")
        }
    } else {
        print("null")
    }
    exit(0)
}

let targetPid: Int32? = CommandLine.arguments.count > 1 ? Int32(CommandLine.arguments[1]) : nil
// IMPORTANT: Omit .optionOnScreenOnly so we find EVE.app windows even when placed on a
// separate macOS Mission Control Desktop Space or secondary display!
let opts = CGWindowListOption(arrayLiteral: .excludeDesktopElements)
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    print("null")
    exit(0)
}
var best: [String: Any]? = nil
var bestScore = 0
for w in list {
    let owner = (w[kCGWindowOwnerName as String] as? String) ?? ""
    let name = (w[kCGWindowName as String] as? String) ?? ""
    let wpid = (w[kCGWindowOwnerPID as String] as? Int32) ?? 0
    let layer = (w[kCGWindowLayer as String] as? Int) ?? 0
    let winId = (w[kCGWindowNumber as String] as? Int) ?? 0
    let onscreen = (w[kCGWindowIsOnscreen as String] as? Bool) ?? false
    guard let bounds = w[kCGWindowBounds as String] as? [String: Any] else { continue }
    let width = Int((bounds["Width"] as? Double) ?? 0)
    let height = Int((bounds["Height"] as? Double) ?? 0)
    let x = Int((bounds["X"] as? Double) ?? 0)
    let y = Int((bounds["Y"] as? Double) ?? 0)
    // EVE.app main 3D game window uses layer 0 (windowed) or layer 25 (borderless/fullscreen),
    // while also creating a hidden 500x500 helper window at layer 0 with name "".
    if width < 320 || height < 240 || layer < 0 || layer > 30 { continue }
    let match: Bool
    if let tp = targetPid, tp > 0 {
        match = (wpid == tp)
    } else {
        let lowOwner = owner.lowercased()
        match = (lowOwner == "exefile" || lowOwner == "eve")
    }
    if match {
        let area = width * height
        let titleBonus = name.uppercased().hasPrefix("EVE") ? 100_000_000 : (name.isEmpty ? 0 : 10_000_000)
        let score = area + titleBonus
        if score > bestScore {
            bestScore = score
            best = [
                "id": winId,
                "pid": Int(wpid),
                "owner": owner,
                "name": name,
                "layer": layer,
                "x": x,
                "y": y,
                "width": width,
                "height": height,
                "area": area,
                "onscreen": onscreen
            ]
        }
    }
}
if let b = best, let data = try? JSONSerialization.data(withJSONObject: b), let s = String(data: data, encoding: .utf8) {
    print(s)
} else {
    print("null")
}
"""


def _ensure_window_info_helper() -> Optional[Path]:
    if WINDOW_INFO_HELPER_BIN.exists():
        return WINDOW_INFO_HELPER_BIN
    try:
        DEFAULT_CACHE_ROOT.mkdir(parents=True, exist_ok=True)
        swift_src = DEFAULT_CACHE_ROOT / "evomu_window_info_v2.swift"
        swift_src.write_text(_SWIFT_WINDOW_INFO_SRC, encoding="utf-8")
        proc = subprocess.run(
            ["swiftc", "-O", str(swift_src), "-o", str(WINDOW_INFO_HELPER_BIN)],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if proc.returncode == 0 and WINDOW_INFO_HELPER_BIN.exists():
            return WINDOW_INFO_HELPER_BIN
    except Exception as exc:
        log_warn(f"Failed to compile evomu_window_info helper: {exc}")
    return None


def get_main_display_info() -> Optional[Dict[str, Any]]:
    """Return main display visibleFrame and Retina backingScaleFactor via Cocoa NSScreen."""
    helper = _ensure_window_info_helper()
    if helper is None:
        return None
    try:
        proc = subprocess.run(
            [str(helper), "--screen"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if proc.returncode == 0:
            data = json.loads(proc.stdout.strip() or "null")
            if isinstance(data, dict) and data.get("visibleWidth") and data.get("visibleHeight"):
                return data
    except Exception:
        pass
    return None


_SUPPORTED_EVE_UI_SCALES = (0.5, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0)


def _snap_eve_ui_scale(scale: float) -> float:
    return min(_SUPPORTED_EVE_UI_SCALES, key=lambda s: (abs(s - scale), -s))


def resolve_playtester_window_spec(
    window_size: str = "full",
    ui_scale: float = 0.0,
    display_info: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    """Compute EVE.app physical WindowedSettings (px) and Cocoa left/top + UI scale."""
    mode = (window_size or "full").strip().lower()
    if mode not in ("full", "max", "maximize", "screen"):
        m = re.fullmatch(r"(\d{3,4})x(\d{3,4})", mode)
        if m:
            w_px, h_px = max(800, int(m.group(1))), max(600, int(m.group(2)))
            sc = _snap_eve_ui_scale(ui_scale if ui_scale and ui_scale > 0 else 1.0)
            return {"width": w_px, "height": h_px, "left": 40, "top": 40, "ui_scale": sc}

    info = display_info if isinstance(display_info, dict) else get_main_display_info()
    if isinstance(info, dict):
        vis_w = int(info.get("visibleWidth") or 0)
        vis_h = int(info.get("visibleHeight") or 0)
        vis_x = int(info.get("visibleMinX") or 0)
        vis_y = int(info.get("visibleMinY") or 0)
        backing = float(info.get("backingScaleFactor") or 1.0)
        if backing < 1.0 or backing > 4.0:
            backing = 1.0
        # macOS windowed titlebar consumes ~28pt above the Tr2MainWindow client height.
        titlebar_pt = 28
        if vis_w >= 800 and vis_h >= 600:
            content_h_pt = max(600, vis_h - titlebar_pt)
            width_px = max(800, int(round(vis_w * backing)))
            height_px = max(600, int(round(content_h_pt * backing)))
            if ui_scale and ui_scale > 0:
                resolved_scale = _snap_eve_ui_scale(float(ui_scale))
            else:
                resolved_scale = _snap_eve_ui_scale(min(2.0, backing) if backing >= 1.5 else 1.0)
            return {
                "width": width_px,
                "height": height_px,
                "left": vis_x,
                "top": vis_y,
                "ui_scale": resolved_scale,
            }

    sc = _snap_eve_ui_scale(ui_scale if ui_scale and ui_scale > 0 else 1.0)
    return {"width": 1280, "height": 800, "left": 40, "top": 40, "ui_scale": sc}


_WINDOW_INFO_CACHE: Dict[int, Tuple[float, Optional[Dict[str, Any]]]] = {}
_WINDOW_INFO_CACHE_TTL_SECS = 30.0


def find_exefile_window(
    pid: Optional[int] = None, force_refresh: bool = False
) -> Optional[Dict[str, Any]]:
    """
    Locate the live EVE.app (`exefile`) window across ALL macOS Mission Control Desktop Spaces
    and displays via CoreGraphics `CGWindowListCopyWindowInfo` (cached 30s).
    """
    cache_key = int(pid or 0)
    now = time.time()
    if not force_refresh:
        cached = _WINDOW_INFO_CACHE.get(cache_key)
        if cached is not None and (now - cached[0]) < _WINDOW_INFO_CACHE_TTL_SECS:
            return cached[1]
    helper = _ensure_window_info_helper()
    if helper is not None:
        try:
            args = [str(helper)]
            if pid:
                args.append(str(pid))
            proc = subprocess.run(args, capture_output=True, text=True, timeout=4)
            if proc.returncode == 0 and proc.stdout.strip():
                data = json.loads(proc.stdout.strip())
                if isinstance(data, dict) and data.get("id"):
                    _WINDOW_INFO_CACHE[cache_key] = (now, data)
                    return data
        except Exception:
            pass
    _WINDOW_INFO_CACHE.pop(cache_key, None)
    return None


def focus_exefile_window(pid: Optional[int] = None) -> bool:
    """Bring the EVE client window to foreground if allowed."""
    if not pid:
        return False
    try:
        apple = (
            f'tell application "System Events"\n'
            f'  set frontmost of (first process whose unix id is {int(pid)}) to true\n'
            f'end tell'
        )
        subprocess.run(["osascript", "-e", apple], capture_output=True, timeout=3)
        return True
    except Exception:
        return False


def _read_image_dimensions(path: Path) -> Optional[Tuple[int, int]]:
    """Read pixel `(width, height)` cheaply from PNG IHDR, falling back to `sips -g`."""
    try:
        with path.open("rb") as fh:
            header = fh.read(24)
        if len(header) >= 24 and header[:8] == b"\x89PNG\r\n\x1a\n" and header[12:16] == b"IHDR":
            w, h = struct.unpack(">II", header[16:24])
            if w > 0 and h > 0:
                return int(w), int(h)
    except Exception:
        pass
    try:
        proc = subprocess.run(
            ["sips", "-g", "pixelWidth", "-g", "pixelHeight", str(path)],
            capture_output=True,
            text=True,
            timeout=4,
        )
        if proc.returncode == 0:
            mw = re.search(r"pixelWidth:\s*(\d+)", proc.stdout)
            mh = re.search(r"pixelHeight:\s*(\d+)", proc.stdout)
            if mw and mh:
                return int(mw.group(1)), int(mh.group(1))
    except Exception:
        pass
    return None


def _downscale_screenshot_if_needed(dest_path: Path, max_dim: int = 1024) -> None:
    """Downscale large/Retina screenshots so max(width, height) <= `max_dim` before agent inspection."""
    try:
        if max_dim <= 0 or not dest_path.exists():
            return
        dims = _read_image_dimensions(dest_path)
        need_resize = (
            max(dims) > int(max_dim)
            if dims is not None
            else dest_path.stat().st_size > 250_000
        )
        if need_resize:
            subprocess.run(
                ["sips", "-Z", str(int(max_dim)), str(dest_path)],
                capture_output=True,
                timeout=6,
            )
    except Exception:
        pass


def capture_window_screenshot(
    dest_path: Path,
    client_pid: Optional[int] = None,
    bring_foreground: bool = False,
    rpc_client: Optional[Any] = None,
    max_dim: int = 1024,
) -> Dict[str, Any]:
    """Capture the live EVE.app window PNG via `screencapture -l` or in-engine Trinity `SaveRenderTarget`."""
    dest_path.parent.mkdir(parents=True, exist_ok=True)
    if bring_foreground and client_pid:
        focus_exefile_window(client_pid)
        time.sleep(0.1)

    win = find_exefile_window(client_pid)
    onscreen = bool(win.get("onscreen", True)) if isinstance(win, dict) else False

    # 1. Primary fast path: capture directly from EVE.app's Trinity backbuffer via RPC (~30 ms,
    # works on any Mission Control Desktop Space or background window without WindowServer hangs).
    if rpc_client is not None:
        try:
            trinity_res = rpc_client.call(
                "capture_screenshot", {"path": str(dest_path)}, timeout=8.0
            )
            if (
                isinstance(trinity_res, dict)
                and trinity_res.get("ok")
                and dest_path.exists()
                and dest_path.stat().st_size > 0
            ):
                _downscale_screenshot_if_needed(dest_path, max_dim=max_dim)
                return {
                    "ok": True,
                    "mode": trinity_res.get("mode", "trinity_backbuffer"),
                    "window": win,
                    "path": str(dest_path),
                }
        except Exception:
            pass

    # 2. Fallback: macOS window-ID capture (`screencapture -l <CGWindowID>`)
    if win and win.get("id"):
        win_id = win["id"]
        try:
            res = subprocess.run(
                ["screencapture", "-l", str(win_id), "-x", "-o", str(dest_path)],
                capture_output=True,
                text=True,
                timeout=4,
            )
            if res.returncode == 0 and dest_path.exists() and dest_path.stat().st_size > 0:
                _downscale_screenshot_if_needed(dest_path, max_dim=max_dim)
                return {"ok": True, "mode": "window_id", "window": win, "path": str(dest_path)}
        except Exception:
            pass

    # If cached window lookup missed or CGWindowID rotated, refresh once before giving up.
    if client_pid is not None:
        fresh_win = find_exefile_window(client_pid, force_refresh=True)
        if fresh_win and fresh_win.get("id") and fresh_win.get("id") != (win or {}).get("id"):
            win = fresh_win
            onscreen = bool(win.get("onscreen", True))
            try:
                res = subprocess.run(
                    ["screencapture", "-l", str(win["id"]), "-x", "-o", str(dest_path)],
                    capture_output=True,
                    text=True,
                    timeout=4,
                )
                if res.returncode == 0 and dest_path.exists() and dest_path.stat().st_size > 0:
                    _downscale_screenshot_if_needed(dest_path, max_dim=max_dim)
                    return {"ok": True, "mode": "window_id", "window": win, "path": str(dest_path)}
            except Exception:
                pass

    # 3. Only use bounding-box / screen capture fallback if EVE.app is actually onscreen on the active Space!
    # Never capture the active screen when EVE.app is on a different Desktop Space (`onscreen == False`),
    # because that would screenshot the user's IDE/desktop instead of EVE.app.
    if win and onscreen:
        rx, ry, rw, rh = win["x"], win["y"], win["width"], win["height"]
        res = subprocess.run(
            ["screencapture", "-R", f"{rx},{ry},{rw},{rh}", "-x", str(dest_path)],
            capture_output=True,
            text=True,
            timeout=6,
        )
        if res.returncode == 0 and dest_path.exists() and dest_path.stat().st_size > 0:
            _downscale_screenshot_if_needed(dest_path, max_dim=max_dim)
            return {"ok": True, "mode": "window_rect", "window": win, "path": str(dest_path)}

    return {"ok": False, "mode": "offscreen_unavailable", "window": win, "path": str(dest_path)}


def quartz_mouse_click(
    x: int,
    y: int,
    right_click: bool = False,
    double_click: bool = False,
    win_bounds: Optional[Dict[str, Any]] = None,
    client_desktop_size: Optional[Tuple[int, int]] = None,
) -> bool:
    """Send an OS-level mouse click at client coordinates (mapped to screen coordinates)."""
    screen_x, screen_y = x, y
    if win_bounds:
        wx = int(win_bounds.get("x", 0))
        wy = int(win_bounds.get("y", 0))
        ww = int(win_bounds.get("width", 1280))
        wh = int(win_bounds.get("height", 800))
        if client_desktop_size and client_desktop_size[0] > 0 and client_desktop_size[1] > 0:
            cw, ch = client_desktop_size
            screen_x = int(wx + (x / float(cw)) * ww)
            screen_y = int(wy + (y / float(ch)) * wh)
        else:
            screen_x = wx + x
            screen_y = wy + y

    script = """
import sys, time
import Quartz

x = float(sys.argv[1])
y = float(sys.argv[2])
right = sys.argv[3] == "1"
double = sys.argv[4] == "1"
pt = Quartz.CGPointMake(x, y)

move = Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventMouseMoved, pt, Quartz.kCGMouseButtonLeft)
Quartz.CGEventPost(Quartz.kCGHIDEventTap, move)
time.sleep(0.04)

btn = Quartz.kCGMouseButtonRight if right else Quartz.kCGMouseButtonLeft
down_type = Quartz.kCGEventRightMouseDown if right else Quartz.kCGEventLeftMouseDown
up_type = Quartz.kCGEventRightMouseUp if right else Quartz.kCGEventLeftMouseUp

clicks = 2 if double else 1
for c in range(1, clicks + 1):
    ev_down = Quartz.CGEventCreateMouseEvent(None, down_type, pt, btn)
    Quartz.CGEventSetIntegerValueField(ev_down, Quartz.kCGMouseEventClickState, c)
    Quartz.CGEventPost(Quartz.kCGHIDEventTap, ev_down)
    time.sleep(0.04)
    ev_up = Quartz.CGEventCreateMouseEvent(None, up_type, pt, btn)
    Quartz.CGEventSetIntegerValueField(ev_up, Quartz.kCGMouseEventClickState, c)
    Quartz.CGEventPost(Quartz.kCGHIDEventTap, ev_up)
    time.sleep(0.05)
"""
    for py_bin in ("/usr/bin/python3", sys.executable):
        if not os.path.exists(py_bin):
            continue
        try:
            res = subprocess.run(
                [
                    py_bin,
                    "-c",
                    script,
                    str(screen_x),
                    str(screen_y),
                    "1" if right_click else "0",
                    "1" if double_click else "0",
                ],
                capture_output=True,
                timeout=4,
            )
            if res.returncode == 0:
                return True
        except Exception:
            continue
    return False


# ---------------------------------------------------------------------------
# In-Client JSON-RPC Bridge Client
# ---------------------------------------------------------------------------

class InClientRpcBridge:
    def __init__(self, host: str = "127.0.0.1", port: int = 26099, timeout: float = 15.0):
        self.host = host
        self.port = port
        self.timeout = timeout
        self._req_id = 0

    def call(
        self,
        method: str,
        params: Optional[Dict[str, Any]] = None,
        timeout: Optional[float] = None,
    ) -> Dict[str, Any]:
        self._req_id += 1
        payload = {
            "id": self._req_id,
            "method": method,
            "params": params or {},
        }
        raw_req = (json.dumps(payload) + "\n").encode("utf-8")
        eff_timeout = timeout if timeout is not None else self.timeout
        with socket.create_connection((self.host, self.port), timeout=eff_timeout) as sock:
            sock.settimeout(eff_timeout)
            sock.sendall(raw_req)
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = sock.recv(65536)
                if not chunk:
                    break
                buf += chunk
        if not buf:
            raise RuntimeError(f"Empty response from in-client RPC ({method})")
        resp = json.loads(buf.decode("utf-8", errors="replace").strip())
        if not resp.get("ok"):
            raise RuntimeError(
                f"In-client RPC {method} error: {resp.get('error')}\n{resp.get('traceback', '')}"
            )
        return resp.get("result") or {}

    def ping(self) -> bool:
        try:
            res = self.call("ping", timeout=3.0)
            return bool(res.get("pong"))
        except Exception:
            return False


# ---------------------------------------------------------------------------
# Cargo Concurrency Guard & Server Build / Hot-Reload Manager
# ---------------------------------------------------------------------------

def active_cargo_count() -> int:
    try:
        res = subprocess.run(["pgrep", "-x", "cargo"], capture_output=True, text=True)
        if res.returncode != 0 or not res.stdout.strip():
            return 0
        return len([ln for ln in res.stdout.strip().splitlines() if ln.strip()])
    except Exception:
        return 0


def wait_for_cargo_slot(max_concurrent: int = 2) -> None:
    waited = 0
    while active_cargo_count() >= max_concurrent:
        if waited == 0:
            log_info(
                f"Cargo concurrency guard: >= {max_concurrent} cargo processes active; waiting..."
            )
        time.sleep(10)
        waited += 10


def get_git_head(repo_dir: Path) -> str:
    try:
        res = subprocess.run(
            ["git", "-C", str(repo_dir), "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        return res.stdout.strip() or "unknown"
    except Exception:
        return "unknown"


def _cutover_worktree_to_origin_main(repo_dir: Path) -> bool:
    """Reset ALL tracked files & index to `origin/main` while preserving runtime dirs."""
    # Unstage any runtime directories first so `git reset --hard` cannot remove staged-only runtime files.
    subprocess.run(
        ["git", "-C", str(repo_dir), "reset", "-q", "--", ".playtester", ".evomu-bugs"],
        capture_output=True,
        timeout=10,
    )
    res = subprocess.run(
        ["git", "-C", str(repo_dir), "reset", "--hard", "origin/main"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if res.returncode != 0:
        err = (res.stderr or res.stdout or "").strip()
        log_warn(f"git reset --hard origin/main failed in {repo_dir}: {err}")
        return False
    return True


def sync_worktree_to_origin_main(
    repo_dir: Path,
    *,
    fetch: bool = True,
    heal_drift: bool = False,
) -> Optional[str]:
    """
    Fetch `origin/main` (when `fetch=True`) and cut ALL tracked files over to
    `origin/main` when `origin/main` is not an ancestor of `HEAD` (or when
    `heal_drift=True` with `HEAD == origin/main` and tracked files/index differ
    from `origin/main`). Preserves untracked/ignored runtime dirs (`.playtester/`).
    """
    try:
        if fetch:
            fetch_res = subprocess.run(
                ["git", "-C", str(repo_dir), "fetch", "origin", "main"],
                capture_output=True,
                text=True,
                timeout=25,
            )
            if fetch_res.returncode != 0 and fetch_res.stderr:
                log_warn(f"git fetch origin/main warning in {repo_dir}: {fetch_res.stderr.strip()}")
        head = subprocess.run(
            ["git", "-C", str(repo_dir), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=5,
        ).stdout.strip()
        origin = subprocess.run(
            ["git", "-C", str(repo_dir), "rev-parse", "origin/main"],
            capture_output=True,
            text=True,
            timeout=5,
        ).stdout.strip()
        if not origin:
            return None
        needs_cutover = False
        if head and head != origin:
            # Only hot-reload if origin/main is NOT already an ancestor of HEAD
            anc = subprocess.run(
                ["git", "-C", str(repo_dir), "merge-base", "--is-ancestor", origin, head],
                capture_output=True,
                timeout=5,
            )
            if anc.returncode != 0:
                needs_cutover = True
        elif heal_drift and head == origin:
            wt_diff = subprocess.run(
                ["git", "-C", str(repo_dir), "diff", "--quiet", "origin/main"],
                capture_output=True,
                timeout=10,
            )
            idx_diff = subprocess.run(
                ["git", "-C", str(repo_dir), "diff", "--cached", "--quiet", "origin/main"],
                capture_output=True,
                timeout=10,
            )
            if wt_diff.returncode != 0 or idx_diff.returncode != 0:
                needs_cutover = True
        if needs_cutover:
            log_info(
                f"Cutting over worktree {repo_dir} from {head[:9] if head else 'unknown'} "
                f"to origin/main ({origin[:9]})..."
            )
            if _cutover_worktree_to_origin_main(repo_dir):
                return origin[:9]
    except Exception as exc:
        log_warn(f"git fetch origin/main check failed: {exc}")
    return None


def check_origin_main_update(repo_dir: Path) -> Optional[str]:
    """Fetch origin/main, cut ALL tracked files over to origin/main, and return new short SHA."""
    return sync_worktree_to_origin_main(repo_dir, fetch=True, heal_drift=False)


def compile_eve_server(worktree_dir: Path) -> Tuple[bool, Path, str]:
    """Compile `target/debug/eve-server` directly (skipping rebuild if HEAD is unchanged; Playtester is exempt from coder cargo slot limits)."""
    binary = worktree_dir / "target" / "debug" / "eve-server"
    stamp_file = worktree_dir / "target" / "debug" / ".eve-server-head"
    cur_head = get_git_head(worktree_dir)
    if binary.exists() and stamp_file.exists():
        try:
            built_head = stamp_file.read_text(encoding="utf-8").strip()
            crates_clean = (
                subprocess.run(
                    ["git", "-C", str(worktree_dir), "diff", "--quiet", "HEAD", "--", "crates"],
                    capture_output=True,
                    timeout=5,
                ).returncode
                == 0
            )
            if built_head == cur_head and crates_clean:
                log_info(f"Reusing up-to-date {binary} for HEAD={cur_head} (0.0s)")
                return True, binary, ""
        except Exception:
            pass

    log_info(f"Building eve-server in {worktree_dir} (cargo build -j 4 --bin eve-server)...")
    t0 = time.time()
    build_env = os.environ.copy()
    if os.path.exists("/opt/homebrew/bin/sccache"):
        build_env.setdefault("RUSTC_WRAPPER", "/opt/homebrew/bin/sccache")
        build_env.setdefault("CARGO_INCREMENTAL", "0")
    proc = subprocess.run(
        ["cargo", "build", "-j", "4", "--bin", "eve-server"],
        cwd=str(worktree_dir),
        env=build_env,
        capture_output=True,
        text=True,
    )
    elapsed = time.time() - t0
    if proc.returncode != 0 or not binary.exists():
        err_tail = "\n".join((proc.stderr or proc.stdout).splitlines()[-40:])
        log_err(f"eve-server build failed in {elapsed:.1f}s:\n{err_tail}")
        return False, binary, err_tail
    try:
        stamp_file.write_text(cur_head, encoding="utf-8")
    except Exception:
        pass
    log_info(f"Built {binary} in {elapsed:.1f}s")
    return True, binary, ""


# ---------------------------------------------------------------------------
# Client Staging (Isolated for Playtester)
# ---------------------------------------------------------------------------

def stage_playtester_client(
    staged_app: Optional[Path],
    inclient_cfg_path: Path,
    hook_port: int,
    system_build_dir: Optional[Path] = None,
    cache_root: Optional[Path] = None,
) -> Path:
    """
    Ensure `~/.cache/evomu/client_build_<build>_playtester` exists (with `bin64/exefile`)
    and has the playtester's `evomu_py_site/sitecustomize.py` + `tools/startup/evomu_playtester.py`
    installed without colliding with `run_journeys`.
    """
    sys_dir = system_build_dir if system_build_dir is not None else SYSTEM_EVE_BUILD_DIR
    croot = cache_root if cache_root is not None else DEFAULT_CACHE_ROOT
    stage_dir = (
        staged_app
        if staged_app is not None
        else resolve_default_staged_app(build_dir=sys_dir, cache_root=croot)
    )
    # Normalize if user passed `.../EVE.app` vs build dir
    if stage_dir.name == "EVE.app" and (stage_dir / "Contents" / "Resources" / "build").exists():
        stage_dir = stage_dir / "Contents" / "Resources" / "build"
    elif stage_dir.name == "EVE.app" and not stage_dir.exists():
        stage_dir = stage_dir.parent

    exefile = stage_dir / "bin64" / "exefile"
    if not exefile.exists():
        src_dir: Optional[Path] = sys_dir
        if not (src_dir / "bin64" / "exefile").exists():
            detected_build = _detect_system_eve_build(sys_dir, cache_root=croot)
            src_dir = _find_cached_client_build(cache_root=croot, prefer_build=detected_build)
            if src_dir is None and SOURCE_STAGED_APP is not None and (SOURCE_STAGED_APP / "bin64" / "exefile").exists():
                src_dir = SOURCE_STAGED_APP
        if src_dir is None or not (src_dir / "bin64" / "exefile").exists():
            raise RuntimeError(
                f"Staged client template not found at {sys_dir} or cache root {croot}."
            )
        log_info(f"Staging isolated playtester client dir: {stage_dir} (from {src_dir})...")
        stage_dir.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["rsync", "-a", f"{src_dir}/", f"{stage_dir}/"],
            check=True,
        )

    site_customize_code = f"""# Auto-generated by EvoMu playtester.py
import os
import sys

sys.dont_write_bytecode = True
HARNESS_DIR = os.environ.get("EVOMU_HARNESS_DIR", "") or {repr(str(HARNESS_DIR))}
if HARNESS_DIR and HARNESS_DIR not in sys.path:
    sys.path.insert(0, HARNESS_DIR)

os.environ.setdefault("EVOMU_INCLIENT_CFG", {repr(str(inclient_cfg_path))})
os.environ.setdefault("EVOMU_PLAYTESTER_CFG", {repr(str(inclient_cfg_path))})
os.environ.setdefault("EVOMU_PLAYTESTER_PORT", {repr(str(hook_port))})

try:
    import evomu_site
    evomu_site.install()
except Exception:
    pass
"""

    startup_code = f"""# Auto-generated by EvoMu playtester.py (/tools=evomu_playtester)
import os
import sys

sys.dont_write_bytecode = True
HARNESS_DIR = os.environ.get("EVOMU_HARNESS_DIR", "") or {repr(str(HARNESS_DIR))}
if HARNESS_DIR and HARNESS_DIR not in sys.path:
    sys.path.insert(0, HARNESS_DIR)

try:
    import evomu_site
    evomu_site.install()
    evomu_site._install_logmodule_hook()
except Exception:
    pass

try:
    import evomu_playtester_hook
    evomu_playtester_hook.schedule_startup()
except Exception:
    pass
"""

    (stage_dir / "common.ini").write_text("[main]\ncryptoPack = Placebo\n", encoding="utf-8")

    for dest_dir in (
        stage_dir / "evomu_py_site",
        stage_dir / "bin64",
        stage_dir / "bin64" / "packages",
        stage_dir,
    ):
        try:
            dest_dir.mkdir(parents=True, exist_ok=True)
            (dest_dir / "sitecustomize.py").write_text(site_customize_code, encoding="utf-8")
            (dest_dir / "usercustomize.py").write_text(site_customize_code, encoding="utf-8")
        except Exception as exc:
            log_warn(f"Could not write sitecustomize in {dest_dir}: {exc}")

    # Install `/tools=evomu_playtester` startup hook in both stage_dir and ~/.cache/evomu/tools/startup
    for startup_target in (
        stage_dir / "tools" / "startup" / "evomu_playtester.py",
        DEFAULT_CACHE_ROOT / "tools" / "startup" / "evomu_playtester.py",
    ):
        try:
            startup_target.parent.mkdir(parents=True, exist_ok=True)
            for init_p in (startup_target.parent.parent / "__init__.py", startup_target.parent / "__init__.py"):
                if not init_p.exists():
                    init_p.write_text("", encoding="utf-8")
            startup_target.write_text(startup_code, encoding="utf-8")
        except Exception as exc:
            log_warn(f"Could not write {startup_target}: {exc}")

    # Pre-seed EVE client `core_public__.yaml` using configured window geometry (full usable screen by default)
    # and matching Retina UI scale so EVE.app is easy to watch on desktop while screenshots stay compact.
    try:
        win_cfg: Dict[str, Any] = {}
        try:
            if inclient_cfg_path.exists():
                loaded_cfg = json.loads(inclient_cfg_path.read_text(encoding="utf-8"))
                if isinstance(loaded_cfg, dict):
                    win_cfg = loaded_cfg
        except Exception:
            win_cfg = {}
        if not win_cfg.get("window_width") or not win_cfg.get("window_height"):
            default_spec = resolve_playtester_window_spec("full")
        else:
            default_spec = {
                "width": int(win_cfg.get("window_width")),
                "height": int(win_cfg.get("window_height")),
                "left": int(win_cfg.get("window_left", 0) if win_cfg.get("window_left") is not None else 0),
                "top": int(win_cfg.get("window_top", 0) if win_cfg.get("window_top") is not None else 0),
                "ui_scale": float(win_cfg.get("window_ui_scale", 1.0) or 1.0),
            }
        w_px = int(default_spec["width"])
        h_px = int(default_spec["height"])
        w_left = int(default_spec["left"])
        w_top = int(default_spec["top"])
        w_ui_scale = float(default_spec["ui_scale"])

        profile_slug = str(stage_dir.resolve()).lower().replace("/", "_") + "_127.0.0.1"
        ccp_settings_dir = (
            Path(os.path.expanduser("~/Library/Application Support/CCP/EVE"))
            / profile_slug
            / "settings"
        )
        ccp_settings_dir.mkdir(parents=True, exist_ok=True)
        ts_ft = 135000000000000000
        core_public_yaml = f"""audio:
  eveampGain: [{ts_ft}, 0.0]
  evevoiceGain: [{ts_ft}, 0.0]
  masterVolume: [{ts_ft}, 0.0]
  suppressTurret: [{ts_ft}, 0]
  uiGain: [{ts_ft}, 0.0]
  useCombatMusic: [{ts_ft}, 0]
  useOldJukeboxOverride: [{ts_ft}, 0]
  worldVolume: [{ts_ft}, 0.0]
device:
  DeviceSettings: [{ts_ft}, null]
  FixedWindow: [{ts_ft}, 0]
  FixedWindowSettings: [{ts_ft}, null]
  FullScreenResolution: [{ts_ft}, null]
  FullScreenSettings:
  - {ts_ft}
  - {{adapter: 0, height: {h_px}, presentInterval: 1, width: {w_px}}}
  UIScaleFullscreen: [{ts_ft}, {w_ui_scale}]
  UIScaleFullscreenSetAutomatically: [{ts_ft}, false]
  UIScaleWindowed: [{ts_ft}, {w_ui_scale}]
  UIScaleWindowedSetAutomatically: [{ts_ft}, false]
  WindowMode: [{ts_ft}, 1]
  WindowedResolution: [{ts_ft}, null]
  WindowedSettings:
  - {ts_ft}
  - {{adapter: 0, height: {h_px}, left: {w_left}, presentInterval: 1, showState: 1, top: {w_top}, width: {w_px}}}
  antiAliasing: [{ts_ft}, 0]
  aoQuality: [{ts_ft}, 0]
  brightness: [{ts_ft}, 1.0]
  charClothSimulation: [{ts_ft}, 0]
  charTextureQuality: [{ts_ft}, 1]
  dofEnabled: [{ts_ft}, false]
  fastCharacterCreation: [{ts_ft}, 1]
  frameGeneration: [{ts_ft}, false]
  lodQuality: [{ts_ft}, 1]
  postProcessingQuality: [{ts_ft}, 0]
  reflectionQuality: [{ts_ft}, 0]
  shaderQuality: [{ts_ft}, 1]
  shadowQuality: [{ts_ft}, 0]
  textureQuality: [{ts_ft}, 1]
  upscalingSetting: [{ts_ft}, 1]
  upscalingTechnique: [{ts_ft}, 0]
  volumetricQuality: [{ts_ft}, 0]
generic: {{}}
ui:
  Insider: [{ts_ft}, true]
  __migrate_existing_font_size_settings_down_one_size__: [{ts_ft}, true]
  clientFontSize: [{ts_ft}, 1]
"""
        (ccp_settings_dir / "core_public__.yaml").write_text(core_public_yaml, encoding="utf-8")
    except Exception as exc:
        log_warn(f"Could not pre-seed windowed core_public__.yaml: {exc}")

    return stage_dir


# ---------------------------------------------------------------------------
# Persistent Memory, Persona & Coverage Frontier Manager
# ---------------------------------------------------------------------------

PLAYTESTER_GM_ADMIN_SECRET = "evomu-scenario-gm-secret"

GM_TESTER_PERSONA: Dict[str, Any] = {
    "id": "gm_tester",
    "title": "GM Admin Tester (Opt-In)",
    "rotate_after_steps": 999999,
    "description": (
        "Explicitly opted-in GM diagnostic persona (`--persona gm_tester`). Permitted to issue "
        "GM slash commands (`/giveisk`, `/createitem`, `/giveskill`, `/advancetime`, `/spawn`) at Step > 0."
    ),
    "goals": [
        "Execute targeted GM diagnostic setup and edge-case verification requested via --persona gm_tester."
    ],
}


class PlaytesterMemory:
    KNOWN_FEATURE_ALIASES = {
        # Legacy initial_frontier IDs
        "ui.fitting.equip_and_online_modules": "ui.fitting.equip_online_and_undock_verify",
        "space.combat.target_lock_and_weapons": "space.combat.target_lock_weapons_and_drones",
        "space.nav.stargate_jump_and_dock": "space.nav.stargate_jump_roundtrip",
        "ui.market.orders_and_history": "ui.market.search_buy_and_cancel",
        "ui.contracts.create_and_search": "ui.contracts.search_and_create",
        "ui.industry.jobs_and_blueprints": "ui.industry.start_complete_and_deliver",
        "ui.agency.and_air_daily_goals": "ui.agency.and_skinr_exploration",
        # Frequent freeform strings observed in live playtester logs
        "skills_window_and_queue": "ui.skills.queue_and_training",
        "train_skill_and_completeskills": "ui.skills.queue_and_training",
        "skill_plans": "ui.skills.skill_plans_quasar",
        "skill_plan_browser": "ui.skills.skill_plans_quasar",
        "plex_store": "ui.store.plex_and_nes_aurum_view",
        "new_eden_store": "ui.store.plex_and_nes_aurum_view",
        "aurum_store": "ui.store.plex_and_nes_aurum_view",
        "plex_vault": "ui.store.plex_and_nes_aurum_view",
        "slash_commands": "ui.slash.remote_command_list_and_errors",
        "slash_errors": "ui.slash.remote_command_list_and_errors",
        "warp_animation": "space.warp.realtime_tunnel_animation",
        "realtime_warp": "space.warp.realtime_tunnel_animation",
        "warp_to": "space.warp.realtime_tunnel_animation",
        "structure_cosmetics": "quasar.cosmetics.structure_states",
        "inventory": "ui.inventory.hangar_and_cargo_ops",
        "inventory_stack_split": "ui.inventory.hangar_and_cargo_ops",
        "fitting": "ui.fitting.equip_online_and_undock_verify",
        "fitting_window": "ui.fitting.equip_online_and_undock_verify",
        "mining": "space.mining.asteroid_lock_and_laser_cycle",
        "mining_laser": "space.mining.asteroid_lock_and_laser_cycle",
        "mining_ledger": "ui.mining.mining_ledger_and_reprocessing",
        "reprocessing": "ui.mining.mining_ledger_and_reprocessing",
        "ore_reprocessing": "ui.mining.mining_ledger_and_reprocessing",
        "venture": "space.mining.venture_and_mining_hold",
        "mining_hold": "space.mining.venture_and_mining_hold",
        "stargate_jump": "space.nav.stargate_jump_roundtrip",
        "jump_gate": "space.nav.stargate_jump_roundtrip",
        "dscan": "space.scanning.dscan_and_probes",
        "probe_scanner": "space.scanning.dscan_and_probes",
        "probe_scanning": "space.scanning.dscan_and_probes",
        "market": "ui.market.search_buy_and_cancel",
        "market_orders": "ui.market.search_buy_and_cancel",
        "regional_market": "ui.market.search_buy_and_cancel",
        "wallet": "ui.wallet.balances_and_journal_tabs",
        "giveisk": "ui.wallet.balances_and_journal_tabs",
        "contracts": "ui.contracts.search_and_create",
        "contract_creation": "ui.contracts.search_and_create",
        "industry": "ui.industry.start_complete_and_deliver",
        "manufacturing": "ui.industry.start_complete_and_deliver",
        "corporation": "ui.corp.window_and_quasar_goals",
        "corp_goals": "ui.corp.window_and_quasar_goals",
        "mail": "ui.comms.mail_and_chat_compose",
        "evemail": "ui.comms.mail_and_chat_compose",
        "chat": "ui.comms.mail_and_chat_compose",
        "fleet": "ui.social.fleet_form_and_broadcasts",
        "fleet_broadcasts": "ui.social.fleet_form_and_broadcasts",
        "agency": "ui.agency.and_skinr_exploration",
        "skinr": "ui.agency.and_skinr_exploration",
    }

    MAX_CUSTOM_FRONTIER_PER_PERSONA = 4
    MAX_TOTAL_FRONTIER = 60

    def __init__(
        self,
        state_dir: Path,
        git_head: Optional[str] = None,
        server_bin_mtime: Optional[float] = None,
        personas_path: Optional[Path] = None,
    ):
        self.state_dir = state_dir
        self.memory_path = state_dir / "memory.json"
        self.personas_path = personas_path or PERSONAS_PATH
        self.personas_cfg = self._load_personas_cfg()
        self.data = self._load_or_init()
        if git_head is not None or server_bin_mtime is not None:
            self.normalize_coverage_frontier(
                git_head=git_head,
                server_bin_mtime=server_bin_mtime,
                save=False,
            )

    def _load_personas_cfg(self) -> Dict[str, Any]:
        if self.personas_path.exists():
            try:
                return json.loads(self.personas_path.read_text(encoding="utf-8"))
            except Exception:
                pass
        return {"personas": [], "initial_coverage_frontier": []}

    @classmethod
    def canonicalize_feature_id(cls, raw_feat: Any, canonical_ids: Optional[set] = None) -> str:
        import re
        s = str(raw_feat or "").strip()
        if not s:
            return ""
        if canonical_ids and s in canonical_ids:
            return s
        low = s.lower()
        if low in cls.KNOWN_FEATURE_ALIASES:
            return cls.KNOWN_FEATURE_ALIASES[low]
        slug = re.sub(r"[^a-z0-9._-]+", "_", low).strip("._-")
        if canonical_ids and slug in canonical_ids:
            return slug
        if slug in cls.KNOWN_FEATURE_ALIASES:
            return cls.KNOWN_FEATURE_ALIASES[slug]
        return s

    @staticmethod
    def _is_valid_custom_slug(feat: str) -> bool:
        import re
        if not feat or len(feat) > 64 or " " in feat or "\n" in feat:
            return False
        return bool(re.match(r"^[a-zA-Z0-9][a-zA-Z0-9._-]{1,63}$", feat))

    def _default_frontier_entries(self) -> List[Dict[str, Any]]:
        raw = (
            self.personas_cfg.get("initial_coverage_frontier")
            or self.personas_cfg.get("initial_frontier")
            or []
        )
        entries: List[Dict[str, Any]] = []
        for item in raw:
            if isinstance(item, dict):
                fid = item.get("feature") or item.get("feature_id")
                if fid:
                    entries.append(
                        {
                            "feature": str(fid),
                            "title": item.get("title", ""),
                            "persona": item.get("persona", ""),
                            "status": item.get("status", "untested"),
                        }
                    )
        return entries

    def normalize_coverage_frontier(
        self,
        git_head: Optional[str] = None,
        server_bin_mtime: Optional[float] = None,
        decay_stale_buggy: bool = False,
        save: bool = True,
    ) -> Dict[str, Any]:
        """
        Curate `coverage_frontier`:
        - Map legacy/alias names onto canonical `initial_frontier` IDs
        - Remove bloated orphan `persona=""` clutter
        - Cap non-canonical custom entries per persona (`MAX_CUSTOM_FRONTIER_PER_PERSONA`)
        - Automatically decay stale `"buggy"` items -> `"untested"` (`needs_retest=True`)
          when `last_server_git_head` or `last_server_bin_mtime` advances (or `decay_stale_buggy=True`).
        """
        default_entries = self._default_frontier_entries()
        canonical_by_id: Dict[str, Dict[str, Any]] = {
            e["feature"]: dict(e) for e in default_entries
        }
        canonical_ids = set(canonical_by_id.keys())
        valid_persona_ids = {
            str(p.get("id")) for p in (self.personas_cfg.get("personas") or []) if p.get("id")
        }

        prev_head = str(self.data.get("last_server_git_head") or "").strip()
        prev_mtime_raw = self.data.get("last_server_bin_mtime")
        prev_mtime = float(prev_mtime_raw) if prev_mtime_raw is not None else None
        head_changed = bool(git_head and prev_head and git_head != prev_head)
        mtime_changed = bool(
            server_bin_mtime is not None
            and prev_mtime is not None
            and abs(float(server_bin_mtime) - prev_mtime) > 1.0
        )
        server_rebuilt = head_changed or mtime_changed
        should_decay_buggy = server_rebuilt or decay_stale_buggy
        decay_reason = (
            f"server_rebuilt(head:{prev_head[:8]}->{str(git_head or '')[:8]})"
            if head_changed
            else (
                "server_binary_updated"
                if mtime_changed
                else ("manual_curation_decay" if decay_stale_buggy else "")
            )
        )

        existing = self.data.get("coverage_frontier") or []
        merged_canonical: Dict[str, Dict[str, Any]] = {
            fid: dict(tmpl) for fid, tmpl in canonical_by_id.items()
        }
        custom_by_key: Dict[str, Dict[str, Any]] = {}
        custom_counts_by_persona: Dict[str, int] = {}
        orphans_pruned = 0
        decayed_count = 0

        for item in existing:
            if not isinstance(item, dict):
                orphans_pruned += 1
                continue
            raw_feat = item.get("feature") or item.get("feature_id")
            if not raw_feat:
                orphans_pruned += 1
                continue
            canon_feat = self.canonicalize_feature_id(raw_feat, canonical_ids=canonical_ids)
            status = str(item.get("status") or "untested")
            if status not in ("untested", "verified", "buggy"):
                status = "untested"

            if canon_feat in merged_canonical:
                cur = merged_canonical[canon_feat]
                cur_status = cur.get("status", "untested")
                v_step = item.get("verified_at_step")
                b_step = item.get("buggy_at_step")
                a_step = item.get("added_at_step")
                if a_step is not None and (cur.get("added_at_step") is None or int(a_step) < int(cur["added_at_step"])):
                    cur["added_at_step"] = int(a_step)
                if status == "verified":
                    if cur_status == "untested" or (
                        cur_status == "buggy"
                        and (v_step or 0) >= (cur.get("buggy_at_step") or 0)
                    ):
                        cur["status"] = "verified"
                        if v_step is not None:
                            cur["verified_at_step"] = int(v_step)
                        cur.pop("buggy_at_step", None)
                elif status == "buggy":
                    if cur_status == "untested" or (
                        cur_status == "verified"
                        and (b_step or 0) > (cur.get("verified_at_step") or 0)
                    ):
                        cur["status"] = "buggy"
                        if b_step is not None:
                            cur["buggy_at_step"] = int(b_step)
                if item.get("needs_retest") and cur.get("status") == "untested":
                    cur["needs_retest"] = True
                if item.get("previously_buggy_at_step") is not None:
                    cur["previously_buggy_at_step"] = item["previously_buggy_at_step"]
            else:
                persona_id = str(item.get("persona") or "").strip()
                if not persona_id or (valid_persona_ids and persona_id not in valid_persona_ids):
                    orphans_pruned += 1
                    continue
                if not self._is_valid_custom_slug(canon_feat):
                    orphans_pruned += 1
                    continue
                if canon_feat in custom_by_key:
                    continue
                if custom_counts_by_persona.get(persona_id, 0) >= self.MAX_CUSTOM_FRONTIER_PER_PERSONA:
                    orphans_pruned += 1
                    continue
                if len(merged_canonical) + len(custom_by_key) >= self.MAX_TOTAL_FRONTIER:
                    orphans_pruned += 1
                    continue
                clean_entry: Dict[str, Any] = {
                    "feature": canon_feat,
                    "persona": persona_id,
                    "status": status,
                }
                if item.get("title"):
                    clean_entry["title"] = str(item["title"])[:140]
                if item.get("added_at_step") is not None:
                    clean_entry["added_at_step"] = item["added_at_step"]
                if item.get("verified_at_step") is not None:
                    clean_entry["verified_at_step"] = item["verified_at_step"]
                if item.get("buggy_at_step") is not None:
                    clean_entry["buggy_at_step"] = item["buggy_at_step"]
                if item.get("needs_retest"):
                    clean_entry["needs_retest"] = True
                custom_by_key[canon_feat] = clean_entry
                custom_counts_by_persona[persona_id] = custom_counts_by_persona.get(persona_id, 0) + 1

        new_frontier = list(merged_canonical.values()) + list(custom_by_key.values())
        if should_decay_buggy:
            for entry in new_frontier:
                if entry.get("status") == "buggy":
                    prev_b = entry.pop("buggy_at_step", None)
                    entry["status"] = "untested"
                    entry["needs_retest"] = True
                    if prev_b is not None:
                        entry["previously_buggy_at_step"] = prev_b
                    if decay_reason:
                        entry["decayed_reason"] = decay_reason
                    decayed_count += 1

        self.data["coverage_frontier"] = new_frontier
        if server_rebuilt:
            self.data["filed_incident_fingerprints"] = []
            self.data["filed_manual_signatures"] = []
            self.data["verification_attempts"] = {}
        if git_head:
            self.data["last_server_git_head"] = str(git_head)
        if server_bin_mtime is not None:
            self.data["last_server_bin_mtime"] = float(server_bin_mtime)
        if save:
            self.save()
        return {
            "before_count": len(existing),
            "after_count": len(new_frontier),
            "canonical_count": len(merged_canonical),
            "custom_count": len(custom_by_key),
            "orphans_pruned": orphans_pruned,
            "buggy_decayed": decayed_count,
            "server_rebuilt": server_rebuilt,
        }

    def _load_or_init(self) -> Dict[str, Any]:
        personas = self.personas_cfg.get("personas") or []
        default_frontier = self._default_frontier_entries()
        if self.memory_path.exists():
            try:
                loaded = json.loads(self.memory_path.read_text(encoding="utf-8"))
                # Merge any newly added initial_frontier items into existing coverage_frontier
                # and refresh canonical metadata (persona/title/acceptance) on known entries.
                frontier = loaded.setdefault("coverage_frontier", [])
                default_by_feature = {entry["feature"]: entry for entry in default_frontier}
                for item in frontier:
                    if isinstance(item, dict) and item.get("feature") in default_by_feature:
                        tmpl = default_by_feature[item["feature"]]
                        for key in ("persona", "title", "acceptance"):
                            if key in tmpl:
                                item[key] = tmpl[key]
                known = {
                    item.get("feature")
                    for item in frontier
                    if isinstance(item, dict) and item.get("feature")
                }
                for entry in default_frontier:
                    if entry["feature"] not in known:
                        frontier.append(dict(entry))
                # Refresh active_persona definition from personas_cfg if matching id exists
                active_id = (loaded.get("active_persona") or {}).get("id")
                for idx, p in enumerate(personas):
                    if p.get("id") == active_id:
                        loaded["active_persona_index"] = idx
                        loaded["active_persona"] = p
                        break
                return loaded
            except Exception as exc:
                log_warn(f"Failed to parse {self.memory_path}, re-initializing: {exc}")

        first_persona = personas[0] if personas else {
            "id": "open_explorer",
            "title": "Open-Ended Capsuleer Explorer",
            "rotate_after_steps": 45,
            "goals": ["Explore all station windows, skill plans, PLEX store, undock, and warp."],
        }
        first_goals = first_persona.get("goals") or (
            [first_persona["description"]] if first_persona.get("description") else ["Explore the game"]
        )
        return {
            "created_at": utc_now_iso(),
            "updated_at": utc_now_iso(),
            "total_steps": 0,
            "server_reloads": 0,
            "last_server_git_head": "",
            "last_server_bin_mtime": None,
            "active_persona_index": 0,
            "active_persona_steps": 0,
            "active_persona": first_persona,
            "current_goal": first_goals[0],
            "working_memory": [
                "Fresh playtester memory initialized. Priority: execute complete persona checkpoints with verified wallet, inventory, session, and UI deltas."
            ],
            "long_term_notes": [],
            "coverage_frontier": default_frontier,
            "recent_actions": [],
            "filed_incident_fingerprints": [],
            "filed_manual_signatures": [],
            "last_run_summary": None,
        }

    def save(self) -> None:
        self.data["updated_at"] = utc_now_iso()
        self.state_dir.mkdir(parents=True, exist_ok=True)
        tmp = self.memory_path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(self.data, indent=2), encoding="utf-8")
        tmp.replace(self.memory_path)

    def maybe_rotate_persona(self, force_id: Optional[str] = None) -> bool:
        personas = self.personas_cfg.get("personas") or []
        if not personas:
            return False
        prev_id = str((self.data.get("active_persona") or {}).get("id") or "")
        if force_id:
            if force_id == GM_TESTER_PERSONA["id"]:
                self.data["active_persona_index"] = -1
                self.data["active_persona"] = dict(GM_TESTER_PERSONA)
                self.data["active_persona_steps"] = 0
                goals = GM_TESTER_PERSONA.get("goals") or ["Explore EVE Online systems"]
                self.data["current_goal"] = goals[0]
                self.save()
                return force_id != prev_id
            for idx, p in enumerate(personas):
                if p.get("id") == force_id:
                    self.data["active_persona_index"] = idx
                    self.data["active_persona"] = p
                    self.data["active_persona_steps"] = 0
                    goals = p.get("goals") or ["Explore EVE Online systems"]
                    self.data["current_goal"] = goals[0]
                    self.save()
                    return force_id != prev_id

        active = self.data.get("active_persona") or personas[0]
        if str(active.get("id") or "") == GM_TESTER_PERSONA["id"]:
            # Do not auto-rotate out of an explicit opt-in gm_tester session mid-run.
            return False
        limit = int(active.get("rotate_after_steps", 45))
        if int(self.data.get("active_persona_steps", 0)) >= limit:
            idx = (int(self.data.get("active_persona_index", 0)) + 1) % len(personas)
            next_p = personas[idx]
            self.data["active_persona_index"] = idx
            self.data["active_persona"] = next_p
            self.data["active_persona_steps"] = 0
            goals = next_p.get("goals") or ["Explore EVE Online systems"]
            self.data["current_goal"] = goals[0]
            log_info(f"Rotated persona -> [{next_p.get('id')}] {next_p.get('title')}")
            self.save()
            return str(next_p.get("id") or "") != prev_id
        return False

    def apply_planner_decision(
        self,
        step_num: int,
        plan_decision: Dict[str, Any],
    ) -> None:
        """Apply strategic memory and coverage-frontier updates from the Parent Planner."""
        if plan_decision.get("current_goal"):
            self.data["current_goal"] = str(plan_decision["current_goal"])[:300]

        mem_upd = plan_decision.get("memory_update") or {}
        if isinstance(mem_upd.get("working_memory"), list) and mem_upd["working_memory"]:
            self.data["working_memory"] = [str(x)[:300] for x in mem_upd["working_memory"][-12:]]

        note = str(mem_upd.get("long_term_note_to_append") or "").strip()
        if note:
            notes = self.data.setdefault("long_term_notes", [])
            notes.append(f"[step {step_num}] {note[:400]}")
            self.data["long_term_notes"] = notes[-60:]

        verified_issues, failed_issues, touched_issues = extract_verification_updates(plan_decision)
        if verified_issues or failed_issues or touched_issues:
            attempts_map = self.data.setdefault("verification_attempts", {})
            if not isinstance(attempts_map, dict):
                attempts_map = {}
                self.data["verification_attempts"] = attempts_map
            for issue_num in sorted(set(touched_issues) | set(verified_issues) | set(failed_issues)):
                key = str(issue_num)
                rec = attempts_map.get(key)
                if not isinstance(rec, dict):
                    rec = {"attempts": 0}
                rec["attempts"] = int(rec.get("attempts", 0)) + 1
                rec["last_step"] = step_num
                attempts_map[key] = rec
            for issue_num in verified_issues:
                key = str(issue_num)
                rec = attempts_map.setdefault(key, {"attempts": 1})
                rec["verified_fixed"] = True
                rec.pop("fix_failed", None)
                rec["verified_at_step"] = step_num
            for issue_num in failed_issues:
                key = str(issue_num)
                rec = attempts_map.setdefault(key, {"attempts": 1})
                rec["fix_failed"] = True
                rec.pop("verified_fixed", None)
                rec["failed_at_step"] = step_num

        frontier = self.data.setdefault("coverage_frontier", [])
        canonical_ids = {e["feature"] for e in self._default_frontier_entries()}
        by_feat = {item.get("feature"): item for item in frontier if isinstance(item, dict) and item.get("feature")}
        active_persona_id = str((self.data.get("active_persona") or {}).get("id") or "regression_and_critical_ui")
        custom_for_persona = sum(
            1
            for item in frontier
            if isinstance(item, dict)
            and item.get("persona") == active_persona_id
            and item.get("feature") not in canonical_ids
        )

        buggy_raw = mem_upd.get("mark_buggy_features") or []
        buggy_set = {
            self.canonicalize_feature_id(f, canonical_ids=canonical_ids)
            for f in buggy_raw
            if f
        }

        for raw_feat in mem_upd.get("mark_tested_features") or []:
            feat = self.canonicalize_feature_id(raw_feat, canonical_ids=canonical_ids)
            if not feat or feat in buggy_set:
                continue
            if feat in by_feat:
                prev_b = by_feat[feat].pop("buggy_at_step", None)
                if prev_b is not None:
                    by_feat[feat]["previously_buggy_at_step"] = prev_b
                by_feat[feat].pop("needs_retest", None)
                by_feat[feat].pop("decayed_reason", None)
                by_feat[feat]["status"] = "verified"
                by_feat[feat]["verified_at_step"] = step_num
            elif (
                len(frontier) < self.MAX_TOTAL_FRONTIER
                and custom_for_persona < self.MAX_CUSTOM_FRONTIER_PER_PERSONA
                and self._is_valid_custom_slug(feat)
            ):
                entry = {
                    "feature": feat,
                    "persona": active_persona_id,
                    "status": "verified",
                    "verified_at_step": step_num,
                }
                frontier.append(entry)
                by_feat[feat] = entry
                custom_for_persona += 1

        for raw_feat in buggy_raw:
            feat = self.canonicalize_feature_id(raw_feat, canonical_ids=canonical_ids)
            if not feat:
                continue
            if feat in by_feat:
                by_feat[feat]["status"] = "buggy"
                by_feat[feat]["buggy_at_step"] = step_num
                by_feat[feat].pop("needs_retest", None)
            elif (
                len(frontier) < self.MAX_TOTAL_FRONTIER
                and custom_for_persona < self.MAX_CUSTOM_FRONTIER_PER_PERSONA
                and self._is_valid_custom_slug(feat)
            ):
                entry = {
                    "feature": feat,
                    "persona": active_persona_id,
                    "status": "buggy",
                    "buggy_at_step": step_num,
                }
                frontier.append(entry)
                by_feat[feat] = entry
                custom_for_persona += 1

        for raw_feat in mem_upd.get("new_frontier_features") or []:
            feat = self.canonicalize_feature_id(raw_feat, canonical_ids=canonical_ids)
            if not feat or feat in by_feat:
                continue
            if (
                len(frontier) < self.MAX_TOTAL_FRONTIER
                and custom_for_persona < self.MAX_CUSTOM_FRONTIER_PER_PERSONA
                and self._is_valid_custom_slug(feat)
            ):
                entry = {
                    "feature": feat,
                    "persona": active_persona_id,
                    "status": "untested",
                    "added_at_step": step_num,
                }
                frontier.append(entry)
                by_feat[feat] = entry
                custom_for_persona += 1

        self.save()

    def apply_agent_decision(
        self,
        step_num: int,
        decision: Dict[str, Any],
        action_outcome: Dict[str, Any],
    ) -> None:
        self.data["total_steps"] = int(self.data.get("total_steps", 0)) + 1
        self.data["active_persona_steps"] = int(self.data.get("active_persona_steps", 0)) + 1

        # In legacy (non-hybrid) mode, apply memory_update on every step; in hybrid mode,
        # `apply_planner_decision` already applied the Planner's memory_update when the Planner ran.
        if decision.get("_mode") != "hybrid":
            self.apply_planner_decision(step_num, decision)
        elif decision.get("current_goal"):
            self.data["current_goal"] = str(decision["current_goal"])[:300]

        act = decision.get("action") or {}
        recent = self.data.setdefault("recent_actions", [])
        recent_entry = {
            "step": step_num,
            "ts": utc_now_iso(),
            "action_type": act.get("type"),
            "target": act.get("target") or act.get("command") or "",
            "expected_ui_and_state_change": str(
                act.get("expected_ui_and_state_change") or ""
            )[:240],
            "observation": str(decision.get("observation") or "")[:200],
            "ok": bool(action_outcome.get("action_ok", True)),
            "action_error": action_outcome.get("action_error"),
            "state_diff": action_outcome.get("state_diff"),
            "guard_hit_count": int(
                action_outcome.get(
                    "guard_hit_count",
                    len(action_outcome.get("guard_hits") or []),
                )
                or 0
            ),
            "guard_hits": list(action_outcome.get("guard_hits") or [])[:25],
            "user_error_exemption_count": int(
                action_outcome.get(
                    "user_error_exemption_count",
                    len(action_outcome.get("user_error_exemptions") or []),
                )
                or 0
            ),
            "user_error_exemptions": list(action_outcome.get("user_error_exemptions") or [])[:25],
        }
        if action_outcome.get("slash_result"):
            recent_entry["slash_result"] = str(action_outcome["slash_result"])[:2000]
        recent.append(recent_entry)
        self.data["recent_actions"] = recent[-20:]
        self.maybe_rotate_persona()
        self.save()

    def update_run_summary(self, summary: Dict[str, Any]) -> None:
        self.data["last_run_summary"] = dict(summary or {})
        self.save()


# ---------------------------------------------------------------------------
# `agy` CLI Reasoning Invoker with Model Fallback Chain
# ---------------------------------------------------------------------------

def _resolve_agy_model_args(model_name: str) -> List[str]:
    """Translate user model alias (e.g. `hydrogen-high`, `gemini-hydrogen-low`, `gemini-4-carbon-low`) into `agy` CLI flags."""
    m = model_name.strip()
    alias_map = {
        "hydrogen-high": "hydrogen-l-tf-high",
        "hydrogen-medium": "hydrogen-l-tf-medium",
        "hydrogen-low": "hydrogen-l-tf-medium",
        "hydrogen-xhigh": "hydrogen-l-tf-xhigh",
        "hydrogen-l-tf-low": "hydrogen-l-tf-medium",
        "gemini-hydrogen-low": "hydrogen-l-tf-medium",
        "gemini-4-hydrogen-low": "hydrogen-l-tf-medium",
        "gemini-4-hydrogen-high": "hydrogen-l-tf-high",
        "gemini-4-hydrogen-medium": "hydrogen-l-tf-medium",
        "gemini-4-hydrogen-xhigh": "hydrogen-l-tf-xhigh",
        "gemini-pro-next": "Gemini Pro Next",
        "gemini-launch-candidate-low": "gemini-launch-candidate-low",
        "gemini-launch-candidate-high": "gemini-launch-candidate-high",
        "gemini-4-carbon-medium": "Gemini 4 Carbon (Medium)",
        "gemini-4-carbon-low": "Gemini 4 Carbon (Low)",
        "gemini-4-carbon-high": "Gemini 4 Carbon (High)",
        "opus-5.5-low": "Claude Opus 5.5 (Low)",
        "opus-5.5-medium": "Claude Opus 5.5 (Medium)",
        "opus-5.5-high": "Claude Opus 5.5 (High)",
    }
    return ["--model", alias_map.get(m, m)]


def _is_session_boundary_flight_recorder(
    last_flight_recorder: Optional[Dict[str, Any]],
) -> bool:
    """Return True when no prior action from the current live client session exists to verify."""
    if not last_flight_recorder or not isinstance(last_flight_recorder, dict):
        return True
    return bool(
        last_flight_recorder.get("server_hot_reloaded_to")
        or last_flight_recorder.get("client_restarted_for_desync")
        or last_flight_recorder.get("client_restarted_for_downtime")
        or last_flight_recorder.get("client_restarted_unexpectedly")
    )


def _is_persona_cleanup_false_desync(
    last_flight_recorder: Optional[Dict[str, Any]],
    bug_report: Optional[Dict[str, Any]],
    prev_verification: Optional[Dict[str, Any]],
    has_hard_errors: bool,
) -> Optional[str]:
    """
    Detect when a UI-state desync claim is explaining away utility windows that the
    playtester harness intentionally closed during persona rotation cleanup before
    capturing the next step's snapshot/screenshot.
    """
    if has_hard_errors or not isinstance(last_flight_recorder, dict):
        return None
    prc = last_flight_recorder.get("persona_rotation_cleanup")
    if not isinstance(prc, dict):
        return None
    closed_wins = [str(w).strip().lower() for w in (prc.get("closed_windows") or []) if str(w).strip()]
    sec_view = str(prc.get("closed_secondary_view") or "").strip().lower()
    if sec_view:
        closed_wins.append(sec_view)
    deleted_contracts = [str(cid).strip() for cid in (prc.get("deleted_contracts") or []) if str(cid).strip()]
    stacked_targets = [str(tgt).strip().lower() for tgt in (prc.get("stacked_targets") or []) if str(tgt).strip()]
    if not closed_wins and not deleted_contracts and not stacked_targets:
        return None

    br = bug_report if isinstance(bug_report, dict) else {}
    pav = prev_verification if isinstance(prev_verification, dict) else {}
    cat = str(br.get("category") or "").strip().lower()
    if br.get("should_file") and cat not in (
        "",
        "none",
        "ui_state_desync",
        "blank_or_stuck_window",
        "broken_game_mechanic",
        "agent_diagnosed_bug",
    ):
        return None

    combined = " ".join(
        str(x or "")
        for x in (
            br.get("title"),
            br.get("actual_behavior"),
            pav.get("discrepancy_summary"),
        )
    ).lower()
    matched = [w for w in closed_wins if w in combined]
    if matched:
        return f"window(s) {matched} were closed by harness persona_rotation_cleanup"

    opened_prev = [
        str(w).strip().lower()
        for w in ((last_flight_recorder.get("state_diff") or {}).get("windows_opened") or [])
        if str(w).strip()
    ]
    overlap = [w for w in closed_wins if w in opened_prev]
    if overlap and any(
        tok in combined for tok in ("disappear", "vanish", "absent", "missing", "closed", "transient")
    ):
        return f"opened window(s) {overlap} were closed by harness persona_rotation_cleanup"
    if deleted_contracts and any(cid in combined for cid in deleted_contracts):
        return f"contract(s) {deleted_contracts} were deleted by harness persona_rotation_cleanup"
    if stacked_targets and any(
        tok in combined for tok in ("stack", "stacked", "merged")
    ) and any(
        tok in combined for tok in ("disappear", "vanish", "missing", "reduced", "fewer", "hangar", "cargo")
    ):
        return f"inventory target(s) {stacked_targets} were stacked by harness persona_rotation_cleanup"
    return None


def _normalize_verification_view(ui_snapshot: Optional[Dict[str, Any]] = None, active_view: str = "") -> str:
    raw = str(active_view or (ui_snapshot or {}).get("active_view") or "").strip().lower()
    if raw in ("hangar", "station", "docked", "station_hangar", "character_selection"):
        return "docked"
    if raw in ("inflight", "space", "in_space", "tactical"):
        return "space"
    sess = (ui_snapshot or {}).get("session") or {}
    if sess.get("stationid") or sess.get("structureid"):
        return "docked"
    space_info = (ui_snapshot or {}).get("space") or {}
    if space_info.get("in_space"):
        return "space"
    return raw or "any"


def load_pending_verifications(state_dir: Path) -> List[Dict[str, Any]]:
    """Load unresolved post-cutover verification candidates from `<state_dir>/pending_verifications.json`."""
    qpath = Path(state_dir) / "pending_verifications.json"
    if not qpath.exists():
        return []
    try:
        raw = json.loads(qpath.read_text(encoding="utf-8"))
    except Exception:
        return []
    entries = raw.get("items") if isinstance(raw, dict) else raw
    if not isinstance(entries, list):
        return []
    items: List[Dict[str, Any]] = []
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        issue_raw = entry.get("issue")
        try:
            issue_num = int(issue_raw)
        except Exception:
            continue
        status = str(entry.get("status") or "pending").strip().lower()
        if status not in ("", "pending", "pending_verification"):
            continue
        if entry.get("blocked_by"):
            continue
        item = dict(entry)
        item["issue"] = issue_num
        item["cluster"] = str(entry.get("cluster") or "general").strip() or "general"
        item["view"] = str(entry.get("view") or "any").strip().lower() or "any"
        item["title"] = str(entry.get("title") or "")[:140]
        item["repro_hint"] = str(entry.get("repro_hint") or "")[:220]
        items.append(item)
    return items


def extract_verification_updates(plan_decision: Dict[str, Any]) -> Tuple[List[int], List[int], List[int]]:
    """Extract VERIFIED_FIXED #<N>, FIX_FAILED #<N>, and verification-touched #<N> refs from a decision."""
    if not isinstance(plan_decision, dict):
        return [], [], []
    mem_upd = plan_decision.get("memory_update") or {}
    note = str(mem_upd.get("long_term_note_to_append") or "")
    wm = " ".join(str(x) for x in (mem_upd.get("working_memory") or []) if x)
    obs = str(plan_decision.get("observation") or "")
    reason = str(plan_decision.get("reasoning") or "")
    goal = str(plan_decision.get("current_goal") or "")
    plan_goal = str((plan_decision.get("delegated_plan") or {}).get("goal") or "")

    status_blob = "\n".join([note, wm, obs, reason])
    verified = sorted(
        {
            int(num)
            for span in re.findall(
                r"\bVERIFIED_FIXED\b((?:\s*(?:,|&|/|\band\b)?\s*#\d+)+)",
                status_blob,
                flags=re.IGNORECASE,
            )
            for num in re.findall(r"#(\d+)", span)
        }
    )
    failed = sorted(
        {
            int(num)
            for span in re.findall(
                r"\b(?:FIX_FAILED|FAILED_VERIFICATION)\b((?:\s*(?:,|&|/|\band\b)?\s*#\d+)+)",
                status_blob,
                flags=re.IGNORECASE,
            )
            for num in re.findall(r"#(\d+)", span)
        }
    )
    goal_blob = "\n".join([goal, plan_goal, note])
    touched = sorted(
        {
            int(m)
            for m in re.findall(
                r"(?:verif(?:y|ying|ied|ication)|retest(?:ing)?)\b[^#\n]{0,40}#(\d+)\b|#(\d+)\b",
                goal_blob,
                flags=re.IGNORECASE,
            )
            for m in (m if isinstance(m, tuple) else (m,))
            if m and int(m) >= 100
        }
    )
    return verified, failed, touched


def select_verification_focus(
    items: List[Dict[str, Any]],
    active_view: str = "any",
    verification_attempts: Optional[Dict[str, Any]] = None,
    limit: int = 5,
) -> Tuple[List[Dict[str, Any]], Dict[str, Any]]:
    """
    Pick up to `limit` pending verification items grouped by active view + feature cluster.
    Items marked verified_fixed/fix_failed are hidden; items with >= 2 attempts yield priority to fresh items.
    """
    attempts_map = verification_attempts if isinstance(verification_attempts, dict) else {}
    cur_view = _normalize_verification_view(active_view=active_view)
    eligible: List[Dict[str, Any]] = []
    skipped_resolved = 0

    for item in items:
        issue_num = int(item["issue"])
        rec = attempts_map.get(str(issue_num)) or {}
        if rec.get("verified_fixed") or rec.get("fix_failed"):
            skipped_resolved += 1
            continue
        item_view = _normalize_verification_view(active_view=str(item.get("view") or "any"))
        view_match = (cur_view == "any") or (item_view == "any") or (item_view == cur_view)
        attempts = int(rec.get("attempts", 0))
        annotated = dict(item)
        annotated["attempts"] = attempts
        annotated["_view_match"] = view_match
        annotated["_stalled"] = attempts >= 2
        eligible.append(annotated)

    if not eligible:
        return [], {
            "total_pending": 0,
            "view_matched_count": 0,
            "focus_cluster": "",
            "skipped_resolved": skipped_resolved,
        }

    # Prefer view-compatible items first; only cross-view if none match current view.
    pool = [it for it in eligible if it["_view_match"]] or eligible
    fresh_pool = [it for it in pool if not it["_stalled"]]
    cluster_source = fresh_pool or pool

    cluster_scores: Dict[str, Tuple[int, int, int]] = {}
    for it in cluster_source:
        cl = it["cluster"]
        prev_cnt, prev_att, prev_min = cluster_scores.get(cl, (0, 0, 10**9))
        cluster_scores[cl] = (prev_cnt + 1, prev_att + it["attempts"], min(prev_min, it["issue"]))

    best_cluster = max(
        cluster_scores.keys(),
        key=lambda cl: (cluster_scores[cl][0], -cluster_scores[cl][1], -cluster_scores[cl][2]),
    )

    ordered = sorted(
        pool,
        key=lambda it: (
            it["_stalled"],
            it["cluster"] != best_cluster,
            it["attempts"],
            it["issue"],
        ),
    )
    selected = []
    for it in ordered[: max(1, limit)]:
        clean = {k: v for k, v in it.items() if not k.startswith("_")}
        selected.append(clean)

    return selected, {
        "total_pending": len(eligible),
        "view_matched_count": sum(1 for it in eligible if it["_view_match"]),
        "focus_cluster": best_cluster,
        "skipped_resolved": skipped_resolved,
    }


def _format_compact_widgets(raw_widgets: List[Any], limit: int = 95) -> str:
    """Format clickable widgets as compact single-line records to shrink prompt size."""
    lines: List[str] = []
    for w in (raw_widgets or [])[:limit]:
        if not isinstance(w, dict):
            continue
        name = str(w.get("name") or "")
        path = str(w.get("path") or "")
        label = str(w.get("label") or w.get("text") or "")
        if len(label) > 48:
            label = label[:45] + "..."
        x = w.get("x")
        y = w.get("y")
        coords = f"@({x},{y})" if (x is not None and y is not None) else ""
        ed = " [editable]" if w.get("editable") else ""
        lbl = f' label="{label}"' if label and label != name else ""
        lines.append(f"  {name or '?'} | {path}{lbl} {coords}{ed}".rstrip())
    return "\n".join(lines)


def _prompt_allows_gm_actions(step_num: int, persona_id: str) -> bool:
    return int(step_num) <= 0 or str(persona_id or "").strip().lower() == GM_TESTER_PERSONA["id"]


def _prompt_action_types_doc(allow_gm: bool, include_widget_actions: bool = False) -> str:
    widget_head = ["click_widget"] if include_widget_actions else []
    widget_tail = ["type_text", "click_xy", "drag_widget"] if include_widget_actions else []
    base_actions = [
        "open_neocom", "close_window", "undock", "dock", "jump_gate", "warp_to",
        "approach", "orbit", "keep_at_range", "lock_target", "activate_module",
    ]
    gm_actions = (
        [
            "train_skill", "complete_skills", "advance_upwell_timer",
            "complete_industry_jobs", "advance_server_time", "clear_timers", "slash_command",
        ]
        if allow_gm
        else []
    )
    end_actions = (
        ["open_plex_store"]
        + (["open_structure_cosmetics"] if allow_gm else [])
        + ["press_key"]
        + widget_tail
        + ["wait"]
    )
    return ", ".join(f"`{a}`" for a in (widget_head + base_actions + gm_actions + end_actions))


def build_reasoning_prompt(
    step_num: int,
    screenshot_path: Optional[Path],
    burst_paths: List[Path],
    ui_snapshot: Dict[str, Any],
    last_flight_recorder: Dict[str, Any],
    memory: PlaytesterMemory,
    git_head: str,
    compact_mode: bool = False,
) -> str:
    mem = memory.data
    persona = mem.get("active_persona") or {}
    persona_id = str(persona.get("id") or "")
    allow_gm_prompt = _prompt_allows_gm_actions(step_num, persona_id)
    all_frontier = [x for x in (mem.get("coverage_frontier") or []) if isinstance(x, dict)]
    persona_pending = [
        x for x in all_frontier
        if x.get("persona") == persona_id and x.get("status") in ("untested", "buggy")
    ]
    other_pending = [
        x for x in all_frontier
        if x.get("persona") != persona_id and x.get("status") in ("untested", "buggy")
    ]
    untested = (persona_pending + other_pending)[:12]
    verified = [
        x.get("feature") for x in all_frontier
        if x.get("status") == "verified"
    ][-18:]
    pending_verif_items = load_pending_verifications(memory.memory_path.parent)
    cur_norm_view = _normalize_verification_view(ui_snapshot=ui_snapshot)
    verif_focus, verif_stats = select_verification_focus(
        pending_verif_items,
        active_view=cur_norm_view,
        verification_attempts=mem.get("verification_attempts") or {},
        limit=5,
    )
    verification_section = ""
    if verif_focus:
        verification_section = (
            f"\n### Post-Cutover Fix Verification Queue "
            f"({verif_stats['total_pending']} pending total, {verif_stats['view_matched_count']} matching view={cur_norm_view}, cluster={verif_stats['focus_cluster']})\n"
            "Prioritize batching these view-compatible issue verifications before unrelated exploration. "
            "Cite `#<N>` in `current_goal` and record `VERIFIED_FIXED #<N>` (with evidence) or `FIX_FAILED #<N>` (with `bug_report.should_file=true`) in `long_term_note_to_append`:\n"
            f"{json.dumps(verif_focus, indent=2)}\n"
        )
    recent_actions = (mem.get("recent_actions") or [])[-8:]
    if not _is_session_boundary_flight_recorder(last_flight_recorder):
        prev_action_entry = recent_actions[-1] if recent_actions else None
    else:
        prev_action_entry = {
            "note": (
                "Fresh client session just started, restarted, or hot-reloaded — do NOT verify the "
                "prior session's last action against this fresh startup window layout. "
                "Set previous_action_verification.expectation_met = true and bug_report.should_file = false."
            )
        }
    compact_recent_lines = ",\n  ".join(
        json.dumps(
            {
                "step": r.get("step"),
                "action_type": r.get("action_type"),
                "target": r.get("target"),
                "ok": r.get("ok"),
                "action_error": r.get("action_error"),
                "ui_changed": (r.get("state_diff") or {}).get("ui_changed"),
                "view_changed": (r.get("state_diff") or {}).get("view_changed"),
            },
            separators=(",", ":"),
        )
        for r in recent_actions
    )

    burst_section = ""
    if burst_paths:
        burst_list = "\n".join(f"  - {p}" for p in burst_paths)
        burst_section = (
            f"\nMulti-frame animation burst screenshots from previous action:\n{burst_list}\n"
            f"(Inspect these if you just triggered a warp, undock, or dock transition to verify visual animation continuity!)\n"
        )

    compact_snap = {
        k: v
        for k, v in (ui_snapshot or {}).items()
        if k not in ("ui", "open_windows", "modals", "context_menu")
    }
    ui_sub = (ui_snapshot or {}).get("ui") or {}
    compact_snap["open_windows"] = ui_sub.get("open_windows") or (ui_snapshot or {}).get("open_windows") or []
    compact_snap["modals"] = ui_sub.get("modals") or (ui_snapshot or {}).get("modals") or []
    compact_snap["recent_modal_messages"] = ui_sub.get("recent_modal_messages") or []
    compact_snap["context_menu"] = ui_sub.get("context_menu") or (ui_snapshot or {}).get("context_menu") or []
    raw_widgets = ui_sub.get("clickable_widgets") or []
    compact_widgets_block = _format_compact_widgets(raw_widgets, limit=135)

    prc_info = (
        last_flight_recorder.get("persona_rotation_cleanup")
        if isinstance(last_flight_recorder, dict)
        else None
    )
    prc_banner = ""
    if isinstance(prc_info, dict) and (
        prc_info.get("closed_windows")
        or prc_info.get("closed_secondary_view")
        or prc_info.get("deleted_contracts")
        or prc_info.get("stacked_targets")
    ):
        prc_parts = []
        if prc_info.get("closed_windows") or prc_info.get("closed_secondary_view"):
            win_part = f"closed utility window(s) `{prc_info.get('closed_windows')}`"
            if prc_info.get("closed_secondary_view"):
                win_part += f" (secondary view `{prc_info.get('closed_secondary_view')}`)"
            prc_parts.append(win_part)
        if prc_info.get("deleted_contracts"):
            prc_parts.append(
                f"deleted stale own Outstanding contract(s) `{prc_info.get('deleted_contracts')}`"
            )
        if prc_info.get("stacked_targets"):
            prc_parts.append(
                f"merged duplicate item stacks in `{prc_info.get('stacked_targets')}`"
            )
        prc_banner = (
            f"\n- **⚠️ Harness Persona-Rotation Cleanup Notice:** Immediately before capturing Step {step_num}, "
            f"the playtester performed automated hygiene cleanup ({'; '.join(prc_parts)}). "
            "Do NOT report those closed windows, deleted stale contracts, or merged item stacks as a bug or `ui_state_desync`.\n"
        )
    inv_snap = compact_snap.get("inventory_state") if isinstance(compact_snap.get("inventory_state"), dict) else {}
    inv_qf = inv_snap.get("quick_filter")
    inv_af = inv_snap.get("active_filters")
    inv_filter_banner = ""
    if inv_qf or inv_af:
        inv_filter_banner = (
            f"- **⚠️ Active Inventory Filter Warning:** Inventory window currently has an active filter "
            f"(`quick_filter={inv_qf!r}`, `active_filters={inv_af!r}`), which hides non-matching items in the UI! "
            f"Do NOT conclude items are missing or `ui_state_desync` before clearing the filter "
            f"(`type_text` with `clear: true` on `quickFilterInputBox` or clicking clear filters).\n"
        )

    has_screen = bool(screenshot_path and Path(screenshot_path).exists())
    screen_line = (
        f"- **Current Live Window Screenshot:** `{screenshot_path}`\n"
        f"  (Only read `{screenshot_path}` if you need visual confirmation of 3D space graphics or warp effects; the complete `uicore` scene graph is already provided below, so do NOT waste tool calls reading `ui_state.json`.)"
        if has_screen
        else "- **Live Screenshot:** Omitted on clean fast step (scene graph below is authoritative; screenshots capture automatically on anomalies/Planner transitions)."
    )

    prompt_flight_recorder = PlaytesterBugOracle.partition_flight_recorder_exceptions(
        last_flight_recorder
    )

    role_summary_line = (
        "Your character has full **GM / QA / Admin role access** (`0x7FFFFFFFFFFFFFFF`) so you can both interact with the UI like a player AND invoke GM timer-acceleration commands to test multi-hour gameplay loops (skill training, Upwell structure timers, industry jobs, cooldowns) immediately without waiting real-time."
        if allow_gm_prompt
        else "You are controlling a **standard player character (`role = 0`)**. Interact through real game UI actions (Market, Inventory, Fitting, Industry, Skills window, Agency); setup/GM shortcuts and timer-acceleration actions are restricted to Step `0` or `--persona gm_tester`."
    )
    skill_section_doc = (
        "1. **Skill Training & Skill Queue (`skills`, `skill_plans`, `train_skill`, `complete_skills`):**\n"
        "   - Adding/starting a skill (`train_skill` or clicking Train in UI) MUST emit `OnNewSkillQueueSaved` / `OnSkillQueueChanged` and increase `skills_state.queue_len > 0` in the live snapshot and Skill Queue UI bar.\n"
        "   - Running `complete_skills` (`/completeskills all`) MUST immediately finish queued skills without waiting real-time, emit `OnSkillTrained` + `OnServerSkillsChanged` + `OnNewSkillQueueSaved`, increase `skills_state.total_sp`, and update the trained skill level in the Character Sheet / Skills window live without requiring a relog.\n"
        "   - If `queue_len` or `total_sp` or the Skills window UI fails to update after `train_skill` or `complete_skills`, set `previous_action_verification.expectation_met = false` and file a `ui_state_desync` bug!"
        if allow_gm_prompt
        else "1. **Skill Training & Skill Queue (`skills`, `skill_plans`):**\n"
        "   - Adding/starting a skill by clicking Train in UI MUST emit `OnNewSkillQueueSaved` / `OnSkillQueueChanged` and increase `skills_state.queue_len > 0` in the live snapshot and Skill Queue UI bar.\n"
        "   - If `queue_len` or the Skills window UI fails to update after queueing a skill in UI, set `previous_action_verification.expectation_met = false` and file a `ui_state_desync` bug!"
    )
    quasar_windows_doc = (
        "7. **Quasar Windows (`open_plex_store`, `skill_plans`, `open_structure_cosmetics`, `agency`, `corporation`):**"
        if allow_gm_prompt
        else "7. **Quasar Windows (`open_plex_store`, `skill_plans`, `agency`, `corporation`):**"
    )
    catalog_skill_line = (
        '7. `train_skill`: Inject and queue a skill (`target` = skill typeID, e.g. `"3386"`, `"3385"`, `"3300"`, `"3327"`).'
        if allow_gm_prompt
        else '7. **Skill Training (`open_neocom` `"skills"` / `click_widget`):** Open the Skills window and manage or queue skills via player UI controls.'
    )
    catalog_probe_line = (
        "9. `open_plex_store` / `open_structure_cosmetics`: Direct probes for `#529` and `#527`."
        if allow_gm_prompt
        else "9. `open_plex_store`: Direct probe for `#529`."
    )

    prompt = f"""You are the Autonomous EvoMu Live Client Playtester playing the real macOS EVE Online V24.01 client (`EVE.app`) connected to `eve-server` (commit `{git_head}`).
{role_summary_line}

### Step {step_num} Perception Bundle
{screen_line}
{burst_section}
### Active Exploration Persona & Goal
- **Persona:** `[{persona.get('id', 'explorer')}]` {persona.get('title', 'Explorer')} (step {mem.get('active_persona_steps', 0)}/{persona.get('rotate_after_steps', 25)} before rotation)
- **Persona Directives:** {json.dumps(persona.get('goals') or ([persona['description']] if persona.get('description') else []))}
- **Current Goal:** {mem.get('current_goal')}

### Persistent Working Memory
{json.dumps(mem.get('working_memory', []), indent=2)}

### Long-Term Session Notes (Most Recent)
{json.dumps((mem.get('long_term_notes') or [])[-6 if compact_mode else -12:], separators=(',', ':') if compact_mode else None, indent=None if compact_mode else 2)}

### Coverage Frontier (Priority Untested / Buggy Features to Probe)
{json.dumps(untested[:8] if compact_mode else untested, separators=(',', ':') if compact_mode else None, indent=None if compact_mode else 2)}

### Already Verified Features
{f"{len(verified)} verified (recent: {json.dumps(verified[-8:], separators=(',', ':'))})" if compact_mode else json.dumps(verified)}
{verification_section}
### Previous Action Expectation vs. Actual State Diff (CRITICAL FOR UI-STATE DESYNC DETECTION)
{prc_banner}- **Previous Action & Expected Change:**
```json
{json.dumps(prev_action_entry, separators=(',', ':') if compact_mode else None, indent=None if compact_mode else 2)}
```
- **Previous Action Flight-Recorder, `state_diff`, & `scatters_observed`:**
```json
{json.dumps(prompt_flight_recorder, separators=(',', ':') if compact_mode else None, indent=None if compact_mode else 2)}
```

### Recent Actions History (Avoid repeating stuck loops!)
[
  {compact_recent_lines}
]

### Live `uicore` Scene-Graph & Session Snapshot
{inv_filter_banner}```json
{json.dumps(compact_snap, separators=(',', ':') if compact_mode else None, indent=None if compact_mode else 2)}
```
### Clickable Widgets ({len(raw_widgets)} total, showing top {min(len(raw_widgets), 135)})
```text
{compact_widgets_block}
```

### Expected EVE Online Mechanics & UI-State Reference Contract
Use these ground-truth EVE Online V24.01 expectations to judge whether the game and UI updated properly after your previous action (even when no Python exception was raised!):
{skill_section_doc}
2. **Upwell Structures & Timers (`advance_upwell_timer` / `/upwelltimer`):**
   - **Key Upwell typeIDs:** Citadels: `Astrahus` (`35832`, Quantum Core `56201`), `Fortizar` (`35833`, Quantum Core `56202`), `Keepstar` (`35834`, Quantum Core `56203`); Engineering Complexes: `Raitaru` (`35825`, Quantum Core `56204`), `Azbel` (`35826`, Quantum Core `56205`), `Sotiyo` (`35827`, Quantum Core `56206`); Refineries: `Athanor` (`35835`, Quantum Core `56207`), `Tatara` (`35836`, Quantum Core `56208`); FLEX: `Ansiblex Jump Gate` (`35841`), `Pharolux Cyno Beacon` (`35840`), `Tenebrex Cyno Jammer` (`37534`); Standup Service Modules: `Standup Cloning Center I` (`35894`), `Standup Market Hub I` (`35892`), `Standup Manufacturing Plant I` (`35878`), `Standup Reprocessing Facility I` (`35899`); Fuel Blocks: `Helium Fuel Block` (`4051`), `Nitrogen Fuel Block` (`4247`); Skills: `Anchoring` (`3443`), `Corporation Management` (`3731`).
   - **Anchoring Prerequisites:** Pilot MUST belong to a player corporation (with Director/Station Manager/CEO role), be in space `> 1,000 km` from stations/stargates/structures and `> 5 km` from asteroids/NPCs (note: `/tr` only accepts `[me|<char_id>] <location_id>`, never `pos x y z`; to get clean deep space, warp toward a distant planet/stargate and press Stop Ship `CTRL+SPACE` mid-warp), and have the structure item in active ship cargo. Right-click structure in cargo -> `Launch for Corporation` (`structureDeployment.Anchor`).
   - **Timer Lifecycle & Quantum Core:** Upwell structures transition through `AnchorVulnerable (15m) -> Anchoring (24h) -> NoCore -> Onlining (15m) -> ShieldVulnerable (Online)`. Running `advance_upwell_timer` (`/upwelltimer`) once advances `AnchorVulnerable -> Anchoring`; running it a second time advances `Anchoring -> NoCore`. Once in `NoCore`, dock inside the structure (`structureDocking.Dock`) and deposit the matching Quantum Core (`56201` for Astrahus, `56204` for Raitaru) into the Core Room (`flag 180` / `StructureDeedBay`), then run `advance_upwell_timer` (`/upwelltimer`) to advance `Onlining -> ShieldVulnerable`.
   - **Tethering, Docking, Fitting, Control & Unanchoring:** An online (`ShieldVulnerable`) structure tethers friendly ships in space (`effects.StructureRepair`), supports docking (`session.structure_id` set, `structureHangarView`), allows fitting Standup service modules (`flag 164..171`) + Fuel Blocks (`flag 133`/`172`) when docked or controlling, supports `Take Control` (`structureControl.TakeControl` / `BoardStructure`) and `Relinquish Control` (`ReleaseControl`), and supports `Decommission` (`structureDeployment.Unanchor` / `CancelUnanchor`).
3. **Industry Jobs (`complete_industry_jobs` / `/completeindustry`):**
   - After starting an industry job, running `complete_industry_jobs` (`/completeindustry`) fast-forwards `start_unix` so the job is immediately ready for delivery in the Industry window.
4. **Ship Fitting & Inventory (`fitting`, `inventory`, `/fit`, `/createitem`):**
   - Fitting a module or creating an item MUST emit `OnItemChange` and update the Fitting window slots / Ship HUD module rack / Hangar inventory list live. (Note: `/createitem <ship_typeID> 1` for ships `categoryID=6` such as `603` Merlin or `649` Tayra spawns an already-assembled ship directly in **`StationShips` (`Ship hangar`)**, NOT `ItemHangar`; both `StationShips` and `ItemHangar` only show the top visible rows without scrolling, so always filter via `quickFilterInputBox` before assuming an item/ship is absent, and clear `quickFilterInputBox` when finished. Reuse existing hangar items and click `Stack All` when cluttered before spawning/buying duplicates. Also, double-clicking a module/charge opens Show Info — fit modules via drag-and-drop to a slot, right-click -> `Fit to Active Ship`, or `/fit me <typeID>`.)
5. **Market, Contracts & Wallet (`market`, `contracts`, `wallet`, `/giveisk`):**
   - Buying an item or running `/giveisk` MUST emit `OnAccountChange`, update `wallet_state.isk_balance` in `state_diff`, and deliver purchased items to the hangar.
   - **V24 Regional Market widget naming:** `buyParent/buyscroll` is the TOP **Sellers** table (double-click or right-click -> `Buy This` to buy from a seller), whereas `sellParent/sellscroll` is the BOTTOM **Buyers** table (right-clicking your own buy order shows `Modify Order` / `Cancel Order` / `Place Buy Order`, and double-clicking a buy order does NOT open a Buy dialog).
   - **V24 Contracts & L0 1-Contract Cap:** Creating a **Public Item Exchange** contract without requesting return items MUST set `Price >= 1,000 ISK` (`< 1,000 ISK` is rejected by V24 `CreateContract` with `UserError`). Starter character `Contracting` is L0 (`1` max Outstanding contract), so after verifying a created test contract in `My Contracts`, click `Delete` to keep the slot free for subsequent runs.
6. **Navigation & Real-Time Warp Animation (`undock`, `dock`, `warp_to`, `approach`, `orbit`):**
   - `undock` MUST transition `active_view` from `hangar` to `inflight` (`space.in_space == true`) and populate `space.nearby_balls`.
   - `warp_to` MUST enter `ball.mode == 3` (`WARP`), activate `effects.Warping` tunnel FX (`warp_telemetry.saw_mode3 == true` and `saw_fx_warping == true`), and move the ship to the destination.
   - `approach` / `orbit` MUST transition `space.ego.mode` to `1` (`FOLLOW`) or `4` (`ORBIT`) and increase `speed_mps > 0`.
{quasar_windows_doc}
   - Opening these windows MUST populate their UI content without `TimeoutException`, `SkillPlanConnectionError`, or permanently blank/spinning panels.
8. **Mining, Asteroid Harvesting, Mining Ledger & Ore Reprocessing (`mining`, `miningledger`, `reprocessing`, `mininghold`):**
   - Key mining typeIDs: Skills `Mining` (`3386`), `Astrogeology` (`3410`), `Mining Frigate` (`32918`), `Reprocessing` (`3385`); Modules `Miner I` (`483`), `Civilian Miner` (`3651`), `Miner II` (`482`); Ship `Venture` (`32880`); Asteroid ores `Veldspar` (`1230`), `Scordite` (`1228`), `Pyroxeres` (`1224`), `Plagioclase` (`18`); Reprocessed mineral `Tritanium` (`34`).
   - Running `/spawn 1230` in space MUST spawn a Veldspar asteroid ball (`categoryID 25`) in `space.nearby_balls` (`DoBallsAdded`).
   - Locking an asteroid within range (`lock_target`) and activating a fitted online mining laser (`activate_module` / `ModuleButton_483` / `ModuleButton_3651`) MUST emit `OnGodmaShipEffect` (`effects.Laser`), deposit mined ore units into the ship's Cargo Hold or Mining Hold (`OnItemChange`), and record a personal/corporate entry in the **Mining Ledger** (`open_neocom` target `"miningledger"`).
   - While docked at a station, opening the **Reprocessing Plant** (`open_neocom` target `"reprocessing"`), quoting ore (`reprocessingSvc.GetQuotes`), and reprocessing (`reprocessingSvc.Reprocess`) MUST consume the ore stack and deliver minerals (`Tritanium` `34`) via `OnItemChange`.
9. **Titans, Doomsday Superweapons, Phenomena Generators & Capital Jump Drive (`titans`, `doomsday`, `superWeapon`):**
   - **Key Titan typeIDs:** Hulls: `Avatar` (`11567`, Amarr Titan skill `3347`), `Erebus` (`671`, Gallente Titan skill `3346`), `Leviathan` (`3764`, Caldari Titan skill `3344`), `Ragnarok` (`23773`, Minmatar Titan skill `3345`); Required Skills: `Capital Ships` (`20533`), `Doomsday Operation` (`24563`), `Jump Portal Generation` (`24562`), `Jump Drive Operation` (`3433`), `Jump Fuel Conservation` (`21610`); Doomsday Modules (group `588`): `'Judgment' Electromagnetic Doomsday` (`24550`, fits Avatar), `'Aurora Ominae' Thermal Doomsday` (`24554`, fits Erebus), `'Oblivion' Kinetic Doomsday` (`24552`, fits Leviathan), `'Gjallarhorn' Explosive Doomsday` (`23674`, fits Ragnarok); Phenomena Generators: `Amarr Phenomena Generator` (`43658`), `Gallente Phenomena Generator` (`43662`); Bridge/Clone Modules: `Jump Portal Generator I` (`23953`), `Clone Vat Bay I` (`23735`); Fuel: `Helium Isotopes` (`16274`), `Oxygen Isotopes` (`17887`), `Nitrogen Isotopes` (`17888`), `Hydrogen Isotopes` (`17889`), `Strontium Clathrates` (`16275`); Capital Targets: `Revelation` (`19720`), `Moros` (`19724`), `Phoenix` (`19726`), `Naglfar` (`19722`).
   - **Security & Target Rules:** Titans (`group 30`) CANNOT be boarded in highsec (`CannotBoardSupercapitalInHighSec`) and Doomsdays CANNOT be activated in highsec or lowsec (`CannotActivateDoomsdayInHighSec` / `CannotActivateLanceInLowSec`) or while tethered (`CantActivateWhileTethered`). Always move to a nullsec system first (`/moveme 30004759` `1DQ1-A` or `/moveme 30000197` `HED-GP`).
   - **Single-Target Doomsday & Phenomena Generator Activation:** Single-target Doomsdays (`24550`/`24554`/`24552`/`23674`) can ONLY target locked Capital/Supercapital ships (`group 30/485/513/547/659/883/902/1538/4594`) or Structures (`category 65`) within `< 200 km` (spawn a capital target via `/spawn 19720` Revelation Dreadnought, set safety to Red via `crimewatch.SetSafetyLevel(0)`, `lock_target`, and `activate_module`). Firing consumes isotopes, emits `OnGodmaShipEffect` (`superWeapon`) + `OnDamageMessages`, and applies a 30s self-immobilization cycle and 300s jump/dock/tether lock (`clear_timers` / `advance_server_time 310` clears cooldowns). **IMPORTANT for Phenomena Generators (`43658`/`43662`, group `1815`):** Phenomena Generators auto-repeat (`repeat=1000`) and immobilize the ship (`0.0 m/s`), warp-scramble (`WarpScrambled`), and block docking (`CantDockWhileModuleActive`) for as long as the module stays green/cycling — `advance_server_time` and `clear_timers` do NOT stop an active module cycle, so you MUST explicitly deactivate `ModuleButton_43658`/`43662` (`activate_module` toggle -> `dogmaIM.Deactivate`) before moving, warping, docking, or leaving the Titan. Also Titans (`group 30`) CANNOT dock at NPC stations (`60003760` -> `DockingRequestDenied`), only Keepstars (`35834`), so board a subcapital ship before `/moveme 30000142` to Jita!

### Action Catalog & Instructions
Choose your next action in the `action` object according to the required JSON schema:
1. `click_widget`: Click a widget by `target` (matching `name`, `path`, or `label` from `open_windows`, `context_menu`, `modals`, or `clickable_widgets`) or fallback coordinates `x`, `y`. Supports `"right_click": true` (context menus) and `"double_click": true` (legacy `OnDblClick` entries AND `eveui` cards/`on_double_click`).
2. `open_neocom`: Open a Neocom window by `target` (`character_sheet`, `skills`, `skill_plans`, `inventory`, `cargohold`, `mininghold`, `mineralhold`, `fitting`, `market`, `miningledger`, `reprocessing`, `moonmining`, `plex_store`, `new_eden_store`, `agency`, `corporation`, `fleet`, `industry`, `contracts`, `mail`, `wallet`, `map`, `dscan`, `probe`, `assets`, `notepad`).
3. `close_window`: Close a window or modal by `target` (`window_id` or `name`, or empty to close modals).
4. `undock` / `dock`: Undock into space or dock at a station (`target` optional station item_id).
5. `jump_gate`: Cross-system Stargate jump (`target` = gate `item_id` or gate/destination system substring such as `"Stargate (Perimeter)"` or `"Perimeter"` from `space.nearby_balls`). Automatically warps to the gate if >150 km, approaches <2.5 km, clears session timer, jumps, and awaits `state_diff.solar_system_changed`.
6. `warp_to` / `approach` / `orbit` / `keep_at_range` / `lock_target` / `activate_module`: In-space navigation, combat, probe launcher/probes (`Core Probe Launcher I` `17938` + `Core Scanner Probe I` `30013`), and mining laser commands.
{catalog_skill_line}
8. **GM Timer-Acceleration & Setup-Only Slash Policy (`role = 0` for player personas):**
   - Exploratory player personas run with **`role = 0` (non-GM)**. `slash_command` / `run_slash` (including `/createitem`, `/giveisk`, `/giveskill`, `/spawn`, `/moveme`) are **STRICTLY RESTRICTED** to Step `0` bootstrap or the explicit `--persona gm_tester` mode and are **REJECTED** by the host action validator at Step `> 0` for all normal player personas! Test game flows via real UI actions (Market, Inventory, Fitting, Industry, Quests/Agency) rather than GM slash shortcuts.
   - `complete_skills` / `advance_upwell_timer` / `complete_industry_jobs` / `advance_server_time` / `clear_timers` are ONLY valid in setup/time-control steps or `--persona gm_tester` mode.
{catalog_probe_line}
10. `type_text` / `press_key` / `click_xy` / `drag_widget` / `wait`: Direct UI manipulation.
   - `type_text`: Targets widgets marked `"editable": true` in `clickable_widgets` (or focused field). Append `\n` at the end of `command`/`text` (e.g. `"Tritanium\n"`) to trigger Return/search submission automatically!
   - `press_key`: Supports `RETURN`/`ENTER`, `TAB`, `SHIFT+TAB`, `ESCAPE`, `CTRL+SPACE` (Stop Ship), `F1`–`F10`, and shortcuts (`ALT+P` Probe Scanner, `ALT+D` D-Scan, `ALT+W` Wallet, `ALT+N` Notepad, `ALT+F` Fitting, `ALT+C` Cargo, `ALT+R` Market, `ALT+G` Corp, `ALT+T` Assets, `ALT+M` Journal, `ALT+X` Skills).
   - `drag_widget`: Drag source widget `target` (at `x`, `y`) to drop coordinates `dest_x`, `dest_y` (from the destination widget's `@(x,y)` in `clickable_widgets`, e.g. `ShipHangar` or a fitting slot, or desired window position). Always supply non-zero `dest_x` and `dest_y` (or `action.dest_target` in direct batch plans).

### Bug Detection Rule (Exceptions AND Silent UI-State Desyncs!)
1. First fill out `previous_action_verification`:
   - Compare the previous action's `expected_ui_and_state_change` against `state_diff`, `scatters_observed`, `ui_snapshot`, and `screen.png`.
   - **Important:** If the previous action returned `ok: false` / non-empty `action_error` (e.g. unsupported `press_key` combo, widget not found, or missing locked target precondition), that is a harness/input rejection rather than a game bug — set `previous_action_verification.expectation_met = true` and `bug_report.should_file = false` unless there was a `log_exceptions`, `macho_exceptions`, `quasar_errors`, or server error.
   - **Important:** If `Previous Action Flight-Recorder` contains `persona_rotation_cleanup` (`closed_windows` / `closed_secondary_view` / `deleted_contracts` / `stacked_targets`), the harness intentionally closed those utility windows, deleted stale own Outstanding contracts, or merged duplicate item stacks at the start of this step after a persona rotation BEFORE capturing `ui_snapshot` and `screen.png` — do NOT mark their disappearance or stack-count change as `ui_state_desync` or `buggy` (`expectation_met = true` and `bug_report.should_file = false` unless there were hard errors or an unrelated mismatch).
   - **Important:** If `Previous Action Flight-Recorder` contains `suppressed_log_exceptions` (with `log_exceptions: []`), a previously-reported client Python exception fired again on the last step and was already captured earlier in this session — if the window/content remained blank or stuck behind a spinner, set `bug_report.should_file = false` unless a distinct new server/RPC error or unrelated bug occurred, and route around the failing UI element.
   - **Important:** If `Previous Action Flight-Recorder` contains `benign_macho_exceptions` (with `macho_exceptions: []`), those are expected/benign `UserError` responses already filtered by the bug oracle (such as `InventoryNotAccessible` for stale UI-cached ships at another location, `ShipInWarp`, `DockingApproach`, skill prerequisites, etc.) — do NOT set `bug_report.should_file = true` for them.
   - **Important:** If `Previous Action Flight-Recorder` contains `benign_log_exceptions` (with `log_exceptions: []`), those are known benign CCP V24 client-only UI layout races or expected UI rejections already filtered by the bug oracle (such as `minimap.py:_ResizeSlider` `MapSlider.minimumWidth` or `marketbase.py:_OnResize` `NoneType.max` that recover cleanly on layout completion) — do NOT set `bug_report.should_file = true` or `expectation_met = false` solely for them unless a real non-benign error or broken UI occurred.
   - Otherwise, if the previous action succeeded (`ok: true`) and should have changed the UI or game state (e.g., opened a window, switched a tab, queued/completed a skill, updated wallet/inventory, changed ship movement mode, or closed a modal) and the UI state **did not update properly**, set `previous_action_verification.expectation_met = false` and describe the exact discrepancy in `discrepancy_summary`.
2. Set `bug_report.should_file = true` whenever ANY of the following holds:
   - `previous_action_verification.expectation_met == false` (use `category: "ui_state_desync"` or `"broken_game_mechanic"` or `"blank_or_stuck_window"`).
   - `modals` contains an error/timeout dialog (`SkillPlanConnectionError`, `TimeoutException`, `CustomError`, etc.).
   - `Previous Action Flight-Recorder` contains `log_exceptions`, `quasar_errors`, `macho_exceptions`, or failed warp animation (`saw_mode3 == false` and `saw_fx_warping == false` after `warp_to`).
"""
    return prompt


def build_planner_prompt(
    step_num: int,
    screenshot_path: Optional[Path],
    burst_paths: List[Path],
    ui_snapshot: Dict[str, Any],
    last_flight_recorder: Dict[str, Any],
    memory: PlaytesterMemory,
    git_head: str,
    escalation_context: Optional[Dict[str, Any]] = None,
    compact_mode: bool = False,
    use_compact_json_contract: bool = False,
) -> str:
    """
    Build the Strategic Parent Planner prompt (`hydrogen-l-tf-medium` routine / `hydrogen-l-tf-high` diagnostic).
    The Planner audits the previous action/batch, files any bug reports (`bug_report`),
    updates persistent working memory & coverage frontier (`memory_update`), and formulates
    a structured multi-step `delegated_plan` (1..10 steps) for direct Python execution / Fast Executor (`hydrogen-l-tf-medium`).
    """
    base_prompt = build_reasoning_prompt(
        step_num=step_num,
        screenshot_path=screenshot_path,
        burst_paths=burst_paths,
        ui_snapshot=ui_snapshot,
        last_flight_recorder=last_flight_recorder,
        memory=memory,
        git_head=git_head,
        compact_mode=compact_mode,
    )
    escalation_section = ""
    if escalation_context:
        escalation_section = f"""
### ⚠️ Executor / Runner Escalation Alert
The batch runner / Executor (`{DEFAULT_EXECUTOR_MODEL_CHAIN[0]}`) paused and escalated control back to you (the Parent Planner) during the previous batch:
```json
{json.dumps(escalation_context, indent=2)}
```
Carefully inspect the `escalation_reason` / `assertion_failure`, `previous_action_verification`, `last_flight_recorder`, and the live `ui_snapshot` above:
1. If the escalation reflects a real game bug, exception, or `ui_state_desync` where an action succeeded (`ok: true`) without updating UI/server state properly, set `previous_action_verification.expectation_met = false` and `bug_report.should_file = true` with full diagnostic details!
2. If the escalation was caused by a missing precondition, wrong widget target, or unexpected modal/window state, record the lesson in `memory_update.working_memory` and formulate a clean corrective `delegated_plan`.
"""

    compact_contract_section = ""
    if use_compact_json_contract:
        compact_contract_section = """
### Compact JSON Output Contract (CRITICAL: Reply with ONE raw JSON object ONLY — no markdown fences, no tool calls, no extra prose)
Keep `observation`, `reasoning`, `horizon_rationale` to 1 concise sentence each. Omit null/false defaults (`x`, `y`, `dest_x`, `dest_y`, `right_click`, `double_click`, `capture_screenshot`) and unused assertion keys (`assertions` is completely optional!).
Exact top-level JSON structure:
{
  "observation": "<1 sentence>",
  "previous_action_verification": {"expectation_met": true, "discrepancy_summary": ""},
  "reasoning": "<1 sentence>",
  "current_goal": "<goal citing #N if verifying fix>",
  "delegated_plan": {
    "plan_goal": "<objective>",
    "horizon_rationale": "<1 short phrase>",
    "max_batch_steps": 6,
    "steps": [
      {"step_number": 1, "exec_mode": "direct", "action": {"type": "...", "target": "...", "wait_ms": 250}, "assertions": {"expect_window_open": "..."}},
      {"step_number": 2, "exec_mode": "llm", "action_type_hint": "click_widget", "target_or_command_hint": "...", "instruction": "..."}
    ],
    "success_criteria": "<condition>",
    "abort_conditions": "Error modal"
  },
  "memory_update": {"working_memory": ["..."], "long_term_note_to_append": "", "mark_tested_features": [], "mark_buggy_features": [], "new_frontier_features": []},
  "bug_report": {"should_file": false}
}
When reporting a real bug (`bug_report.should_file = true`), include `"category"`, `"title"`, `"severity"`, `"subsystem"`, `"expected_behavior"`, `"actual_behavior"`, `"reproduction_steps"` inside `"bug_report"`.
"""

    planner_persona_id = str(((memory.data.get("active_persona") or {}).get("id")) or "")
    allow_gm_prompt = _prompt_allows_gm_actions(step_num, planner_persona_id)
    planner_direct_actions_doc = _prompt_action_types_doc(allow_gm_prompt, include_widget_actions=False)
    planner_batch_example_doc = (
        "`clear_timers` + slash setup -> `open_neocom` -> `click_widget` -> `type_text` -> `close_window`"
        if allow_gm_prompt
        else "`open_neocom` -> `click_widget` -> `type_text` -> `close_window`"
    )
    planner_instructions = f"""{escalation_section}
### Hybrid Planner-Executor Architecture (YOUR ROLE: STRATEGIC PARENT PLANNER)
You are the **Strategic Parent Planner**. You audit prior outcomes and formulate `delegated_plan` (`1..10` concrete steps) executed at high speed by the **Deterministic Python Batch Runner (`~0.2s`/step)** with warm Fast Executor (`{DEFAULT_EXECUTOR_MODEL_CHAIN[0]}`) fallback for dynamic UI:
1. **Audit & Diagnose (`previous_action_verification` + `bug_report`):**
   - You own all bug diagnosis and reporting (`bug_report`) and persistent memory/frontier updates (`memory_update`).
   - Verify whether the last executed action (or escalated batch step) met EVE Online V24.01 expectations.
2. **Delegate an Executable Tactical Plan (`delegated_plan`):**
   - **Step 1 executes immediately from your `delegated_plan.steps[0]` WITHOUT another LLM call** when `exec_mode = "direct"`, and every subsequent `"direct"` step executes at `~0.2s` in Python unless a selector misses or an anomaly triggers escalation.
   - **Choose `exec_mode` per step:**
     - `"direct"` (`~75-85%` of steps): Provide `action_type_hint` AND a concrete `"action"` object (`type`, `target`/`command`, optional `wait_ms` `250`-`500`) for {planner_direct_actions_doc}, AND for `click_widget`/`type_text` when the widget name/path/label is known from `clickable_widgets` or predictable (`context_menu_*`, `yes_dialog_button`, `ok_dialog_button`, `cancelBtn`, `quickFilterInputBox`, `OverviewTab_*`), or `drag_widget` only when both source `target` AND drop `dest_x`/`dest_y` (or `action.dest_target`) are known upfront (use `"llm"` if drop coordinates are not yet visible).
     - `"llm"`: Provide `action_type_hint` (`"click_widget"`, etc.); use `"llm"` ONLY when a previous step in the same batch will open an unfamiliar window/dialog whose child widget names are not known yet (`action` may be omitted).
   - **Optional `"assertions"` (`expect_window_open`, `expect_window_closed`, `expect_modal_absent`, `expect_context_menu_item`, `expect_ball_present`, `expect_session_field`):** All keys are OPTIONAL — ONLY include `expect_modal_absent: true` when no confirmation popup (`Delete`, `Yes/No`) is expected! Keep `capture_screenshot: false` (default) on routine `"direct"` steps.
   - **Choose `max_batch_steps` (`6–10` steps strongly preferred for routine multi-step workflows!)** and explain briefly in `horizon_rationale`:
     - Use **`1–2` steps** only for high-risk/bug-prone probes or escalation diagnosis.
     - Use **`6–10` steps** for normal gameplay sequences (e.g. {planner_batch_example_doc}, or `undock` -> `warp_to` -> `wait` -> `lock_target` -> `activate_module` -> `dock`). Never emit `wait(10000)` for session-change timers (`wait(10000)` if unavoidable).
     - Note: Only `{max(1, int((memory.data.get('active_persona') or {}).get('rotate_after_steps', 25)) - int(memory.data.get('active_persona_steps', 0)))}` steps remain before persona rotation.
   - **Keep `memory_update` coverage frontier clean & annotate Fix Verifications:**
     - Only reference canonical `feature_id`s from Coverage Frontier (or short snake_case slugs when genuinely new). Never emit long prose sentences as feature IDs.
     - When a previously buggy feature succeeds cleanly with verified deltas, include it in `mark_tested_features` (not `mark_buggy_features`) so it transitions from `buggy` -> `verified`.
     - When verifying items from `Post-Cutover Fix Verification Queue`, cite `#<N>` in `current_goal` / `long_term_note_to_append` and record `VERIFIED_FIXED #<N>` (with evidence) or `FIX_FAILED #<N>` (plus `bug_report.should_file = true`) in `long_term_note_to_append`.
{compact_contract_section}"""
    return base_prompt + "\n" + planner_instructions


def build_executor_prompt(
    step_num: int,
    batch_id: int,
    batch_step_idx: int,
    delegated_plan: Dict[str, Any],
    screenshot_path: Optional[Path],
    burst_paths: List[Path],
    ui_snapshot: Dict[str, Any],
    last_flight_recorder: Dict[str, Any],
    memory: PlaytesterMemory,
    batch_history: List[Dict[str, Any]],
    use_compact_json_contract: bool = False,
) -> str:
    """
    Build the Fast Tactical Executor prompt (`{DEFAULT_EXECUTOR_MODEL_CHAIN[0]}`).
    Used on dynamic `'llm'` steps or direct-step selector-miss self-correction.
    """
    max_steps = int(delegated_plan.get("max_batch_steps") or len(delegated_plan.get("steps") or []) or 1)
    planned_steps = delegated_plan.get("steps") or []
    current_planned_step = (
        planned_steps[batch_step_idx - 1]
        if 0 <= batch_step_idx - 1 < len(planned_steps)
        else (planned_steps[-1] if planned_steps else {})
    )

    compact_snap = {
        k: v
        for k, v in (ui_snapshot or {}).items()
        if k not in ("ui", "open_windows", "modals", "context_menu")
    }
    ui_sub = (ui_snapshot or {}).get("ui") or {}
    compact_snap["open_windows"] = ui_sub.get("open_windows") or (ui_snapshot or {}).get("open_windows") or []
    compact_snap["modals"] = ui_sub.get("modals") or (ui_snapshot or {}).get("modals") or []
    compact_snap["recent_modal_messages"] = ui_sub.get("recent_modal_messages") or []
    compact_snap["context_menu"] = ui_sub.get("context_menu") or (ui_snapshot or {}).get("context_menu") or []
    raw_widgets = ui_sub.get("clickable_widgets") or []
    compact_widgets_block = _format_compact_widgets(raw_widgets, limit=110)

    burst_section = ""
    if burst_paths:
        burst_list = "\n".join(f"  - {p}" for p in burst_paths)
        burst_section = f"\nMulti-frame burst screenshots from previous action:\n{burst_list}\n"

    has_screen = bool(screenshot_path and Path(screenshot_path).exists())
    exec_screen_line = (
        f"- **Live Screenshot:** `{screenshot_path}` (Only read `{screenshot_path}` if you need visual confirmation of 3D space graphics; the full `uicore` scene graph is already below — do NOT read `ui_state.json`.)"
        if has_screen and not use_compact_json_contract
        else "- **Live Screenshot:** Omitted on fast Executor step (full `uicore` scene graph is provided below — do NOT call file tools)."
    )
    compact_contract = ""
    if use_compact_json_contract:
        compact_contract = f"""
### Compact JSON Output Contract (CRITICAL: Reply with ONE raw JSON object ONLY — zero markdown fences, zero tool calls, 1-sentence observation/reasoning)
```json
{{"observation":"<1 sentence>","previous_action_verification":{{"expectation_met":true,"discrepancy_summary":""}},"reasoning":"<1 sentence>","batch_step_number":{batch_step_idx},"batch_status":"continue|completed|escalate","escalation_reason":"","action":{{"type":"...","target":"...","x":123,"y":456,"wait_ms":250,"expected_ui_and_state_change":"..."}}}}
```
Omit null/false action fields (`command`, `dest_x`, `dest_y`, `right_click`, `double_click` unless needed)."""
    _plan_summary = (
        {
            "goal": delegated_plan.get("plan_goal") or delegated_plan.get("goal") or "",
            "success_criteria": delegated_plan.get("success_criteria") or "",
            "abort_conditions": delegated_plan.get("abort_conditions") or "",
        }
        if use_compact_json_contract
        else delegated_plan
    )
    _sep = (",", ":") if use_compact_json_contract else None
    _ind = None if use_compact_json_contract else 2

    exec_persona_id = str(((memory.data.get("active_persona") or {}).get("id")) or "")
    exec_actions_doc = _prompt_action_types_doc(
        _prompt_allows_gm_actions(step_num, exec_persona_id),
        include_widget_actions=True,
    )
    if batch_step_idx <= 1:
        return f"""You are the **Fast Tactical Playtester Executor** (`{DEFAULT_EXECUTOR_MODEL_CHAIN[0]}`) operating the live macOS EVE Online V24.01 client (`EVE.app`).
You are executing **Batch #{batch_id} (Step {batch_step_idx} of {max_steps})** delegated to you by the Strategic Parent Planner (`{DEFAULT_MODEL_CHAIN[0]}`).

### Delegated Plan from Parent Planner
```json
{json.dumps(_plan_summary, separators=_sep, indent=_ind)}
```

### Target Step to Execute Now (Batch Step {batch_step_idx} of {max_steps})
```json
{json.dumps(current_planned_step, separators=_sep, indent=_ind)}
```

### Step {step_num} Live Perception
{exec_screen_line}
{burst_section}
### Live `uicore` Scene-Graph & Session Snapshot
```json
{json.dumps(compact_snap, separators=_sep, indent=_ind)}
```
### Clickable Widgets ({len(raw_widgets)} total, showing top {min(len(raw_widgets), 110)})
```text
{compact_widgets_block}
```

### Executor Instructions & Self-Correction / Escalation Rules
1. **Batch Step 1 Verification:** Since the Parent Planner just audited the pre-batch state, set `previous_action_verification.expectation_met = true` and `discrepancy_summary = ""` on Batch Step 1 unless an unexpected error modal is already blocking the screen.
2. **Map the Planned Step to a Concrete Action:**
   - Match `target_or_command_hint` (or `action.target`) from Batch Step {batch_step_idx} against live `clickable_widgets`, `open_windows`, `context_menu`, `modals`, or `space.nearby_balls`.
   - Valid `action.type` values: {exec_actions_doc}.
   - Note: `type_text` MUST set `target` to the editable widget path (or `""` if already focused) AND `command` to the non-empty text string to type (append `\n` to submit); `press_key` supports `RETURN`, `ENTER`, `TAB`, `SHIFT+TAB`, `ESCAPE`, `CTRL+SPACE`, `F1`–`F10`, and `ALT+P/D/W/N/F/C/R/G/T/M/X`; `click_widget` with `"double_click": true` works on both `eveui` cards and legacy rows; `drag_widget` MUST set `dest_x` and `dest_y` to the destination drop coordinates from `clickable_widgets`.
3. **Choosing `batch_status`:**
   - Set `batch_status = "completed"` if `max_batch_steps == 1` OR this action completes the final step of `delegated_plan`.
   - Set `batch_status = "continue"` if more planned steps remain after this action (`batch_step_idx < {max_steps}`).
   - Set `batch_status = "escalate"` (with a clear `escalation_reason`) ONLY if an abort condition in `delegated_plan.abort_conditions` is already triggered or the required state/target for Step 1 is completely absent and cannot be self-corrected.
{compact_contract}"""

    prev_batch_entry = batch_history[-1] if batch_history else None
    return f"""### Batch #{batch_id} Continuation — Global Step {step_num} (Batch Step {batch_step_idx} of {max_steps})
You are executing **Batch Step {batch_step_idx} of {max_steps}** of the delegated plan. First verify the outcome of Batch Step {batch_step_idx - 1}, then either execute Step {batch_step_idx}, self-correct a minor UI hiccup, or **escalate** to the Parent Planner if a bug/desync or abort condition occurred.

### Target Step to Execute Now (Batch Step {batch_step_idx} of {max_steps})
```json
{json.dumps(current_planned_step, separators=_sep, indent=_ind)}
```
- **Success Criteria:** `{delegated_plan.get('success_criteria', '')}`
- **Abort Conditions:** {json.dumps(delegated_plan.get('abort_conditions') or '')}

### Previous Batch Step ({batch_step_idx - 1}/{max_steps}) & Flight-Recorder Outcome
- **Previous Batch Action:**
```json
{json.dumps(prev_batch_entry, separators=_sep, indent=_ind)}
```
- **Flight-Recorder, `state_diff`, & `scatters_observed`:**
```json
{json.dumps(PlaytesterBugOracle.partition_flight_recorder_exceptions(last_flight_recorder), separators=_sep, indent=_ind)}
```

### Step {step_num} Live Perception
{exec_screen_line}
{burst_section}
### Live `uicore` Scene-Graph & Session Snapshot
```json
{json.dumps(compact_snap, separators=_sep, indent=_ind)}
```
### Clickable Widgets ({len(raw_widgets)} total, showing top {min(len(raw_widgets), 110)})
```text
{compact_widgets_block}
```

### Verification, Self-Correction & Escalation Rules
1. **Verify Batch Step {batch_step_idx - 1} (`previous_action_verification`):**
   - If the previous action returned `ok: false` / `action_error` due to a minor widget/input mismatch (e.g. widget name changed, a benign modal/tooltip was in the way, or target needed to be locked first) AND you can self-correct it right now within this batch, set `previous_action_verification.expectation_met = true`, explain the self-correction in `reasoning`, and output the corrective `action`!
   - **CRITICAL ESCALATION RULE:** If the previous action succeeded (`ok: true`) and **failed to update the UI or game state as expected** (silent `ui_state_desync`), OR `last_flight_recorder` contains non-empty `log_exceptions`, `quasar_errors`, or non-benign `macho_exceptions` (ignore `benign_log_exceptions` / `benign_macho_exceptions`), OR any `abort_conditions` were triggered:
     - Set `previous_action_verification.expectation_met = false` with a detailed `discrepancy_summary`.
     - Set `batch_status = "escalate"` and explain why in `escalation_reason` so the Parent Planner (`{DEFAULT_MODEL_CHAIN[0]}`) immediately takes over to diagnose and file the bug!
2. **Otherwise (`expectation_met = true`):**
   - Execute Batch Step {batch_step_idx} (`{current_planned_step.get('instruction') or current_planned_step.get('intent', '')}`).
   - For `drag_widget`, set `dest_x` and `dest_y` to the destination drop coordinates from `clickable_widgets`.
   - Set `batch_status = "completed"` if `{batch_step_idx} >= {max_steps}` or `success_criteria` is now fulfilled by this action; otherwise set `batch_status = "continue"`.
{compact_contract}"""


class _WarmAgyWorker:
    """Single pre-warmed `agy --input-format stream-json --output-format stream-json` worker."""

    def __init__(
        self,
        schema_path: Optional[Path],
        model_name: str,
        sandbox_dir: Path,
        enable_sandbox_files: bool = False,
    ):
        self.schema_path = Path(schema_path) if schema_path is not None else None
        self.model_name = model_name
        self.sandbox_dir = Path(sandbox_dir)
        self.enable_sandbox_files = bool(enable_sandbox_files)
        self.sandbox_dir.mkdir(parents=True, exist_ok=True)
        model_flags = _resolve_agy_model_args(model_name)
        cmd = [
            "agy",
            "-p=",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]
        if self.schema_path is not None:
            cmd.extend(["--json-schema", str(self.schema_path)])
        cmd.extend(model_flags)
        cmd.append("--disable-slash-commands")
        if self.enable_sandbox_files:
            cmd.extend(["--add-dir", str(self.sandbox_dir)])
        self.proc = subprocess.Popen(
            cmd,
            cwd=str(self.sandbox_dir),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
            start_new_session=True,
        )
        self.init_event = threading.Event()
        self.init_error: str = ""
        self.conversation_id: str = ""
        self._buf = bytearray()
        self._warm_thread = threading.Thread(target=self._wait_for_init, daemon=True)
        self._warm_thread.start()

    def _read_ndjson_line(self, deadline: float) -> Optional[str]:
        if self.proc.stdout is None:
            return None
        fd = self.proc.stdout.fileno()
        while True:
            nl = self._buf.find(b"\n")
            if nl != -1:
                raw_line = bytes(self._buf[:nl])
                del self._buf[: nl + 1]
                return raw_line.decode("utf-8", errors="replace")
            rem = deadline - time.time()
            if rem <= 0:
                raise subprocess.TimeoutExpired("agy", max(1.0, rem))
            ready, _, _ = select.select([fd], [], [], min(0.25, rem))
            if not ready:
                if self.proc.poll() is not None:
                    if self._buf:
                        raw_line = bytes(self._buf)
                        self._buf.clear()
                        return raw_line.decode("utf-8", errors="replace")
                    return None
                continue
            chunk = os.read(fd, 65536)
            if not chunk:
                if self._buf:
                    raw_line = bytes(self._buf)
                    self._buf.clear()
                    return raw_line.decode("utf-8", errors="replace")
                return None
            self._buf.extend(chunk)

    def _wait_for_init(self) -> None:
        deadline = time.time() + 20.0
        try:
            while time.time() < deadline:
                ln = self._read_ndjson_line(deadline)
                if ln is None:
                    self.init_error = "agy stream-json closed before init"
                    break
                s = ln.strip()
                if not s:
                    continue
                try:
                    ev = json.loads(s)
                except Exception:
                    continue
                if isinstance(ev, dict) and ev.get("event") == "init":
                    self.conversation_id = str(ev.get("conversation_id") or "")
                    break
        except Exception as exc:
            self.init_error = str(exc)
        finally:
            self.init_event.set()

    def is_alive(self) -> bool:
        return self.proc.poll() is None and not self.init_error

    def _read_available_stderr(self, max_bytes: int = 65536) -> str:
        """Drain already-buffered stderr without blocking if a grandchild keeps the FD open."""
        stderr = getattr(self.proc, "stderr", None)
        if stderr is None:
            return ""
        chunks: List[bytes] = []
        total = 0
        try:
            fd = stderr.fileno()
            os.set_blocking(fd, False)
            while total < max_bytes:
                ready, _, _ = select.select([fd], [], [], 0.0)
                if not ready:
                    break
                try:
                    chunk = os.read(fd, min(16384, max_bytes - total))
                except BlockingIOError:
                    break
                if not chunk:
                    break
                chunks.append(chunk)
                total += len(chunk)
        except Exception:
            pass
        return b"".join(chunks).decode("utf-8", errors="replace")

    def run_turn(self, prompt_text: str, timeout_secs: float) -> Tuple[int, str, str, float]:
        if not self.init_event.wait(timeout=min(15.0, max(5.0, timeout_secs))):
            self.terminate()
            raise subprocess.TimeoutExpired("agy-init", 15)
        if self.init_error or self.proc.poll() is not None:
            stderr_s = self._read_available_stderr()
            raise RuntimeError(self.init_error or stderr_s or "warm agy worker exited")

        t0 = time.time()
        deadline = t0 + max(5.0, float(timeout_secs))
        msg_line = json.dumps({"event": "user", "message": {"content": prompt_text}}) + "\n"
        assert self.proc.stdin is not None
        self.proc.stdin.write(msg_line.encode("utf-8"))
        self.proc.stdin.flush()
        self.proc.stdin.close()

        result_payload: Optional[Dict[str, Any]] = None
        raw_events: List[str] = []
        while True:
            ln = self._read_ndjson_line(deadline)
            if ln is None:
                break
            s = ln.strip()
            if not s:
                continue
            raw_events.append(s)
            try:
                ev = json.loads(s)
            except Exception:
                continue
            if isinstance(ev, dict) and ev.get("event") == "result":
                res_obj = ev.get("result")
                if isinstance(res_obj, dict):
                    result_payload = res_obj
                break

        try:
            rc = self.proc.wait(timeout=max(1.0, min(5.0, deadline - time.time())))
        except subprocess.TimeoutExpired:
            self.terminate()
            if result_payload is None:
                raise
            rc = 0
        stderr_s = self._read_available_stderr()
        self._close_pipes()
        elapsed = time.time() - t0
        if result_payload is not None:
            raw_out = json.dumps(result_payload)
        else:
            raw_out = "\n".join(raw_events)
        return int(rc or 0), raw_out, stderr_s, elapsed

    def _close_pipes(self) -> None:
        for stream in (getattr(self.proc, "stdin", None), getattr(self.proc, "stdout", None), getattr(self.proc, "stderr", None)):
            if stream is not None:
                try:
                    stream.close()
                except Exception:
                    pass

    def _kill_process_group(self, sig: int) -> bool:
        try:
            pgid = os.getpgid(self.proc.pid)
            os.killpg(pgid, sig)
            return True
        except Exception:
            return False

    def terminate(self) -> None:
        try:
            if self.proc.poll() is None:
                if not self._kill_process_group(signal.SIGTERM):
                    self.proc.terminate()
                try:
                    self.proc.wait(timeout=1.5)
                except subprocess.TimeoutExpired:
                    if not self._kill_process_group(signal.SIGKILL):
                        self.proc.kill()
                    try:
                        self.proc.wait(timeout=1.0)
                    except Exception:
                        pass
            else:
                self._kill_process_group(signal.SIGKILL)
        except Exception:
            pass
        finally:
            self._close_pipes()


class AgyStreamWorkerPool:
    """Maintains pre-warmed standby `agy stream-json` workers to eliminate cold CLI boot (~1.4s)."""

    def __init__(
        self,
        sandbox_dir: Path = Path("/tmp/evomu_playtester_agy_sandbox"),
        standby_specs: Optional[List[Tuple[Any, ...]]] = None,
        auto_respawn: bool = True,
    ):
        self.sandbox_dir = Path(sandbox_dir)
        self.auto_respawn = bool(auto_respawn)
        self.standby_specs: Dict[Tuple[str, str, bool], int] = {}
        for spec in standby_specs or []:
            if not spec:
                continue
            sp = spec[0]
            m = spec[1]
            if not m:
                continue
            ef = bool(spec[2]) if len(spec) >= 3 else False
            cnt = int(spec[3]) if len(spec) >= 4 else 1
            key = (str(Path(sp)) if sp is not None else "", str(m), ef)
            self.standby_specs[key] = max(1, cnt)
        self._lock = threading.Lock()
        self._standby: Dict[Tuple[str, str, bool], List[_WarmAgyWorker]] = {}
        self._closed = False
        atexit.register(self.shutdown)

    def prewarm_all(self) -> None:
        for (sp_str, model_name, ef), cnt in self.standby_specs.items():
            sp = Path(sp_str) if sp_str else None
            self._ensure_standby(sp, model_name, enable_sandbox_files=ef, target_count=cnt)

    def _ensure_standby(
        self,
        schema_path: Optional[Path],
        model_name: str,
        enable_sandbox_files: bool = False,
        target_count: int = 1,
    ) -> None:
        key = (str(Path(schema_path)) if schema_path is not None else "", model_name, bool(enable_sandbox_files))
        with self._lock:
            if self._closed:
                return
            alive_list: List[_WarmAgyWorker] = []
            for w in self._standby.get(key, []):
                if not w.init_event.is_set() or w.is_alive():
                    alive_list.append(w)
                else:
                    w.terminate()
            self._standby[key] = alive_list
            needed = max(0, target_count - len(alive_list))
            for _ in range(needed):
                try:
                    alive_list.append(
                        _WarmAgyWorker(
                            schema_path,
                            model_name,
                            self.sandbox_dir,
                            enable_sandbox_files=enable_sandbox_files,
                        )
                    )
                except Exception as exc:
                    log_warn(f"Failed to pre-warm agy worker ({model_name}): {exc}")

    def invalidate_model(self, model_name: str) -> None:
        with self._lock:
            victims = [k for k in self._standby if k[1] == model_name]
            for k in victims:
                for w in self._standby.pop(k, []):
                    w.terminate()

    def run_turn(
        self,
        schema_path: Optional[Path],
        model_name: str,
        prompt_text: str,
        timeout_secs: float = 90.0,
        enable_sandbox_files: bool = False,
    ) -> Tuple[int, str, str, float]:
        key = (str(Path(schema_path)) if schema_path is not None else "", model_name, bool(enable_sandbox_files))
        worker: Optional[_WarmAgyWorker] = None
        with self._lock:
            q_list = self._standby.get(key) or []
            while q_list:
                cand = q_list.pop(0)
                if not cand.init_event.is_set() or cand.is_alive():
                    worker = cand
                    break
                cand.terminate()
            if not q_list:
                self._standby.pop(key, None)
        if not self._closed and self.auto_respawn:
            want_cnt = self.standby_specs.get(key, 1)
            threading.Thread(
                target=self._ensure_standby,
                args=(schema_path, model_name, enable_sandbox_files, want_cnt),
                daemon=True,
            ).start()
        if worker is None:
            worker = _WarmAgyWorker(
                schema_path,
                model_name,
                self.sandbox_dir,
                enable_sandbox_files=enable_sandbox_files,
            )
        try:
            return worker.run_turn(prompt_text, timeout_secs=timeout_secs)
        except Exception:
            worker.terminate()
            raise

    def shutdown(self) -> None:
        with self._lock:
            self._closed = True
            workers = [w for lst in self._standby.values() for w in lst]
            self._standby.clear()
        for w in workers:
            w.terminate()


def invoke_agy_planner(
    prompt_text: str,
    step_dir: Path,
    model_chain: List[str],
    timeout_secs: int = 120,
    stream_pool: Optional[AgyStreamWorkerPool] = None,
    diagnostic: bool = True,
) -> Tuple[Dict[str, Any], str, float]:
    """
    Invoke the Strategic Parent Planner (`hydrogen-l-tf-medium` routine / `hydrogen-l-tf-high` diagnostic).
    Routine turns with `stream_pool` omit `--json-schema` and `--add-dir` (with 1-shot schema retry fallback)
    to eliminate duplicate output-token serialization and screenshot file-read latency.
    """
    step_dir.mkdir(parents=True, exist_ok=True)
    agy_sandbox_dir = Path("/tmp/evomu_playtester_agy_sandbox")
    agy_sandbox_dir.mkdir(parents=True, exist_ok=True)
    src_screen = step_dir / "screen.png"
    sandbox_screen = agy_sandbox_dir / "screen.png"
    if src_screen.exists():
        try:
            shutil.copy2(src_screen, sandbox_screen)
            prompt_text = prompt_text.replace(str(src_screen), str(sandbox_screen))
        except Exception:
            pass
    prompt_file = step_dir / "planner_prompt.md"
    prompt_file.write_text(prompt_text, encoding="utf-8")

    last_err = ""
    for idx, model_name in enumerate(model_chain):
        cooldown_until = _AGY_MODEL_COOLDOWN_UNTIL.get(model_name, 0.0)
        now_ts = time.time()
        if now_ts < cooldown_until and idx < len(model_chain) - 1:
            continue
        model_flags = _resolve_agy_model_args(model_name)
        cmd = [
            "agy",
            "-p",
            prompt_text,
            *model_flags,
            "--output-format",
            "json",
            "--json-schema",
            str(PLANNER_SCHEMA_PATH),
            "--disable-slash-commands",
            "--add-dir",
            str(agy_sandbox_dir),
        ]
        t0 = time.time()
        try:
            if stream_pool is not None:
                use_schema = PLANNER_SCHEMA_PATH if diagnostic else None
                use_files = bool(diagnostic)
                rc, raw_out, stderr_str, elapsed = stream_pool.run_turn(
                    schema_path=use_schema,
                    model_name=model_name,
                    prompt_text=prompt_text,
                    timeout_secs=timeout_secs,
                    enable_sandbox_files=use_files,
                )
                if rc == 0 and not diagnostic:
                    probe_dec = _normalize_planner_decision(
                        _extract_json_decision(raw_out, required_key="delegated_plan")
                    )
                    if not (probe_dec and isinstance(probe_dec.get("delegated_plan"), dict) and probe_dec["delegated_plan"].get("steps")):
                        log_warn(f"Planner no-schema parse missed on {model_name}; retrying once with --json-schema fallback.")
                        rc, raw_out, stderr_str, elapsed2 = stream_pool.run_turn(
                            schema_path=PLANNER_SCHEMA_PATH,
                            model_name=model_name,
                            prompt_text=prompt_text,
                            timeout_secs=timeout_secs,
                            enable_sandbox_files=False,
                        )
                        elapsed += elapsed2
            else:
                proc = subprocess.run(
                    cmd,
                    cwd=str(agy_sandbox_dir),
                    capture_output=True,
                    text=True,
                    timeout=timeout_secs,
                )
                elapsed = time.time() - t0
                rc = proc.returncode
                raw_out = (proc.stdout or "").strip()
                stderr_str = proc.stderr or ""
            (step_dir / f"agy_planner_raw_{model_name}.txt").write_text(
                raw_out + "\n--- STDERR ---\n" + stderr_str,
                encoding="utf-8",
            )
            if rc == 0 and raw_out:
                conv_id = ""
                usage: Dict[str, Any] = {}
                num_turns = 1
                try:
                    outer = json.loads(raw_out)
                    if isinstance(outer, dict):
                        conv_id = str(outer.get("conversation_id") or "")
                        usage = outer.get("usage") or {}
                        num_turns = int(outer.get("num_turns") or 1)
                except Exception:
                    pass
                plan_decision = _extract_json_decision(raw_out, required_key="delegated_plan")
                if plan_decision is None and conv_id:
                    plan_decision = _extract_agy_latest_finish_decision(conv_id, required_key="delegated_plan")
                plan_decision = _normalize_planner_decision(plan_decision)
                if plan_decision and isinstance(plan_decision.get("delegated_plan"), dict):
                    thinking_blocks, tool_calls = _extract_agy_transcript_traces(conv_id, latest_turn_only=True)
                    if conv_id:
                        plan_decision["_conversation_id"] = conv_id
                    if usage:
                        plan_decision["_usage"] = usage
                    plan_decision["_num_turns"] = num_turns
                    if tool_calls:
                        plan_decision["_tool_calls"] = tool_calls
                    if thinking_blocks:
                        plan_decision["_thinking_trace"] = thinking_blocks
                        (step_dir / "planner_thinking.md").write_text(
                            "\n\n---\n\n".join(thinking_blocks) + "\n",
                            encoding="utf-8",
                        )
                    plan_decision["_planner_model"] = model_name
                    plan_decision["_planner_elapsed_sec"] = round(elapsed, 2)
                    (step_dir / "plan.json").write_text(
                        json.dumps(plan_decision, indent=2),
                        encoding="utf-8",
                    )
                    step_num_guess = 0
                    try:
                        step_num_guess = int(step_dir.name.replace("step_", ""))
                    except Exception:
                        pass
                    log_planner_trace(step_num_guess, model_name, elapsed, plan_decision)
                    return plan_decision, model_name, elapsed
            combined_err = f"{stderr_str}\n{raw_out}"
            if "RESOURCE_EXHAUSTED" in combined_err or '"error_code":429' in combined_err or "exhausted your capacity" in combined_err:
                cd_secs = _parse_quota_cooldown_secs(combined_err)
                _AGY_MODEL_COOLDOWN_UNTIL[model_name] = time.time() + cd_secs
                if stream_pool is not None:
                    stream_pool.invalidate_model(model_name)
                log_warn(f"Planner model {model_name} hit 429 RESOURCE_EXHAUSTED; cooling down for {int(cd_secs)}s and trying next fallback...")
                last_err = f"model={model_name} rc={rc} quota_429_cooldown={int(cd_secs)}s"
                continue
            last_err = f"model={model_name} rc={rc} stderr={stderr_str[-300:] if stderr_str else raw_out[-200:]}"
            log_warn(f"Planner model {model_name} failed ({last_err}); trying next fallback...")
        except subprocess.TimeoutExpired:
            last_err = f"model={model_name} timed out after {timeout_secs}s"
            log_warn(f"Planner model {model_name} timed out after {timeout_secs}s; trying next fallback...")
        except Exception as exc:
            last_err = f"model={model_name} exc={exc}"
            log_warn(f"Planner model {model_name} error ({exc}); trying next fallback...")

    log_warn(f"All Planner models in chain {model_chain} failed ({last_err}); using heuristic fallback plan.")
    fallback_plan = _heuristic_fallback_planner_decision()
    return fallback_plan, "heuristic-fallback", 0.0


def invoke_agy_executor(
    prompt_text: str,
    step_dir: Path,
    model_chain: List[str],
    conversation_id: Optional[str] = None,
    batch_id: int = 1,
    batch_step_idx: int = 1,
    max_batch_steps: int = 1,
    timeout_secs: int = 90,
    stream_pool: Optional[AgyStreamWorkerPool] = None,
) -> Tuple[Dict[str, Any], str, str, float]:
    """
    Invoke the Fast Tactical Executor (`hydrogen-l-tf-medium` -> fallback chain) using `EXECUTOR_SCHEMA_PATH`.
    Uses pre-warmed `AgyStreamWorkerPool` when `stream_pool` is provided; falls back to `subprocess.run` otherwise.
    """
    step_dir.mkdir(parents=True, exist_ok=True)
    agy_sandbox_dir = Path("/tmp/evomu_playtester_agy_sandbox")
    agy_sandbox_dir.mkdir(parents=True, exist_ok=True)
    src_screen = step_dir / "screen.png"
    sandbox_screen = agy_sandbox_dir / "screen.png"
    if src_screen.exists():
        try:
            shutil.copy2(src_screen, sandbox_screen)
            prompt_text = prompt_text.replace(str(src_screen), str(sandbox_screen))
        except Exception:
            pass
    prompt_file = step_dir / "executor_prompt.md"
    prompt_file.write_text(prompt_text, encoding="utf-8")

    last_err = ""
    for idx, model_name in enumerate(model_chain):
        cooldown_until = _AGY_MODEL_COOLDOWN_UNTIL.get(model_name, 0.0)
        now_ts = time.time()
        if now_ts < cooldown_until and idx < len(model_chain) - 1:
            continue
        model_flags = _resolve_agy_model_args(model_name)
        conv_flags = ["--conversation", conversation_id] if (conversation_id and idx == 0) else []
        cmd = [
            "agy",
            "-p",
            prompt_text,
            *conv_flags,
            *model_flags,
            "--output-format",
            "json",
            "--json-schema",
            str(EXECUTOR_SCHEMA_PATH),
            "--disable-slash-commands",
            "--add-dir",
            str(agy_sandbox_dir),
        ]
        t0 = time.time()
        try:
            use_stream = stream_pool is not None
            if use_stream:
                rc, raw_out, stderr_str, elapsed = stream_pool.run_turn(
                    schema_path=None,
                    model_name=model_name,
                    prompt_text=prompt_text,
                    timeout_secs=timeout_secs,
                    enable_sandbox_files=False,
                )
                if rc == 0:
                    probe_exec = _normalize_executor_decision(
                        _extract_json_decision(raw_out, required_key="action"),
                        batch_step_idx=batch_step_idx,
                    )
                    if not (probe_exec and isinstance(probe_exec.get("action"), dict) and probe_exec["action"].get("type")):
                        log_warn(f"Executor no-schema parse missed on {model_name}; retrying once with --json-schema fallback.")
                        rc, raw_out, stderr_str, elapsed2 = stream_pool.run_turn(
                            schema_path=EXECUTOR_SCHEMA_PATH,
                            model_name=model_name,
                            prompt_text=prompt_text,
                            timeout_secs=timeout_secs,
                            enable_sandbox_files=False,
                        )
                        elapsed += elapsed2
            else:
                proc = subprocess.run(
                    cmd,
                    cwd=str(agy_sandbox_dir),
                    capture_output=True,
                    text=True,
                    timeout=timeout_secs,
                )
                elapsed = time.time() - t0
                rc = proc.returncode
                raw_out = (proc.stdout or "").strip()
                stderr_str = proc.stderr or ""
            (step_dir / f"agy_executor_raw_{model_name}.txt").write_text(
                raw_out + "\n--- STDERR ---\n" + stderr_str,
                encoding="utf-8",
            )
            if rc == 0 and raw_out:
                step_num_guess = 0
                try:
                    step_num_guess = int(step_dir.name.replace("step_", ""))
                except Exception:
                    pass
                conv_id = conversation_id or ""
                usage: Dict[str, Any] = {}
                num_turns = 1
                try:
                    outer = json.loads(raw_out)
                    if isinstance(outer, dict):
                        conv_id = str(outer.get("conversation_id") or conv_id)
                        usage = outer.get("usage") or {}
                        num_turns = int(outer.get("num_turns") or 1)
                except Exception:
                    pass
                stdout_decision = _extract_json_decision(raw_out, required_key="action")
                used_conv = (conversation_id if idx == 0 else None) if not use_stream else None
                transcript_marker = f"Global Step {step_num_guess}" if (used_conv and step_num_guess > 0) else ""
                transcript_decision = (
                    _extract_agy_latest_finish_decision(
                        conv_id, required_key="action", expected_marker=transcript_marker
                    )
                    if (conv_id and not use_stream)
                    else None
                )
                exec_decision = stdout_decision
                if transcript_decision is not None:
                    if stdout_decision is None:
                        exec_decision = transcript_decision
                    elif used_conv and transcript_decision != stdout_decision:
                        log_warn(
                            f"agy stdout structured_output was stale on resumed batch conv {conv_id[:8]} "
                            f"(stdout step={stdout_decision.get('batch_step_number')} != "
                            f"transcript step={transcript_decision.get('batch_step_number')}); "
                            "using latest-turn finish tool args from transcript."
                        )
                        exec_decision = transcript_decision
                exec_decision = _normalize_executor_decision(exec_decision, batch_step_idx=batch_step_idx)
                if exec_decision and isinstance(exec_decision.get("action"), dict):
                    thinking_blocks, tool_calls = _extract_agy_transcript_traces(conv_id, latest_turn_only=True)
                    if conv_id:
                        exec_decision["_conversation_id"] = conv_id
                    if usage:
                        exec_decision["_usage"] = usage
                    exec_decision["_num_turns"] = num_turns
                    if tool_calls:
                        exec_decision["_tool_calls"] = tool_calls
                    if thinking_blocks:
                        exec_decision["_thinking_trace"] = thinking_blocks
                        (step_dir / "thinking.md").write_text(
                            "\n\n---\n\n".join(thinking_blocks) + "\n",
                            encoding="utf-8",
                        )
                    log_executor_trace(
                        step_num_guess,
                        batch_step_idx,
                        max_batch_steps,
                        model_name,
                        elapsed,
                        exec_decision,
                    )
                    return exec_decision, model_name, conv_id, elapsed
            combined_err = f"{stderr_str}\n{raw_out}"
            if "RESOURCE_EXHAUSTED" in combined_err or '"error_code":429' in combined_err or "exhausted your capacity" in combined_err:
                cd_secs = _parse_quota_cooldown_secs(combined_err)
                _AGY_MODEL_COOLDOWN_UNTIL[model_name] = time.time() + cd_secs
                if stream_pool is not None:
                    stream_pool.invalidate_model(model_name)
                log_warn(f"Executor model {model_name} hit 429 RESOURCE_EXHAUSTED; cooling down for {int(cd_secs)}s and trying next fallback...")
                last_err = f"model={model_name} rc={rc} quota_429_cooldown={int(cd_secs)}s"
                continue
            last_err = f"model={model_name} rc={rc} stderr={stderr_str[-300:] if stderr_str else raw_out[-200:]}"
            log_warn(f"Executor model {model_name} failed ({last_err}); trying next fallback...")
        except subprocess.TimeoutExpired:
            last_err = f"model={model_name} timed out after {timeout_secs}s"
            log_warn(f"Executor model {model_name} timed out after {timeout_secs}s; trying next fallback...")
        except Exception as exc:
            last_err = f"model={model_name} exc={exc}"
            log_warn(f"Executor model {model_name} error ({exc}); trying next fallback...")

    log_warn(f"All Executor models in chain {model_chain} failed ({last_err}); using heuristic fallback action.")
    return _heuristic_fallback_executor_decision(), "heuristic-fallback", conversation_id or "", 0.0


def invoke_agy_reasoning(
    prompt_text: str,
    step_dir: Path,
    model_chain: List[str],
    timeout_secs: int = 120,
) -> Tuple[Dict[str, Any], str]:
    """
    Call `agy -p` in oneshot mode with `--output-format json --json-schema`.
    Runs inside `/tmp/evomu_playtester_agy_sandbox` with `--settings '{"hooks":{}}'`
    so repository code-review Stop hooks do not block the read-only playtester decision.
    """
    step_dir.mkdir(parents=True, exist_ok=True)
    agy_sandbox_dir = Path("/tmp/evomu_playtester_agy_sandbox")
    agy_sandbox_dir.mkdir(parents=True, exist_ok=True)
    src_screen = step_dir / "screen.png"
    sandbox_screen = agy_sandbox_dir / "screen.png"
    if src_screen.exists():
        try:
            shutil.copy2(src_screen, sandbox_screen)
            prompt_text = prompt_text.replace(str(src_screen), str(sandbox_screen))
        except Exception:
            pass
    prompt_file = step_dir / "prompt.md"
    prompt_file.write_text(prompt_text, encoding="utf-8")

    last_err = ""
    for idx, model_name in enumerate(model_chain):
        cooldown_until = _AGY_MODEL_COOLDOWN_UNTIL.get(model_name, 0.0)
        now_ts = time.time()
        if now_ts < cooldown_until and idx < len(model_chain) - 1:
            continue
        model_flags = _resolve_agy_model_args(model_name)
        cmd = [
            "agy",
            "-p",
            prompt_text,
            *model_flags,
            "--output-format",
            "json",
            "--json-schema",
            str(ACTION_SCHEMA_PATH),
            "--disable-slash-commands",
            "--add-dir",
            str(agy_sandbox_dir),
        ]
        t0 = time.time()
        try:
            proc = subprocess.run(
                cmd,
                cwd=str(agy_sandbox_dir),
                capture_output=True,
                text=True,
                timeout=timeout_secs,
            )
            elapsed = time.time() - t0
            raw_out = (proc.stdout or "").strip()
            (step_dir / f"agy_raw_{model_name}.txt").write_text(
                raw_out + "\n--- STDERR ---\n" + (proc.stderr or ""),
                encoding="utf-8",
            )
            if proc.returncode == 0 and raw_out:
                decision = _extract_json_decision(raw_out)
                if decision and isinstance(decision.get("action"), dict):
                    conv_id = ""
                    usage: Dict[str, Any] = {}
                    num_turns = 1
                    try:
                        outer = json.loads(raw_out)
                        if isinstance(outer, dict):
                            conv_id = str(outer.get("conversation_id") or "")
                            usage = outer.get("usage") or {}
                            num_turns = int(outer.get("num_turns") or 1)
                    except Exception:
                        pass
                    thinking_blocks, tool_calls = _extract_agy_transcript_traces(conv_id)
                    if conv_id:
                        decision["_conversation_id"] = conv_id
                    if usage:
                        decision["_usage"] = usage
                    decision["_num_turns"] = num_turns
                    if tool_calls:
                        decision["_tool_calls"] = tool_calls
                    if thinking_blocks:
                        decision["_thinking_trace"] = thinking_blocks
                        (step_dir / "thinking.md").write_text(
                            "\n\n---\n\n".join(thinking_blocks) + "\n",
                            encoding="utf-8",
                        )
                    step_num_guess = 0
                    try:
                        step_num_guess = int(step_dir.name.replace("step_", ""))
                    except Exception:
                        pass
                    log_agy_reasoning_trace(step_num_guess, model_name, elapsed, decision)
                    return decision, model_name
            stderr_str = proc.stderr or ""
            if "RESOURCE_EXHAUSTED" in stderr_str or '"error_code":429' in stderr_str or "exhausted your capacity" in stderr_str:
                cd_secs = _parse_quota_cooldown_secs(stderr_str)
                _AGY_MODEL_COOLDOWN_UNTIL[model_name] = time.time() + cd_secs
                log_warn(f"agy model {model_name} hit 429 RESOURCE_EXHAUSTED; cooling down for {int(cd_secs)}s and trying next fallback...")
                last_err = f"model={model_name} rc={proc.returncode} quota_429_cooldown={int(cd_secs)}s"
                continue
            last_err = f"model={model_name} rc={proc.returncode} stderr={stderr_str[-300:] if stderr_str else ''}"
            log_warn(f"agy model {model_name} failed ({last_err}); trying next fallback...")
        except subprocess.TimeoutExpired:
            last_err = f"model={model_name} timed out after {timeout_secs}s"
            log_warn(f"agy model {model_name} timed out after {timeout_secs}s; trying next fallback...")
        except Exception as exc:
            last_err = f"model={model_name} exc={exc}"
            log_warn(f"agy model {model_name} error ({exc}); trying next fallback...")

    # Deterministic heuristic fallback if all agy models time out or fail
    log_warn(f"All agy models in chain {model_chain} failed ({last_err}); using heuristic fallback step.")
    return _heuristic_fallback_decision(), "heuristic-fallback"


_AGY_MODEL_COOLDOWN_UNTIL: Dict[str, float] = {}


def _parse_quota_cooldown_secs(stderr_text: str) -> float:
    import re as _re
    m = _re.search(r"reset after (?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s)?", stderr_text or "")
    if m and any(m.groups()):
        h = int(m.group(1) or 0)
        mins = int(m.group(2) or 0)
        secs = int(m.group(3) or 0)
        total = float(h * 3600 + mins * 60 + secs)
        if total > 0:
            return min(total, 1800.0)
    return 600.0


def _extract_json_object_from_text(text: str, required_key: str) -> Optional[Dict[str, Any]]:
    """Extract the first JSON object dict containing `required_key` from text (stripping fences/prose)."""
    if not text or not isinstance(text, str):
        return None
    s = text.strip()
    try:
        obj = json.loads(s)
        if isinstance(obj, dict) and required_key in obj:
            return obj
    except Exception:
        pass
    decoder = json.JSONDecoder()
    idx = 0
    while idx < len(s):
        pos = s.find("{", idx)
        if pos == -1:
            break
        try:
            obj, end = decoder.raw_decode(s, pos)
            if isinstance(obj, dict) and required_key in obj:
                return obj
            idx = max(pos + 1, end)
        except Exception:
            idx = pos + 1
    return None


def _extract_json_decision(raw_out: str, required_key: str = "action") -> Optional[Dict[str, Any]]:
    """Parse `agy --output-format json` wrapper or raw markdown/JSON object."""
    try:
        parsed = json.loads(raw_out)
        if isinstance(parsed, dict):
            # Some CLI versions wrap structured output under "result" or "response" or "structured_output"
            if required_key in parsed and "observation" in parsed:
                return parsed
            for key in ("structured_output", "result", "response", "output", "content"):
                inner = parsed.get(key)
                if isinstance(inner, dict) and required_key in inner:
                    return inner
                if isinstance(inner, str):
                    inner_obj = _extract_json_object_from_text(inner, required_key=required_key)
                    if inner_obj is not None:
                        return inner_obj
    except Exception:
        pass

    return _extract_json_object_from_text(raw_out, required_key=required_key)


def _resolve_wait_action_ms(
    act_type: str,
    target: Any,
    command: Any,
    raw_wait_ms: Any,
    default_ms: int = 250,
) -> int:
    """Resolve wait_ms, honoring numeric durations placed in target/command for wait actions."""
    wait_ms = int(raw_wait_ms) if raw_wait_ms is not None else default_ms
    if act_type != "wait":
        return wait_ms
    for candidate in (target, command):
        s = str(candidate or "").strip().lower()
        if s.startswith("wait "):
            s = s[5:].strip()
        if s.endswith("ms"):
            s = s[:-2].strip()
        if s.isdigit():
            parsed = int(s)
            if parsed > 0 and (raw_wait_ms is None or wait_ms <= 500 < parsed):
                return parsed
            break
        if ("warp" in s or "arrival" in s) and (raw_wait_ms is None or wait_ms <= 500):
            return max(wait_ms, 8000)
    return wait_ms


def _coerce_action_command(act: Dict[str, Any]) -> Any:
    """Resolve `command` from explicit field or common LLM/hook aliases (`slash_cmd`, `text`, `input`, `value`)."""
    cmd = act.get("command")
    if cmd not in (None, ""):
        return cmd
    for alias in ("slash_cmd", "text", "input", "value"):
        alt = act.get(alias)
        if alt not in (None, ""):
            return alt
    return cmd


def _canonicalize_action_type(act_type: str, has_target: bool) -> str:
    """Normalize common LLM action type aliases to canonical harness verbs."""
    norm = str(act_type or "").strip()
    if norm == "click":
        return "click_widget" if has_target else "click_xy"
    if norm in ("drag", "drag_and_drop"):
        return "drag_widget"
    if norm in ("type", "input_text", "enter_text"):
        return "type_text"
    if norm in ("key_press", "hotkey", "press_hotkey", "shortcut"):
        return "press_key"
    return norm


def _extract_optional_action_flags(raw_act: Optional[Dict[str, Any]]) -> Dict[str, Any]:
    """Preserve supported optional boolean action flags without leaking arbitrary keys."""
    if not isinstance(raw_act, dict):
        return {}
    flags: Dict[str, Any] = {}
    for key in ("strict_wait", "clear", "expect_in_warp_rejection", "allow_in_warp_error"):
        if raw_act.get(key):
            flags[key] = True
    return flags


def _compute_step_rpc_timeout(action: Dict[str, Any]) -> float:
    """Compute host RPC socket timeout strictly above in-client tasklet wait ceilings."""
    atype = str(action.get("type") or "").strip().lower()
    eff_type = str(action.get("target") or "").strip().lower() if atype == "space_command" else atype
    if eff_type == "jump_gate":
        return 75.0
    if eff_type == "dock":
        return 65.0
    if eff_type in (
        "warp_to",
        "warp_to_location",
        "warp_to_bookmark",
        "undock",
        "select_character",
        "approach",
        "orbit",
        "keep_at_range",
        "align_to",
        "align",
        "stop",
        "space_command",
    ):
        return 35.0
    if atype == "wait":
        try:
            req_s = min(max(float(action.get("wait_ms") or 0) / 1000.0, 0.0), 30.0)
        except Exception:
            req_s = 0.0
        return max(20.0, req_s + 15.0)
    return 20.0


def _normalize_action_defaults(raw_act: Optional[Dict[str, Any]]) -> Dict[str, Any]:
    act = dict(raw_act or {})
    target = act.get("target") if act.get("target") not in ("",) else None
    command = _coerce_action_command(act)
    act_type = _canonicalize_action_type(str(act.get("type") or ""), bool(target))
    extra_dest: Dict[str, str] = {}
    for dest_key in ("dest_target", "dest_widget"):
        raw_dest = act.get(dest_key)
        if raw_dest is not None and not isinstance(raw_dest, bool):
            dest_s = str(raw_dest).strip()
            if dest_s:
                extra_dest[dest_key] = dest_s
    return {
        "type": act_type,
        "target": target,
        "command": command,
        "x": act.get("x"),
        "y": act.get("y"),
        "dest_x": act.get("dest_x"),
        "dest_y": act.get("dest_y"),
        **extra_dest,
        "right_click": bool(act.get("right_click", False)),
        "double_click": bool(act.get("double_click", False)),
        "wait_ms": _resolve_wait_action_ms(
            act_type, target, command, act.get("wait_ms")
        ),
        "expected_ui_and_state_change": str(act.get("expected_ui_and_state_change") or ""),
        **_extract_optional_action_flags(act),
    }


def _normalize_planner_decision(raw_dec: Optional[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
    """Expand slim Routine Planner JSON output into full internal schema shape."""
    if not isinstance(raw_dec, dict) or not isinstance(raw_dec.get("delegated_plan"), dict):
        return raw_dec
    dec = dict(raw_dec)
    pav = dict(dec.get("previous_action_verification") or {})
    dec["previous_action_verification"] = {
        "expectation_met": bool(pav.get("expectation_met", True)),
        "discrepancy_summary": str(pav.get("discrepancy_summary") or ""),
    }
    dec.setdefault("observation", "")
    dec.setdefault("reasoning", "")
    dec.setdefault("current_goal", "")

    dp = dict(dec["delegated_plan"])
    if not dp.get("plan_goal") and dp.get("goal"):
        dp["plan_goal"] = dp.get("goal")
    dp.setdefault("plan_goal", "")
    dp.setdefault("horizon_rationale", "")
    dp.setdefault("success_criteria", "")
    ab = dp.get("abort_conditions")
    if isinstance(ab, list):
        dp["abort_conditions"] = "; ".join(str(x) for x in ab)
    elif ab is None:
        dp["abort_conditions"] = ""

    norm_steps: List[Dict[str, Any]] = []
    for i, st in enumerate(dp.get("steps") or [], start=1):
        if not isinstance(st, dict):
            continue
        ns = dict(st)
        ns["step_number"] = int(ns.get("step_number") or i)
        ns["exec_mode"] = str(ns.get("exec_mode") or "direct").strip().lower()
        ns["capture_screenshot"] = bool(ns.get("capture_screenshot", False))
        raw_act = ns.get("action")
        if isinstance(raw_act, dict) and raw_act.get("type"):
            ns["action"] = _normalize_action_defaults(raw_act)
            ns.setdefault("action_type_hint", ns["action"]["type"])
            ns.setdefault(
                "target_or_command_hint",
                str(ns["action"].get("target") or ns["action"].get("command") or ""),
            )
            ns.setdefault("expected_outcome", ns["action"].get("expected_ui_and_state_change") or "")
            ns.setdefault("instruction", f"{ns['action']['type']} {ns.get('target_or_command_hint', '')}".strip())
        else:
            ns["action"] = None
            ns.setdefault("action_type_hint", str(ns.get("action_type") or ""))
            ns.setdefault("target_or_command_hint", str(ns.get("target_hint") or ""))
            ns.setdefault("expected_outcome", str(ns.get("expected_ui_and_state_change") or ""))
            ns.setdefault("instruction", str(ns.get("intent") or ""))
        raw_assert = ns.get("assertions")
        ns["assertions"] = dict(raw_assert) if isinstance(raw_assert, dict) else {}
        norm_steps.append(ns)
    dp["steps"] = norm_steps
    dp["max_batch_steps"] = int(dp.get("max_batch_steps") or len(norm_steps) or 1)
    dec["delegated_plan"] = dp

    mu = dict(dec.get("memory_update") or {})
    dec["memory_update"] = {
        "working_memory": list(mu.get("working_memory") or []),
        "long_term_note_to_append": str(mu.get("long_term_note_to_append") or ""),
        "mark_tested_features": list(mu.get("mark_tested_features") or []),
        "mark_buggy_features": list(mu.get("mark_buggy_features") or []),
        "new_frontier_features": list(mu.get("new_frontier_features") or []),
    }

    br = dict(dec.get("bug_report") or {})
    dec["bug_report"] = {
        "should_file": bool(br.get("should_file", False)),
        "category": str(br.get("category") or "none"),
        "title": str(br.get("title") or ""),
        "severity": str(br.get("severity") or "low"),
        "reproduction_steps": list(br.get("reproduction_steps") or []),
        "expected_behavior": str(br.get("expected_behavior") or ""),
        "actual_behavior": str(br.get("actual_behavior") or ""),
        "subsystem": str(br.get("subsystem") or ""),
    }
    return dec


def _normalize_executor_decision(
    raw_dec: Optional[Dict[str, Any]], batch_step_idx: int = 1
) -> Optional[Dict[str, Any]]:
    """Expand slim Executor JSON output into full internal schema shape."""
    if not isinstance(raw_dec, dict) or not isinstance(raw_dec.get("action"), dict):
        return raw_dec
    dec = dict(raw_dec)
    pav = dict(dec.get("previous_action_verification") or {})
    dec["previous_action_verification"] = {
        "expectation_met": bool(pav.get("expectation_met", True)),
        "discrepancy_summary": str(pav.get("discrepancy_summary") or ""),
    }
    dec.setdefault("observation", "")
    dec.setdefault("reasoning", "")
    dec["batch_step_number"] = int(dec.get("batch_step_number") or batch_step_idx or 1)
    dec["batch_status"] = str(dec.get("batch_status") or "continue").strip().lower()
    dec["escalation_reason"] = str(dec.get("escalation_reason") or "")
    dec["action"] = _normalize_action_defaults(dec["action"])
    return dec


def _heuristic_fallback_planner_decision() -> Dict[str, Any]:
    return {
        "observation": "Fallback Planner step (agy CLI call timed out or failed).",
        "previous_action_verification": {
            "expectation_met": True,
            "discrepancy_summary": "",
        },
        "reasoning": "Delegate a safe single-step Neocom inspection while preserving session continuity.",
        "current_goal": "Keep exploration loop advancing across core windows",
        "delegated_plan": {
            "plan_goal": "Open Character Sheet / Skills window to verify UI responsiveness",
            "horizon_rationale": "Single-step fallback plan after Planner CLI timeout",
            "max_batch_steps": 1,
            "steps": [
                {
                    "step_number": 1,
                    "intent": "Open Character Sheet / Skills window",
                    "action_type": "open_neocom",
                    "target_hint": "skills",
                    "expected_ui_and_state_change": "Character Sheet / Skills window opens in open_windows",
                }
            ],
            "success_criteria": "Skills window opens without error",
            "abort_conditions": "Error modal appears",
        },
        "memory_update": {
            "working_memory": ["Used heuristic fallback plan due to Planner CLI timeout."],
            "long_term_note_to_append": "",
            "mark_tested_features": [],
            "mark_buggy_features": [],
            "new_frontier_features": [],
        },
        "bug_report": {
            "should_file": False,
            "category": "none",
            "title": "",
            "severity": "medium",
            "reproduction_steps": [],
            "expected_behavior": "",
            "actual_behavior": "",
            "subsystem": "",
        },
    }


def _heuristic_fallback_executor_decision() -> Dict[str, Any]:
    return {
        "observation": "Fallback Executor step (agy CLI call timed out or failed).",
        "previous_action_verification": {
            "expectation_met": True,
            "discrepancy_summary": "",
        },
        "reasoning": "Execute safe fallback Neocom step and mark batch completed so Planner regains control next step.",
        "batch_step_number": 1,
        "batch_status": "completed",
        "escalation_reason": "",
        "action": {
            "type": "open_neocom",
            "target": "skills",
            "command": None,
            "x": None,
            "y": None,
            "dest_x": None,
            "dest_y": None,
            "right_click": False,
            "double_click": False,
            "wait_ms": 1200,
            "expected_ui_and_state_change": "Character Sheet / Skills window opens in open_windows",
        },
    }


def _heuristic_fallback_decision() -> Dict[str, Any]:
    return {
        "observation": "Fallback heuristic step (agy CLI call timed out or returned non-JSON).",
        "previous_action_verification": {
            "expectation_met": True,
            "discrepancy_summary": "",
        },
        "reasoning": "Perform a safe exploration step on the Neocom while preserving session continuity.",
        "current_goal": "Keep exploration loop advancing across core windows",
        "action": {
            "type": "open_neocom",
            "target": "skills",
            "command": None,
            "x": None,
            "y": None,
            "dest_x": None,
            "dest_y": None,
            "right_click": False,
            "double_click": False,
            "wait_ms": 1200,
            "expected_ui_and_state_change": "Character Sheet / Skills window opens in open_windows",
        },
        "memory_update": {
            "working_memory": ["Used heuristic fallback step due to agy CLI timeout."],
            "long_term_note_to_append": "",
            "mark_tested_features": [],
            "mark_buggy_features": [],
            "new_frontier_features": [],
        },
        "bug_report": {
            "should_file": False,
            "category": "none",
            "title": "",
            "severity": "medium",
            "reproduction_steps": [],
            "expected_behavior": "",
            "actual_behavior": "",
            "subsystem": "",
        },
    }


_DIRECT_NON_WIDGET_ACTIONS = frozenset({
    "slash_command",
    "open_neocom",
    "close_window",
    "undock",
    "dock",
    "jump_gate",
    "warp_to",
    "align",
    "align_to",
    "stop",
    "approach",
    "orbit",
    "keep_at_range",
    "lock_target",
    "activate_module",
    "select_character",
    "select_overview",
    "save_location",
    "warp_to_location",
    "warp_to_bookmark",
    "set_destination",
    "engage_autopilot",
    "toggle_autopilot",
    "train_skill",
    "complete_skills",
    "advance_upwell_timer",
    "complete_industry_jobs",
    "advance_server_time",
    "clear_timers",
    "press_key",
    "wait",
    "open_plex_store",
    "open_structure_cosmetics",
})


def _find_widget_in_pools(
    target: str,
    search_pools: Tuple[List[Dict[str, Any]], ...],
    allow_multi_sub: bool = False,
) -> Optional[Dict[str, Any]]:
    """Match a widget target string against candidate `clickable_widgets` pools."""
    t_low = target.lower()
    for pool in search_pools:
        for key in ("path", "name", "label", "text"):
            for w in pool:
                if str(w.get(key) or "") == target:
                    return w
        for w in pool:
            p = str(w.get("path") or "")
            n = str(w.get("name") or "")
            if p.lower() == t_low or n.lower() == t_low or p.lower().endswith("/" + t_low):
                return w
        sub_hits = [
            w
            for w in pool
            if t_low in str(w.get("path") or "").lower()
            or t_low in str(w.get("name") or "").lower()
            or t_low in str(w.get("label") or w.get("text") or "").lower()
        ]
        if len(sub_hits) == 1 or (sub_hits and allow_multi_sub):
            return sub_hits[0]
    return None


def _parse_coord_int(val: Any) -> Optional[int]:
    """Parse a finite integer coordinate, rejecting None, bools, and non-numeric values."""
    if isinstance(val, bool) or val is None:
        return None
    if isinstance(val, int):
        return val
    if isinstance(val, float):
        return int(val) if math.isfinite(val) else None
    if isinstance(val, str):
        s = val.strip()
        if not s:
            return None
        try:
            num = float(s)
        except ValueError:
            return None
        return int(num) if math.isfinite(num) else None
    return None


def _resolve_direct_action(
    step_spec: Dict[str, Any],
    ui_snapshot: Optional[Dict[str, Any]],
) -> Tuple[Optional[Dict[str, Any]], str]:
    """
    Resolve a planned step cleanly into a concrete executable action without an Executor LLM call.
    Returns `(action_dict, "")` on success, or `(None, miss_reason)` to trigger warm LLM fallback.
    """
    if not isinstance(step_spec, dict):
        return None, "missing step spec"
    raw_act = dict(step_spec.get("action") or {})
    raw_type = str(raw_act.get("type") or step_spec.get("action_type_hint") or step_spec.get("action_type") or "").strip()
    target = str(raw_act.get("target") or step_spec.get("target_or_command_hint") or step_spec.get("target_hint") or "").strip()
    act_type = _canonicalize_action_type(raw_type, bool(target))
    if not act_type:
        return None, "missing action type"
    optional_flags = _extract_optional_action_flags(raw_act)
    command = _coerce_action_command(raw_act)
    if command in (None, "") and not optional_flags.get("clear") and act_type in ("slash_command", "type_text"):
        hint_cmd = str(step_spec.get("target_or_command_hint") or step_spec.get("target_hint") or "").strip()
        if hint_cmd:
            command = hint_cmd

    act: Dict[str, Any] = {
        "type": act_type,
        "target": target or None,
        "command": command,
        "x": raw_act.get("x"),
        "y": raw_act.get("y"),
        "dest_x": raw_act.get("dest_x"),
        "dest_y": raw_act.get("dest_y"),
        "right_click": bool(raw_act.get("right_click", False)),
        "double_click": bool(raw_act.get("double_click", False)),
        "wait_ms": _resolve_wait_action_ms(
            act_type, target or None, command, raw_act.get("wait_ms")
        ),
        "expected_ui_and_state_change": str(
            raw_act.get("expected_ui_and_state_change")
            or step_spec.get("expected_outcome")
            or step_spec.get("expected_ui_and_state_change")
            or ""
        ),
        **optional_flags,
    }

    if act_type in _DIRECT_NON_WIDGET_ACTIONS:
        if act_type == "slash_command":
            cmd_str = str(act.get("command") or act.get("target") or "").strip()
            if not cmd_str.startswith("/"):
                return None, f"slash_command missing leading '/': {cmd_str!r}"
            act["command"] = cmd_str
        elif act_type == "open_neocom" and not act.get("target"):
            return None, "open_neocom missing target"
        return act, ""

    if act_type == "click_xy":
        if act.get("x") is None or act.get("y") is None:
            return None, "click_xy missing x/y coordinates"
        return act, ""

    if act_type in ("click_widget", "type_text", "drag_widget"):
        if not target:
            if act_type == "click_widget" and act.get("x") is not None and act.get("y") is not None:
                return act, ""
            return None, f"{act_type} missing widget target"
        ui_sub = (ui_snapshot or {}).get("ui") or {}
        menu_items = list(ui_sub.get("context_menu") or (ui_snapshot or {}).get("context_menu") or [])
        widgets = [w for w in (ui_sub.get("clickable_widgets") or []) if isinstance(w, dict)]
        t_low = target.lower()

        # Context-menu entries take priority when menu is open or target uses context_menu_ prefix
        if act_type != "drag_widget":
            for entry in menu_items:
                if not isinstance(entry, dict):
                    continue
                ename = str(entry.get("name") or "")
                elabel = str(entry.get("label") or entry.get("text") or "")
                if ename == target or elabel == target or ename.lower() == t_low or elabel.lower() == t_low or (
                    len(t_low) >= 4 and (t_low in ename.lower() or t_low in elabel.lower())
                ):
                    if ename:
                        act["target"] = ename
                    if entry.get("x") is not None and entry.get("y") is not None:
                        act["x"] = entry["x"]
                        act["y"] = entry["y"]
                    return act, ""

        # Strict match cascade on live clickable_widgets
        if act_type == "type_text":
            search_pools: Tuple[List[Dict[str, Any]], ...] = (
                [w for w in widgets if w.get("editable")],
                widgets,
            )
        else:
            search_pools = (widgets,)

        matched = _find_widget_in_pools(
            target, search_pools, allow_multi_sub=(act_type == "type_text")
        )
        if matched is None:
            return None, f"widget {target!r} not found in live scene graph"

        if matched.get("path"):
            act["target"] = str(matched["path"])
        elif matched.get("name"):
            act["target"] = str(matched["name"])
        if matched.get("x") is not None and matched.get("y") is not None:
            act["x"] = matched["x"]
            act["y"] = matched["y"]

        if act_type == "drag_widget":
            raw_dx = act.get("dest_x")
            raw_dy = act.get("dest_y")
            dx_val = _parse_coord_int(raw_dx) if raw_dx is not None else None
            dy_val = _parse_coord_int(raw_dy) if raw_dy is not None else None
            if (raw_dx is not None and dx_val is None) or (raw_dy is not None and dy_val is None):
                return None, "drag_widget invalid dest_x/dest_y coordinates"
            dest_target = str(
                raw_act.get("dest_target")
                or raw_act.get("dest_widget")
                or ""
            ).strip()
            if (dx_val is None or dy_val is None or (dx_val <= 0 and dy_val <= 0)) and dest_target:
                dest_matched = _find_widget_in_pools(dest_target, (widgets,))
                if dest_matched is None:
                    return None, f"drag_widget destination widget {dest_target!r} not found in live scene graph"
                dx_val = _parse_coord_int(dest_matched.get("x"))
                dy_val = _parse_coord_int(dest_matched.get("y"))
                if dx_val is None or dy_val is None:
                    return None, f"drag_widget destination widget {dest_target!r} missing coordinates"
            if dx_val is None or dy_val is None:
                return None, "drag_widget missing dest_x/dest_y drop coordinates"
            if dx_val < 0 or dy_val < 0 or (dx_val == 0 and dy_val == 0):
                return None, "drag_widget missing positive dest_x/dest_y drop coordinates"
            act["dest_x"] = dx_val
            act["dest_y"] = dy_val

        return act, ""

    return None, f"unsupported direct action type {act_type!r}"


_CHAT_WIN_MARKERS = ("chatwindowstack", "chatchannel", "xmppchat", "lscchannel", "channelwindow")

_WINDOW_ASSERTION_ALIASES = {
    "people": ("people", "peopleandplaces", "addressbook", "contacts"),
    "peopleandplaces": ("peopleandplaces", "people", "addressbook", "contacts"),
    "addressbook": ("addressbook", "contacts", "peopleandplaces", "people"),
    "contacts": ("contacts", "addressbook", "peopleandplaces", "people"),
    "store": ("store", "newedenstore", "aurumstore", "virtualgoodsstore"),
    "nes": ("newedenstore", "aurumstore", "virtualgoodsstore", "store"),
    "newedenstore": ("newedenstore", "aurumstore", "virtualgoodsstore", "store"),
}


def _target_requests_chat_window(target: str) -> bool:
    tl = str(target or "").strip().lower()
    return any(tok in tl for tok in ("chat", "chatchannel", "lsc", "channel"))


def _is_chat_window_hay(hay: str) -> bool:
    hl = str(hay or "").lower()
    return any(tok in hl for tok in _CHAT_WIN_MARKERS)


def _window_hay_matches_target(target: str, hay: str) -> bool:
    """Return True if window descriptor `hay` matches `target` (ignoring chat unless targeted)."""
    tl = str(target or "").strip().lower()
    if not tl:
        return True
    if not _target_requests_chat_window(tl) and _is_chat_window_hay(hay):
        return False
    t_norm = tl.replace("_", "").replace("-", "").replace(" ", "")
    hay_l = str(hay or "").lower()
    hay_norm = hay_l.replace("_", "").replace("-", "").replace(" ", "")
    tokens = _WINDOW_ASSERTION_ALIASES.get(t_norm, (tl,))
    for tok in tokens:
        tok_norm = tok.replace("_", "").replace("-", "").replace(" ", "")
        if tok in hay_l or (tok_norm and tok_norm in hay_norm):
            return True
    return False


def _evaluate_step_assertions(
    assertions: Optional[Dict[str, Any]],
    exec_res: Dict[str, Any],
    post_state: Optional[Dict[str, Any]],
) -> Tuple[bool, str]:
    """Evaluate fast Python step assertions (`< 1ms`) against post-action telemetry & scene graph."""
    if not assertions or not isinstance(assertions, dict):
        return True, ""
    state = post_state if isinstance(post_state, dict) else {}
    ui_sub = state.get("ui") or {}
    open_wins = list(ui_sub.get("open_windows") or state.get("open_windows") or [])
    sdiff = (exec_res.get("state_diff") or {}) if isinstance(exec_res, dict) else {}

    def _win_entries(wins: List[Any]) -> List[Tuple[str, str]]:
        out: List[Tuple[str, str]] = []
        for w in wins:
            if isinstance(w, dict):
                label = str(w.get("window_id") or w.get("name") or w.get("caption") or w.get("cls") or "")
                hay = " ".join([
                    str(w.get("window_id") or ""),
                    str(w.get("name") or ""),
                    str(w.get("caption") or ""),
                    str(w.get("cls") or ""),
                ]).lower()
                if label or hay.strip():
                    out.append((label or hay.strip(), hay))
            else:
                s = str(w or "")
                if s:
                    out.append((s, s.lower()))
        return out

    win_entries = _win_entries(open_wins)
    win_strs = [lbl for lbl, _ in win_entries]
    opened_strs = [str(x or "") for x in (sdiff.get("windows_opened") or [])]

    want_open = assertions.get("expect_window_open")
    if want_open:
        found = any(_window_hay_matches_target(str(want_open), hay) for _, hay in win_entries) or any(
            _window_hay_matches_target(str(want_open), s) for s in opened_strs
        )
        if not found:
            return False, f"assertion expect_window_open={want_open!r} failed (open_windows={win_strs})"

    want_closed = assertions.get("expect_window_closed")
    if want_closed:
        still_open = [
            lbl
            for lbl, hay in win_entries
            if _window_hay_matches_target(str(want_closed), hay)
        ]
        if still_open:
            return False, f"assertion expect_window_closed={want_closed!r} failed (still open: {still_open})"

    if assertions.get("expect_modal_absent"):
        modals = list(ui_sub.get("modals") or state.get("modals") or [])
        if modals:
            return False, f"assertion expect_modal_absent failed (modals={modals})"

    want_ctx = assertions.get("expect_context_menu_item")
    if want_ctx:
        ml = str(want_ctx).strip().lower()
        ctx = list(ui_sub.get("context_menu") or state.get("context_menu") or [])
        ctx_strs = [
            f"{c.get('name', '')} {c.get('label', '')} {c.get('text', '')}".lower()
            if isinstance(c, dict)
            else str(c or "").lower()
            for c in ctx
        ]
        if not any(ml in cs for cs in ctx_strs):
            return False, f"assertion expect_context_menu_item={want_ctx!r} failed (context_menu={ctx_strs})"

    want_ball = assertions.get("expect_ball_present")
    if want_ball:
        bl = str(want_ball).strip().lower()
        balls = list(((state.get("space") or {}).get("nearby_balls")) or [])
        ball_strs = [
            f"{b.get('item_id', '')} {b.get('type_id', '')} {b.get('name', '')}".lower()
            if isinstance(b, dict)
            else str(b or "").lower()
            for b in balls
        ]
        if not any(bl in bs for bs in ball_strs):
            return False, f"assertion expect_ball_present={want_ball!r} failed"

    want_sess = assertions.get("expect_session_field")
    if want_sess:
        sess = state.get("session") or {}
        space = state.get("space") or {}

        def _lookup_session_or_space(key: str) -> Any:
            k_clean = key.strip().strip("'\"")
            if k_clean.startswith("session."):
                return sess.get(k_clean.split(".", 1)[1])
            if k_clean.startswith("space."):
                return space.get(k_clean.split(".", 1)[1])
            if k_clean in sess:
                return sess.get(k_clean)
            if k_clean in space:
                return space.get(k_clean)
            if k_clean == "in_space":
                return bool(sess.get("solarsystemid2") and not sess.get("stationid"))
            if k_clean in state:
                return state.get(k_clean)
            return None

        def _matches_expected(actual: Any, expected: Any) -> bool:
            if expected is None or str(expected).strip().lower() == "none":
                return actual is None
            if isinstance(expected, bool):
                return bool(actual) is expected if actual is not None else False
            exp_s = str(expected).strip()
            if exp_s.lower() in ("true", "false") and isinstance(actual, bool):
                return actual is (exp_s.lower() == "true")
            return str(actual) == exp_s

        parsed_dict: Optional[Dict[str, Any]] = None
        if isinstance(want_sess, dict):
            parsed_dict = want_sess
        else:
            expr = str(want_sess).strip()
            if expr.startswith("{") and expr.endswith("}"):
                import ast

                try:
                    cand = ast.literal_eval(expr)
                    if isinstance(cand, dict):
                        parsed_dict = cand
                except (SyntaxError, ValueError):
                    parsed_dict = None

        if parsed_dict is not None:
            for k, expected in parsed_dict.items():
                actual = _lookup_session_or_space(str(k))
                if not _matches_expected(actual, expected):
                    return False, f"assertion expect_session_field={want_sess!r} failed (field={k!r}, actual={actual!r})"
        elif "!=" in str(want_sess):
            expr = str(want_sess).strip()
            k, v = [p.strip() for p in expr.split("!=", 1)]
            actual = _lookup_session_or_space(k)
            if _matches_expected(actual, v):
                return False, f"assertion expect_session_field={expr!r} failed (actual={actual!r})"
        elif "==" in str(want_sess):
            expr = str(want_sess).strip()
            k, v = [p.strip() for p in expr.split("==", 1)]
            actual = _lookup_session_or_space(k)
            if not _matches_expected(actual, v):
                return False, f"assertion expect_session_field={expr!r} failed (actual={actual!r})"
        else:
            expr = str(want_sess).strip()
            actual = _lookup_session_or_space(expr)
            if not actual:
                return False, f"assertion expect_session_field={expr!r} failed (actual={actual!r})"

    return True, ""


# ---------------------------------------------------------------------------
# Zero-Tolerance Bug Oracle & Incident Bundle Emitter
# ---------------------------------------------------------------------------

_GATEWAY_SERVER_STATUS_RE = re.compile(
    r"\bstatus\s*=\s*(?:2\d\d|400|401|403|404|409|422)\b", re.IGNORECASE
)
_CLIENT_TRANSPORT_FAILURE_RE = re.compile(
    r"transport|connection|refused|timed?\s*out|deadline|unavailable|channel|"
    r"socket|broken pipe|reset by peer|unreachable|\beof\b|\bdns\b|502\b|503\b|504\b",
    re.IGNORECASE,
)
_QUASAR_LOG_STARTUP_RE = re.compile(
    r"descriptor manifest|listening on|starting quasar|loaded quasar|descriptors loaded",
    re.IGNORECASE,
)


def _read_server_boot_build(worktree_dir: Path) -> Optional[int]:
    """Read `pub const V24_BOOT_BUILD: u32 = ...` from `eve-machonet/src/lib.rs`."""
    candidates = [
        worktree_dir / "crates" / "eve-machonet" / "src" / "lib.rs",
        REPO_ROOT / "crates" / "eve-machonet" / "src" / "lib.rs",
    ]
    for path in candidates:
        try:
            if path.exists():
                text = path.read_text(encoding="utf-8", errors="ignore")
                m = re.search(r"pub\s+const\s+V24_BOOT_BUILD\s*:\s*u32\s*=\s*([0-9_]+)\s*;", text)
                if m:
                    val = int(m.group(1).replace("_", ""))
                    if val > 0:
                        return val
        except Exception:
            pass
    return None


def _read_client_boot_build(staged_app: Optional[Path] = None) -> Optional[int]:
    """Read `build = <id>` from staged client `start.ini` or cache directory name."""
    candidates: List[Path] = []
    if staged_app is not None:
        candidates.extend(
            [
                staged_app / "start.ini",
                staged_app / "Contents" / "Resources" / "start.ini",
            ]
        )
    for ini_path in candidates:
        try:
            if ini_path.exists():
                for line in ini_path.read_text(encoding="utf-8", errors="ignore").splitlines():
                    m = re.match(r"^\s*build\s*=\s*(\d+)\s*$", line)
                    if m:
                        val = int(m.group(1))
                        if val > 0:
                            return val
        except Exception:
            pass
    if staged_app is not None:
        m_dir = re.search(r"client_build_(\d+)", staged_app.name)
        if m_dir:
            val = int(m_dir.group(1))
            if val > 0:
                return val
    return None


def _classify_flight_entry_reach(mc: Dict[str, Any]) -> Tuple[bool, bool, bool]:
    """
    Classify a playtester flight-recorder entry as:
      `(counts_server_request, counts_gateway_request, is_gateway_transport_error)`.
    Distinguishes client-side failed `publicGateway` transport calls from server-reached RPCs.
    """
    kind = str(mc.get("kind") or "").lower()
    svc = str(mc.get("service") or "").lower()
    method = str(mc.get("method") or "").lower()
    explicit_err = str(mc.get("error") or "").strip()
    raw_status = mc.get("status")
    if raw_status is None or not str(raw_status).strip():
        status_str = f"ERR({explicit_err})" if explicit_err else "OK"
    else:
        status_str = str(raw_status).strip()
    err_str = explicit_err or str(mc.get("result") or "").strip()
    combined = f"{status_str} {err_str}".strip()

    is_gateway = (
        kind == "quasar" or "quasar" in svc or "gateway" in svc or "quasar" in method
    )

    if mc.get("transport_error") is True or mc.get("client_transport_failure") is True:
        return (False, False, is_gateway)

    if (status_str.upper().startswith("OK") and not explicit_err) or mc.get("reached_server") is True:
        if svc == "publicgatewaysvc":
            return (True, True, False)
        return (not is_gateway, is_gateway, False)

    if svc in ("quasar_grpc", "publicgatewaysvc") and _CLIENT_TRANSPORT_FAILURE_RE.search(combined):
        return (False, False, True)
    if svc == "quasar_grpc":
        return (False, True, False)
    if svc == "publicgatewaysvc":
        return (True, True, False)

    if is_gateway:
        if _CLIENT_TRANSPORT_FAILURE_RE.search(combined) or not _GATEWAY_SERVER_STATUS_RE.search(
            combined
        ):
            return (False, False, True)
        return (False, True, False)

    if status_str.upper().startswith(("ERR", "FAIL", "TIMEOUT")) and re.search(
        r"connection\s*refused|connection\s*reset|socket\s*error|connect\s*timeout|transport\s*closed",
        combined,
        re.IGNORECASE,
    ):
        return (False, False, False)

    return (True, False, False)


class PlaytesterBugOracle:
    def __init__(
        self,
        worktree_dir: Path,
        bug_queue_dir: Path,
        memory: PlaytesterMemory,
        sync_github: bool = True,
        async_write: bool = False,
    ):
        self.worktree_dir = worktree_dir
        self.bug_queue_dir = bug_queue_dir
        self.memory = memory
        self.sync_github = sync_github
        self.async_write = bool(async_write)
        self._traceback_offset = 0
        self._server_log_offset = 0
        self._last_stale_at_action = 0
        self._seen_tb_sigs: set[str] = set()
        self._prev_step_dropped_all_log_excs: bool = False
        self.last_unseen_tracebacks: List[Dict[str, str]] = []
        self.last_suppressed_tracebacks: List[Dict[str, str]] = []
        self.last_user_error_exemptions: List[Dict[str, Any]] = []
        self._exemption_tracker = _UserErrorExemptionTracker(retain_all=False, max_recent=100)
        self._last_step_ui_state: Optional[Dict[str, Any]] = None
        self._session_server_request_count: int = 0
        self._session_gateway_request_count: int = 0
        self._session_gateway_transport_error_count: int = 0
        self._session_flight_seqs_seen: set = set()
        self._session_unseq_entry_ids: set[int] = set()
        self._session_unseq_entry_refs: List[Dict[str, Any]] = []
        self._session_client_boot_build: Optional[int] = None
        self._session_server_boot_build: Optional[int] = _read_server_boot_build(self.worktree_dir)
        self._write_queue: Optional["queue.Queue[Optional[Tuple[Path, Dict[str, Any], bool]]]"] = None
        self._writer_thread: Optional[threading.Thread] = None
        if self.async_write:
            self._write_queue = queue.Queue()
            self._writer_thread = threading.Thread(target=self._async_writer_loop, daemon=True)
            self._writer_thread.start()

    def reset_session_telemetry(
        self,
        *,
        client_boot_build: Optional[int] = None,
        server_boot_build: Optional[int] = None,
    ) -> None:
        self._session_server_request_count = 0
        self._session_gateway_request_count = 0
        self._session_gateway_transport_error_count = 0
        self._session_flight_seqs_seen.clear()
        self._last_step_ui_state = None
        if getattr(self, "_exemption_tracker", None) is not None:
            self._exemption_tracker.reset_client_session()
        self._session_unseq_entry_ids.clear()
        self._session_unseq_entry_refs.clear()
        if client_boot_build is not None and client_boot_build > 0:
            self._session_client_boot_build = int(client_boot_build)
        else:
            self._session_client_boot_build = None
        if server_boot_build is not None and server_boot_build > 0:
            self._session_server_boot_build = int(server_boot_build)
        else:
            self._session_server_boot_build = _read_server_boot_build(self.worktree_dir)

    def set_handshake_builds(
        self,
        *,
        client_boot_build: Optional[int] = None,
        server_boot_build: Optional[int] = None,
    ) -> None:
        if client_boot_build is not None and client_boot_build > 0:
            self._session_client_boot_build = int(client_boot_build)
        if server_boot_build is not None and server_boot_build > 0:
            self._session_server_boot_build = int(server_boot_build)

    def record_session_flight_calls(self, calls: Any, *, server_log_chunk: str = "") -> None:
        """Accumulate cumulative session server/gateway RPC counts across steps."""
        step_gateway_reached = 0
        any_gateway_in_batch = False
        if isinstance(calls, list):
            for entry in calls:
                if not isinstance(entry, dict):
                    continue
                _, gw_candidate, _ = _classify_flight_entry_reach(entry)
                if gw_candidate:
                    any_gateway_in_batch = True
                seq_val = entry.get("seq")
                if seq_val is not None:
                    if seq_val in self._session_flight_seqs_seen:
                        continue
                    self._session_flight_seqs_seen.add(seq_val)
                else:
                    eid = id(entry)
                    if eid in self._session_unseq_entry_ids:
                        continue
                    self._session_unseq_entry_ids.add(eid)
                    self._session_unseq_entry_refs.append(entry)
                srv_hit, gw_hit, gw_transport_err = _classify_flight_entry_reach(entry)
                if srv_hit:
                    self._session_server_request_count += 1
                if gw_hit:
                    self._session_gateway_request_count += 1
                    step_gateway_reached += 1
                if gw_transport_err:
                    self._session_gateway_transport_error_count += 1

        if server_log_chunk and step_gateway_reached == 0 and not any_gateway_in_batch:
            srv_quasar_hits = sum(
                1
                for ln in server_log_chunk.splitlines()
                if re.search(
                    r"eve_server::quasar_grpc|quasar_grpc::|/eve_public\.|gateway\.Requests|gateway\.Events|publicGatewaySvc",
                    ln,
                )
                and not _QUASAR_LOG_STARTUP_RE.search(ln)
            )
            if srv_quasar_hits > 0:
                self._session_gateway_request_count += srv_quasar_hits

    @staticmethod
    def _serialize_flight_call(
        mc: Dict[str, Any],
        default_service: str = "-",
        default_method: str = "-",
    ) -> Dict[str, Any]:
        raw_status = str(mc.get("status") or "").strip()
        err_msg = str(mc.get("error") or "").strip()
        if not raw_status:
            raw_status = "ERR" if err_msg else "OK"
        detail = mc.get("error") or mc.get("result") or ""
        entry: Dict[str, Any] = {
            "service": mc.get("service", default_service),
            "method": mc.get("method", default_method),
            "args_summary": mc.get("args", mc.get("args_summary", "()")),
            "status": f"{raw_status}({detail})",
            "duration_micros": int(float(mc.get("duration_ms", 0) or 0) * 1000),
        }
        if mc.get("kind"):
            entry["kind"] = mc["kind"]
        if err_msg:
            entry["error"] = err_msg
        return entry

    def _session_telemetry_payload(
        self, ui_snapshot: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        if isinstance(ui_snapshot, dict):
            snap_build = _coerce_int_client_build(ui_snapshot.get("client_build"))
            if snap_build > 0:
                self._session_client_boot_build = snap_build
        if self._session_server_boot_build is None:
            self._session_server_boot_build = _read_server_boot_build(self.worktree_dir)

        payload: Dict[str, Any] = {
            "server_request_count": self._session_server_request_count,
            "gateway_request_count": self._session_gateway_request_count,
        }
        hs: Dict[str, Any] = {}
        if self._session_client_boot_build:
            hs["client_boot_build"] = self._session_client_boot_build
        if self._session_server_boot_build:
            hs["server_boot_build"] = self._session_server_boot_build
        if hs:
            if self._session_client_boot_build and self._session_server_boot_build:
                hs["build_mismatch"] = (
                    self._session_client_boot_build < self._session_server_boot_build
                )
            payload["handshake"] = hs
        if self._session_gateway_transport_error_count > 0:
            payload["telemetry"] = {
                "server_request_count": self._session_server_request_count,
                "gateway_request_count": self._session_gateway_request_count,
                "gateway_transport_error_count": self._session_gateway_transport_error_count,
            }
        return payload

    def _check_destiny_client_logs(self, run_dir: Path) -> Optional[Dict[str, Any]]:
        """Run tools/classify_client_log.py (--expect parity) on run client logs when present."""
        classifier_path = self.worktree_dir / "tools" / "classify_client_log.py"
        if not classifier_path.exists():
            classifier_path = REPO_ROOT / "tools" / "classify_client_log.py"
        if not classifier_path.exists():
            return None
        candidate_logs: List[Path] = []
        clog = run_dir / "client.log"
        if clog.exists() and clog.stat().st_size > 0:
            candidate_logs.append(clog)
        cdir = run_dir / "client_logs"
        if cdir.is_dir():
            candidate_logs.extend(sorted(p for p in cdir.glob("*.log") if p.stat().st_size > 0))
        if not candidate_logs:
            return None
        try:
            proc = subprocess.run(
                [
                    sys.executable,
                    str(classifier_path),
                    "--expect",
                    "parity",
                    "--allow-empty",
                    "--json",
                    *[str(p) for p in candidate_logs],
                ],
                cwd=str(self.worktree_dir),
                capture_output=True,
                text=True,
                timeout=10,
            )
            if proc.stdout.strip():
                return json.loads(proc.stdout)
        except Exception as exc:
            log_warn(f"classify_client_log.py check failed: {exc}")
        return None

    def _run_sync_script(self) -> None:
        sync_script = self.worktree_dir / "tools" / "sync_bugs_to_github.py"
        if sync_script.exists():
            try:
                subprocess.run(
                    [sys.executable, str(sync_script)],
                    cwd=str(self.worktree_dir),
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
            except Exception as exc:
                log_warn(f"sync_bugs_to_github.py failed: {exc}")

    def _async_writer_loop(self) -> None:
        assert self._write_queue is not None
        while True:
            item = self._write_queue.get()
            if item is None:
                self._write_queue.task_done()
                break
            bundle_path, bundle, should_sync = item
            try:
                bundle_path.parent.mkdir(parents=True, exist_ok=True)
                bundle_path.write_text(json.dumps(bundle, indent=2), encoding="utf-8")
                if should_sync:
                    self._run_sync_script()
            except Exception as exc:
                log_warn(f"Async bug bundle write failed ({bundle_path.name}): {exc}")
            finally:
                self._write_queue.task_done()

    def flush(self, timeout: float = 10.0) -> None:
        if self._write_queue is not None:
            deadline = time.time() + max(0.5, float(timeout))
            while self._write_queue.unfinished_tasks > 0 and time.time() < deadline:
                time.sleep(0.05)

    def reset_log_offsets(self, traceback_log: Path, server_log: Path) -> None:
        self._traceback_offset = traceback_log.stat().st_size if traceback_log.exists() else 0
        self._server_log_offset = server_log.stat().st_size if server_log.exists() else 0
        self._seen_tb_sigs.clear()
        self.last_unseen_tracebacks = []
        self.last_suppressed_tracebacks = []

    _TB_EXC_LINE_RE = re.compile(
        r"^[A-Za-z_][A-Za-z0-9_.]*(?:Error|Exception|Warning|Exit|Interrupt|Iteration|Failure)(?:\s*:.*)?$"
    )
    _BUILD_PATH_PREFIX_RE = re.compile(
        r"(?:/Users/[^/]+/(?:[^\"'\s]+/)?)?(?:work/[0-9a-fA-F]+/)?eve/release/V\d+\.\d+/",
        re.IGNORECASE,
    )
    _TB_FILE_FRAME_RE = re.compile(
        r'File\s+"([^"]+)",\s+line\s+(\d+)(?:,\s+in\s+(\S+))?'
    )

    @classmethod
    def _strip_build_path_prefix(cls, text: str) -> str:
        return cls._BUILD_PATH_PREFIX_RE.sub("", str(text or ""))

    @classmethod
    def _format_concise_frame(cls, frame_line: str) -> str:
        norm = cls._strip_build_path_prefix(frame_line).strip()
        m = cls._TB_FILE_FRAME_RE.search(norm)
        if m:
            fpath, lineno, func = m.group(1), m.group(2), m.group(3) or "?"
            # Preserve the meaningful relative module path (or last 3 path parts)
            parts = [p for p in fpath.replace("\\", "/").split("/") if p]
            short_path = "/".join(parts[-4:]) if len(parts) > 4 else "/".join(parts)
            return f'File "{short_path}", line {lineno}, in {func}'
        return norm

    @staticmethod
    def _is_tb_noise_line(ln: str) -> bool:
        s = str(ln or "").strip()
        if not s:
            return True
        if s in ("EXCEPTION END", "Traceback (most recent call last):", "STACKTRACE", "STACKTRACE:"):
            return True
        if s.startswith(("EXCEPTION #", "Common path prefix =", "Reported from", "Local variables", "Thread Locals")):
            return True
        if "evomu_site.py" in s or "evomu_playtester_hook.py" in s or "logmodule/__init__.py" in s:
            return True
        return False

    @classmethod
    def _extract_tb_headline(cls, tb_txt: str, default: str = "Unhandled client Python exception") -> str:
        stripped = sync_bugs_to_github._strip_trace_locals_and_end(str(tb_txt or ""))
        primary_lines, callstack_lines = sync_bugs_to_github._split_trace_sections(stripped)
        base_lines = primary_lines if primary_lines else callstack_lines
        lines = [
            cls._strip_build_path_prefix(ln.strip())
            for ln in base_lines
            if ln.strip()
        ]
        non_noise = [
            ln for ln in lines
            if not cls._is_tb_noise_line(ln)
            and "logmodule" not in ln
            and not ln.startswith("STACKTRACE")
        ]
        for ln in reversed(non_noise):
            if cls._TB_EXC_LINE_RE.match(ln):
                return ln[:200]
        for ln in reversed(non_noise):
            if "Error:" in ln or "Exception:" in ln:
                return ln[:200]
        if non_noise:
            tail = non_noise[-1]
            if 'File "' in tail:
                return cls._format_concise_frame(tail)[:200]
            return tail[:200]
        return default

    _STDLOG_FRAME_RE = re.compile(r"\.py[cwj]?\(\d+\)\s+\S+")

    @classmethod
    def _extract_tb_frames(cls, tb_txt: str) -> Tuple[List[str], List[str]]:
        raw_frames = [
            cls._strip_build_path_prefix(ln.strip())
            for ln in str(tb_txt or "").splitlines()
            if 'File "' in ln or cls._STDLOG_FRAME_RE.search(ln)
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
        return raw_frames, game_frames

    @classmethod
    def _tb_frame_sig(cls, tb_txt: str) -> str:
        raw_frames, game_frames = cls._extract_tb_frames(tb_txt)
        chosen = game_frames or raw_frames
        exc_lines = [
            cls._strip_build_path_prefix(ln.strip())
            for ln in str(tb_txt or "").splitlines()
            if ln.strip()
            and not cls._is_tb_noise_line(ln)
            and (cls._TB_EXC_LINE_RE.match(ln.strip()) or "Error:" in ln or "Exception:" in ln)
        ]
        exc_tail = exc_lines[-1][:140] if exc_lines else ""
        if chosen:
            frame_part = "|".join(chosen[-2:])
            if exc_tail and exc_tail not in frame_part:
                return f"{frame_part}|{exc_tail}"
            return frame_part
        if exc_tail:
            return exc_tail
        return cls._strip_build_path_prefix(str(tb_txt or "")).strip()[:140]

    @classmethod
    def _canonical_client_tb_sig(
        cls, tb_txt: str, detail: Optional[Dict[str, Any]] = None
    ) -> str:
        """Canonical signature for deduplicating mirrored client traceback exemptions."""
        norm_lines = [
            (ln[len("[evomu_site] "):] if ln.startswith("[evomu_site] ") else ln)
            for ln in str(tb_txt or "").splitlines()
        ]
        norm_tb = "\n".join(norm_lines).strip()
        sig = cls._tb_frame_sig(norm_tb) if norm_tb else ""
        key_low = str((detail or {}).get("key") or "unknown").lower()
        if sig:
            sig_norm = re.sub(r"\s+", " ", sig.strip().lower())
            return f"{key_low}|{sig_norm}"
        det_txt = re.sub(r"\s+", " ", str((detail or {}).get("detail") or "").strip().lower())[:140]
        return f"{key_low}|{det_txt}" if det_txt else key_low

    @classmethod
    def _format_tb_summary(cls, headline: str, tb_txt: str) -> str:
        hl = cls._strip_build_path_prefix(str(headline or "")).strip()
        raw_frames, game_frames = cls._extract_tb_frames(tb_txt)
        chosen = game_frames or raw_frames
        if chosen:
            tail_frame = cls._format_concise_frame(chosen[-1])
            if tail_frame and tail_frame not in hl:
                return f"{hl} ({tail_frame})" if hl else tail_frame
        return hl

    @classmethod
    def parse_tb_blocks(cls, raw_tb: str) -> List[Dict[str, str]]:
        """Parse traceback blocks cleanly into `{headline, trace, sig}` dicts without mutating instance state."""
        if not raw_tb:
            return []
        blocks: List[List[str]] = []
        cur: List[str] = []
        ended_prev = False
        prev_nonempty = ""
        in_callstack = False
        cur_post_frame_exc = False
        for ln in raw_tb.splitlines():
            s = ln.strip()
            s_hdr = ln[len("[evomu_site] "):] if ln.startswith("[evomu_site] ") else ln
            cur_has_frames = any('File "' in x or cls._STDLOG_FRAME_RE.search(x) for x in cur)
            is_chained_tb = prev_nonempty.startswith(
                (
                    "INNER TRACEBACK:",
                    "During handling of the above exception",
                    "The above exception was the direct cause",
                )
            )
            is_header = (
                s_hdr.startswith("log.Log")
                or s_hdr.startswith("logmodule.")
                or s_hdr.startswith("service.CoreMixIn.")
                or s_hdr.startswith("blue.pyos.exceptionHandler:")
                or s_hdr.startswith("stdlogutils.")
                or s_hdr.startswith("sys.excepthook:")
                or s_hdr.startswith("Action tasklet exception")
                or s_hdr.startswith("schedule_startup failed:")
                or s_hdr.startswith("EXCEPTION #")
                or (s_hdr.startswith("STACKTRACE") and cur_has_frames)
                or (
                    s_hdr.startswith("Traceback (most recent call last):")
                    and cur_has_frames
                    and cur_post_frame_exc
                    and not is_chained_tb
                )
                or (ended_prev and bool(s))
            )
            if is_header and cur:
                blocks.append(cur)
                cur = [ln]
                cur_has_frames = 'File "' in ln or bool(cls._STDLOG_FRAME_RE.search(ln))
                ended_prev = False
                in_callstack = False
                cur_post_frame_exc = False
            else:
                cur.append(ln)
            if s == "CALLSTACK:":
                in_callstack = True
                if cur_has_frames:
                    cur_post_frame_exc = True
            elif 'File "' in ln or cls._STDLOG_FRAME_RE.search(ln):
                if not in_callstack:
                    cur_post_frame_exc = False
                elif cur_has_frames:
                    cur_post_frame_exc = True
            elif cur_has_frames and not is_chained_tb and s and (
                in_callstack
                or s in ("None", "NoneType: None")
                or s.startswith(("Thread Locals:", "Local variables:"))
                or (
                    not s_hdr[:1].isspace()
                    and re.match(r"^(?:[A-Za-z_][A-Za-z0-9_.]*(?:Error|Exception|Warning|Exit|Interrupt)|UserError)\b", s_hdr)
                )
            ):
                cur_post_frame_exc = True
            if s == "EXCEPTION END":
                ended_prev = True
            elif s:
                ended_prev = False
            if s:
                prev_nonempty = s
        if cur:
            blocks.append(cur)
        parsed: List[Dict[str, str]] = []
        for blk_lines in blocks:
            blk_txt = "\n".join(blk_lines).strip()
            if not blk_txt:
                continue
            has_tb_marker = any(tok in blk_txt for tok in ("Traceback", 'File "', "Error:", "Exception:")) or any(
                cls._TB_EXC_LINE_RE.match(ln.strip()) for ln in blk_lines if not cls._is_tb_noise_line(ln)
            )
            if not has_tb_marker:
                continue
            sig = cls._tb_frame_sig(blk_txt)
            headline = cls._extract_tb_headline(blk_txt)
            tb_excerpt = (
                blk_txt[:1400] + "\n... [truncated] ...\n" + blk_txt[-2500:]
                if len(blk_txt) > 4000
                else blk_txt
            )
            parsed.append({"headline": headline, "trace": tb_excerpt, "sig": sig})
        return parsed

    def _extract_unseen_tb_blocks(self, new_tb: str) -> List[Dict[str, str]]:
        unseen: List[Dict[str, str]] = []
        suppressed: List[Dict[str, str]] = []
        for blk in self.parse_tb_blocks(new_tb):
            sig = blk["sig"]
            headline = blk["headline"]
            if sig and sig in self._seen_tb_sigs:
                suppressed.append({"headline": headline[:160], "sig": sig[:160]})
                continue
            if sig:
                self._seen_tb_sigs.add(sig)
            unseen.append({"headline": headline, "trace": blk["trace"]})
        self.last_suppressed_tracebacks = suppressed
        return unseen

    def read_new_lines(self, path: Path, attr_name: str) -> str:
        if not path.exists():
            return ""
        size = path.stat().st_size
        offset = getattr(self, attr_name, 0)
        if size < offset:
            offset = 0
        if size == offset:
            return ""
        with path.open("rb") as f:
            f.seek(offset)
            chunk = f.read(size - offset)
        setattr(self, attr_name, size)
        return chunk.decode("utf-8", errors="replace")

    @classmethod
    def extract_user_error_key(cls, err_obj: Any) -> Optional[str]:
        return sync_bugs_to_github.extract_user_error_key(err_obj)

    @classmethod
    def extract_client_user_error_key(cls, err_obj: Any) -> Optional[str]:
        return sync_bugs_to_github.extract_client_user_error_key(err_obj)

    @classmethod
    def _incident_user_error_key(cls, inc: Dict[str, Any], side: str) -> Optional[str]:
        return sync_bugs_to_github._extract_bundle_side_user_error_key(inc, side)

    @classmethod
    def _merge_same_step_macho_client_user_errors(
        cls,
        incidents: List[Dict[str, Any]],
    ) -> Tuple[List[Dict[str, Any]], set]:
        """Absorb client_traceback incidents into same-step macho protocol_error incidents with matching UserError key."""
        macho_by_key: Dict[str, Dict[str, Any]] = {}
        for inc in incidents:
            if inc.get("kind") == "protocol_error" and inc.get("subsystem") == "macho":
                m_key = cls._incident_user_error_key(inc, "macho")
                if m_key:
                    macho_by_key.setdefault(m_key.lower(), inc)

        if not macho_by_key:
            return incidents, set()

        kept: List[Dict[str, Any]] = []
        absorbed_keys: set = set()
        for inc in incidents:
            if inc.get("kind") == "client_traceback" and inc.get("subsystem") == "client-python":
                c_key = cls._incident_user_error_key(inc, "client")
                if c_key and c_key.lower() in macho_by_key:
                    target = macho_by_key[c_key.lower()]
                    merged_summary, merged_trace = sync_bugs_to_github.merge_macho_client_user_error_fields(
                        str(target.get("summary") or ""),
                        str(target.get("traceback") or ""),
                        str(inc.get("summary") or ""),
                        str(inc.get("traceback") or ""),
                        c_key,
                        normalize_tb=cls._strip_build_path_prefix,
                    )
                    target["summary"] = merged_summary
                    target["traceback"] = (
                        cls._strip_build_path_prefix(merged_trace) if merged_trace else ""
                    )
                    absorbed_keys.add(c_key.lower())
                    continue
            kept.append(inc)
        return kept, absorbed_keys

    @classmethod
    def _prev_tb_covered_by_macho_user_errors(
        cls,
        raw_prev_tb: str,
        prev_log_excs: List[Any],
        prev_non_benign_macho_excs: List[Any],
        expected_text: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> bool:
        """Return True iff all non-ignorable prev_tb items match a UserError key in prev_non_benign_macho_excs."""
        macho_keys = {
            k.lower()
            for e in prev_non_benign_macho_excs
            if (k := cls.extract_user_error_key(e))
        }
        if not macho_keys:
            return False

        saw_any = False
        for ple in prev_log_excs:
            saw_any = True
            k = cls.extract_client_user_error_key(ple)
            if not k or k.lower() not in macho_keys:
                return False

        tb_text = str(raw_prev_tb or "").strip()
        if tb_text and not cls.is_ignorable_client_traceback("", tb_text, expected_text, ui_state=ui_state):
            blocks = cls.parse_tb_blocks(tb_text)
            if blocks:
                for blk in blocks:
                    hl = str(blk.get("headline") or "")
                    tr = str(blk.get("trace") or "")
                    if cls.is_ignorable_client_traceback(hl, tr, expected_text, ui_state=ui_state):
                        continue
                    saw_any = True
                    k = cls.extract_client_user_error_key(f"{hl}\n{tr}")
                    if not k or k.lower() not in macho_keys:
                        return False
            else:
                saw_any = True
                k = cls.extract_client_user_error_key(tb_text)
                if not k or k.lower() not in macho_keys:
                    return False

        return saw_any

    EXPECTED_USER_ERROR_KEYWORDS = (
        "usererror",
        "reject",
        "refuse",
        "deny",
        "disallow",
        "restrict",
        "invaliditemtypeforhold",
        "invalidtarget",
        "blocked drop",
    )

    BENIGN_USER_ERROR_MARKERS = (
        "skillprerequisitenotmet",
        "queuecannotplaceskillbeforerequirements",
        "cantdeployblocked",
        "corporationtooyoungtoanchor",
        "couriernotatdestinationstation",
        "industryslotsfull",
        "inventorynotaccessible",
        "fleetcannotdoinstation",
        "targettoofar",
        "targetoutofrange",
        "targetnotlocked",
        "targetnotfound",
        "targetingfailed",
        "warpdistancetooshort",
        "warptooshort",
        "shipalreadyinwarp",
        "shipinwarp",
        "notenoughcpu",
        "notenoughpower",
        "notenoughcapacitor",
        "notenoughcapacitortoonline",
        "moduleoffline",
        "moduleisreloading",
        "agentnotcolocated",
        "notenoughcargospace",
        "notenoughdronebayspace",
        "nodronemanagementabilitiesleft",
        "noammoloaded",
        "insufficientfuel",
        "cannot fire doomsday at own ship",
        "cantactivateinwarp",
        "incompatibleammo",
        "cantfittomany",
        "shipoutofboardingrange",
        "lancefiringlockactive",
        "weaponstimeractive",
        "notenoughspecialbayspace",
        "modulealreadyactive",
        "moduleinreactivationdelay",
        "cantdockwhilemoduleactive",
        "cantmovewhilemoduleactive",
        "warpscrambled",
        "dockingrequestdenied",
        "skillalreadyknown",
        "alreadyhaveshipinstation",
        "itemcannotbecontracted",
        "cannotplaceactiveshipinanotherhangar",
        "fittingvalidationfailed",
        "fittinghasskillprerequisites",
        "cannotunfitinspace",
        "cannotfitinspace",
        "cannotrefitwithweaponstimer",
        "tooclosetostructureorstation",
        "deploymenttooclosetogate",
        "deploymenttooclosetostation",
        "deploymenttooclosetostructure",
        "deploymenttooclosetosametype",
        "deploymenttooclosetocelestial",
        "warpdistancetooclose",
        "outsidedockingperimeter",
        "sessionchangetimeractive",
        "structurenotoperational",
        "structurenotdockable",
        "structureserviceaccessdenied",
        "quantumcorerequired",
        "keepstarpersystemlimit",
        "dockingapproach",
        "commands: [",
        "unknownslashcommand",
        "invalidslashargument",
        "cannotloadnotenoughcharges",
        "invalidtarget",
        "warpdestinationgone",
        "notcloseenoughtojump",
        "windowblockedfromopening",
    )

    _MODULE_FIT_FAILED_DICT_START_RE = re.compile(
        r"(?:UserError\s*:?\s*\(\s*u?['\"]ModuleFitFailed['\"]\s*,\s*"
        r"|msg\s*=\s*u?['\"]?ModuleFitFailed\b[^\r\n{]*,\s*dict\s*=\s*)(?=\{)",
        re.IGNORECASE,
    )
    _MODULE_FIT_MISSING_SKILL_RE = re.compile(
        r"^Missing required skill (\d+) level (\d+) \(trained:\s*(\d+)\)(?:\s|$)",
        re.IGNORECASE,
    )
    _WINDOW_OPEN_FRAME_RE = re.compile(
        r"carbonui[/\\]control[/\\]window\.py(?:\"\s*,\s*line\s+\d+,\s+in\s+|\(\d+\)\s+)(?:open|toggleopenclose)\b",
        re.IGNORECASE,
    )

    @staticmethod
    def _extract_balanced_dict_repr(text: str) -> str:
        """Return balanced `{...}` repr starting at `text[0] == '{'`, respecting Python string quotes."""
        if not text or text[0] != "{":
            return ""
        depth = 0
        quote: Optional[str] = None
        escaped = False
        for idx, ch in enumerate(text):
            if quote is not None:
                if escaped:
                    escaped = False
                elif ch == "\\":
                    escaped = True
                elif ch == quote:
                    quote = None
                continue
            if ch in ("'", '"'):
                quote = ch
            elif ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return text[: idx + 1]
        return ""

    @staticmethod
    def _parse_top_level_dict_str_values(dict_repr: str) -> Dict[str, str]:
        """Parse top-level string key/value pairs from a `{...}` repr without descending into nested dicts."""
        if len(dict_repr) < 2 or dict_repr[0] != "{" or dict_repr[-1] != "}":
            return {}
        try:
            parsed = ast.literal_eval(dict_repr)
        except (ValueError, SyntaxError):
            parsed = None
        if isinstance(parsed, dict):
            return {
                k.lower(): v
                for k, v in parsed.items()
                if isinstance(k, str) and isinstance(v, str)
            }
        inner = dict_repr[1:-1]
        entries: List[str] = []
        depth = 0
        quote: Optional[str] = None
        escaped = False
        start = 0
        for idx, ch in enumerate(inner):
            if quote is not None:
                if escaped:
                    escaped = False
                elif ch == "\\":
                    escaped = True
                elif ch == quote:
                    quote = None
                continue
            if ch in ("'", '"'):
                quote = ch
            elif ch in ("{", "[", "("):
                depth += 1
            elif ch in ("}", "]", ")") and depth > 0:
                depth -= 1
            elif ch == "," and depth == 0:
                entries.append(inner[start:idx])
                start = idx + 1
        entries.append(inner[start:])
        result: Dict[str, str] = {}
        for entry in entries:
            colon_idx = -1
            e_depth = 0
            e_quote: Optional[str] = None
            e_escaped = False
            for idx, ch in enumerate(entry):
                if e_quote is not None:
                    if e_escaped:
                        e_escaped = False
                    elif ch == "\\":
                        e_escaped = True
                    elif ch == e_quote:
                        e_quote = None
                    continue
                if ch in ("'", '"'):
                    e_quote = ch
                elif ch in ("{", "[", "("):
                    e_depth += 1
                elif ch in ("}", "]", ")") and e_depth > 0:
                    e_depth -= 1
                elif ch == ":" and e_depth == 0:
                    colon_idx = idx
                    break
            if colon_idx < 0:
                continue
            try:
                key_obj = ast.literal_eval(entry[:colon_idx].strip())
                val_obj = ast.literal_eval(entry[colon_idx + 1 :].strip())
            except (ValueError, SyntaxError):
                continue
            if isinstance(key_obj, str) and isinstance(val_obj, str):
                result[key_obj.lower()] = val_obj
        return result

    @classmethod
    def _strip_trace_frame_and_header_text(cls, headline: str, trace: str = "") -> str:
        """Return lowercased headline + primary traceback text stripped of locals, callstack, and footer."""
        stripped = sync_bugs_to_github._strip_trace_locals_and_end(str(trace or ""))
        primary_lines, _ = sync_bugs_to_github._split_trace_sections(stripped)
        return (f"{headline or ''}\n" + "\n".join(primary_lines)).lower()

    @classmethod
    def _extract_raised_user_error(cls, headline: str, trace: str = "") -> Tuple[Optional[str], str]:
        """Return `(user_error_key, raised_exc_text)` iff the actual raised exception is a `UserError`."""
        stripped_trace = sync_bugs_to_github._strip_trace_locals_and_end(str(trace or "")).strip()
        if stripped_trace and sync_bugs_to_github._RE_TRACEBACK_INDICATORS.search(stripped_trace):
            primary_lines, _ = sync_bugs_to_github._split_trace_sections(stripped_trace)
            primary_only = "\n".join(primary_lines).strip()
            if not primary_only:
                return None, ""
            exc_class, exc_msg, _ = sync_bugs_to_github.parse_traceback_summary(primary_only, "")
            cls_short = exc_class.split(".")[-1].strip()
            if cls_short == "UserError" or exc_class.strip().lower().startswith("user error"):
                raised = f"{exc_class}: {exc_msg}" if exc_msg else exc_class
                ue_key = cls.extract_client_user_error_key(raised)
                return (ue_key, raised) if ue_key else (None, "")
            if (
                cls_short in sync_bugs_to_github._WRAPPER_EXC_CLASSES
                or not cls_short
                or exc_class.startswith(("blue.pyos.", "logmodule.", "log.", "stdlogutils.", "[evomu_site]"))
            ):
                target = f"{exc_class}: {exc_msg}" if exc_msg else exc_class
                target_ue_key, target_non_ue = sync_bugs_to_github._inspect_client_headline_for_user_error(
                    target
                )
                if target_ue_key and not target_non_ue:
                    return target_ue_key, target
            return None, ""
        # Non-traceback / headline-only contexts (e.g. 2-line tasklet summary where headline carries UserError).
        primary_lines, _ = sync_bugs_to_github._split_trace_sections(stripped_trace)
        primary_text = "\n".join(primary_lines).strip()
        if primary_text:
            p_key, p_non_ue = sync_bugs_to_github._inspect_client_headline_for_user_error(primary_text)
            if p_non_ue:
                return None, ""
            if p_key:
                return p_key, primary_text
        hl_text = str(headline or "").strip()
        if hl_text:
            hl_key, hl_non_ue = sync_bugs_to_github._inspect_client_headline_for_user_error(hl_text)
            if hl_key and not hl_non_ue:
                return hl_key, hl_text
        return None, ""

    @staticmethod
    def _extract_trained_skill_levels(
        ui_state: Optional[Dict[str, Any]]
    ) -> Optional[Dict[str, int]]:
        """Extract `{skill_id_str: trained_level}` from live `ui_state.skills_state`, else `None`."""
        if not isinstance(ui_state, dict):
            return None
        skills_state = ui_state.get("skills_state")
        if isinstance(skills_state, dict) and "trained_skill_levels" in skills_state:
            raw_map = skills_state.get("trained_skill_levels")
        elif "trained_skill_levels" in ui_state:
            raw_map = ui_state.get("trained_skill_levels")
        else:
            return None
        if not isinstance(raw_map, dict) or len(raw_map) == 0:
            return None
        parsed: Dict[str, int] = {}
        for k, v in raw_map.items():
            if isinstance(v, bool):
                return None
            try:
                sk_id = str(int(k))
                lvl = int(v)
            except (TypeError, ValueError):
                return None
            parsed[sk_id] = lvl
        return parsed

    @classmethod
    def _verify_module_fit_failed_precondition(
        cls,
        raw_text: str,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> Tuple[bool, Optional[Dict[str, Any]]]:
        """Exempt `ModuleFitFailed` ONLY when the stated precondition is verified against live state."""
        text = str(raw_text or "")
        if "modulefitfailed" not in text.lower():
            return False, None
        if sync_bugs_to_github._RE_TRACEBACK_INDICATORS.search(text):
            ue_key, raised = cls._extract_raised_user_error("", text)
            if not ue_key or ue_key.lower() != "modulefitfailed" or not raised:
                return False, None
            text = raised
        else:
            outer_key = cls.extract_client_user_error_key(text)
            if not outer_key or outer_key.lower() != "modulefitfailed":
                return False, None
        dict_match = cls._MODULE_FIT_FAILED_DICT_START_RE.search(text)
        if not dict_match:
            return False, None
        dict_repr = cls._extract_balanced_dict_repr(text[dict_match.end() :])
        if not dict_repr:
            return False, None
        top_strs = cls._parse_top_level_dict_str_values(dict_repr)
        name_val = (top_strs.get("modulename") or "").strip()
        reason_val = (top_strs.get("reason") or "").strip()
        if not name_val or not reason_val:
            return False, None

        # Only exempt `Missing required skill ...` when live `skills_state.trained_skill_levels`
        # confirms the skill is genuinely untrained / below the required level. Fail closed on
        # CPU, PG, capacitor, calibration, hardpoint, slot, and generic reasons.
        skill_m = cls._MODULE_FIT_MISSING_SKILL_RE.match(reason_val)
        if not skill_m:
            return False, None
        trained_levels = cls._extract_trained_skill_levels(ui_state)
        if trained_levels is None:
            return False, None
        req_skill_id = int(skill_m.group(1))
        req_level = int(skill_m.group(2))
        reported_trained = int(skill_m.group(3))
        live_trained = trained_levels.get(str(req_skill_id), 0)
        if (
            req_skill_id < 1
            or not (1 <= req_level <= 5)
            or not (0 <= live_trained <= 5)
            or live_trained >= req_level
            or live_trained != reported_trained
        ):
            return False, None

        return True, {
            "key": "ModuleFitFailed",
            "mechanism": "precondition_verified",
            "precondition": "missing_required_skill",
            "verified": True,
            "module_name": name_val,
            "skill_id": req_skill_id,
            "required_level": req_level,
            "trained_level": live_trained,
            "detail": f"{name_val}: {reason_val}"[:240],
        }

    @classmethod
    def classify_ignorable_client_traceback(
        cls,
        headline: str,
        trace: str = "",
        expected_text: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> Tuple[bool, Optional[Dict[str, Any]]]:
        """Return `(ignorable, user_error_exemption_detail)` for a client traceback."""
        blocks = cls.parse_tb_blocks(trace) if trace else []
        if len(blocks) > 1:
            first_detail: Optional[Dict[str, Any]] = None
            for blk in blocks:
                ok_blk, det_blk = cls.classify_ignorable_client_traceback(
                    str(blk.get("headline") or ""),
                    str(blk.get("trace") or ""),
                    expected_text,
                    ui_state=ui_state,
                )
                if not ok_blk:
                    return False, None
                if first_detail is None and det_blk is not None:
                    first_detail = det_blk
            return True, first_detail
        if cls.is_benign_client_traceback(headline, trace):
            return True, None
        is_unhandled, tasklet_det = cls._classify_tasklet_user_error(
            headline, trace, ui_state=ui_state
        )
        if is_unhandled:
            return False, None
        ue_key, raised_exc_text = cls._extract_raised_user_error(headline, trace)
        if not ue_key or not raised_exc_text:
            return False, None
        ok_exp, exp_det = cls.classify_expected_user_error(
            raised_exc_text, expected_text, ui_state=ui_state
        )
        if not ok_exp:
            return False, None
        if tasklet_det is not None:
            merged = dict(tasklet_det)
            if isinstance(exp_det, dict) and exp_det.get("verified"):
                merged["mechanism"] = exp_det.get("mechanism", merged.get("mechanism"))
                merged["verified"] = True
            return True, merged
        return True, exp_det

    @classmethod
    def is_ignorable_client_traceback(
        cls,
        headline: str,
        trace: str = "",
        expected_text: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> bool:
        """Return True iff traceback is a benign client-only race or expected game-rule UserError."""
        return cls.classify_ignorable_client_traceback(
            headline, trace, expected_text, ui_state=ui_state
        )[0]

    @classmethod
    def is_benign_client_traceback(cls, headline: str, trace: str = "") -> bool:
        """Filter known CCP V24 client-only UI layout races unrelated to server state."""
        txt = f"{headline or ''}\n{trace or ''}"
        if (
            "marketbase.py" in txt
            and "in _OnResize" in txt
            and "'NoneType' object has no attribute 'max'" in txt
        ):
            return True
        if (
            "minimap.py" in txt
            and "in _ResizeSlider" in txt
            and "'MapSlider' object has no attribute 'minimumWidth'" in txt
        ):
            return True
        return False

    _TASKLET_SIMPLE_USER_ERROR_RULES = (
        (
            "inventory_move_items",
            ("notenoughcargospace",),
            lambda ue_low, sub_low, txt, cls: (
                "bluepytasklet::_moveitems" in txt or "in _moveitems" in txt
            ),
        ),
        (
            "fitting_validation_failed",
            (),
            lambda ue_low, sub_low, txt, cls: (
                sub_low.startswith("fittingvalidationfailed")
                and any(tok in txt for tok in ("fittingslotcontroller", "invcache", "tryfit", "ondropdata"))
            ),
        ),
        (
            "module_button_activate",
            (
                "noammoloaded",
                "insufficientfuel",
                "notenoughcapacitor",
                "invalidtarget",
                "targetnotlocked",
            ),
            lambda ue_low, sub_low, txt, cls: (
                "modulebutton.onstatechange" in txt and "activateeffect" in txt
            ),
        ),
        (
            "radial_menu_stop_ship",
            ("shipinwarp",),
            lambda ue_low, sub_low, txt, cls: (
                ("radialmenu" in txt or "radialmenuactions.py" in txt)
                and ("stopmyship" in txt or "cmdstopship" in txt)
            ),
        ),
        (
            "window_open",
            ("windowblockedfromopening",),
            lambda ue_low, sub_low, txt, cls: bool(cls._WINDOW_OPEN_FRAME_RE.search(txt)),
        ),
        (
            "module_auto_reload",
            ("cannotloadnotenoughcharges",),
            lambda ue_low, sub_low, txt, cls: (
                "autoreload" in txt and "loadammotypetomodule" in txt
            ),
        ),
        (
            "evemisc_drone_launch",
            ("nodronemanagementabilitiesleft", "notenoughdronebayspace"),
            lambda ue_low, sub_low, txt, cls: (
                "evemisc.py" in txt and ("in raise_" in txt or "function raise_" in txt)
            ),
        ),
    )

    @classmethod
    def _classify_tasklet_user_error(
        cls,
        headline: str,
        trace: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> Tuple[bool, Optional[Dict[str, Any]]]:
        """Classify background tasklet tracebacks as `(is_unhandled_crash, exemption_detail)`."""
        blocks = cls.parse_tb_blocks(trace) if trace else []
        if len(blocks) > 1:
            first_det: Optional[Dict[str, Any]] = None
            for blk in blocks:
                bad_blk, det_blk = cls._classify_tasklet_user_error(
                    str(blk.get("headline") or ""),
                    str(blk.get("trace") or ""),
                    ui_state=ui_state,
                )
                if bad_blk:
                    return True, None
                if first_det is None and det_blk is not None:
                    first_det = det_blk
            return False, first_det

        raw_txt = f"{headline or ''}\n{trace or ''}".lower()
        if "unhandled exception in" not in raw_txt:
            return False, None
        ue_key, raised_exc_text = cls._extract_raised_user_error(headline, trace)
        if not ue_key:
            return True, None
        ue_key_low = ue_key.lower()
        ue_subkey_low = ue_key_low.split(":", 1)[1] if ":" in ue_key_low else ue_key_low
        txt = cls._strip_trace_frame_and_header_text(headline, trace)

        # Fitting drag-drop/slot tasklets raise UserError('ModuleFitFailed', ...) ONLY when the
        # stated precondition is verified against live pilot state (e.g. missing skill).
        if ue_key_low == "modulefitfailed" and any(
            tok in txt
            for tok in (
                "fittingslotcontroller",
                "invcache",
                "tryfit",
                "ondropdata",
                "clientdogmalocation",
                "fitmodule",
            )
        ):
            ok_fit, fit_det = cls._verify_module_fit_failed_precondition(
                raised_exc_text, ui_state=ui_state
            )
            if ok_fit and fit_det is not None:
                enriched = dict(fit_det)
                enriched["frame_rule"] = "fitting_tasklet"
                return False, enriched
            return True, None

        for rule_name, keys, pred in cls._TASKLET_SIMPLE_USER_ERROR_RULES:
            key_match = (not keys) or (ue_key_low in keys) or (ue_subkey_low in keys)
            if key_match and pred(ue_key_low, ue_subkey_low, txt, cls):
                return False, {
                    "key": ue_key,
                    "mechanism": "tasklet_user_error",
                    "frame_rule": rule_name,
                    "verified": False,
                    "detail": raised_exc_text[:240],
                }
        return True, None

    @classmethod
    def _is_unhandled_tasklet_traceback(
        cls,
        headline: str,
        trace: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> bool:
        """Background tasklet exceptions (e.g. autopilot UserError) are real crashes, not benign action rejections."""
        return cls._classify_tasklet_user_error(headline, trace, ui_state=ui_state)[0]

    @staticmethod
    def _build_exemption_hit(
        detail: Dict[str, Any],
        *,
        channel: str,
        step_num: int = 0,
        occ_idx: int = 0,
        seq: Any = None,
        mirror_sig: str = "",
        mirror_occ: int = 0,
        dedup_namespace: str = "",
    ) -> Dict[str, Any]:
        """Build a standardized exemption telemetry entry for `_UserErrorExemptionTracker`."""
        hit = dict(detail)
        hit["channel"] = str(channel or "unknown")
        hit["step"] = int(step_num or 0)
        hit["occ_idx"] = int(occ_idx)
        if isinstance(seq, int) and not isinstance(seq, bool):
            hit["seq"] = int(seq)
        ns = str(dedup_namespace or "").strip()
        if "seq" not in hit and (hit["step"] > 0 or ns):
            key_low = str(hit.get("key") or "unknown").lower()
            sig_clean = str(mirror_sig or "").strip()
            prefix = f"{ns}:" if ns else ""
            if sig_clean:
                sig_digest = hashlib.sha256(sig_clean.encode("utf-8")).hexdigest()[:16]
                hit["dedup_id"] = f"{prefix}step:{hit['step']}:tb:{sig_digest}:{int(mirror_occ)}"
            else:
                hit["dedup_id"] = f"{prefix}step:{hit['step']}:{hit['channel']}:{int(occ_idx)}:{key_low}"
        hit.setdefault("ts", time.time())
        return hit

    @classmethod
    def classify_expected_user_error(
        cls,
        err_obj: Any,
        expected_text: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> Tuple[bool, Optional[Dict[str, Any]]]:
        """Return `(is_expected, exemption_detail)` for a UserError string/dict/traceback."""
        if isinstance(err_obj, dict):
            raw_err = str(
                err_obj.get("error")
                or err_obj.get("trace")
                or err_obj.get("traceback")
                or err_obj.get("message")
                or err_obj.get("msg")
                or err_obj.get("headline")
                or err_obj
            )
        else:
            raw_err = str(err_obj)
        ue_key: Optional[str] = None
        if sync_bugs_to_github._RE_TRACEBACK_INDICATORS.search(raw_err):
            blocks = cls.parse_tb_blocks(raw_err)
            if len(blocks) > 1:
                first_det: Optional[Dict[str, Any]] = None
                for blk in blocks:
                    ok_blk, det_blk = cls.classify_expected_user_error(
                        str(blk.get("trace") or blk.get("headline") or ""),
                        expected_text,
                        ui_state=ui_state,
                    )
                    if not ok_blk:
                        return False, None
                    if first_det is None and det_blk is not None:
                        first_det = det_blk
                return True, first_det
            ue_key, raised = cls._extract_raised_user_error("", raw_err)
            if not ue_key or not raised:
                return False, None
            raw_err = raised
        else:
            ue_key, non_ue = sync_bugs_to_github._inspect_client_headline_for_user_error(raw_err)
            if non_ue:
                return False, None
        err_low = raw_err.lower()
        if "usererror(" not in err_low and "usererror:" not in err_low:
            return False, None
        if "slasherror" in err_low:
            if "'reason'" not in err_low and '"reason"' not in err_low:
                return False, None
            if "not found in sde" in err_low:
                return True, {
                    "key": ue_key or "SlashError",
                    "mechanism": "custom_rule",
                    "verified": False,
                    "detail": raw_err[:240],
                }
        if (
            "structurenotoperational" in err_low
            and "in state 'nocore'" in err_low
            and "cannot be controlled" in err_low
        ):
            return True, {
                "key": ue_key or "StructureNotOperational",
                "mechanism": "custom_rule",
                "verified": False,
                "detail": raw_err[:240],
            }
        if "modulefitfailed" in err_low:
            if not ue_key or ue_key.lower() != "modulefitfailed":
                return False, None
            return cls._verify_module_fit_failed_precondition(raw_err, ui_state=ui_state)
        expected_low = (expected_text or "").lower()
        for kw in cls.EXPECTED_USER_ERROR_KEYWORDS:
            if kw in expected_low:
                return True, {
                    "key": ue_key or "UserError",
                    "mechanism": "expected_keyword",
                    "marker": kw,
                    "verified": False,
                    "detail": raw_err[:240],
                }
        for m in cls.BENIGN_USER_ERROR_MARKERS:
            if m in err_low:
                return True, {
                    "key": ue_key or m,
                    "mechanism": "benign_marker",
                    "marker": m,
                    "verified": False,
                    "detail": raw_err[:240],
                }
        return False, None

    @classmethod
    def is_expected_user_error(
        cls,
        err_obj: Any,
        expected_text: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
    ) -> bool:
        return cls.classify_expected_user_error(
            err_obj, expected_text, ui_state=ui_state
        )[0]

    @classmethod
    def _extract_log_exception_parts(
        cls, entry: Any, fallback_trace: str = ""
    ) -> Tuple[str, str, str]:
        """Return `(headline, trace, full_tb)` for a `log_exceptions` dict or raw string."""
        if isinstance(entry, dict):
            hl = str(entry.get("headline") or "").strip()
            tr = str(entry.get("trace") or "").strip() or fallback_trace
            full_tb = tr or hl or fallback_trace
        else:
            raw_s = str(entry).strip()
            if "\n" in raw_s or raw_s.startswith(("STACKTRACE", "Traceback (most recent call last)")):
                hl = ""
                tr = raw_s
            else:
                hl = raw_s
                tr = fallback_trace
            full_tb = tr or hl or fallback_trace
        if (
            cls._is_tb_noise_line(hl)
            or "\n" in hl
            or hl.startswith(("STACKTRACE", "Traceback (most recent call last)"))
        ):
            hl = ""
        if hl:
            hl = cls._strip_build_path_prefix(hl)
            if 'File "' in hl:
                hl = cls._format_concise_frame(hl)
        if not hl:
            hl = cls._extract_tb_headline(full_tb)
        return hl, tr, full_tb

    @classmethod
    def partition_log_exceptions(
        cls,
        entries: Any,
        expected_text: str = "",
        fallback_trace: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
        exemptions_out: Optional[List[Dict[str, Any]]] = None,
        channel: str = "log_exceptions",
        step_num: int = 0,
        mirror_instances_out: Optional[set] = None,
    ) -> Tuple[List[Any], List[Any]]:
        """Split `log_exceptions` into `(non_benign, ignorable_benign)` via `is_ignorable_client_traceback`."""
        non_benign: List[Any] = []
        benign: List[Any] = []
        mirror_occ: Dict[str, int] = {}
        for idx, entry in enumerate(entries or []):
            hl, _, full_tb = cls._extract_log_exception_parts(entry, fallback_trace=fallback_trace)
            ignorable, detail = cls.classify_ignorable_client_traceback(
                hl, full_tb, expected_text, ui_state=ui_state
            )
            if ignorable:
                benign.append(entry)
                if detail is not None:
                    msig = cls._canonical_client_tb_sig(full_tb, detail)
                    m_occ = mirror_occ.get(msig, 0)
                    mirror_occ[msig] = m_occ + 1
                    if mirror_instances_out is not None:
                        mirror_instances_out.add((msig, m_occ))
                    if exemptions_out is not None:
                        exemptions_out.append(
                            cls._build_exemption_hit(
                                detail,
                                channel=channel,
                                step_num=step_num,
                                occ_idx=idx,
                                mirror_sig=msig,
                                mirror_occ=m_occ,
                            )
                        )
            else:
                non_benign.append(entry)
        return non_benign, benign

    @classmethod
    def _all_entries_match_benign_client_race(
        cls, entries: Any, fallback_trace: str = ""
    ) -> bool:
        """Return True iff `entries` is non-empty and every entry is a known CCP V24 client UI layout race."""
        items = list(entries or [])
        if not items:
            return False
        for entry in items:
            hl, _, full_tb = cls._extract_log_exception_parts(entry, fallback_trace=fallback_trace)
            if not cls.is_benign_client_traceback(hl, full_tb):
                return False
        return True

    @classmethod
    def partition_flight_recorder_exceptions(
        cls,
        flight_recorder: Any,
        expected_text: str = "",
        *,
        ui_state: Optional[Dict[str, Any]] = None,
        step_num: int = 0,
    ) -> Any:
        """Return a non-mutating copy of `flight_recorder` moving benign `macho_exceptions` and `log_exceptions` out of hard lists."""
        if not isinstance(flight_recorder, dict):
            return flight_recorder
        raw_macho = flight_recorder.get("macho_exceptions")
        raw_logs = flight_recorder.get("log_exceptions")
        if not raw_macho and not raw_logs:
            return flight_recorder
        resolved_expected = expected_text or str(
            flight_recorder.get("expected_ui_and_state_change") or ""
        )
        out = dict(flight_recorder)
        exemptions: List[Dict[str, Any]] = list(out.get("user_error_exemptions") or [])
        if raw_macho:
            non_benign_macho: List[Any] = []
            benign_macho: List[Any] = list(out.get("benign_macho_exceptions") or [])
            for idx, exc_item in enumerate(raw_macho):
                ok_ex, det_ex = cls.classify_expected_user_error(
                    exc_item, resolved_expected, ui_state=ui_state
                )
                if ok_ex:
                    benign_macho.append(exc_item)
                    if det_ex is not None:
                        exemptions.append(
                            cls._build_exemption_hit(
                                det_ex,
                                channel="macho_exceptions",
                                step_num=step_num,
                                occ_idx=idx,
                                seq=exc_item.get("seq") if isinstance(exc_item, dict) else None,
                            )
                        )
                else:
                    non_benign_macho.append(exc_item)
            out["macho_exceptions"] = non_benign_macho
            if benign_macho:
                out["benign_macho_exceptions"] = benign_macho
        if raw_logs:
            non_benign_logs, benign_logs = cls.partition_log_exceptions(
                raw_logs,
                expected_text=resolved_expected,
                ui_state=ui_state,
                exemptions_out=exemptions,
                channel="log_exceptions",
                step_num=step_num,
            )
            combined_benign_logs = list(out.get("benign_log_exceptions") or []) + benign_logs
            out["log_exceptions"] = non_benign_logs
            if combined_benign_logs:
                out["benign_log_exceptions"] = combined_benign_logs
        if exemptions:
            out["user_error_exemptions"] = exemptions
        return out

    @classmethod
    def collect_exec_res_user_error_exemptions(
        cls,
        exec_res: Any,
        *,
        expected_text: str = "",
        ui_state: Optional[Dict[str, Any]] = None,
        step_num: int = 0,
        unseen_tracebacks: Optional[List[Dict[str, Any]]] = None,
    ) -> List[Dict[str, Any]]:
        """Collect structured, deduplicated UserError exemption records for a step's `exec_res`."""
        if not isinstance(exec_res, dict):
            return []
        collected: List[Dict[str, Any]] = []
        fr = exec_res.get("flight_recorder")
        if isinstance(fr, dict):
            part = cls.partition_flight_recorder_exceptions(
                fr,
                expected_text=expected_text,
                ui_state=ui_state,
                step_num=step_num,
            )
            if isinstance(part, dict) and isinstance(part.get("user_error_exemptions"), list):
                collected.extend(part["user_error_exemptions"])
            macho_seqs = {
                int(h["seq"])
                for h in collected
                if h.get("channel") == "macho_exceptions"
                and isinstance(h.get("seq"), int)
                and not isinstance(h.get("seq"), bool)
            }
            macho_unseq_keys = {
                str(h.get("key") or "").lower()
                for h in collected
                if h.get("channel") == "macho_exceptions"
                and (not isinstance(h.get("seq"), int) or isinstance(h.get("seq"), bool))
            }
            for idx, call_entry in enumerate(fr.get("macho_calls") or ()):
                if not isinstance(call_entry, dict):
                    continue
                if str(call_entry.get("status") or "").upper() != "ERR":
                    continue
                call_err = call_entry.get("error") or call_entry
                ok_call, det_call = cls.classify_expected_user_error(
                    call_err, expected_text, ui_state=ui_state
                )
                if not ok_call or det_call is None:
                    continue
                call_seq = call_entry.get("seq")
                if isinstance(call_seq, int) and not isinstance(call_seq, bool):
                    if int(call_seq) in macho_seqs:
                        continue
                elif str(det_call.get("key") or "").lower() in macho_unseq_keys:
                    continue
                collected.append(
                    cls._build_exemption_hit(
                        det_call,
                        channel="macho_calls",
                        step_num=step_num,
                        occ_idx=idx,
                        seq=call_seq,
                    )
                )
        unseen_list = list(unseen_tracebacks or ())
        if unseen_list:
            seen_tb_dedup_ids = {
                str(h["dedup_id"])
                for h in collected
                if h.get("channel") == "log_exceptions" and h.get("dedup_id")
            }
            seen_mirror_instances: set = set()
            if (
                step_num <= 0
                and isinstance(fr, dict)
                and (fr.get("log_exceptions") or ())
            ):
                cls.partition_log_exceptions(
                    fr.get("log_exceptions") or (),
                    expected_text=expected_text,
                    ui_state=ui_state,
                    step_num=step_num,
                    mirror_instances_out=seen_mirror_instances,
                )
            tb_mirror_occ: Dict[str, int] = {}
            for idx, tb_item in enumerate(unseen_list):
                if not isinstance(tb_item, dict):
                    continue
                hl = str(tb_item.get("headline") or "")
                tr = str(tb_item.get("trace") or "")
                ignorable, det = cls.classify_ignorable_client_traceback(
                    hl, tr, expected_text, ui_state=ui_state
                )
                if ignorable and det is not None:
                    msig = cls._canonical_client_tb_sig(tr or hl, det)
                    m_occ = tb_mirror_occ.get(msig, 0)
                    tb_mirror_occ[msig] = m_occ + 1
                    if (msig, m_occ) in seen_mirror_instances:
                        continue
                    hit = cls._build_exemption_hit(
                        det,
                        channel="client_tracebacks.log",
                        step_num=step_num,
                        occ_idx=idx,
                        mirror_sig=msig,
                        mirror_occ=m_occ,
                    )
                    if hit.get("dedup_id") and hit["dedup_id"] in seen_tb_dedup_ids:
                        continue
                    if hit.get("dedup_id"):
                        seen_tb_dedup_ids.add(str(hit["dedup_id"]))
                    collected.append(hit)
        step_act_err = str(exec_res.get("action_error") or "").strip()
        if step_act_err:
            ok_act, det_act = cls.classify_expected_user_error(
                step_act_err, expected_text, ui_state=ui_state
            )
            if ok_act and det_act is not None:
                seen_keys = {str(h.get("key") or "").lower() for h in collected}
                if str(det_act.get("key") or "").lower() not in seen_keys:
                    collected.append(
                        cls._build_exemption_hit(
                            det_act,
                            channel="action_error",
                            step_num=step_num,
                            occ_idx=0,
                        )
                    )
        return collected

    def evaluate_step(
        self,
        step_num: int,
        git_head: str,
        decision: Dict[str, Any],
        exec_res: Dict[str, Any],
        ui_snapshot: Dict[str, Any],
        screenshot_path: Path,
        burst_paths: List[Path],
        traceback_log: Path,
        server_log: Path,
        step_dir: Path,
        last_flight_recorder: Optional[Dict[str, Any]] = None,
        lazy_screenshot_fn: Optional[Any] = None,
        post_state: Optional[Dict[str, Any]] = None,
    ) -> List[Path]:
        """
        Check all 5 oracle channels:
        1. `client_tracebacks.log` new tracebacks
        2. Flight-recorder `log_exceptions`, `quasar_errors`, `macho_exceptions`
        3. `server.log` PANIC / unhandled ERROR lines
        4. Warp animation telemetry (`warp_to` succeeded in space, distance changed, but neither `saw_mode3` nor `saw_fx_warping` nor `warp_signals` fired)
        5. `agy` `bug_report.should_file == True`
        """
        new_tb = self.read_new_lines(traceback_log, "_traceback_offset").strip()
        new_srv = self.read_new_lines(server_log, "_server_log_offset").strip()
        srv_errors = [
            ln
            for ln in new_srv.splitlines()
            if any(tok in ln for tok in ("PANIC", "panicked at", "ERROR "))
            and " benign " not in ln.lower()
        ]

        fr = (exec_res.get("flight_recorder") or {}) if isinstance(exec_res, dict) else {}
        fr_log_excs = fr.get("log_exceptions") or []
        fr_quasar_errs = fr.get("quasar_errors") or []
        fr_macho_excs = fr.get("macho_exceptions") or []
        self.record_session_flight_calls(fr.get("macho_calls") or [], server_log_chunk=new_srv)
        expected_text = str((decision.get("action") or {}).get("expected_ui_and_state_change") or "")
        step_ui_state = _select_live_skill_state(post_state, ui_snapshot)
        step_exemptions: List[Dict[str, Any]] = []
        fr_fallback_trace = new_tb if len(fr_log_excs) == 1 else ""
        fr_non_benign_log_excs, fr_ignorable_log_excs = self.partition_log_exceptions(
            fr_log_excs,
            expected_text=expected_text,
            fallback_trace=fr_fallback_trace,
            ui_state=step_ui_state,
            exemptions_out=step_exemptions,
            channel="log_exceptions",
            step_num=step_num,
        )
        warp_telemetry = exec_res.get("warp_telemetry") if isinstance(exec_res, dict) else None

        prev_fr = last_flight_recorder if isinstance(last_flight_recorder, dict) else {}
        prev_expected_text = str(prev_fr.get("expected_ui_and_state_change") or "")
        prev_skills_snap = (
            {"skills_state": prev_fr["skills_state"]}
            if isinstance(prev_fr.get("skills_state"), dict)
            else None
        )
        prev_ui_state = _select_live_skill_state(
            prev_skills_snap,
            getattr(self, "_last_step_ui_state", None) or step_ui_state,
        )
        prev_log_excs, prev_ignorable_log_excs = self.partition_log_exceptions(
            prev_fr.get("log_exceptions") or [],
            expected_text=prev_expected_text,
            ui_state=prev_ui_state,
        )
        prev_all_benign_logs = list(prev_fr.get("benign_log_exceptions") or []) + prev_ignorable_log_excs
        prev_suppressed_log_excs = prev_fr.get("suppressed_log_exceptions") or []
        prev_quasar_errs = [e for e in (prev_fr.get("quasar_errors") or []) if isinstance(e, dict)]
        prev_non_benign_macho_excs = [
            e
            for e in (prev_fr.get("macho_exceptions") or [])
            if isinstance(e, dict)
            and not self.is_expected_user_error(e, prev_expected_text, ui_state=prev_ui_state)
        ]
        prev_tb_parts: List[str] = []
        raw_prev_tb = str(prev_fr.get("traceback") or "").strip()
        if raw_prev_tb and not self.is_ignorable_client_traceback(
            "", raw_prev_tb, prev_expected_text, ui_state=prev_ui_state
        ):
            prev_tb_parts.append(raw_prev_tb)
        for ple in prev_log_excs:
            if isinstance(ple, dict):
                if ple.get("headline"):
                    prev_tb_parts.append(str(ple.get("headline")))
                if ple.get("trace"):
                    prev_tb_parts.append(str(ple.get("trace")))
            elif ple:
                prev_tb_parts.append(str(ple))
        prev_tb = "\n".join(prev_tb_parts).strip()
        prev_tb_unexplained = bool(prev_tb) and not self._prev_tb_covered_by_macho_user_errors(
            raw_prev_tb,
            prev_log_excs,
            prev_non_benign_macho_excs,
            prev_expected_text,
            ui_state=prev_ui_state,
        )
        prev_action_err = str(prev_fr.get("action_error") or "").strip()

        incidents: List[Dict[str, Any]] = []
        self.last_unseen_tracebacks = []
        self.last_suppressed_tracebacks = []

        # 1. Hard client Python traceback (hook-reported deduplicated log_exceptions, plus any unseen
        # traceback block in client_tracebacks.log not already seen in this client session)
        if fr_log_excs:
            seen_item_keys = set()
            for e in fr_log_excs[:8]:
                hl, tr, full_tb = self._extract_log_exception_parts(e, fallback_trace=fr_fallback_trace)
                sig = self._tb_frame_sig(full_tb)
                if sig:
                    self._seen_tb_sigs.add(sig)
                if self.is_ignorable_client_traceback(
                    hl, full_tb, expected_text, ui_state=step_ui_state
                ):
                    continue
                item_key = (hl[:120], sig)
                if item_key in seen_item_keys:
                    continue
                seen_item_keys.add(item_key)
                raw_fallback = json.dumps(e, indent=2) if isinstance(e, dict) else str(e).strip()
                clean_tb = self._strip_build_path_prefix(tr or raw_fallback)
                incidents.append(
                    {
                        "kind": "client_traceback",
                        "title": f"[Playtester] Client exception: {hl[:90]}",
                        "severity": "high",
                        "subsystem": "client-python",
                        "summary": self._format_tb_summary(hl, full_tb),
                        "traceback": clean_tb,
                        "action_error": str(exec_res.get("action_error") or "").strip() if isinstance(exec_res, dict) else "",
                    }
                )
        if new_tb:
            unseen_tbs = []
            tb_mirror_occ: Dict[str, int] = {}
            for tb_idx, u in enumerate(self._extract_unseen_tb_blocks(new_tb)):
                ignorable_tb, tb_det = self.classify_ignorable_client_traceback(
                    u.get("headline", ""),
                    u.get("trace", ""),
                    expected_text,
                    ui_state=step_ui_state,
                )
                if ignorable_tb:
                    if tb_det is not None:
                        msig = self._canonical_client_tb_sig(
                            u.get("trace") or u.get("headline") or "", tb_det
                        )
                        m_occ = tb_mirror_occ.get(msig, 0)
                        tb_mirror_occ[msig] = m_occ + 1
                        step_exemptions.append(
                            self._build_exemption_hit(
                                tb_det,
                                channel="client_tracebacks.log",
                                step_num=step_num,
                                occ_idx=tb_idx,
                                mirror_sig=msig,
                                mirror_occ=m_occ,
                            )
                        )
                    continue
                unseen_tbs.append(u)
            if unseen_tbs:
                self.last_unseen_tracebacks = unseen_tbs
                for u in unseen_tbs[:4]:
                    incidents.append(
                        {
                            "kind": "client_traceback",
                            "title": f"[Playtester] Client exception: {u['headline'][:90]}",
                            "severity": "high",
                            "subsystem": "client-python",
                            "summary": self._format_tb_summary(u["headline"], u["trace"]),
                            "traceback": self._strip_build_path_prefix(u["trace"]),
                            "action_error": str(exec_res.get("action_error") or "").strip() if isinstance(exec_res, dict) else "",
                        }
                    )

        # 2. Quasar / MachoNet timeout or error
        if fr_quasar_errs or fr_macho_excs:
            expected_text = str((decision.get("action") or {}).get("expected_ui_and_state_change") or "")
            non_benign_macho_excs = []
            for m_idx, e in enumerate(fr_macho_excs):
                ok_m, det_m = self.classify_expected_user_error(
                    e, expected_text, ui_state=step_ui_state
                )
                if ok_m:
                    if det_m is not None:
                        step_exemptions.append(
                            self._build_exemption_hit(
                                det_m,
                                channel="macho_exceptions",
                                step_num=step_num,
                                occ_idx=m_idx,
                                seq=e.get("seq") if isinstance(e, dict) else None,
                            )
                        )
                    continue
                non_benign_macho_excs.append(e)
            if fr_quasar_errs or non_benign_macho_excs:
                sample = (fr_quasar_errs + non_benign_macho_excs)[0]
                is_quasar_err = bool(fr_quasar_errs) or (
                    isinstance(sample, dict) and str(sample.get("kind") or "").lower() == "quasar"
                )
                label_prefix = "Quasar error" if is_quasar_err else "MachoNet error"
                subsystem = "quasar" if is_quasar_err else "macho"
                act_t = decision.get("action", {}).get("type")
                if isinstance(sample, dict):
                    svc = str(sample.get("service") or "").strip()
                    meth = str(sample.get("method") or "").strip()
                    svc_meth = f"{svc}.{meth}" if (svc and meth) else (svc or meth or str(sample.get("type") or "unknown"))
                    err_s = str(sample.get("error") or sample.get("message") or sample).strip()
                    title_s = f"[Playtester] {label_prefix} on {act_t} ({svc_meth}): {err_s[:80]}"
                    act_err_s = f"{svc_meth}: {err_s}"
                else:
                    err_s = str(sample).strip()
                    title_s = f"[Playtester] {label_prefix} on {act_t}: {err_s[:90]}"
                    act_err_s = err_s
                incidents.append(
                    {
                        "kind": "protocol_error",
                        "title": title_s,
                        "severity": "high",
                        "subsystem": subsystem,
                        "summary": json.dumps(sample),
                        "traceback": new_tb,
                        "action_error": act_err_s,
                    }
                )

        incidents, absorbed_user_error_keys = self._merge_same_step_macho_client_user_errors(incidents)

        # 3. Warp animation missing oracle (#531 regression detector)
        act_type = (decision.get("action") or {}).get("type")
        if act_type == "warp_to" and isinstance(warp_telemetry, dict) and exec_res.get("action_ok"):
            saw_m3 = bool(warp_telemetry.get("saw_mode3"))
            saw_fx = bool(warp_telemetry.get("saw_fx_warping"))
            sig_cnt = int(warp_telemetry.get("warp_signal_count", 0) or 0)
            if not saw_m3 and not saw_fx and sig_cnt == 0:
                incidents.append(
                    {
                        "kind": "warp_animation_missing",
                        "title": "[Playtester] Warp animation / mode-3 transition not observed during warp_to",
                        "severity": "high",
                        "subsystem": "space-destiny-warp",
                        "summary": f"warp_to completed without ball.mode==3, effects.Warping, or warp FX signals: {json.dumps(warp_telemetry)}",
                        "traceback": "",
                        "action_error": "",
                    }
                )

        # 4. Server PANIC / ERROR
        if srv_errors:
            incidents.append(
                {
                    "kind": "server_error",
                    "title": f"[Playtester] Server error during {act_type}: {srv_errors[0][:90]}",
                    "severity": "high",
                    "subsystem": "eve-server",
                    "summary": "\n".join(srv_errors[:15]),
                    "traceback": "",
                    "action_error": "",
                }
            )

        # 4b. Client Destiny log stale-at-action oracle (tools/classify_client_log.py --expect parity)
        if step_num % 10 == 0 or act_type in ("warp_to", "undock", "dock", "jump_gate"):
            cls_report = self._check_destiny_client_logs(step_dir.parent.parent)
            if isinstance(cls_report, dict):
                stale_cnt = int(cls_report.get("stale_at_action", 0) or 0)
                if stale_cnt > self._last_stale_at_action:
                    self._last_stale_at_action = stale_cnt
                    sample_evs = cls_report.get("stale_action_events") or []
                    log_err(
                        f"[classify_client_log] Step {step_num}: stale-at-action={stale_cnt} "
                        f"sample={json.dumps(sample_evs[:3])}"
                    )
                    incidents.append(
                        {
                            "kind": "destiny_stale_stamp",
                            "title": f"[Playtester] Destiny client log reported stale-at-action={stale_cnt} during {act_type}",
                            "severity": "high",
                            "subsystem": "space-destiny-sync",
                            "summary": json.dumps(
                                {
                                    "stale_at_action": stale_cnt,
                                    "stale_at_history": cls_report.get("stale_at_history", 0),
                                    "discarded_old_states": cls_report.get("discarded_old_states", 0),
                                    "desync_warnings": cls_report.get("desync_warnings", 0),
                                    "stale_action_events": sample_evs[:5],
                                }
                            ),
                            "traceback": "",
                            "action_error": "",
                        }
                    )

        # 5. Agent-diagnosed bug (`bug_report.should_file`) OR failed previous_action_verification (`expectation_met == False`)
        # Skip Channel 5 on the first step of a freshly booted, restarted, or hot-reloaded client session
        # so the agent does not mistake the fresh startup window state for a UI-state desync of the old session's last action.
        has_verifiable_prev_step = not _is_session_boundary_flight_recorder(last_flight_recorder)
        br = (decision.get("bug_report") or {}) if has_verifiable_prev_step else {}
        pav = (decision.get("previous_action_verification") or {}) if has_verifiable_prev_step else {}
        has_hard_errs = bool(
            incidents
            or prev_log_excs
            or prev_tb
            or new_tb
            or (
                isinstance(last_flight_recorder, dict)
                and (
                    last_flight_recorder.get("quasar_errors")
                    or last_flight_recorder.get("macho_exceptions")
                    or prev_log_excs
                )
            )
        )
        prc_skip_reason = (
            _is_persona_cleanup_false_desync(last_flight_recorder, br, pav, has_hard_errs)
            if has_verifiable_prev_step
            else None
        )
        if isinstance(br, dict) and br.get("should_file") and br.get("title"):
            cat = str(br.get("category") or "agent_diagnosed_bug")
            if (
                prev_action_err
                and not prev_action_err.startswith("Client action rejected with UserError(")
                and cat in ("ui_state_desync", "none", "agent_diagnosed_bug", "broken_game_mechanic")
                and not prev_tb
            ):
                log_warn(
                    f"Step {step_num}: Suppressing agent bug_report ({cat}) because previous action returned client action_error={prev_action_err[:90]}"
                )
            elif prc_skip_reason:
                log_info(
                    f"Step {step_num}: Suppressing false-positive agent bug_report ({cat}) because {prc_skip_reason}"
                )
            elif (
                (
                    (prev_log_excs and not self._prev_step_dropped_all_log_excs)
                    or (prev_non_benign_macho_excs and prev_tb and not prev_tb_unexplained and not fr_macho_excs)
                )
                and not fr_non_benign_log_excs
                and cat in ("client_exception", "client_traceback", "client_crash")
            ):
                log_info(
                    f"Step {step_num}: Suppressing redundant agent bug_report ({cat}) because Channel 1 already captured previous step log_exceptions"
                )
            elif (
                cat in ("client_exception", "client_traceback", "client_crash")
                and not prev_log_excs
                and not prev_tb
                and not prev_quasar_errs
                and not prev_non_benign_macho_excs
                and (
                    self._all_entries_match_benign_client_race(prev_all_benign_logs)
                    or (
                        not fr_non_benign_log_excs
                        and self._all_entries_match_benign_client_race(
                            fr_ignorable_log_excs, fr_fallback_trace
                        )
                    )
                    or self.is_benign_client_traceback(
                        str(br.get("title") or ""),
                        "\n".join(
                            str(br.get(k) or "")
                            for k in ("title", "actual_behavior", "expected_behavior", "subsystem")
                        ),
                    )
                )
            ):
                log_info(
                    f"Step {step_num}: Suppressing agent bug_report ({cat}) because log_exceptions contained only benign client UI layout race(s)"
                )
            elif (
                (prev_quasar_errs or prev_non_benign_macho_excs)
                and not (fr_quasar_errs or fr_macho_excs)
                and not prev_tb_unexplained
                and cat
                in (
                    "protocol_error",
                    "broken_game_mechanic",
                    "ui_state_desync",
                    "agent_diagnosed_bug",
                    "none",
                )
            ):
                log_info(
                    f"Step {step_num}: Suppressing redundant agent bug_report ({cat}) because Channel 2 already captured previous step macho/quasar error"
                )
            elif (
                prev_suppressed_log_excs
                and not prev_log_excs
                and not fr_log_excs
                and not prev_fr.get("macho_exceptions")
                and not prev_fr.get("quasar_errors")
                and cat in (
                    "blank_or_stuck_window",
                    "ui_state_desync",
                    "client_exception",
                    "client_traceback",
                    "client_crash",
                    "agent_diagnosed_bug",
                    "none",
                )
            ):
                log_info(
                    f"Step {step_num}: Suppressing duplicate symptom bug_report ({cat}) because previous step raised suppressed repeat client exception(s)"
                )
            elif (
                not prev_log_excs
                and not prev_quasar_errs
                and not prev_non_benign_macho_excs
                and not prev_tb
                and not fr_log_excs
                and not fr_quasar_errs
                and pav.get("expectation_met") is not False
                and (
                    lambda _br_low, _has_benign_prev: (
                        any(m in _br_low for m in self.BENIGN_USER_ERROR_MARKERS)
                        and ("usererror" in _br_low or "macho_exception" in _br_low or _has_benign_prev)
                    )
                    or (
                        _has_benign_prev
                        and (
                            cat in ("server_rpc_error", "protocol_error")
                            or "usererror" in _br_low
                            or "macho_exception" in _br_low
                        )
                    )
                )(
                    "\n".join(
                        str(x or "")
                        for x in (br.get("title"), br.get("actual_behavior"), br.get("expected_behavior"))
                    ).lower(),
                    bool(prev_fr.get("macho_exceptions")),
                )
            ):
                log_info(
                    f"Step {step_num}: Suppressing false-positive agent bug_report ({cat}) because exception matches expected/benign UserError markers"
                )
            else:
                incidents.append(
                    {
                        "kind": cat if cat != "none" else "agent_diagnosed_bug",
                        "title": f"[Playtester] {str(br.get('title')).strip()}",
                        "severity": str(br.get("severity") or "medium"),
                        "subsystem": str(br.get("subsystem") or "client-ui"),
                        "summary": (
                            f"Category: {cat}\n"
                            f"Expected: {br.get('expected_behavior')}\n"
                            f"Actual: {br.get('actual_behavior')}\n"
                            f"Discrepancy: {pav.get('discrepancy_summary', '')}\n"
                            f"Steps:\n- " + "\n- ".join(str(s) for s in (br.get("reproduction_steps") or []))
                        ),
                        "traceback": prev_tb,
                        "action_error": prev_action_err,
                    }
                )
        elif (
            isinstance(pav, dict)
            and pav.get("expectation_met") is False
            and str(pav.get("discrepancy_summary") or "").strip()
            and not prev_action_err
        ):
            if prc_skip_reason:
                log_info(
                    f"Step {step_num}: Suppressing false-positive UI State Desync because {prc_skip_reason}"
                )
            elif (prev_quasar_errs or prev_non_benign_macho_excs) and not (fr_quasar_errs or fr_macho_excs) and not prev_tb_unexplained:
                log_info(
                    f"Step {step_num}: Suppressing duplicate UI State Desync because Channel 2 already captured previous step macho/quasar error"
                )
            elif (
                prev_suppressed_log_excs
                and not prev_log_excs
                and not fr_log_excs
                and not prev_fr.get("macho_exceptions")
                and not prev_fr.get("quasar_errors")
            ):
                log_info(
                    f"Step {step_num}: Suppressing duplicate symptom UI State Desync because previous step raised suppressed repeat client exception(s)"
                )
            else:
                disc = str(pav.get("discrepancy_summary")).strip()
                incidents.append(
                    {
                        "kind": "ui_state_desync",
                        "title": f"[Playtester] [UI State Desync] {disc[:90]}",
                        "severity": "medium",
                        "subsystem": "client-ui-state",
                        "summary": f"Previous action expectation failed without exception:\n{disc}",
                        "traceback": prev_tb,
                        "action_error": prev_action_err,
                    }
                )

        created_files: List[Path] = []
        for inc in incidents:
            act_err_txt = str(inc.get("action_error") or "").strip()
            tb_txt = str(inc.get("traceback") or "")
            raw_frames, game_frames = self._extract_tb_frames(tb_txt)
            chosen_frames = game_frames or raw_frames
            tb_sig = self._tb_frame_sig(tb_txt) if tb_txt else ""
            norm_title = self._strip_build_path_prefix(str(inc.get("title") or ""))[:180]
            if tb_sig:
                fp_src = f"{inc['kind']}:{inc['subsystem']}:{tb_sig}:{norm_title}:{act_err_txt[:80]}"
            else:
                fp_src = f"{inc['kind']}:{inc['subsystem']}:{norm_title}:{act_err_txt[:80]}"
            fingerprint = hashlib.sha256(fp_src.encode("utf-8")).hexdigest()[:16]
            tb_fp = ""
            if chosen_frames:
                tail_sig = f"{tb_sig}|{act_err_txt[:120]}" if tb_sig else ("|".join(chosen_frames[-2:]) + f"|{act_err_txt[:120]}")
                tb_fp = "tb:" + hashlib.sha256(tail_sig.encode("utf-8")).hexdigest()[:16]
            err_fp = ""
            if act_err_txt:
                err_fp = "err:" + hashlib.sha256(f"{tb_fp}:{act_err_txt[:140]}".encode("utf-8")).hexdigest()[:16]
            known = self.memory.data.setdefault("filed_incident_fingerprints", [])
            matched_fp = (
                fingerprint if fingerprint in known
                else (tb_fp if tb_fp and tb_fp in known else (err_fp if err_fp and err_fp in known else ""))
            )
            if matched_fp:
                log_info(f"Skipping duplicate incident ({matched_fp}): {inc['title']}")
                continue
            manual_sem_entry: Optional[Dict[str, Any]] = None
            known_manual = self.memory.data.setdefault("filed_manual_signatures", [])
            if not chosen_frames and inc.get("kind") not in ("client_traceback", "client_crash"):
                sem_tokens = sync_bugs_to_github.extract_manual_semantic_tokens(norm_title)
                if len(sem_tokens) >= 3:
                    sem_group = sync_bugs_to_github.manual_semantic_group(
                        str(inc.get("subsystem") or ""),
                        str(inc.get("kind") or ""),
                        act_err_txt,
                    )
                    matched_manual_fp = ""
                    for prior in known_manual:
                        if (
                            isinstance(prior, dict)
                            and prior.get("group") == sem_group
                            and sync_bugs_to_github.manual_semantic_match(prior.get("tokens") or (), sem_tokens)
                        ):
                            matched_manual_fp = str(prior.get("fingerprint") or "manual_semantic")
                            break
                    if matched_manual_fp:
                        log_info(
                            f"Skipping semantically duplicate manual incident ({matched_manual_fp}): {inc['title']}"
                        )
                        continue
                    manual_sem_entry = {
                        "group": sem_group,
                        "tokens": sem_tokens,
                        "fingerprint": fingerprint,
                    }
            if lazy_screenshot_fn is not None and not screenshot_path.exists():
                try:
                    lazy_screenshot_fn()
                except Exception:
                    pass
            known.append(fingerprint)
            if tb_fp and tb_fp not in known:
                known.append(tb_fp)
            if err_fp and err_fp not in known:
                known.append(err_fp)
            if manual_sem_entry is not None:
                known_manual.append(manual_sem_entry)
                if len(known_manual) > 250:
                    del known_manual[:-250]
            self.memory.save()

            self.bug_queue_dir.mkdir(parents=True, exist_ok=True)
            bundle_path = self.bug_queue_dir / f"playtester_step{step_num:04d}_{fingerprint}.json"
            fp_u64 = int(fingerprint, 16)
            fp_i64 = fp_u64 - (1 << 64) if fp_u64 >= (1 << 63) else fp_u64
            clean_title = inc["title"].replace("[Playtester]", "").strip()
            sess_raw = ui_snapshot.get("session") or {}
            fr_raw = (exec_res.get("flight_recorder") or {}) if isinstance(exec_res, dict) else {}
            fr_calls = [
                self._serialize_flight_call(mc)
                for mc in fr_raw.get("macho_calls") or []
                if isinstance(mc, dict)
            ]
            telemetry_payload = self._session_telemetry_payload(ui_snapshot=ui_snapshot)
            manual_report: Dict[str, Any] = {
                "report_key": f"playtester_step{step_num:04d}_{fingerprint}",
                "category_id": inc["subsystem"],
                "category_name": inc["kind"],
                "title": clean_title,
                "description": inc["summary"],
                "reproduction_steps": (
                    f"1. Step {step_num} action: `{json.dumps(decision.get('action'))}`\n"
                    f"2. Observation: {decision.get('observation')}\n"
                    f"3. State diff: `{json.dumps(exec_res.get('state_diff') if isinstance(exec_res, dict) else None)}`"
                ),
            }
            hs_build = (telemetry_payload.get("handshake") or {}).get("client_boot_build")
            if hs_build:
                manual_report["client_build"] = hs_build
            bundle = {
                "fingerprint": fingerprint,
                "stack_hash": fp_i64,
                "stack_key": f"playtester:{inc['subsystem']}:{inc['kind']}",
                "trace": inc["traceback"] or "",
                "report_source": "f12_manual",
                "manual_report": manual_report,
                "flight_recorder": fr_calls,
                **telemetry_payload,
                "timestamp": utc_now_iso(),
                "git_head": git_head,
                "step": step_num,
                "kind": inc["kind"],
                "title": inc["title"],
                "severity": inc["severity"],
                "subsystem": inc["subsystem"],
                "summary": inc["summary"],
                "traceback": inc["traceback"],
                "action": decision.get("action"),
                "previous_action_verification": pav,
                "state_diff": exec_res.get("state_diff") if isinstance(exec_res, dict) else None,
                "observation": decision.get("observation"),
                "reasoning": decision.get("reasoning"),
                "screenshot_path": str(screenshot_path),
                "burst_screenshots": [str(p) for p in burst_paths],
                "step_dir": str(step_dir),
                "session": {
                    "character_id": sess_raw.get("charid"),
                    "corporation_id": sess_raw.get("corpid"),
                    "solar_system_id": sess_raw.get("solarsystemid2") or sess_raw.get("solarsystemid"),
                    "station_id": sess_raw.get("stationid"),
                    "structure_id": sess_raw.get("structureid"),
                    "ship_id": sess_raw.get("shipid"),
                    **sess_raw,
                },
                "active_view": ui_snapshot.get("active_view"),
                "modals": ui_snapshot.get("modals"),
            }
            if self.async_write and self._write_queue is not None:
                self._write_queue.put((bundle_path, bundle, bool(self.sync_github)))
            else:
                bundle_path.write_text(json.dumps(bundle, indent=2), encoding="utf-8")
            created_files.append(bundle_path)
            log_warn(f"Recorded incident bundle -> {bundle_path.name}: {inc['title']}")

        if created_files and self.sync_github and not self.async_write:
            self._run_sync_script()

        captured_client_tb = bool(absorbed_user_error_keys) or any(
            inc.get("kind") == "client_traceback" for inc in incidents
        )
        self._prev_step_dropped_all_log_excs = bool(fr_log_excs) and not captured_client_tb

        step_act_err = str(exec_res.get("action_error") or "").strip() if isinstance(exec_res, dict) else ""
        if step_act_err:
            ok_act, det_act = self.classify_expected_user_error(
                step_act_err, expected_text, ui_state=step_ui_state
            )
            if ok_act and det_act is not None:
                seen_keys = {str(h.get("key") or "").lower() for h in step_exemptions}
                if str(det_act.get("key") or "").lower() not in seen_keys:
                    step_exemptions.append(
                        self._build_exemption_hit(
                            det_act,
                            channel="action_error",
                            step_num=step_num,
                            occ_idx=0,
                        )
                    )
        self.last_user_error_exemptions = list(step_exemptions)
        if getattr(self, "_exemption_tracker", None) is not None and step_exemptions:
            self._exemption_tracker.absorb_hits(step_exemptions, step_num=step_num)
        self._last_step_ui_state = step_ui_state

        return created_files

    BENIGN_FLIGHT_FLOW_MARKERS = BENIGN_USER_ERROR_MARKERS + (
        "dockrequested",
        "sessionchangeinprogress",
        "shipalreadystopped",
        "targetalreadylocked",
        "cantdockwhile",
        "warpcancelled",
    )

    @classmethod
    def analyze_flight_recorder_anomalies(
        cls,
        calls: List[Dict[str, Any]],
        *,
        min_calls: int = 3,
        error_rate_threshold: float = 0.80,
        polling_storm_per_min: float = 200.0,
        ui_state: Optional[Dict[str, Any]] = None,
        exemptions_out: Optional[List[Dict[str, Any]]] = None,
        step_num: int = 0,
    ) -> List[Dict[str, Any]]:
        """
        Inspect session flight-recorder RPC calls for:
        1. High non-OK error rates (`>= min_calls` and `>= error_rate_threshold` excluding benign flow codes).
        2. Polling-storm dispatch loops (`> polling_storm_per_min` calls/min for a `(service, method)`).
        Returns deduplicated incident descriptors sorted by `(service, method, anomaly_kind)`.
        """
        if not calls:
            return []

        deduped_calls: List[Dict[str, Any]] = []
        seen_seqs: set = set()
        for entry in calls:
            if not isinstance(entry, dict):
                continue
            seq_val = entry.get("seq")
            if seq_val is not None:
                if seq_val in seen_seqs:
                    continue
                seen_seqs.add(seq_val)
            deduped_calls.append(entry)

        grouped: Dict[Tuple[str, str], List[Dict[str, Any]]] = {}
        for entry in deduped_calls:
            svc = str(entry.get("service") or entry.get("svc") or "").strip()
            meth = str(entry.get("method") or entry.get("call") or "").strip()
            if not svc and not meth:
                continue
            grouped.setdefault((svc or "-", meth or "-"), []).append(entry)

        anomalies: List[Dict[str, Any]] = []
        for (svc, meth), items in sorted(grouped.items()):
            total = len(items)
            unexpected_errors: List[Dict[str, Any]] = []
            benign_errors = 0
            timestamps: List[float] = []
            for item in items:
                raw_ts = item.get("ts")
                if raw_ts is None:
                    raw_ts = item.get("timestamp")
                if raw_ts is not None:
                    try:
                        ts_f = float(raw_ts)
                        if ts_f > 0:
                            timestamps.append(ts_f)
                    except Exception:
                        pass
                status_up = str(item.get("status") or "").strip().upper()
                err_txt = str(item.get("error") or "").strip()
                is_non_ok = bool(err_txt) or (bool(status_up) and status_up not in ("OK", "SUCCESS"))
                if not is_non_ok:
                    continue
                combined_err = f"{status_up} {err_txt}".lower()
                call_err = err_txt or str(item.get("status") or "").strip()
                ok_ex, ex_det = (
                    cls.classify_expected_user_error({"error": call_err}, ui_state=ui_state)
                    if call_err
                    else (False, None)
                )
                matched_flow = None
                if not ok_ex and "modulefitfailed" not in combined_err:
                    for tok in cls.BENIGN_FLIGHT_FLOW_MARKERS:
                        if tok in combined_err:
                            matched_flow = tok
                            break
                if ok_ex or matched_flow:
                    benign_errors += 1
                    if exemptions_out is not None:
                        if not ok_ex or ex_det is None:
                            ue_k = cls.extract_user_error_key({"error": call_err}) or matched_flow or "UserError"
                            ex_det = {
                                "key": ue_k,
                                "mechanism": "benign_flight_flow_marker",
                                "marker": matched_flow,
                                "verified": False,
                                "detail": call_err[:240],
                            }
                        else:
                            ex_det = dict(ex_det)
                        item_ts = item.get("ts") if item.get("ts") is not None else item.get("timestamp")
                        if item_ts is not None:
                            ex_det["ts"] = item_ts
                        exemptions_out.append(
                            cls._build_exemption_hit(
                                ex_det,
                                channel="flight_recorder_harvester",
                                step_num=int(step_num or 0),
                                occ_idx=len(exemptions_out),
                                seq=item.get("seq"),
                            )
                        )
                    continue
                unexpected_errors.append(item)

            err_count = len(unexpected_errors)
            effective_total = max(1, total - benign_errors) if benign_errors and err_count > 0 else total
            err_rate = err_count / float(effective_total) if total > 0 else 0.0
            has_high_err = (err_count >= min_calls) and (err_rate >= error_rate_threshold)

            max_in_60s = 0
            rate_per_min = 0.0
            if len(timestamps) >= 2:
                timestamps.sort()
                left = 0
                for right, ts_r in enumerate(timestamps):
                    while ts_r - timestamps[left] > 60.0:
                        left += 1
                    win_cnt = right - left + 1
                    if win_cnt > max_in_60s:
                        max_in_60s = win_cnt
                span_sec = max(timestamps[-1] - timestamps[0], 0.0)
                if span_sec >= 10.0 and total >= 35:
                    rate_per_min = max(rate_per_min, (total / span_sec) * 60.0)
                rate_per_min = max(rate_per_min, float(max_in_60s))
            elif total > polling_storm_per_min:
                max_in_60s = total
                rate_per_min = float(total)

            has_polling_storm = (max_in_60s > polling_storm_per_min) or (rate_per_min > polling_storm_per_min)
            if not has_high_err and not has_polling_storm:
                continue

            sample_errs = []
            for e_item in unexpected_errors[:5]:
                et = str(e_item.get("error") or e_item.get("status") or "").strip()
                if et and et not in sample_errs:
                    sample_errs.append(et[:180])

            kind_tags = []
            if has_high_err:
                kind_tags.append("high_error_rate")
            if has_polling_storm:
                kind_tags.append("polling_storm")
            summary_parts = [f"RPC `{svc}.{meth}` telemetry anomaly ({', '.join(kind_tags)}):"]
            if has_high_err:
                summary_parts.append(
                    f"- Non-OK rate: {err_count}/{total} unexpected failures ({err_rate * 100.0:.1f}%, benign skipped={benign_errors})."
                )
            if has_polling_storm:
                summary_parts.append(
                    f"- Dispatch rate: peak {max_in_60s} calls/60s ({rate_per_min:.1f} dispatches/min, total={total})."
                )
            if sample_errs:
                summary_parts.append("- Sample errors: " + "; ".join(sample_errs))

            severity = "high" if has_polling_storm or err_count >= 5 else "medium"
            anomaly_kind = (
                "flight_recorder_polling_storm"
                if has_polling_storm and not has_high_err
                else "flight_recorder_rpc_anomaly"
            )
            title_reason = (
                f"{rate_per_min:.0f}/min polling storm"
                if has_polling_storm and not has_high_err
                else f"{err_count}/{total} non-OK"
            )
            anomalies.append(
                {
                    "kind": anomaly_kind,
                    "service": svc,
                    "method": meth,
                    "total_calls": total,
                    "non_ok_calls": err_count,
                    "benign_skipped": benign_errors,
                    "error_rate": round(err_rate, 4),
                    "peak_per_min": round(rate_per_min, 2),
                    "title": f"[Playtester] [FlightRecorder] {svc}.{meth} ({title_reason})",
                    "severity": severity,
                    "subsystem": "machonet-rpc" if "." in f"{svc}.{meth}" else "client-rpc",
                    "summary": "\n".join(summary_parts),
                    "sample_calls": items[-10:],
                }
            )
        return anomalies

    def record_flight_recorder_anomalies(
        self,
        calls: List[Dict[str, Any]],
        *,
        step_num: int = 0,
        git_head: str = "",
        session: Optional[Dict[str, Any]] = None,
        min_calls: int = 3,
        error_rate_threshold: float = 0.80,
        polling_storm_per_min: float = 200.0,
        ui_state: Optional[Dict[str, Any]] = None,
        exemption_tracker: Optional[_UserErrorExemptionTracker] = None,
    ) -> List[Path]:
        """Harvest flight-recorder anomalies at session end and write deduplicated bug bundles."""
        self.record_session_flight_calls(calls)
        harvester_exemptions: List[Dict[str, Any]] = []
        anomalies = self.analyze_flight_recorder_anomalies(
            calls,
            min_calls=min_calls,
            error_rate_threshold=error_rate_threshold,
            polling_storm_per_min=polling_storm_per_min,
            ui_state=ui_state if ui_state is not None else getattr(self, "_last_step_ui_state", None),
            exemptions_out=harvester_exemptions,
            step_num=step_num,
        )
        if harvester_exemptions:
            if getattr(self, "_exemption_tracker", None) is not None:
                self._exemption_tracker.absorb_hits(harvester_exemptions, step_num=step_num)
            if exemption_tracker is not None:
                exemption_tracker.absorb_hits(harvester_exemptions, step_num=step_num)
        if not anomalies:
            return []

        created_files: List[Path] = []
        known = self.memory.data.setdefault("filed_incident_fingerprints", [])
        sess_raw = dict(session or {})
        telemetry_payload = self._session_telemetry_payload()
        for anom in anomalies:
            fp_src = f"flight_recorder:{anom['kind']}:{anom['service']}.{anom['method']}"
            fingerprint = hashlib.sha256(fp_src.encode("utf-8")).hexdigest()[:16]
            if fingerprint in known:
                log_info(f"Skipping duplicate flight-recorder anomaly ({fingerprint}): {anom['title']}")
                continue
            known.append(fingerprint)
            self.memory.save()

            self.bug_queue_dir.mkdir(parents=True, exist_ok=True)
            bundle_path = self.bug_queue_dir / f"playtester_fr_{fingerprint}.json"
            fp_u64 = int(fingerprint, 16)
            fp_i64 = fp_u64 - (1 << 64) if fp_u64 >= (1 << 63) else fp_u64
            fr_calls = [
                self._serialize_flight_call(
                    mc,
                    default_service=anom["service"],
                    default_method=anom["method"],
                )
                for mc in anom.get("sample_calls") or []
                if isinstance(mc, dict)
            ]
            bundle = {
                "fingerprint": fingerprint,
                "stack_hash": fp_i64,
                "stack_key": f"playtester:{anom['subsystem']}:{anom['kind']}:{anom['service']}.{anom['method']}",
                "trace": "",
                "report_source": "flight_recorder_harvester",
                "manual_report": {
                    "report_key": f"playtester_fr_{fingerprint}",
                    "category_id": anom["subsystem"],
                    "category_name": anom["kind"],
                    "title": anom["title"].replace("[Playtester]", "").strip(),
                    "description": anom["summary"],
                    "reproduction_steps": (
                        f"Detected in end-of-session flight-recorder harvest at step {step_num} "
                        f"({anom['service']}.{anom['method']}: non_ok={anom['non_ok_calls']}/{anom['total_calls']}, "
                        f"peak_per_min={anom['peak_per_min']})."
                    ),
                },
                "anomaly": {
                    k: v
                    for k, v in anom.items()
                    if k != "sample_calls"
                },
                "flight_recorder": fr_calls,
                **telemetry_payload,
                "timestamp": utc_now_iso(),
                "git_head": git_head,
                "step": step_num,
                "kind": anom["kind"],
                "title": anom["title"],
                "severity": anom["severity"],
                "subsystem": anom["subsystem"],
                "summary": anom["summary"],
                "session": {
                    "character_id": sess_raw.get("charid"),
                    "corporation_id": sess_raw.get("corpid"),
                    "solar_system_id": sess_raw.get("solarsystemid2") or sess_raw.get("solarsystemid"),
                    "station_id": sess_raw.get("stationid"),
                    "structure_id": sess_raw.get("structureid"),
                    "ship_id": sess_raw.get("shipid"),
                    **sess_raw,
                },
            }
            if self.async_write and self._write_queue is not None:
                self._write_queue.put((bundle_path, bundle, bool(self.sync_github)))
            else:
                bundle_path.write_text(json.dumps(bundle, indent=2), encoding="utf-8")
            created_files.append(bundle_path)
            log_warn(f"Harvested flight-recorder anomaly -> {bundle_path.name}: {anom['title']}")

        if created_files and self.sync_github and not self.async_write:
            self._run_sync_script()
        return created_files


# ---------------------------------------------------------------------------
# Main Continuous Playtester Supervisor
# ---------------------------------------------------------------------------

def _detect_corrupted_viewstate_desync(ui_snapshot: Optional[Dict[str, Any]]) -> Optional[str]:
    """Detect irrecoverable client viewstate/ballpark desyncs (e.g. open GH #802)."""
    if not isinstance(ui_snapshot, dict):
        return None
    space = ui_snapshot.get("space")
    if not isinstance(space, dict):
        return None
    active_view = str(ui_snapshot.get("active_view") or "").strip().lower()
    in_space = space.get("in_space")
    if active_view == "inflight" and in_space is False:
        return "view_inflight_not_in_space"
    if in_space is True:
        nearby_balls = space.get("nearby_balls") or []
        ego = space.get("ego") or {}
        ego_mode = ego.get("mode")
        ego_type_id = int(ego.get("type_id") or 0)
        if not nearby_balls and (ego_mode == -1 or ego_type_id == 0):
            return "in_space_empty_ballpark_uninitialized_ego"
    return None


def validate_exploratory_action(
    action: Any,
    *,
    step_num: int,
    persona_id: str = "",
) -> Tuple[bool, str]:
    """
    Enforce setup-only GM slash rules for non-GM exploratory runs (`#1242`):
    - Step `0` bootstrap and `--persona gm_tester` allow GM/slash setup commands.
    - For normal player personas (`persona_id != "gm_tester"`) at `step_num > 0`,
      `slash_command`, `run_slash`, `_SCENARIO_GM_ACTIONS`, time-control actions,
      and `/` GM commands embedded in `type_text` are rejected before RPC dispatch.
    """
    if not isinstance(action, dict):
        return False, "Action payload must be a JSON object"
    if _prompt_allows_gm_actions(step_num, persona_id):
        return True, ""

    raw_type = str(action.get("type") or "").strip()
    act_type = _canonicalize_action_type(raw_type, bool(action.get("target")))
    if act_type in ("slash_command", "run_slash") or raw_type.lower() in ("slash_command", "run_slash"):
        cmd_preview = _coerce_action_command(action) or str(action.get("target") or "")
        return (
            False,
            f"Setup-only GM enforcement rejected {raw_type!r} ({cmd_preview!r}) at step {step_num} "
            f"for non-GM persona {persona_id or 'player'!r} (restricted to Step 0 or --persona gm_tester)",
        )
    is_gm, is_tc, reason = _is_gm_or_time_control_action(action)
    if is_gm or is_tc:
        return (
            False,
            f"Setup-only GM enforcement rejected {reason} at step {step_num} "
            f"for non-GM persona {persona_id or 'player'!r} (restricted to Step 0 or --persona gm_tester)",
        )
    if act_type == "type_text":
        for field in ("command", "slash_cmd", "text", "input", "value"):
            val = action.get(field)
            if isinstance(val, str) and val.strip().startswith("/"):
                first_tok = val.strip().split()[0].lower()
                if first_tok in _ALL_REMOTE_GM_SLASH_COMMANDS or any(
                    first_tok.startswith(p) for p in (_GM_SLASH_IN_TEXT_PREFIXES + _TIME_CONTROL_SLASH_PREFIXES)
                ):
                    return (
                        False,
                        f"Setup-only GM enforcement rejected slash input {first_tok!r} at step {step_num} "
                        f"for non-GM persona {persona_id or 'player'!r}",
                    )
    return True, ""


class PlaytesterRunner:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.worktree_dir = Path(args.worktree).resolve()
        self.runtime_dir = self.worktree_dir / ".playtester"
        self.world_dir = self.runtime_dir / "world"
        self.world_db_path = self.world_dir / "harness_world.sqlite"
        self.runs_dir = self.runtime_dir / "runs"
        self.run_id = datetime.now().strftime("%Y%m%d_%H%M%S")
        self.current_run_dir = self.runs_dir / self.run_id

        self.staged_app = Path(args.staged_app).resolve()
        self.ports = dict(DEFAULT_Ports)
        self.ports["macho"] = args.macho_port
        self.ports["bridge"] = getattr(args, "bridge_port", DEFAULT_Ports["bridge"])
        self.ports["http"] = args.http_port
        self.ports["gateway"] = args.gateway_port
        self.ports["xmpp"] = args.xmpp_port
        self.ports["rpc"] = args.rpc_port

        self.model_chain = [
            m.strip()
            for m in (args.model_chain or ",".join(DEFAULT_MODEL_CHAIN)).split(",")
            if m.strip()
        ]
        if args.model:
            # Put explicit `--model` first in the fallback chain
            explicit = args.model.strip()
            self.model_chain = [explicit] + [m for m in self.model_chain if m != explicit]

        self.hybrid_mode = bool(getattr(args, "hybrid", True))
        self.planner_model_chain = [
            m.strip()
            for m in (getattr(args, "planner_model_chain", None) or ",".join(self.model_chain)).split(",")
            if m.strip()
        ]
        if args.model:
            explicit = args.model.strip()
            self.planner_model_chain = [explicit] + [m for m in self.planner_model_chain if m != explicit]

        self.executor_model_chain = [
            m.strip()
            for m in (getattr(args, "executor_model_chain", None) or ",".join(DEFAULT_EXECUTOR_MODEL_CHAIN)).split(",")
            if m.strip()
        ]
        self.routine_planner_model_chain = list(self.executor_model_chain)

        self.window_spec = resolve_playtester_window_spec(
            window_size=getattr(args, "window_size", "full"),
            ui_scale=float(getattr(args, "ui_scale", 0.0) or 0.0),
        )
        self.screenshot_max_dim = max(0, int(getattr(args, "screenshot_max_dim", 1024) or 1024))

        self.server_proc: Optional[subprocess.Popen] = None
        self.client_proc: Optional[subprocess.Popen] = None
        self.rpc = InClientRpcBridge(port=self.ports["rpc"])
        self.reload_requested = False
        self.stop_requested = False
        self.cached_git_head: str = get_git_head(self.worktree_dir)

        self.memory = PlaytesterMemory(
            self.runtime_dir,
            git_head=self.cached_git_head,
            server_bin_mtime=self._server_bin_mtime(),
        )
        self.pending_persona_cleanup: bool = False
        self.pending_startup_hygiene: bool = not bool(
            getattr(args, "scenarios", None) or getattr(args, "scenario", None)
        )
        if args.persona:
            rotated = self.memory.maybe_rotate_persona(force_id=args.persona)
            if rotated:
                self.pending_persona_cleanup = True
        elif str((self.memory.data.get("active_persona") or {}).get("id") or "") == GM_TESTER_PERSONA["id"]:
            default_personas = self.memory.personas_cfg.get("personas") or []
            if default_personas:
                self.memory.maybe_rotate_persona(force_id=str(default_personas[0].get("id") or ""))
                self.pending_persona_cleanup = True

        self._session_flight_calls: List[Dict[str, Any]] = []
        self._session_flight_seqs: set = set()
        self._gm_bridge_conn: Any = None
        self._gm_bridge_sm: Any = None
        self._gm_bridge_lock = threading.RLock()
        self._host_gm_events_lock = threading.Lock()
        self._host_gm_pending_events: List[Dict[str, Any]] = []
        self._host_gm_server: Optional[_HostGmBridgeRpcServer] = None
        self._host_gm_port: int = 0
        self._sim_injection_seq: int = 0
        self._active_step_allows_host_time_control: Optional[bool] = None
        self._active_step_allows_host_gm_slash: Optional[bool] = None

        # Hybrid Planner + Executor + Direct Runner batch state
        self.batch_id: int = 0
        self.active_batch_plan: Optional[Dict[str, Any]] = None
        self.active_planner_decision: Optional[Dict[str, Any]] = None
        self.active_planner_model: str = ""
        self.batch_conversation_id: Optional[str] = None
        self.batch_step_index: int = 0
        self.batch_completed_flag: bool = False
        self.batch_history: List[Dict[str, Any]] = []
        self.last_active_persona_id: str = str(
            (self.memory.data.get("active_persona") or {}).get("id") or ""
        )
        self.corrupted_viewstate_streak: int = 0
        self.last_ui_snapshot: Optional[Dict[str, Any]] = None
        self.cached_post_state: Optional[Dict[str, Any]] = None
        self.cached_post_state_ts: float = 0.0
        self._force_next_screenshot: bool = True
        self._escalate_to_diagnostic_planner: bool = False
        self._pending_assertion_escalation: Optional[Dict[str, Any]] = None
        self._downtime_pause_requested: bool = False
        self.agy_pool: Optional[AgyStreamWorkerPool] = None

        self.bug_queue_dir = self.worktree_dir / ".evomu-bugs" / "queue"
        self.oracle = PlaytesterBugOracle(
            worktree_dir=self.worktree_dir,
            bug_queue_dir=self.bug_queue_dir,
            memory=self.memory,
            sync_github=not args.no_github_sync,
            async_write=True,
        )
        self._refresh_oracle_handshake_builds()
        self._guard_tracker = _GuardHitTracker(retain_all=False, max_recent=100)
        self._sim_time_tracker = _SimTimeTracker(retain_events=False, max_recent=200)
        self._user_error_exemption_tracker = _UserErrorExemptionTracker(
            retain_all=False, max_recent=100
        )

    def _peek_new_log_text(self, path: Optional[Path], offset_attr: str) -> str:
        if path is None or not path.exists():
            return ""
        oracle = getattr(self, "oracle", None)
        offset = int(getattr(oracle, offset_attr, 0) or 0) if oracle is not None else 0
        try:
            data = path.read_bytes()
        except Exception:
            return ""
        if offset < 0 or offset > len(data):
            offset = 0
        return data[offset:].decode("utf-8", errors="replace")

    def _advance_oracle_log_offsets(self) -> None:
        oracle = getattr(self, "oracle", None)
        run_dir = getattr(self, "current_run_dir", None)
        if oracle is None or run_dir is None:
            return
        for fname, offset_attr in (
            ("client_tracebacks.log", "_traceback_offset"),
            ("server.log", "_server_log_offset"),
        ):
            fpath = run_dir / fname
            try:
                if fpath.exists():
                    setattr(oracle, offset_attr, fpath.stat().st_size)
            except Exception:
                pass

    def _is_cluster_downtime_active(
        self,
        *,
        exc: Any = None,
        exec_res: Any = None,
        ui_snapshot: Any = None,
        post_state: Any = None,
        last_flight_recorder: Any = None,
        now_unix: Optional[float] = None,
    ) -> bool:
        run_dir = getattr(self, "current_run_dir", None)
        new_tb = self._peek_new_log_text(
            (run_dir / "client_tracebacks.log") if run_dir is not None else None,
            "_traceback_offset",
        )
        new_srv = _filter_downtime_server_log_lines(
            self._peek_new_log_text(
                (run_dir / "server.log") if run_dir is not None else None,
                "_server_log_offset",
            )
        )
        disconnect_candidates: List[str] = []
        explicit_only_candidates: List[str] = []

        for raw in (_serialize_failure_fragment(exc), new_tb, new_srv):
            if raw:
                disconnect_candidates.append(raw)

        if isinstance(exec_res, dict):
            for key in (
                "action_error",
                "error",
                "traceback",
                "blocked_reason",
                "rpc_error",
                "client_restarted_unexpectedly",
            ):
                frag = _serialize_failure_fragment(exec_res.get(key))
                if frag:
                    disconnect_candidates.append(frag)
            fr_both, fr_explicit = _extract_flight_recorder_failure_texts(
                exec_res.get("flight_recorder")
            )
            disconnect_candidates.extend(fr_both)
            explicit_only_candidates.extend(fr_explicit)
        else:
            frag = _serialize_failure_fragment(exec_res)
            if frag:
                disconnect_candidates.append(frag)

        disconnect_candidates.extend(_extract_downtime_modal_texts(ui_snapshot))
        disconnect_candidates.extend(_extract_downtime_modal_texts(post_state))

        if isinstance(last_flight_recorder, dict):
            last_both, last_explicit = _extract_flight_recorder_failure_texts(
                last_flight_recorder
            )
            disconnect_candidates.extend(last_both)
            explicit_only_candidates.extend(last_explicit)
        else:
            frag = _serialize_failure_fragment(last_flight_recorder)
            if frag:
                disconnect_candidates.append(frag)

        explicit_candidates = disconnect_candidates + explicit_only_candidates
        if any(_text_mentions_cluster_downtime(item) for item in explicit_candidates):
            return True
        in_win, _ = self._effective_downtime_window_and_remaining(now_unix=now_unix)
        if not in_win:
            return False
        if any(_text_mentions_disconnect(item) for item in disconnect_candidates):
            return True
        client_proc = getattr(self, "client_proc", None)
        if client_proc is not None and client_proc.poll() is not None:
            return True
        return False

    def _effective_downtime_window_and_remaining(
        self,
        *,
        now_unix: Optional[float] = None,
        pre_window_secs: int = 0,
    ) -> tuple[bool, float]:
        """Compute (in_downtime_window, remaining_secs) accounting for virtual clock offset and DB state."""
        wall_now = float(time.time() if now_unix is None else now_unix)
        offset_secs, downtime_until_unix = _read_world_downtime_state(getattr(self, "world_db_path", None))
        effective_now = wall_now + float(offset_secs)
        max_wait_secs = float(DEFAULT_DOWNTIME_WINDOW_SECS) + 1200.0 + 15.0
        if downtime_until_unix is not None:
            rem_until = float(downtime_until_unix) - effective_now
            if 0.0 < rem_until <= max_wait_secs:
                return (True, rem_until + float(DEFAULT_DOWNTIME_RESUME_BUFFER_SECS))
        in_win = _is_cluster_downtime_window(now_unix=effective_now)
        wait_secs = _seconds_until_downtime_ends(
            now_unix=effective_now,
            pre_window_secs=pre_window_secs,
        )
        return (in_win, wait_secs)

    def _wait_out_cluster_downtime(
        self,
        reason: str = "",
        *,
        now_unix: Optional[float] = None,
        sleep_fn: Any = time.sleep,
        time_fn: Any = time.time,
        chunk_secs: float = 5.0,
    ) -> float:
        """Pause continuous playtester cleanly across daily cluster downtime (~11:00-11:15 UTC)."""
        now_ts = float(time_fn() if now_unix is None else now_unix)
        _, wait_secs = self._effective_downtime_window_and_remaining(
            now_unix=now_ts,
            pre_window_secs=1200,
        )
        if wait_secs <= 0.0:
            # Server reported ClusterShuttingDown outside pre-window; back off briefly without spinning.
            wait_secs = 15.0
        max_wait_secs = float(DEFAULT_DOWNTIME_WINDOW_SECS) + 1200.0 + 15.0
        target_end = now_ts + min(wait_secs, max_wait_secs)
        log_warn(
            f"Daily cluster downtime active ({reason or 'ClusterShuttingDown'}); "
            f"stopping client and sleeping {wait_secs:.1f}s until ~11:15 UTC..."
        )
        self._downtime_pause_requested = False
        self._escalate_to_diagnostic_planner = False
        self._pending_assertion_escalation = None
        self.reset_batch_state()
        self.stop_client()
        self._advance_oracle_log_offsets()

        slept_total = 0.0
        last_log_at = now_ts
        step_chunk = max(0.05, float(chunk_secs))
        while not getattr(self, "stop_requested", False) and not getattr(self, "reload_requested", False):
            cur_ts = float(time_fn())
            remaining = target_end - cur_ts
            if remaining <= 0.0:
                break
            chunk = min(step_chunk, remaining)
            sleep_fn(chunk)
            slept_total += chunk
            if (cur_ts - last_log_at) >= 60.0 and remaining > 10.0:
                log_info(f"Waiting for daily cluster downtime to end (~{remaining:.0f}s remaining)...")
                last_log_at = cur_ts
        return slept_total

    def _pause_and_resume_after_downtime(self, reason: str) -> Optional[Dict[str, str]]:
        """Pause across cluster downtime, restart server/client if needed, and return boundary FR."""
        self._downtime_pause_requested = False
        self._wait_out_cluster_downtime(reason=reason)
        if getattr(self, "stop_requested", False) or getattr(self, "reload_requested", False):
            return None
        if self.server_proc is None or self.server_proc.poll() is not None:
            if not self.start_server(reset_db=False):
                return {"client_restarted_unexpectedly": "eve_server_restart_failed_after_downtime"}
        if not self.start_client():
            return {"client_restarted_unexpectedly": "client_restart_failed_after_downtime"}
        return {"client_restarted_for_downtime": utc_now_iso()}

    def _ensure_run_guard_state(self) -> _GuardHitTracker:
        tracker = getattr(self, "_guard_tracker", None)
        if tracker is None:
            tracker = _GuardHitTracker(retain_all=False, max_recent=100)
            self._guard_tracker = tracker
        return tracker

    def _ensure_sim_time_state(self) -> _SimTimeTracker:
        tracker = getattr(self, "_sim_time_tracker", None)
        if tracker is None:
            tracker = _SimTimeTracker(retain_events=False, max_recent=200)
            self._sim_time_tracker = tracker
        return tracker

    def _ensure_user_error_exemption_state(self) -> _UserErrorExemptionTracker:
        tracker = getattr(self, "_user_error_exemption_tracker", None)
        if tracker is None:
            tracker = _UserErrorExemptionTracker(retain_all=False, max_recent=100)
            self._user_error_exemption_tracker = tracker
        return tracker

    def _get_gm_bridge_lock(self) -> threading.RLock:
        lock = getattr(self, "_gm_bridge_lock", None)
        if lock is None:
            lock = threading.RLock()
            self._gm_bridge_lock = lock
        return lock

    def _get_host_gm_events_lock(self) -> threading.Lock:
        lock = getattr(self, "_host_gm_events_lock", None)
        if lock is None:
            lock = threading.Lock()
            self._host_gm_events_lock = lock
        return lock

    def _next_sim_injection_id(self) -> str:
        with self._get_host_gm_events_lock():
            seq = int(getattr(self, "_sim_injection_seq", 0) or 0) + 1
            self._sim_injection_seq = seq
            run_id = getattr(self, "run_id", "run")
            return f"pt-sim-{run_id}-{seq}"

    def _record_host_gm_sim_event(
        self,
        seconds: Any,
        target_char_id: int = 0,
        *,
        injection_id: Optional[str] = None,
        source: str = "host_gm_bridge",
        method: str = "AdvanceClock",
    ) -> Dict[str, Any]:
        try:
            sec_f = max(0.0, float(seconds or 0.0))
        except Exception:
            sec_f = 0.0
        inj_id = str(injection_id or self._next_sim_injection_id())
        evt = {
            "injection_id": inj_id,
            "seconds": sec_f,
            "target_char_id": int(target_char_id or 0),
            "via": "gm_bridge",
            "source": str(source or "host_gm_bridge"),
            "method": str(method or "AdvanceClock"),
            "ts": time.time(),
        }
        with self._get_host_gm_events_lock():
            pending = getattr(self, "_host_gm_pending_events", None)
            if not isinstance(pending, list):
                pending = []
                self._host_gm_pending_events = pending
            pending.append(dict(evt))
        return evt

    def _drain_host_gm_sim_events(self) -> List[Dict[str, Any]]:
        with self._get_host_gm_events_lock():
            pending = list(getattr(self, "_host_gm_pending_events", []) or [])
            self._host_gm_pending_events = []
        return pending

    def _absorb_step_sim_time(
        self,
        exec_res: Any,
        *,
        tracker: Optional[_SimTimeTracker] = None,
        extra_events: Optional[List[Dict[str, Any]]] = None,
    ) -> Dict[str, Any]:
        run_tracker = tracker if tracker is not None else self._ensure_sim_time_state()
        step_local = _SimTimeTracker(retain_events=True, max_recent=500)
        step_local.absorb_payload(exec_res)
        drained = self._drain_host_gm_sim_events()
        if drained:
            step_local.absorb_events(drained)
        if extra_events:
            step_local.absorb_events(extra_events)
        run_tracker.absorb_events(step_local.events)
        run_summary = run_tracker.summary()
        step_sec = round(sum(float(e.get("seconds") or 0.0) for e in step_local.events), 6)
        step_info = {
            "sim_time_injected_sec": step_sec,
            "sim_time_injection_count": len(step_local.events),
            "sim_time_injections": list(step_local.events),
            "total_sim_time_injected_sec": run_summary["total_sim_time_injected_sec"],
            "total_sim_time_injection_count": run_summary["total_sim_time_injection_count"],
        }
        if isinstance(exec_res, dict):
            exec_res["sim_time_injected_sec"] = step_info["sim_time_injected_sec"]
            exec_res["sim_time_injection_count"] = step_info["sim_time_injection_count"]
            exec_res["sim_time_injections"] = list(step_info["sim_time_injections"])
            exec_res["sim_time_summary"] = dict(run_summary)
        return step_info

    def _record_step_guard_hits(
        self, step_num: int, exec_res: Any
    ) -> Dict[str, Any]:
        tracker = self._ensure_run_guard_state()
        has_inline_guards = isinstance(exec_res, dict) and (
            isinstance(exec_res.get("guard_hits"), list)
            or isinstance(exec_res.get("guard_summary"), dict)
            or isinstance((exec_res.get("flight_recorder") or {}).get("guard_hits"), list)
        )
        step_hits = tracker.absorb_payload(exec_res)
        if not has_inline_guards:
            step_hits.extend(tracker.poll_rpc(getattr(self, "rpc", None), timeout=2.0))
        run_summary = tracker.summary(default_guards_off=False)
        if isinstance(exec_res, dict):
            step_summary = guard_registry.summarize_guard_hits(
                step_hits, client_guards_off=run_summary["client_guards_off"]
            )
            exec_res["guard_hits"] = step_hits
            exec_res["guard_hit_count"] = len(step_hits)
            exec_res["guard_hits_by_guard"] = step_summary["guard_hits_by_guard"]
            exec_res["guard_summary"] = run_summary
        return run_summary

    def _record_step_user_error_exemptions(
        self,
        step_num: int,
        exec_res: Any,
        *,
        expected_text: str = "",
        ui_state: Optional[Dict[str, Any]] = None,
        extra_hits: Optional[List[Dict[str, Any]]] = None,
    ) -> Tuple[List[Dict[str, Any]], Dict[str, Any]]:
        """Record exempted UserError / tasklet-traceback hits into `exec_res` and run tracker."""
        tracker = self._ensure_user_error_exemption_state()
        candidates = PlaytesterBugOracle.collect_exec_res_user_error_exemptions(
            exec_res,
            expected_text=expected_text,
            ui_state=ui_state,
            step_num=step_num,
        )
        if extra_hits:
            candidates.extend(extra_hits)
        added = tracker.absorb_hits(candidates, step_num=step_num)
        run_summary = tracker.summary()
        if isinstance(exec_res, dict):
            existing = list(exec_res.get("user_error_exemptions") or [])
            merged = existing + added if added else existing
            step_summary = summarize_user_error_exemptions(merged)
            exec_res["user_error_exemptions"] = merged
            exec_res["user_error_exemption_count"] = len(merged)
            exec_res["user_error_exemptions_by_key"] = step_summary["user_error_exemptions_by_key"]
            exec_res["user_error_exemption_summary"] = run_summary
        return added, run_summary

    def _write_run_summary(
        self, step_num: int = 0, final: bool = False
    ) -> Optional[Dict[str, Any]]:
        tracker = self._ensure_run_guard_state()
        sim_tracker = self._ensure_sim_time_state()
        ex_tracker = self._ensure_user_error_exemption_state()
        sim_summary = sim_tracker.summary()
        ex_summary = ex_tracker.summary()
        summary = tracker.summary(default_guards_off=False)
        summary["step"] = int(step_num)
        summary["final"] = bool(final)
        summary["updated_at"] = utc_now_iso()
        summary["recent_guard_hits"] = list(tracker.recent_hits)
        summary["total_sim_time_injected_sec"] = sim_summary["total_sim_time_injected_sec"]
        summary["total_sim_time_injection_count"] = sim_summary["total_sim_time_injection_count"]
        summary["recent_sim_time_injections"] = list(sim_tracker.recent_events)
        summary["user_error_exemptions_count"] = ex_summary["user_error_exemptions_count"]
        summary["user_error_exemptions_by_key"] = ex_summary["user_error_exemptions_by_key"]
        summary["user_error_exemptions_by_mechanism"] = ex_summary["user_error_exemptions_by_mechanism"]
        summary["precondition_verified_user_error_exemptions"] = ex_summary[
            "precondition_verified_user_error_exemptions"
        ]
        summary["unverified_user_error_exemptions"] = ex_summary[
            "unverified_user_error_exemptions"
        ]
        summary["recent_user_error_exemptions"] = list(ex_tracker.recent_hits)
        run_dir = getattr(self, "current_run_dir", None)
        if isinstance(run_dir, Path):
            try:
                run_dir.mkdir(parents=True, exist_ok=True)
                (run_dir / "run_summary.json").write_text(
                    json.dumps(summary, indent=2), encoding="utf-8"
                )
            except Exception as exc:
                log_warn(f"Failed writing run_summary.json: {exc}")
        mem = getattr(self, "memory", None)
        if isinstance(mem, PlaytesterMemory):
            try:
                mem.update_run_summary(summary)
            except Exception as exc:
                log_warn(f"Failed updating memory last_run_summary: {exc}")
        return summary

    def _server_bin_mtime(self) -> Optional[float]:
        for rel in ("target/debug/eve-server", "target/release/eve-server"):
            p = self.worktree_dir / rel
            if p.exists():
                try:
                    return float(p.stat().st_mtime)
                except OSError:
                    return None
        return None

    def reset_batch_state(self) -> None:
        """Reset active Executor batch state so the Parent Planner runs on the next step."""
        self.active_batch_plan = None
        self.active_planner_decision = None
        self.batch_conversation_id = None
        self.batch_step_index = 0
        self.batch_completed_flag = False
        self.batch_history = []
        self.cached_post_state = None
        self.cached_post_state_ts = 0.0
        self._escalate_to_diagnostic_planner = False
        self._pending_assertion_escalation = None
        self._force_next_screenshot = True

    def maybe_recover_corrupted_viewstate(
        self, ui_snapshot: Optional[Dict[str, Any]], threshold: int = 2
    ) -> Optional[str]:
        """Restart only EVE.app if a client viewstate/ballpark desync persists (e.g. GH #802)."""
        reason = _detect_corrupted_viewstate_desync(ui_snapshot)
        if not reason:
            self.corrupted_viewstate_streak = 0
            return None
        self.corrupted_viewstate_streak += 1
        if self.corrupted_viewstate_streak < threshold:
            log_warn(
                f"Detected client viewstate/ballpark desync ({reason}, streak={self.corrupted_viewstate_streak}/{threshold}); monitoring..."
            )
            return None
        log_warn(
            f"Persistent client viewstate/ballpark desync ({reason}, streak={self.corrupted_viewstate_streak}) "
            f"-> restarting live EVE.app client cleanly (keeping eve-server + harness_world.sqlite intact; likely GH #802)."
        )
        self.corrupted_viewstate_streak = 0
        self.reset_batch_state()
        self.stop_client()
        self.start_client()
        return reason

    def install_signal_handlers(self) -> None:
        def _on_usr1(signum: int, frame: Any) -> None:
            log_info("Received SIGUSR1 -> flagging hot-reload at next step boundary!")
            self.reload_requested = True

        def _on_term(signum: int, frame: Any) -> None:
            log_info(f"Received signal {signum} -> stopping playtester cleanly...")
            self.stop_requested = True

        signal.signal(signal.SIGUSR1, _on_usr1)
        signal.signal(signal.SIGINT, _on_term)
        signal.signal(signal.SIGTERM, _on_term)

        DEFAULT_PID_FILE.write_text(str(os.getpid()), encoding="utf-8")

    def check_external_reload_triggers(self, last_origin_check: float) -> Tuple[bool, float]:
        """Check `/tmp/evomu_playtester.control`, `SIGUSR1`, or `origin/main` git advance."""
        if self.reload_requested:
            self.reload_requested = False
            return True, last_origin_check

        if DEFAULT_CONTROL_FILE.exists():
            try:
                cmd = DEFAULT_CONTROL_FILE.read_text(encoding="utf-8").strip().upper()
                DEFAULT_CONTROL_FILE.unlink(missing_ok=True)
                if "RELOAD" in cmd or "RESTART" in cmd:
                    log_info(f"Control file trigger ({cmd}) detected -> initiating hot-reload!")
                    return True, last_origin_check
                if "STOP" in cmd:
                    log_info(f"Control file trigger ({cmd}) detected -> stopping playtester cleanly...")
                    self.stop_requested = True
                    return False, last_origin_check
            except Exception:
                pass

        now = time.time()
        if (
            self.args.watch_origin_interval > 0
            and now - last_origin_check >= self.args.watch_origin_interval
        ):
            last_origin_check = now
            new_sha = check_origin_main_update(self.worktree_dir)
            if new_sha:
                log_info(f"Detected new commit on origin/main ({new_sha}) -> initiating hot-reload!")
                return True, last_origin_check

        return False, last_origin_check

    def _resolve_sde_and_quasar_paths(self) -> Tuple[Path, Path]:
        main_checkout_data = REPO_ROOT.parent / "EvoMu" / "data"
        wt_parent_data = self.worktree_dir.parent.parent / "data"
        sde_candidates = [
            Path(os.environ.get("EVOMU_SDE_PATH", "")) if os.environ.get("EVOMU_SDE_PATH") else None,
            self.worktree_dir / "data" / "evomu_sde.sqlite",
            wt_parent_data / "evomu_sde.sqlite",
            main_checkout_data / "evomu_sde.sqlite",
        ]
        sde_path = next(
            (p for p in sde_candidates if p and p.exists() and p.stat().st_size > 0),
            self.worktree_dir / "data" / "evomu_sde.sqlite",
        )

        qd_candidates = [
            Path(os.environ.get("EVOMU_QUASAR_DESCRIPTORS_PATH", "")) if os.environ.get("EVOMU_QUASAR_DESCRIPTORS_PATH") else None,
            self.worktree_dir / "data" / "quasar_descriptors.sqlite",
            wt_parent_data / "quasar_descriptors.sqlite",
            main_checkout_data / "quasar_descriptors.sqlite",
        ]
        qd_path = next(
            (p for p in qd_candidates if p and p.exists() and p.stat().st_size > 0),
            self.worktree_dir / "data" / "quasar_descriptors.sqlite",
        )
        return sde_path, qd_path

    def _active_persona_id(self) -> str:
        return str((self.memory.data.get("active_persona") or {}).get("id") or "")

    def _refresh_oracle_handshake_builds(self, client_build: Any = None) -> None:
        oracle = getattr(self, "oracle", None)
        if oracle is None:
            return
        resolved_client = _coerce_int_client_build(client_build)
        if resolved_client <= 0:
            resolved_client = _read_client_boot_build(getattr(self, "staged_app", None)) or 0
        oracle.set_handshake_builds(
            client_boot_build=resolved_client if resolved_client > 0 else None,
            server_boot_build=_read_server_boot_build(getattr(self, "worktree_dir", REPO_ROOT)),
        )

    def _sync_initial_session_flight_calls(self, boot_payload: Any = None) -> None:
        boot_state = _unwrap_state_payload(boot_payload) if isinstance(boot_payload, dict) else {}
        client_build = (
            boot_state.get("client_build")
            or (boot_payload.get("client_build") if isinstance(boot_payload, dict) else None)
        )
        self._refresh_oracle_handshake_builds(client_build)
        if isinstance(boot_payload, dict):
            boot_fr = boot_payload.get("flight_recorder")
            if isinstance(boot_fr, dict) and boot_fr.get("macho_calls"):
                self._record_session_flight_calls(boot_fr.get("macho_calls"))
                return
        if (
            getattr(self, "rpc", None) is not None
            and getattr(self, "client_proc", None) is not None
            and self.client_proc.poll() is None
        ):
            try:
                fr_resp = self.rpc.call("get_flight_recorder", {"since_seq": 0}, timeout=5.0) or {}
                if isinstance(fr_resp.get("result"), dict):
                    fr_resp = fr_resp["result"]
                self._record_session_flight_calls(fr_resp.get("macho_calls") or [])
                self._ensure_run_guard_state().absorb_payload(fr_resp)
            except Exception:
                pass

    def _record_session_flight_calls(self, calls: Any) -> None:
        if not isinstance(calls, list):
            return
        oracle = getattr(self, "oracle", None)
        if oracle is not None:
            oracle.record_session_flight_calls(calls)
        for entry in calls:
            if not isinstance(entry, dict):
                continue
            seq_val = entry.get("seq")
            if seq_val is not None:
                if seq_val in self._session_flight_seqs:
                    continue
                self._session_flight_seqs.add(seq_val)
            self._session_flight_calls.append(entry)

    def harvest_flight_recorder_anomalies(
        self,
        *,
        step_num: int = 0,
        calls_override: Optional[List[Dict[str, Any]]] = None,
    ) -> List[Path]:
        """Collect all session flight-recorder RPCs and emit deduplicated anomaly bundles."""
        calls: List[Dict[str, Any]] = []
        if calls_override is not None:
            calls = list(calls_override)
        else:
            calls.extend(self._session_flight_calls)
            if getattr(self, "rpc", None) is not None and self.client_proc is not None and self.client_proc.poll() is None:
                try:
                    fr_resp = self.rpc.call("get_flight_recorder", {"since_seq": 0}, timeout=5.0) or {}
                    if isinstance(fr_resp.get("result"), dict):
                        fr_resp = fr_resp["result"]
                    self._record_session_flight_calls(fr_resp.get("macho_calls") or [])
                    calls = list(self._session_flight_calls)
                except Exception:
                    pass
        sess = (self.last_ui_snapshot or {}).get("session") if isinstance(self.last_ui_snapshot, dict) else {}
        return self.oracle.record_flight_recorder_anomalies(
            calls,
            step_num=step_num,
            git_head=self.cached_git_head,
            session=sess or {},
            ui_state=self.last_ui_snapshot if isinstance(self.last_ui_snapshot, dict) else None,
            exemption_tracker=self._ensure_user_error_exemption_state(),
        )

    def write_server_config(self) -> Path:
        self.world_dir.mkdir(parents=True, exist_ok=True)
        self.current_run_dir.mkdir(parents=True, exist_ok=True)
        cfg_path = self.runtime_dir / "evomu_playtester_server.toml"
        sde_path, _ = self._resolve_sde_and_quasar_paths()
        is_gm_persona = self._active_persona_id() == GM_TESTER_PERSONA["id"]
        lines = [
            'bind_address = "127.0.0.1"',
            f"macho_port = {self.ports['macho']}",
            f"bridge_port = {self.ports['bridge']}",
            f"esi_port = {self.ports['http']}",
            f"quasar_grpc_port = {self.ports['gateway']}",
            f"xmpp_port = {self.ports['xmpp']}",
            f"sde_path = {json.dumps(str(sde_path))}",
            f"world_db_path = {json.dumps(str(self.world_db_path))}",
            "",
            "[feature_flags]",
            "enable_quasar_gateway = true",
            "enable_esi_http = true",
            "enable_living_world = true",
            "seed_demo = true",
            "free_fast_checkout_plex = true",
            "starter_plex_balance = 50000",
            "allow_local_dev_gateway_jwt = true",
            "gm_mode = true",
            "",
            "[[player_accounts]]",
            'username = "demo"',
            'password_token = "demo-secret"',
            "",
            "[player_accounts.starter_character]",
            'name = "Demo Pilot"',
            "bloodline_id = 1",
            "gender = 1",
            "ancestry_id = 11",
            "school_id = 11",
            "solar_system_id = 30000142",
            "station_id = 60003760",
            "initial_balance_isk = 125000000",
            "initial_balance_plex = 50000",
        ]
        # Never emit `[[admin_accounts]]` in TOML: `provision_gm_account` inserts a character before
        # `seed_demo` / `populate_demo_snapshot`, which would clobber `demo` (90000001) on fresh DBs.
        # Keep `demo` at role = 0 with its seeded state intact; set `EVOMU_ADMIN_TOKEN` via
        # `_server_extra_env()` so `admin` (90000003) provisions post-seed and executes all host-side
        # GM & timeControl bridge ops (`#1539`, `BF-1242-R1-03`).
        _ = is_gm_persona
        cfg_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return cfg_path

    def _server_extra_env(self) -> Dict[str, str]:
        return {"EVOMU_ADMIN_TOKEN": PLAYTESTER_GM_ADMIN_SECRET}

    def _default_target_char_id(self) -> int:
        return 90000001

    def _gm_bridge_credentials(self) -> Tuple[str, str, int]:
        return "admin", PLAYTESTER_GM_ADMIN_SECRET, 90000003

    def _connect_gm_bridge(self) -> Tuple[Any, Any]:
        bridge_dir = str(CLIENT_BRIDGE_DIR)
        if bridge_dir not in sys.path:
            sys.path.insert(0, bridge_dir)
        import evomu_bridge  # type: ignore

        gm_user, gm_password, gm_char_id = self._gm_bridge_credentials()

        class _GmBridgeLog:
            def info(self, msg: str) -> None:
                log_info(f"[gm-bridge] {msg}")

            def warn(self, msg: str) -> None:
                log_warn(f"[gm-bridge] {msg}")

            def error(self, msg: str) -> None:
                log_err(f"[gm-bridge] {msg}")

        class _GmBridgeSM:
            def __init__(self) -> None:
                self.services: Dict[str, Any] = {}
                self.logger = _GmBridgeLog()

            def ScatterEvent(self, *_args: Any, **_kw: Any) -> None:
                pass

        gm_sm = _GmBridgeSM()
        bridge_conn = evomu_bridge.install_service_manager_hook(
            gm_sm,
            host="127.0.0.1",
            port=self.ports["bridge"],
        )
        try:
            gm_sm.RemoteSvc("authentication").Login(gm_user, gm_password)
            chars = gm_sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect() or []
            sel_cid = gm_char_id
            avail_ids: List[int] = []
            for ch in chars:
                cid_raw = getattr(ch, "characterID", None) if not isinstance(ch, dict) else ch.get("characterID")
                if cid_raw is not None:
                    avail_ids.append(int(cid_raw))
            if avail_ids and gm_char_id not in avail_ids:
                sel_cid = avail_ids[0]
            gm_sm.RemoteSvc("charUnboundMgr").SelectCharacterID(sel_cid)
        except Exception:
            try:
                bridge_conn.close()
            except Exception:
                pass
            raise
        return bridge_conn, gm_sm

    def _ensure_gm_bridge(self) -> Any:
        with self._get_gm_bridge_lock():
            if (
                self._gm_bridge_sm is not None
                and self._gm_bridge_conn is not None
                and getattr(self._gm_bridge_conn, "connected", True)
            ):
                return self._gm_bridge_sm
            self._close_gm_bridge()
            conn, gm_sm = self._connect_gm_bridge()
            self._gm_bridge_conn = conn
            self._gm_bridge_sm = gm_sm
            return gm_sm

    def _close_gm_bridge(self) -> None:
        with self._get_gm_bridge_lock():
            conn = getattr(self, "_gm_bridge_conn", None)
            self._gm_bridge_conn = None
            self._gm_bridge_sm = None
        if conn is not None:
            try:
                conn.close()
            except Exception:
                pass

    def _host_gm_requests_allowed(self) -> bool:
        explicit = getattr(self, "_active_step_allows_host_time_control", None)
        if explicit is not None:
            return bool(explicit)
        return True

    def _gm_slash_requests_allowed(self) -> bool:
        return True

    def _ensure_host_gm_bridge_server(self) -> int:
        srv = getattr(self, "_host_gm_server", None)
        if srv is not None and int(getattr(srv, "port", 0) or 0) > 0:
            self._host_gm_port = int(srv.port)
            return self._host_gm_port
        srv = _HostGmBridgeRpcServer(self._handle_host_gm_rpc_request)
        self._host_gm_server = srv
        self._host_gm_port = int(srv.port)
        return self._host_gm_port

    def _stop_host_gm_bridge_server(self) -> None:
        srv = getattr(self, "_host_gm_server", None)
        self._host_gm_server = None
        self._host_gm_port = 0
        if srv is not None:
            try:
                srv.close()
            except Exception:
                pass

    def _host_gm_port_for_client(self, ensure: bool = False) -> int:
        if ensure:
            return self._ensure_host_gm_bridge_server()
        return int(getattr(self, "_host_gm_port", 0) or 0)

    def _clear_session_timer_via_gm_bridge(self, target_char_id: int = 0) -> bool:
        tgt = int(target_char_id or self._default_target_char_id() or 0)
        try:
            with self._get_gm_bridge_lock():
                gm_sm = self._ensure_gm_bridge()
                if tgt > 0:
                    gm_sm.RemoteSvc("timeControl").ClearSessionTimer(tgt)
                else:
                    gm_sm.RemoteSvc("timeControl").ClearSessionTimer()
            return True
        except Exception as exc:
            log_warn(f"GM bridge ClearSessionTimer(target={tgt}) failed: {exc}")
            return False

    def _execute_gm_slash_with_system_context(
        self,
        gm_sm: Any,
        rewritten: str,
        target_solar_system_id: int = 0,
    ) -> Any:
        slash_svc = gm_sm.RemoteSvc("slash")
        verb = rewritten.split()[0].lower() if str(rewritten).startswith("/") else ""
        try:
            target_sys = int(target_solar_system_id or 0)
        except Exception:
            target_sys = 0
        home_sys = 30000142
        home_dock = 60003760
        if verb in ("/despawn", "/unspawn") and target_sys > 0 and target_sys != home_sys:
            slash_svc.SlashCmd(f"/tr me {target_sys} noblock")
            try:
                return slash_svc.SlashCmd(rewritten)
            finally:
                try:
                    slash_svc.SlashCmd(f"/tr me {home_dock} noblock")
                except Exception:
                    try:
                        slash_svc.SlashCmd(f"/tr me {home_sys} noblock")
                    except Exception:
                        pass
                try:
                    gm_cid = int(self._gm_bridge_credentials()[2] or 0)
                    if gm_cid > 0:
                        gm_sm.RemoteSvc("timeControl").ClearSessionTimer(gm_cid)
                    else:
                        gm_sm.RemoteSvc("timeControl").ClearSessionTimer()
                except Exception:
                    pass
                try:
                    gm_sm.RemoteSvc("stationSvc").Dock(home_dock)
                except Exception:
                    pass
        return slash_svc.SlashCmd(rewritten)

    def _handle_host_gm_rpc_request(self, req: Dict[str, Any]) -> Dict[str, Any]:
        if not isinstance(req, dict):
            return {"ok": False, "error": "invalid_request"}
        cmd = str(req.get("cmd") or "").strip().lower()
        try:
            tgt = int(req.get("target_char_id") or self._default_target_char_id() or 0)
        except Exception:
            tgt = int(self._default_target_char_id() or 0)
        if cmd == "advance_clock":
            if not self._host_gm_requests_allowed():
                return {"ok": False, "reason": "host_time_control_disabled"}
            try:
                secs_i = max(0, int(req.get("secs") if req.get("secs") is not None else req.get("seconds", 0)))
            except Exception:
                secs_i = 0
            call_res = self._advance_clock_via_gm_bridge(
                secs_i,
                target_char_id=tgt,
                source="inclient_host_gm_bridge",
            )
            if call_res is None:
                return {"ok": False, "error": f"gm_bridge AdvanceClock({secs_i}, {tgt}) failed"}
            return {
                "ok": True,
                "via": "gm_bridge",
                "target_char_id": call_res["target_char_id"],
                "secs": secs_i,
                "seconds": secs_i,
                "injection_id": call_res["injection_id"],
                "time": call_res.get("time"),
            }
        if cmd == "clear_session_timer":
            if not self._host_gm_requests_allowed():
                return {"ok": False, "reason": "host_time_control_disabled"}
            ok = self._clear_session_timer_via_gm_bridge(target_char_id=tgt)
            if not ok:
                return {"ok": False, "error": f"gm_bridge ClearSessionTimer({tgt}) failed"}
            return {"ok": True, "via": "gm_bridge", "target_char_id": tgt}
        if cmd == "slash_cmd":
            if not self._gm_slash_requests_allowed():
                return {"ok": False, "handled": True, "reason": "host_gm_slash_disabled"}
            raw_cmd = str(req.get("command") or "").strip()
            if not raw_cmd.startswith("/"):
                return {"ok": False, "handled": True, "error": f"invalid_slash_cmd:{raw_cmd}"}
            rewritten = self._rewrite_gm_slash_for_target(raw_cmd, tgt)
            try:
                tgt_sys = int(req.get("target_solar_system_id") or 0)
            except Exception:
                tgt_sys = 0
            try:
                with self._get_gm_bridge_lock():
                    gm_sm = self._ensure_gm_bridge()
                    res = self._execute_gm_slash_with_system_context(
                        gm_sm,
                        rewritten,
                        target_solar_system_id=tgt_sys,
                    )
                sim_evt = self._maybe_record_slash_sim_time_event(
                    rewritten,
                    res,
                    target_char_id=tgt,
                    source="inclient_host_gm_bridge",
                )
                resp_payload: Dict[str, Any] = {
                    "ok": True,
                    "handled": True,
                    "via": "gm_bridge",
                    "command": rewritten,
                    "target_char_id": tgt,
                    "target_solar_system_id": tgt_sys,
                    "result": res,
                }
                if sim_evt is not None:
                    resp_payload["injection_id"] = sim_evt["injection_id"]
                    resp_payload["sim_time_injected_sec"] = sim_evt["seconds"]
                return resp_payload
            except Exception as exc:
                return {
                    "ok": False,
                    "handled": True,
                    "via": "gm_bridge",
                    "command": rewritten,
                    "target_char_id": tgt,
                    "target_solar_system_id": tgt_sys,
                    "error": repr(exc),
                }
        return {"ok": False, "handled": False, "error": f"unsupported_cmd:{cmd}"}

    def _maybe_record_slash_sim_time_event(
        self,
        cmd: str,
        gm_res: Any,
        *,
        target_char_id: int = 0,
        source: str = "host_gm_bridge",
    ) -> Optional[Dict[str, Any]]:
        adv_secs = gm_slash_utils.extract_slash_sim_advance_seconds(
            cmd,
            gm_res,
            target_char_id=target_char_id,
        )
        if adv_secs is None:
            return None
        verb = str(cmd or "").strip().split()[0].lower()
        return self._record_host_gm_sim_event(
            adv_secs,
            target_char_id=target_char_id,
            source=source,
            method=f"SlashCmd:{verb}",
        )

    @staticmethod
    def _looks_like_char_id_token(tok: str, target_char_id: int) -> bool:
        return gm_slash_utils.looks_like_char_id_token(tok, target_char_id)

    @staticmethod
    def _looks_like_entity_or_location_token(tok: str, target_char_id: int) -> bool:
        return gm_slash_utils.looks_like_entity_or_location_token(tok, target_char_id)

    @classmethod
    def _rewrite_gm_slash_for_target(cls, cmd: str, target_char_id: int) -> str:
        return gm_slash_utils.rewrite_gm_slash_for_target(cmd, target_char_id)

    @staticmethod
    def _should_dispatch_via_gm_bridge(act: Dict[str, Any], act_type: str) -> bool:
        if act_type in _SCENARIO_TIME_CONTROL_ACTIONS:
            return True
        if act_type in ("slash_command", "run_slash", "train_skill"):
            return True
        if act_type == "station_command" and str(act.get("target") or "").strip().lower() == "inject_and_train_skill":
            return True
        if act_type == "type_text" and any(_is_gm_or_time_control_action(act)[:2]):
            return True
        return False

    def _advance_clock_via_gm_bridge(
        self,
        secs: Any = 12,
        target_char_id: int = 0,
        source: str = "host_gm_bridge",
    ) -> Optional[Dict[str, Any]]:
        tgt = int(target_char_id or self._default_target_char_id() or 0)
        secs_i = max(0, int(secs or 0))
        try:
            with self._get_gm_bridge_lock():
                gm_sm = self._ensure_gm_bridge()
                if tgt > 0:
                    res = gm_sm.RemoteSvc("timeControl").AdvanceClock(secs_i, tgt)
                else:
                    res = gm_sm.RemoteSvc("timeControl").AdvanceClock(secs_i)
            evt = self._record_host_gm_sim_event(
                secs_i,
                target_char_id=tgt,
                source=source,
                method="AdvanceClock",
            )
            return {
                "seq": 0,
                "service": "timeControl",
                "method": "AdvanceClock",
                "ok": True,
                "via": "gm_bridge",
                "target_char_id": tgt,
                "seconds": secs_i,
                "injection_id": evt["injection_id"],
                "time": res,
            }
        except Exception as exc:
            log_warn(f"GM bridge AdvanceClock({secs}) failed: {exc}")
            return None

    @staticmethod
    def _diff_scatter_events(
        pre_scatters: Optional[List[str]],
        post_scatters: Optional[List[str]],
    ) -> List[str]:
        """Return newly observed scatter event names between two sliding-window snapshots."""
        if post_scatters is None:
            return []
        if not pre_scatters:
            return list(post_scatters)
        max_overlap = min(len(pre_scatters), len(post_scatters))
        overlap = 0
        for size in range(max_overlap, 0, -1):
            if pre_scatters[-size:] == post_scatters[:size]:
                overlap = size
                break
        return list(post_scatters[overlap:])

    def _read_client_scatters(self, timeout: float = 5.0) -> Tuple[bool, Optional[List[str]]]:
        """Read current scatter event window from the in-client flight recorder."""
        rpc = getattr(self, "rpc", None)
        if rpc is None:
            return False, None
        try:
            resp = rpc.call("get_flight_recorder", {"since_seq": 0}, timeout=timeout)
        except Exception:
            return False, None
        if not isinstance(resp, dict):
            return False, None
        payload = resp.get("result") if isinstance(resp.get("result"), dict) else resp
        if resp.get("ok") is False and "scatters_observed" not in payload:
            return False, None
        raw = payload.get("scatters_observed")
        if not isinstance(raw, list):
            return True, None
        return True, [str(x) for x in raw if x is not None]

    def _settle_gm_bridge_scatters(
        self,
        pre_ok: bool,
        pre_scatters: Optional[List[str]],
        *,
        min_settle_s: float = 0.25,
        max_settle_s: float = 0.90,
        poll_interval_s: float = 0.06,
    ) -> List[str]:
        """Wait for and capture in-client scatters triggered by a GM bridge call."""
        if not pre_ok:
            return []
        cur_ok, cur_scatters = self._read_client_scatters()
        if not cur_ok:
            return []
        effective_pre = pre_scatters
        if effective_pre is None:
            if cur_scatters is None:
                return []
            effective_pre = []

        delta = self._diff_scatter_events(effective_pre, cur_scatters)
        start = time.monotonic()
        last_delta = delta
        stable_polls = 0
        min_delta_settle = max(0.0, float(min_settle_s))
        max_wait = max(min_delta_settle, float(max_settle_s))
        step = max(0.01, float(poll_interval_s))
        max_polls = max(3, int(round(max_wait / step)) + 2)

        for _ in range(max_polls):
            elapsed = time.monotonic() - start
            if elapsed >= max_wait:
                break
            if elapsed >= min_delta_settle:
                if delta and stable_polls >= 1:
                    break
                if not delta and elapsed >= min_delta_settle + step:
                    break
            time.sleep(min(step, max(0.01, max_wait - elapsed)))
            ok, latest = self._read_client_scatters()
            if not ok or latest is None:
                break
            new_delta = self._diff_scatter_events(effective_pre, latest)
            if new_delta == last_delta:
                stable_polls += 1
            else:
                stable_polls = 0
                last_delta = new_delta
            delta = new_delta

        return delta

    def _execute_gm_or_time_control_via_bridge(
        self,
        act: Dict[str, Any],
        act_type: str,
        target_char_id: int,
    ) -> Dict[str, Any]:
        tgt = int(target_char_id or self._default_target_char_id() or 0)
        calls_recorded: List[Dict[str, Any]] = []
        sim_events: List[Dict[str, Any]] = []
        pre_ok = False
        pre_scatters: Optional[List[str]] = None
        try:
            with self._get_gm_bridge_lock():
                gm_sm = self._ensure_gm_bridge()
                pre_ok, pre_scatters = self._read_client_scatters()
                gm_res: Any = None
                if act_type == "advance_server_time":
                    raw_sec = act.get("seconds")
                    if raw_sec in (None, ""):
                        raw_sec = act.get("target")
                    if raw_sec in (None, ""):
                        raw_sec = act.get("amount")
                    if raw_sec in (None, ""):
                        raw_sec = act.get("command")
                    try:
                        secs = max(0, int(str(raw_sec).strip())) if raw_sec not in (None, "") else 12
                    except Exception:
                        secs = 12
                    if tgt > 0:
                        gm_res = gm_sm.RemoteSvc("timeControl").AdvanceClock(secs, tgt)
                    else:
                        gm_res = gm_sm.RemoteSvc("timeControl").AdvanceClock(secs)
                    evt = self._record_host_gm_sim_event(
                        secs,
                        target_char_id=tgt,
                        source="gm_action_bridge",
                        method="AdvanceClock",
                    )
                    sim_events.append(evt)
                    calls_recorded.append({
                        "seq": 0,
                        "service": "timeControl",
                        "method": "AdvanceClock",
                        "ok": True,
                        "via": "gm_bridge",
                        "target_char_id": tgt,
                        "seconds": secs,
                        "injection_id": evt["injection_id"],
                    })
                elif act_type == "clear_timers":
                    if tgt > 0:
                        gm_sm.RemoteSvc("timeControl").ClearSessionTimer(tgt)
                    else:
                        gm_sm.RemoteSvc("timeControl").ClearSessionTimer()
                    calls_recorded.append({
                        "seq": 0,
                        "service": "timeControl",
                        "method": "ClearSessionTimer",
                        "ok": True,
                        "via": "gm_bridge",
                        "target_char_id": tgt,
                    })
                    clear_cmd = f"/cleartimers {tgt}" if tgt > 0 else "/cleartimers"
                    gm_res = gm_sm.RemoteSvc("slash").SlashCmd(clear_cmd)
                    calls_recorded.append({
                        "seq": 0,
                        "service": "slash",
                        "method": "SlashCmd",
                        "command": clear_cmd,
                        "ok": True,
                        "via": "gm_bridge",
                        "target_char_id": tgt,
                    })
                elif act_type in ("complete_skills", "advance_upwell_timer", "complete_industry_jobs"):
                    slash_map = {
                        "complete_skills": ("/completeskills", "all"),
                        "advance_upwell_timer": ("/upwelltimer", ""),
                        "complete_industry_jobs": ("/completeindustry", ""),
                    }
                    base_cmd, default_arg = slash_map[act_type]
                    arg = str(act.get("target") or act.get("command") or default_arg).strip()
                    raw_cmd = arg if arg.startswith("/") else f"{base_cmd} {arg}".strip()
                    cmd = self._rewrite_gm_slash_for_target(raw_cmd, tgt)
                    try:
                        act_sys = int(act.get("target_solar_system_id") or 0)
                    except Exception:
                        act_sys = 0
                    gm_res = self._execute_gm_slash_with_system_context(
                        gm_sm,
                        cmd,
                        target_solar_system_id=act_sys,
                    )
                    sim_evt = self._maybe_record_slash_sim_time_event(
                        cmd,
                        gm_res,
                        target_char_id=tgt,
                        source="gm_action_bridge",
                    )
                    call_entry = {
                        "seq": 0,
                        "service": "slash",
                        "method": "SlashCmd",
                        "command": cmd,
                        "ok": True,
                        "via": "gm_bridge",
                        "target_char_id": tgt,
                    }
                    if sim_evt is not None:
                        sim_events.append(sim_evt)
                        call_entry["sim_time_injected_sec"] = sim_evt["seconds"]
                        call_entry["injection_id"] = sim_evt["injection_id"]
                    calls_recorded.append(call_entry)
                elif act_type == "train_skill" or (
                    act_type == "station_command"
                    and str(act.get("target") or "").strip().lower() == "inject_and_train_skill"
                ):
                    tid_raw = act.get("type_id")
                    if tid_raw in (None, ""):
                        cand_t = str(act.get("target") or "").strip()
                        if cand_t.isdigit():
                            tid_raw = int(cand_t)
                    if tid_raw in (None, ""):
                        cand_c = str(act.get("command") or "").strip()
                        if cand_c.isdigit():
                            tid_raw = int(cand_c)
                    tid = int(tid_raw or 0)
                    if tid <= 0:
                        raise ValueError(f"train_skill missing numeric type_id: {act!r}")
                    lvl = max(1, min(5, int(act.get("level", 1) or 1)))
                    cmd = f"/giveskill {tgt} {tid} {lvl}"
                    gm_res = gm_sm.RemoteSvc("slash").SlashCmd(cmd)
                    calls_recorded.append({
                        "seq": 0,
                        "service": "slash",
                        "method": "SlashCmd",
                        "command": cmd,
                        "ok": True,
                        "via": "gm_bridge",
                        "target_char_id": tgt,
                    })
                else:
                    raw_cmd = (
                        _coerce_action_command(act)
                        or str(act.get("slash_cmd") or act.get("target") or act.get("text") or act.get("input") or "").strip()
                    )
                    cmd = self._rewrite_gm_slash_for_target(raw_cmd, tgt)
                    try:
                        act_sys = int(act.get("target_solar_system_id") or 0)
                    except Exception:
                        act_sys = 0
                    gm_res = self._execute_gm_slash_with_system_context(
                        gm_sm,
                        cmd,
                        target_solar_system_id=act_sys,
                    )
                    sim_evt = self._maybe_record_slash_sim_time_event(
                        cmd,
                        gm_res,
                        target_char_id=tgt,
                        source="gm_action_bridge",
                    )
                    call_entry = {
                        "seq": 0,
                        "service": "slash",
                        "method": "SlashCmd",
                        "command": cmd,
                        "ok": True,
                        "via": "gm_bridge",
                        "target_char_id": tgt,
                    }
                    if sim_evt is not None:
                        sim_events.append(sim_evt)
                        call_entry["sim_time_injected_sec"] = sim_evt["seconds"]
                        call_entry["injection_id"] = sim_evt["injection_id"]
                    calls_recorded.append(call_entry)

            scatters_observed = self._settle_gm_bridge_scatters(pre_ok, pre_scatters)
            sim_sec = round(sum(float(e.get("seconds") or 0.0) for e in sim_events), 6)
            return {
                "action_ok": True,
                "via": "gm_bridge",
                "gm_result": gm_res,
                "sim_time_injected_sec": sim_sec,
                "sim_time_injection_count": len(sim_events),
                "sim_time_injections": list(sim_events),
                "flight_recorder": {
                    "macho_calls": calls_recorded,
                    "scatters_observed": scatters_observed,
                    "sim_time_injections": list(sim_events),
                    "log_exceptions": [],
                    "quasar_errors": [],
                    "macho_exceptions": [],
                },
            }
        except Exception as exc:
            scatters_observed = []
            if pre_ok:
                try:
                    scatters_observed = self._settle_gm_bridge_scatters(
                        pre_ok,
                        pre_scatters,
                        min_settle_s=0.05,
                        max_settle_s=0.25,
                    )
                except Exception:
                    scatters_observed = []
            return {
                "action_ok": False,
                "action_error": f"gm_bridge {type(exc).__name__}: {exc}",
                "via": "gm_bridge",
                "flight_recorder": {
                    "macho_calls": calls_recorded,
                    "scatters_observed": scatters_observed,
                    "log_exceptions": [],
                    "quasar_errors": [],
                    "macho_exceptions": [],
                },
            }

    def write_inclient_cfg(self) -> Path:
        self.current_run_dir.mkdir(parents=True, exist_ok=True)
        cfg_path = self.current_run_dir / "inclient_cfg.json"
        spec = self.window_spec
        payload = {
            "mode": "playtester",
            "playtester_port": self.ports["rpc"],
            "hook_port": self.ports["rpc"],
            "host_gm_port": self._host_gm_port_for_client(ensure=False),
            "user": "demo",
            "password": "demo-secret",
            "char_id": 90000001,
            "server": f"127.0.0.1:{self.ports['macho']}",
            "http_port": self.ports["http"],
            "gateway_port": self.ports["gateway"],
            "xmpp_port": self.ports["xmpp"],
            "run_dir": str(self.current_run_dir),
            "out_dir": str(self.current_run_dir),
            "window_width": int(spec["width"]),
            "window_height": int(spec["height"]),
            "window_left": int(spec["left"]),
            "window_top": int(spec["top"]),
            "window_ui_scale": float(spec["ui_scale"]),
        }
        cfg_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        return cfg_path

    def wait_for_port(self, port: int, timeout: float = 25.0, proc: Optional[subprocess.Popen] = None) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if proc is not None and proc.poll() is not None:
                return False
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=1.0):
                    return True
            except Exception:
                time.sleep(0.3)
        return False

    def start_server(self, reset_db: bool = False) -> bool:
        """
        Start `eve-server` using persistent `harness_world.sqlite`.
        If `reset_db` is False, backs up `harness_world.sqlite` first; if `eve-server`
        exits immediately due to an incompatible DB schema change, automatically
        archives the incompatible DB and retries with a fresh DB.
        """
        self.stop_server()
        self.world_dir.mkdir(parents=True, exist_ok=True)
        self.current_run_dir.mkdir(parents=True, exist_ok=True)

        if reset_db and self.world_db_path.exists():
            archived = self.world_dir / f"harness_world.reset_{int(time.time())}.sqlite"
            log_info(f"--reset-db requested: archiving {self.world_db_path.name} -> {archived.name}")
            shutil.move(str(self.world_db_path), str(archived))
        elif self.world_db_path.exists():
            backup = self.world_dir / "harness_world.pre_reload.sqlite"
            shutil.copy2(str(self.world_db_path), str(backup))
            log_info(
                f"Preserving persistent play state DB ({self.world_db_path}, "
                f"{self.world_db_path.stat().st_size // 1024} KiB)"
            )

        binary = self.worktree_dir / "target" / "debug" / "eve-server"
        if not binary.exists():
            # Reuse supervisor binary if at the exact same git commit to avoid redundant rebuild
            sup_root = REPO_ROOT.parent / "EvoMu-supervisor"
            sup_bin = sup_root / "target" / "debug" / "eve-server"
            if sup_bin.exists() and get_git_head(sup_root) == get_git_head(self.worktree_dir):
                log_info(f"Reusing matching eve-server binary from {sup_bin} (HEAD={get_git_head(self.worktree_dir)})")
                binary.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(str(sup_bin), str(binary))
            else:
                ok, binary, _ = compile_eve_server(self.worktree_dir)
                if not ok:
                    return False

        cfg_path = self.write_server_config()
        sde_path, qd_path = self._resolve_sde_and_quasar_paths()
        server_log = self.current_run_dir / "server.log"
        log_f = server_log.open("a", encoding="utf-8")

        env = os.environ.copy()
        env["EVOMU_CONFIG_PATH"] = str(cfg_path)
        env["EVOMU_SDE_PATH"] = str(sde_path)
        env["EVOMU_QUASAR_DESCRIPTORS_PATH"] = str(qd_path)
        env.setdefault("RUST_LOG", "info")
        extra_env_fn = getattr(self, "_server_extra_env", None)
        if callable(extra_env_fn):
            for k, v in (extra_env_fn() or {}).items():
                env[str(k)] = str(v)

        log_info(
            f"Launching eve-server ({get_git_head(self.worktree_dir)}) on "
            f"Macho={self.ports['macho']} ESI/HTTP={self.ports['http']} Quasar={self.ports['gateway']} XMPP={self.ports['xmpp']}"
        )
        self.server_proc = subprocess.Popen(
            [str(binary)],
            cwd=str(self.worktree_dir),
            stdout=log_f,
            stderr=subprocess.STDOUT,
            env=env,
        )

        if (
            self.wait_for_port(self.ports["macho"], timeout=90.0, proc=self.server_proc)
            and self.wait_for_port(self.ports["http"], timeout=15.0, proc=self.server_proc)
            and self.wait_for_port(self.ports["gateway"], timeout=15.0, proc=self.server_proc)
            and self.wait_for_port(self.ports["xmpp"], timeout=15.0, proc=self.server_proc)
        ):
            log_info(f"eve-server ready (PID {self.server_proc.pid})")
            return True

        # Check if server failed due to DB schema incompatibility
        tail = ""
        if server_log.exists():
            tail = server_log.read_text(encoding="utf-8", errors="replace")[-2000:]
        log_warn(f"eve-server failed initial startup:\n{tail[-600:]}")
        self.stop_server()

        tail_l = tail.lower()
        if not reset_db and self.world_db_path.exists() and (
            "sde schema error" not in tail_l and "sde_meta" not in tail_l
        ) and any(
            k in tail_l for k in ("sqlite", "schema", "migration", "column", "table", "database")
        ):
            incompat = self.world_dir / f"harness_world.incompat_{int(time.time())}.sqlite"
            log_warn(
                f"Detected incompatible DB schema after server update; archiving old DB to {incompat.name} and starting fresh DB."
            )
            shutil.move(str(self.world_db_path), str(incompat))
            return self.start_server(reset_db=True)

        return False

    def stop_server(self) -> None:
        self._stop_host_gm_bridge_server()
        self._close_gm_bridge()
        if self.server_proc and self.server_proc.poll() is None:
            log_info(f"Stopping eve-server (PID {self.server_proc.pid})...")
            try:
                self.server_proc.terminate()
                self.server_proc.wait(timeout=8)
            except Exception:
                try:
                    self.server_proc.kill()
                except Exception:
                    pass
        self.server_proc = None

    def _crashmon_pids(self) -> set:
        try:
            out = subprocess.check_output(["pgrep", "-f", "eve_crashmon"], text=True)
            return {int(x) for x in out.split() if x.strip().isdigit()}
        except Exception:
            return set()

    def _handle_start_client_failure(
        self,
        *,
        exc: Any = None,
        reason: str = "",
        _retry_after_downtime: bool = True,
    ) -> bool:
        is_downtime = bool(getattr(self.args, "continuous", False)) and self._is_cluster_downtime_active(
            exc=exc
        )
        self.stop_client()
        if is_downtime:
            self._wait_out_cluster_downtime(reason=reason or str(exc or "cluster downtime"))
            if (
                _retry_after_downtime
                and not getattr(self, "stop_requested", False)
                and not getattr(self, "reload_requested", False)
            ):
                return self.start_client(_retry_after_downtime=False)
        return False

    def start_client(self, _retry_after_downtime: bool = True) -> bool:
        self.stop_client()
        host_gm_port = self._host_gm_port_for_client(ensure=True)
        inclient_cfg = self.write_inclient_cfg()
        stage_dir = stage_playtester_client(self.staged_app, inclient_cfg, self.ports["rpc"])

        exefile = stage_dir / "bin64" / "exefile"
        if not exefile.exists():
            raise RuntimeError(f"exefile not found at {exefile}")

        try:
            sso_token = mint_sso_jwt_via_server(self.ports["http"], "demo", "demo-secret")
        except Exception as exc:
            log_warn(f"OAuth PKCE token mint failed ({exc}); falling back to offline JWT")
            sso_token = mint_sso_jwt(account_id=1, character_id=90000001)

        client_log = self.current_run_dir / "client.log"
        log_f = client_log.open("a", encoding="utf-8")
        client_logs_dir = self.current_run_dir / "client_logs"
        client_logs_dir.mkdir(parents=True, exist_ok=True)

        env = os.environ.copy()
        # Continuous playtester keeps server-caused guards enabled so hits are recorded.
        env.pop(guard_registry.DISABLE_SERVER_GUARDS_ENV, None)
        env["EVE_PUBLIC_GATEWAY_ADDRESS"] = f"127.0.0.1:{self.ports['gateway']}"
        env["EVOMU_HARNESS_DIR"] = str(HARNESS_DIR)
        env["PYTHONPATH"] = str(stage_dir / "evomu_py_site")
        env["EVOMU_INCLIENT_CFG"] = str(inclient_cfg)
        env["EVOMU_PLAYTESTER_CFG"] = str(inclient_cfg)
        env["EVOMU_PLAYTESTER_PORT"] = str(self.ports["rpc"])
        if host_gm_port > 0:
            env["EVOMU_HOST_GM_PORT"] = str(host_gm_port)
        else:
            env.pop("EVOMU_HOST_GM_PORT", None)

        cmd = [
            str(exefile),
            "/server:127.0.0.1",
            f"/port:{self.ports['macho']}",
            f"/fileLogDirectory:{client_logs_dir}",
            "/localdevtools",
            "/tools=evomu_playtester",
        ]
        if getattr(self.args, "silent", False):
            cmd.append("/silent")
        cmd.append(f"/ssoToken={sso_token}")
        self._pre_crashmon_pids = self._crashmon_pids()
        log_info(f"Launching live EVE.app client (RPC bridge port {self.ports['rpc']})...")
        self.client_proc = subprocess.Popen(
            cmd,
            cwd=str(stage_dir),
            stdout=log_f,
            stderr=subprocess.STDOUT,
            env=env,
            start_new_session=True,
        )

        # Wait for in-client JSON-RPC hook to accept connections (after _wait_for_session completes;
        # service boot (~45s under load) + _wait_for_session (up to 120s) can exceed 120s)
        deadline = time.time() + 210.0
        while time.time() < deadline:
            if self.client_proc.poll() is not None:
                rc = self.client_proc.returncode
                log_err(f"EVE.app exited early with code {rc}")
                return self._handle_start_client_failure(
                    exc=RuntimeError(f"EVE.app exited early with code {rc}"),
                    reason=f"EVE.app exited early with code {rc}",
                    _retry_after_downtime=_retry_after_downtime,
                )
            if self.rpc.ping():
                log_info(f"In-client JSON-RPC bridge connected (client PID {self.client_proc.pid})")
                break
            time.sleep(0.5)
        else:
            rpc_err = f"Timed out waiting for in-client JSON-RPC hook on port {self.ports['rpc']}"
            log_err(rpc_err)
            return self._handle_start_client_failure(
                exc=TimeoutError(rpc_err),
                reason=rpc_err,
                _retry_after_downtime=_retry_after_downtime,
            )

        # Bootstrap login -> character selection -> hangar/space ready
        log_info("Bootstrapping client session into character hangar/space...")
        try:
            boot = self.rpc.call(
                "bootstrap_session",
                {"timeout_ms": 90000, "select_Wait_ms": 90000},
                timeout=110.0,
            )
            self._sync_initial_session_flight_calls(boot)
            self._ensure_run_guard_state().absorb_payload(boot)
            log_info(
                f"Session ready: charid={boot.get('session', {}).get('charid')} "
                f"view={boot.get('active_view')} stationid={boot.get('session', {}).get('stationid')} "
                f"solarsystemid={boot.get('session', {}).get('solarsystemid')}"
            )
            return True
        except Exception as exc:
            log_err(f"bootstrap_session failed: {exc}")
            return self._handle_start_client_failure(
                exc=exc,
                reason=f"bootstrap_session rejected: {exc}",
                _retry_after_downtime=_retry_after_downtime,
            )

    def stop_client(self) -> None:
        self._stop_host_gm_bridge_server()
        if self.client_proc:
            pid = self.client_proc.pid
            if self.client_proc.poll() is None:
                log_info(f"Stopping EVE.app client (PID {pid})...")
            try:
                os.killpg(pid, signal.SIGKILL)
            except OSError:
                try:
                    self.client_proc.kill()
                except OSError:
                    pass
            try:
                self.client_proc.wait(timeout=6)
            except Exception:
                pass
        pre_cm = getattr(self, "_pre_crashmon_pids", set())
        for cm_pid in self._crashmon_pids() - pre_cm:
            try:
                os.kill(cm_pid, signal.SIGKILL)
            except OSError:
                pass
        self.client_proc = None
        self.corrupted_viewstate_streak = 0
        if hasattr(self, "_session_flight_calls"):
            self._session_flight_calls.clear()
        if hasattr(self, "_session_flight_seqs"):
            self._session_flight_seqs.clear()
        if getattr(self, "_guard_tracker", None) is not None:
            self._guard_tracker.reset_client_session()
        if getattr(self, "_user_error_exemption_tracker", None) is not None:
            self._user_error_exemption_tracker.reset_client_session()
        oracle = getattr(self, "oracle", None)
        if oracle is not None:
            oracle.reset_session_telemetry(
                client_boot_build=_read_client_boot_build(getattr(self, "staged_app", None)),
                server_boot_build=_read_server_boot_build(getattr(self, "worktree_dir", REPO_ROOT)),
            )

    def hot_reload_server_boundary(self) -> bool:
        """
        Fetch latest `origin/main` (unless `--no-git-checkout`), compile `eve-server`,
        and restart `eve-server` + `EVE.app` while preserving `harness_world.sqlite`
        and `memory.json`.
        """
        log_info("=== HOT-RELOADING SERVER BOUNDARY ===")
        self.reload_requested = False
        try:
            DEFAULT_CONTROL_FILE.unlink(missing_ok=True)
        except Exception:
            pass

        if not self.args.no_git_checkout:
            sync_worktree_to_origin_main(self.worktree_dir, fetch=True, heal_drift=True)

        ok, _, err_tail = compile_eve_server(self.worktree_dir)
        if not ok:
            log_err(f"Hot-reload build failed; keeping previous binary and running server/client:\n{err_tail}")
            return False

        self.stop_client()
        self.stop_server()

        new_head = get_git_head(self.worktree_dir)
        self.cached_git_head = new_head
        norm_stats = self.memory.normalize_coverage_frontier(
            git_head=new_head,
            server_bin_mtime=self._server_bin_mtime(),
            save=False,
        )
        self.memory.data["server_reloads"] = int(self.memory.data.get("server_reloads", 0)) + 1
        self.memory.data.setdefault("long_term_notes", []).append(
            f"[reload #{self.memory.data['server_reloads']}] Server hot-reloaded to commit {new_head} "
            f"(world DB preserved; decayed {norm_stats.get('buggy_decayed', 0)} stale buggy features)."
        )
        self.memory.save()
        self.reset_batch_state()

        if not self.start_server(reset_db=False):
            return False
        if not self.start_client():
            return False

        self.oracle.reset_log_offsets(
            self.current_run_dir / "client_tracebacks.log",
            self.current_run_dir / "server.log",
        )
        return True

    def execute_step(
        self,
        step_num: int,
        last_flight_recorder: Dict[str, Any],
        last_burst_paths: List[Path],
    ) -> Tuple[Dict[str, Any], List[Path]]:
        step_dir = self.current_run_dir / "steps" / f"step_{step_num:04d}"
        step_dir.mkdir(parents=True, exist_ok=True)
        git_head = getattr(self, "cached_git_head", None) or get_git_head(self.worktree_dir)

        cur_persona_id = str((self.memory.data.get("active_persona") or {}).get("id") or "")
        if cur_persona_id != self.last_active_persona_id:
            self.last_active_persona_id = cur_persona_id
            self.reset_batch_state()
            self.pending_persona_cleanup = True

        run_window_cleanup = bool(self.pending_persona_cleanup)
        run_hygiene_cleanup = bool(
            self.pending_persona_cleanup or getattr(self, "pending_startup_hygiene", False)
        )
        if run_window_cleanup or run_hygiene_cleanup:
            self.pending_persona_cleanup = False
            self.pending_startup_hygiene = False
            self.cached_post_state = None
            self.cached_post_state_ts = 0.0
            self._force_next_screenshot = True
            closed_list: List[Any] = []
            closed_sec = None
            deleted_contracts: List[Any] = []
            stacked_targets: List[Any] = []
            if run_window_cleanup:
                try:
                    cln_res = self.rpc.call("cleanup_utility_windows", {"keep_chat": True}, timeout=10.0)
                    cln_body = cln_res.get("cleanup_result") or cln_res.get("result") or cln_res
                    closed_list = list(cln_body.get("closed_windows") or [])
                    closed_sec = cln_body.get("closed_secondary_view")
                    log_info(
                        f"Persona rotation cleanup -> closed {len(closed_list)} utility window(s)"
                        + (f" ({', '.join(closed_list[:6])})" if closed_list else "")
                        + (f", secondary_view={closed_sec}" if closed_sec else "")
                    )
                except Exception as exc:
                    log_warn(f"Persona rotation cleanup_utility_windows skipped: {exc}")
            if run_hygiene_cleanup:
                try:
                    hyg_res = self.rpc.call(
                        "cleanup_persona_hygiene",
                        {
                            "cleanup_contracts": True,
                            "stack_hangar": True,
                            "stack_cargo": True,
                            "suppress_flight": True,
                        },
                        timeout=10.0,
                    )
                    hyg_body = hyg_res.get("hygiene_result") or hyg_res.get("result") or hyg_res
                    deleted_contracts = list(hyg_body.get("deleted_contracts") or [])
                    stacked_targets = list(hyg_body.get("stacked_targets") or [])
                    hyg_errs = list(hyg_body.get("errors") or [])
                    if deleted_contracts or stacked_targets or hyg_errs:
                        log_info(
                            f"Persona hygiene cleanup -> deleted_contracts={deleted_contracts}, "
                            f"stacked_targets={stacked_targets}"
                            + (f", errors={hyg_errs}" if hyg_errs else "")
                        )
                except Exception as exc:
                    log_warn(f"Persona hygiene cleanup_persona_hygiene skipped: {exc}")
            if closed_list or closed_sec or deleted_contracts or stacked_targets:
                last_flight_recorder = dict(last_flight_recorder or {})
                last_flight_recorder["persona_rotation_cleanup"] = {
                    "closed_windows": list(closed_list),
                    "closed_secondary_view": closed_sec,
                    "deleted_contracts": list(deleted_contracts),
                    "stacked_targets": list(stacked_targets),
                    "note": (
                        "Harness intentionally performed window, contract, and inventory-stack "
                        "hygiene before capturing this step's UI state."
                    ),
                }
                time.sleep(0.05)

        # 1. Reuse previous step's post_state when fresh (`<= 3.0s`), falling back to `get_state` RPC
        cached_ps = getattr(self, "cached_post_state", None)
        cached_ts = float(getattr(self, "cached_post_state_ts", 0.0) or 0.0)
        if isinstance(cached_ps, dict) and cached_ps and (time.time() - cached_ts) <= 3.0:
            ui_snapshot = cached_ps
            self.cached_post_state = None
        else:
            ui_snapshot = self.rpc.call("get_state", timeout=15.0)
        self.last_ui_snapshot = ui_snapshot
        (step_dir / "ui_state.json").write_text(
            json.dumps(ui_snapshot, separators=(",", ":")), encoding="utf-8"
        )
        log_step_header(step_num, git_head, self.memory, ui_snapshot)

        # Pre-compute hybrid branch & whether a pre-action screenshot is actually needed
        non_benign_log_excs: List[Any] = []
        non_benign_macho_excs: List[Any] = []
        fr_has_hard_error = False
        need_planner = False
        next_step_spec: Dict[str, Any] = {}
        if self.hybrid_mode:
            prev_ui_for_fr = (
                getattr(getattr(self, "oracle", None), "_last_step_ui_state", None)
                or ui_snapshot
            )
            sanitized_prev_fr = PlaytesterBugOracle.partition_flight_recorder_exceptions(
                last_flight_recorder,
                ui_state=prev_ui_for_fr,
            )
            if isinstance(sanitized_prev_fr, dict):
                non_benign_log_excs = list(sanitized_prev_fr.get("log_exceptions") or [])
                non_benign_macho_excs = list(sanitized_prev_fr.get("macho_exceptions") or [])
            fr_has_hard_error = bool(
                isinstance(sanitized_prev_fr, dict)
                and (
                    non_benign_log_excs
                    or sanitized_prev_fr.get("quasar_errors")
                    or non_benign_macho_excs
                )
            )
            max_planned_peek = int(
                (self.active_batch_plan or {}).get("max_batch_steps")
                or len((self.active_batch_plan or {}).get("steps") or [])
                or 1
            )
            need_planner = (
                self.active_batch_plan is None
                or self.batch_completed_flag
                or self.batch_step_index >= max_planned_peek
                or fr_has_hard_error
                or bool(getattr(self, "_escalate_to_diagnostic_planner", False))
            )
            if not need_planner:
                _steps_peek = (self.active_batch_plan or {}).get("steps") or []
                _idx_peek = self.batch_step_index  # next step is 1-based batch_step_index + 1 -> 0-based batch_step_index
                if 0 <= _idx_peek < len(_steps_peek) and isinstance(_steps_peek[_idx_peek], dict):
                    next_step_spec = _steps_peek[_idx_peek]

        screenshot_path = step_dir / "screen.png"
        client_pid = self.client_proc.pid if self.client_proc else None
        _stream_pool_peek = getattr(self, "agy_pool", None)
        _verif_goal_peek = (
            re.search(r"#\d{3,5}\b", str(self.memory.data.get("current_goal") or ""))
            or re.search(r"#\d{3,5}\b", str((self.active_batch_plan or {}).get("plan_goal") or ""))
        )
        _diag_peek = bool(
            need_planner
            and (
                fr_has_hard_error
                or getattr(self, "_pending_assertion_escalation", None) is not None
                or getattr(self, "_escalate_to_diagnostic_planner", False)
                or _verif_goal_peek
            )
        )
        need_pre_screenshot = bool(
            (not self.hybrid_mode)
            or _diag_peek
            or (_stream_pool_peek is None and need_planner)
            or (_stream_pool_peek is None and str(next_step_spec.get("exec_mode") or "direct").strip().lower() == "llm")
            or fr_has_hard_error
            or getattr(self, "_force_next_screenshot", False)
            or bool(next_step_spec.get("capture_screenshot"))
        )
        self._force_next_screenshot = False
        cap_meta: Dict[str, Any] = {}

        def _ensure_screenshot_captured(dest: Optional[Path] = None) -> Dict[str, Any]:
            nonlocal cap_meta
            target_p = dest or screenshot_path
            if dest is None and target_p.exists() and cap_meta:
                return cap_meta
            meta = capture_window_screenshot(
                target_p,
                client_pid=client_pid,
                bring_foreground=self.args.foreground,
                rpc_client=self.rpc,
                max_dim=self.screenshot_max_dim,
            )
            if dest is None:
                cap_meta = meta
            return meta

        if need_pre_screenshot:
            _ensure_screenshot_captured()

        # 2. Reason / Direct Batch Execution
        current_step_spec: Dict[str, Any] = {}
        stream_pool = getattr(self, "agy_pool", None)
        if not self.hybrid_mode:
            prompt_text = build_reasoning_prompt(
                step_num=step_num,
                screenshot_path=screenshot_path,
                burst_paths=last_burst_paths,
                ui_snapshot=ui_snapshot,
                last_flight_recorder=last_flight_recorder,
                memory=self.memory,
                git_head=git_head,
            )
            decision, used_model = invoke_agy_reasoning(
                prompt_text=prompt_text,
                step_dir=step_dir,
                model_chain=self.model_chain,
                timeout_secs=self.args.agy_timeout,
            )
            decision["_mode"] = "legacy"
            decision["_used_model"] = used_model
        else:
            cur_persona_id = str((self.memory.data.get("active_persona") or {}).get("id") or "")
            if cur_persona_id != self.last_active_persona_id:
                self.last_active_persona_id = cur_persona_id
                self.reset_batch_state()

            planner_ran_this_step = False
            planner_model_this_step = ""
            planner_elapsed_this_step = 0.0
            planner_decision_this_step: Optional[Dict[str, Any]] = None

            if need_planner:
                auto_esc_ctx: Optional[Dict[str, Any]] = getattr(self, "_pending_assertion_escalation", None)
                if auto_esc_ctx is None and fr_has_hard_error and self.active_batch_plan is not None:
                    auto_esc_ctx = {
                        "trigger": "flight_recorder_exception",
                        "batch_id": self.batch_id,
                        "batch_step_index": self.batch_step_index,
                        "log_exceptions": non_benign_log_excs,
                        "quasar_errors": last_flight_recorder.get("quasar_errors"),
                        "macho_exceptions": non_benign_macho_excs,
                        "action_error": last_flight_recorder.get("action_error"),
                    }
                prev_goal_str = str(self.memory.data.get("current_goal") or "")
                prev_plan_goal = str((self.active_batch_plan or {}).get("plan_goal") or "")
                verifying_fix_signoff = bool(
                    re.search(r"#\d{3,5}\b", prev_goal_str) or re.search(r"#\d{3,5}\b", prev_plan_goal)
                )
                use_diagnostic_gear = bool(
                    fr_has_hard_error
                    or auto_esc_ctx is not None
                    or getattr(self, "_escalate_to_diagnostic_planner", False)
                    or verifying_fix_signoff
                )
                self._escalate_to_diagnostic_planner = False
                self._pending_assertion_escalation = None
                chosen_planner_chain = (
                    self.planner_model_chain
                    if use_diagnostic_gear
                    else getattr(self, "routine_planner_model_chain", self.executor_model_chain)
                )
                if use_diagnostic_gear and not screenshot_path.exists():
                    _ensure_screenshot_captured()
                planner_prompt = build_planner_prompt(
                    step_num=step_num,
                    screenshot_path=screenshot_path,
                    burst_paths=last_burst_paths,
                    ui_snapshot=ui_snapshot,
                    last_flight_recorder=last_flight_recorder,
                    memory=self.memory,
                    git_head=git_head,
                    escalation_context=auto_esc_ctx,
                    compact_mode=not use_diagnostic_gear,
                    use_compact_json_contract=bool(stream_pool is not None and not use_diagnostic_gear),
                )
                plan_decision, planner_model, planner_elapsed = invoke_agy_planner(
                    prompt_text=planner_prompt,
                    step_dir=step_dir,
                    model_chain=chosen_planner_chain,
                    timeout_secs=self.args.agy_timeout,
                    stream_pool=stream_pool,
                    diagnostic=use_diagnostic_gear,
                )
                self.memory.apply_planner_decision(step_num, plan_decision)
                self.batch_id += 1
                self.active_batch_plan = plan_decision.get("delegated_plan") or {}
                self.active_planner_decision = plan_decision
                self.active_planner_model = planner_model
                self.batch_conversation_id = None
                self.batch_step_index = 1
                self.batch_completed_flag = False
                self.batch_history = []
                planner_ran_this_step = True
                planner_model_this_step = planner_model
                planner_elapsed_this_step = planner_elapsed
                planner_decision_this_step = plan_decision
            else:
                self.batch_step_index += 1

            planned_steps = (self.active_batch_plan or {}).get("steps") or []
            max_batch_steps = int(
                (self.active_batch_plan or {}).get("max_batch_steps")
                or len(planned_steps)
                or 1
            )
            if 0 <= (self.batch_step_index - 1) < len(planned_steps) and isinstance(
                planned_steps[self.batch_step_index - 1], dict
            ):
                current_step_spec = planned_steps[self.batch_step_index - 1]
            elif planned_steps and isinstance(planned_steps[-1], dict):
                current_step_spec = planned_steps[-1]

            step_exec_mode = str(current_step_spec.get("exec_mode") or "direct").strip().lower()
            resolved_direct: Optional[Dict[str, Any]] = None
            miss_reason = ""
            if step_exec_mode != "llm":
                resolved_direct, miss_reason = _resolve_direct_action(current_step_spec, ui_snapshot)

            conv_id = self.batch_conversation_id or ""
            if resolved_direct is not None:
                exec_decision = {
                    "observation": (
                        f"Direct execution of planned Batch #{self.batch_id} Step {self.batch_step_index}/{max_batch_steps}."
                    ),
                    "previous_action_verification": {
                        "expectation_met": True,
                        "discrepancy_summary": "",
                    },
                    "reasoning": str(
                        current_step_spec.get("instruction")
                        or current_step_spec.get("intent")
                        or "Execute concrete planned step directly in Python."
                    ),
                    "batch_step_number": self.batch_step_index,
                    "batch_status": "completed" if self.batch_step_index >= max_batch_steps else "continue",
                    "escalation_reason": "",
                    "action": resolved_direct,
                }
                exec_model = "direct-planner-step1" if planner_ran_this_step else "direct-runner"
                exec_elapsed = 0.0
            else:
                if miss_reason:
                    log_info(
                        f"Step {step_num}: direct selector miss ({miss_reason}); "
                        f"falling back to warm Executor ({self.executor_model_chain[0]})."
                    )
                if stream_pool is None and not screenshot_path.exists():
                    _ensure_screenshot_captured()
                exec_prompt = build_executor_prompt(
                    step_num=step_num,
                    batch_id=self.batch_id,
                    batch_step_idx=self.batch_step_index,
                    delegated_plan=self.active_batch_plan or {},
                    screenshot_path=screenshot_path,
                    burst_paths=last_burst_paths,
                    ui_snapshot=ui_snapshot,
                    last_flight_recorder=last_flight_recorder,
                    memory=self.memory,
                    batch_history=self.batch_history,
                    use_compact_json_contract=bool(stream_pool is not None),
                )
                exec_decision, exec_model, conv_id, exec_elapsed = invoke_agy_executor(
                    prompt_text=exec_prompt,
                    step_dir=step_dir,
                    model_chain=self.executor_model_chain,
                    conversation_id=self.batch_conversation_id,
                    batch_id=self.batch_id,
                    batch_step_idx=self.batch_step_index,
                    max_batch_steps=max_batch_steps,
                    timeout_secs=min(self.args.agy_timeout, 40),
                    stream_pool=stream_pool,
                )
                self.batch_conversation_id = conv_id or self.batch_conversation_id

            # Check if Executor escalated back to Parent Planner mid-batch
            exec_pav = exec_decision.get("previous_action_verification") or {}
            executor_escalated = (
                not planner_ran_this_step
                and (
                    exec_decision.get("batch_status") == "escalate"
                    or (self.batch_step_index > 1 and exec_pav.get("expectation_met") is False)
                )
            )
            if executor_escalated:
                if not screenshot_path.exists():
                    _ensure_screenshot_captured()
                try:
                    _fresh_ui = self.rpc.call("get_state", timeout=15.0)
                    if isinstance(_fresh_ui, dict) and _fresh_ui:
                        _prev_fr = ui_snapshot.get("flight_recorder_delta") if isinstance(ui_snapshot, dict) else None
                        _cur_fr = _fresh_ui.get("flight_recorder_delta")
                        if isinstance(_prev_fr, list) and isinstance(_cur_fr, list):
                            _fresh_ui["flight_recorder_delta"] = _prev_fr + _cur_fr
                        ui_snapshot = _fresh_ui
                        self.last_ui_snapshot = ui_snapshot
                        (step_dir / "ui_state.json").write_text(
                            json.dumps(ui_snapshot, separators=(",", ":")), encoding="utf-8"
                        )
                except Exception:
                    pass
                esc_payload = {
                    "trigger": "executor_escalate",
                    "batch_id": self.batch_id,
                    "failed_at_batch_step": self.batch_step_index,
                    "executor_model": exec_model,
                    "executor_observation": exec_decision.get("observation"),
                    "executor_reasoning": exec_decision.get("reasoning"),
                    "escalation_reason": exec_decision.get("escalation_reason"),
                    "previous_action_verification": exec_pav,
                    "batch_history": self.batch_history[-4:],
                }
                (step_dir / "executor_escalation.json").write_text(
                    json.dumps(esc_payload, indent=2),
                    encoding="utf-8",
                )
                log_warn(
                    f"Step {step_num}: Executor ({exec_model}) escalated at Batch #{self.batch_id} "
                    f"Step {self.batch_step_index}/{max_batch_steps} "
                    f"(reason={exec_decision.get('escalation_reason') or exec_pav.get('discrepancy_summary')}) "
                    f"-> handing back to Diagnostic Parent Planner!"
                )
                planner_prompt = build_planner_prompt(
                    step_num=step_num,
                    screenshot_path=screenshot_path,
                    burst_paths=last_burst_paths,
                    ui_snapshot=ui_snapshot,
                    last_flight_recorder=last_flight_recorder,
                    memory=self.memory,
                    git_head=git_head,
                    escalation_context=esc_payload,
                )
                plan_decision, planner_model, planner_elapsed = invoke_agy_planner(
                    prompt_text=planner_prompt,
                    step_dir=step_dir,
                    model_chain=self.planner_model_chain,
                    timeout_secs=self.args.agy_timeout,
                    stream_pool=stream_pool,
                )
                self.memory.apply_planner_decision(step_num, plan_decision)
                self.batch_id += 1
                self.active_batch_plan = plan_decision.get("delegated_plan") or {}
                self.active_planner_decision = plan_decision
                self.active_planner_model = planner_model
                self.batch_conversation_id = None
                self.batch_step_index = 1
                self.batch_completed_flag = False
                self.batch_history = []
                planner_ran_this_step = True
                planner_model_this_step = planner_model
                planner_elapsed_this_step = planner_elapsed
                planner_decision_this_step = plan_decision

                planned_steps = (self.active_batch_plan or {}).get("steps") or []
                max_batch_steps = int(
                    (self.active_batch_plan or {}).get("max_batch_steps")
                    or len(planned_steps)
                    or 1
                )
                current_step_spec = planned_steps[0] if planned_steps and isinstance(planned_steps[0], dict) else {}
                step1_resolved: Optional[Dict[str, Any]] = None
                if str(current_step_spec.get("exec_mode") or "direct").strip().lower() != "llm":
                    step1_resolved, _ = _resolve_direct_action(current_step_spec, ui_snapshot)
                if step1_resolved is not None:
                    exec_decision = {
                        "observation": f"Direct Step 1 after escalation Planner Batch #{self.batch_id}.",
                        "previous_action_verification": {"expectation_met": True, "discrepancy_summary": ""},
                        "reasoning": str(current_step_spec.get("instruction") or "Execute Step 1 directly."),
                        "batch_step_number": 1,
                        "batch_status": "completed" if max_batch_steps <= 1 else "continue",
                        "escalation_reason": "",
                        "action": step1_resolved,
                    }
                    exec_model = "direct-planner-step1"
                else:
                    exec_prompt = build_executor_prompt(
                        step_num=step_num,
                        batch_id=self.batch_id,
                        batch_step_idx=self.batch_step_index,
                        delegated_plan=self.active_batch_plan or {},
                        screenshot_path=screenshot_path,
                        burst_paths=last_burst_paths,
                        ui_snapshot=ui_snapshot,
                        last_flight_recorder=last_flight_recorder,
                        memory=self.memory,
                        batch_history=self.batch_history,
                    )
                    exec_decision, exec_model, conv_id, exec_elapsed_2 = invoke_agy_executor(
                        prompt_text=exec_prompt,
                        step_dir=step_dir,
                        model_chain=self.executor_model_chain,
                        conversation_id=None,
                        batch_id=self.batch_id,
                        batch_step_idx=self.batch_step_index,
                        max_batch_steps=max_batch_steps,
                        timeout_secs=min(self.args.agy_timeout, 40),
                        stream_pool=stream_pool,
                    )
                    self.batch_conversation_id = conv_id or None
                    exec_elapsed += exec_elapsed_2

            # Assemble unified decision record for step_dir / "decision.json" and bug oracle
            if planner_ran_this_step and planner_decision_this_step:
                step_pav = planner_decision_this_step.get("previous_action_verification") or {
                    "expectation_met": True,
                    "discrepancy_summary": "",
                }
                step_bug_report = planner_decision_this_step.get("bug_report") or {"should_file": False}
                step_mem_upd = planner_decision_this_step.get("memory_update") or {}
                step_goal = str(planner_decision_this_step.get("current_goal") or self.memory.data.get("current_goal") or "")
            else:
                step_pav = exec_decision.get("previous_action_verification") or {
                    "expectation_met": True,
                    "discrepancy_summary": "",
                }
                step_bug_report = {
                    "should_file": False,
                    "category": "none",
                    "title": "",
                    "severity": "medium",
                    "reproduction_steps": [],
                    "expected_behavior": "",
                    "actual_behavior": "",
                    "subsystem": "",
                }
                step_mem_upd = {}
                step_goal = str(self.memory.data.get("current_goal") or "")

            decision = {
                "observation": exec_decision.get("observation") or "",
                "previous_action_verification": step_pav,
                "reasoning": exec_decision.get("reasoning") or "",
                "current_goal": step_goal,
                "action": exec_decision.get("action") or {},
                "memory_update": step_mem_upd,
                "bug_report": step_bug_report,
                "_mode": "hybrid",
                "_used_model": exec_model,
                "_executor_model": exec_model,
                "_executor_elapsed_sec": round(exec_elapsed, 2),
                "_planner_ran": planner_ran_this_step,
                "_planner_model": planner_model_this_step if planner_ran_this_step else self.active_planner_model,
                "_planner_elapsed_sec": round(planner_elapsed_this_step, 2),
                "_step_reasoning_elapsed_sec": round(planner_elapsed_this_step + exec_elapsed, 2),
                "_batch_id": self.batch_id,
                "_batch_step_number": self.batch_step_index,
                "_max_batch_steps": max_batch_steps,
                "_batch_status": exec_decision.get("batch_status"),
                "_batch_conversation_id": self.batch_conversation_id,
                "_delegated_plan_goal": (self.active_batch_plan or {}).get("plan_goal"),
            }
            if exec_decision.get("_thinking_trace"):
                decision["_thinking_trace"] = exec_decision["_thinking_trace"]
            if exec_decision.get("_tool_calls"):
                decision["_tool_calls"] = exec_decision["_tool_calls"]
            if exec_decision.get("_usage"):
                decision["_executor_usage"] = exec_decision["_usage"]
            if planner_ran_this_step and planner_decision_this_step and planner_decision_this_step.get("_usage"):
                decision["_planner_usage"] = planner_decision_this_step["_usage"]

        (step_dir / "decision.json").write_text(
            json.dumps(decision, indent=2), encoding="utf-8"
        )

        # 3. Execute the chosen action in-client (with in-hook burst capture for warp_to)
        action = decision.get("action") or {}
        act_type = str(action.get("type") or "wait")
        burst_paths: List[Path] = []
        exec_action_payload = dict(action)
        if act_type == "warp_to":
            exec_action_payload["burst_paths"] = [
                str(step_dir / f"burst_{b_idx}.png") for b_idx in range(1, 4)
            ]

        rpc_timeout = _compute_step_rpc_timeout(action)
        exec_res: Dict[str, Any] = {}
        valid_action, reject_reason = validate_exploratory_action(
            action,
            step_num=step_num,
            persona_id=self._active_persona_id(),
        )
        if not valid_action:
            log_warn(f"Step {step_num}: {reject_reason}")
            exec_res = {
                "ok": False,
                "action_ok": False,
                "action_error": reject_reason,
                "rejected_by_validator": True,
                "flight_recorder": {},
            }
        else:
            canon_act_type = _canonicalize_action_type(
                str(exec_action_payload.get("type") or ""),
                bool(exec_action_payload.get("target")),
            )
            if self._should_dispatch_via_gm_bridge(exec_action_payload, canon_act_type):
                try:
                    target_cid = int(
                        ((ui_snapshot or {}).get("session") or {}).get("charid")
                        or self._default_target_char_id()
                        or 90000001
                    )
                except Exception:
                    target_cid = self._default_target_char_id() or 90000001
                exec_res = self._execute_gm_or_time_control_via_bridge(
                    exec_action_payload,
                    canon_act_type,
                    target_cid,
                )
                try:
                    obs = self.rpc.call("get_state", timeout=15.0) or {}
                    refreshed = _unwrap_state_payload(obs)
                    if refreshed:
                        exec_res["post_state"] = refreshed
                except Exception:
                    pass
            else:
                prev_allow_tc = getattr(self, "_active_step_allows_host_time_control", None)
                explicit_tc = exec_action_payload.get("time_control")
                self._active_step_allows_host_time_control = (
                    bool(explicit_tc) if isinstance(explicit_tc, bool) else True
                )
                try:
                    exec_res = self.rpc.call("execute_action", {"action": exec_action_payload}, timeout=rpc_timeout)
                except Exception as exc:
                    exec_res = {"action_ok": False, "action_error": str(exc), "flight_recorder": {}}
                finally:
                    self._active_step_allows_host_time_control = prev_allow_tc
        if isinstance(exec_res, dict):
            self._record_session_flight_calls((exec_res.get("flight_recorder") or {}).get("macho_calls"))

        post_state_raw = exec_res.pop("post_state", None) if isinstance(exec_res, dict) else None
        if isinstance(post_state_raw, dict) and post_state_raw:
            self.cached_post_state = post_state_raw
            self.cached_post_state_ts = time.time()
        else:
            self.cached_post_state = None
            self.cached_post_state_ts = 0.0

        # If `click_xy` or widget click fell back to coordinates and foreground OS clicks are enabled
        if (
            self.args.os_input_fallback
            and act_type in ("click_xy", "click_widget")
            and action.get("x") is not None
            and action.get("y") is not None
        ):
            self.cached_post_state = None
            self.cached_post_state_ts = 0.0
            if not cap_meta:
                _ensure_screenshot_captured()
            desktop = ui_snapshot.get("desktop") or {}
            d_size = (int(desktop.get("width", 0) or 0), int(desktop.get("height", 0) or 0))
            quartz_mouse_click(
                int(action["x"]),
                int(action["y"]),
                right_click=bool(action.get("right_click")),
                double_click=bool(action.get("double_click")),
                win_bounds=cap_meta.get("window"),
                client_desktop_size=d_size,
            )

        hook_captured_bursts = (
            exec_res.get("captured_burst_paths") if isinstance(exec_res, dict) else None
        )
        if isinstance(hook_captured_bursts, list) and hook_captured_bursts:
            burst_paths = [Path(p) for p in hook_captured_bursts if Path(p).exists()]
        elif act_type in ("warp_to", "undock", "dock") and (
            (not self.hybrid_mode) or bool(current_step_spec.get("capture_screenshot"))
        ):
            for b_idx in range(1, 4):
                bp = step_dir / f"burst_{b_idx}.png"
                capture_window_screenshot(
                    bp,
                    client_pid=client_pid,
                    bring_foreground=False,
                    rpc_client=self.rpc,
                    max_dim=self.screenshot_max_dim,
                )
                if bp.exists():
                    burst_paths.append(bp)
                time.sleep(0.35)

        post_fr = (exec_res.get("flight_recorder") or {}) if isinstance(exec_res, dict) else {}
        post_expected_text = str(action.get("expected_ui_and_state_change") or "")
        step_eval_state = _select_live_skill_state(post_state_raw, ui_snapshot)

        self._record_step_guard_hits(step_num, exec_res)
        self._absorb_step_sim_time(exec_res)
        self._record_step_user_error_exemptions(
            step_num,
            exec_res,
            expected_text=post_expected_text,
            ui_state=step_eval_state,
        )
        self._write_run_summary(step_num=step_num, final=False)
        (step_dir / "exec_result.json").write_text(
            json.dumps(exec_res, indent=2), encoding="utf-8"
        )
        log_step_execution_outcome(step_num, exec_res, decision)

        # Fast post-action assertion & error check for deterministic escalation
        sanitized_post_fr = PlaytesterBugOracle.partition_flight_recorder_exceptions(
            post_fr,
            expected_text=post_expected_text,
            ui_state=step_eval_state,
            step_num=step_num,
        )
        post_non_benign_logs = (
            list(sanitized_post_fr.get("log_exceptions") or [])
            if isinstance(sanitized_post_fr, dict)
            else []
        )
        post_non_benign_macho = (
            list(sanitized_post_fr.get("macho_exceptions") or [])
            if isinstance(sanitized_post_fr, dict)
            else []
        )
        post_hard_error = bool(
            post_non_benign_logs
            or post_fr.get("quasar_errors")
            or post_non_benign_macho
        )
        assert_ok, assert_err = _evaluate_step_assertions(
            current_step_spec.get("assertions") if self.hybrid_mode else None,
            exec_res,
            post_state_raw if isinstance(post_state_raw, dict) else ui_snapshot,
        )
        step_act_err = str(exec_res.get("action_error") or "").strip() if isinstance(exec_res, dict) else ""
        benign_reject = PlaytesterBugOracle.is_expected_user_error(
            step_act_err, post_expected_text, ui_state=step_eval_state
        )
        if not assert_ok and not benign_reject:
            log_warn(f"Step {step_num}: post-action assertion failed -> {assert_err}")
        downtime_modal_frags = (
            _extract_downtime_modal_texts(ui_snapshot)
            + _extract_downtime_modal_texts(post_state_raw)
            + [
                frag
                for frag in (_serialize_failure_fragment(post_fr.get("modals_opened")),)
                if frag
            ]
        )
        has_downtime_signal = bool(
            (isinstance(exec_res, dict) and exec_res.get("action_ok") is False)
            or post_hard_error
            or (not assert_ok and not benign_reject)
            or step_act_err
            or (
                isinstance(exec_res, dict)
                and (
                    exec_res.get("error")
                    or exec_res.get("traceback")
                    or exec_res.get("blocked_reason")
                    or exec_res.get("rpc_error")
                )
            )
            or (
                getattr(self, "client_proc", None) is not None
                and self.client_proc.poll() is not None
            )
            or any(
                _text_mentions_cluster_downtime(frag) or _text_mentions_disconnect(frag)
                for frag in downtime_modal_frags
            )
            or any(
                _text_mentions_cluster_downtime(frag)
                for frag in _extract_downtime_scatter_texts(post_fr)
            )
        )
        is_downtime_disconnect = (
            bool(getattr(self.args, "continuous", False))
            and has_downtime_signal
            and self._is_cluster_downtime_active(
                exec_res=exec_res,
                ui_snapshot=ui_snapshot,
                post_state=post_state_raw,
                last_flight_recorder=last_flight_recorder,
            )
        )
        if is_downtime_disconnect:
            self._downtime_pause_requested = True
            self._escalate_to_diagnostic_planner = False
            self._pending_assertion_escalation = None
            log_warn(
                f"Step {step_num}: daily cluster downtime disconnect detected; "
                "suppressing downtime bug bundle and scheduling clean pause."
            )
        else:
            if post_hard_error or (not assert_ok and not benign_reject):
                self._escalate_to_diagnostic_planner = True
                self._force_next_screenshot = True
                if self.hybrid_mode:
                    self.batch_completed_flag = True
                    self._pending_assertion_escalation = {
                        "trigger": "post_action_assertion_or_exception",
                        "batch_id": self.batch_id,
                        "batch_step_index": self.batch_step_index,
                        "action": action,
                        "action_ok": exec_res.get("action_ok", True),
                        "action_error": step_act_err,
                        "assertion_failure": "" if assert_ok else assert_err,
                        "log_exceptions": post_non_benign_logs,
                        "quasar_errors": post_fr.get("quasar_errors"),
                        "macho_exceptions": post_non_benign_macho,
                    }
                if not screenshot_path.exists():
                    _ensure_screenshot_captured()
            elif isinstance(exec_res, dict) and not exec_res.get("action_ok", True) and not benign_reject and self.hybrid_mode:
                # Soft execution failure on a direct step -> wake Planner next turn with failure context
                self._force_next_screenshot = True
                self.batch_completed_flag = True
                self._pending_assertion_escalation = {
                    "trigger": "direct_action_failed",
                    "batch_id": self.batch_id,
                    "batch_step_index": self.batch_step_index,
                    "action": action,
                    "action_error": step_act_err,
                }

        # Update Hybrid Executor batch history and completion flag
        if self.hybrid_mode:
            self.batch_history.append(
                {
                    "global_step": step_num,
                    "batch_step": self.batch_step_index,
                    "action_type": act_type,
                    "target": action.get("target") or action.get("command") or "",
                    "expected_ui_and_state_change": action.get("expected_ui_and_state_change"),
                    "action_ok": exec_res.get("action_ok", True),
                    "action_error": exec_res.get("action_error"),
                    "assertion_ok": assert_ok,
                    "assertion_error": assert_err,
                    "ui_changed": (exec_res.get("state_diff") or {}).get("ui_changed"),
                    "view_changed": (exec_res.get("state_diff") or {}).get("view_changed"),
                }
            )
            if (
                decision.get("_batch_status") == "completed"
                or self.batch_step_index >= int(decision.get("_max_batch_steps") or 1)
            ):
                self.batch_completed_flag = True

        # 4. Update persistent agent memory and evaluate zero-tolerance bug oracle
        self.memory.apply_agent_decision(step_num, decision, exec_res)
        new_persona_id = str((self.memory.data.get("active_persona") or {}).get("id") or "")
        if new_persona_id != self.last_active_persona_id:
            self.last_active_persona_id = new_persona_id
            self.reset_batch_state()
            self.pending_persona_cleanup = True
        if getattr(self, "_downtime_pause_requested", False):
            self._advance_oracle_log_offsets()
        else:
            self.oracle.evaluate_step(
                step_num=step_num,
                git_head=git_head,
                decision=decision,
                exec_res=exec_res,
                ui_snapshot=ui_snapshot,
                screenshot_path=screenshot_path,
                burst_paths=burst_paths,
                traceback_log=self.current_run_dir / "client_tracebacks.log",
                server_log=self.current_run_dir / "server.log",
                step_dir=step_dir,
                last_flight_recorder=last_flight_recorder,
                lazy_screenshot_fn=_ensure_screenshot_captured,
                post_state=post_state_raw if isinstance(post_state_raw, dict) else None,
            )
            oracle_exemptions = list(getattr(self.oracle, "last_user_error_exemptions", None) or [])
            if oracle_exemptions:
                extra_added, _ = self._record_step_user_error_exemptions(
                    step_num,
                    exec_res,
                    expected_text=post_expected_text,
                    ui_state=step_eval_state,
                    extra_hits=oracle_exemptions,
                )
                if extra_added:
                    if self.memory.data.get("recent_actions"):
                        last_ra = self.memory.data["recent_actions"][-1]
                        if isinstance(last_ra, dict) and last_ra.get("step") == step_num and isinstance(exec_res, dict):
                            last_ra["user_error_exemption_count"] = exec_res.get("user_error_exemption_count", 0)
                            last_ra["user_error_exemptions"] = list(exec_res.get("user_error_exemptions") or [])[:25]
                    self._write_run_summary(step_num=step_num, final=False)
                    if isinstance(exec_res, dict):
                        (step_dir / "exec_result.json").write_text(
                            json.dumps(exec_res, indent=2), encoding="utf-8"
                        )

        fr_delta = dict((exec_res.get("flight_recorder") or {}) if isinstance(exec_res, dict) else {})
        if self.oracle.last_unseen_tracebacks and not fr_delta.get("log_exceptions"):
            fr_delta["log_exceptions"] = list(self.oracle.last_unseen_tracebacks)
        if self.oracle.last_suppressed_tracebacks and not fr_delta.get("suppressed_log_exceptions"):
            fr_delta["suppressed_log_exceptions"] = list(self.oracle.last_suppressed_tracebacks)
        if isinstance(exec_res, dict) and exec_res.get("state_diff"):
            fr_delta["state_diff"] = exec_res["state_diff"]
        if isinstance(exec_res, dict) and exec_res.get("warp_telemetry"):
            fr_delta["warp_telemetry"] = exec_res["warp_telemetry"]
        if isinstance(exec_res, dict) and exec_res.get("action_error"):
            fr_delta["action_error"] = exec_res["action_error"]
        if isinstance(action, dict) and action.get("expected_ui_and_state_change"):
            fr_delta["expected_ui_and_state_change"] = str(action.get("expected_ui_and_state_change"))
        if isinstance(exec_res, dict) and exec_res.get("slash_result"):
            fr_delta["slash_result"] = str(exec_res["slash_result"])[:2000]
        if isinstance(exec_res, dict) and exec_res.get("traceback"):
            fr_delta["traceback"] = exec_res["traceback"]
        if not assert_ok and not benign_reject:
            fr_delta["assertion_failure"] = assert_err
        fr_delta = PlaytesterBugOracle.partition_flight_recorder_exceptions(
            fr_delta,
            expected_text=str(action.get("expected_ui_and_state_change") or ""),
            ui_state=step_eval_state,
            step_num=step_num,
        )
        return fr_delta, burst_paths

    def run(self) -> int:
        self.install_signal_handlers()
        try:
            if DEFAULT_CONTROL_FILE.exists():
                stale_cmd = DEFAULT_CONTROL_FILE.read_text(encoding="utf-8").strip()
                DEFAULT_CONTROL_FILE.unlink(missing_ok=True)
                log_warn(f"Cleared stale control file from previous run: {stale_cmd!r}")
        except Exception:
            pass
        mode_desc = (
            f"HYBRID(routine_planner={getattr(self, 'routine_planner_model_chain', self.executor_model_chain)}, "
            f"diag_planner={self.planner_model_chain}, executor={self.executor_model_chain}, stream_pool=True)"
            if self.hybrid_mode
            else f"LEGACY(models={self.model_chain})"
        )
        log_info(
            f"Starting EvoMu Playtester in worktree={self.worktree_dir} "
            f"(HEAD={get_git_head(self.worktree_dir)}, mode={mode_desc})"
        )

        if self.hybrid_mode and getattr(self, "agy_pool", None) is None:
            standby_specs: List[Tuple[Any, ...]] = []
            r_chain = getattr(self, "routine_planner_model_chain", None) or self.executor_model_chain
            r0 = r_chain[0] if r_chain else None
            e0 = self.executor_model_chain[0] if self.executor_model_chain else None
            if r0 and e0 and r0 == e0:
                standby_specs.append((None, r0, False, 2))
            else:
                if r0:
                    standby_specs.append((None, r0, False, 1))
                if e0:
                    standby_specs.append((None, e0, False, 1))
            self.agy_pool = AgyStreamWorkerPool(standby_specs=standby_specs, auto_respawn=True)
            self.agy_pool.prewarm_all()

        if not self.start_server(reset_db=self.args.reset_db):
            log_err("Initial eve-server startup failed.")
            return 1
        if not self.start_client():
            self.stop_server()
            log_err("Initial EVE.app client startup failed.")
            return 1

        self.oracle.reset_log_offsets(
            self.current_run_dir / "client_tracebacks.log",
            self.current_run_dir / "server.log",
        )

        step_num = int(self.memory.data.get("total_steps", 0))
        executed_this_invocation = 0
        self._ensure_run_guard_state()
        last_origin_check = time.time()
        last_fr: Dict[str, Any] = {}
        last_burst: List[Path] = []
        hook_file = HARNESS_DIR / "evomu_playtester_hook.py"
        last_hook_mtime = hook_file.stat().st_mtime if hook_file.exists() else 0.0

        try:
            while not self.stop_requested:
                if self.args.max_steps > 0 and executed_this_invocation >= self.args.max_steps:
                    log_info(f"Reached --max-steps {self.args.max_steps}; stopping.")
                    break

                should_reload, last_origin_check = self.check_external_reload_triggers(
                    last_origin_check
                )
                if self.stop_requested:
                    break
                if should_reload:
                    if not self.hot_reload_server_boundary():
                        log_err("Hot-reload failed; waiting 15s before retry...")
                        time.sleep(15)
                        continue
                    last_fr = {"server_hot_reloaded_to": get_git_head(self.worktree_dir)}
                    last_burst = []

                # Verify server & client are still alive; auto-recover if either died
                if self.server_proc is None or self.server_proc.poll() is not None:
                    log_warn("eve-server process exited unexpectedly! Restarting server + client...")
                    self.reset_batch_state()
                    last_fr = {"client_restarted_unexpectedly": "eve_server_exited"}
                    last_burst = []
                    if not self.start_server(reset_db=False) or not self.start_client():
                        time.sleep(10)
                        continue
                if self.client_proc is None or self.client_proc.poll() is not None or not self.rpc.ping():
                    if getattr(self.args, "continuous", False) and self._is_cluster_downtime_active():
                        resumed_fr = self._pause_and_resume_after_downtime(
                            reason="inter-step client/RPC unreachable during cluster downtime"
                        )
                        last_fr = resumed_fr if resumed_fr is not None else {}
                        last_burst = []
                        continue
                    log_warn("EVE.app client or RPC bridge unreachable! Restarting client...")
                    self.reset_batch_state()
                    last_fr = {"client_restarted_unexpectedly": "client_or_rpc_unreachable"}
                    last_burst = []
                    if not self.start_client():
                        time.sleep(10)
                        continue

                if hook_file.exists():
                    cur_hook_mtime = hook_file.stat().st_mtime
                    if cur_hook_mtime > last_hook_mtime:
                        try:
                            self.rpc.call("reload_hook", {}, timeout=10.0)
                            log_info("Hot-reloaded evomu_playtester_hook.py in live EVE.app client")
                        except Exception as exc:
                            log_warn(f"reload_hook RPC failed: {exc}")
                        last_hook_mtime = cur_hook_mtime

                step_num += 1
                executed_this_invocation += 1
                self.last_ui_snapshot = None
                try:
                    last_fr, last_burst = self.execute_step(step_num, last_fr, last_burst)
                except Exception as exc:
                    if getattr(self.args, "continuous", False) and self._is_cluster_downtime_active(exc=exc):
                        resumed_fr = self._pause_and_resume_after_downtime(
                            reason=f"step {step_num} exception: {exc}"
                        )
                        last_fr = resumed_fr if resumed_fr is not None else {}
                        last_burst = []
                        continue
                    else:
                        log_err(f"Step {step_num} raised exception: {exc}")
                        self.reset_batch_state()
                        time.sleep(2.0)
                else:
                    if getattr(self, "_downtime_pause_requested", False):
                        resumed_fr = self._pause_and_resume_after_downtime(
                            reason=f"step {step_num} cluster downtime disconnect"
                        )
                        last_fr = resumed_fr if resumed_fr is not None else {}
                        last_burst = []
                        continue
                    recovery_reason = self.maybe_recover_corrupted_viewstate(self.last_ui_snapshot)
                    if recovery_reason:
                        last_fr = {"client_restarted_for_desync": recovery_reason}
                        last_burst = []

                if not self.args.continuous and self.args.max_steps <= 0:
                    break
        finally:
            try:
                self.harvest_flight_recorder_anomalies(step_num=step_num)
            except Exception as exc:
                log_warn(f"Flight-recorder anomaly harvest skipped due to error: {exc}")
            try:
                self._ensure_run_guard_state().poll_rpc(getattr(self, "rpc", None), timeout=2.0)
                self._write_run_summary(step_num=step_num, final=True)
            except Exception as exc:
                log_warn(f"Final run_summary write skipped due to error: {exc}")
            if getattr(self, "agy_pool", None) is not None:
                try:
                    self.agy_pool.shutdown()
                except Exception:
                    pass
            try:
                self.oracle.flush(timeout=10.0)
            except Exception:
                pass
            self._close_gm_bridge()
            self.stop_client()
            self.stop_server()
            DEFAULT_PID_FILE.unlink(missing_ok=True)

        return 0


# ---------------------------------------------------------------------------
# Deterministic Zero-LLM Scenario Validator, Check Evaluator & Runner (#1202)
# ---------------------------------------------------------------------------

_SCENARIO_GM_ACTIONS = frozenset({"slash_command", "run_slash", "train_skill", "open_structure_cosmetics"})
_SCENARIO_TIME_CONTROL_ACTIONS = frozenset({
    "advance_server_time",
    "advance_upwell_timer",
    "complete_industry_jobs",
    "complete_skills",
    "clear_timers",
})
_SCENARIO_ALLOWED_ACTIONS = _DIRECT_NON_WIDGET_ACTIONS | frozenset({
    "click_widget",
    "click_xy",
    "drag_widget",
    "type_text",
    "open_window",
    "click_top_tabs",
    "show_info",
    "close_modal_or_window",
    "cleanup_utility_windows",
    "station_command",
    "space_command",
    "select_character",
    "select_overview",
    "save_location",
    "warp_to_location",
    "warp_to_bookmark",
    "set_destination",
    "engage_autopilot",
    "toggle_autopilot",
    "align",
    "align_to",
    "stop",
})
_TIME_CONTROL_SLASH_PREFIXES = (
    "/advancetime",
    "/completeskills",
    "/upwelltimer",
    "/completeindustry",
    "/cleartimers",
    "/fastforward",
    "/ff",
    "/finishjobs",
    "/finishskills",
    "/finishstructure",
    "/industrytimer",
    "/structuretimer",
)
_ALL_REMOTE_GM_SLASH_COMMANDS = frozenset({
    "/achievements",
    "/advancetime",
    "/autotidi",
    "/backintime",
    "/ban",
    "/campaign",
    "/chatban",
    "/chatunban",
    "/cleartimers",
    "/completeindustry",
    "/completeskills",
    "/contact",
    "/createccpevent",
    "/createitem",
    "/deadwarp",
    "/despawn",
    "/downtime",
    "/energy",
    "/fastcheckout",
    "/fastforward",
    "/ff",
    "/finishjobs",
    "/finishskills",
    "/finishstructure",
    "/fit",
    "/fwcapture",
    "/getattr",
    "/giveisk",
    "/giveitem",
    "/giveskill",
    "/heal",
    "/industrytimer",
    "/kick",
    "/kill",
    "/load",
    "/moveme",
    "/online",
    "/operation",
    "/pd",
    "/reinforce",
    "/role",
    "/season",
    "/secstatus",
    "/session",
    "/setattr",
    "/setomega",
    "/setskill",
    "/setunderage",
    "/shutdown",
    "/soak",
    "/spawn",
    "/spawnanomaly",
    "/spawnnpc",
    "/spawnwreck",
    "/standing",
    "/structure",
    "/structuretimer",
    "/survey",
    "/suspend",
    "/sysjunkclear",
    "/teleport",
    "/tidi",
    "/tr",
    "/trainskills",
    "/translocate",
    "/unban",
    "/unspawn",
    "/unsuspend",
    "/upwelltimer",
})
_GM_SLASH_IN_TEXT_PREFIXES = (
    "/give",
    "/tr",
    "/moveme",
    "/translocate",
    "/spawn",
    "/livingworld",
    "/unspawn",
    "/despawn",
    "/set",
    "/create",
    "/heal",
    "/kill",
    "/fit",
    "/complete",
    "/advance",
    "/clear",
    "/upwell",
    "/ban",
    "/role",
    "/teleport",
    "/kick",
    "/suspend",
    "/downtime",
    "/reinforce",
    "/backintime",
    "/shutdown",
    "/tidi",
    "/autotidi",
    "/fwcapture",
    "/finish",
    "/fastforward",
    "/ff",
)
_SCENARIO_ALLOWED_CHECK_KEYS = frozenset({
    "expect_window_open",
    "expect_window_closed",
    "expect_modal_absent",
    "expect_context_menu_item",
    "expect_ball_present",
    "expect_session_field",
    "no_error_reply",
    "no_client_exception",
    "include_startup_rpcs",
    "rpc_sent",
    "min_rpcs",
    "notification_received",
    "charsel_listed_all_characters",
    "window_populated",
    "ship_shown",
    "ship_mode",
    "celestials_in_overview",
    "target_distance",
    "warp_started",
    "warp_completed",
    "bookmark_present",
    "bookmark_server_check",
    "handshake_server_check",
    "no_server_unimplemented_rpc",
    "ui_delta_without_relog",
    "state_checks",
})
_SCENARIO_ALLOWED_RULE_OPS = frozenset({
    "equals",
    "not_equals",
    "non_empty",
    "contains",
    "subset",
    "prefix",
    "min",
    "max",
    "increased",
    "decreased",
    "changed",
    "delta",
})
_SCENARIO_ALLOWED_RULE_KEYS = _SCENARIO_ALLOWED_RULE_OPS | frozenset({"tolerance"})
_SCENARIO_ALLOWED_WINDOW_POPULATED_KEYS = frozenset({
    "window",
    "name",
    "min_populated",
    "min_tabs",
    "min_rows",
    "contains_text",
    "require_rpc_or_data",
    "allow_loading",
    "allow_error",
})
_SCENARIO_ALLOWED_WARP_COMPLETED_KEYS = frozenset({"target_id", "landing_distance_max_m"})
_SCENARIO_ALLOWED_WAIT_UNTIL_KEYS = frozenset({"checks", "timeout_ms", "poll_interval_ms"})
_SCENARIO_ERROR_LINE_RE = re.compile(
    r"UnimplementedRPC|panicked at|Traceback \(most recent|\bEXCEPTION\b|_LogException\b|\bLogException\b|\bLogTraceback\b|SendClientStackTraceAlert|client exception report saved"
)
_SCENARIO_DESYNC_LINE_RE = re.compile(
    r"Michelle\s+discarded\s+a\s+state\s+that\s+was\s+too\s+old|Failed\s+to\s+synchronize\s+to\s+\d+|stale-at-action|destiny[_\s-]*desync|sys_rx\s+lagged\s+by\s+\d+\s+ticks",
    re.IGNORECASE,
)


class ScenarioValidationError(ValueError):
    """Raised when a scenario file fails schema or GM/time-control policy validation."""


def load_hook_open_window_tables(hook_path: Optional[Path] = None) -> Tuple[Dict[str, str], Dict[str, str]]:
    """Read `cmd_map` (`OPEN_WINDOW_CMD_MAP`) and `OPEN_WINDOW_GUARDS` directly from `evomu_playtester_hook.py` AST."""
    target_path = Path(hook_path) if hook_path is not None else (HARNESS_DIR / "evomu_playtester_hook.py")
    tree = ast.parse(target_path.read_text(encoding="utf-8"), filename=str(target_path))
    cmd_map: Dict[str, str] = {}
    guards: Dict[str, str] = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            if node.targets[0].id in ("OPEN_WINDOW_CMD_MAP", "cmd_map") and isinstance(node.value, ast.Dict):
                val = ast.literal_eval(node.value)
                if isinstance(val, dict):
                    cmd_map = {str(k): str(v) for k, v in val.items()}
            elif node.targets[0].id == "OPEN_WINDOW_GUARDS" and isinstance(node.value, ast.Dict):
                val = ast.literal_eval(node.value)
                if isinstance(val, dict):
                    guards = {str(k): str(v) for k, v in val.items()}
    if not cmd_map:
        raise RuntimeError(f"OPEN_WINDOW_CMD_MAP/cmd_map not found in {target_path}")
    return cmd_map, guards


def _normalize_window_target_key(raw_target: str) -> str:
    return str(raw_target or "").strip().lower().replace("_", "").replace("-", "").replace(" ", "")


def _validate_value_rule_dict(rule: Dict[str, Any], ctx_label: str, *, allow_field: bool = False) -> None:
    allowed = set(_SCENARIO_ALLOWED_RULE_KEYS) | {"target_id"}
    if allow_field:
        allowed.add("field")
    unknown = sorted(set(rule.keys()) - allowed)
    if unknown:
        raise ScenarioValidationError(f"{ctx_label}: unsupported rule key(s) {unknown!r}")
    if allow_field and not str(rule.get("field") or "").strip():
        raise ScenarioValidationError(f"{ctx_label}: state check item missing non-empty 'field'")
    if not any(op in rule for op in _SCENARIO_ALLOWED_RULE_OPS):
        raise ScenarioValidationError(f"{ctx_label}: rule must specify at least one check operator")


def _validate_scenario_checks_dict(checks: Dict[str, Any], ctx_label: str) -> None:
    unknown = sorted(set(checks.keys()) - _SCENARIO_ALLOWED_CHECK_KEYS)
    if unknown:
        raise ScenarioValidationError(f"{ctx_label}: unsupported check key(s) {unknown!r}")
    sc = checks.get("state_checks")
    if sc is not None:
        if isinstance(sc, dict):
            for fname, rspec in sc.items():
                if isinstance(rspec, dict):
                    _validate_value_rule_dict(rspec, f"{ctx_label}.state_checks[{fname!r}]")
        elif isinstance(sc, list):
            for r_idx, item in enumerate(sc):
                if not isinstance(item, dict):
                    raise ScenarioValidationError(f"{ctx_label}.state_checks[{r_idx}] must be an object")
                _validate_value_rule_dict(item, f"{ctx_label}.state_checks[{r_idx}]", allow_field=True)
        else:
            raise ScenarioValidationError(f"{ctx_label}.state_checks must be a list or object")
    td = checks.get("target_distance")
    if td is not None:
        if not isinstance(td, dict):
            raise ScenarioValidationError(f"{ctx_label}.target_distance must be an object")
        _validate_value_rule_dict(td, f"{ctx_label}.target_distance")
    wp = checks.get("window_populated")
    if isinstance(wp, dict):
        unknown_wp = sorted(set(wp.keys()) - _SCENARIO_ALLOWED_WINDOW_POPULATED_KEYS)
        if unknown_wp:
            raise ScenarioValidationError(f"{ctx_label}.window_populated: unsupported key(s) {unknown_wp!r}")
    wc = checks.get("warp_completed")
    if isinstance(wc, dict):
        unknown_wc = sorted(set(wc.keys()) - _SCENARIO_ALLOWED_WARP_COMPLETED_KEYS)
        if unknown_wc:
            raise ScenarioValidationError(f"{ctx_label}.warp_completed: unsupported key(s) {unknown_wc!r}")
    if "ship_mode" in checks and checks["ship_mode"] is not None:
        sm_val = checks["ship_mode"]
        valid_sm = (
            (isinstance(sm_val, int) and not isinstance(sm_val, bool))
            or (
                isinstance(sm_val, list)
                and len(sm_val) > 0
                and all(isinstance(v, int) and not isinstance(v, bool) for v in sm_val)
            )
        )
        if not valid_sm:
            raise ScenarioValidationError(
                f"{ctx_label}.ship_mode must be an int or non-empty list of ints (Destiny MovementMode), got {sm_val!r}"
            )


def _is_gm_or_time_control_action(action: Dict[str, Any]) -> Tuple[bool, bool, str]:
    """Return `(is_gm, is_time_control, reason)` for a scenario step action dict."""
    raw_type = str(action.get("type") or "").strip()
    target = str(action.get("target") or "").strip()
    act_type = _canonicalize_action_type(raw_type, bool(target))

    if act_type in _SCENARIO_TIME_CONTROL_ACTIONS:
        return False, True, f"time-control action {act_type!r}"
    if act_type in _SCENARIO_GM_ACTIONS:
        return True, False, f"GM action {act_type!r}"
    if act_type == "station_command" and target.lower() in ("inject_and_train_skill", "buy_free_plex_pack"):
        return True, False, f"GM/setup station command {target.lower()!r}"

    text_candidates: List[str] = []
    for val in (
        _coerce_action_command(action),
        action.get("command"),
        action.get("slash_cmd"),
        action.get("text"),
        action.get("input"),
        action.get("value"),
    ):
        if isinstance(val, str):
            s = val.strip()
            if s and s not in text_candidates:
                text_candidates.append(s)

    slash_candidates: List[str] = []
    for cand_s in text_candidates:
        if act_type == "type_text":
            c_low_txt = cand_s.lower()
            first_tok = c_low_txt.split(None, 1)[0] if c_low_txt else ""
            if (
                first_tok in _ALL_REMOTE_GM_SLASH_COMMANDS
                or first_tok in _TIME_CONTROL_SLASH_PREFIXES
                or c_low_txt.startswith(_GM_SLASH_IN_TEXT_PREFIXES)
                or c_low_txt.startswith(_TIME_CONTROL_SLASH_PREFIXES)
            ):
                slash_candidates.append(cand_s)
        elif cand_s.startswith("/"):
            slash_candidates.append(cand_s)

    if act_type not in ("click_widget", "click_xy", "drag_widget", "type_text") and target.startswith("/"):
        slash_candidates.append(target)

    for cand in slash_candidates:
        c_low = cand.lower()
        first_tok = c_low.split(None, 1)[0] if c_low else ""
        if first_tok in _TIME_CONTROL_SLASH_PREFIXES or any(c_low.startswith(pfx) for pfx in _TIME_CONTROL_SLASH_PREFIXES):
            return False, True, f"slash time-control command {cand!r}"
        return True, False, f"GM slash command {cand!r}"

    return False, False, ""


def validate_scenario(spec: Any, source_name: str = "<scenario>") -> Dict[str, Any]:
    """Validate scenario structure and reject undeclared GM or time-control actions."""
    if not isinstance(spec, dict):
        raise ScenarioValidationError(f"{source_name}: scenario root must be a JSON object")

    scenario_id = str(spec.get("id") or spec.get("name") or "").strip()
    if not scenario_id:
        raise ScenarioValidationError(f"{source_name}: scenario must define a non-empty 'id' or 'name'")

    starting_state = spec.get("starting_state")
    if starting_state is None:
        starting_state = {}
    if not isinstance(starting_state, dict):
        raise ScenarioValidationError(f"{source_name}: 'starting_state' must be an object")

    steps = spec.get("steps")
    if not isinstance(steps, list) or not steps:
        raise ScenarioValidationError(f"{source_name}: 'steps' must be a non-empty list")

    declared_setup_steps: List[Dict[str, Any]] = []
    extra_chars = starting_state.get("extra_characters") or []
    if extra_chars:
        if not isinstance(extra_chars, list):
            raise ScenarioValidationError(f"{source_name}: starting_state.extra_characters must be a list")
        declared_setup_steps.append({
            "step_index": 0,
            "step_id": "starting_state.extra_characters",
            "kind": "setup",
            "action_type": "provision_characters",
            "detail": f"Provision {len(extra_chars)} extra character(s) via bridge before client start",
        })

    hook_cmd_map, _ = load_hook_open_window_tables()
    hook_cmd_methods = set(hook_cmd_map.values())

    normalized_steps: List[Dict[str, Any]] = []
    for idx, raw_step in enumerate(steps):
        step_num = idx + 1
        if not isinstance(raw_step, dict):
            raise ScenarioValidationError(f"{source_name} step {step_num}: step must be an object")
        step_id = str(raw_step.get("id") or raw_step.get("name") or f"step_{step_num}").strip()
        action = raw_step.get("action")
        if not isinstance(action, dict) or not str(action.get("type") or "").strip():
            raise ScenarioValidationError(f"{source_name} step {step_num} ({step_id}): missing action.type")

        raw_type = str(action.get("type") or "").strip()
        act_type = _canonicalize_action_type(raw_type, bool(action.get("target")))
        if act_type not in _SCENARIO_ALLOWED_ACTIONS:
            raise ScenarioValidationError(
                f"{source_name} step {step_num} ({step_id}): unsupported action type {raw_type!r}"
            )
        if act_type == "open_window":
            raw_win_tgt = str(action.get("target") or action.get("command") or action.get("slash_cmd") or "").strip()
            norm_win_tgt = _normalize_window_target_key(raw_win_tgt)
            if not norm_win_tgt or (norm_win_tgt not in hook_cmd_map and raw_win_tgt not in hook_cmd_methods):
                raise ScenarioValidationError(
                    f"{source_name} step {step_num} ({step_id}): unknown open_window target {raw_win_tgt!r} "
                    f"(not in evomu_playtester_hook.OPEN_WINDOW_CMD_MAP)"
                )
        elif act_type == "show_info":
            raw_tid = action.get("type_id")
            if raw_tid in (None, "") and str(action.get("target") or "").strip().isdigit():
                raw_tid = str(action.get("target")).strip()
            try:
                tid_int = int(raw_tid or 0)
            except Exception:
                tid_int = 0
            if tid_int <= 0:
                raise ScenarioValidationError(
                    f"{source_name} step {step_num} ({step_id}): show_info requires positive integer type_id"
                )

        is_gm, is_tc, gm_reason = _is_gm_or_time_control_action(action)
        is_setup_declared = bool(raw_step.get("setup", False))
        is_tc_declared = bool(raw_step.get("time_control", False))
        if is_gm and not is_setup_declared:
            raise ScenarioValidationError(
                f"{source_name} step {step_num} ({step_id}): {gm_reason} is forbidden outside setup steps"
            )
        if is_tc and not (is_tc_declared or is_setup_declared):
            raise ScenarioValidationError(
                f"{source_name} step {step_num} ({step_id}): {gm_reason} is forbidden outside time_control/setup steps"
            )
        if is_setup_declared or is_tc_declared or is_gm or is_tc:
            declared_setup_steps.append({
                "step_index": step_num,
                "step_id": step_id,
                "kind": "time_control" if (is_tc or is_tc_declared) else "setup",
                "action_type": act_type,
                "detail": str(action.get("command") or action.get("target") or act_type),
            })

        checks = raw_step.get("checks") if "checks" in raw_step else raw_step.get("assertions")
        if checks is not None:
            if not isinstance(checks, dict):
                raise ScenarioValidationError(f"{source_name} step {step_num} ({step_id}): 'checks' must be an object")
            _validate_scenario_checks_dict(checks, f"{source_name} step {step_num} ({step_id}).checks")
        wait_until = raw_step.get("wait_until")
        if wait_until is not None:
            if not isinstance(wait_until, dict):
                raise ScenarioValidationError(f"{source_name} step {step_num} ({step_id}): 'wait_until' must be an object")
            unknown_wu = sorted(set(wait_until.keys()) - _SCENARIO_ALLOWED_WAIT_UNTIL_KEYS)
            if unknown_wu:
                raise ScenarioValidationError(
                    f"{source_name} step {step_num} ({step_id}).wait_until: unsupported key(s) {unknown_wu!r}"
                )
            wu_checks = wait_until.get("checks")
            if wu_checks is not None:
                if not isinstance(wu_checks, dict):
                    raise ScenarioValidationError(
                        f"{source_name} step {step_num} ({step_id}).wait_until.checks must be an object"
                    )
                _validate_scenario_checks_dict(wu_checks, f"{source_name} step {step_num} ({step_id}).wait_until.checks")

        step_copy = dict(raw_step)
        step_copy["id"] = step_id
        step_copy["checks"] = dict(checks or {})
        normalized_steps.append(step_copy)

    return {
        "id": scenario_id,
        "title": str(spec.get("title") or scenario_id),
        "starting_state": dict(starting_state),
        "steps": normalized_steps,
        "declared_setup_steps": declared_setup_steps,
    }


def load_scenario_file(path: Path) -> Dict[str, Any]:
    """Read and validate a scenario JSON file."""
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except Exception as exc:
        raise ScenarioValidationError(f"Failed to parse scenario {path}: {exc}") from exc
    validated = validate_scenario(data, source_name=str(path))
    validated["_source_path"] = str(path)
    return validated


def _format_rpc_name(call: Dict[str, Any]) -> str:
    if not isinstance(call, dict):
        return str(call or "").strip()
    svc = str(call.get("service") or "").strip()
    meth = str(call.get("method") or "").strip()
    if svc and meth:
        return f"{svc}.{meth}"
    return svc or meth


def _collect_step_rpcs(exec_res: Optional[Dict[str, Any]]) -> List[str]:
    fr = (exec_res or {}).get("flight_recorder") or {}
    names = {_format_rpc_name(c) for c in (fr.get("macho_calls") or [])}
    return sorted(n for n in names if n)


def _find_ball_in_state(
    state: Optional[Dict[str, Any]],
    target_id: Any = None,
) -> Optional[Dict[str, Any]]:
    space = (state or {}).get("space") or {}
    balls = list(space.get("nearby_balls") or [])
    if target_id in (None, ""):
        sel = space.get("selected_item") or {}
        if isinstance(sel, dict) and sel.get("item_id") is not None:
            target_id = sel.get("item_id")
    if target_id not in (None, ""):
        t_str = str(target_id).strip().lower()
        for b in balls:
            if not isinstance(b, dict):
                continue
            bid = b.get("item_id")
            bname = str(b.get("name") or "").lower()
            if (bid is not None and str(bid) == t_str) or (t_str and t_str in bname):
                return b
        return None
    for b in balls:
        if isinstance(b, dict) and (b.get("distance_m") is not None or b.get("surface_dist_m") is not None):
            return b
    return None


def _ball_distance(ball: Optional[Dict[str, Any]]) -> Optional[float]:
    if not isinstance(ball, dict):
        return None
    val = ball.get("surface_dist_m")
    if val is None:
        val = ball.get("distance_m")
    try:
        return float(val) if val is not None else None
    except Exception:
        return None


_STATE_FIELD_ALIASES = {
    "wallet_isk": "wallet_state.isk_balance",
    "hangar_item_count": "inventory_state.hangar_item_count",
    "cargo_item_count": "inventory_state.cargo_item_count",
    "ship_hangar_count": "inventory_state.ship_hangar_count",
    "skill_queue_len": "skills_state.queue_len",
    "trained_skill_count": "skills_state.total_trained_skills",
    "order_count": "market_state.char_orders_count",
    "bookmark_count": "bookmarks_state.count",
    "bookmark_names": "bookmarks_state.names",
    "charsel_active": "charsel_state.active",
    "charsel_names": "charsel_state.names",
    "ship_mode": "space.ego.mode",
    "ship_speed": "space.ego.speed_mps",
    "ship_warping": "space.ego.signal_in_warp",
    "celestials_in_overview": "space.celestials_in_overview",
    "overview_count": "space.overview_count",
    "autopilot_active": "autopilot_state.active",
    "autopilot_route": "autopilot_state.route",
    "autopilot_waypoints": "autopilot_state.waypoints",
    "active_view": "active_view",
}


def _extract_state_value(
    state: Optional[Dict[str, Any]],
    field_path: str,
    target_id: Any = None,
    exec_res: Optional[Dict[str, Any]] = None,
) -> Any:
    st = state if isinstance(state, dict) else {}
    f_raw = str(field_path or "").strip()
    if f_raw == "target_distance":
        return _ball_distance(_find_ball_in_state(st, target_id=target_id))
    if f_raw == "ship_shown":
        sess = st.get("session") or {}
        return bool(sess.get("shipid")) and st.get("active_view") in ("hangar", "inflight")
    if f_raw in ("warp_started", "warp_finished", "warp_signal_fired", "warp_finished_count"):
        wt = (exec_res or {}).get("warp_telemetry") or st.get("warp_telemetry") or {}
        if f_raw == "warp_signal_fired":
            return bool(wt.get("saw_signal") or int(wt.get("warp_signal_count", 0) or 0) > 0)
        if f_raw == "warp_started":
            return bool(
                wt.get("saw_signal")
                or wt.get("saw_mode3")
                or wt.get("saw_fx_warping")
                or int(wt.get("started_count", 0) or 0) > 0
            )
        if f_raw == "warp_finished_count":
            return int(wt.get("finished_count", 0) or 0)
        return bool(int(wt.get("finished_count", 0) or 0) > 0 and not wt.get("in_warp", False))

    resolved = _STATE_FIELD_ALIASES.get(f_raw, f_raw)
    cur: Any = st
    for part in resolved.split("."):
        if isinstance(cur, dict):
            cur = cur.get(part)
        else:
            return None
    return cur


def _evaluate_value_rule(label: str, pre_val: Any, post_val: Any, rule: Dict[str, Any]) -> Optional[str]:
    if "equals" in rule:
        exp = rule["equals"]
        if isinstance(exp, (int, float)) and not isinstance(exp, bool):
            try:
                if post_val is None or abs(float(post_val) - float(exp)) > 1e-6:
                    return f"{label} expected {exp!r}, got {post_val!r}"
            except Exception:
                if post_val != exp:
                    return f"{label} expected {exp!r}, got {post_val!r}"
        elif isinstance(exp, bool):
            if bool(post_val) is not exp:
                return f"{label} expected {exp!r}, got {post_val!r}"
        else:
            if str(post_val) != str(exp) and post_val != exp:
                return f"{label} expected {exp!r}, got {post_val!r}"

    if "not_equals" in rule:
        unexp = rule["not_equals"]
        if post_val == unexp or (unexp is not None and post_val is not None and str(post_val) == str(unexp)):
            return f"{label} unexpectedly equaled {unexp!r}"

    if "non_empty" in rule and rule["non_empty"]:
        if not post_val:
            return f"{label} expected non-empty value, got {post_val!r}"

    if "contains" in rule:
        needle = rule["contains"]
        needle_l = str(needle).strip().lower()
        if isinstance(post_val, (list, tuple, set)):
            if not any(needle_l == str(item).strip().lower() or needle_l in str(item).lower() for item in post_val):
                return f"{label} ({post_val!r}) does not contain {needle!r}"
        elif isinstance(post_val, dict):
            if needle not in post_val and needle_l not in [str(k).lower() for k in post_val.keys()]:
                return f"{label} ({post_val!r}) does not contain key {needle!r}"
        elif post_val is None or needle_l not in str(post_val).lower():
            return f"{label} ({post_val!r}) does not contain {needle!r}"

    if "subset" in rule:
        allowed_spec = rule["subset"]
        if not isinstance(allowed_spec, (list, tuple, set)):
            return f"{label} subset rule expected list/set, got {allowed_spec!r}"
        if post_val is None or isinstance(post_val, (str, bytes, int, float, bool)):
            return f"{label} expected iterable collection for subset check, got {post_val!r}"
        actual_items = list(post_val.keys()) if isinstance(post_val, dict) else list(post_val)
        def _norm_elem(v: Any) -> Any:
            if isinstance(v, bool):
                return ("bool", v)
            if isinstance(v, (int, float)):
                return ("num", float(v))
            return ("str", str(v).strip().lower())
        allowed_norm = {_norm_elem(x) for x in allowed_spec}
        extra = [x for x in actual_items if _norm_elem(x) not in allowed_norm]
        if extra:
            return f"{label} ({post_val!r}) is not a subset of {list(allowed_spec)!r} (unexpected={extra!r})"

    if "prefix" in rule:
        pfx = rule["prefix"]
        if isinstance(pfx, (list, tuple)):
            if not isinstance(post_val, (list, tuple)) or list(post_val[:len(pfx)]) != list(pfx):
                return f"{label} ({post_val!r}) does not start with prefix {list(pfx)!r}"
        else:
            pfx_s = str(pfx).lower()
            if post_val is None or not str(post_val).lower().startswith(pfx_s):
                return f"{label} ({post_val!r}) does not start with prefix {pfx!r}"

    if "min" in rule:
        try:
            if post_val is None or float(post_val) < float(rule["min"]):
                return f"{label} expected >= {rule['min']}, got {post_val!r}"
        except Exception:
            return f"{label} non-numeric for min check: {post_val!r}"

    if "max" in rule:
        try:
            if post_val is None or float(post_val) > float(rule["max"]):
                return f"{label} expected <= {rule['max']}, got {post_val!r}"
        except Exception:
            return f"{label} non-numeric for max check: {post_val!r}"

    if "increased" in rule:
        inc_spec = rule["increased"]
        try:
            pre_f = float(pre_val)
            post_f = float(post_val)
        except Exception:
            return f"{label} missing numeric pre/post values for increased check (pre={pre_val!r}, post={post_val!r})"
        if isinstance(inc_spec, bool):
            if inc_spec and not (post_f > pre_f):
                return f"{label} did not increase (pre={pre_val!r}, post={post_val!r})"
        else:
            if (post_f - pre_f) + 1e-6 < float(inc_spec):
                return f"{label} increased by {post_f - pre_f:.3f}, expected >= {inc_spec}"

    if "decreased" in rule:
        dec_spec = rule["decreased"]
        try:
            pre_f = float(pre_val)
            post_f = float(post_val)
        except Exception:
            return f"{label} missing numeric pre/post values for decreased check (pre={pre_val!r}, post={post_val!r})"
        if isinstance(dec_spec, bool):
            if dec_spec and not (post_f < pre_f):
                return f"{label} did not decrease (pre={pre_val!r}, post={post_val!r})"
        else:
            if (pre_f - post_f) + 1e-6 < float(dec_spec):
                return f"{label} decreased by {pre_f - post_f:.3f}, expected >= {dec_spec}"

    if rule.get("changed"):
        if pre_val == post_val:
            return f"{label} did not change (remained {post_val!r})"

    if "delta" in rule:
        exp_delta = float(rule["delta"])
        try:
            actual_delta = float(post_val) - float(pre_val)
        except Exception:
            return f"{label} missing numeric pre/post values for delta check (pre={pre_val!r}, post={post_val!r})"
        tol = float(rule.get("tolerance", 1e-6))
        if abs(actual_delta - exp_delta) > tol:
            return f"{label} delta was {actual_delta}, expected {exp_delta} (pre={pre_val!r}, post={post_val!r})"

    return None


def _check_window_populated(
    spec: Any,
    open_windows: List[Dict[str, Any]],
    step_rpcs: List[str],
) -> Optional[str]:
    cfg = {"window": spec} if isinstance(spec, str) else dict(spec or {})
    target_w = str(cfg.get("window") or cfg.get("name") or "").strip().lower()
    matches = []
    for w in open_windows:
        if not isinstance(w, dict):
            continue
        if not target_w:
            matches.append(w)
            continue
        hay = " ".join([
            str(w.get("window_id") or ""),
            str(w.get("name") or ""),
            str(w.get("cls") or ""),
            str(w.get("caption") or ""),
        ]).lower()
        if _window_hay_matches_target(target_w, hay):
            matches.append(w)
    if not matches:
        open_names = [str(w.get("window_id") or w.get("name") or w.get("cls") or "") for w in open_windows if isinstance(w, dict)]
        return f"window {target_w or '<any>'!r} is not open (open_windows={open_names})"
    win = matches[0]
    if not cfg.get("allow_loading", False) and win.get("has_loading"):
        return f"window {target_w or win.get('window_id')!r} still shows a loading state"
    if not cfg.get("allow_error", False) and win.get("has_error"):
        return f"window {target_w or win.get('window_id')!r} shows an error state (texts={win.get('sample_texts')})"
    pop_cnt = int(win.get("populated_count", 0) or 0)
    min_pop = int(cfg.get("min_populated", 1))
    if pop_cnt < min_pop:
        return f"window {target_w or win.get('window_id')!r} populated_count={pop_cnt} < {min_pop}"
    if "min_tabs" in cfg and int(win.get("tab_count", 0) or 0) < int(cfg["min_tabs"]):
        return f"window {target_w!r} tab_count={win.get('tab_count')} < {cfg['min_tabs']}"
    if "min_rows" in cfg and int(win.get("row_count", 0) or 0) < int(cfg["min_rows"]):
        return f"window {target_w!r} row_count={win.get('row_count')} < {cfg['min_rows']}"
    if cfg.get("contains_text"):
        want_t = str(cfg["contains_text"]).strip().lower()
        joined = " ".join(str(x) for x in (win.get("sample_texts") or [])).lower()
        if want_t not in joined:
            return f"window {target_w!r} sample_texts={win.get('sample_texts')} missing {want_t!r}"
    if cfg.get("require_rpc_or_data", True) and (len(step_rpcs) == 0 and pop_cnt == 0):
        return f"window {target_w!r} sent 0 RPCs and had 0 populated entries"
    return None


def evaluate_scenario_checks(
    checks: Optional[Dict[str, Any]],
    pre_state: Optional[Dict[str, Any]],
    post_state: Optional[Dict[str, Any]],
    exec_res: Optional[Dict[str, Any]],
    context: Optional[Dict[str, Any]] = None,
) -> Tuple[bool, str, str]:
    """Evaluate all scripted scenario assertions against hook flight recorder and state snapshots.

    Returns `(passed, failed_check_name, failure_reason)`.
    """
    chk = dict(checks or {})
    ctx = dict(context or {})
    res = exec_res if isinstance(exec_res, dict) else {}
    fr = dict(res.get("flight_recorder") or {})
    pre = pre_state if isinstance(pre_state, dict) else {}
    post = post_state if isinstance(post_state, dict) else {}
    expected_text = str(ctx.get("expected_text") or "")
    scen_ui_state = _select_live_skill_state(
        post, _select_live_skill_state(pre, ctx.get("ui_state"))
    )

    # 1. Existing scene-graph / window / modal / ball / session assertions
    base_ok, base_msg = _evaluate_step_assertions(chk, res, post)
    if not base_ok:
        failed_key = "step_assertion"
        for k in (
            "expect_window_open",
            "expect_window_closed",
            "expect_modal_absent",
            "expect_context_menu_item",
            "expect_ball_present",
            "expect_session_field",
        ):
            if k in base_msg:
                failed_key = k
                break
        return False, failed_key, base_msg

    # 1b. Wait-until timeout always fails closed even if checks.no_error_reply is False
    if res.get("wait_until_timed_out"):
        return False, "wait_until_timeout", str(res.get("action_error") or "wait_until timed out")

    # 2. No error reply (action_ok, quasar_errors, non-benign macho_exceptions/ERR calls)
    if chk.get("no_error_reply", True):
        if res.get("action_ok") is False:
            return False, "no_error_reply", f"action_ok is False: {res.get('action_error') or 'unknown error'}"
        q_errs = list(fr.get("quasar_errors") or [])
        if q_errs:
            return False, "no_error_reply", f"quasar_errors observed: {q_errs[:3]}"
        m_excs = [
            e for e in (fr.get("macho_exceptions") or [])
            if not PlaytesterBugOracle.is_expected_user_error(
                e, expected_text, ui_state=scen_ui_state
            )
        ]
        if m_excs:
            return False, "no_error_reply", f"macho_exceptions observed: {m_excs[:3]}"
        err_calls = [
            c for c in (fr.get("macho_calls") or [])
            if isinstance(c, dict)
            and str(c.get("status") or "").upper() == "ERR"
            and not PlaytesterBugOracle.is_expected_user_error(
                c.get("error") or c, expected_text, ui_state=scen_ui_state
            )
        ]
        if err_calls:
            return False, "no_error_reply", f"failed RPC call(s) recorded: {err_calls[:3]}"

    # 3. No client Python exception
    if chk.get("no_client_exception", True):
        bad_excs = []
        for le in (fr.get("log_exceptions") or []):
            hl, _, full_tb = PlaytesterBugOracle._extract_log_exception_parts(le)
            if not PlaytesterBugOracle.is_ignorable_client_traceback(
                hl, full_tb, expected_text, ui_state=scen_ui_state
            ):
                bad_excs.append(hl or full_tb[:120] or repr(le))
        for u in (ctx.get("unseen_tracebacks") or []):
            hl = str(u.get("headline") or "")
            tr = str(u.get("trace") or "")
            if not PlaytesterBugOracle.is_ignorable_client_traceback(
                hl, tr, expected_text, ui_state=scen_ui_state
            ):
                bad_excs.append(hl or tr[:120])
        if bad_excs:
            return False, "no_client_exception", f"client exception(s): {bad_excs[:2]}"

    step_calls = list(fr.get("macho_calls") or [])
    startup_calls = list(ctx.get("startup_macho_calls") or [])
    eval_calls = (step_calls + startup_calls) if chk.get("include_startup_rpcs", True) else step_calls
    step_rpc_names = [_format_rpc_name(c) for c in step_calls]

    # 4. Named RPC sent check
    rpc_spec = chk.get("rpc_sent")
    if rpc_spec is not None:
        requirements: List[Tuple[str, int, bool]] = []
        if isinstance(rpc_spec, str):
            requirements.append((rpc_spec, 1, False))
        elif isinstance(rpc_spec, dict):
            for k, v in rpc_spec.items():
                requirements.append((str(k), int(v), False))
        elif isinstance(rpc_spec, list):
            for item in rpc_spec:
                if isinstance(item, dict):
                    requirements.append((
                        str(item.get("name") or item.get("rpc") or ""),
                        int(item.get("min_count", 1)),
                        bool(item.get("require_ok", False)),
                    ))
                else:
                    requirements.append((str(item), 1, False))
        for req_name, min_cnt, req_ok in requirements:
            matches = [
                c for c in eval_calls
                if _format_rpc_name(c).lower() == req_name.lower()
                and (not req_ok or str(c.get("status") or "OK").upper() == "OK")
            ]
            if len(matches) < min_cnt:
                observed = sorted({_format_rpc_name(c) for c in eval_calls})
                return (
                    False,
                    "rpc_sent",
                    f"expected RPC {req_name!r} >= {min_cnt} time(s), saw {len(matches)} (observed={observed[:15]})",
                )

    if "min_rpcs" in chk:
        req_min = int(chk["min_rpcs"])
        if len(step_calls) < req_min:
            return False, "min_rpcs", f"expected >= {req_min} RPC call(s), recorded {len(step_calls)} ({step_rpc_names})"

    # 5. Notification scatter received check
    notif_spec = chk.get("notification_received")
    if notif_spec is not None:
        req_notifs = [notif_spec] if isinstance(notif_spec, str) else [str(x) for x in notif_spec]
        observed_scatters = [str(s or "") for s in (fr.get("scatters_observed") or [])]
        for want_n in req_notifs:
            wl = want_n.strip().lower()
            if not any(wl == obs.lower() or wl in obs.lower() for obs in observed_scatters):
                return (
                    False,
                    "notification_received",
                    f"expected notification {want_n!r} not in scatters {observed_scatters}",
                )

    # 6. Charsel checks
    charsel_spec = chk.get("charsel_listed_all_characters")
    if charsel_spec:
        cs = post.get("charsel_state") or {}
        if not cs.get("active"):
            return False, "charsel_listed_all_characters", f"character selection screen not active (charsel_state={cs!r})"
        listed_names = [str(n or "").strip() for n in (cs.get("names") or []) if str(n or "").strip()]
        listed_low = {n.lower() for n in listed_names}
        if isinstance(charsel_spec, list):
            expected_names = [str(n).strip() for n in charsel_spec]
        else:
            expected_names = [str(n).strip() for n in (ctx.get("expected_characters") or [])]
        if expected_names:
            missing = [n for n in expected_names if n.lower() not in listed_low]
            if missing:
                return (
                    False,
                    "charsel_listed_all_characters",
                    f"charsel missing expected character(s) {missing!r} (listed={listed_names!r})",
                )
            if not isinstance(charsel_spec, list):
                primary_name = str(ctx.get("primary_character_name") or "").strip()
                if primary_name and primary_name.lower() not in listed_low:
                    return (
                        False,
                        "charsel_listed_all_characters",
                        f"charsel missing primary character {primary_name!r} (listed={listed_names!r})",
                    )
                if len(listed_names) < len(expected_names) + 1:
                    return (
                        False,
                        "charsel_listed_all_characters",
                        f"expected primary + {len(expected_names)} extra character(s), saw {listed_names!r}",
                    )
        elif len(listed_names) < 2:
            return (
                False,
                "charsel_listed_all_characters",
                f"expected >= 2 characters on charsel screen, saw {listed_names!r}",
            )

    # 7. Populated window check
    if "window_populated" in chk and chk["window_populated"] is not None:
        win_err = _check_window_populated(
            chk["window_populated"],
            list(post.get("open_windows") or (post.get("ui") or {}).get("open_windows") or []),
            step_rpc_names,
        )
        if win_err:
            return False, "window_populated", win_err

    # 8. Ship mode / space navigation / warp / bookmark shortcuts
    if "ship_shown" in chk:
        shown = bool(_extract_state_value(post, "ship_shown"))
        if shown is not bool(chk["ship_shown"]):
            return (
                False,
                "ship_shown",
                f"expected ship_shown={chk['ship_shown']!r}, active_view={post.get('active_view')!r} shipid={(post.get('session') or {}).get('shipid')!r}",
            )

    if "ship_mode" in chk:
        actual_mode = _extract_state_value(post, "ship_mode")
        exp_modes = chk["ship_mode"] if isinstance(chk["ship_mode"], list) else [chk["ship_mode"]]
        if actual_mode not in exp_modes:
            return False, "ship_mode", f"expected ego ship_mode in {exp_modes!r}, got {actual_mode!r}"

    if "celestials_in_overview" in chk:
        actual_ov = bool(_extract_state_value(post, "celestials_in_overview"))
        if actual_ov is not bool(chk["celestials_in_overview"]):
            return False, "celestials_in_overview", f"expected celestials_in_overview={chk['celestials_in_overview']!r}, got {actual_ov!r}"

    if "target_distance" in chk and isinstance(chk["target_distance"], dict):
        td_rule = dict(chk["target_distance"])
        tid = td_rule.get("target_id")
        pre_d = _extract_state_value(pre, "target_distance", target_id=tid)
        post_d = _extract_state_value(post, "target_distance", target_id=tid)
        err_msg = _evaluate_value_rule(f"target_distance({tid or 'selected'})", pre_d, post_d, td_rule)
        if err_msg:
            return False, "target_distance", err_msg

    if chk.get("warp_started"):
        pre_wt = pre.get("warp_telemetry") or {}
        wt = res.get("warp_telemetry") or post.get("warp_telemetry") or {}
        started = bool(
            wt.get("saw_signal")
            or wt.get("saw_mode3")
            or wt.get("saw_fx_warping")
            or int(wt.get("started_count", 0) or 0) > int(pre_wt.get("started_count", 0) or 0)
        )
        if not started:
            return False, "warp_started", f"warp start signal/mode did not trigger (warp_telemetry={wt!r})"

    if "warp_completed" in chk and chk["warp_completed"]:
        wcfg = chk["warp_completed"] if isinstance(chk["warp_completed"], dict) else {}
        pre_wt = pre.get("warp_telemetry") or {}
        wt = post.get("warp_telemetry") or res.get("warp_telemetry") or {}
        pre_s = int(pre_wt.get("started_count", 0) or 0)
        post_s = int(wt.get("started_count", 0) or 0)
        pre_f = int(pre_wt.get("finished_count", 0) or 0)
        post_f = int(wt.get("finished_count", 0) or 0)
        saw_start = bool(post_s > pre_s or wt.get("saw_signal") or wt.get("saw_mode3") or wt.get("saw_fx_warping"))
        saw_end = bool(post_f > pre_f and not wt.get("in_warp", False))
        if not (saw_start and saw_end):
            return (
                False,
                "warp_completed",
                f"warp did not start+finish (pre_started={pre_s}, post_started={post_s}, pre_finished={pre_f}, post_finished={post_f}, wt={wt!r})",
            )
        if "landing_distance_max_m" in wcfg:
            tid = wcfg.get("target_id")
            dist_val = _extract_state_value(post, "target_distance", target_id=tid)
            max_d = float(wcfg["landing_distance_max_m"])
            if dist_val is None or float(dist_val) > max_d:
                return (
                    False,
                    "warp_completed.landing_distance_max_m",
                    f"landing distance to {tid!r} was {dist_val!r} m, expected <= {max_d} m",
                )

    if chk.get("bookmark_present"):
        want_bm = str(chk["bookmark_present"]).strip()
        bm_names = [str(n or "") for n in ((post.get("bookmarks_state") or {}).get("names") or [])]
        if not any(want_bm.lower() in n.lower() for n in bm_names):
            return False, "bookmark_present", f"bookmark {want_bm!r} not in client bookmarks {bm_names!r}"

    if chk.get("bookmark_server_check"):
        want_bm = str(chk["bookmark_server_check"]).strip()
        fn = ctx.get("server_bookmark_exists")
        if callable(fn) and not fn(want_bm):
            return False, "bookmark_server_check", f"bookmark {want_bm!r} not found in server database"

    if chk.get("handshake_server_check"):
        acct = str(chk["handshake_server_check"]).strip()
        fn = ctx.get("server_handshake_seen")
        if callable(fn) and not fn(acct):
            return False, "handshake_server_check", f"server log did not record login handshake for account {acct!r}"

    if chk.get("no_server_unimplemented_rpc"):
        fn = ctx.get("check_server_unimplemented_rpc")
        if callable(fn):
            bad_lines = fn()
            if bad_lines:
                return False, "no_server_unimplemented_rpc", f"server log reported UnimplementedRPC: {bad_lines[:2]}"

    # 9. Generic state_checks (list of dicts or dict of field -> rule)
    sc_spec = chk.get("state_checks")
    if sc_spec:
        if isinstance(sc_spec, dict):
            sc_items = [{"field": k, **(v if isinstance(v, dict) else {"equals": v})} for k, v in sc_spec.items()]
        else:
            sc_items = list(sc_spec)
        for item in sc_items:
            if not isinstance(item, dict):
                continue
            fpath = str(item.get("field") or "").strip()
            tid = item.get("target_id")
            pre_v = _extract_state_value(pre, fpath, target_id=tid, exec_res=res)
            post_v = _extract_state_value(post, fpath, target_id=tid, exec_res=res)
            err_msg = _evaluate_value_rule(fpath, pre_v, post_v, item)
            if err_msg:
                return False, f"state_checks.{fpath}", err_msg

    return True, "", ""


def _read_log_chunk_from_offset(path: Optional[Path], log_offsets: Optional[Dict[str, int]]) -> Tuple[str, int]:
    """Read bytes added to `path` since its tracked byte offset and update `log_offsets`."""
    if not path or not path.exists():
        return "", 0
    key = str(path)
    offset = int(log_offsets.get(key, 0) or 0) if log_offsets is not None else 0
    size = path.stat().st_size
    if size < offset:
        offset = 0
    if size == offset:
        if log_offsets is not None:
            log_offsets[key] = size
        return "", offset
    with path.open("rb") as f:
        f.seek(offset)
        chunk = f.read(size - offset)
    if log_offsets is not None:
        log_offsets[key] = size
    return chunk.decode("utf-8", errors="replace"), offset


def _is_explicit_client_log_header(ln: str) -> bool:
    s_hdr = ln[len("[evomu_site] "):] if ln.startswith("[evomu_site] ") else ln
    if ln.startswith("[evomu_site] ") and any(
        tok in s_hdr for tok in ("LogException", "_LogException", "LogTraceback", "sys.excepthook", "Action tasklet exception", "EXCEPTION #", "Traceback (most recent call last):")
    ):
        return True
    return s_hdr.startswith((
        "log.Log",
        "logmodule.",
        "service.CoreMixIn.",
        "blue.pyos.exceptionHandler:",
        "stdlogutils.",
        "sys.excepthook:",
        "Action tasklet exception",
        "EXCEPTION #",
    ))


def _collect_client_log_block(lines: List[str], start_idx: int) -> Tuple[List[str], int]:
    """Collect a multi-line client traceback / LogException block with INNER TRACEBACK & CALLSTACK."""
    first = lines[start_idx]
    block = [first]
    in_exception_banner = (
        first[len("[evomu_site] "):] if first.startswith("[evomu_site] ") else first
    ).startswith("EXCEPTION #")
    saw_tb = "Traceback (most recent call last):" in first
    saw_section_or_frame = saw_tb or _is_explicit_client_log_header(first)
    j = start_idx + 1
    while j < len(lines) and len(block) < 120:
        nxt = lines[j]
        nxt_s = nxt.strip()
        if _is_explicit_client_log_header(nxt):
            break
        if nxt_s == "Traceback (most recent call last):" and saw_tb and not in_exception_banner:
            break
        if nxt_s == "EXCEPTION END":
            block.append(nxt)
            j += 1
            break
        if in_exception_banner:
            block.append(nxt)
            j += 1
            continue
        if nxt_s == "Traceback (most recent call last):":
            saw_tb = True
            saw_section_or_frame = True
            block.append(nxt)
            j += 1
            continue
        if nxt_s in (
            "INNER TRACEBACK:",
            "CALLSTACK:",
            "Local variables:",
            "Thread Locals:",
            "None",
        ) or nxt_s.startswith(("Reported from", "Common path prefix =")):
            saw_section_or_frame = True
            block.append(nxt)
            j += 1
            continue
        if nxt.startswith((" ", "\t")):
            saw_section_or_frame = True
            block.append(nxt)
            j += 1
            continue
        if not nxt_s:
            if j + 1 < len(lines) and (
                lines[j + 1].strip()
                in ("INNER TRACEBACK:", "Traceback (most recent call last):", "CALLSTACK:", "EXCEPTION END", "None")
                or lines[j + 1].startswith((" ", "\t"))
            ):
                block.append(nxt)
                j += 1
                continue
            break
        if saw_section_or_frame and (
            PlaytesterBugOracle._TB_EXC_LINE_RE.match(nxt_s)
            or "Error:" in nxt_s
            or "Exception:" in nxt_s
            or nxt_s.startswith(("UserError", "RuntimeError", "AttributeError", "TypeError", "ValueError", "KeyError"))
        ):
            block.append(nxt)
            j += 1
            continue
        break
    return block, j


def scan_scenario_logs_for_errors(
    server_log_path: Optional[Path],
    client_log_paths: List[Path],
    traceback_log_path: Optional[Path] = None,
    log_offsets: Optional[Dict[str, int]] = None,
    *,
    ui_state: Optional[Dict[str, Any]] = None,
    exemptions_out: Optional[List[Dict[str, Any]]] = None,
    step_num: int = 0,
    channel_prefix: str = "scenario_",
    dedup_namespace: str = "",
) -> List[str]:
    """Scan server and client logs cleanly from optional byte offsets, honoring known benign V24 tracebacks."""
    errors: List[str] = []
    tb_errors: List[str] = []
    emitted_tb_keys: set = set()
    eff_ns = str(dedup_namespace or ("tail" if channel_prefix != "scenario_" else "")).strip()
    srv_txt, _ = _read_log_chunk_from_offset(server_log_path, log_offsets)
    if srv_txt:
        for ln in srv_txt.splitlines():
            if " benign " in ln.lower():
                continue
            if _SCENARIO_DESYNC_LINE_RE.search(ln):
                errors.append(f"[server.log] destiny desync: {ln.strip()[:240]}")
            elif _SCENARIO_ERROR_LINE_RE.search(ln):
                errors.append(f"[server.log] {ln.strip()[:280]}")

    raw_tb, _ = _read_log_chunk_from_offset(traceback_log_path, log_offsets)
    if raw_tb:
        tb_mirror_occ: Dict[str, int] = {}
        tb_channel = f"{channel_prefix}client_tracebacks"
        for u in PlaytesterBugOracle.parse_tb_blocks(raw_tb):
            ign_tb, det_tb = PlaytesterBugOracle.classify_ignorable_client_traceback(
                u.get("headline", ""), u.get("trace", ""), ui_state=ui_state
            )
            if not ign_tb:
                tb_errors.append(f"[client_tracebacks.log] {u.get('headline', '')[:240]}")
            elif exemptions_out is not None and det_tb is not None:
                msig = PlaytesterBugOracle._canonical_client_tb_sig(
                    u.get("trace") or u.get("headline") or "", det_tb
                )
                m_occ = tb_mirror_occ.get(msig, 0)
                tb_mirror_occ[msig] = m_occ + 1
                emitted_tb_keys.add((msig, m_occ))
                exemptions_out.append(
                    PlaytesterBugOracle._build_exemption_hit(
                        det_tb,
                        channel=tb_channel,
                        step_num=step_num,
                        occ_idx=len(exemptions_out),
                        mirror_sig=msig,
                        mirror_occ=m_occ,
                        dedup_namespace=eff_ns,
                    )
                )

    tb_resolved = traceback_log_path.resolve() if traceback_log_path else None
    for clp in client_log_paths:
        if not clp or not clp.exists():
            continue
        if (tb_resolved is not None and clp.resolve() == tb_resolved) or clp.name == "client_tracebacks.log":
            continue
        c_txt, start_off = _read_log_chunk_from_offset(clp, log_offsets)
        if not c_txt:
            continue
        log_channel = f"{channel_prefix}log:{clp.name}"
        file_mirror_occ: Dict[str, int] = {}

        def _record_client_log_exemption(sig_txt: str, det: Dict[str, Any]) -> None:
            msig = PlaytesterBugOracle._canonical_client_tb_sig(sig_txt, det)
            m_occ = file_mirror_occ.get(msig, 0)
            file_mirror_occ[msig] = m_occ + 1
            if (msig, m_occ) in emitted_tb_keys:
                return
            emitted_tb_keys.add((msig, m_occ))
            if exemptions_out is not None:
                exemptions_out.append(
                    PlaytesterBugOracle._build_exemption_hit(
                        det,
                        channel=log_channel,
                        step_num=step_num,
                        occ_idx=len(exemptions_out),
                        mirror_sig=msig,
                        mirror_occ=m_occ,
                        dedup_namespace=eff_ns,
                    )
                )

        lines = c_txt.splitlines()
        seen_started = start_off > 0
        idx = 0
        while idx < len(lines):
            ln = lines[idx]
            if "EVE Client version" in ln and "started" in ln:
                seen_started = True
            starts_block = (
                "Traceback (most recent call last):" in ln
                or _is_explicit_client_log_header(ln)
                or any(tok in ln for tok in ("_LogException", "LogException", "LogTraceback"))
            )
            if starts_block:
                block, next_idx = _collect_client_log_block(lines, idx)
                blk_txt = "\n".join(block)
                if _SCENARIO_DESYNC_LINE_RE.search(blk_txt):
                    desync_ln = next(
                        (b.strip() for b in block if _SCENARIO_DESYNC_LINE_RE.search(b)),
                        ln.strip(),
                    )
                    errors.append(f"[{clp.name}] destiny desync: {desync_ln[:240]}")
                else:
                    hl = PlaytesterBugOracle._extract_tb_headline(blk_txt, default=ln.strip())
                    ign_blk, det_blk = PlaytesterBugOracle.classify_ignorable_client_traceback(
                        hl, blk_txt, ui_state=ui_state
                    )
                    if not ign_blk:
                        errors.append(f"[{clp.name}] {hl[:240]}")
                    elif exemptions_out is not None and det_blk is not None:
                        _record_client_log_exemption(blk_txt, det_blk)
                idx = next_idx
                continue
            if _SCENARIO_DESYNC_LINE_RE.search(ln):
                errors.append(f"[{clp.name}] destiny desync: {ln.strip()[:240]}")
            elif _SCENARIO_ERROR_LINE_RE.search(ln) or (seen_started and "#nolog: An exception has occurred" in ln):
                ign_ln, det_ln = PlaytesterBugOracle.classify_ignorable_client_traceback(
                    ln, ln, ui_state=ui_state
                )
                if not ign_ln:
                    errors.append(f"[{clp.name}] {ln.strip()[:280]}")
                elif exemptions_out is not None and det_ln is not None:
                    _record_client_log_exemption(ln, det_ln)
            idx += 1

    errors.extend(tb_errors)
    return errors


_SCENARIO_DEFAULT_SCHOOL_BY_BLOODLINE: Dict[int, int] = {
    1: 17, 2: 17, 11: 17,   # Caldari -> State War Academy
    3: 14, 4: 14, 14: 14,   # Minmatar -> Republic Military School
    5: 11, 6: 11, 13: 11,   # Amarr -> Imperial Academy
    7: 20, 8: 20, 12: 20,   # Gallente -> Federal Navy Academy
    9: 23, 10: 23, 19: 23,  # Jove -> Society of Conscious Thought
}
_SCENARIO_DEFAULT_ANCESTRY_BY_BLOODLINE: Dict[int, int] = {
    1: 11, 2: 7, 3: 19, 4: 22, 5: 1, 6: 4, 7: 13, 8: 16,
    9: 25, 10: 28, 11: 31, 12: 34, 13: 37, 14: 40, 19: 64,
}


def _coerce_int_client_build(val: Any) -> int:
    if isinstance(val, bool):
        return 0
    if isinstance(val, int):
        return val if val > 0 else 0
    s = str(val or "").strip()
    if s.isdigit():
        return int(s)
    m = re.search(r"\d+", s)
    return int(m.group(0)) if m else 0


def _is_mutating_scenario_step(
    step: Dict[str, Any],
    act_type: str,
    exec_res: Optional[Dict[str, Any]],
) -> bool:
    chk = step.get("checks") or {}
    if chk.get("ui_delta_without_relog") is True:
        return True
    if chk.get("ui_delta_without_relog") is False:
        return False
    readonly_actions = {
        "wait",
        "noop",
        "open_window",
        "close_modal_or_window",
        "cleanup_utility_windows",
        "click_top_tabs",
        "show_info",
        "select_character",
    }
    if act_type not in readonly_actions:
        return True
    fr = (exec_res or {}).get("flight_recorder") or {}
    mutating_prefixes = (
        "buy", "sell", "move", "add", "sethome", "install", "slash", "reprocess",
        "create", "deliver", "transfer", "give", "train", "inject", "board",
        "dock", "undock", "jump", "cancel", "accept", "complete", "fit",
        "online", "offline", "activate", "deactivate", "trash", "split", "merge",
    )
    for call in fr.get("macho_calls") or []:
        if not isinstance(call, dict):
            continue
        meth = str(call.get("method") or "").strip().lower()
        if any(meth.startswith(p) or p in meth for p in mutating_prefixes):
            return True
    return False


def _lookup_sde_type_categories(
    sde_path: Optional[Path],
    type_ids: List[int],
    world_db_path: Optional[Path] = None,
) -> Dict[int, int]:
    """Return `{type_id: category_id}` from `sde_types` when an SDE SQLite db is available."""
    tids = sorted({int(t) for t in type_ids if t is not None and int(t) > 0})
    if not tids:
        return {}
    candidates: List[Path] = []
    if sde_path is not None:
        candidates.append(Path(sde_path))
    else:
        env_sde = os.environ.get("EVOMU_SDE_PATH", "").strip()
        if env_sde:
            candidates.append(Path(env_sde))
        if world_db_path is not None:
            w_path = Path(world_db_path).resolve()
            for idx in range(min(4, len(w_path.parents))):
                candidates.append(w_path.parents[idx] / "data" / "evomu_sde.sqlite")
        candidates.append(REPO_ROOT / "data" / "evomu_sde.sqlite")
        candidates.append(REPO_ROOT.parent / "EvoMu" / "data" / "evomu_sde.sqlite")

    resolved_sde = next(
        (p for p in candidates if p.exists() and p.is_file() and p.stat().st_size > 0),
        None,
    )
    if resolved_sde is None:
        return {}
    conn = None
    out: Dict[int, int] = {}
    try:
        conn = sqlite3.connect(f"file:{resolved_sde.resolve()}?mode=ro", uri=True, timeout=2.0)
        for i in range(0, len(tids), 500):
            chunk = tids[i:i + 500]
            placeholders = ",".join("?" for _ in chunk)
            rows = conn.execute(
                f"SELECT type_id, category_id FROM sde_types WHERE type_id IN ({placeholders})",
                tuple(chunk),
            ).fetchall()
            for tid, cat in rows:
                if cat is not None:
                    out[int(tid)] = int(cat)
    except Exception:
        return {}
    finally:
        if conn is not None:
            try:
                conn.close()
            except Exception:
                pass
    return out


def _evaluate_hangar_parity(
    cur: sqlite3.Cursor,
    char_id: int,
    db_loc_id: int,
    inv: Dict[str, Any],
    sde_path: Optional[Path],
    world_db_path: Path,
) -> List[str]:
    """Reconcile client StationItems / StationShips / unified hangar caches against SQLite `flag = 4`."""
    c_total_cnt = inv.get("hangar_item_count")
    c_total_qty = inv.get("hangar_total_quantity")
    c_ns_cnt = inv.get("hangar_non_ship_count")
    c_ns_qty = inv.get("hangar_non_ship_total_quantity")
    c_sh_cnt = inv.get("ship_hangar_count")
    c_sh_qty = inv.get("ship_hangar_total_quantity")
    if all(v is None for v in (c_total_cnt, c_total_qty, c_ns_cnt, c_ns_qty, c_sh_cnt, c_sh_qty)):
        return []

    h_rows = cur.execute(
        "SELECT type_id, quantity FROM items "
        "WHERE owner_id = ? AND location_id = ? AND flag = 4",
        (char_id, int(db_loc_id)),
    ).fetchall()
    h_cnt = len(h_rows)
    h_qty = sum(max(0, int(q or 0)) for _, q in h_rows)
    sde_cats = _lookup_sde_type_categories(
        sde_path,
        [int(tid) for tid, _ in h_rows],
        world_db_path=world_db_path,
    )
    have_sde_split = (not h_rows) or any(int(tid) in sde_cats for tid, _ in h_rows)
    mismatches: List[str] = []

    if not have_sde_split:
        if c_total_cnt is not None and int(c_total_cnt) != h_cnt:
            mismatches.append(f"invCache.hangar_item_count client={c_total_cnt} != sqlite={h_cnt} (loc={db_loc_id})")
        if c_total_qty is not None and int(c_total_qty) != h_qty:
            mismatches.append(f"invCache.hangar_total_quantity client={c_total_qty} != sqlite={h_qty} (loc={db_loc_id})")
        return mismatches

    ns_rows = [(tid, q) for tid, q in h_rows if sde_cats.get(int(tid)) != 6]
    sh_rows = [(tid, q) for tid, q in h_rows if sde_cats.get(int(tid)) == 6]
    ns_cnt = len(ns_rows)
    ns_qty = sum(max(0, int(q or 0)) for _, q in ns_rows)
    sh_cnt = len(sh_rows)
    sh_qty = sum(max(0, int(q or 0)) for _, q in sh_rows)
    containers_seen = {str(x) for x in (inv.get("hangar_containers_seen") or []) if x}

    only_items_container = (
        (containers_seen == {"StationItems"} and c_sh_cnt in (None, 0))
        or (
            not containers_seen
            and sh_cnt > 0
            and c_sh_cnt == 0
            and (c_ns_cnt is None or int(c_ns_cnt) == ns_cnt)
            and (c_ns_cnt is not None or (c_total_cnt is not None and int(c_total_cnt) == ns_cnt))
            and (c_ns_qty is None or int(c_ns_qty) == ns_qty)
            and (c_total_cnt is None or int(c_total_cnt) == ns_cnt)
        )
    )
    only_ships_container = (
        (
            containers_seen == {"StationHangar"}
            and c_ns_cnt in (None, 0)
            and sh_cnt > 0
            and int(c_sh_cnt or c_total_cnt or 0) > 0
        )
        or (
            not containers_seen
            and ns_cnt > 0
            and c_ns_cnt == 0
            and c_sh_cnt == sh_cnt
            and (c_sh_qty is None or int(c_sh_qty) == sh_qty)
            and (c_total_cnt is None or int(c_total_cnt) == sh_cnt)
        )
    )

    if only_items_container:
        if c_ns_cnt is not None and int(c_ns_cnt) != ns_cnt:
            mismatches.append(f"invCache.hangar_non_ship_count client={c_ns_cnt} != sqlite={ns_cnt} (loc={db_loc_id})")
        if c_ns_qty is not None and int(c_ns_qty) != ns_qty:
            mismatches.append(f"invCache.hangar_non_ship_total_quantity client={c_ns_qty} != sqlite={ns_qty} (loc={db_loc_id})")
        if c_total_cnt is not None and int(c_total_cnt) != ns_cnt:
            mismatches.append(f"invCache.hangar_item_count(StationItems) client={c_total_cnt} != sqlite={ns_cnt} (loc={db_loc_id})")
        if c_total_qty is not None and int(c_total_qty) != ns_qty:
            mismatches.append(f"invCache.hangar_total_quantity(StationItems) client={c_total_qty} != sqlite={ns_qty} (loc={db_loc_id})")
    elif only_ships_container:
        if c_sh_cnt is not None and int(c_sh_cnt) != sh_cnt:
            mismatches.append(f"invCache.ship_hangar_count client={c_sh_cnt} != sqlite={sh_cnt} (loc={db_loc_id})")
        if c_sh_qty is not None and int(c_sh_qty) != sh_qty:
            mismatches.append(f"invCache.ship_hangar_total_quantity client={c_sh_qty} != sqlite={sh_qty} (loc={db_loc_id})")
        if c_total_cnt is not None and int(c_total_cnt) != sh_cnt:
            mismatches.append(f"invCache.hangar_item_count(StationShips) client={c_total_cnt} != sqlite={sh_cnt} (loc={db_loc_id})")
        if c_total_qty is not None and int(c_total_qty) != sh_qty:
            mismatches.append(f"invCache.hangar_total_quantity(StationShips) client={c_total_qty} != sqlite={sh_qty} (loc={db_loc_id})")
    else:
        if c_ns_cnt is not None and int(c_ns_cnt) != ns_cnt:
            mismatches.append(f"invCache.hangar_non_ship_count client={c_ns_cnt} != sqlite={ns_cnt} (loc={db_loc_id})")
        if c_ns_qty is not None and int(c_ns_qty) != ns_qty:
            mismatches.append(f"invCache.hangar_non_ship_total_quantity client={c_ns_qty} != sqlite={ns_qty} (loc={db_loc_id})")
        if c_sh_cnt is not None and int(c_sh_cnt) != sh_cnt:
            mismatches.append(f"invCache.ship_hangar_count client={c_sh_cnt} != sqlite={sh_cnt} (loc={db_loc_id})")
        if c_sh_qty is not None and int(c_sh_qty) != sh_qty:
            mismatches.append(f"invCache.ship_hangar_total_quantity client={c_sh_qty} != sqlite={sh_qty} (loc={db_loc_id})")
        if c_total_cnt is not None and int(c_total_cnt) != h_cnt:
            mismatches.append(f"invCache.hangar_item_count client={c_total_cnt} != sqlite={h_cnt} (loc={db_loc_id})")
        if c_total_qty is not None and int(c_total_qty) != h_qty:
            mismatches.append(f"invCache.hangar_total_quantity client={c_total_qty} != sqlite={h_qty} (loc={db_loc_id})")
    return mismatches


def evaluate_ui_delta_sqlite_parity(
    world_db_path: Path,
    state: Dict[str, Any],
    sde_path: Optional[Path] = None,
) -> Tuple[bool, str]:
    """
    Verify client-cached wallet, invCache, and clone state matches server SQLite (`world_db_path`)
    without relogging (`#1242`). Any populated (`is not None`) client cache field is compared
    directly against SQLite truth.
    """
    if not world_db_path or not Path(world_db_path).exists():
        return True, ""
    sess = (state or {}).get("session") or {}
    try:
        char_id = int(sess.get("charid") or 0)
    except Exception:
        char_id = 0
    if char_id <= 0:
        return True, ""

    wallet = state.get("wallet_state") or {}
    inv = state.get("inventory_state") or {}
    clone = state.get("clone_state") or {}
    has_any_client_cache = any(
        val is not None
        for val in (
            wallet.get("isk_balance"),
            inv.get("hangar_item_count"),
            inv.get("hangar_total_quantity"),
            inv.get("hangar_non_ship_count"),
            inv.get("hangar_non_ship_total_quantity"),
            inv.get("ship_hangar_count"),
            inv.get("ship_hangar_total_quantity"),
            inv.get("cargo_item_count"),
            inv.get("cargo_total_quantity"),
            clone.get("home_station_id"),
            clone.get("jump_clone_count"),
        )
    )
    if not has_any_client_cache:
        return True, ""

    try:
        conn = sqlite3.connect(f"file:{Path(world_db_path).resolve()}?mode=ro", uri=True, timeout=2.0)
    except Exception:
        conn = sqlite3.connect(str(world_db_path), timeout=2.0)
    mismatches: List[str] = []
    try:
        cur = conn.cursor()
        row = cur.execute(
            "SELECT station_id, structure_id, active_ship_id, balance_cents, record_json "
            "FROM characters WHERE character_id = ?",
            (char_id,),
        ).fetchone()
        if row is None:
            return True, ""
        db_station_id, db_structure_id, db_ship_id, db_balance_cents, raw_rec_json = row
        if wallet.get("isk_balance") is not None:
            client_isk = float(wallet["isk_balance"])
            server_isk = round(float(db_balance_cents) / 100.0, 2)
            if abs(client_isk - server_isk) > 0.02:
                mismatches.append(f"wallet.isk_balance client={client_isk:.2f} != sqlite={server_isk:.2f}")

        db_loc_id = db_station_id or db_structure_id
        if db_loc_id:
            mismatches.extend(
                _evaluate_hangar_parity(cur, char_id, int(db_loc_id), inv, sde_path, Path(world_db_path))
            )

        ship_id = sess.get("shipid") or db_ship_id
        if ship_id and (inv.get("cargo_item_count") is not None or inv.get("cargo_total_quantity") is not None):
            c_cnt, c_qty = cur.execute(
                "SELECT COUNT(*), COALESCE(SUM(quantity), 0) FROM items "
                "WHERE owner_id = ? AND location_id = ? AND flag = 5",
                (char_id, int(ship_id)),
            ).fetchone()
            if inv.get("cargo_item_count") is not None and int(inv["cargo_item_count"]) != int(c_cnt):
                mismatches.append(
                    f"invCache.cargo_item_count client={inv['cargo_item_count']} != sqlite={c_cnt} (ship={ship_id})"
                )
            if inv.get("cargo_total_quantity") is not None and int(inv["cargo_total_quantity"]) != int(c_qty):
                mismatches.append(
                    f"invCache.cargo_total_quantity client={inv['cargo_total_quantity']} != sqlite={c_qty} (ship={ship_id})"
                )

        if clone.get("home_station_id") is not None:
            try:
                rec_obj = json.loads(raw_rec_json) if raw_rec_json else {}
            except Exception:
                rec_obj = {}
            db_home = rec_obj.get("home_station_id") if isinstance(rec_obj, dict) else None
            if db_home is not None and int(clone["home_station_id"]) != int(db_home):
                mismatches.append(
                    f"clone_state.home_station_id client={clone['home_station_id']} != sqlite={db_home}"
                )

        if clone.get("jump_clone_count") is not None:
            jc_row = cur.execute(
                "SELECT clones_json FROM jump_clones WHERE character_id = ?",
                (char_id,),
            ).fetchone()
            if jc_row and jc_row[0]:
                try:
                    jc_parsed = json.loads(jc_row[0])
                    db_jc_count = len(jc_parsed) if isinstance(jc_parsed, (list, dict)) else 0
                except Exception:
                    db_jc_count = 0
            else:
                db_jc_count = 0
            if int(clone["jump_clone_count"]) != int(db_jc_count):
                mismatches.append(
                    f"clone_state.jump_clone_count client={clone['jump_clone_count']} != sqlite={db_jc_count}"
                )
    except sqlite3.OperationalError:
        return True, ""
    finally:
        try:
            conn.close()
        except Exception:
            pass

    if mismatches:
        return False, "; ".join(mismatches)
    return True, ""


def _unwrap_state_payload(payload: Any) -> Dict[str, Any]:
    """Return UI/session state snapshot dict whether already RPC-unwrapped or nested under `state`/`result`."""
    if not isinstance(payload, dict) or not payload:
        return {}
    for key in ("state", "result"):
        nested = payload.get(key)
        if isinstance(nested, dict):
            if nested:
                return nested
            if not any(
                k in payload
                for k in ("player", "session", "space", "ui", "active_view", "client_build", "open_windows")
            ):
                return {}
    return payload


class ScenarioRunner(PlaytesterRunner):
    """Deterministic zero-LLM scripted UI runner executing scenario files end-to-end."""

    def __init__(
        self,
        args: argparse.Namespace,
        scenario_paths: Optional[List[Path]] = None,
        out_dir: Optional[Path] = None,
    ):
        super().__init__(args)
        self.pending_startup_hygiene = False
        self.agy_pool = None
        raw_paths = scenario_paths if scenario_paths is not None else [Path(p) for p in (getattr(args, "scenarios", None) or [])]
        self.scenario_paths = [Path(p) for p in raw_paths]
        self.out_dir = Path(out_dir) if out_dir is not None else (
            Path(args.out_dir) if getattr(args, "out_dir", None) else (self.runtime_dir / f"scenario_{self.run_id}")
        )
        self._active_starting_state: Dict[str, Any] = {}
        self._active_declared_setup_steps: List[Dict[str, Any]] = []
        self._active_expected_characters: List[str] = []
        self._last_boot_result: Dict[str, Any] = {}
        self._client_launch_server_log_offset: int = 0

    def _scenario_auth_params(self) -> Tuple[str, str, int, bool]:
        st = self._active_starting_state
        user = str(st.get("account") or st.get("user") or "demo")
        password = str(st.get("password") or "demo-secret")
        char_id = int(st.get("char_id", 90000001) or 90000001)
        defer_cs = bool(st.get("defer_char_select", False))
        return user, password, char_id, defer_cs

    def _default_target_char_id(self) -> int:
        return int(self._scenario_auth_params()[2] or 90000001)

    def _scenario_requires_gm(self) -> bool:
        return any(
            int(d.get("step_index", 0) or 0) > 0
            for d in getattr(self, "_active_declared_setup_steps", [])
        )

    def _scenario_server_extra_env(self) -> Dict[str, str]:
        user, _, _, _ = self._scenario_auth_params()
        if self._scenario_requires_gm() and user == "demo":
            return {"EVOMU_ADMIN_TOKEN": PLAYTESTER_GM_ADMIN_SECRET}
        return {}

    def _server_extra_env(self) -> Dict[str, str]:
        return self._scenario_server_extra_env()

    def _gm_bridge_credentials(self) -> Tuple[str, str, int]:
        user, _, _, _ = self._scenario_auth_params()
        if user == "demo":
            return "admin", PLAYTESTER_GM_ADMIN_SECRET, 90000003
        return "evomu_scenario_gm", PLAYTESTER_GM_ADMIN_SECRET, 90000099

    def _host_gm_requests_allowed(self) -> bool:
        return bool(
            self._scenario_requires_gm()
            and getattr(self, "_active_step_allows_host_time_control", False)
        )

    def _gm_slash_requests_allowed(self) -> bool:
        return bool(
            self._scenario_requires_gm()
            and getattr(self, "_active_step_allows_host_gm_slash", False)
        )

    def _host_gm_port_for_client(self, ensure: bool = False) -> int:
        if not self._scenario_requires_gm():
            return 0
        return super()._host_gm_port_for_client(ensure=ensure)

    def write_server_config(self) -> Path:
        self.world_dir.mkdir(parents=True, exist_ok=True)
        self.current_run_dir.mkdir(parents=True, exist_ok=True)
        cfg_path = self.runtime_dir / "evomu_playtester_server.toml"
        sde_path, _ = self._resolve_sde_and_quasar_paths()
        user, password, char_id, _ = self._scenario_auth_params()
        st = self._active_starting_state
        requires_gm = self._scenario_requires_gm()
        seed_demo_flag = "true" if user == "demo" else "false"
        char_name = str(
            st.get("character_name")
            or ("Demo Pilot" if user == "demo" else f"Scenario {user}")
        )
        bloodline_id = int(st["bloodline_id"]) if st.get("bloodline_id") is not None else 1
        gender = int(st["gender"]) if st.get("gender") is not None else 1
        ancestry_id = (
            int(st["ancestry_id"])
            if st.get("ancestry_id") is not None
            else _SCENARIO_DEFAULT_ANCESTRY_BY_BLOODLINE.get(bloodline_id, 11)
        )
        school_id = (
            int(st["school_id"])
            if st.get("school_id") is not None
            else _SCENARIO_DEFAULT_SCHOOL_BY_BLOODLINE.get(bloodline_id, 17)
        )
        solar_system_id = int(st.get("solar_system_id", 30000142) or 30000142)
        station_id = int(st.get("station_id", 60003760) or 60003760)
        initial_isk = int(st["initial_balance_isk"]) if st.get("initial_balance_isk") is not None else 125000000
        initial_plex = int(st["initial_balance_plex"]) if st.get("initial_balance_plex") is not None else 50000
        gm_flag = "true" if requires_gm else "false"
        lines = [
            'bind_address = "127.0.0.1"',
            f"macho_port = {self.ports['macho']}",
            f"bridge_port = {self.ports['bridge']}",
            f"esi_port = {self.ports['http']}",
            f"quasar_grpc_port = {self.ports['gateway']}",
            f"xmpp_port = {self.ports['xmpp']}",
            f"sde_path = {json.dumps(str(sde_path))}",
            f"world_db_path = {json.dumps(str(self.world_db_path))}",
            "",
            "[feature_flags]",
            "enable_quasar_gateway = true",
            "enable_esi_http = true",
            "enable_living_world = true",
            f"seed_demo = {seed_demo_flag}",
            "free_fast_checkout_plex = true",
            f"starter_plex_balance = {initial_plex}",
            "allow_local_dev_gateway_jwt = true",
            f"gm_mode = {gm_flag}",
            "",
            "[[player_accounts]]",
            f"username = {json.dumps(user)}",
            f"password_token = {json.dumps(password)}",
            "",
            "[player_accounts.starter_character]",
            f"name = {json.dumps(char_name)}",
            f"bloodline_id = {bloodline_id}",
            f"gender = {gender}",
            f"ancestry_id = {ancestry_id}",
            f"school_id = {school_id}",
            f"solar_system_id = {solar_system_id}",
            f"station_id = {station_id}",
            f"initial_balance_isk = {initial_isk}",
            f"initial_balance_plex = {initial_plex}",
        ]
        # Never share the scenario player's username/char_id with [[admin_accounts]], or
        # `provision_gm_account` will pre-create a 0-skill character and skip starter_character/seed_demo.
        # For non-demo accounts, provision a separate helper GM account; for `demo`, pass EVOMU_ADMIN_TOKEN
        # via `_scenario_server_extra_env` so admin provisioning runs AFTER `populate_demo_snapshot`.
        if requires_gm and user != "demo":
            lines.extend([
                "",
                "[[admin_accounts]]",
                'username = "evomu_scenario_gm"',
                'password_token = "evomu-scenario-gm-secret"',
                "character_id = 90000099",
                'character_name = "Scenario GM"',
            ])
        cfg_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return cfg_path

    def write_inclient_cfg(self) -> Path:
        cfg_path = super().write_inclient_cfg()
        user, password, char_id, defer_cs = self._scenario_auth_params()
        payload = json.loads(cfg_path.read_text(encoding="utf-8"))
        payload["user"] = user
        payload["password"] = password
        payload["char_id"] = char_id
        payload["defer_char_select"] = defer_cs
        payload["scenario_mode"] = True
        cfg_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        return cfg_path

    def _mint_scenario_sso_token(self) -> str:
        user, password, char_id, _ = self._scenario_auth_params()
        try:
            return mint_sso_jwt_via_server(self.ports["http"], user, password)
        except Exception as exc:
            log_warn(f"Scenario OAuth PKCE mint failed ({exc}); falling back to offline JWT")
            return mint_sso_jwt(account_id=1, character_id=char_id)

    def start_client(self, _retry_after_downtime: bool = True) -> bool:
        self.stop_client()
        host_gm_port = self._host_gm_port_for_client(ensure=True)
        inclient_cfg = self.write_inclient_cfg()
        stage_dir = stage_playtester_client(self.staged_app, inclient_cfg, self.ports["rpc"])
        exefile = stage_dir / "bin64" / "exefile"
        if not exefile.exists():
            raise RuntimeError(f"exefile not found at {exefile}")

        sso_token = self._mint_scenario_sso_token()
        client_log = self.current_run_dir / "client.log"
        log_f = client_log.open("a", encoding="utf-8")
        client_logs_dir = self.current_run_dir / "client_logs"
        client_logs_dir.mkdir(parents=True, exist_ok=True)

        env = os.environ.copy()
        # Attestation scenarios disable all server-caused client guards so server bugs surface directly.
        env[guard_registry.DISABLE_SERVER_GUARDS_ENV] = "1"
        env["EVE_PUBLIC_GATEWAY_ADDRESS"] = f"127.0.0.1:{self.ports['gateway']}"
        env["EVOMU_HARNESS_DIR"] = str(HARNESS_DIR)
        env["PYTHONPATH"] = str(stage_dir / "evomu_py_site")
        env["EVOMU_INCLIENT_CFG"] = str(inclient_cfg)
        env["EVOMU_PLAYTESTER_CFG"] = str(inclient_cfg)
        env["EVOMU_PLAYTESTER_PORT"] = str(self.ports["rpc"])
        if host_gm_port > 0:
            env["EVOMU_HOST_GM_PORT"] = str(host_gm_port)
        else:
            env.pop("EVOMU_HOST_GM_PORT", None)

        cmd = [
            str(exefile),
            "/server:127.0.0.1",
            f"/port:{self.ports['macho']}",
            f"/fileLogDirectory:{client_logs_dir}",
            "/localdevtools",
            "/tools=evomu_playtester",
        ]
        if getattr(self.args, "silent", False):
            cmd.append("/silent")
        cmd.append(f"/ssoToken={sso_token}")
        self._pre_crashmon_pids = self._crashmon_pids()
        self.client_proc = subprocess.Popen(
            cmd,
            cwd=str(stage_dir),
            stdout=log_f,
            stderr=subprocess.STDOUT,
            env=env,
            start_new_session=True,
        )

        deadline = time.time() + 210.0
        while time.time() < deadline:
            if self.client_proc.poll() is not None:
                log_err(f"EVE.app exited early with code {self.client_proc.returncode}")
                self.stop_client()
                return False
            if self.rpc.ping():
                break
            time.sleep(0.5)
        else:
            log_err("Timed out waiting for in-client JSON-RPC hook on port " + str(self.ports["rpc"]))
            self.stop_client()
            return False

        _, _, _, defer_cs = self._scenario_auth_params()
        try:
            self._last_boot_result = self.rpc.call(
                "bootstrap_session",
                {"timeout_ms": 90000, "select_Wait_ms": 90000, "defer_char_select": defer_cs},
                timeout=110.0,
            ) or {}
            self._sync_initial_session_flight_calls(self._last_boot_result)
            return True
        except Exception as exc:
            log_err(f"Scenario bootstrap_session failed: {exc}")
            self.stop_client()
            return False

    def _provision_extra_characters(self, extra_chars: List[Dict[str, Any]]) -> List[str]:
        """Provision extra account characters cleanly via client-bridge before client launch."""
        if not extra_chars:
            return []
        bridge_dir = str(CLIENT_BRIDGE_DIR)
        if bridge_dir not in sys.path:
            sys.path.insert(0, bridge_dir)
        import evomu_bridge  # type: ignore

        user, password, _, _ = self._scenario_auth_params()

        class _SetupLog:
            def info(self, msg: str) -> None:
                log_info(f"[scenario-setup-bridge] {msg}")

        class _SetupSM:
            def __init__(self) -> None:
                self.services: Dict[str, Any] = {}
                self.logger = _SetupLog()

            def ScatterEvent(self, *_args: Any, **_kw: Any) -> None:
                pass

        sm_tmp = _SetupSM()
        bridge_conn = evomu_bridge.install_service_manager_hook(
            sm_tmp,
            host="127.0.0.1",
            port=self.ports["bridge"],
        )
        names: List[str] = []
        try:
            sm_tmp.RemoteSvc("authentication").Login(user, password)
            existing = sm_tmp.RemoteSvc("charUnboundMgr").GetCharactersToSelect() or []
            for ch in existing:
                cname = getattr(ch, "characterName", None) or (ch.get("characterName") if isinstance(ch, dict) else None)
                if cname:
                    names.append(str(cname))
            existing_low = {n.lower() for n in names}
            for spec in extra_chars:
                cname = str(spec.get("name") or "Scenario Pilot Two").strip()
                if cname.lower() in existing_low:
                    continue
                b_id = int(spec["bloodline_id"]) if spec.get("bloodline_id") is not None else 1
                g_id = int(spec["gender"]) if spec.get("gender") is not None else 0
                a_id = (
                    int(spec["ancestry_id"])
                    if spec.get("ancestry_id") is not None
                    else _SCENARIO_DEFAULT_ANCESTRY_BY_BLOODLINE.get(b_id, 11)
                )
                s_id = (
                    int(spec["school_id"])
                    if spec.get("school_id") is not None
                    else _SCENARIO_DEFAULT_SCHOOL_BY_BLOODLINE.get(b_id, 17)
                )
                sm_tmp.RemoteSvc("charUnboundMgr").CreateCharacterWithDoll(
                    cname,
                    b_id,
                    g_id,
                    a_id,
                    dict(spec.get("appearance") or {"hair": "short"}),
                    dict(spec.get("portrait") or {"poseID": 1}),
                    s_id,
                )
                names.append(cname)
                existing_low.add(cname.lower())
        finally:
            try:
                bridge_conn.close()
            except Exception:
                pass
        return names

    def _server_bookmark_exists(self, name: str) -> bool:
        if not self.world_db_path.exists():
            return False
        import sqlite3
        try:
            with sqlite3.connect(str(self.world_db_path)) as conn:
                cur = conn.execute(
                    "SELECT COUNT(*) FROM bookmarks WHERE LOWER(label) LIKE ? OR LOWER(memo) LIKE ?",
                    (f"%{name.lower()}%", f"%{name.lower()}%"),
                )
                row = cur.fetchone()
                return bool(row and int(row[0]) > 0)
        except Exception:
            return False

    def _server_handshake_seen(self, account: str) -> bool:
        slog = self.current_run_dir / "server.log"
        if not slog.exists():
            return False
        offset = int(getattr(self, "_client_launch_server_log_offset", 0) or 0)
        size = slog.stat().st_size
        if size < offset:
            offset = 0
        with slog.open("rb") as f:
            f.seek(offset)
            post_txt = f.read(size - offset).decode("utf-8", errors="replace").lower()
        marker = "accepted native machonet connection"
        idx = post_txt.find(marker)
        if idx < 0:
            return False
        after_accept = post_txt[idx + len(marker):]
        return (
            "dispatching rpc call" in after_accept
            and (
                "user_id=some(" in after_accept
                or "charunboundmgr" in after_accept
                or "getcharacterselectiondata" in after_accept
                or "getcharacterstoselect" in after_accept
                or "selectcharacterid" in after_accept
                or "authentication" in after_accept
            )
        )

    def _check_server_unimplemented_rpc(self) -> List[str]:
        slog = self.current_run_dir / "server.log"
        if not slog.exists():
            return []
        offset = int(getattr(self, "_client_launch_server_log_offset", 0) or 0)
        size = slog.stat().st_size
        if size < offset:
            offset = 0
        with slog.open("rb") as f:
            f.seek(offset)
            txt = f.read(size - offset).decode("utf-8", errors="replace")
        return [ln.strip()[:240] for ln in txt.splitlines() if "UnimplementedRPC" in ln]

    @staticmethod
    def _step_rpc_timeout(action: Dict[str, Any]) -> float:
        return _compute_step_rpc_timeout(action)

    def _execute_wait_until(
        self,
        step: Dict[str, Any],
        step_pre_state: Dict[str, Any],
        context: Dict[str, Any],
        initial_res: Optional[Dict[str, Any]] = None,
        initial_state: Optional[Dict[str, Any]] = None,
    ) -> Tuple[Dict[str, Any], Dict[str, Any]]:
        wait_until = step.get("wait_until") or {}
        until_checks = dict(wait_until.get("checks") or {})
        timeout_ms = int(wait_until.get("timeout_ms", 60000) or 60000)
        poll_ms = max(250, min(5000, int(wait_until.get("poll_interval_ms", 1200) or 1200)))
        deadline = time.time() + (timeout_ms / 1000.0)
        init_fr = (initial_res or {}).get("flight_recorder") or {}
        all_calls: List[Dict[str, Any]] = list(init_fr.get("macho_calls") or [])
        all_scatters: List[Any] = list(init_fr.get("scatters_observed") or [])
        all_log_excs: List[Any] = list(init_fr.get("log_exceptions") or [])
        all_quasar_errs: List[Any] = list(init_fr.get("quasar_errors") or [])
        all_macho_excs: List[Any] = list(init_fr.get("macho_exceptions") or [])
        all_guard_hits: List[Dict[str, Any]] = []
        seen_guard_keys: set = set()
        latest_guard_summary: Optional[Dict[str, Any]] = None
        sim_wait_tracker = _SimTimeTracker(retain_events=True, max_recent=500)
        sim_wait_tracker.absorb_payload(initial_res)

        def _absorb_wait_guard_telemetry(res_obj: Optional[Dict[str, Any]]) -> None:
            nonlocal latest_guard_summary
            if not isinstance(res_obj, dict):
                return
            fr_obj = res_obj.get("flight_recorder") or {}
            for cand in (res_obj.get("guard_summary"), fr_obj.get("guard_summary")):
                if isinstance(cand, dict):
                    latest_guard_summary = dict(cand)
            raw_hits = res_obj.get("guard_hits")
            if not isinstance(raw_hits, list):
                raw_hits = fr_obj.get("guard_hits")
            if not isinstance(raw_hits, list):
                return
            for gh in raw_hits:
                if not isinstance(gh, dict):
                    continue
                seq_v = gh.get("seq")
                if isinstance(seq_v, int):
                    gkey: Any = ("seq", seq_v)
                else:
                    gkey = (
                        "raw",
                        str(gh.get("guard") or ""),
                        str(gh.get("function") or ""),
                        str(gh.get("reason") or ""),
                        gh.get("time"),
                    )
                if gkey in seen_guard_keys:
                    continue
                seen_guard_keys.add(gkey)
                all_guard_hits.append(dict(gh))

        _absorb_wait_guard_telemetry(initial_res)
        latest_state = dict(initial_state) if isinstance(initial_state, dict) and initial_state else dict(step_pre_state)
        latest_res: Dict[str, Any] = dict(initial_res or {"action_ok": True, "flight_recorder": {}})

        if initial_res is not None:
            if all_log_excs or all_quasar_errs or all_macho_excs or latest_res.get("action_ok") is False:
                return latest_res, latest_state
            ok_init, _, _ = evaluate_scenario_checks(
                until_checks, step_pre_state, latest_state, latest_res, context=context
            )
            if ok_init:
                return latest_res, latest_state

        use_tc_bridge = bool(step.get("time_control", False))
        while True:
            if use_tc_bridge and str((step.get("action") or {}).get("type") or "") != "advance_server_time":
                try:
                    wait_target_cid = int(
                        (latest_state.get("session") or {}).get("charid")
                        or (step_pre_state.get("session") or {}).get("charid")
                        or self._default_target_char_id()
                    )
                except Exception:
                    wait_target_cid = self._default_target_char_id()
                gm_tick_call = self._advance_clock_via_gm_bridge(12, target_char_id=wait_target_cid)
                if gm_tick_call is not None:
                    all_calls.append(gm_tick_call)
                    sim_wait_tracker.absorb_events([
                        {
                            "injection_id": gm_tick_call.get("injection_id"),
                            "seconds": gm_tick_call.get("seconds", 12),
                            "target_char_id": gm_tick_call.get("target_char_id", wait_target_cid),
                            "via": gm_tick_call.get("via", "gm_bridge"),
                            "source": "wait_until_bridge",
                            "method": "AdvanceClock",
                        }
                    ])
            tick_res = self.rpc.call(
                "execute_action",
                {
                    "action": {
                        "type": "wait",
                        "wait_ms": poll_ms,
                        "scenario_mode": True,
                        "time_control": False,
                    }
                },
                timeout=max(20.0, (poll_ms / 1000.0) + 15.0),
            ) or {}
            fr = tick_res.get("flight_recorder") or {}
            all_calls.extend(fr.get("macho_calls") or [])
            all_scatters.extend(fr.get("scatters_observed") or [])
            all_log_excs.extend(fr.get("log_exceptions") or [])
            all_quasar_errs.extend(fr.get("quasar_errors") or [])
            all_macho_excs.extend(fr.get("macho_exceptions") or [])
            _absorb_wait_guard_telemetry(tick_res)
            sim_wait_tracker.absorb_payload(tick_res)
            if isinstance(tick_res.get("post_state"), dict):
                latest_state = tick_res["post_state"]
            merged_res = dict(initial_res or tick_res)
            if initial_res and tick_res.get("action_ok") is False:
                merged_res["action_ok"] = False
                merged_res["action_error"] = tick_res.get("action_error")
            merged_res["guard_hits"] = list(all_guard_hits)
            merged_res["guard_hit_count"] = len(all_guard_hits)
            wait_sim_sum = sim_wait_tracker.summary()
            merged_res["sim_time_injected_sec"] = wait_sim_sum["total_sim_time_injected_sec"]
            merged_res["sim_time_injection_count"] = wait_sim_sum["total_sim_time_injection_count"]
            merged_res["sim_time_injections"] = list(sim_wait_tracker.events)
            merged_fr: Dict[str, Any] = {
                "macho_calls": all_calls,
                "scatters_observed": all_scatters,
                "log_exceptions": all_log_excs,
                "quasar_errors": all_quasar_errs,
                "macho_exceptions": all_macho_excs,
                "guard_hits": list(all_guard_hits),
                "sim_time_injections": list(sim_wait_tracker.events),
            }
            if isinstance(latest_guard_summary, dict):
                has_bool_off = isinstance(latest_guard_summary.get("client_guards_off"), bool)
                wait_guards_off = bool(latest_guard_summary["client_guards_off"]) if has_bool_off else False
                merged_guard_summary = guard_registry.summarize_guard_hits(
                    all_guard_hits,
                    client_guards_off=wait_guards_off,
                )
                if not has_bool_off:
                    merged_guard_summary.pop("client_guards_off", None)
                merged_res["guard_summary"] = dict(merged_guard_summary)
                merged_fr["guard_summary"] = dict(merged_guard_summary)
            else:
                merged_res.pop("guard_summary", None)
            merged_res["flight_recorder"] = merged_fr
            latest_res = merged_res
            ok_until, until_fail_chk, until_fail_msg = evaluate_scenario_checks(
                until_checks, step_pre_state, latest_state, merged_res, context=context
            )
            if ok_until:
                return latest_res, latest_state
            if all_log_excs or all_quasar_errs or all_macho_excs or tick_res.get("action_ok") is False:
                return latest_res, latest_state
            if time.time() >= deadline:
                latest_res = dict(latest_res)
                latest_res["action_ok"] = False
                latest_res["wait_until_timed_out"] = True
                timeout_detail = (
                    f"wait_until_timeout ({timeout_ms}ms, {until_fail_chk}): {until_fail_msg}"
                )
                prev_err = str(latest_res.get("action_error") or "").strip()
                latest_res["action_error"] = (
                    f"{timeout_detail} (previous: {prev_err})" if prev_err else timeout_detail
                )
                return latest_res, latest_state

    def _run_destiny_parity_check(self, client_logs: List[Path]) -> Optional[Dict[str, Any]]:
        cls_script = REPO_ROOT / "tools" / "classify_client_log.py"
        existing = [str(p) for p in client_log_paths_exist(client_logs)]
        if not existing or not cls_script.exists():
            return None
        try:
            proc = subprocess.run(
                [sys.executable, str(cls_script), "--expect", "parity", "--allow-empty", "--json"] + existing,
                cwd=str(REPO_ROOT),
                capture_output=True,
                text=True,
                timeout=15,
            )
            if proc.stdout.strip():
                return json.loads(proc.stdout)
        except Exception as exc:
            log_warn(f"classify_client_log failed: {exc}")
        return None

    def _capture_failure_screenshot(self, dest_path: Path) -> Optional[str]:
        try:
            pid = self.client_proc.pid if self.client_proc is not None else None
            cap = capture_window_screenshot(
                dest_path,
                client_pid=pid,
                bring_foreground=bool(getattr(self.args, "foreground", False)),
                rpc_client=self.rpc,
                max_dim=self.screenshot_max_dim,
            )
            if isinstance(cap, dict) and cap.get("ok"):
                return str(dest_path)
        except Exception:
            pass
        return None

    def run_single_scenario(self, scenario: Dict[str, Any]) -> Tuple[Dict[str, Any], Dict[str, Any]]:
        """Execute one validated scenario cleanly from a fresh world without LLM calls."""
        sid = scenario["id"]
        scen_dir = self.out_dir / sid
        scen_dir.mkdir(parents=True, exist_ok=True)
        self.current_run_dir = scen_dir
        self._active_starting_state = dict(scenario.get("starting_state") or {})
        self._active_declared_setup_steps = list(scenario.get("declared_setup_steps") or [])
        self._active_expected_characters = []
        self._last_boot_result = {}

        t_scen_start = time.time()
        guard_tracker = _GuardHitTracker(retain_all=True, max_recent=500)
        sim_tracker = _SimTimeTracker(retain_events=True, max_recent=500)
        exemption_tracker = _UserErrorExemptionTracker(retain_all=True, max_recent=500)
        self._drain_host_gm_sim_events()
        step_records: List[Dict[str, Any]] = []
        step_timings_ms: Dict[str, float] = {}
        step_rpcs: Dict[str, List[str]] = {}
        failure_info: Optional[Dict[str, Any]] = None
        client_version = ""
        client_build = _coerce_int_client_build(_detect_system_eve_build(cache_root=DEFAULT_CACHE_ROOT))
        destiny_cls: Optional[Dict[str, Any]] = None
        log_errors: List[str] = []
        client_logs: List[Path] = []
        passed = False

        slog_path = scen_dir / "server.log"
        step_log_offsets: Dict[str, int] = {}
        self._client_launch_server_log_offset = 0

        def _gather_client_logs() -> Tuple[List[Path], Path]:
            logs: List[Path] = []
            clog_m = scen_dir / "client.log"
            if clog_m.exists():
                logs.append(clog_m)
            cdir = scen_dir / "client_logs"
            if cdir.exists():
                for p in sorted(cdir.glob("*.log")):
                    if p.name != "client_tracebacks.log" and p not in logs:
                        logs.append(p)
            primary_tb = scen_dir / "client_tracebacks.log"
            legacy_tb = cdir / "client_tracebacks.log"
            if primary_tb.exists():
                return logs, primary_tb
            if legacy_tb.exists():
                return logs, legacy_tb
            return logs, primary_tb

        try:
            if not self.start_server(reset_db=True):
                failure_info = {
                    "step_index": 0,
                    "step_id": "start_server",
                    "failed_check": "server_start",
                    "reason": "eve-server failed to start",
                    "client_state": {},
                    "last_rpcs": [],
                    "screenshot": None,
                }
            else:
                extra_chars = list(self._active_starting_state.get("extra_characters") or [])
                if extra_chars:
                    self._active_expected_characters = self._provision_extra_characters(extra_chars)

                self._client_launch_server_log_offset = slog_path.stat().st_size if slog_path.exists() else 0
                step_log_offsets[str(slog_path)] = self._client_launch_server_log_offset

                if not self.start_client():
                    failure_info = {
                        "step_index": 0,
                        "step_id": "start_client",
                        "failed_check": "client_start",
                        "reason": "EVE.app client failed to launch or bootstrap_session",
                        "client_state": {},
                        "last_rpcs": [],
                        "screenshot": self._capture_failure_screenshot(scen_dir / "failure_start_client.png"),
                    }
                else:
                    boot_state = _unwrap_state_payload(self._last_boot_result)
                    if not boot_state:
                        obs = self.rpc.call("get_state", timeout=15.0) or {}
                        boot_state = _unwrap_state_payload(obs)
                    if boot_state.get("client_build"):
                        client_build = _coerce_int_client_build(boot_state["client_build"])
                    if boot_state.get("client_version"):
                        client_version = str(boot_state["client_version"])

                    startup_fr = (self._last_boot_result.get("flight_recorder") or {})
                    guard_tracker.absorb_payload(self._last_boot_result)
                    startup_calls = list(startup_fr.get("macho_calls") or [])
                    if not startup_calls:
                        try:
                            fr_resp = self.rpc.call("get_flight_recorder", {"since_seq": 0}, timeout=8.0) or {}
                            if isinstance(fr_resp.get("result"), dict):
                                fr_resp = fr_resp["result"]
                            guard_tracker.absorb_payload(fr_resp)
                            startup_calls = list(fr_resp.get("macho_calls") or [])
                        except Exception:
                            startup_calls = []

                    if startup_calls:
                        self._record_session_flight_calls(startup_calls)
                    pre_state = dict(boot_state)
                    last_live_skill_state = dict(boot_state) if isinstance(boot_state, dict) else {}
                    recent_rpcs: List[str] = [_format_rpc_name(c) for c in startup_calls if _format_rpc_name(c)]
                    user_cfg, _, _, _ = self._scenario_auth_params()
                    primary_char_name = str(
                        self._active_starting_state.get("character_name")
                        or ("Demo Pilot" if user_cfg == "demo" else f"Scenario {user_cfg}")
                    )

                    for idx, step in enumerate(scenario["steps"]):
                        step_num = idx + 1
                        step_id = step["id"]
                        act = dict(step["action"])
                        act_type = _canonicalize_action_type(str(act.get("type") or ""), bool(act.get("target")))
                        t_step = time.time()
                        ctx = {
                            "primary_character_name": primary_char_name,
                            "expected_characters": self._active_expected_characters,
                            "startup_macho_calls": startup_calls if idx == 0 else [],
                            "server_bookmark_exists": self._server_bookmark_exists,
                            "server_handshake_seen": self._server_handshake_seen,
                            "check_server_unimplemented_rpc": self._check_server_unimplemented_rpc,
                            "expected_text": str(act.get("expected_ui_and_state_change") or ""),
                            "ui_state": last_live_skill_state,
                        }
                        prev_allow_tc = getattr(self, "_active_step_allows_host_time_control", None)
                        prev_allow_gm_slash = getattr(self, "_active_step_allows_host_gm_slash", None)
                        self._active_step_allows_host_time_control = bool(step.get("time_control", False))
                        self._active_step_allows_host_gm_slash = bool(step.get("setup", False))
                        try:
                            if act_type == "wait" and step.get("wait_until"):
                                exec_res, post_state = self._execute_wait_until(step, pre_state, ctx)
                            elif self._should_dispatch_via_gm_bridge(act, act_type):
                                try:
                                    target_cid = int(
                                        (pre_state.get("session") or {}).get("charid")
                                        or (boot_state.get("session") or {}).get("charid")
                                        or self._scenario_auth_params()[2]
                                        or 90000001
                                    )
                                except Exception:
                                    target_cid = int(self._scenario_auth_params()[2] or 90000001)
                                exec_res = self._execute_gm_or_time_control_via_bridge(act, act_type, target_cid)
                                try:
                                    obs = self.rpc.call("get_state", timeout=15.0) or {}
                                    post_state = _unwrap_state_payload(obs)
                                except Exception:
                                    post_state = {}
                                if step.get("wait_until"):
                                    exec_res, post_state = self._execute_wait_until(
                                        step, pre_state, ctx, initial_res=exec_res, initial_state=post_state
                                    )
                            else:
                                payload = dict(act)
                                payload["type"] = act_type
                                payload["scenario_mode"] = True
                                # Player session always stays role = 0; never invoke timeControl.* on it.
                                payload["time_control"] = False
                                payload["host_time_control"] = bool(step.get("time_control", False))
                                try:
                                    exec_res = self.rpc.call(
                                        "execute_action",
                                        {"action": payload},
                                        timeout=self._step_rpc_timeout(payload),
                                    ) or {}
                                except Exception as exc:
                                    exec_res = {"action_ok": False, "action_error": str(exc), "flight_recorder": {}}
                                post_state = _unwrap_state_payload(exec_res.get("post_state"))
                                if not post_state:
                                    try:
                                        obs = self.rpc.call("get_state", timeout=15.0) or {}
                                        post_state = _unwrap_state_payload(obs)
                                    except Exception:
                                        post_state = {}
                                if step.get("wait_until"):
                                    exec_res, post_state = self._execute_wait_until(
                                        step, pre_state, ctx, initial_res=exec_res, initial_state=post_state
                                    )
                        finally:
                            self._active_step_allows_host_time_control = prev_allow_tc
                            self._active_step_allows_host_gm_slash = prev_allow_gm_slash

                        elapsed_ms = round((time.time() - t_step) * 1000.0, 2)
                        self._record_session_flight_calls(
                            (exec_res.get("flight_recorder") or {}).get("macho_calls")
                            if isinstance(exec_res, dict)
                            else None
                        )
                        rpcs_for_step = _collect_step_rpcs(exec_res)
                        if idx == 0 and startup_calls:
                            rpcs_for_step = sorted(set(rpcs_for_step) | {_format_rpc_name(c) for c in startup_calls if _format_rpc_name(c)})
                        recent_rpcs.extend(rpcs_for_step)
                        step_timings_ms[step_id] = elapsed_ms
                        step_rpcs[step_id] = rpcs_for_step

                        if not client_version and isinstance(post_state, dict) and post_state.get("client_version"):
                            client_version = str(post_state["client_version"])
                        if isinstance(post_state, dict) and post_state.get("client_build"):
                            client_build = _coerce_int_client_build(post_state["client_build"])

                        step_chk_cfg = dict(step.get("checks") or {})
                        ok_step, fail_chk, fail_msg = evaluate_scenario_checks(
                            step_chk_cfg,
                            pre_state,
                            post_state,
                            exec_res,
                            context=ctx,
                        )
                        if (
                            ok_step
                            and self.world_db_path.exists()
                            and step_chk_cfg.get("ui_delta_without_relog") is not False
                            and _is_mutating_scenario_step(step, act_type, exec_res)
                        ):
                            scenario_sde_path, _ = self._resolve_sde_and_quasar_paths()
                            ok_delta, delta_msg = evaluate_ui_delta_sqlite_parity(
                                self.world_db_path, post_state, sde_path=scenario_sde_path
                            )
                            if not ok_delta:
                                delta_deadline = time.time() + 2.0
                                while time.time() < delta_deadline:
                                    time.sleep(min(0.25, max(0.05, delta_deadline - time.time())))
                                    try:
                                        obs_delta = self.rpc.call("get_state", timeout=5.0) or {}
                                        polled_state = _unwrap_state_payload(obs_delta)
                                        if polled_state:
                                            post_state = polled_state
                                    except Exception:
                                        pass
                                    ok_delta, delta_msg = evaluate_ui_delta_sqlite_parity(
                                        self.world_db_path, post_state, sde_path=scenario_sde_path
                                    )
                                    if ok_delta:
                                        break
                            if not ok_delta:
                                ok_step = False
                                fail_chk = "ui_delta_without_relog"
                                fail_msg = f"Client cache diverged from server SQLite after 2.0s without relog: {delta_msg}"
                        cur_logs, cur_tb_path = _gather_client_logs()
                        step_log_exemptions: List[Dict[str, Any]] = []
                        scen_step_state = _select_live_skill_state(
                            post_state,
                            _select_live_skill_state(pre_state, last_live_skill_state),
                        )
                        last_live_skill_state = (
                            _select_live_skill_state(scen_step_state, last_live_skill_state)
                            or last_live_skill_state
                        )
                        step_log_errs = scan_scenario_logs_for_errors(
                            slog_path,
                            cur_logs,
                            traceback_log_path=cur_tb_path,
                            log_offsets=step_log_offsets,
                            ui_state=scen_step_state,
                            exemptions_out=step_log_exemptions,
                            step_num=step_num,
                        )
                        if step_chk_cfg.get("no_client_exception") is False:
                            step_log_errs = [
                                e for e in step_log_errs
                                if "destiny desync" in e.lower() or e.startswith("[server.log]")
                            ]
                        if ok_step and step_log_errs:
                            ok_step = False
                            first_err = step_log_errs[0]
                            if "destiny desync" in first_err.lower():
                                fail_chk = "destiny_desync"
                            elif first_err.startswith("[client_tracebacks.log]") or not first_err.startswith("[server.log]"):
                                fail_chk = "no_client_exception"
                            else:
                                fail_chk = "clean_logs"
                            fail_msg = first_err
                        if step_log_errs:
                            log_errors.extend(step_log_errs)

                        has_inline_guards = isinstance(exec_res, dict) and (
                            isinstance(exec_res.get("guard_hits"), list)
                            or isinstance((exec_res.get("flight_recorder") or {}).get("guard_hits"), list)
                        )
                        step_guard_hits = guard_tracker.absorb_payload(exec_res)
                        if not has_inline_guards:
                            step_guard_hits.extend(
                                guard_tracker.poll_rpc(getattr(self, "rpc", None), timeout=1.0)
                            )
                        step_sim = self._absorb_step_sim_time(exec_res, tracker=sim_tracker)
                        step_ex_candidates = PlaytesterBugOracle.collect_exec_res_user_error_exemptions(
                            exec_res,
                            expected_text=str(ctx.get("expected_text") or ""),
                            ui_state=scen_step_state,
                            step_num=step_num,
                        )
                        step_ex_candidates.extend(step_log_exemptions)
                        step_ex_hits = exemption_tracker.absorb_hits(
                            step_ex_candidates, step_num=step_num
                        )
                        step_records.append({
                            "step_index": step_num,
                            "step_id": step_id,
                            "action": act,
                            "passed": ok_step,
                            "failed_check": fail_chk or None,
                            "failure_reason": fail_msg or None,
                            "elapsed_ms": elapsed_ms,
                            "rpcs": rpcs_for_step,
                            "setup": bool(step.get("setup", False)),
                            "time_control": bool(step.get("time_control", False)),
                            "guard_hits": step_guard_hits,
                            "guard_hits_count": len(step_guard_hits),
                            "user_error_exemptions": step_ex_hits,
                            "user_error_exemption_count": len(step_ex_hits),
                            "sim_time_injected_sec": step_sim["sim_time_injected_sec"],
                            "sim_time_injection_count": step_sim["sim_time_injection_count"],
                            "sim_time_injections": step_sim["sim_time_injections"],
                        })
                        if not ok_step:
                            shot_path = self._capture_failure_screenshot(scen_dir / f"failure_step_{step_num}_{step_id}.png")
                            failure_info = {
                                "step_index": step_num,
                                "step_id": step_id,
                                "failed_check": fail_chk,
                                "reason": fail_msg,
                                "client_state": post_state,
                                "last_rpcs": recent_rpcs[-30:],
                                "screenshot": shot_path,
                            }
                            log_err(f"Scenario {sid} failed at step {step_num} ({step_id}) [{fail_chk}]: {fail_msg}")
                            break
                        pre_state = dict(post_state) if isinstance(post_state, dict) else pre_state
        finally:
            guard_tracker.poll_rpc(getattr(self, "rpc", None), timeout=1.0)
            self._drain_host_gm_sim_events()
            self._close_gm_bridge()
            self.stop_client()
            self.stop_server()

        client_logs, tb_log_path = _gather_client_logs()
        destiny_cls = self._run_destiny_parity_check(client_logs)
        if destiny_cls and int(destiny_cls.get("stale_at_action", 0) or 0) > 0:
            log_errors.append(
                f"[classify_client_log] destiny desync detected: stale_at_action={destiny_cls.get('stale_at_action')}"
            )
        tail_log_exemptions: List[Dict[str, Any]] = []
        tail_ui_state = _select_live_skill_state(
            pre_state if "pre_state" in locals() else None,
            last_live_skill_state if "last_live_skill_state" in locals() else None,
        )
        log_errors.extend(
            scan_scenario_logs_for_errors(
                slog_path,
                client_logs,
                traceback_log_path=tb_log_path,
                log_offsets=step_log_offsets,
                ui_state=tail_ui_state,
                exemptions_out=tail_log_exemptions,
                step_num=len(step_records),
                channel_prefix="scenario_tail_",
                dedup_namespace="tail",
            )
        )
        if tail_log_exemptions:
            exemption_tracker.absorb_hits(
                tail_log_exemptions, step_num=len(step_records)
            )

        if failure_info is None and log_errors:
            failure_info = {
                "step_index": len(step_records),
                "step_id": "log_hygiene",
                "failed_check": "clean_logs",
                "reason": log_errors[0],
                "client_state": pre_state if "pre_state" in locals() else {},
                "last_rpcs": recent_rpcs[-30:] if "recent_rpcs" in locals() else [],
                "screenshot": None,
            }

        passed = (failure_info is None) and (not log_errors)
        total_sec = round(time.time() - t_scen_start, 2)
        if failure_info is not None:
            (scen_dir / "failure.json").write_text(json.dumps(failure_info, indent=2), encoding="utf-8")

        journey_entry = {
            "passed": passed,
            "ui_scenario": True,
            "client_build": client_build,
            "log_errors": list(log_errors),
            "title": scenario["title"],
            "evidence": (
                f"Scenario {sid!r} completed {len(step_records)} steps in {total_sec:.2f}s cleanly"
                if passed
                else f"FAILED at {failure_info.get('step_id')} ({failure_info.get('failed_check')}): {failure_info.get('reason')}"
            ),
            "declared_setup_steps": scenario.get("declared_setup_steps") or [],
            "step_timings_ms": step_timings_ms,
            "step_rpcs": step_rpcs,
            "total_elapsed_sec": total_sec,
            "steps": step_records,
            "failure": failure_info,
        }
        guard_summary = guard_tracker.summary(default_guards_off=False)
        sim_summary = sim_tracker.summary()
        ex_summary = exemption_tracker.summary()
        journey_entry["client_guards_off"] = guard_summary["client_guards_off"]
        journey_entry["guard_hits"] = list(guard_tracker.hits)
        journey_entry["guard_hits_count"] = guard_summary["guard_hits_count"]
        journey_entry["guard_hits_by_guard"] = guard_summary["guard_hits_by_guard"]
        journey_entry["client_only_guard_hits"] = guard_summary["client_only_guard_hits"]
        journey_entry["client_only_guard_hits_by_guard"] = guard_summary[
            "client_only_guard_hits_by_guard"
        ]
        journey_entry["user_error_exemptions"] = list(exemption_tracker.hits)
        journey_entry["user_error_exemptions_count"] = ex_summary["user_error_exemptions_count"]
        journey_entry["user_error_exemptions_by_key"] = ex_summary["user_error_exemptions_by_key"]
        journey_entry["user_error_exemptions_by_mechanism"] = ex_summary[
            "user_error_exemptions_by_mechanism"
        ]
        journey_entry["precondition_verified_user_error_exemptions"] = ex_summary[
            "precondition_verified_user_error_exemptions"
        ]
        journey_entry["unverified_user_error_exemptions"] = ex_summary[
            "unverified_user_error_exemptions"
        ]
        journey_entry["sim_time_injected_sec"] = sim_summary["total_sim_time_injected_sec"]
        journey_entry["sim_time_injection_count"] = sim_summary["total_sim_time_injection_count"]
        journey_entry["sim_time_injections"] = list(sim_tracker.events)
        meta = {
            "client_version": client_version,
            "client_build": client_build,
            "server_log": str(slog_path),
            "client_logs": [str(p) for p in client_logs],
            "log_errors": log_errors,
            "destiny_log_classification": destiny_cls,
        }
        return journey_entry, meta

    def run_scenarios(self) -> int:
        """Load, statically validate, and run all `--scenario` files, writing `<out>/result.json`."""
        if not self.scenario_paths:
            log_err("No --scenario files supplied")
            return 2
        loaded = [load_scenario_file(p) for p in self.scenario_paths]
        self.out_dir.mkdir(parents=True, exist_ok=True)
        server_commit = get_git_head(self.worktree_dir)

        journeys: Dict[str, Any] = {}
        all_client_logs: List[str] = []
        all_log_errors: List[str] = []
        last_server_log = ""
        last_destiny_cls: Optional[Dict[str, Any]] = None
        client_version = ""
        client_build = 0

        for scen in loaded:
            entry, meta = self.run_single_scenario(scen)
            entry["git_sha"] = server_commit
            journeys[scen["id"]] = entry
            all_client_logs.extend(meta["client_logs"])
            all_log_errors.extend(meta["log_errors"])
            last_server_log = meta["server_log"]
            if meta["destiny_log_classification"] is not None:
                last_destiny_cls = meta["destiny_log_classification"]
            if meta["client_version"]:
                client_version = meta["client_version"]
            if meta["client_build"]:
                client_build = _coerce_int_client_build(meta["client_build"])

        all_guard_hits: List[Dict[str, Any]] = []
        for entry_item in journeys.values():
            if isinstance(entry_item.get("guard_hits"), list):
                all_guard_hits.extend(entry_item["guard_hits"])
        all_guards_off = (
            all(bool(entry_item.get("client_guards_off", False)) for entry_item in journeys.values())
            if journeys
            else False
        )
        guard_rollup = guard_registry.summarize_guard_hits(
            all_guard_hits, client_guards_off=all_guards_off
        )
        sim_rollup_tracker = _SimTimeTracker(retain_events=False, max_recent=200)
        for entry_item in journeys.values():
            sim_rollup_tracker.absorb_payload(entry_item)
        sim_rollup = sim_rollup_tracker.summary()
        all_exemptions: List[Dict[str, Any]] = []
        for entry_item in journeys.values():
            if isinstance(entry_item.get("user_error_exemptions"), list):
                all_exemptions.extend(entry_item["user_error_exemptions"])
        ex_rollup = summarize_user_error_exemptions(all_exemptions)

        result_payload = {
            "journeys": journeys,
            "client_version": client_version,
            "client_build": client_build,
            "server_commit": server_commit,
            "server_log": last_server_log,
            "client_logs": all_client_logs,
            "log_errors": all_log_errors,
            "destiny_log_classification": last_destiny_cls,
            "client_guards_off": guard_rollup["client_guards_off"],
            "guard_hits_count": guard_rollup["guard_hits_count"],
            "guard_hits_by_guard": guard_rollup["guard_hits_by_guard"],
            "client_only_guard_hits": guard_rollup["client_only_guard_hits"],
            "client_only_guard_hits_by_guard": guard_rollup["client_only_guard_hits_by_guard"],
            "user_error_exemptions_count": ex_rollup["user_error_exemptions_count"],
            "user_error_exemptions_by_key": ex_rollup["user_error_exemptions_by_key"],
            "user_error_exemptions_by_mechanism": ex_rollup["user_error_exemptions_by_mechanism"],
            "precondition_verified_user_error_exemptions": ex_rollup[
                "precondition_verified_user_error_exemptions"
            ],
            "unverified_user_error_exemptions": ex_rollup["unverified_user_error_exemptions"],
            "total_sim_time_injected_sec": sim_rollup["total_sim_time_injected_sec"],
            "total_sim_time_injection_count": sim_rollup["total_sim_time_injection_count"],
        }
        result_path = self.out_dir / "result.json"
        result_path.write_text(json.dumps(result_payload, indent=2), encoding="utf-8")
        all_passed = all(j.get("passed") for j in journeys.values()) and not all_log_errors
        if all_passed:
            log_info(f"All {len(journeys)} scenario(s) PASSED! Wrote {result_path}")
            return 0
        log_err(f"Scenario run FAILED! Wrote {result_path}")
        return 1


def client_log_paths_exist(paths: List[Path]) -> List[Path]:
    return [p for p in paths if p and p.exists() and p.stat().st_size > 0]


# ---------------------------------------------------------------------------
# CLI Entrypoint & Ping / Self-Test Helpers
# ---------------------------------------------------------------------------

def send_ping_reload() -> int:
    """Signal a running playtester daemon to hot-reload `eve-server` immediately."""
    DEFAULT_CONTROL_FILE.write_text(f"RELOAD {utc_now_iso()}\n", encoding="utf-8")
    signaled_pid = None
    if DEFAULT_PID_FILE.exists():
        try:
            pid = int(DEFAULT_PID_FILE.read_text(encoding="utf-8").strip())
            os.kill(pid, signal.SIGUSR1)
            signaled_pid = pid
        except Exception:
            pass
    if signaled_pid:
        log_info(
            f"Sent SIGUSR1 to running playtester (PID {signaled_pid}) and wrote {DEFAULT_CONTROL_FILE}."
        )
    else:
        log_info(
            f"Wrote reload trigger to {DEFAULT_CONTROL_FILE} (will be picked up by playtester daemon)."
        )
    return 0


def run_self_test(args: argparse.Namespace) -> int:
    """Verify schema, personas, hook syntax, model chain resolution, and worktree paths."""
    log_info("Running playtester self-test...")
    assert ACTION_SCHEMA_PATH.exists(), f"Missing {ACTION_SCHEMA_PATH}"
    schema = json.loads(ACTION_SCHEMA_PATH.read_text(encoding="utf-8"))
    assert "action" in schema.get("properties", {}), "Invalid action schema"
    assert "jump_gate" in schema["properties"]["action"]["properties"]["type"]["enum"], "Missing jump_gate in action schema"

    assert PLANNER_SCHEMA_PATH.exists(), f"Missing {PLANNER_SCHEMA_PATH}"
    planner_schema = json.loads(PLANNER_SCHEMA_PATH.read_text(encoding="utf-8"))
    assert "delegated_plan" in planner_schema.get("properties", {}), "Invalid planner schema"

    assert EXECUTOR_SCHEMA_PATH.exists(), f"Missing {EXECUTOR_SCHEMA_PATH}"
    executor_schema = json.loads(EXECUTOR_SCHEMA_PATH.read_text(encoding="utf-8"))
    assert "action" in executor_schema.get("properties", {}), "Invalid executor schema"
    assert "batch_status" in executor_schema.get("properties", {}), "Invalid executor schema"
    assert "jump_gate" in executor_schema["properties"]["action"]["properties"]["type"]["enum"], "Missing jump_gate in executor schema"

    assert PERSONAS_PATH.exists(), f"Missing {PERSONAS_PATH}"
    personas = json.loads(PERSONAS_PATH.read_text(encoding="utf-8"))
    assert len(personas.get("personas", [])) >= 7, "Expected >= 7 personas"
    for p_item in personas.get("personas", []):
        assert int(p_item.get("rotate_after_steps", 0)) >= 45, f"Persona {p_item.get('id')} rotate_after_steps < 45"

    assert HOOK_SCRIPT_PATH.exists(), f"Missing {HOOK_SCRIPT_PATH}"
    assert SITE_SCRIPT_PATH.exists(), f"Missing {SITE_SCRIPT_PATH}"
    if SCENARIOS_DIR.exists():
        for sp in sorted(SCENARIOS_DIR.glob("*.json")):
            load_scenario_file(sp)

    jwt_tok = mint_sso_jwt()
    assert len(jwt_tok.split(".")) == 3, "JWT minting failed"

    m_flags = _resolve_agy_model_args("gemini-4-carbon-medium")
    assert m_flags == ["--model", "Gemini 4 Carbon (Medium)"], m_flags
    h_flags = _resolve_agy_model_args("hydrogen-l-tf-high")
    assert h_flags == ["--model", "hydrogen-l-tf-high"], h_flags
    exec_flags = _resolve_agy_model_args("gemini-hydrogen-low")
    assert exec_flags == ["--model", "hydrogen-l-tf-medium"], exec_flags
    assert DEFAULT_EXECUTOR_MODEL_CHAIN[0] == "hydrogen-l-tf-medium", DEFAULT_EXECUTOR_MODEL_CHAIN
    assert all("3.8-flash" not in m for m in DEFAULT_MODEL_CHAIN + DEFAULT_EXECUTOR_MODEL_CHAIN)

    import tempfile as _tf
    with _tf.TemporaryDirectory() as td:
        tmp_mem = PlaytesterMemory(Path(td))
        pp = build_planner_prompt(1, Path(td) / "screen.png", [], {"active_view": "hangar"}, {}, tmp_mem, "deadbeef")
        assert "Hybrid Planner-Executor Architecture" in pp
        ep1 = build_executor_prompt(1, 1, 1, _heuristic_fallback_planner_decision()["delegated_plan"], Path(td) / "screen.png", [], {"active_view": "hangar"}, {}, tmp_mem, [])
        assert "Fast Tactical Playtester Executor" in ep1
        ep2 = build_executor_prompt(2, 1, 2, _heuristic_fallback_planner_decision()["delegated_plan"], Path(td) / "screen.png", [], {"active_view": "hangar"}, {}, tmp_mem, [{"batch_step": 1, "action_type": "open_neocom"}])
        assert "Continuation" in ep2
        log_planner_trace(1, DEFAULT_MODEL_CHAIN[0], 1.2, _heuristic_fallback_planner_decision())
        log_executor_trace(1, 1, 3, DEFAULT_EXECUTOR_MODEL_CHAIN[0], 1.7, _heuristic_fallback_executor_decision())

    win = find_exefile_window()
    log_info(
        f"Self-test passed! Worktree={args.worktree} HEAD={get_git_head(Path(args.worktree))} "
        f"PlannerModels={DEFAULT_MODEL_CHAIN} ExecutorModels={DEFAULT_EXECUTOR_MODEL_CHAIN} ActiveExefileWindow={win}"
    )
    return 0


def parse_args(argv: Optional[List[str]] = None) -> argparse.Namespace:
    default_wt = (
        str(DEFAULT_PLAYTESTER_WORKTREE)
        if DEFAULT_PLAYTESTER_WORKTREE.exists()
        else str(REPO_ROOT)
    )
    default_chain_env = os.environ.get(
        "EVOMU_PLAYTESTER_MODEL", ",".join(DEFAULT_MODEL_CHAIN)
    )
    default_exec_chain_env = os.environ.get(
        "EVOMU_PLAYTESTER_EXECUTOR_MODEL", ",".join(DEFAULT_EXECUTOR_MODEL_CHAIN)
    )

    p = argparse.ArgumentParser(
        description="Autonomous Live macOS EVE.app Playtester & Continuous Regression Runner"
    )
    p.add_argument(
        "--worktree",
        default=default_wt,
        help=f"Git worktree for building eve-server and storing persistent play state (default: {default_wt})",
    )
    p.add_argument(
        "--staged-app",
        default=None,
        help="Isolated staged EVE.app bundle path (default: <cache>/client_build_<build>_playtester)",
    )
    p.add_argument(
        "--hybrid",
        dest="hybrid",
        action="store_true",
        default=True,
        help=f"Enable Hybrid Planner ({DEFAULT_MODEL_CHAIN[0]}) + Executor ({DEFAULT_EXECUTOR_MODEL_CHAIN[0]}) architecture (default: True)",
    )
    p.add_argument(
        "--no-hybrid",
        dest="hybrid",
        action="store_false",
        help="Disable Hybrid Planner+Executor mode and run single-model reasoning on every step",
    )
    p.add_argument(
        "--model",
        default=None,
        help="Primary reasoning/planner model override (prepended to --model-chain / --planner-model-chain)",
    )
    p.add_argument(
        "--model-chain",
        default=default_chain_env,
        help=f"Comma-separated agy model fallback chain (default: {default_chain_env})",
    )
    p.add_argument(
        "--planner-model-chain",
        default=default_chain_env,
        help=f"Comma-separated Parent Planner model fallback chain in --hybrid mode (default: {default_chain_env})",
    )
    p.add_argument(
        "--executor-model-chain",
        default=default_exec_chain_env,
        help=f"Comma-separated Fast Executor model fallback chain in --hybrid mode (default: {default_exec_chain_env})",
    )
    p.add_argument(
        "--persona",
        default=None,
        help="Force initial exploration persona ID (e.g. bug_hunter_recent, combat_navigator, skill_and_career_planner)",
    )
    p.add_argument(
        "--continuous",
        action="store_true",
        help="Run continuously until interrupted, rotating personas and auto-reloading on new origin/main commits",
    )
    p.add_argument(
        "--max-steps",
        type=int,
        default=0,
        help="Stop after N reasoning steps (0 = unlimited when --continuous is set, else 1)",
    )
    p.add_argument(
        "--watch-origin-interval",
        type=int,
        default=60,
        help="Seconds between git fetch origin/main checks for automatic server hot-reload (0 = disable)",
    )
    p.add_argument(
        "--no-git-checkout",
        action="store_true",
        help="Do not `git checkout --detach origin/main` during hot-reload (compile current worktree HEAD as-is)",
    )
    p.add_argument(
        "--reset-db",
        action="store_true",
        help="Archive existing harness_world.sqlite and start from a fresh world DB",
    )
    p.add_argument(
        "--foreground",
        action="store_true",
        default=False,
        help="Allow bringing EVE.app to foreground for screenshots and input (default: False so EVE.app can run on a separate macOS Desktop Space without stealing focus)",
    )
    p.add_argument(
        "--no-foreground",
        dest="foreground",
        action="store_false",
        help="Do not bring EVE.app window to foreground before screenshots (default)",
    )
    p.add_argument(
        "--silent",
        action="store_true",
        default=False,
        help="Pass /silent to exefile to suppress 3D Trinity rendering (headless mode; screenshots will be blank)",
    )
    p.add_argument(
        "--os-input-fallback",
        action="store_true",
        default=False,
        help="Enable Quartz OS mouse click fallback for coordinate actions (default: False; in-process uicore dispatch is used instead so cursor focus is never stolen)",
    )
    p.add_argument(
        "--no-github-sync",
        action="store_true",
        help="Write incident bundles to .evomu-bugs/queue/ without running sync_bugs_to_github.py",
    )
    p.add_argument(
        "--window-size",
        default="full",
        help="EVE.app windowed size: 'full' (fill usable screen, default) or 'WxH' physical pixels (e.g. '1280x800')",
    )
    p.add_argument(
        "--ui-scale",
        type=float,
        default=0.0,
        help="EVE.app UI scale override (0 = auto-match Retina display up to 2.0 when full-size, 1.0 otherwise)",
    )
    p.add_argument(
        "--screenshot-max-dim",
        type=int,
        default=1024,
        help="Downscale agent screenshots so max(width, height) <= N pixels while keeping the window full-size (default: 1024)",
    )
    p.add_argument(
        "--agy-timeout",
        type=int,
        default=120,
        help="Timeout in seconds per agy CLI model call before falling back to the next model",
    )
    p.add_argument("--macho-port", type=int, default=DEFAULT_Ports["macho"])
    p.add_argument("--bridge-port", type=int, default=DEFAULT_Ports["bridge"])
    p.add_argument("--http-port", type=int, default=DEFAULT_Ports["http"])
    p.add_argument("--gateway-port", type=int, default=DEFAULT_Ports["gateway"])
    p.add_argument("--xmpp-port", type=int, default=DEFAULT_Ports["xmpp"])
    p.add_argument("--rpc-port", type=int, default=DEFAULT_Ports["rpc"])
    p.add_argument(
        "--scenario",
        action="append",
        default=[],
        dest="scenarios",
        metavar="FILE",
        help="Run zero-LLM scripted UI scenario JSON file (repeatable)",
    )
    p.add_argument(
        "--out",
        default=None,
        dest="out_dir",
        metavar="DIR",
        help="Output directory for --scenario runs (writes DIR/result.json and failure artifacts)",
    )
    p.add_argument(
        "--ping-reload",
        action="store_true",
        help="Signal a running playtester daemon to recompile and hot-reload eve-server immediately",
    )
    p.add_argument(
        "--self-test",
        action="store_true",
        help="Run offline validation of schema, personas, hook, and model fallback chain",
    )
    args = p.parse_args(argv)
    if args.staged_app is None:
        args.staged_app = str(resolve_default_staged_app())
    return args


def main(argv: Optional[List[str]] = None) -> int:
    args = parse_args(argv)
    if args.ping_reload:
        return send_ping_reload()
    if args.self_test:
        return run_self_test(args)
    if getattr(args, "scenarios", None):
        return ScenarioRunner(args).run_scenarios()
    runner = PlaytesterRunner(args)
    return runner.run()


if __name__ == "__main__":
    sys.exit(main())
