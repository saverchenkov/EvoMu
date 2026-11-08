#!/usr/bin/env python3
"""
EvoMu Autonomous Live EVE.app Playtester & Continuous Regression Daemon.

Inspired by Anthropic's Project EVE (Claude Plays MMORPGs), adapted for EvoMu's
macOS V24.01 EVE.app (exefile) client + Rust eve-server:

1. Isolated Playtester Worktree & Staged Client:
   - Runs in `/Users/sergeya/ai/EvoMu-playtester` (or repo root containing this script)
   - Stages EVE.app into `~/.cache/evomu/client_build_3624425_playtester` so it never
     collides with `run_journeys` hourly regressions.
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
     `gemini-flash-next` -> `gemini-pro-next` -> `gemini-4-carbon-medium`
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
   - Obeys the workspace Cargo concurrency cap (`<= 2` active `cargo` processes, `-j 2`).

5. Zero-Tolerance Crash & Visual/Protocol Oracle:
   - Automatically detects Python tracebacks (`client_tracebacks.log`), modal error dialogs,
     Quasar/MachoNet timeouts, missing warp tunnel animations (`ball.mode == 3` / `effects.Warping`),
     `eve-server` ERROR/PANIC logs, and `agy`-diagnosed visual/functional bugs.
   - Emits deduplicated incident bundles to `.evomu-bugs/queue/` and syncs to GitHub via
     `tools/sync_bugs_to_github.py`.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple


HARNESS_DIR = Path(__file__).resolve().parent
REPO_ROOT = HARNESS_DIR.parent.parent
DEFAULT_PLAYTESTER_WORKTREE = Path("/Users/sergeya/ai/EvoMu-playtester")

ACTION_SCHEMA_PATH = HARNESS_DIR / "playtester_action_schema.json"
PERSONAS_PATH = HARNESS_DIR / "playtester_personas.json"
HOOK_SCRIPT_PATH = HARNESS_DIR / "evomu_playtester_hook.py"
SITE_SCRIPT_PATH = HARNESS_DIR / "evomu_site.py"
HELPERS_SCRIPT_PATH = HARNESS_DIR / "evomu_inclient_helpers.py"

DEFAULT_CACHE_ROOT = Path(
    os.environ.get("EVOMU_CLIENT_CACHE", os.path.expanduser("~/.cache/evomu"))
)
DEFAULT_STAGED_APP = (
    DEFAULT_CACHE_ROOT / "client_build_3624425_playtester"
)
SOURCE_STAGED_APP = DEFAULT_CACHE_ROOT / "client_build_3624425"
SYSTEM_EVE_BUILD_DIR = Path(
    os.path.expanduser(
        "~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build"
    )
)
WINDOW_INFO_HELPER_BIN = DEFAULT_CACHE_ROOT / "evomu_window_info"

DEFAULT_PID_FILE = Path("/tmp/evomu_playtester.pid")
DEFAULT_CONTROL_FILE = Path("/tmp/evomu_playtester.control")

DEFAULT_MODEL_CHAIN = [
    "gemini-flash-next",
    "gemini-pro-next",
    "gemini-4-carbon-medium",
]

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


def log_info(msg: str) -> None:
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] [playtester] {msg}", flush=True)


def log_warn(msg: str) -> None:
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] [playtester] WARN: {msg}", file=sys.stderr, flush=True)


def log_err(msg: str) -> None:
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] [playtester] ERROR: {msg}", file=sys.stderr, flush=True)


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
        swift_src = DEFAULT_CACHE_ROOT / "evomu_window_info.swift"
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


def find_exefile_window(pid: Optional[int] = None) -> Optional[Dict[str, Any]]:
    """
    Locate the live EVE.app (`exefile`) window across ALL macOS Mission Control Desktop Spaces
    and displays via CoreGraphics `CGWindowListCopyWindowInfo`.
    """
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
                    return data
        except Exception:
            pass
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


def _downscale_screenshot_if_needed(dest_path: Path, max_dim: int = 1024) -> None:
    """Downscale Retina screenshots to `max_dim` width (e.g. 1024x640) so vision context tokens aren't wasted."""
    try:
        if dest_path.exists() and dest_path.stat().st_size > 250_000:
            subprocess.run(
                ["sips", "-Z", str(int(max_dim)), str(dest_path)],
                capture_output=True,
                timeout=4,
            )
    except Exception:
        pass


def capture_window_screenshot(
    dest_path: Path,
    client_pid: Optional[int] = None,
    bring_foreground: bool = False,
    rpc_client: Optional[Any] = None,
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
                _downscale_screenshot_if_needed(dest_path, max_dim=1024)
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
                _downscale_screenshot_if_needed(dest_path, max_dim=1024)
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
            _downscale_screenshot_if_needed(dest_path, max_dim=1024)
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


def check_origin_main_update(repo_dir: Path) -> Optional[str]:
    """Fetch origin/main, advance HEAD + crates/ to origin/main (preserving tools/client-harness/*), and return new short SHA."""
    try:
        subprocess.run(
            ["git", "-C", str(repo_dir), "fetch", "origin", "main"],
            capture_output=True,
            text=True,
            timeout=25,
        )
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
        if head and origin and head != origin:
            # Only hot-reload if origin/main is NOT already an ancestor of HEAD
            anc = subprocess.run(
                ["git", "-C", str(repo_dir), "merge-base", "--is-ancestor", origin, head],
                capture_output=True,
                timeout=5,
            )
            if anc.returncode != 0:
                log_info(f"Advancing worktree HEAD from {head[:9]} to origin/main ({origin[:9]}) (preserving tools/client-harness/*)...")
                subprocess.run(
                    ["git", "-C", str(repo_dir), "reset", "--soft", "origin/main"],
                    capture_output=True,
                    timeout=10,
                )
                subprocess.run(
                    ["git", "-C", str(repo_dir), "checkout", "origin/main", "--", "crates", "Cargo.toml", "Cargo.lock"],
                    capture_output=True,
                    timeout=15,
                )
                return origin[:9]
    except Exception as exc:
        log_warn(f"git fetch origin/main check failed: {exc}")
    return None


def compile_eve_server(worktree_dir: Path) -> Tuple[bool, Path, str]:
    """Compile `target/debug/eve-server` obeying the <= 2 cargo concurrency rule (skipping rebuild if HEAD is unchanged)."""
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

    wait_for_cargo_slot(max_concurrent=2)
    log_info(f"Building eve-server in {worktree_dir} (cargo build -j 2 -p eve-server)...")
    t0 = time.time()
    proc = subprocess.run(
        ["cargo", "build", "-j", "2", "-p", "eve-server"],
        cwd=str(worktree_dir),
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
    staged_app: Path,
    inclient_cfg_path: Path,
    hook_port: int,
) -> Path:
    """
    Ensure `~/.cache/evomu/client_build_3624425_playtester` exists (with `bin64/exefile`)
    and has the playtester's `evomu_py_site/sitecustomize.py` + `tools/startup/evomu_playtester.py`
    installed without colliding with `run_journeys`.
    """
    # Normalize if user passed `.../EVE.app` vs build dir
    stage_dir = staged_app
    if stage_dir.name == "EVE.app" and (stage_dir / "Contents" / "Resources" / "build").exists():
        stage_dir = stage_dir / "Contents" / "Resources" / "build"
    elif stage_dir.name == "EVE.app" and not stage_dir.exists():
        stage_dir = stage_dir.parent

    exefile = stage_dir / "bin64" / "exefile"
    if not exefile.exists():
        src_dir = SOURCE_STAGED_APP
        if not (src_dir / "bin64" / "exefile").exists():
            src_dir = SYSTEM_EVE_BUILD_DIR
        if not (src_dir / "bin64" / "exefile").exists():
            raise RuntimeError(
                f"Staged client template not found at {SOURCE_STAGED_APP} or {SYSTEM_EVE_BUILD_DIR}."
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

    # Pre-seed EVE client `core_public__.yaml` to launch in compact Windowed Mode (1280x800, 1.0 UI scale)
    # so EVE.app does not open in 3456x2160 Retina fullscreen.
    try:
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
  - {{adapter: 0, height: 800, presentInterval: 1, width: 1280}}
  UIScaleFullscreen: [{ts_ft}, 1.0]
  UIScaleFullscreenSetAutomatically: [{ts_ft}, false]
  UIScaleWindowed: [{ts_ft}, 1.0]
  UIScaleWindowedSetAutomatically: [{ts_ft}, false]
  WindowMode: [{ts_ft}, 1]
  WindowedResolution: [{ts_ft}, null]
  WindowedSettings:
  - {ts_ft}
  - {{adapter: 0, height: 800, left: 40, presentInterval: 1, showState: 1, top: 40, width: 1280}}
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

class PlaytesterMemory:
    def __init__(self, state_dir: Path):
        self.state_dir = state_dir
        self.memory_path = state_dir / "memory.json"
        self.personas_cfg = self._load_personas_cfg()
        self.data = self._load_or_init()

    def _load_personas_cfg(self) -> Dict[str, Any]:
        if PERSONAS_PATH.exists():
            try:
                return json.loads(PERSONAS_PATH.read_text(encoding="utf-8"))
            except Exception:
                pass
        return {"personas": [], "initial_coverage_frontier": []}

    def _load_or_init(self) -> Dict[str, Any]:
        if self.memory_path.exists():
            try:
                return json.loads(self.memory_path.read_text(encoding="utf-8"))
            except Exception as exc:
                log_warn(f"Failed to parse {self.memory_path}, re-initializing: {exc}")

        personas = self.personas_cfg.get("personas") or []
        first_persona = personas[0] if personas else {
            "id": "open_explorer",
            "title": "Open-Ended Capsuleer Explorer",
            "rotate_after_steps": 30,
            "goals": ["Explore all station windows, skill plans, PLEX store, undock, and warp."],
        }
        return {
            "created_at": utc_now_iso(),
            "updated_at": utc_now_iso(),
            "total_steps": 0,
            "server_reloads": 0,
            "active_persona_index": 0,
            "active_persona_steps": 0,
            "active_persona": first_persona,
            "current_goal": first_persona.get("goals", ["Explore the game"])[0],
            "working_memory": [
                "Fresh playtester memory initialized. Priority: verify recently reported bugs (#527-#531: skill training, PLEX store, structure cosmetics, warp animation) and explore core gameplay loops."
            ],
            "long_term_notes": [],
            "coverage_frontier": self.personas_cfg.get("initial_coverage_frontier") or [],
            "recent_actions": [],
            "filed_incident_fingerprints": [],
        }

    def save(self) -> None:
        self.data["updated_at"] = utc_now_iso()
        self.state_dir.mkdir(parents=True, exist_ok=True)
        tmp = self.memory_path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(self.data, indent=2), encoding="utf-8")
        tmp.replace(self.memory_path)

    def maybe_rotate_persona(self, force_id: Optional[str] = None) -> None:
        personas = self.personas_cfg.get("personas") or []
        if not personas:
            return
        if force_id:
            for idx, p in enumerate(personas):
                if p.get("id") == force_id:
                    self.data["active_persona_index"] = idx
                    self.data["active_persona"] = p
                    self.data["active_persona_steps"] = 0
                    goals = p.get("goals") or ["Explore EVE Online systems"]
                    self.data["current_goal"] = goals[0]
                    self.save()
                    return

        active = self.data.get("active_persona") or personas[0]
        limit = int(active.get("rotate_after_steps", 25))
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

    def apply_agent_decision(
        self,
        step_num: int,
        decision: Dict[str, Any],
        action_outcome: Dict[str, Any],
    ) -> None:
        self.data["total_steps"] = int(self.data.get("total_steps", 0)) + 1
        self.data["active_persona_steps"] = int(self.data.get("active_persona_steps", 0)) + 1

        if decision.get("current_goal"):
            self.data["current_goal"] = str(decision["current_goal"])[:300]

        mem_upd = decision.get("memory_update") or {}
        if isinstance(mem_upd.get("working_memory"), list) and mem_upd["working_memory"]:
            self.data["working_memory"] = [str(x)[:300] for x in mem_upd["working_memory"][-12:]]

        note = str(mem_upd.get("long_term_note_to_append") or "").strip()
        if note:
            notes = self.data.setdefault("long_term_notes", [])
            notes.append(f"[step {step_num}] {note[:400]}")
            self.data["long_term_notes"] = notes[-60:]

        # Update coverage frontier items
        frontier = self.data.setdefault("coverage_frontier", [])
        by_feat = {item.get("feature"): item for item in frontier if isinstance(item, dict)}
        for feat in mem_upd.get("mark_tested_features") or []:
            if feat in by_feat and by_feat[feat].get("status") != "buggy":
                by_feat[feat]["status"] = "verified"
                by_feat[feat]["verified_at_step"] = step_num
            elif feat not in by_feat:
                frontier.append({"feature": str(feat), "status": "verified", "verified_at_step": step_num})

        for feat in mem_upd.get("mark_buggy_features") or []:
            if feat in by_feat:
                by_feat[feat]["status"] = "buggy"
                by_feat[feat]["buggy_at_step"] = step_num
            else:
                frontier.append({"feature": str(feat), "status": "buggy", "buggy_at_step": step_num})

        for feat in mem_upd.get("new_frontier_features") or []:
            if feat and feat not in by_feat:
                frontier.append({"feature": str(feat), "status": "untested", "added_at_step": step_num})

        act = decision.get("action") or {}
        recent = self.data.setdefault("recent_actions", [])
        recent.append(
            {
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
            }
        )
        self.data["recent_actions"] = recent[-20:]
        self.maybe_rotate_persona()
        self.save()


# ---------------------------------------------------------------------------
# `agy` CLI Reasoning Invoker with Model Fallback Chain
# ---------------------------------------------------------------------------

def _resolve_agy_model_args(model_name: str) -> List[str]:
    """Translate user model alias (e.g. `gemini-4-carbon-medium`) into `agy` CLI flags."""
    m = model_name.strip()
    alias_map = {
        "gemini-pro-next": "Gemini Pro Next",
        "gemini-4-carbon-medium": "Gemini 4 Carbon (Medium)",
        "gemini-4-carbon-low": "Gemini 4 Carbon (Low)",
        "gemini-4-carbon-high": "Gemini 4 Carbon (High)",
    }
    return ["--model", alias_map.get(m, m)]


def build_reasoning_prompt(
    step_num: int,
    screenshot_path: Path,
    burst_paths: List[Path],
    ui_snapshot: Dict[str, Any],
    last_flight_recorder: Dict[str, Any],
    memory: PlaytesterMemory,
    git_head: str,
) -> str:
    mem = memory.data
    persona = mem.get("active_persona") or {}
    untested = [
        x for x in (mem.get("coverage_frontier") or [])
        if x.get("status") in ("untested", "buggy")
    ][:12]
    verified = [
        x.get("feature") for x in (mem.get("coverage_frontier") or [])
        if x.get("status") == "verified"
    ][-15:]
    recent_actions = (mem.get("recent_actions") or [])[-8:]
    prev_action_entry = recent_actions[-1] if recent_actions else None
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
    compact_widgets_lines = ",\n    ".join(
        json.dumps(w, separators=(",", ":")) for w in raw_widgets[:85]
    )

    prompt = f"""You are the Autonomous EvoMu Live Client Playtester playing the real macOS EVE Online V24.01 client (`EVE.app`) connected to `eve-server` (commit `{git_head}`).
Your character has full **GM / QA / Admin role access** (`0x7FFFFFFFFFFFFFFF`) so you can both interact with the UI like a player AND invoke GM timer-acceleration commands to test multi-hour gameplay loops (skill training, Upwell structure timers, industry jobs, cooldowns) immediately without waiting real-time.

### Step {step_num} Perception Bundle
- **Current Live Window Screenshot:** `{screenshot_path}`
  (Only read `{screenshot_path}` if you need visual confirmation of 3D space graphics or warp effects; the complete `uicore` scene graph is already provided below, so do NOT waste tool calls reading `ui_state.json`.)
{burst_section}
### Active Exploration Persona & Goal
- **Persona:** `[{persona.get('id', 'explorer')}]` {persona.get('title', 'Explorer')} (step {mem.get('active_persona_steps', 0)}/{persona.get('rotate_after_steps', 25)} before rotation)
- **Persona Directives:** {json.dumps(persona.get('goals', []))}
- **Current Goal:** {mem.get('current_goal')}

### Persistent Working Memory
{json.dumps(mem.get('working_memory', []), indent=2)}

### Long-Term Session Notes (Most Recent)
{json.dumps((mem.get('long_term_notes') or [])[-12:], indent=2)}

### Coverage Frontier (Priority Untested / Buggy Features to Probe)
{json.dumps(untested, indent=2)}

### Already Verified Features
{json.dumps(verified)}

### Previous Action Expectation vs. Actual State Diff (CRITICAL FOR UI-STATE DESYNC DETECTION)
- **Previous Action & Expected Change:**
```json
{json.dumps(prev_action_entry, indent=2)}
```
- **Previous Action Flight-Recorder, `state_diff`, & `scatters_observed`:**
```json
{json.dumps(last_flight_recorder, indent=2)}
```

### Recent Actions History (Avoid repeating stuck loops!)
[
  {compact_recent_lines}
]

### Live `uicore` Scene-Graph & Session Snapshot
```json
{json.dumps(compact_snap, indent=2)}
```
### Clickable Widgets ({len(raw_widgets)} total, showing top {min(len(raw_widgets), 85)})
```json
[
    {compact_widgets_lines}
]
```

### Expected EVE Online Mechanics & UI-State Reference Contract
Use these ground-truth EVE Online V24.01 expectations to judge whether the game and UI updated properly after your previous action (even when no Python exception was raised!):
1. **Skill Training & Skill Queue (`skills`, `skill_plans`, `train_skill`, `complete_skills`):**
   - Adding/starting a skill (`train_skill` or clicking Train in UI) MUST emit `OnNewSkillQueueSaved` / `OnSkillQueueChanged` and increase `skills_state.queue_len > 0` in the live snapshot and Skill Queue UI bar.
   - Running `complete_skills` (`/completeskills all`) MUST immediately finish queued skills without waiting real-time, emit `OnSkillTrained` + `OnServerSkillsChanged` + `OnNewSkillQueueSaved`, increase `skills_state.total_sp`, and update the trained skill level in the Character Sheet / Skills window live without requiring a relog.
   - If `queue_len` or `total_sp` or the Skills window UI fails to update after `train_skill` or `complete_skills`, set `previous_action_verification.expectation_met = false` and file a `ui_state_desync` bug!
2. **Upwell Structures & Timers (`advance_upwell_timer` / `/upwelltimer`):**
   - Upwell structures transition through `AnchorVulnerable -> Anchoring -> NoCore -> Onlining -> ShieldVulnerable`. Running `advance_upwell_timer` (`/upwelltimer`) fast-forwards the active timer so you never wait 15m/24h real-time. The structure state and bracket/overview must reflect the new state immediately.
3. **Industry Jobs (`complete_industry_jobs` / `/completeindustry`):**
   - After starting an industry job, running `complete_industry_jobs` (`/completeindustry`) fast-forwards `start_unix` so the job is immediately ready for delivery in the Industry window.
4. **Ship Fitting & Inventory (`fitting`, `inventory`, `/fit`, `/createitem`):**
   - Fitting a module or creating an item MUST emit `OnItemChange` and update the Fitting window slots / Ship HUD module rack / Hangar inventory list live.
5. **Market & Wallet (`market`, `wallet`, `/giveisk`):**
   - Buying an item or running `/giveisk` MUST emit `OnAccountChange`, update `wallet_state.isk_balance` in `state_diff`, and deliver purchased items to the hangar.
6. **Navigation & Real-Time Warp Animation (`undock`, `dock`, `warp_to`, `approach`, `orbit`):**
   - `undock` MUST transition `active_view` from `hangar` to `inflight` (`space.in_space == true`) and populate `space.nearby_balls`.
   - `warp_to` MUST enter `ball.mode == 3` (`WARP`), activate `effects.Warping` tunnel FX (`warp_telemetry.saw_mode3 == true` and `saw_fx_warping == true`), and move the ship to the destination.
   - `approach` / `orbit` MUST transition `space.ego.mode` to `1` (`FOLLOW`) or `4` (`ORBIT`) and increase `speed_mps > 0`.
7. **Quasar Windows (`open_plex_store`, `skill_plans`, `open_structure_cosmetics`, `agency`, `corporation`):**
   - Opening these windows MUST populate their UI content without `TimeoutException`, `SkillPlanConnectionError`, or permanently blank/spinning panels.

### Action Catalog & Instructions
Choose your next action in the `action` object according to the required JSON schema:
1. `click_widget`: Click a widget by `target` (matching `name`, `path`, or `label` from `open_windows`, `context_menu`, `modals`, or `clickable_widgets`) or fallback coordinates `x`, `y`.
2. `open_neocom`: Open a Neocom window by `target` (`character_sheet`, `skills`, `skill_plans`, `inventory`, `fitting`, `market`, `plex_store`, `new_eden_store`, `agency`, `corporation`, `industry`, `contracts`, `mail`, `wallet`, `map`, `assets`, `notepad`).
3. `close_window`: Close a window or modal by `target` (`window_id` or `name`, or empty to close modals).
4. `undock` / `dock`: Undock into space or dock at a station (`target` optional station item_id).
5. `warp_to` / `approach` / `orbit` / `keep_at_range` / `lock_target` / `activate_module`: In-space navigation and combat commands.
6. `train_skill`: Inject and queue a skill (`target` = skill typeID, e.g. `"3300"`, `"3301"`, `"3327"`).
7. **GM Timer-Acceleration Actions (Use these to test multi-hour timers immediately without waiting real-time!):**
   - `complete_skills`: Immediately finishes active skill training (`target` = `"all"` or seconds like `"3600"`).
   - `advance_upwell_timer`: Fast-forwards Upwell structure anchoring/onlining/reinforcement/unanchoring timers in the solar system (`target` = optional `structureID`).
   - `complete_industry_jobs`: Fast-forwards active manufacturing/research industry jobs to completion (`target` = optional `jobID`).
   - `advance_server_time`: Advances virtual server clock by `target` seconds (default `"3600"`), advancing all server lifecycle timers.
   - `clear_timers`: Clears session-change cooldown, jump fatigue/cooldown, clone jump timer, and crimewatch timers.
   - `slash_command`: Run any GM slash command (`command` = e.g. `"/giveisk 100000000"`, `"/createitem 587 1"`, `"/giveskill 3300 5"`, `"/spawn 1201"`, `"/moveme"`).
8. `open_plex_store` / `open_structure_cosmetics`: Direct probes for `#529` and `#527`.
9. `press_key` / `type_text` / `click_xy` / `drag_widget` / `wait`: Direct UI manipulation.

### Bug Detection Rule (Exceptions AND Silent UI-State Desyncs!)
1. First fill out `previous_action_verification`:
   - Compare the previous action's `expected_ui_and_state_change` against `state_diff`, `scatters_observed`, `ui_snapshot`, and `screen.png`.
   - If the previous action should have changed the UI or game state (e.g., opened a window, switched a tab, queued/completed a skill, updated wallet/inventory, changed ship movement mode, or closed a modal) and the UI state **did not update properly**, set `previous_action_verification.expectation_met = false` and describe the exact discrepancy in `discrepancy_summary`.
2. Set `bug_report.should_file = true` whenever ANY of the following holds:
   - `previous_action_verification.expectation_met == false` (use `category: "ui_state_desync"` or `"broken_game_mechanic"` or `"blank_or_stuck_window"`).
   - `modals` contains an error/timeout dialog (`SkillPlanConnectionError`, `TimeoutException`, `CustomError`, etc.).
   - `Previous Action Flight-Recorder` contains `log_exceptions`, `quasar_errors`, `macho_exceptions`, or failed warp animation (`saw_mode3 == false` and `saw_fx_warping == false` after `warp_to`).
"""
    return prompt


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
    for model_name in model_chain:
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
                    log_info(
                        f"agy ({model_name}) decided in {elapsed:.1f}s -> "
                        f"{decision['action'].get('type')} ({decision['action'].get('target') or decision['action'].get('command') or ''})"
                    )
                    return decision, model_name
            last_err = f"model={model_name} rc={proc.returncode} stderr={proc.stderr[-300:] if proc.stderr else ''}"
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


def _extract_json_decision(raw_out: str) -> Optional[Dict[str, Any]]:
    """Parse `agy --output-format json` wrapper or direct JSON object."""
    try:
        parsed = json.loads(raw_out)
        if isinstance(parsed, dict):
            # Some CLI versions wrap structured output under "result" or "response" or "structured_output"
            if "action" in parsed and "observation" in parsed:
                return parsed
            for key in ("structured_output", "result", "response", "output", "content"):
                inner = parsed.get(key)
                if isinstance(inner, dict) and "action" in inner:
                    return inner
                if isinstance(inner, str):
                    try:
                        inner_obj = json.loads(inner)
                        if isinstance(inner_obj, dict) and "action" in inner_obj:
                            return inner_obj
                    except Exception:
                        pass
    except Exception:
        pass

    # Try extracting the outermost JSON object containing `"action"`
    start = raw_out.find("{")
    end = raw_out.rfind("}")
    if start != -1 and end > start:
        try:
            obj = json.loads(raw_out[start : end + 1])
            if isinstance(obj, dict) and "action" in obj:
                return obj
        except Exception:
            pass
    return None


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


# ---------------------------------------------------------------------------
# Zero-Tolerance Bug Oracle & Incident Bundle Emitter
# ---------------------------------------------------------------------------

class PlaytesterBugOracle:
    def __init__(
        self,
        worktree_dir: Path,
        bug_queue_dir: Path,
        memory: PlaytesterMemory,
        sync_github: bool = True,
    ):
        self.worktree_dir = worktree_dir
        self.bug_queue_dir = bug_queue_dir
        self.memory = memory
        self.sync_github = sync_github
        self._traceback_offset = 0
        self._server_log_offset = 0

    def reset_log_offsets(self, traceback_log: Path, server_log: Path) -> None:
        self._traceback_offset = traceback_log.stat().st_size if traceback_log.exists() else 0
        self._server_log_offset = server_log.stat().st_size if server_log.exists() else 0

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
        warp_telemetry = exec_res.get("warp_telemetry") if isinstance(exec_res, dict) else None

        incidents: List[Dict[str, Any]] = []

        # 1. Hard client Python traceback
        if new_tb or fr_log_excs:
            first_headline = ""
            if fr_log_excs:
                first_headline = str(fr_log_excs[0].get("headline") or "")
            if not first_headline and new_tb:
                lines = [ln.strip() for ln in new_tb.splitlines() if ln.strip()]
                first_headline = lines[-1] if lines else "Unhandled client Python exception"
            incidents.append(
                {
                    "kind": "client_traceback",
                    "title": f"[Playtester] Client exception during {decision.get('action', {}).get('type')}: {first_headline[:90]}",
                    "severity": "high",
                    "subsystem": "client-python",
                    "summary": first_headline,
                    "traceback": new_tb or json.dumps(fr_log_excs, indent=2),
                }
            )

        # 2. Quasar / MachoNet timeout or error
        if fr_quasar_errs or fr_macho_excs:
            sample = (fr_quasar_errs + fr_macho_excs)[0]
            incidents.append(
                {
                    "kind": "protocol_error",
                    "title": f"[Playtester] Protocol/Quasar error on {decision.get('action', {}).get('type')}: {str(sample)[:90]}",
                    "severity": "high",
                    "subsystem": "quasar-macho",
                    "summary": json.dumps(sample),
                    "traceback": new_tb,
                }
            )

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
                }
            )

        # 5. Agent-diagnosed bug (`bug_report.should_file`) OR failed previous_action_verification (`expectation_met == False`)
        br = decision.get("bug_report") or {}
        pav = decision.get("previous_action_verification") or {}
        if isinstance(br, dict) and br.get("should_file") and br.get("title"):
            cat = str(br.get("category") or "agent_diagnosed_bug")
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
                    "traceback": new_tb,
                }
            )
        elif isinstance(pav, dict) and pav.get("expectation_met") is False and str(pav.get("discrepancy_summary") or "").strip():
            disc = str(pav.get("discrepancy_summary")).strip()
            incidents.append(
                {
                    "kind": "ui_state_desync",
                    "title": f"[Playtester] [UI State Desync] {disc[:90]}",
                    "severity": "medium",
                    "subsystem": "client-ui-state",
                    "summary": f"Previous action expectation failed without exception:\n{disc}",
                    "traceback": new_tb,
                }
            )

        created_files: List[Path] = []
        for inc in incidents:
            fp_src = f"{inc['kind']}:{inc['subsystem']}:{inc['title'][:70]}"
            fingerprint = hashlib.sha256(fp_src.encode("utf-8")).hexdigest()[:16]
            known = self.memory.data.setdefault("filed_incident_fingerprints", [])
            if fingerprint in known:
                log_info(f"Skipping duplicate incident ({fingerprint}): {inc['title']}")
                continue
            known.append(fingerprint)
            self.memory.save()

            self.bug_queue_dir.mkdir(parents=True, exist_ok=True)
            bundle_path = self.bug_queue_dir / f"playtester_step{step_num:04d}_{fingerprint}.json"
            fp_u64 = int(fingerprint, 16)
            fp_i64 = fp_u64 - (1 << 64) if fp_u64 >= (1 << 63) else fp_u64
            clean_title = inc["title"].replace("[Playtester]", "").strip()
            sess_raw = ui_snapshot.get("session") or {}
            fr_raw = (exec_res.get("flight_recorder") or {}) if isinstance(exec_res, dict) else {}
            fr_calls = []
            for mc in fr_raw.get("macho_calls") or []:
                if isinstance(mc, dict):
                    fr_calls.append(
                        {
                            "service": mc.get("service", "-"),
                            "method": mc.get("method", "-"),
                            "args_summary": mc.get("args", "()"),
                            "status": f"{mc.get('status', 'OK')}({mc.get('error') or mc.get('result') or ''})",
                            "duration_micros": int(float(mc.get("duration_ms", 0) or 0) * 1000),
                        }
                    )
            bundle = {
                "fingerprint": fingerprint,
                "stack_hash": fp_i64,
                "stack_key": f"playtester:{inc['subsystem']}:{inc['kind']}",
                "trace": inc["traceback"] or "",
                "report_source": "f12_manual",
                "manual_report": {
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
                },
                "flight_recorder": fr_calls,
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
                    "ship_type_id": sess_raw.get("shipid"),
                    **sess_raw,
                },
                "active_view": ui_snapshot.get("active_view"),
                "modals": ui_snapshot.get("modals"),
            }
            bundle_path.write_text(json.dumps(bundle, indent=2), encoding="utf-8")
            created_files.append(bundle_path)
            log_warn(f"Recorded incident bundle -> {bundle_path.name}: {inc['title']}")

        if created_files and self.sync_github:
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

        return created_files


# ---------------------------------------------------------------------------
# Main Continuous Playtester Supervisor
# ---------------------------------------------------------------------------

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

        self.server_proc: Optional[subprocess.Popen] = None
        self.client_proc: Optional[subprocess.Popen] = None
        self.rpc = InClientRpcBridge(port=self.ports["rpc"])
        self.reload_requested = False
        self.stop_requested = False

        self.memory = PlaytesterMemory(self.runtime_dir)
        if args.persona:
            self.memory.maybe_rotate_persona(force_id=args.persona)

        self.bug_queue_dir = self.worktree_dir / ".evomu-bugs" / "queue"
        self.oracle = PlaytesterBugOracle(
            worktree_dir=self.worktree_dir,
            bug_queue_dir=self.bug_queue_dir,
            memory=self.memory,
            sync_github=not args.no_github_sync,
        )

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
                    self.stop_requested = True
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
        sde_candidates = [
            Path(os.environ.get("EVOMU_SDE_PATH", "")) if os.environ.get("EVOMU_SDE_PATH") else None,
            self.worktree_dir / "data" / "evomu_sde.sqlite",
            Path("/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite"),
        ]
        sde_path = next((p for p in sde_candidates if p and p.exists()), self.worktree_dir / "data" / "evomu_sde.sqlite")

        qd_candidates = [
            Path(os.environ.get("EVOMU_QUASAR_DESCRIPTORS_PATH", "")) if os.environ.get("EVOMU_QUASAR_DESCRIPTORS_PATH") else None,
            self.worktree_dir / "data" / "quasar_descriptors.sqlite",
            Path("/Users/sergeya/ai/EvoMu/data/quasar_descriptors.sqlite"),
        ]
        qd_path = next((p for p in qd_candidates if p and p.exists()), self.worktree_dir / "data" / "quasar_descriptors.sqlite")
        return sde_path, qd_path

    def write_server_config(self) -> Path:
        self.world_dir.mkdir(parents=True, exist_ok=True)
        self.current_run_dir.mkdir(parents=True, exist_ok=True)
        cfg_path = self.runtime_dir / "evomu_playtester_server.toml"
        sde_path, _ = self._resolve_sde_and_quasar_paths()
        cfg_text = f"""bind_address = "127.0.0.1"
macho_port = {self.ports['macho']}
bridge_port = {self.ports['bridge']}
esi_port = {self.ports['http']}
quasar_grpc_port = {self.ports['gateway']}
sde_path = "{sde_path}"
world_db_path = "{self.world_db_path}"

[feature_flags]
enable_esi = true
enable_quasar_read = true
enable_ti_di = true

[[player_accounts]]
username = "demo"
password_token = "demo-secret"
character_id = 90000001
character_name = "Demo Pilot"
corporation_id = 1000009
alliance_id = 0
solar_system_id = 30000142
station_id = 60003760
ship_type_id = 606
isk_balance = 125000000.0

[[admin_accounts]]
username = "demo"
password_token = "demo-secret"
character_id = 90000001
role_mask = 9223372036854775807
"""
        cfg_path.write_text(cfg_text, encoding="utf-8")
        return cfg_path

    def write_inclient_cfg(self) -> Path:
        self.current_run_dir.mkdir(parents=True, exist_ok=True)
        cfg_path = self.current_run_dir / "inclient_cfg.json"
        payload = {
            "mode": "playtester",
            "playtester_port": self.ports["rpc"],
            "hook_port": self.ports["rpc"],
            "user": "demo",
            "password": "demo-secret",
            "char_id": 90000001,
            "server": f"127.0.0.1:{self.ports['macho']}",
            "http_port": self.ports["http"],
            "gateway_port": self.ports["gateway"],
            "xmpp_port": self.ports["xmpp"],
            "run_dir": str(self.current_run_dir),
            "out_dir": str(self.current_run_dir),
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
            sup_root = Path("/Users/sergeya/ai/EvoMu-supervisor")
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

        log_info(
            f"Launching eve-server ({get_git_head(self.worktree_dir)}) on "
            f"Macho={self.ports['macho']} ESI/HTTP={self.ports['http']} Quasar={self.ports['gateway']}"
        )
        self.server_proc = subprocess.Popen(
            [str(binary)],
            cwd=str(self.worktree_dir),
            stdout=log_f,
            stderr=subprocess.STDOUT,
            env=env,
        )

        if (
            self.wait_for_port(self.ports["macho"], timeout=25.0, proc=self.server_proc)
            and self.wait_for_port(self.ports["http"], timeout=10.0, proc=self.server_proc)
            and self.wait_for_port(self.ports["gateway"], timeout=10.0, proc=self.server_proc)
        ):
            log_info(f"eve-server ready (PID {self.server_proc.pid})")
            return True

        # Check if server failed due to DB schema incompatibility
        tail = ""
        if server_log.exists():
            tail = server_log.read_text(encoding="utf-8", errors="replace")[-2000:]
        log_warn(f"eve-server failed initial startup:\n{tail[-600:]}")

        if not reset_db and self.world_db_path.exists() and any(
            k in tail.lower() for k in ("sqlite", "schema", "migration", "column", "table", "database")
        ):
            incompat = self.world_dir / f"harness_world.incompat_{int(time.time())}.sqlite"
            log_warn(
                f"Detected incompatible DB schema after server update; archiving old DB to {incompat.name} and starting fresh DB."
            )
            shutil.move(str(self.world_db_path), str(incompat))
            return self.start_server(reset_db=True)

        return False

    def stop_server(self) -> None:
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

    def start_client(self) -> bool:
        self.stop_client()
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

        env = os.environ.copy()
        env["EVE_PUBLIC_GATEWAY_ADDRESS"] = f"127.0.0.1:{self.ports['gateway']}"
        env["EVOMU_HARNESS_DIR"] = str(HARNESS_DIR)
        env["PYTHONPATH"] = str(stage_dir / "evomu_py_site")
        env["EVOMU_INCLIENT_CFG"] = str(inclient_cfg)
        env["EVOMU_PLAYTESTER_CFG"] = str(inclient_cfg)
        env["EVOMU_PLAYTESTER_PORT"] = str(self.ports["rpc"])

        cmd = [
            str(exefile),
            "/server:127.0.0.1",
            f"/port:{self.ports['macho']}",
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

        # Wait for in-client JSON-RPC hook to accept connections (after _wait_for_session completes)
        deadline = time.time() + 120.0
        while time.time() < deadline:
            if self.client_proc.poll() is not None:
                log_err(f"EVE.app exited early with code {self.client_proc.returncode}")
                self.stop_client()
                return False
            if self.rpc.ping():
                log_info(f"In-client JSON-RPC bridge connected (client PID {self.client_proc.pid})")
                break
            time.sleep(0.5)
        else:
            log_err("Timed out waiting for in-client JSON-RPC hook on port " + str(self.ports["rpc"]))
            self.stop_client()
            return False

        # Bootstrap login -> character selection -> hangar/space ready
        log_info("Bootstrapping client session into character hangar/space...")
        try:
            boot = self.rpc.call(
                "bootstrap_session",
                {"timeout_ms": 90000, "select_Wait_ms": 90000},
                timeout=110.0,
            )
            log_info(
                f"Session ready: charid={boot.get('session', {}).get('charid')} "
                f"view={boot.get('active_view')} stationid={boot.get('session', {}).get('stationid')} "
                f"solarsystemid={boot.get('session', {}).get('solarsystemid')}"
            )
            return True
        except Exception as exc:
            log_err(f"bootstrap_session failed: {exc}")
            return False

    def stop_client(self) -> None:
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

    def hot_reload_server_boundary(self) -> bool:
        """
        Fetch latest `origin/main` (unless `--no-git-checkout`), compile `eve-server`,
        and restart `eve-server` + `EVE.app` while preserving `harness_world.sqlite`
        and `memory.json`.
        """
        log_info("=== HOT-RELOADING SERVER BOUNDARY ===")
        self.stop_client()
        self.stop_server()

        if not self.args.no_git_checkout:
            subprocess.run(
                ["git", "-C", str(self.worktree_dir), "fetch", "origin", "main"],
                capture_output=True,
                timeout=30,
            )
            subprocess.run(
                ["git", "-C", str(self.worktree_dir), "checkout", "--detach", "origin/main"],
                capture_output=True,
                timeout=15,
            )

        ok, _, err_tail = compile_eve_server(self.worktree_dir)
        if not ok:
            log_err(f"Hot-reload build failed; keeping previous binary if present:\n{err_tail}")

        self.memory.data["server_reloads"] = int(self.memory.data.get("server_reloads", 0)) + 1
        self.memory.data.setdefault("long_term_notes", []).append(
            f"[reload #{self.memory.data['server_reloads']}] Server hot-reloaded to commit {get_git_head(self.worktree_dir)} (world DB preserved)."
        )
        self.memory.save()

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
        git_head = get_git_head(self.worktree_dir)

        # 1. Capture live UI scene graph + window screenshot
        ui_snapshot = self.rpc.call("get_state", timeout=15.0)
        (step_dir / "ui_state.json").write_text(
            json.dumps(ui_snapshot, indent=2), encoding="utf-8"
        )

        screenshot_path = step_dir / "screen.png"
        client_pid = self.client_proc.pid if self.client_proc else None
        cap_meta = capture_window_screenshot(
            screenshot_path,
            client_pid=client_pid,
            bring_foreground=self.args.foreground,
            rpc_client=self.rpc,
        )

        # 2. Reason about next action with `agy` across the configured model fallback chain
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
        decision["_used_model"] = used_model
        (step_dir / "decision.json").write_text(
            json.dumps(decision, indent=2), encoding="utf-8"
        )

        # 3. Execute the chosen action in-client (plus OS Quartz fallback if click_xy)
        action = decision.get("action") or {}
        act_type = str(action.get("type") or "wait")
        burst_paths: List[Path] = []

        # For animation-heavy actions (`warp_to`, `undock`, `dock`), capture burst screenshots
        rpc_timeout = 35.0 if act_type in ("warp_to", "undock", "dock") else 20.0
        exec_res: Dict[str, Any] = {}
        try:
            exec_res = self.rpc.call("execute_action", {"action": action}, timeout=rpc_timeout)
        except Exception as exc:
            exec_res = {"action_ok": False, "action_error": str(exc), "flight_recorder": {}}

        # If `click_xy` or widget click fell back to coordinates and foreground OS clicks are enabled
        if (
            self.args.os_input_fallback
            and act_type in ("click_xy", "click_widget")
            and action.get("x") is not None
            and action.get("y") is not None
        ):
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

        if act_type in ("warp_to", "undock", "dock"):
            for b_idx in range(1, 4):
                bp = step_dir / f"burst_{b_idx}.png"
                capture_window_screenshot(
                    bp,
                    client_pid=client_pid,
                    bring_foreground=False,
                    rpc_client=self.rpc,
                )
                if bp.exists():
                    burst_paths.append(bp)
                time.sleep(0.35)

        (step_dir / "exec_result.json").write_text(
            json.dumps(exec_res, indent=2), encoding="utf-8"
        )

        # 4. Update persistent agent memory and evaluate zero-tolerance bug oracle
        self.memory.apply_agent_decision(step_num, decision, exec_res)
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
        )

        fr_delta = dict((exec_res.get("flight_recorder") or {}) if isinstance(exec_res, dict) else {})
        if isinstance(exec_res, dict) and exec_res.get("state_diff"):
            fr_delta["state_diff"] = exec_res["state_diff"]
        if isinstance(exec_res, dict) and exec_res.get("warp_telemetry"):
            fr_delta["warp_telemetry"] = exec_res["warp_telemetry"]
        return fr_delta, burst_paths

    def run(self) -> int:
        self.install_signal_handlers()
        log_info(
            f"Starting EvoMu Playtester in worktree={self.worktree_dir} "
            f"(HEAD={get_git_head(self.worktree_dir)}, models={self.model_chain})"
        )

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
        last_origin_check = time.time()
        last_fr: Dict[str, Any] = {}
        last_burst: List[Path] = []

        try:
            while not self.stop_requested:
                if self.args.max_steps > 0 and executed_this_invocation >= self.args.max_steps:
                    log_info(f"Reached --max-steps {self.args.max_steps}; stopping.")
                    break

                should_reload, last_origin_check = self.check_external_reload_triggers(
                    last_origin_check
                )
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
                    if not self.start_server(reset_db=False) or not self.start_client():
                        time.sleep(10)
                        continue
                if self.client_proc is None or self.client_proc.poll() is not None or not self.rpc.ping():
                    log_warn("EVE.app client or RPC bridge unreachable! Restarting client...")
                    if not self.start_client():
                        time.sleep(10)
                        continue

                step_num += 1
                executed_this_invocation += 1
                try:
                    last_fr, last_burst = self.execute_step(step_num, last_fr, last_burst)
                except Exception as exc:
                    log_err(f"Step {step_num} raised exception: {exc}")
                    time.sleep(2.0)

                if not self.args.continuous and self.args.max_steps <= 0:
                    break
        finally:
            self.stop_client()
            self.stop_server()
            DEFAULT_PID_FILE.unlink(missing_ok=True)

        return 0


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

    assert PERSONAS_PATH.exists(), f"Missing {PERSONAS_PATH}"
    personas = json.loads(PERSONAS_PATH.read_text(encoding="utf-8"))
    assert len(personas.get("personas", [])) >= 5, "Expected >= 5 personas"

    assert HOOK_SCRIPT_PATH.exists(), f"Missing {HOOK_SCRIPT_PATH}"
    assert SITE_SCRIPT_PATH.exists(), f"Missing {SITE_SCRIPT_PATH}"

    jwt_tok = mint_sso_jwt()
    assert len(jwt_tok.split(".")) == 3, "JWT minting failed"

    m_flags = _resolve_agy_model_args("gemini-4-carbon-medium")
    assert m_flags == ["--model", "Gemini 4 Carbon (Medium)"], m_flags

    win = find_exefile_window()
    log_info(
        f"Self-test passed! Worktree={args.worktree} HEAD={get_git_head(Path(args.worktree))} "
        f"DefaultModels={DEFAULT_MODEL_CHAIN} ActiveExefileWindow={win}"
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
        default=str(DEFAULT_STAGED_APP),
        help=f"Isolated staged EVE.app bundle path (default: {DEFAULT_STAGED_APP})",
    )
    p.add_argument(
        "--model",
        default=None,
        help="Primary reasoning model override (prepended to --model-chain)",
    )
    p.add_argument(
        "--model-chain",
        default=default_chain_env,
        help=f"Comma-separated agy model fallback chain (default: {default_chain_env})",
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
        "--agy-timeout",
        type=int,
        default=120,
        help="Timeout in seconds per agy CLI model call before falling back to the next model",
    )
    p.add_argument("--macho-port", type=int, default=DEFAULT_Ports["macho"])
    p.add_argument("--http-port", type=int, default=DEFAULT_Ports["http"])
    p.add_argument("--gateway-port", type=int, default=DEFAULT_Ports["gateway"])
    p.add_argument("--xmpp-port", type=int, default=DEFAULT_Ports["xmpp"])
    p.add_argument("--rpc-port", type=int, default=DEFAULT_Ports["rpc"])
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
    return p.parse_args(argv)


def main(argv: Optional[List[str]] = None) -> int:
    args = parse_args(argv)
    if args.ping_reload:
        return send_ping_reload()
    if args.self_test:
        return run_self_test(args)
    runner = PlaytesterRunner(args)
    return runner.run()


if __name__ == "__main__":
    sys.exit(main())
